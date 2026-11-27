//! One-pixel GPU check of the near ambient term (#1010). Ignored: it needs a GPU adapter.
//!
//! Run with `MUDCRAB_AMBIENT_PIXEL=1 cargo test -p engine --release -- --ignored ambient_pixel`
//! (through `scripts/workspace.py`). It opens no window: the camera renders to an offscreen
//! image and the test reads one pixel back.
//!
//! The scene is ambient only: one white `StandardMaterial` plane facing the camera, no lights,
//! no tone mapping. A white plane with `reflectance` 0 has no specular ambient, so the pixel is
//! `albedo * ambient * exposure` times the diffuse scale of the ambient term:
//!
//! - flat ambient (the `mudcrab` profile, and every profile without the cube): Bevy's
//!   `EnvBRDFApprox(diffuse, F_AB(1.0, NdotV))`, about 0.4524 at roughness 1;
//! - directional ambient (the `skyrim` profile): retail's raw `albedo * (mean + slopes . N)`, 1.0.
//!
//! The first case pins that the `mudcrab` path still has the 0.4524 scale: it is the render-level
//! gate for "mudcrab unchanged". The others pin the raw `skyrim` ambient, which is the unit the
//! grass shader takes too (#1011: its factor of pi cancels the grass shader's `/ pi`).

