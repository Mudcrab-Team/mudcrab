//! Retain the source spheres used by native static-reference room assignment.

use project_wormhole_nif::{nif_block::NifBlock, nif_file::NifFile};
use project_wormhole_shared::glam::{Affine3A, Mat3, Mat4, Quat, Vec3};
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
            NifBlock::NiNode(node) | NifBlock::BSFadeNode(node) => {
                (&node.av, None, node.children.as_slice())
            }
            NifBlock::BSTriShape(shape) => (&shape.av, Some(shape.bounding_sphere), &[][..]),
            NifBlock::BSLODTriShape(shape) => (
                &shape.bs_tri_shape.av,
                Some(shape.bs_tri_shape.bounding_sphere),
                &[][..],
            ),
            NifBlock::BSSubIndexTriShape(shape) => (
                &shape.bs_tri_shape.av,
                Some(shape.bs_tri_shape.bounding_sphere),
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
            result = result.merge(visit(nif, child, world, scale, path, remaining)?);
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

/// A native bound is usable only when it also covers the emitted render pose.
pub(super) fn encloses_scene(glb: &[u8], sphere: ModelSphere) -> bool {
    scene_fits(glb, sphere).unwrap_or(false)
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

pub(crate) fn encloses_native_scene(
    glb: &[u8],
    document: &serde_json::Value,
    sphere: ModelSphere,
) -> bool {
    // Streaming instantiates scene 0. Prove enclosure only for embedded, rigid geometry.
    document["scene"].as_u64() == Some(0)
        && ["animations", "skins"].into_iter().all(|key| {
            document
                .get(key)
                .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty))
        })
        && ["extensionsUsed", "extensionsRequired"]
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
        && encloses_scene(glb, sphere)
}

fn geometry_document(mut root: gltf::json::Root) -> Option<gltf::Document> {
    // gltf validates required renderer features too. Only geometry is consumed here.
    root.extensions_required
        .retain(|name| !material_or_uv_extension(name));
    // silent-ok: An invalid geometry document cannot certify sphere enclosure.
    gltf::Document::from_json(root).ok()
}

fn scene_fits(glb: &[u8], sphere: ModelSphere) -> Option<bool> {
    // silent-ok: A malformed GLB cannot certify sphere enclosure.
    let chunks = gltf::binary::Glb::from_slice(glb).ok()?;
    // silent-ok: A malformed scene description cannot certify sphere enclosure.
    let root = serde_json::from_slice(&chunks.json).ok()?;
    let gltf = geometry_document(root)?;
    let scene = gltf.default_scene()?;
    let blob = chunks.bin.as_deref()?;
    let buffer = |buffer: gltf::Buffer<'_>| (buffer.index() == 0).then_some(blob);
    let mut visited = vec![false; gltf.nodes().len()];
    let center = Vec3::from_array(sphere.center).as_dvec3();
    let mut pending: Vec<_> = scene
        .nodes()
        .map(|node| (node, Affine3A::IDENTITY))
        .collect();
    let mut seen_position = false;
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
        if !world.is_finite() || visited[node.index()] || node.skin().is_some() {
            return None;
        }
        visited[node.index()] = true;
        pending.extend(node.children().map(|child| (child, world)));
        let Some(mesh) = node.mesh() else { continue };
        let world = Mat4::from(world).as_dmat4();
        for primitive in mesh.primitives() {
            if primitive.morph_targets().len() != 0 {
                return None;
            }
            for position in primitive.reader(buffer).read_positions()? {
                let point = world.transform_point3(Vec3::from_array(position).as_dvec3());
                let distance = point.distance(center);
                if !distance.is_finite() || distance > f64::from(sphere.radius) {
                    return Some(false);
                }
                seen_position = true;
            }
        }
    }
    Some(seen_position)
}
