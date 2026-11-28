//! Opaque-scene fog and saturation for the Skyrim visual profile.
//!
//! SPEC-2 section 3 defines the composite. It reads a packed, unsigned scene copy and writes
//! saturated RGB before water and other blended draws. Fog records 51 and 53 apply the fog ramp
//! and output scale. Record 49 applies neither.
//!
//! STAND-IN(#995): use a neutral occlusion factor until there is a screen-space AO producer.
//! STAND-IN(#362): unsupported adapters and multisampled views use the blend path. It raises
//! negative input to zero and clips the output with a Min blend before transparent geometry.
//! STAND-IN(#362): use colour-stage depth for samples the depth prepass did not cover.
//!
//! The pass runs after the opaque phase and before transmissive and transparent geometry.

use crate::sky::FogCamera;
use bevy::{
    asset::{embedded_asset, load_embedded_asset},
    core_pipeline::{
        FullscreenShader,
        core_3d::{CORE_3D_DEPTH_FORMAT, main_opaque_pass_3d, main_transparent_pass_3d},
        prepass::ViewPrepassTextures,
        schedule::{Core3d, Core3dSystems},
    },
    ecs::query::QueryItem,
    pbr::main_transmissive_pass_3d,
    prelude::*,
    render::{
        Render, RenderApp, RenderStartup, RenderSystems,
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        render_resource::{
            BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent,
            BlendFactor, BlendOperation, BlendState, CachedRenderPipelineId, ColorTargetState,
            ColorWrites, Extent3d, FragmentState, LoadOp, MultisampleState, Operations,
            PipelineCache, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor,
            RenderPipelineDescriptor, ShaderStages, ShaderType, SpecializedRenderPipeline,
            SpecializedRenderPipelines, TextureDescriptor, TextureDimension, TextureFormat,
            TextureSampleType, TextureUsages,
            binding_types::{
                texture_2d, texture_depth_2d, texture_depth_2d_multisampled, uniform_buffer,
            },
        },
        renderer::{RenderContext, RenderDevice, ViewQuery},
        sync_component::SyncComponent,
        texture::{CachedTexture, TextureCache},
        view::{
            ExtractedView, Msaa, ViewDepthTexture, ViewTarget, ViewUniform, ViewUniformOffset,
            ViewUniforms,
        },
    },
    shader::{Shader, ShaderDefVal},
};

/// One RGB channel of the retail fog composite, without the outer clip: `k * ((1 - fc) * a * l + fc * cfc)`.
///
/// This is what the pass draws (the clip is a separate draw, see the module notes). `l` is the opaque
/// colour, `a` the SAO factor (1 in Mudcrab), `fc` the fog factor, `cfc` the fog colour and `k`
/// the output scale `1 / FramebufferRange`.
pub fn composite(l: f32, a: f32, fc: f32, cfc: f32, k: f32) -> f32 {
    k * ((1.0 - fc) * a * l + fc * cfc)
}

/// Retail's form of [`composite`]: `saturate(k * ((1 - fc) * a * l + fc * cfc))`.
pub fn composite_saturated(l: f32, a: f32, fc: f32, cfc: f32, k: f32) -> f32 {
    composite(l, a, fc, cfc, k).clamp(0.0, 1.0)
}

/// The fog factor at linear view depth `z`: `Fc = min(saturate(z * r - near * r) ^ p, max)` with
/// `r = 1 / (far - near)`. A power ramp capped by `max`, linear when `power` is 1. Zero when the ramp
/// is empty (`far <= near`), so no fog is drawn and nothing is invented.
pub fn fog_factor(z: f32, near: f32, far: f32, power: f32, max: f32) -> f32 {
    // NaN fails both comparisons, so a NaN range draws nothing too.
    if far.partial_cmp(&near) != Some(std::cmp::Ordering::Greater) {
        return 0.0;
    }
    let r = 1.0 / (far - near);
    (z * r - near * r).clamp(0.0, 1.0).powf(power).min(max)
}

/// The fog colour at factor `fc`: `Cfc = nearRGB + Fc * (farRGB - nearRGB)`.
pub fn fog_colour(near_rgb: [f32; 3], far_rgb: [f32; 3], fc: f32) -> [f32; 3] {
    std::array::from_fn(|channel| near_rgb[channel] + fc * (far_rgb[channel] - near_rgb[channel]))
}

/// The weather's fog fields for one frame, after the weather blend and the player's density scale,
/// and the two fog colours (linear RGB, the sky brightness scale applied).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetailFogParams {
    pub near: f32,
    pub far: f32,
    pub power: f32,
    pub max: f32,
    pub near_rgb: [f32; 3],
    pub far_rgb: [f32; 3],
}

impl RetailFogParams {
    /// Whether the ramp draws anything. The same test as the Bevy fog's: an ordered, finite range,
    /// a positive max and a positive finite power. Anything else draws no fog and no `k` scale.
    pub fn is_drawn(&self) -> bool {
        self.near.is_finite()
            && self.far.is_finite()
            && self.far > self.near
            && self.max > 0.0
            && self.power.is_finite()
            && self.power > 0.0
    }

