//! Retain the source spheres used by native static-reference room assignment.

use color_eyre::{Result, eyre::eyre};
use project_wormhole_nif::{nif_block::NifBlock, nif_file::NifFile};
use project_wormhole_shared::glam::{Affine3A, Mat3, Vec3};
use shared::room_assignment::ModelSphere;
use std::collections::HashSet;

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
                } else if switch.active_child == u32::MAX {
                    &[]
                } else {
                    std::slice::from_ref(children.get(switch.active_child as usize)?)
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
                let bound = ModelSphere {
                    center: node.alpha_sort_bound[..3].try_into().ok()?,
                    radius: node.alpha_sort_bound[3],
                };
                (&node.node.av, Some(bound), &[][..])
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
            parent * Affine3A::from_mat3_translation(rotation * local_scale, av.translation.0.0)
        };
        let scale = scale * local_scale;
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
            result = result.merge(visit(nif, child, world, scale, path, remaining)?);
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
        ) && !reachable.contains(&(index as u32))
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
        if !visible.insert(index as u32) {
            continue;
        }
        let block = &nif.blocks[index];
        let mut draw = children(block);
        if let NifBlock::NiSwitchNode(switch) = block {
            if switch.active_child == u32::MAX {
                draw = &[];
            } else if let Some(child) = draw.get(switch.active_child as usize)
                && (*child == u32::MAX || (*child as usize) < nif.blocks.len())
            {
                draw = std::slice::from_ref(child);
            }
            // An invalid selection leaves all branches available.
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
    for (index, node) in nodes.iter().enumerate() {
        if let Some(mesh) = node.get("mesh") {
            let block = mesh
                .as_u64()
                .and_then(|id| usize::try_from(id).ok())
                .and_then(|id| shape_blocks.get(id))
                .ok_or_else(|| {
                    eyre!("exported switch node {index} has no source shape for mesh {mesh}")
                })?;
            if !visible.contains(block) {
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
            // Keep an explicit empty scene root list, as in empty_scene_glb. The bounds
            // reader otherwise falls back to all parentless nodes (including detached ones).
            if key == "children" && children.is_empty() {
                fields.remove(key);
            }
        }
    }
    for node in nodes {
        unlink(node, "children", &hidden);
    }
    for root in scene["scenes"]
        .as_array_mut()
        .ok_or_else(|| eyre!("exported switch scene has no scene array"))?
    {
        unlink(root, "nodes", &hidden);
    }
    Ok(())
}
