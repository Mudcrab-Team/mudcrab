// Cluster shadow caster for alpha-masked materials: the vertex fetch of `cluster_draw.wgsl`, written
// as Bevy's own prepass `VertexOutput`, so the stock prepass fragment stage of the material (the
// one a stock shadow draw of the same material runs) tests the base colour alpha against the
// cutoff and discards. Opaque casters keep the depth-only `cluster_shadow.wgsl`.
//
// Group 0 is the light's prepass view, group 2 the mesh uniform array (one entry per instance, the
// material slot included, as in the forward draw), group 3 the material, group 4 the pooled
// payload sections and this view's list of selected clusters.

#import bevy_pbr::{
    prepass_io::VertexOutput,
    mesh_functions,
    view_transformations::position_world_to_clip,
}


@group(4) @binding(0) var<storage, read> positions: array<f32>;
// Per vertex, four words: packed octahedral normal, packed UV, vertex colour (unorm8 RGBA), packed
// octahedral tangent.
@group(4) @binding(1) var<storage, read> attributes: array<u32>;
@group(4) @binding(3) var<storage, read> cluster_vertices: array<u32>;
@group(4) @binding(6) var<storage, read> visible: array<vec4<u32>>;

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
    let vertex_offset = cluster_vertices[vertex_ref_base + draw.z];
    let local = vertex_index;
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
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.unclipped_depth = out.position.z;
    out.position.z = min(out.position.z, 1.0);
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
