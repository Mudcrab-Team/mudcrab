//! Synthetic public legacy-reader publication regressions; no installed inputs.
use converter::esm::{EsmParser, cell_cache::write_cell_cache};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

fn record(tag: &[u8; 4], id: u32, flags: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}

fn group(kind: i32, id: u32, payload: Vec<u8>) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &(payload.len() as u32 + 24).to_le_bytes(),
        &id.to_le_bytes(),
        &kind.to_le_bytes(),
        &[0; 8],
        &payload,
    ]
    .concat()
}

fn plugin(root: &Path, name: &str, masters: &[&str], flags: u32, body: Vec<u8>) -> PathBuf {
    let mut header = Vec::new();
    for master in masters {
        header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
        header.extend(sub(b"DATA", &[0; 8]));
    }
    let path = root.join(name);
    fs::write(&path, [record(b"TES4", 0, flags, header), body].concat()).unwrap();
    path
}

fn fields(conn: &Connection, id: u32, tag: &[u8; 4]) -> Vec<Vec<u8>> {
    let blob: Vec<u8> = conn
        .query_row("SELECT data FROM records WHERE form_id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap();
    let decoded =
        rkyv::from_bytes::<converter::esm::types::ArchivedRecordData, rkyv::rancor::Error>(&blob)
            .unwrap();
    decoded
        .subrecords
        .into_iter()
        .filter(|field| &field.tag == tag)
        .map(|field| field.data)
        .collect()
}

fn ids(conn: &Connection) -> Vec<u32> {
    conn.prepare("SELECT form_id FROM records ORDER BY form_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn npc_faction_structures_publish_resolved_ids_and_preserve_rank_padding() {
    let dir = tempfile::tempdir().unwrap();
    let membership = |id: u32, rank: u8| [id.to_le_bytes(), [rank, 0xA1, 0xB2, 0xC3]].concat();
    let paths = vec![
        plugin(dir.path(), "Filler.esm", &[], 1, Vec::new()),
        plugin(dir.path(), "Base.esm", &[], 1, Vec::new()),
        plugin(dir.path(), "Light.esl", &[], 0x201, Vec::new()),
        plugin(
            dir.path(),
            "Patch.esp",
            &["Light.esl", "Base.esm"],
            0,
            [
                record(
                    b"NPC_",
                    0x0200_0800,
                    0,
                    [
                        sub(b"SNAM", &membership(0x0000_0801, 0xFF)),
                        sub(b"SNAM", &membership(0x0100_0802, 4)),
                        sub(b"SNAM", &membership(0x0200_0803, 7)),
                        sub(b"SNAM", &membership(0x0300_0804, 8)),
                        sub(b"SNAM", &membership(0x0000_1805, 9)),
                        sub(b"SNAM", &[0xAB; 7]),
                    ]
                    .concat(),
                ),
                record(
                    b"CLFM",
                    0x0200_0806,
                    0,
                    sub(b"SNAM", &membership(0x1234_5678, 10)),
                ),
            ]
            .concat(),
        ),
    ];
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&paths, &db).unwrap();
    let conn = Connection::open(db).unwrap();
    assert_eq!(
        fields(&conn, 0x0200_0800, b"SNAM"),
        [
            membership(0xFE00_0801, 0xFF),
            membership(0x0100_0802, 4),
            membership(0x0200_0803, 7),
            membership(0, 8),
            membership(0, 9),
            // The legacy archive keeps malformed SNAM bytes opaque; the independent
            // in-house typed reader still rejects malformed faction memberships.
            vec![0xAB; 7],
        ]
    );
    assert_eq!(
        fields(&conn, 0x0200_0806, b"SNAM"),
        [membership(0x1234_5678, 10)]
    );
}

#[test]
fn torn_payloads_preserve_earlier_winners_neighbors_and_authoritative_deletions() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(
        dir.path(),
        "Base.esm",
        &[],
        1,
        [
            record(b"STAT", 0x800, 0, sub(b"EDID", b"Earlier\0")),
            record(b"STAT", 0x803, 0, sub(b"EDID", b"Deleted\0")),
        ]
        .concat(),
    );
    // The bad inner record cannot cross its declared containing GRUP. Resume the
    // known parent sibling, retaining its original worldspace/cell context.
    let mut truncated = record(b"STAT", 0x0100_0804, 0, Vec::new());
    truncated[4..8].copy_from_slice(&100u32.to_le_bytes());
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Base.esm"],
        0,
        [
            record(b"STAT", 0x800, 0, b"EDID\x02\x00x\0DATA\x04\x00x".to_vec()),
            record(b"STAT", 0x0100_0801, 0x40000, vec![0; 3]),
            record(b"STAT", 0x0100_0802, 0, sub(b"EDID", b"Neighbor\0")),
            record(b"STAT", 0x803, 0x40020, vec![0; 3]),
            group(
                1,
                0x900,
                [
                    group(6, 0x901, truncated),
                    group(
                        6,
                        0x902,
                        record(b"STAT", 0x0100_0805, 0, sub(b"EDID", b"Sibling\0")),
                    ),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&[base, patch], &db).unwrap();
    let conn = Connection::open(db).unwrap();
    assert_eq!(ids(&conn), [0x800, 0x0100_0802, 0x0100_0805]);
    assert_eq!(fields(&conn, 0x800, b"EDID"), [b"Earlier\0".to_vec()]);
    let context: (u32, u32) = conn
        .query_row(
            "SELECT cell_id, worldspace_id FROM records WHERE form_id=?1",
            [0x0100_0805u32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(context, (0x902, 0x900));
}

#[test]
fn scalar_optional_links_clear_or_drop_locally_before_sql_publication() {
    let dir = tempfile::tempdir().unwrap();
    let paths = vec![
        plugin(dir.path(), "Base.esm", &[], 1, Vec::new()),
        plugin(dir.path(), "Filler.esm", &[], 1, Vec::new()),
        plugin(
            dir.path(),
            "Patch.esp",
            &["Base.esm"],
            0,
            [
                record(
                    b"CELL",
                    0x0100_0800,
                    0,
                    [
                        sub(b"XOWN", &0x0200_0900u32.to_le_bytes()),
                        sub(b"XCWT", &0x0200_0901u32.to_le_bytes()),
                        sub(b"XLCN", &[1, 2, 3]),
                        sub(b"XEZN", &0x0100_0902u32.to_le_bytes()),
                    ]
                    .concat(),
                ),
                record(
                    b"RACE",
                    0x0100_0801,
                    0,
                    [
                        sub(b"WKMV", &[1, 2, 3]),
                        sub(b"RNMV", &0x0100_0903u32.to_le_bytes()),
                    ]
                    .concat(),
                ),
                record(
                    b"REFR",
                    0x0100_0802,
                    0,
                    sub(b"NAME", &0x0200_0904u32.to_le_bytes()),
                ),
                record(b"STAT", 0x0100_0803, 0, sub(b"EDID", b"Neighbor\0")),
            ]
            .concat(),
        ),
    ];
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&paths, &db).unwrap();
    let conn = Connection::open(db).unwrap();
    assert_eq!(ids(&conn), [0x0200_0800, 0x0200_0801, 0x0200_0803]);
    assert_eq!(
        fields(&conn, 0x0200_0800, b"XOWN"),
        [0u32.to_le_bytes().to_vec()]
    );
    assert_eq!(
        fields(&conn, 0x0200_0800, b"XCWT"),
        [0u32.to_le_bytes().to_vec()]
    );
    assert!(fields(&conn, 0x0200_0800, b"XLCN").is_empty());
    assert_eq!(
        fields(&conn, 0x0200_0800, b"XEZN"),
        [0x0200_0902u32.to_le_bytes().to_vec()]
    );
    let movement: (Option<u32>, Option<u32>) = conn
        .query_row(
            "SELECT walk_movt_id, run_movt_id FROM race_movement_links WHERE id=?1",
            [0x0200_0801u32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(movement, (None, Some(0x0200_0903)));
    assert!(fields(&conn, 0x0200_0801, b"WKMV").is_empty());
}

fn heightmap(offset: f32) -> Vec<u8> {
    [offset.to_le_bytes().to_vec(), vec![0; 33 * 33 + 3]].concat()
}

#[test]
fn malformed_terrain_and_native_arrays_cannot_replace_complete_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(
        dir.path(),
        "Base.esm",
        &[],
        1,
        [
            group(
                6,
                0x900,
                record(b"LAND", 0x800, 0, sub(b"VHGT", &heightmap(2.0))),
            ),
            record(b"GRAS", 0x810, 0, sub(b"EDID", b"EarlierGrass\0")),
        ]
        .concat(),
    );
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Base.esm"],
        0,
        [
            group(
                6,
                0x900,
                [
                    record(b"LAND", 0x800, 0, sub(b"VHGT", &heightmap(f32::NAN))),
                    record(b"LAND", 0x800, 0, sub(b"VNML", &[1; 3])),
                    record(b"LAND", 0x800, 0, sub(b"BTXT", &[0; 7])),
                    record(b"LAND", 0x800, 0, sub(b"VTEX", &[0; 3])),
                ]
                .concat(),
            ),
            group(
                6,
                0x901,
                record(b"LAND", 0x0100_0801, 0, sub(b"VHGT", &heightmap(3.0))),
            ),
            record(b"GRAS", 0x810, 0, sub(b"MODS", &1u32.to_le_bytes())),
            record(b"STAT", 0x0100_0802, 0, sub(b"EDID", b"Neighbor\0")),
        ]
        .concat(),
    );
    let paths = [base, patch];
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&paths, &db).unwrap();
    let conn = Connection::open(db).unwrap();
    assert_eq!(ids(&conn), [0x800, 0x810, 0x0100_0801, 0x0100_0802]);
    assert_eq!(fields(&conn, 0x800, b"VHGT"), [heightmap(2.0)]);
    assert_eq!(fields(&conn, 0x810, b"EDID"), [b"EarlierGrass\0".to_vec()]);
    let merged = EsmParser::merge_plugins(&paths).unwrap();
    let cache_path = dir.path().join("cell_cache.rkyv");
    assert_eq!(write_cell_cache(&merged, &cache_path).unwrap(), 2);
    let bytes = fs::read(cache_path).unwrap();
    let cache = rkyv::from_bytes::<shared::CellCache, rkyv::rancor::Error>(&bytes).unwrap();
    assert_eq!(cache.cells[0].cell_id, 0x900);
    assert_eq!(cache.cells[0].heights, vec![16.0; 33 * 33]);
    assert_eq!(cache.cells[1].cell_id, 0x901);
    assert_eq!(cache.cells[1].heights, vec![24.0; 33 * 33]);
}

#[test]
fn unusable_movement_override_does_not_claim_a_deletion_alias() {
    let dir = tempfile::tempdir().unwrap();
    let setting = |id, value: f32| {
        record(
            b"GMST",
            id,
            0,
            [
                sub(b"EDID", b"fJumpHeightMin\0"),
                sub(b"DATA", &value.to_le_bytes()),
            ]
            .concat(),
        )
    };
    let speeds: Vec<u8> = (0..10).flat_map(|_| 200.0f32.to_le_bytes()).collect();
    let movement = |data: &[u8]| {
        record(
            b"MOVT",
            0x0003_580D,
            0,
            [sub(b"EDID", b"NPC_Default_MT\0"), sub(b"SPED", data)].concat(),
        )
    };
    let base = plugin(
        dir.path(),
        "Base.esm",
        &[],
        1,
        [setting(0x800, 50.0), movement(&speeds)].concat(),
    );
    let mut unusable_speeds = speeds.clone();
    unusable_speeds[..4].copy_from_slice(&f32::NAN.to_le_bytes());
    let patch = plugin(
        dir.path(),
        "Patch.esp",
        &["Base.esm"],
        0,
        [
            setting(0x0100_0801, f32::NAN),
            record(b"GMST", 0x0100_0801, 0x20, Vec::new()),
            movement(&unusable_speeds),
            record(b"STAT", 0x0100_0802, 0, sub(b"EDID", b"Neighbor\0")),
        ]
        .concat(),
    );
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&[base, patch], &db).unwrap();
    let conn = Connection::open(db).unwrap();
    assert_eq!(ids(&conn), [0x800, 0x0003_580D, 0x0100_0802]);
    let value: f32 = conn
        .query_row(
            "SELECT value FROM movement_game_settings WHERE id=2048",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value.to_bits(), 50.0f32.to_bits());
    let speed: f32 = conn
        .query_row(
            "SELECT forward_walk FROM movement_types WHERE id=219149",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(speed.to_bits(), 200.0f32.to_bits());
    assert_eq!(fields(&conn, 0x0003_580D, b"SPED"), [speeds]);
}

/// Encode asymmetric primary object properties without any record-specific fragments.
fn compatibility_vmad(object_format: u16, references: [u32; 3]) -> Vec<u8> {
    let mut bytes = [
        5u16.to_le_bytes(),
        object_format.to_le_bytes(),
        1u16.to_le_bytes(),
    ]
    .concat();
    let string = |bytes: &mut Vec<u8>, text: &[u8]| {
        bytes.extend_from_slice(&(text.len() as u16).to_le_bytes());
        bytes.extend_from_slice(text);
    };
    let object = |bytes: &mut Vec<u8>, reference: u32| {
        if object_format == 1 {
            bytes.extend_from_slice(&reference.to_le_bytes());
            bytes.extend_from_slice(&(-1i16).to_le_bytes());
            bytes.extend_from_slice(&0xA7B8u16.to_le_bytes());
        } else {
            bytes.extend_from_slice(&0xA7B8u16.to_le_bytes());
            bytes.extend_from_slice(&(-1i16).to_le_bytes());
            bytes.extend_from_slice(&reference.to_le_bytes());
        }
    };
    string(&mut bytes, b"Compatibility");
    bytes.push(0);
    bytes.extend_from_slice(&2u16.to_le_bytes());
    string(&mut bytes, b"Single");
    bytes.extend_from_slice(&[1, 1]);
    object(&mut bytes, references[0]);
    string(&mut bytes, b"Array");
    bytes.extend_from_slice(&[11, 1]);
    bytes.extend_from_slice(&2u32.to_le_bytes());
    object(&mut bytes, references[1]);
    object(&mut bytes, references[2]);
    bytes
}

/// Legacy VMAD links retain admitted compatibility identities while torn overrides recover locally.
#[test]
fn vanilla_vmad_compatibility_preserves_scripts_and_complete_predecessors() {
    let dir = tempfile::tempdir().unwrap();
    let base_vmad = compatibility_vmad(2, [0x1932; 3]);
    let mut torn = base_vmad.clone();
    torn.pop();
    let paths = vec![
        plugin(dir.path(), "Filler.esm", &[], 1, Vec::new()),
        plugin(
            dir.path(),
            "Base.esm",
            &[],
            1,
            [
                record(b"ACTI", 0x1932, 0, sub(b"EDID", b"BaseTarget\0")),
                record(b"ACTI", 0x820, 0, sub(b"VMAD", &base_vmad)),
            ]
            .concat(),
        ),
        plugin(
            dir.path(),
            "Patch.esp",
            &["Base.esm"],
            0,
            [
                record(
                    b"ACTI",
                    0x0300_1931,
                    0,
                    sub(b"EDID", b"CompatibilityTarget\0"),
                ),
                record(
                    b"ACTI",
                    0x0100_1820,
                    0,
                    sub(
                        b"VMAD",
                        &compatibility_vmad(1, [0x0300_1931, 0x1932, 0x0300_1931]),
                    ),
                ),
                record(
                    b"ACTI",
                    0x0100_1821,
                    0,
                    sub(
                        b"VMAD",
                        &compatibility_vmad(2, [0x0300_1931, 0x1932, 0x0300_1931]),
                    ),
                ),
                record(b"ACTI", 0x820, 0, sub(b"VMAD", &torn)),
                record(b"STAT", 0x0100_1822, 0, sub(b"EDID", b"Neighbor\0")),
            ]
            .concat(),
        ),
    ];
    let source_bytes: Vec<_> = paths
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&paths, &db).unwrap();
    let conn = Connection::open(db).unwrap();
    assert_eq!(
        ids(&conn),
        [
            0x0100_0820,
            0x0100_1932,
            0x0200_1820,
            0x0200_1821,
            0x0200_1822,
            0x0200_1931
        ]
    );
    assert_eq!(
        fields(&conn, 0x0100_0820, b"VMAD"),
        [compatibility_vmad(2, [0x0100_1932; 3])]
    );
    for (id, format) in [(0x0200_1820u32, 1), (0x0200_1821, 2)] {
        let expected = compatibility_vmad(format, [0x0200_1931, 0x0100_1932, 0x0200_1931]);
        assert_eq!(
            fields(&conn, id, b"VMAD").as_slice(),
            std::slice::from_ref(&expected)
        );
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT vmad FROM scripts WHERE form_id=?1 AND script_name='Compatibility'",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, expected);
    }
    let script_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM scripts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(script_count, 3);
    for (path, before) in paths.iter().zip(source_bytes) {
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

/// Preserve non-scalar WRLD payloads, including a four-byte texture string, beside real links.
#[test]
fn vanilla_wrld_large_reference_and_texture_payloads_survive_publication() {
    let dir = tempfile::tempdir().unwrap();
    let large_references = [
        (-3i16).to_le_bytes().to_vec(),
        7i16.to_le_bytes().to_vec(),
        2u32.to_le_bytes().to_vec(),
        0x0000_0911u32.to_le_bytes().to_vec(),
        2i16.to_le_bytes().to_vec(),
        (-5i16).to_le_bytes().to_vec(),
        0x0100_0912u32.to_le_bytes().to_vec(),
        (-9i16).to_le_bytes().to_vec(),
        11i16.to_le_bytes().to_vec(),
    ]
    .concat();
    let long_texture = b"Textures\\Terrain\\World.dds\0".to_vec();
    let paths = vec![
        plugin(dir.path(), "Filler.esm", &[], 1, Vec::new()),
        plugin(dir.path(), "Base.esm", &[], 1, Vec::new()),
        plugin(
            dir.path(),
            "Patch.esp",
            &["Base.esm"],
            0,
            record(
                b"WRLD",
                0x0100_0800,
                0,
                [
                    sub(b"RNAM", &large_references),
                    sub(b"TNAM", &long_texture),
                    sub(b"TNAM", b"lod\0"),
                    sub(b"NAM2", &0x0000_0920u32.to_le_bytes()),
                    sub(b"CNAM", &0x0100_0921u32.to_le_bytes()),
                ]
                .concat(),
            ),
        ),
    ];
    let source_bytes: Vec<_> = paths
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let db = dir.path().join("world.db");
    EsmParser::convert_plugins(&paths, &db).unwrap();
    let conn = Connection::open(db).unwrap();
    assert_eq!(ids(&conn), [0x0200_0800]);
    assert_eq!(fields(&conn, 0x0200_0800, b"RNAM"), [large_references]);
    assert_eq!(
        fields(&conn, 0x0200_0800, b"TNAM"),
        [long_texture, b"lod\0".to_vec()]
    );
    assert_eq!(
        fields(&conn, 0x0200_0800, b"NAM2"),
        [0x0100_0920u32.to_le_bytes().to_vec()]
    );
    assert_eq!(
        fields(&conn, 0x0200_0800, b"CNAM"),
        [0x0200_0921u32.to_le_bytes().to_vec()]
    );
    for (path, before) in paths.iter().zip(source_bytes) {
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}
