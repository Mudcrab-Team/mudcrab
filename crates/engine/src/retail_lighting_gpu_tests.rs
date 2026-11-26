//! SPEC-1 worked examples A, B and D as pixels: the vendored retail Lighting shader, the fog
//! composite and the sky shader on a real GPU, read back from an offscreen half-float target.
//! Ignored: it needs a GPU adapter. It opens no window.
//!
//! Run with `MUDCRAB_RETAIL_PIXEL=1` and `--ignored retail_pixel` through
//! `scripts/workspace.py cargo test -p engine`.
//!
//! Each scene is one surface facing the camera, so the centre pixel sees the specification's
//! normal and view directions. The specification's Creation axes (Z up) are the engine's
//! `(x, z, -y)`, so the surface lies flat and the camera looks straight down on it. All record
//! values are the specification's invented examples. Example C (composite with an SAO factor,
//! adaptation and tonemap) has no scene: Mudcrab has no SAO pass, and the HDR shaders are compared
//! with their CPU reference in `retail_hdr_gpu_tests.rs`.

use crate::{
    retail_fog::{RetailFogParams, RetailFogPass, RetailFogPlugin},
    retail_lighting::{CAMERA_FAR, CAMERA_NEAR, UNIT_EXPOSURE_EV100, raw_bytes, sun_illuminance},
    sky::{SkyCamera, SkyPalette, SkyPlugin},
};
use bevy::{
    app::PluginsState,
    asset::RenderAssetUsages,
    camera::{Exposure, Hdr, RenderTarget},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    mesh::{Indices, PrimitiveTopology},
    pbr::{DirectionalAmbientCube, DistanceFog, FogFalloff, RetailSurface},
    prelude::*,
    render::{
        RenderPlugin,
        pipelined_rendering::PipelinedRenderingPlugin,
        render_resource::{Extent3d, TextureDimension, TextureFormat},
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

/// An odd side, so the centre pixel lies on the camera axis.
const SIZE: u32 = 65;
/// Half precision keeps about three decimal digits below 1.
const TOLERANCE: f32 = 1.5e-3;

fn app() -> App {
    assert_eq!(
        std::env::var("MUDCRAB_RETAIL_PIXEL").as_deref(),
        Ok("1"),
        "set MUDCRAB_RETAIL_PIXEL=1"
    );
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
    .add_plugins((RetailFogPlugin, SkyPlugin))
    .insert_resource(ClearColor(Color::BLACK))
    .insert_resource(crate::visual_profile::VisualProfile::Skyrim);
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

fn half_to_f32(half: u16) -> f32 {
    let sign = if half & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = i32::from((half >> 10) & 0x1f);
    let mantissa = f32::from(half & 0x03ff);
    if exponent == 0 {
        sign * mantissa * 2.0_f32.powi(-24)
    } else {
        sign * (1.0 + mantissa / 1024.0) * 2.0_f32.powi(exponent - 15)
    }
}

/// The centre pixel after `frames` settling frames.
fn read_pixel(app: &mut App, target: &Handle<Image>, frames: u32) -> Vec3 {
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
            let data = image.data.expect("a captured image holds its bytes");
            let side = SIZE as usize;
            let start = (side / 2 * side + side / 2) * 8;
            let channel = |index: usize| {
                let at = start + index * 2;
                half_to_f32(u16::from_le_bytes([data[at], data[at + 1]]))
            };
            return Vec3::new(channel(0), channel(1), channel(2));
        }
        assert!(Instant::now() < deadline, "screenshot timed out");
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[track_caller]
fn close(actual: Vec3, expected: [f32; 3], what: &str) {
    let expected = Vec3::from_array(expected);
    println!("{what}: pixel {actual}, expected {expected}");
    assert!(
        (actual - expected).abs().max_element() <= TOLERANCE,
        "{what}: expected {expected}, got {actual}"
    );
}

fn texel(app: &mut App, bytes: [u8; 4]) -> Handle<Image> {
    let image = Image::new(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bytes.to_vec(),
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    app.world_mut().resource_mut::<Assets<Image>>().add(image)
}

/// The common surface: diffuse (128,102,76), normal (128,128,255) with alpha 64, vertex colour
/// (255,230,204,255), in an identity frame (tangent +X, normal up).
fn spawn_surface(app: &mut App, specular: bool, alpha_mode: AlphaMode) -> Entity {
    let diffuse = texel(app, [128, 102, 76, 255]);
    let normal = texel(app, [128, 128, 255, 64]);
    let half = 200.0;
    let vertex = [1.0, 230.0 / 255.0, 204.0 / 255.0, 1.0];
    let mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![
            [-half, 0.0, half],
            [half, 0.0, half],
            [half, 0.0, -half],
            [-half, 0.0, -half],
        ],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4])
    .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[1.0, 0.0, 0.0, 1.0]; 4])
    // One texel: every coordinate samples it.
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.5, 0.5]; 4])
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![vertex; 4])
    .with_inserted_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    let mesh = app.world_mut().resource_mut::<Assets<Mesh>>().add(mesh);
    let material = app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color_texture: Some(diffuse),
            normal_map_texture: Some(normal),
            cull_mode: None,
            alpha_mode,
            retail_surface: Some(RetailSurface {
                specular_color: Vec3::ONE,
                glossiness: 30.0,
                specular,
                emissive_multiple: 0.0,
            }),
            ..default()
        });
    app.world_mut()
        .spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::IDENTITY))
        .id()
}

