//! Renders one converted actor (composed body, FaceGen head, idle pose) to a PNG.
//!
//! `cargo run -p engine --example actor_preview -- <converted_assets_dir> <output.png>`
//!
//! The converted world database does not carry actor models yet, so this example is the only way
//! to look at the real converted body and head without a full world conversion. It uses the same
//! binding rule as the runtime: `engine::actor_animation::animation_targets` names every body
//! node from the `Creation-to-glTF basis` wrapper down, and each target gets the `AnimationTargetId`
//! and `AnimatedBy` the idle clip addresses.
//!
//! The head is a separate scene exported against the same skeleton. A skinned mesh takes its
//! placement from its joints, so the head's skin joint lists are rewritten to the body's bones of
//! the same name and the head follows the animated body. This is the rule in `actor_head`; that
//! helper is private to the engine crate, so the ~30-line mapping lives here.
//!
//! Rendering is offscreen: the camera draws into a 1280x960 image, and a `Screenshot` of that image
//! is saved when the idle pose has settled. By default the real meshes are drawn with neutral
//! preview materials, which keeps the silhouette readable when the tree carries no textures. Pass
//! `--textures` to render the materials and KTX2 images the GLBs reference instead.

use bevy::{
    animation::{
        AnimatedBy, AnimationPlayer,
        prelude::{AnimationGraph, AnimationGraphHandle, AnimationTransitions},
    },
    asset::{AssetPlugin, LoadState},
    camera::RenderTarget,
    gltf::GltfAssetLabel,
    mesh::skinning::SkinnedMesh,
    prelude::*,
    render::{
        render_resource::TextureFormat,
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::{ExitCondition, Window, WindowPlugin, WindowRef},
    world_serialization::{WorldAsset, WorldAssetRoot},
};
use engine::{
    actor_animation::{AnimationTarget, IDLE_ANIMATION, animation_targets, idle_clip_scene},
    actors::{ActorSex, composed_body_scene},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 960;
/// Frames the idle clip advances before the camera captures the pose.
const SETTLE_FRAMES: u32 = 90;
/// Frames the camera renders before the screenshot is requested.
const CAMERA_FRAMES: u32 = 8;
const TIME_LIMIT: Duration = Duration::from_secs(180);
const MALE_HEAD: &str = "meshes/actors/character/facegendata/facegeom/skyrim.esm/00013475.glb";
const FEMALE_HEAD: &str = "meshes/actors/character/facegendata/facegeom/skyrim.esm/0010f5a1.glb";
const BACKGROUND: Color = Color::srgb(0.11, 0.13, 0.16);
/// The head's own scene is discarded when even one of its joints has no body bone.
const MIN_SKIN_FRACTION: f32 = 0.01;
const MIN_LUMINANCE: f32 = 0.02;

#[derive(Debug, Clone)]
enum HeadChoice {
    Default,
    None,
    Path(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Loading,
    Settling,
    Rendering,
    Captured,
}

/// Where the camera stands relative to the default front-three-quarter view.
#[derive(Debug, Clone, Copy, Default)]
struct View {
    yaw_degrees: f32,
    pitch_degrees: f32,
    head: bool,
}

#[derive(Resource)]
struct Preview {
    output: PathBuf,
    view: View,
    windowed: bool,
    textures: bool,
    target: Handle<Image>,
    body_scene: Handle<WorldAsset>,
    idle_gltf: Handle<Gltf>,
    head_path: Option<String>,
    head_scene: Option<Handle<WorldAsset>>,
    /// Fallback surfaces used unless `--textures` asks for the GLBs' own materials.
    body_material: Handle<StandardMaterial>,
    head_material: Handle<StandardMaterial>,
    phase: Phase,
    actor: Option<Entity>,
    body_root: Option<Entity>,
    head_root: Option<Entity>,
    targets: Vec<AnimationTarget>,
    body_nodes: usize,
    bone_names: HashMap<String, Entity>,
    bind_transforms: Vec<(Entity, Transform)>,
    moved_bones: usize,
    head_nodes: usize,
    head_skins: usize,
    head_wait_frames: u32,
    settle_frames: u32,
    camera_frames: u32,
    started: Instant,
}

impl Preview {
    fn fail(&self, message: &str) {
        eprintln!("actor preview: {message}");
    }
}

fn main() {
    let (demo, output, sex, head, windowed, textures, view) = parse_args();
    if let Err(error) = check_inputs(&demo) {
        eprintln!("actor preview: {error}");
        std::process::exit(2);
    }
    // Bevy resolves a relative asset root from the example's package directory, not the
    // caller's working directory.
    let demo = std::fs::canonicalize(&demo).unwrap_or(demo);

    let mut app = App::new();
    app.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.62, 0.68, 0.78),
        brightness: 500.0,
        ..default()
    })
    .add_plugins({
        let plugins = DefaultPlugins
            .set(AssetPlugin {
                file_path: demo.to_string_lossy().into_owned(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: windowed.then(|| Window {
                    title: "Mudcrab actor preview".to_owned(),
                    resolution: (WIDTH, HEIGHT).into(),
                    ..default()
                }),
                exit_condition: if windowed {
                    ExitCondition::OnAllClosed
                } else {
                    ExitCondition::DontExit
                },
                ..default()
            })
            .disable::<bevy::audio::AudioPlugin>();
        if windowed {
            plugins
        } else {
            plugins.disable::<bevy::winit::WinitPlugin>()
        }
    });
    if !windowed {
        app.add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
            Duration::from_millis(16),
        ));
    }

    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            WIDTH,
            HEIGHT,
            TextureFormat::Rgba8UnormSrgb,
            None,
        ));

    let (body_material, head_material) = {
        let mut materials = app.world_mut().resource_mut::<Assets<StandardMaterial>>();
        (
            materials.add(StandardMaterial {
                base_color: Color::srgb(0.72, 0.58, 0.47),
                perceptual_roughness: 0.72,
                ..default()
            }),
            materials.add(StandardMaterial {
                base_color: Color::srgb(0.80, 0.64, 0.51),
                perceptual_roughness: 0.66,
                ..default()
            }),
        )
    };
    let asset_server = app.world().resource::<AssetServer>().clone();
    let head_path = match head {
        HeadChoice::Default => Some(match sex {
            ActorSex::Male => MALE_HEAD.to_owned(),
            ActorSex::Female => FEMALE_HEAD.to_owned(),
        }),
        HeadChoice::None => None,
        HeadChoice::Path(path) => Some(path),
    };
    let body_scene =
        asset_server.load(GltfAssetLabel::Scene(0).from_asset(composed_body_scene(sex).to_owned()));
    let idle_gltf = asset_server.load(idle_clip_scene(sex));
    let head_scene = head_path
        .as_ref()
        .map(|path| asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.clone())));

    app.insert_resource(Preview {
        output,
        view,
        windowed,
        target,
        body_scene,
        idle_gltf,
        head_path,
        head_scene,
        body_material,
        head_material,
        textures,
        phase: Phase::Loading,
        actor: None,
        body_root: None,
        head_root: None,
        targets: Vec::new(),
        body_nodes: 0,
        bone_names: HashMap::new(),
        bind_transforms: Vec::new(),
        moved_bones: 0,
        head_nodes: 0,
        head_skins: 0,
        head_wait_frames: 0,
        settle_frames: 0,
        camera_frames: 0,
        started: Instant::now(),
    })
    .add_systems(Update, drive);

    let result = app.run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

