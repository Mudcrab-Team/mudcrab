//! Legacy Skyrim skin export (#273 stage 1a), with per-shape static recovery
//! when geometry is usable but its skin cannot be bound safely.

use converter::mesh::{MeshConverter, SkeletonResolution, find_skeleton};
use dummy_content::nif::{
    BodyShape, BoxBody, DYNAMIC_POSITION_OFFSET, LegacySkin, SkinInstanceKind, SkinNormals,
    StaticShape, legacy_dynamic_skinned_shape, legacy_dynamic_skinned_shape_with_normals,
    legacy_lod_skinned_shape, legacy_nested_skinned_shape, legacy_skinned_shape,
    legacy_skinned_shape_with_normals, static_shape, static_shape_with_bodies,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, Once, OnceLock},
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

fn spec<'a>(
    instance_kind: SkinInstanceKind,
    palette: &'a [u16],
    weights: &'a [[f32; 4]],
    with_geometry: bool,
    joint_override: Option<[u8; 4]>,
) -> LegacySkin<'a> {
    LegacySkin {
        name: "GeneratedSkin",
        instance_kind,
        bone_transforms: BONE_TRANSFORMS,
        palette,
        weights,
        with_geometry,
        joint_override,
    }
}

fn write_fixture(directory: &Path, spec: &LegacySkin<'_>) -> PathBuf {
    let path = directory.join("skin.nif");
    fs::write(&path, legacy_skinned_shape(spec).unwrap()).unwrap();
    path
}

