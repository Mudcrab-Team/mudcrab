//! The retail inputs of a converted `Lighting` material under the `skyrim` profile (SPEC-1
//! sections 2.7, 2.8, 6.2 and 6.3;
//! [near-lighting.md](../../../../docs/specs/engine/near-lighting.md)).
//!
//! The converter publishes a glTF material for Bevy's light model. This reads back what the retail
//! shader needs and marks the material with a [`RetailSurface`], so the vendored retail path
//! draws it:
//!
//! - **Specular:** colour times strength and the glossiness, with the term on for property flag bit
//!   0 or 41. STAND-IN(#362): the converter clamps the strength and the colour to `[0, 1]`, drops
//!   the strength of a surface that has only bit 41, and stores the glossiness as a roughness;
//!   retail loads the NIF floats unscaled.
//! - **Emission:** the file colour times the multiple only with `Own_Emit` (flag bit 22),
//!   unclamped. An external emittance surface takes its source's colour (`emittance.rs`). The
//!   emission adds to the light sum, so the diffuse-modulated emissive texture that stands in for
//!   that under Bevy's model is not used.
//! - **Alpha test:** the reference `trunc(threshold x property alpha) / 255`.
//! - **Blending:** a surface that is not a decal blends with its blend bit or a property alpha
//!   below one (section 9.2). The converter publishes `MASK` for a material with the test bit, so
//!   such a surface blends here only when it writes no depth.
//!   STAND-IN(#362): a blended surface with an alpha test loses one of the two (see `pass_audit.rs`).

use super::{SLSF1_EXTERNAL_EMITTANCE, native_depth_flags, native_tag};
use crate::skin_material::{SkyrimSkinFlags, SkyrimSkinMaterial};
use ::gltf::Material;
use bevy::{pbr::RetailSurface, prelude::*};

/// Property flag bit 0: specular.
const SLSF1_SPECULAR: u32 = 1;
/// Property flag bit 41 is bit 9 of the second flag word. It also turns the specular term on.
const SLSF2_SPECULAR_BIT_41: u32 = 1 << 9;
/// Property flag bit 22: own emit.
const SLSF1_OWN_EMIT: u32 = 1 << 22;

/// Property flag bits 26 and 27: decal and dynamic decal.
const SLSF1_ANY_DECAL: u32 = (1 << 26) | (1 << 27);
/// Property flag bit 18, which selects the hair technique.
const SLSF1_HAIR_BIT_18: u32 = 1 << 18;
/// Property flag bit 32 is bit 0 of the second flag word: depth write.
const SLSF2_BIT_32: u32 = 1;

/// Marks a retail hair decal (SPEC-1 section 6.3): a decal with property flags 32 and 18 runs the
/// hair alpha rule (`skyrim_skin.wgsl`).
pub(super) fn hair_decal(
    source: &Material<'_>,
    mut hair: SkyrimSkinMaterial,
) -> SkyrimSkinMaterial {
    let Some((flags_1, flags_2)) = native_depth_flags(source) else {
        return hair;
    };
    if hair.base.retail_surface.is_some()
        && flags_1 & SLSF1_ANY_DECAL != 0
        && flags_1 & SLSF1_HAIR_BIT_18 != 0
        && flags_2 & SLSF2_BIT_32 != 0
    {
        hair.extension.settings.flags |= SkyrimSkinFlags::HAIR_DECAL;
    }
    hair
}

/// Whether the converter published this material from a `BSLightingShaderProperty`.
pub(super) fn is_lighting(material: &Material<'_>) -> bool {
    native_tag(material, "shaderFamily", "lighting")
}

/// The signed emissive colour and multiple the converter kept for values glTF cannot hold.
fn source_emission(material: &Material<'_>) -> Option<(Vec3, f32)> {
    // The extras are validated raw JSON; a material without the field has no signed emission.
    // silent-ok: an absent field leaves the published glTF emission in use.
    let extras: serde_json::Value = serde_json::from_str(material.extras().as_ref()?.get()).ok()?;
    let source = extras.pointer("/openSkyrim/sourceEmission")?;
    // silent-ok: a malformed field leaves the published glTF emission in use.
    let colour: [f32; 3] = serde_json::from_value(source.get("color")?.clone()).ok()?;
    // silent-ok: a malformed field leaves the published glTF emission in use.
    let multiple: f32 = serde_json::from_value(source.get("multiple")?.clone()).ok()?;
    Some((Vec3::from_array(colour), multiple))
}

