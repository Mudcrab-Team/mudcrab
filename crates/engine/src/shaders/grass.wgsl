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
#ifdef DISTANCE_FOG
#import bevy_pbr::{mesh_view_bindings::fog, mesh_view_types::FOG_MODE_POWER_RAMP_BLENDED, pbr_functions::apply_fog}
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
    // x fade start, y fade end, z output scale cb2[16].w (RF-SHADER-004, RF-SHADER-001), w fade disabled.
    fade: vec4<f32>,
    absolute_grid: vec4<f32>,
    sun_direction: vec4<f32>,
    sun_color: vec4<f32>,
    ambient: vec4<f32>,
    uv_transform: mat3x3<f32>,
    normal_params: vec4<f32>,
    tuning_light: vec4<f32>,
    tuning_surface: vec4<f32>,
    tuning_color: vec4<f32>,
    tuning_specular: vec4<f32>,
}
@group(#{GRASS_GROUP}) @binding(0) var<uniform> grass: Grass;
@group(#{GRASS_GROUP}) @binding(1) var albedo: texture_2d<f32>;
@group(#{GRASS_GROUP}) @binding(2) var albedo_sampler: sampler;
@group(#{GRASS_GROUP}) @binding(3) var normal_map: texture_2d<f32>;
@group(#{GRASS_GROUP}) @binding(4) var normal_sampler: sampler;

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
    @location(6) tangent: vec4<f32>,
    @location(5) root_fraction: f32,
}
fn signed_byte(value: u32) -> f32 {
    return f32(i32((value & 255u) ^ 128u) - 128) / 127.0;
}
fn rotate_y(p: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle); let s = sin(angle);
    return vec3(c*p.x+s*p.z, p.y, -s*p.x+c*p.z);
}
// Keep these pure helpers executable by the CPU production-formula fixtures.
fn tuning_brightness(original:f32, land:f32, range:f32, tint_strength:f32, range_strength:f32) -> f32 {
    if tint_strength == 1.0 && range_strength == 1.0 { return original; }
    return clamp(mix(1.0,land,tint_strength) * mix(1.0,range,range_strength),0.0,1.0);
}
fn tuning_diffuse(normal:vec3<f32>, sun:vec3<f32>, light:vec4<f32>, surface:vec4<f32>) -> f32 {
    let cosine = dot(normalize(normal),normalize(sun));
    let wrapped = clamp((cosine + surface.x)/(1.0 + surface.x),0.0,1.0);
    return mix(1.0,wrapped,light.w) + surface.w * max(-cosine,0.0);
}
fn tuning_illumination(ambient:vec3<f32>, direct:vec3<f32>, shadow:f32, exposure:f32, light:vec4<f32>, surface:vec4<f32>, specular:vec4<f32>) -> vec3<f32> {
    // RF-GRASS-005: surface.z is the shadow floor (fShadowClampValue, 0.3 by default).
    let visibility = surface.z + (1.0-surface.z) * mix(1.0,shadow,surface.y);
    return (ambient*light.y + direct*light.z*visibility) * exposure / 3.14159265;
}
fn tuning_finish(color:vec3<f32>, root_fraction:f32, light:vec4<f32>, tint:vec4<f32>) -> vec3<f32> {
    let luminance = dot(color,vec3(0.2126,0.7152,0.0722));
    return max(mix(vec3(luminance),color,tint.x),vec3(0.0)) * light.x * (1.0-tint.w*root_fraction);
}
// Bevy 0.19 pbr_lighting.wgsl: D_GGX, V_SmithGGXCorrelated and F_Schlick.
// Dielectric F0=.04; perceptual roughness squared to alpha, clamped away from zero.
fn tuning_ggx(ndh:f32, ndv:f32, ndl:f32, vdh:f32, perceptual_roughness:f32) -> f32 {
    let roughness = max(perceptual_roughness*perceptual_roughness,0.007921);
    let a = ndh * roughness;
    let k = roughness / (1.0-ndh*ndh+a*a);
    let distribution = k*k / 3.14159265;
    let a2 = roughness*roughness;
    let lambda_v = ndl*sqrt((ndv-a2*ndv)*ndv+a2);
    let lambda_l = ndv*sqrt((ndl-a2*ndl)*ndl+a2);
    let visibility = 0.5/max(lambda_v+lambda_l,0.000001);
    let fresnel = 0.04+0.96*pow(1.0-vdh,5.0);
    return distribution*visibility*fresnel*ndl;
}
fn tuning_sheen(normal:vec3<f32>, world:vec3<f32>) -> vec3<f32> {
    if grass.tuning_specular.x == 0.0 { return vec3(0.0); }
    let n = normalize(normal);
    let l = normalize(grass.sun_direction.xyz);
    let eye = view.world_position-world;
    let v = eye/max(length(eye),0.000001);
    let half_vector = l+v;
    let h = half_vector/max(length(half_vector),0.000001);
    let response = tuning_ggx(max(dot(n,h),0.0),max(dot(n,v),0.0001),max(dot(n,l),0.0),clamp(dot(v,h),0.0,1.0),grass.tuning_specular.y);
    return grass.sun_color.rgb * grass.tuning_light.z * grass.tuning_specular.x * response * view.exposure;
}
@vertex
fn vertex(input: Vertex) -> Varyings {
    var out: Varyings;
    let xy = vec2<f32>(f32(input.packed.x & 65535u), f32(input.packed.x >> 16u)) * (4096.0 / 65535.0);
    let height = grass.height_wind.x + f32(input.packed.y & 65535u) * grass.height_wind.y / 65535.0;
    let rotation = f32((input.packed.y >> 16u) & 255u) * (6.28318530718 / 256.0);
    let scale = f32(input.packed.y >> 24u) * (2.0 / 255.0);
    let brightness = tuning_brightness(f32(input.packed.z & 255u)/255.0,
        f32((input.packed.w >> 18u)&127u)/127.0,
        f32(input.packed.w >> 25u)*(2.0/127.0)-1.0,
        grass.tuning_color.y,grass.tuning_color.z);
    let flags = u32(grass.absolute_grid.z);
    var p = (grass.model * vec4(input.position,1.0)).xyz;
    var n = vec3(0.0,1.0,0.0);
#ifdef NORMAL
    n = normalize((grass.model_normal * vec4(input.normal,0.0)).xyz);
#endif
    // Retail grass lights each vertex with the instance's up axis, not the mesh normal
    // (RF-SHADER-003): the terrain normal with Fit to Slope, else straight up.
    var instance_normal = vec3(0.0,1.0,0.0);
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
        let authored_up = vec3(signed_byte(input.packed.z >> 8u), signed_byte(input.packed.z >> 24u), -signed_byte(input.packed.z >> 16u));
        let up = authored_up / max(length(authored_up),0.000001);
        // Tangent selection follows spud (MIT, chasmlol/spud); notice: grass/SPUD-MIT.txt.
        // Work in Creation axes before converting the least-component tangent to runtime axes.
        let creation_normal = vec3(up.x,-up.z,up.y);
        let magnitude = abs(creation_normal);
        var creation_tangent: vec3<f32>;
        if magnitude.x <= magnitude.y && magnitude.x <= magnitude.z {
            creation_tangent = vec3(0.0,-creation_normal.z,creation_normal.y);
        } else if magnitude.y <= magnitude.x && magnitude.y <= magnitude.z {
            creation_tangent = vec3(-creation_normal.z,0.0,creation_normal.x);
        } else {
            creation_tangent = vec3(-creation_normal.y,creation_normal.x,0.0);
        }
        let runtime_tangent = vec3(creation_tangent.x,creation_tangent.z,-creation_tangent.y);
        let tangent = runtime_tangent / max(length(runtime_tangent),0.000001);
        let basis = mat3x3(tangent,up,cross(tangent,up));
        p = basis*p; n = basis*n; t = basis*t;
        instance_normal = up;
    }
    // No wind displacement: retail wind needs the blended WTHR wind speed and wind direction,
    // which the engine does not have yet (see grass/wind.rs for the sourced amplitude and phase).
    let root = (grass.world_from_cell * vec4(xy.x,height,-xy.y,1.0)).xyz;
    let world = grass.world_from_cell * vec4(p + vec3(xy.x,height,-xy.y),1.0);
    out.position = view.clip_from_world * world;
    out.world_position = world;
    let distance = length((root-view.world_position).xz);
    let fraction = select(clamp((grass.fade.y-distance)/max(grass.fade.y-grass.fade.x,1.0),0.0,1.0),1.0,grass.fade.w != 0.0);
    if f32(input.packed.w & 262143u) >= fraction * grass.absolute_grid.w { out.position = vec4(2.0,2.0,2.0,1.0); }
    out.root_fraction = 0.0;
    out.uv = vec2(0.0);
#ifdef UV
    out.uv = input.uv;
    out.root_fraction = clamp(input.uv.y,0.0,1.0);
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
    let light_normal = normalize((grass.world_from_cell * vec4(instance_normal,0.0)).xyz);
    out.light = lighting(light_normal);
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
    if grass.height_wind.w < 0.0 { return (1u << #{SAMPLES}u) - 1u; }
    let edge = clamp((alpha-grass.height_wind.w)/max(width,0.0001)+0.5,0.0,1.0);
    // Accepted constant interiors, including alpha == cutoff == 1, stay fully covered.
    let interior = select(0.0,1.0,alpha >= grass.height_wind.w);
    let coverage = select(interior,edge,width > 0.0);
    let count = u32(round(coverage*f32(#{SAMPLES})));
    if sample_index >= count { discard; }
    return (1u << #{SAMPLES}u) - 1u;
#endif
}
struct Fragment { @location(0) color:vec4<f32>, @builtin(sample_mask) mask:u32 }
// Direct light per vertex: DirLightColor * NdotL, evaluated on the instance normal
// (RF-SHADER-003). With the retail tuning defaults the direct response is saturate(N.L).
fn lighting(normal:vec3<f32>) -> vec3<f32> {
    return grass.sun_color.rgb * tuning_diffuse(normal,grass.sun_direction.xyz,grass.tuning_light,grass.tuning_surface);
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
// Native UV-frame decode; normal textures are linear data. The Bevy-compatible
// frame uses B opposite increasing V; DirectX-tagged maps flip Y once.
fn tangent_normal(sampled:vec3<f32>, n:vec3<f32>, t:vec3<f32>, b:vec3<f32>) -> vec3<f32> {
    var v = sampled * 2.0 - vec3(1.0);
    if grass.normal_params.z != 0.0 { v.z = sqrt(max(0.0,1.0-dot(v.xy,v.xy))); }
    return normalize(t*v.x + b*v.y + n*v.z);
}
fn mapped_normal(input:Varyings) -> vec3<f32> {
    var sampled = textureSample(normal_map,normal_sampler,input.uv).rgb;
    if grass.normal_params.w != 0.0 { sampled = sampled*0.5+vec3(0.5); }
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
        t = (authored_t*v.y+b*u.y)*uv_sign;
        b = (b*u.x+authored_t*v.x)*uv_sign;
    }
    if input.tangent.w == 0.0 {
        t = (px*uy.y - py*ux.y)*sign(det);
        b = (px*uy.x - py*ux.x)*sign(det);
        t -= n*dot(n,t); b -= n*dot(n,b);
    }
    if grass.normal_params.x == 0.0 || dot(t,t)<0.000001 || dot(b,b)<0.000001 { return n; }
    t = normalize(t);
    let orientation = select(-1.0,1.0,dot(cross(n,t),b) >= 0.0);
    return tangent_normal(sampled,n,t,cross(n,t)*orientation);
}
// Inherit terrain's DistanceFog specialization and prepared view uniforms.
// Lighting already includes exposure; Bevy applies fog in this same HDR space.
fn grass_fog(input_color:vec4<f32>, world_position:vec3<f32>, frag_coord:vec2<f32>) -> vec4<f32> {
#ifdef DISTANCE_FOG
    // Grass is drawn in the alpha-mask pass, so the `skyrim` profile fogs it in the full-screen
    // pass after the opaque stage (`retail_fog.rs`, #891) and not here.
    if fog.mode == FOG_MODE_POWER_RAMP_BLENDED {
        return input_color;
    }
    return apply_fog(fog,input_color,world_position,view.world_position,frag_coord);
#else
    return input_color;
#endif
}
@fragment
fn fragment(vertex_input:Varyings,@builtin(sample_index) sample_index:u32) -> Fragment {
    var input = vertex_input;
    let base = textureSample(albedo,albedo_sampler,input.uv) * input.color;
    var out:Fragment;
    out.mask = coverage(base.a,sample_index);
    // Retail grass pixel output (RF-SHADER-003; shadow floor RF-GRASS-005):
    //   (NdotL * C * DirLightColor * albedo * lerp(0.3, 1, shadowMask) + C * AmbientColor * albedo) * cb2[16].w
    // `base` is albedo * C (vertex colour times the per-instance brightness); input.light is
    // NdotL * DirLightColor from the vertex shader; the shadow floor 0.3 is grass.tuning_surface.z
    // (fShadowClampValue); the ambient term is not shadowed; grass.fade.z is cb2[16].w, the
    // output scale 1/FramebufferRange (RF-SHADER-001, RF-SHADER-004). Only grass applies it
    // today; terrain, sky and water do not (open question: does retail apply it there too?).
    // Exposure and 1/pi: 1/pi is exact for the sun (it equals Bevy's Lambert for the sun
    // illuminance). Bevy's own ambient has no 1/pi, so this ambient is about pi times darker
    // than a Bevy PBR surface with the same ambient input. Retail has no such factor. The
    // sun:ambient balance is unverified until the engine light units match retail (#706).
    let shadow = directional_shadow(input);
    let light = tuning_illumination(grass.ambient.rgb,input.light,shadow,view.exposure,grass.tuning_light,grass.tuning_surface,grass.tuning_specular);
    let visibility = grass.tuning_surface.z + (1.0-grass.tuning_surface.z)*mix(1.0,shadow,grass.tuning_surface.y);
    let sheen = tuning_sheen(input.normal,input.world_position.xyz)*visibility;
    out.color = vec4(tuning_finish((base.rgb*light+sheen)*grass.fade.z,input.root_fraction,grass.tuning_light,grass.tuning_color),1.0);
    out.color = grass_fog(out.color,input.world_position.xyz,input.position.xy);
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
