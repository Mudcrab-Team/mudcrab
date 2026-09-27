use crate::{
    profiling::ProfilingState,
    world::{cache::TerrainSnapshot, database::AssetCatalog},
};
use bevy::{
    asset::embedded_asset,
    camera::{RenderTarget, visibility::RenderLayers},
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
    // Each layer's normal map, sampled through `layer_0_sampler`: every terrain layer image is
    // loaded with the same repeating sampler (`terrain_layer_sampler`), so the six need no sampler
    // bindings of their own. Which layers have one is in `TerrainSettings::normal_layers_*`, since
    // Bevy's stand-in for a missing texture is white, which decodes to a steep tilt.
    #[texture(113)]
    normal_0: Option<Handle<Image>>,
    #[texture(114)]
    normal_1: Option<Handle<Image>>,
    #[texture(115)]
    normal_2: Option<Handle<Image>>,
    #[texture(116)]
    normal_3: Option<Handle<Image>>,
    #[texture(117)]
    normal_4: Option<Handle<Image>>,
    #[texture(118)]
    normal_5: Option<Handle<Image>>,
}

#[derive(ShaderType, Reflect, Debug, Clone)]
struct TerrainSettings {
    tiling_and_layer_count: Vec4,
    quadrant_origin: Vec4,
    fallback_weights_0: Vec4,
    fallback_weights_1: Vec4,
    weight_source: Vec4,
    /// 1.0 in component `i` when layer `i` (0-3) has a normal map bound, else 0.0.
    normal_layers_0: Vec4,
    /// Layers 4 and 5 in x and y.
    normal_layers_1: Vec4,
    weights: [Vec4; WEIGHT_FIELD_WORDS],
}

/// The sample grid of one terrain quadrant: `17x17` opacities on the same `128`-unit spacing as the
/// quadrant's mesh columns, which is what LAND's `VTXT` entries index.
pub(crate) const QUADRANT_WEIGHT_SAMPLES: usize = 17;

/// Overlay layers a quadrant can carry beside its base: the runtime's cap, since
/// [`crate::streaming::quadrant_layers`] rejects a quadrant with more than six layers in total.
pub(crate) const OVERLAY_WEIGHT_SLOTS: usize = 5;

/// `Vec4`s per overlay in the weight field: one [`QUADRANT_WEIGHT_SAMPLES`]-square grid packed four
/// samples to a `Vec4` and rounded up to whole `Vec4`s, so every overlay starts on a 16-byte
/// boundary. The shader declares the same number (`WEIGHT_GRID_WORDS` in `terrain.wgsl`).
pub(crate) const OVERLAY_WEIGHT_WORDS: usize =
    (QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES).div_ceil(4);

/// The whole field one material carries: [`OVERLAY_WEIGHT_SLOTS`] overlays of
/// [`OVERLAY_WEIGHT_WORDS`] words each. The shader declares the same number as the length of its
/// `weights` array.
pub(crate) const WEIGHT_FIELD_WORDS: usize = OVERLAY_WEIGHT_SLOTS * OVERLAY_WEIGHT_WORDS;

/// The sampler a terrain layer texture needs: the shader tiles every layer `tiling` times across a
/// cell (`uv * 8`), so the address mode must repeat. Bevy's default sampler clamps to the edge,
/// which stretches a texture's last texel column, row and corner across everything past the first
/// tile - long streaks where the edge column is stretched, and one flat colour where the corner
/// texel covers the rest. A terrain texture that is not loaded through the asset server - the
/// synthetic fixtures build theirs in memory - has to be given the same sampler by hand.
pub(crate) fn terrain_layer_sampler() -> ImageSamplerDescriptor {
    ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::linear()
    }
}

/// Packs a quadrant's overlay weight grids into the field the material uniform carries: sample `s`
/// of overlay `o` lands in component `s % 4` of word `o * OVERLAY_WEIGHT_WORDS + s / 4`, which is
/// where `grid_weight` in `terrain.wgsl` reads it. Grids beyond [`OVERLAY_WEIGHT_SLOTS`] and
/// samples beyond one grid are dropped, so a malformed grid cannot write outside the field.
fn weight_field(overlay_weights: &[Vec<f32>]) -> [Vec4; WEIGHT_FIELD_WORDS] {
    let mut field = [Vec4::ZERO; WEIGHT_FIELD_WORDS];
    for (overlay, grid) in overlay_weights
        .iter()
        .take(OVERLAY_WEIGHT_SLOTS)
        .enumerate()
    {
        for (sample, weight) in grid
            .iter()
            .enumerate()
            .take(QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES)
        {
            field[overlay * OVERLAY_WEIGHT_WORDS + sample / 4][sample % 4] = *weight;
        }
    }
    field
}

impl TerrainSettings {
    /// Marks which layers have a normal map bound, for the shader to blend only those.
    fn set_normal_layers(&mut self, normals: &[Option<Handle<Image>>; 6]) {
        let present = |index: usize| if normals[index].is_some() { 1.0 } else { 0.0 };
        self.normal_layers_0 = Vec4::new(present(0), present(1), present(2), present(3));
        self.normal_layers_1 = Vec4::new(present(4), present(5), 0.0, 0.0);
    }

