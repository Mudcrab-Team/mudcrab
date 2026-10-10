use super::*;
use crate::{
    streaming::{RenderOrigin, StaticCollisionCache, admission::SceneDemand},
    streaming_preparation::StreamingPreparationPlugin,
    world::{
        components::{StreamingCamera, TerrainPatch},
        database::{CellPayload, DatabaseRequest},
    },
};
use bevy::asset::{AssetApp, AssetPlugin};
use shared::streaming_costs::{ResourceCost, SceneCost};
use std::time::Instant;

fn fixture(enabled: bool) -> (App, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: directory.path().to_string_lossy().into_owned(),
            ..default()
        },
        StreamingPreparationPlugin,
    ))
    .init_asset::<WorldAsset>()
    .init_asset::<Mesh>()
    .init_asset::<Image>()
    .init_asset::<StandardMaterial>()
    .init_asset::<TerrainMaterial>()
    .init_asset::<WaterMaterial>()
    .insert_resource(EngineConfig {
        assets_dir: directory.path().into(),
        max_streaming_backlog: if enabled { 256 } else { 0 },
        streaming_memory_mib: if enabled { 64 } else { 0 },
        ..default()
    })
    .init_resource::<SceneAdmission>()
    .init_resource::<StreamingWorld>()
    .init_resource::<ActiveSpace>()
    .init_resource::<StreamingMetrics>()
    .init_resource::<StaticCollisionCache>()
    .init_resource::<ProfilingState>()
    .init_resource::<requests::SceneSchedulingState>()
    .init_resource::<StreamingRuntime>();
    (app, directory)
}

fn scene_key(path: &str) -> SceneKey {
    SceneKey {
        canonical_path: path.into(),
        build_identity: Some("test-pack".into()),
        content_identity: None,
    }
}

fn request(path: &str, handle: Option<Handle<WorldAsset>>) -> SceneRequest {
    SceneRequest {
        path: path.into(),
        cell: None,
        coarse_terrain: false,
        retry_cleanup: None,
        content_identity: None,
        sequence: 0,
        center: DVec3::ZERO,
        radius: Some(1.0),
        collision_candidate: false,
        handle,
        started: Instant::now(),
        allow_retry: false,
    }
}

fn known_catalog() -> StreamingCostCatalog {
    let mut catalog = StreamingCostCatalog::empty(String::new());
    for (path, kind, resident, transient) in [
        ("meshes/a.glb", ResourceKind::SceneGeometry, 64, 8),
        ("meshes/b.glb", ResourceKind::SceneGeometry, 128, 16),
        ("textures/shared.ktx2", ResourceKind::Texture, 512, 32),
    ] {
        catalog.resources.insert(
            path.into(),
            ResourceCost::new(
                kind,
                ByteEstimate::conservative(resident, transient, "fixture"),
            ),
        );
    }
    for path in ["meshes/a.glb", "meshes/b.glb"] {
        catalog.scenes.insert(
            path.into(),
            SceneCost {
                resource_keys: vec![path.into(), "textures/shared.ktx2".into()],
                per_placement_ecs: ByteEstimate::conservative(100, 20, "fixture ECS"),
                per_placement_collision: ByteEstimate::conservative(300, 40, "fixture collider"),
            },
        );
    }
    catalog
}

fn cell_key(x: i32) -> CellKey {
    CellKey::Exterior {
        worldspace_id: 0x3c,
        grid_x: x,
        grid_y: 0,
    }
}

fn observe_at(app: &mut App, frame: u64) {
    app.world_mut().resource_mut::<StreamingRuntime>().frame = frame;
    observe_streaming_ownership(app.world_mut());
}

fn cell_pipeline_fixture(
    capacity: usize,
) -> (
    App,
    tempfile::TempDir,
    crossbeam_channel::Receiver<DatabaseRequest>,
    crossbeam_channel::Sender<DatabaseResponse>,
) {
    use crate::{
        render::WaterReflectionTexture,
        streaming::{StreamingCommitBudget, TerrainContinuity},
        world::{cache::CellCache, database::AssetCatalog},
    };
    let (mut app, directory) = fixture(true);
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.headless = true;
        config.stream_radius = 0;
        config.unload_radius = 0;
        config.max_streaming_backlog = 2;
    }
    let path = directory.path().join("cell-catalog.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT);
             CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER);
             CREATE TABLE waters(id INTEGER PRIMARY KEY,flow_normal_path TEXT);",
        )
        .unwrap();
    drop(connection);
    let cache_path = directory.path().join("cells.rkyv");
    std::fs::write(
        &cache_path,
        rkyv::to_bytes::<rkyv::rancor::Error>(&shared::CellCache {
            version: shared::CELL_CACHE_VERSION,
            cells: vec![],
        })
        .unwrap(),
    )
    .unwrap();
    let (database, requests, responses) = WorldDatabase::channel_fixture(capacity);
    app.insert_resource(database)
        .insert_resource(AssetCatalog::open(&path).unwrap())
        .insert_resource(CellCache::open(&cache_path).unwrap())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(WaterReflectionTexture(Handle::default()))
        .init_resource::<StreamingCommitBudget>()
        .init_resource::<TerrainContinuity>();
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    (app, directory, requests, responses)
}

fn returned_cell(generation: u64, key: CellKey) -> DatabaseResponse {
    DatabaseResponse {
        generation,
        key,
        result: Ok(CellPayload {
            generation,
            key,
            cell_id: 1,
            references: vec![],
        }),
        query_micros: 0,
        queue_wait_micros: 0,
        total_request_micros: 0,
        row_count: 0,
    }
}

#[test]
fn raw_cell_payloads_enter_backpressure_and_drain_without_false_settlement() {
    let (mut app, _directory) = fixture(true);
    let (database, _requests, responses) = WorldDatabase::channel_fixture(1);
    app.insert_resource(database);
    for x in 0..16 {
        responses.send(returned_cell(1, cell_key(x))).unwrap();
    }
    app.world_mut()
        .resource_mut::<StreamingRuntime>()
        .deferred_responses
        .push_back(returned_cell(2, cell_key(16)));
    update_streaming_control(app.world_mut());
    let snapshot = app
        .world()
        .resource::<StreamingRuntime>()
        .snapshot
        .as_ref()
        .unwrap();
    assert_eq!(snapshot.backlog.cell_responses, 17);
    assert!(snapshot.control.reasons.queue_pressure);
    assert!(!snapshot.control.reasons.demand_settled);
    assert!(!snapshot.control.allow_cell_requests);
    let database = app.world().resource::<WorldDatabase>();
    while database.try_response().is_some() {}
    app.world_mut()
        .resource_mut::<StreamingRuntime>()
        .deferred_responses
        .clear();
    update_streaming_control(app.world_mut());
    let snapshot = app
        .world()
        .resource::<StreamingRuntime>()
        .snapshot
        .as_ref()
        .unwrap();
    assert_eq!(snapshot.backlog.cell_responses, 0);
    assert!(!snapshot.control.reasons.queue_pressure);
    assert!(snapshot.control.reasons.recovery_hold);
    assert!(!snapshot.control.allow_cell_requests);
    // Recovery intentionally waits for a stable low-watermark interval.
    for _ in 0..12 {
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_millis(50));
        update_streaming_control(app.world_mut());
    }
    let snapshot = app
        .world()
        .resource::<StreamingRuntime>()
        .snapshot
        .as_ref()
        .unwrap();
    assert!(!snapshot.control.reasons.recovery_hold);
    assert!(snapshot.control.allow_cell_requests);
}

