//! Both record readers publish identical perk, entry-point, condition, and omission rows (#599).

use super::EsmParser;
use crate::{
    config::RecordReader,
    esm::projection_omissions::{OmissionCategory, ProjectionOmissions},
};
use dummy_content::{esm, inhouse_conditions as cond, inhouse_perks as perks, layout};
use rusqlite::{Connection, types::Value};
use std::{fs, path::Path};

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    perks::subrecord(tag, bytes)
}

fn record_with_flags(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    perks::record(tag, id, flags, payload)
}

fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = b"GRUP".to_vec();
    framed.extend((24 + u32::try_from(payload.len()).unwrap()).to_le_bytes());
    framed.extend(label.to_le_bytes());
    framed.extend(kind.to_le_bytes());
    framed.extend([0; 8]);
    framed.extend(payload);
    framed
}

fn top(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    group(0, u32::from_le_bytes(*tag), payload)
}

fn header(flags: u32, masters: &[&str]) -> Vec<u8> {
    let hedr = [
        1.7f32.to_le_bytes(),
        0u32.to_le_bytes(),
        0x800u32.to_le_bytes(),
    ]
    .concat();
    let mut payload = sub(b"HEDR", &hedr);
    for master in masters {
        payload.extend(sub(b"MAST", &perks::text(master)));
        payload.extend(sub(b"DATA", &[0; 8]));
    }
    record_with_flags(b"TES4", 0, flags, &payload)
}

/// Write a synthetic localized `.strings` / `.dlstrings` table (`docs/specs/converters/db-schema.md`).
fn write_string_table(path: &Path, entries: &[(u32, &str)], length_prefixed: bool) {
    let mut directory = Vec::new();
    let mut blob = Vec::new();
    for &(id, text) in entries {
        directory.extend(id.to_le_bytes());
        directory.extend(u32::try_from(blob.len()).unwrap().to_le_bytes());
        let mut encoded = text.as_bytes().to_vec();
        encoded.push(0);
        if length_prefixed {
            blob.extend(u32::try_from(encoded.len()).unwrap().to_le_bytes());
        }
        blob.extend(encoded);
    }
    let mut bytes = Vec::new();
    bytes.extend(u32::try_from(entries.len()).unwrap().to_le_bytes());
    bytes.extend(u32::try_from(blob.len()).unwrap().to_le_bytes());
    bytes.extend(directory);
    bytes.extend(blob);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

type Rows = Vec<(String, Vec<Value>)>;

fn perk_rows(conn: &Connection) -> Rows {
    let mut rows = Rows::new();
    for (table, key) in [
        ("perks", "id"),
        ("perk_effects", "perk_id, effect_index"),
        ("perk_entry_points", "perk_id, effect_index"),
        (
            "record_conditions",
            "owner_id, owner_kind, parent_index, position",
        ),
    ] {
        let mut statement = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY {key}"))
            .unwrap();
        let columns = statement.column_count();
        rows.extend(
            statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map(|values| (table.to_owned(), values))
                })
                .unwrap()
                .map(Result::unwrap),
        );
    }
    rows
}

