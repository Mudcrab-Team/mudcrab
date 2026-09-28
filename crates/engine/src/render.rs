use crate::{
    profiling::ProfilingState,
    world::{cache::TerrainSnapshot, database::AssetCatalog},
};
use bevy::{
    app::{HierarchyPropagatePlugin, PropagateSet},
    asset::embedded_asset,
    camera::{
        RenderTarget,
        primitives::{Aabb, Frustum},
        visibility::{Layer, RenderLayers, VisibilitySystems},
    },
    core_pipeline::{mip_generation::experimental::depth::ViewDepthPyramid, prepass::DepthPrepass},
    image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor},
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        batching::gpu_preprocessing::{
            GpuPreprocessingMode, GpuPreprocessingSupport, IndirectParametersBuffers,
        },
        occlusion_culling::OcclusionCulling,
        render_resource::{AsBindGroup, ShaderType},
    },
    shader::ShaderRef,
};
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainExtension>;
pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>;

fn repeating_terrain_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::linear()
    })
}

/// How far the procedural waves tilt the water's normal. Skyrim's water is close to flat at a
/// distance; a stronger tilt striped lakes with bright and dark bands.
const WAVE_STRENGTH: f32 = 0.05;

/// Skyrim's DefaultWater after Update.esm: Fresnel Amount 0.10 and a Reflectivity Amount of 0.8
/// (WATR `DNAM`). Used when a water has no decoded colours yet, e.g. a database converted before
/// `crates/converter` started reading them.
pub const DEFAULT_WATER_FRESNEL: f32 = 0.10;
pub const DEFAULT_WATER_REFLECTIVITY: f32 = 0.8;

/// One rendering layer per role, so no view list is a bare literal.
///
/// Layer 0 is Bevy's default and holds the world itself: terrain today, and - when they land - the
/// distant LOD land, LOD objects, LOD trees and the sky. Water surfaces take their own layer so a
/// water plane can never reflect another one. Full-detail placed objects take a third layer so the
/// reflection pass can leave them out; see [`REFLECTION_VIEW_LAYERS`] for why.
pub const WORLD_LAYER: Layer = 0;
pub const WATER_LAYER: Layer = 1;
pub const PLACED_OBJECT_LAYER: Layer = 2;

/// Everything the player's own cameras draw: terrain, water and the placed objects.
pub const MAIN_VIEW_LAYERS: &[Layer] = &[WORLD_LAYER, WATER_LAYER, PLACED_OBJECT_LAYER];

/// What the water reflection camera draws.
///
/// Vanilla Skyrim builds its water reflection from the LOD world - LOD land, LOD objects and LOD
/// trees - plus the sky, selected by the `[Water]` settings `bReflectLODLand`, `bReflectLODObjects`,
/// `bReflectLODTrees` and `bReflectSky`; full-detail placed objects, actors and grass are not in it
/// (SE adds screen-space reflections for near geometry, which this engine does not have). This
/// engine has no LOD and no sky yet, so what vanilla reflects is the terrain: the world layer only.
/// LOD and the sky join [`WORLD_LAYER`] when they land, and a full-detail object stays out of the
/// reflection because it is on [`PLACED_OBJECT_LAYER`].
pub const REFLECTION_VIEW_LAYERS: &[Layer] = &[WORLD_LAYER];

/// What every scene light belongs to: the sun today, and any point light a cell adds later.
///
/// A light must intersect a view's layers to light it, so this has to keep [`WORLD_LAYER`] - both
/// cameras render it. It also has to include [`PLACED_OBJECT_LAYER`], because a light only collects
/// shadow casters that share a layer with it (`check_dir_light_mesh_visibility`,
/// `bevy_light-0.19.0/src/lib.rs:423`). Dropping the placed-object layer would light the objects
/// while silently removing them from the sun's shadow cascades.
pub const LIGHT_LAYERS: &[Layer] = &[WORLD_LAYER, PLACED_OBJECT_LAYER];

/// The value [`Propagate`](bevy::app::Propagate) copies onto the meshes below a placed-object
/// reference, which is what keeps those meshes out of the reflection pass.
pub const PLACED_OBJECT_RENDER_LAYERS: RenderLayers = RenderLayers::layer(PLACED_OBJECT_LAYER);

