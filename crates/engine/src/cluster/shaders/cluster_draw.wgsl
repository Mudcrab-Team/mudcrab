// Cluster vertex stage: the index buffer supplies a cluster-local vertex index; its vertex reference
// and vertex data come from storage buffers holding the payload sections as stored. The
// output is Bevy's own forward `VertexOutput`, so the stock pbr fragment shader and the stock
// material bind group draw it unchanged.
//
// `instance_index` is `first_instance` of the indirect draw: the draw's place in the compacted
// list, where the cull pass wrote its instance, mesh and cluster. Meshes are pooled in shared
// buffers, so every offset is the mesh's base from the `meshes` table. The stock fragment reads
// `mesh[in.instance_index]`, so the mesh uniform array bound at group 2 holds one entry per
// instance and the output instance index is the instance only.

#import bevy_pbr::{
    forward_io::VertexOutput,
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
    let normal = decode_normal(attributes[source * 4u]);
    let uv = mesh.uv.xy + unpack2x16unorm(attributes[source * 4u + 1u]) * mesh.uv.zw;
    let colour = unpack4x8unorm(attributes[source * 4u + 2u]);
    let tangent_word = attributes[source * 4u + 3u];
    let tangent_negative = (tangent_word & 0x10000u) != 0u;
    let tangent_direction = decode_normal(tangent_word & 0xfffeffffu);
    let tangent = vec4<f32>(tangent_direction, select(1.0, -1.0, tangent_negative));

    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(instance);
    out.world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(position, 1.0),
    );
    out.position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(normal, instance);
#ifdef VERTEX_UVS_A
    out.uv = uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = uv;
#endif
#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        world_from_local,
        tangent,
        instance,
    );
#endif
#ifdef VERTEX_COLORS
    out.color = colour;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = instance;
#endif
    return out;
}
