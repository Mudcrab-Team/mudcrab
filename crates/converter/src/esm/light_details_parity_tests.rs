//! Both record readers publish the same `lights` details (#269 gap 6).

use super::EsmParser;
use crate::config::RecordReader;
use rusqlite::{Connection, types::Value};
use std::{fs, path::Path};

const SOUND: u32 = 0x0800;
const LENS: u32 = 0x0801;
const TORCH: u32 = 0x0802;
/// The mod's own sound and light, authored with its master count (1) as the file index, and in
/// load-order numbering: the mod is the third plugin.
const MOD_SOUND_AUTHORED: u32 = 0x0100_0800;
const MOD_SOUND: u32 = 0x0200_0800;
const MOD_LIGHT_AUTHORED: u32 = 0x0100_0801;
const MOD_LIGHT: u32 = 0x0200_0801;

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

fn top(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
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
    record_with_flags(b"TES4", 0, flags, &payload)
}

fn named(tag: &[u8; 4], id: u32) -> Vec<u8> {
    let editor_id = format!("{}{id:X}\0", String::from_utf8_lossy(tag));
    top(
        tag,
        &record_with_flags(tag, id, 0, &sub(b"EDID", editor_id.as_bytes())),
    )
}

/// A full 48-byte `DATA`: time, radius, colour, flags, falloff, FOV, near clip, flicker period
/// and amplitudes, value, weight.
fn data(time: i32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(time.to_le_bytes());
    bytes.extend(512u32.to_le_bytes());
    bytes.extend([255, 180, 90, 0]);
    bytes.extend(0x0000_0008u32.to_le_bytes());
    for value in [1.5f32, 90.0, 4.0, 0.3, 0.2, 5.0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(7u32.to_le_bytes());
    bytes.extend(1.25f32.to_le_bytes());
    assert_eq!(bytes.len(), 48);
    bytes
}

fn light(id: u32, data: &[u8], links: &[(&[u8; 4], u32)]) -> Vec<u8> {
    let mut payload = [sub(b"EDID", b"Torch\0"), sub(b"DATA", data)].concat();
    payload.extend(sub(b"FNAM", &0.75f32.to_le_bytes()));
    for (tag, id) in links {
        payload.extend(sub(tag, &id.to_le_bytes()));
    }
    record_with_flags(b"LIGH", id, 0, &payload)
}

fn master_plugin() -> Vec<u8> {
    [
        header(1, &[]),
        named(b"SNDR", SOUND),
        named(b"LENS", LENS),
        top(
            b"LIGH",
            &[light(
                TORCH,
                &data(-1),
                &[(b"SNAM", SOUND), (b"LNAM", LENS)],
            )]
            .concat(),
        ),
    ]
    .concat()
}

fn mod_plugin() -> Vec<u8> {
    [
        header(0, &["Skyrim.esm"]),
        named(b"SNDR", MOD_SOUND_AUTHORED),
        top(
            b"LIGH",
            &light(
                MOD_LIGHT_AUTHORED,
                &data(24),
                &[(b"SNAM", MOD_SOUND_AUTHORED)],
            ),
        ),
    ]
    .concat()
}

fn rows(conn: &Connection) -> Vec<Vec<Value>> {
    let mut statement = conn.prepare("SELECT * FROM lights ORDER BY id").unwrap();
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
    let paths = ["Skyrim.esm", "Other.esm", "Mod.esp"].map(|name| root.join(name));
    let database = root.join(format!("{reader:?}.db"));
    EsmParser::convert_plugins_with_reader(&paths, &database, reader, root).unwrap();
    Connection::open(database).unwrap()
}

#[test]
fn both_record_readers_publish_the_same_light_details() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Skyrim.esm"), master_plugin()).unwrap();
    fs::write(root.path().join("Other.esm"), header(1, &[])).unwrap();
    fs::write(root.path().join("Mod.esp"), mod_plugin()).unwrap();

    let legacy = publish(RecordReader::Legacy, root.path());
    let inhouse = publish(RecordReader::Inhouse, root.path());
    assert_eq!(rows(&legacy), rows(&inhouse));

    type Details = (
        Option<i64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<i64>,
        Option<f64>,
        Option<i64>,
        Option<i64>,
    );
    let details = |id: u32| -> Details {
        inhouse
            .query_row(
                "SELECT time, field_of_view, near_clip, flicker_period, flicker_intensity_amplitude,
                        flicker_movement_amplitude, value, weight, sound_id, lens_id
                 FROM lights WHERE id=?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .unwrap()
    };
    let flicker = f64::from(0.3f32);
    let intensity = f64::from(0.2f32);
    assert_eq!(
        details(TORCH),
        (
            Some(-1),
            Some(90.0),
            Some(4.0),
            Some(flicker),
            Some(intensity),
            Some(5.0),
            Some(7),
            Some(1.25),
            Some(SOUND.into()),
            Some(LENS.into())
        )
    );
    assert_eq!(
        details(MOD_LIGHT).8,
        Some(MOD_SOUND.into()),
        "SNAM is remapped through the mod's master list"
    );
}
