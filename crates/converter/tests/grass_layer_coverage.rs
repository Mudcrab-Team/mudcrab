//! Grass selection uses only the first five painted LAND layers.
use converter::{AssetPipeline, PipelineConfig, PipelineReport, config::RecordReader};
use shared::grass::GrassCache;
use std::{fs, path::Path};

const GRASS_TEXTURE: u32 = 0x0100_0810;
const BARE_TEXTURE: u32 = 0x0100_0811;
const GRASS: u32 = 0x0100_0801;
const SAMPLE: usize = 117;

/// Frame source fields independently of the converter's terrain decoder.
fn field(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Frame an uncompressed plugin record with its file-relative identity.
fn record(tag: &[u8; 4], id: u32, fields: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(fields.len() as u32).to_le_bytes(),
        &[0; 4],
        &id.to_le_bytes(),
        &[0; 8],
        &fields,
    ]
    .concat()
}

/// Preserve worldspace and CELL context for each overriding LAND record.
fn group(kind: i32, label: u32, bytes: &[u8]) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &(bytes.len() as u32 + 24).to_le_bytes(),
        &label.to_le_bytes(),
        &kind.to_le_bytes(),
        &[0; 8],
        bytes,
    ]
    .concat()
}

/// Author one base and optionally six overlays, with the sixth strictly weakest.
fn land(cell: u32, base_grass: bool, overlays: bool) -> Vec<u8> {
    let base = if base_grass {
        GRASS_TEXTURE
    } else {
        BARE_TEXTURE
    };
    let mut fields = field(b"BTXT", &[base.to_le_bytes(), [0; 4]].concat());
    if overlays {
        for slot in 0u16..6 {
            let texture = if !base_grass && slot == 5 {
                GRASS_TEXTURE
            } else {
                BARE_TEXTURE
            };
            fields.extend(field(
                b"ATXT",
                &[
                    texture.to_le_bytes().as_slice(),
                    &[0; 2],
                    &slot.to_le_bytes(),
                ]
                .concat(),
            ));
            let opacity: f32 = if slot == 5 { 0.10 } else { 0.15 };
            fields.extend(field(
                b"VTXT",
                &[
                    (SAMPLE as u16).to_le_bytes().as_slice(),
                    &[0; 2],
                    &opacity.to_le_bytes(),
                ]
                .concat(),
            ));
        }
    }
    if cell == 0x30 {
        fields.extend(field(b"ATXT", &[0; 8]));
        fields.extend(field(
            b"VTXT",
            &[
                (SAMPLE as u16).to_le_bytes().as_slice(),
                &[0; 2],
                &0.75f32.to_le_bytes(),
            ]
            .concat(),
        ));
    }
    group(8, cell, &record(b"LAND", cell + 1, fields))
}

