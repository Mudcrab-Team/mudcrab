//! The retail inputs of a converted `Lighting` material under the `skyrim` profile (SPEC-1
//! sections 2.7, 2.8, 6.2 and 6.3;
//! [near-lighting.md](../../../../docs/specs/engine/near-lighting.md)).
//!
//! The converter publishes a glTF material for Bevy's light model. This reads back what the retail
//! shader needs and marks the material with a [`RetailSurface`], so the vendored retail path
//! draws it:
//!
//! - **Specular:** colour times strength and the glossiness, with the term on for property flag bit
//!   0 or 41, from the NIF floats the pack publishes ([`shared::material_values`]). A
//!   model-space surface without a specular map has a black mask and no highlight.
//!   STAND-IN(#362): without those values the colour and strength are the converter's glTF ones,
//!   clamped to `[0, 1]`, zero for a surface with only bit 41, and the glossiness comes from the
//!   roughness.
//! - **Environment:** the environment technique adds its cube to the light sum's product
//!   (section 9.2), scaled by the published environment map scale and masked by slot 5 or the
//!   normal map's alpha. A pack without the scale draws no reflection.
//!   STAND-IN(#490): the eye technique, a model-space surface and the distance cut at
//!   `fEnvmapLODFadeEnd` are not drawn; the cut's distance input is open.
//! - **Emission:** the file colour times the multiple only with `Own_Emit` (flag bit 22),
//!   unclamped. External emittance (flag bit 29) clears own emit: the surface takes its source's
//!   colour (`emittance.rs`) and is black without a source. The emission adds to the light sum, so
//!   the diffuse-modulated emissive texture that stands in for that under Bevy's model is not
//!   used.
//! - **Alpha test:** the reference `trunc(threshold x property alpha) / 255`.
//! - **Technique:** one technique draws a surface ([`super::technique`]); only the glow
//!   technique reads the glow map.
//! - **Snow:** a surface of the object snow rule adds no specular colour (section 9.4).
//!   STAND-IN(#435): the snow rim and the composite's snow term are not drawn.
//! - **Refraction:** a surface with a refraction flag draws no `Lighting` pass (section 9.2).
//! - **Blending:** a surface that is not a decal blends with its blend bit or a property alpha
//!   below one (section 9.2). The converter publishes `MASK` for a material with the test bit, so
//!   such a surface blends here only when it writes no depth.
//!   STAND-IN(#362): a blended surface with an alpha test loses one of the two (see `pass_audit.rs`).

