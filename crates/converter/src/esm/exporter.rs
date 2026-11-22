use crate::esm::{
    default_object_pairs, emittance,
    extractors::{SubrecordView, extract_cell_info, extract_land_data, serialize_subrecords},
    load_order::{LoadOrder, StableId},
    projection_omissions::{OmissionCategory, ProjectionOmissions},
    records::{RawRecord, achr::SubrecordXESP},
    rooms::RoomLinkSources,
};
use crate::{
    asset_path::{AssetKind, canonical_asset_path},
    esm::records::record_type::vmad::parse_vmad,
    records::{DecodedRecord, ReadResult, source_identity},
};
use rusqlite::{Connection, OptionalExtension, Result, Statement, ToSql, Transaction, params};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    str::from_utf8,
};

const CELL_SIZE: f32 = 4096.0;
const RECORDS_INSERT_SQL: &str = "INSERT OR REPLACE INTO records(form_id, record_type, cell_id, worldspace_id, load_order, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

/// Validate the fixed row shape once before any raw-bound record is executed.
fn check_record_insert(statement: &Statement<'_>) -> Result<()> {
    let expected = statement.parameter_count();
    if expected != 6 {
        // Match the old positional-slice cardinality diagnostic. This
        // constant SQL always has six; never step a drifted statement.
        let observed = 6.min(expected.saturating_add(1));
        return Err(rusqlite::Error::InvalidParameterCount(observed, expected));
    }
    Ok(())
}

/// Write one canonical row with unchanged positional values and clear successful bindings.
/// A failed bind or execute returns before clearing; dropping the cached statement resets it.
#[inline]
fn insert_canonical_record(
    statement: &mut Statement<'_>,
    form_id: u32,
    record_type: impl ToSql,
    cell_form_id: Option<u32>,
    worldspace_form_id: Option<u32>,
    load_order: u32,
    data: &[u8],
) -> Result<usize> {
    statement.raw_bind_parameter(1, form_id)?;
    statement.raw_bind_parameter(2, record_type)?;
    statement.raw_bind_parameter(3, cell_form_id)?;
    statement.raw_bind_parameter(4, worldspace_form_id)?;
    statement.raw_bind_parameter(5, load_order)?;
    statement.raw_bind_parameter(6, data)?;
    let changed = statement.raw_execute()?;
    statement.clear_bindings();
    Ok(changed)
}

#[cfg(test)]
#[path = "canonical_bind_tests.rs"]
mod canonical_bind_tests;

fn invalid_movement_field(form_id: u32, field: &str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("record {form_id:08X} has invalid {field}"),
    )))
}

/// Decode the editor ID and the eight directional walk and run speeds of a `MOVT`.
///
/// Retail movement types can hold zero speeds (a creature that cannot strafe has
/// zero lateral speeds), so only negative and non-finite values are rejected. The
/// engine checks the forward speed that a selected profile must supply. A `MOVT`
/// with no editor ID is malformed: the table never holds an empty name as if it
/// were authored.
fn decode_movement_type(view: &SubrecordView<'_>, form_id: u32) -> Result<(String, [f32; 8])> {
    let editor_id = view
        .get_string(b"EDID")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid_movement_field(form_id, "MOVT EDID (missing)"))?;
    let data = view
        .find(b"SPED")
        .ok_or_else(|| invalid_movement_field(form_id, "MOVT SPED"))?;
    if data.len() != 40 && data.len() != 44 {
        return Err(invalid_movement_field(
            form_id,
            "MOVT SPED length (expected 40 or 44 bytes)",
        ));
    }
    let mut speeds = [0.0; 8];
    for (index, speed) in speeds.iter_mut().enumerate() {
        *speed = f32::from_le_bytes(data[index * 4..index * 4 + 4].try_into().unwrap());
        if !speed.is_finite() || *speed < 0.0 {
            return Err(invalid_movement_field(
                form_id,
                &format!("MOVT SPED[{index}]"),
            ));
        }
    }
    Ok((editor_id, speeds))
}

fn decode_movement_setting(view: &SubrecordView<'_>, form_id: u32) -> Result<f32> {
    let data = view
        .find(b"DATA")
        .ok_or_else(|| invalid_movement_field(form_id, "GMST DATA"))?;
    if data.len() != 4 {
        return Err(invalid_movement_field(
            form_id,
            "GMST DATA length (expected 4 bytes)",
        ));
    }
    let value = f32::from_le_bytes(data.try_into().unwrap());
    if !value.is_finite() {
        return Err(invalid_movement_field(form_id, "GMST DATA value"));
    }
    Ok(value)
}

fn optional_race_movement_link(
    view: &SubrecordView<'_>,
    form_id: u32,
    tag: &[u8; 4],
) -> Result<Option<u32>> {
    let Some(data) = view.find(tag) else {
        return Ok(None);
    };
    if data.len() != 4 {
        return Err(invalid_movement_field(
            form_id,
            &format!(
                "RACE {} length (expected 4 bytes)",
                String::from_utf8_lossy(tag)
            ),
        ));
    }
    let value = u32::from_le_bytes(data.try_into().unwrap());
    Ok((value != 0).then_some(value))
}

/// Creates any missing world-database tables and stamps an empty database with the current schema.
pub fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS schema_info (version INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS plugins (
             id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, priority INTEGER NOT NULL, checksum BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS records (
             form_id INTEGER PRIMARY KEY, record_type TEXT NOT NULL, cell_id INTEGER,
             worldspace_id INTEGER, load_order INTEGER NOT NULL, data BLOB NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_records_type ON records(record_type);
         CREATE INDEX IF NOT EXISTS idx_records_cell_id ON records(cell_id) WHERE cell_id IS NOT NULL;
         CREATE TABLE IF NOT EXISTS worldspaces (
             id INTEGER PRIMARY KEY, editor_id TEXT NOT NULL, parent_world INTEGER, flags INTEGER NOT NULL,
             lod_origin_x INTEGER, lod_origin_y INTEGER, full_name TEXT
         );
         CREATE TABLE IF NOT EXISTS cells (
             id INTEGER PRIMARY KEY, worldspace_id INTEGER, grid_x INTEGER, grid_y INTEGER,
             interior_name TEXT, flags INTEGER NOT NULL, data BLOB, full_name TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_cells_grid ON cells(worldspace_id, grid_x, grid_y);
         CREATE TABLE IF NOT EXISTS "references" (
             id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL, worldspace_id INTEGER,
             base_form_id INTEGER NOT NULL, is_exterior INTEGER NOT NULL,
             pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
             local_x REAL, local_y REAL, rot_x REAL NOT NULL, rot_y REAL NOT NULL,
             rot_z REAL NOT NULL, scale REAL NOT NULL DEFAULT 1.0,
             radius_override REAL, header_flags INTEGER NOT NULL DEFAULT 0,
             enable_parent_id INTEGER, enable_parent_flags INTEGER, emittance_id INTEGER, data BLOB
         );
         CREATE INDEX IF NOT EXISTS idx_references_cell ON "references"(cell_id);
         CREATE VIRTUAL TABLE IF NOT EXISTS exterior_spatial USING rtree(
             id, minX, maxX, minY, maxY, minZ, maxZ, +cell_id, +worldspace_id
         );
         CREATE TABLE IF NOT EXISTS land (
             cell_id INTEGER PRIMARY KEY, heightmap BLOB NOT NULL, vtex BLOB, vclr BLOB, normals BLOB
         );
         CREATE TABLE IF NOT EXISTS statics (
             id INTEGER PRIMARY KEY, editor_id TEXT, model_path TEXT, flags INTEGER NOT NULL,
             full_name TEXT,
             bounds_min_x REAL NOT NULL DEFAULT -64, bounds_min_y REAL NOT NULL DEFAULT -64,
             bounds_min_z REAL NOT NULL DEFAULT -64, bounds_max_x REAL NOT NULL DEFAULT 64,
             bounds_max_y REAL NOT NULL DEFAULT 64, bounds_max_z REAL NOT NULL DEFAULT 64,
             bounds_valid INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_statics_editor_id ON statics(editor_id);
         CREATE TABLE IF NOT EXISTS lights (
             id INTEGER PRIMARY KEY,        -- LIGH FormID
             editor_id TEXT,
             radius REAL NOT NULL,          -- Creation units
             color_r INTEGER NOT NULL, color_g INTEGER NOT NULL, color_b INTEGER NOT NULL,
             flags INTEGER NOT NULL,        -- DATA flags (dynamic, can carry, negative, flicker, off by default, ...)
             falloff REAL NOT NULL,
             fade REAL,                     -- FNAM, if present
             time INTEGER, field_of_view REAL, near_clip REAL,
             flicker_period REAL, flicker_intensity_amplitude REAL, flicker_movement_amplitude REAL,
             value INTEGER, weight REAL,
             sound_id INTEGER,              -- SNAM -> SNDR
             lens_id INTEGER                -- LNAM -> LENS
         );
         CREATE TABLE IF NOT EXISTS emittance (
             id INTEGER PRIMARY KEY,        -- XEMI target: a LIGH or REGN FormID
             source_type TEXT NOT NULL,     -- 'LIGH' or 'REGN'
             weather_id INTEGER,            -- REGN: the WTHR its colours come from
             sunrise_r INTEGER NOT NULL, sunrise_g INTEGER NOT NULL, sunrise_b INTEGER NOT NULL,
             day_r INTEGER NOT NULL, day_g INTEGER NOT NULL, day_b INTEGER NOT NULL,
             sunset_r INTEGER NOT NULL, sunset_g INTEGER NOT NULL, sunset_b INTEGER NOT NULL,
             night_r INTEGER NOT NULL, night_g INTEGER NOT NULL, night_b INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS npcs (
             id INTEGER PRIMARY KEY, editor_id TEXT, full_name TEXT,
             race_id INTEGER, class_id INTEGER, flags INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_npcs_editor_id ON npcs(editor_id);
         CREATE TABLE IF NOT EXISTS actor_models (
             id INTEGER PRIMARY KEY,          -- NPC_ FormID
             race_id INTEGER NOT NULL,
             sex TEXT NOT NULL,               -- 'male' | 'female'
             skin_armo_id INTEGER,            -- effective NPC WNAM override, else effective RACE/WNAM
             head_model TEXT,                 -- FaceGen or default head NIF path
             head_source TEXT,                -- 'facegen' | 'default'
             resolution TEXT NOT NULL,        -- 'body', 'body_head', 'unresolved'
             skin_tone_r REAL,                -- NPC_/QNAM, 0..1, encoded scale
             skin_tone_g REAL,
             skin_tone_b REAL,
             weight REAL                      -- NPC_/NAM7, 0..100
         );
         CREATE TABLE IF NOT EXISTS actor_model_parts (
             npc_id INTEGER NOT NULL,
             slot TEXT NOT NULL,              -- 'body' | 'hands' | 'feet'
             model_path TEXT NOT NULL,
             armature_race_id INTEGER,        -- ARMA/RNAM: the NPC's race or DefaultRace (25)
             skin_texture_set_id INTEGER,     -- ARMA/NAM0 (male) or NAM1 (female), a TXST
             PRIMARY KEY (npc_id, slot)
         );
         CREATE TABLE IF NOT EXISTS movement_types (
             id INTEGER PRIMARY KEY, editor_id TEXT NOT NULL,
             left_walk REAL NOT NULL, left_run REAL NOT NULL,
             right_walk REAL NOT NULL, right_run REAL NOT NULL,
             forward_walk REAL NOT NULL, forward_run REAL NOT NULL,
             back_walk REAL NOT NULL, back_run REAL NOT NULL,
             load_order INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS race_movement_links (
             id INTEGER PRIMARY KEY, walk_movt_id INTEGER, run_movt_id INTEGER,
             load_order INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS default_objects (
             key TEXT PRIMARY KEY,          -- DOBJ DNAM use code, for example DMWL
             form_id INTEGER NOT NULL       -- load-order FormID; a null link has no row
         );
         CREATE TABLE IF NOT EXISTS movement_game_settings (
             id INTEGER PRIMARY KEY, editor_id TEXT NOT NULL,
             value REAL NOT NULL, load_order INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS lod (
             cell_id INTEGER NOT NULL, lod_level INTEGER NOT NULL, mesh_data BLOB NOT NULL,
             PRIMARY KEY (cell_id, lod_level)
         );
         CREATE TABLE IF NOT EXISTS lod_chunks (
             worldspace_id INTEGER NOT NULL, tier INTEGER NOT NULL,
             anchor_x INTEGER NOT NULL, anchor_y INTEGER NOT NULL,
             payload_path TEXT NOT NULL, content_hash TEXT NOT NULL,
             bounds_min_x REAL NOT NULL, bounds_min_y REAL NOT NULL, bounds_min_z REAL NOT NULL,
             bounds_max_x REAL NOT NULL, bounds_max_y REAL NOT NULL, bounds_max_z REAL NOT NULL,
             source_cells TEXT NOT NULL DEFAULT '',
             PRIMARY KEY (worldspace_id, tier, anchor_x, anchor_y)
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS lod_chunks_spatial USING rtree(
             id, minX, maxX, minY, maxY, +worldspace_id, +tier, +anchor_x, +anchor_y
         );
         CREATE TABLE IF NOT EXISTS lod_build (
             id INTEGER PRIMARY KEY CHECK (id = 1), build_identity TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS waters (
             id INTEGER PRIMARY KEY, editor_id TEXT, opacity INTEGER, flags INTEGER NOT NULL,
             shallow_color INTEGER, deep_color INTEGER, reflection_color INTEGER,
             fresnel REAL, reflectivity REAL,
             flow_normal_path TEXT, data BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS texture_sets (
             id INTEGER PRIMARY KEY, editor_id TEXT, diffuse_path TEXT, normal_path TEXT,
             glow_path TEXT, height_path TEXT, environment_path TEXT, mask_path TEXT,
             specular_path TEXT, detail_path TEXT
         );
         CREATE TABLE IF NOT EXISTS landscape_textures (
             id INTEGER PRIMARY KEY, editor_id TEXT, texture_set_id INTEGER,
             material_type INTEGER, friction REAL, restitution REAL
         );
         CREATE TABLE IF NOT EXISTS grass_types (
             id INTEGER PRIMARY KEY, editor_id TEXT, model_path TEXT,
             density INTEGER, min_slope INTEGER, max_slope INTEGER,
             units_from_water INTEGER, water_comparison INTEGER,
             position_range REAL, height_range REAL, color_range REAL,
             wave_period REAL, flags INTEGER, load_order INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS landscape_texture_grasses (
             ltex_id INTEGER NOT NULL, gras_id INTEGER NOT NULL,
             ordinal INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (ltex_id, gras_id)
         );
         CREATE TABLE IF NOT EXISTS scripts (
             form_id INTEGER NOT NULL, script_name TEXT NOT NULL,
             vmad BLOB NOT NULL, properties_json TEXT NOT NULL,
             PRIMARY KEY (form_id, script_name)
         );
         CREATE TABLE IF NOT EXISTS formid_map (
             form_id INTEGER PRIMARY KEY, plugin_name TEXT NOT NULL, internal_id INTEGER NOT NULL, record_type TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS conversion_cache (
             plugin_path TEXT PRIMARY KEY, file_hash BLOB NOT NULL, last_converted INTEGER NOT NULL
         );"#
    )?;
    // Optional interior room graph (db-schema.md section 15); an older reused database gains it
    // here, and an older-shaped `room_portals` (no `pair_index`) is migrated in place.
    crate::esm::rooms::migrate_room_portals(conn)?;
    conn.execute_batch(crate::esm::rooms::ROOM_TABLES_SQL)?;
    crate::esm::rooms::migrate_room_metadata(conn)?;
    // Typed race animation identity (db-schema.md, actor models). Archive-only snapshots keep
    // the same schema and defer its rows like the other runtime views.
    conn.execute_batch(crate::animation::authoring::RACE_ANIMATION_SOURCES_SQL)?;
    // Optional projection omission counters (db-schema.md section 18, #297).
    conn.execute_batch(crate::esm::projection_omissions::PROJECTION_OMISSION_TABLES_SQL)?;
    // `CREATE TABLE IF NOT EXISTS` leaves a reused older table as it was; give it the optional
    // display-name columns (#269), so a subset export can still insert into it.
    for table in ["worldspaces", "cells", "statics"] {
        add_missing_columns(conn, table, &[("full_name", "TEXT")])?;
    }

    // Optional environment, weather and image-space tables (db-schema.md, #269 gaps 1 and 2).
    conn.execute_batch(&crate::esm::environment::tables_sql())?;

    // Optional reference link and detail tables (db-schema.md, #269 gap 3).
    conn.execute_batch(crate::esm::reference_links::REFERENCE_LINK_TABLES_SQL)?;
    super::object_materials::create_tables(conn)?;
    // `CREATE TABLE IF NOT EXISTS` leaves a reused older table as it was; give it the columns
    // later exports added, so a subset export can still insert into it.
    add_missing_columns(conn, "lights", LIGHT_DETAIL_COLUMNS)?;
    add_missing_columns(conn, "lights", &[("fade_status", "INTEGER")])?;

    // Optional load-door table (db-schema.md section 17), added the same way.
    super::doors::ensure_table(conn)?;
    // `CREATE TABLE IF NOT EXISTS` leaves a reused older table as it was; give it the columns
    // later exports added, so a complete export can still insert into it.
    add_missing_columns(
        conn,
        "references",
        &[("emittance_id", "INTEGER"), ("radius_status", "INTEGER")],
    )?;
    // Mark old rows for read-only recovery from the winning raw LTEX payload.
    // Subset exports preserve these lists; included LTEX records replace their ordinals.
    add_missing_columns(
        conn,
        "landscape_texture_grasses",
        &[("ordinal", "INTEGER NOT NULL DEFAULT -1")],
    )?;
    add_missing_columns(conn, "actor_models", ACTOR_SKIN_TONE_COLUMNS)?;
    add_missing_columns(conn, "actor_models", ACTOR_WEIGHT_COLUMNS)?;
    add_missing_columns(conn, "actor_model_parts", ACTOR_PART_SKIN_COLUMNS)?;
    conn.prepare_cached(
        "INSERT INTO schema_info(version) SELECT ?1 WHERE NOT EXISTS (SELECT 1 FROM schema_info)",
    )?
    .execute([shared::WORLD_DATABASE_SCHEMA_VERSION])?;
    Ok(())
}

/// Adds each `(name, declaration)` column `table` lacks.
pub(super) fn add_missing_columns(
    conn: &Connection,
    table: &str,
    columns: &[(&str, &str)],
) -> Result<()> {
    for (name, declaration) in columns {
        let present: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
            [table, name],
            |row| row.get(0),
        )?;
        if !present {
            conn.execute(
                &format!("ALTER TABLE \"{table}\" ADD COLUMN {name} {declaration}"),
                [],
            )?;
        }
    }
    Ok(())
}

/// The `actor_models` columns added after the first catalog export: `NPC_/QNAM`.
const ACTOR_SKIN_TONE_COLUMNS: &[(&str, &str)] = &[
    ("skin_tone_r", "REAL"),
    ("skin_tone_g", "REAL"),
    ("skin_tone_b", "REAL"),
];

/// The `actor_models` column added after the skin tone columns: `NPC_/NAM7`.
const ACTOR_WEIGHT_COLUMNS: &[(&str, &str)] = &[("weight", "REAL")];

/// The `actor_model_parts` columns added after the first catalog export: the armature's race
/// and its skin texture set.
const ACTOR_PART_SKIN_COLUMNS: &[(&str, &str)] = &[
    ("armature_race_id", "INTEGER"),
    ("skin_texture_set_id", "INTEGER"),
];

/// The `lights` columns #269 added after the first light export.
const LIGHT_DETAIL_COLUMNS: &[(&str, &str)] = &[
    ("time", "INTEGER"),
    ("field_of_view", "REAL"),
    ("near_clip", "REAL"),
    ("flicker_period", "REAL"),
    ("flicker_intensity_amplitude", "REAL"),
    ("flicker_movement_amplitude", "REAL"),
    ("value", "INTEGER"),
    ("weight", "REAL"),
    ("sound_id", "INTEGER"),
    ("lens_id", "INTEGER"),
];

type CellMetadata = (Option<i32>, Option<i32>, Option<u32>);

/// Light inputs the in-house decoder rejected as malformed. A rejected field is in neither the
/// canonical subrecords nor the runtime projection, so without this record the status columns
/// would read "absent" (0) where the legacy reader, which sees the raw bytes, writes "malformed" (2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RejectedLightInputs {
    /// `LIGH` `FNAM` fade was rejected.
    pub fnam: bool,
    /// Reference `XRDS` radius offset was rejected.
    pub xrds: bool,
}

/// Rejected light inputs by form ID, for the in-house reader.
pub(crate) type RejectedLightFields = HashMap<u32, RejectedLightInputs>;

/// Select runtime projections and script storage independently of snapshot ownership.
struct ExportOptions<'a> {
    /// Emit runtime views as well as the canonical record archive.
    runtime_views: bool,
    /// Replace in-house script snapshots using their normalized scalar layout.
    scalar_scripts: bool,
    /// Malformed light fields the decoder rejected; `None` for the legacy reader.
    rejected_light_fields: Option<&'a RejectedLightFields>,
    /// Source remapping preserves authored nulls; typed canonical nulls need source verification.
    trusted_portal_nulls: bool,
    deleted_ids: Option<&'a HashSet<u32>>,
    room_link_sources: Option<RoomLinkSources<'a>>,
    subset_sources: Option<(&'a HashMap<u32, DecodedRecord>, &'a LoadOrder)>,
    damaged_placement_cells: Option<&'a HashSet<u32>>,
}

/// Export selected records without changing existing provenance. Grass records
/// outside this subset are preserved; movement projections are rebuilt from it.
/// Synthetic records without a source load order have no formid_map entry.
/// Placed deletions use the deleted header flag and a resolved FormID.
pub fn export_to_db(conn: &Connection, master: &HashMap<u32, RawRecord>) -> Result<()> {
    export_records(
        conn,
        master,
        None,
        None,
        None,
        None,
        ExportOptions {
            runtime_views: true,
            scalar_scripts: false,
            rejected_light_fields: None,
            trusted_portal_nulls: true,
            deleted_ids: None,
            room_link_sources: None,
            subset_sources: None,
            damaged_placement_cells: None,
        },
    )
}

/// Apply decoded, resolved Skyrim placements and header-only deletions to an existing pack.
/// Use an append-only load order with the pack's existing slots intact. The physical source
/// fields provide XORD slot provenance. The original conversion's source archive is preserved;
/// new FormIDs receive their stable owning-plugin identity.
pub fn export_placed_subset_with_sources(
    conn: &Connection,
    records: &HashMap<u32, DecodedRecord>,
    deleted: &[u32],
    order: &LoadOrder,
) -> Result<()> {
    let mut master = HashMap::new();
    for (&id, record) in records {
        let supplier = record.load_order as usize;
        if id == 0
            || id != record.form_id
            || record.record_type != *b"REFR"
            || supplier >= order.names.len()
            || supplier >= order.metadata.len()
            || source_identity::header_runtime_id(b"REFR", record.source_form_id, supplier, order)
                != Some(id)
        {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "expected a resolved REFR for {id:08X}"
            )));
        }
        master.insert(id, record.to_raw_record());
    }
    for &id in deleted {
        if order.identity(id).is_err() {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "unresolved placed deletion {id:08X}"
            )));
        }
        if master
            .insert(
                id,
                RawRecord {
                    form_id: id,
                    record_type: *b"REFR",
                    flags: 0x20,
                    subrecords: Vec::new(),
                    cell_form_id: None,
                    worldspace_form_id: None,
                    load_order: 0,
                },
            )
            .is_some()
        {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "duplicate placed change for {id:08X}"
            )));
        }
    }
    export_records(
        conn,
        &master,
        None,
        None,
        None,
        None,
        ExportOptions {
            runtime_views: true,
            scalar_scripts: false,
            rejected_light_fields: None,
            trusted_portal_nulls: false,
            deleted_ids: None,
            room_link_sources: None,
            subset_sources: Some((records, order)),
            damaged_placement_cells: None,
        },
    )
}

/// Replace the complete canonical/runtime snapshot for an effective load order.
/// Plugin metadata remains caller-managed; subset exports retain unrelated rows.
/// Record stable owning-plugin/local-ID pairs separately from the winning
/// override priority in records.load_order.
pub fn export_to_db_with_load_order(
    conn: &Connection,
    master: &HashMap<u32, RawRecord>,
    order: &LoadOrder,
) -> Result<()> {
    export_records(
        conn,
        master,
        Some(order),
        None,
        None,
        None,
        ExportOptions {
            runtime_views: true,
            scalar_scripts: false,
            rejected_light_fields: None,
            trusted_portal_nulls: true,
            deleted_ids: None,
            room_link_sources: None,
            subset_sources: None,
            damaged_placement_cells: None,
        },
    )
}

/// Replace plugin metadata and the complete legacy snapshot in one transaction.
/// `unresolved_fields` are the localized fields the legacy text pass left without text; like the
/// in-house reader's, they stay in `records.data` and are left out of the runtime projections.
pub(super) fn export_plugins_to_db(
    conn: &Connection,
    merged: &super::LegacyPluginMerge,
    order: &LoadOrder,
    plugin_paths: &[PathBuf],
    unresolved_fields: &HashMap<u32, BTreeSet<usize>>,
) -> Result<()> {
    export_records(
        conn,
        &merged.conversion.records,
        Some(order),
        Some(plugin_paths),
        Some(unresolved_fields),
        None,
        ExportOptions {
            runtime_views: true,
            scalar_scripts: false,
            rejected_light_fields: None,
            trusted_portal_nulls: true,
            deleted_ids: Some(&merged.deleted_ids),
            room_link_sources: Some(RoomLinkSources::Legacy(&merged.room_link_layouts)),
            subset_sources: None,
            damaged_placement_cells: Some(&merged.damaged_cells),
        },
    )
}

/// Export canonical decoded fields with Skyrim's texture slots and byte-wide XESP flags.
/// `rejected_light_fields` records light fields the decoder rejected as malformed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn export_inhouse_to_db(
    conn: &Connection,
    master: &HashMap<u32, RawRecord>,
    order: &LoadOrder,
    plugin_paths: &[PathBuf],
    unresolved_fields: &HashMap<u32, BTreeSet<usize>>,
    terrain_offsets: &HashMap<u32, u64>,
    rejected_light_fields: Option<&RejectedLightFields>,
    decoded: &ReadResult,
) -> Result<()> {
    export_records(
        conn,
        master,
        Some(order),
        Some(plugin_paths),
        Some(unresolved_fields),
        Some(terrain_offsets),
        ExportOptions {
            runtime_views: true,
            scalar_scripts: true,
            rejected_light_fields,
            trusted_portal_nulls: false,
            deleted_ids: Some(&decoded.deleted_ids),
            room_link_sources: Some(RoomLinkSources::Typed(decoded)),
            subset_sources: None,
            damaged_placement_cells: None,
        },
    )
}

/// Fresh diagnostic bundles only: archive canonical records and identities without runtime views.
/// The shared loop writes identical records/formid_map SQL before its explicit deferral branch.
pub(crate) fn export_inhouse_record_archive_to_db(
    conn: &Connection,
    master: &HashMap<u32, RawRecord>,
    order: &LoadOrder,
    plugin_paths: &[PathBuf],
    deleted_ids: &HashSet<u32>,
) -> Result<()> {
    export_records(
        conn,
        master,
        Some(order),
        Some(plugin_paths),
        None,
        None,
        ExportOptions {
            runtime_views: false,
            scalar_scripts: true,
            rejected_light_fields: None,
            trusted_portal_nulls: false,
            deleted_ids: Some(deleted_ids),
            room_link_sources: None,
            subset_sources: None,
            damaged_placement_cells: None,
        },
    )
}

