//! Optional, source-versioned occluder facts. Unsupported ownership and state never hide meshes.

use super::{PortalSide, RoomGraph, occlusion::Occluder};
use rusqlite::{Connection, Error, Result};
use std::collections::HashSet;

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

pub(super) fn load(conn: &Connection, cell: u32, graph: &RoomGraph) -> Result<Vec<Occluder>> {
    if !supported(conn)? || !graph.supports_unbounded {
        return Ok(Vec::new());
    }
    let Some(authored) = &graph.authored else {
        return Ok(Vec::new());
    };
    if !authored.complete || authored.masters.iter().any(Option::is_none) {
        return Ok(Vec::new());
    }
    let mut linked = HashSet::new();
    let mut statement = conn.prepare_cached("SELECT ref_id,linked_ref FROM room_occluder_links")?;
    for link in statement.query_map([], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)))? {
        let (source, target) = link?;
        linked.insert(source);
        linked.insert(target);
    }
    let mut statement = conn.prepare_cached(
        "SELECT ref_id,shape,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z,scale,\
         header_flags,has_enable_parent,has_links,target_ref \
         FROM room_occluders WHERE cell_id=?1 ORDER BY ref_id",
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
        ))
    })?;
    let mut occluders = Vec::new();
    let mut unusable_rows = 0usize;
    for row in rows {
        let (id, shape, position, rotation, half, scale, flags, parent, links, target) = match row {
            Ok(values) => values,
            Err(Error::InvalidColumnType(..) | Error::IntegralValueOutOfRange(..)) => {
                unusable_rows += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        // Initially disabled, deleted, NONE, enable-parent, linked, and explicit override paths
        // need runtime state or construction rules not provided by the static room export.
        if flags & (0x800 | 0x20 | shared::records::MULTIBOUND_REFERENCE_FLAG) != 0
            || parent
            || links
            || linked.contains(&id)
            || target.is_some()
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
            [] => PortalSide::Unbounded,
            [room] => PortalSide::Room(*room),
            _ => continue,
        };
        occluders.push(occluder);
    }
    if unusable_rows > 0 {
        tracing::warn!(cell_id = %format_args!("{cell:08X}"), unusable_rows, "skipped unusable occluder rows");
    }
    Ok(occluders)
}
