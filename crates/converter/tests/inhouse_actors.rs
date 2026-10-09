//! Independent native actor fixtures exercised through the public reader.
//! Format facts: pinned TES5 ACBS24, PLVD12, CRVA optional tails, RACE
//! gender/repeat sections, and common CNTO/COED owner-dependent values.
use converter::{
    esm::load_order::LoadOrder,
    records::{DecodedField, DecodedRecord, ReadResult, Value, read_plugins},
};
use dummy_content::{esm, inhouse_actors as native, layout};
use std::{fs, path::PathBuf};

/// Real generated world metadata followed by independently written actor plugins.
struct Fixture {
    directory: tempfile::TempDir,
    generated: Vec<u8>,
    paths: Vec<PathBuf>,
}

impl Fixture {
    /// Preserve the existing dummy world as the first full slot.
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

    /// Add an independent native plugin with its own MAST order.
    fn add(&mut self, name: &str, masters: &[&str], flags: u32, records: &[u8]) {
        let path = self.directory.path().join(name);
        fs::write(
            &path,
            native::plugin(&self.generated, masters, flags, records),
        )
        .unwrap();
        self.paths.push(path);
    }

    /// Use the engine's single load-order authority and the public typed API.
    fn read(&self) -> (LoadOrder, ReadResult) {
        let order = LoadOrder::read(&self.paths).unwrap();
        let result = read_plugins(&self.paths, &order).unwrap();
        for record in result.records.values() {
            for field in &record.fields {
                assert_resolved(&field.value);
            }
        }
        (order, result)
    }
}

/// A final internal deferred marker is a failed resolution, never a passing fixture.
fn assert_resolved(value: &Value) {
    match value {
        Value::Deferred(_) => panic!("unresolved final actor value"),
        Value::Struct(members) => members.iter().for_each(|(_, value)| assert_resolved(value)),
        Value::Array(values) => values.iter().for_each(assert_resolved),
        _ => {}
    }
}

/// Native supplemental record with a synthetic editor ID and explicit fields.
fn actor(kind: &[u8; 4], id: u32, fields: &[Vec<u8>]) -> Vec<u8> {
    let payload = [
        native::subrecord(b"EDID", &native::text(&format!("Actor{id:X}"))),
        fields.concat(),
    ]
    .concat();
    native::record(kind, id, 0, &payload)
}

/// Find one physical occurrence while keeping repetitions observable.
fn field<'a>(record: &'a DecodedRecord, tag: &[u8; 4]) -> &'a DecodedField {
    record
        .fields
        .iter()
        .find(|field| &field.signature == tag)
        .unwrap_or_else(|| {
            panic!(
                "missing {:?} in {:08X}: {:?}",
                tag, record.form_id, record.rejected_fields
            )
        })
}

/// Read a named struct member without depending on production decoding helpers.
fn member<'a>(value: &'a Value, name: &str) -> &'a Value {
    let Value::Struct(members) = value else {
        panic!("expected {name} struct: {value:?}");
    };
    &members
        .iter()
        .find(|(key, _)| key == name)
        .unwrap_or_else(|| panic!("missing {name}: {value:?}"))
        .1
}

/// Independently inspect one native word in canonical bytes.
fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

