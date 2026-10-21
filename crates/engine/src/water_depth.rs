//! The depth of the opaque bed behind the water surface.
//!
//! Water is drawn in the transparent pass, which attaches the same depth texture the opaque pass
//! writes, so the water shader cannot sample that texture itself. This module snapshots the opaque
//! depth into a plain `R32Float` image between the two passes and hands the image to every water
//! material. It replaces the depth prepass the water shader used to read: the snapshot costs one
//! full-screen pass instead of a second geometry pass, and it survives a profile that removes
//! `DepthPrepass` from the camera.

use crate::{
    config::{EngineConfig, WaterDepthSource},
    render::{WATER_LAYER, WaterMaterial},
};
use bevy::{
    asset::{embedded_asset, load_embedded_asset},
    camera::visibility::RenderLayers,
    core_pipeline::prepass::DepthPrepass,
    core_pipeline::{
        FullscreenShader,
        core_3d::{main_opaque_pass_3d, main_transparent_pass_3d},
        schedule::{Core3d, Core3dSystems},
    },
    image::Image,
    prelude::*,
    render::{
        Render, RenderApp, RenderStartup, RenderSystems,
        extract_component::{ExtractComponent, ExtractComponentPlugin},
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_asset::RenderAssets,
        render_resource::{
            BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
            CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, LoadOp,
            MultisampleState, Operations, PipelineCache, PrimitiveState, RenderPassColorAttachment,
            RenderPassDescriptor, RenderPipelineDescriptor, ShaderStages,
            SpecializedRenderPipeline, SpecializedRenderPipelines, StoreOp, TextureFormat,
            TextureSampleType, TextureUsages, TextureViewId,
            binding_types::{texture_2d, texture_2d_multisampled},
        },
        renderer::{RenderContext, ViewQuery},
        texture::GpuImage,
        view::ViewDepthTexture,
    },
    shader::{Shader, ShaderDefVal},
};

/// The single-sample red-channel image the water shader reads its bed depth from.
#[derive(Resource, Clone, ExtractResource)]
pub(crate) struct WaterDepthSnapshot {
    handle: Option<Handle<Image>>,
    size: UVec2,
    /// Samples in the depth texture the snapshot was taken from. The resolve pass and its pipeline
    /// are built for this count, which follows the camera's `Msaa`.
    samples: u32,
}

impl Default for WaterDepthSnapshot {
    fn default() -> Self {
        Self {
            handle: None,
            size: UVec2::ZERO,
            samples: 1,
        }
    }
}

/// Marks the camera that draws water. The snapshot is one image for one view; the reflection camera
/// never draws water and carries no marker.
#[derive(Component, Clone, ExtractComponent)]
pub(crate) struct WaterDepthView;

pub(crate) struct WaterDepthPlugin;

impl Plugin for WaterDepthPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/water_depth.wgsl");
        app.init_resource::<WaterDepthSnapshot>()
            .add_plugins((
                ExtractResourcePlugin::<WaterDepthSnapshot>::default(),
                ExtractComponentPlugin::<WaterDepthView>::default(),
            ))
            .add_systems(
                PostUpdate,
                (mark_water_depth_views, prepare_water_depth_snapshot).chain(),
            )
            .add_systems(Last, sync_water_depth_materials);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<SpecializedRenderPipelines<WaterDepthPipeline>>()
            .add_systems(RenderStartup, init_water_depth_pipeline)
            .add_systems(
                Render,
                prepare_water_depth_pipelines.in_set(RenderSystems::Prepare),
            )
            .add_systems(
                Core3d,
                run_water_depth_snapshot
                    .in_set(Core3dSystems::MainPass)
                    .after(main_opaque_pass_3d)
                    .before(main_transparent_pass_3d),
            );
    }
}

/// Marks every camera whose render layers include water, and makes its depth texture bindable so
/// the snapshot pass can read it. A camera that stops drawing water loses the marker.
#[allow(clippy::type_complexity)]
fn mark_water_depth_views(
    mut commands: Commands,
    mut cameras: Query<(
        Entity,
        &mut Camera3d,
        Option<&RenderLayers>,
        Option<&WaterDepthView>,
        Has<DepthPrepass>,
    )>,
) {
    let water = RenderLayers::layer(WATER_LAYER);
    for (entity, mut camera, layers, marker, has_prepass) in &mut cameras {
        let draws_water = layers.is_some_and(|layers| layers.intersects(&water));
        if draws_water {
            if marker.is_none() {
                commands.entity(entity).insert(WaterDepthView);
                info!(
                    "water bed depth comes from the opaque pass (depth prepass present: {has_prepass})"
                );
            }
            if !TextureUsages::from(camera.depth_texture_usages)
                .contains(TextureUsages::TEXTURE_BINDING)
            {
                camera.depth_texture_usages.0 |= TextureUsages::TEXTURE_BINDING.bits();
            }
        } else if marker.is_some() {
            commands.entity(entity).remove::<WaterDepthView>();
        }
    }
}

