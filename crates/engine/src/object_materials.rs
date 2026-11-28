//! Per-instance STAT MODS alternate textures, applied the way the retail game applies them.
//!
//! Retail applies a placed reference's MODS list at its 3D load (RF-TEX-001). Entries are keyed by
//! 3D Index; index -1 is skipped and a later entry with the same index replaces an earlier one.
//! The 3D Index is a pre-order, depth-first count of the model's geometry leaves in child order
//! (RF-TEX-002), and the 3D Name with its `:N` suffix is not read. Only lighting-shader shapes are
//! swapped (RF-TEX-003). The new texture set is built from the TXST alone: an empty slot stays
//! empty and the model's own texture for it is not kept (RF-TEX-004). An entry that names the
//! NullTextureSet hides the shape (RF-TEX-005). RF-TEX IDs are in
//! `evidence/rea-20261114/registry.md`; the traces are in `q8-mods.md`.
//!
//! The converter writes each lighting primitive's source index as `mudcrabGeometryIndex`
//! (`object-texture-swap-geometry-index-v1`; `-v2` also tags models with Havok collision), so the
//! index is the number the game computes whatever order the glTF stores meshes in.
//!
//! The `skyrim` profile reads every swapped texture raw, as it reads the model's own (SPEC-1
//! section 2.1, [near-lighting.md](../../../docs/specs/engine/near-lighting.md)): TX00 gets no
//! sRGB view there. A lit surface with an authored depth state ([`NifDepthMaterial`]) is swapped
//! like a plain one. That profile also applies the glow map (NIF slot 2) to a surface that reads
//! one, the height map (slot 3) to a parallax surface, and the environment cube and mask (slots 4
//! and 5) to an environment surface (section 9.2).
//!
//! The specular slot is the mask of a model-space surface and the detail slot belongs to the face
//! and multilayer techniques. A swap reaches no model-space or face surface, so `skyrim` applies a
//! set that fills either one and samples neither.
//!
//! Limits, reported rather than guessed: `mudcrab` applies TX00 (diffuse) and TX01 (normal) only,
//! and a set that fills another slot keeps the model material there.
//! STAND-IN(#490): the glow map reaches a surface only when its model has one.
//! Directional MATO layers and a general TXST shader remain with their own owners.

use crate::nif_depth::NifDepthMaterial;
use bevy::{
    app::SceneSpawnerSystems,
    asset::LoadState,
    gltf::DefaultGltfImageSampler,
    image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor},
    prelude::*,
};
use rusqlite::Connection;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// What one MODS entry asks for. `shape_name` is not kept: retail never reads it (RF-TEX-002).
#[derive(Clone, Debug)]
struct TextureSwap {
    index: i32,
    texture_set: u32,
    kind: SwapKind,
}

#[derive(Clone, Debug)]
enum SwapKind {
    /// A live TXST. Paths are converted texture paths; `None` is an empty slot (RF-TEX-004).
    Textures {
        diffuse: Option<String>,
        normal: Option<String>,
        /// The slots only the `skyrim` profile applies.
        retail: RetailSlots<String>,
        /// Names of filled TXST slots no profile applies.
        unapplied: Vec<&'static str>,
    },
    /// The NullTextureSet hides the shape (RF-TEX-005).
    Hide,
    /// The target is absent, deleted, of another type or has an unusable path.
    Missing(String),
}

/// The texture set slots the `skyrim` profile applies beside the diffuse and normal maps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RetailSlots<T> {
    glow: Option<T>,
    height: Option<T>,
    environment: Option<T>,
    mask: Option<T>,
}

impl RetailSlots<String> {
    fn borrowed(&self) -> RetailSlots<&str> {
        RetailSlots {
            glow: self.glow.as_deref(),
            height: self.height.as_deref(),
            environment: self.environment.as_deref(),
            mask: self.mask.as_deref(),
        }
    }
}

