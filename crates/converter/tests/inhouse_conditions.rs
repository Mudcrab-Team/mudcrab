//! Shared condition native layouts and real publication recovery with asymmetric slots.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::{load_order::LoadOrder, types::ArchivedRecordData},
    records::{
        DecodedRecord, ReadResult, SCHEMA_BYTES, Value, deciders::conditions, read_plugins,
        schema_format::Schema,
    },
};
use dummy_content::{esm, inhouse_conditions as fixture, layout};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Native plugin fixture with a reusable TES4 payload and actual load-order resolution.
struct Plugins {
    directory: tempfile::TempDir,
    header: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Plugins {
    /// Frame an independent base using existing generated world infrastructure.
    fn new(extra: &[u8]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut bytes = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        let length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let header = bytes[24..24 + length].to_vec();
        bytes.extend(extra);
        let path = directory.path().join("Skyrim.esm");
        fs::write(&path, bytes).unwrap();
        Self {
            directory,
            header,
            paths: vec![path],
        }
    }

    /// Make file-local indices deliberately different from normal and light slots.
    fn add(&mut self, name: &str, flags: u32, masters: &[&str], records: &[u8]) {
        let path = self.directory.path().join(name);
        write_plugin(&path, &self.header, flags, masters, records);
        self.paths.push(path);
    }

    /// Exercise both decoder passes and final target validation through the public API.
    fn read(&self) -> ReadResult {
        read_plugins(&self.paths, &LoadOrder::read(&self.paths).unwrap()).unwrap()
    }
}

/// Write a native header and author-supplied record bytes without using schema metadata.
fn write_plugin(path: &Path, header: &[u8], flags: u32, masters: &[&str], records: &[u8]) {
    let mut payload = header.to_vec();
    for master in masters {
        let mut name = master.as_bytes().to_vec();
        name.push(0);
        payload.extend(fixture::subrecord(b"MAST", &name));
        payload.extend(fixture::subrecord(b"DATA", &[0x7A; 8]));
    }
    let mut bytes = fixture::record(b"TES4", 0, flags, &payload);
    bytes.extend(records);
    fs::write(path, bytes).unwrap();
}

/// Pick one native condition occurrence while preserving repeated physical order.
fn condition(record: &DecodedRecord, position: usize) -> &converter::records::DecodedField {
    record
        .fields
        .iter()
        .filter(|field| field.signature == *b"CTDA")
        .nth(position)
        .unwrap()
}

/// Pick an independently named struct member, including selected union values.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}")
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// The complete pinned registry has every declared enum index and both native categories.
#[test]
fn complete_function_registry_and_shared_native_schema_are_consistent() {
    let schema = Schema::parse(SCHEMA_BYTES).unwrap();
    let native = &schema.definitions["condition"];
    assert_eq!(native.size, Some(32));
    let indices = native
        .members
        .iter()
        .find(|member| member.name == "function_index")
        .unwrap()
        .enumeration
        .as_object()
        .unwrap();
    assert_eq!(indices.len(), 402);
    assert_eq!(conditions::FUNCTION_PARAMETERS.len(), 402);
    let first = native
        .members
        .iter()
        .find(|member| member.name == "parameter_1")
        .unwrap();
    let second = native
        .members
        .iter()
        .find(|member| member.name == "parameter_2")
        .unwrap();
    for (index, p1, p2) in conditions::FUNCTION_PARAMETERS {
        assert!(indices.contains_key(&index.to_string()), "{index}");
        assert!(
            first.fields.iter().any(|field| field.name == *p1),
            "{index}/{p1}"
        );
        assert!(
            second.fields.iter().any(|field| field.name == *p2),
            "{index}/{p2}"
        );
    }
    assert_eq!(
        conditions::function_parameters(59),
        Some(("quest", "quest_stage"))
    );
    assert_eq!(
        conditions::function_parameters(639),
        Some(("object_reference", "float"))
    );
    assert_eq!(
        conditions::function_parameters(576),
        Some(("event", "event_data"))
    );
    assert_eq!(conditions::function_parameters(4096), None);
}

/// Real full/light links remap, while padding, no-parameter bytes and unused mode slots survive.
#[test]
fn native_links_use_master_indices_and_keep_unequal_nonlink_bytes() {
    let mut plugins = Plugins::new(&fixture::global(0x2C01));
    plugins.add("Filler.esp", 0, &[], &fixture::global(0x2C30));
    plugins.add(
        "FirstLight.esl",
        0x200,
        &["Skyrim.esm"],
        &fixture::reference(0x0100_0AAA),
    );
    plugins.add(
        "SecondLight.esl",
        0x200,
        &["Skyrim.esm"],
        &fixture::reference(0x0100_0BBB),
    );
    let first = fixture::condition(
        0x54,
        0x0000_2C30,
        1,
        0x0100_0BBB,
        0xFF13_579B,
        2,
        0x0300_2C20,
    );
    let second = fixture::condition(
        0xA1,
        (-8.75f32).to_bits(),
        5,
        0xFE73_2951,
        0xFDFC_BA91,
        4,
        0xDE12_3456,
    );
    let fields = [
        fixture::subrecord(b"CTDA", &first),
        fixture::subrecord(b"CTDA", &second),
    ]
    .concat();
    plugins.add(
        "Conditions.esp",
        0,
        &["Filler.esp", "SecondLight.esl", "Skyrim.esm"],
        &[
            fixture::reference_with_base(0x0300_2C20, 0x0200_0014),
            fixture::spell(0x0300_2C40, &fields),
        ]
        .concat(),
    );
    let result = plugins.read();
    let record = &result.records[&0x0200_2C40];
    let first = condition(record, 0);
    assert_eq!(
        member(&first.value, "comparison"),
        &Value::FormId(0x0100_2C30)
    );
    assert_eq!(
        member(&first.value, "parameter_1"),
        &Value::FormId(0xFE00_1BBB)
    );
    assert_eq!(
        member(&first.value, "parameter_2"),
        &Value::Bytes(0xFF13_579Bu32.to_le_bytes().to_vec())
    );
    assert_eq!(
        member(&first.value, "reference"),
        &Value::FormId(0x0200_2C20)
    );
    assert_eq!(member(&first.value, "parameter_3"), &Value::Signed(-17));
    assert_eq!(&first.canonical_bytes[1..4], &[0x91, 0x37, 0xD5]);
    assert_eq!(&first.canonical_bytes[10..12], &[0xCA, 0x53]);
    let second = condition(record, 1);
    assert_eq!(member(&second.value, "comparison"), &Value::Float(-8.75));
    assert_eq!(
        member(&second.value, "parameter_1"),
        &Value::Bytes(0xFE73_2951u32.to_le_bytes().to_vec())
    );
    assert_eq!(
        member(&second.value, "reference"),
        &Value::Unsigned(0xDE12_3456)
    );
    assert_eq!(result.diagnostics["conditions.esp"].invalid_links, 0);
    assert_eq!(result.diagnostics["conditions.esp"].skipped_fields, 0);
}

/// Alias and package inputs are scalar indices, including when their bytes resemble links.
#[test]
fn alias_package_and_run_on_modes_do_not_remap_scalar_indices() {
    let cases = [
        (0x02, 1, 0xFFFF_FFFD, 5, 0xFFFF_FFF9, 23i32),
        (0x08, 161, 0x0100_2C71, 6, 0xFE23_4567, -5),
        (0x0A, 42, 0xFFFF_FFF5, 7, 0x3256_0002, 0x3146),
    ];
    let fields = cases
        .iter()
        .flat_map(|&(flags, function, first, run_on, reference, argument)| {
            let mut bytes = fixture::condition(
                flags,
                1.0f32.to_bits(),
                function,
                first,
                0xDE65_4321,
                run_on,
                reference,
            );
            bytes[28..32].copy_from_slice(&argument.to_le_bytes());
            fixture::subrecord(b"CTDA", &bytes)
        })
        .collect::<Vec<_>>();
    let result = Plugins::new(&fixture::spell(0x2C70, &fields)).read();
    let record = &result.records[&0x2C70];
    assert_eq!(
        member(&condition(record, 0).value, "parameter_1"),
        &Value::Signed(-3)
    );
    assert_eq!(
        member(&condition(record, 0).value, "reference"),
        &Value::Unsigned(0xFFFF_FFF9)
    );
    assert_eq!(
        member(&condition(record, 1).value, "parameter_1"),
        &Value::Unsigned(0x0100_2C71)
    );
    assert_eq!(
        member(&condition(record, 1).value, "reference"),
        &Value::Unsigned(0xFE23_4567)
    );
    assert_eq!(
        member(&condition(record, 2).value, "parameter_1"),
        &Value::Signed(-11)
    );
    assert_eq!(
        member(&condition(record, 2).value, "reference"),
        &Value::Unsigned(0x3256_0002)
    );
    for (position, (_, _, _, _, _, expected)) in cases.iter().enumerate() {
        assert_eq!(
            member(&condition(record, position).value, "parameter_3"),
            &Value::Signed(i64::from(*expected))
        );
        assert_eq!(
            &condition(record, position).canonical_bytes[28..32],
            &expected.to_le_bytes()
        );
    }
    assert_eq!(result.diagnostics["skyrim.esm"].invalid_links, 0);
}

/// Native event packing, VATS categories, floats and condition strings remain distinct.
#[test]
fn event_vats_float_and_cis_string_parameters_decode_in_physical_order() {
    let mut fields = fixture::subrecord(
        b"CTDA",
        &fixture::condition(0, 1.0f32.to_bits(), 576, 0x3146_0003, 0x2C80, 0, 0),
    );
    fields.extend(fixture::subrecord(b"CIS1", b"event parameter\0"));
    fields.extend(fixture::subrecord(b"CIS2", b"other parameter\0"));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(0, 2.0f32.to_bits(), 407, 2, 0x2C81, 0, 0),
    ));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(0, 3.0f32.to_bits(), 407, 5, (-8i32) as u32, 0, 0),
    ));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(0, 4.0f32.to_bits(), 407, 0xDE98_7654, 0xFEAB_7531, 0, 0),
    ));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(0, 5.0f32.to_bits(), 639, 0x14, (-17.375f32).to_bits(), 0, 0),
    ));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(0, 6.0f32.to_bits(), 675, 0xFF23_4567, 0xDEA5_7654, 0, 0),
    ));
    fields.extend(fixture::subrecord(b"CIS1", &[b'V', 0xE9, b'r', 0]));
    let target = fixture::record(
        b"NPC_",
        0x2C81,
        0,
        &fixture::subrecord(b"EDID", b"ConditionActor\0"),
    );
    let result = Plugins::new(
        &[
            fixture::global(0x2C80),
            target,
            fixture::spell(0x2C82, &fields),
        ]
        .concat(),
    )
    .read();
    let record = &result.records[&0x2C82];
    let event = member(&condition(record, 0).value, "parameter_1");
    assert_eq!(member(event, "event_function"), &Value::Unsigned(3));
    assert_eq!(member(event, "event_member"), &Value::Unsigned(0x3146));
    assert_eq!(
        member(&condition(record, 0).value, "parameter_2"),
        &Value::FormId(0x2C80)
    );
    assert_eq!(
        member(&condition(record, 1).value, "parameter_2"),
        &Value::FormId(0x2C81)
    );
    assert_eq!(
        member(&condition(record, 2).value, "parameter_2"),
        &Value::Signed(-8)
    );
    assert_eq!(
        member(&condition(record, 3).value, "parameter_2"),
        &Value::Bytes(0xFEAB_7531u32.to_le_bytes().to_vec())
    );
    assert_eq!(
        member(&condition(record, 4).value, "parameter_2"),
        &Value::Float(-17.375)
    );
    assert_eq!(
        member(&condition(record, 5).value, "parameter_1"),
        &Value::Unsigned(0xFF23_4567)
    );
    let strings = record
        .fields
        .iter()
        .filter(|field| field.signature == *b"CIS1" || field.signature == *b"CIS2")
        .map(|field| field.value.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        strings,
        [
            Value::String("event parameter".into()),
            Value::String("other parameter".into()),
            Value::String("Vér".into())
        ]
    );
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 0);
}

