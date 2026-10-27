//! Skyrim's skin material: Bevy's PBR lighting fed with model-space (`_msn`) normal maps.
//!
//! Skyrim's actors (bodies, hands, feet, FaceGen heads) and some armour ship their normal map in
//! the shape's own space rather than in tangent space, with the gloss mask in a separate `_s` map
//! (`SLSF1_Model_Space_Normals`). Bevy's `StandardMaterial` decodes tangent-space maps only, so
//! [`crate::nif_material`] swaps such materials for a [`SkyrimSkinMaterial`]: the base material
//! keeps everything the converter published except the normal map and the sRGB `_s` tint, and the
//! extension decodes both maps the way Skyrim's lighting shader does
//! (`shaders/cs_lighting_msn.wgsl`, ported from Community Shaders).
//!
//! # Extension layout
//!
//! | binding | contents |
//! |---|---|
//! | 100 | [`SkyrimSkinSettings`]: a [`SkyrimSkinFlags`] word, a tint colour and detail parameters |
//! | 101/102 | the `_msn` map (linear) |
//! | 103/104 | the `_s` gloss mask (linear; red channel) |
//! | 105/106 | a FaceGen head's tint map, texture-set slot 6 (linear: raw encoded values) |
//! | 107/108 | a FaceGen head's detail map, texture-set slot 3 (linear: raw encoded values) |
//!
//! The FaceGen maps blend with the diffuse on gamma-encoded values, as Skyrim samples every
//! texture without sRGB decoding, so they bind as the linear twins of the sRGB textures the
//! converter publishes, and the shader re-encodes the base colour before the blend.
//!
//! # Skin tone
//!
//! A `skin_tint` surface (body, hands, feet) blends its diffuse with a constant tint colour the
//! same way a FaceGen head blends its tint map ([`SkyrimSkinFlags::SKIN_TINT`], CS's
//! `FACEGEN_RGB_TINT`). The colour is per NPC (`NPC_/QNAM`), while the labelled material is shared
//! by every instance of a GLB, and the body GLB by every actor of a sex. The loader only marks
//! the surface ([`SkinTintSurface`]); [`crate::actor_skin_tone`] gives an actor with QNAM a derived
//! material carrying that tone. Without QNAM, the flag stays clear, even when the converter
//! publishes the NIF's default-white shader-tail colour.
//!
//! # Hair tint
//!
//! FaceGen hair, brows and beards (`hair_tint`) multiply their diffuse by the NIF's hair tint,
//! masked by the vertex colour's green channel ([`SkyrimSkinFlags::HAIR_TINT`], CS's `HAIR`).
//! The colour is already per NPC: the FaceGen NIF carries twice the NPC's `HCLF` hair colour. Hair
//! is tangent-space, so [`hair_material`] keeps the base material whole, normal map included,
//! and only sets the tint; the model-space branch stays off. It shares this material rather than
//! adding one because it is the same lighting shader with another define, and the vertex stage
//! already matches the depth prepass. Most retail brows and hairlines are authored decals and
//! carry a depth state; they draw as a [`SkyrimSkinDepthMaterial`], the depth extension over the
//! hair material, as effect palettes do under theirs.
//!
//! # Hair card edges
//!
//! Hair cards are alpha-tested (glTF `MASK`), and an alpha test leaves a jagged silhouette where
//! a card crosses a pixel. When the view draws with MSAA, the colour pipeline of a masked hair
//! material turns on alpha-to-coverage and the shader (`SKYRIM_HAIR_ALPHA_TO_COVERAGE`) writes a
//! one-pixel coverage ramp instead of discarding, so the samples smooth the edge. The material
//! itself stays [`AlphaMode::Mask`] with the authored cutoff, and only the colour pass changes:
//!
//! - The depth prepass and the shadow passes keep Bevy's alpha test at the authored cutoff.
//!   Bevy's own [`AlphaMode::AlphaToCoverage`] would give them no cutoff at all (the prepass with
//!   MSAA draws the whole card) or its fixed 0.05 (shadows), so beards and hair would shadow and
//!   occlude far more than their visible strands. Retail hair alpha-tests its depth passes too.
//! - The ramp runs from the cutoff outwards: a pixel the prepass covered is fully covered in
//!   colour, so no sample keeps the prepass depth without the hair's colour.
//! - Bevy's mode always sharpens around 0.5, while each retail hair mask authors its own cutoff;
//!   keeping `Mask` keeps that cutoff in the material's uniform.
//! - MSAA is part of the mesh pipeline key, so changing it at runtime re-specialises the pipeline:
//!   without MSAA the colour pass is the plain alpha test.
//!
//! Decal hair ([`SkyrimSkinDepthMaterial`]) draws in the sorted blend pass and keeps its alpha
//! test.

