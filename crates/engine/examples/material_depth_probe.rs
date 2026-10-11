//! glTF native-material hook → scene cloning → GPU decal/depth regression check.
//! --output DIR [--legacy-depth] [--world-scale NUMBER]. No Skyrim assets required.
use bevy::{
    camera::RenderTarget,
    core_pipeline::{prepass::DepthPrepass, tonemapping::DebandDither},
    gltf::GltfAssetLabel,
    light::NotShadowCaster,
    prelude::*,
    render::{
        RenderPlugin,
        occlusion_culling::OcclusionCulling,
        settings::{RenderCreation, WgpuSettings, WgpuSettingsPriority},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::{ExitCondition, WindowPlugin},
};
use engine::{
    color_pipeline::SceneColorPipeline,
    nif_depth::{DECAL, DEPTH_TEST, DEPTH_WRITE, DYNAMIC_DECAL, NifDepthMaterial},
    nif_material::NifMaterialPlugin,
    prepass_vertex_alpha::PrepassVertexAlphaPlugin,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

// Each view checks a coplanar overlay and an overlay just behind its receiver. The
// latter makes the old depth path a useful negative control even on a quiet frame.
const VIEWS: [(f32, f32); 9] = [
    (4.0, 0.0),
    (4.0, 30.0),
    (4.0, -30.0),
    (4.0, 60.0),
    (4.0, -60.0),
    (8.0, 0.0),
    (8.0, 45.0),
    (12.0, 0.0),
    (12.0, -45.0),
];

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    legacy: bool,
    placement: Transform,
    world_scale: f32,
    start: Instant,
    scene: Handle<WorldAsset>,
    target: Handle<Image>,
    spawned: bool,
    view: usize,
    frames: u32,
    pending: bool,
    passed: bool,
    loader: serde_json::Value,
    captures: Vec<serde_json::Value>,
}

fn fixtures(output: &std::path::Path, legacy: bool) -> PathBuf {
    let assets = output.join("fixtures");
    std::fs::create_dir_all(&assets).unwrap();
    let mut bytes = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    for (mesh_index, z) in [0.0_f32, 0.0, -0.000_000_5, 0.02].into_iter().enumerate() {
        let (width, height) = if mesh_index == 3 {
            (0.5, 0.6)
        } else {
            (1.0, 1.2)
        };
        let positions = [
            [-width * 0.5, -height * 0.5, z],
            [width * 0.5, -height * 0.5, z],
            [width * 0.5, height * 0.5, z],
            [-width * 0.5, height * 0.5, z],
        ];
        let accessor = accessors.len();
        for (values, kind) in [
            (positions.into_iter().flatten().collect::<Vec<_>>(), "VEC3"),
            ([0.0_f32, 0.0, 1.0].repeat(4), "VEC3"),
            (
                vec![
                    1.0_f32, 1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
                    0.0,
                ],
                "VEC4",
            ),
        ] {
            let offset = bytes.len();
            for value in values {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            views.push(
                serde_json::json!({"buffer":0,"byteOffset":offset,"byteLength":bytes.len()-offset}),
            );
            let mut a = serde_json::json!({"bufferView":views.len()-1,"componentType":5126,"count":4,"type":kind});
            if accessors.len() == accessor {
                a["min"] = serde_json::json!([-width * 0.5, -height * 0.5, z]);
                a["max"] = serde_json::json!([width * 0.5, height * 0.5, z]);
            }
            accessors.push(a);
        }
        let offset = bytes.len();
        // Different diagonals exercise interpolation of the same plane.
        let indices = if mesh_index == 0 {
            [0_u16, 1, 2, 0, 2, 3]
        } else {
            [0, 1, 3, 1, 2, 3]
        };
        for index in indices {
            bytes.extend_from_slice(&index.to_le_bytes());
        }
        views.push(serde_json::json!({"buffer":0,"byteOffset":offset,"byteLength":12}));
        accessors.push(serde_json::json!({"bufferView":views.len()-1,"componentType":5123,"count":6,"type":"SCALAR"}));
        meshes.push(serde_json::json!({"primitives":[{
            "attributes":{"POSITION":accessor,"NORMAL":accessor+1,"COLOR_0":accessor+2},
            "indices":accessor+3,"material":if mesh_index==0 {0} else if mesh_index==3 {2} else {1}
        }]}));
    }
    let mut materials: Vec<_> = [
        (
            "receiver",
            [1.0, 0.0, 0.0, 1.0],
            "OPAQUE",
            DEPTH_TEST,
            DEPTH_WRITE,
        ),
        (
            "decal",
            [0.0, 1.0, 0.0, 1.0],
            "MASK",
            DEPTH_TEST | DECAL | DYNAMIC_DECAL,
            0,
        ),
        (
            "occluder",
            [0.0, 0.0, 1.0, 1.0],
            "OPAQUE",
            DEPTH_TEST,
            DEPTH_WRITE,
        ),
    ]
    .into_iter()
    .map(|(name, color, alpha, flags_1, flags_2)| {
        serde_json::json!({
            "name":name,"pbrMetallicRoughness":{"baseColorFactor":color,"metallicFactor":0.0},
            "alphaMode":alpha,"alphaCutoff":0.5,"doubleSided":true,
            "extensions":{"KHR_materials_unlit":{}},
            "extras":{"openSkyrim":{"shaderFlags1":flags_1,"shaderFlags2":flags_2}}
        })
    })
    .collect();
    if legacy {
        materials[1].as_object_mut().unwrap().remove("extras");
    }
    let document = serde_json::json!({
        "asset":{"version":"2.0"}, "extensionsUsed":["KHR_materials_unlit"],
        "buffers":[{"uri":"geometry.bin","byteLength":bytes.len()}],
        "bufferViews":views,"accessors":accessors,"meshes":meshes,"materials":materials,
        // Reverse receiver/decal source order between the two panels.
        "nodes":[{"mesh":0,"translation":[-0.65,0,0]},
                 {"mesh":1,"translation":[-0.65,0,0]},
                 {"mesh":2,"translation":[0.65,0,0]},
                 {"mesh":0,"translation":[0.65,0,0]},
                 {"mesh":3,"translation":[-0.4,-0.3,0]},
                 {"mesh":3,"translation":[0.9,-0.3,0]}],
        "scenes":[{"nodes":[0,1,2,3,4,5]}],"scene":0
    });
    std::fs::write(assets.join("geometry.bin"), bytes).unwrap();
    std::fs::write(
        assets.join("depth.gltf"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    assets
}

fn main() {
    let mut output = PathBuf::from("material-depth-probe");
    let mut legacy = false;
    let mut world_scale = 1.0_f32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().expect("output directory").into(),
            "--legacy-depth" => legacy = true,
            "--world-scale" => world_scale = args.next().expect("world scale").parse().unwrap(),
            _ => panic!("unknown argument {arg}"),
        }
    }
    assert!(world_scale.is_finite() && world_scale > 0.0);
    let placement = if world_scale == 1.0 {
        Transform::IDENTITY
    } else {
        Transform::from_xyz(2443.1328, -43.77278, -3830.6895)
            .with_rotation(Quat::from_array(
                shared::coordinates::creation_euler_to_runtime_quaternion([0.0, 0.0, 0.20000502]),
            ))
            .with_scale(Vec3::splat(world_scale))
    };
    std::fs::create_dir_all(&output).unwrap();
    let output = output.canonicalize().unwrap();
    let assets = fixtures(&output, legacy);
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: assets.to_string_lossy().into(),
                ..default()
            })
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
    )
    .add_plugins((NifMaterialPlugin, PrepassVertexAlphaPlugin))
    .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
        Duration::from_millis(16),
    ));
    let scene = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset("depth.gltf"));
    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            960,
            640,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            None,
        ));
    let result = app
        .insert_resource(Probe {
            output,
            legacy,
            placement,
            world_scale,
            start: Instant::now(),
            scene,
            target,
            spawned: false,
            view: 0,
            frames: 0,
            pending: false,
            passed: true,
            loader: serde_json::Value::Null,
            captures: vec![],
        })
        .add_systems(Update, capture)
        .run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