/// Unknown indices preserve slots; malformed CTDA drops its dependent strings.
#[test]
fn unknown_functions_and_malformed_lengths_preserve_neighboring_fields() {
    let unknown = fixture::condition(
        0x0A,
        7.5f32.to_bits(),
        4096,
        0xFE32_1759,
        0xDD74_5281,
        5,
        91,
    );
    let mut fields = fixture::subrecord(b"CTDA", &unknown);
    fields.extend(fixture::subrecord(b"CTDA", &unknown[..31]));
    fields.extend(fixture::subrecord(
        b"CIS1",
        b"malformed predecessor string\0",
    ));
    fields.extend(fixture::subrecord(b"CTDA", &[0; 33]));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(0, 2.75f32.to_bits(), 6, 89, 0xCF25_7391, 0, 0),
    ));
    let result = Plugins::new(&fixture::spell(0x2CA0, &fields)).read();
    let record = &result.records[&0x2CA0];
    assert_eq!(
        record
            .fields
            .iter()
            .filter(|field| field.signature == *b"CTDA")
            .count(),
        2
    );
    assert_eq!(condition(record, 0).canonical_bytes, unknown);
    assert_eq!(
        member(&condition(record, 0).value, "parameter_1"),
        &Value::Bytes(0xFE32_1759u32.to_le_bytes().to_vec())
    );
    assert_eq!(
        member(&condition(record, 1).value, "parameter_1"),
        &Value::Unsigned(89)
    );
    assert_eq!(record.rejected_fields, [*b"CTDA", *b"CIS1"]);
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 3);
    assert_eq!(result.diagnostics["skyrim.esm"].invalid_links, 0);
    assert!(
        !record
            .fields
            .iter()
            .any(|field| field.value == Value::String("malformed predecessor string".into()))
    );
}

