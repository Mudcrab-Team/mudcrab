// mudcrab: this whole file is a Mudcrab-only addition: Skyrim's near `Lighting` pixel shader
// (technique 0 and its output tail) for a `StandardMaterial` that carries a `RetailSurface`. Bevy's
// light model is not used here: no Burley, no GGX, no `EnvBRDFApprox`, no exposure and no Bevy fog.
// Sections below are the Mudcrab near-lighting specification SPEC-1 (static retail facts, build
// 1.7.104.0). See `vendor/BEVY_PATCHES.md`.
//
// `crates/engine/src/retail_lighting.rs` is the CPU twin of this arithmetic, and the GPU pixel
// tests (`retail_lighting_gpu_tests.rs`) draw the specification's worked examples through this
// file. Keep them in step.

#define_import_path bevy_pbr::retail

#import bevy_pbr::{
    ambient::directional_ambient_for_normal,
    clustered_forward as clustering,
    mesh_types::MESH_FLAGS_SHADOW_RECEIVER_BIT,
    mesh_view_bindings as view_bindings,
    mesh_view_types,
    pbr_types,
    shadows,
}

// Section 4: `k`, `B` and `B2`, compiled retail defaults.
const RETAIL_OUTPUT_SCALE: f32 = 0.8333333;
const RETAIL_CLAMP_POST_LIT: f32 = 1.0;
const RETAIL_CLAMP_POST_SPEC: f32 = 1.0;

// The camera planes of the retail clip position that the vertex fog measures (section 3.3).
// STAND-IN(#891): the 15 and 10240 of the default path; the spec names no values.
const RETAIL_CAMERA_NEAR: f32 = 15.0;
const RETAIL_CAMERA_FAR: f32 = 10240.0;

// Retail has no light unit. Mudcrab states a retail light of 1 as the Bevy light whose Lambert
// response to a white surface is 1: a directional light of `PI * colour` and a point light of
// `PI * colour`, so that Bevy's own shaders (water, effects) share the scale. This path takes the
// `PI` out again.
const RETAIL_LIGHT_UNIT: f32 = 0.3183098861837907;

// Section 6.1: `a = 1 - sat(|d| / radius)^2`.
fn retail_point_attenuation(distance_squared: f32, inverse_radius_squared: f32) -> f32 {
    return 1.0 - clamp(distance_squared * inverse_radius_squared, 0.0, 1.0);
}

// Section 6.2: `sat(N . normalize(E + l)) ^ g`. A zero base gives 0 for a positive exponent.
fn retail_specular_lobe(n: vec3<f32>, e: vec3<f32>, l: vec3<f32>, glossiness: f32) -> f32 {
    let base = clamp(dot(n, normalize(e + l)), 0.0, 1.0);
    return select(0.0, pow(base, glossiness), base > 0.0);
}

// Section 3.3: the length of the retail clip position before the divide by W.
fn retail_clip_length(
    view_position: vec3<f32>, scale_x: f32, scale_y: f32, near: f32, far: f32,
) -> f32 {
    let depth = -view_position.z;
    let clip = vec3<f32>(
        view_position.x * scale_x,
        view_position.y * scale_y,
        far / (far - near) * (depth - near),
    );
    return length(clip);
}

// Section 3.3: `F = min(sat(dist * r - near * r) ^ power, max)` with `r = 1 / (far - near)`. When
// near and far are equal retail's writer gives a ramp whose factor is 0.
fn retail_fog_factor(distance: f32, near: f32, far: f32, power: f32, max_factor: f32) -> f32 {
    if !(far > near) {
        return 0.0;
    }
    let r = 1.0 / (far - near);
    let s = clamp(distance * r - near * r, 0.0, 1.0);
    return min(pow(s, power), max_factor);
}

// Section 7, one stage: `Q = L - k M`, `O = min(S Q + B, L) - T S Q`.
fn retail_one_stage(lit: vec3<f32>, f: f32, fog: vec3<f32>, s: f32, t: f32) -> vec3<f32> {
    let m = lit + f * (fog - lit);
    let q = lit - RETAIL_OUTPUT_SCALE * m;
    return min(s * q + vec3<f32>(RETAIL_CLAMP_POST_LIT), lit) - t * s * q;
}

