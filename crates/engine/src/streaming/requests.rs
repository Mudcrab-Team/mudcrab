//! Opt-in admission for full-detail and LOD scenes in one immutable asset pack.

use super::{
    CellStatus, PendingModel, RenderOrigin, StreamingWorld,
    admission::{SceneAdmission, SceneDemand, SceneJobStatus, SceneKey},
    priority::{self, DemandPriority, PriorityView},
    runtime::StreamingRuntime,
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
    config.prioritize_streaming
        || config.max_scene_loads != 0
        || config.streaming_controls_enabled()
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
pub(super) struct SceneRequestKey(pub SceneKey);

#[derive(Resource)]
pub(super) struct SceneSchedulingState {
    pack_identity: String,
    reconciled_subscribers: std::collections::HashSet<Entity>,
    pub dispatch_choices: u64,
    pub activation_choices: u64,
    reservation_retries: BTreeMap<SceneKey, u64>,
    reservation_attempts: u64,
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
            reconciled_subscribers: default(),
            pack_identity: format!(
                "{}#{}",
                root.display(),
                fingerprint.as_deref().unwrap_or("unmanifested")
            ),
            dispatch_choices: 0,
            activation_choices: 0,
            reservation_retries: default(),
            reservation_attempts: 0,
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
    let configured_cap = config.max_scene_loads;
    let prioritize = config.prioritize_streaming;
    let physics = config.interactive_world_physics();
    let control = world
        .get_resource::<StreamingRuntime>()
        .and_then(|runtime| runtime.decision);
    let cap = control.map_or(configured_cap, |decision| decision.budgets.max_scene_jobs);
    let ordinary_intake = control.is_none_or(|decision| decision.allow_scene_intake && cap != 0);
    let mandatory_jobs = control.map_or(0, |decision| decision.mandatory_scene_jobs);
    let select_new_jobs = ordinary_intake || mandatory_jobs != 0;
    let admission_micros = control.map(|decision| decision.budgets.max_admission_micros);
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
    let mut live_requests = world.query::<(Entity, &SceneRequest)>();
    let mut all_assigned = true;
    let live: std::collections::HashSet<_> = live_requests
        .iter(world)
        .map(|(entity, request)| {
            all_assigned &= request.handle.is_some();
            entity
        })
        .collect();
    let admission = world.resource::<SceneAdmission>();
    if keys.is_empty()
        && all_assigned
        && admission.active_jobs() == 0
        && admission.queued_jobs() == 0
        && live
            == world
                .resource::<SceneSchedulingState>()
                .reconciled_subscribers
    {
        // Immutable-pack terminal records need no polling. Compare exact owners so an
        // unload/replacement still reconciles; unassigned, orphan or retry work never skips.
        world
            .resource_mut::<ProfilingState>()
            .record_elapsed("streaming/admit_scenes", started);
        return;
    }
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
    let view = if prioritize || mandatory_jobs != 0 {
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
    let mut placement_costs: BTreeMap<SceneKey, Vec<(Entity, bool)>> = BTreeMap::new();
    for demand in &demands {
        if !select_new_jobs {
            break;
        }
        let request = world.get::<SceneRequest>(demand.subscriber).unwrap();
        if request.handle.is_some() {
            continue;
        }
        placement_costs
            .entry(demand.key.clone())
            .or_default()
            .push((demand.subscriber, physics && request.collision_candidate));
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
    // These handles are already owned by the loader. Adoption accounts for them
    // even when a newly enabled reservation cannot fit the ordinary allowance.
    let tracked = world.resource::<SceneAdmission>().tracked_jobs();
    if let Some(mut runtime) = world.get_resource_mut::<StreamingRuntime>() {
        for (key, id) in tracked {
            runtime.adopt_scene(&key, id);
        }
    }
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
    // Paused intake still reconciles ownership, polls jobs, and fans out handles.
    // It does not need to rank the entire unassigned queue every frame.
    let queued: Vec<_> = if select_new_jobs {
        world
            .resource::<SceneAdmission>()
            .queued_keys(0)
            .into_iter()
            .filter(|key| !cleanup_waiting.contains(key))
            .filter_map(|key| {
                priority::shared_scene_priority(priorities.remove(&key)?)
                    .map(|priority| (key, priority))
            })
            .filter(|(_, rank)| {
                ordinary_intake || (mandatory_jobs != 0 && rank.is_protected_collision())
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut choices = world.resource::<SceneSchedulingState>().dispatch_choices;
    let slots = if ordinary_intake {
        // Controlled zero means paused; legacy zero remains unlimited only when
        // no controller decision exists.
        world.resource::<SceneAdmission>().available_slots(cap)
    } else {
        let hard_cap = if configured_cap == 0 {
            128
        } else {
            configured_cap
        };
        mandatory_jobs
            .min(hard_cap.saturating_sub(world.resource::<SceneAdmission>().active_jobs()))
    }
    .min(queued.len());
    let mut normal: Vec<_> = (0..queued.len()).collect();
    let mut aged = normal.clone();
    let mut retries = std::mem::take(
        &mut world
            .resource_mut::<SceneSchedulingState>()
            .reservation_retries,
    );
    let queued_keys: std::collections::HashSet<_> = queued.iter().map(|(key, _)| key).collect();
    if select_new_jobs {
        retries.retain(|key, _| queued_keys.contains(key));
    }
    // Denied candidates rotate behind work not yet attempted, preventing an
    // unaffordable prefix from consuming every bounded scan forever.
    normal.sort_unstable_by(|&left, &right| {
        retries
            .get(&queued[left].0)
            .copied()
            .unwrap_or(0)
            .cmp(&retries.get(&queued[right].0).copied().unwrap_or(0))
            .then_with(|| {
                if prioritize {
                    queued[left].1.compare(&queued[right].1)
                } else {
                    left.cmp(&right)
                }
            })
            .then_with(|| left.cmp(&right))
    });
    aged.sort_unstable_by(|&left, &right| {
        retries
            .get(&queued[left].0)
            .copied()
            .unwrap_or(0)
            .cmp(&retries.get(&queued[right].0).copied().unwrap_or(0))
            .then_with(|| queued[left].1.compare_age(&queued[right].1))
            .then_with(|| left.cmp(&right))
    });
    let mut normal_cursor = 0;
    let mut aged_cursor = 0;
    let mut used = vec![false; queued.len()];
    let mut attempts = 0;
    let mut dispatched = 0;
    let mut retry_sequence = world
        .resource::<SceneSchedulingState>()
        .reservation_attempts;
    // Reconciliation and ordering are accounted for in the full frame span.
    // Give dispatch its own allowance so a large owner set cannot consume it
    // before the first candidate, reducing every burst to one load per frame.
    let dispatch_started = Instant::now();
    while dispatched < slots && attempts < queued.len() {
        let (order, cursor) = if prioritize
            && choices % priority::AGED_SERVICE_INTERVAL == priority::AGED_SERVICE_INTERVAL - 1
        {
            (&aged, &mut aged_cursor)
        } else {
            (&normal, &mut normal_cursor)
        };
        while *cursor < order.len() && used[order[*cursor]] {
            *cursor += 1;
        }
        if *cursor == order.len() {
            break;
        }
        let index = order[*cursor];
        *cursor += 1;
        used[index] = true;
        attempts += 1;
        let key = &queued[index].0;
        let reserved = world
            .get_resource_mut::<StreamingRuntime>()
            .is_none_or(|mut runtime| {
                runtime.reserve_scene_with_placements(key, &placement_costs[key])
            });
        if !reserved {
            retry_sequence = retry_sequence.saturating_add(1);
            retries.insert(key.clone(), retry_sequence);
        } else {
            let handle =
                server.load(GltfAssetLabel::Scene(0).from_asset(key.canonical_path.clone()));
            if let Some(mut runtime) = world.get_resource_mut::<StreamingRuntime>() {
                runtime.bind_scene(key, handle.id());
            }
            if world
                .resource_mut::<SceneAdmission>()
                .dispatch(key, handle.clone())
            {
                dispatched += 1;
                retries.remove(key);
                choices = choices.saturating_add(1);
                let mut profiler = world.resource_mut::<ProfilingState>();
                profiler.observe_scene(handle.id(), &server);
                profiler.event(&key.canonical_path, "scene_dispatched", None);
            }
        }
        if control.is_some()
            && (attempts >= 128
                || admission_micros.is_some_and(|micros| {
                    dispatch_started.elapsed().as_micros() >= u128::from(micros)
                }))
        {
            // At least one candidate gets a chance after reconciliation. A
            // refused reservation is never converted into a loader request.
            break;
        }
    }
    {
        let mut scheduling = world.resource_mut::<SceneSchedulingState>();
        scheduling.dispatch_choices = choices;
        scheduling.reservation_retries = retries;
        scheduling.reservation_attempts = retry_sequence;
    }
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
        let collision = physics
            && world
                .get::<SceneRequest>(demand.subscriber)
                .unwrap()
                .collision_candidate;
        if !world
            .get_resource_mut::<StreamingRuntime>()
            .is_none_or(|mut runtime| {
                runtime.reserve_scene_placement(&demand.key, demand.subscriber, collision)
            })
        {
            continue;
        }
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
    world
        .resource_mut::<SceneSchedulingState>()
        .reconciled_subscribers = live;
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
    profiler.set_gauge("admission/configured_job_limit", configured_cap as f64);
    if control.is_some() {
        profiler.set_gauge("admission/effective_job_limit", cap as f64);
        profiler.set_gauge("admission/reservation_attempts_this_frame", attempts as f64);
    }
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

    struct TestLoaderGate(Arc<AtomicBool>);

    impl std::ops::Deref for TestLoaderGate {
        type Target = AtomicBool;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl Drop for TestLoaderGate {
        fn drop(&mut self) {
            // A fixture must not leave a closed gate consuming the shared IO
            // pool after its App is dropped or an assertion unwinds.
            self.0.store(true, Ordering::Release);
        }
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
                bevy::tasks::futures_lite::future::yield_now().await;
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
    ) -> (App, tempfile::TempDir, TestLoaderGate, Arc<AtomicBool>) {
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
        (app, dir, TestLoaderGate(gate), broken)
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

    fn attach_control(app: &mut App, paused: bool, collision: bool, memory_mib: usize) {
        use super::super::control::{
            ControllerInput, ControllerSettings, ControllerState, DownstreamBacklog, StageBudgets,
        };
        {
            let mut config = app.world_mut().resource_mut::<EngineConfig>();
            config.max_streaming_backlog = 256;
            config.streaming_memory_mib = memory_mib;
        }
        app.init_resource::<StreamingRuntime>();
        let decision = ControllerState::default().tick(
            &ControllerSettings::default(),
            ControllerInput {
                delta_seconds: 0.05,
                frame_ms: Some(8.0),
                gpu_frame_ms: None,
                streaming_work_ms: 0.5,
                camera_moving: false,
                camera_turning: false,
                camera_discontinuity: false,
                useful_nearby_ready: false,
                demand_settled: false,
                backlog: DownstreamBacklog {
                    ready_placements: if paused { 256 } else { 0 },
                    ..default()
                },
                active_scene_jobs: app.world().resource::<SceneAdmission>().active_jobs(),
                hard_limits: StageBudgets {
                    max_scene_jobs: 1,
                    ..default()
                },
                memory_blocked: false,
                mandatory_collision_pending: collision,
            },
        );
        app.world_mut().resource_mut::<StreamingRuntime>().decision = Some(decision);
    }

    fn write_cost_catalog(app: &mut App, dir: &tempfile::TempDir, scenes: &[(&str, u64, u64)]) {
        use shared::streaming_costs::{
            ByteEstimate, ResourceCost, ResourceKind, SceneCost, StreamingCostCatalog,
        };
        let manifest = b"{}";
        std::fs::write(dir.path().join("conversion-manifest.json"), manifest).unwrap();
        let mut catalog = StreamingCostCatalog::empty(format!("{:x}", Sha256::digest(manifest)));
        for &(path, geometry_bytes, placement_bytes) in scenes {
            catalog.resources.insert(
                path.into(),
                ResourceCost::new(
                    ResourceKind::SceneGeometry,
                    ByteEstimate::conservative(geometry_bytes, 0, "fixture geometry"),
                ),
            );
            catalog.scenes.insert(
                path.into(),
                SceneCost {
                    resource_keys: vec![path.into()],
                    per_placement_collision: ByteEstimate::conservative(
                        0,
                        0,
                        "fixture collision absent",
                    ),
                    per_placement_ecs: ByteEstimate::conservative(
                        placement_bytes,
                        0,
                        "fixture placement",
                    ),
                },
            );
        }
        std::fs::write(
            dir.path().join("streaming-costs.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        let scheduling = SceneSchedulingState::from_world(app.world_mut());
        app.insert_resource(scheduling);
    }

    #[test]
    fn scene_that_fits_without_placement_space_never_starts_loading() {
        let (mut app, dir, _, _) = fixture(true, false);
        write_cost_catalog(&mut app, &dir, &[("a.glb", 1024 * 1024, 1024)]);
        let request = spawn_request(&mut app, "a.glb", 1);
        attach_control(&mut app, false, false, 1);
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(request)
                .unwrap()
                .handle
                .is_none()
        );
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .dispatched_total,
            0
        );
        assert!(
            app.world()
                .resource::<AssetServer>()
                .get_path_ids("a.glb")
                .is_empty()
        );
    }

    #[test]
    fn shared_scene_reserves_the_cost_of_every_placement_before_loading() {
        let (mut app, dir, _, _) = fixture(true, false);
        write_cost_catalog(&mut app, &dir, &[("a.glb", 512 * 1024, 256 * 1024)]);
        let first = spawn_request(&mut app, "a.glb", 1);
        let second = spawn_request(&mut app, "a.glb", 2);
        let third = spawn_request(&mut app, "a.glb", 3);
        attach_control(&mut app, false, false, 1);
        app.update();
        for entity in [first, second, third] {
            assert!(
                app.world()
                    .get::<SceneRequest>(entity)
                    .unwrap()
                    .handle
                    .is_none()
            );
        }
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .dispatched_total,
            0
        );
        app.world_mut().despawn(third);
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
                .get::<SceneRequest>(second)
                .unwrap()
                .handle
                .as_ref()
                .unwrap()
                .id(),
            first_id
        );
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .dispatched_total,
            1
        );
    }

    #[test]
    fn late_subscriber_without_placement_space_does_not_receive_a_shared_handle() {
        let (mut app, dir, _, _) = fixture(true, false);
        write_cost_catalog(&mut app, &dir, &[("a.glb", 768 * 1024, 256 * 1024)]);
        let first = spawn_request(&mut app, "a.glb", 1);
        attach_control(&mut app, false, false, 1);
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(first)
                .unwrap()
                .handle
                .is_some()
        );
        let late = spawn_request(&mut app, "a.glb", 2);
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(late)
                .unwrap()
                .handle
                .is_none()
        );
        assert!(app.world().get::<PendingModel>(late).is_none());
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .dispatched_total,
            1
        );
    }

    #[test]
    fn paused_zero_never_becomes_unlimited_loader_intake() {
        let (mut app, _dir, _, _) = fixture(true, false);
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .max_scene_loads = 0;
        let first = spawn_request(&mut app, "a.glb", 1);
        let second = spawn_request(&mut app, "b.glb", 2);
        attach_control(&mut app, true, false, 0);
        app.update();
        let stats = app.world().resource::<SceneAdmission>().stats();
        assert_eq!(stats.queued_jobs, 2);
        assert_eq!(stats.active_jobs, 0);
        assert_eq!(stats.dispatched_total, 0);
        for entity in [first, second] {
            assert!(
                app.world()
                    .get::<SceneRequest>(entity)
                    .unwrap()
                    .handle
                    .is_none()
            );
        }
        assert!(
            app.world()
                .resource::<AssetServer>()
                .get_path_ids("a.glb")
                .is_empty()
        );
        assert!(
            app.world()
                .resource::<AssetServer>()
                .get_path_ids("b.glb")
                .is_empty()
        );
    }

    #[test]
    fn controlled_finite_cap_overrides_a_legacy_unlimited_configuration() {
        let (mut app, _dir, _, _) = fixture(true, false);
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .max_scene_loads = 0;
        let first = spawn_request(&mut app, "a.glb", 1);
        let second = spawn_request(&mut app, "b.glb", 2);
        attach_control(&mut app, false, false, 0);
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(first)
                .unwrap()
                .handle
                .is_some()
        );
        assert!(
            app.world()
                .get::<SceneRequest>(second)
                .unwrap()
                .handle
                .is_none()
        );
        assert_eq!(app.world().resource::<SceneAdmission>().active_jobs(), 1);
    }

    #[test]
    fn paused_intake_still_polls_adopted_orphan_jobs_until_completion() {
        let (mut app, _dir, gate, _) = fixture(true, false);
        let first = spawn_request(&mut app, "a.glb", 1);
        let second = spawn_request(&mut app, "b.glb", 2);
        app.update();
        assert_eq!(app.world().resource::<SceneAdmission>().active_jobs(), 1);
        attach_control(&mut app, true, false, 0);
        app.world_mut().despawn(first);
        app.update();
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .orphan_active_jobs,
            1
        );
        gate.store(true, Ordering::Release);
        pump_until(&mut app, |world| {
            world.resource::<SceneAdmission>().active_jobs() == 0
        });
        assert!(
            app.world()
                .get::<SceneRequest>(second)
                .unwrap()
                .handle
                .is_none()
        );
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .dispatched_total,
            1
        );
    }

    #[test]
    fn refused_memory_reservations_never_trigger_asset_loads_or_fairness_choices() {
        let (mut app, _dir, _, _) = fixture(true, false);
        let first = spawn_request(&mut app, "a.glb", 1);
        attach_control(&mut app, false, false, 1);
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(first)
                .unwrap()
                .handle
                .is_none()
        );
        assert!(
            app.world()
                .resource::<AssetServer>()
                .get_path_ids("a.glb")
                .is_empty()
        );
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .dispatched_total,
            0
        );
        assert_eq!(
            app.world()
                .resource::<SceneSchedulingState>()
                .dispatch_choices,
            0
        );
    }

    #[test]
    fn paused_collision_service_loads_only_actual_nearby_collision_demand() {
        let (mut app, _dir, _, _) = fixture(true, false);
        app.world_mut()
            .spawn((StreamingCamera, Transform::default()));
        let ordinary = spawn_request(&mut app, "a.glb", 1);
        let protected = spawn_request(&mut app, "b.glb", 2);
        {
            let mut request = app.world_mut().get_mut::<SceneRequest>(protected).unwrap();
            request.collision_candidate = true;
            // Behind the camera, but inside the collision protection distance.
            request.center = DVec3::new(0.0, 0.0, 100.0);
        }
        attach_control(&mut app, true, true, 0);
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(ordinary)
                .unwrap()
                .handle
                .is_none()
        );
        assert!(
            app.world()
                .get::<SceneRequest>(protected)
                .unwrap()
                .handle
                .is_some()
        );
        assert_eq!(
            app.world()
                .resource::<SceneAdmission>()
                .stats()
                .dispatched_total,
            1
        );
    }

    #[test]
    fn bounded_reservation_scan_rotates_past_unaffordable_jobs() {
        use shared::streaming_costs::{
            ByteEstimate, ResourceCost, ResourceKind, SceneCost, StreamingCostCatalog,
        };
        let (mut app, dir, _, _) = fixture(true, false);
        let manifest = b"{}";
        std::fs::write(dir.path().join("conversion-manifest.json"), manifest).unwrap();
        let mut catalog = StreamingCostCatalog::empty(format!("{:x}", Sha256::digest(manifest)));
        for (path, bytes) in [("a.glb", 2 * 1024 * 1024), ("b.glb", 1024)] {
            catalog.resources.insert(
                path.into(),
                ResourceCost::new(
                    ResourceKind::SceneGeometry,
                    ByteEstimate::conservative(bytes, 0, "fixture allocation"),
                ),
            );
            catalog.scenes.insert(
                path.into(),
                SceneCost {
                    resource_keys: vec![path.into()],
                    per_placement_collision: ByteEstimate::conservative(
                        0,
                        0,
                        "fixture collision absent",
                    ),
                    per_placement_ecs: ByteEstimate::conservative(1024, 0, "fixture placement"),
                },
            );
        }
        std::fs::write(
            dir.path().join("streaming-costs.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        let expensive = spawn_request(&mut app, "a.glb", 1);
        let affordable = spawn_request(&mut app, "b.glb", 2);
        attach_control(&mut app, false, false, 1);
        // A zero time allowance still lets one candidate attempt make progress.
        app.world_mut()
            .resource_mut::<StreamingRuntime>()
            .decision
            .as_mut()
            .unwrap()
            .budgets
            .max_admission_micros = 0;
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(expensive)
                .unwrap()
                .handle
                .is_none()
        );
        assert!(
            app.world()
                .get::<SceneRequest>(affordable)
                .unwrap()
                .handle
                .is_none()
        );
        app.update();
        assert!(
            app.world()
                .get::<SceneRequest>(expensive)
                .unwrap()
                .handle
                .is_none()
        );
        assert!(
            app.world()
                .get::<SceneRequest>(affordable)
                .unwrap()
                .handle
                .is_some()
        );
        assert_eq!(
            app.world()
                .resource::<SceneSchedulingState>()
                .dispatch_choices,
            1
        );
        assert!(
            app.world()
                .resource::<AssetServer>()
                .get_path_ids("a.glb")
                .is_empty()
        );
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
    fn settled_admission_still_releases_removed_owners_and_serves_new_demand() {
        let (mut app, _dir, _, _) = fixture(false, false);
        let first = spawn_request(&mut app, "a.glb", 1);
        pump_until(&mut app, |world| {
            world.resource::<SceneAdmission>().stats().completed_total == 1
        });
        app.update(); // Same owners and terminal jobs use the settled path.
        assert_eq!(app.world().resource::<SceneAdmission>().stats().records, 1);
        app.world_mut().despawn(first);
        app.update();
        let stats = app.world().resource::<SceneAdmission>().stats();
        assert_eq!(stats.records, 0);
        assert_eq!(stats.subscribers, 0);
        let replacement = spawn_request(&mut app, "b.glb", 2);
        pump_until(&mut app, |world| {
            world.resource::<SceneAdmission>().stats().completed_total == 2
        });
        assert!(
            app.world()
                .get::<SceneRequest>(replacement)
                .unwrap()
                .handle
                .is_some()
        );
        assert_eq!(
            app.world().resource::<SceneAdmission>().stats().subscribers,
            1
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
