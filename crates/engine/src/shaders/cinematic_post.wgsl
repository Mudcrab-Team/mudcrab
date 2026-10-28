// Experimental clear-day grade using reported IMGS CNAM/TNAM values.
// Bevy post_saturation runs first in linear RGB. This pass converts linear
// post-tonemap RGB to sRGB, mixes toward tinted luminance, applies brightness
// and contrast around an artistic pivot, then converts back to linear RGB.
// The sRGB domain preserves the approved appearance; retail shader slot mapping,
// adapted pivot, tone curve and final transfer remain unverified.
// Negative input and out-of-gamut results are clipped. Alpha passes unchanged;
// retail bloom-feed and alpha grading are not reproduced here.
// Snapshot modes instead evaluate saturation and tint together in the selected
// domain, with the screenshot's rounded values. They retain Bevy's tone curve
// and the fixed artistic pivot; neither mode is a calibrated Skyrim renderer.
// An explicit retail-curve experiment instead disables Bevy tonemapping and
// applies the audited scalar-luminance curve to exposed HDR before the same
// sRGB grade. White-derived CPU coefficients and fixed P.x/P.y overrides are
// supplied separately. Native adaptation, bloom, final power and input-domain
// parity remain unverified; the approved default never enters this branch.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

struct SkyrimCinematicPost {
    brightness: f32,
    contrast: f32,
    tint_amount: f32,
    pivot: f32,
    tint_rgb: vec3<f32>,
    enabled: f32,
    saturation: f32,
    linear_domain: u32,
    tone_operator: u32,
    tone_coefficient: f32,
    sampled_luminance: f32,
    target_luminance: f32,
}

@group(0) @binding(2) var<uniform> settings: SkyrimCinematicPost;

// The luminance weights of the shipped image-space shaders.
const LUMA: vec3<f32> = vec3<f32>(0.2125, 0.7154, 0.0721);

fn linear_to_srgb(color: vec3<f32>) -> vec3<f32> {
    let c = max(color, vec3<f32>(0.0));
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(color: vec3<f32>) -> vec3<f32> {
    let lo = color / 12.92;
    let hi = pow((color + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, color <= vec3<f32>(0.04045));
}

fn retail_scalar_curve(rgb: vec3<f32>) -> vec3<f32> {
    let y = max(dot(rgb, LUMA), 0.00001);
    let z = y * (settings.target_luminance / settings.sampled_luminance);
    var q: f32;
    if settings.tone_operator == 2u {
        let x = max(z - 0.004, 0.0);
        let h = (x * (6.2 * x + 0.5)) / (x * (6.2 * x + 1.7) + 0.06);
        q = settings.tone_coefficient * pow(h, 2.2);
    } else {
        q = z * (1.0 + settings.tone_coefficient * z) / (1.0 + z);
    }
    return rgb * (q / y);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    var color = textureSample(screen_texture, texture_sampler, in.uv);
    if settings.tone_operator != 0u {
        color = vec4<f32>(retail_scalar_curve(color.rgb), color.a);
    }
    if settings.enabled < 0.5 {
        return color;
    }
    var rgb = color.rgb;
    if settings.linear_domain == 0u {
        rgb = linear_to_srgb(rgb);
    }
    let luma = dot(rgb, LUMA);
    // Keep legacy arithmetic unchanged when Bevy owns saturation. In snapshot
    // modes the original luminance feeds both saturation and the tint blend.
    if settings.saturation != 1.0 {
        rgb = mix(vec3<f32>(luma), rgb, settings.saturation);
    }
    rgb = mix(rgb, vec3<f32>(luma) * settings.tint_rgb, settings.tint_amount);
    rgb = settings.contrast * (settings.brightness * rgb - vec3<f32>(settings.pivot))
        + vec3<f32>(settings.pivot);
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if settings.linear_domain == 0u {
        rgb = srgb_to_linear(rgb);
    }
    return vec4<f32>(rgb, color.a);
}