impl<T: Copy> RetailSlots<T> {
    /// The filled slots with their names.
    fn filled(&self) -> impl Iterator<Item = (&'static str, T)> {
        [
            ("glow", self.glow),
            ("height", self.height),
            ("environment", self.environment),
            ("mask", self.mask),
        ]
        .into_iter()
        .filter_map(|(name, slot)| Some((name, slot?)))
    }
}

/// A scene instance's base-record input. Immutable catalog rows are shared across placements.
#[derive(Component, Clone, Debug)]
pub(crate) struct ReferenceTextureSwaps {
    base: u32,
    swaps: Arc<Vec<TextureSwap>>,
}

#[derive(Default)]
pub(crate) struct ObjectTextureCatalog {
    bases: HashMap<u32, Arc<Vec<TextureSwap>>>,
    /// What an older database lacks. The catalog is read before the log starts, so the first
    /// lookup logs this.
    shortfall: Option<&'static str>,
}

impl ObjectTextureCatalog {
    pub(crate) fn read(connection: &Connection) -> rusqlite::Result<Self> {
        let has_table = |name: &str| -> rusqlite::Result<bool> {
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [name],
                |row| row.get(0),
            )
        };
        if !has_table("base_texture_swaps")? {
            return Ok(Self {
                shortfall: Some(NO_SWAP_TABLE),
                ..Self::default()
            });
        }
        let has_sets = has_table("texture_sets")?;
        let mut statement = connection.prepare(
            "SELECT base_id,shape_index,texture_set_id,disposition
             FROM base_texture_swaps ORDER BY base_id,position",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // An older or reduced `texture_sets` table (a column missing) must not stop the catalog
        // from loading: its entries then report that no texture-set row is available.
        let mut sets = if has_sets {
            connection
                .prepare(
                    "SELECT diffuse_path,normal_path,glow_path,height_path,environment_path,
                            mask_path,specular_path,detail_path FROM texture_sets WHERE id=?1",
                )
                .ok()
        } else {
            None
        };
        let mut bases: HashMap<u32, Vec<TextureSwap>> = HashMap::new();
        for (base, index, texture_set, disposition) in rows {
            let kind = match disposition.as_str() {
                "null_texture_set" => SwapKind::Hide,
                "texture_set" => match sets.as_mut() {
                    Some(sets) => sets
                        .query_row([texture_set], |row| {
                            (0..8)
                                .map(|column| row.get::<_, Option<String>>(column))
                                .collect::<rusqlite::Result<Vec<_>>>()
                        })
                        .ok()
                        .map_or_else(
                            || SwapKind::Missing("texture set row absent".into()),
                            |slots| set_kind(&slots),
                        ),
                    None => SwapKind::Missing("no usable texture_sets table".into()),
                },
                other => SwapKind::Missing(other.to_owned()),
            };
            bases.entry(base).or_default().push(TextureSwap {
                index,
                texture_set,
                kind,
            });
        }
        // A database from before the swaps of every model base lists `STAT` bases only.
        let only_statics = has_table("formid_map")?
            && connection.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM base_texture_swaps b
                 JOIN formid_map f ON f.form_id=b.base_id WHERE f.record_type<>'STAT')",
                [],
                |row| row.get::<_, bool>(0),
            )?;
        Ok(Self {
            shortfall: (only_statics && !bases.is_empty()).then_some(STATIC_SWAPS_ONLY),
            bases: bases
                .into_iter()
                .map(|(base, swaps)| (base, Arc::new(swaps)))
                .collect(),
        })
    }

    pub(crate) fn get(&self, base: u32) -> Option<ReferenceTextureSwaps> {
        if let Some(shortfall) = self.shortfall {
            warn_once!("{shortfall}");
        }
        Some(ReferenceTextureSwaps {
            base,
            swaps: self.bases.get(&base)?.clone(),
        })
    }
}

const NO_SWAP_TABLE: &str = "the world database has no base_texture_swaps table; bases keep their \
     own textures (a metadata rebuild with --reuse-assets adds it)";
