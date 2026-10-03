//! Paired vertex-alpha geometry checks through color, depth and shadow passes.
//! --output DIR [--interior] [--legacy-prepass] (negative control).
use bevy::{
    camera::{RenderTarget, ScalingMode},
    core_pipeline::{prepass::DepthPrepass, tonemapping::DebandDither},
    light::CascadeShadowConfigBuilder,
    prelude::*,
    render::{
        RenderPlugin,
        settings::{RenderCreation, WgpuSettings, WgpuSettingsPriority},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::{ExitCondition, WindowPlugin},
};
use engine::{color_pipeline::SceneColorPipeline, prepass_vertex_alpha::PrepassVertexAlphaPlugin};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    target: Handle<Image>,
    frames: u32,
    start: Instant,
    interior: bool,
    legacy: bool,
}

fn main() {
    let mut output = PathBuf::from("material-alpha-probe");
    let mut interior = false;
    let mut legacy = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().expect("output directory").into(),
            "--interior" => interior = true,
            "--legacy-prepass" => legacy = true,
            _ => panic!("unknown argument {arg}"),
        }
    }
    std::fs::create_dir_all(&output).unwrap();
    for name in ["probe.json", "probe.png"] {
        let p = output.join(name);
        if p.exists() {
            std::fs::remove_file(p).unwrap();
        }
    }
    let mut app = App::new();
    app.insert_resource(GlobalAmbientLight {
        brightness: 0.0,
        ..default()
    })
    .add_plugins(
        DefaultPlugins
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    priority: WgpuSettingsPriority::WebGPU,
                    ..default()
                })),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .disable::<bevy::winit::WinitPlugin>()
            .disable::<bevy::audio::AudioPlugin>(),
    );
    if !legacy {
        app.add_plugins(PrepassVertexAlphaPlugin);
    }
    app.add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
        Duration::from_millis(16),
    ));
    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            800,
            600,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            None,
        ));
    let result = app
        .insert_resource(Probe {
            output,
            target,
            frames: 0,
            start: Instant::now(),
            interior,
            legacy,
        })
        .add_systems(Startup, setup)
        .add_systems(Update, capture)
        .run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

fn setup(
    mut commands: Commands,
    probe: Res<Probe>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut faded = Rectangle::new(3.0, 2.0).mesh().build();
    let bevy::mesh::VertexAttributeValues::Float32x3(positions) =
        faded.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
    else {
        unreachable!()
    };
    // Linear fade crosses alpha 0.5 exactly at x=0. The reference is the right half.
    let colors: Vec<_> = positions
        .iter()
        .map(|p| [1.0, 1.0, 1.0, (p[0] + 1.5) / 3.0])
        .collect();
    faded.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    let faded = meshes.add(faded);
    let half = meshes.add(Rectangle::new(1.5, 2.0));
    let receiver = meshes.add(Rectangle::new(3.8, 5.5));
    let front = materials.add(StandardMaterial {
        base_color: Color::linear_rgb(0.1, 0.55, 0.2),
        reflectance: 0.0,
        alpha_mode: AlphaMode::Mask(0.5),
        ..default()
    });
    let reference = materials.add(StandardMaterial {
        base_color: Color::linear_rgb(0.1, 0.55, 0.2),
        reflectance: 0.0,
        ..default()
    });
    let background = materials.add(StandardMaterial {
        base_color: Color::linear_rgb(0.6, 0.4, 0.25),
        reflectance: 0.0,
        ..default()
    });
    commands.spawn((
        Mesh3d(faded),
        MeshMaterial3d(front),
        Transform::from_xyz(-2.0, 0.8, 1.0),
    ));
    commands.spawn((
        Mesh3d(half),
        MeshMaterial3d(reference),
        Transform::from_xyz(2.75, 0.8, 1.0),
    ));
    for x in [-2.0, 2.0] {
        commands.spawn((
            Mesh3d(receiver.clone()),
            MeshMaterial3d(background.clone()),
            Transform::from_xyz(x, 0.0, 0.0),
        ));
    }
    commands.spawn((
        DirectionalLight {
            illuminance: 5000.0,
            shadow_maps_enabled: true,
            shadow_depth_bias: 0.02,
            shadow_normal_bias: 0.0,
            ..default()
        },
        CascadeShadowConfigBuilder {
            num_cascades: 1,
            maximum_distance: 30.0,
            first_cascade_far_bound: 30.0,
            ..default()
        }
        .build(),
        Transform::from_xyz(0.0, 3.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Camera3d::default(),
        RenderTarget::Image(probe.target.clone().into()),
        SceneColorPipeline::default(),
        DepthPrepass,
        DebandDither::Disabled,
        Msaa::Off,
        Camera {
            clear_color: ClearColorConfig::Custom(if probe.interior {
                Color::BLACK
            } else {
                Color::srgb(0.12, 0.18, 0.25)
            }),
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: 6.0,
            },
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 10.0),
    ));
}

fn capture(mut commands: Commands, mut probe: ResMut<Probe>, mut exit: MessageWriter<AppExit>) {
    if probe.start.elapsed() > Duration::from_secs(90) {
        exit.write(AppExit::error());
        return;
    }
    probe.frames += 1;
    if probe.frames == 120 {
        commands
            .spawn(Screenshot::image(probe.target.clone()))
            .observe(evaluate);
    }
}

fn evaluate(
    event: On<ScreenshotCaptured>,
    probe: Res<Probe>,
    adapter: Res<bevy::render::renderer::RenderAdapterInfo>,
    mut exit: MessageWriter<AppExit>,
) {
    let image = event.image.clone().try_into_dynamic().unwrap().to_rgb8();
    image.save(probe.output.join("probe.png")).unwrap();
    // x is relative to each panel center; y is a world coordinate. Samples avoid raster boundaries.
    let cases = [
        ("discarded_depth", -0.8, 0.8),
        ("visible", 0.8, 0.8),
        ("discarded_shadow", -0.8, -1.2),
        ("cast_shadow", 0.8, -1.2),
        ("lit_receiver", -0.8, -2.4),
    ];
    let mut samples = Vec::new();
    let mut passed = true;
    for (name, x, y) in cases {
        let px = (200.0 + x * 100.0) as u32;
        let py = (300.0 - y * 100.0) as u32;
        let a = image.get_pixel(px, py).0;
        let b = image.get_pixel(px + 400, py).0;
        let error = a
            .into_iter()
            .zip(b)
            .map(|(a, b)| a.abs_diff(b))
            .max()
            .unwrap();
        passed &= error <= 2;
        if name != "cast_shadow" {
            passed &= b.iter().any(|v| *v > 10);
        } else {
            passed &= b.iter().all(|v| *v < 10);
        }
        samples
            .push(serde_json::json!({"case":name,"actual":a,"reference":b,"max_error_u8":error}));
    }
    let report = serde_json::json!({"kind":"alpha-color-depth-shadow","retail_parity":false,"legacy_prepass":probe.legacy,"interior":probe.interior,"ev100":9.7,"resolution":[800,600],"tolerance_u8":2,"samples":samples,"adapter":adapter.name,"passed":passed});
    std::fs::write(
        probe.output.join("probe.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!("{report}");
    exit.write(if passed {
        AppExit::Success
    } else {
        AppExit::error()
    });
}
