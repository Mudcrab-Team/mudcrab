//! Native AI unions, contextual repeat groups and broken-mod publication recovery.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{DecodedField, DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, inhouse_ai as ai, layout};
use rusqlite::Connection;
use std::{fs, path::PathBuf};

/// Real generated world framing plus independently authored supplemental records.
struct Fixture {
    directory: tempfile::TempDir,
    header: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Keep the generator's native nested world and append this family's fixtures.
    fn new(records: &[u8]) -> Self {
        let mut bytes = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        let header_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let header = bytes[24..24 + header_size].to_vec();
        bytes.extend(records);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Skyrim.esm");
        fs::write(&path, bytes).unwrap();
        Self {
            directory,
            header,
            paths: vec![path],
        }
    }

    /// Shift the next plugin's global slot without changing its one-master local namespace.
    fn unrelated(&mut self) {
        let path = self.directory.path().join("Unrelated.esp");
        fs::write(&path, ai::record(b"TES4", 0, 0, &self.header)).unwrap();
        self.paths.push(path);
    }

    /// A patch knows Skyrim only; its own file slot1 can differ from its global slot2.
    fn patch(&mut self, name: &str, records: &[u8]) {
        let mut header = self.header.clone();
        header.extend(ai::subrecord(b"MAST", b"Skyrim.esm\0"));
        header.extend(ai::subrecord(b"DATA", &[0xA7; 8]));
        let bytes = [ai::record(b"TES4", 0, 0, &header), records.to_vec()].concat();
        let path = self.directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }

    /// Exercise the public scanner, schema decoder, override selection and winner link checks.
    fn read(&self) -> ReadResult {
        read_plugins(&self.paths, &LoadOrder::read(&self.paths).unwrap()).unwrap()
    }
}

/// Select by authored role, keeping same-signature contexts distinct.
fn named<'a>(record: &'a DecodedRecord, name: &str) -> &'a DecodedField {
    record
        .fields
        .iter()
        .find(|field| field.name == name)
        .unwrap()
}

/// Inspect one typed struct member without reconstructing values from canonical bytes.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}")
    };
    &members.iter().find(|(member, _)| member == name).unwrap().1
}

