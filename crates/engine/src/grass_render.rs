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
        AssetId, LoadState, RecursiveDependencyLoadState, embedded_asset, load_embedded_asset,
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
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

pub struct GrassRenderPlugin;

/// Deterministic ON/OFF controls for explicit shot capture runs.
#[derive(Resource, Clone, Copy, Debug)]
pub(crate) struct GrassCaptureControls {
    pub(crate) capture: bool,
}

/// Capture readiness requires current-view draw proof for visible admitted primitives.
#[derive(Resource, Default, Debug)]
pub struct GrassRenderMetrics {
    pub ready: bool,
    /// Distinct failed models required by the current resident grass batches.
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
            .init_resource::<GrassLightUnits>()
            .add_plugins(ExtractResourcePlugin::<GrassUploadBudget>::default())
            .add_plugins(ExtractResourcePlugin::<GrassLightUnits>::default())
            .add_plugins((
                ExtractComponentPlugin::<GrassPrimitive>::default(),
                ExtractComponentPlugin::<GrassView>::default(),
            ))
            .add_systems(
                PostUpdate,
                (load_grass_models, attach_grass_primitives)
                    .chain()
                    .after(GrassStreaming)
                    .before(bevy::transform::TransformSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                update_grass_uniforms
                    .after(bevy::transform::TransformSystems::Propagate)
                    .after(bevy::camera::CameraUpdateSystems),
            );
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .insert_resource(controls)
            .add_render_command::<AlphaMask3d, DrawGrass>()
            .init_resource::<GrassInstanceBuffers>()
            .init_resource::<GrassQueuedEntities>()
            .init_resource::<GrassPrepassDraws>()
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
    /// Maximum displaced blade reach in cell coordinates, including scale and wind.
    blade_reach: f32,
    mesh: Handle<Mesh>,
    texture: Handle<Image>,
    normal_texture: Handle<Image>,
    uniform: GrassUniform,
    model: Mat4,
    slot_count: u32,
    /// Shared proof of an issued main draw for this camera and pipeline.
    drawn: GrassViewEvidence,
    /// Frustum culling satisfies only the current camera view.
    culled: GrassViewEvidence,
    /// Main-world camera settings governing shared render evidence.
    proof_view: Option<GrassProofView>,
    /// Last queued main specialization for the current view.
    proof_pipeline: Arc<Mutex<Option<GrassKey>>>,
}

/// Shared evidence records the view generation captured by each render-world copy.
#[derive(Clone)]
struct GrassViewEvidence {
    satisfied: Arc<AtomicU64>,
    generation: u64,
}

impl Default for GrassViewEvidence {
    /// Start with a fresh view awaiting evidence.
    fn default() -> Self {
        Self {
            satisfied: Arc::new(AtomicU64::new(0)),
            generation: 1,
        }
    }
}

impl GrassViewEvidence {
    /// Read satisfaction for the view captured by this copy.
    fn load(&self, ordering: Ordering) -> bool {
        self.satisfied.load(ordering) == self.generation
    }

    /// Record satisfaction for the view captured by this copy.
    fn store(&self, value: bool, ordering: Ordering) {
        self.satisfied
            .store(if value { self.generation } else { 0 }, ordering);
    }

    /// Advance the current view while older extracted copies retain their generation.
    fn next_view(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            *self = Self::default();
        }
    }
}

