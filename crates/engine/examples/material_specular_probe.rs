//! Converter → Bevy glTF loader → GPU specular check. No Skyrim assets required.
//! --output <directory> [--interior] [--legacy-specular] (negative control).
use bevy::{
    camera::{RenderTarget, ScalingMode},
    core_pipeline::tonemapping::DebandDither,
    gltf::{GltfAssetLabel, GltfMaterialName},
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
use engine::{color_pipeline::SceneColorPipeline, nif_material::NifMaterialPlugin};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

// Name, exponent, source strength, Specular flag, normal-alpha mask.
const CASES: [(&str, f32, f32, bool, Option<u8>); 9] = [
    ("disabled", 80.0, 1.0, false, Some(255)),
    ("zero_strength", 80.0, 0.0, true, Some(255)),
    ("unmasked", 80.0, 1.0, true, None),
    ("mask_full", 80.0, 1.0, true, Some(255)),
    ("mask_thatch", 80.0, 1.0, true, Some(7)),
    ("mask_zero", 80.0, 1.0, true, Some(0)),
    ("gloss_100", 100.0, 1.0, true, Some(255)),
    ("gloss_200", 200.0, 1.0, true, Some(255)),
    ("dim_strength", 80.0, 0.25, true, Some(128)),
];

fn reference_roughness(exponent: f32) -> f32 {
    match exponent as u32 {
        80 => 0.395_188_28,
        100 => 0.374_203_18,
        200 => 0.315_442_1,
        _ => unreachable!(),
    }
}

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    interior: bool,
    legacy: bool,
    started: Instant,
    scene: Handle<WorldAsset>,
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
    for (index, (_, _, _, _, alpha)) in CASES.iter().enumerate() {
        let Some(alpha) = alpha else {
            continue;
        };
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
        dds.data = [128, 128, 255, *alpha].repeat(16);
        let mut bytes = Vec::new();
        dds.write(&mut bytes).unwrap();
        std::fs::write(
            assets.join(format!("textures/mask{index}_n.ktx2")),
            TextureConverter::convert_uncompressed(&bytes, TextureEncoding::NormalLinear).unwrap(),
        )
        .unwrap();
    }
    let contract: Vec<_> = CASES
        .iter()
        .enumerate()
        .map(|(index, (name, exponent, strength, enabled, mask))| {
            let material = ValidatedNifMaterial {
                uv_offset: [0.0; 2],
                uv_scale: [1.0; 2],
                texture_clamp_mode: 3,
                shader_family: NifShaderFamily::Lighting,
                lighting_shader_type: Some(LightingShaderType::Default),
                shader_block: index as u32,
                texture_set_block: None,
                alpha_property_block: None,
                shader_flags_1: u32::from(*enabled),
                shader_flags_2: 0,
                base_color: [0.0, 0.0, 0.0, 1.0],
                alpha: 1.0,
                alpha_mode: NifAlphaMode::Opaque,
                alpha_threshold: None,
                glossiness: *exponent,
                specular_color: [1.0; 3],
                specular_strength: *strength,
                emissive_color: [0.0; 3],
                emissive_multiple: 0.0,
                double_sided: false,
                textures: if mask.is_some() {
                    vec![NifTextureSlot {
                        slot: 1,
                        semantic: NifTextureSemantic::Normal,
                        path: format!("textures/mask{index}_n.dds"),
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
    // A minimal indexed triangle also exercises the production scene-material binding.
    let mut geometry = Vec::new();
    for values in [
        vec![0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        vec![0.0_f32, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
        vec![0.0_f32, 0.0, 1.0, 0.0, 0.0, 1.0],
    ] {
        for value in values {
            geometry.extend_from_slice(&value.to_le_bytes());
        }
    }
    for index in [0_u16, 1, 2] {
        geometry.extend_from_slice(&index.to_le_bytes());
    }
    std::fs::write(assets.join("triangle.bin"), &geometry).unwrap();
    document["buffers"] = serde_json::json!([{"uri":"triangle.bin","byteLength":geometry.len()}]);
    document["bufferViews"] = serde_json::json!([
        {"buffer":0,"byteOffset":0,"byteLength":36},
        {"buffer":0,"byteOffset":36,"byteLength":36},
        {"buffer":0,"byteOffset":72,"byteLength":24},
        {"buffer":0,"byteOffset":96,"byteLength":6}
    ]);
    document["accessors"] = serde_json::json!([
        {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
        {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
        {"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"},
        {"bufferView":3,"componentType":5123,"count":3,"type":"SCALAR"}
    ]);
    for index in 0..CASES.len() {
        document["meshes"][index]["primitives"][0] = serde_json::json!({
            "attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"indices":3,"material":index
        });
        document["materials"][index]["name"] = serde_json::json!(CASES[index].0);
    }
    let mut nodes: Vec<_> = (0..CASES.len())
        .map(|i| serde_json::json!({"mesh":i}))
        .collect();
    // Bevy generates inverted-scale material variants inside the scene load context.
    let mask_thatch = CASES
        .iter()
        .position(|case| case.0 == "mask_thatch")
        .expect("inverted-scale probe requires mask_thatch");
    nodes.push(serde_json::json!({"mesh":mask_thatch,"scale":[-1,1,1]}));
    document["nodes"] = serde_json::json!(nodes);
    document["scenes"] = serde_json::json!([{"nodes":(0..nodes.len()).collect::<Vec<_>>() }]);
    document["scene"] = serde_json::json!(0);
    if legacy {
        for (index, (_, exponent, strength, _, _)) in CASES.iter().enumerate() {
            let material = &mut document["materials"][index];
            material["pbrMetallicRoughness"]["roughnessFactor"] =
                serde_json::json!(1.0 - (exponent / 100.0).clamp(0.0, 1.0));
            material["extras"]["openSkyrim"]
                .as_object_mut()
                .unwrap()
                .remove("specularMask");
            let extensions = material["extensions"].as_object_mut().unwrap();
            if *strength == 0.0 {
                extensions.remove("KHR_materials_specular");
            } else {
                extensions.insert("KHR_materials_specular".into(),serde_json::json!({
                    "specularFactor": strength.clamp(0.0,1.0),"specularColorFactor":[1.0,1.0,1.0]
                }));
            }
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
    let mut output = PathBuf::from("material-specular-probe");
    let mut interior = false;
    let mut legacy = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().expect("--output requires a directory").into(),
            "--interior" => interior = true,
            "--legacy-specular" => legacy = true,
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
    .add_plugins(NifMaterialPlugin)
    .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
        Duration::from_millis(16),
    ));
    // Match production: retain only the scene label, not the entire glTF root.
    let scene = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset("materials.gltf"));
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
            scene,
            materials: vec![],
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
    scene_assets: (Res<AssetServer>, ResMut<Assets<WorldAsset>>),
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    mut exit: MessageWriter<AppExit>,
) {
    let (asset_server, mut scenes) = scene_assets;
    if probe.started.elapsed() > Duration::from_secs(90) {
        eprintln!(
            "material load/render timed out; scene states: {:?}",
            asset_server.get_load_states(probe.scene.id())
        );
        exit.write(AppExit::error());
        return;
    }
    if !probe.spawned {
        if probe.materials.is_empty() {
            let Some(mut scene) = scenes.get_mut(&probe.scene) else {
                return;
            };
            let mut query = scene
                .world
                .query::<(&GltfMaterialName, &MeshMaterial3d<StandardMaterial>)>();
            let mut handles: Vec<_> = query
                .iter(&scene.world)
                .filter(|(_, material)| {
                    !asset_server
                        .get_path(material.id())
                        .is_some_and(|path| path.label().unwrap_or("").contains(" (inverted)"))
                })
                .map(|(name, material)| {
                    let index = CASES.iter().position(|case| case.0 == name.0).unwrap();
                    (index, material.0.clone())
                })
                .collect();
            handles.sort_by_key(|(index, _)| *index);
            assert_eq!(handles.len(), CASES.len());
            probe.materials = handles.into_iter().map(|(_, handle)| handle).collect();
        }
        if !asset_server.is_loaded_with_dependencies(probe.scene.id()) {
            return;
        }
        if probe.materials.iter().any(|h| materials.get(h).is_none()) {
            return;
        }
        if probe
            .materials
            .iter()
            .filter_map(|h| materials.get(h).unwrap().normal_map_texture.as_ref())
            .any(|h| images.get(h).is_none())
        {
            return;
        }
        let Some(mut scene) = scenes.get_mut(&probe.scene) else {
            return;
        };
        let mut query = scene
            .world
            .query::<(&GltfMaterialName, &MeshMaterial3d<StandardMaterial>)>();
        let bindings: Vec<_> = query
            .iter(&scene.world)
            .map(|(name, material)| {
                let index = CASES.iter().position(|case| case.0 == name.0).unwrap();
                let (_, _, strength, enabled, mask) = CASES[index];
                let suffix = if !probe.legacy && enabled && strength > 0.0 && mask.is_some() {
                    "nif"
                } else {
                    "std"
                };
                let label = format!("Material{index}/{suffix}");
                let actual = asset_server.get_path(material.id());
                let inverted = format!("Material{index} (inverted)/{suffix}");
                let actual_label = actual.as_ref().and_then(|path| path.label());
                let passed =
                    actual_label == Some(label.as_str()) || actual_label == Some(inverted.as_str());
                serde_json::json!({"case":name.0,"label":actual_label,"passed":passed})
            })
            .collect();
        probe.loader_passed &=
            bindings.len() == CASES.len() + 1 && bindings.iter().all(|b| b["passed"] == true);
        probe
            .loaded
            .push(serde_json::json!({"scene_bindings":bindings}));
        let quad = meshes.add(Rectangle::new(3.0, 0.8));
        for (index, (name, exponent, strength, enabled, mask)) in CASES.iter().enumerate() {
            let handle = probe.materials[index].clone();
            let loaded = materials.get(&handle).unwrap();
            let source_strength = if *enabled { *strength } else { 0.0 };
            let has_mask = source_strength > 0.0 && mask.is_some();
            let expected_loaded = source_strength * if has_mask { 1.0 } else { 0.5 };
            let format = loaded
                .normal_map_texture
                .as_ref()
                .map(|h| format!("{:?}", images.get(h).unwrap().texture_descriptor.format));
            let shared = !has_mask || loaded.specular_texture == loaded.normal_map_texture;
            let passed = (loaded.reflectance - expected_loaded).abs() <= 1e-6
                && (loaded.perceptual_roughness - reference_roughness(*exponent)).abs() <= 1e-5
                && loaded.specular_texture.is_some() == has_mask
                && shared
                && (mask.is_none() || format.as_deref() == Some("Rgba8Unorm"));
            probe.loader_passed &= passed;
            probe.loaded.push(serde_json::json!({
                "case":name,"exponent":exponent,"strength":strength,"enabled":enabled,
                "mask_alpha_u8":mask,"expected_loaded_reflectance":expected_loaded,
                "loaded_reflectance":loaded.reflectance,"loaded_roughness":loaded.perceptual_roughness,
                "linear_normal_format":format,"shared_mask":shared,"passed":passed
            }));
            // Same normal sample, independently computed scalar response; no specular texture.
            let normal_map_texture = loaded.normal_map_texture.clone();
            let reference = materials.add(StandardMaterial {
                base_color: Color::BLACK,
                reflectance: 0.5 * source_strength * mask.map_or(1.0, |a| a as f32 / 255.0),
                perceptual_roughness: reference_roughness(*exponent),
                normal_map_texture,
                flip_normal_map_y: true,
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
            DirectionalLight {
                illuminance: 5000.0,
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_translation(Vec3::Z * 10.0).looking_at(Vec3::ZERO, Vec3::Y),
        ));
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
        // The scene must stay fully loaded after unused labeled assets are released.
        if !asset_server.is_loaded_with_dependencies(probe.scene.id()) {
            return;
        }
        probe.loaded.push(serde_json::json!({
            "scene_dependencies_still_loaded": true,
            "retained_root": "Scene0"
        }));
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
    for (index, (name, _, _, _, _)) in CASES.iter().enumerate() {
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
        passed &= if [0, 1, 5].contains(&index) {
            expected == [0; 3]
        } else if index == 4 {
            // Very low mask energy can quantize to black; bright cases guard blank renders.
            expected.iter().all(|v| *v <= 10)
        } else {
            expected.iter().any(|v| *v > 5 && *v < 250)
        };
        samples.push(serde_json::json!({"case":name,"pixel_y":y,"converted_rgb":actual,"reference_rgb":expected,"max_error_u8":error}));
    }
    let report = serde_json::json!({"kind":"converted-material-specular","retail_parity":false,"space":if probe.interior {"interior"} else {"exterior"},"legacy_specular":probe.legacy,"ev100":9.7,"tonemapping":"TonyMcMapface","resolution":[800,CASES.len()*100],"camera":{"position":[0,0,10],"projection":"orthographic","vertical_size":CASES.len()},"ambient_brightness":0,"directional_illuminance":5000,"tolerance_u8":2,"loader":probe.loaded,"samples":samples,"adapter":{"name":adapter.name,"driver":adapter.driver,"driver_info":adapter.driver_info},"passed":passed});
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
