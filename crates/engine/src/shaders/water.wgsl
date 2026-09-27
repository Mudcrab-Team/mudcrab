#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}
#import bevy_pbr::mesh_view_bindings::view

struct WaterSettings {
    wave_scale_speed_strength: vec4<f32>,
    flow_direction: vec4<f32>,
    // x = Fresnel Amount (Schlick F0), y = Reflectivity Amount (WATR DNAM, per-water; falls back
    // to Skyrim's DefaultWater 0.10 / 0.8 when a water has no decoded colours). z/w unused.
    fresnel_reflectivity: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> water: WaterSettings;

@group(#{MATERIAL_BIND_GROUP}) @binding(101) var water_reflection: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var water_reflection_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var water_flow_normal: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var water_flow_normal_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    let scale = water.wave_scale_speed_strength.x;
    let phase = water.wave_scale_speed_strength.w * water.wave_scale_speed_strength.y;
    let strength = water.wave_scale_speed_strength.z;
    let p = in.world_position.xz * scale + water.flow_direction.xy * phase;
    var dx = cos(p.x + sin(p.y * 1.7)) * strength;
    var dz = cos(p.y * 1.3 + sin(p.x)) * strength;
    if water.flow_direction.w > 0.5 {
        let flow = textureSample(water_flow_normal, water_flow_normal_sampler, fract(p * 0.25)).xy * 2.0 - 1.0;
        dx += flow.x * strength;
        dz += flow.y * strength;
    }
    pbr_input.N = normalize(vec3<f32>(-dx, 1.0, -dz));
    let viewport_uv = (in.position.xy - view.viewport.xy) / view.viewport.zw;
    let reflection_uv = clamp(vec2<f32>(viewport_uv.x, 1.0 - viewport_uv.y), vec2<f32>(0.0), vec2<f32>(1.0));
    let reflection_color = textureSample(water_reflection, water_reflection_sampler, reflection_uv);
    // Schlick with F0 = this water's WATR "Fresnel Amount" (DNAM; DefaultWater is 0.10 after
    // Update.esm).
    let water_fresnel_f0 = water.fresnel_reflectivity.x;
    let n_dot_v = clamp(dot(normalize(pbr_input.V), pbr_input.N), 0.0, 1.0);
    let fresnel = water_fresnel_f0 + (1.0 - water_fresnel_f0) * pow(1.0 - n_dot_v, 5.0);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    var out: FragmentOutput;
    // The water's own (dark) colour is lit; the reflection is light that already left the scene, so
    // it is blended in after lighting rather than lit a second time as albedo, which washed the
    // water out to white.
    out.color = apply_pbr_lighting(pbr_input);
    let reflected = fresnel * water.fresnel_reflectivity.y;
    out.color = vec4<f32>(mix(out.color.rgb, reflection_color.rgb, reflected), max(out.color.a, fresnel));
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
