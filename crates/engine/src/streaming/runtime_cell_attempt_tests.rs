//! Cell-attempt reservation and root-accounting regressions.
#![cfg(test)]
use super::*;

fn replacement_catalog() -> StreamingCostCatalog {
    let mut catalog = known_catalog();
    catalog.generated.full_cell_terrain = ByteEstimate::conservative(100, 10, "terrain fixture");
    catalog.generated.full_cell_water = ByteEstimate::conservative(20, 2, "water fixture");
    catalog.generated.full_cell_collision =
        ByteEstimate::conservative(300, 30, "collision fixture");
    catalog.resources.insert(
        "textures/new.ktx2".into(),
        ResourceCost::new(
            ResourceKind::Texture,
            ByteEstimate::conservative(64, 4, "new texture fixture"),
        ),
    );
    catalog
}

fn replacement_surface(app: &mut App) -> (Entity, Handle<Mesh>, Handle<TerrainMaterial>) {
    let mesh = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::default());
    let material = app
        .world_mut()
        .resource_mut::<Assets<TerrainMaterial>>()
        .add(TerrainMaterial {
            base: StandardMaterial::default(),
            extension: default(),
        });
    let root = app.world_mut().spawn_empty().id();
    app.world_mut().spawn((
        TerrainPatch,
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material.clone()),
        ChildOf(root),
    ));
    (root, mesh, material)
}

fn bind_replacement(
    app: &mut App,
    generation: u64,
    physics: bool,
    root: Entity,
    paths: &[&str],
) -> String {
    let key = cell_key(0);
    let resource = {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        assert!(runtime.reserve_cell_attempt_with_textures(
            key,
            generation,
            physics,
            paths.iter().map(|path| (*path).into()).collect()
        ));
        runtime.bind_cell_attempt(key, generation, root);
        runtime.cells[&root].resource.clone()
    };
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(key, CellStatus::Resident { root });
    resource
}

// These fixtures keep A physically alive to check allocation accounting independently of
// the failed request's status. Only actual entity disposal releases A's runtime ownership.
fn retry_failed_cell(app: &mut App) {
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(
            cell_key(0),
            CellStatus::Failed {
                attempts: 1,
                reason: "fixture full-cell load failed".into(),
                retry_at: Some(Instant::now()),
            },
        );
}

fn watch_fixture_texture(app: &mut App, path: &str) -> Handle<Image> {
    let image = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
    let resource = runtime.resource_key(path, None);
    runtime.watches.get_mut(&resource).unwrap().demand.images = vec![image.id()];
    image
}

