//! Usable geometry survives unusable skin data, including publication beside valid assets.

use converter::{
    AssetPipeline, PipelineConfig,
    mesh::{MeshConverter, SkeletonResolution, find_skeleton},
};
use dummy_content::{
    layout,
    nif::{
        LegacySkin, SkinInstanceKind, SkinNormals, StaticShape,
        legacy_dynamic_skinned_shape_with_normals, legacy_skinned_shape_with_normals,
        legacy_skinned_shapes, static_shape,
    },
};
use project_wormhole_nif::{
    bs::prelude::BSDynamicTriShape,
    nif_block::{BSSkinBoneData, BSSkinBoneTrans, BSSkinInstance, NifBlock},
    nif_file::{NifFile, nif_to_model, nif_to_model_with_skin_fallback},
};
use project_wormhole_shared::glam::{Mat4, Quat, Vec3};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

const GOOD: [[f32; 4]; 4] = [[0.75, 0.25, 0.0, 0.0]; 4];

/// Use shuffled palette indices and unequal weights so normalization cannot hide remapping.
fn skin<'a>(name: &'a str, weights: &'a [[f32; 4]]) -> LegacySkin<'a> {
    LegacySkin {
        name,
        instance_kind: SkinInstanceKind::Plain,
        bone_transforms: [
            ([0.0; 3], [0.0, 0.0, 0.0, 1.0], 1.0),
            ([0.0, 0.0, 2.0], [0.0, 0.0, 0.0, 1.0], 1.0),
        ],
        palette: &[1, 0],
        weights,
        with_geometry: true,
        joint_override: None,
    }
}

/// Read the writer's header to locate blocks without searching for coincidental byte patterns.
fn blocks(bytes: &[u8]) -> Vec<(String, usize)> {
    /// Advance over one little-endian header integer.
    fn u32_at(bytes: &[u8], cursor: &mut usize) -> u32 {
        let value = u32::from_le_bytes(bytes[*cursor..*cursor + 4].try_into().unwrap());
        *cursor += 4;
        value
    }
    /// Advance over one short header integer.
    fn u16_at(bytes: &[u8], cursor: &mut usize) -> u16 {
        let value = u16::from_le_bytes(bytes[*cursor..*cursor + 2].try_into().unwrap());
        *cursor += 2;
        value
    }
    /// Skip a length-prefixed exporter string.
    fn skip8(bytes: &[u8], cursor: &mut usize) {
        *cursor += 1 + bytes[*cursor] as usize;
    }
    /// Decode a length-prefixed type or name string.
    fn string32(bytes: &[u8], cursor: &mut usize) -> String {
        let length = u32_at(bytes, cursor) as usize;
        let value = String::from_utf8(bytes[*cursor..*cursor + length].to_vec()).unwrap();
        *cursor += length;
        value
    }
    let mut cursor = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    u32_at(bytes, &mut cursor);
    cursor += 1;
    u32_at(bytes, &mut cursor);
    let count = u32_at(bytes, &mut cursor);
    let version = u32_at(bytes, &mut cursor);
    if version > 130 {
        u32_at(bytes, &mut cursor);
    }
    skip8(bytes, &mut cursor);
    if version < 131 {
        skip8(bytes, &mut cursor);
    }
    skip8(bytes, &mut cursor);
    if version >= 103 {
        skip8(bytes, &mut cursor);
    }
    let type_count = u16_at(bytes, &mut cursor);
    let types: Vec<_> = (0..type_count)
        .map(|_| string32(bytes, &mut cursor))
        .collect();
    let indices: Vec<_> = (0..count).map(|_| u16_at(bytes, &mut cursor)).collect();
    let sizes: Vec<_> = (0..count).map(|_| u32_at(bytes, &mut cursor)).collect();
    let strings = u32_at(bytes, &mut cursor);
    u32_at(bytes, &mut cursor);
    for _ in 0..strings {
        string32(bytes, &mut cursor);
    }
    let groups = u32_at(bytes, &mut cursor);
    for _ in 0..groups {
        u32_at(bytes, &mut cursor);
    }
    indices
        .into_iter()
        .zip(sizes)
        .map(|(kind, size)| {
            let start = cursor;
            cursor += size as usize;
            (types[kind as usize].clone(), start)
        })
        .collect()
}

