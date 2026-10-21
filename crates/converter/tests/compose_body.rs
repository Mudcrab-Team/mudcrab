//! Composing body, hands and feet NIFs into one skinned model with one skeleton.

use converter::compose::{
    BodyPart, BodyPartKind, compose_body, composed_body_path, write_composed_body,
};
use dummy_content::nif::{
    BodyShape, BoxBody, LegacySkin, SkinInstanceKind, StaticShape, legacy_nested_skinned_shape,
    legacy_skinned_shape, static_shape,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const BONE_TRANSFORMS: [([f32; 3], [f32; 4], f32); 2] = [
    (
        [0.0, 0.0, 0.0],
        [
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ],
        1.0,
    ),
    ([0.0, 0.0, 2.0], [0.0, 0.0, 0.0, 1.0], 1.0),
];

const WEIGHTS: [[f32; 4]; 4] = [
    [0.7, 0.3, 0.0, 0.0],
    [0.25, 0.75, 0.0, 0.0],
    [0.5, 0.2, 0.0, 0.0],
    [0.4, 0.6, 0.0, 0.0],
];

const SHUFFLED_PALETTE: [u16; 2] = [1, 0];

fn legacy_spec<'a>(name: &'a str) -> LegacySkin<'a> {
    LegacySkin {
        name,
        instance_kind: SkinInstanceKind::Dismember,
        bone_transforms: BONE_TRANSFORMS,
        palette: &SHUFFLED_PALETTE,
        weights: &WEIGHTS,
        with_geometry: true,
        joint_override: None,
    }
}

fn write_part(directory: &Path, file_name: &str, spec: &LegacySkin<'_>) -> PathBuf {
    let path = directory.join(file_name);
    fs::write(&path, legacy_skinned_shape(spec).unwrap()).unwrap();
    path
}

fn write_nested_part(directory: &Path, file_name: &str, spec: &LegacySkin<'_>) -> PathBuf {
    let path = directory.join(file_name);
    fs::write(&path, legacy_nested_skinned_shape(spec).unwrap()).unwrap();
    path
}

fn skeleton_shape(name: &str) -> StaticShape<'_> {
    StaticShape {
        name,
        positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: &[[0.0, 0.0, 1.0]; 3],
        uvs: &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        indices: &[[0, 1, 2]],
        diffuse: "textures/generated_color.dds",
        normal_texture: "textures/generated_normal.dds",
    }
}

fn skeleton_body(node_name: &str) -> BoxBody<'_> {
    BoxBody {
        node_name,
        half_extents: [0.1, 0.1, 0.1],
        transform: None,
        collision_layer: 4,
        collision_flags: 0,
        collision_response: 1,
        inner_collision_flags: 0,
        inner_collision_response: 1,
        shape: BodyShape::Box,
        motion_system: 4,
        deactivator_type: 1,
        quality_type: 4,
        mass: 1.0,
        inertia: [0.1, 0.0, 0.0, 0.0, 0.1, 0.0, 0.0, 0.0, 0.1],
        center_of_mass: [0.0; 3],
        linear_damping: 0.1,
        angular_damping: 0.05,
        friction: 0.5,
        restitution: 0.4,
        max_linear_velocity: 104.4,
        max_angular_velocity: 31.57,
    }
}

/// A skeleton with `root` plus one `NiNode` per entry of `nodes`. The converter
/// only sees the `NiNode` blocks, so this is enough to share one bone tree.
fn write_node_skeleton(directory: &Path, root: &str, nodes: &[&str]) -> PathBuf {
    let bodies = nodes
        .iter()
        .map(|name| skeleton_body(name))
        .collect::<Vec<_>>();
    let mut bytes =
        dummy_content::nif::static_shape_with_bodies(&skeleton_shape(root), &bodies).unwrap();
    let header = parse_header(&bytes).expect("fixture header must parse");
    bytes[header.block_start(0)..header.block_start(0) + 4].copy_from_slice(&0u32.to_le_bytes());
    let path = directory.join("skeleton.nif");
    fs::write(&path, bytes).unwrap();
    path
}