fn build_fixture(root: &Path) -> Vec<std::path::PathBuf> {
    let mut skyrim = esm::plugin(&esm::Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    })
    .unwrap();
    skyrim.extend(perks::targets());
    skyrim.extend(top(
        b"GLOB",
        &record_with_flags(
            b"GLOB",
            0x900,
            0,
            &sub(b"EDID", &perks::text("GlobalThresh")),
        ),
    ));
    skyrim.extend(top(
        b"KYWD",
        &record_with_flags(
            b"KYWD",
            0x901,
            0,
            &sub(b"EDID", &perks::text("WeaponSword")),
        ),
    ));

    // Perk 1: "multiply outgoing damage" (entry_point=0, function=3 multiply_value) with 1 condition
    // + all 3 PRKE effect types + actor_value_multiplier + leveled_item + spell + text.
    let mut p1 = perks::prefix(15);
    p1.extend(sub(b"NNAM", &0x0000_2601u32.to_le_bytes()));
    // Top-level condition: GetGlobalValue(0x900) >= 1.0 + HasKeyword wrong-type (SPELL_ID) and absent (0x9999).
    p1.extend(sub(
        b"CTDA",
        &cond::condition(96, 1.0f32.to_bits(), 74, 0x900, 0, 0, 0),
    ));
    p1.extend(sub(
        b"CTDA",
        &cond::condition(0, 1.0f32.to_bits(), 560, perks::SPELL_ID, 0, 0, 0),
    ));
    p1.extend(sub(
        b"CTDA",
        &cond::condition(0, 1.0f32.to_bits(), 560, 0x9999, 0, 0, 0),
    ));
    p1.extend(perks::quest_effect(perks::QUEST_ID));
    // Conditioned ability effect (PRKE type 1 with PRKC/CTDA before PRKF).
    p1.extend(sub(b"PRKE", &[1, 0, 5]));
    p1.extend(sub(b"DATA", &perks::SPELL_ID.to_le_bytes()));
    p1.extend(sub(b"PRKC", &[0]));
    p1.extend(sub(
        b"CTDA",
        &cond::condition(0, 1.0f32.to_bits(), 560, 0x901, 0, 0, 0),
    ));
    p1.extend(sub(b"PRKF", &[]));
    // Entry point 0 (calculate_weapon_damage), function 3 (multiply_value), value 2.5, with 1 condition.
    p1.extend(sub(b"PRKE", &[2, 1, 10]));
    p1.extend(sub(b"DATA", &[0, 3, 1]));
    p1.extend(sub(b"PRKC", &[0]));
    p1.extend(sub(
        b"CTDA",
        &cond::condition(0, 1.0f32.to_bits(), 560, 0x901, 0, 0, 0), // HasKeyword(0x901) == 1.0
    ));
    p1.extend(sub(b"EPFT", &[1]));
    p1.extend(sub(b"EPFD", &2.5f32.to_le_bytes()));
    p1.extend(sub(b"PRKF", &[]));
    // Entry point with actor_value_multiplier (function 5, f32-bits AV 24.0, mult 1.5).
    p1.extend(perks::entry_effect(
        35,
        5,
        2,
        Some(&[24.0f32.to_le_bytes(), 1.5f32.to_le_bytes()].concat()),
        None,
    ));
    // Entry point with leveled_item (EPFT 3) and label/fragment (EPF2/EPF3).
    let label = perks::text("Pickpocket Choice");
    p1.extend(perks::entry_effect(
        9,
        8,
        3,
        Some(&perks::LIST_ID.to_le_bytes()),
        Some(&label),
    ));

    let mut p2 = perks::prefix(30);
    // Include Windows-1252 bytes 0xC3 0xA9 (decoded as two UTF-8 chars "Ã©") to test inline parity.
    p2.extend(perks::entry_effect(
        54,
        11,
        6,
        Some(b"bStagger\xC3\xA9OnHit\0"),
        None,
    ));
    // EPFT=0 (none), EPFT=5 (spell), and EPFT=4 add_activate_choice (fn=9) with label and no EPFD or null FormID 0.
    p2.extend(perks::entry_effect(65, 0, 0, None, None));
    p2.extend(perks::entry_effect(
        51,
        10,
        5,
        Some(&perks::SPELL_ID.to_le_bytes()),
        None,
    ));
    let talk_label = perks::text("Talk");
    p2.extend(perks::entry_effect(14, 9, 4, None, Some(&talk_label)));
    let open_label = perks::text("Open");
    p2.extend(perks::entry_effect(
        14,
        9,
        4,
        Some(&0u32.to_le_bytes()),
        Some(&open_label),
    ));
    let deleted_perk_id = perks::FIRST_ID + 2;

    skyrim.extend(top(
        b"PERK",
        &[
            record_with_flags(b"PERK", perks::FIRST_ID, 0, &p1),
            record_with_flags(b"PERK", perks::FIRST_ID + 1, 0, &p2),
            record_with_flags(b"PERK", deleted_perk_id, 0, &perks::prefix(1)),
        ]
        .concat(),
    ));

    let skyrim_path = root.join("Skyrim.esm");
    fs::write(&skyrim_path, skyrim).unwrap();

    // Slot 1: interposed empty plugin shifting TargetMaster.esm from local master index 1 to slot 2.
    let interposed_path = root.join("Interposed.esp");
    fs::write(&interposed_path, header(0, &[])).unwrap();

    // Slot 2: target master holding a GLOB (0x02000A00) and PERK (0x02000A01).
    let target_master = [
        header(0, &[]),
        top(
            b"GLOB",
            &record_with_flags(
                b"GLOB",
                0x0000_0A00,
                0,
                &sub(b"EDID", &perks::text("TargetGlob")),
            ),
        ),
        top(
            b"PERK",
            &record_with_flags(b"PERK", 0x0000_0A01, 0, &perks::prefix(5)),
        ),
    ]
    .concat();
    let target_master_path = root.join("TargetMaster.esm");
    fs::write(&target_master_path, target_master).unwrap();

    // Slot 3: localized dependent plugin referencing Skyrim.esm (index 0 -> slot 0) and
    // TargetMaster.esm (index 1 -> load-order slot 2) for non-identity FormID remap parity.
    let mut loc = sub(b"EDID", &perks::text("LocalizedModPerk"));
    loc.extend(sub(b"FULL", &101u32.to_le_bytes()));
    loc.extend(sub(b"DESC", &202u32.to_le_bytes()));
    loc.extend(sub(b"DATA", &[0, 5, 1, 1, 0]));
    // Local master index 1 (0x01000A01) must remap to slot 2 (0x02000A01).
    loc.extend(sub(b"NNAM", &0x0100_0A01u32.to_le_bytes()));
    loc.extend(perks::entry_effect(
        81,
        15,
        7,
        Some(&404u32.to_le_bytes()),
        Some(&303u32.to_le_bytes()),
    ));
    // Also a condition referencing TargetMaster's global (local 0x01000A00 -> slot 2 0x02000A00).
    let mut loc_cond = sub(b"PRKE", &[2, 2, 1]);
    loc_cond.extend(sub(b"DATA", &[0, 3, 1]));
    loc_cond.extend(sub(b"PRKC", &(-1i8).to_le_bytes()));
    loc_cond.extend(sub(
        b"CTDA",
        &cond::condition(4, 0x0100_0A00, 53, 0x14, 0, 2, 0x14),
    ));
    loc_cond.extend(sub(b"CIS1", b"bEnab\xC3\xA9led\0"));
    loc_cond.extend(sub(b"EPFT", &[1]));
    loc_cond.extend(sub(b"EPFD", &3.0f32.to_le_bytes()));
    loc_cond.extend(sub(b"PRKF", &[]));
    loc.extend(loc_cond);
    // Intentionally unresolved localized text (string ID 0 under EPFT=7) produces NULL without omission.
    loc.extend(perks::entry_effect(
        81,
        15,
        7,
        Some(&0u32.to_le_bytes()),
        Some(&303u32.to_le_bytes()),
    ));

    let mod_bytes = [
        header(0x80, &["Skyrim.esm", "TargetMaster.esm"]),
        top(
            b"PERK",
            &[
                record_with_flags(b"PERK", 0x0200_0800, 0, &loc),
                record_with_flags(b"PERK", deleted_perk_id, 0x20, &[]),
            ]
            .concat(),
        ),
    ]
    .concat();
    let mod_path = root.join("PerkMod.esp");
    fs::write(&mod_path, mod_bytes).unwrap();

    write_string_table(
        &root.join("Strings/PerkMod_English.STRINGS"),
        &[
            (101, "Localized Perk Name"),
            (303, "Activate Label"),
            (404, "Set Text Value"),
        ],
        false,
    );
    write_string_table(
        &root.join("Strings/PerkMod_English.DLSTRINGS"),
        &[(202, "Localized Perk Description")],
        true,
    );

    vec![skyrim_path, interposed_path, target_master_path, mod_path]
}

fn publish(reader: RecordReader, root: &Path) -> Connection {
    let paths = build_fixture(root);
    let db = root.join("skyrim_world.db");
    EsmParser::convert_plugins_with_reader(&paths, &db, reader, root).unwrap();
    Connection::open(db).unwrap()
}

