//! Full-cell loading, retry, failure and ownership regressions.
#![cfg(test)]

use super::tests::{
    crossing_config, exterior_cell, move_camera, spawn_camera, streaming_metrics, terrain_fixture,
    write_empty_catalogue, write_plan_database,
};
use super::*;
use bevy::asset::{AssetApp, AssetPlugin};
use std::time::Duration;

const INTERIOR_CELL: u32 = 99;

fn write_terrain_database(path: &std::path::Path) {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            r#"CREATE TABLE schema_info(version INTEGER NOT NULL);
            INSERT INTO schema_info VALUES({version});
            CREATE TABLE cells(id INTEGER PRIMARY KEY,worldspace_id INTEGER,grid_x INTEGER,grid_y INTEGER);
            CREATE TABLE land(cell_id INTEGER PRIMARY KEY);
            CREATE TABLE statics(id INTEGER PRIMARY KEY,model_path TEXT,flags INTEGER NOT NULL,
                bounds_min_x REAL NOT NULL DEFAULT -64,bounds_min_y REAL NOT NULL DEFAULT -64,bounds_min_z REAL NOT NULL DEFAULT -64,
                bounds_max_x REAL NOT NULL DEFAULT 64,bounds_max_y REAL NOT NULL DEFAULT 64,bounds_max_z REAL NOT NULL DEFAULT 64,
                bounds_valid INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE "references"(id INTEGER PRIMARY KEY,cell_id INTEGER NOT NULL,worldspace_id INTEGER,base_form_id INTEGER NOT NULL,
                is_exterior INTEGER NOT NULL,pos_x REAL NOT NULL,pos_y REAL NOT NULL,pos_z REAL NOT NULL,local_x REAL,local_y REAL,
                rot_x REAL NOT NULL,rot_y REAL NOT NULL,rot_z REAL NOT NULL,scale REAL NOT NULL DEFAULT 1.0);
            CREATE VIRTUAL TABLE exterior_spatial USING rtree(id,minX,maxX,minY,maxY,minZ,maxZ,+cell_id,+worldspace_id);
            INSERT INTO cells VALUES(1,60,2,-3);
            INSERT INTO land VALUES(1);
            -- The cell one grid west, with no landscape in the cache: a full request for it
            -- still resolves to a cell, so a run that starts there has no failed cell in it.
            INSERT INTO cells VALUES(2,60,1,-3);"#,
            version = shared::WORLD_DATABASE_SCHEMA_VERSION
        ))
        .unwrap();
}