use crate::nif_depth::NifDepthExtension;
use bevy::{
    asset::{AssetPath, LoadContext},
    gltf::gltf,
    image::{
        ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler,
        ImageSamplerDescriptor,
    },
    mesh::MeshVertexBufferLayoutRef,
    pbr::{
        ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
        MeshPipelineKey,
    },
    prelude::*,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
    },
    shader::ShaderRef,
};

pub type SkyrimSkinMaterial = ExtendedMaterial<StandardMaterial, SkyrimSkinExtension>;

/// A [`SkyrimSkinMaterial`] under an authored NIF depth state (`crate::nif_depth`): the depth
/// extension keeps the skin material's shaders and bindings and adds the depth state.
pub type SkyrimSkinDepthMaterial = ExtendedMaterial<SkyrimSkinMaterial, NifDepthExtension>;

/// Where `embedded_asset!` places the material's shader.
const SHADER: &str = "embedded://engine/shaders/skyrim_skin.wgsl";

/// `SLSF1_Specular` (`vendor/project-wormhole-nif/src/nif_flags.rs`, bit 0).
const SLSF1_SPECULAR: u32 = 1;

/// Bits of [`SkyrimSkinSettings::flags`]. Keep in sync with the constants in
/// `shaders/skyrim_skin.wgsl`.
pub struct SkyrimSkinFlags;

impl SkyrimSkinFlags {
    /// Binding 101 holds a model-space normal map; the shader replaces the vertex normal with it.
    pub const MODEL_SPACE_NORMALS: u32 = 1 << 0;
    /// Binding 103 holds the `_s` gloss mask. Without it the material has no gloss, as an unbound
    /// texture reads zero in vanilla.
    pub const SPECULAR_MAP: u32 = 1 << 1;
    /// The NIF's `SLSF1_Specular` bit.
    pub const SPECULAR: u32 = 1 << 2;
    /// Bindings 105 and 107 hold a FaceGen head's tint and detail maps; the shader blends them
    /// into the base colour (`GetFacegenBaseColor`).
    pub const FACEGEN: u32 = 1 << 3;
    /// The base colour is blended with the constant `tint_color` (`GetFacegenRGBTintBaseColor`):
    /// the skin tone of a `skin_tint` body, hands or feet.
    pub const SKIN_TINT: u32 = 1 << 4;
    /// The base colour is multiplied by `tint_color`, masked by the vertex colour's green
    /// channel; the vertex colour's red and blue are not applied: the hair tint of a `hair_tint`
    /// surface.
    pub const HAIR_TINT: u32 = 1 << 5;
    /// Debug: output the shading normal as colour (`N * 0.5 + 0.5`) instead of lighting.
    pub const DEBUG_WORLD_NORMAL: u32 = 1 << 16;
    /// Debug: output the interpolated vertex normal as colour, for comparison.
    pub const DEBUG_VERTEX_NORMAL: u32 = 1 << 17;
    /// Every debug bit.
    pub const DEBUG: u32 = Self::DEBUG_WORLD_NORMAL | Self::DEBUG_VERTEX_NORMAL;
}

#[derive(ShaderType, Reflect, Debug, Clone, PartialEq)]
pub struct SkyrimSkinSettings {
    /// [`SkyrimSkinFlags`].
    pub flags: u32,
    /// The [`SkyrimSkinFlags::SKIN_TINT`] or [`SkyrimSkinFlags::HAIR_TINT`] colour, RGB on the
    /// encoded (gamma) scale the game stores it in, as `NPC_/QNAM` and the NIF do; alpha unused.
    /// White when no tint is set.
    pub tint_color: Vec4,
    /// Reserved for detail-map parameters; the FaceGen blend uses none.
    pub detail: Vec4,
}

impl SkyrimSkinSettings {
    /// Turns the skin tint on with `tone` (encoded RGB, 0..1).
    pub fn set_skin_tint(&mut self, tone: Vec3) {
        self.flags |= SkyrimSkinFlags::SKIN_TINT;
        self.tint_color = tone.extend(1.0);
    }

    /// Turns the hair tint on with `tint` (encoded RGB).
    pub fn set_hair_tint(&mut self, tint: Vec3) {
        self.flags |= SkyrimSkinFlags::HAIR_TINT;
        self.tint_color = tint.extend(1.0);
    }
}

