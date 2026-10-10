mod common;

use converter::{
    AssetPipeline, PipelineConfig,
    cache::{ConversionManifest, hash_file},
};
use dummy_content::{Entry, bsa, esm, layout, nif};
use rusqlite::Connection;
use std::{fs, path::Path};

const PACKED_SETTINGS: [u8; 16] = [0xfc, 0xff, 0xfc, 0xff, 32, 0, 0, 0, 4, 0, 0, 0, 32, 0, 0, 0];
const VOLCANIC_MODEL: &str = "meshes/landscape/grass/dlc02volcanicashgrass01.nif";
const VOLCANIC_GLB: &str = "meshes/landscape/grass/dlc02volcanicashgrass01.glb";
const VOLCANIC_ORIGINAL: &str = "textures/landscape/grass/volcanicashgrass02_n.dds";
const VOLCANIC_TARGET: &str = "textures/dlc02/landscape/grass/volcanicashgrass02_n.dds";

/// Adds a scoped volcanic model whose authored normal is absent but DLC02 source exists.
fn add_volcanic_source(data: &Path) {
    let model = data.join(VOLCANIC_MODEL);
    fs::create_dir_all(model.parent().unwrap()).unwrap();
    fs::write(
        model,
        nif::static_shape(&nif::StaticShape {
            name: "VolcanicReuseTriangle",
            positions: &[[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 3.0, 0.0]],
            normals: &[[0.0, 0.0, 1.0]; 3],
            uvs: &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            indices: &[[0, 1, 2]],
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: VOLCANIC_ORIGINAL,
        })
        .unwrap(),
    )
    .unwrap();
    let target = data.join(VOLCANIC_TARGET);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(data.join(layout::GENERATED_NORMAL_PATH), target).unwrap();
}

/// Reads the external normal URI without reproducing the repair's path mapping.
fn volcanic_normal_uri(glb: &Path) -> Option<String> {
    converter::mesh::MeshConverter::glb_texture_dependencies(glb)
        .unwrap()
        .into_iter()
        .find(|dependency| dependency.uri.contains("volcanicashgrass02_n"))
        .map(|dependency| dependency.uri)
}

fn generate(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm,lodsettings").unwrap(),
    )
    .unwrap();
    let plugin = esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    };
    let light = esm::Light {
        model_path: None,
        reference_flags: 0x800,
        enable_parent: Some((0x1234, 1)),
        ..esm::PRESET_LIGHT
    };
    fs::write(
        data.join("Skyrim.esm"),
        esm::plugin_with_lights(&plugin, &light).unwrap(),
    )
    .unwrap();
    fs::write(
        data.join("Skyrim - Misc.bsa"),
        bsa::v105(
            &[
                Entry {
                    name: "LODSettings/GeneratedWorld.LOD",
                    data: &PACKED_SETTINGS,
                },
                Entry {
                    name: "misc/unrelated.txt",
                    data: b"not a setting",
                },
            ],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
}

async fn convert(data: &Path, output: &Path) -> converter::PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(common::cpu_lod_config(data, output), tx)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    report
}

async fn rebuild(
    data: &Path,
    source: &Path,
    output: &Path,
) -> color_eyre::Result<converter::PipelineReport> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result =
        AssetPipeline::rebuild_metadata_async(common::cpu_lod_config(data, output), source, tx)
            .await;
    drain.await.unwrap();
    result
}

fn schema_four_source(source: &Path) {
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(source.join("conversion-manifest.json")).unwrap())
            .unwrap();
    manifest.schema_version = 15;
    manifest.configuration_hash =
        converter::cache::configuration_hash_for_schema(&common::cpu_lod_config(".", "."), 15)
            .unwrap();
    manifest
        .save(&source.join("conversion-manifest.json"))
        .unwrap();
    let connection = Connection::open(source.join("skyrim_world.db")).unwrap();
    connection
        .execute_batch(
            "UPDATE schema_info SET version=4;
        ALTER TABLE \"references\" DROP COLUMN header_flags;
        ALTER TABLE \"references\" DROP COLUMN enable_parent_id;
        ALTER TABLE \"references\" DROP COLUMN enable_parent_flags;
        ALTER TABLE worldspaces DROP COLUMN lod_origin_x;
        ALTER TABLE worldspaces DROP COLUMN lod_origin_y;",
        )
        .unwrap();
}