/// The fixture's BSFadeNode root uses `NULL_REF` for its name and lists the
/// shape as a child, either of which makes it unusable as a skeleton. Rewrite
/// only those two fields in a copy so the bone tree needs no new fixture format.
fn write_skeleton_fixture(directory: &Path, spec: &LegacySkin<'_>) -> PathBuf {
    let mut bytes = legacy_skinned_shape(spec).unwrap();
    let header = parse_header(&bytes).expect("fixture header must parse");
    let root = header.block_data_start;
    bytes[root..root + 4].copy_from_slice(&0u32.to_le_bytes());
    bytes[root + 72..root + 76].copy_from_slice(&2u32.to_le_bytes());
    let path = directory.join("skeleton.nif");
    fs::write(&path, bytes).unwrap();
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

/// A skeleton with a single `BSFadeNode` of `name`. Its shape child is a
/// `BSTriShape`, so it is also a non-`NiNode` child.
fn write_single_node_skeleton(directory: &Path, name: &str) -> PathBuf {
    let mut bytes = static_shape(&skeleton_shape(name)).unwrap();
    let header = parse_header(&bytes).expect("fixture header must parse");
    bytes[header.block_start(0)..header.block_start(0) + 4].copy_from_slice(&0u32.to_le_bytes());
    let path = directory.join("skeleton.nif");
    fs::write(&path, bytes).unwrap();
    path
}

/// A skeleton with `root` plus one `NiNode` per entry of `nodes`, so a fixture
/// can express duplicated leaf names.
fn write_node_skeleton(directory: &Path, root: &str, nodes: &[&str]) -> PathBuf {
    let bodies = nodes
        .iter()
        .map(|name| skeleton_body(name))
        .collect::<Vec<_>>();
    let mut bytes = static_shape_with_bodies(&skeleton_shape(root), &bodies).unwrap();
    let header = parse_header(&bytes).expect("fixture header must parse");
    bytes[header.block_start(0)..header.block_start(0) + 4].copy_from_slice(&0u32.to_le_bytes());
    let path = directory.join("skeleton.nif");
    fs::write(&path, bytes).unwrap();
    path
}

fn gltf_nodes(glb: &Glb) -> &[Value] {
    glb.json["nodes"].as_array().unwrap()
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

fn skeleton_node_names(path: &Path) -> BTreeSet<String> {
    use project_wormhole_nif::{nif_block::NiObjectNETTraits, nif_file::NifFile};

    let nif = NifFile::open(path.to_str().unwrap()).unwrap();
    nif.get_nodes()
        .iter()
        .filter_map(|node| nif.header.get_string(node.name() as usize).ok())
        .map(str::to_owned)
        .collect()
}

fn resolved_bone_names(nif_path: &Path, skeleton_path: &Path) -> BTreeSet<String> {
    use project_wormhole_nif::nif_file::{NifFile, nif_to_model};

    let nif = NifFile::open(nif_path.to_str().unwrap()).unwrap();
    let skeleton = NifFile::open(skeleton_path.to_str().unwrap()).unwrap();
    let model = nif_to_model(&nif, Some(&skeleton)).unwrap();
    let first = model
        .skeletal_meshes
        .first()
        .expect("retail mesh must have a skeletal mesh");
    for (index, mesh) in model.skeletal_meshes.iter().enumerate().skip(1) {
        assert_eq!(
            mesh.bones, first.bones,
            "skeletal mesh {index} has its own bone tree"
        );
    }
    first.bones.bones.keys().cloned().collect()
}

fn convert(nif_path: &Path, skeleton_path: &Path, glb_path: &Path) -> color_eyre::Result<()> {
    MeshConverter::convert_nif_to_glb_with_skeleton(nif_path, glb_path, Some(skeleton_path))
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

fn accessor_f32x4(glb: &Glb, index: u64) -> Vec<[f32; 4]> {
    let accessor = &glb.json["accessors"][index as usize];
    assert_eq!(accessor["componentType"], 5126);
    assert_eq!(accessor["type"], "VEC4");
    let view = &glb.json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
    let count = accessor["count"].as_u64().unwrap() as usize;
    glb.bin[offset..offset + count * 16]
        .as_chunks::<16>()
        .0
        .iter()
        .map(|row| {
            let mut values = [0.0f32; 4];
            for (slot, value) in values.iter_mut().enumerate() {
                *value = f32::from_le_bytes(row[slot * 4..slot * 4 + 4].try_into().unwrap());
            }
            values
        })
        .collect()
}

fn accessor_f32x3(glb: &Glb, index: u64) -> Vec<[f32; 3]> {
    let accessor = &glb.json["accessors"][index as usize];
    assert_eq!(accessor["componentType"], 5126);
    assert_eq!(accessor["type"], "VEC3");
    let view = &glb.json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
    let count = accessor["count"].as_u64().unwrap() as usize;
    glb.bin[offset..offset + count * 12]
        .as_chunks::<12>()
        .0
        .iter()
        .map(|row| {
            [0, 1, 2]
                .map(|slot| f32::from_le_bytes(row[slot * 4..slot * 4 + 4].try_into().unwrap()))
        })
        .collect()
}

fn uses_model_space_normals(glb: &Glb, primitive: &Value) -> bool {
    primitive["material"].as_u64().is_some_and(|material| {
        glb.json["materials"][material as usize]["extras"]["openSkyrim"]["normalConvention"]
            == "model_space"
    })
}

/// `(mesh, primitive, positions, normals)` of one exported primitive.
type PrimitiveNormals = (usize, usize, Vec<[f32; 3]>, Vec<[f32; 3]>);

/// Every model-space-normal primitive's `NORMAL` accessor, after checking that it exists,
/// matches `POSITION` one to one and holds unit vectors. Returns `(mesh, primitive, positions,
/// normals)` per such primitive.
fn model_space_normals(glb: &Glb) -> Vec<PrimitiveNormals> {
    let mut found = Vec::new();
    for (mesh_index, mesh) in glb.json["meshes"].as_array().unwrap().iter().enumerate() {
        for (primitive_index, primitive) in
            mesh["primitives"].as_array().unwrap().iter().enumerate()
        {
            if !uses_model_space_normals(glb, primitive) {
                continue;
            }
            let attributes = &primitive["attributes"];
            let normal = attributes["NORMAL"].as_u64().unwrap_or_else(|| {
                panic!("model-space mesh {mesh_index} primitive {primitive_index} has no NORMAL")
            });
            let positions = accessor_f32x3(glb, attributes["POSITION"].as_u64().unwrap());
            let normals = accessor_f32x3(glb, normal);
            assert_eq!(
                normals.len(),
                positions.len(),
                "mesh {mesh_index} primitive {primitive_index} NORMAL count differs from POSITION"
            );
            for (vertex, value) in normals.iter().enumerate() {
                let length = value.iter().map(|axis| axis * axis).sum::<f32>().sqrt();
                assert!(
                    (length - 1.0).abs() < 1e-4,
                    "mesh {mesh_index} primitive {primitive_index} vertex {vertex} normal {value:?} has length {length}"
                );
            }
            found.push((mesh_index, primitive_index, positions, normals));
        }
    }
    found
}

/// Fraction of vertices whose normal points away from the primitive's centroid. A closed,
/// roughly convex surface with outward normals scores close to 1; inverted winding scores
/// close to 0.
fn outward_fraction(positions: &[[f32; 3]], normals: &[[f32; 3]]) -> f32 {
    let count = positions.len() as f32;
    let centroid = positions.iter().fold([0.0f32; 3], |sum, position| {
        [0, 1, 2].map(|axis| sum[axis] + position[axis] / count)
    });
    let outward = positions
        .iter()
        .zip(normals)
        .filter(|(position, normal)| {
            (0..3)
                .map(|axis| (position[axis] - centroid[axis]) * normal[axis])
                .sum::<f32>()
                > 0.0
        })
        .count();
    outward as f32 / count
}

fn accessor_min_max(glb: &Glb, index: u64) -> ([f32; 3], [f32; 3]) {
    fn read(values: &Value) -> [f32; 3] {
        let values = values.as_array().expect("accessor bound must be an array");
        [0, 1, 2].map(|axis| values[axis].as_f64().unwrap() as f32)
    }
    let accessor = &glb.json["accessors"][index as usize];
    (read(&accessor["min"]), read(&accessor["max"]))
}

fn assert_zero_weight_slots_are_joint_zero(glb: &Glb, mesh_index: usize, primitive_index: usize) {
    let attributes = &glb.json["meshes"][mesh_index]["primitives"][primitive_index]["attributes"];
    let joints = accessor_u8x4(glb, attributes["JOINTS_0"].as_u64().unwrap());
    let weights = accessor_f32x4(glb, attributes["WEIGHTS_0"].as_u64().unwrap());
    assert_eq!(joints.len(), weights.len());
    for (vertex, (joints, weights)) in joints.iter().zip(&weights).enumerate() {
        for slot in 0..4 {
            if weights[slot] == 0.0 {
                assert_eq!(
                    joints[slot], 0,
                    "mesh {mesh_index} primitive {primitive_index} vertex {vertex} slot {slot} has weight 0 but joint {}",
                    joints[slot]
                );
            }
        }
    }
}

fn assert_skinned_primitives(glb: &Glb) {
    assert!(
        glb.json["skins"]
            .as_array()
            .is_some_and(|skins| !skins.is_empty()),
        "GLB has no skins"
    );
    let skinned = skinned_meshes(glb);
    assert!(!skinned.is_empty(), "no node binds a mesh to a skin");
    for (mesh_index, _) in skinned {
        let primitives = glb.json["meshes"][mesh_index]["primitives"]
            .as_array()
            .unwrap();
        assert!(
            !primitives.is_empty(),
            "mesh {mesh_index} has no primitives"
        );
        for (primitive_index, primitive) in primitives.iter().enumerate() {
            let attributes = primitive["attributes"].as_object().unwrap();
            for semantic in ["POSITION", "JOINTS_0", "WEIGHTS_0"] {
                assert!(
                    attributes.contains_key(semantic),
                    "mesh {mesh_index} primitive {primitive_index} is missing {semantic}"
                );
            }
            let positions = attributes["POSITION"].as_u64().unwrap();
            for semantic in ["JOINTS_0", "WEIGHTS_0"] {
                let accessor = attributes[semantic].as_u64().unwrap();
                assert_eq!(
                    accessor_count(glb, accessor),
                    accessor_count(glb, positions),
                    "mesh {mesh_index} primitive {primitive_index} {semantic} count differs from POSITION"
                );
            }
        }
    }
}

#[test]
fn legacy_bsdismember_skin_exports_skin() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = write_fixture(directory.path(), &fixture);
    let skeleton_path = write_skeleton_fixture(directory.path(), &fixture);
    let glb_path = directory.path().join("skin.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();

    let glb = read_glb(&glb_path);
    assert_skinned_primitives(&glb);
    for (mesh_index, skin_index) in skinned_meshes(&glb) {
        let joints = glb.json["skins"][skin_index]["joints"].as_array().unwrap();
        assert!(!joints.is_empty(), "mesh {mesh_index} skin has no joints");
    }
}

#[test]
fn legacy_niskininstance_skin_exports_skin() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Plain,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = write_fixture(directory.path(), &fixture);
    let skeleton_path = write_skeleton_fixture(directory.path(), &fixture);
    let glb_path = directory.path().join("skin.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();

    let glb = read_glb(&glb_path);
    assert_skinned_primitives(&glb);
}

#[test]
fn legacy_skin_reads_buffer_indices_as_instance_bones() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = write_fixture(directory.path(), &fixture);
    let skeleton_path = write_skeleton_fixture(directory.path(), &fixture);
    let glb_path = directory.path().join("skin.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();

    let glb = read_glb(&glb_path);
    let (mesh_index, _) = skinned_meshes(&glb)[0];
    let joints_accessor = glb.json["meshes"][mesh_index]["primitives"][0]["attributes"]["JOINTS_0"]
        .as_u64()
        .unwrap();
    let joints = accessor_u8x4(&glb, joints_accessor);
    // Vertex 0's first influence is palette entry 0, which the shuffled
    // palette names as instance bone 1, and the vertex buffer stores that
    // bone directly. Mapping the buffer index through the palette a second
    // time would emit joint 0.
    assert_eq!(
        u16::from(joints[0][0]),
        SHUFFLED_PALETTE[0],
        "vertex 0 joint is not the instance bone its vertex buffer names"
    );
    assert_ne!(
        SHUFFLED_PALETTE[usize::from(SHUFFLED_PALETTE[0])],
        SHUFFLED_PALETTE[0],
        "palette must not be idempotent, or a second remap would go unnoticed"
    );
    assert_zero_weight_slots_are_joint_zero(&glb, mesh_index, 0);
}

#[test]
/// Invalid influence indices retain the shape as rigid geometry.
fn legacy_skin_keeps_geometry_for_out_of_range_joint() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        Some([2, 0, 0, 0]),
    );
    let nif_path = write_fixture(directory.path(), &fixture);
    let skeleton_path = write_skeleton_fixture(directory.path(), &fixture);
    let glb_path = directory.path().join("skin.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();
    let glb = read_glb(&glb_path);
    assert_eq!(glb.json["meshes"].as_array().unwrap().len(), 1);
    assert!(skinned_meshes(&glb).is_empty());
}

#[test]
fn legacy_skin_rejects_declared_skin_without_geometry() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        false,
        None,
    );
    let nif_path = write_fixture(directory.path(), &fixture);
    let skeleton_path = write_skeleton_fixture(directory.path(), &fixture);
    let glb_path = directory.path().join("skin.glb");
    let error = convert(&nif_path, &skeleton_path, &glb_path).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("declares a skin"),
        "error does not say the NIF declared a skin: {message}"
    );
    assert!(
        message.contains("no skeletal mesh"),
        "error does not say the skin produced no mesh: {message}"
    );
    assert!(
        message.contains(
            "skin instance 3 (BSDismemberSkinInstance) is not referenced by any shape block"
        ),
        "error does not name the unclaimed skin instance and its block type: {message}"
    );
    assert!(
        message.contains("skin.nif"),
        "error does not name the NIF path: {message}"
    );
    assert!(
        !glb_path.exists(),
        "a GLB was published for a skinless scene"
    );
}

