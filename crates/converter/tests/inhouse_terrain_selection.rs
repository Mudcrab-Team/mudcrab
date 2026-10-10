//! Native priority and physical order select complete terrain before SQLite/cache publication.
use converter::{AssetPipeline, PipelineConfig, config::RecordReader};
use dummy_content::{
    inhouse_magic::{record, subrecord},
    layout,
};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// A usable generated world plus independently framed duplicate-parent LAND plugins.
struct Fixture {
    directory: tempfile::TempDir,
    data: PathBuf,
    header: Vec<u8>,
}

impl Fixture {
    /// Generate legal synthetic assets and append a distinct base terrain assignment.
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
        let base_path = data.join("Skyrim.esm");
        let mut base = fs::read(&base_path).unwrap();
        let length = u32::from_le_bytes(base[4..8].try_into().unwrap()) as usize;
        let header = base[24..24 + length].to_vec();
        base.extend(cell_children(&land(0x1001, 3.0, 7)));
        fs::write(base_path, base).unwrap();
        let fixture = Self {
            directory,
            data,
            header,
        };
        fixture.patch("Spare.esm", false, &["Skyrim.esm"], &[]);
        fixture.patch("FirstLight.esl", true, &["Skyrim.esm"], &[]);
        let mut full = land(0x1001, 5.0, 11); // Override an ID owned by the master.
        full.extend(land(0x0100_08F2, 7.0, 13));
        full.extend(land(0x0100_08A1, 13.0, 19)); // Lower ID, later physical position.
        fixture.patch("Terrain.esm", false, &["Skyrim.esm"], &cell_children(&full));
        fixture.patch(
            "TerrainLight.esl",
            true,
            &["Skyrim.esm"],
            &cell_children(&land(0x0100_0ABC, 21.0, 29)),
        );
        fixture
    }

    /// Explicit masters distinguish file-relative identity from global full/light slots.
    fn patch(&self, name: &str, light: bool, masters: &[&str], content: &[u8]) {
        let mut header = self.header.clone();
        for master in masters {
            let mut name = master.as_bytes().to_vec();
            name.push(0);
            header.extend(subrecord(b"MAST", &name));
            header.extend(subrecord(b"DATA", &[0xA7; 8]));
        }
        let flags = if light {
            0x201
        } else if name.ends_with(".esm") {
            1
        } else {
            0
        };
        let mut bytes = record(b"TES4", 0, flags, &header);
        bytes.extend(content);
        fs::write(self.data.join(name), bytes).unwrap();
    }

    /// Save each tested load order explicitly; light slots are independent of full slots.
    fn list(&self, names: &[&str], name: &str) -> PathBuf {
        let path = self.directory.path().join(name);
        let text: String = names.iter().map(|plugin| format!("*{plugin}\n")).collect();
        fs::write(&path, text).unwrap();
        let normalized = converter::esm::read_plugins_txt(&path, &self.data).unwrap();
        let actual: Vec<_> = normalized
            .iter()
            .map(|plugin| plugin.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            actual.as_slice(),
            names,
            "fixture must retain its explicit native priority order"
        );
        path
    }
}

/// Frame a group using native sizes/types, independently of converter internals.
fn group(kind: i32, label: u32, content: &[u8]) -> Vec<u8> {
    let mut bytes = b"GRUP".to_vec();
    bytes.extend((content.len() as u32 + 24).to_le_bytes());
    bytes.extend(label.to_le_bytes());
    bytes.extend(kind.to_le_bytes());
    bytes.extend([0; 8]);
    bytes.extend(content);
    bytes
}

/// Both groups name the master's cell; no optional field fabricates the parent relationship.
fn cell_children(content: &[u8]) -> Vec<u8> {
    group(6, 0x1000, &group(9, 0x1000, content))
}

/// Asymmetric authored heights/normals/colors expose complete-record selection.
fn land(id: u32, offset: f32, marker: u8) -> Vec<u8> {
    let mut heightmap = offset.to_le_bytes().to_vec();
    heightmap.extend(vec![0; 33 * 33 + 3]);
    let mut fields = subrecord(b"VNML", &vec![marker; 33 * 33 * 3]);
    fields.extend(subrecord(b"VHGT", &heightmap));
    fields.extend(subrecord(b"VCLR", &vec![marker + 1; 33 * 33 * 3]));
    record(b"LAND", id, 0, &fields)
}

