//! Converter → Bevy glTF loader → GPU effect palette check. No Skyrim assets required.
//! --output <directory> [--interior] [--standard-material] (negative control).
//!
//! Also checks that a palette material counts its palette image as a dependency: the recursive
//! load must fail with an Image error for the exact missing palette path. The decal cases draw
//! through the depth override's wrapper (`/nif_palette_depth`) and must match the same references.
use bevy::{
    asset::AssetLoadFailedEvent,
    camera::{RenderTarget, ScalingMode},
    core_pipeline::tonemapping::DebandDither,
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
        NifAlphaMode, NifMaterialDisposition, NifShaderFamily, NifShapeMaterial,
        NifTextureSemantic, NifTextureSlot, ValidatedNifMaterial, publish_gltf_materials,
    },
    texture::{TextureConverter, TextureEncoding},
};
use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};
use engine::{
    color_pipeline::SceneColorPipeline,
    nif_depth::{DECAL, DEPTH_TEST},
    nif_material::{
        EffectPaletteDepthMaterial, EffectPaletteMaterial, EffectPaletteMaterialPlugin,
        NifMaterialPlugin,
    },
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const PALETTE_COLOR: u32 = 1 << 4;
const PALETTE_ALPHA: u32 = 1 << 5;
const MISSING_PALETTE_SOURCE_TEXTURE_PATH: &str = "textures/missing_palette.dds";
const MISSING_PALETTE_RUNTIME_TEXTURE_PATH: &str = "textures/missing_palette.ktx2";
/// A depth-tested decal: an authored depth override, so the palette draws through its wrapper.
const DEPTH_DECAL: u32 = DEPTH_TEST | DECAL;
/// 4×4 palette. Red rises and alpha falls along U, green rises along V, so a wrong coordinate
/// shows in a channel of its own; blue marks the palette apart from the source texture. Alpha
/// falls where grey rises, so an opaque source texel can look up a transparent palette entry.
const PALETTE_SIZE: usize = 4;
/// `alphaCutoff` the converter publishes for the masked cases' threshold of 128.
const CUTOFF: f32 = 128.0 / 255.0;

fn palette_texel(x: usize, y: usize) -> [u8; 4] {
    [
        40 + 60 * x as u8,
        30 + 70 * y as u8,
        128,
        230 - 70 * x as u8,
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Opaque,
    Blend,
    Mask,
}

struct Case {
    name: &'static str,
    flags: u32,
    mode: Mode,
    /// The source texture's stored grey level and alpha.
    texel: [u8; 2],
    /// Base colour, the second palette coordinate's source.
    base: [f32; 4],
    /// Base colour scale.
    scale: f32,
}

const CASES: [Case; 9] = [
    Case {
        name: "colour_mid",
        flags: PALETTE_COLOR,
        mode: Mode::Opaque,
        texel: [128, 255],
        base: [0.5, 0.5, 0.5, 1.0],
        scale: 1.0,
    },
    Case {
        // The row comes from the base colour's red channel only.
        name: "colour_hdr_low_row",
        flags: PALETTE_COLOR,
        mode: Mode::Opaque,
        texel: [200, 255],
        base: [0.1, 0.4, 0.9, 1.0],
        scale: 1.6,
    },
    Case {
        name: "colour_clamped_corner",
        flags: PALETTE_COLOR,
        mode: Mode::Opaque,
        texel: [255, 255],
        base: [1.0, 1.0, 1.0, 1.0],
        scale: 0.75,
    },
    Case {
        // The forge's Flames: both channels from the palette, alpha blended.
        name: "colour_and_alpha_blend",
        flags: PALETTE_COLOR | PALETTE_ALPHA,
        mode: Mode::Blend,
        texel: [60, 180],
        base: [0.8, 0.8, 0.8, 0.3],
        scale: 1.2,
    },
    Case {
        name: "alpha_only_blend",
        flags: PALETTE_ALPHA,
        mode: Mode::Blend,
        texel: [150, 100],
        base: [0.6, 0.3, 0.9, 0.7],
        scale: 1.0,
    },
    Case {
        // The ice chunk decals: a palette under the authored depth override.
        name: "decal_colour_and_alpha",
        flags: DEPTH_DECAL | PALETTE_COLOR | PALETTE_ALPHA,
        mode: Mode::Opaque,
        texel: [90, 200],
        base: [0.7, 0.7, 0.7, 1.0],
        scale: 1.3,
    },
    Case {
        // The wrapper draws in the sorted pass, but a cutout palette alpha must still discard.
        name: "decal_mask_palette_discards",
        flags: DEPTH_DECAL | PALETTE_ALPHA,
        mode: Mode::Mask,
        texel: [128, 255],
        base: [0.9, 0.6, 0.3, 1.0],
        scale: 1.0,
    },
    Case {
        // Opaque source alpha, transparent palette alpha: every pass must discard it. A prepass
        // that tested the source alpha would write depth here and hide the background.
        name: "mask_palette_discards",
        flags: PALETTE_ALPHA,
        mode: Mode::Mask,
        texel: [128, 255],
        base: [0.9, 0.6, 0.3, 1.0],
        scale: 1.0,
    },
    Case {
        // The reverse: transparent source alpha, opaque palette alpha.
        name: "mask_palette_keeps",
        flags: PALETTE_COLOR | PALETTE_ALPHA,
        mode: Mode::Mask,
        texel: [100, 0],
        base: [0.4, 0.4, 0.4, 1.0],
        scale: 1.0,
    },
];

/// Unlit plane behind the masked rows, so a fragment a pass wrongly keeps or discards shows.
const BACKGROUND: [u8; 3] = [140, 26, 26];
/// The masked cases are the last rows of [`CASES`]; the background spans exactly these.
const MASKED_ROWS: usize = 3;

fn srgb_decode(byte: u8) -> f32 {
    Color::srgb_u8(byte, 0, 0).to_linear().red
}

/// Bilinear filtering of the decoded palette, the way the GPU filters an sRGB view, with the
/// coordinates clamped to the outer texel centres as the shader clamps them.
fn palette_lookup(u: f32, v: f32) -> [f32; 4] {
    let axis = |coordinate: f32| {
        let half = 0.5 / PALETTE_SIZE as f32;
        let x = coordinate.clamp(half, 1.0 - half) * PALETTE_SIZE as f32 - 0.5;
        let low = (x.floor() as usize).min(PALETTE_SIZE - 1);
        (low, (low + 1).min(PALETTE_SIZE - 1), x - low as f32)
    };
    let (x0, x1, tx) = axis(u);
    let (y0, y1, ty) = axis(v);
    let texel = |x, y| {
        let [r, g, b, a] = palette_texel(x, y);
        [
            srgb_decode(r),
            srgb_decode(g),
            srgb_decode(b),
            f32::from(a) / 255.0,
        ]
    };
    let lerp = |a: [f32; 4], b: [f32; 4], t: f32| std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t);
    lerp(
        lerp(texel(x0, y0), texel(x1, y0), tx),
        lerp(texel(x0, y1), texel(x1, y1), tx),
        ty,
    )
}

/// What the retail effect shader draws for `case`, as linear RGBA, or `None` where its alpha
/// test discards it.
fn expected(case: &Case) -> Option<[f32; 4]> {
    let grey = f32::from(case.texel[0]) / 255.0;
    let texel_alpha = f32::from(case.texel[1]) / 255.0;
    let tint = case.base;
    let mut color = [
        tint[0] * srgb_decode(case.texel[0]),
        tint[1] * srgb_decode(case.texel[0]),
        tint[2] * srgb_decode(case.texel[0]),
        tint[3] * texel_alpha,
    ];
    if case.flags & PALETTE_ALPHA != 0 {
        color[3] = palette_lookup(texel_alpha, tint[3])[3];
    }
    if case.flags & PALETTE_COLOR != 0 {
        let palette = palette_lookup(grey, tint[0]);
        color[..3].copy_from_slice(&palette[..3]);
    }
    if case.mode == Mode::Mask && color[3] < CUTOFF {
        return None;
    }
    Some([
        color[0] * case.scale,
        color[1] * case.scale,
        color[2] * case.scale,
        if case.mode == Mode::Blend {
            color[3]
        } else {
            1.0
        },
    ])
}

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    interior: bool,
    standard: bool,
    started: Instant,
    palettes: Vec<Handle<EffectPaletteMaterial>>,
    /// The depth wrapper each decal case draws with in place of its palette material.
    wrapped: Vec<Option<Handle<EffectPaletteDepthMaterial>>>,
    missing_palette: Handle<EffectPaletteMaterial>,
    standards: Vec<Handle<StandardMaterial>>,
    target: Handle<Image>,
    spawned: bool,
    frames: u32,
    requested: bool,
    loaded: Vec<serde_json::Value>,
    loader_passed: bool,
    missing_palette_failure_seen: bool,
}

