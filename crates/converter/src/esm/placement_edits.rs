//! Offline, append-only imports of interiors and room placements into an existing pack.

use super::{
    RemapWarnings,
    binary::{
        FLAG_COMPRESSED, ParsedRecord, PluginMetadata, parse_plugin_bytes,
        plugin_metadata_from_bytes,
    },
    environment::Links,
    exporter::{insert_reference, project_cell},
    extractors::SubrecordView,
    load_order::{LoadOrder, normalize_plugin_name},
    projection_omissions::ProjectionOmissions,
    records::{
        RawRecord,
        achr::{SubrecordXESP, flags::INITIALLY_DISABLED},
    },
    reference_links, remap_record_form_ids,
    rooms::{
        NativeOccluderShape, OCCLUSION_MARKER_BASE, PORTAL_MARKER_BASE, ROOM_MARKER_BASE,
        RoomLinkLayout, SHAPE_BOX, SHAPE_PORTAL, refresh_placement_cells, update_authored_subset,
    },
    types::ArchivedRecordData,
};
use crate::{
    cache::hash_bytes,
    records::{DiscardedFrameScope, source_identity},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::Deserialize;
use shared::{asset_lock::AssetLock, records::MULTIBOUND_REFERENCE_FLAG};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

const LAYER_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS room_placement_layers (
        id INTEGER PRIMARY KEY, name TEXT NOT NULL, sha256 TEXT NOT NULL,
        plugin_flags INTEGER, plugin_masters TEXT, source BLOB NOT NULL
    );
    CREATE TABLE IF NOT EXISTS room_placement_edits (
        layer_id INTEGER NOT NULL REFERENCES room_placement_layers(id),
        form_id INTEGER NOT NULL, source_form_id INTEGER NOT NULL, deleted INTEGER NOT NULL,
        PRIMARY KEY(form_id,layer_id)
    ) WITHOUT ROWID;
";
const NATIVE_FORM_START: u32 = 0xff00_0001;
const CELL: [u8; 4] = *b"CELL";
const REFR: [u8; 4] = *b"REFR";

/// Source to apply after the pack's original conversion and any earlier placement edits.
#[derive(Clone, Copy)]
pub enum PlacementSource<'a> {
    /// Skyrim interiors and placements. Masters are resolved against the pack and prior imports.
    SkyrimPlugin(&'a Path),
    /// MudCrab placement JSON using this pack's runtime FormIDs.
    NativeJson(&'a Path),
}

#[derive(Debug, Default)]
pub struct PlacementEditReport {
    pub changed_references: usize,
    /// Cells touched by placement edits or room graph invalidation.
    pub affected_cells: Vec<u32>,
    pub created_cells: Vec<u32>,
    pub certified_cells: Vec<u32>,
    /// Counts and sampled reasons for admitted placements with occluder projection omissions.
    /// Placements skipped during plugin admission produce a separate warning.
    pub occluder_omissions: ProjectionOmissions,
}

/// Apply one source under the pack lock. Runtime placements, room projections and the retained
/// edit source commit together. Reapplying an already imported source is a no-op.
pub fn apply(assets: &Path, source: PlacementSource<'_>) -> Result<PlacementEditReport> {
    let lock = AssetLock::acquire_exclusive(assets)?;
    let mut conn = crate::integration::open_private_world_database(lock.asset_dir())?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current_schema: bool = tx.query_row(
        "SELECT version=?1 FROM schema_info",
        [shared::WORLD_DATABASE_SCHEMA_VERSION],
        |row| row.get(0),
    )?;
    ensure!(
        current_schema,
        "placement edits require the current world database schema"
    );
    let (visibility_version, assignment_version): (Option<i64>, Option<i64>) = tx.query_row(
        "SELECT (SELECT value FROM room_graph_stats WHERE name='visibility_version'),
                (SELECT value FROM room_graph_stats WHERE name='assignment_version')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        visibility_version == Some(2) && matches!(assignment_version, None | Some(1)),
        "placement edits require a supported versioned room graph"
    );
    let (path, is_plugin) = match source {
        PlacementSource::SkyrimPlugin(path) => (path, true),
        PlacementSource::NativeJson(path) => (path, false),
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| color_eyre::eyre::eyre!("source needs a UTF-8 filename"))?;
    let bytes = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let checksum = hash_bytes(&bytes);
    tx.execute_batch(LAYER_TABLES)?;
    reference_links::ensure_subset_sources(&tx)?;
    tx.execute_batch(reference_links::SUBSET_NONPLACEMENT_DELETIONS_SQL)?;
    tx.execute_batch(reference_links::REFERENCE_LINK_TABLES_SQL)?;

    let name = if is_plugin {
        let name = normalize_plugin_name(name);
        ensure!(
            name.ends_with(".esm") || name.ends_with(".esp") || name.ends_with(".esl"),
            "Skyrim placement source must have an .esm, .esp, or .esl filename"
        );
        name
    } else {
        name.to_owned()
    };
    let already_applied: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_placement_layers WHERE sha256=?1
         AND ((?2 AND plugin_flags IS NOT NULL AND name=?3) OR (NOT ?2 AND plugin_flags IS NULL)))",
        params![checksum, is_plugin, name],
        |row| row.get(0),
    )?;
    if already_applied {
        return Ok(PlacementEditReport::default());
    }
    if is_plugin {
        let name_taken: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_placement_layers
             WHERE plugin_flags IS NOT NULL AND name=?1)",
            [&name],
            |row| row.get(0),
        )?;
        ensure!(
            !name_taken,
            "{name} is already imported with different contents. To replace it, replay the \
             placement layers on a fresh conversion with the corrected file in the original \
             position. To add a follow-up, author a patch with a new filename and use {name} \
             as a master for its forms."
        );
    }
    // Add missing projection tables transactionally. Existing version rows continue
    // to determine which ownership and occlusion evidence the runtime can trust.
    tx.execute_batch(super::rooms::ROOM_TABLES_SQL)?;
    super::rooms::migrate_room_metadata(&tx)?;
    let (mut import, metadata) = if is_plugin {
        let metadata = plugin_metadata_from_bytes(&bytes)?;
        (plugin_edits(&tx, &name, &bytes, &metadata)?, Some(metadata))
    } else {
        let native = serde_json::from_slice::<NativePlacements>(&bytes)
            .wrap_err("invalid native placement JSON")?;
        (native.into_import(assignment_version.is_none())?, None)
    };
    ensure!(
        !import.cells.is_empty() || !import.edits.is_empty() || !import.damage.is_empty(),
        "{name} contains no cells or placement edits"
    );

    tx.execute(
        "INSERT INTO room_placement_layers(name,sha256,plugin_flags,plugin_masters,source)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            name,
            checksum,
            metadata.as_ref().map(|m| m.flags),
            metadata
                .as_ref()
                .map(|m| serde_json::to_string(&m.masters))
                .transpose()?,
            bytes
        ],
    )?;
    let layer = tx.last_insert_rowid();
    let has_history: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='record_overrides' AND type='table')",
        [],
        |row| row.get(0),
    )?;
    let mut fresh_cells = HashSet::new();
    let empty_master = HashMap::new();
    for creation in &import.cells {
        let record = &creation.record;
        let id = record.form_id;
        require_unused_id(&tx, id, has_history)?;
        require_no_cell_data(&tx, id)?;
        project_cell(
            &tx,
            &Links {
                master: &empty_master,
            },
            id,
            record,
            &record.subrecords,
        )
        .wrap_err_with(|| format!("cell {id:08X}"))?;
        tx.execute(
            "INSERT INTO room_placement_edits VALUES (?1,?2,?3,0)",
            params![layer, id, creation.source_id],
        )?;
        fresh_cells.insert(id);
    }
    let mut cells = fresh_cells.clone();
    let mut changed = HashSet::new();
    let mut unavailable_link_targets = Vec::new();
    let mut available_link_targets = HashSet::new();
    let mut layouts = HashMap::new();
    let mut records = HashMap::new();
    for (index, mut edit) in import.edits.into_iter().enumerate() {
        let id = edit.record.form_id;
        let in_fresh_cell = edit
            .record
            .cell_form_id
            .is_some_and(|cell| fresh_cells.contains(&cell));
        ensure!(changed.insert(id), "duplicate placement edit for {id:08X}");
        let must_be_new = in_fresh_cell || (is_plugin && edit.owns_new_id);
        let was_link_target = reference_links::has_live_target(&tx, id)?;
        apply_edit(
            &tx,
            &mut edit,
            &mut cells,
            must_be_new,
            has_history,
            is_plugin,
        )
        .wrap_err_with(|| format!("placement {id:08X}"))?;
        reference_links::forget_subset_source(&tx, id)?;
        match (was_link_target, reference_links::has_live_target(&tx, id)?) {
            (true, false) => unavailable_link_targets.push(id),
            (false, true) => {
                available_link_targets.insert(id);
            }
            _ => {}
        }
        tx.execute(
            "INSERT INTO room_placement_edits VALUES (?1,?2,?3,?4)",
            params![layer, id, edit.source_id, edit.record.is_deleted()],
        )?;
        if let Some(layout) = edit.room_links {
            layouts.insert(id, layout);
        }
        if import
            .unreadable_refs
            .get(&Some(id))
            .is_some_and(|&next_edit| index >= next_edit)
        {
            import.unreadable_refs.remove(&Some(id));
        }
        records.insert(id, edit.record);
    }
    let mut omissions = ProjectionOmissions::default();
    let occluder_update =
        update_authored_subset(&tx, &records, &import.damaged_occluders, &mut omissions)?;
    for (category, details) in &omissions.categories {
        for sample in &details.samples {
            let message = format!(
                "occluder {:08X}: {category}: {}",
                sample.form_id, sample.reason
            );
            ensure!(is_plugin, "{message}");
            eprintln!(
                "warning: {name}: {message} ({} omissions in category)",
                details.count
            );
        }
    }
    let mut certified_cells = refresh_placement_cells(
        &tx,
        &cells,
        &fresh_cells,
        &changed,
        &layouts,
        std::slice::from_ref(&name),
    )?;
    let incomplete = mark_incomplete(&tx, &import.damage)?;
    if import.unknown_discarded_scope || !import.unreadable_refs.is_empty() {
        tx.execute(
            "UPDATE room_graph_stats SET value=0 WHERE name='occlusion_links_verified'",
            [],
        )?;
    }
    certified_cells.retain(|cell| !incomplete.contains(cell));
    cells.extend(incomplete);
    cells.extend(occluder_update.invalidated_cells);
    super::doors::refresh_placement_links(&tx, &changed)?;
    reference_links::refresh_incoming(
        &tx,
        &unavailable_link_targets,
        &available_link_targets,
        &changed.iter().copied().collect::<Vec<_>>(),
    )?;
    tx.commit()?;
    let mut affected_cells = cells.into_iter().collect::<Vec<_>>();
    affected_cells.sort_unstable();
    let mut created_cells = fresh_cells.into_iter().collect::<Vec<_>>();
    created_cells.sort_unstable();
    Ok(PlacementEditReport {
        changed_references: changed.len(),
        affected_cells,
        created_cells,
        certified_cells,
        occluder_omissions: omissions,
    })
}

