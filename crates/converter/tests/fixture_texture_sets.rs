//! Synthetic physical TXST slots exercised through the real exporter and pipeline.

use converter::{
    AssetPipeline, PipelineConfig, PipelineReport,
    cache::{
        CONVERTER_SCHEMA_VERSION, ConversionManifest, StagedOutput, StagingJournal,
        configuration_hash, configuration_hash_for_schema, hash_bytes, hash_file,
    },
    esm::EsmParser,
    texture::{TextureConverter, TextureEncoding, inspect_runtime_ktx2},
};
use dummy_content::{dds, esm, layout, rng::Rng};
use std::{fs, path::Path};

const TEXTURE_SET_ID: u32 = 0x0000_0809;
const SLOT_PATHS: [&str; 8] = [
    "textures/txst_slot_00.dds",
    "textures/txst_slot_01.dds",
    "textures/txst_slot_02.dds",
    "textures/txst_slot_03.dds",
    "textures/txst_slot_04.dds",
    "textures/txst_slot_05.dds",
    "textures/txst_slot_06.dds",
    "textures/txst_slot_07.dds",
];

/// Appends a distinct TXST record whose FormID cannot alias the generated one.
fn append_texture_set(plugin: &Path, slots: [Option<&str>; 8]) {
    let mut bytes = fs::read(plugin).unwrap();
    bytes.extend(esm::texture_set(TEXTURE_SET_ID, slots).unwrap());
    fs::write(plugin, bytes).unwrap();
}

/// Every TX00-TX07 path must reach its own column, including unchanged slots.
#[test]
fn texture_set_physical_slots_reach_the_correct_database_columns() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = directory.path().join("Skyrim.esm");
    fs::write(
        &plugin,
        esm::plugin(&esm::Plugin {
            author: "Mudcrab texture-set regression",
            worldspace: "GeneratedWorld",
            cells: &[esm::Cell {
                grid_x: 0,
                grid_y: 0,
            }],
            model_path: "meshes/generated.nif",
            diffuse: "textures/generated_color.dds",
            normal_texture: "textures/generated_normal.dds",
        })
        .unwrap(),
    )
    .unwrap();
    append_texture_set(&plugin, SLOT_PATHS.map(Some));
    let database = directory.path().join("skyrim_world.db");
    EsmParser::convert_plugins(&[plugin], &database).unwrap();
    let connection = rusqlite::Connection::open(database).unwrap();
    for (column, physical_slot) in [
        ("diffuse_path", 0),
        ("normal_path", 1),
        ("mask_path", 2),
        ("glow_path", 3),
        ("height_path", 4),
        ("environment_path", 5),
        ("specular_path", 6),
        ("detail_path", 7),
    ] {
        let stored: String = connection
            .query_row(
                &format!("SELECT {column} FROM texture_sets WHERE id=?1"),
                [TEXTURE_SET_ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, SLOT_PATHS[physical_slot], "{column}");
    }
}

/// Runs the public pipeline with a progress drain and verifies publication completed.
async fn run_pipeline(config: PipelineConfig) -> PipelineReport {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{report:?}");
    report
}

/// Converts textures with neutral filenames reached only through TX02/TX05.
async fn convert_texture_set_semantics(data: &Path, output: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(data, layout::DEFAULT_SEED, layout::Formats::all()).unwrap();
    let mut slots = [None; 8];
    slots[2] = Some(SLOT_PATHS[2]);
    slots[5] = Some(SLOT_PATHS[5]);
    append_texture_set(&data.join("Skyrim.esm"), slots);
    let mut rng = Rng::new(194);
    for (slot, spec) in [
        (2, dds::Spec::new(dds::Format::Bc1Unorm, 4, 4)),
        (5, dds::Spec::new(dds::Format::Bc1Unorm, 4, 4).as_cubemap()),
    ] {
        fs::write(
            data.join(SLOT_PATHS[slot]),
            dds::generate(&spec, &mut rng).unwrap(),
        )
        .unwrap();
    }
    let mut config = PipelineConfig::new(data, output);
    config.no_lod = true;
    run_pipeline(config).await;
}

