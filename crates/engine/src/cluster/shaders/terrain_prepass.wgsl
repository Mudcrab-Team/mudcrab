// Prepass and shadow fragment stage of the cluster terrain material. It is Bevy 0.19's
// `pbr_prepass.wgsl` fragment stage with one addition: after the stock alpha test, fragments over
// quadrants the stock terrain draws (or in a worldspace that is not active) are discarded, by the
// same function the forward stage uses (`terrain_clip.wgsl`). Without it the depth, normal and
// motion-vector textures and the shadow maps would still carry the clipped terrain.
//
// The terrain atlas material has no normal map, so the stock stage's normal-mapping branch
// (`STANDARD_MATERIAL_NORMAL_MAP`) is not reproduced here; the vertex normal is written. The meshlet
// branches are left out too. If the material ever gains a normal map, this file has to follow
// `pbr_prepass.wgsl` again.
//
// The material is an alpha mask with cutoff 0 (`cluster/terrain.rs`), which is what makes Bevy
// compile this stage into the depth-only and shadow pipelines (`MAY_DISCARD`) instead of skipping
// the fragment stage for an opaque material.

#import bevy_pbr::{
    pbr_prepass_functions,
    pbr_bindings,
    pbr_types,
    pbr_functions,
    prepass_io,
    mesh_bindings::mesh,
}
#import cluster_terrain::clip::terrain_clip

#ifdef BINDLESS
#import bevy_pbr::pbr_bindings::material_indices
#endif  // BINDLESS

#ifdef PREPASS_FRAGMENT
@fragment
fn fragment(
    in: prepass_io::VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> prepass_io::FragmentOutput {
#ifdef BINDLESS
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    let flags = pbr_bindings::material_array[material_indices[slot].material].flags;
#else   // BINDLESS
    let flags = pbr_bindings::material.flags;
#endif  // BINDLESS

#ifdef VISIBILITY_RANGE_DITHER
    pbr_functions::visibility_range_dither(in.position, in.visibility_range_dither);
#endif  // VISIBILITY_RANGE_DITHER

    pbr_prepass_functions::prepass_alpha_discard(in);
    if terrain_clip(in.world_position, in.instance_index, 10u) {
        discard;
    }

    var out: prepass_io::FragmentOutput;

#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.frag_depth = in.unclipped_depth;
#endif // UNCLIPPED_DEPTH_ORTHO_EMULATION

#ifdef NORMAL_PREPASS
    if (flags & pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        let double_sided = (flags & pbr_types::STANDARD_MATERIAL_FLAGS_DOUBLE_SIDED_BIT) != 0u;
        let world_normal = pbr_functions::prepare_world_normal(
            in.world_normal,
            double_sided,
            is_front,
        );
        out.normal = vec4(world_normal * 0.5 + vec3(0.5), 1.0);
    } else {
        out.normal = vec4(in.world_normal * 0.5 + vec3(0.5), 1.0);
    }
#endif // NORMAL_PREPASS

#ifdef MOTION_VECTOR_PREPASS
    out.motion_vector = pbr_prepass_functions::calculate_motion_vector(
        in.world_position,
        in.previous_world_position,
    );
#endif

    return out;
}
#else
@fragment
fn fragment(in: prepass_io::VertexOutput) {
    pbr_prepass_functions::prepass_alpha_discard(in);
    if terrain_clip(in.world_position, in.instance_index, 10u) {
        discard;
    }
}
#endif // PREPASS_FRAGMENT
