//! Skyrim's near lighting under the `skyrim` visual profile: the units of the scene's lights and a
//! CPU reference of the pixel formulas.
//!
//! The specification is SPEC-1, the near-lighting finding of the skyrim-research lane (static
//! reading of the pinned 1.7.104.0 executable and shader package; no game run and no frame
//! capture). Section numbers below are its sections, and
//! [`docs/specs/engine/near-lighting.md`](../../../docs/specs/engine/near-lighting.md) maps each
//! one to the code.
//!
//! ## Retail has no light unit
//!
//! A colour byte of a light, of the ambient, of the fog or of a vertex becomes `byte / 255`, with
//! no sRGB decode (section 2.1). The sun is `colour x fade x SunlightScale`, a point light is
//! `colour x fade x dimmer`, and the ambient is the `DALC` cube. A pixel is
//! `albedo x (sun + points + ambient + emission)`.
//!
//! ## How the engine writes those numbers into Bevy
//!
//! The vendored shader (`vendor/bevy_pbr/src/render/pbr_retail.wgsl`) draws every surface that
//! carries a [`bevy::pbr::RetailSurface`]. Bevy's own shaders still draw water and effects, so a
//! retail light of 1 is stated as the Bevy light whose Lambert response to a white surface is 1:
//!
//! | Light | Bevy value | The retail shader reads |
//! |---|---|---|
//! | sun, interior key | linear colour, illuminance `PI x fade x SunlightScale` | `colour x illuminance / PI` |
//! | point | linear colour, intensity `4 PI^2 x fade x dimmer` | `colour x intensity / (4 PI) / PI` |
//! | ambient | the raw cube, brightness 1 | the cube |
//!
//! The camera exposure is 1 ([`UNIT_EXPOSURE_EV100`]), so nothing scales the retail values between
//! the shader and the HDR chain.

use crate::visual_profile::VisualProfile;
use bevy::prelude::*;
use core::f32::consts::PI;

/// Section 4: `k`, the output scale (compiled 1 / 1.2).
pub const OUTPUT_SCALE: f32 = 0.833_333_3;
/// The frame clear colour's own scale, a literal of the executable and not `k` (SPEC-4 section 10,
/// which corrects SPEC-1 section 8.1 and SPEC-2 section 3;
/// [near-lighting.md](../../../docs/specs/engine/near-lighting.md)).
#[allow(
    clippy::excessive_precision,
    reason = "the stored literal, kept as the finding prints it"
)]
pub const CLEAR_COLOUR_SCALE: f32 = 0.833_329_98;
/// Section 4: `B`, `fLightingOutputColourClampPostLit`.
pub const CLAMP_POST_LIT: f32 = 1.0;
/// Section 4: `B2`, `fLightingOutputColourClampPostSpec`.
pub const CLAMP_POST_SPEC: f32 = 1.0;

/// The camera planes of the retail clip position that the vertex fog measures (section 3.3).
/// STAND-IN(#891): the 15 and 10240 of the default path; the spec names no values.
pub const CAMERA_NEAR: f32 = 15.0;
/// See [`CAMERA_NEAR`].
pub const CAMERA_FAR: f32 = 10_240.0;

/// The EV100 at which Bevy's exposure `1 / (1.2 x 2^EV100)` is exactly 1.
pub const UNIT_EXPOSURE_EV100: f32 = -0.263_034_4;

/// `fSunlightScale` of an image-space block the record does not set: the constructor value
/// (section 2.4).
pub const CONSTRUCTOR_SUNLIGHT_SCALE: f32 = 1.8;

/// Which units the scene's lights are written in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightUnits {
    /// The `mudcrab` profile's physical units, unchanged.
    #[default]
    Mudcrab,
    /// The `skyrim` profile: one retail unit is a Lambert response of 1
    /// ([near-lighting.md](../../../docs/specs/engine/near-lighting.md)).
    Retail,
}

impl LightUnits {
    pub fn of(profile: VisualProfile) -> Self {
        match profile {
            VisualProfile::Mudcrab => Self::Mudcrab,
            VisualProfile::Skyrim => Self::Retail,
        }
    }