// Section 7, two stages (specular or ambient specular): the highlight joins between them.
fn retail_two_stages(
    lit: vec3<f32>, highlight: vec3<f32>, f: f32, fog: vec3<f32>, s: f32, t: f32,
) -> vec3<f32> {
    let m = lit + f * (fog - lit);
    let x1 = min(s * (lit - RETAIL_OUTPUT_SCALE * m) + vec3<f32>(RETAIL_CLAMP_POST_LIT), lit);
    let y = x1 + highlight;
    let m2 = y + f * (fog - y);
    let q2 = y - RETAIL_OUTPUT_SCALE * m2;
    return min(s * q2 + vec3<f32>(RETAIL_CLAMP_POST_SPEC), y) - t * s * q2;
}

// Sorted blended draws run with `(S, T) = (1, 1)`, every other draw with `(1, 0)` (section 4).
// STAND-IN(#362): retail keeps blended decals in the first stage; Mudcrab draws every blended
// material after the composite, so it takes the second stage's tail.
fn retail_blend_selector(flags: u32) -> f32 {
    let mode = flags & pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS;
    if mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_OPAQUE
        || mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_MASK
        || mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_ALPHA_TO_COVERAGE {
        return 0.0;
    }
    return 1.0;
}

// The output tail of a draw at `world_position`, with the camera's weather or cell fog.
fn retail_output(
    flags: u32,
    world_position: vec3<f32>,
    lit: vec3<f32>,
    highlight: vec3<f32>,
    two_stages: bool,
) -> vec3<f32> {
    var f = 0.0;
    var fog_colour = vec3<f32>(0.0);
#ifdef DISTANCE_FOG
    let mode = view_bindings::fog.mode;
    if (flags & pbr_types::STANDARD_MATERIAL_FLAGS_FOG_ENABLED_BIT) != 0u
        && (mode == mesh_view_types::FOG_MODE_POWER_RAMP
            || mode == mesh_view_types::FOG_MODE_POWER_RAMP_BLENDED) {
        // STAND-IN(#891): retail evaluates the fog at the vertices and interpolates it. This
        // evaluates it per pixel.
        let view_position =
            (view_bindings::view.view_from_world * vec4<f32>(world_position, 1.0)).xyz;
        let distance = retail_clip_length(
            view_position,
            view_bindings::view.clip_from_view[0][0],
            view_bindings::view.clip_from_view[1][1],
            RETAIL_CAMERA_NEAR,
            RETAIL_CAMERA_FAR,
        );
        // The power ramp uniform: `be` = (near, far, power), the far colour in `base_color` with
        // the maximum in its alpha, the near colour in `bi`.
        f = retail_fog_factor(
            distance,
            view_bindings::fog.be.x,
            view_bindings::fog.be.y,
            view_bindings::fog.be.z,
            view_bindings::fog.base_color.a,
        );
        fog_colour = view_bindings::fog.bi
            + f * (view_bindings::fog.base_color.rgb - view_bindings::fog.bi);
    }
#endif
    let t = retail_blend_selector(flags);
    if two_stages {
        return retail_two_stages(lit, highlight, f, fog_colour, 1.0, t);
    }
    return retail_one_stage(lit, f, fog_colour, 1.0, t);
}

