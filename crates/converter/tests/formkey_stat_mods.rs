//! STAT MODS uses optional source-master links in both readers after the main merge.
use converter::esm::{EsmParser, inhouse};
use rusqlite::Connection;
use std::fs;

/// Frame a synthetic subrecord independently of either reader.
fn sub(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(data.len() as u16).to_le_bytes(), data].concat()
}

/// Frame a synthetic record with independently chosen source identity and flags.
fn record(tag: &[u8; 4], id: u32, flags: u32, data: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(data.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        data,
    ]
    .concat()
}

/// Read every archived occurrence of one field, preserving array order and NULL words.
fn fields(db: &Connection, id: u32, tag: &[u8; 4]) -> Vec<Vec<u8>> {
    let bytes: Vec<u8> = db
        .query_row("SELECT data FROM records WHERE form_id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    rkyv::from_bytes::<converter::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&bytes)
        .unwrap()
        .subrecords
        .into_iter()
        .filter(|s| &s.tag == tag)
        .map(|s| s.data)
        .collect()
}

/// Publish localized Base/Filler/Patch with forward self, past-self, master and absent links.
fn check_stat_mods(light: bool) {
    let tag = b"MODS";
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("Data");
    fs::create_dir(&data).unwrap();
    let target = b"TXST";
    fs::write(
        data.join("Base.esm"),
        [record(b"TES4", 0, 0, &[]), record(target, 0x901, 0, &[])].concat(),
    )
    .unwrap();
    // A wrong global-slot interpretation can select this unrelated Filler record.
    fs::write(
        data.join("Filler.esm"),
        [record(b"TES4", 0, 0, &[]), record(target, 0x900, 0, &[])].concat(),
    )
    .unwrap();
    let words = [0x0100_0900u32, 0x0300_0900, 0x0300_0bad, 0, 0x901];
    let mut body = Vec::new();
    for (index, word) in words.into_iter().enumerate() {
        let payload = mods(word);
        let kind = b"STAT";
        body.extend(record(
            kind,
            0x0100_0800 + index as u32,
            0,
            &sub(tag, &payload),
        ));
    }
    body.extend(record(target, 0x0100_0900, 0, &[])); // Forward source header.
    body.extend(record(
        b"STAT",
        0x0100_0a00,
        0,
        &sub(b"EDID", b"Neighbor\0"),
    ));
    fs::write(
        data.join("Patch.esp"),
        [
            record(
                b"TES4",
                0,
                0x80 | if light { 0x200 } else { 0 },
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
    let legacy = root.path().join("legacy.db");
    EsmParser::convert_plugins(&paths, &legacy).unwrap();
    let list = root.path().join("plugins.txt");
    fs::write(&list, "*Base.esm\n*Filler.esm\n*Patch.esp\n").unwrap();
    let typed = root.path().join("typed");
    inhouse::export_record_bundle(&data, &list, &typed).unwrap();
    // Check the typed control first, then require the identical literal contract from legacy.
    let own = if light { 0xfe00_0000u32 } else { 0x0200_0000 };
    for path in [typed.join("skyrim_world.db"), legacy] {
        let db = Connection::open(&path).unwrap();
        for (index, word) in [own | 0x900, own | 0x900, 0, 0, 0x901]
            .into_iter()
            .enumerate()
        {
            let expected = mods(word);
            let (target, disposition): (u32, String) = db
                .query_row(
                    "SELECT texture_set_id,disposition FROM base_texture_swaps WHERE base_id=?1",
                    [own | (0x800 + index as u32)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(target, word, "{path:?}");
            assert_eq!(
                disposition,
                if word == 0 {
                    "missing_texture_set"
                } else {
                    "texture_set"
                }
            );
            assert_eq!(
                fields(&db, own | (0x800 + index as u32), tag),
                vec![expected],
                "{tag:?} in {path:?}"
            );
        }
        assert_eq!(
            fields(&db, own | 0xa00, b"EDID"),
            vec![b"Neighbor\0".to_vec()]
        );
    }
}

/// Count/name/TXST/index framing is independent of the production offset decoder.
fn mods(word: u32) -> Vec<u8> {
    [
        1u32.to_le_bytes().as_slice(),
        &7u32.to_le_bytes(),
        b"Shape:3",
        &word.to_le_bytes(),
        &3i32.to_le_bytes(),
    ]
    .concat()
}

/// Declared self and past-self links select Patch, never the decoy runtime slot.
#[test]
fn stat_mods_both_readers_resolve_full_self_and_clear_absent_targets() {
    check_stat_mods(false);
}

/// Light self links pack into FE while absent links clear and the neighbor survives.
#[test]
fn stat_mods_both_readers_resolve_light_self_and_clear_absent_targets() {
    check_stat_mods(true);
}