fn write_terrain_cache(path: &std::path::Path, cell_id: u32) {
    let cache = shared::CellCache {
        version: shared::CELL_CACHE_VERSION,
        cells: vec![shared::CachedLand {
            cell_id,
            width: 33,
            height: 33,
            heights: vec![0.0; 33 * 33],
            normals: (0..33 * 33).flat_map(|_| [0, 0, 127]).collect(),
            vertex_colors: Vec::new(),
            layers: Vec::new(),
            water_height: None,
            water_type_form_id: None,
        }],
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn write_invalid_terrain_cache(path: &std::path::Path, cell_id: u32) {
    let cache = shared::CellCache {
        version: shared::CELL_CACHE_VERSION,
        cells: vec![shared::CachedLand {
            cell_id,
            width: 33,
            height: 33,
            heights: vec![0.0; 33 * 33],
            normals: vec![0; 33 * 33 * 3],
            vertex_colors: Vec::new(),
            layers: Vec::new(),
            water_height: None,
            water_type_form_id: None,
        }],
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn terrain_streaming_app_with(
    database_path: &std::path::Path,
    cache_path: &std::path::Path,
    walk: bool,
) -> App {
    let catalogue_path = database_path.with_extension("catalogue");
    write_empty_catalogue(&catalogue_path);
    let mut app = App::new();
    let mut config = EngineConfig {
        // Plan only the camera's own full cell.
        stream_radius: 0,
        unload_radius: 1,
        ..EngineConfig::default()
    };
    config.headless = !walk;
    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin::default())
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .init_asset::<TerrainMaterial>()
        .init_asset::<WaterMaterial>()
        .insert_resource(config)
        .insert_resource(RenderOrigin(IVec2::new(2, -3)))
        .insert_resource(ActiveSpace {
            worldspace_id: Some(60),
            interior: None,
        })
        .insert_resource(CellCache::open(cache_path).unwrap())
        .insert_resource(AssetCatalog::open(&catalogue_path).unwrap())
        .init_resource::<ActorCatalog>()
        .insert_resource(WaterReflectionTexture(Handle::default()))
        .insert_resource(WorldDatabase::open(database_path).unwrap())
        .init_resource::<StreamingWorld>()
        .init_resource::<TerrainContinuity>()
        .init_resource::<StreamingMetrics>()
        .init_resource::<StreamingCommitBudget>()
        .init_resource::<ProfilingState>()
        .add_systems(Update, (plan_cells, collect_cells).chain());
    app
}

fn terrain_streaming_app(database_path: &std::path::Path, cache_path: &std::path::Path) -> App {
    terrain_streaming_app_with(database_path, cache_path, false)
}

fn request_fixture_cell(app: &mut App, key: CellKey) {
    let generation = {
        let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
        assert!(!streaming.cells.contains_key(&key));
        streaming.generation = streaming.generation.wrapping_add(1);
        let generation = streaming.generation;
        streaming
            .cells
            .insert(key, CellStatus::Loading { generation });
        streaming.outstanding_cells.insert(key, generation);
        generation
    };
    app.world()
        .resource::<WorldDatabase>()
        .request(DatabaseRequest::Load {
            generation,
            key,
            queued_at: Instant::now(),
        })
        .unwrap();
    app.world_mut()
        .resource_mut::<StreamingMetrics>()
        .requests_submitted += 1;
}

fn run_until<F: Fn(&App) -> bool>(app: &mut App, what: &str, done: F) {
    for _ in 0..600 {
        app.update();
        if done(app) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let streaming = app.world().resource::<StreamingWorld>();
    panic!(
        "timed out waiting for {what}; cells={:#?}; \
         outstanding={:?}; held_responses={}; queued_responses={}; metrics={:#?}",
        streaming.cells,
        streaming.outstanding_cells,
        streaming.held_responses.len(),
        app.world()
            .resource::<WorldDatabase>()
            .pending_cell_responses(),
        app.world().resource::<StreamingMetrics>(),
    );
}

fn hold_backoff(app: &mut App, key: CellKey) {
    let future = Instant::now() + Duration::from_secs(3600);
    let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
    if let Some(CellStatus::Failed {
        retry_at: Some(retry_at),
        ..
    }) = streaming.cells.get_mut(&key)
    {
        *retry_at = future;
    }
}

fn failed_full_app(database_path: &std::path::Path, cache_path: &std::path::Path) -> App {
    rusqlite::Connection::open(database_path)
        .unwrap()
        .execute_batch(r#"DROP TABLE "references";"#)
        .unwrap();
    terrain_streaming_app(database_path, cache_path)
}

fn restore_references_table(database_path: &std::path::Path) {
    rusqlite::Connection::open(database_path)
        .unwrap()
        .execute_batch(
            r#"CREATE TABLE "references"(id INTEGER PRIMARY KEY,cell_id INTEGER NOT NULL,worldspace_id INTEGER,base_form_id INTEGER NOT NULL,
                is_exterior INTEGER NOT NULL,pos_x REAL NOT NULL,pos_y REAL NOT NULL,pos_z REAL NOT NULL,local_x REAL,local_y REAL,
                rot_x REAL NOT NULL,rot_y REAL NOT NULL,rot_z REAL NOT NULL,scale REAL NOT NULL DEFAULT 1.0);"#,
        )
        .unwrap();
}

fn write_empty_cell_cache(path: &std::path::Path) {
    let cache = shared::CellCache {
        version: shared::CELL_CACHE_VERSION,
        cells: Vec::new(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn elapse_backoff(app: &mut App, key: CellKey) {
    let past = Instant::now() - Duration::from_millis(1);
    let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
    if let Some(CellStatus::Failed {
        retry_at: Some(retry_at),
        ..
    }) = streaming.cells.get_mut(&key)
    {
        *retry_at = past;
    }
}

fn roots_of(app: &mut App, cell_id: u32) -> usize {
    let mut roots = app
        .world_mut()
        .query_filtered::<&CellRef, With<StreamedCellRoot>>();
    roots
        .iter(app.world())
        .filter(|cell| cell.0 == cell_id)
        .count()
}

fn terrain_less_full_cell_app(validate: bool) -> (App, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_empty_cell_cache(&cache_path);
    let mut app = terrain_streaming_app(&database_path, &cache_path);
    if validate {
        app.add_systems(Update, validate_streaming_lifecycle.after(collect_cells));
    }
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    run_until(&mut app, "the full cell to be committed", |app| {
        matches!(
            app.world()
                .resource::<StreamingWorld>()
                .cells
                .get(&exterior_cell(2, -3)),
            Some(CellStatus::Resident { .. })
        )
    });
    (app, directory)
}

fn consume_mock_cell_answer(app: &mut App, key: CellKey) {
    let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
    if let Some(CellStatus::Loading { generation, .. }) = streaming.cells.get(&key) {
        let generation = *generation;
        streaming.consume_cell_response(key, generation);
    }
}

fn spawn_mock_interior(app: &mut App, cell_id: u32) -> Entity {
    let root = app
        .world_mut()
        .spawn((
            CellRef(cell_id),
            StreamedCellRoot,
            Transform::default(),
            Visibility::default(),
        ))
        .id();
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(CellKey::Interior(cell_id), CellStatus::Resident { root });
    root
}

fn streaming_test_app(config: EngineConfig) -> (App, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("world.db");
    write_plan_database(&path);
    let mut app = App::new();
    app.insert_resource(config)
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(ActiveSpace {
            worldspace_id: Some(60),
            interior: None,
        })
        .insert_resource(WorldDatabase::open(&path).unwrap())
        .init_resource::<StreamingWorld>()
        .init_resource::<StreamingMetrics>()
        .init_resource::<StreamingCommitBudget>()
        .init_resource::<lod::LodStreaming>()
        .init_resource::<TerrainContinuity>()
        .init_resource::<ActiveSpace>()
        .init_resource::<ProfilingState>()
        .add_systems(
            Update,
            (plan_cells, despawn_cells, validate_streaming_lifecycle).chain(),
        );
    (app, directory)
}

fn spawn_mock_cell(app: &mut App, grid_x: i32, grid_y: i32, resident: bool) -> Entity {
    let root = app
        .world_mut()
        .spawn((
            CellRef(cell_id_of(grid_x, grid_y)),
            StreamedCellRoot,
            ExteriorCellGrid(IVec2::new(grid_x, grid_y)),
            Transform::default(),
            Visibility::default(),
        ))
        .id();
    // The first child is the cell's landscape, as in a real cell: the lifecycle check expects
    // every resident full exterior root to hold a terrain patch.
    let child = app
        .world_mut()
        .spawn((Transform::default(), TerrainPatch, ChildOf(root)))
        .id();
    app.world_mut()
        .spawn((Transform::default(), ChildOf(child)));
    let status = if resident {
        CellStatus::Resident { root }
    } else {
        CellStatus::Retiring { root }
    };
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(exterior_cell(grid_x, grid_y), status);
    root
}

fn untextured_fixture(cell_id: u32, height: f32) -> TerrainSnapshot {
    let mut terrain = terrain_fixture(cell_id, height);
    terrain.layers.clear();
    terrain
}

fn cell_id_of(grid_x: i32, grid_y: i32) -> u32 {
    ((grid_x + 64) as u32) * 256 + (grid_y + 64) as u32
}

// This resource is consumed only by the lost-LAND proof. It does not replace the collector.
#[derive(Resource, Default)]
struct QueuedCells(Vec<(CellKey, TerrainSnapshot)>);

// Lifecycle CellAssets/spawn_cell harness uses the actual collector's native inputs.
#[expect(
    clippy::too_many_arguments,
    reason = "test Bevy system mirrors native cell-spawn inputs"
)]
fn spawn_queued_cells(
    mut commands: Commands,
    queued: Res<QueuedCells>,
    asset_server: Res<AssetServer>,
    catalog: Res<AssetCatalog>,
    reflection: Res<WaterReflectionTexture>,
    mut assets: CellAssets,
    mut streaming: ResMut<StreamingWorld>,
    mut profiler: ResMut<ProfilingState>,
) {
    for (key, terrain) in &queued.0 {
        let payload = CellPayload {
            generation: 1,
            key: *key,
            cell_id: terrain.cell_id,
            references: Vec::new(),
        };
        let root = spawn_cell(
            &mut commands,
            &asset_server,
            &catalog,
            &ActorCatalog::default(),
            &reflection,
            &mut assets.meshes,
            &mut assets.terrain_materials,
            &mut assets.water_materials,
            IVec2::ZERO,
            false,
            false,
            false,
            payload,
            Some(terrain.clone()),
            None,
            &mut 0,
            &mut profiler,
        )
        .expect("spawn the prior full terrain root through the real legacy spawner");
        streaming.cells.insert(*key, CellStatus::Resident { root });
    }
}

fn spawn_cells_app(cells: Vec<(CellKey, TerrainSnapshot)>, metrics: StreamingMetrics) -> App {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalogue.db");
    write_empty_catalogue(&path);
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin::default())
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .init_asset::<TerrainMaterial>()
        .init_asset::<WaterMaterial>()
        .insert_resource(EngineConfig::default())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(ActiveSpace {
            worldspace_id: Some(60),
            interior: None,
        })
        .insert_resource(AssetCatalog::open(&path).unwrap())
        .insert_resource(WaterReflectionTexture(Handle::default()))
        .insert_resource(QueuedCells(cells))
        .insert_resource(metrics)
        .init_resource::<StreamingWorld>()
        .init_resource::<TerrainContinuity>()
        .init_resource::<ProfilingState>()
        .add_systems(
            Update,
            (spawn_queued_cells, validate_streaming_lifecycle).chain(),
        );
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    app.update();
    app
}

// The reference table remains absent. Each accepted real full request must fail in collect_cells.
// Called only after preserving and checking the initial backoff state in the caller.
fn exhaust_remaining_failures(
    app: &mut App,
    key: CellKey,
    exits: &mut bevy::ecs::message::MessageCursor<bevy::app::AppExit>,
) -> usize {
    let mut error_exits = 0;
    for attempt in 2..=MAX_CELL_LOAD_ATTEMPTS {
        elapse_backoff(app, key);
        run_until(app, "terminal full-cell replacement/load failure", |app| {
            matches!(app.world().resource::<StreamingWorld>().cells.get(&key),
                Some(CellStatus::Failed { attempts, .. }) if *attempts == attempt)
        });
        error_exits += exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .filter(|exit| **exit == bevy::app::AppExit::error())
            .count();
        if attempt < MAX_CELL_LOAD_ATTEMPTS {
            assert_eq!(error_exits, 0);
            assert!(
                !app.world()
                    .contains_resource::<crate::streaming::failure::LoadingRefusal>()
            );
        }
    }
    assert!(app.world().resource::<StreamingWorld>().is_failed(key));
    error_exits
}

#[test]
fn transient_full_cell_failure_retries_without_terrain_or_terminal_refusal() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_terrain_cache(&cache_path, 1);
    let mut app = failed_full_app(&database_path, &cache_path);
    app.add_message::<bevy::app::AppExit>();
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    let key = exterior_cell(2, -3);
    run_until(&mut app, "the retry-eligible full-cell failure", |app| {
        matches!(
            app.world().resource::<StreamingWorld>().cells.get(&key),
            Some(CellStatus::Failed {
                attempts: 1,
                retry_at: Some(_),
                ..
            })
        )
    });
    assert!(app.world().resource::<StreamingWorld>().has_failed(&key));
    assert!(
        !app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>(),
        "a retry-eligible full-cell error must not stop the process before its retry"
    );
    let mut exits = bevy::ecs::message::MessageCursor::<bevy::app::AppExit>::default();
    assert_eq!(
        exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0
    );
    hold_backoff(&mut app, key);
    let submitted = app
        .world()
        .resource::<StreamingMetrics>()
        .requests_submitted;
    for _ in 0..8 {
        app.update();
    }
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        submitted,
        "backoff never requests a terrain-only substitute"
    );
    assert_eq!(roots_of(&mut app, 1), 0);
    let mut terrain = app.world_mut().query::<&TerrainPatch>();
    assert_eq!(terrain.iter(app.world()).count(), 0);
    restore_references_table(&database_path);
    elapse_backoff(&mut app, key);
    run_until(&mut app, "the healed full-cell retry", |app| {
        app.world().resource::<StreamingWorld>().is_resident(&key)
    });
    assert!(
        !app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .failed_cell_retries,
        1
    );
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .failed_cells_given_up,
        0
    );
    assert_eq!(roots_of(&mut app, 1), 1);
    assert_eq!(
        exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0
    );
}

#[test]
fn exhausted_full_cell_failure_refuses_once_without_terrain_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_terrain_cache(&cache_path, 1);
    let mut app = failed_full_app(&database_path, &cache_path);
    app.add_message::<bevy::app::AppExit>();
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    let key = exterior_cell(2, -3);
    let mut exits = bevy::ecs::message::MessageCursor::<bevy::app::AppExit>::default();
    let mut exit_count = 0;
    for attempt in 1..=MAX_CELL_LOAD_ATTEMPTS {
        run_until(&mut app, "the next failed full-cell attempt", |app| {
            matches!(app.world().resource::<StreamingWorld>().cells.get(&key),
                Some(CellStatus::Failed { attempts, .. }) if *attempts == attempt)
        });
        exit_count += exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .filter(|exit| **exit == bevy::app::AppExit::error())
            .count();
        if attempt < MAX_CELL_LOAD_ATTEMPTS {
            assert_eq!(exit_count, 0, "retryable attempts must remain alive");
            assert!(
                !app.world()
                    .contains_resource::<crate::streaming::failure::LoadingRefusal>()
            );
            elapse_backoff(&mut app, key);
        }
    }
    let streaming = app.world().resource::<StreamingWorld>();
    let Some(CellStatus::Failed {
        attempts,
        reason,
        retry_at,
    }) = streaming.cells.get(&key)
    else {
        panic!("exhausted full-cell failure must remain explicit")
    };
    assert_eq!(*attempts, MAX_CELL_LOAD_ATTEMPTS);
    assert!(retry_at.is_none());
    assert!(reason.contains("references"));
    let refusal = app
        .world()
        .resource::<crate::streaming::failure::LoadingRefusal>();
    assert!(refusal.0.contains(reason.as_str()));
    assert_eq!(exit_count, 1);
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .failed_cell_retries,
        u64::from(MAX_CELL_LOAD_RETRIES)
    );
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .failed_cells_given_up,
        1
    );
    assert_eq!(roots_of(&mut app, 1), 0);
    let mut terrain = app.world_mut().query::<&TerrainPatch>();
    assert_eq!(terrain.iter(app.world()).count(), 0);
    let submitted = app
        .world()
        .resource::<StreamingMetrics>()
        .requests_submitted;
    for _ in 0..8 {
        app.update();
    }
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        submitted
    );
    assert_eq!(
        exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0
    );
}

