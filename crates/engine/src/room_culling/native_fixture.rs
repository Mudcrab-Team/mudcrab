use super::*;
use bevy::{camera::primitives::Aabb, math::Affine3A};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Bounds {
    min: [f32; 3],
    max: [f32; 3],
}

#[derive(Deserialize)]
struct Campaign {
    schema: u32,
    mesh_bounds_runtime: Bounds,
    cases: Vec<Observation>,
}

#[derive(Deserialize)]
struct NativeRoom {
    role: String,
    form: u32,
    center: [f32; 3],
    half: [f32; 3],
}

#[derive(Deserialize)]
struct NativePortal {
    role: String,
    form: u32,
    center: [f32; 3],
    half: [f32; 2],
    rooms: [u32; 2],
    corners: [[f32; 3]; 4],
}

#[derive(Deserialize)]
struct Marker {
    room: String,
    label: String,
    position: [f32; 3],
    sphere: [f32; 4],
    native: bool,
}

#[derive(Deserialize)]
struct Observation {
    name: String,
    window: usize,
    camera: [f32; 3],
    near_shifted: [f32; 3],
    frustum: [f32; 6],
    rooms: Vec<NativeRoom>,
    portals: Vec<NativePortal>,
    markers: Vec<Marker>,
    native_controls: BTreeMap<String, String>,
}

fn runtime(point: [f32; 3]) -> Vec3 {
    Vec3::from_array(shared::coordinates::creation_to_runtime_vector(point))
}

fn room_database(rooms: &[NativeRoom]) -> Connection {
    let connection = super::tests::authored_database();
    for room in rooms {
        connection.execute(
            "INSERT INTO room_bounds(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_y,half_z,flags) VALUES(?1,7,?2,?3,?4,0,0,0,?5,?6,?7,128)",
            rusqlite::params![room.form, room.center[0], room.center[1], room.center[2], room.half[0], room.half[1], room.half[2]],
        ).unwrap();
    }
    connection
}

fn graph(observation: &Observation) -> RoomGraph {
    let connection = room_database(&observation.rooms);
    for portal in &observation.portals {
        connection.execute(
            "INSERT INTO room_portals(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_z,room_a,room_b,unbounded_a,unbounded_b) VALUES(?1,7,?2,?3,?4,0,0,0,?5,?6,?7,?8,0,0)",
            rusqlite::params![portal.form, portal.center[0], portal.center[1], portal.center[2], portal.half[0], portal.half[1], portal.rooms[0], portal.rooms[1]],
        ).unwrap();
    }
    let graph = load_room_graph(&connection, 7).unwrap();
    assert!(graph.authored.as_ref().is_some_and(|a| a.complete));
    assert!(!graph.incomplete);
    assert!(graph.overlaps.is_empty() && graph.links.is_empty() && graph.occluders.is_empty());
    for source in &observation.portals {
        let loaded = graph
            .portals
            .iter()
            .find(|p| p.ref_id == source.form)
            .unwrap();
        for point in source.corners.map(runtime) {
            assert!(
                loaded.corners.iter().any(|p| p.distance(point) < 0.001),
                "{}",
                source.role
            );
        }
    }
    graph
}

