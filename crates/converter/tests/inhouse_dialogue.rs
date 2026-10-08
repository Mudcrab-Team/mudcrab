//! Native quests/dialogue, repeated contextual roles, and publication recovery.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::{load_order::LoadOrder, types::ArchivedRecordData},
    records::{DecodedField, DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, inhouse_dialogue as native, layout};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Existing generated world plus independent native plugins with chosen master orders.
struct Fixture {
    directory: tempfile::TempDir,
    generated: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Keep the generated world in slot zero, leaving divergent synthetic slots.
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

    /// Give each native plugin an independently specified MAST list.
    fn add(&mut self, name: &str, masters: &[&str], flags: u32, records: &[u8]) {
        let path = self.directory.path().join(name);
        fs::write(
            &path,
            native::plugin(&self.generated, masters, flags, records),
        )
        .unwrap();
        self.paths.push(path);
    }

    /// Exercise the public reader and its accepted-winner target validation.
    fn read(&self) -> ReadResult {
        read_plugins(&self.paths, &LoadOrder::read(&self.paths).unwrap()).unwrap()
    }
}

/// Locate a semantic leaf while preserving repeated physical occurrences.
fn named<'a>(record: &'a DecodedRecord, name: &str) -> &'a DecodedField {
    record
        .fields
        .iter()
        .find(|field| field.name == name)
        .unwrap_or_else(|| {
            panic!(
                "missing {name} in {:08X}: {:?}",
                record.form_id, record.rejected_fields
            )
        })
}

/// Collect repetitions by role, rather than treating a signature as a last-value map.
fn values<'a>(record: &'a DecodedRecord, name: &str) -> Vec<&'a Value> {
    record
        .fields
        .iter()
        .filter(|field| field.name == name)
        .map(|field| &field.value)
        .collect()
}

/// Read a named member from an independently verified struct layout.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}")
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// All eleven families preserve native widths, repeated values, and distinctive padding.
#[test]
fn all_dialogue_families_decode_complete_native_representatives() {
    let mut fixture = Fixture::new();
    fixture.add("Dialogue.esm", &[], 1, &native::representatives());
    let result = fixture.read();
    for (index, signature) in native::SIGNATURES.iter().enumerate() {
        let record = &result.records[&(0x0100_0000 | (native::FIRST_ID + index as u32))];
        assert_eq!(&record.record_type, signature);
        assert!(record.supported);
        assert!(record.payload_complete);
        assert!(
            record.rejected_fields.is_empty(),
            "{signature:?}: {:?}",
            record.rejected_fields
        );
        assert_eq!(record.version_control, 0x1739_5b7d);
        assert_eq!(record.header_unknown, 0xa571);
    }
    let topic = &result.records[&0x0100_3401];
    assert_eq!(named(topic, "priority").value, Value::Float(37.625));
    assert_eq!(
        member(&named(topic, "topic_data").value, "subtype"),
        &Value::Unsigned(102)
    );
    let response = &result.records[&0x0100_3402];
    assert_eq!(
        member(
            &named(response, "response_flags").value,
            "reset_hours_encoded"
        ),
        &Value::Unsigned(0x39b7)
    );
    assert_eq!(
        values(response, "response_text"),
        [
            &Value::String("First response".into()),
            &Value::String("Second response".into())
        ]
    );
    assert_eq!(
        values(response, "response")
            .iter()
            .map(|v| member(v, "emotion_value"))
            .collect::<Vec<_>>(),
        [&Value::Unsigned(17), &Value::Unsigned(73)]
    );
    assert_eq!(
        &named(response, "response").canonical_bytes[8..16],
        &[0xa1, 0xb2, 0xc3, 0xd4, 3, 0x29, 0x71, 0xa5]
    );
    let story = &result.records[&0x0100_3407];
    assert_eq!(
        values(story, "reset_hours"),
        [&Value::Float(3.75), &Value::Float(19.125)]
    );
    let message = &result.records[&0x0100_3409];
    assert_eq!(
        values(message, "button_text"),
        [
            &Value::String("First button".into()),
            &Value::String("Second button".into())
        ]
    );
    let diagnostics = &result.diagnostics["dialogue.esm"];
    assert_eq!(diagnostics.skipped_fields, 0);
    assert_eq!(diagnostics.unexpected_subrecords, 0);
}

