//! GPU probe for the colour-texture transfer function.
//!
//! The `skyrim` profile samples authored colour bytes raw, while `mudcrab` samples them through
//! an sRGB view. This builds synthetic DDS fixtures, publishes them as KTX2 through the same
//! converter the pipeline uses, and reads the sampled texel back from a float render target.
//! `--output <directory> [--profile mudcrab|skyrim]`

use bevy::{
    app::ScheduleRunnerPlugin,
    asset::{AssetServer, LoadState},
    camera::{CompositingSpace, RenderTarget, ScalingMode},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    image::{
        ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler,
        ImageSamplerDescriptor,
    },
    prelude::*,
    render::{
        RenderPlugin,
        render_resource::{TextureFormat, TextureUsages},
        renderer::{RenderAdapter, RenderAdapterInfo},
        settings::{RenderCreation, WgpuFeatures, WgpuSettings, WgpuSettingsPriority},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::{ExitCondition, WindowPlugin},
};
use converter::texture::{TextureConverter, TextureEncoding};
use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};
use engine::{raw_texel::RawTexelPlugin, visual_profile::VisualProfile};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const TARGET_WIDTH: u32 = 192;
const TARGET_HEIGHT: u32 = 128;
const PIXELS_PER_WORLD: f32 = 32.0;
// 40 px across an 8-texel fixture: 5 px per texel puts every texel centre on a pixel centre.
const HIGH_QUAD: f32 = 1.25;
// 4 px across the same fixture: half its texel count, so the quad minifies by two and selects mip 1.
const LOW_QUAD: f32 = 0.125;
const TOLERANCE: f32 = 1.0 / 255.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    NativeBc,
    Rgba8,
    MipChain,
    WrapAlias,
}

impl Group {
    fn label(self) -> &'static str {
        match self {
            Self::NativeBc => "native_bc",
            Self::Rgba8 => "rgba8",
            Self::MipChain => "mip_chain",
            Self::WrapAlias => "wrap_alias",
        }
    }

    fn is_verdict(self) -> bool {
        self == Self::NativeBc
    }
}

struct FixtureSpec {
    name: &'static str,
    group: Group,
    format: DxgiFormat,
    width: u32,
    height: u32,
    mip_colours: Vec<[u8; 4]>,
    alias: bool,
}

struct Fixture {
    name: &'static str,
    group: Group,
    path0: String,
    path1: String,
    red: Vec<u8>,
}

struct LoadedCase {
    name: &'static str,
    group: Group,
    mip0: Handle<Image>,
    mip1: Handle<Image>,
    red: Vec<u8>,
}

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    profile: VisualProfile,
    started: Instant,
    target: Handle<Image>,
    cases: Vec<LoadedCase>,
    spawned: bool,
    frames: u32,
    requested: bool,
}

