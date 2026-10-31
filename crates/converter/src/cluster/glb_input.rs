//! Reads a converted GLB into [`ModelPrimitive`]s for classification and building.
//!
//! Node transforms (and the scene root's Creation-to-runtime rotation, which the converter writes
//! as a node) are baked into the vertex data, so a primitive's positions are in runtime (Y-up)
//! axes and a reference's TRS applies directly. A mirrored node flips the triangle winding.
//! A skin in a model that never moves its joints (no animations, no morph targets) is posed at
//! rest into the same model space, as the stock renderer draws it.

use super::model::{AlphaMode, ModelPrimitive, PrimitiveGeometry, Reject, RejectReason};
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use gltf::{Gltf, Node, Semantic, mesh::Mode};
use serde_json::Value;
use shared::cluster_material::{stock_requires_sorted_depth, unsupported_feature};
use shared::cluster_recipe::model_facts::{
    material_key, model_rests, parse_glb_json, raw_material,
};
use std::collections::HashMap;

type Matrix = [[f32; 4]; 4];

const IDENTITY: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Loads every (node, primitive) pair of the default scene. `glb_path` is the GLB's path inside
/// the output (`meshes/.../x.glb`), used to resolve texture references into asset keys.
pub fn load_model(bytes: &[u8], glb_path: &str) -> Result<Vec<ModelPrimitive>> {
    let Gltf { document, blob } = Gltf::from_slice(bytes).wrap_err("not a valid glTF")?;
    let buffers: Vec<Vec<u8>> = document
        .buffers()
        .map(|buffer| match buffer.source() {
            gltf::buffer::Source::Bin => {
                blob.clone().ok_or_else(|| eyre!("GLB has no binary chunk"))
            }
            gltf::buffer::Source::Uri(uri) => Err(eyre!("external buffer {uri} is not supported")),
        })
        .collect::<Result<_>>()?;
    // Materials are read as raw JSON (not a typed, re-serialized model) so the key is the same in
    // every build, whichever crate features are on.
    let raw = parse_glb_json(bytes)?;
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or_else(|| eyre!("GLB has no scene"))?;

    let rests = model_rests(
        document.skins().next().is_some(),
        document.meshes().any(|mesh| {
            mesh.primitives()
                .any(|primitive| primitive.morph_targets().next().is_some())
        }),
        document.animations().next().is_some(),
    );
    let node_worlds = scene_node_worlds(&scene);

    let mut primitives = Vec::new();
    let mut stack: Vec<(Node, Matrix)> = scene.nodes().map(|node| (node, IDENTITY)).collect();
    stack.reverse();
    while let Some((node, parent)) = stack.pop() {
        let world = multiply(&parent, &node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            let skinning = match node.skin() {
                Some(skin) if rests => skin_pose(&skin, &world, &node_worlds, &buffers)
                    .map_or_else(|detail| Skinning::Refused(detail.into()), Skinning::Rest),
                Some(_) => Skinning::Refused("skin in a document with animations".into()),
                None => Skinning::None,
            };
            for primitive in mesh.primitives() {
                let index = primitives.len() as u32;
                let material = raw_material(&raw, primitive.material().index())?;
                let material_key = material_key(&material, &raw, glb_path)?;
                primitives.push(read_primitive(
                    index,
                    &primitive,
                    &skinning,
                    &world,
                    &buffers,
                    &material,
                    material_key,
                ));
            }
        }
        let mut children: Vec<_> = node.children().map(|child| (child, world)).collect();
        children.reverse();
        stack.extend(children);
    }
    Ok(primitives)
}

/// How a mesh node's skin, if any, reaches the cluster.
enum Skinning {
    None,
    /// One matrix per joint, `joint world * inverse bind`, in the scene's model space.
    Rest(Vec<Matrix>),
    Refused(String),
}

/// World matrix of every node reachable from the scene roots, by node index.
fn scene_node_worlds(scene: &gltf::Scene<'_>) -> HashMap<usize, Matrix> {
    let mut worlds = HashMap::new();
    let mut stack: Vec<(Node, Matrix)> = scene.nodes().map(|node| (node, IDENTITY)).collect();
    while let Some((node, parent)) = stack.pop() {
        let world = multiply(&parent, &node.transform().matrix());
        // A node reached twice is not a tree; leave it out so a joint on it is refused.
        if worlds.insert(node.index(), world).is_some() {
            worlds.insert(node.index(), [[f32::NAN; 4]; 4]);
            continue;
        }
        stack.extend(node.children().map(|child| (child, world)));
    }
    worlds
}