/// Mutate only a NiAVObject's translation and uniform scale, preserving every block size.
fn transform(bytes: &mut [u8], start: usize, translation: [f32; 3], scale: f32) {
    for (axis, value) in translation.into_iter().enumerate() {
        bytes[start + 16 + axis * 4..start + 20 + axis * 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[start + 64..start + 68].copy_from_slice(&scale.to_le_bytes());
}

/// Generate two parts under a nonidentity root block, with distinct shape transforms.
/// The root transform is authored but not expected in world space; each shape's own is.
fn fixture(weights: &[[f32; 4]], rigid: bool) -> Vec<u8> {
    let mut bytes =
        legacy_skinned_shapes(&[skin("Affected", weights), skin("Healthy", &GOOD)]).unwrap();
    let layout = blocks(&bytes);
    transform(&mut bytes, layout[0].1, [11.0, -7.0, 5.0], 2.0);
    let shapes: Vec<_> = layout
        .iter()
        .filter(|(kind, _)| kind == "BSTriShape")
        .collect();
    transform(&mut bytes, shapes[0].1, [3.0, 4.0, -2.0], 1.5);
    transform(&mut bytes, shapes[1].1, [-4.0, 2.0, 9.0], 0.75);
    if rigid {
        // NiAVObject (72 bytes), bounding sphere (16 bytes), then skin reference.
        bytes[shapes[0].1 + 88..shapes[0].1 + 92].copy_from_slice(&u32::MAX.to_le_bytes());
    }
    bytes
}

struct Glb {
    json: Value,
    bin: Vec<u8>,
}

/// Read JSON and binary chunks from an actual public-API GLB output.
fn read_glb(path: &Path) -> Glb {
    let bytes = fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"glTF");
    let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json = serde_json::from_slice(&bytes[20..20 + length]).unwrap();
    let bin_start = 28 + length;
    Glb {
        json,
        bin: bytes[bin_start..].to_vec(),
    }
}

/// Select a mesh by its authored name rather than relying on export order.
fn mesh_node<'a>(glb: &'a Glb, name: &str) -> (usize, &'a Value) {
    let nodes = glb.json["nodes"].as_array().unwrap();
    nodes
        .iter()
        .enumerate()
        .find(|(_, node)| {
            node["mesh"].as_u64().is_some_and(|mesh| {
                node["name"].as_str() == Some(name)
                    || glb.json["meshes"][mesh as usize]["name"].as_str() == Some(name)
            })
        })
        .unwrap_or_else(|| panic!("missing geometry for {name}: {}", glb.json))
}

/// Decode float accessors, respecting buffer-view stride and accessor offset.
fn floats<const N: usize>(glb: &Glb, index: u64) -> Vec<[f32; N]> {
    let accessor = &glb.json["accessors"][index as usize];
    assert_eq!(accessor["componentType"], 5126);
    let view = &glb.json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let start = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
    let stride = view["byteStride"].as_u64().unwrap_or((N * 4) as u64) as usize;
    (0..accessor["count"].as_u64().unwrap() as usize)
        .map(|row| {
            std::array::from_fn(|axis| {
                let offset = start + row * stride + axis * 4;
                f32::from_le_bytes(glb.bin[offset..offset + 4].try_into().unwrap())
            })
        })
        .collect()
}

/// Compose the exported scene hierarchy so either flattened or nested rigid nodes are valid.
fn world_matrix(glb: &Glb, index: usize) -> Mat4 {
    let nodes = glb.json["nodes"].as_array().unwrap();
    let node = &nodes[index];
    let local = if let Some(matrix) = node["matrix"].as_array() {
        Mat4::from_cols_array(&std::array::from_fn(|i| matrix[i].as_f64().unwrap() as f32))
    } else {
        let translation = node["translation"].as_array().map_or(Vec3::ZERO, |v| {
            Vec3::new(
                v[0].as_f64().unwrap() as f32,
                v[1].as_f64().unwrap() as f32,
                v[2].as_f64().unwrap() as f32,
            )
        });
        let scale = node["scale"].as_array().map_or(Vec3::ONE, |v| {
            Vec3::new(
                v[0].as_f64().unwrap() as f32,
                v[1].as_f64().unwrap() as f32,
                v[2].as_f64().unwrap() as f32,
            )
        });
        let rotation = node["rotation"].as_array().map_or(Quat::IDENTITY, |v| {
            Quat::from_xyzw(
                v[0].as_f64().unwrap() as f32,
                v[1].as_f64().unwrap() as f32,
                v[2].as_f64().unwrap() as f32,
                v[3].as_f64().unwrap() as f32,
            )
        });
        Mat4::from_scale_rotation_translation(scale, rotation, translation)
    };
    if let Some(parent) = nodes.iter().position(|node| {
        node["children"].as_array().is_some_and(|children| {
            children
                .iter()
                .any(|child| child.as_u64() == Some(index as u64))
        })
    }) {
        world_matrix(glb, parent) * local
    } else {
        local
    }
}

