#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_functions,
    mesh_view_bindings as view_bindings,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    view_transformations::position_world_to_clip,
}

struct ObjectLighting {
    mean: vec4<f32>,
    x: vec4<f32>,
    y: vec4<f32>,
    z: vec4<f32>,
    sun_direction: vec4<f32>,
    sun_rgb: vec4<f32>,
    fog_ramp: vec4<f32>,
    fog_near: vec4<f32>,
    fog_far: vec4<f32>,
    output_controls: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var normal_atlas: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var atlas_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> lighting: ObjectLighting;

struct ObjectVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
    @location(8) red: vec3<f32>,
    @location(9) green: vec3<f32>,
}

struct ObjectFragment {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) blue: vec3<f32>,
    @location(2) uv: vec2<f32>,
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    @location(6) @interpolate(flat) instance_index: u32,
#endif
    @location(8) red: vec3<f32>,
    @location(9) green: vec3<f32>,
    @location(10) @interpolate(flat) ambient_r: vec3<f32>,
    @location(11) @interpolate(flat) ambient_g: vec3<f32>,
    @location(12) @interpolate(flat) ambient_b: vec3<f32>,
    @location(13) @interpolate(flat) sun_direction: vec3<f32>,
    @location(14) fog: vec4<f32>,
}

@vertex
fn vertex(in: ObjectVertex) -> ObjectFragment {
    let world = mesh_functions::get_world_from_local(in.instance_index);
    var out: ObjectFragment;
    out.world_position = mesh_functions::mesh_position_local_to_world(world, vec4(in.position, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    out.uv = in.uv;
    out.red = in.red;
    out.green = in.green;
    out.blue = in.normal;

    // https://github.com/Mudcrab-Team/skyrim_decomp/pull/1003 G02 and G03.
    // Object LOD uses uniform scale. Rotate the world inputs into the atlas basis space.
    let model_from_world = transpose(mat3x3(
        normalize(world[0].xyz), normalize(world[1].xyz), normalize(world[2].xyz),
    ));
    // Creation axes (X,Y,Z) become runtime axes (X,Z,-Y).
    let world_slopes = transpose(mat3x3(lighting.x.rgb, lighting.z.rgb, -lighting.y.rgb));
    out.ambient_r = model_from_world * world_slopes[0];
    out.ambient_g = model_from_world * world_slopes[1];
    out.ambient_b = model_from_world * world_slopes[2];
    out.sun_direction = model_from_world * lighting.sun_direction.xyz;

    out.fog = vec4(0.0);
    if lighting.fog_ramp.y > 0.0 {
        // STAND-IN(#946): reconstruct forward depth from Bevy's reversed clip depth.
        // The exact retail projection is unverified. See #1003 G05.
        let clip_xyz = vec3(out.position.xy, out.position.w - out.position.z);
        let ramp = lighting.fog_ramp;
        let strength = min(pow(saturate(length(clip_xyz) * ramp.y - ramp.x), ramp.z), ramp.w);
        out.fog = vec4(mix(lighting.fog_near.rgb, lighting.fog_far.rgb, strength), strength);
    }
#ifdef VERTEX_COLORS
    out.color = in.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = in.instance_index;
#endif
    return out;
}

// skyrim_decomp#948 G03. The blue channel directly supplies tangent-space Z.
fn decode_object_atlas(rgb: vec3<f32>) -> vec3<f32> {
    return rgb * 2.0 - vec3(1.0);
}

@fragment
fn fragment(in: ObjectFragment, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var mesh: VertexOutput;
    mesh.world_position = in.world_position;
    mesh.position = in.position;
    mesh.world_normal = normalize(in.blue);
    mesh.uv = in.uv;
#ifdef VERTEX_COLORS
    mesh.color = vec4(1.0);
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    mesh.instance_index = in.instance_index;
#endif
    let input = pbr_input_from_standard_material(mesh, is_front);
    let diffuse = alpha_discard(input.material, input.material.base_color);

    // https://github.com/Mudcrab-Team/skyrim_decomp/pull/1003 G02–G05.
    let tangent_normal = decode_object_atlas(textureSample(normal_atlas, atlas_sampler, in.uv).rgb);
    let normal = normalize(mat3x3(in.red, in.green, in.blue) * tangent_normal);
    let ambient = lighting.mean.rgb + vec3(
        dot(normal, in.ambient_r), dot(normal, in.ambient_g), dot(normal, in.ambient_b),
    );
    let sun = saturate(dot(normal, in.sun_direction)) * lighting.sun_rgb.rgb;
    // The ordinary exterior light assignment skips the stock object LOD root (G04).
    // STAND-IN(#946): use the stock zero property emission and omit shader-object additive
    // colour. Native projected detail and auxiliary outputs have no matching engine inputs.
    var vertex_rgb = vec3(1.0);
#ifdef VERTEX_COLORS
    vertex_rgb = in.color.rgb;
#endif
    let lit = (sun + ambient) * diffuse.rgb * vertex_rgb;
    var fogged = lit;
#ifdef DISTANCE_FOG
    if view_bindings::fog.mode != 0u {
        fogged = mix(lit, in.fog.rgb, in.fog.a);
    }
#endif
    let controls = lighting.output_controls;
    let scaled = controls.z * (lit - controls.x * fogged);
    var out: FragmentOutput;
    out.color = vec4(min(lit, scaled + vec3(controls.y)) - controls.w * scaled, diffuse.a);
    return out;
}
