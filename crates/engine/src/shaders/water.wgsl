#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    view_transformations::{depth_ndc_to_view_z, position_world_to_view},
}
#import bevy_pbr::mesh_view_bindings::view
#ifdef DEPTH_PREPASS
#import bevy_pbr::prepass_utils::prepass_depth
#endif

struct WaterSettings {
    wave_scale_speed_strength: vec4<f32>,
    flow_direction: vec4<f32>,
    // x = Fresnel Amount (Schlick F0), y = Reflectivity Amount (WATR DNAM, per-water; falls back
    // to Skyrim's DefaultWater 0.10 / 0.8 when a water has no decoded colours).
    // z = interactive reflection multiplier, w unused.
    fresnel_reflectivity: vec4<f32>,
    // rgb = the water's Reflection Color (linear): what it reflects where the reflection image has
    // no coverage (alpha 0). Vanilla fades the reflection to this colour as reflectivity drops.
    reflection_color: vec4<f32>,
    // rgb = Shallow Color (linear); the deep colour is the material base colour.
    shallow_color: vec4<f32>,
    // x = opacity 0..1 (WATR ANAM), used where the depth is unknown; y, z = above-water fog near
    // and far distance in world units; w = fog amount, the curve exponent (WATR DNAM).
    body: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> water: WaterSettings;

@group(#{MATERIAL_BIND_GROUP}) @binding(101) var water_reflection: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var water_reflection_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var water_flow_normal: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var water_flow_normal_sampler: sampler;

// Clamp on the distance the view travels through water; anything longer is treated as bottomless.
const MAX_WATER_PATH: f32 = 16384.0;

// How far, in world units, the view ray travels through water before it reaches whatever opaque
// lies behind this fragment; negative when that is unknown because there is no depth prepass.
fn water_path_length(frag_coord: vec4<f32>, world_position: vec3<f32>, sample_index: u32) -> f32 {
#ifdef DEPTH_PREPASS
    let ndc = prepass_depth(frag_coord, sample_index);
    if ndc <= 0.0 {
        // Nothing opaque behind the water (reverse-Z: 0 is infinitely far).
        return MAX_WATER_PATH;
    }
    let bed_view_z = depth_ndc_to_view_z(ndc);
    let surface = position_world_to_view(world_position);
    // The surface and the bed lie on one view ray, so the bed is that ray scaled by the ratio of
    // their view depths (both negative in front of the camera); the part past the surface is water.
    let past_surface = max(bed_view_z / min(surface.z, -1e-4) - 1.0, 0.0);
    return min(length(surface) * past_surface, MAX_WATER_PATH);
#else
    return -1.0;
#endif
}

// x = how far the colour has moved from shallow to deep, y = the body's opacity, both 0..1.
// This is Skyrim's above-water fog: the colour moves linearly until the far distance, and the
// body clouds from clear at the near distance to opaque at the far one along a curve whose
// exponent is the fog amount, so a water with no fog amount stays clear at any depth. When the
// depth is unknown the water draws as its deep colour at its ANAM opacity, as Skyrim draws water
// it has no depth for.
fn water_body(path: f32) -> vec2<f32> {
    if path < 0.0 {
        return vec2<f32>(1.0, water.body.x);
    }
    let near = water.body.y;
    let far = max(water.body.z, near + 1.0);
    let colour = clamp(path / far, 0.0, 1.0);
    let amount = water.body.w;
    if amount <= 0.0 {
        return vec2<f32>(colour, 0.0);
    }
    let clear = clamp((far - path) / (far - near), 0.0, 1.0);
    return vec2<f32>(colour, 1.0 - pow(clear, amount));
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
#ifdef MULTISAMPLED
    @builtin(sample_index) sample_index: u32,
#endif
) -> FragmentOutput {
#ifndef MULTISAMPLED
    let sample_index = 0u;
#endif
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    let body = water_body(water_path_length(in.position, in.world_position.xyz, sample_index));
    let deep = pbr_input.material.base_color.rgb;
    pbr_input.material.base_color = vec4<f32>(mix(water.shallow_color.rgb, deep, body.x), 1.0);
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
    let reflection_sample = textureSample(water_reflection, water_reflection_sampler, reflection_uv);
    let reflection_color = vec4<f32>(
        mix(water.reflection_color.rgb, reflection_sample.rgb, reflection_sample.a),
        1.0,
    );
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
    let reflected = fresnel * water.fresnel_reflectivity.y * water.fresnel_reflectivity.z;
    // The Fresnel reflection can make the surface less transparent at grazing angles than the body
    // alone, so shallow water still reflects.
    let body_alpha = body.y;
    out.color = vec4<f32>(mix(out.color.rgb, reflection_color.rgb, reflected), max(body_alpha, fresnel));
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
