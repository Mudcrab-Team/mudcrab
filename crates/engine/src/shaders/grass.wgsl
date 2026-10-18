// One vertex transform and coverage function for depth and color. Masked materials
// use authored vertex alpha in both passes (unlike the standard depth path).
#import bevy_render::view::View
#ifdef TONEMAP_IN_SHADER
#import bevy_core_pipeline::tonemapping::tone_mapping
#endif

@group(0) @binding(0) var<uniform> view: View;
struct Grass {
    world_from_cell: mat4x4<f32>,
    model: mat4x4<f32>,
    model_normal: mat4x4<f32>,
    color: vec4<f32>,
    height_wind: vec4<f32>,
    fade: vec4<f32>,
    absolute_grid: vec4<f32>,
    sun_direction: vec4<f32>,
    sun_color: vec4<f32>,
    ambient: vec4<f32>,
    uv_transform: mat3x3<f32>,
}
@group(1) @binding(0) var<uniform> grass: Grass;
@group(1) @binding(1) var albedo: texture_2d<f32>;
@group(1) @binding(2) var albedo_sampler: sampler;
@group(1) @binding(3) var wind: texture_2d<f32>;
@group(1) @binding(4) var wind_sampler: sampler;

struct Vertex {
    @location(0) position: vec3<f32>,
#ifdef NORMAL
    @location(1) normal: vec3<f32>,
#endif
#ifdef UV
    @location(2) uv: vec2<f32>,
#endif
#ifdef COLOR
    @location(3) color: vec4<f32>,
#endif
    @location(4) packed: vec4<u32>,
}
struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) normal: vec3<f32>,
    @location(3) light: vec3<f32>,
}
fn signed_byte(value: u32) -> f32 {
    return f32(i32((value & 255u) ^ 128u) - 128) / 127.0;
}
fn rotate_y(p: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle); let s = sin(angle);
    return vec3(c*p.x+s*p.z, p.y, -s*p.x+c*p.z);
}
@vertex
fn vertex(input: Vertex) -> Varyings {
    var out: Varyings;
    let xy = vec2<f32>(f32(input.packed.x & 65535u), f32(input.packed.x >> 16u)) * (4096.0 / 65535.0);
    let height = grass.height_wind.x + f32(input.packed.y & 65535u) * grass.height_wind.y / 65535.0;
    let rotation = f32((input.packed.y >> 16u) & 255u) * (6.28318530718 / 256.0);
    let scale = f32(input.packed.y >> 24u) * (2.0 / 255.0);
    let brightness = f32(input.packed.z & 255u) * (2.0 / 255.0);
    let flags = u32(grass.absolute_grid.z);
    var p = (grass.model * vec4(input.position,1.0)).xyz;
    var n = vec3(0.0,1.0,0.0);
#ifdef NORMAL
    n = normalize((grass.model_normal * vec4(input.normal,0.0)).xyz);
#endif
    // xEdit GRAS.DATA flags: vertex lighting=1, uniform scaling=2, fit slope=4.
    if (flags & 2u) != 0u { p *= scale; } else { p.y *= scale; n.y /= max(scale,0.001); }
    p = rotate_y(p, rotation); n = rotate_y(n, rotation);
    if (flags & 4u) != 0u {
        let up = normalize(vec3(signed_byte(input.packed.z >> 8u), signed_byte(input.packed.z >> 24u), -signed_byte(input.packed.z >> 16u)));
        // Quantization can make a steep normal exactly parallel to Z.
        let axis = select(vec3(0.0,0.0,1.0),vec3(1.0,0.0,0.0),abs(up.z)>0.99);
        let tangent = normalize(cross(up, axis));
        let basis = mat3x3(tangent,up,cross(tangent,up));
        p = basis*p; n = basis*n;
    }
    let absolute_xy = grass.absolute_grid.xy + xy;
    let phase = grass.fade.z / max(grass.height_wind.z,0.01);
    let gust = textureSampleLevel(wind,wind_sampler,absolute_xy / 8192.0 + vec2(phase*0.035,phase*0.02),0.0).r * 2.0 - 1.0;
    if grass.height_wind.z > 0.0 { p.x += gust * max(p.y,0.0) * 0.10; p.z += gust * max(p.y,0.0) * 0.035; }
    let root = (grass.world_from_cell * vec4(xy.x,height,-xy.y,1.0)).xyz;
    let world = grass.world_from_cell * vec4(p + vec3(xy.x,height,-xy.y),1.0);
    out.position = view.clip_from_world * world;
    let distance = length((root-view.world_position).xz);
    let fraction = select(clamp((grass.fade.y-distance)/max(grass.fade.y-grass.fade.x,1.0),0.0,1.0),1.0,grass.fade.w != 0.0);
    if f32(input.packed.w) >= fraction * grass.absolute_grid.w { out.position = vec4(2.0,2.0,2.0,1.0); }
    out.uv = vec2(0.0);
#ifdef UV
    out.uv = input.uv;
#endif
    // Share authored texture coordinates between color and depth coverage.
    // Apply offsets to the zero default when the mesh has no UV attribute.
    out.uv = (grass.uv_transform * vec3(out.uv,1.0)).xy;
    out.color = grass.color * vec4(vec3(brightness),1.0);
#ifdef COLOR
    out.color *= input.color;
#endif
    out.normal = normalize((grass.world_from_cell * vec4(n,0.0)).xyz);
    out.light = lighting(out.normal);
    return out;
}