#[test]
fn retryable_full_cell_failure_is_pending_for_destination_and_world_readiness() {
    let key = exterior_cell(2, -3);
    let CellKey::Exterior {
        worldspace_id,
        grid_x,
        grid_y,
    } = key
    else {
        unreachable!()
    };
    let space = ActiveSpace {
        worldspace_id: Some(worldspace_id),
        interior: None,
    };
    let mut streaming = StreamingWorld::default();
    let mut metrics = StreamingMetrics::default();
    record_failed_cell(
        &mut streaming,
        key,
        None,
        1,
        "transient reference read".to_owned(),
        false,
        &mut metrics,
    );
    assert!(
        streaming.has_failed(&key),
        "the failed attempt remains diagnosable"
    );
    assert!(
        !streaming.is_failed(key),
        "backoff is not a terminal destination failure"
    );
    assert_eq!(
        streaming.active_residency(&space, worldspace_id, IVec2::new(grid_x, grid_y), 0),
        (1, 0, 0, 0),
        "retry backoff must not count as a settled world window"
    );
    let Some(CellStatus::Failed {
        reason,
        retry_at: Some(_),
        ..
    }) = streaming.cells.get(&key)
    else {
        panic!("a retryable failure must retain its reason and backoff")
    };
    assert_eq!(reason, "transient reference read");
    record_failed_cell(
        &mut streaming,
        key,
        None,
        MAX_CELL_LOAD_ATTEMPTS,
        "exhausted reference read".to_owned(),
        false,
        &mut metrics,
    );
    assert!(streaming.is_failed(key));
    assert_eq!(
        streaming.active_residency(&space, worldspace_id, IVec2::new(grid_x, grid_y), 0),
        (1, 0, 1, 0)
    );
    assert_eq!(metrics.failed_cells_given_up, 1);
}

