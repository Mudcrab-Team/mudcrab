//! Interactive NPC viewer: one assembled actor at a time with its idle, outfit and a status label.
//!
//! ```text
//! cargo run -p engine --example npc_viewer -- --assets <converted pack> [--npc <FormID|EditorID>]...
//! ```
//!
//! Each NPC spawns through the same components the streamed world uses: the composed body, the
//! FaceGen head, the worn default outfit (`actor-outfits.json`), skin tone, body weight and the
//! shared idle clip. The on-screen label names the NPC, its race and outfit, and lists every worn
//! part as attached or failed.
//!
//! Keys: `N`/`PageDown`/`Tab` next, `P`/`PageUp`/`Shift+Tab` previous, drag or arrows to orbit,
//! scroll or Up/Down to zoom, `R` resets the camera, `Space` pauses the idle, `H` hides the label,
//! `F12` saves a PNG next to the working directory, `Esc` quits.
//!
//! `--npc` adds NPCs (hex FormID or EditorID) after the curated list and starts on the first one
//! given; `--only` drops the curated list. `--list` prints the resolved list and exits.
//! `--capture <new dir>` runs without a window: it steps through the list, writes PNGs from several
//! angles plus `capture.json`, and exits.
use bevy::{
    animation::AnimationPlayer,
    asset::LoadState,
    camera::{Exposure, RenderTarget, primitives::Aabb},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    gltf::GltfAssetLabel,
    input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll},
    mesh::skinning::SkinnedMesh,
    prelude::*,
    render::{
        RenderPlugin,
        render_resource::TextureFormat,
        settings::{Backends, RenderCreation, WgpuFeatures, WgpuSettings, WgpuSettingsPriority},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    time::TimeUpdateStrategy,
    window::ExitCondition,
    world_serialization::{WorldAsset, WorldAssetRoot},
};
use engine::{
    actor_animation::ActorAnimationPlugin,
    actor_body_weight::ActorBodyWeightPlugin,
    actor_drive::ActorDrivePlugin,
    actor_head::{ActorHeadInstance, ActorHeadPlugin},
    actor_outfit::{ActorOutfitInstance, ActorOutfitPlugin},
    actor_skin_tone::ActorSkinTonePlugin,
    nif_material::{EffectPaletteMaterialPlugin, NifMaterialPlugin},
    streaming::converted_model_path,
    world::actor_catalog::{ActorAnimationSetup, ActorCatalog, ActorSpawnPlan},
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use shared::actor_outfits::OutfitPart;
use std::{
    f32::consts::{FRAC_PI_2, PI},
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

/// The stepping list: `(NPC_ FormID, why it is here)`. FormIDs are stable; names come from the pack.
const CURATED: &[(u32, &str)] = &[
    (0x13669, "Bjorlam, farmer robe, boots, gloves"),
    (0x13475, "Riverwood: blacksmith"),
    (0x13476, "Riverwood: innkeeper"),
    (0x1347C, "Riverwood"),
    (0x1347D, "Riverwood: heavy build"),
    (0x13480, "Riverwood: wood elf"),
    (0x1347F, "Riverwood"),
    (0x1347B, "Riverwood: Imperial"),
    (0x1347A, "Riverwood: Imperial, thin"),
    (0x13479, "Riverwood"),
    (0x13478, "Riverwood: Breton"),
    (0x13477, "Child (Riverwood)"),
    (0x1347E, "Child (Riverwood)"),
    (0x13668, "Child (Whiterun)"),
    (0xD1985, "Whiterun guard: no worn outfit data in the pack"),
    (0xA27CC, "Whiterun guard: no worn outfit data in the pack"),
    (0x1B1D2, "Khajiit in Nord plate and gauntlets"),
    (0x1C195, "Khajiit mage"),
    (0x1328D, "Argonian in hide"),
    (0x1335A, "Argonian in hide, helmet and gauntlets"),
    (
        0x537F2,
        "Heavy armor: Markarth guard cuirass, gauntlets, shield",
    ),
    (0x40195AF, "Heavy armor: Redoran guard, helmet"),
    (0x1C1A0, "Mage robe: Mirabelle Ervine"),
    (0x1C19E, "Mage robe: Tolfdir"),
    (0x13BBB, "Mage robe: Farengar, hood"),
    (0x13288, "Thin (weight 0), hat"),
    (0x13273, "Thin (weight 0), robe"),
    (0x13290, "Heavy (weight 100), dress, hat"),
    (0x1334C, "Heavy (weight 100), blacksmith"),
];

const IDLE_FRAMES: u32 = 45;
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Frames a loaded part scene may go without a drawn mesh before it counts as dropped.
const DROPPED_AFTER_FRAMES: u32 = 40;
/// Textures keep arriving after a scene is ready: a capture waits this long, and this many frames.
const TEXTURE_SETTLE: Duration = Duration::from_millis(1500);
const TEXTURE_SETTLE_FRAMES: u32 = 90;

#[derive(Clone)]
struct Npc {
    form_id: u32,
    editor_id: String,
    name: String,
    race_id: u32,
    race: String,
    note: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum PartState {
    Loading,
    Attached,
    LoadFailed,
    Dropped,
}

/// How a tracked part is found among the actor's instantiated scenes.
enum Identity {
    Head,
    Outfit(OutfitPart),
}

struct Tracked {
    label: String,
    scene: String,
    handle: Handle<WorldAsset>,
    mask: u32,
    /// Which instantiated scene belongs to this part: matched by the engine's own components.
    identity: Identity,
    state: PartState,
    loaded_frames: u32,
}

struct Shown {
    npc: Npc,
    root: Entity,
    plan: Option<ActorSpawnPlan>,
    skip: Option<String>,
    outfit_id: Option<u32>,
    outfit_name: Option<String>,
    head: Option<Tracked>,
    parts: Vec<Tracked>,
    started: Instant,
    frames: u32,
    idle_expected: bool,
    idle_playing: bool,
    ready_frames: u32,
    ready_since: Option<Instant>,
    revealed: bool,
    timed_out: bool,
    hidden_meshes: Vec<String>,
}

#[derive(Clone, Copy)]
struct Shot {
    name: &'static str,
    yaw: f32,
    pitch: f32,
    focus: f32,
    zoom: f32,
    later_seconds: f32,
}

const SHOTS: &[Shot] = &[
    shot("front", 0.0, 0.05, 0.5, 1.0, 0.0),
    shot("three-quarter", 0.8, 0.1, 0.5, 1.0, 0.0),
    shot("back", PI, 0.05, 0.5, 1.0, 0.0),
    shot("side", FRAC_PI_2, 0.05, 0.5, 1.0, 0.0),
    shot("upper", 0.35, 0.05, 0.74, 0.45, 0.0),
    shot("hands", 0.5, 0.05, 0.45, 0.4, 0.0),
    shot("feet", 0.4, 0.05, 0.08, 0.4, 0.0),
    shot("front-later", 0.0, 0.05, 0.5, 1.0, 0.9),
];

const fn shot(name: &'static str, yaw: f32, pitch: f32, focus: f32, zoom: f32, later: f32) -> Shot {
    Shot {
        name,
        yaw,
        pitch,
        focus,
        zoom,
        later_seconds: later,
    }
}

#[derive(Serialize)]
struct PartRecord {
    scene: String,
    mask: u32,
    state: PartState,
}

#[derive(Serialize)]
struct NpcRecord {
    index: usize,
    form_id: String,
    editor_id: String,
    name: String,
    race: String,
    note: String,
    skipped: Option<String>,
    outfit: Option<String>,
    head: Option<PartRecord>,
    parts: Vec<PartRecord>,
    hidden_body_meshes: Vec<String>,
    idle_playing: bool,
    timed_out: bool,
    problems: Vec<String>,
    seconds_to_ready: f32,
    images: Vec<String>,
}

struct Capture {
    dir: PathBuf,
    target: Handle<Image>,
    shot: usize,
    settle: u32,
    phase: Phase,
    images: Vec<String>,
    /// Image files that could not be saved, for the NPC being captured.
    failures: Vec<String>,
    /// Set when a PNG or `capture.json` could not be written: the run then exits with an error.
    failed: bool,
    records: Vec<NpcRecord>,
    frames_until_next: u32,
}

#[derive(Resource)]
struct Viewer {
    connection: Mutex<Connection>,
    list: Vec<Npc>,
    index: usize,
    shown: Option<Shown>,
    yaw: f32,
    pitch: f32,
    distance: f32,
    fit_distance: f32,
    center: Vec3,
    bounds: Option<(Vec3, Vec3)>,
    paused: bool,
    label_visible: bool,
    user_shots: u32,
    capture: Option<Capture>,
}

#[derive(Component)]
struct ViewCamera;
#[derive(Component)]
struct ViewLight;
#[derive(Component)]
struct Label;

fn settings() -> WgpuSettings {
    let mut value = WgpuSettings {
        backends: Some(Backends::METAL),
        priority: WgpuSettingsPriority::WebGPU,
        ..default()
    };
    value.features |= WgpuFeatures::TEXTURE_COMPRESSION_BC;
    value
}

/// The text before the first NUL of a record's data is its `EDID`.
fn editor_id_of(connection: &Connection, form_id: u32) -> Option<String> {
    let data: Vec<u8> = connection
        .query_row(
            "SELECT data FROM records WHERE form_id = ?1",
            [form_id],
            |row| row.get(0),
        )
        .ok()?;
    let end = data.iter().position(|byte| *byte == 0)?;
    let text = std::str::from_utf8(&data[..end]).ok()?;
    (text.len() >= 3 && text.chars().all(|c| c.is_ascii_graphic())).then(|| text.to_owned())
}

fn lookup(connection: &Connection, form_id: u32, note: &str) -> Option<Npc> {
    let (editor_id, name, race_id): (Option<String>, Option<String>, u32) = connection
        .query_row(
            "SELECT editor_id, full_name, race_id FROM npcs WHERE id = ?1",
            [form_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()?;
    let editor_id = editor_id.unwrap_or_default();
    Some(Npc {
        form_id,
        name: name
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| editor_id.clone()),
        editor_id,
        race_id,
        race: editor_id_of(connection, race_id).unwrap_or_else(|| format!("{race_id:08X}")),
        note: note.to_owned(),
    })
}

fn find_npc(connection: &Connection, query: &str) -> Result<Npc, String> {
    let hex = query.trim_start_matches("0x");
    if let Ok(form_id) = u32::from_str_radix(hex, 16)
        && let Some(npc) = lookup(connection, form_id, "from --npc")
    {
        return Ok(npc);
    }
    let found: Option<u32> = connection
        .query_row(
            "SELECT id FROM npcs WHERE editor_id = ?1 COLLATE NOCASE ORDER BY id LIMIT 1",
            [query],
            |row| row.get(0),
        )
        .ok();
    found
        .and_then(|form_id| lookup(connection, form_id, "from --npc"))
        .ok_or_else(|| format!("no NPC_ with FormID or EditorID `{query}`"))
}

struct Options {
    assets: PathBuf,
    npcs: Vec<String>,
    only: bool,
    list: bool,
    capture: Option<PathBuf>,
    size: (u32, u32),
}

fn parse_args() -> Options {
    let mut options = Options {
        assets: PathBuf::new(),
        npcs: Vec::new(),
        only: false,
        list: false,
        capture: None,
        size: (1280, 800),
    };
    let mut args = std::env::args().skip(1);
    let usage = "npc_viewer --assets <absolute converted pack> [--npc <FormID|EditorID>]... [--only] [--list] [--capture <new dir>]";
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .unwrap_or_else(|| panic!("{name} needs a value"))
        };
        match arg.as_str() {
            "--assets" => options.assets = PathBuf::from(value("--assets")),
            "--npc" => options.npcs.push(value("--npc")),
            "--only" => options.only = true,
            "--list" => options.list = true,
            "--capture" => options.capture = Some(PathBuf::from(value("--capture"))),
            "--size" => {
                let text = value("--size");
                let (w, h) = text.split_once('x').expect("--size WxH");
                options.size = (w.parse().unwrap(), h.parse().unwrap());
            }
            other => panic!("unknown option {other}\n{usage}"),
        }
    }
    assert!(
        options.assets.is_absolute() && options.assets.is_dir(),
        "{usage}"
    );
    options
}

fn main() {
    let options = parse_args();
    let connection = Connection::open_with_flags(
        format!(
            "file:{}?mode=ro",
            options.assets.join("skyrim_world.db").display()
        ),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open skyrim_world.db read-only");
    let mut list: Vec<Npc> = if options.only {
        Vec::new()
    } else {
        CURATED
            .iter()
            .filter_map(|(id, note)| {
                let npc = lookup(&connection, *id, note);
                if npc.is_none() {
                    eprintln!("npc_viewer: curated NPC {id:08X} is not in this pack");
                }
                npc
            })
            .collect()
    };
    let mut start = 0;
    for (n, query) in options.npcs.iter().enumerate() {
        let npc = find_npc(&connection, query).unwrap_or_else(|error| panic!("{error}"));
        let position = list
            .iter()
            .position(|item| item.form_id == npc.form_id)
            .unwrap_or_else(|| {
                list.push(npc);
                list.len() - 1
            });
        if n == 0 {
            start = position;
        }
    }
    assert!(!list.is_empty(), "nothing to show");
    if options.list {
        for (index, npc) in list.iter().enumerate() {
            println!(
                "{:>2} {:08X} {:<28} {:<26} {:<22} {}",
                index + 1,
                npc.form_id,
                npc.editor_id,
                npc.name,
                npc.race,
                npc.note
            );
        }
        return;
    }
    let _pack_lock = shared::asset_lock::AssetLock::acquire_shared(&options.assets)
        .expect("the pack is busy (exclusive lock held)");
    let catalog = ActorCatalog::open(&options.assets.join("skyrim_world.db"));
    // This is a clothing viewer: a pack with no readable outfit catalog would only show naked
    // actors and blame the actors. Stop with the real prerequisite instead.
    if catalog.outfit_count() == 0 {
        eprintln!(
            "npc_viewer: {} has no usable actor-outfits.json (missing, unreadable or rejected; the engine log above says which). Run `converter derive-outfits` on an APFS clone of the pack and pass the clone.",
            options.assets.display()
        );
        std::process::exit(2);
    }
    let (width, height) = options.size;
    let windowed = options.capture.is_none();
    let mut app = App::new();
    let plugins = DefaultPlugins
        .set(AssetPlugin {
            file_path: options.assets.to_string_lossy().into_owned(),
            ..default()
        })
        .set(WindowPlugin {
            primary_window: windowed.then(|| Window {
                title: "Mudcrab NPC viewer".into(),
                resolution: (width, height).into(),
                ..default()
            }),
            exit_condition: if windowed {
                ExitCondition::OnAllClosed
            } else {
                ExitCondition::DontExit
            },
            ..default()
        })
        .set(RenderPlugin {
            render_creation: RenderCreation::Automatic(Box::new(settings())),
            ..default()
        })
        .disable::<bevy::audio::AudioPlugin>();
    app.add_plugins(if windowed {
        plugins
    } else {
        plugins.disable::<bevy::winit::WinitPlugin>()
    });
    if !windowed {
        app.add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
            Duration::from_millis(8),
        ));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / 30.0,
        )));
    }
    app.add_plugins((
        EffectPaletteMaterialPlugin,
        NifMaterialPlugin,
        ActorSkinTonePlugin,
        ActorBodyWeightPlugin,
        ActorAnimationPlugin,
        ActorDrivePlugin,
        ActorHeadPlugin,
        ActorOutfitPlugin,
    ))
    .insert_resource(catalog)
    .insert_resource(ClearColor(Color::srgb(0.045, 0.05, 0.06)))
    .insert_resource(GlobalAmbientLight {
        brightness: 90.0,
        ..default()
    });
    let capture = options.capture.map(|dir| {
        assert!(
            dir.is_absolute() && !dir.exists(),
            "--capture needs a new absolute directory"
        );
        std::fs::create_dir_all(&dir).unwrap();
        let target =
            app.world_mut()
                .resource_mut::<Assets<Image>>()
                .add(Image::new_target_texture(
                    width,
                    height,
                    TextureFormat::Rgba8UnormSrgb,
                    None,
                ));
        Capture {
            dir,
            target,
            shot: 0,
            settle: 0,
            phase: Phase::Pose,
            images: Vec::new(),
            failures: Vec::new(),
            failed: false,
            records: Vec::new(),
            frames_until_next: 0,
        }
    });
    app.insert_resource(Viewer {
        connection: Mutex::new(connection),
        list,
        // A capture covers the whole list; `--npc` only picks where an interactive run starts.
        index: if capture.is_some() { 0 } else { start },
        shown: None,
        yaw: 0.5,
        pitch: 0.1,
        distance: 300.0,
        fit_distance: 300.0,
        center: Vec3::new(0.0, 70.0, 0.0),
        bounds: None,
        paused: false,
        label_visible: true,
        user_shots: 0,
        capture,
    })
    .add_systems(Startup, setup)
    .add_systems(Update, update);
    let result = app.run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

