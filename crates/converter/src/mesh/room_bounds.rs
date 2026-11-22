//! Retain source spheres and initial switch geometry for room assignment. Verify each static
//! drawable’s shape-local sphere against its exported vertices for portal culling.

use color_eyre::{Result, eyre::eyre};
use project_wormhole_nif::{model::all::StaticMesh, nif_block::NifBlock, nif_file::NifFile};
use project_wormhole_shared::glam::{Affine3A, DVec3, Mat3, Mat4, Quat, Vec3, Vec4};
use shared::room_assignment::{DRAWABLE_SPHERE_EXTRA, ModelSphere};
use std::collections::HashSet;

/// Publish the shape-local bound only when it encloses every emitted vertex.
pub(super) fn tag_drawable_spheres(
    nif: &NifFile,
    meshes: &[StaticMesh],
    blocks: &[u32],
    document: &mut serde_json::Value,
) -> Result<()> {
    let Some(output) = document
        .get_mut("meshes")
        .and_then(serde_json::Value::as_array_mut)
        .filter(|output| output.len() == blocks.len() && output.len() == meshes.len())
    else {
        return Ok(());
    };
    for ((output, mesh), block) in output.iter_mut().zip(meshes).zip(blocks) {
        let Some(NifBlock::BSTriShape(shape)) = nif.blocks.get(*block as usize) else {
            continue;
        };
        let sphere = ModelSphere {
            center: shape.bounding_sphere.center,
            radius: shape.bounding_sphere.radius,
        };
        if !sphere.valid() || sphere.radius <= 0.0 || mesh.positions.is_empty() {
            continue;
        }
        let center = Vec3::from_array(sphere.center).as_dvec3();
        if mesh
            .positions
            .iter()
            .all(|pos| pos.as_dvec3().distance(center) <= f64::from(sphere.radius))
        {
            output["extras"][DRAWABLE_SPHERE_EXTRA] = serde_json::to_value(sphere)?;
        }
    }
    Ok(())
}

