//! Synthetic output-domain check; no Skyrim assets or retail parity claim.
//! Headless GPU readback: --output <directory> [--interior] [--gray] [--legacy-output]
//! [--color-grade <off|saturation|full|snapshot-srgb|snapshot-linear>].
//! Use --input-linear <r,g,b> for an unclipped colored composition probe.
use bevy::{
    camera::{
        Hdr, RenderTarget, ScalingMode,
        visibility::{NoFrustumCulling, RenderLayers},
    },
    core_pipeline::tonemapping::DebandDither,
    light::NotShadowCaster,
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
    render::{
        RenderPlugin,
        settings::{RenderCreation, WgpuLimits, WgpuSettings, WgpuSettingsPriority},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::{ExitCondition, WindowPlugin},
};
use engine::{
    color_pipeline::{
        SceneColorPipeline, SceneGradeMode, SkyrimCinematicPost, SkyrimCinematicPostPlugin,
    },
    profiling::ProfilingState,
    render::{
        TerrainExtension, TerrainMaterial, VercidiumRendererPlugin, WaterExtension, WaterMaterial,
        WaterReflectionTexture,
    },
    sky::{SkyMaterial, SkyPalette, SkyPlugin, SkyUniform, dome_mesh},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    interior: bool,
    legacy_output: bool,
    color_grade: SceneGradeMode,
    color: LinearRgba,
    started: Instant,
    requested: bool,
    frames: u32,
}

#[derive(Resource)]
struct ProbeTarget(Handle<Image>);

fn main() {
    let mut output = PathBuf::from("color-pipeline-probe");
    let mut interior = false;
    let mut gray = false;
    let mut input_linear = None;
    let mut legacy_output = false;
    let mut color_grade = SceneGradeMode::Full;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().expect("--output requires a directory").into(),
            "--interior" => interior = true,
            "--gray" => gray = true,
            "--input-linear" => {
                let raw = args.next().expect("--input-linear requires r,g,b");
                let values = raw
                    .split(',')
                    .map(|part| part.parse::<f32>().expect("invalid RGB component"))
                    .collect::<Vec<_>>();
                assert!(
                    values.len() == 3
                        && values
                            .iter()
                            .all(|value| value.is_finite() && *value >= 0.0),
                    "--input-linear needs three finite nonnegative components"
                );
                input_linear = Some(LinearRgba::rgb(values[0], values[1], values[2]));
            }
            "--legacy-output" => legacy_output = true,
            "--color-grade" => {
                color_grade =
                    SceneGradeMode::parse(&args.next().expect("--color-grade requires a mode"))
                        .expect("invalid --color-grade mode");
            }
            _ => panic!("unknown probe option {arg:?}"),
        }
    }
    if color_grade == SceneGradeMode::RecordSrgb {
        eprintln!(
            "record-srgb requires an image-space record from a world database; \
             use the engine with --image-space instead of this synthetic probe"
        );
        std::process::exit(2);
    }
    assert!(
        !gray || input_linear.is_none(),
        "--gray cannot be combined with --input-linear"
    );
    assert!(
        !legacy_output || color_grade == SceneGradeMode::Full,
        "--legacy-output cannot be combined with --color-grade"
    );
    if legacy_output {
        color_grade = SceneGradeMode::Off;
    }
    std::fs::create_dir_all(&output).expect("create probe output directory");
    // Remove stale verdicts before rendering; a crash must not look like a successful new run.
    for name in ["probe.json", "probe.png"] {
        let path = output.join(name);
        if path.exists() {
            std::fs::remove_file(path).expect("remove stale probe output");
        }
    }
    let result = App::new()
        .insert_resource(color_grade)
        .insert_resource(Probe {
            output,
            interior,
            legacy_output,
            color_grade,
            color: input_linear.unwrap_or_else(|| {
                if gray {
                    LinearRgba::rgb(0.18, 0.18, 0.18)
                } else {
                    LinearRgba::rgb(0.18, 0.4, 2.0)
                }
            }),
            started: Instant::now(),
            requested: false,
            frames: 0,
        })
        .init_resource::<ProfilingState>()
        .add_plugins(
            DefaultPlugins
                .set(RenderPlugin {
                    // Exercise the same material shaders without requesting every optional adapter
                    // feature. Terrain's six color/normal pairs exceed WebGPU's default 16 textures.
                    // They share one sampler; retain the default 16-sampler limit for Metal.
                    render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                        priority: WgpuSettingsPriority::WebGPU,
                        limits: WgpuLimits {
                            max_sampled_textures_per_shader_stage: 32,
                            ..default()
                        },
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
        )
        .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
            Duration::from_millis(16),
        ))
        .add_plugins((
            VercidiumRendererPlugin,
            SkyPlugin,
            SkyrimCinematicPostPlugin,
        ))
        .add_systems(PostStartup, setup)
        .add_systems(Update, capture)
        .run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_arguments)]