fn view_transform(view: usize, placement: Transform) -> Transform {
    let (distance, angle) = VIEWS[view];
    let angle = angle.to_radians();
    let local = Transform::from_xyz(distance * angle.sin(), 0.0, distance * angle.cos())
        .looking_at(Vec3::ZERO, Vec3::Y);
    Transform::from_translation(
        placement
            .compute_affine()
            .transform_point3(local.translation),
    )
    .with_rotation(placement.rotation * local.rotation)
}

type DepthBindings<'w, 's> = Query<
    'w,
    's,
    (
        &'static MeshMaterial3d<NifDepthMaterial>,
        Option<&'static NotShadowCaster>,
        Option<&'static MeshMaterial3d<StandardMaterial>>,
    ),
>;

fn capture(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    server: Res<AssetServer>,
    bindings: DepthBindings,
    materials: Res<Assets<NifDepthMaterial>>,
    mut cameras: Query<&mut Transform, With<Camera3d>>,
    mut exit: MessageWriter<AppExit>,
) {
    if probe.start.elapsed() > Duration::from_secs(90) {
        eprintln!(
            "depth probe timed out: {:?}",
            server.get_load_states(probe.scene.id())
        );
        exit.write(AppExit::error());
        return;
    }
    if !probe.spawned {
        if server
            .get_load_states(probe.scene.id())
            .is_some_and(|(state, _, _)| matches!(state, bevy::asset::LoadState::Failed(_)))
        {
            eprintln!("depth fixture failed to load");
            exit.write(AppExit::error());
            return;
        }
        if !server.is_loaded_with_dependencies(probe.scene.id()) {
            return;
        }
        commands.spawn((WorldAssetRoot(probe.scene.clone()), probe.placement));
        commands.spawn((
            Camera3d::default(),
            RenderTarget::Image(probe.target.clone().into()),
            SceneColorPipeline::default(),
            DepthPrepass,
            OcclusionCulling,
            DebandDither::Disabled,
            Msaa::Off,
            view_transform(0, probe.placement),
        ));
        probe.spawned = true;
        return;
    }
    if probe.pending {
        return;
    }
    if probe.frames == 0 {
        for mut camera in &mut cameras {
            *camera = view_transform(probe.view, probe.placement);
        }
    }
    probe.frames += 1;
    if probe.frames == 60 {
        let checks: Vec<_> = bindings
            .iter()
            .map(|(handle, no_shadow, standard)| {
                let material = materials.get(&handle.0).unwrap();
                let state = material.extension.state;
                let passed = state.depth_test
                    && !state.depth_write
                    && state.decal
                    && state.alpha_mask
                    && material.base.alpha_mode == AlphaMode::Mask(0.5)
                    && no_shadow.is_some()
                    && standard.is_none();
                serde_json::json!({"passed":passed,"depth_write":state.depth_write,
                "alpha_mode":format!("{:?}",material.base.alpha_mode),"decal":state.decal})
            })
            .collect();
        let loader_passed = if probe.legacy {
            checks.is_empty()
        } else {
            checks.len() == 2 && checks.iter().all(|c| c["passed"] == true)
        };
        probe.passed &= loader_passed;
        probe.loader = serde_json::json!({"passed":loader_passed,"bindings":checks});
        probe.pending = true;
        commands
            .spawn(Screenshot::image(probe.target.clone()))
            .observe(evaluate);
    }
}

