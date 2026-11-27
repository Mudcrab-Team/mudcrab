//! Object LOD material for chunks with a shared normal atlas.

use super::lod_objects::{LodObjectRoot, ObjectSceneReady};
use crate::{
    config::EngineConfig,
    lights::AMBIENT_ILLUMINANCE,
    sun::{GameClock, world_sun_direction},
    visual_profile::VisualProfile,
    weather_sky::{DEFAULT_WEATHER_FORM_ID, slot_weights},
    world::{
        database::{LodObjectAtlas, verify_lod_object_atlas},
        environment::{EnvironmentCatalog, TimeOfDay, interior::AmbientCube},
    },
};
use bevy::{
    asset::LoadState,
    ecs::schedule::common_conditions::any_with_component,
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
// skyrim_decomp#1003 G03. SunlightScale is authored in IMGS.HNAM. Neither Ultra.ini nor
// Skyrim_Default.ini sets it. It applies only when the pack's image space for a day part has no
// sunlight scale. The engine's near path uses its own `SUNLIGHT_SCALE` (2.8) for the directional
// light. That constant does not feed this material.
// STAND-IN(#946): the IMGS constructor default. G03 gives "about 1.8". The exact float is
// unverified.
const DEFAULT_SUNLIGHT_SCALE: f32 = 1.8;
const SHADER: &str = "embedded://engine/streaming/lod_object.wgsl";

type ObjectMaterial = ExtendedMaterial<StandardMaterial, ObjectAtlas>;

#[derive(Clone, Copy, Debug, Default, PartialEq, ShaderType)]
struct ObjectLighting {
    mean: Vec4,
    x: Vec4,
    y: Vec4,
    z: Vec4,
    sun_direction: Vec4,
    sun_rgb: Vec4,
    /// `x` is 1 under the `skyrim` visual profile and 0 under `mudcrab`. The shader picks the
    /// retail terms or the engine light scale by this flag (a uniform branch).
    profile: Vec4,
}

impl ObjectLighting {
    /// The unlit starting value for a profile. Under `skyrim` this is black until the system finds
    /// the weather rows: the retail shader has no other light source.
    fn for_profile(profile: VisualProfile) -> Self {
        Self {
            profile: Vec4::new(f32::from(profile == VisualProfile::Skyrim), 0.0, 0.0, 0.0),
            ..default()
        }
    }
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct ObjectAtlas {
    #[texture(100)]
    #[sampler(101)]
    normal: Handle<Image>,
    #[uniform(102)]
    lighting: ObjectLighting,
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
    /// Worlds whose `skyrim` lighting is missing and already logged.
    unlit: HashSet<u32>,
}

#[derive(Component)]
struct PendingObjectAtlas;

// skyrim_decomp#948: both atlas slots use the same clamp sampler with anisotropy and mips.
fn atlas_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        anisotropy_clamp: 16,
        ..ImageSamplerDescriptor::linear()
    })
}

pub(super) fn build(app: &mut App) {
    bevy::asset::embedded_asset!(app, "lod_object.wgsl");
    app.init_resource::<Atlases>()
        .add_plugins(MaterialPlugin::<ObjectMaterial>::default())
        .add_systems(
            Update,
            (
                queue_object_atlases,
                apply_object_atlases.run_if(any_with_component::<PendingObjectAtlas>),
                update_object_lighting,
            )
                .chain(),
        );
}

#[derive(serde::Deserialize)]
struct LodAtlases {
    #[serde(default)]
    objects_compiler_version: u32,
    #[serde(default)]
    object_normal_atlases: HashMap<String, String>,
}

fn world_editor_id(pack: &std::path::Path, world: u32) -> String {
    let editor_id = rusqlite::Connection::open_with_flags(
        pack.join("skyrim_world.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .and_then(|db| {
        db.query_row(
            "SELECT editor_id FROM worldspaces WHERE id=?1",
            [world],
            |row| row.get::<_, String>(0),
        )
    });
    match editor_id {
        Ok(name) => name,
        Err(error) => {
            debug!(world, %error, "object LOD: cannot read the world editor ID");
            format!("{world:08X}")
        }
    }
}

fn load_normal(
    pack: &std::path::Path,
    assets: &AssetServer,
    world: u32,
) -> color_eyre::Result<Option<Handle<Image>>> {
    let payload_path = object_normal_atlas_path(world);
    let manifest: LodAtlases =
        serde_json::from_slice(&std::fs::read(pack.join("lod-manifest.json"))?)?;
    let Some(content_hash) = manifest.object_normal_atlases.get(&payload_path) else {
        let message = if manifest.objects_compiler_version < 6 {
            "chunks built before compiler 6"
        } else {
            "missing_normal_atlas"
        };
        // STAND-IN(#983): the retail missing-file result is unverified. Keep the existing material.
        warn!(world = %world_editor_id(pack, world), "object LOD: {message}");
        return Ok(None);
    };
    verify_lod_object_atlas(
        pack,
        &LodObjectAtlas {
            worldspace_id: world,
            payload_path: payload_path.clone(),
            content_hash: content_hash.clone(),
        },
    )?;
    Ok(Some(
        assets
            .load_builder()
            .with_settings(|settings: &mut ImageLoaderSettings| {
                settings.is_srgb = false;
                settings.sampler = atlas_sampler();
            })
            .load(payload_path),
    ))
}

