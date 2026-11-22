//! Derive retail terrain LOD into an APFS clone of a registered pack, without reconverting anything.
//!
//! The pack already holds terrain LOD chunks that the converter built from cell heights. For every
//! worldspace that has retail terrain LOD files in the game data, this command replaces those
//! chunks with chunks compiled from the retail `.btr` meshes, colour maps and normal maps (see
//! [`crate::lod::retail_terrain`]). It touches only the terrain LOD chunk files, their database
//! rows, `lod-manifest.json`, and writes a receipt. It does not read or write any NIF, texture,
//! script or record output, `conversion-manifest.json` or `integration-report.json`.
//!
//! Fails closed. It refuses a target that is not an exact clone of a registered pack, a pack
//! without terrain LOD, a pack whose terrain LOD is already imported (unless `force`), and an
//! archive that is not the one the pack was converted from. A block with a missing, unreadable or
//! ambiguous file is skipped and counted with its reason; nothing is generated in its place. A
//! worldspace with no retail terrain LOD files keeps the chunks it has and is listed in the
//! receipt.

use crate::{
    cache::{CONVERTER_SCHEMA_VERSION, ConversionManifest, hash_bytes, hash_file},
    integration::open_private_world_database,
    lod::{
        retail_terrain::{IMPORT_VERSION, RETAIL_NORMAL_AXES},
        retail_terrain_import::{LodWorld, WorldReport, import_world},
        retail_terrain_source::{self as source, BlockId},
        reuse::{LodManifest, TERRAIN_SOURCE_RETAIL},
    },
    pack_clone::{check_clone, lock_for_derive, require_private_output},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::Connection;
use shared::lod::LodOrigin;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

/// Name of the receipt written into the clone.
pub const RECEIPT_FILE: &str = "terrain-lod-receipt.json";
/// Receipt format name, so a reader can recognise the file.
pub const RECEIPT_FORMAT: &str = "terrain-lod-receipt/1";

/// Inputs of `derive-terrain-lod`.
#[derive(Debug, Clone)]
pub struct DeriveTerrainLodOptions {
    /// The APFS clone that receives the retail terrain LOD.
    pub target: PathBuf,
    /// `packs.json` of the shared storage. The command only reads it.
    pub registry: PathBuf,
    /// Registered pack that `target` must be a clone of.
    pub clone_of: String,
    /// The Skyrim `Data` folder the pack was converted from. Read only.
    pub data: PathBuf,
    /// Import again over a target that already has retail terrain LOD.
    pub force: bool,
    /// Git head of the converter build, recorded in the receipt. The caller supplies it.
    pub converter_head: Option<String>,
    /// The command line as the caller ran it, recorded in the receipt.
    pub command: Vec<String>,
}

/// What a successful run produced.
#[derive(Debug)]
pub struct DeriveTerrainLodReport {
    pub receipt: PathBuf,
    pub worlds: Vec<WorldReport>,
    /// Worldspaces of the pack with terrain LOD and no retail files; their chunks are untouched.
    pub kept_self_built: Vec<String>,
    pub chunks: usize,
    pub elapsed_ms: u128,
}

/// Derive terrain LOD into `options.target`. See the module documentation for the rules.
pub fn run(options: &DeriveTerrainLodOptions) -> Result<DeriveTerrainLodReport> {
    let started = Instant::now();
    let clone = check_clone(&options.registry, &options.clone_of, &options.target)?;
    let target = clone.target;
    let _lock = lock_for_derive(&target)?;
    for file in ["lod-manifest.json", RECEIPT_FILE] {
        require_private_output(&target, Path::new(file))?;
    }
    let connection = open_private_world_database(&target)?;
    let conversion = ConversionManifest::load(&target.join("conversion-manifest.json"))
        .wrap_err("refused: the target conversion manifest cannot be read")?;
    ensure!(
        conversion.schema_version == CONVERTER_SCHEMA_VERSION && conversion.complete,
        "refused: the target manifest is not a complete schema {CONVERTER_SCHEMA_VERSION} package"
    );
    shared::world_assets::validate_lod_build_contract(&target, CONVERTER_SCHEMA_VERSION)
        .wrap_err("refused: the target has no valid terrain LOD to replace")?;
    let manifest_path = target.join("lod-manifest.json");
    let manifest_before =
        fs::read(&manifest_path).wrap_err("refused: the target has no lod-manifest.json")?;
    let mut manifest: LodManifest = serde_json::from_slice(&manifest_before)
        .wrap_err("refused: lod-manifest.json is not valid")?;
    ensure!(
        options.force
            || (manifest.terrain_lod_source.as_deref() != Some(TERRAIN_SOURCE_RETAIL)
                && !target.join(RECEIPT_FILE).exists()),
        "refused: the target already has retail terrain LOD; pass --force to import again"
    );

    let worlds = lod_worlds(&connection)?;
    ensure!(
        !worlds.is_empty(),
        "refused: the pack has no worldspace with terrain LOD"
    );

    // List what the game data offers; verify every archive that provides a file.
    let files = source::list(&options.data)?;
    for warning in &files.archive_warnings {
        eprintln!("{warning}");
    }
    let mut sources = Vec::new();
    let relevant_ids = files
        .blocks
        .keys()
        .filter(|(world, ..)| worlds.iter().any(|candidate| candidate.file_name == *world));
    let verified_archives = files.archives_used(relevant_ids);
    for (&name, &path) in &verified_archives {
        let recorded = conversion.archives.get(name).ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "refused: archive {name} provides retail terrain LOD files but the pack's manifest does not list it, so it is not proven to be the archive the pack was converted from"
            )
        })?;
        let actual = files.archive_hash(path)?;
        ensure!(
            actual == recorded.source_hash,
            "refused: archive {name} differs from the one the pack was converted from (hash {actual}, manifest {})",
            recorded.source_hash
        );
        sources.push(
            serde_json::json!({ "archive": name, "sha256": actual, "matches_pack_manifest": true }),
        );
    }

    let mut reports = Vec::new();
    let mut kept = Vec::new();
    let mut retail_inputs: BTreeMap<String, String> = BTreeMap::new();
    let mut payload_hashes: Vec<String> = Vec::new();
    let pool = rayon::ThreadPoolBuilder::new()
        .build()
        .wrap_err("failed to create the terrain LOD compiler pool")?;
    for world in &worlds {
        let ids: Vec<BlockId> = files
            .blocks
            .keys()
            .filter(|(name, ..)| *name == world.file_name)
            .cloned()
            .collect();
        if ids.is_empty() {
            kept.push(world.editor_id.clone());
            continue;
        }
        let report = import_world(
            &connection,
            &target,
            world,
            &files,
            &ids,
            &pool,
            &mut retail_inputs,
            &mut payload_hashes,
            || Ok(()),
        )?;
        if report.compiled.is_empty() {
            kept.push(world.editor_id.clone());
            eprintln!(
                "worldspace {} kept its existing terrain LOD: no blocks compiled ({:?})",
                world.editor_id, report.skipped_by_class
            );
        }
        reports.push(report);
    }
    ensure!(
        !reports.is_empty() && reports.iter().any(|report| !report.compiled.is_empty()),
        "refused: no retail terrain LOD block compiled; existing terrain LOD is intact"
    );
    for (name, path) in verified_archives {
        ensure!(
            files.archive_hash(path)? == conversion.archives[name].source_hash,
            "archive {name} changed during terrain LOD import; discard this clone"
        );
    }

    // Manifest and database identity: one new identity shared by both.
    let chunk_count: i64 =
        connection.query_row("SELECT count(*) FROM lod_chunks", [], |row| row.get(0))?;
    payload_hashes.sort();
    let identity = hash_bytes(&serde_json::to_vec(&serde_json::json!({
        "previous": manifest.build_identity,
        "terrain_lod_source": TERRAIN_SOURCE_RETAIL,
        "import_version": IMPORT_VERSION,
        "payloads": payload_hashes,
    }))?);
    connection.execute(
        "INSERT OR REPLACE INTO lod_build(id, build_identity) VALUES (1, ?1)",
        [&identity],
    )?;
    manifest.build_identity = identity;
    manifest.chunks = u64::try_from(chunk_count)?;
    for path in reports.iter().flat_map(|report| &report.replaced_paths) {
        manifest.chunk_inputs.remove(path);
        manifest.retail_terrain_inputs.remove(path);
    }
    manifest.terrain_lod_source = Some(if kept.is_empty() {
        TERRAIN_SOURCE_RETAIL.to_owned()
    } else {
        format!("{TERRAIN_SOURCE_RETAIL}+self-built")
    });
    manifest.retail_terrain_import_version = IMPORT_VERSION;
    manifest.retail_terrain_inputs.extend(retail_inputs);
    if kept.is_empty() {
        // Every world's land texture sources fed only the chunks that were replaced.
        manifest.terrain_sources.clear();
        manifest.chunk_inputs.clear();
    }
    crate::mesh::write_glb_atomic(&manifest_path, &serde_json::to_vec_pretty(&manifest)?)?;
    drop(connection);
    shared::world_assets::validate_lod_build_contract(&target, CONVERTER_SCHEMA_VERSION).wrap_err(
        "the derived pack no longer satisfies the engine's LOD contract; discard this clone",
    )?;

    let chunks = reports
        .iter()
        .map(|report| report.compiled.values().sum::<usize>())
        .sum();
    let elapsed_ms = started.elapsed().as_millis();
    let receipt = serde_json::json!({
        "format": RECEIPT_FORMAT,
        "command": options.command,
        "converter_head": options.converter_head,
        "import_version": IMPORT_VERSION,
        "normal_map_axes": {
            "east": { "channel": RETAIL_NORMAL_AXES.east.0, "sign": RETAIL_NORMAL_AXES.east.1 },
            "north": { "channel": RETAIL_NORMAL_AXES.north.0, "sign": RETAIL_NORMAL_AXES.north.1 },
            "up": { "channel": RETAIL_NORMAL_AXES.up.0, "sign": RETAIL_NORMAL_AXES.up.1 },
        },
        "clone_of": { "name": options.clone_of, "manifests": clone.manifests },
        "game_data_read": true,
        "sources": sources,
        "archive_warnings": files.archive_warnings,
        "totals": { "chunks": chunks, "skipped": reports.iter().map(|r| r.skipped.len()).sum::<usize>() },
        "kept_self_built_worlds": kept,
        "worlds": reports.iter().map(|report| serde_json::json!({
            "editor_id": report.editor_id,
            "blocks_found_by_level": report.found,
            "chunks_by_tier": report.compiled,
            "triangles": report.triangles,
            "vertices": report.vertices,
            "chunk_bytes": report.bytes,
            "old_chunks_removed": report.old_chunks_removed,
            "skipped_by_class": report.skipped_by_class,
            "skipped": report.skipped.iter().map(|((world, level, x, y), skipped)| serde_json::json!({
                "block": format!("{world}.{level}.{x}.{y}"),
                "class": skipped.class,
                "detail": skipped.detail,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "lod_manifest_sha256": {
            "before": hash_bytes(&manifest_before),
            "after": hash_file(&manifest_path)?,
        },
        "unverified": [
            "Shading beyond sampling the shipped colour and normal maps is research item 7 and is not verified.",
            "The normal map channel layout (red east, green up, blue north) is the best fit against the retail mesh, not decoded from retail code.",
            "Water planes in the .btr files are not imported here.",
            "Level 32 blocks are not imported: the runtime has no level 32 tier.",
        ],
        "elapsed_ms": elapsed_ms,
    });
    crate::mesh::write_glb_atomic(
        &target.join(RECEIPT_FILE),
        (serde_json::to_string_pretty(&receipt)? + "\n").as_bytes(),
    )
    .wrap_err("failed to write the terrain LOD receipt")?;
    // The registered manifests are untouched by this command; prove it.
    for (file, expected) in &clone.manifests {
        ensure!(
            &hash_file(&target.join(file))? == expected,
            "the derive changed {file}, which it must not touch"
        );
    }
    Ok(DeriveTerrainLodReport {
        receipt: target.join(RECEIPT_FILE),
        worlds: reports,
        kept_self_built: kept,
        chunks,
        elapsed_ms,
    })
}

/// Worldspaces that have a LOD origin and terrain LOD chunks, by id.
fn lod_worlds(connection: &Connection) -> Result<Vec<LodWorld>> {
    let mut statement = connection.prepare(
        "SELECT w.id, w.editor_id, w.lod_origin_x, w.lod_origin_y FROM worldspaces w \
         WHERE w.lod_origin_x IS NOT NULL AND w.lod_origin_y IS NOT NULL \
         AND EXISTS (SELECT 1 FROM lod_chunks c WHERE c.worldspace_id = w.id) ORDER BY w.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i32>(2)?,
            row.get::<_, i32>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (id, editor_id, x, y) = row?;
        Ok(LodWorld {
            id,
            file_name: editor_id.to_ascii_lowercase(),
            editor_id,
            origin: LodOrigin::new(x, y),
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_derive_rejects_a_hard_link_to_the_registered_database() {
        let directory = tempfile::tempdir().unwrap();
        let registered = directory.path().join("registered");
        let target = directory.path().join("clone");
        fs::create_dir(&registered).unwrap();
        fs::create_dir(&target).unwrap();
        let manifest = b"{ synthetic manifest }";
        let registry = directory.path().join("packs.json");
        for root in [&registered, &target] {
            fs::write(root.join("conversion-manifest.json"), manifest).unwrap();
        }
        fs::write(
            &registry,
            serde_json::to_vec(&serde_json::json!({
                "pack": {
                    "path": registered,
                    "manifests": { "conversion-manifest.json": hash_bytes(manifest) }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let database = registered.join("skyrim_world.db");
        fs::write(&database, b"database").unwrap();
        fs::hard_link(&database, target.join("skyrim_world.db")).unwrap();
        let options = DeriveTerrainLodOptions {
            target: target.clone(),
            registry,
            clone_of: "pack".into(),
            data: directory.path().into(),
            force: false,
            converter_head: None,
            command: Vec::new(),
        };

        let error = run(&options).unwrap_err().to_string();
        assert!(error.contains("skyrim_world.db") && error.contains("needs a private copy"));
        assert_eq!(fs::read(database).unwrap(), b"database");
    }
}
