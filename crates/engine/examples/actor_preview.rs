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
//!
//! `--dark-skin-check MAX_FRACTION` turns the render into a regression check: after the PNG is
//! written, the actor's pixels (everything that differs from the background) are tested against a
//! luminance floor (`--dark-floor`, default 0.10 on the encoded 0..1 scale), and the run exits
//! non-zero when more than `MAX_FRACTION` of them fall below it. A model-space normal map decoded
//! as a tangent-space one shows up as large near-black patches on the back and shoulders, which
//! this catches without comparing against a reference image.
//!
//! The body, hands and feet take an NPC's skin tone (`NPC_/QNAM`) the way the engine's actors do
//! (`engine::actor_skin_tone`). `--skin-tone R,G,B` sets it directly; otherwise, with
//! `--database PATH` (a converted `skyrim_world.db` with the actor catalog), it is read from the
//! catalog row of `--npc FORMID`, by default Alvor (`00013475`) for `--sex male` and Sigrid
//! (`00013476`) for `--sex female`, whose FaceGen heads the preview draws. With neither, the skin
//! is untinted.
//!
//! The body takes the same NPC's weight (`NPC_/NAM7`) through the `thin` morph target
//! (`engine::actor_body_weight`). `--weight 0..100` sets it directly; otherwise it comes from the
//! same catalog row. With neither, the body stays heavy.
//!
//! `--debug-normals msn|vertex` renders the skin materials' normals as colour (`N * 0.5 + 0.5`,
//! written so the PNG holds that value) instead of lighting them: `msn` the shading normal decoded
//! from the model-space map, `vertex` the interpolated vertex normal. Red is world +X, green +Y
//! (up), blue +Z. The actor faces -Z, where the camera stands at yaw 0, so the front has almost no
//! blue and the back (seen at yaw 180) is blue.
//!
//! `--body PATH` loads another composed body GLB (relative to the converted tree) in place of the
//! default one for `--sex`.
//!
//! `--focus` frames the whole `body`, the `upper` body (head and chest) or the `head`.
//!
//! `--frames N` switches to a sequence: instead of one PNG it writes `<stem>_0000.png` up to
//! `<stem>_{N-1}.png` next to the output path, all from one process. Every frame advances the idle
//! clip by exactly `1 / --fps` seconds (default 30) of virtual time, whatever the capture costs:
//! the clock only steps once the previous frame is on disk, so the animation never skips or
//! repeats a pose. `--orbit DEGREES` turns the camera rig (camera and both lights) around the
//! actor by that much over the sequence, starting at `--yaw`, and `--focus-end body|upper|head`
//! eases the framing from `--focus` to that box, for a push-in. Every frame is checked for being
//! empty, and the run prints how much consecutive frames differ.

use bevy::time::TimeUpdateStrategy;
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
    actor_body_weight::{ActorBodyWeight, ActorBodyWeightPlugin},
    actor_skin_tone::{ActorSkinTone, ActorSkinTonePlugin},
    actors::{ActorSex, composed_body_scene},
    nif_material::NifMaterialPlugin,
    skin_material::{SkinDebugView, SkyrimSkinMaterial},
    world::actor_catalog::ActorCatalog,
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
const FEMALE_HEAD: &str = "meshes/actors/character/facegendata/facegeom/skyrim.esm/00013476.glb";
/// The `NPC_` records whose heads `MALE_HEAD` and `FEMALE_HEAD` are: Alvor and Sigrid.
const MALE_NPC: u32 = 0x0001_3475;
const FEMALE_NPC: u32 = 0x0001_3476;
const BACKGROUND: Color = Color::srgb(0.11, 0.13, 0.16);
/// The head's own scene is discarded when even one of its joints has no body bone.
const MIN_SKIN_FRACTION: f32 = 0.01;
const MIN_LUMINANCE: f32 = 0.02;
/// Per-channel distance from the background colour above which a pixel counts as the actor.
const FOREGROUND_DISTANCE: u16 = 16;
/// The dark-skin check uses a tighter actor mask: the blotches it looks for sit within
/// `FOREGROUND_DISTANCE` of the dark background, while the cleared background itself is exact.
const DARK_CHECK_DISTANCE: u16 = 4;
/// Encoded luminance of the darkest blotches a tangent-decoded model-space map leaves on the
/// back (about RGB 22,21,23), with headroom; lit skin sits well above it.
const DEFAULT_DARK_FLOOR: f64 = 0.10;
/// Height of the head framing box centre above the head bone. The camera sits and aims above the
/// box centre, so a small lift keeps the face mid-frame with the crown and chin in view.
const HEAD_FRAME_LIFT: f32 = 2.0;
/// Offset of the upper-body framing box centre from the head bone: the box reaches from just
/// above the crown to the chest.
const UPPER_FRAME_DROP: f32 = -8.0;

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
    /// `--frames`: capturing the sequence one frame at a time.
    Sequence,
    Captured,
}