#[test]
/// Unsupported LOD skinning does not discard its supported static geometry.
fn legacy_lod_skin_keeps_static_geometry() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = directory.path().join("skin.nif");
    fs::write(&nif_path, legacy_lod_skinned_shape(&fixture).unwrap()).unwrap();
    let skeleton_path = write_skeleton_fixture(directory.path(), &fixture);
    let glb_path = directory.path().join("skin.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();
    let glb = read_glb(&glb_path);
    assert_eq!(glb.json["meshes"].as_array().unwrap().len(), 1);
    assert!(skinned_meshes(&glb).is_empty());
}

#[test]
fn legacy_dynamic_tri_shape_exports_skin() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = directory.path().join("dynamic.nif");
    fs::write(&nif_path, legacy_dynamic_skinned_shape(&fixture).unwrap()).unwrap();
    let skeleton_path = write_skeleton_fixture(directory.path(), &fixture);
    let glb_path = directory.path().join("dynamic.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();

    let glb = read_glb(&glb_path);
    assert_skinned_primitives(&glb);
    let skinned = skinned_meshes(&glb);
    assert_eq!(skinned.len(), 1, "expected one skinned mesh node");
    let (mesh_index, skin_index) = skinned[0];
    assert!(
        !glb.json["skins"][skin_index]["joints"]
            .as_array()
            .unwrap()
            .is_empty(),
        "dynamic shape skin has no joints"
    );
    let attributes = &glb.json["meshes"][mesh_index]["primitives"][0]["attributes"];
    let positions = attributes["POSITION"].as_u64().unwrap();
    assert_eq!(
        accessor_count(&glb, positions),
        WEIGHTS.len(),
        "dynamic positions must come from the trailing array, one per vertex"
    );
    let (min, max) = accessor_min_max(&glb, positions);
    assert!(
        (min[2] - DYNAMIC_POSITION_OFFSET).abs() < 1e-4
            && (max[2] - DYNAMIC_POSITION_OFFSET).abs() < 1e-4,
        "dynamic positions won: expected z={DYNAMIC_POSITION_OFFSET}, got min={min:?} max={max:?}"
    );
    assert_zero_weight_slots_are_joint_zero(&glb, mesh_index, 0);
}

