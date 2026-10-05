//! Skyrim sidecars through archive ingestion, loose overrides, and LOD compilation.

use dummy_content::{Entry, bsa, layout};
use std::{fs, path::Path};

// Skyrim layout: two i16 origins, then i32 stride/minimum/maximum levels.
const PACKED_SETTINGS: [u8; 16] = [0xfc, 0xff, 0xfc, 0xff, 32, 0, 0, 0, 4, 0, 0, 0, 32, 0, 0, 0];
const LOOSE_SETTINGS: [u8; 16] = [0xf8, 0xff, 0xf8, 0xff, 32, 0, 0, 0, 4, 0, 0, 0, 32, 0, 0, 0];

fn generate_data(root: &Path) {
    layout::prepare_directory(root, false).unwrap();
    let formats = layout::Formats::parse("dds,pex,nif,esm").unwrap();
    layout::generate(root, layout::DEFAULT_SEED, formats).unwrap();
    let archive = bsa::v105(
        &[Entry {
            name: "LODSettings/GeneratedWorld.LOD",
            data: &PACKED_SETTINGS,
        }],
        bsa::Compression::None,
    )
    .unwrap();
    fs::write(root.join("Skyrim - Misc.bsa"), archive).unwrap();
}

async fn convert(data: &Path, output: &Path) -> converter::pipeline::PipelineReport {
    convert_config(converter::PipelineConfig::new(data, output)).await
}

async fn convert_config(config: converter::PipelineConfig) -> converter::PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = converter::AssetPipeline::run_async(config, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    report
}

#[tokio::test]
async fn no_lod_replaces_generated_lod_without_reconverting_ordinary_assets() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("assets");
    generate_data(&data);
    let enabled = convert(&data, &output).await;
    assert!(enabled.lod_chunks > 0);
    let manifest: converter::cache::ConversionManifest =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    let original_outputs: Vec<_> = manifest
        .entries
        .values()
        .map(|entry| {
            (
                entry.output.clone(),
                fs::read(output.join(&entry.output)).unwrap(),
            )
        })
        .collect();

    let mut config = converter::PipelineConfig::new(&data, &output);
    config.no_lod = true;
    let disabled = convert_config(config).await;
    assert!(disabled.complete);
    assert_eq!(disabled.converted, 0);
    assert_eq!(disabled.lod_chunks, 0);
    assert!(
        disabled
            .notices
            .iter()
            .any(|notice| notice.contains("--no-lod"))
    );
    assert!(!output.join("lod-manifest.json").exists());
    assert!(!output.join("lod").exists());
    let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM lod_chunks", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert!(
        shared::world_assets::validate_lod_build_contract(
            &output,
            shared::LOD_CONVERTER_SCHEMA_VERSION
        )
        .is_ok()
    );
    for (path, bytes) in original_outputs {
        assert_eq!(fs::read(output.join(path)).unwrap(), bytes);
    }
    drop(db);
    let reenabled = convert(&data, &output).await;
    assert!(reenabled.complete);
    assert_eq!(reenabled.converted, 0);
    assert_eq!(reenabled.lod_chunks, enabled.lod_chunks);
}

#[tokio::test]
async fn metadata_no_lod_omits_chunks_and_preserves_the_source_package() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("rebuilt");
    generate_data(&data);
    let enabled = convert(&data, &source).await;
    assert!(enabled.lod_chunks > 0);
    let source_manifest = fs::read(source.join("lod-manifest.json")).unwrap();
    let mut config = converter::PipelineConfig::new(&data, &output);
    config.no_lod = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = converter::AssetPipeline::rebuild_metadata_async(config, &source, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete);
    assert_eq!(report.converted, 0);
    assert_eq!(report.lod_chunks, 0);
    assert!(!output.join("lod-manifest.json").exists());
    assert!(!output.join("lod").exists());
    assert_eq!(
        fs::read(source.join("lod-manifest.json")).unwrap(),
        source_manifest
    );
    assert!(
        shared::world_assets::validate_lod_build_contract(
            &output,
            shared::LOD_CONVERTER_SCHEMA_VERSION
        )
        .is_ok()
    );
}

#[tokio::test]
async fn v115_resumed_no_lod_discards_staged_lod_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("assets");
    generate_data(&data);
    assert!(convert(&data, &output).await.lod_chunks > 0);
    let staging = directory.path().join("assets.staging-lod-retry");
    fs::rename(&output, &staging).unwrap();
    let mut config = converter::PipelineConfig::new(&data, &output);
    config.no_lod = true;
    config.resume_staging = Some(staging);
    let report = convert_config(config).await;
    assert!(report.complete);
    assert_eq!(report.lod_chunks, 0);
    assert!(indexed_chunks(&output).is_empty());
    assert!(!output.join("lod-manifest.json").exists());
    assert!(!output.join("lod").exists());
}

