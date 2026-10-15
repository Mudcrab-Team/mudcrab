//! R3 native values stay independent of reference-tool integer and ratio views.
mod common;

use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{DecodedField, DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, layout};
use rusqlite::Connection;
use std::{fs, path::Path};

/// Frame a synthetic subrecord without using the schema's type declarations.
fn subrecord(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend(u16::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Frame a synthetic SSE record with a safely established outer boundary.
fn record(tag: &[u8; 4], id: u32, fields: &[Vec<u8>]) -> Vec<u8> {
    let payload = fields.concat();
    let mut bytes = tag.to_vec();
    bytes.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(44u16.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(payload);
    bytes
}

/// Use the real dummy world header so the same fixture can reach publication.
fn generated_plugin() -> Vec<u8> {
    esm::plugin(&esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap()
}

/// Read author-written supplemental bytes through the public scanner and decoder.
fn read_records(records: &[u8]) -> ReadResult {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Skyrim.esm");
    let mut bytes = generated_plugin();
    bytes.extend(records);
    fs::write(&path, bytes).unwrap();
    let paths = vec![path];
    read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap()
}

/// Select the physical field while retaining its native canonical bytes.
fn field<'a>(record: &'a DecodedRecord, tag: &[u8; 4]) -> &'a DecodedField {
    record
        .fields
        .iter()
        .find(|field| &field.signature == tag)
        .unwrap()
}

/// Select a named struct member without converting its native scalar type.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}")
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// Global short markers never cast FLTV; leveled percentages remain byte values.
#[test]
fn global_and_leveled_values_preserve_native_storage() {
    let mut records = Vec::new();
    for (id, bits) in [(0x5800, 40960.0f32.to_bits()), (0x5801, 0x3e99_999a)] {
        records.extend(record(
            b"GLOB",
            id,
            &[
                subrecord(b"FNAM", b"s"),
                subrecord(b"FLTV", &bits.to_le_bytes()),
            ],
        ));
    }
    for (id, tag, percent) in [
        (0x5803, b"LVLI", 25),
        (0x5804, b"LVLN", 37),
        (0x5805, b"LVSP", 73),
    ] {
        records.extend(record(tag, id, &[subrecord(b"LVLD", &[percent])]));
    }
    let result = read_records(&records);
    for (id, bits) in [(0x5800, 40960.0f32.to_bits()), (0x5801, 0x3e99_999a)] {
        let global = &result.records[&id];
        assert_eq!(field(global, b"FNAM").value, Value::Unsigned(115));
        assert_eq!(
            field(global, b"FLTV").value,
            Value::Float(f32::from_bits(bits))
        );
        assert_eq!(field(global, b"FLTV").canonical_bytes, bits.to_le_bytes());
    }
    for (id, percent) in [(0x5803, 25), (0x5804, 37), (0x5805, 73)] {
        let chance = field(&result.records[&id], b"LVLD");
        assert_eq!(chance.value, Value::Unsigned(percent));
        assert_eq!(chance.canonical_bytes, [u8::try_from(percent).unwrap()]);
    }
}

/// Native NPC selector/face widths and race enum/flag widths survive presentation differences.
#[test]
fn actor_flags_signed_members_and_padding_preserve_native_roles() {
    let mut configuration = vec![0; 24];
    configuration[..4].copy_from_slice(&0x891u32.to_le_bytes());
    configuration[4..6].copy_from_slice(&(-27i16).to_le_bytes());
    configuration[8..10].copy_from_slice(&1237i16.to_le_bytes());
    configuration[14..16].copy_from_slice(&137u16.to_le_bytes());
    let face_parts = [
        13u32.to_le_bytes(),
        (-1i32).to_le_bytes(),
        29u32.to_le_bytes(),
        41u32.to_le_bytes(),
    ]
    .concat();
    let mut race_data = vec![0; 128];
    race_data[10] = 0xff;
    race_data[11] = 17;
    race_data[64..68].copy_from_slice(&2u32.to_le_bytes());
    race_data[68..72].copy_from_slice(&(-1i32).to_le_bytes());
    let biped = [3u32.to_le_bytes(), [4, 0x48, 0x94, 0], 2u32.to_le_bytes()].concat();
    let records = [
        record(
            b"NPC_",
            0x5806,
            &[
                subrecord(b"ACBS", &configuration),
                subrecord(b"NAMA", &face_parts),
            ],
        ),
        record(
            b"RACE",
            0x5807,
            &[
                subrecord(b"BODT", &biped),
                subrecord(b"DATA", &race_data),
                subrecord(b"VNAM", &0xffff_e6ffu32.to_le_bytes()),
            ],
        ),
    ]
    .concat();
    let result = read_records(&records);
    let npc = &result.records[&0x5806];
    let acbs = field(npc, b"ACBS");
    assert_eq!(member(&acbs.value, "flags"), &Value::Unsigned(0x891));
    assert_eq!(member(&acbs.value, "magicka_offset"), &Value::Signed(-27));
    assert_eq!(
        member(&acbs.value, "level_multiplier_thousandths"),
        &Value::Signed(1237)
    );
    assert_eq!(
        member(&acbs.value, "speed_multiplier"),
        &Value::Unsigned(137)
    );
    assert_eq!(acbs.canonical_bytes, configuration);
    let face = field(npc, b"NAMA");
    assert_eq!(member(&face.value, "unknown"), &Value::Signed(-1));
    assert_eq!(member(&face.value, "nose"), &Value::Unsigned(13));
    assert_eq!(member(&face.value, "eyes"), &Value::Unsigned(29));
    assert_eq!(member(&face.value, "mouth"), &Value::Unsigned(41));
    assert_eq!(face.canonical_bytes, face_parts);
    let race = &result.records[&0x5807];
    let data = field(race, b"DATA");
    // The memory track stores native pairs compactly; their logical values are unchanged.
    let boosts = member(&data.value, "skill_boosts")
        .materialized_array()
        .expect("expected native skill pairs");
    assert_eq!(boosts.len(), 7);
    assert_eq!(member(&boosts[5], "skill"), &Value::Signed(-1));
    assert_eq!(member(&boosts[5], "boost"), &Value::Signed(17));
    assert_eq!(member(&data.value, "size"), &Value::Unsigned(2));
    assert_eq!(member(&data.value, "head_biped"), &Value::Signed(-1));
    assert_eq!(field(race, b"VNAM").value, Value::Unsigned(0xffff_e6ff));
    let biped = field(race, b"BODT");
    assert_eq!(member(&biped.value, "general_flags"), &Value::Unsigned(4));
    assert_eq!(
        member(&biped.value, "unused"),
        &Value::Bytes(vec![0x48, 0x94, 0])
    );
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_records, 0);
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 0);
}