fn write_texture(
    path: PathBuf,
    width: usize,
    height: usize,
    data: Vec<u8>,
    encoding: TextureEncoding,
) {
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: height as u32,
        width: width as u32,
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
    dds.data = data;
    let mut bytes = Vec::new();
    dds.write(&mut bytes).unwrap();
    std::fs::write(
        path,
        TextureConverter::convert_uncompressed(&bytes, encoding).unwrap(),
    )
    .unwrap();
}

fn effect_material(index: usize, case: &Case, palette: &str) -> NifShapeMaterial {
    let material = ValidatedNifMaterial {
        uv_offset: [0.0; 2],
        uv_scale: [1.0; 2],
        texture_clamp_mode: 3,
        tint_color: None,
        shader_family: NifShaderFamily::Effect,
        lighting_shader_type: None,
        shader_block: index as u32,
        texture_set_block: None,
        alpha_property_block: None,
        shader_flags_1: case.flags,
        shader_flags_2: 0,
        base_color: case.base,
        alpha: case.base[3],
        alpha_mode: match case.mode {
            Mode::Opaque => NifAlphaMode::Opaque,
            Mode::Blend => NifAlphaMode::Blend,
            Mode::Mask => NifAlphaMode::Cutout,
        },
        alpha_threshold: (case.mode == Mode::Mask).then_some(128),
        glossiness: 0.0,
        specular_color: [0.0; 3],
        specular_strength: 0.0,
        emissive_color: [case.base[0], case.base[1], case.base[2]],
        emissive_multiple: case.scale,
        double_sided: false,
        textures: vec![
            NifTextureSlot {
                slot: 0,
                semantic: NifTextureSemantic::Diffuse,
                path: format!("textures/source{index}.dds"),
                required: true,
            },
            NifTextureSlot {
                slot: 1,
                semantic: NifTextureSemantic::Greyscale,
                path: palette.into(),
                required: false,
            },
        ],
    };
    NifShapeMaterial {
        shape_block: index as u32,
        shape_name: Some(case.name.into()),
        shader_property_block: Some(index as u32),
        alpha_property_block: None,
        disposition: NifMaterialDisposition::Validated { material },
    }
}