/// Native values, flags, contextual signatures and all three package events survive in order.
#[test]
fn all_ai_types_keep_native_values_and_contextual_groups() {
    let result = Fixture::new(&ai::records()).read();
    for (index, signature) in ai::SIGNATURES.iter().enumerate() {
        let record = &result.records[&(ai::FIRST_ID + index as u32)];
        assert_eq!(&record.record_type, signature);
        assert!(record.supported);
        assert!(record.payload_complete);
        assert!(record.rejected_fields.is_empty(), "{signature:?}");
        assert_eq!(record.version_control, 0x1937_A5C2);
        assert_eq!(record.header_unknown, 0xA753);
    }
    let package = &result.records[&ai::FIRST_ID];
    let configuration = &named(package, "package_configuration").value;
    assert_eq!(
        member(configuration, "general_flags"),
        &Value::Unsigned(0x8002_0205)
    );
    assert_eq!(
        member(configuration, "unknown_7"),
        &Value::Bytes(vec![0xA7])
    );
    assert_eq!(
        member(configuration, "unknown_10"),
        &Value::Bytes(vec![0x53, 0xC9])
    );
    let schedule = &named(package, "schedule").value;
    assert_eq!(member(schedule, "month"), &Value::Signed(-1));
    assert_eq!(member(schedule, "date"), &Value::Unsigned(13));
    assert_eq!(member(schedule, "minute"), &Value::Signed(37));
    assert_eq!(member(schedule, "duration_minutes"), &Value::Signed(-97));
    let values = package
        .fields
        .iter()
        .filter(|field| field.name == "input_value")
        .map(|field| field.value.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        [
            Value::Unsigned(1),
            Value::Unsigned(0xF173_59A2),
            Value::Float(-3.625),
            Value::Float(19.875)
        ]
    );
    assert_eq!(
        member(&named(package, "location").value, "value"),
        &Value::Signed(-19)
    );
    assert_eq!(
        member(&named(package, "target").value, "value"),
        &Value::Signed(-23)
    );
    let branch_types = package
        .fields
        .iter()
        .filter(|field| field.name == "branch_type")
        .map(|field| field.value.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        branch_types,
        [
            Value::String("Sequence".into()),
            Value::String("Procedure".into())
        ]
    );
    let procedure_types = package
        .fields
        .iter()
        .filter(|field| field.name == "procedure_type")
        .map(|field| field.value.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        procedure_types,
        [Value::String("Travel".into()), Value::String("Wait".into())]
    );
    assert_eq!(
        package
            .fields
            .iter()
            .filter(|field| field.signature == *b"PFO2")
            .count(),
        4
    );
    assert_eq!(
        package
            .fields
            .iter()
            .filter(|field| field.signature == *b"INAM")
            .count(),
        3
    );
    assert_eq!(named(package, "combat_style").value, Value::FormId(0));
    assert_eq!(
        named(package, "procedure_marker").value,
        Value::Bytes(vec![0xA7])
    );
    for event in ["on_begin", "on_end", "on_change"] {
        assert_eq!(
            named(package, &format!("{event}_idle")).value,
            Value::FormId(ai::FIRST_ID + 1)
        );
        assert_eq!(
            member(&named(package, &format!("{event}_topic")).value, "value"),
            &Value::String("Purg".into())
        );
        assert_eq!(
            named(package, &format!("{event}_schr_unused")).value,
            Value::Bytes(vec![0xA7; 20])
        );
        assert_eq!(
            named(package, &format!("{event}_qnam_unused")).value,
            Value::Bytes(0xFEA7_53C9u32.to_le_bytes().to_vec())
        );
        assert_eq!(
            named(package, &format!("{event}_tnam_unused")).value,
            Value::Bytes(vec![0x19, 0x73, 0xA7, 0x53])
        );
    }
    assert_eq!(
        named(package, "on_change_scda_unused").value,
        Value::Bytes(vec![0x53; 17])
    );
    let idle = &result.records[&(ai::FIRST_ID + 1)];
    assert_eq!(
        named(idle, "related_idles").value,
        Value::Array(vec![
            Value::FormId(ai::FIRST_ID + 3),
            Value::FormId(ai::FIRST_ID + 1)
        ])
    );
    let idle_data = &named(idle, "unused_animation_data").value;
    assert_eq!(member(idle_data, "loop_min"), &Value::Unsigned(11));
    assert_eq!(member(idle_data, "loop_max"), &Value::Unsigned(37));
    assert_eq!(member(idle_data, "replay_delay"), &Value::Unsigned(0xA753));
    let marker = &result.records[&(ai::FIRST_ID + 2)];
    assert_eq!(named(marker, "animation_count").value, Value::Unsigned(2));
    assert_eq!(named(marker, "idle_timer").value, Value::Float(-7.875));
    assert_eq!(
        named(marker, "animations").value,
        Value::Array(vec![Value::FormId(ai::FIRST_ID + 1); 2])
    );
    let color = &named(&result.records[&(ai::FIRST_ID + 3)], "color").value;
    for (name, value) in [("red", 19), ("green", 73), ("blue", 167), ("alpha", 211)] {
        assert_eq!(member(color, name), &Value::Unsigned(value));
    }
    let diagnostics = &result.diagnostics["skyrim.esm"];
    assert_eq!(diagnostics.skipped_records, 0);
    assert_eq!(diagnostics.skipped_fields, 0);
    assert_eq!(diagnostics.unexpected_subrecords, 0);
    assert_eq!(diagnostics.invalid_links, 0);
}