/// Distinct widths/values cover every assigned type through the assembled schema.
#[test]
fn all_fourteen_actor_types_decode_distinct_native_members() {
    let mut fixture = Fixture::new();
    fixture.add("Actors.esm", &[], 1, &native::representatives());
    let (_, result) = fixture.read();
    let expected = [
        b"NPC_", b"RACE", b"CLAS", b"FACT", b"OTFT", b"HDPT", b"EYES", b"VTYP", b"CSTY", b"BPTD",
        b"RELA", b"ASTP", b"LCRT", b"MOVT",
    ];
    for (index, kind) in expected.iter().enumerate() {
        let record = &result.records[&(0x0100_2400 + index as u32)];
        assert_eq!(&record.record_type, *kind);
        assert!(record.supported);
        assert!(record.rejected_fields.is_empty(), "{kind:?}");
    }
    let class = field(&result.records[&0x0100_2402], b"DATA");
    assert_eq!(class.canonical_bytes.len(), 36);
    assert_eq!(member(&class.value, "training_skill"), &Value::Signed(7));
    assert_eq!(
        member(&class.value, "voice_points"),
        &Value::Unsigned(0x0123_4567)
    );
    assert_eq!(
        member(&class.value, "bleedout_default"),
        &Value::Float(0.3125)
    );
    assert_eq!(
        field(&result.records[&0x0100_2406], b"DATA").value,
        Value::Unsigned(5)
    );
    assert_eq!(
        field(&result.records[&0x0100_2407], b"DNAM").value,
        Value::Unsigned(3)
    );
    let body = field(&result.records[&0x0100_2409], b"BPND");
    assert_eq!(member(&body.value, "actor_value"), &Value::Signed(-1));
    assert_eq!(
        member(&body.value, "severable_debris_count"),
        &Value::Signed(-17)
    );
    assert_eq!(member(&body.value, "gore_rot_y"), &Value::Float(-0.5));
    assert_eq!(&body.canonical_bytes[76..80], &[13, 19, 0xa5, 0x5a]);
    let color = field(&result.records[&0x0100_240c], b"CNAM");
    assert_eq!(color.canonical_bytes, [19, 71, 133, 211]);
    let speeds = field(&result.records[&0x0100_240d], b"SPED");
    assert_eq!(
        member(&speeds.value, "rotate_while_moving_run"),
        &Value::Float(1.25)
    );
    assert_eq!(result.diagnostics["actors.esm"].skipped_fields, 0);
}

/// ACBS flag0x80 changes the level role, preserving its signed native integer.
#[test]
fn npc_level_selector_preserves_native_thousandths_and_signed_offsets() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Levels.esm",
        &[],
        1,
        &[
            actor(
                b"NPC_",
                0x2500,
                &[native::subrecord(
                    b"ACBS",
                    &native::npc_configuration(false),
                )],
            ),
            actor(
                b"NPC_",
                0x2501,
                &[native::subrecord(b"ACBS", &native::npc_configuration(true))],
            ),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    for (id, role) in [
        (0x0100_2500, "level"),
        (0x0100_2501, "level_multiplier_thousandths"),
    ] {
        let config = field(&result.records[&id], b"ACBS");
        assert_eq!(member(&config.value, role), &Value::Signed(1500));
        assert_eq!(member(&config.value, "magicka_offset"), &Value::Signed(-21));
        assert_eq!(
            member(&config.value, "unused_disposition"),
            &Value::Signed(-3)
        );
        assert_eq!(
            member(&config.value, "speed_multiplier"),
            &Value::Unsigned(115)
        );
        assert_eq!(
            member(&config.value, "template_flags"),
            &Value::Unsigned(0x1201)
        );
    }
}

/// A light NPC owner selects GLOB; a full FACT owner selects signed rank.
#[test]
fn inventory_extra_uses_accepted_owner_kind_and_divergent_full_light_slots() {
    let mut fixture = Fixture::new();
    fixture.add("Unused.esm", &[], 1, &[]);
    fixture.add(
        "Owner.esm",
        &[],
        1,
        &[
            actor(b"FACT", 0x1500, &[]),
            actor(b"GLOB", 0x1501, &[]),
            actor(b"MISC", 0x1502, &[]),
            actor(b"NPC_", 0x1503, &[]),
        ]
        .concat(),
    );
    fixture.add("Light.esl", &[], 0x201, &actor(b"NPC_", 0xabc, &[]));
    // The later accepted kind of 1503 is FACT, not the speculative base NPC.
    fixture.add("Kind.esp", &["Owner.esm"], 0, &actor(b"FACT", 0x1503, &[]));
    let items = [
        native::inventory(0x1502, 0x0100_0abc, 0x1501, 17),
        native::inventory(0x1502, 0x1500, (-4i32) as u32, -9),
        native::inventory(0x1502, 0, 0xa5b6_c7d8, 3),
        native::inventory(0x1502, 0x1503, (-7i32) as u32, 5),
    ]
    .concat();
    fixture.add(
        "Consumer.esp",
        &["Owner.esm", "Light.esl"],
        0,
        &actor(
            b"NPC_",
            0x0200_1800,
            &[native::subrecord(b"COCT", &4u32.to_le_bytes()), items],
        ),
    );
    let (order, result) = fixture.read();
    assert_eq!(order.normal["owner.esm"], 2);
    assert_eq!(order.light["light.esl"], 0);
    let record = &result.records[&0x0400_1800];
    let extras: Vec<_> = record
        .fields
        .iter()
        .filter(|field| field.signature == *b"COED")
        .collect();
    assert_eq!(extras.len(), 4);
    assert_eq!(word(&extras[0].canonical_bytes, 0), 0xfe00_0abc);
    assert_eq!(word(&extras[0].canonical_bytes, 4), 0x0200_1501);
    assert_eq!(word(&extras[1].canonical_bytes, 4), (-4i32) as u32);
    assert_eq!(word(&extras[2].canonical_bytes, 4), 0xa5b6_c7d8);
    assert_eq!(word(&extras[3].canonical_bytes, 4), (-7i32) as u32);
    assert_eq!(
        member(&extras[1].value, "global_or_rank"),
        &Value::Signed(-4)
    );
    assert_eq!(result.diagnostics["consumer.esp"].invalid_links, 0);
}