fn setup(
    mut commands: Commands,
    probe: Res<Probe>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut terrain: ResMut<Assets<TerrainMaterial>>,
    mut water: ResMut<Assets<WaterMaterial>>,
    mut skies: ResMut<Assets<SkyMaterial>>,
    reflection: Res<WaterReflectionTexture>,
    mut cameras: Query<(Entity, &mut Camera)>,
) {
    if probe.legacy_output {
        use bevy::render::render_resource::TextureFormat;
        *images.get_mut(&reflection.0).unwrap() = Image::new_target_texture(
            1024,
            576,
            TextureFormat::Rgba8Unorm,
            Some(TextureFormat::Rgba8UnormSrgb),
        );
    }
    // Reuse the production reflection target/camera with a constant unlit surface.
    // This exercises material tone mapping too; a clear-only reflection would miss that path.
    // No StreamingCamera runs the visibility gate.
    for (entity, mut camera) in &mut cameras {
        if camera.order == -1 {
            if probe.legacy_output {
                commands
                    .entity(entity)
                    .remove::<Hdr>()
                    .insert(bevy::core_pipeline::tonemapping::Tonemapping::TonyMcMapface);
            }
            camera.is_active = true;
            camera.clear_color = ClearColorConfig::Custom(Color::BLACK);
            commands.entity(entity).insert((
                RenderLayers::layer(7),
                DebandDither::Disabled,
                Msaa::Off,
            ));
        }
    }
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(10.0, 10.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::LinearRgba(probe.color),
            unlit: true,
            fog_enabled: false,
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, -1.0),
        RenderLayers::layer(7),
    ));
    let target = images.add(Image::new_target_texture(
        800,
        600,
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        None,
    ));
    commands.insert_resource(ProbeTarget(target.clone()));
    let main_camera = commands
        .spawn((
            Camera3d::default(),
            RenderTarget::Image(target.into()),
            SceneColorPipeline::default(),
            DebandDither::Disabled,
            Msaa::Off,
            Camera {
                clear_color: ClearColorConfig::Custom(Color::BLACK),
                ..default()
            },
            Projection::Orthographic(OrthographicProjection {
                scaling_mode: ScalingMode::FixedVertical {
                    viewport_height: 6.0,
                },
                ..OrthographicProjection::default_3d()
            }),
            Transform::from_xyz(0.0, 0.0, 10.0),
            DistanceFog {
                color: Color::LinearRgba(probe.color),
                directional_light_color: Color::NONE,
                falloff: FogFalloff::Linear {
                    start: 0.0,
                    end: 1.0,
                },
                ..default()
            },
        ))
        .id();
    if probe.legacy_output {
        commands.entity(main_camera).remove::<Hdr>().insert((
            bevy::render::view::ColorGrading::default(),
            SkyrimCinematicPost::default(),
        ));
    }
    let quad = meshes.add(Rectangle::new(3.0, 1.5));
    // Sample centres: mesh (200,150), fog (600,150), terrain (200,450), water (600,450).
    commands.spawn((
        Mesh3d(quad.clone()),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::LinearRgba(probe.color),
            unlit: true,
            fog_enabled: false,
            ..default()
        })),
        Transform::from_xyz(-2.0, 1.5, 0.0),
    ));
    commands.spawn((
        Mesh3d(quad.clone()),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::BLACK,
            unlit: true,
            ..default()
        })),
        Transform::from_xyz(2.0, 1.5, 0.0),
    ));
    commands.spawn((
        Mesh3d(quad.clone()),
        MeshMaterial3d(terrain.add(TerrainMaterial {
            base: StandardMaterial {
                base_color: Color::BLACK,
                emissive: probe.color,
                fog_enabled: false,
                ..default()
            },
            extension: TerrainExtension::default(),
        })),
        Transform::from_xyz(-2.0, -1.5, 0.0),
    ));
    commands.spawn((
        Mesh3d(quad),
        MeshMaterial3d(water.add(WaterMaterial {
            base: StandardMaterial {
                base_color: Color::BLACK,
                fog_enabled: false,
                ..default()
            },
            // A unit reflector isolates the sampled reflection from the water's own lighting.
            extension: WaterExtension::with_reflection_and_factors(
                reflection.0.clone(),
                None,
                1.0,
                1.0,
            ),
        })),
        Transform::from_xyz(2.0, -1.5, 0.0),
    ));
    if !probe.interior {
        let encoded = probe.color.to_f32_array();
        let srgb = Color::linear_rgba(encoded[0], encoded[1], encoded[2], 1.0).to_srgba();
        let mut sky = SkyUniform::new(&SkyPalette::default());
        // Constant rows test the real sky shader without fitting weather interpolation.
        sky.colours = [Vec4::new(srgb.red, srgb.green, srgb.blue, 1.0); 16];
        commands.spawn((
            Mesh3d(meshes.add(dome_mesh())),
            MeshMaterial3d(skies.add(SkyMaterial { sky })),
            Transform::from_xyz(0.0, 0.0, 10.0),
            NoFrustumCulling,
            NotShadowCaster,
        ));
    }
}