/// Camera and pipeline settings identifying the view awaiting draw evidence.
#[derive(Clone, Copy, PartialEq)]
struct GrassProofView {
    camera: Entity,
    world_from_view: Mat4,
    clip_from_view: Mat4,
    samples: u32,
    tonemapping: Option<Tonemapping>,
    hdr: bool,
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

/// Load requested grass species and release handles after their last batch leaves.
fn load_grass_models(
    batches: Query<&GrassBatch>,
    server: Res<AssetServer>,
    mut models: ResMut<GrassModels>,
    mut metrics: ResMut<GrassRenderMetrics>,
) {
    let needed: HashSet<_> = batches
        .iter()
        .map(|batch| batch.model_path.as_str())
        .collect();
    models.0.retain(|path, model| {
        let keep = needed.contains(path.as_str());
        if !keep && model.failed {
            metrics.failures = metrics.failures.saturating_sub(1);
        }
        keep
    });
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
        let Some(work) = build.pending.pop() else {
            break;
        };
        match work {
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
    vertex_meshes: Option<Res<Assets<Mesh>>>,
    config: Res<crate::config::EngineConfig>,
    server: Res<AssetServer>,
    mut metrics: ResMut<GrassRenderMetrics>,
) {
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
                blade_reach: vertex_meshes
                    .as_ref()
                    .and_then(|meshes| meshes.get(&template.mesh))
                    .and_then(|mesh| mesh.attribute(Mesh::ATTRIBUTE_POSITION))
                    .and_then(|values| values.as_float3())
                    .map_or(f32::INFINITY, |positions| {
                        positions
                            .iter()
                            .map(|position| {
                                template
                                    .model
                                    .transform_point3(Vec3::from_array(*position))
                                    .length()
                                    * 2.3
                            })
                            .fold(0.0_f32, f32::max)
                    }),
                mesh: template.mesh.clone(),
                texture: template.texture.clone(),
                normal_texture: template.normal_texture.clone(),
                model: template.model,
                slot_count: batch.slot_count,
                drawn: GrassViewEvidence::default(),
                culled: GrassViewEvidence::default(),
                proof_view: None,
                proof_pipeline: Arc::new(Mutex::new(None)),
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

#[allow(clippy::too_many_arguments, clippy::type_complexity)] // Independent per-view ECS inputs.
/// Update propagated grass uniforms and readiness after deferred batch/primitive admission.
fn update_grass_uniforms(
    mut commands: Commands,
    cameras: Query<
        (
            Entity,
            &GlobalTransform,
            Option<&Camera>,
            Option<&Msaa>,
            Option<&Tonemapping>,
            Has<bevy::camera::Hdr>,
            Option<&GrassView>,
        ),
        With<StreamingCamera>,
    >,
    retired_views: Query<Entity, (With<GrassView>, Without<StreamingCamera>)>,
    mut primitives: Query<(&GlobalTransform, &mut GrassPrimitive)>,
    shots: Option<Res<crate::shots::ShotsRun>>,
    controls: Option<Res<GrassCaptureControls>>,
    fade: Res<GrassFade>,
    fogs: Query<(), (With<StreamingCamera>, With<bevy::pbr::DistanceFog>)>,
    tuning: Option<Res<crate::grass_tuning::GrassTuningState>>,
    ambient: Option<Res<GlobalAmbientLight>>,
    cube: Option<Res<bevy::pbr::DirectionalAmbientCube>>,
    profile: Option<Res<crate::visual_profile::VisualProfile>>,
    mut light_units: Option<ResMut<GrassLightUnits>>,
    batches: Query<(&GrassBatch, Option<&GrassAttached>)>,
    models: Res<GrassModels>,
    mut metrics: ResMut<GrassRenderMetrics>,
) {
    // The profile is read once at startup. Grass takes retail's light units only under `skyrim`.
    let units = profile
        .as_deref()
        .copied()
        .map_or_else(GrassLightUnits::default, GrassLightUnits::of);
    if let Some(current) = light_units.as_deref_mut()
        && *current != units
    {
        *current = units;
    }
    for camera in &retired_views {
        commands.entity(camera).remove::<GrassView>();
    }
    for (camera, _, component, _, _, _, marker) in &cameras {
        let active = component.is_none_or(|camera| camera.is_active);
        if active && marker.is_none() {
            commands.entity(camera).insert(GrassView);
        } else if !active && marker.is_some() {
            commands.entity(camera).remove::<GrassView>();
        }
    }
    let view = cameras
        .single()
        .ok()
        .filter(|(_, _, camera, _, _, _, _)| camera.is_none_or(|camera| camera.is_active))
        .map(
            |(entity, transform, camera, msaa, tonemapping, hdr, _)| GrassProofView {
                camera: entity,
                world_from_view: transform.to_matrix(),
                clip_from_view: camera.map_or(Mat4::IDENTITY, Camera::clip_from_view),
                samples: msaa.map_or(1, |msaa| msaa.samples()),
                tonemapping: tonemapping.copied(),
                hdr,
            },
        );
    let eye = view.map(|view| view.world_from_view.w_axis.truncate());
    let mut all_drawn = true;
    let capture = controls.as_ref().is_some_and(|c| c.capture);
    for (transform, mut primitive) in &mut primitives {
        primitive.suppress_draw =
            capture && shots.as_ref().is_some_and(|run| run.grass_draws_off());
        if primitive.proof_view != view
            || primitive.uniform.world_from_cell != transform.to_matrix()
        {
            // Old render-world copies retain evidence only for their original view.
            primitive.drawn.next_view();
            primitive.culled.next_view();
        }
        primitive.proof_view = view;
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
            // cb2[16].w: the output scale (RF-SHADER-001, RF-SHADER-004; output_scale.rs). The
            // DistanceFog component stands in for "retail fog distances are non-zero" (unverified).
            crate::output_scale::framebuffer_output_scale(!fogs.is_empty()),
            if fade.enabled { 0.0 } else { 1.0 },
        );
        if let Some(ambient) = &ambient {
            primitive.uniform.ambient = units.ambient(
                grass_ambient_colour(ambient, cube.as_deref().filter(|_| units.retail)),
                ambient.brightness,
            );
        }
        all_drawn &= primitive.suppress_draw
            || primitive.culled.load(Ordering::Acquire)
            || primitive.drawn.load(Ordering::Acquire)
            || eye.is_some_and(|eye| visible_instance_count(&primitive, eye) == 0);
    }
    // Cached model diagnostics remain available; each view counts its currently required models.
    metrics.failures = batches
        .iter()
        .filter(|(batch, _)| {
            models
                .0
                .get(&batch.model_path)
                .is_some_and(|model| model.failed)
        })
        .map(|(batch, _)| batch.model_path.as_str())
        .collect::<HashSet<_>>()
        .len() as u64;
    metrics.ready = eye.is_some()
        && metrics.failures == 0
        && batches.iter().all(|(_, attached)| attached.is_some())
        && all_drawn;
}

#[derive(Resource)]
struct GrassPipeline {
    shader: Handle<Shader>,
    view_layout: BindGroupLayoutDescriptor,
    batch_layout: BindGroupLayoutDescriptor,
    scene: Option<bevy::pbr::MeshPipeline>,
}

/// Load the grass shader and initialize its render pipeline resource.
fn init_pipeline(mut commands: Commands, server: Res<AssetServer>) {
    let pipeline = GrassPipeline::new(load_embedded_asset!(server.as_ref(), "shaders/grass.wgsl"));
    commands.insert_resource(pipeline);
}

impl GrassPipeline {
    /// Create grass view and batch layouts for the supplied shader.
    fn new(shader: Handle<Shader>) -> Self {
        Self {
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
            // queue_grass installs the scene pipeline before submitting scene keys.
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
            // Bevy's fog entry point shares the PBR library's import graph.
            // Its unused material bindings are removed when composing apply_fog.
            "MATERIAL_BIND_GROUP".into(),
            3,
        ));
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
    units: GrassLightUnits,
) -> (Vec4, Vec4) {
    let sun = suns
        .iter()
        .filter(|(_, light)| light.render_layers.intersects(layers))
        // Keep in sync with Bevy's prepare_lights ordering before its per-view filter.
        .min_by_key(|(entity, light)| (light.volumetric, light.shadow_maps_enabled, *entity));
    sun.map_or((Vec4::Y, Vec4::ZERO), |(_, light)| {
        (
            light.transform.back().extend(0.0),
            units.sun(light.color.to_f32_array(), light.illuminance),
        )
    })
}

/// The ambient colour grass takes before [`GrassLightUnits::ambient`]. Under `skyrim` the near
/// meshes read the directional ambient cube (#1010), and grass takes the same cube's mean, so grass
/// and the near meshes use one ambient in one unit. Without an enabled cube (the `mudcrab`
/// profile, or a record that lacks the lighting) it is the flat `GlobalAmbientLight` colour, as
/// before. Grass has no normal in retail's grass shader that matches the cube's by-normal form
/// (unverified), so it takes the mean.
fn grass_ambient_colour(
    ambient: &GlobalAmbientLight,
    cube: Option<&bevy::pbr::DirectionalAmbientCube>,
) -> [f32; 4] {
    match cube {
        Some(cube) if cube.enabled => cube.mean.extend(1.0).to_array(),
        _ => ambient.color.to_linear().to_f32_array(),
    }
}

/// Which light units the grass shader's sun and ambient uniforms are in.
///
/// Under `skyrim` they are retail's grass units: the raw sun and the raw ambient (see
/// [`GrassLightUnits::sun`] and [`GrassLightUnits::ambient`]). Under `mudcrab` they are the
/// scene's own sun and ambient, unchanged, so that profile renders as it always did.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, ExtractResource)]
pub(crate) struct GrassLightUnits {
    pub retail: bool,
}

impl GrassLightUnits {
    pub(crate) fn of(profile: crate::visual_profile::VisualProfile) -> Self {
        Self {
            retail: profile == crate::visual_profile::VisualProfile::Skyrim,
        }
    }

    /// The sun colour the grass shader receives.
    ///
    /// The scene's sun (`SUN_ILLUMINANCE`) stands for retail's Lighting sun,
    /// `NiLight.diffuse * SunlightScale`. Retail grass does not take `SunlightScale`: its pixel
    /// shader gets the raw `NiLight.diffuse` (RF-GRASS-004; skyrim-research NEEDS 18 question 3,
    /// skyrim_decomp PR 1059; static reading, unverified at run time). So under `skyrim` the
    /// grass sun is the scene sun divided by `SUNLIGHT_SCALE`. Without the division, grass in the
    /// sun is `SUNLIGHT_SCALE` times too bright beside every other surface (#796). The constant
    /// is the exterior's; an interior key light carries its own scale (XCIM), so a later
    /// interior-grass change must not copy it. The optional sheen reads this colour too, so it
    /// drops by the same factor.
    pub(crate) fn sun(self, colour: [f32; 4], illuminance: f32) -> Vec4 {
        let scale = if self.retail {
            crate::lights::SUNLIGHT_SCALE
        } else {
            1.0
        };
        Vec4::from_array(colour) * (illuminance / scale)
    }

    /// The ambient colour the grass shader receives.
    ///
    /// The shader divides the whole lit sum by pi. That is exact for the sun (Bevy's Lambert
    /// diffuse is `albedo / pi`) and wrong for an ambient, which retail applies as
    /// `albedo * ambient` (RF-GRASS-004: grass takes the raw ambient; unverified at run time).
    /// Under `skyrim` the ambient carries a factor pi here, which cancels the shader's `1 / pi`,
    /// so grass gets the raw ambient. The factor sits in the uniform, not in the shader, so the
    /// `mudcrab` profile stays byte-identical and the pinned shader fixture is unchanged.
    ///
    /// The near meshes take the same unit: under `skyrim` their ambient term adds
    /// `albedo * ambient` from the directional cube with no `EnvBRDFApprox` scale (#1010), and grass
    /// takes the cube's mean ([`grass_ambient_colour`]). Whether retail's grass `NiLight.ambient` is
    /// the same value as the near Lighting ambient is unverified.
    pub(crate) fn ambient(self, colour: [f32; 4], brightness: f32) -> Vec4 {
        let factor = if self.retail {
            std::f32::consts::PI
        } else {
            1.0
        };
        Vec4::from_array(colour) * (brightness * factor)
    }
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
    units: Option<Res<GrassLightUnits>>,
) {
    let layers = views.single().ok().flatten().cloned().unwrap_or_default();
    let (sun_direction, sun_color) = prepared_grass_sun(
        &suns,
        &layers,
        // silent-ok: `GrassRenderPlugin` initialises and extracts this resource before this system runs, so only a world without the plugin takes the default.
        units.as_deref().copied().unwrap_or_default(),
    );
    buffers
        .0
        .retain(|_, (instances, _)| instances.strong_count() > 0);
    let limit = if budget.0 == 0 { usize::MAX } else { budget.0 };
    let mut uploaded = 0;
    for (entity, data, gpu) in &mut query {
        if let Some(mut gpu) = gpu
            && Arc::ptr_eq(&gpu.instances, &data.instances)
        {
            let uniform =
                updated_grass_uniform(&data.uniform, gpu.uniform.get(), sun_direction, sun_color);
            gpu.uniform.set(uniform);
            gpu.uniform.write_buffer(&device, &queue);
            continue;
        }
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
        value.normal_params.z = u32::from(matches!(
            normal.texture_descriptor.format,
            TextureFormat::Bc5RgUnorm | TextureFormat::Bc5RgSnorm
        )) as f32;
        value.normal_params.w =
            u32::from(normal.texture_descriptor.format == TextureFormat::Bc5RgSnorm) as f32;
        value.sun_direction = sun_direction;
        value.sun_color = sun_color;
        let mut uniform = UniformBuffer::from(value);
        uniform.write_buffer(&device, &queue);
        let Some(binding) = uniform.binding() else {
            warn!("grass batch uniform is unavailable for {entity:?}");
            continue;
        };
        let bind_group = device.create_bind_group(
            "grass batch",
            &cache.get_bind_group_layout(&pipeline.batch_layout),
            &BindGroupEntries::sequential((
                binding,
                &texture.texture_view,
                &texture.sampler,
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

/// Refresh frame values while retaining texture-format decoding flags.
fn updated_grass_uniform(
    current: &GrassUniform,
    previous: &GrassUniform,
    sun_direction: Vec4,
    sun_color: Vec4,
) -> GrassUniform {
    let mut uniform = current.clone();
    uniform.normal_params.z = previous.normal_params.z;
    uniform.normal_params.w = previous.normal_params.w;
    uniform.sun_direction = sun_direction;
    uniform.sun_color = sun_color;
    uniform
}

/// Record visibility for the current grass view before queueing a draw.
fn grass_visible_in_view(data: &GrassPrimitive, clip_from_world: Mat4) -> bool {
    let visible = grass_in_frustum(data, clip_from_world);
    data.culled.store(!visible, Ordering::Release);
    visible
}

/// Select the camera whose render result can supply primitive evidence.
fn grass_matches_evidence_camera(data: &GrassPrimitive, camera: MainEntity) -> bool {
    data.proof_view
        .is_some_and(|view| view.camera == camera.id())
}

/// Refresh main-draw evidence when its specialization or availability changes.
fn record_grass_pipeline(data: &GrassPrimitive, key: GrassKey, ready: bool) {
    let mut previous = data
        .proof_pipeline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !ready || previous.is_some_and(|previous| previous != key) {
        data.drawn.store(false, Ordering::Release);
    }
    *previous = Some(key);
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
    prepass_draws: Res<GrassPrepassDraws>,
) {
    prepass_draws
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    pipeline.scene = Some(scene.clone());
    clear_queued_grass(&mut queued, &mut main_phases, &mut pre_phases);
    for (view, msaa, camera, tonemapping) in &views {
        for (entity, main_entity, data) in &primitives {
            if !grass_matches_evidence_camera(data, view.retained_view_entity.main_entity) {
                continue;
            }
            if !grass_visible_in_view(
                data,
                view.clip_from_world.unwrap_or_else(|| {
                    view.clip_from_view * view.world_from_view.to_matrix().inverse()
                }),
            ) {
                continue;
            }
            let Some(mesh) = meshes.get(&data.mesh) else {
                data.drawn.store(false, Ordering::Release);
                continue;
            };
            let Some(slabs) = allocator.mesh_slabs(&data.mesh.id()) else {
                data.drawn.store(false, Ordering::Release);
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
                        // The terrain view uses the scene pipeline's matching layout.
                        data.drawn.store(false, Ordering::Release);
                        continue;
                    };
                    key.scene = Some(scene_key);
                }
                let Ok(id) = pipelines.specialize(&cache, &pipeline, key, &mesh.layout) else {
                    if !prepass {
                        data.drawn.store(false, Ordering::Release);
                    }
                    continue;
                };
                if !prepass {
                    record_grass_pipeline(data, key, cache.get_render_pipeline(id).is_some());
                }
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

/// Test the entire cell and its authored blade reach, never a single-blade Mesh3d AABB.
fn grass_in_frustum(data: &GrassPrimitive, clip_from_world: Mat4) -> bool {
    let reach = data.blade_reach;
    if !reach.is_finite() {
        return true;
    }
    let low = Vec3::new(-reach, data.uniform.height_wind.x - reach, -4096.0 - reach);
    let high = Vec3::new(
        4096.0 + reach,
        data.uniform.height_wind.x + data.uniform.height_wind.y + reach,
        reach,
    );
    let clip = clip_from_world * data.uniform.world_from_cell;
    let corners: [Vec4; 8] = std::array::from_fn(|i| {
        clip * Vec3::new(
            if i & 1 == 0 { low.x } else { high.x },
            if i & 2 == 0 { low.y } else { high.y },
            if i & 4 == 0 { low.z } else { high.z },
        )
        .extend(1.0)
    });
    !(0..6).any(|plane| {
        corners.iter().all(|p| match plane {
            0 => p.x < -p.w,
            1 => p.x > p.w,
            2 => p.y < -p.w,
            3 => p.y > p.w,
            4 => p.z < 0.0,
            _ => p.z > p.w,
        })
    })
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
    crate::grass::submitted_instance_count(
        &data.instances,
        data.slot_count,
        eye,
        data.uniform.fade.x,
        data.uniform.fade.y,
        true,
    )
}

/// Bevy's late prepass repeats NonMesh bins; issue each view/primitive depth draw once.
#[derive(Resource, Default)]
struct GrassPrepassDraws(std::sync::Mutex<HashSet<(RetainedViewEntity, Entity)>>);

struct DrawGrassInstances;
impl<P: PhaseItem> RenderCommand<P> for DrawGrassInstances {
    type Param = (
        SRes<RenderAssets<RenderMesh>>,
        SRes<MeshAllocator>,
        SRes<GrassPrepassDraws>,
    );
    type ViewQuery = (
        &'static ViewUniformOffset,
        &'static ExtractedView,
        &'static GrassViewBindGroup,
        Option<&'static bevy::pbr::MeshViewBindGroup>,
    );
    type ItemQuery = (Read<GrassPrimitive>, Read<GrassGpu>);
    /// Bind grass resources and issue an instanced draw for the prepared primitive.
    fn render<'w>(
        phase: &P,
        (offset, view, view_group, scene_group): QueryItem<'w, '_, Self::ViewQuery>,
        item: Option<QueryItem<'w, '_, Self::ItemQuery>>,
        (meshes, allocator, prepass_draws): SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some((data, gpu)) = item else {
            return RenderCommandResult::Skip;
        };
        if data.suppress_draw
            || !grass_matches_evidence_camera(data, view.retained_view_entity.main_entity)
        {
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
        if std::any::TypeId::of::<P>() == std::any::TypeId::of::<AlphaMask3dPrepass>()
            && !prepass_draws
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((view.retained_view_entity, phase.entity()))
        {
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
    use bevy::asset::RenderAssetUsages;
    use bevy::gltf::GltfPrimitive;

    include!("grass_render_alpha_tests.rs");

    /// Retail grass takes the raw sun and the raw ambient (RF-GRASS-004). The scene's sun stands for
    /// `diffuse * SunlightScale`, so grass divides it. The shader divides the lit sum by pi, so
    /// the grass ambient carries the matching factor and the sun does not. `mudcrab` keeps the
    /// scene's own values.
    #[test]
    fn grass_light_units_by_profile() {
        let retail = GrassLightUnits::of(crate::visual_profile::VisualProfile::Skyrim);
        let scene = GrassLightUnits::of(crate::visual_profile::VisualProfile::Mudcrab);
        assert!(retail.retail && !scene.retail);
        assert_eq!(scene, GrassLightUnits::default());

        let colour = [0.5, 1.0, 0.25, 1.0];
        let lux = crate::lights::SUN_ILLUMINANCE;
        let divided = Vec4::from_array(colour) * (lux / crate::lights::SUNLIGHT_SCALE);
        assert!(retail.sun(colour, lux).abs_diff_eq(divided, 1.0e-3));
        // The old expression, bit for bit.
        assert_eq!(scene.sun(colour, lux), Vec4::from_array(colour) * lux);

        let brightness = crate::lights::AMBIENT_ILLUMINANCE;
        let pi = std::f32::consts::PI;
        let raised = Vec4::from_array(colour) * (brightness * pi);
        assert!(
            retail
                .ambient(colour, brightness)
                .abs_diff_eq(raised, 1.0e-2)
        );
        assert_eq!(
            scene.ambient(colour, brightness),
            Vec4::from_array(colour) * brightness
        );
    }

    /// The call site: `update_grass_uniforms` writes the ambient uniform and the extracted unit
    /// switch from the profile. Reverting either line fails this test.
    #[test]
    fn update_grass_uniforms_applies_the_profile_light_units() {
        use crate::visual_profile::VisualProfile;
        let ambient = GlobalAmbientLight {
            color: Color::linear_rgb(0.5, 0.25, 1.0),
            brightness: 400.0,
            ..default()
        };
        let expected_ambient = |units: GrassLightUnits| {
            units.ambient(ambient.color.to_linear().to_f32_array(), ambient.brightness)
        };
        for profile in [
            None,
            Some(VisualProfile::Mudcrab),
            Some(VisualProfile::Skyrim),
        ] {
            let mut app = App::new();
            app.insert_resource(GrassFade {
                enabled: true,
                start: 1000.0,
                end: 2000.0,
            })
            .insert_resource(GrassModels::default())
            .insert_resource(GrassRenderMetrics::default())
            .init_resource::<GrassLightUnits>()
            .insert_resource(ambient.clone())
            .add_systems(Update, update_grass_uniforms);
            if let Some(profile) = profile {
                app.insert_resource(profile);
            }
            app.world_mut()
                .spawn((StreamingCamera, GlobalTransform::IDENTITY));
            let entity = app
                .world_mut()
                .spawn((primitive(), GlobalTransform::IDENTITY))
                .id();
            app.update();
            let units = GrassLightUnits {
                retail: profile == Some(VisualProfile::Skyrim),
            };
            assert_eq!(*app.world().resource::<GrassLightUnits>(), units);
            let uniform = &app.world().get::<GrassPrimitive>(entity).unwrap().uniform;
            assert_eq!(uniform.ambient, expected_ambient(units), "{profile:?}");
            let plain =
                Vec4::from_array(ambient.color.to_linear().to_f32_array()) * ambient.brightness;
            assert_eq!(
                uniform.ambient == plain,
                !units.retail,
                "only skyrim changes the ambient: {profile:?}"
            );
        }
    }

    /// Grass and the near meshes use one ambient in one unit under `skyrim` (#1010, #1011). The
    /// near ambient term adds `cube.mean * brightness` to the diffuse sum before the view
    /// exposure, with no other scale (`pbr_ambient.wgsl`; the render pixel test
    /// `ambient_pixel_tests` pins that). The grass shader divides its lit sum by pi and the
    /// uniform carries a factor of pi, so grass adds the same `cube.mean * brightness`. This test
    /// runs the call site with an enabled cube and checks the uniform against the near term.
    /// `mudcrab` keeps its flat ambient even if a cube exists.
    #[test]
    fn grass_and_near_meshes_take_the_same_ambient_under_skyrim() {
        use crate::visual_profile::VisualProfile;
        let ambient = GlobalAmbientLight {
            // The flat, decoded colour: not what the cube holds.
            color: Color::linear_rgb(0.1, 0.1, 0.1),
            brightness: 400.0,
            ..default()
        };
        let cube = bevy::pbr::DirectionalAmbientCube {
            enabled: true,
            mean: Vec3::new(0.5, 0.4, 0.3),
            slope_x: Vec3::new(0.1, 0.0, 0.0),
            ..default()
        };
        for profile in [VisualProfile::Mudcrab, VisualProfile::Skyrim] {
            let mut app = App::new();
            app.insert_resource(GrassFade {
                enabled: true,
                start: 1000.0,
                end: 2000.0,
            })
            .insert_resource(GrassModels::default())
            .insert_resource(GrassRenderMetrics::default())
            .init_resource::<GrassLightUnits>()
            .insert_resource(ambient.clone())
            .insert_resource(cube)
            .insert_resource(profile)
            .add_systems(Update, update_grass_uniforms);
            app.world_mut()
                .spawn((StreamingCamera, GlobalTransform::IDENTITY));
            let entity = app
                .world_mut()
                .spawn((primitive(), GlobalTransform::IDENTITY))
                .id();
            app.update();
            let uniform = app
                .world()
                .get::<GrassPrimitive>(entity)
                .unwrap()
                .uniform
                .ambient;
            // What the grass shader adds: the uniform divided by its `1 / pi`.
            let grass_adds = uniform.truncate() / std::f32::consts::PI;
            // What the near ambient term adds for a normal-less surface: the cube mean at the
            // scene brightness.
            let near_adds = cube.evaluate(Vec3::ZERO) * ambient.brightness;
            if profile == VisualProfile::Skyrim {
                assert!(
                    (grass_adds - near_adds).abs().max_element() < 1.0e-3,
                    "grass adds {grass_adds}, near meshes add {near_adds}"
                );
            } else {
                let flat = Vec3::splat(0.1) * ambient.brightness;
                assert!(
                    (uniform.truncate() - flat).abs().max_element() < 1.0e-3,
                    "mudcrab grass keeps the flat ambient"
                );
            }
        }
    }

    /// Keep signed and unsigned BC5 decode flags through successive live updates.
    #[test]
    fn bot_bc5_flags_survive_successive_uniform_updates() {
        for flags in [Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0), Vec2::ZERO] {
            let mut uploaded = primitive().uniform;
            uploaded.normal_params = Vec4::new(1.0, -1.0, flags.x, flags.y);
            let mut current = primitive().uniform;
            current.normal_params.x = 1.0;
            current.normal_params.y = -1.0;
            for brightness in [3.0, 7.0] {
                current.ambient = Vec4::splat(brightness);
                uploaded = updated_grass_uniform(&current, &uploaded, Vec4::Z, Vec4::splat(5.0));
                assert_eq!(uploaded.normal_params.zw(), flags);
                assert_eq!(uploaded.normal_params.xy(), Vec2::new(1.0, -1.0));
                assert_eq!(uploaded.ambient, Vec4::splat(brightness));
                assert_eq!(uploaded.sun_direction, Vec4::Z);
                assert_eq!(uploaded.sun_color, Vec4::splat(5.0));
            }
        }
    }

    /// Retain shared live model handles and release a model after its last batch leaves.
    #[test]
    fn bot_model_handles_follow_current_batches() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Gltf>()
            .init_resource::<GrassModels>()
            .init_resource::<GrassRenderMetrics>()
            .add_systems(Update, load_grass_models);
        let handle = app.world_mut().resource_mut::<Assets<Gltf>>().add(Gltf {
            scenes: Vec::new(),
            named_scenes: Default::default(),
            meshes: Vec::new(),
            named_meshes: Default::default(),
            materials: Vec::new(),
            named_materials: Default::default(),
            nodes: Vec::new(),
            named_nodes: Default::default(),
            skins: Vec::new(),
            named_skins: Default::default(),
            default_scene: None,
            animations: Vec::new(),
            named_animations: Default::default(),
            source: None,
        });
        let model = GrassModel {
            handle,
            templates: Some(Vec::new()),
            build: TemplateBuild::default(),
            failed: false,
        };
        app.world_mut()
            .resource_mut::<GrassModels>()
            .0
            .insert("meshes/grass.glb".into(), model);
        let first = app.world_mut().spawn(batch()).id();
        let second = app.world_mut().spawn(batch()).id();
        app.update();
        assert_eq!(app.world().resource::<GrassModels>().0.len(), 1);
        app.world_mut().despawn(first);
        app.update();
        assert_eq!(app.world().resource::<GrassModels>().0.len(), 1);
        app.world_mut()
            .resource_mut::<GrassModels>()
            .0
            .get_mut("meshes/grass.glb")
            .unwrap()
            .failed = true;
        app.world_mut()
            .resource_mut::<GrassRenderMetrics>()
            .failures = 1;
        app.world_mut().despawn(second);
        app.update();
        assert!(app.world().resource::<GrassModels>().0.is_empty());
        assert_eq!(app.world().resource::<GrassRenderMetrics>().failures, 0);
        app.world_mut().spawn(batch());
        app.update();
        assert!(!app.world().resource::<GrassModels>().0["meshes/grass.glb"].failed);
    }

    /// Require a fresh main draw after specialization changes or a pipeline recompiles.
    #[test]
    fn bot_queued_pipeline_changes_refresh_draw_evidence() {
        let data = primitive();
        let key = GrassKey::new(1, TextureFormat::Rgba8UnormSrgb, false, false, None);
        record_grass_pipeline(&data, key, true);
        data.drawn.store(true, Ordering::Release);
        record_grass_pipeline(&data, key, true);
        assert!(data.drawn.load(Ordering::Acquire));
        let next = GrassKey { samples: 4, ..key };
        record_grass_pipeline(&data, next, true);
        assert!(!data.drawn.load(Ordering::Acquire));
        data.drawn.store(true, Ordering::Release);
        record_grass_pipeline(&data, next, false);
        assert!(!data.drawn.load(Ordering::Acquire));
    }

    /// A culled primitive satisfies only its present view and obtains draw proof from a draw.
    #[test]
    fn bot_culled_primitive_keeps_draw_proof_scoped_to_current_camera() {
        let mut app = App::new();
        app.insert_resource(GrassFade {
            enabled: false,
            start: 1000.0,
            end: 2000.0,
        })
        .init_resource::<GrassModels>()
        .init_resource::<GrassRenderMetrics>()
        .add_systems(Update, update_grass_uniforms);
        let camera = app
            .world_mut()
            .spawn((StreamingCamera, GlobalTransform::IDENTITY))
            .id();
        app.world_mut().spawn((batch(), GrassAttached));
        let mut data = primitive();
        data.blade_reach = 1.0;
        let entity = app
            .world_mut()
            .spawn((
                data,
                GlobalTransform::from_translation(Vec3::new(-10000.0, 0.0, 0.0)),
            ))
            .id();
        app.update();
        let old_render_copy = app.world().get::<GrassPrimitive>(entity).unwrap().clone();
        let proof = old_render_copy.drawn.clone();
        let clip = Mat4::orthographic_rh(-20.0, 20.0, -20.0, 20.0, 0.1, 5000.0);
        assert!(!grass_visible_in_view(&old_render_copy, clip));
        assert!(
            !proof.load(Ordering::Acquire),
            "culling leaves real draw proof unset"
        );
        app.update();
        assert!(app.world().resource::<GrassRenderMetrics>().ready);
        app.world_mut()
            .entity_mut(camera)
            .insert(GlobalTransform::from(Transform::from_rotation(
                Quat::from_rotation_y(1.0),
            )));
        app.update();
        assert!(
            !app.world().resource::<GrassRenderMetrics>().ready,
            "a camera turn requires fresh view evidence"
        );
        old_render_copy.drawn.store(true, Ordering::Release);
        app.update();
        assert!(
            !app.world().resource::<GrassRenderMetrics>().ready,
            "old extracted draws satisfy their original view"
        );
        app.world()
            .get::<GrassPrimitive>(entity)
            .unwrap()
            .drawn
            .store(true, Ordering::Release);
        app.update();
        assert!(app.world().resource::<GrassRenderMetrics>().ready);
    }

    /// Require new draw evidence after a previously drawn camera turns.
    #[test]
    fn bot_camera_turn_requires_fresh_draw_proof() {
        assert_view_edit_requires_fresh_draw(|world, camera| {
            world
                .entity_mut(camera)
                .insert(GlobalTransform::from(Transform::from_rotation(
                    Quat::from_rotation_y(1.0),
                )));
        });
    }

    /// Require new draw evidence when the camera changes render specialization.
    #[test]
    fn bot_pipeline_settings_require_fresh_draw_proof() {
        assert_view_edit_requires_fresh_draw(|world, camera| {
            world.entity_mut(camera).insert(Msaa::Sample4);
        });
    }

    /// A replacement streaming camera owns the sole marker used to queue grass.
    #[test]
    fn bot_view_transition_retires_former_camera_marker() {
        let (mut app, camera, _) = view_transition_app();
        app.update();
        assert!(app.world().get::<GrassView>(camera).is_some());
        app.world_mut()
            .entity_mut(camera)
            .remove::<StreamingCamera>();
        let replacement = app
            .world_mut()
            .spawn((StreamingCamera, GlobalTransform::IDENTITY))
            .id();
        app.update();
        assert!(app.world().get::<GrassView>(replacement).is_some());
        assert!(
            app.world().get::<GrassView>(camera).is_none(),
            "the former camera leaves the grass render views"
        );
    }

    /// A render result captured while no camera was selected stays in that generation.
    #[test]
    fn bot_view_transition_camera_recovery_requires_new_evidence() {
        let (mut app, camera, entity) = view_transition_app();
        app.update();
        app.world_mut()
            .entity_mut(camera)
            .remove::<StreamingCamera>();
        app.update();
        assert!(!app.world().resource::<GrassRenderMetrics>().ready);
        let absent_view = app.world().get::<GrassPrimitive>(entity).unwrap().clone();
        app.world_mut().entity_mut(camera).insert(StreamingCamera);
        app.update();
        absent_view.drawn.store(true, Ordering::Release);
        app.update();
        assert!(
            !app.world().resource::<GrassRenderMetrics>().ready,
            "camera recovery awaits a draw captured for the restored camera"
        );
    }

    /// A disabled camera remains pending until an active view supplies a fresh draw.
    #[test]
    fn bot_view_transition_disabled_camera_waits_for_active_draw() {
        let (mut app, camera, entity) = view_transition_app();
        app.world_mut().entity_mut(camera).insert(Camera::default());
        app.update();
        app.world()
            .get::<GrassPrimitive>(entity)
            .unwrap()
            .drawn
            .store(true, Ordering::Release);
        app.update();
        assert!(app.world().resource::<GrassRenderMetrics>().ready);
        app.world_mut().get_mut::<Camera>(camera).unwrap().is_active = false;
        app.update();
        assert!(
            !app.world().resource::<GrassRenderMetrics>().ready,
            "a disabled camera waits for an active view"
        );
    }

    /// Former camera views cannot cull or draw on behalf of the selected camera.
    #[test]
    fn bot_view_transition_accepts_only_selected_camera() {
        let (mut app, camera, entity) = view_transition_app();
        let former_camera = app.world_mut().spawn_empty().id();
        app.update();
        let mut data = app.world().get::<GrassPrimitive>(entity).unwrap().clone();
        assert!(grass_matches_evidence_camera(&data, camera.into()));
        assert!(!grass_matches_evidence_camera(&data, former_camera.into()));
        data.proof_view = None;
        assert!(!grass_matches_evidence_camera(&data, camera.into()));
    }

    /// Build an admitted primitive and independently replaceable streaming camera.
    fn view_transition_app() -> (App, Entity, Entity) {
        let mut app = App::new();
        app.insert_resource(GrassFade {
            enabled: false,
            start: 1000.0,
            end: 2000.0,
        })
        .init_resource::<GrassModels>()
        .init_resource::<GrassRenderMetrics>()
        .add_systems(Update, update_grass_uniforms);
        let camera = app
            .world_mut()
            .spawn((StreamingCamera, GlobalTransform::IDENTITY))
            .id();
        app.world_mut().spawn((batch(), GrassAttached));
        let entity = app
            .world_mut()
            .spawn((primitive(), GlobalTransform::IDENTITY))
            .id();
        (app, camera, entity)
    }

    /// Projection refreshes reach the production grass snapshot in the same frame.
    #[test]
    fn bot_projection_refresh_precedes_grass_proof_snapshot() {
        /// Publish an asymmetric computed projection in Bevy's camera phase.
        fn refresh_projection(mut cameras: Query<&mut Camera>) {
            for mut camera in &mut cameras {
                camera.computed.clip_from_view = Mat4::from_scale(Vec3::new(2.0, 3.0, 1.0));
            }
        }
        for camera_phase_first in [true, false] {
            let mut app = App::new();
            app.add_plugins((
                bevy::app::TaskPoolPlugin::default(),
                bevy::time::TimePlugin,
                bevy::transform::TransformPlugin,
                bevy::asset::AssetPlugin::default(),
            ))
            .insert_resource(crate::config::EngineConfig::default())
            .init_asset::<Shader>()
            .init_asset::<Gltf>()
            .init_asset::<GltfNode>()
            .init_asset::<GltfMesh>()
            .init_asset::<GltfMaterial>()
            .insert_resource(GrassFade {
                enabled: false,
                start: 1000.0,
                end: 2000.0,
            });
            if camera_phase_first {
                app.add_systems(
                    PostUpdate,
                    refresh_projection
                        .in_set(bevy::camera::CameraUpdateSystems)
                        .after(bevy::transform::TransformSystems::Propagate),
                );
            }
            app.add_plugins(GrassRenderPlugin);
            if !camera_phase_first {
                app.add_systems(
                    PostUpdate,
                    refresh_projection
                        .in_set(bevy::camera::CameraUpdateSystems)
                        .after(bevy::transform::TransformSystems::Propagate),
                );
            }
            app.edit_schedule(PostUpdate, |schedule| {
                schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::default());
                schedule.set_build_settings(bevy::ecs::schedule::ScheduleBuildSettings {
                    ambiguity_detection: bevy::ecs::schedule::LogLevel::Error,
                    ..default()
                });
            });
            app.world_mut().spawn((
                StreamingCamera,
                Camera::default(),
                GlobalTransform::IDENTITY,
            ));
            let entity = app
                .world_mut()
                .spawn((primitive(), GlobalTransform::IDENTITY))
                .id();
            app.world_mut()
                .schedule_scope(PostUpdate, |world, schedule| {
                    let failure = schedule.initialize(world).err();
                    assert!(
                        failure.is_none(),
                        "grass camera projection order is unambiguous: {failure:?}"
                    );
                });
            app.update();
            assert_eq!(
                app.world()
                    .get::<GrassPrimitive>(entity)
                    .unwrap()
                    .proof_view
                    .unwrap()
                    .clip_from_view,
                Mat4::from_scale(Vec3::new(2.0, 3.0, 1.0)),
                "grass evidence snapshots the refreshed projection (camera phase first={camera_phase_first})"
            );
        }
    }

    /// Exercise a current-view edit after a primitive has issued an earlier main draw.
    fn assert_view_edit_requires_fresh_draw(edit: impl FnOnce(&mut World, Entity)) {
        let mut app = App::new();
        app.insert_resource(GrassFade {
            enabled: false,
            start: 1000.0,
            end: 2000.0,
        })
        .init_resource::<GrassModels>()
        .init_resource::<GrassRenderMetrics>()
        .add_systems(Update, update_grass_uniforms);
        let camera = app
            .world_mut()
            .spawn((StreamingCamera, GlobalTransform::IDENTITY, Msaa::Off))
            .id();
        app.world_mut().spawn((batch(), GrassAttached));
        let entity = app
            .world_mut()
            .spawn((primitive(), GlobalTransform::IDENTITY))
            .id();
        app.update();
        app.world()
            .get::<GrassPrimitive>(entity)
            .unwrap()
            .drawn
            .store(true, Ordering::Release);
        app.update();
        assert!(app.world().resource::<GrassRenderMetrics>().ready);
        edit(app.world_mut(), camera);
        app.update();
        assert!(
            !app.world().resource::<GrassRenderMetrics>().ready,
            "current view waits for its own main draw"
        );
    }

    #[test]
    fn frustum_uses_whole_cell_blade_reach_and_propagated_transform() {
        let mut data = primitive();
        data.blade_reach = 2.0;
        data.uniform.height_wind = Vec4::new(0.0, 10.0, 1.0, 0.5);
        // A tiny blade at the cell origin is outside this view; the far cell edge overlaps it.
        let clip = Mat4::orthographic_rh(4000.0, 4200.0, -20.0, 20.0, 0.1, 5000.0);
        assert!(grass_in_frustum(&data, clip));
        data.uniform.world_from_cell = Mat4::from_translation(Vec3::new(-10000.0, 0.0, 0.0));
        assert!(!grass_in_frustum(&data, clip));
        data.blade_reach = f32::INFINITY;
        assert!(
            grass_in_frustum(&data, clip),
            "missing geometry must not cull"
        );
    }

    /// Validate real scene shader layouts on software Vulkan, plus all MSAA semantic variants.
    #[test]
    #[ignore = "requires software Vulkan; run with the renderer smoke tests"]
    fn production_scene_pipeline_shader_uses_real_mesh_bindings() {
        let mut app = App::new();
        app.add_plugins(
            DefaultPlugins
                .build()
                .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>()
                .disable::<bevy::audio::AudioPlugin>()
                .disable::<bevy::winit::WinitPlugin>()
                .set(bevy::window::WindowPlugin {
                    primary_window: None,
                    exit_condition: bevy::window::ExitCondition::DontExit,
                    ..default()
                }),
        );
        let shader = app
            .world_mut()
            .resource_mut::<Assets<Shader>>()
            .add(Shader::from_wgsl(
                include_str!("shaders/grass.wgsl"),
                "grass.wgsl",
            ));
        app.finish();
        app.cleanup();
        app.update();
        let render = app.sub_app(RenderApp);
        let scene = render.world().resource::<bevy::pbr::MeshPipeline>().clone();
        let adapter = render
            .world()
            .resource::<bevy::render::renderer::RenderAdapter>()
            .clone();
        let info = adapter.get_info();
        assert_eq!(
            info.backend,
            wgpu::Backend::Vulkan,
            "software Vulkan required: {info:?}"
        );
        assert_eq!(
            info.device_type,
            wgpu::DeviceType::Cpu,
            "software adapter required: {info:?}"
        );
        eprintln!("grass pipeline software adapter: {info:?}");
        let mut pipeline = GrassPipeline::new(shader);
        pipeline.scene = Some(scene);
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
        let mut queued = Vec::new();
        for samples in [1, 2, 4, 8] {
            for hdr in [false, true] {
                let mut key = GrassKey::new(
                    samples,
                    TextureFormat::Rgba16Float,
                    false,
                    hdr,
                    Some(Tonemapping::Reinhard),
                );
                key.scene = Some(
                    bevy::pbr::MeshPipelineKey::from_msaa_samples(samples)
                        | bevy::pbr::MeshPipelineKey::from_target_format(
                            TextureFormat::Rgba16Float,
                        )
                        | if hdr {
                            bevy::pbr::MeshPipelineKey::empty()
                        } else {
                            bevy::pbr::MeshPipelineKey::TONEMAP_IN_SHADER
                                | bevy::pbr::MeshPipelineKey::TONEMAP_METHOD_REINHARD
                        },
                );
                let descriptor = pipeline.specialize(key, &layout).unwrap();
                assert_eq!(descriptor.layout.len(), 3);
                // PipelineCache supplies device globals and each imported shader's own
                // definitions. Use that production path instead of composing the descriptor
                // in isolation. CPU shader matrices also cover unsupported sample counts.
                if [TextureFormat::Rgba16Float, TextureFormat::Depth32Float]
                    .into_iter()
                    .all(|format| {
                        adapter
                            .get_texture_format_features(format)
                            .flags
                            .sample_count_supported(samples)
                    })
                {
                    let id = app
                        .sub_app(RenderApp)
                        .world()
                        .resource::<PipelineCache>()
                        .queue_render_pipeline(descriptor);
                    queued.push((samples, hdr, id));
                }
            }
        }
        assert!(
            queued.len() >= 2,
            "both single-sample HDR policies must compile"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            app.update();
            let cache = app.sub_app(RenderApp).world().resource::<PipelineCache>();
            if queued
                .iter()
                .all(|(_, _, id)| cache.get_render_pipeline(*id).is_some())
            {
                break;
            }
            if std::time::Instant::now() >= deadline {
                for (samples, hdr, id) in &queued {
                    assert!(
                        cache.get_render_pipeline(*id).is_some(),
                        "grass pipeline samples={samples} hdr={hdr}: {:?}",
                        cache.get_render_pipeline_state(*id)
                    );
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

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
                    B::ShiftLeft => ((left as u32) << (right as u32)) as f32,
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
                kind: wgpu::naga::ScalarKind::Float,
                ..
            } => eval(expr),
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
        let shader = include_str!("shaders/grass.wgsl");
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
        let source = source.replace("#{SAMPLES}", &samples.to_string());
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
        for samples in [1, 2, 4, 8] {
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
        for samples in [2, 4, 8] {
            assert_eq!(
                shader_coverage_samples(0.5, 0.5, 0.25, samples),
                samples as usize / 2
            );
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
        let mut defs = super::grass_shader_tests::definitions(false, 4, 15, false);
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
            super::grass_shader_tests::compose(include_str!("shaders/grass.wgsl"), defs).unwrap();
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
        // Under `skyrim` the grass sun is the scene sun divided by SunlightScale.
        let units = GrassLightUnits { retail: true };
        let per_scale = |value: Vec4| value / crate::lights::SUNLIGHT_SCALE;
        let (direction, color) = prepared_grass_sun(&suns, &RenderLayers::default(), units);
        assert_eq!(direction, Vec4::Z);
        assert!(color.abs_diff_eq(per_scale(Vec4::new(5.0, 10.0, 20.0, 20.0)), 1.0e-5));
        assert!(
            prepared_grass_sun(&suns, &RenderLayers::layer(1), units)
                .1
                .abs_diff_eq(per_scale(Vec4::new(0.0, 13.0, 0.0, 13.0)), 1.0e-5)
        );
        assert_eq!(
            prepared_grass_sun(&suns, &RenderLayers::layer(9), units),
            (Vec4::Y, Vec4::ZERO)
        );
        // `mudcrab` keeps the scene sun untouched.
        assert_eq!(
            prepared_grass_sun(&suns, &RenderLayers::default(), GrassLightUnits::default()).1,
            Vec4::new(5.0, 10.0, 20.0, 20.0)
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
            blade_reach: f32::INFINITY,
            instances: batch().instances,
            mesh: Handle::default(),
            texture: Handle::default(),
            normal_texture: Handle::default(),
            model: Mat4::IDENTITY,
            slot_count: 4,
            drawn: GrassViewEvidence::default(),
            culled: GrassViewEvidence::default(),
            proof_view: None,
            proof_pipeline: Arc::new(Mutex::new(None)),
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
            .init_resource::<GrassModels>()
            .init_resource::<GrassRenderMetrics>()
            .add_systems(Update, update_grass_uniforms);
        app.world_mut().spawn((
            StreamingCamera,
            GlobalTransform::from_translation(beyond_end),
        ));
        app.world_mut().spawn((batch(), GrassAttached));
        let primitive = app
            .world_mut()
            .spawn((data, GlobalTransform::IDENTITY))
            .id();
        app.update();
        let proof = app
            .world()
            .get::<GrassPrimitive>(primitive)
            .unwrap()
            .drawn
            .clone();
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
        let shader = include_str!("shaders/grass.wgsl");
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
            .init_resource::<GrassModels>()
            .init_resource::<GrassRenderMetrics>()
            .add_systems(Update, update_grass_uniforms);
        app.world_mut()
            .spawn((StreamingCamera, GlobalTransform::IDENTITY));
        let batch_entity = app.world_mut().spawn(batch()).id();
        let entity = app
            .world_mut()
            .spawn((primitive(), GlobalTransform::IDENTITY))
            .id();
        app.update();
        let proof = app
            .world()
            .get::<GrassPrimitive>(entity)
            .unwrap()
            .drawn
            .clone();
        assert!(!app.world().resource::<GrassRenderMetrics>().ready);
        app.world_mut()
            .entity_mut(batch_entity)
            .insert(GrassAttached);
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
        let mut failed = GrassModel {
            handle: Handle::default(),
            templates: None,
            build: TemplateBuild::default(),
            failed: false,
        };
        fail_model(
            &mut failed,
            &batch().model_path,
            "missing",
            &mut app.world_mut().resource_mut::<GrassRenderMetrics>(),
        );
        app.world_mut()
            .resource_mut::<GrassModels>()
            .0
            .insert(batch().model_path, failed);
        app.update();
        assert!(!app.world().resource::<GrassRenderMetrics>().ready);
    }

    /// Current batch requirements scope failures before model-cache retirement.
    #[test]
    fn failed_model_history_only_blocks_current_required_batches() {
        let mut failed = GrassModel {
            handle: Handle::default(),
            templates: None,
            build: TemplateBuild::default(),
            failed: false,
        };
        let failed_path = "meshes/broken-grass.glb".to_owned();
        let mut metrics = GrassRenderMetrics::default();
        fail_model(&mut failed, &failed_path, "missing", &mut metrics);
        let mut app = App::new();
        app.insert_resource(GrassFade {
            enabled: true,
            start: 1000.0,
            end: 2000.0,
        })
        .insert_resource(GrassModels(HashMap::from([(failed_path.clone(), failed)])))
        .insert_resource(metrics)
        .add_systems(Update, update_grass_uniforms);
        app.world_mut()
            .spawn((StreamingCamera, GlobalTransform::IDENTITY));
        let primitive = app
            .world_mut()
            .spawn((primitive(), GlobalTransform::IDENTITY))
            .id();

        let mut bad_batch = batch();
        bad_batch.model_path.clone_from(&failed_path);
        let first = app.world_mut().spawn(bad_batch.clone()).id();
        let second = app.world_mut().spawn(bad_batch.clone()).id();
        app.update();
        let renderer = app.world().resource::<GrassRenderMetrics>();
        assert_eq!(
            renderer.failures, 1,
            "one failed model shared by two batches"
        );
        assert!(!renderer.ready);
        let cpu = crate::grass::GrassMetrics {
            ready: true,
            ..default()
        };
        let mut counts = crate::shots::SettleCounts::default();
        counts.read_grass(true, Some(&cpu), Some(renderer));
        assert_eq!(counts.grass_failures, 1);
        assert!(counts.grass_pending);
        app.world()
            .get::<GrassPrimitive>(primitive)
            .unwrap()
            .drawn
            .store(true, Ordering::Release);

        app.world_mut().despawn(first);
        app.world_mut().despawn(second);
        let healthy = app.world_mut().spawn((batch(), GrassAttached)).id();
        app.update();
        let renderer = app.world().resource::<GrassRenderMetrics>();
        assert_eq!(
            renderer.failures, 0,
            "a healthy view must not inherit an earlier model failure"
        );
        assert!(renderer.ready, "the healthy attached batch has draw proof");
        counts.read_grass(true, Some(&cpu), Some(renderer));
        assert_eq!(counts.grass_failures, 0);
        assert!(!counts.grass_pending);
        assert!(
            app.world().resource::<GrassModels>().0[&failed_path].failed,
            "the loader owns model retirement after metrics observation"
        );

        app.world_mut().despawn(healthy);
        app.world_mut().spawn(bad_batch);
        app.update();
        let renderer = app.world().resource::<GrassRenderMetrics>();
        assert_eq!(
            renderer.failures, 1,
            "a returning failed model still blocks"
        );
        assert!(!renderer.ready);
        counts.read_grass(true, Some(&cpu), Some(renderer));
        assert_eq!(counts.grass_failures, 1);
        assert!(counts.grass_pending);
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
