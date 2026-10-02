//! Converter → Bevy glTF loader → GPU emission check. No Skyrim assets required.
//! --output <directory> [--interior] [--legacy-emission] (negative control).
use bevy::{
    camera::{RenderTarget, ScalingMode},
    core_pipeline::tonemapping::DebandDither,
    gltf::Gltf,
    prelude::*,
    render::{
        RenderPlugin,
        settings::{RenderCreation, WgpuSettings, WgpuSettingsPriority},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::{ExitCondition, WindowPlugin},
};
use converter::{
    material::{
        LightingShaderType, NifAlphaMode, NifMaterialDisposition, NifShaderFamily,
        NifShapeMaterial, NifTextureSemantic, NifTextureSlot, ValidatedNifMaterial,
        publish_gltf_materials,
    },
    texture::{TextureConverter, TextureEncoding},
};
use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};
use engine::color_pipeline::SceneColorPipeline;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const GLOW: [u8; 4] = [128, 64, 255, 128];
const CASES: [(&str, [f32; 3], f32, bool); 9] = [
    ("zero", [1.0, 0.5, 0.25], 0.0, true),
    ("dim", [1.0, 0.5, 0.25], 0.25, true),
    ("unit", [1.0, 0.5, 0.25], 1.0, true),
    ("hdr", [4.0, 2.0, 0.5], 3.0, true),
    ("hdr_dim", [4.0, 2.0, 0.5], 0.125, true),
    ("black_glow", [0.0; 3], 2.0, true),
    ("untextured_hdr", [2.0, 0.5, 0.25], 2.0, false),
    ("own_emit_atlas", [0.0; 3], 0.0, false),
    ("signed_tint", [1.0, -0.25, 0.5], 0.5, true),
];

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    interior: bool,
    legacy: bool,
    started: Instant,
    gltf: Handle<Gltf>,
    materials: Vec<Handle<StandardMaterial>>,
    target: Handle<Image>,
    spawned: bool,
    frames: u32,
    requested: bool,
    loaded: Vec<serde_json::Value>,
    loader_passed: bool,
}

fn fixtures(output: &std::path::Path, legacy: bool) -> PathBuf {
    let assets = output.join("fixtures");
    std::fs::create_dir_all(assets.join("textures")).unwrap();
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: 4,
        width: 4,
        depth: None,
        format: DxgiFormat::R8G8B8A8_UNorm,
        mipmap_levels: None,
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap();
    dds.data = GLOW.repeat(16);
    let mut bytes = Vec::new();
    dds.write(&mut bytes).unwrap();
    for (name, encoding) in [
        ("glow.ktx2", TextureEncoding::NormalLinear),
        ("glow.opensky-srgb.ktx2", TextureEncoding::ColorSrgb),
    ] {
        std::fs::write(
            assets.join("textures").join(name),
            TextureConverter::convert_uncompressed(&bytes, encoding).unwrap(),
        )
        .unwrap();
    }
    let contract: Vec<_> = CASES
        .iter()
        .enumerate()
        .map(|(index, (name, color, multiple, glow))| {
            let material = ValidatedNifMaterial {
                shader_family: NifShaderFamily::Lighting,
                lighting_shader_type: Some(LightingShaderType::Default),
                shader_block: index as u32,
                texture_set_block: None,
                alpha_property_block: None,
                shader_flags_1: 1 << 22, // Own_Emit: does not imply Glow_Map.
                shader_flags_2: if *glow { 1 << 6 } else { 0 },
                base_color: [0.0, 0.0, 0.0, 1.0],
                alpha: 1.0,
                alpha_mode: NifAlphaMode::Opaque,
                alpha_threshold: None,
                glossiness: 0.0,
                specular_color: [0.0; 3],
                specular_strength: 0.0,
                emissive_color: *color,
                emissive_multiple: *multiple,
                double_sided: false,
                textures: if *glow || *name == "own_emit_atlas" {
                    vec![NifTextureSlot {
                        slot: 2,
                        semantic: if *glow {
                            NifTextureSemantic::Glow
                        } else {
                            NifTextureSemantic::Unclassified
                        },
                        path: "textures/glow.dds".into(),
                        required: false,
                    }]
                } else {
                    vec![]
                },
            };
            NifShapeMaterial {
                shape_block: index as u32,
                shape_name: Some((*name).into()),
                shader_property_block: Some(index as u32),
                alpha_property_block: None,
                disposition: NifMaterialDisposition::Validated { material },
            }
        })
        .collect();
    let mut document = serde_json::json!({"asset":{"version":"2.0"}, "meshes": CASES.iter().map(|_| serde_json::json!({"primitives":[{}]})).collect::<Vec<_>>()});
    publish_gltf_materials(
        &mut document,
        &contract,
        &(0..CASES.len() as u32).collect::<Vec<_>>(),
        std::path::Path::new("materials.glb"),
    )
    .unwrap();
    // Load the published materials on diagnostic rectangles; geometry is not under test.
    document.as_object_mut().unwrap().remove("meshes");
    if legacy {
        for (index, (_, color, multiple, glow)) in CASES.iter().enumerate() {
            let material = &mut document["materials"][index];
            let has_color = color.iter().any(|v| *v > 0.0);
            material["emissiveFactor"] = serde_json::json!(if has_color {
                color.map(|v| v.clamp(0.0, 1.0))
            } else {
                [1.0; 3]
            });
            material["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"] =
                serde_json::json!(multiple.max(1.0));
            if *glow || index == 7 {
                material["emissiveTexture"] = serde_json::json!({"index": 0});
            }
        }
        let extensions = document["extensionsUsed"].as_array_mut().unwrap();
        if !extensions
            .iter()
            .any(|v| v == "KHR_materials_emissive_strength")
        {
            extensions.push(serde_json::json!("KHR_materials_emissive_strength"));
        }
    }
    std::fs::write(
        assets.join("materials.gltf"),
        serde_json::to_vec_pretty(&document).unwrap(),
    )
    .unwrap();
    assets
}