/// Nested repeat boundaries preserve both alias kinds and the two distinct QSTA layouts.
#[test]
fn quest_stages_logs_objectives_aliases_and_targets_keep_context() {
    let mut fixture = Fixture::new();
    fixture.add("Quest.esm", &[], 1, &native::quest(0x3500));
    let result = fixture.read();
    let quest = &result.records[&0x0100_3500];
    assert!(
        quest.rejected_fields.is_empty(),
        "{:?}",
        quest.rejected_fields
    );
    assert_eq!(
        values(quest, "stage")
            .iter()
            .map(|v| member(v, "index"))
            .collect::<Vec<_>>(),
        [&Value::Unsigned(17), &Value::Unsigned(93)]
    );
    assert_eq!(
        values(quest, "log_text"),
        [
            &Value::String("First log".into()),
            &Value::String("Second log".into()),
            &Value::String("Third log".into()),
            &Value::String("Fourth log".into())
        ]
    );
    assert_eq!(
        values(quest, "objective_index"),
        [&Value::Unsigned(31), &Value::Unsigned(79)]
    );
    assert_eq!(
        values(quest, "objective_target")
            .iter()
            .map(|v| member(v, "alias"))
            .collect::<Vec<_>>(),
        [
            &Value::Signed(-7),
            &Value::Signed(29),
            &Value::Signed(-7),
            &Value::Signed(29)
        ]
    );
    assert_eq!(
        values(quest, "reference_alias_id"),
        [&Value::Unsigned(7), &Value::Unsigned(43)]
    );
    assert_eq!(values(quest, "location_alias_id"), [&Value::Unsigned(19)]);
    let alias_flags = values(quest, "alias_flags");
    let Value::Struct(first) = alias_flags[0] else {
        panic!("flags struct")
    };
    assert_eq!(first.len(), 1);
    assert_eq!(
        member(alias_flags[1], "additional_flags"),
        &Value::Unsigned(3)
    );
    assert_eq!(
        member(&named(quest, "quest_target").value, "reference"),
        &Value::FormId(0)
    );
    assert_eq!(
        named(quest, "quest_description").value,
        Value::String("After aliases".into())
    );
}

/// Timer SNAM and action-start SNAM retain their different roles across every action type.
#[test]
fn repeated_scene_phases_actors_and_action_variants_keep_native_roles() {
    let mut fixture = Fixture::new();
    fixture.add("Scenes.esm", &[], 1, &native::scene(0x3600, 0, 0, 0));
    let result = fixture.read();
    let scene = &result.records[&0x0100_3600];
    assert!(
        scene.rejected_fields.is_empty(),
        "{:?}",
        scene.rejected_fields
    );
    assert_eq!(
        values(scene, "phase_editor_width"),
        [&Value::Unsigned(179), &Value::Unsigned(263)]
    );
    assert_eq!(
        values(scene, "actor_alias"),
        [&Value::Unsigned(7), &Value::Unsigned(29)]
    );
    assert_eq!(
        values(scene, "action_type"),
        [
            &Value::Unsigned(2),
            &Value::Unsigned(0),
            &Value::Unsigned(1),
            &Value::Unsigned(2)
        ]
    );
    assert_eq!(
        values(scene, "action_start_phase"),
        [
            &Value::Unsigned(31),
            &Value::Unsigned(32),
            &Value::Unsigned(33),
            &Value::Unsigned(34)
        ]
    );
    assert_eq!(
        values(scene, "timer_duration"),
        [&Value::Float(2.375), &Value::Float(5.375)]
    );
    assert_eq!(
        values(scene, "action_package"),
        [&Value::FormId(0), &Value::FormId(0)]
    );
    assert_eq!(named(scene, "loop_max").value, Value::Float(3.75));
    assert_eq!(named(scene, "loop_min").value, Value::Float(0.625));
    assert_eq!(
        member(&named(scene, "actor_behavior_settings").value, "death"),
        &Value::Unsigned(2)
    );
}

