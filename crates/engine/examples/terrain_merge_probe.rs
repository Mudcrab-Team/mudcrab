//! Renders merged terrain cells on the GPU and compares them with plain quads of the colours the
//! blend and lighting maths predict. --output DIR.
//!
//! The probe builds two cells by hand: one with 8 slots per quadrant and one with 16, so both
//! vertex layouts and both shader specialisations run through the colour pass. The merged
//! material stays out of the depth prepass (`MergedTerrainExtension::enable_prepass`), so the
//! prepass is not exercised for merged cells; only the plain reference quads use it. The
//! textures are small solid-colour texture arrays in two classes, plus a normal class, so the
//! class switch, the lookup table, the implied base weight and the cross-class blend are all
//! exercised. Three more merged cells test lighting under the sun: a lit grey cell, seen
//! from the front and turned over, and an `unlit` twin that must read its albedo exactly. No
//! converted data is read.
use bevy::{
    asset::RenderAssetUsages,
    camera::{RenderTarget, ScalingMode},
    core_pipeline::{
        prepass::DepthPrepass,
        tonemapping::{DebandDither, Tonemapping},
    },
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    prelude::*,
    render::{
        RenderPlugin,
        render_resource::{
            Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
        },
        settings::{RenderCreation, WgpuSettings, WgpuSettingsPriority},
        storage::ShaderBuffer,
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::{ExitCondition, WindowPlugin},
};
use engine::terrain_merge::{
    TerrainMergePlugin,
    classes::TerrainClasses,
    material::{MergedTerrainExtension, MergedTerrainMaterial},
    mesh::build_merged_mesh,
};
use shared::terrain_mesh::{
    ArrayLayer, CELL_MESH_VERTICES, LAND_TEXTURE_SIZE_CLASSES, LandTextureArray, LandTextureEntry,
    LandTextureFormat, LandTextureTable, LandTextureUsage, TERRAIN_MESH_FORMAT_VERSION,
    TerrainCellMesh, vertex_index,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const FRAME_W: u32 = 1024;
const FRAME_H: u32 = 1536;
/// How much brighter than its unlit reading a lit cell must read, so "lit" means visibly lit.
const LIT_MARGIN_U8: u8 = 32;

/// World units across the view: two cells wide, and six rows tall: the terrain row, the row of
/// blend reference quads, the lit row, a row with the unlit twin and its reference quads, and two
/// empty rows of clear colour.
const VIEW_W: f32 = 8192.0;
const VIEW_H: f32 = 24576.0;

/// A pixel position in the probe's render target.
type Pixel = (u32, u32);

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    target: Handle<Image>,
    frames: u32,
    start: Instant,
    /// (name, terrain pixel, reference pixel) pairs: the terrain pixel must equal the reference
    /// pixel within the tolerance, and the reference must not be empty space.
    expected: Vec<(String, Pixel, Pixel)>,
    /// Single-pixel cases, each with its own [`PixelCheck`].
    special: Vec<PixelCase>,
    /// A pixel of empty sky, which must show the green clear colour. This is what lets
    /// [`PixelCheck::Black`] tell a drawn black pixel from an empty one.
    empty: (u32, u32),
}

/// A single-pixel case: where to read the frame and what the pixel must show.
struct PixelCase {
    name: String,
    px: u32,
    py: u32,
    check: PixelCheck,
}

/// What one [`PixelCase`] asserts.
enum PixelCheck {
    /// A merged cell lit by the sun: it must equal the reference pixel (a plain lit quad of the
    /// same albedo and orientation), read clearly brighter than the albedo's unlit reading, and
    /// not be white-clipped.
    Lit {
        reference: (u32, u32),
        /// The albedo's unlit screen reading, per channel.
        unlit: [u8; 3],
    },
    /// A merged cell facing away from the sun: it must be black. Empty space shows the green clear
    /// colour instead, so this cannot pass on a pixel the cell never covered.
    Black,
    /// A merged cell with `unlit: true` beside the sun: it must equal the reference pixel (a plain
    /// unlit quad of the same albedo), i.e. read its albedo exactly.
    Unlit { reference: (u32, u32) },
    /// Empty sky: it must show the green clear colour, proving the colour is really set.
    Empty,
}

fn main() {
    let mut output = PathBuf::from("terrain-merge-probe");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = args.next().expect("output directory").into(),
            _ => panic!("unknown argument {arg}"),
        }
    }
    std::fs::create_dir_all(&output).unwrap();
    let mut app = App::new();
    // No ambient light, so a face the sun does not reach is black.
    app.insert_resource(GlobalAmbientLight {
        brightness: 0.0,
        ..default()
    })
    .add_plugins(
        DefaultPlugins
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(WgpuSettings {
                    priority: WgpuSettingsPriority::Functionality,
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
    .add_plugins(TerrainMergePlugin::new(None))
    .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
        Duration::from_millis(16),
    ));
    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            FRAME_W,
            FRAME_H,
            TextureFormat::Rgba8UnormSrgb,
            None,
        ));
    let result = app
        .insert_resource(Probe {
            output,
            target,
            frames: 0,
            start: Instant::now(),
            expected: Vec::new(),
            special: Vec::new(),
            empty: (0, 0),
        })
        .add_systems(Startup, setup)
        .add_systems(Update, capture)
        .run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