    /// Retail's fallback for a ramp that draws nothing: `(near * r, r, p, m) = (5000000, 0.1, 1, 0)`,
    /// whose max of 0 gives `Fc = 0` at every depth. The pass keeps the colours and uses an equal
    /// ramp that is finite for the shader: `Fc` is 0 everywhere, and `k` still applies.
    pub fn fallback(self) -> Self {
        Self {
            near: 0.0,
            far: 1.0,
            power: 1.0,
            max: 0.0,
            ..self
        }
    }

    /// The fog factor at linear view depth `z`.
    pub fn factor(&self, z: f32) -> f32 {
        fog_factor(z, self.near, self.far, self.power, self.max.clamp(0.0, 1.0))
    }

    /// The composited colour of an opaque pixel `l` (RGB) at linear view depth `z`, as the pass
    /// draws it (no outer clip, `A = 1`). A pixel with no geometry (`z` not finite) is unchanged.
    pub fn composite_pixel(&self, l: [f32; 3], z: f32, k: f32) -> [f32; 3] {
        if !z.is_finite() {
            return l;
        }
        // A ramp that draws nothing takes the same fallback as the pass (`RetailFogPass::new`).
        let ramp = if self.is_drawn() {
            *self
        } else {
            self.fallback()
        };
        let fc = ramp.factor(z);
        let cfc = fog_colour(self.near_rgb, self.far_rgb, fc);
        std::array::from_fn(|channel| composite(l[channel], 1.0, fc, cfc[channel], k))
    }
}

/// The pass settings of one camera, written by `sky::apply_fog` while the camera is outside, fog is
/// on, the camera is above water and the profile is `skyrim`. The component exists only then, so
/// the pass costs nothing otherwise.
#[derive(Component, Clone, Copy, Debug, PartialEq, ShaderType)]
pub struct RetailFogPass {
    /// x = near, y = far, z = power, w = max.
    range: Vec4,
    /// rgb = the near fog colour (linear), w = `k`, the output scale `1 / FramebufferRange`.
    near_colour: Vec4,
    /// rgb = the far fog colour (linear), w = the SAO factor `A`.
    far_colour: Vec4,
    /// x = projection near, y = projection far. Zero for other projection types.
    camera_range: Vec2,
}

impl SyncComponent for RetailFogPass {
    type Target = Self;
}

impl ExtractComponent for RetailFogPass {
    type QueryData = (&'static Self, Option<&'static Projection>);
    type QueryFilter = ();
    type Out = Self;

    fn extract_component((pass, projection): QueryItem<'_, '_, Self::QueryData>) -> Option<Self> {
        let mut pass = *pass;
        if let Some(Projection::Perspective(camera)) = projection
            && camera.near.is_finite()
            && camera.far.is_finite()
            && camera.near > 0.0
            && camera.far > camera.near
        {
            pass.camera_range = Vec2::new(camera.near, camera.far);
        }
        Some(pass)
    }
}

impl RetailFogPass {
    /// `A` while Mudcrab has no SAO pass. STAND-IN(#995): factor one replaces the retail SAO texture.
    pub const SAO_FACTOR: f32 = 1.0;

    /// Retail's no-fog composite (record 49): the clip alone, with no fog and no `k`
    /// (SPEC-1 section 8.3). With `Fc = 0` and a `k` of 1 the blend leaves the colour as it is,
    /// and the saturate draw after it clips.
    pub fn without_fog() -> Self {
        Self {
            range: Vec4::new(0.0, 1.0, 1.0, 0.0),
            near_colour: Vec3::ZERO.extend(1.0),
            far_colour: Vec3::ZERO.extend(Self::SAO_FACTOR),
            camera_range: Vec2::ZERO,
        }
    }

    /// The pass for `params`. A ramp that draws nothing (`far <= near`, a max that is not positive,
    /// or a power that is not positive and finite) takes retail's fallback: its composite still
    /// runs with `Fc = 0` and still applies `k`. The weather record must have published its fog
    /// fields to reach here at all; a record that omitted one makes no pass (`sky.rs`).
    pub fn new(params: RetailFogParams) -> Self {
        let params = if params.is_drawn() {
            params
        } else {
            params.fallback()
        };
        Self {
            range: Vec4::new(
                params.near,
                params.far,
                params.power,
                params.max.clamp(0.0, 1.0),
            ),
            near_colour: Vec3::from_array(params.near_rgb)
                .extend(crate::output_scale::framebuffer_output_scale(true)),
            far_colour: Vec3::from_array(params.far_rgb).extend(Self::SAO_FACTOR),
            camera_range: Vec2::ZERO,
        }
    }

    pub fn near(&self) -> f32 {
        self.range.x
    }

    pub fn far(&self) -> f32 {
        self.range.y
    }

