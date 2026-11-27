// Retail exterior distance fog for the opaque scene (#891). `retail_fog.rs` runs this full-screen
// pass after the opaque stage and before the blended stages, over the colour the opaque pass left.
//
// Per RGB channel, with L the scene colour already in the target:
//   out = k * ((1 - Fc) * A * L + Fc * Cfc)
//   Fc  = min(saturate(z * r - near * r) ^ p, max)      r = 1 / (far - near)
//   Cfc = nearRGB + Fc * (farRGB - nearRGB)
//   k   = 1 / FramebufferRange = 0.8333 (the uniform carries `output_scale.rs`, the one constant)
// The pass is a blend, `src.rgb + dst.rgb * src.a`, so it outputs src.rgb = k * Fc * Cfc and
// src.a = k * (1 - Fc) * A. The outer saturate of retail's formula is not drawn (HDR target); the
// notes in `retail_fog.rs` say why. A = 1 while Mudcrab has no SAO pass: STAND-IN(#995): factor one replaces the retail SAO texture. A pixel with no
// geometry (depth 0 in Mudcrab's reverse-Z buffer, the sky) skips both the fog and the `k` scale.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput
#import bevy_render::view::{View, frag_coord_to_uv, position_ndc_to_view, uv_to_ndc}

struct RetailFog {
    // x = near, y = far, z = power, w = max.
    range: vec4<f32>,
    // rgb = the near fog colour (linear), w = k.
    near_colour: vec4<f32>,
    // rgb = the far fog colour (linear), w = A.
    far_colour: vec4<f32>,
}

@group(0) @binding(0) var<uniform> fog: RetailFog;
@group(0) @binding(1) var<uniform> view: View;
#ifdef MULTISAMPLED
@group(0) @binding(2) var scene_depth: texture_depth_multisampled_2d;
#else
@group(0) @binding(2) var scene_depth: texture_depth_2d;
#endif

// Retail's composite reads a copy of the scene in an unsigned format, so a negative scene value
// reads as 0 (SPEC-2 section 3, docs/specs/engine/near-lighting.md). This draw, with a Max blend,
// raises the negative values of the target to 0 before the composite.
@fragment
fn unsigned_input() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}

@fragment
fn fragment(in: FullscreenVertexOutput, @builtin(sample_index) sample_index: u32) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(in.position.xy);
#ifdef MULTISAMPLED
    let ndc = textureLoad(scene_depth, coord, sample_index);
#else
    let ndc = textureLoad(scene_depth, coord, 0);
#endif
    // The identity of the blend: src.rgb = 0 and src.a = 1.
    if ndc <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    // Axial linear view depth. The depth is unprojected through the full inverse projection, not
    // `near / depth`: the water reflection camera has an oblique near plane, so its depth is not a
    // function of the view distance alone. For the world camera (infinite reverse-Z, m22 = 0 and
    // m32 = near) this gives `near / depth`, and the tests `view_depth_*` in `retail_fog.rs` pin both.
    let uv = frag_coord_to_uv(in.position.xy, view.viewport);
    let view_position = position_ndc_to_view(
        vec3<f32>(uv_to_ndc(uv), ndc),
        view.view_from_clip,
    );
    let z = -view_position.z;
    let near = fog.range.x;
    let r = 1.0 / (fog.range.y - fog.range.x);
    let s = clamp(z * r - near * r, 0.0, 1.0);
    // exp2(log2(s) * p) is s ^ p. At s = 0 the log is -inf and exp2 gives 0 for a positive power.
    let fc = min(exp2(log2(s) * fog.range.z), fog.range.w);
    let cfc = fog.near_colour.rgb + fc * (fog.far_colour.rgb - fog.near_colour.rgb);
    let k = fog.near_colour.w;
    return vec4<f32>(k * fc * cfc, k * (1.0 - fc) * fog.far_colour.w);
}