#[derive(Default)]
struct Import {
    cells: Vec<CellCreation>,
    edits: Vec<Edit>,
    damaged_occluders: HashSet<u32>,
    /// Index of the first edit following the latest unreadable REFR for each identity.
    unreadable_refs: HashMap<Option<u32>, usize>,
    unknown_discarded_scope: bool,
    damage: HashSet<DiscardedFrameScope>,
}

struct CellCreation {
    source_id: u32,
    record: RawRecord,
}

struct Edit {
    source_id: u32,
    record: RawRecord,
    owns_new_id: bool,
    room_links: Option<RoomLinkLayout>,
    inherit_room_master: bool,
}

struct PreviousPlacement {
    cell: u32,
    data: Vec<u8>,
    flags: u32,
    enable_parent_id: Option<u32>,
    enable_parent_flags: Option<u32>,
}

fn plugin_edits(
    tx: &Transaction<'_>,
    name: &str,
    bytes: &[u8],
    metadata: &PluginMetadata,
) -> Result<Import> {
    let (mut order, _) = crate::records::history::stored_order(tx).wrap_err(
        "Skyrim placement imports require a pack with retained in-house plugin headers",
    )?;
    let plugin_count: usize = tx.query_row("SELECT count(*) FROM plugins", [], |row| row.get(0))?;
    ensure!(
        order.names.len() == plugin_count
            && plugin_count > 0
            && order.normal.get("skyrim.esm") == Some(&0),
        "pack has no complete Skyrim plugin slot mapping"
    );
    let mut statement = tx.prepare(
        "SELECT name,plugin_flags,plugin_masters FROM room_placement_layers
         WHERE plugin_flags IS NOT NULL ORDER BY id",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (name, flags, masters) = row?;
        order.names.push(name);
        order.metadata.push(PluginMetadata {
            flags,
            masters: serde_json::from_str(&masters)?,
        });
    }
    let priority = order.names.len();
    let load_order = u32::try_from(priority)?;
    order.names.push(name.to_owned());
    order.metadata.push(metadata.clone());
    let order = LoadOrder::from_metadata(order.names, order.metadata)?;
    let mut candidates = Vec::new();
    let mut damaged_new_cells = HashSet::new();
    parse_plugin_bytes(bytes, name, |candidate| {
        if let ParsedRecord::DamagedRecord(cell) = &candidate
            && cell.record_type == CELL
            && (cell.form_id >> 24) as usize == metadata.masters.len()
        {
            damaged_new_cells.insert(cell.form_id);
        }
        candidates.push(candidate);
    })?;
    // A usable declaration in the same plugin can supply the missing parent.
    if !damaged_new_cells.is_empty() {
        for candidate in &candidates {
            if let ParsedRecord::Usable(cell) = candidate
                && cell.record_type == CELL
                && !cell.is_deleted()
            {
                damaged_new_cells.remove(&cell.form_id);
            }
        }
    }
    let warnings = RemapWarnings::default();
    let mut import = Import::default();
    let mut damage_after = HashMap::new();
    for candidate in candidates {
        let mut record = match candidate {
            ParsedRecord::Usable(record) => record,
            ParsedRecord::DamagedRecord(record) => {
                let id = record_damage(&mut damage_after, &mut import, &record, priority, &order);
                if record.record_type == REFR {
                    import.unreadable_refs.insert(id, import.edits.len());
                }
                continue;
            }
            ParsedRecord::TruncatedPlacement(placement) => {
                let id = placement.runtime_id(priority, &order);
                if placement.could_be_reference() {
                    import.unreadable_refs.insert(id, import.edits.len());
                }
                if let Some(id) = id {
                    damage_after.insert(id, import.edits.len());
                } else {
                    import.damage.insert(DiscardedFrameScope::Unknown);
                }
                continue;
            }
            ParsedRecord::DiscardedFrame(scope) => {
                let scope = scope.resolve(priority, &order);
                import.damage.insert(scope);
                import.unknown_discarded_scope |= scope == DiscardedFrameScope::Unknown;
                continue;
            }
        };
        let source_id = record.form_id;
        let Some(remapped) =
            source_identity::header_runtime_id(&record.record_type, source_id, priority, &order)
                .filter(|&id| id != 0)
        else {
            ensure!(
                record.record_type == REFR,
                "{name}: invalid record FormID {source_id:08X}"
            );
            eprintln!(
                "warning: {name}: skipped placement {source_id:08X} with invalid record FormID"
            );
            let view = SubrecordView::new(&record.subrecords);
            if !record.is_deleted() && may_link_occluder(&view, priority, &order) {
                import.unreadable_refs.insert(None, import.edits.len());
            }
            record_damage(&mut damage_after, &mut import, &record, priority, &order);
            continue;
        };
        let owns_new_id = (source_id >> 24) as usize == metadata.masters.len();
        ensure!(
            !owns_new_id || source_id & 0x00ff_ffff != 0,
            "{name}: new FormID {source_id:08X} has a zero local ID"
        );
        if record.record_type == CELL && !record.is_deleted() {
            if owns_new_id {
                validate_new_cell(&record, metadata.flags & 0x80 != 0)?;
                record.form_id = remapped;
                import.cells.push(CellCreation { source_id, record });
            } else {
                // A master-owned CELL can carry children for an existing interior.
                require_interior(tx, remapped)?;
            }
            continue;
        }
        ensure!(
            record.record_type == REFR,
            "{name}: unsupported {} record {source_id:08X}",
            String::from_utf8_lossy(&record.record_type)
        );
        let view = SubrecordView::new(&record.subrecords);
        let base = view.unique_form_id(b"NAME");
        let resolve_target = |id| {
            source_identity::header_runtime_id(b"REFR", id, priority, &order)
                .filter(|&runtime| runtime != 0)
        };
        let resolved_base = base.and_then(resolve_target);
        let occluder = resolved_base == Some(super::rooms::OCCLUSION_MARKER_BASE);
        if !record.is_deleted() {
            let missing_parent = record
                .cell_form_id
                .is_some_and(|id| damaged_new_cells.contains(&id));
            let rejection = if missing_parent {
                Some("because its new CELL is damaged")
            } else if record.cell_form_id.is_some_and(|cell| {
                DiscardedFrameScope::Cell(cell).resolve(priority, &order)
                    == DiscardedFrameScope::Unknown
            }) {
                Some("with invalid parent CELL")
            } else {
                let mut primitives = view.find_all(b"XPRM");
                if resolved_base.is_none() {
                    Some("with malformed NAME")
                } else if !occluder
                    && primitives
                        .next()
                        .is_some_and(|bytes| bytes.len() > 32 || primitives.next().is_some())
                {
                    Some("with repeated or oversized XPRM")
                } else if let Some(reason) = malformed_placement_geometry(&view) {
                    Some(reason)
                } else if view.find_all(b"XESP").enumerate().any(|(index, bytes)| {
                    index != 0
                        || bytes.len() != 8
                        || SubrecordXESP::parse(bytes).is_none_or(|parent| {
                            parent.parent_ref != 0 && resolve_target(parent.parent_ref).is_none()
                        })
                }) {
                    Some("with malformed XESP")
                } else if view.find_all(b"XTEL").enumerate().any(|(index, data)| {
                    let Some((target, arrival_and_flags)) = data.split_first_chunk::<4>() else {
                        return true;
                    };
                    let target = u32::from_le_bytes(*target);
                    index != 0
                        || arrival_and_flags.len() != 28
                        || !super::valid_placement_transform(std::iter::once(
                            &arrival_and_flags[..24],
                        ))
                        || (target != 0 && resolve_target(target).is_none())
                }) {
                    Some("with malformed XTEL")
                } else {
                    None
                }
            };
            if let Some(reason) = rejection {
                eprintln!("warning: {name}: skipped placement {source_id:08X} {reason}");
                if may_link_occluder(&view, priority, &order) {
                    import
                        .unreadable_refs
                        .insert(Some(remapped), import.edits.len());
                }
                record_damage(&mut damage_after, &mut import, &record, priority, &order);
                continue;
            }
            validate_room_field_tags(&record.subrecords)?;
        }
        let room_links = base
            .filter(|id| id & 0x00ff_ffff == ROOM_MARKER_BASE)
            .map(|_| RoomLinkLayout::from_raw(&record));
        let unresolved_link = occluder
            && view.find_all(b"XORD").any(|data| {
                data.as_chunks::<4>().0.iter().any(|word| {
                    let target = u32::from_le_bytes(*word);
                    target != 0 && resolve_target(target).is_none()
                })
            });
        let marker_warnings = RemapWarnings::default();
        record.load_order = load_order;
        remap_record_form_ids(
            &mut record,
            name,
            &metadata.masters,
            &order.normal,
            &order.light,
            if occluder {
                &marker_warnings
            } else {
                &warnings
            },
        )?;
        if occluder {
            let (warnings, notices) = marker_warnings.report();
            if unresolved_link || !warnings.is_empty() || !notices.is_empty() {
                import.damaged_occluders.insert(remapped);
            }
        }
        import.edits.push(Edit {
            source_id,
            record,
            room_links,
            owns_new_id,
            inherit_room_master: false,
        });
    }
    warnings.report();
    for (index, edit) in import.edits.iter().enumerate() {
        if damage_after
            .get(&edit.record.form_id)
            .is_some_and(|&end| index < end)
            && !edit.record.is_deleted()
            && let Some(cell) = edit.record.cell_form_id
        {
            import.damage.insert(DiscardedFrameScope::Cell(cell));
        }
    }
    for id in damage_after.into_keys() {
        mark_predecessor_damage(tx, &mut import, id)?;
    }
    Ok(import)
}

