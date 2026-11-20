//! Retain the source spheres used by native static-reference room assignment.

use project_wormhole_nif::{nif_block::NifBlock, nif_file::NifFile};
use project_wormhole_shared::glam::{Affine3A, DVec3, Mat3, Mat4, Quat, Vec3, Vec4};
use shared::room_assignment::ModelSphere;

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
            // LoadBinary overwrites the file bit and clears it for zero or unordered radii.
            NifBlock::BSOrderedNode(node)
                if node.static_bound != 0 && node.alpha_sort_bound[3].abs() > 0.0 =>
            {
                let bound = ModelSphere {
                    center: node.alpha_sort_bound[..3].try_into().ok()?,
                    radius: node.alpha_sort_bound[3],
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

/// A native bound is usable only when it also covers the emitted render pose.
pub(super) fn encloses_scene(glb: &[u8], sphere: ModelSphere) -> bool {
    scene_fits(glb, sphere).unwrap_or(false)
}

fn scene_fits(glb: &[u8], sphere: ModelSphere) -> Option<bool> {
    let chunks = gltf::binary::Glb::from_slice(glb).ok()?;
    let gltf = gltf::Gltf::from_slice(&chunks.json).ok()?;
    let scene = gltf.default_scene()?;
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
