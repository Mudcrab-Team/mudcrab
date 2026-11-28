// Retail exterior distance fog for the opaque scene (#891). `retail_fog.rs` runs this full-screen
// pass after the opaque stage and before the blended stages, over the colour the opaque pass left.
//
// Per RGB channel, with L the scene colour already in the target:
//   out = k * ((1 - Fc) * A * L + Fc * Cfc)
//   Fc  = min(saturate(z * r - near * r) ^ p, max)      r = 1 / (far - near)
//   Cfc = nearRGB + Fc * (farRGB - nearRGB)
//   k   = 1 / FramebufferRange = 0.8333 (the uniform carries `output_scale.rs`, the one constant)
// The packed path applies the mix and saturates it in one write. The blend fallback leaves the
// saturation to the following Min-blend pass.
// STAND-IN(#995): factor one replaces the retail SAO texture.
// A sky pixel has reverse-Z depth 0 and skips the fog and output scale.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput
#import bevy_render::view::{View, frag_coord_to_uv, position_ndc_to_view, uv_to_ndc}

struct RetailFog {
    // x = near, y = far, z = power, w = max.
    range: vec4<f32>,
    // rgb = the near fog colour (linear), w = k.
    near_colour: vec4<f32>,
    // rgb = the far fog colour (linear), w = A.
    far_colour: vec4<f32>,
    // x = perspective near, y = perspective far, or zero for another projection.
    camera_range: vec2<f32>,
}

@group(0) @binding(0) var<uniform> fog: RetailFog;
@group(0) @binding(1) var<uniform> view: View;
#ifdef MULTISAMPLED
@group(0) @binding(2) var copied_depth: texture_depth_multisampled_2d;
@group(0) @binding(4) var colour_depth: texture_depth_multisampled_2d;
#else
@group(0) @binding(2) var copied_depth: texture_depth_2d;
@group(0) @binding(4) var colour_depth: texture_depth_2d;
#endif
@group(0) @binding(3) var scene_colour: texture_2d<f32>;

// Retail's composite reads a copy of the scene in an unsigned format, so a negative scene value
// reads as 0 (SPEC-2 section 3, docs/specs/engine/near-lighting.md). This draw, with a Max blend,
// raises the negative values of the target to 0 before the composite.
@fragment
fn unsigned_input() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}

fn composite_terms(in: FullscreenVertexOutput, sample_index: u32) -> vec4<f32> {
    let coord = vec2<i32>(in.position.xy);
#ifdef MULTISAMPLED
    let copied_z = textureLoad(copied_depth, coord, sample_index);
    let colour_z = textureLoad(colour_depth, coord, sample_index);
#else
    let copied_z = textureLoad(copied_depth, coord, 0);
    let colour_z = textureLoad(colour_depth, coord, 0);
#endif
    // STAND-IN(#362): merged terrain and some decals write depth only in the colour pass.
    // Reverse-Z puts a visible surface ahead of any object it hides in the prepass.
    let ndc = max(copied_z, colour_z);
    // Uncovered depth skips fog and output scale. Occlusion still applies.
    if ndc <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, fog.far_colour.w);
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
    var z = -view_position.z;
    let camera_near = fog.camera_range.x;
    let camera_far = fog.camera_range.y;
    if camera_far > 0.0 {
        // SPEC-2 section 3. Re-express the finite forward-Z depth and 1.01d - 0.01
        // remap in view units to retain precision at the far end of the fog ramp.
        let sky_edge = camera_far * camera_near /
            (camera_near + (camera_far - camera_near) * 0.000001);
        if z >= sky_edge {
            return vec4<f32>(0.0, 0.0, 0.0, fog.far_colour.w);
        }
        z /= 1.01 - 0.01 * (z / camera_far);
    }
    let near = fog.range.x;
    let r = 1.0 / (fog.range.y - fog.range.x);
    let s = clamp(z * r - near * r, 0.0, 1.0);
    // exp2(log2(s) * p) is s ^ p. At s = 0 the log is -inf and exp2 gives 0 for a positive power.
    let fc = min(exp2(log2(s) * fog.range.z), fog.range.w);
    let cfc = fog.near_colour.rgb + fc * (fog.far_colour.rgb - fog.near_colour.rgb);
    let k = fog.near_colour.w;
    return vec4<f32>(k * fc * cfc, k * (1.0 - fc) * fog.far_colour.w);
}

@fragment
fn fragment(in: FullscreenVertexOutput, @builtin(sample_index) sample_index: u32) -> @location(0) vec4<f32> {
    return composite_terms(in, sample_index);
}

// SPEC-2 section 3: target 113 stores the scene copy in R11G11B10_FLOAT.
@fragment
fn copy_scene(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let scene = textureLoad(scene_colour, vec2<i32>(in.position.xy), 0).rgb;
    let valid = (bitcast<vec3<u32>>(scene) & vec3<u32>(0x7fffffffu)) <= vec3<u32>(0x7f800000u);
    return vec4<f32>(
        clamp(select(vec3<f32>(0.0), scene, valid), vec3<f32>(0.0),
              vec3<f32>(65024.0, 65024.0, 64512.0)),
        1.0,
    );
}

@fragment
fn composite_scene(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let terms = composite_terms(in, 0u);
    let scene = textureLoad(scene_colour, vec2<i32>(in.position.xy), 0).rgb;
    return vec4<f32>(saturate(scene * terms.a + terms.rgb), 1.0);
}