struct HeaderLayout {
    block_sizes: Vec<u32>,
    block_data_start: usize,
}

impl HeaderLayout {
    fn block_start(&self, index: usize) -> usize {
        self.block_data_start
            + self.block_sizes[..index]
                .iter()
                .map(|size| *size as usize)
                .sum::<usize>()
    }
}

fn parse_header(bytes: &[u8]) -> Option<HeaderLayout> {
    fn read_u32(bytes: &[u8], cursor: &mut usize) -> Option<u32> {
        let end = cursor.checked_add(4)?;
        let value = u32::from_le_bytes(bytes.get(*cursor..end)?.try_into().ok()?);
        *cursor = end;
        Some(value)
    }
    fn read_u16(bytes: &[u8], cursor: &mut usize) -> Option<u16> {
        let end = cursor.checked_add(2)?;
        let value = u16::from_le_bytes(bytes.get(*cursor..end)?.try_into().ok()?);
        *cursor = end;
        Some(value)
    }
    fn skip_string8(bytes: &[u8], cursor: &mut usize) -> Option<()> {
        let length = usize::from(*bytes.get(*cursor)?);
        *cursor = cursor.checked_add(1 + length)?;
        bytes.get(..*cursor)?;
        Some(())
    }
    fn read_string32(bytes: &[u8], cursor: &mut usize) -> Option<String> {
        let length = read_u32(bytes, cursor)? as usize;
        let end = cursor.checked_add(length)?;
        let value = String::from_utf8_lossy(bytes.get(*cursor..end)?).into_owned();
        *cursor = end;
        Some(value)
    }

    let newline = bytes.iter().position(|byte| *byte == b'\n')?;
    let mut cursor = newline + 1;
    read_u32(bytes, &mut cursor)?;
    cursor += 1;
    read_u32(bytes, &mut cursor)?;
    let block_count = read_u32(bytes, &mut cursor)?;
    let bethesda_version = read_u32(bytes, &mut cursor)?;
    if bethesda_version > 130 {
        read_u32(bytes, &mut cursor)?;
    }
    skip_string8(bytes, &mut cursor)?;
    if bethesda_version < 131 {
        skip_string8(bytes, &mut cursor)?;
    }
    skip_string8(bytes, &mut cursor)?;
    if bethesda_version >= 103 {
        skip_string8(bytes, &mut cursor)?;
    }
    let type_count = read_u16(bytes, &mut cursor)?;
    for _ in 0..type_count {
        read_string32(bytes, &mut cursor)?;
    }
    for _ in 0..block_count {
        read_u16(bytes, &mut cursor)?;
    }
    let mut block_sizes = Vec::with_capacity(block_count as usize);
    for _ in 0..block_count {
        block_sizes.push(read_u32(bytes, &mut cursor)?);
    }
    let string_count = read_u32(bytes, &mut cursor)?;
    read_u32(bytes, &mut cursor)?;
    for _ in 0..string_count {
        read_string32(bytes, &mut cursor)?;
    }
    let group_count = read_u32(bytes, &mut cursor)?;
    for _ in 0..group_count {
        read_u32(bytes, &mut cursor)?;
    }
    Some(HeaderLayout {
        block_sizes,
        block_data_start: cursor,
    })
}

struct Glb {
    json: Value,
    bin: Vec<u8>,
}

fn read_glb(path: &Path) -> Glb {
    let bytes = fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"glTF", "not a GLB: {}", path.display());
    let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    assert_eq!(&bytes[16..20], b"JSON");
    let json = serde_json::from_slice(&bytes[20..20 + json_length]).unwrap();
    let mut cursor = 20 + json_length;
    let bin_length = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
    assert_eq!(&bytes[cursor + 4..cursor + 8], b"BIN\0");
    cursor += 8;
    Glb {
        json,
        bin: bytes[cursor..cursor + bin_length].to_vec(),
    }
}

fn gltf_nodes(glb: &Glb) -> &[Value] {
    glb.json["nodes"].as_array().unwrap()
}