/// Require all four source vertices and both triangles, static influences, and authored world positions.
fn assert_rigid(glb: &Glb, name: &str) {
    let (index, node) = mesh_node(glb, name);
    assert!(
        node.get("skin").is_none(),
        "{name} must use static fallback"
    );
    let primitive = &glb.json["meshes"][node["mesh"].as_u64().unwrap() as usize]["primitives"][0];
    assert!(primitive["attributes"].get("WEIGHTS_0").is_none());
    assert!(primitive["attributes"].get("JOINTS_0").is_none());
    let positions = floats::<3>(glb, primitive["attributes"]["POSITION"].as_u64().unwrap());
    assert_eq!(positions.len(), 4);
    assert_eq!(
        glb.json["accessors"][primitive["indices"].as_u64().unwrap() as usize]["count"],
        6
    );
    // The ordinary static path writes the NIF root block with an identity transform, because a
    // placed reference replaces it (nif-to-gltf.md, "Root transform"). The fixture still authors
    // (11, -7, 5) at scale 2 on block 0, so these world positions also prove the recovery path
    // follows that rule instead of reviving it for rigid shapes.
    let world = world_matrix(glb, index);
    let actual = world.transform_point3(Vec3::from_array(positions[0]));
    assert!(
        (actual - Vec3::new(3.0, -2.0, -4.0)).length() < 1e-4,
        "lost shape translation: {actual:?}"
    );
    let second = world.transform_point3(Vec3::from_array(positions[1]));
    assert!(
        (second - Vec3::new(4.5, -2.0, -4.0)).length() < 1e-4,
        "lost shape scale: {second:?}"
    );
}