fn may_link_occluder(view: &SubrecordView<'_>, priority: usize, order: &LoadOrder) -> bool {
    view.find_all(b"XORD").any(super::rooms::xord_may_link)
        && super::rooms::may_name_occlusion_marker(view.find_all(b"NAME"), priority, order)
}

fn record_damage(
    damage_after: &mut HashMap<u32, usize>,
    import: &mut Import,
    record: &RawRecord,
    priority: usize,
    order: &LoadOrder,
) -> Option<u32> {
    let scoped_cell = |cell: Option<u32>| DiscardedFrameScope::from(cell).resolve(priority, order);
    if record.record_type == CELL {
        let scope = scoped_cell(Some(record.form_id));
        import.damage.insert(scope);
    } else if super::is_placed_record_type(&record.record_type) {
        import.damage.insert(scoped_cell(record.cell_form_id));
        if let Some(id) =
            source_identity::header_runtime_id(&record.record_type, record.form_id, priority, order)
                .filter(|&id| id != 0)
        {
            damage_after.insert(id, import.edits.len());
            return Some(id);
        }
    }
    None
}

fn mark_predecessor_damage(tx: &Transaction<'_>, import: &mut Import, id: u32) -> Result<()> {
    // A prior import can move or delete the canonical placement.
    let predecessor = tx
        .prepare_cached(
            "SELECT coalesce(
                (SELECT cell_id FROM [references] WHERE id=?1),
                (SELECT cell_id FROM records WHERE form_id=?1 AND record_type='REFR'
                 AND NOT EXISTS(SELECT 1 FROM room_placement_edits WHERE form_id=?1)))",
        )?
        .query_row([id], |row| row.get::<_, Option<u32>>(0))?;
    if let Some(cell) = predecessor {
        import.damage.insert(DiscardedFrameScope::Cell(cell));
    }
    Ok(())
}

