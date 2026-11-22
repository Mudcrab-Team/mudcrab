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
    lod::{
        retail_terrain::{IMPORT_VERSION, RETAIL_NORMAL_AXES},
        retail_terrain_import::{LodWorld, WorldReport, import_world},
        retail_terrain_source::{self as source, BlockId},
        reuse::{LodManifest, TERRAIN_SOURCE_RETAIL},
    },
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rusqlite::Connection;
use serde::Deserialize;
use shared::lod::LodOrigin;
use std::{collections::BTreeMap, fs, path::PathBuf, time::Instant};

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

#[derive(Debug, Deserialize)]
struct RegisteredPack {
    path: PathBuf,
    #[serde(default)]
    manifests: BTreeMap<String, String>,
}

struct CloneFacts {
    manifests: BTreeMap<String, String>,
}

/// Derive terrain LOD into `options.target`. See the module documentation for the rules.
pub fn run(options: &DeriveTerrainLodOptions) -> Result<DeriveTerrainLodReport> {
    let started = Instant::now();
    let clone = check_clone(options)?;
    let target = fs::canonicalize(&options.target)?;
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

    let database_path = target.join("skyrim_world.db");
    let connection = Connection::open(&database_path)
        .wrap_err("cannot open the target world database for writing")?;
    let worlds = lod_worlds(&connection)?;
    ensure!(
        !worlds.is_empty(),
        "refused: the pack has no worldspace with terrain LOD"
    );

    // List what the game data offers; verify every archive that provides a file.
    let files = source::list(&options.data)?;
    let mut sources = Vec::new();
    for (name, path) in files.archives_used() {
        // Only archives that provide a file of a world in the pack matter.
        let relevant = files.blocks.iter().any(|((world, ..), block)| {
            worlds.iter().any(|w| w.file_name == *world)
                && block
                    .mesh
                    .iter()
                    .chain(&block.colour)
                    .chain(&block.normal)
                    .any(|provider| matches!(provider, source::Provider::Archive { name: n, .. } if *n == name))
        });
        if !relevant {
            continue;
        }
        let recorded = conversion.archives.get(&name).ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "refused: archive {name} provides retail terrain LOD files but the pack's manifest does not list it, so it is not proven to be the archive the pack was converted from"
            )
        })?;
        let actual = hash_file(&path)?;
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
        )?;
        reports.push(report);
    }
    ensure!(
        !reports.is_empty() && reports.iter().any(|report| !report.compiled.is_empty()),
        "refused: no retail terrain LOD block compiled; the target is unchanged except for removed chunks, discard this clone"
    );

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
    // Imported chunks have no cell-height inputs; their own proof is `retail_terrain_inputs`.
    manifest
        .chunk_inputs
        .retain(|path, _| !retail_inputs.contains_key(path));
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
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
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
    fs::write(
        target.join(RECEIPT_FILE),
        serde_json::to_string_pretty(&receipt)? + "\n",
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

/// Refuses anything that is not an exact APFS clone of a registered pack.
fn check_clone(options: &DeriveTerrainLodOptions) -> Result<CloneFacts> {
    let registry: BTreeMap<String, RegisteredPack> =
        serde_json::from_slice(&fs::read(&options.registry).wrap_err_with(|| {
            format!("cannot read pack registry {}", options.registry.display())
        })?)
        .wrap_err_with(|| format!("pack registry {} is not valid", options.registry.display()))?;
    let Some(source) = registry.get(&options.clone_of) else {
        bail!(
            "refused: `{}` is not a registered pack in {}",
            options.clone_of,
            options.registry.display()
        );
    };
    let metadata = fs::symlink_metadata(&options.target).wrap_err_with(|| {
        format!(
            "refused: target {} does not exist",
            options.target.display()
        )
    })?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "refused: target {} must be a real directory, not a file or a symlink",
        options.target.display()
    );
    let target = fs::canonicalize(&options.target)?;
    for (name, registered) in &registry {
        let Ok(path) = fs::canonicalize(&registered.path) else {
            continue;
        };
        ensure!(
            target != path && !target.starts_with(&path) && !path.starts_with(&target),
            "refused: target {} is, is inside, or contains the registered pack `{name}` ({}); derive into an APFS clone, never into a registered pack",
            target.display(),
            path.display()
        );
    }
    ensure!(
        !source.manifests.is_empty(),
        "refused: the registry records no manifest hashes for `{}`, so the clone cannot be proven",
        options.clone_of
    );
    for (file, expected) in &source.manifests {
        let candidate = target.join(file);
        ensure!(
            candidate.is_file(),
            "refused: target has no {file}, so it is not a clone of `{}`",
            options.clone_of
        );
        let actual = hash_file(&candidate)?;
        ensure!(
            &actual == expected,
            "refused: target {file} hash {actual} differs from the registered `{}` hash {expected}; the target is not an exact clone",
            options.clone_of
        );
        let original = source.path.join(file);
        if original.is_file() {
            ensure!(
                !same_file::is_same_file(&candidate, &original).unwrap_or(false),
                "refused: target {file} is the same file as the registered pack's; use `cp -c -R` (an APFS clone) to make the target"
            );
        }
    }
    Ok(CloneFacts {
        manifests: source.manifests.clone(),
    })
}
