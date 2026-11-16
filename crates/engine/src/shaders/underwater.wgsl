// The underwater post-pass: retail's depth-of-field and fog effect (`ISDepthOfFieldFogged`),
// RF-WATER-031 and RF-WATER-052 to RF-WATER-054. `underwater_pass.rs` runs a separable blur of the
// frame and then this file's composite, which blends the sharp frame toward the blurred one by the
// view-space depth and then toward the water's fog colour by the same depth.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct UnderwaterPass {
    // rgb = the fog colour (linear), w = the fog share at full depth.
    fog_color: vec4<f32>,
    // x = fog near, y = fog far (WATR DNAM +0x90, +0x94), z = the camera's near plane.
    fog_range: vec4<f32>,
    // x = blur focus, y = blur range, z = blur strength (RF-WATER-015, RF-WATER-053).
    blur: vec4<f32>,
}

@group(0) @binding(0) var<uniform> settings: UnderwaterPass;
@group(0) @binding(1) var source: texture_2d<f32>;
// The blurred frame and the opaque depth (the water bed-depth snapshot, reverse-Z NDC depth, 0 for
// the sky and negative when unresolved); only the composite reads them.
@group(0) @binding(2) var blurred: texture_2d<f32>;
@group(0) @binding(3) var bed_depth: texture_2d<f32>;

// The five-tap blur weights of `ISDepthOfFieldBlurHCS` and `ISDepthOfFieldBlurVCS` (RF-WATER-054);
// they sum to 1.
const WEIGHT_CENTRE: f32 = 0.38774;
const WEIGHT_NEAR: f32 = 0.24477;
const WEIGHT_FAR: f32 = 0.06136;
// The smallest depth taken as a real point, so a pixel of sky (depth 0, infinitely far) divides to a
// huge finite distance (an engineering guard; retail's sky is the far plane).
const DEPTH_FLOOR: f32 = 1.0e-30;

fn blur(position: vec2<f32>, axis: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(source));
    let centre = vec2<i32>(position);
    let last = size - vec2<i32>(1);
    let a = textureLoad(source, clamp(centre - axis * 2, vec2<i32>(0), last), 0);
    let b = textureLoad(source, clamp(centre - axis, vec2<i32>(0), last), 0);
    let c = textureLoad(source, centre, 0);
    let d = textureLoad(source, clamp(centre + axis, vec2<i32>(0), last), 0);
    let e = textureLoad(source, clamp(centre + axis * 2, vec2<i32>(0), last), 0);
    return (a + e) * WEIGHT_FAR + (b + d) * WEIGHT_NEAR + c * WEIGHT_CENTRE;
}

@fragment
fn blur_horizontal(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return blur(in.position.xy, vec2<i32>(1, 0));
}

@fragment
fn blur_vertical(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return blur(in.position.xy, vec2<i32>(0, 1));
}

@fragment
fn composite(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(in.position.xy);
    let sharp = textureLoad(source, pixel, 0);
    let z = textureLoad(bed_depth, pixel, 0).x;
    // A negative depth is the unresolved sentinel: the depth is unknown this frame, so the frame
    // is left as it is.
    if z < 0.0 {
        return sharp;
    }
    // The view-space depth: for Bevy's reverse-Z projection z = near / d, so d = near / z, and the
    // sky (z = 0) is infinitely far (RF-WATER-052: the sky is fully fogged).
    let depth = settings.fog_range.z / max(z, DEPTH_FLOOR);
    let fog_near = settings.fog_range.x;
    let fog_far = settings.fog_range.y;
    // The blur share on the far side of the focus only (flags 18, RF-WATER-053), then the fog share
    // (RF-WATER-052).
    let k = clamp((depth - settings.blur.x) / settings.blur.y, 0.0, 1.0) * settings.blur.z;
    let colour = mix(sharp.rgb, textureLoad(blurred, pixel, 0).rgb, k);
    let fog = clamp((depth - fog_near) / (fog_far - fog_near), 0.0, 1.0) * settings.fog_color.w;
    return vec4<f32>(mix(colour, settings.fog_color.rgb, fog), sharp.a);
}
