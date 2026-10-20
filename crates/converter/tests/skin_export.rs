//! Legacy Skyrim skin export (#273 stage 1a): a NIF that declares a skin must
//! either publish a skinned GLB or fail, never a static fallback.

use converter::mesh::MeshConverter;
use dummy_content::nif::{
    DYNAMIC_POSITION_OFFSET, LegacySkin, SkinInstanceKind, legacy_dynamic_skinned_shape,
    legacy_lod_skinned_shape, legacy_skinned_shape,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
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
fn legacy_skin_remaps_partition_palette() {
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
    // Vertex 0's raw slot 0 is palette index 0; the shuffled palette maps that
    // to joint 1. Emitting the raw index here means the remap was skipped.
    assert_eq!(
        u16::from(joints[0][0]),
        SHUFFLED_PALETTE[0],
        "vertex 0 joint was not remapped through the partition palette"
    );
    assert_ne!(
        u16::from(joints[0][0]),
        0,
        "palette must differ from the raw joint to make the remap observable"
    );
    assert_zero_weight_slots_are_joint_zero(&glb, mesh_index, 0);
}

#[test]
fn legacy_skin_rejects_out_of_range_joint() {
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
    let error = convert(&nif_path, &skeleton_path, &glb_path).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("shape block"),
        "error does not name the offending shape/block: {message}"
    );
    assert!(
        message.contains("palette"),
        "error does not mention the palette: {message}"
    );
    assert!(!glb_path.exists(), "a static GLB was published anyway");
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
fn legacy_lod_skin_reports_the_unhandled_shape() {
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
    let error = convert(&nif_path, &skeleton_path, &glb_path).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains(
            "skin instance 4 (BSDismemberSkinInstance) is referenced by BSLODTriShape 3, which the legacy exporter does not handle"
        ),
        "error does not name the shape type that claimed no exported mesh: {message}"
    );
    assert!(
        message.contains("skin.nif"),
        "error does not name the NIF path: {message}"
    );
    assert!(!glb_path.exists(), "a static GLB was published anyway");
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

fn actor_root() -> Option<PathBuf> {
    std::env::var_os("MUDCRAB_ACTOR_NIF_DIR").map(PathBuf::from)
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

    let head = glb.json["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|node| node["name"].as_str())
        .any(|name| name.starts_with("NPC Head [Head]"));
    println!("skins={} npc_head_node_present={head}", skins.len());
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
    block_data_start: usize,
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
    for _ in 0..block_count {
        read_u32(bytes, &mut cursor)?;
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