/// The part of the actor the camera frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Framing {
    #[default]
    Body,
    Upper,
    Head,
}

impl Framing {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value {
            Some("body") => Some(Self::Body),
            Some("upper") => Some(Self::Upper),
            Some("head") => Some(Self::Head),
            _ => None,
        }
    }
}

/// Where the camera stands relative to the default front-three-quarter view.
#[derive(Debug, Clone, Copy, Default)]
struct View {
    yaw_degrees: f32,
    pitch_degrees: f32,
    framing: Framing,
}

/// `--frames`: a fixed-step image sequence instead of a single PNG.
#[derive(Debug, Clone, Copy)]
struct Sequence {
    frames: u32,
    orbit_degrees: f32,
    end_framing: Option<Framing>,
    step: Duration,
}

/// Tags the entities `spawn_camera` places, so a sequence can move the rig per frame.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum RigPart {
    Camera,
    Key,
    Fill,
}

/// Progress of a `--frames` capture.
#[derive(Default)]
struct SequenceState {
    start_bounds: (Vec3, Vec3),
    end_bounds: (Vec3, Vec3),
    /// Index of the next frame to request.
    next: u32,
    /// The frame whose screenshot has been requested but not yet written.
    pending: Option<u32>,
    /// The clock was set to step on the coming update, so the next request sees a new pose.
    armed: bool,
    written: u32,
    empty_frames: u32,
    identical_frames: u32,
    previous: Option<Vec<u8>>,
    /// Mean absolute per-channel difference between consecutive frames, on a 0..255 scale.
    differences: Vec<f64>,
}

/// Fails the run when too many actor pixels are darker than `floor`.
#[derive(Debug, Clone, Copy)]
struct DarkSkinCheck {
    max_fraction: f64,
    floor: f64,
}

/// Everything the command line selects.
struct Options {
    demo: PathBuf,
    output: PathBuf,
    sex: ActorSex,
    head: HeadChoice,
    body: Option<String>,
    windowed: bool,
    textures: bool,
    view: View,
    msaa: Msaa,
    /// Skin tone in 0..1 on the encoded scale of an `NPC_` record's `QNAM`.
    skin_tone: Option<[f32; 3]>,
    /// `NPC_/NAM7` body weight in 0..100; the morph value is `1 - weight / 100`.
    weight: Option<f32>,
    /// A world database whose actor catalog supplies the tone when `skin_tone` is not given.
    database: Option<PathBuf>,
    npc: Option<u32>,
    dark_skin_check: Option<f64>,
    dark_floor: f64,
    debug_normals: SkinDebugView,
    sequence: Option<Sequence>,
}

#[derive(Resource)]
struct Preview {
    output: PathBuf,
    view: View,
    windowed: bool,
    textures: bool,
    msaa: Msaa,
    dark_skin_check: Option<DarkSkinCheck>,
    debug_normals: SkinDebugView,
    skin_tone: Option<ActorSkinTone>,
    body_weight: Option<f32>,
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
    sequence: Option<Sequence>,
    sequence_state: SequenceState,
}

impl Preview {
    fn fail(&self, message: &str) {
        eprintln!("actor preview: {message}");
    }
}