fn replacement_preparation_transition(prepared_a: bool) {
    let (mut app, _directory) = fixture(true);
    app.world_mut().resource_mut::<StreamingRuntime>().catalog = replacement_catalog();
    let (a, mesh_a, material_a) = replacement_surface(&mut app);
    let image_a = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    app.world_mut()
        .resource_mut::<Assets<TerrainMaterial>>()
        .get_mut(material_a.id())
        .unwrap()
        .base
        .base_color_texture = Some(image_a.clone());
    let resource_a = bind_replacement(&mut app, 1, false, a, &["textures/shared.ktx2"]);
    let _shared = watch_fixture_texture(&mut app, "textures/shared.ktx2");
    let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
    bridge.publish_fixture(1, vec![], true);
    observe_at(&mut app, 1);
    let demand_a = app.world().resource::<StreamingRuntime>().watches[&resource_a]
        .demand
        .clone();
    assert_eq!(demand_a.meshes, vec![mesh_a.id()]);
    assert_eq!(demand_a.images, vec![image_a.id()]);
    assert_eq!(
        demand_a.materials,
        vec![bevy::asset::UntypedAssetId::from(material_a.id())]
    );
    let a_demands = watched_demands(&app);
    if prepared_a {
        bridge.publish_fixture(2, a_demands.clone(), true);
        observe_at(&mut app, 2);
        assert_eq!(
            app.world()
                .resource::<StreamingRuntime>()
                .ledger
                .totals()
                .transient_bytes,
            0
        );
    }
    let before = app.world().resource::<StreamingRuntime>().ledger.totals();
    let (b, mesh_b, material_b) = replacement_surface(&mut app);
    let image_b = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    app.world_mut()
        .resource_mut::<Assets<TerrainMaterial>>()
        .get_mut(material_b.id())
        .unwrap()
        .base
        .base_color_texture = Some(image_b.clone());
    app.world_mut()
        .entity_mut(b)
        .insert(bevy_rapier3d::prelude::Collider::ball(1.0));
    {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        let extra_resident = MIB + 100 + 20 + 300 + 64;
        let extra_transient = MIB + 10 + 2 + 30 + 4 + if prepared_a { 32 } else { 0 };
        runtime.memory_limit =
            Some(before.peak_bytes().unwrap() + extra_resident + extra_transient - 1);
        assert!(!runtime.reserve_cell_attempt_with_textures(
            cell_key(0),
            2,
            true,
            vec!["textures/shared.ktx2".into(), "textures/new.ktx2".into()]
        ));
        assert!(runtime.pending_cells.is_empty());
        assert_eq!(runtime.ledger.totals().peak_bytes(), before.peak_bytes());
        assert_eq!(
            runtime.ledger.totals().denied_total,
            before.denied_total + 1
        );
        runtime.memory_limit =
            Some(before.peak_bytes().unwrap() + extra_resident + extra_transient);
        assert!(runtime.reserve_cell_attempt_with_textures(
            cell_key(0),
            2,
            true,
            vec!["textures/shared.ktx2".into(), "textures/new.ktx2".into()]
        ));
        let admitted = runtime.ledger.totals();
        assert_eq!(
            admitted.resident_bytes - before.resident_bytes,
            MIB + 100 + 20 + 300 + 64
        );
        assert_eq!(
            admitted.transient_bytes - before.transient_bytes,
            MIB + 10 + 2 + 30 + 4 + if prepared_a { 32 } else { 0 }
        );
        assert_eq!(admitted.owners, 2);
        assert_eq!(
            admitted.resources, 4,
            "two generated allocations and two texture identities"
        );
        assert!(runtime.reserve_cell_attempt(cell_key(0), 2, true));
        assert!(runtime.reserve_cell_attempt_with_textures(
            cell_key(0),
            2,
            true,
            vec!["textures/shared.ktx2".into(), "textures/new.ktx2".into()]
        ));
        assert_eq!(
            runtime.ledger.totals(),
            admitted,
            "planner/collect repetition must be idempotent"
        );
        runtime.bind_cell_attempt(cell_key(0), 2, b);
    }
    let _new = watch_fixture_texture(&mut app, "textures/new.ktx2");
    let resource_b = app.world().resource::<StreamingRuntime>().cells[&b]
        .resource
        .clone();
    {
        let runtime = app.world().resource::<StreamingRuntime>();
        assert_ne!(resource_a, resource_b);
        assert_ne!(
            runtime.watches[&resource_a].demand.key,
            runtime.watches[&resource_b].demand.key
        );
        assert_eq!(runtime.watches[&resource_a].root_entity, Some(a));
        assert_eq!(runtime.watches[&resource_b].root_entity, Some(b));
        assert!(!runtime.cells[&b].dependencies_cached);
        assert!(!runtime.prepared_resources.contains(&resource_b));
    }
    bridge.publish_fixture(3, a_demands, true);
    observe_at(&mut app, 3);
    let pending = app
        .world()
        .resource::<StreamingRuntime>()
        .ledger
        .totals()
        .transient_bytes;
    {
        let runtime = app.world().resource::<StreamingRuntime>();
        assert!(runtime.prepared_resources.contains(&resource_a));
        assert!(!runtime.prepared_resources.contains(&resource_b));
        assert_eq!(runtime.watches[&resource_a].demand, demand_a);
        assert_eq!(
            runtime.watches[&resource_b].demand.meshes,
            vec![mesh_b.id()]
        );
        assert_eq!(
            runtime.watches[&resource_b].demand.images,
            vec![image_b.id()]
        );
        assert_eq!(
            runtime.watches[&resource_b].demand.materials,
            vec![bevy::asset::UntypedAssetId::from(material_b.id())]
        );
        assert!(pending >= MIB + 42 + 4);
    }
    let new_texture_demand = {
        let runtime = app.world().resource::<StreamingRuntime>();
        runtime.watches[&runtime.resource_key("textures/new.ktx2", None)]
            .demand
            .key
    };
    let without_new_texture: Vec<_> = watched_demands(&app)
        .into_iter()
        .filter(|demand| demand.key != new_texture_demand)
        .collect();
    bridge.publish_fixture(4, without_new_texture, true);
    observe_at(&mut app, 4);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .transient_bytes,
        pending
    );
    bridge.publish_fixture(5, watched_demands(&app), true);
    observe_at(&mut app, 5);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .transient_bytes,
        pending,
        "GPU readiness alone cannot release collision preparation headroom"
    );
    // Publish the installation marker with a real collider-set handle; no physics simulation runs.
    let mut colliders = bevy_rapier3d::rapier::geometry::ColliderSet::new();
    let handle =
        colliders.insert(bevy_rapier3d::rapier::geometry::ColliderBuilder::ball(1.0).build());
    app.world_mut()
        .entity_mut(b)
        .insert(bevy_rapier3d::prelude::RapierColliderHandle(handle));
    observe_at(&mut app, 6);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(runtime.prepared_resources.contains(&resource_b));
    assert_eq!(runtime.ledger.totals().transient_bytes, 0);
    assert_eq!(
        runtime.ledger.totals().resident_bytes,
        2 * (MIB + 120) + 300 + 512 + 64
    );
}