use bevy::{
    app::PluginsState,
    camera::{Exposure, RenderTarget, ScalingMode},
    core_pipeline::tonemapping::Tonemapping,
    pbr::DirectionalAmbientCube,
    prelude::*,
    render::{
        RenderPlugin,
        pipelined_rendering::PipelinedRenderingPlugin,
        render_resource::TextureFormat,
        settings::RenderCreation,
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::ExitCondition,
    winit::WinitPlugin,
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

const SIZE: u32 = 64;
/// The scene's ambient, linear.
const MEAN: Vec3 = Vec3::new(0.60, 0.40, 0.20);
/// The polynomial `F_AB(1.0, NdotV).x` at perceptual roughness 1, for every `NdotV`. With
/// `r = 1.0 * c0 + c1 = (0, 0.015, 0.468, -0.018)` the term `min(r.x^2, ...) * r.x + r.y` is 0.015
/// and `F_AB.x = -1.04 * 0.015 + 0.468 = 0.4524` (`pbr_lighting.wgsl`, the path without
/// `DFG_LUT`, which the workspace does not enable).
const BEVY_DIFFUSE_AMBIENT_SCALE: f32 = 0.4524;

fn srgb_to_linear(byte: u8) -> f32 {
    let c = f32::from(byte) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn app() -> App {
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::default()),
                synchronous_pipeline_compilation: true,
                ..default()
            })
            .disable::<WinitPlugin>()
            .disable::<bevy::audio::AudioPlugin>()
            .disable::<PipelinedRenderingPlugin>(),
    )
    .insert_resource(ClearColor(Color::BLACK))
    .insert_resource(GlobalAmbientLight {
        color: Color::LinearRgba(LinearRgba::rgb(MEAN.x, MEAN.y, MEAN.z)),
        brightness: 1.0,
        ..default()
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.plugins_state() != PluginsState::Ready {
        assert!(
            Instant::now() < deadline,
            "renderer initialization timed out"
        );
        bevy::tasks::tick_global_task_pools_on_main_thread();
        std::thread::sleep(Duration::from_millis(10));
    }
    app.finish();
    app.cleanup();
    app
}

/// The centre pixel in linear light, after `frames` settling frames.
fn read_pixel(app: &mut App, target: &Handle<Image>, frames: u32) -> [f32; 3] {
    for _ in 0..frames {
        app.update();
        std::thread::sleep(Duration::from_millis(15));
    }
    let (sender, receiver) = mpsc::channel::<Image>();
    app.world_mut()
        .spawn(Screenshot::image(target.clone()))
        .observe(move |event: On<ScreenshotCaptured>| {
            sender.send(event.image.clone()).unwrap();
        });
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.update();
        if let Ok(image) = receiver.try_recv() {
            let rgb = image.try_into_dynamic().unwrap().to_rgb8();
            let pixel = rgb.get_pixel(SIZE / 2, SIZE / 2);
            return [0, 1, 2].map(|channel| srgb_to_linear(pixel[channel]));
        }
        assert!(Instant::now() < deadline, "screenshot timed out");
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[test]
#[ignore = "needs a GPU adapter; run with MUDCRAB_AMBIENT_PIXEL=1 and --ignored"]
fn ambient_pixel_flat_is_bevys_scale_and_the_cube_is_retails_raw() {
    assert_eq!(
        std::env::var("MUDCRAB_AMBIENT_PIXEL").as_deref(),
        Ok("1"),
        "set MUDCRAB_AMBIENT_PIXEL=1"
    );
    let mut app = app();
    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            SIZE,
            SIZE,
            TextureFormat::Rgba8UnormSrgb,
            None,
        ));
    let exposure = Exposure { ev100: 0.0 }.exposure();
    app.world_mut().spawn((
        Camera3d::default(),
        RenderTarget::Image(target.clone().into()),
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: 2.0,
            },
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 10.0).looking_at(Vec3::ZERO, Vec3::Y),
        Msaa::Off,
        Tonemapping::None,
        Exposure { ev100: 0.0 },
    ));
    let mesh = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Rectangle::new(4.0, 4.0));
    let material = app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::WHITE,
            // No specular ambient: F0 = 0 makes the specular occlusion 0.
            reflectance: 0.0,
            perceptual_roughness: 1.0,
            ..default()
        });
    // The plane's normal is the engine's +Z, which is Creation -Y.
    app.world_mut()
        .spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::IDENTITY));

    // 1. Flat ambient, as under `mudcrab`: the original expression, Bevy's diffuse scale.
    let flat = read_pixel(&mut app, &target, 8);
    for channel in 0..3 {
        let expected = MEAN[channel] * exposure * BEVY_DIFFUSE_AMBIENT_SCALE;
        assert!(
            (flat[channel] - expected).abs() < 0.01 * expected + 0.004,
            "flat ambient, channel {channel}: pixel {}, expected {expected} \
             (= mean x exposure x {BEVY_DIFFUSE_AMBIENT_SCALE})",
            flat[channel]
        );
    }
    println!(
        "flat ambient pixel (linear) = {flat:?}, mean x exposure = {:?}",
        MEAN * exposure
    );

    // 2. The cube with equal faces: retail's raw ambient, no scale.
    app.insert_resource(DirectionalAmbientCube {
        enabled: true,
        mean: MEAN,
        ..default()
    });
    let raw = read_pixel(&mut app, &target, 8);
    for channel in 0..3 {
        let expected = MEAN[channel] * exposure;
        assert!(
            (raw[channel] - expected).abs() < 0.01 * expected + 0.004,
            "directional ambient, channel {channel}: pixel {}, expected {expected} \
             (= mean x exposure, no EnvBRDFApprox scale)",
            raw[channel]
        );
    }
    println!("directional ambient pixel (linear) = {raw:?}");

    // 3. With slopes: the pixel is `albedo * (mean + creation . slopes) * exposure` for the
    // normal (0, 0, 1), which is Creation (0, -1, 0): `mean - slope_y`.
    let slope_y = Vec3::new(0.15, -0.10, 0.05);
    app.insert_resource(DirectionalAmbientCube {
        enabled: true,
        mean: MEAN,
        slope_x: Vec3::new(0.3, 0.3, 0.3),
        slope_y,
        slope_z: Vec3::new(0.2, 0.2, 0.2),
    });
    let sloped = read_pixel(&mut app, &target, 8);
    for channel in 0..3 {
        let expected = (MEAN[channel] - slope_y[channel]) * exposure;
        assert!(
            (sloped[channel] - expected).abs() < 0.01 * expected + 0.004,
            "sloped ambient, channel {channel}: pixel {}, expected {expected}",
            sloped[channel]
        );
    }
    println!("sloped ambient pixel (linear) = {sloped:?}");

    // 4. A disabled cube is the flat colour again, to the byte.
    app.insert_resource(DirectionalAmbientCube {
        enabled: false,
        mean: Vec3::splat(0.9),
        ..default()
    });
    let back = read_pixel(&mut app, &target, 8);
    assert_eq!(
        flat, back,
        "a disabled cube leaves the flat ambient pixel unchanged"
    );
}