/// The rest-pose joint matrices of a skin, as the stock renderer computes them from the loaded
/// scene: each joint's world matrix times its inverse bind matrix (identity when absent). The
/// skinned node's own transform does not place its vertices; a mirrored one is refused because
/// its determinant decides the stock path's face winding.
fn skin_pose(
    skin: &gltf::Skin<'_>,
    node_world: &Matrix,
    node_worlds: &HashMap<usize, Matrix>,
    buffers: &[Vec<u8>],
) -> Result<Vec<Matrix>, &'static str> {
    if determinant3(node_world) <= 0.0 {
        return Err("skinned node transform is mirrored or degenerate");
    }
    let reader = skin.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));
    let inverse_binds: Option<Vec<Matrix>> =
        reader.read_inverse_bind_matrices().map(Iterator::collect);
    let joints: Vec<Node> = skin.joints().collect();
    if inverse_binds
        .as_ref()
        .is_some_and(|binds| binds.len() != joints.len())
    {
        return Err("inverse bind matrix count differs from the joint count");
    }
    joints
        .iter()
        .enumerate()
        .map(|(slot, joint)| {
            let world = node_worlds
                .get(&joint.index())
                .ok_or("joint is not in the scene")?;
            let bind = inverse_binds.as_ref().map_or(IDENTITY, |binds| binds[slot]);
            let matrix = multiply(world, &bind);
            if !matrix.iter().flatten().all(|c| c.is_finite()) {
                return Err("joint matrix is not finite");
            }
            Ok(matrix)
        })
        .collect()
}