    pub fn power(&self) -> f32 {
        self.range.z
    }

    pub fn max(&self) -> f32 {
        self.range.w
    }

    pub fn near_rgb(&self) -> [f32; 3] {
        self.near_colour.truncate().to_array()
    }

    pub fn far_rgb(&self) -> [f32; 3] {
        self.far_colour.truncate().to_array()
    }

    /// The output scale `k`.
    pub fn output_scale(&self) -> f32 {
        self.near_colour.w
    }
}

pub struct RetailFogPlugin;

impl Plugin for RetailFogPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/retail_fog.wgsl");
        app.add_plugins((
            ExtractComponentPlugin::<RetailFogPass>::default(),
            UniformComponentPlugin::<RetailFogPass>::default(),
        ))
        .add_systems(PostUpdate, bind_depth_for_retail_fog);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<SpecializedRenderPipelines<RetailFogPipeline>>()
            .add_systems(RenderStartup, init_retail_fog_pipeline)
            .add_systems(
                Render,
                (
                    clear_inactive_retail_fog_views.in_set(RenderSystems::PrepareResources),
                    prepare_retail_fog_pipelines.in_set(RenderSystems::Prepare),
                ),
            )
            .add_systems(
                Core3d,
                run_retail_fog_pass
                    .in_set(Core3dSystems::MainPass)
                    .after(main_opaque_pass_3d)
                    // The blended stages fog themselves, and the transmissive pass copies the
                    // fogged opaque colour for water to refract.
                    .before(main_transmissive_pass_3d)
                    .before(main_transparent_pass_3d),
            );
    }
}

/// Makes the depth texture of every camera that draws the pass bindable, so the pass can read it.
fn bind_depth_for_retail_fog(mut cameras: Query<&mut Camera3d, With<RetailFogPass>>) {
    for mut camera in &mut cameras {
        if !TextureUsages::from(camera.depth_texture_usages)
            .contains(TextureUsages::TEXTURE_BINDING)
        {
            camera.depth_texture_usages.0 |= TextureUsages::TEXTURE_BINDING.bits();
        }
    }
}

#[derive(Resource)]
struct RetailFogPipeline {
    shader: Handle<Shader>,
    fullscreen_shader: FullscreenShader,
}

fn init_retail_fog_pipeline(
    mut commands: Commands,
    fullscreen_shader: Res<FullscreenShader>,
    asset_server: Res<AssetServer>,
) {
    commands.insert_resource(RetailFogPipeline {
        shader: load_embedded_asset!(asset_server.as_ref(), "shaders/retail_fog.wgsl"),
        fullscreen_shader: fullscreen_shader.clone(),
    });
}

fn retail_fog_layout(samples: u32) -> BindGroupLayoutDescriptor {
    let depth = || {
        if samples > 1 {
            texture_depth_2d_multisampled()
        } else {
            texture_depth_2d()
        }
    };
    BindGroupLayoutDescriptor::new(
        "retail_fog_layout",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::FRAGMENT,
            (
                (0, uniform_buffer::<RetailFogPass>(true)),
                (1, uniform_buffer::<ViewUniform>(true)),
                (2, depth()),
                (4, depth()),
            ),
        ),
    )
}

fn scene_copy_layout() -> BindGroupLayoutDescriptor {
    BindGroupLayoutDescriptor::new(
        "retail_fog_scene_copy_layout",
        &BindGroupLayoutEntries::with_indices(
            ShaderStages::FRAGMENT,
            ((
                3,
                texture_2d(TextureSampleType::Float { filterable: false }),
            ),),
        ),
    )
}

fn packed_fog_layout() -> BindGroupLayoutDescriptor {
    BindGroupLayoutDescriptor::new(
        "retail_fog_packed_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                uniform_buffer::<RetailFogPass>(true),
                uniform_buffer::<ViewUniform>(true),
                texture_depth_2d(),
                texture_2d(TextureSampleType::Float { filterable: false }),
                texture_depth_2d(),
            ),
        ),
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum CompositeDraw {
    ClampInput,
    Blend,
    Copy,
    Replace,
}

/// The attachment format, sample count and draw a pipeline is built for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct RetailFogKey {
    format: TextureFormat,
    samples: u32,
    draw: CompositeDraw,
}

/// `max(dst, 0)` on the colour channels: retail's composite reads an unsigned copy of the scene
/// (SPEC-2 section 3, [near-lighting.md](../../../docs/specs/engine/near-lighting.md)).
pub(crate) const UNSIGNED_INPUT_BLEND: BlendState = BlendState {
    color: BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Max,
    },
    alpha: BlendComponent {
        src_factor: BlendFactor::Zero,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
};

/// The blend that makes the pass `src.rgb + dst.rgb * src.a`. The alpha channel is left as it is,
/// so the water reflection's "no geometry" alpha survives.
pub(crate) const COMPOSITE_BLEND: BlendState = BlendState {
    color: BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::SrcAlpha,
        operation: BlendOperation::Add,
    },
    alpha: BlendComponent {
        src_factor: BlendFactor::Zero,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
};