/// Registers the propagation of [`PLACED_OBJECT_RENDER_LAYERS`] down reference hierarchies.
///
/// A reference entity is spawned in `streaming::spawn_cell` and the meshes it draws arrive later as
/// descendants from the converted glb, so `RenderLayers` on the reference alone would not reach
/// them. The propagation runs in `PostUpdate` before [`VisibilitySystems::CheckVisibility`]:
/// visibility compares render layers after that, and the glTF scene children are spawned in
/// `SpawnScene` between `Update` and `PostUpdate`, so the layers are in place in the first frame a
/// mesh exists - including for descendants spawned after the reference.
pub fn add_placed_object_layer_propagation(app: &mut App) {
    app.add_plugins(HierarchyPropagatePlugin::<RenderLayers>::new(PostUpdate))
        .configure_sets(
            PostUpdate,
            PropagateSet::<RenderLayers>::default().before(VisibilitySystems::CheckVisibility),
        );
}

pub struct VercidiumRendererPlugin;

impl Plugin for VercidiumRendererPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/terrain.wgsl");
        embedded_asset!(app, "shaders/water.wgsl");
        app.add_plugins((
            MaterialPlugin::<TerrainMaterial>::default(),
            MaterialPlugin::<WaterMaterial>::default(),
        ))
        .init_resource::<RendererMetrics>()
        .add_systems(Startup, setup_water_reflection)
        .add_systems(
            Update,
            (
                animate_water_materials,
                update_water_reflection_camera,
                sync_renderer_metrics,
            ),
        );

        add_placed_object_layer_propagation(app);

        let bridge = RendererProofBridge::default();
        app.insert_resource(bridge.clone());
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.insert_resource(bridge).add_systems(
                Render,
                sample_renderer_path.after(RenderSystems::PrepareResourcesCollectPhaseBuffers),
            );
        }
    }
}

#[derive(Resource, Debug, Clone, Default, Serialize)]
pub struct RendererMetrics {
    pub gpu_preprocessing_active: bool,
    pub gpu_culling_active: bool,
    pub indirect_drawing_active: bool,
    pub occlusion_culling_views: u64,
    pub hzb_views: u64,
    pub indirect_phase_buffers: u64,
    pub indirect_batch_sets: u64,
    pub proof_frames: u64,
    pub renderer_fixture_validated: bool,
    pub renderer_validation_failures: u64,
}

impl RendererMetrics {
    pub fn final_path_active(&self) -> bool {
        self.gpu_preprocessing_active
            && self.gpu_culling_active
            && self.indirect_drawing_active
            && self.occlusion_culling_views > 0
            && self.hzb_views > 0
            && self.indirect_phase_buffers > 0
            && self.indirect_batch_sets > 0
            && self.proof_frames > 0
            && self.renderer_validation_failures == 0
    }
}

#[derive(Default)]
struct RendererProofState {
    gpu_preprocessing: AtomicBool,
    gpu_culling: AtomicBool,
    indirect_drawing: AtomicBool,
    occlusion_views: AtomicU64,
    hzb_views: AtomicU64,
    indirect_phases: AtomicU64,
    indirect_batch_sets: AtomicU64,
    frames: AtomicU64,
}

#[derive(Resource, Clone, Default)]
struct RendererProofBridge(Arc<RendererProofState>);

fn sample_renderer_path(
    bridge: Res<RendererProofBridge>,
    support: Option<Res<GpuPreprocessingSupport>>,
    indirect: Option<Res<IndirectParametersBuffers>>,
    views: Query<Option<&ViewDepthPyramid>, With<OcclusionCulling>>,
) {
    let Some(support) = support else { return };
    let preprocessing = support.is_available();
    let culling = support.max_supported_mode == GpuPreprocessingMode::Culling;
    let (phase_count, batch_sets, indirect_active) =
        indirect.as_deref().map_or((0, 0, false), |buffers| {
            let phases = buffers.len() as u64;
            let batch_sets = buffers
                .values()
                .map(|phase| {
                    phase.batch_set_count(true) as u64 + phase.batch_set_count(false) as u64
                })
                .sum::<u64>();
            let active = buffers.values().any(|phase| {
                phase.indexed.data_buffer().is_some() || phase.non_indexed.data_buffer().is_some()
            });
            (phases, batch_sets, active)
        });
    let occlusion_views = views.iter().count() as u64;
    let hzb_views = views.iter().filter(|pyramid| pyramid.is_some()).count() as u64;
    bridge
        .0
        .gpu_preprocessing
        .fetch_or(preprocessing, Ordering::Relaxed);
    bridge.0.gpu_culling.fetch_or(culling, Ordering::Relaxed);
    bridge
        .0
        .indirect_drawing
        .fetch_or(indirect_active, Ordering::Relaxed);
    bridge
        .0
        .occlusion_views
        .fetch_max(occlusion_views, Ordering::Relaxed);
    bridge.0.hzb_views.fetch_max(hzb_views, Ordering::Relaxed);
    bridge
        .0
        .indirect_phases
        .fetch_max(phase_count, Ordering::Relaxed);
    bridge
        .0
        .indirect_batch_sets
        .fetch_max(batch_sets, Ordering::Relaxed);
    bridge.0.frames.fetch_add(1, Ordering::Relaxed);
}

