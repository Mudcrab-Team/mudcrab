// Skyrim's water, as far as it is known. The mix, the opacity, the Fresnel term, the reflection and
// the depth factors are the retail water pixel shader's, read from the disassembled retail
// programs (`shaders011.fxp` group 8, keys 0x1C, 0x11C and 0x18) and the WATR material copy in
// `SkyrimSE.exe` 1.7.104.0: `evidence/rea-20261114/s1-water-mix.md`, registry RF-SHADER-001 and
// RF-SHADER-006 to RF-SHADER-010. The noise scroll is the retail engine's own, `0x1405290ac` to
// `0x140529356` (`evidence/rea-20261114/q1-water.md`, RF-WATER-001 to RF-WATER-006). The noise
// normal blend follows Community Shaders `package/Shaders/Water.hlsl` (GPL-3.0, commit dc89d339), which
// agrees with the retail programs; the WATR field names and offsets are CommonLibSSE-NG
// `TESWaterForm.h` (MIT, b93280e). `docs/research/water.md` lists each source and what is still
// UNKNOWN. Every number in this file is a WATR field, a retail number from those sources, or an
// engineering guard.
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::main_pass_post_lighting_processing,
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
    // x = Fresnel Amount, y = Reflectivity Amount, z = the user's reflection multiplier (1 by
    // default), w = Reflection Magnitude (WATR DNAM, per water).
    fresnel_reflectivity: vec4<f32>,
    // rgb = the water's Reflection Color times the weather's Water Multiplier (linear), alpha unused.
    reflection_color: vec4<f32>,
    // rgb = Shallow Color times the Water Multiplier (linear); the deep colour, likewise multiplied,
    // is the material base colour.
    shallow_color: vec4<f32>,
    // x = opacity 0..1 (WATR ANAM), used where the depth is unknown; y = the fog range (far minus
    // near), z = max(fog far, 1), w = the curve exponent max(4 * fog amount, 1) (RF-SHADER-006).
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
// FramebufferRange (RF-SHADER-001): the retail water shader scales every texture it reads by this
// and its output by the inverse, so the colours it computes from constants (the body and the flat
// reflection colour) reach the screen divided by it. Only the console command `SetFramebufferRange`
// changes it.
const FRAMEBUFFER_RANGE: f32 = 1.2;
// The retail literal of the near-camera distance ratio (RF-SHADER-008, RF-WATER-009).
const WATER_DISTANCE_LITERAL: f32 = 8192.0;
// fWaterSSRIntensity:Display, weighting the reflection image the way the retail pixel shader weights
// its screen-space reflection (RF-WATER-009). 0.5 is this install's Skyrim.ini value; the
// executable's own default is 1.3. The install value is followed.
const WATER_SSR_INTENSITY: f32 = 0.5;
// The main pass's frame flag `w` in `distT = sat(lerp(w, 1, ratio))`. UNVERIFIED (#698); 1 makes
// `distT` 1 at every distance, as the arithmetic does for any flag up to 4007 units.
const WATER_FRAME_FLAG: f32 = 1.0;

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