    /// The settings of one quadrant of `terrain`: the tiling the shader repeats every layer by, the
    /// quadrant's origin inside the cell, and its overlay weight field. The shader interpolates the
    /// field itself, so a weight reaches the fragment stage exactly as LAND records it instead of
    /// through a vertex attribute Bevy re-normalizes.
    fn for_quadrant(quadrant: u8, layers: usize, overlay_weights: &[Vec<f32>]) -> Self {
        Self {
            tiling_and_layer_count: Vec4::new(8.0, 8.0, layers as f32, 0.0),
            quadrant_origin: Vec4::new(f32::from(quadrant % 2), f32::from(quadrant / 2), 0.0, 0.0),
            fallback_weights_0: Vec4::X,
            fallback_weights_1: Vec4::ZERO,
            // A quadrant with no overlay has nothing in the field to read, so the shader is told to
            // skip it: the packed attributes carry the same (empty) overlays.
            normal_layers_0: Vec4::ZERO,
            normal_layers_1: Vec4::ZERO,
            weight_source: if overlay_weights.is_empty() {
                Vec4::ZERO
            } else {
                Vec4::X
            },
            weights: weight_field(overlay_weights),
        }
    }

    /// The settings of a material that carries no weight field, so the shader reads the weights the
    /// mesh packs into its vertex attributes instead. Only the stand-in terrain a synthetic scene
    /// draws without a LAND snapshot - the streaming and benchmark fixtures - uses it.
    fn vertex_weights_only(layers: f32) -> Self {
        Self {
            tiling_and_layer_count: Vec4::new(8.0, 8.0, layers, 0.0),
            quadrant_origin: Vec4::ZERO,
            fallback_weights_0: Vec4::X,
            fallback_weights_1: Vec4::ZERO,
            normal_layers_0: Vec4::ZERO,
            normal_layers_1: Vec4::ZERO,
            weight_source: Vec4::ZERO,
            weights: [Vec4::ZERO; WEIGHT_FIELD_WORDS],
        }
    }
}

/// The images a terrain quadrant's material waits on, by the colour space each
/// must decode to.
pub struct TerrainImages {
    /// The layers' diffuse images, sRGB.
    pub color: Vec<Handle<Image>>,
    /// The layers' normal maps, linear.
    pub normal: Vec<Handle<Image>>,
}

impl TerrainExtension {
    pub fn from_quadrant(
        terrain: &TerrainSnapshot,
        quadrant: u8,
        catalog: &AssetCatalog,
        asset_server: &AssetServer,
    ) -> Result<(Self, TerrainImages), String> {
        let mut textures: [Option<Handle<Image>>; 6] = std::array::from_fn(|_| None);
        let mut normals: [Option<Handle<Image>>; 6] = std::array::from_fn(|_| None);
        let layers = crate::streaming::quadrant_layers(terrain, quadrant)?;
        let mut handles = Vec::with_capacity(layers.len());
        let mut normal_handles = Vec::new();
        for ((target, normal), layer) in textures.iter_mut().zip(normals.iter_mut()).zip(&layers) {
            if layer.is_base && layer.texture_form_id == 0 {
                continue;
            }
            if let Some(path) = catalog.landscape_normal(layer.texture_form_id) {
                // A normal map holds directions, not colours: it must not be decoded as sRGB.
                let handle = asset_server
                    .load_builder()
                    .with_settings(|settings: &mut ImageLoaderSettings| {
                        settings.sampler = ImageSampler::Descriptor(terrain_layer_sampler());
                        settings.is_srgb = false;
                    })
                    .load(path.to_owned());
                *normal = Some(handle.clone());
                normal_handles.push(handle);
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
                    settings.sampler = ImageSampler::Descriptor(terrain_layer_sampler());
                })
                .load(path.to_owned());
            *target = Some(handle.clone());
            handles.push(handle);
        }
        let overlay_weights = crate::streaming::quadrant_overlay_weights(terrain, quadrant)?;
        let mut settings = TerrainSettings::for_quadrant(quadrant, layers.len(), &overlay_weights);
        settings.set_normal_layers(&normals);
        Ok((
            Self {
                layer_0: textures[0].clone(),
                layer_1: textures[1].clone(),
                layer_2: textures[2].clone(),
                layer_3: textures[3].clone(),
                layer_4: textures[4].clone(),
                layer_5: textures[5].clone(),
                settings,
                normal_0: normals[0].clone(),
                normal_1: normals[1].clone(),
                normal_2: normals[2].clone(),
                normal_3: normals[3].clone(),
                normal_4: normals[4].clone(),
                normal_5: normals[5].clone(),
            },
            TerrainImages {
                color: handles,
                normal: normal_handles,
            },
        ))
    }

    /// The material of one quadrant of a synthetic `terrain`, drawn with textures a fixture already
    /// holds in memory. It carries the same settings a streamed quadrant does, so a fixture with
    /// overlay layers shows the weight field the shader interpolates rather than only the packed
    /// vertex attributes it falls back on.
    pub(crate) fn fixture(
        terrain: &TerrainSnapshot,
        quadrant: u8,
        textures: [Handle<Image>; 6],
    ) -> Result<Self, String> {
        let layers = crate::streaming::quadrant_layers(terrain, quadrant)?;
        let overlay_weights = crate::streaming::quadrant_overlay_weights(terrain, quadrant)?;
        Ok(Self {
            layer_0: Some(textures[0].clone()),
            layer_1: Some(textures[1].clone()),
            layer_2: Some(textures[2].clone()),
            layer_3: Some(textures[3].clone()),
            layer_4: Some(textures[4].clone()),
            layer_5: Some(textures[5].clone()),
            settings: TerrainSettings::for_quadrant(quadrant, layers.len(), &overlay_weights),
            normal_0: None,
            normal_1: None,
            normal_2: None,
            normal_3: None,
            normal_4: None,
            normal_5: None,
        })
    }

    /// Whether the shader reads this material's overlay weights from its uniform weight field rather
    /// than from the weights the mesh packs into its vertex attributes. False only for a material
    /// with no overlays to read: a quadrant whose sole layer is its base, or a stand-in terrain.
    pub(crate) fn reads_weight_field(&self) -> bool {
        self.settings.weight_source.x > 0.5
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
            settings: TerrainSettings::vertex_weights_only(0.0),
            normal_0: None,
            normal_1: None,
            normal_2: None,
            normal_3: None,
            normal_4: None,
            normal_5: None,
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
}