fn queue_object_atlases(
    mut commands: Commands,
    ready: Query<Entity, (Added<ObjectSceneReady>, With<LodObjectRoot>)>,
) {
    for entity in &ready {
        commands.entity(entity).insert(PendingObjectAtlas);
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_object_atlases(
    mut commands: Commands,
    roots: Query<(Entity, &LodObjectRoot), With<PendingObjectAtlas>>,
    children: Query<&Children>,
    pieces: Query<(&Mesh3d, &MeshMaterial3d<StandardMaterial>)>,
    config: Res<EngineConfig>,
    assets: Res<AssetServer>,
    meshes: Res<Assets<Mesh>>,
    standard: Res<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut atlas_materials: ResMut<Assets<ObjectMaterial>>,
    mut atlases: ResMut<Atlases>,
) {
    if atlases.pack != config.assets_dir {
        *atlases = Atlases {
            pack: config.assets_dir.clone(),
            ..default()
        };
    }
    for (entity, root) in &roots {
        let world = root.key.worldspace_id;
        let normal = atlases.normals.entry(world).or_insert_with(|| {
            load_normal(&config.assets_dir, &assets, world)
                .unwrap_or_else(|error| {
                    warn!(world = %world_editor_id(&config.assets_dir, world), %error, "object LOD: cannot use the normal atlas");
                    None
                })
        });
        let Some(image) = normal.clone() else {
            commands.entity(entity).remove::<PendingObjectAtlas>();
            continue;
        };
        if let LoadState::Failed(error) = assets.load_state(image.id()) {
            warn!(world = %world_editor_id(&config.assets_dir, world), %error, "object LOD: cannot load the normal atlas");
            *normal = None;
            commands.entity(entity).remove::<PendingObjectAtlas>();
            continue;
        }
        if !images.contains(image.id()) {
            continue;
        }
        for piece in children.iter_descendants(entity) {
            let Ok((mesh, material)) = pieces.get(piece) else {
                continue;
            };
            let Some(mesh) = meshes.get(&mesh.0) else {
                continue;
            };
            if !mesh.contains_attribute(RED_AXIS) || !mesh.contains_attribute(GREEN_AXIS) {
                if atlases.missing_basis.insert(world) {
                    warn!(world = %world_editor_id(&config.assets_dir, world), "object LOD: a chunk has no normal atlas basis");
                }
                continue;
            }
            let Some(base) = standard.get(&material.0) else {
                continue;
            };
            let handle = atlases.materials.entry(world).or_insert_with(|| {
                if let Some(texture) = &base.base_color_texture
                    && let Some(mut diffuse) = images.get_mut(texture)
                {
                    // The glTF sampler has clamp and mip filters, but glTF has no
                    // anisotropy field. Apply the same descriptor as the normal slot.
                    diffuse.sampler = atlas_sampler();
                }
                atlas_materials.add(ExtendedMaterial {
                    base: object_atlas_base(base),
                    extension: ObjectAtlas {
                        normal: image.clone(),
                        lighting: ObjectLighting::for_profile(config.visual_profile),
                    },
                })
            });
            commands
                .entity(piece)
                .remove::<MeshMaterial3d<StandardMaterial>>()
                .insert(MeshMaterial3d(handle.clone()));
        }
        commands.entity(entity).remove::<PendingObjectAtlas>();
    }
}

/// The base material of an object LOD draw: the chunk's own material in the forward renderer, in
/// the opaque or alpha-mask pass and without transmission.
///
/// Under the `skyrim` profile the retail fog composite (`retail_fog.rs`, #1004) runs after the
/// opaque pass and before the transmissive and transparent passes. It fogs only draws in the
/// opaque, alpha-mask or alpha-to-coverage phases that do not read the view transmission texture.
/// Bevy's shader fog skips exactly those draws. A draw outside that set would not be fogged by
/// the composite and would not be fogged by the shader either. The converter writes `MASK` for
/// object LOD, so this keeps the chunk's mode and only repairs a mode that leaves the set.
fn object_atlas_base(base: &StandardMaterial) -> StandardMaterial {
    let alpha_mode = match base.alpha_mode {
        mode @ (AlphaMode::Opaque | AlphaMode::Mask(_) | AlphaMode::AlphaToCoverage) => mode,
        _ => AlphaMode::Opaque,
    };
    StandardMaterial {
        opaque_render_method: OpaqueRendererMethod::Forward,
        alpha_mode,
        specular_transmission: 0.0,
        diffuse_transmission: 0.0,
        ..base.clone()
    }
}

fn update_object_lighting(
    clock: Option<Res<GameClock>>,
    catalog: Option<Res<EnvironmentCatalog>>,
    config: Res<EngineConfig>,
    mut materials: ResMut<Assets<ObjectMaterial>>,
    mut atlases: ResMut<Atlases>,
) {
    let skyrim = config.visual_profile == VisualProfile::Skyrim;
    let Atlases {
        materials: handles,
        unlit,
        ..
    } = &mut *atlases;
    let (Some(clock), Some(catalog)) = (clock, catalog) else {
        if skyrim {
            for world in handles.keys() {
                warn_unlit(
                    &config,
                    unlit,
                    *world,
                    "no game clock or environment catalog",
                );
            }
        }
        return;
    };
    for (world, handle) in handles.iter() {
        let lighting = if skyrim {
            object_lighting(&catalog, &config, clock.hour, *world)
        } else {
            mudcrab_lighting(&catalog, clock.hour, *world)
        };
        let Some(lighting) = lighting else {
            if skyrim {
                warn_unlit(
                    &config,
                    unlit,
                    *world,
                    missing_lighting_reason(&catalog, *world),
                );
            }
            continue;
        };
        unlit.remove(world);
        if materials
            .get(handle)
            .is_some_and(|material| material.extension.lighting != lighting)
            && let Some(mut material) = materials.get_mut(handle)
        {
            material.extension.lighting = lighting;
        }
    }
}

/// Logs once per world that the `skyrim` object LOD has no light inputs.
///
/// There is no silent fallback. The material starts with zero rows and the retail shader has no
/// other light source, so a world that lacks the clock, the catalog, the default weather or a
/// climate sun timing draws its object LOD black until the inputs exist. Before the retail terms
/// the Bevy sun still lit such a mesh. Converted packs have climate timing, so this affects
/// fixtures, benchmarks and packs without environment tables.
fn warn_unlit(config: &EngineConfig, unlit: &mut HashSet<u32>, world: u32, reason: &str) {
    if unlit.insert(world) {
        warn!(
            world = %world_editor_id(&config.assets_dir, world),
            "object LOD is black under the skyrim profile: {reason}"
        );
    }
}

/// Why `object_lighting` has no rows for a world.
fn missing_lighting_reason(catalog: &EnvironmentCatalog, world: u32) -> &'static str {
    if catalog.weather(DEFAULT_WEATHER_FORM_ID).is_none() {
        "the default exterior weather is missing"
    } else if catalog
        .worldspace_climate(world)
        .and_then(|id| catalog.sun_timing(id))
        .is_none()
    {
        "the worldspace has no climate sun timing"
    } else {
        "no light inputs"
    }
}

/// The `mudcrab` profile's object LOD lighting, unchanged by the retail terms: the directional
/// ambient rows scaled by the engine ambient illuminance. The sun comes from the Bevy directional
/// light in the shader, and the shader multiplies by the view exposure. A missing input leaves the
/// rows unchanged, as before.
// STAND-IN(#983): use the engine's default exterior weather and light scale.
fn mudcrab_lighting(catalog: &EnvironmentCatalog, hour: f64, world: u32) -> Option<ObjectLighting> {
    let weather = catalog.weather(DEFAULT_WEATHER_FORM_ID)?;
    let timing = catalog
        .worldspace_climate(world)
        .and_then(|id| catalog.sun_timing(id))?;
    let mut lighting = ObjectLighting::for_profile(VisualProfile::Mudcrab);
    for (time, weight) in TimeOfDay::ALL.into_iter().zip(slot_weights(hour, timing)) {
        if let Some(dalc) = weather.directional_ambient.get(&time) {
            let cube = AmbientCube::from_dalc(dalc);
            let weighted = |rgb| Vec3::from_array(rgb).extend(0.0) * (weight * AMBIENT_ILLUMINANCE);
            lighting.mean += weighted(cube.mean);
            lighting.x += weighted(cube.half_difference_x);
            lighting.y += weighted(cube.half_difference_y);
            lighting.z += weighted(cube.half_difference_z);
        }
    }
    Some(lighting)
}

/// The retail directional ambient rows, sun RGB and sun direction for one worldspace and hour.
/// The shader has no fog input: the retail fog composite fogs the draw (`object_atlas_base`).
fn object_lighting(
    catalog: &EnvironmentCatalog,
    config: &EngineConfig,
    hour: f64,
    world: u32,
) -> Option<ObjectLighting> {
    let weather = catalog.weather(DEFAULT_WEATHER_FORM_ID)?;
    let timing = catalog
        .worldspace_climate(world)
        .and_then(|id| catalog.sun_timing(id))?;
    let mut lighting = ObjectLighting::for_profile(VisualProfile::Skyrim);
    let mut sunlight_scale = 0.0;
    // STAND-IN(#946): use the engine's single exterior weather and base image space.
    // Weather transitions, flashes, image-space overrides and modifiers are unmodeled.
    // Use the constructor default when the pack lacks a resolved IMSP slot.
    for (index, (time, weight)) in TimeOfDay::ALL
        .into_iter()
        .zip(slot_weights(hour, timing))
        .enumerate()
    {
        if weight == 0.0 {
            continue;
        }
        // skyrim_decomp#1003 G02. A day part without a DALC row adds nothing, so its share of the
        // ambient is dark. Retail's constructor default is white. The engine omits it instead of
        // inventing a row for a weather that lacks one (no invented data).
        if let Some(dalc) = weather.directional_ambient.get(&time) {
            let cube = AmbientCube::from_dalc(dalc);
            let weighted = |rgb| (Vec3::from_array(rgb) * weight).extend(0.0);
            lighting.mean += weighted(cube.mean);
            lighting.x += weighted(cube.half_difference_x);
            lighting.y += weighted(cube.half_difference_y);
            lighting.z += weighted(cube.half_difference_z);
        }
        // skyrim_decomp#1003 G03. A day part without a NAM0 sunlight row adds no sun.
        if let Some(sunlight) = weather.color("sunlight", time) {
            lighting.sun_rgb +=
                (Vec3::from_array(sunlight.map(f32::from)) * (weight / 255.0)).extend(0.0);
        }
        // A day part with no form takes the default image space (SPEC-1 section 2.4).
        let form = weather.image_spaces[index]
            .filter(|id| *id != 0)
            .unwrap_or(crate::retail_hdr::DEFAULT_IMAGE_SPACE_FORM);
        sunlight_scale += weight
            * catalog
                .image_space(form)
                .and_then(|image_space| image_space.sunlight_scale)
                .unwrap_or(DEFAULT_SUNLIGHT_SCALE);
    }
    // STAND-IN(#946): use the initialized sun fade of 1 and DALC intercept operand of 0.
    // The effective fade and any runtime writes to the intercept operand are unverified.
    // The colour and the scale are each blended by the day-part weights, then multiplied. The
    // product of the two sums differs from the sum of the products. Retail takes the scale from
    // the active image space after its own blend (G03). Whether the two agree is unverified.
    lighting.sun_rgb *= sunlight_scale;
    let direction = world_sun_direction(config, hour, Some(catalog), world);
    lighting.sun_direction = if config.sun_position.is_some() {
        crate::app::sun_rotation(config, direction) * Vec3::Z
    } else {
        direction.unwrap_or(Vec3::ZERO)
    }
    .extend(0.0);
    Some(lighting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::environment::{DirectionalAmbient, ImageSpace, Weather};
    use bevy::{
        asset::{AssetPlugin, RenderAssetUsages},
        ecs::schedule::SingleThreadedExecutor,
        log::tracing_subscriber::{Layer, Registry, layer::Context, prelude::*},
        render::render_resource::PrimitiveTopology,
    };
    use shared::lod::{ChunkAnchor, ChunkKey, LodTier};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Warnings(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> Layer<S> for Warnings {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            if *event.metadata().level() == tracing::Level::WARN
                && event.metadata().target() == module_path!().trim_end_matches("::tests")
            {
                event.record(
                    &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                        if matches!(field.name(), "world" | "message") {
                            self.0.lock().unwrap().push(format!("{value:?}"));
                        }
                    },
                );
            }
        }
    }

    fn test_app(pack: &std::path::Path) -> App {
        let db = rusqlite::Connection::open(pack.join("skyrim_world.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE worldspaces(id INTEGER PRIMARY KEY, editor_id TEXT);
             INSERT INTO worldspaces VALUES (1, 'FixtureWorld');",
        )
        .unwrap();
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .insert_resource(EngineConfig {
                assets_dir: pack.to_owned(),
                ..default()
            })
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .init_asset::<ObjectMaterial>()
            .init_resource::<Atlases>()
            .add_systems(Update, (queue_object_atlases, apply_object_atlases).chain())
            .edit_schedule(Update, |schedule| {
                schedule.set_executor(SingleThreadedExecutor::new());
            });
        app
    }

    fn add_chunk(app: &mut App, has_basis: bool) -> Entity {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        if has_basis {
            mesh.insert_attribute(RED_AXIS, vec![[1.0f32, 0.0, 0.0]]);
            mesh.insert_attribute(GREEN_AXIS, vec![[0.0f32, 1.0, 0.0]]);
        }
        let world = app.world_mut();
        let mesh = world.resource_mut::<Assets<Mesh>>().add(mesh);
        let diffuse = world.resource_mut::<Assets<Image>>().add(Image::default());
        let material = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color_texture: Some(diffuse),
                ..default()
            });
        let root = world
            .spawn((
                LodObjectRoot {
                    key: ChunkKey::new(1, LodTier::Tier16, ChunkAnchor { x: 0, y: 0 }),
                },
                ObjectSceneReady,
            ))
            .id();
        world
            .spawn((Mesh3d(mesh), MeshMaterial3d(material), ChildOf(root)))
            .id()
    }

    #[test]
    fn a_missing_atlas_and_old_chunks_keep_the_standard_material_and_warn_once() {
        for (version, reason) in [
            (5, "chunks built before compiler 6"),
            (6, "missing_normal_atlas"),
        ] {
            let pack = tempfile::tempdir().unwrap();
            std::fs::write(
                pack.path().join("lod-manifest.json"),
                format!(r#"{{"objects_compiler_version":{version}}}"#),
            )
            .unwrap();
            let mut app = test_app(pack.path());
            let warnings = Warnings::default();
            tracing::subscriber::with_default(Registry::default().with(warnings.clone()), || {
                for _ in 0..2 {
                    let piece = add_chunk(&mut app, true);
                    app.update();
                    assert!(
                        app.world()
                            .get::<MeshMaterial3d<StandardMaterial>>(piece)
                            .is_some()
                    );
                }
            });
            let messages = warnings.0.lock().unwrap();
            assert_eq!(
                messages.iter().filter(|text| text.contains(reason)).count(),
                1,
                "{messages:?}"
            );
            assert_eq!(
                messages
                    .iter()
                    .filter(|text| text.contains("FixtureWorld"))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn a_failed_normal_load_keeps_the_standard_material_and_warns_once() {
        let pack = tempfile::tempdir().unwrap();
        let mut app = test_app(pack.path());
        let pieces = [add_chunk(&mut app, true), add_chunk(&mut app, true)];
        let server = app.world().resource::<AssetServer>().clone();
        let normal = server.load::<Image>("missing-object-lod-normal-for-test.ktx2");
        {
            let mut atlases = app.world_mut().resource_mut::<Atlases>();
            atlases.pack = pack.path().to_owned();
            atlases.normals.insert(1, Some(normal));
        }

        let warnings = Warnings::default();
        tracing::subscriber::with_default(Registry::default().with(warnings.clone()), || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while app.world().resource::<Atlases>().normals[&1].is_some() {
                app.update();
                assert!(
                    std::time::Instant::now() < deadline,
                    "normal load did not fail"
                );
                std::thread::yield_now();
            }
        });
        for piece in pieces {
            assert!(
                app.world()
                    .get::<MeshMaterial3d<StandardMaterial>>(piece)
                    .is_some()
            );
        }
        let messages = warnings.0.lock().unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|text| text.contains("cannot load the normal atlas"))
                .count(),
            1,
            "{messages:?}"
        );
    }

    #[test]
    fn material_swap_waits_for_the_image_and_keeps_pieces_without_a_basis() {
        let pack = tempfile::tempdir().unwrap();
        let mut app = test_app(pack.path());
        let ready = add_chunk(&mut app, true);
        let without_basis = [add_chunk(&mut app, false), add_chunk(&mut app, false)];
        let normal = app.world().resource::<Assets<Image>>().reserve_handle();
        {
            let mut atlases = app.world_mut().resource_mut::<Atlases>();
            atlases.pack = pack.path().to_owned();
            atlases.normals.insert(1, Some(normal.clone()));
        }
        app.update();
        assert!(
            app.world()
                .get::<MeshMaterial3d<StandardMaterial>>(ready)
                .is_some()
        );
        app.world_mut()
            .resource_mut::<Assets<Image>>()
            .insert(
                normal.id(),
                Image {
                    sampler: atlas_sampler(),
                    ..default()
                },
            )
            .unwrap();

        let warnings = Warnings::default();
        tracing::subscriber::with_default(Registry::default().with(warnings.clone()), || {
            app.update()
        });
        let world = app.world();
        assert!(
            world
                .get::<MeshMaterial3d<StandardMaterial>>(ready)
                .is_none()
        );
        let handle = world.get::<MeshMaterial3d<ObjectMaterial>>(ready).unwrap();
        let material = world
            .resource::<Assets<ObjectMaterial>>()
            .get(&handle.0)
            .unwrap();
        assert_eq!(material.extension.normal, normal);
        let images = world.resource::<Assets<Image>>();
        let diffuse = images
            .get(material.base.base_color_texture.as_ref().unwrap())
            .unwrap();
        assert_eq!(diffuse.sampler, images.get(&normal).unwrap().sampler);
        assert_eq!(diffuse.sampler, atlas_sampler());
        for piece in without_basis {
            assert!(
                world
                    .get::<MeshMaterial3d<StandardMaterial>>(piece)
                    .is_some()
            );
        }
        let messages = warnings.0.lock().unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|text| text.contains("no normal atlas basis"))
                .count(),
            1,
            "{messages:?}"
        );
    }

    /// The composite fogs a draw only in the opaque, alpha-mask or alpha-to-coverage phase without
    /// transmission (`object_atlas_base`). A chunk material outside that set is repaired, and one
    /// inside it keeps its mode.
    #[test]
    fn the_atlas_material_stays_in_the_opaque_pass_without_transmission() {
        for mode in [
            AlphaMode::Opaque,
            AlphaMode::Mask(0.5),
            AlphaMode::AlphaToCoverage,
        ] {
            let base = object_atlas_base(&StandardMaterial {
                alpha_mode: mode,
                ..default()
            });
            assert_eq!(base.alpha_mode, mode);
        }
        for mode in [
            AlphaMode::Blend,
            AlphaMode::Premultiplied,
            AlphaMode::Add,
            AlphaMode::Multiply,
        ] {
            let base = object_atlas_base(&StandardMaterial {
                alpha_mode: mode,
                specular_transmission: 0.5,
                diffuse_transmission: 0.25,
                ..default()
            });
            assert_eq!(base.alpha_mode, AlphaMode::Opaque, "{mode:?}");
            assert_eq!(base.specular_transmission, 0.0);
            assert_eq!(base.diffuse_transmission, 0.0);
        }
        assert!(matches!(
            object_atlas_base(&default()).opaque_render_method,
            OpaqueRendererMethod::Forward
        ));
    }

    /// The material that `apply_object_atlases` builds keeps a `MASK` chunk in the alpha-mask pass,
    /// and starts with the profile flag the shader branches on.
    #[test]
    fn the_applied_object_material_is_in_the_pass_the_composite_fogs() {
        for (profile, flag) in [(VisualProfile::Mudcrab, 0.0), (VisualProfile::Skyrim, 1.0)] {
            let pack = tempfile::tempdir().unwrap();
            let mut app = test_app(pack.path());
            app.world_mut()
                .resource_mut::<EngineConfig>()
                .visual_profile = profile;
            let piece = add_chunk(&mut app, true);
            let standard = app
                .world()
                .get::<MeshMaterial3d<StandardMaterial>>(piece)
                .unwrap()
                .0
                .clone();
            app.world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .get_mut(&standard)
                .unwrap()
                .alpha_mode = AlphaMode::Mask(0.5);
            let normal = app.world().resource::<Assets<Image>>().reserve_handle();
            {
                let mut atlases = app.world_mut().resource_mut::<Atlases>();
                atlases.pack = pack.path().to_owned();
                atlases.normals.insert(1, Some(normal.clone()));
            }
            app.update();
            app.world_mut()
                .resource_mut::<Assets<Image>>()
                .insert(
                    normal.id(),
                    Image {
                        sampler: atlas_sampler(),
                        ..default()
                    },
                )
                .unwrap();
            app.update();
            let handle = app
                .world()
                .get::<MeshMaterial3d<ObjectMaterial>>(piece)
                .unwrap();
            let material = app
                .world()
                .resource::<Assets<ObjectMaterial>>()
                .get(&handle.0)
                .unwrap();
            assert_eq!(material.base.alpha_mode, AlphaMode::Mask(0.5));
            assert_eq!(material.base.specular_transmission, 0.0);
            assert_eq!(material.base.diffuse_transmission, 0.0);
            assert_eq!(material.extension.lighting.profile.x, flag, "{profile:?}");
        }
    }

    // The lighting fixture. Red face bytes 51,153,0,204,102,255 give mean 0.5 and slopes 0.2,
    // 0.4 and 0.3. Green is all 255 (mean 1, no slope). Blue is all 0.
    const WORLD: u32 = 7;
    const TIMING: crate::sun::SunTiming = crate::sun::SunTiming([33, 60, 96, 123]);
    const DAY_SPACE: u32 = 0x77;
    const SUNSET_SPACE: u32 = 0x78;

    fn dalc(red: [u8; 6], green: [u8; 6], blue: [u8; 6]) -> DirectionalAmbient {
        DirectionalAmbient {
            colors: std::array::from_fn(|face| [red[face], green[face], blue[face]]),
            ..default()
        }
    }

    fn day_weather() -> Weather {
        let mut weather = Weather::default();
        weather.directional_ambient.insert(
            TimeOfDay::Day,
            dalc([51, 153, 0, 204, 102, 255], [255; 6], [0; 6]),
        );
        weather
            .colors
            .insert(("sunlight".to_owned(), TimeOfDay::Day), [255, 51, 0]);
        weather
    }

    fn space(sunlight_scale: f32) -> ImageSpace {
        ImageSpace {
            sunlight_scale: Some(sunlight_scale),
            ..default()
        }
    }

    fn catalog(weather: Weather, spaces: Vec<(u32, ImageSpace)>) -> EnvironmentCatalog {
        EnvironmentCatalog::with_sun_for_tests(
            WORLD,
            1,
            TIMING,
            [
                Some(400.0),
                Some(40.0),
                Some(-150.0),
                Some(200.0),
                Some(0.5),
            ],
        )
        .with_weather_for_tests(DEFAULT_WEATHER_FORM_ID, weather, spaces)
    }

    fn assert_near(actual: Vec4, expected: [f32; 4]) {
        for (a, e) in actual.to_array().into_iter().zip(expected) {
            assert!(
                (a - e).abs() <= 1.0e-5 * e.abs().max(1.0),
                "{actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn noon_lighting_is_the_day_dalc_rows_and_the_image_space_sun() {
        let mut weather = day_weather();
        weather.image_spaces[1] = Some(DAY_SPACE);
        let catalog = catalog(weather, vec![(DAY_SPACE, space(2.5))]);
        let config = EngineConfig::default();
        let lighting = object_lighting(&catalog, &config, 12.0, WORLD).unwrap();
        // Mean is the face average. Slopes are half the face differences, unclamped and unscaled.
        assert_near(lighting.mean, [0.5, 1.0, 0.0, 0.0]);
        assert_near(lighting.x, [0.2, 0.0, 0.0, 0.0]);
        assert_near(lighting.y, [0.4, 0.0, 0.0, 0.0]);
        assert_near(lighting.z, [0.3, 0.0, 0.0, 0.0]);
        // Sun RGB is the NAM0 row over 255 times the image-space scale. No engine multiplier.
        assert_near(lighting.sun_rgb, [2.5, 0.5, 0.0, 0.0]);
        let sun = world_sun_direction(&config, 12.0, Some(&catalog), WORLD).unwrap();
        assert_near(lighting.sun_direction, [sun.x, sun.y, sun.z, 0.0]);
        assert!(sun.y > 0.0, "the noon sun is above the horizon: {sun:?}");
    }

    #[test]
    fn a_fixed_sun_position_gives_the_rotated_to_sun_vector() {
        let catalog = catalog(day_weather(), Vec::new());
        let config = EngineConfig {
            sun_position: Some((90.0, 35.0)),
            ..default()
        };
        let lighting = object_lighting(&catalog, &config, 12.0, WORLD).unwrap();
        let expected = crate::app::sun_rotation(&config, None) * Vec3::Z;
        assert_near(
            lighting.sun_direction,
            [expected.x, expected.y, expected.z, 0.0],
        );
        assert!((expected.y - 35.0_f32.to_radians().sin()).abs() < 1.0e-4);
    }

    #[test]
    fn a_missing_image_space_uses_the_labelled_constructor_default() {
        let lighting = object_lighting(
            &catalog(day_weather(), Vec::new()),
            &EngineConfig::default(),
            12.0,
            WORLD,
        )
        .unwrap();
        assert_near(
            lighting.sun_rgb,
            [
                DEFAULT_SUNLIGHT_SCALE,
                0.2 * DEFAULT_SUNLIGHT_SCALE,
                0.0,
                0.0,
            ],
        );
    }

    #[test]
    fn the_day_part_blend_weights_rows_colour_and_scale_separately() {
        let hour = 17.0;
        let weights = slot_weights(hour, TIMING);
        let (day, sunset) = (weights[1], weights[2]);
        assert!(
            day > 0.0 && sunset > 0.0,
            "day and sunset share the blend at hour {hour}: {weights:?}"
        );
        let mut weather = day_weather();
        weather
            .directional_ambient
            .insert(TimeOfDay::Sunset, dalc([102; 6], [0; 6], [255; 6]));
        weather
            .colors
            .insert(("sunlight".to_owned(), TimeOfDay::Sunset), [0, 255, 51]);
        weather.image_spaces[1] = Some(DAY_SPACE);
        weather.image_spaces[2] = Some(SUNSET_SPACE);
        let catalog = catalog(
            weather,
            vec![(DAY_SPACE, space(3.0)), (SUNSET_SPACE, space(1.0))],
        );
        let lighting = object_lighting(&catalog, &EngineConfig::default(), hour, WORLD).unwrap();
        // The weights sum to 1. Day mean is (0.5, 1, 0). Sunset mean is (0.4, 0, 1).
        assert_near(lighting.mean, [0.5 * day + 0.4 * sunset, day, sunset, 0.0]);
        assert_near(lighting.x, [0.2 * day, 0.0, 0.0, 0.0]);
        // Colour and scale are blended separately, then multiplied: (sum w c) * (sum w s).
        let scale = 3.0 * day + 1.0 * sunset;
        assert_near(
            lighting.sun_rgb,
            [
                day * scale,
                (0.2 * day + sunset) * scale,
                0.2 * sunset * scale,
                0.0,
            ],
        );
    }

    #[test]
    fn a_missing_dalc_row_or_sun_colour_adds_nothing() {
        let mut weather = day_weather();
        weather.directional_ambient.clear();
        weather.colors.clear();
        let lighting = object_lighting(
            &catalog(weather, Vec::new()),
            &EngineConfig::default(),
            12.0,
            WORLD,
        )
        .unwrap();
        for value in [
            lighting.mean,
            lighting.x,
            lighting.y,
            lighting.z,
            lighting.sun_rgb,
        ] {
            assert_eq!(value, Vec4::ZERO);
        }
    }

    #[test]
    fn lighting_needs_the_default_weather_and_a_worldspace_climate() {
        let config = EngineConfig::default();
        let catalog = catalog(day_weather(), Vec::new());
        assert!(object_lighting(&catalog, &config, 12.0, WORLD + 1).is_none());
        assert!(object_lighting(&EnvironmentCatalog::default(), &config, 12.0, WORLD).is_none());
    }

    /// Builds an app with the lighting system, one world material and the given profile.
    fn lighting_app(
        profile: VisualProfile,
        catalog: Option<EnvironmentCatalog>,
    ) -> (App, Handle<ObjectMaterial>) {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .init_asset::<ObjectMaterial>()
            .init_resource::<Atlases>()
            .insert_resource(EngineConfig {
                visual_profile: profile,
                ..default()
            })
            .insert_resource(GameClock {
                hour: 12.0,
                timescale: 0.0,
            })
            .add_systems(Update, update_object_lighting)
            .edit_schedule(Update, |schedule| {
                schedule.set_executor(SingleThreadedExecutor::new());
            });
        if let Some(catalog) = catalog {
            app.insert_resource(catalog);
        }
        let handle = app
            .world_mut()
            .resource_mut::<Assets<ObjectMaterial>>()
            .add(ObjectMaterial {
                base: StandardMaterial::default(),
                extension: ObjectAtlas {
                    normal: Handle::default(),
                    lighting: ObjectLighting::for_profile(profile),
                },
            });
        app.world_mut()
            .resource_mut::<Atlases>()
            .materials
            .insert(WORLD, handle.clone());
        (app, handle)
    }

    fn lighting_of(app: &App, handle: &Handle<ObjectMaterial>) -> ObjectLighting {
        app.world()
            .resource::<Assets<ObjectMaterial>>()
            .get(handle)
            .unwrap()
            .extension
            .lighting
    }

    /// Under `mudcrab` the rows are the old ones: raw rows times the engine ambient illuminance,
    /// no retail sun colour or direction, and the flag stays off so the shader keeps the engine
    /// light scale, the exposure and the Bevy sun.
    #[test]
    fn the_mudcrab_profile_keeps_the_engine_light_scale() {
        let mut weather = day_weather();
        weather.image_spaces[1] = Some(DAY_SPACE);
        let catalog = catalog(weather, vec![(DAY_SPACE, space(2.5))]);
        let (mut app, handle) = lighting_app(VisualProfile::Mudcrab, Some(catalog));
        app.update();
        let lighting = lighting_of(&app, &handle);
        assert_near(
            lighting.mean,
            [
                0.5 * AMBIENT_ILLUMINANCE,
                1.0 * AMBIENT_ILLUMINANCE,
                0.0,
                0.0,
            ],
        );
        assert_near(lighting.x, [0.2 * AMBIENT_ILLUMINANCE, 0.0, 0.0, 0.0]);
        assert_near(lighting.y, [0.4 * AMBIENT_ILLUMINANCE, 0.0, 0.0, 0.0]);
        assert_near(lighting.z, [0.3 * AMBIENT_ILLUMINANCE, 0.0, 0.0, 0.0]);
        assert_eq!(lighting.sun_rgb, Vec4::ZERO);
        assert_eq!(lighting.sun_direction, Vec4::ZERO);
        assert_eq!(lighting.profile.x, 0.0);
    }

    /// Under `skyrim` the same weather gives the retail rows with the flag on.
    #[test]
    fn the_skyrim_profile_writes_the_retail_rows_with_the_flag_on() {
        let mut weather = day_weather();
        weather.image_spaces[1] = Some(DAY_SPACE);
        let catalog = catalog(weather, vec![(DAY_SPACE, space(2.5))]);
        let (mut app, handle) = lighting_app(VisualProfile::Skyrim, Some(catalog));
        app.update();
        let lighting = lighting_of(&app, &handle);
        assert_near(lighting.mean, [0.5, 1.0, 0.0, 0.0]);
        assert_near(lighting.sun_rgb, [2.5, 0.5, 0.0, 0.0]);
        assert_eq!(lighting.profile.x, 1.0);
    }

    /// A `mudcrab` world without lighting inputs is unchanged and silent, as before.
    #[test]
    fn the_mudcrab_profile_stays_silent_without_inputs() {
        let (mut app, handle) = lighting_app(VisualProfile::Mudcrab, None);
        let warnings = Warnings::default();
        tracing::subscriber::with_default(Registry::default().with(warnings.clone()), || {
            app.update();
            app.update();
        });
        assert!(warnings.0.lock().unwrap().is_empty());
        assert_eq!(
            lighting_of(&app, &handle),
            ObjectLighting::for_profile(VisualProfile::Mudcrab)
        );
    }

    /// A `skyrim` world without the clock, the catalog, the weather or the climate is black and
    /// says so once, however many frames pass.
    #[test]
    fn a_skyrim_world_without_inputs_is_black_and_logged_once() {
        let cases = [
            (None, "no game clock or environment catalog"),
            (
                Some(EnvironmentCatalog::default()),
                "the default exterior weather is missing",
            ),
            (
                Some(EnvironmentCatalog::default().with_weather_for_tests(
                    DEFAULT_WEATHER_FORM_ID,
                    day_weather(),
                    Vec::new(),
                )),
                "the worldspace has no climate sun timing",
            ),
        ];
        for (catalog, reason) in cases {
            let (mut app, handle) = lighting_app(VisualProfile::Skyrim, catalog);
            let warnings = Warnings::default();
            tracing::subscriber::with_default(Registry::default().with(warnings.clone()), || {
                for _ in 0..3 {
                    app.update();
                }
            });
            let messages = warnings.0.lock().unwrap();
            assert_eq!(
                messages.iter().filter(|text| text.contains(reason)).count(),
                1,
                "{reason}: {messages:?}"
            );
            let lighting = lighting_of(&app, &handle);
            assert_eq!(lighting.profile.x, 1.0);
            for value in [
                lighting.mean,
                lighting.x,
                lighting.y,
                lighting.z,
                lighting.sun_rgb,
            ] {
                assert_eq!(value, Vec4::ZERO, "{reason}");
            }
        }
    }
}
