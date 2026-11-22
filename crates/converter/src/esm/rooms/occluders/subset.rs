//! Incremental edits to authored occluders in a converted world database.

use super::{Exported, MarkerSource, is_marker, placement, primitive, project};
use crate::{
    esm::{
        extractors::SubrecordView,
        load_order::LoadOrder,
        projection_omissions::ProjectionOmissions,
        records::RawRecord,
        rooms::{OCCLUSION_MARKER_BASE, PORTAL_MARKER_BASE, ROOM_MARKER_BASE, u32_at},
    },
    records::{DecodedRecord, source_identity},
};
use rusqlite::{Connection, OptionalExtension, Result, Transaction, params};
use shared::records::ArchivedRecordData;
use std::collections::{HashMap, HashSet};

/// Shape dimensions in Creation units. A plane lies on its local X-Z axes.
#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeOccluderShape {
    Plane {
        half_width: f32,
        half_height: f32,
    },
    /// Physical XORD slots and optional XMBR in the pack's runtime FormID space.
    #[serde(skip)]
    PlaneWithReferences {
        half_width: f32,
        half_height: f32,
        links: Option<[u32; 4]>,
        room: Option<u32>,
    },
    Box {
        half_extents: [f32; 3],
    },
}

impl NativeOccluderShape {
    pub(in crate::esm) fn primitive_bytes(self) -> Result<Vec<u8>> {
        let (kind, half) = match self {
            NativeOccluderShape::Plane {
                half_width,
                half_height,
            }
            | NativeOccluderShape::PlaneWithReferences {
                half_width,
                half_height,
                ..
            } => (3u32, [half_width, 0.0, half_height]),
            NativeOccluderShape::Box { half_extents } => (1, half_extents),
        };
        let mut xprm: Vec<u8> = half.into_iter().flat_map(f32::to_le_bytes).collect();
        xprm.resize(28, 0);
        xprm.extend(kind.to_le_bytes());
        if primitive(&xprm).is_none() {
            return Err(invalid_input("invalid native occluder extents"));
        }
        Ok(xprm)
    }
}

/// One authored change to an existing pack. Use a stable, nonzero local ID in
/// the 0xFF FormID slot, which the Skyrim plugin load order does not allocate.
#[derive(Clone, Copy, Debug)]
pub enum NativeOccluderChange {
    Upsert {
        id: u32,
        cell_id: u32,
        position: [f32; 3],
        rotation: [f32; 3],
        shape: NativeOccluderShape,
    },
    Remove {
        id: u32,
    },
}

