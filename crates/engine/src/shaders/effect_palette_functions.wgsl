// The palette colour lookup shared by the effect palette material's forward and prepass shaders.
#define_import_path engine::effect_palette

#import bevy_pbr::{
    pbr_bindings,
    pbr_types::STANDARD_MATERIAL_FLAGS_BASE_COLOR_TEXTURE_BIT,
    mesh_view_bindings::view,
}
#import bevy_render::color_operations::linear_to_srgb

struct EffectPaletteSettings {
    // x = 1 when the palette gives the colour, y = 1 when it gives the alpha,
    // z = the shader's base colour scale. w unused.
    lookup: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> effect: EffectPaletteSettings;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var palette: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var palette_sampler: sampler;

fn palette_sample(x: f32, y: f32) -> vec4<f32> {
    // The game clamps the lookup; clamping to the outer texel centres also keeps a wrapping
    // sampler from blending in the opposite edge of the palette.
    let half_texel = 0.5 / vec2<f32>(textureDimensions(palette));
    let uv = clamp(vec2<f32>(x, y), half_texel, vec2<f32>(1.0) - half_texel);
    return textureSampleLevel(palette, palette_sampler, uv, 0.0);
}

// The source texture at a mesh UV, or white without one.
fn source_texel(mesh_uv: vec2<f32>) -> vec4<f32> {
    if (pbr_bindings::material.flags & STANDARD_MATERIAL_FLAGS_BASE_COLOR_TEXTURE_BIT) == 0u {
        return vec4<f32>(1.0);
    }
    let uv = (pbr_bindings::material.uv_transform * vec3<f32>(mesh_uv, 1.0)).xy;
    return textureSampleBias(
        pbr_bindings::base_color_texture,
        pbr_bindings::base_color_sampler,
        uv,
        view.mip_bias,
    );
}

// The effect's alpha: from the palette with Greyscale_To_PaletteAlpha, otherwise the texture's
// alpha times the tint's.
fn effect_alpha(texel: vec4<f32>, tint: vec4<f32>) -> f32 {
    if effect.lookup.y > 0.5 {
        return palette_sample(texel.a, tint.a).a;
    }
    return texel.a * tint.a;
}

// Match the colour assembled by the forward effect shader before Bevy applies alpha handling.
fn effect_color(standard_color: vec4<f32>, texel: vec4<f32>, tint: vec4<f32>) -> vec4<f32> {
    var color = vec4<f32>(standard_color.rgb, effect_alpha(texel, tint));
    if effect.lookup.x > 0.5 {
        let grey = linear_to_srgb(texel.rgb).g;
        color = vec4<f32>(palette_sample(grey, tint.r).rgb, color.a);
    }
    return vec4<f32>(color.rgb * effect.lookup.z, color.a);
}
