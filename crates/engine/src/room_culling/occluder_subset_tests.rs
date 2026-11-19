use super::*;
use converter::{
    EsmParser,
    config::RecordReader,
    esm::{
        exporter::{export_placed_subset_with_sources, export_to_db},
        load_order::LoadOrder,
        rooms::{NativeOccluderChange, NativeOccluderShape, apply_native_occluders},
    },
    records::read_plugins,
};
use dummy_content::inhouse_dialogue::{plugin, record, subrecord};
use std::{fs, path::Path};

const FIRST: u32 = 0x900;
const SECOND: u32 = 0x901;
const OTHER: u32 = 0x902;
const OTHER_MARKER: u32 = 0x802;
const MOD_MARKER: u32 = 0x0100_0800;

fn group(kind: u32, cell: u32, bytes: Vec<u8>) -> Vec<u8> {
    [
        b"GRUP".as_slice(),
        &((bytes.len() + 24) as u32).to_le_bytes(),
        &cell.to_le_bytes(),
        &kind.to_le_bytes(),
        &[0; 8],
        &bytes,
    ]
    .concat()
}

fn cell_contents(cell: u32, bytes: Vec<u8>) -> Vec<u8> {
    group(6, cell, group(9, cell, bytes))
}

fn marker(id: u32, x: f32, links: &[&[u8]]) -> Vec<u8> {
    let mut shape = [0; 32];
    shape[..4].copy_from_slice(&8.0f32.to_le_bytes());
    shape[8..12].copy_from_slice(&8.0f32.to_le_bytes());
    shape[28..].copy_from_slice(&3u32.to_le_bytes());
    let placement: Vec<_> = [x, 20.0, 0.0, 0.0, 0.0, 0.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    let mut payload = [
        subrecord(b"NAME", &0x17u32.to_le_bytes()),
        subrecord(b"XPRM", &shape),
    ]
    .concat();
    for bytes in links {
        payload.extend(subrecord(b"XORD", bytes));
    }
    payload.extend(subrecord(b"DATA", &placement));
    record(b"REFR", id, 0, &payload)
}

fn converted_pack(dir: &Path) -> (Connection, PathBuf, Vec<u8>) {
    let hedr = [1.7f32.to_le_bytes(), [0; 4], 0x800u32.to_le_bytes()].concat();
    let header = record(b"TES4", 0, 1, &subrecord(b"HEDR", &hedr));
    let base = dir.join("Skyrim.esm");
    let db = dir.join("skyrim_world.db");
    fs::write(
        &base,
        [
            header.clone(),
            record(b"STAT", 0x17, 0, &subrecord(b"EDID", b"PlaneMarker\0")),
            record(b"CELL", FIRST, 0, &subrecord(b"DATA", &[1, 0])),
            record(b"CELL", SECOND, 0, &subrecord(b"DATA", &[1, 0])),
            record(b"CELL", OTHER, 0, &subrecord(b"DATA", &[1, 0])),
            cell_contents(OTHER, marker(OTHER_MARKER, 0.0, &[])),
        ]
        .concat(),
    )
    .unwrap();
    EsmParser::convert_plugins_with_reader(
        std::slice::from_ref(&base),
        &db,
        RecordReader::Inhouse,
        dir,
    )
    .unwrap();
    (Connection::open(db).unwrap(), base, header)
}

fn hides_mesh(conn: &Connection, cell: u32) -> bool {
    let bounds = MeshBounds::transformed(
        &Aabb::from_min_max(-Vec3::splat(0.2), Vec3::splat(0.2)),
        bevy::math::Affine3A::from_translation(Vec3::new(0.0, 0.0, -40.0)),
    )
    .unwrap();
    let clip = Mat4::perspective_infinite_reverse_rh(std::f32::consts::FRAC_PI_3, 1.0, 0.1);
    load_room_graph(conn, cell)
        .unwrap()
        .visibility_volumes(Vec3::ZERO, clip)
        .is_some_and(|visible| !visible.unbounded.intersects(Some(&bounds)))
}

#[test]
fn skyrim_occluder_subset_adds_moves_and_deletes_an_active_plane() {
    let dir = tempfile::tempdir().unwrap();
    let (conn, base, header) = converted_pack(dir.path());
    let patch = dir.path().join("Patch.esp");
    let paths = [base, patch.clone()];
    let replace_patch = |cell, contents| {
        fs::write(
            &patch,
            plugin(&header, &["Skyrim.esm"], 0, &cell_contents(cell, contents)),
        )
        .unwrap();
    };
    let active = || [FIRST, SECOND, OTHER].map(|cell| hides_mesh(&conn, cell));
    assert_eq!(active(), [false, false, true]);
    let original: Vec<u8> = conn
        .query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=?1",
            [OTHER_MARKER],
            |row| row.get(0),
        )
        .unwrap();

    replace_patch(FIRST, marker(MOD_MARKER, 0.0, &[]));
    let mut merged = EsmParser::merge_plugins(&paths).unwrap();
    export_to_db(
        &conn,
        &HashMap::from([(MOD_MARKER, merged.remove(&MOD_MARKER).unwrap())]),
    )
    .unwrap();
    assert_eq!(active(), [true, false, true]);

    replace_patch(SECOND, marker(MOD_MARKER, 0.0, &[]));
    let order = LoadOrder::read(&paths).unwrap();
    let mut decoded = read_plugins(&paths, &order).unwrap();
    let moved = decoded.records.remove(&MOD_MARKER).unwrap();
    export_placed_subset_with_sources(&conn, &HashMap::from([(MOD_MARKER, moved)]), &[], &order)
        .unwrap();
    assert_eq!(active(), [false, true, true]);
    assert_eq!(
        conn.query_row(
            "SELECT plugin_name,internal_id FROM formid_map WHERE form_id=?1",
            [MOD_MARKER],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)),
        )
        .unwrap(),
        ("patch.esp".into(), 0x800)
    );

    replace_patch(SECOND, record(b"REFR", MOD_MARKER, 0x20, &[]));
    let decoded = read_plugins(&paths, &order).unwrap();
    let deleted: Vec<_> = decoded
        .history
        .iter()
        .filter_map(|source| {
            (source.accepted && source.flags & 0x20 != 0)
                .then_some(source.form_id)
                .flatten()
        })
        .collect();
    assert_eq!(deleted, [MOD_MARKER]);
    export_placed_subset_with_sources(&conn, &HashMap::new(), &deleted, &order).unwrap();
    assert_eq!(active(), [false, false, true]);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM [references] WHERE id=?1",
            [MOD_MARKER],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );

    let link = [0, 0, OTHER_MARKER, 0].map(u32::to_le_bytes).concat();
    replace_patch(FIRST, marker(MOD_MARKER, 0.0, &[&[0; 3], &link]));
    let mut decoded = read_plugins(&paths, &order).unwrap();
    let linked = decoded.records.remove(&MOD_MARKER).unwrap();
    export_placed_subset_with_sources(&conn, &HashMap::from([(MOD_MARKER, linked)]), &[], &order)
        .unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT link_index,linked_ref FROM room_occluder_links WHERE ref_id=?1",
            [MOD_MARKER],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
        )
        .unwrap(),
        (6, OTHER_MARKER)
    );
    assert_eq!(active(), [false, false, false]);
    assert_eq!(
        conn.query_row(
            "SELECT payload FROM inhouse_source_records WHERE form_id=?1",
            [OTHER_MARKER],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .unwrap(),
        original
    );
}