/// The glossiness behind the converter's roughness `(2 / (g + 2))^(1/4)`.
fn glossiness(roughness: f32) -> f32 {
    if roughness > 0.0 {
        (2.0 / roughness.powi(4) - 2.0).max(0.0)
    } else {
        0.0
    }
}

/// Writes the retail inputs of `source` into `material`. `loaded` is the stock glTF material the
/// published values are read from; `material` may already carry the `mudcrab` adaptations.
pub(super) fn apply(
    source: &Material<'_>,
    loaded: &StandardMaterial,
    material: &mut StandardMaterial,
) {
    // silent-ok: a lighting material without published flag words has every flag clear.
    let (flags_1, flags_2) = native_depth_flags(source).unwrap_or_default();
    let specular = flags_1 & SLSF1_SPECULAR != 0 || flags_2 & SLSF2_SPECULAR_BIT_41 != 0;
    // The glTF loader stores `specularFactor / 2` as the reflectance.
    let strength = loaded.reflectance * 2.0;
    let tint = loaded.specular_tint.to_linear();

    // The emissive texture is the glow map alone.
    material.emissive_texture = loaded.emissive_texture.clone();
    material.emissive_channel = loaded.emissive_channel.clone();
    let published = loaded.emissive;
    let (emission, multiple) = match source_emission(source) {
        Some((colour, multiple)) => (colour * multiple, multiple),
        None => (
            Vec3::new(published.red, published.green, published.blue),
            source.emissive_strength().unwrap_or(1.0),
        ),
    };
    let external = u64::from(flags_1) & SLSF1_EXTERNAL_EMITTANCE != 0;
    // STAND-IN(#362): an external emittance surface that no source repoints follows the own-emit
    // gate; the specification does not say what retail points it at.
    let emission = if flags_1 & SLSF1_OWN_EMIT != 0 {
        emission
    } else {
        Vec3::ZERO
    };
    material.emissive = LinearRgba::new(emission.x, emission.y, emission.z, published.alpha);

    // A sorted draw takes the fog and `k` in its shader only when the material blends.
    let alpha = loaded.base_color.to_linear().alpha;
    let blends = flags_1 & SLSF1_ANY_DECAL == 0
        && (super::authored_blend_factors(source).is_some() || alpha < 1.0);
    let converts = match material.alpha_mode {
        AlphaMode::Opaque => true,
        AlphaMode::Mask(_) => flags_2 & SLSF2_BIT_32 == 0,
        _ => false,
    };
    if blends && converts {
        material.alpha_mode = AlphaMode::Blend;
    }

    if let AlphaMode::Mask(cutoff) = material.alpha_mode {
        // The published cutoff is the threshold byte / 255; the half step guards the f32 product.
        let threshold = (cutoff * 255.0).round();
        material.alpha_mode = AlphaMode::Mask((threshold * alpha + 1.0e-3).floor() / 255.0);
    }

    material.retail_surface = Some(RetailSurface {
        specular_color: Vec3::new(tint.red, tint.green, tint.blue) * strength,
        glossiness: glossiness(loaded.perceptual_roughness),
        specular,
        emissive_multiple: if external { multiple } else { 0.0 },
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::gltf::Gltf;

    fn document(flags_1: u32, flags_2: u32, extra: &str, strength: f32) -> Gltf {
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"extensionsUsed":["KHR_materials_emissive_strength"],
            "materials":[{{"emissiveFactor":[1.0,0.5,0.25],
            "extensions":{{"KHR_materials_emissive_strength":{{"emissiveStrength":{strength}}}}},
            "extras":{{"openSkyrim":{{"shaderFamily":"lighting","shaderFlags1":{flags_1},
            "shaderFlags2":{flags_2}{extra}}}}}}}]}}"#
        );
        Gltf::from_slice_without_validation(json.as_bytes()).unwrap()
    }

    fn loaded() -> StandardMaterial {
        StandardMaterial {
            base_color: Color::linear_rgba(1.0, 1.0, 1.0, 0.5),
            emissive: LinearRgba::rgb(2.0, 1.0, 0.5),
            // Specular factor 0.8 and the roughness of glossiness 80.
            reflectance: 0.4,
            specular_tint: Color::linear_rgb(0.5, 0.25, 1.0),
            perceptual_roughness: (2.0_f32 / 82.0).powf(0.25),
            alpha_mode: AlphaMode::Mask(128.0 / 255.0),
            ..default()
        }
    }

    fn applied(flags_1: u32, flags_2: u32, extra: &str) -> StandardMaterial {
        let gltf = document(flags_1, flags_2, extra, 2.0);
        let source = gltf.materials().next().unwrap();
        let loaded = loaded();
        // The `mudcrab` adaptation that copies the diffuse map into the emissive slot is undone.
        let mut material = StandardMaterial {
            emissive_texture: Some(Handle::default()),
            ..loaded.clone()
        };
        apply(&source, &loaded, &mut material);
        material
    }

    #[test]
    fn specular_is_colour_times_strength_with_the_published_glossiness() {
        let material = applied(SLSF1_SPECULAR, 0, "");
        let surface = material.retail_surface.unwrap();
        assert!(surface.specular);
        assert!((surface.specular_color - Vec3::new(0.4, 0.2, 0.8)).length() < 1.0e-6);
        assert!(
            (surface.glossiness - 80.0).abs() < 1.0e-3,
            "{}",
            surface.glossiness
        );
        assert!(!applied(0, 0, "").retail_surface.unwrap().specular);
        assert!(
            applied(0, SLSF2_SPECULAR_BIT_41, "")
                .retail_surface
                .unwrap()
                .specular
        );
    }

    #[test]
    fn only_own_emit_keeps_the_file_emission() {
        let own = applied(SLSF1_OWN_EMIT, 0, "");
        assert_eq!(own.emissive, LinearRgba::rgb(2.0, 1.0, 0.5));
        assert!(own.emissive_texture.is_none());
        assert_eq!(own.retail_surface.unwrap().emissive_multiple, 0.0);
        assert_eq!(applied(0, 0, "").emissive, LinearRgba::rgb(0.0, 0.0, 0.0));
    }

    #[test]
    fn an_external_emittance_surface_keeps_its_multiple() {
        let flags = u32::try_from(SLSF1_EXTERNAL_EMITTANCE).unwrap();
        let material = applied(flags, 0, "");
        assert_eq!(material.emissive, LinearRgba::rgb(0.0, 0.0, 0.0));
        assert_eq!(material.retail_surface.unwrap().emissive_multiple, 2.0);
    }

    #[test]
    fn signed_source_emission_is_not_clamped() {
        let material = applied(
            SLSF1_OWN_EMIT,
            0,
            r#","sourceEmission":{"color":[-1.0,0.5,2.0],"multiple":3.0}"#,
        );
        assert_eq!(material.emissive, LinearRgba::rgb(-3.0, 1.5, 6.0));
    }

    #[test]
    fn the_alpha_reference_is_the_truncated_threshold_times_the_property_alpha() {
        let material = applied(0, SLSF2_BIT_32, "");
        assert_eq!(material.alpha_mode, AlphaMode::Mask(64.0 / 255.0));
    }

    /// The fixture's property alpha is 0.5, so a surface that is not a decal blends (section 9.2).
    #[test]
    fn a_surface_below_full_alpha_without_depth_write_blends_unless_it_is_a_decal() {
        assert_eq!(applied(0, 0, "").alpha_mode, AlphaMode::Blend);
        let decal = applied(SLSF1_ANY_DECAL, 0, "");
        assert_eq!(decal.alpha_mode, AlphaMode::Mask(64.0 / 255.0));
    }
}