#[tokio::test]
async fn packed_sidecars_compile_lod_and_loose_settings_override_them() {
    for loose_override in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        generate_data(&data);
        if loose_override {
            fs::create_dir(data.join("LodSettings")).unwrap();
            fs::write(data.join("LodSettings/GeneratedWorld.LOD"), LOOSE_SETTINGS).unwrap();
        }
        let output = directory.path().join("modern");
        let report = convert(&data, &output).await;

        assert!(report.complete, "{:?}", report.warnings);
        assert!(report.lod_warnings.is_empty(), "{:?}", report.lod_warnings);
        assert!(report.lod_chunks > 0, "archive sidecar must produce chunks");
        let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
        let origin: (i32, i32) = connection
            .query_row(
                "SELECT lod_origin_x, lod_origin_y FROM worldspaces WHERE editor_id = 'GeneratedWorld'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(origin, if loose_override { (-8, -8) } else { (-4, -4) });
        let indexed: u64 = connection
            .query_row("SELECT count(*) FROM lod_chunks", [], |row| row.get(0))
            .unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("lod-manifest.json")).unwrap()).unwrap();
        assert_eq!(indexed, report.lod_chunks);
        assert_eq!(manifest["chunks"], indexed);
        let paths: Vec<String> = connection
            .prepare("SELECT payload_path FROM lod_chunks")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for path in paths {
            let bytes = fs::read(output.join(path)).unwrap();
            let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
            let gltf: serde_json::Value =
                serde_json::from_slice(&bytes[20..20 + json_length]).unwrap();
            assert_eq!(
                gltf["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["index"], 0,
                "LOD-V9: terrain must carry baked LAND albedo, not vertex tint alone"
            );
            assert_eq!(gltf["images"][0]["mimeType"], "image/ktx2");
            assert!(gltf["images"][0]["bufferView"].is_u64());
        }
    }
}

#[tokio::test]
async fn malformed_loose_settings_do_not_fall_back_to_packed_settings() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    generate_data(&data);
    fs::create_dir(data.join("LodSettings")).unwrap();
    fs::write(data.join("LodSettings/GeneratedWorld.LOD"), b"invalid").unwrap();
    let output = directory.path().join("modern");
    let report = convert(&data, &output).await;

    assert!(report.complete);
    assert_eq!(report.lod_chunks, 0);
    assert_eq!(report.lod_warnings.len(), 1);
    assert!(report.lod_warnings[0].contains("invalid LOD settings"));
    assert!(!output.join("lod-manifest.json").exists());
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    let origin: (Option<i32>, Option<i32>) = connection
        .query_row(
            "SELECT lod_origin_x, lod_origin_y FROM worldspaces",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, (None, None));
}

fn indexed_chunks(output: &Path) -> Vec<(i32, i32, i32, String)> {
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    connection
        .prepare("SELECT tier, anchor_x, anchor_y, content_hash FROM lod_chunks ORDER BY tier, anchor_x, anchor_y")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test]
async fn resumed_lod_with_changed_origin_matches_a_clean_build() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    generate_data(&data);
    let output = directory.path().join("modern");
    convert(&data, &output).await;
    let staging = output.with_extension("staging-lod-retry");
    fs::rename(&output, &staging).unwrap();

    let mut resumed = converter::PipelineConfig::new(&data, &output);
    resumed.resume_staging = Some(staging);
    resumed
        .lod_origins
        .insert("GeneratedWorld".into(), [96, 96]);
    let report = convert_config(resumed.clone()).await;
    let clean_output = directory.path().join("clean");
    let mut clean = resumed;
    clean.output_dir = clean_output.clone();
    clean.resume_staging = None;
    convert_config(clean).await;

    assert_eq!(indexed_chunks(&output), indexed_chunks(&clean_output));
    assert_eq!(indexed_chunks(&output).len() as u64, report.lod_chunks);
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    let indexed: u64 = connection
        .query_row("SELECT count(*) FROM lod_chunks_spatial", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(indexed, report.lod_chunks);
}

#[tokio::test]
async fn resumed_lod_drops_removed_sidecars_and_the_previous_manifest() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    generate_data(&data);
    let output = directory.path().join("modern");
    convert(&data, &output).await;
    let staging = output.with_extension("staging-lod-retry");
    fs::rename(&output, &staging).unwrap();
    fs::remove_file(data.join("Skyrim - Misc.bsa")).unwrap();

    let mut config = converter::PipelineConfig::new(&data, &output);
    config.resume_staging = Some(staging);
    let report = convert_config(config).await;
    assert!(report.complete);
    assert_eq!(report.lod_chunks, 0);
    assert!(indexed_chunks(&output).is_empty());
    assert!(!output.join("lod-manifest.json").exists());
    assert!(!output.join("lod").exists());
    assert!(!output.join("vfs/lodsettings/generatedworld.lod").exists());
}