    /// The units that go with a profile's colour space: only `skyrim` samples authored bytes raw.
    pub fn of_colour_space(decodes_srgb: bool) -> Self {
        if decodes_srgb {
            Self::Mudcrab
        } else {
            Self::Retail
        }
    }

    pub fn is_retail(self) -> bool {
        self == Self::Retail
    }

    /// The Bevy intensity of a point light of fade `fade`, never negative.
    pub fn point_intensity(self, fade: f32) -> f32 {
        match self {
            Self::Mudcrab => crate::lights::retail_intensity(fade),
            Self::Retail => {
                if fade.is_finite() && fade > 0.0 {
                    point_intensity(fade).min(f32::MAX)
                } else {
                    0.0
                }
            }
        }
    }

    /// The sun illuminance and the ambient brightness a scene starts with, before any weather or
    /// cell writes them.
    pub fn startup_levels(self) -> (f32, f32) {
        match self {
            Self::Mudcrab => (
                crate::lights::SUN_ILLUMINANCE,
                crate::lights::AMBIENT_ILLUMINANCE,
            ),
            Self::Retail => (sun_illuminance(CONSTRUCTOR_SUNLIGHT_SCALE), 1.0),
        }
    }
}

/// The linear Bevy colour that holds an authored colour's bytes unchanged: `byte / 255` per
/// channel, with no sRGB decode (section 2.1).
pub fn raw_colour(authored: Color) -> Color {
    let encoded = authored.to_srgba();
    Color::linear_rgb(encoded.red, encoded.green, encoded.blue)
}

/// [`raw_colour`] of three record bytes.
pub fn raw_bytes(bytes: [u8; 3]) -> Color {
    raw_colour(Color::srgb_u8(bytes[0], bytes[1], bytes[2]))
}

/// The Bevy illuminance of the sun or the interior key light: `cb2[1] = colour x fade x
/// SunlightScale` (section 2.3). The colour stays on the light and the fade is 1.
pub fn sun_illuminance(sunlight_scale: f32) -> f32 {
    PI * sunlight_scale
}

/// The Bevy intensity of a point light of retail colour scale `fade x dimmer` (section 2.6).
pub fn point_intensity(fade: f32) -> f32 {
    4.0 * PI * PI * fade
}

// --- CPU reference of `pbr_retail.wgsl` ------------------------------------------------------

/// Section 6.1: `a = 1 - sat(distance / radius)^2`.
pub fn point_attenuation(distance: f32, radius: f32) -> f32 {
    let ratio = (distance / radius).clamp(0.0, 1.0);
    1.0 - ratio * ratio
}

/// Section 6.2: `sat(N . normalize(E + l)) ^ g`.
pub fn specular_lobe(n: Vec3, e: Vec3, l: Vec3, glossiness: f32) -> f32 {
    let base = n.dot((e + l).normalize()).clamp(0.0, 1.0);
    if base > 0.0 {
        base.powf(glossiness)
    } else {
        0.0
    }
}

/// Section 2.5: the directional ambient of six record colours (already divided by 255) at a
/// Creation-axes normal.
pub fn dalc_ambient(faces: [Vec3; 6], n: Vec3) -> Vec3 {
    let [f0, f1, f2, f3, f4, f5] = faces;
    let mean = (f0 + f1 + f2 + f3 + f4 + f5) / 6.0;
    mean + (f1 - f0) * 0.5 * n.x + (f3 - f2) * 0.5 * n.y + (f5 - f4) * 0.5 * n.z
}

/// One point light as the pixel shader reads it.
#[derive(Clone, Copy, Debug)]
pub struct PointTerm {
    /// Unit direction from the surface to the light.
    pub to_light: Vec3,
    pub distance: f32,
    pub radius: f32,
    /// `cb2[22 + i]`: colour times fade times dimmer.
    pub colour: Vec3,
}

/// The light inputs of one pixel.
#[derive(Clone, Debug, Default)]
pub struct LightSum {
    /// `cb2[1]`.
    pub sun_colour: Vec3,
    /// Unit direction to the sun.
    pub sun_direction: Vec3,
    /// The sun's shadow mask, 1 when unshadowed.
    pub sun_mask: f32,
    pub points: Vec<PointTerm>,
    /// The directional ambient at the surface normal.
    pub ambient: Vec3,
    pub emission: Vec3,
}