/// An empty action-end closes its repetition before native root PNAM and INAM.
#[tokio::test]
async fn scene_action_end_keeps_root_quest_and_index_through_publication() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    generate(&data);
    let plugin_path = data.join("Skyrim.esm");
    let mut bytes = fs::read(&plugin_path).unwrap();
    bytes.extend(native::quest(0x3b00));
    bytes.extend(native::named(
        b"DIAL",
        0x3b01,
        &[native::subrecord(b"DATA", &[1, 2, 14, 0])],
    ));
    bytes.extend(native::named(b"PACK", 0x3b02, &[]));
    let before = native::named(
        b"MESG",
        0x3b04,
        &[native::subrecord(b"DNAM", &1u32.to_le_bytes())],
    );
    let after = native::named(
        b"MESG",
        0x3b05,
        &[native::subrecord(b"DNAM", &3u32.to_le_bytes())],
    );
    bytes.extend(&before);
    // Official SCEN records can omit the legacy scene script NEXT marker.
    bytes.extend(native::scene_with_root(
        0x3b03, 0x3b00, 0x3b01, 0x3b02, 83, false,
    ));
    bytes.extend(&after);
    fs::write(&plugin_path, bytes).unwrap();
    let paths = vec![plugin_path];
    let result = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    let scene = &result.records[&0x3b03];
    let root_quest = scene
        .fields
        .iter()
        .rev()
        .find(|field| field.signature == *b"PNAM")
        .unwrap();
    assert_eq!(
        root_quest.name, "quest",
        "root PNAM after the empty action-end must retain its quest role"
    );
    assert_eq!(root_quest.value, Value::FormId(0x3b00));
    assert_eq!(root_quest.canonical_bytes, 0x3b00u32.to_le_bytes());
    let root_index = scene
        .fields
        .iter()
        .rev()
        .find(|field| field.signature == *b"INAM")
        .unwrap();
    assert_eq!(root_index.name, "last_action_index");
    assert_eq!(root_index.value, Value::Unsigned(83));
    assert_eq!(
        values(scene, "action_package"),
        [&Value::FormId(0x3b02), &Value::FormId(0x3b02)]
    );
    assert_eq!(
        values(scene, "action_index"),
        [
            &Value::Unsigned(17),
            &Value::Unsigned(18),
            &Value::Unsigned(19),
            &Value::Unsigned(20)
        ]
    );
    assert_eq!(result.diagnostics["skyrim.esm"].invalid_links, 0);
    assert!(scene.rejected_fields.is_empty());
    for id in [0x3b04, 0x3b05] {
        assert!(result.records.contains_key(&id));
    }

    let plugins = temp.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n").unwrap();
    let output = temp.path().join("pack");
    let report = convert(&data, &output, plugins).await;
    assert!(report.complete, "{:?}", report.warnings);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    let published_scene = published(&database, 0x3b03);
    let pnam: Vec<_> = published_scene
        .subrecords
        .iter()
        .filter(|field| field.tag == *b"PNAM")
        .map(|field| u32::from_le_bytes(field.data.as_slice().try_into().unwrap()))
        .collect();
    assert_eq!(pnam, [0x3b02, 0x3b02, 0x3b00]);
    let inam: Vec<_> = published_scene
        .subrecords
        .iter()
        .filter(|field| field.tag == *b"INAM")
        .map(|field| u32::from_le_bytes(field.data.as_slice().try_into().unwrap()))
        .collect();
    assert_eq!(inam, [17, 18, 19, 20, 83]);
    for id in [0x3b04, 0x3b05] {
        let count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "valid scene neighbor{id:08X}");
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["skyrim.esm"]["invalid_links"], 0);
}