fn main() {
    let Options {
        demo,
        output,
        sex,
        head,
        body,
        windowed,
        textures,
        view,
        msaa,
        skin_tone,
        weight,
        database,
        npc,
        dark_skin_check,
        dark_floor,
        debug_normals,
        sequence,
    } = parse_args();
    let npc = npc.unwrap_or(match sex {
        ActorSex::Male => MALE_NPC,
        ActorSex::Female => FEMALE_NPC,
    });
    let skin_tone = resolve_skin_tone(skin_tone, database.as_deref(), npc);
    let body_weight = resolve_body_weight(weight, database.as_deref(), npc);
    if let Err(error) = check_inputs(&demo, sex) {
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
    // The engine builds converted NIF materials through this glTF hook; without it the preview
    // would render the stock glTF materials instead of what the game draws.
    app.add_plugins((
        NifMaterialPlugin,
        ActorSkinTonePlugin,
        ActorBodyWeightPlugin,
    ));
    if !windowed {
        app.add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
            Duration::from_millis(16),
        ));
    }
    if let Some(sequence) = sequence {
        // Virtual time follows this step instead of the wall clock, so the settle and every
        // captured frame see the same amount of animation regardless of render speed.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(sequence.step));
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
    let body_path = body.unwrap_or_else(|| composed_body_scene(sex).to_owned());
    let body_scene = asset_server.load(GltfAssetLabel::Scene(0).from_asset(body_path));
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
        msaa,
        dark_skin_check: dark_skin_check.map(|max_fraction| DarkSkinCheck {
            max_fraction,
            floor: dark_floor,
        }),
        debug_normals,
        skin_tone,
        body_weight,
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
        sequence,
        sequence_state: SequenceState::default(),
    })
    .add_systems(Update, (drive, drive_sequence).chain());

    let result = app.run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

fn usage_error(message: &str) -> ! {
    eprintln!("actor preview: {message}");
    std::process::exit(2);
}

fn parse_fraction(flag: &str, value: Option<String>) -> f64 {
    match value.and_then(|value| value.parse::<f64>().ok()) {
        Some(value) if (0.0..=1.0).contains(&value) => value,
        _ => usage_error(&format!("{flag} needs a number between 0 and 1")),
    }
}

fn parse_msaa(value: Option<String>) -> Msaa {
    match value.as_deref() {
        Some("1" | "off") => Msaa::Off,
        Some("2") => Msaa::Sample2,
        Some("4") => Msaa::Sample4,
        Some("8") => Msaa::Sample8,
        _ => usage_error("--msaa needs 1, 2, 4 or 8"),
    }
}

fn parse_skin_tone(value: Option<String>) -> [f32; 3] {
    let channels: Option<Vec<f32>> = value.as_deref().map(|value| {
        value
            .split(',')
            .filter_map(|channel| channel.trim().parse::<f32>().ok())
            .filter(|channel| (0.0..=1.0).contains(channel))
            .collect()
    });
    match channels.as_deref() {
        Some(&[red, green, blue]) => [red, green, blue],
        _ => usage_error("--skin-tone needs R,G,B with each channel between 0 and 1"),
    }
}

fn parse_body_weight(value: Option<String>) -> f32 {
    match value.as_deref().map(str::trim).map(str::parse::<f32>) {
        Some(Ok(weight)) if (0.0..=100.0).contains(&weight) => weight,
        _ => usage_error("--weight needs a number between 0 and 100"),
    }
}

/// The morph value for the preview's `thin` body target: `--weight` when given, else the catalog
/// row of `npc` in `database`, else none (the heavy body).
fn resolve_body_weight(explicit: Option<f32>, database: Option<&Path>, npc: u32) -> Option<f32> {
    if let Some(weight) = explicit {
        let morph = 1.0 - weight / 100.0;
        println!("actor preview: body weight {weight:.0} (morph thin {morph:.3}) from --weight");
        return Some(morph);
    }
    let database = database?;
    if !database.is_file() {
        usage_error(&format!("--database {} is not a file", database.display()));
    }
    let catalog = ActorCatalog::open(database);
    let Some(model) = catalog.get(npc) else {
        println!(
            "actor preview: NPC_ {npc:08X} has no actor catalog row in {}; the body stays heavy",
            database.display()
        );
        return None;
    };
    match model.weight {
        Some(weight) => {
            let morph = 1.0 - weight / 100.0;
            println!(
                "actor preview: body weight {weight:.0} (morph thin {morph:.3}) from NPC_ {npc:08X} NAM7"
            );
            Some(morph)
        }
        None => {
            println!(
                "actor preview: NPC_ {npc:08X} has no body weight in {}; the body stays heavy",
                database.display()
            );
            None
        }
    }
}