fn main() {
    let mut output = PathBuf::from("material-texel-probe");
    let mut profile = VisualProfile::Mudcrab;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().expect("--output requires a directory").into(),
            "--profile" => {
                let value = args.next().expect("--profile requires a name");
                profile = VisualProfile::parse(&value)
                    .unwrap_or_else(|| panic!("unknown visual profile {value:?}"));
            }
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
    let assets = output.join("fixtures");
    let fixtures = build_fixtures(&assets);
    let mut app = App::new();
    app.insert_resource(profile)
        .insert_resource(FixtureList(fixtures))
        .insert_resource(ProbeOutput(output))
        .add_plugins(
            DefaultPlugins
                .set(AssetPlugin {
                    file_path: assets.to_string_lossy().into(),
                    ..default()
                })
                .set(RenderPlugin {
                    render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                        priority: WgpuSettingsPriority::WebGPU,
                        // WebGPU's default feature set omits BC; the native-block fixtures
                        // cannot load without it, while limits stay at WebGPU defaults.
                        features: WgpuFeatures::TEXTURE_COMPRESSION_BC,
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
        .add_plugins(RawTexelPlugin { profile })
        .add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(16)))
        .add_systems(Startup, prepare)
        .add_systems(Update, render_and_capture);
    if !app.run().is_success() {
        std::process::exit(1);
    }
}

#[derive(Resource)]
struct FixtureList(Vec<Fixture>);

#[derive(Resource)]
struct ProbeOutput(PathBuf);

fn prepare(
    mut commands: Commands,
    fixtures: Res<FixtureList>,
    output: Res<ProbeOutput>,
    profile: Res<VisualProfile>,
    asset_server: Res<AssetServer>,
    adapter: Res<RenderAdapter>,
    mut images: ResMut<Assets<Image>>,
) {
    let features = adapter.get_texture_format_features(TextureFormat::Rgba32Float);
    let target_format = if features
        .allowed_usages
        .contains(TextureUsages::RENDER_ATTACHMENT)
    {
        TextureFormat::Rgba32Float
    } else {
        TextureFormat::Rgba16Float
    };
    let target = images.add(Image::new_target_texture(
        TARGET_WIDTH,
        TARGET_HEIGHT,
        target_format,
        None,
    ));
    let cases = fixtures
        .0
        .iter()
        .map(|fixture| {
            // Every fixture is an `.opensky-srgb` alias: the profile's loader, not the probe,
            // decides its colour space. That is the path a converted colour texture takes.
            let load = |path: &str, mip1: bool| {
                asset_server
                    .load_builder()
                    .with_settings(move |settings: &mut ImageLoaderSettings| {
                        settings.sampler = ImageSampler::Descriptor(texel_sampler(mip1));
                    })
                    .load::<Image>(path.to_owned())
            };
            LoadedCase {
                name: fixture.name,
                group: fixture.group,
                mip0: load(&fixture.path0, false),
                mip1: load(&fixture.path1, true),
                red: fixture.red.clone(),
            }
        })
        .collect();
    commands.insert_resource(Probe {
        output: output.0.clone(),
        profile: *profile,
        started: Instant::now(),
        target,
        cases,
        spawned: false,
        frames: 0,
        requested: false,
    });
}

fn texel_sampler(mip1: bool) -> ImageSamplerDescriptor {
    let lod = if mip1 { 1.0 } else { 0.0 };
    ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        address_mode_w: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        mipmap_filter: ImageFilterMode::Nearest,
        lod_min_clamp: lod,
        lod_max_clamp: lod,
        ..default()
    }
}