fn native_configuration_hash(config: &PipelineConfig, zstd_level: u32) -> String {
    converter::cache::hash_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "schema": 16,
            "texture_etc1s_quality": config.texture_fallback_quality,
            "texture_uastc_level": config.texture_uastc_level,
            "texture_zstd_level": zstd_level,
            "script_abi_version": config.script_abi_version,
        }))
        .unwrap(),
    )
}

#[tokio::test]
async fn native_retained_configuration_survives_rebuilds_without_becoming_cache_hits() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    let repeated = directory.path().join("derived-again");
    generate(&data);
    convert(&data, &source).await;
    let config = common::cpu_lod_config(&data, &output);
    let producer_hash = native_configuration_hash(&config, 6);
    assert_eq!(
        producer_hash,
        "ebc4fe2f7d531e796e6a5e22b70a1f6a877c01e963b54b416b789d0f64312427"
    );
    let path = source.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest.schema_version = 16;
    manifest.configuration_hash = producer_hash.clone();
    manifest.save(&path).unwrap();
    let original = hash_file(&path).unwrap();
    rebuild(&data, &source, &output).await.unwrap();
    rebuild(&data, &output, &repeated).await.unwrap();
    assert_eq!(hash_file(&path).unwrap(), original);
    let retained: ConversionManifest =
        serde_json::from_slice(&fs::read(repeated.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(
        retained.retained_asset_configuration_hash,
        Some(producer_hash.clone())
    );
    assert_eq!(retained.retained_mesh_schema_version, Some(16));
    for entry in manifest.entries.values() {
        assert_eq!(
            hash_file(&repeated.join(&entry.output)).unwrap(),
            entry.output_hash
        );
    }
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(repeated.join("metadata-rebuild.json")).unwrap()).unwrap();
    assert_eq!(
        provenance["retained_asset_configuration_hash"],
        producer_hash
    );
    let mut missing_producer = retained;
    missing_producer.retained_asset_configuration_hash = None;
    missing_producer
        .save(&repeated.join("conversion-manifest.json"))
        .unwrap();
    let error = rebuild(&data, &repeated, &directory.path().join("missing-producer"))
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("missing retained producer configuration"));
    let report = convert(&data, &repeated).await;
    assert_eq!(
        report.cache_hits, 0,
        "native producer bytes must not become normal converter cache hits"
    );
    let current: ConversionManifest =
        serde_json::from_slice(&fs::read(repeated.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(current.retained_asset_configuration_hash, None);
}

#[tokio::test]
async fn metadata_rebuild_rejects_changed_native_producer_settings() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    generate(&data);
    convert(&data, &source).await;
    let path = source.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for (index, retained) in [false, true].into_iter().enumerate() {
        let output = directory.path().join(format!("rejected-{index}"));
        let changed = native_configuration_hash(&common::cpu_lod_config(&data, &output), 7);
        if retained {
            manifest.schema_version = converter::cache::CONVERTER_SCHEMA_VERSION;
            manifest.configuration_hash =
                converter::cache::configuration_hash(&common::cpu_lod_config(&data, &output))
                    .unwrap();
            manifest.retained_mesh_schema_version = Some(16);
            manifest.retained_asset_configuration_hash = Some(changed);
        } else {
            manifest.schema_version = 16;
            manifest.configuration_hash = changed;
        }
        manifest.save(&path).unwrap();
        let error = rebuild(&data, &source, &output).await.unwrap_err();
        assert!(format!("{error:?}").contains("configuration does not match"));
        assert!(!output.exists());
    }
}