fn parse_args() -> (PathBuf, PathBuf, ActorSex, HeadChoice, bool, bool, View) {
    let mut args = std::env::args().skip(1);
    let usage = "usage: actor_preview <converted_assets_dir> <output.png> [--sex male|female] [--head PATH|none] [--textures] [--window] [--yaw DEG] [--pitch DEG] [--focus body|head]";
    let Some(demo) = args.next() else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let Some(output) = args.next() else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let mut sex = ActorSex::Male;
    let mut head = HeadChoice::Default;
    let mut windowed = false;
    let mut textures = false;
    let mut view = View::default();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--sex" => {
                let Some(value) = args.next() else {
                    eprintln!("actor preview: --sex needs male or female");
                    std::process::exit(2);
                };
                sex = match ActorSex::parse(&value) {
                    Some(sex) => sex,
                    None => {
                        eprintln!("actor preview: unknown sex `{value}`");
                        std::process::exit(2);
                    }
                };
            }
            "--window" => windowed = true,
            "--yaw" | "--pitch" => {
                let Some(value) = args.next().and_then(|value| value.parse::<f32>().ok()) else {
                    eprintln!("actor preview: {flag} needs a number of degrees");
                    std::process::exit(2);
                };
                if flag == "--yaw" {
                    view.yaw_degrees = value;
                } else {
                    view.pitch_degrees = value;
                }
            }
            "--focus" => {
                view.head = match args.next().as_deref() {
                    Some("head") => true,
                    Some("body") => false,
                    _ => {
                        eprintln!("actor preview: --focus needs body or head");
                        std::process::exit(2);
                    }
                };
            }
            "--textures" => textures = true,
            "--head" => {
                let Some(value) = args.next() else {
                    eprintln!("actor preview: --head needs a path or none");
                    std::process::exit(2);
                };
                head = if value == "none" {
                    HeadChoice::None
                } else {
                    HeadChoice::Path(value)
                };
            }
            other => {
                eprintln!("actor preview: unknown argument `{other}`");
                std::process::exit(2);
            }
        }
    }
    (
        PathBuf::from(demo),
        PathBuf::from(output),
        sex,
        head,
        windowed,
        textures,
        view,
    )
}

