//! The underwater post-pass: retail's depth-of-field and fog image-space effect in one pass.
//!
//! What retail does (RF-WATER-031, RF-WATER-052 to RF-WATER-054): below the surface the frame goes
//! through `ISDepthOfFieldFogged`. It rebuilds the linear view-space depth of every pixel from the
//! depth buffer, blends the sharp frame toward a blurred copy by `sat((d - focus) / range)` times
//! the strength, and then toward the water's fog colour by `sat((d - near) / (far - near))`. Every
//! pixel is fogged, the sky included, because the sky's depth is the far plane. The blur is a
//! separable five-tap pass (weights 0.38774, 0.24477, 0.06136).
//!
//! What Mudcrab did before: Bevy's distance fog on the world camera, which fogs only what a lit
//! material draws (not the sky), follows the straight-line distance from the eye instead of the
//! view-space depth, and cannot blur.
//!
//! What Mudcrab does now, and differently from retail:
//! - the depth is the opaque depth the water shader already snapshots (`water_depth.rs`), which
//!   holds no water surface, so the sky and the surface seen from below get the sky's full fog;
//! - the blur is the five-tap pair without retail's depth-edge term (`lap`), whose scale depends on
//!   the retail camera's near and far planes (the registry does not give them; #698);
//! - the focus, range and strength are the values of `UnderwaterImageSpace` (RF-WATER-015), the
//!   image space every Skyrim, Update and Dragonborn water points at, not read per record;
//! - the crossing mode (mode 1, the surface cutting the screen) draws nothing: it needs retail's
//!   underwater plane mask (RF-WATER-043).

use crate::water_depth::WaterDepthSnapshot;
use bevy::{
    asset::{embedded_asset, load_embedded_asset},
    core_pipeline::{
        FullscreenShader,
        schedule::{Core3d, Core3dSystems},
        tonemapping::tonemapping,
    },
    prelude::*,
    render::{
        Render, RenderApp, RenderStartup, RenderSystems,
        camera::ExtractedCamera,
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        render_asset::RenderAssets,
        render_resource::{
            BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
            CachedRenderPipelineId, ColorTargetState, ColorWrites, Extent3d, FragmentState, LoadOp,
            Operations, PipelineCache, PrimitiveState, RenderPassColorAttachment,
            RenderPassDescriptor, RenderPipelineDescriptor, ShaderStages, ShaderType,
            SpecializedRenderPipeline, SpecializedRenderPipelines, StoreOp, TextureDescriptor,
            TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
            binding_types::{texture_2d, uniform_buffer},
        },
        renderer::{RenderContext, RenderDevice, ViewQuery},
        texture::{CachedTexture, GpuImage, TextureCache},
        view::{ExtractedView, ViewTarget},
    },
    shader::Shader,
};

/// `UnderwaterImageSpace` DNAM focus distance (RF-WATER-015: strength 1, focus -50, range 500,
/// flags 18). Flags 18 select the far side only (RF-WATER-032, RF-WATER-053).
pub(crate) const UNDERWATER_BLUR_FOCUS: f32 = -50.0;
/// `UnderwaterImageSpace` DNAM range (RF-WATER-015).
pub(crate) const UNDERWATER_BLUR_RANGE: f32 = 500.0;
/// `UnderwaterImageSpace` DNAM strength (RF-WATER-015).
pub(crate) const UNDERWATER_BLUR_STRENGTH: f32 = 1.0;

/// The settings of the pass, one per camera that is under water. The component exists only while
/// the camera is fully below the surface, so the pass costs nothing above it.
#[derive(Component, Clone, Copy, Debug, PartialEq, ExtractComponent, ShaderType)]
pub(crate) struct UnderwaterPass {
    /// rgb = the fog colour `C` (linear): the Shallow Color times the Water Multiplier;
    /// w = the fog share at full depth, 1 (RF-WATER-052: `cb2[4].w`).
    fog_color: Vec4,
    /// x = fog near (WATR DNAM +0x90), y = fog far (+0x94), z = the camera's near plane, w unused.
    fog_range: Vec4,
    /// x = blur focus, y = blur range, z = blur strength, w unused.
    blur: Vec4,
}

