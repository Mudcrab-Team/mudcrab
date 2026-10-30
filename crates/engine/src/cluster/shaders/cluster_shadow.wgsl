// Cluster shadow caster: the same vertex fetch as `cluster_draw.wgsl`, written to depth only.
//
// Group 0 is Bevy's prepass view bind group (the light's `View` uniform at binding 0), the one a
// stock shadow draw binds, so the cascade or cube face is rendered with the light's own matrices.
// Group 1 holds the pooled payload sections, the instance transforms and the light's own list of
// selected clusters; a shared index buffer supplies local indices, with no fragment stage (opaque casters
// write depth only).

#import bevy_render::view::View

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

struct Instance {
    world: mat4x4<f32>,
    mesh: u32,
    batch: u32,
    region_start: u32,
    pad0: u32,
}

@group(0) @binding(0) var<uniform> view: View;

@group(1) @binding(0) var<storage, read> positions: array<f32>;
@group(1) @binding(1) var<storage, read> clusters: array<u32>;
@group(1) @binding(2) var<storage, read> cluster_vertices: array<u32>;
@group(1) @binding(4) var<storage, read> instances: array<Instance>;
@group(1) @binding(5) var<storage, read> meshes: array<MeshInfo>;
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

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) slot: u32,
) -> VertexOutput {
    let draw = visible[slot];
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
