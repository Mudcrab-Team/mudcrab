#define_import_path bevy_pbr::ambient

#import bevy_pbr::{
    lighting::{EnvBRDFApprox, F_AB},
    mesh_view_bindings::lights,
}

// mudcrab: retail's directional ambient (`DirectionalAmbientCube`). `ambient_color` is the mean and the
// three slopes are along Creation X, Y and Z. The engine's world normal (X, Y up, Z south) becomes
// a Creation normal with (n.x, -n.z, n.y), the same map as `lod_object.wgsl`. No clamp, as in
// retail. Keep in step with `DirectionalAmbientCube::evaluate`. Only called when
// `lights.ambient_directional` is not 0.
fn directional_ambient_for_normal(world_normal: vec3<f32>) -> vec3<f32> {
    let creation_normal = vec3<f32>(world_normal.x, -world_normal.z, world_normal.y);
    return lights.ambient_color.rgb
        + creation_normal.x * lights.ambient_slope_x.rgb
        + creation_normal.y * lights.ambient_slope_y.rgb
        + creation_normal.z * lights.ambient_slope_z.rgb;
}

// A precomputed `NdotV` is provided because it is computed regardless,
// but `world_normal` and the view vector `V` are provided separately for more advanced uses.
fn ambient_light(
    world_position: vec4<f32>,
    world_normal: vec3<f32>,
    V: vec3<f32>,
    NdotV: f32,
    diffuse_color: vec3<f32>,
    specular_color: vec3<f32>,
    perceptual_roughness: f32,
    occlusion: vec3<f32>,
) -> vec3<f32> {
    let diffuse_ambient = EnvBRDFApprox(diffuse_color, F_AB(1.0, NdotV));
    let specular_ambient = EnvBRDFApprox(specular_color, F_AB(perceptual_roughness, NdotV));

    // No real world material has specular values under 0.02, so we use this range as a
    // "pre-baked specular occlusion" that extinguishes the fresnel term, for artistic control.
    // See: https://google.github.io/filament/Filament.md.html#specularocclusion
    let specular_occlusion = saturate(dot(specular_color, vec3(50.0 * 0.33)));

    // mudcrab: with the directional ambient the diffuse part is retail's `albedo * ambient`. Retail
    // adds the DALC ambient to the diffuse sum unscaled and multiplies the sum by the diffuse
    // colour (NEEDS 26: nothing scales it). Bevy's `EnvBRDFApprox(diffuse_color, F_AB(1.0, NdotV))`
    // above scales it by about 0.4524 (the polynomial F_AB at roughness 1). The specular ambient
    // term is kept as it is: NEEDS 26 does not describe a retail counterpart.
    if (lights.ambient_directional != 0u) {
        return (diffuse_color + specular_ambient * specular_occlusion) * directional_ambient_for_normal(world_normal) * occlusion;
    }

    return (diffuse_ambient + specular_ambient * specular_occlusion) * lights.ambient_color.rgb * occlusion;
}