#[test]
fn rejected_cell_enqueue_stays_retryable_and_its_unused_reservation_is_reclaimed() {
    use bevy::ecs::system::RunSystemOnce;
    let (mut app, _directory, requests, _responses) = cell_pipeline_fixture(1);
    app.world()
        .resource::<WorldDatabase>()
        .try_request(DatabaseRequest::Load {
            generation: 99,
            key: CellKey::Interior(99),
            queued_at: Instant::now(),
        })
        .unwrap();
    app.world_mut()
        .run_system_once(super::super::plan_cells)
        .unwrap();
    let streaming = app.world().resource::<StreamingWorld>();
    assert!(streaming.cells.is_empty());
    assert!(streaming.outstanding_cells.is_empty());
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        0
    );
    let reserved = app.world().resource::<StreamingRuntime>().ledger.totals();
    assert!(reserved.resident_bytes > 0);
    observe_at(&mut app, 1);
    assert!(app.world().resource::<StreamingRuntime>().cells.is_empty());
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .orphan_bytes()
            .unwrap()
            > 0
    );
    observe_at(&mut app, 3);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .peak_bytes(),
        Some(0)
    );
    requests.try_recv().unwrap();
    app.world_mut()
        .run_system_once(super::super::plan_cells)
        .unwrap();
    let streaming = app.world().resource::<StreamingWorld>();
    assert!(matches!(
        streaming.cells[&cell_key(0)],
        CellStatus::Loading { .. }
    ));
    assert_eq!(streaming.outstanding_cells.len(), 1);
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        1
    );
}

#[test]
fn stale_cell_payloads_retain_reservations_until_consumed_and_do_not_block_reload() {
    use crate::streaming::{StreamingCommitBudget, collect_cells, plan_cells};
    use bevy::ecs::system::RunSystemOnce;
    let (mut app, _directory, requests, responses) = cell_pipeline_fixture(4);
    app.world_mut().run_system_once(plan_cells).unwrap();
    let DatabaseRequest::Load {
        generation: old_generation,
        key: old_key,
        ..
    } = requests.try_recv().unwrap()
    else {
        panic!("expected a cell request");
    };
    let camera = {
        let world = app.world_mut();
        world
            .query_filtered::<Entity, With<StreamingCamera>>()
            .single(world)
            .unwrap()
    };
    app.world_mut()
        .get_mut::<Transform>(camera)
        .unwrap()
        .translation
        .x = CELL_SIZE * 100.0;
    app.world_mut().run_system_once(plan_cells).unwrap();
    let DatabaseRequest::Load {
        generation: far_generation,
        key: far_key,
        ..
    } = requests.try_recv().unwrap()
    else {
        panic!("expected a second cell request");
    };
    app.world_mut()
        .get_mut::<Transform>(camera)
        .unwrap()
        .translation = Vec3::ZERO;
    app.world_mut().run_system_once(plan_cells).unwrap();
    assert!(
        requests.is_empty(),
        "same-key reload must wait for its old payload"
    );
    assert!(app.world().resource::<StreamingWorld>().cells.is_empty());
    assert_eq!(
        app.world()
            .resource::<StreamingWorld>()
            .outstanding_cells
            .len(),
        2
    );
    let reserved = app.world().resource::<StreamingRuntime>().ledger.totals();
    observe_at(&mut app, 1);
    observe_at(&mut app, 4);
    assert_eq!(
        app.world().resource::<StreamingRuntime>().ledger.totals(),
        reserved
    );
    assert_eq!(app.world().resource::<StreamingRuntime>().cells.len(), 2);
    responses
        .send(returned_cell(old_generation, old_key))
        .unwrap();
    responses
        .send(returned_cell(far_generation, far_key))
        .unwrap();
    app.world_mut().run_system_once(collect_cells).unwrap();
    assert!(
        app.world()
            .resource::<StreamingWorld>()
            .outstanding_cells
            .is_empty()
    );
    assert!(app.world().resource::<StreamingWorld>().cells.is_empty());
    assert_eq!(
        app.world().resource::<StreamingMetrics>().stale_responses,
        2
    );
    observe_at(&mut app, 5);
    assert!(app.world().resource::<StreamingRuntime>().cells.is_empty());
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .peak_bytes(),
        reserved.peak_bytes()
    );
    observe_at(&mut app, 7);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .peak_bytes(),
        Some(0)
    );
    app.world_mut().run_system_once(plan_cells).unwrap();
    let DatabaseRequest::Load {
        generation: fresh_generation,
        key: fresh_key,
        ..
    } = requests.try_recv().unwrap()
    else {
        panic!("expected reload after stale payload disposal");
    };
    assert_eq!(fresh_key, old_key);
    assert_ne!(fresh_generation, old_generation);
    responses
        .send(returned_cell(old_generation, old_key))
        .unwrap();
    responses
        .send(returned_cell(fresh_generation, fresh_key))
        .unwrap();
    app.world_mut()
        .resource_mut::<StreamingCommitBudget>()
        .remaining = 1;
    app.world_mut().run_system_once(collect_cells).unwrap();
    let streaming = app.world().resource::<StreamingWorld>();
    assert!(matches!(
        streaming.cells[&fresh_key],
        CellStatus::Resident { .. }
    ));
    assert!(streaming.outstanding_cells.is_empty());
    assert_eq!(streaming.cells.len(), 1);
    let metrics = app.world().resource::<StreamingMetrics>();
    assert_eq!(metrics.requests_submitted, 3);
    assert_eq!(metrics.responses_received, 4);
    assert_eq!(metrics.stale_responses, 3);
    assert_eq!(metrics.failed_cells, 0);
}

#[test]
fn startup_usefulness_counts_unadmitted_scene_demand() {
    let (mut app, _directory) = fixture(true);
    let key = scene_key("meshes/a.glb");
    {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        runtime.catalog = known_catalog();
        assert!(runtime.reserve_scene(&key));
        runtime.scenes.get_mut(&key).unwrap().prepared = true;
    }
    app.world_mut()
        .resource_mut::<StreamingMetrics>()
        .terrain_patches_validated = 4;
    let demands: Vec<_> = (0..20)
        .map(|index| SceneDemand {
            key: scene_key(&format!("meshes/queued-{index}.glb")),
            subscriber: app.world_mut().spawn_empty().id(),
            existing_handle: None,
            priority: index,
        })
        .collect();
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .reconcile_demands(demands);
    update_streaming_control(app.world_mut());
    assert!(
        !app.world()
            .resource::<StreamingRuntime>()
            .snapshot
            .as_ref()
            .unwrap()
            .control
            .reasons
            .startup_useful
    );
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .reconcile_demands([]);
    app.world_mut()
        .resource_mut::<StreamingMetrics>()
        .pending_lod_chunks = 1;
    update_streaming_control(app.world_mut());
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .snapshot
            .as_ref()
            .unwrap()
            .control
            .reasons
            .startup_useful
    );
}

