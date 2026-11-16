//! Retain the source spheres used by native static-reference room assignment.

use project_wormhole_nif::{nif_block::NifBlock, nif_file::NifFile};
use project_wormhole_shared::glam::{Affine3A, Vec3};
use shared::room_assignment::ModelSphere;

/// Unsupported node types stay unknown instead of becoming an AABB approximation.
pub(super) fn source_sphere(nif: &NifFile) -> Option<ModelSphere> {
    fn visit(
        nif: &NifFile,
        index: u32,
        parent: Affine3A,
        scale: f32,
        path: &mut Vec<u32>,
    ) -> Option<ModelSphere> {
        if path.contains(&index) || path.len() >= 256 {
            return None;
        }
        path.push(index);
        let block = nif.blocks.get(index as usize)?;
        let (av, sphere, children) = match block {
            NifBlock::NiNode(node)
            | NifBlock::BSFadeNode(node)
            | NifBlock::BSLeafAnimNode(node) => (&node.av, None, node.children.as_slice()),
            NifBlock::BSOrderedNode(node)
                if node.static_bound != 0 && node.node.av.flags.fixed_bound() =>
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
            parent
                * Affine3A::from_mat3_translation(av.rotation.0.0 * local_scale, av.translation.0.0)
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
            result = result.merge(visit(nif, child, world, scale, path)?);
        }
        path.pop();
        result.valid().then_some(result)
    }
    // Static fallback exports disconnected geometry as additional roots. A bound computed only
    // from block zero must not claim ownership of those extra meshes. Follow structural children
    // even below fixed-bound nodes: their authored bound still represents that whole subtree.
    let mut reachable = std::collections::HashSet::new();
    let mut pending = vec![0u32];
    while let Some(index) = pending.pop() {
        if !reachable.insert(index) {
            continue;
        }
        if let Ok(node) = nif.blocks.get(index as usize)?.as_node() {
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
    let mut sphere = visit(nif, 0, Affine3A::IDENTITY, 1.0, &mut Vec::new())?;
    sphere.center = shared::coordinates::creation_to_runtime_vector(sphere.center);
    (sphere.radius > 0.0).then_some(sphere)
}
