#define_import_path engine::terrain_normal

// The one source of the near landscape's tangent-space normal convention, shared by `terrain.wgsl`
// and `terrain_merge/terrain_merged.wgsl`. `terrain_normal_frame_tests` in `terrain_normal.rs`
// reads this file, so a change to the sign or to the frame that makes them disagree fails there.
//
// Skyrim's `_n` maps are DirectX-convention: the green channel follows increasing texture V (down
// the image). The texture is sampled with no V flip and the converter does not flip green, so V
// grows with the image row.
//
// `terrain_tbn` builds its frame from the mesh's own coordinates. `build_terrain_quadrant_mesh` and
// `build_merged_mesh` put grid point (x, y) at world (x, h, -y) and texture coordinate (x, y) / 32,
// so U grows along world +X and V along world -Z. For a flat normal +Y the frame is
// (t, b, n) = (+X, cross(+Y, +X), +Y) = (+X, -Z, +Y): its bitangent is dP/dV, the direction a
// DirectX green channel points. The lit normal of a relief h(u, v) is (-h_u, -h_v, 1) in the frame
// (dP/du, dP/dv, n), and DirectX green stores the -h_v term along +V. So the DirectX green channel
// needs no flip here. The sign is +1.
//
// The object, grass and skin paths flip because Bevy builds glTF tangents with the bitangent along
// -V (see `flip_normal_map_y` in `nif_material.rs`). The terrain does not use glTF tangents.
// Issue #963 recorded the earlier value, -1, which tilted every north-south slope the wrong way.
const NORMAL_GREEN_SIGN: f32 = 1.0;

// The tangent-plane tilt a stored texel decodes to: red along +U, green along +V.
fn terrain_tilt(sample_xy: vec2<f32>) -> vec2<f32> {
    let xy = sample_xy * 2.0 - 1.0;
    return vec2<f32>(xy.x, NORMAL_GREEN_SIGN * xy.y);
}

// The unit tangent-space normal a stored texel decodes to. Z is rebuilt from the tilt.
fn terrain_tangent_normal(sample_xy: vec2<f32>) -> vec3<f32> {
    let tilt = terrain_tilt(sample_xy);
    return vec3<f32>(tilt, sqrt(max(0.0, 1.0 - dot(tilt, tilt))));
}

// The terrain's tangent frame. Its texture coordinates run exactly along world +X (u) and world -Z
// (v), so the frame is analytic: the u axis made perpendicular to the interpolated normal, and the
// v axis from their cross product. The mesh's tangent attribute cannot be used: it carries weights.
fn terrain_tbn(n: vec3<f32>) -> mat3x3<f32> {
    let u_axis = vec3<f32>(1.0, 0.0, 0.0);
    let t = normalize(u_axis - n * dot(n, u_axis) + vec3<f32>(0.0, 0.0, 1.0e-6));
    return mat3x3<f32>(t, cross(n, t), n);
}