/// Build two over-capacity quadrants and an unaffected grass-bearing neighbor.
fn fixture(root: &Path, reader: RecordReader) -> PipelineConfig {
    let data = root.join("Data");
    fs::create_dir_all(&data).unwrap();
    fs::write(
        data.join("Skyrim.esm"),
        dummy_content::esm::plugin(&dummy_content::esm::Plugin {
            author: "Grass layer capacity regression",
            worldspace: "GrassLayerWorld",
            cells: &[
                dummy_content::esm::Cell {
                    grid_x: 3,
                    grid_y: -2,
                },
                dummy_content::esm::Cell {
                    grid_x: 4,
                    grid_y: -2,
                },
                dummy_content::esm::Cell {
                    grid_x: 5,
                    grid_y: -2,
                },
            ],
            model_path: "architecture/unavailable.nif",
            diffuse: "textures/unavailable.dds",
            normal_texture: "textures/unavailable_n.dds",
        })
        .unwrap(),
    )
    .unwrap();
    let mut plugin = record(
        b"TES4",
        0,
        [field(b"MAST", b"Skyrim.esm\0"), field(b"DATA", &[0; 8])].concat(),
    );
    let mut rule = vec![40, 0, 90, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    for value in [0.0f32, 0.0, 0.0, 1.0] {
        rule.extend(value.to_le_bytes());
    }
    rule.extend([0; 4]);
    plugin.extend(record(
        b"GRAS",
        GRASS,
        [
            field(b"MODL", b"grass/optional.nif\0"),
            field(b"DATA", &rule),
        ]
        .concat(),
    ));
    plugin.extend(record(
        b"LTEX",
        GRASS_TEXTURE,
        [
            field(b"TNAM", &2u32.to_le_bytes()),
            field(b"GNAM", &GRASS.to_le_bytes()),
        ]
        .concat(),
    ));
    plugin.extend(record(
        b"LTEX",
        BARE_TEXTURE,
        [
            field(b"EDID", b"BareTexture\0"),
            field(b"TNAM", &2u32.to_le_bytes()),
        ]
        .concat(),
    ));
    plugin.extend(group(
        1,
        1,
        &[
            land(0x10, false, true),
            land(0x20, true, true),
            land(0x30, true, false),
        ]
        .concat(),
    ));
    fs::write(data.join("Layers.esp"), plugin).unwrap();
    let mut config = PipelineConfig::new(data, root.join("pack"));
    config.record_reader = reader;
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    config
}

/// Publish through the public conversion entry point while draining progress.
async fn publish(config: &PipelineConfig) -> PipelineReport {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), sender)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert!(
        report
            .artifacts
            .contains(&"cell_cache_preserved.rkyv".into())
    );
    report
}

/// Compare decoded grass coverage against the independently authored source weights.
fn assert_coverage(output: &Path) {
    let bytes = fs::read(output.join("grass_cache.rkyv")).unwrap();
    let cache = rkyv::from_bytes::<GrassCache, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(cache.version, shared::grass::GRASS_CACHE_VERSION);
    let layer = |cell_id| {
        let cell = cache
            .cells
            .iter()
            .find(|cell| cell.cell_id == cell_id)
            .expect("a grass-bearing base must retain its cell");
        let layers = &cell.quadrants[0].layers;
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].grass_ids, [GRASS]);
        &layers[0]
    };
    assert!(cache.cells.iter().all(|cell| cell.cell_id != 0x10));
    assert!(
        (layer(0x20).weights[SAMPLE] - 65.0 / 255.0).abs() < 1e-6,
        "only the first five bare overlays attenuate the grass-bearing base"
    );
    assert_eq!(layer(0x20).weights[0], 1.0);
    assert_eq!(
        layer(0x30).weights[SAMPLE],
        64.0 / 255.0,
        "NULL paint remains selection coverage even though terrain omits it"
    );
    assert_eq!(
        cache.cells.len(),
        2,
        "the healthy neighbor must remain present"
    );
    let runtime = fs::read(output.join("cell_cache.rkyv")).unwrap();
    let runtime = rkyv::from_bytes::<shared::CellCache, rkyv::rancor::Error>(&runtime).unwrap();
    for cell in runtime
        .cells
        .iter()
        .filter(|cell| [0x10, 0x20].contains(&cell.cell_id))
    {
        assert_eq!(
            cell.layers
                .iter()
                .filter(|layer| layer.quadrant == 0)
                .count(),
            6,
            "the runtime terrain material must retain its existing capacity"
        );
    }
    let preserved = fs::read(output.join("cell_cache_preserved.rkyv")).unwrap();
    let preserved = rkyv::from_bytes::<shared::CellCache, rkyv::rancor::Error>(&preserved).unwrap();
    for cell in preserved
        .cells
        .iter()
        .filter(|cell| [0x10, 0x20].contains(&cell.cell_id))
    {
        assert_eq!(
            cell.layers
                .iter()
                .filter(|layer| layer.quadrant == 0)
                .count(),
            7,
            "the preserved terrain source retains all seven layers"
        );
    }
}