/// Unsupported node types stay unknown instead of becoming an AABB approximation.
pub(super) fn source_sphere(nif: &NifFile) -> Option<ModelSphere> {
    fn visit(
        nif: &NifFile,
        index: u32,
        parent: Affine3A,
        scale: f32,
        path: &mut Vec<u32>,
        remaining: &mut usize,
    ) -> Option<ModelSphere> {
        if path.contains(&index) {
            return None;
        }
        *remaining = remaining.checked_sub(1)?;
        let block = nif.blocks.get(index as usize)?;
        if block.as_node().is_ok() && path.len() >= super::MAX_SCENE_DEPTH {
            return None;
        }
        path.push(index);
        let (av, sphere, children) = match block {
            NifBlock::NiNode(node)
            | NifBlock::BSFadeNode(node)
            | NifBlock::BSLeafAnimNode(node) => (&node.av, None, node.children.as_slice()),
            NifBlock::BSValueNode(node) if node.node.children.is_empty() => {
                (&node.node.av, None, &[][..])
            }
            NifBlock::NiSwitchNode(switch) => {
                let children = switch.node.children.as_slice();
                // NiSwitchNode::UpdateWorldBound uses only the active child when bit 0 is set.
                let contributing = if switch.flags & 1 == 0 {
                    children
                } else if let Ok(active_child) = usize::try_from(switch.active_child) {
                    std::slice::from_ref(children.get(active_child)?)
                } else {
                    &[]
                };
                (&switch.node.av, None, contributing)
            }
            NifBlock::BSMultiBoundNode(node) => {
                let NifBlock::BSMultiBound(bound) = nif.blocks.get(node.bound.0? as usize)? else {
                    return None;
                };
                let NifBlock::BSMultiBoundOBB(shape) = nif.blocks.get(bound.data.0? as usize)?
                else {
                    return None;
                };
                let extent = shape.size.0;
                if !extent.is_finite() || extent.min_element() < 0.0 {
                    return None;
                }
                let bound = ModelSphere {
                    center: shape.center.0.to_array(),
                    radius: extent.length(),
                };
                (&node.node.av, Some(bound), &[][..])
            }
            // LoadBinary overwrites the file bit and clears it for zero or unordered radii.
            NifBlock::BSOrderedNode(node)
                if node.static_bound != 0 && node.alpha_sort_bound[3].abs() > 0.0 =>
            {
                let [x, y, z, radius] = node.alpha_sort_bound;
                let bound = ModelSphere {
                    center: [x, y, z],
                    radius,
                };
                (&node.node.av, Some(bound), node.node.children.as_slice())
            }
            NifBlock::BSOrderedNode(node) => (&node.node.av, None, node.node.children.as_slice()),
            // Empty damage-stage nodes contribute a zero sphere. Nonempty ranges still need
            // native selection semantics before they can supply assignment metadata.
            NifBlock::BSBlastNode(node) if node.node.children.is_empty() => {
                (&node.node.av, None, &[][..])
            }
            NifBlock::BSTriShape(shape) => (
                &shape.av,
                Some(ModelSphere {
                    center: shape.bounding_sphere.center,
                    radius: shape.bounding_sphere.radius,
                }),
                &[][..],
            ),
            NifBlock::BSLODTriShape(shape) => (
                &shape.bs_tri_shape.av,
                Some(ModelSphere {
                    center: shape.bs_tri_shape.bounding_sphere.center,
                    radius: shape.bs_tri_shape.bounding_sphere.radius,
                }),
                &[][..],
            ),
            NifBlock::BSSubIndexTriShape(shape) => (
                &shape.bs_tri_shape.av,
                Some(ModelSphere {
                    center: shape.bs_tri_shape.bounding_sphere.center,
                    radius: shape.bs_tri_shape.bounding_sphere.radius,
                }),
                &[][..],
            ),
            _ => return None,
        };
        let local_scale = if index == super::NIF_ROOT_BLOCK {
            1.0
        } else {
            av.scale.0
        };
        let world = if index == super::NIF_ROOT_BLOCK {
            parent
        } else {
            let rotation = av.rotation.0.0;
            if rotation.determinant() <= 0.0
                || !(rotation.transpose() * rotation).abs_diff_eq(Mat3::IDENTITY, 1.0e-4)
            {
                return None;
            }
            let rotation = Quat::from_mat3(&rotation).normalize();
            parent
                * Affine3A::from_scale_rotation_translation(
                    Vec3::splat(local_scale),
                    rotation,
                    av.translation.0.0,
                )
        };
        let scale = scale * local_scale;
        let basis = Mat3::from(world.matrix3);
        if scale == 0.0 {
            if basis != Mat3::ZERO {
                return None;
            }
        } else {
            let rotation = basis / scale;
            if !(rotation.transpose() * rotation).abs_diff_eq(Mat3::IDENTITY, 1.0e-4) {
                return None;
            }
        }
        let mut result = if let Some(sphere) = sphere {
            ModelSphere {
                center: world
                    .transform_point3(Vec3::from_array(sphere.center))
                    .to_array(),
                radius: sphere.radius * scale.abs(),
            }
        } else {
            ModelSphere {
                center: [0.0; 3],
                radius: 0.0,
            }
        };
        for &child in children.iter().filter(|&&child| child != u32::MAX) {
            let child_bound = visit(nif, child, world, scale, path, remaining)?;
            if sphere.is_none() {
                result = result.merge(child_bound);
            }
        }
        path.pop();
        result.valid().then_some(result)
    }
    // Static fallback exports disconnected geometry as additional roots. A bound computed only
    // from block zero must not claim ownership of those extra meshes. Follow structural children
    // even below fixed-bound nodes: their authored bound still represents that whole subtree.
    let mut reachable = HashSet::new();
    let mut pending = vec![0u32];
    while let Some(index) = pending.pop() {
        if !reachable.insert(index) {
            continue;
        }
        let block = nif.blocks.get(index as usize)?;
        if let Ok(node) = block.as_node() {
            pending.extend(
                node.children
                    .iter()
                    .copied()
                    .filter(|&child| child != u32::MAX),
            );
        }
    }
    if nif.blocks.iter().enumerate().any(|(index, block)| {
        matches!(
            block,
            NifBlock::BSTriShape(_)
                | NifBlock::BSDynamicTriShape(_)
                | NifBlock::BSSubIndexTriShape(_)
                | NifBlock::BSLODTriShape(_)
                | NifBlock::NiTriShape(_)
        ) && !u32::try_from(index).is_ok_and(|index| reachable.contains(&index))
    }) {
        return None;
    }
    // Shared children can expand exponentially without a cycle. Trees need at most one visit
    // per block; decline bounds when repeated paths exceed that work budget.
    let mut remaining = nif.blocks.len();
    let mut sphere = visit(
        nif,
        0,
        Affine3A::IDENTITY,
        1.0,
        &mut Vec::new(),
        &mut remaining,
    )?;
    sphere.center = shared::coordinates::creation_to_runtime_vector(sphere.center);
    (sphere.radius > 0.0).then_some(sphere)
}