/// Converts the standard fixture with its shading normals supplied as `normals` says.
fn convert_with_normals(directory: &Path, normals: SkinNormals, dynamic: bool) -> Glb {
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let name = format!("{normals:?}-{dynamic}").to_ascii_lowercase();
    let nif_path = directory.join(format!("{name}.nif"));
    let bytes = if dynamic {
        legacy_dynamic_skinned_shape_with_normals(&fixture, normals)
    } else {
        legacy_skinned_shape_with_normals(&fixture, normals)
    };
    fs::write(&nif_path, bytes.unwrap()).unwrap();
    let skeleton_path = write_skeleton_fixture(directory, &fixture);
    let glb_path = directory.join(format!("{name}.glb"));
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();
    let glb = read_glb(&glb_path);
    assert_skinned_primitives(&glb);
    glb
}

#[test]
fn legacy_model_space_skin_gets_smooth_vertex_normals() {
    let directory = tempfile::tempdir().unwrap();
    for dynamic in [false, true] {
        let glb = convert_with_normals(directory.path(), SkinNormals::ModelSpaceMap, dynamic);
        let primitives = model_space_normals(&glb);
        assert_eq!(
            primitives.len(),
            1,
            "dynamic={dynamic}: the model-space shape must be exported once"
        );
        let (_, _, positions, normals) = &primitives[0];
        assert_eq!(positions.len(), WEIGHTS.len());
        // The fixture is a flat fan wound counter-clockwise about +Z.
        for (vertex, normal) in normals.iter().enumerate() {
            assert!(
                normal[2] > 0.9999,
                "dynamic={dynamic} vertex {vertex}: expected +Z, got {normal:?}"
            );
        }
    }
}

#[test]
fn skins_with_vertex_normals_or_tangent_maps_keep_their_normal_attribute() {
    let directory = tempfile::tempdir().unwrap();
    let normal_attribute = |glb: &Glb| {
        let (mesh_index, _) = skinned_meshes(glb)[0];
        glb.json["meshes"][mesh_index]["primitives"][0]["attributes"]["NORMAL"].as_u64()
    };

    let omitted = convert_with_normals(directory.path(), SkinNormals::Omitted, false);
    assert_eq!(
        normal_attribute(&omitted),
        None,
        "a tangent-space shape without vertex normals must stay without NORMAL"
    );

    let authored = convert_with_normals(directory.path(), SkinNormals::Authored, false);
    let computed = convert_with_normals(directory.path(), SkinNormals::ModelSpaceMap, false);
    let authored_normals = accessor_f32x3(&authored, normal_attribute(&authored).unwrap());
    let computed_normals = accessor_f32x3(&computed, normal_attribute(&computed).unwrap());
    assert!(authored_normals.iter().all(|normal| normal[2] > 0.99));
    // Packed authored normals decode with byte quantisation, the computed ones are exact, so
    // equal values would mean the authored normals had been replaced.
    assert_ne!(
        authored_normals, computed_normals,
        "authored vertex normals must pass through unchanged"
    );
}

#[test]
fn legacy_skeleton_duplicate_leaf_names_keep_the_first_and_skip_non_nodes() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = write_fixture(directory.path(), &fixture);
    let skeleton_path =
        write_node_skeleton(directory.path(), "Bone0", &["Bone1", "Helper", "Helper"]);
    let glb_path = directory.path().join("skin.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();

    let glb = read_glb(&glb_path);
    assert_skinned_primitives(&glb);
    let helpers = gltf_nodes(&glb)
        .iter()
        .filter(|node| node["name"] == "Helper")
        .count();
    assert_eq!(helpers, 1, "the duplicate helper leaf must be kept once");
}

#[test]
fn legacy_skeleton_duplicate_bound_joint_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = write_fixture(directory.path(), &fixture);
    let skeleton_path = write_node_skeleton(directory.path(), "Root", &["Bone0", "Bone0", "Bone1"]);
    let glb_path = directory.path().join("skin.glb");
    let error = convert(&nif_path, &skeleton_path, &glb_path).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("\"Bone0\"") && message.contains("more than one skeleton node"),
        "error does not name the ambiguous joint: {message}"
    );
    assert!(
        !glb_path.exists(),
        "a GLB was published for an ambiguous skin"
    );
}

#[test]
fn legacy_skin_binds_mesh_local_joints_when_the_skeleton_covers_half() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = write_fixture(directory.path(), &fixture);
    let skeleton_path = write_single_node_skeleton(directory.path(), "Bone0");
    let glb_path = directory.path().join("skin.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();

    let glb = read_glb(&glb_path);
    assert_skinned_primitives(&glb);
    let local_index = gltf_nodes(&glb)
        .iter()
        .position(|node| node["name"] == "Bone1")
        .expect("the mesh-local joint must be exported");
    let scene_roots: BTreeSet<usize> = glb.json["scenes"][0]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|node| node.as_u64().unwrap() as usize)
        .collect();
    for (index, node) in gltf_nodes(&glb).iter().enumerate() {
        if scene_roots.contains(&index) {
            continue;
        }
        if let Some(children) = node["children"].as_array() {
            assert!(
                !children
                    .iter()
                    .any(|child| child.as_u64() == Some(local_index as u64)),
                "node {index} parents the local joint although its NIF parent is the root"
            );
        }
    }
    let skin = &glb.json["skins"][0];
    assert_eq!(skin["joints"].as_array().unwrap().len(), 2);
}

#[test]
/// An explicitly supplied skeleton with no matching joints retains rigid geometry.
fn legacy_skin_without_skeleton_coverage_keeps_geometry() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = write_fixture(directory.path(), &fixture);
    let skeleton_path = write_single_node_skeleton(directory.path(), "SkeletonRoot");
    let glb_path = directory.path().join("skin.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();
    let glb = read_glb(&glb_path);
    assert_eq!(glb.json["meshes"].as_array().unwrap().len(), 1);
    assert!(skinned_meshes(&glb).is_empty());
}

