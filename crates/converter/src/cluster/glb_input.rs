//! Reads a converted GLB into [`ModelPrimitive`]s for classification and building.
//!
//! Node transforms (and the scene root's Creation-to-runtime rotation, which the converter writes
//! as a node) are baked into the vertex data, so a primitive's positions are in runtime (Y-up)
//! axes and a reference's TRS applies directly. A mirrored node flips the triangle winding.

use super::model::{AlphaMode, ModelPrimitive, PrimitiveGeometry, Reject, RejectReason};
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use gltf::{Gltf, Node, Semantic, mesh::Mode};
use serde_json::Value;
use shared::cluster_material::{stock_requires_sorted_depth, unsupported_feature};
use shared::cluster_recipe::model_facts::{material_key, parse_glb_json, raw_material};

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

    let mut primitives = Vec::new();
    let mut stack: Vec<(Node, Matrix)> = scene.nodes().map(|node| (node, IDENTITY)).collect();
    stack.reverse();
    while let Some((node, parent)) = stack.pop() {
        let world = multiply(&parent, &node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                let index = primitives.len() as u32;
                let material = raw_material(&raw, primitive.material().index())?;
                let material_key = material_key(&material, &raw, glb_path)?;
                primitives.push(read_primitive(
                    index,
                    &primitive,
                    node.skin().is_some(),
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

fn read_primitive(
    index: u32,
    primitive: &gltf::Primitive<'_>,
    node_skinned: bool,
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
    let skinned = node_skinned || primitive.get(&Semantic::Joints(0)).is_some();
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
    if structural.is_some() {
        geometry = PrimitiveGeometry::default();
    } else {
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
    } else if skinned {
        Some(Reject::new(RejectReason::Skinned, "skin or JOINTS_0"))
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
    let determinant = linear(0, 0) * (linear(1, 1) * linear(2, 2) - linear(1, 2) * linear(2, 1))
        - linear(0, 1) * (linear(1, 0) * linear(2, 2) - linear(1, 2) * linear(2, 0))
        + linear(0, 2) * (linear(1, 0) * linear(2, 1) - linear(1, 1) * linear(2, 0));
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
