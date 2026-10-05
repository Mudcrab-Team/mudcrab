//! Database and LOD package startup contracts shared by the runtime and launcher.
use color_eyre::{Result, eyre::WrapErr};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::path::Path;

pub fn validate_world_database(path: &Path) -> Result<u32> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .wrap_err_with(|| format!("failed to open {}", path.display()))?;
    let version: u32 = connection
        .query_row("SELECT version FROM schema_info LIMIT 1", [], |row| {
            row.get(0)
        })
        .wrap_err("world database has no schema version")?;
    color_eyre::eyre::ensure!(
        crate::supports_runtime_world_database_schema(version),
        "world database schema {version} is unsupported; supported versions are {} through {}",
        crate::MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION,
        crate::WORLD_DATABASE_SCHEMA_VERSION
    );
    Ok(version)
}

pub fn validate_lod_build_contract(assets_dir: &Path, converter_schema: u32) -> Result<u32> {
    let database_path = assets_dir.join("skyrim_world.db");
    let version = validate_world_database(&database_path)?;
    let connection = Connection::open_with_flags(&database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .wrap_err_with(|| format!("failed to open {}", database_path.display()))?;
    let manifest_path = assets_dir.join("lod-manifest.json");
    if !has_lod_table(&connection)? {
        let version: u32 =
            connection.query_row("SELECT version FROM schema_info", [], |row| row.get(0))?;
        color_eyre::eyre::ensure!(
            version < crate::WORLD_DATABASE_LOD_SCHEMA_VERSION && !manifest_path.exists(),
            "world database has no LOD chunk table"
        );
        return Ok(version);
    }
    let chunk_count: i64 = connection
        .query_row("SELECT count(*) FROM lod_chunks", [], |row| row.get(0))
        .wrap_err("world database has no LOD chunk table")?;
    if !manifest_path.is_file() {
        color_eyre::eyre::ensure!(
            chunk_count == 0,
            "world database contains {chunk_count} LOD chunks but {} is missing",
            manifest_path.display()
        );
        return Ok(version);
    }

    let conversion: LodProducerManifest = serde_json::from_slice(
        &std::fs::read(assets_dir.join("conversion-manifest.json"))
            .wrap_err("LOD package has no conversion producer manifest")?,
    )
    .wrap_err("invalid LOD conversion producer manifest")?;
    color_eyre::eyre::ensure!(
        conversion.schema_version == crate::LOD_CONVERTER_SCHEMA_VERSION,
        "LOD conversion producer is stale; reconvert assets with converter schema {}",
        crate::LOD_CONVERTER_SCHEMA_VERSION
    );

    let bytes = std::fs::read(&manifest_path)
        .wrap_err_with(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: LodBuildManifest =
        serde_json::from_slice(&bytes).wrap_err("invalid LOD build manifest")?;
    color_eyre::eyre::ensure!(
        manifest.land_texture_repeats_per_cell == crate::LAND_TEXTURE_REPEATS_PER_CELL,
        "LOD terrain texture scale is stale; rebuild LOD metadata for {} repeats per cell",
        crate::LAND_TEXTURE_REPEATS_PER_CELL
    );
    color_eyre::eyre::ensure!(
        converter_schema == crate::LOD_CONVERTER_SCHEMA_VERSION
            && manifest.converter_schema == converter_schema
            && manifest.world_database_schema == crate::WORLD_DATABASE_SCHEMA_VERSION
            && connection.query_row("SELECT version FROM schema_info", [], |row| row
                .get::<_, u32>(0))?
                == crate::WORLD_DATABASE_SCHEMA_VERSION,
        "LOD manifest schema is stale; reconvert assets with converter schema {converter_schema} and world database schema {}",
        crate::WORLD_DATABASE_SCHEMA_VERSION
    );
    color_eyre::eyre::ensure!(
        is_canonical_sha256(&manifest.build_identity),
        "LOD manifest build identity is not a lowercase SHA-256 digest"
    );
    let database_identity: String = connection
        .query_row(
            "SELECT build_identity FROM lod_build WHERE id=1",
            [],
            |row| row.get(0),
        )
        .wrap_err("world database has no LOD build identity")?;
    color_eyre::eyre::ensure!(
        database_identity == manifest.build_identity,
        "LOD database and manifest build identities do not match"
    );
    color_eyre::eyre::ensure!(
        u64::try_from(chunk_count).ok() == Some(manifest.chunks),
        "LOD manifest declares {} chunks but the world database contains {chunk_count}",
        manifest.chunks
    );
    Ok(version)
}

#[derive(Deserialize)]
struct LodProducerManifest {
    schema_version: u32,
}

#[derive(Deserialize)]
struct LodBuildManifest {
    build_identity: String,
    converter_schema: u32,
    world_database_schema: u32,
    chunks: u64,
    #[serde(default)]
    land_texture_repeats_per_cell: f32,
}

fn has_lod_table(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lod_chunks')",
        [],
        |row| row.get(0),
    )?)
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
