//! Both record readers publish the same reference links and details (#269 gap 3).

use super::EsmParser;
use crate::config::RecordReader;
use rusqlite::{Connection, types::Value};
use std::{fs, path::Path};

const KEYWORD: u32 = 0x0800;
const FACTION: u32 = 0x0801;
const KEY: u32 = 0x0802;
const LOCATION: u32 = 0x0803;
const ZONE: u32 = 0x0804;
const CRATE: u32 = 0x0805;
const CELL: u32 = 0x0900;
const CHEST: u32 = 0x0A00;
const LEVER: u32 = 0x0A01;
const TRAP: u32 = 0x0A02;
const MARKER: u32 = 0x0A03;
/// The mod's own key and door, authored with its master count (1) as the file index, and their
/// load-order numbers: the mod is the third plugin.
const MOD_KEY_AUTHORED: u32 = 0x0100_0800;
const MOD_KEY: u32 = 0x0200_0800;
const MOD_DOOR_AUTHORED: u32 = 0x0100_0801;
const MOD_DOOR: u32 = 0x0200_0801;
const MOD_PLATE_AUTHORED: u32 = 0x0100_0802;
const MOD_PLATE: u32 = 0x0200_0802;

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
    framed.extend(bytes);
    framed
}

fn record_with_flags(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    framed.extend(flags.to_le_bytes());
    framed.extend(id.to_le_bytes());
    framed.extend(0u32.to_le_bytes());
    framed.extend(44u16.to_le_bytes());
    framed.extend(0u16.to_le_bytes());
    framed.extend(payload);
    framed
}

fn record(tag: &[u8; 4], id: u32, payload: &[u8]) -> Vec<u8> {
    record_with_flags(tag, id, 0, payload)
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
        payload.extend(sub(b"MAST", &[master.as_bytes(), &[0]].concat()));
        payload.extend(sub(b"DATA", &[0; 8]));
    }
    record_with_flags(b"TES4", 0, flags, &payload)
}

fn named(tag: &[u8; 4], id: u32) -> Vec<u8> {
    let editor_id = format!("{}{id:X}\0", String::from_utf8_lossy(tag));
    top(tag, &record(tag, id, &sub(b"EDID", editor_id.as_bytes())))
}