const STATIC_SWAPS_ONLY: &str = "the world database lists texture swaps for STAT bases only; other \
     bases keep their own textures (a metadata rebuild with --reuse-assets adds them)";

/// The `texture_sets` columns no profile applies, with their place in the query above: diffuse,
/// normal, glow, height, environment, mask, specular and detail (the projection of TX02 to TX07
/// in `db-schema.md`).
const UNAPPLIED_SLOTS: [(usize, &str); 2] = [(6, "specular"), (7, "detail")];

fn filled(slot: &Option<String>) -> Option<&str> {
    slot.as_deref().filter(|path| !path.is_empty())
}

fn set_kind(slots: &[Option<String>]) -> SwapKind {
    let convert = |slot: &Option<String>| -> Result<Option<String>, String> {
        match filled(slot) {
            None => Ok(None),
            Some(path) => super::world::database::converted_texture_path(path.to_owned())
                .map(Some)
                .ok_or_else(|| format!("unusable texture path {path:?}")),
        }
    };
    let converted: Result<Vec<_>, String> = [0, 1, 2, 3, 4, 5]
        .map(|column| convert(&slots[column]))
        .into_iter()
        .collect();
    let [diffuse, normal, glow, height, environment, mask] = match converted.map(<[_; 6]>::try_from)
    {
        Ok(Ok(converted)) => converted,
        Ok(Err(_)) => return SwapKind::Missing("unusable texture set row".into()),
        Err(reason) => return SwapKind::Missing(reason),
    };
    let unapplied = UNAPPLIED_SLOTS
        .into_iter()
        .filter(|(column, _)| filled(&slots[*column]).is_some())
        .map(|(_, name)| name)
        .collect();
    SwapKind::Textures {
        diffuse,
        normal,
        retail: RetailSlots {
            glow,
            height,
            environment,
            mask,
        },
        unapplied,
    }
}

/// The retail 3D Index of a lighting primitive, written by the converter (RF-TEX-002).
/// Absent on older GLBs, static fallbacks, skins and non-lighting shapes.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub(crate) struct ObjectGeometryIndex(pub(crate) u32);

impl ObjectGeometryIndex {
    pub(crate) fn from_primitive(primitive: &gltf::Primitive<'_>) -> Option<Self> {
        let extras: serde_json::Value =
            serde_json::from_str(primitive.extras().as_ref()?.get()).ok()?;
        let index = extras.get("mudcrabGeometryIndex")?.as_u64()?;
        u32::try_from(index).ok().map(Self)
    }
}

/// Inspectable result on each placed root. Unsupported/failed inputs retain the NIF material.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub enum ObjectTextureSwapState {
    Pending,
    /// Every matching entry was applied or reported: `textured` shapes got a new texture set and
    /// `hidden` shapes were hidden. Entries that name no tagged shape apply to nothing.
    Applied {
        textured: usize,
        hidden: usize,
    },
    Unsupported(String),
    Failed(String),
}

#[derive(Component)]
struct PendingTextureSwap;

/// Hold every asynchronous request for the reference's lifetime, including while pending.
#[derive(Component, Default)]
struct RequestedImages(HashMap<String, Handle<Image>>);

#[derive(Component)]
struct DiffuseSource<M: Asset = StandardMaterial>(Handle<M>);

/// Derived materials, shared by equal inputs: the model material and the TXST (RF-TEX-004).
#[derive(Resource)]
struct DiffuseMaterials<M: Asset = StandardMaterial>(HashMap<(AssetId<M>, u32), AssetId<M>>);

impl<M: Asset> Default for DiffuseMaterials<M> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

/// A material whose texture set a MODS entry replaces.
trait SwappedMaterial: bevy::pbr::Material + Clone {
    fn standard(&mut self) -> &mut StandardMaterial;
    fn standard_ref(&self) -> &StandardMaterial;
}

impl SwappedMaterial for StandardMaterial {
    fn standard(&mut self) -> &mut StandardMaterial {
        self
    }
    fn standard_ref(&self) -> &StandardMaterial {
        self
    }
}

