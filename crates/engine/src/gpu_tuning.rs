//! Renderer settings that depend on how the GPU draws.
//!
//! This engine's views were set up on immediate-mode desktop GPUs. Apple's GPUs - every device Metal
//! drives on Apple Silicon - are tile-based deferred renderers, and several of those choices cost
//! them more than they buy:
//!
//! - **Depth prepass.** A tiled GPU already shades each opaque pixel once: hidden-surface removal
//!   sorts a tile's fragments before shading them, which is the work a depth prepass exists to do
//!   on a desktop GPU. The prepass then only adds a second geometry pass - vertex shading and
//!   tiling for every opaque mesh again - and a depth buffer stored to memory and read back.
//!   [`OcclusionCulling`] builds its depth pyramid from that prepass, so it goes with it; Metal has
//!   no indirect-count draw either, so culled draws are still encoded one by one and the culling
//!   saves vertex work only.
//! - **Shadow cascades.** Every cascade is another pass over the shadow casters, each flushing its
//!   tiles to memory; the tiled profile draws three, and filters them with the hardware's 2x2
//!   comparison instead of the Gaussian kernel's many taps.
//! - **Water reflection.** A second view of the world: the tiled profile draws it at a little over
//!   half the pixels and, where the GPU can render it, in a 32-bit float format instead of a 64-bit
//!   one.
//! - **Resolution.** `crate::render_scale` draws the 3D view at one pixel per point on a Retina
//!   window instead of four.
//!
//! The profile also cuts how many draws each frame encodes, which matters on Metal because wgpu
//! has no multi-draw there and encodes every indirect draw on its own. Once the rest of the profile
//! takes the GPU off the critical path, encoding on the render thread is what sets the frame time:
//!
//! - **CPU-culled direct draws.** Bevy's GPU-driven path culls each view in a compute pass and
//!   draws from indirect buffers, which on Metal still costs one encoded draw per batch plus the
//!   per-view preprocessing passes and buffers. The tiled profile gives every 3D camera
//!   [`NoIndirectDrawing`], so its views - and their shadow views - are culled on the CPU and drawn
//!   directly: 1.2 ms less render thread a frame on an M1 Pro, mostly in `prepare`.
//! - **CPU light clustering.** Bevy assigns point and spot lights to view clusters with compute
//!   passes and per-view buffers on every 3D camera when the device allows it. The tiled profile
//!   turns that off ([`GlobalClusterSettings::gpu_clustering`]), so Bevy clusters on the CPU, which
//!   is close to free with the few lights a cell places: 1.1 ms less render thread a frame in the
//!   wilderness and 0.5 ms in Riverwood with `--lights`, on an M1 Pro.
//! - **Shadow distance.** Without an INI `[Display] fShadowDistance`, the sun's cascades stop at
//!   Skyrim's Ultra value rather than at the far corner of the streamed grid, so each cascade
//!   collects the casters of a few cells rather than all of them.
//! - **No shadow passes for the water reflection.** Bevy builds a full set of sun cascades for
//!   every camera a light reaches, so the reflection camera cost three more shadow passes - a
//!   millisecond of render thread on an M1 Pro. The reflection draws [`REFLECTED_LAYER`], which
//!   every world-layer mesh also carries, and is lit by a shadowless copy of the sun on that layer,
//!   so the terrain reflected in water is lit but not shadowed.
//! - **Small objects by distance.** A placed object's mesh stops drawing beyond a distance
//!   proportional to its size ([`small_object_draw_distance`]): a cup is not drawn three cells away,
//!   in any view or shadow cascade, while a house still draws to the edge of the grid.
//!
//! `--gpu-profile` picks the tuning; `auto` resolves it from the backend the renderer selected.

use crate::{
    config::{EngineConfig, GpuProfile, RenderScale},
    render::{PLACED_OBJECT_LAYER, REFLECTED_LAYER, REFLECTION_VIEW_LAYERS, WORLD_LAYER},
    world::components::InstanceBounds,
};
use bevy::{
    app::PropagateSet,
    camera::{
        primitives::Aabb,
        visibility::{RenderLayers, VisibilityRange, VisibilitySystems},
    },
    core_pipeline::prepass::DepthPrepass,
    light::{ShadowFilteringMethod, cluster::GlobalClusterSettings},
    prelude::*,
    render::{
        occlusion_culling::OcclusionCulling,
        render_resource::TextureFormat,
        renderer::{RenderAdapterInfo, RenderDevice},
        view::NoIndirectDrawing,
    },
    transform::TransformSystems,
};