#[tokio::test]
async fn resumed_conversion_drops_removed_textures_and_stale_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    generate_data(&data);
    let output = directory.path().join("modern");
    assert!(convert(&data, &output).await.complete);
    let staging = output.with_extension("staging-assets-retry");
    fs::rename(&output, &staging).unwrap();
    fs::write(staging.join("scripts/obsolete.luau"), b"-- removed script").unwrap();
    for sidecar in [
        "metadata-rebuild.json",
        "metadata-source-conversion-manifest.json",
    ] {
        fs::write(staging.join(sidecar), b"obsolete provenance").unwrap();
    }
    fs::remove_file(data.join(layout::GENERATED_DIFFUSE_PATH)).unwrap();
    let mut resumed = converter::PipelineConfig::new(&data, &output);
    resumed.resume_staging = Some(staging);
    assert!(convert_config(resumed).await.complete);
    let clean = directory.path().join("clean");
    assert!(convert(&data, &clean).await.complete);
    let published = |root: &Path| -> converter::cache::ConversionManifest {
        serde_json::from_slice(&fs::read(root.join("conversion-manifest.json")).unwrap()).unwrap()
    };
    assert_eq!(published(&output).entries, published(&clean).entries);
    assert_eq!(
        published(&output).pruned_texture_references,
        published(&clean).pruned_texture_references
    );
    let asset_files = |root: &Path| {
        let mut files = std::collections::BTreeMap::new();
        for directory in ["meshes", "textures", "scripts"] {
            for entry in walkdir::WalkDir::new(root.join(directory)) {
                let entry = entry.unwrap();
                if entry.file_type().is_file() {
                    files.insert(
                        entry.path().strip_prefix(root).unwrap().to_path_buf(),
                        converter::cache::hash_file(entry.path()).unwrap(),
                    );
                }
            }
        }
        files
    };
    assert_eq!(asset_files(&output), asset_files(&clean));
    for sidecar in [
        "metadata-rebuild.json",
        "metadata-source-conversion-manifest.json",
    ] {
        assert!(!output.join(sidecar).exists());
    }
}