impl UnderwaterPass {
    pub(crate) fn new(
        fog_color: LinearRgba,
        fog_near: f32,
        fog_far: f32,
        camera_near: f32,
    ) -> Self {
        Self {
            fog_color: Vec4::new(fog_color.red, fog_color.green, fog_color.blue, 1.0),
            fog_range: Vec4::new(fog_near, fog_far, camera_near, 0.0),
            blur: Vec4::new(
                UNDERWATER_BLUR_FOCUS,
                UNDERWATER_BLUR_RANGE,
                UNDERWATER_BLUR_STRENGTH,
                0.0,
            ),
        }
    }
}

/// Preparation and drawing use only the current frame's extracted underwater state.
type ActiveUnderwaterView = With<UnderwaterPass>;

/// The complete production draw query, shared with the CPU contract regression.
type UnderwaterDrawView<'w, 's> = ViewQuery<
    'w,
    's,
    (
        &'static ViewTarget,
        &'static DynamicUniformIndex<UnderwaterPass>,
        &'static UnderwaterTextures,
        &'static UnderwaterPipelineIds,
    ),
    ActiveUnderwaterView,
>;

/// The share of the blurred frame at view depth `d` (RF-WATER-053, far side only):
/// `sat((d - focus) / range) * strength`.
#[cfg(test)]
pub(crate) fn underwater_blur_share(depth: f32) -> f32 {
    ((depth - UNDERWATER_BLUR_FOCUS) / UNDERWATER_BLUR_RANGE).clamp(0.0, 1.0)
        * UNDERWATER_BLUR_STRENGTH
}

/// The share of the fog colour at view depth `d` (RF-WATER-052): `sat((d - near) / (far - near))`.
/// A sky pixel has depth zero in the reverse-Z buffer, so it is infinitely far and fully fogged.
#[cfg(test)]
pub(crate) fn underwater_fog_share(depth: f32, near: f32, far: f32) -> f32 {
    ((depth - near) / (far - near)).clamp(0.0, 1.0)
}

pub(crate) struct UnderwaterPassPlugin;

impl Plugin for UnderwaterPassPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/underwater.wgsl");
        app.add_plugins((
            ExtractComponentPlugin::<UnderwaterPass>::default(),
            UniformComponentPlugin::<UnderwaterPass>::default(),
        ));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<SpecializedRenderPipelines<UnderwaterPipeline>>()
            .add_systems(RenderStartup, init_underwater_pipeline)
            .add_systems(
                Render,
                (
                    (clear_inactive_underwater_views, prepare_underwater_textures)
                        .chain()
                        .in_set(RenderSystems::PrepareResources),
                    prepare_underwater_pipelines.in_set(RenderSystems::Prepare),
                ),
            )
            .add_systems(
                Core3d,
                run_underwater_pass
                    .in_set(Core3dSystems::PostProcess)
                    .before(tonemapping),
            );
    }
}

/// Which of the three fullscreen passes a pipeline draws.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Stage {
    BlurHorizontal,
    BlurVertical,
    Composite,
}

impl Stage {
    fn entry_point(self) -> &'static str {
        match self {
            Self::BlurHorizontal => "blur_horizontal",
            Self::BlurVertical => "blur_vertical",
            Self::Composite => "composite",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct UnderwaterPipelineKey {
    stage: Stage,
    format: TextureFormat,
}

#[derive(Resource)]
struct UnderwaterPipeline {
    shader: Handle<Shader>,
    fullscreen_shader: FullscreenShader,
    blur_layout: BindGroupLayoutDescriptor,
    composite_layout: BindGroupLayoutDescriptor,
}

fn init_underwater_pipeline(
    mut commands: Commands,
    fullscreen_shader: Res<FullscreenShader>,
    asset_server: Res<AssetServer>,
) {
    // Every bound texture is read with `textureLoad`, so none needs a sampler or a filter.
    let float = || texture_2d(TextureSampleType::Float { filterable: false });
    commands.insert_resource(UnderwaterPipeline {
        shader: load_embedded_asset!(asset_server.as_ref(), "shaders/underwater.wgsl"),
        fullscreen_shader: fullscreen_shader.clone(),
        blur_layout: BindGroupLayoutDescriptor::new(
            "underwater_blur_layout",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (uniform_buffer::<UnderwaterPass>(true), float()),
            ),
        ),
        composite_layout: BindGroupLayoutDescriptor::new(
            "underwater_composite_layout",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (
                    uniform_buffer::<UnderwaterPass>(true),
                    float(),
                    float(),
                    float(),
                ),
            ),
        ),
    });
}

