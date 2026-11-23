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
//! ambiguous file is skipped and counted with its reason. A forced reimport keeps prior retail
//! chunks that receive no replacement. A worldspace with no compiled blocks keeps all its chunks.

use crate::{
    cache::{CONVERTER_SCHEMA_VERSION, ConversionManifest, hash_bytes, hash_file},
    integration::open_private_world_database,
    lod::{
        generation::StagedGeneration,
        reconciliation::SkipPresence,
        retail_terrain::{IMPORT_VERSION, RETAIL_NORMAL_AXES, tier_for_level},
        retail_terrain_import::{LodWorld, WorldReport, stage_world},
        retail_terrain_source::{self as source, BlockId},
        reuse::{LodManifest, TERRAIN_SOURCE_RETAIL},
    },
    pack_clone::{check_clone, lock_for_derive, require_private_output},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::{Connection, TransactionBehavior};
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
    /// Worldspaces that kept self-built chunks because no retail blocks compiled.
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
    let mut connection = open_private_world_database(&target)?;
    let conversion = ConversionManifest::load(&target.join("conversion-manifest.json"))
        .wrap_err("refused: the target conversion manifest cannot be read")?;
    ensure!(
        conversion.schema_version == CONVERTER_SCHEMA_VERSION && conversion.complete,
        "refused: the target manifest is not a complete schema {CONVERTER_SCHEMA_VERSION} package"
    );
    shared::world_assets::validate_lod_build_contract(&target, CONVERTER_SCHEMA_VERSION)
        .wrap_err("refused: the target has no valid terrain LOD to replace")?;
    let manifest_path = Path::new("lod-manifest.json");
    let manifest_before = fs::read(target.join(manifest_path))
        .wrap_err("refused: the target has no lod-manifest.json")?;
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

    // Verify archives that supply blocks eligible for compilation.
    let files = source::list(&options.data)?;
    for warning in &files.archive_warnings {
        eprintln!("{warning}");
    }
    let mut sources = Vec::new();
    let relevant_ids = files.blocks.keys().filter(|(world, level, ..)| {
        tier_for_level(*level).is_some()
            && worlds.iter().any(|candidate| candidate.file_name == *world)
    });
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

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut staged = StagedGeneration::new(&target)?;
    let mut reports = Vec::new();
    let mut kept = Vec::new();
    // silent-ok: a pack made before the block existed has no recorded skips
    let mut reconciliation = manifest.reconciliation.take().unwrap_or_default();
    for world in &worlds {
        reconciliation.remove_terrain_world(&world.file_name);
    }
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
        let imported = stage_world(
            &transaction,
            &mut staged,
            world,
            &files,
            &ids,
            &pool,
            &manifest.retail_terrain_inputs,
            || Ok(()),
        )
        .wrap_err_with(|| format!("failed to stage terrain LOD for {}", world.editor_id))?;
        for (relative, input_hash, payload_hash) in imported.new_inputs() {
            retail_inputs.insert(relative.to_owned(), input_hash.to_owned());
            payload_hashes.push(format!("{relative}:{payload_hash}"));
        }
        let report = imported.report;
        for (id, skipped) in &report.skipped {
            reconciliation.record_terrain_skip(
                &world.file_name,
                id.1,
                skipped.class,
                (id.2, id.3),
                SkipPresence::of(&files, id),
            );
        }
        if report.kept_self_built {
            kept.push(world.editor_id.clone());
        }
        if ids.is_empty() && report.kept_from_previous_generation == 0 {
            continue;
        }
        if !ids.is_empty() && report.compiled.is_empty() {
            eprintln!(
                "worldspace {} kept its existing terrain LOD: no blocks compiled ({:?}), {} retail chunks kept from previous generation",
                world.editor_id, report.skipped_by_class, report.kept_from_previous_generation
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
            "archive {name} changed during terrain LOD import"
        );
    }

    // Manifest and database identity: one new identity shared by both.
    let chunk_count: i64 =
        transaction.query_row("SELECT count(*) FROM lod_chunks", [], |row| row.get(0))?;
    payload_hashes.sort();
    let identity = hash_bytes(&serde_json::to_vec(&serde_json::json!({
        "previous": manifest.build_identity,
        "terrain_lod_source": TERRAIN_SOURCE_RETAIL,
        "import_version": IMPORT_VERSION,
        "payloads": payload_hashes,
    }))?);
    transaction.execute(
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
    reconciliation.terrain_recorded = true;
    manifest.reconciliation = Some(reconciliation);
    if kept.is_empty() {
        // Every world's land texture sources fed only the chunks that were replaced.
        manifest.terrain_sources.clear();
        manifest.chunk_inputs.clear();
    }
    let staging = staged.staging_root();
    let staged_manifest = staging.join(manifest_path);
    fs::write(&staged_manifest, serde_json::to_vec_pretty(&manifest)?)
        .wrap_err("failed to stage the terrain LOD manifest")?;
    let manifest_after = fs::read(&staged_manifest)?;
    shared::world_assets::validate_lod_manifest(
        &transaction,
        &manifest_after,
        CONVERTER_SCHEMA_VERSION,
    )
    .wrap_err("the staged generation does not satisfy the engine's LOD contract")?;
    staged.paths.insert(manifest_path.into());

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
        "totals": {
            "chunks": chunks,
            "skipped": reports.iter().map(|r| r.skipped.len()).sum::<usize>(),
            "kept_from_previous_generation": reports.iter().map(|r| r.kept_from_previous_generation).sum::<usize>(),
        },
        "kept_self_built_worlds": kept,
        "worlds": reports.iter().map(|report| serde_json::json!({
            "editor_id": report.editor_id,
            "blocks_found_by_level": report.found,
            "chunks_by_tier": report.compiled,
            "triangles": report.triangles,
            "vertices": report.vertices,
            "chunk_bytes": report.bytes,
            "old_chunks_removed": report.old_chunks_removed,
            "kept_from_previous_generation": report.kept_from_previous_generation,
            "skipped_by_class": report.skipped_by_class,
            "skipped": report.skipped.iter().map(|((world, level, x, y), skipped)| serde_json::json!({
                "block": format!("{world}.{level}.{x}.{y}"),
                "class": skipped.class,
                "detail": skipped.detail,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "lod_manifest_sha256": {
            "before": hash_bytes(&manifest_before),
            "after": hash_bytes(&manifest_after),
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
        staging.join(RECEIPT_FILE),
        serde_json::to_string_pretty(&receipt)? + "\n",
    )
    .wrap_err("failed to stage the terrain LOD receipt")?;
    staged.paths.insert(RECEIPT_FILE.into());
    // The registered manifests are untouched by this command; prove it.
    for (file, expected) in &clone.manifests {
        ensure!(
            &hash_file(&target.join(file))? == expected,
            "the derive changed {file}, which it must not touch"
        );
    }
    staged.publish(&target, transaction, || Ok(()))?;
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
    use shared::lod::{ChunkAnchor, ChunkKey, LodTier, chunk_payload_path};

    fn payload_path(world: u32, x: i32) -> String {
        chunk_payload_path(ChunkKey::new(
            world,
            LodTier::Tier16,
            ChunkAnchor::new(x, 0),
        ))
    }

    fn derive_fixture(directory: &Path) -> DeriveTerrainLodOptions {
        let registered = directory.join("registered");
        let target = directory.join("clone");
        let data = directory.join("Data");
        fs::create_dir(&registered).unwrap();
        fs::create_dir(&target).unwrap();
        let conversion = serde_json::to_vec(&ConversionManifest {
            schema_version: CONVERTER_SCHEMA_VERSION,
            complete: true,
            ..Default::default()
        })
        .unwrap();
        for root in [&registered, &target] {
            fs::write(root.join("conversion-manifest.json"), &conversion).unwrap();
        }
        let registry = directory.join("packs.json");
        fs::write(
            &registry,
            serde_json::to_vec(&serde_json::json!({
                "pack": {
                    "path": registered,
                    "manifests": { "conversion-manifest.json": hash_bytes(&conversion) },
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let db = target.join("skyrim_world.db");
        let connection = Connection::open(&db).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute_batch(
                "INSERT INTO worldspaces(id, editor_id, flags, lod_origin_x, lod_origin_y)
                    VALUES (60, 'First', 0, 0, 0), (61, 'Second', 0, 0, 0);",
            )
            .unwrap();
        for (world, x) in [(60, 0), (60, 1), (61, 0)] {
            let relative = payload_path(world, x);
            let path = target.join(&relative);
            let bytes = format!("old terrain for world {world}, anchor {x}");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &bytes).unwrap();
            connection
                .execute(
                    "INSERT INTO lod_chunks VALUES (?1, 16, ?2, 0, ?3, ?4, 0, 0, 0, 1, 1, 1, '0,0')",
                    rusqlite::params![world, x, relative, hash_bytes(bytes.as_bytes())],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO lod_chunks_spatial VALUES ((SELECT count(*)+1 FROM lod_chunks_spatial),
                     0, 1, 0, 1, ?1, 16, ?2, 0)",
                    rusqlite::params![world, x],
                )
                .unwrap();
        }
        let identity = "a".repeat(64);
        connection
            .execute("INSERT INTO lod_build VALUES (1, ?1)", [&identity])
            .unwrap();
        drop(connection);
        let manifest = serde_json::to_vec(&serde_json::json!({
            "build_identity": identity,
            "converter_schema": CONVERTER_SCHEMA_VERSION,
            "world_database_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
            "chunks": 3,
            "land_texture_repeats_per_cell": shared::LAND_TEXTURE_REPEATS_PER_CELL,
            "terrain_sources": {},
            "terrain_lod_source": crate::lod::reuse::TERRAIN_SOURCE_SELF_BUILT,
        }))
        .unwrap();
        fs::write(target.join("lod-manifest.json"), &manifest).unwrap();
        fs::write(target.join(RECEIPT_FILE), b"previous receipt").unwrap();
        shared::world_assets::validate_lod_build_contract(&target, CONVERTER_SCHEMA_VERSION)
            .unwrap();

        for world in ["first", "second"] {
            write_block(&data, world, 0, 5);
        }
        DeriveTerrainLodOptions {
            target,
            registry,
            clone_of: "pack".into(),
            data,
            force: true,
            converter_head: None,
            command: Vec::new(),
        }
    }

    fn write_block(data: &Path, world: &str, x: i32, seed: u64) {
        let mesh = crate::lod::retail::btr::tests::land_block();
        let map = dummy_content::dds::generate(
            &dummy_content::dds::Spec::new(dummy_content::dds::Format::X8R8G8B8, 8, 8),
            &mut dummy_content::rng::Rng::new(seed),
        )
        .unwrap();
        let meshes = data.join("meshes/terrain").join(world);
        let textures = data.join("textures/terrain").join(world);
        fs::create_dir_all(&meshes).unwrap();
        fs::create_dir_all(&textures).unwrap();
        fs::write(meshes.join(format!("{world}.16.{x}.0.btr")), &mesh).unwrap();
        fs::write(textures.join(format!("{world}.16.{x}.0.dds")), &map).unwrap();
        fs::write(textures.join(format!("{world}.16.{x}.0_n.dds")), &map).unwrap();
    }

    #[test]
    fn a_second_world_failure_keeps_the_previous_terrain_generation() {
        let directory = tempfile::tempdir().unwrap();
        let options = derive_fixture(directory.path());
        let target = &options.target;
        let db = target.join("skyrim_world.db");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_second_world BEFORE INSERT ON lod_chunks
                    WHEN new.worldspace_id = 61 AND
                         (SELECT count(*) FROM lod_chunks WHERE worldspace_id = 60) = 1
                    BEGIN SELECT RAISE(ABORT, 'second world failed after the first replaced its chunks'); END;",
            )
            .unwrap();
        drop(connection);
        let manifest_path = target.join("lod-manifest.json");
        let receipt_path = target.join(RECEIPT_FILE);
        let stale_path = target.join(payload_path(60, 1));
        let previous = [
            target.join(payload_path(60, 0)),
            stale_path.clone(),
            target.join(payload_path(61, 0)),
            manifest_path.clone(),
            receipt_path.clone(),
            db.clone(),
        ]
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        });
        let error = run(&options).unwrap_err();
        assert!(
            format!("{error:#}")
                .contains("second world failed after the first replaced its chunks"),
            "{error:#}"
        );
        for (path, bytes) in &previous {
            assert_eq!(&fs::read(path).unwrap(), bytes, "{}", path.display());
        }
        shared::world_assets::validate_lod_build_contract(target, CONVERTER_SCHEMA_VERSION)
            .unwrap();

        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch("DROP TRIGGER fail_second_world")
            .unwrap();
        drop(connection);
        let report = run(&options).unwrap();
        assert_eq!(report.chunks, 2);
        assert_eq!(report.worlds.len(), 2);
        assert!(
            !stale_path.exists(),
            "the stale chunk is retired on success"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(receipt_path).unwrap()).unwrap()
                ["lod_manifest_sha256"]["after"],
            hash_file(&manifest_path).unwrap()
        );
        shared::world_assets::validate_lod_build_contract(target, CONVERTER_SCHEMA_VERSION)
            .unwrap();
    }

    #[test]
    fn forced_reimport_keeps_a_failed_retail_chunk_when_its_sibling_compiles() {
        let directory = tempfile::tempdir().unwrap();
        let options = derive_fixture(directory.path());
        let target = &options.target;
        write_block(&options.data, "first", 16, 5);
        assert_eq!(run(&options).unwrap().chunks, 3);

        let manifest_path = target.join("lod-manifest.json");
        let failed_relative = payload_path(60, 1);
        let sibling_relative = payload_path(60, 0);
        let previous: LodManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let failed_bytes = fs::read(target.join(&failed_relative)).unwrap();
        let sibling_bytes = fs::read(target.join(&sibling_relative)).unwrap();
        let saved_row = || {
            let connection = Connection::open(target.join("skyrim_world.db")).unwrap();
            connection
                .query_row(
                    "SELECT c.rowid, content_hash, source_cells, s.id, s.minX, s.maxX, s.minY, s.maxY
                     FROM lod_chunks c JOIN lod_chunks_spatial s
                     USING (worldspace_id, tier, anchor_x, anchor_y)
                     WHERE c.payload_path = ?1",
                    [&failed_relative],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, f64>(4)?,
                            row.get::<_, f64>(5)?,
                            row.get::<_, f64>(6)?,
                            row.get::<_, f64>(7)?,
                        ))
                    },
                )
                .unwrap()
        };
        let row_before = saved_row();
        fs::write(
            options.data.join("meshes/terrain/first/first.16.16.0.btr"),
            b"damaged mesh",
        )
        .unwrap();
        write_block(&options.data, "first", 0, 7);

        let report = run(&options).unwrap();
        assert_eq!(report.chunks, 2);
        assert_eq!(report.worlds[0].skipped_by_class["unreadable_mesh"], 1);
        assert_eq!(report.worlds[0].kept_from_previous_generation, 1);
        assert_eq!(report.worlds[0].old_chunks_removed, 0);
        assert_eq!(saved_row(), row_before);
        assert_eq!(
            fs::read(target.join(&failed_relative)).unwrap(),
            failed_bytes
        );
        assert_ne!(
            fs::read(target.join(&sibling_relative)).unwrap(),
            sibling_bytes
        );
        let manifest: LodManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.chunks, 3);
        assert_eq!(
            manifest.retail_terrain_inputs[&failed_relative],
            previous.retail_terrain_inputs[&failed_relative]
        );
        assert_ne!(
            manifest.retail_terrain_inputs[&sibling_relative],
            previous.retail_terrain_inputs[&sibling_relative]
        );
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(report.receipt).unwrap()).unwrap();
        assert_eq!(receipt["totals"]["kept_from_previous_generation"], 1);
        assert_eq!(receipt["worlds"][0]["kept_from_previous_generation"], 1);
        shared::world_assets::validate_lod_build_contract(target, CONVERTER_SCHEMA_VERSION)
            .unwrap();
    }

    #[test]
    fn unlisted_level_32_archive_is_skipped_but_still_reported() {
        use dummy_content::{Entry, bsa};

        let directory = tempfile::tempdir().unwrap();
        let options = derive_fixture(directory.path());
        let path = options.data.join("Unused.bsa");
        let level_32_mesh = Entry::new("meshes/terrain/first/first.32.0.0.btr", b"level 32");
        fs::write(
            &path,
            bsa::v105(&[level_32_mesh], bsa::Compression::None).unwrap(),
        )
        .unwrap();

        let report = run(&options).unwrap();
        assert_eq!(report.chunks, 2);
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(report.receipt).unwrap()).unwrap();
        assert_eq!(receipt["sources"], serde_json::json!([]));
        let world = &receipt["worlds"][0];
        assert_eq!(world["blocks_found_by_level"]["32"], 1);
        assert_eq!(world["skipped_by_class"]["no_runtime_tier"], 1);
        assert_eq!(world["skipped"][0]["block"], "first.32.0.0");

        let level_16_mesh = Entry::new("meshes/terrain/first/first.16.16.0.btr", b"level 16");
        fs::write(
            &path,
            bsa::v105(&[level_16_mesh], bsa::Compression::None).unwrap(),
        )
        .unwrap();
        let error = run(&options).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("archive unused.bsa provides retail terrain LOD files"),
            "{error:#}"
        );
    }

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
