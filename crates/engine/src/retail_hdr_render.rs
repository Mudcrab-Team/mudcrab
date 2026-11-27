//! The render-world passes of retail's HDR chain (`retail_hdr.rs`): the per-view targets, the
//! pipelines and the draws. [`encode_chain`] records the passes on plain `wgpu` types, so the GPU
//! test runs the same code as the engine.

use crate::{
    retail_fog::{RetailFogPass, run_retail_fog_pass},
    retail_hdr::RetailHdr,
    retail_hdr_chain::{ChainPass, ChainTarget, PassKind, bloom_size, chain_plan, chain_sizes},
};
use bevy::{
    asset::load_embedded_asset,
    core_pipeline::{
        FullscreenShader,
        core_3d::{main_opaque_pass_3d, main_transparent_pass_3d},
        schedule::{Core3d, Core3dSystems},
        tonemapping::tonemapping as tonemapping_system,
    },
    ecs::{schedule::ScheduleConfigs, system::BoxedSystem},
    pbr::main_transmissive_pass_3d,
    prelude::*,
    render::{
        Render, RenderApp, RenderStartup, RenderSystems,
        extract_component::{ComponentUniforms, DynamicUniformIndex},
        render_resource::{
            BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent,
            BlendFactor, BlendOperation, BlendState, CachedRenderPipelineId, ColorTargetState,
            ColorWrites, Extent3d, FragmentState, LoadOp, MultisampleState, Operations,
            PipelineCache, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor,
            RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages,
            SpecializedRenderPipeline, SpecializedRenderPipelines, Texture, TextureDescriptor,
            TextureDimension, TextureFormat, TextureSampleType, TextureUsages, TextureView,
            binding_types::{sampler, texture_2d, uniform_buffer},
        },
        renderer::{RenderContext, RenderDevice, ViewQuery},
        view::{ExtractedView, Msaa, ViewTarget},
    },
    shader::Shader,
};
use std::sync::atomic::{AtomicU32, Ordering};

/// The format of the chain, bloom and adapted-pair targets: the HDR format of the `Ultra` preset
/// (`bUse64bitsHDRRenderTarget` 1).
pub const CHAIN_FORMAT: TextureFormat = TextureFormat::Rgba16Float;

/// The Min-blend draw of 1.0 that saturates the opaque scene after the fog composite (D3).
pub const SATURATE_BLEND: BlendState = BlendState {
    color: BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Min,
    },
    alpha: BlendComponent {
        src_factor: BlendFactor::Zero,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
};

/// The chain and its tonemap. Each other pass that swaps the post-process texture is ordered
/// against this set (`post_process_order.rs`).
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RetailHdrSet;

/// The tonemap with its place in `PostProcess`: after Bevy's tone mapping, which does nothing for
/// a `skyrim` camera.
pub(crate) fn post_process_systems() -> ScheduleConfigs<BoxedSystem> {
    run_retail_hdr
        .in_set(Core3dSystems::PostProcess)
        .in_set(RetailHdrSet)
        .after(tonemapping_system)
}

/// Adds the render-world systems of the chain.
pub(crate) fn build(app: &mut App) {
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render_app
        .init_resource::<SpecializedRenderPipelines<TonemapPipeline>>()
        .init_resource::<SpecializedRenderPipelines<SaturatePipeline>>()
        .add_systems(RenderStartup, init_retail_hdr_pipelines)
        .add_systems(
            Render,
            (
                clear_inactive_retail_hdr_views.in_set(RenderSystems::PrepareResources),
                prepare_retail_hdr_targets.in_set(RenderSystems::PrepareResources),
                prepare_retail_hdr_pipelines.in_set(RenderSystems::Prepare),
            ),
        )
        .add_systems(
            Core3d,
            run_retail_saturate
                .in_set(Core3dSystems::MainPass)
                .after(main_opaque_pass_3d)
                // The fog pass is a blend; this clips its output.
                .after(run_retail_fog_pass)
                .before(main_transmissive_pass_3d)
                .before(main_transparent_pass_3d),
        )
        .add_systems(Core3d, post_process_systems());
}

/// The pipelines of the chain passes and the layout and samplers they share.
#[derive(Resource)]
struct RetailHdrPipelines {
    chain_layout: BindGroupLayoutDescriptor,
    chain: [CachedRenderPipelineId; PassKind::ALL.len()],
    linear: Sampler,
    nearest: Sampler,
}

#[derive(Resource)]
struct TonemapPipeline {
    shader: Handle<Shader>,
    layout: BindGroupLayoutDescriptor,
    fullscreen_shader: FullscreenShader,
}

#[derive(Resource)]
struct SaturatePipeline {
    shader: Handle<Shader>,
    fullscreen_shader: FullscreenShader,
}