fn mark_incomplete(
    tx: &Transaction<'_>,
    damage: &HashSet<DiscardedFrameScope>,
) -> Result<HashSet<u32>> {
    const INTERIORS: &str = "INSERT INTO room_graph_cells(cell_id,complete)
         SELECT id,0 FROM cells WHERE id<>0 AND worldspace_id IS NULL
         AND grid_x IS NULL AND grid_y IS NULL";
    const INCOMPLETE: &str = "ON CONFLICT(cell_id) DO UPDATE SET complete=0 RETURNING cell_id";
    if damage.is_empty() {
        return Ok(HashSet::new());
    }
    if damage.contains(&DiscardedFrameScope::Unknown) {
        return Ok(tx
            .prepare(&format!("{INTERIORS} {INCOMPLETE}"))?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?);
    }
    let mut statement = tx.prepare(&format!("{INTERIORS} AND id=?1 {INCOMPLETE}"))?;
    let mut cells = HashSet::new();
    for &scope in damage {
        if let DiscardedFrameScope::Cell(id) = scope {
            cells.extend(
                statement
                    .query_row([id], |row| row.get::<_, u32>(0))
                    .optional()?,
            );
        }
    }
    Ok(cells)
}

fn validate_new_cell(record: &RawRecord, localized: bool) -> Result<()> {
    let id = record.form_id;
    ensure!(
        record.cell_form_id.is_none()
            && record.worldspace_form_id.is_none()
            && record.flags & !FLAG_COMPRESSED == 0,
        "unsupported CELL context or header flags for {id:08X}"
    );
    let mut tags = HashSet::new();
    for (tag, data) in &record.subrecords {
        let name = String::from_utf8_lossy(tag);
        ensure!(tags.insert(tag), "repeated CELL field {name} in {id:08X}");
        let valid = match tag.as_slice() {
            b"EDID" => valid_cell_name(data),
            b"FULL" => !localized && valid_cell_name(data),
            b"DATA" => matches!(data.as_slice(), [1] | [1, 0]),
            b"XCLL" => {
                matches!(data.len(), 72 | 92)
                    && [12, 16, 28, 32, 36, 76, 80, 84].into_iter().all(|offset| {
                        data.get(offset..)
                            .and_then(<[u8]>::first_chunk::<4>)
                            .is_none_or(|word| f32::from_le_bytes(*word).is_finite())
                    })
            }
            _ => bail!("unsupported CELL field {name} in {id:08X}"),
        };
        ensure!(valid, "invalid CELL field {name} in {id:08X}");
    }
    let view = SubrecordView::new(&record.subrecords);
    ensure!(
        view.find(b"DATA").is_some() && view.find(b"EDID").is_some(),
        "new CELL {id:08X} requires DATA and EDID"
    );
    Ok(())
}