// x = sat(L / F), L being the view ray's length through the water and F the fog far distance: it
// moves the body colour from shallow to deep and is the path term of the depth factors; y = the
// body's share over what lies behind, 1 - sat((F - min(L, F)) / range) ^ exponent (RF-SHADER-010);
// z = sat(h / F), h being the vertical depth, which the normals depth factor reads. When the depth
// is unknown the water draws as its deep colour at its ANAM opacity; the retail programs examined
// all have depth, so that fallback is not retail.
fn water_body(path: f32, vertical_depth: f32) -> vec3<f32> {
    if path < 0.0 {
        return vec3<f32>(1.0, water.body.x, 1.0);
    }
    let range = water.body.y;
    let far = water.body.z;
    let exponent = water.body.w;
    let along = clamp(path / far, 0.0, 1.0);
    let vertical = clamp(vertical_depth / far, 0.0, 1.0);
    let clear = clamp((far - min(path, far)) / range, 0.0, 1.0);
    return vec3<f32>(along, 1.0 - pow(clear, exponent), vertical);
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

    // A negative time runs the waves on the frame clock; zero or more pins them (`ANIMATED_WAVE_PHASE`).
    let pinned = water.wave_scale_speed_strength.w;
    let time = select(globals.time, pinned, pinned >= 0.0);
    let position = vec2<f32>(in.world_position.x, -in.world_position.z);
    // depthControl.z = lerp(1, sat(h / F), DepthControl.z): the normals fade in with depth.
    let normals_depth = water.depth_control.z * (body.z - 1.0) + 1.0;
    let normal_skyrim = noise_normal(position, time, normals_depth);
    // Skyrim (x, y, z) is Bevy (x, z, -y).
    let normal = vec3<f32>(normal_skyrim.x, normal_skyrim.z, -normal_skyrim.y);
    pbr_input.N = normal;

    // The reflection: Skyrim reflects along reflect(view, lerp(up, n, m)), m being the Reflection
    // Magnitude (RF-SHADER-009). The planar reflection is looked up where that direction lands,
    // the flat one being the fragment's own mirrored position.
    let view_direction = normalize(to_surface);
    let magnitude = water.fresnel_reflectivity.w;
    let up = vec3<f32>(0.0, 1.0, 0.0);
    let reflected_normal = mix(up, normal, magnitude);
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
    // distT = sat(lerp(flag, 1, (D - 8192) / (falloff - 8192))), D the eye distance and falloff the
    // record's noise falloff clamped to [1, 8191] on the CPU (RF-SHADER-007, RF-SHADER-008).
    let ratio = (length(to_surface) - WATER_DISTANCE_LITERAL) / (water.depth_params.y - WATER_DISTANCE_LITERAL);
    let dist_t = clamp(mix(WATER_FRAME_FLAG, 1.0, ratio), 0.0, 1.0);
    // refl = lerp(ReflectionColor, env, Reflection Amount) (RF-SHADER-009). Retail multiplies env by
    // FramebufferRange and the result by its inverse, so env passes unchanged and the flat colour is
    // divided. env is the flat colour (standing in for the sky cube map, until one exists) toward
    // the reflection image, weighted like retail's screen-space reflection:
    // sat(distT * fWaterSSRIntensity * image alpha). Where the image has no coverage (alpha 0) the
    // environment is the flat colour.
    let flat_reflection = water.reflection_color.rgb / FRAMEBUFFER_RANGE;
    let environment = mix(
        flat_reflection,
        reflection_sample.rgb,
        clamp(dist_t * WATER_SSR_INTENSITY * reflection_sample.a, 0.0, 1.0),
    );
    let reflection = mix(flat_reflection, environment, water.fresnel_reflectivity.y);

    // F = FresnelRI.x + (1 - FresnelRI.x) * (1 - sat(-V.N))^5 (RF-SHADER-008).
    let fresnel_amount = water.fresnel_reflectivity.x;
    let n_dot_v = clamp(dot(normalize(pbr_input.V), normal), 0.0, 1.0);
    let fresnel = fresnel_amount + (1.0 - fresnel_amount) * pow(1.0 - n_dot_v, 5.0);

    // The retail mix (RF-SHADER-008, RF-SHADER-010):
    //   diffuse = lerp(what lies behind, water, a)        a = body.y
    //   color   = lerp(diffuse, reflection, m)            m = lerp(1, F * depthReflections, distT)
    // where depthReflections = lerp(1, sat(L / F), DepthControl.x). The main pass's frame flag is
    // unverified (#698). The first lerp is the alpha
    // blend with the bed already in the frame, so both lerps become one blend with coverage
    // 1 - (1 - a)(1 - m) and the colour that makes the product match.
    let depth_reflections = water.depth_control.x * (body.x - 1.0) + 1.0;
    let m = mix(1.0, fresnel * depth_reflections * water.fresnel_reflectivity.z, dist_t);
    let a = body.y;
    // The body colour, lerp(Shallow, Deep, sat(L / F)), divided by FramebufferRange as the retail
    // output is.
    let deep = pbr_input.material.base_color.rgb;
    let water_colour = mix(water.shallow_color.rgb, deep, body.x) / FRAMEBUFFER_RANGE;
    let coverage = a + m - a * m;
    var out: FragmentOutput;
    out.color = vec4<f32>((water_colour * (1.0 - m) * a + reflection * m) / max(coverage, 1.0e-6), coverage);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
