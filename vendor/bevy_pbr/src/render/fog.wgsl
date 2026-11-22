#define_import_path bevy_pbr::fog

#import bevy_pbr::{
    mesh_view_bindings::fog,
    mesh_view_types::Fog,
}

// Fog formulas adapted from:
// https://learn.microsoft.com/en-us/windows/win32/direct3d9/fog-formulas
// https://catlikecoding.com/unity/tutorials/rendering/part-14/
// https://iquilezles.org/articles/fog/ (Atmospheric Fog and Scattering)

fn scattering_adjusted_fog_color(
    fog_params: Fog,
    scattering: vec3<f32>,
) -> vec4<f32> {
    if (fog_params.directional_light_color.a > 0.0) {
        return vec4<f32>(
            fog_params.base_color.rgb
                + scattering * fog_params.directional_light_color.rgb * fog_params.directional_light_color.a,
            fog_params.base_color.a,
        );
    } else {
        return fog_params.base_color;
    }
}

fn linear_fog(
    fog_params: Fog,
    input_color: vec4<f32>,
    distance: f32,
    scattering: vec3<f32>,
) -> vec4<f32> {
    var fog_color = scattering_adjusted_fog_color(fog_params, scattering);
    let start = fog_params.be.x;
    let end = fog_params.be.y;
    fog_color.a *= 1.0 - clamp((end - distance) / (end - start), 0.0, 1.0);
    return vec4<f32>(mix(input_color.rgb, fog_color.rgb, fog_color.a), input_color.a);
}

fn exponential_fog(
    fog_params: Fog,
    input_color: vec4<f32>,
    distance: f32,
    scattering: vec3<f32>,
) -> vec4<f32> {
    var fog_color = scattering_adjusted_fog_color(fog_params, scattering);
    let density = fog_params.be.x;
    fog_color.a *= 1.0 - 1.0 / exp(distance * density);
    return vec4<f32>(mix(input_color.rgb, fog_color.rgb, fog_color.a), input_color.a);
}

fn exponential_squared_fog(
    fog_params: Fog,
    input_color: vec4<f32>,
    distance: f32,
    scattering: vec3<f32>,
) -> vec4<f32> {
    var fog_color = scattering_adjusted_fog_color(fog_params, scattering);
    let distance_times_density = distance * fog_params.be.x;
    fog_color.a *= 1.0 - 1.0 / exp(distance_times_density * distance_times_density);
    return vec4<f32>(mix(input_color.rgb, fog_color.rgb, fog_color.a), input_color.a);
}

// mudcrab: the weather's exterior fog (#891). Ramp: s = saturate((distance - start) / (end - start)),
// intensity F = min(s^power, max), with max in the fog colour's alpha. The fog colour runs from the
// near colour (`bi`) to the far colour (`base_color`) by F, and the lit colour runs to it by F.
fn power_ramp_fog(
    fog_params: Fog,
    input_color: vec4<f32>,
    distance: f32,
    scattering: vec3<f32>,
) -> vec4<f32> {
    let far_color = scattering_adjusted_fog_color(fog_params, scattering);
    let start = fog_params.be.x;
    let end = fog_params.be.y;
    let power = fog_params.be.z;
    let s = clamp((distance - start) / (end - start), 0.0, 1.0);
    // exp2(log2(s) * power) is s^power. At s = 0 the log is -inf and exp2 gives 0 for a positive power.
    let intensity = min(exp2(log2(s) * power), far_color.a);
    let fog_rgb = fog_params.bi + intensity * (far_color.rgb - fog_params.bi);
    return vec4<f32>(mix(input_color.rgb, fog_rgb, intensity), input_color.a);
}

fn atmospheric_fog(
    fog_params: Fog,
    input_color: vec4<f32>,
    distance: f32,
    scattering: vec3<f32>,
) -> vec4<f32> {
    var fog_color = scattering_adjusted_fog_color(fog_params, scattering);
    let extinction_factor = 1.0 - 1.0 / exp(distance * fog_params.be);
    let inscattering_factor = 1.0 - 1.0 / exp(distance * fog_params.bi);
    return vec4<f32>(
        input_color.rgb * (1.0 - extinction_factor * fog_color.a)
            + fog_color.rgb * inscattering_factor * fog_color.a,
        input_color.a
    );
}