fn skinned_meshes(glb: &Glb) -> Vec<(usize, usize)> {
    glb.json["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|node| {
            let skin = node["skin"].as_u64()? as usize;
            let mesh = node["mesh"].as_u64()? as usize;
            Some((mesh, skin))
        })
        .collect()
}

/// Parent of every node, plus every node reachable from the scene roots.
fn scene_graph(glb: &Glb) -> (BTreeMap<usize, usize>, BTreeSet<usize>) {
    let nodes = gltf_nodes(glb);
    let mut parents = BTreeMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if let Some(children) = node["children"].as_array() {
            for child in children {
                let child = child.as_u64().unwrap() as usize;
                assert!(
                    parents.insert(child, index).is_none(),
                    "node {child} has more than one parent"
                );
            }
        }
    }

    let mut reachable = BTreeSet::new();
    let mut stack: Vec<usize> = glb.json["scenes"][0]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|node| node.as_u64().unwrap() as usize)
        .collect();
    while let Some(index) = stack.pop() {
        if reachable.insert(index)
            && let Some(children) = nodes[index]["children"].as_array()
        {
            stack.extend(
                children
                    .iter()
                    .map(|child| child.as_u64().unwrap() as usize),
            );
        }
    }
    (parents, reachable)
}

fn accessor_count(glb: &Glb, index: u64) -> usize {
    glb.json["accessors"][index as usize]["count"]
        .as_u64()
        .unwrap() as usize
}

fn accessor_u8x4(glb: &Glb, index: u64) -> Vec<[u8; 4]> {
    let accessor = &glb.json["accessors"][index as usize];
    assert_eq!(accessor["componentType"], 5121, "JOINTS_0 must be U8");
    assert_eq!(accessor["type"], "VEC4");
    let view = &glb.json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
    let count = accessor["count"].as_u64().unwrap() as usize;
    glb.bin[offset..offset + count * 4]
        .as_chunks::<4>()
        .0
        .to_vec()
}

fn skeleton_node_names(path: &Path) -> BTreeSet<String> {
    use project_wormhole_nif::{nif_block::NiObjectNETTraits, nif_file::NifFile};

    let nif = NifFile::open(path.to_str().unwrap()).unwrap();
    nif.get_nodes()
        .iter()
        .filter_map(|node| nif.header.get_string(node.name() as usize).ok())
        .map(str::to_owned)
        .collect()
}

fn actor_root() -> Option<PathBuf> {
    std::env::var_os("MUDCRAB_ACTOR_NIF_DIR").map(PathBuf::from)
}

fn assert_bone_nodes_once(glb: &Glb, bone_names: &BTreeSet<String>) {
    let nodes = gltf_nodes(glb);
    let (_, reachable) = scene_graph(glb);
    for name in bone_names {
        assert!(
            !name.contains(':'),
            "bone {name} must not carry a mesh-index suffix"
        );
        let matching: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node["name"].as_str() == Some(name.as_str()))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "bone {name} must appear exactly once across nodes"
        );
        assert!(
            reachable.contains(&matching[0]),
            "bone {name} node {} is unreachable from scenes[0].nodes",
            matching[0]
        );
    }
}

fn assert_published_materials(glb: &Glb, expected_shapes: usize) {
    let materials = glb.json["materials"]
        .as_array()
        .unwrap_or_else(|| panic!("GLB has no published materials"));
    assert_eq!(
        materials.len(),
        expected_shapes,
        "every exported shape must get its own published material"
    );
    for (mesh_index, mesh) in glb.json["meshes"].as_array().unwrap().iter().enumerate() {
        assert!(
            mesh["primitives"][0]["material"].is_number(),
            "mesh {mesh_index} has no published material"
        );
    }
    let extensions = glb.json["extensionsUsed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        !extensions.contains(&"KHR_materials_pbrSpecularGlossiness"),
        "published materials must not rely on the unsupported spec-gloss extension"
    );
}