/// Sizes the snapshot to the water view's target, and to the view's `Msaa` sample count. A new image
/// handle replaces the old one, so the materials' bind groups are rebuilt against the new texture.
fn prepare_water_depth_snapshot(
    mut snapshot: ResMut<WaterDepthSnapshot>,
    mut images: ResMut<Assets<Image>>,
    config: Option<Res<EngineConfig>>,
    cameras: Query<(Entity, &Camera, &Msaa), With<WaterDepthView>>,
    mut reported_no_view: Local<bool>,
    mut reported_extra_view: Local<bool>,
) {
    let mode = config
        .as_deref()
        .map(|config| config.water_depth)
        .unwrap_or_default();
    let mut marked = cameras.iter();
    let Some((_, camera, msaa)) = marked.next() else {
        if snapshot.handle.is_some() {
            snapshot.handle = None;
        }
        if !*reported_no_view && mode == WaterDepthSource::Snapshot {
            debug!("water bed depth has no camera drawing water; water uses its ANAM opacity");
            *reported_no_view = true;
        }
        return;
    };
    *reported_no_view = false;
    // One image serves one view. A second camera drawing water would overwrite this view's depth,
    // so say so instead of picking one silently.
    if !*reported_extra_view && let Some((other, _, _)) = marked.next() {
        error!(
            "a second camera ({other}) draws water; the bed-depth snapshot serves one view only"
        );
        *reported_extra_view = true;
    }
    let Some(size) = camera.physical_target_size() else {
        return;
    };
    let samples = msaa.samples();
    if mode == WaterDepthSource::Constant {
        if snapshot.handle.is_some() || snapshot.size != size || snapshot.samples != samples {
            snapshot.handle = None;
            snapshot.size = size;
            snapshot.samples = samples;
        }
        return;
    }
    if snapshot.handle.is_some() && snapshot.size == size && snapshot.samples == samples {
        return;
    }
    let mut image = Image::new_target_texture(size.x, size.y, TextureFormat::R32Float, None);
    image.data = None;
    image.copy_on_resize = false;
    snapshot.handle = Some(images.add(image));
    snapshot.size = size;
    snapshot.samples = samples;
}

/// Points every water material at the current snapshot. Only a material whose handle or depth flag
/// differs is written, so a steady frame uploads nothing.
fn sync_water_depth_materials(
    snapshot: Res<WaterDepthSnapshot>,
    config: Option<Res<EngineConfig>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    mut reported_missing: Local<bool>,
    mut reported_snapshot: Local<bool>,
) {
    let wants_snapshot = snapshot.handle.is_some();
    if !wants_snapshot
        && !*reported_missing
        && config
            .as_deref()
            .map(|config| config.water_depth)
            .unwrap_or_default()
            == WaterDepthSource::Snapshot
        && materials.iter().next().is_some()
    {
        warn!(
            "water has no bed-depth snapshot; drawing its deep colour at its ANAM opacity. \
             This is a fallback, not the intended configuration."
        );
        *reported_missing = true;
    }
    let ids: Vec<_> = materials.ids().collect();
    for id in ids {
        let Some(current) = materials.get(id) else {
            continue;
        };
        if current.extension.bed_depth() == snapshot.handle.as_ref() {
            continue;
        }
        if let Some(mut material) = materials.get_mut(id) {
            material.extension.set_bed_depth(snapshot.handle.clone());
            if wants_snapshot && !*reported_snapshot {
                info!(
                    "water bed depth: {}x{} snapshot, sample count {}",
                    snapshot.size.x, snapshot.size.y, snapshot.samples
                );
                *reported_snapshot = true;
            }
        }
    }
}

#[derive(Resource)]
struct WaterDepthPipeline {
    shader: Handle<Shader>,
    fullscreen_shader: FullscreenShader,
}

fn init_water_depth_pipeline(
    mut commands: Commands,
    fullscreen_shader: Res<FullscreenShader>,
    asset_server: Res<AssetServer>,
) {
    commands.insert_resource(WaterDepthPipeline {
        shader: load_embedded_asset!(asset_server.as_ref(), "shaders/water_depth.wgsl"),
        fullscreen_shader: fullscreen_shader.clone(),
    });
}

#[derive(Component, Clone, Copy)]
struct WaterDepthPipelineId(CachedRenderPipelineId);

