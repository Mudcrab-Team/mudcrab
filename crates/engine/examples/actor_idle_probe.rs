//! Bounded actor-only native skinning probe. Build/run require separate admission.
//! Uses the real catalog, glTF loader and ActorAnimationPlugin, with white unlit
//! skinned primitives. It does not exercise streaming spawn or production materials.
use bevy::{
    animation::{
        AnimatedBy, AnimationTargetId,
        graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeType},
    },
    camera::{RenderTarget, ScalingMode},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    gltf::{Gltf, GltfAssetLabel},
    mesh::{
        VertexAttributeValues,
        skinning::{SkinnedMesh, SkinnedMeshInverseBindposes},
    },
    prelude::*,
    render::{
        RenderPlugin,
        settings::{RenderCreation, WgpuFeatures, WgpuSettings, WgpuSettingsPriority},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
    window::{ExitCondition, WindowPlugin},
};
use engine::{
    actor_animation::{ActorAnimationPlugin, ActorClips, ActorIdleLibrary},
    actor_drive::ActorDrive,
    world::actor_catalog::ActorCatalog,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize)]
struct Case {
    name: String,
    base_form_id: u32,
    race_id: u32,
    sex: String,
    body_scene: String,
    source: String,
    clip_scene: String,
    body_sha256: String,
    clip_sha256: String,
    sample_seconds: f32,
}

#[derive(Deserialize)]
struct Contract {
    version: u32,
    dimensions: [u32; 2],
    settle_frames: u32,
    maximum_sample_vertices: usize,
    cases: Vec<Case>,
}

#[derive(Resource)]
struct Probe {
    output: PathBuf,
    case: Case,
    contract_sha256: String,
    scene: Handle<WorldAsset>,
    clip: Handle<Gltf>,
    target: Handle<Image>,
    actor: Option<Entity>,
    camera: Option<Entity>,
    started: Instant,
    phase: usize,
    frames: u32,
    settle_frames: u32,
    maximum_sample_vertices: usize,
    sought: bool,
    requested: bool,
    pending_pose: Option<Value>,
    poses: Vec<Value>,
    skin_count: usize,
    vertex_count: usize,
    curve_targets: usize,
}

