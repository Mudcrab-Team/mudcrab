//! Ignored native acceptance fixture. Execution requires separate build and Metal admission.
//! This uses the production catalog and material path, but does not exercise world streaming.

use super::*;
use crate::world::database::AssetCatalog;
use bevy::{
    app::PluginsState,
    camera::{Exposure, RenderTarget, ScalingMode},
    core_pipeline::tonemapping::Tonemapping,
    gltf::GltfAssetLabel,
    image::{ImageAddressMode, ImageSampler},
    render::{
        RenderApp, RenderPlugin,
        pipelined_rendering::PipelinedRenderingPlugin,
        render_resource::{PipelineCache, TextureFormat},
        renderer::RenderAdapterInfo,
        settings::{Backends, RenderCreation, WgpuFeatures, WgpuSettings},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::ExitCondition,
    winit::WinitPlugin,
    world_serialization::{WorldAsset, WorldAssetRoot},
};
use converter::{
    esm::{
        exporter::{create_tables, export_to_db},
        records::RawRecord,
    },
    mesh::MeshConverter,
    texture::{TextureConverter, TextureEncoding},
};
use dummy_content::{dds, nif, rng::Rng};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

const WIDTH: u32 = 512;
const HEIGHT: u32 = 256;
const SIDE: u32 = 192;
const FRAME_LIMIT: u32 = 32;
// Screen origin is top-left. Each probe is a 3 x 3 region centered at this pixel.
// The geometry projects A to [32,224) x [32,224), B to [288,480) x [32,224).
const PROBES: [(u32, u32, [bool; 3]); 8] = [
    (44, 56, [true, false, false]),
    (92, 56, [false, true, false]),
    (68, 56, [false, true, false]),
    (140, 56, [true, false, false]),
    (188, 56, [false, true, false]),
    (44, 104, [false, false, true]),
    (92, 104, [true, true, false]),
    (44, 200, [false, false, true]),
];

fn write_json(path: &Path, value: &serde_json::Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn record(id: u32, tag: &[u8; 4], fields: Vec<(&[u8; 4], Vec<u8>)>) -> RawRecord {
    RawRecord {
        form_id: id,
        record_type: *tag,
        flags: 0,
        subrecords: fields
            .into_iter()
            .map(|(tag, bytes)| (tag.to_vec(), bytes))
            .collect(),
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    }
}

fn base(id: u32, texture: Option<u32>) -> RawRecord {
    let mut fields = vec![(b"MODL", b"meshes/shared.nif\0".to_vec())];
    if let Some(texture) = texture {
        fields.push((
            b"MODS",
            [
                1u32.to_le_bytes().as_slice(),
                &4u32.to_le_bytes(),
                b"Rock",
                &texture.to_le_bytes(),
                &0u32.to_le_bytes(),
            ]
            .concat(),
        ));
    }
    record(id, b"STAT", fields)
}

fn catalog(path: &Path, case: usize) -> AssetCatalog {
    let connection = Connection::open(path).unwrap();
    create_tables(&connection).unwrap();
    let mut records: HashMap<_, _> = [
        base(
            1,
            match case {
                0 => Some(101),
                1 => Some(103),
                _ => None,
            },
        ),
        base(2, Some(if case == 2 { 0xDEAD } else { 102 })),
    ]
    .into_iter()
    .map(|record| (record.form_id, record))
    .collect();
    for (id, name) in [(101, "red"), (102, "blue"), (103, "pattern")] {
        records.insert(
            id,
            record(
                id,
                b"TXST",
                vec![(b"TX00", format!("textures/{name}.dds\0").into_bytes())],
            ),
        );
    }
    export_to_db(&connection, &records).unwrap();
    drop(connection);
    AssetCatalog::open(path).unwrap()
}

/// Exact opaque BC1 blocks. Both endpoints are equal and every index selects endpoint zero.
fn texture(directory: &Path, name: &str, width: u32, colors: &[u16]) {
    let generated = dds::generate(
        &dds::Spec::new(dds::Format::Bc1Unorm, width, width),
        &mut Rng::new(0),
    )
    .unwrap();
    let mut dds = ddsfile::Dds::read(&mut std::io::Cursor::new(generated)).unwrap();
    dds.data = colors
        .iter()
        .flat_map(|color| {
            [
                color.to_le_bytes().as_slice(),
                &color.to_le_bytes(),
                &[0; 4],
            ]
            .concat()
        })
        .collect();
    let mut bytes = Vec::new();
    dds.write(&mut bytes).unwrap();
    fs::write(directory.join(format!("textures/{name}.dds")), &bytes).unwrap();
    let converted =
        TextureConverter::convert_uncompressed(&bytes, TextureEncoding::ColorSrgb).unwrap();
    fs::write(directory.join(format!("textures/{name}.ktx2")), converted).unwrap();
}

fn fixture(directory: &Path) {
    fs::create_dir(directory.join("meshes")).unwrap();
    fs::create_dir(directory.join("textures")).unwrap();
    // Creation -> runtime rotates (x,y,z) to (x,z,-y). The quad faces the +Z camera.
    let bytes = nif::static_shape_with_material_sampling(
        &nif::StaticShape {
            name: "Rock",
            positions: &[[0., 0., 0.], [1.5, 0., 0.], [1.5, 0., 1.5], [0., 0., 1.5]],
            normals: &[[0., -1., 0.]; 4],
            uvs: &[[0., 1.], [1., 1.], [1., 0.], [0., 0.]],
            indices: &[[0, 1, 2], [0, 2, 3]],
            diffuse: "textures/original.dds",
            normal_texture: "",
        },
        (1 << 31) | (1 << 9),
        1,
        nif::StaticSampling {
            offset: [0.25, 0.25],
            scale: [2., 1.],
            clamp_mode: 2,
        },
    )
    .unwrap();
    let input = directory.join("shared.nif");
    fs::write(&input, bytes).unwrap();
    MeshConverter::convert_nif_to_glb(&input, &directory.join("meshes/shared.glb")).unwrap();
    texture(directory, "red", 4, &[0xF800]);
    texture(directory, "blue", 4, &[0x001F]);
    texture(directory, "original", 4, &[0x8410]);
    texture(directory, "pattern", 8, &[0xF800, 0x07E0, 0x001F, 0xFFE0]);
    let original = fs::read(directory.join("textures/original.ktx2")).unwrap();
    for uri in MeshConverter::glb_texture_uris(&directory.join("meshes/shared.glb")).unwrap() {
        assert!(uri.starts_with("../textures/original."));
        let target = directory.join("meshes").join(uri);
        if !target.exists() {
            fs::write(target, &original).unwrap();
        }
    }
}

fn native_app(directory: &Path) -> App {
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: directory.to_str().unwrap().into(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    backends: Some(Backends::METAL),
                    features: WgpuFeatures::TEXTURE_COMPRESSION_BC
                        | WgpuSettings::default().features,
                    ..default()
                })),
                synchronous_pipeline_compilation: true,
                ..default()
            })
            .disable::<WinitPlugin>()
            .disable::<bevy::audio::AudioPlugin>()
            .disable::<PipelinedRenderingPlugin>(),
    )
    .add_plugins((crate::nif_material::NifMaterialPlugin, ObjectMaterialPlugin))
    .insert_resource(crate::visual_profile::VisualProfile::Mudcrab)
    .insert_resource(ClearColor(Color::BLACK))
    .insert_resource(GlobalAmbientLight::NONE);
    let deadline = Instant::now() + Duration::from_secs(15);
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
    assert_eq!(
        app.world().resource::<RenderAdapterInfo>().backend,
        wgpu::Backend::Metal
    );
    app
}

