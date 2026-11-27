// mudcrab: this whole file is a Mudcrab-only addition. Bevy has one physically based light model
// per material and no way to shade a surface with another game's equations. See
// `vendor/BEVY_PATCHES.md`.

//! A [`StandardMaterial`] surface that Skyrim's near `Lighting` shader would draw.
//!
//! When [`StandardMaterial::retail_surface`] is `Some`, the forward lighting shader replaces
//! Bevy's whole light model (Burley diffuse, the GGX lobe, `EnvBRDFApprox`, exposure and Bevy's
//! distance fog) with retail's unitless sum and output tail (`render/pbr_retail.wgsl`). `None`,
//! the default, leaves every shader output identical to unpatched Bevy.
//!
//! [`StandardMaterial`]: crate::StandardMaterial
//! [`StandardMaterial::retail_surface`]: crate::StandardMaterial::retail_surface

use bevy_math::Vec3;
use bevy_reflect::Reflect;

/// The material inputs of the retail Lighting shader that [`StandardMaterial`] has no field for.
///
/// The other inputs come from the material's own fields: the base colour and its texture (the
/// diffuse map times the vertex colour), the normal map (its alpha is the specular mask), the
/// emissive colour (the shader's additive emission, unitless) and the alpha mode.
///
/// [`StandardMaterial`]: crate::StandardMaterial
#[derive(Clone, Copy, Debug, Default, PartialEq, Reflect)]
pub struct RetailSurface {
    /// Specular colour times specular strength, unitless (retail `cb1[4].xyz`).
    pub specular_color: Vec3,
    /// Glossiness, the specular exponent (retail `cb1[4].w`).
    pub glossiness: f32,
    /// The surface runs the specular term and the second output stage.
    pub specular: bool,
    /// The emissive multiple an external emittance source's colour takes. The shader does not
    /// read it: the emission it reads is [`crate::StandardMaterial::emissive`].
    pub emissive_multiple: f32,
}

impl RetailSurface {
    /// A surface with no specular term: one output stage.
    pub const MATTE: Self = Self {
        specular_color: Vec3::ZERO,
        glossiness: 0.0,
        specular: false,
        emissive_multiple: 0.0,
    };
}
