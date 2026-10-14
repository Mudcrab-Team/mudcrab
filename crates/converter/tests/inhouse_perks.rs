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

/// Root and nested CTDA preserve tab boundaries, typed links and all native non-link bytes.
#[test]
fn perk_condition_tabs_preserve_typed_groups_and_remapped_native_bytes() {
    use dummy_content::inhouse_conditions as native;
    let full = 0x0100_2731u32;
    let light = 0x0000_08A3u32;
    let global = 0x0100_2749u32;
    let mut source_conditions = Vec::new();
    for index in 0..5usize {
        let first = if index % 2 == 0 { full } else { light };
        let reference = if index % 2 == 0 { light } else { full };
        let flags = [4, 1, 32, 65, 96][index];
        let comparison = if index == 0 {
            global
        } else {
            (-3.125f32 - index as f32).to_bits()
        };
        let mut bytes = native::condition(
            flags,
            comparison,
            1,
            first,
            0xFEA7_5300 + index as u32,
            2,
            reference,
        );
        bytes[1] = 0x91 + index as u8;
        bytes[3] = 0xD5 - index as u8;
        bytes[11] = 0x53 + index as u8;
        bytes[28..32].copy_from_slice(&(-17i32 - index as i32).to_le_bytes());
        source_conditions.push(bytes);
    }
    let mut payload = perks::subrecord(b"EDID", &perks::text("ConditionBoundaryPerk"));
    payload.extend(perks::subrecord(b"CTDA", &source_conditions[0]));
    payload.extend(perks::subrecord(b"CIS1", &perks::text("root-one")));
    payload.extend(perks::subrecord(b"DATA", &[0, 9, 2, 1, 0]));
    for (effect, function, first_tab) in [(0usize, 4, -1i8), (1, 5, 2)] {
        payload.extend(perks::subrecord(b"PRKE", &[2, 3, 17]));
        payload.extend(perks::subrecord(b"DATA", &[35, function, 2]));
        for ordinal in 0..2usize {
            payload.extend(perks::subrecord(
                b"PRKC",
                &(first_tab + ordinal as i8).to_le_bytes(),
            ));
            payload.extend(perks::subrecord(
                b"CTDA",
                &source_conditions[1 + effect * 2 + ordinal],
            ));
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
    let mut fixture = Fixture::new(&[]);
    fixture.plugin("Interposed.esp", 0, &[], &[]);
    fixture.plugin(
        "ConditionObjects.esm",
        1,
        &[],
        &[native::reference(0x2731), native::global(0x2749)].concat(),
    );
    fixture.plugin(
        "ConditionLight.esl",
        0x200,
        &["Skyrim.esm"],
        &native::reference_with_base(0x0100_08A3, 0x0000_0014),
    );
    fixture.plugin(
        "ConditionPerks.esp",
        0,
        &["ConditionLight.esl", "ConditionObjects.esm", "Skyrim.esm"],
        &perks::record(b"PERK", 0x0300_2600, 0, &payload),
    );
    let result = fixture.read();
    let record = &result.records[&0x0300_2600];
    assert_eq!(record.raw_payload, payload);
    assert_eq!(
        values(record, "condition_tab"),
        [
            &Value::Signed(-1),
            &Value::Signed(0),
            &Value::Signed(2),
            &Value::Signed(3)
        ]
    );
    let conditions: Vec<_> = record
        .fields
        .iter()
        .filter(|field| field.signature == *b"CTDA")
        .collect();
    assert_eq!(conditions.len(), 5);
    for (index, field) in conditions.iter().enumerate() {
        assert_eq!(field.name, "condition");
        let first = if index % 2 == 0 {
            0x0200_2731u32
        } else {
            0xFE00_08A3
        };
        let reference = if index % 2 == 0 {
            0xFE00_08A3u32
        } else {
            0x0200_2731
        };
        assert_eq!(member(&field.value, "function_index"), &Value::Unsigned(1));
        assert_eq!(
            member(&field.value, "flags_and_operator"),
            &Value::Unsigned([4, 1, 32, 65, 96][index])
        );
        assert_eq!(member(&field.value, "parameter_1"), &Value::FormId(first));
        assert_eq!(
            member(&field.value, "parameter_2"),
            &Value::Bytes((0xFEA7_5300u32 + index as u32).to_le_bytes().to_vec())
        );
        assert_eq!(member(&field.value, "reference"), &Value::FormId(reference));
        assert_eq!(
            member(&field.value, "parameter_3"),
            &Value::Signed(-17 - index as i64)
        );
        assert_eq!(
            member(&field.value, "comparison"),
            &if index == 0 {
                Value::FormId(0x0200_2749)
            } else {
                Value::Float(-3.125 - index as f32)
            }
        );
        let mut expected = source_conditions[index].clone();
        expected[12..16].copy_from_slice(&first.to_le_bytes());
        expected[24..28].copy_from_slice(&reference.to_le_bytes());
        if index == 0 {
            expected[4..8].copy_from_slice(&0x0200_2749u32.to_le_bytes());
        }
        assert_eq!(
            field.canonical_bytes, expected,
            "CTDA {index}: only known link spans change"
        );
    }
    let grouped: Vec<_> = record
        .fields
        .iter()
        .filter(|field| matches!(&field.signature, b"PRKE" | b"PRKC" | b"CTDA" | b"PRKF"))
        .map(|field| field.signature)
        .collect();
    assert_eq!(
        grouped,
        [
            *b"CTDA", *b"PRKE", *b"PRKC", *b"CTDA", *b"PRKC", *b"CTDA", *b"PRKF", *b"PRKE",
            *b"PRKC", *b"CTDA", *b"PRKC", *b"CTDA", *b"PRKF"
        ]
    );
    let strings: Vec<_> = record
        .fields
        .iter()
        .filter(|field| matches!(&field.signature, b"CIS1" | b"CIS2"))
        .map(|field| field.value.clone())
        .collect();
    assert_eq!(
        strings,
        [
            "root-one",
            "condition-one",
            "condition-two",
            "condition-one",
            "condition-two",
            "condition-one",
            "condition-two",
            "condition-one",
            "condition-two"
        ]
        .map(|text| Value::String(text.into()))
    );
    let parameters = values(record, "function_parameters");
    assert_eq!(member(parameters[0], "first"), &Value::Float(2.75));
    assert_eq!(member(parameters[1], "actor_value"), &Value::Unsigned(24));
    assert!(record.rejected_fields.is_empty());
    let diagnostics = &result.diagnostics["conditionperks.esp"];
    assert_eq!(diagnostics.skipped_fields, 0);
    assert_eq!(diagnostics.skipped_records, 0);
    assert_eq!(diagnostics.unexpected_subrecords, 0);
    assert_eq!(diagnostics.invalid_links, 0);
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