fn build_fixtures(directory: &Path) -> Vec<Fixture> {
    std::fs::create_dir_all(directory).unwrap();
    let specs = vec![
        FixtureSpec {
            name: "native_bc1",
            group: Group::NativeBc,
            format: DxgiFormat::BC1_UNorm,
            width: 8,
            height: 8,
            mip_colours: vec![[212, 96, 48, 255], [64, 160, 224, 255]],
            alias: false,
        },
        FixtureSpec {
            name: "native_bc3",
            group: Group::NativeBc,
            format: DxgiFormat::BC3_UNorm,
            width: 8,
            height: 8,
            mip_colours: vec![[40, 188, 96, 255], [232, 72, 32, 255]],
            alias: false,
        },
        FixtureSpec {
            name: "native_bc7",
            group: Group::NativeBc,
            format: DxgiFormat::BC7_UNorm,
            width: 8,
            height: 8,
            // BC7 mode 6 endpoints share one p-bit across all channels, so the
            // fixture colours keep every channel on the same parity.
            mip_colours: vec![[214, 214, 214, 214], [96, 96, 96, 96]],
            alias: false,
        },
        FixtureSpec {
            name: "rgba8",
            group: Group::Rgba8,
            format: DxgiFormat::R8G8B8A8_UNorm,
            width: 8,
            height: 8,
            mip_colours: vec![[32, 144, 208, 255], [208, 80, 16, 255]],
            alias: false,
        },
        FixtureSpec {
            name: "mip_chain",
            group: Group::MipChain,
            format: DxgiFormat::BC1_UNorm,
            width: 8,
            height: 8,
            mip_colours: vec![[40, 64, 112, 255], [200, 96, 32, 255], [16, 208, 96, 255]],
            alias: false,
        },
        FixtureSpec {
            name: "wrap_alias",
            group: Group::WrapAlias,
            format: DxgiFormat::BC1_UNorm,
            width: 8,
            height: 8,
            mip_colours: vec![[128, 196, 64, 255], [48, 80, 176, 255]],
            alias: true,
        },
    ];
    specs
        .into_iter()
        .map(|spec| {
            let dds = dds_bytes(&spec);
            let ktx2 =
                TextureConverter::convert_uncompressed(&dds, TextureEncoding::ColorSrgb).unwrap();
            let path0 = if spec.alias {
                format!("{}.opensky-srgb.opensky-wrap0.ktx2", spec.name)
            } else {
                format!("{}.opensky-srgb.ktx2", spec.name)
            };
            let path1 = if spec.alias {
                format!("{}.mip1.opensky-srgb.opensky-wrap0.ktx2", spec.name)
            } else {
                format!("{}.mip1.opensky-srgb.ktx2", spec.name)
            };
            std::fs::write(directory.join(&path0), &ktx2).unwrap();
            std::fs::write(directory.join(&path1), &ktx2).unwrap();
            Fixture {
                name: spec.name,
                group: spec.group,
                path0,
                path1,
                red: (0..spec.mip_colours.len() as u32)
                    .map(|mip| decode_red(&dds, mip))
                    .collect(),
            }
        })
        .collect()
}

fn dds_bytes(spec: &FixtureSpec) -> Vec<u8> {
    let mips: Vec<Vec<u8>> = spec
        .mip_colours
        .iter()
        .enumerate()
        .map(|(mip, colour)| {
            let width = (spec.width >> mip).max(1);
            let height = (spec.height >> mip).max(1);
            match spec.format {
                DxgiFormat::BC1_UNorm => {
                    let block = bc1_block(*colour);
                    block.repeat((width.div_ceil(4) * height.div_ceil(4)) as usize)
                }
                DxgiFormat::BC3_UNorm => {
                    let block = bc3_block(*colour);
                    block.repeat((width.div_ceil(4) * height.div_ceil(4)) as usize)
                }
                DxgiFormat::BC7_UNorm => {
                    let block = bc7_uniform_block(*colour);
                    block.repeat((width.div_ceil(4) * height.div_ceil(4)) as usize)
                }
                DxgiFormat::R8G8B8A8_UNorm => colour
                    .iter()
                    .copied()
                    .cycle()
                    .take((width * height * 4) as usize)
                    .collect(),
                other => panic!("unsupported fixture format {other:?}"),
            }
        })
        .collect();
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: spec.height,
        width: spec.width,
        depth: None,
        format: spec.format,
        mipmap_levels: Some(spec.mip_colours.len() as u32),
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap();
    dds.data = mips.concat();
    let mut bytes = Vec::new();
    dds.write(&mut bytes).unwrap();
    bytes
}

fn bc1_block(colour: [u8; 4]) -> Vec<u8> {
    let r = (colour[0] >> 3) as u16;
    let g = (colour[1] >> 2) as u16;
    let b = (colour[2] >> 3) as u16;
    let endpoint = (r << 11) | (g << 5) | b;
    let mut block = vec![0u8; 8];
    block[..2].copy_from_slice(&endpoint.to_le_bytes());
    block
}

fn bc3_block(colour: [u8; 4]) -> Vec<u8> {
    let mut block = vec![0u8; 16];
    block[0] = colour[3];
    block[1] = colour[3];
    block[8..].copy_from_slice(&bc1_block(colour));
    block
}

