//! Placeable base types with a model reach `statics` identically from both record readers.

use super::EsmParser;
use crate::config::RecordReader;
use rusqlite::Connection;
use std::{fs, path::Path};

const CELL: u32 = 0x0900;

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

fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
    let mut framed = b"GRUP".to_vec();
    framed.extend((24 + u32::try_from(payload.len()).unwrap()).to_le_bytes());
    framed.extend(label.to_le_bytes());
    framed.extend(kind.to_le_bytes());
    framed.extend([0; 8]);
    framed.extend(payload);
    framed
}

fn text(value: &str) -> Vec<u8> {
    [value.as_bytes(), &[0]].concat()
}

/// One base record per newly covered type, each with `EDID` and `MODL`, and an interior cell
/// placing each of them once.
fn plugin(bases: &[([u8; 4], u32, Option<&str>)]) -> Vec<u8> {
    let hedr = [
        1.7f32.to_le_bytes(),
        0u32.to_le_bytes(),
        0x800u32.to_le_bytes(),
    ]
    .concat();
    let mut bytes = record(b"TES4", 0, &sub(b"HEDR", &hedr));
    let mut references = Vec::new();
    for (index, (tag, id, model)) in bases.iter().enumerate() {
        let mut fields = vec![sub(b"EDID", &text(&format!("Base{index}")))];
        if let Some(model) = model {
            fields.push(sub(b"MODL", &text(model)));
        }
        let base = record(tag, *id, &fields.concat());
        bytes.extend(group(0, u32::from_le_bytes(*tag), &base));
        references.extend(record(
            b"REFR",
            0x0A00 + index as u32,
            &[sub(b"NAME", &id.to_le_bytes()), sub(b"DATA", &[0; 24])].concat(),
        ));
    }
    let cell = record(
        b"CELL",
        CELL,
        &[sub(b"EDID", b"BaseCell\0"), sub(b"DATA", &[1, 0])].concat(),
    );
    let children = group(6, CELL, &group(9, CELL, &references));
    bytes.extend(group(
        0,
        u32::from_le_bytes(*b"CELL"),
        &group(2, 0, &group(3, 0, &[cell, children].concat())),
    ));
    bytes
}

type Placed = (u32, u32, Option<String>, Option<String>);

/// Every reference with its base's `statics` row, as the engine's `LEFT JOIN statics` sees it.
fn placed(reader: RecordReader, plugin: &Path, root: &Path) -> Vec<Placed> {
    let database = root.join(format!("{reader:?}.db"));
    EsmParser::convert_plugins_with_reader(&[plugin.to_path_buf()], &database, reader, root)
        .unwrap();
    Connection::open(database)
        .unwrap()
        .prepare(
            "SELECT r.id, r.base_form_id, s.editor_id, s.model_path
             FROM \"references\" r LEFT JOIN statics s ON s.id = r.base_form_id ORDER BY r.id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn archived_model(root: &Path, reader: RecordReader, form_id: u32) -> Option<Vec<u8>> {
    let database = Connection::open(root.join(format!("{reader:?}.db"))).unwrap();
    let bytes: Vec<u8> = database
        .query_row(
            "SELECT data FROM records WHERE form_id=?1",
            [form_id],
            |row| row.get(0),
        )
        .unwrap();
    let record =
        rkyv::from_bytes::<super::types::ArchivedRecordData, rkyv::rancor::Error>(&bytes).unwrap();
    record
        .subrecords
        .iter()
        .find(|field| field.tag == *b"MODL")
        .map(|field| field.data.to_vec())
}

/// `TACT`, `ADDN`, `APPA` and `ARTO` references resolve a model path (#269 gap 4), and the legacy
/// and in-house frontends publish the same rows.
#[test]
fn both_record_readers_publish_models_for_every_placeable_base_type() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("Skyrim.esm");
    let bases = [
        (*b"TACT", 0x0800, Some("Clutter\\Talking\\Skull.nif")),
        (*b"ADDN", 0x0801, Some("Effects\\AddonCandleFlame.nif")),
        (*b"APPA", 0x0802, Some("Clutter\\Alchemy\\Mortar.nif")),
        (*b"ARTO", 0x0803, Some("Magic\\Art\\Glow.nif")),
    ];
    fs::write(&path, plugin(&bases)).unwrap();

    let legacy = placed(RecordReader::Legacy, &path, root.path());
    let inhouse = placed(RecordReader::Inhouse, &path, root.path());
    assert_eq!(legacy, inhouse);
    assert_eq!(
        inhouse,
        bases
            .iter()
            .enumerate()
            .map(|(index, (_, id, model))| (
                0x0A00 + index as u32,
                *id,
                Some(format!("Base{index}")),
                Some(model.unwrap().to_owned()),
            ))
            .collect::<Vec<_>>()
    );
}

/// Missing and NUL-only MODL values are NULL for each added base type in both readers.
#[test]
fn both_record_readers_project_missing_empty_and_valid_models_consistently() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("Skyrim.esm");
    let signatures = [*b"TACT", *b"ADDN", *b"APPA", *b"ARTO"];
    let mut bases = Vec::new();
    for (type_index, signature) in signatures.into_iter().enumerate() {
        let first_id = 0x0800 + (type_index as u32 * 3);
        bases.extend([
            (signature, first_id, Some("Clutter\\Synthetic\\Valid.nif")),
            (signature, first_id + 1, None),
            (signature, first_id + 2, Some("")),
        ]);
    }
    fs::write(&path, plugin(&bases)).unwrap();

    let legacy = placed(RecordReader::Legacy, &path, root.path());
    let inhouse = placed(RecordReader::Inhouse, &path, root.path());
    assert_eq!(legacy, inhouse);
    for (index, (_, base_id, model)) in bases.iter().enumerate() {
        let projected = &inhouse[index];
        assert_eq!(projected.0, 0x0A00 + index as u32);
        assert_eq!(projected.1, *base_id);
        assert_eq!(projected.2, Some(format!("Base{index}")));
        assert_eq!(
            projected.3.as_deref(),
            model.as_ref().filter(|path| !path.is_empty()).copied()
        );
        let archived = archived_model(root.path(), RecordReader::Inhouse, *base_id);
        assert_eq!(archived, model.map(text));
    }
}