impl SpecializedRenderPipeline for RetailFogPipeline {
    type Key = RetailFogKey;

    fn specialize(&self, key: RetailFogKey) -> RenderPipelineDescriptor {
        let mut shader_defs = vec![ShaderDefVal::UInt("DEPTH_SAMPLES".into(), key.samples)];
        if key.samples > 1 {
            shader_defs.push("MULTISAMPLED".into());
        }
        let (entry_point, blend, layout, write_mask) = match key.draw {
            CompositeDraw::ClampInput => (
                "unsigned_input",
                Some(UNSIGNED_INPUT_BLEND),
                retail_fog_layout(key.samples),
                ColorWrites::ALL,
            ),
            CompositeDraw::Blend => (
                "fragment",
                Some(COMPOSITE_BLEND),
                retail_fog_layout(key.samples),
                ColorWrites::ALL,
            ),
            CompositeDraw::Copy => ("copy_scene", None, scene_copy_layout(), ColorWrites::COLOR),
            // Water uses Bevy's alpha for geometry coverage.
            CompositeDraw::Replace => (
                "composite_scene",
                None,
                packed_fog_layout(),
                ColorWrites::COLOR,
            ),
        };
        RenderPipelineDescriptor {
            label: Some("retail_fog_composite".into()),
            layout: vec![layout],
            vertex: self.fullscreen_shader.to_vertex_state(),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            // The fragment reads `@builtin(sample_index)`, so it runs once per sample and each
            // sample reads its own depth.
            multisample: MultisampleState {
                count: key.samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                shader_defs,
                entry_point: Some(entry_point.into()),
                targets: vec![Some(ColorTargetState {
                    format: key.format,
                    blend,
                    write_mask,
                })],
            }),
            ..default()
        }
    }
}

#[derive(Component, Clone, Copy)]
pub(crate) struct RetailFogPipelineId {
    id: CachedRenderPipelineId,
    unsigned_input: CachedRenderPipelineId,
    samples: u32,
}

#[derive(Component)]
pub(crate) struct PackedComposite {
    scene: CachedTexture,
    copy: CachedRenderPipelineId,
    replace: CachedRenderPipelineId,
}

/// Views that still hold pass state after the extracted settings left them.
type InactiveRetailFogView = (
    Without<RetailFogPass>,
    Or<(
        With<RetailFogPipelineId>,
        With<DynamicUniformIndex<RetailFogPass>>,
    )>,
);

fn clear_inactive_retail_fog_views(
    mut commands: Commands,
    views: Query<Entity, InactiveRetailFogView>,
) {
    for entity in &views {
        commands.entity(entity).remove::<(
            RetailFogPipelineId,
            PackedComposite,
            DynamicUniformIndex<RetailFogPass>,
        )>();
    }
}

