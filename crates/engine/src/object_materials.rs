//! Per-instance STAT MODS diffuse substitutions on certified single-shape ordinary NIFs.
//! This deliberately does not implement directional MATO layers or a general TXST shader.

use bevy::{app::SceneSpawnerSystems, asset::LoadState, prelude::*};
use rusqlite::Connection;
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Debug)]
struct TextureSwap {
    name: String,
    index: u32,
    texture_set: u32,
    path: Option<String>,
    disposition: String,
}

/// A scene instance's base-record input. Immutable catalog rows are shared across placements.
#[derive(Component, Clone, Debug)]
pub(crate) struct ReferenceTextureSwaps {
    base: u32,
    swaps: Arc<Vec<TextureSwap>>,
}

#[derive(Default)]
pub(crate) struct ObjectTextureCatalog(HashMap<u32, Arc<Vec<TextureSwap>>>);

impl ObjectTextureCatalog {
    pub(crate) fn read(connection: &Connection) -> rusqlite::Result<Self> {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='base_texture_swaps')",
            [], |row| row.get(0),
        )?;
        if !exists {
            return Ok(Self::default());
        }
        let mut statement = connection.prepare(
            "SELECT base_id,shape_name,shape_index,texture_set_id,diffuse_path,disposition
             FROM base_texture_swaps ORDER BY base_id,position",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                TextureSwap {
                    name: row.get(1)?,
                    index: row.get(2)?,
                    texture_set: row.get(3)?,
                    path: row
                        .get::<_, Option<String>>(4)?
                        .and_then(super::world::database::converted_texture_path),
                    disposition: row.get(5)?,
                },
            ))
        })?;
        let mut bases: HashMap<u32, Vec<TextureSwap>> = HashMap::new();
        for row in rows {
            let (base, swap) = row?;
            bases.entry(base).or_default().push(swap);
        }
        Ok(Self(
            bases
                .into_iter()
                .map(|(base, swaps)| (base, Arc::new(swaps)))
                .collect(),
        ))
    }

    pub(crate) fn get(&self, base: u32) -> Option<ReferenceTextureSwaps> {
        Some(ReferenceTextureSwaps {
            base,
            swaps: self.0.get(&base)?.clone(),
        })
    }
}

/// Source identity certified by the converter, carried on the instantiated primitive.
/// Absent on older GLBs, multi-shape sources, skins and non-default lighting shaders.
#[derive(Component, Clone, Debug, Reflect)]
#[reflect(Component)]
pub(crate) struct ObjectDiffuseShape {
    name: String,
}

impl ObjectDiffuseShape {
    pub(crate) fn from_primitive(primitive: &gltf::Primitive<'_>) -> Option<Self> {
        let extras: serde_json::Value =
            serde_json::from_str(primitive.extras().as_ref()?.get()).ok()?;
        let tag = extras.get("mudcrabObjectDiffuseV1")?;
        if tag.get("shapeIndex")?.as_u64()? != 0 {
            return None;
        }
        let name = tag.get("shapeName")?.as_str()?.to_owned();
        (!name.is_empty()).then_some(Self { name })
    }
}

/// Inspectable result on each placed root. Unsupported/failed inputs retain the NIF material.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub enum ObjectTextureSwapState {
    Pending,
    Applied { texture_set: u32 },
    Unsupported(String),
    Failed(String),
}

#[derive(Component)]
struct PendingTextureSwap;

/// Hold the asynchronous request for the reference's lifetime, including while it is pending.
#[derive(Component)]
struct RequestedDiffuse(Handle<Image>);

#[derive(Component)]
struct DiffuseSource(Handle<StandardMaterial>);

#[derive(Resource, Default)]
struct DiffuseMaterials(
    HashMap<(AssetId<StandardMaterial>, AssetId<Image>), AssetId<StandardMaterial>>,
);

pub struct ObjectMaterialPlugin;

impl Plugin for ObjectMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DiffuseMaterials>()
            .add_systems(
                SpawnScene,
                apply_swaps.after(SceneSpawnerSystems::WorldInstanceSpawn),
            )
            .add_systems(Update, forget_materials);
    }
}

type SwapRoots<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static ReferenceTextureSwaps,
        Option<&'static RequestedDiffuse>,
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

type DiffuseSurfaces<'w, 's> = Query<
    'w,
    's,
    (
        &'static ObjectDiffuseShape,
        &'static mut MeshMaterial3d<StandardMaterial>,
        Option<&'static DiffuseSource>,
    ),
    Without<crate::nif_material::ExternalEmittance>,
>;