fn surface(app: &App, root: Entity) -> Option<(Handle<Mesh>, Handle<StandardMaterial>)> {
    let mut entities = vec![root];
    while let Some(entity) = entities.pop() {
        if let Some(material) = app.world().get::<MeshMaterial3d<StandardMaterial>>(entity) {
            return Some((
                app.world().get::<Mesh3d>(entity)?.0.clone(),
                material.0.clone(),
            ));
        }
        if let Some(children) = app.world().get::<Children>(entity) {
            entities.extend(children.iter());
        }
    }
    None
}

fn state_ready(app: &App, roots: [Entity; 2], case: usize) -> bool {
    for (index, root) in roots.into_iter().enumerate() {
        if surface(app, root).is_none() {
            return false;
        }
        let state = app.world().get::<ObjectTextureSwapState>(root);
        if case < 2 {
            let expected = if index == 1 {
                102
            } else if case == 0 {
                101
            } else {
                103
            };
            match state {
                Some(ObjectTextureSwapState::Applied { texture_set }) => {
                    assert_eq!(*texture_set, expected)
                }
                None | Some(ObjectTextureSwapState::Pending) => return false,
                other => panic!("unexpected swap state: {other:?}"),
            }
        } else if index == 0 {
            assert!(
                state.is_none(),
                "no-MODS control must have no substitution state"
            );
        } else {
            match state {
                Some(ObjectTextureSwapState::Unsupported(reason)) => {
                    assert!(reason.contains("missing_texture_set"))
                }
                None | Some(ObjectTextureSwapState::Pending) => return false,
                other => panic!("unexpected missing-link control: {other:?}"),
            }
        }
    }
    true
}