/// Location coordinates, variable cell lists and padding remain independent of link mapping.
#[test]
fn location_arrays_keep_y_x_order_and_repeat_each_world_entry() {
    let mut fixture = Fixture::new();
    fixture.add("Locations.esm", &[], 1, &native::location(0x3700, 0, 0));
    let result = fixture.read();
    let location = &result.records[&0x0100_3700];
    assert!(location.rejected_fields.is_empty());
    let Value::Array(persistent) = &named(location, "added_persistent").value else {
        panic!("persistent array")
    };
    assert_eq!(member(&persistent[0], "grid_y"), &Value::Signed(-13));
    assert_eq!(member(&persistent[0], "grid_x"), &Value::Signed(37));
    let worlds = values(location, "added_world_cells");
    assert_eq!(worlds.len(), 2);
    for (world, y) in worlds.into_iter().zip([-17i64, 43]) {
        let Value::Array(cells) = member(world, "cells") else {
            panic!("cells array")
        };
        assert_eq!(cells.len(), 2);
        assert_eq!(member(&cells[0], "grid_y"), &Value::Signed(y));
        assert_eq!(member(&cells[1], "grid_x"), &Value::Signed(-71));
    }
    let Value::Array(enable) = &named(location, "added_enable_points").value else {
        panic!("enable array")
    };
    assert_eq!(member(&enable[0], "flags"), &Value::Unsigned(3));
    assert_eq!(
        member(&enable[0], "unused"),
        &Value::Bytes(vec![0xa5, 0x71, 0x29])
    );
    assert_eq!(
        member(&named(location, "color").value, "alpha"),
        &Value::Unsigned(131)
    );
}

/// Full and light master slots differ from file-relative slots for aliases and INFO links.
#[test]
fn quest_aliases_and_info_order_links_remap_divergent_full_light_masters() {
    let mut fixture = Fixture::new();
    fixture.add("Light.esl", &[], 0x201, &native::named(b"INFO", 0x801, &[]));
    fixture.add("Distractor.esm", &[], 1, &[]);
    fixture.add(
        "Targets.esm",
        &[],
        1,
        &[
            native::named(b"NPC_", 0x880, &[]),
            native::named(b"DIAL", 0x881, &[]),
            native::named(b"INFO", 0x882, &[]),
        ]
        .concat(),
    );
    let alias = native::named(
        b"QUST",
        0x0200_3800,
        &[
            native::subrecord(b"ALST", &7u32.to_le_bytes()),
            native::subrecord(b"ALID", &native::text("Linked alias")),
            native::subrecord(b"FNAM", &[2, 0, 1, 0]),
            native::subrecord(b"ALFR", &0x0100_0801u32.to_le_bytes()),
            native::subrecord(b"ALUA", &0x0000_0880u32.to_le_bytes()),
            native::subrecord(b"ALED", &[]),
        ],
    );
    fixture.add(
        "Patch.esp",
        &["Targets.esm", "Light.esl"],
        0,
        &[
            alias,
            native::response(0x0200_3801, 0x881, 0x0100_0801),
            native::response(0x0200_3802, 0x881, 0x882),
        ]
        .concat(),
    );
    let result = fixture.read();
    let quest = &result.records[&0x0300_3800];
    assert_eq!(
        named(quest, "forced_reference").value,
        Value::FormId(0xfe00_0801)
    );
    assert_eq!(
        named(quest, "unique_actor").value,
        Value::FormId(0x0200_0880)
    );
    let response = &result.records[&0x0300_3801];
    assert_eq!(named(response, "topic").value, Value::FormId(0x0200_0881));
    assert_eq!(
        named(response, "previous_info").value,
        Value::FormId(0xfe00_0801)
    );
    assert_eq!(
        named(&result.records[&0x0300_3802], "previous_info").value,
        Value::FormId(0x0200_0882)
    );
    assert_eq!(result.diagnostics["patch.esp"].invalid_links, 0);
}

