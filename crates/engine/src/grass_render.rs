//! Authored grass meshes, drawn with one instanced draw per primitive and cell.
//!
//! The low-level draw follows Bevy's `custom_shader_instancing` example. Both
//! passes use our own shader and bindings; no Bevy shader source is patched.
use crate::{
    grass::{GrassBatch, GrassFade, GrassInstance, GrassStreaming},
    world::components::StreamingCamera,
};
use bevy::{
    asset::{
        AssetId, LoadState, RecursiveDependencyLoadState, RenderAssetUsages, embedded_asset,
        load_embedded_asset,
    },
    core_pipeline::{
        core_3d::{AlphaMask3d, CORE_3D_DEPTH_FORMAT},
        prepass::{AlphaMask3dPrepass, OpaqueNoLightmap3dBatchSetKey, OpaqueNoLightmap3dBinKey},
        tonemapping::{Tonemapping, TonemappingLuts, get_lut_bindings},
    },
    ecs::{
        query::QueryItem,
        system::{SystemParamItem, lifetimeless::*},
    },
    gltf::{Gltf, GltfMaterial, GltfMesh, GltfNode},
    image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor},
    mesh::{MeshVertexBufferLayoutRef, VertexBufferLayout},
    prelude::*,
    render::{
        Render, RenderApp, RenderStartup, RenderSystems,
        camera::ExtractedCamera,
        extract_component::{ExtractComponent, ExtractComponentPlugin},
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        mesh::{RenderMesh, RenderMeshBufferInfo, allocator::MeshAllocator},
        render_asset::RenderAssets,
        render_phase::{
            AddRenderCommand, BinnedRenderPhaseType, DrawFunctions, PhaseItem, RenderCommand,
            RenderCommandResult, SetItemPipeline, TrackedRenderPass, ViewBinnedRenderPhases,
        },
        render_resource::{binding_types::*, *},
        renderer::{RenderDevice, RenderQueue},
        sync_component::SyncComponent,
        sync_world::MainEntity,
        texture::GpuImage,
        view::{ExtractedView, RetainedViewEntity, ViewUniform, ViewUniformOffset, ViewUniforms},
    },
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct GrassRenderPlugin;

/// Deterministic ON/OFF controls for explicit shot capture runs.
#[derive(Resource, Clone, Copy, Debug)]
pub(crate) struct GrassCaptureControls {
    pub(crate) capture: bool,
}

/// Capture readiness requires an issued main draw for every admitted primitive.
#[derive(Resource, Default, Debug)]
pub struct GrassRenderMetrics {
    pub ready: bool,
    pub failures: u64,
}