/// The sun's shadow cascades on a tiled GPU: one pass fewer over the shadow casters, with the
/// remaining three stretched over the same distance.
const TILED_SUN_SHADOW_CASCADES: usize = 3;

/// How far the sun's shadows reach on a tiled GPU when no INI sets `[Display] fShadowDistance`, in
/// Creation units: Skyrim's Ultra preset (`SkyrimPrefs.ini`). The streamed grid's corner, which
/// the cascades reach otherwise, is about 23,200 units away at the default radius.
const TILED_SUN_SHADOW_DISTANCE: f32 = 8_000.0;

/// How far a placed object's mesh draws per unit of its bounding radius on a tiled GPU. At 150 an
/// object with a 16-unit radius - a plate or a cup - draws to 2,400 units, about 34 metres, past
/// which it covers a pixel or two of the screen; one with a 155-unit radius draws to the far corner
/// of the default grid, and anything larger is never cut.
const SMALL_OBJECT_DRAW_DISTANCE_PER_RADIUS: f32 = 150.0;

/// The nearest a placed object's mesh stops drawing, however small it is: half a cell, so nothing
/// vanishes within a few steps of the player.
const MIN_SMALL_OBJECT_DRAW_DISTANCE: f32 = 2_048.0;

/// The water reflection's size on a desktop GPU.
const DESKTOP_WATER_REFLECTION_SIZE: UVec2 = UVec2::new(1024, 576);

/// The water reflection's size on a tiled GPU: 56% of the desktop pixels. The waves already blur
/// the reflection, so the lost detail is hard to see.
const TILED_WATER_REFLECTION_SIZE: UVec2 = UVec2::new(768, 432);

/// What the renderer's setup is tuned for, resolved once the renderer has picked a backend.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct GpuTuning {
    /// Tuned for a tile-based deferred renderer rather than an immediate-mode desktop GPU.
    pub tiled: bool,
    /// The 3D view's resolution as the command line asked for it.
    pub render_scale: RenderScale,
    /// Whether the device can render to [`TextureFormat::Rg11b10Ufloat`].
    pub rg11b10_renderable: bool,
}

impl Default for GpuTuning {
    /// The desktop setup the engine was first tuned on, for a run with no renderer to ask.
    fn default() -> Self {
        Self {
            tiled: false,
            render_scale: RenderScale::Auto,
            rg11b10_renderable: false,
        }
    }
}

impl GpuTuning {
    /// The tuning `profile` asks for on `backend`; `auto` is tiled exactly when the backend is
    /// Metal. With no backend - a headless run - `auto` keeps the desktop setup.
    pub fn resolve(
        profile: GpuProfile,
        backend: Option<wgpu::Backend>,
        render_scale: RenderScale,
        rg11b10_renderable: bool,
    ) -> Self {
        let tiled = match profile {
            GpuProfile::Auto => backend == Some(wgpu::Backend::Metal),
            GpuProfile::Tiled => true,
            GpuProfile::Desktop => false,
        };
        Self {
            tiled,
            render_scale,
            rg11b10_renderable,
        }
    }

    /// How many cascades the sun's shadow map is split into.
    pub fn sun_shadow_cascades(&self) -> usize {
        if self.tiled {
            TILED_SUN_SHADOW_CASCADES
        } else {
            crate::app::SUN_SHADOW_CASCADES
        }
    }

    /// How far the sun's shadow cascades reach at most: the INI's `fShadowDistance` when one was
    /// read, Skyrim's Ultra value on a tiled GPU otherwise, and no limit beyond the streamed grid
    /// on a desktop GPU.
    pub fn sun_shadow_distance_limit(&self, configured: Option<f32>) -> Option<f32> {
        configured.or(self.tiled.then_some(TILED_SUN_SHADOW_DISTANCE))
    }

    /// The layers the water reflection camera draws: [`REFLECTED_LAYER`] on a tiled GPU, so the
    /// sun's shadow cascades are not built for it, and [`REFLECTION_VIEW_LAYERS`] otherwise.
    pub fn reflection_view_layers(&self) -> RenderLayers {
        if self.tiled {
            RenderLayers::layer(REFLECTED_LAYER)
        } else {
            RenderLayers::from_layers(REFLECTION_VIEW_LAYERS)
        }
    }