/// Bind a derived material only after the replacement image is loaded. The model material,
/// mesh, scene asset, UV transform, alpha/depth settings and every other texture stay intact.
#[allow(clippy::too_many_arguments)]
fn apply_swaps(
    mut commands: Commands,
    roots: SwapRoots,
    profile: Option<Res<crate::visual_profile::VisualProfile>>,
    children: Query<&Children>,
    shapes: Query<(), With<ObjectDiffuseShape>>,
    mut surfaces: DiffuseSurfaces,
    server: Res<AssetServer>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<DiffuseMaterials>,
) {
    for (root, input, requested) in &roots {
        // Scene loading and failure belong to streaming. Only instantiated roots reach this
        // query; otherwise a missing model could poll for appearance forever without children.
        let result = (|| {
            if profile.as_deref() == Some(&crate::visual_profile::VisualProfile::Skyrim) {
                return ObjectTextureSwapState::Unsupported(
                    "raw-colour profile integration pending".into(),
                );
            }
            let [swap] = input.swaps.as_slice() else {
                return ObjectTextureSwapState::Unsupported("requires one MODS entry".into());
            };
            if swap.index != 0 || swap.disposition != "diffuse_only" {
                return ObjectTextureSwapState::Unsupported(format!(
                    "index {}: {}",
                    swap.index, swap.disposition
                ));
            }
            let Some(path) = &swap.path else {
                return ObjectTextureSwapState::Unsupported("invalid diffuse path".into());
            };
            let matching: Vec<_> = children
                .iter_descendants(root)
                .filter(|entity| shapes.contains(*entity))
                .collect();
            let [entity] = matching.as_slice() else {
                return ObjectTextureSwapState::Unsupported(
                    "requires certified single-shape GLB".into(),
                );
            };
            let Ok((shape, mut binding, source)) = surfaces.get_mut(*entity) else {
                return ObjectTextureSwapState::Unsupported(
                    "material type or external emittance".into(),
                );
            };
            if shape.name != swap.name {
                return ObjectTextureSwapState::Unsupported("MODS shape name mismatch".into());
            }
            let original = source.map_or_else(|| binding.0.clone(), |source| source.0.clone());
            if !materials.contains(original.id()) {
                return ObjectTextureSwapState::Pending;
            }
            let target = requested
                .filter(|request| {
                    server
                        .get_path(request.0.id())
                        .is_some_and(|loaded| loaded.path() == std::path::Path::new(path))
                })
                .map(|request| request.0.clone())
                .unwrap_or_else(|| {
                    let target: Handle<Image> = server.load(path.clone());
                    commands
                        .entity(root)
                        .insert(RequestedDiffuse(target.clone()));
                    target
                });
            if let LoadState::Failed(error) = server.load_state(target.id()) {
                return ObjectTextureSwapState::Failed(format!("{path}: {error}"));
            }
            let Some(image) = images.get(&target) else {
                return ObjectTextureSwapState::Pending;
            };
            use bevy::render::render_resource::{TextureDimension, TextureViewDimension};
            let format = image.texture_descriptor.format.add_srgb_suffix();
            if image.texture_descriptor.dimension != TextureDimension::D2
                || image.texture_descriptor.size.depth_or_array_layers != 1
                || image.texture_view_descriptor.as_ref().is_some_and(|view| {
                    view.dimension
                        .is_some_and(|dimension| dimension != TextureViewDimension::D2)
                })
                || !format.is_srgb()
            {
                return ObjectTextureSwapState::Unsupported(
                    "requires a 2D diffuse image with an sRGB view".into(),
                );
            }
            let key = (original.id(), target.id());
            let derived = cache
                .0
                .get(&key)
                .and_then(|id| materials.get_strong_handle(*id));
            let derived = if let Some(derived) = derived {
                derived
            } else {
                let mut derived = materials
                    .get(&original)
                    .expect("source checked above")
                    .clone();
                let mut replacement = image.clone();
                if let Some(source_image) = &derived.base_color_texture {
                    let Some(source_image) = images.get(source_image) else {
                        return ObjectTextureSwapState::Pending;
                    };
                    replacement.sampler = source_image.sampler.clone();
                }
                replacement.texture_descriptor.format = format;
                if let Some(view) = &mut replacement.texture_view_descriptor {
                    view.format = Some(format);
                }
                derived.base_color_texture = Some(images.add(replacement));
                let derived = materials.add(derived);
                cache.0.insert(key, derived.id());
                derived
            };
            if source.is_none() {
                commands.entity(*entity).insert(DiffuseSource(original));
            }
            if binding.0 != derived {
                binding.0 = derived;
            }
            ObjectTextureSwapState::Applied {
                texture_set: swap.texture_set,
            }
        })();
        finish(&mut commands, root, input.base, result);
    }
}

fn finish(commands: &mut Commands, root: Entity, base: u32, result: ObjectTextureSwapState) {
    if matches!(result, ObjectTextureSwapState::Pending) {
        commands.entity(root).insert(PendingTextureSwap);
    } else {
        if matches!(
            result,
            ObjectTextureSwapState::Unsupported(_) | ObjectTextureSwapState::Failed(_)
        ) {
            warn!(
                base = format_args!("{base:08X}"),
                ?result,
                "STAT MODS diffuse substitution not applied"
            );
        }
        commands.entity(root).remove::<PendingTextureSwap>();
    }
    commands.entity(root).insert(result);
}

fn forget_materials(
    mut events: MessageReader<AssetEvent<StandardMaterial>>,
    mut cache: ResMut<DiffuseMaterials>,
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
