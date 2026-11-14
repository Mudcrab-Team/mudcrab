//! Interactive selected-creature inspection, adapted from the #483 model viewer.
//! This shows catalog-selected idle playback beside a separate, unanimated body instance.
use bevy::{
    animation::{AnimatedBy, AnimationTargetId},
    camera::{Exposure, primitives::Aabb},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    gltf::{Gltf, GltfAssetLabel},
    prelude::*,
    render::{
        RenderPlugin,
        settings::{Backends, RenderCreation, WgpuFeatures, WgpuSettings, WgpuSettingsPriority},
        view::screenshot::{Screenshot, save_to_disk},
    },
    world_serialization::{WorldAsset, WorldAssetRoot},
};
use engine::{
    actor_animation::{ActorAnimationPlugin, ActorClips, ActorIdleLibrary},
    nif_material::NifMaterialPlugin,
    world::actor_catalog::ActorCatalog,
};
use std::{path::PathBuf, time::Instant};

#[derive(Clone, Copy)]
struct Selection {
    name: &'static str,
    key: &'static str,
    base: u32,
    race: u32,
    body: &'static str,
    source: &'static str,
}

const SELECTIONS: [Selection; 2] = [
    Selection {
        name: "Hare",
        key: "hare",
        base: 0x0006_DC9D,
        race: 0x0006_DC99,
        body: "meshes/actors/ambient/hare/character assets/rabbit.glb",
        source: "meshes/actors/ambient/hare/animations/mt_idle.hkx",
    },
    Selection {
        name: "Wolf",
        key: "wolf",
        base: 0x0002_3ABE,
        race: 0x0001_320A,
        body: "meshes/actors/canine/character assets wolf/wolfblack.glb",
        source: "meshes/actors/canine/animations/mt_idle_wolf.hkx",
    },
];

#[derive(Clone, Copy)]
struct Frame {
    center: Vec3,
    radius: f32,
    meshes: usize,
}

struct Model {
    scene: Handle<WorldAsset>,
    clip: Handle<Gltf>,
    live: Option<Entity>,
    reference: Option<Entity>,
    frame: Option<Frame>,
}

#[derive(Resource)]
struct Viewer {
    models: Vec<Model>,
    output: PathBuf,
    selected: usize,
    paused: bool,
    yaw: f32,
    pitch: f32,
    distance: f32,
    reset_camera: bool,
    captures: u32,
    started: Instant,
    ready_at: Option<Instant>,
    smoke: bool,
    smoke_captures: [bool; 4],
    smoke_stage_at: Option<Instant>,
    smoke_first_capture_at: Option<Instant>,
}

#[derive(Component)]
struct CreatureRoot {
    model: usize,
    live: bool,
}

#[derive(Component)]
struct LiveCreature;

#[derive(Component)]
struct InspectionCamera;

#[derive(Component)]
struct Instructions;