impl Plugin for GrassRenderPlugin {
    /// Install grass extraction, preparation and render-phase systems.
    fn build(&self, app: &mut App) {
        let config = app.world().resource::<crate::config::EngineConfig>();
        let controls = GrassCaptureControls {
            capture: config.shots.is_some(),
        };
        let tuning = crate::grass_tuning::GrassTuningState::new(config.grass_tuning.clone());
        app.insert_resource(tuning)
            .insert_resource(controls)
            .add_systems(Update, crate::grass_tuning::reload_grass_tuning);
        embedded_asset!(app, "shaders/grass.wgsl");
        let upload_budget = app
            .world()
            .resource::<crate::config::EngineConfig>()
            .grass_uploads_per_frame;
        app.init_resource::<GrassModels>()
            .init_resource::<GrassRenderMetrics>()
            .insert_resource(GrassUploadBudget(upload_budget))
            .add_plugins(ExtractResourcePlugin::<GrassUploadBudget>::default())
            .add_plugins((
                ExtractComponentPlugin::<GrassPrimitive>::default(),
                ExtractComponentPlugin::<GrassView>::default(),
            ))
            .add_systems(Startup, create_wind)
            .add_systems(
                PostUpdate,
                (load_grass_models, attach_grass_primitives)
                    .chain()
                    .after(GrassStreaming)
                    .before(bevy::transform::TransformSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                update_grass_uniforms.after(bevy::transform::TransformSystems::Propagate),
            );
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .insert_resource(controls)
            .add_render_command::<AlphaMask3d, DrawGrass>()
            .init_resource::<GrassInstanceBuffers>()
            .init_resource::<GrassQueuedEntities>()
            .add_render_command::<AlphaMask3dPrepass, DrawGrass>()
            .init_resource::<SpecializedMeshPipelines<GrassPipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(
                Render,
                (
                    prepare_grass.in_set(RenderSystems::PrepareResources),
                    prepare_view.in_set(RenderSystems::PrepareBindGroups),
                    queue_grass
                        .in_set(RenderSystems::QueueMeshes)
                        .after(bevy::pbr::queue_material_meshes)
                        .after(bevy::pbr::queue_prepass_material_meshes),
                ),
            );
    }
}

#[derive(Component, Clone, ExtractComponent)]
struct GrassView;

#[derive(Resource)]
struct WindTexture(Handle<Image>);

#[derive(Resource, Default)]
struct GrassModels(HashMap<String, GrassModel>);

struct GrassModel {
    handle: Handle<Gltf>,
    templates: Option<Vec<PrimitiveTemplate>>,
    build: TemplateBuild,
    failed: bool,
}

#[derive(Clone)]
struct PrimitiveTemplate {
    mesh: Handle<Mesh>,
    texture: Handle<Image>,
    normal_texture: Handle<Image>,
    model: Mat4,
    model_normal: Mat4,
    color: Vec4,
    /// Authored mask threshold; negative means opaque and bypasses alpha coverage.
    cutoff: f32,
    uv_transform: Mat3,
    normal_params: Vec4,
}

/// Root discovery, hierarchy traversal and primitive extraction resume between frames.
#[derive(Default)]
struct TemplateBuild {
    scan_node: usize,
    scan_child: usize,
    children: HashSet<AssetId<GltfNode>>,
    root: usize,
    pending: Vec<TemplateWork>,
    visited: HashSet<AssetId<GltfNode>>,
    output: Vec<PrimitiveTemplate>,
}

enum TemplateWork {
    Node(Handle<GltfNode>, Mat4),
    Children(Handle<GltfNode>, Mat4, usize),
    Primitive(Handle<GltfMesh>, Mat4, usize),
}

struct FrameBudget {
    left: usize,
}

impl FrameBudget {
    /// Initialize a per-frame allowance, treating zero as unlimited.
    fn new(limit: usize) -> Self {
        Self {
            left: if limit == 0 { usize::MAX } else { limit },
        }
    }
    /// Consume one allowance unit if work remains this frame.
    fn spend(&mut self) -> bool {
        if self.left == 0 {
            return false;
        }
        self.left -= 1;
        true
    }
}

#[derive(Resource, Clone, ExtractResource)]
struct GrassUploadBudget(usize);

/// Primitive submeshes share one GPU instance allocation for their species batch.
#[derive(Resource, Default)]
struct GrassInstanceBuffers(HashMap<usize, (Weak<Vec<GrassInstance>>, Buffer)>);

#[derive(Component)]
struct GrassAttached;

#[derive(Component, Default)]
struct GrassAttachmentProgress(usize);

/// The immutable instance array is shared through extraction, never copied per frame.
#[derive(Component, Clone)]
struct GrassPrimitive {
    suppress_draw: bool,
    instances: Arc<Vec<GrassInstance>>,
    mesh: Handle<Mesh>,
    texture: Handle<Image>,
    normal_texture: Handle<Image>,
    wind: Handle<Image>,
    uniform: GrassUniform,
    model: Mat4,
    slot_count: u32,
    drawn: Arc<AtomicBool>,
}

impl SyncComponent for GrassPrimitive {
    type Target = Self;
}
impl ExtractComponent for GrassPrimitive {
    type QueryData = &'static Self;
    type QueryFilter = ();
    type Out = Self;
    /// Share immutable primitive data and draw proof with the render world.
    fn extract_component(item: QueryItem<'_, '_, Self::QueryData>) -> Option<Self> {
        Some(item.clone())
    }
}

#[derive(Clone, ShaderType)]
struct GrassUniform {
    world_from_cell: Mat4,
    model: Mat4,
    model_normal: Mat4,
    color: Vec4,
    height_wind: Vec4,
    fade: Vec4,
    absolute_grid: Vec4,
    sun_direction: Vec4,
    sun_color: Vec4,
    ambient: Vec4,
    uv_transform: Mat3,
    normal_params: Vec4,
    tuning_light: Vec4,
    tuning_surface: Vec4,
    tuning_color: Vec4,
    tuning_specular: Vec4,
}

/// A small, shared wind field; generated data, not an authored grass replacement.
fn create_wind(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut bytes = Vec::with_capacity(64 * 64);
    for y in 0..64 {
        for x in 0..64 {
            let a = x as f32 * std::f32::consts::TAU / 64.0;
            let b = y as f32 * std::f32::consts::TAU / 64.0;
            bytes.push(
                ((a.sin() * b.cos() * 0.5 + (a * 3.0 + b * 2.0).sin() * 0.25 + 1.0) * 127.0) as u8,
            );
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bytes,
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    commands.insert_resource(WindTexture(images.add(image)));
}

/// Advance bounded model-template loading for requested grass species.
fn load_grass_models(
    batches: Query<&GrassBatch>,
    server: Res<AssetServer>,
    mut models: ResMut<GrassModels>,
) {
    for batch in &batches {
        models
            .0
            .entry(batch.model_path.clone())
            .or_insert_with(|| GrassModel {
                handle: server.load(batch.model_path.clone()),
                templates: None,
                build: TemplateBuild::default(),
                failed: false,
            });
    }
}

/// Cache authored transforms and materials once per model with bounded main-thread work.
fn build_templates(
    build: &mut TemplateBuild,
    gltf: &Gltf,
    nodes: &Assets<GltfNode>,
    meshes: &Assets<GltfMesh>,
    materials: &Assets<GltfMaterial>,
    budget: &mut FrameBudget,
) -> Result<bool, &'static str> {
    while build.scan_node < gltf.nodes.len() && budget.spend() {
        let node = nodes
            .get(&gltf.nodes[build.scan_node])
            .ok_or("missing loaded grass node")?;
        if let Some(child) = node.children.get(build.scan_child) {
            build.children.insert(child.id());
            build.scan_child += 1;
        } else {
            build.scan_node += 1;
            build.scan_child = 0;
        }
    }
    if build.scan_node < gltf.nodes.len() {
        return Ok(false);
    }
    while build.root < gltf.nodes.len() && budget.spend() {
        let node = &gltf.nodes[build.root];
        if !build.children.contains(&node.id()) {
            build
                .pending
                .push(TemplateWork::Node(node.clone(), Mat4::IDENTITY));
        }
        build.root += 1;
    }
    if build.root < gltf.nodes.len() {
        return Ok(false);
    }
    while !build.pending.is_empty() && budget.spend() {
        match build.pending.pop().unwrap() {
            TemplateWork::Node(handle, parent) => {
                if !build.visited.insert(handle.id()) {
                    return Err("grass node hierarchy is not a tree");
                }
                let node = nodes.get(&handle).ok_or("missing loaded grass node")?;
                let model = parent * node.transform.to_matrix();
                if !model.is_finite() || model.determinant().abs() < 1.0e-12 {
                    return Err("invalid grass node transform");
                }
                if let Some(mesh) = &node.mesh {
                    build
                        .pending
                        .push(TemplateWork::Primitive(mesh.clone(), model, 0));
                }
                build.pending.push(TemplateWork::Children(handle, model, 0));
            }
            TemplateWork::Children(handle, model, index) => {
                let node = nodes.get(&handle).ok_or("missing loaded grass node")?;
                if let Some(child) = node.children.get(index) {
                    build
                        .pending
                        .push(TemplateWork::Children(handle, model, index + 1));
                    build.pending.push(TemplateWork::Node(child.clone(), model));
                }
            }
            TemplateWork::Primitive(handle, model, index) => {
                let mesh = meshes.get(&handle).ok_or("missing loaded grass mesh")?;
                if let Some(primitive) = mesh.primitives.get(index) {
                    let material = primitive
                        .material
                        .as_ref()
                        .map(|handle| materials.get(handle).ok_or("missing loaded grass material"))
                        .transpose()?;
                    let tags = primitive.material_extras.as_ref().and_then(|extras| {
                        serde_json::from_str::<serde_json::Value>(&extras.value).ok()
                    });
                    let convention = tags
                        .as_ref()
                        .and_then(|v| v.pointer("/openSkyrim/normalConvention"))
                        .and_then(|v| v.as_str());
                    let normal_supported = convention != Some("model_space")
                        && material
                            .is_none_or(|m| m.normal_map_channel == bevy::mesh::UvChannel::Uv0);
                    if !normal_supported && material.is_some_and(|m| m.normal_map_texture.is_some())
                    {
                        warn!(
                            "grass primitive normal map needs unsupported model-space or UV1 mapping; using geometry normal"
                        );
                    }
                    build.output.push(PrimitiveTemplate {
                        mesh: primitive.mesh.clone(),
                        texture: material
                            .and_then(|material| material.base_color_texture.clone())
                            .unwrap_or_default(),
                        normal_texture: material
                            .and_then(|m| m.normal_map_texture.clone())
                            .unwrap_or_default(),
                        normal_params: Vec4::new(
                            u32::from(
                                normal_supported
                                    && material.is_some_and(|m| m.normal_map_texture.is_some()),
                            ) as f32,
                            if convention == Some("directx") {
                                -1.0
                            } else {
                                1.0
                            },
                            0.0,
                            0.0,
                        ),
                        model,
                        model_normal: model.inverse().transpose(),
                        color: material.map_or(Vec4::ONE, |material| {
                            Vec4::from_array(material.base_color.to_linear().to_f32_array())
                        }),
                        cutoff: material.map_or(-1.0, |material| match material.alpha_mode {
                            AlphaMode::Opaque => -1.0,
                            AlphaMode::Mask(cutoff) => cutoff,
                            _ => 0.5,
                        }),
                        uv_transform: material.map_or(Mat3::IDENTITY, |material| {
                            let transform = material.uv_transform;
                            Mat3::from_cols(
                                transform.matrix2.x_axis.extend(0.0),
                                transform.matrix2.y_axis.extend(0.0),
                                transform.translation.extend(1.0),
                            )
                        }),
                    });
                    build
                        .pending
                        .push(TemplateWork::Primitive(handle, model, index + 1));
                }
            }
        }
    }
    if !build.pending.is_empty() {
        return Ok(false);
    }
    if build.output.is_empty() {
        return Err("grass model contains no renderable primitives");
    }
    Ok(true)
}

/// Record a model failure once and retain its diagnostic state.
fn fail_model(
    model: &mut GrassModel,
    path: &str,
    reason: impl std::fmt::Display,
    metrics: &mut GrassRenderMetrics,
) {
    if model.failed {
        return;
    }
    model.failed = true;
    metrics.failures = metrics.failures.saturating_add(1);
    warn!("grass model {path} failed: {reason}");
}

/// Admit primitives gradually; expensive hierarchy work is shared by every species and cell.
#[allow(clippy::too_many_arguments)]
fn attach_grass_primitives(
    mut commands: Commands,
    batches: Query<(Entity, &GrassBatch, Option<&GrassAttachmentProgress>), Without<GrassAttached>>,
    mut models: ResMut<GrassModels>,
    gltfs: Res<Assets<Gltf>>,
    nodes: Res<Assets<GltfNode>>,
    meshes: Res<Assets<GltfMesh>>,
    materials: Res<Assets<GltfMaterial>>,
    wind: Option<Res<WindTexture>>,
    config: Res<crate::config::EngineConfig>,
    server: Res<AssetServer>,
    mut metrics: ResMut<GrassRenderMetrics>,
) {
    let Some(wind) = wind else { return };
    let mut budget = FrameBudget::new(config.grass_uploads_per_frame);
    for (path, model) in &mut models.0 {
        if model.failed || model.templates.is_some() {
            continue;
        }
        let failure = match server.get_load_state(model.handle.id()) {
            Some(LoadState::Failed(reason)) => Some(reason),
            _ => match server.get_recursive_dependency_load_state(model.handle.id()) {
                Some(RecursiveDependencyLoadState::Failed(reason)) => Some(reason),
                _ => None,
            },
        };
        if let Some(reason) = failure {
            fail_model(model, path, reason, &mut metrics);
            continue;
        }
        if !server.is_loaded_with_dependencies(model.handle.id()) {
            continue;
        }
        let Some(gltf) = gltfs.get(&model.handle) else {
            continue;
        };
        match build_templates(
            &mut model.build,
            gltf,
            &nodes,
            &meshes,
            &materials,
            &mut budget,
        ) {
            Ok(true) => model.templates = Some(std::mem::take(&mut model.build.output)),
            Ok(false) => {}
            Err(reason) => fail_model(model, path, reason, &mut metrics),
        }
    }
    for (entity, batch, progress) in &batches {
        let Some(templates) = models
            .0
            .get(&batch.model_path)
            .and_then(|model| model.templates.as_ref())
        else {
            continue;
        };
        let mut index = progress.map_or(0, |progress| progress.0);
        while index < templates.len() && budget.spend() {
            let template = &templates[index];
            let primitive = GrassPrimitive {
                suppress_draw: false,
                instances: batch.instances.clone(),
                mesh: template.mesh.clone(),
                texture: template.texture.clone(),
                normal_texture: template.normal_texture.clone(),
                wind: wind.0.clone(),
                model: template.model,
                slot_count: batch.slot_count,
                drawn: Arc::new(AtomicBool::new(false)),
                uniform: GrassUniform {
                    world_from_cell: Mat4::IDENTITY,
                    model: template.model,
                    model_normal: template.model_normal,
                    color: template.color,
                    height_wind: Vec4::new(
                        batch.height_min,
                        batch.height_span,
                        batch.wave_period,
                        template.cutoff,
                    ),
                    fade: Vec4::ZERO,
                    absolute_grid: Vec4::new(
                        batch.grid[0] as f32 * 4096.0,
                        batch.grid[1] as f32 * 4096.0,
                        batch.flags as f32,
                        batch.slot_count as f32,
                    ),
                    sun_direction: Vec4::Y,
                    sun_color: Vec4::ZERO,
                    ambient: Vec4::ZERO,
                    uv_transform: template.uv_transform,
                    normal_params: template.normal_params,
                    tuning_light: crate::grass_tuning::GrassTuning::default().as_uniforms()[0],
                    tuning_surface: crate::grass_tuning::GrassTuning::default().as_uniforms()[1],
                    tuning_color: crate::grass_tuning::GrassTuning::default().as_uniforms()[2],
                    tuning_specular: crate::grass_tuning::GrassTuning::default().as_uniforms()[3],
                },
            };
            commands.entity(entity).with_children(|parent| {
                parent.spawn((
                    Mesh3d(primitive.mesh.clone()),
                    primitive,
                    Transform::default(),
                    Visibility::Visible,
                ));
            });
            index += 1;
        }
        if index == templates.len() {
            commands
                .entity(entity)
                .insert(GrassAttached)
                .remove::<GrassAttachmentProgress>();
        } else if index != progress.map_or(0, |progress| progress.0) {
            commands
                .entity(entity)
                .insert(GrassAttachmentProgress(index));
        }
    }
}

#[allow(clippy::too_many_arguments)] // Independent ECS queries/resources are scheduled by Bevy.
/// Update propagated grass uniforms and readiness after deferred batch/primitive admission.
fn update_grass_uniforms(
    mut commands: Commands,
    cameras: Query<(Entity, &GlobalTransform, Option<&GrassView>), With<StreamingCamera>>,
    mut primitives: Query<(&GlobalTransform, &mut GrassPrimitive)>,
    time: Res<Time>,
    shots: Option<Res<crate::shots::ShotsRun>>,
    controls: Option<Res<GrassCaptureControls>>,
    fade: Res<GrassFade>,
    tuning: Option<Res<crate::grass_tuning::GrassTuningState>>,
    ambient: Option<Res<GlobalAmbientLight>>,
    batches: Query<Option<&GrassAttached>, With<GrassBatch>>,
    mut metrics: ResMut<GrassRenderMetrics>,
) {
    for (camera, _, marker) in &cameras {
        if marker.is_none() {
            commands.entity(camera).insert(GrassView);
        }
    }
    let eye = cameras
        .single()
        .ok()
        .map(|(_, transform, _)| transform.translation());
    let mut all_drawn = true;
    let capture = controls.as_ref().is_some_and(|c| c.capture);
    for (transform, mut primitive) in &mut primitives {
        primitive.suppress_draw =
            capture && shots.as_ref().is_some_and(|run| run.grass_draws_off());
        primitive.uniform.world_from_cell = transform.to_matrix();
        primitive.uniform.model = primitive.model;
        if let Some(tuning) = &tuning {
            let [light, surface, color, specular] = tuning.effective.as_uniforms();
            primitive.uniform.tuning_light = light;
            primitive.uniform.tuning_surface = surface;
            primitive.uniform.tuning_color = color;
            primitive.uniform.tuning_specular = specular;
        }
        primitive.uniform.fade = Vec4::new(
            fade.start,
            fade.end,
            if capture && shots.is_some() {
                1.0
            } else {
                time.elapsed_secs_wrapped()
            },
            if fade.enabled { 0.0 } else { 1.0 },
        );
        if let Some(ambient) = &ambient {
            primitive.uniform.ambient =
                Vec4::from_array(ambient.color.to_linear().to_f32_array()) * ambient.brightness;
        }
        all_drawn &= primitive.suppress_draw
            || primitive.drawn.load(Ordering::Acquire)
            || eye.is_some_and(|eye| visible_instance_count(&primitive, eye) == 0);
    }
    metrics.ready = eye.is_some()
        && metrics.failures == 0
        && batches.iter().all(|attached| attached.is_some())
        && all_drawn;
}

#[derive(Resource)]
struct GrassPipeline {
    tuning: bool,
    shader: Handle<Shader>,
    view_layout: BindGroupLayoutDescriptor,
    batch_layout: BindGroupLayoutDescriptor,
    scene: Option<bevy::pbr::MeshPipeline>,
}

/// Load the grass shader and initialize its render pipeline resource.
fn init_pipeline(mut commands: Commands, server: Res<AssetServer>) {
    let mut pipeline =
        GrassPipeline::new(load_embedded_asset!(server.as_ref(), "shaders/grass.wgsl"));
    pipeline.tuning = true;
    commands.insert_resource(pipeline);
}

impl GrassPipeline {
    /// Create grass view and batch layouts for the supplied shader.
    fn new(shader: Handle<Shader>) -> Self {
        Self {
            tuning: true,
            shader,
            scene: None,
            view_layout: BindGroupLayoutDescriptor::new(
                "grass view",
                &BindGroupLayoutEntries::sequential(
                    ShaderStages::VERTEX_FRAGMENT,
                    (
                        uniform_buffer::<ViewUniform>(true),
                        texture_3d(TextureSampleType::Float { filterable: true }),
                        sampler(SamplerBindingType::Filtering),
                    ),
                ),
            ),
            batch_layout: BindGroupLayoutDescriptor::new(
                "grass batch",
                &BindGroupLayoutEntries::sequential(
                    ShaderStages::VERTEX_FRAGMENT,
                    (
                        uniform_buffer::<GrassUniform>(false),
                        texture_2d(TextureSampleType::Float { filterable: true }),
                        sampler(SamplerBindingType::Filtering),
                        texture_2d(TextureSampleType::Float { filterable: true }),
                        sampler(SamplerBindingType::Filtering),
                        texture_2d(TextureSampleType::Float { filterable: true }),
                        sampler(SamplerBindingType::Filtering),
                    ),
                ),
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GrassKey {
    samples: u32,
    format: TextureFormat,
    prepass: bool,
    tonemapping: Option<Tonemapping>,
    scene: Option<bevy::pbr::MeshPipelineKey>,
}

impl GrassKey {
    /// Build a specialization key from view samples, target format and display mapping.
    fn new(
        samples: u32,
        format: TextureFormat,
        prepass: bool,
        hdr: bool,
        tonemapping: Option<Tonemapping>,
    ) -> Self {
        Self {
            samples,
            format,
            prepass,
            scene: None,
            // Like Bevy's material pipeline, HDR leaves mapping to the camera
            // postprocess. Depth coverage never needs display conversion.
            tonemapping: if prepass || hdr { None } else { tonemapping },
        }
    }
}

impl SpecializedMeshPipeline for GrassPipeline {
    type Key = GrassKey;
    /// Specialize grass rendering for the mesh layout and view pipeline key.
    fn specialize(
        &self,
        key: GrassKey,
        layout: &MeshVertexBufferLayoutRef,
    ) -> Result<RenderPipelineDescriptor, SpecializedMeshPipelineError> {
        let mut attributes = vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0)];
        let mut defs = vec![];
        if self.tuning {
            defs.push("GRASS_TUNING".into());
        }
        for (attribute, location, name) in [
            (Mesh::ATTRIBUTE_NORMAL, 1, "NORMAL"),
            (Mesh::ATTRIBUTE_UV_0, 2, "UV"),
            (Mesh::ATTRIBUTE_COLOR, 3, "COLOR"),
            (Mesh::ATTRIBUTE_TANGENT, 5, "TANGENT"),
        ] {
            if layout.0.contains(attribute) {
                attributes.push(attribute.at_shader_location(location));
                defs.push(name.into());
            }
        }
        defs.push(bevy::shader::ShaderDefVal::UInt(
            "SAMPLES".into(),
            key.samples,
        ));
        if let Some(method) = key.tonemapping {
            defs.push("TONEMAP_IN_SHADER".into());
            defs.push(bevy::shader::ShaderDefVal::UInt(
                "TONEMAPPING_LUT_TEXTURE_BINDING_INDEX".into(),
                1,
            ));
            defs.push(bevy::shader::ShaderDefVal::UInt(
                "TONEMAPPING_LUT_SAMPLER_BINDING_INDEX".into(),
                2,
            ));
            defs.push(
                match method {
                    Tonemapping::None => "TONEMAP_METHOD_NONE",
                    Tonemapping::Reinhard => "TONEMAP_METHOD_REINHARD",
                    Tonemapping::ReinhardLuminance => "TONEMAP_METHOD_REINHARD_LUMINANCE",
                    Tonemapping::AcesFitted => "TONEMAP_METHOD_ACES_FITTED",
                    Tonemapping::AgX => "TONEMAP_METHOD_AGX",
                    Tonemapping::SomewhatBoringDisplayTransform => {
                        "TONEMAP_METHOD_SOMEWHAT_BORING_DISPLAY_TRANSFORM"
                    }
                    Tonemapping::TonyMcMapface => "TONEMAP_METHOD_TONY_MC_MAPFACE",
                    Tonemapping::BlenderFilmic => "TONEMAP_METHOD_BLENDER_FILMIC",
                    Tonemapping::KhronosPbrNeutral => "TONEMAP_METHOD_PBR_NEUTRAL",
                }
                .into(),
            );
        }
        // Reuse the prepared terrain view layout, including shadow textures,
        // cascades, filtering and bias. The depth pass keeps its coverage-only layout.
        let mut layouts = vec![self.view_layout.clone(), self.batch_layout.clone()];
        if let Some(scene_key) = key.scene {
            let descriptor = self
                .scene
                .as_ref()
                .expect("scene pipeline initialized")
                .specialize(scene_key, layout)?;
            layouts = descriptor.layout[..2].to_vec();
            layouts.push(self.batch_layout.clone());
            defs.retain(|def| {
                !matches!(def,
                bevy::shader::ShaderDefVal::UInt(name, _) if name.starts_with("TONEMAPPING_LUT_"))
            });
            defs.extend(descriptor.vertex.shader_defs);
            defs.push("GRASS_SCENE".into());
        }
        defs.push(bevy::shader::ShaderDefVal::UInt(
            "GRASS_GROUP".into(),
            if key.scene.is_some() { 2 } else { 1 },
        ));
        let fragment = if key.prepass { "prepass" } else { "fragment" };
        Ok(RenderPipelineDescriptor {
            label: Some("grass instances".into()),
            layout: layouts,
            vertex: VertexState {
                shader: self.shader.clone(),
                shader_defs: defs.clone(),
                entry_point: Some("vertex".into()),
                buffers: vec![
                    layout.0.get_layout(&attributes)?,
                    VertexBufferLayout {
                        array_stride: 16,
                        step_mode: VertexStepMode::Instance,
                        attributes: vec![VertexAttribute {
                            format: VertexFormat::Uint32x4,
                            offset: 0,
                            shader_location: 4,
                        }],
                    },
                ],
            },
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                shader_defs: defs,
                entry_point: Some(fragment.into()),
                targets: if key.prepass {
                    vec![]
                } else {
                    vec![Some(ColorTargetState {
                        format: key.format,
                        blend: None,
                        write_mask: ColorWrites::ALL,
                    })]
                },
            }),
            primitive: PrimitiveState {
                cull_mode: None,
                ..default()
            },
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(CompareFunction::GreaterEqual),
                stencil: default(),
                bias: default(),
            }),
            multisample: MultisampleState {
                count: key.samples,
                ..default()
            },
            ..default()
        })
    }
}

