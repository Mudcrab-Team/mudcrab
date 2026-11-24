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
#import bevy_render::view::View

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
    // Linear view depth. Bevy's reverse-Z projection has clip.z = m22 * z + m32 and clip.w = -z for
    // a point in front of the camera, so ndc = m32 / d - m22 and d = m32 / (ndc + m22). For the
    // infinite reverse-Z projection m22 = 0 and m32 = near, which gives d = near / ndc.
    let z = view.clip_from_view[3][2] / (ndc + view.clip_from_view[2][2]);
    let near = fog.range.x;
    let r = 1.0 / (fog.range.y - fog.range.x);
    let s = clamp(z * r - near * r, 0.0, 1.0);
    // exp2(log2(s) * p) is s ^ p. At s = 0 the log is -inf and exp2 gives 0 for a positive power.
    let fc = min(exp2(log2(s) * fog.range.z), fog.range.w);
    let cfc = fog.near_colour.rgb + fc * (fog.far_colour.rgb - fog.near_colour.rgb);
    let k = fog.near_colour.w;
    return vec4<f32>(k * fc * cfc, k * (1.0 - fc) * fog.far_colour.w);
}
