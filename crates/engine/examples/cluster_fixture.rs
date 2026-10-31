//! Draws the shared cluster fixture (ADR-0013 section 8) through the stock mesh path and through
//! the cluster path, for an image diff. No Skyrim data required.
//!
//! `--mode stock` builds one ordinary `Mesh` from the source-geometry clusters (those no
//! simplification produced) and places it at the block's instances. `--mode cluster` draws the
//! same payload through GPU cluster selection. `--distance` is the camera distance in units of
//! the mesh's bounding radius, `--error-pixels` the selection threshold.
use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::{
        render_resource::{Extent3d, TextureDimension, TextureFormat},
        view::screenshot::{Screenshot, save_to_disk},
    },
};
use engine::cluster::{
    assets::{ClusterBlockAsset, ClusterMeshAsset},
    render::{
        CLUSTER_CARRIERS, ClusterBatch, ClusterBatchHandles, ClusterBatchStockMeshes,
        ClusterInstance, ClusterRenderPlugin, ClusterScene, ClusterSettings, batch_bounds,
        world_from_local,
    },
};
use std::path::PathBuf;

const MCL: &[u8] = include_bytes!("../../shared/tests/data/cluster/fixture.mcl");
const MCI: &[u8] = include_bytes!("../../shared/tests/data/cluster/fixture.mci");

#[derive(Resource, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Stock,
    Cluster,
}

#[derive(Resource)]
struct Output {
    directory: PathBuf,
    name: String,
}

#[derive(Resource, Default)]
struct Frames(u32);

#[derive(Default)]
struct Requested(bool);

#[derive(Resource, Clone, Copy)]
struct CaptureAfter(f32);

#[derive(Resource, Clone, Copy)]
struct Distance(f32);

/// Whether the sun casts shadows; a ground plane under the objects then receives them.
#[derive(Resource, Clone, Copy)]
struct Shadows(bool);

/// Materials the instances are dealt out across; each one is a batch of the cluster scene.
#[derive(Resource, Clone, Copy)]
struct MaterialCount(u32);

/// A render-origin shift applied to the scene: the stock path places every object this far
/// away from its absolute position from the start; the cluster path starts unshifted and is moved
/// by the origin offset a few frames in, so the picture proves the GPU shift pass.
#[derive(Resource, Clone, Copy)]
struct OriginShift(Vec3);

/// Whether the materials are alpha-cutout (`AlphaMode::Mask`) with a texture that has holes.
#[derive(Resource, Clone, Copy)]
struct Cutout(bool);

/// Whether the materials carry a normal map, and the stock mesh the tangents the glTF loader
/// would generate for it.
#[derive(Resource, Clone, Copy)]
struct NormalMapped(bool);

/// Copies of the mesh the cluster scene pools; instances are dealt out across them.
#[derive(Resource, Clone, Copy)]
struct PoolCopies(u32);

/// Camera height above the focus, in radii.
#[derive(Resource, Clone, Copy)]
struct Height(f32);

/// The instance the camera looks at, if any.
#[derive(Resource, Clone, Copy)]
struct Focus(Option<usize>);

