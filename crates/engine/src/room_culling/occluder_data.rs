//! Optional, source-versioned occluder facts. Unsupported ownership and state never hide meshes.

use super::{PortalSide, RoomGraph, assignment::master_side, occlusion::Occluder};
use rusqlite::{Connection, Error, Result};
use std::collections::HashMap;

fn supported(conn: &Connection) -> Result<bool> {
    let tables: bool = conn.query_row(
        "SELECT count(*)=4 FROM sqlite_master WHERE type='table' AND name IN \
         ('room_occluders','room_occluder_links','room_graph_stats','room_graph_cells')",
        [],
        |row| row.get(0),
    )?;
    if !tables {
        return Ok(false);
    }
    conn.query_row(
        "SELECT count(*)=2 FROM room_graph_stats WHERE name IN ('occlusion_version','occlusion_links_verified') AND value=1",
        [],
        |row| row.get(0),
    )
}

/// A roomless interior can still have a source-verified occluder graph.
pub(super) fn has_occluders(conn: &Connection, cell: u32) -> Result<bool> {
    if !supported(conn)? {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_occluders WHERE cell_id=?1)",
        [cell],
        |row| row.get(0),
    )
}

/// Retail renders both members of isolated one-way and reciprocal pairs. Record each
/// authored edge at both endpoints so a disabled or missing peer also disables its neighbor.
fn plane_neighbors(conn: &Connection) -> Result<HashMap<u32, Option<u32>>> {
    let verified_column: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('room_occluder_links') WHERE name='single_target_verified')",
        [], |row| row.get(0),
    )?;
    let query = if verified_column {
        "SELECT ref_id,linked_ref,single_target_verified=1 AND link_index BETWEEN 0 AND 3 FROM room_occluder_links"
    } else {
        "SELECT ref_id,linked_ref,0 FROM room_occluder_links"
    };
    let mut neighbors = HashMap::new();
    let mut statement = conn.prepare_cached(query)?;
    for edge in statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, bool>(2)?,
        ))
    })? {
        let (a, b, verified) = edge?;
        for (id, neighbor) in [(a, b), (b, a)] {
            let candidate = (verified && a != b).then_some(neighbor);
            neighbors
                .entry(id)
                .and_modify(|old| {
                    if *old != candidate {
                        *old = None;
                    }
                })
                .or_insert(candidate);
        }
    }
    Ok(neighbors)
}

pub(super) fn load(
    conn: &Connection,
    cell: u32,
    graph: &RoomGraph,
    room_index: &HashMap<u32, usize>,
) -> Result<Vec<Occluder>> {
    if !supported(conn)? || !graph.supports_unbounded {
        return Ok(Vec::new());
    }
    let Some(authored) = &graph.authored else {
        return Ok(Vec::new());
    };
    if !authored.complete || authored.masters.iter().any(Option::is_none) {
        return Ok(Vec::new());
    }
    let neighbors = plane_neighbors(conn)?;
    let mut statement = conn.prepare_cached(
        "SELECT ref_id,shape,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z,scale,\
         header_flags,has_enable_parent,has_links,o.target_ref,
                EXISTS(SELECT 1 FROM room_reference_rules r WHERE r.reference_id=o.ref_id
                       AND r.cell_id=o.cell_id AND r.assignment='target' AND r.target_ref=o.target_ref)
         FROM room_occluders o WHERE cell_id=?1 ORDER BY ref_id",
    )?;
    let rows = statement.query_map([cell], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, u32>(1)?,
            [row.get(2)?, row.get(3)?, row.get(4)?],
            [row.get(5)?, row.get(6)?, row.get(7)?],
            [row.get(8)?, row.get(9)?, row.get(10)?],
            row.get::<_, f32>(11)?,
            row.get::<_, u32>(12)?,
            row.get::<_, bool>(13)?,
            row.get::<_, bool>(14)?,
            row.get::<_, Option<u32>>(15)?,
            row.get::<_, bool>(16)?,
        ))
    })?;
    let mut occluders = Vec::new();
    let mut unusable_rows = 0usize;
    for row in rows {
        let (
            id,
            shape,
            position,
            rotation,
            half,
            scale,
            flags,
            parent,
            links,
            target,
            verified_target,
        ) = match row {
            Ok(values) => values,
            Err(Error::InvalidColumnType(..) | Error::IntegralValueOutOfRange(..)) => {
                unusable_rows += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        // Retail kept isolated authored XORD pairs as separate render planes.
        if flags & (0x800 | 0x20 | shared::records::MULTIBOUND_REFERENCE_FLAG) != 0
            || parent
            || ((links || neighbors.contains_key(&id))
                && (shape != 3 || neighbors.get(&id).copied().flatten().is_none()))
            || (target.is_some() && (shape != 3 || !verified_target))
        {
            continue;
        }
        let Some(mut occluder) = Occluder::from_creation(
            id,
            shape,
            position,
            rotation,
            half,
            scale,
            PortalSide::Unknown,
        ) else {
            unusable_rows += 1;
            continue;
        };
        // Native occlusion-shape assignment tests its translation before portal sharing. More
        // than one master would require reproducing native handle/hash iteration order.
        occluder.owner = match graph.rooms_containing(occluder.center).as_slice() {
            [] if target.is_none() => PortalSide::Unbounded,
            [room]
                if target.is_none_or(|id| {
                    master_side(id, room_index, &authored.masters) == PortalSide::Room(*room)
                }) =>
            {
                PortalSide::Room(*room)
            }
            _ => continue,
        };
        occluders.push(occluder);
    }
    if !neighbors.is_empty() {
        let owners: HashMap<_, _> = occluders
            .iter()
            .filter(|o| neighbors.contains_key(&o.ref_id))
            .map(|o| (o.ref_id, o.owner))
            .collect();
        occluders.retain(|o| match neighbors.get(&o.ref_id) {
            None => true,
            Some(Some(peer)) => {
                neighbors.get(peer) == Some(&Some(o.ref_id)) && owners.get(peer) == Some(&o.owner)
            }
            Some(None) => false,
        });
    }
    if unusable_rows > 0 {
        tracing::warn!(cell_id = %format_args!("{cell:08X}"), unusable_rows, "skipped unusable occluder rows");
    }
    Ok(occluders)
}