/// Read the canonical public record wire representation, rather than only source bytes.
fn published_record(database: &Connection, id: u32) -> ArchivedRecordData {
    let bytes: Vec<u8> = database
        .query_row("SELECT data FROM records WHERE form_id=?", [id], |row| {
            row.get(0)
        })
        .unwrap();
    rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes).unwrap()
}

/// Conversion publishes neighbors, clears broken full/light refs and omits malformed CTDA.
#[tokio::test]
async fn malformed_conditions_and_optional_refs_complete_real_publication() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let base_path = data.join("Skyrim.esm");
    let base_bytes = fs::read(&base_path).unwrap();
    let header_length = u32::from_le_bytes(base_bytes[4..8].try_into().unwrap()) as usize;
    let header = &base_bytes[24..24 + header_length];
    write_plugin(
        &data.join("Filler.esp"),
        header,
        0,
        &[],
        &fixture::global(0x2D30),
    );
    write_plugin(
        &data.join("FirstLight.esl"),
        header,
        0x200,
        &["Skyrim.esm"],
        &fixture::reference(0x0100_0AAA),
    );
    write_plugin(
        &data.join("SecondLight.esl"),
        header,
        0x200,
        &["Skyrim.esm"],
        &fixture::reference(0x0100_0BBB),
    );
    let mut fields = fixture::subrecord(b"CTDA", &[0; 31]);
    fields.extend(fixture::subrecord(b"CIS1", b"rejected condition string\0"));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(4, 0x0000_2D30, 1, 0x0100_0BBB, 0xF173_9517, 2, 0x0100_0BBB),
    ));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(4, 0x0400_2D30, 1, 0x0400_0BBB, 0xDEA5_1379, 2, 0x0400_0BBB),
    ));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(
            0,
            3.0f32.to_bits(),
            1,
            0x0100_2BBB,
            0xD579_1357,
            2,
            0x0100_2BBB,
        ),
    ));
    fields.extend(fixture::subrecord(
        b"CTDA",
        &fixture::condition(0, 7.75f32.to_bits(), 4096, 0xFE35_7193, 0xDF12_3456, 0, 0),
    ));
    let before = fixture::spell(0x0300_2D40, &[]);
    let affected = fixture::spell(0x0300_2D41, &fields);
    let after = fixture::spell(0x0300_2D42, &[]);
    write_plugin(
        &data.join("BrokenConditions.esp"),
        header,
        0,
        &["Filler.esp", "SecondLight.esl", "Skyrim.esm"],
        &[before, affected, after].concat(),
    );
    let plugins_file = temp.path().join("plugins.txt");
    fs::write(
        &plugins_file,
        "*Skyrim.esm\n*Filler.esp\n*FirstLight.esl\n*SecondLight.esl\n*BrokenConditions.esp\n",
    )
    .unwrap();
    let paths = [
        "Skyrim.esm",
        "Filler.esp",
        "FirstLight.esl",
        "SecondLight.esl",
        "BrokenConditions.esp",
    ]
    .map(|name| data.join(name));
    let decoded = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    let diagnostics = &decoded.diagnostics["brokenconditions.esp"];
    assert_eq!(diagnostics.skipped_fields, 2);
    assert_eq!(diagnostics.invalid_links, 5);
    assert_eq!(diagnostics.first_by_category.len(), 2);
    let output = temp.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(plugins_file);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [0x0200_2D40, 0x0200_2D41, 0x0200_2D42] {
        assert!(!published_record(&database, id).subrecords.is_empty());
    }
    let published = published_record(&database, 0x0200_2D41);
    let native = published
        .subrecords
        .iter()
        .filter(|field| field.tag == *b"CTDA")
        .map(|field| &field.data)
        .collect::<Vec<_>>();
    assert_eq!(native.len(), 4);
    assert!(native.iter().all(|bytes| bytes.len() == 32));
    assert_eq!(&native[0][4..8], &0x0100_2D30u32.to_le_bytes());
    assert_eq!(&native[0][12..16], &0xFE00_1BBBu32.to_le_bytes());
    assert_eq!(&native[0][24..28], &0xFE00_1BBBu32.to_le_bytes());
    assert_eq!(&native[1][4..8], &[0; 4]);
    assert_eq!(&native[1][12..16], &[0; 4]);
    assert_eq!(&native[1][24..28], &[0; 4]);
    assert_eq!(&native[2][12..16], &[0; 4]);
    assert_eq!(&native[2][24..28], &[0; 4]);
    assert_eq!(&native[3][12..16], &0xFE35_7193u32.to_le_bytes());
    assert!(
        published
            .subrecords
            .iter()
            .all(|field| field.tag != *b"CIS1")
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["brokenconditions.esp"]["skipped_fields"],
        2
    );
    assert_eq!(
        diagnostics["decoder"]["brokenconditions.esp"]["invalid_links"],
        5
    );
}