#[test]
fn unavailable_and_stale_process_memory_are_unknown_while_fresh_pressure_blocks_intake() {
    use bevy::diagnostic::{Diagnostic, DiagnosticMeasurement};
    let (mut app, _directory) = fixture(true);
    app.world_mut().insert_resource(system_info(32));
    let mut diagnostics = DiagnosticsStore::default();
    let mut process = Diagnostic::new(SystemInformationDiagnosticsPlugin::PROCESS_MEM_USAGE);
    process.add_measurement(DiagnosticMeasurement {
        time: Instant::now(),
        value: 0.0,
    });
    let mut system = Diagnostic::new(SystemInformationDiagnosticsPlugin::SYSTEM_MEM_USAGE);
    system.add_measurement(DiagnosticMeasurement {
        time: Instant::now(),
        value: 50.0,
    });
    diagnostics.add(process);
    diagnostics.add(system);
    app.world_mut().insert_resource(diagnostics);
    update_streaming_control(app.world_mut());
    let snapshot = app
        .world()
        .resource::<StreamingRuntime>()
        .snapshot
        .as_ref()
        .unwrap();
    assert_eq!(snapshot.process_rss_bytes, None);
    assert_eq!(snapshot.system_free_estimate_bytes, Some(16 * 1024 * MIB));
    assert!(!snapshot.pressure_sample_fresh);
    assert!(!snapshot.control.reasons.memory_pressure);
    app.world_mut()
        .resource_mut::<DiagnosticsStore>()
        .get_mut(&SystemInformationDiagnosticsPlugin::PROCESS_MEM_USAGE)
        .unwrap()
        .add_measurement(DiagnosticMeasurement {
            time: Instant::now(),
            value: 31.0,
        });
    update_streaming_control(app.world_mut());
    let snapshot = app
        .world()
        .resource::<StreamingRuntime>()
        .snapshot
        .as_ref()
        .unwrap();
    assert_eq!(snapshot.process_rss_bytes, Some(31 * 1024 * MIB));
    assert!(snapshot.pressure_sample_fresh);
    assert!(snapshot.control.reasons.memory_pressure);
    assert!(!snapshot.control.allow_scene_intake);
    let mut stale_process = Diagnostic::new(SystemInformationDiagnosticsPlugin::PROCESS_MEM_USAGE);
    stale_process.add_measurement(DiagnosticMeasurement {
        time: Instant::now() - std::time::Duration::from_secs(3),
        value: 31.0,
    });
    app.world_mut()
        .resource_mut::<DiagnosticsStore>()
        .add(stale_process);
    update_streaming_control(app.world_mut());
    let snapshot = app
        .world()
        .resource::<StreamingRuntime>()
        .snapshot
        .as_ref()
        .unwrap();
    assert_eq!(snapshot.process_rss_bytes, None);
    assert!(!snapshot.pressure_sample_fresh);
    assert!(!snapshot.control.reasons.memory_pressure);
}

#[test]
fn physical_pressure_blocks_new_subscribers_but_keeps_funded_drain_work() {
    let (mut app, _directory) = fixture(true);
    let initial = app.world_mut().spawn_empty().id();
    let late = app.world_mut().spawn_empty().id();
    let key = scene_key("meshes/a.glb");
    let mut runtime = app
        .world_mut()
        .remove_resource::<StreamingRuntime>()
        .unwrap();
    runtime.catalog = known_catalog();
    assert!(runtime.reserve_scene_with_placements(&key, &[(initial, true)]));
    assert!(runtime.reserve_cell(cell_key(0), false));
    assert!(runtime.reserve_cell_textures(cell_key(0), vec!["textures/shared.ktx2".into()]));
    assert!(runtime.reserve_cell(cell_key(1), false));
    let funded = runtime.ledger.totals();
    runtime.pressure_blocked = true;
    assert!(runtime.reserve_scene_placement(&key, initial, true));
    assert!(runtime.reserve_placement(initial, None, true));
    assert!(runtime.reserve_cell_textures(cell_key(0), vec!["textures/shared.ktx2".into()]));
    assert!(!runtime.reserve_scene_placement(&key, late, true));
    assert!(!runtime.reserve_placement(late, None, true));
    assert!(!runtime.reserve_cell_textures(cell_key(1), vec!["textures/shared.ktx2".into()]));
    assert_eq!(runtime.ledger.totals(), funded);
    runtime.pressure_blocked = false;
    assert!(runtime.reserve_scene_placement(&key, late, true));
    assert!(runtime.reserve_cell_textures(cell_key(1), vec!["textures/shared.ktx2".into()]));
}

#[test]
fn disabled_controls_leave_catalog_and_loader_state_unused() {
    let (mut app, directory) = fixture(false);
    let broken = directory.path().join("invalid-costs.json");
    std::fs::write(&broken, b"not JSON").unwrap();
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .streaming_costs = Some(broken);
    let mut runtime = StreamingRuntime::from_world(app.world_mut());
    assert!(!runtime.enabled);
    assert!(!runtime.catalog_loaded);
    assert!(runtime.reserve_scene(&scene_key("meshes/nonexistent.glb")));
    assert!(runtime.reserve_cell(cell_key(0), true));
    assert_eq!(runtime.ledger.totals().resources, 0);
    app.world_mut().insert_resource(runtime);
    let entity = app
        .world_mut()
        .spawn(request("meshes/nonexistent.glb", None))
        .id();
    requests::dispatch_scene_requests(app.world_mut());
    assert!(app.world().get::<SceneRequestKey>(entity).is_none());
    assert!(app.world().get::<PendingModel>(entity).is_none());
    update_streaming_control(app.world_mut());
    observe_streaming_ownership(app.world_mut());
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .snapshot
            .is_none()
    );
    assert!(
        app.world()
            .resource::<StreamingPreparationBridge>()
            .latest()
            .is_none()
    );
    assert!(
        app.world()
            .resource::<AssetServer>()
            .get_path_ids("meshes/nonexistent.glb#Scene0")
            .is_empty()
    );
}

#[test]
fn unknown_scene_is_denied_atomically_before_asset_server_load() {
    let (mut app, _directory) = fixture(true);
    app.world_mut()
        .resource_mut::<StreamingRuntime>()
        .memory_limit = Some(2 * UNKNOWN_BYTES - 1);
    let entity = app
        .world_mut()
        .spawn(request("meshes/nonexistent.glb", None))
        .id();
    requests::dispatch_scene_requests(app.world_mut());
    assert!(
        app.world()
            .get::<SceneRequest>(entity)
            .unwrap()
            .handle
            .is_none()
    );
    assert!(app.world().get::<PendingModel>(entity).is_none());
    assert_eq!(app.world().resource::<SceneAdmission>().active_jobs(), 0);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(runtime.scenes.is_empty());
    assert!(runtime.watches.is_empty());
    let totals = runtime.ledger.totals();
    assert_eq!(totals.resources, 0);
    assert_eq!(totals.peak_bytes(), Some(0));
    assert_eq!(totals.denied_total, 1);
    assert!(
        app.world()
            .resource::<AssetServer>()
            .get_path_ids("meshes/nonexistent.glb#Scene0")
            .is_empty()
    );
}

#[test]
fn shared_texture_is_reserved_once_and_each_placement_has_its_own_cost() {
    let (mut app, _directory) = fixture(true);
    let a = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    let b = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    let first = app.world_mut().spawn_empty().id();
    let second = app.world_mut().spawn_empty().id();
    let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
    runtime.catalog = known_catalog();
    assert!(runtime.reserve_scene(&scene_key("meshes/a.glb")));
    assert!(runtime.reserve_scene(&scene_key("meshes/b.glb")));
    runtime.bind_scene(&scene_key("meshes/a.glb"), a.id());
    runtime.bind_scene(&scene_key("meshes/b.glb"), b.id());
    assert!(runtime.reserve_placement(first, Some(&request("meshes/a.glb", Some(a))), true));
    assert!(runtime.reserve_placement(second, Some(&request("meshes/b.glb", Some(b))), true));
    let totals = runtime.ledger.totals();
    assert_eq!(totals.resources, 5);
    assert_eq!(totals.owners, 4);
    assert_eq!(totals.resident_bytes, 64 + 128 + 512 + 2 * (100 + 300));
    assert_eq!(totals.transient_bytes, 8 + 16 + 32 + 2 * (20 + 40));
}