impl Default for WaterExtension {
    fn default() -> Self {
        Self {
            settings: WaterSettings {
                wave_scale_speed_strength: Vec4::new(0.006, 0.15, 0.32, 0.0),
                flow_direction: Vec4::new(0.8, 0.35, 0.0, 0.0),
            },
            reflection: None,
            flow_normal: None,
        }
    }
}

impl WaterExtension {
    pub fn with_reflection(reflection: Handle<Image>, flow_normal: Option<Handle<Image>>) -> Self {
        let has_flow_normal = flow_normal.is_some() as u8 as f32;
        Self {
            reflection: Some(reflection),
            flow_normal,
            settings: WaterSettings {
                wave_scale_speed_strength: Vec4::new(0.006, 0.15, 0.32, 0.0),
                flow_direction: Vec4::new(0.8, 0.35, 0.0, has_flow_normal),
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
            invert_culling: true,
            is_active: false,
            ..default()
        },
        RenderTarget::Image(image.into()),
        Transform::default(),
        DepthPrepass,
        OcclusionCulling,
        RenderLayers::layer(0),
        WaterReflectionCamera,
    ));
}

fn update_water_reflection_camera(
    main_camera: Query<
        &GlobalTransform,
        (
            With<crate::world::components::StreamingCamera>,
            Without<WaterReflectionCamera>,
        ),
    >,
    water: Query<&GlobalTransform, With<crate::world::components::WaterSurface>>,
    mut reflection_camera: Query<(&mut Transform, &mut Camera), With<WaterReflectionCamera>>,
    mut profiler: ResMut<ProfilingState>,
) {
    let started = std::time::Instant::now();
    let (Ok(main), Ok((mut reflection, mut camera))) =
        (main_camera.single(), reflection_camera.single_mut())
    else {
        return;
    };
    let Some(surface) = water.iter().min_by(|left, right| {
        let left_distance = (left.translation().y - main.translation().y).abs();
        let right_distance = (right.translation().y - main.translation().y).abs();
        left_distance.total_cmp(&right_distance)
    }) else {
        camera.is_active = false;
        return;
    };
    let water_y = surface.translation().y;
    *reflection = reflected_camera_transform(main, water_y);
    camera.is_active = true;
    profiler.record_elapsed("render/water_reflection_camera", started);
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
    use crate::world::cache::TerrainLayerSnapshot;
    use bevy::{
        image::ImageFilterMode,
        mesh::VertexAttributeValues,
        reflect::{PartialReflect, ReflectRef},
    };

    /// A cell whose four quadrants each carry a base layer plus the given overlays, in the order the
    /// `VTXT` lists are given, so a weight field built from it can be read back against the lists
    /// that produced it.
    fn terrain_fixture_with_overlays(
        cell_id: u32,
        overlays: &[Vec<(u16, f32)>],
    ) -> TerrainSnapshot {
        let mut layers = Vec::new();
        for quadrant in 0..4u8 {
            layers.push(TerrainLayerSnapshot {
                texture_form_id: 1,
                quadrant,
                layer: 0,
                is_base: true,
                weights: Vec::new(),
            });
            for (slot, weights) in overlays.iter().enumerate() {
                let layer = u16::try_from(slot + 1).expect("fixture overlay count fits a u16");
                layers.push(TerrainLayerSnapshot {
                    texture_form_id: u32::from(layer) + 6,
                    quadrant,
                    layer,
                    is_base: false,
                    weights: weights.clone(),
                });
            }
        }
        TerrainSnapshot {
            cell_id,
            width: 33,
            height: 33,
            heights: vec![0.0; 33 * 33],
            normals: [0, 0, 127].repeat(33 * 33),
            vertex_colors: vec![255; 33 * 33 * 3],
            layers,
            water_height: None,
            water_type_form_id: None,
        }
    }

    /// The samples of one overlay's grid, in `VTXT` vertex order.
    fn overlay_grid(sample: impl Fn(usize, usize) -> f32) -> Vec<(u16, f32)> {
        let mut grid = Vec::with_capacity(QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES);
        for y in 0..QUADRANT_WEIGHT_SAMPLES {
            for x in 0..QUADRANT_WEIGHT_SAMPLES {
                let index = y * QUADRANT_WEIGHT_SAMPLES + x;
                grid.push((
                    u16::try_from(index).expect("a sample index fits a u16"),
                    sample(x, y),
                ));
            }
        }
        grid
    }

    /// The weight a fragment receives from `grid_point` and `grid_weight` in `terrain.wgsl`: the
    /// quadrant-local coordinate clamped onto the `17x17` sample grid, then read bilinearly, clamped
    /// at the far edge. A model of the shader, not of the field: which sample of which word a
    /// coordinate reads is pinned against the shader's own expressions by
    /// `shader_weight_index_expressions_address_the_packed_field`.
    fn shader_weight(settings: &TerrainSettings, overlay: usize, coordinate: [f32; 2]) -> f32 {
        let last = (QUADRANT_WEIGHT_SAMPLES - 1) as f32;
        let sample = |x: usize, y: usize| -> f32 {
            let index = y * QUADRANT_WEIGHT_SAMPLES + x;
            settings.weights[overlay * OVERLAY_WEIGHT_WORDS + index / 4][index % 4]
        };
        // `grid_point`: clamping before the fraction is taken keeps the edge sample of a coordinate
        // an ulp outside the quadrant from blending its neighbour in.
        let column = coordinate[0].clamp(0.0, last);
        let row = coordinate[1].clamp(0.0, last);
        let blend = [column - column.floor(), row - row.floor()];
        let axis = |value: f32| -> (usize, usize) {
            let base = value.floor() as usize;
            (base, (base + 1).min(last as usize))
        };
        let (west, east) = axis(column);
        let (north, south) = axis(row);
        let top = sample(west, north) * (1.0 - blend[0]) + sample(east, north) * blend[0];
        let bottom = sample(west, south) * (1.0 - blend[0]) + sample(east, south) * blend[0];
        top * (1.0 - blend[1]) + bottom * blend[1]
    }

    /// The value of a `const NAME: u32 = <n>u;` declaration in `terrain.wgsl`.
    fn shader_constant(source: &str, name: &str) -> usize {
        let declaration = format!("const {name}: u32 = ");
        let rest = source
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `{declaration}`"))
            .1;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("`{declaration}` is not followed by a number: {rest}"))
    }

    /// The element count of a `field: array<vec4<f32>, <n>>` declaration in `terrain.wgsl`.
    fn shader_array_length(source: &str, field: &str) -> usize {
        let declaration = format!("{field}: array<vec4<f32>, ");
        let rest = source
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `{declaration}`"))
            .1;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("`{declaration}` is not followed by a number: {rest}"))
    }

    /// The field names of `struct NAME { .. }` in `terrain.wgsl`, in declaration order, so the
    /// uniform's layout can be compared with the Rust struct that is written into it.
    fn shader_struct_fields(source: &str, name: &str) -> Vec<String> {
        let body = source
            .split_once(&format!("struct {name} {{"))
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `struct {name}`"))
            .1
            .split_once('}')
            .expect("the struct declaration must be closed")
            .0;
        body.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .map(|line| {
                line.split_once(':')
                    .unwrap_or_else(|| panic!("`{line}` is not a field declaration"))
                    .0
                    .to_owned()
            })
            .collect()
    }

    /// The expression a `const NAME: u32 = <expression>;` declaration in `terrain.wgsl` holds, so a
    /// constant derived from another is read as the shader writes it.
    fn shader_constant_expression<'a>(source: &'a str, name: &str) -> &'a str {
        let declaration = format!("const {name}: u32 = ");
        source
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `{declaration}`"))
            .1
            .split_once(';')
            .expect("a const declaration must end with a semicolon")
            .0
            .trim()
    }

    /// The body of `fn NAME(..) { <body> }` in `terrain.wgsl`, for the helpers whose whole body is
    /// the arithmetic under test.
    fn shader_function_body<'a>(source: &'a str, name: &str) -> &'a str {
        source
            .split_once(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("terrain.wgsl declares no `fn {name}`"))
            .1
            .split_once('{')
            .expect("a function declaration must open its body")
            .1
            .split_once('}')
            .expect("a function body must be closed")
            .0
    }

    /// `source` without its whitespace, so a pinned expression survives a reformat of the file.
    fn without_whitespace(source: &str) -> String {
        source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }

    /// The value of one of `terrain.wgsl`'s `u32` expressions over the named `terms`: `+`, `-`, `*`,
    /// `/`, `%`, parentheses, literals with their `u` suffix, and identifiers. This runs the
    /// shader's own arithmetic rather than a copy of it, which is the only way a test can notice a
    /// transposed index - a copy of the expression transposes with it.
    fn evaluate_u32(expression: &str, terms: &[(&str, u32)]) -> u32 {
        /// The expression left to read, and what the shader's identifiers stand for.
        struct Parser<'a> {
            characters: Vec<char>,
            position: usize,
            terms: &'a [(&'a str, u32)],
        }

        impl<'a> Parser<'a> {
            fn skip_whitespace(&mut self) {
                while self
                    .characters
                    .get(self.position)
                    .is_some_and(|character| character.is_whitespace())
                {
                    self.position += 1;
                }
            }

            /// Reads `expected` if it is next, whitespace aside.
            fn eat(&mut self, expected: char) -> bool {
                self.skip_whitespace();
                if self.characters.get(self.position) == Some(&expected) {
                    self.position += 1;
                    return true;
                }
                false
            }

            /// `<operand> (('*' | '/' | '%') <operand>)*`, left to right as WGSL binds it.
            fn product(&mut self) -> u32 {
                let mut value = self.operand();
                loop {
                    if self.eat('*') {
                        value *= self.operand();
                    } else if self.eat('/') {
                        value /= self.operand();
                    } else if self.eat('%') {
                        value %= self.operand();
                    } else {
                        return value;
                    }
                }
            }

            /// `<product> (('+' | '-') <product>)*`.
            fn sum(&mut self) -> u32 {
                let mut value = self.product();
                loop {
                    if self.eat('+') {
                        value += self.product();
                    } else if self.eat('-') {
                        value -= self.product();
                    } else {
                        return value;
                    }
                }
            }

            /// The whole expression, for the messages of the assertions below.
            fn text(&self) -> String {
                self.characters.iter().collect()
            }

            /// A literal, one of the named terms, or a parenthesised sum.
            fn operand(&mut self) -> u32 {
                self.skip_whitespace();
                if self.eat('(') {
                    let value = self.sum();
                    assert!(
                        self.eat(')'),
                        "`{}` must close its parentheses",
                        self.text()
                    );
                    return value;
                }
                let mut name = String::new();
                while let Some(&character) = self.characters.get(self.position) {
                    if !character.is_ascii_alphanumeric() && character != '_' {
                        break;
                    }
                    name.push(character);
                    self.position += 1;
                }
                // A literal may carry WGSL's `u` suffix, which says nothing about its value.
                if let Ok(literal) = name.trim_end_matches('u').parse::<u32>() {
                    return literal;
                }
                let (_, value) = self
                    .terms
                    .iter()
                    .find(|(term, _)| *term == name.as_str())
                    .unwrap_or_else(|| {
                        panic!(
                            "`{}` names `{name}`, which the test does not define",
                            self.text()
                        )
                    });
                *value
            }
        }

        let mut parser = Parser {
            characters: expression.chars().collect(),
            position: 0,
            terms,
        };
        let value = parser.sum();
        parser.skip_whitespace();
        assert_eq!(
            parser.position,
            parser.characters.len(),
            "`{expression}` has a suffix the test cannot evaluate"
        );
        value
    }

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

    /// The shader tiles every layer `tiling` times across a cell (`uv * 8`), so the layer textures
    /// must be sampled with a repeating address mode. Bevy's default clamps to the edge, which
    /// stretched the textures in the frames: everything past the first tile read the edge texels.
    #[test]
    fn tiled_terrain_layers_are_sampled_with_a_repeating_sampler() {
        let sampler = terrain_layer_sampler();
        assert_eq!(sampler.address_mode_u, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_v, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_w, ImageAddressMode::Repeat);
        // Only the address mode deviates from the sampler Bevy installs by default (`ImagePlugin`'s
        // `ImageSamplerDescriptor::linear()`), so layer textures keep filtering as they did.
        let default_sampler = ImageSamplerDescriptor::linear();
        assert_eq!(sampler.mag_filter, default_sampler.mag_filter);
        assert_eq!(sampler.min_filter, default_sampler.min_filter);
        assert_eq!(sampler.mipmap_filter, default_sampler.mipmap_filter);
        assert_eq!(sampler.lod_min_clamp, default_sampler.lod_min_clamp);
        assert_eq!(sampler.lod_max_clamp, default_sampler.lod_max_clamp);
        assert_eq!(sampler.anisotropy_clamp, default_sampler.anisotropy_clamp);
        assert_eq!(sampler.compare, default_sampler.compare);
        assert_eq!(sampler.mag_filter, ImageFilterMode::Linear);
        assert_eq!(
            default_sampler.address_mode_u,
            ImageAddressMode::ClampToEdge
        );
        assert_eq!(
            default_sampler.address_mode_v,
            ImageAddressMode::ClampToEdge
        );
        assert!(
            TerrainSettings::vertex_weights_only(1.0)
                .tiling_and_layer_count
                .x
                > 1.0,
            "layers only need a repeating sampler because the shader tiles them"
        );
    }

    /// The weight field is the quadrant's own `17x17` grids, one per overlay, in the order
    /// `quadrant_layers` returns them: an overlay's `VTXT` list reaches the fragment stage at the
    /// samples it names, and every sample it does not name reads as opacity 0.
    #[test]
    fn weight_field_carries_every_overlay_grid_from_the_layer_list() {
        let terrain = terrain_fixture_with_overlays(
            0x0001_2345,
            &[
                vec![(0, 1.0), (17 + 1, 0.5), (16 * 17 + 16, 0.25)],
                vec![(17 * 8 + 8, 0.75)],
            ],
        );
        let grids = crate::streaming::quadrant_overlay_weights(&terrain, 0).unwrap();
        assert_eq!(grids.len(), 2, "one grid per overlay, base excluded");
        assert_eq!(
            grids[0].len(),
            QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES,
            "a grid covers the quadrant's whole sample square"
        );
        let settings = TerrainSettings::for_quadrant(0, 3, &grids);
        assert_eq!(
            settings.tiling_and_layer_count,
            Vec4::new(8.0, 8.0, 3.0, 0.0)
        );
        assert_eq!(settings.quadrant_origin.xy(), Vec2::ZERO);
        assert_eq!(
            settings.weight_source.x, 1.0,
            "the material carries the weight field"
        );

        let sample = |overlay: usize, x: usize, y: usize| -> f32 {
            let index = y * QUADRANT_WEIGHT_SAMPLES + x;
            settings.weights[overlay * OVERLAY_WEIGHT_WORDS + index / 4][index % 4]
        };
        assert_eq!(sample(0, 0, 0), 1.0);
        assert_eq!(sample(0, 1, 1), 0.5);
        assert_eq!(sample(0, 16, 16), 0.25);
        assert_eq!(sample(0, 5, 5), 0.0, "an unnamed sample is opacity 0");
        assert_eq!(sample(1, 8, 8), 0.75, "the second overlay's own grid");
        assert_eq!(
            sample(1, 0, 0),
            0.0,
            "the two grids do not bleed into each other"
        );
        for overlay in 2..OVERLAY_WEIGHT_SLOTS {
            assert_eq!(
                sample(overlay, 8, 8),
                0.0,
                "an overlay the quadrant does not use leaves its slot empty"
            );
        }
        assert_eq!(
            settings.weights[OVERLAY_WEIGHT_WORDS - 1].w,
            0.0,
            "the padding word after the last sample stays zero"
        );
    }

    /// Each quadrant reads its own origin out of the cell, so the same cell uv addresses the same
    /// ground in every quadrant.
    #[test]
    fn quadrant_origin_places_each_quadrant_inside_the_cell() {
        let origins: Vec<[f32; 2]> = (0..4)
            .map(|quadrant| {
                let settings = TerrainSettings::for_quadrant(quadrant, 2, &[]);
                assert_eq!(
                    settings.quadrant_origin.z, 0.0,
                    "only xy carries the origin"
                );
                [settings.quadrant_origin.x, settings.quadrant_origin.y]
            })
            .collect();
        assert_eq!(
            origins,
            [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            "the mesh's quadrants in the same order: west/east then south/north"
        );
    }

    /// The weight field is only worth reading where there are overlays to read. A quadrant whose only
    /// layer is its base has none, and the packed vertex attributes carry none either, so the shader
    /// is told to skip the field's five lookups altogether.
    #[test]
    fn weight_source_is_set_only_for_quadrants_with_overlays() {
        let grid = vec![1.0; QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES];
        assert_eq!(
            TerrainSettings::for_quadrant(0, 2, &[grid]).weight_source,
            Vec4::X
        );
        assert_eq!(
            TerrainSettings::for_quadrant(0, 1, &[]).weight_source,
            Vec4::ZERO,
            "a base-only quadrant has no overlay to read"
        );
        assert_eq!(
            TerrainSettings::vertex_weights_only(6.0).weight_source,
            Vec4::ZERO,
            "a material with no weight field reads the packed attributes"
        );
    }

    /// A fixture material carries the quadrant's own weight field, so the no-game-data scene draws
    /// its overlays through the interpolation the streamed path uses instead of the packed tangent.
    #[test]
    fn fixture_materials_carry_the_quadrants_weight_field() {
        let sample = QUADRANT_WEIGHT_SAMPLES * 8 + 8;
        let terrain =
            terrain_fixture_with_overlays(0x0001_2345, &[vec![(0, 1.0), (sample as u16, 0.5)]]);
        for quadrant in 0..4u8 {
            let extension = TerrainExtension::fixture(
                &terrain,
                quadrant,
                std::array::from_fn(|_| Handle::<Image>::default()),
            )
            .expect("the fixture material must build");
            assert!(
                extension.reads_weight_field(),
                "quadrant {quadrant} must render its overlay through the weight field"
            );
            assert_eq!(
                extension.settings.quadrant_origin.xy(),
                Vec2::new(f32::from(quadrant % 2), f32::from(quadrant / 2)),
                "each quadrant reads its own origin"
            );
            assert_eq!(extension.settings.weights[0][0], 1.0);
            assert_eq!(
                extension.settings.weights[sample / 4][sample % 4],
                0.5,
                "the fixture's own VTXT opacity reaches the field"
            );
        }
    }

    /// The overlay weight field is the quadrant's own sample grid, interpolated bilinearly. A layer
    /// that is full strength on one sample and absent on the next must reach the fragment as the
    /// linear interpolation of the two - `0.5` halfway between them - not as the sharpened ramp the
    /// packed vertex attributes produce.
    #[test]
    fn weight_field_interpolates_linearly_between_samples() {
        let terrain = terrain_fixture_with_overlays(0x0001_2345, &[vec![(0, 1.0)]]);
        let grids = crate::streaming::quadrant_overlay_weights(&terrain, 0).unwrap();
        let settings = TerrainSettings::for_quadrant(0, 2, &grids);

        assert_eq!(shader_weight(&settings, 0, [0.0, 0.0]), 1.0);
        // Sample 0 is the quadrant's (x = 0, y = 0) corner and its x neighbour is empty, so one
        // sample step in x crosses an opacity of 1.0 down to 0.0.
        let halfway = shader_weight(&settings, 0, [0.5, 0.0]);
        assert!(
            (halfway - 0.5).abs() < 1.0e-5,
            "the midpoint of a 1.0 -> 0.0 edge must read 0.5, not the packed tangent's 0.25: {halfway}"
        );
        assert!((shader_weight(&settings, 0, [0.25, 0.0]) - 0.75).abs() < 1.0e-5);
        assert!((shader_weight(&settings, 0, [0.75, 0.0]) - 0.25).abs() < 1.0e-5);
        // Both axes: the field is bilinear, so the centre of the quad whose only full sample is the
        // corner reads 0.25 - what Skyrim's interpolation gives, and not what the triangle split
        // would if the weight stayed per vertex.
        assert!(
            (shader_weight(&settings, 0, [0.5, 0.5]) - 0.25).abs() < 1.0e-5,
            "the quad centre is the bilinear blend of its four samples"
        );
        // At the quadrant's far edge the field clamps to the last sample instead of wrapping.
        assert_eq!(shader_weight(&settings, 0, [16.0, 16.0]), 0.0);
        assert_eq!(shader_weight(&settings, 0, [16.0, 0.0]), 0.0);
        assert_eq!(shader_weight(&settings, 0, [0.0, 16.0]), 0.0);
        for overlay in 1..OVERLAY_WEIGHT_SLOTS {
            assert_eq!(shader_weight(&settings, overlay, [0.0, 0.0]), 0.0);
        }
    }

    /// A coordinate outside the sample square reads the sample it is clamped onto. `grid_point`
    /// clamps before `grid_weight` takes its blend fraction, so a coordinate an ulp below the
    /// quadrant's west or south edge reads that edge sample rather than blending its neighbour in.
    #[test]
    fn weight_field_clamps_coordinates_onto_the_sample_square() {
        let last = (QUADRANT_WEIGHT_SAMPLES - 1) as f32;
        let edge = QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES - 1;
        let terrain =
            terrain_fixture_with_overlays(0x0001_2345, &[vec![(0, 1.0), (edge as u16, 0.25)]]);
        let grids = crate::streaming::quadrant_overlay_weights(&terrain, 0).unwrap();
        let settings = TerrainSettings::for_quadrant(0, 2, &grids);

        assert_eq!(shader_weight(&settings, 0, [-1.0e-7, 0.0]), 1.0);
        assert_eq!(shader_weight(&settings, 0, [0.0, -1.0e-7]), 1.0);
        assert_eq!(shader_weight(&settings, 0, [last + 1.0e-7, last]), 0.25);
        assert_eq!(shader_weight(&settings, 0, [last, last + 1.0e-7]), 0.25);
    }

    /// The mechanism the weight field replaces. `build_terrain_quadrant_mesh` still packs overlays
    /// into `ATTRIBUTE_TANGENT` for materials without a weight field, and Bevy re-normalizes the
    /// tangent's direction in the vertex shader (`mesh_tangent_local_to_world`), so the direction
    /// interpolates at full length while the magnitude it is multiplied by does not: halfway across
    /// the same 1.0 -> 0.0 edge the product reads 0.25 where the opacity is 0.5. Streamed quadrants
    /// read the weight field instead.
    #[test]
    fn packed_vertex_weights_sharpen_the_midpoint_to_a_quarter() {
        let terrain = terrain_fixture_with_overlays(0x0001_2345, &[vec![(0, 1.0)]]);
        let mesh = crate::streaming::build_terrain_quadrant_mesh(&terrain, 0).unwrap();
        let VertexAttributeValues::Float32x4(tangents) =
            mesh.attribute(Mesh::ATTRIBUTE_TANGENT).unwrap()
        else {
            panic!("terrain tangents must be Float32x4");
        };
        assert_eq!(
            tangents[0],
            [1.0, 0.0, 0.0, 1.0],
            "full weight, unit direction"
        );
        assert_eq!(tangents[1], [0.0; 4], "no weight at all");
        // The vertex shader normalizes xyz per vertex and the rasterizer interpolates the result;
        // the fragment shader multiplies the interpolated xyz by the interpolated magnitude.
        let midpoint: [f32; 4] =
            std::array::from_fn(|axis| (tangents[0][axis] + tangents[1][axis]) * 0.5);
        let reconstructed = midpoint[0] * midpoint[3].abs();
        assert!(
            (reconstructed - 0.25).abs() < 1.0e-5,
            "the packed path reads {reconstructed} where the interpolated opacity is 0.5"
        );
    }

    /// The mesh's cell UVs and the weight field agree sample for sample: `uv * 2 - quadrant_origin`
    /// scaled onto the grid lands exactly on the sample the shader must read for that vertex, with
    /// no scaling or orientation drift between the two.
    #[test]
    fn quadrant_uvs_address_the_weight_grid_sample_for_sample() {
        let samples = QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES;
        // A `VTXT` list that names every sample with its own index, so the weight read at a vertex
        // says which sample the shader looked up.
        let terrain = terrain_fixture_with_overlays(
            0x0001_2345,
            &[overlay_grid(|x, y| {
                (y * QUADRANT_WEIGHT_SAMPLES + x) as f32 / samples as f32
            })],
        );
        let last = (QUADRANT_WEIGHT_SAMPLES - 1) as f32;
        for quadrant in 0..4u8 {
            let mesh = crate::streaming::build_terrain_quadrant_mesh(&terrain, quadrant).unwrap();
            let grids = crate::streaming::quadrant_overlay_weights(&terrain, quadrant).unwrap();
            let settings = TerrainSettings::for_quadrant(quadrant, 2, &grids);
            let VertexAttributeValues::Float32x2(uvs) =
                mesh.attribute(Mesh::ATTRIBUTE_UV_0).unwrap()
            else {
                panic!("terrain UVs must be Float32x2");
            };
            assert_eq!(uvs.len(), samples, "one vertex per grid sample");
            for (sample, uv) in uvs.iter().enumerate() {
                let coordinate = [
                    (uv[0] * 2.0 - settings.quadrant_origin.x) * last,
                    (uv[1] * 2.0 - settings.quadrant_origin.y) * last,
                ];
                let expected = sample as f32 / samples as f32;
                assert!(
                    (shader_weight(&settings, 0, coordinate) - expected).abs() < 1.0e-5,
                    "quadrant {quadrant} vertex {sample} reads {coordinate:?}, not its own sample"
                );
            }
        }
    }

    /// The uniform's Rust layout and the shader's must say the same numbers, and list the same
    /// fields in the same order - an insertion on one side alone shifts every field after it, and
    /// the shader then reads its neighbour's bytes.
    #[test]
    fn terrain_uniform_layout_matches_the_shader_source() {
        let source = include_str!("shaders/terrain.wgsl");
        assert_eq!(
            shader_constant(source, "WEIGHT_GRID_SIDE"),
            QUADRANT_WEIGHT_SAMPLES
        );
        assert_eq!(
            shader_constant(source, "WEIGHT_GRID_WORDS"),
            OVERLAY_WEIGHT_WORDS
        );
        assert_eq!(shader_array_length(source, "weights"), WEIGHT_FIELD_WORDS);
        assert_eq!(
            WEIGHT_FIELD_WORDS,
            OVERLAY_WEIGHT_SLOTS * OVERLAY_WEIGHT_WORDS
        );
        assert_eq!(
            OVERLAY_WEIGHT_WORDS, 73,
            "289 samples four to a vec4, rounded up"
        );
        assert_eq!(WEIGHT_FIELD_WORDS, 365, "five overlays of 73 vec4s");

        let settings = TerrainSettings::for_quadrant(
            0,
            2,
            &[vec![1.0; QUADRANT_WEIGHT_SAMPLES * QUADRANT_WEIGHT_SAMPLES]],
        );
        let ReflectRef::Struct(fields) = settings.reflect_ref() else {
            panic!("TerrainSettings must reflect as a struct");
        };
        let rust_fields: Vec<String> = (0..fields.field_len())
            .map(|index| {
                fields
                    .name_at(index)
                    .expect("every field is named")
                    .to_owned()
            })
            .collect();
        assert_eq!(rust_fields, shader_struct_fields(source, "TerrainSettings"));
    }

    /// The shader's own index arithmetic, against the field the Rust side packs: which word and
    /// component a sample occupies, and which sample each corner of a grid point names. The
    /// expressions are read out of `terrain.wgsl` and evaluated, so a transposed index fails here -
    /// the model in `shader_weight` would transpose with it and keep passing.
    #[test]
    fn shader_weight_index_expressions_address_the_packed_field() {
        let source = include_str!("shaders/terrain.wgsl");
        let side = shader_constant(source, "WEIGHT_GRID_SIDE") as u32;
        let words = shader_constant(source, "WEIGHT_GRID_WORDS") as u32;
        // Every sample carries a value of its own, so an index that lands anywhere but on its own
        // sample reads a different opacity rather than a plausible one.
        let grid = |overlay: u32| -> Vec<f32> {
            (0..side * side)
                .map(|sample| (overlay * side * side + sample) as f32)
                .collect()
        };
        let grids: Vec<Vec<f32>> = (0..OVERLAY_WEIGHT_SLOTS as u32).map(grid).collect();
        let settings = TerrainSettings::for_quadrant(0, OVERLAY_WEIGHT_SLOTS + 1, &grids);

        // How `packed_weight` splits a sample between the word that holds it and the component of
        // that word.
        let packed = shader_function_body(source, "packed_weight");
        let field = packed
            .split_once("terrain.weights[")
            .expect("`packed_weight` must read the weight field")
            .1;
        let (word_of, rest) = field.split_once(']').expect("the word index must close");
        let (_, rest) = rest.split_once('[').expect("the component index must open");
        let component_of = rest.split_once(']').expect("the component must close").0;
        let (word_of, component_of) = (word_of.trim(), component_of.trim());

        // How `grid_weight` names the sample each corner of the coordinate reads, in the order the
        // bilinear blend takes them: west and east of the north row, then of the south row.
        let grid_weight = shader_function_body(source, "grid_weight");
        let corners: Vec<&str> = grid_weight
            .split("packed_weight(overlay, ")
            .skip(1)
            .map(|call| call.split(')').next().expect("a call must close"))
            .collect();
        assert_eq!(
            corners.len(),
            4,
            "the blend reads the four samples around the coordinate"
        );

        // The last row and column have no next sample - the shader clamps `next` there - so the
        // corners are evaluated only where all four neighbours are on the grid.
        for row in [0u32, 1, 7, 15] {
            for column in [0u32, 1, 7, 15] {
                let terms = [
                    ("west", column),
                    ("east", column + 1),
                    ("north", row),
                    ("south", row + 1),
                    ("WEIGHT_GRID_SIDE", side),
                ];
                let samples: Vec<u32> = corners
                    .iter()
                    .map(|corner| evaluate_u32(corner, &terms))
                    .collect();
                assert_eq!(
                    samples,
                    [
                        row * side + column,
                        row * side + column + 1,
                        (row + 1) * side + column,
                        (row + 1) * side + column + 1,
                    ],
                    "the corners of grid point ({column}, {row}) must be its own row and column"
                );
                // Three of the four corners, over three overlays: the field's first, middle and last
                // slots, so an index that reads another overlay's word fails too.
                let probes = [(0u32, samples[0]), (2, samples[3]), (4, samples[2])];
                for (overlay, sample) in probes {
                    let word_terms = [
                        ("overlay", overlay),
                        ("sample", sample),
                        ("WEIGHT_GRID_WORDS", words),
                    ];
                    let word = evaluate_u32(word_of, &word_terms) as usize;
                    let component = evaluate_u32(component_of, &[("sample", sample)]) as usize;
                    assert_eq!(
                        settings.weights[word][component],
                        (overlay * side * side + sample) as f32,
                        "overlay {overlay} sample {sample} must read the Rust side's value"
                    );
                }
            }
        }
    }

    /// The parts of `terrain.wgsl`'s weight arithmetic the evaluator cannot run, pinned as the
    /// shader writes them: the two places that bound the sample grid, where a bound of
    /// `WEIGHT_GRID_SIDE` instead of the last sample would read the next quadrant's edge texels
    /// along this one, and the fragment's quadrant-local coordinate.
    #[test]
    fn shader_weight_field_bounds_its_grid_at_the_last_sample() {
        let source = include_str!("shaders/terrain.wgsl");
        let side = evaluate_u32(shader_constant_expression(source, "WEIGHT_GRID_SIDE"), &[]);
        let last = evaluate_u32(
            shader_constant_expression(source, "GRID_LAST_SAMPLE"),
            &[("WEIGHT_GRID_SIDE", side)],
        );
        assert_eq!(
            last,
            side - 1,
            "the grid's far edge is its last sample index, not the grid's side"
        );

        let point = "clamp(coordinate, vec2<f32>(0.0), vec2<f32>(f32(GRID_LAST_SAMPLE)))";
        assert!(
            without_whitespace(shader_function_body(source, "grid_point"))
                .contains(&without_whitespace(point)),
            "a coordinate must be clamped onto the sample square before its blend fraction is taken"
        );
        let next = "min(base + vec2<u32>(1u), vec2<u32>(GRID_LAST_SAMPLE))";
        assert!(
            without_whitespace(shader_function_body(source, "grid_weight"))
                .contains(&without_whitespace(next)),
            "the blend must pair the far edge's last sample with itself"
        );
        let coordinate = "(in.uv * 2.0 - terrain.quadrant_origin.xy) * f32(GRID_LAST_SAMPLE)";
        assert!(
            without_whitespace(source).contains(&without_whitespace(coordinate)),
            "the fragment's coordinate must be quadrant-local, on the grid's own scale"
        );
    }
}