#[test]
fn failed_load_status_does_not_release_a_live_surface_allocation() {
    let (mut app, _directory) = fixture(true);
    let key = cell_key(0);
    let root = app.world_mut().spawn_empty().id();
    {
        let mut runtime = app.world_mut().resource_mut::<StreamingRuntime>();
        runtime.catalog = known_catalog();
        assert!(runtime.reserve_cell(key, false));
        runtime.bind_cell(key, root);
    }
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(
            key,
            CellStatus::Failed {
                attempts: 1,
                reason: "full reference query failed".into(),
                retry_at: None,
            },
        );
    let reserved = app.world().resource::<StreamingRuntime>().ledger.totals();
    assert!(reserved.resident_bytes > 0);
    observe_at(&mut app, 1);
    observe_at(&mut app, 4);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(runtime.cells.contains_key(&root));
    assert_eq!(
        runtime.ledger.totals().resident_bytes,
        reserved.resident_bytes
    );
    assert_eq!(runtime.ledger.totals().orphan_resident_bytes, 0);

    app.world_mut().despawn(root);
    observe_at(&mut app, 5);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(!runtime.cells.contains_key(&root));
    assert_eq!(
        runtime.ledger.totals().orphan_resident_bytes,
        reserved.resident_bytes
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
}

#[test]
fn prepared_full_cell_replacement_reserves_collision_and_new_preparation() {
    replacement_preparation_transition(true);
}

#[test]
fn cached_unprepared_replacement_requires_its_exact_dependencies_and_new_texture() {
    replacement_preparation_transition(false);
}

#[test]
fn retry_denied_in_planner_and_collect_keeps_live_allocations_and_leaks_no_pending_owner() {
    use bevy::ecs::system::RunSystemOnce;
    let (mut app, _directory, requests, responses) = cell_pipeline_fixture(4);
    app.world_mut().resource_mut::<StreamingRuntime>().catalog = replacement_catalog();
    let (a, _mesh, _material) = replacement_surface(&mut app);
    let resource_a = bind_replacement(&mut app, 0, false, a, &[]);
    retry_failed_cell(&mut app);
    observe_at(&mut app, 1);
    let funded = app.world().resource::<StreamingRuntime>().ledger.totals();
    app.world_mut()
        .resource_mut::<StreamingRuntime>()
        .memory_limit = Some(funded.resident_bytes);
    app.world_mut()
        .run_system_once(crate::streaming::plan_cells)
        .unwrap();
    assert!(requests.is_empty());
    assert!(matches!(
        app.world().resource::<StreamingWorld>().cells[&cell_key(0)],
        CellStatus::Failed {
            attempts: 1,
            retry_at: Some(_),
            ..
        }
    ));
    {
        let runtime = app.world().resource::<StreamingRuntime>();
        assert!(runtime.pending_cells.is_empty());
        assert_eq!(runtime.cells.len(), 1);
        assert_eq!(
            runtime.ledger.totals().resident_bytes,
            funded.resident_bytes
        );
        assert_eq!(
            runtime.ledger.totals().denied_total,
            funded.denied_total + 1
        );
    }
    // Direct/pinned consumers can reach collect without planner admission.
    {
        let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
        streaming
            .cells
            .insert(cell_key(0), CellStatus::Loading { generation: 7 });
        streaming.outstanding_cells.insert(cell_key(0), 7);
    }
    responses.send(returned_cell(7, cell_key(0))).unwrap();
    for _ in 0..2 {
        app.world_mut()
            .resource_mut::<crate::streaming::StreamingCommitBudget>()
            .remaining = 1;
        app.world_mut()
            .run_system_once(crate::streaming::collect_cells)
            .unwrap();
        let runtime = app.world().resource::<StreamingRuntime>();
        assert!(runtime.pending_cells.is_empty());
        assert_eq!(runtime.cells[&a].resource, resource_a);
        assert_eq!(runtime.ledger.totals().peak_bytes(), funded.peak_bytes());
        assert_eq!(runtime.deferred_responses.len(), 1);
        assert_eq!(runtime.ledger.totals().orphan_bytes(), Some(0));
        assert!(app.world().get_entity(a).is_ok());
    }
    app.world_mut()
        .resource_mut::<StreamingRuntime>()
        .memory_limit = Some(64 * MIB);
    app.world_mut()
        .resource_mut::<crate::streaming::StreamingCommitBudget>()
        .remaining = 1;
    app.world_mut()
        .run_system_once(crate::streaming::collect_cells)
        .unwrap();
    let CellStatus::Resident { root: b, .. } =
        app.world().resource::<StreamingWorld>().cells[&cell_key(0)]
    else {
        panic!("funded deferred response must commit");
    };
    let runtime = app.world().resource::<StreamingRuntime>();
    assert_ne!(a, b);
    assert_eq!(runtime.cells.len(), 2);
    assert_ne!(runtime.cells[&a].resource, runtime.cells[&b].resource);
    assert!(runtime.pending_cells.is_empty());
    assert!(runtime.deferred_responses.is_empty());
}

#[test]
fn failed_pending_attempt_retires_only_its_reservation_and_real_retry_binds_a_fresh_owner() {
    use bevy::ecs::system::RunSystemOnce;
    let (mut app, _directory, requests, responses) = cell_pipeline_fixture(4);
    app.world_mut().resource_mut::<StreamingRuntime>().catalog = replacement_catalog();
    let (a, _mesh, _material) = replacement_surface(&mut app);
    let resource_a = bind_replacement(&mut app, 0, false, a, &[]);
    retry_failed_cell(&mut app);
    observe_at(&mut app, 1);
    let demand_a = app.world().resource::<StreamingRuntime>().watches[&resource_a]
        .demand
        .clone();
    app.world_mut()
        .run_system_once(crate::streaming::plan_cells)
        .unwrap();
    let DatabaseRequest::Load {
        generation, key, ..
    } = requests.try_recv().unwrap()
    else {
        panic!("expected full-cell retry");
    };
    let failed_resource = app.world().resource::<StreamingRuntime>().pending_cells
        [&CellAttempt { key, generation }]
        .resource
        .clone();
    let mut failure = returned_cell(generation, key);
    failure.result = Err("fixture reference read failed".into());
    responses.send(failure).unwrap();
    app.world_mut()
        .resource_mut::<crate::streaming::StreamingCommitBudget>()
        .remaining = 1;
    app.world_mut()
        .run_system_once(crate::streaming::collect_cells)
        .unwrap();
    assert!(matches!(
        app.world().resource::<StreamingWorld>().cells[&key],
        CellStatus::Failed {
            attempts: 2,
            retry_at: Some(_),
            ..
        }
    ));
    {
        let streaming = app.world().resource::<StreamingWorld>();
        let CellStatus::Failed { reason, .. } = &streaming.cells[&key] else {
            unreachable!();
        };
        assert_eq!(reason, "fixture reference read failed");
    }
    {
        let runtime = app.world().resource::<StreamingRuntime>();
        assert!(runtime.pending_cells.is_empty());
        assert_eq!(runtime.cells.len(), 1);
        assert_eq!(runtime.cells[&a].resource, resource_a);
        assert_eq!(runtime.watches[&resource_a].demand, demand_a);
        assert!(runtime.watches[&failed_resource].orphaned_frame.is_some());
        assert!(runtime.ledger.resource_has_owners(&resource_a));
        assert!(!runtime.ledger.resource_has_owners(&failed_resource));
    }
    observe_at(&mut app, 2);
    observe_at(&mut app, 4);
    assert!(
        !app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .contains_resource(&failed_resource)
    );
    {
        let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
        let Some(CellStatus::Failed { retry_at, .. }) = streaming.cells.get_mut(&key) else {
            panic!("transient failure must remain visible during backoff");
        };
        *retry_at = Some(Instant::now() + std::time::Duration::from_secs(60));
    }
    app.world_mut()
        .run_system_once(crate::streaming::plan_cells)
        .unwrap();
    assert!(
        requests.is_empty(),
        "backoff must not dispatch a terrain rescue"
    );
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .pending_cells
            .is_empty()
    );
    assert!(matches!(
        app.world().resource::<StreamingWorld>().cells[&key],
        CellStatus::Failed { attempts: 2, .. }
    ));
    {
        let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
        let Some(CellStatus::Failed { retry_at, .. }) = streaming.cells.get_mut(&key) else {
            panic!("failed load must retain its retry state");
        };
        *retry_at = Some(Instant::now());
    }
    app.world_mut()
        .run_system_once(crate::streaming::plan_cells)
        .unwrap();
    let DatabaseRequest::Load {
        generation: retry, ..
    } = requests.try_recv().unwrap()
    else {
        panic!("expected restored Full request");
    };
    assert_ne!(retry, generation);
    responses.send(returned_cell(retry, key)).unwrap();
    app.world_mut()
        .resource_mut::<crate::streaming::StreamingCommitBudget>()
        .remaining = 1;
    app.world_mut()
        .run_system_once(crate::streaming::collect_cells)
        .unwrap();
    let CellStatus::Resident { root: b } = app.world().resource::<StreamingWorld>().cells[&key]
    else {
        panic!("restored Full response must commit");
    };
    let runtime = app.world().resource::<StreamingRuntime>();
    assert_eq!(runtime.cells.len(), 2);
    assert_eq!(runtime.cells[&a].resource, resource_a);
    assert_ne!(runtime.cells[&b].resource, resource_a);
    assert_ne!(runtime.cells[&b].resource, failed_resource);
    assert!(!runtime.cells[&b].dependencies_cached);
    assert!(runtime.pending_cells.is_empty());
    assert!(app.world().get_entity(a).is_ok());
}