fn hash(path: &Path) -> String {
    assert!(
        fs::metadata(path).unwrap().len() <= 256 * 1024 * 1024,
        "asset cap"
    );
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn probe_render_settings() -> WgpuSettings {
    let mut settings = WgpuSettings {
        priority: WgpuSettingsPriority::WebGPU,
        ..default()
    };
    // The frozen pack needs BC textures. Unsupported adapters fail device creation.
    settings.features |= WgpuFeatures::TEXTURE_COMPRESSION_BC;
    settings
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut assets = None;
    let mut contract_path = None;
    let mut name = None;
    let mut output = None;
    while let Some(arg) = args.next() {
        let value = args.next().expect("each option requires a value");
        match arg.as_str() {
            "--assets" => assets = Some(PathBuf::from(value)),
            "--contract" => contract_path = Some(PathBuf::from(value)),
            "--case" => name = Some(value),
            "--output" => output = Some(PathBuf::from(value)),
            _ => panic!("unknown option {arg}"),
        }
    }
    let assets = assets.unwrap().canonicalize().unwrap();
    let contract_path = contract_path.unwrap();
    let output = output.unwrap();
    assert!(output.is_absolute() && output.is_dir());
    let storage =
        PathBuf::from(std::env::var_os("MUDCRAB_STORAGE").expect("managed wrapper required"));
    assert!(std::env::var_os("MUDCRAB_BUILD_LEASE").is_some());
    assert!(
        output
            .canonicalize()
            .unwrap()
            .starts_with(storage.join("evidence").canonicalize().unwrap())
    );
    assert_eq!(
        std::env::var("MUDCRAB_ACTOR_IDLE_CONTRACT_SHA256").expect("admitted controller required"),
        hash(&contract_path)
    );
    for file in ["probe.json", "pose-0.png", "pose-1.png"] {
        assert!(!output.join(file).exists(), "refusing previous output");
    }
    assert!(fs::metadata(&contract_path).unwrap().len() <= 1024 * 1024);
    let contract: Contract = serde_json::from_slice(&fs::read(&contract_path).unwrap()).unwrap();
    assert_eq!(contract.version, 1);
    assert_eq!(contract.dimensions, [512, 512]);
    assert!((10..=60).contains(&contract.settle_frames));
    assert!((1..=4096).contains(&contract.maximum_sample_vertices));
    let cases: Vec<_> = contract
        .cases
        .iter()
        .filter(|case| Some(&case.name) == name.as_ref())
        .collect();
    assert_eq!(cases.len(), 1);
    let case = cases[0].clone();
    assert!(case.sample_seconds.is_finite() && case.sample_seconds > 0.0);
    for key in [&case.body_scene, &case.clip_scene] {
        assert!(shared::actor_idle_catalog::canonical_mesh_key(key, ".glb"));
    }
    assert!(shared::actor_idle_catalog::canonical_mesh_key(
        &case.source,
        ".hkx"
    ));
    assert_eq!(hash(&assets.join(&case.body_scene)), case.body_sha256);
    assert_eq!(hash(&assets.join(&case.clip_scene)), case.clip_sha256);
    let catalog = ActorCatalog::open(&assets.join("skyrim_world.db"));
    let plan = catalog
        .spawn_plan(catalog.get(case.base_form_id).unwrap())
        .unwrap();
    assert!(!plan.uses_composed_body());
    assert_eq!(plan.scene_path, case.body_scene);
    assert_eq!(plan.actor.race_id, case.race_id);
    assert_eq!(plan.actor.sex.as_str(), case.sex);
    assert_eq!(catalog.idle_scene(&plan), Some(case.clip_scene.as_str()));
    assert_eq!(
        catalog.authored_idle_scene(&plan.actor, &plan.scene_path, &case.source),
        Some(case.clip_scene.as_str())
    );

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: assets.to_string_lossy().into_owned(),
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(Box::new(probe_render_settings())),
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
    .add_plugins(ActorAnimationPlugin)
    .add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(
        Duration::from_millis(16),
    ))
    .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        Duration::ZERO,
    ))
    .insert_resource(catalog);
    let server = app.world().resource::<AssetServer>();
    let scene = server.load(GltfAssetLabel::Scene(0).from_asset(case.body_scene.clone()));
    let clip = server.load(case.clip_scene.clone());
    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::new_target_texture(
            512,
            512,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            None,
        ));
    let result = app
        .insert_resource(Probe {
            output,
            case,
            contract_sha256: hash(&contract_path),
            scene,
            clip,
            target,
            actor: None,
            camera: None,
            started: Instant::now(),
            phase: 0,
            frames: 0,
            settle_frames: contract.settle_frames,
            maximum_sample_vertices: contract.maximum_sample_vertices,
            sought: false,
            requested: false,
            pending_pose: None,
            poses: vec![],
            skin_count: 0,
            vertex_count: 0,
            curve_targets: 0,
        })
        .add_systems(Update, step)
        .run();
    if !result.is_success() {
        std::process::exit(1);
    }
}

/// Use the same joint-global * inverse-bind matrices that supply native skinning.
/// Every skin contributes; sample indices stay identical across the two poses.
fn skin_snapshot(
    world: &mut World,
    actor: Entity,
    maximum: usize,
) -> (Value, Vec3, Vec3, usize, usize) {
    let descendants: HashSet<_> = world
        .query::<(Entity, &SkinnedMesh)>()
        .iter(world)
        .filter(|(entity, _)| world.get::<Mesh3d>(*entity).is_some())
        .map(|(entity, _)| entity)
        .collect();
    let mut entities: Vec<_> = descendants.into_iter().collect();
    entities.sort_by_key(|entity| entity.to_bits());
    let belongs = |mut entity: Entity| {
        for _ in 0..512 {
            if entity == actor {
                return true;
            }
            let Some(parent) = world.get::<ChildOf>(entity) else {
                return false;
            };
            entity = parent.parent();
        }
        panic!("body hierarchy cap")
    };
    entities.retain(|entity| belongs(*entity));
    let meshes = world.resource::<Assets<Mesh>>();
    let inverse = world.resource::<Assets<SkinnedMeshInverseBindposes>>();
    let total: usize = entities
        .iter()
        .map(|entity| {
            let handle = &world.get::<Mesh3d>(*entity).unwrap().0;
            meshes.get(handle).unwrap().count_vertices()
        })
        .sum();
    assert!(total > 0 && total <= 1_000_000, "skin vertex cap");
    let stride = total.div_ceil(maximum);
    let mut cursor = 0;
    let mut vertices = Vec::new();
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for entity in &entities {
        let skin = world.get::<SkinnedMesh>(*entity).unwrap();
        let bind = inverse.get(&skin.inverse_bindposes).unwrap();
        let mesh = meshes
            .get(&world.get::<Mesh3d>(*entity).unwrap().0)
            .unwrap();
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("skin positions")
        };
        let Some(VertexAttributeValues::Uint16x4(indices)) =
            mesh.attribute(Mesh::ATTRIBUTE_JOINT_INDEX)
        else {
            panic!("skin joint indices")
        };
        let Some(VertexAttributeValues::Float32x4(weights)) =
            mesh.attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT)
        else {
            panic!("skin weights")
        };
        assert_eq!(positions.len(), indices.len());
        assert_eq!(positions.len(), weights.len());
        for (index, position) in positions.iter().enumerate() {
            let mut posed = Vec3::ZERO;
            let mut sum = 0.0;
            for (&joint, &weight) in indices[index].iter().zip(&weights[index]) {
                assert!(weight.is_finite() && weight >= 0.0);
                if weight == 0.0 {
                    continue;
                }
                let joint = usize::from(joint);
                let global = world
                    .get::<GlobalTransform>(skin.joints[joint])
                    .unwrap()
                    .to_matrix();
                posed +=
                    (global * bind[joint]).transform_point3(Vec3::from_array(*position)) * weight;
                sum += weight;
            }
            assert!(
                (sum - 1.0).abs() < 0.01 && posed.is_finite(),
                "invalid skin sample"
            );
            lo = lo.min(posed);
            hi = hi.max(posed);
            if cursor % stride == 0 {
                vertices.push(json!({"id":format!("{}:{index}", entity.to_bits()), "position":posed.to_array()}));
            }
            cursor += 1;
        }
    }
    (json!(vertices), lo, hi, entities.len(), total)
}