/// A normal map of slanted ridges: every pixel tilts the normal one way along x, then the other
/// along y, so a wrong tangent frame shows as a wrong shading direction.
fn ridges() -> Image {
    let mut data = Vec::new();
    for y in 0..64u32 {
        for x in 0..64u32 {
            let tilt_x = if (x / 4) % 2 == 0 { 0.6 } else { -0.6 };
            let tilt_y = if (y / 8) % 2 == 0 { 0.4 } else { -0.4 };
            let length = (tilt_x * tilt_x + tilt_y * tilt_y + 1.0_f32).sqrt();
            let pack = |value: f32| ((value / length * 0.5 + 0.5) * 255.0) as u8;
            data.extend_from_slice(&[pack(tilt_x), pack(tilt_y), pack(1.0), 255]);
        }
    }
    Image::new(
        Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn checker(cutout: bool) -> Image {
    let mut data = Vec::new();
    for y in 0..64u32 {
        for x in 0..64u32 {
            let on = ((x / 8) + (y / 8)) % 2 == 0;
            let shade = if on { 230 } else { 60 };
            // A cutout texture is clear in every third column of squares.
            let alpha = if cutout && (x / 8) % 3 == 2 { 0 } else { 255 };
            data.extend_from_slice(&[shade, (x * 4) as u8, (y * 4) as u8, alpha]);
        }
    }
    Image::new(
        Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn main() {
    let mut mode = Mode::Stock;
    let mut directory = PathBuf::from("cluster-fixture");
    let mut name = None;
    let mut distance = 3.0_f32;
    let mut error_pixels = 1.0_f32;
    let mut focus = None;
    let mut fixed_count_draw = false;
    let mut shadows = false;
    let mut height = 0.0_f32;
    let mut pool_copies = 1_u32;
    let mut cutout = false;
    let mut normal_mapped = false;
    let mut shift = Vec3::ZERO;
    let mut material_count = 1_u32;
    let mut capture_after = 6.0_f32;
    let mut run_label = String::from("shader and draw verification");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--run-label" => run_label = args.next().expect("--run-label value"),
            "--mode" => {
                mode = match args.next().as_deref() {
                    Some("stock") => Mode::Stock,
                    Some("cluster") => Mode::Cluster,
                    other => panic!("--mode takes stock or cluster, got {other:?}"),
                }
            }
            "--output" => directory = args.next().expect("--output requires a directory").into(),
            "--name" => name = args.next(),
            "--distance" => distance = args.next().expect("--distance value").parse().unwrap(),
            "--capture-after" => {
                capture_after = args
                    .next()
                    .expect("--capture-after seconds")
                    .parse()
                    .unwrap()
            }
            "--fixed-count" => fixed_count_draw = true,
            "--shadows" => shadows = true,
            "--cutout" => cutout = true,
            "--normal-map" => normal_mapped = true,
            "--origin-shift" => {
                let mut value = || -> f32 {
                    args.next()
                        .expect("--origin-shift takes x y z")
                        .parse()
                        .unwrap()
                };
                shift = Vec3::new(value(), value(), value());
            }
            "--materials" => {
                material_count = args.next().expect("--materials count").parse().unwrap()
            }
            "--pool-copies" => {
                pool_copies = args.next().expect("--pool-copies count").parse().unwrap()
            }
            "--height" => height = args.next().expect("--height radii").parse().unwrap(),
            "--focus" => focus = Some(args.next().expect("--focus index").parse().unwrap()),
            "--error-pixels" => {
                error_pixels = args.next().expect("--error-pixels value").parse().unwrap()
            }
            _ => panic!("unknown option {arg}"),
        }
    }
    std::fs::create_dir_all(&directory).unwrap();
    let name = name.unwrap_or_else(|| {
        if mode == Mode::Stock {
            "stock"
        } else {
            "cluster"
        }
        .to_owned()
    });
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("Mudcrab [TEST] cluster fixture: {run_label} ({name})"),
                resolution: (1280, 720).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(ClusterRenderPlugin)
        .insert_resource(engine::graphics_settings::GraphicsSettings {
            cluster_error_pixels: error_pixels,
            ..default()
        })
        .insert_resource(ClusterSettings {
            error_pixels,
            fixed_count_draw,
            ..default()
        })
        .insert_resource(mode)
        .insert_resource(Distance(distance))
        .insert_resource(Focus(focus))
        .insert_resource(Shadows(shadows))
        .insert_resource(Height(height))
        .insert_resource(PoolCopies(pool_copies))
        .insert_resource(Cutout(cutout))
        .insert_resource(NormalMapped(normal_mapped))
        .insert_resource(OriginShift(shift))
        .insert_resource(MaterialCount(material_count))
        .insert_resource(CaptureAfter(capture_after))
        .insert_resource(Output { directory, name })
        .init_resource::<Frames>()
        .add_systems(Startup, setup)
        .add_systems(Update, (capture_and_exit, apply_origin_shift))
        .run();
}

#[allow(clippy::too_many_arguments)]
fn setup(
    mut commands: Commands,
    mode: Res<Mode>,
    distance: Res<Distance>,
    focus: Res<Focus>,
    shadows: Res<Shadows>,
    height: Res<Height>,
    pool_copies: Res<PoolCopies>,
    material_count: Res<MaterialCount>,
    cutout: Res<Cutout>,
    normal_mapped: Res<NormalMapped>,
    origin_shift: Res<OriginShift>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let checker_image = images.add(checker(cutout.0));
    let normal_map = normal_mapped.0.then(|| images.add(ridges()));
    let material_count = material_count.0.max(1);
    // Each material tints the same texture, so a wrong batch shows as a wrong colour.
    let palette: Vec<Handle<StandardMaterial>> = (0..material_count)
        .map(|index| {
            materials.add(StandardMaterial {
                base_color: if material_count == 1 {
                    Color::WHITE
                } else {
                    Color::hsv(360.0 * index as f32 / material_count as f32, 0.6, 1.0)
                },
                base_color_texture: Some(checker_image.clone()),
                normal_map_texture: normal_map.clone(),
                perceptual_roughness: 0.45,
                metallic: 0.1,
                alpha_mode: if cutout.0 {
                    AlphaMode::Mask(0.5)
                } else {
                    AlphaMode::Opaque
                },
                double_sided: cutout.0,
                cull_mode: if cutout.0 {
                    None
                } else {
                    Some(bevy::render::render_resource::Face::Back)
                },
                ..default()
            })
        })
        .collect();
    let mesh = ClusterMeshAsset::from_bytes(MCL.to_vec()).expect("fixture mesh is valid");
    let block = ClusterBlockAsset::from_bytes(MCI).expect("fixture block is valid");
    let center = (Vec3::from_array(block.bounds_min) + Vec3::from_array(block.bounds_max)) * 0.5;
    let radius =
        (Vec3::from_array(block.bounds_max) - Vec3::from_array(block.bounds_min)).length() * 0.5;
    let (center, radius) = match focus.0 {
        Some(index) => {
            let transform = world_from_local(&block.instances[index]);
            let sphere = mesh.bounds;
            (
                transform.transform_point3(Vec3::new(sphere[0], sphere[1], sphere[2])),
                sphere[3] * transform.x_axis.truncate().length(),
            )
        }
        None => (center, radius),
    };
    // Both modes retain the exact geometry the stock path would draw. This
    // attribute survives later normal-map removal, just as glTF's generated one does.
    let Ok(mut source) = mesh.source_mesh() else {
        error!("fixture mesh source payload is unavailable");
        return;
    };
    if normal_mapped.0 && !source.contains_attribute(Mesh::ATTRIBUTE_TANGENT) {
        source
            .generate_tangents()
            .expect("the fixture mesh has tangents");
    }
    let handle = meshes.add(source);
    commands.insert_resource(ClusterBatchStockMeshes(vec![
        handle.clone();
        (material_count + 1) as usize
    ]));
    match *mode {
        Mode::Stock => {
            for (index, record) in block.instances.iter().enumerate() {
                let mut transform = Transform::from_matrix(world_from_local(record));
                transform.translation += origin_shift.0;
                commands.spawn((
                    Mesh3d(handle.clone()),
                    MeshMaterial3d(palette[index % palette.len()].clone()),
                    transform,
                ));
            }
        }
        Mode::Cluster => {
            let copies = pool_copies.0.max(1);
            commands.insert_resource(ClusterScene {
                ready_revision: default(),
                residency_revision: default(),
                certificate: None,
                inventory: None,
                terrain_blocks: Vec::new(),
                origin_offset: Vec3::ZERO,
                blocks: Vec::new(),
                meshes: (0..copies).map(|_| mesh.clone()).collect(),
                instances: block
                    .instances
                    .iter()
                    .enumerate()
                    .map(|(index, record)| ClusterInstance {
                        identity: engine::cluster::render::ClusterInstanceIdentity::Fixture,
                        mesh: index as u32 % copies,
                        batch: index as u32 % material_count,
                        world_from_local: world_from_local(record),
                        source_worldspace: 0,
                    })
                    .collect(),
            });
            for index in 0..CLUSTER_CARRIERS {
                commands.spawn((
                    Visibility::default(),
                    Transform::default(),
                    batch_bounds(),
                    ClusterBatch { index },
                ));
            }
            for (index, material) in palette.iter().enumerate() {
                let material = material.clone();
                commands.queue(move |world: &mut World| {
                    world
                        .resource_mut::<ClusterBatchHandles>()
                        .set(index as u32, material);
                });
            }
        }
    }
    if shadows.0 {
        let floor = meshes.add(Plane3d::default().mesh().size(radius * 12.0, radius * 12.0));
        let floor_material = materials.add(StandardMaterial {
            base_color: Color::srgb(0.6, 0.6, 0.6),
            perceptual_roughness: 1.0,
            ..default()
        });
        commands.spawn((
            Mesh3d(floor),
            MeshMaterial3d(floor_material),
            Transform::from_translation(center + origin_shift.0 - Vec3::Y * radius),
        ));
    }
    commands.spawn((
        DirectionalLight {
            illuminance: 12_000.0,
            shadow_maps_enabled: shadows.0,
            ..default()
        },
        if shadows.0 {
            // Shadow falls toward the camera so the floor shows it.
            Transform::from_xyz(-3.0, 5.0, -2.0).looking_at(Vec3::ZERO, Vec3::Y)
        } else {
            Transform::from_xyz(3.0, 5.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y)
        },
    ));
    commands.spawn((
        Camera3d::default(),
        Transform::from_translation(
            center + origin_shift.0 + (Vec3::Z * distance.0 + Vec3::Y * height.0) * radius,
        )
        .looking_at(center + origin_shift.0, Vec3::Y),
    ));
}

fn capture_and_exit(
    mut commands: Commands,
    mut frames: ResMut<Frames>,
    mut requested: Local<Requested>,
    output: Res<Output>,
    mut exit: MessageWriter<AppExit>,
    time: Res<Time>,
    capture_after: Res<CaptureAfter>,
) {
    frames.0 += 1;
    let path = output.directory.join(format!("{}.png", output.name));
    if frames.0 > 600 && time.elapsed_secs() > capture_after.0 && !requested.0 {
        requested.0 = true;
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path.clone()));
    }
    if requested.0 && path.exists() {
        exit.write(AppExit::Success);
    }
}

/// Moves the cluster scene's origin offset to the shift a few frames after startup, so the GPU
/// shift pass has to place every instance again.
fn apply_origin_shift(
    shift: Res<OriginShift>,
    frames: Res<Frames>,
    scene: Option<ResMut<ClusterScene>>,
) {
    if frames.0 != 20 {
        return;
    }
    if let Some(mut scene) = scene {
        scene.origin_offset = shift.0;
    }
}