#[test]
fn headless_owner_loss_keeps_resident_bytes_until_cpu_assets_are_absent() {
    let (mut app, _directory) = fixture(true);
    let key = scene_key("meshes/a.glb");
    let scene = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    let image = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    let subscriber = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .reconcile_demands([SceneDemand {
            key: key.clone(),
            subscriber,
            existing_handle: Some(scene.clone()),
            priority: 0,
        }]);
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .set_status(&key, super::super::admission::SceneJobStatus::Ready);
    {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        runtime.catalog = known_catalog();
        assert!(runtime.reserve_scene(&key));
        runtime.bind_scene(&key, scene.id());
        let texture = runtime.resource_key("textures/shared.ktx2", None);
        runtime.watches.get_mut(&texture).unwrap().demand.images = vec![image.id()];
        runtime.scenes.get_mut(&key).unwrap().dependencies_cached = true;
    }
    observe_at(&mut app, 1);
    let before = app.world().resource::<StreamingRuntime>().ledger.totals();
    assert_eq!(before.transient_bytes, 0);
    assert_eq!(before.resident_bytes, 64 + 512);
    app.world_mut().despawn(subscriber);
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .reconcile_demands([]);
    observe_at(&mut app, 2);
    observe_at(&mut app, 4);
    let retained = app.world().resource::<StreamingRuntime>().ledger.totals();
    assert_eq!(retained.orphan_resident_bytes, before.resident_bytes);
    app.world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .remove(scene.id());
    app.world_mut()
        .resource_mut::<Assets<Image>>()
        .remove(image.id());
    observe_at(&mut app, 5);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .peak_bytes(),
        Some(0)
    );
}

#[test]
fn generated_cell_waits_for_current_geometry_and_all_shared_texture_gpu_proofs() {
    let (mut app, _directory) = fixture(true);
    let key = cell_key(0);
    let mesh = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::default());
    let image = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    let root = app.world_mut().spawn_empty().id();
    app.world_mut()
        .spawn((TerrainPatch, Mesh3d(mesh), ChildOf(root)));
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(key, CellStatus::Resident { root });
    {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        runtime.catalog = known_catalog();
        assert!(runtime.reserve_cell(key, false));
        assert!(runtime.reserve_cell_textures(key, vec!["textures/shared.ktx2".into()]));
        runtime.bind_cell(key, root);
        let texture = runtime.resource_key("textures/shared.ktx2", None);
        runtime.watches.get_mut(&texture).unwrap().demand.images = vec![image.id()];
    }
    let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
    let initial = app
        .world()
        .resource::<StreamingRuntime>()
        .watches
        .values()
        .map(|watch| PreparationDemand {
            key: watch.demand.key,
            meshes: vec![],
            images: vec![],
            materials: vec![],
        })
        .collect();
    bridge.publish_fixture(1, initial, true);
    observe_at(&mut app, 2);
    let pending_bytes = app
        .world()
        .resource::<StreamingRuntime>()
        .ledger
        .totals()
        .transient_bytes;
    assert!(
        pending_bytes > 0,
        "An empty-set observation must not certify discovered dependencies"
    );
    let demands: Vec<_> = app
        .world()
        .resource::<StreamingRuntime>()
        .watches
        .values()
        .map(|watch| watch.demand.clone())
        .collect();
    let geometry_only = demands
        .iter()
        .filter(|demand| demand.images.is_empty())
        .cloned()
        .collect();
    bridge.publish_fixture(3, geometry_only, true);
    observe_at(&mut app, 3);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .transient_bytes,
        pending_bytes
    );
    bridge.publish_fixture(4, demands, true);
    observe_at(&mut app, 4);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .transient_bytes,
        0
    );
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .resident_bytes
            > 0
    );
}

#[test]
fn orphaned_scene_cannot_reload_until_assets_and_deferred_cleanup_are_absent() {
    let (mut app, _directory) = fixture(true);
    let key = scene_key("meshes/a.glb");
    let scene = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        runtime.catalog = known_catalog();
        assert!(runtime.reserve_scene(&key));
        runtime.bind_scene(&key, scene.id());
    }
    observe_at(&mut app, 1);
    assert!(
        !app.world_mut()
            .resource_mut::<StreamingRuntime>()
            .reserve_scene(&key)
    );
    app.world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .remove(scene.id());
    observe_at(&mut app, 2);
    assert!(
        !app.world_mut()
            .resource_mut::<StreamingRuntime>()
            .reserve_scene(&key)
    );
    observe_at(&mut app, 3);
    assert!(
        app.world_mut()
            .resource_mut::<StreamingRuntime>()
            .reserve_scene(&key)
    );
}

#[test]
fn generated_cost_sum_overflow_is_denied_without_partial_reservations() {
    let (mut app, _directory) = fixture(true);
    let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
    runtime.catalog.generated.full_cell_terrain =
        ByteEstimate::conservative(u64::MAX - MIB, 0, "oversized fixture");
    runtime.catalog.generated.full_cell_water = ByteEstimate::conservative(1, 0, "fixture");
    assert!(!runtime.reserve_cell(cell_key(0), false));
    assert!(runtime.cells.is_empty());
    assert!(runtime.watches.is_empty());
    assert_eq!(runtime.ledger.totals().resources, 0);
}

#[test]
fn door_landing_keeps_uploads_unlimited_across_controller_updates() {
    use crate::door_crossing::DoorCrossing;
    let (mut app, _directory) = fixture(true);
    app.insert_resource(RenderAssetBytesPerFrame { max_bytes: Some(1) })
        .insert_resource(DoorCrossing::holding_for_test());
    for _ in 0..4 {
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(std::time::Duration::from_millis(50));
        update_streaming_control(app.world_mut());
        assert_eq!(
            app.world().resource::<RenderAssetBytesPerFrame>().max_bytes,
            None
        );
        let runtime = app.world().resource::<StreamingRuntime>();
        let expected = Some(runtime.decision.unwrap().budgets.max_upload_bytes_per_frame as usize);
        assert_eq!(
            current_upload_budget(app.world().resource::<EngineConfig>(), Some(runtime)),
            expected
        );
    }
    app.insert_resource(DoorCrossing::default());
    update_streaming_control(app.world_mut());
    let restored = current_upload_budget(
        app.world().resource::<EngineConfig>(),
        Some(app.world().resource::<StreamingRuntime>()),
    );
    assert!(restored.is_some());
    assert_eq!(
        app.world().resource::<RenderAssetBytesPerFrame>().max_bytes,
        restored
    );
}

#[test]
fn native_material_reservations_wait_for_cpu_absence_and_unknown_stores() {
    let (mut app, _directory) = fixture(true);
    app.init_asset::<NifDepthMaterial>();
    let material = app
        .world_mut()
        .resource_mut::<Assets<NifDepthMaterial>>()
        .add(crate::nif_depth::depth_material(
            StandardMaterial::default(),
            crate::nif_depth::NifDepthState {
                depth_test: false,
                depth_write: false,
                decal: false,
                alpha_mask: false,
            },
        ));
    let watch = ResourceWatch {
        demand: PreparationDemand {
            key: PreparationKey::Resource(1),
            meshes: vec![],
            images: vec![],
            materials: vec![material.id().into()],
        },
        root_scene: None,
        root_entity: None,
        texture_path: None,
        orphaned_frame: Some(1),
    };
    assert!(!cpu_absent(app.world(), &watch));
    let store = app
        .world_mut()
        .remove_resource::<Assets<NifDepthMaterial>>()
        .unwrap();
    assert!(
        !cpu_absent(app.world(), &watch),
        "unknown typed storage retains its reservation"
    );
    app.insert_resource(store);
    app.world_mut()
        .resource_mut::<Assets<NifDepthMaterial>>()
        .remove(material.id());
    assert!(cpu_absent(app.world(), &watch));
}