/// Generate a complete ordinary dummy asset set for real pipeline publication.
fn generate(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
}

/// Run the public pipeline while draining progress events to completion.
async fn convert(data: &Path, output: &Path, plugins: PathBuf) -> converter::PipelineReport {
    let mut config = PipelineConfig::new(data, output);
    config.plugins_file = Some(plugins);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    report
}

/// Decode the actual published canonical record bytes using the shared rkyv contract.
fn published(database: &Connection, id: u32) -> ArchivedRecordData {
    let bytes: Vec<u8> = database
        .query_row("SELECT data FROM records WHERE form_id=?", [id], |row| {
            row.get(0)
        })
        .unwrap();
    rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes).unwrap()
}

/// Every family keeps its prior winner and good neighbors after bad compression and fields.
#[tokio::test]
async fn broken_dialogue_mod_publishes_all_family_neighbors_and_omits_unsafe_fields() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    generate(&data);
    let base_path = data.join("Skyrim.esm");
    let generated = fs::read(&base_path).unwrap();
    let mut base = generated.clone();
    base.extend(native::representatives());
    fs::write(&base_path, base).unwrap();
    let paths = vec![base_path.clone()];
    let earlier = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    let mut patch = Vec::new();
    let mut neighbors = Vec::new();
    for (index, signature) in native::SIGNATURES.iter().enumerate() {
        let before = 0x0100_3900 + index as u32 * 2;
        let after = before + 1;
        neighbors.extend([before, after]);
        patch.extend(native::named(
            b"MESG",
            before,
            &[native::subrecord(b"DNAM", &3u32.to_le_bytes())],
        ));
        patch.extend(native::record(
            signature,
            native::FIRST_ID + index as u32,
            0x40000,
            &[24, 0, 0, 0, 99],
        ));
        patch.extend(native::named(
            b"MESG",
            after,
            &[native::subrecord(b"DNAM", &1u32.to_le_bytes())],
        ));
    }
    let unsafe_id = 0x0100_3980;
    patch.extend(native::named(
        b"INFO",
        unsafe_id,
        &[
            native::subrecord(b"TRDT", &[0xa5; 23]),
            native::subrecord(b"NAM1", &native::text("Safe response beside damaged data")),
            native::subrecord(b"ANAM", &0x0200_0880u32.to_le_bytes()),
        ],
    ));
    let patch_path = data.join("DamagedDialogue.esp");
    fs::write(
        &patch_path,
        native::plugin(&generated, &["Skyrim.esm"], 0, &patch),
    )
    .unwrap();
    let plugins = temp.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n*DamagedDialogue.esp\n").unwrap();
    let output = temp.path().join("pack");
    let report = convert(&data, &output, plugins).await;
    assert!(report.complete, "{:?}", report.warnings);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for (index, _) in native::SIGNATURES.iter().enumerate() {
        let id = native::FIRST_ID + index as u32;
        let (load_order, payload): (i64, Vec<u8>) = database
            .query_row(
                "SELECT load_order,payload FROM inhouse_source_records WHERE form_id=?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(load_order, 0);
        assert_eq!(payload, earlier.records[&id].raw_payload);
    }
    for id in neighbors {
        let count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "missing neighbor{id:08X}");
    }
    let safe = published(&database, unsafe_id);
    assert!(!safe.subrecords.iter().any(|field| field.tag == *b"TRDT"));
    assert_eq!(
        safe.subrecords
            .iter()
            .find(|field| field.tag == *b"NAM1")
            .unwrap()
            .data,
        native::text("Safe response beside damaged data")
    );
    assert_eq!(
        safe.subrecords
            .iter()
            .find(|field| field.tag == *b"ANAM")
            .unwrap()
            .data,
        [0, 0, 0, 0]
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    let damaged = &diagnostics["decoder"]["damageddialogue.esp"];
    assert_eq!(damaged["skipped_records"], 11);
    assert_eq!(damaged["skipped_fields"], 1);
    assert_eq!(damaged["invalid_links"], 1);
    assert!(
        damaged["first_by_category"]["record"]
            .as_str()
            .unwrap()
            .contains("QUST")
    );
    assert!(
        damaged["first_by_category"]["field"]
            .as_str()
            .unwrap()
            .contains("TRDT")
    );
}