#[test]
fn both_readers_publish_identical_perk_entry_points_and_conditions() {
    let legacy_dir = tempfile::tempdir().unwrap();
    let inhouse_dir = tempfile::tempdir().unwrap();
    let legacy = publish(RecordReader::Legacy, legacy_dir.path());
    let inhouse = publish(RecordReader::Inhouse, inhouse_dir.path());

    assert_eq!(perk_rows(&legacy), perk_rows(&inhouse));

    // Verify Smallest Check from #599: multiply outgoing damage entry point + condition + 0 omissions.
    let (ep, fn_id, val): (i64, i64, f64) = inhouse
        .query_row(
            "SELECT entry_point, function, value_float_1
             FROM perk_entry_points
             WHERE perk_id = ?1 AND effect_index = 2",
            [perks::FIRST_ID],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((ep, fn_id, val), (0, 3, 2.5));

    let (fn_idx, param1_form): (i64, Option<u32>) = inhouse
        .query_row(
            "SELECT function_index, param1_form_id
             FROM record_conditions
             WHERE owner_id = ?1 AND owner_kind = 'PERK_EFFECT' AND parent_index = 2",
            [perks::FIRST_ID],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((fn_idx, param1_form), (560, Some(0x901)));

    // Verify localized strings resolved on EPFT=7, EPF2, FULL, DESC, and remapped NNAM (0x01->0x02) + CIS1.
    let (full, desc, next_id): (String, String, u32) = inhouse
        .query_row(
            "SELECT full_name, description, next_perk_id FROM perks WHERE id = 0x03000800",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(full, "Localized Perk Name");
    assert_eq!(desc, "Localized Perk Description");
    assert_eq!(next_id, 0x0200_0A01);

    let (label, value_text): (String, String) = inhouse
        .query_row(
            "SELECT button_label, value_text
             FROM perk_entry_points
             WHERE perk_id = 0x03000800 AND effect_index = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(label, "Activate Label");
    assert_eq!(value_text, "Set Text Value");

    let (cond_tab, global_id, ref_id, p1_str): (i64, Option<u32>, Option<u32>, Option<String>) =
        inhouse
            .query_row(
                "SELECT condition_tab, comparison_global_id, reference_id, param1_string
                 FROM record_conditions
                 WHERE owner_id = 0x03000800 AND owner_kind = 'PERK_EFFECT' AND parent_index = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
    assert_eq!(cond_tab, -1);
    assert_eq!(global_id, Some(0x0200_0A00));
    assert_eq!(ref_id, Some(0x14));
    assert_eq!(p1_str.as_deref(), Some("bEnab\u{c3}\u{a9}led"));

    let (av, av_mult): (i64, f64) = inhouse
        .query_row(
            "SELECT actor_value, value_float_1 FROM perk_entry_points WHERE perk_id = ?1 AND effect_index = 3",
            [perks::FIRST_ID],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(av, 24);
    assert_eq!(av_mult.to_bits(), 1.5f64.to_bits());

    let cp1252_epfd_text: String = inhouse
        .query_row(
            "SELECT value_text FROM perk_entry_points WHERE perk_id = ?1 AND parameter_type = 6",
            [perks::FIRST_ID + 1],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cp1252_epfd_text, "bStagger\u{c3}\u{a9}OnHit");

    // Deleted perk overrides emit no row in `perks`, and EPFT=0/4/5/7 resolve cleanly.
    let ability_cond_kw: Option<u32> = inhouse
        .query_row(
            "SELECT param1_form_id FROM record_conditions
             WHERE owner_id = ?1 AND owner_kind = 'PERK_EFFECT' AND parent_index = 1",
            [perks::FIRST_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ability_cond_kw, Some(0x901));

    let bad_conds: Vec<(u32, Option<u32>)> = inhouse
        .prepare(
            "SELECT param1_raw, param1_form_id FROM record_conditions
             WHERE owner_id = ?1 AND owner_kind = 'PERK' AND function_index = 560
             ORDER BY position",
        )
        .unwrap()
        .query_map([perks::FIRST_ID], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(bad_conds, vec![(0, None), (0, None)]);

    let activate_labels: Vec<(Option<String>, Option<u32>)> = inhouse
        .prepare(
            "SELECT button_label, target_form_id
             FROM perk_entry_points
             WHERE perk_id = ?1 AND entry_point = 14 AND function = 9 AND parameter_type = 4
             ORDER BY effect_index",
        )
        .unwrap()
        .query_map([perks::FIRST_ID + 1], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        activate_labels,
        vec![
            (Some("Talk".to_owned()), None),
            (Some("Open".to_owned()), None),
        ]
    );

    let deleted_count: i64 = inhouse
        .query_row(
            "SELECT COUNT(*) FROM perks WHERE id = ?1",
            [perks::FIRST_ID + 2],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(deleted_count, 0);

    let spell_target: Option<u32> = inhouse
        .query_row(
            "SELECT target_form_id FROM perk_entry_points WHERE perk_id = ?1 AND parameter_type = 5",
            [perks::FIRST_ID + 1],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(spell_target, Some(perks::SPELL_ID));

    // Zero undecoded entry points.
    let omissions = ProjectionOmissions::read(&inhouse).unwrap();
    assert!(
        !omissions
            .categories
            .contains_key(OmissionCategory::PerkEffectUndecoded.name()),
        "expected zero undecoded perk effects, got {omissions:?}"
    );
}

#[test]
fn undecoded_perk_entry_points_are_reported_in_projection_omissions() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut skyrim = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        skyrim.extend(perks::targets());
        let mut bad = perks::prefix(10);
        // 1) Unknown EPFT parameter type 9
        bad.extend(perks::entry_effect(35, 1, 9, Some(&[1, 2, 3, 4]), None));
        // 2) Malformed 2-byte link-valued EPFD for EPFT=3 (dropped by both readers before projection)
        bad.extend(perks::entry_effect(9, 8, 3, Some(&[1, 2]), None));
        // 3) Malformed 2-byte link-valued EPFD for EPFT=5 (dropped by both readers before projection)
        bad.extend(perks::entry_effect(51, 10, 5, Some(&[1, 2]), None));
        // 4) Missing required numeric EPFD for EPFT=1
        bad.extend(perks::entry_effect(36, 3, 1, None, None));
        // 5) Parameterized entry-point function=3 (multiply_value) with neither EPFT nor EPFD
        bad.extend(sub(b"PRKE", &[2, 1, 0]));
        bad.extend(sub(b"DATA", &[36, 3, 1]));
        bad.extend(sub(b"PRKF", &[]));
        // 6) EPFT=3 pointing at a wrong-type SPEL target instead of LVLI
        bad.extend(perks::entry_effect(
            9,
            8,
            3,
            Some(&perks::SPELL_ID.to_le_bytes()),
            None,
        ));
        // 7) EPFT=5 pointing at an absent FormID
        bad.extend(perks::entry_effect(
            51,
            10,
            5,
            Some(&0x0000_BEEFu32.to_le_bytes()),
            None,
        ));
        // 8) EPFT=7 (set_text) missing its EPFD subrecord
        bad.extend(perks::entry_effect(81, 15, 7, None, None));
        // 9) EPFT=4 (add_activate_choice) with null FormID 0 and no label/flags
        bad.extend(perks::entry_effect(
            14,
            9,
            4,
            Some(&0u32.to_le_bytes()),
            None,
        ));
        // 10) EPFT=6 text payload on multiply_value (function=3)
        bad.extend(perks::entry_effect(36, 3, 6, Some(b"not a float\0"), None));
        // Valid sibling effect must survive.
        bad.extend(perks::entry_effect(
            36,
            3,
            1,
            Some(&1.5f32.to_le_bytes()),
            None,
        ));
        skyrim.extend(top(
            b"PERK",
            &record_with_flags(b"PERK", perks::FIRST_ID, 0, &bad),
        ));
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        let entry = &omissions.categories[OmissionCategory::PerkEffectUndecoded.name()];
        assert_eq!(entry.count, 10, "{reader:?}: {entry:?}");
        assert_eq!(entry.samples[0].form_id, perks::FIRST_ID);

        let surviving: Vec<(i64, i64, f64)> = conn
            .prepare(
                "SELECT effect_index, entry_point, value_float_1
                 FROM perk_entry_points
                 WHERE perk_id = ?1",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(surviving, vec![(10, 36, 1.5)], "{reader:?}");
    }
}

#[test]
fn rejected_perk_conditions_and_script_flags_report_omissions_without_stripping_conditions() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut skyrim = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        skyrim.extend(perks::targets());
        skyrim.extend(top(
            b"KYWD",
            &record_with_flags(
                b"KYWD",
                0x901,
                0,
                &sub(b"EDID", &perks::text("WeaponSword")),
            ),
        ));

        let valid_ctda = cond::condition(0, 1.0f32.to_bits(), 560, 0x901, 0, 0, 0);
        let short_ctda = valid_ctda[..31].to_vec();
        let mut perk = perks::prefix(12);

        // Effect 0: Ability with valid SPELL_ID and a single short 31-byte CTDA -> must not publish unconditional ability.
        perk.extend(sub(b"PRKE", &[1, 0, 0]));
        perk.extend(sub(b"DATA", &perks::SPELL_ID.to_le_bytes()));
        perk.extend(sub(b"PRKC", &[0]));
        perk.extend(sub(b"CTDA", &short_ctda));
        perk.extend(sub(b"PRKF", &[]));

        // Effect 1: Entry point (fn=3) with 1 valid CTDA + 1 short 31-byte CTDA -> must not publish weakened conditions.
        perk.extend(sub(b"PRKE", &[2, 0, 1]));
        perk.extend(sub(b"DATA", &[0, 3, 1]));
        perk.extend(sub(b"PRKC", &[0]));
        perk.extend(sub(b"CTDA", &valid_ctda));
        perk.extend(sub(b"CTDA", &short_ctda));
        perk.extend(sub(b"EPFT", &[1]));
        perk.extend(sub(b"EPFD", &2.0f32.to_le_bytes()));
        perk.extend(sub(b"PRKF", &[]));

        // Effect 2: Activation choice with 3-byte EPF3 -> must record omission instead of publishing without script flags.
        perk.extend(sub(b"PRKE", &[2, 0, 2]));
        perk.extend(sub(b"DATA", &[14, 9, 1]));
        perk.extend(sub(b"EPFT", &[4]));
        perk.extend(sub(b"EPF2", &perks::text("Choice")));
        perk.extend(sub(b"EPF3", &[1, 0, 2]));
        perk.extend(sub(b"EPFD", &perks::SPELL_ID.to_le_bytes()));
        perk.extend(sub(b"PRKF", &[]));

        // Effect 3: Valid conditioned ability sibling must survive with its condition intact.
        perk.extend(sub(b"PRKE", &[1, 0, 3]));
        perk.extend(sub(b"DATA", &perks::SPELL_ID.to_le_bytes()));
        perk.extend(sub(b"PRKC", &[0]));
        perk.extend(sub(b"CTDA", &valid_ctda));
        perk.extend(sub(b"PRKF", &[]));

        skyrim.extend(top(
            b"PERK",
            &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
        ));
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        let entry = &omissions.categories[OmissionCategory::PerkEffectUndecoded.name()];
        assert_eq!(entry.count, 3, "{reader:?}: {entry:?}");

        let effects: Vec<(i64, i64)> = conn
            .prepare("SELECT effect_index, ability_spell_id FROM perk_effects WHERE perk_id = ?1 ORDER BY effect_index")
            .unwrap()
            .query_map([perks::FIRST_ID], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(effects, vec![(3, i64::from(perks::SPELL_ID))], "{reader:?}");

        let entry_points: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM perk_entry_points WHERE perk_id = ?1",
                [perks::FIRST_ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(entry_points, 0, "{reader:?}");

        let conditions: Vec<(i64, Option<u32>)> = conn
            .prepare(
                "SELECT parent_index, param1_form_id
                 FROM record_conditions
                 WHERE owner_id = ?1 AND owner_kind = 'PERK_EFFECT'
                 ORDER BY parent_index, position",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(conditions, vec![(3, Some(0x901))], "{reader:?}");
    }
}

#[test]
fn localized_malformed_epfd_triggers_omission_while_null_key_stays_nullable() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut perk = sub(b"EDID", &perks::text("LocalizedMalformedEpfd"));
        perk.extend(sub(b"FULL", &101u32.to_le_bytes()));
        perk.extend(sub(b"DESC", &202u32.to_le_bytes()));
        perk.extend(sub(b"DATA", &[0, 10, 1, 1, 0]));
        // Effect 0: intentional null key (0) -> NULL value_text, zero omissions.
        perk.extend(perks::entry_effect(
            81,
            15,
            7,
            Some(&0u32.to_le_bytes()),
            None,
        ));
        // Effect 1: malformed 2-byte localized EPFD -> perk_effect_undecoded.
        perk.extend(perks::entry_effect(81, 15, 7, Some(&[1, 2]), None));
        let skyrim = [
            header(0x80, &[]),
            top(
                b"PERK",
                &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
            ),
        ]
        .concat();
        write_string_table(
            &dir.path().join("Strings/Skyrim_English.STRINGS"),
            &[(101, "Perk Name")],
            false,
        );
        write_string_table(
            &dir.path().join("Strings/Skyrim_English.DLSTRINGS"),
            &[(202, "Perk Description")],
            true,
        );
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        let entry = &omissions.categories[OmissionCategory::PerkEffectUndecoded.name()];
        assert_eq!(entry.count, 1, "{reader:?}: {entry:?}");

        let rows: Vec<(i64, Option<String>)> = conn
            .prepare(
                "SELECT effect_index, value_text FROM perk_entry_points WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![(0, None)], "{reader:?}");
    }
}

#[test]
fn non_finite_ctda_nonempty_prkf_and_rejected_epfd_link_trigger_omissions() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut skyrim = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        skyrim.extend(perks::targets());
        skyrim.extend(top(
            b"KYWD",
            &record_with_flags(
                b"KYWD",
                0x901,
                0,
                &sub(b"EDID", &perks::text("WeaponSword")),
            ),
        ));

        let mut perk = perks::prefix(14);

        // Effect 0: Ability with non-finite (NaN) literal CTDA comparison -> perk_effect_undecoded.
        perk.extend(sub(b"PRKE", &[1, 0, 0]));
        perk.extend(sub(b"DATA", &perks::SPELL_ID.to_le_bytes()));
        perk.extend(sub(b"PRKC", &[0]));
        perk.extend(sub(
            b"CTDA",
            &cond::condition(0, f32::NAN.to_bits(), 560, 0x901, 0, 0, 0),
        ));
        perk.extend(sub(b"PRKF", &[]));

        // Effect 1: Entry point (fn=3) whose PRKF terminator is non-empty -> perk_effect_undecoded.
        perk.extend(sub(b"PRKE", &[2, 0, 1]));
        perk.extend(sub(b"DATA", &[0, 3, 0]));
        perk.extend(sub(b"EPFT", &[1]));
        perk.extend(sub(b"EPFD", &2.5f32.to_le_bytes()));
        perk.extend(sub(b"PRKF", &[1]));

        // Effect 2: Labeled activation choice with out-of-range non-zero spell FormID (0x01000900)
        // cleared by remapping -> must omit rather than treating the cleared zero as authored null.
        perk.extend(perks::entry_effect(
            14,
            9,
            4,
            Some(&0x0100_0900u32.to_le_bytes()),
            Some(&perks::text("Choice")),
        ));

        // Effect 3: Valid sibling after rejected effects must survive at effect_index 3.
        perk.extend(perks::entry_effect(
            36,
            3,
            1,
            Some(&1.75f32.to_le_bytes()),
            None,
        ));

        skyrim.extend(top(
            b"PERK",
            &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
        ));
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        let entry = &omissions.categories[OmissionCategory::PerkEffectUndecoded.name()];
        assert_eq!(entry.count, 3, "{reader:?}: {entry:?}");

        let ability_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM perk_effects WHERE perk_id = ?1 AND effect_type = 1",
                [perks::FIRST_ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ability_count, 0, "{reader:?}");

        let surviving: Vec<(i64, i64, f64)> = conn
            .prepare(
                "SELECT effect_index, function, value_float_1
                 FROM perk_entry_points WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(surviving, vec![(3, 3, 1.75)], "{reader:?}");
    }
}

#[test]
fn localized_unterminated_epfd_bank_entry_triggers_omission_while_absent_id_stays_nullable() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut perk = sub(b"EDID", &perks::text("LocalizedMalformedEntry"));
        perk.extend(sub(b"FULL", &101u32.to_le_bytes()));
        perk.extend(sub(b"DESC", &202u32.to_le_bytes()));
        perk.extend(sub(b"DATA", &[0, 10, 1, 1, 0]));
        // Effect 0: absent string ID 999 in a readable bank -> NULL value_text, 0 omissions.
        perk.extend(perks::entry_effect(
            81,
            15,
            7,
            Some(&999u32.to_le_bytes()),
            None,
        ));
        // Effect 1: string ID 404 referencing an unterminated bank entry -> perk_effect_undecoded.
        perk.extend(perks::entry_effect(
            81,
            15,
            7,
            Some(&404u32.to_le_bytes()),
            None,
        ));
        let skyrim = [
            header(0x80, &[]),
            top(
                b"PERK",
                &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
            ),
        ]
        .concat();

        // Build STRINGS manually so entry 404 points to bytes with no trailing NUL.
        let mut dir_bytes = Vec::new();
        let mut blob: Vec<u8> = Vec::new();
        dir_bytes.extend(101u32.to_le_bytes());
        dir_bytes.extend(0u32.to_le_bytes());
        blob.extend(b"Perk Name\0");
        dir_bytes.extend(404u32.to_le_bytes());
        dir_bytes.extend(u32::try_from(blob.len()).unwrap().to_le_bytes());
        blob.extend(b"NoNul");
        let mut strings_bytes = Vec::new();
        strings_bytes.extend(2u32.to_le_bytes());
        strings_bytes.extend(u32::try_from(blob.len()).unwrap().to_le_bytes());
        strings_bytes.extend(dir_bytes);
        strings_bytes.extend(blob);
        let strings_path = dir.path().join("Strings/Skyrim_English.STRINGS");
        fs::create_dir_all(strings_path.parent().unwrap()).unwrap();
        fs::write(&strings_path, strings_bytes).unwrap();
        write_string_table(
            &dir.path().join("Strings/Skyrim_English.DLSTRINGS"),
            &[(202, "Perk Description")],
            true,
        );
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        let entry = &omissions.categories[OmissionCategory::PerkEffectUndecoded.name()];
        assert_eq!(entry.count, 1, "{reader:?}: {entry:?}");

        let rows: Vec<(i64, Option<String>)> = conn
            .prepare(
                "SELECT effect_index, value_text FROM perk_entry_points WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows, vec![(0, None)], "{reader:?}");
    }
}

#[test]
fn truncated_top_level_ctda_marks_top_level_rejected() {
    let mut raw_payload = sub(b"EDID", &perks::text("TruncatedTopCtda"));
    raw_payload.extend(sub(b"DATA", &[0, 1, 1, 1, 0]));
    // Declared 32-byte CTDA whose payload stops after 31 bytes at EOF.
    raw_payload.extend(b"CTDA");
    raw_payload.extend(32u16.to_le_bytes());
    raw_payload.extend([0u8; 31]);

    let canonical = super::records::RawRecord {
        form_id: perks::FIRST_ID,
        record_type: *b"PERK",
        flags: 0,
        subrecords: vec![
            (b"EDID".to_vec(), perks::text("TruncatedTopCtda")),
            (b"DATA".to_vec(), vec![0, 1, 1, 1, 0]),
        ],
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    };

    let sections = super::perks::inhouse_rejected_sections(&raw_payload, &canonical, &[], false);
    assert!(sections.top_level);
    assert!(sections.effect_indices.is_empty());
}

#[test]
fn truncated_trailing_prke_records_omission_without_surviving_effect_block() {
    let mut raw_payload = perks::prefix(5);
    raw_payload.extend(perks::entry_effect(
        36,
        3,
        1,
        Some(&2.0f32.to_le_bytes()),
        None,
    ));
    // Trailing PRKE claiming 3 bytes with only 2 present at EOF.
    raw_payload.extend(b"PRKE");
    raw_payload.extend(3u16.to_le_bytes());
    raw_payload.extend([2, 0]);

    let mut canon_subs = vec![
        (b"EDID".to_vec(), perks::text("TruncatedPrke")),
        (b"DATA".to_vec(), vec![0, 5, 1, 1, 0]),
    ];
    canon_subs.extend([
        (b"PRKE".to_vec(), vec![2, 0, 0]),
        (b"DATA".to_vec(), vec![36, 3, 0]),
        (b"EPFT".to_vec(), vec![1]),
        (b"EPFD".to_vec(), 2.0f32.to_le_bytes().to_vec()),
        (b"PRKF".to_vec(), Vec::new()),
    ]);
    let canonical = super::records::RawRecord {
        form_id: perks::FIRST_ID,
        record_type: *b"PERK",
        flags: 0,
        subrecords: canon_subs,
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    };

    let sections = super::perks::inhouse_rejected_sections(&raw_payload, &canonical, &[], false);
    assert!(!sections.top_level);
    assert!(sections.effect_indices.contains(&1));

    let conn = Connection::open_in_memory().unwrap();
    super::exporter::create_tables(&conn).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let master = std::collections::HashMap::from([(canonical.form_id, canonical.clone())]);
    let mut omissions = ProjectionOmissions::default();
    super::perks::insert_record(
        &tx,
        &super::gameplay_common::Links { master: &master },
        &canonical,
        &canonical,
        Some(&sections),
        false,
        &mut omissions,
    )
    .unwrap();
    tx.commit().unwrap();
    omissions.write(&conn).unwrap();
    let summary = ProjectionOmissions::read(&conn).unwrap();
    assert_eq!(
        summary.categories[OmissionCategory::PerkEffectUndecoded.name()].count,
        1
    );
    let surviving: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM perk_entry_points WHERE perk_id = ?1 AND effect_index = 0",
            [canonical.form_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(surviving, 1);
}

#[test]
fn localized_key_zero_with_malformed_bank_zero_and_unresolved_epf2_stay_nullable() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut perk = sub(b"EDID", &perks::text("LocalizedNullZeroAndEpf2"));
        perk.extend(sub(b"FULL", &101u32.to_le_bytes()));
        perk.extend(sub(b"DESC", &202u32.to_le_bytes()));
        perk.extend(sub(b"DATA", &[0, 10, 1, 1, 0]));
        // Effect 0: key 0 under EPFT=7 even when bank has malformed directory entry 0 -> NULL, 0 omissions.
        perk.extend(perks::entry_effect(
            81,
            15,
            7,
            Some(&0u32.to_le_bytes()),
            None,
        ));
        // Effect 1: localized add_activate_choice with missing EPF2 label ID (999) and zero EPFD (no EPF3).
        perk.extend(perks::entry_effect(
            14,
            9,
            4,
            Some(&0u32.to_le_bytes()),
            Some(&999u32.to_le_bytes()),
        ));
        let skyrim = [
            header(0x80, &[]),
            top(
                b"PERK",
                &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
            ),
        ]
        .concat();

        // STRINGS table with malformed ID 0 (unterminated) and valid ID 101.
        let mut dir_bytes = Vec::new();
        let mut blob: Vec<u8> = Vec::new();
        dir_bytes.extend(101u32.to_le_bytes());
        dir_bytes.extend(0u32.to_le_bytes());
        blob.extend_from_slice(b"Perk Name\0");
        dir_bytes.extend(0u32.to_le_bytes());
        dir_bytes.extend(u32::try_from(blob.len()).unwrap().to_le_bytes());
        blob.extend_from_slice(b"BadZero");
        let mut strings_bytes = Vec::new();
        strings_bytes.extend(2u32.to_le_bytes());
        strings_bytes.extend(u32::try_from(blob.len()).unwrap().to_le_bytes());
        strings_bytes.extend(dir_bytes);
        strings_bytes.extend(blob);
        let strings_path = dir.path().join("Strings/Skyrim_English.STRINGS");
        fs::create_dir_all(strings_path.parent().unwrap()).unwrap();
        fs::write(&strings_path, strings_bytes).unwrap();
        write_string_table(
            &dir.path().join("Strings/Skyrim_English.DLSTRINGS"),
            &[(202, "Perk Description")],
            true,
        );
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        assert!(
            !omissions
                .categories
                .contains_key(OmissionCategory::PerkEffectUndecoded.name()),
            "{reader:?}: {omissions:?}"
        );

        type NullableChoiceRow = (i64, Option<String>, Option<String>, Option<u32>);
        let rows: Vec<NullableChoiceRow> = conn
            .prepare(
                "SELECT effect_index, button_label, value_text, target_form_id
                 FROM perk_entry_points WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![(0, None, None, None), (1, None, None, None)],
            "{reader:?}"
        );
    }
}

#[test]
fn subset_export_skips_perk_projection_when_optional_perk_tables_are_absent() {
    let conn = Connection::open_in_memory().unwrap();
    super::exporter::create_tables(&conn).unwrap();
    conn.execute_batch(
        "DROP TABLE perks;
         DROP TABLE perk_effects;
         DROP TABLE perk_entry_points;
         DROP TABLE record_conditions;",
    )
    .unwrap();
    let perk = super::records::RawRecord {
        form_id: perks::FIRST_ID,
        record_type: *b"PERK",
        flags: 0,
        subrecords: vec![
            (b"EDID".to_vec(), perks::text("SubsetPerk")),
            (b"DATA".to_vec(), vec![0, 5, 1, 1, 0]),
        ],
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    };
    let subset = std::collections::HashMap::from([(perk.form_id, perk)]);
    super::exporter::export_to_db(&conn, &subset).unwrap();
    let canonical_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE form_id = ?1 AND record_type = 'PERK'",
            [perks::FIRST_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(canonical_count, 1);
}

#[test]
fn select_spell_without_epfd_and_float_actor_value_ids_publish_cleanly_on_both_readers() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut skyrim = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        skyrim.extend(perks::targets());
        // Retail Gray Cowl shape (PERK 0x810 effect 0): DATA 45 0A 01, EPFT 05, empty PRKF, no EPFD.
        let mut perk = perks::prefix(12);
        perk.extend(sub(b"PRKE", &[2, 0, 0]));
        perk.extend(sub(b"DATA", &[0x45, 0x0A, 0x01]));
        perk.extend(sub(b"EPFT", &[5]));
        perk.extend(sub(b"PRKF", &[]));
        // Retail actor_value_multiplier (fn=12, AV 145.0f32 -> 145, mult 0.25).
        perk.extend(perks::entry_effect(
            66,
            12,
            2,
            Some(&[145.0f32.to_le_bytes(), 0.25f32.to_le_bytes()].concat()),
            None,
        ));
        // Invalid negative float AV (-1.0f32) must still produce perk_effect_undecoded.
        perk.extend(perks::entry_effect(
            66,
            12,
            2,
            Some(&[(-1.0f32).to_le_bytes(), 0.5f32.to_le_bytes()].concat()),
            None,
        ));
        // Malformed 2-byte EPFD under EPFT=5 must stay distinct from genuinely absent EPFD.
        perk.extend(perks::entry_effect(0x45, 0x0A, 5, Some(&[1, 2]), None));
        skyrim.extend(top(
            b"PERK",
            &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
        ));
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        type EntryPointCheck = (i64, i64, i64, Option<i64>, Option<u32>, Option<f64>);
        let rows: Vec<EntryPointCheck> = conn
            .prepare(
                "SELECT effect_index, entry_point, function, actor_value, target_form_id, value_float_1
                 FROM perk_entry_points WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (0, 0x45, 0x0A, None, None, None),
                (1, 66, 12, Some(145), None, Some(0.25)),
            ],
            "{reader:?}"
        );
        let omissions = ProjectionOmissions::read(&conn).unwrap();
        assert_eq!(
            omissions.categories[OmissionCategory::PerkEffectUndecoded.name()].count,
            2,
            "{reader:?}: {omissions:?}"
        );
    }
}

#[test]
fn first_definition_bad_prke_or_effect_data_keeps_intact_sibling_on_both_readers() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut skyrim = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        skyrim.extend(perks::targets());
        // First-definition PERK A: short 2-byte PRKE in effect 0 + intact entry point in effect 1.
        let mut short_prke = perks::prefix(10);
        short_prke.extend(sub(b"PRKE", &[2, 0]));
        short_prke.extend(sub(b"DATA", &[0, 3, 0]));
        short_prke.extend(sub(b"EPFT", &[1]));
        short_prke.extend(sub(b"EPFD", &1.5f32.to_le_bytes()));
        short_prke.extend(sub(b"PRKF", &[]));
        short_prke.extend(perks::entry_effect(
            0,
            3,
            1,
            Some(&2.5f32.to_le_bytes()),
            None,
        ));
        // First-definition PERK B: short 7-byte quest effect DATA in effect 0 + intact ability in effect 1.
        let mut short_data = perks::prefix(20);
        short_data.extend(sub(b"PRKE", &[0, 0, 0]));
        short_data.extend(sub(b"DATA", &[0xA7; 7]));
        short_data.extend(sub(b"PRKF", &[]));
        short_data.extend(perks::ability_effect(perks::SPELL_ID));
        skyrim.extend(top(
            b"PERK",
            &[
                record_with_flags(b"PERK", perks::FIRST_ID, 0, &short_prke),
                record_with_flags(b"PERK", perks::FIRST_ID + 1, 0, &short_data),
            ]
            .concat(),
        ));
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let survived_a: Vec<(i64, f64)> = conn
            .prepare(
                "SELECT effect_index, value_float_1 FROM perk_entry_points
                 WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(survived_a, vec![(1, 2.5)], "{reader:?}");

        let survived_b: Vec<(i64, Option<u32>)> = conn
            .prepare(
                "SELECT effect_index, ability_spell_id FROM perk_effects
                 WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID + 1], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(survived_b, vec![(1, Some(perks::SPELL_ID))], "{reader:?}");

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        assert_eq!(
            omissions.categories[OmissionCategory::PerkEffectUndecoded.name()].count,
            2,
            "{reader:?}: {omissions:?}"
        );
    }
}

#[test]
fn missing_prkf_and_malformed_localized_epf2_omit_only_broken_effect_on_both_readers() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut perk = sub(b"EDID", &perks::text("MissingPrkfAndBadEpf2"));
        perk.extend(sub(b"FULL", &101u32.to_le_bytes()));
        perk.extend(sub(b"DESC", &202u32.to_le_bytes()));
        perk.extend(sub(b"DATA", &[0, 10, 1, 1, 0]));
        // Effect 0: missing PRKF completely -> omitted on both readers.
        perk.extend(sub(b"PRKE", &[2, 0, 0]));
        perk.extend(sub(b"DATA", &[0, 3, 0]));
        perk.extend(sub(b"EPFT", &[1]));
        perk.extend(sub(b"EPFD", &1.75f32.to_le_bytes()));
        // Effect 1: wrong-width 2-byte localized EPF2 -> omitted on both readers.
        perk.extend(perks::entry_effect(
            14,
            9,
            4,
            Some(&0u32.to_le_bytes()),
            Some(&[1, 2]),
        ));
        // Effect 2: 4-byte localized EPF2 pointing at malformed bank entry 555 -> omitted on both readers.
        perk.extend(perks::entry_effect(
            14,
            9,
            4,
            Some(&0u32.to_le_bytes()),
            Some(&555u32.to_le_bytes()),
        ));
        // Effect 3: valid entry point -> survives cleanly.
        perk.extend(perks::entry_effect(
            0,
            3,
            1,
            Some(&3.25f32.to_le_bytes()),
            None,
        ));
        let skyrim = [
            header(0x80, &[]),
            top(
                b"PERK",
                &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
            ),
        ]
        .concat();

        let mut dir_bytes = Vec::new();
        let mut blob: Vec<u8> = Vec::new();
        dir_bytes.extend(101u32.to_le_bytes());
        dir_bytes.extend(0u32.to_le_bytes());
        blob.extend_from_slice(b"Perk Name\0");
        dir_bytes.extend(555u32.to_le_bytes());
        dir_bytes.extend(u32::try_from(blob.len()).unwrap().to_le_bytes());
        blob.extend_from_slice(b"UnterminatedLabel");
        let mut strings_bytes = Vec::new();
        strings_bytes.extend(2u32.to_le_bytes());
        strings_bytes.extend(u32::try_from(blob.len()).unwrap().to_le_bytes());
        strings_bytes.extend(dir_bytes);
        strings_bytes.extend(blob);
        let strings_path = dir.path().join("Strings/Skyrim_English.STRINGS");
        fs::create_dir_all(strings_path.parent().unwrap()).unwrap();
        fs::write(&strings_path, strings_bytes).unwrap();
        write_string_table(
            &dir.path().join("Strings/Skyrim_English.DLSTRINGS"),
            &[(202, "Perk Description")],
            true,
        );
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let surviving: Vec<(i64, Option<f64>)> = conn
            .prepare(
                "SELECT effect_index, value_float_1 FROM perk_entry_points
                 WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(surviving, vec![(3, Some(3.25))], "{reader:?}");

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        assert_eq!(
            omissions.categories[OmissionCategory::PerkEffectUndecoded.name()].count,
            3,
            "{reader:?}: {omissions:?}"
        );
    }
}

#[test]
fn subset_export_retains_existing_sqlite_next_perk_target() {
    let conn = Connection::open_in_memory().unwrap();
    super::exporter::create_tables(&conn).unwrap();
    let perk_b_id = perks::FIRST_ID + 1;
    let perk_b = super::records::RawRecord {
        form_id: perk_b_id,
        record_type: *b"PERK",
        flags: 0,
        subrecords: vec![
            (b"EDID".to_vec(), perks::text("TargetPerkB")),
            (b"DATA".to_vec(), vec![0, 20, 1, 1, 0]),
        ],
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    };
    let perk_a = super::records::RawRecord {
        form_id: perks::FIRST_ID,
        record_type: *b"PERK",
        flags: 0,
        subrecords: vec![
            (b"EDID".to_vec(), perks::text("SourcePerkA")),
            (b"DATA".to_vec(), vec![0, 10, 1, 1, 0]),
            (b"NNAM".to_vec(), perk_b_id.to_le_bytes().to_vec()),
        ],
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    };
    // Initial export writes both A -> B.
    let both =
        std::collections::HashMap::from([(perk_a.form_id, perk_a.clone()), (perk_b_id, perk_b)]);
    super::exporter::export_to_db(&conn, &both).unwrap();
    // Re-exporting only A must keep next_perk_id = Some(B) because B exists in SQLite `perks`.
    let only_a = std::collections::HashMap::from([(perk_a.form_id, perk_a.clone())]);
    super::exporter::export_to_db(&conn, &only_a).unwrap();
    let next_id: Option<u32> = conn
        .query_row(
            "SELECT next_perk_id FROM perks WHERE id = ?1",
            [perks::FIRST_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(next_id, Some(perk_b_id));

    // Deleting B in a subset alongside A must clear A's next_perk_id to NULL.
    let deleted_b = super::records::RawRecord {
        form_id: perk_b_id,
        record_type: *b"PERK",
        flags: 0x20,
        subrecords: Vec::new(),
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    };
    let with_deleted_b =
        std::collections::HashMap::from([(perk_a.form_id, perk_a), (perk_b_id, deleted_b)]);
    super::exporter::export_to_db(&conn, &with_deleted_b).unwrap();
    let cleared: Option<u32> = conn
        .query_row(
            "SELECT next_perk_id FROM perks WHERE id = ?1",
            [perks::FIRST_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cleared, None);
}

#[test]
fn wrong_type_condition_target_warns_and_clears_on_both_readers() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut skyrim = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        skyrim.extend(perks::targets());
        let mut perk = perks::prefix(10);
        // HasKeyword (560) pointing at a SPEL instead of a KYWD.
        perk.extend(sub(
            b"CTDA",
            &cond::condition(0, 1.0f32.to_bits(), 560, perks::SPELL_ID, 0, 0, 0),
        ));
        skyrim.extend(top(
            b"PERK",
            &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
        ));
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        let conversion = EsmParser::convert_plugins_with_reader_and_schemas(
            &[skyrim_path],
            &db,
            reader,
            dir.path(),
            "english",
            None,
        )
        .unwrap();
        assert!(
            conversion
                .warnings
                .iter()
                .chain(&conversion.notices)
                .any(|diagnostic| {
                    diagnostic.contains("CTDA")
                        || diagnostic.contains("disallowed target")
                        || diagnostic.contains(&format!("{:08X}", perks::SPELL_ID))
                }),
            "{reader:?}: expected diagnostic for wrong-type CTDA target, got warnings={:?} notices={:?}",
            conversion.warnings,
            conversion.notices
        );
        let conn = Connection::open(db).unwrap();
        let row: (i64, Option<u32>) = conn
            .query_row(
                "SELECT param1_raw, param1_form_id FROM record_conditions
                 WHERE owner_id = ?1 AND owner_kind = 'PERK' AND position = 0",
                [perks::FIRST_ID],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (0, None), "{reader:?}");
    }
}

#[test]
fn unknown_effect_type_prke_preserves_alignment_for_following_sibling_on_both_readers() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut skyrim = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        skyrim.extend(perks::targets());
        let mut perk = perks::prefix(10);
        // Effect 0: 3-byte PRKE with unknown effect_type=3 (> 2); decoder keeps PRKE and drops DATA.
        perk.extend(sub(b"PRKE", &[3, 0, 0]));
        perk.extend(sub(b"DATA", &[0, 3, 0]));
        perk.extend(sub(b"PRKF", &[]));
        // Effect 1: valid entry point must remain aligned at effect_index = 1 and publish cleanly.
        perk.extend(perks::entry_effect(
            0,
            3,
            1,
            Some(&2.25f32.to_le_bytes()),
            None,
        ));
        skyrim.extend(top(
            b"PERK",
            &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
        ));
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let surviving: Vec<(i64, f64)> = conn
            .prepare(
                "SELECT effect_index, value_float_1 FROM perk_entry_points
                 WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(surviving, vec![(1, 2.25)], "{reader:?}");

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        assert_eq!(
            omissions.categories[OmissionCategory::PerkEffectUndecoded.name()].count,
            1,
            "{reader:?}: {omissions:?}"
        );
    }
}

#[test]
fn trailing_effect_without_prkf_at_eof_is_omitted_on_both_readers() {
    for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
        let dir = tempfile::tempdir().unwrap();
        let mut skyrim = esm::plugin(&esm::Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &[esm::PRESET_EXTERIOR_CELL],
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        })
        .unwrap();
        skyrim.extend(perks::targets());
        let mut perk = perks::prefix(10);
        // Effect 0: intact entry point with PRKF.
        perk.extend(perks::entry_effect(
            0,
            3,
            1,
            Some(&4.5f32.to_le_bytes()),
            None,
        ));
        // Effect 1 (last effect at EOF): valid fields but missing its PRKF terminator.
        perk.extend(sub(b"PRKE", &[2, 0, 0]));
        perk.extend(sub(b"DATA", &[0, 3, 0]));
        perk.extend(sub(b"EPFT", &[1]));
        perk.extend(sub(b"EPFD", &1.25f32.to_le_bytes()));
        skyrim.extend(top(
            b"PERK",
            &record_with_flags(b"PERK", perks::FIRST_ID, 0, &perk),
        ));
        let skyrim_path = dir.path().join("Skyrim.esm");
        fs::write(&skyrim_path, skyrim).unwrap();
        let db = dir.path().join("skyrim_world.db");

        EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, reader, dir.path()).unwrap();
        let conn = Connection::open(db).unwrap();

        let surviving: Vec<(i64, f64)> = conn
            .prepare(
                "SELECT effect_index, value_float_1 FROM perk_entry_points
                 WHERE perk_id = ?1 ORDER BY effect_index",
            )
            .unwrap()
            .query_map([perks::FIRST_ID], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(surviving, vec![(0, 4.5)], "{reader:?}");

        let omissions = ProjectionOmissions::read(&conn).unwrap();
        assert_eq!(
            omissions.categories[OmissionCategory::PerkEffectUndecoded.name()].count,
            1,
            "{reader:?}: {omissions:?}"
        );
    }
}
