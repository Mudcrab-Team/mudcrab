//! Builds the `.mcl` payload of one primitive from its geometry (ADR-0013 sections 2 and 4).

use super::model::{ModelPrimitive, Reject, RejectReason};
use meshopt_cluster::{Attributes, Config, Error as ClusterError, Mesh};
use serde_json::{Value, json};
use shared::cluster::{
    BvhNode, ClusterFormatError, ClusterRecord, GroupRecord, MclContents, NO_GROUP, TERMINAL_ERROR,
    encode_oct_snorm16, encode_uv_unorm16, write_mcl,
};

/// Triangles per cluster. Both clusterlod presets cap at 128 triangles.
pub const MAX_CLUSTER_TRIANGLES: usize = 128;
/// Coarser levels above each mesh's root cluster, down to about this many triangles (see
/// `meshopt_cluster::Config::tail_min_triangles`): a distant instance then draws a handful of
/// triangles instead of a whole root cluster.
pub const TAIL_MIN_TRIANGLES: usize = 8;
/// Width of the BVH over groups.
pub const BVH_NODE_WIDTH: usize = 4;
/// Attribute weights for attribute-aware simplification: normal xyz, uv.
pub const ATTRIBUTE_WEIGHTS: [f32; 5] = [0.5, 0.5, 0.5, 0.5, 0.5];
/// Revision of the Rust and C++ glue that is not part of the vendored sources: bump it when the
/// way the builder calls clusterlod changes. It is part of the builder identity.
pub const WRAPPER_REVISION: u32 = 2;

/// The builder settings as the canonical JSON object stored in the identity (ADR-0013 section 1).
pub fn builder_config_json(wrapper_source_sha256: &str, rust_sources_sha256: &str) -> Value {
    json!({
        "preset": "classic",
        "material_certification_version": shared::cluster_material::CERTIFICATION_VERSION,
        "tangent_generation": "bevy_mikktspace-1.0.0-shared-leaf-proof-v1",
        "max_triangles": MAX_CLUSTER_TRIANGLES,
        "tail_min_triangles": TAIL_MIN_TRIANGLES,
        "bvh_node_width": BVH_NODE_WIDTH,
        "attribute_weights": ATTRIBUTE_WEIGHTS,
        "attribute_protect_mask": 0,
        "attributes": ["normal.x", "normal.y", "normal.z", "uv.x", "uv.y"],
        "wrapper_revision": WRAPPER_REVISION,
        "wrapper_source": wrapper_source_sha256,
        "rust_sources": rust_sources_sha256,
    })
}

/// What one built payload holds, for the report and the database row.
#[derive(Debug, Clone, Copy, Default)]
pub struct MeshStats {
    pub source_triangles: u64,
    pub vertex_count: u32,
    pub cluster_count: u32,
    pub group_count: u32,
    pub depth: u32,
    pub payload_bytes: u64,
    /// Groups whose error had to be raised to dominate their input clusters (clusterlod's own
    /// output was not monotonic). Zero is expected; the report counts it.
    pub monotonic_fixups: u32,
    pub root_error: f32,
    /// Whole-mesh bounding sphere: centre xyz, radius.
    pub bounds: [f32; 4],
}

pub struct BuiltMesh {
    pub bytes: Vec<u8>,
    pub stats: MeshStats,
}

/// Builds the payload for `primitive`, or the reason it cannot be built.
pub fn build_mesh_payload(
    primitive: &ModelPrimitive,
    builder_identity: [u8; 32],
    mesh_hash: [u8; 32],
) -> Result<BuiltMesh, Reject> {
    build_payload(primitive, builder_identity, mesh_hash, None)
}