fn setup(mut commands: Commands, viewer: Res<Viewer>) {
    let mut camera = commands.spawn((
        Camera3d::default(),
        ViewCamera,
        Exposure { ev100: 9.7 },
        Tonemapping::TonyMcMapface,
        DebandDither::Disabled,
        Transform::from_xyz(0.0, 80.0, -300.0).looking_at(Vec3::new(0.0, 70.0, 0.0), Vec3::Y),
    ));
    if let Some(capture) = &viewer.capture {
        camera.insert(RenderTarget::Image(capture.target.clone().into()));
    }
    let camera = camera.id();
    commands.spawn((
        DirectionalLight {
            illuminance: 6000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        ViewLight,
        Transform::default(),
    ));
    commands.spawn((
        Text::new("Loading…"),
        Label,
        UiTargetCamera(camera),
        TextFont {
            font_size: FontSize::Px(16.0),
            ..default()
        },
        TextColor(Color::WHITE),
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        Node {
            position_type: PositionType::Absolute,
            left: px(10),
            top: px(10),
            padding: UiRect::all(px(8)),
            ..default()
        },
    ));
}

fn track(server: &AssetServer, scene: &str, label: &str, mask: u32, identity: Identity) -> Tracked {
    Tracked {
        label: label.to_owned(),
        scene: scene.to_owned(),
        handle: server.load(GltfAssetLabel::Scene(0).from_asset(scene.to_owned())),
        mask,
        identity,
        state: PartState::Loading,
        loaded_frames: 0,
    }
}

/// Spawns the NPC the way `streaming::spawn_cell` does for a placed reference.
fn spawn(world: &mut World, viewer: &mut Viewer) -> Shown {
    let npc = viewer.list[viewer.index].clone();
    let mut shown = Shown {
        npc: npc.clone(),
        root: Entity::PLACEHOLDER,
        plan: None,
        skip: None,
        outfit_id: world
            .resource::<ActorCatalog>()
            .outfit_for(npc.form_id)
            .map(|row| row.outfit_id),
        outfit_name: None,
        head: None,
        parts: Vec::new(),
        started: Instant::now(),
        frames: 0,
        idle_expected: false,
        idle_playing: false,
        ready_frames: 0,
        ready_since: None,
        revealed: false,
        timed_out: false,
        hidden_meshes: Vec::new(),
    };
    shown.outfit_name = shown
        .outfit_id
        .and_then(|id| editor_id_of(&viewer.connection.lock().unwrap(), id));
    let catalog = world.resource::<ActorCatalog>();
    let plan = match catalog.get(npc.form_id) {
        None => Err("no actor_models row".to_owned()),
        Some(model) => catalog.spawn_plan(model).map_err(|error| error.to_string()),
    };
    let plan = match plan {
        Ok(plan) => plan,
        Err(reason) => {
            shown.skip = Some(reason);
            return shown;
        }
    };
    let server = world.resource::<AssetServer>().clone();
    let scene = server.load(GltfAssetLabel::Scene(0).from_asset(plan.scene_path.clone()));
    let root = world
        .spawn((
            Name::new(format!("Viewer NPC {:08X}", npc.form_id)),
            WorldAssetRoot(scene),
            Transform::default(),
            Visibility::Hidden,
        ))
        .id();
    // The streamed world puts the same components on a placed actor through this method, so the
    // viewer cannot drift from what the game draws. The NPC's own FormID stands in for the
    // placed reference's, and a viewed NPC is never a corpse.
    let setup = world.resource_scope(|world: &mut World, catalog: Mut<ActorCatalog>| {
        let mut commands = world.commands();
        let mut entity = commands.entity(root);
        catalog.insert_actor_components(&mut entity, &plan, &server, npc.form_id, false)
    });
    world.flush();
    shown.idle_expected = matches!(
        setup,
        ActorAnimationSetup::Gait
            | ActorAnimationSetup::AuthoredIdle
            | ActorAnimationSetup::CreatureIdle
    );
    shown.root = root;
    if let Some(head) = &plan.head
        && head.source == engine::actors::ActorHeadSource::Facegen
    {
        // The engine's own path rule, with its folder guards: a head it would not load is not
        // tracked as one.
        if let Some(converted) = converted_model_path(head.model_path.clone()) {
            shown.head = Some(track(&server, &converted, "head", 0, Identity::Head));
        }
    }
    if let Some(outfit) = &plan.outfit {
        for part in &outfit.0 {
            shown.parts.push(track(
                &server,
                &part.scene,
                &short(&part.scene),
                part.biped_mask,
                Identity::Outfit(part.clone()),
            ));
        }
    }
    shown.plan = Some(plan);
    shown
}

fn short(scene: &str) -> String {
    scene.strip_prefix("meshes/").unwrap_or(scene).to_owned()
}

/// The instantiated scene the engine made for a tracked part of the actor `root`, if it still
/// exists. It is matched by the engine's own components, not by entity name.
fn instance_of(world: &mut World, root: Entity, identity: &Identity) -> Option<Entity> {
    match identity {
        Identity::Head => {
            let mut query = world.query::<(Entity, &ActorHeadInstance)>();
            query
                .iter(world)
                .find(|(_, instance)| instance.actor == root)
                .map(|(entity, _)| entity)
        }
        Identity::Outfit(part) => {
            let mut query = world.query::<(Entity, &ActorOutfitInstance)>();
            query
                .iter(world)
                .find(|(_, instance)| instance.actor == root && instance.part == *part)
                .map(|(entity, _)| entity)
        }
    }
}

fn is_descendant(world: &World, mut entity: Entity, root: Entity) -> bool {
    for _ in 0..64 {
        if entity == root {
            return true;
        }
        match world.get::<ChildOf>(entity) {
            Some(parent) => entity = parent.parent(),
            None => return false,
        }
    }
    false
}

fn has_mesh_below(world: &mut World, entity: Entity) -> bool {
    let mut stack = vec![entity];
    while let Some(next) = stack.pop() {
        if world.get::<Mesh3d>(next).is_some() {
            return true;
        }
        if let Some(children) = world.get::<Children>(next) {
            stack.extend(children.iter());
        }
    }
    false
}

fn refresh(world: &mut World, shown: &mut Shown) {
    shown.frames += 1;
    let server = world.resource::<AssetServer>().clone();
    let root = shown.root;
    let mut trackers: Vec<&mut Tracked> = shown
        .parts
        .iter_mut()
        .chain(shown.head.iter_mut())
        .collect();
    for tracked in &mut trackers {
        if tracked.state == PartState::LoadFailed {
            continue;
        }
        match server.load_state(tracked.handle.id()) {
            LoadState::Failed(_) => tracked.state = PartState::LoadFailed,
            LoadState::Loaded => {
                tracked.loaded_frames += 1;
                let instance = instance_of(world, root, &tracked.identity);
                let attached = instance.is_some_and(|entity| has_mesh_below(world, entity));
                tracked.state = if attached {
                    PartState::Attached
                } else if tracked.loaded_frames > DROPPED_AFTER_FRAMES {
                    PartState::Dropped
                } else {
                    PartState::Loading
                };
            }
            _ => {}
        }
    }
    shown.idle_playing = world
        .get::<AnimationPlayer>(root)
        .is_some_and(|player| player.playing_animations().next().is_some());
    let mut hidden = Vec::new();
    let mut query = world.query_filtered::<(Entity, &Name, &Visibility), With<SkinnedMesh>>();
    for (entity, name, visibility) in query.iter(world) {
        if *visibility == Visibility::Hidden && is_descendant(world, entity, root) {
            hidden.push(name.as_str().to_owned());
        }
    }
    hidden.sort();
    shown.hidden_meshes = hidden;
}

fn settled(shown: &Shown) -> bool {
    if shown.skip.is_some() {
        return true;
    }
    let all_final = shown
        .parts
        .iter()
        .chain(shown.head.iter())
        .all(|t| t.state != PartState::Loading);
    all_final && (shown.idle_playing || !shown.idle_expected)
}

fn bounds(world: &mut World, root: Entity) -> Option<(Vec3, Vec3)> {
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    let mut query = world.query::<(Entity, &Aabb, &GlobalTransform, &Visibility)>();
    let items: Vec<_> = query
        .iter(world)
        .map(|(e, a, g, v)| (e, *a, *g, *v))
        .collect();
    for (entity, aabb, global, visibility) in items {
        if visibility == Visibility::Hidden || !is_descendant(world, entity, root) {
            continue;
        }
        let center: Vec3 = aabb.center.into();
        let half: Vec3 = aabb.half_extents.into();
        for x in [-1.0, 1.0] {
            for y in [-1.0, 1.0] {
                for z in [-1.0, 1.0] {
                    let point = global.transform_point(center + half * Vec3::new(x, y, z));
                    low = low.min(point);
                    high = high.max(point);
                }
            }
        }
    }
    (low.is_finite() && high.is_finite()).then_some((low, high))
}

fn label_text(viewer: &Viewer) -> String {
    let total = viewer.list.len();
    let npc = &viewer.list[viewer.index];
    let hint = if viewer.capture.is_none() {
        "     >> press N = next NPC, P = previous <<"
    } else {
        ""
    };
    let mut out = format!(
        "[{}/{}] {}   NPC_ {:08X} ({}){}\nRace: {} ({:08X})",
        viewer.index + 1,
        total,
        npc.name,
        npc.form_id,
        npc.editor_id,
        hint,
        npc.race,
        npc.race_id
    );
    let Some(shown) = &viewer.shown else {
        return out;
    };
    if let Some(plan) = &shown.plan {
        out += &format!(
            " · {} · weight {} · {}\nBody: {}",
            plan.actor.sex.as_str(),
            plan.weight.map_or("?".into(), |w| format!("{w:.0}")),
            if plan.skin_tone.is_some() {
                "skin tone"
            } else {
                "no skin tone"
            },
            short(&plan.scene_path)
        );
    }
    out += &format!("\nWhy: {}", npc.note);
    if let Some(reason) = &shown.skip {
        out += &format!("\nNOT SPAWNED: {reason}");
        return out;
    }
    if let Some(head) = &shown.head {
        out += &format!("\nHead: {:?}  {}", head.state, short(&head.scene));
    } else {
        out += "\nHead: none (no FaceGen head in the catalog)";
    }
    match (shown.outfit_id, shown.parts.is_empty()) {
        (Some(id), false) => {
            let attached = shown
                .parts
                .iter()
                .filter(|p| p.state == PartState::Attached)
                .count();
            out += &format!(
                "\nOutfit: {} ({:08X})  attached {}/{}",
                shown.outfit_name.as_deref().unwrap_or("unnamed"),
                id,
                attached,
                shown.parts.len()
            );
            for part in &shown.parts {
                let tag = match part.state {
                    PartState::Attached => "ok    ",
                    PartState::Loading => "load  ",
                    PartState::LoadFailed => "FAILED (no asset) ",
                    PartState::Dropped => "FAILED (skinless/joints) ",
                };
                out += &format!("\n  {tag}{}  slots {:#x}", part.label, part.mask);
            }
        }
        (Some(id), true) => {
            out += &format!(
                "\nOutfit: {} ({:08X})  NOT WORN: this actor does not use the shared composed body, so the engine skips its outfit",
                shown.outfit_name.as_deref().unwrap_or("unnamed"),
                id
            );
        }
        (None, _) => {
            out += "\nOutfit: none in actor-outfits.json (leveled list, template or no DOFT)"
        }
    }
    out += &format!(
        "\nBody meshes hidden by outfit: {}",
        if shown.hidden_meshes.is_empty() {
            "none".to_owned()
        } else {
            shown.hidden_meshes.join(", ")
        }
    );
    let status = if !shown.revealed {
        if shown.timed_out {
            "TIMED OUT"
        } else {
            "loading"
        }
    } else if shown.idle_playing {
        if viewer.paused {
            "idle paused"
        } else {
            "idle playing"
        }
    } else {
        "NO IDLE (bind pose): no gait clip for this body"
    };
    out += &format!("\nState: {status}");
    if viewer.capture.is_none() {
        out += "\nN/P next/prev · drag/arrows orbit · wheel/Up/Down zoom · R reset · Space pause · H label · F12 PNG · Esc quit";
    }
    out
}

fn move_camera(world: &mut World, viewer: &Viewer) {
    let direction = Vec3::new(
        viewer.yaw.sin() * viewer.pitch.cos(),
        viewer.pitch.sin(),
        -viewer.yaw.cos() * viewer.pitch.cos(),
    );
    let position = viewer.center + direction * viewer.distance;
    let transform = Transform::from_translation(position).looking_at(viewer.center, Vec3::Y);
    let mut cameras = world.query_filtered::<&mut Transform, With<ViewCamera>>();
    for mut camera in cameras.iter_mut(world) {
        *camera = transform;
    }
    let mut lights =
        world.query_filtered::<&mut Transform, (With<ViewLight>, Without<ViewCamera>)>();
    for mut light in lights.iter_mut(world) {
        *light = Transform::from_translation(position + Vec3::new(40.0, 120.0, 0.0))
            .looking_at(viewer.center, Vec3::Y);
    }
}

fn fit(viewer: &mut Viewer, bounds: (Vec3, Vec3)) {
    let (low, high) = bounds;
    viewer.bounds = Some(bounds);
    viewer.center = (low + high) * 0.5;
    let radius = ((high - low).length() * 0.5).max(1.0);
    // The default perspective field of view is 45 degrees.
    viewer.fit_distance = radius / (22.5_f32.to_radians()).sin() * 0.82;
    viewer.distance = viewer.fit_distance;
}

fn unload(world: &mut World, viewer: &mut Viewer) {
    if let Some(shown) = viewer.shown.take()
        && shown.root != Entity::PLACEHOLDER
    {
        world.despawn(shown.root);
    }
    viewer.bounds = None;
}

fn select(world: &mut World, viewer: &mut Viewer, index: usize) {
    unload(world, viewer);
    viewer.index = index % viewer.list.len();
    viewer.yaw = 0.5;
    viewer.pitch = 0.1;
    // The pause belongs to the NPC that was on screen; a new NPC starts playing.
    viewer.paused = false;
    let shown = spawn(world, viewer);
    println!(
        "NPC_START {:08X} {} ({})",
        shown.npc.form_id, shown.npc.name, shown.npc.editor_id
    );
    viewer.shown = Some(shown);
}

fn record(viewer: &Viewer, shown: &Shown, images: Vec<String>, failures: &[String]) -> NpcRecord {
    let part = |t: &Tracked| PartRecord {
        scene: t.scene.clone(),
        mask: t.mask,
        state: t.state,
    };
    let mut problems = Vec::new();
    if let Some(reason) = &shown.skip {
        problems.push(format!("not spawned: {reason}"));
    }
    if shown.outfit_id.is_some() && shown.parts.is_empty() && shown.skip.is_none() {
        problems.push(
            "outfit in sidecar but not worn (actor is not on the shared composed body)".into(),
        );
    }
    for part in shown.parts.iter().chain(shown.head.iter()) {
        if part.state != PartState::Attached {
            problems.push(format!("{} {:?}", part.label, part.state));
        }
    }
    if shown.skip.is_none() && !shown.idle_playing {
        problems.push("no idle playing (bind pose)".into());
    }
    if shown.timed_out {
        problems.push("timed out waiting for assets".into());
    }
    if shown.skip.is_none() && viewer.bounds.is_none() {
        problems.push("no geometry: the body scene did not load".into());
    }
    problems.extend(failures.iter().cloned());
    NpcRecord {
        index: viewer.index + 1,
        form_id: format!("{:08X}", shown.npc.form_id),
        editor_id: shown.npc.editor_id.clone(),
        name: shown.npc.name.clone(),
        race: shown.npc.race.clone(),
        note: shown.npc.note.clone(),
        skipped: shown.skip.clone(),
        outfit: shown.outfit_id.map(|id| {
            format!(
                "{} ({id:08X})",
                shown.outfit_name.clone().unwrap_or_default()
            )
        }),
        head: shown.head.as_ref().map(part),
        parts: shown.parts.iter().map(part).collect(),
        hidden_body_meshes: shown.hidden_meshes.clone(),
        idle_playing: shown.idle_playing,
        timed_out: shown.timed_out,
        problems,
        seconds_to_ready: shown
            .ready_since
            .map_or(0.0, |t| t.duration_since(shown.started).as_secs_f32()),
        images,
    }
}

fn update(world: &mut World) {
    world.resource_scope(|world, mut viewer: Mut<Viewer>| {
        let viewer = &mut *viewer;
        if viewer.shown.is_none() {
            let index = viewer.index;
            select(world, viewer, index);
            return;
        }
        let (keys, motion, scroll, buttons) = {
            let keys = world.resource::<ButtonInput<KeyCode>>();
            let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            let next = keys.just_pressed(KeyCode::KeyN)
                || keys.just_pressed(KeyCode::PageDown)
                || (keys.just_pressed(KeyCode::Tab) && !shift);
            let prev = keys.just_pressed(KeyCode::KeyP)
                || keys.just_pressed(KeyCode::PageUp)
                || (keys.just_pressed(KeyCode::Tab) && shift);
            let held = |code| if keys.pressed(code) { 1.0 } else { 0.0 };
            (
                (
                    next,
                    prev,
                    keys.just_pressed(KeyCode::Escape),
                    keys.just_pressed(KeyCode::KeyR),
                    keys.just_pressed(KeyCode::Space),
                    keys.just_pressed(KeyCode::KeyH),
                    keys.just_pressed(KeyCode::F12),
                ),
                world.resource::<AccumulatedMouseMotion>().delta,
                world.resource::<AccumulatedMouseScroll>().delta.y,
                (
                    held(KeyCode::ArrowRight) - held(KeyCode::ArrowLeft),
                    held(KeyCode::ArrowDown) - held(KeyCode::ArrowUp),
                    world
                        .resource::<ButtonInput<MouseButton>>()
                        .pressed(MouseButton::Left),
                ),
            )
        };
        let dt = world.resource::<Time<Real>>().delta_secs();
        let (next, prev, quit, reset, pause, hide, shoot) = keys;
        if quit {
            world.write_message(AppExit::Success);
            return;
        }
        if viewer.capture.is_none() {
            if next || prev {
                let count = viewer.list.len();
                let index = if next {
                    viewer.index + 1
                } else {
                    viewer.index + count - 1
                };
                select(world, viewer, index);
                return;
            }
            viewer.yaw += buttons.0 * 1.6 * dt;
            viewer.distance = (viewer.distance * (1.0 + buttons.1 * 1.2 * dt)).max(5.0);
            viewer.distance = (viewer.distance * (1.0 - scroll * 0.08)).max(5.0);
            if buttons.2 {
                viewer.yaw -= motion.x * 0.008;
                viewer.pitch = (viewer.pitch + motion.y * 0.008).clamp(-1.2, 1.4);
            }
            if reset {
                viewer.yaw = 0.5;
                viewer.pitch = 0.1;
                viewer.distance = viewer.fit_distance;
            }
            if hide {
                viewer.label_visible = !viewer.label_visible;
            }
            if pause {
                viewer.paused = !viewer.paused;
                let root = viewer.shown.as_ref().unwrap().root;
                if let Some(mut player) = world.get_mut::<AnimationPlayer>(root) {
                    if viewer.paused {
                        player.pause_all();
                    } else {
                        player.resume_all();
                    }
                }
            }
            if shoot {
                viewer.user_shots += 1;
                let path = format!("npc-viewer-{:03}.png", viewer.user_shots);
                let name = path.clone();
                world
                    .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
                    .observe(bevy::render::view::screenshot::save_to_disk(path));
                println!("saved {name}");
            }
        }
        let mut shown = viewer.shown.take().unwrap();
        refresh(world, &mut shown);
        // Space may be pressed before the player exists: apply the pause once it does.
        if viewer.paused
            && shown.root != Entity::PLACEHOLDER
            && let Some(mut player) = world.get_mut::<AnimationPlayer>(shown.root)
        {
            player.pause_all();
        }
        if shown.timed_out || shown.started.elapsed() > READY_TIMEOUT {
            shown.timed_out = true;
        }
        if shown.ready_since.is_none() && (settled(&shown) || shown.timed_out) {
            shown.ready_frames += 1;
            if shown.ready_frames >= IDLE_FRAMES {
                shown.ready_since = Some(Instant::now());
            }
        }
        if shown.ready_since.is_some() && !shown.revealed {
            // Settled: show the actor and frame it once.
            if shown.root != Entity::PLACEHOLDER
                && let Some(mut visibility) = world.get_mut::<Visibility>(shown.root)
            {
                *visibility = Visibility::Inherited;
            }
            shown.revealed = true;
        }
        if shown.revealed && viewer.bounds.is_none() {
            let found = if shown.root != Entity::PLACEHOLDER {
                bounds(world, shown.root)
            } else {
                None
            };
            if let Some(found) = found {
                fit(viewer, found);
            }
        }
        viewer.shown = Some(shown);
        let text = label_text(viewer);
        let visible = viewer.label_visible;
        let mut labels = world.query_filtered::<(&mut Text, &mut Visibility), With<Label>>();
        for (mut label, mut visibility) in labels.iter_mut(world) {
            if label.0 != text {
                label.0.clone_from(&text);
            }
            *visibility = if visible {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
        drive_capture(world, viewer);
        move_camera(world, viewer);
    });
}

/// Where one capture shot stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Choose the next shot and move the camera.
    Pose,
    /// Let the camera and the idle advance, then ask for the image.
    Posed,
    /// The image is requested; wait for the file.
    Requested,
}

/// Hidden-window capture: wait until the NPC is settled, take each angle, then move on.
fn drive_capture(world: &mut World, viewer: &mut Viewer) {
    let Some(mut capture) = viewer.capture.take() else {
        return;
    };
    step_capture(world, viewer, &mut capture);
    viewer.capture = Some(capture);
}

fn step_capture(world: &mut World, viewer: &mut Viewer, capture: &mut Capture) {
    let Some(shown) = viewer.shown.as_ref() else {
        return;
    };
    let skipped = shown.skip.is_some();
    let ready = shown.ready_since.map(|t| t.elapsed());
    capture.settle += 1;
    if capture.frames_until_next > 0 {
        capture.frames_until_next -= 1;
        return;
    }
    if capture.phase == Phase::Requested {
        return;
    }
    // Textures keep arriving after the scene is ready; give them wall-clock time and frames.
    // A body that never loads has no bounds. After the timeout the NPC ends with a recorded failure
    // instead of holding the whole batch.
    let no_geometry = shown.timed_out && viewer.bounds.is_none();
    let ready_enough = skipped
        || no_geometry
        || (ready.is_some_and(|r| r >= TEXTURE_SETTLE)
            && capture.settle >= TEXTURE_SETTLE_FRAMES
            && viewer.bounds.is_some());
    if !ready_enough {
        return;
    }
    if skipped || no_geometry || capture.shot >= SHOTS.len() {
        finish_npc(world, viewer, capture);
        return;
    }
    let shot = SHOTS[capture.shot];
    if capture.phase == Phase::Pose {
        let (low, high) = viewer.bounds.unwrap();
        let height = (high.y - low.y).max(1.0);
        let base = (low + high) * 0.5;
        viewer.yaw = shot.yaw;
        viewer.pitch = shot.pitch;
        viewer.center = Vec3::new(base.x, low.y + height * shot.focus, base.z);
        viewer.distance = viewer.fit_distance * shot.zoom;
        // The "later" shot lets the idle advance first, so the two front shots can be compared.
        capture.frames_until_next = 3 + (shot.later_seconds * 30.0) as u32;
        capture.phase = Phase::Posed;
        return;
    }
    let file = format!(
        "{:02}-{:08X}-{}.png",
        viewer.index + 1,
        shown.npc.form_id,
        shot.name
    );
    let path = capture.dir.join(&file);
    capture.phase = Phase::Requested;
    let target = capture.target.clone();
    world.spawn(Screenshot::image(target)).observe(
        move |event: On<ScreenshotCaptured>, mut viewer: ResMut<Viewer>| {
            let saved = event
                .image
                .clone()
                .try_into_dynamic()
                .map_err(|error| format!("cannot convert the capture: {error}"))
                .and_then(|image| {
                    image
                        .to_rgb8()
                        .save(&path)
                        .map_err(|error| format!("cannot save {}: {error}", path.display()))
                });
            if let Some(capture) = viewer.capture.as_mut() {
                match saved {
                    Ok(()) => capture.images.push(file.clone()),
                    Err(error) => {
                        eprintln!("npc_viewer: {file}: {error}");
                        capture.failures.push(format!("{file}: {error}"));
                        capture.failed = true;
                    }
                }
                capture.phase = Phase::Pose;
                capture.shot += 1;
            }
        },
    );
}

fn finish_npc(world: &mut World, viewer: &mut Viewer, capture: &mut Capture) {
    let shown = viewer.shown.take().unwrap();
    let failures = std::mem::take(&mut capture.failures);
    let rec = record(
        viewer,
        &shown,
        std::mem::take(&mut capture.images),
        &failures,
    );
    println!(
        "CAPTURED {} {} parts={} states={:?} hidden={:?}",
        rec.form_id,
        rec.name,
        rec.parts.len(),
        rec.parts.iter().map(|p| p.state).collect::<Vec<_>>(),
        rec.hidden_body_meshes
    );
    capture.records.push(rec);
    viewer.shown = Some(shown);
    capture.shot = 0;
    capture.settle = 0;
    capture.phase = Phase::Pose;
    let next = viewer.index + 1;
    let report = capture.dir.join("capture.json");
    if let Err(error) = std::fs::write(
        &report,
        serde_json::to_vec_pretty(&capture.records).unwrap(),
    ) {
        eprintln!("npc_viewer: cannot write {}: {error}", report.display());
        capture.failed = true;
    }
    if next >= viewer.list.len() {
        println!("CAPTURE_DONE {}", capture.records.len());
        world.write_message(if capture.failed {
            AppExit::error()
        } else {
            AppExit::Success
        });
        return;
    }
    select(world, viewer, next);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_facegen_head_path_converts_with_the_engines_rule() {
        for source in [
            "actors/character/facegendata/facegeom/skyrim.esm/00013669.nif",
            r"Actors\Character\FaceGenData\FaceGeom\Skyrim.esm\00013669.NIF",
            "Meshes/actors/character/facegendata/facegeom/skyrim.esm/00013669.nif",
        ] {
            assert_eq!(
                converted_model_path(source.to_owned()).as_deref(),
                Some("meshes/actors/character/facegendata/facegeom/skyrim.esm/00013669.glb"),
                "{source}"
            );
        }
        assert!(converted_model_path("../escape.nif".to_owned()).is_none());
    }

    #[test]
    fn finds_an_npc_by_hex_form_id_or_editor_id() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE npcs(id INTEGER, editor_id TEXT, full_name TEXT, race_id INTEGER, class_id INTEGER, flags INTEGER);
                 CREATE TABLE records(form_id INTEGER, data BLOB);
                 INSERT INTO npcs VALUES (79977, 'CarriageDriverWhiterun', 'Bjorlam', 79686, 0, 0);
                 INSERT INTO records VALUES (79686, X'4E6F7264526163650000');",
            )
            .unwrap();
        for query in ["0x13869", "13869", "carriagedriverwhiterun"] {
            let npc = find_npc(&connection, query).unwrap();
            assert_eq!(npc.form_id, 79977, "{query}");
            assert_eq!(npc.name, "Bjorlam");
            assert_eq!(npc.race, "NordRace");
        }
        assert!(find_npc(&connection, "NoSuchNpc").is_err());
    }
}
