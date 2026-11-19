//! Offline, append-only edits to interior room placements in an existing pack.

use super::{
    RemapWarnings,
    binary::{ParsedRecord, PluginMetadata, parse_plugin_bytes, plugin_metadata_from_bytes},
    exporter::insert_reference,
    extractors::SubrecordView,
    load_order::{LoadOrder, normalize_plugin_name},
    records::RawRecord,
    remap_record_form_ids,
    rooms::{PORTAL_MARKER_BASE, ROOM_MARKER_BASE, refresh_placement_cells},
    types::ArchivedRecordData,
};
use crate::{cache::hash_bytes, check::WORLD_DATABASE_FILE_NAME, records::source_identity};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::Deserialize;
use shared::{asset_lock::AssetLock, records::MULTIBOUND_REFERENCE_FLAG};
use std::{collections::HashSet, fs, path::Path};

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
const REFR: [u8; 4] = *b"REFR";

/// Source to apply after the pack's original conversion and any earlier placement edits.
#[derive(Clone, Copy)]
pub enum PlacementSource<'a> {
    /// Placement-only Skyrim plugin. Its master list is resolved against the pack and prior imports.
    SkyrimPlugin(&'a Path),
    /// MudCrab placement JSON using this pack's runtime FormIDs.
    NativeJson(&'a Path),
}

#[derive(Debug, Default)]
pub struct PlacementEditReport {
    pub changed_references: usize,
    pub affected_cells: Vec<u32>,
    pub certified_cells: Vec<u32>,
}

/// Apply one source under the pack lock. Runtime placements, room projections and the retained
/// edit source commit together. Reapplying an already imported source is a no-op.
pub fn apply(assets: &Path, source: PlacementSource<'_>) -> Result<PlacementEditReport> {
    let lock = AssetLock::acquire_exclusive(assets)?;
    let database = lock.asset_dir().join(WORLD_DATABASE_FILE_NAME);
    let file = fs::symlink_metadata(&database)?;
    ensure!(
        file.is_file(),
        "{} is not a regular file",
        database.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            file.nlink() == 1,
            "the pack needs a private copy of skyrim_world.db"
        );
    }
    let mut conn = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .wrap_err_with(|| format!("failed to open {}", database.display()))?;
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
    let versions: u32 = tx.query_row(
        "SELECT count(*) FROM room_graph_stats WHERE
         (name='visibility_version' AND value=2) OR (name='assignment_version' AND value=1)",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        versions == 2,
        "placement edits require a versioned room graph"
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
    let (edits, metadata) = if is_plugin {
        let metadata = plugin_metadata_from_bytes(&bytes)?;
        (plugin_edits(&tx, &name, &bytes, &metadata)?, Some(metadata))
    } else {
        let edits = serde_json::from_slice::<NativePlacements>(&bytes)
            .wrap_err("invalid native placement JSON")?;
        (
            edits
                .edits
                .into_iter()
                .map(NativeEdit::into_edit)
                .collect::<Result<_>>()?,
            None,
        )
    };
    ensure!(!edits.is_empty(), "{name} contains no placement edits");

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
    let mut cells = HashSet::new();
    let mut changed = HashSet::new();
    for edit in &edits {
        let id = edit.record.form_id;
        ensure!(changed.insert(id), "duplicate placement edit for {id:08X}");
        apply_edit(&tx, edit, &mut cells).wrap_err_with(|| format!("placement {id:08X}"))?;
        tx.execute(
            "INSERT INTO room_placement_edits VALUES (?1,?2,?3,?4)",
            params![layer, id, edit.source_id, edit.record.is_deleted()],
        )?;
    }
    let certified_cells = refresh_placement_cells(&tx, &cells, &changed)?;
    tx.commit()?;
    let mut affected_cells = cells.into_iter().collect::<Vec<_>>();
    affected_cells.sort_unstable();
    Ok(PlacementEditReport {
        changed_references: edits.len(),
        affected_cells,
        certified_cells,
    })
}

struct Edit {
    source_id: u32,
    record: RawRecord,
    owns_new_id: bool,
}

fn plugin_edits(
    tx: &Transaction<'_>,
    name: &str,
    bytes: &[u8],
    metadata: &PluginMetadata,
) -> Result<Vec<Edit>> {
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
    order.names.push(name.to_owned());
    order.metadata.push(metadata.clone());
    let order = LoadOrder::from_metadata(order.names, order.metadata)?;
    let mut candidates = Vec::new();
    let parse_warnings = parse_plugin_bytes(bytes, name, |candidate| candidates.push(candidate))?;
    ensure!(
        parse_warnings.is_empty(),
        "{name} contains malformed records or groups"
    );
    let warnings = RemapWarnings::default();
    let mut edits = Vec::new();
    for candidate in candidates {
        let ParsedRecord::Usable(mut record) = candidate else {
            bail!("{name} contains a damaged placement");
        };
        let source_id = record.form_id;
        let remapped =
            source_identity::header_runtime_id(&record.record_type, source_id, priority, &order)
                .filter(|&id| id != 0)
                .ok_or_else(|| {
                    color_eyre::eyre::eyre!("{name}: invalid record FormID {source_id:08X}")
                })?;
        if record.record_type == *b"CELL" && !record.is_deleted() {
            // A group carrier can include an existing CELL record. Only its interior identity
            // participates in placement import.
            require_interior(tx, remapped)?;
            continue;
        }
        ensure!(
            record.record_type == REFR,
            "{name}: unsupported {} record {source_id:08X}",
            String::from_utf8_lossy(&record.record_type)
        );
        if !record.is_deleted() {
            validate_room_field_tags(&record.subrecords)?;
        }
        record.load_order = priority as u32;
        remap_record_form_ids(
            &mut record,
            name,
            &metadata.masters,
            &order.normal,
            &order.light,
            &warnings,
        )?;
        edits.push(Edit {
            source_id,
            record,
            owns_new_id: (source_id >> 24) as usize == metadata.masters.len(),
        });
    }
    warnings.report();
    Ok(edits)
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

fn apply_edit(tx: &Transaction<'_>, edit: &Edit, cells: &mut HashSet<u32>) -> Result<()> {
    let record = &edit.record;
    let id = record.form_id;
    let canonical: Option<(String, Option<u32>, Vec<u8>)> = tx
        .query_row(
            "SELECT record_type,cell_id,data FROM records WHERE form_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let previous: Option<(u32, Vec<u8>)> = tx
        .query_row(
            "SELECT cell_id,data FROM [references] WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let previously_edited: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_placement_edits WHERE form_id=?1)",
        [id],
        |row| row.get(0),
    )?;
    let (old_cell, old_data) = match (&canonical, &previous) {
        (Some((kind, _, _)), _) if kind != "REFR" => bail!("FormID belongs to a {kind} record"),
        (_, Some((cell, data))) => (Some(*cell), Some(data.as_slice())),
        (Some((_, cell, data)), None) if !previously_edited => (*cell, Some(data.as_slice())),
        (_, None) => (None, None),
    };
    if let Some(cell) = old_cell {
        require_interior(tx, cell)?;
        cells.insert(cell);
    }
    if let Some(data) = old_data {
        let old_record = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(data)?;
        let subrecords = old_record
            .subrecords
            .into_iter()
            .map(|s| (s.tag.to_vec(), s.data))
            .collect::<Vec<_>>();
        validate_room_fields(tx, &subrecords)?;
    }
    ensure!(
        old_data.is_some() || ((edit.owns_new_id || previously_edited) && !record.is_deleted()),
        "FormID has no existing placement"
    );
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
    validate_room_fields(tx, &record.subrecords)?;
    let view = SubrecordView::new(&record.subrecords);
    let mut transforms = view.find_all(b"DATA");
    let transform = transforms
        .next()
        .and_then(|data| <&[u8; 24]>::try_from(data).ok());
    ensure!(
        transforms.next().is_none()
            && transform.is_some_and(|data| data
                .as_chunks::<4>()
                .0
                .iter()
                .all(|word| f32::from_le_bytes(*word).is_finite())),
        "invalid placement transform"
    );
    ensure!(
        view.find(b"XSCL")
            .is_none_or(
                |data| <[u8; 4]>::try_from(data)
                    .is_ok_and(|word| f32::from_le_bytes(word).is_finite()
                        && f32::from_le_bytes(word) > 0.0)
            ),
        "invalid placement scale"
    );
    cells.insert(cell);
    insert_reference(tx, id, cell, None, record.flags, &record.subrecords)?;
    Ok(())
}

fn validate_room_fields(tx: &Transaction<'_>, subrecords: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
    validate_room_field_tags(subrecords)?;
    let view = SubrecordView::new(subrecords);
    let mut bases = view.find_all(b"NAME");
    let base = bases
        .next()
        .and_then(|data| <[u8; 4]>::try_from(data).ok())
        .map(u32::from_le_bytes)
        .filter(|_| bases.next().is_none())
        .ok_or_else(|| color_eyre::eyre::eyre!("placement requires one base FormID"))?;
    if !matches!(base, ROOM_MARKER_BASE | PORTAL_MARKER_BASE) {
        ensure!(
            view.find(b"XPRM").is_none(),
            "static primitives are outside the room placement format"
        );
        let is_static: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM records WHERE form_id=?1 AND record_type='STAT')",
            [base],
            |row| row.get(0),
        )?;
        ensure!(is_static, "base {base:08X} is not an existing STAT");
    }
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
                    | b"XPRM"
                    | b"XRMR"
                    | b"XLRM"
                    | b"XPOD"
                    | b"LNAM"
                    | b"INAM"
                    | b"XESP"
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
    edits: Vec<NativeEdit>,
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
        #[serde(default)]
        ownership: NativeOwnership,
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
    fn into_edit(self) -> Result<Edit> {
        let (id, record) = match self {
            Self::Put {
                id,
                cell,
                base,
                position,
                rotation,
                scale,
                ownership,
            } => {
                ensure!(
                    !matches!(base, ROOM_MARKER_BASE | PORTAL_MARKER_BASE),
                    "native put requires a STAT base"
                );
                let transform = position
                    .into_iter()
                    .chain(rotation)
                    .flat_map(f32::to_le_bytes)
                    .collect();
                let mut subrecords = vec![
                    (b"NAME".to_vec(), base.to_le_bytes().to_vec()),
                    (b"DATA".to_vec(), transform),
                ];
                if let Some(scale) = scale {
                    subrecords.push((b"XSCL".to_vec(), scale.to_le_bytes().to_vec()));
                }
                let flags = match ownership {
                    NativeOwnership::Auto => 0,
                    NativeOwnership::None => MULTIBOUND_REFERENCE_FLAG,
                    NativeOwnership::Target { room } => {
                        ensure!(room != 0, "ownership target must have a nonzero FormID");
                        subrecords.push((b"XMBR".to_vec(), room.to_le_bytes().to_vec()));
                        0
                    }
                };
                (
                    id,
                    RawRecord {
                        form_id: id,
                        record_type: REFR,
                        flags,
                        subrecords,
                        cell_form_id: Some(cell),
                        worldspace_form_id: None,
                        load_order: 0,
                    },
                )
            }
            Self::Delete { id } => (
                id,
                RawRecord {
                    form_id: id,
                    record_type: REFR,
                    flags: 0x20,
                    subrecords: Vec::new(),
                    cell_form_id: None,
                    worldspace_form_id: None,
                    load_order: 0,
                },
            ),
        };
        Ok(Edit {
            source_id: id,
            record,
            owns_new_id: id >= NATIVE_FORM_START,
        })
    }
}