impl SwappedMaterial for NifDepthMaterial {
    fn standard(&mut self) -> &mut StandardMaterial {
        &mut self.base
    }
    fn standard_ref(&self) -> &StandardMaterial {
        &self.base
    }
}

/// Base records and refusal reasons that already produced a warning. A placed base can have
/// many instances and reload with every cell, so each pair warns once per run; the rest log
/// at debug level.
#[derive(Resource, Default)]
struct ReportedSwapRefusals(HashSet<(u32, String)>);

pub struct ObjectMaterialPlugin;

impl Plugin for ObjectMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<NifDepthMaterial>()
            .init_resource::<DiffuseMaterials>()
            .init_resource::<DiffuseMaterials<NifDepthMaterial>>()
            .init_resource::<ReportedSwapRefusals>()
            .add_systems(
                SpawnScene,
                apply_swaps.after(SceneSpawnerSystems::WorldInstanceSpawn),
            )
            .add_systems(
                Update,
                (
                    forget_materials::<StandardMaterial>,
                    forget_materials::<NifDepthMaterial>,
                ),
            );
    }
}

type SwapRoots<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static ReferenceTextureSwaps,
        Option<&'static RequestedImages>,
    ),
    (
        With<Children>,
        Or<(
            Changed<Children>,
            Changed<ReferenceTextureSwaps>,
            With<PendingTextureSwap>,
        )>,
    ),
>;

type SwapSurfaces<'w, 's, M = StandardMaterial> = Query<
    'w,
    's,
    (
        &'static ObjectGeometryIndex,
        &'static mut MeshMaterial3d<M>,
        Option<&'static DiffuseSource<M>>,
    ),
    Without<crate::nif_material::ExternalEmittance>,
>;

/// The sampler a swap image is requested with: the one `bevy_gltf` gives a texture that names no
/// sampler. That is the glTF default sampler with the glTF default wrap, which repeats. Models,
/// terrain and swaps share images by path, and the first request fixes the sampler for all of
/// them, so a swap that asks first must ask the way the model loader would have.
fn swap_image_sampler(default: Option<&DefaultGltfImageSampler>) -> ImageSamplerDescriptor {
    let mut sampler = default.map_or_else(crate::render::terrain_layer_sampler, |default| {
        default.get()
    });
    sampler.address_mode_u = ImageAddressMode::Repeat;
    sampler.address_mode_v = ImageAddressMode::Repeat;
    sampler
}

/// The model material of a surface, once it has loaded: the one a swap derives from.
fn model_of<'a, M: SwappedMaterial>(
    surfaces: &SwapSurfaces<M>,
    entity: Entity,
    materials: &'a Assets<M>,
) -> Option<&'a StandardMaterial> {
    // silent-ok: an entity that draws the other material type has no surface of this one.
    let (_, binding, source) = surfaces.get(entity).ok()?;
    let original = source.map_or(binding.0.id(), |source| source.0.id());
    materials.get(original).map(SwappedMaterial::standard_ref)
}

/// The one place a swap requests an image. Bevy keys an image by path and the first request's
/// settings win for every later consumer of that path, so the settings must match what the other
/// consumers of the path would have asked for. Landscape textures serve swaps and models alike: a
/// rock model reads the same `_n.ktx2` normal map a swap names. A normal map is data, never sRGB
/// (`srgb` false); asking with the default, sRGB, would give the model an sRGB view of its own
/// normal map, which strict validation refuses and which hides the model.
fn load_swap_image(
    server: &AssetServer,
    path: &str,
    srgb: bool,
    sampler: &ImageSamplerDescriptor,
) -> Handle<Image> {
    let sampler = sampler.clone();
    server
        .load_builder()
        .with_settings(move |settings: &mut ImageLoaderSettings| {
            settings.is_srgb = srgb;
            settings.sampler = ImageSampler::Descriptor(sampler.clone());
        })
        .load(path.to_owned())
}