/// Build an independent one-entry STRINGS or length-prefixed bank with the same key.
fn string_bank(value: &str, length_prefixed: bool) -> Vec<u8> {
    let text = native::text(value);
    let mut payload = Vec::new();
    if length_prefixed {
        payload.extend((text.len() as u32).to_le_bytes());
    }
    payload.extend(text);
    [
        1u32.to_le_bytes().to_vec(),
        (payload.len() as u32).to_le_bytes().to_vec(),
        17u32.to_le_bytes().to_vec(),
        0u32.to_le_bytes().to_vec(),
        payload,
    ]
    .concat()
}

/// Published quest logs, INFO responses/prompts, and message buttons select their own banks.
#[tokio::test]
async fn localized_dialogue_fields_publish_from_three_distinct_string_banks() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    generate(&data);
    let generated = fs::read(data.join("Skyrim.esm")).unwrap();
    let key = 17u32.to_le_bytes();
    let records = [
        native::named(
            b"QUST",
            0x0100_3a00,
            &[
                native::subrecord(b"FULL", &key),
                native::subrecord(b"INDX", &[17, 0, 2, 0]),
                native::subrecord(b"QSDT", &[1]),
                native::subrecord(b"CNAM", &key),
                native::subrecord(b"QOBJ", &31u16.to_le_bytes()),
                native::subrecord(b"NNAM", &key),
            ],
        ),
        native::named(
            b"INFO",
            0x0100_3a01,
            &[
                native::subrecord(b"TRDT", &[0; 24]),
                native::subrecord(b"NAM1", &key),
                native::subrecord(b"RNAM", &key),
            ],
        ),
        native::named(
            b"MESG",
            0x0100_3a02,
            &[
                native::subrecord(b"DESC", &key),
                native::subrecord(b"FULL", &key),
                native::subrecord(b"DNAM", &1u32.to_le_bytes()),
                native::subrecord(b"ITXT", &key),
            ],
        ),
    ]
    .concat();
    fs::write(
        data.join("Localized.esp"),
        native::plugin(&generated, &["Skyrim.esm"], 0x80, &records),
    )
    .unwrap();
    let strings = data.join("Strings");
    fs::create_dir_all(&strings).unwrap();
    for (suffix, value, prefixed) in [
        ("STRINGS", "Ordinary display text", false),
        ("DLSTRINGS", "Quest log and message description", true),
        ("ILSTRINGS", "Spoken response text", true),
    ] {
        fs::write(
            strings.join(format!("Localized_English.{suffix}")),
            string_bank(value, prefixed),
        )
        .unwrap();
    }
    let plugins = temp.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n*Localized.esp\n").unwrap();
    let output = temp.path().join("pack");
    let report = convert(&data, &output, plugins).await;
    assert!(report.complete, "{:?}", report.warnings);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for (id, tag, expected) in [
        (0x0100_3a00, b"FULL", "Ordinary display text"),
        (0x0100_3a00, b"CNAM", "Quest log and message description"),
        (0x0100_3a00, b"NNAM", "Ordinary display text"),
        (0x0100_3a01, b"NAM1", "Spoken response text"),
        (0x0100_3a01, b"RNAM", "Ordinary display text"),
        (0x0100_3a02, b"DESC", "Quest log and message description"),
        (0x0100_3a02, b"ITXT", "Ordinary display text"),
    ] {
        let record = published(&database, id);
        assert_eq!(
            record
                .subrecords
                .iter()
                .find(|field| &field.tag == tag)
                .unwrap()
                .data,
            native::text(expected),
            "{id:08X} {tag:?}"
        );
    }
}