fn write_gltf(path: PathBuf, contract: &[NifShapeMaterial]) {
    let mut document = serde_json::json!({"asset":{"version":"2.0"}, "meshes": contract.iter().map(|_| serde_json::json!({"primitives":[{}]})).collect::<Vec<_>>()});
    publish_gltf_materials(
        &mut document,
        contract,
        &(0..contract.len() as u32).collect::<Vec<_>>(),
        std::path::Path::new("materials.glb"),
    )
    .unwrap();
    // Load the published materials on diagnostic rectangles; geometry is not under test.
    document.as_object_mut().unwrap().remove("meshes");
    std::fs::write(path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
}

fn is_missing_palette_failure_path(path: &Path) -> bool {
    path == Path::new(MISSING_PALETTE_RUNTIME_TEXTURE_PATH)
}

fn missing_palette_negative_control_passed(
    recursive_failed: bool,
    expected_image_failure_seen: bool,
) -> bool {
    recursive_failed && expected_image_failure_seen
}

#[cfg(test)]
mod missing_palette_failure_tests {
    use super::*;

    #[test]
    fn converter_published_palette_uri_matches_only_its_image_failure() {
        let material = effect_material(0, &CASES[0], MISSING_PALETTE_SOURCE_TEXTURE_PATH);
        let NifMaterialDisposition::Validated {
            material: validated,
        } = &material.disposition
        else {
            panic!("palette fixture must have a validated material");
        };
        assert_eq!(
            validated
                .textures
                .iter()
                .find(|slot| slot.semantic == NifTextureSemantic::Greyscale)
                .unwrap()
                .path,
            MISSING_PALETTE_SOURCE_TEXTURE_PATH
        );

        let mut document = serde_json::json!({
            "asset": {"version": "2.0"},
            "meshes": [{"primitives": [{}]}]
        });
        publish_gltf_materials(&mut document, &[material], &[0], Path::new("materials.glb"))
            .unwrap();

        let texture_slots =
            document["materials"][0]["extensions"]["OPEN_SKYRIM_material"]["textureSlots"]
                .as_array()
                .unwrap();
        let greyscale_texture_index = texture_slots
            .iter()
            .find(|slot| slot["semantic"] == "greyscale")
            .unwrap()["texture"]
            .as_u64()
            .unwrap() as usize;
        let image_index = document["textures"][greyscale_texture_index]["source"]
            .as_u64()
            .unwrap() as usize;
        let published_uri = document["images"][image_index]["uri"].as_str().unwrap();

        assert_eq!(published_uri, MISSING_PALETTE_RUNTIME_TEXTURE_PATH);
        let expected_failure = is_missing_palette_failure_path(Path::new(published_uri));
        assert!(missing_palette_negative_control_passed(
            true,
            expected_failure
        ));

        let source_dds_failure =
            is_missing_palette_failure_path(Path::new(MISSING_PALETTE_SOURCE_TEXTURE_PATH));
        assert!(!missing_palette_negative_control_passed(
            true,
            source_dds_failure
        ));

        let unrelated_runtime_failure =
            is_missing_palette_failure_path(Path::new("textures/missing_normal.ktx2"));
        assert!(!missing_palette_negative_control_passed(
            true,
            unrelated_runtime_failure
        ));

        // A malformed parent glTF can fail recursively without an Image failure event.
        assert!(!missing_palette_negative_control_passed(true, false));

        // A successful parent must fail the negative control even if a stale event was seen.
        assert!(!missing_palette_negative_control_passed(
            false,
            expected_failure
        ));
    }
}

fn fixtures(output: &std::path::Path) -> PathBuf {
    let assets = output.join("fixtures");
    let textures = assets.join("textures");
    std::fs::create_dir_all(&textures).unwrap();
    let palette = (0..PALETTE_SIZE)
        .flat_map(|y| (0..PALETTE_SIZE).flat_map(move |x| palette_texel(x, y)))
        .collect();
    write_texture(
        textures.join("palette.ktx2"),
        PALETTE_SIZE,
        PALETTE_SIZE,
        palette,
        TextureEncoding::DataLinear,
    );
    for (index, case) in CASES.iter().enumerate() {
        let [grey, alpha] = case.texel;
        write_texture(
            textures.join(format!("source{index}.opensky-srgb.ktx2")),
            4,
            4,
            [grey, grey, grey, alpha].repeat(16),
            TextureEncoding::ColorSrgb,
        );
    }
    let contract: Vec<_> = CASES
        .iter()
        .enumerate()
        .map(|(index, case)| effect_material(index, case, "textures/palette.dds"))
        .collect();
    write_gltf(assets.join("materials.gltf"), &contract);
    // No such palette file: the material must not count as loaded with its dependencies.
    write_gltf(
        assets.join("missing_palette.gltf"),
        &[effect_material(
            0,
            &CASES[0],
            MISSING_PALETTE_SOURCE_TEXTURE_PATH,
        )],
    );
    assets
}

fn main() {
    let (unmasked, masked) = CASES.split_at(CASES.len() - MASKED_ROWS);
    assert!(
        unmasked.iter().all(|case| case.mode != Mode::Mask)
            && masked.iter().all(|case| case.mode == Mode::Mask),
        "the background covers only the last {MASKED_ROWS} cases, which must be the masked ones"
    );
    let mut output = PathBuf::from("material-palette-probe");
    let mut interior = false;
    let mut standard = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().expect("--output requires a directory").into(),
            "--interior" => interior = true,
            "--standard-material" => standard = true,
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
    let assets = fixtures(&output);
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
    .add_plugins((EffectPaletteMaterialPlugin, NifMaterialPlugin))
    .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
        Duration::from_millis(16),
    ));
    // The production handler publishes the palette material under the /nif_palette label; the
    // negative control draws the stock /std material the engine drew before.
    let server = app.world().resource::<AssetServer>().clone();
    let palettes = (0..CASES.len())
        .map(|index| server.load(format!("materials.gltf#Material{index}/nif_palette")))
        .collect();
    let wrapped = CASES
        .iter()
        .enumerate()
        .map(|(index, case)| {
            (case.flags & DECAL != 0)
                .then(|| server.load(format!("materials.gltf#Material{index}/nif_palette_depth")))
        })
        .collect();
    let missing_palette = server.load("missing_palette.gltf#Material0/nif_palette");
    let standards = (0..CASES.len())
        .map(|index| server.load(format!("materials.gltf#Material{index}/std")))
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
            standard,
            started: Instant::now(),
            palettes,
            wrapped,
            missing_palette,
            standards,
            target,
            spawned: false,
            frames: 0,
            requested: false,
            loaded: vec![],
            loader_passed: true,
            missing_palette_failure_seen: false,
        })
        .add_systems(Update, render_and_capture)
        .run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_and_capture(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    palettes: Res<Assets<EffectPaletteMaterial>>,
    wrapped: Res<Assets<EffectPaletteDepthMaterial>>,
    mut standards: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    server: Res<AssetServer>,
    mut image_failures: MessageReader<AssetLoadFailedEvent<Image>>,
    mut exit: MessageWriter<AppExit>,
) {
    probe.missing_palette_failure_seen |= image_failures
        .read()
        .any(|failure| is_missing_palette_failure_path(failure.path.path()));
    if probe.started.elapsed() > Duration::from_secs(90) {
        eprintln!("material load/render timed out");
        exit.write(AppExit::error());
        return;
    }
    if !probe.spawned {
        let Some(loaded) = probe
            .palettes
            .iter()
            .map(|handle| palettes.get(handle))
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        if probe.standards.iter().any(|h| standards.get(h).is_none())
            || probe
                .wrapped
                .iter()
                .flatten()
                .any(|h| wrapped.get(h).is_none())
        {
            return;
        }
        let dependencies = loaded.iter().flat_map(|material| {
            [
                material.base.base_color_texture.clone(),
                Some(material.extension.palette().clone()),
            ]
        });
        if dependencies.flatten().any(|h| images.get(&h).is_none()) {
            return;
        }
        use bevy::asset::RecursiveDependencyLoadState;
        let missing = server.get_recursive_dependency_load_state(&probe.missing_palette);
        let recursive_failed = matches!(missing, Some(RecursiveDependencyLoadState::Failed(_)));
        let recursive_loaded = matches!(missing, Some(RecursiveDependencyLoadState::Loaded));
        if !recursive_failed && !recursive_loaded {
            return;
        }
        let passed = missing_palette_negative_control_passed(
            recursive_failed,
            probe.missing_palette_failure_seen,
        );
        probe.loader_passed &= passed;
        let failure_path = probe
            .missing_palette_failure_seen
            .then_some(MISSING_PALETTE_RUNTIME_TEXTURE_PATH);
        probe.loaded.push(serde_json::json!({
            "case":"missing_palette",
            "recursive_load":if recursive_failed {"failed"} else {"loaded"},
            "failure_path":failure_path,
            "passed":passed
        }));
        for (case, material) in CASES.iter().zip(&loaded) {
            let format = format!(
                "{:?}",
                images
                    .get(material.extension.palette())
                    .unwrap()
                    .texture_descriptor
                    .format
            );
            // The shader takes the palette colour as decoded on sampling.
            let passed = format == "Rgba8UnormSrgb";
            probe.loader_passed &= passed;
            probe.loaded.push(
                serde_json::json!({"case":case.name,"palette_format":format,"passed":passed}),
            );
        }
        let quad = meshes.add(Rectangle::new(3.0, 0.8));
        let background = standards.add(StandardMaterial {
            base_color: Color::srgb_u8(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]),
            unlit: true,
            ..default()
        });
        commands.spawn((
            Mesh3d(meshes.add(Rectangle::new(8.0, MASKED_ROWS as f32))),
            MeshMaterial3d(background),
            Transform::from_xyz(
                0.0,
                (CASES.len() as f32 - 1.0) * 0.5
                    - (CASES.len() as f32 - (MASKED_ROWS as f32 + 1.0) * 0.5),
                -1.0,
            ),
        ));
        for (index, case) in CASES.iter().enumerate() {
            let y = (CASES.len() as f32 - 1.0) * 0.5 - index as f32;
            if let Some([r, g, b, a]) = expected(case) {
                let reference = standards.add(StandardMaterial {
                    base_color: LinearRgba::new(r, g, b, a).into(),
                    unlit: true,
                    alpha_mode: if case.mode == Mode::Blend {
                        bevy::prelude::AlphaMode::Blend
                    } else {
                        bevy::prelude::AlphaMode::Opaque
                    },
                    ..default()
                });
                commands.spawn((
                    Mesh3d(quad.clone()),
                    MeshMaterial3d(reference),
                    Transform::from_xyz(2.0, y, 0.0),
                ));
            }
            if probe.standard {
                commands.spawn((
                    Mesh3d(quad.clone()),
                    MeshMaterial3d(probe.standards[index].clone()),
                    Transform::from_xyz(-2.0, y, 0.0),
                ));
            } else if let Some(wrapped) = &probe.wrapped[index] {
                commands.spawn((
                    Mesh3d(quad.clone()),
                    MeshMaterial3d(wrapped.clone()),
                    Transform::from_xyz(-2.0, y, 0.0),
                ));
            } else {
                commands.spawn((
                    Mesh3d(quad.clone()),
                    MeshMaterial3d(probe.palettes[index].clone()),
                    Transform::from_xyz(-2.0, y, 0.0),
                ));
            }
        }
        commands.spawn((
            Camera3d::default(),
            // As the engine's cameras: a prepass that keeps a fragment the colour pass discards
            // leaves a hole in what lies behind it.
            bevy::core_pipeline::prepass::DepthPrepass,
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
    for (index, case) in CASES.iter().enumerate() {
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
        let reference = image.get_pixel(600, y).0;
        passed &= error <= 2;
        // Guard against a blank render satisfying a pairwise comparison.
        passed &= reference.iter().any(|v| *v > 5 && *v < 250);
        samples.push(serde_json::json!({"case":case.name,"flags1":case.flags,"mode":format!("{:?}", case.mode),"source_grey_alpha_u8":case.texel,"base_color":case.base,"base_color_scale":case.scale,"depth_wrapped":probe.wrapped[index].is_some(),"expected_linear":expected(case),"pixel_y":y,"converted_rgb":actual,"reference_rgb":reference,"max_error_u8":error}));
    }
    let palette: Vec<_> = (0..PALETTE_SIZE)
        .map(|y| {
            (0..PALETTE_SIZE)
                .map(|x| palette_texel(x, y))
                .collect::<Vec<_>>()
        })
        .collect();
    let report = serde_json::json!({"kind":"converted-effect-palette","retail_parity":false,"space":if probe.interior {"interior"} else {"exterior"},"standard_material":probe.standard,"ev100":9.7,"tonemapping":"TonyMcMapface","palette_rgba_u8_rows":palette,"resolution":[800,CASES.len()*100],"camera":{"position":[0,0,10],"projection":"orthographic","vertical_size":CASES.len()},"ambient_brightness":0,"lights":0,"tolerance_u8":2,"loader":probe.loaded,"samples":samples,"adapter":{"name":adapter.name,"driver":adapter.driver,"driver_info":adapter.driver_info},"passed":passed});
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