fn render_settings() -> WgpuSettings {
    let mut settings = WgpuSettings {
        backends: Some(Backends::METAL),
        priority: WgpuSettingsPriority::WebGPU,
        ..default()
    };
    settings.features |= WgpuFeatures::TEXTURE_COMPRESSION_BC;
    settings
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert!(
        args.len() == 2 || (args.len() == 3 && args[2] == "--smoke"),
        "creature_idle_viewer <absolute assets> <new output> [--smoke]"
    );
    let assets = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    assert!(assets.is_absolute() && assets.is_dir());
    assert!(
        output.is_absolute() && !output.exists(),
        "use a new output directory"
    );
    let storage =
        PathBuf::from(std::env::var_os("MUDCRAB_STORAGE").expect("managed wrapper required"));
    assert!(std::env::var_os("MUDCRAB_BUILD_LEASE").is_some());
    assert!(
        output
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .starts_with(storage.join("evidence").canonicalize().unwrap())
    );
    let _pack_lock = shared::asset_lock::AssetLock::acquire_shared(&assets)
        .expect("immutable fixture pack is busy or unavailable");
    let catalog = ActorCatalog::open(&assets.join("skyrim_world.db"));
    let mut plans = Vec::new();
    for selection in SELECTIONS {
        let model = catalog
            .get(selection.base)
            .expect("selected actor is absent");
        let plan = catalog
            .spawn_plan(model)
            .expect("selected actor has no body");
        assert!(!plan.uses_composed_body());
        assert_eq!(plan.scene_path, selection.body);
        assert_eq!(plan.actor.race_id, selection.race);
        assert_eq!(plan.actor.sex.as_str(), "male");
        let idle = catalog
            .idle_scene(&plan)
            .expect("selected default idle is absent");
        assert_eq!(
            catalog.authored_idle_scene(&plan.actor, &plan.scene_path, selection.source),
            Some(idle)
        );
        assert!(assets.join(&plan.scene_path).is_file() && assets.join(idle).is_file());
        plans.push((plan, idle.to_owned()));
    }
    std::fs::create_dir(&output).unwrap();
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: assets.to_string_lossy().into_owned(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Mudcrab — hare and wolf idle comparison".into(),
                    resolution: (1280, 800).into(),
                    ..default()
                }),
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(render_settings())),
                ..default()
            })
            .disable::<bevy::audio::AudioPlugin>(),
    )
    .add_plugins((NifMaterialPlugin, ActorAnimationPlugin))
    .insert_resource(catalog)
    .insert_resource(ClearColor(Color::srgb(0.025, 0.03, 0.04)))
    .insert_resource(GlobalAmbientLight {
        brightness: 40.0,
        ..default()
    });
    let server = app.world().resource::<AssetServer>();
    let models = plans
        .iter()
        .map(|(plan, idle)| Model {
            scene: server.load(GltfAssetLabel::Scene(0).from_asset(plan.scene_path.clone())),
            clip: server.load(idle.clone()),
            live: None,
            reference: None,
            frame: None,
        })
        .collect();
    app.insert_resource(Viewer {
        models,
        output,
        selected: 0,
        paused: false,
        yaw: 0.6,
        pitch: 0.15,
        distance: 100.0,
        reset_camera: true,
        captures: 0,
        started: Instant::now(),
        ready_at: None,
        smoke: args.len() == 3,
        smoke_captures: [false; 4],
        smoke_stage_at: None,
        smoke_first_capture_at: None,
    });
    for (index, (plan, idle)) in plans.into_iter().enumerate() {
        let scene = app.world().resource::<Viewer>().models[index].scene.clone();
        let clips = ActorClips::load_idle(app.world().resource::<AssetServer>(), &idle);
        let live = app
            .world_mut()
            .spawn((
                WorldAssetRoot(scene.clone()),
                plan.actor,
                ActorIdleLibrary {
                    body_scene: plan.scene_path,
                },
                clips,
                CreatureRoot {
                    model: index,
                    live: true,
                },
                LiveCreature,
                Transform::default(),
                Visibility::Hidden,
            ))
            .id();
        // This independently instantiated hierarchy never receives animation components.
        // Stopping an already animated body would retain its last pose, not its rest pose.
        let reference = app
            .world_mut()
            .spawn((
                WorldAssetRoot(scene),
                plan.actor,
                CreatureRoot {
                    model: index,
                    live: false,
                },
                Transform::default(),
                Visibility::Hidden,
            ))
            .id();
        let mut viewer = app.world_mut().resource_mut::<Viewer>();
        viewer.models[index].live = Some(live);
        viewer.models[index].reference = Some(reference);
    }
    let result = app
        .add_systems(Startup, setup)
        .add_systems(Update, update)
        .run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