/// Recreate reader-owned outputs while preserving caller tables and asset-cache proofs.
/// DDL participates in the export transaction, so an export error restores the old snapshot.
fn replace_complete_snapshot(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"DROP TABLE IF EXISTS record_diagnostics;
        DROP TABLE IF EXISTS inhouse_plugin_masters;
        DROP TABLE IF EXISTS record_overrides;
        DROP TABLE IF EXISTS room_placement_edits;
        DROP TABLE IF EXISTS room_placement_layers;
        DROP TABLE IF EXISTS subset_nonplacement_deletions;
        DROP TABLE IF EXISTS inhouse_schema_addons;
        DROP TABLE IF EXISTS inhouse_unresolved_terrain_textures;
        DROP TABLE IF EXISTS inhouse_terrain_layers;
        DROP TABLE IF EXISTS inhouse_terrain_source_order;
        DROP TABLE IF EXISTS inhouse_localized_fields;
        DROP TABLE IF EXISTS inhouse_source_records;
        DROP TABLE IF EXISTS script_property_values;
        DROP TABLE IF EXISTS script_properties;
        DROP TABLE IF EXISTS scripts;
        DROP TABLE IF EXISTS landscape_texture_grasses;
        DROP TABLE IF EXISTS grass_types;
        DROP TABLE IF EXISTS landscape_textures;
        DROP TABLE IF EXISTS texture_sets;
        DROP TABLE IF EXISTS waters;
        DROP TABLE IF EXISTS movement_game_settings;
        DROP TABLE IF EXISTS race_movement_links;
        DROP TABLE IF EXISTS default_objects;
        DROP TABLE IF EXISTS race_animation_sources;
        DROP TABLE IF EXISTS movement_types;
        DROP TABLE IF EXISTS npcs;
        DROP TABLE IF EXISTS actor_model_parts;
        DROP TABLE IF EXISTS actor_models;
        DROP TABLE IF EXISTS lights;
        DROP TABLE IF EXISTS statics;
        DROP TABLE IF EXISTS land;
        DROP TABLE IF EXISTS exterior_spatial;
        DROP TABLE IF EXISTS "references";
        DROP TABLE IF EXISTS cells;
        DROP TABLE IF EXISTS worldspaces;
        DROP TABLE IF EXISTS records;
        DROP TABLE IF EXISTS formid_map;
        DROP TABLE IF EXISTS subset_placement_deletions;
        DROP TABLE IF EXISTS reference_link_subset_sources;
        DROP TABLE IF EXISTS reference_authored_targets;
        DROP TABLE IF EXISTS reference_authored_changes;
        DROP TABLE IF EXISTS lod_chunks_spatial;
        DROP TABLE IF EXISTS lod_chunks;
        DROP TABLE IF EXISTS lod_build;
        DROP TABLE IF EXISTS lod;"#,
    )?;
    for table in crate::esm::environment::ENVIRONMENT_TABLES
        .iter()
        .chain(crate::esm::reference_links::REFERENCE_LINK_TABLES.iter())
        .chain(std::iter::once(&super::object_materials::TABLE))
    {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))?;
    }
    create_tables(tx)?;
    super::reference_links::create_authored_index(tx)?;
    // Every complete snapshot owns these derived rows, including archive-only exports
    // that deliberately defer all runtime projections.
    crate::esm::rooms::clear(tx)?;
    crate::esm::projection_omissions::clear(tx)?;
    super::doors::clear(tx)?;
    tx.execute("DELETE FROM schema_info", [])?;
    tx.execute(
        "INSERT INTO schema_info(version) VALUES (?1)",
        [shared::WORLD_DATABASE_SCHEMA_VERSION],
    )?;
    Ok(())
}

/// Keep unresolved text occurrences in the archive while nullable projections see no text.
/// A genuinely resolved empty string is not filtered out.
fn runtime_projection(
    record: &RawRecord,
    unresolved_fields: Option<&HashMap<u32, BTreeSet<usize>>>,
) -> Option<RawRecord> {
    let unresolved = unresolved_fields?.get(&record.form_id)?;
    Some(RawRecord {
        form_id: record.form_id,
        record_type: record.record_type,
        flags: record.flags,
        subrecords: record
            .subrecords
            .iter()
            .enumerate()
            .filter(|(index, _)| !unresolved.contains(index))
            .map(|(_, field)| field.clone())
            .collect(),
        cell_form_id: record.cell_form_id,
        worldspace_form_id: record.worldspace_form_id,
        load_order: record.load_order,
    })
}

/// Validate movement projections before either frontend replaces a usable predecessor.
pub(crate) fn validate_inhouse_record(record: &RawRecord) -> Result<()> {
    let view = SubrecordView::new(&record.subrecords);
    match &record.record_type {
        b"MOVT" => {
            decode_movement_type(&view, record.form_id)?;
        }
        b"GMST"
            if view.get_string(b"EDID").is_some_and(|name| {
                ["fMoveCharWalkBase", "fJumpHeightMin"]
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&name))
            }) =>
        {
            decode_movement_setting(&view, record.form_id)?;
        }
        b"RACE" => {
            optional_race_movement_link(&view, record.form_id, b"WKMV")?;
            optional_race_movement_link(&view, record.form_id, b"RNMV")?;
        }
        _ => {}
    }
    Ok(())
}

/// Writes every merged record; with a load order, also records each record's
/// owning plugin and plugin-local ID in `formid_map`.
fn export_records(
    conn: &Connection,
    master: &HashMap<u32, RawRecord>,
    order: Option<&LoadOrder>,
    plugin_paths: Option<&[PathBuf]>,
    unresolved_fields: Option<&HashMap<u32, BTreeSet<usize>>>,
    terrain_offsets: Option<&HashMap<u32, u64>>,
    options: ExportOptions<'_>,
) -> Result<()> {
    let ExportOptions {
        runtime_views,
        scalar_scripts,
        rejected_light_fields,
        trusted_portal_nulls,
        deleted_ids,
        room_link_sources,
        subset_sources,
        damaged_placement_cells,
    } = options;
    conn.set_prepared_statement_cache_capacity(64);
    let tx = conn.unchecked_transaction()?;
    // Only a full load-order export writes these; a subset export keeps the stored ones.
    let mut omissions = ProjectionOmissions::default();
    if order.is_some() {
        replace_complete_snapshot(&tx)?;
        if let Some(paths) = plugin_paths {
            tx.execute("DELETE FROM plugins", [])?;
            super::insert_plugins(&tx, paths).map_err(|error| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                    error.to_string(),
                )))
            })?;
        }
    } else {
        // Older packs acquire this only when edited. Mapped deletions retain formid_map instead.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS subset_placement_deletions (form_id INTEGER PRIMARY KEY)",
        )?;
        super::reference_links::ensure_subset_sources(&tx)?;
    }
    tx.execute_batch(super::reference_links::SUBSET_NONPLACEMENT_DELETIONS_SQL)?;
    if let Some(ids) = deleted_ids {
        let mut insert =
            tx.prepare_cached("INSERT INTO subset_nonplacement_deletions(form_id) VALUES (?1)")?;
        for id in ids {
            insert.execute([id])?;
        }
    }
    if scalar_scripts {
        // In-house exports own the complete script snapshot; legacy partial APIs retain theirs.
        super::inhouse_script_properties::replace_snapshot_tables(&tx)?;
    }
    // Partial public exports retain unrelated rows and follow the existing database's format.
    let scalar_script_storage = scalar_scripts || tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='script_properties')",
        [], |row| row.get::<_, bool>(0),
    )?;
    if runtime_views && (order.is_some() || subset_sources.is_none()) {
        tx.prepare_cached("DELETE FROM movement_types")?
            .execute([])?;
        tx.prepare_cached("DELETE FROM movement_game_settings")?
            .execute([])?;
        tx.prepare_cached("DELETE FROM race_movement_links")?
            .execute([])?;
    }
    let mut cells: HashMap<u32, CellMetadata> = HashMap::new();
    let unordered_offsets = HashMap::new();
    let terrain_winners = if runtime_views {
        crate::esm::cell_cache::land_winners_by_cell(
            master,
            terrain_offsets.unwrap_or(&unordered_offsets),
        )
    } else {
        HashMap::new()
    };

    if runtime_views {
        for (&form_id, record) in master
            .iter()
            .filter(|(_, record)| &record.record_type == b"CELL")
        {
            let projection = runtime_projection(record, unresolved_fields);
            let projected = projection.as_ref().unwrap_or(record);
            let metadata = project_cell(
                &tx,
                &crate::esm::environment::Links { master },
                form_id,
                record,
                &projected.subrecords,
            )?;
            cells.insert(form_id, metadata);
        }
    }
    let interior_cells: std::collections::HashSet<u32> = cells
        .iter()
        .filter(|(_, (grid_x, grid_y, worldspace))| {
            grid_x.is_none() && grid_y.is_none() && worldspace.is_none()
        })
        .map(|(&form_id, _)| form_id)
        .collect();

    let only_occluders = if runtime_views && order.is_none() {
        crate::esm::rooms::update_subset_occluders(&tx, master, subset_sources, &mut omissions)?
            .only_occluders
    } else {
        HashSet::new()
    };
    let mut ordered: Vec<_> = master.iter().collect();
    ordered.sort_unstable_by_key(|(form_id, _)| **form_id);
    // Keep the hot statement checked out. Repeated cache checkout/return recreates
    // its owned SQL key and cache entry for every record. Prepare lazily for an
    // empty legacy subset, and clear each row's bindings as before.
    let mut records_insert = if ordered.is_empty() {
        None
    } else {
        let statement = tx.prepare_cached(RECORDS_INSERT_SQL)?;
        check_record_insert(&statement)?;
        Some(statement)
    };
    let link_context = super::reference_links::Context {
        master,
        order,
        subset: order.is_none(),
        deleted_ids,
    };
    let mut subset_link_sources = Vec::new();
    let mut unavailable_link_targets = Vec::new();
    let mut available_link_targets = HashSet::new();
    let mut edited_references = HashSet::new();
    for (&form_id, record) in ordered {
        let type_str = from_utf8(&record.record_type).unwrap_or("UNKN");
        let subset_placement = order.is_none() && super::is_placed_record_type(&record.record_type);
        if subset_placement && (subset_sources.is_some() || record.is_deleted()) {
            let type_mismatch: bool = tx
                .prepare_cached(
                    "SELECT EXISTS(
                         SELECT 1 FROM records WHERE form_id=?1 AND record_type<>?2
                         UNION ALL
                         SELECT 1 FROM formid_map WHERE form_id=?1 AND record_type<>?2
                     )",
                )?
                .query_row(params![form_id, type_str], |row| row.get(0))?;
            if type_mismatch {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "placed edit {form_id:08X} does not match its existing record type"
                )));
            }
        }
        if let Some(identity_order) = order.or(subset_sources.map(|(_, order)| order)) {
            let identity = identity_order.identity(form_id).map_err(|error| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("record {form_id:08X}: {error}"),
                )))
            })?;
            if order.is_some() {
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO formid_map(form_id, plugin_name, internal_id, record_type) VALUES (?1, ?2, ?3, ?4)",
                )?.execute(params![form_id, identity.plugin, identity.local_id, type_str])?;
            } else {
                let written = tx.prepare_cached(
                    "INSERT INTO formid_map(form_id, plugin_name, internal_id, record_type) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(form_id) DO UPDATE SET record_type=excluded.record_type
                     WHERE plugin_name=excluded.plugin_name AND internal_id=excluded.internal_id AND record_type=excluded.record_type",
                )?.execute(params![form_id, identity.plugin, identity.local_id, type_str])?;
                if written == 0 {
                    return Err(rusqlite::Error::InvalidParameterName(format!(
                        "subset load order changes the existing identity of {form_id:08X}"
                    )));
                }
            }
        }

        if subset_placement {
            if type_str == "REFR" {
                edited_references.insert(form_id);
            }
            if !only_occluders.contains(&form_id) {
                // A regular placement can change room membership that a subset cannot rebuild.
                tx.prepare_cached(
                    "INSERT INTO room_graph_cells(cell_id,complete)
                     SELECT cell_id,0 FROM [references] WHERE id=?1 AND cell_id<>0
                     UNION SELECT ?2,0 WHERE ?2 IS NOT NULL AND ?2<>0
                     ON CONFLICT(cell_id) DO UPDATE SET complete=0",
                )?
                .execute(params![form_id, record.cell_form_id])?;
            }
            tx.prepare_cached("DELETE FROM reference_rooms WHERE reference_id=?1")?
                .execute([form_id])?;
            if record.is_deleted() {
                delete_reference_scripts(&tx, form_id, scalar_script_storage)?;
                clear_reference_placement(&tx, form_id)?;
                super::reference_links::forget_subset_source(&tx, form_id)?;
                tx.prepare_cached("DELETE FROM records WHERE form_id=?1")?
                    .execute([form_id])?;
                if (1..0x800).contains(&form_id) {
                    tx.prepare_cached(
                        "INSERT OR IGNORE INTO subset_placement_deletions(form_id)
                         SELECT ?1 WHERE NOT EXISTS (SELECT 1 FROM formid_map WHERE form_id=?1)",
                    )?
                    .execute([form_id])?;
                }
                unavailable_link_targets.push(form_id);
                continue;
            }
        }
        let link_target =
            form_id != 0 && super::reference_links::accepts_link_target(&record.record_type);
        let target_changed = order.is_none()
            && runtime_views
            && form_id != 0
            && !record.is_deleted()
            && super::reference_links::target_kind_changed(&tx, form_id, &record.record_type)?;
        let blob = serialize_subrecords(&record.subrecords);
        let insert = records_insert.as_mut().expect("nonempty record loop");
        insert_canonical_record(
            insert,
            form_id,
            type_str,
            record.cell_form_id,
            record.worldspace_form_id,
            record.load_order,
            &blob,
        )?;
        if record.is_deleted() {
            if !super::is_placed_record_type(&record.record_type) {
                if order.is_none() && runtime_views {
                    unavailable_link_targets.push(form_id);
                }
                tx.prepare_cached(
                    "INSERT OR IGNORE INTO subset_nonplacement_deletions(form_id) VALUES (?1)",
                )?
                .execute([form_id])?;
            }
        } else if order.is_none() {
            tx.prepare_cached("DELETE FROM subset_nonplacement_deletions WHERE form_id=?1")?
                .execute([form_id])?;
            if target_changed && !super::is_placed_record_type(&record.record_type) {
                if link_target {
                    available_link_targets.insert(form_id);
                } else {
                    unavailable_link_targets.push(form_id);
                }
            }
        }

        if !runtime_views {
            // Canonical records/formid_map are complete; only materialized runtime views defer.
            continue;
        }
        let canonical_record = record;
        let projection = runtime_projection(record, unresolved_fields);
        let record = projection.as_ref().unwrap_or(record);
        let view = SubrecordView::new(&record.subrecords);
        if record.record_type == *b"STAT" {
            super::object_materials::insert(&tx, master, record, &view)?;
        }
        if order.is_none()
            && super::is_placed_record_type(&record.record_type)
            && !record.is_deleted()
        {
            delete_reference_scripts(&tx, form_id, scalar_script_storage)?;
        }
        let vmad_bytes = view.find(b"VMAD");
        if let Some(vmad_bytes) = vmad_bytes {
            let parsed = parse_vmad(vmad_bytes, &record.record_type);
            if let Ok((_, vmad)) = parsed {
                for script in vmad.scripts {
                    if scalar_script_storage {
                        super::inhouse_script_properties::write_script(
                            &tx, form_id, &script, vmad_bytes,
                        )?;
                        continue;
                    }
                    let properties =
                        serde_json::to_string(&script.properties).map_err(|error| {
                            rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                        })?;
                    tx.prepare_cached(
                    "INSERT OR REPLACE INTO scripts(form_id, script_name, vmad, properties_json) VALUES (?1, ?2, ?3, ?4)")?.execute(
                    params![form_id, script.name, vmad_bytes, properties],
                )?;
                }
            } else if super::is_placed_record_type(&record.record_type) && !record.is_deleted() {
                eprintln!(
                    "warning: {type_str} {form_id:08X}: malformed VMAD ({} bytes), script rows omitted",
                    vmad_bytes.len()
                );
            }
        }

        match type_str {
            "LGTM" | "WTHR" | "CLMT" | "REGN" | "IMGS" => {
                crate::esm::environment::insert_record(
                    &tx,
                    &crate::esm::environment::Links { master },
                    record,
                    &view,
                    &mut omissions,
                )?;
            }
            "GRAS" => {
                insert_grass(&tx, form_id, &view, record.load_order, &mut omissions)?;
            }
            "WRLD" => {
                let view = SubrecordView::new(&record.subrecords);
                let editor_id = view
                    .get_string(b"EDID")
                    .unwrap_or_else(|| format!("WRLD_{form_id:08X}"));
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO worldspaces(id, editor_id, parent_world, flags, full_name) VALUES (?1, ?2, ?3, ?4, ?5)")?.execute(
                    params![form_id, editor_id, view.get_form_id(b"WNAM"), record.flags, view.get_string(b"FULL")],
                )?;
                crate::esm::environment::insert_worldspace(
                    &tx,
                    &crate::esm::environment::Links { master },
                    form_id,
                    &view,
                )?;
            }
            _ if super::is_placed_record_type(&record.record_type) => {
                let cell_id = record.cell_form_id.unwrap_or(0);
                let mut cell = cells.get(&cell_id).copied();
                if subset_placement && cell.is_none() {
                    cell = tx
                        .prepare_cached(
                            "SELECT grid_x,grid_y,worldspace_id FROM cells WHERE id=?1",
                        )?
                        .query_row([cell_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                        .optional()?;
                }
                let inserted = insert_reference_with_canonical_data(
                    &tx,
                    form_id,
                    cell_id,
                    cell,
                    record.flags,
                    &record.subrecords,
                    &canonical_record.subrecords,
                    rejected_light_fields
                        .and_then(|rejected| rejected.get(&form_id))
                        .is_some_and(|rejected| rejected.xrds),
                )?;
                if !inserted {
                    if subset_placement {
                        unavailable_link_targets.push(form_id);
                        super::reference_links::forget_subset_source(&tx, form_id)?;
                    }
                    omissions.record(
                        OmissionCategory::ReferenceUnusableData,
                        form_id,
                        format!(
                            "{} DATA is {} in the runtime projection; expected at least 24 bytes; no placement or spatial row",
                            type_str,
                            view.find(b"DATA").map_or_else(|| "unavailable".to_owned(), |data| format!("{} bytes", data.len())),
                        ),
                    );
                    continue;
                }
                if order.is_none() {
                    if (1..0x800).contains(&form_id) {
                        tx.prepare_cached(
                            "DELETE FROM subset_placement_deletions WHERE form_id=?1",
                        )?
                        .execute([form_id])?;
                    }
                    subset_link_sources.push(canonical_record);
                    if !link_target {
                        unavailable_link_targets.push(form_id);
                    } else if target_changed {
                        available_link_targets.insert(form_id);
                    }
                } else {
                    let typed = match (&room_link_sources, order) {
                        (Some(RoomLinkSources::Typed(decoded)), Some(order)) => {
                            decoded.records.get(&form_id).map(|source| (source, order))
                        }
                        _ => None,
                    };
                    super::reference_links::insert(&tx, &link_context, record, typed)?;
                }
            }
            "LAND" => {
                let cell_id = record.cell_form_id.unwrap_or(form_id);
                if terrain_winners.get(&cell_id) != Some(&form_id) {
                    continue;
                }
                let (heightmap, vtex, vclr, normals) = extract_land_data(&record.subrecords);
                tx.prepare_cached("INSERT OR REPLACE INTO land(cell_id, heightmap, vtex, vclr, normals) VALUES (?1, ?2, ?3, ?4, ?5)")?.execute( params![cell_id, heightmap, vtex, vclr, normals])?;
            }
            "LIGH" => {
                let view = SubrecordView::new(&record.subrecords);
                // Every `LIGH` lights the space whether or not it has
                // geometry, so it gets a `lights` row regardless.
                insert_light(
                    &tx,
                    master,
                    form_id,
                    &view,
                    rejected_light_fields
                        .and_then(|rejected| rejected.get(&form_id))
                        .is_some_and(|rejected| rejected.fnam),
                    &mut omissions,
                )?;
                // Most `LIGH` records carry no model: they light the space with
                // nothing to draw. Storing one row per invisible light would put
                // a meshless entry in `statics` for every candle in the game, so
                // those are skipped; a light with geometry is stored like any
                // other base object.
                let model = view.get_string(b"MODL").filter(|path| !path.is_empty());
                write_static_model(
                    &tx,
                    form_id,
                    canonical_record,
                    &view,
                    model.as_deref(),
                    order.is_none(),
                )?;
            }
            // Every base record type with a world model. The runtime spawns
            // anything that resolves a model path here, so trees, flora,
            // containers, doors, activators, and placed inventory all render;
            // Records without MODL (triggers, markers) store NULL and are
            // skipped at spawn time. `LIGH` is handled above: the light goes
            // in `lights` and only a light with geometry lands in `statics`.
            // `TACT` (talking activators), `ADDN` (addon nodes), `APPA`
            // (apparatus) and `ARTO` (art objects) are placeable `REFR` bases
            // with a model too (`REFR.NAME` targets in xEdit's TES5 definitions).
            "STAT" | "MSTT" | "FURN" | "TREE" | "FLOR" | "CONT" | "DOOR" | "ACTI" | "TACT"
            | "WEAP" | "MISC" | "BOOK" | "AMMO" | "ALCH" | "INGR" | "SLGM" | "KEYM" | "SCRL"
            | "ARMO" | "APPA" | "ADDN" | "ARTO" | "PROJ" | "HAZD" => {
                let view = SubrecordView::new(&record.subrecords);
                // ARMO has no MODL path; its world models live in the
                // gendered world-model slots (male first, female fallback).
                // Both frontends use MOD4 for the female world model.
                let model = if type_str == "ARMO" {
                    view.get_string(b"MOD2")
                        .filter(|path| !path.is_empty())
                        .or_else(|| view.get_string(b"MOD4"))
                } else {
                    view.get_string(b"MODL")
                }
                .filter(|path| !path.is_empty());
                write_static_model(
                    &tx,
                    form_id,
                    canonical_record,
                    &view,
                    model.as_deref(),
                    order.is_none(),
                )?;
            }
            "NPC_" => {
                let view = SubrecordView::new(&record.subrecords);
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO npcs(id, editor_id, full_name, race_id, class_id, flags) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?.execute(
                    params![form_id, view.get_string(b"EDID"), view.get_string(b"FULL"), view.get_form_id(b"RNAM"), view.get_form_id(b"CNAM"), record.flags],
                )?;
            }
            "RACE" => {
                let walk = optional_race_movement_link(&view, form_id, b"WKMV")?;
                let run = optional_race_movement_link(&view, form_id, b"RNMV")?;
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO race_movement_links(id, walk_movt_id, run_movt_id, load_order) VALUES (?1, ?2, ?3, ?4)")?.execute(
                    params![form_id, walk, run, record.load_order],
                )?;
            }
            "MOVT" => {
                let (editor_id, speeds) = decode_movement_type(&view, form_id)?;
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO movement_types(id, editor_id, left_walk, left_run, right_walk, right_run, forward_walk, forward_run, back_walk, back_run, load_order) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)")?.execute(
                    params![form_id, editor_id, speeds[0], speeds[1], speeds[2], speeds[3], speeds[4], speeds[5], speeds[6], speeds[7], record.load_order],
                )?;
            }
            // The readers merge overrides key by key before export (see `merge_default_objects`).
            // A null link, or a record without usable pairs, adds no default: the engine then
            // reports the missing default instead of inventing a movement type. A listed key
            // with a null link removes its stored row. Only a complete snapshot clears the
            // table, so a partial export keeps the rows it does not list.
            "DOBJ" => {
                for (key, target) in default_object_pairs(&record.subrecords) {
                    let key = String::from_utf8_lossy(&key);
                    if target == 0 {
                        // The loader sets a listed slot to null, so a stored default goes too.
                        tx.prepare_cached("DELETE FROM default_objects WHERE key=?1")?
                            .execute(params![key])?;
                    } else {
                        tx.prepare_cached(
                            "INSERT OR REPLACE INTO default_objects(key, form_id) VALUES (?1, ?2)",
                        )?
                        .execute(params![key, target])?;
                    }
                }
            }
            "GMST" => {
                let editor_id = view.get_string(b"EDID");
                let movement_name = editor_id.as_deref().and_then(|name| {
                    ["fMoveCharWalkBase", "fJumpHeightMin"]
                        .into_iter()
                        .find(|known| known.eq_ignore_ascii_case(name))
                });
                if let Some(editor_id) = movement_name {
                    let value = decode_movement_setting(&view, form_id)?;
                    tx.prepare_cached(
                        "INSERT OR REPLACE INTO movement_game_settings(id, editor_id, value, load_order) VALUES (?1, ?2, ?3, ?4)")?.execute(
                        params![form_id, editor_id, value, record.load_order],
                    )?;
                }
            }
            "WATR" => {
                let view = SubrecordView::new(&record.subrecords);
                let dnam = view.find(b"DNAM").and_then(parse_water_dnam);
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO waters(id,editor_id,opacity,flags,shallow_color,deep_color,reflection_color,fresnel,reflectivity,flow_normal_path,data) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)")?.execute(
                    params![
                        form_id,
                        view.get_string(b"EDID"),
                        view.find(b"ANAM").and_then(|bytes| bytes.first()).copied(),
                        record.flags,
                        dnam.as_ref().map(|dnam| dnam.shallow_color),
                        dnam.as_ref().map(|dnam| dnam.deep_color),
                        dnam.as_ref().map(|dnam| dnam.reflection_color),
                        dnam.as_ref().map(|dnam| dnam.fresnel),
                        dnam.as_ref().map(|dnam| dnam.reflectivity),
                        water_flow_normal_path(&view),
                        blob,
                    ],
                )?;
            }
            "TXST" => {
                let view = SubrecordView::new(&record.subrecords);
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO texture_sets(id,editor_id,diffuse_path,normal_path,glow_path,height_path,environment_path,mask_path,specular_path,detail_path) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")?.execute(
                    params![
                        form_id,
                        view.get_string(b"EDID"),
                        texture_set_slot_path(&view, form_id, b"TX00"),
                        texture_set_slot_path(&view, form_id, b"TX01"),
                        texture_set_slot_path(&view, form_id, b"TX03"),
                        texture_set_slot_path(&view, form_id, b"TX04"),
                        texture_set_slot_path(&view, form_id, b"TX05"),
                        texture_set_slot_path(&view, form_id, b"TX02"),
                        texture_set_slot_path(&view, form_id, b"TX07"),
                        texture_set_slot_path(&view, form_id, b"TX06"),
                    ],
                )?;
            }
            "LTEX" => {
                let view = SubrecordView::new(&record.subrecords);
                let havok = view.find(b"HNAM");
                let friction = havok
                    .and_then(|bytes| bytes.first())
                    .copied()
                    .map(f32::from);
                let restitution = havok.and_then(|bytes| bytes.get(1)).copied().map(f32::from);
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO landscape_textures(id,editor_id,texture_set_id,material_type,friction,restitution) VALUES (?1,?2,?3,?4,?5,?6)")?.execute(
                    params![form_id, view.get_string(b"EDID"), view.get_form_id(b"TNAM"), view.get_form_id(b"MNAM"), friction, restitution],
                )?;
                // An included LTEX always replaces its own list, including an
                // empty list, even when other records are outside this export.
                tx.prepare_cached("DELETE FROM landscape_texture_grasses WHERE ltex_id=?1")?
                    .execute([form_id])?;
                // Keep the first occurrence of each grass in the winning source list.
                for (ordinal, (_, data)) in record
                    .subrecords
                    .iter()
                    .filter(|(tag, _)| tag == b"GNAM")
                    .enumerate()
                {
                    if let Ok(bytes) = <[u8; 4]>::try_from(data.as_slice()) {
                        let grass_id = u32::from_le_bytes(bytes);
                        if grass_id != 0 {
                            tx.prepare_cached(
                                "INSERT OR IGNORE INTO landscape_texture_grasses(ltex_id, gras_id, ordinal) VALUES (?1, ?2, ?3)")?.execute(
                                params![form_id, grass_id, ordinal as i64],
                            )?;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    drop(records_insert);
    // A later edit can remove a placement while retaining its canonical record.
    for source in subset_link_sources {
        let projection = runtime_projection(source, unresolved_fields);
        let projected = projection.as_ref().unwrap_or(source);
        let typed = subset_sources
            .and_then(|(records, order)| Some((records.get(&source.form_id)?, order)));
        super::reference_links::insert(&tx, &link_context, projected, typed)?;
    }
    if order.is_none() {
        super::reference_links::refresh_incoming(
            &tx,
            &unavailable_link_targets,
            &available_link_targets,
            &master.keys().copied().collect::<Vec<_>>(),
        )?;
    }
    // Derived snapshots like the actor model catalog and the grass tables: only a full load-order
    // export owns them, so the movement annotator's subset export leaves them alone, and
    // diagnostic archives defer them. The emittance pass holds every reference and the records
    // their XEMI names, and may run on a database older than `emittance_id`.
    if runtime_views && let Some(order) = order {
        insert_actor_models_and_race_authoring(&tx, master, &mut omissions)?;
        crate::esm::rooms::export_room_graph_with_sources(
            &tx,
            master,
            &interior_cells,
            room_link_sources,
            Some(order),
            damaged_placement_cells,
            &mut omissions,
        )?;
        if !trusted_portal_nulls {
            // Canonical nulls need their source provenance. Keep legacy-safe semantics until
            // the full in-house source transaction verifies and publishes them.
            tx.execute("UPDATE room_portals SET unbounded_a=0,unbounded_b=0", [])?;
            tx.execute(
                "DELETE FROM room_graph_stats WHERE name IN ('visibility_version','occlusion_version')",
                [],
            )?;
        }
        insert_emittance_sources(&tx, master, &mut omissions)?;
        omissions.write(&tx)?;
        omissions.print_summary();
        // Read back from the canonical `records` and `references` rows just written, so the
        // legacy and in-house readers share one projection.
        super::doors::project_door_links(&tx)?;
    } else {
        super::doors::refresh_placement_links(&tx, &edited_references)?;
    }
    tx.commit()
}

fn write_static_model(
    tx: &Transaction<'_>,
    form_id: u32,
    record: &RawRecord,
    view: &SubrecordView<'_>,
    model: Option<&str>,
    subset: bool,
) -> Result<()> {
    use rusqlite::OptionalExtension;
    let fields = params![
        form_id,
        view.get_string(b"EDID"),
        model,
        record.flags,
        view.get_string(b"FULL")
    ];
    if subset
        && let Some(previous) = tx
            .prepare_cached("SELECT model_path FROM statics WHERE id=?1")?
            .query_row([form_id], |row| row.get::<_, Option<String>>(0))
            .optional()?
    {
        let previous = previous.as_deref();
        let unchanged = !record.is_deleted()
            && (previous == model
                || previous.zip(model).is_some_and(|(old, new)| {
                    let key = shared::world_assets::streamed_model_path;
                    key(old).is_some_and(|old_key| Some(old_key) == key(new))
                }));
        if unchanged && model.is_some() {
            // A different source spelling can identify the same converted GLB.
            // Keep its measured render bounds as well as its room evidence.
            tx.prepare_cached("UPDATE statics SET editor_id=?2, model_path=?3, flags=?4, full_name=?5 WHERE id=?1")?.execute(fields)?;
            return Ok(());
        }
        if !unchanged {
            if super::rooms::table_exists(tx, "room_model_bounds")? {
                tx.prepare_cached("DELETE FROM room_model_bounds WHERE base_form_id=?1")?
                    .execute([form_id])?;
            }
            tx.prepare_cached(
                "INSERT INTO room_graph_cells(cell_id,complete) \
             SELECT DISTINCT cell_id,0 FROM [references] WHERE base_form_id=?1 \
             ON CONFLICT(cell_id) DO UPDATE SET complete=0",
            )?
            .execute([form_id])?;
        }
    }
    if record.record_type == *b"LIGH" && model.is_none() {
        if subset {
            tx.prepare_cached("DELETE FROM statics WHERE id=?1")?
                .execute([form_id])?;
        }
    } else {
        tx.prepare_cached("INSERT OR REPLACE INTO statics(id, editor_id, model_path, flags, full_name) VALUES (?1, ?2, ?3, ?4, ?5)")?.execute(fields)?;
    }
    Ok(())
}

/// Publishes actor models and their shared RACE authoring projection in the record transaction.
///
/// The resolver needs the whole record map, so it runs once after the record loop and only when
/// `formid_map` carries complete ownership. Each `NPC_`'s own row and parts are replaced, which
/// keeps the freshly created tables consistent with `master`.
fn insert_actor_models_and_race_authoring(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    omissions: &mut ProjectionOmissions,
) -> Result<()> {
    let plugin_ids: HashMap<u32, StableId> = tx
        .prepare(
            "SELECT form_id, plugin_name, internal_id FROM formid_map WHERE record_type='NPC_'",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                StableId {
                    plugin: row.get(1)?,
                    local_id: row.get(2)?,
                },
            ))
        })?
        .collect::<Result<_>>()?;
    let (catalog, race_authoring) =
        crate::actor_models::resolve_actor_models_with_race_authoring(master, &plugin_ids);

    let mut delete_model = tx.prepare_cached("DELETE FROM actor_models WHERE id=?1")?;
    let mut delete_parts = tx.prepare_cached("DELETE FROM actor_model_parts WHERE npc_id=?1")?;
    let mut insert_model = tx.prepare_cached(
        "INSERT OR REPLACE INTO actor_models(id, race_id, sex, skin_armo_id, head_model, head_source, resolution, skin_tone_r, skin_tone_g, skin_tone_b, weight) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    let mut insert_part = tx.prepare_cached(
        "INSERT OR REPLACE INTO actor_model_parts(npc_id, slot, model_path, armature_race_id, skin_texture_set_id) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for row in &catalog.models {
        delete_model.execute([row.id])?;
        delete_parts.execute([row.id])?;
        insert_model.execute(params![
            row.id,
            row.race_id,
            row.sex,
            row.skin_armo_id,
            row.head_model,
            row.head_source,
            row.resolution,
            row.skin_tone.map(|tone| tone[0]),
            row.skin_tone.map(|tone| tone[1]),
            row.skin_tone.map(|tone| tone[2]),
            row.weight
        ])?;
    }
    for part in &catalog.parts {
        insert_part.execute(params![
            part.npc_id,
            part.slot.as_str(),
            part.model_path,
            part.armature_race_id,
            part.skin_texture_set_id
        ])?;
    }
    report_actor_model_diagnostics(&catalog.diagnostics);
    for (id, error) in catalog.race_authoring_errors {
        // One omission per unreadable RACE. Its canonical record and usable neighbors survive.
        omissions.record(
            OmissionCategory::RaceUnreadableAuthoring,
            id,
            format!(
                "RACE animation authoring is unreadable; no race_animation_sources row: {error}"
            ),
        );
    }
    crate::animation::authoring::replace_race_projection(tx, race_authoring)
}

