// Forward fragment stage of the cluster terrain material: Bevy's standard material, plus the
// near-stock clip (`terrain_clip.wgsl`). The lighting, the fog and the tone path are the stock
// ones; the only additions are that the alpha is forced opaque (the material is an alpha mask with
// a cutoff of 0 only so that the prepass and shadow passes run a fragment stage, see
// `cluster/terrain.rs`) and that fragments over quadrants the stock terrain draws are discarded.
//
// The clip comes after the base-colour sample and the stock alpha test, as `snow.wgsl` places its
// own work: a `discard` before a derivative-using texture sample would break its uniformity.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}
#import cluster_terrain::clip::terrain_clip

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    if terrain_clip(in.world_position, in.instance_index, 0u) {
        discard;
    }
    pbr_input.material.base_color.a = 1.0;
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