fn valid_cell_name(data: &[u8]) -> bool {
    data.strip_suffix(&[0]).is_some_and(|name| {
        !name.is_empty() && !name.contains(&0) && std::str::from_utf8(name).is_ok()
    })
}

fn require_unused_id(tx: &Transaction<'_>, id: u32, has_history: bool) -> Result<()> {
    let occupied: bool = tx
        .prepare_cached(
            "SELECT EXISTS(
             SELECT 1 FROM records WHERE form_id=?1
             UNION ALL SELECT 1 FROM formid_map WHERE form_id=?1
             UNION ALL SELECT 1 FROM cells WHERE id=?1
             UNION ALL SELECT 1 FROM worldspaces WHERE id=?1
             UNION ALL SELECT 1 FROM [references] WHERE id=?1
             UNION ALL SELECT 1 FROM room_bounds WHERE ref_id=?1
             UNION ALL SELECT 1 FROM room_occluders WHERE ref_id=?1
             UNION ALL SELECT 1 FROM room_occluder_links WHERE ref_id=?1
             UNION ALL SELECT 1 FROM room_portals WHERE ref_id=?1
             UNION ALL SELECT 1 FROM room_placement_edits WHERE form_id=?1
         )",
        )?
        .query_row([id], |row| row.get(0))?;
    ensure!(!occupied, "FormID {id:08X} is already in use");
    if has_history {
        let historical: bool = tx
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM record_overrides WHERE form_id=?1)")?
            .query_row([id], |row| row.get(0))?;
        ensure!(!historical, "FormID {id:08X} is already in source history");
    }
    Ok(())
}

fn require_no_cell_data(tx: &Transaction<'_>, cell: u32) -> Result<()> {
    let occupied: bool = tx
        .prepare_cached(
            "SELECT EXISTS(
             SELECT 1 FROM records WHERE cell_id=?1
             UNION ALL SELECT 1 FROM [references] WHERE cell_id=?1
             UNION ALL SELECT 1 FROM room_graph_cells WHERE cell_id=?1
             UNION ALL SELECT 1 FROM room_bounds WHERE cell_id=?1
             UNION ALL SELECT 1 FROM room_occluders WHERE cell_id=?1
             UNION ALL SELECT 1 FROM room_portals WHERE cell_id=?1
             UNION ALL SELECT 1 FROM cell_environment WHERE cell_id=?1
             UNION ALL SELECT 1 FROM cell_lighting WHERE cell_id=?1
             UNION ALL SELECT 1 FROM cell_regions WHERE cell_id=?1
             UNION ALL SELECT 1 FROM land WHERE cell_id=?1
         )",
        )?
        .query_row([cell], |row| row.get(0))?;
    ensure!(!occupied, "cell {cell:08X} already has associated data");
    Ok(())
}

fn require_interior(tx: &Transaction<'_>, cell: u32) -> Result<()> {
    let interior: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM cells WHERE id=?1 AND worldspace_id IS NULL
         AND grid_x IS NULL AND grid_y IS NULL)",
        [cell],
        |row| row.get(0),
    )?;
    ensure!(
        interior && cell != 0,
        "cell {cell:08X} is not an existing interior"
    );
    Ok(())
}