fn prepare_retail_fog_pipelines(
    mut commands: Commands,
    pipeline: Res<RetailFogPipeline>,
    pipeline_cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    mut textures: ResMut<TextureCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<RetailFogPipeline>>,
    views: Query<(Entity, &ExtractedView, &Msaa, &ViewTarget), With<RetailFogPass>>,
) {
    for (entity, view, msaa, target) in &views {
        let samples = msaa.samples();
        let mut specialize = |draw, format| {
            pipelines.specialize(
                &pipeline_cache,
                &pipeline,
                RetailFogKey {
                    format,
                    samples,
                    draw,
                },
            )
        };
        let main_format = view.target_format;
        let id = specialize(CompositeDraw::Blend, main_format);
        let unsigned_input = specialize(CompositeDraw::ClampInput, main_format);
        let mut entity = commands.entity(entity);
        entity.insert(RetailFogPipelineId {
            id,
            unsigned_input,
            samples,
        });
        // STAND-IN(#361): the input clamp stands in for target 113 on adapters without
        // renderable packed floats and on multisampled views.
        if samples == 1
            && device
                .features()
                .contains(wgpu::Features::RG11B10UFLOAT_RENDERABLE)
        {
            let format = TextureFormat::Rg11b10Ufloat;
            let main = target.main_texture();
            let scene = textures.get(
                &device,
                TextureDescriptor {
                    label: Some("retail_fog_scene_copy"),
                    size: Extent3d {
                        width: main.width(),
                        height: main.height(),
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format,
                    usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            );
            entity.insert(PackedComposite {
                scene,
                copy: specialize(CompositeDraw::Copy, format),
                replace: specialize(CompositeDraw::Replace, main_format),
            });
        } else {
            entity.remove::<PackedComposite>();
        }
    }
}

/// The components the draw reads from a view that carries [`RetailFogPass`].
type RetailFogDrawView<'w, 's> = ViewQuery<
    'w,
    's,
    (
        &'static ViewTarget,
        &'static ViewDepthTexture,
        Option<&'static ViewPrepassTextures>,
        &'static DynamicUniformIndex<RetailFogPass>,
        &'static ViewUniformOffset,
        &'static RetailFogPipelineId,
        Option<&'static PackedComposite>,
    ),
    With<RetailFogPass>,
>;

/// Draws the composite over the opaque colour of a view that carries [`RetailFogPass`].
///
/// A view whose pipeline is still compiling, or whose depth texture cannot be bound or has another
/// sample count, is left unfogged for that frame.
pub(crate) fn run_retail_fog_pass(
    view: RetailFogDrawView,
    uniforms: Res<ComponentUniforms<RetailFogPass>>,
    view_uniforms: Res<ViewUniforms>,
    pipeline_cache: Res<PipelineCache>,
    mut ctx: RenderContext,
) {
    let (target, depth, prepass, index, view_offset, id, packed) = view.into_inner();
    let (Some(pipeline), Some(unsigned_input)) = (
        pipeline_cache.get_render_pipeline(id.id),
        pipeline_cache.get_render_pipeline(id.unsigned_input),
    ) else {
        return;
    };
    if depth.texture.sample_count() != id.samples
        || !depth
            .texture
            .usage()
            .contains(TextureUsages::TEXTURE_BINDING)
        || depth.texture.format() != CORE_3D_DEPTH_FORMAT
    {
        return;
    }
    let depth_view = prepass
        .and_then(ViewPrepassTextures::depth_view)
        .unwrap_or_else(|| depth.view());
    let (Some(settings), Some(view_binding)) = (
        uniforms.uniforms().binding(),
        view_uniforms.uniforms.binding(),
    ) else {
        return;
    };
    let offsets = [index.index(), view_offset.offset];
    if let Some(packed) = packed
        && target
            .main_texture()
            .usage()
            .contains(TextureUsages::TEXTURE_BINDING)
        && let (Some(copy), Some(replace)) = (
            pipeline_cache.get_render_pipeline(packed.copy),
            pipeline_cache.get_render_pipeline(packed.replace),
        )
    {
        let device = ctx.render_device();
        let copy_group = device.create_bind_group(
            "retail_fog_scene_copy",
            &pipeline_cache.get_bind_group_layout(&scene_copy_layout()),
            &BindGroupEntries::with_indices(((3, target.main_texture_view()),)),
        );
        let replace_group = device.create_bind_group(
            "retail_fog_composite_packed",
            &pipeline_cache.get_bind_group_layout(&packed_fog_layout()),
            &BindGroupEntries::sequential((
                settings,
                view_binding,
                depth_view,
                &packed.scene.default_view,
                depth.view(),
            )),
        );
        let mut destination = target.get_color_attachment();
        destination.ops.load = LoadOp::Load;
        let scene = RenderPassColorAttachment {
            view: &packed.scene.default_view,
            depth_slice: None,
            resolve_target: None,
            ops: Operations::default(),
        };
        for (label, attachment, pipeline, group, offsets) in [
            ("retail_fog_scene_copy", scene, copy, &copy_group, &[][..]),
            (
                "retail_fog_composite_packed",
                destination,
                replace,
                &replace_group,
                &offsets[..],
            ),
        ] {
            let mut pass = ctx
                .command_encoder()
                .begin_render_pass(&RenderPassDescriptor {
                    label: Some(label),
                    color_attachments: &[Some(attachment)],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, offsets);
            pass.draw(0..3, 0..1);
        }
        return;
    }
    let bind_group = ctx.render_device().create_bind_group(
        "retail_fog_composite",
        &pipeline_cache.get_bind_group_layout(&retail_fog_layout(id.samples)),
        &BindGroupEntries::with_indices((
            (0, settings),
            (1, view_binding),
            (2, depth_view),
            (4, depth.view()),
        )),
    );
    let mut color_attachment = target.get_color_attachment();
    // The opaque pass has already cleared and drawn: keep what it left.
    color_attachment.ops = Operations {
        load: LoadOp::Load,
        ..color_attachment.ops
    };
    let descriptor = RenderPassDescriptor {
        label: Some("retail_fog_composite"),
        color_attachments: &[Some(color_attachment)],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };
    let mut pass = ctx.command_encoder().begin_render_pass(&descriptor);
    pass.set_bind_group(0, &bind_group, &offsets);
    // First the unsigned input, then the composite.
    pass.set_pipeline(unsigned_input);
    pass.draw(0..3, 0..1);
    pass.set_pipeline(pipeline);
    pass.draw(0..3, 0..1);
}

/// Marks the cameras that fog the exterior (every [`FogCamera`] outside) so the sky's `apply_fog`
/// and this module agree on which cameras take the pass. Kept as a type alias for the query that
/// `apply_fog` uses.
pub(crate) type FogCameraQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        Option<&'static bevy::pbr::DistanceFog>,
        Option<&'static RetailFogPass>,
    ),
    With<FogCamera>,
>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The one named constant for `k` (0.8333, from `output_scale.rs`).
    fn k() -> f32 {
        crate::output_scale::framebuffer_output_scale(true)
    }

    fn close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1.0e-4,
            "expected {expected}, got {actual}"
        );
    }

    /// The worked examples: scene colour 0.4, fog colour 0.6, `A = 1`, `k = 0.8333`.
    #[test]
    fn composite_matches_the_worked_examples() {
        close(composite(0.4, 1.0, 0.5, 0.6, k()), 0.416_65);
        close(composite(0.4, 1.0, 1.0, 0.6, k()), 0.5);
        close(composite(0.4, 1.0, 0.0, 0.6, k()), 0.333_32);
    }

    /// At `Fc = 0` the composite still scales an opaque pixel by `k` (the note for #796).
    #[test]
    fn composite_scales_by_k_with_no_fog() {
        for l in [0.0, 0.25, 0.9] {
            close(composite(l, 1.0, 0.0, 0.7, k()), k() * l);
        }
    }

    /// The SAO factor scales only the lit colour, never the fog colour.
    #[test]
    fn the_sao_factor_scales_the_lit_term_only() {
        close(composite(0.4, 0.5, 0.0, 0.6, k()), k() * 0.2);
        close(composite(0.4, 0.5, 1.0, 0.6, k()), k() * 0.6);
    }

    /// Retail clips to [0, 1]; the pass draws the same value without the clip.
    #[test]
    fn the_saturated_form_clips_and_otherwise_agrees() {
        close(composite_saturated(0.4, 1.0, 0.5, 0.6, k()), 0.416_65);
        assert_eq!(composite_saturated(3.0, 1.0, 0.0, 0.6, k()), 1.0);
        assert!(composite(3.0, 1.0, 0.0, 0.6, k()) > 1.0);
        assert_eq!(composite_saturated(-1.0, 1.0, 0.0, 0.6, k()), 0.0);
    }

    #[test]
    fn the_fog_factor_is_a_power_ramp_capped_by_max() {
        // Before near: 0. At far: capped at max. Between: s ^ power.
        assert_eq!(fog_factor(0.0, 100.0, 1100.0, 0.4, 0.85), 0.0);
        assert_eq!(fog_factor(100.0, 100.0, 1100.0, 0.4, 0.85), 0.0);
        close(fog_factor(600.0, 100.0, 1100.0, 1.0, 0.85), 0.5);
        close(fog_factor(600.0, 100.0, 1100.0, 0.5, 1.0), 0.5_f32.sqrt());
        assert_eq!(fog_factor(1100.0, 100.0, 1100.0, 0.4, 0.85), 0.85);
        assert_eq!(fog_factor(9.0e6, 100.0, 1100.0, 0.4, 0.85), 0.85);
        // An empty ramp draws nothing.
        assert_eq!(fog_factor(500.0, 800.0, 800.0, 0.4, 0.85), 0.0);
        assert_eq!(fog_factor(500.0, 900.0, 800.0, 0.4, 0.85), 0.0);
    }

    /// `SkyrimClear` by day: near 0, far 80,000, power 0.4, max 0.85 (sky.md#fog).
    fn clear_day() -> RetailFogParams {
        RetailFogParams {
            near: 0.0,
            far: 80_000.0,
            power: 0.4,
            max: 0.85,
            near_rgb: [0.1, 0.2, 0.3],
            far_rgb: [0.5, 0.6, 0.7],
        }
    }

    #[test]
    fn the_fog_colour_runs_from_near_to_far_by_the_factor() {
        let params = clear_day();
        assert_eq!(
            fog_colour(params.near_rgb, params.far_rgb, 0.0),
            params.near_rgb
        );
        assert_eq!(
            fog_colour(params.near_rgb, params.far_rgb, 1.0),
            params.far_rgb
        );
        let half = fog_colour(params.near_rgb, params.far_rgb, 0.5);
        close(half[0], 0.3);
        close(half[2], 0.5);
    }

    #[test]
    fn a_pixel_with_no_geometry_is_not_touched() {
        let params = clear_day();
        let sky = [0.2, 0.4, 0.8];
        assert_eq!(params.composite_pixel(sky, f32::INFINITY, k()), sky);
        assert_eq!(params.composite_pixel(sky, f32::NAN, k()), sky);
    }

    /// The whole chain on one pixel, against the formula by hand.
    #[test]
    fn a_pixel_at_the_cap_is_the_capped_fog_mix() {
        let params = clear_day();
        let l = [0.4, 0.4, 0.4];
        let out = params.composite_pixel(l, 80_000.0, k());
        let fc = 0.85;
        for (channel, value) in out.into_iter().enumerate() {
            let cfc = params.near_rgb[channel]
                + fc * (params.far_rgb[channel] - params.near_rgb[channel]);
            close(value, k() * ((1.0 - fc) * 0.4 + fc * cfc));
        }
    }

    /// A ramp that draws nothing still runs the composite: `Fc = 0` and `k` applies, as retail's
    /// fallback does for `far == near`.
    #[test]
    fn a_ramp_that_draws_nothing_keeps_k_and_no_fog() {
        assert!(clear_day().is_drawn());
        for broken in [
            RetailFogParams {
                far: 0.0,
                ..clear_day()
            },
            RetailFogParams {
                max: 0.0,
                ..clear_day()
            },
            RetailFogParams {
                power: 0.0,
                ..clear_day()
            },
            RetailFogParams {
                power: f32::NAN,
                ..clear_day()
            },
            RetailFogParams {
                near: f32::INFINITY,
                ..clear_day()
            },
        ] {
            assert!(!broken.is_drawn(), "{broken:?}");
            let pass = RetailFogPass::new(broken);
            assert_eq!(pass.max(), 0.0, "{broken:?}");
            assert_eq!(pass.output_scale(), k());
            let (near, far, power, max) = (pass.near(), pass.far(), pass.power(), pass.max());
            for z in [0.0, 1.0, 1.0e3, 8.0e4, 1.0e7] {
                assert_eq!(fog_factor(z, near, far, power, max), 0.0);
            }
            let out = broken.composite_pixel([0.4, 0.4, 0.4], 5.0e3, k());
            close(out[0], k() * 0.4);
        }
    }

    /// The uniform carries the weather's values, the output scale and the SAO stand-in.
    #[test]
    fn the_pass_carries_the_weather_fields() {
        let pass = RetailFogPass::new(clear_day());
        assert_eq!(pass.near(), 0.0);
        assert_eq!(pass.far(), 80_000.0);
        assert_eq!(pass.power(), 0.4);
        assert_eq!(pass.max(), 0.85);
        assert_eq!(pass.near_rgb(), [0.1, 0.2, 0.3]);
        assert_eq!(pass.far_rgb(), [0.5, 0.6, 0.7]);
        assert_eq!(pass.output_scale(), k());
        assert_eq!(pass.far_colour.w, RetailFogPass::SAO_FACTOR);
    }

    /// The shader's depth unprojection: `view_from_clip * (ndc.xy, depth, 1)`, then `-z / w`.
    fn view_depth(clip_from_view: Mat4, uv: Vec2, ndc_depth: f32) -> f32 {
        let ndc_xy = uv * Vec2::new(2.0, -2.0) + Vec2::new(-1.0, 1.0);
        let position = clip_from_view.inverse() * ndc_xy.extend(ndc_depth).extend(1.0);
        -(position.z / position.w)
    }

    /// The depth a point at `view` (view space, looking down -Z) writes, and the uv it covers.
    fn project(clip_from_view: Mat4, view: Vec3) -> (Vec2, f32) {
        let clip = clip_from_view * view.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        (Vec2::new(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5), ndc.z)
    }

    fn relative_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= expected.abs() * 1.0e-3,
            "expected {expected}, got {actual}"
        );
    }

    /// The world camera is infinite reverse-Z with near 15 (`fNearDistance`, #949): the unprojected
    /// depth is the axial view depth, `near / depth`, at every distance the fog ramp reaches.
    #[test]
    fn view_depth_is_the_axial_distance_for_the_world_camera() {
        let projection = Projection::Perspective(PerspectiveProjection {
            near: 15.0,
            ..default()
        });
        let clip_from_view = projection.get_clip_from_view();
        for (x, y, z) in [
            (0.0, 0.0, 15.5),
            (30.0, -10.0, 100.0),
            (-500.0, 200.0, 1_000.0),
            (4_000.0, 900.0, 10_000.0),
            (0.0, 0.0, 80_000.0),
        ] {
            let (uv, depth) = project(clip_from_view, Vec3::new(x, y, -z));
            relative_close(view_depth(clip_from_view, uv, depth), z);
            // The same value as `near / depth` (m22 = 0, m32 = near).
            relative_close(15.0 / depth, z);
        }
    }

    #[test]
    fn perspective_fog_depth_matches_the_forward_depth_remap() {
        for far in [200.0_f64, 10_240.0, 76_800.0] {
            let near = 15.0;
            for depth in [near, 0.1 * far, 0.5 * far, 0.99 * far, far] {
                let d = far * (depth - near) / ((far - near) * depth);
                let remapped = 1.01 * d - 0.01;
                let expected =
                    2.0 * near * far / ((near + far) - (2.0 * remapped - 1.0) * (far - near));
                let stable = depth / (1.01 - 0.01 * depth / far);
                assert!((stable - expected).abs() < expected * 1.0e-11);
                let sky_edge = near * far / (near + (far - near) * 0.000001);
                assert_eq!(depth < sky_edge, d < 0.999999);
            }
        }
    }

    /// The water reflection camera clips obliquely at the water plane (`reflection_lens`), so its
    /// depth also depends on the pixel. `near / depth` is wrong there; the unprojection is not.
    #[test]
    fn view_depth_is_the_axial_distance_for_the_oblique_reflection_camera() {
        let projection = Projection::Perspective(PerspectiveProjection {
            near: 15.0,
            near_clip_plane: Vec3::new(0.2, 0.9, -0.3).normalize().extend(-12.0),
            ..default()
        });
        let clip_from_view = projection.get_clip_from_view();
        let mut worst_flat_error: f32 = 0.0;
        for (x, y, z) in [
            (0.0, 0.0, 100.0),
            (300.0, 200.0, 1_000.0),
            (-800.0, 100.0, 2_500.0),
            (1_500.0, 600.0, 6_000.0),
        ] {
            let (uv, depth) = project(clip_from_view, Vec3::new(x, y, -z));
            relative_close(view_depth(clip_from_view, uv, depth), z);
            worst_flat_error = worst_flat_error.max((15.0 / depth - z).abs() / z);
        }
        assert!(
            worst_flat_error > 0.01,
            "the oblique matrix should break `near / depth` (error {worst_flat_error})"
        );
    }

    /// The shader draws the formula pinned above. Each line below is a term of it.
    #[test]
    fn the_shader_draws_the_pinned_formula() {
        let shader = include_str!("shaders/retail_fog.wgsl");
        let code: String = shader
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for term in [
            // Fc = min(saturate(z * r - near * r) ^ p, max), r = 1 / (far - near).
            "let r = 1.0 / (fog.range.y - fog.range.x);",
            "let s = clamp(z * r - near * r, 0.0, 1.0);",
            "let fc = min(exp2(log2(s) * fog.range.z), fog.range.w);",
            // Cfc = nearRGB + Fc * (farRGB - nearRGB).
            "let cfc = fog.near_colour.rgb + fc * (fog.far_colour.rgb - fog.near_colour.rgb);",
            // The blend: src.rgb = k * Fc * Cfc, src.a = k * (1 - Fc) * A.
            "return vec4<f32>(k * fc * cfc, k * (1.0 - fc) * fog.far_colour.w);",
            // Sky skips fog and output scale but keeps the ambient occlusion term.
            "let k = fog.near_colour.w;",
            "if ndc <= 0.0 {",
            "return vec4<f32>(0.0, 0.0, 0.0, fog.far_colour.w);",
            // The axial depth, unprojected through the full inverse projection.
            "view.view_from_clip,",
            "var z = -view_position.z;",
            "z /= 1.01 - 0.01 * (z / camera_far);",
            "max(copied_z, colour_z)",
        ] {
            assert!(code.contains(term), "retail_fog.wgsl lost `{term}`");
        }
        assert!(
            !code.contains("0.8333") && !code.contains("0.833_3"),
            "k must come from the uniform"
        );
    }

    /// Grass (the alpha-mask pass) leaves its fog to the composite and keeps its own `k`, so a grass
    /// pixel gets `k` squared at `Fc = 0` (skyrim-research NEEDS 24, inferred, medium confidence). If
    /// grass fogged itself in the blended-only mode it would be fogged twice.
    #[test]
    fn grass_skips_its_own_fog_under_the_composite_and_keeps_k() {
        let grass = include_str!("shaders/grass.wgsl");
        assert!(
            grass.contains(
                "if fog.mode == FOG_MODE_POWER_RAMP_BLENDED {\n        return input_color;\n    }"
            ),
            "grass.wgsl must skip apply_fog under the blended-only mode"
        );
        // Grass takes k from the same constant, when the camera has fog (the proxy for retail's
        // non-zero fog distances).
        assert!(
            include_str!("grass_render.rs")
                .contains("crate::output_scale::framebuffer_output_scale(!fogs.is_empty())")
        );
        // The retail net factor on a grass pixel with no fog and no SAO: k * k.
        let grass_scale = crate::output_scale::framebuffer_output_scale(true);
        close(
            composite(0.4 * grass_scale, 1.0, 0.0, 0.6, k()),
            0.4 * k() * k(),
        );
        close(k() * k(), 0.694_444);
    }

    /// The blend is `src.rgb + dst.rgb * src.a` and leaves the alpha channel alone.
    #[test]
    fn the_blend_composes_the_formula_and_keeps_alpha() {
        assert_eq!(COMPOSITE_BLEND.color.src_factor, BlendFactor::One);
        assert_eq!(COMPOSITE_BLEND.color.dst_factor, BlendFactor::SrcAlpha);
        assert_eq!(COMPOSITE_BLEND.alpha.src_factor, BlendFactor::Zero);
        assert_eq!(COMPOSITE_BLEND.alpha.dst_factor, BlendFactor::One);
        // Evaluate the blend by hand for one channel and compare with the formula.
        let (l, fc, cfc) = (0.4_f32, 0.5_f32, 0.6_f32);
        let (src_rgb, src_a) = (k() * fc * cfc, k() * (1.0 - fc) * RetailFogPass::SAO_FACTOR);
        close(src_rgb + l * src_a, composite(l, 1.0, fc, cfc, k()));
    }
}