fn read_primitive(
    index: u32,
    primitive: &gltf::Primitive<'_>,
    skinning: &Skinning,
    world: &Matrix,
    buffers: &[Vec<u8>],
    material: &Value,
    material_key: String,
) -> ModelPrimitive {
    let material_index = primitive.material().index().map(|slot| slot as u32);
    let alpha = match material.get("alphaMode").and_then(Value::as_str) {
        Some("MASK") => AlphaMode::Mask,
        Some("BLEND") => AlphaMode::Blend,
        _ => AlphaMode::Opaque,
    };
    let alpha_cutoff = (alpha == AlphaMode::Mask).then(|| {
        material
            .get("alphaCutoff")
            .and_then(Value::as_f64)
            .map_or(0.5, |cutoff| cutoff as f32)
    });
    let double_sided = material
        .get("doubleSided")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let open_skyrim = material.pointer("/extras/openSkyrim");
    let family = open_skyrim
        .and_then(|value| value.get("shaderFamily"))
        .and_then(Value::as_str);
    let non_rendering = open_skyrim
        .and_then(|value| value.get("nonRenderingExclusion"))
        .is_some_and(|value| value.as_bool() != Some(false))
        || primitive_has_exclusion(primitive);
    let effect = family == Some("effect");

    let mut geometry = PrimitiveGeometry::default();
    let mut structural = None;
    let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));
    let positions = reader
        .read_positions()
        .map(Iterator::collect::<Vec<[f32; 3]>>);
    let normals = reader
        .read_normals()
        .map(Iterator::collect::<Vec<[f32; 3]>>);
    let uvs = reader
        .read_tex_coords(0)
        .map(|uvs| uvs.into_f32().collect::<Vec<[f32; 2]>>());
    let indices = reader
        .read_indices()
        .map(|indices| indices.into_u32().collect::<Vec<u32>>());
    if primitive.mode() != Mode::Triangles || indices.is_none() {
        structural = Some(Reject::new(
            RejectReason::NotIndexedTriangles,
            format!("mode {:?}, indexed {}", primitive.mode(), indices.is_some()),
        ));
    }
    match (positions, normals, uvs, indices) {
        (Some(positions), Some(normals), Some(uvs), Some(indices))
            if positions.len() == normals.len() && positions.len() == uvs.len() =>
        {
            geometry.positions = positions;
            geometry.normals = normals;
            geometry.uvs = uvs;
            geometry.indices = indices;
            geometry.tangents = reader.read_tangents().map(Iterator::collect);
            geometry.colours = reader
                .read_colors(0)
                .map(|colours| colours.into_rgba_f32().collect());
            let vertex_count = geometry.positions.len();
            let ragged = geometry
                .tangents
                .as_ref()
                .is_some_and(|tangents| tangents.len() != vertex_count)
                || geometry
                    .colours
                    .as_ref()
                    .is_some_and(|colours| colours.len() != vertex_count);
            if ragged {
                structural = Some(Reject::new(
                    RejectReason::MissingAttributes,
                    "TANGENT or COLOR_0 length differs from POSITION",
                ));
            }
        }
        _ if structural.is_none() => {
            structural = Some(Reject::new(
                RejectReason::MissingAttributes,
                "needs POSITION, NORMAL, TEXCOORD_0 of equal length",
            ));
        }
        _ => {}
    }
    if structural.is_none() {
        let vertex_count = geometry.positions.len();
        let finite = geometry
            .positions
            .iter()
            .chain(&geometry.normals)
            .all(|value| value.iter().all(|c| c.is_finite()))
            && geometry
                .uvs
                .iter()
                .all(|value| value.iter().all(|c| c.is_finite()));
        if geometry.indices.len() % 3 != 0
            || geometry.indices.iter().any(|&i| i as usize >= vertex_count)
        {
            structural = Some(Reject::new(
                RejectReason::NotIndexedTriangles,
                "indices out of range or not a multiple of three",
            ));
        } else if !finite {
            structural = Some(Reject::new(
                RejectReason::NonFiniteGeometry,
                "non-finite position, normal or uv",
            ));
        }
    }
    // A refused skin keeps its geometry out like a structural reject would; the reason stays
    // "skinned" so the report counts it where it always has.
    let mut skinned = None;
    if structural.is_none() {
        match skinning {
            Skinning::None if primitive.get(&Semantic::Joints(0)).is_some() => {
                skinned = Some("JOINTS_0 without a skin".to_owned());
            }
            Skinning::None => {}
            Skinning::Refused(detail) => skinned = Some(detail.clone()),
            Skinning::Rest(joints) => {
                if let Err(detail) = bake_skin(&mut geometry, primitive, joints, buffers) {
                    skinned = Some(detail.to_owned());
                }
            }
        }
    }
    if structural.is_some() || skinned.is_some() {
        geometry = PrimitiveGeometry::default();
    } else if !matches!(skinning, Skinning::Rest(_)) {
        bake_transform(&mut geometry, world);
    }
    let bounds = bounds_of(&geometry.positions);

    let reject = if let Some(reject) = structural.clone() {
        Some(reject)
    } else if alpha == AlphaMode::Blend {
        // Cutout (`Mask`) primitives are clustered: the engine draws them through its alpha-mask
        // path from the material the stock path loads. Only blended ones stay out.
        Some(Reject::new(RejectReason::NotOpaque, format!("{alpha:?}")))
    } else if effect {
        Some(Reject::new(
            RejectReason::EffectShader,
            "shaderFamily effect",
        ))
    } else if let Some(detail) = skinned {
        Some(Reject::new(RejectReason::Skinned, detail))
    } else if non_rendering {
        Some(Reject::new(
            RejectReason::NonRendering,
            "non-rendering stand-in",
        ))
    } else if stock_requires_sorted_depth(material, alpha == AlphaMode::Blend) {
        Some(Reject::new(
            RejectReason::NotOpaque,
            "stock native depth material requires the sorted color pass",
        ))
    } else {
        unsupported_feature(material, family)
            .map(|feature| Reject::new(RejectReason::UnsupportedMaterialFeature, feature))
    };
    ModelPrimitive {
        index,
        geometry,
        alpha,
        alpha_cutoff,
        double_sided,
        material_key,
        material_index,
        bounds,
        reject,
    }
}

fn primitive_has_exclusion(primitive: &gltf::Primitive<'_>) -> bool {
    primitive
        .extras()
        .as_ref()
        .and_then(|extras| serde_json::from_str::<Value>(extras.get()).ok())
        .is_some_and(|extras| extras.pointer("/openSkyrim/materialExclusion").is_some())
}

fn bounds_of(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    if positions.is_empty() {
        return ([0.0; 3], [0.0; 3]);
    }
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for position in positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    (min, max)
}