#[test]
fn dynamic_clutter_preflight_includes_the_collision_estimate() {
    let (mut app, _directory) = fixture(true);
    let entity = app.world_mut().spawn_empty().id();
    let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
    runtime.catalog = known_catalog();
    let collision = super::super::collision_record_eligible(Some("MISC"));
    assert!(
        runtime.reserve_scene_with_placements(&scene_key("meshes/a.glb"), &[(entity, collision)])
    );
    let totals = runtime.ledger.totals();
    assert_eq!(totals.resident_bytes, 64 + 512 + 100 + 300);
    assert_eq!(totals.transient_bytes, 8 + 32 + 20 + 40);
}

#[test]
fn camera_rebase_preserves_motion_history_and_teleport_resets_control() {
    let (mut app, _directory) = fixture(true);
    app.world_mut().insert_resource(RenderOrigin(IVec2::ZERO));
    let camera = app
        .world_mut()
        .spawn((
            StreamingCamera,
            Transform::from_translation(Vec3::new(40.0, 0.0, 0.0)),
        ))
        .id();
    update_streaming_control(app.world_mut());
    app.world_mut().resource_mut::<RenderOrigin>().0 = IVec2::new(1, 0);
    app.world_mut()
        .get_mut::<Transform>(camera)
        .unwrap()
        .translation
        .x -= CELL_SIZE;
    update_streaming_control(app.world_mut());
    let rebased = app.world().resource::<StreamingRuntime>().decision.unwrap();
    assert!(!rebased.reasons.camera_discontinuity);
    assert!(!rebased.reasons.camera_motion);
    app.world_mut()
        .get_mut::<Transform>(camera)
        .unwrap()
        .translation
        .x += 3.0 * CELL_SIZE;
    update_streaming_control(app.world_mut());
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .decision
            .unwrap()
            .reasons
            .camera_discontinuity
    );
    let position = app.world().get::<Transform>(camera).unwrap().translation;
    for space in [
        ActiveSpace {
            worldspace_id: Some(61),
            interior: None,
        },
        ActiveSpace {
            worldspace_id: Some(61),
            interior: Some(7),
        },
        ActiveSpace {
            worldspace_id: Some(61),
            interior: Some(8),
        },
    ] {
        app.insert_resource(space);
        update_streaming_control(app.world_mut());
        assert!(
            app.world()
                .resource::<StreamingRuntime>()
                .decision
                .unwrap()
                .reasons
                .camera_discontinuity,
            "space change at unchanged camera coordinates"
        );
        assert_eq!(
            app.world().get::<Transform>(camera).unwrap().translation,
            position
        );
    }
}

#[test]
fn exact_fit_preflight_transfers_placement_claim_without_activation_growth() {
    let (mut app, _directory) = fixture(true);
    let key = scene_key("meshes/a.glb");
    let entity = app.world_mut().spawn_empty().id();
    let scene = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
    runtime.catalog = known_catalog();
    let full_pipeline_peak = (64 + 8) + (512 + 32) + (100 + 20) + (300 + 40);
    runtime.memory_limit = Some(full_pipeline_peak);
    assert!(runtime.reserve_scene_with_placements(&key, &[(entity, true)]));
    runtime.bind_scene(&key, scene.id());
    let before = runtime.ledger.totals();
    assert_eq!(before.peak_bytes(), Some(full_pipeline_peak));
    let scene_owner = &runtime.scenes[&key].owner;
    let placement_owner = &runtime.placements[&entity];
    assert!(
        !runtime
            .ledger
            .owner_resources(scene_owner)
            .unwrap()
            .contains(placement_owner)
    );
    assert!(
        runtime
            .ledger
            .owner_resources(placement_owner)
            .unwrap()
            .contains(placement_owner)
    );
    assert!(runtime.reserve_placement(entity, Some(&request("meshes/a.glb", Some(scene))), true));
    assert_eq!(runtime.ledger.totals(), before);
}

#[test]
fn denied_late_subscriber_does_not_hold_prepared_scene_transient() {
    let (mut app, _directory) = fixture(true);
    let key = scene_key("meshes/a.glb");
    let scene = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    let image = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    let first = app
        .world_mut()
        .spawn((
            SceneRequestKey(key.clone()),
            request("meshes/a.glb", Some(scene.clone())),
        ))
        .id();
    let late = app
        .world_mut()
        .spawn((
            SceneRequestKey(key.clone()),
            request("meshes/a.glb", None),
            PendingAssetProfile {
                started: Instant::now(),
                scene_spawned: false,
                path: "meshes/a.glb".into(),
                form_id: 1,
                base_form_id: 2,
                base_record_type: None,
                static_physics: false,
                cell_id: 3,
            },
        ))
        .id();
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .reconcile_demands([SceneDemand {
            key: key.clone(),
            subscriber: first,
            existing_handle: Some(scene.clone()),
            priority: 0,
        }]);
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .set_status(&key, super::super::admission::SceneJobStatus::Ready);
    {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        runtime.catalog = known_catalog();
        runtime.memory_limit = Some((64 + 8) + (512 + 32) + (100 + 20));
        assert!(runtime.reserve_scene_with_placements(&key, &[(first, false)]));
        runtime.bind_scene(&key, scene.id());
        runtime.scenes.get_mut(&key).unwrap().dependencies_cached = true;
        let texture = runtime.resource_key("textures/shared.ktx2", None);
        runtime.watches.get_mut(&texture).unwrap().demand.images = vec![image.id()];
    }
    requests::dispatch_scene_requests(app.world_mut());
    assert!(
        app.world()
            .get::<SceneRequest>(late)
            .unwrap()
            .handle
            .is_none()
    );
    assert!(app.world().get::<PendingModel>(late).is_none());
    assert!(
        !app.world()
            .resource::<StreamingRuntime>()
            .placements
            .contains_key(&late)
    );
    let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
    let demands = app
        .world()
        .resource::<StreamingRuntime>()
        .watches
        .values()
        .map(|watch| watch.demand.clone())
        .collect();
    bridge.publish_fixture(1, demands, true);
    observe_at(&mut app, 1);
    assert!(app.world().get::<PendingAssetProfile>(late).is_some());
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(runtime.scenes[&key].prepared);
    assert_eq!(runtime.ledger.totals().transient_bytes, 0);
    assert_eq!(runtime.ledger.totals().resident_bytes, 64 + 512 + 100);
}

#[test]
fn abandoning_one_preflighted_subscriber_preserves_shared_scene_resources() {
    let (mut app, _directory) = fixture(true);
    let key = scene_key("meshes/a.glb");
    let scene = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    let first = app.world_mut().spawn_empty().id();
    let second = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .reconcile_demands([SceneDemand {
            key: key.clone(),
            subscriber: second,
            existing_handle: Some(scene.clone()),
            priority: 0,
        }]);
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .set_status(&key, super::super::admission::SceneJobStatus::Ready);
    {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        runtime.catalog = known_catalog();
        assert!(runtime.reserve_scene_with_placements(&key, &[(first, false), (second, false)]));
        runtime.bind_scene(&key, scene.id());
    }
    app.world_mut().despawn(first);
    observe_at(&mut app, 1);
    let runtime = app.world().resource::<StreamingRuntime>();
    for resource in &runtime.scenes[&key].resources {
        assert!(runtime.ledger.resource_has_owners(resource));
    }
    assert_eq!(runtime.ledger.totals().orphan_resident_bytes, 100);
    observe_at(&mut app, 3);
    let totals = app.world().resource::<StreamingRuntime>().ledger.totals();
    assert_eq!(totals.orphan_resident_bytes, 0);
    assert_eq!(totals.resident_bytes, 64 + 512 + 100);
}