/// Bindings of `retail_hdr_chain.wgsl`: the uniform, the source, the previous pair and the two
/// clamp samplers.
fn chain_layout() -> BindGroupLayoutDescriptor {
    BindGroupLayoutDescriptor::new(
        "retail_hdr_chain_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                uniform_buffer::<RetailHdr>(true),
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                sampler(SamplerBindingType::Filtering),
            ),
        ),
    )
}

/// Bindings of `retail_hdr_tonemap.wgsl`: the uniform, the scene, the bloom, the adapted pair and
/// the linear sampler.
fn tonemap_layout() -> BindGroupLayoutDescriptor {
    BindGroupLayoutDescriptor::new(
        "retail_hdr_tonemap_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                uniform_buffer::<RetailHdr>(true),
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
            ),
        ),
    )
}

fn chain_pipeline_descriptor(
    shader: Handle<Shader>,
    layout: BindGroupLayoutDescriptor,
    fullscreen: &FullscreenShader,
    kind: PassKind,
) -> RenderPipelineDescriptor {
    RenderPipelineDescriptor {
        label: Some(format!("retail_hdr_{}", kind.entry_point()).into()),
        layout: vec![layout],
        vertex: fullscreen.to_vertex_state(),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        fragment: Some(FragmentState {
            shader,
            shader_defs: Vec::new(),
            entry_point: Some(kind.entry_point().into()),
            targets: vec![Some(ColorTargetState {
                format: CHAIN_FORMAT,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
        }),
        ..default()
    }
}

fn init_retail_hdr_pipelines(
    mut commands: Commands,
    fullscreen_shader: Res<FullscreenShader>,
    asset_server: Res<AssetServer>,
    pipeline_cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
) {
    let chain_shader = load_embedded_asset!(asset_server.as_ref(), "shaders/retail_hdr_chain.wgsl");
    let layout = chain_layout();
    let chain = PassKind::ALL.map(|kind| {
        pipeline_cache.queue_render_pipeline(chain_pipeline_descriptor(
            chain_shader.clone(),
            layout.clone(),
            &fullscreen_shader,
            kind,
        ))
    });
    commands.insert_resource(RetailHdrPipelines {
        chain_layout: layout,
        chain,
        linear: device.create_sampler(&SamplerDescriptor {
            label: Some("retail_hdr_linear_clamp"),
            mag_filter: bevy::render::render_resource::FilterMode::Linear,
            min_filter: bevy::render::render_resource::FilterMode::Linear,
            ..default()
        }),
        nearest: device.create_sampler(&SamplerDescriptor {
            label: Some("retail_hdr_nearest_clamp"),
            ..default()
        }),
    });
    commands.insert_resource(TonemapPipeline {
        shader: load_embedded_asset!(asset_server.as_ref(), "shaders/retail_hdr_tonemap.wgsl"),
        layout: tonemap_layout(),
        fullscreen_shader: fullscreen_shader.clone(),
    });
    commands.insert_resource(SaturatePipeline {
        shader: load_embedded_asset!(asset_server.as_ref(), "shaders/retail_hdr_saturate.wgsl"),
        fullscreen_shader: fullscreen_shader.clone(),
    });
}

impl SpecializedRenderPipeline for TonemapPipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("retail_hdr_tonemap".into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen_shader.to_vertex_state(),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                shader_defs: Vec::new(),
                entry_point: Some("fragment".into()),
                targets: vec![Some(ColorTargetState {
                    format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            ..default()
        }
    }
}

/// The attachment format and sample count of the saturate draw.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct SaturateKey {
    format: TextureFormat,
    samples: u32,
}

impl SpecializedRenderPipeline for SaturatePipeline {
    type Key = SaturateKey;

    fn specialize(&self, key: SaturateKey) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("retail_hdr_saturate".into()),
            layout: Vec::new(),
            vertex: self.fullscreen_shader.to_vertex_state(),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState {
                count: key.samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                shader_defs: Vec::new(),
                entry_point: Some("fragment".into()),
                targets: vec![Some(ColorTargetState {
                    format: key.format,
                    blend: Some(SATURATE_BLEND),
                    // Alpha keeps the water reflection's "no geometry" value, as the fog pass does.
                    write_mask: ColorWrites::COLOR,
                })],
            }),
            ..default()
        }
    }
}

/// The per-view pipelines, written each frame.
#[derive(Component, Clone, Copy)]
struct RetailHdrPipelineIds {
    tonemap: CachedRenderPipelineId,
    saturate: CachedRenderPipelineId,
}

/// One render target of the chain.
struct Target {
    // Owns the texture the view looks into.
    _texture: Texture,
    view: TextureView,
}

