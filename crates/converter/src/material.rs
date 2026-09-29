use crate::asset_path::{AssetKind, canonical_asset_path};
use color_eyre::{Result, eyre::ensure};
use project_wormhole_nif::{
    bs::prelude::{BSShaderTextureSet, BSTriShape},
    nif_block::{BSEffectShaderProperty, BSLightingShaderProperty, NiAlphaProperty, NifBlock},
    nif_file::NifFile,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const NULL_BLOCK: u32 = u32::MAX;
const SLSF1_ENVIRONMENT_MAPPING: u32 = 1 << 7;
const SLSF1_VERTEX_ALPHA: u32 = 1 << 3;
const SLSF1_SCREENDOOR_ALPHA_FADE: u32 = 1 << 19;
const SLSF1_OWN_EMIT: u32 = 1 << 22;
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
    Greyscale,
    Unclassified,
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

pub fn build_nif_material_contract(nif: &NifFile, source: &Path) -> Result<Vec<NifShapeMaterial>> {
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
                "name": "OpenSkyrim non-rendering excluded geometry",
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
            primitive["extras"] = serde_json::json!({
                "openSkyrim": {
                    "shapeBlock": shape.shape_block,
                    "materialExclusion": reason
                }
            });
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
    indices: BTreeMap<(String, bool), usize>,
    images: Vec<serde_json::Value>,
    textures: Vec<serde_json::Value>,
}

impl TextureRegistry {
    fn texture(&mut self, path: &str, glb_output_path: &Path, is_srgb: bool) -> Result<usize> {
        let canonical = canonical_asset_path(path, AssetKind::Texture, "ktx2")?;
        let key = (canonical.clone(), is_srgb);
        if let Some(index) = self.indices.get(&key) {
            return Ok(*index);
        }
        let index = self.textures.len();
        let runtime_path = if is_srgb {
            srgb_texture_alias(&canonical)?
        } else {
            canonical.clone()
        };
        self.images.push(serde_json::json!({
            "uri": runtime_texture_uri(glb_output_path, &runtime_path)?
        }));
        self.textures.push(serde_json::json!({ "source": index }));
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
        "roughnessFactor": (1.0 - (material.glossiness / 100.0).clamp(0.0, 1.0))
    });
    if let Some(slot) = diffuse {
        pbr["baseColorTexture"] = serde_json::json!({
            "index": registry.texture(&slot.path, glb_output_path, true)?,
            "texCoord": 0
        });
    }
    let mut alpha_mode = material.alpha_mode;
    if alpha_mode == NifAlphaMode::Opaque && material.alpha < 1.0 {
        alpha_mode = NifAlphaMode::Blend;
    }
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
        output["normalTexture"] = serde_json::json!({
            "index": registry.texture(&slot.path, glb_output_path, false)?,
            "texCoord": 0,
            "scale": 1.0
        });
    }
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
        specular,
        glb_output_path,
        registry,
        used_extensions,
    )?;
    publish_skyrim_extension(
        &mut output,
        material,
        glb_output_path,
        registry,
        used_extensions,
    )?;
    Ok(output)
}

fn publish_emissive(
    output: &mut serde_json::Value,
    material: &ValidatedNifMaterial,
    glow: Option<&NifTextureSlot>,
    glb_output_path: &Path,
    registry: &mut TextureRegistry,
    used_extensions: &mut BTreeSet<String>,
) -> Result<()> {
    let has_color = material.emissive_color.iter().any(|value| *value > 0.0);
    if glow.is_none() && !has_color {
        return Ok(());
    }
    let color = if has_color {
        material.emissive_color.map(|value| value.clamp(0.0, 1.0))
    } else {
        [1.0; 3]
    };
    output["emissiveFactor"] = serde_json::json!(color);
    if let Some(slot) = glow {
        output["emissiveTexture"] = serde_json::json!({
            "index": registry.texture(&slot.path, glb_output_path, true)?,
            "texCoord": 0
        });
    }
    let strength = material.emissive_multiple.max(1.0);
    if strength > 1.0 {
        output["extensions"]["KHR_materials_emissive_strength"] =
            serde_json::json!({ "emissiveStrength": strength });
        used_extensions.insert("KHR_materials_emissive_strength".to_owned());
    }
    Ok(())
}