/// The tone the preview applies: `--skin-tone` when given, else the catalog row of `npc` in
/// `database`, else none.
fn resolve_skin_tone(
    explicit: Option<[f32; 3]>,
    database: Option<&Path>,
    npc: u32,
) -> Option<ActorSkinTone> {
    if let Some(tone) = explicit {
        let tone = ActorSkinTone::new(tone);
        if let Some(ActorSkinTone(rgb)) = tone {
            println!(
                "actor preview: skin tone {:.3},{:.3},{:.3} from --skin-tone",
                rgb.x, rgb.y, rgb.z
            );
        }
        return tone;
    }
    let database = database?;
    if !database.is_file() {
        usage_error(&format!("--database {} is not a file", database.display()));
    }
    let catalog = ActorCatalog::open(database);
    let Some(model) = catalog.get(npc) else {
        println!(
            "actor preview: NPC_ {npc:08X} has no actor catalog row in {}; the skin stays untinted",
            database.display()
        );
        return None;
    };
    match model.skin_tone {
        Some(tone) => {
            println!(
                "actor preview: skin tone {:.3},{:.3},{:.3} ({:.0},{:.0},{:.0}/255) from NPC_ {npc:08X} QNAM",
                tone.0.x,
                tone.0.y,
                tone.0.z,
                tone.0.x * 255.0,
                tone.0.y * 255.0,
                tone.0.z * 255.0
            );
            Some(tone)
        }
        None => {
            println!(
                "actor preview: NPC_ {npc:08X} has no skin tone in {}; the skin stays untinted",
                database.display()
            );
            None
        }
    }
}

fn parse_form_id(value: Option<String>) -> u32 {
    value
        .as_deref()
        .map(|value| value.trim_start_matches("0x"))
        .and_then(|value| u32::from_str_radix(value, 16).ok())
        .unwrap_or_else(|| usage_error("--npc needs a hexadecimal FormID"))
}

fn parse_args() -> Options {
    let mut args = std::env::args().skip(1);
    let usage = "usage: actor_preview <converted_assets_dir> <output.png> [--sex male|female] [--head PATH|none] [--body PATH] [--textures] [--window] [--yaw DEG] [--pitch DEG] [--focus body|upper|head] [--msaa 1|2|4|8] [--skin-tone R,G,B] [--weight 0..100] [--database WORLD_DB] [--npc FORMID] [--dark-skin-check MAX_FRACTION] [--dark-floor LUMINANCE] [--debug-normals msn|vertex] [--frames N [--fps FPS] [--orbit DEG] [--focus-end body|upper|head]]";
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
    let mut body = None;
    let mut windowed = false;
    let mut textures = false;
    let mut view = View::default();
    let mut msaa = Msaa::Off;
    let mut skin_tone = None;
    let mut weight = None;
    let mut database = None;
    let mut npc = None;
    let mut dark_skin_check = None;
    let mut dark_floor = DEFAULT_DARK_FLOOR;
    let mut debug_normals = SkinDebugView::Off;
    let mut frames = None;
    let mut fps = 30.0f32;
    let mut orbit_degrees = 0.0f32;
    let mut end_framing = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--frames" => {
                frames = match args.next().and_then(|value| value.parse::<u32>().ok()) {
                    Some(count) if count > 0 => Some(count),
                    _ => usage_error("--frames needs a positive frame count"),
                }
            }
            "--fps" => {
                fps = match args.next().and_then(|value| value.parse::<f32>().ok()) {
                    Some(rate) if rate.is_finite() && rate > 0.0 => rate,
                    _ => usage_error("--fps needs a positive frame rate"),
                }
            }
            "--orbit" => {
                orbit_degrees = match args.next().and_then(|value| value.parse::<f32>().ok()) {
                    Some(degrees) if degrees.is_finite() => degrees,
                    _ => usage_error("--orbit needs a number of degrees"),
                }
            }
            "--focus-end" => {
                end_framing = match Framing::parse(args.next().as_deref()) {
                    Some(framing) => Some(framing),
                    None => usage_error("--focus-end needs body, upper or head"),
                }
            }
            "--debug-normals" => {
                debug_normals = match args.next().as_deref().and_then(SkinDebugView::parse) {
                    Some(view) => view,
                    None => usage_error("--debug-normals needs msn, vertex or off"),
                }
            }
            "--msaa" => msaa = parse_msaa(args.next()),
            "--skin-tone" => skin_tone = Some(parse_skin_tone(args.next())),
            "--weight" => weight = Some(parse_body_weight(args.next())),
            "--database" => {
                database = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage_error("--database needs a path")),
                ))
            }
            "--npc" => npc = Some(parse_form_id(args.next())),
            "--dark-skin-check" => dark_skin_check = Some(parse_fraction(&flag, args.next())),
            "--dark-floor" => dark_floor = parse_fraction(&flag, args.next()),
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
                view.framing = match Framing::parse(args.next().as_deref()) {
                    Some(framing) => framing,
                    None => usage_error("--focus needs body, upper or head"),
                };
            }
            "--textures" => textures = true,
            "--body" => {
                body = Some(
                    args.next()
                        .unwrap_or_else(|| usage_error("--body needs a GLB path")),
                )
            }
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
    let sequence = frames.map(|frames| Sequence {
        frames,
        orbit_degrees,
        end_framing,
        step: Duration::from_secs_f64(1.0 / f64::from(fps)),
    });
    if sequence.is_none() && (orbit_degrees != 0.0 || end_framing.is_some()) {
        usage_error("--orbit and --focus-end need --frames");
    }
    if sequence.is_some() && (windowed || dark_skin_check.is_some()) {
        usage_error("--frames cannot be combined with --window or --dark-skin-check");
    }
    Options {
        demo: PathBuf::from(demo),
        output: PathBuf::from(output),
        sex,
        head,
        body,
        windowed,
        textures,
        view,
        msaa,
        skin_tone,
        weight,
        database,
        npc,
        dark_skin_check,
        dark_floor,
        debug_normals,
        sequence,
    }
}

