//! Final, asset-aware validation performed after all offline conversions.

use crate::{
    asset_path::{AssetKind, canonical_asset_path},
    mesh::MeshConverter,
};
use color_eyre::{Result, eyre::WrapErr};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
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
        match MeshConverter::glb_bounds(path) {
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
    let mut files = HashMap::new();
    if !root.is_dir() {
        return Ok(files);
    }
    for entry in WalkDir::new(&root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(&root)?;
            files.insert(normalize(relative), entry.into_path());
        }
    }
    Ok(files)
}

fn converted_file_index(staging: &Path) -> Result<HashMap<String, PathBuf>> {
    let mut files = HashMap::new();
    for entry in WalkDir::new(staging).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(staging)?;
            files.insert(normalize(relative), entry.into_path());
        }
    }
    Ok(files)
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