#[test]
fn native_occluder_subset_adds_moves_and_deletes_an_active_shape() {
    let dir = tempfile::tempdir().unwrap();
    let (conn, _, _) = converted_pack(dir.path());
    let id = 0xff00_0800;
    let upsert = |cell_id, shape| NativeOccluderChange::Upsert {
        id,
        cell_id,
        shape,
        position: [0.0, 20.0, 0.0],
        rotation: [0.0; 3],
    };
    let plane = NativeOccluderShape::Plane {
        half_width: 8.0,
        half_height: 8.0,
    };
    let box_shape = NativeOccluderShape::Box {
        half_extents: [8.0; 3],
    };
    let active = || [FIRST, SECOND, OTHER].map(|cell| hides_mesh(&conn, cell));
    apply_native_occluders(&conn, &[upsert(FIRST, plane)]).unwrap();
    assert_eq!(active(), [true, false, true]);
    apply_native_occluders(&conn, &[upsert(SECOND, box_shape)]).unwrap();
    assert_eq!(active(), [false, true, true]);
    assert!(
        apply_native_occluders(
            &conn,
            &[upsert(FIRST, plane), NativeOccluderChange::Remove { id }]
        )
        .is_err()
    );
    assert_eq!(active(), [false, true, true]);
    for _ in 0..2 {
        apply_native_occluders(&conn, &[NativeOccluderChange::Remove { id }]).unwrap();
        assert_eq!(active(), [false, false, true]);
    }
}