/// Every native location/target discriminator selects an explicitly authored alternative.
#[test]
fn package_location_target_and_topic_unions_cover_all_native_kinds() {
    let mut payload = ai::subrecord(b"EDID", &ai::text("AllNativeKinds"));
    let sentinel = 0xFEA7_53C9u32;
    for kind in 0..=12 {
        payload.extend(ai::subrecord(b"ANAM", &ai::text("Location")));
        let value = match kind {
            0 | 1 | 4 | 6 => 0,
            8 | 9 => (-29i32) as u32,
            _ => sentinel,
        };
        payload.extend(ai::subrecord(
            b"PLDT",
            &ai::location(kind, value, -73 - kind),
        ));
    }
    for kind in 0..=6 {
        payload.extend(ai::subrecord(b"ANAM", &ai::text("Target")));
        let value = match kind {
            0 | 1 => 0,
            3 => ai::FIRST_ID + 1,
            4 => (-31i32) as u32,
            _ => sentinel,
        };
        payload.extend(ai::subrecord(b"PTDA", &ai::target(kind, value, 17 + kind)));
    }
    payload.extend(ai::subrecord(b"ANAM", &ai::text("Topic")));
    payload.extend(ai::subrecord(b"PDTO", &ai::topic(0, 0u32.to_le_bytes())));
    payload.extend(ai::subrecord(b"PDTO", &ai::topic(1, *b"Helo")));
    let records = [ai::records(), ai::record(b"PACK", 0x1D00, 0, &payload)].concat();
    let result = Fixture::new(&records).read();
    let package = &result.records[&0x1D00];
    let locations = package
        .fields
        .iter()
        .filter(|field| field.signature == *b"PLDT")
        .collect::<Vec<_>>();
    assert_eq!(locations.len(), 13);
    for (kind, field) in locations.iter().enumerate() {
        let expected = match kind {
            0 | 1 | 4 | 6 => Value::FormId(0),
            5 => Value::Unsigned(u64::from(sentinel)),
            8 | 9 => Value::Signed(-29),
            _ => Value::Bytes(sentinel.to_le_bytes().to_vec()),
        };
        assert_eq!(
            member(&field.value, "value"),
            &expected,
            "location kind {kind}"
        );
        assert_eq!(
            member(&field.value, "radius"),
            &Value::Signed(-73 - kind as i64)
        );
    }
    let targets = package
        .fields
        .iter()
        .filter(|field| field.signature == *b"PTDA")
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 7);
    for (kind, field) in targets.iter().enumerate() {
        let expected = match kind {
            0 | 1 => Value::FormId(0),
            2 => Value::Unsigned(u64::from(sentinel)),
            3 => Value::FormId(ai::FIRST_ID + 1),
            4 => Value::Signed(-31),
            _ => Value::Bytes(sentinel.to_le_bytes().to_vec()),
        };
        assert_eq!(
            member(&field.value, "value"),
            &expected,
            "target kind {kind}"
        );
    }
    let topics = package
        .fields
        .iter()
        .filter(|field| field.signature == *b"PDTO")
        .collect::<Vec<_>>();
    assert_eq!(member(&topics[0].value, "value"), &Value::FormId(0));
    assert_eq!(
        member(&topics[1].value, "value"),
        &Value::String("Helo".into())
    );
    assert!(package.rejected_fields.is_empty());
    assert_eq!(result.diagnostics["skyrim.esm"].invalid_links, 0);
}

/// File slot1 maps to global slot2; aliases and ignored native words stay unchanged.
#[test]
fn package_links_remap_independently_of_aliases_unused_words_and_bad_neighbors() {
    let mut fixture = Fixture::new(&ai::records());
    fixture.unrelated();
    let local_reference = 0x0100_1DB0u32;
    let local_idle = 0x0100_1DB1u32;
    let mut records = ai::record(
        b"REFR",
        local_reference,
        0,
        &ai::subrecord(b"EDID", &ai::text("PatchReference")),
    );
    records.extend(ai::record(
        b"IDLE",
        local_idle,
        0,
        &ai::subrecord(b"EDID", &ai::text("PatchIdle")),
    ));
    let mut payload = ai::subrecord(b"EDID", &ai::text("PatchPackage"));
    payload.extend(ai::subrecord(
        b"IDLA",
        &[local_idle.to_le_bytes(), 0x0300_1DB1u32.to_le_bytes()].concat(),
    ));
    for (kind, value) in [
        (0, local_reference),
        (8, local_reference),
        (12, 0xFEA7_53C9),
    ] {
        payload.extend(ai::subrecord(b"ANAM", &ai::text("Location")));
        payload.extend(ai::subrecord(b"PLDT", &ai::location(kind, value, 37)));
    }
    payload.extend(ai::subrecord(b"ANAM", &ai::text("Target")));
    payload.extend(ai::subrecord(b"PTDA", &ai::target(0, local_reference, 19)));
    records.extend(ai::record(b"PACK", 0x0100_1DB2, 0, &payload));
    fixture.patch("PackageLinks.esp", &records);
    let result = fixture.read();
    let package = &result.records[&0x0200_1DB2];
    assert_eq!(
        named(package, "animations").value,
        Value::Array(vec![Value::FormId(0x0200_1DB1), Value::FormId(0)])
    );
    let locations = package
        .fields
        .iter()
        .filter(|field| field.signature == *b"PLDT")
        .collect::<Vec<_>>();
    assert_eq!(
        member(&locations[0].value, "value"),
        &Value::FormId(0x0200_1DB0)
    );
    assert_eq!(
        &locations[0].canonical_bytes[4..8],
        &0x0200_1DB0u32.to_le_bytes()
    );
    assert_eq!(
        member(&locations[1].value, "value"),
        &Value::Signed(i64::from(local_reference))
    );
    assert_eq!(
        &locations[1].canonical_bytes[4..8],
        &local_reference.to_le_bytes()
    );
    assert_eq!(
        member(&locations[2].value, "value"),
        &Value::Bytes(0xFEA7_53C9u32.to_le_bytes().to_vec())
    );
    assert_eq!(
        member(&named(package, "target").value, "value"),
        &Value::FormId(0x0200_1DB0)
    );
    assert_eq!(result.diagnostics["packagelinks.esp"].invalid_links, 1);
    assert_eq!(result.diagnostics["packagelinks.esp"].skipped_fields, 0);
}