#ifndef PREPASS_FRAGMENT
// Sections 6 and 7 for one fragment. `base_color` already holds the diffuse texel times the vertex
// colour, so `L = base_color x (S + P + Am + Em)` and no colour or sum is clamped.
// STAND-IN(#362): retail's additive constant `cb1[8]` is zero here, its start-up value.
// STAND-IN(#999): every point light of the fragment's cluster adds. Retail shades at most 7 per
// geometry, in the order of the geometry's own light list.
// STAND-IN(#362): the sun and local shadow masks are Mudcrab's shadow maps.
fn retail_lighting(in: pbr_types::PbrInput) -> vec4<f32> {
    let flags = in.material.flags;
    let two_stages = (flags & pbr_types::STANDARD_MATERIAL_FLAGS_RETAIL_SPECULAR_BIT) != 0u;
    let glossiness = in.material.retail_specular.w;
    // A blended surface with the alpha test (SPEC-1 section 6.3): the alpha is the texel's
    // times the property alpha times the vertex alpha.
    if (flags & pbr_types::STANDARD_MATERIAL_FLAGS_RETAIL_ALPHA_TEST_BIT) != 0u
        && in.material.base_color.a < in.material.alpha_cutoff {
        discard;
    }
    let receives_shadows = (in.flags & MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u;
    let position = in.world_position.xyz;
    let view_z = dot(vec4<f32>(
        view_bindings::view.view_from_world[0].z,
        view_bindings::view.view_from_world[1].z,
        view_bindings::view.view_from_world[2].z,
        view_bindings::view.view_from_world[3].z
    ), in.world_position);

    var diffuse = vec3<f32>(0.0);
    var highlight = vec3<f32>(0.0);

    // The sun or the interior key light: `sat(N . L) x cb2[1] x mask` (section 2.3).
    let n_directional_lights = view_bindings::lights.n_directional_lights;
    for (var i: u32 = 0u; i < n_directional_lights; i = i + 1u) {
        let light = &view_bindings::lights.directional_lights[i];
        var mask = 1.0;
        if receives_shadows
            && ((*light).flags & mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
            mask = shadows::fetch_directional_shadow(
                i, in.world_position, in.world_normal, view_z, in.frag_coord.xy,
            );
        }
        let direction = (*light).direction_to_light;
        let colour = (*light).color.rgb * RETAIL_LIGHT_UNIT * mask;
        diffuse += clamp(dot(in.N, direction), 0.0, 1.0) * colour;
        highlight += retail_specular_lobe(in.N, in.V, direction, glossiness) * colour;
    }

    // Point lights: `sat(N . l) x colour x attenuation` (section 6.1).
    let cluster_index =
        clustering::view_fragment_cluster_index(in.frag_coord.xy, view_z, in.is_orthographic);
    let ranges = clustering::unpack_clusterable_object_index_ranges(cluster_index);
    for (var i: u32 = ranges.first_point_light_index_offset;
            i < ranges.first_spot_light_index_offset;
            i = i + 1u) {
        let light_id = clustering::get_clusterable_object_id(i);
        let to_light = view_bindings::clustered_lights.data[light_id].position_radius.xyz - position;
        let attenuation = retail_point_attenuation(
            dot(to_light, to_light),
            view_bindings::clustered_lights.data[light_id].color_inverse_square_range.w,
        );
        var mask = 1.0;
        if receives_shadows
            && (view_bindings::clustered_lights.data[light_id].flags
                & mesh_view_types::POINT_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
            mask = shadows::fetch_point_shadow(
                light_id, in.world_position, in.world_normal, in.frag_coord.xy,
            );
        }
        let direction = normalize(to_light);
        let colour = view_bindings::clustered_lights.data[light_id].color_inverse_square_range.rgb
            * RETAIL_LIGHT_UNIT * attenuation * mask;
        diffuse += clamp(dot(in.N, direction), 0.0, 1.0) * colour;
        highlight += retail_specular_lobe(in.N, in.V, direction, glossiness) * colour;
    }

    // Directional ambient, per normal, unscaled and unclamped (section 2.5).
    var ambient = view_bindings::lights.ambient_color.rgb;
    if view_bindings::lights.ambient_directional != 0u {
        ambient = directional_ambient_for_normal(in.N);
    }

    let sum = diffuse + ambient + in.material.emissive.rgb;
    let lit = in.material.base_color.rgb * sum;
    // `Spec` has no diffuse texel and no vertex colour (section 6.2).
    // STAND-IN(#362): the specular distance fade `cb2[3].y` is 1; its distance input is open.
    // STAND-IN(#362): ambient specular (key bit 17) is not drawn; its predicate is not traced.
    let specular = highlight * in.retail_specular_mask * in.material.retail_specular.rgb;
    let colour = retail_output(flags, position, lit, specular, two_stages);
    return vec4<f32>(colour, in.material.base_color.a);
}
#endif // PREPASS_FRAGMENT
