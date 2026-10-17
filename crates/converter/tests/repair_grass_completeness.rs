//! Asset repair cannot certify grass rules omitted after successful record decoding.
use converter::{
    AssetPipeline, PipelineConfig, PipelineReport, config::RecordReader, repair::repair_failed,
};
use dummy_content::{dds, rng::Rng};
use serde_json::Value;
use std::{fs, path::Path};

/// Encode ordinary subrecord framing without either frontend's decoder.
fn field(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Frame a top-level record independently of the projection implementation.
fn record(tag: &[u8; 4], id: u32, fields: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(fields.len() as u32).to_le_bytes(),
        &[0; 4],
        &id.to_le_bytes(),
        &[0; 8],
        fields,
    ]
    .concat()
}

/// Both rules have complete 32-byte DATA; only the second water enum is unusable.
fn rule(id: u32, water_comparison: u32) -> Vec<u8> {
    let mut data = vec![40, 0, 90, 0, 0, 0, 0, 0];
    data.extend(water_comparison.to_le_bytes());
    for value in [0.0f32, 0.0, 0.0, 1.0] {
        data.extend(value.to_le_bytes());
    }
    data.extend([0; 4]);
    assert_eq!(data.len(), 32);
    record(
        b"GRAS",
        id,
        &[
            field(b"MODL", b"grass/optional.nif\0"),
            field(b"DATA", &data),
        ]
        .concat(),
    )
}

/// Build two terrain neighbors, a usable rule, and an independently broken texture.
fn fixture(root: &Path, reader: RecordReader, grass_loss: bool) -> PipelineConfig {
    let data = root.join("Data");
    fs::create_dir_all(data.join("textures")).unwrap();
    let plugin = dummy_content::esm::plugin(&dummy_content::esm::Plugin {
        author: "Grass repair completeness",
        worldspace: "GrassRepairWorld",
        cells: &[
            dummy_content::esm::Cell {
                grid_x: 3,
                grid_y: -2,
            },
            dummy_content::esm::Cell {
                grid_x: 4,
                grid_y: -2,
            },
        ],
        model_path: "architecture/unavailable.nif",
        diffuse: "textures/unavailable.dds",
        normal_texture: "textures/unavailable_n.dds",
    })
    .unwrap();
    fs::write(data.join("Skyrim.esm"), plugin).unwrap();
    let mut grass_plugin = record(
        b"TES4",
        0,
        &[
            field(
                b"HEDR",
                &[
                    1.7f32.to_le_bytes(),
                    3u32.to_le_bytes(),
                    0x803u32.to_le_bytes(),
                ]
                .concat(),
            ),
            field(b"MAST", b"Skyrim.esm\0"),
            field(b"DATA", &[0; 8]),
        ]
        .concat(),
    );
    grass_plugin.extend(rule(0x0100_0801, 0));
    grass_plugin.extend(rule(0x0100_0802, if grass_loss { 8 } else { 7 }));
    grass_plugin.extend(record(
        b"LTEX",
        4,
        &[
            field(b"GNAM", &0x0100_0801u32.to_le_bytes()),
            field(b"GNAM", &0x0100_0802u32.to_le_bytes()),
        ]
        .concat(),
    ));
    fs::write(data.join("Grass.esp"), grass_plugin).unwrap();
    fs::write(data.join("textures/repair.dds"), b"independent invalid DDS").unwrap();
    let mut config = PipelineConfig::new(data, root.join("pack"));
    config.record_reader = reader;
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    config
}

/// Drain actual conversion progress so this exercises publication rather than a helper.
async fn publish(config: &PipelineConfig) -> PipelineReport {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config.clone(), sender).await;
    drain.await.unwrap();
    result.unwrap()
}