/// Applies native occluder changes to the existing world database atomically.
/// Position uses Creation units and rotation uses Creation Euler radians.
/// The pack must have a source-verified room/occluder projection.
pub fn apply_native_occluders(conn: &Connection, changes: &[NativeOccluderChange]) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    if !tables_present(&tx)?
        || !tx.query_row(
            "SELECT count(*)=4 FROM room_graph_stats WHERE
             (name IN ('occlusion_version','occlusion_links_verified','assignment_version') AND value=1)
             OR (name='visibility_version' AND value=2)",
            [],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(invalid_input("native occluders require a source-verified world database"));
    }
    let has_placement_edits = crate::esm::rooms::table_exists(&tx, "room_placement_edits")?;
    let mut master = HashMap::new();
    for &change in changes {
        let mut record = RawRecord {
            form_id: 0,
            record_type: *b"REFR",
            flags: 0,
            subrecords: Vec::new(),
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        record.form_id = match change {
            NativeOccluderChange::Upsert {
                id,
                cell_id,
                position,
                rotation,
                shape,
            } => {
                if !is_interior(&tx, cell_id)? {
                    return Err(invalid_input(format!(
                        "native occluder {id:08X} requires an interior cell"
                    )));
                }
                let xprm = shape.primitive_bytes()?;
                let data: Vec<u8> = position
                    .into_iter()
                    .chain(rotation)
                    .flat_map(f32::to_le_bytes)
                    .collect();
                if placement(&data).is_none() {
                    return Err(invalid_input(format!(
                        "invalid geometry for native occluder {id:08X}"
                    )));
                }
                record.cell_form_id = Some(cell_id);
                record.subrecords = vec![
                    (
                        b"NAME".to_vec(),
                        OCCLUSION_MARKER_BASE.to_le_bytes().to_vec(),
                    ),
                    (b"XPRM".to_vec(), xprm),
                    (b"DATA".to_vec(), data),
                ];
                if let NativeOccluderShape::PlaneWithReferences { links, room, .. } = shape {
                    if let Some(slots) = links {
                        record
                            .subrecords
                            .push((b"XORD".to_vec(), slots.map(u32::to_le_bytes).concat()));
                    }
                    if let Some(room) = room {
                        record
                            .subrecords
                            .push((b"XMBR".to_vec(), room.to_le_bytes().to_vec()));
                    }
                }
                id
            }
            NativeOccluderChange::Remove { id } => {
                record.flags = 0x20;
                id
            }
        };
        let id = record.form_id;
        if id <= 0xff00_0000
            || tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM records WHERE form_id=?1) OR EXISTS(SELECT 1 FROM [references] WHERE id=?1)
                 OR EXISTS(SELECT 1 FROM cells WHERE id=?1)",
                [id],
                |row| row.get::<_, bool>(0),
            )?
            || has_placement_edits && tx.prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM room_placement_edits WHERE form_id=?1)",
            )?.query_row([id], |row| row.get::<_, bool>(0))?
        {
            return Err(invalid_input(format!(
                "invalid or occupied native occluder ID {id:08X}"
            )));
        }
        if master.insert(id, record).is_some() {
            return Err(invalid_input(format!(
                "duplicate native occluder ID {id:08X}"
            )));
        }
    }
    let mut omissions = ProjectionOmissions::default();
    update_authored_subset(&tx, &master, &HashSet::new(), &mut omissions)?;
    for (category, details) in omissions.categories {
        if let Some(sample) = details.samples.first() {
            return Err(invalid_input(format!(
                "occluder {:08X}: {category}: {}",
                sample.form_id, sample.reason
            )));
        }
    }
    tx.commit()
}

fn invalid_input(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message.into(),
    )))
}

fn tables_present(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT count(*)=2 FROM sqlite_master WHERE type='table'
         AND name IN ('room_occluders','room_occluder_links')",
        [],
        |row| row.get(0),
    )
}

fn is_interior(tx: &Transaction<'_>, cell: u32) -> Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM cells WHERE id=?1
         AND worldspace_id IS NULL AND grid_x IS NULL AND grid_y IS NULL)",
    )?
    .query_row([cell], |row| row.get(0))
}

/// A missing completeness row on an existing pack is common for roomless cells.
/// Check the stored placements before certifying one, including skipped markers.
fn seed_roomless_cell(tx: &Transaction<'_>, cell: u32) -> Result<()> {
    let previously_projected: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM room_graph_cells WHERE cell_id=?1)
          OR EXISTS(SELECT 1 FROM room_bounds WHERE cell_id=?1)
          OR EXISTS(SELECT 1 FROM room_portals WHERE cell_id=?1)",
        )?
        .query_row([cell], |row| row.get(0))?;
    if previously_projected {
        return Ok(());
    }
    let mut select =
        tx.prepare_cached("SELECT data FROM records WHERE cell_id=?1 AND record_type='REFR'")?;
    let mut rows = select.query([cell])?;
    let mut complete = true;
    while let Some(row) = rows.next()? {
        let bytes: Vec<u8> = row.get(0)?;
        let Ok(record) = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes) else {
            complete = false;
            break;
        };
        if record.subrecords.iter().any(|field| {
            field.tag == *b"NAME"
                && matches!(
                    u32_at(&field.data, 0),
                    Some(ROOM_MARKER_BASE | PORTAL_MARKER_BASE | OCCLUSION_MARKER_BASE)
                )
        }) {
            complete = false;
            break;
        }
    }
    tx.prepare_cached("INSERT INTO room_graph_cells(cell_id,complete) VALUES(?1,?2)")?
        .execute(params![cell, complete])?;
    Ok(())
}