fn main() {
    let mut output = PathBuf::from("material-emission-probe");
    let mut interior = false;
    let mut legacy = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().expect("--output requires a directory").into(),
            "--interior" => interior = true,
            "--legacy-emission" => legacy = true,
            _ => panic!("unknown option {arg}"),
        }
    }
    std::fs::create_dir_all(&output).unwrap();
    let output = output.canonicalize().unwrap();
    for name in ["probe.json", "probe.png"] {
        let path = output.join(name);
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    let assets = fixtures(&output, legacy);
    let mut app = App::new();
    app.insert_resource(GlobalAmbientLight {
        brightness: 0.0,
        ..default()
    })
    .add_plugins(
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
    .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
        Duration::from_millis(16),
    ));
    let gltf = app.world().resource::<AssetServer>().load("materials.gltf");
    // Bevy 0.19 stores GltfMaterial separately; production PBR handler publishes
    // StandardMaterial under the /std label. Exercise that handler directly.
    let loaded_materials = (0..CASES.len())
        .map(|index| {
            app.world()
                .resource::<AssetServer>()
                .load(format!("materials.gltf#Material{index}/std"))
        })
        .collect();
    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            800,
            CASES.len() as u32 * 100,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            None,
        ));
    let result = app
        .insert_resource(Probe {
            output,
            interior,
            legacy,
            started: Instant::now(),
            gltf,
            materials: loaded_materials,
            target,
            spawned: false,
            frames: 0,
            requested: false,
            loaded: vec![],
            loader_passed: true,
        })
        .add_systems(Update, render_and_capture)
        .run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