/// Read the persisted contract rather than relying on Rust struct defaults.
fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// Require healthy neighbors and rules to survive actual conversion and asset repair.
fn assert_cache(output: &Path, grass_loss: bool) {
    let bytes = fs::read(output.join("grass_cache.rkyv")).unwrap();
    let cache = rkyv::from_bytes::<shared::grass::GrassCache, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(cache.cells.len(), 2);
    let ids: Vec<_> = cache.types.iter().map(|rule| rule.form_id).collect();
    assert_eq!(
        ids,
        if grass_loss {
            vec![0x0100_0801]
        } else {
            vec![0x0100_0801, 0x0100_0802]
        }
    );
    let mut coordinates = cache
        .cells
        .iter()
        .map(|cell| (cell.grid_x, cell.grid_y))
        .collect::<Vec<_>>();
    coordinates.sort_unstable();
    assert_eq!(coordinates, [(3, -2), (4, -2)]);
}

/// A successful DDS repair must preserve the independently omitted grass-data status.
async fn check_repair(reader: RecordReader, grass_loss: bool) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), reader, grass_loss);
    let report = publish(&config).await;
    assert!(!report.complete);
    assert_eq!(report.skipped, 1, "{report:?}");
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|warning| warning.contains("incomplete or invalid grass rules"))
            .count(),
        usize::from(grass_loss),
        "{report:?}"
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("Skyrim.esm") || warning.contains("Grass.esp")),
        "the fixture must reach grass baking without reader losses: {report:?}"
    );
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(manifest["failures"].as_object().unwrap().len(), 1);
    assert!(manifest["failures"].get("textures/repair.dds").is_some());
    assert_cache(&config.output_dir, grass_loss);
    let cache_before = fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap();
    let plugin_before = fs::read(config.data_dir.join("Skyrim.esm")).unwrap();
    let grass_plugin_before = fs::read(config.data_dir.join("Grass.esp")).unwrap();
    let replacement = dds::generate(
        &dds::Spec::new(dds::Format::Bc1Unorm, 4, 4),
        &mut Rng::new(0x1389_74A5),
    )
    .unwrap();
    fs::write(config.data_dir.join("textures/repair.dds"), replacement).unwrap();
    let repair = repair_failed(&config, true).unwrap();
    assert!(repair.published);
    assert_eq!(repair.converted, 1);
    assert!(repair.failures.is_empty());
    assert!(config.output_dir.join("textures/repair.ktx2").is_file());
    assert_eq!(
        fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap(),
        cache_before
    );
    assert_eq!(
        fs::read(config.data_dir.join("Skyrim.esm")).unwrap(),
        plugin_before
    );
    assert_eq!(
        fs::read(config.data_dir.join("Grass.esp")).unwrap(),
        grass_plugin_before
    );
    assert_cache(&config.output_dir, grass_loss);
    let manifest = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(
        manifest["complete"], !grass_loss,
        "successful asset repair must not certify omitted grass data"
    );
    assert_eq!(manifest["record_data_complete"], !grass_loss);
    let check =
        converter::check_output(&config.output_dir, converter::CheckMode::Full, |_, _| {}).unwrap();
    assert_eq!(
        check.problems,
        if grass_loss {
            vec![converter::CheckProblem::Incomplete]
        } else {
            vec![]
        }
    );
}

/// Default frontend must preserve a grass-only loss after successful asset repair.
#[tokio::test]
async fn inhouse_grass_loss_survives_asset_repair() {
    check_repair(RecordReader::Inhouse, true).await;
}

/// Explicit legacy must preserve the same derived-data loss.
#[tokio::test]
async fn legacy_grass_loss_survives_asset_repair() {
    check_repair(RecordReader::Legacy, true).await;
}

/// A valid enum boundary supplies the healthy control without suppressing asset repair.
#[tokio::test]
async fn healthy_grass_asset_only_repair_becomes_complete() {
    check_repair(RecordReader::Inhouse, false).await;
}