#[test]
fn deterministic_validation_failure_preserves_live_allocations_and_releases_pending_attempt() {
    use bevy::ecs::system::RunSystemOnce;
    let (mut app, directory, requests, responses) = cell_pipeline_fixture(4);
    let path = directory.path().join("invalid-replacement.rkyv");
    std::fs::write(
        &path,
        rkyv::to_bytes::<rkyv::rancor::Error>(&shared::CellCache {
            version: shared::CELL_CACHE_VERSION,
            cells: vec![shared::CachedLand {
                cell_id: 1,
                width: 33,
                height: 33,
                heights: vec![f32::NAN; 33 * 33],
                normals: vec![0; 33 * 33 * 3],
                vertex_colors: vec![],
                layers: vec![],
                water_height: None,
                water_type_form_id: None,
            }],
        })
        .unwrap(),
    )
    .unwrap();
    app.insert_resource(crate::world::cache::CellCache::open(&path).unwrap());
    app.world_mut().resource_mut::<StreamingRuntime>().catalog = replacement_catalog();
    let (a, _mesh, _material) = replacement_surface(&mut app);
    let resource_a = bind_replacement(&mut app, 0, false, a, &[]);
    retry_failed_cell(&mut app);
    app.world_mut()
        .run_system_once(crate::streaming::plan_cells)
        .unwrap();
    let DatabaseRequest::Load {
        generation, key, ..
    } = requests.try_recv().unwrap()
    else {
        panic!("expected full-cell retry");
    };
    responses.send(returned_cell(generation, key)).unwrap();
    app.world_mut()
        .resource_mut::<crate::streaming::StreamingCommitBudget>()
        .remaining = 1;
    app.world_mut()
        .run_system_once(crate::streaming::collect_cells)
        .unwrap();
    assert!(matches!(
        app.world().resource::<StreamingWorld>().cells[&key],
        CellStatus::Failed { retry_at: None, .. }
    ));
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .terrain_validation_failures,
        1
    );
    let runtime = app.world().resource::<StreamingRuntime>();
    assert!(runtime.pending_cells.is_empty());
    assert_eq!(runtime.cells.len(), 1);
    assert!(runtime.ledger.resource_has_owners(&resource_a));
    assert!(app.world().get_entity(a).is_ok());
    app.world_mut()
        .run_system_once(crate::streaming::plan_cells)
        .unwrap();
    assert!(
        requests.is_empty(),
        "deterministic failure must not retry or dispatch terrain rescue"
    );
    assert!(matches!(
        app.world().resource::<StreamingWorld>().cells[&key],
        CellStatus::Failed { retry_at: None, .. }
    ));
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .pending_cells
            .is_empty()
    );
}

