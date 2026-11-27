// The retail tonemap pass (ISHDR, NEEDS 18, `retail_hdr.rs`): luminance tonemap, bloom add,
// saturation, tint, brightness and contrast, saturate, power of 1 / fGamma, fade. It runs after
// the scene and the fog composite and writes the display value.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput
#import engine::retail_hdr::{RetailHdr, finite, luminance}

@group(0) @binding(0) var<uniform> hdr: RetailHdr;
@group(0) @binding(1) var scene: texture_2d<f32>;
// kHDR_BLOOM, sampled bilinearly at the screen coordinate.
@group(0) @binding(2) var bloom_texture: texture_2d<f32>;
// The 2 by 2 adapted pair (R = A.x, G = A.y), sampled bilinearly at the screen coordinate with
// clamp addressing, so the value varies across the screen (NEEDS 27).
@group(0) @binding(3) var adapted_texture: texture_2d<f32>;
@group(0) @binding(4) var linear_clamp: sampler;

// Filmic: h(x) = x (6.2 x + 0.5) / (x (6.2 x + 1.7) + 0.06).
fn filmic(x: f32) -> f32 {
    return (x * (6.2 * x + 0.5)) / (x * (6.2 * x + 1.7) + 0.06);
}

// The curve on the scaled luminance `ls`. `w2` is the white coefficient from the CPU
// (Reinhard: 1 / (1.1 White)^2. Filmic: h(max(1.4 White - 0.004, 0))^-2.2).
fn tone_curve(ls: f32, w2: f32, use_filmic: bool) -> f32 {
    if use_filmic {
        let x = max(ls - 0.004, 0.0);
        return pow(filmic(x), 2.2) * w2;
    }
    return ls * (1.0 + ls * w2) / (1.0 + ls);
}

// One pixel of the chain. `a_x` and `a_y` are the adapted pair, `bloom` the bloom colour.
fn retail_tonemap(scene_colour: vec3<f32>, a_x: f32, a_y: f32, bloom: vec3<f32>) -> vec3<f32> {
    let l = max(luminance(scene_colour), 1.0e-5);
    // Retail has no clamp here and divides by a_x. A zero adapted value (a black frame) is a NaN
    // there (https://github.com/Mudcrab-Team/skyrim_decomp/pull/1059). The ratio is 1 at steady
    // state, so 1 stands in for it.
    var ratio = 1.0;
    if a_x > 0.0 {
        ratio = a_y / a_x;
    }
    let lt = tone_curve(l * ratio, hdr.curve.y, hdr.curve.x > 0.5);
    // The bloom enters where the toned luminance is low.
    var colour = scene_colour * (lt / l) + bloom * saturate(hdr.adapt.z - lt);
    // Saturation about the luminance of the colour, then the tint toward that luminance.
    let l1 = luminance(colour);
    colour = vec3<f32>(l1) + hdr.grade.x * (colour - vec3<f32>(l1));
    colour = colour + hdr.grade.w * (vec3<f32>(l1) * hdr.tint.rgb - colour);
    // Brightness and contrast are multipliers. The contrast pivot is the adapted x value.
    colour = hdr.grade.z * (hdr.grade.y * colour - vec3<f32>(a_x)) + vec3<f32>(a_x);
    colour = saturate(finite(colour));
    colour = pow(colour, vec3<f32>(hdr.curve.z));
    // The fade variant is drawn when the fade amount is above 0.
    if hdr.curve.w > 0.0 {
        colour = colour + hdr.curve.w * (hdr.fade.rgb - colour);
    }
    return colour;
}

// Retail writes the value to an R8G8B8A8_UNORM backbuffer with no transfer function (see
// research_notes, https://github.com/Mudcrab-Team/mudcrab/blob/main/research_notes/Skyrim%20native%20tone%20mapping/output_transfer_trace.md). Mudcrab's
// surface encodes linear values to sRGB on write, so the display value is stored as its linear
// decode and the byte that reaches the screen equals retail's.
fn srgb_to_linear(colour: vec3<f32>) -> vec3<f32> {
    let low = colour / 12.92;
    let high = pow((colour + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, colour <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let source = textureLoad(scene, vec2<i32>(in.position.xy), 0);
    let adapted = finite(textureSampleLevel(adapted_texture, linear_clamp, in.uv, 0.0).rgb);
    let bloom = finite(textureSampleLevel(bloom_texture, linear_clamp, in.uv, 0.0).rgb);
    let display = retail_tonemap(finite(source.rgb), adapted.x, adapted.y, bloom);
    return vec4<f32>(srgb_to_linear(display), source.a);
}