    /// How a view filters the shadow maps it samples. Desktop keeps Bevy's default.
    pub fn shadow_filtering(&self) -> ShadowFilteringMethod {
        if self.tiled {
            ShadowFilteringMethod::Hardware2x2
        } else {
            ShadowFilteringMethod::default()
        }
    }

    /// The size of the water reflection's render target.
    pub fn water_reflection_size(&self) -> UVec2 {
        if self.tiled {
            TILED_WATER_REFLECTION_SIZE
        } else {
            DESKTOP_WATER_REFLECTION_SIZE
        }
    }

    /// The water reflection's format. The reflection is light in scene-linear units with no alpha
    /// the water reads, so on a tiled GPU it takes the 32-bit float format when the device can
    /// render to it, halving what the pass stores and the water samples.
    pub fn water_reflection_format(&self) -> TextureFormat {
        if self.tiled && self.rg11b10_renderable {
            TextureFormat::Rg11b10Ufloat
        } else {
            TextureFormat::Rgba16Float
        }
    }

    /// The fraction of a window's physical resolution its 3D view is drawn at, on each axis.
    /// `scale_factor` is the window's physical pixels per logical point.
    pub fn render_scale(&self, scale_factor: f32) -> f32 {
        match self.render_scale {
            RenderScale::Factor(factor) => factor,
            RenderScale::Auto if self.tiled && scale_factor > 1.0 => scale_factor.recip(),
            RenderScale::Auto => 1.0,
        }
        .clamp(crate::config::MIN_RENDER_SCALE, 1.0)
    }
}

/// Resolves [`GpuTuning`] before any startup system spawns a view, and adjusts every 3D camera
/// to it as the camera is spawned.
pub struct GpuTuningPlugin;

impl Plugin for GpuTuningPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GpuTuning>()
            .add_systems(PreStartup, resolve_gpu_tuning)
            .add_systems(Startup, cluster_lights_on_the_cpu)
            .add_systems(PostUpdate, tune_new_cameras)
            .add_systems(
                PostUpdate,
                limit_small_object_draw_distance
                    .after(VisibilitySystems::CalculateBounds)
                    .after(TransformSystems::Propagate)
                    .after(PropagateSet::<RenderLayers>::default())
                    .before(VisibilitySystems::CheckVisibility),
            )
            .add_systems(
                PostUpdate,
                (
                    mirror_world_layer_into_reflection,
                    (spawn_reflection_suns, sync_reflection_suns).chain(),
                )
                    .after(PropagateSet::<RenderLayers>::default())
                    .before(VisibilitySystems::CheckVisibility),
            )
            .add_plugins(crate::render_scale::RenderScalePlugin);
    }
}

/// Reads the selected backend and the device's features into [`GpuTuning`].
fn resolve_gpu_tuning(
    mut commands: Commands,
    config: Res<EngineConfig>,
    adapter: Option<Res<RenderAdapterInfo>>,
    device: Option<Res<RenderDevice>>,
) {
    let backend = adapter.as_ref().map(|adapter| adapter.backend);
    let rg11b10_renderable = device.is_some_and(|device| {
        device
            .features()
            .contains(wgpu::Features::RG11B10UFLOAT_RENDERABLE)
    });
    let tuning = GpuTuning::resolve(
        config.gpu_profile,
        backend,
        config.render_scale,
        rg11b10_renderable,
    );
    info!(
        ?backend,
        profile = ?config.gpu_profile,
        tiled = tuning.tiled,
        render_scale = ?tuning.render_scale,
        "GPU tuning resolved"
    );
    commands.insert_resource(tuning);
}

/// On a tiled GPU, has Bevy cluster lights on the CPU instead of in compute passes per view. Runs
/// in `Startup`, after [`resolve_gpu_tuning`] and after the renderer plugins created the settings.
fn cluster_lights_on_the_cpu(
    tuning: Res<GpuTuning>,
    settings: Option<ResMut<GlobalClusterSettings>>,
) {
    if let Some(mut settings) = settings
        && tuning.tiled
        && settings.gpu_clustering.is_some()
    {
        settings.gpu_clustering = None;
    }
}