fn assert_skinned_glb(glb: &Glb, expected_skeletal_meshes: usize) {
    let skins = glb.json["skins"]
        .as_array()
        .unwrap_or_else(|| panic!("GLB has no skins"));
    assert_eq!(skins.len(), expected_skeletal_meshes);
    let (_, reachable) = scene_graph(glb);
    assert_eq!(
        reachable.len(),
        gltf_nodes(glb).len(),
        "every node must be reachable from scenes[0].nodes"
    );

    for (node_index, node) in gltf_nodes(glb).iter().enumerate() {
        if node["mesh"].is_number() && node["skin"].is_number() {
            assert!(
                reachable.contains(&node_index),
                "skinned mesh node {node_index} is unreachable from the scene"
            );
        }
    }

    for (skin_index, skin) in skins.iter().enumerate() {
        let joints = skin["joints"].as_array().unwrap();
        let inverse_binds = skin["inverseBindMatrices"].as_u64().unwrap();
        assert_eq!(
            joints.len(),
            accessor_count(glb, inverse_binds),
            "skin {skin_index} joint and inverse-bind counts differ"
        );
        for joint in joints {
            let joint = joint.as_u64().unwrap() as usize;
            assert!(
                reachable.contains(&joint),
                "skin {skin_index} joint node {joint} is unreachable from the scene"
            );
        }
    }

    for (mesh_index, skin_index) in skinned_meshes(glb) {
        let joint_count = glb.json["skins"][skin_index]["joints"]
            .as_array()
            .unwrap()
            .len();
        for (primitive_index, primitive) in glb.json["meshes"][mesh_index]["primitives"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let attributes = primitive["attributes"].as_object().unwrap();
            for semantic in ["POSITION", "JOINTS_0", "WEIGHTS_0"] {
                assert!(
                    attributes.contains_key(semantic),
                    "mesh {mesh_index} primitive {primitive_index} is missing {semantic}"
                );
            }
            let positions = attributes["POSITION"].as_u64().unwrap();
            let joint_accessor = attributes["JOINTS_0"].as_u64().unwrap();
            let max = accessor_u8x4(glb, joint_accessor)
                .iter()
                .flatten()
                .copied()
                .max()
                .unwrap_or(0);
            assert!(
                usize::from(max) < joint_count,
                "mesh {mesh_index} primitive {primitive_index} joint {max} >= {joint_count}"
            );
            for semantic in ["JOINTS_0", "WEIGHTS_0"] {
                assert_eq!(
                    accessor_count(glb, attributes[semantic].as_u64().unwrap()),
                    accessor_count(glb, positions),
                    "mesh {mesh_index} primitive {primitive_index} {semantic} count differs from POSITION"
                );
            }
        }
    }
}

#[test]
fn composed_body_path_is_vfs_relative() {
    assert_eq!(
        composed_body_path("default", "male"),
        PathBuf::from("meshes/actors/character/composed/default/male/body.glb")
    );
}

#[test]
fn compose_merges_two_parts_into_one_shared_skeleton_model() {
    let directory = tempfile::tempdir().unwrap();
    let skeleton_path = write_node_skeleton(directory.path(), "SkeletonRoot", &["Bone0", "Bone1"]);
    let body_path = write_part(directory.path(), "body.nif", &legacy_spec("BodyShape"));
    let hands_path = write_part(directory.path(), "hands.nif", &legacy_spec("HandsShape"));
    let body_part = BodyPart {
        path: &body_path,
        kind: BodyPartKind::Body,
    };
    let hands_part = BodyPart {
        path: &hands_path,
        kind: BodyPartKind::Hands,
    };

    let body_only = compose_body(std::slice::from_ref(&body_part), &skeleton_path).unwrap();
    let hands_only = compose_body(std::slice::from_ref(&hands_part), &skeleton_path).unwrap();
    let model = compose_body(&[body_part, hands_part], &skeleton_path).unwrap();

    assert_eq!(
        model.skeletal_meshes.len(),
        body_only.skeletal_meshes.len() + hands_only.skeletal_meshes.len()
    );
    assert_eq!(
        model.materials.len(),
        body_only.materials.len() + hands_only.materials.len()
    );
    assert!(!model.materials.is_empty());
    assert_eq!(
        model.material_indices.len(),
        model.skeletal_meshes.len(),
        "each composed skeletal mesh must keep its material index"
    );
    assert!(
        model
            .material_indices
            .iter()
            .all(|index| usize::from(*index) < model.materials.len()),
        "material index out of range: {:?} with {} materials",
        model.material_indices,
        model.materials.len()
    );
    let bones = &model.skeletal_meshes[0].bones;
    assert!(!bones.bones.is_empty());
    assert!(
        model
            .skeletal_meshes
            .iter()
            .all(|mesh| &mesh.bones == bones),
        "composed parts must share one bone tree"
    );

    let glb_path = directory.path().join("body.glb");
    write_composed_body(&model, &glb_path).unwrap();
    let glb = read_glb(&glb_path);
    let bone_names: BTreeSet<String> = bones.bones.keys().cloned().collect();
    assert_bone_nodes_once(&glb, &bone_names);
    assert_skinned_glb(&glb, model.skeletal_meshes.len());
    assert_published_materials(&glb, model.skeletal_meshes.len());
}

