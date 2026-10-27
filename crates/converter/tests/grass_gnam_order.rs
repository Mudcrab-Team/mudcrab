//! Published grass layers retain the winning LTEX GNAM sequence through both readers.
use converter::{AssetPipeline, PipelineConfig, config::RecordReader};
use shared::grass::GrassCache;
use std::{fs, path::Path};

const LOW: u32 = 0x0100_0801;
const MIDDLE: u32 = 0x0100_0802;
const HIGH: u32 = 0x0100_0803;
const TEXTURE: u32 = 0x0100_0810;
const NEIGHBOR_TEXTURE: u32 = 0x0100_0811;
const BASE_ORDER: [u32; 3] = [HIGH, LOW, MIDDLE];
const OVERRIDE_ORDER: [u32; 3] = [MIDDLE, HIGH, LOW];

/// Frame a source subrecord without relying on the converter's field decoder.
fn field(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Frame an uncompressed source record with its file-relative FormID.
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

/// Supply the worldspace and cell context used by the existing layer-coverage fixture.
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

/// Author a grass-bearing base texture in quadrant zero of an existing exterior cell.
fn land(cell: u32, texture: u32) -> Vec<u8> {
    group(
        8,
        cell,
        &record(
            b"LAND",
            cell + 1,
            field(b"BTXT", &[texture.to_le_bytes(), [0; 4]].concat()),
        ),
    )
}

/// Author an LTEX list, optionally surrounding a broken occurrence with valid GNAMs.
fn landscape(texture: u32, order: &[u32], malformed: bool) -> Vec<u8> {
    let mut fields = Vec::new();
    for (index, grass) in order.iter().enumerate() {
        fields.extend(field(b"GNAM", &grass.to_le_bytes()));
        if malformed && index == 0 {
            fields.extend(field(b"GNAM", &[0x81, 0x08, 0x01]));
        }
    }
    if malformed {
        // Duplicates retain their first occurrence and null links add no species.
        fields.extend(field(b"GNAM", &order[0].to_le_bytes()));
        fields.extend(field(b"GNAM", &0u32.to_le_bytes()));
    }
    record(b"LTEX", texture, fields)
}

/// Build three species, a nonascending list, and a healthy neighboring texture and cell.
fn fixture(root: &Path, reader: RecordReader, overridden: bool, malformed: bool) -> PipelineConfig {
    let data = root.join("Data");
    fs::create_dir_all(&data).unwrap();
    fs::write(
        data.join("Skyrim.esm"),
        dummy_content::esm::plugin(&dummy_content::esm::Plugin {
            author: "Grass GNAM order regression",
            worldspace: "GrassOrderWorld",
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
    for grass in [LOW, MIDDLE, HIGH] {
        plugin.extend(record(
            b"GRAS",
            grass,
            [
                field(b"MODL", b"grass/optional.nif\0"),
                field(b"DATA", &rule),
            ]
            .concat(),
        ));
    }
    plugin.extend(landscape(TEXTURE, &BASE_ORDER, malformed));
    plugin.extend(landscape(NEIGHBOR_TEXTURE, &[LOW], false));
    plugin.extend(group(
        1,
        1,
        &[land(0x10, TEXTURE), land(0x20, NEIGHBOR_TEXTURE)].concat(),
    ));
    fs::write(data.join("Order.esp"), plugin).unwrap();
    let mut selection = String::from("*Skyrim.esm\n*Order.esp\n");
    if overridden {
        // Order.esp is master index one here, preserving the species' owning identities.
        // Only the list order changes: the override names exactly the same three species.
        let patch = [
            record(
                b"TES4",
                0,
                [
                    field(b"MAST", b"Skyrim.esm\0"),
                    field(b"DATA", &[0; 8]),
                    field(b"MAST", b"Order.esp\0"),
                    field(b"DATA", &[0; 8]),
                ]
                .concat(),
            ),
            landscape(TEXTURE, &OVERRIDE_ORDER, malformed),
        ]
        .concat();
        fs::write(data.join("OrderPatch.esp"), patch).unwrap();
        selection.push_str("*OrderPatch.esp\n");
    }
    let plugins_file = root.join("plugins.txt");
    fs::write(&plugins_file, selection).unwrap();
    let mut config = PipelineConfig::new(data, root.join("pack"));
    config.plugins_file = Some(plugins_file);
    config.record_reader = reader;
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    config
}

/// Convert and publish through baseline-compatible public APIs, then inspect the archive.
async fn check_order(reader: RecordReader, overridden: bool, malformed: bool) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), reader, overridden, malformed);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), sender)
        .await
        .unwrap();
    drain.await.unwrap();
    if !malformed {
        assert!(report.complete, "{:?}", report.warnings);
    }
    // Reader recovery can mark record data incomplete while still publishing usable output.
    assert_eq!(report.skipped, 0, "reader recovery must not fail assets");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(config.output_dir.join("conversion-manifest.json")).unwrap(),
    )
    .unwrap();
    assert!(manifest["failures"].as_object().unwrap().is_empty());
    for artifact in ["skyrim_world.db", "cell_cache.rkyv", "grass_cache.rkyv"] {
        assert!(
            config.output_dir.join(artifact).is_file(),
            "unpublished {artifact}"
        );
    }
    let bytes = fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap();
    let cache = rkyv::from_bytes::<GrassCache, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(
        cache.types.len(),
        3,
        "all healthy GRAS records must survive"
    );
    assert_eq!(
        cache.cells.len(),
        2,
        "the healthy neighboring cell must survive"
    );
    let neighbor = cache
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x20)
        .unwrap();
    assert_eq!(neighbor.quadrants[0].layers.len(), 1);
    assert_eq!(neighbor.quadrants[0].layers[0].grass_ids, [LOW]);
    let cell = cache
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x10)
        .unwrap();
    assert_eq!(cell.quadrants[0].layers.len(), 1);
    let expected = if overridden {
        OVERRIDE_ORDER
    } else {
        BASE_ORDER
    };
    assert_eq!(
        cell.quadrants[0].layers[0].grass_ids, expected,
        "published species must retain the winning LTEX GNAM order"
    );
}

/// In-house full publication must retain a non-FormID base list.
#[tokio::test]
async fn inhouse_publication_preserves_base_gnam_order() {
    check_order(RecordReader::Inhouse, false, false).await;
}

/// Legacy full publication must retain a non-FormID base list.
#[tokio::test]
async fn legacy_publication_preserves_base_gnam_order() {
    check_order(RecordReader::Legacy, false, false).await;
}

/// In-house publication must use an override that only reorders the same species.
#[tokio::test]
async fn inhouse_publication_preserves_winning_gnam_order() {
    check_order(RecordReader::Inhouse, true, false).await;
}

/// Legacy publication must use an override that only reorders the same species.
#[tokio::test]
async fn legacy_publication_preserves_winning_gnam_order() {
    check_order(RecordReader::Legacy, true, false).await;
}

/// A broken GNAM occurrence must not prevent in-house publication of valid neighbors.
#[tokio::test]
async fn inhouse_publication_recovers_malformed_gnam_with_valid_neighbors() {
    check_order(RecordReader::Inhouse, true, true).await;
}

/// A broken GNAM occurrence must not prevent legacy publication of valid neighbors.
#[tokio::test]
async fn legacy_publication_recovers_malformed_gnam_with_valid_neighbors() {
    check_order(RecordReader::Legacy, true, true).await;
}