fn evaluate(
    event: On<ScreenshotCaptured>,
    mut probe: ResMut<Probe>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera3d>>,
    adapter: Res<bevy::render::renderer::RenderAdapterInfo>,
    mut exit: MessageWriter<AppExit>,
) {
    let image = event.image.clone().try_into_dynamic().unwrap().to_rgb8();
    image
        .save(probe.output.join(format!("view-{}.png", probe.view)))
        .unwrap();
    let (camera, transform) = cameras.single().unwrap();
    let mut samples = Vec::new();
    let mut passed = true;
    for (panel, center) in [("coplanar", -0.65), ("behind", 0.65)] {
        for (name, x, y, channel) in [
            ("cutout", -0.25, 0.3, 0),
            ("decal", 0.25, 0.3, 1),
            ("occluder", 0.25, -0.3, 2),
        ] {
            let pixel = camera
                .world_to_viewport(
                    transform,
                    probe.placement.compute_affine().transform_point3(Vec3::new(
                        center + x,
                        y,
                        0.0,
                    )),
                )
                .unwrap();
            let mut colors = [0_u32; 3];
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let rgb = image
                        .get_pixel((pixel.x as i32 + dx) as u32, (pixel.y as i32 + dy) as u32)
                        .0;
                    for i in 0..3 {
                        colors[i] += u32::from(rgb[i]);
                    }
                }
            }
            let rgb = colors.map(|v| v / 9);
            let sample_passed =
                rgb[channel] > 50 && (0..3).all(|i| i == channel || rgb[channel] > rgb[i] + 30);
            passed &= sample_passed;
            samples.push(serde_json::json!({"panel":panel,"case":name,"rgb":rgb,
                "expected_channel":channel,"passed":sample_passed}));
        }
    }
    let view = probe.view;
    probe
        .captures
        .push(serde_json::json!({"view":view,"distance":VIEWS[view].0,
        "angle_degrees":VIEWS[view].1,"samples":samples,"passed":passed}));
    probe.passed &= passed;
    probe.view += 1;
    probe.frames = 0;
    probe.pending = false;
    if probe.view == VIEWS.len() {
        let report = serde_json::json!({"kind":"native-depth-decal","legacy_depth":probe.legacy,
            "occlusion_culling":true,
            "retail_parity":false,"adapter":adapter.name,"loader":probe.loader,"world_scale":probe.world_scale,
            "captures":probe.captures,"passed":probe.passed});
        std::fs::write(
            probe.output.join("probe.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        println!("depth probe passed: {}", probe.passed);
        exit.write(if probe.passed {
            AppExit::Success
        } else {
            AppExit::error()
        });
    }
}