#[test]
#[ignore = "requires sealed native fixture input and a fresh output path"]
fn native_portal_fixture_comparison() {
    let input = std::env::var("MUDCRAB_PORTAL_FIXTURE_INPUT").unwrap();
    let campaign: Campaign = serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
    assert_eq!(campaign.schema, 1);
    assert_eq!(campaign.cases.len(), 24);
    let model = Aabb::from_min_max(
        Vec3::from_array(campaign.mesh_bounds_runtime.min),
        Vec3::from_array(campaign.mesh_bounds_runtime.max),
    );
    let mut results = Vec::new();
    for observation in campaign.cases {
        let graph = graph(&observation);
        let eye = runtime(observation.camera);
        let starts = graph.rooms_containing(eye);
        assert_eq!(starts.len(), 1);
        assert_eq!(
            starts,
            graph.rooms_containing(runtime(observation.near_shifted))
        );
        let [left, right, top, bottom, near, far] = observation.frustum;
        assert!((left + right).abs() < 0.00001 && (top + bottom).abs() < 0.00001);
        assert!(near > 0.0 && far > 1500.0);
        let clip = Mat4::perspective_infinite_reverse_rh(2.0 * top.atan(), right / top, near)
            * Mat4::from_translation(-eye);
        let visible = graph
            .visibility_volumes(eye, runtime(observation.near_shifted), clip)
            .unwrap();
        let mut bits: BTreeMap<String, String> = BTreeMap::new();
        let mut native_bits: BTreeMap<String, String> = BTreeMap::new();
        let mut judgments = Vec::new();
        for marker in &observation.markers {
            let source = observation
                .rooms
                .iter()
                .find(|r| r.role == marker.room)
                .unwrap();
            let owner = graph
                .rooms
                .iter()
                .position(|r| r.ref_id == source.form)
                .unwrap();
            let mesh = MeshBounds::transformed(
                &model,
                Affine3A::from_translation(runtime(marker.position)),
            )
            .unwrap();
            let center = runtime([marker.sphere[0], marker.sphere[1], marker.sphere[2]]);
            let radius = marker.sphere[3];
            assert!(radius > 0.0 && radius.is_finite());
            let cube = MeshBounds::transformed(
                &Aabb::from_min_max(-Vec3::splat(radius), Vec3::splat(radius)),
                Affine3A::from_translation(center),
            )
            .unwrap();
            let point = MeshBounds::transformed(
                &Aabb::from_min_max(Vec3::ZERO, Vec3::ZERO),
                Affine3A::from_translation(center),
            )
            .unwrap();
            let actual = visible.rooms[owner].intersects(Some(&mesh));
            let expanded = visible.rooms[owner].intersects(Some(&cube));
            let collapsed = visible.rooms[owner].intersects(Some(&point));
            assert_eq!(
                (actual, actual),
                (expanded, collapsed),
                "{} {}/{} depends on bound representation",
                observation.name,
                marker.room,
                marker.label
            );
            bits.entry(marker.room.clone())
                .or_default()
                .push(if actual { '1' } else { '0' });
            native_bits
                .entry(marker.room.clone())
                .or_default()
                .push(if marker.native { '1' } else { '0' });
            judgments.push(
                serde_json::json!({"room": marker.room, "marker": marker.label,
                "native": marker.native, "mudcrab_mesh_aabb": actual,
                "mudcrab_native_sphere_cube": expanded, "mudcrab_center": collapsed}),
            );
        }
        assert_eq!(
            native_bits, observation.native_controls,
            "{} native marker controls, window {}",
            observation.name, observation.window
        );
        assert_eq!(bits, observation.native_controls, "{}", observation.name);
        results.push(
            serde_json::json!({"case": observation.name, "window": observation.window,
            "native": observation.native_controls, "mudcrab": bits, "markers": judgments}),
        );
    }
    let output = std::env::var("MUDCRAB_PORTAL_FIXTURE_OUTPUT").unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .unwrap();
    serde_json::to_writer_pretty(
        &mut file,
        &serde_json::json!({"schema": 1,
        "test_executable": std::env::current_exe().unwrap(), "observations": results}),
    )
    .unwrap();
}

#[derive(Deserialize)]
struct OverlapFixture {
    eye: [f32; 3],
    lookup: [f32; 3],
    frustum: [f32; 6],
    mesh: Bounds,
    rooms: Vec<NativeRoom>,
    portals: Vec<OverlapPortal>,
    markers: Vec<RoomMarker>,
    captures: [OverlapCapture; 2],
}

#[derive(Deserialize)]
struct OverlapPortal {
    id: u32,
    center: [f32; 3],
    half: [f32; 2],
    rooms: [u32; 2],
}

#[derive(Deserialize)]
struct RoomMarker {
    owner: u32,
    position: [f32; 3],
}

#[derive(Deserialize)]
struct OverlapCapture {
    order: [u32; 4],
    starts: [u32; 2],
    visible: [bool; 6],
}

