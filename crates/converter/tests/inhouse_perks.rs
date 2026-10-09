//! Complete PERK alternatives, isolated dependency groups, and real publication recovery.
use converter::{
    AssetPipeline, PipelineConfig,
    config::RecordReader,
    esm::load_order::LoadOrder,
    records::{DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, inhouse_perks as perks, layout};
use rusqlite::Connection;
use std::{fs, path::PathBuf};

/// A real generated world with independently authored native perk records.
struct Fixture {
    directory: tempfile::TempDir,
    header: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Put independent PERK/target records after the normal nested dummy world.
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

    /// Explicit master order and ESL flags allow local and global indices to differ.
    fn plugin(&mut self, name: &str, flags: u32, masters: &[&str], records: &[u8]) {
        let mut header = self.header.clone();
        for master in masters {
            header.extend(perks::subrecord(b"MAST", &perks::text(master)));
            header.extend(perks::subrecord(b"DATA", &[0xA5; 8]));
        }
        let mut bytes = perks::record(b"TES4", 0, flags, &header);
        bytes.extend(records);
        let path = self.directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        self.paths.push(path);
    }

    /// Exercise the accepted winner catalogue and final typed target validation.
    fn read(&self) -> ReadResult {
        read_plugins(&self.paths, &LoadOrder::read(&self.paths).unwrap()).unwrap()
    }
}

/// Find a named typed struct member without relying on ordering within that struct.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected struct: {value:?}")
    };
    &members.iter().find(|(key, _)| key == name).unwrap().1
}

/// Ordered field names distinguish top-level DATA from repeated effect DATA.
fn values<'a>(record: &'a DecodedRecord, name: &str) -> Vec<&'a Value> {
    record
        .fields
        .iter()
        .filter(|field| field.name == name)
        .map(|field| &field.value)
        .collect()
}

/// Every native effect/function layout is decoded with discriminating values and padding.
#[test]
fn all_perk_effects_functions_and_entry_points_keep_native_values() {
    let fixture = Fixture::new(
        &[
            perks::targets(),
            perks::complete(perks::FIRST_ID),
            perks::all_entry_points(perks::FIRST_ID + 1),
        ]
        .concat(),
    );
    let result = fixture.read();
    let record = &result.records[&perks::FIRST_ID];
    assert!(record.supported);
    assert!(record.rejected_fields.is_empty());
    assert_eq!(record.version_control, 0x5739_A7C1);
    assert_eq!(record.header_unknown, 0x39B7);
    let data = values(record, "perk_data")[0];
    assert_eq!(member(data, "trait"), &Value::Unsigned(1));
    assert_eq!(member(data, "level"), &Value::Unsigned(23));
    assert_eq!(member(data, "rank_count"), &Value::Unsigned(3));
    assert_eq!(member(data, "playable"), &Value::Unsigned(0));
    assert_eq!(member(data, "hidden"), &Value::Unsigned(1));
    let effects = values(record, "effect_data");
    assert_eq!(effects.len(), 18);
    assert_eq!(member(effects[0], "quest"), &Value::FormId(perks::QUEST_ID));
    assert_eq!(member(effects[0], "stage"), &Value::Unsigned(37));
    assert_eq!(
        member(effects[0], "unused"),
        &Value::Bytes(vec![0xA7, 0x53, 0xC1])
    );
    assert_eq!(effects[1], &Value::FormId(perks::SPELL_ID));
    for function in 0..=15usize {
        let entry = effects[function + 2];
        assert_eq!(
            member(entry, "entry_point"),
            &Value::Unsigned((85 - function) as u64)
        );
        assert_eq!(member(entry, "function"), &Value::Unsigned(function as u64));
    }
    let parameters = values(record, "function_parameters");
    assert_eq!(parameters.len(), 14);
    assert_eq!(parameters[0], &Value::Bytes(vec![0xA7, 0x53]));
    assert_eq!(parameters[1], &Value::Float(3.375));
    assert_eq!(parameters[2], &Value::Float(-7.75));
    assert_eq!(parameters[3], &Value::Float(2.125));
    assert_eq!(member(parameters[4], "first"), &Value::Float(-3.75));
    assert_eq!(member(parameters[4], "second"), &Value::Float(17.125));
    for (index, actor_value, multiplier) in [
        (5, 24, 1.875),
        (10, 7, -1.125),
        (11, 42, 2.25),
        (12, 47, 0.625),
    ] {
        assert_eq!(
            member(parameters[index], "actor_value"),
            &Value::Unsigned(actor_value)
        );
        assert_eq!(
            member(parameters[index], "multiplier"),
            &Value::Float(multiplier)
        );
    }
    assert_eq!(parameters[6], &Value::FormId(perks::LIST_ID));
    assert_eq!(parameters[7], &Value::FormId(perks::SPELL_ID));
    assert_eq!(parameters[8], &Value::FormId(perks::SPELL_ID));
    assert_eq!(parameters[9], &Value::String("Actor graph variable".into()));
    assert_eq!(
        parameters[13],
        &Value::String("Distinct activation text".into())
    );
    let flags = values(record, "script_flags")[0];
    assert_eq!(member(flags, "flags"), &Value::Unsigned(0x8003));
    assert_eq!(member(flags, "fragment_index"), &Value::Unsigned(0x0B17));
    assert_eq!(
        values(record, "button_label")[0],
        &Value::String("Run independent choice".into())
    );
    let all = &result.records[&(perks::FIRST_ID + 1)];
    let entry_points = values(all, "effect_data");
    assert_eq!(entry_points.len(), 92);
    for (number, entry) in entry_points.iter().enumerate() {
        assert_eq!(
            member(entry, "entry_point"),
            &Value::Unsigned(number as u64)
        );
    }
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 0);
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_records, 0);
}