/// Full conversion and an unchanged rerun must preserve the selected grass coverage.
async fn check_conversion(reader: RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), reader);
    publish(&config).await;
    assert_coverage(&config.output_dir);
    let before = fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap();
    publish(&config).await;
    assert_coverage(&config.output_dir);
    assert_eq!(
        fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap(),
        before
    );
}

/// Rebuilt metadata must restore full coverage and then reuse both selected cells.
async fn check_metadata(reader: RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), reader);
    publish(&source).await;
    let mut config = source.clone();
    config.output_dir = directory.path().join("metadata");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::rebuild_metadata_async(config.clone(), &source.output_dir, sender)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_coverage(&config.output_dir);
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(config.output_dir.join("metadata-rebuild.json")).unwrap())
            .unwrap();
    assert_eq!(provenance["grass_reused_cells"], 2);
    assert_eq!(provenance["grass_baked_cells"], 0);
    assert_eq!(
        fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap(),
        fs::read(source.output_dir.join("grass_cache.rkyv")).unwrap()
    );
}

/// The default reader must retain source layers but exclude the sixth painted layer from grass.
#[tokio::test]
async fn inhouse_conversion_uses_first_five_painted_layers() {
    check_conversion(RecordReader::Inhouse).await;
}

/// The legacy reader must supply the same complete grass coverage.
#[tokio::test]
async fn legacy_conversion_uses_first_five_painted_layers() {
    check_conversion(RecordReader::Legacy).await;
}

/// In-house metadata rebuilding must preserve and reuse complete grass coverage.
#[tokio::test]
async fn inhouse_metadata_uses_first_five_painted_layers() {
    check_metadata(RecordReader::Inhouse).await;
}

/// Legacy metadata rebuilding must supply complete layers without an in-house sidecar.
#[tokio::test]
async fn legacy_metadata_uses_first_five_painted_layers() {
    check_metadata(RecordReader::Legacy).await;
}

/// Old capped coverage is rebuildable metadata, independent of retained asset authentication.
async fn check_old_version_migration(reader: RecordReader, version: u32) {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), reader);
    publish(&source).await;
    let cache_path = source.output_dir.join("grass_cache.rkyv");
    let mut old =
        rkyv::from_bytes::<GrassCache, rkyv::rancor::Error>(&fs::read(&cache_path).unwrap())
            .unwrap();
    old.version = version;
    // Use wrong historical coverage so rebuilding must replace these bytes.
    old.cells.retain(|cell| cell.cell_id != 0x10);
    old.cells
        .iter_mut()
        .find(|cell| cell.cell_id == 0x20)
        .unwrap()
        .quadrants[0]
        .layers[0]
        .weights[SAMPLE] = 0.91;
    let old_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&old)
        .unwrap()
        .to_vec();
    fs::write(&cache_path, &old_bytes).unwrap();
    if reader == RecordReader::Legacy {
        // Published legacy packs did not have a full terrain sidecar before this fix.
        fs::remove_file(source.output_dir.join("cell_cache_preserved.rkyv")).unwrap();
    }
    let manifest_path = source.output_dir.join("conversion-manifest.json");
    let manifest_before = fs::read(&manifest_path).unwrap();
    let mut config = source.clone();
    config.output_dir = directory.path().join("migrated");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::rebuild_metadata_async(config.clone(), &source.output_dir, sender)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_coverage(&config.output_dir);
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(config.output_dir.join("metadata-rebuild.json")).unwrap())
            .unwrap();
    assert_eq!(provenance["grass_reused_cells"], 0);
    assert_eq!(provenance["grass_baked_cells"], 2);
    assert_eq!(fs::read(&cache_path).unwrap(), old_bytes);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
}

/// In-house packages with version-one grass rebuild without trusting their old masks.
#[tokio::test]
async fn inhouse_metadata_rebuilds_version_one_grass_without_mutating_source() {
    check_old_version_migration(RecordReader::Inhouse, 1).await;
}

/// Legacy version-one packages can migrate without a preexisting full terrain sidecar.
#[tokio::test]
async fn legacy_metadata_rebuilds_version_one_grass_without_mutating_source() {
    check_old_version_migration(RecordReader::Legacy, 1).await;
}