/// Valid one-byte IDLC and source-known unknown CNAM data remain bounded explicit alternatives.
#[test]
fn package_legacy_idle_prefix_and_explicit_unknown_input_are_preserved() {
    let payload = [
        ai::subrecord(b"EDID", &ai::text("LegacyIdlePrefix")),
        ai::subrecord(b"IDLC", &[7]),
        ai::subrecord(b"ANAM", &ai::text("SourceUnknownType")),
        ai::subrecord(b"CNAM", &[0x19, 0x73, 0xA7, 0x53, 0xC9]),
    ]
    .concat();
    let result = Fixture::new(&ai::record(b"PACK", 0x1D30, 0, &payload)).read();
    let package = &result.records[&0x1D30];
    assert_eq!(
        member(&named(package, "idle_count").value, "count"),
        &Value::Unsigned(7)
    );
    assert_eq!(
        named(package, "input_value").value,
        Value::Bytes(vec![0x19, 0x73, 0xA7, 0x53, 0xC9])
    );
    assert!(package.rejected_fields.is_empty());
}

/// Type selectors use the same first-NUL prefix as strings while preserving native padding.
#[test]
fn package_padded_input_types_keep_typed_values_and_canonical_padding() {
    let bool_type = b"Bool\0\0";
    let float_type = b"Float\0padding";
    let payload = [
        ai::subrecord(b"EDID", &ai::text("PaddedInputTypes")),
        ai::subrecord(b"ANAM", bool_type),
        ai::subrecord(b"CNAM", &[1]),
        ai::subrecord(b"ANAM", float_type),
        ai::subrecord(b"CNAM", &17.125f32.to_le_bytes()),
    ]
    .concat();
    let result = Fixture::new(&ai::record(b"PACK", 0x1D35, 0, &payload)).read();
    let package = &result.records[&0x1D35];
    let types = package
        .fields
        .iter()
        .filter(|field| field.name == "input_type")
        .collect::<Vec<_>>();
    assert_eq!(types[0].value, Value::String("Bool".into()));
    assert_eq!(types[1].value, Value::String("Float".into()));
    assert_eq!(types[0].canonical_bytes.as_slice(), bool_type.as_slice());
    assert_eq!(types[1].canonical_bytes.as_slice(), float_type.as_slice());
    let values = package
        .fields
        .iter()
        .filter(|field| field.name == "input_value")
        .map(|field| &field.value)
        .collect::<Vec<_>>();
    assert_eq!(values, [&Value::Unsigned(1), &Value::Float(17.125)]);
    assert_eq!(package.raw_payload, payload);
    assert!(package.rejected_fields.is_empty());
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 0);
}