/// Different local/global normal slots and a light master prove links alone are rewritten.
#[test]
fn perk_links_remap_across_normal_and_light_plugins_without_touching_padding_or_values() {
    let mut fixture = Fixture::new(&perks::targets());
    fixture.plugin("Interposed.esp", 0, &[], &[]);
    fixture.plugin("OffsetTwo.esp", 0, &[], &[]);
    fixture.plugin(
        "LightTarget.esl",
        0x200,
        &[],
        &perks::record(
            b"SPEL",
            0x8A3,
            0,
            &perks::subrecord(b"EDID", &perks::text("LightSpell")),
        ),
    );
    let mut payload = perks::prefix(11);
    payload.extend(perks::subrecord(b"NNAM", &0x0200_2700u32.to_le_bytes()));
    payload.extend(perks::quest_effect(0x0100_0000 | perks::QUEST_ID));
    payload.extend(perks::ability_effect(0x0000_08A3));
    payload.extend(perks::entry_effect(
        51,
        10,
        5,
        Some(&0x0000_08A3u32.to_le_bytes()),
        None,
    ));
    payload.extend(perks::entry_effect(
        9,
        8,
        3,
        Some(&(0x0100_0000 | perks::LIST_ID).to_le_bytes()),
        None,
    ));
    let floats = [
        f32::from_bits(0x0000_08A3).to_le_bytes(),
        (-6.25f32).to_le_bytes(),
    ]
    .concat();
    payload.extend(perks::entry_effect(35, 4, 2, Some(&floats), None));
    let actor_value = [24u32.to_le_bytes(), 1.625f32.to_le_bytes()].concat();
    payload.extend(perks::entry_effect(85, 5, 2, Some(&actor_value), None));
    fixture.plugin(
        "PerkLinks.esp",
        0,
        &["LightTarget.esl", "Skyrim.esm"],
        &perks::record(b"PERK", 0x0200_2700, 0, &payload),
    );
    let result = fixture.read();
    let record = &result.records[&0x0300_2700];
    assert_eq!(values(record, "next_perk")[0], &Value::FormId(0x0300_2700));
    let effects = values(record, "effect_data");
    assert_eq!(member(effects[0], "quest"), &Value::FormId(perks::QUEST_ID));
    assert_eq!(
        member(effects[0], "unused"),
        &Value::Bytes(vec![0xA7, 0x53, 0xC1])
    );
    assert_eq!(effects[1], &Value::FormId(0xFE00_08A3));
    let parameters = values(record, "function_parameters");
    assert_eq!(parameters[0], &Value::FormId(0xFE00_08A3));
    assert_eq!(parameters[1], &Value::FormId(perks::LIST_ID));
    assert_eq!(
        member(parameters[2], "first"),
        &Value::Float(f32::from_bits(0x0000_08A3))
    );
    assert_eq!(member(parameters[2], "second"), &Value::Float(-6.25));
    assert_eq!(member(parameters[3], "actor_value"), &Value::Unsigned(24));
    assert_eq!(member(parameters[3], "multiplier"), &Value::Float(1.625));
    let quest = record
        .fields
        .iter()
        .find(|field| field.name == "effect_data")
        .unwrap();
    assert_eq!(&quest.canonical_bytes[..4], &perks::QUEST_ID.to_le_bytes());
    assert_eq!(&quest.canonical_bytes[4..], &[37, 0xA7, 0x53, 0xC1]);
    assert_eq!(result.diagnostics["perklinks.esp"].invalid_links, 0);
    assert!(record.rejected_fields.is_empty());
}

