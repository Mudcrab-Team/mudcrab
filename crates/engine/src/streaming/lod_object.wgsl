#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_functions,
    mesh_view_bindings as view_bindings,
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
    // x is 1 under the `skyrim` visual profile (retail terms) and 0 under `mudcrab` (the engine
    // light scale, the exposure and the Bevy sun). The CPU fills the rows by the same flag.
    profile: vec4<f32>,
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

// skyrim_decomp#1003 G02. The ambient rows for the vertex stage, as the columns of a matrix:
// column 0 is the red row, column 1 green and column 2 blue. Each channel's creation-axis
// slopes (`lighting.x`, `.y`, `.z` of that channel) become runtime axes (X,Z,-Y), then rotate
// into model space with `Rᵀ × v`. Axis i of the model is the normalised column i of the world
// matrix, so component i is the projection on that axis. Object LOD has uniform scale.
fn retail_ambient_rows(
    lighting: ObjectLighting, column_x: vec3<f32>, column_y: vec3<f32>, column_z: vec3<f32>,
) -> mat3x3<f32> {
    return mat3x3(
        vec3(
            dot(normalize(column_x), vec3(lighting.x.r, lighting.z.r, -lighting.y.r)),
            dot(normalize(column_y), vec3(lighting.x.r, lighting.z.r, -lighting.y.r)),
            dot(normalize(column_z), vec3(lighting.x.r, lighting.z.r, -lighting.y.r)),
        ),
        vec3(
            dot(normalize(column_x), vec3(lighting.x.g, lighting.z.g, -lighting.y.g)),
            dot(normalize(column_y), vec3(lighting.x.g, lighting.z.g, -lighting.y.g)),
            dot(normalize(column_z), vec3(lighting.x.g, lighting.z.g, -lighting.y.g)),
        ),
        vec3(
            dot(normalize(column_x), vec3(lighting.x.b, lighting.z.b, -lighting.y.b)),
            dot(normalize(column_y), vec3(lighting.x.b, lighting.z.b, -lighting.y.b)),
            dot(normalize(column_z), vec3(lighting.x.b, lighting.z.b, -lighting.y.b)),
        ),
    );
}