fn pixel(image: &::image::RgbImage, x: u32, y: u32) -> [f32; 3] {
    let mut value = [0.; 3];
    for dy in -1..=1 {
        for dx in -1..=1 {
            let pixel = image.get_pixel((x as i32 + dx) as u32, (y as i32 + dy) as u32);
            for channel in 0..3 {
                value[channel] += f32::from(pixel[channel]) / 9.;
            }
        }
    }
    value
}

fn expect_color(pixel: [f32; 3], high: [bool; 3]) {
    let signal = pixel
        .into_iter()
        .zip(high)
        .filter_map(|(v, h)| h.then_some(v))
        .fold(255., f32::min);
    assert!(signal >= 48., "dark or missing surface: {pixel:?}");
    for (value, high) in pixel.into_iter().zip(high) {
        if !high {
            assert!(
                value <= signal * 0.45 && signal - value >= 32.,
                "wrong color: {pixel:?}"
            );
        }
    }
    if high[0] && high[1] {
        assert!(
            (pixel[0] - pixel[1]).abs() <= signal * 0.25,
            "wrong yellow: {pixel:?}"
        );
    }
}

fn compare_region(a: &::image::RgbImage, b: &::image::RgbImage, shift: u32) {
    for y in 48..208 {
        for x in 304..464 {
            for channel in 0..3 {
                assert!(
                    a.get_pixel(x, y)[channel].abs_diff(b.get_pixel(x - shift, y)[channel]) <= 3,
                    "control changed at {x},{y}, channel {channel}"
                );
            }
        }
    }
}

fn verify_oracle_math() {
    // Probe centers and the complete 3x3 footprint must agree with the predeclared block.
    let classify = |u: f32, v: f32, variant: usize| {
        let (mut u, mut v) = if variant == 1 {
            (u, v)
        } else {
            (2. * u + 0.25, v + 0.25)
        };
        u = if variant == 2 {
            u.clamp(0., 1.)
        } else {
            u.fract()
        };
        v = if variant == 3 {
            v.fract()
        } else {
            v.clamp(0., 1.)
        };
        match (u >= 0.5, v >= 0.5) {
            (false, false) => [true, false, false],
            (true, false) => [false, true, false],
            (false, true) => [false, false, true],
            (true, true) => [true, true, false],
        }
    };
    for (x, y, expected) in PROBES {
        for dx in -1..=1 {
            for dy in -1..=1 {
                let u = (x as f32 + dx as f32 + 0.5 - 32.) / SIDE as f32;
                let v = (y as f32 + dy as f32 + 0.5 - 32.) / SIDE as f32;
                assert_eq!(classify(u, v, 0), expected);
            }
        }
    }
    for variant in 1..=3 {
        assert!(
            PROBES.iter().any(|&(x, y, expected)| classify(
                (x as f32 + 0.5 - 32.) / SIDE as f32,
                (y as f32 + 0.5 - 32.) / SIDE as f32,
                variant
            ) != expected),
            "oracle cannot reject negative control {variant}"
        );
    }
}