/// What one matched shape will receive once its images are ready.
enum Plan<'a> {
    Hide,
    Textures {
        swap: &'a TextureSwap,
        paths: SwapPaths<'a>,
    },
}

/// The paths one derived material takes.
#[derive(Clone, Copy)]
struct SwapPaths<'a> {
    diffuse: Option<&'a str>,
    normal: Option<&'a str>,
    /// Empty under a profile that applies the diffuse and normal maps only.
    retail: RetailSlots<&'a str>,
}

impl RetailSlots<&str> {
    /// The slots a surface of `model` reads: the glow map with a model glow map, the height map
    /// on a parallax surface, and the cube and its mask on a surface that reflects. A swap does
    /// not request another slot, so a missing file there cannot fail it.
    fn read_by(self, model: &StandardMaterial) -> Self {
        let parallax = model.retail_surface.is_some() && model.depth_map.is_some();
        let reflects = self.environment.is_some()
            && model
                .retail_surface
                .is_some_and(|surface| surface.environment_scale != 0.0);
        Self {
            glow: self.glow.filter(|_| model.emissive_texture.is_some()),
            height: self.height.filter(|_| parallax),
            environment: self.environment.filter(|_| reflects),
            mask: self.mask.filter(|_| reflects),
        }
    }
}

/// The derived textures of one texture set, or why the images cannot take the slots.
struct SwapTextures {
    diffuse: Option<Handle<Image>>,
    normal: Option<Handle<Image>>,
    glow: Option<Handle<Image>>,
}

/// Replaces the texture set of the material `binding` draws with. `None` when the model's
/// material has not loaded yet.
#[allow(clippy::too_many_arguments)]
fn bind_swapped<M: SwappedMaterial>(
    commands: &mut Commands,
    entity: Entity,
    binding: &mut Mut<MeshMaterial3d<M>>,
    source: Option<&DiffuseSource<M>>,
    texture_set: u32,
    paths: SwapPaths<'_>,
    handles: &HashMap<String, Handle<Image>>,
    colour_srgb: bool,
    materials: &mut Assets<M>,
    cache: &mut DiffuseMaterials<M>,
    images: &mut Assets<Image>,
) -> Option<Result<(), String>> {
    let original = source.map_or_else(|| binding.0.clone(), |source| source.0.clone());
    if !materials.contains(original.id()) {
        return None;
    }
    let key = (original.id(), texture_set);
    let derived = cache
        .0
        .get(&key)
        .and_then(|id| materials.get_strong_handle(*id));
    let derived = match derived {
        Some(derived) => derived,
        None => {
            let mut derived = materials
                .get(&original)
                .expect("source checked above")
                .clone();
            let textures =
                match swap_textures(derived.standard_ref(), paths, handles, colour_srgb, images) {
                    Ok(textures) => textures,
                    Err(reason) => return Some(Err(reason)),
                };
            // RF-TEX-004: the new set comes from the TXST alone. Slots it leaves empty are
            // empty; the model's own textures are dropped.
            let standard = derived.standard();
            standard.base_color_texture = textures.diffuse;
            // An empty base slot of a retail surface samples the default texel (SPEC-1
            // section 5). Without a texture a lit surface draws white.
            if standard.retail_surface.is_some() && standard.base_color_texture.is_none() {
                standard.base_color_texture = Some(images.add(crate::nif_material::texel_image(
                    crate::nif_material::EMPTY_BASE_TEXEL,
                )));
            }
            standard.normal_map_texture = textures.normal;
            standard.emissive_texture = textures.glow;
            standard.specular_texture = None;
            standard.metallic_roughness_texture = None;
            standard.occlusion_texture = None;
            // The environment technique reads the new cube and mask. `retail_environment` binds
            // them once their shapes are checked; an empty cube slot reflects nothing.
            let image = |path: Option<&str>| path.map(|path| handles[path].clone());
            // A parallax surface reads the new height map, and an empty slot is black.
            // `retail_environment` makes the view read the stored bytes.
            if standard.retail_surface.is_some() && standard.depth_map.is_some() {
                standard.depth_map = Some(image(paths.retail.height).unwrap_or_else(|| {
                    images.add(crate::nif_material::texel_image(
                        crate::nif_material::EMPTY_HEIGHT_TEXEL,
                    ))
                }));
            }
            standard.retail_environment_cube = None;
            standard.retail_environment_mask = None;
            standard.retail_environment_cube_source = image(paths.retail.environment);
            standard.retail_environment_mask_source = image(paths.retail.mask);
            let derived = materials.add(derived);
            cache.0.insert(key, derived.id());
            derived
        }
    };
    if source.is_none() {
        commands.entity(entity).insert(DiffuseSource(original));
    }
    if binding.0 != derived {
        binding.0 = derived;
    }
    Some(Ok(()))
}