/// Summarizes the catalog's diagnostics and names every unrecognized biped slot.
fn report_actor_model_diagnostics(diagnostics: &[crate::actor_models::ActorModelDiagnostic]) {
    use crate::actor_models::UnresolvedReason;
    if diagnostics.is_empty() {
        return;
    }
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for diagnostic in diagnostics {
        *counts.entry(diagnostic.reason.as_str()).or_default() += 1;
    }
    let summary: Vec<String> = counts
        .iter()
        .map(|(reason, count)| format!("{reason}={count}"))
        .collect();
    eprintln!(
        "actor models: {} diagnostic(s): {}",
        diagnostics.len(),
        summary.join(", ")
    );
    for diagnostic in diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.reason == UnresolvedReason::UnrecognizedSlot)
    {
        eprintln!(
            "actor model: NPC {:08X} has an armature with no stored biped slot",
            diagnostic.npc_id
        );
    }
}

/// Rebuilds `emittance`: one row per `XEMI` target in `references` that resolves to a colour.
///
/// A light lends its `DATA` colour at every hour; a region lends its likeliest weather's Effect
/// Lighting colour at each time of day (see [`crate::esm::emittance`]). A target that is missing
/// or does not resolve gets no row, so the runtime keeps the model's own emission for its
/// references. Rows from an earlier export into the same database are dropped first, so a target
/// that no longer resolves loses its old colour.
fn insert_emittance_sources(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    omissions: &mut ProjectionOmissions,
) -> Result<()> {
    tx.execute("DELETE FROM emittance", [])?;
    let targets: Vec<u32> = tx
        .prepare("SELECT DISTINCT emittance_id FROM \"references\" WHERE emittance_id IS NOT NULL")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_>>()?;
    for target in targets {
        let Some(record) = master.get(&target).filter(|record| !record.is_deleted()) else {
            omissions.record(
                OmissionCategory::EmittanceTargetAbsent,
                target,
                "not in the load order; no emittance row",
            );
            continue;
        };
        let view = SubrecordView::new(&record.subrecords);
        let (source_type, weather, colours) = match &record.record_type {
            b"LIGH" => {
                let Some(data) = view
                    .find(b"DATA")
                    .filter(|data| data.len() >= LIGHT_DATA_MIN)
                else {
                    omissions.record(
                        OmissionCategory::EmittanceLightUnusableData,
                        target,
                        "LIGH has no usable DATA; no emittance row",
                    );
                    continue;
                };
                ("LIGH", None, [[data[8], data[9], data[10]]; 4])
            }
            b"REGN" => {
                let Some(weather) = view.find(b"RDWT").and_then(emittance::region_weather) else {
                    omissions.record(
                        OmissionCategory::EmittanceRegionNoWeather,
                        target,
                        "REGN names no weather; no emittance row",
                    );
                    continue;
                };
                let Some(colours) = master
                    .get(&weather)
                    .filter(|record| record.record_type == *b"WTHR" && !record.is_deleted())
                    .and_then(|record| SubrecordView::new(&record.subrecords).find(b"NAM0"))
                    .and_then(emittance::weather_effect_lighting)
                else {
                    omissions.record(
                        OmissionCategory::EmittanceWeatherNoLighting,
                        target,
                        format!(
                            "REGN weather {weather:08X} has no Effect Lighting colours; no emittance row"
                        ),
                    );
                    continue;
                };
                ("REGN", Some(weather), colours)
            }
            other => {
                omissions.record(
                    OmissionCategory::EmittanceTargetWrongType,
                    target,
                    format!(
                        "is a {}, not a LIGH or REGN; no emittance row",
                        String::from_utf8_lossy(other)
                    ),
                );
                continue;
            }
        };
        let [sunrise, day, sunset, night] = colours;
        tx.execute(
            "INSERT OR REPLACE INTO emittance(id, source_type, weather_id, sunrise_r, sunrise_g, sunrise_b, day_r, day_g, day_b, sunset_r, sunset_g, sunset_b, night_r, night_g, night_b)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                target, source_type, weather,
                sunrise[0], sunrise[1], sunrise[2],
                day[0], day[1], day[2],
                sunset[0], sunset[1], sunset[2],
                night[0], night[1], night[2],
            ],
        )?;
    }
    Ok(())
}