/// A rejected condition must not publish its CIS2 as another condition's variable name.
#[tokio::test]
async fn rejected_condition_drops_only_its_strings_in_real_publication() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let base_bytes = fs::read(data.join("Skyrim.esm")).unwrap();
    let header_length = u32::from_le_bytes(base_bytes[4..8].try_into().unwrap()) as usize;
    let header = &base_bytes[24..24 + header_length];
    let first = fixture::condition(0, 1.0f32.to_bits(), 53, 0x14, 0, 0, 0);
    let broken = fixture::condition(0, 2.0f32.to_bits(), 53, 0x14, 0, 0, 0);
    let third = fixture::condition(0, 3.0f32.to_bits(), 53, 0x14, 0, 0, 0);
    let fields = [
        fixture::subrecord(b"CTDA", &first),
        fixture::subrecord(b"CIS2", b"first_variable\0"),
        fixture::subrecord(b"CTDA", &broken[..31]),
        fixture::subrecord(b"CIS2", b"rejected_variable\0"),
        fixture::subrecord(b"CTDA", &third),
        fixture::subrecord(b"CIS2", b"third_variable\0"),
    ]
    .concat();
    write_plugin(
        &data.join("BrokenConditionStrings.esp"),
        header,
        0,
        &["Skyrim.esm"],
        &[
            fixture::spell(0x0100_2E40, &[]),
            fixture::spell(0x0100_2E41, &fields),
            fixture::spell(0x0100_2E42, &[]),
        ]
        .concat(),
    );
    let plugins_file = temp.path().join("plugins.txt");
    fs::write(&plugins_file, "*Skyrim.esm\n*BrokenConditionStrings.esp\n").unwrap();
    let output = temp.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(plugins_file);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [0x0100_2E40, 0x0100_2E42] {
        assert!(!published_record(&database, id).subrecords.is_empty());
    }
    let published = published_record(&database, 0x0100_2E41);
    let condition_fields = published
        .subrecords
        .iter()
        .filter(|field| matches!(&field.tag, b"CTDA" | b"CIS1" | b"CIS2"))
        .map(|field| (field.tag, field.data.clone()))
        .collect::<Vec<_>>();
    // Each native CTDA starts a distinct string-parameter ownership block.
    // Removing the bad CTDA alone would publish two CIS2 values under the first.
    assert_eq!(
        condition_fields,
        [
            (*b"CTDA", first.to_vec()),
            (*b"CIS2", b"first_variable\0".to_vec()),
            (*b"CTDA", third.to_vec()),
            (*b"CIS2", b"third_variable\0".to_vec()),
        ]
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["brokenconditionstrings.esp"]["skipped_fields"],
        2
    );
}
/// Rejected response header must not attach its text to a different response.
#[tokio::test]
async fn malformed_info_response_header_preserves_response_ownership_in_publication() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let base_bytes = fs::read(data.join("Skyrim.esm")).unwrap();
    let header_length = u32::from_le_bytes(base_bytes[4..8].try_into().unwrap()) as usize;
    let header = &base_bytes[24..24 + header_length];
    let mut first = [0u8; 24];
    first[12] = 1;
    let mut broken = [0u8; 24];
    broken[12] = 2;
    let mut third = [0u8; 24];
    third[12] = 3;
    let fields = [
        fixture::subrecord(b"EDID", b"ResponseOwnershipProbe\0"),
        fixture::subrecord(b"TRDT", &first),
        fixture::subrecord(b"NAM1", b"First response text\0"),
        fixture::subrecord(b"TRDT", &broken[..23]),
        fixture::subrecord(b"NAM1", b"Rejected response text\0"),
        fixture::subrecord(b"TRDT", &third),
        fixture::subrecord(b"NAM1", b"Third response text\0"),
    ]
    .concat();
    let info = fixture::record(b"INFO", 0x0100_2F41, 0, &fields);
    write_plugin(
        &data.join("BrokenInfoResponses.esp"),
        header,
        0,
        &["Skyrim.esm"],
        &[
            fixture::spell(0x0100_2F40, &[]),
            info,
            fixture::spell(0x0100_2F42, &[]),
        ]
        .concat(),
    );
    let plugins_file = temp.path().join("plugins.txt");
    fs::write(&plugins_file, "*Skyrim.esm\n*BrokenInfoResponses.esp\n").unwrap();
    let output = temp.path().join("pack");
    let mut config = PipelineConfig::new(&data, &output);
    config.plugins_file = Some(plugins_file);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    let retained_fixture = temp.keep();
    eprintln!(
        "Retained INFO recovery fixture: {}",
        retained_fixture.display()
    );
    assert!(report.complete, "{:?}", report.warnings);
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["brokeninforesponses.esp"]["skipped_fields"],
        2
    );
    assert_eq!(
        diagnostics["decoder"]["brokeninforesponses.esp"]["skipped_records"],
        0
    );
    let database = Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [0x0100_2F40, 0x0100_2F42] {
        assert!(!published_record(&database, id).subrecords.is_empty());
    }
    let published = published_record(&database, 0x0100_2F41);
    let fields = published
        .subrecords
        .iter()
        .filter(|field| matches!(&field.tag, b"TRDT" | b"NAM1"))
        .map(|field| (field.tag, field.data.clone()))
        .collect::<Vec<_>>();
    // The wire format uses each TRDT as the parent of following NAM1 text.
    // Two NAM1 fields between valid headers would associate the bad second
    // response's text with the first response, losing the native parent boundary.
    assert_eq!(
        fields,
        [
            (*b"TRDT", first.to_vec()),
            (*b"NAM1", b"First response text\0".to_vec()),
            (*b"TRDT", third.to_vec()),
            (*b"NAM1", b"Third response text\0".to_vec()),
        ]
    );
}
#[test]
fn malformed_info_text_omits_only_itself_with_valid_response_anchor() {
    let mut first = [0u8; 24];
    first[12] = 1;
    let mut second = [0u8; 24];
    second[12] = 2;
    let fields = [
        fixture::subrecord(b"TRDT", &first),
        fixture::subrecord(b"NAM1", &[0x31, 0x42, 0x53]),
        fixture::subrecord(b"NAM2", b"Usable first response notes\0"),
        fixture::subrecord(b"TRDT", &second),
        fixture::subrecord(b"NAM1", &0x3142_5364u32.to_le_bytes()),
    ]
    .concat();
    let plugins = Plugins::new(&fixture::record(b"INFO", 0x2F51, 0, &fields));
    // A localized source requires exactly four bytes for each NAM1 key.
    let mut bytes = fs::read(&plugins.paths[0]).unwrap();
    bytes[8..12].copy_from_slice(&0x80u32.to_le_bytes());
    fs::write(&plugins.paths[0], bytes).unwrap();
    let result = plugins.read();
    let info = &result.records[&0x2F51];
    let projected = info
        .fields
        .iter()
        .map(|field| field.signature)
        .collect::<Vec<_>>();
    assert_eq!(projected, [*b"TRDT", *b"NAM2", *b"TRDT", *b"NAM1"]);
    assert_eq!(info.fields[0].canonical_bytes, first);
    assert_eq!(info.fields[2].canonical_bytes, second);
    assert!(matches!(
        info.fields[3].value,
        Value::LocalizedString(0x3142_5364)
    ));
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 1);
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_records, 0);
}

#[test]
fn rejected_info_response_block_stops_at_matched_condition_group() {
    let fields = [
        fixture::subrecord(b"TRDT", &[0; 23]),
        fixture::subrecord(b"NAM1", b"Rejected response text\0"),
        fixture::subrecord(b"NAM2", b"Rejected response notes\0"),
        fixture::subrecord(
            b"CTDA",
            &fixture::condition(0, 1.0f32.to_bits(), 53, 0x14, 0, 0, 0),
        ),
        fixture::subrecord(b"CIS2", b"Usable condition variable\0"),
    ]
    .concat();
    let result = Plugins::new(&fixture::record(b"INFO", 0x2F52, 0, &fields)).read();
    let info = &result.records[&0x2F52];
    assert_eq!(
        info.fields
            .iter()
            .map(|field| field.signature)
            .collect::<Vec<_>>(),
        [*b"CTDA", *b"CIS2"]
    );
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 3);
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_records, 0);
}