impl Default for SkyrimSkinSettings {
    fn default() -> Self {
        Self {
            flags: 0,
            tint_color: Vec4::ONE,
            detail: Vec4::ZERO,
        }
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
#[bind_group_data(SkyrimSkinKey)]
pub struct SkyrimSkinExtension {
    #[uniform(100)]
    pub settings: SkyrimSkinSettings,
    #[texture(101)]
    #[sampler(102)]
    #[dependency]
    pub model_space_normal: Option<Handle<Image>>,
    #[texture(103)]
    #[sampler(104)]
    #[dependency]
    pub specular: Option<Handle<Image>>,
    #[texture(105)]
    #[sampler(106)]
    #[dependency]
    pub facegen_tint: Option<Handle<Image>>,
    #[texture(107)]
    #[sampler(108)]
    #[dependency]
    pub facegen_detail: Option<Handle<Image>>,
    /// The [`SkyrimSkinImages`] holder this material depends on. Nothing else holds it, and a
    /// labeled asset without a strong handle can be freed before its dependency reports loaded.
    #[reflect(ignore)]
    pub(crate) images: Option<Handle<SkyrimSkinImages>>,
}

/// The part of a [`SkyrimSkinExtension`] its pipelines specialise on.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub struct SkyrimSkinKey {
    /// The material is hair ([`SkyrimSkinFlags::HAIR_TINT`]): a masked one draws its colour pass
    /// with alpha-to-coverage under MSAA.
    pub hair: bool,
}

impl From<&SkyrimSkinExtension> for SkyrimSkinKey {
    fn from(extension: &SkyrimSkinExtension) -> Self {
        Self {
            hair: extension.settings.flags & SkyrimSkinFlags::HAIR_TINT != 0,
        }
    }
}

/// Marks a primitive whose NIF shader is `skin_tint` and which draws with a
/// [`SkyrimSkinMaterial`]: the surfaces an actor's skin tone applies to. Reflected so the marker
/// is cloned with the scene into every world instance.
#[derive(Component, Clone, Copy, Default, Reflect)]
#[reflect(Component, Default)]
pub struct SkinTintSurface;

/// A FaceGen head's tint (texture-set slot 6) and detail (slot 3) maps, both loaded linear.
#[derive(Clone, Debug, PartialEq)]
pub struct FacegenMaps {
    pub tint: Handle<Image>,
    pub detail: Handle<Image>,
}

impl MaterialExtension for SkyrimSkinExtension {
    fn vertex_shader() -> ShaderRef {
        SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    // The prepass and shadow vertex shaders stay Bevy's: the main-pass vertex shader computes
    // positions with the same calls, so the depth it tests against matches.

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        specialize_hair_coverage(descriptor, key.mesh_key, key.bind_group_data);
        Ok(())
    }
}

/// The shader define that swaps a hair card's alpha test for a coverage ramp.
const HAIR_ALPHA_TO_COVERAGE_DEF: &str = "SKYRIM_HAIR_ALPHA_TO_COVERAGE";

/// Turns on alpha-to-coverage for the colour pass of a masked hair material under MSAA (see
/// "Hair card edges" above). `MAY_DISCARD` in the key means the material is [`AlphaMode::Mask`];
/// the sorted decal variant is keyed as a blend and is left alone, as are the prepass and shadow
/// pipelines (`PREPASS_PIPELINE`), which keep the alpha test.
fn specialize_hair_coverage(
    descriptor: &mut RenderPipelineDescriptor,
    mesh_key: MeshPipelineKey,
    key: SkyrimSkinKey,
) {
    if !key.hair
        || !mesh_key.contains(MeshPipelineKey::MAY_DISCARD)
        || descriptor.multisample.count <= 1
    {
        return;
    }
    let Some(fragment) = descriptor.fragment.as_mut() else {
        return;
    };
    let prepass = fragment
        .shader_defs
        .iter()
        .any(|def| *def == "PREPASS_PIPELINE".into());
    if prepass {
        return;
    }
    fragment.shader_defs.push(HAIR_ALPHA_TO_COVERAGE_DEF.into());
    descriptor.multisample.alpha_to_coverage_enabled = true;
}

/// The skin material's images, held under a label of their own. `ExtendedMaterial` reports none
/// of its fields' handles to the asset system, so the skin material depends on this holder
/// instead: a scene then counts as loaded only once its `_msn`, linear `_s` and linear FaceGen
/// maps are.
#[derive(Asset, TypePath)]
pub(crate) struct SkyrimSkinImages {
    #[dependency]
    model_space_normal: Handle<Image>,
    #[dependency]
    specular: Option<Handle<Image>>,
    #[dependency]
    facegen_tint: Option<Handle<Image>>,
    #[dependency]
    facegen_detail: Option<Handle<Image>>,
}

impl SkyrimSkinImages {
    pub(crate) fn new(
        model_space_normal: Handle<Image>,
        specular: Option<Handle<Image>>,
        facegen: Option<&FacegenMaps>,
    ) -> Self {
        Self {
            model_space_normal,
            specular,
            facegen_tint: facegen.map(|maps| maps.tint.clone()),
            facegen_detail: facegen.map(|maps| maps.detail.clone()),
        }
    }
}

/// The debug output a skin material renders instead of lighting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkinDebugView {
    #[default]
    Off,
    /// The shading normal (the decoded `_msn` where there is one).
    WorldNormal,
    /// The interpolated vertex normal.
    VertexNormal,
}