#[test]
fn replacement_roots_dispose_independently_and_shared_texture_claim_survives_either_order() {
    for dispose_new_first in [false, true] {
        let (mut app, _directory) = fixture(true);
        app.world_mut().resource_mut::<StreamingRuntime>().catalog = replacement_catalog();
        let (a, mesh_a, material_a) = replacement_surface(&mut app);
        let resource_a = bind_replacement(&mut app, 1, false, a, &["textures/shared.ktx2"]);
        let _shared = watch_fixture_texture(&mut app, "textures/shared.ktx2");
        let (b, mesh_b, material_b) = replacement_surface(&mut app);
        let resource_b = bind_replacement(&mut app, 2, true, b, &["textures/shared.ktx2"]);
        for material in [&material_a, &material_b] {
            let image = app
                .world_mut()
                .resource_mut::<Assets<Image>>()
                .add(Image::default());
            app.world_mut()
                .resource_mut::<Assets<TerrainMaterial>>()
                .get_mut(material.id())
                .unwrap()
                .base
                .base_color_texture = Some(image);
        }
        observe_at(&mut app, 1);
        let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
        bridge.publish_fixture(2, watched_demands(&app), true);
        observe_at(&mut app, 2);
        assert_eq!(
            app.world()
                .resource::<StreamingRuntime>()
                .ledger
                .totals()
                .resident_bytes,
            2 * (MIB + 120) + 300 + 512
        );
        let (disposed, retained, resource, retained_resource, mesh, material) = if dispose_new_first
        {
            (b, a, resource_b, resource_a, mesh_b, material_b)
        } else {
            (a, b, resource_a, resource_b, mesh_a, material_a)
        };
        app.world_mut().despawn(disposed);
        observe_at(&mut app, 3);
        bridge.publish_absent_fixture(4, watched_demands(&app));
        observe_at(&mut app, 6);
        // Exact CPU IDs alone prevent reclamation, even with GPU absence.
        assert!(
            app.world()
                .resource::<StreamingRuntime>()
                .ledger
                .contains_resource(&resource)
        );
        app.world_mut()
            .resource_mut::<Assets<Mesh>>()
            .remove(mesh.id());
        app.world_mut()
            .resource_mut::<Assets<TerrainMaterial>>()
            .remove(material.id());
        observe_at(&mut app, 6);
        assert!(
            app.world()
                .resource::<StreamingRuntime>()
                .ledger
                .contains_resource(&resource),
            "the disposed root's generated image alone must retain its allocation watch"
        );
        let image_ids = app.world().resource::<StreamingRuntime>().watches[&resource]
            .demand
            .images
            .clone();
        assert_eq!(image_ids.len(), 1);
        for id in image_ids {
            app.world_mut().resource_mut::<Assets<Image>>().remove(id);
        }
        bridge.publish_fixture(7, watched_demands(&app), true);
        observe_at(&mut app, 7);
        // CPU absence alone cannot reclaim an exact GPU allocation still present.
        assert!(
            app.world()
                .resource::<StreamingRuntime>()
                .ledger
                .contains_resource(&resource)
        );
        bridge.publish_absent_fixture(8, watched_demands(&app));
        observe_at(&mut app, 8);
        let runtime = app.world().resource::<StreamingRuntime>();
        assert!(!runtime.ledger.contains_resource(&resource));
        assert!(runtime.cells.contains_key(&retained));
        assert_eq!(
            runtime.watches[&retained_resource].root_entity,
            Some(retained)
        );
        assert!(runtime.ledger.resource_has_owners(&retained_resource));
        let texture = runtime.resource_key("textures/shared.ktx2", None);
        assert!(runtime.ledger.resource_has_owners(&texture));
        assert_eq!(runtime.ledger.totals().orphan_bytes(), Some(0));
        assert_eq!(
            runtime.ledger.totals().resident_bytes,
            MIB + 120 + if dispose_new_first { 0 } else { 300 } + 512
        );
    }
}

