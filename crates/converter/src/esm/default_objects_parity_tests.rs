//! Both record readers publish the same default movement types and movement types.
//!
//! Update.esm and the DLC override the `DOBJ` that Skyrim.esm defines with partial key lists.
//! The game's loader sets only the keys that a record lists, so the walk default that only the
//! first plugin names must survive in the published `default_objects` rows.

use super::EsmParser;
use crate::config::RecordReader;
use rusqlite::{Connection, types::Value};
use std::{fs, path::Path};

const DEFAULT_MT: u32 = 0x0003_580D;
const CREATURE_MT: u32 = 0x0003_5810;
const BROKEN_MT: u32 = 0x0003_5811;
const UNNAMED_MT: u32 = 0x0003_5812;
const DEFAULT_OBJECTS: u32 = 0x0000_0031;
/// The mod's own movement type, authored with its master count (1) as the file index, and in
/// load-order numbering: the mod is the third plugin.
const MOD_MT_AUTHORED: u32 = 0x0100_0800;
const MOD_MT: u32 = 0x0200_0800;

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
    framed.extend(bytes);
    framed
}

fn record(tag: &[u8; 4], id: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    framed.extend(0u32.to_le_bytes());
    framed.extend(id.to_le_bytes());
    framed.extend(0u32.to_le_bytes());
    framed.extend(44u16.to_le_bytes());
    framed.extend(0u16.to_le_bytes());
    framed.extend(payload);
    framed
}

fn top(tag: &[u8; 4], records: &[Vec<u8>]) -> Vec<u8> {
    let payload = records.concat();
    let mut framed = b"GRUP".to_vec();
    framed.extend((24 + u32::try_from(payload.len()).unwrap()).to_le_bytes());
    framed.extend(u32::from_le_bytes(*tag).to_le_bytes());
    framed.extend(0u32.to_le_bytes());
    framed.extend([0; 8]);
    framed.extend(payload);
    framed
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
        payload.extend(sub(b"MAST", &[master.as_bytes(), &[0]].concat()));
        payload.extend(sub(b"DATA", &[0; 8]));
    }
    let mut framed = b"TES4".to_vec();
    framed.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    framed.extend(flags.to_le_bytes());
    framed.extend(0u32.to_le_bytes());
    framed.extend(0u32.to_le_bytes());
    framed.extend(44u16.to_le_bytes());
    framed.extend(0u16.to_le_bytes());
    framed.extend(payload);
    framed
}

/// A `MOVT` with the given eight directional speeds and the three rotation rates.
fn movement_type(id: u32, name: &str, speeds: [f32; 8]) -> Vec<u8> {
    named_movement_type(id, Some(name), speeds)
}

/// Like [`movement_type`], with no editor ID when `name` is `None`.
fn named_movement_type(id: u32, name: Option<&str>, speeds: [f32; 8]) -> Vec<u8> {
    let sped: Vec<u8> = speeds
        .into_iter()
        .chain([std::f32::consts::PI; 3])
        .flat_map(f32::to_le_bytes)
        .collect();
    let edid = name.map_or_else(Vec::new, |name| {
        sub(b"EDID", format!("{name}\0").as_bytes())
    });
    record(b"MOVT", id, &[edid, sub(b"SPED", &sped)].concat())
}

fn default_objects(pairs: &[(&[u8; 4], u32)]) -> Vec<u8> {
    let dnam: Vec<u8> = pairs
        .iter()
        .flat_map(|(key, target)| [key.as_slice(), &target.to_le_bytes()].concat())
        .collect();
    record(
        b"DOBJ",
        DEFAULT_OBJECTS,
        &[sub(b"EDID", b"DefaultObjectManager\0"), sub(b"DNAM", &dnam)].concat(),
    )
}

const RETAIL: [f32; 8] = [80.09, 370.0, 79.75, 370.0, 80.1, 370.0, 71.93, 205.25];