/// Certify the encoded geometry that runtime tangent preparation actually reads.
/// Authored MCL tangents already take runtime's direct packed-stream path. Without
/// them, MikkTSpace must prove every leaf corner; Bevy's default for an absent
/// tangent is diagnostic geometry, never permission to certify a normal map.
pub(super) fn certify_material_geometry(bytes: &[u8], material: &Value) -> Result<(), Reject> {
    use shared::cluster_material::{GeometryField, geometry_requirements};
    let requirements = geometry_requirements(
        material
            .get("normalTexture")
            .filter(|texture| !texture.is_null())
            .into_iter()
            .flat_map(|texture| {
                [
                    GeometryField::PresentTexture("normal_map_texture"),
                    GeometryField::Channel {
                        uv0: texture.get("texCoord").map_or(Some(0), Value::as_u64) == Some(0),
                        texture_present: Some(true),
                    },
                ]
            }),
    );
    if !requirements.supported {
        return Err(Reject::new(
            RejectReason::UnsupportedMaterialFeature,
            "normal map requires UV0",
        ));
    }
    if !requirements.tangents {
        return Ok(());
    }
    let view = shared::cluster::read_mcl(bytes).map_err(format_error)?;
    if view.flags & shared::cluster::FLAG_TANGENTS != 0 {
        return Ok(());
    }
    if !shared::cluster_tangents::generate_mcl_tangents(&view, |_, _| {}) {
        return Err(Reject::new(
            RejectReason::MissingAttributes,
            "normal map has no usable generated tangents in encoded MCL leaf geometry",
        ));
    }
    Ok(())
}

/// Like [`build_mesh_payload`], with one lock byte per vertex: a non-zero byte keeps that vertex
/// where it is at every level, so a border shared with another mesh (a terrain block's edge)
/// stays identical there. `locks` must be as long as the primitive's vertex count.
pub fn build_mesh_payload_locked(
    primitive: &ModelPrimitive,
    builder_identity: [u8; 32],
    mesh_hash: [u8; 32],
    locks: &[u8],
) -> Result<BuiltMesh, Reject> {
    // The terrain adapter carries reason bits; meshoptimizer requires the Lock bit specifically.
    let locks: Vec<u8> = locks.iter().map(|&reason| u8::from(reason != 0)).collect();
    build_payload(primitive, builder_identity, mesh_hash, Some(&locks))
}

