// Cluster shadow caster: the same vertex fetch as `cluster_draw.wgsl`, written to depth only.
//
// Group 0 is Bevy's prepass view bind group (the light's `View` uniform at binding 0), the one a
// stock shadow draw binds, so the cascade or cube face is rendered with the light's own matrices.
// Group 1 holds the pooled payload sections, the instance transforms and the light's own list of
// selected clusters; a shared index buffer supplies local indices, with no fragment stage (opaque casters
// write depth only).

#import bevy_render::view::View



struct Instance {
    world: mat4x4<f32>,
    mesh: u32,
    batch: u32,
    region_start: u32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> view: View;

@group(1) @binding(0) var<storage, read> positions: array<f32>;
@group(1) @binding(2) var<storage, read> cluster_vertices: array<u32>;
@group(1) @binding(4) var<storage, read> instances: array<Instance>;
@group(1) @binding(6) var<storage, read> visible: array<vec4<u32>>;

// Where the adapter cannot leave depth unclipped, a sun cascade clamps the caster's depth to the far
// side of the clip range and a fragment stage writes the depth it had, as the stock shadow pass
// does (`UNCLIPPED_DEPTH_ORTHO_EMULATION`): a caster in front of the cascade still shades behind it.
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    @location(0) unclipped_depth: f32,
#endif
}

#ifdef CLUSTER_VERTEX_PULL
// Naga rejects a literal division by zero; routing it through a function keeps the NaN.
fn divide(a: f32, b: f32) -> f32 {
    return a / b;
}

// A padding vertex of a pulled draw: a NaN position makes the whole triangle degenerate, so the
// rasterizer discards it (the same device Bevy's meshlet hardware raster uses).
fn dummy_vertex() -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(divide(0.0, 0.0));
    return out;
}
#endif

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) slot: u32,
) -> VertexOutput {
    let draw = visible[slot];
    // Sixteen-word MeshInfo rows prefix the reference pool (see mesh_info_row).
    let info = draw.y * 16u;
    let vertex_base = cluster_vertices[info];
    let vertex_ref_base = cluster_vertices[info + 2u];
    let cluster_count = cluster_vertices[info + 5u];
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
    let world = instances[draw.x].world * vec4<f32>(position, 1.0);
    var out: VertexOutput;
    out.position = view.clip_from_world * world;
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.unclipped_depth = out.position.z;
    out.position.z = min(out.position.z, 1.0);
#endif
    return out;
}

#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
struct FragmentOutput {
    @builtin(frag_depth) frag_depth: f32,
}

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    var out: FragmentOutput;
    out.frag_depth = in.unclipped_depth;
    return out;
}
#endif