/// Unlink inactive mesh leaves from the initial scene. Static and skinned mesh nodes both
/// carry the exported mesh index, even when the skinned node is its own glTF scene root.
pub(super) fn select_switch_geometry(
    nif: &NifFile,
    shape_blocks: &[u32],
    scene: &mut serde_json::Value,
) -> Result<()> {
    fn children(block: &NifBlock) -> &[u32] {
        match block {
            NifBlock::BSBlastNode(blast) => &blast.node.children,
            _ => block.as_node().map_or(&[], |node| node.children.as_slice()),
        }
    }

    // Start from every structural root, as the exporter does for disconnected geometry.
    // A child shared with an active branch remains visible through that branch.
    let mut root = vec![true; nif.blocks.len()];
    for child in nif.blocks.iter().flat_map(children) {
        if let Some(slot) = root.get_mut(*child as usize) {
            *slot = false;
        }
    }
    let mut pending: Vec<_> = (0..root.len()).filter(|&index| root[index]).collect();
    let mut visible = HashSet::new();
    while let Some(index) = pending.pop() {
        if !visible.insert(index) {
            continue;
        }
        let block = &nif.blocks[index];
        let mut draw = children(block);
        if let NifBlock::NiSwitchNode(switch) = block {
            if let Ok(active_child) = usize::try_from(switch.active_child) {
                if let Some(child) = draw.get(active_child)
                    && (*child == u32::MAX || (*child as usize) < nif.blocks.len())
                {
                    draw = std::slice::from_ref(child);
                }
            } else {
                draw = &[];
            }
            // An out-of-range index leaves all branches available.
        }
        pending.extend(
            draw.iter()
                .map(|&id| id as usize)
                .filter(|&id| id < nif.blocks.len()),
        );
    }
    let nodes = scene["nodes"]
        .as_array_mut()
        .ok_or_else(|| eyre!("exported switch scene has no node array"))?;
    let mut hidden = HashSet::new();
    let mut mesh_nodes = 0;
    for (index, node) in nodes.iter().enumerate() {
        if let Some(mesh) = node.get("mesh") {
            mesh_nodes += 1;
            let block = mesh
                .as_u64()
                // silent-ok: An unrepresentable mesh index returns the missing-source-shape error below.
                .and_then(|id| usize::try_from(id).ok())
                .and_then(|id| shape_blocks.get(id))
                .ok_or_else(|| {
                    eyre!("exported switch node {index} has no source shape for mesh {mesh}")
                })?;
            if !visible.contains(&(*block as usize)) {
                hidden.insert(index as u64);
            }
        }
    }
    fn unlink(parent: &mut serde_json::Value, key: &str, hidden: &HashSet<u64>) {
        let Some(fields) = parent.as_object_mut() else {
            return;
        };
        if let Some(children) = fields
            .get_mut(key)
            .and_then(serde_json::Value::as_array_mut)
        {
            children.retain(|child| child.as_u64().is_none_or(|id| !hidden.contains(&id)));
            if key == "children" && children.is_empty() {
                fields.remove(key);
            }
        }
    }
    let empty_root = nodes.len();
    let mut needs_root = false;
    for node in nodes {
        unlink(node, "children", &hidden);
    }
    for root in scene["scenes"]
        .as_array_mut()
        .ok_or_else(|| eyre!("exported switch scene has no scene array"))?
    {
        unlink(root, "nodes", &hidden);
        if root["nodes"].as_array().is_some_and(Vec::is_empty) {
            root["nodes"] = serde_json::json!([empty_root]);
            needs_root = true;
        }
    }
    if needs_root {
        scene["nodes"]
            .as_array_mut()
            .ok_or_else(|| eyre!("missing node array"))?
            .push(serde_json::json!({}));
    }
    if mesh_nodes > 0 && hidden.len() == mesh_nodes {
        scene["extras"][super::INACTIVE_SWITCH] = serde_json::json!(true);
    }
    Ok(())
}