#[test]
fn legacy_skin_binds_missing_ancestors_under_the_skeleton_bone() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = spec(
        SkinInstanceKind::Dismember,
        &SHUFFLED_PALETTE,
        &WEIGHTS,
        true,
        None,
    );
    let nif_path = directory.path().join("nested.nif");
    fs::write(&nif_path, legacy_nested_skinned_shape(&fixture).unwrap()).unwrap();
    let skeleton_path = write_single_node_skeleton(directory.path(), "Bone0");
    let glb_path = directory.path().join("nested.glb");
    convert(&nif_path, &skeleton_path, &glb_path).unwrap();

    let glb = read_glb(&glb_path);
    assert_skinned_primitives(&glb);
    let nodes = gltf_nodes(&glb);
    let index_of = |name: &str| {
        nodes
            .iter()
            .position(|node| node["name"] == name)
            .unwrap_or_else(|| panic!("GLB has no node named {name}"))
    };
    let children_of = |index: usize| -> Vec<u64> {
        nodes[index]["children"]
            .as_array()
            .map(|children| children.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default()
    };
    assert_eq!(
        nodes.iter().filter(|node| node["name"] == "Bone0").count(),
        1,
        "the skeleton bone must exist once"
    );
    let skeleton_bone = index_of("Bone0");
    let intermediate = index_of("NestedParent");
    let joint = index_of("Bone1");

    assert!(
        children_of(skeleton_bone).contains(&(intermediate as u64)),
        "the skeleton bone must parent the intermediate node"
    );
    assert!(
        !children_of(skeleton_bone).contains(&(joint as u64)),
        "the skeleton bone must not parent the local joint directly"
    );
    assert!(
        children_of(intermediate).contains(&(joint as u64)),
        "the intermediate node must parent the local joint"
    );
    assert!(
        children_of(joint).is_empty(),
        "the local joint must not parent anything"
    );

    let mut parent = BTreeMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if let Some(children) = node["children"].as_array() {
            for child in children {
                let child = child.as_u64().unwrap();
                assert!(
                    parent.insert(child, index as u64).is_none(),
                    "node {child} appears in more than one children array"
                );
            }
        }
    }
    assert_eq!(
        parent.get(&(intermediate as u64)),
        Some(&(skeleton_bone as u64)),
        "the intermediate node must hang under the skeleton bone, not under a joint or the scene"
    );
    assert_eq!(parent.get(&(joint as u64)), Some(&(intermediate as u64)));

    let joints = glb.json["skins"][0]["joints"].as_array().unwrap();
    assert_eq!(joints.len(), 2);
    for index in [skeleton_bone, joint] {
        assert!(
            joints
                .iter()
                .any(|value| value.as_u64() == Some(index as u64)),
            "skin joint node {index} is missing"
        );
    }
}

fn actor_root() -> Option<PathBuf> {
    std::env::var_os("MUDCRAB_ACTOR_NIF_DIR").map(PathBuf::from)
}

static CAPTURED_WARNINGS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn warning_slot() -> &'static Mutex<Vec<String>> {
    CAPTURED_WARNINGS.get_or_init(|| Mutex::new(Vec::new()))
}

struct WarningCapture;

impl log::Log for WarningCapture {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if record.level() <= log::Level::Warn
            && let Ok(mut slot) = warning_slot().lock()
        {
            slot.push(format!("{}", record.args()));
        }
    }

    fn flush(&self) {}
}

fn install_warning_capture() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = log::set_boxed_logger(Box::new(WarningCapture));
        log::set_max_level(log::LevelFilter::Warn);
    });
}

fn take_captured_warnings() -> Vec<String> {
    warning_slot()
        .lock()
        .map(|mut slot| std::mem::take(&mut *slot))
        .unwrap_or_default()
}