fn check_inputs(demo: &Path) -> Result<(), String> {
    let body = demo.join(composed_body_scene(ActorSex::Male));
    let female = demo.join(composed_body_scene(ActorSex::Female));
    let idle = demo.join(idle_clip_scene(ActorSex::Male));
    if !body.is_file() && !female.is_file() {
        return Err(format!(
            "{} has neither a male nor a female composed body",
            demo.display()
        ));
    }
    if !idle.is_file() {
        return Err(format!("{} has no male idle clip", demo.display()));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn drive(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    gltfs: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut preview: ResMut<Preview>,
    names: Query<&Name>,
    children: Query<&Children>,
    mut skins: Query<&mut SkinnedMesh>,
    globals: Query<&GlobalTransform>,
    material_entities: Query<&MeshMaterial3d<StandardMaterial>>,
    transforms: Query<&Transform>,
    mut exit: MessageWriter<AppExit>,
) {
    if preview.started.elapsed() > TIME_LIMIT {
        preview.fail("timed out before the frame was captured");
        exit.write(AppExit::error());
        return;
    }

    if preview.actor.is_none() {
        match asset_server.load_state(preview.body_scene.id()) {
            LoadState::Failed(error) => {
                preview.fail(&format!("body scene failed to load: {error}"));
                exit.write(AppExit::error());
                return;
            }
            LoadState::Loaded => {
                let actor = commands
                    .spawn((
                        Name::new("Actor preview"),
                        WorldAssetRoot(preview.body_scene.clone()),
                    ))
                    .id();
                preview.actor = Some(actor);
            }
            LoadState::NotLoaded | LoadState::Loading => return,
        }
    }

    let Some(actor) = preview.actor else {
        return;
    };

    if preview.phase == Phase::Loading {
        let idle_loaded = match asset_server.load_state(preview.idle_gltf.id()) {
            LoadState::Failed(error) => {
                preview.fail(&format!("idle clip failed to load: {error}"));
                exit.write(AppExit::error());
                return;
            }
            LoadState::Loaded => gltfs.get(&preview.idle_gltf).is_some(),
            LoadState::NotLoaded | LoadState::Loading => false,
        };
        if !idle_loaded {
            return;
        }
        let Ok(targets) = animation_targets(actor, &names, &children) else {
            // The scene instance has not spawned its node tree yet.
            return;
        };
        let Some(first) = targets.first().copied() else {
            preview.fail("the body scene has no animation targets");
            exit.write(AppExit::error());
            return;
        };
        let Some(clip) = clip_animation(gltfs.get(&preview.idle_gltf)) else {
            preview.fail("the idle clip has no animation");
            exit.write(AppExit::error());
            return;
        };

        preview.body_root = Some(first.entity);
        preview.body_nodes = targets.len();
        preview.bind_transforms = targets
            .iter()
            .filter_map(|target| {
                transforms
                    .get(target.entity)
                    .ok()
                    .map(|transform| (target.entity, *transform))
            })
            .collect();
        preview.bone_names = targets
            .iter()
            .filter_map(|target| {
                names
                    .get(target.entity)
                    .ok()
                    .map(|name| (name.as_str().to_owned(), target.entity))
            })
            .collect();
        preview.targets = targets.clone();

        for target in &targets {
            commands
                .entity(target.entity)
                .insert((target.id, AnimatedBy(actor)));
        }

        let mut graph = AnimationGraph::new();
        let root = graph.root;
        let idle_node = graph.add_clip(clip, 1.0, root);
        let graph = graphs.add(graph);
        let mut player = AnimationPlayer::default();
        let mut transitions = AnimationTransitions::new();
        transitions
            .play(&mut player, idle_node, Duration::ZERO)
            .repeat();
        commands
            .entity(actor)
            .insert((player, AnimationGraphHandle(graph), transitions));

        preview.phase = Phase::Settling;
    }

    if preview.head_root.is_none()
        && let Some(head_scene) = preview.head_scene.clone()
    {
        match asset_server.load_state(head_scene.id()) {
            LoadState::Loaded => {
                let head_root = commands
                    .spawn((
                        Name::new("Actor preview head"),
                        WorldAssetRoot(head_scene),
                        ChildOf(actor),
                    ))
                    .id();
                preview.head_root = Some(head_root);
            }
            LoadState::Failed(error) => {
                preview.fail(&format!("head scene failed to load: {error}"));
                preview.head_scene = None;
            }
            LoadState::NotLoaded | LoadState::Loading => {}
        }
    }

    if let Some(head_root) = preview.head_root
        && preview.head_skins == 0
    {
        let mut rehosted = 0;
        let mut missing = 0;
        for entity in std::iter::once(head_root).chain(children.iter_descendants(head_root)) {
            let Ok(mut skin) = skins.get_mut(entity) else {
                continue;
            };
            let mut joints = Vec::with_capacity(skin.joints.len());
            let mut unresolved = 0;
            for joint in skin.joints.iter() {
                match names
                    .get(*joint)
                    .ok()
                    .and_then(|name| preview.bone_names.get(name.as_str()))
                {
                    Some(body_bone) => joints.push(*body_bone),
                    None => unresolved += 1,
                }
            }
            if unresolved == 0 {
                skin.joints = joints;
                rehosted += 1;
            } else {
                missing += unresolved;
            }
        }
        if rehosted > 0 {
            preview.head_skins = rehosted;
            preview.head_nodes = std::iter::once(head_root)
                .chain(children.iter_descendants(head_root))
                .filter(|entity| names.contains(*entity))
                .count();
            println!(
                "actor preview: rehosted {rehosted} head skins onto the body bones ({missing} unresolved joints)"
            );
            preview.head_scene = None;
        } else {
            preview.head_wait_frames += 1;
            if preview.head_wait_frames > 300 {
                preview.fail("the head scene produced no skinned mesh");
                preview.head_scene = None;
            }
        }
    }

    if preview.phase == Phase::Settling {
        let head_ready =
            preview.head_path.is_none() || preview.head_skins > 0 || preview.head_scene.is_none();
        if head_ready {
            preview.settle_frames += 1;
            if preview.settle_frames >= SETTLE_FRAMES {
                preview.moved_bones = preview
                    .bind_transforms
                    .iter()
                    .filter(|(entity, before)| {
                        transforms.get(*entity).is_ok_and(|after| after != before)
                    })
                    .count();
                let bounds = if preview.view.head {
                    head_bounds(&preview, &globals)
                } else {
                    body_bounds(&preview, &globals)
                };
                if !preview.textures {
                    if let Some(body_root) = preview.body_root {
                        for entity in
                            std::iter::once(body_root).chain(children.iter_descendants(body_root))
                        {
                            if material_entities.contains(entity) {
                                commands
                                    .entity(entity)
                                    .insert(MeshMaterial3d(preview.body_material.clone()));
                            }
                        }
                    }
                    if let Some(head_root) = preview.head_root {
                        for entity in
                            std::iter::once(head_root).chain(children.iter_descendants(head_root))
                        {
                            if material_entities.contains(entity) {
                                commands
                                    .entity(entity)
                                    .insert(MeshMaterial3d(preview.head_material.clone()));
                            }
                        }
                    }
                }
                spawn_camera(&mut commands, &preview, bounds);
                preview.phase = Phase::Rendering;
            }
        }
    }

    if preview.phase == Phase::Rendering {
        preview.camera_frames += 1;
        if preview.camera_frames >= CAMERA_FRAMES {
            if preview.windowed {
                println!(
                    "actor preview: window open with {} bound targets and {} moving bones; close it to exit",
                    preview.targets.len(),
                    preview.moved_bones
                );
            } else {
                commands
                    .spawn(Screenshot::image(preview.target.clone()))
                    .observe(write_output);
            }
            preview.phase = Phase::Captured;
        }
    }
}

fn clip_animation(gltf: Option<&Gltf>) -> Option<Handle<AnimationClip>> {
    let gltf = gltf?;
    gltf.named_animations
        .get(IDLE_ANIMATION)
        .or_else(|| gltf.animations.first())
        .cloned()
}

/// The world-space bounds of the body skeleton, ignoring the head scene.
///
/// The body's vertex positions are authored in a different space from the entity that carries
/// them, so the mesh AABB is not a reliable world bound; the skeleton's global transforms are.
fn body_bounds(preview: &Preview, globals: &Query<&GlobalTransform>) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for target in &preview.targets {
        let Ok(global) = globals.get(target.entity) else {
            continue;
        };
        let point = global.translation();
        min = min.min(point);
        max = max.max(point);
    }
    if !(min.is_finite() && max.is_finite()) || min == max {
        return (Vec3::new(-60.0, 0.0, -15.0), Vec3::new(60.0, 120.0, 15.0));
    }
    // Bone tips stop short of toes, the crown, and the hands; pad the skeleton extent.
    let pad = ((max.y - min.y) * 0.08).max(2.0);
    (min - Vec3::splat(pad), max + Vec3::splat(pad))
}

/// A box around the head bone, sized to frame the face and hair.
fn head_bounds(preview: &Preview, globals: &Query<&GlobalTransform>) -> (Vec3, Vec3) {
    let Some(head) = preview
        .bone_names
        .get("NPC Head [Head]")
        .and_then(|entity| globals.get(*entity).ok())
    else {
        return body_bounds(preview, globals);
    };
    // The head bone sits at the base of the skull, so the box is shifted upwards.
    let center = head.translation() + Vec3::new(0.0, 6.0, 0.0);
    let half = Vec3::new(9.0, 10.0, 9.0);
    (center - half, center + half)
}

fn spawn_camera(commands: &mut Commands, preview: &Preview, (min, max): (Vec3, Vec3)) {
    let center = (min + max) * 0.5;
    let size = max - min;
    let fov = 40f32.to_radians();
    let aspect = WIDTH as f32 / HEIGHT as f32;
    let margin = 1.18;
    let half_height = (size.y * 0.5 * margin).max(1.0);
    let half_width = (size.x.max(size.z) * 0.5 * margin).max(1.0);
    let distance_v = half_height / (fov * 0.5).tan();
    let distance_h = half_width / ((fov * 0.5).tan() * aspect);
    let distance = distance_v.max(distance_h) + size.z * 0.5;

    // The converted actor faces -Z, so the camera sits on that side of the body.
    // The rig (camera and both lights) orbits the actor together, so every view stays lit.
    let orbit = Quat::from_rotation_y(preview.view.yaw_degrees.to_radians());
    let around = |offset: Vec3| center + orbit * offset;
    let pitch = preview.view.pitch_degrees.to_radians();
    let camera_offset = Vec3::new(distance * 0.16, size.y * 0.12, -distance);
    let camera_offset = if pitch == 0.0 {
        camera_offset
    } else {
        let flat = Vec3::new(camera_offset.x, 0.0, camera_offset.z).length();
        let horizontal = Vec3::new(camera_offset.x, 0.0, camera_offset.z).normalize();
        horizontal * flat * pitch.cos() + Vec3::Y * flat * pitch.sin()
    };
    let camera_position = around(camera_offset);
    let look_at = center + Vec3::new(0.0, size.y * 0.04, 0.0);

    commands.spawn((
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(BACKGROUND),
            ..default()
        },
        Projection::Perspective(PerspectiveProjection {
            fov,
            aspect_ratio: aspect,
            near: distance * 0.01,
            far: distance * 10.0,
            ..default()
        }),
        if preview.windowed {
            RenderTarget::Window(WindowRef::Primary)
        } else {
            RenderTarget::Image(preview.target.clone().into())
        },
        Msaa::Off,
        Transform::from_translation(camera_position).looking_at(look_at, Vec3::Y),
    ));

    commands.spawn((
        DirectionalLight {
            color: Color::srgb(1.0, 0.96, 0.9),
            illuminance: 12_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_translation(around(Vec3::new(
            -distance * 0.6,
            max.y - center.y + size.y * 0.8,
            -distance * 0.9,
        )))
        .looking_at(center, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight {
            color: Color::srgb(0.55, 0.65, 0.85),
            illuminance: 3_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_translation(around(Vec3::new(
            distance * 0.8,
            size.y * 0.3,
            -distance * 0.4,
        )))
        .looking_at(center, Vec3::Y),
    ));
}

fn write_output(
    event: On<ScreenshotCaptured>,
    preview: Res<Preview>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(dynamic) = event.image.clone().try_into_dynamic() else {
        eprintln!("actor preview: the captured image cannot be converted to a PNG");
        exit.write(AppExit::error());
        return;
    };
    let image = dynamic.to_rgb8();
    let width = image.width();
    let height = image.height();
    let mut luminance = 0.0f64;
    for pixel in image.pixels() {
        let [red, green, blue] = pixel.0;
        luminance += 0.2126 * f64::from(red) + 0.7152 * f64::from(green) + 0.0722 * f64::from(blue);
    }
    let mean_luminance = luminance / (f64::from(width) * f64::from(height) * 255.0);

    let background = corner_color(&image);
    let mut foreground = 0u64;
    for pixel in image.pixels() {
        let distance = pixel
            .0
            .iter()
            .zip(background)
            .map(|(channel, background)| u16::from(*channel).abs_diff(u16::from(background)))
            .max()
            .unwrap_or(0);
        if distance > 16 {
            foreground += 1;
        }
    }
    let foreground_fraction = foreground as f64 / (f64::from(width) * f64::from(height));

    if let Some(parent) = preview.output.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        eprintln!("actor preview: cannot create {}: {error}", parent.display());
        exit.write(AppExit::error());
        return;
    }
    if let Err(error) = image.save(&preview.output) {
        eprintln!(
            "actor preview: cannot write {}: {error}",
            preview.output.display()
        );
        exit.write(AppExit::error());
        return;
    }

    println!(
        "actor preview: wrote {} ({width}x{height}) mean luminance {mean_luminance:.3}, non-background fraction {foreground_fraction:.3}",
        preview.output.display()
    );
    println!(
        "actor preview: {} body scene nodes, {} animation targets bound, {} bones moved since bind, {} head skins rehosted, {} head nodes",
        preview.body_nodes,
        preview.targets.len(),
        preview.moved_bones,
        preview.head_skins,
        preview.head_nodes
    );

    let passed = foreground_fraction >= f64::from(MIN_SKIN_FRACTION)
        && mean_luminance >= f64::from(MIN_LUMINANCE);
    if !passed {
        eprintln!(
            "actor preview: the frame looks empty (foreground {foreground_fraction:.3}, luminance {mean_luminance:.3})"
        );
    }
    exit.write(if passed {
        AppExit::Success
    } else {
        AppExit::error()
    });
}

fn corner_color(image: &image::RgbImage) -> [u8; 3] {
    let width = image.width();
    let height = image.height();
    let patch = 8;
    let mut sum = [0u64; 3];
    let mut count = 0u64;
    for (x0, y0) in [
        (0, 0),
        (width.saturating_sub(patch), 0),
        (0, height.saturating_sub(patch)),
        (width.saturating_sub(patch), height.saturating_sub(patch)),
    ] {
        for y in y0..(y0 + patch).min(height) {
            for x in x0..(x0 + patch).min(width) {
                let pixel = image.get_pixel(x, y).0;
                for (channel, value) in pixel.iter().zip(sum.iter_mut()) {
                    *value += u64::from(*channel);
                }
                count += 1;
            }
        }
    }
    let count = count.max(1);
    [
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
    ]
}
