// One vertex transform and coverage function for depth and color. Masked materials
// use texture/material alpha; authored vertex alpha remains available for animation.
#ifdef GRASS_SCENE
#import bevy_pbr::{
    mesh_view_bindings::{view, lights},
    shadows::fetch_directional_shadow,
}
#else
#import bevy_render::view::View
@group(0) @binding(0) var<uniform> view: View;
#endif
#ifdef TONEMAP_IN_SHADER
#import bevy_core_pipeline::tonemapping::tone_mapping
#endif

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
    normal_params: vec4<f32>,
}
@group(#{GRASS_GROUP}) @binding(0) var<uniform> grass: Grass;
@group(#{GRASS_GROUP}) @binding(1) var albedo: texture_2d<f32>;
@group(#{GRASS_GROUP}) @binding(2) var albedo_sampler: sampler;
@group(#{GRASS_GROUP}) @binding(3) var wind: texture_2d<f32>;
@group(#{GRASS_GROUP}) @binding(4) var wind_sampler: sampler;
@group(#{GRASS_GROUP}) @binding(5) var normal_map: texture_2d<f32>;
@group(#{GRASS_GROUP}) @binding(6) var normal_sampler: sampler;

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
#ifdef TANGENT
    @location(5) tangent: vec4<f32>,
#endif
}
struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) normal: vec3<f32>,
    @location(3) light: vec3<f32>,
    @location(4) world_position: vec4<f32>,
    @location(5) tangent: vec4<f32>,
}
fn wind_weight(alpha:f32) -> f32 { return clamp(alpha,0.0,1.0); }
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
    let brightness = f32(input.packed.z & 255u) / 255.0;
    let flags = u32(grass.absolute_grid.z);
    var p = (grass.model * vec4(input.position,1.0)).xyz;
    var n = vec3(0.0,1.0,0.0);
#ifdef NORMAL
    n = normalize((grass.model_normal * vec4(input.normal,0.0)).xyz);
#endif
    var t = vec3(1.0,0.0,0.0);
    var handedness = 0.0;
#ifdef TANGENT
    t = (grass.model * vec4(input.tangent.xyz,0.0)).xyz;
    handedness = input.tangent.w * sign(determinant(mat3x3(grass.model[0].xyz,grass.model[1].xyz,grass.model[2].xyz)));
#endif
    // xEdit GRAS.DATA flags: vertex lighting=1, uniform scaling=2, fit slope=4.
    if (flags & 2u) != 0u { p *= scale; } else { p.y *= scale; n.y /= max(scale,0.001); t.y *= scale; }
    p = rotate_y(p, rotation); n = rotate_y(n, rotation); t = rotate_y(t, rotation);
    if (flags & 4u) != 0u {
        let up = normalize(vec3(signed_byte(input.packed.z >> 8u), signed_byte(input.packed.z >> 24u), -signed_byte(input.packed.z >> 16u)));
        // Quantization can make a steep normal exactly parallel to Z.
        let axis = select(vec3(0.0,0.0,1.0),vec3(1.0,0.0,0.0),abs(up.z)>0.99);
        let tangent = normalize(cross(up, axis));
        let basis = mat3x3(tangent,up,cross(tangent,up));
        p = basis*p; n = basis*n; t = basis*t;
    }
    let absolute_xy = grass.absolute_grid.xy + xy;
    let phase = grass.fade.z / max(grass.height_wind.z,0.01);
    let gust = textureSampleLevel(wind,wind_sampler,absolute_xy / 8192.0 + vec2(phase*0.035,phase*0.02),0.0).r * 2.0 - 1.0;
    var sway = 1.0;
#ifdef COLOR
    sway = wind_weight(input.color.a);
#endif
    if grass.height_wind.z > 0.0 { p.x += gust * max(p.y,0.0) * 0.10 * sway; p.z += gust * max(p.y,0.0) * 0.035 * sway; }
    let root = (grass.world_from_cell * vec4(xy.x,height,-xy.y,1.0)).xyz;
    let world = grass.world_from_cell * vec4(p + vec3(xy.x,height,-xy.y),1.0);
    out.position = view.clip_from_world * world;
    out.world_position = world;
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
    // Authored RGB tints albedo; alpha weights wind independently of coverage.
    out.color *= vec4(input.color.rgb,1.0);
#endif
    out.normal = normalize((grass.world_from_cell * vec4(n,0.0)).xyz);
    out.tangent = vec4((grass.world_from_cell * vec4(t,0.0)).xyz,handedness);
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
    // Keep the authored blade normal on both raster sides, matching the k=0
    // measured model. Flipping a back face would restore unwanted direct sun.
    let sun_direction = grass.sun_direction.xyz;
    let sun_color = grass.sun_color.rgb;
    let diffuse = max(dot(normalize(normal),normalize(sun_direction)),0.0);
    // Interpolate only direct illumination; ambient is added once per fragment.
    return sun_color*diffuse;
}
// Reuse terrain's prepared directional shadow maps and cascade/filter/bias policy.
// Disabled shadows (or no directional light) leave the direct term unchanged.
fn directional_shadow(input:Varyings) -> f32 {
#ifdef GRASS_SCENE
    if lights.n_directional_lights > 0u {
        if (lights.directional_lights[0].flags & 1u) != 0u {
            return fetch_directional_shadow(0u,input.world_position,normalize(input.normal),
                (view.view_from_world*input.world_position).z,input.position.xy);
        }
    }
#endif
    return 1.0;
}
// Native tangent-space decode; normal textures are linear data. Source NIF
// bitangent is converted to glTF handedness; DirectX-tagged maps flip Y once.
fn tangent_normal(sampled:vec3<f32>, n:vec3<f32>, t:vec3<f32>, b:vec3<f32>) -> vec3<f32> {
    let v = sampled * 2.0 - vec3(1.0);
    return normalize(t*v.x + b*v.y + n*v.z);
}
fn mapped_normal(input:Varyings) -> vec3<f32> {
    var sampled = textureSample(normal_map,normal_sampler,input.uv).rgb;
    sampled.y = 0.5 + (sampled.y-0.5)*grass.normal_params.y;
    let n = normalize(input.normal);
    var t = input.tangent.xyz - n*dot(n,input.tangent.xyz);
    var b = cross(n,t)*input.tangent.w;
    // Older/modded GLBs can reconstruct a frame from retained position/UVs.
    let px = dpdx(input.world_position.xyz); let py = dpdy(input.world_position.xyz);
    let ux = dpdx(input.uv); let uy = dpdy(input.uv);
    let det = ux.x*uy.y - ux.y*uy.x;
    if input.tangent.w != 0.0 {
        // The normal texture uses transformed UVs, so its frame follows the
        // inverse UV transform too. A singular mapping falls back below.
        let u = grass.uv_transform[0].xy; let v = grass.uv_transform[1].xy;
        let uv_sign = sign(u.x*v.y-v.x*u.y);
        let authored_t = t;
        t = (authored_t*v.y-b*u.y)*uv_sign;
        b = (b*u.x-authored_t*v.x)*uv_sign;
    }
    if input.tangent.w == 0.0 {
        t = (px*uy.y - py*ux.y)*sign(det);
        b = (py*ux.x - px*uy.x)*sign(det);
        t -= n*dot(n,t); b -= n*dot(n,b);
    }
    if grass.normal_params.x == 0.0 || dot(t,t)<0.000001 || dot(b,b)<0.000001 { return n; }
    return tangent_normal(sampled,n,normalize(t),normalize(b));
}
@fragment
fn fragment(vertex_input:Varyings,@builtin(sample_index) sample_index:u32) -> Fragment {
    var input = vertex_input;
    input.normal = mapped_normal(input);
    if grass.normal_params.x != 0.0 { input.light = lighting(input.normal); }
    let base = textureSample(albedo,albedo_sampler,input.uv) * input.color;
    var out:Fragment;
    out.mask = coverage(base.a,sample_index);
    let direct = select(lighting(input.normal),input.light,(u32(grass.absolute_grid.z)&1u)!=0u);
    let shadow = directional_shadow(input);
    let light = (grass.ambient.rgb + direct*shadow) * view.exposure / 3.14159265;
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
