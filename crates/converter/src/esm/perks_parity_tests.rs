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

/// Write an ESM/ESP string table in Skyrim's `.strings` / `.dlstrings` binary format.
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
    // EPFT=0 (none), EPFT=5 (spell), and EPFT=4 add_activate_choice (fn=9) with label and no EPFD.
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
    loc_cond.extend(sub(b"PRKC", &[-1i8 as u8]));
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
    assert_eq!(av, i64::from(24.0f32.to_bits()));
    assert_eq!(av_mult, 1.5);

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

    let activate_label: Option<String> = inhouse
        .query_row(
            "SELECT button_label FROM perk_entry_points
             WHERE perk_id = ?1 AND entry_point = 14 AND function = 9 AND parameter_type = 4",
            [perks::FIRST_ID + 1],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(activate_label.as_deref(), Some("Talk"));

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
        let mut bad = perks::prefix(10);
        // 1) Unknown EPFT parameter type 9
        bad.extend(perks::entry_effect(35, 1, 9, Some(&[1, 2, 3, 4]), None));
        // 2) Malformed 2-byte link-valued EPFD for EPFT=3 (dropped by both readers before projection)
        bad.extend(perks::entry_effect(9, 8, 3, Some(&[1, 2]), None));
        // 3) Malformed 2-byte link-valued EPFD for EPFT=5 (dropped by both readers before projection)
        bad.extend(perks::entry_effect(51, 10, 5, Some(&[1, 2]), None));
        // 4) Missing required numeric EPFD for EPFT=1
        bad.extend(perks::entry_effect(36, 3, 1, None, None));
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
        assert_eq!(entry.count, 4, "{reader:?}: {entry:?}");
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
        assert_eq!(surviving, vec![(4, 36, 1.5)], "{reader:?}");
    }
}