/// An environment cubemap has sRGB color despite having no NIF dependency.
#[tokio::test]
async fn texture_set_only_environment_cubemap_uses_color_encoding() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("modern");
    convert_texture_set_semantics(&directory.path().join("Data"), &output).await;
    let bytes = fs::read(output.join("textures/txst_slot_05.ktx2")).unwrap();
    let metadata = inspect_runtime_ktx2(&bytes).unwrap();
    assert_eq!(metadata.faces, 6);
    assert_eq!(metadata.encoding, TextureEncoding::ColorSrgb);
}

/// An environment mask keeps linear channels despite having no NIF dependency.
#[tokio::test]
async fn texture_set_only_environment_mask_uses_linear_encoding() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("modern");
    convert_texture_set_semantics(&directory.path().join("Data"), &output).await;
    let bytes = fs::read(output.join("textures/txst_slot_02.ktx2")).unwrap();
    let metadata = inspect_runtime_ktx2(&bytes).unwrap();
    assert_eq!(metadata.faces, 1);
    assert_eq!(metadata.encoding, TextureEncoding::DataLinear);
}

/// Reads the published provenance without applying normal cache migration filters.
fn manifest(output: &Path) -> ConversionManifest {
    serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap()).unwrap()
}

/// Reproduces the old physical-slot projection, encoding bytes and honest cache proofs.
fn legacy_pack(data: &Path, output: &Path, schema: u32) -> ConversionManifest {
    let config = PipelineConfig::new(data, output);
    let mut legacy = manifest(output);
    legacy.schema_version = schema;
    legacy.configuration_hash = configuration_hash_for_schema(&config, schema).unwrap();
    for (slot, old_encoding) in [
        (2, TextureEncoding::ColorSrgb),
        (5, TextureEncoding::DataLinear),
    ] {
        let source = data.join(SLOT_PATHS[slot]);
        let bytes = TextureConverter::convert(&fs::read(&source).unwrap(), old_encoding).unwrap();
        let entry = legacy.entries.get_mut(SLOT_PATHS[slot]).unwrap();
        fs::write(output.join(&entry.output), &bytes).unwrap();
        entry.source_hash = format!(
            "{}:texture-encoding:{old_encoding:?}",
            hash_file(&source).unwrap()
        );
        entry.output_size = bytes.len() as u64;
        entry.output_hash = hash_bytes(&bytes);
        assert_eq!(inspect_runtime_ktx2(&bytes).unwrap().encoding, old_encoding);
    }
    let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    connection
        .execute(
            "UPDATE texture_sets SET glow_path=?1, height_path=NULL, environment_path=NULL, mask_path=?2 WHERE id=?3",
            rusqlite::params![SLOT_PATHS[2], SLOT_PATHS[5], TEXTURE_SET_ID],
        )
        .unwrap();
    connection
        .execute("UPDATE schema_info SET version=7", [])
        .unwrap();
    drop(connection);
    let integration_path = output.join("integration-report.json");
    let mut integration: serde_json::Value =
        serde_json::from_slice(&fs::read(&integration_path).unwrap()).unwrap();
    integration["schema_version"] = 7.into();
    fs::write(integration_path, serde_json::to_vec(&integration).unwrap()).unwrap();
    legacy
        .save(&output.join("conversion-manifest.json"))
        .unwrap();
    legacy
}