fn sync_renderer_metrics(bridge: Res<RendererProofBridge>, mut metrics: ResMut<RendererMetrics>) {
    metrics.gpu_preprocessing_active = bridge.0.gpu_preprocessing.load(Ordering::Relaxed);
    metrics.gpu_culling_active = bridge.0.gpu_culling.load(Ordering::Relaxed);
    metrics.indirect_drawing_active = bridge.0.indirect_drawing.load(Ordering::Relaxed);
    metrics.occlusion_culling_views = bridge.0.occlusion_views.load(Ordering::Relaxed);
    metrics.hzb_views = bridge.0.hzb_views.load(Ordering::Relaxed);
    metrics.indirect_phase_buffers = bridge.0.indirect_phases.load(Ordering::Relaxed);
    metrics.indirect_batch_sets = bridge.0.indirect_batch_sets.load(Ordering::Relaxed);
    metrics.proof_frames = bridge.0.frames.load(Ordering::Relaxed);
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct TerrainExtension {
    #[texture(100)]
    #[sampler(101)]
    layer_0: Option<Handle<Image>>,
    #[texture(102)]
    #[sampler(103)]
    layer_1: Option<Handle<Image>>,
    #[texture(104)]
    #[sampler(105)]
    layer_2: Option<Handle<Image>>,
    #[texture(106)]
    #[sampler(107)]
    layer_3: Option<Handle<Image>>,
    #[texture(108)]
    #[sampler(109)]
    layer_4: Option<Handle<Image>>,
    #[texture(110)]
    #[sampler(111)]
    layer_5: Option<Handle<Image>>,
    #[uniform(112)]
    settings: TerrainSettings,
}

#[derive(ShaderType, Reflect, Debug, Clone)]
struct TerrainSettings {
    tiling_and_layer_count: Vec4,
    fallback_weights_0: Vec4,
    fallback_weights_1: Vec4,
}

impl TerrainExtension {
    pub fn from_quadrant(
        terrain: &TerrainSnapshot,
        quadrant: u8,
        catalog: &AssetCatalog,
        asset_server: &AssetServer,
    ) -> Result<(Self, Vec<Handle<Image>>), String> {
        let mut textures: [Option<Handle<Image>>; 6] = std::array::from_fn(|_| None);
        let layers = crate::streaming::quadrant_layers(terrain, quadrant)?;
        let mut handles = Vec::with_capacity(layers.len());
        for (target, layer) in textures.iter_mut().zip(&layers) {
            if layer.is_base && layer.texture_form_id == 0 {
                continue;
            }
            let path = catalog
                .landscape_diffuse(layer.texture_form_id)
                .ok_or_else(|| {
                    format!(
                        "LAND {:08X} quadrant {quadrant} texture {:08X} has no diffuse image",
                        terrain.cell_id, layer.texture_form_id
                    )
                })?;
            let handle = asset_server
                .load_builder()
                .with_settings(|settings: &mut ImageLoaderSettings| {
                    settings.sampler = repeating_terrain_sampler();
                })
                .load(path.to_owned());
            *target = Some(handle.clone());
            handles.push(handle);
        }
        Ok((
            Self {
                layer_0: textures[0].clone(),
                layer_1: textures[1].clone(),
                layer_2: textures[2].clone(),
                layer_3: textures[3].clone(),
                layer_4: textures[4].clone(),
                layer_5: textures[5].clone(),
                settings: TerrainSettings {
                    tiling_and_layer_count: Vec4::new(8.0, 8.0, layers.len() as f32, 0.0),
                    fallback_weights_0: Vec4::new(1.0, 0.0, 0.0, 0.0),
                    fallback_weights_1: Vec4::ZERO,
                },
            },
            handles,
        ))
    }

    pub(crate) fn fixture(textures: [Handle<Image>; 6]) -> Self {
        Self {
            layer_0: Some(textures[0].clone()),
            layer_1: Some(textures[1].clone()),
            layer_2: Some(textures[2].clone()),
            layer_3: Some(textures[3].clone()),
            layer_4: Some(textures[4].clone()),
            layer_5: Some(textures[5].clone()),
            settings: TerrainSettings {
                tiling_and_layer_count: Vec4::new(8.0, 8.0, 6.0, 0.0),
                fallback_weights_0: Vec4::X,
                fallback_weights_1: Vec4::ZERO,
            },
        }
    }
}

impl Default for TerrainExtension {
    fn default() -> Self {
        Self {
            layer_0: None,
            layer_1: None,
            layer_2: None,
            layer_3: None,
            layer_4: None,
            layer_5: None,
            settings: TerrainSettings {
                tiling_and_layer_count: Vec4::new(8.0, 8.0, 0.0, 0.0),
                fallback_weights_0: Vec4::X,
                fallback_weights_1: Vec4::ZERO,
            },
        }
    }
}

impl MaterialExtension for TerrainExtension {
    fn fragment_shader() -> ShaderRef {
        "embedded://engine/shaders/terrain.wgsl".into()
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct WaterExtension {
    #[uniform(100)]
    settings: WaterSettings,
    #[texture(101)]
    #[sampler(102)]
    reflection: Option<Handle<Image>>,
    #[texture(103)]
    #[sampler(104)]
    flow_normal: Option<Handle<Image>>,
}

#[derive(ShaderType, Reflect, Debug, Clone)]
struct WaterSettings {
    wave_scale_speed_strength: Vec4,
    flow_direction: Vec4,
    /// x = Fresnel Amount (Schlick F0), y = Reflectivity Amount. z/w unused.
    fresnel_reflectivity: Vec4,
}

impl Default for WaterExtension {
    fn default() -> Self {
        Self {
            settings: WaterSettings {
                wave_scale_speed_strength: Vec4::new(0.006, 0.15, WAVE_STRENGTH, 0.0),
                flow_direction: Vec4::new(0.8, 0.35, 0.0, 0.0),
                fresnel_reflectivity: Vec4::new(
                    DEFAULT_WATER_FRESNEL,
                    DEFAULT_WATER_REFLECTIVITY,
                    0.0,
                    0.0,
                ),
            },
            reflection: None,
            flow_normal: None,
        }
    }
}

impl WaterExtension {
    /// Builds a water material with Skyrim's DefaultWater fresnel and reflectivity. Callers that
    /// know a water's own factors (from [`crate::world::database::AssetCatalog::water_colors`])
    /// should use [`Self::with_reflection_and_factors`] instead.
    pub fn with_reflection(reflection: Handle<Image>, flow_normal: Option<Handle<Image>>) -> Self {
        Self::with_reflection_and_factors(
            reflection,
            flow_normal,
            DEFAULT_WATER_FRESNEL,
            DEFAULT_WATER_REFLECTIVITY,
        )
    }

    pub fn with_reflection_and_factors(
        reflection: Handle<Image>,
        flow_normal: Option<Handle<Image>>,
        fresnel: f32,
        reflectivity: f32,
    ) -> Self {
        let has_flow_normal = flow_normal.is_some() as u8 as f32;
        Self {
            reflection: Some(reflection),
            flow_normal,
            settings: WaterSettings {
                wave_scale_speed_strength: Vec4::new(0.006, 0.15, WAVE_STRENGTH, 0.0),
                flow_direction: Vec4::new(0.8, 0.35, 0.0, has_flow_normal),
                fresnel_reflectivity: Vec4::new(fresnel, reflectivity, 0.0, 0.0),
            },
        }
    }
}

impl MaterialExtension for WaterExtension {
    fn fragment_shader() -> ShaderRef {
        "embedded://engine/shaders/water.wgsl".into()
    }
}

fn animate_water_materials(
    time: Res<Time>,
    config: Option<Res<crate::config::EngineConfig>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    mut profiler: ResMut<ProfilingState>,
) {
    let started = std::time::Instant::now();
    let elapsed = if config.is_some_and(|config| config.terrain_water_fixture) {
        1.0
    } else {
        time.elapsed_secs()
    };
    for (_, material) in materials.iter_mut() {
        material.extension.settings.wave_scale_speed_strength.w = elapsed;
    }
    profiler.record_elapsed("render/water_animation", started);
}

#[derive(Resource, Clone)]
pub struct WaterReflectionTexture(pub Handle<Image>);

#[derive(Component)]
struct WaterReflectionCamera;

/// Slack added to a water plane's bounds before the main camera frustum test, in world units (4096
/// to a cell).
///
/// The frustum carried by the main camera was built at the end of the previous frame, so a fast
/// turn brings water into a view the gate has already closed for, and the frame that renders it
/// shows the reflection of the frame before. A plane this far outside the frustum still keeps the
/// reflection camera on: seen from one cell away, it buys about seven degrees of extra turn per
/// frame - a flick past 400 degrees a second - or, when the camera moves instead of turning, 512
/// units of travel between two frames. A plane that far off the edge of a 45 degree view is a
/// sliver of the frame, so little of the saving is given back.
const WATER_REFLECTION_FRUSTUM_MARGIN: f32 = 512.0;

/// Spawns the camera that fills [`WaterReflectionTexture`], and the texture it renders into.
///
/// It is the flipped copy of the streaming camera, drawn before the main view, and it renders
/// [`REFLECTION_VIEW_LAYERS`]: the world layer only. What Skyrim puts in a water reflection, and
/// why full-detail placed objects are not in it, is documented on that constant.
fn setup_water_reflection(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let image = images.add(Image::new_target_texture(
        1024,
        576,
        bevy::render::render_resource::TextureFormat::Rgba8Unorm,
        Some(bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb),
    ));
    commands.insert_resource(WaterReflectionTexture(image.clone()));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: -1,
            // The camera is placed below the water looking up, not mirrored, so its triangles keep
            // their winding: inverting the culling drew the back faces.
            invert_culling: false,
            is_active: false,
            ..default()
        },
        RenderTarget::Image(image.into()),
        Transform::default(),
        DepthPrepass,
        OcclusionCulling,
        RenderLayers::from_layers(REFLECTION_VIEW_LAYERS),
        WaterReflectionCamera,
    ));
}

