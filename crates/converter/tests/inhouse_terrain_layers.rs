//! Full terrain preservation and the existing six-layer runtime policy through publication.

mod common;

use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::types::ArchivedRecordData,
    records::history_source::{SourceQuery, visit_source_history},
    shared::{CachedLand, CellCache, TerrainLayer, TerrainWeight},
};
use dummy_content::{
    inhouse_magic::{record, subrecord},
    layout,
};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

const LAND_SOURCE_ID: u32 = 0x0100_08A1;
const LAND_GLOBAL_ID: u32 = 0x0200_08A1;
const TEXTURE_SOURCE_ID: u32 = 0x0100_1801;
const TEXTURE_GLOBAL_ID: u32 = 0x0200_1801;
const CELL_ID: u32 = 0x1000;

/// Synthetic assets remain alive while publication and guarded source replay are inspected.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    header: Vec<u8>,
    fields: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Fixture {
    /// Native owner index one deliberately differs from global full-plugin slot two.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        layout::prepare_directory(&data, false).unwrap();
        layout::generate(
            &data,
            layout::DEFAULT_SEED,
            layout::Formats::parse("dds,nif,pex,esm").unwrap(),
        )
        .unwrap();
        let base = fs::read(data.join("Skyrim.esm")).unwrap();
        let length = u32::from_le_bytes(base[4..8].try_into().unwrap()) as usize;
        let header = base[24..24 + length].to_vec();
        let fixture = Self {
            directory,
            data,
            header,
            fields: authored_fields(),
        };
        fixture.patch("Spare.esm", &["Skyrim.esm"], &[]);
        let mut content = record(
            b"LTEX",
            TEXTURE_SOURCE_ID,
            0,
            &subrecord(b"EDID", b"AsymmetricTerrainTexture\0"),
        );
        content.extend(cell_children(&record(
            b"LAND",
            LAND_SOURCE_ID,
            0,
            &frame_fields(&fixture.fields),
        )));
        fixture.patch("Terrain.esm", &["Skyrim.esm"], &content);
        fixture
    }

    /// Explicit native master lists distinguish supplier indices from global load-order slots.
    fn patch(&self, name: &str, masters: &[&str], content: &[u8]) {
        let mut header = self.header.clone();
        for master in masters {
            header.extend(subrecord(b"MAST", format!("{master}\0").as_bytes()));
            header.extend(subrecord(b"DATA", &[0xA7; 8]));
        }
        let flags = u32::from(name.ends_with(".esm"));
        let mut bytes = record(b"TES4", 0, flags, &header);
        bytes.extend(content);
        fs::write(self.data.join(name), bytes).unwrap();
    }

    /// Publication always uses the authored priority order, including the spare full-plugin slot.
    fn list(&self, extra: &[&str]) -> PathBuf {
        let names: Vec<_> = ["Skyrim.esm", "Spare.esm", "Terrain.esm"]
            .into_iter()
            .chain(extra.iter().copied())
            .collect();
        let path = self.directory.path().join("plugins.txt");
        fs::write(
            &path,
            names
                .iter()
                .map(|name| format!("*{name}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let normalized = converter::esm::read_plugins_txt(&path, &self.data).unwrap();
        let actual: Vec<_> = normalized
            .iter()
            .map(|plugin| plugin.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(actual, names, "synthetic native priority must be explicit");
        path
    }

    /// Run the actual full asset publication path, with LOD disabled for this fixture.
    async fn publish(&self, extra: &[&str]) -> PathBuf {
        let output = self.directory.path().join("pack");
        let mut config = PipelineConfig::new(&self.data, &output);
        config.plugins_file = Some(self.list(extra));
        config.record_reader = RecordReader::Inhouse;
        config.no_lod = true;
        config.cpu_jobs = 2;
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let report = AssetPipeline::run_async(config, tx).await.unwrap();
        drain.await.unwrap();
        common::assert_reader_warning_publication(&report, &output);
        for name in ["cell_cache.rkyv", "cell_cache_preserved.rkyv"] {
            assert!(report.artifacts.contains(&PathBuf::from(name)));
        }
        output
    }
}

/// Frame native groups independently of converter selection and cache normalization.
fn group(kind: i32, label: u32, content: &[u8]) -> Vec<u8> {
    let mut bytes = b"GRUP".to_vec();
    bytes.extend(u32::try_from(content.len() + 24).unwrap().to_le_bytes());
    bytes.extend(label.to_le_bytes());
    bytes.extend(kind.to_le_bytes());
    bytes.extend([0; 8]);
    bytes.extend(content);
    bytes
}

/// Source group labels prove the terrain's parent CELL without an invented optional link.
fn cell_children(content: &[u8]) -> Vec<u8> {
    group(6, CELL_ID, &group(9, CELL_ID, content))
}

/// Encode native assignments with distinct unused bytes and sparse overlay slots.
fn assignment(tag: &[u8; 4], id: u32, quadrant: u8, slot: u16) -> (Vec<u8>, Vec<u8>) {
    let mut bytes = id.to_le_bytes().to_vec();
    bytes.extend([quadrant, 0xA7]);
    bytes.extend(slot.to_le_bytes());
    (tag.to_vec(), bytes)
}

/// Encode independent native VTXT facts, preserving nonzero unused source bytes.
fn weights(values: &[(u16, f32)]) -> (Vec<u8>, Vec<u8>) {
    let mut bytes = Vec::new();
    for (vertex, opacity) in values {
        bytes.extend(vertex.to_le_bytes());
        bytes.extend([0x53, 0xC9]);
        bytes.extend(opacity.to_le_bytes());
    }
    (b"VTXT".to_vec(), bytes)
}

/// Two over-capacity quadrants distinguish weight sums, sparse slots, ties, and missing IDs.
fn authored_fields() -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut heightmap = 13.0f32.to_le_bytes().to_vec();
    heightmap.extend(vec![0; 33 * 33 + 3]);
    let mut fields = vec![
        (b"VNML".to_vec(), vec![19; 33 * 33 * 3]),
        (b"VHGT".to_vec(), heightmap),
        (b"VCLR".to_vec(), vec![29; 33 * 33 * 3]),
        assignment(b"BTXT", 4, 0, 0),
    ];
    for (slot, id, values) in [
        (2, TEXTURE_SOURCE_ID, vec![(3, 0.25), (211, 0.5)]),
        (5, 0x844, vec![(17, 0.875)]),
        (7, TEXTURE_SOURCE_ID, vec![(31, 0.125)]),
        (9, 0x845, vec![(43, 0.125)]),
        (11, TEXTURE_SOURCE_ID, vec![(59, 0.25)]),
        (12, 4, vec![(71, 0.25)]),
        (41, 4, vec![(287, 1.0)]),
        (7, 4, vec![(83, 0.5)]), // Later nonnull assignment owns this slot and its weights.
        (2, 0, vec![(97, 0.9375)]), // Native NULL does not displace the earlier active slot.
    ] {
        fields.push(assignment(b"ATXT", id, 0, slot));
        fields.push(weights(&values));
    }
    // Quadrant one lacks an authored base; normalization supplies the implicit zero base.
    for (slot, id, values) in [
        (1, TEXTURE_SOURCE_ID, vec![(7, 0.875)]),
        (4, 4, vec![(23, 0.5)]),
        (6, 4, vec![(37, 0.375)]),
        (8, TEXTURE_SOURCE_ID, vec![(101, 0.25)]),
        (13, 0x847, vec![(127, 0.125)]),
        (14, 4, vec![(149, 0.25)]),
        (53, 4, vec![(269, 0.625)]),
    ] {
        fields.push(assignment(b"ATXT", id, 1, slot));
        fields.push(weights(&values));
    }
    fields.push(assignment(b"BTXT", 0x846, 2, 0));
    fields
}

/// Recreate exact source framing from fixture facts, without using decoder serialization.
fn frame_fields(fields: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (tag, data) in fields {
        bytes.extend(subrecord(tag.as_slice().try_into().unwrap(), data));
    }
    bytes
}

/// Construct literal expected layers, independently of production normalization or trimming.
fn expected_layer(id: u32, quadrant: u8, slot: u16, values: &[(u16, f32)]) -> TerrainLayer {
    TerrainLayer {
        texture_form_id: id,
        quadrant,
        layer: slot,
        is_base: values.is_empty(),
        weights: values
            .iter()
            .map(|&(vertex, opacity)| TerrainWeight { vertex, opacity })
            .collect(),
    }
}

/// The normalized preservation contract retains all seventeen active source layers.
fn expected_preserved_layers() -> Vec<TerrainLayer> {
    vec![
        expected_layer(4, 0, 0, &[]),
        expected_layer(TEXTURE_GLOBAL_ID, 0, 2, &[(3, 0.25), (211, 0.5)]),
        expected_layer(0x844, 0, 5, &[(17, 0.875)]),
        expected_layer(4, 0, 7, &[(83, 0.5)]),
        expected_layer(0x845, 0, 9, &[(43, 0.125)]),
        expected_layer(TEXTURE_GLOBAL_ID, 0, 11, &[(59, 0.25)]),
        expected_layer(4, 0, 12, &[(71, 0.25)]),
        expected_layer(4, 0, 41, &[(287, 1.0)]),
        expected_layer(0, 1, 0, &[]),
        expected_layer(TEXTURE_GLOBAL_ID, 1, 1, &[(7, 0.875)]),
        expected_layer(4, 1, 4, &[(23, 0.5)]),
        expected_layer(4, 1, 6, &[(37, 0.375)]),
        expected_layer(TEXTURE_GLOBAL_ID, 1, 8, &[(101, 0.25)]),
        expected_layer(0x847, 1, 13, &[(127, 0.125)]),
        expected_layer(4, 1, 14, &[(149, 0.25)]),
        expected_layer(4, 1, 53, &[(269, 0.625)]),
        expected_layer(0x846, 2, 0, &[]),
    ]
}

/// Runtime keeps five strongest overlays per quadrant before clearing unresolved texture keys.
fn expected_runtime_layers() -> Vec<TerrainLayer> {
    vec![
        expected_layer(4, 0, 0, &[]),
        expected_layer(TEXTURE_GLOBAL_ID, 0, 2, &[(3, 0.25), (211, 0.5)]),
        expected_layer(0, 0, 5, &[(17, 0.875)]),
        expected_layer(4, 0, 7, &[(83, 0.5)]),
        expected_layer(TEXTURE_GLOBAL_ID, 0, 11, &[(59, 0.25)]),
        expected_layer(4, 0, 41, &[(287, 1.0)]),
        expected_layer(0, 1, 0, &[]),
        expected_layer(TEXTURE_GLOBAL_ID, 1, 1, &[(7, 0.875)]),
        expected_layer(4, 1, 4, &[(23, 0.5)]),
        expected_layer(4, 1, 6, &[(37, 0.375)]),
        expected_layer(TEXTURE_GLOBAL_ID, 1, 8, &[(101, 0.25)]),
        expected_layer(4, 1, 53, &[(269, 0.625)]),
        expected_layer(0, 2, 0, &[]),
    ]
}

/// Inspect the real published archive by stable CELL identity.
fn cached_land(output: &Path, name: &str) -> CachedLand {
    let cache =
        rkyv::from_bytes::<CellCache, rkyv::rancor::Error>(&fs::read(output.join(name)).unwrap())
            .unwrap();
    cache
        .cells
        .into_iter()
        .find(|cell| cell.cell_id == CELL_ID)
        .unwrap()
}

/// Assert both layer projections, untouched geometry, and complete SQL preservation.
fn assert_published_layers(output: &Path) {
    let preserved = cached_land(output, "cell_cache_preserved.rkyv");
    let runtime = cached_land(output, "cell_cache.rkyv");
    assert_eq!(
        preserved.layers,
        expected_preserved_layers(),
        "preserved cache must retain over-capacity layers, including dropped missing references"
    );
    assert_eq!(runtime.layers, expected_runtime_layers());
    let mut expected_runtime = preserved.clone();
    expected_runtime.layers = expected_runtime_layers();
    assert_eq!(
        runtime, expected_runtime,
        "capacity affects only runtime layers"
    );
    assert_eq!(preserved.heights, vec![104.0; 33 * 33]);
    assert_eq!(preserved.normals, vec![19; 33 * 33 * 3]);
    assert_eq!(preserved.vertex_colors, vec![29; 33 * 33 * 3]);
    for (quadrant, full_count, runtime_count) in [(0, 8, 6), (1, 8, 6), (2, 1, 1), (3, 0, 0)] {
        assert_eq!(
            preserved
                .layers
                .iter()
                .filter(|layer| layer.quadrant == quadrant)
                .count(),
            full_count
        );
        assert_eq!(
            runtime
                .layers
                .iter()
                .filter(|layer| layer.quadrant == quadrant)
                .count(),
            runtime_count
        );
    }
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    // R5 normalizes unresolved texture membership into scalar child rows.
    let (cell, priority, blob): (u32, u32, Vec<u8>) = db
        .query_row(
            "SELECT cell_id,load_order,layer_data FROM inhouse_terrain_layers WHERE form_id=?",
            [LAND_GLOBAL_ID],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((cell, priority), (CELL_ID, 2));
    assert_eq!(
        rkyv::from_bytes::<Vec<TerrainLayer>, rkyv::rancor::Error>(&blob).unwrap(),
        expected_preserved_layers(),
        "SQL layer_data must retain every normalized active source layer"
    );
    let unresolved = db
        .prepare("SELECT texture_form_id FROM inhouse_unresolved_terrain_textures WHERE form_id=? ORDER BY texture_form_id")
        .unwrap()
        .query_map([LAND_GLOBAL_ID], |row| row.get::<_, u32>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        unresolved,
        vec![0x844, 0x845, 0x846, 0x847],
        "unresolved inventory includes missing references in both retained and discarded runtime layers"
    );
    for id in [4, TEXTURE_GLOBAL_ID] {
        assert_eq!(
            db.query_row(
                "SELECT record_type FROM records WHERE form_id=?",
                [id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "LTEX"
        );
    }
    let (heightmap, colors): (Vec<u8>, Vec<u8>) = db
        .query_row(
            "SELECT heightmap,vclr FROM land WHERE cell_id=?",
            [CELL_ID],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(f32::from_le_bytes(heightmap[..4].try_into().unwrap()), 13.0);
    assert_eq!(colors, preserved.vertex_colors);
}

/// Full source and canonical archives retain every assignment, unused byte, and weight.
fn assert_source_and_canonical(output: &Path, fields: &[(Vec<u8>, Vec<u8>)]) {
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let (source, priority, payload): (u32, u32, Vec<u8>) = db
        .query_row(
            "SELECT source_form_id,load_order,payload FROM inhouse_source_records WHERE form_id=?",
            [LAND_GLOBAL_ID],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((source, priority), (LAND_SOURCE_ID, 2));
    assert_eq!(payload, frame_fields(fields));
    let canonical: Vec<u8> = db
        .query_row(
            "SELECT data FROM records WHERE form_id=?",
            [LAND_GLOBAL_ID],
            |row| row.get(0),
        )
        .unwrap();
    let canonical =
        rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&canonical).unwrap();
    assert_eq!(canonical.subrecords.len(), fields.len());
    for (actual, (tag, native)) in canonical.subrecords.iter().zip(fields) {
        assert_eq!(actual.tag.as_slice(), tag.as_slice());
        let mut expected = native.clone();
        if matches!(tag.as_slice(), b"BTXT" | b"ATXT") {
            let id = u32::from_le_bytes(expected[..4].try_into().unwrap());
            let resolved = match id {
                TEXTURE_SOURCE_ID => TEXTURE_GLOBAL_ID,
                0x844..=0x847 => 0,
                other => other,
            };
            expected[..4].copy_from_slice(&resolved.to_le_bytes());
        }
        assert_eq!(
            actual.data, expected,
            "canonical field {tag:?} must remain complete"
        );
    }
}

/// Archive preservation retains over-capacity layers without certifying missing texture links.
#[tokio::test]
async fn over_capacity_layers_publish_but_reader_loss_blocks_metadata_rebuild() {
    let fixture = Fixture::new();
    let output = fixture.publish(&[]).await;
    assert_published_layers(&output);
    assert_source_and_canonical(&output, &fixture.fields);
    let expected_payload = frame_fields(&fixture.fields);
    let expected_header = record(b"LAND", LAND_SOURCE_ID, 0, &expected_payload)[..24].to_vec();
    let mut seen = 0;
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Source {
            original_owner: "terrain.esm".into(),
            full_local: 0x08A1,
        },
        |version| {
            assert_eq!(version.status, "accepted");
            assert_eq!(version.plugin_id, 2);
            assert_eq!(version.runtime_form_id, Some(LAND_GLOBAL_ID));
            assert!(version.accepted_source_winner);
            let raw = version.raw.as_ref().unwrap();
            assert_eq!(raw.header.as_slice(), expected_header.as_slice());
            assert_eq!(
                raw.stored_payload.as_deref(),
                Some(expected_payload.as_slice())
            );
            seen += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(seen, 1);
    assert!(summary.fully_raw_available);
    assert!(!summary.typed_runtime_decoding_attempted);
    let rebuilt = fixture.directory.path().join("metadata");
    let mut config = PipelineConfig::new(&fixture.data, &rebuilt);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let cache_bytes: Vec<_> = ["cell_cache.rkyv", "cell_cache_preserved.rkyv"]
        .into_iter()
        .map(|name| (name, fs::read(output.join(name)).unwrap()))
        .collect();
    let result = AssetPipeline::rebuild_metadata_async(config, &output, tx).await;
    drain.await.unwrap();
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains("requires complete converter"),
        "{error:#}"
    );
    assert!(!rebuilt.exists(), "rejected rebuild must not publish");
    for (name, bytes) in cache_bytes {
        assert_eq!(
            fs::read(output.join(name)).unwrap(),
            bytes,
            "rejected rebuild must preserve source {name}"
        );
    }
    assert_published_layers(&output);
    assert_source_and_canonical(&output, &fixture.fields);
}

/// Malformed fields and compressed overrides stay local, preserving accepted layers and neighbors.
#[tokio::test]
async fn malformed_over_capacity_overrides_keep_prior_layers_and_valid_neighbors() {
    let fixture = Fixture::new();
    let mut short_weights = fixture.fields.clone();
    short_weights.push(assignment(b"ATXT", 4, 0, 67));
    short_weights.push((b"VTXT".to_vec(), vec![0x7D; 7]));
    let mut bad_quadrant = fixture.fields.clone();
    bad_quadrant.push(assignment(b"ATXT", 4, 4, 71));
    bad_quadrant.push(weights(&[(23, 0.625)]));
    let cases = [
        ("BadWeights.esp", 0, frame_fields(&short_weights)),
        ("BadQuadrant.esp", 0, frame_fields(&bad_quadrant)),
        ("Torn.esp", 0x40000, vec![24, 0, 0, 0, 99]),
    ];
    for (name, flags, payload) in &cases {
        let mut content = record(
            b"STAT",
            0x0200_0D01,
            0,
            &subrecord(b"EDID", b"BeforeBrokenTerrain\0"),
        );
        content.extend(record(b"LAND", LAND_SOURCE_ID, *flags, payload));
        content.extend(record(
            b"STAT",
            0x0200_0D02,
            0,
            &subrecord(b"EDID", b"AfterBrokenTerrain\0"),
        ));
        fixture.patch(
            name,
            &["Skyrim.esm", "Terrain.esm"],
            &cell_children(&content),
        );
    }
    let output = fixture
        .publish(&["BadWeights.esp", "BadQuadrant.esp", "Torn.esp"])
        .await;
    assert_published_layers(&output);
    assert_source_and_canonical(&output, &fixture.fields);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    for slot in 3..=5 {
        for local in [0x0D01, 0x0D02] {
            let id = (slot << 24) | local;
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM records WHERE form_id=?",
                    [id],
                    |row| row.get::<_, u32>(0)
                )
                .unwrap(),
                1
            );
        }
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    for name in ["badweights.esp", "badquadrant.esp", "torn.esp"] {
        assert_eq!(diagnostics["decoder"][name]["skipped_records"], 1);
    }
    let accepted_payload = frame_fields(&fixture.fields);
    let mut seen = 0;
    let summary = visit_source_history(
        &output,
        None,
        SourceQuery::Source {
            original_owner: "terrain.esm".into(),
            full_local: 0x08A1,
        },
        |version| {
            let raw = version.raw.as_ref().unwrap();
            assert_eq!(version.runtime_form_id, Some(LAND_GLOBAL_ID));
            assert_eq!(version.source_form_id, LAND_SOURCE_ID);
            if seen == 0 {
                assert_eq!(version.status, "accepted");
                assert!(version.accepted_source_winner);
                assert!(version.runtime_fallback_before_latest_source);
                assert_eq!(
                    raw.stored_payload.as_deref(),
                    Some(accepted_payload.as_slice())
                );
            } else {
                let (name, flags, payload) = &cases[seen - 1];
                assert_eq!(version.supplying_plugin.to_lowercase(), name.to_lowercase());
                assert_eq!(version.status, "skipped");
                assert!(!version.accepted_source_winner);
                assert!(version.rejection_reason.is_some());
                assert_eq!(version.flags, *flags);
                assert_eq!(raw.stored_payload.as_deref(), Some(payload.as_slice()));
            }
            seen += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(seen, 4, "every rejected override remains in source history");
    assert!(summary.fully_raw_available);
    assert!(!summary.typed_runtime_decoding_attempted);
}