impl SpecializedRenderPipeline for UnderwaterPipeline {
    type Key = UnderwaterPipelineKey;

    fn specialize(&self, key: UnderwaterPipelineKey) -> RenderPipelineDescriptor {
        let layout = match key.stage {
            Stage::Composite => self.composite_layout.clone(),
            _ => self.blur_layout.clone(),
        };
        RenderPipelineDescriptor {
            label: Some(format!("underwater_{:?}", key.stage).into()),
            layout: vec![layout],
            vertex: self.fullscreen_shader.to_vertex_state(),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                shader_defs: Vec::new(),
                entry_point: Some(key.stage.entry_point().into()),
                targets: vec![Some(ColorTargetState {
                    format: key.format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            ..default()
        }
    }
}

/// The three pipelines of one view.
#[derive(Component, Clone, Copy)]
struct UnderwaterPipelineIds {
    horizontal: CachedRenderPipelineId,
    vertical: CachedRenderPipelineId,
    composite: CachedRenderPipelineId,
    format: TextureFormat,
}

/// Views that still hold underwater state after the extracted effect has left them.
type InactiveUnderwaterView = (
    Without<UnderwaterPass>,
    Or<(
        With<UnderwaterTextures>,
        With<UnderwaterPipelineIds>,
        With<DynamicUniformIndex<UnderwaterPass>>,
    )>,
);

/// Releases per-view state as soon as the extracted effect leaves the view.
fn clear_inactive_underwater_views(
    mut commands: Commands,
    views: Query<Entity, InactiveUnderwaterView>,
) {
    for entity in &views {
        commands.entity(entity).remove::<(
            UnderwaterTextures,
            UnderwaterPipelineIds,
            DynamicUniformIndex<UnderwaterPass>,
        )>();
    }
}

fn prepare_underwater_pipelines(
    mut commands: Commands,
    pipeline: Res<UnderwaterPipeline>,
    pipeline_cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<UnderwaterPipeline>>,
    views: Query<(Entity, &ExtractedView), ActiveUnderwaterView>,
) {
    for (entity, view) in &views {
        let mut id = |stage| {
            pipelines.specialize(
                &pipeline_cache,
                &pipeline,
                UnderwaterPipelineKey {
                    stage,
                    format: view.target_format,
                },
            )
        };
        commands.entity(entity).insert(UnderwaterPipelineIds {
            horizontal: id(Stage::BlurHorizontal),
            vertical: id(Stage::BlurVertical),
            composite: id(Stage::Composite),
            format: view.target_format,
        });
    }
}

/// The two blur targets of one view: the horizontal pass writes `first`, the vertical pass reads it
/// and writes `second`.
#[derive(Component)]
struct UnderwaterTextures {
    first: CachedTexture,
    second: CachedTexture,
    target: UnderwaterTarget,
}

/// The attachment contract for both blur textures and the view they will composite into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UnderwaterTarget {
    size: UVec2,
    format: TextureFormat,
}

/// A resized or reformatted view waits until matching resources are prepared.
fn underwater_resources_current(
    target: UnderwaterTarget,
    textures: UnderwaterTarget,
    pipeline_format: TextureFormat,
) -> bool {
    target.size.x > 0 && target.size.y > 0 && target == textures && target.format == pipeline_format
}

fn prepare_underwater_textures(
    mut commands: Commands,
    mut cache: ResMut<TextureCache>,
    device: Res<RenderDevice>,
    views: Query<(Entity, &ExtractedCamera, &ExtractedView), ActiveUnderwaterView>,
) {
    for (entity, camera, view) in &views {
        let Some(size) = camera
            .physical_target_size
            .filter(|size| size.x > 0 && size.y > 0)
        else {
            commands.entity(entity).remove::<UnderwaterTextures>();
            continue;
        };
        let mut texture = |label| {
            cache.get(
                &device,
                TextureDescriptor {
                    label: Some(label),
                    size: Extent3d {
                        width: size.x,
                        height: size.y,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format: view.target_format,
                    usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };
        let first = texture("underwater_blur_first");
        let second = texture("underwater_blur_second");
        commands.entity(entity).insert(UnderwaterTextures {
            first,
            second,
            target: UnderwaterTarget {
                size,
                format: view.target_format,
            },
        });
    }
}

/// Runs the blur pair and the composite for a view that is under water. Without the water depth
/// snapshot (or when it is unresolved this frame) the composite leaves the frame as it is.
fn run_underwater_pass(
    view: UnderwaterDrawView,
    uniforms: Res<ComponentUniforms<UnderwaterPass>>,
    pipeline: Res<UnderwaterPipeline>,
    pipeline_cache: Res<PipelineCache>,
    snapshot: Res<WaterDepthSnapshot>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
) {
    let (target, index, textures, ids) = view.into_inner();
    if !underwater_resources_current(
        UnderwaterTarget {
            size: UVec2::new(
                target.main_texture().width(),
                target.main_texture().height(),
            ),
            format: target.main_texture_format(),
        },
        textures.target,
        ids.format,
    ) {
        return;
    }
    let Some(settings) = uniforms.uniforms().binding() else {
        return;
    };
    let (Some(horizontal), Some(vertical), Some(composite)) = (
        pipeline_cache.get_render_pipeline(ids.horizontal),
        pipeline_cache.get_render_pipeline(ids.vertical),
        pipeline_cache.get_render_pipeline(ids.composite),
    ) else {
        return;
    };
    // The depth is the water snapshot: no snapshot, no depth, no pass.
    let Some(depth) = snapshot
        .image()
        .and_then(|handle| images.get(handle))
        .map(|image| &image.texture_view)
    else {
        return;
    };
    let post_process = target.post_process_write();
    let device = ctx.render_device().clone();
    let blur_layout = pipeline_cache.get_bind_group_layout(&pipeline.blur_layout);
    let composite_layout = pipeline_cache.get_bind_group_layout(&pipeline.composite_layout);

    let blur_from_frame = device.create_bind_group(
        "underwater_blur_horizontal",
        &blur_layout,
        &BindGroupEntries::sequential((settings.clone(), post_process.source)),
    );
    let blur_from_first = device.create_bind_group(
        "underwater_blur_vertical",
        &blur_layout,
        &BindGroupEntries::sequential((settings.clone(), &textures.first.default_view)),
    );
    let composite_bind_group = device.create_bind_group(
        "underwater_composite",
        &composite_layout,
        &BindGroupEntries::sequential((
            settings,
            post_process.source,
            &textures.second.default_view,
            depth,
        )),
    );

    let mut draw = |label: &'static str,
                    pipeline: &bevy::render::render_resource::RenderPipeline,
                    bind_group: &bevy::render::render_resource::BindGroup,
                    destination: &bevy::render::render_resource::TextureView| {
        let descriptor = RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: destination,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(wgpu::Color::BLACK),
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        };
        let mut pass = ctx.command_encoder().begin_render_pass(&descriptor);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[index.index()]);
        pass.draw(0..3, 0..1);
    };
    draw(
        "underwater_blur_horizontal",
        horizontal,
        &blur_from_frame,
        &textures.first.default_view,
    );
    draw(
        "underwater_blur_vertical",
        vertical,
        &blur_from_first,
        &textures.second.default_view,
    );
    draw(
        "underwater_composite",
        composite,
        &composite_bind_group,
        post_process.destination,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Matches the real draw query's component contract without allocating GPU objects.
    fn draw_view_matches<D: bevy::ecs::query::QueryData, F: bevy::ecs::query::QueryFilter>(
        _: std::marker::PhantomData<ViewQuery<D, F>>,
        world: &mut World,
        underwater: bool,
    ) -> bool {
        let mut present = vec![
            world.register_component::<ViewTarget>(),
            world.register_component::<DynamicUniformIndex<UnderwaterPass>>(),
            world.register_component::<UnderwaterTextures>(),
            world.register_component::<UnderwaterPipelineIds>(),
        ];
        if underwater {
            present.push(world.register_component::<UnderwaterPass>());
        }
        world
            .query_filtered::<D, F>()
            .matches_component_set(&|id| present.contains(&id))
    }

    #[test]
    fn production_draw_query_requires_current_underwater_state() {
        let mut world = World::new();
        let query = std::marker::PhantomData::<UnderwaterDrawView>;
        assert!(draw_view_matches(query, &mut world, true));
        assert!(
            !draw_view_matches(query, &mut world, false),
            "the production draw query must reject retained resources without current underwater state"
        );
    }

    #[test]
    fn production_plugin_registers_cleanup_before_texture_preparation() {
        use bevy::{
            app::SubApp,
            asset::AssetApp,
            ecs::{
                schedule::Schedules,
                system::{IntoSystem, System},
            },
        };

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Shader>();
        let mut render = SubApp::new();
        render.add_schedule(Render::base_schedule());
        app.insert_sub_app(RenderApp, render);
        app.add_plugins(UnderwaterPassPlugin);
        let world = app.sub_app_mut(RenderApp).world_mut();
        let mut schedule = world.resource_mut::<Schedules>().remove(Render).unwrap();
        schedule.initialize(world).unwrap();
        let prepare = schedule
            .graph()
            .systems_in_set(RenderSystems::PrepareResources.intern())
            .unwrap();
        let systems: Vec<_> = schedule.systems().unwrap().collect();
        let cleanup_type = IntoSystem::into_system(clear_inactive_underwater_views).system_type();
        let textures_type = IntoSystem::into_system(prepare_underwater_textures).system_type();
        let cleanup = systems
            .iter()
            .position(|(id, system)| prepare.contains(id) && system.system_type() == cleanup_type)
            .expect("the production plugin registers inactive-view cleanup in PrepareResources");
        let textures = systems
            .iter()
            .position(|(_, system)| system.system_type() == textures_type)
            .unwrap();
        assert!(
            cleanup < textures,
            "cleanup precedes texture preparation in the production schedule"
        );
    }

    #[test]
    fn removing_underwater_state_releases_retained_resources() {
        use bevy::ecs::system::RunSystemOnce;

        let mut world = World::new();
        let pass = UnderwaterPass::new(LinearRgba::new(0.1, 0.2, 0.3, 1.0), -500.0, 1600.0, 0.1);
        let retained = UnderwaterPipelineIds {
            horizontal: CachedRenderPipelineId::INVALID,
            vertical: CachedRenderPipelineId::INVALID,
            composite: CachedRenderPipelineId::INVALID,
            format: TextureFormat::Rgba16Float,
        };
        let view = world.spawn((pass, retained)).id();
        world
            .run_system_once(clear_inactive_underwater_views)
            .unwrap();
        assert!(world.get::<UnderwaterPipelineIds>(view).is_some());
        world.entity_mut(view).remove::<UnderwaterPass>();
        world
            .run_system_once(clear_inactive_underwater_views)
            .unwrap();
        assert!(world.get::<UnderwaterPipelineIds>(view).is_none());
        world.entity_mut(view).insert((pass, retained));
        world
            .run_system_once(clear_inactive_underwater_views)
            .unwrap();
        assert!(world.get::<UnderwaterPipelineIds>(view).is_some());
    }

    #[test]
    fn resized_and_reformatted_views_wait_for_matching_underwater_resources() {
        let initial = UnderwaterTarget {
            size: UVec2::new(800, 600),
            format: TextureFormat::Rgba16Float,
        };
        let resized = UnderwaterTarget {
            size: UVec2::new(1600, 900),
            ..initial
        };
        let reformatted = UnderwaterTarget {
            format: TextureFormat::Rgba8Unorm,
            ..resized
        };
        assert!(underwater_resources_current(
            initial,
            initial,
            initial.format
        ));
        assert!(!underwater_resources_current(
            resized,
            initial,
            initial.format
        ));
        assert!(underwater_resources_current(
            resized,
            resized,
            resized.format
        ));
        assert!(!underwater_resources_current(
            reformatted,
            resized,
            resized.format
        ));
        assert!(!underwater_resources_current(
            reformatted,
            reformatted,
            resized.format
        ));
        assert!(underwater_resources_current(
            reformatted,
            reformatted,
            reformatted.format
        ));
        let minimized = UnderwaterTarget {
            size: UVec2::ZERO,
            ..initial
        };
        assert!(!underwater_resources_current(
            minimized,
            minimized,
            minimized.format
        ));
    }

    #[test]
    fn the_blur_and_fog_shares_are_the_retail_ramps() {
        // RF-WATER-053: UnderwaterImageSpace (1, -50, 500, 18) gives 0.1 at d = 0, 0.5 at 200 and
        // 1 from 450.
        assert!((underwater_blur_share(0.0) - 0.1).abs() < 1.0e-6);
        assert!((underwater_blur_share(200.0) - 0.5).abs() < 1.0e-6);
        assert_eq!(underwater_blur_share(450.0), 1.0);
        assert_eq!(underwater_blur_share(1.0e30), 1.0);
        // RF-WATER-052: DefaultWater (-500, 1600) is 24 % fog at 0, 48 % at 500, 100 % at 1600 and
        // beyond, and a sky pixel (infinitely far) is fully fogged.
        let fog = |d| underwater_fog_share(d, -500.0, 1600.0);
        assert!((fog(0.0) - 500.0 / 2100.0).abs() < 1.0e-6);
        assert!((fog(500.0) - 1000.0 / 2100.0).abs() < 1.0e-6);
        assert_eq!(fog(1600.0), 1.0);
        assert_eq!(fog(1.0e30), 1.0);
        // RiverWaterFlow (-1400, 2000): 41 % at 0, 71 % at 1000.
        assert!((underwater_fog_share(0.0, -1400.0, 2000.0) - 0.4118).abs() < 1.0e-3);
        assert!((underwater_fog_share(1000.0, -1400.0, 2000.0) - 0.7059).abs() < 1.0e-3);
    }

    #[test]
    fn the_shader_holds_the_retail_pass() {
        let source = include_str!("shaders/underwater.wgsl");
        for needle in [
            "const WEIGHT_CENTRE: f32 = 0.38774;",
            "const WEIGHT_NEAR: f32 = 0.24477;",
            "const WEIGHT_FAR: f32 = 0.06136;",
            "clamp((depth - fog_near) / (fog_far - fog_near), 0.0, 1.0) * settings.fog_color.w",
            "clamp((depth - settings.blur.x) / settings.blur.y, 0.0, 1.0) * settings.blur.z",
            "settings.fog_range.z / max(z, DEPTH_FLOOR)",
            "mix(colour, settings.fog_color.rgb, fog)",
        ] {
            assert!(source.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn the_settings_carry_the_image_space_and_the_water_fog() {
        let pass = UnderwaterPass::new(LinearRgba::new(0.1, 0.2, 0.3, 1.0), -500.0, 1600.0, 0.1);
        assert_eq!(pass.fog_color, Vec4::new(0.1, 0.2, 0.3, 1.0));
        assert_eq!(pass.fog_range, Vec4::new(-500.0, 1600.0, 0.1, 0.0));
        assert_eq!(pass.blur, Vec4::new(-50.0, 500.0, 1.0, 0.0));
    }

    #[test]
    fn there_is_no_depth_image_without_a_snapshot() {
        assert!(WaterDepthSnapshot::default().image().is_none());
    }
}