fn repeating() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::linear()
    })
}

/// A texture array of solid-colour layers, `side` texels wide.
fn solid_array(side: u32, format: TextureFormat, layers: &[[u8; 4]]) -> Image {
    let data: Vec<u8> = layers
        .iter()
        .flat_map(|colour| (0..side * side).flat_map(move |_| *colour))
        .collect();
    let mut image = Image::new(
        Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: layers.len() as u32,
        },
        TextureDimension::D2,
        data,
        format,
        // As the engine loads them: pixels on the GPU only.
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    image.sampler = repeating();
    image
}

/// The land texture table of the probe: five textures over three arrays (two diffuse classes of
/// different sizes, one normal class).
fn table() -> LandTextureTable {
    let array = |usage, format, size, layers, name: &str| LandTextureArray {
        path: format!("terrain/{name}.ktx2"),
        usage,
        format,
        size,
        layers,
    };
    let entry =
        |id: u16, ltex_form_id, diffuse: (u8, u16), normal: Option<(u8, u16)>| LandTextureEntry {
            id,
            ltex_form_id,
            diffuse: ArrayLayer {
                array: diffuse.0,
                layer: diffuse.1,
            },
            normal: normal.map(|(array, layer)| ArrayLayer { array, layer }),
        };
    LandTextureTable {
        format_version: TERRAIN_MESH_FORMAT_VERSION,
        max_layers_per_array: 2048,
        settings: None,
        arrays: vec![
            array(
                LandTextureUsage::DiffuseSrgb,
                LandTextureFormat::Bc1,
                LAND_TEXTURE_SIZE_CLASSES[0],
                3,
                "d0",
            ),
            array(
                LandTextureUsage::DiffuseSrgb,
                LandTextureFormat::Bc3,
                LAND_TEXTURE_SIZE_CLASSES[1],
                2,
                "d1",
            ),
            array(
                LandTextureUsage::NormalLinear,
                LandTextureFormat::Bc3,
                LAND_TEXTURE_SIZE_CLASSES[0],
                2,
                "n0",
            ),
        ],
        entries: vec![
            entry(0, 0, (0, 0), None),
            entry(1, 0x100, (0, 1), Some((2, 0))),
            entry(2, 0x101, (0, 2), None),
            entry(3, 0x102, (1, 0), Some((2, 1))),
            entry(4, 0x103, (1, 1), None),
        ],
    }
}

/// The colour of each probe texture, by LTEX FormID.
fn colour(ltex: u32) -> [u8; 3] {
    match ltex {
        0x100 => [255, 0, 0],
        0x101 => [0, 255, 0],
        0x102 => [0, 0, 255],
        0x103 => [255, 255, 0],
        _ => [128, 128, 128],
    }
}

fn linear(value: u8) -> f32 {
    Color::srgb_u8(value, value, value).to_linear().red
}

