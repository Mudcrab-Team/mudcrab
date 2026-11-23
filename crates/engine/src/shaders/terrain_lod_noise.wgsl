#import engine::terrain_blend::noisy_land_colour

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var land_noise: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var land_noise_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var input = pbr_input_from_standard_material(in, is_front);
#ifdef VERTEX_UVS_A
#ifdef STANDARD_MATERIAL_BASE_COLOR_UV_B
    let base_uv = (input.material.uv_transform * vec3<f32>(in.uv_b, 1.0)).xy;
#else
    let base_uv = (input.material.uv_transform * vec3<f32>(in.uv, 1.0)).xy;
#endif
    let base = input.material.base_color;
    input.material.base_color = vec4<f32>(noisy_land_colour(base.rgb, land_noise, land_noise_sampler, base_uv), base.a);
#endif
    input.material.base_color = alpha_discard(input.material, input.material.base_color);
    var out: FragmentOutput;
    if (input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(input);
    } else {
        out.color = input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(input, out.color);
    return out;
}