/// PLVD uses its native discriminator, not merely the target's existence.
#[test]
fn faction_vendor_location_link_numeric_and_alias_variants_remain_distinct() {
    let mut fixture = Fixture::new();
    fixture.add("Unused.esm", &[], 1, &[]);
    fixture.add(
        "Owner.esm",
        &[],
        1,
        &[
            actor(b"REFR", 0x1500, &[]),
            actor(b"CELL", 0x1501, &[]),
            actor(b"MISC", 0x1502, &[]),
            actor(b"KYWD", 0x1503, &[]),
        ]
        .concat(),
    );
    let cases = [
        (0, 0x1500),
        (1, 0x1501),
        (4, 0x1502),
        (5, 27),
        (6, 0x1503),
        (8, (-2i32) as u32),
        (9, (-13i32) as u32),
        (2, 0xaabb_ccdd),
        (0, 0x1502),
    ];
    let records: Vec<_> = cases
        .iter()
        .enumerate()
        .flat_map(|(index, (kind, value))| {
            actor(
                b"FACT",
                0x0100_2600 + index as u32,
                &[native::subrecord(
                    b"PLVD",
                    &native::vendor_location(*kind, *value),
                )],
            )
        })
        .collect();
    fixture.add("Vendors.esp", &["Owner.esm"], 0, &records);
    let (_, result) = fixture.read();
    for (index, expected) in [
        0x0200_1500,
        0x0200_1501,
        0x0200_1502,
        27,
        0x0200_1503,
        (-2i32) as u32,
        (-13i32) as u32,
        0xaabb_ccdd,
        0,
    ]
    .into_iter()
    .enumerate()
    {
        let location = field(&result.records[&(0x0300_2600 + index as u32)], b"PLVD");
        assert_eq!(word(&location.canonical_bytes, 4), expected);
        assert_eq!(word(&location.canonical_bytes, 8), (-73i32) as u32);
    }
    assert_eq!(result.diagnostics["vendors.esp"].invalid_links, 1);
}