fn apply_edit(
    tx: &Transaction<'_>,
    edit: &mut Edit,
    cells: &mut HashSet<u32>,
    must_be_new: bool,
    has_history: bool,
    is_plugin: bool,
) -> Result<()> {
    let record = &mut edit.record;
    let id = record.form_id;
    if must_be_new {
        ensure!(
            edit.owns_new_id,
            "placement needs a new source-owned FormID"
        );
        require_unused_id(tx, id, has_history)?;
    }
    let cell_collision: bool = tx
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM cells WHERE id=?1)")?
        .query_row([id], |row| row.get(0))?;
    ensure!(!cell_collision, "FormID belongs to a CELL");
    let canonical: Option<(String, Option<u32>, Vec<u8>)> = tx
        .query_row(
            "SELECT record_type,cell_id,data FROM records WHERE form_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let previous = tx
        .query_row(
            "SELECT cell_id,data,header_flags,enable_parent_id,enable_parent_flags
             FROM [references] WHERE id=?1",
            [id],
            |row| {
                Ok(PreviousPlacement {
                    cell: row.get(0)?,
                    data: row.get(1)?,
                    flags: row.get(2)?,
                    enable_parent_id: row.get(3)?,
                    enable_parent_flags: row.get(4)?,
                })
            },
        )
        .optional()?;
    let previously_edited: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_placement_edits WHERE form_id=?1)",
        [id],
        |row| row.get(0),
    )?;
    let (old_cell, old_data) = match (&canonical, &previous) {
        (Some((kind, _, _)), _) if kind != "REFR" => bail!("FormID belongs to a {kind} record"),
        (_, Some(previous)) => (Some(previous.cell), Some(previous.data.as_slice())),
        (Some((_, cell, data)), None) if !previously_edited => (*cell, Some(data.as_slice())),
        (_, None) => {
            ensure!(
                is_plugin || !previously_edited || record.is_deleted(),
                "native replacement requires an existing runtime enable state"
            );
            (None, None)
        }
    };
    if let Some(cell) = old_cell {
        require_interior(tx, cell)?;
        cells.insert(cell);
    }
    if old_data.is_none() && edit.owns_new_id && id > 0xff00_0000 {
        let has_shapes: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='room_occluders')",[],|row|row.get(0))?;
        ensure!(
            !has_shapes
                || !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM room_occluders WHERE ref_id=?1)",
                    [id],
                    |row| row.get::<_, bool>(0)
                )?,
            "FormID belongs to an existing native occluder"
        );
    }
    if let Some(data) = old_data {
        let old_record = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(data)?;
        let subrecords = old_record
            .subrecords
            .into_iter()
            .map(|s| (s.tag.to_vec(), s.data))
            .collect::<Vec<_>>();
        validate_room_fields(tx, &subrecords, is_plugin)?;
        if !is_plugin && !record.is_deleted() {
            let previous = previous.as_ref().ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "native replacement requires an existing runtime enable state"
                )
            })?;
            let old_view = SubrecordView::new(&subrecords);
            let parent = old_view
                .find(b"XESP")
                .and_then(SubrecordXESP::parse)
                .map(|parent| (parent.parent_ref, parent.flags))
                .unzip();
            ensure!(
                parent == (previous.enable_parent_id, previous.enable_parent_flags),
                "native replacement cannot preserve existing enable-parent data"
            );
            record.flags |= previous.flags & INITIALLY_DISABLED;
            if edit.inherit_room_master
                && old_view.unique_form_id(b"NAME") == Some(ROOM_MARKER_BASE)
            {
                let mut headers = old_view.find_all(b"XRMR");
                if let Some([_, _, master, _]) = headers.next()
                    && headers.next().is_none()
                {
                    let master_byte = record
                        .subrecords
                        .iter_mut()
                        .find(|(tag, _)| tag == b"XRMR")
                        .and_then(|(_, header)| header.get_mut(2))
                        .ok_or_else(|| {
                            color_eyre::eyre::eyre!("native room edit has no XRMR master byte")
                        })?;
                    *master_byte = *master;
                    edit.room_links = Some(RoomLinkLayout::from_raw(record));
                }
            }
            record
                .subrecords
                .extend(subrecords.into_iter().filter(|(tag, _)| tag == b"XESP"));
        }
    }
    ensure!(
        old_data.is_some() || ((edit.owns_new_id || previously_edited) && !record.is_deleted()),
        "FormID has no existing placement"
    );
    if !must_be_new && old_data.is_none() && !previously_edited {
        require_unused_id(tx, id, has_history)?;
    }
    // Each supported reference has only room and transform semantics. Its old memberships
    // and assignment rule are discarded even when it moves out of the old cell.
    tx.execute("DELETE FROM reference_rooms WHERE reference_id=?1", [id])?;
    tx.execute(
        "DELETE FROM room_reference_rules WHERE reference_id=?1",
        [id],
    )?;
    if record.is_deleted() {
        tx.execute("DELETE FROM [references] WHERE id=?1", [id])?;
        return Ok(());
    }
    let cell = record
        .cell_form_id
        .filter(|_| record.worldspace_form_id.is_none())
        .ok_or_else(|| color_eyre::eyre::eyre!("placement has no interior parent cell"))?;
    require_interior(tx, cell)?;
    validate_placement(tx, record, is_plugin)?;
    cells.insert(cell);
    insert_reference(tx, id, cell, None, record.flags, &record.subrecords)?;
    Ok(())
}

