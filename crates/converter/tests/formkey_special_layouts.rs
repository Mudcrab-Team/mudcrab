//! Special field layouts share absent-target recovery through both publication readers.
use converter::esm::{EsmParser, inhouse};
use rusqlite::Connection;
use std::fs;

/// Frame an independent synthetic subrecord.
fn sub(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(data.len() as u16).to_le_bytes(), data].concat()
}

/// Frame an independent synthetic record.
fn record(tag: &[u8; 4], id: u32, data: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(data.len() as u32).to_le_bytes(),
        &[0; 4],
        &id.to_le_bytes(),
        &[0; 8],
        data,
    ]
    .concat()
}

/// Frame a containing group without assuming a target header exists.
fn group(kind: u32, id: u32, data: &[u8]) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &((data.len() + 24) as u32).to_le_bytes(),
        &id.to_le_bytes(),
        &kind.to_le_bytes(),
        &[0; 8],
        data,
    ]
    .concat()
}

/// Frame two VMAD object properties, preserving distinct alias and padding sentinels.
fn vmad() -> (Vec<u8>, Vec<usize>) {
    let mut data = [
        5u16.to_le_bytes(),
        2u16.to_le_bytes(),
        1u16.to_le_bytes(),
        6u16.to_le_bytes(),
    ]
    .concat();
    data.extend(b"Script");
    data.extend([0, 2, 0]);
    let mut offsets = Vec::new();
    for (name, raw) in [
        (b"Absent".as_slice(), 0x0300_1234u32),
        (b"Forward".as_slice(), 0x0300_1235),
    ] {
        data.extend((name.len() as u16).to_le_bytes());
        data.extend(name);
        data.extend([1, 1, 0xa1, 0xb2, 0x56, 0x34]);
        offsets.push(data.len());
        data.extend(raw.to_le_bytes());
    }
    (data, offsets)
}

