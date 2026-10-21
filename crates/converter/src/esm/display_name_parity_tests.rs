//! Both record readers publish the same display names (`FULL`), localized or not (#269 gap 5).

use super::{EsmParser, types::ArchivedRecordData};
use crate::config::RecordReader;
use rusqlite::Connection;
use std::{fs, path::Path};

const WORLD: u32 = 0x0800;
const CELL: u32 = 0x0801;
const DOOR: u32 = 0x0802;
const CHEST: u32 = 0x0803;
const PERSON: u32 = 0x0804;
const UNNAMED: u32 = 0x0805;
const DELETED: u32 = 0x0806;
const LOCALIZED: u32 = 0x80;

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut framed = tag.to_vec();
    framed.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
    framed.extend(bytes);
    framed
}

fn record(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
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
    record(b"TES4", 0, flags, &payload)
}

/// A base object with a model whose `FULL` holds `full` (an ID or Windows-1252 text).
fn base(tag: &[u8; 4], id: u32, editor_id: &str, full: &[u8]) -> Vec<u8> {
    record(
        tag,
        id,
        0,
        &[
            sub(b"EDID", &[editor_id.as_bytes(), &[0]].concat()),
            sub(b"FULL", full),
            sub(b"MODL", b"clutter\\thing.nif\0"),
        ]
        .concat(),
    )
}

/// A `.STRINGS` table: a directory of (ID, offset) pairs, then zero-terminated Windows-1252 text.
fn strings_table(entries: &[(u32, &[u8])]) -> Vec<u8> {
    let mut directory = Vec::new();
    let mut data = Vec::new();
    for (id, text) in entries {
        directory.extend(id.to_le_bytes());
        directory.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
        data.extend(*text);
        data.push(0);
    }
    let mut bytes = u32::try_from(entries.len()).unwrap().to_le_bytes().to_vec();
    bytes.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    bytes.extend(directory);
    bytes.extend(data);
    bytes
}

/// A localized master: every `FULL` is a string ID, one of which (99) has no string.
fn master_plugin() -> Vec<u8> {
    let id = |value: u32| value.to_le_bytes();
    let cell = record(
        b"CELL",
        CELL,
        0,
        &[
            sub(b"EDID", b"SvensHouse\0"),
            sub(b"FULL", &id(1)),
            sub(b"DATA", &[1, 0]),
        ]
        .concat(),
    );
    let world = record(
        b"WRLD",
        WORLD,
        0,
        &[sub(b"EDID", b"Tamriel\0"), sub(b"FULL", &id(2))].concat(),
    );
    let person = record(
        b"NPC_",
        PERSON,
        0,
        &[sub(b"EDID", b"Sven\0"), sub(b"FULL", &id(5))].concat(),
    );
    [
        header(1 | LOCALIZED, &[]),
        top(b"DOOR", &base(b"DOOR", DOOR, "HouseDoor", &id(3))),
        top(b"CONT", &base(b"CONT", CHEST, "BreadChest", &id(4))),
        top(b"MISC", &base(b"MISC", UNNAMED, "Unnamed", &id(99))),
        top(b"MISC", &base(b"MISC", DELETED, "Deleted", &id(6))),
        top(b"NPC_", &person),
        top(b"WRLD", &world),
        top(b"CELL", &group(2, 0, &group(3, 0, &cell))),
    ]
    .concat()
}

/// A plain plugin renaming the door in Windows-1252 text.
fn override_plugin() -> Vec<u8> {
    let deleted = record(b"MISC", DELETED, 0x20, &[]);
    [
        header(0, &["Skyrim.esm"]),
        top(b"DOOR", &base(b"DOOR", DOOR, "HouseDoor", b"T\xFCr\0")),
        top(b"MISC", &deleted),
    ]
    .concat()
}

type Names = Vec<(String, u32, Option<String>)>;

/// Every display-name column, and the `FULL` bytes each reader stored in `records.data`.
fn publish(reader: RecordReader, data: &Path, root: &Path) -> (Names, Vec<(u32, Vec<u8>)>) {
    let database = root.join(format!("{reader:?}.db"));
    let paths = vec![data.join("Skyrim.esm"), data.join("Rename.esp")];
    EsmParser::convert_plugins_with_reader(&paths, &database, reader, data).unwrap();
    let conn = Connection::open(database).unwrap();
    let mut names = Names::new();
    for table in ["worldspaces", "cells", "statics", "npcs"] {
        let mut statement = conn
            .prepare(&format!("SELECT id, full_name FROM {table} ORDER BY id"))
            .unwrap();
        names.extend(
            statement
                .query_map([], |row| Ok((table.to_owned(), row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(Result::unwrap),
        );
    }
    let fulls = conn
        .prepare("SELECT form_id, data FROM records ORDER BY form_id")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .filter_map(|(id, data)| {
            let record = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&data).ok()?;
            let full = record
                .subrecords
                .into_iter()
                .find(|sub| &sub.tag == b"FULL")?;
            Some((id, full.data))
        })
        .collect();
    (names, fulls)
}

/// Localized names resolve through the plugin's string table and plain names decode from
/// Windows-1252, the same way for both frontends, in the projections and in `records.data`. A
/// name without a string stays NULL in both.
#[test]
fn both_record_readers_publish_the_same_display_names() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir_all(data.join("Strings")).unwrap();
    fs::write(data.join("Skyrim.esm"), master_plugin()).unwrap();
    fs::write(data.join("Rename.esp"), override_plugin()).unwrap();
    fs::write(
        data.join("Strings").join("Skyrim_english.STRINGS"),
        strings_table(&[
            (1, b"Sven's House"),
            (2, b"Skyrim"),
            (3, b"Door"),
            (4, b"Brotkiste f\xFCr K\xE4se"),
            (5, b"Sven"),
            (6, b"Deleted record name"),
        ]),
    )
    .unwrap();

    let legacy = publish(RecordReader::Legacy, &data, root.path());
    let inhouse = publish(RecordReader::Inhouse, &data, root.path());
    assert_eq!(legacy.0, inhouse.0, "display-name columns");
    assert_eq!(legacy.1, inhouse.1, "FULL bytes in records.data");

    let name = |table: &str, id: u32| {
        inhouse
            .0
            .iter()
            .find(|(owner, row, _)| owner == table && *row == id)
            .unwrap_or_else(|| panic!("{table} {id:08X} has a row"))
            .2
            .clone()
    };
    assert_eq!(name("worldspaces", WORLD).as_deref(), Some("Skyrim"));
    assert_eq!(name("cells", CELL).as_deref(), Some("Sven's House"));
    assert_eq!(
        name("statics", DOOR).as_deref(),
        Some("Tür"),
        "the plain override wins"
    );
    assert_eq!(
        name("statics", CHEST).as_deref(),
        Some("Brotkiste für Käse")
    );
    assert_eq!(name("statics", UNNAMED), None, "ID 99 has no string");
    assert!(
        !legacy.0.iter().any(|(_, id, _)| *id == DELETED),
        "the winning tombstone removes its earlier projection"
    );
    assert!(
        !legacy.1.iter().any(|(id, _)| *id == DELETED),
        "the winning tombstone removes its earlier canonical row"
    );
    assert_eq!(name("npcs", PERSON).as_deref(), Some("Sven"));
}