/// Verify the unaffected part remains attached to a real skin and keeps its vertex influences.
fn assert_healthy(glb: &Glb) {
    let (_, node) = mesh_node(glb, "Healthy");
    assert!(node["skin"].as_u64().is_some(), "healthy shape lost skin");
    assert!(
        node["skin"].as_u64().unwrap() < glb.json["skins"].as_array().unwrap().len() as u64,
        "mixed mesh must reference an existing skin independently of its mesh index"
    );
    let primitive = &glb.json["meshes"][node["mesh"].as_u64().unwrap() as usize]["primitives"][0];
    let weights = floats::<4>(glb, primitive["attributes"]["WEIGHTS_0"].as_u64().unwrap());
    assert_eq!(weights.len(), 4);
    for row in weights {
        assert!((row.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        let mut positive: Vec<_> = row.into_iter().filter(|weight| *weight > 0.0).collect();
        positive.sort_by(f32::total_cmp);
        assert_eq!(positive.len(), 2);
        // Unequal influences are exactly representable in the fixture's binary16 buffer.
        assert!((positive[0] - 0.25).abs() < 1e-7 && (positive[1] - 0.75).abs() < 1e-7);
    }
}

/// Convert through the public self-contained-rig API and make failures assertion reds.
fn convert_fixture(bytes: &[u8]) -> Glb {
    let directory = tempfile::tempdir().unwrap();
    let nif = directory.path().join("fixture.nif");
    let glb = directory.path().join("fixture.glb");
    fs::write(&nif, bytes).unwrap();
    let result = MeshConverter::convert_nif_to_glb_with_skeleton(&nif, &glb, None);
    assert!(result.is_ok(), "usable geometry must convert: {result:?}");
    read_glb(&glb)
}

/// Zero total weight affects only that part; its parent and local transforms survive.
#[test]
fn zero_total_weight_preserves_static_shape_and_healthy_skin() {
    let mut weights = GOOD;
    weights[0] = [0.0; 4];
    let glb = convert_fixture(&fixture(&weights, false));
    assert_rigid(&glb, "Affected");
    assert_healthy(&glb);
}

/// Main's normal generation must also reach recovered ordinary and dynamic rigid shapes.
#[test]
fn model_space_normals_survive_rigid_skin_fallback() {
    let mut weights = GOOD;
    weights[0] = [0.0; 4];
    for dynamic in [false, true] {
        let shape = skin("NormalFallback", &weights);
        let bytes = if dynamic {
            legacy_dynamic_skinned_shape_with_normals(&shape, SkinNormals::ModelSpaceMap)
        } else {
            legacy_skinned_shape_with_normals(&shape, SkinNormals::ModelSpaceMap)
        }
        .unwrap();
        let glb = convert_fixture(&bytes);
        let (_, node) = mesh_node(&glb, "NormalFallback");
        assert!(node.get("skin").is_none(), "bad skin must become rigid");
        let primitive =
            &glb.json["meshes"][node["mesh"].as_u64().unwrap() as usize]["primitives"][0];
        let attributes = &primitive["attributes"];
        let positions = floats::<3>(&glb, attributes["POSITION"].as_u64().unwrap());
        let z = if dynamic { 7.0 } else { 0.0 };
        assert_eq!(positions.len(), 4);
        assert_eq!(positions[0], [0.0, 0.0, z]);
        assert_eq!(positions[1], [1.0, 0.0, z]);
        assert_eq!(
            glb.json["accessors"][primitive["indices"].as_u64().unwrap() as usize]["count"],
            6
        );
        let normal = attributes["NORMAL"]
            .as_u64()
            .expect("rigid model-space shape must receive generated normals");
        let normals = floats::<3>(&glb, normal);
        assert_eq!(normals.len(), positions.len());
        for normal in normals {
            assert_eq!(normal, [0.0, 0.0, 1.0], "dynamic={dynamic}");
        }
    }
}

/// Retail-sized negative rounding below the documented 1e-5 tolerance is clamped and normalized.
#[test]
fn tiny_negative_weight_is_clamped_and_renormalized() {
    let mut weights = GOOD;
    weights[0] = [-0.000_003_814_697_3, 0.25, 0.5, 0.0];
    let glb = convert_fixture(&fixture(&weights, false));
    let (_, node) = mesh_node(&glb, "Affected");
    assert!(
        node["skin"].as_u64().is_some(),
        "rounding must preserve skin"
    );
    let primitive = &glb.json["meshes"][node["mesh"].as_u64().unwrap() as usize]["primitives"][0];
    let rows = floats::<4>(&glb, primitive["attributes"]["WEIGHTS_0"].as_u64().unwrap());
    assert_eq!(rows.len(), 4);
    for row in &rows {
        assert!(row.iter().all(|v| v.is_finite() && *v >= 0.0));
        assert!((row.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    }
    let mut repaired = rows[0];
    repaired.sort_by(f32::total_cmp);
    for (actual, expected) in repaired.into_iter().zip([0.0, 0.0, 1.0 / 3.0, 2.0 / 3.0]) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "clamp before normalization: {repaired:?}"
        );
    }
    assert_healthy(&glb);
}

/// A negative influence above the tolerance preserves geometry as a rigid part.
#[test]
fn negative_weight_beyond_tolerance_preserves_static_geometry() {
    let mut weights = GOOD;
    weights[0] = [-0.000_02, 0.25, 0.5, 0.0];
    let glb = convert_fixture(&fixture(&weights, false));
    assert_rigid(&glb, "Affected");
    assert_healthy(&glb);
}

/// A shape with no skin reference stays visible beside a valid skinned shape.
#[test]
fn mixed_skinned_and_unskinned_parts_keep_geometry_and_transforms() {
    let glb = convert_fixture(&fixture(&GOOD, true));
    assert_rigid(&glb, "Affected");
    assert_healthy(&glb);
    assert_eq!(
        glb.json["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["mesh"].is_u64())
            .count(),
        2
    );
}

/// Generate an independent valid static neighbor or single-node skeleton body.
fn static_fixture(name: &str) -> Vec<u8> {
    static_shape(&StaticShape {
        name,
        positions: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        normals: &[[0.0, 0.0, 1.0]; 3],
        uvs: &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        indices: &[[0, 1, 2]],
        diffuse: "textures/generated_color.dds",
        normal_texture: "textures/generated_normal.dds",
    })
    .unwrap()
}

/// Install a valid skeleton covering only Bone0 in a different actor's directory.
fn partial_skeleton(data: &Path) {
    let path = data.join("meshes/actors/other/skeleton.nif");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = static_fixture("Bone0");
    let layout = blocks(&bytes);
    let root = layout[0].1;
    bytes[root..root + 4].copy_from_slice(&0u32.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

/// Real skeleton discovery reports partial coverage, yet the standalone API retains both shapes.
#[test]
fn unresolved_skeleton_discovery_retains_geometry() {
    let directory = tempfile::tempdir().unwrap();
    partial_skeleton(directory.path());
    let nif = directory.path().join("meshes/actors/test/body.nif");
    fs::create_dir_all(nif.parent().unwrap()).unwrap();
    fs::write(&nif, fixture(&GOOD, false)).unwrap();
    let resolution = find_skeleton(&nif);
    assert!(
        matches!(resolution, SkeletonResolution::Unresolved { best_coverage } if (best_coverage - 0.5).abs() < 1e-6),
        "fixture must reach the unresolved resolver state: {resolution:?}"
    );
    let output = directory.path().join("body.glb");
    let result = MeshConverter::convert_nif_to_glb(&nif, &output);
    assert!(
        result.is_ok(),
        "unresolved skin must retain geometry: {result:?}"
    );
    let glb = read_glb(&output);
    assert_rigid(&glb, "Affected");
    let (_, healthy) = mesh_node(&glb, "Healthy");
    assert!(
        healthy.get("skin").is_none(),
        "unresolved rig must be rigid"
    );
}

/// Model the parsed Bethesda skin variants without claiming Fallout 4 parser coverage.
fn bethesda_skin_fixture(dynamic: bool, mixed: bool) -> NifFile {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bethesda.nif");
    let bytes = if mixed {
        fixture(&GOOD, false)
    } else {
        legacy_skinned_shapes(&[skin("Affected", &GOOD)]).unwrap()
    };
    fs::write(&path, bytes).unwrap();
    let mut nif = NifFile::open(path.to_str().unwrap()).unwrap();
    let NifBlock::BSFadeNode(root) = &mut nif.blocks[0] else {
        panic!("fixture must have a scene root");
    };
    // Reuse the non-joint shape name: strict skeleton loading requires a named root.
    root.av.object.name = 0;
    let (index, instance) = nif
        .blocks
        .iter()
        .enumerate()
        .find_map(|(index, block)| match block {
            NifBlock::NiSkinInstance(instance) => Some((index, instance.clone())),
            _ => None,
        })
        .unwrap();
    let NifBlock::NiSkinData(data) = &nif.blocks[instance.data as usize] else {
        panic!("fixture must have legacy skin data");
    };
    let bone_list = data
        .bones
        .iter()
        .map(|bone| BSSkinBoneTrans {
            bounding_sphere: bone.bounding_sphere,
            rotation: bone.skin_transform.rotation,
            translation: bone.skin_transform.translation,
            scale: bone.skin_transform.scale,
        })
        .collect();
    nif.blocks[instance.data as usize] = NifBlock::BSSkinBoneData(BSSkinBoneData { bone_list });
    nif.blocks[index] = NifBlock::BSSkinInstance(BSSkinInstance {
        skeleton_root: instance.skeleton_root,
        data: instance.data,
        bones: instance.bones,
        scales: Vec::new(),
    });
    if dynamic {
        let block = nif
            .blocks
            .iter_mut()
            .find(|block| matches!(block, NifBlock::BSTriShape(_)))
            .unwrap();
        let NifBlock::BSTriShape(shape) = block else {
            unreachable!();
        };
        let dynamic_vertices = shape
            .vertex_data
            .iter()
            .map(|vertex| vertex.position.unwrap().extend(1.0))
            .collect();
        *block = NifBlock::BSDynamicTriShape(BSDynamicTriShape {
            dynamic_data_size: u32::from(shape.num_vertices) * 16,
            dynamic_vertices,
            bs_tri_shape: shape.clone(),
        });
    }
    nif
}

/// Unresolved Bethesda and legacy neighbours must both retain geometry with one cause each.
#[test]
fn unresolved_bethesda_skins_recover_rigid_geometry_and_diagnostics() {
    for dynamic in [false, true] {
        let nif = bethesda_skin_fixture(dynamic, true);
        let reason = "unresolved Bethesda test rig";
        let (model, diagnostics) =
            nif_to_model_with_skin_fallback(&nif, None, Some(reason)).unwrap();
        assert!(model.skeletal_meshes.is_empty());
        assert_eq!(model.static_meshes.len(), 2);
        assert_eq!(diagnostics.len(), 2);
        for block in [3, 9] {
            assert_eq!(
                diagnostics
                    .iter()
                    .filter(|(index, cause)| *index == block && cause.contains(reason))
                    .count(),
                1
            );
        }
        model.validate().unwrap();
        let (json, bin) = model.to_gltf("unresolved-bethesda".into()).unwrap();
        let glb = Glb {
            json: serde_json::to_value(json).unwrap(),
            bin,
        };
        for name in ["Affected", "Healthy"] {
            let (_, node) = mesh_node(&glb, name);
            assert!(node.get("skin").is_none());
            let primitive =
                &glb.json["meshes"][node["mesh"].as_u64().unwrap() as usize]["primitives"][0];
            let attributes = &primitive["attributes"];
            assert!(attributes.get("JOINTS_0").is_none());
            assert!(attributes.get("WEIGHTS_0").is_none());
            for channel in ["POSITION", "NORMAL", "TEXCOORD_0"] {
                assert_eq!(
                    glb.json["accessors"][attributes[channel].as_u64().unwrap() as usize]["count"],
                    4
                );
            }
            assert_eq!(
                glb.json["accessors"][primitive["indices"].as_u64().unwrap() as usize]["count"],
                6
            );
        }
    }
}

/// A resolved Bethesda rig must keep a bound skin through both public APIs.
#[test]
fn resolved_bethesda_skins_keep_the_existing_skeletal_path() {
    for dynamic in [false, true] {
        let nif = bethesda_skin_fixture(dynamic, false);
        let (model, diagnostics) = nif_to_model_with_skin_fallback(&nif, Some(&nif), None).unwrap();
        assert!(diagnostics.is_empty());
        let strict = nif_to_model(&nif, Some(&nif)).unwrap();
        // Per-shape material association belongs to the converter's finishing step.
        for candidate in [&model, &strict] {
            candidate.validate().unwrap();
            assert!(candidate.static_meshes.is_empty());
            assert_eq!(candidate.skeletal_meshes.len(), 1);
            let mesh = &candidate.skeletal_meshes[0];
            assert_eq!(mesh.skin.len(), 2);
            assert_eq!(mesh.weights.len(), 4);
            assert_eq!(mesh.joints.len(), 4);
            for joint in &mesh.skin {
                assert!(mesh.bones.bones.contains_key(joint));
            }
        }
    }
}

/// Publish a fresh synthetic pack without GPU LOD and drain progress while conversion runs.
async fn publish(data: &Path, output: &Path) -> converter::pipeline::PipelineReport {
    let mut config = PipelineConfig::new(data, output);
    config.no_lod = true;
    config.cpu_jobs = 2;
    config.io_jobs = 1;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result = AssetPipeline::run_async(config, sender).await;
    drain.await.unwrap();
    assert!(
        result.is_ok(),
        "skin problem must not abort publication: {result:?}"
    );
    result.unwrap()
}

/// Bad skin and unresolved coverage publish beside byte-identical good meshes in a complete pack.
#[tokio::test]
async fn bad_skin_neighbors_publish_complete_and_good_meshes_are_byte_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,pex,nif,esm").unwrap(),
    )
    .unwrap();
    let good = data.join("meshes/healthy.nif");
    fs::write(&good, static_fixture("HealthyNeighbor")).unwrap();
    let baseline = directory.path().join("baseline");
    let baseline_report = publish(&data, &baseline).await;
    assert!(baseline_report.complete, "control: {baseline_report:?}");
    let neighbors: Vec<(PathBuf, Vec<u8>)> = ["meshes/generated.glb", "meshes/healthy.glb"]
        .into_iter()
        .map(|key| (PathBuf::from(key), fs::read(baseline.join(key)).unwrap()))
        .collect();
    let mut weights = GOOD;
    weights[0] = [0.0; 4];
    fs::write(data.join("meshes/bad.nif"), fixture(&weights, false)).unwrap();
    let output = directory.path().join("with_bad");
    let report = publish(&data, &output).await;
    assert!(
        report.complete,
        "usable skin failures must not make publication incomplete: {report:?}"
    );
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("conversion-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["complete"], true);
    let bad = read_glb(&output.join("meshes/bad.glb"));
    assert_rigid(&bad, "Affected");
    assert_healthy(&bad);
    for (relative, before) in &neighbors {
        assert_eq!(
            fs::read(output.join(relative)).unwrap(),
            *before,
            "neighbor changed: {}",
            relative.display()
        );
    }
    // Adding the remote partial skeleton deliberately changes resolution for skinned inputs.
    // Static neighbors still have exactly the same input and must remain byte-identical.
    partial_skeleton(&data);
    let unresolved = data.join("meshes/actors/test/body.nif");
    fs::create_dir_all(unresolved.parent().unwrap()).unwrap();
    fs::write(unresolved, fixture(&GOOD, false)).unwrap();
    let output = directory.path().join("with_unresolved");
    let report = publish(&data, &output).await;
    assert!(
        report.complete,
        "unresolved coverage must publish: {report:?}"
    );
    let unresolved = read_glb(&output.join("meshes/actors/test/body.glb"));
    assert_rigid(&unresolved, "Affected");
    let (_, sibling) = mesh_node(&unresolved, "Healthy");
    assert!(sibling.get("skin").is_none());
    for (relative, before) in neighbors {
        assert_eq!(
            fs::read(output.join(&relative)).unwrap(),
            before,
            "neighbor changed: {}",
            relative.display()
        );
    }
}

/// Recreates #300's omitted rigid part while keeping valid published materials and buffers.
fn omit_rigid_geometry(path: &Path) -> Vec<u8> {
    let mut glb = read_glb(path);
    let (_, healthy) = mesh_node(&glb, "Healthy");
    let keep = healthy["mesh"].as_u64().unwrap();
    let mesh = glb.json["meshes"][keep as usize].clone();
    glb.json["meshes"] = serde_json::json!([mesh]);
    for node in glb.json["nodes"].as_array_mut().unwrap() {
        if node["mesh"].as_u64() == Some(keep) {
            node["mesh"] = serde_json::json!(0);
        } else if node.get("mesh").is_some() {
            node.as_object_mut().unwrap().remove("mesh");
            node.as_object_mut().unwrap().remove("skin");
        }
    }
    let mut json = serde_json::to_vec(&glb.json).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bytes = b"glTF".to_vec();
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&((28 + json.len() + glb.bin.len()) as u32).to_le_bytes());
    bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"JSON");
    bytes.extend_from_slice(&json);
    bytes.extend_from_slice(&(glb.bin.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"BIN\0");
    bytes.extend_from_slice(&glb.bin);
    bytes
}

/// Both main revisions and the unpublished fallback proof must invalidate incomplete mixed output.
#[tokio::test]
async fn old_skin_output_is_rejected_for_retention_and_rebuilt_from_warm_cache() {
    for contracts in [
        &["legacy-skin-export"][..],
        &[
            "legacy-skin-export",
            "msn-vertex-normals",
            "facegen-tint-slot",
        ][..],
        &["legacy-skin-export", "skin-static-fallback"][..],
        &[
            "legacy-skin-export",
            "msn-vertex-normals",
            "facegen-tint-slot",
            "lighting-type-data",
            "msn-specular-default",
            "hidden-niavobject-excluded",
            "nif-root-transform-ignored",
        ][..],
    ] {
        assert_stale_mixed_output_rebuilds(contracts).await;
    }
}

/// Seed an independently pinned prior contract and verify retention rejection and warm rebuild.
async fn assert_stale_mixed_output_rebuilds(contracts: &[&str]) {
    use converter::cache::{ConversionManifest, hash_bytes};
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,pex,nif,esm").unwrap(),
    )
    .unwrap();
    let source = data.join("meshes/mixed.nif");
    fs::write(&source, fixture(&GOOD, true)).unwrap();
    let output = directory.path().join("warm");
    assert!(publish(&data, &output).await.complete);

    // Pin the actual old fixed-producer settings payload; do not ask the new helper for it.
    let old_proof = hash_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "mesh_producer": 28, "mesh_contracts": contracts
        }))
        .unwrap(),
    );
    let old_glb = omit_rigid_geometry(&output.join("meshes/mixed.glb"));
    fs::write(output.join("meshes/mixed.glb"), &old_glb).unwrap();
    let manifest_path = output.join("conversion-manifest.json");
    let mut manifest: ConversionManifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let entry = manifest.entries.get_mut("meshes/mixed.nif").unwrap();
    entry.output_size = old_glb.len() as u64;
    entry.output_hash = hash_bytes(&old_glb);
    manifest
        .asset_configuration_hashes
        .insert("nif".into(), old_proof);
    manifest.save(&manifest_path).unwrap();
    assert_eq!(
        read_glb(&output.join("meshes/mixed.glb")).json["meshes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let retained = directory.path().join("retained");
    let mut config = PipelineConfig::new(&data, &retained);
    config.no_lod = true;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let result = AssetPipeline::rebuild_metadata_async(config, &output, sender).await;
    drain.await.unwrap();
    assert!(
        result.is_err(),
        "old mesh output contract must not survive metadata retention"
    );
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.contains("mesh output contract"),
        "unexpected rejection: {error}"
    );
    assert!(!retained.exists());

    let rebuilt = publish(&data, &output).await;
    assert!(rebuilt.complete);
    assert!(
        rebuilt.converted >= 2,
        "all stale mesh proofs must rebuild: {rebuilt:?}"
    );
    let glb = read_glb(&output.join("meshes/mixed.glb"));
    assert_rigid(&glb, "Affected");
    assert_healthy(&glb);
    assert_eq!(publish(&data, &output).await.converted, 0);
}