/// One quadrant to put in a cell: its slot textures and the weight of each slot above 0.
struct Quad {
    slots: Vec<u32>,
    weights: Vec<u8>,
}

fn cell(stride: u8, quads: [Quad; 4]) -> TerrainCellMesh {
    let stride_len = usize::from(stride);
    let mut cell = TerrainCellMesh {
        cell_id: 1,
        key: [0; 32],
        heights: vec![0.0; CELL_MESH_VERTICES],
        normals: [0, 0, 127].repeat(CELL_MESH_VERTICES),
        vertex_colors: Vec::new(),
        slot_stride: stride,
        quadrant_slots: vec![0; 4 * stride_len],
        quadrant_slot_counts: [1; 4],
        weights: vec![0; stride_len * CELL_MESH_VERTICES],
    };
    for (quadrant, quad) in quads.iter().enumerate() {
        cell.quadrant_slot_counts[quadrant] = quad.slots.len() as u8;
        for (slot, &ltex) in quad.slots.iter().enumerate() {
            cell.quadrant_slots[quadrant * stride_len + slot] = ltex;
            if slot > 0 {
                for y in 0..17 {
                    for x in 0..17 {
                        cell.weights[vertex_index(quadrant, x, y) * stride_len + slot] =
                            quad.weights[slot - 1];
                    }
                }
            }
        }
    }
    cell
}

/// The linear colour the blend maths gives: `sum(colour_i * w_i) / sum(w_i)`, slot 0 implied.
fn blend(quad: &Quad) -> [f32; 3] {
    let overlay: f32 = quad.weights.iter().map(|&w| f32::from(w) / 255.0).sum();
    let base = (1.0 - overlay).max(0.0);
    let total = (base + overlay).max(0.0001);
    let mut out = [0.0; 3];
    for (slot, &ltex) in quad.slots.iter().enumerate() {
        let weight = if slot == 0 {
            base
        } else {
            f32::from(quad.weights[slot - 1]) / 255.0
        };
        for (channel, value) in colour(ltex).into_iter().enumerate() {
            out[channel] += linear(value) * weight / total;
        }
    }
    out
}

