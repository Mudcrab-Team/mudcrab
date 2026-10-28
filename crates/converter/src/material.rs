use crate::asset_path::{AssetKind, canonical_asset_path};
use color_eyre::{Result, eyre::ensure};
use project_wormhole_nif::{
    bs::prelude::{BSShaderTextureSet, BSTriShape},
    nif_block::{
        BSEffectShaderProperty, BSLightingShaderProperty, NiAlphaProperty, NiFloatInterpController,
        NiSingleInterpController, NiTimeController, NifBlock,
    },
    nif_enum::{EffectShaderControlledVariable, KeyType, LightingShaderControlledFloat},
    nif_file::NifFile,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const NULL_BLOCK: u32 = u32::MAX;
const SLSF1_SPECULAR: u32 = 1;
const SLSF1_MODEL_SPACE_NORMALS: u32 = 1 << 12;
const SLSF1_ENVIRONMENT_MAPPING: u32 = 1 << 7;
#[cfg(test)]
const SLSF1_VERTEX_ALPHA: u32 = 1 << 3;
const SLSF1_SCREENDOOR_ALPHA_FADE: u32 = 1 << 19;
const SLSF2_DOUBLE_SIDED: u32 = 1 << 4;
const SLSF2_GLOW_MAP: u32 = 1 << 6;
const SLSF2_PREMULTIPLIED_ALPHA: u32 = 1 << 19;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NifShaderFamily {
    Lighting,
    Effect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightingShaderType {
    Default,
    EnvironmentMap,
    Glow,
    Parallax,
    FaceTint,
    SkinTint,
    HairTint,
    ParallaxOcclusion,
    MultitextureLandscape,
    LodLandscape,
    Snow,
    MultiLayerParallax,
    TreeAnimation,
    LodObjects,
    SparkleSnow,
    LodObjectsHd,
    EyeEnvironmentMap,
    Cloud,
    LodLandscapeNoise,
    MultitextureLandscapeLodBlend,
    Fo4Dismemberment,
}

impl TryFrom<u32> for LightingShaderType {
    type Error = color_eyre::Report;

    fn try_from(value: u32) -> Result<Self> {
        Ok(match value {
            0 => Self::Default,
            1 => Self::EnvironmentMap,
            2 => Self::Glow,
            3 => Self::Parallax,
            4 => Self::FaceTint,
            5 => Self::SkinTint,
            6 => Self::HairTint,
            7 => Self::ParallaxOcclusion,
            8 => Self::MultitextureLandscape,
            9 => Self::LodLandscape,
            10 => Self::Snow,
            11 => Self::MultiLayerParallax,
            12 => Self::TreeAnimation,
            13 => Self::LodObjects,
            14 => Self::SparkleSnow,
            15 => Self::LodObjectsHd,
            16 => Self::EyeEnvironmentMap,
            17 => Self::Cloud,
            18 => Self::LodLandscapeNoise,
            19 => Self::MultitextureLandscapeLodBlend,
            20 => Self::Fo4Dismemberment,
            _ => {
                return Err(color_eyre::eyre::eyre!(
                    "unknown lighting shader type {value}"
                ));
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NifTextureSemantic {
    Diffuse,
    Normal,
    Glow,
    Height,
    Detail,
    Specular,
    EnvironmentCube,
    EnvironmentMask,
    InnerLayer,
    /// The FaceGen tint texture of a `face_tint` head (slot 6), overlaid on the diffuse.
    FacegenTint,
    Greyscale,
    Unclassified,
}

impl NifTextureSemantic {
    /// Colour images published as sRGB: the runtime treats them like the diffuse, while the other
    /// Skyrim-only slots hold masks, heights or cube data.
    fn is_srgb_color(self) -> bool {
        matches!(self, Self::Detail | Self::FacegenTint)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NifTextureSlot {
    pub slot: u8,
    pub semantic: NifTextureSemantic,
    pub path: String,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NifAlphaMode {
    Opaque,
    Cutout,
    Blend,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidatedNifMaterial {
    pub shader_family: NifShaderFamily,
    pub lighting_shader_type: Option<LightingShaderType>,
    pub shader_block: u32,
    pub texture_set_block: Option<u32>,
    pub alpha_property_block: Option<u32>,
    pub shader_flags_1: u32,
    pub shader_flags_2: u32,
    pub base_color: [f32; 4],
    pub alpha: f32,
    pub alpha_mode: NifAlphaMode,
    pub alpha_threshold: Option<u8>,
    pub glossiness: f32,
    pub specular_color: [f32; 3],
    pub specular_strength: f32,
    pub emissive_color: [f32; 3],
    pub emissive_multiple: f32,
    pub double_sided: bool,
    pub textures: Vec<NifTextureSlot>,
    pub uv_offset: [f32; 2],
    pub uv_scale: [f32; 2],
    pub texture_clamp_mode: u8,
    /// The tint colour of a `skin_tint` or `hair_tint` lighting shader, as the NIF stores it
    /// (encoded RGB in 0..1). On FaceGen hair it is the NPC's hair colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint_color: Option<[f32; 3]>,
    /// Shader-variable float controllers on this shape's shader property.
    ///
    /// Skyrim animates hearth flames, lava, steam and glow cards by driving a
    /// shader variable from a keyframe controller; the static runtime has no
    /// controller evaluation, so the channels are published as the
    /// `OPEN_SKYRIM_material_animation` glTF extension for the engine to play
    /// (`docs/specs/converters/nif-to-gltf.md`). Empty for a static material.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animation: Vec<NifMaterialAnimationChannel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum NifMaterialDisposition {
    Validated { material: ValidatedNifMaterial },
    Excluded { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NifShapeMaterial {
    pub shape_block: u32,
    pub shape_name: Option<String>,
    pub shader_property_block: Option<u32>,
    pub alpha_property_block: Option<u32>,
    pub disposition: NifMaterialDisposition,
}

/// Builds the per-shape material contract, including each shape's animation
/// channels.
///
/// `animation_skips` accumulates the controllers dropped on the way, by
/// reason.
pub fn build_nif_material_contract(
    nif: &NifFile,
    source: &Path,
    animation_skips: &mut NifAnimationSkips,
) -> Result<Vec<NifShapeMaterial>> {
    let mut contract = Vec::new();
    for (index, block) in nif.blocks.iter().enumerate() {
        let shape = modern_shape(block);
        let legacy = match block {
            NifBlock::NiTriShape(shape) => {
                Some((shape.name, shape.shader_property, shape.alpha_property))
            }
            _ => None,
        };
        let Some((name_index, shader_property, alpha_property)) = shape
            .map(|shape| {
                (
                    shape.av.object.name,
                    shape.shader_property,
                    shape.alpha_property,
                )
            })
            .or(legacy)
        else {
            continue;
        };
        let shape_block = u32::try_from(index)?;
        let shape_name = nif
            .header
            .get_string(name_index as usize)
            .ok()
            .map(str::to_owned);
        let disposition = build_shape_material(
            nif,
            source,
            shape_block,
            shape_name.as_deref(),
            shader_property,
            alpha_property,
            animation_skips,
        )?;
        contract.push(NifShapeMaterial {
            shape_block,
            shape_name,
            shader_property_block: (shader_property != NULL_BLOCK).then_some(shader_property),
            alpha_property_block: (alpha_property != NULL_BLOCK).then_some(alpha_property),
            disposition,
        });
    }
    Ok(contract)
}

/// Replaces the legacy exporter's order-based materials with deterministic
/// glTF/PBR materials built from the per-shape NIF contract.
pub fn publish_gltf_materials(
    document: &mut serde_json::Value,
    contract: &[NifShapeMaterial],
    exported_shape_blocks: &[u32],
    glb_output_path: &Path,
) -> Result<()> {
    ensure!(
        contract.len() == exported_shape_blocks.len(),
        "material publication received {} shapes for {} exported meshes",
        contract.len(),
        exported_shape_blocks.len()
    );
    let by_block = contract
        .iter()
        .map(|shape| (shape.shape_block, shape))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        by_block.len() == contract.len(),
        "material contract contains duplicate shape blocks"
    );

    let mut registry = TextureRegistry::default();
    let mut materials = Vec::new();
    let mut material_by_block = BTreeMap::<u32, usize>::new();
    let mut used_extensions = BTreeSet::new();
    for shape in contract {
        let NifMaterialDisposition::Validated { material } = &shape.disposition else {
            continue;
        };
        let published = publish_material(
            shape,
            material,
            glb_output_path,
            &mut registry,
            &mut used_extensions,
        )?;
        material_by_block.insert(shape.shape_block, materials.len());
        materials.push(published);
    }
    let excluded_material = contract
        .iter()
        .any(|shape| matches!(shape.disposition, NifMaterialDisposition::Excluded { .. }))
        .then(|| {
            let index = materials.len();
            materials.push(serde_json::json!({
                "name": "Mudcrab non-rendering excluded geometry",
                "alphaMode": "MASK",
                "alphaCutoff": 1.0,
                "pbrMetallicRoughness": {
                    "baseColorFactor": [0.0, 0.0, 0.0, 0.0],
                    "metallicFactor": 0.0,
                    "roughnessFactor": 1.0
                },
                "extras": {
                    "openSkyrim": {
                        "nonRenderingExclusion": true
                    }
                }
            }));
            index
        });

    let meshes = document
        .get_mut("meshes")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| color_eyre::eyre::eyre!("glTF document has no mesh array"))?;
    ensure!(
        meshes.len() == exported_shape_blocks.len(),
        "glTF contains {} meshes for {} material mappings",
        meshes.len(),
        exported_shape_blocks.len()
    );
    for (mesh, shape_block) in meshes.iter_mut().zip(exported_shape_blocks) {
        let shape = by_block.get(shape_block).ok_or_else(|| {
            color_eyre::eyre::eyre!("exported mesh references unknown shape block {shape_block}")
        })?;
        let primitives = mesh
            .get_mut("primitives")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| color_eyre::eyre::eyre!("glTF mesh has no primitive array"))?;
        // Every exported shape owns one primitive. Segment tables do not multiply draw calls.
        ensure!(
            primitives.len() == 1,
            "shape block {shape_block} exported {} primitives; expected one",
            primitives.len()
        );
        let primitive = &mut primitives[0];
        if let Some(material_index) = material_by_block.get(shape_block) {
            primitive["material"] = serde_json::json!(material_index);
        } else if let NifMaterialDisposition::Excluded { reason } = &shape.disposition {
            primitive["material"] = serde_json::json!(
                excluded_material.expect("an excluded shape must have a non-rendering material")
            );
            let extras = serde_json::json!({
                "openSkyrim": {
                    "shapeBlock": shape.shape_block,
                    "materialExclusion": reason
                }
            });
            primitive["extras"] = extras;
        }
    }

    document["materials"] = serde_json::Value::Array(materials);
    if registry.images.is_empty() {
        document
            .as_object_mut()
            .expect("glTF document must be an object")
            .remove("images");
        document
            .as_object_mut()
            .expect("glTF document must be an object")
            .remove("textures");
    } else {
        document["images"] = serde_json::Value::Array(registry.images);
        document["textures"] = serde_json::Value::Array(registry.textures);
        document["samplers"] = serde_json::json!(
            (0..4)
                .map(|mode| serde_json::json!({
                    "wrapS": if mode & 2 == 0 { 33071 } else { 10497 },
                    "wrapT": if mode & 1 == 0 { 33071 } else { 10497 },
                    "magFilter": 9729, "minFilter": 9987
                }))
                .collect::<Vec<_>>()
        );
    }
    let extensions = document
        .as_object_mut()
        .expect("glTF document must be an object")
        .entry("extensionsUsed")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .expect("extensionsUsed must be an array");
    extensions.retain(|value| {
        value
            .as_str()
            .is_some_and(|name| name != "KHR_materials_pbrSpecularGlossiness")
    });
    for extension in used_extensions {
        if !extensions.iter().any(|value| value == extension.as_str()) {
            extensions.push(serde_json::Value::String(extension));
        }
    }
    if extensions.is_empty() {
        document
            .as_object_mut()
            .expect("glTF document must be an object")
            .remove("extensionsUsed");
    }
    if let Some(required) = document
        .get_mut("extensionsRequired")
        .and_then(serde_json::Value::as_array_mut)
    {
        required.retain(|value| {
            value
                .as_str()
                .is_some_and(|name| name != "KHR_materials_pbrSpecularGlossiness")
        });
        if required.is_empty() {
            document
                .as_object_mut()
                .expect("glTF document must be an object")
                .remove("extensionsRequired");
        }
    }
    Ok(())
}

#[derive(Default)]
struct TextureRegistry {
    indices: BTreeMap<(String, bool, u8), usize>,
    images: Vec<serde_json::Value>,
    textures: Vec<serde_json::Value>,
}

impl TextureRegistry {
    fn texture(
        &mut self,
        path: &str,
        glb_output_path: &Path,
        is_srgb: bool,
        clamp: u8,
    ) -> Result<usize> {
        let canonical = canonical_asset_path(path, AssetKind::Texture, "ktx2")?;
        let key = (canonical.clone(), is_srgb, clamp);
        if let Some(index) = self.indices.get(&key) {
            return Ok(*index);
        }
        let index = self.textures.len();
        let mut runtime_path = if is_srgb {
            srgb_texture_alias(&canonical)?
        } else {
            canonical.clone()
        };
        if clamp != 3 {
            runtime_path = format!(
                "{}.opensky-wrap{clamp}.ktx2",
                runtime_path.strip_suffix(".ktx2").ok_or_else(|| {
                    color_eyre::eyre::eyre!("runtime texture is not KTX2: {runtime_path}")
                })?
            );
        }
        self.images.push(serde_json::json!({
            "uri": runtime_texture_uri(glb_output_path, &runtime_path)?
        }));
        self.textures
            .push(serde_json::json!({ "source": index, "sampler": clamp }));
        self.indices.insert(key, index);
        Ok(index)
    }
}

fn srgb_texture_alias(canonical: &str) -> Result<String> {
    let stem = canonical
        .strip_suffix(".ktx2")
        .ok_or_else(|| color_eyre::eyre::eyre!("runtime texture is not KTX2: {canonical}"))?;
    Ok(format!("{stem}.opensky-srgb.ktx2"))
}

/// Approximate the width of Skyrim's Blinn-Phong exponent with a GGX lobe.
/// Bevy squares perceptual roughness to obtain the microfacet alpha parameter.
/// This preserves exponent ordering, but does not recover Skyrim's BRDF.
fn perceptual_roughness(glossiness: f32) -> f32 {
    if !glossiness.is_finite() || glossiness < 0.0 {
        return 1.0;
    }
    (2.0 / (glossiness + 2.0)).powf(0.25)
}

fn publish_material(
    shape: &NifShapeMaterial,
    material: &ValidatedNifMaterial,
    glb_output_path: &Path,
    registry: &mut TextureRegistry,
    used_extensions: &mut BTreeSet<String>,
) -> Result<serde_json::Value> {
    let diffuse = texture_with_semantic(material, NifTextureSemantic::Diffuse);
    let normal = texture_with_semantic(material, NifTextureSemantic::Normal);
    let glow = texture_with_semantic(material, NifTextureSemantic::Glow);
    let specular = texture_with_semantic(material, NifTextureSemantic::Specular);
    let mut pbr = serde_json::json!({
        "baseColorFactor": material.base_color,
        "metallicFactor": 0.0,
        "roughnessFactor": perceptual_roughness(material.glossiness)
    });
    if let Some(slot) = diffuse {
        pbr["baseColorTexture"] = serde_json::json!({
            "index": registry.texture(&slot.path, glb_output_path, true, material.texture_clamp_mode)?,
            "texCoord": 0
        });
    }
    let alpha_mode = material.alpha_mode;
    let mut output = serde_json::json!({
        "alphaMode": match alpha_mode {
            NifAlphaMode::Opaque => "OPAQUE",
            NifAlphaMode::Cutout => "MASK",
            NifAlphaMode::Blend => "BLEND",
        },
        "pbrMetallicRoughness": pbr,
        "extras": {
            "openSkyrim": {
                "shapeBlock": shape.shape_block,
                "shaderBlock": material.shader_block
            }
        }
    });
    if let Some(name) = &shape.shape_name {
        output["name"] = serde_json::json!(name);
    }
    if material.double_sided {
        output["doubleSided"] = serde_json::json!(true);
    }
    if alpha_mode == NifAlphaMode::Cutout {
        output["alphaCutoff"] =
            serde_json::json!(f32::from(material.alpha_threshold.unwrap_or(128)) / 255.0);
    }
    if let Some(slot) = normal {
        output["extras"]["openSkyrim"]["normalConvention"] =
            serde_json::json!(
                if material.shader_flags_1 & SLSF1_MODEL_SPACE_NORMALS != 0 {
                    "model_space"
                } else {
                    "directx"
                }
            );
        output["normalTexture"] = serde_json::json!({
            "index": registry.texture(&slot.path, glb_output_path, false, material.texture_clamp_mode)?,
            "texCoord": 0,
            "scale": 1.0
        });
    }
    output["extras"]["openSkyrim"]["shaderFamily"] = serde_json::json!(material.shader_family);
    output["extras"]["openSkyrim"]["lightingShaderType"] =
        serde_json::json!(material.lighting_shader_type);
    output["extras"]["openSkyrim"]["shaderFlags1"] = serde_json::json!(material.shader_flags_1);
    output["extras"]["openSkyrim"]["shaderFlags2"] = serde_json::json!(material.shader_flags_2);
    publish_emissive(
        &mut output,
        material,
        glow,
        glb_output_path,
        registry,
        used_extensions,
    )?;
    publish_specular(
        &mut output,
        material,
        normal,
        specular,
        glb_output_path,
        registry,
        used_extensions,
    )?;
    if material.uv_offset != [0.0; 2] || material.uv_scale != [1.0; 2] {
        let transform = serde_json::json!({"offset":material.uv_offset,"scale":material.uv_scale});
        for pointer in [
            "/pbrMetallicRoughness/baseColorTexture",
            "/normalTexture",
            "/emissiveTexture",
            "/extensions/KHR_materials_specular/specularTexture",
            "/extensions/KHR_materials_specular/specularColorTexture",
        ] {
            if let Some(info) = output.pointer_mut(pointer) {
                info["extensions"]["KHR_texture_transform"] = transform.clone();
                used_extensions.insert("KHR_texture_transform".into());
            }
        }
        output["extras"]["openSkyrim"]["uvTransform"] = transform;
    }
    publish_skyrim_extension(
        &mut output,
        material,
        glb_output_path,
        registry,
        used_extensions,
    )?;
    publish_material_animation(&mut output, material, used_extensions);
    Ok(output)
}

/// Publishes the shader-variable channels as their own extension.
///
/// The channels sit beside the core `OPEN_SKYRIM_material` rather than inside it: a
/// playback consumer reads only animation. `extensionsUsed` lists
/// it, never `extensionsRequired`, so a consumer that does not play animation
/// still loads the model - it just shows one still frame, which is what the
/// shape did before this extension existed.
fn publish_material_animation(
    output: &mut serde_json::Value,
    material: &ValidatedNifMaterial,
    used_extensions: &mut BTreeSet<String>,
) {
    if material.animation.is_empty() {
        return;
    }
    output["extensions"]["OPEN_SKYRIM_material_animation"] =
        serde_json::json!({ "channels": material.animation });
    used_extensions.insert("OPEN_SKYRIM_material_animation".to_owned());
}

fn publish_emissive(
    output: &mut serde_json::Value,
    material: &ValidatedNifMaterial,
    glow: Option<&NifTextureSlot>,
    glb_output_path: &Path,
    registry: &mut TextureRegistry,
    used_extensions: &mut BTreeSet<String>,
) -> Result<()> {
    // NIF can store signed tints (including unused channels with multiplier 0).
    // glTF emission is nonnegative. Retain raw signed values for a future native
    // shader; keep the previous lower clamp in this compatibility projection.
    if material.emissive_color.iter().any(|value| *value < 0.0) || material.emissive_multiple < 0.0
    {
        output["extras"]["openSkyrim"]["sourceEmission"] = serde_json::json!({
            "color": material.emissive_color,
            "multiple": material.emissive_multiple,
            "representation": if material.emissive_multiple < 0.0 {
                "negative_values_clamped_for_gltf"
            } else {
                "negative_channels_clamped_for_gltf"
            }
        });
    }
    let has_color = material.emissive_color.iter().any(|value| *value > 0.0);
    if glow.is_none() && !has_color {
        return Ok(());
    }
    // glTF bounds the tint to [0, 1], but the extension preserves its authored
    // linear energy. Keep black tints black, and permit strengths below one.
    let peak = material.emissive_color.into_iter().fold(1.0_f32, f32::max);
    let color = material.emissive_color.map(|value| value.max(0.0) / peak);
    output["emissiveFactor"] = serde_json::json!(color);
    if let Some(slot) = glow {
        output["emissiveTexture"] = serde_json::json!({
            "index": registry.texture(&slot.path, glb_output_path, true, material.texture_clamp_mode)?,
            "texCoord": 0
        });
    }
    let strength = material.emissive_multiple.max(0.0) * peak;
    if strength != 1.0 {
        output["extensions"]["KHR_materials_emissive_strength"] =
            serde_json::json!({ "emissiveStrength": strength });
        used_extensions.insert("KHR_materials_emissive_strength".to_owned());
    }
    Ok(())
}

fn publish_specular(
    output: &mut serde_json::Value,
    material: &ValidatedNifMaterial,
    normal: Option<&NifTextureSlot>,
    specular: Option<&NifTextureSlot>,
    glb_output_path: &Path,
    registry: &mut TextureRegistry,
    used_extensions: &mut BTreeSet<String>,
) -> Result<()> {
    let enabled = material.shader_flags_1 & SLSF1_SPECULAR != 0;
    // On a model-space-normal material the `_s` red channel is the gloss mask the specular term
    // is multiplied by, and an unbound `_s` samples as zero in Direct3D 11: without one the
    // material has no specular at all, whatever its strength.
    let unmasked_model_space =
        material.shader_flags_1 & SLSF1_MODEL_SPACE_NORMALS != 0 && specular.is_none();
    let strength = if enabled && !unmasked_model_space {
        material.specular_strength.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mask = normal
        .filter(|_| strength > 0.0 && material.shader_flags_1 & SLSF1_MODEL_SPACE_NORMALS == 0);
    // Always publish zero explicitly: omitting the extension enables glTF's default shine.
    let mut extension = serde_json::json!({
        "specularFactor": strength,
        "specularColorFactor": material.specular_color.map(|value| value.clamp(0.0, 1.0))
    });
    if let Some(slot) = mask {
        extension["specularTexture"] = serde_json::json!({
            "index": registry.texture(&slot.path, glb_output_path, false, material.texture_clamp_mode)?,
            "texCoord": 0
        });
        // Keep the glTF factor bounded. The native loader owns Bevy-specific compensation.
        output["extras"]["openSkyrim"]["specularMask"] = serde_json::json!("normal_alpha");
    }
    if let Some(slot) = specular {
        extension["specularColorTexture"] = serde_json::json!({
            "index": registry.texture(&slot.path, glb_output_path, true, material.texture_clamp_mode)?,
            "texCoord": 0
        });
    }
    output["extensions"]["KHR_materials_specular"] = extension;
    used_extensions.insert("KHR_materials_specular".to_owned());
    Ok(())
}

fn publish_skyrim_extension(
    output: &mut serde_json::Value,
    material: &ValidatedNifMaterial,
    glb_output_path: &Path,
    registry: &mut TextureRegistry,
    used_extensions: &mut BTreeSet<String>,
) -> Result<()> {
    let mut slots = Vec::new();
    for slot in &material.textures {
        if matches!(
            slot.semantic,
            NifTextureSemantic::Diffuse
                | NifTextureSemantic::Normal
                | NifTextureSemantic::Glow
                | NifTextureSemantic::Specular
        ) {
            continue;
        }
        slots.push(serde_json::json!({
            "slot": slot.slot,
            "semantic": slot.semantic,
            "texture": registry.texture(
                &slot.path,
                glb_output_path,
                slot.semantic.is_srgb_color(),
                material.texture_clamp_mode,
            )?,
            "required": slot.required,
            "colorSpace": if slot.semantic.is_srgb_color() { "srgb" } else { "linear" }
        }));
    }
    let premultiplied_alpha = material.shader_flags_2 & SLSF2_PREMULTIPLIED_ALPHA != 0;
    let screen_door_alpha_fade = material.shader_flags_1 & SLSF1_SCREENDOOR_ALPHA_FADE != 0;
    if slots.is_empty()
        && !premultiplied_alpha
        && !screen_door_alpha_fade
        && material.tint_color.is_none()
    {
        return Ok(());
    }
    let mut extension = serde_json::json!({
        "shaderFamily": material.shader_family,
        "lightingShaderType": material.lighting_shader_type,
        "shaderFlags1": material.shader_flags_1,
        "shaderFlags2": material.shader_flags_2,
        "premultipliedAlpha": premultiplied_alpha,
        "screenDoorAlphaFade": screen_door_alpha_fade,
        "textureSlots": slots
    });
    if let Some(tint) = material.tint_color {
        extension["tintColor"] = serde_json::json!(tint);
    }
    output["extensions"]["OPEN_SKYRIM_material"] = extension;
    used_extensions.insert("OPEN_SKYRIM_material".to_owned());
    Ok(())
}

fn texture_with_semantic(
    material: &ValidatedNifMaterial,
    semantic: NifTextureSemantic,
) -> Option<&NifTextureSlot> {
    material
        .textures
        .iter()
        .find(|slot| slot.semantic == semantic)
}

fn runtime_texture_uri(glb_output_path: &Path, canonical_texture_path: &str) -> Result<String> {
    let components = glb_output_path.components().collect::<Vec<_>>();
    let Some(meshes_index) = components.iter().rposition(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("meshes")
    }) else {
        return Ok(canonical_texture_path.to_owned());
    };
    let directories_below_meshes = components
        .len()
        .saturating_sub(meshes_index)
        .saturating_sub(2);
    Ok(format!(
        "{}{}",
        "../".repeat(directories_below_meshes.saturating_add(1)),
        canonical_texture_path
    ))
}

fn modern_shape(block: &NifBlock) -> Option<&BSTriShape> {
    match block {
        NifBlock::BSTriShape(shape) => Some(shape),
        NifBlock::BSDynamicTriShape(shape) => Some(&shape.bs_tri_shape),
        NifBlock::BSSubIndexTriShape(shape) => Some(&shape.bs_tri_shape),
        NifBlock::BSLODTriShape(shape) => Some(&shape.bs_tri_shape),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_shape_material(
    nif: &NifFile,
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    shader_reference: u32,
    alpha_reference: u32,
    animation_skips: &mut NifAnimationSkips,
) -> Result<NifMaterialDisposition> {
    if shape_name == Some("EditorMarker") && nif.blocks.iter().any(|block| {
        matches!(block, NifBlock::BSXFlags(flags) if flags.integer_data & (1 << 5) != 0)
    }) {
        return Ok(NifMaterialDisposition::Excluded { reason: "declared BSX EditorMarker".into() });
    }
    if shader_reference == NULL_BLOCK {
        return Ok(NifMaterialDisposition::Excluded {
            reason: "shape has no shader property".to_owned(),
        });
    }
    let shader_index = usize::try_from(shader_reference)?;
    let shader = nif.blocks.get(shader_index).ok_or_else(|| {
        material_error(
            source,
            shape_block,
            shape_name,
            format!("shader reference {shader_reference} is out of range"),
        )
    })?;
    let flags = match shader {
        NifBlock::BSLightingShaderProperty(p) => p.shader_flags_1.raw(),
        NifBlock::BSEffectShaderProperty(p) => p.shader_flags_1.raw(),
        _ => 0,
    };
    if flags & (1 << 16) != 0 {
        return Ok(NifMaterialDisposition::Excluded {
            reason: "unsupported Fire_Refraction distortion surface".into(),
        });
    }
    let alpha = resolve_alpha(nif, source, shape_block, shape_name, alpha_reference)?;
    let animation = collect_material_animation(nif, shader_reference, animation_skips);
    let material = match shader {
        NifBlock::BSLightingShaderProperty(property) => build_lighting_material(
            nif,
            source,
            shape_block,
            shape_name,
            shader_reference,
            property,
            alpha,
            animation,
        )?,
        NifBlock::BSEffectShaderProperty(property) => build_effect_material(
            source,
            shape_block,
            shape_name,
            shader_reference,
            property,
            alpha,
            animation,
        )?,
        NifBlock::Unhandled => {
            let kind = nif.header.get_block_type(shader_index).unwrap_or("unknown");
            return Ok(NifMaterialDisposition::Excluded {
                reason: format!("{kind} is outside the static material contract"),
            });
        }
        _ => {
            let kind = nif.header.get_block_type(shader_index).unwrap_or("unknown");
            return Err(material_error(
                source,
                shape_block,
                shape_name,
                format!("shader reference {shader_reference} points to incompatible {kind}"),
            ));
        }
    };
    validate_material(source, shape_block, shape_name, &material)?;
    Ok(NifMaterialDisposition::Validated { material })
}

fn resolve_alpha<'a>(
    nif: &'a NifFile,
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    reference: u32,
) -> Result<Option<(u32, &'a NiAlphaProperty)>> {
    if reference == NULL_BLOCK {
        return Ok(None);
    }
    match nif.blocks.get(reference as usize) {
        Some(NifBlock::NiAlphaProperty(property)) => Ok(Some((reference, property))),
        Some(_) => Err(material_error(
            source,
            shape_block,
            shape_name,
            format!("alpha reference {reference} does not point to NiAlphaProperty"),
        )),
        None => Err(material_error(
            source,
            shape_block,
            shape_name,
            format!("alpha reference {reference} is out of range"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_lighting_material(
    nif: &NifFile,
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    shader_block: u32,
    property: &BSLightingShaderProperty,
    alpha: Option<(u32, &NiAlphaProperty)>,
    animation: Vec<NifMaterialAnimationChannel>,
) -> Result<ValidatedNifMaterial> {
    let shader_type = LightingShaderType::try_from(property.shader_type).map_err(|error| {
        material_error(
            source,
            shape_block,
            shape_name,
            format!("shader block {shader_block}: {error}"),
        )
    })?;
    let flags_1 = property.shader_flags_1.raw();
    let flags_2 = property.shader_flags_2.raw();
    let material_alpha = normalize_alpha(
        source,
        shape_block,
        shape_name,
        shader_block,
        property.alpha,
    )?;
    let texture_set =
        resolve_texture_set(nif, source, shape_block, shape_name, property.texture_set)?;
    let textures = texture_set
        .map(|(block, set)| {
            lighting_texture_slots(set, shader_type, flags_1, flags_2)
                .map(|slots| (block, slots))
                .map_err(|error| {
                    material_error(
                        source,
                        shape_block,
                        shape_name,
                        format!("shader block {shader_block}: {error:#}"),
                    )
                })
        })
        .transpose()?;
    let (alpha_mode, alpha_threshold, alpha_property_block) =
        alpha_contract(alpha, flags_1, flags_2);
    Ok(ValidatedNifMaterial {
        shader_family: NifShaderFamily::Lighting,
        lighting_shader_type: Some(shader_type),
        shader_block,
        texture_set_block: textures.as_ref().map(|(block, _)| *block),
        alpha_property_block,
        shader_flags_1: flags_1,
        shader_flags_2: flags_2,
        base_color: [1.0, 1.0, 1.0, material_alpha],
        alpha: material_alpha,
        alpha_mode,
        alpha_threshold,
        glossiness: property.glossiness,
        specular_color: property.specular_color.0.to_array(),
        specular_strength: property.specular_strength,
        emissive_color: property.emissive_color.0.to_array(),
        emissive_multiple: source_emission_multiple(
            source,
            shape_block,
            shape_name,
            shader_block,
            property.emissive_multiple,
        )?,
        double_sided: flags_2 & SLSF2_DOUBLE_SIDED != 0,
        uv_offset: property.uv_offset.0.to_array(),
        uv_scale: property.uv_scale.0.to_array(),
        texture_clamp_mode: match property.texture_clamp_mode {
            project_wormhole_nif::nif_enum::TexClampMode::ClampSClampT => 0,
            project_wormhole_nif::nif_enum::TexClampMode::ClampSWrapT => 1,
            project_wormhole_nif::nif_enum::TexClampMode::WrapSClampT => 2,
            project_wormhole_nif::nif_enum::TexClampMode::WrapSWrapT => 3,
        },
        textures: textures.map(|(_, slots)| slots).unwrap_or_default(),
        tint_color: lighting_tint_color(shader_type, property),
        animation,
    })
}

/// The tint colour a `skin_tint` or `hair_tint` shader carries in its type-specific fields.
fn lighting_tint_color(
    shader_type: LightingShaderType,
    property: &BSLightingShaderProperty,
) -> Option<[f32; 3]> {
    if !matches!(
        shader_type,
        LightingShaderType::SkinTint | LightingShaderType::HairTint
    ) {
        return None;
    }
    let colour = property.type_data.tint_color()?.0.to_array();
    colour
        .iter()
        .all(|channel| channel.is_finite())
        .then_some(colour)
}

fn build_effect_material(
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    shader_block: u32,
    property: &BSEffectShaderProperty,
    alpha: Option<(u32, &NiAlphaProperty)>,
    animation: Vec<NifMaterialAnimationChannel>,
) -> Result<ValidatedNifMaterial> {
    let flags_1 = property.shader_flags_1.raw();
    let flags_2 = property.shader_flags_2.raw();
    let mut textures = Vec::new();
    push_effect_texture(
        &mut textures,
        0,
        NifTextureSemantic::Diffuse,
        &property.source_texture.0,
        true,
    )
    .map_err(|error| {
        material_error(
            source,
            shape_block,
            shape_name,
            format!("shader block {shader_block}: {error:#}"),
        )
    })?;
    push_effect_texture(
        &mut textures,
        1,
        NifTextureSemantic::Greyscale,
        &property.greyscale_texture.0,
        false,
    )
    .map_err(|error| {
        material_error(
            source,
            shape_block,
            shape_name,
            format!("shader block {shader_block}: {error:#}"),
        )
    })?;
    let (alpha_mode, alpha_threshold, alpha_property_block) =
        alpha_contract(alpha, flags_1, flags_2);
    let mut color = property.base_color.0.to_array();
    color[3] = normalize_alpha(source, shape_block, shape_name, shader_block, color[3])?;
    let material = ValidatedNifMaterial {
        shader_family: NifShaderFamily::Effect,
        lighting_shader_type: None,
        shader_block,
        texture_set_block: None,
        alpha_property_block,
        shader_flags_1: flags_1,
        shader_flags_2: flags_2,
        base_color: color,
        alpha: color[3],
        alpha_mode,
        alpha_threshold,
        glossiness: 0.0,
        specular_color: [0.0; 3],
        specular_strength: 0.0,
        emissive_color: [color[0], color[1], color[2]],
        emissive_multiple: property.base_color_scale,
        double_sided: flags_2 & SLSF2_DOUBLE_SIDED != 0,
        uv_offset: property.uv_offset.0.to_array(),
        uv_scale: property.uv_scale.0.to_array(),
        texture_clamp_mode: property.texture_clamp_mode & 3,
        tint_color: None,
        textures,
        animation,
    };
    validate_material(source, shape_block, shape_name, &material)?;
    Ok(material)
}

/// One shader variable a shape's shader property animates.
///
/// The contract is the `OPEN_SKYRIM_material_animation` glTF extension agreed
/// with the engine side (`docs/specs/converters/nif-to-gltf.md`): a channel is a
/// variable name, its keyframes, the interpolation between them and the timing
/// the controller replays them on. Skyrim stores the timing on the controller
/// and the keys on an interpolator, so one channel is one controller of the
/// shape's `NiTimeController` chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NifMaterialAnimationChannel {
    /// The shader variable this channel drives, in the camelCase spelling of
    /// the nif.xml enums (`EffectShaderControlledVariable`,
    /// `LightingShaderControlledVariable`).
    pub variable: String,
    pub interpolation: NifAnimationInterpolation,
    pub times: Vec<f32>,
    pub values: Vec<f32>,
    /// `[outgoing, incoming]` tangents per key, present only for `QUADRATIC`: the key's
    /// NIF `Backward` and `Forward` fields, in that order (see where they are collected).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tangents: Vec<[f32; 2]>,
    /// `NiTimeController` cycle mode: `cycle`, `reverse` or `clamp`.
    #[serde(rename = "loop")]
    pub loop_mode: String,
    pub frequency: f32,
    pub phase: f32,
    pub start: f32,
    pub stop: f32,
    /// Values per key: 1 for a float variable, 3 for a colour (`values` and `tangents` are then
    /// key-major, `[r0, g0, b0, r1, ...]`). Omitted when 1, so float channels read as before.
    #[serde(
        default = "one_animation_component",
        skip_serializing_if = "is_one_animation_component"
    )]
    pub components: u8,
}

fn one_animation_component() -> u8 {
    1
}

fn is_one_animation_component(components: &u8) -> bool {
    *components == 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum NifAnimationInterpolation {
    Linear,
    Quadratic,
    Step,
}

/// Counters for controllers that were dropped instead of published, keyed by
/// the reason.
///
/// Animation is never a conversion failure: a controller the converter cannot
/// read costs one still frame, while failing the conversion costs the model.
pub type NifAnimationSkips = BTreeMap<String, usize>;

fn record_animation_skip(skips: &mut NifAnimationSkips, reason: &str) {
    *skips.entry(reason.to_owned()).or_default() += 1;
}

/// Everything the collector needs from one controller block.
struct ShaderFloatController {
    next_controller: u32,
    target: u32,
    flags: u16,
    frequency: f32,
    phase: f32,
    start_time: f32,
    stop_time: f32,
    interpolator: u32,
    /// `None` for a variable no renderer input maps to.
    variable: Option<&'static str>,
    /// 1 for a float controller, 3 for a colour controller.
    components: u8,
}

/// The shader variable an effect-shader float controller drives
/// (`EffectShaderControlledVariable`, nif.xml).
fn effect_shader_variable(variable: &EffectShaderControlledVariable) -> Option<&'static str> {
    Some(match variable {
        EffectShaderControlledVariable::EmissiveMultiple => "emissiveMultiple",
        EffectShaderControlledVariable::FalloffStartAngle => "falloffStartAngle",
        EffectShaderControlledVariable::FalloffStopAngle => "falloffStopAngle",
        EffectShaderControlledVariable::FalloffStartOpacity => "falloffStartOpacity",
        EffectShaderControlledVariable::FalloffStopOpacity => "falloffStopOpacity",
        EffectShaderControlledVariable::AlphaTransparency => "alpha",
        EffectShaderControlledVariable::UOffset => "uOffset",
        EffectShaderControlledVariable::UScale => "uScale",
        EffectShaderControlledVariable::VOffset => "vOffset",
        EffectShaderControlledVariable::VScale => "vScale",
        // `Unknown11` to `Unknown14` name no shader input; publishing them
        // would invite playback of a variable nothing consumes.
        EffectShaderControlledVariable::Unknown11
        | EffectShaderControlledVariable::Unknown12
        | EffectShaderControlledVariable::Unknown13
        | EffectShaderControlledVariable::Unknown14 => return None,
    })
}

/// The shader variable a lighting-shader float controller drives
/// (`LightingShaderControlledVariable`, nif.xml).
///
/// The variables the two enums share keep the same published name (`alpha`,
/// `emissiveMultiple`, `uOffset`, `uScale`, `vOffset`, `vScale`); the lighting
/// family's own variables are named in camelCase.
fn lighting_shader_variable(variable: &LightingShaderControlledFloat) -> Option<&'static str> {
    Some(match variable {
        LightingShaderControlledFloat::RefractionStrength => "refractionStrength",
        LightingShaderControlledFloat::EnvironmentMapScale => "environmentMapScale",
        LightingShaderControlledFloat::Glossiness => "glossiness",
        LightingShaderControlledFloat::SpecularStrength => "specularStrength",
        LightingShaderControlledFloat::EmissiveMultiple => "emissiveMultiple",
        LightingShaderControlledFloat::Alpha => "alpha",
        LightingShaderControlledFloat::UOffset => "uOffset",
        LightingShaderControlledFloat::UScale => "uScale",
        LightingShaderControlledFloat::VOffset => "vOffset",
        LightingShaderControlledFloat::VScale => "vScale",
        LightingShaderControlledFloat::Unknown3
        | LightingShaderControlledFloat::Unknown4
        | LightingShaderControlledFloat::Unknown13
        | LightingShaderControlledFloat::Unknown14 => return None,
    })
}

fn time_controller_parts(
    controller: &NiTimeController,
    interpolator: u32,
    variable: Option<&'static str>,
) -> ShaderFloatController {
    ShaderFloatController {
        next_controller: controller.next_controller,
        target: controller.target,
        flags: controller.flags,
        frequency: controller.frequency,
        phase: controller.phase,
        start_time: controller.start_time,
        stop_time: controller.stop_time,
        interpolator,
        variable,
        components: 1,
    }
}

/// A colour controller: `NiPoint3InterpController` (no fields of its own) → `NiSingleInterpController`.
fn color_interp_parts(
    single: &NiSingleInterpController,
    variable: Option<&'static str>,
) -> ShaderFloatController {
    ShaderFloatController {
        components: 3,
        ..time_controller_parts(&single.parent.parent, single.interpolator, variable)
    }
}

/// `NiFloatInterpController` → `NiSingleInterpController` (which holds the
/// interpolator) → `NiInterpController` → `NiTimeController`.
fn float_interp_parts(
    controller: &NiFloatInterpController,
    variable: Option<&'static str>,
) -> ShaderFloatController {
    let single = &controller.parent;
    time_controller_parts(&single.parent.parent, single.interpolator, variable)
}

/// Reads the controller block at `index`, whatever shape the block took.
///
/// Both float-controller block types dispatch to their own struct in the
/// vendored parser (`vendor/project-wormhole-nif/src/nif_block.rs`); a block
/// that fails to parse, is out of range or points at an unrelated block type
/// arrives as something other than one of these two variants.
fn shader_float_controller(
    nif: &NifFile,
    index: u32,
) -> std::result::Result<ShaderFloatController, &'static str> {
    let Some(block) = usize::try_from(index)
        .ok()
        .and_then(|index| nif.blocks.get(index))
    else {
        return Err("controller block out of range");
    };
    match block {
        NifBlock::BSLightingShaderPropertyFloatController(controller) => Ok(float_interp_parts(
            &controller.parent,
            lighting_shader_variable(&controller.controlled_variable),
        )),
        NifBlock::BSEffectShaderPropertyFloatController(controller) => Ok(float_interp_parts(
            &controller.parent,
            effect_shader_variable(&controller.controlled_variable),
        )),
        // nif.xml `EffectShaderControlledColor` / `LightingShaderControlledColor`.
        NifBlock::BSEffectShaderPropertyColorController(controller) => Ok(color_interp_parts(
            &controller.parent,
            (controller.controlled_color == 0).then_some("emissiveColor"),
        )),
        NifBlock::BSLightingShaderPropertyColorController(controller) => Ok(color_interp_parts(
            &controller.parent,
            match controller.controlled_color {
                0 => Some("specularColor"),
                1 => Some("emissiveColor"),
                _ => None,
            },
        )),
        _ => Err("unsupported controller type"),
    }
}

/// The controller chain heads that drive a shape's shader property.
///
/// The canonical link is the shader property's inherited `NiProperty`
/// controller reference (`property.controller`, kept instead of discarded -
/// see `NiProperty` in the vendored parser). Controllers whose
/// `NiTimeController.target` names this shader property are collected as a
/// second, independent source, so a file that leaves either field null still
/// animates; the collector visits each block once, so a chain found twice
/// publishes one set of channels.
fn animation_controller_heads(nif: &NifFile, shader_block: u32) -> Vec<u32> {
    let mut heads = Vec::new();
    if let Some(controller) = shader_property_controller(nif, shader_block) {
        heads.push(controller);
    }
    for index in 0..nif.blocks.len() {
        let Ok(index) = u32::try_from(index) else {
            break;
        };
        let Ok(controller) = shader_float_controller(nif, index) else {
            continue;
        };
        if controller.target == shader_block && !heads.contains(&index) {
            heads.push(index);
        }
    }
    heads
}

/// The controller reference in a shader property's inherited `NiProperty`.
fn shader_property_controller(nif: &NifFile, shader_block: u32) -> Option<u32> {
    match nif.blocks.get(usize::try_from(shader_block).ok()?)? {
        NifBlock::BSLightingShaderProperty(property) => {
            Some(property.ni_shader_property.controller)
        }
        NifBlock::BSEffectShaderProperty(property) => Some(property.parent.controller),
        _ => None,
    }
}

/// Builds the channel for one controller, or records why it was dropped.
fn build_animation_channel(
    nif: &NifFile,
    controller: &ShaderFloatController,
    skips: &mut NifAnimationSkips,
) -> Option<NifMaterialAnimationChannel> {
    let Some(variable) = controller.variable else {
        record_animation_skip(skips, "unknown controlled variable");
        return None;
    };
    let Some(loop_mode) = animation_loop_mode(controller.flags) else {
        record_animation_skip(skips, "unknown loop mode");
        return None;
    };
    let keys = if controller.components == 3 {
        color_animation_keys(nif, controller.interpolator)
    } else {
        float_animation_keys(nif, controller.interpolator)
    };
    let (interpolation, times, values, tangents) = match keys {
        Ok(keys) => keys,
        Err(reason) => {
            record_animation_skip(skips, reason);
            return None;
        }
    };
    let timing = [
        controller.frequency,
        controller.phase,
        controller.start_time,
        controller.stop_time,
    ];
    let finite = times
        .iter()
        .chain(values.iter())
        .chain(tangents.iter().flatten())
        .chain(timing.iter())
        .all(|value| value.is_finite());
    if !finite {
        // `serde_json` writes a non-finite float as `null`, which would break
        // the extension for every consumer; drop the channel instead.
        record_animation_skip(skips, "non-finite key data");
        return None;
    }
    Some(NifMaterialAnimationChannel {
        variable: variable.to_owned(),
        interpolation,
        times,
        values,
        tangents: if interpolation == NifAnimationInterpolation::Quadratic {
            tangents
        } else {
            Vec::new()
        },
        loop_mode: loop_mode.to_owned(),
        frequency: controller.frequency,
        phase: controller.phase,
        start: controller.start_time,
        stop: controller.stop_time,
        components: controller.components,
    })
}

/// A channel's keys: interpolation, one time per key, `components` values per key, and one
/// `[outgoing, incoming]` tangent pair per value.
type AnimationKeys = (NifAnimationInterpolation, Vec<f32>, Vec<f32>, Vec<[f32; 2]>);

/// The keys of a float controller's `NiFloatInterpolator` → `NiFloatData`.
fn float_animation_keys(
    nif: &NifFile,
    interpolator: u32,
) -> std::result::Result<AnimationKeys, &'static str> {
    let Some(NifBlock::NiFloatInterpolator(interpolator)) = nif.blocks.get(interpolator as usize)
    else {
        return Err("missing float interpolator");
    };
    let Some(NifBlock::NiFloatData(data)) = nif.blocks.get(interpolator.data as usize) else {
        return Err("missing float data");
    };
    let interpolation =
        animation_interpolation(&data.data.key_type).ok_or("unsupported key type")?;
    let mut times = Vec::with_capacity(data.data.keys.len());
    let mut values = Vec::with_capacity(data.data.keys.len());
    let mut tangents = Vec::with_capacity(data.data.keys.len());
    for key in &data.data.keys {
        times.push(key.time);
        values.push(key.value);
        // Published as [outgoing, incoming]. The NIF names them the other way round: the
        // segment from key i to key i+1 uses key i's `Backward` as its outgoing slope and key
        // i+1's `Forward` as its incoming one (NifSkope's evaluator, src/gl/glcontroller.cpp).
        tangents.push([key.backward.unwrap_or(0.0), key.forward.unwrap_or(0.0)]);
    }
    Ok((interpolation, times, values, tangents))
}

/// The keys of a colour controller's `NiPoint3Interpolator` → `NiPosData`, flattened key-major:
/// `[r0, g0, b0, r1, ...]`, and each component's tangent pair in the same order.
fn color_animation_keys(
    nif: &NifFile,
    interpolator: u32,
) -> std::result::Result<AnimationKeys, &'static str> {
    let Some(NifBlock::NiPoint3Interpolator(interpolator)) = nif.blocks.get(interpolator as usize)
    else {
        return Err("missing point3 interpolator");
    };
    let Some(NifBlock::NiPosData(data)) = nif.blocks.get(interpolator.data as usize) else {
        return Err("missing point3 data");
    };
    let interpolation = data
        .key_type
        .as_ref()
        .and_then(animation_interpolation)
        .ok_or("unsupported key type")?;
    let mut times = Vec::with_capacity(data.keys.len());
    let mut values = Vec::with_capacity(data.keys.len() * 3);
    let mut tangents = Vec::with_capacity(data.keys.len() * 3);
    for key in &data.keys {
        times.push(key.time);
        values.extend(key.value);
        let backward = key.backward.unwrap_or([0.0; 3]);
        let forward = key.forward.unwrap_or([0.0; 3]);
        // [outgoing, incoming] = [Backward, Forward], per component, as for floats.
        tangents.extend((0..3).map(|component| [backward[component], forward[component]]));
    }
    Ok((interpolation, times, values, tangents))
}

/// `NiTimeController` cycle mode, bits 1-2 of its flags (nif.xml `CycleType`).
fn animation_loop_mode(flags: u16) -> Option<&'static str> {
    match (flags >> 1) & 3 {
        0 => Some("cycle"),
        1 => Some("reverse"),
        2 => Some("clamp"),
        _ => None,
    }
}

fn animation_interpolation(key_type: &KeyType) -> Option<NifAnimationInterpolation> {
    match key_type {
        KeyType::LinearKey => Some(NifAnimationInterpolation::Linear),
        KeyType::QuadraticKey => Some(NifAnimationInterpolation::Quadratic),
        KeyType::ConstKey => Some(NifAnimationInterpolation::Step),
        // TBC and XYZ-rotation keys carry data this contract does not express.
        KeyType::TbcKey | KeyType::XyzRotationKey => None,
    }
}

/// Collects every float controller channel a shape's shader property drives.
///
/// A controller with no variable, no interpolator, no key data, an unreadable
/// key type or a non-finite value is dropped and counted in `skips`, never
/// fatal: the shape keeps its static material.
fn collect_material_animation(
    nif: &NifFile,
    shader_block: u32,
    skips: &mut NifAnimationSkips,
) -> Vec<NifMaterialAnimationChannel> {
    let mut channels = Vec::new();
    let mut visited = BTreeSet::new();
    for head in animation_controller_heads(nif, shader_block) {
        let mut next = Some(head);
        while let Some(index) = next {
            if index == NULL_BLOCK || !visited.insert(index) {
                break;
            }
            let controller = match shader_float_controller(nif, index) {
                Ok(controller) => controller,
                Err(reason) => {
                    record_animation_skip(skips, reason);
                    break;
                }
            };
            if let Some(channel) = build_animation_channel(nif, &controller, skips) {
                channels.push(channel);
            }
            next = (controller.next_controller != NULL_BLOCK).then_some(controller.next_controller);
        }
    }
    channels
}

fn resolve_texture_set<'a>(
    nif: &'a NifFile,
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    reference: u32,
) -> Result<Option<(u32, &'a BSShaderTextureSet)>> {
    if reference == NULL_BLOCK {
        return Ok(None);
    }
    match nif.blocks.get(reference as usize) {
        Some(NifBlock::BSShaderTextureSet(set)) => Ok(Some((reference, set))),
        Some(_) => Err(material_error(
            source,
            shape_block,
            shape_name,
            format!("texture-set reference {reference} does not point to BSShaderTextureSet"),
        )),
        None => Err(material_error(
            source,
            shape_block,
            shape_name,
            format!("texture-set reference {reference} is out of range"),
        )),
    }
}

fn lighting_texture_slots(
    set: &BSShaderTextureSet,
    shader_type: LightingShaderType,
    flags_1: u32,
    flags_2: u32,
) -> Result<Vec<NifTextureSlot>> {
    let mut slots = Vec::new();
    for (index, path) in set.textures.iter().enumerate() {
        let Some(path) = path else { continue };
        slots.push(lighting_texture_slot(
            index,
            path,
            shader_type,
            flags_1,
            flags_2,
        )?);
    }
    Ok(slots)
}

/// One `BSShaderTextureSet` slot of a lighting shader, classified by the shader type and flags.
fn lighting_texture_slot(
    index: usize,
    path: &str,
    shader_type: LightingShaderType,
    flags_1: u32,
    flags_2: u32,
) -> Result<NifTextureSlot> {
    let environment = matches!(
        shader_type,
        LightingShaderType::EnvironmentMap
            | LightingShaderType::EyeEnvironmentMap
            | LightingShaderType::MultiLayerParallax
    ) || flags_1 & SLSF1_ENVIRONMENT_MAPPING != 0;
    let glow = matches!(shader_type, LightingShaderType::Glow) || flags_2 & SLSF2_GLOW_MAP != 0;
    let height = matches!(
        shader_type,
        LightingShaderType::Parallax
            | LightingShaderType::ParallaxOcclusion
            | LightingShaderType::MultiLayerParallax
    );
    // Slot 6 is the multilayer-parallax inner layer, but a FaceGen head keeps its tint texture
    // there, which the `FACEGEN` lighting shader samples as `TexTintSampler`.
    let facegen = matches!(shader_type, LightingShaderType::FaceTint);
    let semantic = match index {
        0 => NifTextureSemantic::Diffuse,
        1 => NifTextureSemantic::Normal,
        2 if glow => NifTextureSemantic::Glow,
        2 => NifTextureSemantic::Unclassified,
        3 if height => NifTextureSemantic::Height,
        3 => NifTextureSemantic::Detail,
        4 if environment => NifTextureSemantic::EnvironmentCube,
        4 => NifTextureSemantic::Unclassified,
        5 if environment => NifTextureSemantic::EnvironmentMask,
        5 => NifTextureSemantic::Unclassified,
        6 if facegen => NifTextureSemantic::FacegenTint,
        6 => NifTextureSemantic::InnerLayer,
        7 => NifTextureSemantic::Specular,
        _ => NifTextureSemantic::Unclassified,
    };
    texture_slot(
        index,
        semantic,
        path,
        index == 0 || (environment && matches!(index, 4 | 5)),
    )
}

/// A `TXST` texture set laid out in `BSShaderTextureSet` slot order, as the game applies it to a
/// NIF shape: `TX00` diffuse (0), `TX01` normal (1), `TX03` glow/skin (2), `TX04` height (3),
/// `TX05` environment (4), `TX02` environment mask (5), `TX06` multilayer (6), `TX07` specular (7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NifSlotTextureSet {
    /// The `TXST` FormID, for diagnostics.
    pub form_id: u32,
    /// The path for each NIF slot; `None` or empty leaves the shape's own slot in place.
    pub slots: [Option<String>; 8],
}

/// Applies an armature's skin texture set over a `skin_tint` lighting material's own textures.
///
/// The game replaces the skin shapes' textures with the `ARMA` skin `TXST` (`NAM0` male, `NAM1`
/// female), so a part NIF's texture set is only a default: retail `femalehands_1.nif` has no `_s`
/// while its `SkinHandFemale_1` set has `FemaleHands_1_S.dds`. A slot the set leaves empty keeps
/// the NIF's path. Returns whether the material is a `skin_tint` lighting material, the only kind
/// the set applies to.
pub fn apply_skin_texture_set(
    material: &mut ValidatedNifMaterial,
    set: &NifSlotTextureSet,
) -> Result<bool> {
    let (NifShaderFamily::Lighting, Some(shader_type @ LightingShaderType::SkinTint)) =
        (material.shader_family, material.lighting_shader_type)
    else {
        return Ok(false);
    };
    for (index, path) in set.slots.iter().enumerate() {
        let Some(path) = path.as_deref().filter(|path| !path.trim().is_empty()) else {
            continue;
        };
        let slot = lighting_texture_slot(
            index,
            path,
            shader_type,
            material.shader_flags_1,
            material.shader_flags_2,
        )
        .map_err(|error| {
            color_eyre::eyre::eyre!(
                "texture set {:08X} slot {index} ({path}): {error:#}",
                set.form_id
            )
        })?;
        match material
            .textures
            .iter_mut()
            .find(|existing| existing.slot == slot.slot)
        {
            Some(existing) => *existing = slot,
            None => material.textures.push(slot),
        }
    }
    material.textures.sort_by_key(|slot| slot.slot);
    Ok(true)
}

fn push_effect_texture(
    slots: &mut Vec<NifTextureSlot>,
    slot: usize,
    semantic: NifTextureSemantic,
    path: &str,
    required: bool,
) -> Result<()> {
    if !path.is_empty() {
        slots.push(texture_slot(slot, semantic, path, required)?);
    }
    Ok(())
}

fn texture_slot(
    index: usize,
    semantic: NifTextureSemantic,
    path: &str,
    required: bool,
) -> Result<NifTextureSlot> {
    let normalized = if Path::new(path).extension().is_some_and(|extension| {
        ["tga", "bmp", "png", "jpg", "jpeg"]
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    }) {
        let mut path = PathBuf::from(path);
        path.set_extension("dds");
        path.to_string_lossy().into_owned()
    } else {
        path.to_owned()
    };
    Ok(NifTextureSlot {
        slot: u8::try_from(index)?,
        semantic,
        path: canonical_asset_path(&normalized, AssetKind::Texture, "dds")?,
        required,
    })
}

fn alpha_contract(
    alpha: Option<(u32, &NiAlphaProperty)>,
    _shader_flags_1: u32,
    _shader_flags_2: u32,
) -> (NifAlphaMode, Option<u8>, Option<u32>) {
    let mut alpha_property_block = None;
    if let Some((block, property)) = alpha {
        alpha_property_block = Some(block);
        if property.flags.test_enabled() {
            return (NifAlphaMode::Cutout, Some(property.threshold), Some(block));
        }
        if property.flags.blend_enabled() {
            return (NifAlphaMode::Blend, None, Some(block));
        }
    }
    (NifAlphaMode::Opaque, None, alpha_property_block)
}

fn validate_material(
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    material: &ValidatedNifMaterial,
) -> Result<()> {
    ensure!(
        material.texture_clamp_mode <= 3,
        "invalid NIF texture clamp mode"
    );
    let values = material
        .base_color
        .into_iter()
        .chain(material.specular_color)
        .chain(material.emissive_color)
        .chain(material.uv_offset)
        .chain(material.uv_scale)
        .chain([
            material.alpha,
            material.glossiness,
            material.specular_strength,
            material.emissive_multiple,
        ]);
    ensure!(
        values.clone().all(f32::is_finite),
        "{}",
        material_error(
            source,
            shape_block,
            shape_name,
            format!(
                "shader block {} contains a non-finite material value",
                material.shader_block
            )
        )
    );
    ensure!(
        (0.0..=1.0).contains(&material.alpha),
        "{}",
        material_error(
            source,
            shape_block,
            shape_name,
            format!(
                "shader block {} alpha {} is outside [0, 1]",
                material.shader_block, material.alpha
            )
        )
    );
    ensure!(
        material.glossiness >= 0.0
            && material.specular_strength >= 0.0
            && (material.shader_family == NifShaderFamily::Lighting
                || material.emissive_multiple >= 0.0),
        "{}",
        material_error(
            source,
            shape_block,
            shape_name,
            format!(
                "shader block {} contains a negative intensity: glossiness={}, specular_strength={}, emissive_multiple={}",
                material.shader_block,
                material.glossiness,
                material.specular_strength,
                material.emissive_multiple
            )
        )
    );
    ensure!(
        (material.emissive_color.into_iter().fold(1.0_f32, f32::max)
            * material.emissive_multiple.max(0.0))
        .is_finite(),
        "{}",
        material_error(
            source,
            shape_block,
            shape_name,
            format!(
                "shader block {} contains overflowing emissive energy",
                material.shader_block
            )
        )
    );
    Ok(())
}

/// Validate the raw static multiplier; only glTF publication may clamp its sign.
fn source_emission_multiple(
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    shader_block: u32,
    multiple: f32,
) -> Result<f32> {
    ensure!(
        multiple.is_finite(),
        "{}",
        material_error(
            source,
            shape_block,
            shape_name,
            format!("shader block {shader_block} emissive multiplier is non-finite")
        ),
    );
    Ok(multiple)
}

fn normalize_alpha(
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    shader_block: u32,
    alpha: f32,
) -> Result<f32> {
    ensure!(
        alpha.is_finite(),
        "{}",
        material_error(
            source,
            shape_block,
            shape_name,
            format!("shader block {shader_block} alpha is non-finite"),
        )
    );
    Ok(alpha.clamp(0.0, 1.0))
}

fn material_error(
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    detail: String,
) -> color_eyre::Report {
    color_eyre::eyre::eyre!(
        "invalid NIF material in {}: shape block {} ({:?}): {}",
        source.display(),
        shape_block,
        shape_name,
        detail
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLSF1_OWN_EMIT: u32 = 1 << 22;

    fn fixture(
        mode: NifAlphaMode,
        emissive: bool,
        double_sided: bool,
        environment: bool,
    ) -> ValidatedNifMaterial {
        ValidatedNifMaterial {
            shader_family: NifShaderFamily::Lighting,
            lighting_shader_type: Some(if environment {
                LightingShaderType::EnvironmentMap
            } else {
                LightingShaderType::Default
            }),
            shader_block: 4,
            texture_set_block: Some(5),
            alpha_property_block: (mode != NifAlphaMode::Opaque).then_some(6),
            shader_flags_1: SLSF1_SPECULAR,
            shader_flags_2: 0,
            base_color: [1.0; 4],
            alpha: 1.0,
            alpha_mode: mode,
            alpha_threshold: (mode == NifAlphaMode::Cutout).then_some(128),
            glossiness: 32.0,
            specular_color: [1.0; 3],
            specular_strength: 1.0,
            emissive_color: if emissive { [1.0, 0.5, 0.25] } else { [0.0; 3] },
            emissive_multiple: if emissive { 2.0 } else { 0.0 },
            double_sided,
            uv_offset: [0.0; 2],
            uv_scale: [1.0; 2],
            texture_clamp_mode: 3,
            tint_color: None,
            textures: Vec::new(),
            animation: Vec::new(),
        }
    }

    fn shape(block: u32, material: ValidatedNifMaterial) -> NifShapeMaterial {
        NifShapeMaterial {
            shape_block: block,
            shape_name: Some(format!("shape-{block}")),
            shader_property_block: Some(material.shader_block),
            alpha_property_block: material.alpha_property_block,
            disposition: NifMaterialDisposition::Validated { material },
        }
    }

    fn gltf(mesh_count: usize) -> serde_json::Value {
        serde_json::json!({
            "asset": {"version": "2.0"},
            "meshes": (0..mesh_count)
                .map(|_| serde_json::json!({"primitives": [{}]}))
                .collect::<Vec<_>>(),
            "extensionsUsed": ["KHR_materials_pbrSpecularGlossiness"],
            "extensionsRequired": ["KHR_materials_pbrSpecularGlossiness"]
        })
    }

    #[test]
    fn ordinary_shape_cannot_publish_multiple_primitives_even_with_segment_extras() {
        let mut document = gltf(1);
        document["meshes"][0]["primitives"] = serde_json::json!([
            {"extras": {"openSkyrim": {"lodSegment": 0}}},
            {"extras": {"openSkyrim": {"lodSegment": 1}}}
        ]);
        let contract = [shape(
            10,
            fixture(NifAlphaMode::Opaque, false, false, false),
        )];
        assert!(
            publish_gltf_materials(
                &mut document,
                &contract,
                &[10],
                Path::new("meshes/ordinary.glb")
            )
            .is_err()
        );
    }

    fn publish_specular_fixture(material: ValidatedNifMaterial) -> serde_json::Value {
        let mut document = gltf(1);
        publish_gltf_materials(
            &mut document,
            &[shape(10, material)],
            &[10],
            Path::new("assets/meshes/specular.glb"),
        )
        .unwrap();
        document
    }

    #[test]
    fn v9_glossiness_is_a_monotonic_blinn_phong_exponent() {
        let mut previous = 2.0;
        for (exponent, expected) in [
            (0.0, 1.0),
            (5.0, 0.731_110_5),
            (30.0, 0.5),
            (80.0, 0.395_188_28),
            (100.0, 0.374_203_18),
            (200.0, 0.315_442_1),
            (400.0, 0.265_583_43),
        ] {
            let mut source = fixture(NifAlphaMode::Opaque, false, false, false);
            source.glossiness = exponent;
            let document = publish_specular_fixture(source);
            let roughness = document["materials"][0]["pbrMetallicRoughness"]["roughnessFactor"]
                .as_f64()
                .unwrap();
            assert!(
                (roughness - expected).abs() < 1e-5,
                "exponent {exponent}: {roughness}"
            );
            assert!(roughness > 0.0 && roughness <= 1.0 && roughness < previous);
            previous = roughness;
        }
    }

    #[test]
    fn v10_disabled_and_zero_specular_are_explicitly_nonreflecting() {
        for (flags, strength) in [(0, 1.0), (0, 0.0), (1, 0.0)] {
            let mut source = fixture(NifAlphaMode::Opaque, false, false, false);
            source.shader_flags_1 = flags;
            source.specular_strength = strength;
            source.textures.push(NifTextureSlot {
                slot: 1,
                semantic: NifTextureSemantic::Normal,
                path: "textures/mask_n.dds".into(),
                required: false,
            });
            let document = publish_specular_fixture(source);
            let material = &document["materials"][0];
            let specular = &material["extensions"]["KHR_materials_specular"];
            assert_eq!(specular["specularFactor"], 0.0);
            assert!(specular.get("specularTexture").is_none());
            assert!(
                material
                    .pointer("/extras/openSkyrim/specularMask")
                    .is_none()
            );
        }
    }

    /// Retail `femalehands_1.nif`: a model-space `skin_tint` material with the Specular flag,
    /// strength 1.0, and a texture set with diffuse, `_msn` and `_sk` but no `_s`.
    fn female_hands_material() -> ValidatedNifMaterial {
        let mut material = fixture(NifAlphaMode::Opaque, false, false, false);
        material.lighting_shader_type = Some(LightingShaderType::SkinTint);
        material.shader_flags_1 = SLSF1_SPECULAR | SLSF1_MODEL_SPACE_NORMALS;
        material.glossiness = 80.0;
        material.textures = vec![
            NifTextureSlot {
                slot: 0,
                semantic: NifTextureSemantic::Diffuse,
                path: "textures/actors/character/female/femalehands_1.dds".into(),
                required: true,
            },
            NifTextureSlot {
                slot: 1,
                semantic: NifTextureSemantic::Normal,
                path: "textures/actors/character/female/femalehands_1_msn.dds".into(),
                required: false,
            },
            NifTextureSlot {
                slot: 2,
                semantic: NifTextureSemantic::Unclassified,
                path: "textures/actors/character/female/femalehands_1_sk.dds".into(),
                required: false,
            },
        ];
        material
    }

    fn skin_hand_female_1() -> NifSlotTextureSet {
        let mut set = NifSlotTextureSet {
            form_id: 0x0003_EDE9,
            ..NifSlotTextureSet::default()
        };
        set.slots[0] = Some("Actors\\Character\\Female\\FemaleHands_1.dds".into());
        set.slots[1] = Some("Actors\\Character\\Female\\FemaleHands_1_msn.dds".into());
        set.slots[2] = Some("Actors\\Character\\Female\\FemaleHands_1_sk.dds".into());
        set.slots[7] = Some("Actors\\Character\\Female\\FemaleHands_1_S.dds".into());
        set
    }

    #[test]
    fn a_model_space_material_without_a_specular_map_publishes_zero_specular() {
        let document = publish_specular_fixture(female_hands_material());
        let specular = &document["materials"][0]["extensions"]["KHR_materials_specular"];
        assert_eq!(specular["specularFactor"], 0.0);
        assert!(specular.get("specularColorTexture").is_none());
        assert!(specular.get("specularTexture").is_none());

        // The tangent-space twin keeps its strength: there the normal map's alpha is the mask.
        let mut tangent = female_hands_material();
        tangent.shader_flags_1 = SLSF1_SPECULAR;
        let document = publish_specular_fixture(tangent);
        let specular = &document["materials"][0]["extensions"]["KHR_materials_specular"];
        assert_eq!(specular["specularFactor"], 1.0);
    }

    #[test]
    fn a_model_space_material_with_a_specular_map_keeps_its_strength() {
        let mut material = female_hands_material();
        material.textures.push(NifTextureSlot {
            slot: 7,
            semantic: NifTextureSemantic::Specular,
            path: "textures/actors/character/female/femalehands_1_s.dds".into(),
            required: false,
        });
        let document = publish_specular_fixture(material);
        let specular = &document["materials"][0]["extensions"]["KHR_materials_specular"];
        assert_eq!(specular["specularFactor"], 1.0);
        let image = specular["specularColorTexture"]["index"].as_u64().unwrap() as usize;
        let source = document["textures"][image]["source"].as_u64().unwrap() as usize;
        let uri = document["images"][source]["uri"].as_str().unwrap();
        assert!(uri.contains("femalehands_1_s"), "{uri}");
    }

    #[test]
    fn the_armature_skin_texture_set_supplies_the_missing_specular_map() {
        let mut material = female_hands_material();
        assert!(apply_skin_texture_set(&mut material, &skin_hand_female_1()).unwrap());
        let slots: Vec<_> = material
            .textures
            .iter()
            .map(|slot| (slot.slot, slot.semantic, slot.path.as_str()))
            .collect();
        assert_eq!(
            slots,
            [
                (
                    0,
                    NifTextureSemantic::Diffuse,
                    "textures/actors/character/female/femalehands_1.dds"
                ),
                (
                    1,
                    NifTextureSemantic::Normal,
                    "textures/actors/character/female/femalehands_1_msn.dds"
                ),
                (
                    2,
                    NifTextureSemantic::Unclassified,
                    "textures/actors/character/female/femalehands_1_sk.dds"
                ),
                (
                    7,
                    NifTextureSemantic::Specular,
                    "textures/actors/character/female/femalehands_1_s.dds"
                ),
            ]
        );
        let document = publish_specular_fixture(material);
        let specular = &document["materials"][0]["extensions"]["KHR_materials_specular"];
        assert_eq!(specular["specularFactor"], 1.0);
        assert!(specular.get("specularColorTexture").is_some());
    }

    #[test]
    fn a_skin_texture_set_replaces_set_slots_and_keeps_the_rest() {
        let mut material = female_hands_material();
        let mut set = NifSlotTextureSet::default();
        set.slots[0] = Some("actors\\character\\female\\astridhands.dds".into());
        set.slots[1] = Some(String::new());
        assert!(apply_skin_texture_set(&mut material, &set).unwrap());
        let paths: Vec<_> = material
            .textures
            .iter()
            .map(|slot| (slot.slot, slot.path.as_str()))
            .collect();
        assert_eq!(
            paths,
            [
                (0, "textures/actors/character/female/astridhands.dds"),
                (1, "textures/actors/character/female/femalehands_1_msn.dds"),
                (2, "textures/actors/character/female/femalehands_1_sk.dds"),
            ]
        );
    }

    #[test]
    fn a_skin_texture_set_leaves_other_shaders_alone() {
        let mut material = female_hands_material();
        material.lighting_shader_type = Some(LightingShaderType::Default);
        let before = material.clone();
        assert!(!apply_skin_texture_set(&mut material, &skin_hand_female_1()).unwrap());
        assert_eq!(material, before);
    }

    #[test]
    fn v13_normal_convention_distinguishes_tangent_and_model_space() {
        for model_space in [false, true] {
            let mut source = fixture(NifAlphaMode::Opaque, false, false, false);
            source.shader_flags_1 = if model_space { 1 << 12 } else { 0 };
            source.textures.push(NifTextureSlot {
                slot: 1,
                semantic: NifTextureSemantic::Normal,
                path: "textures/normal_n.dds".into(),
                required: false,
            });
            let document = publish_specular_fixture(source);
            assert_eq!(
                document["materials"][0]["extras"]["openSkyrim"]["normalConvention"],
                if model_space {
                    "model_space"
                } else {
                    "directx"
                }
            );
        }
    }

    #[test]
    fn v10_normal_alpha_mask_is_linear_shared_and_excludes_model_space_normals() {
        for model_space in [false, true] {
            let mut source = fixture(NifAlphaMode::Opaque, false, false, false);
            source.shader_flags_1 = 1 | if model_space { 1 << 12 } else { 0 };
            source.specular_strength = 0.8;
            source.textures.push(NifTextureSlot {
                slot: 1,
                semantic: NifTextureSemantic::Normal,
                path: "textures/mask_n.dds".into(),
                required: false,
            });
            let document = publish_specular_fixture(source);
            let material = &document["materials"][0];
            let specular = &material["extensions"]["KHR_materials_specular"];
            // Model space has no `_s` here, so nothing masks it in and its specular is zero.
            let expected = if model_space { 0.0 } else { 0.8 };
            assert!((specular["specularFactor"].as_f64().unwrap() - expected).abs() < 1e-6);
            if model_space {
                assert!(specular.get("specularTexture").is_none());
                assert!(
                    material
                        .pointer("/extras/openSkyrim/specularMask")
                        .is_none()
                );
            } else {
                assert_eq!(
                    specular["specularTexture"]["index"],
                    material["normalTexture"]["index"]
                );
                assert_eq!(
                    material["extras"]["openSkyrim"]["specularMask"],
                    "normal_alpha"
                );
                assert_eq!(document["images"][0]["uri"], "../textures/mask_n.ktx2");
                assert_eq!(document["textures"].as_array().unwrap().len(), 1);
            }
        }
    }

    #[test]
    fn v15_shared_images_keep_all_wrap_modes_and_per_use_uv_transforms() {
        let mut shapes = Vec::new();
        for mode in 0..4 {
            let mut material = fixture(NifAlphaMode::Opaque, false, true, false);
            material.texture_clamp_mode = mode;
            material.uv_offset = [mode as f32 * 0.125, -0.25];
            material.uv_scale = [2.0, -0.5];
            for (slot, semantic) in [
                (0, NifTextureSemantic::Diffuse),
                (1, NifTextureSemantic::Normal),
                (2, NifTextureSemantic::Glow),
            ] {
                material.textures.push(NifTextureSlot {
                    slot,
                    semantic,
                    path: "textures/shared.dds".into(),
                    required: false,
                });
            }
            shapes.push(shape(mode as u32, material));
        }
        let mut document = gltf(4);
        publish_gltf_materials(
            &mut document,
            &shapes,
            &[0, 1, 2, 3],
            Path::new("meshes/test.glb"),
        )
        .unwrap();
        let images = document["images"].as_array().unwrap();
        assert_eq!(images.len(), 8, "four samplers times two transfer views");
        let uris: BTreeSet<_> = images
            .iter()
            .map(|image| image["uri"].as_str().unwrap())
            .collect();
        assert_eq!(uris.len(), 8);
        for mode in 0..4 {
            let material = &document["materials"][mode];
            for pointer in [
                "/pbrMetallicRoughness/baseColorTexture",
                "/normalTexture",
                "/emissiveTexture",
                "/extensions/KHR_materials_specular/specularTexture",
            ] {
                let info = material.pointer(pointer).unwrap();
                assert_eq!(
                    info["extensions"]["KHR_texture_transform"]["scale"],
                    serde_json::json!([2.0, -0.5])
                );
                assert_eq!(
                    info["extensions"]["KHR_texture_transform"]["offset"],
                    serde_json::json!([mode as f32 * 0.125, -0.25])
                );
                let texture = &document["textures"][info["index"].as_u64().unwrap() as usize];
                let sampler = &document["samplers"][texture["sampler"].as_u64().unwrap() as usize];
                assert_eq!(sampler["wrapS"], if mode & 2 == 0 { 33071 } else { 10497 });
                assert_eq!(sampler["wrapT"], if mode & 1 == 0 { 33071 } else { 10497 });
            }
        }
    }

    #[test]
    fn v15_rejects_invalid_uv_values_before_publication() {
        let mut material = fixture(NifAlphaMode::Opaque, false, false, false);
        material.uv_scale[0] = f32::INFINITY;
        assert!(validate_material(Path::new("bad.nif"), 1, None, &material).is_err());
        material.uv_scale = [1.0; 2];
        material.texture_clamp_mode = 4;
        assert!(validate_material(Path::new("bad.nif"), 1, None, &material).is_err());
    }

    #[test]
    fn validates_six_canonical_material_fixtures() {
        for material in [
            fixture(NifAlphaMode::Opaque, false, false, false),
            fixture(NifAlphaMode::Cutout, false, false, false),
            fixture(NifAlphaMode::Blend, false, false, false),
            fixture(NifAlphaMode::Opaque, true, false, false),
            fixture(NifAlphaMode::Opaque, false, true, false),
            fixture(NifAlphaMode::Opaque, false, false, true),
        ] {
            validate_material(Path::new("fixture.nif"), 3, Some("fixture"), &material).unwrap();
        }
    }

    #[test]
    fn rejects_non_finite_and_impossible_values_with_context() {
        let mut material = fixture(NifAlphaMode::Opaque, false, false, false);
        material.glossiness = f32::NAN;
        let error = validate_material(Path::new("broken.nif"), 7, Some("bad shape"), &material)
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("broken.nif"));
        assert!(message.contains("shape block 7"));
        assert!(message.contains("non-finite"));
    }

    #[test]
    fn v6_source_emission_rejects_nonfinite_before_projection() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let error = source_emission_multiple(
                Path::new("invalid-emission.nif"),
                3,
                Some("glow"),
                4,
                value,
            )
            .unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("invalid-emission.nif"));
            assert!(message.contains("shape block 3"));
            assert!(message.contains("shader block 4"));
            assert!(message.contains("non-finite"));
        }
        for (value, expected) in [(-2.0, -2.0), (0.0, 0.0), (0.25, 0.25), (8.0, 8.0)] {
            assert_eq!(
                source_emission_multiple(Path::new("fixture.nif"), 3, None, 4, value).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn v6_emission_contract_rejects_invalid_channels_and_negative_effect_strength() {
        for channel in 0..3 {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let mut material = fixture(NifAlphaMode::Opaque, true, false, false);
                material.emissive_color[channel] = value;
                assert!(validate_material(Path::new("fixture.nif"), 3, None, &material).is_err());
            }
        }
        let mut material = fixture(NifAlphaMode::Opaque, true, false, false);
        material.emissive_multiple = -1.0;
        material.shader_family = NifShaderFamily::Effect;
        assert!(validate_material(Path::new("fixture.nif"), 3, None, &material).is_err());
    }

    #[test]
    fn clamps_only_small_serialization_overshoot_in_alpha() {
        assert_eq!(
            normalize_alpha(Path::new("fixture.nif"), 3, None, 4, -0.001).unwrap(),
            0.0
        );
        assert_eq!(
            normalize_alpha(Path::new("fixture.nif"), 3, None, 4, -0.25).unwrap(),
            0.0
        );
        assert_eq!(
            normalize_alpha(Path::new("fixture.nif"), 3, None, 4, 1.25).unwrap(),
            1.0
        );
    }

    #[test]
    fn assigns_texture_semantics_from_shader_contract_not_names() {
        let set = BSShaderTextureSet {
            textures: vec![
                Some("odd/a.dds".to_owned()),
                Some("odd/b.dds".to_owned()),
                None,
                None,
                Some("odd/c.dds".to_owned()),
                Some("odd/d.dds".to_owned()),
            ],
            diffuse: None,
            normal: None,
            glow: None,
            height_or_detail: None,
            environment: None,
            environment_mask: None,
            inner_layer: None,
            specular: None,
        };
        let slots = lighting_texture_slots(&set, LightingShaderType::EnvironmentMap, 0, 0).unwrap();
        assert_eq!(slots[0].semantic, NifTextureSemantic::Diffuse);
        assert_eq!(slots[1].semantic, NifTextureSemantic::Normal);
        assert_eq!(slots[2].semantic, NifTextureSemantic::EnvironmentCube);
        assert_eq!(slots[3].semantic, NifTextureSemantic::EnvironmentMask);
    }

    #[test]
    fn maps_legacy_authoring_references_to_runtime_texture_paths() {
        let slot = texture_slot(
            0,
            NifTextureSemantic::Diffuse,
            "textures/current/source/clothes/gloves.tga",
            true,
        )
        .unwrap();
        assert_eq!(slot.path, "textures/current/source/clothes/gloves.dds");
        assert_eq!(
            texture_slot(0, NifTextureSemantic::Diffuse, "textures/grey.bmp", true,)
                .unwrap()
                .path,
            "textures/grey.dds"
        );
        assert_eq!(
            texture_slot(
                0,
                NifTextureSemantic::Diffuse,
                "textures/program files (x86)/steam/steamapps/common/cc-s/data/textures/creationclub/cbhsse001/glass/gaunts2.dds",
                true,
            )
            .unwrap()
            .path,
            "textures/creationclub/cbhsse001/glass/gaunts2.dds"
        );
    }

    #[test]
    fn v6_rejects_overflowing_emission_with_context() {
        let mut material = fixture(NifAlphaMode::Opaque, false, false, false);
        material.emissive_color = [f32::MAX; 3];
        material.emissive_multiple = 2.0;
        let error = validate_material(
            Path::new("invalid-emission.nif"),
            10,
            Some("emitter"),
            &material,
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("invalid-emission.nif"));
        assert!(message.contains("shape block 10"));
        assert!(message.contains("emissive energy"));
    }

    #[test]
    fn v8_signed_static_multiplier_survives_source_metadata() {
        for color in [[1.0, 0.5, 0.25], [-1.0, 0.5, 0.25], [0.0; 3]] {
            for glow in [false, true] {
                let mut source = fixture(NifAlphaMode::Opaque, true, false, false);
                source.emissive_color = color;
                source.emissive_multiple = -2.0;
                validate_material(Path::new("signed-multiplier.nif"), 3, None, &source).unwrap();
                let document = publish_emission_fixture(color, -2.0, glow);
                let material = &document["materials"][0];
                let metadata = &material["extras"]["openSkyrim"]["sourceEmission"];
                assert_eq!(metadata["color"], serde_json::json!(color));
                assert_eq!(metadata["multiple"], -2.0);
                if let Some(factor) = material.get("emissiveFactor") {
                    let strength = material["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"].as_f64().unwrap_or(1.0);
                    assert!(
                        factor
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|channel| channel.as_f64().unwrap() * strength == 0.0)
                    );
                }
            }
        }
    }

    #[test]
    fn v8_signed_tint_keeps_source_and_clamps_only_gltf_negative_channels() {
        for (color, multiple) in [
            ([1.0, -0.25, 0.5], 0.5),
            ([-0.67, -1.0, 1.0], 0.2),
            ([-0.1; 3], 1.5),
            ([-1.12e38, 1.0e-39, -1.12e38], 0.0),
        ] {
            let mut source = fixture(NifAlphaMode::Opaque, false, false, false);
            source.emissive_color = color;
            source.emissive_multiple = multiple;
            validate_material(Path::new("signed-tint.nif"), 10, None, &source).unwrap();
            assert_eq!(source.emissive_color, color);
            for glow in [false, true] {
                let document = publish_emission_fixture(color, multiple, glow);
                let material = &document["materials"][0];
                let saved = &material["extras"]["openSkyrim"]["sourceEmission"];
                assert_eq!(saved["color"], serde_json::json!(color));
                assert_eq!(saved["multiple"], serde_json::json!(multiple));
                assert_eq!(
                    saved["representation"],
                    "negative_channels_clamped_for_gltf"
                );
                let strength =
                    material["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"]
                        .as_f64()
                        .unwrap_or(1.0);
                for (channel, value) in color.into_iter().enumerate() {
                    let factor = material["emissiveFactor"][channel].as_f64().unwrap_or(0.0);
                    assert!((0.0..=1.0).contains(&factor));
                    assert!(
                        (factor * strength - f64::from(value.max(0.0) * multiple)).abs() < 1e-6
                    );
                }
            }
        }
    }

    fn publish_emission_fixture(color: [f32; 3], multiple: f32, glow: bool) -> serde_json::Value {
        let mut material = fixture(NifAlphaMode::Opaque, false, false, false);
        material.emissive_color = color;
        material.emissive_multiple = multiple;
        if glow {
            material.textures.push(NifTextureSlot {
                slot: 2,
                semantic: NifTextureSemantic::Glow,
                path: "textures/glow.dds".to_owned(),
                required: false,
            });
        }
        let mut document = gltf(1);
        publish_gltf_materials(
            &mut document,
            &[shape(10, material)],
            &[10],
            Path::new("meshes/test.glb"),
        )
        .unwrap();
        document
    }

    #[test]
    fn v6_emission_preserves_zero_dim_hdr_and_black_glow_energy() {
        for (color, multiple) in [
            ([1.0, 0.5, 0.25], 0.0),
            ([1.0, 0.5, 0.25], 0.25),
            ([4.0, 2.0, 0.5], 3.0),
            ([4.0, 2.0, 0.5], 0.125),
            ([0.0; 3], 2.0),
        ] {
            for glow in [false, true] {
                let document = publish_emission_fixture(color, multiple, glow);
                let material = &document["materials"][0];
                let strength =
                    material["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"]
                        .as_f64()
                        .unwrap_or(1.0);
                for (channel, source) in color.into_iter().enumerate() {
                    let factor = material["emissiveFactor"][channel].as_f64().unwrap_or(0.0);
                    assert!((0.0..=1.0).contains(&factor));
                    assert!(
                        (factor * strength - f64::from(source * multiple)).abs() < 1e-6,
                        "color={color:?}, multiple={multiple}, glow={glow}, channel={channel}"
                    );
                }
                assert_eq!(material.get("emissiveTexture").is_some(), glow);
            }
        }
    }

    #[test]
    fn slot_six_is_the_facegen_tint_only_on_face_tint_heads() {
        let mut textures = vec![None; 7];
        textures[0] = Some("textures/actors/character/male/malehead.dds".to_owned());
        textures[3] = Some("textures/actors/character/male/blankdetailmap.dds".to_owned());
        textures[6] = Some(
            "textures/actors/character/facegendata/facetint/skyrim.esm/00013475.dds".to_owned(),
        );
        let set = BSShaderTextureSet {
            textures,
            diffuse: None,
            normal: None,
            glow: None,
            height_or_detail: None,
            environment: None,
            environment_mask: None,
            inner_layer: None,
            specular: None,
        };
        for (shader, slot_3, slot_6) in [
            (
                LightingShaderType::FaceTint,
                NifTextureSemantic::Detail,
                NifTextureSemantic::FacegenTint,
            ),
            (
                LightingShaderType::MultiLayerParallax,
                NifTextureSemantic::Height,
                NifTextureSemantic::InnerLayer,
            ),
            (
                LightingShaderType::SkinTint,
                NifTextureSemantic::Detail,
                NifTextureSemantic::InnerLayer,
            ),
            (
                LightingShaderType::Default,
                NifTextureSemantic::Detail,
                NifTextureSemantic::InnerLayer,
            ),
        ] {
            let slots = lighting_texture_slots(&set, shader, 0, 0).unwrap();
            let semantic = |index| {
                slots
                    .iter()
                    .find(|slot| slot.slot == index)
                    .map(|slot| slot.semantic)
            };
            assert_eq!(semantic(3), Some(slot_3), "{shader:?} slot 3");
            assert_eq!(semantic(6), Some(slot_6), "{shader:?} slot 6");
        }

        let mut material = fixture(NifAlphaMode::Opaque, false, false, false);
        material.lighting_shader_type = Some(LightingShaderType::FaceTint);
        material.textures = lighting_texture_slots(&set, LightingShaderType::FaceTint, 0, 0)
            .unwrap()
            .into_iter()
            .filter(|slot| slot.slot != 0)
            .collect();
        let mut document = gltf(1);
        publish_gltf_materials(
            &mut document,
            &[shape(10, material)],
            &[10],
            Path::new("meshes/actors/character/facegendata/facegeom/skyrim.esm/00013475.glb"),
        )
        .unwrap();
        let slots = document["materials"][0]["extensions"]["OPEN_SKYRIM_material"]["textureSlots"]
            .as_array()
            .unwrap();
        let tint = slots.iter().find(|slot| slot["slot"] == 6).unwrap();
        assert_eq!(tint["semantic"], "facegen_tint");
        assert_eq!(tint["colorSpace"], "srgb");
        let image = document["textures"][tint["texture"].as_u64().unwrap() as usize]["source"]
            .as_u64()
            .unwrap() as usize;
        assert!(
            document["images"][image]["uri"]
                .as_str()
                .unwrap()
                .ends_with("facetint/skyrim.esm/00013475.opensky-srgb.ktx2")
        );
    }

    /// A `BSLightingShaderProperty` block of `shader_type` with `tail` as its type-specific
    /// fields, laid out as Skyrim SE stores it.
    fn lighting_property(shader_type: u32, tail: &[f32]) -> BSLightingShaderProperty {
        use nom_derive::Parse;
        let mut words = vec![shader_type, u32::MAX, 0, u32::MAX, 0x8244_030b, 0x0000_8021];
        words.extend([0.0f32, 0.0, 1.0, 1.0].map(f32::to_bits));
        words.push(7);
        words.extend([0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits));
        words.push(3);
        words.extend([1.0f32, 0.0, 70.0, 1.0, 1.0, 1.0, 1.8, 0.3, 2.0].map(f32::to_bits));
        words.extend(tail.iter().map(|value| value.to_bits()));
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let (rest, property) = BSLightingShaderProperty::parse(&bytes).unwrap();
        assert!(rest.is_empty());
        property
    }

    #[test]
    fn only_skin_and_hair_tint_shaders_carry_a_tint_colour() {
        let alvor = [0.439_215_7, 0.462_745_1, 0.345_098_05];
        for (shader_type, tail, expected) in [
            (6, &alvor[..], Some(alvor)),
            (5, &[0.8, 0.6, 0.5][..], Some([0.8, 0.6, 0.5])),
            (6, &[f32::NAN, 0.5, 0.5][..], None),
            (1, &[0.5][..], None),
            (16, &[1.3, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0][..], None),
            (4, &[][..], None),
            (0, &[][..], None),
        ] {
            let property = lighting_property(shader_type, tail);
            let shader = LightingShaderType::try_from(shader_type).unwrap();
            assert_eq!(
                lighting_tint_color(shader, &property),
                expected,
                "type {shader_type}"
            );
        }
    }

    #[test]
    fn tint_colour_is_published_in_the_skyrim_extension() {
        let alvor = [0.439_215_7, 0.462_745_1, 0.345_098_05];
        let mut hair = fixture(NifAlphaMode::Cutout, false, false, false);
        hair.lighting_shader_type = Some(LightingShaderType::HairTint);
        hair.tint_color = Some(alvor);
        let mut plain = fixture(NifAlphaMode::Cutout, false, false, false);
        plain.lighting_shader_type = Some(LightingShaderType::HairTint);
        let mut document = gltf(2);
        publish_gltf_materials(
            &mut document,
            &[shape(10, hair), shape(11, plain)],
            &[10, 11],
            Path::new("meshes/actors/character/facegendata/facegeom/skyrim.esm/00013475.glb"),
        )
        .unwrap();
        let extension = &document["materials"][0]["extensions"]["OPEN_SKYRIM_material"];
        assert_eq!(extension["lightingShaderType"], "hair_tint");
        assert_eq!(extension["tintColor"], serde_json::json!(alvor));
        assert_eq!(extension["textureSlots"], serde_json::json!([]));
        assert!(
            document["materials"][1]["extensions"]
                .get("OPEN_SKYRIM_material")
                .is_none(),
            "a material with no tint, slots or alpha flags publishes no extension"
        );
    }

    #[test]
    fn v6_own_emit_does_not_enable_slot_two_glow() {
        let set = BSShaderTextureSet {
            textures: vec![None, None, Some("textures/atlas.dds".to_owned())],
            diffuse: None,
            normal: None,
            glow: None,
            height_or_detail: None,
            environment: None,
            environment_mask: None,
            inner_layer: None,
            specular: None,
        };
        for (shader, flags_1, flags_2, expected) in [
            (
                LightingShaderType::Default,
                SLSF1_OWN_EMIT,
                0,
                NifTextureSemantic::Unclassified,
            ),
            (LightingShaderType::Glow, 0, 0, NifTextureSemantic::Glow),
            (
                LightingShaderType::Default,
                0,
                SLSF2_GLOW_MAP,
                NifTextureSemantic::Glow,
            ),
        ] {
            let slots = lighting_texture_slots(&set, shader, flags_1, flags_2).unwrap();
            assert_eq!(slots[0].semantic, expected);
            let mut material = fixture(NifAlphaMode::Opaque, false, false, false);
            material.textures = slots;
            let mut document = gltf(1);
            publish_gltf_materials(
                &mut document,
                &[shape(10, material)],
                &[10],
                Path::new("meshes/test.glb"),
            )
            .unwrap();
            assert_eq!(
                document["materials"][0].get("emissiveTexture").is_some(),
                expected == NifTextureSemantic::Glow
            );
        }
    }

    #[test]
    fn publishes_core_pbr_alpha_emissive_and_double_sided_contract() {
        let mut material = fixture(NifAlphaMode::Cutout, true, true, false);
        material.base_color = [0.8, 0.7, 0.6, 0.4];
        material.alpha = 0.4;
        material.textures = vec![
            NifTextureSlot {
                slot: 0,
                semantic: NifTextureSemantic::Diffuse,
                path: "textures/architecture/wall.dds".to_owned(),
                required: true,
            },
            NifTextureSlot {
                slot: 1,
                semantic: NifTextureSemantic::Normal,
                path: "textures/architecture/wall_n.dds".to_owned(),
                required: false,
            },
            NifTextureSlot {
                slot: 2,
                semantic: NifTextureSemantic::Glow,
                path: "textures/architecture/wall_g.dds".to_owned(),
                required: false,
            },
            NifTextureSlot {
                slot: 7,
                semantic: NifTextureSemantic::Specular,
                path: "textures/architecture/wall_s.dds".to_owned(),
                required: false,
            },
        ];
        let contract = vec![shape(10, material)];
        let mut document = gltf(1);

        publish_gltf_materials(
            &mut document,
            &contract,
            &[10],
            Path::new("assets/meshes/architecture/wall.glb"),
        )
        .unwrap();

        let published = &document["materials"][0];
        assert_eq!(published["alphaMode"], "MASK");
        let alpha_cutoff = published["alphaCutoff"].as_f64().unwrap();
        assert!((alpha_cutoff - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(published["doubleSided"], true);
        let base_color = published["pbrMetallicRoughness"]["baseColorFactor"]
            .as_array()
            .unwrap();
        for (actual, expected) in base_color.iter().zip([0.8, 0.7, 0.6, 0.4]) {
            assert!((actual.as_f64().unwrap() - expected).abs() < 1e-6);
        }
        assert_eq!(published["pbrMetallicRoughness"]["metallicFactor"], 0.0);
        let roughness = published["pbrMetallicRoughness"]["roughnessFactor"]
            .as_f64()
            .unwrap();
        assert!((roughness - 0.492_479_06).abs() < 1e-6);
        assert_eq!(
            published["emissiveFactor"],
            serde_json::json!([1.0, 0.5, 0.25])
        );
        assert_eq!(
            published["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"],
            2.0
        );
        assert_eq!(
            published["extensions"]["KHR_materials_specular"]["specularColorTexture"]["index"],
            3
        );
        assert_eq!(document["meshes"][0]["primitives"][0]["material"], 0);
        assert_eq!(
            document["images"][0]["uri"],
            "../../textures/architecture/wall.opensky-srgb.ktx2"
        );
        assert_eq!(
            document["images"][1]["uri"],
            "../../textures/architecture/wall_n.ktx2"
        );
        assert!(
            !document["extensionsUsed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "KHR_materials_pbrSpecularGlossiness")
        );
        assert!(document.get("extensionsRequired").is_none());
    }

    #[test]
    fn preserves_shape_association_and_skyrim_only_texture_semantics() {
        let first = fixture(NifAlphaMode::Opaque, false, false, false);
        let mut second = fixture(NifAlphaMode::Blend, false, false, true);
        second.textures = vec![
            NifTextureSlot {
                slot: 3,
                semantic: NifTextureSemantic::Detail,
                path: "textures/skyrimhd/build/pc/data/textures/detail.dds".to_owned(),
                required: false,
            },
            NifTextureSlot {
                slot: 4,
                semantic: NifTextureSemantic::EnvironmentCube,
                path: "textures/cubemaps/ore_e.dds".to_owned(),
                required: true,
            },
        ];
        let contract = vec![shape(10, first), shape(20, second)];
        let mut document = gltf(2);

        publish_gltf_materials(
            &mut document,
            &contract,
            &[20, 10],
            Path::new("assets/meshes/landscape/trees/driftwood.glb"),
        )
        .unwrap();

        assert_eq!(document["meshes"][0]["primitives"][0]["material"], 1);
        assert_eq!(document["meshes"][1]["primitives"][0]["material"], 0);
        assert!(document["materials"][0].get("doubleSided").is_none());
        let slots = document["materials"][1]["extensions"]["OPEN_SKYRIM_material"]["textureSlots"]
            .as_array()
            .unwrap();
        assert_eq!(slots[0]["colorSpace"], "srgb");
        assert_eq!(slots[1]["colorSpace"], "linear");
        assert_eq!(
            document["images"][0]["uri"],
            "../../../textures/detail.opensky-srgb.ktx2"
        );
        assert_eq!(
            document["images"][1]["uri"],
            "../../../textures/cubemaps/ore_e.ktx2"
        );
    }

    #[test]
    fn publishes_exclusions_without_reusing_an_exporter_material() {
        let contract = vec![NifShapeMaterial {
            shape_block: 42,
            shape_name: Some("collision-only".to_owned()),
            shader_property_block: None,
            alpha_property_block: None,
            disposition: NifMaterialDisposition::Excluded {
                reason: "shape has no shader property".to_owned(),
            },
        }];
        let mut document = gltf(1);
        document["meshes"][0]["primitives"][0]["material"] = serde_json::json!(7);

        publish_gltf_materials(
            &mut document,
            &contract,
            &[42],
            Path::new("assets/meshes/excluded.glb"),
        )
        .unwrap();

        let primitive = &document["meshes"][0]["primitives"][0];
        assert_eq!(primitive["material"], 0);
        assert_eq!(primitive["extras"]["openSkyrim"]["shapeBlock"], 42);
        assert_eq!(document["materials"][0]["alphaMode"], "MASK");
        assert_eq!(document["materials"][0]["alphaCutoff"], 1.0);
        assert_eq!(
            document["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"],
            serde_json::json!([0.0, 0.0, 0.0, 0.0])
        );
        assert_eq!(
            document["materials"][0]["extras"]["openSkyrim"]["nonRenderingExclusion"],
            true
        );
    }

    #[test]
    fn publishes_distinct_texture_objects_for_srgb_and_linear_uses() {
        let mut material = fixture(NifAlphaMode::Opaque, false, false, false);
        material.textures = vec![
            NifTextureSlot {
                slot: 0,
                semantic: NifTextureSemantic::Diffuse,
                path: "textures/effects/shared.dds".to_owned(),
                required: true,
            },
            NifTextureSlot {
                slot: 1,
                semantic: NifTextureSemantic::Normal,
                path: "textures/effects/shared.dds".to_owned(),
                required: false,
            },
        ];
        let mut document = gltf(1);

        publish_gltf_materials(
            &mut document,
            &[shape(10, material)],
            &[10],
            Path::new("assets/meshes/effects/shared.glb"),
        )
        .unwrap();

        let published = &document["materials"][0];
        assert_eq!(
            published["pbrMetallicRoughness"]["baseColorTexture"]["index"],
            0
        );
        assert_eq!(published["normalTexture"]["index"], 1);
        assert_eq!(document["textures"].as_array().unwrap().len(), 2);
        assert_eq!(document["images"].as_array().unwrap().len(), 2);
        assert_eq!(
            document["images"][0]["uri"],
            "../../textures/effects/shared.opensky-srgb.ktx2"
        );
        assert_eq!(
            document["images"][1]["uri"],
            "../../textures/effects/shared.ktx2"
        );
    }

    #[test]
    fn v14_shader_alpha_flags_do_not_enable_blending_without_an_alpha_property() {
        let mut material = fixture(NifAlphaMode::Opaque, false, false, false);
        material.alpha = 0.25;
        material.base_color[3] = 0.25;
        assert_eq!(
            publish_specular_fixture(material)["materials"][0]["alphaMode"],
            "OPAQUE"
        );
        assert_eq!(
            alpha_contract(None, SLSF1_VERTEX_ALPHA, 0).0,
            NifAlphaMode::Opaque
        );
        assert_eq!(
            alpha_contract(None, 0, SLSF2_PREMULTIPLIED_ALPHA).0,
            NifAlphaMode::Opaque
        );
    }
    fn quad_shape() -> dummy_content::nif::StaticShape<'static> {
        dummy_content::nif::StaticShape {
            name: "GeneratedFlame",
            positions: &[
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [1.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
            ],
            normals: &[[0.0, 0.0, 1.0]; 4],
            uvs: &[[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
            indices: &[[0, 1, 2], [0, 2, 3]],
            diffuse: "textures/effects/fxfireatlas04.dds",
            normal_texture: "textures/generated_normal.dds",
        }
    }

    /// Converts a fixture NIF and parses the GLB's JSON chunk back.
    fn convert_fixture(bytes: &[u8], directory: &Path) -> serde_json::Value {
        let nif_path = directory.join("animated.nif");
        std::fs::write(&nif_path, bytes).unwrap();
        let glb_path = directory.join("animated.glb");
        crate::mesh::MeshConverter::convert_nif_to_glb(&nif_path, &glb_path).unwrap();
        crate::mesh::glb_json_from_bytes(&std::fs::read(&glb_path).unwrap()).unwrap()
    }

    fn close(left: f64, right: f32) -> bool {
        (left - f64::from(right)).abs() < 1.0e-4
    }

    /// The hearth-flame card of `fireplacewood01burning.nif`: a quadratic
    /// V-offset controller, `flags` 0x48 (cycle), frequency 1, stop 5.6667.
    fn hearth_flame_controller<'a>(
        keys: &'a [[f32; 4]],
    ) -> dummy_content::nif::FloatController<'a> {
        dummy_content::nif::FloatController {
            flags: 0x48,
            frequency: 1.0,
            phase: 0.0,
            start_time: 0.0,
            stop_time: 5.6667,
            variable: 8,
            key_type: 2,
            keys,
        }
    }

    #[test]
    fn publishes_a_quadratic_controller_as_a_material_animation_channel() {
        let directory = tempfile::tempdir().unwrap();
        let keys = [[0.0f32, 0.0, 0.0, 0.0], [5.6667, 1.0, 0.0, 0.0]];
        let bytes = dummy_content::nif::effect_shape_with_controllers(
            &quad_shape(),
            &[hearth_flame_controller(&keys)],
        )
        .unwrap();
        let document = convert_fixture(&bytes, directory.path());

        let channel = &document["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"]["channels"]
            [0];
        assert_eq!(channel["variable"], "vOffset");
        assert_eq!(channel["interpolation"], "QUADRATIC");
        assert_eq!(channel["loop"], "cycle");
        assert!(close(channel["frequency"].as_f64().unwrap(), 1.0));
        assert!(close(channel["phase"].as_f64().unwrap(), 0.0));
        assert!(close(channel["start"].as_f64().unwrap(), 0.0));
        assert!(close(channel["stop"].as_f64().unwrap(), 5.6667));
        assert!(close(channel["times"][0].as_f64().unwrap(), 0.0));
        assert!(close(channel["times"][1].as_f64().unwrap(), 5.6667));
        assert!(close(channel["values"][0].as_f64().unwrap(), 0.0));
        assert!(close(channel["values"][1].as_f64().unwrap(), 1.0));
        assert_eq!(channel["tangents"].as_array().unwrap().len(), 2);
        // The extension is listed as used but never required: a consumer that
        // does not play it still loads the model.
        let used = document["extensionsUsed"].as_array().unwrap();
        assert!(
            used.iter()
                .any(|value| value == "OPEN_SKYRIM_material_animation"),
            "extensionsUsed does not list the animation extension: {used:?}"
        );
        let required = document["extensionsRequired"]
            .as_array()
            .map(|required| {
                required
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert!(
            !required.contains(&"OPEN_SKYRIM_material_animation"),
            "the animation extension must not be required: {required:?}"
        );
    }

    #[test]
    fn publishes_an_emissive_colour_controller_and_keeps_walking_the_chain() {
        let directory = tempfile::tempdir().unwrap();
        // The hearth Glow card's keys (fireplacewood01burning.nif): grey 0.196, 0.392, 0.196,
        // quadratic, with distinct tangents on the first key so their order is checked.
        let color_keys = [
            [0.0f32, 0.196, 0.196, 0.196, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            [1.9667, 0.392, 0.392, 0.392, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [4.6333, 0.196, 0.196, 0.196, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ];
        let float_keys = [[0.0f32, 0.0, 0.0, 0.0], [5.6667, 1.0, 0.0, 0.0]];
        let bytes = dummy_content::nif::effect_shape_with_color_controller(
            &quad_shape(),
            &dummy_content::nif::ColorController {
                flags: 0x48,
                frequency: 1.0,
                phase: 0.0,
                start_time: 0.0,
                stop_time: 4.6333,
                key_type: 2,
                keys: &color_keys,
            },
            &[hearth_flame_controller(&float_keys)],
        )
        .unwrap();
        let document = convert_fixture(&bytes, directory.path());

        let channels =
            document["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"]["channels"]
                .as_array()
                .unwrap();
        assert_eq!(channels.len(), 2, "{channels:?}");
        let color = &channels[0];
        assert_eq!(color["variable"], "emissiveColor");
        assert_eq!(color["components"], 3);
        assert_eq!(color["times"].as_array().unwrap().len(), 3);
        let values: Vec<f64> = color["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_f64().unwrap())
            .collect();
        assert_eq!(values.len(), 9);
        assert!(close(values[3], 0.392) && close(values[5], 0.392));
        // Per component, [outgoing, incoming] = [Backward, Forward].
        let first_red = &color["tangents"][0];
        assert!(close(first_red[0].as_f64().unwrap(), 4.0));
        assert!(close(first_red[1].as_f64().unwrap(), 1.0));
        let first_blue = &color["tangents"][2];
        assert!(close(first_blue[0].as_f64().unwrap(), 6.0));
        assert!(close(first_blue[1].as_f64().unwrap(), 3.0));
        // The float controller chained after it is still published, and a float channel
        // carries no `components`.
        assert_eq!(channels[1]["variable"], "vOffset");
        assert!(channels[1].get("components").is_none());
    }

    #[test]
    fn maps_a_linear_u_scale_channel_and_skips_controllers_it_cannot_read() {
        let directory = tempfile::tempdir().unwrap();
        let linear_keys = [[0.0f32, 0.0, 0.0, 0.0], [2.0, 0.5, 0.0, 0.0]];
        let bytes = dummy_content::nif::effect_shape_with_controllers(
            &quad_shape(),
            &[
                dummy_content::nif::FloatController {
                    flags: 0x40,
                    frequency: 1.0,
                    phase: 0.0,
                    start_time: 0.0,
                    stop_time: 2.0,
                    variable: 7,
                    key_type: 1,
                    keys: &linear_keys,
                },
                // `Unknown11`: outside the shader inputs the contract names.
                dummy_content::nif::FloatController {
                    variable: 11,
                    ..hearth_flame_controller(&linear_keys)
                },
                // Value 10 is not in `EffectShaderControlledVariable` at all, so
                // the block does not even parse: the conversion must survive it.
                dummy_content::nif::FloatController {
                    variable: 10,
                    ..hearth_flame_controller(&linear_keys)
                },
            ],
        )
        .unwrap();
        let nif_path = directory.path().join("animated.nif");
        std::fs::write(&nif_path, &bytes).unwrap();
        let diagnostics = crate::mesh::MeshConverter::inspect_nif(&nif_path).unwrap();
        let document = convert_fixture(&bytes, directory.path());

        let channels =
            document["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"]["channels"]
                .as_array()
                .unwrap();
        assert_eq!(
            channels.len(),
            1,
            "one readable channel out of three: {channels:?}"
        );
        let channel = &channels[0];
        assert_eq!(channel["variable"], "uScale");
        assert_eq!(channel["interpolation"], "LINEAR");
        assert!(close(channel["times"][1].as_f64().unwrap(), 2.0));
        assert!(close(channel["values"][1].as_f64().unwrap(), 0.5));
        // A linear key carries no tangents, so the array is omitted entirely.
        assert!(channel.get("tangents").is_none());

        assert_eq!(
            diagnostics
                .animation_skipped_channels
                .get("unknown controlled variable"),
            Some(&1)
        );
        // Variable 10 is outside `EffectShaderControlledVariable` entirely, so
        // the whole block fails to parse and falls back to `NifBlock::Unhandled`
        // (the same fallback any other unparsable NIF block takes); reading its
        // controller then reports the generic reason.
        assert_eq!(
            diagnostics
                .animation_skipped_channels
                .get("unsupported controller type"),
            Some(&1)
        );
    }

    #[test]
    fn a_shape_without_controllers_carries_no_animation_extension() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = dummy_content::nif::static_shape(&quad_shape()).unwrap();
        let document = convert_fixture(&bytes, directory.path());

        assert!(
            document["materials"][0]["extensions"]
                .get("OPEN_SKYRIM_material_animation")
                .is_none()
        );
        let used = document["extensionsUsed"].as_array().unwrap();
        assert!(
            used.iter()
                .all(|value| value != "OPEN_SKYRIM_material_animation"),
            "a static material must not claim the animation extension: {used:?}"
        );
    }

    #[test]
    #[ignore = "requires MUDCRAB_HEARTH_FIXTURE with the extracted Skyrim hearth NIF"]
    fn real_hearth_flames_publish_their_v_offset_controllers() {
        // The extracted `meshes/clutter/woodfires/fireplacewood01burning.nif` of a Skyrim install.
        let Some(path) = std::env::var_os("MUDCRAB_HEARTH_FIXTURE")
            .or_else(|| std::env::var_os("OPENSKYRIM_HEARTH_FIXTURE"))
            .map(PathBuf::from)
        else {
            eprintln!("MUDCRAB_HEARTH_FIXTURE is unset; skipping");
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let document = convert_fixture(&std::fs::read(&path).unwrap(), directory.path());

        let mut flames = Vec::new();
        for material in document["materials"].as_array().unwrap() {
            let channels = &material["extensions"]["OPEN_SKYRIM_material_animation"]["channels"];
            for channel in channels.as_array().into_iter().flatten() {
                flames.push(channel.clone());
            }
        }
        assert_eq!(flames.len(), 2, "hearth flame cards: {flames:#?}");
        for channel in &flames {
            assert_eq!(channel["variable"], "vOffset");
            assert_eq!(channel["interpolation"], "QUADRATIC");
            assert_eq!(channel["loop"], "cycle");
            assert!(close(channel["values"][0].as_f64().unwrap(), 0.0));
            assert!(close(channel["values"][1].as_f64().unwrap(), 1.0));
            assert!(close(channel["times"][0].as_f64().unwrap(), 0.0));
            assert!(close(channel["frequency"].as_f64().unwrap(), 1.0));
            // A steady scroll: key 0 leaves with slope 1 and key 1 arrives with slope 1.
            assert!(close(channel["tangents"][0][0].as_f64().unwrap(), 1.0));
            assert!(close(channel["tangents"][1][1].as_f64().unwrap(), 1.0));
        }
        let mut stops = flames
            .iter()
            .map(|channel| channel["stop"].as_f64().unwrap())
            .collect::<Vec<_>>();
        stops.sort_by(|left, right| left.partial_cmp(right).unwrap());
        assert!(
            close(stops[0], 4.2667) && close(stops[1], 5.6667),
            "{stops:?}"
        );
    }

    #[test]
    fn material_controller_chain_longer_than_64_retains_every_channel() {
        let directory = tempfile::tempdir().unwrap();
        let keys = [[0.0, 0.0, 0.0, 0.0], [5.6667, 1.0, 0.0, 0.0]];
        let controllers: Vec<_> = (0..65).map(|_| hearth_flame_controller(&keys)).collect();
        let bytes =
            dummy_content::nif::effect_shape_with_controllers(&quad_shape(), &controllers).unwrap();
        let document = convert_fixture(&bytes, directory.path());
        assert_eq!(
            document["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"]["channels"]
                .as_array()
                .unwrap()
                .len(),
            65
        );
        assert!(
            document
                .pointer("/asset/extras/openSkyrim/animationSkippedChannels")
                .is_none()
        );
    }
}