fn setup(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        InspectionCamera,
        Exposure { ev100: 9.7 },
        Tonemapping::TonyMcMapface,
        DebandDither::Disabled,
        Transform::from_xyz(100.0, 30.0, 150.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 5000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(100.0, 200.0, 150.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Text::new("Loading hare and wolf…"),
        Instructions,
        TextFont {
            font_size: FontSize::Px(20.0),
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            left: px(16),
            top: px(16),
            ..default()
        },
    ));
}

fn descendant_of(mut entity: Entity, root: Entity, parents: &Query<&ChildOf>) -> bool {
    for _ in 0..256 {
        if entity == root {
            return true;
        }
        let Ok(parent) = parents.get(entity) else {
            return false;
        };
        entity = parent.parent();
    }
    false
}

fn model_frame(
    root: Entity,
    translation: Vec3,
    bounds: &Query<(Entity, &Aabb, &GlobalTransform), With<Mesh3d>>,
    parents: &Query<&ChildOf>,
) -> Option<Frame> {
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    let mut meshes = 0;
    for (entity, aabb, global) in bounds.iter() {
        if !descendant_of(entity, root, parents) {
            continue;
        }
        meshes += 1;
        let center: Vec3 = aabb.center.into();
        let half: Vec3 = aabb.half_extents.into();
        for x in [-1.0, 1.0] {
            for y in [-1.0, 1.0] {
                for z in [-1.0, 1.0] {
                    let point =
                        global.transform_point(center + half * Vec3::new(x, y, z)) - translation;
                    low = low.min(point);
                    high = high.max(point);
                }
            }
        }
    }
    (meshes > 0 && low.is_finite() && high.is_finite()).then_some(Frame {
        center: (low + high) * 0.5,
        radius: ((high - low).length() * 0.5).max(0.1),
        meshes,
    })
}

fn smoke_path(output: &std::path::Path, index: usize) -> PathBuf {
    output.join(format!(
        "{}-smoke-{}.png",
        SELECTIONS[index / 2].key,
        index % 2
    ))
}