fn step(world: &mut World) {
    world.resource_scope(|world, mut probe: Mut<Probe>| {
        assert!(
            probe.started.elapsed() < Duration::from_secs(45),
            "actor probe deadline"
        );
        if probe.requested {
            return;
        }
        if probe.actor.is_none() {
            if !world
                .resource::<AssetServer>()
                .is_loaded_with_dependencies(probe.scene.id())
            {
                return;
            }
            let white = world
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial {
                    base_color: Color::WHITE,
                    unlit: true,
                    ..default()
                });
            {
            let mut scenes = world.resource_mut::<Assets<WorldAsset>>();
            let mut scene = scenes.get_mut(&probe.scene).unwrap();
            let entities: Vec<_> = scene
                .world
                .query::<(Entity, Option<&SkinnedMesh>)>()
                .iter(&scene.world)
                .filter(|(entity, _)| scene.world.get::<Mesh3d>(*entity).is_some())
                .map(|(entity, skin)| (entity, skin.is_some()))
                .collect();
            for (entity, skinned) in entities {
                if skinned {
                    scene
                        .world
                        .entity_mut(entity)
                        .insert(MeshMaterial3d(white.clone()));
                } else {
                    scene.world.entity_mut(entity).insert(Visibility::Hidden);
                }
            }
            }
            let catalog = world.resource::<ActorCatalog>();
            let plan = catalog
                .spawn_plan(catalog.get(probe.case.base_form_id).unwrap())
                .unwrap();
            let clips =
                ActorClips::load_idle(world.resource::<AssetServer>(), &probe.case.clip_scene);
            let actor = world
                .spawn((
                    WorldAssetRoot(probe.scene.clone()),
                    plan.actor,
                    ActorIdleLibrary {
                        body_scene: probe.case.body_scene.clone(),
                    },
                    clips,
                    Transform::default(),
                    Visibility::default(),
                ))
                .id();
            probe.actor = Some(actor);
            return;
        }
        let actor = probe.actor.unwrap();
        assert!(world.get::<ActorDrive>(actor).is_none());
        let Some(player) = world.get::<AnimationPlayer>(actor) else {
            return;
        };
        let Some((idle, _)) = player.playing_animations().next() else {
            return;
        };
        let idle = *idle;
        if !probe.sought {
            let seconds = if probe.phase == 0 {
                0.0
            } else {
                probe.case.sample_seconds
            };
            world
                .get_mut::<AnimationPlayer>(actor)
                .unwrap()
                .play(idle)
                .seek_to(seconds)
                .pause();
            probe.sought = true;
            probe.frames = 0;
            return;
        }
        probe.frames += 1;
        if probe.frames < probe.settle_frames {
            return;
        }
        let gltf = world.resource::<Assets<Gltf>>().get(&probe.clip).unwrap();
        let graph_handle = &world.get::<AnimationGraphHandle>(actor).unwrap().0;
        let graph = world
            .resource::<Assets<AnimationGraph>>()
            .get(graph_handle)
            .unwrap();
        let AnimationNodeType::Clip(handle) = &graph.graph[idle].node_type else {
            panic!("idle is not a clip")
        };
        assert!(gltf.animations.contains(handle), "wrong player clip");
        let targets: HashSet<_> = world
            .resource::<Assets<AnimationClip>>()
            .get(handle)
            .unwrap()
            .curves()
            .keys()
            .copied()
            .collect();
        let bound: HashSet<_> = world
            .query::<(&AnimatedBy, &AnimationTargetId)>()
            .iter(world)
            .filter(|(owner, _)| owner.0 == actor)
            .map(|(_, id)| *id)
            .collect();
        assert!(
            !targets.is_empty() && targets.is_subset(&bound),
            "unbound targets"
        );
        probe.curve_targets = targets.len();
        let (vertices, lo, hi, skin_count, vertex_count) =
            skin_snapshot(world, actor, probe.maximum_sample_vertices);
        probe.skin_count = skin_count;
        probe.vertex_count = vertex_count;
        if probe.camera.is_none() {
            let center = (lo + hi) * 0.5;
            let size = (hi - lo).max_element();
            assert!(size.is_finite() && size > 0.0);
            probe.camera = Some(
                world
                    .spawn((
                        Camera3d::default(),
                        RenderTarget::Image(probe.target.clone().into()),
                        Camera {
                            clear_color: ClearColorConfig::Custom(Color::BLACK),
                            ..default()
                        },
                        Projection::Orthographic(OrthographicProjection {
                            scaling_mode: ScalingMode::FixedVertical {
                                viewport_height: size * 1.5,
                            },
                            ..OrthographicProjection::default_3d()
                        }),
                        Transform::from_translation(
                            center + Vec3::new(1.0, 0.35, 1.0).normalize() * size * 3.0,
                        )
                        .looking_at(center, Vec3::Y),
                        Tonemapping::None,
                        DebandDither::Disabled,
                        Msaa::Off,
                    ))
                    .id(),
            );
            probe.frames = 0;
            return;
        }
        if probe.started.elapsed() < Duration::from_secs(1) {
            return;
        }
        probe.pending_pose = Some(
            json!({"seconds":if probe.phase == 0 {0.0} else {probe.case.sample_seconds},
            "settled_frames":probe.frames,"vertices":vertices,
            "camera_matrix":world.get::<Transform>(probe.camera.unwrap()).unwrap().to_matrix().to_cols_array()}),
        );
        world
            .spawn(Screenshot::image(probe.target.clone()))
            .observe(captured);
        probe.requested = true;
    });
}

