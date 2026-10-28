//! Whole-file placement regression: inspect render vertices and embedded physics after conversion.
use super::*;
use dummy_content::nif::{BodyShape, BoxBody, StaticShape, static_shape_with_bodies};
use project_wormhole_shared::glam::{Mat3, Mat4, Quat, Vec3};
use shared::collision::CollisionShape;

fn close(actual: Vec3, expected: Vec3) {
    assert!(
        actual.abs_diff_eq(expected, 2.0e-3),
        "{actual:?} != {expected:?}"
    );
}

/// Mutate only an NiAVObject's transform in an otherwise generated valid NIF.
fn set_transform(bytes: &mut [u8], block: usize, transform: Mat4) {
    let (data, header) = parse_skyrim_header(bytes, Path::new("fixture.nif")).unwrap();
    let start = bytes.len() - data.len()
        + header.block_size_index[..block]
            .iter()
            .map(|size| *size as usize)
            .sum::<usize>();
    let (scale, rotation, translation) = transform.to_scale_rotation_translation();
    assert!(scale.abs_diff_eq(Vec3::splat(scale.x), 1.0e-6));
    // NiObjectNET is 16 bytes. NiAVObject then writes translation, row-major rotation, scale.
    let rotation = Mat3::from_quat(rotation).transpose().to_cols_array();
    let values = translation
        .to_array()
        .into_iter()
        .chain(rotation)
        .chain([scale.x]);
    for (index, value) in values.enumerate() {
        bytes[start + 16 + index * 4..start + 20 + index * 4].copy_from_slice(&value.to_le_bytes());
    }
}

fn local_matrix(node: &serde_json::Value) -> Mat4 {
    if let Some(matrix) = node.get("matrix") {
        let values = std::array::from_fn(|i| matrix[i].as_f64().unwrap() as f32);
        return Mat4::from_cols_array(&values);
    }
    let vector = |field, default: [f32; 3]| {
        node.get(field).map_or(Vec3::from_array(default), |v| {
            Vec3::new(
                v[0].as_f64().unwrap() as f32,
                v[1].as_f64().unwrap() as f32,
                v[2].as_f64().unwrap() as f32,
            )
        })
    };
    let rotation = node.get("rotation").map_or(Quat::IDENTITY, |v| {
        Quat::from_array(std::array::from_fn(|i| v[i].as_f64().unwrap() as f32))
    });
    Mat4::from_scale_rotation_translation(
        vector("scale", [1.0; 3]),
        rotation,
        vector("translation", [0.0; 3]),
    )
}

