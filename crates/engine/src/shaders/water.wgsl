// Skyrim's water, as far as it is known. The pixel formulas follow the water pixel shader of
// Community Shaders, `package/Shaders/Water.hlsl` (GPL-3.0, https://github.com/doodlum/skyrim-community-shaders,
// read at commit dc89d339), which reimplements the retail shader's `GetWaterNormal`, `GetFresnelValue`,
// `GetWaterDiffuseColor` and `main`. The noise scroll is the retail engine's own, read from
// `SkyrimSE.exe` 1.7.104.0 (`0x1405290ac` to `0x140529356`; `evidence/rea-20261114/q1-water.md`).
// The WATR field names and offsets are those of CommonLibSSE-NG `TESWaterForm.h` (MIT, commit
// b93280e). `docs/research/water.md` lists each source and what is still UNKNOWN. Every number in
// this file is a WATR field, a number from one of those sources, or an engineering guard.
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    view_transformations::{depth_ndc_to_view_z, position_world_to_view},
}
#import bevy_pbr::mesh_view_bindings::{globals, view}

struct WaterSettings {
    // x = multiplier on the noise UV scales, y = multiplier on the wind speeds, z = multiplier on
    // the noise amplitudes (each the user's setting over its default; z is 0 when waves are off),
    // w = pinned wave time, or negative to run on the frame clock.
    wave_scale_speed_strength: vec4<f32>,
    // xyz = the three noise layers' wind directions in radians (WATR noiseWindDirectionA, the
    // compass bearing the wind comes from: 0 north, 90 east), w unused.
    noise_wind_direction: vec4<f32>,
    // xyz = the three noise layers' wind speeds in tiles per second (noiseWindSpeedA).
    noise_wind_speed: vec4<f32>,
    // x = Fresnel Amount, y = Reflectivity Amount, z = the user's reflection multiplier, w =
    // Reflection Magnitude (WATR DNAM, per water).
    fresnel_reflectivity: vec4<f32>,
    // rgb = the water's Reflection Color (linear), alpha unused.
    reflection_color: vec4<f32>,
    // rgb = Shallow Color (linear); the deep colour is the material base colour.
    shallow_color: vec4<f32>,
    // x = opacity 0..1 (WATR ANAM), used where the depth is unknown; y, z = above-water fog near
    // and far distance in world units; w = fog amount, the curve exponent (WATR DNAM).
    body: vec4<f32>,
    // x = 1 when a snapshot texture is bound to bed_depth, 0 when none is and the body falls back
    // to its ANAM opacity. Whether a given frame's texels are valid is carried by the texels
    // themselves (a negative value means unknown), not by this flag.
    depth_params: vec4<f32>,
    // xyz = world units per tile of the three noise layers (DNAM uvScaleA), w = 1 when the noise
    // textures are bound, 0 for a flat surface.
    noise_uv_scale: vec4<f32>,
    // xyz = the amplitudes of the three noise layers (DNAM amplitudeA); a layer without a texture
    // has amplitude 0.
    noise_amplitude: vec4<f32>,
    // DNAM depth properties: reflections, refraction, normals, specular lighting.
    depth_control: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> water: WaterSettings;

@group(#{MATERIAL_BIND_GROUP}) @binding(101) var water_reflection: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var water_reflection_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var water_flow_normal: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var water_flow_normal_sampler: sampler;
// The opaque bed's reverse-Z NDC depth, one sample per pixel, written by the engine before the
// transparent pass. See `water_depth.rs`.
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var bed_depth: texture_2d<f32>;
// The water's three noise normal maps (WATR NAM2 to NAM4), read with the flow normal's sampler.
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var water_noise_0: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var water_noise_1: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var water_noise_2: texture_2d<f32>;

// Any path at or past the fog far distance is opaque and fully deep (Skyrim saturates the ratio),
// so this only has to be larger than any fog far distance.
const BOTTOMLESS_PATH: f32 = 1.0e7;

// How far, in world units, the view ray travels through water before it reaches whatever opaque
// lies behind this fragment; negative when that is unknown because this view has no bed-depth
// snapshot. The ratio below is a perspective identity - surface and bed share a ray from the
// camera - so this and the vertical depth in the fragment assume a perspective view. The engine's
// world and reflection cameras are always perspective; an orthographic view would need parallel
// rays.
fn water_path_length(frag_coord: vec4<f32>, world_position: vec3<f32>) -> f32 {
    if water.depth_params.x < 0.5 {
        return -1.0;
    }
    let ndc = textureLoad(bed_depth, vec2<i32>(frag_coord.xy), 0).x;
    if ndc < 0.0 {
        // The engine could not resolve the bed this frame and cleared the snapshot to
        // `UNRESOLVED_BED_DEPTH`; draw the body as if this view had no depth.
        return -1.0;
    }
    if ndc <= 0.0 {
        // Nothing opaque behind the water (reverse-Z: 0 is infinitely far).
        return BOTTOMLESS_PATH;
    }
    let bed_view_z = depth_ndc_to_view_z(ndc);
    let surface = position_world_to_view(world_position);
    // The surface and the bed lie on one view ray, so the bed is that ray scaled by the ratio of
    // their view depths (both negative in front of the camera); the part past the surface is water.
    let past_surface = max(bed_view_z / min(surface.z, -1e-4) - 1.0, 0.0);
    return min(length(surface) * past_surface, BOTTOMLESS_PATH);
}

// x = how far the body colour has moved from shallow to deep (Skyrim's distanceMul.y, the path
// through the water over the fog far distance), y = how much of the surface is the water body
// rather than what lies behind (refractionMul, 1 - saturate((far - path) / (far - near))^amount),
// z = the vertical depth over the fog far distance (distanceMul.z, which the depth properties
// read). A water with no fog amount stays clear at any depth. When the depth is unknown the water
// draws as its deep colour at its ANAM opacity, as Skyrim draws water it has no depth for.
fn water_body(path: f32, vertical_depth: f32) -> vec3<f32> {
    if path < 0.0 {
        return vec3<f32>(1.0, water.body.x, 1.0);
    }
    let near = water.body.y;
    // The CPU already keeps far at least one unit past near and at least one (`usable_fog_far`);
    // the shader keeps the same bound so no uniform can make the divisions below divide by zero.
    let far = max(water.body.z, max(near + 1.0, 1.0));
    let colour = clamp(path / far, 0.0, 1.0);
    let vertical = clamp(vertical_depth / far, 0.0, 1.0);
    let amount = water.body.w;
    if amount <= 0.0 {
        return vec3<f32>(colour, 0.0, vertical);
    }
    let clear = clamp((far - path) / (far - near), 0.0, 1.0);
    return vec3<f32>(colour, 1.0 - pow(clear, amount), vertical);
}

// One noise normal map sampled at a world-space UV. The map tiles, so the UV is wrapped by hand
// and the gradients of the unwrapped UV are passed, which keeps the mip level continuous across
// the wrap.
fn noise_texel(normal_map: texture_2d<f32>, uv: vec2<f32>) -> vec3<f32> {
    return textureSampleGrad(normal_map, water_flow_normal_sampler, fract(uv), dpdx(uv), dpdy(uv)).xyz;
}

// Where layer `layer`'s pattern sits at the water clock: the retail engine wraps
// (sin(theta) * speed * T, cos(theta) * speed * T) into [0, 1), theta being the layer's wind
// direction in radians and speed its wind speed in tiles per second. U runs east and V north, and
// the offset is added to the UV, so the pattern moves toward the bearing theta + 180 degrees.
fn noise_scroll(theta: f32, speed: f32, seconds: f32) -> vec2<f32> {
    return fract(vec2<f32>(sin(theta), cos(theta)) * speed * seconds);
}

// The surface normal in Skyrim's frame (x east, y north, z up), from the three noise layers:
// normalize((0, 0, 1) + a0 * n0 + a1 * n1 + a2 * n2), flattened toward (0, 0, 1) in shallow water
// by the normals depth property. Layer 0 is decoded as 2t + (-1, -1, -2), the others as 2t - 1,
// as Skyrim's shader does. `position` is world x and Creation y (the Bevy z negated).
fn noise_normal(position: vec2<f32>, seconds: f32, normals_depth: f32) -> vec3<f32> {
    if water.noise_uv_scale.w < 0.5 {
        return vec3<f32>(0.0, 0.0, 1.0);
    }
    let size = water.wave_scale_speed_strength.x;
    let clock = seconds * water.wave_scale_speed_strength.y;
    let uv0 = position * size / water.noise_uv_scale.x + noise_scroll(water.noise_wind_direction.x, water.noise_wind_speed.x, clock);
    let uv1 = position * size / water.noise_uv_scale.y + noise_scroll(water.noise_wind_direction.y, water.noise_wind_speed.y, clock);
    let uv2 = position * size / water.noise_uv_scale.z + noise_scroll(water.noise_wind_direction.z, water.noise_wind_speed.z, clock);
    let n0 = noise_texel(water_noise_0, uv0) * 2.0 + vec3<f32>(-1.0, -1.0, -2.0);
    let n1 = noise_texel(water_noise_1, uv1) * 2.0 - 1.0;
    let n2 = noise_texel(water_noise_2, uv2) * 2.0 - 1.0;
    let amplitude = water.noise_amplitude.xyz * water.wave_scale_speed_strength.z;
    let blended = normalize(vec3<f32>(0.0, 0.0, 1.0) + amplitude.x * n0 + amplitude.y * n1 + amplitude.z * n2);
    return normalize(mix(vec3<f32>(0.0, 0.0, 1.0), blended, normals_depth));
}

// Where the planar reflection image shows the direction `world_direction`: the mirrored camera
// sees it at the screen position of the direction mirrored about the water plane, in the camera's
// own view-space axes with y flipped (the reflection camera keeps +Y up). For the direction
// reflected about a flat surface this is exactly the fragment's mirrored position, so only the
// difference between two directions matters.
fn reflection_image_uv(world_direction: vec3<f32>) -> vec2<f32> {
    let mirrored = vec3<f32>(world_direction.x, -world_direction.y, world_direction.z);
    let v = (view.view_from_world * vec4<f32>(mirrored, 0.0)).xyz;
    let ndc = vec2<f32>(view.clip_from_view[0][0] * v.x, -view.clip_from_view[1][1] * v.y) / max(-v.z, 1.0e-3);
    return vec2<f32>(0.5 + 0.5 * ndc.x, 0.5 - 0.5 * ndc.y);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    let path = water_path_length(in.position, in.world_position.xyz);
    // The bed lies on the view ray, so the vertical depth is the path times the sine of the ray's
    // pitch.
    let to_surface = in.world_position.xyz - view.world_position.xyz;
    let vertical_depth = path * abs(to_surface.y) / max(length(to_surface), 1e-4);
    let body = water_body(path, vertical_depth);
    let deep = pbr_input.material.base_color.rgb;
    pbr_input.material.base_color = vec4<f32>(mix(water.shallow_color.rgb, deep, body.x), 1.0);

    // A negative time runs the waves on the frame clock; zero or more pins them (`ANIMATED_WAVE_PHASE`).
    let pinned = water.wave_scale_speed_strength.w;
    let time = select(globals.time, pinned, pinned >= 0.0);
    let position = vec2<f32>(in.world_position.x, -in.world_position.z);
    // depthControl.z = DepthControl.z * (distanceMul.z - 1) + 1: the normals fade in with depth.
    let normals_depth = water.depth_control.z * (body.z - 1.0) + 1.0;
    let normal_skyrim = noise_normal(position, time, normals_depth);
    // Skyrim (x, y, z) is Bevy (x, z, -y).
    let normal = vec3<f32>(normal_skyrim.x, normal_skyrim.z, -normal_skyrim.y);
    pbr_input.N = normal;

    // The reflection: Skyrim reflects along reflect(view, m * normal + (0, 0, 1 - m)), m being the
    // Reflection Magnitude. The planar reflection is looked up where that direction lands, the
    // flat one being the fragment's own mirrored position.
    let view_direction = normalize(to_surface);
    let magnitude = water.fresnel_reflectivity.w;
    let up = vec3<f32>(0.0, 1.0, 0.0);
    let reflected_normal = magnitude * normal + (1.0 - magnitude) * up;
    var reflected = reflect(view_direction, reflected_normal);
    reflected.y = max(reflected.y, 0.0);
    let flat_reflected = reflect(view_direction, up);
    let viewport_uv = (in.position.xy - view.viewport.xy) / view.viewport.zw;
    let reflection_uv = clamp(
        vec2<f32>(viewport_uv.x, 1.0 - viewport_uv.y) + reflection_image_uv(reflected) - reflection_image_uv(flat_reflected),
        vec2<f32>(0.0),
        vec2<f32>(1.0),
    );
    let reflection_sample = textureSample(water_reflection, water_reflection_sampler, reflection_uv);
    // Without a reflection image (alpha 0) Skyrim reflects its flat Reflection Color scaled by the
    // Reflectivity Amount.
    let reflection_color = vec4<f32>(
        mix(water.reflection_color.rgb * water.fresnel_reflectivity.y, reflection_sample.rgb, reflection_sample.a),
        1.0,
    );
    // GetFresnelValue: (1 - F) * (1 - n.v)^5 + F, F being the water's Fresnel Amount.
    let water_fresnel_f0 = water.fresnel_reflectivity.x;
    let n_dot_v = clamp(dot(normalize(pbr_input.V), pbr_input.N), 0.0, 1.0);
    let fresnel = water_fresnel_f0 + (1.0 - water_fresnel_f0) * pow(1.0 - n_dot_v, 5.0);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    var out: FragmentOutput;
    // The water's own colour is lit; the reflection is light that already left the scene, so it is
    // blended in after lighting. Skyrim: finalColor = lerp(refractionDiffuseColor, specularColor,
    // fresnel), then lerp(what lies behind, finalColor, refractionMul). The second lerp is the
    // alpha blend, with the body's coverage as alpha: the Fresnel reflection does not raise it.
    out.color = apply_pbr_lighting(pbr_input);
    let reflected_weight = fresnel * water.fresnel_reflectivity.z;
    out.color = vec4<f32>(mix(out.color.rgb, reflection_color.rgb, reflected_weight), body.y);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
