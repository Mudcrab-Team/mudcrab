#define_import_path engine::terrain_blend

// Noise equation: https://github.com/Mudcrab-Team/skyrim_decomp/pull/946, G02.
fn noise_multiplier(base: vec3<f32>, red: f32) -> f32 {
    let a = clamp((0.55 * dot(base, vec3<f32>(1.0)) - 0.4) * 1.666667, 0.0, 1.0);
    let h = a * a * (3.0 - 2.0 * a);
    let mixed_red = red + (0.37 - red) * (0.8 * h);
    return 0.37 + 0.833333 * mixed_red;
}

// Sample at three times the UV and use the non-projected 0.8h weight
// from https://github.com/Mudcrab-Team/skyrim_decomp/pull/946, G02.
fn noisy_land_colour(
    base: vec3<f32>, noise: texture_2d<f32>, noise_sampler: sampler, uv: vec2<f32>,
) -> vec3<f32> {
    let red = textureSample(noise, noise_sampler, 3.0 * uv).r;
    return base * noise_multiplier(base, red);
}

struct NearTerrainBlendSettings {
    // Cell-local centre in land XY. The atlas offset and scale use cell-wide UVs.
    centre: vec4<f32>,
    atlas: vec4<f32>,
}

// G02 of https://github.com/Mudcrab-Team/skyrim_decomp/pull/947.
// The vertex weight is interpolated before applying the pixel gate.
fn terrain_blend_ramp(distance: f32) -> f32 {
    return 1.0 - clamp((9625.599609375 - distance) * 0.0003756009682547301, 0.0, 1.0);
}

fn terrain_blend_weight(ramp: f32, gate: f32) -> f32 {
    return ramp * gate;
}

fn terrain_atlas_uv(cell_uv: vec2<f32>, atlas: vec4<f32>) -> vec2<f32> {
    let uv = atlas.xy + cell_uv * atlas.z;
    return vec2<f32>(uv.x, 1.0 - uv.y);
}