fn build_payload(
    primitive: &ModelPrimitive,
    builder_identity: [u8; 32],
    mesh_hash: [u8; 32],
    vertex_lock: Option<&[u8]>,
) -> Result<BuiltMesh, Reject> {
    let geometry = &primitive.geometry;
    let attributes: Vec<f32> = geometry
        .normals
        .iter()
        .zip(&geometry.uvs)
        .flat_map(|(normal, uv)| [normal[0], normal[1], normal[2], uv[0], uv[1]])
        .collect();
    let hierarchy = meshopt_cluster::build(
        &Mesh {
            positions: &geometry.positions,
            indices: &geometry.indices,
            attributes: Some(Attributes {
                data: &attributes,
                stride: 5,
                weights: &ATTRIBUTE_WEIGHTS,
                protect_mask: 0,
            }),
            vertex_lock,
        },
        Config::classic(MAX_CLUSTER_TRIANGLES).with_tail(TAIL_MIN_TRIANGLES),
    )
    .map_err(|error: ClusterError| Reject::new(RejectReason::BuilderFailed, error.to_string()))?;
    drop(attributes);

    let source_clusters = hierarchy.clusters();
    let source_groups = hierarchy.groups();
    if source_clusters.is_empty() {
        return Err(Reject::new(
            RejectReason::BuilderFailed,
            "no clusters built",
        ));
    }
    if source_clusters.len() > usize::from(u16::MAX) || source_groups.len() > usize::from(u16::MAX)
    {
        return Err(Reject::new(
            RejectReason::TooManyClusters,
            format!(
                "{} clusters, {} groups",
                source_clusters.len(),
                source_groups.len()
            ),
        ));
    }

    // Group errors, raised where needed so they dominate every input cluster's error: a
    // cluster's error is its producing group's error, and groups merge clusters of lower depth.
    let mut group_error: Vec<f32> = source_groups.iter().map(|g| g.bounds.error).collect();
    let mut order: Vec<usize> = (0..source_groups.len()).collect();
    order.sort_by_key(|&group| (source_groups[group].depth, group));
    let mut monotonic_fixups = 0u32;
    for &group in &order {
        if group_error[group] == TERMINAL_ERROR {
            continue;
        }
        let mut required = group_error[group];
        for cluster in &source_clusters[source_groups[group].clusters.clone()] {
            if let Some(producer) = cluster.refined {
                required = required.max(group_error[producer]);
            }
        }
        if required > group_error[group] {
            group_error[group] = required;
            monotonic_fixups += 1;
        }
    }

    let mut cluster_group = vec![0u16; source_clusters.len()];
    let mut groups = Vec::with_capacity(source_groups.len());
    for (index, group) in source_groups.iter().enumerate() {
        for cluster in group.clusters.clone() {
            cluster_group[cluster] = index as u16;
        }
        groups.push(GroupRecord {
            bounds: [
                group.bounds.center[0],
                group.bounds.center[1],
                group.bounds.center[2],
                group.bounds.radius,
            ],
            error: group_error[index],
            depth: group.depth as u16,
            first_cluster: group.clusters.start as u16,
            cluster_count: group.clusters.len() as u16,
        });
    }

    // The vertex subset any cluster references, in source order.
    let source_vertex_count = geometry.positions.len();
    let mut remap = vec![u32::MAX; source_vertex_count];
    let mut locals = Vec::with_capacity(source_clusters.len());
    for cluster in source_clusters {
        let local = cluster.local_geometry();
        for &vertex in &local.vertices {
            remap[vertex as usize] = 0;
        }
        locals.push(local);
    }
    let mut used: Vec<u32> = Vec::new();
    for (vertex, slot) in remap.iter_mut().enumerate() {
        if *slot != u32::MAX {
            *slot = used.len() as u32;
            used.push(vertex as u32);
        }
    }

    let mut clusters = Vec::with_capacity(source_clusters.len());
    let mut cluster_vertices: Vec<u32> = Vec::new();
    let mut cluster_triangles: Vec<[u8; 3]> = Vec::new();
    let mut root_error = 0.0f32;
    for (index, (cluster, local)) in source_clusters.iter().zip(&locals).enumerate() {
        let triangle_count = local.triangles.len() / 3;
        if local.vertices.len() > usize::from(u8::MAX) || triangle_count > usize::from(u8::MAX) {
            return Err(Reject::new(
                RejectReason::BuilderFailed,
                format!("cluster {index} exceeds the u8 record counts"),
            ));
        }
        let group = cluster_group[index];
        let (error, refined_group) = match cluster.refined {
            Some(producer) => (group_error[producer], producer as u16),
            None => (0.0, NO_GROUP),
        };
        if error < TERMINAL_ERROR {
            root_error = root_error.max(error);
        }
        clusters.push(ClusterRecord {
            bounds: [
                cluster.bounds.center[0],
                cluster.bounds.center[1],
                cluster.bounds.center[2],
                cluster.bounds.radius,
            ],
            error,
            parent_error: group_error[usize::from(group)],
            vertex_offset: cluster_vertices.len() as u32,
            triangle_offset: cluster_triangles.len() as u32,
            vertex_count: local.vertices.len() as u8,
            triangle_count: triangle_count as u8,
            group,
            refined_group,
        });
        cluster_vertices.extend(local.vertices.iter().map(|&vertex| remap[vertex as usize]));
        cluster_triangles.extend(local.triangles.as_chunks::<3>().0.iter().copied());
    }

    let mut bvh = hierarchy
        .build_bvh(BVH_NODE_WIDTH)
        .map_err(|error| Reject::new(RejectReason::BuilderFailed, error.to_string()))?
        .nodes()
        .iter()
        .map(|node| BvhNode {
            bounds: [
                node.bounds.center[0],
                node.bounds.center[1],
                node.bounds.center[2],
                node.bounds.radius,
            ],
            error: node.bounds.error,
            first_child: node.children.start as u32,
            child_count: node.children.len() as u16,
            group: node.group.map_or(NO_GROUP, |group| group as u16),
        })
        .collect::<Vec<_>>();
    // Node errors follow the group errors as raised above: a leaf carries its group's error and
    // an internal node the largest error below it, so pruning by node error never skips a group
    // that selection by group error would draw.
    for node in &mut bvh {
        if node.group != NO_GROUP {
            node.error = group_error[usize::from(node.group)];
        }
    }
    loop {
        let mut changed = false;
        for index in 0..bvh.len() {
            if bvh[index].group != NO_GROUP {
                continue;
            }
            let first = bvh[index].first_child as usize;
            let last = first + usize::from(bvh[index].child_count);
            let below = bvh[first..last]
                .iter()
                .map(|child| child.error)
                .fold(0.0f32, f32::max);
            if below > bvh[index].error || bvh[index].error > below {
                bvh[index].error = below;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Vertex streams over the used subset.
    let positions: Vec<[f32; 3]> = used
        .iter()
        .map(|&v| geometry.positions[v as usize])
        .collect();
    let normals: Vec<[i16; 2]> = used
        .iter()
        .map(|&v| encode_oct_snorm16(geometry.normals[v as usize]))
        .collect();
    let (mut uv_min, mut uv_max) = ([f32::MAX; 2], [f32::MIN; 2]);
    for &vertex in &used {
        for axis in 0..2 {
            uv_min[axis] = uv_min[axis].min(geometry.uvs[vertex as usize][axis]);
            uv_max[axis] = uv_max[axis].max(geometry.uvs[vertex as usize][axis]);
        }
    }
    let uv_scale = [uv_max[0] - uv_min[0], uv_max[1] - uv_min[1]];
    let uvs: Vec<[u16; 2]> = used
        .iter()
        .map(|&v| encode_uv_unorm16(geometry.uvs[v as usize], uv_min, uv_scale))
        .collect();
    let tangents: Option<Vec<[i16; 2]>> = geometry.tangents.as_ref().map(|tangents| {
        used.iter()
            .map(|&v| {
                let tangent = tangents[v as usize];
                let mut encoded = encode_oct_snorm16([tangent[0], tangent[1], tangent[2]]);
                encoded[1] = (encoded[1] & !1) | i16::from(tangent[3] < 0.0);
                encoded
            })
            .collect()
    });
    let colours: Option<Vec<[u8; 4]>> = geometry.colours.as_ref().map(|colours| {
        used.iter()
            .map(|&v| colours[v as usize].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8))
            .collect()
    });

    let mesh_bounds = sphere_of(&positions);
    let depth = groups.iter().map(|group| group.depth).max().unwrap_or(0);
    let bytes = write_mcl(&MclContents {
        builder_identity,
        mesh_hash,
        primitive_index: primitive.index,
        root_error,
        bounds: mesh_bounds,
        uv_min: if uv_min[0] > uv_max[0] {
            [0.0; 2]
        } else {
            uv_min
        },
        uv_scale: if uv_min[0] > uv_max[0] {
            [0.0; 2]
        } else {
            uv_scale
        },
        positions: &positions,
        normals: &normals,
        uvs: &uvs,
        tangents: tangents.as_deref(),
        colours: colours.as_deref(),
        clusters: &clusters,
        cluster_vertices: &cluster_vertices,
        cluster_triangles: &cluster_triangles,
        groups: &groups,
        bvh: &bvh,
    })
    .map_err(format_error)?;
    // The reader's checks are the contract: a payload this converter writes must pass them.
    shared::cluster::read_mcl(&bytes).map_err(format_error)?;

    Ok(BuiltMesh {
        stats: MeshStats {
            source_triangles: geometry.triangle_count() as u64,
            vertex_count: positions.len() as u32,
            cluster_count: clusters.len() as u32,
            group_count: groups.len() as u32,
            depth: u32::from(depth),
            payload_bytes: bytes.len() as u64,
            monotonic_fixups,
            root_error,
            bounds: mesh_bounds,
        },
        bytes,
    })
}

fn format_error(error: ClusterFormatError) -> Reject {
    let reason = match error {
        ClusterFormatError::TooManyClusters(_) => RejectReason::TooManyClusters,
        ClusterFormatError::NonMonotonicError(_) => RejectReason::NonMonotonicError,
        _ => RejectReason::BuilderFailed,
    };
    Reject::new(reason, error.to_string())
}

/// Bounding sphere around the bounding box centre.
fn sphere_of(positions: &[[f32; 3]]) -> [f32; 4] {
    if positions.is_empty() {
        return [0.0; 4];
    }
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for position in positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    let centre = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let radius = positions
        .iter()
        .map(|p| {
            let d = [p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
        })
        .fold(0.0f32, f32::max);
    [centre[0], centre[1], centre[2], radius]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::model::{AlphaMode, PrimitiveGeometry};
    use shared::cluster::read_mcl;

    /// A closed-ish grid terrain of `n * n` quads with gentle noise, `2 n^2` triangles.
    pub(crate) fn grid_primitive(n: usize) -> ModelPrimitive {
        let side = n + 1;
        let mut geometry = PrimitiveGeometry::default();
        for y in 0..side {
            for x in 0..side {
                let height = ((x * 7 + y * 13) % 5) as f32 * 0.05;
                geometry.positions.push([x as f32, y as f32, height]);
                geometry.normals.push([0.0, 0.0, 1.0]);
                geometry
                    .uvs
                    .push([x as f32 / n as f32 * 4.0, y as f32 / n as f32 * 4.0]);
            }
        }
        for y in 0..n {
            for x in 0..n {
                let a = (y * side + x) as u32;
                let (b, c, d) = (a + 1, a + side as u32, a + side as u32 + 1);
                geometry.indices.extend([a, b, c, b, d, c]);
            }
        }
        ModelPrimitive {
            index: 0,
            geometry,
            alpha: AlphaMode::Opaque,
            alpha_cutoff: None,
            double_sided: false,
            material_key: "m".into(),
            material_index: Some(0),
            bounds: ([0.0; 3], [n as f32; 3]),
            reject: None,
        }
    }

    #[test]
    fn builds_a_valid_deterministic_payload() {
        let primitive = grid_primitive(48);
        let first = build_mesh_payload(&primitive, [1; 32], [2; 32]).unwrap_or_else(|r| {
            panic!("{}: {}", r.reason, r.detail);
        });
        let second = build_mesh_payload(&primitive, [1; 32], [2; 32])
            .ok()
            .unwrap();
        assert_eq!(first.bytes, second.bytes);
        let view = read_mcl(&first.bytes).unwrap();
        assert!(view.cluster_count > 1 && view.group_count > 1);
        assert_eq!(first.stats.source_triangles, 48 * 48 * 2);
        assert!(first.stats.depth >= 1);
        assert!(view.root_error > 0.0);
        assert!(view.bvh_node_count > 0);
        // Every source triangle is covered by the level 0 clusters.
        let level0: usize = (0..view.cluster_count as usize)
            .map(|i| view.cluster(i))
            .filter(|c| c.refined_group == NO_GROUP)
            .map(|c| usize::from(c.triangle_count))
            .sum();
        assert_eq!(level0 as u64, first.stats.source_triangles);
    }

    #[test]
    fn identity_and_hash_land_in_the_header() {
        let primitive = grid_primitive(16);
        let built = build_mesh_payload(&primitive, [7; 32], [9; 32])
            .ok()
            .unwrap();
        let view = read_mcl(&built.bytes).unwrap();
        assert_eq!(view.builder_identity, [7; 32]);
        assert_eq!(view.mesh_hash, [9; 32]);
    }
}