#[test]
fn failed_full_cell_never_submits_terrain_only_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_terrain_cache(&cache_path, 1);
    let mut app = failed_full_app(&database_path, &cache_path);
    app.add_message::<bevy::app::AppExit>();
    let mut exits = bevy::ecs::message::MessageCursor::<bevy::app::AppExit>::default();
    let key = exterior_cell(2, -3);
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    run_until(&mut app, "the failed full cell", |app| {
        matches!(
            app.world().resource::<StreamingWorld>().cells.get(&key),
            Some(CellStatus::Failed {
                attempts: 1,
                retry_at: Some(_),
                ..
            })
        )
    });
    hold_backoff(&mut app, key);
    let submitted = app
        .world()
        .resource::<StreamingMetrics>()
        .requests_submitted;
    let mut error_exits = exits
        .read(app.world().resource::<Messages<bevy::app::AppExit>>())
        .filter(|exit| **exit == bevy::app::AppExit::error())
        .count();
    for _ in 0..8 {
        app.update();
        error_exits += exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .filter(|exit| **exit == bevy::app::AppExit::error())
            .count();
    }
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        submitted,
        "a failed full cell must not submit an alternate terrain-only request during backoff"
    );
    assert_eq!(
        roots_of(&mut app, 1),
        0,
        "a failed first load has no resident recovery root"
    );
    let mut terrain = app.world_mut().query::<&TerrainPatch>();
    assert_eq!(
        terrain.iter(app.world()).count(),
        0,
        "failure must not draw terrain-only recovery"
    );
    let streaming = app.world().resource::<StreamingWorld>();
    assert!(streaming.has_failed(&key));
    let Some(CellStatus::Failed {
        attempts,
        reason,
        retry_at,
        ..
    }) = streaming.cells.get(&key)
    else {
        panic!("the failed cell must remain explicit")
    };
    assert_eq!(*attempts, 1);
    assert!(
        reason.contains("references"),
        "the original database error remains visible: {reason}"
    );
    assert!(retry_at.is_some_and(|retry_at| retry_at > Instant::now()));
    let metrics = app.world().resource::<StreamingMetrics>();
    assert_eq!(metrics.failed_cells, 1);
    assert_eq!(metrics.failed_cell_retries, 0);
    assert_eq!(metrics.failed_cells_given_up, 0);
    assert!(
        !app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
    assert_eq!(error_exits, 0, "initial retry backoff has no terminal exit");
    let first_reason = reason.clone();
    error_exits += exhaust_remaining_failures(&mut app, key, &mut exits);
    let streaming = app.world().resource::<StreamingWorld>();
    let Some(CellStatus::Failed {
        reason,
        retry_at: None,
        ..
    }) = streaming.cells.get(&key)
    else {
        panic!("exhaustion must be terminal");
    };
    assert_eq!(
        reason, &first_reason,
        "the terminal read retains the original diagnostic"
    );
    let refusal = app
        .world()
        .get_resource::<crate::streaming::failure::LoadingRefusal>()
        .expect("an exhausted full-cell failure must take the explicit loading refusal path");
    assert!(
        refusal.0.contains(reason.as_str()),
        "the refusal must retain the failure reason"
    );
    assert_eq!(
        error_exits, 1,
        "the terminal failure emits one error exit after preserving initial retry state"
    );
    app.update();
    assert_eq!(
        exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0,
        "quiet failed frames do not emit another exit"
    );
}

