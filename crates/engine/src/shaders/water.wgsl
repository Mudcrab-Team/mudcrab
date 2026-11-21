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
    view_transformations::{depth_ndc_to_view_z, position_ndc_to_world, position_world_to_clip, position_world_to_view},
}
#import bevy_pbr::mesh_view_bindings::{globals, lights, view, view_transmission_sampler, view_transmission_texture}

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
    // x = sun specular power, y = its magnitude, z = sparkle power, w = its magnitude (WATR DNAM
    // +0x10, +0xCC, +0xE0, +0xC8; RF-WATER-010).
    sun_terms: vec4<f32>,
    // rgb = the sun colour the terms are multiplied by (linear), w unused. The sun intensity
    // `SunDir.w` is not traced and is 1 (#698).
    sun_color: vec4<f32>,
    // x = the refraction magnitude in world units (WATR DNAM +0x98; RF-WATER-049); yzw unused.
    refraction: vec4<f32>,
    // xy = the cell's grid coordinates, z = 1 when the flow-map program runs, w unused.
    flow: vec4<f32>,
    // The sky dome's colour table (sky.rs `SkyUniform`), which the water reflects: row elevations in
    // degrees four to a vector, per row the sRGB-encoded colour, and x = the linear brightness.
    sky_elevations: array<vec4<f32>, 4>,
    sky_colours: array<vec4<f32>, 16>,
    sky_brightness: vec4<f32>,
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
// The cell's 64 by 64 flow map (RF-WATER-036): rg = the flow direction as (cos, sin) * 0.5 + 0.5,
// b = the scale term, a = the phase speed. Read by texel (`flow_texel`). `water.flow.z`
// says whether the flow program runs.
@group(#{MATERIAL_BIND_GROUP}) @binding(110) var water_flow_map: texture_2d<f32>;
// The flow maps of the east, south and south-east neighbour cells, read where a tap shifted half a
// texel passes this cell's edge (RF-WATER-063). `water.flow.w` is the bit set of the bound ones.
@group(#{MATERIAL_BIND_GROUP}) @binding(111) var water_flow_map_east: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(112) var water_flow_map_south: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(113) var water_flow_map_south_east: texture_2d<f32>;

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
// The sparkle direction, a retail shader literal in Skyrim's frame (RF-WATER-010).
const WATER_SPARKLE_DIRECTION: vec3<f32> = vec3<f32>(-0.099, -0.099, 0.99);
// The flow-map program's literals, all read from the retail bytecode (RF-WATER-044, RF-WATER-039)
// unless noted. The flow map is 64 by 64 texels per cell (RF-WATER-036).
const FLOW_MAP_TEXELS: f32 = 64.0;
const FLOW_RIPPLE_SCALE: f32 = 64.0;
const FLOW_SCALE_BASE: f32 = 1.01;
const FLOW_CLOCK_FACTOR: f32 = 0.001;
// The least the depth factor leaves of the flow normal (RF-WATER-039).
const FLOW_DEPTH_FLOOR: f32 = 0.4;
// Each tap's scroll rate and UV offset, in the order A, B, C, D (RF-WATER-044).
const FLOW_RATES: vec4<f32> = vec4<f32>(10.64, 9.92, 8.0, 8.48);
const FLOW_OFFSETS: vec4<f32> = vec4<f32>(0.27, 0.0, 0.0, 0.62);
// The flow clock is the game hour in seconds (RF-WATER-038: GameHour x 3600, wrapping at midnight,
// fallback hour 12). The engine has no game clock (#698), so it starts at the fallback hour 12 and
// runs at the retail default time scale of 20 game seconds per real second (RF-WATER-038).
const FLOW_CLOCK_START: f32 = 43200.0;
const FLOW_CLOCK_SPEED: f32 = 20.0;
const FLOW_CLOCK_DAY: f32 = 86400.0;
// The smallest reverse-Z depth taken as a real point, so a pixel of sky (depth 0, infinitely far) still
// gives a finite scene point for the plane test (an engineering guard; retail reads the depth as is).
const WATER_DEPTH_FLOOR: f32 = 1.0e-7;
// Floors the base of a power so log2 of a base of 0 does not give NaN (an engineering guard; the
// retail shader writes exp2(power * log2(base)) without one).
const WATER_POWER_FLOOR: f32 = 1.0e-30;

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

const SKY_ROW_COUNT: u32 = 16u;

fn sky_row_elevation(row: u32) -> f32 {
    return water.sky_elevations[row / 4u][row % 4u];
}

fn sky_srgb_to_linear(colour: vec3<f32>) -> vec3<f32> {
    let low = colour / 12.92;
    let high = pow((colour + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, colour <= vec3<f32>(0.04045));
}

// The sky dome's colour at `elevation` degrees: the dome shader's own interpolation of its colour
// table (`sky.wgsl`), so the water reflects the sky the camera sees.
fn sky_dome_colour(elevation: f32) -> vec3<f32> {
    var colour = water.sky_colours[SKY_ROW_COUNT - 1u].rgb;
    if elevation <= sky_row_elevation(0u) {
        colour = water.sky_colours[0].rgb;
    } else {
        for (var row = 1u; row < SKY_ROW_COUNT; row += 1u) {
            let above = sky_row_elevation(row);
            if elevation <= above {
                let below = sky_row_elevation(row - 1u);
                let t = (elevation - below) / max(above - below, 1.0e-6);
                colour = mix(water.sky_colours[row - 1u].rgb, water.sky_colours[row].rgb, t);
                break;
            }
        }
    }
    return sky_srgb_to_linear(colour) * water.sky_brightness.x;
}

// The scene colour the water is seen over, as the retail pixel shader reads it (RF-WATER-049): the
// point on the water is moved by (N.xy * DNAM 0x98 * lerp(1, sat(L / F), DNAM 0xD4), 0) in Skyrim's
// world XY and projected, and the scene copy is read there. The depth at that point is loaded and
// its scene point tested against the water plane: a point on the camera's side of the plane is
// something in front of the water (a bank, a reed), so the unperturbed pixel is used instead. The
// copy is Bevy's view transmission texture, taken after the opaque pass and before any transmissive
// draw, so it excludes the water itself, as retail's copy does. `normal_skyrim` is the surface
// normal in Skyrim's axes, `along` is sat(L / F) and `depth_known` is false when the bed depth is
// unknown (the interior and no-depth programs, which have no leak test and so no offset).
fn refracted_scene(frag_coord: vec4<f32>, world_position: vec3<f32>, normal_skyrim: vec3<f32>, along: f32, depth_known: bool) -> vec3<f32> {
    let unperturbed_uv = (frag_coord.xy - view.viewport.xy) / view.viewport.zw;
    var uv = unperturbed_uv;
    if depth_known {
        let factor = water.depth_control.y * (along - 1.0) + 1.0;
        // Skyrim's (x, y, z) is Bevy's (x, z, -y): the offset is in the water plane, so it has no
        // Bevy y part.
        let offset = vec3<f32>(normal_skyrim.x, 0.0, -normal_skyrim.y) * water.refraction.x * factor;
        let clip = position_world_to_clip(world_position + offset);
        let candidate = vec2<f32>(0.5 + 0.5 * clip.x / clip.w, 0.5 - 0.5 * clip.y / clip.w);
        let size = vec2<i32>(textureDimensions(bed_depth));
        let texel = clamp(vec2<i32>(candidate * view.viewport.zw + view.viewport.xy), vec2<i32>(0), size - vec2<i32>(1));
        let ndc_depth = textureLoad(bed_depth, texel, 0).x;
        // A negative depth is the unresolved sentinel: the leak cannot be tested, so no offset.
        if ndc_depth >= 0.0 && clip.w > 0.0 {
            // The scene point seen at the candidate pixel; reverse-Z 0 is infinitely far, which the
            // engineering floor keeps finite.
            let candidate_ndc = vec3<f32>(candidate.x * 2.0 - 1.0, 1.0 - candidate.y * 2.0, max(ndc_depth, WATER_DEPTH_FLOOR));
            let scene_point = position_ndc_to_world(candidate_ndc);
            // Both the camera and the scene point are tested against the water plane (horizontal at
            // the surface's height); the same side means the point is in front of the water.
            let camera_side = view.world_position.y - world_position.y;
            let scene_side = scene_point.y - world_position.y;
            if camera_side * scene_side <= 0.0 {
                uv = candidate;
            }
        }
    }
    return textureSampleLevel(view_transmission_texture, view_transmission_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
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

// The flow map texel under `uv`, point-sampled. The map holds a direction, a scale and a phase speed
// per texel, which warp the ripple coordinates by large factors (64 times the scale, 346 tiles of
// phase), so a value interpolated between two texels is not a flow at all: filtering the map smears
// the ripples into flat patches and sheared streaks (controlled renders at the same pose). The four
// taps and their bilinear weights do the interpolation on the resulting normals instead
// (RF-WATER-044), which only makes sense with point taps; retail's atlas sampler is POINT with wrap
// addressing (RF-WATER-063).
//
// The taps are shifted half a texel east and south, so at the cell's east and south edges they read
// one texel past it. Retail's atlas holds geographic neighbours side by side, so that texel is the
// neighbour cell's first column or row, and the result is continuous across every cell border
// (RF-WATER-063). Each cell here has its own texture, so the east, south and south-east
// neighbours are bound too and the read goes to them; a side whose neighbour has no flow map clamps
// to this cell's edge texel instead (retail's atlas slot would hold whatever it held, which the
// registry does not give).
fn flow_texel(uv: vec2<f32>) -> vec4<f32> {
    let texel = vec2<i32>(floor(max(uv, vec2<f32>(0.0)) * FLOW_MAP_TEXELS));
    let last = i32(FLOW_MAP_TEXELS) - 1;
    let bound = u32(water.flow.w + 0.5);
    let past_east = texel.x > last;
    let past_south = texel.y > last;
    if past_east && past_south && (bound & 4u) != 0u {
        return textureLoad(water_flow_map_south_east, vec2<i32>(0, 0), 0);
    }
    if past_east && !past_south && (bound & 1u) != 0u {
        return textureLoad(water_flow_map_east, vec2<i32>(0, texel.y), 0);
    }
    if past_south && !past_east && (bound & 2u) != 0u {
        return textureLoad(water_flow_map_south, vec2<i32>(texel.x, 0), 0);
    }
    return textureLoad(water_flow_map, min(texel, vec2<i32>(last)), 0);
}

// One tap of the flow program (RF-WATER-044): the flow texel `flow` turns and scales the ripple
// coordinates `ripple`, the phase speed scrolls them with the flow clock, and the tiling flow
// normal texture is read there. Returns that texture's rg.
fn flow_tap(flow: vec4<f32>, ripple: vec2<f32>, clock: f32, rate: f32, offset: f32) -> vec2<f32> {
    let rotation = flow.rg * 2.0 - 1.0;
    let scaled = FLOW_RIPPLE_SCALE * ripple * sqrt(max(FLOW_SCALE_BASE - flow.b, 0.0));
    let turned = vec2<f32>(rotation.x * scaled.x - rotation.y * scaled.y, rotation.y * scaled.x + rotation.x * scaled.y);
    let phase = FLOW_CLOCK_FACTOR * clock * flow.a;
    let uv = turned - vec2<f32>(rate * phase, 0.0) + vec2<f32>(offset, offset);
    return noise_texel(water_flow_normal, uv).rg;
}

// The flow-map program's normal xy (RF-WATER-044): four taps of the cell's flow map around the
// pixel, each steering one read of the tiling flow normals, blended with the weights of a
// bilinear lookup and renormalised, then faded by the normals depth factor (RF-WATER-047).
// `cell_uv` is the cell-local texture coordinate (u east, v south, as the retail cell mesh has it)
// and `normals_depth` is the DNAM 0xD8 depth factor.
fn flow_normal_xy(cell_uv: vec2<f32>, seconds: f32, normals_depth: f32) -> vec2<f32> {
    // Each cell has its own texture here where retail has one atlas of the loaded cells, so the
    // taps past the cell's east and south edges read the bound neighbour textures (`flow_texel`).
    let h = 0.5 / FLOW_MAP_TEXELS;
    let w = 1.0 - abs(2.0 * fract(FLOW_MAP_TEXELS * cell_uv) - 1.0);
    let flow_a = flow_texel(cell_uv + vec2<f32>(0.0, h));
    let flow_b = flow_texel(cell_uv + vec2<f32>(h, h));
    let flow_c = flow_texel(cell_uv);
    let flow_d = flow_texel(cell_uv + vec2<f32>(h, 0.0));
    // The player's wave settings apply to the flow path as to the noise path, and are neutral at
    // their defaults (the retail flow program itself reads no amplitude, RF-WATER-023): the scale
    // multiplies the ripple frequency, the speed multiplies the clock's advance (0 freezes the
    // phase), and the strength, which is 0 with waves off, scales the normal below.
    let wave_scale = water.wave_scale_speed_strength.x;
    let wave_speed = water.wave_scale_speed_strength.y;
    let wave_strength = water.wave_scale_speed_strength.z;
    // The ripple coordinate is continuous across cells: the cell's mesh coordinate plus (cellX, -cellY).
    let ripple = (cell_uv + vec2<f32>(water.flow.x, -water.flow.y)) * wave_scale;
    let clock = (FLOW_CLOCK_START + seconds * wave_speed * FLOW_CLOCK_SPEED) % FLOW_CLOCK_DAY;
    let n_a = flow_tap(flow_a, ripple, clock, FLOW_RATES.x, FLOW_OFFSETS.x);
    let n_b = flow_tap(flow_b, ripple, clock, FLOW_RATES.y, FLOW_OFFSETS.y);
    let n_c = flow_tap(flow_c, ripple, clock, FLOW_RATES.z, FLOW_OFFSETS.z);
    let n_d = flow_tap(flow_d, ripple, clock, FLOW_RATES.w, FLOW_OFFSETS.w);
    let blended = w.y * (w.x * n_c + (1.0 - w.x) * n_d) + (1.0 - w.y) * (w.x * n_a + (1.0 - w.x) * n_b);
    let renormalised = (blended - 0.5)
        / (sqrt((1.0 - w.x) * (1.0 - w.x) + w.x * w.x) * sqrt((1.0 - w.y) * (1.0 - w.y) + w.y * w.y));
    return renormalised * max(normals_depth, FLOW_DEPTH_FLOOR) * wave_strength;
}

// The flow program's one tiled layer (RF-WATER-047): the first noise map at worldXY / DNAM 0xAC
// plus its scroll, decoded as 2t - 1. It shows only past the noise falloff.
fn flow_tiled_normal(position: vec2<f32>, seconds: f32) -> vec3<f32> {
    if water.noise_uv_scale.w < 0.5 {
        return vec3<f32>(0.0, 0.0, 1.0);
    }
    let size = water.wave_scale_speed_strength.x;
    let clock = seconds * water.wave_scale_speed_strength.y;
    let uv = position * size / water.noise_uv_scale.x + noise_scroll(water.noise_wind_direction.x, water.noise_wind_speed.x, clock);
    // The strength scales the tilt away from flat, as the noise path scales its amplitudes.
    return normalize(mix(vec3<f32>(0.0, 0.0, 1.0), normalize(noise_texel(water_noise_0, uv) * 2.0 - 1.0), water.wave_scale_speed_strength.z));
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
    // Interior cells use retail's interior program (RF-WATER-017). The sun is the engine's first
    // directional light.
    let interior = water.depth_params.z > 0.5;
    var sun_direction = vec3<f32>(0.0, 1.0, 0.0);
    if lights.n_directional_lights > 0u {
        sun_direction = lights.directional_lights[0].direction_to_light;
    }
    let vertical_depth = path * abs(to_surface.y) / max(length(to_surface), 1e-4);
    let body = water_body(path, vertical_depth);

    // A negative time runs the waves on the frame clock; zero or more pins them (`ANIMATED_WAVE_PHASE`).
    let pinned = water.wave_scale_speed_strength.w;
    let time = select(globals.time, pinned, pinned >= 0.0);
    let position = vec2<f32>(in.world_position.x, -in.world_position.z);
    // depthControl.z = lerp(1, sat(h / F), DepthControl.z): the normals fade in with depth.
    let normals_depth = water.depth_control.z * (body.z - 1.0) + 1.0;
    // distT = sat(lerp(flag, 1, (D - 8192) / (falloff - 8192))), D the eye distance and falloff the
    // record's noise falloff clamped to [1, 8191] on the CPU (RF-SHADER-007, RF-SHADER-008).
    let ratio = (length(to_surface) - WATER_DISTANCE_LITERAL) / (water.depth_params.y - WATER_DISTANCE_LITERAL);
    let dist_t = clamp(mix(WATER_FRAME_FLAG, 1.0, ratio), 0.0, 1.0);
    // Retail's flow-map program (RF-WATER-044, RF-WATER-047) takes the normal from the flow map
    // and the flow normals, with only one tiled layer, which distT fades out; the other programs
    // blend the three noise layers.
    var normal_skyrim = noise_normal(position, time, normals_depth);
    if water.flow.z > 0.5 && !interior {
        let flow_xy = flow_normal_xy(in.uv, time, normals_depth);
        let flow = vec3<f32>(flow_xy, sqrt(max(1.0 - dot(flow_xy, flow_xy), 0.0)));
        normal_skyrim = normalize(mix(flow_tiled_normal(position, time), flow, dist_t));
    }
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
    // An engineering guard, not retail (retail has no clamp: it samples a cube map): a planar image
    // cannot show a direction that points down, so it is turned level.
    reflected.y = max(reflected.y, 0.0);
    let flat_reflected = reflect(view_direction, up);
    let viewport_uv = (in.position.xy - view.viewport.xy) / view.viewport.zw;
    let reflection_uv = clamp(
        vec2<f32>(viewport_uv.x, 1.0 - viewport_uv.y) + reflection_image_uv(reflected) - reflection_image_uv(flat_reflected),
        vec2<f32>(0.0),
        vec2<f32>(1.0),
    );
    let reflection_sample = textureSample(water_reflection, water_reflection_sampler, reflection_uv);
    // refl = lerp(ReflectionColor, env, DNAM 0x14) with env = lerp(sky * FBR, ssr, s),
    // s = sat(distT * fWaterSSRIntensity * ssr.a) (RF-WATER-050). Retail multiplies env by
    // FramebufferRange and the result by its inverse, so env passes unchanged and the flat colour is
    // divided. The sky is the dome's colour along reflect(V, lerp(up, N, Reflection Magnitude)), with no
    // clamp as in retail's cube lookup (`WaterExtension::with_sky` says why it is not a cube map).
    // The planar reflection image takes the place of retail's screen-space reflection: it is the
    // local scene, with an alpha that is 0 where it has no coverage. Without a sky (an interior) the
    // flat colour stands in for it.
    let flat_reflection = water.reflection_color.rgb / FRAMEBUFFER_RANGE;
    let sky_ray = reflect(view_direction, reflected_normal);
    let sky_elevation = degrees(asin(clamp(sky_ray.y, -1.0, 1.0)));
    let sky = select(flat_reflection, sky_dome_colour(sky_elevation), water.depth_params.w > 0.5);
    let environment = mix(
        sky,
        reflection_sample.rgb,
        clamp(dist_t * WATER_SSR_INTENSITY * reflection_sample.a, 0.0, 1.0),
    );
    let reflection_planar = mix(flat_reflection, environment, water.fresnel_reflectivity.y);

    // F = FresnelRI.x + (1 - FresnelRI.x) * (1 - sat(-V.N))^5 (RF-SHADER-008).
    let fresnel_amount = water.fresnel_reflectivity.x;
    let n_dot_v = clamp(dot(normalize(pbr_input.V), normal), 0.0, 1.0);
    let fresnel = fresnel_amount + (1.0 - fresnel_amount) * pow(1.0 - n_dot_v, 5.0);

    // The retail mix (RF-SHADER-008, RF-SHADER-010):
    //   diffuse = lerp(what lies behind, water, a)        a = body.y
    //   color   = lerp(diffuse, reflection, m)            m = lerp(1, F * depthReflections, distT)
    // where depthReflections = lerp(1, sat(L / F), DepthControl.x). The main pass's frame flag is
    // unverified (#698). What lies behind is the refracted scene copy (`refracted_scene`).
    let depth_reflections = water.depth_control.x * (body.x - 1.0) + 1.0;
    let m = mix(1.0, fresnel * depth_reflections * water.fresnel_reflectivity.z, dist_t);
    var reflection = reflection_planar;
    if interior {
        reflection = flat_reflection * water.fresnel_reflectivity.y;
    }
    // The variants without depth: the interior program and the exterior no-depth program use the
    // ANAM opacity (RF-WATER-026) and a body of lerp(Shallow, Deep, F), times sat(sunDir.N) for the
    // exterior one (RF-WATER-017). The interior program also has no reflection image, so its
    // reflection is the flat colour times the Reflection Amount.
    let no_depth = interior || path < 0.0;
    let a = select(body.y, water.body.x, no_depth);
    let deep = pbr_input.material.base_color.rgb;
    var water_colour = mix(water.shallow_color.rgb, deep, body.x) / FRAMEBUFFER_RANGE;
    if no_depth {
        var sun_lit = 1.0;
        if !interior && lights.n_directional_lights > 0u {
            sun_lit = clamp(dot(sun_direction, normal), 0.0, 1.0);
        }
        water_colour = mix(water.shallow_color.rgb, deep, fresnel) * sun_lit / FRAMEBUFFER_RANGE;
    }
    let depth_known = !no_depth;
    // The scene the water is seen over: what the retail first lerp, lerp(refr, body, a), blends the
    // body with. Refraction offsets only the programs that have depth.
    let refracted = refracted_scene(in.position, in.world_position.xyz, normal_skyrim, body.x, depth_known);
    // The sun specular and sparkle (RF-WATER-010), added after the mix: spec =
    // sat(R.sunDir)^power * magnitude, sparkle = sat(N.(-0.099, -0.099, 0.99))^power * magnitude,
    // both times the sun colour S and the specular depth factor dS = lerp(1, sat(h / F),
    // DepthControl.w), then divided by FramebufferRange like every constant-derived colour. R and N
    // are the full normal's (no Reflection Magnitude), as in retail. The sun is the engine's first
    // directional light; with none (an interior) the surface gets no sun, as in retail.
    var sun = vec3<f32>(0.0);
    if lights.n_directional_lights > 0u && !interior {
        let mirror = reflect(view_direction, normal);
        let spec_base = max(clamp(dot(mirror, sun_direction), 0.0, 1.0), WATER_POWER_FLOOR);
        let sparkle_base = max(clamp(dot(normal_skyrim, WATER_SPARKLE_DIRECTION), 0.0, 1.0), WATER_POWER_FLOOR);
        let spec = exp2(water.sun_terms.x * log2(spec_base)) * water.sun_terms.y;
        let sparkle = exp2(water.sun_terms.z * log2(sparkle_base)) * water.sun_terms.w;
        let specular_depth = water.depth_control.w * (body.z - 1.0) + 1.0;
        sun = (spec + sparkle) * water.sun_color.rgb * specular_depth / FRAMEBUFFER_RANGE;
    }
    // The sun adds to the whole pixel after the mix (RF-WATER-010).
    var out: FragmentOutput;
    // color = lerp(lerp(refr, body, a), reflection, m) + sun, written opaque: the scene is already
    // in the colour, so the water replaces the pixel instead of blending over it.
    out.color = vec4<f32>(refracted * (1.0 - a) * (1.0 - m) + water_colour * (1.0 - m) * a + reflection * m + sun, 1.0);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