/// Copies of the loaded images in the colour space and sampler of the slots they take. TX00 is
/// colour, which only a profile that decodes sRGB views as sRGB; TX01 is direction data.
fn swap_textures(
    model: &StandardMaterial,
    SwapPaths {
        diffuse,
        normal,
        retail,
    }: SwapPaths<'_>,
    handles: &HashMap<String, Handle<Image>>,
    colour_srgb: bool,
    images: &mut Assets<Image>,
) -> Result<SwapTextures, String> {
    let sampler_of = |texture: &Option<Handle<Image>>| {
        texture
            .as_ref()
            .and_then(|texture| images.get(texture))
            .map(|image| image.sampler.clone())
    };
    let diffuse_sampler = sampler_of(&model.base_color_texture);
    let normal_sampler = sampler_of(&model.normal_map_texture).or_else(|| diffuse_sampler.clone());
    let glow_sampler = sampler_of(&model.emissive_texture);
    let mut build = |path: Option<&str>,
                     srgb: bool,
                     sampler: Option<bevy::image::ImageSampler>|
     -> Result<Option<Handle<Image>>, String> {
        let Some(path) = path else {
            return Ok(None);
        };
        use bevy::render::render_resource::{TextureDimension, TextureViewDimension};
        let image = images
            .get(&handles[path])
            .expect("every requested image is loaded");
        // The shared image keeps the colour space its first requester chose; this copy takes
        // the slot's own.
        let format = if srgb {
            image.texture_descriptor.format.add_srgb_suffix()
        } else {
            image.texture_descriptor.format.remove_srgb_suffix()
        };
        if image.texture_descriptor.dimension != TextureDimension::D2
            || image.texture_descriptor.size.depth_or_array_layers != 1
            || image.texture_view_descriptor.as_ref().is_some_and(|view| {
                view.dimension
                    .is_some_and(|dimension| dimension != TextureViewDimension::D2)
            })
            || (srgb && !format.is_srgb())
        {
            return Err(format!(
                "{path}: requires a 2D image{}",
                if srgb { " with an sRGB view" } else { "" }
            ));
        }
        let mut replacement = image.clone();
        if let Some(sampler) = sampler {
            replacement.sampler = sampler;
        }
        replacement.texture_descriptor.format = format;
        if let Some(view) = &mut replacement.texture_view_descriptor {
            view.format = Some(format);
        }
        Ok(Some(images.add(replacement)))
    };
    Ok(SwapTextures {
        diffuse: build(diffuse, colour_srgb, diffuse_sampler)?,
        normal: build(normal, false, normal_sampler)?,
        glow: build(retail.glow, colour_srgb, glow_sampler)?,
    })
}