#[test]
fn a_failed_full_load_preserves_its_neighbor_and_failure() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_terrain_cache(&cache_path, 1);
    let mut app = terrain_streaming_app(&database_path, &cache_path);
    app.add_message::<bevy::app::AppExit>();
    let mut exits = bevy::ecs::message::MessageCursor::<bevy::app::AppExit>::default();
    let prior_key = exterior_cell(2, -3);
    let key = exterior_cell(1, -3);
    request_fixture_cell(&mut app, prior_key);
    run_until(&mut app, "initial full root", |app| {
        app.world()
            .resource::<StreamingWorld>()
            .is_resident(&prior_key)
    });
    let root = match app.world().resource::<StreamingWorld>().cells[&prior_key] {
        CellStatus::Resident { root } => root,
        _ => unreachable!(),
    };
    assert_eq!(roots_of(&mut app, 1), 1);
    assert_eq!(app.world().resource::<StreamingMetrics>().failed_cells, 0);
    let original_east = app.world().resource::<TerrainContinuity>().edges[&prior_key]
        .east
        .clone();
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute_batch(r#"DROP TABLE "references";"#)
        .unwrap();
    app.world_mut()
        .spawn((Transform::from_xyz(-CELL_SIZE, 0.0, 0.0), StreamingCamera));
    request_fixture_cell(&mut app, key);
    run_until(&mut app, "failed full load", |app| {
        app.world().resource::<StreamingWorld>().has_failed(&key)
    });
    hold_backoff(&mut app, key);
    assert_eq!(
        app.world()
            .resource::<TerrainContinuity>()
            .edges
            .get(&prior_key)
            .map(|e| &e.east),
        Some(&original_east),
        "failed cell must preserve its neighbor's terrain authority"
    );
    let mut neighbor = terrain_fixture(2, 16.0);
    app.world_mut()
        .resource_scope(|world, mut continuity: Mut<TerrainContinuity>| {
            let mut metrics = world.resource_mut::<StreamingMetrics>();
            validate_and_register_terrain_edges(
                exterior_cell(3, -3),
                &mut neighbor,
                &mut continuity,
                &mut metrics,
            )
            .unwrap();
        });
    assert_eq!(
        TerrainEdges::of(&neighbor).west,
        original_east,
        "native welding must still use the resident neighbor after a load fails"
    );
    let submitted = app
        .world()
        .resource::<StreamingMetrics>()
        .requests_submitted;
    for _ in 0..40 {
        app.update();
        assert!(app.world().get::<StreamedCellRoot>(root).is_some());
        assert_eq!(
            roots_of(&mut app, 1),
            1,
            "the neighboring full root remains resident without duplicates"
        );
    }
    let streaming = app.world().resource::<StreamingWorld>();
    assert!(streaming.has_failed(&key));
    assert!(
        !streaming.is_resident(&key),
        "a neighboring resident root does not make the failed cell resident"
    );
    let Some(CellStatus::Failed {
        attempts,
        reason,
        retry_at,
    }) = streaming.cells.get(&key)
    else {
        panic!("load failure must remain explicit")
    };
    assert_eq!(*attempts, 1);
    assert!(reason.contains("no such table: references"), "{reason}");
    assert_eq!(streaming.resident_root(prior_key), Some(root));
    assert!(retry_at.is_some_and(|at| at > Instant::now()));
    let metrics = app.world().resource::<StreamingMetrics>();
    assert_eq!(metrics.requests_submitted, submitted);
    assert_eq!(metrics.failed_cells, 1);
    assert!(
        !app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
    assert_eq!(
        exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0
    );
    let first_reason = reason.clone();
    let error_exits = exhaust_remaining_failures(&mut app, key, &mut exits);
    assert_eq!(error_exits, 1);
    assert!(app.world().get::<StreamedCellRoot>(root).is_some());
    assert_eq!(roots_of(&mut app, 1), 1);
    let streaming = app.world().resource::<StreamingWorld>();
    let Some(CellStatus::Failed {
        reason,
        retry_at: None,
        ..
    }) = streaming.cells.get(&key)
    else {
        panic!("exhausted failure must retain its terminal reason");
    };
    assert_eq!(streaming.resident_root(prior_key), Some(root));
    assert_eq!(reason, &first_reason);
    assert!(
        app.world()
            .resource::<crate::streaming::failure::LoadingRefusal>()
            .0
            .contains(reason)
    );
    app.update();
    assert_eq!(
        exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0
    );
}

#[test]
fn a_full_cell_with_an_invalid_landscape_remains_an_explicit_failure() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_invalid_terrain_cache(&cache_path, 1);
    let mut app = terrain_streaming_app(&database_path, &cache_path);
    let key = exterior_cell(2, -3);
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    run_until(&mut app, "invalid full landscape", |app| {
        matches!(
            app.world().resource::<StreamingWorld>().cells.get(&key),
            Some(CellStatus::Failed {
                attempts: 1,
                retry_at: None,
                ..
            })
        )
    });
    assert_eq!(roots_of(&mut app, 1), 0);
    assert!(app.world().resource::<StreamingWorld>().has_failed(&key));
    let metrics = app.world().resource::<StreamingMetrics>();
    assert_eq!(metrics.failed_cells, 1);
    assert_eq!(metrics.failed_cells_given_up, 1);
    assert_eq!(metrics.terrain_validation_failures, 1);
    assert_eq!(metrics.asset_failures.len(), 1);
    assert!(
        metrics.asset_failures[0]
            .dependency_chain
            .iter()
            .any(|reason| reason.contains("normal"))
    );
    assert!(
        app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
}

#[test]
fn a_full_exterior_cell_without_terrain_is_counted_once_and_stays_resident() {
    let (mut app, _directory) = terrain_less_full_cell_app(false);
    // More frames: a counter bumped by anything but the commit would keep climbing.
    for _ in 0..10 {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    let key = exterior_cell(2, -3);
    let root = match app.world().resource::<StreamingWorld>().cells.get(&key) {
        Some(CellStatus::Resident { root }) => *root,
        other => panic!("the terrain-less cell is not resident in full: {other:?}"),
    };
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .full_cells_without_terrain,
        1
    );
    assert_eq!(
        app.world().resource::<StreamingMetrics>().failed_cells,
        0,
        "the cell is held, not failed"
    );
    assert!(
        app.world().get::<AuthoredWithoutLand>(root).is_some(),
        "the counted root carries the marker the lifecycle check reads"
    );
    assert_eq!(roots_of(&mut app, 1), 1);
    let patches = app
        .world_mut()
        .query_filtered::<(), With<TerrainPatch>>()
        .iter(app.world())
        .count();
    assert_eq!(patches, 0, "no terrain was answered, so none is drawn");
}

#[test]
fn the_lifecycle_check_accepts_a_counted_terrain_less_cell() {
    let (mut app, _directory) = terrain_less_full_cell_app(true);
    // The validator runs after the commit that made the cell resident.
    app.update();
    let metrics = app.world().resource::<StreamingMetrics>();
    assert_eq!(
        metrics.resident_roots, 1,
        "the check saw the terrain-less root"
    );
    assert_eq!(metrics.full_cells_without_terrain, 1);
    assert_eq!(metrics.streaming_invariant_failures, 0);
}

#[test]
fn the_lifecycle_check_flags_a_full_exterior_root_that_lost_its_terrain() {
    let key = exterior_cell(2, -3);
    let mut app = spawn_cells_app(
        vec![(key, untextured_fixture(1, 100.0))],
        StreamingMetrics::default(),
    );
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .streaming_invariant_failures,
        0,
        "a cell holding its landscape is fine"
    );
    // The cell is already spawned and recorded; spawn nothing more.
    app.world_mut().resource_mut::<QueuedCells>().0.clear();
    let patches: Vec<Entity> = app
        .world_mut()
        .query_filtered::<Entity, With<TerrainPatch>>()
        .iter(app.world())
        .collect();
    assert_eq!(patches.len(), 4);
    for patch in patches {
        app.world_mut().entity_mut(patch).despawn();
    }
    app.update();
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .streaming_invariant_failures,
        1,
        "an uncounted full exterior root without terrain is a violation"
    );
}

#[test]
fn a_failed_full_load_is_retried_after_its_backoff_and_becomes_resident() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_terrain_cache(&cache_path, 1);
    let mut app = failed_full_app(&database_path, &cache_path);
    let key = exterior_cell(2, -3);
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    run_until(&mut app, "first full failure", |app| {
        matches!(
            app.world().resource::<StreamingWorld>().cells.get(&key),
            Some(CellStatus::Failed {
                attempts: 1,
                retry_at: Some(_),
                ..
            })
        )
    });
    {
        let metrics = app.world().resource::<StreamingMetrics>();
        assert_eq!(metrics.failed_cells, 1);
        assert_eq!(metrics.failed_cell_retries, 0);
        assert_eq!(metrics.failed_cells_given_up, 0);
    }
    hold_backoff(&mut app, key);
    let submitted = app
        .world()
        .resource::<StreamingMetrics>()
        .requests_submitted;
    for _ in 0..5 {
        app.update();
    }
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        submitted
    );
    assert_eq!(roots_of(&mut app, 1), 0);
    let Some(CellStatus::Failed {
        attempts,
        reason,
        retry_at,
        ..
    }) = app.world().resource::<StreamingWorld>().cells.get(&key)
    else {
        panic!("missing failure")
    };
    assert_eq!(*attempts, 1);
    assert!(!reason.is_empty());
    assert!(retry_at.is_some_and(|at| at > Instant::now()));
    restore_references_table(&database_path);
    elapse_backoff(&mut app, key);
    run_until(&mut app, "retried full cell", |app| {
        app.world().resource::<StreamingWorld>().is_resident(&key)
    });
    assert_eq!(roots_of(&mut app, 1), 1);
    let streaming = app.world().resource::<StreamingWorld>();
    assert!(!streaming.has_failed(&key));
    assert!(
        !streaming.failed_retries.contains_key(&key),
        "success clears the retry history"
    );
    let metrics = app.world().resource::<StreamingMetrics>();
    assert_eq!(metrics.failed_cells, 1);
    assert_eq!(metrics.failed_cell_retries, 1);
    assert_eq!(metrics.failed_cells_given_up, 0);
}