#[derive(Component)]
struct GrassGpu {
    instances: Arc<Vec<GrassInstance>>,
    buffer: Buffer,
    uniform: UniformBuffer<GrassUniform>,
    bind_group: BindGroup,
}
#[derive(Component)]
struct GrassViewBindGroup(BindGroup);

#[allow(clippy::too_many_arguments)]
/// Prepare the grass view uniform binding for the current render view.
fn prepare_view(
    mut commands: Commands,
    pipeline: Res<GrassPipeline>,
    cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    uniforms: Res<ViewUniforms>,
    views: Query<(Entity, Option<&Tonemapping>), With<GrassView>>,
    images: Res<RenderAssets<GpuImage>>,
    luts: Res<TonemappingLuts>,
    fallback: Res<bevy::render::texture::FallbackImage>,
) {
    let Some(binding) = uniforms.uniforms.binding() else {
        return;
    };
    for (entity, tonemapping) in &views {
        let (texture, sampler) = get_lut_bindings(
            &images,
            &luts,
            tonemapping.unwrap_or(&Tonemapping::None),
            &fallback,
        );
        commands
            .entity(entity)
            .insert(GrassViewBindGroup(device.create_bind_group(
                "grass view",
                &cache.get_bind_group_layout(&pipeline.view_layout),
                &BindGroupEntries::sequential((binding.clone(), texture, sampler)),
            )));
    }
}

