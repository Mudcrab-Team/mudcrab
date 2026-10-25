//! Rest-pose skin evaluation for optional grass obstructions.

use super::*;
use std::collections::BTreeSet;

/// Check POSITION references before gltf-json's validation hook indexes them unchecked.
pub(super) fn validate_position_references(document: &gltf::Document) -> Result<()> {
    let root = document.as_json();
    for mesh in &root.meshes {
        for primitive in &mesh.primitives {
            if let Some(index) = primitive
                .attributes
                .get(&gltf::json::validation::Checked::Valid(
                    gltf::json::mesh::Semantic::Positions,
                ))
            {
                ensure!(
                    index.value() < root.accessors.len(),
                    "position accessor out of bounds"
                );
            }
        }
    }
    Ok(())
}

/// Remove only skin dependencies, then validate the remaining static model normally.
pub(super) fn static_fallback(bytes: &[u8]) -> Result<gltf::Gltf> {
    let container = gltf::binary::Glb::from_slice(bytes)?;
    let mut json: serde_json::Value = serde_json::from_slice(&container.json)?;
    let mut had_skin = false;
    if let Some(nodes) = json
        .get_mut("nodes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for node in nodes {
            if let Some(node) = node.as_object_mut() {
                had_skin |= node.remove("skin").is_some();
            }
        }
    }
    ensure!(had_skin, "model has no skin to fall back from");
    if let Some(root) = json.as_object_mut() {
        root.remove("skins");
        // Animation data does not participate in the authored static pose.
        root.remove("animations");
    }
    if let Some(meshes) = json
        .get_mut("meshes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for mesh in meshes {
            if let Some(primitives) = mesh
                .get_mut("primitives")
                .and_then(serde_json::Value::as_array_mut)
            {
                for primitive in primitives {
                    if let Some(attributes) = primitive
                        .get_mut("attributes")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        attributes.retain(|key, _| {
                            !key.starts_with("JOINTS_") && !key.starts_with("WEIGHTS_")
                        });
                    }
                }
            }
        }
    }
    isolate_static_data(&mut json)?;
    let document = gltf::Document::from_json_without_validation(serde_json::from_value(json)?);
    validate_position_references(&document)?;
    let document = gltf::Document::from_json(document.into_json())?;
    Ok(gltf::Gltf {
        document,
        blob: container.bin.map(|bin| bin.into_owned()),
    })
}

/// Read an index without narrowing or indexing unchecked, including during fallback preparation.
fn json_index(value: &serde_json::Value) -> Result<usize> {
    value
        .as_u64()
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid static data index"))
}

