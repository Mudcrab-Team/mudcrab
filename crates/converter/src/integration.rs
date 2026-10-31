//! Final, asset-aware validation performed after all offline conversions.

use crate::{
    asset_path::{AssetKind, canonical_asset_path},
    esm::projection_omissions::ProjectionOmissions,
    mesh::glb_bounds_from_bytes,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

const MAX_REPORTED_ISSUES: usize = 100;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntegrationReport {
    pub schema_version: u32,
    pub statics_total: u64,
    pub statics_with_models: u64,
    pub bounds_updated: u64,
    pub references_total: u64,
    pub exterior_cells: u64,
    pub terrain_cells: u64,
    pub cache_cells: u64,
    pub texture_sets_with_diffuse: u64,
    pub waters_with_flow_normal: u64,
    pub missing_model_count: u64,
    pub invalid_model_count: u64,
    pub unavailable_model_source_count: u64,
    pub unbounded_model_count: u64,
    /// Interior room graph counters (db-schema.md section 15); all zero without room data.
    #[serde(default)]
    pub room_graph: crate::esm::rooms::RoomGraphReport,
    /// Plugin rows the light and emittance projections left out (db-schema.md section 18);
    /// empty when nothing was left out or the database predates the tables.
    #[serde(default, skip_serializing_if = "ProjectionOmissions::is_empty")]
    pub projection_omissions: ProjectionOmissions,
    pub missing_texture_count: u64,
    pub unavailable_texture_source_count: u64,
    pub issues: Vec<String>,
    pub passed: bool,
}

pub fn finalize_world_database(staging: &Path) -> Result<Option<IntegrationReport>> {
    let sources = source_file_index(staging)?;
    finalize_world_database_with_sources(staging, &sources)
}

/// Audits against the merged winning source index, including loose files read outside staging.
pub fn finalize_world_database_with_sources(
    staging: &Path,
    sources: &HashMap<String, PathBuf>,
) -> Result<Option<IntegrationReport>> {
    let database_path = staging.join("skyrim_world.db");
    if !database_path.is_file() {
        return Ok(None);
    }
    let mut connection = Connection::open(&database_path)?;
    let mut report = IntegrationReport {
        schema_version: connection.query_row(
            "SELECT version FROM schema_info LIMIT 1",
            [],
            |row| row.get(0),
        )?,
        statics_total: count(&connection, "SELECT count(*) FROM statics")?,
        statics_with_models: count(
            &connection,
            "SELECT count(*) FROM statics WHERE model_path IS NOT NULL AND model_path <> ''",
        )?,
        references_total: count(&connection, "SELECT count(*) FROM \"references\"")?,
        exterior_cells: count(
            &connection,
            "SELECT count(*) FROM cells WHERE worldspace_id IS NOT NULL AND grid_x IS NOT NULL AND grid_y IS NOT NULL",
        )?,
        terrain_cells: count(&connection, "SELECT count(*) FROM land")?,
        texture_sets_with_diffuse: count(
            &connection,
            "SELECT count(*) FROM texture_sets WHERE diffuse_path IS NOT NULL AND diffuse_path <> ''",
        )?,
        waters_with_flow_normal: count(
            &connection,
            "SELECT count(*) FROM waters WHERE flow_normal_path IS NOT NULL AND flow_normal_path <> ''",
        )?,
        ..Default::default()
    };
    let files = converted_file_index(staging)?;
    let static_models = {
        let mut statement = connection.prepare(
            "SELECT s.id,s.model_path,r.record_type,p.name
             FROM statics s
             LEFT JOIN records r ON r.form_id=s.id
             LEFT JOIN (
                 SELECT priority,
                        CASE WHEN COUNT(*)=1 THEN MIN(name) ELSE NULL END AS name
                 FROM plugins
                 GROUP BY priority
             ) p ON p.priority=r.load_order
             WHERE s.model_path IS NOT NULL AND s.model_path <> ''
             ORDER BY s.id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    // Several records can name one model. The staged files are immutable during
    // this pass, so both successful bounds and errors can be shared by key.
    let mut model_bounds = HashMap::new();
    let transaction = connection.transaction()?;
    for (form_id, model_path, record_type, plugin) in static_models {
        let key = match converted_key(&model_path, "meshes", "glb") {
            Ok(key) => key,
            Err(error) => {
                recover_invalid_static_model(
                    &transaction,
                    &mut report,
                    form_id,
                    &model_path,
                    record_type.as_deref(),
                    plugin.as_deref(),
                    error,
                )?;
                continue;
            }
        };
        let Some(path) = files.get(&key) else {
            let source_key = match converted_key(&model_path, "meshes", "nif") {
                Ok(key) => key,
                Err(error) => {
                    recover_invalid_static_model(
                        &transaction,
                        &mut report,
                        form_id,
                        &model_path,
                        record_type.as_deref(),
                        plugin.as_deref(),
                        error,
                    )?;
                    continue;
                }
            };
            if sources.contains_key(&source_key) {
                report.missing_model_count += 1;
                issue(
                    &mut report,
                    format!("missing converted model {model_path} for {form_id:08X}"),
                );
            } else {
                report.unavailable_model_source_count += 1;
            }
            continue;
        };
        let bounds = model_bounds
            .entry(key)
            .or_insert_with(|| read_glb_bounds(path).map_err(|error| format!("{error:#}")));
        match bounds {
            Ok(bounds) => {
                transaction.execute(
                    "UPDATE statics SET bounds_min_x=?1,bounds_min_y=?2,bounds_min_z=?3,bounds_max_x=?4,bounds_max_y=?5,bounds_max_z=?6,bounds_valid=1 WHERE id=?7",
                    params![bounds.min[0], bounds.min[1], bounds.min[2], bounds.max[0], bounds.max[1], bounds.max[2], form_id],
                )?;
                report.bounds_updated += 1;
            }
            Err(error) => {
                report.unbounded_model_count += 1;
                issue(
                    &mut report,
                    format!("model has no static bounds {model_path}: {error:#}"),
                );
            }
        }
    }
    transaction.commit()?;
    report.statics_with_models = count(
        &connection,
        "SELECT count(*) FROM statics WHERE model_path IS NOT NULL AND model_path <> ''",
    )?;
    // Room membership by model bounds needs the bounds just written.
    crate::esm::rooms::add_overlap_rows(&mut connection)?;
    report.room_graph = crate::esm::rooms::RoomGraphReport::read(&connection)?;
    report.projection_omissions = ProjectionOmissions::read(&connection)?;

    let diffuse_paths = {
        let mut statement = connection.prepare(
            "SELECT id,diffuse_path FROM texture_sets WHERE diffuse_path IS NOT NULL AND diffuse_path <> '' ORDER BY id",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (form_id, texture_path) in diffuse_paths {
        let key = converted_key(&texture_path, "textures", "ktx2")?;
        if !files.contains_key(&key) {
            let source_key = converted_key(&texture_path, "textures", "dds")?;
            if sources.contains_key(&source_key) {
                report.missing_texture_count += 1;
                issue(
                    &mut report,
                    format!(
                        "missing converted diffuse texture {texture_path} for TXST {form_id:08X}"
                    ),
                );
            } else {
                report.unavailable_texture_source_count += 1;
            }
        }
    }
    let flow_paths = {
        let mut statement = connection.prepare(
            "SELECT id,flow_normal_path FROM waters WHERE flow_normal_path IS NOT NULL AND flow_normal_path <> '' ORDER BY id",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (form_id, texture_path) in flow_paths {
        let key = converted_key(&texture_path, "textures", "ktx2")?;
        if !files.contains_key(&key) {
            let source_key = converted_key(&texture_path, "textures", "dds")?;
            if sources.contains_key(&source_key) {
                report.missing_texture_count += 1;
                issue(
                    &mut report,
                    format!(
                        "missing converted flow-normal texture {texture_path} for WATR {form_id:08X}"
                    ),
                );
            } else {
                report.unavailable_texture_source_count += 1;
            }
        }
    }
    let cache_path = staging.join("cell_cache.rkyv");
    if cache_path.is_file() {
        let mmap = crate::esm::cell_cache::validate_cell_cache(&cache_path)?;
        let cache = rkyv::access::<shared::ArchivedCellCache, rkyv::rancor::Error>(&mmap)
            .wrap_err("invalid integration cell cache")?;
        report.cache_cells = cache.cells.len() as u64;
        if report.cache_cells != report.terrain_cells {
            let database_cells = report.terrain_cells;
            let cache_cells = report.cache_cells;
            issue(
                &mut report,
                format!(
                    "terrain/cache cell count mismatch: database={}, cache={}",
                    database_cells, cache_cells
                ),
            );
        }
    } else {
        issue(&mut report, "missing cell_cache.rkyv".to_owned());
    }
    report.passed = report.schema_version == shared::WORLD_DATABASE_SCHEMA_VERSION
        && report.missing_model_count == 0
        && report.invalid_model_count == 0
        && report.missing_texture_count == 0
        && report.cache_cells == report.terrain_cells;
    let output = staging.join("integration-report.json");
    // Staged outputs may share an inode with a previous pack via
    // hard link; replace the path instead of writing through it.
    if output.is_file() {
        fs::remove_file(&output)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)
        .wrap_err_with(|| format!("failed to write {}", output.display()))?;
    Ok(Some(report))
}

fn source_file_index(staging: &Path) -> Result<HashMap<String, PathBuf>> {
    let root = staging.join("vfs");
    if !root.is_dir() {
        return Ok(HashMap::new());
    }
    asset_file_index(&root, &["meshes", "textures"])
}

fn converted_file_index(staging: &Path) -> Result<HashMap<String, PathBuf>> {
    asset_file_index(staging, &["meshes", "textures", "scripts"])
}

fn asset_file_index(root: &Path, folders: &[&str]) -> Result<HashMap<String, PathBuf>> {
    let mut files = HashMap::new();
    // Only runtime asset folders participate in integration. Walking all of
    // staging also visited source files, ingestion blobs and unrelated metadata.
    // Match folder names without case sensitivity, as the indexed keys do.
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_name().to_str().is_some_and(|name| {
            folders
                .iter()
                .any(|folder| name.eq_ignore_ascii_case(folder))
        }) || !entry.file_type()?.is_dir()
        {
            continue;
        }
        for entry in WalkDir::new(entry.path()).follow_links(false) {
            let entry = entry?;
            if entry.file_type().is_file() {
                let relative = entry.path().strip_prefix(root)?;
                files.insert(normalize(relative), entry.into_path());
            }
        }
    }
    Ok(files)
}

fn read_glb_bounds(path: &Path) -> Result<shared::Bounds3> {
    let mut file =
        fs::File::open(path).wrap_err_with(|| format!("failed to open {}", path.display()))?;
    let file_length = file.metadata()?.len();
    read_glb_bounds_from_reader(&mut file, file_length)
        .wrap_err_with(|| format!("failed to extract bounds from {}", path.display()))
}

fn read_glb_bounds_from_reader(
    reader: &mut impl Read,
    file_length: u64,
) -> Result<shared::Bounds3> {
    let mut header = [0u8; 20];
    reader
        .read_exact(&mut header)
        .wrap_err("invalid GLB container")?;
    ensure!(&header[..4] == b"glTF", "invalid GLB container");
    ensure!(&header[16..20] == b"JSON", "GLB JSON chunk is missing");
    let json_length = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    ensure!(
        u64::from(json_length) <= file_length.saturating_sub(header.len() as u64),
        "truncated GLB JSON chunk"
    );
    let prefix_length = header
        .len()
        .checked_add(json_length as usize)
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB JSON chunk length overflow"))?;
    let mut prefix = Vec::with_capacity(prefix_length);
    prefix.extend_from_slice(&header);
    prefix.resize(prefix_length, 0);
    reader
        .read_exact(&mut prefix[header.len()..])
        .wrap_err("truncated GLB JSON chunk")?;
    glb_bounds_from_bytes(&prefix)
}

fn converted_key(source: &str, kind: &str, extension: &str) -> Result<String> {
    let kind = match kind {
        "meshes" => AssetKind::Mesh,
        "textures" => AssetKind::Texture,
        "scripts" => AssetKind::Script,
        _ => color_eyre::eyre::bail!("unsupported asset kind: {kind}"),
    };
    canonical_asset_path(source, kind, extension)
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn count(connection: &Connection, sql: &str) -> Result<u64> {
    Ok(connection.query_row(sql, [], |row| row.get(0))?)
}

fn issue(report: &mut IntegrationReport, message: String) {
    if report.issues.len() < MAX_REPORTED_ISSUES {
        report.issues.push(message);
    }
}

fn recover_invalid_static_model(
    transaction: &rusqlite::Transaction<'_>,
    report: &mut IntegrationReport,
    form_id: u32,
    model_path: &str,
    record_type: Option<&str>,
    plugin: Option<&str>,
    error: impl std::fmt::Display,
) -> Result<()> {
    transaction.execute("UPDATE statics SET model_path=NULL WHERE id=?1", [form_id])?;
    let owner = match (plugin.filter(|name| !name.is_empty()), record_type) {
        (Some(plugin), Some(record_type)) => {
            format!("plugin {plugin} {record_type} {form_id:08X}")
        }
        (Some(plugin), None) => format!("plugin {plugin} record {form_id:08X}"),
        (None, Some(record_type)) => format!("{record_type} {form_id:08X}"),
        (None, None) => format!("record {form_id:08X}"),
    };
    issue(
        report,
        format!("recovered invalid static model projection for {owner}: {model_path:?}: {error}"),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glb_with_json(mut json: Vec<u8>, binary: &[u8]) -> Vec<u8> {
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total = 20
            + json.len()
            + if binary.is_empty() {
                0
            } else {
                8 + binary.len()
            };
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        if !binary.is_empty() {
            glb.extend_from_slice(&(binary.len() as u32).to_le_bytes());
            glb.extend_from_slice(b"BIN\0");
            glb.extend_from_slice(binary);
        }
        glb
    }

    fn bounded_glb(min: [f32; 3], max: [f32; 3], translation: [f32; 3]) -> Vec<u8> {
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0, "translation": translation}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"min": min, "max": max}],
        });
        glb_with_json(serde_json::to_vec(&document).unwrap(), &[0; 4096])
    }

    fn write_file(directory: &Path, relative: &str, bytes: &[u8]) {
        let path = directory.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn empty_cache(directory: &Path) {
        let cache = shared::CellCache {
            version: shared::CELL_CACHE_VERSION,
            cells: vec![],
        };
        fs::write(
            directory.join("cell_cache.rkyv"),
            rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn maps_creation_paths_to_converted_assets() {
        assert_eq!(
            converted_key("Meshes\\Architecture\\Wall.NIF", "meshes", "glb").unwrap(),
            "meshes/architecture/wall.glb"
        );
        assert_eq!(
            converted_key("land/grass.dds", "textures", "ktx2").unwrap(),
            "textures/land/grass.ktx2"
        );
    }

    #[test]
    fn indexes_runtime_assets_without_staging_workspace() {
        let directory = tempfile::tempdir().unwrap();
        for relative in [
            "Meshes/Architecture/Wall.GLB",
            "Textures/Land/Rock.KTX2",
            "Scripts/Test.LUAU",
            "vfs/Meshes/Architecture/Wall.NIF",
            "vfs/Textures/Land/Rock.DDS",
            "vfs/sound/voice/test.fuz",
            ".ingestion-cache/sha256/ab/blob",
            "stale-metadata/meshes/stale.glb",
        ] {
            write_file(directory.path(), relative, b"asset");
        }
        let converted = converted_file_index(directory.path()).unwrap();
        assert_eq!(converted.len(), 3);
        for key in [
            "meshes/architecture/wall.glb",
            "textures/land/rock.ktx2",
            "scripts/test.luau",
        ] {
            assert!(converted.contains_key(key), "missing runtime asset {key}");
        }
        let sources = source_file_index(directory.path()).unwrap();
        assert_eq!(sources.len(), 2);
        assert!(sources.contains_key("meshes/architecture/wall.nif"));
        assert!(sources.contains_key("textures/land/rock.dds"));
    }

    #[test]
    fn reads_bounds_without_reading_glb_binary_payload() {
        let glb = bounded_glb([-2.0, -3.0, -4.0], [5.0, 6.0, 7.0], [3.0, 4.0, 5.0]);
        let json_length = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let mut reader = std::io::Cursor::new(&glb);
        let bounds = read_glb_bounds_from_reader(&mut reader, glb.len() as u64).unwrap();
        assert_eq!(bounds.min, [1.0, 1.0, 1.0]);
        assert_eq!(bounds.max, [8.0, 10.0, 12.0]);
        assert_eq!(reader.position(), (20 + json_length) as u64);
        assert!(reader.position() < glb.len() as u64);
    }

    #[test]
    fn rejects_invalid_or_truncated_glb_json() {
        let valid = bounded_glb([0.0; 3], [1.0; 3], [0.0; 3]);
        let json_end = 20 + u32::from_le_bytes(valid[12..16].try_into().unwrap()) as usize;
        assert!(
            read_glb_bounds_from_reader(&mut std::io::Cursor::new(&valid), valid.len() as u64,)
                .is_ok()
        );
        let mut bad_magic = valid.clone();
        bad_magic[..4].copy_from_slice(b"nope");
        let mut bad_chunk = valid.clone();
        bad_chunk[16..20].copy_from_slice(b"BIN\0");
        let mut bad_json = valid.clone();
        bad_json[20] = b'?';
        let mut impossible_length = valid.clone();
        impossible_length[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        for bytes in [
            valid[..19].to_vec(),
            valid[..json_end - 1].to_vec(),
            bad_magic,
            bad_chunk,
            bad_json,
            impossible_length,
        ] {
            assert!(
                read_glb_bounds_from_reader(&mut std::io::Cursor::new(&bytes), bytes.len() as u64)
                    .is_err()
            );
        }
    }

    #[test]
    fn enriches_each_record_when_models_are_shared_or_different() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("skyrim_world.db");
        let connection = Connection::open(&database).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        for (id, model) in [
            (1, "architecture/wall.nif"),
            (2, r"Meshes\Architecture\WALL.NIF"),
            (3, "architecture/other.nif"),
        ] {
            connection
                .execute(
                    "INSERT INTO statics(id,model_path,flags) VALUES(?1,?2,0)",
                    params![id, model],
                )
                .unwrap();
        }
        drop(connection);
        write_file(
            directory.path(),
            "Meshes/Architecture/Wall.GLB",
            &bounded_glb([-2.0, -3.0, -4.0], [5.0, 6.0, 7.0], [3.0, 4.0, 5.0]),
        );
        write_file(
            directory.path(),
            "Meshes/Architecture/Other.GLB",
            &bounded_glb([0.0, 0.0, 0.0], [1.0, 2.0, 3.0], [20.0, 30.0, 40.0]),
        );
        empty_cache(directory.path());
        let report = finalize_world_database(directory.path()).unwrap().unwrap();
        assert!(report.passed);
        assert_eq!(report.bounds_updated, 3);
        let connection = Connection::open(database).unwrap();
        let mut statement = connection
            .prepare("SELECT id,bounds_min_x,bounds_max_z,bounds_valid FROM statics ORDER BY id")
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, f32>(1)?,
                    row.get::<_, f32>(2)?,
                    row.get::<_, i32>(3)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            [(1, 1.0, 12.0, 1), (2, 1.0, 12.0, 1), (3, 20.0, 43.0, 1)]
        );
    }

    #[test]
    fn recovers_unsafe_static_paths_without_dropping_valid_neighbors() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("skyrim_world.db");
        let connection = Connection::open(&database).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO plugins(id,name,priority,checksum) VALUES(1,'Synthetic.esp',0,?1)",
                [vec![0u8]],
            )
            .unwrap();
        let rows = [
            (1u32, "STAT", "architecture/wall.nif"),
            (2, "TACT", "../bad.nif"),
            (3, "ADDN", "/meshes/bad.nif"),
            (4, "APPA", r"..\bad.nif"),
            (5, "ARTO", r"C:\Meshes\bad.nif"),
            (6, "STAT", "architecture/wall.nif"),
        ];
        for (id, record_type, model_path) in rows {
            connection
                .execute(
                    "INSERT INTO statics(id,model_path,flags) VALUES(?1,?2,0)",
                    params![id, model_path],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO records(form_id,record_type,load_order,data) VALUES(?1,?2,0,?3)",
                    params![id, record_type, vec![id as u8, 0xA5]],
                )
                .unwrap();
        }
        drop(connection);
        write_file(
            directory.path(),
            "Meshes/Architecture/Wall.GLB",
            &bounded_glb([-2.0, -3.0, -4.0], [5.0, 6.0, 7.0], [3.0, 4.0, 5.0]),
        );
        empty_cache(directory.path());

        let report = finalize_world_database(directory.path()).unwrap().unwrap();

        assert!(report.passed, "{:?}", report.issues);
        assert_eq!(report.bounds_updated, 2);
        assert_eq!(report.statics_total, 6);
        assert_eq!(report.statics_with_models, 2);
        assert_eq!(report.issues.len(), 4);
        for (record_type, form_id, source_path) in [
            ("TACT", 2, "../bad.nif"),
            ("ADDN", 3, "/meshes/bad.nif"),
            ("APPA", 4, r"..\bad.nif"),
            ("ARTO", 5, r"C:\Meshes\bad.nif"),
        ] {
            let diagnostic = report
                .issues
                .iter()
                .find(|issue| issue.contains(&format!("{record_type} {form_id:08X}")))
                .unwrap();
            assert!(diagnostic.contains("plugin Synthetic.esp"));
            assert!(diagnostic.contains(&format!("{source_path:?}")));
        }

        let connection = Connection::open(database).unwrap();
        let projections: Vec<(u32, Option<String>)> = connection
            .prepare("SELECT id,model_path FROM statics ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            projections,
            [
                (1, Some("architecture/wall.nif".into())),
                (2, None),
                (3, None),
                (4, None),
                (5, None),
                (6, Some("architecture/wall.nif".into())),
            ]
        );
        let archived: Vec<(u32, Vec<u8>)> = connection
            .prepare("SELECT form_id,data FROM records ORDER BY form_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            archived,
            rows.iter()
                .map(|(id, _, _)| (*id, vec![*id as u8, 0xA5]))
                .collect::<Vec<_>>()
        );
        let published: IntegrationReport = serde_json::from_slice(
            &fs::read(directory.path().join("integration-report.json")).unwrap(),
        )
        .unwrap();
        assert!(published.passed);
        assert_eq!(published.issues, report.issues);
    }

    #[test]
    fn duplicate_plugin_priorities_do_not_duplicate_static_integration_rows() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("skyrim_world.db");
        let connection = Connection::open(&database).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute_batch(
                "INSERT INTO plugins(id,name,priority,checksum)
                     VALUES(1,'First.esm',0,X''),(2,'Second.esm',0,X'');
                 INSERT INTO statics(id,model_path,flags) VALUES
                     (1,'architecture/wall.nif',0),
                     (2,'../unsafe.nif',0),
                     (3,'architecture/unavailable.nif',0),
                     (4,'../without-plugin.nif',0);
                 INSERT INTO records(form_id,record_type,load_order,data) VALUES
                     (1,'STAT',0,X'01'),
                     (2,'TACT',0,X'02'),
                     (3,'ADDN',0,X'03'),
                     (4,'APPA',7,X'04');",
            )
            .unwrap();
        drop(connection);
        write_file(
            directory.path(),
            "Meshes/Architecture/Wall.GLB",
            &bounded_glb([-2.0, -3.0, -4.0], [5.0, 6.0, 7.0], [3.0, 4.0, 5.0]),
        );
        empty_cache(directory.path());

        let report = finalize_world_database(directory.path()).unwrap().unwrap();

        assert!(report.passed, "{:?}", report.issues);
        assert_eq!(report.statics_total, 4);
        assert_eq!(report.bounds_updated, 1);
        assert_eq!(report.missing_model_count, 0);
        assert_eq!(report.unavailable_model_source_count, 1);
        assert_eq!(report.statics_with_models, 2);
        assert_eq!(report.issues.len(), 2);
        let ambiguous_priority_diagnostic = report
            .issues
            .iter()
            .find(|issue| issue.contains("TACT 00000002"))
            .unwrap();
        assert!(ambiguous_priority_diagnostic.contains("../unsafe.nif"));
        assert!(!ambiguous_priority_diagnostic.contains("First.esm"));
        assert!(!ambiguous_priority_diagnostic.contains("Second.esm"));
        assert_eq!(
            report
                .issues
                .iter()
                .filter(|issue| issue.contains("TACT 00000002"))
                .count(),
            1
        );
        let absent_metadata_diagnostic = report
            .issues
            .iter()
            .find(|issue| issue.contains("APPA 00000004"))
            .unwrap();
        assert!(absent_metadata_diagnostic.contains("../without-plugin.nif"));
        assert!(!absent_metadata_diagnostic.contains("plugin "));

        let connection = Connection::open(database).unwrap();
        let projections: Vec<(u32, Option<String>)> = connection
            .prepare("SELECT id,model_path FROM statics ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            projections,
            [
                (1, Some("architecture/wall.nif".into())),
                (2, None),
                (3, Some("architecture/unavailable.nif".into())),
                (4, None),
            ]
        );
        let archived: Vec<(u32, Vec<u8>)> = connection
            .prepare("SELECT form_id,data FROM records ORDER BY form_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            archived,
            [(1, vec![1]), (2, vec![2]), (3, vec![3]), (4, vec![4])]
        );
        let published: IntegrationReport = serde_json::from_slice(
            &fs::read(directory.path().join("integration-report.json")).unwrap(),
        )
        .unwrap();
        assert!(published.passed);
        assert_eq!(published.bounds_updated, 1);
        assert_eq!(published.unavailable_model_source_count, 1);
        assert_eq!(published.issues, report.issues);
    }

    #[test]
    fn keeps_source_availability_classification_for_models_and_textures() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection.execute_batch(
            "INSERT INTO statics(id,model_path,flags) VALUES(1,'test/lost.nif',0),(2,'test/absent.nif',0);
             INSERT INTO texture_sets(id,diffuse_path) VALUES(3,'land/lost.dds'),(4,'land/absent.dds');",
        ).unwrap();
        drop(connection);
        write_file(
            directory.path(),
            "vfs/Meshes/Test/Lost.NIF",
            b"source model",
        );
        write_file(
            directory.path(),
            "vfs/Textures/Land/Lost.DDS",
            b"source texture",
        );
        empty_cache(directory.path());
        let report = finalize_world_database(directory.path()).unwrap().unwrap();
        assert!(!report.passed);
        assert_eq!(report.missing_model_count, 1);
        assert_eq!(report.unavailable_model_source_count, 1);
        assert_eq!(report.missing_texture_count, 1);
        assert_eq!(report.unavailable_texture_source_count, 1);
    }

    #[test]
    fn enriches_database_with_real_glb_bounds() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("skyrim_world.db");
        let connection = Connection::open(&database).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO statics(id,model_path,flags) VALUES(1,'architecture/wall.nif',0)",
                [],
            )
            .unwrap();
        drop(connection);
        let mesh_path = directory.path().join("meshes/architecture/wall.glb");
        fs::create_dir_all(mesh_path.parent().unwrap()).unwrap();
        let mut json = br#"{
            "asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],
            "nodes":[{"mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"min":[-2,-3,-4],"max":[5,6,7]}]
        }"#
        .to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total = 20 + json.len();
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        fs::write(mesh_path, glb).unwrap();
        empty_cache(directory.path());

        let report = finalize_world_database(directory.path()).unwrap().unwrap();
        assert!(report.passed);
        assert_eq!(report.bounds_updated, 1);
        let connection = Connection::open(database).unwrap();
        let bounds: (f32, f32, i32) = connection
            .query_row(
                "SELECT bounds_min_x,bounds_max_z,bounds_valid FROM statics WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(bounds, (-2.0, 7.0, 1));
    }

    #[test]
    fn treats_undistributed_model_reference_as_coverage_metric() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO statics(id,model_path,flags) VALUES(1,'test/missing.nif',0)",
                [],
            )
            .unwrap();
        drop(connection);
        empty_cache(directory.path());

        let report = finalize_world_database(directory.path()).unwrap().unwrap();
        assert!(report.passed);
        assert_eq!(report.unavailable_model_source_count, 1);
        assert_eq!(report.missing_model_count, 0);
    }

    #[test]
    fn audits_winning_loose_model_and_texture_sources_outside_vfs() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO statics(id,model_path,flags) VALUES(1,'test/lost.nif',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO texture_sets(id,diffuse_path) VALUES(2,'test/lost.dds')",
                [],
            )
            .unwrap();
        drop(connection);
        empty_cache(directory.path());
        let loose = tempfile::tempdir().unwrap();
        fs::write(loose.path().join("lost.nif"), b"source").unwrap();
        fs::write(loose.path().join("lost.dds"), b"source").unwrap();
        let sources = HashMap::from([
            ("meshes/test/lost.nif".into(), loose.path().join("lost.nif")),
            (
                "textures/test/lost.dds".into(),
                loose.path().join("lost.dds"),
            ),
        ]);
        let report = finalize_world_database_with_sources(directory.path(), &sources)
            .unwrap()
            .unwrap();
        assert!(!report.passed);
        assert_eq!(report.missing_model_count, 1);
        assert_eq!(report.missing_texture_count, 1);
        assert_eq!(report.unavailable_model_source_count, 0);
        assert_eq!(report.unavailable_texture_source_count, 0);
    }

    #[test]
    fn fails_when_existing_model_source_has_no_converted_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("skyrim_world.db")).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO statics(id,model_path,flags) VALUES(1,'test/lost.nif',0)",
                [],
            )
            .unwrap();
        drop(connection);
        let source = directory.path().join("vfs/meshes/test/lost.nif");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(source, b"source exists").unwrap();
        empty_cache(directory.path());

        let report = finalize_world_database(directory.path()).unwrap().unwrap();
        assert!(!report.passed);
        assert_eq!(report.missing_model_count, 1);
        assert_eq!(report.unavailable_model_source_count, 0);
    }

    fn write_box_glb(path: &Path, min: [f32; 3], max: [f32; 3]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut json = format!(
            r#"{{
            "asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
            "nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
            "accessors":[{{"min":{min:?},"max":{max:?}}}]
        }}"#
        )
        .into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((20 + json.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        fs::write(path, glb).unwrap();
    }

    /// Objects whose pivot is outside a room but whose model reaches into it get a
    /// `contained = 0` row. The GLB bounds are glTF-axis (Y-up) numbers in Creation units:
    /// this model is 100 long along Creation X, 20 along Creation Y and 10 along Creation Z.
    #[test]
    fn adds_overlap_rows_from_model_bounds_in_creation_axes_and_units() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("skyrim_world.db");
        let connection = Connection::open(&database).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO statics(id,model_path,flags) VALUES(0x500,'architecture/wall.nif',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z)
                 VALUES(0x1001,0x100,0,0,0,0,0,0,100,100,100)",
                [],
            )
            .unwrap();
        let place = |id: u32, position: [f32; 3], rot_z: f32, scale: f32| {
            connection
                .execute(
                    "INSERT INTO \"references\"(id,cell_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale)
                     VALUES(?1,0x100,0x500,0,?2,?3,?4,0,0,?5,?6)",
                    rusqlite::params![id, position[0], position[1], position[2], rot_z, scale],
                )
                .unwrap();
        };
        let quarter_turn = std::f32::consts::FRAC_PI_2;
        // Pivot inside: a pivot row, never also an overlap row.
        place(0x2001, [10.0, 0.0, 0.0], 0.0, 1.0);
        connection
            .execute(
                "INSERT INTO reference_rooms(reference_id,room_ref,contained) VALUES(0x2001,0x1001,1)",
                [],
            )
            .unwrap();
        // Pivot outside on X; the 100-long side reaches in (x 90..190).
        place(0x2002, [140.0, 0.0, 0.0], 0.0, 1.0);
        // Far away.
        place(0x2003, [300.0, 0.0, 0.0], 0.0, 1.0);
        // A quarter turn about Z puts the long side along Y: x only spans 130..150.
        place(0x2004, [140.0, 0.0, 0.0], quarter_turn, 1.0);
        // The same turn at y = 140: the long side now reaches y 90..190.
        place(0x2005, [0.0, 140.0, 0.0], quarter_turn, 1.0);
        // Scale 0.1 shrinks the reach to 135..145.
        place(0x2006, [140.0, 0.0, 0.0], 0.0, 0.1);
        // Creation Z extent is +-5 (runtime Y), so z = 108 reaches only 103..113. Reading the
        // glTF Y axis as Creation Y would make it +-5 in y and +-10 in z, and overlap.
        place(0x2007, [0.0, 0.0, 108.0], 0.0, 1.0);
        // Creation Y extent is +-10 (runtime Z): y = 108 reaches 98..118 and overlaps.
        place(0x2008, [0.0, 108.0, 0.0], 0.0, 1.0);
        drop(connection);
        write_box_glb(
            &directory.path().join("meshes/architecture/wall.glb"),
            [-50.0, -5.0, -10.0],
            [50.0, 5.0, 10.0],
        );
        empty_cache(directory.path());

        let report = finalize_world_database(directory.path()).unwrap().unwrap();
        assert!(report.passed);
        let connection = Connection::open(database).unwrap();
        let rows: Vec<(u32, i64)> = connection
            .prepare("SELECT reference_id,contained FROM reference_rooms ORDER BY reference_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![(0x2001, 1), (0x2002, 0), (0x2005, 0), (0x2008, 0),]
        );
        assert_eq!(report.room_graph.overlap_rows, 3);
        // Running the stage again replaces the overlap rows rather than duplicating them.
        let mut connection = connection;
        assert_eq!(
            crate::esm::rooms::add_overlap_rows(&mut connection).unwrap(),
            3
        );
    }

    /// A room turned 0.6 rad with an asymmetric box, and a model turned with it: an object placed
    /// along the room's long axis overlaps, its mirror image across the X axis does not. The
    /// forward matrix, or the opposite turn, would swap the two.
    #[test]
    fn overlap_rows_follow_a_rotated_room_and_rotated_model() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("skyrim_world.db");
        let connection = Connection::open(&database).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO statics(id,model_path,flags) VALUES(0x500,'architecture/wall.nif',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z)
                 VALUES(0x1001,0x100,0,0,0,0,0,0.6,200,20,50)",
                [],
            )
            .unwrap();
        let (sin, cos) = 0.6f32.sin_cos();
        // 240 along the room's long axis (local +X is (cos, -sin) in world), turned with it, so
        // the model's 100-long side lies along the axis and spans 190..290, past the end at 200.
        for (id, y_sign) in [(0x2001u32, -1.0f32), (0x2002, 1.0)] {
            connection
                .execute(
                    "INSERT INTO \"references\"(id,cell_id,base_form_id,is_exterior,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,scale)
                     VALUES(?1,0x100,0x500,0,?2,?3,0,0,0,0.6,1)",
                    rusqlite::params![id, 240.0 * cos, y_sign * 240.0 * sin],
                )
                .unwrap();
        }
        drop(connection);
        write_box_glb(
            &directory.path().join("meshes/architecture/wall.glb"),
            [-50.0, -5.0, -10.0],
            [50.0, 5.0, 10.0],
        );
        empty_cache(directory.path());
        finalize_world_database(directory.path()).unwrap().unwrap();
        let connection = Connection::open(database).unwrap();
        let rows: Vec<(u32, i64)> = connection
            .prepare("SELECT reference_id,contained FROM reference_rooms ORDER BY reference_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![(0x2001, 0)]);
    }

    /// A reused older database whose `statics` has no bounds columns finalises, and the overlap
    /// pass leaves it alone even when it holds rooms.
    #[test]
    fn finalises_a_legacy_database_without_bounds_columns() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("skyrim_world.db");
        let connection = Connection::open(&database).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute_batch(
                "DROP TABLE statics;
                 CREATE TABLE statics(id INTEGER PRIMARY KEY, editor_id TEXT, model_path TEXT, flags INTEGER NOT NULL);",
            )
            .unwrap();
        drop(connection);
        empty_cache(directory.path());
        assert!(finalize_world_database(directory.path()).unwrap().is_some());

        let mut connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z)
                 VALUES(1,1,0,0,0,0,0,0,1,1,1)",
                [],
            )
            .unwrap();
        assert_eq!(
            crate::esm::rooms::add_overlap_rows(&mut connection).unwrap(),
            0
        );
    }
}