/// Applies the node's world transform to positions, normals (cofactor matrix) and tangents; a
/// mirrored transform (negative determinant) flips triangle winding and tangent handedness.
fn bake_transform(geometry: &mut PrimitiveGeometry, world: &Matrix) {
    if *world == IDENTITY {
        return;
    }
    let linear = |row: usize, column: usize| world[column][row];
    let cofactor = cofactor_matrix(&linear);
    let determinant = determinant3(world);
    for position in &mut geometry.positions {
        let [x, y, z] = *position;
        *position = [
            linear(0, 0) * x + linear(0, 1) * y + linear(0, 2) * z + world[3][0],
            linear(1, 0) * x + linear(1, 1) * y + linear(1, 2) * z + world[3][1],
            linear(2, 0) * x + linear(2, 1) * y + linear(2, 2) * z + world[3][2],
        ];
    }
    for normal in &mut geometry.normals {
        let transformed = apply3(&cofactor, *normal);
        // The cofactor of a mirror points inward; the true normal follows the determinant sign.
        let outward = if determinant < 0.0 {
            transformed.map(|c| -c)
        } else {
            transformed
        };
        *normal = normalise(outward);
    }
    if let Some(tangents) = &mut geometry.tangents {
        for tangent in tangents {
            let direction = normalise([
                linear(0, 0) * tangent[0] + linear(0, 1) * tangent[1] + linear(0, 2) * tangent[2],
                linear(1, 0) * tangent[0] + linear(1, 1) * tangent[1] + linear(1, 2) * tangent[2],
                linear(2, 0) * tangent[0] + linear(2, 1) * tangent[1] + linear(2, 2) * tangent[2],
            ]);
            let handedness = if determinant < 0.0 {
                -tangent[3]
            } else {
                tangent[3]
            };
            *tangent = [direction[0], direction[1], direction[2], handedness];
        }
    }
    if determinant < 0.0 {
        for triangle in geometry.indices.as_chunks_mut::<3>().0 {
            triangle.swap(1, 2);
        }
    }
}

/// Poses a skinned primitive at rest the way bevy_pbr's skinning shader does: each vertex takes
/// `sum(weight * joint matrix)` from JOINTS_0/WEIGHTS_0 (weights as authored, not renormalised),
/// normals the blended matrix's inverse transpose and tangents its linear part. A blend with a
/// non-positive determinant would need per-vertex winding the cluster cannot carry, so it is
/// refused, as are a second influence set and joint slots past the skin.
fn bake_skin(
    geometry: &mut PrimitiveGeometry,
    primitive: &gltf::Primitive<'_>,
    joints: &[Matrix],
    buffers: &[Vec<u8>],
) -> Result<(), &'static str> {
    if primitive.get(&Semantic::Joints(1)).is_some()
        || primitive.get(&Semantic::Weights(1)).is_some()
    {
        return Err("more than four joint influences");
    }
    let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(Vec::as_slice));
    let indices: Vec<[u16; 4]> = reader
        .read_joints(0)
        .ok_or("skin without JOINTS_0")?
        .into_u16()
        .collect();
    let weights: Vec<[f32; 4]> = reader
        .read_weights(0)
        .ok_or("skin without WEIGHTS_0")?
        .into_f32()
        .collect();
    let vertex_count = geometry.positions.len();
    if indices.len() != vertex_count || weights.len() != vertex_count {
        return Err("JOINTS_0 or WEIGHTS_0 length differs from POSITION");
    }
    for vertex in 0..vertex_count {
        let mut blend = [[0.0f32; 4]; 4];
        for (&joint, &weight) in indices[vertex].iter().zip(&weights[vertex]) {
            if weight == 0.0 {
                continue;
            }
            let matrix = joints
                .get(usize::from(joint))
                .ok_or("joint slot outside the skin")?;
            for (column, source) in blend.iter_mut().zip(matrix) {
                for (out, value) in column.iter_mut().zip(source) {
                    *out += weight * value;
                }
            }
        }
        if determinant3(&blend) <= 0.0 {
            return Err("blended joint matrix is mirrored or degenerate");
        }
        let linear = |row: usize, column: usize| blend[column][row];
        let [x, y, z] = geometry.positions[vertex];
        geometry.positions[vertex] = [
            linear(0, 0) * x + linear(0, 1) * y + linear(0, 2) * z + blend[3][0],
            linear(1, 0) * x + linear(1, 1) * y + linear(1, 2) * z + blend[3][1],
            linear(2, 0) * x + linear(2, 1) * y + linear(2, 2) * z + blend[3][2],
        ];
        // Positive determinant: the cofactor matrix points the same way as the inverse transpose.
        let cofactor = cofactor_matrix(&linear);
        geometry.normals[vertex] = normalise(apply3(&cofactor, geometry.normals[vertex]));
        if let Some(tangent) = geometry
            .tangents
            .as_mut()
            .and_then(|tangents| tangents.get_mut(vertex))
        {
            let direction = normalise([
                linear(0, 0) * tangent[0] + linear(0, 1) * tangent[1] + linear(0, 2) * tangent[2],
                linear(1, 0) * tangent[0] + linear(1, 1) * tangent[1] + linear(1, 2) * tangent[2],
                linear(2, 0) * tangent[0] + linear(2, 1) * tangent[1] + linear(2, 2) * tangent[2],
            ]);
            *tangent = [direction[0], direction[1], direction[2], tangent[3]];
        }
        let finite = geometry.positions[vertex]
            .iter()
            .chain(&geometry.normals[vertex])
            .chain(
                geometry
                    .tangents
                    .as_ref()
                    .and_then(|tangents| tangents.get(vertex))
                    .into_iter()
                    .flatten(),
            )
            .all(|c| c.is_finite());
        if !finite {
            return Err("skinned vertex is not finite");
        }
    }
    Ok(())
}