fn publish_specular(
    output: &mut serde_json::Value,
    material: &ValidatedNifMaterial,
    specular: Option<&NifTextureSlot>,
    glb_output_path: &Path,
    registry: &mut TextureRegistry,
    used_extensions: &mut BTreeSet<String>,
) -> Result<()> {
    let enabled = material.specular_strength > 0.0 || specular.is_some();
    if !enabled {
        return Ok(());
    }
    let mut extension = serde_json::json!({
        "specularFactor": material.specular_strength.clamp(0.0, 1.0),
        "specularColorFactor": material.specular_color.map(|value| value.clamp(0.0, 1.0))
    });
    if let Some(slot) = specular {
        extension["specularColorTexture"] = serde_json::json!({
            "index": registry.texture(&slot.path, glb_output_path, true)?,
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
                matches!(slot.semantic, NifTextureSemantic::Detail),
            )?,
            "required": slot.required,
            "colorSpace": if matches!(slot.semantic, NifTextureSemantic::Detail) { "srgb" } else { "linear" }
        }));
    }
    let premultiplied_alpha = material.shader_flags_2 & SLSF2_PREMULTIPLIED_ALPHA != 0;
    let screen_door_alpha_fade = material.shader_flags_1 & SLSF1_SCREENDOOR_ALPHA_FADE != 0;
    if slots.is_empty() && !premultiplied_alpha && !screen_door_alpha_fade {
        return Ok(());
    }
    output["extensions"]["OPEN_SKYRIM_material"] = serde_json::json!({
        "shaderFamily": material.shader_family,
        "lightingShaderType": material.lighting_shader_type,
        "shaderFlags1": material.shader_flags_1,
        "shaderFlags2": material.shader_flags_2,
        "premultipliedAlpha": premultiplied_alpha,
        "screenDoorAlphaFade": screen_door_alpha_fade,
        "textureSlots": slots
    });
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

fn build_shape_material(
    nif: &NifFile,
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    shader_reference: u32,
    alpha_reference: u32,
) -> Result<NifMaterialDisposition> {
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
    let alpha = resolve_alpha(nif, source, shape_block, shape_name, alpha_reference)?;
    let material = match shader {
        NifBlock::BSLightingShaderProperty(property) => build_lighting_material(
            nif,
            source,
            shape_block,
            shape_name,
            shader_reference,
            property,
            alpha,
        )?,
        NifBlock::BSEffectShaderProperty(property) => build_effect_material(
            source,
            shape_block,
            shape_name,
            shader_reference,
            property,
            alpha,
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

fn build_lighting_material(
    nif: &NifFile,
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    shader_block: u32,
    property: &BSLightingShaderProperty,
    alpha: Option<(u32, &NiAlphaProperty)>,
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
        // Animated Bethesda materials can ship with a negative base value and
        // drive it positive through a controller. The static runtime has no
        // controller evaluation yet, so use the non-emissive endpoint.
        emissive_multiple: property.emissive_multiple.max(0.0),
        double_sided: flags_2 & SLSF2_DOUBLE_SIDED != 0,
        textures: textures.map(|(_, slots)| slots).unwrap_or_default(),
    })
}

fn build_effect_material(
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    shader_block: u32,
    property: &BSEffectShaderProperty,
    alpha: Option<(u32, &NiAlphaProperty)>,
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
        textures,
    };
    validate_material(source, shape_block, shape_name, &material)?;
    Ok(material)
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
    let environment = matches!(
        shader_type,
        LightingShaderType::EnvironmentMap
            | LightingShaderType::EyeEnvironmentMap
            | LightingShaderType::MultiLayerParallax
    ) || flags_1 & SLSF1_ENVIRONMENT_MAPPING != 0;
    let glow = matches!(shader_type, LightingShaderType::Glow)
        || flags_1 & SLSF1_OWN_EMIT != 0
        || flags_2 & SLSF2_GLOW_MAP != 0;
    let height = matches!(
        shader_type,
        LightingShaderType::Parallax
            | LightingShaderType::ParallaxOcclusion
            | LightingShaderType::MultiLayerParallax
    );
    let mut slots = Vec::new();
    for (index, path) in set.textures.iter().enumerate() {
        let Some(path) = path else { continue };
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
            6 => NifTextureSemantic::InnerLayer,
            7 => NifTextureSemantic::Specular,
            _ => NifTextureSemantic::Unclassified,
        };
        slots.push(texture_slot(
            index,
            semantic,
            path,
            index == 0 || (environment && matches!(index, 4 | 5)),
        )?);
    }
    Ok(slots)
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
    shader_flags_1: u32,
    shader_flags_2: u32,
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
    let shader_requires_blend = shader_flags_1 & (SLSF1_VERTEX_ALPHA | SLSF1_SCREENDOOR_ALPHA_FADE)
        != 0
        || shader_flags_2 & SLSF2_PREMULTIPLIED_ALPHA != 0;
    (
        if shader_requires_blend {
            NifAlphaMode::Blend
        } else {
            NifAlphaMode::Opaque
        },
        None,
        alpha_property_block,
    )
}

fn validate_material(
    source: &Path,
    shape_block: u32,
    shape_name: Option<&str>,
    material: &ValidatedNifMaterial,
) -> Result<()> {
    let values = material
        .base_color
        .into_iter()
        .chain(material.specular_color)
        .chain(material.emissive_color)
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
            && material.emissive_multiple >= 0.0,
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
    Ok(())
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
            shader_flags_1: 0,
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
            textures: Vec::new(),
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
        assert!((roughness - 0.68).abs() < 1e-6);
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
    fn shader_alpha_flags_require_blending_without_an_alpha_property() {
        assert_eq!(
            alpha_contract(None, SLSF1_VERTEX_ALPHA, 0).0,
            NifAlphaMode::Blend
        );
        assert_eq!(
            alpha_contract(None, 0, SLSF2_PREMULTIPLIED_ALPHA).0,
            NifAlphaMode::Blend
        );
    }
}