#[test]
fn compose_rejects_parts_with_different_bone_trees() {
    let directory = tempfile::tempdir().unwrap();
    // Covering only Bone0 makes each part bind its missing joints from its own
    // hierarchy; the nested part therefore gains an extra intermediate bone.
    let skeleton_path = write_node_skeleton(directory.path(), "Bone0", &[]);
    let body_path = write_part(directory.path(), "body.nif", &legacy_spec("BodyShape"));
    let hands_path = write_nested_part(directory.path(), "hands.nif", &legacy_spec("HandsShape"));

    let error = compose_body(
        &[
            BodyPart {
                path: &body_path,
                kind: BodyPartKind::Body,
            },
            BodyPart {
                path: &hands_path,
                kind: BodyPartKind::Hands,
            },
        ],
        &skeleton_path,
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("body part") && message.contains(body_path.to_str().unwrap()),
        "error does not name the body part: {message}"
    );
    assert!(
        message.contains("hands part") && message.contains(hands_path.to_str().unwrap()),
        "error does not name the hands part: {message}"
    );
    assert!(
        message.contains("bone tree"),
        "error does not report the bone-tree mismatch: {message}"
    );
}

#[test]
fn compose_rejects_a_part_that_is_not_skinned() {
    let directory = tempfile::tempdir().unwrap();
    let skeleton_path = write_node_skeleton(directory.path(), "SkeletonRoot", &["Bone0", "Bone1"]);
    let static_path = directory.path().join("static.nif");
    fs::write(
        &static_path,
        static_shape(&skeleton_shape("StaticShape")).unwrap(),
    )
    .unwrap();

    let error = compose_body(
        &[BodyPart {
            path: &static_path,
            kind: BodyPartKind::Feet,
        }],
        &skeleton_path,
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("feet part") && message.contains(static_path.to_str().unwrap()),
        "error does not name the feet part: {message}"
    );
    assert!(
        message.contains("no skeletal mesh"),
        "error does not say the part produced no skeletal mesh: {message}"
    );
}

