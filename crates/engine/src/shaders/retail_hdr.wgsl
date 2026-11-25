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
}

// The luminance weights of the shipped image-space shaders (NEEDS 18).
const LUMA: vec3<f32> = vec3<f32>(0.2125, 0.7154, 0.0721);

fn luminance(colour: vec3<f32>) -> f32 {
    return dot(colour, LUMA);
}
