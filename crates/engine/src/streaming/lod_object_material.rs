//! Object LOD material for chunks with a shared normal atlas.

use super::lod_objects::LodObjectRoot;
use crate::{
    config::EngineConfig,
    lights::AMBIENT_ILLUMINANCE,
    sun::GameClock,
    weather_sky::{DEFAULT_WEATHER_FORM_ID, slot_weights},
    world::{
        database::{LodObjectAtlas, verify_lod_object_atlas},
        environment::{EnvironmentCatalog, TimeOfDay, interior::AmbientCube},
    },
};
use bevy::{
    image::{ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor},
    material::OpaqueRendererMethod,
    mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef},
    pbr::{
        ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
        MaterialPlugin,
    },
    prelude::*,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
        VertexFormat,
    },
    shader::ShaderRef,
    world_serialization::WorldInstanceReady,
};
use shared::lod::object_normal_atlas_path;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

pub(crate) const RED_AXIS: MeshVertexAttribute =
    MeshVertexAttribute::new("ObjectAtlasRed", 41_501, VertexFormat::Float32x3);
pub(crate) const GREEN_AXIS: MeshVertexAttribute =
    MeshVertexAttribute::new("ObjectAtlasGreen", 41_502, VertexFormat::Float32x3);
const SHADER: &str = "embedded://engine/streaming/lod_object.wgsl";

type ObjectMaterial = ExtendedMaterial<StandardMaterial, ObjectAtlas>;

#[derive(Clone, Copy, Default, PartialEq, ShaderType)]
struct Ambient {
    mean: Vec4,
    x: Vec4,
    y: Vec4,
    z: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct ObjectAtlas {
    #[texture(100)]
    #[sampler(101)]
    normal: Handle<Image>,
    #[uniform(102)]
    ambient: Ambient,
}

impl MaterialExtension for ObjectAtlas {
    fn vertex_shader() -> ShaderRef {
        SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The depth and shadow passes still use Bevy's standard vertex shader.
        if let Some(buffer) = descriptor.vertex.buffers.first_mut()
            && !descriptor.vertex.shader_defs.iter().any(|def| {
                matches!(def, bevy::shader::ShaderDefVal::Bool(name, true) if name == "PREPASS_PIPELINE")
            })
        {
            let axes = layout.0.get_layout(&[RED_AXIS.at_shader_location(8), GREEN_AXIS.at_shader_location(9)])?;
            buffer.attributes.extend(axes.attributes);
        }
        Ok(())
    }
}

#[derive(Resource, Default)]
struct Atlases {
    pack: PathBuf,
    normals: HashMap<u32, Option<Handle<Image>>>,
    missing_basis: HashSet<u32>,
    materials: HashMap<u32, Handle<ObjectMaterial>>,
}

pub(super) fn build(app: &mut App) {
    bevy::asset::embedded_asset!(app, "lod_object.wgsl");
    app.init_resource::<Atlases>()
        .add_plugins(MaterialPlugin::<ObjectMaterial>::default())
        .add_observer(apply_object_atlas)
        .add_systems(Update, update_object_ambient);
}

fn load_normal(
    config: &EngineConfig,
    assets: &AssetServer,
    world: u32,
) -> color_eyre::Result<Option<Handle<Image>>> {
    #[derive(serde::Deserialize)]
    struct LodAtlases {
        #[serde(default)]
        object_normal_atlases: HashMap<String, String>,
    }
    let payload_path = object_normal_atlas_path(world);
    let manifest: LodAtlases =
        serde_json::from_slice(&std::fs::read(config.assets_dir.join("lod-manifest.json"))?)?;
    let Some(content_hash) = manifest.object_normal_atlases.get(&payload_path) else {
        return Ok(None);
    };
    verify_lod_object_atlas(
        &config.assets_dir,
        &LodObjectAtlas {
            worldspace_id: world,
            payload_path: payload_path.clone(),
            content_hash: content_hash.clone(),
        },
    )?;
    let normal = assets
        .load_builder()
        .with_settings(|settings: &mut ImageLoaderSettings| {
            settings.is_srgb = false;
            settings.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                anisotropy_clamp: 16,
                ..ImageSamplerDescriptor::linear()
            });
        })
        .load(payload_path);
    Ok(Some(normal))
}

