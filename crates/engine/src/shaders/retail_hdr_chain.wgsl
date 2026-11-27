// The passes of retail's HDR chain before the tonemap (NEEDS 18 D4 and NEEDS 27, reviewed):
// the 4-tap downsample, the luminance steps down to the 2 by 2 adapted pair, the adaptation and
// the bloom. `retail_hdr.rs` orders them. Every pass is one full-screen triangle into a
// Rgba16Float target. `retail_hdr.rs` holds the same arithmetic as a CPU reference.
//
// Tap offsets are in source texels around the source position of the destination texel centre.
// The 4-tap table is (+-1, +-1) with weight 1/4. The 16-tap table is -1.5, -0.5, 0.5 and 1.5 in
// both axes with weight 1/16.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput
#import engine::retail_hdr::{RetailHdr, finite, luminance}

@group(0) @binding(0) var<uniform> hdr: RetailHdr;
@group(0) @binding(1) var source: texture_2d<f32>;
// The previous adapted pair, read by the adapt pass only.
@group(0) @binding(2) var previous: texture_2d<f32>;
@group(0) @binding(3) var linear_clamp: sampler;
@group(0) @binding(4) var nearest_clamp: sampler;

fn texel() -> vec2<f32> {
    return 1.0 / vec2<f32>(textureDimensions(source));
}

// The average of the 4-tap table, with bilinear samples.
fn average4(uv: vec2<f32>) -> vec3<f32> {
    let step = texel();
    var sum = vec3<f32>(0.0);
    for (var j = 0; j < 2; j++) {
        for (var i = 0; i < 2; i++) {
            let offset = vec2<f32>(f32(i) * 2.0 - 1.0, f32(j) * 2.0 - 1.0);
            sum += finite(textureSampleLevel(source, linear_clamp, uv + offset * step, 0.0).rgb);
        }
    }
    return sum * 0.25;
}

// The luminance average of the 16-tap table. The adapt pass samples bilinearly, the luminance
// steps sample the nearest texel.
fn average16(uv: vec2<f32>, use_linear: bool) -> f32 {
    let step = texel();
    var sum = 0.0;
    for (var j = 0; j < 4; j++) {
        for (var i = 0; i < 4; i++) {
            let offset = vec2<f32>(f32(i) - 1.5, f32(j) - 1.5);
            let at = uv + offset * step;
            if use_linear {
                sum += luminance(finite(textureSampleLevel(source, linear_clamp, at, 0.0).rgb));
            } else {
                sum += luminance(finite(textureSampleLevel(source, nearest_clamp, at, 0.0).rgb));
            }
        }
    }
    return sum * 0.0625;
}

// The effect input to kHDR_DOWNSAMPLE0.
@fragment
fn downsample4(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(average4(in.uv), 1.0);
}

// The first luminance step: 4 taps, luminance in all three channels.
@fragment
fn luminance4(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let l = luminance(average4(in.uv));
    return vec4<f32>(l, l, l, 1.0);
}

// A later luminance step, and the last step on a frame without an adapted pair: the pair then is
// (n, n, n), so the tonemap ratio is 1.
@fragment
fn luminance16(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let l = average16(in.uv, false);
    return vec4<f32>(l, l, l, 1.0);
}

fn is_nan(value: f32) -> bool {
    return (bitcast<u32>(value) & 0x7fffffffu) > 0x7f800000u;
}

// delta = target - previous; step = sign(delta * rate) * min(|delta|, max(|delta * rate|, 1/256));
// new = previous + step. At rate 0 the channel does not move (SPEC-1 section 10, correction C1).
// A NaN target keeps the previous value.
fn adapt_channel(previous_value: f32, measured: f32, rate: f32) -> f32 {
    if is_nan(measured) {
        return previous_value;
    }
    let delta = measured - previous_value;
    let step = sign(delta * rate) * min(abs(delta), max(abs(delta * rate), 1.0 / 256.0));
    return previous_value + step;
}

// The adapt draw replaces the last luminance step: it takes the 16 taps itself and moves each
// channel of the previous pair toward that average. R holds A.x and G holds A.y.
@fragment
fn adapt(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let measured = average16(in.uv, true);
    let old = finite(textureSampleLevel(previous, nearest_clamp, in.uv, 0.0).rgb).rg;
    let x = adapt_channel(old.x, measured, hdr.adapt.x);
    let y = adapt_channel(old.y, measured, hdr.adapt.y);
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn weight(tap: i32) -> f32 {
    return hdr.weights[tap + 7].x;
}

// Pass 0 of the bloom: the bright pass with the vertical blur, k_r * BloomScale *
// sum w * max(c / k_r - threshold, 0), per channel. The taps are whole target texels apart.
@fragment
fn bright_blur_vertical(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    // One target texel in uv: the derivative of the full-screen triangle's uv, 1 / H of the
    // blur-swap target.
    let row = abs(dpdy(in.uv.y));
    let k = i32(hdr.bloom.z);
    let k_r = hdr.bloom.w;
    var sum = vec3<f32>(0.0);
    for (var u = -k; u <= k; u++) {
        let at = in.uv + vec2<f32>(0.0, f32(u) * row);
        let colour = finite(textureSampleLevel(source, nearest_clamp, at, 0.0).rgb);
        sum += weight(u) * max(colour / k_r - vec3<f32>(hdr.bloom.x), vec3<f32>(0.0));
    }
    return vec4<f32>(finite(k_r * hdr.bloom.y * sum), 1.0);
}

// Pass 1 of the bloom: the plain horizontal blur. k_r cancels.
@fragment
fn blur_horizontal(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(source));
    let k = i32(hdr.bloom.z);
    var sum = vec3<f32>(0.0);
    for (var u = -k; u <= k; u++) {
        let at = in.uv + vec2<f32>(f32(u) / size.x, 0.0);
        sum += weight(u) * finite(textureSampleLevel(source, nearest_clamp, at, 0.0).rgb);
    }
    return vec4<f32>(finite(sum), 1.0);
}