/// On a tiled GPU, takes the depth prepass and the occlusion culling built on it off every new 3D
/// camera, switches it to CPU-culled direct draws, and gives it the cheaper shadow filter. The
/// spawning code states the desktop setup, so this is the one place the tiled setup departs from it.
///
/// Runs in `PostUpdate`, so a camera spawned in any earlier schedule of the frame is adjusted
/// before the render world first extracts it.
fn tune_new_cameras(
    mut commands: Commands,
    tuning: Res<GpuTuning>,
    cameras: Query<Entity, Added<Camera3d>>,
) {
    if !tuning.tiled {
        return;
    }
    for camera in &cameras {
        commands
            .entity(camera)
            .remove::<(DepthPrepass, OcclusionCulling)>()
            .insert((tuning.shadow_filtering(), NoIndirectDrawing));
    }
}

/// How far a placed object's mesh with a world-space bounding `radius` draws, or `None` when it
/// is large enough to draw as far as anything is streamed.
fn small_object_draw_distance(radius: f32) -> Option<f32> {
    let distance =
        (radius * SMALL_OBJECT_DRAW_DISTANCE_PER_RADIUS).max(MIN_SMALL_OBJECT_DRAW_DISTANCE);
    // Past the far corner of the default grid, a range would never cut anything.
    (distance < crate::world::components::CELL_SIZE * 4.0 * std::f32::consts::SQRT_2)
        .then_some(distance)
}

/// On a tiled GPU, gives each newly bounded placed-object mesh a [`VisibilityRange`] that ends at
/// [`small_object_draw_distance`] for the whole object it belongs to: the size is the placed
/// reference's [`InstanceBounds`], so the parts of a house share the house's distance rather than
/// its trim vanishing before its walls. A mesh with no bounded reference above it is sized by its
/// own bounds.
///
/// The range ends without a cross-fade margin: Bevy only dithers a range with margins, and
/// dithering would add a `discard` pipeline variant per material that defeats the hidden-surface
/// removal the tiled profile relies on.
///
/// Runs after bounds, transforms and the placed-object layer have reached the mesh, and before
/// visibility is checked, so a mesh's first visible frame already honours its range.
#[allow(clippy::type_complexity)]
fn limit_small_object_draw_distance(
    mut commands: Commands,
    tuning: Res<GpuTuning>,
    meshes: Query<
        (Entity, &Aabb, &GlobalTransform, &RenderLayers),
        (
            With<Mesh3d>,
            Without<VisibilityRange>,
            Or<(Added<Aabb>, Changed<RenderLayers>)>,
        ),
    >,
    parents: Query<&ChildOf>,
    references: Query<&InstanceBounds>,
) {
    if !tuning.tiled {
        return;
    }
    for (entity, bounds, transform, layers) in &meshes {
        if !layers.intersects(&RenderLayers::layer(PLACED_OBJECT_LAYER)) {
            continue;
        }
        let reference = parents
            .iter_ancestors(entity)
            .find_map(|ancestor| references.get(ancestor).ok());
        let radius = match reference {
            Some(reference) => (reference.max - reference.min).length() * 0.5,
            None => {
                let scale = transform.compute_transform().scale.abs().max_element();
                Vec3::from(bounds.half_extents).length() * scale
            }
        };
        if let Some(distance) = small_object_draw_distance(radius) {
            commands.entity(entity).insert(VisibilityRange {
                start_margin: 0.0..0.0,
                end_margin: distance..distance,
                use_aabb: true,
            });
        }
    }
}

/// On a tiled GPU, adds [`REFLECTED_LAYER`] to every mesh on [`WORLD_LAYER`] - a mesh without
/// [`RenderLayers`] is on it by default - so the reflection camera, which draws that layer alone,
/// still sees the terrain, LOD land and sky it reflected before. A placed object's meshes carry
/// [`PLACED_OBJECT_LAYER`] alone and stay out, as before.
#[allow(clippy::type_complexity)]
fn mirror_world_layer_into_reflection(
    mut commands: Commands,
    tuning: Res<GpuTuning>,
    meshes: Query<
        (Entity, Option<&RenderLayers>),
        (With<Mesh3d>, Or<(Added<Mesh3d>, Changed<RenderLayers>)>),
    >,
) {
    if !tuning.tiled {
        return;
    }
    let world = RenderLayers::layer(WORLD_LAYER);
    let reflected = RenderLayers::layer(REFLECTED_LAYER);
    for (entity, layers) in &meshes {
        let layers = layers.cloned().unwrap_or_default();
        if layers.intersects(&world) && !layers.intersects(&reflected) {
            commands.entity(entity).insert(layers.with(REFLECTED_LAYER));
        }
    }
}