fn target(app: &mut App) -> Handle<Image> {
    app.world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            SIZE,
            SIZE,
            TextureFormat::Rgba16Float,
            None,
        ))
}

fn spawn_camera(app: &mut App, target: &Handle<Image>, transform: Transform) -> Entity {
    app.world_mut()
        .spawn((
            Camera3d::default(),
            Hdr,
            RenderTarget::Image(target.clone().into()),
            Projection::Perspective(PerspectiveProjection {
                fov: 0.02,
                near: 1.0,
                far: 20_000.0,
                ..default()
            }),
            transform,
            Msaa::Off,
            Tonemapping::None,
            DebandDither::Disabled,
            Exposure {
                ev100: UNIT_EXPOSURE_EV100,
            },
        ))
        .id()
}

/// A camera straight above the origin, `height` away, with Creation +Y up on screen.
fn overhead(height: f32) -> Transform {
    Transform::from_xyz(0.0, height, 0.0).looking_at(Vec3::ZERO, Vec3::NEG_Z)
}

/// The engine direction of a Creation-axes direction.
fn engine(creation: Vec3) -> Vec3 {
    Vec3::new(creation.x, creation.z, -creation.y)
}

fn spawn_sun(app: &mut App, colour: [u8; 3], sunlight_scale: f32, toward_creation: Vec3) -> Entity {
    let travel = -engine(toward_creation).normalize();
    app.world_mut()
        .spawn((
            DirectionalLight {
                color: raw_bytes(colour),
                illuminance: sun_illuminance(sunlight_scale),
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::IDENTITY.looking_to(travel, Vec3::Z),
        ))
        .id()
}

fn set_ambient(app: &mut App, faces: [[u8; 3]; 6]) {
    let lighting = crate::sky::WeatherLighting {
        sunlight: Color::BLACK,
        ambient_cube: faces.map(|[r, g, b]| Color::srgb_u8(r, g, b)),
    };
    app.insert_resource(lighting.directional_ambient())
        .insert_resource(GlobalAmbientLight {
            color: Color::WHITE,
            brightness: 1.0,
            ..default()
        });
    assert!(app.world().resource::<DirectionalAmbientCube>().enabled);
}

/// The camera height at which the surface's retail clip length is `distance` (section 3.3 of
/// [near-lighting.md](../../../docs/specs/engine/near-lighting.md)'s source).
fn height_for_clip_length(distance: f32) -> f32 {
    distance * (CAMERA_FAR - CAMERA_NEAR) / CAMERA_FAR + CAMERA_NEAR
}

#[test]
#[ignore = "needs a GPU adapter; run with MUDCRAB_RETAIL_PIXEL=1 and --ignored"]
fn retail_pixel_example_a_interior() {
    for (specular, blended, expected, what) in [
        (
            false,
            false,
            [0.961_737, 0.589_320, 0.368_039],
            "A first stage",
        ),
        (false, true, [0.706_963, 0.445_619, 0.292_230], "A blended"),
        (true, false, [1.189_643, 0.810_637, 0.606_589], "A specular"),
        (
            true,
            true,
            [0.869_753, 0.603_703, 0.462_623],
            "A specular blended",
        ),
    ] {
        let mut app = app();
        let alpha_mode = if blended {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        };
        spawn_surface(&mut app, specular, alpha_mode);
        // XCLL directional (80,90,110) overhead at SunlightScale 2.
        spawn_sun(&mut app, [80, 90, 110], 2.0, Vec3::Z);
        set_ambient(
            &mut app,
            [
                [40, 40, 50],
                [40, 40, 50],
                [30, 30, 40],
                [50, 50, 60],
                [20, 16, 12],
                [60, 70, 90],
            ],
        );
        // The point light: (255,160,80), fade 1.5 + 0.25, radius 256, distance 128.
        app.world_mut().spawn((
            PointLight {
                color: raw_bytes([255, 160, 80]),
                intensity: crate::retail_lighting::point_intensity(1.75),
                range: 256.0,
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_translation(engine(Vec3::new(0.6, 0.0, 0.8)) * 128.0),
        ));
        let target = target(&mut app);
        let camera = spawn_camera(&mut app, &target, overhead(height_for_clip_length(800.0)));
        let linear = |bytes| raw_bytes(bytes);
        app.world_mut().entity_mut(camera).insert(DistanceFog {
            color: linear([60, 70, 80]).with_alpha(0.9),
            falloff: FogFalloff::PowerRampBlended {
                start: 100.0,
                end: 5000.0,
                power: 1.0,
                near_color: linear([40, 50, 60]),
            },
            ..default()
        });
        close(read_pixel(&mut app, &target, 12), expected, what);
    }
}

/// Example B's scene at one hour: `(sun bytes' mix, SunlightScale, direction, DALC faces, fog
/// near and far colours, fog scalars)` and the expected lit pixel and tonemap input.
struct Exterior {
    sun: [f32; 3],
    sunlight_scale: f32,
    toward: Vec3,
    faces: [[f32; 3]; 6],
    near_rgb: [f32; 3],
    far_rgb: [f32; 3],
    ramp: [f32; 4],
    lit: [f32; 3],
    input: [f32; 3],
}

/// The bytes of a 0.2 day, 0.8 sunset mix.
fn at_18(day: [u8; 3], sunset: [u8; 3]) -> [f32; 3] {
    std::array::from_fn(|i| 0.2 * f32::from(day[i]) + 0.8 * f32::from(sunset[i]))
}

fn unit(bytes: [f32; 3]) -> [f32; 3] {
    bytes.map(|byte| byte / 255.0)
}

fn check_exterior(scene: &Exterior, what: &str) {
    for composite in [false, true] {
        let mut app = app();
        spawn_surface(&mut app, false, AlphaMode::Opaque);
        let [r, g, b] = unit(scene.sun);
        let travel = -engine(scene.toward).normalize();
        app.world_mut().spawn((
            DirectionalLight {
                color: Color::linear_rgb(r, g, b),
                illuminance: sun_illuminance(scene.sunlight_scale),
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::IDENTITY.looking_to(travel, Vec3::Z),
        ));
        let lighting = crate::sky::WeatherLighting {
            sunlight: Color::BLACK,
            ambient_cube: scene.faces.map(unit).map(|[r, g, b]| Color::srgb(r, g, b)),
        };
        app.insert_resource(lighting.directional_ambient())
            .insert_resource(GlobalAmbientLight {
                color: Color::WHITE,
                brightness: 1.0,
                ..default()
            });
        let target = target(&mut app);
        // The composite measures the axial depth: 5000, as the example states.
        let camera = spawn_camera(&mut app, &target, overhead(5000.0));
        if composite {
            let [near, far, power, max] = scene.ramp;
            app.world_mut()
                .entity_mut(camera)
                .insert(RetailFogPass::new(RetailFogParams {
                    near,
                    far,
                    power,
                    max,
                    near_rgb: unit(scene.near_rgb),
                    far_rgb: unit(scene.far_rgb),
                }));
        }
        let expected = if composite { scene.input } else { scene.lit };
        let stage = if composite { "tonemap input" } else { "L" };
        close(
            read_pixel(&mut app, &target, 12),
            expected,
            &format!("{what} {stage}"),
        );
    }
}

#[test]
#[ignore = "needs a GPU adapter; run with MUDCRAB_RETAIL_PIXEL=1 and --ignored"]
fn retail_pixel_example_b_exterior() {
    let bytes = |rgb: [u8; 3]| rgb.map(f32::from);
    check_exterior(
        &Exterior {
            sun: bytes([230, 215, 190]),
            sunlight_scale: 1.6,
            toward: Vec3::new(0.0, 0.242_536, 0.970_143),
            faces: [
                bytes([90, 90, 100]),
                bytes([110, 105, 100]),
                bytes([95, 95, 105]),
                bytes([105, 100, 100]),
                bytes([60, 55, 45]),
                bytes([130, 150, 180]),
            ],
            near_rgb: bytes([150, 170, 190]),
            far_rgb: bytes([130, 160, 200]),
            ramp: [1000.0, 80_000.0, 0.5, 0.8],
            lit: [0.966_026, 0.680_193, 0.437_308],
            input: [0.730_871, 0.562_637, 0.423_793],
        },
        "B 12:00",
    );
    check_exterior(
        &Exterior {
            sun: at_18([230, 215, 190], [200, 120, 70]),
            sunlight_scale: 1.84,
            toward: Vec3::new(-0.957_657, 0.069_829, 0.279_317),
            faces: [
                at_18([90, 90, 100], [100, 70, 60]),
                at_18([110, 105, 100], [60, 50, 60]),
                at_18([95, 95, 105], [75, 60, 65]),
                at_18([105, 100, 100], [80, 60, 60]),
                at_18([60, 55, 45], [45, 35, 30]),
                at_18([130, 150, 180], [80, 80, 120]),
            ],
            near_rgb: at_18([150, 170, 190], [170, 120, 110]),
            far_rgb: at_18([130, 160, 200], [150, 110, 120]),
            ramp: [800.0, 64_000.0, 0.54, 0.84],
            lit: [0.401_255, 0.233_716, 0.159_762],
            input: [0.379_016, 0.246_230, 0.199_329],
        },
        "B 18:00",
    );
}

#[test]
#[ignore = "needs a GPU adapter; run with MUDCRAB_RETAIL_PIXEL=1 and --ignored"]
fn retail_pixel_example_d_sky() {
    let mut app = app();
    // The zenith draws the upper row alone, so it takes the example's mix of rows 7 and 0.
    let upper = Color::srgb(110.0 / 255.0, 145.0 / 255.0, 202.5 / 255.0);
    app.insert_resource(SkyPalette {
        upper,
        ..SkyPalette::SKYRIM_CLEAR_DAY
    });
    let mut image_space = crate::retail_hdr::RetailImageSpace::default();
    image_space.0.sky_scale = 0.2;
    app.insert_resource(image_space);
    let target = target(&mut app);
    let camera = spawn_camera(
        &mut app,
        &target,
        Transform::IDENTITY.looking_to(Vec3::Y, Vec3::NEG_Z),
    );
    app.world_mut().entity_mut(camera).insert(SkyCamera);
    close(
        read_pixel(&mut app, &target, 12),
        [0.526_144, 0.640_523, 0.828_431],
        "D dome",
    );
}