/// A uniform BC7 block in mode 6: equal endpoints, zero indices, and a p-bit shared by all four
/// channels of each endpoint. Mode 6 carries 7-bit endpoints plus that p-bit, so every channel
/// must have the same low bit.
fn bc7_uniform_block(colour: [u8; 4]) -> Vec<u8> {
    let parity = colour[0] & 1;
    assert!(
        colour.iter().all(|channel| channel & 1 == parity),
        "BC7 mode 6 shares one p-bit per endpoint across RGBA"
    );
    let mut bits = 0u128;
    let mut position = 0u32;
    let push = |value: u128, width: u32, bits: &mut u128, position: &mut u32| {
        *bits |= (value & ((1u128 << width) - 1)) << *position;
        *position += width;
    };
    // Mode 6 is six zero bits followed by a one.
    push(0x40, 7, &mut bits, &mut position);
    for channel in colour {
        let endpoint = u128::from(channel >> 1);
        push(endpoint, 7, &mut bits, &mut position);
        push(endpoint, 7, &mut bits, &mut position);
    }
    push(u128::from(parity), 1, &mut bits, &mut position);
    push(u128::from(parity), 1, &mut bits, &mut position);
    // The remaining 63 index bits stay zero: every texel takes endpoint zero.
    bits.to_le_bytes().to_vec()
}

fn decode_red(dds_bytes: &[u8], mip: u32) -> u8 {
    let dds = Dds::read(std::io::Cursor::new(dds_bytes)).unwrap();
    let surface = image_dds::SurfaceRgba8::decode_dds(&dds).unwrap();
    let data = surface.get(0, 0, mip).unwrap();
    let width = (dds.get_width() >> mip).max(1);
    let height = (dds.get_height() >> mip).max(1);
    let x = (width / 2).min(width - 1);
    let y = (height / 2).min(height - 1);
    data[((y * width + x) * 4) as usize]
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn expected_value(byte: u8, profile: VisualProfile) -> f32 {
    let encoded = f32::from(byte) / 255.0;
    if profile.decodes_colour_srgb() {
        srgb_to_linear(encoded)
    } else {
        encoded
    }
}

fn render_and_capture(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    assets: Res<AssetServer>,
    mut exit: MessageWriter<AppExit>,
) {
    if probe.started.elapsed() > Duration::from_secs(90) {
        eprintln!("material texel probe timed out waiting for assets");
        exit.write(AppExit::error());
        return;
    }
    if !probe.spawned {
        for case in &probe.cases {
            for handle in [&case.mip0, &case.mip1] {
                if let LoadState::Failed(error) = assets.load_state(handle.id()) {
                    eprintln!("{}: fixture load failed: {error}", case.name);
                    exit.write(AppExit::error());
                    return;
                }
                if images.get(handle).is_none() {
                    return;
                }
            }
        }
        let high = meshes.add(Rectangle::new(HIGH_QUAD, HIGH_QUAD));
        let low = meshes.add(Rectangle::new(LOW_QUAD, LOW_QUAD));
        for (index, case) in probe.cases.iter().enumerate() {
            let column = index % 3;
            let row = index / 3;
            let y = 1.0 - 2.0 * row as f32;
            for (handle, mesh, x) in [
                (&case.mip0, high.clone(), 2.0 * column as f32 - 2.375),
                (&case.mip1, low.clone(), 2.0 * column as f32 - 1.3125),
            ] {
                let material = materials.add(StandardMaterial {
                    unlit: true,
                    base_color: Color::WHITE,
                    base_color_texture: Some(handle.clone()),
                    fog_enabled: false,
                    ..default()
                });
                commands.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(material),
                    Transform::from_xyz(x, y, 0.0),
                ));
            }
        }
        commands.spawn((
            Camera3d::default(),
            RenderTarget::Image(probe.target.clone().into()),
            Camera {
                clear_color: ClearColorConfig::Custom(Color::BLACK),
                ..default()
            },
            Projection::Orthographic(OrthographicProjection {
                scaling_mode: ScalingMode::FixedVertical {
                    viewport_height: TARGET_HEIGHT as f32 / PIXELS_PER_WORLD,
                },
                ..OrthographicProjection::default_3d()
            }),
            Transform::from_xyz(0.0, 0.0, 10.0),
            CompositingSpace::Linear,
            Tonemapping::None,
            DebandDither::Disabled,
            Msaa::Off,
        ));
        probe.spawned = true;
    }
    probe.frames += 1;
    if probe.frames >= 60 && probe.started.elapsed() >= Duration::from_secs(2) && !probe.requested {
        probe.requested = true;
        commands
            .spawn(Screenshot::image(probe.target.clone()))
            .observe(evaluate);
    }
}