#[test]
#[ignore = "requires MUDCRAB_ACTOR_NIF_DIR pointing at a Skyrim meshes/actors tree"]
fn retail_actor_skeleton_resolution_matches_reference_ranking() {
    let Some(root) = actor_root() else {
        return;
    };
    install_warning_capture();

    // (mesh under actors/, expected skeleton under actors/ or None for a
    // self-contained rig, whether the ranking must warn about a tie)
    let rows: &[(&str, Option<&str>, bool)] = &[
        (
            "alduin/alduin.nif",
            Some("dragon/character assets/skeleton.nif"),
            false,
        ),
        (
            "alduin/farskin_alduin.nif",
            Some("dragon/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc01/dragon/dragon_purple.nif",
            Some("dragon/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc02/dragon/dragon_blackvariant.nif",
            Some("dragon/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc01/character/hair/dlc1femaleseranahair.nif",
            Some("character/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc01/ancientfrostatronach/ancientfrostatronach.nif",
            Some("atronachfrost/character assets/skeleton.nif"),
            true,
        ),
        (
            "dlc01/soulwisp/character assets/dlcsoulcairnwisparmoraddon.nif",
            Some("witchlight/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc01/chaurusfrozen/chaurusfrozen.nif",
            Some("chaurus/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc02/spider_poison/characterassets/spider_zombie.nif",
            Some("frostbitespider/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc02/ashman/ashman.nif",
            Some("draugr/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc02/hulkingdraugr/hulkingdraugr.nif",
            Some("draugr/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc02/dragonpriestacolyte/dragonpriestacolyte03.nif",
            Some("dragonpriest/character assets/skeleton.nif"),
            false,
        ),
        (
            "dlc02/werebear/werebear.nif",
            Some("werewolfbeast/character assets/skeleton.nif"),
            false,
        ),
        (
            "dog/character assets/mg07dog.nif",
            Some("canine/character assets dog/skeleton.nif"),
            true,
        ),
        (
            "wolf/fox.nif",
            Some("canine/character assets dog/skeleton.nif"),
            true,
        ),
        (
            "manekin/manekin.nif",
            Some("character/character assets/skeleton.nif"),
            true,
        ),
        (
            "manekin/manekin_feet.nif",
            Some("character/character assets/skeleton.nif"),
            true,
        ),
        (
            "manekin/manekinhands.nif",
            Some("character/character assets/skeleton.nif"),
            true,
        ),
        (
            "manekin/manekinhead.nif",
            Some("character/character assets/skeleton.nif"),
            true,
        ),
        (
            "male/greybeardstatic.nif",
            Some("character/character assets/skeleton.nif"),
            true,
        ),
        (
            "character/character assets/maletailkhajiit.nif",
            Some("character/character assets/skeletonbeast.nif"),
            false,
        ),
        ("dog/character assets/fxmg07dogeyeglow.nif", None, false),
        ("draugr/character assets/fxdraugrmaleeyes.nif", None, false),
        (
            "atronachflame/character assets/fxatronachfiretrail.nif",
            None,
            false,
        ),
        (
            "witchlight/character assets/witchlightmesh.nif",
            None,
            false,
        ),
        (
            "dlc02/spider_poison/characterassets/spiderinhandfxfire.nif",
            None,
            false,
        ),
        (
            "character/character assets/malebody_1.nif",
            Some("character/character assets/skeleton.nif"),
            false,
        ),
    ];

    for (mesh, expected, tie) in rows {
        let nif_path = root.join(mesh);
        assert!(nif_path.is_file(), "missing {}", nif_path.display());
        let _ = take_captured_warnings();
        let resolution = find_skeleton(&nif_path);
        let warnings = take_captured_warnings();
        let actual = match (&resolution, expected) {
            (SkeletonResolution::Skeleton(path), Some(expected)) => {
                let actual = path
                    .strip_prefix(&root)
                    .unwrap_or(path.as_path())
                    .to_string_lossy()
                    .replace('\\', "/");
                assert_eq!(&actual, expected, "{mesh}");
                actual
            }
            (SkeletonResolution::SelfContained, None) => "<self-contained>".to_owned(),
            _ => panic!("{mesh}: expected {expected:?}, got {resolution:?}; warnings={warnings:?}"),
        };
        if *tie {
            let file_name = Path::new(mesh)
                .file_name()
                .expect("mesh has a file name")
                .to_string_lossy()
                .into_owned();
            assert!(
                warnings.iter().any(|warning| {
                    warning.contains(&file_name) && warning.to_ascii_lowercase().contains("tie")
                }),
                "{mesh} -> {actual}: expected a tie warning, got {warnings:?}"
            );
        }
    }
}

#[test]
#[ignore = "requires MUDCRAB_ACTOR_NIF_DIR pointing at a Skyrim meshes/actors tree"]
fn retail_pipeline_and_standalone_resolution_agree() {
    let Some(root) = actor_root() else {
        return;
    };
    let Some(meshes_root) = root.parent() else {
        return;
    };
    let mut nifs = Vec::new();
    collect_nif_paths(&root, &mut nifs);
    assert!(nifs.len() > 1000, "expected a retail actor tree");
    nifs.sort();

    let mut sources = BTreeMap::new();
    for path in &nifs {
        if let Ok(relative) = path.strip_prefix(meshes_root) {
            sources.insert(relative.to_path_buf(), path.clone());
        }
    }

    // The pipeline index is rebuilt per call here, so a stride keeps the test bounded.
    let mut checked = 0;
    for (position, path) in nifs.iter().enumerate() {
        if position % 41 != 0 {
            continue;
        }
        let Ok(relative) = path.strip_prefix(meshes_root) else {
            continue;
        };
        let standalone = find_skeleton(path);
        let pipeline = converter::mesh::resolve_skeleton(relative, &sources);
        let describe = |resolution: &SkeletonResolution| match resolution {
            SkeletonResolution::Skeleton(candidate) => {
                let candidate = candidate.strip_prefix(meshes_root).unwrap_or(candidate);
                format!(
                    "skeleton:{}",
                    candidate
                        .to_string_lossy()
                        .replace('\\', "/")
                        .to_lowercase()
                )
            }
            SkeletonResolution::SelfContained => "self-contained".to_owned(),
            SkeletonResolution::Unresolved { best_coverage } => {
                format!("unresolved:{best_coverage:.3}")
            }
        };
        assert_eq!(
            describe(&standalone),
            describe(&pipeline),
            "{} resolves differently on the two paths",
            relative.display()
        );
        checked += 1;
    }
    assert!(checked > 50, "only checked {checked} meshes");
}

fn collect_nif_paths(directory: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_nif_paths(&path, paths);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("nif"))
        {
            paths.push(path);
        }
    }
}

#[test]
#[ignore = "requires MUDCRAB_ACTOR_NIF_DIR pointing at a Skyrim meshes/actors tree"]
fn retail_malebody_has_skin_and_joint_nodes() {
    let Some(root) = actor_root() else {
        return;
    };
    let nif_path = root.join("character/character assets/malebody_1.nif");
    assert!(nif_path.is_file(), "missing {}", nif_path.display());
    let directory = tempfile::tempdir().unwrap();
    let glb_path = directory.path().join("malebody_1.glb");
    MeshConverter::convert_nif_to_glb(&nif_path, &glb_path).unwrap();

    let glb = read_glb(&glb_path);
    let skins = glb.json["skins"].as_array().unwrap();
    // This body has two skinned shapes, a 5-joint and a 24-joint one, so the
    // exporter writes two skins (not the one an older plan assumed).
    assert_eq!(skins.len(), 2, "malebody_1 must carry two skins");
    for (skin_index, skin) in skins.iter().enumerate() {
        let joints = skin["joints"].as_array().unwrap();
        let inverse_binds = skin["inverseBindMatrices"].as_u64().unwrap();
        assert_eq!(
            joints.len(),
            accessor_count(&glb, inverse_binds),
            "skin {skin_index} joint and inverse-bind counts differ"
        );
    }

    let skinned = skinned_meshes(&glb);
    assert_eq!(skinned.len(), 2, "expected two skinned mesh nodes");
    for (mesh_index, skin_index) in skinned {
        let joints = glb.json["skins"][skin_index]["joints"].as_array().unwrap();
        let primitives = glb.json["meshes"][mesh_index]["primitives"]
            .as_array()
            .unwrap();
        for (primitive_index, primitive) in primitives.iter().enumerate() {
            let attributes = primitive["attributes"].as_object().unwrap();
            for semantic in ["JOINTS_0", "WEIGHTS_0"] {
                assert!(
                    attributes.contains_key(semantic),
                    "mesh {mesh_index} primitive {primitive_index} is missing {semantic}"
                );
            }
            let joints_accessor = attributes["JOINTS_0"].as_u64().unwrap();
            let values = accessor_u8x4(&glb, joints_accessor);
            let max = values.iter().flatten().copied().max().unwrap_or(0);
            assert!(
                usize::from(max) < joints.len(),
                "mesh {mesh_index} primitive {primitive_index} joint {max} >= {}",
                joints.len()
            );
            assert_eq!(
                accessor_count(&glb, joints_accessor),
                accessor_count(&glb, attributes["POSITION"].as_u64().unwrap()),
                "mesh {mesh_index} primitive {primitive_index} joint count differs from positions"
            );
            println!(
                "mesh={mesh_index} primitive={primitive_index} skin={skin_index} joints={} vertices={} max_joint={max}",
                joints.len(),
                accessor_count(&glb, attributes["POSITION"].as_u64().unwrap()),
            );
        }
    }

    let skeleton_path = match find_skeleton(&nif_path) {
        SkeletonResolution::Skeleton(path) => path,
        other => panic!("malebody_1 must resolve a skeleton, got {other:?}"),
    };
    let skeleton_names = skeleton_node_names(&skeleton_path);
    assert!(!skeleton_names.is_empty(), "skeleton has no nodes");
    let bone_names = resolved_bone_names(&nif_path, &skeleton_path);

    let nodes = gltf_nodes(&glb);
    let mut bone_nodes = BTreeSet::new();
    for name in &bone_names {
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
        bone_nodes.insert(matching[0]);
    }
    assert_eq!(
        bone_nodes.len(),
        skeleton_names.len(),
        "unique bone nodes must equal the unique bone names of the resolved skeleton"
    );

    let (parents, reachable) = scene_graph(&glb);
    assert_eq!(
        reachable.len(),
        nodes.len(),
        "every node must be reachable from scenes[0].nodes, but {} are orphaned",
        nodes.len() - reachable.len()
    );
    for (skin_index, skin) in skins.iter().enumerate() {
        for joint in skin["joints"].as_array().unwrap() {
            let joint = joint.as_u64().unwrap() as usize;
            assert!(
                reachable.contains(&joint),
                "skin {skin_index} joint node {joint} is unreachable from the scene"
            );
        }
    }

    let npc_head = nodes
        .iter()
        .filter_map(|node| node["name"].as_str())
        .any(|name| name == "NPC Head [Head]");
    assert!(npc_head, "NPC Head [Head] must be present");
    println!(
        "skins={} nodes={} reachable={} bone_nodes={} skeleton_bones={} parents={} npc_head_node_present={npc_head}",
        skins.len(),
        nodes.len(),
        reachable.len(),
        bone_nodes.len(),
        skeleton_names.len(),
        parents.len(),
    );
}

#[test]
#[ignore = "requires MUDCRAB_ACTOR_NIF_DIR pointing at a Skyrim meshes/actors tree"]
fn retail_heads_export_skin() {
    let Some(root) = actor_root() else {
        return;
    };
    let heads = [
        "character/character assets/malehead.nif",
        "character/facegendata/facegeom/skyrim.esm/0010f5a1.nif",
    ];
    let directory = tempfile::tempdir().unwrap();
    for (index, relative) in heads.iter().enumerate() {
        let nif_path = root.join(relative);
        assert!(nif_path.is_file(), "missing {}", nif_path.display());
        let glb_path = directory.path().join(format!("head{index}.glb"));
        MeshConverter::convert_nif_to_glb(&nif_path, &glb_path).unwrap();

        let glb = read_glb(&glb_path);
        assert_skinned_primitives(&glb);
        let skins = glb.json["skins"].as_array().unwrap();
        let mut vertices = 0usize;
        for (mesh_index, _) in skinned_meshes(&glb) {
            let attributes = &glb.json["meshes"][mesh_index]["primitives"][0]["attributes"];
            let positions = attributes["POSITION"].as_u64().unwrap();
            let count = accessor_count(&glb, positions);
            vertices += count;
            let (min, max) = accessor_min_max(&glb, positions);
            assert!(
                min.iter().chain(&max).all(|value| value.is_finite()),
                "{} mesh {mesh_index} has non-finite bounds {min:?}..{max:?}",
                nif_path.display()
            );
            let extent = max[0] - min[0] + (max[1] - min[1]) + (max[2] - min[2]);
            assert!(
                extent > 1.0,
                "{} mesh {mesh_index} has degenerate bounds {min:?}..{max:?}",
                nif_path.display()
            );
            println!("head={relative} mesh={mesh_index} vertices={count} min={min:?} max={max:?}");
        }
        assert!(
            vertices > 100,
            "{} exported an implausible {vertices} skinned vertices",
            nif_path.display()
        );
        println!("head={relative} skins={} vertices={vertices}", skins.len());

        let model_space = model_space_normals(&glb);
        assert!(
            !model_space.is_empty(),
            "{} has no model-space-normal primitive",
            nif_path.display()
        );
        // The head skin is the largest model-space shape; eyes, mouth and brows are small
        // and not closed around their own centroid.
        let (mesh_index, _, positions, normals) = model_space
            .iter()
            .max_by_key(|(_, _, positions, _)| positions.len())
            .unwrap();
        let outward = outward_fraction(positions, normals);
        println!(
            "head={relative} model_space_primitives={} largest_mesh={mesh_index} vertices={} outward={outward:.3}",
            model_space.len(),
            positions.len()
        );
        assert!(
            outward > 0.8,
            "{} head normals point inward: only {outward:.3} face away from the centroid",
            nif_path.display()
        );
    }
}

#[test]
#[ignore = "requires MUDCRAB_ACTOR_NIF_DIR pointing at a Skyrim meshes/actors tree"]
fn retail_actor_sample_skin_rate() {
    let Some(root) = actor_root() else {
        return;
    };
    let files = stratified_sample(&root, SAMPLE_PER_TOP_LEVEL_DIRECTORY);

    let directory = tempfile::tempdir().unwrap();
    let (mut produced_skin, mut errored, mut skipped) = (0usize, 0usize, 0usize);
    let mut per_directory: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    let mut joint_histogram: BTreeMap<String, usize> = BTreeMap::new();
    let mut error_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for (index, nif_path) in files.iter().enumerate() {
        let top = top_level_directory(&root, nif_path);
        let bytes = match fs::read(nif_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                errored += 1;
                per_directory.entry(top).or_default().2 += 1;
                println!(
                    "FAIL file={} stage=read panic=<none> error={error:#}",
                    nif_path.display()
                );
                continue;
            }
        };
        if !header_declares_skin(&bytes) {
            skipped += 1;
            per_directory.entry(top).or_default().1 += 1;
            continue;
        }
        let skeleton = MeshConverter::dependency_paths(nif_path);
        let skeleton = skeleton
            .first()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".to_owned());
        let glb_path = directory.path().join(format!("{index}.glb"));
        let _ = take_captured_panic();
        match MeshConverter::convert_nif_to_glb(nif_path, &glb_path) {
            Ok(()) => {
                per_directory.entry(top).or_default().0 += 1;
                let glb = read_glb(&glb_path);
                if glb
                    .json
                    .get("skins")
                    .and_then(Value::as_array)
                    .is_some_and(|skins| !skins.is_empty())
                {
                    produced_skin += 1;
                }
                let _ = fs::remove_file(&glb_path);
            }
            Err(error) => {
                errored += 1;
                per_directory.entry(top).or_default().2 += 1;
                let panic = take_captured_panic();
                let message = format!("{error:#}");
                let missing = panic
                    .as_deref()
                    .and_then(missing_joint)
                    .or_else(|| unresolved_joint(&message));
                let kind = match missing {
                    Some(joint) => {
                        *joint_histogram.entry(joint.to_owned()).or_default() += 1;
                        "joint_not_found"
                    }
                    None => "other",
                };
                *error_kinds.entry(kind.to_owned()).or_default() += 1;
                println!(
                    "FAIL file={} skeleton={} panic={panic:?} error={error:#}",
                    nif_path.display(),
                    skeleton
                );
            }
        }
    }
    println!(
        "sample={} per_directory_cap={SAMPLE_PER_TOP_LEVEL_DIRECTORY} skipped_without_skin={skipped} produced_skin={produced_skin} errored={errored}",
        files.len()
    );
    for (directory, (ok, skip, error)) in &per_directory {
        let status = if *error == 0 { "pass" } else { "fail" };
        println!(
            "PER_DIRECTORY dir={directory} converted_ok={ok} skipped={skip} errored={error} status={status}"
        );
    }
    for (joint, count) in &joint_histogram {
        println!("MISSING_JOINT joint={joint} count={count}");
    }
    for (kind, count) in &error_kinds {
        println!("ERROR_KIND kind={kind} count={count}");
    }
    assert_eq!(
        errored, 0,
        "{errored} sampled skinned NIFs failed conversion; see the FAIL lines above"
    );
}

/// The vendor exporter panics with the missing joint name; the converter
/// catches the panic and drops the payload, so record it before it unwinds.
static CAPTURED_PANIC: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn captured_panic_slot() -> &'static Mutex<Option<String>> {
    CAPTURED_PANIC.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|value| (*value).to_owned())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_owned());
            if let Some(slot) = CAPTURED_PANIC.get()
                && let Ok(mut slot) = slot.lock()
            {
                *slot = Some(message);
            }
            previous(info);
        }));
        Mutex::new(None)
    })
}