/// The shadowless copy of a world-lighting directional light that lights the water reflection on
/// a tiled GPU, and the light it copies.
#[derive(Component, Debug)]
struct ReflectionSun {
    source: Entity,
}

/// Spawns a [`ReflectionSun`] for each new shadow-casting directional light that lights the world
/// layer, on a tiled GPU.
#[allow(clippy::type_complexity)]
fn spawn_reflection_suns(
    mut commands: Commands,
    tuning: Res<GpuTuning>,
    lights: Query<
        (
            Entity,
            &DirectionalLight,
            &GlobalTransform,
            Option<&RenderLayers>,
        ),
        (Added<DirectionalLight>, Without<ReflectionSun>),
    >,
) {
    if !tuning.tiled {
        return;
    }
    for (source, light, transform, layers) in &lights {
        if !light.shadow_maps_enabled
            || !layers
                .cloned()
                .unwrap_or_default()
                .intersects(&RenderLayers::layer(WORLD_LAYER))
        {
            continue;
        }
        commands.spawn((
            Name::new("Reflection sun"),
            DirectionalLight {
                shadow_maps_enabled: false,
                ..*light
            },
            transform.compute_transform(),
            RenderLayers::layer(REFLECTED_LAYER),
            ReflectionSun { source },
        ));
    }
}

