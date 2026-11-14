//! Data-driven, lazy actor-idle gallery built on the selected-creature inspection viewer.
use bevy::{
    animation::{
        AnimatedBy, AnimationTargetId,
        graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeType},
    },
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
    nif_material::{EffectPaletteMaterialPlugin, NifMaterialPlugin},
    world::actor_catalog::ActorCatalog,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Clone, Deserialize, Serialize)]
struct Entry {
    project: String,
    project_name: String,
    base_form_id: u32,
    race_id: u32,
    sex: String,
    body_scene: String,
    source: String,
    clip_scene: String,
    expected_bound_tracks: usize,
    moving_bound_tracks: usize,
    #[serde(default = "default_sample_delay")]
    sample_delay_seconds: f32,
    #[serde(default)]
    risk: String,
}

fn default_sample_delay() -> f32 {
    1.35
}

#[derive(Deserialize)]
struct Manifest {
    cases: Vec<Entry>,
}

struct Project {
    name: String,
    key: String,
    path: String,
    cases: Vec<usize>,
}

#[derive(Clone, Copy)]
struct Frame {
    center: Vec3,
    radius: f32,
    meshes: usize,
}

#[derive(Default, Clone, Serialize)]
struct Motion {
    sampled_targets: usize,
    changed_targets: usize,
    max_translation: f32,
    max_rotation_radians: f32,
    max_scale: f32,
    nonfinite_targets: usize,
    minimum_seek: Option<f32>,
    maximum_seek: Option<f32>,
}

struct Model {
    scene: Handle<WorldAsset>,
    clip: Handle<Gltf>,
    live: Entity,
    reference: Entity,
    directory: PathBuf,
    started: Instant,
    ready_at: Option<Instant>,
    frame: Option<Frame>,
    first_capture_at: Option<Instant>,
    captures: [bool; 2],
    seek_times: Vec<f32>,
    baseline: HashMap<Entity, Transform>,
    changed: HashSet<Entity>,
    motion: Motion,
    curve_targets: usize,
    hierarchy_targets: usize,
    unbound_targets: usize,
    failure: Option<String>,
    recorded: bool,
    visual_mark: Option<String>,
}

#[derive(Clone, Serialize)]
struct CaseRecord {
    case_index: usize,
    visit: usize,
    entry: Entry,
    status: String,
    reason: Option<String>,
    live_player: bool,
    static_players: usize,
    curve_targets: usize,
    hierarchy_targets: usize,
    unbound_targets: usize,
    motion: Motion,
    seek_times: Vec<f32>,
    captures: Vec<String>,
    visual_mark: Option<String>,
    elapsed_seconds: f32,
}

#[derive(Resource)]
struct Gallery {
    cases: Vec<Entry>,
    projects: Vec<Project>,
    assets: PathBuf,
    output: PathBuf,
    selected: usize,
    model: Option<Model>,
    selection_error: Option<String>,
    visit: usize,
    records: Vec<CaseRecord>,
    paused: bool,
    yaw: f32,
    pitch: f32,
    distance: f32,
    reset_camera: bool,
    user_captures: usize,
    started: Instant,
    tour: bool,
    tour_cases: Vec<usize>,
    tour_position: usize,
    finished: bool,
}

#[derive(Component)]
struct LiveCreature;
#[derive(Component)]
struct InspectionCamera;
#[derive(Component)]
struct Instructions;

#[derive(Default)]
struct Input {
    quit: bool,
    project_step: i32,
    default_step: i32,
    jump: Option<&'static str>,
    pause: bool,
    reset: bool,
    capture: bool,
    yaw: f32,
    zoom: f32,
    mark: Option<&'static str>,
}