/// Version-eight warm metadata must rebuild changed selection data and preserve source bytes.
#[tokio::test]
async fn inhouse_metadata_rebuilds_version_eight_grass() {
    check_old_version_migration(RecordReader::Inhouse, 8).await;
}

/// The legacy reader also rebuilds version-eight warm metadata.
#[tokio::test]
async fn legacy_metadata_rebuilds_version_eight_grass() {
    check_old_version_migration(RecordReader::Legacy, 8).await;
}

/// A broken modeled grass rule must not prevent publication of healthy neighboring cells.
async fn check_broken_rule_publication(reader: RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), reader);
    let plugin_path = directory.path().join("Data/Layers.esp");
    let mut bytes = fs::read(&plugin_path).unwrap();
    bytes.extend(record(
        b"GRAS",
        0x0100_0802,
        field(b"MODL", b"grass/broken.nif\0"),
    ));
    let mut negative = vec![0u8; 32];
    negative[0] = 50;
    negative[2] = 90;
    negative[12..16].copy_from_slice(&(-20.0f32).to_le_bytes());
    bytes.extend(record(
        b"GRAS",
        0x0100_0803,
        [
            field(b"MODL", b"grass/negative.nif\0"),
            field(b"DATA", &negative),
        ]
        .concat(),
    ));
    fs::write(plugin_path, bytes).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), sender)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("grass") && warning.contains("invalid")),
        "{:?}",
        report.warnings
    );
    assert_coverage(&config.output_dir);
}

/// In-house broken-mod publication retains both healthy grass-bearing neighbors.
#[tokio::test]
async fn inhouse_broken_rule_keeps_neighbor_cells() {
    check_broken_rule_publication(RecordReader::Inhouse).await;
}

/// Legacy broken-mod publication retains both healthy grass-bearing neighbors.
#[tokio::test]
async fn legacy_broken_rule_keeps_neighbor_cells() {
    check_broken_rule_publication(RecordReader::Legacy).await;
}

/// A malformed NULL-paint field must not abort publication or remove the healthy neighboring cell.
async fn check_broken_null_paint(reader: RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), reader);
    let path = config.data_dir.join("Layers.esp");
    let mut bytes = fs::read(&path).unwrap();
    let fields = [
        field(b"BTXT", &[GRASS_TEXTURE.to_le_bytes(), [0; 4]].concat()),
        field(b"ATXT", &[0; 8]),
        field(
            b"VTXT",
            &[
                (SAMPLE as u16).to_le_bytes().as_slice(),
                &[0; 2],
                &f32::NAN.to_le_bytes(),
            ]
            .concat(),
        ),
    ]
    .concat();
    bytes.extend(group(1, 1, &group(8, 0x30, &record(b"LAND", 0x31, fields))));
    fs::write(path, bytes).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), sender)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(
        !report.warnings.is_empty(),
        "malformed field must be diagnosed"
    );
    let cache = rkyv::from_bytes::<GrassCache, rkyv::rancor::Error>(
        &fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap(),
    )
    .unwrap();
    assert!(
        cache.cells.iter().any(|cell| cell.cell_id == 0x20),
        "healthy neighbor must publish"
    );
    assert!(
        cache
            .cells
            .iter()
            .flat_map(|cell| &cell.quadrants)
            .flat_map(|quadrant| &quadrant.layers)
            .flat_map(|layer| &layer.weights)
            .all(|weight| weight.is_finite())
    );
    assert!(config.output_dir.join("conversion-manifest.json").exists());
}

/// In-house malformed LAND publication keeps valid neighboring grass.
#[tokio::test]
async fn inhouse_broken_null_paint_keeps_neighbor() {
    check_broken_null_paint(RecordReader::Inhouse).await;
}

/// Legacy malformed LAND publication keeps valid neighboring grass.
#[tokio::test]
async fn legacy_broken_null_paint_keeps_neighbor() {
    check_broken_null_paint(RecordReader::Legacy).await;
}