impl SkinDebugView {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "msn" | "world" => Some(Self::WorldNormal),
            "vertex" => Some(Self::VertexNormal),
            _ => None,
        }
    }

    /// Replaces the debug bits of `settings` with this view's.
    pub fn apply(self, settings: &mut SkyrimSkinSettings) {
        settings.flags &= !SkyrimSkinFlags::DEBUG;
        settings.flags |= match self {
            Self::Off => 0,
            Self::WorldNormal => SkyrimSkinFlags::DEBUG_WORLD_NORMAL,
            Self::VertexNormal => SkyrimSkinFlags::DEBUG_VERTEX_NORMAL,
        };
    }
}

/// Builds the skin material from the native base material (`crate::nif_material`), the `_msn`
/// map, the linear `_s` mask and, on a FaceGen head, its linear tint and detail maps.
///
/// The base keeps its colour, roughness, reflectance, specular tint factor, alpha and UV
/// transform. Its normal map and `_s` tint texture are cleared: Bevy would decode the former as a
/// tangent-space map and the latter as an sRGB colour.
pub fn skin_material(
    mut base: StandardMaterial,
    model_space_normal: Handle<Image>,
    specular: Option<Handle<Image>>,
    facegen: Option<FacegenMaps>,
    skin_tint: Option<Vec3>,
    shader_flags_1: u32,
) -> SkyrimSkinMaterial {
    base.normal_map_texture = None;
    base.specular_tint_texture = None;
    let mut flags = SkyrimSkinFlags::MODEL_SPACE_NORMALS;
    if specular.is_some() {
        flags |= SkyrimSkinFlags::SPECULAR_MAP;
    }
    if shader_flags_1 & SLSF1_SPECULAR != 0 {
        flags |= SkyrimSkinFlags::SPECULAR;
    }
    if facegen.is_some() {
        flags |= SkyrimSkinFlags::FACEGEN;
    }
    let (facegen_tint, facegen_detail) =
        facegen.map_or((None, None), |maps| (Some(maps.tint), Some(maps.detail)));
    let mut settings = SkyrimSkinSettings { flags, ..default() };
    if let Some(tone) = skin_tint {
        settings.set_skin_tint(tone);
    }
    SkyrimSkinMaterial {
        base,
        extension: SkyrimSkinExtension {
            settings,
            model_space_normal: Some(model_space_normal),
            specular,
            facegen_tint,
            facegen_detail,
            images: None,
        },
    }
}

/// Builds the hair material: `base` unchanged (its tangent-space normal map included) with the
/// hair tint set. The model-space branch stays off, so the extension binds no images.
pub fn hair_material(base: StandardMaterial, tint: Vec3) -> SkyrimSkinMaterial {
    let mut settings = SkyrimSkinSettings::default();
    settings.set_hair_tint(tint);
    SkyrimSkinMaterial {
        base,
        extension: SkyrimSkinExtension {
            settings,
            ..default()
        },
    }
}

/// Registers the skin material, its image holder and its shaders. Called by
/// [`crate::nif_material::NifMaterialPlugin`].
pub(crate) fn build(app: &mut App) {
    // Headless apps that only build materials have no shader store; they need no shaders.
    if app.world().contains_resource::<Assets<Shader>>() {
        bevy::shader::load_shader_library!(app, "shaders/cs_lighting_msn.wgsl");
        bevy::asset::embedded_asset!(app, "shaders/skyrim_skin.wgsl");
    }
    if !app.world().contains_resource::<Assets<SkyrimSkinImages>>() {
        app.init_asset::<SkyrimSkinImages>();
    }
    if !app.is_plugin_added::<MaterialPlugin<SkyrimSkinMaterial>>() {
        app.add_plugins(MaterialPlugin::<SkyrimSkinMaterial>::default());
    }
    if !app.is_plugin_added::<MaterialPlugin<SkyrimSkinDepthMaterial>>() {
        app.add_plugins(MaterialPlugin::<SkyrimSkinDepthMaterial>::default());
    }
}

/// Loads the linear twin of an sRGB glTF texture with `sampler`.
///
/// The converter publishes each sRGB use under an `.opensky-srgb` alias and a paired linear
/// alias, retaining any `.opensky-wrapN` sampler suffix. Bevy keys images by path and colour space
/// is a load setting, so the distinct linear path keeps both colour space and sampler identity.
/// A texture with no sRGB alias is loaded from its own path, which returns the image already loaded
/// there.
pub(crate) fn load_linear_twin(
    context: &mut LoadContext<'_>,
    srgb: &Handle<Image>,
    sampler: ImageSamplerDescriptor,
) -> Option<Handle<Image>> {
    let path = linear_twin_path(srgb.path()?)?;
    Some(
        context
            .load_builder()
            .with_settings(move |settings: &mut ImageLoaderSettings| {
                settings.is_srgb = false;
                settings.sampler = ImageSampler::Descriptor(sampler.clone());
            })
            .load::<Image>(path),
    )
}