fn key(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn settings() -> WgpuSettings {
    let mut value = WgpuSettings {
        backends: Some(Backends::METAL),
        priority: WgpuSettingsPriority::WebGPU,
        ..default()
    };
    value.features |= WgpuFeatures::TEXTURE_COMPRESSION_BC;
    value
}

fn save(path: &Path, value: &impl Serialize) {
    let temporary = path.with_extension("json.partial");
    std::fs::write(&temporary, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    std::fs::rename(temporary, path).unwrap();
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert!(
        args.len() >= 3,
        "actor_idle_gallery <absolute assets> <manifest.json> <new output> [--tour] [--projects draugr,dragon]"
    );
    let assets = PathBuf::from(&args[0]);
    let manifest_path = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);
    assert!(assets.is_absolute() && assets.is_dir());
    assert!(
        manifest_path.is_absolute() && manifest_path.metadata().unwrap().len() < 8 * 1024 * 1024
    );
    assert!(
        output.is_absolute() && !output.exists(),
        "preserve previous gallery output"
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
    let mut tour = false;
    let mut requested = HashSet::new();
    let mut index = 3;
    while index < args.len() {
        match args[index].as_str() {
            "--tour" => tour = true,
            "--projects" => {
                index += 1;
                requested.extend(
                    args.get(index)
                        .expect("--projects needs names")
                        .split(',')
                        .map(key),
                );
            }
            other => panic!("unknown gallery option {other}"),
        }
        index += 1;
    }
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    assert!(!manifest.cases.is_empty());
    let mut projects: Vec<Project> = Vec::new();
    for (index, entry) in manifest.cases.iter().enumerate() {
        assert!(
            entry.sample_delay_seconds.is_finite()
                && entry.sample_delay_seconds > 0.0
                && entry.sample_delay_seconds <= 60.0
        );
        assert!(shared::actor_idle_catalog::canonical_mesh_key(
            &entry.body_scene,
            ".glb"
        ));
        assert!(shared::actor_idle_catalog::canonical_mesh_key(
            &entry.clip_scene,
            ".glb"
        ));
        assert!(shared::actor_idle_catalog::canonical_mesh_key(
            &entry.source,
            ".hkx"
        ));
        if let Some(project) = projects
            .iter_mut()
            .find(|project| project.path == entry.project)
        {
            project.cases.push(index);
        } else {
            projects.push(Project {
                name: entry.project_name.clone(),
                key: key(&entry.project_name),
                path: entry.project.clone(),
                cases: vec![index],
            });
        }
    }
    assert!(
        requested
            .iter()
            .all(|name| projects.iter().any(|project| &project.key == name)),
        "unknown --projects name"
    );
    let tour_cases: Vec<_> = projects
        .iter()
        .filter(|project| requested.is_empty() || requested.contains(&project.key))
        .map(|project| project.cases[0])
        .collect();
    assert!(!tour_cases.is_empty());
    let selected = tour_cases[0];
    let _pack_lock =
        shared::asset_lock::AssetLock::acquire_shared(&assets).expect("fixture pack is busy");
    let catalog = ActorCatalog::open(&assets.join("skyrim_world.db"));
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
                    title: "Mudcrab — authored actor idle gallery".into(),
                    resolution: (1280, 800).into(),
                    ..default()
                }),
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(settings())),
                ..default()
            })
            .disable::<bevy::audio::AudioPlugin>(),
    )
    .add_plugins((
        EffectPaletteMaterialPlugin,
        NifMaterialPlugin,
        ActorAnimationPlugin,
    ))
    .insert_resource(catalog)
    .insert_resource(ClearColor(Color::srgb(0.025, 0.03, 0.04)))
    .insert_resource(GlobalAmbientLight {
        brightness: 40.0,
        ..default()
    })
    .insert_resource(Gallery {
        cases: manifest.cases,
        projects,
        assets,
        output,
        selected,
        model: None,
        selection_error: None,
        visit: 0,
        records: Vec::new(),
        paused: false,
        yaw: 0.6,
        pitch: 0.15,
        distance: 100.0,
        reset_camera: true,
        user_captures: 0,
        started: Instant::now(),
        tour,
        tour_cases,
        tour_position: 0,
        finished: false,
    })
    .add_systems(Startup, setup)
    .add_systems(Update, update);
    let result = app.run();
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
        Text::new("Loading actor gallery…"),
        Instructions,
        TextFont {
            font_size: FontSize::Px(19.0),
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

fn input(world: &World) -> Input {
    let keys = world.resource::<ButtonInput<KeyCode>>();
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let mut value = Input {
        quit: keys.just_pressed(KeyCode::Escape),
        pause: keys.just_pressed(KeyCode::Space),
        reset: keys.just_pressed(KeyCode::KeyR),
        capture: keys.just_pressed(KeyCode::F12),
        ..default()
    };
    if keys.just_pressed(KeyCode::PageDown) || keys.just_pressed(KeyCode::Tab) && !shift {
        value.project_step = 1;
    }
    if keys.just_pressed(KeyCode::PageUp) || keys.just_pressed(KeyCode::Tab) && shift {
        value.project_step = -1;
    }
    if keys.just_pressed(KeyCode::KeyN) {
        value.default_step = 1;
    }
    if keys.just_pressed(KeyCode::KeyP) {
        value.default_step = -1;
    }
    for (code, name) in [
        (KeyCode::Digit1, "draugr"),
        (KeyCode::Digit2, "draugr skeleton"),
        (KeyCode::Digit3, "dragon"),
        (KeyCode::Digit4, "dragon priest"),
    ] {
        if keys.just_pressed(code) {
            value.jump = Some(name);
        }
    }
    for (code, mark) in [
        (KeyCode::KeyC, "looks-correct"),
        (KeyCode::KeyT, "t-pose"),
        (KeyCode::KeyM, "no-motion"),
        (KeyCode::KeyW, "wrong-skeleton"),
    ] {
        if keys.just_pressed(code) {
            value.mark = Some(mark);
        }
    }
    value.yaw = if keys.pressed(KeyCode::ArrowRight) {
        1.0
    } else {
        0.0
    } - if keys.pressed(KeyCode::ArrowLeft) {
        1.0
    } else {
        0.0
    };
    value.zoom = if keys.pressed(KeyCode::ArrowDown) {
        1.0
    } else {
        0.0
    } - if keys.pressed(KeyCode::ArrowUp) {
        1.0
    } else {
        0.0
    };
    value
}

fn descendant(world: &World, mut entity: Entity, root: Entity) -> bool {
    for _ in 0..256 {
        if entity == root {
            return true;
        }
        let Some(parent) = world.get::<ChildOf>(entity) else {
            return false;
        };
        entity = parent.parent();
    }
    false
}

fn frame(world: &mut World, root: Entity) -> Option<Frame> {
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    let mut meshes = 0;
    let mut query = world.query::<(Entity, &Aabb, &GlobalTransform)>();
    for (entity, aabb, global) in query.iter(world) {
        if !descendant(world, entity, root) {
            continue;
        }
        meshes += 1;
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
    (meshes > 0 && low.is_finite() && high.is_finite()).then_some(Frame {
        center: (low + high) * 0.5,
        radius: ((high - low).length() * 0.5).max(0.1),
        meshes,
    })
}

fn spawn_model(world: &mut World, gallery: &mut Gallery) -> Result<Model, String> {
    gallery.visit += 1;
    let entry = &gallery.cases[gallery.selected];
    if !gallery.assets.join(&entry.body_scene).is_file()
        || !gallery.assets.join(&entry.clip_scene).is_file()
    {
        return Err("body or clip file is absent".into());
    }
    let plan = {
        let catalog = world.resource::<ActorCatalog>();
        let actor = catalog
            .get(entry.base_form_id)
            .ok_or("base actor is absent")?;
        let plan = catalog
            .spawn_plan(actor)
            .map_err(|error| error.to_string())?;
        if plan.uses_composed_body()
            || plan.scene_path != entry.body_scene
            || plan.actor.race_id != entry.race_id
            || plan.actor.sex.as_str() != entry.sex
        {
            return Err("selected base does not match the catalog body/race/sex".into());
        }
        if catalog.idle_scene(&plan) != Some(entry.clip_scene.as_str())
            || catalog.authored_idle_scene(&plan.actor, &plan.scene_path, &entry.source)
                != Some(entry.clip_scene.as_str())
        {
            return Err("selected authored default does not match the catalog".into());
        }
        plan
    };
    let scene = world
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset(entry.body_scene.clone()));
    let clip = world
        .resource::<AssetServer>()
        .load(entry.clip_scene.clone());
    let clips = ActorClips::load_idle(world.resource::<AssetServer>(), &entry.clip_scene);
    let directory = gallery.output.join(format!(
        "visit-{:03}-case-{:03}",
        gallery.visit, gallery.selected
    ));
    std::fs::create_dir(&directory).map_err(|error| error.to_string())?;
    let live = world
        .spawn((
            WorldAssetRoot(scene.clone()),
            plan.actor,
            ActorIdleLibrary {
                body_scene: entry.body_scene.clone(),
            },
            clips,
            LiveCreature,
            Transform::default(),
            Visibility::Hidden,
        ))
        .id();
    let reference = world
        .spawn((
            WorldAssetRoot(scene.clone()),
            plan.actor,
            Transform::default(),
            Visibility::Hidden,
        ))
        .id();
    println!(
        "GALLERY_CASE_START project={} default={} base={:08X} body={}",
        key(&entry.project_name),
        gallery.selected,
        entry.base_form_id,
        entry.body_scene
    );
    Ok(Model {
        scene,
        clip,
        live,
        reference,
        directory,
        started: Instant::now(),
        ready_at: None,
        frame: None,
        first_capture_at: None,
        captures: [false; 2],
        seek_times: Vec::new(),
        baseline: HashMap::new(),
        changed: HashSet::new(),
        motion: Motion::default(),
        curve_targets: 0,
        hierarchy_targets: 0,
        unbound_targets: 0,
        failure: None,
        recorded: false,
        visual_mark: None,
    })
}

fn unload(world: &mut World, model: Model) {
    world.despawn(model.live);
    world.despawn(model.reference);
    // Dropping the two strong scene/clip handles permits AssetServer dependency release.
}

fn pose(world: &mut World, model: &mut Model) {
    let mut query = world.query::<(Entity, &AnimatedBy, &Transform)>();
    for (entity, owner, transform) in query.iter(world) {
        if owner.0 != model.live {
            continue;
        }
        if !transform.translation.is_finite()
            || !transform.rotation.is_finite()
            || !transform.scale.is_finite()
        {
            model.motion.nonfinite_targets += 1;
            continue;
        }
        if let Some(initial) = model.baseline.get(&entity) {
            let translation = transform.translation.distance(initial.translation);
            let rotation = transform.rotation.angle_between(initial.rotation).abs();
            let scale = transform.scale.distance(initial.scale);
            model.motion.max_translation = model.motion.max_translation.max(translation);
            model.motion.max_rotation_radians = model.motion.max_rotation_radians.max(rotation);
            model.motion.max_scale = model.motion.max_scale.max(scale);
            if translation > 1e-6 || rotation > 1e-5 || scale > 1e-6 {
                model.changed.insert(entity);
            }
        } else {
            model.baseline.insert(entity, *transform);
        }
    }
    model.motion.sampled_targets = model.baseline.len();
    model.motion.changed_targets = model.changed.len();
    if let Some(player) = world.get::<AnimationPlayer>(model.live)
        && let Some((_, active)) = player.playing_animations().next()
    {
        let seek = active.seek_time();
        model.motion.minimum_seek = Some(
            model
                .motion
                .minimum_seek
                .map_or(seek, |value| value.min(seek)),
        );
        model.motion.maximum_seek = Some(
            model
                .motion
                .maximum_seek
                .map_or(seek, |value| value.max(seek)),
        );
    }
}

fn binding(world: &mut World, model: &mut Model) {
    let mut query = world.query::<(&AnimatedBy, &AnimationTargetId)>();
    let bound: HashSet<_> = query
        .iter(world)
        .filter(|(owner, _)| owner.0 == model.live)
        .map(|(_, id)| *id)
        .collect();
    model.hierarchy_targets = bound.len();
    let Some(player) = world.get::<AnimationPlayer>(model.live) else {
        return;
    };
    let Some((index, _)) = player.playing_animations().next() else {
        return;
    };
    let Some(handle) = world.get::<AnimationGraphHandle>(model.live) else {
        return;
    };
    let Some(graph) = world.resource::<Assets<AnimationGraph>>().get(&handle.0) else {
        return;
    };
    let AnimationNodeType::Clip(clip) = &graph.graph[*index].node_type else {
        return;
    };
    let Some(clip) = world.resource::<Assets<AnimationClip>>().get(clip.id()) else {
        return;
    };
    model.curve_targets = clip.curves().len();
    model.unbound_targets = clip
        .curves()
        .keys()
        .filter(|id| !bound.contains(*id))
        .count();
}

fn freeze_reference(world: &mut World, root: Entity) {
    // Some legacy body files contain an embedded player. This separate hierarchy is
    // deliberately the rest reference, so never let that player animate its bones.
    let mut query = world.query_filtered::<Entity, With<AnimationPlayer>>();
    let players: Vec<_> = query
        .iter(world)
        .filter(|entity| descendant(world, *entity, root))
        .collect();
    for entity in players {
        world
            .entity_mut(entity)
            .remove::<AnimationPlayer>()
            .remove::<AnimationGraphHandle>();
    }
}

fn project_index(gallery: &Gallery) -> usize {
    gallery
        .projects
        .iter()
        .position(|project| project.cases.contains(&gallery.selected))
        .unwrap()
}

fn write_report(gallery: &Gallery, state: &str) {
    let projects: Vec<_> = gallery.projects.iter().map(|project| {
        let records: Vec<_> = gallery.records.iter().filter(|record| record.entry.project == project.path).collect();
        let visited: HashSet<_> = records.iter().map(|record| record.case_index).collect();
        serde_json::json!({ "project": project.path, "project_name": project.name, "default_contracts": project.cases.len(), "visited_default_contracts": visited.len(), "records": records })
    }).collect();
    save(
        &gallery.output.join("gallery-report.json"),
        &serde_json::json!({
            "state": state, "catalog_projects": gallery.projects.len(), "catalog_default_contracts": gallery.cases.len(),
            "visited_projects": projects.iter().filter(|project| project["visited_default_contracts"].as_u64().unwrap_or(0) > 0).count(),
            "tour_requested_projects": gallery.tour_cases.len(), "tour_one_default_per_project": gallery.tour,
            "records": gallery.records, "projects": projects,
            "limits": ["Transform motion and player readiness are diagnostics, not proof of visually correct anatomy or adequate deformation.", "Independent rest bodies have no skeletal animation player. Material/effect time may remain. A marked project covers only the displayed default.", "No production spawn, Starts Dead, world, behavior graph or Skyrim parity acceptance."]
        }),
    );
    let lines: Vec<_> = gallery
        .projects
        .iter()
        .map(|project| {
            let records: Vec<_> = gallery
                .records
                .iter()
                .filter(|record| record.entry.project == project.path)
                .collect();
            let visited: HashSet<_> = records.iter().map(|record| record.case_index).collect();
            let mut statuses: Vec<_> = records
                .iter()
                .map(|record| record.visual_mark.as_deref().unwrap_or(&record.status))
                .collect();
            statuses.sort_unstable();
            statuses.dedup();
            let status = if statuses.is_empty() {
                "not-visited".into()
            } else {
                statuses.join(", ")
            };
            format!(
                "{} | {}/{} defaults visited | {}",
                project.name,
                visited.len(),
                project.cases.len(),
                status
            )
        })
        .collect();
    std::fs::write(gallery.output.join("projects.txt"), lines.join("\n") + "\n").unwrap();
}

fn record(world: &mut World, gallery: &mut Gallery, model: &mut Model) {
    if model.recorded {
        return;
    }
    let entry = gallery.cases[gallery.selected].clone();
    let pair_saved = model.captures.iter().all(|value| *value)
        && (0..2).all(|index| model.directory.join(format!("pose-{index}.png")).is_file());
    let status = if model.failure.is_some() {
        "case-failed"
    } else if !pair_saved {
        "incomplete-observation"
    } else if model.motion.nonfinite_targets > 0 {
        "nonfinite-pose"
    } else if model.unbound_targets > 0 {
        "unbound-targets"
    } else if entry.moving_bound_tracks == 0 {
        "authored-static"
    } else if gallery.paused {
        "paused-observation"
    } else if model.motion.changed_targets == 0 {
        "no-motion-observed"
    } else {
        "motion-observed"
    };
    let mut players = world.query::<(Entity, &AnimationPlayer)>();
    let static_players = players
        .iter(world)
        .filter(|(entity, _)| descendant(world, *entity, model.reference))
        .count();
    let row = CaseRecord {
        case_index: gallery.selected,
        visit: gallery.visit,
        entry,
        status: status.into(),
        reason: model.failure.clone(),
        live_player: world.get::<AnimationPlayer>(model.live).is_some(),
        static_players,
        curve_targets: model.curve_targets,
        hierarchy_targets: model.hierarchy_targets,
        unbound_targets: model.unbound_targets,
        motion: model.motion.clone(),
        seek_times: model.seek_times.clone(),
        captures: (0..2)
            .filter(|index| model.directory.join(format!("pose-{index}.png")).is_file())
            .map(|index| {
                model
                    .directory
                    .join(format!("pose-{index}.png"))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
        visual_mark: model.visual_mark.clone(),
        elapsed_seconds: model.started.elapsed().as_secs_f32(),
    };
    println!(
        "GALLERY_CASE_RESULT project={} default={} status={} changed_targets={} curve_targets={} unbound_targets={}",
        key(&row.entry.project_name),
        row.case_index,
        row.status,
        row.motion.changed_targets,
        row.curve_targets,
        row.unbound_targets
    );
    save(&model.directory.join("case.json"), &row);
    gallery.records.push(row);
    model.recorded = true;
    write_report(gallery, "running");
}

fn advance_tour(world: &mut World, gallery: &mut Gallery) {
    gallery.tour_position += 1;
    if gallery.tour_position >= gallery.tour_cases.len() {
        gallery.finished = true;
        write_report(gallery, "tour-complete");
        println!(
            "GALLERY_TOUR_COMPLETE projects={} defaults={}",
            gallery.tour_cases.len(),
            gallery.records.len()
        );
        world.write_message(AppExit::Success);
    } else {
        gallery.selected = gallery.tour_cases[gallery.tour_position];
        gallery.selection_error = None;
        gallery.reset_camera = true;
    }
}

fn selection_error(world: &mut World, gallery: &mut Gallery, reason: String) {
    let entry = gallery.cases[gallery.selected].clone();
    println!(
        "GALLERY_CASE_FAILED project={} default={} reason={reason}",
        key(&entry.project_name),
        gallery.selected
    );
    gallery.records.push(CaseRecord {
        case_index: gallery.selected,
        visit: gallery.visit,
        entry,
        status: "selection-unresolved".into(),
        reason: Some(reason.clone()),
        live_player: false,
        static_players: 0,
        curve_targets: 0,
        hierarchy_targets: 0,
        unbound_targets: 0,
        motion: Motion::default(),
        seek_times: Vec::new(),
        captures: Vec::new(),
        visual_mark: None,
        elapsed_seconds: 0.0,
    });
    gallery.selection_error = Some(reason);
    write_report(gallery, "running");
    if gallery.tour {
        advance_tour(world, gallery);
    }
}

fn update(world: &mut World) {
    let input = input(world);
    let dt = world.resource::<Time>().delta_secs();
    world.resource_scope(|world, mut gallery: Mut<Gallery>| {
        if gallery.finished { return; }
        let limit = if gallery.tour { 900 } else { 1800 };
        if input.quit || gallery.started.elapsed().as_secs() >= limit {
            gallery.finished = true;
            if let Some(mut model) = gallery.model.take() { record(world, &mut gallery, &mut model); gallery.model = Some(model); }
            write_report(&gallery, if input.quit { "user-closed" } else { "time-limit" });
            println!("GALLERY_EXIT reason={}", if input.quit { "user" } else { "time-limit" });
            world.write_message(AppExit::Success); return;
        }
        if !gallery.tour {
            let project = project_index(&gallery);
            let mut selected = gallery.selected;
            if input.project_step != 0 {
                let next = (project as i32 + input.project_step).rem_euclid(gallery.projects.len() as i32) as usize;
                selected = gallery.projects[next].cases[0];
            }
            if input.default_step != 0 {
                let cases = &gallery.projects[project].cases;
                let local = cases.iter().position(|index| *index == gallery.selected).unwrap();
                selected = cases[(local as i32 + input.default_step).rem_euclid(cases.len() as i32) as usize];
            }
            if let Some(name) = input.jump
                && let Some(project) = gallery.projects.iter().find(|project| project.key == key(name))
            { selected = project.cases[0]; }
            if selected != gallery.selected {
                if let Some(mut model) = gallery.model.take() { record(world, &mut gallery, &mut model); unload(world, model); }
                gallery.selected = selected; gallery.selection_error = None; gallery.reset_camera = true;
            }
            if input.pause { gallery.paused = !gallery.paused; }
        }
        if gallery.model.is_none() && gallery.selection_error.is_none() {
            match spawn_model(world, &mut gallery) {
                Ok(model) => gallery.model = Some(model),
                Err(reason) => { selection_error(world, &mut gallery, reason); return; }
            }
        }
        let Some(mut model) = gallery.model.take() else {
            hud(world, &gallery, "Unavailable — use N/P or Page Down"); return;
        };
        let mut loaded = true;
        for id in [model.scene.id().untyped(), model.clip.id().untyped()] {
            let server = world.resource::<AssetServer>();
            if let bevy::asset::LoadState::Failed(reason) = server.load_state(id) { model.failure = Some(format!("asset load failed: {reason}")); }
            loaded &= server.is_loaded_with_dependencies(id);
        }
        if model.frame.is_none() && loaded { model.frame = frame(world, model.reference); }
        let has_player = world.get::<AnimationPlayer>(model.live).is_some_and(|player| player.playing_animations().next().is_some());
        if model.ready_at.is_none() && loaded && has_player && model.frame.is_some() {
            binding(world, &mut model);
            if model.curve_targets > 0 {
                model.ready_at = Some(Instant::now());
                println!("GALLERY_CASE_READY project={} default={} curve_targets={} unbound_targets={} meshes={}", key(&gallery.cases[gallery.selected].project_name), gallery.selected, model.curve_targets, model.unbound_targets, model.frame.unwrap().meshes);
            }
        }
        if model.started.elapsed().as_secs() >= 20 && model.ready_at.is_none() { model.failure.get_or_insert_with(|| "body, clip or idle player not ready within20s".into()); }
        freeze_reference(world, model.reference);
        if let Some(player) = world.get_mut::<AnimationPlayer>(model.live) {
            let mut player = player;
            if gallery.paused { player.pause_all(); } else { player.resume_all(); }
        }
        if model.ready_at.is_some() { pose(world, &mut model); }
        if let Some(mark) = input.mark {
            model.visual_mark = Some(mark.into());
            if model.recorded {
                let visit = gallery.visit;
                if let Some(row) = gallery.records.iter_mut().find(|row| row.visit == visit) { row.visual_mark = model.visual_mark.clone(); save(&model.directory.join("case.json"), row); }
                write_report(&gallery, "running");
            }
        }
        if let Some(frame) = model.frame {
            if gallery.reset_camera || input.reset { gallery.yaw = 0.6; gallery.pitch = 0.15; gallery.distance = frame.radius * 4.8; gallery.reset_camera = false; }
            gallery.yaw += input.yaw * dt; gallery.distance *= (input.zoom * dt).exp();
            gallery.distance = gallery.distance.clamp(frame.radius * 1.5, frame.radius * 20.0);
            let right = Vec3::new(gallery.yaw.cos(), 0.0, -gallery.yaw.sin());
            for (entity, side) in [(model.live, -1.0), (model.reference, 1.0)] {
                world.get_mut::<Transform>(entity).unwrap().translation = right * side * frame.radius * 1.2 - frame.center;
                *world.get_mut::<Visibility>(entity).unwrap() = Visibility::Visible;
            }
            let offset = Vec3::new(gallery.yaw.sin() * gallery.pitch.cos(), gallery.pitch.sin(), gallery.yaw.cos() * gallery.pitch.cos());
            let mut cameras = world.query_filtered::<&mut Transform, With<InspectionCamera>>();
            for mut camera in cameras.iter_mut(world) { *camera = Transform::from_translation(offset * gallery.distance).looking_at(Vec3::ZERO, Vec3::Y); }
        }
        let state = if model.failure.is_some() { "Idle unavailable" } else if model.ready_at.is_none() { "Loading…" } else if gallery.paused { "Idle paused" } else { "Idle playing" };
        hud(world, &gallery, state);
        let mut capture = None;
        if let Some(ready) = model.ready_at {
            if !model.captures[0] && ready.elapsed().as_secs_f32() >= 1.0 {
                model.first_capture_at = Some(Instant::now()); model.captures[0] = true; capture = Some(model.directory.join("pose-0.png"));
            } else if model.captures[0] && !model.captures[1] && model.directory.join("pose-0.png").is_file() && model.first_capture_at.is_some_and(|first| first.elapsed().as_secs_f32() >= gallery.cases[gallery.selected].sample_delay_seconds) {
                model.captures[1] = true; capture = Some(model.directory.join("pose-1.png"));
            }
        }
        if input.capture && !gallery.tour && capture.is_none() {
            capture = Some(model.directory.join(format!("user-{:03}.png", gallery.user_captures))); gallery.user_captures += 1;
        }
        if let Some(path) = capture {
            let seek = world.get::<AnimationPlayer>(model.live).and_then(|player| player.playing_animations().next().map(|(_, active)| active.seek_time())).unwrap_or_default();
            model.seek_times.push(seek);
            println!("GALLERY_CAPTURE project={} default={} seek={} changed_targets={} file={}", key(&gallery.cases[gallery.selected].project_name), gallery.selected, seek, model.motion.changed_targets, path.display());
            world.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        }
        if model.ready_at.is_some_and(|ready| ready.elapsed().as_secs_f32() > gallery.cases[gallery.selected].sample_delay_seconds + 20.0) && !model.recorded {
            model.failure.get_or_insert_with(|| "capture pair did not finish within its bounded window".into());
        }
        let pair_saved = model.captures.iter().all(|value| *value) && (0..2).all(|index| model.directory.join(format!("pose-{index}.png")).is_file());
        if model.failure.is_some() || pair_saved { record(world, &mut gallery, &mut model); }
        if gallery.tour && model.recorded {
            unload(world, model); advance_tour(world, &mut gallery);
        } else { gallery.model = Some(model); }
    });
}

fn hud(world: &mut World, gallery: &Gallery, state: &str) {
    let project = project_index(gallery);
    let entry = &gallery.cases[gallery.selected];
    let local = gallery.projects[project]
        .cases
        .iter()
        .position(|index| *index == gallery.selected)
        .unwrap();
    let body = Path::new(&entry.body_scene)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let idle = Path::new(&entry.source)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let text = format!(
        "{} — project {}/{} — default {}/{} — {}\n{} · {} · {}\nLEFT: authored idle   RIGHT: no skeletal idle / rest pose\n1 Draugr · 2 Skeleton · 3 Dragon · 4 Dragon priest\nTab/Page Down: next project · Page Up: previous · N/P: defaults\nSpace: pause · Arrows: orbit/zoom · R: reset · F12: capture · Esc: close\nC: looks correct · T: T-pose · M: no motion · W: wrong skeleton",
        gallery.projects[project].name,
        project + 1,
        gallery.projects.len(),
        local + 1,
        gallery.projects[project].cases.len(),
        state,
        body,
        entry.sex,
        idle
    );
    let mut instructions = world.query_filtered::<&mut Text, With<Instructions>>();
    for mut value in instructions.iter_mut(world) {
        **value = text.clone();
    }
}