/// Select the same first per-view sun as Bevy's prepared directional-light array.
/// Lights are fragment-only in the shared view layout, so vertex lighting receives
/// direction/color through the grass uniform instead of reading that binding.
fn prepared_grass_sun(
    suns: &Query<(Entity, &bevy::pbr::ExtractedDirectionalLight)>,
    layers: &bevy::camera::visibility::RenderLayers,
) -> (Vec4, Vec4) {
    let sun = suns
        .iter()
        .filter(|(_, light)| light.render_layers.intersects(layers))
        // Keep in sync with Bevy's prepare_lights ordering before its per-view filter.
        .min_by_key(|(entity, light)| (light.volumetric, light.shadow_maps_enabled, *entity));
    sun.map_or((Vec4::Y, Vec4::ZERO), |(_, light)| {
        (
            light.transform.back().extend(0.0),
            Vec4::from_array(light.color.to_f32_array()) * light.illuminance,
        )
    })
}

#[allow(clippy::too_many_arguments)]
/// Prepare shared instance buffers and per-primitive GPU bindings.
fn prepare_grass(
    mut commands: Commands,
    mut query: Query<(Entity, &GrassPrimitive, Option<&mut GrassGpu>)>,
    pipeline: Res<GrassPipeline>,
    cache: Res<PipelineCache>,
    images: Res<RenderAssets<GpuImage>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    fallback: Res<bevy::render::texture::FallbackImage>,
    budget: Res<GrassUploadBudget>,
    mut buffers: ResMut<GrassInstanceBuffers>,
    suns: Query<(Entity, &bevy::pbr::ExtractedDirectionalLight)>,
    views: Query<Option<&bevy::camera::visibility::RenderLayers>, With<GrassView>>,
) {
    let layers = views.single().ok().flatten().cloned().unwrap_or_default();
    let (sun_direction, sun_color) = prepared_grass_sun(&suns, &layers);
    buffers
        .0
        .retain(|_, (instances, _)| instances.strong_count() > 0);
    let limit = if budget.0 == 0 { usize::MAX } else { budget.0 };
    let mut uploaded = 0;
    for (entity, data, gpu) in &mut query {
        if let Some(mut gpu) = gpu
            && Arc::ptr_eq(&gpu.instances, &data.instances)
        {
            let mut uniform = data.uniform.clone();
            uniform.sun_direction = sun_direction;
            uniform.sun_color = sun_color;
            gpu.uniform.set(uniform);
            gpu.uniform.write_buffer(&device, &queue);
            continue;
        }
        let Some(wind) = images.get(&data.wind) else {
            continue;
        };
        let texture = if data.texture == Handle::default() {
            &fallback.d2
        } else {
            let Some(texture) = images.get(&data.texture) else {
                continue;
            };
            texture
        };
        let normal = if data.normal_texture == Handle::default() {
            &fallback.d2
        } else {
            let Some(image) = images.get(&data.normal_texture) else {
                continue;
            };
            image
        };
        let key = Arc::as_ptr(&data.instances) as usize;
        let buffer = if let Some((_, buffer)) = buffers.0.get(&key) {
            buffer.clone()
        } else {
            if uploaded >= limit {
                continue;
            }
            let buffer = device.create_buffer_with_data(&BufferInitDescriptor {
                label: Some("grass instances"),
                contents: bytemuck::cast_slice(data.instances.as_slice()),
                usage: BufferUsages::VERTEX,
            });
            buffers
                .0
                .insert(key, (Arc::downgrade(&data.instances), buffer.clone()));
            uploaded += 1;
            buffer
        };
        let mut value = data.uniform.clone();
        value.sun_direction = sun_direction;
        value.sun_color = sun_color;
        let mut uniform = UniformBuffer::from(value);
        uniform.write_buffer(&device, &queue);
        let bind_group = device.create_bind_group(
            "grass batch",
            &cache.get_bind_group_layout(&pipeline.batch_layout),
            &BindGroupEntries::sequential((
                uniform.binding().unwrap(),
                &texture.texture_view,
                &texture.sampler,
                &wind.texture_view,
                &wind.sampler,
                &normal.texture_view,
                &normal.sampler,
            )),
        );
        commands.entity(entity).insert(GrassGpu {
            instances: data.instances.clone(),
            buffer,
            uniform,
            bind_group,
        });
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)] // Independent per-view ECS inputs.
/// Queue eligible grass primitives into the main and depth render phases.
fn queue_grass(
    mut pipeline: ResMut<GrassPipeline>,
    scene: Res<bevy::pbr::MeshPipeline>,
    view_keys: Res<bevy::pbr::ViewKeyCache>,
    mut pipelines: ResMut<SpecializedMeshPipelines<GrassPipeline>>,
    cache: Res<PipelineCache>,
    meshes: Res<RenderAssets<RenderMesh>>,
    allocator: Res<MeshAllocator>,
    primitives: Query<(Entity, &MainEntity, &GrassPrimitive)>,
    views: Query<
        (
            &ExtractedView,
            &Msaa,
            Option<&ExtractedCamera>,
            Option<&Tonemapping>,
        ),
        With<GrassView>,
    >,
    main_draws: Res<DrawFunctions<AlphaMask3d>>,
    pre_draws: Res<DrawFunctions<AlphaMask3dPrepass>>,
    mut main_phases: ResMut<ViewBinnedRenderPhases<AlphaMask3d>>,
    mut pre_phases: ResMut<ViewBinnedRenderPhases<AlphaMask3dPrepass>>,
    mut queued: ResMut<GrassQueuedEntities>,
) {
    pipeline.scene = Some(scene.clone());
    clear_queued_grass(&mut queued, &mut main_phases, &mut pre_phases);
    for (view, msaa, camera, tonemapping) in &views {
        for (entity, main_entity, data) in &primitives {
            let Some(mesh) = meshes.get(&data.mesh) else {
                continue;
            };
            let Some(slabs) = allocator.mesh_slabs(&data.mesh.id()) else {
                continue;
            };
            queued
                .0
                .entry(view.retained_view_entity)
                .or_default()
                .push(*main_entity);
            for prepass in [true, false] {
                let mut key = GrassKey::new(
                    msaa.samples(),
                    view.target_format,
                    prepass,
                    camera.is_some_and(|camera| camera.hdr),
                    tonemapping.copied(),
                );
                if !prepass {
                    let Some(scene_key) = view_keys.get(&view.retained_view_entity).copied() else {
                        // Never bind a terrain view to the depth-only layout.
                        continue;
                    };
                    key.scene = Some(scene_key);
                }
                let Ok(id) = pipelines.specialize(&cache, &pipeline, key, &mesh.layout) else {
                    continue;
                };
                let batch = OpaqueNoLightmap3dBatchSetKey {
                    pipeline: id,
                    draw_function: if prepass {
                        pre_draws.read().id::<DrawGrass>()
                    } else {
                        main_draws.read().id::<DrawGrass>()
                    },
                    material_bind_group_index: None,
                    slabs,
                };
                let bin = OpaqueNoLightmap3dBinKey {
                    asset_id: data.mesh.id().into(),
                };
                if prepass {
                    if let Some(phase) = pre_phases.get_mut(&view.retained_view_entity) {
                        phase.add(
                            batch,
                            bin,
                            (entity, *main_entity),
                            default(),
                            BinnedRenderPhaseType::NonMesh,
                        );
                    }
                } else if let Some(phase) = main_phases.get_mut(&view.retained_view_entity) {
                    phase.add(
                        batch,
                        bin,
                        (entity, *main_entity),
                        default(),
                        BinnedRenderPhaseType::NonMesh,
                    );
                }
            }
        }
    }
}

/// Bevy retains NonMesh bins across frames; custom draws own their cleanup.
#[derive(Resource, Default)]
struct GrassQueuedEntities(HashMap<RetainedViewEntity, Vec<MainEntity>>);

/// Retire previous grass phase entries while preserving other draw functions.
fn clear_queued_grass(
    queued: &mut GrassQueuedEntities,
    main_phases: &mut ViewBinnedRenderPhases<AlphaMask3d>,
    pre_phases: &mut ViewBinnedRenderPhases<AlphaMask3dPrepass>,
) {
    // Remove before re-adding so unloads, missing meshes, changed pipelines and
    // views that lost GrassView cannot leave entries in their previous bins.
    for (view, entities) in queued.0.drain() {
        for entity in entities {
            if let Some(phase) = main_phases.get_mut(&view) {
                phase.remove(entity);
            }
            if let Some(phase) = pre_phases.get_mut(&view) {
                phase.remove(entity);
            }
        }
    }
}

type DrawGrass = (SetItemPipeline, DrawGrassInstances);

/// Conservative cell-distance bound; the shader applies per-instance distance thinning.
fn visible_instance_count(data: &GrassPrimitive, eye_world: Vec3) -> usize {
    if data.uniform.fade.w != 0.0 {
        return data.instances.len();
    }
    let eye = data
        .uniform
        .world_from_cell
        .inverse()
        .transform_point3(eye_world);
    let closest = Vec3::new(eye.x.clamp(0.0, 4096.0), eye.y, eye.z.clamp(-4096.0, 0.0));
    let distance = eye.distance(closest);
    let fraction = ((data.uniform.fade.y - distance)
        / (data.uniform.fade.y - data.uniform.fade.x).max(1.0))
    .clamp(0.0, 1.0);
    let limit = (fraction * data.slot_count as f32).ceil() as u32;
    data.instances
        .partition_point(|instance| instance.candidate_slot() < limit)
}