/// The streamed world camera, never the reflection camera.
type WaterReflectionObserver = (
    With<crate::world::components::StreamingCamera>,
    Without<WaterReflectionCamera>,
);

/// What the gate reads off the world camera: its pose, the [`Frustum`] Bevy keeps in step with it,
/// and the projection whose far distance the frustum itself does not carry.
type WaterReflectionObserverView = (
    &'static GlobalTransform,
    Option<&'static Frustum>,
    Option<&'static Projection>,
);

fn update_water_reflection_camera(
    main_camera: Query<WaterReflectionObserverView, WaterReflectionObserver>,
    water: Query<(&GlobalTransform, Option<&Aabb>), With<crate::world::components::WaterSurface>>,
    mut reflection_camera: Query<(&mut Transform, &mut Camera), With<WaterReflectionCamera>>,
    mut profiler: ResMut<ProfilingState>,
) {
    let started = std::time::Instant::now();
    let (Ok((main, frustum, projection)), Ok((mut reflection, mut camera))) =
        (main_camera.single(), reflection_camera.single_mut())
    else {
        return;
    };
    // The surface nearest the main camera fixes the mirror plane; a further surface would put the
    // reflection at the wrong height when more than one water level is streamed in.
    let mut mirror_surface = None;
    let mut mirror_distance = f32::INFINITY;
    let mut surface_in_view = false;
    for (surface, bounds) in &water {
        let distance = (surface.translation().y - main.translation().y).abs();
        if distance < mirror_distance {
            mirror_distance = distance;
            mirror_surface = Some(surface);
        }
        surface_in_view |= water_plane_in_view(
            main,
            frustum,
            projection.map(|projection| projection.far()),
            surface,
            bounds,
        );
    }
    let Some(surface) = mirror_surface else {
        camera.is_active = false;
        return;
    };
    // The mirror plane is refreshed even while the view is gated off, so the frame the gate reopens
    // reflects the camera as it stands then rather than the last frame water was on screen.
    *reflection = reflected_camera_transform(main, surface.translation().y);
    camera.is_active = surface_in_view;
    profiler.record_elapsed("render/water_reflection_camera", started);
}

