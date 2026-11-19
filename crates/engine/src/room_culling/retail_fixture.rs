use super::*;
use bevy::math::Affine3A;
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, path::Path};

#[derive(Deserialize)]
struct Input {
    binding: Binding,
    camera: CameraCapture,
}

#[derive(Deserialize)]
struct Binding {
    schema: u32,
    case: String,
    editor_id: String,
    source_database: PathBuf,
    live_forms: BTreeMap<String, u32>,
    probes: BTreeMap<String, BTreeMap<String, [f32; 4]>>,
}

#[derive(Deserialize)]
struct CameraCapture {
    cell_id: u32,
    position: [f32; 3],
    rotation_matrix: [f32; 9],
    forward: [f32; 3],
    frustum: [f32; 6],
    near_shifted: [f32; 3],
    orthographic: bool,
}

fn runtime(vector: [f32; 3]) -> Vec3 {
    assert!(vector.iter().all(|component| component.is_finite()));
    Vec3::from_array(shared::coordinates::creation_to_runtime_vector(vector))
}

#[test]
#[ignore = "requires a retail binding, captured camera and fresh output path"]
fn retail_portal_fixture_comparison() {
    let input_path = std::env::var_os("MUDCRAB_RETAIL_FIXTURE_INPUT").unwrap();
    let input: Input = serde_json::from_slice(&std::fs::read(&input_path).unwrap()).unwrap();
    let Input { binding, camera } = input;
    assert_eq!(binding.schema, 1);
    let cell = binding.live_forms["cell"];
    assert_eq!(camera.cell_id, cell, "captured camera cell");
    let source_database = binding.source_database;
    assert!(source_database.is_absolute() && Path::is_file(&source_database));
    let connection =
        Connection::open_with_flags(&source_database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open converted source database");
    let graph = load_room_graph(&connection, cell).expect("load full cell room graph");
    assert!(!graph.incomplete);
    assert!(
        graph
            .authored
            .as_ref()
            .is_some_and(|authored| authored.complete)
    );

    let eye = runtime(camera.position);
    let near_shifted = runtime(camera.near_shifted);
    let forward = runtime(camera.forward);
    let [m00, m01, m02, m10, m11, m12, m20, m21, m22] = camera.rotation_matrix;
    let matrix_forward = runtime([m00, m10, m20]);
    let up = runtime([m01, m11, m21]);
    let right = runtime([m02, m12, m22]);
    let tolerance = 0.0001;
    for axis in [matrix_forward, up, right] {
        assert!((axis.length_squared() - 1.0).abs() < tolerance);
    }
    assert!(matrix_forward.dot(up).abs() < tolerance);
    assert!(matrix_forward.cross(up).distance(right) < tolerance);
    assert!(forward.distance(matrix_forward) < tolerance);

    assert!(!camera.orthographic);
    assert!(camera.frustum.iter().all(|component| component.is_finite()));
    let [left, right_slope, top, bottom, near, far] = camera.frustum;
    assert!((left + right_slope).abs() < tolerance && (top + bottom).abs() < tolerance);
    assert!(right_slope > 0.0 && top > 0.0 && near > 0.0 && far > near);
    let fov = 2.0 * top.atan();
    let aspect_ratio = right_slope / top;
    assert!(fov.is_finite() && aspect_ratio.is_finite() && aspect_ratio > 0.0);
    let projection = Projection::Perspective(PerspectiveProjection {
        fov,
        aspect_ratio,
        near,
        far,
        near_clip_plane: Vec4::new(0.0, 0.0, -1.0, -near),
    });
    let camera_transform = Transform::from_translation(eye).looking_to(forward, up);
    let camera_global = GlobalTransform::from(camera_transform);
    let view = camera_global.to_matrix().inverse();
    assert!(view.transform_vector3(forward).distance(Vec3::NEG_Z) < tolerance);
    assert!(view.transform_vector3(up).distance(Vec3::Y) < tolerance);
    assert!(view.transform_vector3(right).distance(Vec3::X) < tolerance);
    let ordinary_frustum = projection.compute_frustum(&camera_global);
    let clip = projection.get_clip_from_view() * view;
    assert!(clip.is_finite());

    let room_index = |role: &str| {
        let form = binding.live_forms[role];
        graph
            .rooms
            .iter()
            .position(|room| room.ref_id == form)
            .unwrap_or_else(|| panic!("missing {role} form {form:#010x}"))
    };
    let selected = room_index("room/A");
    let starts = graph.rooms_containing(eye);
    let room_point = RoomCullingNear::SKYRIM.world_point(&camera_global);
    assert!(room_point.distance(near_shifted) < 0.001);
    let near_starts = graph.rooms_containing(room_point);
    assert_eq!(starts, [selected], "unique eye seed must be room/A");
    assert_eq!(near_starts.len(), 1, "expected a unique lookup room");
    let visible = graph
        .visibility_volumes(eye, room_point, clip)
        .expect("room traversal");
    let mut rooms = BTreeMap::new();
    let mut probes = BTreeMap::new();
    for role in ["A", "B", "C", "D"] {
        let owner = room_index(&format!("room/{role}"));
        let volumes = &visible.rooms[owner];
        rooms.insert(
            role,
            json!({
                "ref_id": graph.rooms[owner].ref_id,
                "room_index": owner,
                "reached": volumes.is_reached(),
            }),
        );
        if !matches!(role, "C" | "D") {
            continue;
        }
        for label in ["short", "long"] {
            let sphere = binding.probes[role][label];
            let [x, y, z, radius] = sphere;
            let center = runtime([x, y, z]);
            assert!(radius.is_finite() && radius > 0.0);
            let translation = Affine3A::from_translation(center);
            let test_bounds = |half: Vec3| {
                let aabb = Aabb::from_min_max(-half, half);
                let bounds = MeshBounds::transformed(&aabb, translation).unwrap();
                (
                    volumes.intersects(Some(&bounds)),
                    ordinary_frustum.intersects_obb(&aabb, &translation, true, true),
                )
            };
            let (cube, ordinary_cube) = test_bounds(Vec3::splat(radius));
            let (point, ordinary_point) = test_bounds(Vec3::ZERO);
            probes.insert(
                format!("{role}/{label}"),
                json!({
                    "sphere_creation": sphere,
                    "center_runtime": center.to_array(),
                    "radius_runtime": radius,
                    "portal_aabb": cube,
                    "portal_point": point,
                    "portal_bounds_disagree": cube != point,
                    "ordinary_aabb": ordinary_cube,
                    "ordinary_point": ordinary_point,
                    "ordinary_bounds_disagree": ordinary_cube != ordinary_point,
                }),
            );
        }
    }

    let result = json!({
        "schema": 1,
        "case": binding.case,
        "editor_id": binding.editor_id,
        "input_path": PathBuf::from(input_path),
        "source_database": source_database,
        "test_executable": std::env::current_exe().unwrap(),
        "cell_ref_id": cell,
        "graph": {
            "rooms": graph.rooms.len(),
            "portals": graph.portals.len(),
            "occluders": graph.occluders.values().map(Vec::len).sum::<usize>(),
            "links": graph.links.len(),
            "overlaps": graph.overlaps.len(),
        },
        "seed_ref_ids": {
            "eye": graph.rooms[starts[0]].ref_id,
            "near_shifted": graph.rooms[near_starts[0]].ref_id,
            "selected_room_a": graph.rooms[selected].ref_id,
        },
        "camera": {
            "eye_creation": camera.position,
            "eye_runtime": eye.to_array(),
            "near_shifted_creation": camera.near_shifted,
            "near_shifted_runtime": near_shifted.to_array(),
            "rotation_matrix_creation": camera.rotation_matrix,
            "forward_creation": camera.forward,
            "forward_runtime": forward.to_array(),
            "up_runtime": up.to_array(),
            "right_runtime": right.to_array(),
            "frustum": camera.frustum,
            "vertical_fov_radians": fov,
            "aspect_ratio": aspect_ratio,
            "clip_from_runtime_columns": clip.to_cols_array(),
        },
        "rooms": rooms,
        "probes": probes,
    });
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(std::env::var_os("MUDCRAB_RETAIL_FIXTURE_OUTPUT").unwrap())
        .unwrap();
    serde_json::to_writer_pretty(&mut file, &result).unwrap();
}

#[derive(Deserialize)]
struct SeamPose {
    name: String,
    camera: CameraCapture,
    root: Option<u32>,
    rooms: Vec<u32>,
    unbounded: bool,
}

#[test]
#[ignore = "requires the converted retail database in MUDCRAB_SEAM_DATABASE"]
fn gallows_rock_seam_replay() {
    let database = std::env::var_os("MUDCRAB_SEAM_DATABASE").unwrap();
    let poses: Vec<SeamPose> =
        serde_json::from_str(include_str!("../../tests/fixtures/gallows-rock-seam.json")).unwrap();
    let db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let graph = load_room_graph(&db, 86643).unwrap();
    assert!(graph.supports_unbounded && graph.authored.as_ref().unwrap().complete);
    assert!(!poses.is_empty());
    let mut differences = Vec::new();
    for pose in poses {
        let c = pose.camera;
        assert_eq!(c.cell_id, 86643);
        let eye = runtime(c.position);
        let [_, ux, _, _, uy, _, _, uz, _] = c.rotation_matrix;
        let camera = GlobalTransform::from(
            Transform::from_translation(eye).looking_to(runtime(c.forward), runtime([ux, uy, uz])),
        );
        let point = RoomCullingNear::SKYRIM.world_point(&camera);
        assert!(point.distance(runtime(c.near_shifted)) < 0.002);
        let starts: Vec<_> = graph
            .rooms_containing(point)
            .into_iter()
            .map(|i| graph.rooms[i].ref_id)
            .collect();
        assert_eq!(
            starts,
            pose.root.into_iter().collect::<Vec<_>>(),
            "{}",
            pose.name
        );
        let [_, right, top, _, near, _] = c.frustum;
        for render_near in [near, PerspectiveProjection::default().near] {
            let clip =
                Mat4::perspective_infinite_reverse_rh(2.0 * top.atan(), right / top, render_near)
                    * camera.to_matrix().inverse();
            let actual = graph.visibility_volumes(eye, point, clip).map(|view| {
                let mut rooms: Vec<_> = graph
                    .rooms
                    .iter()
                    .zip(&view.rooms)
                    .filter(|(_, volume)| volume.is_reached())
                    .map(|(r, _)| r.ref_id)
                    .collect();
                rooms.sort_unstable();
                (rooms, view.unbounded.is_reached())
            });
            let expected = Some((pose.rooms.clone(), pose.unbounded));
            if actual != expected {
                differences.push((pose.name.clone(), render_near, actual, expected));
            }
        }
    }
    assert!(differences.is_empty(), "{differences:#?}");
}