// Bevy system parameter inference needs these nested query lifetimes independent.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update(
    mut commands: Commands,
    mut viewer: ResMut<Viewer>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    server: Res<AssetServer>,
    bounds: Query<(Entity, &Aabb, &GlobalTransform), With<Mesh3d>>,
    parents: Query<&ChildOf>,
    targets: Query<(&AnimatedBy, &AnimationTargetId)>,
    mut players: ParamSet<(
        Query<(Entity, &AnimationPlayer)>,
        Query<&mut AnimationPlayer, With<LiveCreature>>,
    )>,
    mut roots: Query<(&CreatureRoot, &mut Transform, &mut Visibility), Without<InspectionCamera>>,
    mut cameras: Query<&mut Transform, (With<InspectionCamera>, Without<CreatureRoot>)>,
    mut instructions: Query<&mut Text, With<Instructions>>,
    mut exit: MessageWriter<AppExit>,
) {
    if keys.just_pressed(KeyCode::Escape) || viewer.started.elapsed().as_secs() >= 1800 {
        println!("CREATURE_VIEWER_EXIT reason=user-or-30-minute-limit");
        exit.write(AppExit::Success);
        return;
    }
    for (index, model) in viewer.models.iter_mut().enumerate() {
        for id in [model.scene.id().untyped(), model.clip.id().untyped()] {
            if let bevy::asset::LoadState::Failed(reason) = server.load_state(id) {
                eprintln!(
                    "CREATURE_VIEWER_LOAD_FAILED model={} reason={reason}",
                    SELECTIONS[index].key
                );
                exit.write(AppExit::error());
                return;
            }
        }
        let live = model.live.unwrap();
        let reference = model.reference.unwrap();
        let read_players = players.p0();
        if read_players
            .iter()
            .any(|(entity, _)| descendant_of(entity, reference, &parents))
        {
            eprintln!(
                "CREATURE_VIEWER_STATIC_HAS_PLAYER model={}",
                SELECTIONS[index].key
            );
            exit.write(AppExit::error());
            return;
        }
        if model.frame.is_some()
            && !read_players
                .get(live)
                .is_ok_and(|(_, player)| player.playing_animations().next().is_some())
        {
            eprintln!(
                "CREATURE_VIEWER_LIVE_PLAYER_MISSING model={}",
                SELECTIONS[index].key
            );
            exit.write(AppExit::error());
            return;
        }
        if model.frame.is_some()
            || !server.is_loaded_with_dependencies(model.scene.id())
            || !server.is_loaded_with_dependencies(model.clip.id())
        {
            continue;
        }
        let Ok((_, player)) = read_players.get(live) else {
            continue;
        };
        if player.playing_animations().next().is_none() {
            continue;
        }
        let Ok((_, transform, _)) = roots.get(reference) else {
            continue;
        };
        let Some(frame) = model_frame(reference, transform.translation, &bounds, &parents) else {
            continue;
        };
        let target_count = targets.iter().filter(|(owner, _)| owner.0 == live).count();
        if target_count == 0 {
            continue;
        }
        println!(
            "CREATURE_VIEWER_READY model={} base={:08X} live_player=true active={} animation_targets={} static_players=0 meshes={} radius={}",
            SELECTIONS[index].key,
            SELECTIONS[index].base,
            player.playing_animations().count(),
            target_count,
            frame.meshes,
            frame.radius
        );
        model.frame = Some(frame);
    }
    if viewer.models.iter().all(|model| model.frame.is_some()) && viewer.ready_at.is_none() {
        viewer.ready_at = Some(Instant::now());
        viewer.smoke_stage_at = viewer.ready_at;
        println!("CREATURE_VIEWER_ALL_READY selected=hare live_idle=true static_reference=true");
    }
    if viewer.ready_at.is_none() && viewer.started.elapsed().as_secs() >= 90 {
        eprintln!(
            "CREATURE_VIEWER_LOAD_TIMEOUT: body, clip, live player or static bounds not ready"
        );
        exit.write(AppExit::error());
        return;
    }
    let prior = viewer.selected;
    if viewer.smoke {
        if let Some(ready) = viewer.ready_at {
            if ready.elapsed().as_secs() >= 90 {
                eprintln!("CREATURE_VIEWER_SMOKE_TIMEOUT: both saved capture pairs are required");
                exit.write(AppExit::error());
                return;
            }
            let first = viewer.selected * 2;
            let pair_saved = (first..first + 2).all(|index| {
                viewer.smoke_captures[index] && smoke_path(&viewer.output, index).is_file()
            });
            if viewer.smoke_stage_at.unwrap().elapsed().as_secs_f32() >= 6.0 && pair_saved {
                if viewer.selected == 0 {
                    viewer.selected = 1;
                    viewer.smoke_stage_at = Some(Instant::now());
                    viewer.smoke_first_capture_at = None;
                } else if (0..4).all(|index| smoke_path(&viewer.output, index).is_file()) {
                    println!(
                        "CREATURE_VIEWER_SMOKE_COMPLETE models=2 captures=4 live_players=2 static_players=0"
                    );
                    exit.write(AppExit::Success);
                    return;
                } else {
                    eprintln!("CREATURE_VIEWER_SMOKE_CAPTURE_INCOMPLETE");
                    exit.write(AppExit::error());
                    return;
                }
            }
        }
    } else {
        if keys.just_pressed(KeyCode::Digit1) {
            viewer.selected = 0;
        }
        if keys.just_pressed(KeyCode::Digit2) {
            viewer.selected = 1;
        }
        if keys.just_pressed(KeyCode::Space) {
            viewer.paused = !viewer.paused;
            println!("CREATURE_VIEWER_PLAYBACK paused={}", viewer.paused);
        }
    }
    if viewer.selected != prior {
        viewer.reset_camera = true;
        println!(
            "CREATURE_VIEWER_SWITCH model={}",
            SELECTIONS[viewer.selected].key
        );
    }
    for mut player in &mut players.p1() {
        if viewer.paused {
            player.pause_all();
        } else {
            player.resume_all();
        }
    }
    let selected = viewer.selected;
    let Some(frame) = viewer.models[selected].frame else {
        return;
    };
    if viewer.reset_camera || keys.just_pressed(KeyCode::KeyR) {
        viewer.yaw = 0.6;
        viewer.pitch = 0.15;
        viewer.distance = frame.radius * 4.8;
        viewer.reset_camera = false;
    }
    let dt = time.delta_secs();
    if keys.pressed(KeyCode::ArrowLeft) {
        viewer.yaw -= dt;
    }
    if keys.pressed(KeyCode::ArrowRight) {
        viewer.yaw += dt;
    }
    if keys.pressed(KeyCode::ArrowUp) {
        viewer.distance *= (-dt).exp();
    }
    if keys.pressed(KeyCode::ArrowDown) {
        viewer.distance *= dt.exp();
    }
    viewer.distance = viewer
        .distance
        .clamp(frame.radius * 1.5, frame.radius * 20.0);
    let right = Vec3::new(viewer.yaw.cos(), 0.0, -viewer.yaw.sin());
    for (root, mut transform, mut visibility) in &mut roots {
        *visibility = if root.model == selected {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if let Some(model_frame) = viewer.models[root.model].frame {
            let side = if root.live { -1.0 } else { 1.0 };
            transform.translation = right * side * model_frame.radius * 1.2 - model_frame.center;
        }
    }
    let offset = Vec3::new(
        viewer.yaw.sin() * viewer.pitch.cos(),
        viewer.pitch.sin(),
        viewer.yaw.cos() * viewer.pitch.cos(),
    );
    for mut camera in &mut cameras {
        *camera =
            Transform::from_translation(offset * viewer.distance).looking_at(Vec3::ZERO, Vec3::Y);
    }
    let state = if viewer.paused {
        "idle paused"
    } else {
        "idle playing"
    };
    for mut text in &mut instructions {
        **text = format!(
            "{} — {}\nLEFT: authored idle     RIGHT: before / no animation\n1: Hare   2: Wolf   Space: pause/resume   R: reset view\nArrows: orbit/zoom   F12: capture   Esc: close",
            SELECTIONS[selected].name, state
        );
    }
    let mut capture = None;
    if viewer.smoke {
        if let Some(stage) = viewer.smoke_stage_at {
            let first = selected * 2;
            let next = if !viewer.smoke_captures[first] && stage.elapsed().as_secs_f32() >= 1.5 {
                viewer.smoke_first_capture_at = Some(Instant::now());
                Some(first)
            } else if viewer.smoke_captures[first]
                && !viewer.smoke_captures[first + 1]
                && smoke_path(&viewer.output, first).is_file()
                && viewer
                    .smoke_first_capture_at
                    .is_some_and(|first| first.elapsed().as_secs_f32() >= 3.0)
            {
                Some(first + 1)
            } else {
                None
            };
            if let Some(index) = next {
                viewer.smoke_captures[index] = true;
                capture = Some(smoke_path(&viewer.output, index));
            }
        }
    } else if keys.just_pressed(KeyCode::F12) {
        capture = Some(viewer.output.join(format!(
            "{}-user-{:03}.png",
            SELECTIONS[selected].key, viewer.captures
        )));
        viewer.captures += 1;
    }
    if let Some(path) = capture {
        let live = viewer.models[selected].live.unwrap();
        let read_players = players.p0();
        let (_, player) = read_players
            .get(live)
            .expect("ready live player disappeared");
        let seek = player
            .playing_animations()
            .next()
            .map(|(_, active)| active.seek_time())
            .unwrap_or_default();
        println!(
            "CREATURE_VIEWER_CAPTURE model={} seek={} paused={} file={}",
            SELECTIONS[selected].key,
            seek,
            player.all_paused(),
            path.display()
        );
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path));
    }
}