#[test]
fn rapid_replacements_retain_every_root_and_same_root_revival_does_not_allocate() {
    use bevy::ecs::system::RunSystemOnce;
    let (mut app, _directory, requests, _responses) = cell_pipeline_fixture(4);
    app.world_mut().resource_mut::<StreamingRuntime>().catalog = replacement_catalog();
    let mut roots = Vec::new();
    let mut resources = HashSet::new();
    // Three full-cell allocations; collision ownership varies independently.
    for (generation, physics) in [(1, false), (2, true), (3, false)] {
        let (root, _mesh, _material) = replacement_surface(&mut app);
        resources.insert(bind_replacement(&mut app, generation, physics, root, &[]));
        roots.push(root);
    }
    observe_at(&mut app, 1);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert_eq!(resources.len(), 3);
    assert_eq!(runtime.cells.len(), 3);
    assert_eq!(
        runtime.ledger.totals().resident_bytes,
        3 * (MIB + 120) + 300
    );
    for root in &roots {
        assert_eq!(
            runtime.watches[&runtime.cells[root].resource].root_entity,
            Some(*root)
        );
    }
    let before = runtime.ledger.totals();
    let root = roots[2];
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .cells
        .insert(cell_key(0), CellStatus::Retiring { root });
    // Revive the same full-cell root without issuing another request.
    app.world_mut()
        .run_system_once(crate::streaming::plan_cells)
        .unwrap();
    assert!(
        matches!(app.world().resource::<StreamingWorld>().cells[&cell_key(0)],
        CellStatus::Resident { root: revived } if revived == root)
    );
    assert!(requests.is_empty());
    assert_eq!(
        app.world().resource::<StreamingRuntime>().ledger.totals(),
        before
    );
    assert_eq!(app.world().resource::<StreamingRuntime>().cells.len(), 3);
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .pending_cells
            .is_empty()
    );
    let after_revival: HashSet<_> = app
        .world()
        .resource::<StreamingRuntime>()
        .cells
        .values()
        .map(|cell| cell.resource.clone())
        .collect();
    assert_eq!(after_revival, resources);
    for (root, disposed_at, reclaimed_at, expected_resident) in [
        (roots[1], 2, 4, 2 * (MIB + 120)),
        (roots[0], 5, 7, MIB + 120),
    ] {
        let demand = {
            let runtime = app.world().resource::<StreamingRuntime>();
            runtime.watches[&runtime.cells[&root].resource]
                .demand
                .clone()
        };
        app.world_mut().despawn(root);
        for id in demand.meshes {
            app.world_mut().resource_mut::<Assets<Mesh>>().remove(id);
        }
        for id in demand.materials {
            app.world_mut()
                .resource_mut::<Assets<TerrainMaterial>>()
                .remove(id.typed_debug_checked::<TerrainMaterial>());
        }
        observe_at(&mut app, disposed_at);
        assert!(
            app.world()
                .resource::<StreamingRuntime>()
                .ledger
                .totals()
                .orphan_resident_bytes
                > 0
        );
        observe_at(&mut app, reclaimed_at);
        let runtime = app.world().resource::<StreamingRuntime>();
        assert_eq!(runtime.ledger.totals().resident_bytes, expected_resident);
        assert_eq!(runtime.ledger.totals().orphan_resident_bytes, 0);
        assert!(runtime.cells.contains_key(&roots[2]));
    }
}