impl SpecializedRenderPipeline for WaterDepthPipeline {
    type Key = u32;

    fn specialize(&self, samples: u32) -> RenderPipelineDescriptor {
        let multisampled = samples > 1;
        let mut shader_defs = vec![ShaderDefVal::UInt("BED_DEPTH_SAMPLES".into(), samples)];
        if multisampled {
            shader_defs.push("MULTISAMPLED".into());
        }
        RenderPipelineDescriptor {
            label: Some("water_depth_snapshot".into()),
            layout: vec![snapshot_layout(samples)],
            vertex: self.fullscreen_shader.to_vertex_state(),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                shader_defs,
                entry_point: Some("fragment".into()),
                targets: vec![Some(ColorTargetState {
                    format: TextureFormat::R32Float,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            ..default()
        }
    }
}

fn snapshot_layout(samples: u32) -> BindGroupLayoutDescriptor {
    let texture = if samples > 1 {
        texture_2d_multisampled(TextureSampleType::Depth)
    } else {
        texture_2d(TextureSampleType::Depth)
    };
    BindGroupLayoutDescriptor::new(
        "water_depth_snapshot_layout",
        &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (texture,)),
    )
}

fn prepare_water_depth_pipelines(
    mut commands: Commands,
    pipeline: Res<WaterDepthPipeline>,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<WaterDepthPipeline>>,
    snapshot: Res<WaterDepthSnapshot>,
    views: Query<Entity, With<WaterDepthView>>,
) {
    for entity in &views {
        let id = pipelines.specialize(&pipeline_cache, &pipeline, snapshot.samples);
        commands.entity(entity).insert(WaterDepthPipelineId(id));
    }
}

#[derive(Default)]
struct SnapshotLog {
    resolved: bool,
    size_mismatch: bool,
    sample_mismatch: bool,
}

/// Copies the opaque pass's depth into the snapshot before the transparent pass attaches it.
fn run_water_depth_snapshot(
    view: ViewQuery<(
        &ViewDepthTexture,
        Option<&WaterDepthPipelineId>,
        Has<WaterDepthView>,
    )>,
    snapshot: Res<WaterDepthSnapshot>,
    pipeline_cache: Res<PipelineCache>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
    mut cached: Local<Option<(TextureViewId, BindGroup)>>,
    mut log: Local<SnapshotLog>,
) {
    let (depth, pipeline_id, marked) = view.into_inner();
    let Some(pipeline_id) = pipeline_id else {
        return;
    };
    if !marked {
        return;
    }
    let Some(target) = snapshot
        .handle
        .as_ref()
        .and_then(|handle| images.get(handle))
    else {
        return;
    };
    if target.texture.width() != depth.texture.width()
        || target.texture.height() != depth.texture.height()
    {
        if !log.size_mismatch {
            warn!(
                "water bed depth snapshot {}x{} does not match the view depth {}x{}; water reads no \
                 bed depth until they match",
                target.texture.width(),
                target.texture.height(),
                depth.texture.width(),
                depth.texture.height()
            );
            log.size_mismatch = true;
        }
        return;
    }
    let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_id.0) else {
        return;
    };

    let samples = snapshot.samples;
    if depth.texture.sample_count() != samples {
        if !log.sample_mismatch {
            warn!(
                "water bed depth snapshot was built for {samples} samples but the view depth has \
                 {}; water reads no bed depth until they match",
                depth.texture.sample_count()
            );
            log.sample_mismatch = true;
        }
        return;
    }
    let source = depth.view();
    let bind_group = match &mut *cached {
        Some((id, bind_group)) if source.id() == *id => bind_group,
        cached => {
            let bind_group = ctx.render_device().create_bind_group(
                None,
                &pipeline_cache.get_bind_group_layout(&snapshot_layout(samples)),
                &BindGroupEntries::single(source),
            );
            &cached.insert((source.id(), bind_group)).1
        }
    };

    let pass_descriptor = RenderPassDescriptor {
        label: Some("water_depth_snapshot"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: &target.texture_view,
            depth_slice: None,
            resolve_target: None,
            ops: Operations {
                load: LoadOp::Clear(Default::default()),
                store: StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };

    let mut render_pass = ctx.command_encoder().begin_render_pass(&pass_descriptor);
    render_pass.set_pipeline(pipeline);
    render_pass.set_bind_group(0, bind_group, &[]);
    render_pass.draw(0..3, 0..1);
    drop(render_pass);
    if !log.resolved {
        debug!(
            "water bed depth snapshot resolved {}x{}, sample count {}",
            target.texture.width(),
            target.texture.height(),
            samples
        );
        log.resolved = true;
    }
}