/// The material inputs of one pixel.
#[derive(Clone, Copy, Debug)]
pub struct SurfaceSample {
    /// The diffuse texel times the vertex colour.
    pub albedo: Vec3,
    pub normal: Vec3,
    /// Unit direction to the eye.
    pub view: Vec3,
    /// `Some((mask x colour x strength, glossiness))` on a specular surface.
    pub specular: Option<(Vec3, f32)>,
}

/// `L` and `Spec` of one pixel (sections 6.1 and 6.2). Nothing is clamped.
pub fn light_surface(surface: &SurfaceSample, light: &LightSum) -> (Vec3, Vec3) {
    let n = surface.normal;
    let sun_colour = light.sun_colour * light.sun_mask;
    let mut sum = sun_colour * n.dot(light.sun_direction).clamp(0.0, 1.0);
    let mut highlight = Vec3::ZERO;
    let glossiness = surface.specular.map_or(0.0, |(_, g)| g);
    highlight += sun_colour * specular_lobe(n, surface.view, light.sun_direction, glossiness);
    for point in &light.points {
        let attenuated = point.colour * point_attenuation(point.distance, point.radius);
        sum += attenuated * n.dot(point.to_light).clamp(0.0, 1.0);
        highlight += attenuated * specular_lobe(n, surface.view, point.to_light, glossiness);
    }
    sum += light.ambient + light.emission;
    let specular = surface
        .specular
        .map_or(Vec3::ZERO, |(colour, _)| highlight * colour);
    (surface.albedo * sum, specular)
}

/// A fog ramp as the retail fog property holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogProperty {
    pub near: f32,
    pub far: f32,
    pub power: f32,
    pub max: f32,
    pub near_colour: Vec3,
    pub far_colour: Vec3,
}

impl FogProperty {
    /// `min(sat(d r - near r) ^ power, max)`; 0 for an empty ramp (sections 3.3 and 8.3).
    pub fn factor(&self, distance: f32) -> f32 {
        crate::retail_fog::fog_factor(distance, self.near, self.far, self.power, self.max)
    }

    pub fn colour(&self, factor: f32) -> Vec3 {
        self.near_colour + factor * (self.far_colour - self.near_colour)
    }
}

/// The output tail (section 7). `selector` is `T`: 0 in the first stage, 1 for sorted blended
/// draws; `S` is 1 in both. `highlight` is `Spec + Aspec` on a two-stage surface.
pub fn output_tail(
    lit: Vec3,
    highlight: Option<Vec3>,
    fog_factor: f32,
    fog_colour: Vec3,
    selector: f32,
) -> Vec3 {
    let k = OUTPUT_SCALE;
    let fogged = |x: Vec3| x + fog_factor * (fog_colour - x);
    match highlight {
        None => {
            let q = lit - k * fogged(lit);
            (q + Vec3::splat(CLAMP_POST_LIT)).min(lit) - selector * q
        }
        Some(highlight) => {
            let first = (lit - k * fogged(lit) + Vec3::splat(CLAMP_POST_LIT)).min(lit);
            let y = first + highlight;
            let q = y - k * fogged(y);
            (q + Vec3::splat(CLAMP_POST_SPEC)).min(y) - selector * q
        }
    }
}

/// The weights of the four day parts (sunrise, day, sunset, night) are
/// [`crate::weather_sky::slot_weights`] (section 2.2). The sky pixel of section 2.4:
/// `v + y` with `v = k x rows` and `y = k x SkyScale`.
pub fn sky_pixel(rows: Vec3, sky_scale: f32) -> Vec3 {
    OUTPUT_SCALE * rows + Vec3::splat(OUTPUT_SCALE * sky_scale)
}

#[cfg(test)]
#[path = "retail_lighting_examples.rs"]
mod examples;

#[cfg(test)]
#[path = "retail_output_examples.rs"]
mod output_examples;
