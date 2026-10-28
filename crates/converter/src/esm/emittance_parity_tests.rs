//! Both record readers publish the same `references.emittance_id` and `emittance` rows.

use super::EsmParser;
use crate::config::RecordReader;
use rusqlite::Connection;
use std::{fs, path::PathBuf};

const LIGHT: u32 = 0x0900;
const WEATHER: u32 = 0x0901;
const REGION: u32 = 0x0902;
const WINDOW: u32 = 0x0903;
const CELL: u32 = 0x0904;
const REGION_REFERENCE: u32 = 0x0905;
const LIGHT_REFERENCE: u32 = 0x0906;
const PLAIN_REFERENCE: u32 = 0x0907;
/// The override plugin's own light, authored with master index 1 (itself).
const LOCAL_LIGHT: u32 = 0x0100_0800;
const LOCAL_LIGHT_REFERENCE: u32 = 0x0100_0801;

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

fn top(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    group(0, u32::from_le_bytes(*tag), payload)
}

fn header(masters: &[&str]) -> Vec<u8> {
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
    record(b"TES4", 0, &payload)
}

/// A model-less `LIGH` whose 48-byte `DATA` carries `colour` at bytes 8-10.
fn light(id: u32, colour: [u8; 3]) -> Vec<u8> {
    let mut data = vec![0u8; 48];
    data[4..8].copy_from_slice(&256u32.to_le_bytes());
    data[8..11].copy_from_slice(&colour);
    record(
        b"LIGH",
        id,
        &[sub(b"EDID", b"Light\0"), sub(b"DATA", &data)].concat(),
    )
}

/// A placed `REFR` of the window static, naming `source` in `XEMI` when given.
fn reference(id: u32, source: Option<u32>) -> Vec<u8> {
    let mut payload = sub(b"NAME", &WINDOW.to_le_bytes());
    if let Some(source) = source {
        payload.extend(sub(b"XEMI", &source.to_le_bytes()));
    }
    payload.extend(sub(b"DATA", &[0; 24]));
    record(b"REFR", id, &payload)
}

fn interior(references: &[u8]) -> Vec<u8> {
    let cell = record(
        b"CELL",
        CELL,
        &[sub(b"EDID", b"WindowCell\0"), sub(b"DATA", &[1, 0])].concat(),
    );
    let children = group(6, CELL, &group(9, CELL, references));
    top(
        b"CELL",
        &group(2, 0, &group(3, 0, &[cell, children].concat())),
    )
}

fn master_plugin() -> Vec<u8> {
    // Fourteen NAM0 colour types; only Effect Lighting (the tenth) has colour.
    let mut nam0 = vec![0u8; 14 * 16];
    for (time, colour) in [[1, 2, 3], [0, 0, 0], [7, 8, 9], [242, 176, 55]]
        .iter()
        .enumerate()
    {
        nam0[9 * 16 + time * 4..9 * 16 + time * 4 + 3].copy_from_slice(colour);
    }
    let weather = record(
        b"WTHR",
        WEATHER,
        &[sub(b"EDID", b"WindowWeather\0"), sub(b"NAM0", &nam0)].concat(),
    );
    let rdat = [3u32.to_le_bytes(), [0, 50, 0, 0]].concat();
    let rdwt = [
        WEATHER.to_le_bytes(),
        100u32.to_le_bytes(),
        0u32.to_le_bytes(),
    ]
    .concat();
    let region = record(
        b"REGN",
        REGION,
        &[
            sub(b"EDID", b"WindowRegion\0"),
            sub(b"RDAT", &rdat),
            sub(b"RDWT", &rdwt),
        ]
        .concat(),
    );
    let window = record(
        b"STAT",
        WINDOW,
        &[sub(b"EDID", b"Window\0"), sub(b"MODL", b"window.nif\0")].concat(),
    );
    [
        header(&[]),
        top(b"STAT", &window),
        top(b"LIGH", &light(LIGHT, [10, 20, 30])),
        top(b"WTHR", &weather),
        top(b"REGN", &region),
        interior(
            &[
                reference(REGION_REFERENCE, Some(REGION)),
                reference(LIGHT_REFERENCE, Some(LIGHT)),
                reference(PLAIN_REFERENCE, None),
            ]
            .concat(),
        ),
    ]
    .concat()
}

/// An override plugin whose new reference names its own light through master index 1.
fn override_plugin() -> Vec<u8> {
    let reference = reference(LOCAL_LIGHT_REFERENCE, Some(LOCAL_LIGHT));
    let cell = record(
        b"CELL",
        CELL,
        &[sub(b"EDID", b"WindowCell\0"), sub(b"DATA", &[1, 0])].concat(),
    );
    [
        header(&["Skyrim.esm"]),
        top(b"LIGH", &light(LOCAL_LIGHT, [40, 50, 60])),
        top(
            b"CELL",
            &group(
                2,
                0,
                &group(
                    3,
                    0,
                    &[cell, group(6, CELL, &group(9, CELL, &reference))].concat(),
                ),
            ),
        ),
    ]
    .concat()
}

type Emittance = (u32, String, Option<u32>, [u8; 12]);

fn publish(
    reader: RecordReader,
    paths: &[PathBuf],
    root: &std::path::Path,
) -> (Vec<(u32, Option<u32>)>, Vec<Emittance>) {
    let database = root.join(format!("{reader:?}.db"));
    EsmParser::convert_plugins_with_reader(paths, &database, reader, root).unwrap();
    let conn = Connection::open(database).unwrap();
    let references = conn
        .prepare("SELECT id, emittance_id FROM \"references\" ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let emittance = conn
        .prepare(
            "SELECT id, source_type, weather_id, sunrise_r, sunrise_g, sunrise_b, day_r, day_g, day_b,
                    sunset_r, sunset_g, sunset_b, night_r, night_g, night_b
             FROM emittance ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            let mut colours = [0u8; 12];
            for (index, colour) in colours.iter_mut().enumerate() {
                *colour = row.get(3 + index)?;
            }
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, colours))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    (references, emittance)
}

/// The projection reads the canonical records, so the legacy and in-house frontends must agree on
/// every `XEMI` target (remapped through the load order), every `RDWT` weather and every colour.
#[test]
fn both_record_readers_publish_the_same_emittance() {
    let root = tempfile::tempdir().unwrap();
    let master = root.path().join("Skyrim.esm");
    let plugin = root.path().join("Windows.esp");
    fs::write(&master, master_plugin()).unwrap();
    fs::write(&plugin, override_plugin()).unwrap();
    let paths = vec![master, plugin];

    let legacy = publish(RecordReader::Legacy, &paths, root.path());
    let inhouse = publish(RecordReader::Inhouse, &paths, root.path());
    assert_eq!(legacy, inhouse);

    let (references, emittance) = inhouse;
    assert_eq!(
        references,
        vec![
            (REGION_REFERENCE, Some(REGION)),
            (LIGHT_REFERENCE, Some(LIGHT)),
            (PLAIN_REFERENCE, None),
            (LOCAL_LIGHT_REFERENCE, Some(LOCAL_LIGHT)),
        ]
    );
    let light = |colour: [u8; 3]| -> [u8; 12] { [colour; 4].concat().try_into().unwrap() };
    assert_eq!(
        emittance,
        vec![
            (LIGHT, "LIGH".into(), None, light([10, 20, 30])),
            (
                REGION,
                "REGN".into(),
                Some(WEATHER),
                [1, 2, 3, 0, 0, 0, 7, 8, 9, 242, 176, 55]
            ),
            (LOCAL_LIGHT, "LIGH".into(), None, light([40, 50, 60])),
        ]
    );
}