/// Whether a water plane's bounds reach the main camera's view, its bounds grown by
/// [`WATER_REFLECTION_FRUSTUM_MARGIN`] first.
///
/// The frustum half is Bevy's own test against the plane's [`Aabb`]. Bevy's perspective projection
/// is infinite reverse-z, so the [`Frustum`] carries no far plane at all (`from_clip_from_world`
/// leaves its last half space at `(NaN, NaN, NaN, inf)`), and the far distance of the camera's
/// [`Projection`] is applied here by hand: the plane's nearest reach along the camera's forward
/// axis, from its bounds projected onto that axis.
///
/// Missing pieces mean the test cannot run - a plane whose mesh has not produced an [`Aabb`] yet,
/// or a main camera without a [`Frustum`] - and the plane then counts as visible, keeping the
/// reflection camera on rather than risk a stale reflection.
fn water_plane_in_view(
    main: &GlobalTransform,
    frustum: Option<&Frustum>,
    far: Option<f32>,
    surface: &GlobalTransform,
    bounds: Option<&Aabb>,
) -> bool {
    let (Some(frustum), Some(far), Some(bounds)) = (frustum, far, bounds) else {
        return true;
    };
    let margin = Vec3::splat(WATER_REFLECTION_FRUSTUM_MARGIN);
    let grown = Aabb::from_min_max(
        Vec3::from(bounds.min()) - margin,
        Vec3::from(bounds.max()) + margin,
    );
    let surface_to_world = surface.affine();
    let centre = Vec3::from(surface_to_world.transform_point3a(grown.center));
    let radius = grown.relative_radius(&main.forward().as_vec3().into(), &surface_to_world.matrix3);
    if (centre - main.translation()).dot(*main.forward()) - radius > far {
        return false;
    }
    frustum.intersects_obb(&grown, &surface_to_world, true, true)
}