#[test]
fn a_cell_that_keeps_failing_stops_after_the_attempt_budget() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_terrain_cache(&cache_path, 1);
    let mut app = failed_full_app(&database_path, &cache_path);
    let key = exterior_cell(2, -3);
    let camera = app
        .world_mut()
        .spawn((Transform::default(), StreamingCamera))
        .id();
    for attempt in 1..=MAX_CELL_LOAD_ATTEMPTS {
        if attempt > 1 {
            elapse_backoff(&mut app, key);
        }
        run_until(&mut app, &format!("attempt {attempt}"), |app| {
            matches!(
            app.world().resource::<StreamingWorld>().cells.get(&key),
            Some(CellStatus::Failed { attempts, .. }) if *attempts == attempt)
        });
        assert_eq!(
            roots_of(&mut app, 1),
            0,
            "failure never allocates terrain-only recovery"
        );
    }
    let Some(CellStatus::Failed {
        attempts,
        reason,
        retry_at,
    }) = app.world().resource::<StreamingWorld>().cells.get(&key)
    else {
        panic!("missing failure")
    };
    assert_eq!(*attempts, MAX_CELL_LOAD_ATTEMPTS);
    assert!(!reason.is_empty());
    assert!(retry_at.is_none());
    let metrics = app.world().resource::<StreamingMetrics>();
    assert_eq!(metrics.failed_cells, u64::from(MAX_CELL_LOAD_ATTEMPTS));
    assert_eq!(
        metrics.failed_cell_retries,
        u64::from(MAX_CELL_LOAD_ATTEMPTS - 1)
    );
    assert_eq!(metrics.failed_cells_given_up, 1);
    let submitted = metrics.requests_submitted;
    for _ in 0..10 {
        app.update();
    }
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        submitted
    );
    assert_eq!(
        app.world().resource::<StreamingMetrics>().failed_cells,
        u64::from(MAX_CELL_LOAD_ATTEMPTS)
    );
    assert!(app.world().resource::<StreamingWorld>().has_failed(&key));
    // Leaving the active space forgets the exhausted visit; returning starts a fresh budget.
    app.world_mut().resource_mut::<ActiveSpace>().interior = Some(INTERIOR_CELL);
    app.update();
    assert!(
        !app.world()
            .resource::<StreamingWorld>()
            .cells
            .contains_key(&key)
    );
    assert!(
        !app.world()
            .resource::<StreamingWorld>()
            .failed_retries
            .contains_key(&key)
    );
    app.world_mut().resource_mut::<ActiveSpace>().interior = None;
    app.world_mut()
        .entity_mut(camera)
        .insert(Transform::default());
    run_until(&mut app, "fresh visit's first failure", |app| {
        matches!(
            app.world().resource::<StreamingWorld>().cells.get(&key),
            Some(CellStatus::Failed {
                attempts: 1,
                retry_at: Some(_),
                ..
            })
        )
    });
}

#[test]
fn invalid_terrain_data_is_not_retried() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_invalid_terrain_cache(&cache_path, 1);
    let mut app = terrain_streaming_app(&database_path, &cache_path);
    app.add_message::<bevy::app::AppExit>();
    let mut exits = bevy::ecs::message::MessageCursor::<bevy::app::AppExit>::default();
    let key = exterior_cell(2, -3);
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));

    run_until(&mut app, "the full request's failure", |app| {
        matches!(
            app.world().resource::<StreamingWorld>().cells.get(&key),
            Some(CellStatus::Failed {
                attempts: 1,
                retry_at: None,
                ..
            })
        )
    });
    let CellStatus::Failed { reason, .. } = app
        .world()
        .resource::<StreamingWorld>()
        .cells
        .get(&key)
        .unwrap()
    else {
        unreachable!("checked above");
    };
    assert!(
        reason.contains("normal"),
        "the reason names the invalid data: {reason}"
    );
    {
        let metrics = app.world().resource::<StreamingMetrics>();
        assert_eq!(metrics.failed_cells, 1);
        assert_eq!(metrics.failed_cell_retries, 0, "no retry was scheduled");
        assert_eq!(metrics.failed_cells_given_up, 1);
        assert_eq!(metrics.terrain_validation_failures, 1);
        assert_eq!(metrics.asset_failures.len(), 1);
    }

    assert_eq!(roots_of(&mut app, 1), 0);
    assert!(
        app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
    assert_eq!(
        exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .filter(|exit| **exit == bevy::app::AppExit::error())
            .count(),
        1
    );
    let requests_quiet = app
        .world()
        .resource::<StreamingMetrics>()
        .requests_submitted;
    for _ in 0..10 {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        requests_quiet,
        "a cell with invalid data is not re-queried"
    );
    assert_eq!(
        exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0
    );
}

#[test]
fn an_exterior_full_failure_is_failed_at_once() {
    let mut streaming = StreamingWorld {
        active_interior: Some(INTERIOR_CELL),
        ..StreamingWorld::default()
    };
    let key = exterior_cell(0, 0);
    streaming.cells.insert(
        key,
        CellStatus::Failed {
            attempts: 1,
            reason: "the cell could not be streamed".to_owned(),
            retry_at: Some(Instant::now() + Duration::from_secs(1)),
        },
    );
    assert!(streaming.has_failed(&key));
    assert!(!streaming.is_resident(&key));
    streaming.cells.remove(&key);
    assert!(
        !streaming.has_failed(&key),
        "leaving the plan forgets the failure"
    );
}

#[test]
fn a_failed_interior_is_asked_for_again_while_the_camera_stands_in_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("world.db");
    write_plan_database(&path);
    let mut app = App::new();
    app.insert_resource(EngineConfig::default())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(ActiveSpace {
            worldspace_id: Some(60),
            interior: Some(INTERIOR_CELL),
        })
        .insert_resource(WorldDatabase::open(&path).unwrap())
        .init_resource::<StreamingWorld>()
        .init_resource::<TerrainContinuity>()
        .init_resource::<StreamingMetrics>()
        .init_resource::<StreamingCommitBudget>()
        .init_resource::<ProfilingState>()
        .add_systems(Update, plan_cells);
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    let status = |app: &App| match app
        .world()
        .resource::<StreamingWorld>()
        .cells
        .get(&CellKey::Interior(INTERIOR_CELL))
    {
        Some(CellStatus::Loading { .. }) => "loading".to_owned(),
        Some(CellStatus::Resident { .. }) => "resident".to_owned(),
        Some(CellStatus::Retiring { .. }) => "retiring".to_owned(),
        Some(CellStatus::Failed { .. }) => "failed".to_owned(),
        Some(CellStatus::Absent) => "absent".to_owned(),
        None => "gone".to_owned(),
    };
    // What a failed answer leaves behind: the record `record_failed_cell` writes for a
    // transient database error, with the backoff already paid so the next plan pass may ask
    // again.
    let answer_failed = |app: &mut App| {
        consume_mock_cell_answer(app, CellKey::Interior(INTERIOR_CELL));
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(
                CellKey::Interior(INTERIOR_CELL),
                CellStatus::Failed {
                    attempts: 1,
                    reason: "the interior could not be streamed".to_owned(),
                    retry_at: Some(Instant::now()),
                },
            );
    };

    answer_failed(&mut app);
    for attempt in 1..=MAX_CELL_LOAD_RETRIES {
        app.update();
        assert_eq!(
            status(&app),
            "loading",
            "the interior the camera stands in is asked for again, attempt {attempt}"
        );
        answer_failed(&mut app);
    }
    // The budget is spent: the last answer fails and the plan leaves the cell alone.
    app.update();
    assert_eq!(
        status(&app),
        "failed",
        "a cell whose data really is not there is not re-requested every frame"
    );
    app.update();
    assert_eq!(status(&app), "failed", "and it stays that way");
}

