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
//! `--gpu-profile` picks the tuning; `auto` resolves it from the backend the renderer selected.

use crate::config::{EngineConfig, GpuProfile, RenderScale};
use bevy::{
    core_pipeline::prepass::DepthPrepass,
    light::ShadowFilteringMethod,
    prelude::*,
    render::{
        occlusion_culling::OcclusionCulling,
        render_resource::TextureFormat,
        renderer::{RenderAdapterInfo, RenderDevice},
    },
};

/// The sun's shadow cascades on a tiled GPU: one pass fewer over the shadow casters, with the
/// remaining three stretched over the same distance.
const TILED_SUN_SHADOW_CASCADES: usize = 3;

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
            .add_systems(PostUpdate, tune_new_cameras)
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

/// On a tiled GPU, takes the depth prepass and the occlusion culling built on it off every new 3D
/// camera, and gives it the cheaper shadow filter. The spawning code states the desktop setup, so
/// this is the one place the tiled setup departs from it.
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
            .insert(tuning.shadow_filtering());
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
    fn tiled_cameras_lose_the_depth_prepass_and_gain_the_hardware_shadow_filter() {
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
            assert_eq!(
                entity.get::<ShadowFilteringMethod>().copied(),
                tiled.then_some(ShadowFilteringMethod::Hardware2x2)
            );
        }
    }
}