/// Determinant of a column-major matrix's upper-left 3x3.
fn determinant3(m: &Matrix) -> f32 {
    let linear = |row: usize, column: usize| m[column][row];
    linear(0, 0) * (linear(1, 1) * linear(2, 2) - linear(1, 2) * linear(2, 1))
        - linear(0, 1) * (linear(1, 0) * linear(2, 2) - linear(1, 2) * linear(2, 0))
        + linear(0, 2) * (linear(1, 0) * linear(2, 1) - linear(1, 1) * linear(2, 0))
}

/// Cofactor matrix of a 3x3 matrix: the inverse transpose scaled by the determinant.
fn cofactor_matrix(m: &impl Fn(usize, usize) -> f32) -> [[f32; 3]; 3] {
    [
        [
            m(1, 1) * m(2, 2) - m(1, 2) * m(2, 1),
            m(1, 2) * m(2, 0) - m(1, 0) * m(2, 2),
            m(1, 0) * m(2, 1) - m(1, 1) * m(2, 0),
        ],
        [
            m(0, 2) * m(2, 1) - m(0, 1) * m(2, 2),
            m(0, 0) * m(2, 2) - m(0, 2) * m(2, 0),
            m(0, 1) * m(2, 0) - m(0, 0) * m(2, 1),
        ],
        [
            m(0, 1) * m(1, 2) - m(0, 2) * m(1, 1),
            m(0, 2) * m(1, 0) - m(0, 0) * m(1, 2),
            m(0, 0) * m(1, 1) - m(0, 1) * m(1, 0),
        ],
    ]
}

fn apply3(matrix: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * v[0] + matrix[0][1] * v[1] + matrix[0][2] * v[2],
        matrix[1][0] * v[0] + matrix[1][1] * v[1] + matrix[1][2] * v[2],
        matrix[2][0] * v[0] + matrix[2][1] * v[1] + matrix[2][2] * v[2],
    ]
}

fn normalise(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length > 0.0 && length.is_finite() {
        [v[0] / length, v[1] / length, v[2] / length]
    } else {
        [0.0, 0.0, 1.0]
    }
}