struct DrawGrassInstances;
impl<P: PhaseItem> RenderCommand<P> for DrawGrassInstances {
    type Param = (SRes<RenderAssets<RenderMesh>>, SRes<MeshAllocator>);
    type ViewQuery = (
        &'static ViewUniformOffset,
        &'static ExtractedView,
        &'static GrassViewBindGroup,
        Option<&'static bevy::pbr::MeshViewBindGroup>,
    );
    type ItemQuery = (Read<GrassPrimitive>, Read<GrassGpu>);
    /// Bind grass resources and issue an instanced draw for the prepared primitive.
    fn render<'w>(
        _: &P,
        (offset, view, view_group, scene_group): QueryItem<'w, '_, Self::ViewQuery>,
        item: Option<QueryItem<'w, '_, Self::ItemQuery>>,
        (meshes, allocator): SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some((data, gpu)) = item else {
            return RenderCommandResult::Skip;
        };
        if data.suppress_draw {
            return RenderCommandResult::Skip;
        }
        let Some(mesh) = meshes.into_inner().get(&data.mesh) else {
            return RenderCommandResult::Skip;
        };
        let allocator = allocator.into_inner();
        let Some(vertices) = allocator.mesh_vertex_slice(&data.mesh.id()) else {
            return RenderCommandResult::Skip;
        };
        let count = visible_instance_count(data, view.world_from_view.translation()) as u32;
        if count == 0 {
            return RenderCommandResult::Skip;
        }
        if std::any::TypeId::of::<P>() == std::any::TypeId::of::<AlphaMask3d>() {
            let Some(group) = scene_group else {
                return RenderCommandResult::Skip;
            };
            pass.set_bind_group(0, &group.main, &group.main_offsets);
            pass.set_bind_group(1, &group.binding_array, &[]);
            pass.set_bind_group(2, &gpu.bind_group, &[]);
        } else {
            pass.set_bind_group(0, &view_group.0, &[offset.offset]);
            pass.set_bind_group(1, &gpu.bind_group, &[]);
        }
        pass.set_vertex_buffer(0, vertices.buffer.slice(..));
        pass.set_vertex_buffer(1, gpu.buffer.slice(..));
        match &mesh.buffer_info {
            RenderMeshBufferInfo::Indexed {
                index_format,
                count: index_count,
            } => {
                let Some(indices) = allocator.mesh_index_slice(&data.mesh.id()) else {
                    return RenderCommandResult::Skip;
                };
                pass.set_index_buffer(indices.buffer.slice(..), *index_format);
                pass.draw_indexed(
                    indices.range.start..indices.range.start + index_count,
                    vertices.range.start as i32,
                    0..count,
                );
            }
            RenderMeshBufferInfo::NonIndexed => pass.draw(vertices.range, 0..count),
        }
        if std::any::TypeId::of::<P>() == std::any::TypeId::of::<AlphaMask3d>() {
            data.drawn.store(true, Ordering::Release);
        }
        RenderCommandResult::Success
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::gltf::GltfPrimitive;

    include!("grass_render_alpha_tests.rs");

    /// Build one authored material through the production primitive-template traversal.
    fn alpha_template(alpha_mode: Option<AlphaMode>, alpha: f32) -> PrimitiveTemplate {
        let mut materials = Assets::<GltfMaterial>::default();
        let material = alpha_mode.map(|alpha_mode| {
            materials.add(GltfMaterial {
                alpha_mode,
                base_color: Color::linear_rgba(0.2, 0.5, 0.7, alpha),
                ..default()
            })
        });
        let mut meshes = Assets::<GltfMesh>::default();
        let mesh = meshes.add(GltfMesh {
            index: 0,
            name: "authored alpha mode".into(),
            extras: None,
            primitives: vec![GltfPrimitive {
                index: 0,
                parent_mesh_index: 0,
                name: "alpha primitive".into(),
                mesh: Handle::default(),
                material,
                extras: None,
                material_extras: None,
            }],
        });
        let gltf = Gltf {
            scenes: vec![],
            named_scenes: default(),
            meshes: vec![mesh.clone()],
            named_meshes: default(),
            materials: vec![],
            named_materials: default(),
            nodes: vec![],
            named_nodes: default(),
            skins: vec![],
            named_skins: default(),
            default_scene: None,
            animations: vec![],
            named_animations: default(),
            source: None,
        };
        let mut build = TemplateBuild {
            pending: vec![TemplateWork::Primitive(mesh, Mat4::IDENTITY, 0)],
            ..default()
        };
        assert!(
            build_templates(
                &mut build,
                &gltf,
                &Assets::default(),
                &meshes,
                &materials,
                &mut FrameBudget::new(0),
            )
            .unwrap()
        );
        build.output.pop().unwrap()
    }

    /// Interpret the actual Naga coverage expressions with a supplied local alpha derivative.
    /// Unsupported shader operations fail the test instead of silently using a copied formula.
    fn coverage_expression(
        module: &wgpu::naga::Module,
        function: &wgpu::naga::Function,
        expression: wgpu::naga::Handle<wgpu::naga::Expression>,
        inputs: [f32; 4],
    ) -> f32 {
        use wgpu::naga::{BinaryOperator as B, Expression as E, Literal, MathFunction as M};
        let eval = |expression| coverage_expression(module, function, expression, inputs);
        match function.expressions[expression] {
            E::Literal(Literal::F32(value)) => value,
            E::Literal(Literal::U32(value)) => value as f32,
            E::Literal(Literal::I32(value)) => value as f32,
            E::Literal(Literal::Bool(value)) => u32::from(value) as f32,
            E::FunctionArgument(index) => inputs[index as usize],
            E::Load { pointer } => eval(pointer),
            E::AccessIndex { base, index } => {
                let E::AccessIndex { base, index: field } = function.expressions[base] else {
                    panic!("coverage must read a uniform vector member");
                };
                let E::GlobalVariable(global) = function.expressions[base] else {
                    panic!("coverage must read the grass uniform");
                };
                let wgpu::naga::TypeInner::Struct { ref members, .. } =
                    module.types[module.global_variables[global].ty].inner
                else {
                    panic!("grass uniform must be a structure");
                };
                assert_eq!(members[field as usize].name.as_deref(), Some("height_wind"));
                assert_eq!(index, 3);
                inputs[2]
            }
            E::Derivative {
                axis: wgpu::naga::DerivativeAxis::Width,
                expr,
                ..
            } => {
                assert!(
                    matches!(function.expressions[expr], E::FunctionArgument(0)),
                    "coverage must measure the local alpha edge"
                );
                inputs[3]
            }
            E::Binary { op, left, right } => {
                let (left, right) = (eval(left), eval(right));
                match op {
                    B::Add => left + right,
                    B::Subtract => left - right,
                    B::Multiply => left * right,
                    B::Divide => left / right,
                    B::Less => u32::from(left < right) as f32,
                    B::Greater => u32::from(left > right) as f32,
                    B::LessEqual => u32::from(left <= right) as f32,
                    B::GreaterEqual => u32::from(left >= right) as f32,
                    _ => panic!("unsupported coverage operator: {op:?}"),
                }
            }
            E::Select {
                condition,
                accept,
                reject,
            } => eval(if eval(condition) != 0.0 {
                accept
            } else {
                reject
            }),
            E::As {
                expr,
                kind: wgpu::naga::ScalarKind::Uint,
                ..
            } => eval(expr) as u32 as f32,
            E::Math {
                fun,
                arg,
                arg1,
                arg2,
                ..
            } => match fun {
                M::Max => eval(arg).max(eval(arg1.unwrap())),
                M::Clamp => eval(arg).clamp(eval(arg1.unwrap()), eval(arg2.unwrap())),
                M::Round => eval(arg).round(),
                _ => panic!("unsupported coverage math: {fun:?}"),
            },
            ref expression => panic!("unsupported coverage expression: {expression:?}"),
        }
    }

    /// Execute discard, conditionals and sample-mask returns from the parsed coverage body.
    fn coverage_block(
        module: &wgpu::naga::Module,
        function: &wgpu::naga::Function,
        block: &wgpu::naga::Block,
        inputs: [f32; 4],
    ) -> Option<bool> {
        use wgpu::naga::Statement as S;
        for statement in block.iter() {
            let result = match statement {
                S::Emit(_) => None,
                S::Block(block) => coverage_block(module, function, block, inputs),
                S::If {
                    condition,
                    accept,
                    reject,
                } => coverage_block(
                    module,
                    function,
                    if coverage_expression(module, function, *condition, inputs) != 0.0 {
                        accept
                    } else {
                        reject
                    },
                    inputs,
                ),
                S::Kill => Some(false),
                S::Return { value: Some(value) } => Some(
                    (coverage_expression(module, function, *value, inputs) as u32
                        & (1 << inputs[1] as u32))
                        != 0,
                ),
                statement => panic!("unsupported coverage statement: {statement:?}"),
            };
            if result.is_some() {
                return result;
            }
        }
        None
    }

    /// Parse and validate the real shared shader coverage function for either MSAA variant.
    fn shader_coverage_samples(alpha: f32, cutoff: f32, width: f32, samples: u32) -> usize {
        let shader = grass_lighting_tests::shading_source(include_str!("shaders/grass.wgsl"), "d");
        let uniform = shader
            .split("struct Grass {")
            .nth(1)
            .unwrap()
            .split("@group(")
            .next()
            .unwrap();
        let coverage = shader
            .split("fn coverage(")
            .nth(1)
            .unwrap()
            .split("struct Fragment")
            .next()
            .unwrap();
        let mut selected = true;
        let mut source = format!(
            "struct Grass {{{uniform}\n@group(1) @binding(0) var<uniform> grass: Grass;\nfn coverage("
        );
        for line in coverage.lines() {
            match line.trim() {
                "#if SAMPLES == 1" => selected = samples == 1,
                "#else" => selected = samples != 1,
                "#endif" => selected = true,
                _ if selected => {
                    source.push_str(line);
                    source.push('\n');
                }
                _ => {}
            }
        }
        let module = wgpu::naga::front::wgsl::parse_str(&source).unwrap();
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        let (_, function) = module
            .functions
            .iter()
            .find(|(_, function)| function.name.as_deref() == Some("coverage"))
            .unwrap();
        (0..samples)
            .filter(|sample| {
                coverage_block(
                    &module,
                    function,
                    &function.body,
                    [alpha, *sample as f32, cutoff, width],
                )
                .expect("coverage must return or discard")
            })
            .count()
    }

    #[test]
    /// Opaque material and texture/vertex alpha must not remove samples in either pass.
    fn g3_opaque_templates_ignore_alpha_coverage() {
        let template = alpha_template(Some(AlphaMode::Opaque), 0.25);
        assert_eq!(template.color.w, 0.25);
        for samples in [1, 4] {
            for alpha in [0.0, template.color.w, 1.0] {
                assert_eq!(
                    shader_coverage_samples(alpha, template.cutoff, 0.0, samples),
                    samples as usize
                );
            }
            let default_material = alpha_template(None, 1.0);
            assert_eq!(
                shader_coverage_samples(0.0, default_material.cutoff, 0.0, samples),
                samples as usize
            );
        }
    }

