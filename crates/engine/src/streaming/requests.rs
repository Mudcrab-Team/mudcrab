//! Opt-in admission for full-detail and LOD scenes in one immutable asset pack.

use super::{
    CellStatus, PendingModel, RenderOrigin, StreamingWorld,
    admission::{SceneAdmission, SceneDemand, SceneJobStatus, SceneKey},
    priority::{self, DemandPriority, PriorityView},
};
use crate::{
    config::EngineConfig,
    profiling::ProfilingState,
    world::components::{CELL_SIZE, InstanceBounds, StreamingCamera, WorldPosition},
};
use bevy::{
    asset::{LoadState, RecursiveDependencyLoadState},
    gltf::GltfAssetLabel,
    math::DVec3,
    prelude::*,
    world_serialization::WorldAsset,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Instant};

pub(super) fn enabled(config: &EngineConfig) -> bool {
    config.prioritize_streaming || config.max_scene_loads != 0
}

/// Remains on the placement until its owner despawns, so resource ownership includes
/// already-activated models. Queued requests have no asset handle.
#[derive(Component)]
pub(super) struct SceneRequest {
    pub path: String,
    pub cell: Option<crate::world::database::CellKey>,
    pub coarse_terrain: bool,
    pub retry_cleanup: Option<bevy::asset::AssetId<WorldAsset>>,
    pub content_identity: Option<String>,
    pub sequence: u64,
    pub center: DVec3,
    pub radius: Option<f64>,
    pub collision_candidate: bool,
    pub handle: Option<Handle<WorldAsset>>,
    pub started: Instant,
    pub allow_retry: bool,
}

#[derive(Component)]
struct SceneRequestKey(SceneKey);

#[derive(Resource)]
pub(super) struct SceneSchedulingState {
    pack_identity: String,
    pub dispatch_choices: u64,
    pub activation_choices: u64,
}

impl FromWorld for SceneSchedulingState {
    fn from_world(world: &mut World) -> Self {
        let config = world.resource::<EngineConfig>();
        let root =
            std::path::absolute(&config.assets_dir).unwrap_or_else(|_| config.assets_dir.clone());
        // Hash metadata, never decode the pack to discover costs. Synthetic fixtures can
        // have no manifest; their identity is then scoped to this immutable asset root.
        let fingerprint = if enabled(config) {
            std::fs::read(root.join("conversion-manifest.json"))
                .ok()
                .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
        } else {
            None
        };
        Self {
            pack_identity: format!(
                "{}#{}",
                root.display(),
                fingerprint.as_deref().unwrap_or("unmanifested")
            ),
            dispatch_choices: 0,
            activation_choices: 0,
        }
    }
}

pub(super) fn reference_geometry(
    position: WorldPosition,
    transform: &Transform,
    bounds: Option<InstanceBounds>,
) -> (DVec3, Option<f64>) {
    let origin = DVec3::new(
        f64::from(position.grid.x) * f64::from(CELL_SIZE) + f64::from(position.local.x),
        f64::from(position.local.z),
        -(f64::from(position.grid.y) * f64::from(CELL_SIZE) + f64::from(position.local.y)),
    );
    match bounds {
        Some(bounds) => (
            origin + ((bounds.min + bounds.max) * 0.5 - transform.translation).as_dvec3(),
            Some(((bounds.max - bounds.min) * 0.5).as_dvec3().length()),
        ),
        None => (origin, None),
    }
}

pub(super) fn priority_view(world: &mut World) -> Option<PriorityView> {
    let origin = world
        .get_resource::<RenderOrigin>()
        .map_or(IVec2::ZERO, |origin| origin.0);
    let mut query = world.query_filtered::<&Transform, With<StreamingCamera>>();
    let camera = query.single(world).ok()?;
    Some(PriorityView {
        position: camera.translation.as_dvec3()
            + DVec3::new(
                f64::from(origin.x) * f64::from(CELL_SIZE),
                0.0,
                -f64::from(origin.y) * f64::from(CELL_SIZE),
            ),
        forward: (camera.rotation * Vec3::NEG_Z).as_dvec3(),
    })
}

pub(super) fn relevant(request: &SceneRequest, streaming: &StreamingWorld) -> bool {
    request
        .cell
        .is_none_or(|key| matches!(streaming.cells.get(&key), Some(CellStatus::Resident { .. })))
}