/// A native bound is usable only when it also covers the emitted render pose.
pub(super) fn encloses_scene(
    glb: &[u8],
    document: &serde_json::Value,
    sphere: ModelSphere,
) -> bool {
    scene_fits(glb, document, sphere, None).unwrap_or(false)
}

// These extensions affect shading or UVs and leave the positions used for enclosure intact.
fn material_or_uv_extension(name: &str) -> bool {
    matches!(
        name,
        "KHR_materials_anisotropy"
            | "KHR_materials_clearcoat"
            | "KHR_materials_diffuse_transmission"
            | "KHR_materials_dispersion"
            | "KHR_materials_emissive_strength"
            | "KHR_materials_ior"
            | "KHR_materials_iridescence"
            | "KHR_materials_pbrSpecularGlossiness"
            | "KHR_materials_sheen"
            | "KHR_materials_specular"
            | "KHR_materials_transmission"
            | "KHR_materials_unlit"
            | "KHR_materials_variants"
            | "KHR_materials_volume"
            | "KHR_texture_transform"
    )
}

pub(crate) fn native_sphere_eligible(document: &serde_json::Value) -> bool {
    // Streaming instantiates scene 0. Prove enclosure only for embedded, rigid geometry.
    ["animations", "skins"].into_iter().all(|key| {
        document
            .get(key)
            .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty))
    }) && ["extensionsUsed", "extensionsRequired"]
        .into_iter()
        .all(|key| {
            document.get(key).is_none_or(|value| {
                value.as_array().is_some_and(|names| {
                    names
                        .iter()
                        .all(|name| name.as_str().is_some_and(material_or_uv_extension))
                })
            })
        })
        && document["buffers"].as_array().is_some_and(
            |buffers| matches!(buffers.as_slice(), [buffer] if buffer.get("uri").is_none()),
        )
}

pub(crate) fn encloses_native_scene(
    glb: &[u8],
    document: &serde_json::Value,
    sphere: ModelSphere,
) -> bool {
    scene_fits(glb, document, sphere, Some(0)).unwrap_or(false)
}

fn geometry_document(mut root: gltf::json::Root) -> Option<gltf::Document> {
    // gltf validates required renderer features too. Only geometry is consumed here.
    root.extensions_required
        .retain(|name| !material_or_uv_extension(name));
    // silent-ok: An invalid geometry document cannot certify sphere enclosure.
    gltf::Document::from_json(root).ok()
}