/// Inspect partition-only channels without deriving expected values from exporter helpers.
fn assert_dynamic_channels(path: &Path) {
    let glb = read_glb(path);
    let (_, node) = mesh_node(&glb, "Affected");
    assert!(node.get("skin").is_none());
    let primitive = &glb.json["meshes"][node["mesh"].as_u64().unwrap() as usize]["primitives"][0];
    let attributes = &primitive["attributes"];
    let positions = floats::<3>(&glb, attributes["POSITION"].as_u64().unwrap());
    assert_eq!(positions.len(), 4);
    assert_eq!(positions[0], [0.0, 0.0, 7.0]);
    assert_eq!(positions[1], [1.0, 0.0, 7.0]);
    let normals = floats::<3>(&glb, attributes["NORMAL"].as_u64().unwrap());
    assert_eq!(normals.len(), 4);
    for normal in normals {
        // Zero normal components are packed as UNORM8 128, decoded to 1/255.
        assert!((normal[0] - 1.0 / 255.0).abs() < 1e-7);
        assert!((normal[1] - 1.0 / 255.0).abs() < 1e-7);
        assert_eq!(normal[2], 1.0);
    }
    let uvs = floats::<2>(&glb, attributes["TEXCOORD_0"].as_u64().unwrap());
    assert_eq!(uvs.len(), 4);
    assert_eq!(uvs[0], [0.5, 0.5]);
    assert_eq!(uvs[1], [1.0, 0.5]);
    assert_eq!(
        glb.json["accessors"][primitive["indices"].as_u64().unwrap() as usize]["count"],
        6
    );
}