/// Producer 24 rebuilds every texture and world output, retaining verified GLBs/Luau.
/// Producer 23 remains a negative control for the earlier changed mesh contract.
#[tokio::test]
async fn texture_semantic_migration_preserves_only_known_compatible_published_meshes() {
    for schema in [24, 23] {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        let output = directory.path().join("modern");
        convert_texture_set_semantics(&data, &output).await;
        let expected_cube = fs::read(output.join("textures/txst_slot_05.ktx2")).unwrap();
        let expected_mask = fs::read(output.join("textures/txst_slot_02.ktx2")).unwrap();
        let expected_mesh = fs::read(output.join("meshes/generated.glb")).unwrap();
        let legacy = legacy_pack(&data, &output, schema);
        let textures = legacy
            .entries
            .values()
            .filter(|entry| entry.output.ends_with(".ktx2"))
            .count();
        let meshes = legacy
            .entries
            .values()
            .filter(|entry| entry.output.ends_with(".glb"))
            .count();
        let mut config = PipelineConfig::new(&data, &output);
        config.no_lod = true;
        let report = run_pipeline(config.clone()).await;
        assert_eq!(
            report.converted,
            (textures + if schema == 24 { 0 } else { meshes }) as u64
        );
        assert_eq!(
            fs::read(output.join("textures/txst_slot_05.ktx2")).unwrap(),
            expected_cube
        );
        assert_eq!(
            fs::read(output.join("textures/txst_slot_02.ktx2")).unwrap(),
            expected_mask
        );
        assert_eq!(
            fs::read(output.join("meshes/generated.glb")).unwrap(),
            expected_mesh
        );
        let current = manifest(&output);
        assert_eq!(current.schema_version, CONVERTER_SCHEMA_VERSION);
        assert_eq!(current.retained_mesh_schema_version, None);
        assert_eq!(run_pipeline(config).await.converted, 0);
        let connection = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT version FROM schema_info", [], |row| row
                    .get::<_, u32>(0))
                .unwrap(),
            shared::WORLD_DATABASE_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT environment_path FROM texture_sets WHERE id=?1",
                    [TEXTURE_SET_ID],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            SLOT_PATHS[5]
        );
    }
}

/// A matching old producer cannot erase a real change to conversion settings.
#[tokio::test]
async fn texture_semantic_migration_still_requires_matching_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let output = directory.path().join("modern");
    convert_texture_set_semantics(&data, &output).await;
    let legacy = legacy_pack(&data, &output, 24);
    let mut changed = PipelineConfig::new(&data, &output);
    changed.no_lod = true;
    changed.texture_zstd_level = 0;
    assert_ne!(
        legacy.configuration_hash,
        configuration_hash_for_schema(&changed, 24).unwrap()
    );
    let expected_configuration = configuration_hash(&changed).unwrap();
    let raw_entries = legacy
        .archives
        .values()
        .map(|archive| archive.files.len() as u64)
        .sum::<u64>();
    let report = run_pipeline(changed).await;
    assert_eq!(report.cache_hits, raw_entries);
    assert_eq!(report.converted, legacy.entries.len() as u64);
    let current = manifest(&output);
    assert_eq!(current.configuration_hash, expected_configuration);
    assert_eq!(
        current.entries.keys().collect::<Vec<_>>(),
        legacy.entries.keys().collect::<Vec<_>>()
    );
    for entry in current.entries.values() {
        if entry.output.ends_with(".ktx2") {
            let bytes = fs::read(output.join(&entry.output)).unwrap();
            assert_eq!(
                inspect_runtime_ktx2(&bytes).unwrap().supercompression,
                "None"
            );
        }
    }
    for (slot, encoding) in [
        (2, TextureEncoding::DataLinear),
        (5, TextureEncoding::ColorSrgb),
    ] {
        let bytes = fs::read(output.join(&current.entries[SLOT_PATHS[slot]].output)).unwrap();
        assert_eq!(inspect_runtime_ktx2(&bytes).unwrap().encoding, encoding);
    }
}