// skyrim_decomp#1003 G02 and G03. The to-sun vector in model space: the same `Rᵀ × v`.
fn retail_sun_direction(
    lighting: ObjectLighting, column_x: vec3<f32>, column_y: vec3<f32>, column_z: vec3<f32>,
) -> vec3<f32> {
    return vec3(
        dot(normalize(column_x), lighting.sun_direction.xyz),
        dot(normalize(column_y), lighting.sun_direction.xyz),
        dot(normalize(column_z), lighting.sun_direction.xyz),
    );
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
    if lighting.profile.x > 0.5 {
        // `skyrim` profile. The basis stays in model space and the light vectors rotate into it.
        out.red = in.red;
        out.green = in.green;
        out.blue = in.normal;
        let rows = retail_ambient_rows(
            lighting, world[0].xyz, world[1].xyz, world[2].xyz,
        );
        out.ambient_r = rows[0];
        out.ambient_g = rows[1];
        out.ambient_b = rows[2];
        out.sun_direction = retail_sun_direction(
            lighting, world[0].xyz, world[1].xyz, world[2].xyz,
        );
    } else {
        // `mudcrab` profile, unchanged. Object LOD roots have a rotation and uniform scale. Keep
        // the relative basis lengths through interpolation and normalize only the result of the
        // full transform.
        let basis = mat3x3(world[0].xyz, world[1].xyz, world[2].xyz);
        out.red = basis * in.red;
        out.green = basis * in.green;
        out.blue = basis * in.normal;
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
    let retail = lighting.profile.x > 0.5;
    var mesh: VertexOutput;
    mesh.world_position = in.world_position;
    mesh.position = in.position;
    // Under `skyrim` the interpolated basis is in model space, so this holds a model-space vector,
    // not a world normal. Nothing later may read it as a world normal.
    mesh.world_normal = normalize(in.blue);
    mesh.uv = in.uv;
#ifdef VERTEX_COLORS
    // `skyrim` applies the vertex colour once, below. `mudcrab` leaves it to the standard material.
    mesh.color = select(in.color, vec4(1.0), retail);
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    mesh.instance_index = in.instance_index;
#endif
    let input = pbr_input_from_standard_material(mesh, is_front);
    let diffuse = alpha_discard(input.material, input.material.base_color);

    let tangent_normal = decode_object_atlas(textureSample(normal_atlas, atlas_sampler, in.uv).rgb);
    let normal = normalize(mat3x3(in.red, in.green, in.blue) * tangent_normal);
    var color = vec3(0.0);
    if retail {
        // skyrim_decomp#1003 G02 to G05.
        let ambient = object_ambient(
            lighting.mean.rgb, in.ambient_r, in.ambient_g, in.ambient_b, normal,
        );
        let sun = object_sun(normal, in.sun_direction, lighting.sun_rgb.rgb);
        // The ordinary exterior light assignment skips the stock object LOD root (G04). This shader
        // has no point-light loop, so placed `LIGH` lights (#955) light the full-detail mesh only.
        // Registry `RF-LIGHT-GPU-004`.
        // STAND-IN(#946): use the stock zero property emission and omit shader-object additive
        // colour. Native projected detail and auxiliary outputs have no matching engine inputs.
        // Technique 13 has no vertex colour attribute: its vertex shader supplies white (G01).
        var vertex_rgb = vec3(1.0);
#ifdef VERTEX_COLORS
        vertex_rgb = in.color.rgb;
#endif
        color = object_lit(sun, ambient, diffuse.rgb, vertex_rgb);
    } else {
        // `mudcrab` profile, unchanged: the engine light scale and the view exposure. The rows are
        // already scaled by the ambient illuminance on the CPU.
        // STAND-IN(#983): base atlas lighting uses the engine's light lists and colour pipeline.
        // Projected detail and auxiliary outputs require retail inputs unavailable here.
        let creation_normal = vec3(normal.x, -normal.z, normal.y);
        var light = lighting.mean.rgb + creation_normal.x * lighting.x.rgb
            + creation_normal.y * lighting.y.rgb + creation_normal.z * lighting.z.rgb;
        for (var i = 0u; i < view_bindings::lights.n_directional_lights; i += 1u) {
            let sun = view_bindings::lights.directional_lights[i];
            light += clamp(dot(normal, sun.direction_to_light), 0.0, 1.0) * sun.color.rgb / 3.14159265;
        }
        // Retail's stock object LOD (BTO, techniques 13 and 15) receives no point lights:
        // skyrim_decomp PR 1003 and NEEDS 14 found no path that fills its light list. This shader
        // has no point-light loop, so placed `LIGH` lights (#955) light the full-detail mesh only.
        // Registry `RF-LIGHT-GPU-004`.
        color = diffuse.rgb * light * view_bindings::view.exposure;
    }
    // Fog: retail has no fog colour in this pixel shader for stock object LOD. The haze is the
    // full-screen depth composite after the opaque stage (`retail_fog.rs`, #1004;
    // skyrim_decomp PR 1061 and 1077; docs/specs/engine/visual-profiles.md). Under the `skyrim`
    // profile that composite fogs this draw, and Bevy's shader fog skips it
    // (`FOG_MODE_POWER_RAMP_BLENDED`) because the material is in the opaque or alpha-mask pass and
    // has no transmission. Do not call `apply_fog` or a retail fog tail here: that would fog twice.
    // Under the `mudcrab` profile the composite does not run, and
    // `main_pass_post_lighting_processing` keeps the per-material distance fog, as for near
    // objects and terrain LOD.
    var out: FragmentOutput;
    // STAND-IN(#946): the `skyrim` alpha omits the retail alpha factor of G05, the property alpha
    // times the pass fade. No engine input supplies it. Its value for stock object LOD is
    // unverified.
    out.color = main_pass_post_lighting_processing(input, vec4(color, diffuse.a));
    return out;
}