/// Bind a derived material only after every replacement image is loaded. The model's mesh,
/// scene asset, UV transform and alpha/depth settings stay intact; the texture set is replaced
/// as a whole (RF-TEX-004) so a slot the TXST leaves empty is empty.
#[allow(clippy::too_many_arguments)]
fn apply_swaps(
    mut commands: Commands,
    roots: SwapRoots,
    profile: Option<Res<crate::visual_profile::VisualProfile>>,
    children: Query<&Children>,
    tagged: Query<(), With<ObjectGeometryIndex>>,
    mut surfaces: SwapSurfaces,
    mut depth_surfaces: SwapSurfaces<NifDepthMaterial>,
    server: Res<AssetServer>,
    default_sampler: Option<Res<DefaultGltfImageSampler>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut depth_materials: ResMut<Assets<NifDepthMaterial>>,
    mut cache: ResMut<DiffuseMaterials>,
    mut depth_cache: ResMut<DiffuseMaterials<NifDepthMaterial>>,
    mut reported: ResMut<ReportedSwapRefusals>,
) {
    let colour_srgb = profile
        .as_deref()
        .is_none_or(|profile| profile.decodes_colour_srgb());
    for (root, input, requested) in &roots {
        // Scene loading and failure belong to streaming. Only instantiated roots reach this
        // query; otherwise a missing model could poll for appearance forever without children.
        let result = (|| {
            // RF-TEX-001: index -1 is skipped; the later of two entries with one index wins.
            let mut by_index: HashMap<u32, &TextureSwap> = HashMap::new();
            for swap in input.swaps.iter() {
                if let Ok(index) = u32::try_from(swap.index) {
                    by_index.insert(index, swap);
                }
            }
            let shapes: Vec<(Entity, u32)> = children
                .iter_descendants(root)
                .filter(|entity| tagged.contains(*entity))
                .filter_map(|entity| {
                    let index = match surfaces.get(entity) {
                        Ok(found) => found.0.0,
                        Err(_) => depth_surfaces.get(entity).ok()?.0.0,
                    };
                    Some((entity, index))
                })
                .collect();
            if shapes.is_empty() {
                return ObjectTextureSwapState::Unsupported(
                    "model has no geometry-index tags (older GLB, skin or static fallback)".into(),
                );
            }
            // Phase 1: decide each matched shape's plan and request its images.
            let mut plans: Vec<(Entity, Plan)> = Vec::new();
            let mut skipped: Vec<String> = Vec::new();
            for (entity, index) in &shapes {
                let Some(swap) = by_index.get(index).copied() else {
                    continue;
                };
                match &swap.kind {
                    SwapKind::Hide => plans.push((*entity, Plan::Hide)),
                    SwapKind::Missing(reason) => {
                        skipped.push(format!("index {index}: {reason}"));
                    }
                    SwapKind::Textures {
                        diffuse,
                        normal,
                        retail,
                        unapplied,
                    } => {
                        // A profile that decodes colour applies the first two slots only. Under
                        // `skyrim` no technique that a swap reaches samples the specular or
                        // detail slot (SPEC-1 sections 5 and 9.2), so such a set applies.
                        let mut refused = unapplied.clone();
                        let mut retail = retail.borrowed();
                        if colour_srgb {
                            refused.extend(retail.filled().map(|(name, _)| name));
                            retail = RetailSlots::default();
                        } else {
                            refused.clear();
                        }
                        if !refused.is_empty() {
                            skipped.push(format!(
                                "index {index}: texture set fills unsupported slots {refused:?}"
                            ));
                            continue;
                        }
                        if retail.filled().next().is_some() {
                            let model = model_of(&surfaces, *entity, &materials)
                                .or_else(|| model_of(&depth_surfaces, *entity, &depth_materials));
                            let Some(model) = model else {
                                return ObjectTextureSwapState::Pending;
                            };
                            retail = retail.read_by(model);
                        }
                        plans.push((
                            *entity,
                            Plan::Textures {
                                swap,
                                paths: SwapPaths {
                                    diffuse: diffuse.as_deref(),
                                    normal: normal.as_deref(),
                                    retail,
                                },
                            },
                        ));
                    }
                }
            }
            let mut handles: HashMap<String, Handle<Image>> =
                requested.map(|held| held.0.clone()).unwrap_or_default();
            let sampler = swap_image_sampler(default_sampler.as_deref());
            let mut new_request = false;
            let mut pending = false;
            for (_, plan) in &plans {
                let Plan::Textures { paths, .. } = plan else {
                    continue;
                };
                // TX00 is colour and TX01 is direction data. The slots only `skyrim` applies are
                // read raw.
                for (path, srgb) in [(paths.diffuse, colour_srgb), (paths.normal, false)]
                    .into_iter()
                    .filter_map(|(path, srgb)| Some((path?, srgb)))
                    .chain(paths.retail.filled().map(|(_, path)| (path, false)))
                {
                    let handle = handles.entry(path.to_owned()).or_insert_with(|| {
                        new_request = true;
                        load_swap_image(&server, path, srgb, &sampler)
                    });
                    if let LoadState::Failed(error) = server.load_state(handle.id()) {
                        return ObjectTextureSwapState::Failed(format!("{path}: {error}"));
                    }
                    pending |= images.get(&*handle).is_none();
                }
            }
            if new_request {
                commands
                    .entity(root)
                    .insert(RequestedImages(handles.clone()));
            }
            if pending {
                return ObjectTextureSwapState::Pending;
            }
            // Phase 2: every image is loaded; apply.
            let mut textured = 0;
            let mut hidden = 0;
            for (entity, plan) in plans {
                match plan {
                    Plan::Hide => {
                        commands.entity(entity).insert(Visibility::Hidden);
                        hidden += 1;
                    }
                    Plan::Textures { swap, paths } => {
                        let bound = if let Ok((_, mut binding, source)) = surfaces.get_mut(entity) {
                            bind_swapped(
                                &mut commands,
                                entity,
                                &mut binding,
                                source,
                                swap.texture_set,
                                paths,
                                &handles,
                                colour_srgb,
                                &mut materials,
                                &mut cache,
                                &mut images,
                            )
                        } else if let Ok((_, mut binding, source)) = depth_surfaces.get_mut(entity)
                        {
                            bind_swapped(
                                &mut commands,
                                entity,
                                &mut binding,
                                source,
                                swap.texture_set,
                                paths,
                                &handles,
                                colour_srgb,
                                &mut depth_materials,
                                &mut depth_cache,
                                &mut images,
                            )
                        } else {
                            continue;
                        };
                        match bound {
                            None => return ObjectTextureSwapState::Pending,
                            Some(Err(reason)) => {
                                return ObjectTextureSwapState::Unsupported(reason);
                            }
                            Some(Ok(())) => textured += 1,
                        }
                    }
                }
            }
            for reason in skipped {
                report(&mut reported, input.base, &reason);
            }
            ObjectTextureSwapState::Applied { textured, hidden }
        })();
        finish(&mut commands, &mut reported, root, input.base, result);
    }
}