#[test]
#[ignore = "requires separate managed build and bounded Metal admission via object_materials_oracle.py"]
fn object_diffuse_two_instance_readback() {
    assert_eq!(
        std::env::var("MUDCRAB_OBJECT_NATIVE_ADMITTED").as_deref(),
        Ok("r1")
    );
    let run =
        PathBuf::from(std::env::var_os("MUDCRAB_OBJECT_MATERIAL_RUN").expect("watchdog run path"));
    verify_oracle_math();
    let assets = run.join("fixture");
    fs::create_dir(&assets).unwrap();
    fixture(&assets);
    let catalogs: Vec<_> = (0..3)
        .map(|case| catalog(&assets.join(format!("case-{case}.db")), case))
        .collect();
    let mut files = serde_json::Map::new();
    for directory in [&assets, &assets.join("textures"), &assets.join("meshes")] {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                files.insert(
                    path.strip_prefix(&assets)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    serde_json::json!(format!("{:x}", Sha256::digest(fs::read(&path).unwrap()))),
                );
            }
        }
    }
    write_json(
        &run.join("fixture-manifest.json"),
        &serde_json::json!({
        "files": files, "reader": "synthetic RawRecord -> production legacy SQL exporter -> AssetCatalog",
        "converter_head": std::env::var("MUDCRAB_OBJECT_SOURCE_HEAD").unwrap(),
        "source_packet_sha256": std::env::var("MUDCRAB_OBJECT_PACKET_SHA256").unwrap(),
            "plugin_order": [], "schema": "world schema unchanged; MODS optional projection v5",
            "profile": "mudcrab", "encoding": "BC1 ColorSrgb, one mip", "sampling": {"offset":[0.25,0.25],"scale":[2,1],"clamp_mode":2},
            "probes": PROBES, "low_channel_ratio_max":0.45, "min_signal":48, "min_channel_gap":32,
            "control_max_byte_difference":3, "settled_frame_max_byte_difference":2,
            "gray_channel_spread_max":12, "gray_range":[32,245], "gpu_allocation":"unmeasured"
        }),
    );
    let mut app = native_app(&assets);
    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            WIDTH,
            HEIGHT,
            TextureFormat::Rgba8UnormSrgb,
            None,
        ));
    app.world_mut().spawn((
        Camera3d::default(),
        RenderTarget::Image(target.clone().into()),
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: 2.,
            },
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0., 0., 10.).looking_at(Vec3::ZERO, Vec3::Y),
        Msaa::Off,
        Tonemapping::None,
        Exposure { ev100: 12. },
    ));
    app.world_mut().spawn((
        DirectionalLight {
            illuminance: 10_000.,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(0., 0., 4.).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    let scene: Handle<WorldAsset> = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset("meshes/shared.glb"));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut loaded = false;
    for _ in 0..256 {
        app.update();
        let server = app.world().resource::<AssetServer>();
        assert!(
            !matches!(server.load_state(scene.id()), LoadState::Failed(_)),
            "scene failed"
        );
        if server.is_loaded_with_dependencies(scene.id()) {
            loaded = true;
            break;
        }
        assert!(Instant::now() < deadline, "scene load timeout");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(loaded, "scene dependency frame bound exceeded");
    let mut captures: Vec<Vec<::image::RgbImage>> = Vec::new();
    let mut identities = Vec::new();
    let mut shared_mesh = None;
    for (case, catalog) in catalogs.iter().enumerate() {
        let roots = [-1.75, 0.25].map(|x| {
            app.world_mut()
                .spawn((
                    WorldAssetRoot(scene.clone()),
                    Transform::from_xyz(x, -0.75, 0.),
                    Visibility::default(),
                ))
                .id()
        });
        for (base, root) in (1..=2).zip(roots) {
            if let Some(input) = catalog.object_texture_swaps(base) {
                app.world_mut().entity_mut(root).insert(input);
            }
        }
        let (sender, receiver) = mpsc::channel::<Image>();
        let mut requested = 0;
        let mut frames = Vec::new();
        let mut frame_numbers = Vec::new();
        let mut requested_frames = Vec::new();
        let mut ready_frames = 0;
        for frame in 0..FRAME_LIMIT {
            app.update();
            let ready = state_ready(&app, roots, case)
                && app
                    .sub_app(RenderApp)
                    .world()
                    .resource::<PipelineCache>()
                    .waiting_pipelines()
                    .next()
                    .is_none();
            ready_frames = if ready { ready_frames + 1 } else { 0 };
            while let Ok(image) = receiver.try_recv() {
                let rgb = image.try_into_dynamic().unwrap().to_rgb8();
                assert_eq!(rgb.dimensions(), (WIDTH, HEIGHT));
                rgb.save(run.join(format!("case-{case}-frame-{}.png", frames.len())))
                    .unwrap();
                frames.push(rgb);
                frame_numbers.push(frame);
            }
            if frames.len() == 2 {
                break;
            }
            if ready_frames >= 6 && requested == frames.len() && requested < 2 {
                let sender = sender.clone();
                app.world_mut()
                    .spawn(Screenshot::image(target.clone()))
                    .observe(move |event: On<ScreenshotCaptured>| {
                        sender.send(event.image.clone()).unwrap();
                    });
                requested += 1;
                requested_frames.push(frame);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        assert_eq!(
            frames.len(),
            2,
            "case {case} exceeded capture/settling frame bound"
        );
        for (a, b) in frames[0].as_raw().iter().zip(frames[1].as_raw()) {
            assert!(a.abs_diff(*b) <= 2, "unstable capture");
        }
        let a = surface(&app, roots[0]).unwrap();
        let b = surface(&app, roots[1]).unwrap();
        assert_eq!(a.0, b.0, "mesh must be shared");
        if let Some(mesh) = &shared_mesh {
            assert_eq!(
                &a.0, mesh,
                "authored intervention must preserve mesh identity"
            );
        } else {
            shared_mesh = Some(a.0.clone());
        }
        if case < 2 {
            assert_ne!(a.1, b.1);
        } else {
            assert_eq!(a.1, b.1);
        }
        let materials = app.world().resource::<Assets<StandardMaterial>>();
        for handle in [&a.1, &b.1] {
            let material = materials.get(handle).unwrap();
            assert_eq!(
                material.uv_transform,
                bevy::math::Affine2::from_scale_angle_translation(
                    Vec2::new(2., 1.),
                    0.,
                    Vec2::splat(0.25)
                )
            );
            let images = app.world().resource::<Assets<Image>>();
            let image = images
                .get(material.base_color_texture.as_ref().unwrap())
                .unwrap();
            let ImageSampler::Descriptor(sampler) = &image.sampler else {
                panic!("missing source sampler");
            };
            assert_eq!(sampler.address_mode_u, ImageAddressMode::Repeat);
            assert_eq!(sampler.address_mode_v, ImageAddressMode::ClampToEdge);
        }
        identities.push(serde_json::json!({"case":case,"scene":format!("{:?}",scene.id()),
            "mesh_a":format!("{:?}",a.0.id()),"mesh_b":format!("{:?}",b.0.id()),
            "material_a":format!("{:?}",a.1.id()),"material_b":format!("{:?}",b.1.id()),
            "readback_delivery_case_frames":frame_numbers,"requested_after_case_frames":requested_frames,
            "state_a":format!("{:?}",app.world().get::<ObjectTextureSwapState>(roots[0])),
            "state_b":format!("{:?}",app.world().get::<ObjectTextureSwapState>(roots[1])),
            "probe_rgb":PROBES.iter().map(|&(x,y,_)| pixel(&frames[0],x,y)).collect::<Vec<_>>() }));
        write_json(
            &run.join(format!("case-{case}-receipt.json")),
            identities.last().unwrap(),
        );
        captures.push(frames);
        for root in roots {
            app.world_mut().despawn(root);
        }
    }
    for image in &captures[0] {
        expect_color(pixel(image, 128, 128), [true, false, false]);
        expect_color(pixel(image, 384, 128), [false, false, true]);
    }
    for image in &captures[1] {
        for (x, y, color) in PROBES {
            expect_color(pixel(image, x, y), color);
        }
    }
    for ((baseline, intervention), control) in
        captures[0].iter().zip(&captures[1]).zip(&captures[2])
    {
        compare_region(baseline, intervention, 0);
        compare_region(control, control, 256);
        for x in [128, 384] {
            let rgb = pixel(control, x, 128);
            let min = rgb.into_iter().fold(255., f32::min);
            let max = rgb.into_iter().fold(0., f32::max);
            assert!(
                min >= 32. && max <= 245. && max - min <= 12.,
                "invalid gray control: {rgb:?}"
            );
        }
    }
    write_json(
        &run.join("native-result.json"),
        &serde_json::json!({"passed":true,"cases":identities,
        "adapter":{"name":app.world().resource::<RenderAdapterInfo>().name,
            "backend":format!("{:?}",app.world().resource::<RenderAdapterInfo>().backend)},"gpu_allocation":"unmeasured",
        "limits":"2 instances; one quad; 3 cases; 2 captures/case; 32 settling frames/case",
        "not_verified":["world streaming","snow/MATO","Skyrim shader parity","performance","subjective appearance"]}),
    );
}