fn evaluate(
    event: On<ScreenshotCaptured>,
    probe: Res<Probe>,
    adapter: Res<RenderAdapterInfo>,
    mut exit: MessageWriter<AppExit>,
) {
    let image = &event.image;
    let format = image.texture_descriptor.format;
    if image.width() != TARGET_WIDTH || image.height() != TARGET_HEIGHT {
        eprintln!(
            "unexpected probe readback size {}x{}",
            image.width(),
            image.height()
        );
        exit.write(AppExit::error());
        return;
    }
    let mut png = image::RgbImage::new(TARGET_WIDTH, TARGET_HEIGHT);
    for y in 0..TARGET_HEIGHT {
        for x in 0..TARGET_WIDTH {
            let sampled = pixel(image, format, x, y);
            png.put_pixel(
                x,
                y,
                image::Rgb([
                    encode_u8(sampled[0]),
                    encode_u8(sampled[1]),
                    encode_u8(sampled[2]),
                ]),
            );
        }
    }
    png.save(probe.output.join("probe.png")).unwrap();

    let mut cases = Vec::new();
    let mut verdict_passed = true;
    let mut measured_spread = 0.0f32;
    let mut summary = Vec::new();
    for (index, case) in probe.cases.iter().enumerate() {
        let column = index % 3;
        let row = index / 3;
        let high_origin = (64 * column as u32 + 22, 12 + 64 * row as u32 + 22);
        let low_origin = (64 * column as u32 + 52, 30 + 64 * row as u32);
        let (sampled0, spread0) = sample(image, format, high_origin, 3, 3);
        let (sampled1, spread1) = sample(image, format, low_origin, 4, 4);
        measured_spread = measured_spread.max(spread0).max(spread1);
        let mip0 = measurement(
            case.red.first().copied().unwrap_or(0),
            probe.profile,
            sampled0,
            spread0,
        );
        let mip1 = measurement(
            case.red.get(1).copied().unwrap_or(0),
            probe.profile,
            sampled1,
            spread1,
        );
        let pass =
            mip0["pass"].as_bool().unwrap_or(false) && mip1["pass"].as_bool().unwrap_or(false);
        if case.group.is_verdict() {
            verdict_passed &= pass;
        }
        summary.push(serde_json::json!({
            "case": case.name,
            "group": case.group.label(),
            "verdict": case.group.is_verdict(),
            "pass": pass,
            "mip0_error": mip0["error"],
            "mip1_error": mip1["error"],
        }));
        cases.push(serde_json::json!({
            "case": case.name,
            "group": case.group.label(),
            "verdict": case.group.is_verdict(),
            "pass": pass,
            "mip0": mip0,
            "mip1": mip1,
        }));
    }
    let report = serde_json::json!({
        "kind": "material-texel-probe",
        "profile": probe.profile.name(),
        "decodes_colour_srgb": probe.profile.decodes_colour_srgb(),
        "srgb_curve": "IEC 61966-2-1 piecewise",
        "target_format": format!("{format:?}"),
        "tolerance": TOLERANCE,
        "measured_spread": measured_spread,
        "resolution": [TARGET_WIDTH, TARGET_HEIGHT],
        "high_pose": {
            "quad_pixels": 40,
            "pixels_per_texel": 5,
            "quad_world": HIGH_QUAD,
            "mip": 0,
        },
        "low_pose": {
            "quad_pixels": 4,
            "source_texels_per_pixel": 2,
            "quad_world": LOW_QUAD,
            "mip": 1,
            "lod_clamp": [1.0, 1.0],
        },
        "cases": cases,
        "passed": verdict_passed,
        "adapter": {
            "name": adapter.name,
            "driver": adapter.driver,
            "driver_info": adapter.driver_info,
        },
    });
    std::fs::write(
        probe.output.join("probe.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!(
        "{}",
        serde_json::json!({
            "kind": "material-texel-probe",
            "profile": probe.profile.name(),
            "target_format": format!("{format:?}"),
            "passed": verdict_passed,
            "cases": summary,
        })
    );
    exit.write(if verdict_passed {
        AppExit::Success
    } else {
        AppExit::error()
    });
}

fn measurement(byte: u8, profile: VisualProfile, sampled: f32, spread: f32) -> serde_json::Value {
    let expected = expected_value(byte, profile);
    let error = (sampled - expected).abs();
    serde_json::json!({
        "texel_red_u8": byte,
        "expected": expected,
        "sampled": sampled,
        "error": error,
        "spread": spread,
        "tolerance": TOLERANCE,
        "pass": error <= TOLERANCE,
    })
}

fn sample(
    image: &Image,
    format: TextureFormat,
    origin: (u32, u32),
    width: u32,
    height: u32,
) -> (f32, f32) {
    let mut sum = 0.0f32;
    let mut minimum = f32::INFINITY;
    let mut maximum = f32::NEG_INFINITY;
    for y in origin.1..origin.1 + height {
        for x in origin.0..origin.0 + width {
            let red = pixel(image, format, x, y)[0];
            sum += red;
            minimum = minimum.min(red);
            maximum = maximum.max(red);
        }
    }
    let count = f32::from(u16::try_from(width * height).unwrap());
    (sum / count, maximum - minimum)
}

fn pixel(image: &Image, format: TextureFormat, x: u32, y: u32) -> [f32; 4] {
    let stride = image.width() as usize;
    let offset = (y as usize * stride + x as usize) * pixel_bytes(format);
    let data = &image.data.as_deref().unwrap()[offset..];
    match format {
        TextureFormat::Rgba32Float => [
            f32::from_le_bytes(data[..4].try_into().unwrap()),
            f32::from_le_bytes(data[4..8].try_into().unwrap()),
            f32::from_le_bytes(data[8..12].try_into().unwrap()),
            f32::from_le_bytes(data[12..16].try_into().unwrap()),
        ],
        TextureFormat::Rgba16Float => [
            half_to_f32(u16::from_le_bytes(data[..2].try_into().unwrap())),
            half_to_f32(u16::from_le_bytes(data[2..4].try_into().unwrap())),
            half_to_f32(u16::from_le_bytes(data[4..6].try_into().unwrap())),
            half_to_f32(u16::from_le_bytes(data[6..8].try_into().unwrap())),
        ],
        other => panic!("unexpected probe readback format {other:?}"),
    }
}

fn pixel_bytes(format: TextureFormat) -> usize {
    match format {
        TextureFormat::Rgba32Float => 16,
        TextureFormat::Rgba16Float => 8,
        other => panic!("unexpected probe readback format {other:?}"),
    }
}

fn encode_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn half_to_f32(bits: u16) -> f32 {
    let sign = (bits >> 15) & 1;
    let exponent = (bits >> 10) & 0x1f;
    let fraction = bits & 0x3ff;
    let magnitude = if exponent == 0 {
        f32::from(fraction) * 2f32.powi(-24)
    } else if exponent == 0x1f {
        if fraction == 0 {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1.0 + f32::from(fraction) / 1024.0) * 2f32.powi(i32::from(exponent) - 15)
    };
    if sign == 1 { -magnitude } else { magnitude }
}