#[test]
fn abandoned_preflight_claim_waits_for_entity_absence_and_cleanup_frames() {
    let (mut app, _directory) = fixture(true);
    let key = scene_key("meshes/a.glb");
    let entity = app.world_mut().spawn_empty().id();
    let owner = {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        runtime.catalog = known_catalog();
        assert!(runtime.reserve_scene_with_placements(&key, &[(entity, false)]));
        let owner = runtime.placements.remove(&entity).unwrap();
        runtime.frame = 1;
        runtime.abandon(&owner);
        owner
    };
    observe_at(&mut app, 3);
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .contains_resource(&owner)
    );
    app.world_mut().despawn(entity);
    observe_at(&mut app, 4);
    assert!(
        !app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .contains_resource(&owner)
    );
}

#[test]
fn coarse_chunk_retains_scene_and_placement_transient_until_instantiation_finishes() {
    use crate::world::database::{LodChunkBounds, LodChunkMetadata};
    use shared::lod::{ChunkAnchor, ChunkKey, LodOrigin, LodTier};

    let (mut app, _directory) = fixture(true);
    let key = scene_key("meshes/a.glb");
    let scene = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    let image = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    let pending = super::super::lod::pending_fixture(LodChunkMetadata {
        key: ChunkKey::new(0x3c, LodTier::Tier4, ChunkAnchor::new(0, 0)),
        payload_path: key.canonical_path.clone(),
        content_hash: "a".repeat(64),
        bounds: LodChunkBounds {
            min: [0.0; 3],
            max: [100.0; 3],
        },
        source_cells: vec![[0, 0]],
        build_identity: "test-pack".into(),
        origin: LodOrigin::new(0, 0),
    });
    let mut coarse = request("meshes/a.glb", Some(scene.clone()));
    coarse.coarse_terrain = true;
    let entity = app
        .world_mut()
        .spawn((SceneRequestKey(key.clone()), coarse, pending))
        .id();
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .reconcile_demands([SceneDemand {
            key: key.clone(),
            subscriber: entity,
            existing_handle: Some(scene.clone()),
            priority: 0,
        }]);
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .set_status(&key, super::super::admission::SceneJobStatus::Ready);
    {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        runtime.catalog = known_catalog();
        assert!(runtime.reserve_scene_with_placements(&key, &[(entity, false)]));
        runtime.bind_scene(&key, scene.id());
        runtime.scenes.get_mut(&key).unwrap().dependencies_cached = true;
        let texture = runtime.resource_key("textures/shared.ktx2", None);
        runtime.watches.get_mut(&texture).unwrap().demand.images = vec![image.id()];
    }
    let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
    let demands = app
        .world()
        .resource::<StreamingRuntime>()
        .watches
        .values()
        .map(|watch| watch.demand.clone())
        .collect();
    bridge.publish_fixture(1, demands, true);
    observe_at(&mut app, 1);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(!runtime.scenes[&key].prepared);
    assert_eq!(runtime.ledger.totals().transient_bytes, 8 + 32 + 20);
    app.world_mut()
        .entity_mut(entity)
        .remove::<super::super::lod::PendingLodChunk>();
    observe_at(&mut app, 2);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(runtime.scenes[&key].prepared);
    assert_eq!(runtime.ledger.totals().transient_bytes, 0);
}

fn system_info(gib: u32) -> SystemInfo {
    SystemInfo {
        os: "fixture".into(),
        kernel: "fixture".into(),
        cpu: "fixture".into(),
        core_count: "1".into(),
        memory: format!("{gib} GiB"),
    }
}

#[test]
fn adaptive_automatic_memory_limit_uses_half_physical_ram_with_a_finite_ceiling() {
    let mut world = World::new();
    let config = EngineConfig {
        adaptive_streaming: true,
        ..default()
    };
    for (physical_gib, expected_gib) in [(32, 16), (8, 4), (64, 16)] {
        world.insert_resource(system_info(physical_gib));
        assert_eq!(
            automatic_memory_limit(&config, &world),
            Some(expected_gib * 1024 * MIB)
        );
    }
}

#[test]
fn missing_physical_memory_uses_fallback_and_explicit_memory_limit_takes_precedence() {
    let mut world = World::new();
    let mut config = EngineConfig {
        adaptive_streaming: true,
        ..default()
    };
    assert_eq!(
        automatic_memory_limit(&config, &world),
        Some(8 * 1024 * MIB)
    );
    world.insert_resource(system_info(32));
    config.streaming_memory_mib = 64;
    assert_eq!(automatic_memory_limit(&config, &world), Some(64 * MIB));
    config.adaptive_streaming = false;
    assert_eq!(automatic_memory_limit(&config, &world), Some(64 * MIB));
}

#[test]
fn physical_ram_at_or_below_headroom_never_becomes_unlimited_and_blocks_intake() {
    let (mut app, _directory) = fixture(true);
    app.world_mut().insert_resource(system_info(8));
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.adaptive_streaming = true;
        config.streaming_memory_mib = 0;
        config.streaming_headroom_mib = 8 * 1024;
    }
    let runtime = StreamingRuntime::from_world(app.world_mut());
    assert_eq!(runtime.memory_limit, Some(1));
    app.world_mut().insert_resource(runtime);
    for headroom_mib in [8 * 1024, 9 * 1024] {
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .streaming_headroom_mib = headroom_mib;
        assert_eq!(
            automatic_memory_limit(app.world().resource::<EngineConfig>(), app.world()),
            Some(1)
        );
        update_streaming_control(app.world_mut());
        let runtime = app.world().resource::<StreamingRuntime>();
        assert!(runtime.pressure_blocked);
        let decision = runtime.decision.unwrap();
        assert!(decision.reasons.memory_pressure);
        assert!(!decision.allow_scene_intake);
        assert!(!decision.allow_cell_requests);
        assert_eq!(
            runtime.snapshot.as_ref().unwrap().memory_limit_bytes,
            Some(1)
        );
    }
}