#[test]
fn a_retried_interior_is_not_failed_until_its_retries_run_out() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("world.db");
    write_plan_database(&path);
    let mut app = App::new();
    app.insert_resource(EngineConfig::default())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(ActiveSpace {
            worldspace_id: Some(60),
            interior: Some(INTERIOR_CELL),
        })
        .insert_resource(WorldDatabase::open(&path).unwrap())
        .init_resource::<StreamingWorld>()
        .init_resource::<TerrainContinuity>()
        .init_resource::<StreamingMetrics>()
        .init_resource::<StreamingCommitBudget>()
        .init_resource::<ProfilingState>()
        .add_systems(Update, plan_cells);
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    let key = CellKey::Interior(INTERIOR_CELL);
    let has_failed = |app: &App| app.world().resource::<StreamingWorld>().has_failed(&key);
    // What a failed answer leaves behind: the record `record_failed_cell` writes for a
    // transient database error, with the backoff already paid so the next plan pass may ask
    // again.
    let answer_failed = |app: &mut App| {
        consume_mock_cell_answer(app, CellKey::Interior(INTERIOR_CELL));
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(
                key,
                CellStatus::Failed {
                    attempts: 1,
                    reason: "the interior could not be streamed".to_owned(),
                    retry_at: Some(Instant::now()),
                },
            );
    };

    // The first request, and every retry, fails: in each gap before the next retry the cell
    // is waited for, not given up on.
    app.update();
    for attempt in 0..=MAX_CELL_LOAD_RETRIES {
        answer_failed(&mut app);
        let retries_spent = attempt == MAX_CELL_LOAD_RETRIES;
        assert_eq!(
            has_failed(&app),
            retries_spent,
            "after failure {} of {}",
            attempt + 1,
            MAX_CELL_LOAD_RETRIES + 1
        );
        app.update();
    }
    assert!(has_failed(&app), "its budget spent, the failure is final");

    // The camera steps out to the worldspace: the same failed cell, no longer the interior it
    // stands in, is never retried, so its failure is final at once - whatever budget it has.
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .failed_retries
        .clear();
    *app.world_mut().resource_mut::<ActiveSpace>() = ActiveSpace {
        worldspace_id: Some(60),
        interior: None,
    };
    app.update();
    answer_failed(&mut app);
    assert!(
        has_failed(&app),
        "an interior the camera is not in is failed at its first failure"
    );
}

#[test]
fn a_retiring_cell_keeps_its_root_until_disposal_or_revival() {
    let (mut app, _directory) = streaming_test_app(crossing_config(1));
    spawn_camera(&mut app, IVec2::ZERO);
    let key = exterior_cell(7, 0);
    let root = spawn_mock_cell(&mut app, 7, 0, true);
    // A resident cell closer in the budget's order takes the frame's one unload.
    let decoy = spawn_mock_cell(&mut app, 6, 0, true);
    app.update();

    assert!(
        app.world().get_entity(decoy).is_err(),
        "the decoy took the only unload this frame"
    );
    assert!(
        matches!(app.world().resource::<StreamingWorld>().cells.get(&key),
        Some(CellStatus::Retiring { root: now }) if *now == root)
    );
    assert!(
        app.world().get_entity(root).is_ok(),
        "its root must not be despawned while it waits for the budget"
    );

    // Returning revives the retained full root without another allocation.
    move_camera(&mut app, IVec2::new(7, 0));
    app.update();
    assert!(
        matches!(app.world().resource::<StreamingWorld>().cells.get(&key),
        Some(CellStatus::Resident { root: now }) if *now == root)
    );
    assert!(app.world().get_entity(root).is_ok());
    assert_eq!(streaming_metrics(&app).despawns_this_frame, 0);
}

#[test]
fn a_pinned_retiring_cell_is_revived_with_its_root() {
    let (mut app, _directory) = streaming_test_app(crossing_config(1));
    spawn_camera(&mut app, IVec2::ZERO);
    // Native lifecycle paces all retiring roots. Model an interior left for another active
    // space, so the ordinary planner does not revive it before the disposal proof.
    app.world_mut().resource_mut::<ActiveSpace>().interior = Some(100);
    let unpinned = spawn_mock_interior(&mut app, 98);
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(
            CellKey::Interior(98),
            CellStatus::Retiring { root: unpinned },
        );
    app.update();
    assert!(
        app.world().get_entity(unpinned).is_err(),
        "an interior left behind goes at once, whatever state it was in"
    );
    assert_eq!(streaming_metrics(&app).despawns_this_frame, 1);

    // The same cell again, but pinned this frame: `request_cell` takes it out of the backlog.
    let root = spawn_mock_interior(&mut app, 99);
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(CellKey::Interior(99), CellStatus::Retiring { root });
    let revived = {
        let world = app.world_mut();
        let database = world.remove_resource::<WorldDatabase>().unwrap();
        let mut metrics = world.remove_resource::<StreamingMetrics>().unwrap();
        let mut profiler = world.remove_resource::<ProfilingState>().unwrap();
        world.resource_mut::<StreamingWorld>().request_cell(
            &database,
            CellKey::Interior(99),
            &mut metrics,
            &mut profiler,
        );
        let revived = metrics.revived_cells;
        world.insert_resource(database);
        world.insert_resource(metrics);
        world.insert_resource(profiler);
        revived
    };
    assert_eq!(revived, 1, "pinning a retiring cell revives it on the spot");
    app.update();
    assert!(
        app.world().get_entity(root).is_ok(),
        "the pinned cell kept the root it was drawn by"
    );
    assert!(
        matches!(
            app.world()
                .resource::<StreamingWorld>()
                .cells
                .get(&CellKey::Interior(99)),
            Some(CellStatus::Resident { root: now, .. }) if *now == root
        ),
        "the pinned cell is resident under the same root"
    );
    let metrics = streaming_metrics(&app);
    assert_eq!(metrics.despawns_this_frame, 0);
    assert_eq!(metrics.retiring_cells, 0);
    assert_eq!(metrics.unloaded_cells, 1, "only the unpinned one went");
    assert_eq!(metrics.streaming_invariant_failures, 0);
}

#[test]
fn pinned_retiring_full_cell_restores_saved_visibility_before_readiness() {
    let (mut app, _directory) = streaming_test_app(crossing_config(1));
    spawn_camera(&mut app, IVec2::ZERO);
    let root = spawn_mock_interior(&mut app, 99);
    app.world_mut()
        .entity_mut(root)
        .insert((Visibility::Hidden, RetireHidden(Visibility::Visible)));
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(CellKey::Interior(99), CellStatus::Retiring { root });
    {
        let world = app.world_mut();
        let database = world.remove_resource::<WorldDatabase>().unwrap();
        let mut metrics = world.remove_resource::<StreamingMetrics>().unwrap();
        let mut profiler = world.remove_resource::<ProfilingState>().unwrap();
        world.resource_mut::<StreamingWorld>().request_cell(
            &database,
            CellKey::Interior(99),
            &mut metrics,
            &mut profiler,
        );
        world.insert_resource(database);
        world.insert_resource(metrics);
        world.insert_resource(profiler);
    }
    app.update();
    assert!(app.world().get_entity(root).is_ok());
    assert_eq!(
        app.world().get::<Visibility>(root),
        Some(&Visibility::Visible),
        "pinning must revive the real root's saved visibility"
    );
    assert!(app.world().get::<RetireHidden>(root).is_none());
    assert_eq!(
        app.world()
            .resource::<StreamingWorld>()
            .resident_root(CellKey::Interior(99)),
        Some(root)
    );
}