fn take_captured_panic() -> Option<String> {
    captured_panic_slot()
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
}

fn missing_joint(message: &str) -> Option<&str> {
    let rest = message.split_once("Joint ")?.1;
    let rest = rest.split_once(" not found")?.0;
    Some(rest.rsplit_once(':').map_or(rest, |(name, _)| name))
}

fn unresolved_joint(message: &str) -> Option<&str> {
    let rest = message
        .split_once("skin joints could not be resolved against the bone tree:")?
        .1;
    let first = rest.split(',').next()?.trim();
    Some(first.rsplit_once(':').map_or(first, |(name, _)| name))
}

/// Deterministic sample spread across each top-level actor directory: up to
/// `per_directory` files per directory, evenly spaced in sorted order so a
/// single subdirectory (for example `character/facegendata`) cannot dominate.
const SAMPLE_PER_TOP_LEVEL_DIRECTORY: usize = 4;

fn stratified_sample(root: &Path, per_directory: usize) -> Vec<PathBuf> {
    let mut by_directory: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut all = Vec::new();
    collect_nifs(root, &mut all);
    for path in all {
        by_directory
            .entry(top_level_directory(root, &path))
            .or_default()
            .push(path);
    }

    let mut sample = Vec::new();
    for files in by_directory.values_mut() {
        files.sort();
        let take = per_directory.min(files.len());
        for slot in 0..take {
            sample.push(files[slot * files.len() / take].clone());
        }
    }
    sample
}