fn render_and_capture(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    gltfs: Res<Assets<Gltf>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    mut exit: MessageWriter<AppExit>,
) {
    if probe.started.elapsed() > Duration::from_secs(90) {
        eprintln!("material load/render timed out");
        exit.write(AppExit::error());
        return;
    }
    if !probe.spawned {
        let Some(gltf) = gltfs.get(&probe.gltf) else {
            return;
        };
        if probe.materials.iter().any(|h| materials.get(h).is_none()) {
            return;
        }
        if probe
            .materials
            .iter()
            .filter_map(|h| materials.get(h).unwrap().emissive_texture.as_ref())
            .any(|h| images.get(h).is_none())
        {
            return;
        }
        assert_eq!(gltf.materials.len(), CASES.len());
        let glow_rgb = Color::srgb_u8(GLOW[0], GLOW[1], GLOW[2])
            .to_linear()
            .to_f32_array();
        let quad = meshes.add(Rectangle::new(3.0, 0.8));
        for (index, (name, color, multiple, glow)) in CASES.iter().enumerate() {
            let handle = probe.materials[index].clone();
            let loaded = materials.get(&handle).unwrap();
            let energy = color.map(|v| v.max(0.0) * multiple);
            let actual = loaded.emissive.to_f32_array();
            let format = loaded
                .emissive_texture
                .as_ref()
                .map(|h| format!("{:?}", images.get(h).unwrap().texture_descriptor.format));
            let passed = actual[..3]
                .iter()
                .zip(energy)
                .all(|(a, e)| (*a - e).abs() <= 1e-5)
                && loaded.emissive_texture.is_some() == *glow
                && (!*glow || format.as_deref() == Some("Rgba8UnormSrgb"));
            probe.loader_passed &= passed;
            probe.loaded.push(serde_json::json!({"case":name,"authored_color":color,"multiple":multiple,"expected_linear":energy,"loaded_linear":actual[..3],"glow_format":format,"passed":passed}));
            let reference = materials.add(StandardMaterial {
                base_color: Color::BLACK,
                emissive: LinearRgba::rgb(
                    energy[0] * if *glow { glow_rgb[0] } else { 1.0 },
                    energy[1] * if *glow { glow_rgb[1] } else { 1.0 },
                    energy[2] * if *glow { glow_rgb[2] } else { 1.0 },
                ),
                ..default()
            });
            for (x, material) in [(-2.0, handle.clone()), (2.0, reference)] {
                commands.spawn((
                    Mesh3d(quad.clone()),
                    MeshMaterial3d(material),
                    Transform::from_xyz(x, (CASES.len() as f32 - 1.0) * 0.5 - index as f32, 0.0),
                ));
            }
        }
        commands.spawn((
            Camera3d::default(),
            RenderTarget::Image(probe.target.clone().into()),
            SceneColorPipeline::default(),
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
                    viewport_height: CASES.len() as f32,
                },
                ..OrthographicProjection::default_3d()
            }),
            Transform::from_xyz(0.0, 0.0, 10.0),
        ));
        probe.spawned = true;
    }
    probe.frames += 1;
    if probe.frames >= 90 && !probe.requested {
        probe.requested = true;
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
    let mut passed = probe.loader_passed;
    let mut samples = Vec::new();
    for (index, (name, _, _, _)) in CASES.iter().enumerate() {
        let y = 50 + index as u32 * 100;
        let mut error = 0;
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                let a = image
                    .get_pixel((200i32 + dx) as u32, (y as i32 + dy) as u32)
                    .0;
                let e = image
                    .get_pixel((600i32 + dx) as u32, (y as i32 + dy) as u32)
                    .0;
                for (a, e) in a.into_iter().zip(e) {
                    error = error.max(a.abs_diff(e));
                }
            }
        }
        let actual = image.get_pixel(200, y).0;
        let expected = image.get_pixel(600, y).0;
        passed &= error <= 2;
        // Guard against a blank render satisfying a pairwise comparison.
        passed &= if [0, 5, 7].contains(&index) {
            expected == [0; 3]
        } else {
            expected.iter().any(|v| *v > 5 && *v < 250)
        };
        samples.push(serde_json::json!({"case":name,"pixel_y":y,"converted_rgb":actual,"reference_rgb":expected,"max_error_u8":error}));
    }
    let report = serde_json::json!({"kind":"converted-material-emission","retail_parity":false,"space":if probe.interior {"interior"} else {"exterior"},"legacy_emission":probe.legacy,"ev100":9.7,"tonemapping":"TonyMcMapface","glow_rgba_u8":GLOW,"resolution":[800,CASES.len()*100],"camera":{"position":[0,0,10],"projection":"orthographic","vertical_size":CASES.len()},"ambient_brightness":0,"lights":0,"tolerance_u8":2,"loader":probe.loaded,"samples":samples,"adapter":{"name":adapter.name,"driver":adapter.driver,"driver_info":adapter.driver_info},"passed":passed});
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