fn render_vertices(glb: &[u8], document: &serde_json::Value) -> Vec<Vec3> {
    fn visit(glb: &[u8], doc: &serde_json::Value, index: usize, parent: Mat4, out: &mut Vec<Vec3>) {
        let node = &doc["nodes"][index];
        let world = parent * local_matrix(node);
        if let Some(mesh) = node["mesh"].as_u64() {
            let accessor = doc["meshes"][mesh as usize]["primitives"][0]["attributes"]["POSITION"]
                .as_u64()
                .unwrap();
            let accessor = &doc["accessors"][accessor as usize];
            assert_eq!(accessor["componentType"], 5126);
            assert_eq!(accessor["type"], "VEC3");
            let view = &doc["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
            let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
            let offset = 20
                + json_len
                + 8
                + view["byteOffset"].as_u64().unwrap_or(0) as usize
                + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
            let stride = view["byteStride"].as_u64().unwrap_or(12) as usize;
            for i in 0..accessor["count"].as_u64().unwrap() as usize {
                let position = Vec3::from_array(std::array::from_fn(|axis| {
                    let start = offset + i * stride + axis * 4;
                    f32::from_le_bytes(glb[start..start + 4].try_into().unwrap())
                }));
                out.push(world.transform_point3(position));
            }
        }
        if let Some(children) = node["children"].as_array() {
            for child in children {
                visit(glb, doc, child.as_u64().unwrap() as usize, world, out);
            }
        }
    }
    let mut points = Vec::new();
    for root in document["scenes"][0]["nodes"].as_array().unwrap() {
        visit(
            glb,
            document,
            root.as_u64().unwrap() as usize,
            Mat4::IDENTITY,
            &mut points,
        );
    }
    points
}

#[test]
fn conversion_places_render_collision_com_and_inertia_in_one_reference_frame() {
    let root = Mat4::from_scale_rotation_translation(
        Vec3::splat(2.5),
        Quat::from_rotation_x(0.7),
        Vec3::new(101.0, 203.0, 307.0),
    );
    let child = Mat4::from_scale_rotation_translation(
        Vec3::splat(1.5),
        Quat::from_rotation_z(0.4),
        Vec3::new(11.0, 13.0, 17.0),
    );
    let body_rotation = Quat::from_rotation_y(0.3);
    let body_frame = Mat4::from_rotation_translation(body_rotation, Vec3::new(70.0, 140.0, 210.0));
    let half = Vec3::new(7.0, 14.0, 21.0);
    let corners: Vec<Vec3> = [-1.0, 1.0]
        .into_iter()
        .flat_map(|x| {
            [-1.0, 1.0].into_iter().flat_map(move |y| {
                [-1.0, 1.0]
                    .into_iter()
                    .map(move |z| half * Vec3::new(x, y, z))
            })
        })
        .collect();
    let positions: Vec<[f32; 3]> = corners
        .iter()
        .map(|point| body_frame.transform_point3(*point).to_array())
        .collect();
    let body = BoxBody {
        node_name: "Body",
        half_extents: [0.1, 0.2, 0.3],
        transform: Some(([1.0, 2.0, 3.0], body_rotation.to_array())),
        collision_layer: 4,
        collision_flags: 0,
        collision_response: 1,
        inner_collision_flags: 0,
        inner_collision_response: 1,
        shape: BodyShape::Box,
        motion_system: 4,
        deactivator_type: 1,
        quality_type: 4,
        mass: 2.0,
        inertia: [0.1, 0.0, 0.0, 0.0, 0.2, 0.0, 0.0, 0.0, 0.3],
        center_of_mass: [0.2, -0.3, 0.4],
        linear_damping: 0.1,
        angular_damping: 0.05,
        friction: 0.5,
        restitution: 0.4,
        max_linear_velocity: 104.4,
        max_angular_velocity: 31.57,
    };
    let mut source = static_shape_with_bodies(
        &StaticShape {
            name: "OffsetBox",
            positions: &positions,
            normals: &[[0.0, 0.0, 1.0]; 8],
            uvs: &[[0.0, 0.0]; 8],
            indices: &[
                [0, 1, 3],
                [0, 3, 2],
                [4, 6, 7],
                [4, 7, 5],
                [0, 4, 5],
                [0, 5, 1],
                [2, 3, 7],
                [2, 7, 6],
                [0, 2, 6],
                [0, 6, 4],
                [1, 5, 7],
                [1, 7, 3],
            ],
            diffuse: "textures/fixture.dds",
            normal_texture: "textures/fixture_n.dds",
        },
        std::slice::from_ref(&body),
    )
    .unwrap();
    set_transform(&mut source, 0, root);
    set_transform(&mut source, 1, child);
    set_transform(&mut source, 4, child);
    let basis = Mat4::from_quat(Quat::from_array(
        shared::coordinates::CREATION_TO_RUNTIME_ROTATION,
    ));
    let reference = Mat4::from_scale_rotation_translation(
        Vec3::splat(1.2),
        Quat::from_array(shared::coordinates::creation_euler_to_runtime_quaternion([
            0.2, -0.1, 0.8,
        ])),
        Vec3::from_array(shared::coordinates::creation_to_runtime_vector([
            1000.0, 2000.0, 3000.0,
        ])),
    );
    let directory = tempfile::tempdir().unwrap();
    // Extension is the production policy boundary. LOD roots must survive, including case.
    for extension in ["nif", "btr", "BTO"] {
        let input = directory.path().join(format!("placement.{extension}"));
        let output = directory.path().join(format!("placement-{extension}.glb"));
        fs::write(&input, &source).unwrap();
        MeshConverter::convert_nif_to_glb_with_skeleton(&input, &output, None).unwrap();
        let bytes = fs::read(&output).unwrap();
        let document = glb_json_from_bytes(&bytes).unwrap();
        let rendered = render_vertices(&bytes, &document);
        let collision: CollisionAsset =
            serde_json::from_value(document["scenes"][0]["extras"]["mudcrabCollision"].clone())
                .unwrap();
        assert!(collision.skipped.is_empty(), "{:?}", collision.skipped);
        assert_eq!(collision.shapes.len(), 1);
        assert_eq!(collision.bodies.len(), 1);
        let CollisionShape::Hull { points } = &collision.shapes[0] else {
            panic!("expected box hull");
        };
        assert_eq!(points.len(), 8);
        assert_eq!(rendered.len(), 8);
        let retained_root = if extension == "nif" {
            Mat4::IDENTITY
        } else {
            root
        };
        let frame = retained_root * child * body_frame;
        for corner in &corners {
            let expected = (basis * frame).transform_point3(*corner);
            assert!(
                rendered
                    .iter()
                    .any(|point| point.abs_diff_eq(expected, 2.0e-3)),
                "{extension}: missing render corner {expected:?}"
            );
            assert!(
                points
                    .iter()
                    .any(|point| Vec3::from_array(*point).abs_diff_eq(expected, 2.0e-3)),
                "{extension}: missing collision corner {expected:?}"
            );
            let expected_world = (reference * basis * frame).transform_point3(*corner);
            let render_world = rendered
                .iter()
                .map(|point| reference.transform_point3(*point))
                .collect::<Vec<_>>();
            let collision_world = points
                .iter()
                .map(|point| reference.transform_point3(Vec3::from_array(*point)))
                .collect::<Vec<_>>();
            assert!(
                render_world
                    .iter()
                    .any(|point| point.abs_diff_eq(expected_world, 5.0e-3))
            );
            assert!(
                collision_world
                    .iter()
                    .any(|point| point.abs_diff_eq(expected_world, 5.0e-3))
            );
        }
        let dynamics = &collision.bodies[0];
        assert_eq!(dynamics.mass, 2.0);
        assert_eq!(dynamics.shapes, vec![0]);
        let com = (basis * frame).transform_point3(Vec3::from_array(body.center_of_mass) * 70.0);
        close(Vec3::from_array(dynamics.center_of_mass), com);
        close(
            reference.transform_point3(Vec3::from_array(dynamics.center_of_mass)),
            reference.transform_point3(com),
        );
        let (scale, rotation, _) = frame.to_scale_rotation_translation();
        let orientation = Mat3::from_mat4(basis) * Mat3::from_quat(rotation);
        let tensor = orientation
            * Mat3::from_diagonal(Vec3::new(0.1, 0.2, 0.3) * (70.0 * scale.x).powi(2))
            * orientation.transpose();
        for row in 0..3 {
            for column in 0..3 {
                let actual = dynamics.inertia[row * 3 + column];
                let expected = tensor.col(column)[row];
                assert!(
                    (actual - expected).abs() <= 2.0e-3 + expected.abs() * 1.0e-5,
                    "{extension}: tensor {row},{column}: {actual} != {expected}"
                );
            }
        }
        // Read-only extraction and re-annotation must select the same frame as conversion.
        assert_eq!(
            serde_json::to_value(MeshConverter::extract_collision(&input).unwrap()).unwrap(),
            serde_json::to_value(&collision).unwrap()
        );
        MeshConverter::annotate_glb_collision(&input, &output).unwrap();
        let annotated = fs::read(&output).unwrap();
        assert_eq!(
            render_vertices(&annotated, &glb_json_from_bytes(&annotated).unwrap()),
            rendered
        );
        // Compare the complete payloads using the CollisionAsset f32 contract.
        let annotated_collision: CollisionAsset = serde_json::from_value(
            glb_json_from_bytes(&annotated).unwrap()["scenes"][0]["extras"]["mudcrabCollision"]
                .clone(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(annotated_collision).unwrap(),
            serde_json::to_value(collision).unwrap()
        );
    }
}

#[test]
fn parsed_skin_keeps_its_bone_tree_under_a_nonidentity_mesh_root() {
    use dummy_content::nif::{LegacySkin, SkinInstanceKind, legacy_skinned_shape};
    let mut bytes = legacy_skinned_shape(&LegacySkin {
        name: "SkinPolicy",
        instance_kind: SkinInstanceKind::Plain,
        bone_transforms: [([0.0; 3], [0.0, 0.0, 0.0, 1.0], 1.0); 2],
        palette: &[1, 0],
        weights: &[[0.7, 0.3, 0.0, 0.0]; 4],
        with_geometry: true,
        joint_override: None,
    })
    .unwrap();
    set_transform(
        &mut bytes,
        0,
        Mat4::from_scale_rotation_translation(
            Vec3::splat(2.0),
            Quat::from_rotation_z(0.6),
            Vec3::new(3.0, 5.0, 7.0),
        ),
    );
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("skin.nif");
    fs::write(&input, bytes).unwrap();
    let (nif, _, _) = open_nif_resilient(&input).unwrap();
    assert!(nif.has_skeleton());
    let placement = crate::nif_placement::PlacementFrame::from_nif(&input, &nif);
    assert!(!placement.replaces_block(0));
    let mut model = nif_to_model(&nif, None).unwrap();
    assert!(!model.skeletal_meshes.is_empty());
    assert!(model.static_nodes.is_empty());
    let bone_trees: Vec<_> = model
        .skeletal_meshes
        .iter()
        .map(|mesh| mesh.bones.clone())
        .collect();
    placement.place_model(&mut model);
    assert_eq!(
        model
            .skeletal_meshes
            .iter()
            .map(|mesh| mesh.bones.clone())
            .collect::<Vec<_>>(),
        bone_trees
    );
    assert_eq!(
        model.scene_root_rotation,
        Some(shared::coordinates::CREATION_TO_RUNTIME_ROTATION)
    );
    let output = directory.path().join("skin.glb");
    MeshConverter::convert_nif_to_glb_with_skeleton(&input, &output, None).unwrap();
    let document = glb_json_from_bytes(&fs::read(output).unwrap()).unwrap();
    assert!(!document["skins"].as_array().unwrap().is_empty());
    assert!(
        document["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["skin"].is_u64())
    );
}