fn job_status(server: &AssetServer, handle: &Handle<WorldAsset>) -> SceneJobStatus {
    match server.get_load_states(handle.id()) {
        Some((LoadState::Failed(_), _, _))
        | Some((_, _, RecursiveDependencyLoadState::Failed(_))) => SceneJobStatus::Failed,
        Some((LoadState::Loaded, _, RecursiveDependencyLoadState::Loaded)) => SceneJobStatus::Ready,
        // Missing loader state does not confirm cancellation. Keep the slot and handle.
        _ => SceneJobStatus::Loading,
    }
}

pub(super) fn dispatch_scene_requests(world: &mut World) {
    let config = world.resource::<EngineConfig>();
    if !enabled(config) {
        return;
    }
    let started = Instant::now();
    let cap = config.max_scene_loads;
    let prioritize = config.prioritize_streaming;
    let pack_identity = world
        .resource::<SceneSchedulingState>()
        .pack_identity
        .clone();
    let server = world.resource::<AssetServer>().clone();
    let mut new_requests =
        world.query_filtered::<(Entity, &SceneRequest), Without<SceneRequestKey>>();
    let keys: Vec<_> = new_requests
        .iter(world)
        .map(|(entity, request)| {
            (
                entity,
                request.allow_retry,
                SceneKey {
                    canonical_path: request.path.clone(),
                    build_identity: Some(pack_identity.clone()),
                    content_identity: request.content_identity.clone(),
                },
            )
        })
        .collect();
    let mut live_requests = world.query_filtered::<Entity, With<SceneRequest>>();
    let live: std::collections::HashSet<_> = live_requests.iter(world).collect();
    for (entity, allow_retry, key) in keys {
        if allow_retry {
            let previous = world.resource::<SceneAdmission>().handle(&key);
            let reset = world
                .resource_mut::<SceneAdmission>()
                .retry_failed_if_unowned(&key, &live);
            if reset
                && let Some(previous) = previous
                && matches!(
                    server.get_load_states(previous.id()),
                    Some((
                        LoadState::Loaded,
                        _,
                        RecursiveDependencyLoadState::Failed(_)
                    ))
                )
            {
                // Request-mode load does not restart a loaded root with failed dependencies.
                // Let the unowned asset actually leave the loader before requesting it again.
                world.get_mut::<SceneRequest>(entity).unwrap().retry_cleanup = Some(previous.id());
            }
        }
        world.entity_mut(entity).insert(SceneRequestKey(key));
    }
    let view = if prioritize {
        priority_view(world)
    } else {
        None
    }
    .unwrap_or(PriorityView {
        position: DVec3::NAN,
        forward: DVec3::NAN,
    });
    let mut requests = world.query::<(Entity, &SceneRequestKey, &SceneRequest)>();
    let demands: Vec<_> = requests
        .iter(world)
        .filter(|(_, _, request)| {
            request.handle.is_some() || relevant(request, world.resource::<StreamingWorld>())
        })
        .map(|(entity, key, request)| SceneDemand {
            key: key.0.clone(),
            subscriber: entity,
            existing_handle: request.handle.clone(),
            priority: request.sequence,
        })
        .collect();
    let mut priorities: BTreeMap<SceneKey, Vec<DemandPriority>> = BTreeMap::new();
    for demand in &demands {
        let request = world.get::<SceneRequest>(demand.subscriber).unwrap();
        if request.handle.is_some() {
            continue;
        }
        let priority = DemandPriority::new(
            view,
            request.center,
            request.radius,
            request.collision_candidate,
            request.sequence,
        )
        .with_coarse_terrain(request.coarse_terrain);
        priorities
            .entry(demand.key.clone())
            .or_default()
            .push(priority);
    }
    world
        .resource_mut::<SceneAdmission>()
        .reconcile_demands(demands.iter().cloned());
    let jobs = world.resource::<SceneAdmission>().jobs();
    for (key, handle) in jobs {
        let status = job_status(&server, &handle);
        world
            .resource_mut::<SceneAdmission>()
            .set_status(&key, status);
    }
    let cleanup_waiting: std::collections::HashSet<_> = requests
        .iter(world)
        .filter(|(_, _, request)| {
            request.retry_cleanup.is_some_and(|id| {
                matches!(
                    server.get_load_states(id),
                    Some((
                        LoadState::Loaded,
                        _,
                        RecursiveDependencyLoadState::Failed(_)
                    ))
                )
            })
        })
        .map(|(_, key, _)| key.0.clone())
        .collect();
    let queued: Vec<_> = world
        .resource::<SceneAdmission>()
        .queued_keys(0)
        .into_iter()
        .filter(|key| !cleanup_waiting.contains(key))
        .filter_map(|key| {
            priority::shared_scene_priority(priorities.remove(&key)?)
                .map(|priority| (key, priority))
        })
        .collect();
    let mut choices = world.resource::<SceneSchedulingState>().dispatch_choices;
    let slots = world
        .resource::<SceneAdmission>()
        .available_slots(cap)
        .min(queued.len());
    let selected: Vec<_> = if prioritize {
        priority::ordered_choices(
            &queued.iter().map(|(_, rank)| *rank).collect::<Vec<_>>(),
            choices,
            slots,
        )
    } else {
        // queued_keys already orders by subscriber age, with deterministic key ties.
        (0..slots).collect()
    };
    for index in selected {
        let key = &queued[index].0;
        let handle = server.load(GltfAssetLabel::Scene(0).from_asset(key.canonical_path.clone()));
        if world
            .resource_mut::<SceneAdmission>()
            .dispatch(key, handle.clone())
        {
            choices = choices.saturating_add(1);
            let mut profiler = world.resource_mut::<ProfilingState>();
            profiler.observe_scene(handle.id(), &server);
            profiler.event(&key.canonical_path, "scene_dispatched", None);
        }
    }
    world
        .resource_mut::<SceneSchedulingState>()
        .dispatch_choices = choices;
    // Fan out the same strong handle, including failure, without a new AssetServer::load.
    for demand in demands {
        if world
            .get::<SceneRequest>(demand.subscriber)
            .unwrap()
            .handle
            .is_some()
        {
            continue;
        }
        let Some(handle) = world.resource::<SceneAdmission>().handle(&demand.key) else {
            continue;
        };
        let mut request = world.get_mut::<SceneRequest>(demand.subscriber).unwrap();
        request.handle = Some(handle.clone());
        let sequence = request.sequence;
        let queued_at = request.started;
        world.entity_mut(demand.subscriber).insert(PendingModel {
            handle: handle.clone(),
            sequence,
        });
        super::lod::start_admitted_chunk(world, demand.subscriber, handle);
        world
            .resource_mut::<ProfilingState>()
            .record_completed_latency_elapsed("streaming/scene_admission_wait", queued_at);
    }
    let stats = world.resource::<SceneAdmission>().stats();
    let mut profiler = world.resource_mut::<ProfilingState>();
    for (name, count) in [
        ("active_jobs", stats.active_jobs),
        ("queued_jobs", stats.queued_jobs),
        ("records", stats.records),
        ("subscribers", stats.subscribers),
        ("orphan_active_jobs", stats.orphan_active_jobs),
        ("peak_active", stats.peak_active),
    ] {
        profiler.set_gauge(format!("admission/{name}"), count as f64);
    }
    for (name, count) in [
        ("dispatched_total", stats.dispatched_total),
        ("completed_total", stats.completed_total),
        ("failed_total", stats.failed_total),
        ("canceled_total", stats.canceled_total),
    ] {
        profiler.set_gauge(format!("admission/{name}"), count as f64);
    }
    profiler.set_gauge(
        "admission/cleanup_waiting_jobs",
        cleanup_waiting.len() as f64,
    );
    profiler.set_gauge("admission/configured_job_limit", cap as f64);
    profiler.record_elapsed("streaming/admit_scenes", started);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        asset::{AssetApp, AssetLoader, AssetPlugin, LoadContext, io::Reader},
        world_serialization::WorldSerializationPlugin,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[derive(TypePath)]
    struct TestSceneLoader {
        gate: Arc<AtomicBool>,
        dependency: bool,
    }
    impl AssetLoader for TestSceneLoader {
        type Asset = WorldAsset;
        type Settings = ();
        type Error = std::io::Error;
        async fn load(
            &self,
            _: &mut dyn Reader,
            _: &(),
            context: &mut LoadContext<'_>,
        ) -> Result<WorldAsset, std::io::Error> {
            let deadline = Instant::now() + std::time::Duration::from_secs(2);
            while !self.gate.load(Ordering::Acquire) {
                if Instant::now() > deadline {
                    return Err(std::io::Error::other("test gate timed out"));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let mut labeled = context.begin_labeled_asset();
            let mut scene = World::new();
            if self.dependency {
                scene.spawn(Mesh3d(labeled.load("dependency.mesh")));
            }
            context.add_loaded_labeled_asset("Scene0", labeled.finish(WorldAsset::new(scene)));
            Ok(WorldAsset::new(World::new()))
        }
        fn extensions(&self) -> &[&str] {
            &["glb"]
        }
    }

    #[derive(TypePath)]
    struct TestMeshLoader {
        broken: Arc<AtomicBool>,
    }
    impl AssetLoader for TestMeshLoader {
        type Asset = Mesh;
        type Settings = ();
        type Error = std::io::Error;
        async fn load(
            &self,
            _: &mut dyn Reader,
            _: &(),
            _: &mut LoadContext<'_>,
        ) -> Result<Mesh, std::io::Error> {
            if self.broken.load(Ordering::Acquire) {
                return Err(std::io::Error::other("dependency deliberately broken"));
            }
            Ok(Cuboid::default().mesh().build())
        }
        fn extensions(&self) -> &[&str] {
            &["mesh"]
        }
    }

    fn fixture(
        gated: bool,
        dependency: bool,
    ) -> (App, tempfile::TempDir, Arc<AtomicBool>, Arc<AtomicBool>) {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.glb", "b.glb", "dependency.mesh"] {
            std::fs::write(dir.path().join(name), []).unwrap();
        }
        let gate = Arc::new(AtomicBool::new(!gated));
        let broken = Arc::new(AtomicBool::new(dependency));
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin {
                file_path: dir.path().to_string_lossy().into_owned(),
                ..default()
            },
            WorldSerializationPlugin,
        ))
        .init_asset::<Mesh>()
        .register_asset_loader(TestSceneLoader {
            gate: gate.clone(),
            dependency,
        })
        .register_asset_loader(TestMeshLoader {
            broken: broken.clone(),
        })
        .register_type::<Mesh3d>()
        .insert_resource(EngineConfig {
            assets_dir: dir.path().into(),
            max_scene_loads: 1,
            ..default()
        })
        .init_resource::<StreamingWorld>()
        .init_resource::<SceneAdmission>()
        .init_resource::<SceneSchedulingState>()
        .init_resource::<ProfilingState>()
        .add_systems(Update, dispatch_scene_requests);
        (app, dir, gate, broken)
    }

    fn spawn_request(app: &mut App, path: &str, sequence: u64) -> Entity {
        app.world_mut()
            .spawn(SceneRequest {
                path: path.into(),
                cell: None,
                coarse_terrain: false,
                retry_cleanup: None,
                content_identity: None,
                sequence,
                center: DVec3::ZERO,
                radius: None,
                collision_candidate: false,
                handle: None,
                started: Instant::now(),
                allow_retry: false,
            })
            .id()
    }

    fn pump_until(app: &mut App, mut done: impl FnMut(&World) -> bool) {
        for _ in 0..1000 {
            app.update();
            assert!(app.world().resource::<SceneAdmission>().active_jobs() <= 1);
            if done(app.world()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("asset fixture did not settle within bounded updates");
    }

    #[test]
    fn actual_loader_fanout_and_orphan_loading_keep_unique_cap() {
        let (mut app, _dir, gate, _) = fixture(true, false);
        let first = spawn_request(&mut app, "a.glb", 1);
        let shared = spawn_request(&mut app, "a.glb", 2);
        let next = spawn_request(&mut app, "b.glb", 3);
        app.update();
        let first_id = app
            .world()
            .get::<SceneRequest>(first)
            .unwrap()
            .handle
            .as_ref()
            .unwrap()
            .id();
        assert_eq!(
            app.world()
                .get::<SceneRequest>(shared)
                .unwrap()
                .handle
                .as_ref()
                .unwrap()
                .id(),
            first_id
        );
        assert!(
            app.world()
                .get::<SceneRequest>(next)
                .unwrap()
                .handle
                .is_none()
        );
        app.world_mut().despawn(first);
        app.world_mut().despawn(shared);
        app.update();
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .orphan_active_jobs,
            1
        );
        assert!(
            app.world()
                .get::<SceneRequest>(next)
                .unwrap()
                .handle
                .is_none()
        );
        gate.store(true, Ordering::Release);
        pump_until(&mut app, |world| {
            world.get::<SceneRequest>(next).unwrap().handle.is_some()
        });
        pump_until(&mut app, |world| {
            world.resource::<SceneAdmission>().active_jobs() == 0
        });
    }

    #[test]
    fn retiring_cell_waits_for_revival_before_dispatch() {
        let (mut app, _dir, _, _) = fixture(false, false);
        let entity = spawn_request(&mut app, "a.glb", 1);
        let cell = crate::world::database::CellKey::Exterior {
            worldspace_id: 1,
            grid_x: 0,
            grid_y: 0,
        };
        app.world_mut()
            .get_mut::<SceneRequest>(entity)
            .unwrap()
            .cell = Some(cell);
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(cell, CellStatus::Retiring { root: entity });
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(entity)
                .unwrap()
                .handle
                .is_none()
        );
        assert_eq!(app.world().resource::<SceneAdmission>().active_jobs(), 0);
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(cell, CellStatus::Resident { root: entity });
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(entity)
                .unwrap()
                .handle
                .is_some()
        );
    }

    #[test]
    fn recursive_failure_retry_waits_for_asset_removal_before_repaired_load() {
        let (mut app, _dir, _, broken) = fixture(false, true);
        let first = spawn_request(&mut app, "a.glb", 1);
        pump_until(&mut app, |world| {
            let request = world.get::<SceneRequest>(first).unwrap();
            request.handle.as_ref().is_some_and(|handle| {
                matches!(
                    world.resource::<AssetServer>().get_load_states(handle.id()),
                    Some((
                        LoadState::Loaded,
                        _,
                        RecursiveDependencyLoadState::Failed(_)
                    ))
                )
            })
        });
        app.update(); // Reconcile the terminal failure into admission.
        let old_id = app
            .world()
            .get::<SceneRequest>(first)
            .unwrap()
            .handle
            .as_ref()
            .unwrap()
            .id();
        app.world_mut().despawn(first);
        broken.store(false, Ordering::Release);
        let retry = spawn_request(&mut app, "a.glb", 2);
        app.world_mut()
            .get_mut::<SceneRequest>(retry)
            .unwrap()
            .allow_retry = true;
        app.update();
        assert_eq!(
            app.world()
                .get::<SceneRequest>(retry)
                .unwrap()
                .retry_cleanup,
            Some(old_id)
        );
        assert!(
            app.world()
                .get::<SceneRequest>(retry)
                .unwrap()
                .handle
                .is_none()
        );
        pump_until(&mut app, |world| {
            world.get::<SceneRequest>(retry).unwrap().handle.is_some()
        });
        assert!(
            app.world()
                .resource::<AssetServer>()
                .get_load_states(old_id)
                .is_none()
        );
        pump_until(&mut app, |world| {
            world
                .get::<SceneRequest>(retry)
                .unwrap()
                .handle
                .as_ref()
                .is_some_and(|handle| {
                    world
                        .resource::<AssetServer>()
                        .is_loaded_with_dependencies(handle.id())
                })
        });
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .failed_total,
            1
        );
    }

    #[test]
    fn reference_bounds_stay_absolute_across_negative_grid_and_rebase() {
        let position = WorldPosition::from_creation_units(Vec3::new(-8100.0, -20500.0, 50.0));
        let bound = |origin| {
            let transform = Transform::from_translation(super::super::creation_to_bevy(
                position.relative_to(origin),
            ));
            let bounds = InstanceBounds::transformed(
                Vec3::splat(-10.0),
                Vec3::splat(10.0),
                transform.to_matrix(),
            );
            reference_geometry(position, &transform, Some(bounds))
        };
        assert_eq!(bound(IVec2::ZERO), bound(IVec2::new(-2, -5)));
        assert_eq!(bound(IVec2::ZERO).0, DVec3::new(-8100.0, 50.0, 20500.0));
    }
}
