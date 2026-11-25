// mudcrab: this whole file is a Mudcrab-only addition. Bevy has one ambient colour per view and no
// per-normal ambient. See `vendor/BEVY_PATCHES.md`.

//! Retail's directional ambient: a mean colour plus linear normal slopes.
//!
//! Skyrim's near `Lighting` shader evaluates the `DALC` six-colour record per pixel normal, as a
//! mean plus linear slopes dotted with the normal, `ambient = mean + N . slopes` per channel,
//! with no clamp and no scale. The ambient joins the sum of the sun, point lights and emissive
//! term, before the diffuse multiply (skyrim-research NEEDS 26, the comment on Mudcrab issue #1010).
//!
//! Insert [`DirectionalAmbientCube`] with `enabled` set to make Bevy's ambient term read this form
//! instead of the flat [`GlobalAmbientLight`] colour. While `enabled` is `false` (the default)
//! every shader output is identical to unpatched Bevy.
//!
//! [`GlobalAmbientLight`]: bevy_light::GlobalAmbientLight

use bevy_ecs::prelude::Resource;
use bevy_math::Vec3;
use bevy_render::extract_resource::ExtractResource;

/// The `DALC` ambient of the active scene: a mean and three slopes, in linear light.
///
/// ## Axes and face order
///
/// The slopes are in Creation axes (the `DALC` record's own axes, X east, Y north, Z up), the
/// same convention as the object LOD shader's `Ambient` (`lod_object.wgsl`). The shader turns the
/// engine's world normal `n` (X, Y up, Z south) into Creation axes with
/// `creation = (n.x, -n.z, n.y)`. With `c[0..6]` the record's faces in stored order (+X, -X, +Y,
/// -Y, +Z, -Z), per channel:
///
/// ```text
/// mean    = (c0 + c1 + c2 + c3 + c4 + c5) / 6
/// slope_x = (c1 - c0) / 2
/// slope_y = (c3 - c2) / 2
/// slope_z = (c5 - c4) / 2
/// ambient = mean + creation.x * slope_x + creation.y * slope_y + creation.z * slope_z
/// ```
///
/// A normal along +X leans to stored face 1, +Y to face 3 and +Z (up) to face 5.
///
/// Colours are linear and unscaled. The render world multiplies them by the
/// `GlobalAmbientLight` brightness, the same factor the flat ambient takes.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, ExtractResource)]
pub struct DirectionalAmbientCube {
    /// Whether the ambient term uses this form. `false` keeps Bevy's flat ambient colour.
    pub enabled: bool,
    /// The mean of the six faces.
    pub mean: Vec3,
    /// `(c1 - c0) / 2`: the slope along Creation X.
    pub slope_x: Vec3,
    /// `(c3 - c2) / 2`: the slope along Creation Y.
    pub slope_y: Vec3,
    /// `(c5 - c4) / 2`: the slope along Creation Z (up).
    pub slope_z: Vec3,
}

impl DirectionalAmbientCube {
    /// The ambient colour for the engine-space world normal `normal` (X, Y up, Z south), in linear
    /// light and before the brightness scale. This is the CPU twin of `ambient_cube` in
    /// `pbr_ambient.wgsl`: the same axis map and the same arithmetic, with no clamp.
    pub fn evaluate(&self, normal: Vec3) -> Vec3 {
        // mudcrab: Creation (x, y, z) is engine (x, z, -y), so a Creation normal is
        // (engine.x, -engine.z, engine.y). Keep in step with `lod_object.wgsl`.
        let creation = Vec3::new(normal.x, -normal.z, normal.y);
        self.mean
            + self.slope_x * creation.x
            + self.slope_y * creation.y
            + self.slope_z * creation.z
    }
}