#[tokio::test]
async fn metadata_rebuild_reuses_bytes_and_recovers_authoritative_flags() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    convert(&data, &source).await;
    schema_four_source(&source);
    let old_manifest = hash_file(&source.join("conversion-manifest.json")).unwrap();
    let old_db = hash_file(&source.join("skyrim_world.db")).unwrap();
    let source_manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(source.join("conversion-manifest.json")).unwrap())
            .unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete);
    assert_eq!(report.converted, 0);
    assert!(report.cache_hits > 0 && report.lod_chunks > 0);
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        old_manifest
    );
    assert_eq!(hash_file(&source.join("skyrim_world.db")).unwrap(), old_db);
    for entry in source_manifest.entries.values() {
        assert_eq!(
            hash_file(&output.join(&entry.output)).unwrap(),
            entry.output_hash
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_ne!(
                fs::metadata(source.join(&entry.output)).unwrap().ino(),
                fs::metadata(output.join(&entry.output)).unwrap().ino()
            );
        }
    }
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    let flags: (u32, u32, u32) = connection.query_row(
        "SELECT header_flags, enable_parent_id, enable_parent_flags FROM \"references\" WHERE header_flags=2048", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(flags, (0x800, 0x1234, 1));
    let version: u32 = connection
        .query_row("SELECT version FROM schema_info", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, shared::WORLD_DATABASE_SCHEMA_VERSION);
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap()).unwrap();
    assert_eq!(provenance["source_converter_schema"], 15);
    assert_eq!(provenance["source_retained_mesh_schema"], 15);
    assert_eq!(provenance["source_manifest_hash"], old_manifest);
    assert_eq!(
        hash_file(&output.join("metadata-source-conversion-manifest.json")).unwrap(),
        old_manifest
    );
    assert!(output.join("lod-manifest.json").is_file());
}

/// Retained asset provenance allows proven producer-24 GLBs but rebuilds its textures.
#[tokio::test]
async fn metadata_rebuild_preserves_retained_mesh_cache_contract() {
    for source_schema in [
        15,
        16,
        19,
        20,
        21,
        22,
        23,
        24,
        converter::cache::CONVERTER_SCHEMA_VERSION,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        let repeated = directory.path().join("derived-again");
        generate(&data);
        convert(&data, &source).await;
        if source_schema == 15 {
            schema_four_source(&source);
        } else {
            let path = source.join("conversion-manifest.json");
            let mut manifest: ConversionManifest =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            manifest.schema_version = source_schema;
            manifest.configuration_hash = converter::cache::configuration_hash_for_schema(
                &common::cpu_lod_config(&data, &output),
                source_schema,
            )
            .unwrap();
            manifest.save(&path).unwrap();
        }
        rebuild(&data, &source, &output).await.unwrap();
        rebuild(&data, &output, &repeated).await.unwrap();
        let retained: ConversionManifest =
            serde_json::from_slice(&fs::read(repeated.join("conversion-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(retained.retained_mesh_schema_version, Some(source_schema));
        let meshes = retained
            .entries
            .values()
            .filter(|entry| entry.output.ends_with(".glb"))
            .count();
        assert!(meshes > 0);
        let eligible =
            ConversionManifest::load(&repeated.join("conversion-manifest.json")).unwrap();
        assert_eq!(
            eligible
                .entries
                .values()
                .filter(|entry| entry.output.ends_with(".glb"))
                .count(),
            if source_schema == 24 || source_schema == converter::cache::CONVERTER_SCHEMA_VERSION {
                meshes
            } else {
                0
            },
            "metadata-only upgrades preserve original provenance; only known compatible producer meshes are normal cache hits"
        );
        let report = convert(&data, &repeated).await;
        let regenerated = if source_schema == converter::cache::CONVERTER_SCHEMA_VERSION {
            0
        } else {
            retained
                .entries
                .values()
                .filter(|entry| {
                    let output = entry.output.to_ascii_lowercase();
                    !output.ends_with(".luau") && !(source_schema == 24 && output.ends_with(".glb"))
                })
                .count()
        };
        let current: ConversionManifest =
            serde_json::from_slice(&fs::read(repeated.join("conversion-manifest.json")).unwrap())
                .unwrap();
        assert!(retained.archives.is_empty());
        let extracted = current
            .archives
            .values()
            .map(|archive| archive.files.len())
            .sum::<usize>();
        assert_eq!(report.converted, (regenerated + extracted) as u64);
        assert_eq!(
            report.cache_hits,
            (retained.entries.len() - regenerated) as u64
        );
        assert_eq!(current.retained_mesh_schema_version, None);
    }
}

/// Invalid source/retained producer pairs are stamped faithfully and rejected.
#[tokio::test]
async fn metadata_rebuild_rejects_unsupported_mesh_provenance() {
    for (schema, mesh_schema) in [
        (15, 16),
        (21, 17),
        (21, 18),
        (24, 25),
        (
            converter::cache::CONVERTER_SCHEMA_VERSION,
            converter::cache::CONVERTER_SCHEMA_VERSION + 1,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        generate(&data);
        convert(&data, &source).await;
        if schema == 15 {
            schema_four_source(&source);
        }
        let mut manifest: ConversionManifest =
            serde_json::from_slice(&fs::read(source.join("conversion-manifest.json")).unwrap())
                .unwrap();
        manifest.schema_version = schema;
        manifest.retained_mesh_schema_version = Some(mesh_schema);
        manifest
            .save(&source.join("conversion-manifest.json"))
            .unwrap();
        let error = rebuild(&data, &source, &output)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("unsupported retained mesh cache contract"),
            "{error}"
        );
        assert!(!output.exists());
    }
}

#[tokio::test]
async fn markerless_legacy_metadata_never_promotes_retained_meshes() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let legacy = directory.path().join("legacy-derived");
    let output = directory.path().join("derived-again");
    generate(&data);
    convert(&data, &source).await;
    schema_four_source(&source);
    rebuild(&data, &source, &legacy).await.unwrap();
    let manifest_path = legacy.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest.retained_mesh_schema_version = None;
    manifest.save(&manifest_path).unwrap();
    fs::remove_file(legacy.join("metadata-source-conversion-manifest.json")).unwrap();
    let eligible = ConversionManifest::load(&manifest_path).unwrap();
    assert!(
        eligible
            .entries
            .values()
            .all(|entry| !entry.output.ends_with(".glb")),
        "markerless metadata-only output must not become a mesh cache hit"
    );
    let error = rebuild(&data, &legacy, &output)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("missing retained mesh provenance"),
        "{error}"
    );
    assert!(!output.exists());
}

#[tokio::test]
async fn metadata_rebuild_rejects_changed_plugins_and_assets_without_publication() {
    for corrupt_plugin in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        generate(&data);
        convert(&data, &source).await;
        if corrupt_plugin {
            fs::write(data.join("Skyrim.esm"), b"different plugin").unwrap();
        } else {
            let manifest =
                ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
            let asset = &manifest.entries.values().next().unwrap().output;
            fs::write(source.join(asset), b"corrupt asset").unwrap();
        }
        let error = rebuild(&data, &source, &output)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("checksum mismatch"), "{error}");
        assert!(!output.exists());
        assert!(!fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("staging")
        }));
    }
}