use super::{
    SLSF1_EXTERNAL_EMITTANCE, native_depth_flags, native_tag,
    technique::{Technique, technique},
};
use crate::skin_material::{SkyrimSkinFlags, SkyrimSkinMaterial};
use ::gltf::Material;
use bevy::{
    asset::RenderAssetUsages,
    pbr::RetailSurface,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use shared::material_values::{MaterialValues, MaterialValuesFile};

/// Property flag bit 0: specular.
const SLSF1_SPECULAR: u32 = 1;
/// Property flag bit 12: model-space normals.
const SLSF1_MODEL_SPACE: u32 = 1 << 12;
/// Property flag bit 41 is bit 9 of the second flag word. It also turns the specular term on.
const SLSF2_SPECULAR_BIT_41: u32 = 1 << 9;
/// Property flag bits 2 and 15: the two refraction flags.
const SLSF1_REFRACTION_BITS: u32 = (1 << 2) | (1 << 15);
/// Property flag bit 20 forces a black base map.
const SLSF1_BLACK_BASE: u32 = 1 << 20;
/// Property flag bit 23: projected UV.
const SLSF1_PROJECTED_UV: u32 = 1 << 23;
/// Property flag bit 30, which turns the object snow rule on.
const SLSF1_SOFT_EFFECT: u32 = 1 << 30;
/// Flags 33, 34, 46 and 63 exclude the object snow rule.
const SLSF2_LOD_AND_BLEND_BITS: u32 = (1 << 1) | (1 << 2) | (1 << 14) | (1 << 31);
/// Flag 60: snow.
const SLSF2_SNOW: u32 = 1 << 28;
/// Flag 61: tree animation.
const SLSF2_TREE_ANIM: u32 = 1 << 29;
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

/// Whether the surface is a decal whose colour pass makes no alpha test: flag 41 set, no blend
/// and no hair decal rule (SPEC-1 section 6.3).
fn uncut_decal(source: &Material<'_>, flags_1: u32, flags_2: u32) -> bool {
    let hair = flags_1 & SLSF1_HAIR_BIT_18 != 0 && flags_2 & SLSF2_BIT_32 != 0;
    flags_1 & SLSF1_ANY_DECAL != 0
        && flags_2 & SLSF2_SPECULAR_BIT_41 != 0
        && !hair
        && super::authored_blend_factors(source).is_none()
}

/// Whether the converter published this material from a `BSLightingShaderProperty`.
pub(super) fn is_lighting(material: &Material<'_>) -> bool {
    native_tag(material, "shaderFamily", "lighting")
}

/// The NIF values of `source`: the ones the GLB publishes, else the pack's sidecar entry of the
/// same shader block.
pub(super) fn material_values(
    source: &Material<'_>,
    sidecar: Option<&MaterialValuesFile>,
    mesh: &str,
) -> Option<MaterialValues> {
    // silent-ok: a material without extras publishes no values and names no shader block.
    let extras: serde_json::Value = serde_json::from_str(source.extras().as_ref()?.get()).ok()?;
    let published = extras
        .pointer("/openSkyrim/materialValues")
        // silent-ok: a malformed field is read from the sidecar, or not at all.
        .and_then(|values| serde_json::from_value::<MaterialValues>(values.clone()).ok());
    published
        .or_else(|| {
            let block = extras.pointer("/openSkyrim/shaderBlock")?.as_u64()?;
            // silent-ok: a block index past `u32` names no sidecar entry.
            sidecar?.get(mesh, u32::try_from(block).ok()?)
        })
        .filter(MaterialValues::is_finite)
}

/// The glTF texture index of the environment slot `semantic`.
fn environment_slot(source: &Material<'_>, semantic: &str) -> Option<usize> {
    let slot = source
        .extension_value("OPEN_SKYRIM_material")?
        .get("textureSlots")?
        .as_array()?
        .iter()
        .find(|slot| slot.get("semantic").and_then(|value| value.as_str()) == Some(semantic))?;
    // silent-ok: an index past `usize` names no image, as an absent one.
    usize::try_from(slot.get("texture")?.as_u64()?).ok()
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
/// `values` are the NIF floats ([`material_values`]) and `textures` the glTF load's images by
/// texture index.
pub(super) fn apply(
    source: &Material<'_>,
    loaded: &StandardMaterial,
    values: Option<MaterialValues>,
    textures: &[Handle<Image>],
    material: &mut StandardMaterial,
) {
    // silent-ok: a lighting material without published flag words has every flag clear.
    let (flags_1, flags_2) = native_depth_flags(source).unwrap_or_default();
    let specular = flags_1 & SLSF1_SPECULAR != 0 || flags_2 & SLSF2_SPECULAR_BIT_41 != 0;
    let (colour, glossiness) = match values {
        // An empty specular slot of a model-space surface is black (section 5).
        Some(values) => {
            let unmasked =
                flags_1 & SLSF1_MODEL_SPACE != 0 && loaded.specular_tint_texture.is_none();
            let colour = Vec3::from_array(values.specular_color) * values.specular_strength;
            (
                if unmasked { Vec3::ZERO } else { colour },
                values.glossiness,
            )
        }
        // The glTF loader stores `specularFactor / 2` as the reflectance.
        None => {
            let tint = loaded.specular_tint.to_linear();
            (
                Vec3::new(tint.red, tint.green, tint.blue) * (loaded.reflectance * 2.0),
                glossiness(loaded.perceptual_roughness),
            )
        }
    };

    // The emissive texture is the glow map alone, which only the glow technique reads.
    material.emissive_texture = (technique(flags_1, flags_2) == Technique::Glow)
        .then(|| loaded.emissive_texture.clone())
        .flatten();
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
    // External emittance clears own emit (SPEC-1 section 2.7).
    // STAND-IN(#362): a surface that no source repoints keeps the constructor colour, black,
    // which the specification infers.
    let emission = if flags_1 & SLSF1_OWN_EMIT != 0 && !external {
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
    // The published cutoff is the threshold byte / 255; the half step guards the f32 product.
    let reference = |cutoff: f32| ((cutoff * 255.0).round() * alpha + 1.0e-3).floor() / 255.0;
    // A blended surface keeps its alpha test (section 6.3).
    let mut blended_reference = 0.0;
    if blends && converts {
        if let AlphaMode::Mask(cutoff) = material.alpha_mode {
            blended_reference = reference(cutoff);
        }
        material.alpha_mode = AlphaMode::Blend;
    }
    if let AlphaMode::Mask(cutoff) = material.alpha_mode {
        material.alpha_mode = AlphaMode::Mask(reference(cutoff));
    }
    // A decal that does not blend has the alpha test only with flag 41 clear (section 6.3). A
    // hair decal keeps its own cut.
    if uncut_decal(source, flags_1, flags_2) && matches!(material.alpha_mode, AlphaMode::Mask(_)) {
        material.alpha_mode = AlphaMode::Opaque;
    }

    // Property flag bit 20 forces a black base map (section 5).
    if flags_1 & SLSF1_BLACK_BASE != 0 {
        material.base_color_texture = None;
        material.base_color = Color::linear_rgba(0.0, 0.0, 0.0, alpha);
    }

    // The object snow rule adds no specular colour, and the surface keeps two output stages.
    // With projected UV the specification does not cover the surface, which draws as before.
    let snow = snows(flags_1, flags_2) && flags_1 & SLSF1_PROJECTED_UV == 0;
    // The environment technique, with the cube its slot names. An empty cube slot is the zero
    // cube, which adds nothing.
    // STAND-IN(#490): a model-space surface has no reflection here.
    let environment = values
        .and_then(|values| values.environment_map_scale)
        .filter(|_| technique(flags_1, flags_2) == Technique::Environment)
        .filter(|_| flags_1 & SLSF1_MODEL_SPACE == 0);
    let texture = |semantic| {
        textures
            .get(environment_slot(source, semantic)?)
            .filter(|handle| **handle != Handle::default())
            .cloned()
    };
    // The parallax technique reads its height map in the shader (section 9.2). An empty height
    // slot is black (section 5), which the loader binds.
    material.depth_map = (technique(flags_1, flags_2) == Technique::Parallax)
        .then(|| texture("height"))
        .flatten();
    material.retail_environment_cube_source = environment.and_then(|_| texture("environment_cube"));
    material.retail_environment_mask_source = material
        .retail_environment_cube_source
        .as_ref()
        .and_then(|_| texture("environment_mask"));

    material.retail_surface = Some(RetailSurface {
        specular_color: if snow { Vec3::ZERO } else { colour },
        glossiness,
        specular: specular || snow,
        emissive_multiple: if external { multiple } else { 0.0 },
        alpha_reference: blended_reference,
        // retail-default: SPEC-1 section 9.2, a surface without the technique reflects nothing.
        environment_scale: environment.unwrap_or(0.0),
    });
}

/// The object snow rule (section 9.4, `bEnableImprovedSnow` at its compiled 1): none of flags
/// 33, 34, 46 and 63, and either flag 60 without flag 61, or flag 30.
fn snows(flags_1: u32, flags_2: u32) -> bool {
    let excluded = SLSF2_LOD_AND_BLEND_BITS;
    let snow = flags_2 & SLSF2_SNOW != 0 && flags_2 & SLSF2_TREE_ANIM == 0;
    flags_2 & excluded == 0 && (snow || flags_1 & SLSF1_SOFT_EFFECT != 0)
}

/// The texel retail binds to an empty base map slot, the default of the normal map: (128, 128,
/// 255, 255) as stored (section 5).
pub(super) const EMPTY_BASE_TEXEL: [u8; 4] = [128, 128, 255, 255];

/// Whether a lit surface's base map slot is empty, so it samples [`EMPTY_BASE_TEXEL`]. Property
/// flag 20 forces black instead. A skin or face tint surface takes its maps from its actor at run
/// time, so its slot is not empty.
pub(super) fn empty_base_slot(source: &Material<'_>, material: &StandardMaterial) -> bool {
    // silent-ok: a lighting material without published flag words has every flag clear.
    let (flags_1, _) = native_depth_flags(source).unwrap_or_default();
    material.base_color_texture.is_none()
        && flags_1 & SLSF1_BLACK_BASE == 0
        && !native_tag(source, "lightingShaderType", "skin_tint")
        && !native_tag(source, "lightingShaderType", "face_tint")
}

/// A one-texel image that reads its stored bytes.
pub(crate) fn texel_image(texel: [u8; 4]) -> Image {
    Image::new_fill(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &texel,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    )
}

/// The texel of an empty height slot (section 5).
pub(crate) const EMPTY_HEIGHT_TEXEL: [u8; 4] = [0, 0, 0, 255];

/// Whether a lit surface of the parallax technique has an empty height slot, so it samples
/// [`EMPTY_HEIGHT_TEXEL`].
pub(super) fn empty_height_slot(source: &Material<'_>, material: &StandardMaterial) -> bool {
    // silent-ok: a lighting material without published flag words has every flag clear.
    let (flags_1, flags_2) = native_depth_flags(source).unwrap_or_default();
    material.depth_map.is_none() && technique(flags_1, flags_2) == Technique::Parallax
}

/// Whether a lit material has a refraction flag (flag 2 or 15): retail draws no `Lighting` pass
/// for it.
pub(super) fn refracts(source: &Material<'_>) -> bool {
    is_lighting(source)
        && native_depth_flags(source)
            .is_some_and(|(flags_1, _)| flags_1 & SLSF1_REFRACTION_BITS != 0)
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
        apply(&source, &loaded, None, &[], &mut material);
        material
    }

    /// A surface with the environment slots and published values of `flags_1`, loaded with the
    /// textures its glTF indices name.
    fn environment(
        (flags_1, flags_2): (u32, u32),
        values: Option<MaterialValues>,
        slots: &str,
        textures: &[Handle<Image>],
    ) -> StandardMaterial {
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"extensionsUsed":["OPEN_SKYRIM_material"],
            "materials":[{{"extensions":{{"OPEN_SKYRIM_material":{{"textureSlots":[{slots}]}}}},
            "extras":{{"openSkyrim":{{"shaderFamily":"lighting","shaderFlags1":{flags_1},
            "shaderFlags2":{flags_2}}}}}}}]}}"#
        );
        let gltf = Gltf::from_slice_without_validation(json.as_bytes()).unwrap();
        let source = gltf.materials().next().unwrap();
        let loaded = loaded();
        let mut material = loaded.clone();
        apply(&source, &loaded, values, textures, &mut material);
        material
    }

    const ENVIRONMENT_BIT: u32 = 1 << 7;
    const BOTH_SLOTS: &str = r#"{"semantic":"environment_cube","texture":1},
        {"semantic":"environment_mask","texture":2}"#;

    fn authored(scale: Option<f32>) -> MaterialValues {
        MaterialValues {
            specular_color: [1.5, 0.5, 0.25],
            specular_strength: 2.0,
            glossiness: 500.0,
            environment_map_scale: scale,
        }
    }

    fn handles() -> Vec<Handle<Image>> {
        let mut images = Assets::<Image>::default();
        (0..3).map(|_| images.add(Image::default())).collect()
    }

    /// SPEC-1 section 5: an empty base slot samples (128, 128, 255, 255), except under flag 20
    /// and on the surfaces an actor supplies.
    #[test]
    fn only_a_plain_surface_without_a_base_map_takes_the_empty_slot_texel() {
        let empty = StandardMaterial::default();
        let textured = StandardMaterial {
            base_color_texture: Some(Handle::default()),
            ..default()
        };
        let check = |flags_1, extra: &str, material: &StandardMaterial| {
            let json = document(flags_1, 0, extra, 1.0);
            empty_base_slot(&json.materials().next().unwrap(), material)
        };
        assert!(check(0, "", &empty));
        assert!(!check(0, "", &textured));
        assert!(!check(SLSF1_BLACK_BASE, "", &empty));
        for kind in ["skin_tint", "face_tint"] {
            let extra = format!(r#","lightingShaderType":"{kind}""#);
            assert!(!check(0, &extra, &empty), "{kind}");
        }
    }

    /// SPEC-1 section 6.3: a decal that does not blend is cut only with flag 41 clear. A hair
    /// decal keeps its own cut.
    #[test]
    fn a_decal_with_flag_41_is_not_cut() {
        let cut = |material: StandardMaterial| matches!(material.alpha_mode, AlphaMode::Mask(_));
        assert!(cut(applied(SLSF1_ANY_DECAL, 0, "")));
        let uncut = applied(SLSF1_ANY_DECAL, SLSF2_SPECULAR_BIT_41, "");
        assert_eq!(uncut.alpha_mode, AlphaMode::Opaque);
        assert!(cut(applied(
            SLSF1_ANY_DECAL | SLSF1_HAIR_BIT_18,
            SLSF2_SPECULAR_BIT_41 | SLSF2_BIT_32,
            ""
        )));
        // A surface that is not a decal keeps its test.
        assert!(cut(applied(0, SLSF2_SPECULAR_BIT_41 | SLSF2_BIT_32, "")));
    }

    /// SPEC-1 section 9.2: only the parallax technique (flag 11 without flag 28) reads a height
    /// map, and its empty slot is black.
    #[test]
    fn only_the_parallax_technique_reads_a_height_map() {
        let slots = r#"{"semantic":"height","texture":1}"#;
        let textures = handles();
        let (parallax, occlusion) = (1 << 11, 1 << 28);
        let material = environment((parallax, 0), None, slots, &textures);
        assert_eq!(material.depth_map.as_ref(), textures.get(1));
        for flags in [parallax | occlusion, 0] {
            let material = environment((flags, 0), None, slots, &textures);
            assert!(material.depth_map.is_none(), "{flags:#x}");
        }
        let bare = StandardMaterial::default();
        for (flags, empty) in [(parallax, true), (0, false)] {
            let json = document(flags, 0, "", 1.0);
            let source = json.materials().next().unwrap();
            assert_eq!(empty_height_slot(&source, &bare), empty);
        }
    }

    /// SPEC-1 section 2.8: the NIF floats load unscaled, with the term of flag bit 41 too.
    #[test]
    fn published_values_replace_the_clamped_gltf_specular() {
        for (flags_1, flags_2) in [(SLSF1_SPECULAR, 0), (0, SLSF2_SPECULAR_BIT_41)] {
            let json = document(flags_1, flags_2, "", 1.0);
            let source = json.materials().next().unwrap();
            let mut material = loaded();
            apply(&source, &loaded(), Some(authored(None)), &[], &mut material);
            let surface = material.retail_surface.unwrap();
            assert!(surface.specular);
            assert_eq!(surface.specular_color, Vec3::new(3.0, 1.0, 0.5));
            assert_eq!(surface.glossiness, 500.0);
        }
        // A model-space surface with no specular map has a black mask.
        let unmasked = environment(
            (SLSF1_SPECULAR | SLSF1_MODEL_SPACE, 0),
            Some(authored(None)),
            "",
            &[],
        );
        assert_eq!(unmasked.retail_surface.unwrap().specular_color, Vec3::ZERO);
    }

    #[test]
    fn the_values_come_from_the_mesh_before_the_sidecar() {
        let mut sidecar = MaterialValuesFile::new();
        sidecar
            .meshes
            .entry("meshes/a.glb".into())
            .or_default()
            .insert(22, authored(Some(0.5)));
        let read = |extra: &str, mesh: &str| {
            let json = document(0, 0, extra, 1.0);
            material_values(&json.materials().next().unwrap(), Some(&sidecar), mesh)
        };
        assert_eq!(
            read(r#","shaderBlock":22"#, "meshes/a.glb"),
            Some(authored(Some(0.5)))
        );
        assert_eq!(read(r#","shaderBlock":23"#, "meshes/a.glb"), None);
        assert_eq!(read(r#","shaderBlock":22"#, "meshes/b.glb"), None);
        assert_eq!(read("", "meshes/a.glb"), None);
        let published = r#","shaderBlock":22,"materialValues":{"specularColor":[0.0,0.0,1.0],
            "specularStrength":3.0,"glossiness":4.0}"#;
        assert_eq!(
            read(published, "meshes/a.glb").unwrap().specular_strength,
            3.0
        );
    }

    /// SPEC-1 section 9.2, technique 1: the cube, the mask and the scale, only for the
    /// environment technique, and never with an invented scale.
    #[test]
    fn the_environment_technique_names_its_cube_mask_and_scale() {
        let textures = handles();
        let values = Some(authored(Some(0.25)));
        let drawn = environment((ENVIRONMENT_BIT, 0), values, BOTH_SLOTS, &textures);
        assert_eq!(
            drawn.retail_environment_cube_source.as_ref(),
            Some(&textures[1])
        );
        assert_eq!(
            drawn.retail_environment_mask_source.as_ref(),
            Some(&textures[2])
        );
        assert_eq!(drawn.retail_surface.unwrap().environment_scale, 0.25);
        // Bound only after the images are checked.
        assert!(drawn.retail_environment_cube.is_none());

        let cube_only = r#"{"semantic":"environment_cube","texture":1}"#;
        let unmasked = environment((ENVIRONMENT_BIT, 0), values, cube_only, &textures);
        assert!(unmasked.retail_environment_cube_source.is_some());
        assert!(unmasked.retail_environment_mask_source.is_none());

        let mask_only = r#"{"semantic":"environment_mask","texture":2}"#;
        let plain = (ENVIRONMENT_BIT, 0);
        // Flag 38, glow, has the higher priority.
        let glow = (ENVIRONMENT_BIT, 1 << 6);
        let model_space = (ENVIRONMENT_BIT | SLSF1_MODEL_SPACE, 0);
        for (flags, values, slots, name) in [
            ((0, 0), values, BOTH_SLOTS, "no environment flag"),
            (glow, values, BOTH_SLOTS, "the glow technique wins"),
            (model_space, values, BOTH_SLOTS, "model space"),
            (plain, Some(authored(None)), BOTH_SLOTS, "no scale"),
            (plain, None, BOTH_SLOTS, "no values"),
            (plain, values, mask_only, "no cube"),
        ] {
            let surface = environment(flags, values, slots, &textures);
            assert!(surface.retail_environment_cube_source.is_none(), "{name}");
            assert!(surface.retail_environment_mask_source.is_none(), "{name}");
        }
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

    /// SPEC-1 section 6.3: a blended surface keeps the alpha test; the fixture's property alpha
    /// is 0.5, so a surface that is not a decal blends.
    #[test]
    fn a_blended_surface_keeps_its_alpha_reference() {
        let blended = applied(0, 0, "");
        assert_eq!(blended.alpha_mode, AlphaMode::Blend);
        assert_eq!(
            blended.retail_surface.unwrap().alpha_reference,
            64.0 / 255.0
        );
        let cutout = applied(0, SLSF2_BIT_32, "");
        assert_eq!(cutout.retail_surface.unwrap().alpha_reference, 0.0);
    }

    /// SPEC-1 section 9.4: a snow surface adds no specular colour and keeps two stages. Tree
    /// animation, LOD flags and projected UV leave it out.
    #[test]
    fn the_object_snow_rule_drops_the_specular_colour() {
        let snow = applied(0, SLSF2_SNOW, "").retail_surface.unwrap();
        assert_eq!(snow.specular_color, Vec3::ZERO);
        assert!(snow.specular);
        let soft = applied(SLSF1_SOFT_EFFECT, 0, "").retail_surface.unwrap();
        assert_eq!(soft.specular_color, Vec3::ZERO);
        for (flags_1, flags_2) in [
            (0, SLSF2_SNOW | SLSF2_TREE_ANIM),
            (0, SLSF2_SNOW | (1 << 2)),
            (SLSF1_PROJECTED_UV, SLSF2_SNOW),
        ] {
            let plain = applied(flags_1 | SLSF1_SPECULAR, flags_2, "")
                .retail_surface
                .unwrap();
            assert_ne!(
                plain.specular_color,
                Vec3::ZERO,
                "{flags_1:#x} {flags_2:#x}"
            );
        }
    }

    #[test]
    fn flag_bit_20_blackens_the_base_and_only_the_glow_technique_reads_the_glow_map() {
        let textured = StandardMaterial {
            base_color_texture: Some(Handle::default()),
            emissive_texture: Some(Handle::default()),
            ..loaded()
        };
        let apply_flags = |flags_1: u32, flags_2: u32| {
            let gltf = document(flags_1, flags_2, "", 1.0);
            let mut material = textured.clone();
            apply(
                &gltf.materials().next().unwrap(),
                &textured,
                None,
                &[],
                &mut material,
            );
            material
        };
        let black = apply_flags(SLSF1_BLACK_BASE, 0);
        assert_eq!(black.base_color, Color::linear_rgba(0.0, 0.0, 0.0, 0.5));
        assert!(black.base_color_texture.is_none());
        // Flag 38 selects the glow technique; the environment flag alone does not.
        assert!(apply_flags(0, 1 << 6).emissive_texture.is_some());
        assert!(apply_flags(1 << 7, 0).emissive_texture.is_none());
        assert!(apply_flags(1 << 10, 1 << 6).emissive_texture.is_none());
    }

    #[test]
    fn refraction_flags_hide_a_lit_surface_only() {
        for bit in [2_u32, 15] {
            let gltf = document(1 << bit, 0, "", 1.0);
            assert!(refracts(&gltf.materials().next().unwrap()), "bit {bit}");
        }
        let gltf = document(0, 0, "", 1.0);
        assert!(!refracts(&gltf.materials().next().unwrap()));
    }

    #[test]
    fn external_emittance_clears_own_emit() {
        let external = SLSF1_OWN_EMIT | u32::try_from(SLSF1_EXTERNAL_EMITTANCE).unwrap();
        assert_eq!(applied(external, 0, "").emissive.red, 0.0);
        assert!(applied(SLSF1_OWN_EMIT, 0, "").emissive.red > 0.0);
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