fn validate_placement(tx: &Transaction<'_>, record: &RawRecord, allow_doors: bool) -> Result<()> {
    validate_room_fields(tx, &record.subrecords, allow_doors)?;
    let view = SubrecordView::new(&record.subrecords);
    let base = view.unique_form_id(b"NAME");
    let deleted: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM subset_nonplacement_deletions WHERE form_id=?1)",
        )?
        .query_row([base], |row| row.get(0))?;
    ensure!(!deleted, "placement base was deleted by a subset export");
    if let Some(bounds) = view.find(b"XMBO") {
        ensure!(
            view.find(b"XPRM").and_then(|data| data.get(..12)) == Some(bounds),
            "XMBO extents disagree with XPRM"
        );
    }
    if let Some(reason) = malformed_placement_geometry(&view) {
        bail!("placement {reason}");
    }
    Ok(())
}

fn malformed_placement_geometry(view: &SubrecordView<'_>) -> Option<&'static str> {
    if !super::valid_placement_transform(view.find_all(b"DATA")) {
        return Some("with malformed DATA");
    }
    let mut scales = view.find_all(b"XSCL");
    if let Some(bytes) = scales.next()
        && (scales.next().is_some()
            || !<[u8; 4]>::try_from(bytes).is_ok_and(|word| {
                let scale = f32::from_le_bytes(word);
                scale.is_finite() && scale > 0.0
            }))
    {
        return Some("with malformed XSCL");
    }
    None
}

fn validate_room_fields(
    tx: &Transaction<'_>,
    subrecords: &[(Vec<u8>, Vec<u8>)],
    allow_doors: bool,
) -> Result<()> {
    validate_room_field_tags(subrecords)?;
    let view = SubrecordView::new(subrecords);
    let base = view
        .unique_form_id(b"NAME")
        .ok_or_else(|| color_eyre::eyre::eyre!("placement requires one base FormID"))?;
    let mut marker_bounds = view.find_all(b"XMBO");
    if let Some(bounds) = marker_bounds.next() {
        ensure!(
            base == ROOM_MARKER_BASE && bounds.len() == 12 && marker_bounds.next().is_none(),
            "XMBO requires one 12-byte room marker extent"
        );
    }
    let mut is_door = false;
    if base == OCCLUSION_MARKER_BASE {
        ensure!(tx.query_row(
            "SELECT count(*)=2 FROM room_graph_stats WHERE name IN ('occlusion_version','occlusion_links_verified') AND value=1",
            [], |row| row.get::<_, bool>(0))?, "occluder edits require a source-verified pack");
        for tag in [b"XMBR", b"XSCL", b"XESP"] {
            ensure!(
                view.find_all(tag).nth(1).is_none(),
                "ambiguous occluder field {}",
                String::from_utf8_lossy(tag)
            );
        }
    } else if !matches!(base, ROOM_MARKER_BASE | PORTAL_MARKER_BASE) {
        ensure!(
            view.find(b"XPRM").is_none(),
            "static primitives are outside the room placement format"
        );
        let kind: Option<String> = tx
            .prepare_cached("SELECT record_type FROM records WHERE form_id=?1")?
            .query_row([base], |row| row.get(0))
            .optional()?;
        is_door = allow_doors && kind.as_deref() == Some("DOOR");
        ensure!(
            kind.as_deref() == Some("STAT") || is_door,
            "unsupported placement base {base:08X}"
        );
    }
    ensure!(
        is_door || view.find(b"XTEL").is_none(),
        "XTEL requires a DOOR base"
    );
    Ok(())
}

fn validate_room_field_tags(subrecords: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
    for (tag, _) in subrecords {
        ensure!(
            matches!(
                tag.as_slice(),
                b"EDID"
                    | b"NAME"
                    | b"DATA"
                    | b"XSCL"
                    | b"XMBR"
                    | b"XORD"
                    | b"XMBO"
                    | b"XPRM"
                    | b"XRMR"
                    | b"XLRM"
                    | b"XPOD"
                    | b"LNAM"
                    | b"INAM"
                    | b"XESP"
                    | b"XTEL"
            ),
            "unsupported REFR field {}",
            String::from_utf8_lossy(tag)
        );
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePlacements {
    #[serde(default)]
    cells: Vec<NativeCell>,
    #[serde(default)]
    edits: Vec<NativeEdit>,
}

impl NativePlacements {
    fn into_import(self, geometric_ownership: bool) -> Result<Import> {
        Ok(Import {
            cells: self
                .cells
                .into_iter()
                .map(NativeCell::into_creation)
                .collect::<Result<_>>()?,
            edits: self
                .edits
                .into_iter()
                .map(|edit| edit.into_edit(geometric_ownership))
                .collect::<Result<_>>()?,
            ..Import::default()
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCell {
    id: u32,
    editor_id: String,
    full_name: Option<String>,
}

impl NativeCell {
    fn into_creation(self) -> Result<CellCreation> {
        let id = self.id;
        ensure!(
            id >= NATIVE_FORM_START,
            "new native CELL {id:08X} needs a native FormID"
        );
        let terminated = |name: String| {
            let mut data = name.into_bytes();
            data.push(0);
            data
        };
        let mut subrecords = vec![
            (b"EDID".to_vec(), terminated(self.editor_id)),
            (b"DATA".to_vec(), vec![1, 0]),
        ];
        if let Some(name) = self.full_name {
            subrecords.push((b"FULL".to_vec(), terminated(name)));
        }
        let record = RawRecord {
            form_id: id,
            record_type: CELL,
            flags: 0,
            subrecords,
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        validate_new_cell(&record, false)?;
        Ok(CellCreation {
            source_id: id,
            record,
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum NativeEdit {
    Put {
        id: u32,
        cell: u32,
        base: u32,
        position: [f32; 3],
        #[serde(default)]
        rotation: [f32; 3],
        scale: Option<f32>,
        shape: Option<NativeOccluderShape>,
        links: Option<[u32; 4]>,
        #[serde(default)]
        ownership: NativeOwnership,
    },
    PutRoom {
        id: u32,
        cell: u32,
        position: [f32; 3],
        #[serde(default)]
        rotation: [f32; 3],
        half_extents: [f32; 3],
        master: Option<bool>,
        #[serde(default)]
        links: Vec<u32>,
    },
    PutPortal {
        id: u32,
        cell: u32,
        position: [f32; 3],
        #[serde(default)]
        rotation: [f32; 3],
        half_extents: [f32; 2],
        endpoints: [u32; 2],
    },
    Delete {
        id: u32,
    },
}

#[derive(Default, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum NativeOwnership {
    #[default]
    Auto,
    None,
    Target {
        room: u32,
    },
}

impl NativeEdit {
    fn into_edit(self, geometric_ownership: bool) -> Result<Edit> {
        let mut room_links = None;
        let mut inherit_room_master = false;
        let record = match self {
            Self::Put {
                id,
                cell,
                base,
                position,
                rotation,
                scale,
                shape,
                links,
                ownership,
            } => {
                ensure!(
                    !matches!(base, ROOM_MARKER_BASE | PORTAL_MARKER_BASE),
                    "native put requires a STAT base"
                );
                let mut record = native_reference(id, cell, base, position, rotation);
                ensure!(
                    (base == OCCLUSION_MARKER_BASE) == shape.is_some(),
                    "PlaneMarker placements require a shape and other bases cannot supply one"
                );
                ensure!(
                    links.is_none() || shape.is_some(),
                    "only occluders accept XORD slots"
                );
                if let Some(shape) = shape {
                    record
                        .subrecords
                        .push((b"XPRM".to_vec(), shape.primitive_bytes()?));
                }
                if let Some(slots) = links.filter(|slots| slots.iter().any(|&id| id != 0)) {
                    record
                        .subrecords
                        .push((b"XORD".to_vec(), slots.map(u32::to_le_bytes).concat()));
                }
                if let Some(scale) = scale {
                    record
                        .subrecords
                        .push((b"XSCL".to_vec(), scale.to_le_bytes().to_vec()));
                }
                record.flags = match ownership {
                    NativeOwnership::Auto => 0,
                    NativeOwnership::None => MULTIBOUND_REFERENCE_FLAG,
                    NativeOwnership::Target { room } => {
                        ensure!(room != 0, "ownership target must have a nonzero FormID");
                        record
                            .subrecords
                            .push((b"XMBR".to_vec(), room.to_le_bytes().to_vec()));
                        if geometric_ownership {
                            MULTIBOUND_REFERENCE_FLAG
                        } else {
                            0
                        }
                    }
                };
                record
            }
            Self::PutRoom {
                id,
                cell,
                position,
                rotation,
                half_extents,
                master,
                links,
            } => {
                ensure!(
                    half_extents
                        .iter()
                        .all(|value| value.is_finite() && *value >= 0.0),
                    "room half-extents must be finite and nonnegative"
                );
                let mut record = native_reference(id, cell, ROOM_MARKER_BASE, position, rotation);
                record
                    .subrecords
                    .push(native_primitive(half_extents, SHAPE_BOX));
                ensure!(links.len() <= 0x3fff, "room has too many links");
                inherit_room_master = master.is_none();
                let [low, high] = u16::try_from(links.len())?.to_le_bytes();
                record.subrecords.push((
                    b"XRMR".to_vec(),
                    vec![low, high, u8::from(master.unwrap_or(false)), 0],
                ));
                record.subrecords.extend(
                    links
                        .into_iter()
                        .map(|id| (b"XLRM".to_vec(), id.to_le_bytes().to_vec())),
                );
                room_links = Some(RoomLinkLayout::from_raw(&record));
                record
            }
            Self::PutPortal {
                id,
                cell,
                position,
                rotation,
                half_extents: [x, z],
                endpoints,
            } => {
                ensure!(
                    x.is_finite() && x > 0.0 && z.is_finite() && z > 0.0,
                    "portal half-extents must be finite and positive"
                );
                let mut record = native_reference(id, cell, PORTAL_MARKER_BASE, position, rotation);
                record
                    .subrecords
                    .push(native_primitive([x, 0.0, z], SHAPE_PORTAL));
                record.subrecords.push((
                    b"XPOD".to_vec(),
                    endpoints.into_iter().flat_map(u32::to_le_bytes).collect(),
                ));
                record
            }
            Self::Delete { id } => RawRecord {
                form_id: id,
                record_type: REFR,
                flags: 0x20,
                subrecords: Vec::new(),
                cell_form_id: None,
                worldspace_form_id: None,
                load_order: 0,
            },
        };
        let id = record.form_id;
        Ok(Edit {
            source_id: id,
            record,
            room_links,
            owns_new_id: id >= NATIVE_FORM_START,
            inherit_room_master,
        })
    }
}

fn native_reference(
    id: u32,
    cell: u32,
    base: u32,
    position: [f32; 3],
    rotation: [f32; 3],
) -> RawRecord {
    RawRecord {
        form_id: id,
        record_type: REFR,
        flags: 0,
        subrecords: vec![
            (b"NAME".to_vec(), base.to_le_bytes().to_vec()),
            (
                b"DATA".to_vec(),
                position
                    .into_iter()
                    .chain(rotation)
                    .flat_map(f32::to_le_bytes)
                    .collect(),
            ),
        ],
        cell_form_id: Some(cell),
        worldspace_form_id: None,
        load_order: 0,
    }
}

fn native_primitive(half_extents: [f32; 3], shape: u32) -> (Vec<u8>, Vec<u8>) {
    let mut data: Vec<_> = half_extents
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    data.resize(28, 0);
    data.extend(shape.to_le_bytes());
    (b"XPRM".to_vec(), data)
}