#[test]
#[ignore = "requires MUDCRAB_ACTOR_NIF_DIR pointing at a Skyrim meshes/actors tree"]
fn retail_composed_bodies_share_one_skeleton() {
    let Some(root) = actor_root() else {
        return;
    };
    let character_assets = root.join("character/character assets");
    let skeleton_path = character_assets.join("skeleton.nif");
    assert!(
        skeleton_path.is_file(),
        "missing {}",
        skeleton_path.display()
    );
    let expected_bone_names = skeleton_node_names(&skeleton_path);
    assert_eq!(
        expected_bone_names.len(),
        99,
        "skeleton.nif must expose 99 unique node names"
    );
    for required in ["NPC Head [Head]", "NPC L Foot [Lft ]", "NPC L Toe0 [LToe]"] {
        assert!(
            expected_bone_names.contains(required),
            "skeleton is missing {required}"
        );
    }

    for (sex, prefix) in [("male", "male"), ("female", "female")] {
        let paths = [
            character_assets.join(format!("{prefix}body_1.nif")),
            character_assets.join(format!("{prefix}hands_1.nif")),
            character_assets.join(format!("{prefix}feet_1.nif")),
        ];
        for path in &paths {
            assert!(path.is_file(), "missing {}", path.display());
        }
        let parts = [
            BodyPart {
                path: &paths[0],
                kind: BodyPartKind::Body,
            },
            BodyPart {
                path: &paths[1],
                kind: BodyPartKind::Hands,
            },
            BodyPart {
                path: &paths[2],
                kind: BodyPartKind::Feet,
            },
        ];
        let model = compose_body(&parts, &skeleton_path).unwrap();
        assert_eq!(
            model.skeletal_meshes.len(),
            4,
            "{sex} body must compose four skeletal meshes (body 2 + hands 1 + feet 1)"
        );
        let bone_names: BTreeSet<String> = model.skeletal_meshes[0]
            .bones
            .bones
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            bone_names, expected_bone_names,
            "{sex} composed bone names must equal skeleton.nif's unique node names"
        );
        assert_eq!(bone_names.len(), 99);

        let directory = tempfile::tempdir().unwrap();
        let glb_path = directory.path().join(format!("{sex}.glb"));
        write_composed_body(&model, &glb_path).unwrap();
        let glb = read_glb(&glb_path);
        assert_bone_nodes_once(&glb, &bone_names);
        assert_skinned_glb(&glb, model.skeletal_meshes.len());

        let (parents, reachable) = scene_graph(&glb);
        assert_eq!(
            reachable.len(),
            gltf_nodes(&glb).len(),
            "{sex} GLB has unreachable nodes"
        );
        for (mesh_index, skin_index) in skinned_meshes(&glb) {
            let joints = glb.json["skins"][skin_index]["joints"]
                .as_array()
                .unwrap()
                .len();
            let vertices = accessor_count(
                &glb,
                glb.json["meshes"][mesh_index]["primitives"][0]["attributes"]["POSITION"]
                    .as_u64()
                    .unwrap(),
            );
            println!(
                "sex={sex} mesh={mesh_index} skin={skin_index} joints={joints} vertices={vertices}"
            );
        }
        println!(
            "sex={sex} skeletal_meshes={} skins={} materials={} nodes={} reachable={} parents={} bone_nodes={}",
            model.skeletal_meshes.len(),
            glb.json["skins"].as_array().unwrap().len(),
            model.materials.len(),
            gltf_nodes(&glb).len(),
            reachable.len(),
            parents.len(),
            bone_names.len(),
        );

        if sex == "male" {
            let second = compose_body(&parts, &skeleton_path).unwrap();
            let second_path = directory.path().join("male-second.glb");
            write_composed_body(&second, &second_path).unwrap();
            assert_eq!(
                fs::read(&glb_path).unwrap(),
                fs::read(&second_path).unwrap(),
                "composing the same parts twice must produce identical GLB bytes"
            );
        }
    }
}

#[test]
#[ignore = "requires MUDCRAB_ACTOR_NIF_DIR pointing at a Skyrim meshes/actors tree"]
fn retail_composed_body_is_deterministic() {
    let Some(root) = actor_root() else {
        return;
    };
    let character_assets = root.join("character/character assets");
    let skeleton_path = character_assets.join("skeleton.nif");
    let paths = [
        character_assets.join("malebody_1.nif"),
        character_assets.join("malehands_1.nif"),
        character_assets.join("malefeet_1.nif"),
    ];
    let parts = [
        BodyPart {
            path: &paths[0],
            kind: BodyPartKind::Body,
        },
        BodyPart {
            path: &paths[1],
            kind: BodyPartKind::Hands,
        },
        BodyPart {
            path: &paths[2],
            kind: BodyPartKind::Feet,
        },
    ];
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.glb");
    let second_path = directory.path().join("second.glb");
    let first = compose_body(&parts, &skeleton_path).unwrap();
    write_composed_body(&first, &first_path).unwrap();
    let second = compose_body(&parts, &skeleton_path).unwrap();
    write_composed_body(&second, &second_path).unwrap();
    assert_eq!(
        fs::read(&first_path).unwrap(),
        fs::read(&second_path).unwrap(),
        "composing the same parts twice must produce identical GLB bytes"
    );
}