/// Generate assets required for an ordinary pipeline publication without LOD.
fn generate_assets(data: &Path) {
    layout::prepare_directory(data, false).unwrap();
    layout::generate(
        data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
}

/// A framed bad mandatory FLTV drops its record while neighboring native globals publish.
#[tokio::test]
async fn malformed_global_value_preserves_neighbors_through_publication() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    generate_assets(&data);
    let plugin_path = data.join("Skyrim.esm");
    let mut plugin = fs::read(&plugin_path).unwrap();
    let mut expected_neighbors = Vec::new();
    for (id, payload) in [
        (0x5810, 40960.0f32.to_le_bytes().to_vec()),
        (0x5811, vec![0x9a, 0x99, 0x99]),
        (0x5812, 0x3e99_999au32.to_le_bytes().to_vec()),
    ] {
        let fields = [subrecord(b"FNAM", b"s"), subrecord(b"FLTV", &payload)];
        let bytes = record(b"GLOB", id, &fields);
        if id != 0x5811 {
            expected_neighbors.push((id, fields.concat()));
        }
        plugin.extend(bytes);
    }
    fs::write(&plugin_path, plugin).unwrap();
    let output = directory.path().join("pack");
    let plugins_file = directory.path().join("plugins.txt");
    fs::write(&plugins_file, "*Skyrim.esm\n").unwrap();
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(plugins_file);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    common::assert_reader_warning_publication(&report, &output);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for (id, expected_payload) in expected_neighbors {
        let (load_order, payload): (i64, Vec<u8>) = database
            .query_row(
                "SELECT source.load_order,source.payload FROM inhouse_source_records source JOIN records ON records.form_id=source.form_id WHERE source.form_id=?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(load_order, 0);
        assert_eq!(payload, expected_payload);
    }
    for table in ["records", "inhouse_source_records"] {
        let count: i64 = database
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE form_id=?"),
                [0x5811],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "bad native global escaped into {table}");
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["skyrim.esm"]["skipped_records"], 1);
    assert!(
        diagnostics["decoder"]["skyrim.esm"]["first_by_category"]["record"]
            .as_str()
            .unwrap()
            .contains("GLOB 00005811")
    );
}