#[derive(Default)]
pub(in crate::esm) struct SubsetUpdate {
    /// Edits for which the existing room, portal and object assignments still hold.
    pub only_occluders: HashSet<u32>,
    pub invalidated_cells: HashSet<u32>,
}

/// Replaces the supplied placements' occluder facts and invalidates linked cells.
pub(in crate::esm) fn update_subset(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    sources: Option<(&HashMap<u32, DecodedRecord>, &LoadOrder)>,
    omissions: &mut ProjectionOmissions,
) -> Result<SubsetUpdate> {
    project_subset(
        tx,
        master,
        |id| {
            MarkerSource::from_typed(
                sources.and_then(|(records, order)| records.get(&id).map(|record| (record, order))),
            )
        },
        omissions,
    )
}

/// Placement imports validate linked FormIDs against the source before remapping.
pub(in crate::esm) fn update_authored_subset(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    damaged_occluders: &HashSet<u32>,
    omissions: &mut ProjectionOmissions,
) -> Result<SubsetUpdate> {
    project_subset(
        tx,
        master,
        |id| {
            if damaged_occluders.contains(&id) {
                MarkerSource::DamagedPlugin
            } else {
                MarkerSource::Authored
            }
        },
        omissions,
    )
}

fn project_subset<'a>(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    provenance: impl Fn(u32) -> MarkerSource<'a>,
    omissions: &mut ProjectionOmissions,
) -> Result<SubsetUpdate> {
    let mut update = SubsetUpdate::default();
    if !master.values().any(|record| record.record_type == *b"REFR") || !tables_present(tx)? {
        return Ok(update);
    }
    super::super::migrate_occluder_links(tx)?;
    let has_placement_edits = super::super::table_exists(tx, "room_placement_edits")?;
    let mut affected = HashSet::new();
    let mut markers = Vec::new();
    let mut collect_targets = tx.prepare_cached(
        "SELECT target.cell_id FROM room_occluder_links link
         JOIN room_occluders target ON target.ref_id=link.linked_ref
         LEFT JOIN room_occluders source ON source.ref_id=link.ref_id WHERE link.ref_id=?1
         AND (link.single_target_verified<>1 OR source.cell_id IS NOT target.cell_id)",
    )?;
    let mut collect_sources = tx.prepare_cached(
        "SELECT coalesce(marker.cell_id, canonical.cell_id) FROM room_occluder_links link
         LEFT JOIN room_occluders marker ON marker.ref_id=link.ref_id
         LEFT JOIN records canonical ON canonical.form_id=link.ref_id
         LEFT JOIN room_occluders target ON target.ref_id=link.linked_ref
         WHERE link.linked_ref=?1 AND (link.single_target_verified<>1
           OR (target.cell_id IS NOT NULL AND marker.cell_id IS NOT target.cell_id))",
    )?;
    for record in master
        .values()
        .filter(|record| record.record_type == *b"REFR")
    {
        let id = record.form_id;
        let old_base: Option<u32> = tx
            .prepare_cached("SELECT base_form_id FROM [references] WHERE id=?1")?
            .query_row([id], |row| row.get(0))
            .optional()?;
        let old_occluder = old_base == Some(OCCLUSION_MARKER_BASE)
            || tx
                .prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM room_occluders WHERE ref_id=?1)
                   OR EXISTS(SELECT 1 FROM room_occluder_links WHERE ref_id=?1)",
                )?
                .query_row([id], |row| row.get(0))?;
        let source = provenance(id);
        let marker = is_marker(record, &SubrecordView::new(&record.subrecords))
            || !record.is_deleted()
                && source.typed().is_some_and(|(record, order)| {
                    record.framed_source_prefix().iter().any(|(tag, data)| {
                        tag == b"NAME"
                            && data.len() == 4
                            && u32_at(data, 0).is_some_and(|id| {
                                source_identity::header_runtime_id(
                                    b"STAT",
                                    id,
                                    record.load_order as usize,
                                    order,
                                ) == Some(OCCLUSION_MARKER_BASE)
                            })
                    })
                });
        if marker && old_base.is_none_or(|base| base == OCCLUSION_MARKER_BASE)
            || record.is_deleted() && old_occluder
        {
            update.only_occluders.insert(id);
        }
        if old_occluder {
            for cell in collect_targets.query_map([id], |row| row.get::<_, u32>(0))? {
                affected.insert(cell?);
            }
        }
        if record.is_deleted() {
            for cell in collect_sources.query_map([id], |row| row.get::<_, Option<u32>>(0))? {
                affected.extend(cell?);
            }
        }
        if old_occluder || marker {
            let interior = match record.cell_form_id {
                Some(cell) if record.worldspace_form_id.is_none() => is_interior(tx, cell)?,
                _ => false,
            };
            if let Some(cell) = record.cell_form_id.filter(|_| marker && interior) {
                seed_roomless_cell(tx, cell)?;
            }
            markers.push((record, marker, interior, source));
        }
    }

    markers.sort_unstable_by_key(|(record, ..)| record.form_id);
    for (record, marker, interior, source) in &markers {
        let id = record.form_id;
        tx.prepare_cached("DELETE FROM room_occluder_links WHERE ref_id=?1")?
            .execute([id])?;
        let previous_cell: Option<u32> = tx
            .prepare_cached("DELETE FROM room_occluders WHERE ref_id=?1 RETURNING cell_id")?
            .query_row([id], |row| row.get(0))
            .optional()?;
        tx.prepare_cached("DELETE FROM room_reference_rules WHERE reference_id=?1")?
            .execute([id])?;
        if *marker {
            let mut result = Exported::default();
            project(
                tx,
                record,
                *interior,
                *source,
                |target| {
                    if let Some(record) = master.get(&target) {
                        return Ok(record.record_type == *b"REFR" && !record.is_deleted());
                    }
                    if has_placement_edits
                        && let Some(live) = tx
                            .prepare_cached(
                                "SELECT EXISTS(SELECT 1 FROM [references] WHERE id=?1)
                         FROM room_placement_edits WHERE form_id=?1 LIMIT 1",
                            )?
                            .query_row([target], |row| row.get(0))
                            .optional()?
                    {
                        return Ok(live);
                    }
                    tx.prepare_cached(
                    "SELECT coalesce((SELECT record_type='REFR' FROM records WHERE form_id=?1), ?1 < 2048 OR EXISTS(SELECT 1 FROM room_occluders WHERE ref_id=?1))",
                )?.query_row([target], |row| row.get(0))
                },
                &mut result,
                omissions,
            )?;
            if result.unverified_link_targets {
                tx.prepare_cached(
                    "UPDATE room_graph_stats SET value=0 WHERE name='occlusion_links_verified'",
                )?
                .execute([])?;
            }
            // Remapped legacy XORD bytes cannot distinguish authored nulls from failed links.
            let unverified_links = matches!(source, MarkerSource::Canonical)
                && SubrecordView::new(&record.subrecords)
                    .find(b"XORD")
                    .is_some();
            if result.invalid != 0 || unverified_links {
                result.incomplete_cells.extend(record.cell_form_id);
            }
            if !result.incomplete_cells.is_empty() {
                affected.extend(previous_cell);
                affected.extend(result.incomplete_cells);
            }
        }
    }
    for (record, ..) in markers {
        for cell in collect_targets.query_map([record.form_id], |row| row.get::<_, u32>(0))? {
            affected.insert(cell?);
        }
    }
    for cell in affected {
        let updated = tx
            .prepare_cached(
                "INSERT INTO room_graph_cells(cell_id,complete)
             SELECT id,0 FROM cells WHERE id=?1
             ON CONFLICT(cell_id) DO UPDATE SET complete=0",
            )?
            .execute([cell])?;
        if updated != 0 {
            update.invalidated_cells.insert(cell);
        }
    }
    Ok(update)
}