    #[test]
    /// Constant accepted mask plateaus retain every sample, while actual edges remain smooth.
    fn g3_mask_plateaus_and_edges_preserve_authored_cutoff() {
        for (cutoff, alpha, expected) in [
            (1.0, 1.0, 4),
            (0.0, 0.0, 4),
            (0.5, 0.5, 4),
            (0.5, 0.25, 0),
            (0.5, 0.75, 4),
        ] {
            let template = alpha_template(Some(AlphaMode::Mask(cutoff)), alpha);
            assert_eq!(template.cutoff, cutoff);
            assert_eq!(
                shader_coverage_samples(alpha, template.cutoff, 0.0, 4),
                expected
            );
        }
        for (alpha, expected) in [(0.25, 0), (0.4375, 1), (0.5, 2), (0.5625, 3), (0.75, 4)] {
            assert_eq!(shader_coverage_samples(alpha, 0.5, 0.25, 4), expected);
        }
        assert_eq!(shader_coverage_samples(1.0, 1.0, 0.0, 1), 1);
        assert_eq!(shader_coverage_samples(0.25, 0.5, 0.0, 1), 0);
    }

    #[test]
    /// A final deferred batch admission must invalidate the previous frame's ready result.
    fn g3_deferred_streaming_admission_precedes_render_readiness() {
        /// Admit a translated batch through the same deferred system set as the worker poll.
        fn admit_batch(mut commands: Commands, mut admitted: Local<bool>) {
            if !*admitted {
                let root = commands.spawn(Transform::from_xyz(17.0, 23.0, -31.0)).id();
                commands.spawn((batch(), Transform::default(), ChildOf(root)));
                *admitted = true;
            }
        }
        for loaded in [false, true] {
            let mut app = App::new();
            app.add_plugins((
                MinimalPlugins,
                bevy::asset::AssetPlugin::default(),
                TransformPlugin,
            ))
            .init_asset::<Image>()
            .init_asset::<Shader>()
            .init_asset::<Gltf>()
            .init_asset::<GltfNode>()
            .init_asset::<GltfMesh>()
            .init_asset::<GltfMaterial>()
            .insert_resource(crate::config::EngineConfig::default())
            .insert_resource(GrassFade {
                enabled: true,
                start: 1000.0,
                end: 2000.0,
            })
            .add_plugins(GrassRenderPlugin)
            .add_systems(PostUpdate, admit_batch.in_set(GrassStreaming));
            if loaded {
                app.world_mut().resource_mut::<GrassModels>().0.insert(
                    batch().model_path,
                    GrassModel {
                        handle: Handle::default(),
                        templates: Some(vec![alpha_template(Some(AlphaMode::Mask(0.375)), 1.0)]),
                        build: TemplateBuild::default(),
                        failed: false,
                    },
                );
            }
            app.world_mut().resource_mut::<GrassRenderMetrics>().ready = true;
            app.world_mut()
                .spawn((StreamingCamera, Transform::default()));
            app.update();
            assert_eq!(
                app.world_mut()
                    .query::<&GrassBatch>()
                    .iter(app.world())
                    .count(),
                1
            );
            assert!(
                !app.world().resource::<GrassRenderMetrics>().ready,
                "a newly admitted batch cannot inherit an earlier ready proof"
            );
            assert!(
                app.world()
                    .resource::<GrassModels>()
                    .0
                    .contains_key(&batch().model_path),
                "loading must observe this frame's admitted model"
            );
            if loaded {
                let mut primitives = app
                    .world_mut()
                    .query::<(&GlobalTransform, &GrassPrimitive)>();
                let (transform, primitive) = primitives.single(app.world()).unwrap();
                let expected = Vec3::new(17.0, 23.0, -31.0);
                assert_eq!(
                    transform.translation(),
                    expected,
                    "new primitives must propagate their parent transform before extraction"
                );
                assert_eq!(
                    primitive
                        .uniform
                        .world_from_cell
                        .transform_point3(Vec3::ZERO),
                    expected
                );
                assert_eq!(primitive.uniform.height_wind.w, 0.375);
            }
        }
    }