fn le(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

/// A placed crate with `extra` subrecords between its base and its position.
fn reference(id: u32, extra: &[Vec<u8>]) -> Vec<u8> {
    record(
        b"REFR",
        id,
        &[
            sub(b"NAME", &le(CRATE)),
            extra.concat(),
            sub(b"DATA", &[0; 24]),
        ]
        .concat(),
    )
}

/// A 20-byte `XLOC`: level, three unused bytes, key, flags, then padding.
fn lock(level: u8, key: u32, flags: u8) -> Vec<u8> {
    [
        vec![level, 0, 0, 0],
        le(key).to_vec(),
        vec![flags, 0, 0, 0],
        vec![0; 8],
    ]
    .concat()
}

fn interior(references: &[u8]) -> Vec<u8> {
    let cell = record(
        b"CELL",
        CELL,
        &[sub(b"EDID", b"Vault\0"), sub(b"DATA", &[1, 0])].concat(),
    );
    top(
        b"CELL",
        &group(
            2,
            0,
            &group(
                3,
                0,
                &[cell, group(6, CELL, &group(9, CELL, references))].concat(),
            ),
        ),
    )
}

fn master_plugin() -> Vec<u8> {
    let chest = reference(
        CHEST,
        &[
            sub(b"XLKR", &[le(KEYWORD), le(LEVER)].concat()),
            sub(b"XLKR", &le(TRAP)),
            sub(b"XLKR", &[le(KEYWORD), le(0xDEAD)].concat()),
            sub(b"XAPD", &[1]),
            sub(
                b"XAPR",
                &[le(LEVER).to_vec(), 0.5f32.to_le_bytes().to_vec()].concat(),
            ),
            sub(
                b"XAPR",
                &[le(KEY).to_vec(), 1.0f32.to_le_bytes().to_vec()].concat(),
            ),
            sub(b"XLCN", &le(LOCATION)),
            sub(b"XEZN", &le(ZONE)),
            sub(b"XOWN", &le(FACTION)),
            sub(b"XRNK", &3i32.to_le_bytes()),
            sub(b"XLOC", &lock(50, KEY, 0x04)),
        ],
    );
    let marker = reference(
        MARKER,
        &[
            sub(b"XMRK", &[]),
            sub(b"FNAM", &[0x03]),
            sub(b"TNAM", &[5, 0]),
        ],
    );
    [
        header(1, &[]),
        named(b"KYWD", KEYWORD),
        named(b"FACT", FACTION),
        named(b"KEYM", KEY),
        named(b"LCTN", LOCATION),
        named(b"ECZN", ZONE),
        top(
            b"STAT",
            &record(
                b"STAT",
                CRATE,
                &[sub(b"EDID", b"Crate\0"), sub(b"MODL", b"crate.nif\0")].concat(),
            ),
        ),
        interior(&[chest, reference(LEVER, &[]), reference(TRAP, &[]), marker].concat()),
    ]
    .concat()
}

/// A mod after an unrelated master: its own key and a door into the master's cell, locked with it.
fn mod_plugin() -> Vec<u8> {
    let door = reference(
        MOD_DOOR_AUTHORED,
        &[
            sub(b"ONAM", &[]),
            sub(b"XLOC", &lock(255, MOD_KEY_AUTHORED, 0)),
        ],
    );
    let plate = reference(MOD_PLATE_AUTHORED, &[sub(b"XATR", &le(MOD_DOOR_AUTHORED))]);
    [
        header(0, &["Skyrim.esm"]),
        named(b"KEYM", MOD_KEY_AUTHORED),
        interior(&[door, plate].concat()),
    ]
    .concat()
}

type Rows = Vec<(String, Vec<Value>)>;

fn rows(conn: &Connection) -> Rows {
    let mut rows = Rows::new();
    for (table, key) in [
        ("reference_linked_refs", "reference_id, position"),
        ("reference_activate_parents", "reference_id, position"),
        ("reference_details", "reference_id"),
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

fn publish(reader: RecordReader, root: &Path) -> Connection {
    let paths = ["Skyrim.esm", "Other.esm", "Mod.esp"].map(|name| root.join(name));
    let database = root.join(format!("{reader:?}.db"));
    EsmParser::convert_plugins_with_reader(&paths, &database, reader, root).unwrap();
    Connection::open(database).unwrap()
}

fn get<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> T {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn both_record_readers_publish_the_same_reference_links() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Skyrim.esm"), master_plugin()).unwrap();
    fs::write(root.path().join("Other.esm"), header(1, &[])).unwrap();
    fs::write(root.path().join("Mod.esp"), mod_plugin()).unwrap();

    let legacy = publish(RecordReader::Legacy, root.path());
    let inhouse = publish(RecordReader::Inhouse, root.path());
    assert_eq!(rows(&legacy), rows(&inhouse));

    let conn = inhouse;
    let links: Vec<(Option<i64>, Option<i64>)> = conn
        .prepare(&format!(
            "SELECT keyword_id, linked_ref_id FROM reference_linked_refs WHERE reference_id={CHEST} ORDER BY position"
        ))
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        links,
        vec![
            (Some(KEYWORD.into()), Some(LEVER.into())),
            (None, Some(TRAP.into())),
            (Some(KEYWORD.into()), None),
        ],
        "keyword and reference, the reference alone, and a reference that does not exist"
    );
    let parents: Vec<(Option<i64>, Option<f64>)> = conn
        .prepare(&format!(
            "SELECT parent_ref_id, delay FROM reference_activate_parents WHERE reference_id={CHEST} ORDER BY position"
        ))
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        parents,
        vec![(Some(LEVER.into()), Some(0.5)), (None, Some(1.0))],
        "a key is not a placed reference"
    );

    let detail = |id: u32, column: &str| -> Option<i64> {
        get(
            &conn,
            &format!("SELECT {column} FROM reference_details WHERE reference_id={id}"),
        )
    };
    assert_eq!(detail(CHEST, "owner_id"), Some(FACTION.into()));
    assert_eq!(detail(CHEST, "owner_rank"), Some(3));
    assert_eq!(detail(CHEST, "activate_parent_flags"), Some(1));
    assert_eq!(detail(CHEST, "lock_level"), Some(50));
    assert_eq!(detail(CHEST, "lock_key_id"), Some(KEY.into()));
    assert_eq!(detail(CHEST, "lock_flags"), Some(4));
    assert_eq!(detail(CHEST, "location_id"), Some(LOCATION.into()));
    assert_eq!(detail(CHEST, "encounter_zone_id"), Some(ZONE.into()));
    assert_eq!(detail(CHEST, "map_marker"), Some(0));
    assert_eq!(detail(MARKER, "map_marker"), Some(1));
    assert_eq!(detail(MARKER, "map_marker_flags"), Some(3));
    assert_eq!(detail(MARKER, "map_marker_type"), Some(5));
    assert_eq!(detail(MOD_DOOR, "open_by_default"), Some(1));
    assert_eq!(detail(MOD_DOOR, "lock_level"), Some(255));
    assert_eq!(
        detail(MOD_DOOR, "lock_key_id"),
        Some(MOD_KEY.into()),
        "the XLOC key is remapped through the mod's master list"
    );
    assert_eq!(
        detail(MOD_PLATE, "attached_ref_id"),
        Some(MOD_DOOR.into()),
        "XATR is remapped by both readers"
    );
    let plain: i64 = get(
        &conn,
        &format!("SELECT count(*) FROM reference_details WHERE reference_id IN ({LEVER}, {TRAP})"),
    );
    assert_eq!(
        plain, 0,
        "references without any of these fields get no row"
    );
}
