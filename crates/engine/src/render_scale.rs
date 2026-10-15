//! Draws the 3D view below the window's resolution and scales it up to fill the window.
//!
//! A Retina window has two physical pixels per logical point on each axis, so a window that is
//! 1600x900 points on screen is 3200x1800 pixels - four times the fragments of the same window on
//! a standard display, every one of them through terrain blending, lighting, fog and the HDR
//! targets. Mac games draw their 3D view below that and scale it up; this does the same. The world
//! camera renders into an image of the scaled size, and a 2D camera stretches that image across
//! the window. The UI follows the highest-ordered camera on the window - the 2D one - so text
//! stays sharp at the window's full resolution.
//!
//! [`GpuTuning::render_scale`] picks the factor. At 1 the world camera draws straight to the window
//! as it always has, with no image and no extra camera.

use crate::{gpu_tuning::GpuTuning, render::SCALED_VIEW_LAYER, world::components::StreamingCamera};
use bevy::{
    camera::{CameraUpdateSystems, ImageRenderTarget, RenderTarget, visibility::RenderLayers},
    core_pipeline::tonemapping::DebandDither,
    image::ImageSampler,
    prelude::*,
    render::render_resource::{Extent3d, TextureFormat},
    window::{PrimaryWindow, WindowRef},
};

/// Scales the world camera's view to [`GpuTuning::render_scale`].
pub struct RenderScalePlugin;

impl Plugin for RenderScalePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, apply_render_scale.before(CameraUpdateSystems));
    }
}

/// The image the world camera draws into while its view is scaled, and the entities that put it
/// on the window.
#[derive(Resource)]
struct ScaledView {
    image: Handle<Image>,
    blit_camera: Entity,
    sprite: Entity,
}

/// The physical size the world camera draws at for a window of `physical` pixels: `scale` of each
/// axis, rounded, and never empty.
fn scaled_size(physical: UVec2, scale: f32) -> UVec2 {
    (physical.as_vec2() * scale)
        .round()
        .as_uvec2()
        .max(UVec2::ONE)
}