#[tokio::test]
async fn metadata_rebuild_requires_new_disjoint_output() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    generate(&data);
    convert(&data, &source).await;
    let original = hash_file(&source.join("conversion-manifest.json")).unwrap();
    for output in [&source, &source.join("nested"), &data.join("nested")] {
        assert!(rebuild(&data, &source, output).await.is_err());
    }
    let existing = directory.path().join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("sentinel"), b"keep").unwrap();
    assert!(rebuild(&data, &source, &existing).await.is_err());
    assert_eq!(fs::read(existing.join("sentinel")).unwrap(), b"keep");
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        original
    );
}

#[tokio::test]
async fn metadata_rebuild_preserves_package_snapshot_after_unrelated_data_asset_changes() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    convert(&data, &source).await;
    let manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(source.join("conversion-manifest.json")).unwrap())
            .unwrap();
    fs::write(
        data.join(layout::GENERATED_MODEL_PATH),
        b"changed NIF override",
    )
    .unwrap();
    fs::write(
        data.join(layout::GENERATED_NORMAL_PATH),
        b"changed normal DDS override",
    )
    .unwrap();
    fs::write(data.join("scripts/generated.pex"), b"changed PEX override").unwrap();
    fs::write(data.join("meshes/new.nif"), b"new asset").unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete);
    assert_eq!(report.converted, 0);
    for entry in manifest.entries.values() {
        assert_eq!(
            hash_file(&output.join(&entry.output)).unwrap(),
            entry.output_hash
        );
        assert_eq!(
            hash_file(&source.join(&entry.output)).unwrap(),
            entry.output_hash
        );
    }
    assert!(!output.join("meshes/new.glb").exists());
}