/// Root and nested conditions keep their raw pending bytes and signed tab boundaries.
#[test]
fn perk_condition_tabs_preserve_repeated_groups_and_pending_condition_bytes() {
    let mut payload = perks::subrecord(b"EDID", &perks::text("ConditionBoundaryPerk"));
    payload.extend(perks::subrecord(b"CTDA", &[0xA7; 32]));
    payload.extend(perks::subrecord(b"CIS1", &perks::text("root-one")));
    payload.extend(perks::subrecord(b"DATA", &[0, 9, 2, 1, 0]));
    for (function, first_tab, fill) in [(4, -1i8, 0x53u8), (5, 2, 0xC1)] {
        payload.extend(perks::subrecord(b"PRKE", &[2, 3, 17]));
        payload.extend(perks::subrecord(b"DATA", &[35, function, 2]));
        for (tab, condition) in [(first_tab, fill), (first_tab + 1, fill + 1)] {
            payload.extend(perks::subrecord(b"PRKC", &tab.to_le_bytes()));
            payload.extend(perks::subrecord(b"CTDA", &[condition; 32]));
            payload.extend(perks::subrecord(b"CIS1", &perks::text("condition-one")));
            payload.extend(perks::subrecord(b"CIS2", &perks::text("condition-two")));
        }
        payload.extend(perks::subrecord(b"EPFT", &[2]));
        let data = if function == 4 {
            [2.75f32.to_le_bytes(), (-9.125f32).to_le_bytes()].concat()
        } else {
            [24u32.to_le_bytes(), 1.875f32.to_le_bytes()].concat()
        };
        payload.extend(perks::subrecord(b"EPFD", &data));
        payload.extend(perks::subrecord(b"PRKF", &[]));
    }
    let fixture = Fixture::new(&perks::record(b"PERK", perks::FIRST_ID, 0, &payload));
    let result = fixture.read();
    let record = &result.records[&perks::FIRST_ID];
    assert_eq!(
        values(record, "condition_tab"),
        [
            &Value::Signed(-1),
            &Value::Signed(0),
            &Value::Signed(2),
            &Value::Signed(3)
        ]
    );
    let conditions = values(record, "pending_condition");
    assert_eq!(
        conditions,
        [0xA7, 0x53, 0x54, 0xC1, 0xC2]
            .iter()
            .map(|fill| Value::Bytes(vec![*fill; 32]))
            .collect::<Vec<_>>()
            .iter()
            .collect::<Vec<_>>()
    );
    let parameters = values(record, "function_parameters");
    assert_eq!(member(parameters[0], "first"), &Value::Float(2.75));
    assert_eq!(member(parameters[1], "actor_value"), &Value::Unsigned(24));
    assert!(record.rejected_fields.is_empty());
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 0);
}

/// Selector dependencies never borrow an EPFT from a completed previous effect.
#[test]
fn missing_or_invalid_parameter_tags_drop_only_the_unsafe_field() {
    let mut payload = perks::prefix(5);
    payload.extend(perks::entry_effect(
        38,
        1,
        1,
        Some(&3.75f32.to_le_bytes()),
        None,
    ));
    payload.extend(perks::subrecord(b"PRKE", &[2, 1, 9]));
    payload.extend(perks::subrecord(b"DATA", &[51, 10, 0]));
    payload.extend(perks::subrecord(b"EPFD", &0x0700_0001u32.to_le_bytes()));
    payload.extend(perks::subrecord(b"PRKF", &[]));
    payload.extend(perks::entry_effect(
        35,
        1,
        8,
        Some(&9.125f32.to_le_bytes()),
        None,
    ));
    payload.extend(perks::entry_effect(
        36,
        1,
        1,
        Some(&(-5.875f32).to_le_bytes()),
        None,
    ));
    let fixture = Fixture::new(&perks::record(b"PERK", perks::FIRST_ID, 0, &payload));
    let result = fixture.read();
    let record = &result.records[&perks::FIRST_ID];
    assert_eq!(
        values(record, "function_parameters"),
        [&Value::Float(3.75), &Value::Float(-5.875)]
    );
    assert_eq!(record.rejected_fields, [*b"EPFD"]);
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_fields, 2);
    assert_eq!(result.diagnostics["skyrim.esm"].skipped_records, 0);
}