/// Broken input values and invalid discriminators omit only their framed fields.
#[test]
fn malformed_package_values_recover_at_the_next_input_group() {
    let payload = [
        ai::subrecord(b"EDID", &ai::text("LocalFieldRecovery")),
        ai::subrecord(b"ANAM", &ai::text("Bool")),
        ai::subrecord(b"CNAM", &[1, 2]),
        ai::subrecord(b"ANAM", &ai::text("Location")),
        ai::subrecord(b"PLDT", &ai::location(13, 0, 17)),
        ai::subrecord(b"ANAM", &ai::text("Target")),
        ai::subrecord(b"PTDA", &ai::target(-1, 0, 19)),
        ai::subrecord(b"ANAM", &ai::text("Topic")),
        ai::subrecord(b"PDTO", &ai::topic(2, *b"Helo")),
        ai::subrecord(b"ANAM", &ai::text("Int")),
        ai::subrecord(b"CNAM", &0xF173_59A2u32.to_le_bytes()),
        ai::subrecord(b"XNAM", &[]),
        ai::subrecord(b"ANAM", &ai::text("Procedure")),
        ai::subrecord(b"PNAM", &ai::text("Wait")),
    ]
    .concat();
    let result = Fixture::new(&ai::record(b"PACK", 0x1D40, 0, &payload)).read();
    let package = &result.records[&0x1D40];
    assert_eq!(
        named(package, "input_value").value,
        Value::Unsigned(0xF173_59A2)
    );
    assert_eq!(
        named(package, "procedure_type").value,
        Value::String("Wait".into())
    );
    for rejected in [*b"CNAM", *b"PLDT", *b"PTDA", *b"PDTO"] {
        assert!(package.rejected_fields.contains(&rejected));
    }
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 4);
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_records, 0);
}

/// All four families retain prior winners and valid patch neighbors through real publication.
#[tokio::test]
async fn ai_families_publish_prior_winners_and_neighbors_after_bad_compression() {
    let temporary = tempfile::tempdir().unwrap();
    let data = temporary.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let base_path = data.join("Skyrim.esm");
    let mut bytes = fs::read(&base_path).unwrap();
    let header_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let mut header = bytes[24..24 + header_size].to_vec();
    header.extend(ai::subrecord(b"MAST", b"Skyrim.esm\0"));
    header.extend(ai::subrecord(b"DATA", &[0xA7; 8]));
    bytes.extend(ai::records());
    fs::write(&base_path, bytes).unwrap();
    let earlier = read_plugins(
        std::slice::from_ref(&base_path),
        &LoadOrder::read(std::slice::from_ref(&base_path)).unwrap(),
    )
    .unwrap();
    let mut patch = ai::record(b"TES4", 0, 0, &header);
    let mut neighbors = Vec::new();
    for (index, signature) in ai::SIGNATURES.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        let before = 0x0100_1E00 + index * 2;
        let after = before + 1;
        neighbors.extend([before, after]);
        patch.extend(ai::action(before));
        patch.extend(ai::record(
            signature,
            ai::FIRST_ID + index,
            0x40000,
            &[24, 0, 0, 0, 99],
        ));
        patch.extend(ai::action(after));
    }
    // Required malformed configuration invalidates this candidate, retaining its base winner.
    let damaged = [
        ai::subrecord(b"EDID", &ai::text("BrokenRequiredConfiguration")),
        ai::subrecord(b"PKDT", &[0; 11]),
    ]
    .concat();
    patch.extend(ai::record(b"PACK", ai::FIRST_ID, 0, &damaged));
    // Padded type names retain their first-NUL meaning: malformed Bool is omitted,
    // while the following padded Float and valid integer reach real publication.
    let recovered_package = [
        ai::subrecord(b"EDID", &ai::text("OptionalUnionRecovery")),
        ai::subrecord(b"ANAM", b"Bool\0\0"),
        ai::subrecord(b"CNAM", &[1, 2]),
        ai::subrecord(b"ANAM", b"Float\0publication-padding"),
        ai::subrecord(b"CNAM", &(-11.375f32).to_le_bytes()),
        ai::subrecord(b"ANAM", &ai::text("Int")),
        ai::subrecord(b"CNAM", &93u32.to_le_bytes()),
    ]
    .concat();
    patch.extend(ai::record(b"PACK", 0x0100_1E90, 0, &recovered_package));
    let patch_path = data.join("DamagedAI.esp");
    fs::write(&patch_path, patch).unwrap();
    let paths = [base_path, patch_path];
    let recovered = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    let warnings = &recovered.diagnostics["damagedai.esp"];
    assert_eq!(warnings.skipped_records, 5);
    assert_eq!(warnings.skipped_fields, 1);
    for index in 0..4 {
        let id = ai::FIRST_ID + index;
        assert_eq!(recovered.records[&id].load_order, 0);
        assert_eq!(
            recovered.records[&id].raw_payload,
            earlier.records[&id].raw_payload
        );
    }
    let input_values = recovered.records[&0x0100_1E90]
        .fields
        .iter()
        .filter(|field| field.name == "input_value")
        .map(|field| &field.value)
        .collect::<Vec<_>>();
    assert_eq!(input_values, [&Value::Float(-11.375), &Value::Unsigned(93)]);
    let plugins = temporary.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n*DamagedAI.esp\n").unwrap();
    let output = temporary.path().join("pack");
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
    for index in 0..4 {
        let id = ai::FIRST_ID + index;
        let (load_order, payload): (i64, Vec<u8>) = database.query_row(
            "SELECT source.load_order,source.payload FROM inhouse_source_records source JOIN records ON records.form_id=source.form_id WHERE source.form_id=?",
            [id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(load_order, 0);
        assert_eq!(payload, earlier.records[&id].raw_payload);
    }
    for id in neighbors.into_iter().chain([0x0100_1E90]) {
        let count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "missing usable patch record {id:08X}");
    }
    let published: Vec<u8> = database
        .query_row(
            "SELECT data FROM records WHERE form_id=0x01001E90",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let canonical = rkyv::from_bytes::<
        converter::esm::types::ArchivedRecordData,
        rkyv::rancor::Error,
    >(&published)
    .unwrap();
    let values = canonical
        .subrecords
        .iter()
        .filter(|field| field.tag == *b"CNAM")
        .collect::<Vec<_>>();
    assert_eq!(
        values.len(),
        2,
        "malformed optional bytes must not enter canonical consumer data"
    );
    assert_eq!(values[0].data, (-11.375f32).to_le_bytes());
    assert_eq!(values[1].data, 93u32.to_le_bytes());
    let source: Vec<u8> = database
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=0x01001E90",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source, recovered_package);
    let types = canonical
        .subrecords
        .iter()
        .filter(|field| field.tag == *b"ANAM")
        .collect::<Vec<_>>();
    // The existing runtime adapter emits UTF-8 strings with one NUL; provenance
    // above retains their complete native padding and the rejected value bytes.
    assert_eq!(types[0].data.as_slice(), b"Bool\0".as_slice());
    assert_eq!(types[1].data.as_slice(), b"Float\0".as_slice());
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["damagedai.esp"]["skipped_records"],
        5
    );
    assert_eq!(diagnostics["decoder"]["damagedai.esp"]["skipped_fields"], 1);
}