/// Keeps the world camera's target in step with the window: an image of the scaled size, stretched
/// across the window, when the view is scaled, and the window itself when it is not. Runs every
/// frame, so a resized window or one moved to a display of another scale factor is followed.
fn apply_render_scale(
    mut commands: Commands,
    tuning: Res<GpuTuning>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<&mut RenderTarget, (With<Camera3d>, With<StreamingCamera>)>,
    mut images: ResMut<Assets<Image>>,
    mut sprites: Query<&mut Sprite>,
    scaled: Option<Res<ScaledView>>,
) {
    let (Ok(window), Ok(mut target)) = (windows.single(), cameras.single_mut()) else {
        return;
    };
    // Only a camera drawing to the window, or into this module's own image, is scaled. A camera
    // something else pointed at an image - a shots run renders into its capture image - keeps it.
    let owned = match &*target {
        RenderTarget::Window(WindowRef::Primary) => true,
        RenderTarget::Image(image) => scaled
            .as_ref()
            .is_some_and(|scaled| scaled.image == image.handle),
        _ => false,
    };
    if !owned {
        return;
    }
    let physical = window.physical_size();
    // A minimised window has no pixels to scale to; keep whatever was set up until it returns.
    if physical.x == 0 || physical.y == 0 {
        return;
    }
    let size = scaled_size(physical, tuning.render_scale(window.scale_factor()));
    if size == physical {
        if let Some(scaled) = scaled {
            commands.entity(scaled.blit_camera).despawn();
            commands.entity(scaled.sprite).despawn();
            commands.remove_resource::<ScaledView>();
            *target = RenderTarget::Window(WindowRef::Primary);
            info!(physical = ?physical, "3D view drawn at the window's resolution");
        }
        return;
    }
    let logical = Vec2::new(window.width(), window.height());
    let image = match scaled {
        Some(scaled) => {
            let extent = Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 1,
            };
            if images
                .get(&scaled.image)
                .is_some_and(|image| image.texture_descriptor.size != extent)
            {
                if let Some(mut image) = images.get_mut(&scaled.image) {
                    image.resize(extent);
                }
                info!(physical = ?physical, scaled = ?size, "3D view resized");
            }
            if let Ok(mut sprite) = sprites.get_mut(scaled.sprite)
                && sprite.custom_size != Some(logical)
            {
                sprite.custom_size = Some(logical);
            }
            scaled.image.clone()
        }
        None => {
            let mut image =
                Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
            // Bilinear, so the scaled view is smoothed rather than blocky.
            image.sampler = ImageSampler::linear();
            let image = images.add(image);
            let blit_camera = commands
                .spawn((
                    Name::new("Scaled 3D view camera"),
                    Camera2d,
                    // After the world camera, so its image is complete when it is drawn.
                    Camera {
                        order: 1,
                        ..default()
                    },
                    Msaa::Off,
                    // The world camera already dithered its output.
                    DebandDither::Disabled,
                    RenderLayers::layer(SCALED_VIEW_LAYER),
                ))
                .id();
            let sprite = commands
                .spawn((
                    Name::new("Scaled 3D view"),
                    Sprite {
                        image: image.clone(),
                        custom_size: Some(logical),
                        ..default()
                    },
                    RenderLayers::layer(SCALED_VIEW_LAYER),
                ))
                .id();
            commands.insert_resource(ScaledView {
                image: image.clone(),
                blit_camera,
                sprite,
            });
            info!(physical = ?physical, scaled = ?size, "3D view drawn below the window's resolution");
            image
        }
    };
    // Written only when it changes, so the camera's target is not marked changed every frame.
    if !matches!(&*target, RenderTarget::Image(current) if current.handle == image) {
        *target = RenderTarget::Image(ImageRenderTarget {
            handle: image,
            scale_factor: 1.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GpuProfile, RenderScale};

    #[test]
    fn scaled_size_rounds_each_axis_and_never_collapses() {
        assert_eq!(
            scaled_size(UVec2::new(3200, 1800), 0.5),
            UVec2::new(1600, 900)
        );
        assert_eq!(scaled_size(UVec2::new(3, 5), 0.5), UVec2::new(2, 3));
        assert_eq!(scaled_size(UVec2::new(1, 1), 0.25), UVec2::ONE);
    }

    /// A world camera and a primary window of `scale_factor`, with the scaling system alone.
    fn harness(tuning: GpuTuning, scale_factor: f32) -> (App, Entity, Entity) {
        let mut app = App::new();
        app.insert_resource(tuning)
            .init_resource::<Assets<Image>>()
            .add_systems(PostUpdate, apply_render_scale);
        let mut window = Window::default();
        window.resolution = bevy::window::WindowResolution::new(3200, 1800);
        window.resolution.set_scale_factor(scale_factor);
        let window = app.world_mut().spawn((window, PrimaryWindow)).id();
        let camera = app
            .world_mut()
            .spawn((Camera3d::default(), StreamingCamera))
            .id();
        (app, window, camera)
    }

    fn target(app: &App, camera: Entity) -> RenderTarget {
        app.world().get::<RenderTarget>(camera).unwrap().clone()
    }

    #[test]
    fn a_retina_window_draws_the_world_at_one_pixel_per_point() {
        let tuning = GpuTuning::resolve(GpuProfile::Tiled, None, RenderScale::Auto, true);
        let (mut app, _, camera) = harness(tuning, 2.0);
        app.update();
        let RenderTarget::Image(image) = target(&app, camera) else {
            panic!("the world camera should draw into the scaled image");
        };
        let size = app
            .world()
            .resource::<Assets<Image>>()
            .get(&image.handle)
            .unwrap()
            .size();
        assert_eq!(size, UVec2::new(1600, 900));
        let sprites: Vec<_> = app
            .world_mut()
            .query::<&Sprite>()
            .iter(app.world())
            .map(|sprite| sprite.custom_size)
            .collect();
        assert_eq!(sprites, vec![Some(Vec2::new(1600.0, 900.0))]);
    }

    #[test]
    fn a_full_resolution_view_draws_straight_to_the_window() {
        let (mut app, _, camera) = harness(GpuTuning::default(), 2.0);
        app.update();
        assert!(matches!(
            target(&app, camera),
            RenderTarget::Window(WindowRef::Primary)
        ));
        assert!(app.world().get_resource::<ScaledView>().is_none());
    }

    #[test]
    fn a_camera_drawing_into_another_image_keeps_it() {
        let tuning = GpuTuning::resolve(GpuProfile::Tiled, None, RenderScale::Auto, true);
        let (mut app, _, camera) = harness(tuning, 2.0);
        let capture = app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(Image::default());
        app.world_mut()
            .entity_mut(camera)
            .insert(RenderTarget::Image(capture.clone().into()));
        app.update();
        assert!(matches!(
            target(&app, camera),
            RenderTarget::Image(image) if image.handle == capture
        ));
        assert!(app.world().get_resource::<ScaledView>().is_none());
    }

    #[test]
    fn moving_to_a_standard_display_drops_the_scaled_view() {
        let tuning = GpuTuning::resolve(GpuProfile::Tiled, None, RenderScale::Auto, true);
        let (mut app, window, camera) = harness(tuning, 2.0);
        app.update();
        assert!(app.world().get_resource::<ScaledView>().is_some());
        app.world_mut()
            .get_mut::<Window>(window)
            .unwrap()
            .resolution
            .set_scale_factor(1.0);
        app.update();
        assert!(matches!(
            target(&app, camera),
            RenderTarget::Window(WindowRef::Primary)
        ));
        assert!(app.world().get_resource::<ScaledView>().is_none());
        assert_eq!(
            app.world_mut().query::<&Sprite>().iter(app.world()).count(),
            0
        );
    }
}