fn scene_fits(
    glb: &[u8],
    document: &serde_json::Value,
    sphere: ModelSphere,
    scene_index: Option<usize>,
) -> Option<bool> {
    // silent-ok: A malformed GLB cannot certify sphere enclosure.
    let chunks = gltf::binary::Glb::from_slice(glb).ok()?;
    // silent-ok: A malformed scene description cannot certify sphere enclosure.
    let gltf = geometry_document(serde_json::from_value(document.clone()).ok()?)?;
    let scene = match scene_index {
        Some(index) => gltf.scenes().nth(index),
        None => gltf.default_scene(),
    }?;
    let blob = chunks.bin.as_deref()?;
    let buffer = |buffer: gltf::Buffer<'_>| (buffer.index() == 0).then_some(blob);
    let mut transforms = vec![None; gltf.nodes().len()];
    let mut pending: Vec<_> = scene
        .nodes()
        .map(|node| (node, Affine3A::IDENTITY))
        .collect();
    let mut drawables = Vec::new();
    while let Some((node, parent)) = pending.pop() {
        let gltf::scene::Transform::Decomposed {
            translation,
            rotation,
            scale,
        } = node.transform()
        else {
            return None;
        };
        let world = parent
            * Affine3A::from_scale_rotation_translation(
                Vec3::from_array(scale),
                Quat::from_array(rotation),
                Vec3::from_array(translation),
            );
        if !world.is_finite() || transforms[node.index()].replace(world).is_some() {
            return None;
        }
        pending.extend(node.children().map(|child| (child, world)));
        if let Some(mesh) = node.mesh() {
            drawables.push((mesh, node.skin(), world));
        }
    }
    let center = Vec3::from_array(sphere.center).as_dvec3();
    let contains = |point: DVec3| point.distance(center) <= f64::from(sphere.radius);
    let mut seen_position = false;
    for (mesh, skin, world) in drawables {
        let joint_transforms = if let Some(skin) = skin {
            let matrices = skin.reader(buffer).read_inverse_bind_matrices()?;
            let joints = skin.joints();
            if joints.len() == 0 || matrices.len() != joints.len() {
                return None;
            }
            joints
                .zip(matrices)
                .map(|(joint, matrix)| {
                    let inverse_bind = Mat4::from_cols_array_2d(&matrix);
                    // Skinning uses the world joint transform directly. Only affine
                    // bind matrices support the point transformation below.
                    if inverse_bind.row(3) != Vec4::W {
                        return None;
                    }
                    let pose = Mat4::from(transforms[joint.index()]?) * inverse_bind;
                    pose.is_finite().then_some(pose.as_dmat4())
                })
                .collect::<Option<Vec<_>>>()?
        } else {
            Vec::new()
        };
        for primitive in mesh.primitives() {
            if primitive.morph_targets().len() != 0 {
                return None;
            }
            let reader = primitive.reader(buffer);
            let positions = reader.read_positions()?;
            seen_position |= positions.len() != 0;
            if joint_transforms.is_empty() {
                let world = Mat4::from(world).as_dmat4();
                if !positions
                    .map(|position| world.transform_point3(Vec3::from_array(position).as_dvec3()))
                    .all(contains)
                {
                    return Some(false);
                }
                continue;
            }
            let joints = reader.read_joints(0)?.into_u16();
            let weights = reader.read_weights(0)?.into_f32();
            if joints.len() != positions.len() || weights.len() != positions.len() {
                return None;
            }
            for ((position, joints), weights) in positions.zip(joints).zip(weights) {
                let position = Vec3::from_array(position).as_dvec3();
                let mut deformed = DVec3::ZERO;
                for (joint, weight) in joints.into_iter().zip(weights) {
                    if !weight.is_finite() || weight < 0.0 {
                        return None;
                    }
                    let transform = joint_transforms.get(joint as usize)?;
                    deformed += transform.transform_point3(position) * f64::from(weight);
                }
                if (weights.iter().sum::<f32>() - 1.0).abs() > 4.0 * f32::EPSILON
                    || !contains(deformed)
                {
                    return Some(false);
                }
            }
        }
    }
    Some(seen_position)
}