#[allow(clippy::too_many_arguments)]
fn apply_object_atlas(
    event: On<WorldInstanceReady>,
    mut commands: Commands,
    roots: Query<&LodObjectRoot>,
    children: Query<&Children>,
    pieces: Query<(&Mesh3d, &MeshMaterial3d<StandardMaterial>)>,
    config: Res<EngineConfig>,
    assets: Res<AssetServer>,
    meshes: Res<Assets<Mesh>>,
    standard: Res<Assets<StandardMaterial>>,
    mut atlas_materials: ResMut<Assets<ObjectMaterial>>,
    mut atlases: ResMut<Atlases>,
) {
    let Ok(root) = roots.get(event.entity) else {
        return;
    };
    let world = root.key.worldspace_id;
    if atlases.pack != config.assets_dir {
        *atlases = Atlases {
            pack: config.assets_dir.clone(),
            ..default()
        };
    }
    let normal = atlases.normals.entry(world).or_insert_with(|| {
        // STAND-IN(#940): the retail missing-file result is unverified. Keep the existing material.
        match load_normal(&config, &assets, world) {
            Ok(Some(normal)) => Some(normal),
            Ok(None) => {
                warn!(world, "object LOD: missing_normal_atlas");
                None
            }
            Err(error) => {
                warn!(world, %error, "object LOD: cannot use the normal atlas");
                None
            }
        }
    });
    let Some(normal) = normal.clone() else { return };
    for entity in children.iter_descendants(event.entity) {
        let Ok((mesh, material)) = pieces.get(entity) else {
            continue;
        };
        let Some(mesh) = meshes.get(&mesh.0) else {
            continue;
        };
        if !mesh.contains_attribute(RED_AXIS) || !mesh.contains_attribute(GREEN_AXIS) {
            if atlases.missing_basis.insert(world) {
                warn!(world, "object LOD: a chunk has no normal atlas basis");
            }
            continue;
        }
        let Some(base) = standard.get(&material.0) else {
            continue;
        };
        let handle = atlases.materials.entry(world).or_insert_with(|| {
            atlas_materials.add(ExtendedMaterial {
                base: StandardMaterial {
                    opaque_render_method: OpaqueRendererMethod::Forward,
                    ..base.clone()
                },
                extension: ObjectAtlas {
                    normal: normal.clone(),
                    ambient: Ambient::default(),
                },
            })
        });
        commands
            .entity(entity)
            .remove::<MeshMaterial3d<StandardMaterial>>()
            .insert(MeshMaterial3d(handle.clone()));
    }
}

fn update_object_ambient(
    clock: Option<Res<GameClock>>,
    catalog: Option<Res<EnvironmentCatalog>>,
    mut materials: ResMut<Assets<ObjectMaterial>>,
    atlases: Res<Atlases>,
) {
    let (Some(clock), Some(catalog)) = (clock, catalog) else {
        return;
    };
    let Some(weather) = catalog.weather(DEFAULT_WEATHER_FORM_ID) else {
        return;
    };
    for (world, handle) in &atlases.materials {
        let Some(timing) = catalog
            .worldspace_climate(*world)
            .and_then(|id| catalog.sun_timing(id))
        else {
            continue;
        };
        let mut ambient = Ambient::default();
        // STAND-IN(#940): use the engine's default exterior weather and light scale.
        for (time, weight) in TimeOfDay::ALL
            .into_iter()
            .zip(slot_weights(clock.hour, timing))
        {
            if let Some(dalc) = weather.directional_ambient.get(&time) {
                let cube = AmbientCube::from_dalc(dalc);
                let weighted =
                    |rgb| Vec3::from_array(rgb).extend(0.0) * (weight * AMBIENT_ILLUMINANCE);
                ambient.mean += weighted(cube.mean);
                ambient.x += weighted(cube.half_difference_x);
                ambient.y += weighted(cube.half_difference_y);
                ambient.z += weighted(cube.half_difference_z);
            }
        }
        if materials
            .get(handle)
            .is_some_and(|material| material.extension.ambient != ambient)
            && let Some(mut material) = materials.get_mut(handle)
        {
            material.extension.ambient = ambient;
        }
    }
}