fn top_level_directory(root: &Path, nif_path: &Path) -> String {
    nif_path
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| "<outside-root>".to_owned())
}

fn collect_nifs(directory: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_nifs(&path, output);
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("nif"))
        {
            output.push(path);
        }
    }
}

struct HeaderLayout {
    block_types: Vec<String>,
    block_type_index: Vec<u16>,
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
    let mut block_types = Vec::with_capacity(usize::from(type_count));
    for _ in 0..type_count {
        block_types.push(read_string32(bytes, &mut cursor)?);
    }
    let mut block_type_index = Vec::with_capacity(block_count as usize);
    for _ in 0..block_count {
        block_type_index.push(read_u16(bytes, &mut cursor)?);
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
        block_types,
        block_type_index,
        block_sizes,
        block_data_start: cursor,
    })
}

fn header_declares_skin(bytes: &[u8]) -> bool {
    let Some(header) = parse_header(bytes) else {
        return false;
    };
    header.block_type_index.iter().any(|index| {
        header
            .block_types
            .get(usize::from(*index))
            .is_some_and(|name| {
                matches!(
                    name.as_str(),
                    "BSSkin::Instance"
                        | "BSSkinInstance"
                        | "NiSkinInstance"
                        | "BsDismemberSkinInstance"
                        | "BSDismemberSkinInstance"
                )
            })
    })
}
