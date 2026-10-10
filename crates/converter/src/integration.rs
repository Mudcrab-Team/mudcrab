//! Final, asset-aware validation performed after all offline conversions.

use crate::{
    asset_path::{AssetKind, canonical_asset_path},
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
    pub missing_texture_count: u64,
    pub unavailable_texture_source_count: u64,
    pub issues: Vec<String>,
    pub passed: bool,
}

pub fn finalize_world_database(staging: &Path) -> Result<Option<IntegrationReport>> {
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
    let sources = source_file_index(staging)?;
    let static_models = {
        let mut statement = connection.prepare(
            "SELECT id,model_path FROM statics WHERE model_path IS NOT NULL AND model_path <> '' ORDER BY id",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    // Several records can name one model. The staged files are immutable during
    // this pass, so both successful bounds and errors can be shared by key.
    let mut model_bounds = HashMap::new();
    let transaction = connection.transaction()?;
    for (form_id, model_path) in static_models {
        let key = converted_key(&model_path, "meshes", "glb")?;
        let Some(path) = files.get(&key) else {
            let source_key = converted_key(&model_path, "meshes", "nif")?;
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
    let json_length = u32::from_le_bytes(header[12..16].try_into().unwrap());
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
}