#[tokio::test]
async fn combined_export_keeps_grass_links_lod_origins_and_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    generate_data(&data);
    fn sub(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        [
            tag.as_slice(),
            &(payload.len() as u16).to_le_bytes(),
            payload,
        ]
        .concat()
    }
    fn record(tag: &[u8; 4], id: u32, payload: &[u8]) -> Vec<u8> {
        [
            tag.as_slice(),
            &(payload.len() as u32).to_le_bytes(),
            &[0; 4],
            &id.to_le_bytes(),
            &[0; 8],
            payload,
        ]
        .concat()
    }
    let mut plugin = record(b"TES4", 0, &[]);
    plugin.extend(record(b"GRAS", 0x801, &sub(b"EDID", b"CombinedGrass\0")));
    plugin.extend(record(
        b"LTEX",
        0x802,
        &sub(b"GNAM", &0x801u32.to_le_bytes()),
    ));
    fs::write(data.join("Grass.esp"), plugin).unwrap();
    let output = directory.path().join("combined");
    let report = convert(&data, &output).await;
    assert!(report.complete, "{:?}", report.warnings);
    assert!(report.lod_chunks > 0);
    let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM grass_types g JOIN landscape_texture_grasses l ON g.id=l.gras_id",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let origin: (i32, i32) = db
        .query_row(
            "SELECT lod_origin_x,lod_origin_y FROM worldspaces WHERE editor_id='GeneratedWorld'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, (-4, -4));
    let world_schema: u32 = db
        .query_row("SELECT version FROM schema_info", [], |row| row.get(0))
        .unwrap();
    assert_eq!(world_schema, 7);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("lod-manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["converter_schema"], 24);
    assert_eq!(manifest["world_database_schema"], 7);
    assert_eq!(manifest["chunks"], report.lod_chunks);
}

#[tokio::test]
async fn v118_lod_reuse_is_incremental_and_rejects_damaged_or_unproven_payloads() {
    use dummy_content::esm;
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("assets");
    generate_data(&data);
    let cells = [
        esm::Cell {
            grid_x: 0,
            grid_y: 0,
        },
        esm::Cell {
            grid_x: 64,
            grid_y: 0,
        },
    ];
    let plugin = esm::plugin(&esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &cells,
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    fs::write(data.join("Skyrim.esm"), &plugin).unwrap();
    let config = || {
        let mut config = converter::PipelineConfig::new(&data, &output);
        config.cpu_jobs = 2;
        config
    };
    let cold = convert_config(config()).await;
    assert!(cold.complete);
    assert_eq!(cold.lod_chunks, 6);
    assert_eq!(cold.lod_cache_hits, 0);
    let manifest_path = output.join("lod-manifest.json");
    let original_manifest = fs::read(&manifest_path).unwrap();
    let original: serde_json::Value = serde_json::from_slice(&original_manifest).unwrap();
    let paths: Vec<_> = original["chunk_inputs"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let payloads: Vec<_> = paths
        .iter()
        .map(|path| fs::read(output.join(path)).unwrap())
        .collect();
    let warm = convert_config(config()).await;
    assert!(warm.complete);
    assert_eq!(warm.converted, 0);
    assert_eq!(warm.lod_cache_hits, cold.lod_chunks);
    assert_eq!(fs::read(&manifest_path).unwrap(), original_manifest);
    for (path, bytes) in paths.iter().zip(&payloads) {
        assert_eq!(fs::read(output.join(path)).unwrap(), *bytes);
    }

    let derived = directory.path().join("derived");
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let metadata = converter::AssetPipeline::rebuild_metadata_async(
        converter::PipelineConfig::new(&data, &derived),
        &output,
        tx,
    )
    .await
    .unwrap();
    drain.await.unwrap();
    assert_eq!(metadata.lod_cache_hits, cold.lod_chunks);
    assert_eq!(fs::read(&manifest_path).unwrap(), original_manifest);

    // Only the damaged payload rebuilds; input proof cannot hide corruption.
    fs::write(output.join(&paths[0]), b"broken GLB").unwrap();
    let repaired = convert_config(config()).await;
    assert!(repaired.complete);
    assert_eq!(repaired.lod_cache_hits, cold.lod_chunks - 1);
    assert_eq!(fs::read(output.join(&paths[0])).unwrap(), payloads[0]);

    // A same-numbered legacy manifest has no explicit current compiler proof.
    let mut unproven = original.clone();
    unproven.as_object_mut().unwrap().remove("compiler_version");
    fs::write(&manifest_path, serde_json::to_vec(&unproven).unwrap()).unwrap();
    let rebuilt = convert_config(config()).await;
    assert_eq!(rebuilt.lod_cache_hits, 0);
    assert_eq!(rebuilt.lod_chunks, cold.lod_chunks);

    // Editing one LAND height affects its three tiers, leaving the other cell reusable.
    let mut changed_plugin = plugin.clone();
    let height = changed_plugin
        .windows(4)
        .position(|bytes| bytes == b"VHGT")
        .unwrap()
        + 6;
    changed_plugin[height..height + 4].copy_from_slice(&16.0f32.to_le_bytes());
    fs::write(data.join("Skyrim.esm"), changed_plugin).unwrap();
    let changed = convert_config(config()).await;
    assert!(changed.complete);
    assert_eq!(changed.lod_chunks, cold.lod_chunks);
    assert_eq!(changed.lod_cache_hits, 3);

    // An origin change invalidates placement and layout even with identical source cells.
    let mut moved_config = config();
    moved_config
        .lod_origins
        .insert(layout::GENERATED_WORLDSPACE.to_owned(), [0, 0]);
    let moved = convert_config(moved_config).await;
    assert_eq!(moved.lod_cache_hits, 0);
    assert_eq!(moved.lod_chunks, cold.lod_chunks);

    let diffuse = data.join(layout::GENERATED_DIFFUSE_PATH);
    let mut dds = ddsfile::Dds::read(std::io::Cursor::new(fs::read(&diffuse).unwrap())).unwrap();
    dds.data[0] ^= 0xff;
    let mut bytes = Vec::new();
    dds.write(&mut bytes).unwrap();
    fs::write(diffuse, bytes).unwrap();
    let mut material_config = config();
    material_config
        .lod_origins
        .insert(layout::GENERATED_WORLDSPACE.to_owned(), [0, 0]);
    let material = convert_config(material_config).await;
    assert!(material.complete);
    assert_eq!(material.lod_chunks, cold.lod_chunks);
    assert_eq!(material.lod_cache_hits, 0);

    fs::write(
        data.join("Skyrim.esm"),
        esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &cells[1..],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap(),
    )
    .unwrap();
    let removed = convert_config(config()).await;
    assert!(removed.complete);
    assert_eq!(removed.lod_chunks, 3);
    let database = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    let source_cells: Vec<String> = database
        .prepare("SELECT source_cells FROM lod_chunks")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(source_cells, ["64,0", "64,0", "64,0"]);
}
