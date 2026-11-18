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
    // Top-level condition: GetGlobalValue(0x900) >= 1.0
    p1.extend(sub(
        b"CTDA",
        &cond::condition(96, 1.0f32.to_bits(), 74, 0x900, 0, 0, 0),
    ));
    p1.extend(perks::quest_effect(perks::QUEST_ID));
    p1.extend(perks::ability_effect(perks::SPELL_ID));
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
    // Entry point with actor_value_multiplier (function 5, AV 24, mult 1.5).
    p1.extend(perks::entry_effect(
        35,
        5,
        2,
        Some(&[24u32.to_le_bytes(), 1.5f32.to_le_bytes()].concat()),
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
    p2.extend(perks::entry_effect(
        54,
        11,
        6,
        Some(&perks::text("bStaggerOnHit")),
        None,
    ));
    // EPFT=0 (none) and EPFT=5 (spell).
    p2.extend(perks::entry_effect(65, 0, 0, None, None));
    p2.extend(perks::entry_effect(
        51,
        10,
        5,
        Some(&perks::SPELL_ID.to_le_bytes()),
        None,
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

    // Localized dependent plugin (slot 1) referencing Skyrim.esm (slot 0) in `Strings/` tables.
    let mut loc = sub(b"EDID", &perks::text("LocalizedModPerk"));
    loc.extend(sub(b"FULL", &101u32.to_le_bytes()));
    loc.extend(sub(b"DESC", &202u32.to_le_bytes()));
    loc.extend(sub(b"DATA", &[0, 5, 1, 1, 0]));
    loc.extend(sub(b"NNAM", &0x0000_2600u32.to_le_bytes()));
    loc.extend(perks::entry_effect(
        81,
        15,
        7,
        Some(&404u32.to_le_bytes()),
        Some(&303u32.to_le_bytes()),
    ));
    // Also a condition referencing Skyrim.esm's global and keyword from a dependent plugin.
    let mut loc_cond = sub(b"PRKE", &[2, 2, 1]);
    loc_cond.extend(sub(b"DATA", &[0, 3, 1]));
    loc_cond.extend(sub(b"PRKC", &[-1i8 as u8]));
    loc_cond.extend(sub(
        b"CTDA",
        &cond::condition(4, 0x0000_0900, 53, 0x14, 0, 2, 0x14),
    ));
    loc_cond.extend(sub(b"CIS1", &perks::text("bEnabled")));
    loc_cond.extend(sub(b"EPFT", &[1]));
    loc_cond.extend(sub(b"EPFD", &3.0f32.to_le_bytes()));
    loc_cond.extend(sub(b"PRKF", &[]));
    loc.extend(loc_cond);

    let mod_bytes = [
        header(0x80, &["Skyrim.esm"]),
        top(
            b"PERK",
            &[
                record_with_flags(b"PERK", 0x0100_0800, 0, &loc),
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

    vec![skyrim_path, mod_path]
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

    // Verify localized strings resolved on EPFT=7, EPF2, FULL, DESC, and remapped NNAM + CIS1.
    let (full, desc, next_id): (String, String, u32) = inhouse
        .query_row(
            "SELECT full_name, description, next_perk_id FROM perks WHERE id = 0x01000800",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(full, "Localized Perk Name");
    assert_eq!(desc, "Localized Perk Description");
    assert_eq!(next_id, perks::FIRST_ID);

    let (label, value_text): (String, String) = inhouse
        .query_row(
            "SELECT button_label, value_text
             FROM perk_entry_points
             WHERE perk_id = 0x01000800 AND effect_index = 0",
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
                 WHERE owner_id = 0x01000800 AND owner_kind = 'PERK_EFFECT' AND parent_index = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
    assert_eq!(cond_tab, -1);
    assert_eq!(global_id, Some(0x900));
    assert_eq!(ref_id, Some(0x14));
    assert_eq!(p1_str.as_deref(), Some("bEnabled"));

    // Deleted perk overrides emit no row in `perks`, and EPFT=0/5 spell links resolve.
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
    // Unknown EPFT parameter type 9 should be reported as PerkEffectUndecoded, while valid
    // sibling effect survives.
    bad.extend(perks::entry_effect(35, 1, 9, Some(&[1, 2, 3, 4]), None));
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

    EsmParser::convert_plugins_with_reader(&[skyrim_path], &db, RecordReader::Inhouse, dir.path())
        .unwrap();
    let conn = Connection::open(db).unwrap();

    let omissions = ProjectionOmissions::read(&conn).unwrap();
    let entry = &omissions.categories[OmissionCategory::PerkEffectUndecoded.name()];
    assert_eq!(entry.count, 1);
    assert_eq!(entry.samples[0].form_id, perks::FIRST_ID);

    let kept_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM perk_entry_points WHERE perk_id = ?1",
            [perks::FIRST_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept_count, 1);
}