/// Exercise absent, present-forward and zero fields with an unrelated surviving neighbour.
fn check_layout(kind: &[u8; 4], tag: &[u8; 4]) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    fs::write(data.join("Base.esm"), record(b"TES4", 0, &[])).unwrap();
    fs::write(data.join("Filler.esm"), record(b"TES4", 0, &[])).unwrap();
    let (payload, offsets) = match tag {
        b"VMAD" => vmad(),
        b"MODS" => {
            let mut bytes = 2u32.to_le_bytes().to_vec();
            let mut offsets = Vec::new();
            for (name, raw, index) in [
                (b"First".as_slice(), 0x0300_1234u32, 17u32),
                (b"Second".as_slice(), 0x0300_1235, 29),
            ] {
                bytes.extend((name.len() as u32).to_le_bytes());
                bytes.extend(name);
                offsets.push(bytes.len());
                bytes.extend(raw.to_le_bytes());
                bytes.extend(index.to_le_bytes());
            }
            (bytes, offsets)
        }
        b"VTEX" => (
            [
                0x0300_1234u32.to_le_bytes(),
                0x0300_1235u32.to_le_bytes(),
                [0; 4],
            ]
            .concat(),
            vec![0, 4],
        ),
        b"BTXT" | b"ATXT" => (
            [0x0300_1234u32.to_le_bytes(), [2, 0xa7, 3, 0]].concat(),
            vec![0],
        ),
        b"NAME" => (0x0300_1234u32.to_le_bytes().to_vec(), vec![0]),
        _ => unreachable!(),
    };
    let mut fields = if kind == b"LAND" {
        Vec::new()
    } else {
        sub(b"EDID", b"Special\0")
    };
    if kind == b"GRAS" {
        fields.extend(sub(b"MODL", b"Special.nif\0"));
    }
    if tag == b"ATXT" {
        fields.extend(sub(
            b"BTXT",
            &[0x0100_1235u32.to_le_bytes(), [2, 0, 0, 0]].concat(),
        ));
    }
    fields.extend(sub(tag, &payload));
    if kind == b"GRAS" {
        fields.extend(sub(b"DATA", &[0; 32]));
    }
    // Single-word layouts get a second record to prove present forward links survive too.
    let mut body = record(kind, 0x0100_0811, &fields);
    if matches!(tag, b"BTXT" | b"ATXT" | b"NAME") {
        let mut present = fields.clone();
        let needle = 0x0300_1234u32.to_le_bytes();
        let offset = present.windows(4).position(|part| part == needle).unwrap();
        present[offset..offset + 4].copy_from_slice(&0x0300_1235u32.to_le_bytes());
        let second = record(kind, 0x0100_0813, &present);
        if kind == b"LAND" {
            body = [group(6, 0x0100_0901, &body), group(6, 0x0100_0902, &second)].concat();
        } else {
            body.extend(second);
        }
    } else if kind == b"LAND" {
        body = group(6, 0x0100_0901, &body);
    }
    let target = if kind == b"LAND" {
        b"LTEX"
    } else if kind == b"GRAS" {
        b"TXST"
    } else if kind == b"ACHR" {
        b"NPC_"
    } else if tag == b"NAME" && !matches!(kind, b"REFR" | b"ACRE") {
        b"HAZD"
    } else {
        b"STAT"
    };
    body.extend(record(
        target,
        0x0100_1235,
        &sub(b"EDID", b"ForwardTarget\0"),
    ));
    body.extend(record(b"STAT", 0x0100_0812, &sub(b"EDID", b"Neighbor\0")));
    fs::write(
        data.join("Patch.esp"),
        [
            record(
                b"TES4",
                0,
                &[sub(b"MAST", b"Base.esm\0"), sub(b"DATA", &[0; 8])].concat(),
            ),
            body,
        ]
        .concat(),
    )
    .unwrap();
    let paths = [
        data.join("Base.esm"),
        data.join("Filler.esm"),
        data.join("Patch.esp"),
    ];
    let merged = EsmParser::merge_plugins(&paths).unwrap();
    assert!(merged.contains_key(&0x0200_0812));
    let legacy = root.path().join("legacy.db");
    EsmParser::convert_plugins(&paths, &legacy).unwrap();
    let list = root.path().join("plugins.txt");
    fs::write(&list, "*Base.esm\n*Filler.esm\n*Patch.esp\n").unwrap();
    let typed = root.path().join("typed");
    inhouse::export_record_bundle(&data, &list, &typed).unwrap();
    for db in [legacy, typed.join("skyrim_world.db")] {
        let conn = Connection::open(&db).unwrap();
        let count: u32 = conn
            .query_row(
                "SELECT count(*) FROM records WHERE form_id IN (33556498,33559093)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "target and neighbour in {db:?}");
        for id in [0x0200_0811u32, 0x0200_0813].into_iter().take(
            if matches!(tag, b"BTXT" | b"ATXT" | b"NAME") {
                2
            } else {
                1
            },
        ) {
            let bytes: Vec<u8> = conn
                .query_row("SELECT data FROM records WHERE form_id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap();
            let archived = rkyv::from_bytes::<
                converter::esm::types::ArchivedRecordData,
                rkyv::rancor::Error,
            >(&bytes)
            .unwrap();
            let actual = &archived
                .subrecords
                .iter()
                .find(|s| &s.tag == tag)
                .unwrap()
                .data;
            let mut expected = payload.clone();
            for (i, offset) in offsets.iter().enumerate() {
                let word = if id == 0x0200_0813 || i == 1 {
                    0x0200_1235u32
                } else {
                    0
                };
                expected[*offset..*offset + 4].copy_from_slice(&word.to_le_bytes());
            }
            assert_eq!(
                actual, &expected,
                "{kind:?}/{tag:?} in {db:?}: only missing target words may change"
            );
        }
    }
    let conn = Connection::open(typed.join("skyrim_world.db")).unwrap();
    let missing: u32 = conn.query_row("SELECT count(*) FROM record_diagnostics WHERE reason='optional FormID 03001234 exceeds master list'", [], |r| r.get(0)).unwrap();
    assert_eq!(missing, 1);
}

/// VMAD keeps object aliases/padding and valid sibling properties.
#[test]
fn vmad_absent_target_both_readers() {
    check_layout(b"ACTI", b"VMAD");
}
/// MODS keeps names, shape indices and valid sibling entries.
#[test]
fn mods_absent_target_both_readers() {
    check_layout(b"GRAS", b"MODS");
}
/// BTXT keeps quadrant, padding and forward targets.
#[test]
fn btxt_absent_target_both_readers() {
    check_layout(b"LAND", b"BTXT");
}
/// ATXT keeps quadrant, padding, layer and forward targets.
#[test]
fn atxt_absent_target_both_readers() {
    check_layout(b"LAND", b"ATXT");
}
/// VTEX keeps valid sibling IDs and declared NULLs.
#[test]
fn vtex_absent_target_both_readers() {
    check_layout(b"LAND", b"VTEX");
}
/// Every placed-reference NAME shares typed absent-target publication.
#[test]
fn name_absent_target_both_readers() {
    for kind in [
        b"REFR", b"ACHR", b"ACRE", b"PGRE", b"PMIS", b"PHZD", b"PARW", b"PBAR", b"PBEA", b"PCON",
        b"PFLA",
    ] {
        check_layout(kind, b"NAME");
    }
}

/// Native successful self resolutions are notices after resolution, rather than loss warnings.
#[test]
fn native_projection_is_a_notice() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("Example.esm");
    fs::write(
        &path,
        [
            record(b"TES4", 0, &[]),
            record(b"STAT", 0x0200_0811, &sub(b"EDID", b"Self\0")),
        ]
        .concat(),
    )
    .unwrap();
    let merged = EsmParser::merge_plugins_form_keys(&[path]).unwrap();
    assert_eq!(merged.records.len(), 1);
    assert!(
        !merged
            .warnings
            .iter()
            .any(|line| line.contains("resolved to self"))
    );
    assert_eq!(
        merged
            .notices
            .iter()
            .filter(|line| line.contains("1 FormID occurrence(s)"))
            .count(),
        1
    );
    assert!(
        merged
            .notices
            .iter()
            .all(|line| line.starts_with("notice:"))
    );
}
