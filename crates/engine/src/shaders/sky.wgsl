// The sky dome: a gradient of the weather's Horizon, Sky-Lower and Sky-Upper colours by the
// elevation of the view ray, with the dome's alpha fading the lowest degrees out. The colour
// table is built on the CPU (`SkyUniform` in sky.rs) from the measured dome weights; this shader
// only interpolates it. The sky is not fogged and not lit.

#import bevy_pbr::{
    mesh_functions::{get_world_from_local, mesh_position_local_to_world},
    mesh_view_bindings::view,
    view_transformations::position_world_to_clip,
}

const ROW_COUNT: u32 = 16u;

struct SkyUniform {
    // Row elevations in degrees, four rows to a vector.
    elevations: array<vec4<f32>, 4>,
    // Per row: the sRGB-encoded colour and the alpha.
    colours: array<vec4<f32>, 16>,
    // x: linear brightness scale.
    brightness: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> sky: SkyUniform;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
}

struct SkyVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
}

@vertex
fn vertex(vertex: Vertex) -> SkyVertexOutput {
    let world_from_local = get_world_from_local(vertex.instance_index);
    let world_position = mesh_position_local_to_world(world_from_local, vec4<f32>(vertex.position, 1.0));
    let clip = position_world_to_clip(world_position.xyz);
    var out: SkyVertexOutput;
    // Bevy's projection is reverse-Z with the far plane at infinity, so a clip depth of zero puts
    // the dome behind everything; the depth test (greater-or-equal) still lets it through where
    // nothing has been drawn. This is the equivalent of the vanilla sky shader's `.xyww`.
    out.clip_position = vec4<f32>(clip.xy, 0.0, clip.w);
    out.world_position = world_position.xyz;
    return out;
}

fn row_elevation(row: u32) -> f32 {
    return sky.elevations[row / 4u][row % 4u];
}

fn srgb_to_linear(colour: vec3<f32>) -> vec3<f32> {
    let low = colour / 12.92;
    let high = pow((colour + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, colour <= vec3<f32>(0.04045));
}

fn dome_colour(elevation: f32) -> vec4<f32> {
    if elevation <= row_elevation(0u) {
        return sky.colours[0];
    }
    for (var row = 1u; row < ROW_COUNT; row += 1u) {
        let above = row_elevation(row);
        if elevation <= above {
            let below = row_elevation(row - 1u);
            let t = (elevation - below) / max(above - below, 1.0e-6);
            return mix(sky.colours[row - 1u], sky.colours[row], t);
        }
    }
    return sky.colours[ROW_COUNT - 1u];
}

@fragment
fn fragment(in: SkyVertexOutput) -> @location(0) vec4<f32> {
    // The elevation of the ray from the viewing camera, so a mirrored reflection camera sees the
    // gradient mirrored too.
    let direction = normalize(in.world_position - view.world_position);
    let elevation = degrees(asin(clamp(direction.y, -1.0, 1.0)));
    let colour = dome_colour(elevation);
    // Vanilla mixes the 8-bit weather colours as they are; the mix is done in sRGB and the
    // result converted for the linear render target.
    return vec4<f32>(srgb_to_linear(colour.rgb) * sky.brightness.x, colour.a);
}
