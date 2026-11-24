#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_functions,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, main_pass_post_lighting_processing},
    view_transformations::position_world_to_clip,
}

struct ObjectLighting {
    mean: vec4<f32>,
    x: vec4<f32>,
    y: vec4<f32>,
    z: vec4<f32>,
    sun_direction: vec4<f32>,
    sun_rgb: vec4<f32>,
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
}

// skyrim_decomp#1003 G02. Creation axes (X,Y,Z) become runtime axes (X,Z,-Y). The inputs are
// one directional ambient slope per creation axis for one colour channel.
fn runtime_slope(x: f32, y: f32, z: f32) -> vec3<f32> {
    return vec3(x, z, -y);
}

// skyrim_decomp#1003 G02. `Rᵀ × v` for model axes expressed in world space. Component i is
// the projection of `v` on model axis i.
fn to_model_space(
    axis_x: vec3<f32>, axis_y: vec3<f32>, axis_z: vec3<f32>, v: vec3<f32>,
) -> vec3<f32> {
    return vec3(dot(axis_x, v), dot(axis_y, v), dot(axis_z, v));
}

// skyrim_decomp#1003 G02 and G05. The mean is the intercept. Each row dots with (N,1). No clamp.
fn object_ambient(
    mean: vec3<f32>, row_r: vec3<f32>, row_g: vec3<f32>, row_b: vec3<f32>, n: vec3<f32>,
) -> vec3<f32> {
    return mean + vec3(dot(n, row_r), dot(n, row_g), dot(n, row_b));
}

// skyrim_decomp#1003 G03 and G05. The dot is saturated. The RGB already holds the sunlight scale.
fn object_sun(n: vec3<f32>, sun_direction: vec3<f32>, sun_rgb: vec3<f32>) -> vec3<f32> {
    return saturate(dot(n, sun_direction)) * sun_rgb;
}

// skyrim_decomp#1003 G05. Diffuse RGB multiplies the light sum, then vertex RGB.
fn object_lit(
    sun: vec3<f32>, ambient: vec3<f32>, diffuse: vec3<f32>, vertex_rgb: vec3<f32>,
) -> vec3<f32> {
    return (sun + ambient) * diffuse * vertex_rgb;
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

    // skyrim_decomp#1003 G02 and G03. Object LOD uses uniform scale. Rotate the world inputs
    // into the atlas basis space. Axis i of the model is the normalised column i of `world`.
    let axis_x = normalize(world[0].xyz);
    let axis_y = normalize(world[1].xyz);
    let axis_z = normalize(world[2].xyz);
    out.ambient_r = to_model_space(
        axis_x, axis_y, axis_z, runtime_slope(lighting.x.r, lighting.y.r, lighting.z.r),
    );
    out.ambient_g = to_model_space(
        axis_x, axis_y, axis_z, runtime_slope(lighting.x.g, lighting.y.g, lighting.z.g),
    );
    out.ambient_b = to_model_space(
        axis_x, axis_y, axis_z, runtime_slope(lighting.x.b, lighting.y.b, lighting.z.b),
    );
    out.sun_direction = to_model_space(axis_x, axis_y, axis_z, lighting.sun_direction.xyz);

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

    // skyrim_decomp#1003 G02 to G05.
    let tangent_normal = decode_object_atlas(textureSample(normal_atlas, atlas_sampler, in.uv).rgb);
    let normal = normalize(mat3x3(in.red, in.green, in.blue) * tangent_normal);
    let ambient = object_ambient(
        lighting.mean.rgb, in.ambient_r, in.ambient_g, in.ambient_b, normal,
    );
    let sun = object_sun(normal, in.sun_direction, lighting.sun_rgb.rgb);
    // The ordinary exterior light assignment skips the stock object LOD root (G04).
    // STAND-IN(#946): use the stock zero property emission and omit shader-object additive
    // colour. Native projected detail and auxiliary outputs have no matching engine inputs.
    // Technique 13 has no vertex colour attribute: its vertex shader supplies white (G01).
    var vertex_rgb = vec3(1.0);
#ifdef VERTEX_COLORS
    vertex_rgb = in.color.rgb;
#endif
    let lit = object_lit(sun, ambient, diffuse.rgb, vertex_rgb);
    // STAND-IN(#946): fog and output use Bevy's distance fog, as terrain LOD and near objects do.
    // The retail equation is `M = L + F(Cf - L)`, `Q = L - kM`, `E = SQ`,
    // `out = min(L, QS + B) - TE`. The frame flags S and T are unverified (NEEDS 17 in the
    // research lane): with S=1 and T=0 the fog has almost no effect on colour, and with
    // S=1 and T=1 it is `min(kM, B)`. Do not choose between them until a source settles it.
    var out: FragmentOutput;
    // STAND-IN(#946): alpha omits the retail `b2[3].z` factor (G05), the property alpha times
    // the pass fade. No engine input supplies it. Its value for stock object LOD is unverified.
    out.color = main_pass_post_lighting_processing(input, vec4(lit, diffuse.a));
    return out;
}