/// Metadata retains honest producer-24 texture provenance until normal conversion.
#[tokio::test]
async fn texture_semantic_migration_preserves_original_metadata_asset_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let source = directory.path().join("source");
    let output = directory.path().join("derived");
    convert_texture_set_semantics(&data, &source).await;
    let expected_cube = fs::read(source.join("textures/txst_slot_05.ktx2")).unwrap();
    let expected_mask = fs::read(source.join("textures/txst_slot_02.ktx2")).unwrap();
    let legacy = legacy_pack(&data, &source, 24);
    let mut config = PipelineConfig::new(&data, &output);
    config.no_lod = true;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::rebuild_metadata_async(config.clone(), &source, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete);
    assert_eq!(report.converted, 0);
    let retained = manifest(&output);
    assert_eq!(retained.schema_version, CONVERTER_SCHEMA_VERSION);
    assert_eq!(retained.retained_mesh_schema_version, Some(24));
    assert_eq!(
        retained.retained_asset_configuration_hash,
        Some(legacy.configuration_hash)
    );
    for path in [
        "textures/txst_slot_05.ktx2",
        "textures/txst_slot_02.ktx2",
        "meshes/generated.glb",
    ] {
        assert_eq!(
            fs::read(output.join(path)).unwrap(),
            fs::read(source.join(path)).unwrap()
        );
    }
    let eligible = ConversionManifest::load(&output.join("conversion-manifest.json")).unwrap();
    assert!(
        eligible
            .entries
            .values()
            .any(|entry| entry.output.ends_with(".glb"))
    );
    assert!(
        eligible
            .entries
            .values()
            .all(|entry| !entry.output.ends_with(".ktx2"))
    );
    let normal = run_pipeline(config).await;
    let current = manifest(&output);
    let textures = retained
        .entries
        .values()
        .filter(|entry| entry.output.ends_with(".ktx2"))
        .count();
    let ingested = current
        .archives
        .values()
        .map(|archive| archive.files.len())
        .sum::<usize>();
    assert_eq!(normal.converted, (textures + ingested) as u64);
    assert_eq!(current.retained_mesh_schema_version, None);
    assert_eq!(
        fs::read(output.join("textures/txst_slot_05.ktx2")).unwrap(),
        expected_cube
    );
    assert_eq!(
        fs::read(output.join("textures/txst_slot_02.ktx2")).unwrap(),
        expected_mask
    );
}

/// Published-mesh compatibility does not admit an old producer's staging journal.
#[tokio::test]
async fn texture_semantic_migration_rejects_old_staged_mesh_and_texture_proof() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    let baseline = directory.path().join("baseline");
    convert_texture_set_semantics(&data, &baseline).await;
    let original = manifest(&baseline);
    let output = directory.path().join("resumed");
    let staging = output.with_extension(format!("staging-{}-1", std::process::id()));
    let mut config = PipelineConfig::new(&data, &output);
    config.no_lod = true;
    config.resume_staging = Some(staging.clone());
    fs::create_dir_all(&staging).unwrap();
    let mut journal = StagingJournal::open(&staging).unwrap();
    for key in ["meshes/generated.nif", "textures/txst_slot_05.dds"] {
        let entry = &original.entries[key];
        let expected = fs::read(baseline.join(&entry.output)).unwrap();
        let mut stale = expected.clone();
        if entry.output.ends_with(".glb") {
            let position = stale
                .windows(b"GeneratedQuad".len())
                .position(|bytes| bytes == b"GeneratedQuad")
                .unwrap();
            stale[position..position + b"GeneratedQuad".len()].copy_from_slice(b"RetainedProof");
        } else {
            stale = TextureConverter::convert(
                &fs::read(data.join(key)).unwrap(),
                TextureEncoding::DataLinear,
            )
            .unwrap();
        }
        assert_ne!(stale, expected);
        let path = staging.join(&entry.output);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &stale).unwrap();
        journal
            .record(
                key,
                &StagedOutput {
                    schema_version: 24,
                    configuration_hash: configuration_hash(&config).unwrap(),
                    source_hash: entry.source_hash.clone(),
                    output_size: stale.len() as u64,
                    output_hash: hash_bytes(&stale),
                },
            )
            .unwrap();
    }
    drop(journal);
    let report = run_pipeline(config).await;
    assert_eq!(report.cache_hits, 0);
    for key in ["meshes/generated.nif", "textures/txst_slot_05.dds"] {
        let entry = &original.entries[key];
        assert_eq!(
            fs::read(output.join(&entry.output)).unwrap(),
            fs::read(baseline.join(&entry.output)).unwrap()
        );
    }
}