fn setup(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut terrain_materials: ResMut<Assets<MergedTerrainMaterial>>,
    mut standard: ResMut<Assets<StandardMaterial>>,
) {
    let classes = TerrainClasses::from_table(table()).unwrap();
    let diffuse = [
        images.add(solid_array(
            4,
            TextureFormat::Rgba8UnormSrgb,
            &[[128, 128, 128, 255], [255, 0, 0, 255], [0, 255, 0, 255]],
        )),
        images.add(solid_array(
            8,
            TextureFormat::Rgba8UnormSrgb,
            &[[0, 0, 255, 255], [255, 255, 0, 255]],
        )),
    ];
    let normal = [images.add(solid_array(
        4,
        TextureFormat::Rgba8Unorm,
        &[[128, 128, 255, 255], [200, 128, 255, 255]],
    ))];
    let entries = buffers.add(ShaderBuffer::new(
        &classes.entry_bytes(),
        RenderAssetUsages::RENDER_WORLD,
    ));
    let material = terrain_materials.add(MergedTerrainMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            unlit: true,
            cull_mode: None,
            double_sided: true,
            ..default()
        },
        extension: MergedTerrainExtension::new(&diffuse, &normal, entries.clone()).unwrap(),
    });

    let narrow = cell(
        8,
        [
            Quad {
                slots: vec![0x100, 0x101],
                weights: vec![128],
            },
            Quad {
                slots: vec![0x100, 0x101],
                weights: vec![255],
            },
            Quad {
                slots: vec![0x102],
                weights: vec![],
            },
            Quad {
                slots: vec![0x100, 0x102, 0x103],
                weights: vec![85, 85],
            },
        ],
    );
    let wide_quads = [
        Quad {
            slots: vec![
                0x100, 0x101, 0x102, 0x103, 0x100, 0x101, 0x102, 0x103, 0x100, 0x102,
            ],
            weights: vec![0, 0, 0, 0, 0, 0, 0, 0, 255],
        },
        Quad {
            slots: vec![0x103, 0x103, 0x103, 0x103, 0x101],
            weights: vec![0, 0, 0, 64],
        },
        Quad {
            slots: vec![0x101],
            weights: vec![],
        },
        Quad {
            slots: vec![
                0x100, 0x100, 0x100, 0x100, 0x100, 0x100, 0x100, 0x100, 0x100, 0x100, 0x100, 0x100,
                0x103,
            ],
            weights: vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128],
        },
    ];
    let wide = cell(16, wide_quads);
    let narrow_quads = [
        Quad {
            slots: vec![0x100, 0x101],
            weights: vec![128],
        },
        Quad {
            slots: vec![0x100, 0x101],
            weights: vec![255],
        },
        Quad {
            slots: vec![0x102],
            weights: vec![],
        },
        Quad {
            slots: vec![0x100, 0x102, 0x103],
            weights: vec![85, 85],
        },
    ];
    let wide_expect = [
        Quad {
            slots: vec![0x102],
            weights: vec![],
        },
        Quad {
            slots: vec![0x103, 0x101],
            weights: vec![64],
        },
        Quad {
            slots: vec![0x101],
            weights: vec![],
        },
        Quad {
            slots: vec![0x100, 0x103],
            weights: vec![128],
        },
    ];
    let expectations = [
        (0.0, &narrow_quads, "stride8"),
        (4096.0, &wide_expect, "stride16"),
    ];

    // The expected colours go on plain unlit quads a cell-height north of the terrain: the same
    // pipeline's output for a colour the blend maths predicts, so no tonemapping or exposure has
    // to be modelled.
    let quadrant_side = 2048.0;
    for (offset, quads, name) in expectations {
        for (quadrant, quad) in quads.iter().enumerate() {
            let rgb = blend(quad);
            let centre_x = offset + (quadrant % 2) as f32 * quadrant_side + quadrant_side / 2.0;
            let centre_north = (quadrant / 2) as f32 * quadrant_side + quadrant_side / 2.0;
            commands.spawn((
                Mesh3d(meshes.add(Rectangle::new(quadrant_side, quadrant_side))),
                MeshMaterial3d(standard.add(StandardMaterial {
                    base_color: Color::linear_rgb(rgb[0], rgb[1], rgb[2]),
                    unlit: true,
                    ..default()
                })),
                Transform::from_xyz(centre_x, 0.0, -(centre_north + 4096.0))
                    .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
            ));
            // Pixel of the quadrant's centre, in the terrain row and in the reference row.
            let px = (centre_x / VIEW_W * FRAME_W as f32) as u32;
            let terrain_py = ((VIEW_H - centre_north) / VIEW_H * FRAME_H as f32) as u32;
            let reference_py = ((VIEW_H - centre_north - 4096.0) / VIEW_H * FRAME_H as f32) as u32;
            probe.expected.push((
                format!("{name} quadrant {quadrant}"),
                (px, terrain_py),
                (px, reference_py),
            ));
        }
    }
    let ids = &classes.ids_by_form_id;
    for (offset, cell) in [(0.0, &narrow), (4096.0, &wide)] {
        commands.spawn((
            Mesh3d(meshes.add(build_merged_mesh(cell, ids).unwrap())),
            MeshMaterial3d(material.clone()),
            Transform::from_xyz(offset, 0.0, 0.0),
        ));
    }
    // The lit row: merged cells with lighting enabled under the sun (there is no ambient light, so
    // a face the sun does not reach is black). The albedo is mid grey (LTEX 0), below full
    // brightness, so lighting visibly changes it and the lit reading stays short of white. One
    // copy faces the sun; one is turned over, so the camera above sees its back faces and its
    // normals point down. The quadrant path never flips a normal on a back face (its meshes carry
    // tangents), so the sun reaches the first and leaves the second black; a flipped normal would
    // light it. A third copy keeps `unlit: true` and must read its albedo exactly even next to
    // the sun.
    let lit = terrain_materials.add(MergedTerrainMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 1.0,
            reflectance: 0.0,
            cull_mode: None,
            double_sided: true,
            ..default()
        },
        extension: MergedTerrainExtension::new(&diffuse, &normal, entries.clone()).unwrap(),
    });
    let grey = || Quad {
        slots: vec![0],
        weights: vec![],
    };
    let lit_mesh =
        meshes.add(build_merged_mesh(&cell(8, [grey(), grey(), grey(), grey()]), ids).unwrap());
    commands.spawn((
        Mesh3d(lit_mesh.clone()),
        MeshMaterial3d(lit.clone()),
        // z -8192: the mesh spans the lit row.
        Transform::from_xyz(0.0, 0.0, -8192.0),
    ));
    commands.spawn((
        Mesh3d(lit_mesh.clone()),
        MeshMaterial3d(lit.clone()),
        // Turned over: the rotation maps the mesh onto the lit row from below, so the whole cell
        // stays inside the view and the camera's near and far planes.
        Transform::from_xyz(4096.0, 0.0, -12288.0)
            .with_rotation(Quat::from_rotation_x(std::f32::consts::PI)),
    ));
    commands.spawn((
        Mesh3d(lit_mesh),
        MeshMaterial3d(material.clone()),
        // The unlit twin, one row below the lit row.
        Transform::from_xyz(0.0, 0.0, -12288.0),
    ));
    // Plain quads of the same grey albedo, one lit and one unlit, beside the unlit twin: the
    // references the merged cells are read against, so no lighting model has to be built.
    let grey_linear = linear(128);
    let grey_colour = Color::linear_rgb(grey_linear, grey_linear, grey_linear);
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(quadrant_side, quadrant_side))),
        MeshMaterial3d(standard.add(StandardMaterial {
            base_color: grey_colour,
            unlit: true,
            ..default()
        })),
        Transform::from_xyz(5120.0, 0.0, -14336.0)
            .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(quadrant_side, quadrant_side))),
        MeshMaterial3d(standard.add(StandardMaterial {
            base_color: grey_colour,
            perceptual_roughness: 1.0,
            reflectance: 0.0,
            ..default()
        })),
        Transform::from_xyz(7168.0, 0.0, -14336.0)
            .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
    ));
    let px = |x: f32| (x / VIEW_W * FRAME_W as f32) as u32;
    let py = |dist_north: f32| ((VIEW_H - dist_north) / VIEW_H * FRAME_H as f32) as u32;
    probe.special.push(PixelCase {
        name: "front face, lit".to_owned(),
        px: px(1024.0),
        py: py(11264.0),
        check: PixelCheck::Lit {
            reference: (px(7168.0), py(14336.0)),
            // With the sun off the cell would read its albedo exactly, as the unlit twin does.
            unlit: colour(0),
        },
    });
    probe.special.push(PixelCase {
        name: "back face, lit from above, turned over".to_owned(),
        px: px(4608.0),
        py: py(11264.0),
        check: PixelCheck::Black,
    });
    probe.special.push(PixelCase {
        name: "unlit cell beside the sun reads its albedo".to_owned(),
        px: px(1024.0),
        py: py(15360.0),
        check: PixelCheck::Unlit {
            reference: (px(5120.0), py(14336.0)),
        },
    });
    // Empty sky in the row above the unlit twin's row: nothing is spawned there, so the pixel must
    // show the clear colour.
    let empty = (px(2048.0), py(18432.0));
    probe.special.push(PixelCase {
        name: "empty sky shows the clear colour".to_owned(),
        px: empty.0,
        py: empty.1,
        check: PixelCheck::Empty,
    });
    probe.empty = empty;
    commands.spawn((
        DirectionalLight {
            illuminance: 10_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::default().looking_to(Vec3::NEG_Y, Vec3::Z),
    ));
    commands.spawn((
        Camera3d::default(),
        RenderTarget::Image(probe.target.clone().into()),
        DepthPrepass,
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Camera {
            // Green, so an empty pixel can be told from a drawn black one (see `PixelCheck`).
            clear_color: ClearColorConfig::Custom(Color::linear_rgb(0.0, 0.15, 0.0)),
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::Fixed {
                width: VIEW_W,
                height: VIEW_H,
            },
            // The camera is 6000 units above the terrain; the default far plane is 1000, which
            // would clip everything.
            near: 0.0,
            far: 12_000.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(VIEW_W / 2.0, 6000.0, -VIEW_H / 2.0)
            .looking_at(Vec3::new(VIEW_W / 2.0, 0.0, -VIEW_H / 2.0), Vec3::NEG_Z),
    ));
}

fn capture(mut commands: Commands, mut probe: ResMut<Probe>, mut exit: MessageWriter<AppExit>) {
    if probe.start.elapsed() > Duration::from_secs(90) {
        eprintln!("terrain merge probe timed out");
        exit.write(AppExit::error());
        return;
    }
    probe.frames += 1;
    if probe.frames == 180 {
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
    let mut samples = Vec::new();
    let mut passed = true;
    // Each pair's terrain pixel is compared with its reference pixel, so the expectation needs no
    // tonemapping or exposure model. The reference is also checked not to be empty space: black,
    // or the green clear colour.
    let empty = image.get_pixel(probe.empty.0, probe.empty.1).0;
    for (name, terrain, at) in &probe.expected {
        let actual = image.get_pixel(terrain.0, terrain.1).0;
        let reference = image.get_pixel(at.0, at.1).0;
        let error = actual
            .into_iter()
            .zip(reference)
            .map(|(a, b)| a.abs_diff(b))
            .max()
            .unwrap();
        let drawn = reference.iter().any(|value| *value > 20)
            && empty.iter().zip(reference).any(|(&e, r)| e.abs_diff(r) > 3);
        passed &= error <= 3 && drawn;
        samples.push(serde_json::json!({
            "case": name, "terrain": actual, "reference": reference, "max_error_u8": error
        }));
    }
    for case in &probe.special {
        let actual = image.get_pixel(case.px, case.py).0;
        let (sample, ok) = match &case.check {
            PixelCheck::Lit { reference, unlit } => {
                let expected = image.get_pixel(reference.0, reference.1).0;
                let error = actual
                    .iter()
                    .zip(expected)
                    .map(|(a, b)| a.abs_diff(b))
                    .max()
                    .unwrap();
                let brighter = actual
                    .iter()
                    .zip(unlit)
                    .all(|(&value, &raw)| value > raw + LIT_MARGIN_U8);
                let unclipped = actual.iter().all(|&value| value < 255);
                let ok = error <= 3 && brighter && unclipped;
                (
                    serde_json::json!({
                        "case": case.name, "pixel": actual, "check": "lit", "ok": ok,
                        "reference": expected, "max_error_u8": error,
                        "clearly_brighter_than_unlit": brighter, "not_white_clipped": unclipped,
                    }),
                    ok,
                )
            }
            PixelCheck::Black => {
                let ok = actual.iter().all(|&value| value < 10);
                (
                    serde_json::json!({
                        "case": case.name, "pixel": actual, "check": "black", "ok": ok,
                    }),
                    ok,
                )
            }
            PixelCheck::Unlit { reference } => {
                let expected = image.get_pixel(reference.0, reference.1).0;
                let error = actual
                    .iter()
                    .zip(expected)
                    .map(|(a, b)| a.abs_diff(b))
                    .max()
                    .unwrap();
                let ok = error <= 3;
                (
                    serde_json::json!({
                        "case": case.name, "pixel": actual, "check": "unlit reads albedo", "ok": ok,
                        "reference": expected, "max_error_u8": error,
                    }),
                    ok,
                )
            }
            PixelCheck::Empty => {
                let ok = actual[1] > 20 && actual[0] < 10 && actual[2] < 10;
                (
                    serde_json::json!({
                        "case": case.name, "pixel": actual, "check": "empty shows the clear colour",
                        "ok": ok,
                    }),
                    ok,
                )
            }
        };
        passed &= ok;
        samples.push(sample);
    }
    let report = serde_json::json!({
        "kind": "merged-terrain-blend",
        "resolution": [FRAME_W, FRAME_H],
        "tolerance_u8": 3,
        "samples": samples,
        "adapter": adapter.name,
        "passed": passed
    });
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