#[test]
fn leaving_a_failed_cell_starts_a_fresh_attempt_budget_on_immediate_return() {
    let (mut app, _directory) = streaming_test_app(crossing_config(1));
    spawn_camera(&mut app, IVec2::ZERO);
    let key = exterior_cell(20, 0);
    {
        let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
        streaming.failed_retries.insert(key, 1);
        streaming.cells.insert(
            key,
            CellStatus::Failed {
                attempts: 2,
                reason: "old visit".to_owned(),
                retry_at: Some(Instant::now() + Duration::from_secs(60)),
            },
        );
    }
    app.update();
    assert!(
        !app.world()
            .resource::<StreamingWorld>()
            .cells
            .contains_key(&key)
    );
    move_camera(&mut app, IVec2::new(20, 0));
    app.update();
    assert!(matches!(
        app.world().resource::<StreamingWorld>().cells.get(&key),
        Some(CellStatus::Loading { .. })
    ));
    assert_eq!(
        next_full_attempt(app.world().resource::<StreamingWorld>(), &key),
        1,
        "new visit gets its initial attempt and both retries"
    );
}

#[test]
fn an_absent_exterior_square_settles_without_retry_refusal_or_terrain() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute_batch("DELETE FROM land; DELETE FROM cells;")
        .unwrap();
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_empty_cell_cache(&cache_path);
    let mut app = terrain_streaming_app(&database_path, &cache_path);
    app.add_message::<bevy::app::AppExit>();
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    let key = exterior_cell(2, -3);
    run_until(
        &mut app,
        "the absent square's completed database response",
        |app| {
            app.world()
                .resource::<StreamingMetrics>()
                .requests_submitted
                > 0
                && app
                    .world()
                    .resource::<StreamingWorld>()
                    .outstanding_cells
                    .is_empty()
        },
    );
    assert!(
        !app.world().resource::<StreamingWorld>().has_failed(&key),
        "an absent exterior record must never enter failed-cell retry state"
    );
    assert!(
        !app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
    let submitted = app
        .world()
        .resource::<StreamingMetrics>()
        .requests_submitted;
    for _ in 0..8 {
        elapse_backoff(&mut app, key);
        app.update();
    }
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        submitted,
        "settled empty space is never retried"
    );
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .failed_cell_retries,
        0
    );
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .failed_cells_given_up,
        0
    );
    assert_eq!(roots_of(&mut app, 1), 0);
    let mut terrain = app.world_mut().query::<&TerrainPatch>();
    assert_eq!(terrain.iter(app.world()).count(), 0);
    let mut roots = app
        .world_mut()
        .query_filtered::<Entity, With<StreamedCellRoot>>();
    assert_eq!(
        roots.iter(app.world()).count(),
        0,
        "absence never fabricates an empty cell root"
    );
    let world = app.world();
    let (ready, failed) = crate::pacing::world_ready_at_frame_end(
        world.resource::<EngineConfig>(),
        world.resource::<RenderOrigin>().0,
        world.resource::<StreamingWorld>(),
        world.resource::<ActiveSpace>(),
        world.resource::<StreamingMetrics>(),
        Vec3::ZERO,
        0,
        None,
    );
    assert!(ready, "a verified empty window is settled");
    assert_eq!(failed, 0);
    let mut exits = bevy::ecs::message::MessageCursor::<bevy::app::AppExit>::default();
    assert_eq!(
        exits
            .read(world.resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0
    );
}

#[test]
fn a_pinned_inactive_interior_retries_and_refuses_once_without_waiting_for_a_door_timeout() {
    use bevy::ecs::system::RunSystemOnce;
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("world.db");
    write_terrain_database(&database_path);
    let cache_path = directory.path().join("cell_cache.rkyv");
    write_empty_cell_cache(&cache_path);
    let mut app = failed_full_app(&database_path, &cache_path);
    app.add_message::<bevy::app::AppExit>();
    app.world_mut()
        .spawn((Transform::default(), StreamingCamera));
    app.world_mut().resource_mut::<ActiveSpace>().interior = Some(100);
    spawn_mock_interior(&mut app, 100);
    let key = CellKey::Interior(99);
    app.world_mut()
        .run_system_once(
            move |mut streaming: ResMut<StreamingWorld>,
                  database: Res<WorldDatabase>,
                  mut metrics: ResMut<StreamingMetrics>,
                  mut profiler: ResMut<ProfilingState>| {
                assert!(streaming.request_cell(&database, key, &mut metrics, &mut profiler))
            },
        )
        .unwrap();
    run_until(
        &mut app,
        "the explicitly demanded interior's first real read error",
        |app| {
            matches!(
                app.world().resource::<StreamingWorld>().cells.get(&key),
                Some(CellStatus::Failed {
                    attempts: 1,
                    retry_at: Some(_),
                    ..
                })
            )
        },
    );
    assert!(
        !app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
    let first = app
        .world()
        .resource::<StreamingMetrics>()
        .requests_submitted;
    elapse_backoff(&mut app, key);
    app.update();
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .requests_submitted,
        first + 1,
        "a pinned destination is still demanded while another interior is active"
    );
    let mut exits = bevy::ecs::message::MessageCursor::<bevy::app::AppExit>::default();
    assert_eq!(exhaust_remaining_failures(&mut app, key, &mut exits), 1);
    assert!(
        app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
    for _ in 0..8 {
        app.update();
    }
    assert_eq!(
        exits
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0
    );
}

#[test]
fn precommit_terrain_unwind_restores_prior_edges_or_explicit_absence() {
    let key = exterior_cell(2, -3);
    let other = exterior_cell(3, -3);
    let original = TerrainEdges::of(&terrain_fixture(1, 0.0));
    let changed = TerrainEdges::of(&terrain_fixture(1, 32.0));
    let mut continuity = TerrainContinuity::default();
    continuity.edges.insert(key, changed.clone());
    continuity.edges.insert(other, changed.clone());

    continuity.restore(key, Some(original.clone()));
    for side in TerrainEdgeSide::ALL {
        assert_eq!(continuity.edges[&key].side(side), original.side(side));
    }
    // No prior LAND is an explicit absent authority, not permission to retain new edges.
    continuity.restore(key, None);
    assert!(!continuity.edges.contains_key(&key));
    for side in TerrainEdgeSide::ALL {
        assert_eq!(continuity.edges[&other].side(side), changed.side(side));
    }
}
