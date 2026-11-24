//! Object LOD material for chunks with a shared normal atlas.

use super::lod_objects::{LodObjectRoot, ObjectSceneReady};
use crate::{
    config::EngineConfig,
    sky::SkyPalette,
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
// https://github.com/Mudcrab-Team/skyrim_decomp/pull/1003 G03 and G05.
// Ultra.ini and Skyrim_Default.ini supply neither setting. SunlightScale is
// authored in IMGS.HNAM. The clamp is the compiled fLightingOutputColourClampPostLit default.
const DEFAULT_SUNLIGHT_SCALE: f32 = 1.8;
const POST_LIT_CLAMP: f32 = 1.0;
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
    fog_ramp: Vec4,
    fog_near: Vec4,
    fog_far: Vec4,
    output_controls: Vec4,
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
    palette: Option<Res<SkyPalette>>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
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
        let mut lighting = ObjectLighting::default();
        let mut sunlight_scale = 0.0;
        // STAND-IN(#946): use the engine's single exterior weather and base image space.
        // Weather transitions, flashes, image-space overrides and modifiers are unmodeled.
        // Use the constructor default when the pack lacks a resolved IMSP slot.
        for (index, (time, weight)) in TimeOfDay::ALL
            .into_iter()
            .zip(slot_weights(clock.hour, timing))
            .enumerate()
        {
            if weight == 0.0 {
                continue;
            }
            // https://github.com/Mudcrab-Team/skyrim_decomp/pull/1003 G02 and G03.
            if let Some(dalc) = weather.directional_ambient.get(&time) {
                let cube = AmbientCube::from_dalc(dalc);
                let weighted = |rgb| (Vec3::from_array(rgb) * weight).extend(0.0);
                lighting.mean += weighted(cube.mean);
                lighting.x += weighted(cube.half_difference_x);
                lighting.y += weighted(cube.half_difference_y);
                lighting.z += weighted(cube.half_difference_z);
            }
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
        lighting.sun_rgb *= sunlight_scale;
        let direction = world_sun_direction(&config, clock.hour, Some(&catalog), *world);
        lighting.sun_direction = if config.sun_position.is_some() {
            crate::app::sun_rotation(&config, direction) * Vec3::Z
        } else {
            direction.unwrap_or(Vec3::ZERO)
        }
        .extend(0.0);

        // STAND-IN(#946): use the engine's active sky palette and density preference.
        // The retail scene fog property and exact projection are unverified at runtime.
        if let Some(palette) = palette.as_deref()
            && let Some(ramp) = palette.fog_ramp
        {
            let scale = (palette.fog_density_scale
                * settings.as_deref().map_or(1.0, |s| s.fog_density_scale))
            .max(1.0e-3);
            let (near, far) = (ramp.near / scale, ramp.far / scale);
            let width = far - near;
            if width.is_finite() && width > 0.0 && ramp.power > 0.0 {
                lighting.fog_ramp = Vec4::new(near / width, 1.0 / width, ramp.power, ramp.max);
                let rgb = |color: Color| {
                    let color = color.to_srgba();
                    Vec4::new(color.red, color.green, color.blue, 0.0)
                };
                lighting.fog_near = rgb(palette.fog_near);
                lighting.fog_far = rgb(palette.fog_far);
            }
        }
        // STAND-IN(#946): use the initialized frame flags S=1, T=0.
        // The active frame-flag inputs at the distant pose are unverified.
        lighting.output_controls = Vec4::new(
            crate::output_scale::framebuffer_output_scale(true),
            POST_LIT_CLAMP,
            1.0,
            0.0,
        );

        if materials
            .get(handle)
            .is_some_and(|material| material.extension.lighting != lighting)
            && let Some(mut material) = materials.get_mut(handle)
        {
            material.extension.lighting = lighting;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