fn captured(
    event: On<ScreenshotCaptured>,
    mut probe: ResMut<Probe>,
    adapter: Res<bevy::render::renderer::RenderAdapterInfo>,
    mut exit: MessageWriter<AppExit>,
) {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(probe.output.join(format!("pose-{}.png", probe.phase)))
        .unwrap();
    event
        .image
        .clone()
        .try_into_dynamic()
        .unwrap()
        .write_to(&mut BufWriter::new(file), image::ImageFormat::Png)
        .unwrap();
    let pose = probe.pending_pose.take().unwrap();
    probe.poses.push(pose);
    probe.phase += 1;
    probe.requested = false;
    probe.sought = false;
    if probe.phase != 2 {
        return;
    }
    let case = &probe.case;
    let report = json!({"version":1,"scope":"actor-only-unlit-skinning",
        "fixture_spawn":"catalog-plan-with-explicit-live-idle",
        "name":case.name,"base_form_id":case.base_form_id,"race_id":case.race_id,"sex":case.sex,
        "body_scene":case.body_scene,"source":case.source,"clip_scene":case.clip_scene,
        "body_sha256":case.body_sha256,"clip_sha256":case.clip_sha256,"contract_sha256":probe.contract_sha256,
        "actor_drive":false,"graph_clip_matches":true,"curve_targets":probe.curve_targets,"unbound_targets":0,
        "skinned_meshes":probe.skin_count,"skinned_vertices_total":probe.vertex_count,"readback_frames":2,
        "poses":probe.poses,"adapter":{"name":adapter.name,"backend":format!("{:?}",adapter.backend)},
        "limits":"No production streaming spawn, corpse selection, materials, world or all-idle native acceptance."});
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(probe.output.join("probe.json"))
        .unwrap();
    serde_json::to_writer_pretty(file, &report).unwrap();
    exit.write(AppExit::Success);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc_textures_keep_webgpu_limits_and_only_add_bc() {
        let previous = WgpuSettings {
            priority: WgpuSettingsPriority::WebGPU,
            ..default()
        };
        let settings = probe_render_settings();
        assert!(matches!(settings.priority, WgpuSettingsPriority::WebGPU));
        assert_eq!(
            settings.features,
            previous.features | WgpuFeatures::TEXTURE_COMPRESSION_BC
        );
        assert_eq!(settings.limits, previous.limits);
        assert_eq!(settings.constrained_limits, previous.constrained_limits);
        assert_eq!(settings.disabled_features, previous.disabled_features);
    }
}
