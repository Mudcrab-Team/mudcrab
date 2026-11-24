#import bevy_pbr::{
    clustered_forward,
    forward_io::{VertexOutput, FragmentOutput},
    mesh_functions,
    lighting::getDistanceAttenuation,
    mesh_view_bindings as view_bindings,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, main_pass_post_lighting_processing},
    view_transformations::position_world_to_clip,
}

struct Ambient {
    mean: vec4<f32>,
    x: vec4<f32>,
    y: vec4<f32>,
    z: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var normal_atlas: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var atlas_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> ambient: Ambient;

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
}

@vertex
fn vertex(in: ObjectVertex) -> ObjectFragment {
    let world = mesh_functions::get_world_from_local(in.instance_index);
    var out: ObjectFragment;
    out.world_position = mesh_functions::mesh_position_local_to_world(world, vec4(in.position, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    out.uv = in.uv;
    // Object LOD roots have a rotation and uniform scale. Keep the relative basis lengths
    // through interpolation and normalize only the result of the full transform.
    let basis = mat3x3(world[0].xyz, world[1].xyz, world[2].xyz);
    out.red = basis * in.red;
    out.green = basis * in.green;
    out.blue = basis * in.normal;
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
    mesh.color = in.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    mesh.instance_index = in.instance_index;
#endif
    let input = pbr_input_from_standard_material(mesh, is_front);
    let diffuse = alpha_discard(input.material, input.material.base_color);

    // STAND-IN(#983): base atlas lighting uses the engine's light lists and colour pipeline.
    // Projected detail and auxiliary outputs require retail inputs unavailable here.
    let tangent_normal = decode_object_atlas(textureSample(normal_atlas, atlas_sampler, in.uv).rgb);
    let normal = normalize(mat3x3(in.red, in.green, in.blue) * tangent_normal);
    let creation_normal = vec3(normal.x, -normal.z, normal.y);
    var light = ambient.mean.rgb + creation_normal.x * ambient.x.rgb
        + creation_normal.y * ambient.y.rgb + creation_normal.z * ambient.z.rgb;
    let view = view_bindings::view.view_from_world;
    let view_z = (view * in.world_position).z;
    for (var i = 0u; i < view_bindings::lights.n_directional_lights; i += 1u) {
        let sun = view_bindings::lights.directional_lights[i];
        light += clamp(dot(normal, sun.direction_to_light), 0.0, 1.0) * sun.color.rgb / 3.14159265;
    }
    let cluster = clustered_forward::view_fragment_cluster_index(in.position.xy, view_z, input.is_orthographic);
    let ranges = clustered_forward::unpack_clusterable_object_index_ranges(cluster);
    let first = ranges.first_point_light_index_offset;
    let end = min(ranges.first_spot_light_index_offset, first + 7u);
    for (var i = first; i < end; i += 1u) {
        let point = view_bindings::clustered_lights.data[clustered_forward::get_clusterable_object_id(i)];
        let delta = point.position_radius.xyz - in.world_position.xyz;
        // STAND-IN(#983): G05 in skyrim_decomp#948 uses a radius-based falloff.
        // Our lights carry Bevy intensities, so use the falloff in pbr_lighting.wgsl.
        let distance_squared = dot(delta, delta);
        let angle = clamp(dot(normal, delta) / max(sqrt(distance_squared), 0.0001), 0.0, 1.0);
        let attenuation = getDistanceAttenuation(distance_squared, point.color_inverse_square_range.w);
        light += point.color_inverse_square_range.rgb * angle * attenuation / 3.14159265;
    }
    var out: FragmentOutput;
    let exposed = diffuse.rgb * light * view_bindings::view.exposure;
    out.color = main_pass_post_lighting_processing(input, vec4(exposed, diffuse.a));
    return out;
}