/// Projects xEdit's TES5 GRAS/DATA layout without inventing missing fields.
/// The complete payload is 32 bytes; unknown padding stays in records.data.
/// Short DATA retains each complete scalar at its known offset; unavailable values are NULL.
/// Missing/short DATA is still a reported omission, and the model and identity survive.
/// See xEdit xedit-4.1.5f Core/wbDefinitionsTES5.pas:10199-10230.
fn insert_grass(
    tx: &Transaction<'_>,
    form_id: u32,
    view: &SubrecordView<'_>,
    load_order: u32,
    omissions: &mut ProjectionOmissions,
) -> Result<()> {
    let data = view.find(b"DATA");
    if !matches!(data, Some(data) if data.len() >= 32) {
        let extent = data.map_or_else(
            || "missing".to_owned(),
            |data| format!("{} bytes", data.len()),
        );
        omissions.record(
            OmissionCategory::GrassUnusableData,
            form_id,
            format!("GRAS DATA is {extent}; expected at least 32 bytes; unavailable scalar columns remain NULL"),
        );
    }
    let data = data.unwrap_or_default();
    let float = |offset| {
        data.get(offset..offset + 4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(f32::from_le_bytes)
            .filter(|value| value.is_finite())
    };
    let model = view
        .get_string(b"MODL")
        .and_then(|path| canonical_asset_path(&path, AssetKind::Mesh, "glb").ok());
    tx.prepare_cached(
        "INSERT OR REPLACE INTO grass_types(id, editor_id, model_path, density, min_slope, max_slope, units_from_water, water_comparison, position_range, height_range, color_range, wave_period, flags, load_order) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)")?.execute(
        params![
            form_id,
            view.get_string(b"EDID"),
            model,
            data.first().copied(),
            data.get(1).copied(),
            data.get(2).copied(),
            data.get(4..6).and_then(|bytes| <[u8; 2]>::try_from(bytes).ok()).map(u16::from_le_bytes),
            data.get(8..12).and_then(|bytes| <[u8; 4]>::try_from(bytes).ok()).map(u32::from_le_bytes),
            float(12),
            float(16),
            float(20),
            float(24),
            data.get(28).copied(),
            load_order,
        ],
    )?;
    Ok(())
}

/// One TXST slot's path as authored, or nothing when no asset lookup could accept it. The later
/// texture passes treat a stored path they cannot canonicalize as fatal, so one bad slot would
/// otherwise stop the whole conversion. The archived record keeps the original bytes.
fn texture_set_slot_path(view: &SubrecordView<'_>, form_id: u32, slot: &[u8; 4]) -> Option<String> {
    let path = view.get_string(slot)?;
    if path.is_empty() {
        return Some(path);
    }
    match canonical_asset_path(&path, AssetKind::Texture, "dds") {
        Ok(_) => Some(path),
        Err(error) => {
            eprintln!(
                "warning: TXST {form_id:08X} {}: dropped invalid texture path {path:?}: {error}",
                String::from_utf8_lossy(slot)
            );
            None
        }
    }
}

fn water_flow_normal_path(view: &SubrecordView<'_>) -> Option<String> {
    view.get_string(b"NAM5").and_then(|path| {
        canonical_asset_path(&path, AssetKind::Texture, "dds")
            .ok()
            .map(|canonical| canonical.trim_start_matches("textures/").to_owned())
    })
}

/// The WATR colours and reflectivity factors decoded from `DNAM`. Field order and offsets are
/// xEdit's `Water.psc` layout (`docs/research/water.md` section 1.2), confirmed against
/// `Skyrim.esm`/`Update.esm`. `DNAM` is 228 bytes (30 records) or 232 bytes (8 SSE records, an
/// extra trailing Flowmap Scale float); none of the fields read here move between the two.
struct WaterDnam {
    reflectivity: f32,
    fresnel: f32,
    shallow_color: u32,
    deep_color: u32,
    reflection_color: u32,
}

/// Packs a WATR colour subrecord's leading RGB bytes (plus the trailing pad byte, ignored by
/// callers) into a `u32`, the same little-endian layout the `waters` table has always stored
/// colours in.
fn dnam_color(dnam: &[u8], offset: usize) -> Option<u32> {
    dnam.get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four-byte color")))
}

fn dnam_f32(dnam: &[u8], offset: usize) -> Option<f32> {
    dnam.get(offset..offset + 4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four-byte float")))
}

/// Decodes Reflectivity Amount, Fresnel Amount, and the three colours from a WATR `DNAM`
/// subrecord. Returns `None` if `dnam` is too short to hold every field (real records never are:
/// the shortest is 228 bytes, and the last field read here ends at byte 52).
fn parse_water_dnam(dnam: &[u8]) -> Option<WaterDnam> {
    Some(WaterDnam {
        reflectivity: dnam_f32(dnam, 20)?,
        fresnel: dnam_f32(dnam, 24)?,
        shallow_color: dnam_color(dnam, 40)?,
        deep_color: dnam_color(dnam, 44)?,
        reflection_color: dnam_color(dnam, 48)?,
    })
}

/// `LIGH` `DATA`, per UESP "Skyrim Mod:Mod File Format/LIGH": time (i32),
/// radius (u32, Creation units), colour (RGB + one unused byte), flags (u32),
/// falloff exponent (f32), then FOV, near clip, flicker period, flicker
/// intensity amplitude, flicker movement amplitude, value (u32) and weight
/// (f32). Every one of the 435 `LIGH` records in Skyrim.esm carries exactly
/// 48 bytes, which is the sum of those fields.
///
/// The engine reads only the first four - radius, colour, flags, falloff - so
/// a `DATA` holding at least those 20 bytes is accepted and its tail ignored;
/// one shorter than that is a record this code cannot read and is dropped with
/// a warning rather than guessed at. A shorter `DATA` is accepted because the
/// leading fields are the same ones Oblivion-era light data has.
const LIGHT_DATA_MIN: usize = 20;
/// The full `LIGH` `DATA` in Skyrim SE.
const LIGHT_DATA_FULL: usize = 48;

fn insert_light(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    form_id: u32,
    view: &SubrecordView<'_>,
    fnam_rejected: bool,
    omissions: &mut ProjectionOmissions,
) -> Result<()> {
    let Some(data) = view.find(b"DATA") else {
        omissions.record(
            OmissionCategory::LightMissingData,
            form_id,
            "LIGH has no DATA; no lights row",
        );
        return Ok(());
    };
    if data.len() < LIGHT_DATA_MIN {
        omissions.record(
            OmissionCategory::LightShortData,
            form_id,
            format!(
                "LIGH DATA is {} bytes, expected at least {LIGHT_DATA_MIN}; no lights row",
                data.len()
            ),
        );
        return Ok(());
    }
    // The radius is a u32 even though the column is REAL: a radius of 256
    // reads as 256.0, where the same four bytes read as f32 are a denormal.
    let radius = u32::from_le_bytes(data[4..8].try_into().expect("four-byte light radius")) as f32;
    let flags = u32::from_le_bytes(data[12..16].try_into().expect("four-byte light flags"));
    let falloff = f32::from_le_bytes(data[16..20].try_into().expect("four-byte light falloff"));
    // Both readers share one rule for a repeated subrecord: the first FNAM decides. The in-house
    // decoder keeps the first occurrence (a malformed one is rejected and flagged) and drops a
    // later one as out of order, so `find` returns the same subrecord in the legacy reader.
    let fade = view
        .find(b"FNAM")
        .filter(|bytes| bytes.len() == 4)
        // silent-ok: the length filter above admits only four-byte FNAM; a shorter field is already status 2.
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(f32::from_le_bytes);
    // 0 absent, 1 valid finite nonnegative, 2 malformed/unsupported negative.
    // Do not merge valid zero, missing FNAM and malformed FNAM into one state.
    let fade_status = match fade {
        Some(value) if value.is_finite() && value >= 0.0 => 1,
        Some(_) => 2,
        None if fnam_rejected || view.find(b"FNAM").is_some() => 2,
        None => 0,
    };
    let fade = (fade_status == 1).then_some(fade).flatten();
    // The rest of the 48-byte DATA (xEdit 4.1.5f `wbDefinitionsTES5.pas`, LIGH): time, FOV, near
    // clip, the flicker period and amplitudes, value and weight. The in-house schema accepts only
    // the full 48 bytes, so a shorter DATA leaves them NULL for both readers.
    let full = (data.len() == LIGHT_DATA_FULL).then_some(data);
    let real = |offset: usize| {
        full.map(|data| f32::from_le_bytes(data[offset..offset + 4].try_into().expect("f32")))
            .filter(|value| value.is_finite())
    };
    let word = |offset: usize| {
        full.map(|data| u32::from_le_bytes(data[offset..offset + 4].try_into().expect("u32")))
    };
    // Links keep a FormID only when the winning record exists with the allowed type: the
    // in-house reader clears unresolved links and the legacy reader keeps them.
    let link = |tag: &[u8; 4], target: &[u8; 4]| {
        view.find(tag)
            .filter(|bytes| bytes.len() == 4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four-byte link")))
            .filter(|id| {
                master
                    .get(id)
                    .is_some_and(|record| !record.is_deleted() && record.record_type == *target)
            })
    };
    tx.prepare_cached(
        "INSERT OR REPLACE INTO lights(id, editor_id, radius, color_r, color_g, color_b, flags, falloff, fade, time, field_of_view, near_clip, flicker_period, flicker_intensity_amplitude, flicker_movement_amplitude, value, weight, sound_id, lens_id, fade_status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)")?.execute(
        params![
            form_id,
            view.get_string(b"EDID"),
            radius,
            i64::from(data[8]),
            i64::from(data[9]),
            i64::from(data[10]),
            flags,
            falloff,
            fade,
            word(0).map(|time| time as i32),
            real(20),
            real(24),
            real(28),
            real(32),
            real(36),
            word(40),
            real(44),
            link(b"SNAM", b"SNDR"),
            link(b"LNAM", b"LENS"),
            fade_status,
        ],
    )?;
    Ok(())
}

/// Write a reference and its exterior spatial projection when DATA holds all six floats.
/// An unusable transform removes any stale placement and warns; canonical records remain owned
/// by the caller. This low-level subset helper has no report sink: its warning is terminal-only.
/// Full exports separately persist placement omissions in their integration report.
pub fn insert_reference(
    tx: &Transaction<'_>,
    form_id: u32,
    cell_id: u32,
    cell: Option<CellMetadata>,
    header_flags: u32,
    subs: &[(Vec<u8>, Vec<u8>)],
) -> Result<()> {
    if !insert_reference_with_canonical_data(
        tx,
        form_id,
        cell_id,
        cell,
        header_flags,
        subs,
        subs,
        false,
    )? {
        eprintln!(
            "warning: placed record {form_id:08X} has missing or short DATA; no placement row"
        );
    }
    Ok(())
}

/// Remove the previous script projection before a placed record is replaced or deleted.
fn delete_reference_scripts(
    tx: &Transaction<'_>,
    form_id: u32,
    scalar_storage: bool,
) -> Result<()> {
    if scalar_storage {
        tx.prepare_cached("DELETE FROM script_property_values WHERE form_id=?1")?
            .execute([form_id])?;
        tx.prepare_cached("DELETE FROM script_properties WHERE form_id=?1")?
            .execute([form_id])?;
    }
    tx.prepare_cached("DELETE FROM scripts WHERE form_id=?1")?
        .execute([form_id])?;
    Ok(())
}

/// Removes one unusable placement and every owned or spatially dependent runtime row.
/// Incoming authored links may still name the canonical record; usable neighbors stay intact.
/// Extend the table list when adding a placement table keyed on `reference_id`.
fn clear_reference_placement(tx: &Transaction<'_>, form_id: u32) -> Result<()> {
    if super::rooms::table_exists(tx, "room_reference_rules")? {
        tx.prepare_cached("DELETE FROM room_reference_rules WHERE reference_id=?1")?
            .execute([form_id])?;
    }
    for table in super::reference_links::REFERENCE_LINK_TABLES {
        tx.prepare_cached(&format!("DELETE FROM {table} WHERE reference_id=?1"))?
            .execute([form_id])?;
    }
    for sql in [
        "DELETE FROM door_links WHERE ref_id=?1 OR destination_ref_id=?1",
        "DELETE FROM reference_rooms WHERE reference_id=?1 OR room_ref=?1",
        "DELETE FROM room_links WHERE room_ref=?1 OR linked_ref=?1",
        "DELETE FROM room_portals WHERE ref_id=?1",
        "UPDATE room_portals SET room_a=NULL WHERE room_a=?1",
        "UPDATE room_portals SET room_b=NULL WHERE room_b=?1",
        "DELETE FROM room_bounds WHERE ref_id=?1",
        "DELETE FROM exterior_spatial WHERE id=?1",
        "DELETE FROM \"references\" WHERE id=?1",
    ] {
        tx.prepare_cached(sql)?.execute([form_id])?;
    }
    Ok(())
}

/// Extract runtime columns from projected fields and preserve the canonical data mirror.
/// Returns false after removing a placement whose required 24-byte DATA cannot be read.
/// xEdit xedit-4.1.5f Core/wbDefinitionsTES5.pas:5320-5323 uses wbDATAPosRot for 3D data.
#[allow(clippy::too_many_arguments)]
fn insert_reference_with_canonical_data(
    tx: &Transaction<'_>,
    form_id: u32,
    cell_id: u32,
    cell: Option<CellMetadata>,
    header_flags: u32,
    projected_subrecords: &[(Vec<u8>, Vec<u8>)],
    canonical_subrecords: &[(Vec<u8>, Vec<u8>)],
    xrds_rejected: bool,
) -> Result<bool> {
    let view = SubrecordView::new(projected_subrecords);
    let Some(transform) = view.find(b"DATA").and_then(|data| data.first_chunk::<24>()) else {
        // A subset update must also clear a formerly usable placement. The complete export
        // already replaced its snapshot; either route keeps records.data for inspection.
        clear_reference_placement(tx, form_id)?;
        return Ok(false);
    };
    let component = |offset| {
        f32::from_le_bytes([
            transform[offset],
            transform[offset + 1],
            transform[offset + 2],
            transform[offset + 3],
        ])
    };
    let pos = [component(0), component(4), component(8)];
    let rot = [component(12), component(16), component(20)];
    let scale = view
        .find(b"XSCL")
        .filter(|data| data.len() >= 4)
        .map(|data| f32::from_le_bytes(data[..4].try_into().unwrap()))
        .unwrap_or(1.0);
    let base_form_id = view.get_form_id(b"NAME").unwrap_or(0);
    let (grid_x, grid_y, worldspace_id) = cell.unwrap_or((None, None, None));
    let is_exterior = worldspace_id.is_some() || (grid_x.is_some() && grid_y.is_some());
    // Exterior persistent references are owned by the worldspace's persistent
    // cell even when their position lies many cells away. Derive local
    // coordinates from the actual position and keep the R-Tree global so
    // streaming is spatial rather than tied to the owning CELL record.
    let local_x = is_exterior.then(|| pos[0] - (pos[0] / CELL_SIZE).floor() * CELL_SIZE);
    let local_y = is_exterior.then(|| pos[1] - (pos[1] / CELL_SIZE).floor() * CELL_SIZE);
    let blob = serialize_subrecords(canonical_subrecords);
    // XRDS is a signed f32 radius offset. xEdit cce1c424, wbDefinitionsCommon.pas
    // wbREFRRadiusToStr adds it to LIGH DATA radius when displaying/editing effective range.
    // Keep the raw offset; the runtime evaluates and bounds the sum separately.
    // A repeated XRDS follows the in-house decoder, which accepts repeated REFR fields in any
    // order: the first four-byte XRDS wins, and a malformed one counts only when none is
    // well-formed. (A repeated LIGH FNAM differs: the decoder keeps the first occurrence.)
    let radius_override = view
        .find_all(b"XRDS")
        .find(|bytes| bytes.len() == 4)
        // silent-ok: find_all is filtered to four-byte XRDS; other lengths already count as status 2.
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(f32::from_le_bytes);
    let radius_status = match radius_override {
        Some(value) if value.is_finite() => 1,
        Some(_) => 2,
        None if xrds_rejected || view.find(b"XRDS").is_some() => 2,
        None => 0,
    };
    let radius_override = (radius_status == 1).then_some(radius_override).flatten();
    let (enable_parent_id, enable_parent_flags) = view
        .find(b"XESP")
        // xEdit 4.1.5f, wbDefinitionsTES5.pas:5156 (wbXESP): FormID, one flags byte, three unused bytes.
        // Pinned layout source: see SubrecordXESP.
        .and_then(SubrecordXESP::parse)
        .map(|parent| (parent.parent_ref, parent.flags))
        .unzip();
    // `XEMI` names the light or region whose colour the model's `External_Emittance` surfaces
    // take; `insert_emittance_sources` resolves it. A cleared link (0) stores NULL.
    let emittance_id = view
        .find(b"XEMI")
        .filter(|bytes| bytes.len() >= 4)
        .map(|bytes| u32::from_le_bytes(bytes[..4].try_into().expect("four-byte XEMI source")))
        .filter(|&id| id != 0);

    tx.prepare_cached(
        "INSERT OR REPLACE INTO \"references\"(id, cell_id, worldspace_id, base_form_id, is_exterior, pos_x, pos_y, pos_z, local_x, local_y, rot_x, rot_y, rot_z, scale, radius_override, header_flags, enable_parent_id, enable_parent_flags, emittance_id, data, radius_status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)")?.execute(
        params![form_id, cell_id, worldspace_id, base_form_id, is_exterior, pos[0], pos[1], pos[2], local_x, local_y, rot[0], rot[1], rot[2], scale, radius_override, header_flags, enable_parent_id, enable_parent_flags, emittance_id, blob, radius_status],
    )?;
    // A subset update can move an existing reference into an interior.
    tx.prepare_cached("DELETE FROM exterior_spatial WHERE id=?1")?
        .execute([form_id])?;
    if is_exterior {
        tx.prepare_cached("INSERT OR REPLACE INTO exterior_spatial(id, minX, maxX, minY, maxY, minZ, maxZ, cell_id, worldspace_id) VALUES (?1, ?2, ?2, ?3, ?3, ?4, ?4, ?5, ?6)")?.execute( params![form_id, pos[0], pos[1], pos[2], cell_id, worldspace_id])?;
    }
    Ok(true)
}

pub(super) fn project_cell(
    tx: &Transaction<'_>,
    links: &crate::esm::environment::Links<'_>,
    form_id: u32,
    record: &RawRecord,
    projected_subrecords: &[(Vec<u8>, Vec<u8>)],
) -> Result<CellMetadata> {
    let (grid_x, grid_y, interior_name) = extract_cell_info(projected_subrecords);
    let view = SubrecordView::new(projected_subrecords);
    let full_name = view.get_string(b"FULL");
    crate::esm::environment::insert_cell(tx, links, form_id, &view)?;
    tx.prepare_cached(
        "INSERT OR REPLACE INTO cells(id, worldspace_id, grid_x, grid_y, interior_name, flags, data, full_name) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?.execute(
        params![form_id, record.worldspace_form_id, grid_x, grid_y, interior_name, record.flags, serialize_subrecords(&record.subrecords), full_name],
    )?;
    Ok((grid_x, grid_y, record.worldspace_form_id))
}

pub fn validate_database(conn: &Connection) -> Result<()> {
    let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result != "ok" {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let version: u32 = conn.query_row("SELECT version FROM schema_info LIMIT 1", [], |row| {
        row.get(0)
    })?;
    if version != shared::WORLD_DATABASE_SCHEMA_VERSION {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;

    #[test]
    fn combined_optional_schema_upgrade_preserves_rows_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        for table in ["worldspaces", "cells", "statics"] {
            conn.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN full_name;"))
                .unwrap();
        }
        conn.execute_batch("ALTER TABLE door_links DROP COLUMN destination_name;")
            .unwrap();
        // Use the pre-extension table verbatim: SQLite cannot DROP its final commented column.
        conn.execute_batch(
            "DROP TABLE lights;
             CREATE TABLE lights (
                 id INTEGER PRIMARY KEY, editor_id TEXT, radius REAL NOT NULL,
                 color_r INTEGER NOT NULL, color_g INTEGER NOT NULL, color_b INTEGER NOT NULL,
                 flags INTEGER NOT NULL, falloff REAL NOT NULL, fade REAL
             );",
        )
        .unwrap();
        conn.execute_batch("INSERT INTO statics(id, editor_id, flags) VALUES (1, 'retained', 0);")
            .unwrap();
        create_tables(&conn).unwrap();
        create_tables(&conn).unwrap();
        conn.prepare("SELECT destination_name FROM door_links")
            .unwrap();
        let row: (String, Option<String>) = conn
            .query_row(
                "SELECT editor_id, full_name FROM statics WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("retained".to_owned(), None));
        for table in ["worldspaces", "cells", "statics"] {
            conn.prepare(&format!("SELECT full_name FROM {table}"))
                .unwrap();
        }
        for (column, _) in LIGHT_DETAIL_COLUMNS {
            conn.prepare(&format!("SELECT {column} FROM lights"))
                .unwrap();
        }
    }

    #[test]
    fn complete_snapshot_replaces_both_optional_families_transactionally() {
        let mut conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO cell_environment(cell_id) VALUES (1);
            INSERT INTO reference_details(reference_id) VALUES (2);
            INSERT INTO door_links(ref_id,destination_ref_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,destination_cell_id) VALUES (2,3,1,2,3,0,0,0,4);
            CREATE TABLE caller_owned(value INTEGER);
            INSERT INTO caller_owned VALUES (3);",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        replace_complete_snapshot(&tx).unwrap();
        for table in crate::esm::environment::ENVIRONMENT_TABLES
            .iter()
            .chain(crate::esm::reference_links::REFERENCE_LINK_TABLES.iter())
            .chain(["door_links"].iter())
        {
            let count: i64 = tx
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        tx.rollback().unwrap();
        for table in [
            "cell_environment",
            "reference_details",
            "door_links",
            "caller_owned",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "{table}");
        }
        let tx = conn.transaction().unwrap();
        replace_complete_snapshot(&tx).unwrap();
        tx.commit().unwrap();
        let value: i64 = conn
            .query_row("SELECT value FROM caller_owned", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, 3);
    }

    /// The schema has the interior index, the exterior spatial table and every projection table.
    #[test]
    fn creates_hybrid_spatial_schema() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let interior_index: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_references_cell'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let exterior_table: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'exterior_spatial'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((interior_index, exterior_table), (1, 1));
        for table in [
            "worldspaces",
            "cells",
            "references",
            "land",
            "statics",
            "lights",
            "emittance",
            "npcs",
            "movement_types",
            "race_movement_links",
            "default_objects",
            "movement_game_settings",
            "scripts",
            "waters",
            "texture_sets",
            "landscape_textures",
            "grass_types",
            "landscape_texture_grasses",
        ] {
            let present: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing semantic table {table}");
        }
        validate_database(&conn).unwrap();
    }

    #[test]
    fn exports_selected_movement_values_and_rejects_bad_sped() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let speeds: Vec<u8> = [
            80.09f32,
            370.0,
            79.75,
            370.0,
            80.1,
            370.0,
            71.93,
            205.25,
            std::f32::consts::PI,
            std::f32::consts::PI,
            std::f32::consts::PI,
        ]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
        let movement = RawRecord {
            form_id: 0x0003_580D,
            record_type: *b"MOVT",
            flags: 0,
            subrecords: vec![
                (b"EDID".to_vec(), b"NPC_Default_MT\0".to_vec()),
                (b"SPED".to_vec(), speeds),
            ],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 2,
        };
        let jump = RawRecord {
            form_id: 0x000A_BEF6,
            record_type: *b"GMST",
            flags: 0,
            subrecords: vec![
                (b"EDID".to_vec(), b"fJumpHeightMin\0".to_vec()),
                (b"DATA".to_vec(), 76.0f32.to_le_bytes().to_vec()),
            ],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        let race = RawRecord {
            form_id: 0x0001_3746,
            record_type: *b"RACE",
            flags: 0,
            subrecords: vec![(b"EDID".to_vec(), b"NordRace\0".to_vec())],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 1,
        };
        let master = HashMap::from([
            (movement.form_id, movement.clone()),
            (jump.form_id, jump),
            (race.form_id, race.clone()),
        ]);
        export_to_db(&conn, &master).unwrap();
        let (walk_link, run_link, race_source): (Option<u32>, Option<u32>, u32) = conn
            .query_row(
                "SELECT walk_movt_id, run_movt_id, load_order FROM race_movement_links WHERE id=79686",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((walk_link, run_link, race_source), (None, None, 1));
        let (forward_walk, forward_run, back_run, source): (f32, f32, f32, u32) = conn.query_row(
            "SELECT forward_walk, forward_run, back_run, load_order FROM movement_types WHERE id=219149",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).unwrap();
        assert_eq!(
            (forward_walk, forward_run, back_run, source),
            (80.1, 370.0, 205.25, 2)
        );
        let height: f32 = conn
            .query_row(
                "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(height, 76.0);

        let mut legacy = movement.clone();
        legacy.subrecords[1].1.truncate(40);
        export_to_db(&conn, &HashMap::from([(legacy.form_id, legacy)])).unwrap();

        // Any MOVT exports with its own editor ID, including retail zero speeds (no strafing).
        let mut creature = movement.clone();
        creature.form_id = 0x000C_EFCF;
        creature.subrecords[0].1 = b"NPC_Drunk_MT\0".to_vec();
        creature.subrecords[1].1[0..4].copy_from_slice(&0.0f32.to_le_bytes());
        let defaults = RawRecord {
            form_id: 0x0000_0031,
            record_type: *b"DOBJ",
            flags: 0,
            subrecords: vec![(
                b"DNAM".to_vec(),
                [
                    *b"DMWL",
                    0x0003_580Du32.to_le_bytes(),
                    *b"DMRN",
                    0x000C_EFCFu32.to_le_bytes(),
                    // A null link names no movement type, so it exports no row.
                    *b"DMSW",
                    0u32.to_le_bytes(),
                ]
                .concat(),
            )],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 4,
        };
        export_to_db(
            &conn,
            &HashMap::from([(creature.form_id, creature), (defaults.form_id, defaults)]),
        )
        .unwrap();
        let (editor_id, left_walk): (String, f32) = conn
            .query_row(
                "SELECT editor_id, left_walk FROM movement_types WHERE id=0x000CEFCF",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((editor_id.as_str(), left_walk), ("NPC_Drunk_MT", 0.0));
        let defaults: Vec<(String, u32)> = conn
            .prepare("SELECT key, form_id FROM default_objects ORDER BY key")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            defaults,
            [
                ("DMRN".to_string(), 0x000C_EFCF),
                ("DMWL".to_string(), 0x0003_580D)
            ]
        );

        // A later export that lists DMRN with a null link removes it and keeps DMWL.
        let null_run = RawRecord {
            subrecords: vec![(
                b"DNAM".to_vec(),
                [b"DMRN".as_slice(), &0u32.to_le_bytes()].concat(),
            )],
            load_order: 5,
            form_id: 0x0000_0031,
            record_type: *b"DOBJ",
            flags: 0,
            cell_form_id: None,
            worldspace_form_id: None,
        };
        export_to_db(&conn, &HashMap::from([(null_run.form_id, null_run)])).unwrap();
        let after_null: Vec<String> = conn
            .prepare("SELECT key FROM default_objects ORDER BY key")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(after_null, ["DMWL"]);

        // A MOVT with no editor ID is malformed, not published under an empty name.
        let mut unnamed = movement.clone();
        unnamed.form_id = 0x000C_EFD0;
        unnamed.subrecords.remove(0);
        let error = export_to_db(&conn, &HashMap::from([(unnamed.form_id, unnamed)])).unwrap_err();
        assert!(error.to_string().contains("MOVT EDID"), "{error}");
        let mut empty_name = movement.clone();
        empty_name.form_id = 0x000C_EFD1;
        empty_name.subrecords[0].1 = b"\0".to_vec();
        let error =
            export_to_db(&conn, &HashMap::from([(empty_name.form_id, empty_name)])).unwrap_err();
        assert!(error.to_string().contains("MOVT EDID"), "{error}");

        let mut negative = movement.clone();
        negative.subrecords[1].1[4..8].copy_from_slice(&(-1.0f32).to_le_bytes());
        let error =
            export_to_db(&conn, &HashMap::from([(negative.form_id, negative)])).unwrap_err();
        assert!(error.to_string().contains("MOVT SPED[1]"));
        let mut broken = movement;
        broken.subrecords[1].1[0..4].copy_from_slice(&f32::NAN.to_le_bytes());
        let error = export_to_db(&conn, &HashMap::from([(broken.form_id, broken)])).unwrap_err();
        assert!(error.to_string().contains("MOVT SPED[0]"));
        let mut broken_race = race;
        broken_race
            .subrecords
            .push((b"WKMV".to_vec(), vec![1, 2, 3]));
        let error =
            export_to_db(&conn, &HashMap::from([(broken_race.form_id, broken_race)])).unwrap_err();
        assert!(error.to_string().contains("RACE WKMV length"));
    }

    #[test]
    fn default_objects_without_usable_pairs_export_no_rows_and_do_not_fail() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let dobj = |form_id: u32, subrecords: Vec<(Vec<u8>, Vec<u8>)>| RawRecord {
            form_id,
            record_type: *b"DOBJ",
            flags: 0,
            subrecords,
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 3,
        };
        let missing = dobj(0x0100_0031, vec![(b"EDID".to_vec(), b"Mod\0".to_vec())]);
        // A trailing partial pair is ignored; the whole pair before it is kept.
        let partial_tail = dobj(
            0x0200_0031,
            vec![(
                b"DNAM".to_vec(),
                [
                    b"DMWL".as_slice(),
                    &0x0003_580Du32.to_le_bytes(),
                    &[1, 2, 3],
                ]
                .concat(),
            )],
        );
        let short = dobj(0x0300_0031, vec![(b"DNAM".to_vec(), vec![1, 2, 3])]);
        export_to_db(
            &conn,
            &HashMap::from([
                (missing.form_id, missing),
                (partial_tail.form_id, partial_tail),
                (short.form_id, short),
            ]),
        )
        .unwrap();
        let rows: Vec<(String, u32)> = conn
            .prepare("SELECT key, form_id FROM default_objects")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, [("DMWL".to_string(), 0x0003_580D)]);
    }

    #[test]
    fn reads_water_flow_normals_from_nam5_not_binary_dnam() {
        let subrecords = vec![
            (b"DNAM".to_vec(), vec![0, 1, 2, 3, 4, 5]),
            (
                b"NAM5".to_vec(),
                b"Data\\Textures\\Water\\RiverFlow.dds\0".to_vec(),
            ),
        ];
        let view = SubrecordView::new(&subrecords);

        assert_eq!(
            water_flow_normal_path(&view).as_deref(),
            Some("water/riverflow.dds")
        );
    }

    /// A synthetic DNAM with DefaultWater's final (Update.esm-applied) values at the documented
    /// offsets (`docs/research/water.md` section 1.2/1.3): reflectivity 0.8, fresnel 0.10,
    /// shallow (38,39,24), deep (5,14,18), reflection (119,140,157). Everything else is zeroed.
    fn default_water_dnam(len: usize) -> Vec<u8> {
        let mut dnam = vec![0u8; len];
        dnam[20..24].copy_from_slice(&0.8f32.to_le_bytes());
        dnam[24..28].copy_from_slice(&0.10f32.to_le_bytes());
        dnam[40..44].copy_from_slice(&[38, 39, 24, 0]);
        dnam[44..48].copy_from_slice(&[5, 14, 18, 0]);
        dnam[48..52].copy_from_slice(&[119, 140, 157, 0]);
        dnam
    }

    #[test]
    fn parses_water_dnam_colors_and_factors_at_both_dnam_lengths() {
        for len in [228usize, 232] {
            let dnam = default_water_dnam(len);
            let parsed =
                parse_water_dnam(&dnam).unwrap_or_else(|| panic!("{len}-byte DNAM must decode"));
            assert_eq!(parsed.shallow_color, u32::from_le_bytes([38, 39, 24, 0]));
            assert_eq!(parsed.deep_color, u32::from_le_bytes([5, 14, 18, 0]));
            assert_eq!(
                parsed.reflection_color,
                u32::from_le_bytes([119, 140, 157, 0])
            );
            assert!((parsed.fresnel - 0.10).abs() < 1.0e-6);
            assert!((parsed.reflectivity - 0.8).abs() < 1.0e-6);
        }
    }

    #[test]
    fn water_export_reads_colors_from_dnam_not_nam0_nam1() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        // NAM0 (linear velocity) and NAM1 (angular velocity) are three f32s each; the old code
        // packed their first four bytes as if they were the shallow/deep colour. Give them a
        // value that would decode to a colour nothing like DNAM's, so the test fails on the old
        // code and passes once WATR reads colours from DNAM instead.
        let nam0: Vec<u8> = [9.0f32, 9.0, 9.0]
            .into_iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let nam1 = nam0.clone();
        let record = RawRecord {
            form_id: 0x0000_0018,
            record_type: *b"WATR",
            flags: 0,
            subrecords: vec![
                (b"EDID".to_vec(), b"DefaultWater\0".to_vec()),
                (b"NAM0".to_vec(), nam0),
                (b"NAM1".to_vec(), nam1),
                (b"DNAM".to_vec(), default_water_dnam(232)),
            ],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        let mut master = HashMap::new();
        master.insert(record.form_id, record);
        export_to_db(&conn, &master).unwrap();

        let (shallow, deep, reflection, fresnel, reflectivity): (u32, u32, u32, f32, f32) = conn
            .query_row(
                "SELECT shallow_color, deep_color, reflection_color, fresnel, reflectivity FROM waters WHERE id=?1",
                [0x0000_0018u32],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(shallow, u32::from_le_bytes([38, 39, 24, 0]));
        assert_eq!(deep, u32::from_le_bytes([5, 14, 18, 0]));
        assert_eq!(reflection, u32::from_le_bytes([119, 140, 157, 0]));
        assert!((fresnel - 0.10).abs() < 1.0e-6);
        assert!((reflectivity - 0.8).abs() < 1.0e-6);
    }

    #[test]
    fn reads_ltex_material_from_mnam_and_havok_from_hnam() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        // Real-world shape: MNAM is the material-type MATT FormID, HNAM is two
        // Havok bytes (friction, restitution). Values mirror observed rows like
        // LDirtSnowPath01 (HNAM 1e 1e).
        let record = RawRecord {
            form_id: 0x0001_B342,
            record_type: *b"LTEX",
            flags: 0,
            subrecords: vec![
                (b"EDID".to_vec(), b"LDirtSnowPath01\0".to_vec()),
                (b"TNAM".to_vec(), 0x0001_2F38u32.to_le_bytes().to_vec()),
                (b"MNAM".to_vec(), 0x0001_2F38u32.to_le_bytes().to_vec()),
                (b"HNAM".to_vec(), vec![0x1e, 0x1e]),
            ],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        let mut master = HashMap::new();
        master.insert(record.form_id, record);
        export_to_db(&conn, &master).unwrap();

        let (texture_set_id, material, friction, restitution): (
            Option<u32>,
            Option<u32>,
            Option<f32>,
            Option<f32>,
        ) = conn
            .query_row(
                "SELECT texture_set_id, material_type, friction, restitution FROM landscape_textures WHERE id=?1",
                [0x0001_B342u32],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(texture_set_id, Some(0x0001_2F38));
        assert_eq!(material, Some(0x0001_2F38));
        assert_eq!(friction, Some(30.0));
        assert_eq!(restitution, Some(30.0));
    }

    /// Exports placeable world models, including ARMO male and female world slots.
    #[test]
    fn exports_world_models_for_every_placeable_base_type() {
        /// Builds one synthetic base object with an optional model-slot payload.
        fn base(
            form_id: u32,
            record_type: &[u8; 4],
            model_tag: &[u8; 4],
            model: Option<&str>,
        ) -> RawRecord {
            let mut subrecords = vec![(b"EDID".to_vec(), b"TestBase\0".to_vec())];
            if let Some(model) = model {
                let mut path = model.as_bytes().to_vec();
                path.push(0);
                subrecords.push((model_tag.to_vec(), path));
            }
            RawRecord {
                form_id,
                record_type: *record_type,
                flags: 0,
                subrecords,
                cell_form_id: None,
                worldspace_form_id: None,
                load_order: 0,
            }
        }
        let master: HashMap<u32, RawRecord> = [
            base(1, b"TREE", b"MODL", Some("Landscape\\Trees\\Pine01.nif")),
            base(
                2,
                b"FLOR",
                b"MODL",
                Some("Landscape\\Plants\\Thistle01.nif"),
            ),
            base(3, b"CONT", b"MODL", Some("Furniture\\Chest01.nif")),
            base(4, b"DOOR", b"MODL", Some("Architecture\\Door01.nif")),
            base(5, b"ACTI", b"MODL", None),
            // A light with geometry lands in `statics` like any other modelled
            // base object; a modelless light lives only in the `lights` table
            // (PR40) and gets no `statics` row at all.
            base(6, b"LIGH", b"MODL", Some("Clutter\\Lantern01.nif")),
            base(7, b"WEAP", b"MODL", Some("Weapons\\Sword01.nif")),
            base(8, b"ARMO", b"MOD2", Some("Armor\\Helmet01.nif")),
            base(9, b"ARMO", b"MOD4", Some("Armor\\HelmetFemale01.nif")),
            base(10, b"NPC_", b"MODL", None),
            // ARMO carries a binary MODL chunk that is not a path; with no
            // MOD2/MOD4 present the row must store NULL, not garbage.
            base(11, b"ARMO", b"MODL", Some("G.\u{1}binary")),
        ]
        .into_iter()
        .map(|record| (record.form_id, record))
        .collect();
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        export_to_db(&conn, &master).unwrap();
        let models: Vec<(u32, Option<String>)> = conn
            .prepare("SELECT id, model_path FROM statics ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            models,
            vec![
                (1, Some("Landscape\\Trees\\Pine01.nif".to_owned())),
                (2, Some("Landscape\\Plants\\Thistle01.nif".to_owned())),
                (3, Some("Furniture\\Chest01.nif".to_owned())),
                (4, Some("Architecture\\Door01.nif".to_owned())),
                (5, None),
                (6, Some("Clutter\\Lantern01.nif".to_owned())),
                (7, Some("Weapons\\Sword01.nif".to_owned())),
                (8, Some("Armor\\Helmet01.nif".to_owned())),
                (9, Some("Armor\\HelmetFemale01.nif".to_owned())),
                (11, None),
            ]
        );
    }

    #[test]
    fn indexes_persistent_exterior_references_by_global_position() {
        let mut conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let tx = conn.transaction().unwrap();
        let position = [147_182.05f32, 34_033.137f32, 80.0f32];
        let mut data = Vec::new();
        for value in position.into_iter().chain([0.0, 0.0, 0.0]) {
            data.extend_from_slice(&value.to_le_bytes());
        }
        insert_reference(
            &tx,
            0xE7F,
            1,
            Some((None, None, Some(0x3C))),
            0,
            &[(b"DATA".to_vec(), data)],
        )
        .unwrap();
        let (x, y, local_x, local_y): (f32, f32, f32, f32) = tx
            .query_row(
                "SELECT x.minX,x.minY,r.local_x,r.local_y FROM exterior_spatial x JOIN \"references\" r ON r.id=x.id WHERE x.id=?1",
                [0xE7Fu32],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert!((x - position[0]).abs() < 0.1);
        assert!((y - position[1]).abs() < 0.1);
        assert!((0.0..CELL_SIZE).contains(&local_x));
        assert!((0.0..CELL_SIZE).contains(&local_y));
    }

    fn cstr(value: &str) -> Vec<u8> {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        bytes
    }

    fn floats(values: [f32; 3]) -> Vec<u8> {
        values.into_iter().flat_map(f32::to_le_bytes).collect()
    }

    fn record(
        form_id: u32,
        record_type: &[u8; 4],
        cell: Option<u32>,
        worldspace: Option<u32>,
        subrecords: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> RawRecord {
        RawRecord {
            form_id,
            record_type: *record_type,
            flags: 0,
            subrecords,
            cell_form_id: cell,
            worldspace_form_id: worldspace,
            load_order: 0,
        }
    }

    /// A `REFR` inside `cell` with a `DATA` transform. The owning cell, not the
    /// record, supplies the worldspace the exporter stores, so it is not a
    /// parameter here.
    fn reference(
        form_id: u32,
        cell: u32,
        base_form_id: u32,
        position: [f32; 3],
        rotation: [f32; 3],
    ) -> RawRecord {
        record(
            form_id,
            b"REFR",
            Some(cell),
            None,
            vec![
                (b"NAME".to_vec(), base_form_id.to_le_bytes().to_vec()),
                (
                    b"DATA".to_vec(),
                    [floats(position), floats(rotation)].concat(),
                ),
            ],
        )
    }

    /// A `LIGH` `DATA` built the way the game writes it: time, radius, colour
    /// (RGB + one unused byte), flags and falloff first, then the FOV, near
    /// clip, flicker period, two flicker amplitudes, value and weight the
    /// engine does not read - 48 bytes in all.
    fn light_data_bytes(radius: u32, color: [u8; 3], flags: u32, falloff: f32) -> Vec<u8> {
        let mut bytes = (-1i32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&radius.to_le_bytes());
        bytes.extend_from_slice(&[color[0], color[1], color[2], 0]);
        bytes.extend_from_slice(&flags.to_le_bytes());
        bytes.extend_from_slice(&falloff.to_le_bytes());
        for value in [90.0f32, 6.585, 0.333, 0.5, 16.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0.0f32.to_le_bytes());
        assert_eq!(bytes.len(), 48);
        bytes
    }

    /// A `LIGH` record: editor id, `DATA`, and `FNAM` when the light has one.
    fn light(
        form_id: u32,
        editor_id: &str,
        data: Vec<u8>,
        fade: Option<f32>,
        model: Option<&str>,
    ) -> RawRecord {
        let mut subrecords = vec![
            (b"EDID".to_vec(), cstr(editor_id)),
            (b"DATA".to_vec(), data),
        ];
        if let Some(fade) = fade {
            subrecords.push((b"FNAM".to_vec(), fade.to_le_bytes().to_vec()));
        }
        if let Some(model) = model {
            subrecords.push((b"MODL".to_vec(), cstr(model)));
        }
        record(form_id, b"LIGH", None, None, subrecords)
    }

    #[test]
    fn writes_a_lights_row_from_the_light_data() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let mut master = HashMap::new();
        master.insert(
            0xCB3B0,
            light(
                0xCB3B0,
                "DefaultTorch01NS_FastSaturate",
                light_data_bytes(256, [0xE9, 0x9E, 0x4B], 0x2009, 1.0),
                Some(1.25),
                None,
            ),
        );

        export_to_db(&conn, &master).unwrap();

        type LightRow = (u32, String, f64, i64, i64, i64, i64, f64, f64);
        let row: LightRow = conn
            .query_row(
                "SELECT id,editor_id,radius,color_r,color_g,color_b,flags,falloff,fade FROM lights WHERE id=?1",
                [0xCB3B0u32],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                0xCB3B0,
                "DefaultTorch01NS_FastSaturate".to_owned(),
                256.0,
                233,
                158,
                75,
                0x2009,
                1.0,
                1.25,
            )
        );
    }

    #[test]
    fn gives_a_lights_row_to_a_light_with_no_model_and_none_to_a_light_without_data() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let mut master = HashMap::new();
        // Invisible lights are the common case: they light the space with
        // nothing to draw, so they belong in `lights` and not in `statics`.
        master.insert(
            0x800,
            light(
                0x800,
                "FXLightInvisible",
                light_data_bytes(128, [255, 255, 255], 0x10, 1.0),
                None,
                None,
            ),
        );
        // A light that does have geometry is both.
        master.insert(
            0x600,
            light(
                0x600,
                "LightWithModel",
                light_data_bytes(512, [16, 32, 64], 0x1, 2.0),
                None,
                Some("Clutter\\InvisibleLightMarker.nif"),
            ),
        );
        // `radius`, `color_*`, `flags` and `falloff` are NOT NULL, so a record
        // whose DATA cannot supply them gets no row rather than invented ones.
        master.insert(
            0x900,
            record(
                0x900,
                b"LIGH",
                None,
                None,
                vec![(b"EDID".to_vec(), cstr("NoDataAtAll"))],
            ),
        );

        export_to_db(&conn, &master).unwrap();

        let rows: Vec<(u32, f64)> = {
            let mut statement = conn
                .prepare("SELECT id,radius FROM lights ORDER BY id")
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(rows, vec![(0x600, 512.0), (0x800, 128.0)]);
        let statics: Vec<u32> = {
            let mut statement = conn.prepare("SELECT id FROM statics ORDER BY id").unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<u32>>>()
                .unwrap()
        };
        assert_eq!(statics, vec![0x600], "only the modelled light is a static");
    }

    #[test]
    fn drops_a_truncated_light_data_without_panicking() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let mut master = HashMap::new();
        let full = light_data_bytes(256, [1, 2, 3], 0x8, 1.0);
        for (form_id, length) in [(0x1000u32, 0usize), (0x2000, 4), (0x3000, 12), (0x4000, 19)] {
            let mut truncated = full.clone();
            truncated.truncate(length);
            master.insert(form_id, light(form_id, "Truncated", truncated, None, None));
        }
        // A light whose DATA is shorter than the four fields the row needs is
        // dropped; the rest of the export still runs.
        master.insert(
            0x5000,
            light(
                0x5000,
                "Complete",
                full,
                Some(1.0),
                Some("Clutter\\Candle.nif"),
            ),
        );

        export_to_db(&conn, &master).unwrap();

        let ids: Vec<u32> = {
            let mut statement = conn.prepare("SELECT id FROM lights").unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<u32>>>()
                .unwrap()
        };
        assert_eq!(ids, vec![0x5000], "only the complete DATA produced a row");
        let statics: i64 = conn
            .query_row("SELECT count(*) FROM statics WHERE id=0x5000", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(statics, 1);
    }

    #[test]
    fn v174_exported_enable_parent_ignores_xesp_padding() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let mut master = HashMap::new();
        let mut expected = Vec::new();
        let parent = 0x1234_5678u32;
        let mut id = 0x1000;
        for record_type in [
            *b"REFR", *b"ACHR", *b"ACRE", *b"PGRE", *b"PMIS", *b"PHZD", *b"PARW", *b"PBAR",
            *b"PBEA", *b"PCON", *b"PFLA",
        ] {
            for flags in [0u8, 1, 2, 3, 0x80, 0xFF] {
                for padding in [[0; 3], [0x0D, 0xBF, 0x38]] {
                    let mut record = reference(id, 0x100, 0x900, [0.0; 3], [0.0; 3]);
                    record.record_type = record_type;
                    let mut xesp = parent.to_le_bytes().to_vec();
                    xesp.push(flags);
                    xesp.extend_from_slice(&padding);
                    record.subrecords.push((b"XESP".to_vec(), xesp));
                    master.insert(id, record);
                    expected.push((id, Some(parent), Some(u32::from(flags))));
                    id += 1;
                }
            }
        }
        // Missing and incomplete enable-parent subrecords have no projection.
        master.insert(id, reference(id, 0x100, 0x900, [0.0; 3], [0.0; 3]));
        expected.push((id, None, None));
        id += 1;
        for length in 0..8 {
            let mut record = reference(id, 0x100, 0x900, [0.0; 3], [0.0; 3]);
            record
                .subrecords
                .push((b"XESP".to_vec(), vec![0xFF; length]));
            master.insert(id, record);
            expected.push((id, None, None));
            id += 1;
        }

        export_to_db(&conn, &master).unwrap();

        for (id, parent, flags) in expected {
            let actual: (Option<u32>, Option<u32>, Vec<u8>) = conn
                .query_row(
                    "SELECT enable_parent_id, enable_parent_flags, data FROM \"references\" WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!((actual.0, actual.1), (parent, flags), "reference {id:08X}");
            let raw =
                rkyv::from_bytes::<crate::esm::types::ArchivedRecordData, rkyv::rancor::Error>(
                    &actual.2,
                )
                .unwrap();
            let stored = raw.subrecords.iter().find(|sub| &sub.tag == b"XESP");
            let original = master[&id]
                .subrecords
                .iter()
                .find(|(tag, _)| tag == b"XESP");
            assert_eq!(
                stored.map(|sub| &sub.data),
                original.map(|(_, data)| data),
                "raw XESP for reference {id:08X}"
            );
        }
    }

    #[test]
    fn stores_a_reference_radius_override_from_xrds() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let mut master = HashMap::new();
        let mut overridden = reference(0x1000, 0x100, 0x900, [0.0; 3], [0.0; 3]);
        overridden.subrecords.push((
            b"XRDS".to_vec(),
            544.3856f32.to_le_bytes().to_vec(), // an ice candle in Skyrim.esm
        ));
        master.insert(0x1000, overridden);
        master.insert(0x2000, reference(0x2000, 0x100, 0x900, [0.0; 3], [0.0; 3]));
        // `XRDS` shorter than the float it holds is not a radius.
        let mut truncated = reference(0x3000, 0x100, 0x900, [0.0; 3], [0.0; 3]);
        truncated
            .subrecords
            .push((b"XRDS".to_vec(), vec![0xAE, 0x18]));
        master.insert(0x3000, truncated);

        export_to_db(&conn, &master).unwrap();

        let radius = |id: u32| -> Option<f64> {
            conn.query_row(
                "SELECT radius_override FROM \"references\" WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(radius(0x1000), Some(f64::from(544.3856f32)));
        assert_eq!(radius(0x2000), None, "a reference without XRDS stays NULL");
        assert_eq!(radius(0x3000), None, "a two-byte XRDS holds no radius");
    }

    /// The in-house decoder drops a field too short for its float before export, so the exporter
    /// never sees the bytes. The rejection it reports must write the same "malformed" status (2)
    /// the legacy reader writes for the same bytes, not "absent" (0).
    #[test]
    fn rejected_light_inputs_write_the_same_malformed_status_as_the_legacy_reader() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let mut master = HashMap::new();
        let data = || light_data_bytes(256, [255, 200, 120], 0, 1.0);
        // Legacy shape: the short fields are still in the record.
        let mut legacy_light = light(0xA00, "LegacyShortFade", data(), None, None);
        legacy_light
            .subrecords
            .push((b"FNAM".to_vec(), vec![0xAE, 0x18]));
        master.insert(0xA00, legacy_light);
        let mut legacy_ref = reference(0xB00, 0x100, 0xA00, [0.0; 3], [0.0; 3]);
        legacy_ref
            .subrecords
            .push((b"XRDS".to_vec(), vec![0xAE, 0x18]));
        master.insert(0xB00, legacy_ref);
        // In-house shape: the same fields were rejected, so the record has no such subrecord.
        master.insert(0xA01, light(0xA01, "RejectedFade", data(), None, None));
        master.insert(0xB01, reference(0xB01, 0x100, 0xA01, [0.0; 3], [0.0; 3]));
        // Neither shape: genuinely absent.
        master.insert(0xA02, light(0xA02, "NoFade", data(), None, None));
        master.insert(0xB02, reference(0xB02, 0x100, 0xA02, [0.0; 3], [0.0; 3]));
        // A valid field that is present wins over a rejection flag for a duplicate occurrence.
        master.insert(0xA03, light(0xA03, "ValidFade", data(), Some(1.5), None));
        let mut valid_ref = reference(0xB03, 0x100, 0xA03, [0.0; 3], [0.0; 3]);
        valid_ref
            .subrecords
            .push((b"XRDS".to_vec(), 64.0f32.to_le_bytes().to_vec()));
        master.insert(0xB03, valid_ref);
        // Legacy shape with a repeated subrecord: the first occurrence decides, in either order.
        let mut short_then_valid = light(0xA04, "ShortThenValid", data(), Some(2.5), None);
        short_then_valid
            .subrecords
            .insert(0, (b"FNAM".to_vec(), vec![0xAE, 0x18]));
        master.insert(0xA04, short_then_valid);
        let mut valid_then_short = light(0xA05, "ValidThenShort", data(), Some(3.5), None);
        valid_then_short
            .subrecords
            .push((b"FNAM".to_vec(), vec![0xAE, 0x18]));
        master.insert(0xA05, valid_then_short);
        let mut short_then_valid_ref = reference(0xB04, 0x100, 0xA04, [0.0; 3], [0.0; 3]);
        short_then_valid_ref
            .subrecords
            .push((b"XRDS".to_vec(), vec![0xAE, 0x18]));
        short_then_valid_ref
            .subrecords
            .push((b"XRDS".to_vec(), 32.0f32.to_le_bytes().to_vec()));
        master.insert(0xB04, short_then_valid_ref);
        let mut valid_then_short_ref = reference(0xB05, 0x100, 0xA05, [0.0; 3], [0.0; 3]);
        valid_then_short_ref
            .subrecords
            .push((b"XRDS".to_vec(), 48.0f32.to_le_bytes().to_vec()));
        valid_then_short_ref
            .subrecords
            .push((b"XRDS".to_vec(), vec![0xAE, 0x18]));
        master.insert(0xB05, valid_then_short_ref);

        let mut rejected = RejectedLightFields::new();
        rejected.insert(
            0xA01,
            RejectedLightInputs {
                fnam: true,
                xrds: false,
            },
        );
        rejected.insert(
            0xB01,
            RejectedLightInputs {
                fnam: false,
                xrds: true,
            },
        );
        rejected.insert(
            0xA03,
            RejectedLightInputs {
                fnam: true,
                xrds: false,
            },
        );
        rejected.insert(
            0xB03,
            RejectedLightInputs {
                fnam: false,
                xrds: true,
            },
        );
        export_records(
            &conn,
            &master,
            None,
            None,
            None,
            None,
            ExportOptions {
                runtime_views: true,
                scalar_scripts: false,
                rejected_light_fields: Some(&rejected),
                trusted_portal_nulls: true,
                deleted_ids: None,
                room_link_sources: None,
                subset_sources: None,
                damaged_placement_cells: None,
            },
        )
        .unwrap();

        let fade = |id: u32| -> (Option<f64>, i64) {
            conn.query_row(
                "SELECT fade, fade_status FROM lights WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        let radius = |id: u32| -> (Option<f64>, i64) {
            conn.query_row(
                "SELECT radius_override, radius_status FROM \"references\" WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(fade(0xA00), (None, 2), "legacy short FNAM");
        assert_eq!(fade(0xA01), (None, 2), "rejected FNAM");
        assert_eq!(fade(0xA02), (None, 0), "absent FNAM");
        assert_eq!(fade(0xA03), (Some(1.5), 1), "valid FNAM beats the flag");
        assert_eq!(radius(0xB00), (None, 2), "legacy short XRDS");
        assert_eq!(radius(0xB01), (None, 2), "rejected XRDS");
        assert_eq!(radius(0xB02), (None, 0), "absent XRDS");
        assert_eq!(radius(0xB03), (Some(64.0), 1), "valid XRDS beats the flag");
        assert_eq!(fade(0xA04), (None, 2), "a short FNAM before a valid one");
        assert_eq!(
            fade(0xA05),
            (Some(3.5), 1),
            "a short FNAM after a valid one"
        );
        assert_eq!(
            radius(0xB04),
            (Some(32.0), 1),
            "a short XRDS before a valid one"
        );
        assert_eq!(
            radius(0xB05),
            (Some(48.0), 1),
            "a short XRDS after a valid one"
        );
    }

    fn omission_ids(conn: &Connection) -> Vec<(String, u64, Vec<u32>)> {
        ProjectionOmissions::read(conn)
            .unwrap()
            .categories
            .into_iter()
            .map(|(category, outcome)| {
                let ids = outcome
                    .samples
                    .iter()
                    .map(|sample| sample.form_id)
                    .collect();
                (category, outcome.count, ids)
            })
            .collect()
    }

    /// Every light and emittance branch that writes no row is counted under its own category,
    /// next to healthy rows that are still written (#297).
    #[test]
    fn records_light_and_emittance_omissions_beside_healthy_rows() {
        use crate::esm::projection_omissions::Severity;
        let order = LoadOrder::from_metadata(
            vec!["Lights.esp".into()],
            vec![crate::esm::binary::PluginMetadata {
                masters: Vec::new(),
                flags: 0,
            }],
        )
        .unwrap();
        let mut short = light_data_bytes(256, [1, 2, 3], 0, 1.0);
        short.truncate(12);
        let healthy = light(
            0x700,
            "Healthy",
            light_data_bytes(256, [255, 200, 120], 0, 1.0),
            None,
            None,
        );
        let broken = [
            record(
                0x801,
                b"LIGH",
                None,
                None,
                vec![(b"EDID".to_vec(), cstr("NoData"))],
            ),
            light(0x802, "Short", short, None, None),
        ];
        let mut records = vec![
            healthy.clone(),
            emitting_reference(0x1000, 0x700),
            emitting_reference(0x1001, 0xDEAD),
            emitting_reference(0x1002, 0x900),
            emitting_reference(0x1003, 0x600),
            emitting_reference(0x1004, 0x610),
            emitting_reference(0x1005, 0x802),
            record(0x900, b"STAT", None, None, Vec::new()),
            record(0x600, b"REGN", None, None, Vec::new()),
            region(0x610, &[(0x611, 100)]),
            record(0x611, b"WTHR", None, None, Vec::new()),
        ];
        records.extend(broken.iter().cloned());
        let master: HashMap<u32, RawRecord> = records
            .into_iter()
            .map(|record| (record.form_id, record))
            .collect();
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();

        export_to_db_with_load_order(&conn, &master, &order).unwrap();

        let lights: Vec<u32> = conn
            .prepare("SELECT id FROM lights ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(lights, vec![0x700], "the healthy light keeps its row");
        assert_eq!(
            emittance_row(&conn, 0x700),
            Some(("LIGH".into(), None, [[255, 200, 120]; 4]))
        );
        let expected = vec![
            ("emittance_light_unusable_data".to_owned(), 1, vec![0x802]),
            ("emittance_region_no_weather".to_owned(), 1, vec![0x600]),
            ("emittance_target_absent".to_owned(), 1, vec![0xDEAD]),
            ("emittance_target_wrong_type".to_owned(), 1, vec![0x900]),
            ("emittance_weather_no_lighting".to_owned(), 1, vec![0x610]),
            ("light_missing_data".to_owned(), 1, vec![0x801]),
            ("light_short_data".to_owned(), 1, vec![0x802]),
        ];
        assert_eq!(omission_ids(&conn), expected);
        let stored = ProjectionOmissions::read(&conn).unwrap();
        for (category, outcome) in &stored.categories {
            let data_loss = category.starts_with("light_");
            assert_eq!(
                outcome.severity == Severity::DataLoss,
                data_loss,
                "{category}"
            );
        }
        assert!(
            stored.categories["light_short_data"].samples[0]
                .reason
                .contains("12 bytes"),
            "{stored:?}"
        );

        // Exporting the same load order again replaces the counts instead of adding to them.
        export_to_db_with_load_order(&conn, &master, &order).unwrap();
        assert_eq!(omission_ids(&conn), expected);

        // A subset export does not own the snapshot, so it keeps the stored omissions.
        export_to_db(&conn, &HashMap::new()).unwrap();
        assert_eq!(omission_ids(&conn), expected);

        // Once the data is repaired, a full export leaves nothing behind.
        let repaired: HashMap<u32, RawRecord> = [healthy, emitting_reference(0x1000, 0x700)]
            .into_iter()
            .map(|record| (record.form_id, record))
            .collect();
        export_to_db_with_load_order(&conn, &repaired, &order).unwrap();
        assert!(omission_ids(&conn).is_empty());
    }

    /// A RACE whose authoring cannot be read keeps no animation row, as before, but it is
    /// named in the result instead of vanishing. A deleted RACE is not read at all, and the
    /// readable race beside it is unchanged.
    #[test]
    fn race_authoring_scan_names_each_unreadable_race() {
        let readable = record(
            0x30,
            b"RACE",
            None,
            None,
            vec![
                (b"MNAM".to_vec(), Vec::new()),
                (b"ANAM".to_vec(), cstr("rig/male.nif")),
            ],
        );
        // `ANAM` before any `MNAM` or `FNAM` has no sex selector.
        let no_selector = record(
            0x20,
            b"RACE",
            None,
            None,
            vec![(b"ANAM".to_vec(), cstr("rig/male.nif"))],
        );
        let bad_utf8 = record(
            0x10,
            b"RACE",
            None,
            None,
            vec![
                (b"MNAM".to_vec(), Vec::new()),
                (b"ANAM".to_vec(), vec![0xFF, 0xFE, 0]),
            ],
        );
        let mut deleted = no_selector.clone();
        deleted.form_id = 0x40;
        deleted.flags = 0x20;
        let master: HashMap<u32, RawRecord> = [readable, no_selector, bad_utf8, deleted]
            .into_iter()
            .map(|record| (record.form_id, record))
            .collect();

        let (catalog, rows) =
            crate::actor_models::resolve_actor_models_with_race_authoring(&master, &HashMap::new());

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[&0x30].male_skeleton.as_deref(),
            Some("meshes/rig/male.nif")
        );
        let unreadable = catalog.race_authoring_errors;
        let ids: Vec<u32> = unreadable.keys().copied().collect();
        assert_eq!(ids, vec![0x10, 0x20], "FormID order, deleted RACE skipped");
        assert!(unreadable[&0x10].contains("utf-8"), "{unreadable:?}");
        assert!(
            unreadable[&0x20].contains("no sex selector"),
            "{unreadable:?}"
        );
    }

    /// A reference lit by `source` through `XEMI`.
    fn emitting_reference(form_id: u32, source: u32) -> RawRecord {
        let mut reference = reference(form_id, 0x100, 0x900, [0.0; 3], [0.0; 3]);
        reference
            .subrecords
            .push((b"XEMI".to_vec(), source.to_le_bytes().to_vec()));
        reference
    }

    /// A `WTHR` whose 14 `NAM0` colour types are black except Effect Lighting.
    fn weather(form_id: u32, effect_lighting: [[u8; 3]; 4]) -> RawRecord {
        let mut nam0 = vec![0u8; 14 * 16];
        for (time, rgb) in effect_lighting.iter().enumerate() {
            nam0[9 * 16 + time * 4..9 * 16 + time * 4 + 3].copy_from_slice(rgb);
        }
        record(form_id, b"WTHR", None, None, vec![(b"NAM0".to_vec(), nam0)])
    }

    /// A `REGN` listing `(weather, chance)` pairs, without globals.
    fn region(form_id: u32, weathers: &[(u32, u32)]) -> RawRecord {
        let rdwt = weathers
            .iter()
            .flat_map(|(weather, chance)| {
                [weather.to_le_bytes(), chance.to_le_bytes(), [0; 4]].concat()
            })
            .collect();
        record(form_id, b"REGN", None, None, vec![(b"RDWT".to_vec(), rdwt)])
    }

    type EmittanceRow = (String, Option<u32>, [[u8; 3]; 4]);

    fn emittance_row(conn: &Connection, id: u32) -> Option<EmittanceRow> {
        conn.query_row(
            "SELECT source_type, weather_id, sunrise_r, sunrise_g, sunrise_b, day_r, day_g, day_b,
                    sunset_r, sunset_g, sunset_b, night_r, night_g, night_b
             FROM emittance WHERE id=?1",
            [id],
            |row| {
                let colour = |time: usize| -> Result<[u8; 3]> {
                    Ok([
                        row.get(2 + time * 3)?,
                        row.get(3 + time * 3)?,
                        row.get(4 + time * 3)?,
                    ])
                };
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    [colour(0)?, colour(1)?, colour(2)?, colour(3)?],
                ))
            },
        )
        .optional()
        .unwrap()
    }

    /// Writes the `emittance` rows the way the load-order export does after its records.
    fn export_with_emittance(conn: &Connection, master: &HashMap<u32, RawRecord>) {
        export_to_db(conn, master).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        insert_emittance_sources(&tx, master, &mut ProjectionOmissions::default()).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn resolves_reference_emittance_from_regions_and_lights() {
        const INVERT_WINDOW: [[u8; 3]; 4] =
            [[124, 70, 16], [0, 0, 0], [243, 118, 54], [242, 176, 55]];
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let mut master = HashMap::new();
        for record in [
            emitting_reference(0x1000, 0x8277B),
            emitting_reference(0x1001, 0x8277B),
            emitting_reference(0x2000, 0x700),
            emitting_reference(0x3000, 0x600),
            emitting_reference(0x4000, 0x900),
            emitting_reference(0x5000, 0xDEAD),
            emitting_reference(0x6000, 0),
            reference(0x7000, 0x100, 0x900, [0.0; 3], [0.0; 3]),
            // The likeliest weather wins.
            region(0x8277B, &[(0x500, 10), (0x8277A, 90)]),
            weather(0x500, [[255; 3]; 4]),
            weather(0x8277A, INVERT_WINDOW),
            light(
                0x700,
                "WindowFill",
                light_data_bytes(256, [255, 200, 120], 0, 1.0),
                None,
                None,
            ),
            // A region whose only weather is missing resolves to nothing.
            region(0x600, &[(0x601, 100)]),
            // `XEMI` names a light or a region; a static has no colour to lend.
            record(0x900, b"STAT", None, None, Vec::new()),
        ] {
            master.insert(record.form_id, record);
        }

        export_with_emittance(&conn, &master);

        let emittance_id = |id: u32| -> Option<u32> {
            conn.query_row(
                "SELECT emittance_id FROM \"references\" WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(emittance_id(0x1000), Some(0x8277B));
        assert_eq!(emittance_id(0x6000), None, "a cleared XEMI stores NULL");
        assert_eq!(emittance_id(0x7000), None, "no XEMI stores NULL");

        assert_eq!(
            emittance_row(&conn, 0x8277B),
            Some(("REGN".into(), Some(0x8277A), INVERT_WINDOW))
        );
        assert_eq!(
            emittance_row(&conn, 0x700),
            Some(("LIGH".into(), None, [[255, 200, 120]; 4]))
        );
        for unresolved in [0x600, 0x900, 0xDEAD] {
            assert_eq!(emittance_row(&conn, unresolved), None, "{unresolved:08X}");
        }
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM emittance", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2);
    }

    /// A second export into the same database drops the colour of a target that no longer
    /// resolves, instead of keeping the first export's row.
    #[test]
    fn re_exporting_drops_emittance_that_no_longer_resolves() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let mut master = HashMap::new();
        for record in [
            emitting_reference(0x1000, 0x8277B),
            region(0x8277B, &[(0x8277A, 100)]),
            weather(0x8277A, [[0; 3]; 4]),
        ] {
            master.insert(record.form_id, record);
        }
        export_with_emittance(&conn, &master);
        assert!(emittance_row(&conn, 0x8277B).is_some());

        master.remove(&0x8277A);
        export_with_emittance(&conn, &master);
        assert_eq!(emittance_row(&conn, 0x8277B), None);
    }

    /// A `references` table from before the emittance export has no `emittance_id`; `create_tables`
    /// adds it, so a complete export into a reused database still inserts its references.
    #[test]
    fn create_tables_adds_emittance_id_to_an_older_references_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"CREATE TABLE "references" (
                id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL, worldspace_id INTEGER,
                base_form_id INTEGER NOT NULL, is_exterior INTEGER NOT NULL,
                pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
                local_x REAL, local_y REAL, rot_x REAL NOT NULL, rot_y REAL NOT NULL,
                rot_z REAL NOT NULL, scale REAL NOT NULL DEFAULT 1.0,
                radius_override REAL, header_flags INTEGER NOT NULL DEFAULT 0,
                enable_parent_id INTEGER, enable_parent_flags INTEGER, data BLOB
            );"#,
        )
        .unwrap();
        create_tables(&conn).unwrap();
        // A second call finds the column and leaves the table alone.
        create_tables(&conn).unwrap();
        let mut master = HashMap::new();
        for record in [
            emitting_reference(0x1000, 0x8277B),
            region(0x8277B, &[(0x8277A, 100)]),
            weather(0x8277A, [[0; 3]; 4]),
        ] {
            master.insert(record.form_id, record);
        }
        export_with_emittance(&conn, &master);
        let emittance_id: Option<u32> = conn
            .query_row(
                "SELECT emittance_id FROM \"references\" WHERE id=0x1000",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(emittance_id, Some(0x8277B));
        assert!(emittance_row(&conn, 0x8277B).is_some());
    }

    /// Update.esm and every DLC override `DOBJ` `00000031` with a partial key list. Merged
    /// key by key, Skyrim.esm's default walk and run movement types survive as
    /// `NPC_Default_MT`, and every retail `MOVT` exports, zero strafe speeds included.
    ///
    /// Reads the game install, so it is opt-in:
    /// `cargo test -p converter --lib -- --ignored`.
    #[test]
    #[ignore = "requires MUDCRAB_SKYRIM_DATA with locally installed game assets"]
    fn default_movement_types_of_the_real_masters_survive_overrides() {
        let data_dir = std::env::var_os("MUDCRAB_SKYRIM_DATA")
            .map(std::path::PathBuf::from)
            .expect("set MUDCRAB_SKYRIM_DATA to the Skyrim Data directory");
        let plugins: Vec<_> = [
            "Skyrim.esm",
            "Update.esm",
            "Dawnguard.esm",
            "HearthFires.esm",
            "Dragonborn.esm",
        ]
        .iter()
        .map(|name| data_dir.join(name))
        .collect();
        let merged = crate::esm::EsmParser::merge_plugins(&plugins).unwrap();
        let selected: HashMap<u32, RawRecord> = merged
            .into_iter()
            .filter(|(_, record)| matches!(&record.record_type, b"DOBJ" | b"MOVT"))
            .collect();
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        export_to_db(&conn, &selected).unwrap();
        for key in ["DMWL", "DMRN"] {
            let (form_id, editor_id): (u32, String) = conn
                .query_row(
                    "SELECT d.form_id, m.editor_id FROM default_objects d \
                     JOIN movement_types m ON m.id = d.form_id WHERE d.key = ?1",
                    [key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                (form_id, editor_id.as_str()),
                (0x0003_580D, "NPC_Default_MT")
            );
        }
        let movement_types: i64 = conn
            .query_row("SELECT count(*) FROM movement_types", [], |row| row.get(0))
            .unwrap();
        assert!(movement_types >= 107, "only {movement_types} MOVT exported");
    }

    /// The Sleeping Giant Inn's reference in Riverwood names `FXLightRegionInvertWindowWhiterun`,
    /// whose weather `FXWthrInvertWindowsWhiterun` gives its windows no colour by day (#242).
    ///
    /// Reads the game install, so it is opt-in:
    /// `cargo test -p converter --lib -- --ignored`.
    #[test]
    #[ignore = "requires MUDCRAB_SKYRIM_DATA with locally installed game assets"]
    fn the_sleeping_giant_inn_windows_are_dark_by_day() {
        const INN: u32 = 0x2D763;
        const REGION: u32 = 0x8277B;
        const WEATHER: u32 = 0x8277A;
        let data_dir = std::env::var_os("MUDCRAB_SKYRIM_DATA")
            .or_else(|| std::env::var_os("OPENSKYRIM_SKYRIM_DATA"))
            .map(std::path::PathBuf::from)
            .expect("set MUDCRAB_SKYRIM_DATA to the Skyrim Data directory");
        let records = crate::esm::binary::parse_plugin_file(&data_dir.join("Skyrim.esm")).unwrap();
        let master: HashMap<u32, RawRecord> = records
            .into_iter()
            .filter(|record| [INN, REGION, WEATHER].contains(&record.form_id))
            .map(|record| (record.form_id, record))
            .collect();
        assert_eq!(master.len(), 3, "the inn, its region and its weather");

        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        export_with_emittance(&conn, &master);

        let source: Option<u32> = conn
            .query_row(
                "SELECT emittance_id FROM \"references\" WHERE id=?1",
                [INN],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source, Some(REGION));
        assert_eq!(
            emittance_row(&conn, REGION),
            Some((
                "REGN".into(),
                Some(WEATHER),
                [[124, 70, 16], [0, 0, 0], [243, 118, 54], [242, 176, 55]]
            ))
        );
    }

    /// The real `LIGH` records and their references, read through the same
    /// `export_to_db` path the converter uses. Two things were measured here
    /// before the table was written, and both are asserted so a change in the
    /// game data or in this code shows up as a failure rather than as wrong
    /// light in the engine:
    ///
    /// - `DATA` is 48 bytes in every `LIGH` record of Skyrim.esm, which is the
    ///   size of the layout UESP documents.
    /// - `XRDS`, the reference's own radius, rides on almost every light
    ///   reference (10,810 of 12,148 in Skyrim.esm), which is why the
    ///   `references` table has a `radius_override` column at all.
    ///
    /// The expected torch values are the bytes `DefaultTorch01NS_FastSaturate`
    /// (000CB3B0) holds: `FF FF FF FF` time -1, `00 01 00 00` radius 256,
    /// `E9 9E 4B 00` colour (233, 158, 75), `09 20 00 00` flags 0x2009,
    /// `00 00 80 3F` falloff 1.0, and `FNAM` 1.0.
    ///
    /// Reads the game install, so it is opt-in:
    /// `cargo test -p converter --lib -- --ignored`.
    #[test]
    #[ignore = "requires MUDCRAB_SKYRIM_DATA with locally installed game assets"]
    fn lights_of_the_real_plugin_decode_through_export() {
        use std::collections::HashSet;
        let data_dir = std::env::var_os("MUDCRAB_SKYRIM_DATA")
            .or_else(|| std::env::var_os("OPENSKYRIM_SKYRIM_DATA"))
            .map(std::path::PathBuf::from)
            .expect("set MUDCRAB_SKYRIM_DATA to the Skyrim Data directory");
        let plugin = data_dir.join("Skyrim.esm");
        assert!(plugin.is_file(), "no Skyrim.esm at {}", plugin.display());
        let records = crate::esm::binary::parse_plugin_file(&plugin).unwrap();

        let mut light_count = 0usize;
        for record in records.iter().filter(|r| r.record_type == *b"LIGH") {
            light_count += 1;
            let data_length = record
                .subrecords
                .iter()
                .find(|(tag, _)| tag.as_slice() == b"DATA")
                .map(|(_, data)| data.len());
            assert_eq!(
                data_length,
                Some(48),
                "LIGH {:08X} DATA is {data_length:?} bytes, expected 48",
                record.form_id
            );
        }
        eprintln!("LIGH records with a 48-byte DATA: {light_count}");
        assert!(light_count > 400, "only {light_count} LIGH records found");

        let light_ids: HashSet<u32> = records
            .iter()
            .filter(|record| record.record_type == *b"LIGH")
            .map(|record| record.form_id)
            .collect();
        let (mut all_refs, mut all_overrides) = (0usize, 0usize);
        for record in records.iter().filter(|r| r.record_type == *b"REFR") {
            let base = record
                .subrecords
                .iter()
                .find(|(tag, _)| tag.as_slice() == b"NAME")
                .filter(|(_, data)| data.len() >= 4)
                .map(|(_, data)| u32::from_le_bytes([data[0], data[1], data[2], data[3]]));
            if !base.is_some_and(|base| light_ids.contains(&base)) {
                continue;
            }
            let has_override = record
                .subrecords
                .iter()
                .any(|(tag, data)| tag.as_slice() == b"XRDS" && data.len() >= 4);
            all_refs += 1;
            all_overrides += usize::from(has_override);
        }
        eprintln!("LIGH references with an XRDS radius override: {all_overrides}/{all_refs}");
        assert!(
            all_overrides * 10 > all_refs * 8,
            "an override is common in the plugin itself"
        );

        const TORCH: u32 = 0xCB3B0;
        // An ice candle's reference, its `IceCandleLight01` base, and the cell it stands in.
        const CANDLE_REF: u32 = 0x56D56;
        const CANDLE_LIGHT: u32 = 0x194F4;
        let wanted: HashSet<u32> = [TORCH, CANDLE_REF, CANDLE_LIGHT, 0x152C3]
            .into_iter()
            .collect();
        let master: HashMap<u32, RawRecord> = records
            .into_iter()
            .filter(|record| wanted.contains(&record.form_id))
            .map(|record| (record.form_id, record))
            .collect();
        for form_id in &wanted {
            assert!(
                master.contains_key(form_id),
                "{form_id:08X} is not in Skyrim.esm"
            );
        }

        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        export_to_db(&conn, &master).unwrap();

        type TorchRow = (f64, i64, i64, i64, i64, f64, f64);
        let torch: TorchRow = conn
            .query_row(
                "SELECT radius,color_r,color_g,color_b,flags,falloff,fade FROM lights WHERE id=?1",
                [TORCH],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            torch,
            (256.0, 233, 158, 75, 0x2009, 1.0, 1.0),
            "DefaultTorch01NS_FastSaturate"
        );
        // A torch is warm: reading the colour at the wrong offset would not be.
        assert!(
            torch.1 > torch.2 && torch.2 > torch.3,
            "torch colour is not warm: ({}, {}, {})",
            torch.1,
            torch.2,
            torch.3
        );

        // The reference's override is its own, not its base light's: 544.4
        // units against the 256 its `IceCandleLight01` base asks for.
        let override_radius: Option<f64> = conn
            .query_row(
                "SELECT radius_override FROM \"references\" WHERE id=?1",
                [CANDLE_REF],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(override_radius, Some(f64::from(544.3856f32)));
        let base_radius: f64 = conn
            .query_row(
                "SELECT radius FROM lights WHERE id=?1",
                [CANDLE_LIGHT],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(base_radius, 256.0);
    }

    // ---- Interior room graph (db-schema.md section 15) ----

    use crate::esm::rooms::{self, PORTAL_MARKER_BASE, ROOM_MARKER_BASE};

    fn xprm(half: [f32; 3], shape: u32) -> Vec<u8> {
        let mut bytes = floats(half);
        bytes.extend(floats([0.5, 0.5, 0.5]));
        bytes.extend(0.2f32.to_le_bytes());
        bytes.extend(shape.to_le_bytes());
        bytes
    }

    fn word(value: u32) -> Vec<u8> {
        value.to_le_bytes().to_vec()
    }

    /// A marker reference: `NAME`, `DATA` and any extra subrecords.
    fn marker(
        form_id: u32,
        cell: u32,
        base: u32,
        position: [f32; 3],
        rotation: [f32; 3],
        extra: Vec<(&[u8; 4], Vec<u8>)>,
    ) -> RawRecord {
        let mut record = reference(form_id, cell, base, position, rotation);
        record
            .subrecords
            .extend(extra.into_iter().map(|(tag, data)| (tag.to_vec(), data)));
        record
    }

    fn room(
        form_id: u32,
        cell: u32,
        position: [f32; 3],
        rotation: [f32; 3],
        half: [f32; 3],
        mut extra: Vec<(&[u8; 4], Vec<u8>)>,
    ) -> RawRecord {
        extra.insert(0, (b"XPRM", xprm(half, 1)));
        marker(form_id, cell, ROOM_MARKER_BASE, position, rotation, extra)
    }

    fn portal(form_id: u32, cell: u32, sides: Option<(u32, u32)>) -> RawRecord {
        let mut extra = vec![(b"XPRM", xprm([40.0, 1.0, 60.0], 3))];
        if let Some((a, b)) = sides {
            extra.push((b"XPOD", [word(a), word(b)].concat()));
        }
        marker(form_id, cell, PORTAL_MARKER_BASE, [0.0; 3], [0.0; 3], extra)
    }

    fn interior_cell(form_id: u32) -> RawRecord {
        record(form_id, b"CELL", None, None, Vec::new())
    }

    /// Full runtime exports own the room graph; diagnostic archives defer it and subsets retain it.
    #[test]
    fn archive_exports_clear_room_graph_and_runtime_exports_restore_it() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("Rooms.esp");
        std::fs::write(&plugin, b"synthetic room provenance").unwrap();
        let paths = [plugin];
        let order = LoadOrder::from_metadata(
            vec!["Rooms.esp".into()],
            vec![crate::esm::binary::PluginMetadata {
                masters: Vec::new(),
                flags: 0,
            }],
        )
        .unwrap();
        let mut occluder = portal(0x3002, 0x100, None);
        occluder
            .subrecords
            .iter_mut()
            .find(|(tag, _)| tag.as_slice() == b"NAME")
            .unwrap()
            .1 = rooms::OCCLUSION_MARKER_BASE.to_le_bytes().to_vec();
        occluder.subrecords.push((
            b"XORD".to_vec(),
            [0x3003u32, 0, 0, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
        ));
        let records = vec![
            interior_cell(0x100),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [100.0; 3], vec![]),
            portal(0x3001, 0x100, Some((0x1001, 0))),
            marker(
                0x2001,
                0x100,
                0x500,
                [20.0, 0.0, 0.0],
                [0.0; 3],
                vec![(b"XLKR", word(0x1001))],
            ),
            occluder,
        ];
        let master: HashMap<u32, RawRecord> = records
            .into_iter()
            .map(|record| (record.form_id, record))
            .collect();
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        export_inhouse_to_db(
            &conn,
            &master,
            &order,
            &paths,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &ReadResult::default(),
        )
        .unwrap();
        let expected = rooms::RoomGraphReport::read(&conn).unwrap();
        assert_eq!((expected.rooms, expected.portals), (1, 1));
        assert_eq!(expected.occluders, 1);
        assert_eq!(rooms_of(&conn, 0x2001), vec![(0x1001, 1)]);
        assert_eq!(
            conn.query_row(
                "SELECT target_id FROM reference_authored_targets WHERE reference_id=0x2001",
                [],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            0x1001
        );
        assert!(
            !conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM room_graph_stats WHERE name='occlusion_version')",
                    [],
                    |row| row.get::<_, bool>(0)
                )
                .unwrap()
        );

        conn.execute("INSERT INTO room_model_bounds VALUES(1280,1,2,3,4)", [])
            .unwrap();
        export_to_db(&conn, &HashMap::new()).unwrap();
        assert_eq!(rooms::RoomGraphReport::read(&conn).unwrap(), expected);
        assert_eq!(rooms_of(&conn, 0x2001), vec![(0x1001, 1)]);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM room_occluders", [], |row| row
                .get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM room_occluder_links", [], |row| row
                .get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM room_model_bounds", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            1
        );

        export_inhouse_record_archive_to_db(&conn, &master, &order, &paths, &HashSet::new())
            .unwrap();
        for table in [
            "room_bounds",
            "room_portals",
            "room_occluders",
            "room_occluder_links",
            "room_links",
            "room_graph_cells",
            "reference_rooms",
            "room_graph_stats",
            "room_reference_rules",
            "room_graph_cells",
            "room_model_bounds",
            "projection_omission_stats",
            "projection_omission_samples",
            "cells",
            "references",
            "reference_authored_targets",
            "reference_authored_changes",
        ] {
            let count: u64 = conn
                .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "deferred runtime table {table}");
        }
        let canonical_rows: u64 = conn
            .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(canonical_rows, 5);
        export_inhouse_to_db(
            &conn,
            &master,
            &order,
            &paths,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &ReadResult::default(),
        )
        .unwrap();
        assert_eq!(rooms::RoomGraphReport::read(&conn).unwrap(), expected);
        assert_eq!(rooms_of(&conn, 0x2001), vec![(0x1001, 1)]);
        let outside: bool = conn
            .query_row(
                "SELECT unbounded_b FROM room_portals WHERE ref_id=0x3001",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !outside,
            "canonical nulls stay unknown until their source is verified"
        );
        assert!(
            !conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM room_graph_stats WHERE name='visibility_version')",
                    [],
                    |row| row.get::<_, bool>(0)
                )
                .unwrap()
        );
    }

    #[test]
    fn sourced_subset_deletion_checks_owning_identity_and_type_on_retries() {
        use crate::esm::binary::PluginMetadata;

        let db = Connection::open_in_memory().unwrap();
        let id = 0x0100_0800;
        let order = |owner: &str| {
            LoadOrder::from_metadata(
                vec!["Skyrim.esm".into(), owner.into()],
                vec![PluginMetadata::default(), PluginMetadata::default()],
            )
            .unwrap()
        };
        let matching = order("First.esp");
        let foreign = order("Second.esp");
        export_to_db_with_load_order(
            &db,
            &[(id, reference(id, 0x100, 0x500, [0.0; 3], [0.0; 3]))].into(),
            &matching,
        )
        .unwrap();
        db.execute("INSERT INTO scripts VALUES (?1,'Test',X'','[]')", [id])
            .unwrap();
        let delete = |order| export_placed_subset_with_sources(&db, &HashMap::new(), &[id], order);
        let counts = || {
            db.query_row(
                "SELECT (SELECT count(*) FROM [references] WHERE id=?1),
                        (SELECT count(*) FROM scripts WHERE form_id=?1),
                        (SELECT count(*) FROM records WHERE form_id=?1)",
                [id],
                |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, u32>(2)?,
                    ))
                },
            )
            .unwrap()
        };
        assert!(
            delete(&foreign)
                .unwrap_err()
                .to_string()
                .contains("identity")
        );
        assert_eq!(counts(), (1, 1, 1));
        db.execute(
            "UPDATE formid_map SET internal_id=0x801 WHERE form_id=?1",
            [id],
        )
        .unwrap();
        assert!(
            delete(&matching)
                .unwrap_err()
                .to_string()
                .contains("identity")
        );
        assert_eq!(counts(), (1, 1, 1));
        db.execute(
            "UPDATE formid_map SET internal_id=0x800 WHERE form_id=?1",
            [id],
        )
        .unwrap();
        db.execute(
            "UPDATE records SET record_type='ACHR' WHERE form_id=?1",
            [id],
        )
        .unwrap();
        assert!(
            delete(&matching)
                .unwrap_err()
                .to_string()
                .contains("record type")
        );
        assert_eq!(counts(), (1, 1, 1));
        db.execute(
            "UPDATE records SET record_type='REFR' WHERE form_id=?1",
            [id],
        )
        .unwrap();

        delete(&matching).unwrap();
        assert_eq!(counts(), (0, 0, 0));
        delete(&matching).unwrap();
        assert!(
            delete(&foreign)
                .unwrap_err()
                .to_string()
                .contains("identity")
        );
        db.execute(
            "UPDATE formid_map SET record_type='ACHR' WHERE form_id=?1",
            [id],
        )
        .unwrap();
        assert!(
            delete(&matching)
                .unwrap_err()
                .to_string()
                .contains("record type")
        );
        assert_eq!(counts(), (0, 0, 0));
    }

    #[test]
    fn subset_reference_links_use_pack_targets_and_the_current_edit_set() {
        let db = Connection::open_in_memory().unwrap();
        create_tables(&db).unwrap();
        let placed = |id| reference(id, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        let target = 0x810;
        let keyword = 0x811;
        let deleted_target = 0x700;
        let mapped_deleted_target = 0x701;
        db.execute(
            "INSERT INTO formid_map VALUES (?1,'skyrim.esm',?1,'REFR')",
            [mapped_deleted_target],
        )
        .unwrap();
        let new_target = 0x910;
        let edited = 0x900;
        let unusable = 0x911;
        export_to_db(
            &db,
            &[
                (target, placed(target)),
                (keyword, record(keyword, b"KYWD", None, None, Vec::new())),
                (deleted_target, placed(deleted_target)),
                (mapped_deleted_target, placed(mapped_deleted_target)),
                (unusable, placed(unusable)),
            ]
            .into(),
        )
        .unwrap();

        let mut source = placed(edited);
        for id in [
            target,
            deleted_target,
            mapped_deleted_target,
            new_target,
            unusable,
            keyword,
            0x999,
            0x14,
            0,
        ] {
            source.subrecords.push((
                b"XAPR".to_vec(),
                [id.to_le_bytes(), 1.0f32.to_le_bytes()].concat(),
            ));
            source
                .subrecords
                .push((b"XLKR".to_vec(), [word(keyword), word(id)].concat()));
        }
        let deleted = |id| {
            let mut record = record(id, b"REFR", None, None, Vec::new());
            record.flags = 0x20;
            (id, record)
        };
        let expected = [
            Some(target),
            None,
            None,
            Some(new_target),
            None,
            None,
            None,
            Some(0x14),
            None,
        ];
        let mut mismatches = Vec::new();
        let restore_targets = || {
            [
                (deleted_target, placed(deleted_target)),
                (mapped_deleted_target, placed(mapped_deleted_target)),
            ]
            .into()
        };
        for (label, edits, restored, control) in [
            (
                "source and targets edited together",
                [
                    (edited, source.clone()),
                    deleted(deleted_target),
                    deleted(mapped_deleted_target),
                    (new_target, placed(new_target)),
                    (unusable, record(unusable, b"REFR", None, None, Vec::new())),
                ]
                .into(),
                false,
                true,
            ),
            (
                "source-only while targets absent",
                [(edited, source.clone())].into(),
                false,
                true,
            ),
            ("target-only restoration", restore_targets(), true, false),
            (
                "source-only with targets present",
                [(edited, source)].into(),
                true,
                true,
            ),
            (
                "target-only deletion",
                [deleted(deleted_target), deleted(mapped_deleted_target)].into(),
                false,
                false,
            ),
        ] {
            export_to_db(&db, &edits).unwrap();
            let mut expected = expected;
            if restored {
                expected[1] = Some(deleted_target);
                expected[2] = Some(mapped_deleted_target);
            }
            let parents = db
                .prepare(
                    "SELECT parent_ref_id, delay FROM reference_activate_parents
                          WHERE reference_id=?1 ORDER BY position",
                )
                .unwrap()
                .query_map([edited], |row| {
                    Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, f32>(1)?))
                })
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            let linked = db
                .prepare(
                    "SELECT keyword_id, linked_ref_id FROM reference_linked_refs
                          WHERE reference_id=?1 ORDER BY position",
                )
                .unwrap()
                .query_map([edited], |row| {
                    Ok((row.get::<_, u32>(0)?, row.get::<_, Option<u32>>(1)?))
                })
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            let expected_parents = expected.map(|id| (id, 1.0));
            let expected_linked = expected.map(|id| (keyword, id));
            if control {
                assert_eq!(parents, expected_parents, "{label}");
                assert_eq!(linked, expected_linked, "{label}");
            } else if parents != expected_parents || linked != expected_linked {
                mismatches.push(format!(
                    "{label}: XAPR {parents:?}, XLKR {linked:?}, expected targets {expected:?}"
                ));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn subset_incoming_links_use_effective_source_positions_and_roll_back() {
        let db = Connection::open_in_memory().unwrap();
        create_tables(&db).unwrap();
        let placed = |id| reference(id, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        let target = 0x810;
        let source = 0x900;
        let other = 0x811;
        let key = 0x812;
        let order = LoadOrder::from_metadata(
            vec!["Skyrim.esm".into()],
            vec![crate::esm::binary::PluginMetadata::default()],
        )
        .unwrap();
        let mut initial = placed(source);
        initial.subrecords.extend([
            (b"XLKR".to_vec(), word(other)),
            (b"XLKR".to_vec(), vec![0; 6]),
            (b"XLKR".to_vec(), [word(target), word(other)].concat()),
            (b"XAPR".to_vec(), vec![0; 12]),
            (
                b"XAPR".to_vec(),
                [word(target), 2.0f32.to_le_bytes().to_vec()].concat(),
            ),
            (b"XAPD".to_vec(), vec![3]),
            (
                b"XLOC".to_vec(),
                [vec![50, 0, 0, 0], word(key), vec![4, 0, 0, 0]].concat(),
            ),
        ]);
        export_to_db_with_load_order(
            &db,
            &[
                (source, initial.clone()),
                (other, placed(other)),
                (key, record(key, b"KEYM", None, None, Vec::new())),
            ]
            .into(),
            &order,
        )
        .unwrap();
        let links = || {
            db.query_row(
                "SELECT x0.linked_ref_id,x1.keyword_id,x1.linked_ref_id,p.parent_ref_id,p.delay,d.activate_parent_flags,d.lock_key_id
                 FROM reference_linked_refs x0
                 JOIN reference_linked_refs x1 ON x1.reference_id=x0.reference_id AND x1.position=1
                 JOIN reference_activate_parents p ON p.reference_id=x0.reference_id AND p.position=0
                 JOIN reference_details d ON d.reference_id=x0.reference_id
                 WHERE x0.reference_id=?1 AND x0.position=0",
                [source],
                |row| Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, Option<u32>>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, Option<u32>>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, u32>(6)?,
                )),
            )
            .unwrap()
        };
        let restore = |record| export_to_db(&db, &[(target, record)].into());
        let absent = (other, None, other, None, 2.0, 3, key);
        assert_eq!(links(), absent);
        // The original archive may be absent from the placement mirror in an older pack.
        db.execute("UPDATE [references] SET data=NULL WHERE id=?1", [source])
            .unwrap();
        let mut wrong_type = placed(target);
        wrong_type.record_type = *b"ACRE";
        restore(wrong_type).unwrap();
        assert_eq!(links(), absent);
        db.execute_batch(
            "CREATE TRIGGER reject_incoming BEFORE UPDATE OF keyword_id ON reference_linked_refs
             BEGIN SELECT RAISE(ABORT, 'reject incoming link'); END;",
        )
        .unwrap();
        assert!(
            restore(placed(target))
                .unwrap_err()
                .to_string()
                .contains("reject incoming link")
        );
        assert_eq!(links(), absent);
        let kind: String = db
            .query_row(
                "SELECT record_type FROM records WHERE form_id=?1",
                [target],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kind, "ACRE");
        db.execute_batch("DROP TRIGGER reject_incoming").unwrap();
        restore(placed(target)).unwrap();
        assert_eq!(
            links(),
            (other, Some(target), other, Some(target), 2.0, 3, key)
        );

        // A room placement layer can retain a different original archive for the same FormID.
        db.execute_batch(
            "CREATE TABLE room_placement_edits (
                layer_id INTEGER, form_id INTEGER, source_form_id INTEGER, deleted INTEGER,
                PRIMARY KEY(form_id,layer_id)
             );",
        )
        .unwrap();
        db.execute(
            "INSERT INTO room_placement_edits VALUES (1,?1,0x01000800,0)",
            [source],
        )
        .unwrap();
        let (_, authored_link) = initial
            .subrecords
            .iter_mut()
            .find(|(tag, data)| tag == b"XLKR" && data.len() == 8)
            .unwrap();
        authored_link[..4].fill(0);
        db.execute(
            "UPDATE [references] SET data=?2 WHERE id=?1",
            params![source, serialize_subrecords(&initial.subrecords)],
        )
        .unwrap();

        export_placed_subset_with_sources(&db, &HashMap::new(), &[target], &order).unwrap();
        assert_eq!(links(), absent);
        restore(placed(target)).unwrap();
        assert_eq!(links(), (other, None, other, Some(target), 2.0, 3, key));
    }

    #[test]
    fn subset_links_respect_nonplacement_deletions_in_full_snapshot() {
        let db = Connection::open_in_memory().unwrap();
        create_tables(&db).unwrap();
        let order = LoadOrder::from_metadata(
            vec!["Skyrim.esm".to_owned()],
            vec![crate::esm::binary::PluginMetadata::default()],
        )
        .unwrap();
        let owner = 0x910;
        let source = |id| {
            let mut source = reference(id, 0x100, 0x500, [0.0; 3], [0.0; 3]);
            source
                .subrecords
                .extend([(b"XOWN".to_vec(), word(owner)), (b"XRNK".to_vec(), word(7))]);
            (id, source)
        };
        let faction = record(owner, b"FACT", None, None, Vec::new());
        let mut deleted = faction.clone();
        deleted.flags = 0x20;
        export_to_db_with_load_order(&db, &[(owner, deleted), source(0x880)].into(), &order)
            .unwrap();

        let details = |id| {
            db.query_row(
                "SELECT owner_id,owner_rank FROM reference_details WHERE reference_id=?1",
                [id],
                |row| Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, u32>(1)?)),
            )
            .unwrap()
        };
        assert_eq!(details(0x880), (None, 7));
        export_to_db(&db, &[source(0x881)].into()).unwrap();
        let owner_before_restore = details(0x881);

        export_to_db(&db, &[(owner, faction)].into()).unwrap();
        assert_eq!(
            (owner_before_restore, details(0x880), details(0x881)),
            ((None, 7), (Some(owner), 7), (Some(owner), 7)),
        );
    }

    #[test]
    fn subset_reference_links_keep_deleted_keywords_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let db = Connection::open(&path).unwrap();
        create_tables(&db).unwrap();
        let placed = |id| reference(id, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        let target = 0x810;
        let keyword = 0x811;
        let live_keyword = 0x812;
        let source_id = 0x900;
        let order = LoadOrder::from_metadata(
            vec!["Skyrim.esm".to_owned()],
            vec![crate::esm::binary::PluginMetadata {
                flags: 0,
                masters: Vec::new(),
            }],
        )
        .unwrap();
        let keyword_record = |id| record(id, b"KYWD", None, None, Vec::new());
        let mut source = placed(source_id);
        for id in [keyword, live_keyword] {
            source
                .subrecords
                .push((b"XLKR".to_vec(), [word(id), word(target)].concat()));
        }
        let assert_links = |db: &Connection, first_keyword| {
            let links = db
                .prepare(
                    "SELECT keyword_id, linked_ref_id FROM reference_linked_refs
                     WHERE reference_id=?1 ORDER BY position",
                )
                .unwrap()
                .query_map([source_id], |row| {
                    Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, u32>(1)?))
                })
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                links,
                [(first_keyword, target), (Some(live_keyword), target)]
            );
        };
        let source_only = [(source_id, source.clone())].into();
        let mut deleted = keyword_record(keyword);
        deleted.flags = 0x20;
        let delete_only = [(keyword, deleted.clone())].into();
        let delete_and_edit = [(keyword, deleted), (source_id, source.clone())].into();
        let restore_and_edit = [
            (keyword, keyword_record(keyword)),
            (source_id, source.clone()),
        ]
        .into();
        let reject_links = |db: &Connection, edits| {
            db.execute_batch(
                "CREATE TEMP TRIGGER reject_links BEFORE INSERT ON reference_linked_refs
                 BEGIN SELECT RAISE(ABORT, 'link write rejected'); END",
            )
            .unwrap();
            assert!(
                export_to_db(db, edits)
                    .unwrap_err()
                    .to_string()
                    .contains("link write rejected")
            );
            db.execute_batch("DROP TRIGGER reject_links").unwrap();
        };

        export_to_db(
            &db,
            &[
                (target, placed(target)),
                (live_keyword, keyword_record(live_keyword)),
                (source_id, source.clone()),
            ]
            .into(),
        )
        .unwrap();
        assert_links(&db, None);
        export_to_db(&db, &[(keyword, keyword_record(keyword))].into()).unwrap();
        assert_links(&db, Some(keyword));

        let snapshot = [
            (target, placed(target)),
            (keyword, keyword_record(keyword)),
            (live_keyword, keyword_record(live_keyword)),
            (source_id, source),
        ]
        .into();
        export_to_db_with_load_order(&db, &snapshot, &order).unwrap();
        assert_links(&db, Some(keyword));

        reject_links(&db, &delete_and_edit);
        export_to_db(&db, &source_only).unwrap();
        assert_links(&db, Some(keyword));

        export_to_db(&db, &delete_only).unwrap();
        assert_links(&db, None);
        drop(db);

        let db = Connection::open(&path).unwrap();
        export_to_db(&db, &source_only).unwrap();
        assert_links(&db, None);

        export_to_db(&db, &delete_and_edit).unwrap();
        reject_links(&db, &restore_and_edit);
        export_to_db(&db, &source_only).unwrap();
        assert_links(&db, None);

        export_to_db(&db, &[(keyword, keyword_record(keyword))].into()).unwrap();
        assert_links(&db, Some(keyword));
        export_to_db(&db, &source_only).unwrap();
        assert_links(&db, Some(keyword));

        export_to_db(&db, &delete_and_edit).unwrap();
        export_to_db_with_load_order(&db, &snapshot, &order).unwrap();
        export_to_db(&db, &source_only).unwrap();
        assert_links(&db, Some(keyword));
    }

    #[test]
    fn subset_target_only_edits_restore_matching_link_fields() {
        let db = Connection::open_in_memory().unwrap();
        create_tables(&db).unwrap();
        let placed = |id| reference(id, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        let target = 0x880;
        let mut source = placed(0x881);
        source.subrecords.extend([
            (b"XLKR".to_vec(), [word(target), word(target)].concat()),
            (
                b"XAPR".to_vec(),
                [word(target), word(2.5f32.to_bits())].concat(),
            ),
            (b"XATR".to_vec(), word(target)),
            (b"XAPD".to_vec(), vec![3]),
        ]);
        let check = |keyword, reference, attached| {
            let row = db.query_row(
                "SELECT keyword_id,linked_ref_id,parent_ref_id,delay,attached_ref_id,activate_parent_flags
                 FROM reference_linked_refs JOIN reference_activate_parents USING(reference_id,position)
                 JOIN reference_details USING(reference_id) WHERE reference_id=0x881",
                [],
                |r| Ok((
                    r.get::<_, Option<u32>>(0)?,
                    r.get::<_, Option<u32>>(1)?,
                    r.get::<_, Option<u32>>(2)?,
                    r.get::<_, f32>(3)?,
                    r.get::<_, Option<u32>>(4)?,
                    r.get::<_, u32>(5)?,
                )),
            ).unwrap();
            assert_eq!(row, (keyword, reference, reference, 2.5, attached, 3));
        };
        export_to_db(
            &db,
            &[(target, placed(target)), (source.form_id, source)].into(),
        )
        .unwrap();
        let player = record(target, b"PLYR", None, None, Vec::new());
        let mut deleted = placed(target);
        deleted.flags = 0x20;
        export_to_db(&db, &[(target, deleted.clone())].into()).unwrap();
        export_to_db(&db, &[(target, player)].into()).unwrap();
        check(Some(target), Some(target), None);
        export_to_db(&db, &[(target, placed(target))].into()).unwrap();
        check(Some(target), Some(target), Some(target));
        let mut actor = placed(target);
        actor.record_type = *b"ACHR";
        export_to_db(&db, &[(target, actor.clone())].into()).unwrap();
        check(Some(target), Some(target), None);
        export_to_db(&db, &[(target, placed(target))].into()).unwrap();
        check(Some(target), Some(target), Some(target));
        for replacement in [actor, placed(target)] {
            export_to_db(&db, &[(target, deleted.clone())].into()).unwrap();
            check(None, None, None);
            let attached = (replacement.record_type == *b"REFR").then_some(target);
            deleted.record_type = replacement.record_type;
            export_to_db(&db, &[(target, replacement)].into()).unwrap();
            check(Some(target), Some(target), attached);
        }
    }

    #[test]
    fn subset_target_only_additions_create_sparse_link_details() {
        let db = Connection::open_in_memory().unwrap();
        create_tables(&db).unwrap();
        let order = LoadOrder::from_metadata(
            vec!["Skyrim.esm".to_owned()],
            vec![crate::esm::binary::PluginMetadata {
                flags: 0,
                masters: Vec::new(),
            }],
        )
        .unwrap();
        let placed = |id| reference(id, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        let source = |id, tag: &[u8; 4], target| {
            let mut source = placed(id);
            source.subrecords.push((tag.to_vec(), word(target)));
            (id, source)
        };
        let attachment = 0x910;
        let owner = 0x911;
        export_to_db_with_load_order(
            &db,
            &[
                source(0x880, b"XATR", attachment),
                source(0x881, b"XOWN", owner),
                (0x882, placed(0x882)),
            ]
            .into(),
            &order,
        )
        .unwrap();
        let details = || {
            db.prepare(
                "SELECT reference_id,attached_ref_id,owner_id,open_by_default,map_marker
                 FROM reference_details ORDER BY reference_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, Option<u32>>(1)?,
                    row.get::<_, Option<u32>>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap()
        };
        assert!(details().is_empty());
        // A pack made before the index existed must backfill sparse sources on first use.
        db.execute_batch("DROP TRIGGER changed_authored_placement; DROP TABLE reference_authored_targets; DROP TABLE reference_authored_changes;").unwrap();
        export_to_db(&db, &[(attachment, placed(attachment))].into()).unwrap();
        assert_eq!(details(), [(0x880, Some(attachment), None, false, false)]);
        // After the one-time backfill, restoring the owner must not decode an unrelated source.
        db.execute("UPDATE records SET data=x'01' WHERE form_id=0x882", [])
            .unwrap();
        export_to_db(
            &db,
            &[(owner, record(owner, b"FACT", None, None, Vec::new()))].into(),
        )
        .unwrap();
        assert_eq!(
            details(),
            [
                (0x880, Some(attachment), None, false, false),
                (0x881, None, Some(owner), false, false),
            ]
        );
    }

    #[test]
    fn subset_attachment_only_source_survives_target_deletion_and_reexport() {
        let db = Connection::open_in_memory().unwrap();
        create_tables(&db).unwrap();
        let placed = |id| reference(id, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        let target = 0x880;
        let mut source = placed(0x881);
        source.subrecords.push((b"XATR".to_vec(), word(target)));
        let mut malformed = placed(0x882);
        malformed.subrecords.push((b"XATR".to_vec(), vec![1]));
        let read = || {
            db.prepare(
                "SELECT reference_id,attached_ref_id FROM reference_details ORDER BY reference_id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((r.get::<_, u32>(0)?, r.get::<_, Option<u32>>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap()
        };
        export_to_db(
            &db,
            &[
                (target, placed(target)),
                (0x881, source.clone()),
                (0x882, malformed),
                (0x883, placed(0x883)),
            ]
            .into(),
        )
        .unwrap();
        assert_eq!(read(), [(0x881, Some(target))]);
        let mut deleted = placed(target);
        deleted.flags = 0x20;
        export_to_db(&db, &[(target, deleted)].into()).unwrap();
        export_to_db(&db, &[(0x881, source)].into()).unwrap();
        assert_eq!(read(), [(0x881, None)]);
        export_to_db(&db, &[(target, placed(target))].into()).unwrap();
        assert_eq!(read(), [(0x881, Some(target))]);
    }

    #[test]
    fn subset_owner_deletion_preserves_rank_and_flags() {
        let db = Connection::open_in_memory().unwrap();
        create_tables(&db).unwrap();
        let owner = 0x880;
        let mut source = reference(0x881, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        source.subrecords.extend([
            (b"XOWN".to_vec(), word(owner)),
            (b"XRNK".to_vec(), word(7)),
            (b"XAPD".to_vec(), vec![3]),
        ]);
        let mut faction = record(owner, b"FACT", None, None, Vec::new());
        let details = || {
            db.query_row(
                "SELECT owner_id, owner_rank, activate_parent_flags
                 FROM reference_details WHERE reference_id=0x881",
                [],
                |r| {
                    Ok((
                        r.get::<_, Option<u32>>(0)?,
                        r.get::<_, i32>(1)?,
                        r.get::<_, u8>(2)?,
                    ))
                },
            )
            .unwrap()
        };
        export_to_db(&db, &[(owner, faction.clone()), (0x881, source)].into()).unwrap();
        assert_eq!(details(), (Some(owner), 7, 3));
        faction.flags = 0x20;
        export_to_db(&db, &[(owner, faction)].into()).unwrap();
        assert_eq!(details(), (None, 7, 3));
        export_to_db(
            &db,
            &[(owner, record(owner, b"FACT", None, None, Vec::new()))].into(),
        )
        .unwrap();
        assert_eq!(details(), (Some(owner), 7, 3));
    }

    #[test]
    fn actor_tombstone_clears_scripts_and_links_from_surviving_sources() {
        let db = Connection::open_in_memory().unwrap();
        create_tables(&db).unwrap();
        let mut actor = reference(0x880, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        actor.record_type = *b"ACHR";
        let mut source = reference(0x881, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        source.subrecords.extend([
            (b"XLKR".to_vec(), [word(0x880), word(0x880)].concat()),
            (
                b"XAPR".to_vec(),
                [word(0x880), 2.5f32.to_le_bytes().to_vec()].concat(),
            ),
        ]);
        export_to_db(
            &db,
            &[(actor.form_id, actor), (source.form_id, source)].into(),
        )
        .unwrap();
        let links = || {
            db.query_row(
            "SELECT l.keyword_id, l.linked_ref_id, a.parent_ref_id, a.delay FROM reference_linked_refs l
             JOIN reference_activate_parents a USING(reference_id,position) WHERE l.reference_id=0x881 AND l.position=0",
            [], |r| Ok((r.get::<_,Option<u32>>(0)?,r.get::<_,Option<u32>>(1)?,r.get::<_,Option<u32>>(2)?,r.get::<_,f32>(3)?))
        ).unwrap()
        };
        assert_eq!(links(), (Some(0x880), Some(0x880), Some(0x880), 2.5));
        db.execute("INSERT INTO scripts(form_id,script_name,vmad,properties_json) VALUES(0x880,'ActorScript',X'00','[]')", []).unwrap();
        let mut tombstone = record(0x880, b"ACHR", None, None, Vec::new());
        tombstone.flags = 0x20;
        export_to_db(&db, &[(0x880, tombstone)].into()).unwrap();
        assert_eq!(links(), (None, None, None, 2.5));
        for (table, id) in [("scripts", "form_id"), ("[references]", "id")] {
            let count: u32 = db
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE {id}=0x880"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
    }

    #[test]
    fn subset_reference_script_projections_replace_atomically() {
        use dummy_content::inhouse_vmad as fixture;
        let vmad = |name, limit: i32| {
            fixture::field(
                5,
                2,
                &[fixture::script(
                    5,
                    name,
                    0,
                    &[fixture::property(5, "Limit", 3, 1, &limit.to_le_bytes())],
                )],
                &[],
            )
        };
        let original_vmad = vmad("DoorScript", 42);
        let replacement_vmad = vmad("ReplacementScript", 71);
        let scripted = |id, data: &[u8]| {
            let mut row = reference(id, 0x100, 0x500, [0.0; 3], [0.0; 3]);
            if id == 0x802 {
                row.record_type = *b"ACHR";
            }
            row.subrecords.push((b"VMAD".to_vec(), data.to_vec()));
            (id, row)
        };
        for normalized in [false, true] {
            let mut db = Connection::open_in_memory().unwrap();
            create_tables(&db).unwrap();
            if normalized {
                let tx = db.transaction().unwrap();
                super::super::inhouse_script_properties::replace_snapshot_tables(&tx).unwrap();
                tx.commit().unwrap();
            }
            let tables = if normalized {
                &["scripts", "script_properties", "script_property_values"][..]
            } else {
                &["scripts"][..]
            };
            let assert_scripts = |expected: &[(u32, &str)]| {
                for table in tables {
                    let actual = db
                        .prepare(&format!(
                            "SELECT form_id, script_name FROM {table} ORDER BY form_id, script_name"
                        ))
                        .unwrap()
                        .query_map([], |row| {
                            Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
                        })
                        .unwrap()
                        .collect::<Result<Vec<_>>>()
                        .unwrap();
                    assert_eq!(
                        actual
                            .iter()
                            .map(|(id, name)| (*id, name.as_str()))
                            .collect::<Vec<_>>(),
                        expected,
                        "{table}"
                    );
                }
            };
            let owner_data = || {
                db.query_row("SELECT data FROM records WHERE form_id=0x801", [], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .unwrap()
            };
            let original = [
                scripted(0x801, &original_vmad),
                scripted(0x802, &original_vmad),
            ]
            .into();
            export_to_db(&db, &original).unwrap();
            let original_scripts = [(0x801, "DoorScript"), (0x802, "DoorScript")];
            assert_scripts(&original_scripts);
            let before = owner_data();
            let replacement = [scripted(0x801, &replacement_vmad)].into();
            db.execute_batch(
                "CREATE TRIGGER reject_script BEFORE INSERT ON scripts
                 WHEN NEW.script_name='ReplacementScript'
                 BEGIN SELECT RAISE(ABORT, 'script write rejected'); END;",
            )
            .unwrap();
            assert!(
                export_to_db(&db, &replacement)
                    .unwrap_err()
                    .to_string()
                    .contains("script write rejected")
            );
            assert_scripts(&original_scripts);
            assert_eq!(owner_data(), before);
            db.execute_batch("DROP TRIGGER reject_script").unwrap();

            export_to_db(&db, &replacement).unwrap();
            assert_scripts(&[(0x801, "ReplacementScript"), (0x802, "DoorScript")]);
            let actual: Vec<u8> = db
                .query_row("SELECT vmad FROM scripts WHERE form_id=0x801", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(actual, replacement_vmad);
            assert_ne!(owner_data(), before);

            export_to_db(&db, &[scripted(0x801, &[5])].into()).unwrap();
            assert_scripts(&[(0x802, "DoorScript")]);
            export_to_db(&db, &replacement).unwrap();

            let plain = reference(0x801, 0x100, 0x500, [0.0; 3], [0.0; 3]);
            export_to_db(&db, &[(0x801, plain)].into()).unwrap();
            assert_scripts(&[(0x802, "DoorScript")]);

            export_to_db(&db, &replacement).unwrap();
            let mut deleted = record(0x801, b"REFR", None, None, Vec::new());
            deleted.flags = 0x20;
            export_to_db(&db, &[(0x801, deleted)].into()).unwrap();
            assert_scripts(&[(0x802, "DoorScript")]);

            let (id, mut actor) = scripted(0x802, &original_vmad);
            actor.flags = 0x20;
            export_to_db(&db, &[(id, actor)].into()).unwrap();
            assert_scripts(&[]);
        }
    }

    // ---- Load door links (db-schema.md section 17) ----

    fn load_door(form_id: u32, cell: u32, destination: u32, arrival: [f32; 3]) -> RawRecord {
        let mut xtel = word(destination);
        xtel.extend(floats(arrival));
        xtel.extend(floats([0.0, 0.0, 1.5]));
        xtel.extend(word(0));
        let mut door = reference(form_id, cell, 0x500, [0.0; 3], [0.0; 3]);
        door.subrecords.push((b"XTEL".to_vec(), xtel));
        door
    }

    fn door_links(conn: &Connection) -> Vec<(u32, u32, u32, Option<String>)> {
        conn.prepare(
            "SELECT ref_id,destination_ref_id,destination_cell_id,destination_name
             FROM door_links ORDER BY ref_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    /// Full runtime exports own `door_links`; diagnostic archives clear it and subsets retain it.
    #[test]
    fn archive_exports_clear_door_links_and_runtime_exports_restore_them() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("Doors.esp");
        std::fs::write(&plugin, b"synthetic door provenance").unwrap();
        let paths = [plugin];
        let order = LoadOrder::from_metadata(
            vec!["Doors.esp".into()],
            vec![crate::esm::binary::PluginMetadata {
                masters: Vec::new(),
                flags: 0,
            }],
        )
        .unwrap();
        let named_cell = |form_id: u32, name: &[u8]| {
            record(
                form_id,
                b"CELL",
                None,
                None,
                vec![(b"FULL".to_vec(), name.to_vec())],
            )
        };
        let records = vec![
            record(0x500, b"DOOR", None, None, Vec::new()),
            named_cell(0x100, b"Hall\0"),
            named_cell(0x101, b"Cellar\0"),
            load_door(0x2001, 0x100, 0x2002, [1.0, 2.0, 3.0]),
            load_door(0x2002, 0x101, 0x2001, [4.0, 5.0, 6.0]),
        ];
        let master: HashMap<u32, RawRecord> = records
            .into_iter()
            .map(|record| (record.form_id, record))
            .collect();
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        assert!(door_links(&conn).is_empty(), "create_tables adds the table");
        let export = |conn: &Connection| {
            export_inhouse_to_db(
                conn,
                &master,
                &order,
                &paths,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &ReadResult::default(),
            )
            .unwrap()
        };
        let expected = vec![
            (0x2001, 0x2002, 0x101, Some("Cellar".to_owned())),
            (0x2002, 0x2001, 0x100, Some("Hall".to_owned())),
        ];
        export(&conn);
        assert_eq!(door_links(&conn), expected);

        export_to_db(&conn, &HashMap::new()).unwrap();
        assert_eq!(door_links(&conn), expected, "a subset export leaves links");

        export_inhouse_record_archive_to_db(&conn, &master, &order, &paths, &HashSet::new())
            .unwrap();
        assert!(door_links(&conn).is_empty(), "deferred runtime table");
        export(&conn);
        assert_eq!(door_links(&conn), expected);
    }

    /// Exports `records` and returns the connection plus the room graph report.
    fn export_rooms(records: Vec<RawRecord>) -> (Connection, rooms::RoomGraphReport) {
        let master: HashMap<u32, RawRecord> = records.into_iter().map(|r| (r.form_id, r)).collect();
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        export_to_db(&conn, &master).unwrap();
        let interior: std::collections::HashSet<u32> = master
            .values()
            .filter(|r| &r.record_type == b"CELL" && r.worldspace_form_id.is_none())
            .map(|r| r.form_id)
            .collect();
        let tx = conn.unchecked_transaction().unwrap();
        let report = rooms::export_room_graph(&tx, &master, &interior).unwrap();
        tx.commit().unwrap();
        (conn, report)
    }

    fn rooms_of(conn: &Connection, reference: u32) -> Vec<(u32, i64)> {
        let mut statement = conn
            .prepare(
                "SELECT room_ref, contained FROM reference_rooms WHERE reference_id=?1 ORDER BY room_ref",
            )
            .unwrap();
        statement
            .query_map([reference], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn assigns_objects_to_the_room_boxes_that_contain_their_pivot() {
        let (conn, report) = export_rooms(vec![
            interior_cell(0x100),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [100.0; 3], vec![]),
            room(
                0x1002,
                0x100,
                [150.0, 0.0, 0.0],
                [0.0; 3],
                [100.0; 3],
                vec![],
            ),
            reference(0x2001, 0x100, 0x500, [-50.0, 0.0, 0.0], [0.0; 3]),
            reference(0x2002, 0x100, 0x500, [1000.0, 0.0, 0.0], [0.0; 3]),
            reference(0x2003, 0x100, 0x500, [75.0, 0.0, 0.0], [0.0; 3]),
        ]);
        assert_eq!(rooms_of(&conn, 0x2001), vec![(0x1001, 1)]);
        assert_eq!(rooms_of(&conn, 0x2002), vec![]);
        assert_eq!(rooms_of(&conn, 0x2003), vec![(0x1001, 1), (0x1002, 1)]);
        // Markers are not members of anything.
        assert_eq!(rooms_of(&conn, 0x1001), vec![]);
        assert_eq!(report.rooms, 2);
        assert_eq!(report.objects_in_no_room, 1);
        assert_eq!(report.objects_in_several_rooms, 1);
        let read = rooms::RoomGraphReport::read(&conn).unwrap();
        assert_eq!(read, report);
    }

    #[test]
    fn authored_multibound_overrides_do_not_acquire_geometric_room_membership() {
        let mut unrestricted = reference(0x2001, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        unrestricted.flags |= 0x8000_0000;
        let mut assigned = unrestricted.clone();
        assigned.form_id = 0x2002;
        assigned.subrecords.push((b"XMBR".to_vec(), word(0x1001)));
        let (conn, _) = export_rooms(vec![
            interior_cell(0x100),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [100.0; 3], vec![]),
            unrestricted,
            assigned,
            reference(0x2003, 0x100, 0x500, [0.0; 3], [0.0; 3]),
        ]);
        assert!(
            rooms_of(&conn, 0x2001).is_empty(),
            "NONE stays unrestricted"
        );
        assert!(
            rooms_of(&conn, 0x2002).is_empty(),
            "authored targets bypass geometric membership too"
        );
        assert_eq!(rooms_of(&conn, 0x2003), vec![(0x1001, 1)]);
        let rules = conn
            .prepare("SELECT assignment,target_ref FROM room_reference_rules ORDER BY reference_id")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<u32>>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rules,
            vec![
                ("none".into(), None),
                ("target".into(), Some(0x1001)),
                ("auto".into(), None)
            ]
        );
    }

    #[test]
    fn a_rotated_room_is_tested_in_its_own_frame() {
        // Long along local X (half 100), narrow along local Y (half 10). Turned a quarter
        // turn about Z, the long side lies along world Y.
        let (conn, _) = export_rooms(vec![
            interior_cell(0x100),
            room(
                0x1001,
                0x100,
                [0.0; 3],
                [0.0, 0.0, std::f32::consts::FRAC_PI_2],
                [100.0, 10.0, 50.0],
                vec![],
            ),
            reference(0x2001, 0x100, 0x500, [0.0, 80.0, 0.0], [0.0; 3]),
            reference(0x2002, 0x100, 0x500, [80.0, 0.0, 0.0], [0.0; 3]),
        ]);
        assert_eq!(rooms_of(&conn, 0x2001), vec![(0x1001, 1)]);
        assert_eq!(rooms_of(&conn, 0x2002), vec![]);
    }

    #[test]
    fn writes_room_bounds_with_lighting_template_image_space_flags_and_links() {
        let xrmr = vec![3, 0xC0, 1, 0];
        let (conn, report) = export_rooms(vec![
            interior_cell(0x100),
            room(
                0x1001,
                0x100,
                [10.0, 20.0, 30.0],
                [0.1, 0.2, 0.3],
                [100.0, 200.0, 300.0],
                vec![
                    (b"XRMR", xrmr),
                    (b"LNAM", word(0x0001_2345)),
                    (b"INAM", word(0x0006_7890)),
                    (b"XLRM", word(0x1002)),
                    (b"XLRM", word(0x1003)),
                    (b"XLRM", word(0)),
                ],
            ),
            room(0x1002, 0x100, [0.0; 3], [0.0; 3], [1.0; 3], vec![]),
        ]);
        let (cell, pos_x, pos_z, rot_y, rot_z): (u32, f64, f64, f64, f64) = conn
            .query_row(
                "SELECT cell_id,pos_x,pos_z,rot_y,rot_z FROM room_bounds WHERE ref_id=0x1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(cell, 0x100);
        assert_eq!((pos_x, pos_z), (10.0, 30.0));
        assert_eq!((rot_y, rot_z), (f64::from(0.2f32), f64::from(0.3f32)));
        let half: (f64, f64, f64) = conn
            .query_row(
                "SELECT half_x,half_y,half_z FROM room_bounds WHERE ref_id=0x1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(half, (100.0, 200.0, 300.0));
        let lighting: (Option<u32>, Option<u32>, i64) = conn
            .query_row(
                "SELECT lighting_template_id,image_space_id,flags FROM room_bounds WHERE ref_id=0x1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        // flags: XRMR flags byte | (u16 << 8)
        assert_eq!(
            lighting,
            (Some(0x0001_2345), Some(0x0006_7890), 0xC0 | (1 << 8))
        );
        let second: (Option<u32>, Option<u32>) = conn
            .query_row(
                "SELECT lighting_template_id,image_space_id FROM room_bounds WHERE ref_id=0x1002",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(second, (None, None));
        // Links are stored as written, including one to a room that does not exist; a zero
        // target (no room) is not a link.
        let links: Vec<(u32, u32)> = conn
            .prepare("SELECT room_ref, linked_ref FROM room_links ORDER BY linked_ref")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(links, vec![(0x1001, 0x1002), (0x1001, 0x1003)]);
        assert_eq!(report.links, 2);
    }

    #[test]
    fn portals_name_the_rooms_on_both_sides_or_null() {
        let (conn, report) = export_rooms(vec![
            interior_cell(0x100),
            interior_cell(0x101),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            room(0x1002, 0x100, [50.0, 0.0, 0.0], [0.0; 3], [10.0; 3], vec![]),
            room(0x1003, 0x101, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            portal(0x3001, 0x100, Some((0x1001, 0x1002))),
            portal(0x3002, 0x100, Some((0x1001, 0))),
            // 0x9999 is no room; 0x1003 is a room of another cell.
            portal(0x3003, 0x100, Some((0x9999, 0x1003))),
            portal(0x3004, 0x100, None),
        ]);
        let sides = |id: u32| -> (Option<u32>, Option<u32>) {
            conn.query_row(
                "SELECT room_a, room_b FROM room_portals WHERE ref_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(sides(0x3001), (Some(0x1001), Some(0x1002)));
        assert_eq!(sides(0x3002), (Some(0x1001), None));
        assert_eq!(sides(0x3003), (None, None));
        assert_eq!(sides(0x3004), (None, None));
        let size: (f64, f64) = conn
            .query_row(
                "SELECT half_x, half_z FROM room_portals WHERE ref_id=0x3001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(size, (40.0, 60.0));
        assert_eq!(report.portals, 4);
        assert_eq!(report.portals_with_missing_side, 3);
    }

    /// A portal marker whose `XPOD` is the given raw bytes.
    fn portal_with_xpod(form_id: u32, cell: u32, xpod: Vec<u8>) -> RawRecord {
        marker(
            form_id,
            cell,
            PORTAL_MARKER_BASE,
            [0.0; 3],
            [0.0; 3],
            vec![(b"XPRM", xprm([40.0, 1.0, 60.0], 3)), (b"XPOD", xpod)],
        )
    }

    fn portal_rows(conn: &Connection, id: u32) -> Vec<(i64, Option<u32>, Option<u32>)> {
        conn.prepare(
            "SELECT pair_index, room_a, room_b FROM room_portals WHERE ref_id=?1 ORDER BY pair_index",
        )
        .unwrap()
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    #[test]
    fn only_authored_null_endpoints_name_unbounded_space() {
        let (conn, _) = export_rooms(vec![
            interior_cell(0x100),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            portal(0x3001, 0x100, Some((0x1001, 0))),
            portal(0x3002, 0x100, Some((0x1001, 0x9999))),
            portal(0x3003, 0x100, None),
        ]);
        let flags = |id| {
            conn.query_row(
                "SELECT unbounded_a,unbounded_b FROM room_portals WHERE ref_id=?1",
                [id],
                |r| Ok((r.get::<_, bool>(0)?, r.get::<_, bool>(1)?)),
            )
            .unwrap()
        };
        assert_eq!(flags(0x3001), (false, true));
        assert_eq!(flags(0x3002), (false, false));
        assert_eq!(flags(0x3003), (false, false));
        // A typed reader can clear a failed nonzero link. Its original source disambiguates it.
        let invalid = [word(0x1001), word(0x0900_1234)].concat();
        rooms::restore_portal_endpoint_provenance(&conn, [(0x3001, Some(invalid.as_slice()))])
            .unwrap();
        assert_eq!(flags(0x3001), (false, false));
        let outside = [word(0x1001), word(0)].concat();
        rooms::restore_portal_endpoint_provenance(&conn, [(0x3001, Some(outside.as_slice()))])
            .unwrap();
        rooms::restore_portal_endpoint_provenance(&conn, [(0x3001, Some(outside.as_slice()))])
            .unwrap();
        assert_eq!(flags(0x3001), (false, true));
    }

    #[test]
    fn a_nonzero_missing_endpoint_invalidates_only_its_cell() {
        let (conn, _) = export_rooms(vec![
            interior_cell(0x100),
            interior_cell(0x200),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            room(0x2001, 0x200, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            portal(0x3001, 0x100, Some((0x1001, 0))),
            portal(0x3002, 0x200, Some((0x2001, 0x1001))),
        ]);
        let complete = |cell: u32| {
            conn.query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
                [cell],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
        };
        assert!(complete(0x100));
        assert!(!complete(0x200));
        let invalid = [word(0x1001), word(0x0900_1234)].concat();
        rooms::restore_portal_endpoint_provenance(&conn, [(0x3001, Some(invalid.as_slice()))])
            .unwrap();
        assert!(!complete(0x100));
    }

    #[test]
    fn endpoint_migration_keeps_legacy_nulls_unknown() {
        let conn = old_shape_database();
        create_tables(&conn).unwrap();
        create_tables(&conn).unwrap();
        let flags: (bool, bool) = conn
            .query_row(
                "SELECT unbounded_a,unbounded_b FROM room_portals WHERE ref_id=5",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(flags, (false, false));
        assert_eq!(portal_rows(&conn, 5), vec![(0, None, None)]);
    }

    #[test]
    fn a_portal_with_several_xpod_pairs_gets_one_row_per_pair() {
        let (conn, report) = export_rooms(vec![
            interior_cell(0x100),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            room(0x1002, 0x100, [50.0, 0.0, 0.0], [0.0; 3], [10.0; 3], vec![]),
            room(0x1003, 0x100, [90.0, 0.0, 0.0], [0.0; 3], [10.0; 3], vec![]),
            // Two pairs.
            portal_with_xpod(
                0x3001,
                0x100,
                [word(0x1001), word(0x1002), word(0x1002), word(0x1003)].concat(),
            ),
            // One pair and four trailing bytes, which are ignored and counted.
            portal_with_xpod(
                0x3002,
                0x100,
                [word(0x1001), word(0x1002), word(0x1003)].concat(),
            ),
            // The old shape: exactly one pair.
            portal_with_xpod(0x3003, 0x100, [word(0x1002), word(0x1003)].concat()),
            // Shorter than one pair: one row, NULL sides, counted as trailing bytes.
            portal_with_xpod(0x3004, 0x100, word(0x1001)),
            // Empty XPOD, and none at all.
            portal_with_xpod(0x3005, 0x100, Vec::new()),
            portal(0x3006, 0x100, None),
        ]);
        assert_eq!(
            portal_rows(&conn, 0x3001),
            vec![
                (0, Some(0x1001), Some(0x1002)),
                (1, Some(0x1002), Some(0x1003))
            ]
        );
        assert_eq!(
            portal_rows(&conn, 0x3002),
            vec![(0, Some(0x1001), Some(0x1002))]
        );
        assert_eq!(
            portal_rows(&conn, 0x3003),
            vec![(0, Some(0x1002), Some(0x1003))]
        );
        for id in [0x3004, 0x3005, 0x3006] {
            assert_eq!(portal_rows(&conn, id), vec![(0, None, None)], "{id:X}");
        }
        // Geometry repeats on every row of a portal.
        let sizes: Vec<(f64, f64)> = conn
            .prepare("SELECT half_x, half_z FROM room_portals WHERE ref_id=0x3001")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(sizes, vec![(40.0, 60.0); 2]);
        // `portals` counts markers; the missing-side counter counts rows (3004-3006).
        assert_eq!(report.portals, 6);
        assert_eq!(report.portals_with_missing_side, 3);
        assert_eq!(report.portals_with_extra_pairs, 1);
        assert_eq!(report.portals_with_trailing_xpod_bytes, 2);
        let read = rooms::RoomGraphReport::read(&conn).unwrap();
        assert_eq!(read, report);
    }

    /// A database whose `room_portals` has the shape before `pair_index`, holding two portals.
    fn old_shape_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE room_portals (
                 ref_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
                 pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
                 rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
                 half_x REAL NOT NULL, half_z REAL NOT NULL,
                 room_a INTEGER, room_b INTEGER
             );
             CREATE INDEX idx_room_portals_cell ON room_portals(cell_id);
             INSERT INTO room_portals VALUES (1, 2, 0, 0, 0, 0, 0, 0, 3, 4, 7, 8);
             INSERT INTO room_portals VALUES (5, 2, 1, 2, 3, 0, 0, 0, 5, 6, NULL, NULL);",
        )
        .unwrap();
        conn
    }

    /// `room_portals` as (ref_id, pair_index, cell_id, half_x, room_a, room_b).
    type PortalTableRow = (i64, i64, i64, f64, Option<i64>, Option<i64>);

    fn portal_table(conn: &Connection) -> Vec<PortalTableRow> {
        conn.prepare(
            "SELECT ref_id, pair_index, cell_id, half_x, room_a, room_b FROM room_portals ORDER BY ref_id, pair_index",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    fn old_shape_rows() -> Vec<PortalTableRow> {
        vec![(1, 0, 2, 3.0, Some(7), Some(8)), (5, 0, 2, 5.0, None, None)]
    }

    #[test]
    fn an_older_room_portals_table_is_migrated_in_place_keeping_its_rows() {
        let conn = old_shape_database();
        create_tables(&conn).unwrap();
        assert_eq!(portal_table(&conn), old_shape_rows());
        // The key is now (ref_id, pair_index), and the cell index is back.
        conn.execute(
            "INSERT INTO room_portals(ref_id, pair_index, cell_id, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, half_x, half_z)
             VALUES (1, 1, 2, 0, 0, 0, 0, 0, 0, 3, 4)",
            [],
        )
        .unwrap();
        let index: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_room_portals_cell'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index, 1);
        let leftovers: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='room_portals_new'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leftovers, 0);
        // A second pass is a no-op.
        create_tables(&conn).unwrap();
        assert_eq!(portal_table(&conn).len(), 3);
    }

    #[test]
    fn a_subset_export_on_a_migrated_database_leaves_the_portals_alone() {
        let conn = old_shape_database();
        create_tables(&conn).unwrap();
        let records = vec![
            interior_cell(0x100),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
        ];
        let master: HashMap<u32, RawRecord> = records.into_iter().map(|r| (r.form_id, r)).collect();
        export_to_db(&conn, &master).unwrap();
        assert_eq!(portal_table(&conn), old_shape_rows());
    }

    /// A RELEASE that fails (another connection holds a read lock, so the commit is busy)
    /// leaves no transaction open and the old table untouched; the next pass migrates it.
    #[test]
    fn a_busy_commit_rolls_the_migration_back_and_closes_the_savepoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE room_portals (
                     ref_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL,
                     pos_x REAL NOT NULL, pos_y REAL NOT NULL, pos_z REAL NOT NULL,
                     rot_x REAL NOT NULL, rot_y REAL NOT NULL, rot_z REAL NOT NULL,
                     half_x REAL NOT NULL, half_z REAL NOT NULL,
                     room_a INTEGER, room_b INTEGER
                 );
                 INSERT INTO room_portals VALUES (1, 2, 0, 0, 0, 0, 0, 0, 3, 4, 7, 8);",
            )
            .unwrap();
        writer.busy_timeout(std::time::Duration::ZERO).unwrap();
        let reader = Connection::open(&path).unwrap();
        // An open read transaction keeps its shared lock until COMMIT.
        reader.execute_batch("BEGIN").unwrap();
        let held: i64 = reader
            .query_row("SELECT count(*) FROM room_portals", [], |r| r.get(0))
            .unwrap();
        assert_eq!(held, 1);

        assert!(rooms::migrate_room_portals(&writer).is_err());
        assert!(writer.is_autocommit(), "the savepoint was left open");

        reader.execute_batch("COMMIT").unwrap();
        use rusqlite::OptionalExtension;
        let pair_index: Option<i64> = writer
            .query_row(
                "SELECT 1 FROM pragma_table_info('room_portals') WHERE name='pair_index'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(pair_index, None);
        rooms::migrate_room_portals(&writer).unwrap();
        assert_eq!(
            portal_table(&writer),
            vec![(1, 0, 2, 3.0, Some(7), Some(8))]
        );
    }

    #[test]
    fn a_failed_full_export_leaves_the_migrated_portals_intact() {
        use crate::esm::load_order::LoadOrder;
        let conn = old_shape_database();
        create_tables(&conn).unwrap();
        // Fail when the export writes its counters, which is after it cleared the room tables
        // inside the transaction.
        conn.execute_batch(
            "CREATE TRIGGER fail_stats_insert BEFORE INSERT ON room_graph_stats
             BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
        )
        .unwrap();
        let master: HashMap<u32, RawRecord> = HashMap::new();
        let error = export_to_db_with_load_order(&conn, &master, &LoadOrder::read(&[]).unwrap())
            .unwrap_err();
        assert!(error.to_string().contains("forced failure"), "{error}");
        assert_eq!(portal_table(&conn), old_shape_rows());
    }

    #[test]
    fn deleted_portal_does_not_connect_rooms() {
        let live = portal(0x104, 7, Some((0x101, 0x102)));
        let mut deleted = live.clone();
        deleted.form_id = 0x105;
        deleted.flags |= 0x20;
        let (db, _) = export_rooms(vec![
            interior_cell(7),
            room(0x101, 7, [0.; 3], [0.; 3], [10.; 3], vec![]),
            room(0x102, 7, [20.; 3], [0.; 3], [10.; 3], vec![]),
            live,
            deleted,
        ]);
        assert_eq!(
            db.query_row("SELECT count(*),min(ref_id) FROM room_portals", [], |r| Ok(
                (r.get::<_, u32>(0)?, r.get::<_, u32>(1)?)
            ))
            .unwrap(),
            (1, 0x104)
        );
    }

    #[test]
    fn skips_non_box_rooms_and_exterior_markers() {
        let mut sphere = room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]);
        sphere.subrecords.retain(|(tag, _)| tag != b"XPRM");
        sphere
            .subrecords
            .push((b"XPRM".to_vec(), xprm([10.0; 3], 2)));
        let (conn, report) = export_rooms(vec![
            interior_cell(0x100),
            // An exterior cell: it has a worldspace.
            record(0x200, b"CELL", None, Some(0x3C), Vec::new()),
            sphere,
            room(0x1002, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            room(0x1003, 0x200, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            portal(0x3001, 0x200, Some((0x1003, 0x1003))),
            reference(0x2001, 0x200, 0x500, [0.0; 3], [0.0; 3]),
        ]);
        let ids: Vec<u32> = conn
            .prepare("SELECT ref_id FROM room_bounds")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(ids, vec![0x1002]);
        assert_eq!(report.rooms, 1);
        assert_eq!(report.non_box_rooms_skipped, 1);
        let complete: bool = conn
            .query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=0x100",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!complete);
        assert_eq!(report.portals, 0);
        assert_eq!(rooms_of(&conn, 0x2001), vec![]);
    }

    #[test]
    fn a_repeated_export_replaces_the_room_tables() {
        let records = vec![
            interior_cell(0x100),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            reference(0x2001, 0x100, 0x500, [0.0; 3], [0.0; 3]),
        ];
        let master: HashMap<u32, RawRecord> = records.into_iter().map(|r| (r.form_id, r)).collect();
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        export_to_db(&conn, &master).unwrap();
        let interior = std::collections::HashSet::from([0x100u32]);
        for _ in 0..2 {
            let tx = conn.unchecked_transaction().unwrap();
            rooms::export_room_graph(&tx, &master, &interior).unwrap();
            tx.commit().unwrap();
        }
        let count: i64 = conn
            .query_row("SELECT count(*) FROM reference_rooms", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        // With no rooms left in the records the tables empty out.
        let tx = conn.unchecked_transaction().unwrap();
        rooms::export_room_graph(&tx, &HashMap::new(), &interior).unwrap();
        tx.commit().unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM room_bounds", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn human_review_negative_room_and_portal_extents_are_incomplete() {
        for is_room in [true, false] {
            for axis in if is_room { vec![0, 1, 2] } else { vec![0, 2] } {
                let mut invalid = if is_room {
                    room(0x1002, 0x100, [50.0; 3], [0.0; 3], [10.0; 3], vec![])
                } else {
                    portal(0x1002, 0x100, Some((0x1001, 0x1001)))
                };
                let shape = &mut invalid
                    .subrecords
                    .iter_mut()
                    .find(|(tag, _)| tag == b"XPRM")
                    .unwrap()
                    .1;
                shape[axis * 4..axis * 4 + 4].copy_from_slice(&(-1f32).to_le_bytes());
                let (conn, report) = export_rooms(vec![
                    interior_cell(0x100),
                    room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
                    invalid,
                ]);
                assert_eq!(
                    report.invalid_markers_skipped, 1,
                    "is_room={is_room},axis={axis}"
                );
                let complete: bool = conn
                    .query_row(
                        "SELECT complete FROM room_graph_cells WHERE cell_id=0x100",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert!(!complete);
            }
        }
    }

    #[test]
    fn incomplete_positions_omit_room_membership_and_report_the_placements() {
        let mut missing = reference(0x1010, 0x100, 0x700, [0.0; 3], [0.0; 3]);
        missing.subrecords.retain(|(tag, _)| tag != b"DATA");
        let mut short = missing.clone();
        short.form_id = 0x1011;
        short.subrecords.push((b"DATA".to_vec(), vec![0; 8]));
        let records = [
            interior_cell(0x100),
            record(0x700, b"STAT", None, None, Vec::new()),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            missing,
            short,
            reference(0x1012, 0x100, 0x700, [0.0; 3], [0.0; 3]),
        ]
        .into_iter()
        .map(|record| (record.form_id, record))
        .collect();
        let order = LoadOrder::from_metadata(
            vec!["Rooms.esp".into()],
            vec![crate::esm::binary::PluginMetadata {
                masters: vec![],
                flags: 0,
            }],
        )
        .unwrap();
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        export_to_db_with_load_order(&conn, &records, &order).unwrap();

        assert!(rooms_of(&conn, 0x1010).is_empty());
        assert!(rooms_of(&conn, 0x1011).is_empty());
        assert_eq!(rooms_of(&conn, 0x1012), [(0x1001, 1)]);
        assert_eq!(
            omission_ids(&conn),
            [(
                "reference_unusable_data".to_owned(),
                2,
                vec![0x1010, 0x1011]
            )]
        );
    }

    #[test]
    fn room_and_portal_markers_require_a_complete_transform() {
        for is_room in [true, false] {
            for truncated in [false, true] {
                let mut invalid = if is_room {
                    room(0x1002, 0x100, [50.0; 3], [0.0; 3], [10.0; 3], vec![])
                } else {
                    portal(0x1002, 0x100, Some((0x1001, 0x1001)))
                };
                invalid.subrecords.retain(|(tag, _)| tag != b"DATA");
                if truncated {
                    invalid.subrecords.push((b"DATA".to_vec(), vec![0; 20]));
                }
                let (conn, report) = export_rooms(vec![
                    interior_cell(0x100),
                    room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
                    invalid,
                ]);
                assert_eq!(report.invalid_markers_skipped, 1);
                assert_eq!(report.rooms, 1);
                assert_eq!(report.portals, 0);
                let complete: bool = conn
                    .query_row(
                        "SELECT complete FROM room_graph_cells WHERE cell_id=0x100",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(!complete);
            }
        }
    }

    #[test]
    fn subset_base_model_updates_invalidate_spheres_and_referencing_cells() {
        for kind in [b"STAT", b"LIGH"] {
            for (replacement, deleted, unchanged) in [
                (Some("A.nif"), false, true),
                (Some(r"Meshes\A.NIF"), false, true),
                (Some(r"Meshes\%41.NIF"), false, false),
                (Some("A.nif"), true, false),
                (Some("B.nif"), false, false),
                (None, false, false),
            ] {
                let base = |model: Option<&str>| {
                    let mut fields = model
                        .map(|path| (b"MODL".to_vec(), cstr(path)))
                        .into_iter()
                        .collect::<Vec<_>>();
                    if kind == b"LIGH" {
                        fields.push((b"DATA".to_vec(), vec![0; 48]));
                    }
                    record(0x500, kind, None, None, fields)
                };
                let (conn, _) = export_rooms(vec![
                    interior_cell(0x100),
                    interior_cell(0x200),
                    interior_cell(0x300),
                    room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
                    room(0x1002, 0x200, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
                    room(0x1003, 0x300, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
                    reference(0x2001, 0x100, 0x500, [0.0; 3], [0.0; 3]),
                    reference(0x2002, 0x200, 0x500, [0.0; 3], [0.0; 3]),
                    reference(0x2003, 0x300, 0x501, [0.0; 3], [0.0; 3]),
                    base(Some("A.nif")),
                ]);
                conn.execute_batch(
                    "INSERT INTO room_model_bounds VALUES(1280,1,2,3,4),(1281,5,6,7,8);
                     UPDATE statics SET bounds_valid=1,bounds_min_x=-12 WHERE id=1280",
                )
                .unwrap();
                let mut updated = base(replacement);
                if deleted {
                    updated.flags |= 0x20;
                }
                export_to_db(&conn, &HashMap::from([(0x500, updated)])).unwrap();
                let measured: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM statics WHERE id=1280 AND bounds_valid=1 AND bounds_min_x=-12)",
                    [], |row| row.get(0)
                ).unwrap();
                assert_eq!(measured, unchanged);
                let retained: bool = conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM room_model_bounds WHERE base_form_id=1280)",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(
                    retained, unchanged,
                    "kind={kind:?}, replacement={replacement:?}: never keep another model's sphere"
                );
                for cell in [0x100, 0x200, 0x300] {
                    let complete: bool = conn
                        .query_row(
                            "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
                            [cell],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert_eq!(
                        complete,
                        unchanged || cell == 0x300,
                        "kind={kind:?}, replacement={replacement:?}, cell={cell:x}"
                    );
                }
                let path: Option<String> = conn
                    .query_row(
                        "SELECT max(model_path) FROM statics WHERE id=1280",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(path.as_deref(), replacement);
                let other_radius: f32 = conn
                    .query_row(
                        "SELECT radius FROM room_model_bounds WHERE base_form_id=1281",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!((other_radius - 8.0).abs() < f32::EPSILON);
            }
        }
    }

    #[test]
    fn subset_base_model_change_invalidates_a_cell_without_model_bounds() -> Result<()> {
        let base = |path| {
            record(
                0x500,
                b"STAT",
                None,
                None,
                vec![(b"MODL".to_vec(), cstr(path))],
            )
        };
        let (conn, _) = export_rooms(vec![
            interior_cell(0x100),
            room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            reference(0x2001, 0x100, 0x500, [0.0; 3], [0.0; 3]),
            base("A.nif"),
        ]);
        conn.execute_batch("DROP TABLE room_model_bounds")?;
        let complete = || {
            conn.query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=0x100",
                [],
                |row| row.get::<_, bool>(0),
            )
        };
        assert!(complete()?);
        export_to_db(&conn, &HashMap::from([(0x500, base("B.nif"))]))?;
        let path: String =
            conn.query_row("SELECT model_path FROM statics WHERE id=0x500", [], |row| {
                row.get(0)
            })?;
        assert_eq!(path, "B.nif");
        assert!(!complete()?);
        Ok(())
    }

    #[test]
    fn subset_reference_updates_invalidate_room_membership_and_affected_cells() {
        for destination in [0x100, 0x200] {
            let (conn, _) = export_rooms(vec![
                interior_cell(0x100),
                interior_cell(0x200),
                interior_cell(0x300),
                room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
                room(
                    0x1002,
                    0x100,
                    [100.0, 0.0, 0.0],
                    [0.0; 3],
                    [10.0; 3],
                    vec![],
                ),
                room(
                    0x1003,
                    0x200,
                    [100.0, 0.0, 0.0],
                    [0.0; 3],
                    [10.0; 3],
                    vec![],
                ),
                room(0x1004, 0x300, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
                reference(0x2001, 0x100, 0x500, [0.0; 3], [0.0; 3]),
                reference(0x2002, 0x100, 0x500, [0.0; 3], [0.0; 3]),
            ]);
            assert_eq!(rooms_of(&conn, 0x2001), vec![(0x1001, 1)]);
            let moved = reference(0x2001, destination, 0x500, [100.0, 0.0, 0.0], [0.0; 3]);
            export_to_db(&conn, &HashMap::from([(moved.form_id, moved)])).unwrap();
            assert!(rooms_of(&conn, 0x2001).is_empty());
            assert_eq!(rooms_of(&conn, 0x2002), vec![(0x1001, 1)]);
            for cell in [0x100, destination] {
                let complete: bool = conn
                    .query_row(
                        "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
                        [cell],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(!complete);
            }
            let unrelated: bool = conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=0x300",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(unrelated);
        }
    }

    #[test]
    fn raw_load_order_ignores_deleted_room_markers() {
        let mut deleted = room(0x1001, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]);
        deleted.flags = 0x20;
        let mut missing_transform = room(0x1004, 0x200, [0.0; 3], [0.0; 3], [10.0; 3], vec![]);
        missing_transform
            .subrecords
            .retain(|(tag, _)| tag != b"DATA");
        let records = [
            interior_cell(0x100),
            interior_cell(0x200),
            deleted,
            room(0x1002, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            room(0x1003, 0x200, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            missing_transform,
        ]
        .into_iter()
        .map(|record| (record.form_id, record))
        .collect();
        let order = LoadOrder::from_metadata(
            vec!["Skyrim.esm".into()],
            vec![crate::esm::binary::PluginMetadata::default()],
        )
        .unwrap();
        let conn = Connection::open_in_memory().unwrap();
        export_to_db_with_load_order(&conn, &records, &order).unwrap();
        let projected = conn
            .prepare(
                "SELECT b.ref_id,g.complete FROM room_bounds b
                 JOIN room_graph_cells g USING (cell_id) ORDER BY b.ref_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, bool>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(projected, [(0x1002, true), (0x1003, false)]);
    }

    #[test]
    fn subset_placed_update_accepts_the_previous_room_schema() {
        let (conn, _) = export_rooms(vec![
            interior_cell(0x100),
            reference(0x2001, 0x100, 0x500, [0.0; 3], [0.0; 3]),
            reference(0x2002, 0x100, 0x500, [0.0; 3], [0.0; 3]),
        ]);
        let mut deleted = reference(0x2002, 0x100, 0x500, [0.0; 3], [0.0; 3]);
        deleted.flags = 0x20;
        export_to_db(&conn, &HashMap::from([(deleted.form_id, deleted.clone())])).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM room_reference_rules WHERE reference_id=0x2002",
                [],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
        conn.execute_batch(
            "DROP TABLE room_occluder_links; DROP TABLE room_occluders; DROP TABLE room_reference_rules;",
        )
        .unwrap();
        let updated = reference(0x2001, 0x100, 0x500, [12.0, 0.0, 0.0], [0.0; 3]);
        export_to_db(&conn, &HashMap::from([(updated.form_id, updated)])).unwrap();
        let pos_x: f32 = conn
            .query_row(
                "SELECT pos_x FROM [references] WHERE id=0x2001",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!((pos_x - 12.0).abs() < f32::EPSILON);
        deleted.form_id = 0x2001;
        export_to_db(&conn, &HashMap::from([(deleted.form_id, deleted)])).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM [references] WHERE id=0x2001",
                [],
                |row| { row.get::<_, u32>(0) }
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn subset_occluder_changes_invalidate_old_and_new_cross_cell_targets() {
        let occluder = |id, cell, target| {
            let mut extra = vec![(b"XPRM", xprm([10.0; 3], 3))];
            if let Some(target) = target {
                extra.push((b"XORD", [word(0), word(target), word(0), word(0)].concat()));
            }
            marker(
                id,
                cell,
                rooms::OCCLUSION_MARKER_BASE,
                [0.0; 3],
                [0.0; 3],
                extra,
            )
        };
        let (conn, _) = export_rooms(vec![
            interior_cell(0x100),
            interior_cell(0x200),
            interior_cell(0x300),
            interior_cell(0x400),
            occluder(0x1001, 0x100, Some(0x2001)),
            occluder(0x2001, 0x200, None),
            occluder(0x3001, 0x300, None),
            occluder(0x4001, 0x400, None),
        ]);
        let complete = |cell| {
            conn.query_row(
                "SELECT complete FROM room_graph_cells WHERE cell_id=?1",
                [cell],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
        };
        assert!([0x100, 0x200, 0x300, 0x400].into_iter().all(complete));
        let updated = HashMap::from([(0x1001, occluder(0x1001, 0x100, Some(0x3001)))]);
        let linked_target = || {
            conn.query_row(
                "SELECT link_index,linked_ref FROM room_occluder_links WHERE ref_id=0x1001",
                [],
                |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
            )
            .unwrap()
        };
        conn.execute_batch(
            "CREATE TEMP TRIGGER fail_subset BEFORE INSERT ON records WHEN NEW.form_id=0x1001
             BEGIN SELECT RAISE(ABORT, 'placement insert failed'); END;",
        )
        .unwrap();
        let error = export_to_db(&conn, &updated).unwrap_err();
        assert!(error.to_string().contains("placement insert failed"));
        assert_eq!(linked_target(), (1, 0x2001));
        assert!([0x100, 0x200, 0x300, 0x400].into_iter().all(complete));

        conn.execute_batch("DROP TRIGGER fail_subset").unwrap();
        export_to_db(&conn, &updated).unwrap();
        assert_eq!(linked_target(), (1, 0x3001));
        for cell in [0x100, 0x200, 0x300] {
            assert!(!complete(cell), "cell {cell:08X} stayed complete");
        }
        assert!(complete(0x400));
    }

    #[test]
    fn dangling_authored_room_links_mark_only_their_cell_incomplete() {
        for target in [0x9999, 0x3001] {
            let (conn, _) = export_rooms(vec![
                interior_cell(0x100),
                interior_cell(0x200),
                room(
                    0x1001,
                    0x100,
                    [0.0; 3],
                    [0.0; 3],
                    [10.0; 3],
                    vec![(b"XRMR", vec![1, 0, 1, 0]), (b"XLRM", word(target))],
                ),
                room(
                    0x1002,
                    0x100,
                    [100.0, 0.0, 0.0],
                    [0.0; 3],
                    [10.0; 3],
                    vec![],
                ),
                room(0x3001, 0x200, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            ]);
            let complete: bool = conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=0x100",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!complete);
            let unrelated: bool = conn
                .query_row(
                    "SELECT complete FROM room_graph_cells WHERE cell_id=0x200",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(unrelated);
            let stored: u32 = conn
                .query_row(
                    "SELECT linked_ref FROM room_links WHERE room_ref=0x1001",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(stored, target);
        }
    }

    #[test]
    fn skips_markers_with_non_finite_sizes() {
        let (conn, report) = export_rooms(vec![
            interior_cell(0x100),
            room(
                0x1001,
                0x100,
                [0.0; 3],
                [0.0; 3],
                [f32::NAN, 1.0, 1.0],
                vec![],
            ),
            room(
                0x1002,
                0x100,
                [0.0; 3],
                [0.0; 3],
                [f32::INFINITY, 1.0, 1.0],
                vec![],
            ),
            room(0x1003, 0x100, [0.0; 3], [0.0; 3], [10.0; 3], vec![]),
            marker(
                0x3001,
                0x100,
                PORTAL_MARKER_BASE,
                [0.0; 3],
                [0.0; 3],
                vec![(b"XPRM", xprm([f32::NAN, 1.0, 60.0], 3))],
            ),
            portal(0x3002, 0x100, Some((0x1003, 0))),
        ]);
        assert_eq!(report.rooms, 1);
        assert_eq!(report.portals, 1);
        assert_eq!(report.invalid_markers_skipped, 3);
        let read = rooms::RoomGraphReport::read(&conn).unwrap();
        assert_eq!(read.invalid_markers_skipped, 3);
    }

    /// A room turned 0.6 rad about Z with an asymmetric box: probe points on its long axis are
    /// inside only under the right inverse. The forward matrix, or the opposite turn, puts the
    /// same points outside.
    #[test]
    fn a_room_turned_by_an_arbitrary_yaw_uses_the_inverse_rotation() {
        let (sin, cos) = 0.6f32.sin_cos();
        // Local +X in world space is (cos, -sin) for a Creation yaw of +0.6 (clockwise).
        let along = |distance: f32| [distance * cos, -distance * sin, 0.0];
        let mirrored = |distance: f32| [distance * cos, distance * sin, 0.0];
        let beside = {
            let on_axis = along(100.0);
            // 30 from the axis across the narrow side (half 20).
            [on_axis[0] + 30.0 * sin, on_axis[1] + 30.0 * cos, 0.0]
        };
        let (conn, _) = export_rooms(vec![
            interior_cell(0x100),
            room(
                0x1001,
                0x100,
                [0.0; 3],
                [0.0, 0.0, 0.6],
                [200.0, 20.0, 50.0],
                vec![],
            ),
            reference(0x2001, 0x100, 0x500, along(150.0), [0.0; 3]),
            reference(0x2002, 0x100, 0x500, along(190.0), [0.0; 3]),
            reference(0x2003, 0x100, 0x500, along(210.0), [0.0; 3]),
            reference(0x2004, 0x100, 0x500, mirrored(150.0), [0.0; 3]),
            reference(0x2005, 0x100, 0x500, beside, [0.0; 3]),
        ]);
        assert_eq!(rooms_of(&conn, 0x2001), vec![(0x1001, 1)]);
        assert_eq!(rooms_of(&conn, 0x2002), vec![(0x1001, 1)]);
        assert_eq!(rooms_of(&conn, 0x2003), vec![]);
        assert_eq!(rooms_of(&conn, 0x2004), vec![]);
        assert_eq!(rooms_of(&conn, 0x2005), vec![]);
    }
}