#[test]
fn unfit_deferred_cell_does_not_block_affordable_cells_or_fresh_stale_responses() {
    use crate::{
        render::WaterReflectionTexture,
        streaming::{CellStatus, StreamingCommitBudget, TerrainContinuity, collect_cells},
        world::{
            cache::CellCache,
            database::{
                AssetCatalog, CellPayload, DatabaseRequest, DatabaseResponse, WorldDatabase,
            },
        },
    };
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    let (mut app, directory) = fixture(true);
    app.world_mut().resource_mut::<EngineConfig>().headless = true;
    let database_path = directory.path().join("collector.db");
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE schema_info(version INTEGER NOT NULL);
             INSERT INTO schema_info VALUES({});
             CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT);
             CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER);
             CREATE TABLE waters(id INTEGER PRIMARY KEY,flow_normal_path TEXT);
             INSERT INTO texture_sets VALUES(1,'textures/too-large.dds');
             INSERT INTO landscape_textures VALUES(1,1);",
            shared::WORLD_DATABASE_SCHEMA_VERSION,
        ))
        .unwrap();
    drop(connection);
    let cache_path = directory.path().join("collector.rkyv");
    let cache = shared::CellCache {
        version: shared::CELL_CACHE_VERSION,
        cells: vec![shared::CachedLand {
            cell_id: 1,
            width: 33,
            height: 33,
            heights: vec![0.0; 33 * 33],
            normals: vec![0; 33 * 33 * 3],
            vertex_colors: vec![],
            layers: (0..4)
                .map(|quadrant| shared::TerrainLayer {
                    texture_form_id: 1,
                    quadrant,
                    layer: 0,
                    is_base: true,
                    weights: vec![],
                })
                .collect(),
            water_height: None,
            water_type_form_id: None,
        }],
    };
    std::fs::write(
        &cache_path,
        rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap(),
    )
    .unwrap();
    app.insert_resource(WorldDatabase::open(&database_path).unwrap())
        .insert_resource(AssetCatalog::open(&database_path).unwrap())
        .insert_resource(CellCache::open(&cache_path).unwrap())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(WaterReflectionTexture(Handle::default()))
        .init_resource::<StreamingCommitBudget>()
        .init_resource::<TerrainContinuity>();

    let blocked = CellKey::Interior(1);
    let affordable = CellKey::Interior(2);
    let response = |key, cell_id| DatabaseResponse {
        generation: 7,
        key,
        result: Ok(CellPayload {
            generation: 7,
            key,
            cell_id,
            references: vec![],
        }),
        query_micros: 0,
        queue_wait_micros: 0,
        total_request_micros: 0,
        row_count: 0,
    };
    {
        let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
        for key in [blocked, affordable] {
            streaming
                .cells
                .insert(key, CellStatus::Loading { generation: 7 });
            streaming.outstanding_cells.insert(key, 7);
        }
    }
    app.world_mut()
        .resource_mut::<StreamingRuntime>()
        .deferred_responses = VecDeque::from([response(blocked, 1), response(affordable, 2)]);
    // Deferred entries were counted when the channel first delivered them.
    app.world_mut()
        .resource_mut::<StreamingMetrics>()
        .responses_received = 2;
    app.world()
        .resource::<WorldDatabase>()
        .request(DatabaseRequest::Load {
            generation: 1,
            key: CellKey::Interior(999),
            queued_at: Instant::now(),
        })
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        {
            let mut budget = app.world_mut().resource_mut::<StreamingCommitBudget>();
            budget.remaining = 1;
            budget.commits = 0;
        }
        app.world_mut().run_system_once(collect_cells).unwrap();
        let streaming = app.world().resource::<StreamingWorld>();
        let metrics = app.world().resource::<StreamingMetrics>();
        if matches!(streaming.cells[&affordable], CellStatus::Resident { .. })
            && metrics.responses_received == 3
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cell response queues stopped making progress"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..3 {
        {
            let mut budget = app.world_mut().resource_mut::<StreamingCommitBudget>();
            budget.remaining = 1;
            budget.commits = 0;
        }
        app.world_mut().run_system_once(collect_cells).unwrap();
    }
    let metrics = app.world().resource::<StreamingMetrics>();
    assert_eq!(metrics.responses_received, 3);
    assert_eq!(metrics.stale_responses, 1);
    assert_eq!(metrics.failed_cells, 0);
    assert!(matches!(
        app.world().resource::<StreamingWorld>().cells[&blocked],
        CellStatus::Loading { generation: 7 }
    ));
    let runtime = app.world().resource::<StreamingRuntime>();
    assert_eq!(runtime.deferred_responses.len(), 1);
    assert_eq!(runtime.deferred_responses[0].key, blocked);
    assert_eq!(runtime.cells.len(), 2);
    assert!(
        app.world()
            .resource::<StreamingWorld>()
            .outstanding_cells
            .contains_key(&blocked)
    );
    assert!(
        !app.world()
            .resource::<StreamingWorld>()
            .outstanding_cells
            .contains_key(&affordable)
    );
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .remove(&blocked);
    {
        let mut budget = app.world_mut().resource_mut::<StreamingCommitBudget>();
        budget.remaining = 1;
        budget.commits = 0;
    }
    app.world_mut().run_system_once(collect_cells).unwrap();
    assert!(
        app.world()
            .resource::<StreamingWorld>()
            .outstanding_cells
            .is_empty()
    );
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .deferred_responses
            .is_empty()
    );
    let metrics = app.world().resource::<StreamingMetrics>();
    assert_eq!(metrics.responses_received, 3);
    assert_eq!(metrics.stale_responses, 2);
}

#[derive(TypePath)]
struct CatalogSceneLoader;

impl bevy::asset::AssetLoader for CatalogSceneLoader {
    type Asset = WorldAsset;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        _: &mut dyn bevy::asset::io::Reader,
        _: &(),
        context: &mut bevy::asset::LoadContext<'_>,
    ) -> Result<WorldAsset, std::io::Error> {
        let mut scene = World::new();
        if context
            .path()
            .path()
            .file_name()
            .is_some_and(|name| name == "b.glb")
        {
            let image = context.load::<Image>("textures/shared.ktx2");
            let material = context.add_labeled_asset(
                "Material",
                StandardMaterial {
                    base_color_texture: Some(image),
                    ..default()
                },
            );
            scene.spawn(MeshMaterial3d(material));
        }
        Ok(WorldAsset::new(scene))
    }

    fn extensions(&self) -> &[&str] {
        &["glb"]
    }
}

#[derive(TypePath)]
struct CatalogImageLoader;

impl bevy::asset::AssetLoader for CatalogImageLoader {
    type Asset = Image;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        _: &mut dyn bevy::asset::io::Reader,
        _: &(),
        _: &mut bevy::asset::LoadContext<'_>,
    ) -> Result<Image, std::io::Error> {
        Ok(Image::default())
    }

    fn extensions(&self) -> &[&str] {
        &["ktx2"]
    }
}

fn catalog_dependency_fixture() -> (App, tempfile::TempDir) {
    let (mut app, directory) = fixture(true);
    app.register_asset_loader(CatalogSceneLoader)
        .register_asset_loader(CatalogImageLoader);
    for path in ["meshes/a.glb", "meshes/b.glb", "textures/shared.ktx2"] {
        let file = directory.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, []).unwrap();
    }
    app.world_mut().resource_mut::<StreamingRuntime>().catalog = known_catalog();
    (app, directory)
}