fn report(reported: &mut ReportedSwapRefusals, base: u32, reason: &str) {
    debug!(
        base = format_args!("{base:08X}"),
        reason, "STAT MODS entry not applied to an instance"
    );
    if reported.0.insert((base, reason.to_owned())) {
        warn!(
            base = format_args!("{base:08X}"),
            reason,
            "STAT MODS entry not applied (once per base record and reason; later instances log at debug level)"
        );
    }
}

fn finish(
    commands: &mut Commands,
    reported: &mut ReportedSwapRefusals,
    root: Entity,
    base: u32,
    result: ObjectTextureSwapState,
) {
    if matches!(result, ObjectTextureSwapState::Pending) {
        commands.entity(root).insert(PendingTextureSwap);
    } else {
        if let ObjectTextureSwapState::Unsupported(reason)
        | ObjectTextureSwapState::Failed(reason) = &result
        {
            report(reported, base, &format!("{result:?}: {reason}"));
        }
        commands.entity(root).remove::<PendingTextureSwap>();
    }
    commands.entity(root).insert(result);
}

fn forget_materials<M: Asset>(
    mut events: MessageReader<AssetEvent<M>>,
    mut cache: ResMut<DiffuseMaterials<M>>,
) {
    for event in events.read() {
        if let AssetEvent::Removed { id } = event {
            cache
                .0
                .retain(|(source, _), derived| source != id && derived != id);
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_tests;