fn linear_twin_path(path: &AssetPath<'static>) -> Option<AssetPath<'static>> {
    let name = path.path().file_name()?.to_str()?;
    let linear = name.replace(".opensky-srgb", "");
    Some(
        AssetPath::from_path_buf(path.path().with_file_name(linear))
            .with_source(path.source().clone_owned()),
    )
}

/// The sampler `bevy_gltf` 0.19 builds for a texture from its default sampler
/// (`loader/gltf_ext/texture.rs`, `texture_sampler`), which is private to that crate.
pub(crate) fn gltf_sampler(
    texture: &gltf::Texture<'_>,
    default_sampler: &ImageSamplerDescriptor,
) -> ImageSamplerDescriptor {
    use ::gltf::texture::{MagFilter, MinFilter, WrappingMode};
    let address = |mode: WrappingMode| match mode {
        WrappingMode::ClampToEdge => ImageAddressMode::ClampToEdge,
        WrappingMode::Repeat => ImageAddressMode::Repeat,
        WrappingMode::MirroredRepeat => ImageAddressMode::MirrorRepeat,
    };
    let source = texture.sampler();
    let mut sampler = default_sampler.clone();
    sampler.address_mode_u = address(source.wrap_s());
    sampler.address_mode_v = address(source.wrap_t());
    // Anisotropic filtering requires trilinear filters, so Bevy keeps them as they are.
    if sampler.anisotropy_clamp != 1 {
        return sampler;
    }
    if let Some(filter) = source.mag_filter() {
        sampler.mag_filter = match filter {
            MagFilter::Nearest => ImageFilterMode::Nearest,
            MagFilter::Linear => ImageFilterMode::Linear,
        };
    }
    if let Some(filter) = source.min_filter() {
        sampler.min_filter = match filter {
            MinFilter::Nearest
            | MinFilter::NearestMipmapNearest
            | MinFilter::NearestMipmapLinear => ImageFilterMode::Nearest,
            MinFilter::Linear | MinFilter::LinearMipmapNearest | MinFilter::LinearMipmapLinear => {
                ImageFilterMode::Linear
            }
        };
        sampler.mipmap_filter = match filter {
            MinFilter::Nearest
            | MinFilter::Linear
            | MinFilter::NearestMipmapNearest
            | MinFilter::LinearMipmapNearest => ImageFilterMode::Nearest,
            MinFilter::NearestMipmapLinear | MinFilter::LinearMipmapLinear => {
                ImageFilterMode::Linear
            }
        };
    }
    sampler
}

/// The glTF texture indices of a FaceGen head's tint and detail maps: the `facegen_tint` and
/// `detail` slots of a `face_tint` material's `OPEN_SKYRIM_material.textureSlots`. `None` unless
/// both are published (packs converted before `facegen_tint` carry the tint as `inner_layer`).
pub(crate) fn facegen_texture_indices(material: &gltf::Material<'_>) -> Option<(usize, usize)> {
    let extension = material.extensions()?.get("OPEN_SKYRIM_material")?;
    if extension.get("lightingShaderType")?.as_str()? != "face_tint" {
        return None;
    }
    let slots = extension.get("textureSlots")?.as_array()?;
    let texture = |semantic: &str| {
        let index = slots
            .iter()
            .find(|slot| slot.get("semantic").and_then(|value| value.as_str()) == Some(semantic))?
            .get("texture")?
            .as_u64()?;
        usize::try_from(index).ok()
    };
    Some((texture("facegen_tint")?, texture("detail")?))
}

/// Whether the material's NIF shader is `skin_tint` (`OPEN_SKYRIM_material.lightingShaderType`).
pub(crate) fn is_skin_tint(material: &gltf::Material<'_>) -> bool {
    lighting_shader_type(material).as_deref() == Some("skin_tint")
}

fn lighting_shader_type(material: &gltf::Material<'_>) -> Option<String> {
    let extension = material.extensions()?.get("OPEN_SKYRIM_material")?;
    Some(extension.get("lightingShaderType")?.as_str()?.to_owned())
}

/// The NIF's hair tint colour for a `hair_tint` material (`OPEN_SKYRIM_material.tintColor`,
/// encoded RGB). Packs whose converter does not read the NIF's shader tail carry none, and the
/// surface keeps the plain material.
pub(crate) fn nif_hair_tint(material: &gltf::Material<'_>) -> Option<Vec3> {
    nif_tint_color(material, "hair_tint")
}

fn nif_tint_color(material: &gltf::Material<'_>, shader: &str) -> Option<Vec3> {
    if lighting_shader_type(material).as_deref() != Some(shader) {
        return None;
    }
    let colour = material
        .extensions()?
        .get("OPEN_SKYRIM_material")?
        .get("tintColor")?
        .as_array()?;
    let channels: Vec<f32> = colour
        .iter()
        .take(3)
        .map(|value| value.as_f64().map(|value| value as f32))
        .collect::<Option<_>>()?;
    let tone = Vec3::from_slice(channels.get(..3)?);
    tone.is_finite().then_some(tone)
}

/// The glTF texture index of the material's `KHR_materials_specular.specularColorTexture`, which
/// is where the converter publishes a model-space material's `_s` map.
pub(crate) fn specular_color_texture_index(material: &gltf::Material<'_>) -> Option<usize> {
    let index = material
        .extension_value("KHR_materials_specular")?
        .pointer("/specularColorTexture/index")?
        .as_u64()?;
    usize::try_from(index).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_twin_keeps_sampler_alias_and_removes_only_srgb_alias() {
        for (alias, linear) in [
            ("body_s.opensky-srgb.ktx2", "body_s.ktx2"),
            (
                "body_s.opensky-srgb.opensky-wrap0.ktx2",
                "body_s.opensky-wrap0.ktx2",
            ),
            (
                "body_s.opensky-srgb.opensky-wrap2.ktx2",
                "body_s.opensky-wrap2.ktx2",
            ),
            ("body_s.opensky-wrap2.ktx2", "body_s.opensky-wrap2.ktx2"),
            ("body_s.ktx2", "body_s.ktx2"),
            ("body_s.opensky-srgb.png", "body_s.png"),
        ] {
            let path = AssetPath::from(format!("textures/{alias}"));
            assert_eq!(
                linear_twin_path(&path).unwrap(),
                AssetPath::from(format!("textures/{linear}"))
            );
        }
    }

    #[test]
    fn linear_twin_keeps_the_directory_and_source() {
        let srgb = AssetPath::from(
            "pack://textures/actors/character/male/malebody_1_s.opensky-srgb.opensky-wrap0.ktx2",
        );
        let twin = linear_twin_path(&srgb).unwrap();
        assert_eq!(
            twin,
            AssetPath::from(
                "pack://textures/actors/character/male/malebody_1_s.opensky-wrap0.ktx2"
            )
        );
        let plain = AssetPath::from("textures/a b/c_s.opensky-srgb.ktx2");
        assert_eq!(
            linear_twin_path(&plain).unwrap(),
            AssetPath::from("textures/a b/c_s.ktx2")
        );
    }

    #[test]
    fn skin_material_drops_the_tangent_and_srgb_bindings_and_keeps_the_rest() {
        let normal = Handle::<Image>::default();
        let base = StandardMaterial {
            base_color: Color::srgb(0.8, 0.6, 0.5),
            base_color_texture: Some(Handle::default()),
            normal_map_texture: Some(normal.clone()),
            specular_tint_texture: Some(Handle::default()),
            specular_tint: Color::srgb(0.5, 0.6, 1.0),
            alpha_mode: AlphaMode::Mask(0.3),
            reflectance: 0.4,
            perceptual_roughness: 0.6,
            uv_transform: bevy::math::Affine2::from_translation(Vec2::new(0.5, 0.0)),
            ..default()
        };
        for (specular, flags_1, expected) in [
            (None, 0, SkyrimSkinFlags::MODEL_SPACE_NORMALS),
            (
                Some(Handle::default()),
                0,
                SkyrimSkinFlags::MODEL_SPACE_NORMALS | SkyrimSkinFlags::SPECULAR_MAP,
            ),
            (
                Some(Handle::default()),
                SLSF1_SPECULAR,
                SkyrimSkinFlags::MODEL_SPACE_NORMALS
                    | SkyrimSkinFlags::SPECULAR_MAP
                    | SkyrimSkinFlags::SPECULAR,
            ),
        ] {
            let material = skin_material(
                base.clone(),
                normal.clone(),
                specular.clone(),
                None,
                None,
                flags_1,
            );
            assert!(material.base.normal_map_texture.is_none());
            assert!(material.base.specular_tint_texture.is_none());
            assert_eq!(material.base.base_color, base.base_color);
            assert_eq!(material.base.base_color_texture, base.base_color_texture);
            assert_eq!(material.base.specular_tint, base.specular_tint);
            assert_eq!(material.base.alpha_mode, base.alpha_mode);
            assert_eq!(material.base.reflectance, base.reflectance);
            assert_eq!(
                material.base.perceptual_roughness,
                base.perceptual_roughness
            );
            assert_eq!(material.base.uv_transform, base.uv_transform);
            assert_eq!(material.extension.model_space_normal, Some(normal.clone()));
            assert_eq!(material.extension.specular, specular);
            assert_eq!(material.extension.settings.flags, expected);
            assert_eq!(material.extension.settings.tint_color, Vec4::ONE);
            assert!(material.extension.facegen_tint.is_none());
            assert!(material.extension.facegen_detail.is_none());
        }

        let mut images = Assets::<Image>::default();
        let maps = FacegenMaps {
            tint: images.add(Image::default()),
            detail: images.add(Image::default()),
        };
        let material = skin_material(
            base.clone(),
            normal.clone(),
            None,
            Some(maps.clone()),
            None,
            0,
        );
        assert_eq!(
            material.extension.settings.flags,
            SkyrimSkinFlags::MODEL_SPACE_NORMALS | SkyrimSkinFlags::FACEGEN
        );
        assert_eq!(material.extension.facegen_tint, Some(maps.tint));
        assert_eq!(material.extension.facegen_detail, Some(maps.detail));

        let tone = Vec3::new(0.78, 0.69, 0.66);
        let material = skin_material(base, normal, None, None, Some(tone), 0);
        assert_eq!(
            material.extension.settings.flags,
            SkyrimSkinFlags::MODEL_SPACE_NORMALS | SkyrimSkinFlags::SKIN_TINT
        );
        assert_eq!(material.extension.settings.tint_color, tone.extend(1.0));
    }

    #[test]
    fn skin_tint_shader_type_is_detected() {
        for (shader, expected) in [
            ("skin_tint", true),
            ("hair_tint", false),
            ("face_tint", false),
        ] {
            let document = material_with_skyrim_extension(serde_json::json!({
                "lightingShaderType": shader,
                "tintColor": [1.0, 1.0, 1.0]
            }));
            assert_eq!(
                is_skin_tint(&document.materials().next().unwrap()),
                expected
            );
        }
        let document = material_with_skyrim_extension(serde_json::json!({
            "lightingShaderType": "skin_tint"
        }));
        assert!(is_skin_tint(&document.materials().next().unwrap()));
    }

    fn material_with_skyrim_extension(extension: serde_json::Value) -> gltf::Gltf {
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "extensionsUsed": ["OPEN_SKYRIM_material"],
            "materials": [{"extensions": {"OPEN_SKYRIM_material": extension}}]
        });
        gltf::Gltf::from_slice(&serde_json::to_vec(&document).unwrap()).unwrap()
    }

    #[test]
    fn facegen_maps_need_a_face_tint_material_with_both_slots() {
        let slots = |tint: &str| {
            serde_json::json!([
                {"slot": 2, "semantic": "unclassified", "texture": 5},
                {"slot": 3, "semantic": "detail", "texture": 6},
                {"slot": 6, "semantic": tint, "texture": 7}
            ])
        };
        for (shader, tint, expected) in [
            ("face_tint", "facegen_tint", Some((7, 6))),
            ("face_tint", "inner_layer", None),
            ("skin_tint", "facegen_tint", None),
            ("multi_layer_parallax", "inner_layer", None),
        ] {
            let document = material_with_skyrim_extension(serde_json::json!({
                "lightingShaderType": shader,
                "textureSlots": slots(tint)
            }));
            let material = document.materials().next().unwrap();
            assert_eq!(
                facegen_texture_indices(&material),
                expected,
                "{shader} {tint}"
            );
        }
        let document = material_with_skyrim_extension(serde_json::json!({
            "lightingShaderType": "face_tint",
            "textureSlots": [{"slot": 6, "semantic": "facegen_tint", "texture": 7}]
        }));
        assert_eq!(
            facegen_texture_indices(&document.materials().next().unwrap()),
            None
        );
    }

    #[test]
    fn the_nif_hair_tint_is_read_from_hair_tint_materials_only() {
        for (shader, expected) in [
            ("hair_tint", Some(Vec3::new(0.5, 0.25, 1.0))),
            ("skin_tint", None),
            ("default", None),
        ] {
            let document = material_with_skyrim_extension(serde_json::json!({
                "lightingShaderType": shader,
                "tintColor": [0.5, 0.25, 1.0]
            }));
            let material = document.materials().next().unwrap();
            assert_eq!(nif_hair_tint(&material), expected, "{shader}");
        }
        let document = material_with_skyrim_extension(serde_json::json!({
            "lightingShaderType": "hair_tint"
        }));
        assert_eq!(nif_hair_tint(&document.materials().next().unwrap()), None);
    }

    #[test]
    fn hair_material_keeps_the_whole_base_and_sets_only_the_tint() {
        let base = StandardMaterial {
            base_color_texture: Some(Handle::default()),
            normal_map_texture: Some(Handle::default()),
            specular_tint_texture: Some(Handle::default()),
            flip_normal_map_y: true,
            alpha_mode: AlphaMode::Mask(0.5),
            ..default()
        };
        let tint = Vec3::new(0.44, 0.46, 0.35);
        let hair = hair_material(base.clone(), tint);
        assert_eq!(hair.base.normal_map_texture, base.normal_map_texture);
        assert_eq!(hair.base.specular_tint_texture, base.specular_tint_texture);
        assert_eq!(hair.base.flip_normal_map_y, base.flip_normal_map_y);
        assert_eq!(hair.base.alpha_mode, base.alpha_mode);
        assert_eq!(hair.extension.settings.flags, SkyrimSkinFlags::HAIR_TINT);
        assert_eq!(hair.extension.settings.tint_color, tint.extend(1.0));
        assert!(hair.extension.model_space_normal.is_none());
        assert!(hair.extension.specular.is_none());
        assert!(SkyrimSkinKey::from(&hair.extension).hair);
        assert!(!SkyrimSkinKey::from(&SkyrimSkinExtension::default()).hair);
    }

    fn pipeline_descriptor(samples: u32, shader_defs: &[&str]) -> RenderPipelineDescriptor {
        use bevy::render::render_resource::{FragmentState, MultisampleState};
        RenderPipelineDescriptor {
            multisample: MultisampleState {
                count: samples,
                ..default()
            },
            fragment: Some(FragmentState {
                shader_defs: shader_defs.iter().map(|def| (*def).into()).collect(),
                ..default()
            }),
            ..default()
        }
    }

    fn uses_coverage(descriptor: &RenderPipelineDescriptor) -> bool {
        let defined = descriptor.fragment.as_ref().is_some_and(|fragment| {
            fragment
                .shader_defs
                .contains(&HAIR_ALPHA_TO_COVERAGE_DEF.into())
        });
        assert_eq!(defined, descriptor.multisample.alpha_to_coverage_enabled);
        defined
    }

    fn specialized(
        samples: u32,
        shader_defs: &[&str],
        mesh_key: MeshPipelineKey,
        hair: bool,
    ) -> bool {
        let mut descriptor = pipeline_descriptor(samples, shader_defs);
        specialize_hair_coverage(&mut descriptor, mesh_key, SkyrimSkinKey { hair });
        uses_coverage(&descriptor)
    }

    #[test]
    fn masked_hair_colour_pass_uses_coverage_only_under_msaa() {
        let masked = MeshPipelineKey::MAY_DISCARD;
        assert!(specialized(4, &[], masked, true));
        assert!(specialized(2, &[], masked, true));
        assert!(!specialized(1, &[], masked, true), "no MSAA: alpha test");
    }

    #[test]
    fn hair_depth_passes_and_other_materials_keep_the_alpha_test() {
        let masked = MeshPipelineKey::MAY_DISCARD;
        assert!(
            !specialized(4, &["PREPASS_PIPELINE"], masked, true),
            "prepass and shadows"
        );
        assert!(!specialized(4, &[], masked, false), "masked skin");
        assert!(
            !specialized(4, &[], MeshPipelineKey::BLEND_ALPHA, true),
            "sorted decal hair"
        );
        assert!(
            !specialized(4, &[], MeshPipelineKey::NONE, true),
            "opaque hair"
        );
    }

    #[test]
    fn debug_views_replace_only_the_debug_bits() {
        let mut settings = SkyrimSkinSettings {
            flags: SkyrimSkinFlags::MODEL_SPACE_NORMALS | SkyrimSkinFlags::SPECULAR_MAP,
            ..default()
        };
        SkinDebugView::WorldNormal.apply(&mut settings);
        SkinDebugView::VertexNormal.apply(&mut settings);
        assert_eq!(
            settings.flags,
            SkyrimSkinFlags::MODEL_SPACE_NORMALS
                | SkyrimSkinFlags::SPECULAR_MAP
                | SkyrimSkinFlags::DEBUG_VERTEX_NORMAL
        );
        SkinDebugView::Off.apply(&mut settings);
        assert_eq!(
            settings.flags,
            SkyrimSkinFlags::MODEL_SPACE_NORMALS | SkyrimSkinFlags::SPECULAR_MAP
        );
        assert_eq!(
            SkinDebugView::parse("msn"),
            Some(SkinDebugView::WorldNormal)
        );
        assert_eq!(SkinDebugView::parse("x"), None);
    }

    /// The flag values the shader tests must be the ones the material sets.
    #[test]
    fn shader_flag_constants_match_the_rust_flags() {
        let shader = include_str!("shaders/skyrim_skin.wgsl");
        for (name, value) in [
            (
                "FLAG_MODEL_SPACE_NORMALS",
                SkyrimSkinFlags::MODEL_SPACE_NORMALS,
            ),
            ("FLAG_SPECULAR_MAP", SkyrimSkinFlags::SPECULAR_MAP),
            ("FLAG_SPECULAR", SkyrimSkinFlags::SPECULAR),
            ("FLAG_FACEGEN", SkyrimSkinFlags::FACEGEN),
            ("FLAG_SKIN_TINT", SkyrimSkinFlags::SKIN_TINT),
            ("FLAG_HAIR_TINT", SkyrimSkinFlags::HAIR_TINT),
            (
                "FLAG_DEBUG_WORLD_NORMAL",
                SkyrimSkinFlags::DEBUG_WORLD_NORMAL,
            ),
            (
                "FLAG_DEBUG_VERTEX_NORMAL",
                SkyrimSkinFlags::DEBUG_VERTEX_NORMAL,
            ),
        ] {
            assert!(
                shader.contains(&format!("const {name}: u32 = {value}u;")),
                "{name} = {value}"
            );
        }
    }
}