/// EPFD text stays inline while EPFD localized text and the activation label retain IDs.
#[test]
fn localized_perk_parameters_distinguish_inline_text_and_string_ids() {
    let mut fixture = Fixture::new(&perks::targets());
    let mut payload = perks::subrecord(b"EDID", &perks::text("LocalizedParameterPerk"));
    payload.extend(perks::subrecord(b"FULL", &101u32.to_le_bytes()));
    payload.extend(perks::subrecord(b"DESC", &202u32.to_le_bytes()));
    payload.extend(perks::subrecord(b"DATA", &[0, 17, 2, 1, 0]));
    payload.extend(perks::entry_effect(
        54,
        11,
        6,
        Some(&perks::text("Inline graph name")),
        None,
    ));
    payload.extend(perks::entry_effect(
        81,
        15,
        7,
        Some(&404u32.to_le_bytes()),
        None,
    ));
    payload.extend(perks::entry_effect(
        14,
        9,
        4,
        Some(&perks::SPELL_ID.to_le_bytes()),
        Some(&303u32.to_le_bytes()),
    ));
    fixture.plugin(
        "LocalizedPerk.esp",
        0x80,
        &["Skyrim.esm"],
        &perks::record(b"PERK", 0x0100_2800, 0, &payload),
    );
    let result = fixture.read();
    let record = &result.records[&0x0100_2800];
    assert_eq!(values(record, "name")[0], &Value::LocalizedString(101));
    assert_eq!(
        values(record, "description")[0],
        &Value::LocalizedString(202)
    );
    assert_eq!(
        values(record, "button_label")[0],
        &Value::LocalizedString(303)
    );
    assert_eq!(
        values(record, "function_parameters"),
        [
            &Value::String("Inline graph name".into()),
            &Value::LocalizedString(404),
            &Value::FormId(perks::SPELL_ID)
        ]
    );
    assert!(record.rejected_fields.is_empty());
    assert_eq!(result.diagnostics["localizedperk.esp"].skipped_fields, 0);
}