fn master_plugin() -> Vec<u8> {
    [
        header(1, &[]),
        top(
            b"MOVT",
            &[
                movement_type(DEFAULT_MT, "NPC_Default_MT", RETAIL),
                // Retail movement types hold zero speeds where a creature cannot move.
                movement_type(
                    CREATURE_MT,
                    "NPC_Creature_MT",
                    [0.0, 0.0, 0.0, 0.0, 90.0, 200.0, 0.0, 0.0],
                ),
                // A negative speed is unusable. It must not stop the conversion.
                movement_type(
                    BROKEN_MT,
                    "NPC_Broken_MT",
                    [-1.0, 0.0, 0.0, 0.0, 90.0, 200.0, 0.0, 0.0],
                ),
                // A movement type with no editor ID is malformed. It is skipped, not named "".
                named_movement_type(UNNAMED_MT, None, RETAIL),
            ],
        ),
        top(
            b"DOBJ",
            &[default_objects(&[
                (b"DMWL", DEFAULT_MT),
                (b"DMRN", DEFAULT_MT),
                (b"DMSW", CREATURE_MT),
            ])],
        ),
    ]
    .concat()
}

/// Like Update.esm: overrides the record with a partial key list.
fn partial_override_plugin() -> Vec<u8> {
    [
        header(1, &["Skyrim.esm"]),
        top(b"DOBJ", &[default_objects(&[(b"DMSW", DEFAULT_MT)])]),
    ]
    .concat()
}

/// A mod with its own movement type that replaces the run default.
fn mod_plugin() -> Vec<u8> {
    [
        header(0, &["Skyrim.esm"]),
        top(
            b"MOVT",
            &[movement_type(
                MOD_MT_AUTHORED,
                "Mod_Run_MT",
                [50.0, 60.0, 50.0, 60.0, 55.0, 65.0, 40.0, 50.0],
            )],
        ),
        top(b"DOBJ", &[default_objects(&[(b"DMRN", MOD_MT_AUTHORED)])]),
    ]
    .concat()
}