fn check_inputs(demo: &Path, sex: ActorSex) -> Result<(), String> {
    let sex_name = sex.as_str();
    if !demo.join(composed_body_scene(sex)).is_file() {
        return Err(format!(
            "{} has no {sex_name} composed body",
            demo.display()
        ));
    }
    if !demo.join(idle_clip_scene(sex)).is_file() {
        return Err(format!("{} has no {sex_name} idle clip", demo.display()));
    }
    Ok(())
}

/// Mesh entities whose material the flat preview replaces.
type MaterialBound = Or<(
    With<MeshMaterial3d<StandardMaterial>>,
    With<MeshMaterial3d<SkyrimSkinMaterial>>,
)>;

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
    material_entities: Query<(), MaterialBound>,
    mut skin_materials: ResMut<Assets<SkyrimSkinMaterial>>,
    transforms: Query<&Transform>,
    mut exit: MessageWriter<AppExit>,
) {
    let time_limit = TIME_LIMIT
        + preview.sequence.map_or(Duration::ZERO, |sequence| {
            Duration::from_secs(sequence.frames.into())
        });
    if preview.started.elapsed() > time_limit {
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
                let mut actor = commands.spawn((
                    Name::new("Actor preview"),
                    WorldAssetRoot(preview.body_scene.clone()),
                ));
                if let Some(tone) = preview.skin_tone {
                    actor.insert(tone);
                }
                if let Some(weight) = preview.body_weight {
                    actor.insert(ActorBodyWeight(weight));
                }
                let actor = actor.id();
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
                let bounds = framing_bounds(&preview, &globals, preview.view.framing);
                if let Some(sequence) = preview.sequence {
                    let end = sequence.end_framing.map_or(bounds, |framing| {
                        framing_bounds(&preview, &globals, framing)
                    });
                    preview.sequence_state.start_bounds = bounds;
                    preview.sequence_state.end_bounds = end;
                }
                if !preview.textures {
                    if let Some(body_root) = preview.body_root {
                        for entity in
                            std::iter::once(body_root).chain(children.iter_descendants(body_root))
                        {
                            if material_entities.contains(entity) {
                                commands
                                    .entity(entity)
                                    .remove::<MeshMaterial3d<SkyrimSkinMaterial>>()
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
                                    .remove::<MeshMaterial3d<SkyrimSkinMaterial>>()
                                    .insert(MeshMaterial3d(preview.head_material.clone()));
                            }
                        }
                    }
                }
                if preview.debug_normals != SkinDebugView::Off {
                    let mut count = 0;
                    for (_, material) in skin_materials.iter_mut() {
                        preview
                            .debug_normals
                            .apply(&mut material.extension.settings);
                        count += 1;
                    }
                    println!(
                        "actor preview: {count} skin materials render {:?}",
                        preview.debug_normals
                    );
                }
                spawn_camera(&mut commands, &preview, bounds);
                preview.phase = Phase::Rendering;
            }
        }
    }

    if preview.phase == Phase::Rendering {
        preview.camera_frames += 1;
        if preview.camera_frames >= CAMERA_FRAMES {
            if preview.sequence.is_some() {
                preview.phase = Phase::Sequence;
                return;
            }
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

/// Captures a `--frames` sequence one frame at a time.
///
/// Each frame is requested on an update whose clock stepped by exactly one frame, and the clock
/// is then held (`ManualDuration(ZERO)`) until that frame is written. Re-arming the step takes one
/// extra update, because the strategy is read at the start of the next frame.
fn drive_sequence(
    mut commands: Commands,
    mut preview: ResMut<Preview>,
    mut strategy: ResMut<TimeUpdateStrategy>,
    mut rig: Query<(&RigPart, &mut Transform, Option<&mut Projection>)>,
    mut exit: MessageWriter<AppExit>,
) {
    if preview.phase != Phase::Sequence {
        return;
    }
    let Some(sequence) = preview.sequence else {
        return;
    };
    if preview.sequence_state.pending.is_some() {
        return;
    }
    if preview.sequence_state.written >= sequence.frames {
        finish_sequence(&preview, sequence, &mut exit);
        preview.phase = Phase::Captured;
        return;
    }
    if !preview.sequence_state.armed {
        *strategy = TimeUpdateStrategy::ManualDuration(sequence.step);
        preview.sequence_state.armed = true;
        return;
    }

    let frame = preview.sequence_state.next;
    let progress = if sequence.frames > 1 {
        frame as f32 / (sequence.frames - 1) as f32
    } else {
        0.0
    };
    let ease = progress * progress * (3.0 - 2.0 * progress);
    let (start_min, start_max) = preview.sequence_state.start_bounds;
    let (end_min, end_max) = preview.sequence_state.end_bounds;
    let bounds = (start_min.lerp(end_min, ease), start_max.lerp(end_max, ease));
    let view = View {
        yaw_degrees: preview.view.yaw_degrees
            + sequence.orbit_degrees * frame as f32 / sequence.frames as f32,
        ..preview.view
    };
    let layout = rig_layout(view, bounds);
    for (part, mut transform, projection) in &mut rig {
        *transform = match part {
            RigPart::Camera => layout.camera,
            RigPart::Key => layout.key,
            RigPart::Fill => layout.fill,
        };
        if let Some(mut projection) = projection {
            *projection = layout.projection();
        }
    }

    commands
        .spawn(Screenshot::image(preview.target.clone()))
        .observe(write_sequence_frame);
    preview.sequence_state.pending = Some(frame);
    preview.sequence_state.next = frame + 1;
    preview.sequence_state.armed = false;
    *strategy = TimeUpdateStrategy::ManualDuration(Duration::ZERO);
}

/// `<dir>/<stem>_0000.png` for frame 0 of `output`.
fn sequence_frame_path(output: &Path, frame: u32) -> PathBuf {
    let stem = output
        .file_stem()
        .map_or_else(|| "frame".into(), |stem| stem.to_string_lossy());
    output.with_file_name(format!("{stem}_{frame:04}.png"))
}

fn write_sequence_frame(
    event: On<ScreenshotCaptured>,
    mut preview: ResMut<Preview>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(frame) = preview.sequence_state.pending else {
        return;
    };
    let Ok(dynamic) = event.image.clone().try_into_dynamic() else {
        eprintln!("actor preview: frame {frame} cannot be converted to a PNG");
        exit.write(AppExit::error());
        return;
    };
    let image = dynamic.to_rgb8();
    let pixels = f64::from(image.width()) * f64::from(image.height());
    let mean_luminance = image
        .pixels()
        .map(|pixel| pixel_luminance(pixel.0))
        .sum::<f64>()
        / pixels;
    let background = corner_color(&image);
    let foreground = image
        .pixels()
        .filter(|pixel| {
            pixel.0.iter().zip(background).any(|(channel, background)| {
                u16::from(*channel).abs_diff(u16::from(background)) > FOREGROUND_DISTANCE
            })
        })
        .count() as f64
        / pixels;
    let path = sequence_frame_path(&preview.output, frame);
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        eprintln!("actor preview: cannot create {}: {error}", parent.display());
        exit.write(AppExit::error());
        return;
    }
    if let Err(error) = image.save(&path) {
        eprintln!("actor preview: cannot write {}: {error}", path.display());
        exit.write(AppExit::error());
        return;
    }

    let state = &mut preview.sequence_state;
    if foreground < f64::from(MIN_SKIN_FRACTION) || mean_luminance < f64::from(MIN_LUMINANCE) {
        eprintln!(
            "actor preview: frame {frame} looks empty (foreground {foreground:.3}, luminance {mean_luminance:.3})"
        );
        state.empty_frames += 1;
    }
    let raw = image.into_raw();
    if let Some(previous) = &state.previous {
        let total: u64 = previous
            .iter()
            .zip(&raw)
            .map(|(before, after)| u64::from(before.abs_diff(*after)))
            .sum();
        let difference = total as f64 / raw.len() as f64;
        if total == 0 {
            state.identical_frames += 1;
        }
        state.differences.push(difference);
    }
    state.previous = Some(raw);
    state.pending = None;
    state.written += 1;
}

fn finish_sequence(preview: &Preview, sequence: Sequence, exit: &mut MessageWriter<AppExit>) {
    let state = &preview.sequence_state;
    let (min, max, mean) = if state.differences.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        let min = state
            .differences
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        let max = state.differences.iter().copied().fold(0.0, f64::max);
        let mean = state.differences.iter().sum::<f64>() / state.differences.len() as f64;
        (min, max, mean)
    };
    println!(
        "actor preview: wrote {} frames {} .. {} at {:.3} s per frame, orbit {} degrees",
        state.written,
        sequence_frame_path(&preview.output, 0).display(),
        sequence_frame_path(&preview.output, sequence.frames - 1).display(),
        sequence.step.as_secs_f64(),
        sequence.orbit_degrees
    );
    println!(
        "actor preview: consecutive frame difference min {min:.3} mean {mean:.3} max {max:.3}; {} identical, {} empty",
        state.identical_frames, state.empty_frames
    );
    println!(
        "actor preview: {} body scene nodes, {} animation targets bound, {} bones moved since bind, {} head skins rehosted, {} head nodes",
        preview.body_nodes,
        preview.targets.len(),
        preview.moved_bones,
        preview.head_skins,
        preview.head_nodes
    );
    exit.write(if state.empty_frames == 0 {
        AppExit::Success
    } else {
        AppExit::error()
    });
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

fn framing_bounds(
    preview: &Preview,
    globals: &Query<&GlobalTransform>,
    framing: Framing,
) -> (Vec3, Vec3) {
    match framing {
        Framing::Body => body_bounds(preview, globals),
        Framing::Upper => upper_bounds(preview, globals),
        Framing::Head => head_bounds(preview, globals),
    }
}

/// A box from the crown down to the chest, centred under the head bone.
fn upper_bounds(preview: &Preview, globals: &Query<&GlobalTransform>) -> (Vec3, Vec3) {
    let Some(head) = preview
        .bone_names
        .get("NPC Head [Head]")
        .and_then(|entity| globals.get(*entity).ok())
    else {
        return body_bounds(preview, globals);
    };
    let center = head.translation() + Vec3::new(0.0, UPPER_FRAME_DROP, 0.0);
    let half = Vec3::new(14.0, 17.0, 11.0);
    (center - half, center + half)
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
    let center = head.translation() + Vec3::new(0.0, HEAD_FRAME_LIFT, 0.0);
    let half = Vec3::new(9.0, 10.0, 9.0);
    (center - half, center + half)
}

/// Camera and light placement for one view of `bounds`.
struct RigLayout {
    camera: Transform,
    key: Transform,
    fill: Transform,
    fov: f32,
    distance: f32,
}

impl RigLayout {
    fn projection(&self) -> Projection {
        Projection::Perspective(PerspectiveProjection {
            fov: self.fov,
            aspect_ratio: WIDTH as f32 / HEIGHT as f32,
            near: self.distance * 0.01,
            far: self.distance * 10.0,
            ..default()
        })
    }
}

fn rig_layout(view: View, (min, max): (Vec3, Vec3)) -> RigLayout {
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
    let orbit = Quat::from_rotation_y(view.yaw_degrees.to_radians());
    let around = |offset: Vec3| center + orbit * offset;
    let pitch = view.pitch_degrees.to_radians();
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

    RigLayout {
        camera: Transform::from_translation(camera_position).looking_at(look_at, Vec3::Y),
        key: Transform::from_translation(around(Vec3::new(
            -distance * 0.6,
            max.y - center.y + size.y * 0.8,
            -distance * 0.9,
        )))
        .looking_at(center, Vec3::Y),
        fill: Transform::from_translation(around(Vec3::new(
            distance * 0.8,
            size.y * 0.3,
            -distance * 0.4,
        )))
        .looking_at(center, Vec3::Y),
        fov,
        distance,
    }
}

fn spawn_camera(commands: &mut Commands, preview: &Preview, bounds: (Vec3, Vec3)) {
    let layout = rig_layout(preview.view, bounds);
    commands.spawn((
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(BACKGROUND),
            ..default()
        },
        layout.projection(),
        if preview.windowed {
            RenderTarget::Window(WindowRef::Primary)
        } else {
            RenderTarget::Image(preview.target.clone().into())
        },
        preview.msaa,
        layout.camera,
        RigPart::Camera,
    ));

    commands.spawn((
        DirectionalLight {
            color: Color::srgb(1.0, 0.96, 0.9),
            illuminance: 12_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        layout.key,
        RigPart::Key,
    ));
    commands.spawn((
        DirectionalLight {
            color: Color::srgb(0.55, 0.65, 0.85),
            illuminance: 3_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        layout.fill,
        RigPart::Fill,
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
        luminance += pixel_luminance(pixel.0);
    }
    let mean_luminance = luminance / (f64::from(width) * f64::from(height));

    let background = corner_color(&image);
    let dark_floor = preview
        .dark_skin_check
        .map_or(DEFAULT_DARK_FLOOR, |check| check.floor);
    let mut foreground = 0u64;
    let mut actor = 0u64;
    let mut dark = 0u64;
    for pixel in image.pixels() {
        let distance = pixel
            .0
            .iter()
            .zip(background)
            .map(|(channel, background)| u16::from(*channel).abs_diff(u16::from(background)))
            .max()
            .unwrap_or(0);
        if distance > FOREGROUND_DISTANCE {
            foreground += 1;
        }
        if distance > DARK_CHECK_DISTANCE {
            actor += 1;
            if pixel_luminance(pixel.0) < dark_floor {
                dark += 1;
            }
        }
    }
    let foreground_fraction = foreground as f64 / (f64::from(width) * f64::from(height));
    let dark_fraction = if actor == 0 {
        0.0
    } else {
        dark as f64 / actor as f64
    };

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

    let mut passed = foreground_fraction >= f64::from(MIN_SKIN_FRACTION)
        && mean_luminance >= f64::from(MIN_LUMINANCE);
    if !passed {
        eprintln!(
            "actor preview: the frame looks empty (foreground {foreground_fraction:.3}, luminance {mean_luminance:.3})"
        );
    }
    println!(
        "actor preview: dark actor fraction {dark_fraction:.4} ({dark} of {actor} actor pixels below luminance {dark_floor:.3})"
    );
    if let Some(check) = preview.dark_skin_check
        && dark_fraction > check.max_fraction
    {
        eprintln!(
            "actor preview: dark skin check failed: {dark_fraction:.4} of the actor is below luminance {:.3}, limit {:.4}",
            check.floor, check.max_fraction
        );
        passed = false;
    }
    exit.write(if passed {
        AppExit::Success
    } else {
        AppExit::error()
    });
}

/// Rec. 709 luminance of an encoded 8-bit pixel, on a 0..1 scale.
fn pixel_luminance([red, green, blue]: [u8; 3]) -> f64 {
    (0.2126 * f64::from(red) + 0.7152 * f64::from(green) + 0.0722 * f64::from(blue)) / 255.0
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