/// Real conversion publishes usable neighbors and prior winners around malformed PERK data.
#[tokio::test]
async fn damaged_perks_publish_neighbors_and_prior_winners_with_bounded_diagnostics() {
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
    let mut base_bytes = fs::read(&base_path).unwrap();
    let header_size = u32::from_le_bytes(base_bytes[4..8].try_into().unwrap()) as usize;
    let mut header = base_bytes[24..24 + header_size].to_vec();
    header.extend(perks::subrecord(b"MAST", b"Skyrim.esm\0"));
    header.extend(perks::subrecord(b"DATA", &[0xA5; 8]));
    base_bytes.extend(perks::targets());
    base_bytes.extend(perks::complete(perks::FIRST_ID));
    base_bytes.extend(perks::complete(perks::FIRST_ID + 1));
    fs::write(&base_path, base_bytes).unwrap();
    let earlier = read_plugins(
        std::slice::from_ref(&base_path),
        &LoadOrder::read(std::slice::from_ref(&base_path)).unwrap(),
    )
    .unwrap();
    let mut patch = perks::record(b"TES4", 0, 0, &header);
    patch.extend(perks::record(b"PERK", 0x0100_2700, 0, &perks::prefix(43)));
    // A bad mandatory effect DATA rejects this candidate, leaving its earlier base winner.
    let mut bad_effect = perks::prefix(51);
    bad_effect.extend(perks::subrecord(b"PRKE", &[0, 1, 7]));
    bad_effect.extend(perks::subrecord(b"DATA", &[0xA7; 7]));
    bad_effect.extend(perks::subrecord(b"PRKF", &[]));
    patch.extend(perks::record(b"PERK", perks::FIRST_ID, 0, &bad_effect));
    // Framing is safe, but no complete zlib stream exists for this override candidate.
    patch.extend(perks::record(
        b"PERK",
        perks::FIRST_ID + 1,
        0x40000,
        &[24, 0, 0, 0, 99],
    ));
    // A malformed top-level DATA has its own safe record boundary and is skipped.
    patch.extend(perks::record(
        b"PERK",
        0x0100_2701,
        0,
        &perks::subrecord(b"DATA", &[1, 9, 1, 0]),
    ));
    // A malformed optional parameter omits only EPFD; its following good effect survives.
    let mut partial = perks::prefix(67);
    partial.extend(perks::entry_effect(35, 1, 1, Some(&[0xA7; 3]), None));
    partial.extend(perks::entry_effect(
        36,
        1,
        1,
        Some(&(-8.375f32).to_le_bytes()),
        None,
    ));
    patch.extend(perks::record(b"PERK", 0x0100_2702, 0, &partial));
    // A broken optional master index becomes null; following typed parameters remain usable.
    let mut bad_link = perks::prefix(71);
    bad_link.extend(perks::ability_effect(0x0700_0001));
    bad_link.extend(perks::entry_effect(
        38,
        1,
        1,
        Some(&2.875f32.to_le_bytes()),
        None,
    ));
    patch.extend(perks::record(b"PERK", 0x0100_2703, 0, &bad_link));
    patch.extend(perks::record(b"PERK", 0x0100_2704, 0, &perks::prefix(79)));
    let patch_path = data.join("DamagedPerks.esp");
    fs::write(&patch_path, patch).unwrap();
    let paths = [base_path, patch_path];
    let recovered = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
    let warnings = &recovered.diagnostics["damagedperks.esp"];
    assert_eq!(warnings.skipped_records, 3);
    assert_eq!(warnings.skipped_fields, 1);
    assert_eq!(warnings.invalid_links, 1);
    assert!(warnings.first_by_category["record"].contains("PERK"));
    assert!(warnings.first_by_category["field"].contains("EPFD"));
    assert!(warnings.first_by_category["link"].contains("07000001"));
    for id in [perks::FIRST_ID, perks::FIRST_ID + 1] {
        assert_eq!(recovered.records[&id].load_order, 0);
        assert_eq!(
            recovered.records[&id].raw_payload,
            earlier.records[&id].raw_payload
        );
        assert!(!recovered.overrides.contains_key(&id));
    }
    assert!(!recovered.records.contains_key(&0x0100_2701));
    assert_eq!(
        values(&recovered.records[&0x0100_2702], "function_parameters"),
        [&Value::Float(-8.375)]
    );
    assert_eq!(
        values(&recovered.records[&0x0100_2703], "effect_data")[0],
        &Value::FormId(0)
    );
    let plugins = temp.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n*DamagedPerks.esp\n").unwrap();
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
    for id in [perks::FIRST_ID, perks::FIRST_ID + 1] {
        let (load_order,payload):(i64,Vec<u8>) = database.query_row(
            "SELECT source.load_order,source.payload FROM inhouse_source_records source JOIN records ON records.form_id=source.form_id WHERE source.form_id=?",
            [id],|row| Ok((row.get(0)?,row.get(1)?)),
        ).unwrap();
        assert_eq!(load_order, 0);
        assert_eq!(payload, earlier.records[&id].raw_payload);
    }
    for id in [0x0100_2700, 0x0100_2702, 0x0100_2703, 0x0100_2704] {
        let count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "missing usable neighbor {id:08X}");
    }
    let missing: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM records WHERE form_id=0x01002701",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(missing, 0);
    let published: Vec<u8> = database
        .query_row(
            "SELECT data FROM records WHERE form_id=0x01002702",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let canonical = recovered.records[&0x0100_2702].to_raw_record();
    assert_eq!(
        published,
        converter::esm::extractors::serialize_subrecords(&canonical.subrecords)
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["damagedperks.esp"]["skipped_records"],
        3
    );
    assert_eq!(
        diagnostics["decoder"]["damagedperks.esp"]["skipped_fields"],
        1
    );
    assert_eq!(
        diagnostics["decoder"]["damagedperks.esp"]["invalid_links"],
        1
    );
}
