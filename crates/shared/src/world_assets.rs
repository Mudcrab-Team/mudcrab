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

/// Shared geometry admission for cluster reader integration.
/// Cluster packages require the current conversion producer without
/// a baked LOD dependency. Their overlay binding and certificates are validated by the resolver.
pub fn validate_geometry_build_contract(assets_dir: &Path, converter_schema: u32) -> Result<u32> {
    match crate::geometry::GeometryManifest::load(assets_dir)?.geometry_mode {
        crate::geometry::GeometryMode::Baked => {
            validate_lod_build_contract(assets_dir, converter_schema)
        }
        crate::geometry::GeometryMode::Cluster => {
            let version = validate_world_database(&assets_dir.join("skyrim_world.db"))?;
            validate_conversion_producer(assets_dir, converter_schema)?;
            Ok(version)
        }
    }
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

    validate_conversion_producer(assets_dir, converter_schema)?;

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

fn validate_conversion_producer(assets_dir: &Path, converter_schema: u32) -> Result<()> {
    let conversion: ConversionProducerManifest = serde_json::from_slice(
        &std::fs::read(assets_dir.join("conversion-manifest.json"))
            .wrap_err("geometry package has no conversion producer manifest")?,
    )
    .wrap_err("invalid geometry conversion producer manifest")?;
    color_eyre::eyre::ensure!(
        converter_schema == crate::LOD_CONVERTER_SCHEMA_VERSION
            && conversion.schema_version == converter_schema,
        "conversion producer is stale or unsupported; reconvert assets with converter schema {}",
        crate::LOD_CONVERTER_SCHEMA_VERSION
    );
    Ok(())
}

#[derive(Deserialize)]
struct ConversionProducerManifest {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    struct ClusterBase(PathBuf);

    impl ClusterBase {
        fn new(producer: Option<u32>) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "shared-geometry-contract-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            fs::write(
                root.join("OWNER.txt"),
                "shared geometry contract test fixture\n",
            )
            .unwrap();
            let connection = Connection::open(root.join("skyrim_world.db")).unwrap();
            connection.execute_batch(&format!(
                "CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES ({});",
                crate::WORLD_DATABASE_SCHEMA_VERSION)).unwrap();
            let mut manifest = serde_json::json!({
                "geometry_mode": "cluster",
                "cluster_package": "derived/clusters-0123456789abcdef"
            });
            if let Some(producer) = producer {
                manifest["schema_version"] = producer.into();
            }
            fs::write(
                root.join("conversion-manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            Self(root)
        }
    }

    impl Drop for ClusterBase {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn legacy_baked_geometry_retains_its_existing_schema_admission() {
        let base = ClusterBase::new(None);
        let connection = Connection::open(base.0.join("skyrim_world.db")).unwrap();
        connection
            .execute(
                "UPDATE schema_info SET version=?1",
                [crate::MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION],
            )
            .unwrap();
        drop(connection);
        fs::write(base.0.join("conversion-manifest.json"), b"{}").unwrap();
        assert_eq!(
            validate_geometry_build_contract(&base.0, crate::MIN_RUNTIME_CONVERTER_SCHEMA_VERSION)
                .unwrap(),
            crate::MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION
        );
    }

    #[test]
    fn cluster_geometry_accepts_current_producer_without_baked_lod_tables() {
        let base = ClusterBase::new(Some(crate::LOD_CONVERTER_SCHEMA_VERSION));
        assert_eq!(
            validate_geometry_build_contract(&base.0, crate::LOD_CONVERTER_SCHEMA_VERSION).unwrap(),
            crate::WORLD_DATABASE_SCHEMA_VERSION
        );
    }

    #[test]
    fn cluster_geometry_refuses_stale_future_or_missing_conversion_producers() {
        for producer in [
            Some(crate::LOD_CONVERTER_SCHEMA_VERSION - 1),
            Some(crate::LOD_CONVERTER_SCHEMA_VERSION + 1),
            None,
        ] {
            let base = ClusterBase::new(producer);
            assert!(
                validate_geometry_build_contract(&base.0, crate::LOD_CONVERTER_SCHEMA_VERSION)
                    .is_err(),
                "producer: {producer:?}"
            );
        }
    }

    #[test]
    fn cluster_geometry_refuses_a_caller_with_another_converter_contract() {
        let base = ClusterBase::new(Some(crate::LOD_CONVERTER_SCHEMA_VERSION));
        for converter in [
            crate::LOD_CONVERTER_SCHEMA_VERSION - 1,
            crate::LOD_CONVERTER_SCHEMA_VERSION + 1,
        ] {
            assert!(validate_geometry_build_contract(&base.0, converter).is_err());
        }
    }
}