#[tokio::test]
async fn metadata_rebuild_resolves_current_packed_settings_and_omits_stale_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    convert(&data, &source).await;
    fs::remove_file(data.join("lodsettings/GeneratedWorld.lod")).unwrap();
    fs::write(
        data.join("Unmatched - Misc.bsa"),
        bsa::v105(
            &[Entry::new(
                "LODSettings/GeneratedWorld.LOD",
                b"invalid unmatched override",
            )],
            bsa::Compression::None,
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(source.join("lod/stale.glb"), b"obsolete generation").unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.lod_chunks > 0);
    assert!(
        report
            .lod_warnings
            .iter()
            .any(|warning| warning.contains("no matching source-package plugin"))
    );
    assert!(!output.join("lod/stale.glb").exists());
    assert!(!output.join("vfs/misc/unrelated.txt").exists());
    let connection = Connection::open(output.join("skyrim_world.db")).unwrap();
    let origin: (i32, i32) = connection
        .query_row(
            "SELECT lod_origin_x,lod_origin_y FROM worldspaces WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, (-4, -4));
    let indexed: u64 = connection
        .query_row("SELECT count(*) FROM lod_chunks_spatial", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(indexed, report.lod_chunks);
}

/// Verified reuse preserves the repaired URI and bytes despite changed current Data assets.
#[tokio::test]
async fn metadata_rebuild_preserves_repaired_volcanic_normal_uri() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    generate(&data);
    add_volcanic_source(&data);
    convert(&data, &source).await;
    let uri = volcanic_normal_uri(&source.join(VOLCANIC_GLB)).unwrap();
    assert!(uri.contains("textures/dlc02/landscape/grass/volcanicashgrass02_n"));
    let manifest = ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
    assert!(
        manifest.entries[VOLCANIC_MODEL]
            .source_hash
            .ends_with(":volcanic-normal-repair:1:original=false:target=true")
    );
    let source_manifest_hash = hash_file(&source.join("conversion-manifest.json")).unwrap();
    let retained_hash = hash_file(&source.join(VOLCANIC_GLB)).unwrap();
    assert!(
        !source.join("vfs").exists(),
        "exercise normal runtime-package reuse"
    );

    // These changes would undo the repair during normal conversion. Reuse keeps SOURCE's bytes.
    let original = data.join(VOLCANIC_ORIGINAL);
    fs::create_dir_all(original.parent().unwrap()).unwrap();
    fs::copy(data.join(layout::GENERATED_NORMAL_PATH), original).unwrap();
    fs::remove_file(data.join(VOLCANIC_TARGET)).unwrap();
    fs::write(data.join(VOLCANIC_MODEL), b"changed current NIF override").unwrap();
    let report = rebuild(&data, &source, &output).await.unwrap();
    assert!(report.complete);
    assert_eq!(report.converted, 0);
    assert_eq!(volcanic_normal_uri(&output.join(VOLCANIC_GLB)), Some(uri));
    assert_eq!(
        hash_file(&output.join(VOLCANIC_GLB)).unwrap(),
        retained_hash
    );
    for entry in manifest.entries.values() {
        assert_eq!(
            hash_file(&output.join(&entry.output)).unwrap(),
            entry.output_hash
        );
    }
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap()).unwrap();
    assert_eq!(
        provenance["replay_verified_pruned_assets"],
        serde_json::json!([])
    );
    assert_eq!(
        hash_file(&source.join("conversion-manifest.json")).unwrap(),
        source_manifest_hash
    );
}