/// Dynamic positions must survive zero-weight fallback with dismember partition attributes.
#[test]
fn dynamic_zero_weight_retains_live_positions_normals_uvs_and_triangles() {
    let directory = tempfile::tempdir().unwrap();
    let mut weights = GOOD;
    weights[0] = [0.0; 4];
    let mut spec = skin("Affected", &weights);
    spec.instance_kind = SkinInstanceKind::Dismember;
    let source = directory.path().join("dynamic.nif");
    fs::write(
        &source,
        dummy_content::nif::legacy_dynamic_skinned_shape(&spec).unwrap(),
    )
    .unwrap();
    let output = directory.path().join("dynamic.glb");
    let result = MeshConverter::convert_nif_to_glb(&source, &output);
    assert!(result.is_ok(), "dynamic geometry must survive: {result:?}");
    assert_dynamic_channels(&output);
}

/// Unresolved dynamic rigs use the same complete per-channel static recovery.
#[test]
fn dynamic_unresolved_retains_live_positions_normals_uvs_and_triangles() {
    let directory = tempfile::tempdir().unwrap();
    partial_skeleton(directory.path());
    let source = directory.path().join("meshes/actors/test/dynamic.nif");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let mut spec = skin("Affected", &GOOD);
    spec.instance_kind = SkinInstanceKind::Dismember;
    fs::write(
        &source,
        dummy_content::nif::legacy_dynamic_skinned_shape(&spec).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        find_skeleton(&source),
        SkeletonResolution::Unresolved { .. }
    ));
    let output = directory.path().join("dynamic.glb");
    let result = MeshConverter::convert_nif_to_glb(&source, &output);
    assert!(
        result.is_ok(),
        "dynamic unresolved geometry must survive: {result:?}"
    );
    assert_dynamic_channels(&output);
}

