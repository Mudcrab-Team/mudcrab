//! Object LOD material for chunks with a shared normal atlas.

use super::lod_objects::{LodObjectRoot, ObjectSceneReady};
use crate::{
    config::EngineConfig,
    sun::{GameClock, world_sun_direction},
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
// Skyrim_Default.ini sets it. The constructor default is "about 1.8" in G03. The exact float is
// unverified, so this value is a STAND-IN(#946) for that default. It applies only when the
// pack's image space for a day part has no sunlight scale. The engine's near path uses its own
// `SUNLIGHT_SCALE` (2.8) for the directional light. That constant does not feed this material.
const DEFAULT_SUNLIGHT_SCALE: f32 = 1.8;
const SHADER: &str = "embedded://engine/streaming/lod_object.wgsl";

type ObjectMaterial = ExtendedMaterial<StandardMaterial, ObjectAtlas>;

#[derive(Clone, Copy, Default, PartialEq, ShaderType)]
struct ObjectLighting {
    mean: Vec4,
    x: Vec4,
    y: Vec4,
    z: Vec4,
    sun_direction: Vec4,
    sun_rgb: Vec4,
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
        // STAND-IN(#940): the retail missing-file result is unverified. Keep the existing material.
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
                    base: StandardMaterial {
                        opaque_render_method: OpaqueRendererMethod::Forward,
                        ..base.clone()
                    },
                    extension: ObjectAtlas {
                        normal: image.clone(),
                        lighting: ObjectLighting::default(),
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

fn update_object_lighting(
    clock: Option<Res<GameClock>>,
    catalog: Option<Res<EnvironmentCatalog>>,
    config: Res<EngineConfig>,
    mut materials: ResMut<Assets<ObjectMaterial>>,
    atlases: Res<Atlases>,
) {
    let (Some(clock), Some(catalog)) = (clock, catalog) else {
        return;
    };
    for (world, handle) in &atlases.materials {
        let Some(lighting) = object_lighting(&catalog, &config, clock.hour, *world) else {
            continue;
        };
        if materials
            .get(handle)
            .is_some_and(|material| material.extension.lighting != lighting)
            && let Some(mut material) = materials.get_mut(handle)
        {
            material.extension.lighting = lighting;
        }
    }
}

/// The retail directional ambient rows, sun RGB and sun direction for one worldspace and hour.
/// Fog and output use Bevy's distance fog in the shader, so no fog input is built here.
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
    let mut lighting = ObjectLighting::default();
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
        sunlight_scale += weight
            * weather.image_spaces[index]
                .and_then(|id| catalog.image_space(id))
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
            atlases.normals.insert(1, Some(normal.clone()));
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
            assert!((a - e).abs() < 1.0e-5, "{actual:?} != {expected:?}");
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

    #[test]
    fn the_system_writes_the_lighting_into_the_world_material() {
        let mut weather = day_weather();
        weather.image_spaces[1] = Some(DAY_SPACE);
        let catalog = catalog(weather, vec![(DAY_SPACE, space(2.5))]);
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .init_asset::<ObjectMaterial>()
            .init_resource::<Atlases>()
            .insert_resource(EngineConfig::default())
            .insert_resource(GameClock {
                hour: 12.0,
                timescale: 0.0,
            })
            .insert_resource(catalog)
            .add_systems(Update, update_object_lighting);
        let handle = app
            .world_mut()
            .resource_mut::<Assets<ObjectMaterial>>()
            .add(ObjectMaterial {
                base: StandardMaterial::default(),
                extension: ObjectAtlas {
                    normal: Handle::default(),
                    lighting: ObjectLighting::default(),
                },
            });
        app.world_mut()
            .resource_mut::<Atlases>()
            .materials
            .insert(WORLD, handle.clone());
        app.update();
        let materials = app.world().resource::<Assets<ObjectMaterial>>();
        let lighting = materials.get(&handle).unwrap().extension.lighting;
        assert_near(lighting.mean, [0.5, 1.0, 0.0, 0.0]);
        assert_near(lighting.sun_rgb, [2.5, 0.5, 0.0, 0.0]);
    }
}