/// Legacy replay accepts old and scoped hashes using SOURCE's context and never applies repair.
#[tokio::test]
async fn metadata_rebuild_replays_volcanic_prunes_with_legacy_and_scoped_source_hashes() {
    for (scoped_hash, wrong_presence, corrupt_nif) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (false, false, true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        let preprune = directory.path().join("preprune");
        generate(&data);
        add_volcanic_source(&data);
        convert(&data, &source).await;

        // Build the historical raw GLB and retain a pure prune with no DLC02 repair available.
        let candidate = preprune.join(VOLCANIC_GLB);
        converter::mesh::MeshConverter::convert_nif_to_glb(
            data.join(VOLCANIC_MODEL),
            candidate.clone(),
        )
        .unwrap();
        let original_hash = hash_file(&candidate).unwrap();
        let original_size = fs::metadata(&candidate).unwrap().len();
        for dependency in converter::mesh::MeshConverter::glb_texture_dependencies(&candidate)
            .unwrap()
            .into_iter()
            .filter(|dependency| !dependency.uri.contains("volcanicashgrass02_n"))
        {
            let published = converter::asset_path::resolve_asset_uri(
                &source,
                &source.join(VOLCANIC_GLB),
                &dependency.uri,
            )
            .unwrap();
            let retained =
                converter::asset_path::resolve_asset_uri(&preprune, &candidate, &dependency.uri)
                    .unwrap();
            fs::create_dir_all(retained.parent().unwrap()).unwrap();
            fs::copy(published, retained).unwrap();
        }
        let pruned =
            converter::mesh::MeshConverter::prune_dangling_texture_uris(&preprune).unwrap();
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0].removed_uris.len(), 1);
        let missing = converter::asset_path::resolve_asset_uri(
            &preprune,
            &candidate,
            &pruned[0].removed_uris[0],
        )
        .unwrap();
        let missing = missing
            .strip_prefix(&preprune)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        assert_eq!(
            missing,
            "textures/landscape/grass/volcanicashgrass02_n.opensky-wrap0.ktx2"
        );
        assert!(volcanic_normal_uri(&candidate).is_none());
        fs::copy(&candidate, source.join(VOLCANIC_GLB)).unwrap();

        let retained_nif = source.join("vfs").join(VOLCANIC_MODEL);
        fs::create_dir_all(retained_nif.parent().unwrap()).unwrap();
        fs::copy(data.join(VOLCANIC_MODEL), &retained_nif).unwrap();
        let mut manifest =
            ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
        let entry = manifest.entries.get_mut(VOLCANIC_MODEL).unwrap();
        entry.output_hash = original_hash;
        entry.output_size = original_size;
        // No skeleton is present: the pre-repair contract is exactly the raw NIF checksum.
        entry.source_hash = hash_file(&retained_nif).unwrap();
        if scoped_hash {
            entry.source_hash.push_str(if wrong_presence {
                ":volcanic-normal-repair:1:original=true:target=true"
            } else {
                ":volcanic-normal-repair:1:original=false:target=true"
            });
        }
        manifest
            .pruned_texture_references
            .insert(VOLCANIC_GLB.into(), [missing].into());
        manifest
            .save(&source.join("conversion-manifest.json"))
            .unwrap();
        let source_manifest_hash = hash_file(&source.join("conversion-manifest.json")).unwrap();
        let retained_hash = hash_file(&source.join(VOLCANIC_GLB)).unwrap();
        if corrupt_nif {
            fs::write(&retained_nif, b"tampered retained source NIF").unwrap();
        }

        // Data's current presence bits differ; only the retained package can verify its hash.
        let original = data.join(VOLCANIC_ORIGINAL);
        fs::create_dir_all(original.parent().unwrap()).unwrap();
        fs::copy(data.join(layout::GENERATED_NORMAL_PATH), original).unwrap();
        fs::remove_file(data.join(VOLCANIC_TARGET)).unwrap();
        let result = rebuild(&data, &source, &output).await;
        if wrong_presence || corrupt_nif {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("pruned GLB source checksum mismatch"),
                "{error}"
            );
            assert!(!output.exists());
        } else {
            let report = result.unwrap();
            assert!(report.complete);
            assert_eq!(
                hash_file(&output.join(VOLCANIC_GLB)).unwrap(),
                retained_hash
            );
            assert!(volcanic_normal_uri(&output.join(VOLCANIC_GLB)).is_none());
            let updated =
                ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
            assert_eq!(updated.entries[VOLCANIC_MODEL].output_hash, retained_hash);
            let provenance: serde_json::Value =
                serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap())
                    .unwrap();
            assert_eq!(
                provenance["replay_verified_pruned_assets"],
                serde_json::json!([VOLCANIC_GLB])
            );
        }
        assert_eq!(
            hash_file(&source.join(VOLCANIC_GLB)).unwrap(),
            retained_hash
        );
        assert_eq!(
            hash_file(&source.join("conversion-manifest.json")).unwrap(),
            source_manifest_hash
        );
    }
}