    #[test]
    fn authored_tangent_shader_inputs_match_specialized_vertex_layout() {
        let mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0_f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0_f32; 2]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0_f32; 4]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[1.0, 0.0, 0.0, 1.0]; 3]);
        let layout =
            mesh.get_mesh_vertex_buffer_layout(&mut bevy::mesh::MeshVertexBufferLayouts::default());
        let descriptor = GrassPipeline::new(Handle::default())
            .specialize(
                GrassKey::new(4, TextureFormat::Rgba16Float, false, true, None),
                &layout,
            )
            .unwrap();
        let mut defs = crate::grass_shader_tests::definitions(false, 4, 15, false);
        for def in &descriptor.vertex.shader_defs {
            match def {
                bevy::shader::ShaderDefVal::Bool(name, value) => {
                    defs.insert(
                        name.clone(),
                        naga_oil::compose::ShaderDefValue::Bool(*value),
                    );
                }
                bevy::shader::ShaderDefVal::UInt(name, value) => {
                    defs.insert(
                        name.clone(),
                        naga_oil::compose::ShaderDefValue::UInt(*value),
                    );
                }
                _ => {}
            }
        }
        let module =
            crate::grass_shader_tests::compose(include_str!("shaders/grass.wgsl"), defs).unwrap();
        let vertex = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("Vertex"))
            .unwrap()
            .1;
        let wgpu::naga::TypeInner::Struct { members, .. } = &vertex.inner else {
            panic!("vertex struct");
        };
        let required: std::collections::BTreeSet<_> = members
            .iter()
            .filter_map(|m| match m.binding {
                Some(wgpu::naga::Binding::Location { location, .. }) => Some(location),
                _ => None,
            })
            .collect();
        let supplied: std::collections::BTreeSet<_> = descriptor
            .vertex
            .buffers
            .iter()
            .flat_map(|b| b.attributes.iter().map(|a| a.shader_location))
            .collect();
        assert_eq!(required, supplied);
    }

    #[test]
    /// Verify display mapping specialization matches camera and skips hdr and depth.
    fn display_mapping_specialization_matches_camera_and_skips_hdr_and_depth() {
        use bevy::{mesh::MeshVertexBufferLayouts, shader::ShaderDefVal};

        let pipeline = GrassPipeline::new(Handle::default());
        let mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3]);
        let layout = mesh.get_mesh_vertex_buffer_layout(&mut MeshVertexBufferLayouts::default());
        let specialize = |hdr, prepass, method| {
            pipeline
                .specialize(
                    GrassKey::new(
                        4,
                        if hdr {
                            TextureFormat::Rgba16Float
                        } else {
                            TextureFormat::Bgra8UnormSrgb
                        },
                        prepass,
                        hdr,
                        Some(method),
                    ),
                    &layout,
                )
                .unwrap()
        };
        for (method, definition) in [
            (Tonemapping::TonyMcMapface, "TONEMAP_METHOD_TONY_MC_MAPFACE"),
            (Tonemapping::Reinhard, "TONEMAP_METHOD_REINHARD"),
            (Tonemapping::None, "TONEMAP_METHOD_NONE"),
        ] {
            let ldr = specialize(false, false, method);
            let fragment = ldr.fragment.as_ref().unwrap();
            assert!(fragment.shader_defs.contains(&"TONEMAP_IN_SHADER".into()));
            assert!(fragment.shader_defs.contains(&definition.into()));
            assert!(fragment.shader_defs.contains(&ShaderDefVal::UInt(
                "TONEMAPPING_LUT_TEXTURE_BINDING_INDEX".into(),
                1,
            )));
            assert!(fragment.shader_defs.contains(&ShaderDefVal::UInt(
                "TONEMAPPING_LUT_SAMPLER_BINDING_INDEX".into(),
                2,
            )));
            assert_eq!(ldr.layout.len(), 2);
            let view_bindings = &ldr.layout[0].entries;
            assert_eq!(view_bindings.len(), 3);
            assert!(matches!(
                view_bindings[0].ty,
                BindingType::Buffer {
                    has_dynamic_offset: true,
                    ..
                }
            ));
            assert_eq!(view_bindings[1].binding, 1);
            assert!(matches!(
                view_bindings[1].ty,
                BindingType::Texture {
                    view_dimension: TextureViewDimension::D3,
                    ..
                }
            ));
            assert_eq!(view_bindings[2].binding, 2);
            assert_eq!(
                view_bindings[2].ty,
                BindingType::Sampler(SamplerBindingType::Filtering)
            );
            assert_eq!(ldr.multisample.count, 4);
            assert_eq!(fragment.entry_point.as_deref(), Some("fragment"));

            for (hdr, prepass) in [(true, false), (false, true), (true, true)] {
                let descriptor = specialize(hdr, prepass, method);
                let fragment = descriptor.fragment.as_ref().unwrap();
                assert!(!fragment.shader_defs.contains(&"TONEMAP_IN_SHADER".into()));
                assert!(!fragment.shader_defs.contains(&definition.into()));
                assert_eq!(descriptor.multisample.count, 4);
                assert_eq!(descriptor.layout, ldr.layout);
                if prepass {
                    assert!(fragment.targets.is_empty());
                    assert_eq!(fragment.entry_point.as_deref(), Some("prepass"));
                } else {
                    assert_eq!(
                        fragment.targets[0].as_ref().unwrap().format,
                        TextureFormat::Rgba16Float
                    );
                }
            }
        }
    }

    /// Build an extracted sun with independently chosen ordering, layers and radiance.
    fn extracted_sun(
        shadows: bool,
        layer: usize,
        color: LinearRgba,
        illuminance: f32,
    ) -> bevy::pbr::ExtractedDirectionalLight {
        bevy::pbr::ExtractedDirectionalLight {
            color,
            illuminance,
            transform: GlobalTransform::IDENTITY,
            shadow_maps_enabled: shadows,
            contact_shadows_enabled: false,
            volumetric: false,
            affects_lightmapped_mesh_diffuse: true,
            shadow_depth_bias: 0.02,
            shadow_normal_bias: 0.3,
            cascade_shadow_config: default(),
            cascades: default(),
            frusta: default(),
            render_layers: bevy::camera::visibility::RenderLayers::layer(layer),
            soft_shadow_size: None,
            occlusion_culling: false,
            sun_disk_angular_size: 0.0,
            sun_disk_intensity: 0.0,
        }
    }

    #[test]
    /// Match prepared-light ordering, stable entity ties, view layers and the no-sun fallback.
    fn g3_vertex_sun_matches_prepared_shadow_light() {
        use bevy::{camera::visibility::RenderLayers, ecs::system::SystemState};
        let mut world = World::new();
        world.spawn(extracted_sun(true, 0, LinearRgba::RED, 11.0));
        world.spawn(extracted_sun(false, 1, LinearRgba::GREEN, 13.0));
        let first = world.spawn_empty().id();
        let second = world.spawn_empty().id();
        // Entity ordering is opaque and need not follow allocation order.
        let (lower, higher) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        world.entity_mut(lower).insert(extracted_sun(
            false,
            0,
            LinearRgba::new(0.25, 0.5, 1.0, 1.0),
            20.0,
        ));
        world
            .entity_mut(higher)
            .insert(extracted_sun(false, 0, LinearRgba::RED, 17.0));
        let mut state: SystemState<Query<(Entity, &bevy::pbr::ExtractedDirectionalLight)>> =
            SystemState::new(&mut world);
        let suns = state.get(&world).unwrap();
        let (direction, color) = prepared_grass_sun(&suns, &RenderLayers::default());
        assert_eq!(direction, Vec4::Z);
        assert_eq!(color, Vec4::new(5.0, 10.0, 20.0, 20.0));
        assert_eq!(
            prepared_grass_sun(&suns, &RenderLayers::layer(1)).1,
            Vec4::new(0.0, 13.0, 0.0, 13.0)
        );
        assert_eq!(
            prepared_grass_sun(&suns, &RenderLayers::layer(9)),
            (Vec4::Y, Vec4::ZERO)
        );
    }

    #[test]
    /// Verify retained non mesh bins retire grass without removing other draws.
    fn retained_non_mesh_bins_retire_grass_without_removing_other_draws() {
        use bevy::render::{
            batching::gpu_preprocessing::GpuPreprocessingMode, render_phase::DrawFunctionId,
        };

        let mut world = World::new();
        let view = RetainedViewEntity::new(world.spawn_empty().id().into(), None, 0);
        let live = world.spawn_empty().id();
        let retired = world.spawn_empty().id();
        let unrelated = world.spawn_empty().id();
        let retired_main: MainEntity = retired.into();
        let unrelated_main: MainEntity = unrelated.into();
        let mut main = ViewBinnedRenderPhases::<AlphaMask3d>::default();
        let mut pre = ViewBinnedRenderPhases::<AlphaMask3dPrepass>::default();
        let mut queued = GrassQueuedEntities::default();
        main.prepare_for_new_frame(view, GpuPreprocessingMode::None);
        pre.prepare_for_new_frame(view, GpuPreprocessingMode::None);
        let batch = |pipeline| OpaqueNoLightmap3dBatchSetKey {
            pipeline: CachedRenderPipelineId::new(pipeline),
            draw_function: DrawFunctionId(0),
            material_bind_group_index: None,
            slabs: default(),
        };
        let bin = OpaqueNoLightmap3dBinKey {
            asset_id: Handle::<Mesh>::default().id().into(),
        };
        macro_rules! add_to_both {
            ($entity:expr, $pipeline:expr) => {
                main.get_mut(&view).unwrap().add(
                    batch($pipeline),
                    bin.clone(),
                    ($entity, $entity.into()),
                    default(),
                    BinnedRenderPhaseType::NonMesh,
                );
                pre.get_mut(&view).unwrap().add(
                    batch($pipeline),
                    bin.clone(),
                    ($entity, $entity.into()),
                    default(),
                    BinnedRenderPhaseType::NonMesh,
                );
            };
        }
        add_to_both!(live, 1);
        add_to_both!(retired, 1);
        add_to_both!(unrelated, 9);
        queued.0.insert(view, vec![live.into(), retired.into()]);
        world.despawn(retired);
        main.prepare_for_new_frame(view, GpuPreprocessingMode::None);
        pre.prepare_for_new_frame(view, GpuPreprocessingMode::None);
        // Bevy's frame preparation alone deliberately retains these entries.
        assert_eq!(main.get(&view).unwrap().non_mesh_items.len(), 2);
        clear_queued_grass(&mut queued, &mut main, &mut pre);
        add_to_both!(live, 2);
        queued.0.insert(view, vec![live.into()]);
        for bins in [
            &main.get(&view).unwrap().non_mesh_items,
            &pre.get(&view).unwrap().non_mesh_items,
        ] {
            assert_eq!(bins.len(), 2);
            assert!(
                bins.keys()
                    .all(|(key, _)| key.pipeline != batch(1).pipeline)
            );
            assert!(
                bins.values()
                    .all(|bin| !bin.entities.contains_key(&retired_main))
            );
        }
        // Even when no GrassView remains to be visited, its old draws retire.
        clear_queued_grass(&mut queued, &mut main, &mut pre);
        assert!(queued.0.is_empty());
        for bins in [
            &main.get(&view).unwrap().non_mesh_items,
            &pre.get(&view).unwrap().non_mesh_items,
        ] {
            assert_eq!(bins.len(), 1);
            assert!(
                bins.values()
                    .next()
                    .unwrap()
                    .entities
                    .contains_key(&unrelated_main)
            );
        }
    }

    /// Build an immutable synthetic instance batch for render-state tests.
    fn batch() -> GrassBatch {
        GrassBatch {
            cell_id: 1,
            grid: [0, 0],
            grass_id: 7,
            model_path: "meshes/grass.glb".into(),
            instances: Arc::new(vec![GrassInstance {
                position: [0; 3],
                rotation: 0,
                scale: 128,
                color: 128,
                normal: [0, 0, 127],
                slot: 0,
            }]),
            height_min: 0.0,
            height_span: 1.0,
            slot_count: 4,
            wave_period: 1.0,
            flags: 0,
        }
    }

    /// Build a synthetic primitive with shared instance data and draw proof.
    fn primitive() -> GrassPrimitive {
        GrassPrimitive {
            suppress_draw: false,
            instances: batch().instances,
            mesh: Handle::default(),
            texture: Handle::default(),
            normal_texture: Handle::default(),
            wind: Handle::default(),
            model: Mat4::IDENTITY,
            slot_count: 4,
            drawn: Arc::new(AtomicBool::new(false)),
            uniform: GrassUniform {
                world_from_cell: Mat4::IDENTITY,
                model: Mat4::IDENTITY,
                model_normal: Mat4::IDENTITY,
                color: Vec4::ONE,
                height_wind: Vec4::new(0.0, 1.0, 1.0, 0.5),
                fade: Vec4::new(1000.0, 2000.0, 0.0, 0.0),
                absolute_grid: Vec4::new(0.0, 0.0, 0.0, 4.0),
                sun_direction: Vec4::Y,
                sun_color: Vec4::ONE,
                ambient: Vec4::ONE,
                uv_transform: Mat3::IDENTITY,
                normal_params: Vec4::ZERO,
                tuning_light: crate::grass_tuning::GrassTuning::default().as_uniforms()[0],
                tuning_surface: crate::grass_tuning::GrassTuning::default().as_uniforms()[1],
                tuning_color: crate::grass_tuning::GrassTuning::default().as_uniforms()[2],
                tuning_specular: crate::grass_tuning::GrassTuning::default().as_uniforms()[3],
            },
        }
    }

    #[test]
    /// Metadata must not disturb candidate ordering or CPU fade readiness.
    fn grass_tuning_metadata_fade_uses_candidate_identity() {
        let mut data = primitive();
        data.instances = Arc::new(
            [0, 1, 3]
                .map(|slot| GrassInstance {
                    slot: slot | (127 << 18) | (91 << 25),
                    ..batch().instances[0]
                })
                .to_vec(),
        );
        assert_eq!(
            visible_instance_count(&data, Vec3::new(-1500.0, 0.0, 0.0)),
            2
        );
        assert_eq!(
            visible_instance_count(&data, Vec3::new(-10000.0, 0.0, 0.0)),
            0
        );
    }

    #[test]
    /// Verify disabling fade preserves every slot and requires far primitive draw proof.
    fn disabling_fade_preserves_every_slot_and_requires_far_primitive_draw_proof() {
        let mut data = primitive();
        data.instances = Arc::new(
            [0, 1, 3]
                .map(|slot| GrassInstance {
                    slot,
                    ..batch().instances[0]
                })
                .to_vec(),
        );
        let halfway = Vec3::new(-1500.0, 0.0, 0.0);
        let beyond_end = Vec3::new(-10000.0, 0.0, 0.0);
        assert_eq!(visible_instance_count(&data, halfway), 2);
        assert_eq!(visible_instance_count(&data, beyond_end), 0);
        data.uniform.fade.w = 1.0;
        assert_eq!(visible_instance_count(&data, halfway), 3);
        assert_eq!(visible_instance_count(&data, beyond_end), 3);

        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(GrassFade {
                enabled: true,
                start: 1000.0,
                end: 2000.0,
            })
            .init_resource::<GrassRenderMetrics>()
            .add_systems(Update, update_grass_uniforms);
        app.world_mut().spawn((
            StreamingCamera,
            GlobalTransform::from_translation(beyond_end),
        ));
        app.world_mut().spawn((batch(), GrassAttached));
        let proof = data.drawn.clone();
        let primitive = app
            .world_mut()
            .spawn((data, GlobalTransform::IDENTITY))
            .id();
        app.update();
        assert!(app.world().resource::<GrassRenderMetrics>().ready);
        assert_eq!(
            app.world()
                .get::<GrassPrimitive>(primitive)
                .unwrap()
                .uniform
                .fade
                .w,
            0.0
        );
        app.world_mut().resource_mut::<GrassFade>().enabled = false;
        app.update();
        assert_eq!(
            app.world()
                .get::<GrassPrimitive>(primitive)
                .unwrap()
                .uniform
                .fade
                .w,
            1.0
        );
        assert!(!app.world().resource::<GrassRenderMetrics>().ready);
        proof.store(true, Ordering::Release);
        app.update();
        assert!(app.world().resource::<GrassRenderMetrics>().ready);
    }

    #[test]
    /// Verify bounded traversal and attachment preserve authored node and texture transforms.
    fn template_traversal_resumes_within_budget_and_preserves_authored_transforms() {
        let mut nodes = Assets::<GltfNode>::default();
        let mut meshes = Assets::<GltfMesh>::default();
        let mut materials = Assets::<GltfMaterial>::default();
        let material = materials.add(GltfMaterial {
            alpha_mode: AlphaMode::Mask(0.375),
            base_color: Color::linear_rgb(0.2, 0.5, 0.7),
            uv_transform: bevy::math::Affine2::from_scale_angle_translation(
                Vec2::new(2.0, -3.0),
                0.0,
                Vec2::new(0.25, -0.5),
            ),
            ..default()
        });
        let mesh = meshes.add(GltfMesh {
            index: 0,
            name: "five primitives".into(),
            extras: None,
            primitives: (0..5)
                .map(|index| GltfPrimitive {
                    index,
                    parent_mesh_index: 0,
                    name: format!("primitive{index}"),
                    mesh: Handle::default(),
                    material: Some(material.clone()),
                    extras: None,
                    material_extras: None,
                })
                .collect(),
        });
        let child_pose = Transform::from_translation(Vec3::new(5.0, 6.0, 7.0));
        let child = nodes.add(GltfNode {
            index: 1,
            name: "child".into(),
            children: vec![],
            mesh: Some(mesh.clone()),
            skin: None,
            transform: child_pose,
            is_animation_root: false,
            extras: None,
        });
        let root_pose = Transform::from_translation(Vec3::new(7.0, 9.0, 11.0))
            .with_scale(Vec3::new(2.0, 3.0, 4.0));
        let root = nodes.add(GltfNode {
            index: 0,
            name: "root".into(),
            children: vec![child.clone()],
            mesh: None,
            skin: None,
            transform: root_pose,
            is_animation_root: false,
            extras: None,
        });
        let gltf = Gltf {
            scenes: vec![],
            named_scenes: default(),
            meshes: vec![mesh],
            named_meshes: default(),
            materials: vec![material],
            named_materials: default(),
            nodes: vec![root, child],
            named_nodes: default(),
            skins: vec![],
            named_skins: default(),
            default_scene: None,
            animations: vec![],
            named_animations: default(),
            source: None,
        };
        let mut build = TemplateBuild::default();
        let mut frames = 0;
        loop {
            let previous = build.output.len();
            let mut budget = FrameBudget::new(2);
            let complete =
                build_templates(&mut build, &gltf, &nodes, &meshes, &materials, &mut budget)
                    .unwrap();
            assert!(
                build.output.len() - previous <= 2,
                "one complex mesh cannot bypass primitive budget"
            );
            frames += 1;
            assert!(
                frames < 32,
                "bounded resumable traversal must eventually complete"
            );
            if complete {
                break;
            }
        }
        assert!(frames > 2);
        assert_eq!(build.output.len(), 5);
        for template in &build.output {
            assert_eq!(
                template.model,
                root_pose.to_matrix() * child_pose.to_matrix()
            );
            assert_eq!(
                template.model.transform_point3(Vec3::ZERO),
                Vec3::new(17.0, 27.0, 39.0)
            );
            let actual = template
                .model_normal
                .transform_vector3(Vec3::ONE)
                .normalize();
            assert!(actual.distance(Vec3::new(0.5, 1.0 / 3.0, 0.25).normalize()) < 1.0e-6);
            assert_eq!(template.cutoff, 0.375);
            assert_eq!(template.color, Vec4::new(0.2, 0.5, 0.7, 1.0));
        }

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .insert_resource(Assets::<Gltf>::default())
            .insert_resource(nodes)
            .insert_resource(meshes)
            .insert_resource(materials)
            .insert_resource(WindTexture(Handle::default()))
            .insert_resource(crate::config::EngineConfig {
                grass_uploads_per_frame: 0,
                ..default()
            })
            .init_resource::<GrassRenderMetrics>()
            .insert_resource(GrassModels(HashMap::from([(
                batch().model_path,
                GrassModel {
                    handle: Handle::default(),
                    templates: Some(build.output),
                    build: TemplateBuild::default(),
                    failed: false,
                },
            )])))
            .add_systems(Update, attach_grass_primitives);
        app.world_mut().spawn(batch());
        app.update();
        let mut primitives = app.world_mut().query::<&GrassPrimitive>();
        assert_eq!(primitives.iter(app.world()).count(), 5);
        for primitive in primitives.iter(app.world()) {
            let mut encoded = encase::UniformBuffer::new(Vec::new());
            encoded.write(&primitive.uniform).unwrap();
            let bytes = encoded.into_inner();
            assert_eq!(
                bytes.len(),
                432,
                "the attached GPU uniform must retain the authored texture transform"
            );
            // WGSL mat3 columns each occupy sixteen bytes in uniform storage.
            let column = |offset: usize| {
                Vec3::from_array(std::array::from_fn(|axis| {
                    let start = offset + axis * 4;
                    f32::from_le_bytes(bytes[start..start + 4].try_into().unwrap())
                }))
            };
            let transform = Mat3::from_cols(column(304), column(320), column(336));
            for (uv, expected) in [
                (Vec2::new(0.2, 0.6), Vec2::new(0.65, -2.3)),
                // The shader's zero UV default must also retain the authored offset.
                (Vec2::ZERO, Vec2::new(0.25, -0.5)),
            ] {
                assert!((transform * uv.extend(1.0)).truncate().distance(expected) < 1.0e-6);
            }
        }

        // Parse the actual shared shader declaration, rather than a second copy
        // of its fields, to check the CPU upload's member offset and total span.
        let shader =
            grass_lighting_tests::shading_source(include_str!("shaders/grass.wgsl"), "tuning");
        let start = shader.find("struct Grass {").unwrap();
        let end = shader[start..].find("@group(").unwrap() + start;
        let module = wgpu::naga::front::wgsl::parse_str(&shader[start..end]).unwrap();
        let (_, uniform) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("Grass"))
            .unwrap();
        let wgpu::naga::TypeInner::Struct { members, span } = &uniform.inner else {
            panic!("grass shader uniform must be a structure");
        };
        assert_eq!(*span, GrassUniform::min_size().get() as u32);
        let baseline =
            grass_lighting_tests::shading_source(include_str!("shaders/grass.wgsl"), "d");
        let baseline_start = baseline.find("struct Grass {").unwrap();
        let baseline_end = baseline[baseline_start..].find("@group(").unwrap() + baseline_start;
        let baseline_module =
            wgpu::naga::front::wgsl::parse_str(&baseline[baseline_start..baseline_end]).unwrap();
        let baseline_uniform = baseline_module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("Grass"))
            .unwrap()
            .1;
        let wgpu::naga::TypeInner::Struct {
            span: baseline_span,
            ..
        } = &baseline_uniform.inner
        else {
            panic!("baseline grass structure");
        };
        assert_eq!(*baseline_span, 368);
        assert_eq!(*baseline_span + 64, *span);

        let transform = members
            .iter()
            .find(|member| member.name.as_deref() == Some("uv_transform"))
            .expect("both shader passes need the uploaded texture transform");
        assert_eq!(transform.offset, 304);
    }

    #[test]
    /// Verify render readiness requires attachment and draw proof for every in range primitive.
    fn render_readiness_requires_attachment_and_draw_proof_for_every_in_range_primitive() {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(GrassFade {
                enabled: true,
                start: 1000.0,
                end: 2000.0,
            })
            .init_resource::<GrassRenderMetrics>()
            .add_systems(Update, update_grass_uniforms);
        app.world_mut()
            .spawn((StreamingCamera, GlobalTransform::IDENTITY));
        let batch = app.world_mut().spawn(batch()).id();
        let data = primitive();
        let proof = data.drawn.clone();
        app.world_mut().spawn((data, GlobalTransform::IDENTITY));
        app.update();
        assert!(!app.world().resource::<GrassRenderMetrics>().ready);
        app.world_mut().entity_mut(batch).insert(GrassAttached);
        app.update();
        assert!(
            !app.world().resource::<GrassRenderMetrics>().ready,
            "attachment/upload cannot prove a draw"
        );
        proof.store(true, Ordering::Release);
        app.update();
        assert!(app.world().resource::<GrassRenderMetrics>().ready);
        let unseen = app
            .world_mut()
            .spawn((primitive(), GlobalTransform::IDENTITY))
            .id();
        app.update();
        assert!(
            !app.world().resource::<GrassRenderMetrics>().ready,
            "one earlier primitive cannot satisfy the next primitive"
        );
        app.world_mut()
            .entity_mut(unseen)
            .insert(GlobalTransform::from_translation(Vec3::new(
                10000.0, 0.0, 0.0,
            )));
        app.update();
        assert!(
            app.world().resource::<GrassRenderMetrics>().ready,
            "fully faded primitives require no draw"
        );
        app.world_mut()
            .resource_mut::<GrassRenderMetrics>()
            .failures = 1;
        app.update();
        assert!(!app.world().resource::<GrassRenderMetrics>().ready);
    }

    #[test]
    /// Verify shared draw proof survives extraction and model failure is counted once.
    fn shared_draw_proof_survives_extraction_and_model_failure_is_counted_once() {
        let data = primitive();
        let extracted = data.clone();
        extracted.drawn.store(true, Ordering::Release);
        assert!(data.drawn.load(Ordering::Acquire));
        let mut model = GrassModel {
            handle: Handle::default(),
            templates: None,
            build: TemplateBuild::default(),
            failed: false,
        };
        let mut metrics = GrassRenderMetrics::default();
        fail_model(&mut model, "meshes/missing.glb", "missing", &mut metrics);
        fail_model(
            &mut model,
            "meshes/missing.glb",
            "missing again",
            &mut metrics,
        );
        assert_eq!(metrics.failures, 1);
    }
}

#[cfg(test)]
#[path = "grass_lighting_tests.rs"]
mod grass_lighting_tests;

#[cfg(test)]
#[path = "grass_shader_tests.rs"]
mod grass_shader_tests;