/// Ordinary CLI runs must print one bounded fallback warning even without a log subscriber.
#[test]
fn cli_reports_one_bounded_fallback_warning_per_shape() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("Data");
    layout::prepare_directory(&data, false).unwrap();
    layout::generate(
        &data,
        layout::DEFAULT_SEED,
        layout::Formats::parse("dds,pex,nif,esm").unwrap(),
    )
    .unwrap();
    fs::write(data.join("meshes/bad.nif"), fixture(&[[0.0; 4]; 4], false)).unwrap();
    let output = directory.path().join("modern");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_converter"))
        .arg(&data)
        .arg(&output)
        .args(["--no-lod", "--cpu-jobs", "2", "--io-jobs", "1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "{stderr}");
    assert_eq!(
        stderr.matches("static skin fallback:").count(),
        1,
        "the CLI must print a single shape warning, not one warning per vertex: {stderr}"
    );
    let warning = stderr
        .lines()
        .find(|line| line.contains("static skin fallback:"))
        .unwrap();
    assert!(
        warning.contains("bad.nif")
            && warning.contains("shape block 3")
            && warning.contains("Affected")
    );
    assert!(warning.contains("skin weight sum of 0"));
    assert!(
        warning
            .split_once(": static skin fallback:")
            .unwrap()
            .1
            .chars()
            .count()
            <= 512
    );
    let glb = read_glb(&output.join("meshes/bad.glb"));
    assert_rigid(&glb, "Affected");
    assert_healthy(&glb);
}