#[test]
fn refused_retry_queue_retires_pending_attempt_without_releasing_live_allocations() {
    use bevy::ecs::system::RunSystemOnce;
    let (mut app, _directory, requests, _responses) = cell_pipeline_fixture(1);
    app.world_mut().resource_mut::<StreamingRuntime>().catalog = replacement_catalog();
    let (a, _mesh, _material) = replacement_surface(&mut app);
    let resource_a = bind_replacement(&mut app, 0, false, a, &[]);
    retry_failed_cell(&mut app);
    observe_at(&mut app, 1);
    let resident_a = app
        .world()
        .resource::<StreamingRuntime>()
        .ledger
        .totals()
        .resident_bytes;
    app.world()
        .resource::<WorldDatabase>()
        .try_request(DatabaseRequest::Load {
            generation: 99,
            key: CellKey::Interior(99),
            queued_at: Instant::now(),
        })
        .unwrap();
    app.world_mut()
        .run_system_once(crate::streaming::plan_cells)
        .unwrap();
    {
        let runtime = app.world().resource::<StreamingRuntime>();
        assert!(runtime.pending_cells.is_empty());
        assert_eq!(runtime.cells[&a].resource, resource_a);
        assert!(runtime.ledger.resource_has_owners(&resource_a));
        assert!(runtime.ledger.totals().orphan_resident_bytes > 0);
    }
    assert!(
        app.world()
            .resource::<StreamingWorld>()
            .outstanding_cells
            .is_empty()
    );
    assert!(matches!(
        app.world().resource::<StreamingWorld>().cells[&cell_key(0)],
        CellStatus::Failed {
            attempts: 1,
            retry_at: Some(_),
            ..
        }
    ));
    observe_at(&mut app, 2);
    observe_at(&mut app, 4);
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .resident_bytes,
        resident_a
    );
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .orphan_bytes(),
        Some(0)
    );
    requests.try_recv().unwrap();
    app.world_mut()
        .run_system_once(crate::streaming::plan_cells)
        .unwrap();
    assert_eq!(
        app.world()
            .resource::<StreamingRuntime>()
            .pending_cells
            .len(),
        1
    );
    assert!(matches!(
        app.world().resource::<StreamingWorld>().cells[&cell_key(0)],
        CellStatus::Loading { .. }
    ));
    assert_eq!(
        app.world().resource::<StreamingRuntime>().cells[&a].resource,
        resource_a
    );
}

#[derive(TypePath)]
struct SurfaceDemandImageLoader;

impl bevy::asset::AssetLoader for SurfaceDemandImageLoader {
    type Asset = Image;
    type Settings = bevy::image::ImageLoaderSettings;
    type Error = std::io::Error;

    async fn load(
        &self,
        _: &mut dyn bevy::asset::io::Reader,
        settings: &Self::Settings,
        _: &mut bevy::asset::LoadContext<'_>,
    ) -> Result<Image, Self::Error> {
        let mut image = Image {
            sampler: settings.sampler.clone(),
            ..default()
        };
        image.texture_descriptor.format = if settings.is_srgb {
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb
        } else {
            bevy::render::render_resource::TextureFormat::Rgba8Unorm
        };
        Ok(image)
    }

    fn extensions(&self) -> &[&str] {
        &["ktx2"]
    }
}

