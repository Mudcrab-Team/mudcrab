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
            [
                field(b"VHGT", &[0u8; 4 + 33 * 33 + 3]),
                field(b"BTXT", &[texture.to_le_bytes(), [0; 4]].concat()),
            ]
            .concat(),
        ),
    )
}

/// Author an LTEX list, optionally surrounding a broken occurrence with valid GNAMs.
fn landscape(texture: u32, order: &[u32], malformed: bool) -> Vec<u8> {
    let mut fields = field(b"TNAM", &2u32.to_le_bytes());
    for (index, grass) in order.iter().enumerate() {
        fields.extend(field(b"GNAM", &grass.to_le_bytes()));
        if malformed && index == 0 {
            fields.extend(field(b"GNAM", &[0x81, 0x08, 0x01]));
        }
    }
    if malformed {
        // Duplicates retain their authored occurrences and null links add no species.
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
    // The fixture supplies no landscape images: select the labelled white default (#811).
    config.terrain_default_texture = converter::config::TerrainDefaultTexture::White;
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
    let mut expected = if overridden {
        OVERRIDE_ORDER.to_vec()
    } else {
        BASE_ORDER.to_vec()
    };
    if malformed {
        expected.push(expected[0]);
    }
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

/// Modelless and incomplete rules retain their authored positions through full publication.
async fn check_placeholder_publication(reader: RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), reader, false, false);
    let path = config.data_dir.join("Order.esp");
    let mut bytes = fs::read(&path).unwrap();
    let mut data = vec![0u8; 32];
    data[0] = 63;
    data[2] = 90;
    data[12..16].copy_from_slice(&73.0f32.to_le_bytes());
    // Replace source definitions rather than appending colliding same-plugin records.
    // The first species loses only its model; the next loses DATA entirely.
    for (id, fields) in [
        (HIGH, field(b"DATA", &data)),
        (LOW, field(b"MODL", b"grass/incomplete.nif\0")),
    ] {
        let offset = bytes
            .windows(16)
            .position(|header| header[..4] == *b"GRAS" && header[12..16] == id.to_le_bytes())
            .unwrap();
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        bytes.splice(offset..offset + 24 + size, record(b"GRAS", id, fields));
    }
    fs::write(path, bytes).unwrap();
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
            .any(|w| w.contains("invalid grass rules")),
        "{:?}",
        report.warnings
    );
    let cache = rkyv::from_bytes::<GrassCache, rkyv::rancor::Error>(
        &fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        cache.types.len(),
        3,
        "resolved unusable species must retain their slots"
    );
    let placeholder = cache.types.iter().find(|r| r.form_id == HIGH).unwrap();
    assert!(placeholder.model_path.is_empty());
    assert_eq!(
        (placeholder.density, placeholder.position_range),
        (63, 73.0)
    );
    assert!(
        cache
            .types
            .iter()
            .find(|r| r.form_id == LOW)
            .unwrap()
            .model_path
            .is_empty()
    );
    let affected = cache.cells.iter().find(|c| c.cell_id == 0x10).unwrap();
    assert_eq!(affected.quadrants[0].layers[0].grass_ids, BASE_ORDER);
    assert!(cache.cells.iter().any(|c| c.cell_id == 0x20));
    assert!(config.output_dir.join("conversion-manifest.json").exists());
}

/// In-house publication retains unusable species and healthy neighbors.
#[tokio::test]
async fn inhouse_publication_retains_placeholder_species() {
    check_placeholder_publication(RecordReader::Inhouse).await;
}

/// Legacy publication retains the same unusable-species slots.
#[tokio::test]
async fn legacy_publication_retains_placeholder_species() {
    check_placeholder_publication(RecordReader::Legacy).await;
}

/// A lower-FormID LAND from a later plugin must own the whole restored selection.
async fn check_land_source_winner(reader: RecordReader) {
    let directory = tempfile::tempdir().unwrap();
    let config = fixture(directory.path(), reader, false, false);
    let path = config.data_dir.join("Order.esp");
    let bytes = fs::read(&path).unwrap();
    let old = land(0x10, TEXTURE);
    let loser = group(
        8,
        0x10,
        &record(
            b"LAND",
            0x0100_08f0,
            [
                field(b"VHGT", &[0u8; 4 + 33 * 33 + 3]),
                field(b"BTXT", &[NEIGHBOR_TEXTURE.to_le_bytes(), [0; 4]].concat()),
            ]
            .concat(),
        ),
    );
    let offset = bytes
        .windows(old.len())
        .position(|part| part == old)
        .unwrap();
    let mut replaced = bytes[..offset].to_vec();
    replaced.extend(&loser);
    replaced.extend(&bytes[offset + old.len()..]);
    // Framing lengths are unchanged because both LAND assignments have equal size.
    assert_eq!(replaced.len(), bytes.len());
    fs::write(path, replaced).unwrap();
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
        group(
            1,
            1,
            &group(
                8,
                0x10,
                &record(
                    b"LAND",
                    0x0100_0820,
                    [
                        field(b"VHGT", &[0u8; 4 + 33 * 33 + 3]),
                        field(b"BTXT", &[TEXTURE.to_le_bytes(), [0; 4]].concat()),
                    ]
                    .concat(),
                ),
            ),
        ),
    ]
    .concat();
    fs::write(config.data_dir.join("LandWinner.esp"), patch).unwrap();
    fs::write(
        config.plugins_file.as_ref().unwrap(),
        "*Skyrim.esm\n*Order.esp\n*LandWinner.esp\n",
    )
    .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config.clone(), sender)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let cache = rkyv::from_bytes::<GrassCache, rkyv::rancor::Error>(
        &fs::read(config.output_dir.join("grass_cache.rkyv")).unwrap(),
    )
    .unwrap();
    let winner = cache
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x10)
        .unwrap();
    assert_eq!(
        winner.quadrants[0].layers[0].grass_ids, BASE_ORDER,
        "restoration must use the winning lower-ID later-plugin LAND, never the higher-ID loser"
    );
    assert!(cache.cells.iter().any(|cell| cell.cell_id == 0x20));
    assert!(config.output_dir.join("conversion-manifest.json").is_file());
}

/// In-house publication restores precisely the selected LAND source.
#[tokio::test]
async fn inhouse_publication_uses_winning_land_source() {
    check_land_source_winner(RecordReader::Inhouse).await;
}

/// Legacy publication selects the same whole LAND by supplying plugin priority.
#[tokio::test]
async fn legacy_publication_uses_winning_land_source() {
    check_land_source_winner(RecordReader::Legacy).await;
}