/// Shared D conditions activate PACK root/procedure and IDLE roles with source-slot remapping.
#[test]
fn shared_conditions_activate_all_ai_roles_and_preserve_native_bytes() {
    use dummy_content::inhouse_conditions as native;
    let mut fixture = Fixture::new(&ai::records());
    fixture.unrelated();
    let add_plugin =
        |fixture: &mut Fixture, name: &str, flags: u32, masters: &[&str], records: &[u8]| {
            let mut header = fixture.header.clone();
            for master in masters {
                header.extend(ai::subrecord(b"MAST", &ai::text(master)));
                header.extend(ai::subrecord(b"DATA", &[0xA7; 8]));
            }
            let path = fixture.directory.path().join(name);
            fs::write(
                &path,
                [ai::record(b"TES4", 0, flags, &header), records.to_vec()].concat(),
            )
            .unwrap();
            fixture.paths.push(path);
        };
    let objects = [
        native::global(0x0100_1DC0),
        native::reference_with_base(0x0100_1DC1, 0x0000_0014),
    ]
    .concat();
    add_plugin(
        &mut fixture,
        "ConditionObjects.esm",
        1,
        &["Skyrim.esm"],
        &objects,
    );
    add_plugin(
        &mut fixture,
        "ConditionLight.esl",
        0x200,
        &["Skyrim.esm"],
        &native::reference_with_base(0x0100_08A3, 0x0000_0014),
    );
    let source_conditions: Vec<_> = (0..3usize)
        .map(|index| {
            let global = index != 1;
            let mut bytes = native::condition(
                if global { 4 } else { 0 },
                if global {
                    0x0100_1DC0
                } else {
                    (-7.375f32).to_bits()
                },
                1,
                if index == 1 { 0x0000_08A3 } else { 0x0100_1DC1 },
                0xFEA7_5319 + index as u32,
                2,
                if index == 1 { 0x0100_1DC1 } else { 0x0000_08A3 },
            );
            bytes[1] = 0x91 + index as u8;
            bytes[11] = 0x53 + index as u8;
            bytes[28..32].copy_from_slice(&(-19i32 - index as i32).to_le_bytes());
            bytes
        })
        .collect();
    let package = [
        ai::subrecord(b"EDID", &ai::text("ActivatedPackageConditions")),
        ai::subrecord(b"CTDA", &source_conditions[0]),
        ai::subrecord(b"CIS1", &ai::text("root-argument")),
        ai::subrecord(
            b"PKCU",
            &[0u32.to_le_bytes(), 0u32.to_le_bytes(), 19u32.to_le_bytes()].concat(),
        ),
        ai::subrecord(b"XNAM", &[0xA7]),
        ai::subrecord(b"ANAM", &ai::text("Procedure")),
        ai::subrecord(b"CITC", &1u32.to_le_bytes()),
        ai::subrecord(b"CTDA", &source_conditions[1]),
        ai::subrecord(b"CIS1", &ai::text("procedure-first")),
        ai::subrecord(b"CIS2", &ai::text("procedure-second")),
        ai::subrecord(b"PNAM", &ai::text("Wait")),
    ]
    .concat();
    let idle = [
        ai::subrecord(b"EDID", &ai::text("ActivatedIdleCondition")),
        ai::subrecord(b"CTDA", &source_conditions[2]),
        ai::subrecord(b"CIS1", &ai::text("idle-argument")),
        ai::subrecord(b"DATA", &[11, 37, 0x89, 5, 0x53, 0xA7]),
    ]
    .concat();
    let records = [
        ai::record(b"PACK", 0x0300_1DC2, 0, &package),
        ai::record(b"IDLE", 0x0300_1DC3, 0, &idle),
    ]
    .concat();
    add_plugin(
        &mut fixture,
        "TypedAIConditions.esp",
        0,
        &["ConditionLight.esl", "ConditionObjects.esm", "Skyrim.esm"],
        &records,
    );
    let result = fixture.read();
    let package_record = &result.records[&0x0300_1DC2];
    let idle_record = &result.records[&0x0300_1DC3];
    assert_eq!(package_record.raw_payload, package);
    assert_eq!(idle_record.raw_payload, idle);
    let conditions: Vec<_> = package_record
        .fields
        .iter()
        .chain(&idle_record.fields)
        .filter(|field| field.signature == *b"CTDA")
        .collect();
    assert_eq!(conditions.len(), 3);
    for (index, field) in conditions.iter().enumerate() {
        assert_eq!(field.name, "condition");
        assert_eq!(member(&field.value, "function_index"), &Value::Unsigned(1));
        let first = if index == 1 {
            0xFE00_08A3u32
        } else {
            0x0200_1DC1
        };
        let reference = if index == 1 {
            0x0200_1DC1u32
        } else {
            0xFE00_08A3
        };
        assert_eq!(member(&field.value, "parameter_1"), &Value::FormId(first));
        assert_eq!(
            member(&field.value, "parameter_2"),
            &Value::Bytes((0xFEA7_5319u32 + index as u32).to_le_bytes().to_vec())
        );
        assert_eq!(member(&field.value, "reference"), &Value::FormId(reference));
        assert_eq!(
            member(&field.value, "parameter_3"),
            &Value::Signed(-19 - index as i64)
        );
        assert_eq!(
            member(&field.value, "comparison"),
            &if index == 1 {
                Value::Float(-7.375)
            } else {
                Value::FormId(0x0200_1DC0)
            }
        );
        let mut expected = source_conditions[index].clone();
        expected[12..16].copy_from_slice(&first.to_le_bytes());
        expected[24..28].copy_from_slice(&reference.to_le_bytes());
        if index != 1 {
            expected[4..8].copy_from_slice(&0x0200_1DC0u32.to_le_bytes());
        }
        assert_eq!(
            field.canonical_bytes, expected,
            "AI CTDA role {index}: exact non-link bytes"
        );
    }
    let package_strings: Vec<_> = package_record
        .fields
        .iter()
        .filter(|field| matches!(&field.signature, b"CIS1" | b"CIS2"))
        .map(|field| field.value.clone())
        .collect();
    assert_eq!(
        package_strings,
        ["root-argument", "procedure-first", "procedure-second"]
            .map(|text| Value::String(text.into()))
    );
    assert!(package_record.rejected_fields.is_empty());
    assert!(idle_record.rejected_fields.is_empty());
    let diagnostics = &result.diagnostics["typedaiconditions.esp"];
    assert_eq!(diagnostics.skipped_records, 0);
    assert_eq!(diagnostics.skipped_fields, 0);
    assert_eq!(diagnostics.invalid_links, 0);
    assert_eq!(diagnostics.unexpected_subrecords, 0);
}