fn capture(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    target: Res<ProbeTarget>,
    mut exit: MessageWriter<AppExit>,
) {
    if probe.started.elapsed() > Duration::from_secs(90) {
        eprintln!("color probe timed out before readback");
        exit.write(AppExit::error());
    }
    probe.frames = probe.frames.saturating_add(1);
    // Allow pipeline compilation and reflection rendering before requesting the readback.
    if probe.requested || probe.frames < 30 || probe.started.elapsed() < Duration::from_secs(5) {
        return;
    }
    probe.requested = true;
    commands
        .spawn(Screenshot::image(target.0.clone()))
        .observe(evaluate);
}

fn evaluate(
    event: On<ScreenshotCaptured>,
    probe: Res<Probe>,
    mut exit: MessageWriter<AppExit>,
    adapter: Res<bevy::render::renderer::RenderAdapterInfo>,
) {
    let image = event
        .image
        .clone()
        .try_into_dynamic()
        .expect("decode GPU screenshot")
        .to_rgb8();
    image
        .save(probe.output.join("probe.png"))
        .expect("save probe PNG");
    assert_eq!(
        image.dimensions(),
        (800, 600),
        "probe requires an 800x600 framebuffer"
    );
    let locations = [
        ("mesh", 200, 150),
        ("fog", 600, 150),
        ("terrain", 200, 450),
        ("water", 600, 450),
        ("background", 400, 250),
    ];
    let mut samples = serde_json::Map::new();
    let reference = image.get_pixel(200, 150).0;
    let reference_unclipped = reference.iter().all(|v| *v > 10 && *v < 250);
    let mut composition_consistent = true;
    for (name, x, y) in locations {
        let expected = if probe.interior && name == "background" {
            [0; 3]
        } else {
            reference
        };
        let mut max_error = 0u8;
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                let pixel = image
                    .get_pixel((x as i32 + dx) as u32, (y as i32 + dy) as u32)
                    .0;
                for (actual, expected) in pixel.into_iter().zip(expected) {
                    max_error = max_error.max(actual.abs_diff(expected));
                }
            }
        }
        composition_consistent &= max_error <= 2;
        samples.insert(name.into(), serde_json::json!({"pixel": [x,y], "rgb": image.get_pixel(x,y).0, "max_error_u8": max_error}));
    }
    let passed = reference_unclipped && composition_consistent;
    let preset = engine::color_pipeline::SceneVisualPreset::default();
    let resolved = probe.color_grade.grade(preset);
    let post = probe.color_grade.post(preset);
    let report = serde_json::json!({
        "kind": "synthetic-output-consistency", "retail_parity": false,
        "space": if probe.interior { "interior" } else { "exterior" },
        "input_linear": probe.color.to_f32_array(), "ev100": engine::color_pipeline::DEFAULT_SCENE_EV100,
        "tonemapping": "TonyMcMapface", "legacy_output": probe.legacy_output,
        "cinematic_grade": probe.color_grade.name(),
        "resolved_grade": {
            "saturation": resolved.saturation, "brightness": resolved.brightness,
            "contrast": resolved.contrast, "tint_amount": resolved.tint_amount,
            "tint_rgb": resolved.tint_rgb,
            "bevy_saturation": probe.color_grade.bevy_saturation(preset),
            "post_saturation": post.saturation, "post_linear_domain": post.linear_domain,
            "post_enabled": post.enabled,
        },
        "saturation_domain": match probe.color_grade {
            SceneGradeMode::Off => None,
            SceneGradeMode::SnapshotSrgb => Some("srgb-post-tonemap"),
            _ => Some("linear-post-tonemap"),
        },
        "post_grade_domain": match probe.color_grade {
            SceneGradeMode::Off | SceneGradeMode::Saturation => None,
            SceneGradeMode::SnapshotLinear => Some("linear"),
            _ => Some("srgb"),
        },
        "artistic_pivot": engine::color_pipeline::CINEMATIC_PIVOT,
        "reflection_format": if probe.legacy_output { "Rgba8UnormSrgb" } else { "Rgba16Float" },
        "resolution": [800,600], "camera": {"position": [0,0,10], "projection": "orthographic", "vertical_size": 6},
        "adapter": {"name": adapter.name, "backend": format!("{:?}", adapter.backend), "driver": adapter.driver, "driver_info": adapter.driver_info},
        "tolerance_u8": 2, "samples": samples,
        "reference_unclipped": reference_unclipped,
        "composition_consistent": composition_consistent, "passed": passed,
    });
    std::fs::write(
        probe.output.join("probe.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .expect("save probe report");
    println!("{}", report);
    exit.write(if passed {
        AppExit::Success
    } else {
        AppExit::error()
    });
}