/// The per-view targets. `state` is 0 while no adapted pair exists, else 1 plus the index of the
/// adapted texture that holds the last frame's pair.
#[derive(Component)]
struct RetailHdrTargets {
    key: (u32, u32, bool),
    levels: Vec<Target>,
    adapted: [Target; 2],
    bloom_swap: Target,
    bloom: Target,
    state: AtomicU32,
}

type InactiveRetailHdrView = (
    Without<RetailHdr>,
    Or<(With<RetailHdrTargets>, With<RetailHdrPipelineIds>)>,
);

fn clear_inactive_retail_hdr_views(
    mut commands: Commands,
    views: Query<Entity, InactiveRetailHdrView>,
) {
    for entity in &views {
        commands.entity(entity).remove::<(
            RetailHdrTargets,
            RetailHdrPipelineIds,
            DynamicUniformIndex<RetailHdr>,
        )>();
    }
}

fn target(device: &RenderDevice, label: &'static str, (width, height): (u32, u32)) -> Target {
    let texture = device.create_texture(&TextureDescriptor {
        label: Some(label),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: CHAIN_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&default());
    Target {
        _texture: texture,
        view,
    }
}

/// Makes the targets of every view for its size. New targets hold no adapted pair, so the next
/// frame sets it to the current luminance, as retail's render-target rebuild does (NEEDS 27).
fn prepare_retail_hdr_targets(
    mut commands: Commands,
    device: Res<RenderDevice>,
    views: Query<(Entity, &ViewTarget, &RetailHdr, Option<&RetailHdrTargets>)>,
) {
    for (entity, view, settings, existing) in &views {
        let texture = view.main_texture();
        let key = (
            texture.width(),
            texture.height(),
            settings.multiple_luminance_references(),
        );
        if existing.is_some_and(|targets| targets.key == key) {
            continue;
        }
        let (width, height, multiple) = key;
        let sizes = chain_sizes(width, height, multiple);
        let Some((&pair, intermediate)) = sizes.split_last() else {
            continue;
        };
        let bloom = bloom_size(width, height);
        commands.entity(entity).insert(RetailHdrTargets {
            key,
            levels: intermediate
                .iter()
                .map(|&size| target(&device, "retail_hdr_level", size))
                .collect(),
            adapted: [
                target(&device, "retail_hdr_adapted_a", pair),
                target(&device, "retail_hdr_adapted_b", pair),
            ],
            bloom_swap: target(&device, "retail_hdr_blur_swap", bloom),
            bloom: target(&device, "retail_hdr_bloom", bloom),
            state: AtomicU32::new(0),
        });
    }
}

fn prepare_retail_hdr_pipelines(
    mut commands: Commands,
    tonemap: Res<TonemapPipeline>,
    saturate: Res<SaturatePipeline>,
    pipeline_cache: Res<PipelineCache>,
    mut tonemaps: ResMut<SpecializedRenderPipelines<TonemapPipeline>>,
    mut saturates: ResMut<SpecializedRenderPipelines<SaturatePipeline>>,
    views: Query<(Entity, &ExtractedView, &Msaa), With<RetailHdr>>,
) {
    for (entity, view, msaa) in &views {
        commands.entity(entity).insert(RetailHdrPipelineIds {
            tonemap: tonemaps.specialize(&pipeline_cache, &tonemap, view.target_format),
            saturate: saturates.specialize(
                &pipeline_cache,
                &saturate,
                SaturateKey {
                    format: view.target_format,
                    samples: msaa.samples(),
                },
            ),
        });
    }
}

/// A view of the chain that also draws the fog composite.
type CompositeView = (With<RetailHdr>, With<RetailFogPass>);

/// Clips the fog composite's output to `[0, 1]`, as retail's composite does (D3). The draw is one
/// Min-blend of 1.0 and runs only on a view that draws the composite ([`RetailFogPass`]). Retail
/// saturates its composite's output only, and later draws add above 1 (NEEDS 27). Interiors and
/// underwater views have no composite in Mudcrab, so nothing is clipped there.
fn run_retail_saturate(
    view: ViewQuery<(&ViewTarget, &RetailHdrPipelineIds), CompositeView>,
    pipeline_cache: Res<PipelineCache>,
    mut ctx: RenderContext,
) {
    let (target, ids) = view.into_inner();
    let Some(pipeline) = pipeline_cache.get_render_pipeline(ids.saturate) else {
        return;
    };
    let mut attachment = target.get_color_attachment();
    attachment.ops = Operations {
        load: LoadOp::Load,
        ..attachment.ops
    };
    let descriptor = RenderPassDescriptor {
        label: Some("retail_hdr_saturate"),
        color_attachments: &[Some(attachment)],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };
    let mut pass = ctx.command_encoder().begin_render_pass(&descriptor);
    pass.set_pipeline(pipeline);
    pass.draw(0..3, 0..1);
}

/// What [`encode_chain`] needs beyond the targets: the pipelines in [`PassKind::ALL`] order, the
/// shared layout and samplers, and the uniform with its dynamic offset.
pub(crate) struct ChainGpu<'a> {
    pub layout: &'a wgpu::BindGroupLayout,
    pub linear: &'a wgpu::Sampler,
    pub nearest: &'a wgpu::Sampler,
    pub pipelines: [&'a wgpu::RenderPipeline; PassKind::ALL.len()],
    pub uniform: wgpu::BindingResource<'a>,
    pub offsets: &'a [u32],
}

/// Records the passes of `plan`. `view` returns the texture view of a chain target.
pub(crate) fn encode_chain<'a>(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    gpu: &ChainGpu<'_>,
    plan: &[ChainPass],
    view: impl Fn(ChainTarget) -> &'a wgpu::TextureView,
) {
    for step in plan {
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(step.kind.entry_point()),
            layout: gpu.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: gpu.uniform.clone(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view(step.source)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(view(step.previous)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(gpu.linear),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(gpu.nearest),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(step.kind.entry_point()),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: view(step.target),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(gpu.pipelines[step.kind.slot()]);
        pass.set_bind_group(0, &group, gpu.offsets);
        pass.draw(0..3, 0..1);
    }
}

/// The chain, then the tonemap, of one view.
fn run_retail_hdr(
    view: ViewQuery<(
        &ViewTarget,
        &DynamicUniformIndex<RetailHdr>,
        &RetailHdrTargets,
        &RetailHdrPipelineIds,
    )>,
    pipelines: Option<Res<RetailHdrPipelines>>,
    uniforms: Res<ComponentUniforms<RetailHdr>>,
    pipeline_cache: Res<PipelineCache>,
    mut ctx: RenderContext,
) {
    let (target_view, index, targets, ids) = view.into_inner();
    let Some(pipelines) = pipelines else {
        return;
    };
    let chain: Vec<_> = pipelines
        .chain
        .iter()
        .filter_map(|id| pipeline_cache.get_render_pipeline(*id))
        .map(|pipeline| &**pipeline)
        .collect();
    let (Ok(chain), Some(tonemap), Some(settings)) = (
        <[&wgpu::RenderPipeline; PassKind::ALL.len()]>::try_from(chain),
        pipeline_cache.get_render_pipeline(ids.tonemap),
        uniforms.uniforms().binding(),
    ) else {
        // A pipeline is still compiling: the frame shows the untoned scene and the pair stays
        // unset.
        return;
    };
    let state = targets.state.load(Ordering::Relaxed);
    let current = usize::from(state == 2);
    let plan = chain_plan(targets.levels.len() + 1, state != 0, current);
    let write = target_view.post_process_write();
    let offsets = [index.index()];
    let device = ctx.render_device().clone();
    let gpu = ChainGpu {
        layout: &pipeline_cache.get_bind_group_layout(&pipelines.chain_layout),
        linear: &pipelines.linear,
        nearest: &pipelines.nearest,
        pipelines: chain,
        uniform: settings.clone(),
        offsets: &offsets,
    };
    encode_chain(
        device.wgpu_device(),
        ctx.command_encoder(),
        &gpu,
        &plan,
        |id| match id {
            ChainTarget::Scene => &**write.source,
            ChainTarget::Level(level) => &*targets.levels[level].view,
            ChainTarget::Adapted(pair) => &*targets.adapted[pair].view,
            ChainTarget::BloomSwap => &*targets.bloom_swap.view,
            ChainTarget::Bloom => &*targets.bloom.view,
        },
    );
    let next = 1 - current;
    targets
        .state
        .store(if next == 1 { 2 } else { 1 }, Ordering::Relaxed);
    let tonemap_group = device.create_bind_group(
        "retail_hdr_tonemap",
        &pipeline_cache.get_bind_group_layout(&tonemap_layout()),
        &BindGroupEntries::sequential((
            settings,
            write.source,
            &targets.bloom.view,
            &targets.adapted[next].view,
            &pipelines.linear,
        )),
    );
    let descriptor = RenderPassDescriptor {
        label: Some("retail_hdr_tonemap"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: write.destination,
            depth_slice: None,
            resolve_target: None,
            ops: Operations::default(),
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };
    let mut pass = ctx.command_encoder().begin_render_pass(&descriptor);
    pass.set_pipeline(tonemap);
    pass.set_bind_group(0, &tonemap_group, &offsets);
    pass.draw(0..3, 0..1);
}