fn rows(conn: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut statement = conn.prepare(sql).unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns)
                .map(|index| row.get::<_, Value>(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn publish(reader: RecordReader, root: &Path) -> Connection {
    let paths = ["Skyrim.esm", "Update.esm", "Mod.esp"].map(|name| root.join(name));
    let database = root.join(format!("{reader:?}.db"));
    EsmParser::convert_plugins_with_reader(&paths, &database, reader, root).unwrap();
    Connection::open(database).unwrap()
}

#[test]
fn both_record_readers_publish_the_same_default_movement_types() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Skyrim.esm"), master_plugin()).unwrap();
    fs::write(root.path().join("Update.esm"), partial_override_plugin()).unwrap();
    fs::write(root.path().join("Mod.esp"), mod_plugin()).unwrap();

    let legacy = publish(RecordReader::Legacy, root.path());
    let inhouse = publish(RecordReader::Inhouse, root.path());
    let defaults = "SELECT key, form_id FROM default_objects ORDER BY key";
    let movement_types = "SELECT id, editor_id, left_walk, left_run, forward_walk, forward_run, \
                          back_walk, back_run, load_order FROM movement_types ORDER BY id";
    assert_eq!(rows(&legacy, defaults), rows(&inhouse, defaults));
    assert_eq!(
        rows(&legacy, movement_types),
        rows(&inhouse, movement_types)
    );

    let text = |key: &str, id: u32| vec![Value::Text(key.into()), Value::Integer(i64::from(id))];
    assert_eq!(
        rows(&inhouse, defaults),
        [
            // Only Skyrim.esm sets the walk default; Update.esm lists other keys.
            text("DMRN", MOD_MT),
            text("DMSW", DEFAULT_MT),
            text("DMWL", DEFAULT_MT),
        ],
        "the mod replaces the run default, Update.esm the swim default, and the walk default survives"
    );
    let ids: Vec<i64> = rows(&inhouse, "SELECT id FROM movement_types ORDER BY id")
        .into_iter()
        .map(|row| match row[0] {
            Value::Integer(id) => id,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        ids,
        [
            i64::from(DEFAULT_MT),
            i64::from(CREATURE_MT),
            i64::from(MOD_MT)
        ],
        "every usable movement type exports, and the negative and unnamed ones are skipped"
    );
    let zero_strafe: f64 = inhouse
        .query_row(
            "SELECT left_walk FROM movement_types WHERE id=?1",
            [CREATURE_MT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(zero_strafe, 0.0);
}

/// A plugin whose `DOBJ` lists a key with a null link, and padding pairs with a zero key.
fn nulling_plugin() -> Vec<u8> {
    [
        header(1, &["Skyrim.esm"]),
        top(
            b"DOBJ",
            &[default_objects(&[
                // Update.esm holds hundreds of all-zero padding pairs like this one.
                (b"\0\0\0\0", 0),
                // A zero key never names a default, even with a target.
                (b"\0\0\0\0", DEFAULT_MT),
                (b"DMWL", 0),
            ])],
        ),
    ]
    .concat()
}

#[test]
fn a_later_plugin_that_nulls_a_key_removes_the_default_in_both_readers() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Skyrim.esm"), master_plugin()).unwrap();
    fs::write(root.path().join("Update.esm"), nulling_plugin()).unwrap();
    fs::write(root.path().join("Mod.esp"), header(0, &["Skyrim.esm"])).unwrap();

    let legacy = publish(RecordReader::Legacy, root.path());
    let inhouse = publish(RecordReader::Inhouse, root.path());
    let defaults = "SELECT key, form_id FROM default_objects ORDER BY key";
    assert_eq!(rows(&legacy, defaults), rows(&inhouse, defaults));
    let text = |key: &str, id: u32| vec![Value::Text(key.into()), Value::Integer(i64::from(id))];
    assert_eq!(
        rows(&inhouse, defaults),
        [text("DMRN", DEFAULT_MT), text("DMSW", CREATURE_MT)],
        "the null link removes DMWL; DMRN and DMSW survive; the zero key adds no row"
    );
}

#[test]
fn superseded_placement_duplicates_contribute_only_admitted_default_objects() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Skyrim.esm"), master_plugin()).unwrap();
    let placement = |base: u32| {
        let cell = 0x900u32;
        let reference = record(
            b"REFR",
            DEFAULT_OBJECTS,
            &[sub(b"NAME", &base.to_le_bytes()), sub(b"DATA", &[0; 24])].concat(),
        );
        let cell_group = |kind: u32, payload: Vec<u8>| {
            let mut group = b"GRUP".to_vec();
            group.extend((24 + u32::try_from(payload.len()).unwrap()).to_le_bytes());
            group.extend(cell.to_le_bytes());
            group.extend(kind.to_le_bytes());
            group.extend([0; 8]);
            group.extend(payload);
            group
        };
        let group = cell_group(6, cell_group(9, reference));
        top(b"CELL", &[group])
    };
    let cell = top(b"CELL", &[record(b"CELL", 0x900, &sub(b"DATA", &[1, 0]))]);
    let defaults = "SELECT key, form_id FROM default_objects ORDER BY key";
    let text = |key: &str, id: u32| vec![Value::Text(key.into()), Value::Integer(i64::from(id))];
    let mut deleted = record(b"DOBJ", DEFAULT_OBJECTS, &[]);
    deleted[8..12].copy_from_slice(&0x20u32.to_le_bytes());
    let mut deleted_reference = deleted.clone();
    deleted_reference[..4].copy_from_slice(b"REFR");
    let original = vec![
        text("DMRN", DEFAULT_MT),
        text("DMSW", CREATURE_MT),
        text("DMWL", DEFAULT_MT),
    ];
    for (name, base, middle, intervening, expected, history) in [
        (
            "invalid first",
            0xdead,
            top(
                b"DOBJ",
                &[default_objects(&[
                    (b"DMSN", CREATURE_MT),
                    (b"DMRN", CREATURE_MT),
                ])],
            ),
            vec![],
            vec![
                text("DMRN", DEFAULT_MT),
                text("DMSN", CREATURE_MT),
                text("DMSW", CREATURE_MT),
                text("DMWL", DEFAULT_MT),
            ],
            vec!["skipped", "accepted", "skipped"],
        ),
        (
            "valid first",
            0x20,
            top(b"DOBJ", &[default_objects(&[(b"DMSN", CREATURE_MT)])]),
            vec![],
            original.clone(),
            vec!["accepted", "skipped", "skipped"],
        ),
        (
            "usable placement before DOBJ",
            0xdead,
            placement(0x20),
            vec![],
            original.clone(),
            vec!["skipped", "accepted", "skipped"],
        ),
        (
            "queued grouped owner blocks ungrouped claimant",
            0xdead,
            placement(0x20),
            [
                record(
                    b"REFR",
                    DEFAULT_OBJECTS,
                    &[sub(b"NAME", &0x20u32.to_le_bytes()), sub(b"DATA", &[0; 24])].concat(),
                ),
                top(b"DOBJ", &[default_objects(&[(b"DMSN", CREATURE_MT)])]),
            ]
            .concat(),
            vec![
                text("DMRN", DEFAULT_MT),
                text("DMSN", CREATURE_MT),
                text("DMSW", CREATURE_MT),
                text("DMWL", DEFAULT_MT),
            ],
            vec!["skipped", "accepted", "skipped"],
        ),
        (
            "queued tombstone",
            0xdead,
            top(b"DOBJ", &[deleted.clone()]),
            vec![],
            vec![text("DMRN", DEFAULT_MT)],
            vec!["skipped", "accepted", "skipped"],
        ),
        (
            "valid first before queued tombstone",
            0x20,
            top(b"DOBJ", &[deleted.clone()]),
            vec![],
            original,
            vec!["accepted", "skipped", "skipped"],
        ),
        (
            "intervening placement tombstone",
            0xdead,
            top(b"DOBJ", &[default_objects(&[(b"DMSN", CREATURE_MT)])]),
            top(b"REFR", &[deleted_reference]),
            vec![text("DMRN", DEFAULT_MT)],
            vec!["skipped", "accepted", "skipped"],
        ),
        (
            "intervening tombstone",
            0xdead,
            top(b"DOBJ", &[default_objects(&[(b"DMSN", CREATURE_MT)])]),
            top(b"DOBJ", &[deleted]),
            vec![text("DMRN", DEFAULT_MT)],
            vec!["skipped", "accepted", "skipped"],
        ),
    ] {
        fs::write(
            root.path().join("Update.esm"),
            [
                header(1, &["Skyrim.esm"]),
                cell.clone(),
                placement(base),
                middle,
                top(b"DOBJ", &[default_objects(&[(b"DMBK", CREATURE_MT)])]),
            ]
            .concat(),
        )
        .unwrap();
        let intermediate = root.path().join("Intermediate.esp");
        fs::write(
            &intermediate,
            [header(0, &["Skyrim.esm"]), intervening].concat(),
        )
        .unwrap();
        let winner = default_objects(&[(b"DMRN", DEFAULT_MT)]);
        fs::write(
            root.path().join("Mod.esp"),
            [
                header(0, &["Skyrim.esm"]),
                top(b"DOBJ", std::slice::from_ref(&winner)),
            ]
            .concat(),
        )
        .unwrap();
        let output = root.path().join(format!("{name}.db"));
        let paths = ["Skyrim.esm", "Update.esm", "Intermediate.esp", "Mod.esp"]
            .map(|name| root.path().join(name));
        EsmParser::convert_plugins_with_reader(&paths, &output, RecordReader::Inhouse, root.path())
            .unwrap();
        let db = Connection::open(output).unwrap();
        assert_eq!(rows(&db, defaults), expected, "{name}");
        let source: (u32, Vec<u8>) = db
            .query_row(
                "SELECT load_order, payload FROM inhouse_source_records WHERE form_id=?1",
                [DEFAULT_OBJECTS],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(source, (3, winner[24..].to_vec()), "{name}");

        assert_eq!(
            rows(
                &db,
                "SELECT status FROM record_overrides WHERE form_id=0x31 AND plugin_id=1 ORDER BY chain_position",
            ),
            history
                .into_iter()
                .map(|status| vec![Value::Text(status.into())])
                .collect::<Vec<_>>(),
            "{name}",
        );
    }
}