fn wait_for_catalog_asset<A: Asset>(app: &mut App, handle: &Handle<A>) {
    let deadline = Instant::now() + std::time::Duration::from_secs(2);
    loop {
        app.update();
        if app
            .world()
            .resource::<AssetServer>()
            .is_loaded_with_dependencies(handle.id())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "catalog fixture asset did not load"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn register_catalog_scene(app: &mut App, path: &str) -> (SceneKey, Handle<WorldAsset>) {
    let scene: Handle<WorldAsset> = app.world().resource::<AssetServer>().load(path.to_owned());
    wait_for_catalog_asset(app, &scene);
    let key = scene_key(path);
    let subscriber = app.world_mut().spawn_empty().id();
    let mut demands: Vec<_> = app
        .world()
        .resource::<SceneAdmission>()
        .tracked_jobs()
        .into_iter()
        .map(|(key, _)| SceneDemand {
            existing_handle: app.world().resource::<SceneAdmission>().handle(&key),
            key,
            subscriber: app.world_mut().spawn_empty().id(),
            priority: 0,
        })
        .collect();
    demands.push(SceneDemand {
        key: key.clone(),
        subscriber,
        existing_handle: Some(scene.clone()),
        priority: 0,
    });
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .reconcile_demands(demands);
    app.world_mut()
        .resource_mut::<SceneAdmission>()
        .set_status(&key, super::super::admission::SceneJobStatus::Ready);
    let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
    assert!(runtime.reserve_scene(&key));
    runtime.bind_scene(&key, scene.id());
    (key, scene)
}

fn watched_demands(app: &App) -> Vec<PreparationDemand> {
    app.world()
        .resource::<StreamingRuntime>()
        .watches
        .values()
        .map(|watch| watch.demand.clone())
        .collect()
}

#[test]
fn unused_catalog_texture_waits_for_cpu_materials_then_releases_after_absence() {
    let (mut app, _directory) = catalog_dependency_fixture();
    let (key, scene) = register_catalog_scene(&mut app, "meshes/a.glb");
    let material = app
        .world()
        .resource::<Assets<StandardMaterial>>()
        .reserve_handle();
    app.world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .get_mut(scene.id())
        .unwrap()
        .world
        .spawn(MeshMaterial3d(material.clone()));
    let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
    bridge.publish_fixture(1, watched_demands(&app), true);
    observe_at(&mut app, 1);
    let runtime = app.world().resource::<StreamingRuntime>();
    let texture = runtime.resource_key("textures/shared.ktx2", None);
    assert!(!runtime.scenes[&key].dependencies_cached);
    assert!(runtime.scenes[&key].resources.contains(&texture));
    assert!(runtime.watches[&texture].orphaned_frame.is_none());

    app.world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .insert(material.id(), StandardMaterial::default())
        .unwrap();
    observe_at(&mut app, 2);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(runtime.scenes[&key].dependencies_cached);
    assert!(!runtime.scenes[&key].resources.contains(&texture));
    assert_eq!(runtime.watches[&texture].orphaned_frame, Some(2));
    assert_eq!(runtime.ledger.totals().orphan_bytes(), Some(512 + 32));
    assert!(
        app.world()
            .resource::<AssetServer>()
            .get_path_ids("textures/shared.ktx2")
            .is_empty()
    );
    bridge.publish_fixture(3, watched_demands(&app), true);
    observe_at(&mut app, 3);
    assert!(app.world().resource::<StreamingRuntime>().scenes[&key].prepared);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .transient_bytes,
        32
    );
    let orphan = app.world().resource::<StreamingRuntime>().watches[&texture]
        .demand
        .clone();
    bridge.publish_absent_fixture(4, vec![orphan]);
    observe_at(&mut app, 4);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(!runtime.watches.contains_key(&texture));
    assert_eq!(runtime.ledger.totals().resident_bytes, 64);
    assert_eq!(runtime.ledger.totals().transient_bytes, 0);
}

#[test]
fn detached_catalog_texture_retains_all_cpu_and_gpu_image_generations_until_disposal() {
    let (mut app, _directory) = catalog_dependency_fixture();
    let (_, _scene) = register_catalog_scene(&mut app, "meshes/a.glb");
    let previous = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    let current: Handle<Image> = app
        .world()
        .resource::<AssetServer>()
        .load("textures/shared.ktx2");
    wait_for_catalog_asset(&mut app, &current);
    let texture = {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        let texture = runtime.resource_key("textures/shared.ktx2", None);
        runtime.watches.get_mut(&texture).unwrap().demand.images = vec![previous.id()];
        texture
    };
    let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
    bridge.publish_fixture(1, watched_demands(&app), true);
    observe_at(&mut app, 1);
    let demand = app.world().resource::<StreamingRuntime>().watches[&texture]
        .demand
        .clone();
    assert!(demand.images.contains(&previous.id()));
    assert!(demand.images.contains(&current.id()));
    assert_eq!(demand.images.len(), 2);
    bridge.publish_absent_fixture(3, vec![demand.clone()]);
    observe_at(&mut app, 3);
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .watches
            .contains_key(&texture),
        "CPU-owned image data must retain the orphan charge"
    );
    app.world_mut()
        .resource_mut::<Assets<Image>>()
        .remove(previous.id());
    app.world_mut()
        .resource_mut::<Assets<Image>>()
        .remove(current.id());
    bridge.publish_fixture(4, vec![demand.clone()], true);
    observe_at(&mut app, 4);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .orphan_bytes(),
        Some(512 + 32),
        "GPU image presence must retain bytes after CPU disposal"
    );
    bridge.publish_absent_fixture(5, vec![demand]);
    observe_at(&mut app, 5);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(!runtime.watches.contains_key(&texture));
    assert_eq!(runtime.ledger.totals().resident_bytes, 64);
    assert_eq!(runtime.ledger.totals().transient_bytes, 0);
}

#[test]
fn native_material_image_dependencies_keep_their_catalog_reservation() {
    let (mut app, _directory) = catalog_dependency_fixture();
    app.init_asset::<NifDepthMaterial>();
    let (key, scene) = register_catalog_scene(&mut app, "meshes/a.glb");
    let image: Handle<Image> = app
        .world()
        .resource::<AssetServer>()
        .load("textures/shared.ktx2");
    wait_for_catalog_asset(&mut app, &image);
    let material = app
        .world_mut()
        .resource_mut::<Assets<NifDepthMaterial>>()
        .add(crate::nif_depth::depth_material(
            StandardMaterial {
                base_color_texture: Some(image.clone()),
                ..default()
            },
            crate::nif_depth::NifDepthState {
                depth_test: false,
                depth_write: false,
                decal: false,
                alpha_mask: false,
            },
        ));
    app.world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .get_mut(scene.id())
        .unwrap()
        .world
        .spawn(MeshMaterial3d(material.clone()));
    observe_at(&mut app, 1);
    let runtime = app.world().resource::<StreamingRuntime>();
    let texture = runtime.resource_key("textures/shared.ktx2", None);
    assert!(runtime.scenes[&key].dependencies_cached);
    assert!(runtime.scenes[&key].resources.contains(&texture));
    assert_eq!(runtime.watches[&texture].demand.images, vec![image.id()]);
    assert!(runtime.watches[&texture].orphaned_frame.is_none());
    assert!(
        runtime
            .watches
            .values()
            .any(|watch| watch.demand.materials.contains(&material.id().into()))
    );
    assert_eq!(runtime.ledger.totals().resident_bytes, 64 + 512);
}

#[test]
fn detaching_unused_texture_claim_preserves_the_scene_that_actually_uses_it() {
    let (mut app, _directory) = catalog_dependency_fixture();
    let (unused, _a) = register_catalog_scene(&mut app, "meshes/a.glb");
    let (used, _b) = register_catalog_scene(&mut app, "meshes/b.glb");
    let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
    bridge.publish_fixture(1, watched_demands(&app), true);
    observe_at(&mut app, 1);
    let runtime = app.world().resource::<StreamingRuntime>();
    let texture = runtime.resource_key("textures/shared.ktx2", None);
    assert!(!runtime.scenes[&unused].resources.contains(&texture));
    assert!(runtime.scenes[&used].resources.contains(&texture));
    assert!(
        runtime
            .ledger
            .owner_resources(&runtime.scenes[&used].owner)
            .unwrap()
            .contains(&texture)
    );
    assert!(runtime.watches[&texture].orphaned_frame.is_none());
    assert_eq!(runtime.watches[&texture].demand.images.len(), 1);
    bridge.publish_fixture(2, watched_demands(&app), true);
    observe_at(&mut app, 2);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(runtime.scenes[&unused].prepared);
    assert!(runtime.scenes[&used].prepared);
    assert_eq!(runtime.ledger.totals().resident_bytes, 64 + 128 + 512);
    assert_eq!(runtime.ledger.totals().transient_bytes, 0);
    assert_eq!(runtime.ledger.totals().orphan_bytes(), Some(0));
}

#[test]
fn unknown_material_image_identity_keeps_catalog_texture_claims_conservative() {
    let (mut app, _directory) = catalog_dependency_fixture();
    let (key, scene) = register_catalog_scene(&mut app, "meshes/a.glb");
    let image = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    let material = app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color_texture: Some(image),
            ..default()
        });
    app.world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .get_mut(scene.id())
        .unwrap()
        .world
        .spawn(MeshMaterial3d(material));
    observe_at(&mut app, 1);
    let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
    bridge.publish_fixture(2, watched_demands(&app), true);
    observe_at(&mut app, 2);
    let runtime = app.world().resource::<StreamingRuntime>();
    let texture = runtime.resource_key("textures/shared.ktx2", None);
    assert!(runtime.scenes[&key].dependencies_cached);
    assert!(runtime.scenes[&key].resources.contains(&texture));
    assert!(runtime.watches[&texture].orphaned_frame.is_none());
    assert!(!runtime.scenes[&key].prepared);
    assert_eq!(runtime.ledger.totals().transient_bytes, 8 + 32);
}
