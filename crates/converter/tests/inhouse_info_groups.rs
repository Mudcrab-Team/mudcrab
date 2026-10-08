//! Native INFO topic-group provenance, slot mapping, and bounded owner recovery.
//! Metadata assertions use serialized keys so the pre-extension API still compiles.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::{inhouse::export_record_bundle_typed, load_order::LoadOrder, types::ArchivedRecordData},
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, inhouse_dialogue as native, layout};
use rusqlite::Connection;
use serde_json::{Value as Json, json};
use std::{collections::HashMap, fs, path::PathBuf};

/// Independent native GRUP framing; labels remain file-relative words.
fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"GRUP".to_vec();
    bytes.extend((24u32 + u32::try_from(payload.len()).unwrap()).to_le_bytes());
    bytes.extend(label.to_le_bytes());
    bytes.extend(kind.to_le_bytes());
    bytes.extend(0x1739_5b7du32.to_le_bytes());
    bytes.extend(0xa571u16.to_le_bytes());
    bytes.extend(0x2943u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Substantive INFO fields without a TPIC owner subrecord.
fn info(id: u32, text: &str) -> Vec<u8> {
    native::named(
        b"INFO",
        id,
        &[
            native::subrecord(b"TRDT", &[0; 24]),
            native::subrecord(b"NAM1", &native::text(text)),
        ],
    )
}

/// Real dummy world followed by independently framed source plugins.
struct Fixture {
    directory: tempfile::TempDir,
    generated: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Keep full slot zero occupied by the real dummy world.
    fn new() -> Self {
        let generated = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Skyrim.esm");
        fs::write(&path, &generated).unwrap();
        Self {
            directory,
            generated,
            paths: vec![path],
        }
    }

    /// Return the independently written TES4 size, for source-header offset assertions.
    fn add(&mut self, name: &str, masters: &[&str], flags: u32, records: &[u8]) -> usize {
        let header = native::plugin(&self.generated, masters, flags, &[]);
        let path = self.directory.path().join(name);
        fs::write(&path, [header.clone(), records.to_vec()].concat()).unwrap();
        self.paths.push(path);
        header.len()
    }

    /// Exercise accepted winners and final target validation through the public reader.
    fn read(&self) -> ReadResult {
        read_plugins(&self.paths, &LoadOrder::read(&self.paths).unwrap()).unwrap()
    }
}

/// Serialize the existing public contract without referring to newly added Rust members.
fn metadata(record: &DecodedRecord) -> Json {
    serde_json::to_value(record).unwrap()
}

/// Require all four source-provenance keys; missing baseline keys fail assertions.
fn assert_metadata(
    record: &Json,
    topic: Option<u32>,
    source: Option<u32>,
    group_offset: Option<usize>,
    record_offset: usize,
) {
    assert_eq!(
        record.get("topic_form_id"),
        Some(&json!(topic)),
        "resolved lexical topic"
    );
    assert_eq!(
        record.get("source_topic_form_id"),
        Some(&json!(source)),
        "native group label"
    );
    assert_eq!(
        record.get("topic_group_offset"),
        Some(&json!(group_offset)),
        "native GRUP7 header offset"
    );
    assert_eq!(
        record.get("source_record_offset"),
        Some(&json!(record_offset)),
        "native record header offset"
    );
}

/// Nested/sibling groups restore lexical ownership, retaining source physical order.
#[test]
fn grouped_info_without_tpic_keeps_lexical_owner_and_source_offsets() {
    let mut fixture = Fixture::new();
    let topic_a = native::named(b"DIAL", 0x4100, &[]);
    let topic_b = native::named(b"DIAL", 0x4101, &[]);
    let first = info(0x4200, "Outer before nested group");
    let inner_info = info(0x4201, "Nested topic");
    let restored = info(0x4202, "Outer after nested group");
    let sibling_info = info(0x4203, "Sibling topic");
    let outside = info(0x4204, "Outside any topic group");
    let inside_neighbor = native::named(b"MESG", 0x4205, &[]);
    let nested = group(7, 0x4101, &inner_info);
    let outer_payload = [
        first.clone(),
        nested.clone(),
        restored.clone(),
        inside_neighbor.clone(),
    ]
    .concat();
    let outer = group(7, 0x4100, &outer_payload);
    let sibling = group(7, 0x4101, &sibling_info);
    let records = [
        topic_a.clone(),
        topic_b.clone(),
        outer.clone(),
        outside.clone(),
        sibling.clone(),
    ]
    .concat();
    let header = fixture.add("Grouped.esm", &[], 1, &records);
    let outer_offset = header + topic_a.len() + topic_b.len();
    let nested_offset = outer_offset + 24 + first.len();
    let outside_offset = outer_offset + outer.len();
    let sibling_offset = outside_offset + outside.len();
    let result = fixture.read();
    for (id, topic, source, group_offset, record_offset) in [
        (
            0x0100_4200,
            Some(0x0100_4100),
            Some(0x4100),
            Some(outer_offset),
            outer_offset + 24,
        ),
        (
            0x0100_4201,
            Some(0x0100_4101),
            Some(0x4101),
            Some(nested_offset),
            nested_offset + 24,
        ),
        (
            0x0100_4202,
            Some(0x0100_4100),
            Some(0x4100),
            Some(outer_offset),
            nested_offset + nested.len(),
        ),
        (0x0100_4204, None, None, None, outside_offset),
        (
            0x0100_4203,
            Some(0x0100_4101),
            Some(0x4101),
            Some(sibling_offset),
            sibling_offset + 24,
        ),
    ] {
        let record = &result.records[&id];
        assert!(
            !record
                .fields
                .iter()
                .any(|field| field.signature == *b"TPIC")
        );
        assert_metadata(
            &metadata(record),
            topic,
            source,
            group_offset,
            record_offset,
        );
    }
    assert_metadata(
        &metadata(&result.records[&0x0100_4205]),
        None,
        None,
        None,
        nested_offset + nested.len() + restored.len(),
    );
    assert_eq!(result.diagnostics["grouped.esm"].invalid_links, 0);
}

/// Group labels use the declaring plugin's MAST list for asymmetric full/light targets.
#[test]
fn grouped_info_labels_remap_different_full_and_light_master_positions() {
    let mut fixture = Fixture::new();
    fixture.add("Light.esl", &[], 0x201, &native::named(b"DIAL", 0x801, &[]));
    fixture.add("Unused.esm", &[], 1, &[]);
    fixture.add(
        "FullTopics.esm",
        &[],
        1,
        &native::named(b"DIAL", 0x841, &[]),
    );
    let full_info = info(0x0200_4300, "Full master topic");
    let light_info = info(0x0200_4301, "Light master topic");
    let full_group = group(7, 0x841, &full_info);
    let light_group = group(7, 0x0100_0801, &light_info);
    let header = fixture.add(
        "Patch.esp",
        &["FullTopics.esm", "Light.esl"],
        0,
        &[full_group.clone(), light_group].concat(),
    );
    let result = fixture.read();
    assert_metadata(
        &metadata(&result.records[&0x0300_4300]),
        Some(0x0200_0841),
        Some(0x841),
        Some(header),
        header + 24,
    );
    assert_metadata(
        &metadata(&result.records[&0x0300_4301]),
        Some(0xfe00_0801),
        Some(0x0100_0801),
        Some(header + full_group.len()),
        header + full_group.len() + 24,
    );
    assert_eq!(result.diagnostics["patch.esp"].invalid_links, 0);
}

/// A catalogue-dependent alias inventory replay preserves the original record offset.
#[test]
fn deferred_inventory_replay_keeps_source_record_provenance() {
    let mut fixture = Fixture::new();
    let mut inventory = 0x841u32.to_le_bytes().to_vec();
    inventory.extend((-17i32).to_le_bytes());
    let mut extra = 0x842u32.to_le_bytes().to_vec();
    extra.extend(0x843u32.to_le_bytes());
    extra.extend(0.625f32.to_le_bytes());
    let quest = native::named(
        b"QUST",
        0x4400,
        &[
            native::subrecord(b"ALST", &7u32.to_le_bytes()),
            native::subrecord(b"ALID", &native::text("Deferred inventory")),
            native::subrecord(b"FNAM", &[2, 0]),
            native::subrecord(b"COCT", &1u32.to_le_bytes()),
            native::subrecord(b"CNTO", &inventory),
            native::subrecord(b"COED", &extra),
            native::subrecord(b"ALED", &[]),
        ],
    );
    let before = native::named(b"DIAL", 0x840, &[]);
    let records = [
        before.clone(),
        quest,
        native::named(b"MISC", 0x841, &[]),
        native::named(b"NPC_", 0x842, &[]),
        native::named(b"GLOB", 0x843, &[]),
    ]
    .concat();
    let header = fixture.add("Deferred.esm", &[], 1, &records);
    let result = fixture.read();
    let quest = &result.records[&0x0100_4400];
    assert_metadata(&metadata(quest), None, None, None, header + before.len());
    let extra = quest
        .fields
        .iter()
        .find(|field| field.signature == *b"COED")
        .unwrap();
    let Value::Struct(members) = &extra.value else {
        panic!("resolved inventory struct")
    };
    assert!(
        members
            .iter()
            .any(|(_, value)| value == &Value::FormId(0x0100_0842))
    );
    assert!(
        !members
            .iter()
            .any(|(_, value)| matches!(value, Value::Deferred(_)))
    );
    assert_eq!(
        &extra.canonical_bytes[..8],
        &[0x42, 0x08, 0, 1, 0x43, 0x08, 0, 1]
    );
    assert_eq!(result.diagnostics["deferred.esm"].invalid_links, 0);
}

/// Prepare linked overrides, a deletion, bad group owners, and safely framed field damage.
fn damaged_plugins(generated: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let topic = native::named(b"DIAL", 0x4500, &[]);
    let deleted_topic = native::named(b"DIAL", 0x4501, &[]);
    let wrong_kind = native::named(b"NPC_", 0x4502, &[]);
    let original = group(7, 0x4500, &info(0x4600, "Original usable INFO"));
    let base = native::plugin(
        generated,
        &[],
        1,
        &[topic, deleted_topic, wrong_kind, original].concat(),
    );
    let mut payload = group(
        7,
        0x4501,
        &info(0x4600, "Winning INFO whose topic is later deleted"),
    );
    for (id, owner, text) in [
        (0x0100_4601, 0x4502, "Wrong-kind topic owner"),
        (0x0100_4602, 0x4503, "Missing topic owner"),
        (0x0100_4603, 0x0200_4500, "Bad-master topic owner"),
        (0x0100_4604, 0, "Null topic owner"),
    ] {
        payload.extend(group(7, owner, &info(id, text)));
    }
    let damaged = native::named(
        b"INFO",
        0x0100_4605,
        &[
            native::subrecord(b"TRDT", &[0xa5; 23]),
            native::subrecord(b"NAM1", &native::text("Safe field beside damaged response")),
        ],
    );
    payload.extend(group(7, 0x4500, &damaged));
    payload.extend(group(7, 0x4500, &info(0x0100_4606, "Usable neighbor")));
    let patch = native::plugin(generated, &["Topics.esm"], 0, &payload);
    let deletion = native::plugin(
        generated,
        &["Topics.esm"],
        0,
        &native::record(b"DIAL", 0x4501, 0x20, &[]),
    );
    (base, patch, deletion)
}

/// Absent/deleted/wrong-kind owners null only metadata, keeping winning INFO and neighbors.
#[test]
fn invalid_topic_owners_are_bounded_and_keep_override_source_provenance() {
    let mut fixture = Fixture::new();
    let (base, patch, deletion) = damaged_plugins(&fixture.generated);
    for (name, bytes) in [
        ("Topics.esm", base),
        ("Patch.esp", patch.clone()),
        ("Delete.esp", deletion),
    ] {
        let path = fixture.directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        fixture.paths.push(path);
    }
    let result = fixture.read();
    assert!(!result.records.contains_key(&0x0100_4501));
    let header = u32::from_le_bytes(patch[4..8].try_into().unwrap()) as usize + 24;
    let winner = &result.records[&0x0100_4600];
    assert_eq!(winner.load_order, 2);
    assert_metadata(
        &metadata(winner),
        None,
        Some(0x4501),
        Some(header),
        header + 24,
    );
    assert_eq!(result.overrides[&0x0100_4600].len(), 2);
    for id in [0x0200_4601, 0x0200_4602, 0x0200_4603, 0x0200_4604] {
        let record = metadata(&result.records[&id]);
        assert_eq!(record["topic_form_id"], Json::Null);
        assert!(record["source_topic_form_id"].is_number());
        assert!(record["topic_group_offset"].is_number());
    }
    for id in [0x0200_4605, 0x0200_4606] {
        assert_eq!(
            metadata(&result.records[&id])["topic_form_id"],
            json!(0x0100_4500u32)
        );
    }
    let damaged = &result.records[&0x0200_4605];
    assert_eq!(damaged.rejected_fields, [*b"TRDT"]);
    assert!(
        damaged
            .fields
            .iter()
            .any(|field| field.signature == *b"NAM1")
    );
    let diagnostics = &result.diagnostics["patch.esp"];
    assert_eq!(diagnostics.skipped_records, 0);
    assert_eq!(diagnostics.skipped_fields, 1);
    assert_eq!(diagnostics.invalid_links, 5);
    assert_eq!(diagnostics.first_by_category.len(), 2);
}

/// Actual diagnostic and pipeline publication retain grouped metadata and the rkyv wire.
#[tokio::test]
async fn grouped_info_metadata_exports_and_broken_owners_still_publish_neighbors() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let generated = fs::read(data.join("Skyrim.esm")).unwrap();
    let (base, patch, deletion) = damaged_plugins(&generated);
    for (name, bytes) in [
        ("Topics.esm", base),
        ("Patch.esp", patch),
        ("Delete.esp", deletion),
    ] {
        fs::write(data.join(name), bytes).unwrap();
    }
    let plugins = temp.path().join("plugins.txt");
    fs::write(
        &plugins,
        "*Skyrim.esm\n*Topics.esm\n*Patch.esp\n*Delete.esp\n",
    )
    .unwrap();
    let paths = ["Skyrim.esm", "Topics.esm", "Patch.esp", "Delete.esp"].map(|name| data.join(name));
    let decoded = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    let bundle = temp.path().join("bundle");
    export_record_bundle_typed(&data, &plugins, &bundle, &[*b"INFO"]).unwrap();
    let rows: HashMap<u32, Json> = fs::read_to_string(bundle.join("typed-records.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            let row: Json = serde_json::from_str(line).unwrap();
            (
                u32::try_from(row["form_id"].as_u64().unwrap()).unwrap(),
                row,
            )
        })
        .collect();
    for (&id, row) in &rows {
        let native = metadata(&decoded.records[&id]);
        for key in [
            "topic_form_id",
            "source_topic_form_id",
            "topic_group_offset",
            "source_record_offset",
        ] {
            assert!(row.get(key).is_some(), "missing typed export {key}");
            assert_eq!(row.get(key), native.get(key), "{id:08X} {key}");
        }
    }
    assert_eq!(rows.len(), 7);
    let output = temp.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(plugins);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [
        0x0100_4600u32,
        0x0200_4601,
        0x0200_4602,
        0x0200_4603,
        0x0200_4604,
        0x0200_4605,
        0x0200_4606,
    ] {
        let bytes: Vec<u8> = database
            .query_row("SELECT data FROM records WHERE form_id=?", [id], |row| {
                row.get(0)
            })
            .unwrap();
        let record = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes).unwrap();
        assert!(
            !record.subrecords.iter().any(|field| field.tag == *b"TPIC"),
            "metadata must not synthesize a runtime subrecord"
        );
        assert!(record.subrecords.iter().any(|field| field.tag == *b"NAM1"));
        if id == 0x0200_4605 {
            assert!(!record.subrecords.iter().any(|field| field.tag == *b"TRDT"));
        }
    }
    let source: (u32, Vec<u8>) = database
        .query_row(
            "SELECT load_order,payload FROM inhouse_source_records WHERE form_id=0x01004600",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(source.0, 2);
    assert_eq!(source.1, decoded.records[&0x0100_4600].raw_payload);
    let diagnostics: Json =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    let patch = &diagnostics["decoder"]["patch.esp"];
    assert_eq!(patch["skipped_records"], 0);
    assert_eq!(patch["skipped_fields"], 1);
    assert_eq!(patch["invalid_links"], 5);
}
