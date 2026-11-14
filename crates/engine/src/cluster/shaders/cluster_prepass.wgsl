// Cluster prepass vertex stage: the vertex fetch of `cluster_draw.wgsl`, written as Bevy's own
// prepass `VertexOutput`, so the material's stock prepass fragment stage (the one a stock mesh of
// the same material runs) writes the normal and the motion vector and discards by alpha. The
// cluster path's draws are then in the depth, normal and motion vector textures that ambient
// occlusion, temporal anti-aliasing and DLSS, and the water's reflections read.
//
// Group 0 is the camera's prepass view, group 2 the mesh uniform array (one entry per instance,
// the material slot included, as in the forward draw), group 3 the material, group 4 the pooled
// payload sections and this view's list of selected clusters.
//
// The instances are static, so an object's previous world position is its current one; the
// fragment stage turns that into the camera's motion from the previous frame's view.

#import bevy_pbr::{
    prepass_io::VertexOutput,
    mesh_functions,
    view_transformations::position_world_to_clip,
}


@group(4) @binding(0) var<storage, read> positions: array<f32>;
// Per vertex, four words: packed octahedral normal, packed UV, vertex colour (unorm8 RGBA), packed
// octahedral tangent (handedness in the low bit of the second component, set when negative).
@group(4) @binding(1) var<storage, read> attributes: array<u32>;
@group(4) @binding(3) var<storage, read> cluster_vertices: array<u32>;
@group(4) @binding(6) var<storage, read> visible: array<vec4<u32>>;

// Octahedral decode of two snorm16 values (ADR-0013 section 4, Cigolle et al. 2014).
fn decode_normal(packed: u32) -> vec3<f32> {
    let f = unpack2x16snorm(packed);
    var n = vec3<f32>(f.x, f.y, 1.0 - abs(f.x) - abs(f.y));
    if n.z < 0.0 {
        let folded = (1.0 - abs(vec2<f32>(n.y, n.x))) * select(vec2<f32>(-1.0), vec2<f32>(1.0), n.xy >= vec2<f32>(0.0));
        n = vec3<f32>(folded.x, folded.y, n.z);
    }
    return normalize(n);
}

#ifdef CLUSTER_VERTEX_PULL
// A padding vertex of a pulled draw: all three vertices of a padding triangle share one finite
// clip position, so the triangle has zero area and the rasterizer discards it. A NaN position would
// do the same under strict IEEE rules, but fast-math compilation may assume it never occurs.
fn dummy_vertex() -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    return out;
}
#endif

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) slot: u32,
) -> VertexOutput {
    let draw = visible[slot];
    let instance = draw.x;
    // Sixteen-word MeshInfo rows prefix the reference pool (see mesh_info_row).
    let info = draw.y * 16u;
    let vertex_base = cluster_vertices[info];
    let vertex_ref_base = cluster_vertices[info + 2u];
    let cluster_count = cluster_vertices[info + 5u];
    let uv_transform = bitcast<vec4<f32>>(vec4<u32>(
        cluster_vertices[info + 8u], cluster_vertices[info + 9u],
        cluster_vertices[info + 10u], cluster_vertices[info + 11u],
    ));
    // The mesh's vertex references start with one word per cluster: the offset of the cluster's
    // vertex list, which the references after the table are indexed by.
    let vertex_offset = cluster_vertices[vertex_ref_base + (draw.z & 0xffffffu)];
#ifdef CLUSTER_VERTEX_PULL
    // Vertex pulling (no draw count on the GPU, e.g. Metal): one non-indexed instanced draw covers a
    // group, every instance spans the scene's largest cluster, and the cluster-local index is read
    // here from the u16 index pool instead of arriving from an index buffer.
    let tri = vertex_index / 3u;
    let corner = vertex_index % 3u;
    let tri_count = draw.z >> 24u;
    if tri >= tri_count {
        return dummy_vertex();
    }
    // First index and offsets are in u16 units from the start of the merged buffer.
    let index_pos = draw.w + tri * 3u + corner;
    let word = cluster_vertices[index_pos >> 1u];
    let local = (word >> ((index_pos & 1u) * 16u)) & 0xffffu;
#else
    let local = vertex_index;
#endif
    let source = vertex_base + cluster_vertices[vertex_ref_base + cluster_count + vertex_offset + local];

    let position = vec3<f32>(
        positions[source * 3u],
        positions[source * 3u + 1u],
        positions[source * 3u + 2u],
    );

    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(instance);
    out.world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(position, 1.0),
    );
    out.position = position_world_to_clip(out.world_position.xyz);
#ifdef NORMAL_PREPASS_OR_DEFERRED_PREPASS
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        decode_normal(attributes[source * 4u]),
        instance,
    );
#ifdef VERTEX_TANGENTS
    let tangent_word = attributes[source * 4u + 3u];
    let tangent_negative = (tangent_word & 0x10000u) != 0u;
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        world_from_local,
        vec4<f32>(decode_normal(tangent_word & 0xfffeffffu), select(1.0, -1.0, tangent_negative)),
        instance,
    );
#endif
#endif
#ifdef MOTION_VECTOR_PREPASS
    out.previous_world_position = out.world_position;
#endif
#ifdef VERTEX_UVS_A
    out.uv = uv_transform.xy + unpack2x16unorm(attributes[source * 4u + 1u]) * uv_transform.zw;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = uv_transform.xy + unpack2x16unorm(attributes[source * 4u + 1u]) * uv_transform.zw;
#endif
#ifdef VERTEX_COLORS
    out.color = unpack4x8unorm(attributes[source * 4u + 2u]);
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = instance;
#endif
    return out;
}