#[tokio::test]
async fn metadata_rebuild_v87_accepts_old_prune_hashes_only_after_exact_source_replay() {
    for (corrupt, with_skeleton, with_vfs) in [
        (false, false, true),
        (true, false, true),
        (false, true, true),
        (false, false, false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let source = directory.path().join("source");
        let output = directory.path().join("derived");
        generate(&data);
        if with_skeleton {
            // Static meshes also record a discovered skeleton's dependency hash.
            fs::copy(
                data.join(layout::GENERATED_MODEL_PATH),
                data.join("meshes/skeleton.nif"),
            )
            .unwrap();
        }
        fs::remove_file(data.join(layout::GENERATED_NORMAL_PATH)).unwrap();
        convert(&data, &source).await;
        // Historical packs kept verified raw VFS files; main's runtime packs do not.
        let legacy_meshes = source.join("vfs/meshes");
        fs::create_dir_all(&legacy_meshes).unwrap();
        fs::copy(
            data.join(layout::GENERATED_MODEL_PATH),
            legacy_meshes.join("generated.nif"),
        )
        .unwrap();
        if with_skeleton {
            fs::copy(
                data.join("meshes/skeleton.nif"),
                legacy_meshes.join("skeleton.nif"),
            )
            .unwrap();
        }
        let mut manifest =
            ConversionManifest::load(&source.join("conversion-manifest.json")).unwrap();
        let key = layout::GENERATED_MODEL_PATH;
        let candidate = directory.path().join("preprune/meshes/generated.glb");
        converter::mesh::MeshConverter::convert_nif_to_glb(data.join(key), candidate.clone())
            .unwrap();
        let entry = manifest.entries.get_mut(key).unwrap();
        entry.output_hash = hash_file(&candidate).unwrap();
        entry.output_size = fs::metadata(&candidate).unwrap().len();
        manifest
            .save(&source.join("conversion-manifest.json"))
            .unwrap();
        if corrupt {
            fs::write(source.join("meshes/generated.glb"), b"tampered").unwrap();
        }
        if !with_vfs {
            fs::remove_dir_all(source.join("vfs")).unwrap();
        }
        let retained = hash_file(&source.join("meshes/generated.glb")).unwrap();
        let result = rebuild(&data, &source, &output).await;
        if !with_vfs {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("legacy pruned GLB replay requires raw source NIF"),
                "{error}"
            );
            assert!(!output.exists());
        } else if corrupt {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("checksum mismatch")
            );
            assert!(!output.exists());
        } else {
            assert!(result.unwrap().complete);
            assert_eq!(
                hash_file(&output.join("meshes/generated.glb")).unwrap(),
                retained
            );
            let updated =
                ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
            assert_eq!(updated.entries[key].output_hash, retained);
            let provenance: serde_json::Value =
                serde_json::from_slice(&fs::read(output.join("metadata-rebuild.json")).unwrap())
                    .unwrap();
            assert_eq!(
                provenance["replay_verified_pruned_assets"],
                serde_json::json!(["meshes/generated.glb"])
            );
        }
        assert_eq!(
            hash_file(&source.join("meshes/generated.glb")).unwrap(),
            retained
        );
    }
}

/// Historical producer 17/18 and the separate 25 candidates are ambiguous for rebuild.
#[tokio::test]
async fn v91_metadata_rebuild_rejects_ambiguous_lod_and_lighting_producers() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    generate(&data);
    convert(&data, &source).await;
    let path = source.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for schema in [17, 18, 25] {
        manifest.schema_version = schema;
        manifest.save(&path).unwrap();
        let output = directory.path().join(format!("derived-{schema}"));
        let error = rebuild(&data, &source, &output)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!(
                "complete converter schema 15, 16, 19 through 24 or {}",
                converter::cache::CONVERTER_SCHEMA_VERSION
            )),
            "{error}"
        );
        assert!(!output.exists());
    }
}
