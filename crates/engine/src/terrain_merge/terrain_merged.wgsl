#import engine::terrain_blend::{NearTerrainBlendSettings, noise_multiplier, noisy_land_colour, retail_land_alpha, terrain_atlas_uv, terrain_blend_ramp, terrain_blend_weight}
#import engine::terrain_normal::{retail_terrain_normal, terrain_blend_divisor, terrain_blended_normal, terrain_tangent_normal, terrain_tbn}

// The shared terrain shader of merged cells (#215): one material for every cell.
//
// Each vertex carries its quadrant's land texture ids and its own slot weights (see
// `terrain_merge/mesh.rs`). Slot 0 is the quadrant's base and its weight is implied,
// `max(0, 1 - sum of the others)`; the colour is `sum(texture_i * w_i) / max(0.0001, sum w_i)` and the
// normals are blended the same way as `terrain.wgsl`, which is today's path for cells with no
// merged data. The land textures live in game-wide texture arrays, one binding per class the
// converter lists (`classes.rs`); this file is specialised to those classes by shader defs.
//
// The class an id belongs to is data, so the array sampled can differ between neighbouring
// fragments. Every sample therefore uses `textureSampleGrad` with the derivatives of the shared
// UV, computed once in uniform control flow.

#import bevy_pbr::{
    mesh_functions,
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::{STANDARD_MATERIAL_FLAGS_RETAIL_LIGHTING_BIT, STANDARD_MATERIAL_FLAGS_UNLIT_BIT},
    view_transformations::position_world_to_clip,
}

#ifdef TERRAIN_SLOT_STRIDE_16
const SLOT_COUNT: u32 = 16u;
#else
const SLOT_COUNT: u32 = 8u;
#endif

// A packed lookup word is `class << 16 | layer`; an entry with no normal map has every bit set.
const NO_NORMAL_WORD: u32 = 0xFFFFFFFFu;

struct MergedTerrainSettings {
    // xy = land texture repeats per cell.
    tiling: vec4<f32>,
}