/// Inspect actual archived terrain, not a comparator's transformed presentation.
fn assert_terrain(output: &Path, height: f32, marker: u8) {
    let cache = rkyv::from_bytes::<converter::shared::CellCache, rkyv::rancor::Error>(
        &fs::read(output.join("cell_cache.rkyv")).unwrap(),
    )
    .unwrap();
    let cell = cache
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x1000)
        .unwrap();
    assert_eq!(cell.heights, vec![height; 33 * 33]);
    assert_eq!(cell.normals, vec![marker as i8; 33 * 33 * 3]);
    assert_eq!(cell.vertex_colors, vec![marker + 1; 33 * 33 * 3]);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    let (heightmap, colors): (Vec<u8>, Vec<u8>) = db
        .query_row(
            "SELECT heightmap,vclr FROM land WHERE cell_id=4096",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        f32::from_le_bytes(heightmap[..4].try_into().unwrap()) * 8.0,
        height
    );
    assert_eq!(colors, vec![marker + 1; 33 * 33 * 3]);
}

/// Native order beats ID order; plugin priority beats full/light identity and file offsets.
#[test]
fn published_terrain_uses_native_order_and_winning_full_light_priority() {
    let fixture = Fixture::new();
    for (index, light_later) in [false, true].into_iter().enumerate() {
        let tail = if light_later {
            ["Terrain.esm", "TerrainLight.esl"]
        } else {
            ["TerrainLight.esl", "Terrain.esm"]
        };
        let list = fixture.list(
            &[
                "Skyrim.esm",
                "Spare.esm",
                "FirstLight.esl",
                tail[0],
                tail[1],
            ],
            &format!("order-{index}.txt"),
        );
        let output = fixture.directory.path().join(format!("output-{index}"));
        converter::esm::inhouse::export_record_bundle(&fixture.data, &list, &output).unwrap();
        assert_terrain(
            &output,
            if light_later { 168.0 } else { 104.0 },
            if light_later { 29 } else { 19 },
        );
        let db = Connection::open(output.join("skyrim_world.db")).unwrap();
        for id in [0x1001u32, 0x0200_08F2, 0x0200_08A1, 0xFE00_1ABC] {
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
        let earlier: u64 = db
            .query_row(
                "SELECT source_record_offset FROM inhouse_terrain_source_order WHERE form_id=?",
                [0x0200_08F2u32],
                |row| row.get(0),
            )
            .unwrap();
        let later: u64 = db
            .query_row(
                "SELECT source_record_offset FROM inhouse_terrain_source_order WHERE form_id=?",
                [0x0200_08A1u32],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            earlier < later,
            "physical order must not be sorted by FormID"
        );
    }
}

/// A malformed highest-priority override cannot abort publication or discard valid neighbors.
#[tokio::test]
async fn malformed_terrain_override_preserves_prior_winner_and_neighbor_publication() {
    let fixture = Fixture::new();
    let mut broken = record(
        b"STAT",
        0x0200_0D01,
        0,
        &subrecord(b"EDID", b"BeforeBadTerrain\0"),
    );
    broken.extend(record(b"LAND", 0x0100_08A1, 0x40000, &[24, 0, 0, 0, 99]));
    broken.extend(record(
        b"STAT",
        0x0200_0D02,
        0,
        &subrecord(b"EDID", b"AfterBadTerrain\0"),
    ));
    fixture.patch(
        "Damaged.esp",
        false,
        &["Skyrim.esm", "Terrain.esm"],
        &cell_children(&broken),
    );
    let plugins = fixture.list(
        &[
            "Skyrim.esm",
            "Spare.esm",
            "FirstLight.esl",
            "TerrainLight.esl",
            "Terrain.esm",
            "Damaged.esp",
        ],
        "damaged.txt",
    );
    let output = fixture.directory.path().join("pack");
    let mut config = PipelineConfig::new(&fixture.data, &output);
    config.plugins_file = Some(plugins);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert_terrain(&output, 104.0, 19);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [0x0300_0D01u32, 0x0300_0D02] {
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
    let priority: u32 = db
        .query_row(
            "SELECT load_order FROM inhouse_source_records WHERE form_id=?",
            [0x0200_08A1u32],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(priority, 4);
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["damaged.esp"]["skipped_records"], 1);
}

/// Missing targets retain their authored weights and source identities through publication.
#[tokio::test]
async fn published_terrain_preserves_missing_texture_assignment_without_reviving_native_null() {
    let fixture = Fixture::new();
    let framed = land(0x0100_08A1, 13.0, 19);
    let mut fields = framed[24..].to_vec();
    let assignment = |id: u32| {
        let mut bytes = id.to_le_bytes().to_vec();
        bytes.extend([0, 0]); // Quadrant zero and unused byte.
        bytes.extend(9u16.to_le_bytes());
        subrecord(b"ATXT", &bytes)
    };
    let weights = |vertex: u16, opacity: f32| {
        let mut bytes = vertex.to_le_bytes().to_vec();
        bytes.extend([0xA7, 0x53]); // Unused source bytes remain in native provenance.
        bytes.extend(opacity.to_le_bytes());
        subrecord(b"VTXT", &bytes)
    };
    fields.extend(assignment(4)); // An existing LTEX in the generated master.
    fields.extend(weights(2, 0.25));
    fields.extend(assignment(0x844)); // Later nonnull assignment owns slot nine.
    fields.extend(weights(9, 0.75));
    fields.extend(assignment(0)); // Authored NULL does not consume/displace that slot.
    fields.extend(weights(11, 0.5));
    let native_land = record(b"LAND", 0x0100_08A1, 0, &fields);
    fixture.patch(
        "Terrain.esm",
        false,
        &["Skyrim.esm"],
        &cell_children(&native_land),
    );
    let mut damaged = record(b"LAND", 0x0100_08A1, 0x40000, &[24, 0, 0, 0, 99]);
    damaged.extend(record(
        b"STAT",
        0x0200_0D03,
        0,
        &subrecord(b"EDID", b"NeighborOfMissingTexture\0"),
    ));
    fixture.patch(
        "Damaged.esp",
        false,
        &["Skyrim.esm", "Terrain.esm"],
        &cell_children(&damaged),
    );
    let plugins = fixture.list(&["Skyrim.esm", "Terrain.esm", "Damaged.esp"], "layers.txt");
    let output = fixture.directory.path().join("layers-pack");
    let mut config = PipelineConfig::new(&fixture.data, &output);
    config.plugins_file = Some(plugins);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    assert!(
        report
            .artifacts
            .contains(&PathBuf::from("cell_cache_preserved.rkyv"))
    );
    let read_cache = |name| {
        rkyv::from_bytes::<converter::shared::CellCache, rkyv::rancor::Error>(
            &fs::read(output.join(name)).unwrap(),
        )
        .unwrap()
    };
    let runtime = read_cache("cell_cache.rkyv");
    let preserved = read_cache("cell_cache_preserved.rkyv");
    let runtime = runtime
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x1000)
        .unwrap();
    let preserved = preserved
        .cells
        .iter()
        .find(|cell| cell.cell_id == 0x1000)
        .unwrap();
    assert_eq!(preserved.layers.len(), 2); // Implicit base plus the surviving nonnull overlay.
    assert_eq!(preserved.layers[0].texture_form_id, 0);
    assert!(preserved.layers[0].is_base);
    assert_eq!(preserved.layers[1].texture_form_id, 0x844);
    assert_eq!(preserved.layers[1].layer, 9);
    assert_eq!(
        preserved.layers[1].weights,
        vec![converter::shared::TerrainWeight {
            vertex: 9,
            opacity: 0.75
        }]
    );
    let mut expected_runtime = preserved.clone();
    expected_runtime.layers[1].texture_form_id = 0;
    assert_eq!(runtime, &expected_runtime);
    let db = Connection::open(output.join("skyrim_world.db")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT record_type FROM records WHERE form_id=4",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "LTEX"
    );
    let (blob, unresolved): (Vec<u8>, String) = db
        .query_row(
            "SELECT layer_data,unresolved_texture_ids FROM inhouse_terrain_layers WHERE form_id=?",
            [0x0100_08A1u32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(unresolved, "[2116]");
    assert_eq!(
        rkyv::from_bytes::<Vec<converter::shared::TerrainLayer>, rkyv::rancor::Error>(&blob)
            .unwrap(),
        preserved.layers
    );
    let native: Vec<u8> = db
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=?",
            [0x0100_08A1u32],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        native, fields,
        "raw provenance also retains authored NULL weights and unused bytes"
    );
    assert_eq!(
        db.query_row(
            "SELECT load_order FROM inhouse_terrain_layers WHERE form_id=?",
            [0x0100_08A1u32],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM records WHERE form_id=?",
            [0x0200_0D03u32],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["damaged.esp"]["skipped_records"], 1);
    let rebuilt = fixture.directory.path().join("layers-metadata");
    let mut config = PipelineConfig::new(&fixture.data, &rebuilt);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::rebuild_metadata_async(config, &output, tx)
        .await
        .unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let rebuilt_cache = rkyv::from_bytes::<converter::shared::CellCache, rkyv::rancor::Error>(
        &fs::read(rebuilt.join("cell_cache.rkyv")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        rebuilt_cache
            .cells
            .iter()
            .find(|cell| cell.cell_id == 0x1000)
            .unwrap()
            .layers,
        runtime.layers,
        "metadata rebuild must preserve the winning assignment rather than revive the earlier valid texture"
    );
    assert!(
        report
            .artifacts
            .contains(&PathBuf::from("cell_cache_preserved.rkyv"))
    );
    for name in ["cell_cache.rkyv", "cell_cache_preserved.rkyv"] {
        assert_eq!(
            fs::read(rebuilt.join(name)).unwrap(),
            fs::read(output.join(name)).unwrap(),
            "metadata rebuild must retain the same normalized layers and weights in {name}"
        );
    }
}