/// Short whole members pass; an interior partial float stays local to its field.
#[test]
fn optional_tails_keep_native_lengths_and_reject_partial_members() {
    let mut fixture = Fixture::new();
    let short = native::floats(&[0.125, 0.375]);
    let full = native::floats(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
    let mut partial = short.clone();
    partial.push(0xa5);
    fixture.add(
        "Tails.esm",
        &[],
        1,
        &[
            actor(b"CSTY", 0x2700, &[native::subrecord(b"CSGD", &short)]),
            actor(b"CSTY", 0x2701, &[native::subrecord(b"CSGD", &full)]),
            actor(
                b"CSTY",
                0x2702,
                &[
                    native::subrecord(b"CSGD", &partial),
                    native::subrecord(b"DATA", &5u32.to_le_bytes()),
                ],
            ),
            actor(b"MOVT", 0x2703, &[native::subrecord(b"SPED", &full)]),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    assert_eq!(
        field(&result.records[&0x0100_2700], b"CSGD").canonical_bytes,
        short
    );
    assert_eq!(
        field(&result.records[&0x0100_2701], b"CSGD").canonical_bytes,
        full
    );
    assert_eq!(
        field(&result.records[&0x0100_2703], b"SPED")
            .canonical_bytes
            .len(),
        40
    );
    assert_eq!(
        field(&result.records[&0x0100_2702], b"DATA").value,
        Value::Unsigned(5)
    );
    assert_eq!(result.records[&0x0100_2702].rejected_fields, [*b"CSGD"]);
    assert_eq!(result.diagnostics["tails.esm"].skipped_fields, 1);
}

/// Active male body repeats must not be mistaken for the following female group.
#[test]
fn race_gender_sections_repeat_without_binding_to_future_roles() {
    let mut fixture = Fixture::new();
    let mut fields = vec![
        native::subrecord(b"NAM1", &[]),
        native::subrecord(b"MNAM", &[]),
    ];
    for (index, path) in [(3u32, "male-one.nif"), (7, "male-two.nif")] {
        fields.extend([
            native::subrecord(b"INDX", &index.to_le_bytes()),
            native::subrecord(b"MODL", &native::text(path)),
        ]);
    }
    fields.extend([
        native::subrecord(b"FNAM", &[]),
        native::subrecord(b"INDX", &11u32.to_le_bytes()),
        native::subrecord(b"MODL", b"female-only.nif\0"),
        native::subrecord(b"NAM3", &[]),
        native::subrecord(b"MNAM", &[]),
        native::subrecord(b"MODL", b"male-behavior.hkx\0"),
        native::subrecord(b"FNAM", &[]),
        native::subrecord(b"MODL", b"female-behavior.hkx\0"),
    ]);
    fixture.add("Sections.esm", &[], 1, &actor(b"RACE", 0x2800, &fields));
    let (_, result) = fixture.read();
    let record = &result.records[&0x0100_2800];
    let roles: Vec<_> = record
        .fields
        .iter()
        .filter(|field| field.signature == *b"MODL")
        .map(|field| (field.name.as_str(), &field.value))
        .collect();
    assert_eq!(
        roles,
        [
            (
                "male_body_part_model_path",
                &Value::String("male-one.nif".into())
            ),
            (
                "male_body_part_model_path",
                &Value::String("male-two.nif".into())
            ),
            (
                "female_body_part_model_path",
                &Value::String("female-only.nif".into())
            ),
            (
                "male_behavior_model_path",
                &Value::String("male-behavior.hkx".into())
            ),
            (
                "female_behavior_model_path",
                &Value::String("female-behavior.hkx".into())
            )
        ]
    );
    assert_eq!(result.diagnostics["sections.esm"].unexpected_subrecords, 0);
}

/// Malformed mandatory ACBS cannot replace a usable winner or lose valid neighbors.
#[test]
fn malformed_actor_override_preserves_prior_winner_deletion_and_restoration() {
    let mut fixture = Fixture::new();
    fixture.add(
        "Base.esm",
        &[],
        1,
        &actor(
            b"NPC_",
            0x2900,
            &[native::subrecord(
                b"ACBS",
                &native::npc_configuration(false),
            )],
        ),
    );
    fixture.add(
        "Broken.esp",
        &["Base.esm"],
        0,
        &[
            actor(b"EYES", 0x0100_2901, &[native::subrecord(b"DATA", &[1])]),
            actor(b"NPC_", 0x2900, &[native::subrecord(b"ACBS", &[0; 23])]),
            actor(b"EYES", 0x0100_2902, &[native::subrecord(b"DATA", &[4])]),
        ]
        .concat(),
    );
    let (_, prior) = fixture.read();
    assert_eq!(prior.records[&0x0100_2900].load_order, 1);
    assert!(prior.records.contains_key(&0x0200_2901));
    assert!(prior.records.contains_key(&0x0200_2902));
    assert_eq!(prior.diagnostics["broken.esp"].skipped_records, 1);
    fixture.add(
        "Deleted.esp",
        &["Base.esm"],
        0,
        &native::record(b"NPC_", 0x2900, 0x20, &[]),
    );
    let (_, deleted) = fixture.read();
    assert!(!deleted.records.contains_key(&0x0100_2900));
    fixture.add(
        "Restored.esp",
        &["Base.esm"],
        0,
        &actor(
            b"NPC_",
            0x2900,
            &[native::subrecord(b"ACBS", &native::npc_configuration(true))],
        ),
    );
    let (_, restored) = fixture.read();
    assert_eq!(restored.records[&0x0100_2900].load_order, 4);
    assert_eq!(restored.overrides[&0x0100_2900].len(), 3);
    assert!(restored.overrides[&0x0100_2900][1].deleted);
}

/// Interior compound links remap while adjacent ranks, flags and angles stay exact.
#[test]
fn npc_attacks_perks_factions_and_body_part_links_preserve_padding() {
    let mut fixture = Fixture::new();
    fixture.add("Unused.esm", &[], 1, &[]);
    let targets: Vec<_> = [
        b"FACT", b"SPEL", b"KYWD", b"PERK", b"DEBR", b"EXPL", b"IPDS",
    ]
    .into_iter()
    .enumerate()
    .flat_map(|(index, kind)| actor(kind, 0x1600 + index as u32, &[]))
    .collect();
    fixture.add("Owner.esm", &[], 1, &targets);
    let mut faction = 0x1600u32.to_le_bytes().to_vec();
    faction.extend([0xfc, 0xa1, 0xb2, 0xc3]);
    let mut perk = 0x1603u32.to_le_bytes().to_vec();
    perk.extend([7, 0xd4, 0xe5, 0xf6]);
    let mut attack = native::floats(&[0.625, 0.875]);
    attack.extend(0x1601u32.to_le_bytes());
    attack.extend(0x8000_0015u32.to_le_bytes());
    attack.extend(native::floats(&[0.25, -0.5, 1.25]));
    attack.extend(0x1602u32.to_le_bytes());
    attack.extend(native::floats(&[2.25, 3.25, 4.25]));
    let npc = actor(
        b"NPC_",
        0x0100_2a00,
        &[
            native::subrecord(b"SNAM", &faction),
            native::subrecord(b"ATKD", &attack),
            native::subrecord(b"ATKE", b"attackLeft\0"),
            native::subrecord(b"PRKZ", &1u32.to_le_bytes()),
            native::subrecord(b"PRKR", &perk),
        ],
    );
    let mut body = vec![0; 84];
    for (offset, id) in [
        (12, 0x1604u32),
        (16, 0x1605),
        (32, 0x1604),
        (36, 0x1605),
        (68, 0x1606),
        (72, 0x1606),
    ] {
        body[offset..offset + 4].copy_from_slice(&id.to_le_bytes());
    }
    body[44..68].copy_from_slice(&native::floats(&[1.25, -2.5, 3.75, 0.125, -0.375, 0.625]));
    body[76..80].copy_from_slice(&[7, 11, 0xa5, 0x5a]);
    fixture.add(
        "Compound.esp",
        &["Owner.esm"],
        0,
        &[
            npc,
            actor(
                b"BPTD",
                0x0100_2a01,
                &[
                    native::subrecord(b"BPTN", b"SyntheticHead\0"),
                    native::subrecord(b"BPND", &body),
                ],
            ),
        ]
        .concat(),
    );
    let (_, result) = fixture.read();
    let npc = &result.records[&0x0300_2a00];
    assert_eq!(word(&field(npc, b"SNAM").canonical_bytes, 0), 0x0200_1600);
    assert_eq!(
        &field(npc, b"SNAM").canonical_bytes[4..],
        &[0xfc, 0xa1, 0xb2, 0xc3]
    );
    assert_eq!(
        member(&field(npc, b"SNAM").value, "rank"),
        &Value::Signed(-4)
    );
    assert_eq!(word(&field(npc, b"ATKD").canonical_bytes, 8), 0x0200_1601);
    assert_eq!(word(&field(npc, b"ATKD").canonical_bytes, 28), 0x0200_1602);
    assert_eq!(word(&field(npc, b"ATKD").canonical_bytes, 12), 0x8000_0015);
    assert_eq!(
        member(&field(npc, b"ATKD").value, "strike_angle"),
        &Value::Float(-0.5)
    );
    assert_eq!(
        &field(npc, b"PRKR").canonical_bytes[4..],
        &[7, 0xd4, 0xe5, 0xf6]
    );
    let decoded = field(&result.records[&0x0300_2a01], b"BPND");
    for (offset, id) in [
        (12, 0x0200_1604),
        (16, 0x0200_1605),
        (32, 0x0200_1604),
        (36, 0x0200_1605),
        (68, 0x0200_1606),
        (72, 0x0200_1606),
    ] {
        assert_eq!(word(&decoded.canonical_bytes, offset), id);
    }
    assert_eq!(&decoded.canonical_bytes[44..68], &body[44..68]);
    assert_eq!(&decoded.canonical_bytes[76..80], &[7, 11, 0xa5, 0x5a]);
    assert_eq!(result.diagnostics["compound.esp"].invalid_links, 0);
}

/// Sequential PHWT roles retain both legacy and extended whole-tail layouts.
#[test]
fn race_phonemes_and_head_tints_preserve_occurrence_roles() {
    let mut fixture = Fixture::new();
    let mut fields = Vec::new();
    for index in 0..43 {
        let count = if index % 2 == 0 { 8 } else { 16 };
        let floats: Vec<_> = (0..count)
            .map(|member| index as f32 + member as f32 / 32.0)
            .collect();
        fields.push(native::subrecord(b"PHWT", &native::floats(&floats)));
    }
    fields.extend([
        native::subrecord(b"NAM0", &[]),
        native::subrecord(b"MNAM", &[]),
        native::subrecord(b"TINI", &3u16.to_le_bytes()),
        native::subrecord(b"TINT", b"male-tint.dds\0"),
        native::subrecord(b"TINP", &7u16.to_le_bytes()),
        native::subrecord(b"MODL", b"male-head.nif\0"),
        native::subrecord(b"NAM0", &[]),
        native::subrecord(b"FNAM", &[]),
        native::subrecord(b"TINI", &11u16.to_le_bytes()),
        native::subrecord(b"TINT", b"female-tint.dds\0"),
        native::subrecord(b"TINP", &13u16.to_le_bytes()),
        native::subrecord(b"MODL", b"female-head.nif\0"),
    ]);
    fixture.add("Faces.esm", &[], 1, &actor(b"RACE", 0x2b00, &fields));
    let (_, result) = fixture.read();
    let record = &result.records[&0x0100_2b00];
    let weights: Vec<_> = record
        .fields
        .iter()
        .filter(|field| field.signature == *b"PHWT")
        .collect();
    assert_eq!(weights.len(), 43);
    assert_eq!(weights[0].name, "phoneme_iy");
    assert_eq!(weights[20].name, "phoneme_f");
    assert_eq!(weights[42].name, "phoneme_flap");
    assert_eq!(weights[0].canonical_bytes.len(), 32);
    assert_eq!(weights[1].canonical_bytes.len(), 64);
    assert_eq!(
        member(&weights[42].value, "aah_or_lip_big_aah"),
        &Value::Float(42.0)
    );
    let tints: Vec<_> = record
        .fields
        .iter()
        .filter(|field| field.signature == *b"TINT")
        .map(|field| (field.name.as_str(), &field.value))
        .collect();
    assert_eq!(
        tints,
        [
            ("male_tint_texture", &Value::String("male-tint.dds".into())),
            (
                "female_tint_texture",
                &Value::String("female-tint.dds".into())
            )
        ]
    );
    assert_eq!(result.diagnostics["faces.esm"].unexpected_subrecords, 0);
}

/// Native crime layouts stop only at whole optional members, including the 18-byte form.
#[test]
fn faction_crime_all_whole_optional_tails_are_accepted() {
    let mut fixture = Fixture::new();
    let mut payload = vec![1, 0];
    for value in [1301u16, 229, 47, 83, 0xa55a] {
        payload.extend(value.to_le_bytes());
    }
    payload.extend(0.4375f32.to_le_bytes());
    payload.extend(733u16.to_le_bytes());
    payload.extend(911u16.to_le_bytes());
    let records: Vec<_> = [12, 16, 18, 20]
        .into_iter()
        .enumerate()
        .flat_map(|(index, length)| {
            actor(
                b"FACT",
                0x2c00 + index as u32,
                &[native::subrecord(b"CRVA", &payload[..length])],
            )
        })
        .collect();
    fixture.add("Crime.esm", &[], 1, &records);
    let (_, result) = fixture.read();
    for (index, length) in [12, 16, 18, 20].into_iter().enumerate() {
        let values = field(&result.records[&(0x0100_2c00 + index as u32)], b"CRVA");
        assert_eq!(values.canonical_bytes, payload[..length]);
        assert_eq!(member(&values.value, "murder"), &Value::Unsigned(1301));
        if length >= 18 {
            assert_eq!(member(&values.value, "escape"), &Value::Unsigned(733));
        }
    }
    assert_eq!(result.diagnostics["crime.esm"].skipped_fields, 0);
}

/// Actual publication retains valid actor neighbors and a usable pre-override actor.
#[tokio::test]
async fn actor_corruption_is_local_through_real_pack_publication() {
    use converter::{AssetPipeline, PipelineConfig, config::RecordReader};
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    let output = temp.path().join("pack");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let mut generated = fs::read(data.join("Skyrim.esm")).unwrap();
    generated.extend(native::representatives());
    fs::write(data.join("Skyrim.esm"), &generated).unwrap();
    let broken = [
        actor(b"EYES", 0x0100_3400, &[native::subrecord(b"DATA", &[1])]),
        actor(b"NPC_", 0x2400, &[native::subrecord(b"ACBS", &[0; 23])]),
        actor(
            b"NPC_",
            0x0100_3401,
            &[native::subrecord(b"ACBS", &[0; 23])],
        ),
        actor(b"EYES", 0x0100_3402, &[native::subrecord(b"DATA", &[4])]),
    ]
    .concat();
    fs::write(
        data.join("Broken.esp"),
        native::plugin(&generated, &["Skyrim.esm"], 0, &broken),
    )
    .unwrap();
    fs::write(data.join("plugins.txt"), "*Skyrim.esm\n*Broken.esp\n").unwrap();
    let mut config = PipelineConfig::new(&data, &output);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    for id in [0x0100_3400u32, 0x0100_3402] {
        assert_eq!(
            db.query_row(
                "SELECT record_type FROM records WHERE form_id=?1",
                [id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "EYES"
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM records WHERE form_id=?1",
            [0x0100_3401u32],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT load_order FROM records WHERE form_id=?1",
            [0x2400u32],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(diagnostics["decoder"]["broken.esp"]["skipped_records"], 2);
    assert!(output.join("cell_cache.rkyv").is_file());
    assert!(output.join("conversion-manifest.json").is_file());
}

/// Every actor type retains its prior usable winner across a bad compressed override.
#[tokio::test]
async fn all_actor_types_recover_bad_compressed_overrides_during_publication() {
    use converter::{AssetPipeline, PipelineConfig, config::RecordReader};
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    let output = temp.path().join("pack");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,nif,pex,esm").unwrap(),
    )
    .unwrap();
    let mut generated = fs::read(data.join("Skyrim.esm")).unwrap();
    generated.extend(native::representatives());
    fs::write(data.join("Skyrim.esm"), &generated).unwrap();
    let tags = [
        *b"NPC_", *b"RACE", *b"CLAS", *b"FACT", *b"OTFT", *b"HDPT", *b"EYES", *b"VTYP", *b"CSTY",
        *b"BPTD", *b"RELA", *b"ASTP", *b"LCRT", *b"MOVT",
    ];
    let mut broken = Vec::new();
    for (index, tag) in tags.iter().enumerate() {
        let neighbor = 0x0100_3600 + index as u32 * 2;
        broken.extend(actor(
            b"EYES",
            neighbor,
            &[native::subrecord(b"DATA", &[1])],
        ));
        // Native compressed framing: u32 declared size followed by an unusable zlib stream.
        let payload = [64u32.to_le_bytes().as_slice(), &[0, 0xff, 0x7f]].concat();
        broken.extend(native::record(
            tag,
            0x2400 + index as u32,
            0x40000,
            &payload,
        ));
        broken.extend(actor(
            b"EYES",
            neighbor + 1,
            &[native::subrecord(b"DATA", &[4])],
        ));
    }
    fs::write(
        data.join("Broken.esp"),
        native::plugin(&generated, &["Skyrim.esm"], 0, &broken),
    )
    .unwrap();
    fs::write(data.join("plugins.txt"), "*Skyrim.esm\n*Broken.esp\n").unwrap();
    let mut config = PipelineConfig::new(&data, &output);
    config.record_reader = RecordReader::Inhouse;
    config.no_lod = true;
    config.cpu_jobs = 2;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let report = AssetPipeline::run_async(config, tx).await.unwrap();
    drain.await.unwrap();
    assert!(report.complete, "{:?}", report.warnings);
    let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
    for (index, tag) in tags.iter().enumerate() {
        let id = 0x2400 + index as u32;
        let winner = db
            .query_row(
                "SELECT record_type,load_order FROM records WHERE form_id=?1",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
            )
            .unwrap();
        assert_eq!(
            winner,
            (String::from_utf8_lossy(tag).into_owned(), 0),
            "prior winner {id:08X}"
        );
        assert_eq!(
            db.query_row(
                "SELECT load_order FROM inhouse_source_records WHERE form_id=?1",
                [id],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
        for neighbor in [
            0x0100_3600 + index as u32 * 2,
            0x0100_3601 + index as u32 * 2,
        ] {
            let row = db
                .query_row(
                    "SELECT record_type,load_order FROM records WHERE form_id=?1",
                    [neighbor],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
                )
                .unwrap();
            assert_eq!(row, ("EYES".into(), 1), "neighbor {neighbor:08X}");
        }
    }
    let diagnostics: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("inhouse-reader-diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        diagnostics["decoder"]["broken.esp"]["skipped_records"],
        tags.len()
    );
    assert!(output.join("cell_cache.rkyv").is_file());
    assert!(output.join("conversion-manifest.json").is_file());
}

/// The same localized ID resolves through the authored actor field's specific bank.
#[test]
fn typed_actor_export_uses_strings_and_dlstrings_without_conflating_ids() {
    let fixture = Fixture::new();
    let data = fixture.directory.path();
    let fields = [
        native::subrecord(b"FULL", &17u32.to_le_bytes()),
        native::subrecord(b"DESC", &17u32.to_le_bytes()),
    ];
    fs::write(
        data.join("Localized.esp"),
        native::plugin(
            &fixture.generated,
            &["Skyrim.esm"],
            0x80,
            &actor(b"CLAS", 0x0100_3500, &fields),
        ),
    )
    .unwrap();
    fs::write(data.join("plugins.txt"), "*Skyrim.esm\n*Localized.esp\n").unwrap();
    fs::create_dir(data.join("strings")).unwrap();
    let mut strings = vec![1, 0, 0, 0];
    let name = b"Localized class name\0";
    strings.extend((name.len() as u32).to_le_bytes());
    strings.extend(17u32.to_le_bytes());
    strings.extend(0u32.to_le_bytes());
    strings.extend(name);
    fs::write(data.join("strings/Localized_english.strings"), strings).unwrap();
    let description = b"Description from a different bank\0";
    let mut dlstrings = 1u32.to_le_bytes().to_vec();
    dlstrings.extend((description.len() as u32 + 4).to_le_bytes());
    dlstrings.extend(17u32.to_le_bytes());
    dlstrings.extend(0u32.to_le_bytes());
    dlstrings.extend((description.len() as u32).to_le_bytes());
    dlstrings.extend(description);
    fs::write(data.join("strings/Localized_english.dlstrings"), dlstrings).unwrap();
    let exported = tempfile::tempdir().unwrap();
    let output = exported.path().join("typed-bundle");
    converter::esm::inhouse::export_record_bundle_typed(
        data,
        &data.join("plugins.txt"),
        &output,
        &[*b"CLAS"],
    )
    .unwrap();
    let dump = fs::read_to_string(output.join("typed-records.jsonl")).unwrap();
    let record: serde_json::Value = serde_json::from_str(dump.lines().next().unwrap()).unwrap();
    let localization = record["localization"].as_object().unwrap();
    let lookups: Vec<_> = localization
        .values()
        .map(|value| {
            (
                value["bank"].as_str().unwrap(),
                value["text"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        lookups,
        [
            ("strings", "Localized class name"),
            ("dlstrings", "Description from a different bank")
        ]
    );
    for value in localization.values() {
        assert_eq!(value["id"], 17);
    }
}