/// A pre-grass producer's complete retained assets do not certify newly omitted grass data.
#[tokio::test]
async fn historical_metadata_rebuild_retains_grass_loss_completeness() {
    let directory = tempfile::tempdir().unwrap();
    let source_config = fixture(directory.path(), RecordReader::Legacy, true);
    let texture = dds::generate(
        &dds::Spec::new(dds::Format::Bc1Unorm, 4, 4),
        &mut Rng::new(0x1389_74A5),
    )
    .unwrap();
    fs::write(source_config.data_dir.join("textures/repair.dds"), texture).unwrap();
    let initial = publish(&source_config).await;
    assert_eq!(initial.skipped, 0, "{initial:?}");
    assert!(!initial.complete);
    assert_eq!(initial.warnings.len(), 1, "{initial:?}");
    assert!(initial.warnings[0].contains("incomplete or invalid grass rules"));
    assert_cache(&source_config.output_dir, true);

    // Reconstruct the explicit historical producer contract: schema 24 predates
    // this grass artifact and could certify these healthy assets without baking
    // the semantically invalid grass rule. The raw plugin bytes stay unchanged.
    let manifest_path = source_config.output_dir.join("conversion-manifest.json");
    let mut historical = converter::cache::ConversionManifest::load(&manifest_path).unwrap();
    assert!(historical.failures.is_empty());
    historical.schema_version = 24;
    historical.configuration_hash =
        converter::cache::configuration_hash_for_schema(&source_config, 24).unwrap();
    historical.asset_configuration_hashes.clear();
    historical.complete = true;
    historical.record_data_complete = None;
    historical.save(&manifest_path).unwrap();
    fs::remove_file(source_config.output_dir.join("grass_cache.rkyv")).unwrap();
    fs::write(
        source_config.output_dir.join("record-reader.json"),
        br#"{"mode":"legacy"}"#,
    )
    .unwrap();

    let source_manifest_before = fs::read(&manifest_path).unwrap();
    let source_database_before =
        converter::cache::hash_file(&source_config.output_dir.join("skyrim_world.db")).unwrap();
    let texture_before = fs::read(source_config.output_dir.join("textures/repair.ktx2")).unwrap();
    let plugin_before = fs::read(source_config.data_dir.join("Skyrim.esm")).unwrap();
    let grass_plugin_before = fs::read(source_config.data_dir.join("Grass.esp")).unwrap();
    let mut config = PipelineConfig::new(
        source_config.data_dir.clone(),
        directory.path().join("metadata-derived"),
    );
    assert_eq!(config.record_reader, RecordReader::Inhouse);
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result =
        AssetPipeline::rebuild_metadata_async(config.clone(), &source_config.output_dir, sender)
            .await;
    drain.await.unwrap();
    let report = result.unwrap();
    assert!(!report.complete, "{report:?}");
    assert_eq!(report.skipped, 0, "{report:?}");
    assert_eq!(report.warnings.len(), 1, "{report:?}");
    assert!(report.warnings[0].contains("incomplete or invalid grass rules"));
    assert_eq!(report.converted, 0);
    assert_cache(&config.output_dir, true);
    let derived = json(&config.output_dir.join("conversion-manifest.json"));
    assert_eq!(derived["complete"], false);
    assert_eq!(
        derived["record_data_complete"], false,
        "metadata grass losses must clear proof independently of healthy reader decoding"
    );
    assert_eq!(
        fs::read(config.output_dir.join("textures/repair.ktx2")).unwrap(),
        texture_before
    );
    assert_eq!(fs::read(&manifest_path).unwrap(), source_manifest_before);
    assert_eq!(
        converter::cache::hash_file(&source_config.output_dir.join("skyrim_world.db")).unwrap(),
        source_database_before
    );
    assert_eq!(
        fs::read(source_config.output_dir.join("textures/repair.ktx2")).unwrap(),
        texture_before
    );
    assert_eq!(
        fs::read(config.data_dir.join("Skyrim.esm")).unwrap(),
        plugin_before
    );
    assert_eq!(
        fs::read(config.data_dir.join("Grass.esp")).unwrap(),
        grass_plugin_before
    );
    assert!(!source_config.output_dir.join("grass_cache.rkyv").exists());
}