#[test]
fn overlapping_starts_match_both_retail_renders() {
    let fixture: OverlapFixture = serde_json::from_str(include_str!(
        "../../tests/fixtures/overlapping-room-starts.json"
    ))
    .unwrap();
    let connection = room_database(&fixture.rooms);
    let model = Aabb::from_min_max(fixture.mesh.min.into(), fixture.mesh.max.into());
    let eye = runtime(fixture.eye);
    let lookup = runtime(fixture.lookup);
    let [_, right, top, _, near, _] = fixture.frustum;
    let clip = Mat4::perspective_infinite_reverse_rh(2.0 * top.atan(), right / top, near)
        * Mat4::from_translation(-eye);
    let expected = fixture.captures.each_ref().map(|capture| capture.visible);
    let actual = fixture.captures.map(|capture| {
        // Rebuild indices in the captured graph order, retaining every placed FormID.
        let mut graph = RoomGraph::default();
        for id in capture.order {
            let room = fixture.rooms.iter().find(|r| r.form == id).unwrap();
            graph
                .rooms
                .push(Room::from_creation(id, room.center, [0.; 3], room.half));
        }
        let indices: HashMap<_, _> = capture
            .order
            .into_iter()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        let index = |id| indices[&id];
        for portal in &fixture.portals {
            graph.portals.push(Portal::from_creation(
                portal.id,
                portal.center,
                [0.; 3],
                portal.half,
                portal.rooms.map(|id| PortalSide::Room(index(id))),
            ));
        }
        graph.authored =
            assignment::load(&connection, 7, &graph.rooms, &indices, &mut graph.portals).unwrap();
        assert!(graph.authored.as_ref().unwrap().complete);
        assert_eq!(graph.rooms_containing(lookup), capture.starts.map(index));
        let visible = graph.visibility_volumes(eye, lookup, clip).unwrap();
        fixture
            .markers
            .iter()
            .map(|marker| {
                let bounds = MeshBounds::transformed(
                    &model,
                    Affine3A::from_translation(runtime(marker.position)),
                )
                .unwrap();
                assignment::draws(
                    &[assignment::Owner::Room(index(marker.owner))],
                    &graph.portals,
                    &visible,
                    Some(&bounds),
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(actual, expected, "left, gap, right in C followed by D");
}

#[derive(Deserialize)]
struct OperatorFixture {
    mesh: Bounds,
    markers: [[f32; 3]; 3],
    cases: Vec<OperatorCase>,
}

#[derive(Deserialize)]
struct OperatorCase {
    name: String,
    rooms: Vec<NativeRoom>,
    portals: Vec<OverlapPortal>,
    /// The one authored independent plane, in Creation coordinates.
    occluder: Option<OperatorOccluder>,
    start: u32,
    destination: Option<u32>,
    eye: [f32; 3],
    near_shifted: [f32; 3],
    frustum: [f32; 6],
    visible: [bool; 3],
}

#[derive(Deserialize)]
struct OperatorOccluder {
    explicit: bool,
    id: u32,
    center: [f32; 3],
    half: [f32; 3],
    owner: u32,
}

#[test]
fn operator_limit_matches_retail_marker_visibility() {
    let fixture: OperatorFixture = serde_json::from_str(include_str!(
        "../../tests/fixtures/portal-operator-limit.json"
    ))
    .unwrap();
    assert_eq!(fixture.cases.len(), 14);
    let model = Aabb::from_min_max(fixture.mesh.min.into(), fixture.mesh.max.into());
    for case in fixture.cases {
        let db = room_database(&case.rooms);
        for portal in case.portals {
            let [x, y, z] = portal.center;
            let [a, b] = portal.rooms;
            db.execute(
                "INSERT INTO room_portals(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,half_x,half_z,room_a,room_b,unbounded_a,unbounded_b)
                 VALUES(?1,7,?2,?3,?4,0,0,0,?5,?6,?7,?8,0,?9)",
                rusqlite::params![portal.id, x, y, z, portal.half[0], portal.half[1], a, (b != 0).then_some(b), b == 0],
            ).unwrap();
        }
        if let Some(occluder) = &case.occluder {
            super::tests::add_occluder_tables(&db);
            let [x, y, z] = occluder.center;
            let [hx, hy, hz] = occluder.half;
            db.execute(
                "INSERT INTO room_occluders VALUES(?1,7,3,?2,?3,?4,0,0,0,?5,?6,?7,1,0,0,0,?8)",
                rusqlite::params![
                    occluder.id,
                    x,
                    y,
                    z,
                    hx,
                    hy,
                    hz,
                    occluder.explicit.then_some(occluder.owner)
                ],
            )
            .unwrap();
            if occluder.explicit {
                db.execute(
                    "INSERT INTO room_reference_rules VALUES(?1,7,'target',?2)",
                    rusqlite::params![occluder.id, occluder.owner],
                )
                .unwrap();
            }
        }
        let graph = load_room_graph(&db, 7).unwrap();
        assert!(graph.authored.as_ref().unwrap().complete);
        let room = |id| graph.rooms.iter().position(|r| r.ref_id == id).unwrap();
        if let Some(occluder) = case.occluder {
            assert_eq!(graph.occluders.len(), 1);
            assert_eq!(
                graph.occluders[&PortalSide::Room(room(occluder.owner))].len(),
                1
            );
        }
        let eye = runtime(case.eye);
        let lookup = runtime(case.near_shifted);
        assert_eq!(
            graph.rooms_containing(lookup),
            [room(case.start)],
            "{}",
            case.name
        );
        let [_, right, top, _, near, _] = case.frustum;
        let clip = Mat4::perspective_infinite_reverse_rh(2. * top.atan(), right / top, near)
            * Mat4::from_translation(-eye);
        let visible = graph.visibility_volumes(eye, lookup, clip).unwrap();
        let owner = case.destination.map_or(assignment::Owner::Unbounded, |id| {
            assignment::Owner::Room(room(id))
        });
        let actual = fixture.markers.map(|position| {
            let bounds =
                MeshBounds::transformed(&model, Affine3A::from_translation(runtime(position)))
                    .unwrap();
            assignment::draws(
                std::slice::from_ref(&owner),
                &graph.portals,
                &visible,
                Some(&bounds),
            )
        });
        assert_eq!(actual, case.visible, "{}", case.name);
    }
}

#[derive(Deserialize)]
struct LinkedPlaneCase {
    name: String,
    planes: Vec<LinkedPlane>,
    eye: [f32; 3],
    frustum: [f32; 6],
    visible: [bool; 3],
}

#[derive(Deserialize)]
struct LinkedPlane {
    id: u32,
    center: [f32; 3],
    rotation: [f32; 3],
    flags: u32,
    half: [f32; 3],
    links: Option<[u32; 4]>,
}

#[test]
fn linked_planes_match_retail_marker_visibility() {
    #[derive(Deserialize)]
    struct Capture {
        mesh: Bounds,
        markers: [[f32; 3]; 3],
        cases: Vec<LinkedPlaneCase>,
    }

    let fixture: Capture =
        serde_json::from_str(include_str!("../../tests/fixtures/linked-occluders.json")).unwrap();
    let model = Aabb::from_min_max(fixture.mesh.min.into(), fixture.mesh.max.into());
    assert_eq!(fixture.cases.len(), 17);
    for case in fixture.cases {
        let db = room_database(&[NativeRoom {
            role: "D".into(),
            form: 1,
            center: [0., 450., 0.],
            half: [700., 650., 350.],
        }]);
        super::tests::add_occluder_tables(&db);
        db.execute_batch(
            "ALTER TABLE room_occluder_links ADD COLUMN single_target_verified INTEGER DEFAULT 0",
        )
        .unwrap();
        for plane in case.planes {
            let [x, y, z] = plane.center;
            let [hx, hy, hz] = plane.half;
            let [rx, ry, rz] = plane.rotation;
            db.execute(
                "INSERT INTO room_occluders VALUES(
                    ?1,7,3,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,?11,0,?12,NULL
                )",
                rusqlite::params![
                    plane.id,
                    x,
                    y,
                    z,
                    rx,
                    ry,
                    rz,
                    hx,
                    hy,
                    hz,
                    plane.flags,
                    plane.links.is_some()
                ],
            )
            .unwrap();
            if let Some(links) = plane.links {
                for (slot, other) in links
                    .into_iter()
                    .enumerate()
                    .filter(|(_, other)| *other != 0)
                {
                    db.execute(
                        "INSERT INTO room_occluder_links VALUES(?1,?2,?3,1)",
                        rusqlite::params![plane.id, slot, other],
                    )
                    .unwrap();
                }
            }
        }
        let graph = load_room_graph(&db, 7).unwrap();
        let eye = runtime(case.eye);
        let [_, right, top, _, near, _] = case.frustum;
        let clip = Mat4::perspective_infinite_reverse_rh(2. * top.atan(), right / top, near)
            * Mat4::from_translation(-eye);
        let visibility = graph
            .visibility_volumes(eye, eye + Vec3::NEG_Z * near, clip)
            .unwrap();
        let actual = fixture.markers.map(|point| {
            let bounds =
                MeshBounds::transformed(&model, Affine3A::from_translation(runtime(point)))
                    .unwrap();
            assignment::draws(
                &[assignment::Owner::Room(0)],
                &graph.portals,
                &visibility,
                Some(&bounds),
            )
        });
        assert_eq!(actual, case.visible, "{}", case.name);
    }
}

#[derive(Deserialize)]
struct PortalOrderCase {
    name: String,
    cell: u32,
    rooms: Vec<NativeRoom>,
    /// Physical portal references in the observed registration sequence.
    portals: Vec<OverlapPortal>,
    eye: [f32; 3],
    lookup: [f32; 3],
    frustum: [f32; 6],
    start: u32,
    markers: Vec<RoomMarker>,
    visible: Vec<bool>,
}

#[test]
fn portal_incidence_order_matches_retail_renders() {
    #[derive(Deserialize)]
    struct Fixture {
        mesh: Bounds,
        cases: Vec<PortalOrderCase>,
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/room-portal-order.json");
    let fixture: Fixture = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let model = Aabb::from_min_max(fixture.mesh.min.into(), fixture.mesh.max.into());
    assert_eq!(fixture.cases.len(), 6);
    for case in fixture.cases {
        let db = room_database(&case.rooms);
        db.execute_batch("ALTER TABLE room_portals ADD COLUMN retail_visit_index INTEGER")
            .unwrap();
        for (rank, p) in case.portals.iter().enumerate() {
            let [x, y, z] = p.center;
            let [w, h] = p.half;
            let [a, b] = p.rooms;
            db.execute(
                "INSERT INTO room_portals(ref_id,cell_id,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z,
                 half_x,half_z,room_a,room_b,retail_visit_index) VALUES(?1,7,?2,?3,?4,0,0,0,?5,?6,?7,?8,?9)",
                rusqlite::params![p.id,x,y,z,w,h,a,b,rank],
            ).unwrap();
        }
        check_portal_order_meshes(&case, &load_room_graph(&db, 7).unwrap(), &model);
        if let Some(root) = std::env::var_os("MUDCRAB_PORTAL_ORDER_DATABASES") {
            let path = std::path::PathBuf::from(root)
                .join(&case.name)
                .with_extension("db");
            let converted =
                Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap();
            check_portal_order_meshes(
                &case,
                &load_room_graph(&converted, case.cell).unwrap(),
                &model,
            );
        }
    }
}

fn check_portal_order_meshes(case: &PortalOrderCase, graph: &RoomGraph, model: &Aabb) {
    let room = |id| graph.rooms.iter().position(|r| r.ref_id == id).unwrap();
    let eye = runtime(case.eye);
    let lookup = runtime(case.lookup);
    assert_eq!(graph.rooms_containing(lookup), [room(case.start)]);
    let [_, right, top, _, near, _] = case.frustum;
    let clip = Mat4::perspective_infinite_reverse_rh(2. * top.atan(), right / top, near)
        * Mat4::from_translation(-eye);
    let visibility = graph.visibility_volumes(eye, lookup, clip).unwrap();
    let actual: Vec<_> = case
        .markers
        .iter()
        .map(|marker| {
            let bounds = MeshBounds::transformed(
                model,
                Affine3A::from_translation(runtime(marker.position)),
            )
            .unwrap();
            assignment::draws(
                &[assignment::Owner::Room(room(marker.owner))],
                &graph.portals,
                &visibility,
                Some(&bounds),
            )
        })
        .collect();
    assert_eq!(actual, case.visible, "{}", case.name);
}
