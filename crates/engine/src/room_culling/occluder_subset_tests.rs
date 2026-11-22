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
        .visibility_volumes(Vec3::ZERO, Vec3::ZERO, clip)
        .is_some_and(|visible| !visible.unbounded.intersects(Some(&bounds)))
}

#[test]
fn full_export_keeps_a_cross_cell_occluder_target_from_acting_independently() {
    let dir = tempfile::tempdir().unwrap();
    let (original, base, header) = converted_pack(dir.path());
    assert!(hides_mesh(&original, OTHER));
    let patch = dir.path().join("Patch.esp");
    let link = [0, 0, OTHER_MARKER, 0].map(u32::to_le_bytes).concat();
    for (name, bytes, verified, expected_target, target_hides_mesh) in [
        ("empty", &[0; 16][..], true, None, true),
        ("short", &link[..12], false, None, false),
        ("linked", &link, true, Some(OTHER_MARKER), false),
    ] {
        fs::write(
            &patch,
            plugin(
                &header,
                &["Skyrim.esm"],
                0,
                &cell_contents(FIRST, marker(MOD_MARKER, 0.0, &[bytes])),
            ),
        )
        .unwrap();
        let db = dir.path().join(format!("{name}.db"));
        EsmParser::convert_plugins_with_reader(
            &[base.clone(), patch.clone()],
            &db,
            RecordReader::Inhouse,
            dir.path(),
        )
        .unwrap();
        let conn = Connection::open(db).unwrap();
        let (link_verified, target): (bool, Option<u32>) = conn
            .query_row(
                "SELECT value,(SELECT linked_ref FROM room_occluder_links WHERE ref_id=?1)
                 FROM room_graph_stats WHERE name='occlusion_links_verified'",
                [MOD_MARKER],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (link_verified, target),
            (verified, expected_target),
            "{name}"
        );
        assert_eq!(hides_mesh(&conn, OTHER), target_hides_mesh, "{name}");
    }
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

fn active_planes(conn: &Connection, cell: u32) -> Vec<u32> {
    let mut ids: Vec<_> = load_room_graph(conn, cell)
        .unwrap()
        .occluders
        .values()
        .flatten()
        .map(|shape| shape.ref_id)
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn typed_subset_replaces_the_single_slot_used_by_the_loaded_pair() {
    let dir = tempfile::tempdir().unwrap();
    let (conn, base, header) = converted_pack(dir.path());
    let patch = dir.path().join("Patch.esp");
    let paths = [base, patch.clone()];
    let apply = |fields: &[&[u8]]| {
        fs::write(
            &patch,
            plugin(
                &header,
                &["Skyrim.esm"],
                0,
                &cell_contents(OTHER, marker(MOD_MARKER, 8.0, fields)),
            ),
        )
        .unwrap();
        let order = LoadOrder::read(&paths).unwrap();
        let mut decoded = read_plugins(&paths, &order).unwrap();
        export_placed_subset_with_sources(
            &conn,
            &HashMap::from([(MOD_MARKER, decoded.records.remove(&MOD_MARKER).unwrap())]),
            &[],
            &order,
        )
        .unwrap();
    };
    let link = [0, OTHER_MARKER, 0, 0].map(u32::to_le_bytes).concat();
    conn.execute_batch("ALTER TABLE room_occluder_links DROP COLUMN single_target_verified")
        .unwrap();
    apply(&[&link]);
    assert_eq!(active_planes(&conn, OTHER), [OTHER_MARKER, MOD_MARKER]);
    let stored = || {
        conn.query_row(
            "SELECT link_index,single_target_verified FROM room_occluder_links WHERE ref_id=?1",
            [MOD_MARKER],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, bool>(1)?)),
        )
        .unwrap()
    };
    assert_eq!(stored(), (1, true));
    let override_link = [0, 0, 0, OTHER_MARKER].map(u32::to_le_bytes).concat();
    apply(&[&override_link]);
    assert_eq!(stored(), (3, true));
    let order = LoadOrder::read(&paths).unwrap();
    export_placed_subset_with_sources(&conn, &HashMap::new(), &[MOD_MARKER], &order).unwrap();
    assert_eq!(active_planes(&conn, OTHER), [OTHER_MARKER]);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM room_occluder_links WHERE ref_id=?1",
            [MOD_MARKER],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn native_subset_replaces_the_link_and_named_owner_used_by_the_loaded_pair() {
    let dir = tempfile::tempdir().unwrap();
    let (conn, _, _) = converted_pack(dir.path());
    let id = 0xff00_0800;
    let room = 0x810;
    super::tests::insert_room(&conn, room, OTHER, [0.0, 20.0, 0.0]);
    conn.execute("UPDATE room_bounds SET flags=256 WHERE ref_id=?1", [room])
        .unwrap();
    let apply = |slots, owner| {
        apply_native_occluders(
            &conn,
            &[NativeOccluderChange::Upsert {
                id,
                cell_id: OTHER,
                position: [0.0, 20.0, 0.0],
                rotation: [0.0; 3],
                shape: NativeOccluderShape::PlaneWithReferences {
                    half_width: 8.0,
                    half_height: 8.0,
                    links: Some(slots),
                    room: Some(owner),
                },
            }],
        )
        .unwrap();
    };
    apply([0, OTHER_MARKER, 0, 0], room);
    assert_eq!(active_planes(&conn, OTHER), [OTHER_MARKER, id]);
    apply([0, 0, 0, OTHER_MARKER], room + 1);
    assert!(active_planes(&conn, OTHER).is_empty());
    apply([OTHER_MARKER, 0, 0, 0], room);
    assert_eq!(active_planes(&conn, OTHER), [OTHER_MARKER, id]);
    let peer = id + 1;
    apply_native_occluders(
        &conn,
        &[NativeOccluderChange::Upsert {
            id: peer,
            cell_id: OTHER,
            position: [0.0, 20.0, 0.0],
            rotation: [0.0; 3],
            shape: NativeOccluderShape::Plane {
                half_width: 8.0,
                half_height: 8.0,
            },
        }],
    )
    .unwrap();
    apply([peer, 0, 0, 0], room);
    assert_eq!(active_planes(&conn, OTHER), [OTHER_MARKER, id, peer]);
    for _ in 0..2 {
        apply_native_occluders(&conn, &[NativeOccluderChange::Remove { id: peer }]).unwrap();
        assert_eq!(active_planes(&conn, OTHER), [OTHER_MARKER]);
    }
    apply_native_occluders(&conn, &[NativeOccluderChange::Remove { id }]).unwrap();
    assert_eq!(active_planes(&conn, OTHER), [OTHER_MARKER]);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM room_reference_rules WHERE reference_id=?1",
            [id],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
}