fn reflected_camera_transform(main: &GlobalTransform, water_y: f32) -> Transform {
    let mut position = main.translation();
    position.y = water_y * 2.0 - position.y;
    let mut forward = main.forward().as_vec3();
    forward.y = -forward.y;
    Transform::from_translation(position).looking_to(forward, Vec3::Y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::components::{StreamingCamera, WaterSurface};

    const MAIN_CAMERA_FAR: f32 = 1000.0;
    /// The mesh bounds of the water plane streaming.rs spawns: a `Plane3d` quad a cell wide with no
    /// thickness at all.
    const CELL_WATER_HALF_EXTENTS: Vec3 = Vec3::new(2048.0, 0.0, 2048.0);

    /// The main camera and the reflection camera the gate drives, with no plugins. The gate reads
    /// the camera's pose, frustum and far distance and writes `Camera::is_active` on the reflection
    /// camera, so a headless `App` is enough to run its frames.
    struct ReflectionHarness {
        app: App,
        main_camera: Entity,
        reflection_camera: Entity,
    }

    impl ReflectionHarness {
        fn new(main: Transform) -> Self {
            let mut app = App::new();
            app.init_resource::<ProfilingState>()
                .add_systems(Update, update_water_reflection_camera);
            let main_camera = app
                .world_mut()
                .spawn((
                    GlobalTransform::from(main),
                    main_camera_frustum(&main),
                    main_camera_projection(),
                    StreamingCamera,
                ))
                .id();
            let reflection_camera = app
                .world_mut()
                .spawn((
                    Camera {
                        order: -1,
                        is_active: false,
                        ..default()
                    },
                    Transform::default(),
                    WaterReflectionCamera,
                ))
                .id();
            Self {
                app,
                main_camera,
                reflection_camera,
            }
        }

        /// Turns the main camera and the frustum Bevy keeps in step with it.
        fn aim(&mut self, main: Transform) {
            self.app
                .world_mut()
                .entity_mut(self.main_camera)
                .insert((GlobalTransform::from(main), main_camera_frustum(&main)));
        }

        fn spawn_water(&mut self, translation: Vec3, half_extents: Vec3) {
            self.app.world_mut().spawn((
                GlobalTransform::from(Transform::from_translation(translation)),
                water_bounds(half_extents),
                WaterSurface,
            ));
        }

        fn frame(&mut self) -> ReflectionFrame {
            self.app.update();
            let world = self.app.world();
            let camera = world.get::<Camera>(self.reflection_camera).unwrap();
            let transform = world.get::<Transform>(self.reflection_camera).unwrap();
            ReflectionFrame {
                active: camera.is_active,
                transform: *transform,
            }
        }
    }

    struct ReflectionFrame {
        active: bool,
        transform: Transform,
    }

    fn main_camera_projection() -> Projection {
        Projection::Perspective(PerspectiveProjection {
            far: MAIN_CAMERA_FAR,
            ..default()
        })
    }

    /// The `Frustum` `update_frusta` derives from a camera's projection and pose.
    fn main_camera_frustum(main: &Transform) -> Frustum {
        let projection = main_camera_projection();
        let global = GlobalTransform::from(*main);
        let clip_from_world = projection.get_clip_from_view() * global.to_matrix().inverse();
        Frustum(ViewFrustum::from_clip_from_world(&clip_from_world))
    }

    /// A water plane's bounds in its own space, as `calculate_bounds` leaves them on the entity.
    fn water_bounds(half_extents: Vec3) -> Aabb {
        Aabb::from_min_max(-half_extents, half_extents)
    }

    #[test]
    fn a_spawned_camera_carries_the_components_the_gate_reads() {
        let mut app = App::new();
        let camera = app
            .world_mut()
            .spawn((Camera3d::default(), StreamingCamera))
            .id();
        let world = app.world();
        assert!(world.get::<Frustum>(camera).is_some());
        assert!(world.get::<Projection>(camera).is_some());
    }

    #[test]
    fn reflection_camera_renders_while_a_water_plane_is_in_view() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, -800.0), CELL_WATER_HALF_EXTENTS);
        assert!(harness.frame().active);
    }

    #[test]
    fn reflection_camera_skips_water_behind_the_main_camera() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, 5_000.0), Vec3::new(200.0, 0.0, 200.0));
        assert!(!harness.frame().active);
    }

    #[test]
    fn reflection_camera_skips_water_past_the_far_plane() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, -4_000.0), Vec3::new(200.0, 0.0, 200.0));
        assert!(!harness.frame().active);
    }

    #[test]
    fn reflection_camera_skips_a_cell_sized_plane_past_the_far_plane() {
        // The plane's bounding sphere reaches inside the far distance, but no part of the plane
        // does: its nearest edge, grown by the margin, is still 1,940 units ahead.
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.spawn_water(Vec3::new(0.0, 40.0, -4_500.0), CELL_WATER_HALF_EXTENTS);
        assert!(!harness.frame().active);
    }

    #[test]
    fn reflection_camera_renders_when_only_a_plane_edge_reaches_the_view() {
        // A cell-sized plane a full cell to the side: its centre is far outside the view, but the
        // near corner of the plane crosses into the frustum.
        let main = Transform::from_xyz(0.0, 0.0, 0.0);
        let mut harness = ReflectionHarness::new(main);
        let centre = Vec3::new(2048.0, 0.0, -700.0);
        harness.spawn_water(centre, CELL_WATER_HALF_EXTENTS);
        assert!(harness.frame().active);

        // Counted as a point at its centre, the same plane stays outside the frustum even with the
        // gate's margin, so only the plane's bounds can carry the test.
        assert!(!water_plane_in_view(
            &GlobalTransform::from(main),
            Some(&main_camera_frustum(&main)),
            Some(MAIN_CAMERA_FAR),
            &GlobalTransform::from_translation(centre),
            Some(&water_bounds(Vec3::ZERO)),
        ));
    }

    #[test]
    fn reflection_camera_stays_off_without_water() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        assert!(!harness.frame().active);
    }

    #[test]
    fn reflection_camera_is_mirrored_in_the_frame_it_becomes_active() {
        let facing_away = Transform::from_xyz(0.0, 300.0, 0.0).looking_to(Vec3::Z, Vec3::Y);
        let mut harness = ReflectionHarness::new(facing_away);
        harness.spawn_water(Vec3::new(0.0, 40.0, -800.0), Vec3::new(200.0, 0.0, 200.0));
        assert!(!harness.frame().active);

        // Facing the water reopens the gate, and the mirror of that frame reflects the camera of
        // that frame, not the pose the gate closed at.
        harness.aim(Transform::from_xyz(0.0, 120.0, 0.0));
        let frame = harness.frame();
        assert!(frame.active);
        assert!((frame.transform.translation.y + 40.0).abs() < 1.0e-4);
        assert!(frame.transform.forward().z < 0.0);
    }

    #[test]
    fn reflection_camera_renders_until_a_plane_reports_its_bounds() {
        let mut harness = ReflectionHarness::new(Transform::from_xyz(0.0, 120.0, 0.0));
        harness.app.world_mut().spawn((
            GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 40.0, -800.0))),
            WaterSurface,
        ));
        assert!(harness.frame().active);
    }

    #[test]
    fn terrain_sampler_repeats_with_linear_mip_filtering() {
        let ImageSampler::Descriptor(sampler) = repeating_terrain_sampler() else {
            panic!("terrain sampler must have a descriptor");
        };
        assert_eq!(sampler.address_mode_u, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_v, ImageAddressMode::Repeat);
        assert_eq!(sampler.mag_filter, bevy::image::ImageFilterMode::Linear);
        assert_eq!(sampler.min_filter, bevy::image::ImageFilterMode::Linear);
        assert_eq!(sampler.mipmap_filter, bevy::image::ImageFilterMode::Linear);
        assert_eq!(sampler.anisotropy_clamp, 1);
    }

    use bevy::app::Propagate;
    use bevy::asset::AssetApp;

    #[test]
    fn reflects_camera_above_and_below_the_water_plane() {
        let above = GlobalTransform::from(
            Transform::from_xyz(2.0, 10.0, 4.0).looking_to(Vec3::new(0.0, -0.5, -1.0), Vec3::Y),
        );
        let reflected = reflected_camera_transform(&above, 3.0);
        assert!((reflected.translation.y + 4.0).abs() < 1.0e-5);
        assert!(reflected.forward().y > 0.0);

        let below = GlobalTransform::from(
            Transform::from_xyz(2.0, -4.0, 4.0).looking_to(Vec3::new(0.0, 0.5, -1.0), Vec3::Y),
        );
        let reflected = reflected_camera_transform(&below, 3.0);
        assert!((reflected.translation.y - 10.0).abs() < 1.0e-5);
        assert!(reflected.forward().y < 0.0);
    }

    /// The reflection pass is what costs 1.72 ms of a 4.41 ms frame at the rural cell, because it
    /// redraws every placed object; the camera must be spawned on the world layer alone. Asserting
    /// on the spawned camera rather than on the constant keeps the two from drifting apart.
    #[test]
    fn the_reflection_camera_renders_the_world_layer_only() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .add_systems(Startup, setup_water_reflection);
        app.update();

        let mut cameras = app
            .world_mut()
            .query_filtered::<&RenderLayers, With<WaterReflectionCamera>>();
        let layers = cameras
            .single(app.world())
            .expect("the reflection camera must be spawned");
        assert!(layers.intersects(&RenderLayers::layer(WORLD_LAYER)));
        assert!(!layers.intersects(&RenderLayers::layer(WATER_LAYER)));
        assert!(!layers.intersects(&PLACED_OBJECT_RENDER_LAYERS));
    }

    /// The other half of the contract: the main camera is what still draws the placed objects and
    /// the water, so a new layer has to be added to its list and not only taken out of the
    /// reflection camera's.
    #[test]
    fn the_main_view_renders_every_layer() {
        let main_view = RenderLayers::from_layers(MAIN_VIEW_LAYERS);
        assert!(main_view.intersects(&RenderLayers::layer(WORLD_LAYER)));
        assert!(main_view.intersects(&RenderLayers::layer(WATER_LAYER)));
        assert!(main_view.intersects(&PLACED_OBJECT_RENDER_LAYERS));
        assert!(
            !RenderLayers::from_layers(REFLECTION_VIEW_LAYERS)
                .intersects(&PLACED_OBJECT_RENDER_LAYERS),
            "a placed object must never be drawn into the reflection texture"
        );
    }

    /// A light needs the world layer for both cameras and the placed-object layer for its shadow
    /// cascades: `check_dir_light_mesh_visibility` only collects casters that share a layer with
    /// the light.
    #[test]
    fn lights_cover_both_cameras_and_the_placed_objects() {
        let light = RenderLayers::from_layers(LIGHT_LAYERS);
        for view in [MAIN_VIEW_LAYERS, REFLECTION_VIEW_LAYERS] {
            assert!(
                light.intersects(&RenderLayers::from_layers(view)),
                "a light must reach {view:?}"
            );
        }
        assert!(light.intersects(&PLACED_OBJECT_RENDER_LAYERS));
    }

    /// The propagation has to be in place before visibility compares layers, and the meshes it
    /// carries arrive from the glb spawner in `SpawnScene` - between `Update` and `PostUpdate`. A
    /// descendant spawned after its reference must still inherit the layer in that same frame.
    #[test]
    fn a_descendant_spawned_after_its_reference_inherits_the_placed_object_layer() {
        let mut app = App::new();
        add_placed_object_layer_propagation(&mut app);
        let reference = app
            .world_mut()
            .spawn(Propagate(PLACED_OBJECT_RENDER_LAYERS))
            .id();
        app.update();
        // The glb is spawned frames after the reference: a scene root with the mesh primitives
        // below it.
        let scene_root = app.world_mut().spawn(ChildOf(reference)).id();
        let mesh = app
            .world_mut()
            .spawn((Mesh3d(Handle::default()), ChildOf(scene_root)))
            .id();

        app.update();

        for entity in [reference, scene_root, mesh] {
            assert_eq!(
                app.world().entity(entity).get::<RenderLayers>(),
                Some(&PLACED_OBJECT_RENDER_LAYERS),
                "a reference and everything below it must carry the placed-object layer"
            );
        }
    }

    #[test]
    fn animated_water_advances_the_shader_phase() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Assets<WaterMaterial>>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, animate_water_materials);
        let handle = app
            .world_mut()
            .resource_mut::<Assets<WaterMaterial>>()
            .add(WaterMaterial {
                base: StandardMaterial::default(),
                extension: WaterExtension::default(),
            });
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(2));
        app.update();
        let material = app
            .world()
            .resource::<Assets<WaterMaterial>>()
            .get(&handle)
            .unwrap();
        assert_eq!(material.extension.settings.wave_scale_speed_strength.w, 2.0);
    }

    #[test]
    fn final_renderer_requires_every_gpu_path_signal() {
        let complete = RendererMetrics {
            gpu_preprocessing_active: true,
            gpu_culling_active: true,
            indirect_drawing_active: true,
            occlusion_culling_views: 1,
            hzb_views: 1,
            indirect_phase_buffers: 1,
            indirect_batch_sets: 1,
            proof_frames: 1,
            ..default()
        };
        assert!(complete.final_path_active());
        assert!(
            !RendererMetrics {
                hzb_views: 0,
                ..complete.clone()
            }
            .final_path_active()
        );
        assert!(
            !RendererMetrics {
                renderer_validation_failures: 1,
                ..complete
            }
            .final_path_active()
        );
    }
}