// Use the same alpha coverage in the color and depth-only passes. Per-sample
// discard avoids dynamic sample_mask exports, which crash AMD's Vulkan shader
// compiler on the tested RX 9070 XT driver. The surviving sample prefix is
// identical to the original (1 << count) - 1 mask; MSAA remains enabled.
fn coverage(alpha:f32,sample_index:u32) -> u32 {
#if SAMPLES == 1
    // A negative threshold encodes an authored opaque material.
    if grass.height_wind.w < 0.0 { return 1u; }
    if alpha < grass.height_wind.w { discard; }
    return 1u;
#else
    // Evaluate derivatives before branching; opaque materials ignore all alpha inputs.
    let width = fwidth(alpha);
    if grass.height_wind.w < 0.0 { return 15u; }
    let edge = clamp((alpha-grass.height_wind.w)/max(width,0.0001)+0.5,0.0,1.0);
    // Accepted constant interiors, including alpha == cutoff == 1, stay fully covered.
    let interior = select(0.0,1.0,alpha >= grass.height_wind.w);
    let coverage = select(interior,edge,width > 0.0);
    let count = u32(round(coverage*4.0));
    if sample_index >= count { discard; }
    return 15u;
#endif
}
struct Fragment { @location(0) color:vec4<f32>, @builtin(sample_mask) mask:u32 }
fn lighting(normal:vec3<f32>) -> vec3<f32> {
    let diffuse = abs(dot(normalize(normal),normalize(grass.sun_direction.xyz)));
    return (grass.ambient.rgb + grass.sun_color.rgb*diffuse) * view.exposure / 3.14159265;
}
@fragment
fn fragment(input:Varyings,@builtin(sample_index) sample_index:u32) -> Fragment {
    let base = textureSample(albedo,albedo_sampler,input.uv) * input.color;
    var out:Fragment;
    out.mask = coverage(base.a,sample_index);
    let light = select(lighting(input.normal),input.light,(u32(grass.absolute_grid.z)&1u)!=0u);
    out.color = vec4(base.rgb * light,1.0);
#ifdef TONEMAP_IN_SHADER
    // Match standard materials for LDR targets. HDR is mapped after the main
    // pass by the camera, and depth coverage is independent of display color.
    out.color = tone_mapping(out.color, view.color_grading);
#endif
    return out;
}
@fragment
fn prepass(input:Varyings,@builtin(sample_index) sample_index:u32) -> @builtin(sample_mask) u32 {
    let alpha = textureSample(albedo,albedo_sampler,input.uv).a * input.color.a;
    return coverage(alpha,sample_index);
}