/// Column-major 4x4 product `a * b`.
fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0.0; 4]; 4];
    for column in 0..4 {
        for row in 0..4 {
            out[column][row] = (0..4).map(|k| a[k][row] * b[column][k]).sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::test_support::glb_with_nodes;
    use shared::cluster_recipe::model_facts::model_facts_from_json;
    use std::collections::BTreeSet;

    #[test]
    fn the_primitive_set_is_what_load_model_numbers() {
        // Node 0 carries the two-primitive mesh and has node 2 (the one-primitive mesh) as a child;
        // node 1 is a second root with the two-primitive mesh again. A shared mesh counts once per
        // node that uses it.
        let bytes = glb_with_nodes(
            &[2, 1],
            &[(Some(0), vec![2]), (Some(0), vec![]), (Some(1), vec![])],
            &[0, 1],
            &["../textures/A/Diffuse.ktx2"],
        );
        let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let facts = model_facts_from_json(&bytes[20..20 + json_length], "meshes/x/y.glb").unwrap();
        let loaded = load_model(&bytes, "meshes/x/y.glb").unwrap();
        assert_eq!(
            facts.primitives,
            loaded.iter().map(|p| p.index).collect::<BTreeSet<_>>()
        );
        assert_eq!(facts.primitives.len(), 2 + 1 + 2);
        // The same key, slot and render flags `load_model` reads for each primitive.
        assert_eq!(facts.materials.len(), loaded.len());
        for primitive in &loaded {
            let facts = &facts.materials[&primitive.index];
            assert_eq!(facts.material_key, primitive.material_key);
            assert_eq!(facts.material_index, primitive.material_index);
            assert_eq!(facts.alpha_mask, primitive.alpha == AlphaMode::Mask);
            assert_eq!(facts.alpha_cutoff, primitive.alpha_cutoff);
            assert_eq!(facts.double_sided, primitive.double_sided);
        }
        assert_eq!(
            facts.texture_paths.iter().collect::<Vec<_>>(),
            ["meshes/textures/a/diffuse.ktx2"]
        );
    }

    #[test]
    fn unproven_material_features_remain_refused() {
        use serde_json::json;
        let ordinary = json!({"extras": {"openSkyrim": {
            "shaderFamily": "lighting", "lightingShaderType": "default",
            "normalConvention": "directx"
        }}, "alphaMode": "MASK"});
        assert_eq!(unsupported_feature(&ordinary, Some("lighting")), None);
        for kind in [
            "environment_map",
            "multi_layer_parallax",
            "parallax",
            "skin_tint",
            "glow",
        ] {
            let mut material = ordinary.clone();
            material["extras"]["openSkyrim"]["lightingShaderType"] = json!(kind);
            assert_eq!(
                unsupported_feature(&material, Some("lighting")),
                Some(format!("lightingShaderType {kind}"))
            );
        }
        for (pointer, value, reason) in [
            ("/emissiveTexture", json!({"index": 0}), "emissive texture"),
            ("/emissiveFactor", json!([0.1, 0.0, 0.0]), "emissive factor"),
            (
                "/extras/openSkyrim/normalConvention",
                json!("opengl"),
                "normalConvention opengl",
            ),
        ] {
            let mut material = ordinary.clone();
            if pointer == "/emissiveTexture" {
                material["emissiveTexture"] = value;
            } else if pointer == "/emissiveFactor" {
                material["emissiveFactor"] = value;
            } else {
                *material.pointer_mut(pointer).unwrap() = value;
            }
            assert_eq!(
                unsupported_feature(&material, Some("lighting")),
                Some(reason.into())
            );
        }
        for flag in ["premultipliedAlpha", "screenDoorAlphaFade"] {
            let mut material = ordinary.clone();
            material["extensions"]["OPEN_SKYRIM_material"][flag] = json!(true);
            assert_eq!(
                unsupported_feature(&material, Some("lighting")),
                Some(flag.into())
            );
        }
    }

    /// One triangle skinned to two joints: joint slot 0 is node 1 (world translation (5, 0, 10)),
    /// slot 1 is node 0 (world (0, 0, 10)) with the inverse bind that cancels it. Vertex 0 follows
    /// slot 0, vertex 1 is split half and half, vertex 2 follows slot 1. The skinned node 2 sits at
    /// (100, 0, 0), which skinning ignores. `skin` and `animated` toggle the skin binding and a
    /// one-key translation animation on node 1.
    fn skinned_triangle(skin: bool, animated: bool) -> Vec<u8> {
        skinned_triangle_with(skin, animated, |_| {})
    }

    /// [`skinned_triangle`] with a last edit to its JSON document.
    fn skinned_triangle_with(
        skin: bool,
        animated: bool,
        tweak: impl FnOnce(&mut serde_json::Value),
    ) -> Vec<u8> {
        use serde_json::json;
        let mut binary = Vec::new();
        let mut floats = |values: &[f32]| {
            for value in values {
                binary.extend_from_slice(&value.to_le_bytes());
            }
        };
        floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]); // positions, 0
        floats(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]); // normals, 36
        floats(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]); // uvs, 72
        binary.extend_from_slice(&[0, 0, 1, 0, 2, 0, 0, 0]); // u16 indices + pad, 96
        binary.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0]); // joints, 104
        let mut floats = |values: &[f32]| {
            for value in values {
                binary.extend_from_slice(&value.to_le_bytes());
            }
        };
        floats(&[1.0, 0.0, 0.0, 0.0, 0.5, 0.5, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]); // weights, 116
        let identity: Vec<f32> = IDENTITY.iter().flatten().copied().collect();
        floats(&identity); // inverse bind slot 0, 164
        let mut untranslate = identity.clone();
        untranslate[14] = -10.0;
        floats(&untranslate); // inverse bind slot 1, 228
        floats(&[0.0]); // animation key time, 292
        let view = |offset: usize, length: usize| json!({"buffer": 0, "byteOffset": offset, "byteLength": length});
        let mut mesh_node = json!({"mesh": 0, "translation": [100.0, 0.0, 0.0]});
        if skin {
            mesh_node["skin"] = json!(0);
        }
        let mut document = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": binary.len()}],
            "bufferViews": [view(0, 36), view(36, 36), view(72, 24), view(96, 6), view(104, 12),
                view(116, 48), view(164, 128), view(292, 4)],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                    "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
                {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
                {"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"},
                {"bufferView": 4, "componentType": 5121, "count": 3, "type": "VEC4"},
                {"bufferView": 5, "componentType": 5126, "count": 3, "type": "VEC4"},
                {"bufferView": 6, "componentType": 5126, "count": 2, "type": "MAT4"},
                {"bufferView": 7, "componentType": 5126, "count": 1, "type": "SCALAR",
                    "min": [0.0], "max": [0.0]},
                {"bufferView": 0, "componentType": 5126, "count": 1, "type": "VEC3"}
            ],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1,
                "TEXCOORD_0": 2, "JOINTS_0": 4, "WEIGHTS_0": 5}, "indices": 3, "material": 0}]}],
            "materials": [{"extras": {"openSkyrim": {"shaderFamily": "lighting",
                "lightingShaderType": "default", "normalConvention": "directx"}}}],
            "nodes": [
                {"translation": [0.0, 0.0, 10.0], "children": [1, 2]},
                {"translation": [5.0, 0.0, 0.0]},
                mesh_node
            ],
            "skins": [{"joints": [1, 0], "inverseBindMatrices": 6}],
            "scene": 0,
            "scenes": [{"nodes": [0]}]
        });
        if animated {
            document["animations"] = json!([{
                "channels": [{"sampler": 0, "target": {"node": 1, "path": "translation"}}],
                "samplers": [{"input": 7, "output": 8}]
            }]);
        }
        tweak(&mut document);
        crate::cluster::test_support::glb(&document, &binary)
    }

    fn skin_reject(bytes: &[u8]) -> Option<Reject> {
        load_model(bytes, "meshes/x/tree.glb").unwrap()[0]
            .reject
            .clone()
    }

    #[test]
    fn a_rotated_joint_turns_positions_and_normals_with_it() {
        use serde_json::json;
        // Joint slot 0 also turns 90 degrees about Y: +Z normals face +X, (1, 0, 0) goes to (0, 0, -1).
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let bytes = skinned_triangle_with(true, false, |document| {
            document["nodes"][1]["rotation"] = json!([0.0, half, 0.0, half]);
        });
        let loaded = load_model(&bytes, "meshes/x/tree.glb").unwrap();
        assert_eq!(loaded[0].reject, None);
        let geometry = &loaded[0].geometry;
        let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|axis| (a[axis] - b[axis]).abs() < 1e-5);
        assert!(close(geometry.positions[0], [5.0, 0.0, 10.0]));
        assert!(
            close(geometry.normals[0], [1.0, 0.0, 0.0]),
            "{:?}",
            geometry.normals[0]
        );
        // Half joint 0, half identity: the blended linear part is not a rotation, and the
        // normal follows its inverse transpose, renormalised.
        assert!(
            close(geometry.positions[1], [3.0, 0.0, 4.5]),
            "{:?}",
            geometry.positions[1]
        );
        assert!(close(geometry.normals[2], [0.0, 0.0, 1.0]));
    }

    #[test]
    fn unbakeable_skins_stay_refused_with_their_cause() {
        use serde_json::json;
        type Tweak = fn(&mut serde_json::Value);
        let cases: [(&str, Tweak); 3] = [
            ("mirrored", |document| {
                document["nodes"][2]["scale"] = json!([-1.0, 1.0, 1.0]);
            }),
            ("joint is not in the scene", |document| {
                document["nodes"][0]["children"] = json!([2]);
                document["scenes"][0]["nodes"] = json!([0]);
            }),
            ("more than four joint influences", |document| {
                let attributes = &mut document["meshes"][0]["primitives"][0]["attributes"];
                attributes["JOINTS_1"] = json!(4);
                attributes["WEIGHTS_1"] = json!(5);
            }),
        ];
        for (cause, tweak) in cases {
            let reject = skin_reject(&skinned_triangle_with(true, false, tweak)).unwrap();
            assert_eq!(reject.reason, RejectReason::Skinned, "{cause}");
            assert!(reject.detail.contains(cause), "{cause}: {}", reject.detail);
        }
    }

    #[test]
    fn a_resting_skin_is_posed_like_the_stock_skinning_shader() {
        let loaded = load_model(&skinned_triangle(true, false), "meshes/x/tree.glb").unwrap();
        assert_eq!(loaded[0].reject, None);
        let positions = &loaded[0].geometry.positions;
        let expected = [[5.0, 0.0, 10.0], [3.5, 0.0, 5.0], [0.0, 1.0, 0.0]];
        for (actual, expected) in positions.iter().zip(expected) {
            for axis in 0..3 {
                assert!(
                    (actual[axis] - expected[axis]).abs() < 1e-5,
                    "{positions:?} != {expected:?}"
                );
            }
        }
        assert_eq!(loaded[0].bounds, ([0.0, 0.0, 0.0], [5.0, 1.0, 10.0]));
    }

    #[test]
    fn a_skin_in_an_animated_model_stays_refused() {
        let loaded = load_model(&skinned_triangle(true, true), "meshes/x/tree.glb").unwrap();
        let reject = loaded[0].reject.as_ref().unwrap();
        assert_eq!(reject.reason, RejectReason::Skinned);
        assert!(loaded[0].geometry.positions.is_empty());
    }

    #[test]
    fn joint_influences_without_a_skin_stay_refused() {
        let loaded = load_model(&skinned_triangle(false, false), "meshes/x/tree.glb").unwrap();
        assert_eq!(
            loaded[0].reject.as_ref().map(|reject| reject.reason),
            Some(RejectReason::Skinned)
        );
    }

    fn point(position: [f32; 3], normal: [f32; 3]) -> PrimitiveGeometry {
        PrimitiveGeometry {
            positions: vec![position],
            normals: vec![normal],
            uvs: vec![[0.0, 0.0]],
            ..Default::default()
        }
    }

    #[test]
    fn matrix_product_composes_translation_after_rotation() {
        let translate = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [10.0, 0.0, 0.0, 1.0],
        ];
        // 90 degrees about Z, column-major: the x axis maps to y.
        let rotate = [
            [0.0, 1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut geometry = point([1.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
        bake_transform(&mut geometry, &multiply(&translate, &rotate));
        assert!((geometry.positions[0][0] - 10.0).abs() < 1e-5);
        assert!((geometry.positions[0][1] - 1.0).abs() < 1e-5);
        assert!((geometry.normals[0][1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_mirror_flips_winding_and_keeps_normals_outward() {
        let mirror = [
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut geometry = PrimitiveGeometry {
            positions: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            normals: vec![[1.0, 0.0, 0.0]; 3],
            uvs: vec![[0.0, 0.0]; 3],
            indices: vec![0, 1, 2],
            ..Default::default()
        };
        bake_transform(&mut geometry, &mirror);
        assert_eq!(geometry.indices, vec![0, 2, 1]);
        assert!((geometry.normals[0][0] + 1.0).abs() < 1e-5);
    }
}