/// Neutralize unused skin data without renumbering references into usable static buffers.
fn isolate_static_data(json: &mut serde_json::Value) -> Result<()> {
    let mut accessors = BTreeSet::new();
    if let Some(meshes) = json.get("meshes").and_then(serde_json::Value::as_array) {
        for mesh in meshes {
            if let Some(primitives) = mesh.get("primitives").and_then(serde_json::Value::as_array) {
                for primitive in primitives {
                    if let Some(attributes) = primitive
                        .get("attributes")
                        .and_then(serde_json::Value::as_object)
                    {
                        for index in attributes.values() {
                            accessors.insert(json_index(index)?);
                        }
                    }
                    if let Some(index) = primitive.get("indices") {
                        accessors.insert(json_index(index)?);
                    }
                    if let Some(targets) = primitive
                        .get("targets")
                        .and_then(serde_json::Value::as_array)
                    {
                        for target in targets {
                            if let Some(attributes) = target.as_object() {
                                for index in attributes.values() {
                                    accessors.insert(json_index(index)?);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut views = BTreeSet::new();
    for &index in &accessors {
        let accessor = json
            .get("accessors")
            .and_then(|array| array.get(index))
            .ok_or_else(|| color_eyre::eyre::eyre!("static accessor out of bounds"))?;
        for view in [
            accessor.get("bufferView"),
            accessor.pointer("/sparse/indices/bufferView"),
            accessor.pointer("/sparse/values/bufferView"),
        ]
        .into_iter()
        .flatten()
        {
            views.insert(json_index(view)?);
        }
    }
    if let Some(images) = json.get("images").and_then(serde_json::Value::as_array) {
        for image in images {
            if let Some(view) = image.get("bufferView") {
                views.insert(json_index(view)?);
            }
        }
    }
    let mut buffers = BTreeSet::new();
    for &index in &views {
        let buffer = json
            .get("bufferViews")
            .and_then(|array| array.get(index))
            .and_then(|view| view.get("buffer"))
            .ok_or_else(|| color_eyre::eyre::eyre!("static buffer view out of bounds"))?;
        buffers.insert(json_index(buffer)?);
    }
    for (key, used) in [
        ("accessors", accessors),
        ("bufferViews", views),
        ("buffers", buffers),
    ] {
        let array = json
            .get_mut(key)
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| color_eyre::eyre::eyre!("missing static model data"))?;
        let replacement = used
            .first()
            .and_then(|&index| array.get(index))
            .cloned()
            .ok_or_else(|| color_eyre::eyre::eyre!("no usable static model data"))?;
        ensure!(
            used.iter().all(|&index| index < array.len()),
            "static data index out of bounds"
        );
        // gltf-json validates even unused entries. Skin-only corrupt accessors/views/buffers
        // must not prevent the separately validated static geometry from being recovered.
        for (index, value) in array.iter_mut().enumerate() {
            if !used.contains(&index) {
                *value = replacement.clone();
            }
        }
    }
    Ok(())
}

/// Collect selected-scene globals independently of mesh/joint visitation order.
pub(super) fn scene_transforms(
    scene: &gltf::Scene<'_>,
    count: usize,
) -> Result<Vec<Option<DMat4>>> {
    /// Reject cycles and ambiguous paths before descending through a node twice.
    fn visit(
        node: gltf::Node<'_>,
        parent: DMat4,
        globals: &mut [Option<DMat4>],
        depth: usize,
    ) -> Result<()> {
        ensure!(depth < 128, "model hierarchy exceeds inspection depth");
        ensure!(
            globals[node.index()].is_none(),
            "cyclic or multiply parented skin hierarchy"
        );
        let matrix = node
            .transform()
            .matrix()
            .map(|column| column.map(f64::from));
        let global = parent * DMat4::from_cols_array_2d(&matrix);
        ensure!(global.is_finite(), "nonfinite joint transform");
        globals[node.index()] = Some(global);
        for child in node.children() {
            visit(child, global, globals, depth + 1)?;
        }
        Ok(())
    }
    let mut globals = vec![None; count];
    for node in scene.nodes() {
        visit(node, DMat4::IDENTITY, &mut globals, 0)?;
    }
    Ok(globals)
}

/// Require finite, nonsingular affine skin matrices without an arbitrary scale cutoff.
fn validate_matrix(matrix: DMat4) -> Result<()> {
    let determinant = matrix.determinant();
    ensure!(
        matrix.is_finite() && determinant.is_finite() && determinant != 0.0,
        "nonfinite or singular skin matrix"
    );
    ensure!(matrix.row(3) == glam::DVec4::W, "nonaffine skin matrix");
    Ok(())
}

/// Evaluate every declared weight set as global joint * inverse bind, ignoring mesh transforms.
pub(super) fn positions(
    primitive: &gltf::Primitive<'_>,
    skin: gltf::Skin<'_>,
    globals: &[Option<DMat4>],
    blob: &[u8],
    positions: &[DVec3],
) -> Result<Vec<DVec3>> {
    let joints: Vec<_> = skin.joints().collect();
    ensure!(!joints.is_empty(), "skin has no joints");
    let binds = if let Some(accessor) = skin.inverse_bind_matrices() {
        ensure!(
            accessor.data_type() == gltf::accessor::DataType::F32
                && accessor.dimensions() == gltf::accessor::Dimensions::Mat4
                && !accessor.normalized()
                && accessor.count() >= joints.len(),
            "invalid inverse bind matrix format or count"
        );
        validate_accessor(&accessor, blob)?;
        skin.reader(|_| Some(blob))
            .read_inverse_bind_matrices()
            .ok_or_else(|| color_eyre::eyre::eyre!("unreadable inverse bind matrices"))?
            .take(joints.len())
            .map(|matrix| DMat4::from_cols_array_2d(&matrix.map(|column| column.map(f64::from))))
            .collect::<Vec<_>>()
    } else {
        vec![DMat4::IDENTITY; joints.len()]
    };
    ensure!(binds.len() == joints.len(), "missing inverse bind matrices");
    let mut palette = Vec::with_capacity(joints.len());
    for (joint, bind) in joints.iter().zip(binds) {
        let global = globals
            .get(joint.index())
            .copied()
            .flatten()
            .ok_or_else(|| color_eyre::eyre::eyre!("joint missing from model scene"))?;
        validate_matrix(global)?;
        validate_matrix(bind)?;
        let matrix = global * bind;
        validate_matrix(matrix)?;
        palette.push(matrix);
    }
    let mut joint_sets = BTreeSet::new();
    let mut weight_sets = BTreeSet::new();
    for (semantic, _) in primitive.attributes() {
        match semantic {
            gltf::Semantic::Joints(set) => {
                joint_sets.insert(set);
            }
            gltf::Semantic::Weights(set) => {
                weight_sets.insert(set);
            }
            _ => {}
        }
    }
    ensure!(
        !joint_sets.is_empty() && joint_sets == weight_sets,
        "missing or unpaired skin attributes"
    );
    let reader = primitive.reader(|_| Some(blob));
    let mut result = vec![DVec3::ZERO; positions.len()];
    let mut totals = vec![0.0f64; positions.len()];
    for (expected, set) in joint_sets.into_iter().enumerate() {
        ensure!(
            set as usize == expected,
            "noncontiguous skin attribute sets"
        );
        let joints = primitive.get(&gltf::Semantic::Joints(set)).unwrap();
        let weights = primitive.get(&gltf::Semantic::Weights(set)).unwrap();
        ensure!(
            matches!(
                joints.data_type(),
                gltf::accessor::DataType::U8 | gltf::accessor::DataType::U16
            ) && joints.dimensions() == gltf::accessor::Dimensions::Vec4
                && !joints.normalized(),
            "unsupported joint attribute format"
        );
        ensure!(
            weights.dimensions() == gltf::accessor::Dimensions::Vec4
                && match weights.data_type() {
                    gltf::accessor::DataType::F32 => !weights.normalized(),
                    gltf::accessor::DataType::U8 | gltf::accessor::DataType::U16 =>
                        weights.normalized(),
                    _ => false,
                },
            "unsupported weight attribute format"
        );
        ensure!(
            joints.count() == positions.len() && weights.count() == positions.len(),
            "skin attribute count differs from positions"
        );
        validate_accessor(&joints, blob)?;
        validate_accessor(&weights, blob)?;
        let joints: Vec<_> = reader
            .read_joints(set)
            .ok_or_else(|| color_eyre::eyre::eyre!("unreadable joints"))?
            .into_u16()
            .collect();
        let weights: Vec<_> = reader
            .read_weights(set)
            .ok_or_else(|| color_eyre::eyre::eyre!("unreadable weights"))?
            .into_f32()
            .collect();
        ensure!(
            joints.len() == positions.len() && weights.len() == positions.len(),
            "short skin attributes"
        );
        for (index, (joints, weights)) in joints.into_iter().zip(weights).enumerate() {
            for (joint, weight) in joints.into_iter().zip(weights) {
                let matrix = palette
                    .get(usize::from(joint))
                    .ok_or_else(|| color_eyre::eyre::eyre!("skin joint index out of bounds"))?;
                let weight = f64::from(weight);
                ensure!(weight.is_finite() && weight >= 0.0, "invalid skin weight");
                result[index] += matrix.transform_point3(positions[index]) * weight;
                totals[index] += weight;
            }
        }
    }
    for (position, total) in result.iter_mut().zip(totals) {
        ensure!(total.is_finite() && total > 0.0, "invalid skin weight sum");
        *position /= total;
        ensure!(position.is_finite(), "nonfinite skinned position");
    }
    Ok(result)
}
