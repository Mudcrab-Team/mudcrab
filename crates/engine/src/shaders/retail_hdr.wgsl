#define_import_path engine::retail_hdr

// Shared types of the retail HDR chain (`retail_hdr.rs`, #796, #361). The uniform is the Rust
// `RetailHdr` component, field for field.
struct RetailHdr {
    // x = 1 when the filmic curve is selected (`bUseFilmicCurve`), y = W2 (the white-point
    // coefficient of the selected curve), z = 1 / fGamma, w = the fade amount.
    curve: vec4<f32>,
    // x = saturation, y = brightness, z = contrast (all multipliers, neutral 1), w = tint amount.
    grade: vec4<f32>,
    // rgb = the tint colour.
    tint: vec4<f32>,
    // rgb = the fade colour.
    fade: vec4<f32>,
    // x = the x adaptation rate, y = the y adaptation rate (per frame, from the smoothed frame
    // time), z = ReceiveBloomThreshold.
    adapt: vec4<f32>,
    // x = the bright-pass threshold (BloomThreshold + fGlobalBloomThresholdBoost), y = BloomScale,
    // z = the blur radius k in 1 to 7, w = k_r, the output scale.
    bloom: vec4<f32>,
    // The blur weights of radius k: entry u + 7 holds the weight of tap u, in x. Zero beyond k.
    weights: array<vec4<f32>, 15>,
    output: vec4<u32>,
}

// The luminance weights of the shipped image-space shaders (NEEDS 18).
const LUMA: vec3<f32> = vec3<f32>(0.2125, 0.7154, 0.0721);

fn luminance(colour: vec3<f32>) -> f32 {
    return dot(colour, LUMA);
}

// The largest 16-bit float, the format of the chain's targets.
const HALF_MAX: f32 = 65504.0;

// A guard of Mudcrab, with no effect on a finite value: a NaN reads as 0 and an infinity as the
// largest value of the targets, so one bad scene pixel cannot reach the adapted pair and the
// whole screen. The test reads the bits, because fast math may drop a NaN comparison.
fn finite(value: vec3<f32>) -> vec3<f32> {
    let bits = bitcast<vec3<u32>>(value) & vec3<u32>(0x7fffffffu);
    let number = select(value, vec3<f32>(0.0), bits > vec3<u32>(0x7f800000u));
    return clamp(number, vec3<f32>(-HALF_MAX), vec3<f32>(HALF_MAX));
}