/// Keeps each [`ReflectionSun`] on its source's colour, brightness and direction, and removes it
/// with its source.
fn sync_reflection_suns(
    mut commands: Commands,
    sources: Query<(Ref<DirectionalLight>, Ref<GlobalTransform>), Without<ReflectionSun>>,
    mut copies: Query<(
        Entity,
        &ReflectionSun,
        &mut DirectionalLight,
        &mut Transform,
    )>,
) {
    for (entity, copy, mut light, mut transform) in &mut copies {
        let Ok((source, source_transform)) = sources.get(copy.source) else {
            commands.entity(entity).despawn();
            continue;
        };
        if source.is_changed() {
            *light = DirectionalLight {
                shadow_maps_enabled: false,
                ..*source
            };
        }
        if source_transform.is_changed() {
            *transform = source_transform.compute_transform();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_tiled_on_metal_only() {
        let tiled =
            |backend| GpuTuning::resolve(GpuProfile::Auto, backend, RenderScale::Auto, true).tiled;
        assert!(tiled(Some(wgpu::Backend::Metal)));
        assert!(!tiled(Some(wgpu::Backend::Vulkan)));
        assert!(!tiled(Some(wgpu::Backend::Dx12)));
        assert!(!tiled(None));
    }

    #[test]
    fn an_explicit_profile_overrides_the_backend() {
        let resolve = |profile, backend| {
            GpuTuning::resolve(profile, Some(backend), RenderScale::Auto, true).tiled
        };
        assert!(resolve(GpuProfile::Tiled, wgpu::Backend::Vulkan));
        assert!(!resolve(GpuProfile::Desktop, wgpu::Backend::Metal));
    }

    #[test]
    fn desktop_keeps_the_original_setup() {
        let tuning = GpuTuning::default();
        assert_eq!(tuning.sun_shadow_cascades(), 4);
        assert_eq!(tuning.shadow_filtering(), ShadowFilteringMethod::Gaussian);
        assert_eq!(tuning.water_reflection_size(), UVec2::new(1024, 576));
        assert_eq!(tuning.water_reflection_format(), TextureFormat::Rgba16Float);
        assert_eq!(tuning.render_scale(2.0), 1.0);
    }

    #[test]
    fn tiled_reflection_format_needs_device_support() {
        let tiled =
            |renderable| GpuTuning::resolve(GpuProfile::Tiled, None, RenderScale::Auto, renderable);
        assert_eq!(
            tiled(true).water_reflection_format(),
            TextureFormat::Rg11b10Ufloat
        );
        assert_eq!(
            tiled(false).water_reflection_format(),
            TextureFormat::Rgba16Float
        );
    }

    #[test]
    fn auto_render_scale_draws_one_pixel_per_point_on_a_tiled_gpu() {
        let tuning = GpuTuning::resolve(GpuProfile::Tiled, None, RenderScale::Auto, true);
        assert_eq!(tuning.render_scale(2.0), 0.5);
        assert_eq!(tuning.render_scale(1.0), 1.0);
        // A scale factor past four would ask for less than the smallest accepted factor.
        assert_eq!(tuning.render_scale(8.0), crate::config::MIN_RENDER_SCALE);
    }

    #[test]
    fn an_explicit_render_scale_applies_on_any_gpu() {
        let tuning = GpuTuning::resolve(GpuProfile::Desktop, None, RenderScale::Factor(0.75), true);
        assert_eq!(tuning.render_scale(1.0), 0.75);
        assert_eq!(tuning.render_scale(2.0), 0.75);
    }

    #[test]
    fn tiled_gpus_cluster_lights_on_the_cpu() {
        for tiled in [false, true] {
            let mut app = App::new();
            app.insert_resource(GpuTuning { tiled, ..default() })
                .insert_resource(GlobalClusterSettings {
                    supports_storage_buffers: true,
                    clustered_decals_are_usable: false,
                    gpu_clustering: Some(bevy::light::cluster::GlobalClusterGpuSettings {
                        initial_z_slice_list_capacity: 1,
                        initial_index_list_capacity: 1,
                    }),
                    max_uniform_buffer_clusterable_objects: 1,
                    view_cluster_bindings_max_indices: 1,
                })
                .add_systems(Startup, cluster_lights_on_the_cpu);
            app.update();
            let settings = app.world().resource::<GlobalClusterSettings>();
            assert_eq!(settings.gpu_clustering.is_some(), !tiled);
        }
    }

    #[test]
    fn tiled_shadows_stop_at_skyrims_ultra_distance_unless_an_ini_says_otherwise() {
        let tiled = GpuTuning::resolve(GpuProfile::Tiled, None, RenderScale::Auto, true);
        assert_eq!(tiled.sun_shadow_distance_limit(None), Some(8_000.0));
        assert_eq!(
            tiled.sun_shadow_distance_limit(Some(4_000.0)),
            Some(4_000.0)
        );
        let desktop = GpuTuning::default();
        assert_eq!(desktop.sun_shadow_distance_limit(None), None);
        assert_eq!(
            desktop.sun_shadow_distance_limit(Some(4_000.0)),
            Some(4_000.0)
        );
    }

    #[test]
    fn small_objects_draw_in_proportion_to_their_size() {
        assert_eq!(
            small_object_draw_distance(1.0),
            Some(MIN_SMALL_OBJECT_DRAW_DISTANCE)
        );
        assert_eq!(small_object_draw_distance(16.0), Some(2_400.0));
        assert_eq!(small_object_draw_distance(100.0), Some(15_000.0));
        // A building draws as far as anything is streamed.
        assert_eq!(small_object_draw_distance(400.0), None);
    }

    #[test]
    fn only_small_placed_objects_get_a_draw_distance_on_a_tiled_gpu() {
        for tiled in [false, true] {
            let mut app = App::new();
            app.insert_resource(GpuTuning { tiled, ..default() })
                .add_systems(PostUpdate, limit_small_object_draw_distance);
            let placed = RenderLayers::layer(PLACED_OBJECT_LAYER);
            let small = Aabb::from_min_max(Vec3::splat(-8.0), Vec3::splat(8.0));
            let large = Aabb::from_min_max(Vec3::splat(-400.0), Vec3::splat(400.0));
            let mut spawn = |bounds: Aabb, layers: RenderLayers| {
                app.world_mut()
                    .spawn((Mesh3d::default(), bounds, GlobalTransform::IDENTITY, layers))
                    .id()
            };
            let cup = spawn(small, placed.clone());
            let house = spawn(large, placed.clone());
            let terrain = spawn(small, RenderLayers::layer(crate::render::WORLD_LAYER));
            // A small part of a large reference takes the reference's size.
            let trim = spawn(small, placed.clone());
            let reference = app
                .world_mut()
                .spawn(InstanceBounds {
                    min: Vec3::splat(-400.0),
                    max: Vec3::splat(400.0),
                })
                .add_child(trim)
                .id();
            app.update();
            let range = |entity| app.world().get::<VisibilityRange>(entity).cloned();
            assert_eq!(range(cup).is_some(), tiled);
            assert!(range(house).is_none());
            assert!(range(terrain).is_none());
            assert!(range(trim).is_none(), "trim of reference {reference}");
        }
    }

    #[test]
    fn the_reflection_draws_its_own_layer_only_on_a_tiled_gpu() {
        let tiled = GpuTuning::resolve(GpuProfile::Tiled, None, RenderScale::Auto, true);
        assert_eq!(
            tiled.reflection_view_layers(),
            RenderLayers::layer(REFLECTED_LAYER)
        );
        // The sun's layers never reach the tiled reflection view, so no cascades are built for it.
        assert!(
            !RenderLayers::from_layers(crate::render::LIGHT_LAYERS)
                .intersects(&tiled.reflection_view_layers())
        );
        assert_eq!(
            GpuTuning::default().reflection_view_layers(),
            RenderLayers::from_layers(REFLECTION_VIEW_LAYERS)
        );
    }

    #[test]
    fn world_meshes_are_mirrored_into_the_reflection_and_placed_objects_are_not() {
        let mut app = App::new();
        app.insert_resource(GpuTuning {
            tiled: true,
            ..default()
        })
        .add_systems(PostUpdate, mirror_world_layer_into_reflection);
        let terrain = app.world_mut().spawn(Mesh3d::default()).id();
        let placed = app
            .world_mut()
            .spawn((Mesh3d::default(), RenderLayers::layer(PLACED_OBJECT_LAYER)))
            .id();
        let water = app
            .world_mut()
            .spawn((
                Mesh3d::default(),
                RenderLayers::layer(crate::render::WATER_LAYER),
            ))
            .id();
        app.update();
        let layers = |entity| app.world().get::<RenderLayers>(entity).cloned();
        assert_eq!(
            layers(terrain),
            Some(RenderLayers::from_layers(&[WORLD_LAYER, REFLECTED_LAYER]))
        );
        assert_eq!(
            layers(placed),
            Some(RenderLayers::layer(PLACED_OBJECT_LAYER))
        );
        assert_eq!(
            layers(water),
            Some(RenderLayers::layer(crate::render::WATER_LAYER))
        );
    }

    #[test]
    fn the_reflection_sun_follows_the_sun_without_shadows() {
        let mut app = App::new();
        app.insert_resource(GpuTuning {
            tiled: true,
            ..default()
        })
        .add_systems(
            PostUpdate,
            (spawn_reflection_suns, sync_reflection_suns).chain(),
        );
        let sun = app
            .world_mut()
            .spawn((
                DirectionalLight {
                    illuminance: 1000.0,
                    shadow_maps_enabled: true,
                    ..default()
                },
                GlobalTransform::from(Transform::from_rotation(Quat::from_rotation_x(-0.5))),
                RenderLayers::from_layers(crate::render::LIGHT_LAYERS),
            ))
            .id();
        app.update();
        app.update();
        let mut copies =
            app.world_mut()
                .query::<(&DirectionalLight, &Transform, &RenderLayers, &ReflectionSun)>();
        let (light, transform, layers, copy) = copies.single(app.world()).unwrap();
        assert!(!light.shadow_maps_enabled);
        assert_eq!(light.illuminance, 1000.0);
        assert!(
            transform
                .rotation
                .abs_diff_eq(Quat::from_rotation_x(-0.5), 1e-6)
        );
        assert_eq!(layers, &RenderLayers::layer(REFLECTED_LAYER));
        assert_eq!(copy.source, sun);

        app.world_mut()
            .get_mut::<DirectionalLight>(sun)
            .unwrap()
            .illuminance = 500.0;
        app.update();
        let (light, ..) = copies.single(app.world()).unwrap();
        assert_eq!(light.illuminance, 500.0);

        app.world_mut().despawn(sun);
        app.update();
        assert_eq!(copies.iter(app.world()).count(), 0);
    }

    #[test]
    fn tiled_cameras_lose_the_depth_prepass_and_indirect_draws_and_gain_the_hardware_shadow_filter()
    {
        for tiled in [false, true] {
            let mut app = App::new();
            app.insert_resource(GpuTuning { tiled, ..default() })
                .add_systems(PostUpdate, tune_new_cameras);
            let camera = app
                .world_mut()
                .spawn((Camera3d::default(), DepthPrepass, OcclusionCulling))
                .id();
            app.update();
            let entity = app.world().entity(camera);
            assert_eq!(entity.contains::<DepthPrepass>(), !tiled);
            assert_eq!(entity.contains::<OcclusionCulling>(), !tiled);
            assert_eq!(entity.contains::<NoIndirectDrawing>(), tiled);
            assert_eq!(
                entity.get::<ShadowFilteringMethod>().copied(),
                tiled.then_some(ShadowFilteringMethod::Hardware2x2)
            );
        }
    }
}