#[test]
fn failed_optional_normal_writer_refreshes_cell_demand_and_releases_transient_ownership() {
    use crate::world::{cache::TerrainSnapshot, database::AssetCatalog};
    use bevy::ecs::system::RunSystemOnce;
    let (mut app, directory) = fixture(true);
    app.register_asset_loader(SurfaceDemandImageLoader);
    std::fs::create_dir_all(directory.path().join("textures")).unwrap();
    std::fs::write(directory.path().join("textures/shared.ktx2"), []).unwrap();
    let db = directory.path().join("surface-catalog.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TABLE texture_sets(id INTEGER PRIMARY KEY,diffuse_path TEXT,normal_path TEXT);
         CREATE TABLE landscape_textures(id INTEGER PRIMARY KEY,texture_set_id INTEGER);
         CREATE TABLE waters(id INTEGER PRIMARY KEY,flow_normal_path TEXT);
         INSERT INTO texture_sets VALUES(2,'textures/shared.ktx2','textures/missing-normal.ktx2');
         INSERT INTO landscape_textures VALUES(1,2);",
        )
        .unwrap();
    let catalog = AssetCatalog::open(&db).unwrap();
    let terrain = TerrainSnapshot {
        cell_id: 1,
        width: 33,
        height: 33,
        heights: vec![0.0; 33 * 33],
        normals: [0, 0, 127].repeat(33 * 33),
        vertex_colors: vec![255; 33 * 33 * 3],
        layers: vec![crate::world::cache::TerrainLayerSnapshot {
            texture_form_id: 1,
            quadrant: 0,
            layer: 0,
            is_base: true,
            weights: Vec::new(),
        }],
        water_height: None,
        water_type_form_id: None,
    };
    let (extension, inputs) = crate::render::TerrainExtension::from_quadrant(
        &terrain,
        0,
        &catalog,
        app.world().resource::<AssetServer>(),
    )
    .unwrap();
    assert_eq!(inputs.color.len(), 1);
    assert_eq!(inputs.normal.len(), 1);
    let diffuse = inputs.color[0].clone();
    let normal = inputs.normal[0].clone();
    let generated = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(Image::default());
    let (root, _mesh, material) = replacement_surface(&mut app);
    let patch = app.world().get::<Children>(root).unwrap()[0];
    {
        let mut materials = app.world_mut().resource_mut::<Assets<TerrainMaterial>>();
        let mut material = materials.get_mut(material.id()).unwrap();
        material.extension = extension;
        material.base.emissive_texture = Some(generated.clone());
    }
    app.world_mut().entity_mut(patch).insert((
        crate::streaming::PendingTerrainProfile {
            cell_id: 1,
            quadrant: 0,
            images: inputs.color,
            normals: inputs.normal,
        },
        crate::swap_bridge::SwapSurface::new(material.id().into(), [diffuse.id(), normal.id()]),
        crate::swap_bridge::SwapWaiting,
    ));
    app.world_mut().resource_mut::<StreamingRuntime>().catalog = replacement_catalog();
    let resource = bind_replacement(&mut app, 1, false, root, &["textures/shared.ktx2"]);
    let bridge = app.world().resource::<StreamingPreparationBridge>().clone();
    bridge.publish_fixture(1, vec![], false);
    observe_at(&mut app, 1);
    assert!(
        app.world().resource::<StreamingRuntime>().watches[&resource]
            .demand
            .images
            .contains(&normal.id())
    );
    assert!(
        app.world()
            .resource::<StreamingRuntime>()
            .ledger
            .totals()
            .transient_bytes
            > 0
    );

    // Asset loading is real; preparation observations below are test-owned protocol evidence.
    // Wait on terminal loader state, with a deadline and scheduler yield rather than a fixed sleep.
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    loop {
        app.update();
        let server = app.world().resource::<AssetServer>();
        if server.is_loaded_with_dependencies(diffuse.id())
            && matches!(
                server.get_load_state(normal.id()),
                Some(LoadState::Failed(_))
            )
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "surface fixture did not resolve diffuse and fail the absent normal"
        );
        std::thread::yield_now();
    }
    app.world_mut()
        .run_system_once(crate::streaming::track_surface_readiness)
        .unwrap();
    assert!(
        app.world()
            .get::<crate::streaming::PendingTerrainProfile>(patch)
            .is_none()
    );
    assert!(
        app.world()
            .get::<crate::streaming::TerrainSurfaceReady>(patch)
            .is_some()
    );
    assert!(
        !app.world()
            .resource::<Assets<TerrainMaterial>>()
            .get(material.id())
            .unwrap()
            .extension
            .image_ids()
            .any(|id| id == normal.id())
    );
    observe_at(&mut app, 2);
    {
        let runtime = app.world().resource::<StreamingRuntime>();
        let demand = &runtime.watches[&resource].demand;
        let mut expected = vec![diffuse.id(), generated.id()];
        expected.sort_unstable();
        assert_eq!(
            demand.images, expected,
            "only currently bound images remain after the real normal-drop writer"
        );
        assert_eq!(
            runtime.watches[&resource].allocation_images,
            Some(vec![generated.id()])
        );
        let external = runtime.resource_key("textures/shared.ktx2", None);
        assert_eq!(runtime.watches[&external].demand.images, vec![diffuse.id()]);
        assert!(runtime.ledger.resource_has_owners(&external));
    }
    bridge.publish_fixture(3, watched_demands(&app), true);
    observe_at(&mut app, 3);
    let runtime = app.world().resource::<StreamingRuntime>();
    assert_eq!(runtime.ledger.totals().transient_bytes, 0);
    let external = runtime.resource_key("textures/shared.ktx2", None);
    assert!(
        runtime.ledger.resource_has_owners(&external),
        "preparation does not release the live external texture lifetime claim"
    );
}

#[test]
fn failed_terrain_texture_writer_marks_the_actual_surface_failed() {
    use bevy::ecs::system::RunSystemOnce;
    let (mut app, _directory) = fixture(true);
    app.register_asset_loader(SurfaceDemandImageLoader);
    let missing: Handle<Image> = app
        .world()
        .resource::<AssetServer>()
        .load("textures/absent-diffuse.ktx2");
    let surface = app
        .world_mut()
        .spawn((
            TerrainPatch,
            Visibility::Hidden,
            crate::streaming::PendingTerrainProfile {
                cell_id: 1,
                quadrant: 0,
                images: vec![missing.clone()],
                normals: vec![],
            },
        ))
        .id();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    loop {
        app.update();
        if matches!(
            app.world()
                .resource::<AssetServer>()
                .get_load_state(missing.id()),
            Some(LoadState::Failed(_))
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "missing diffuse did not reach its real loader failure"
        );
        std::thread::yield_now();
    }
    app.world_mut()
        .run_system_once(crate::streaming::track_surface_readiness)
        .unwrap();
    assert!(
        app.world()
            .get::<crate::streaming::PendingTerrainProfile>(surface)
            .is_none()
    );
    assert_eq!(
        app.world().get::<Visibility>(surface),
        Some(&Visibility::Hidden)
    );
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .terrain_validation_failures,
        1
    );
    assert!(
        app.world()
            .get::<crate::streaming::AssetFailed>(surface)
            .is_some(),
        "the real required-texture failure writer must publish the failure marker"
    );
}