#ifdef TERRAIN_DIFFUSE_CLASS_0
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var diffuse_0: texture_2d_array<f32>;
#endif
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var land_sampler: sampler;
#ifdef TERRAIN_DIFFUSE_CLASS_1
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var diffuse_1: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_2
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var diffuse_2: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_3
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var diffuse_3: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_4
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var diffuse_4: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_5
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var diffuse_5: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_6
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var diffuse_6: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_7
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var diffuse_7: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_8
@group(#{MATERIAL_BIND_GROUP}) @binding(109) var diffuse_8: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_NORMAL_CLASS_0
@group(#{MATERIAL_BIND_GROUP}) @binding(110) var normal_0: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_NORMAL_CLASS_1
@group(#{MATERIAL_BIND_GROUP}) @binding(111) var normal_1: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_NORMAL_CLASS_2
@group(#{MATERIAL_BIND_GROUP}) @binding(112) var normal_2: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_NORMAL_CLASS_3
@group(#{MATERIAL_BIND_GROUP}) @binding(113) var normal_3: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_NORMAL_CLASS_4
@group(#{MATERIAL_BIND_GROUP}) @binding(114) var normal_4: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_NORMAL_CLASS_5
@group(#{MATERIAL_BIND_GROUP}) @binding(115) var normal_5: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_NORMAL_CLASS_6
@group(#{MATERIAL_BIND_GROUP}) @binding(116) var normal_6: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_NORMAL_CLASS_7
@group(#{MATERIAL_BIND_GROUP}) @binding(117) var normal_7: texture_2d_array<f32>;
#endif
#ifdef TERRAIN_NORMAL_CLASS_8
@group(#{MATERIAL_BIND_GROUP}) @binding(118) var normal_8: texture_2d_array<f32>;
#endif
@group(#{MATERIAL_BIND_GROUP}) @binding(119) var<storage, read> land_entries: array<vec2<u32>>;
@group(#{MATERIAL_BIND_GROUP}) @binding(120) var<uniform> settings: MergedTerrainSettings;

struct TerrainVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
    @location(8) ids_0: vec4<u32>,
    @location(9) ids_1: vec4<u32>,
#ifdef TERRAIN_SLOT_STRIDE_16
    @location(10) ids_2: vec4<u32>,
    @location(11) ids_3: vec4<u32>,
#endif
    @location(12) weights_0: vec4<f32>,
    @location(13) weights_1: vec4<f32>,
#ifdef TERRAIN_SLOT_STRIDE_16
    @location(14) weights_2: vec4<f32>,
    @location(15) weights_3: vec4<f32>,
#endif
}

@group(#{MATERIAL_BIND_GROUP}) @binding(121) var<uniform> seam: NearTerrainBlendSettings;
@group(#{MATERIAL_BIND_GROUP}) @binding(122) var seam_overlay: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(123) var seam_overlay_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(124) var seam_noise: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(125) var seam_noise_sampler: sampler;

struct TerrainVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    @location(6) @interpolate(flat) instance_index: u32,
#endif
#ifdef VISIBILITY_RANGE_DITHER
    @location(7) @interpolate(flat) visibility_range_dither: i32,
#endif
    // Every vertex of a quadrant carries the same ids, so the flat provoking value is exact.
    @location(8) @interpolate(flat) ids_0: vec4<u32>,
    @location(9) @interpolate(flat) ids_1: vec4<u32>,
#ifdef TERRAIN_SLOT_STRIDE_16
    @location(10) @interpolate(flat) ids_2: vec4<u32>,
    @location(11) @interpolate(flat) ids_3: vec4<u32>,
#endif
    @location(12) weights_0: vec4<f32>,
    @location(13) weights_1: vec4<f32>,
#ifdef TERRAIN_SLOT_STRIDE_16
    @location(14) weights_2: vec4<f32>,
    @location(15) weights_3: vec4<f32>,
#endif
    @location(3) seam_ramp: f32,
}

@vertex
fn vertex(input: TerrainVertex) -> TerrainVertexOutput {
    var out: TerrainVertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(input.instance_index);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(input.normal, input.instance_index);
    out.world_position = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(input.position, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    out.uv = input.uv;
#ifdef VERTEX_COLORS
    out.color = input.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = input.instance_index;
#endif
#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        input.instance_index, world_from_local[3]);
#endif
    out.ids_0 = input.ids_0;
    out.ids_1 = input.ids_1;
    out.weights_0 = input.weights_0;
    out.weights_1 = input.weights_1;
#ifdef TERRAIN_SLOT_STRIDE_16
    out.ids_2 = input.ids_2;
    out.ids_3 = input.ids_3;
    out.weights_2 = input.weights_2;
    out.weights_3 = input.weights_3;
#endif
    out.seam_ramp = terrain_blend_ramp(distance(vec2<f32>(input.position.x, -input.position.z), seam.centre.xy));
    return out;
}

// One diffuse sample from the class slot `class`. A class the table does not list is white, which
// cannot happen for a validated table; the default arm keeps the switch total.
fn sample_diffuse(class_slot: u32, uv: vec2<f32>, layer: u32, ddx: vec2<f32>, ddy: vec2<f32>) -> vec4<f32> {
    switch class_slot {
#ifdef TERRAIN_DIFFUSE_CLASS_0
        case 0u: { return textureSampleGrad(diffuse_0, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_1
        case 1u: { return textureSampleGrad(diffuse_1, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_2
        case 2u: { return textureSampleGrad(diffuse_2, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_3
        case 3u: { return textureSampleGrad(diffuse_3, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_4
        case 4u: { return textureSampleGrad(diffuse_4, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_5
        case 5u: { return textureSampleGrad(diffuse_5, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_6
        case 6u: { return textureSampleGrad(diffuse_6, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_7
        case 7u: { return textureSampleGrad(diffuse_7, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_DIFFUSE_CLASS_8
        case 8u: { return textureSampleGrad(diffuse_8, land_sampler, uv, layer, ddx, ddy); }
#endif
        default: { return vec4<f32>(1.0); }
    }
}

// One normal RGBA sample: xy supplies the normal and alpha supplies specular intensity.
fn sample_normal(class_slot: u32, uv: vec2<f32>, layer: u32, ddx: vec2<f32>, ddy: vec2<f32>) -> vec4<f32> {
#ifdef TERRAIN_HAS_NORMAL_CLASSES
    switch class_slot {
#ifdef TERRAIN_NORMAL_CLASS_0
        case 0u: { return textureSampleGrad(normal_0, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_NORMAL_CLASS_1
        case 1u: { return textureSampleGrad(normal_1, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_NORMAL_CLASS_2
        case 2u: { return textureSampleGrad(normal_2, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_NORMAL_CLASS_3
        case 3u: { return textureSampleGrad(normal_3, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_NORMAL_CLASS_4
        case 4u: { return textureSampleGrad(normal_4, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_NORMAL_CLASS_5
        case 5u: { return textureSampleGrad(normal_5, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_NORMAL_CLASS_6
        case 6u: { return textureSampleGrad(normal_6, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_NORMAL_CLASS_7
        case 7u: { return textureSampleGrad(normal_7, land_sampler, uv, layer, ddx, ddy); }
#endif
#ifdef TERRAIN_NORMAL_CLASS_8
        case 8u: { return textureSampleGrad(normal_8, land_sampler, uv, layer, ddx, ddy); }
#endif
        default: { return vec4<f32>(0.5, 0.5, 1.0, 0.0); }
    }
#else
    return vec4<f32>(0.5, 0.5, 1.0, 0.0);
#endif
}

// Same PBR bridge as terrain.wgsl: normal alpha is Skyrim's specular intensity, not roughness.
// Community Shaders (GPL-3.0), Lighting.hlsl at e53c0a1ffaa01199490c6097a7dabb559932a88e,
// lines 1192-1357 and 1968-1976. Bevy's F0 is quadratic in reflectance, hence sqrt(mask).
fn terrain_reflectance(reflectance: vec3<f32>, mask_sum: f32, total: f32) -> vec3<f32> {
    return reflectance * sqrt(clamp(mask_sum / total, 0.0, 1.0));
}

fn slot_id(in: TerrainVertexOutput, slot: u32) -> u32 {
    let lane = slot % 4u;
    switch slot / 4u {
        case 0u: { return in.ids_0[lane]; }
        case 1u: { return in.ids_1[lane]; }
#ifdef TERRAIN_SLOT_STRIDE_16
        case 2u: { return in.ids_2[lane]; }
        case 3u: { return in.ids_3[lane]; }
#endif
        default: { return 0u; }
    }
}

fn slot_weight(in: TerrainVertexOutput, slot: u32) -> f32 {
    let lane = slot % 4u;
    switch slot / 4u {
        case 0u: { return in.weights_0[lane]; }
        case 1u: { return in.weights_1[lane]; }
#ifdef TERRAIN_SLOT_STRIDE_16
        case 2u: { return in.weights_2[lane]; }
        case 3u: { return in.weights_3[lane]; }
#endif
        default: { return 0.0; }
    }
}

@fragment
fn fragment(in: TerrainVertexOutput) -> FragmentOutput {
    var standard_in: VertexOutput;
    standard_in.position = in.position;
    standard_in.world_position = in.world_position;
    standard_in.world_normal = in.world_normal;
    standard_in.uv = in.uv;
#ifdef VERTEX_COLORS
    standard_in.color = in.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    standard_in.instance_index = in.instance_index;
#endif
#ifdef VISIBILITY_RANGE_DITHER
    standard_in.visibility_range_dither = in.visibility_range_dither;
#endif
    // Today's quadrant meshes carry tangents, so Bevy never flips their normal on a back face
    // (`prepare_world_normal` flips only a mesh without `VERTEX_TANGENTS`). This mesh has none, so
    // the face is reported as a front face: the terrain is lit by its interpolated normal from
    // both sides, exactly as the quadrant path lights it.
    var pbr_input = pbr_input_from_standard_material(standard_in, true);
    let retail =
        (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_RETAIL_LIGHTING_BIT) != 0u;
    // A layer with no normal map: the geometric normal, or retail's black texel (SPEC-1 section 5).
    var empty_normal = vec3<f32>(0.0, 0.0, 1.0);
    if retail {
        empty_normal = retail_terrain_normal(vec3<f32>(0.0));
    }

    // One UV for every layer, and its derivatives once, in uniform control flow.
    let uv = in.uv * settings.tiling.xy;
    let ddx = dpdx(uv);
    let ddy = dpdy(uv);

    // The overlay slots' weights, then the implied base weight, exactly as `terrain.wgsl`.
    var overlay_sum = 0.0;
    for (var slot = 1u; slot < SLOT_COUNT; slot += 1u) {
        overlay_sum += slot_weight(in, slot);
    }
    let base_weight = max(0.0, 1.0 - overlay_sum);
    let total = max(0.0001, base_weight + overlay_sum);

    var color = vec4<f32>(0.0);
    var tangent_normal = vec3<f32>(0.0);
    var any_normal = false;
    var specular_mask = 0.0;
    for (var slot = 0u; slot < SLOT_COUNT; slot += 1u) {
        var weight = base_weight;
        if slot > 0u {
            weight = slot_weight(in, slot);
        }
        // A slot with no weight here contributes nothing, so its textures are not read.
        if weight <= 0.0 {
            continue;
        }
        let entry = land_entries[slot_id(in, slot)];
        color += sample_diffuse(entry.x >> 16u, uv, entry.x & 0xFFFFu, ddx, ddy) * weight;
#ifdef TERRAIN_HAS_NORMAL_CLASSES
        if entry.y != NO_NORMAL_WORD {
            let sample = sample_normal(entry.y >> 16u, uv, entry.y & 0xFFFFu, ddx, ddy);
            if retail {
                tangent_normal += retail_terrain_normal(sample.xyz) * weight;
            } else {
                tangent_normal += terrain_tangent_normal(sample.xy) * weight;
            }
            specular_mask += sample.a * weight;
            any_normal = true;
        } else {
            tangent_normal += empty_normal * weight;
        }
#else
        tangent_normal += empty_normal * weight;
#endif
    }
    let divisor = terrain_blend_divisor(total, retail);
    color = color / divisor;
    let seam_weight = terrain_blend_weight(in.seam_ramp, seam.atlas.w);
    // Retail's landscape writes a target 0 alpha of 0, or the distant blend's.
    var land_alpha = 0.0;
    if seam.atlas.w != 0.0 {
        let distant_uv = terrain_atlas_uv(in.uv, seam.atlas);
        let distant = textureSample(seam_overlay, seam_overlay_sampler, distant_uv);
        let noisy = noisy_land_colour(distant.rgb, seam_noise, seam_noise_sampler, distant_uv);
        let red = textureSample(seam_noise, seam_noise_sampler, 3.0 * distant_uv).r;
        var vertex_alpha = 1.0;
#ifdef VERTEX_COLORS
        vertex_alpha = in.color.a;
#endif
        land_alpha = retail_land_alpha(distant.a, noise_multiplier(distant.rgb, red), seam_weight, vertex_alpha);
        color = vec4<f32>(mix(color.rgb, noisy, seam_weight), color.a);
    }
    // Technique 19 order (skyrim_decomp#979): noise scales the distant sample before blending.
    // Vertex tint scales the full lit diffuse before the first fog bound. Highlights enter
    // separately before a second bound when enabled.
    // Bevy's vertex color scales the blended base color before diffuse lighting. The specular
    // reflectance of these opaque dielectric materials is independent of base color.
    // Bevy fog processes the combined lit result. The separate fog bounds need draw inputs
    // that this pass does not bind.
    pbr_input.material.base_color *= color;
    pbr_input.material.reflectance = terrain_reflectance(pbr_input.material.reflectance, specular_mask, total);
    // Only a fragment that reads at least one normal map turns the blend into a world normal.
    if any_normal || retail {
        let geometric = normalize(pbr_input.world_normal);
        pbr_input.N = normalize(terrain_tbn(geometric) * terrain_blended_normal(mix(tangent_normal / divisor, vec3<f32>(0.0, 0.0, 1.0), seam_weight)));
    }
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    var out: FragmentOutput;
    // The same unlit guard as Bevy's own `pbr.wgsl` fragment: `apply_pbr_lighting` has no unlit
    // check of its own, so a material flagged unlit must skip it here or it is lit regardless.
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    if retail {
        out.color.a = land_alpha;
    }
    return out;
}
