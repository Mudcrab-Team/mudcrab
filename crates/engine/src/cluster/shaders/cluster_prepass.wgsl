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

struct MeshInfo {
    vertex_base: u32,
    cluster_base: u32,
    vertex_ref_base: u32,
    index_base: u32,
    group_base: u32,
    cluster_count: u32,
    pad0: u32,
    pad1: u32,
    // uv_min (xy), uv_scale (zw)
    uv: vec4<f32>,
    // Whole-mesh bounding sphere, read by the cull pass.
    bounds: vec4<f32>,
}

@group(4) @binding(0) var<storage, read> positions: array<f32>;
// Per vertex, four words: packed octahedral normal, packed UV, vertex colour (unorm8 RGBA), packed
// octahedral tangent (handedness in the low bit of the second component, set when negative).
@group(4) @binding(1) var<storage, read> attributes: array<u32>;
@group(4) @binding(2) var<storage, read> clusters: array<u32>;
@group(4) @binding(3) var<storage, read> cluster_vertices: array<u32>;
@group(4) @binding(5) var<storage, read> meshes: array<MeshInfo>;
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

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) slot: u32,
) -> VertexOutput {
    let draw = visible[slot];
    let instance = draw.x;
    let mesh = meshes[draw.y];
    let base = (mesh.cluster_base + draw.z) * 12u;
    let vertex_offset = clusters[base + 6u];
    let local = vertex_index;
    let source = mesh.vertex_base + cluster_vertices[mesh.vertex_ref_base + vertex_offset + local];

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
    out.uv = mesh.uv.xy + unpack2x16unorm(attributes[source * 4u + 1u]) * mesh.uv.zw;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = mesh.uv.xy + unpack2x16unorm(attributes[source * 4u + 1u]) * mesh.uv.zw;
#endif
#ifdef VERTEX_COLORS
    out.color = unpack4x8unorm(attributes[source * 4u + 2u]);
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = instance;
#endif
    return out;
}
