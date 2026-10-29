use super::*;
use crate::streaming::lod::LodStreaming;
use crate::{
    cluster::reader::{
        ReferencePose, StockRefusalClass,
        producer_fixture_tests::{
            published_for_inventory, published_for_legacy_threshold_inventory,
            published_for_loader, published_for_runtime_inventory,
        },
    },
    streaming::{PendingModel, despawn_subtree, grid_translation, reference_placement},
    world::components::StreamedCellRoot,
};
use bevy::{
    app::{Propagate, PropagateOver},
    camera::visibility::RenderLayers,
};
use bevy_rapier3d::prelude::{Collider, ColliderDisabled};
use std::time::Duration;

#[test]
fn runtime_refused_statics_have_unbounded_stock_reach() {
    let fixture = Fixture::from_directory(published_for_runtime_inventory());
    let reference = fixture.package.placed_reference(0x109).unwrap();
    assert_eq!(reference.refusal_class, StockRefusalClass::Runtime);
    assert!(
        full_stock_reach(reference),
        "runtime-refused static geometry needs the same distance-free source route"
    );
}

#[path = "far_stock_pipeline_tests.rs"]
mod sparse_pipeline;

#[path = "far_stock_runtime_tests.rs"]
mod runtime_enable;

struct Fixture {
    directory: tempfile::TempDir,
    package: Arc<CertifiedPackage>,
    rows: Vec<(CellKey, ReferenceRow)>,
}

impl Fixture {
    fn new() -> Self {
        Self::from_directory(published_for_inventory())
    }

    fn from_directory(directory: tempfile::TempDir) -> Self {
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let path = directory.path().join("skyrim_world.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        let ids: Vec<u32> = connection
            .prepare("SELECT id FROM \"references\" WHERE is_exterior=1 ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let mut cells = HashMap::<CellKey, BTreeSet<u32>>::new();
        for id in ids {
            if let Some(placed) = package.placed_reference(id) {
                let snapshot = &placed.snapshot;
                let key = exterior(
                    snapshot.placed_worldspace_id,
                    IVec2::new(snapshot.cell.0, snapshot.cell.1),
                );
                cells.entry(key).or_default().insert(id);
            }
        }
        drop(connection);
        let database = WorldDatabase::open(&path).unwrap();
        let mut rows = Vec::new();
        for (key, ids) in cells {
            let response = database
                .request_references(17, key, ids)
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
            assert_eq!(response.generation, 17);
            assert_eq!(response.key, key);
            rows.extend(response.result.unwrap().into_iter().map(|row| (key, row)));
        }
        rows.sort_by_key(|(_, row)| row.form_id);
        assert!(!rows.is_empty());
        Self {
            directory,
            package,
            rows,
        }
    }

    fn selected(&self) -> (CellKey, ReferenceRow, ReferencePose) {
        let (key, row) = self
            .rows
            .iter()
            .find(|(key, row)| {
                matches!(key, CellKey::Exterior { grid_x, .. } if *grid_x < 0)
                    && row.model_path.is_some()
                    && self.package.placed_reference(row.form_id).unwrap().coverage
                        == CoverageState::Partial
            })
            .expect("producer inventory has a negative-grid partial source");
        (
            *key,
            row.clone(),
            self.package.placed_reference(row.form_id).unwrap().pose,
        )
    }
}

fn complete_plan_app(fixture: &Fixture, key: CellKey) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(EngineConfig {
            worldspace_id: cell_world(key).unwrap(),
            ..default()
        })
        .insert_resource(RenderOrigin(grid(key)))
        .insert_resource(ValidatedPackage(fixture.package.clone()))
        .init_resource::<ActiveSpace>()
        .init_resource::<LodStreaming>()
        .init_resource::<FarStock>()
        .init_resource::<SourceFallbackDemand>()
        .init_resource::<crate::streaming::StreamingMetrics>()
        .init_resource::<Messages<AppExit>>()
        .add_systems(Update, (plan, publish_work).chain());
    app.world_mut().spawn((
        crate::world::components::StreamingCamera,
        Transform::default(),
    ));
    app
}

fn exterior(world: u32, grid: IVec2) -> CellKey {
    CellKey::Exterior {
        worldspace_id: world,
        grid_x: grid.x,
        grid_y: grid.y,
    }
}

fn grid(key: CellKey) -> IVec2 {
    match key {
        CellKey::Exterior { grid_x, grid_y, .. } => IVec2::new(grid_x, grid_y),
        _ => panic!("exterior test owner required"),
    }
}

fn world() -> World {
    let mut world = World::new();
    let config = EngineConfig {
        // This ownership fixture has no walking player or physics schedule.
        headless: true,
        ..default()
    };
    assert!(!config.interactive_world_physics());
    world.insert_resource(config);
    world.insert_resource(RenderOrigin(IVec2::new(310_000, -270_000)));
    world.init_resource::<Messages<AppExit>>();
    world.init_resource::<ActiveSpace>();
    world.init_resource::<FarStock>();
    domain::install(&mut world);
    world
}

fn root(world: &mut World, key: CellKey, far: bool) -> Entity {
    world.init_resource::<ActiveSpace>();
    if world.resource::<ActiveSpace>().worldspace_id.is_none() {
        world.resource_mut::<ActiveSpace>().worldspace_id = cell_world(key);
    }
    let translation = grid_translation(grid(key), world.resource::<RenderOrigin>().0);
    let mut entity = world.spawn((
        ExteriorCellGrid(grid(key)),
        Transform::from_translation(translation),
        Visibility::Inherited,
    ));
    if far {
        entity.insert(DistantSourceRoot);
    } else {
        entity.insert(StreamedCellRoot);
    }
    // As in a real cell bundle, the role-owner marker must exist when the key observer runs.
    entity.insert(StreamedCellKey(key));
    entity.id()
}

fn source(
    world: &mut World,
    key: CellKey,
    parent: Entity,
    row: &ReferenceRow,
    far: bool,
) -> (Owner, Entity) {
    let (position, transform) = reference_placement(row.position, row.rotation, row.scale, key);
    let bounds =
        ExpectedModelBounds::new(Vec3::new(-2.0, -3.0, -4.0), Vec3::new(5.0, 6.0, 7.0)).unwrap();
    let path = row
        .model_path
        .clone()
        .and_then(crate::streaming::converted_model_path)
        .unwrap();
    let entity = world
        .spawn((
            FormId(row.form_id),
            CellRef(row.cell_id),
            position,
            transform,
            WorldTransform(transform.to_matrix()),
            bounds,
            InstanceBounds::transformed(bounds.min, bounds.max, transform.to_matrix()),
            PendingModel {
                handle: Handle::default(),
                sequence: 0,
            },
            PendingAssetProfile {
                started: Instant::now(),
                scene_spawned: false,
                path,
                form_id: row.form_id,
                base_form_id: row.base_form_id,
                base_record_type: row.base_record_type.clone(),
                static_physics: false,
                cell_id: row.cell_id,
                actor_scene: false,
            },
            Collider::cuboid(1.0, 1.0, 1.0),
            ColliderDisabled,
            ChildOf(parent),
        ))
        .id();
    let model = world
        .spawn((Name::new("retained model child"), ChildOf(entity)))
        .id();
    (
        Owner {
            entity,
            parent,
            key,
            near_key: (!far).then_some(key),
            near_root: (!far).then_some(parent),
            far,
            row: Some(row.clone()),
            physics_requested: false,
        },
        model,
    )
}

fn state(fixture: &Fixture, key: CellKey, owner: Owner) -> FarStock {
    let placing_world = cell_world(key).unwrap();
    let identity = (placing_world, owner.row.as_ref().unwrap().form_id);
    let mut far = FarStock {
        package: Some(fixture.package.clone()),
        demand: Some(LodDemand {
            world: placing_world,
            center: grid(key),
        }),
        desired: HashMap::from([(identity, key)]),
        near_roots: owner.near_key.zip(owner.near_root).into_iter().collect(),
        ..Default::default()
    };
    far.put_owner(identity, owner);
    far
}

fn assert_one_terminal_exit(app: &mut App, reason: &str) {
    assert!(
        app.world()
            .resource::<crate::streaming::failure::LoadingRefusal>()
            .0
            .contains(reason)
    );
    assert_eq!(
        app.world_mut()
            .resource_mut::<Messages<AppExit>>()
            .drain()
            .collect::<Vec<_>>(),
        [AppExit::error()]
    );
}

fn assert_no_exit(world: &World) {
    if let Some(far) = world.get_resource::<FarStock>() {
        assert_bookkeeping(far);
    }
    assert!(world.resource::<Messages<AppExit>>().is_empty());
}

// Exercise the real sparse collector and shared spawner with an already delivered database row.
// Scene admission retains the request, so these tests need neither a glTF loader nor a renderer.
fn layer_app(fixture: &Fixture, key: CellKey, row: &ReferenceRow) -> App {
    let mut app = App::new();
    let config = EngineConfig {
        max_scene_loads: 1,
        lights: true,
        ..default()
    };
    let (_sender, receiver) = crossbeam_channel::bounded(1);
    let far = FarStock {
        package: Some(fixture.package.clone()),
        demand: Some(LodDemand {
            world: cell_world(key).unwrap(),
            center: grid(key),
        }),
        desired: HashMap::from([((cell_world(key).unwrap(), row.form_id), key)]),
        pending: HashMap::from([(
            key,
            Pending {
                ids: BTreeSet::from([row.form_id]),
                receiver,
                ready: Some(ReferenceResponse {
                    generation: 0,
                    key,
                    result: Ok(vec![row.clone()]),
                    query_micros: 0,
                    queue_wait_micros: 0,
                    total_request_micros: 0,
                }),
            },
        )]),
        ..default()
    };
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .insert_resource(config)
        .insert_resource(RenderOrigin(grid(key)))
        .insert_resource(
            WorldDatabase::open(&fixture.directory.path().join("skyrim_world.db")).unwrap(),
        )
        .insert_resource(far)
        .insert_resource(StreamingCommitBudget {
            remaining: 1,
            ..default()
        })
        .init_resource::<ActiveSpace>()
        .init_resource::<StreamingWorld>()
        .init_resource::<Messages<AppExit>>()
        .add_systems(Update, collect);
    crate::render::add_placed_object_layer_propagation(&mut app);
    app
}

fn assert_stock_layers(world: &World, entities: &[Entity]) {
    for &entity in entities {
        let layers = world.get::<RenderLayers>(entity).unwrap();
        assert_eq!(*layers, crate::render::PLACED_OBJECT_RENDER_LAYERS);
        assert!(!layers.intersects(&RenderLayers::from_layers(
            crate::render::REFLECTION_VIEW_LAYERS,
        )));
        assert!(layers.intersects(&RenderLayers::from_layers(crate::render::MAIN_VIEW_LAYERS)));
        assert!(layers.intersects(&RenderLayers::from_layers(crate::render::LIGHT_LAYERS)));
    }
}

#[test]
fn nonseamless_exact_sources_keep_stock_layers_for_both_producer_orders_and_late_scenes() {
    let fixture = Fixture::new();
    let (key, mut row, _) = fixture.selected();
    // The shared spawn helper must keep its real light override through every transfer too.
    row.light = Some(crate::world::database::LightRow {
        radius: 128.0,
        color: [255, 200, 150],
        flags: 0,
    });
    let identity = (cell_world(key).unwrap(), row.form_id);
    for near_first in [false, true] {
        let mut app = layer_app(&fixture, key, &row);
        let near = root(app.world_mut(), key, false);
        let ordinary = if near_first {
            let asset_server = app.world().resource::<AssetServer>().clone();
            let mut sequence = 0;
            let mut context = ReferenceSpawnContext {
                key,
                model_sequence: &mut sequence,
                asset_server: &asset_server,
                actor_catalog: &crate::world::actor_catalog::ActorCatalog::default(),
                world_physics: false,
                lights: true,
                schedule_scenes: true,
            };
            let mut spawned = None;
            app.world_mut()
                .commands()
                .entity(near)
                .with_children(|parent| {
                    spawned = spawn_reference(parent, row.clone(), &mut context);
                });
            app.world_mut().flush();
            with_far(app.world_mut(), |world, far| {
                register_near(world, far, near, key)
            });
            Some(spawned.unwrap().0)
        } else {
            None
        };
        app.update();
        let entity = app.world().resource::<FarStock>().owners[&identity].entity;
        let sequence = app
            .world()
            .get::<super::super::requests::SceneRequest>(entity)
            .unwrap()
            .sequence;
        if let Some(ordinary) = ordinary {
            assert_eq!(
                entity, ordinary,
                "sparse response must reuse the near producer"
            );
        }
        assert_eq!(
            app.world()
                .get::<Propagate<RenderLayers>>(entity)
                .unwrap()
                .0,
            crate::render::PLACED_OBJECT_RENDER_LAYERS,
            "exact sources must select the ordinary shared spawn branch"
        );
        let light = app
            .world()
            .get::<Children>(entity)
            .unwrap()
            .iter()
            .find(|&child| app.world().get::<PointLight>(child).is_some())
            .unwrap();
        let model = app.world_mut().spawn(ChildOf(entity)).id();
        let mesh = app.world_mut().spawn(ChildOf(model)).id();
        let own_layers = RenderLayers::layer(7);
        let own = app
            .world_mut()
            .spawn((
                own_layers.clone(),
                PropagateOver::<RenderLayers>::default(),
                ChildOf(entity),
            ))
            .id();
        app.update();
        assert_stock_layers(app.world(), &[entity, model, mesh]);
        assert_eq!(app.world().get::<RenderLayers>(own), Some(&own_layers));

        let mut payload = vec![row.clone()];
        let mut adoptions = Vec::new();
        with_far(app.world_mut(), |world, far| {
            adoptions =
                far.take_adoptions(key, &mut payload, |entity| world.get_entity(entity).is_ok());
        });
        assert!(payload.is_empty());
        if near_first {
            with_far(app.world_mut(), refresh_holders);
        } else {
            queue_adopt(&mut app.world_mut().commands(), near, key, adoptions);
        }
        app.world_mut().flush();
        app.update();
        assert_eq!(app.world().get::<ChildOf>(entity).unwrap().parent(), near);
        assert_stock_layers(app.world(), &[entity, model, mesh]);
        assert_eq!(app.world().get::<RenderLayers>(own), Some(&own_layers));

        release(app.world_mut(), near);
        app.world_mut().entity_mut(near).insert(Visibility::Hidden);
        with_far(app.world_mut(), reconcile_near);
        let holder = app.world().resource::<FarStock>().holders[&key];
        assert_eq!(app.world().get::<ChildOf>(entity).unwrap().parent(), holder);
        // Public demand controls holder visibility; source layers survive every refresh.
        let demand = app.world().resource::<FarStock>().demand;
        for active in [false, true] {
            with_far(app.world_mut(), |world, far| {
                far.demand = if active { demand } else { None };
                refresh_holders(world, far);
            });
            let late = app.world_mut().spawn(ChildOf(model)).id();
            app.update();
            assert_stock_layers(app.world(), &[entity, model, mesh, late]);
            assert_eq!(app.world().get::<RenderLayers>(own), Some(&own_layers));
            assert_eq!(
                app.world().get::<Visibility>(holder),
                Some(&holder_visibility(active))
            );
        }
        app.world_mut()
            .entity_mut(near)
            .insert(Visibility::Inherited);
        queue_revive(&mut app.world_mut().commands(), near, key);
        app.world_mut().flush();
        app.update();
        assert_eq!(app.world().get::<ChildOf>(entity).unwrap().parent(), near);
        assert_stock_layers(app.world(), &[entity, model, mesh]);
        assert_eq!(app.world().get::<RenderLayers>(own), Some(&own_layers));
        assert_eq!(
            app.world()
                .get::<super::super::requests::SceneRequest>(entity)
                .unwrap()
                .sequence,
            sequence,
            "transfers must retain the original model request"
        );
        assert_eq!(
            app.world().get::<RenderLayers>(light),
            Some(&RenderLayers::from_layers(crate::render::LIGHT_LAYERS))
        );
        assert!(
            app.world()
                .get::<bevy::app::PropagateOver<RenderLayers>>(light)
                .is_some()
        );
        let mut references = app.world_mut().query::<&FormId>();
        assert_eq!(
            references
                .iter(app.world())
                .filter(|form| form.0 == row.form_id)
                .count(),
            1
        );
        assert_no_exit(app.world());
    }
}

fn assert_bookkeeping(far: &FarStock) {
    let mut by_root = HashMap::<Entity, HashSet<Identity>>::new();
    let mut by_near_key = HashMap::<CellKey, HashSet<Identity>>::new();
    for (&id, owner) in &far.owners {
        for root in [Some(owner.parent), owner.near_root].into_iter().flatten() {
            by_root.entry(root).or_default().insert(id);
        }
        if let Some(key) = owner.near_key {
            by_near_key.entry(key).or_default().insert(id);
        }
    }
    assert_eq!(far.by_root, by_root);
    assert_eq!(far.by_near_key, by_near_key);
    let queued_keys: HashSet<_> = far.queued.iter().map(|(key, _)| *key).collect();
    assert_eq!(far.queued_keys, queued_keys);
    assert_eq!(far.queued.len(), queued_keys.len());
}

fn assert_placement_transform(
    world: &World,
    entity: Entity,
    pose: ReferencePose,
    key: CellKey,
) -> Transform {
    let expected = reference_placement(pose.position, pose.rotation, pose.scale, key).1;
    let actual = world.get::<Transform>(entity).unwrap();
    assert_eq!(
        actual.translation.to_array().map(f32::to_bits),
        expected.translation.to_array().map(f32::to_bits)
    );
    assert_eq!(
        actual.rotation.to_array().map(f32::to_bits),
        expected.rotation.to_array().map(f32::to_bits)
    );
    assert_eq!(
        actual.scale.to_array().map(f32::to_bits),
        expected.scale.to_array().map(f32::to_bits)
    );
    assert_eq!(
        world
            .get::<WorldTransform>(entity)
            .unwrap()
            .0
            .to_cols_array()
            .map(f32::to_bits),
        expected.to_matrix().to_cols_array().map(f32::to_bits)
    );
    expected
}

fn assert_placement(world: &World, entity: Entity, pose: ReferencePose, key: CellKey) {
    let expected = assert_placement_transform(world, entity, pose, key);
    let bounds = world.get::<ExpectedModelBounds>(entity).unwrap();
    let expected_bounds = InstanceBounds::transformed(bounds.min, bounds.max, expected.to_matrix());
    let actual_bounds = world.get::<InstanceBounds>(entity).unwrap();
    assert_eq!(
        actual_bounds.min.to_array().map(f32::to_bits),
        expected_bounds.min.to_array().map(f32::to_bits)
    );
    assert_eq!(
        actual_bounds.max.to_array().map(f32::to_bits),
        expected_bounds.max.to_array().map(f32::to_bits)
    );
}

#[test]
fn producer_coverage_selects_stock_and_exact_rows_refuse_forged_placement_fields() {
    let fixture = Fixture::new();
    let (_, selected, _) = fixture.selected();
    let mut complete = fixture
        .package
        .placed_reference(selected.form_id)
        .unwrap()
        .clone();
    complete.coverage = CoverageState::Complete;
    assert!(!needs_stock(&complete));
    let mut observed = Vec::new();
    for (_, row) in &fixture.rows {
        let placed = fixture.package.placed_reference(row.form_id).unwrap();
        if !observed.contains(&placed.coverage) {
            observed.push(placed.coverage);
        }
        assert_eq!(needs_stock(placed), placed.model_path.is_some());
        assert_eq!(
            super::super::spawned_model_path(row.model_path.clone(), row.base_is_marker),
            placed.model_path
        );
        assert!(row_matches(&fixture.package, row));
    }
    for coverage in [
        CoverageState::Partial,
        CoverageState::Unknown,
        CoverageState::NotEligible,
    ] {
        assert!(
            observed.contains(&coverage),
            "missing actual {coverage:?} control"
        );
    }
    let no_model = fixture
        .rows
        .iter()
        .find_map(|(_, row)| {
            let placed = fixture.package.placed_reference(row.form_id).unwrap();
            placed.model_path.is_none().then_some(placed)
        })
        .expect("producer fixture keeps a no-model reference explicit");
    assert_eq!(no_model.coverage, CoverageState::NotEligible);
    assert!(no_model.stock_primitives.is_none());
    assert!(!needs_stock(no_model));
    let missing = fixture
        .rows
        .iter()
        .find_map(|(_, row)| {
            let placed = fixture.package.placed_reference(row.form_id).unwrap();
            (placed.coverage == CoverageState::Unknown
                && placed
                    .model_path
                    .as_ref()
                    .is_some_and(|path| !fixture.directory.path().join(path).exists()))
            .then_some(placed)
        })
        .expect("producer fixture keeps a missing converted model unknown");
    assert!(needs_stock(missing));
    assert!(missing.stock_primitives.is_none());
    let (_, row, _) = fixture.selected();
    for field in 0..8 {
        let mut forged = row.clone();
        match field {
            0 => forged.form_id = u32::MAX,
            1 => forged.base_form_id ^= 1,
            2 => forged.cell_id ^= 1,
            3 => forged.position[0] = f32::from_bits(forged.position[0].to_bits() ^ 1),
            4 => forged.position[1] = f32::NAN,
            5 => forged.rotation[2] = f32::from_bits(forged.rotation[2].to_bits() ^ 1),
            6 => forged.scale = f32::from_bits(forged.scale.to_bits() ^ 1),
            _ => forged.model_path = Some("invented/other.nif".into()),
        }
        assert!(!row_matches(&fixture.package, &forged), "field {field}");
    }
}

#[test]
fn pristine_roundtrips_keep_loader_bits_entity_pending_model_and_child_at_extreme_owner_grids() {
    let fixture = Fixture::new();
    let (far_key, row, pose) = fixture.selected();
    let mut world = world();
    let near_key = exterior(cell_world(far_key).unwrap(), IVec2::new(-170_003, 220_009));
    let near = root(&mut world, near_key, false);
    let far = root(&mut world, far_key, true);
    let (mut owner, model) = source(&mut world, near_key, near, &row, false);
    let entity = owner.entity;
    let pending_id = world.get::<PendingModel>(entity).unwrap().handle.id();
    let started = world.get::<PendingAssetProfile>(entity).unwrap().started;
    for _ in 0..3 {
        move_owner(&mut world, &mut owner, far, far_key, true, Some(pose)).unwrap();
        assert_placement(&world, entity, pose, far_key);
        assert!(world.get::<ColliderDisabled>(entity).is_some());
        move_owner(&mut world, &mut owner, near, near_key, false, Some(pose)).unwrap();
        assert_placement(&world, entity, pose, near_key);
        assert!(world.get::<ColliderDisabled>(entity).is_none());
        assert_eq!(owner.entity, entity);
        assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
        assert_eq!(
            world.get::<PendingModel>(entity).unwrap().handle.id(),
            pending_id
        );
        assert_eq!(
            world.get::<PendingAssetProfile>(entity).unwrap().started,
            started
        );
    }
    assert_no_exit(&world);
}

#[test]
fn runtime_edited_pose_survives_transfer_instead_of_resetting_to_certified_pose() {
    let fixture = Fixture::new();
    let (key, row, pose) = fixture.selected();
    let mut world = world();
    let near_key = exterior(cell_world(key).unwrap(), IVec2::new(-7, 11));
    let near = root(&mut world, near_key, false);
    let far = root(&mut world, key, true);
    let (mut owner, model) = source(&mut world, near_key, near, &row, false);
    let mut edited = *world.get::<Transform>(owner.entity).unwrap();
    edited.translation += Vec3::new(128.0, 256.0, -512.0);
    edited.rotation = Quat::from_rotation_y(0.7);
    edited.scale = Vec3::new(0.5, 1.5, 2.5);
    world.entity_mut(owner.entity).insert(edited);
    let offset = world.get::<Transform>(near).unwrap().translation
        - world.get::<Transform>(far).unwrap().translation;
    move_owner(&mut world, &mut owner, far, key, true, Some(pose)).unwrap();
    let moved = world.get::<Transform>(owner.entity).unwrap();
    assert_eq!(moved.translation, edited.translation + offset);
    assert_eq!(moved.rotation, edited.rotation);
    assert_eq!(moved.scale, edited.scale);
    assert_ne!(
        *moved,
        reference_placement(pose.position, pose.rotation, pose.scale, key).1
    );
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), owner.entity);
    assert_eq!(
        world.get::<WorldTransform>(owner.entity).unwrap().0,
        moved.to_matrix()
    );
    assert_no_exit(&world);
}

#[test]
fn recursive_near_despawn_protects_the_selected_reference_before_deleting_its_old_root() {
    let fixture = Fixture::new();
    let (key, row, pose) = fixture.selected();
    let mut world = world();
    let near_key = exterior(cell_world(key).unwrap(), IVec2::ZERO);
    let near = root(&mut world, near_key, false);
    let (owner, model) = source(&mut world, near_key, near, &row, false);
    let entity = owner.entity;
    world.insert_resource(state(&fixture, key, owner));
    let unrelated = world.spawn(ChildOf(near)).id();
    despawn_subtree(&mut world, near);
    assert!(world.get_entity(near).is_err());
    assert!(world.get_entity(unrelated).is_err());
    assert!(world.get_entity(entity).is_ok());
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    let far = world.resource::<FarStock>();
    let owner = &far.owners[&(cell_world(key).unwrap(), row.form_id)];
    assert!(owner.far);
    assert_eq!(
        world.get::<ChildOf>(entity).unwrap().parent(),
        far.holders[&key]
    );
    assert_placement(&world, entity, pose, key);
    assert_no_exit(&world);
}

#[test]
fn unchanged_demand_reconciles_hidden_then_revived_near_root_without_new_payload() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut world = world();
    let near = root(&mut world, key, false);
    let (owner, model) = source(&mut world, key, near, &row, false);
    let entity = owner.entity;
    world.insert_resource(state(&fixture, key, owner));
    with_far(&mut world, reconcile_near);
    let demand = world.resource::<FarStock>().demand;
    let pending_id = world.get::<PendingModel>(entity).unwrap().handle.id();
    world.entity_mut(near).insert(Visibility::Hidden);
    with_far(&mut world, reconcile_near);
    assert!(world.resource::<FarStock>().owners[&(cell_world(key).unwrap(), row.form_id)].far);
    assert_ne!(world.get::<ChildOf>(entity).unwrap().parent(), near);
    assert!(world.get::<ColliderDisabled>(entity).is_some());
    world.entity_mut(near).insert(Visibility::Inherited);
    queue_revive(&mut world.commands(), near, key);
    world.flush();
    assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), near);
    assert!(!world.resource::<FarStock>().owners[&(cell_world(key).unwrap(), row.form_id)].far);
    assert!(world.get::<ColliderDisabled>(entity).is_none());
    assert_eq!(world.resource::<FarStock>().demand, demand);
    assert!(world.resource::<FarStock>().pending.is_empty());
    assert!(world.resource::<FarStock>().queued.is_empty());
    assert_eq!(
        world.get::<PendingModel>(entity).unwrap().handle.id(),
        pending_id
    );
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    assert_no_exit(&world);
}

#[test]
fn refresh_removes_empty_or_missing_holders_before_role_writes_and_keeps_filled_holder() {
    let mut world = world();
    let key = exterior(17, IVec2::new(-3, 4));
    let empty_key = exterior(17, IVec2::new(-4, 4));
    let missing_key = exterior(17, IVec2::new(-5, 4));
    let filled = root(&mut world, key, true);
    let empty = root(&mut world, empty_key, true);
    let missing = root(&mut world, missing_key, true);
    world.entity_mut(missing).despawn();
    let child = world.spawn(ChildOf(filled)).id();
    let mut far = FarStock {
        holders: HashMap::from([(key, filled), (empty_key, empty), (missing_key, missing)]),
        ..Default::default()
    };
    refresh_holders(&mut world, &mut far);
    assert_eq!(far.holders, HashMap::from([(key, filled)]));
    assert!(world.get_entity(empty).is_err());
    assert!(world.get_entity(child).is_ok());
    assert_eq!(world.get::<ChildOf>(child).unwrap().parent(), filled);
    assert_no_exit(&world);
}

#[test]
fn withdrawal_and_replacement_restore_the_original_near_entity_before_holder_teardown() {
    let fixture = Fixture::new();
    let (key, row, pose) = fixture.selected();
    for replacement in [false, true] {
        let mut world = world();
        let near_key = exterior(cell_world(key).unwrap(), IVec2::ZERO);
        let near = root(&mut world, near_key, false);
        let far = root(&mut world, key, true);
        let (mut owner, model) = source(&mut world, near_key, near, &row, false);
        let entity = owner.entity;
        move_owner(&mut world, &mut owner, far, key, true, Some(pose)).unwrap();
        if replacement {
            let newer = Arc::new(
                crate::cluster::overlay_fixture::open_fixture(fixture.directory.path(), false)
                    .unwrap(),
            );
            assert!(!Arc::ptr_eq(&newer, &fixture.package));
            world.resource_mut::<FarStock>().package = Some(newer);
        }
        retire_sources(
            &mut world,
            Some(fixture.package.clone()),
            HashMap::from([((cell_world(key).unwrap(), row.form_id), owner)]),
            HashMap::from([(key, far)]),
        );
        assert!(world.get_entity(far).is_err());
        assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), near);
        assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
        assert!(world.get::<PendingModel>(entity).is_some());
        assert_placement(&world, entity, pose, near_key);
        assert_no_exit(&world);
    }
}

#[test]
fn payload_adoption_reuses_live_scene_and_rejects_a_dead_generational_owner() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let other = fixture
        .rows
        .iter()
        .find(|(other_key, other)| *other_key == key && other.form_id != row.form_id)
        .unwrap()
        .1
        .clone();
    let mut world = world();
    let holder = root(&mut world, key, true);
    let (owner, model) = source(&mut world, key, holder, &row, true);
    let entity = owner.entity;
    let mut far = state(&fixture, key, owner);
    far.holders.insert(key, holder);
    let before = world.entity_count();
    let mut payload = vec![row.clone(), other.clone()];
    let adoptions =
        far.take_adoptions(key, &mut payload, |entity| world.get_entity(entity).is_ok());
    assert_eq!(adoptions, [row.form_id]);
    assert_eq!(payload.len(), 1);
    assert_eq!(payload[0].form_id, other.form_id);
    assert_eq!(world.entity_count(), before);
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    let near = root(&mut world, key, false);
    world.insert_resource(far);
    let before_adoption = world.entity_count();
    queue_adopt(&mut world.commands(), near, key, adoptions);
    world.flush();
    assert_eq!(world.entity_count(), before_adoption - 1);
    assert!(world.get_entity(holder).is_err());
    assert!(!world.resource::<FarStock>().holders.contains_key(&key));
    assert!(world.get_entity(near).is_ok());
    assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), near);
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    assert!(world.get::<PendingModel>(entity).is_some());
    let mut far = world.remove_resource::<FarStock>().unwrap();
    world.entity_mut(entity).despawn();
    let replacement = world.spawn(FormId(row.form_id)).id();
    assert_ne!(entity, replacement);
    let mut payload = vec![row.clone()];
    assert!(
        far.take_adoptions(key, &mut payload, |entity| world.get_entity(entity).is_ok())
            .is_empty()
    );
    assert_eq!(payload.len(), 1);
    assert!(
        !far.owners
            .contains_key(&(cell_world(key).unwrap(), row.form_id))
    );
    assert!(world.get_entity(replacement).is_ok());
    assert_bookkeeping(&far);
    assert_no_exit(&world);
}

#[test]
fn first_near_physics_promotion_updates_pending_or_requeues_completed_profile_once() {
    let fixture = Fixture::new();
    let (key, row, pose) = fixture.selected();
    for pending_load in [false, true] {
        let mut world = world();
        let walking = EngineConfig::default();
        assert!(walking.interactive_world_physics());
        world.insert_resource(walking);
        let near = root(&mut world, key, false);
        let far = root(&mut world, key, true);
        let (mut owner, model) = source(&mut world, key, far, &row, true);
        let entity = owner.entity;
        let pending_id = world.get::<PendingModel>(entity).unwrap().handle.id();
        if !pending_load {
            world
                .entity_mut(entity)
                .remove::<(PendingModel, PendingAssetProfile)>();
        }
        let before = world.entity_count();
        move_owner(&mut world, &mut owner, near, key, false, Some(pose)).unwrap();
        let profile = world.get::<PendingAssetProfile>(entity).unwrap();
        assert!(profile.static_physics);
        assert_eq!(profile.scene_spawned, !pending_load);
        assert_eq!(profile.form_id, row.form_id);
        assert_eq!(profile.base_form_id, row.base_form_id);
        assert_eq!(profile.cell_id, row.cell_id);
        assert_eq!(world.entity_count(), before);
        assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
        if pending_load {
            assert_eq!(
                world.get::<PendingModel>(entity).unwrap().handle.id(),
                pending_id
            );
        } else {
            assert!(world.get::<PendingModel>(entity).is_none());
        }
        assert!(owner.physics_requested);
        world.entity_mut(entity).remove::<PendingAssetProfile>();
        move_owner(&mut world, &mut owner, far, key, true, Some(pose)).unwrap();
        move_owner(&mut world, &mut owner, near, key, false, Some(pose)).unwrap();
        assert!(
            world.get::<PendingAssetProfile>(entity).is_none(),
            "completed collision work must not be queued twice"
        );
        assert_no_exit(&world);
    }
}

#[test]
fn refused_release_blocks_recursive_despawn_queued_hide_and_replacement_of_the_old_root() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut world = world();
    let near_key = exterior(cell_world(key).unwrap(), IVec2::ZERO);
    let near = root(&mut world, near_key, false);
    let (owner, model) = source(&mut world, near_key, near, &row, false);
    let entity = owner.entity;
    let original = *world.get::<Transform>(entity).unwrap();
    world.insert_resource(state(&fixture, key, owner));
    // A rotated parent is outside move_owner's accepted translation-only contract. Exercise
    // the refusal through the actual recursive teardown entry, without injecting failure state.
    world.get_mut::<Transform>(near).unwrap().rotation = Quat::from_rotation_y(0.25);
    assert_eq!(despawn_subtree(&mut world, near), 0);
    assert!(removal_blocked(&world, near));
    assert!(world.get::<FailedOwnerRoot>(near).is_some());
    assert!(!world.resource::<FarStock>().failed.is_empty());
    assert!(
        world
            .resource::<FarStock>()
            .failed
            .values()
            .any(|reason| reason.contains("unrotated cell translation"))
    );
    assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), near);
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    assert_eq!(*world.get::<Transform>(entity).unwrap(), original);
    let registered = &world.resource::<FarStock>().owners[&(cell_world(key).unwrap(), row.form_id)];
    assert_eq!(registered.entity, entity);
    assert_eq!(registered.parent, near);
    assert!(!registered.far);
    assert_bookkeeping(world.resource::<FarStock>());

    let mut hiding = bevy::ecs::system::SystemState::<(
        Commands,
        Query<(&Visibility, Option<&crate::streaming::RetireHidden>)>,
    )>::new(&mut world);
    {
        let (mut commands, roots) = hiding.get_mut(&mut world).expect("retire-hide query");
        crate::streaming::hide_retiring_root(&mut commands, &roots, near);
    }
    hiding.apply(&mut world);
    queue_despawn(&mut world.commands(), near);
    assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), near);
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    assert!(world.get::<PendingModel>(entity).is_some());
    assert!(
        world
            .resource_mut::<Messages<AppExit>>()
            .drain()
            .any(|exit| exit.is_error())
    );
}

#[test]
fn refused_withdrawal_restore_preserves_the_far_holder_and_its_source_through_later_teardown() {
    let fixture = Fixture::new();
    let (key, row, pose) = fixture.selected();
    let mut world = world();
    let near_key = exterior(cell_world(key).unwrap(), IVec2::ZERO);
    let near = root(&mut world, near_key, false);
    let far = root(&mut world, key, true);
    let (mut owner, model) = source(&mut world, near_key, near, &row, false);
    let entity = owner.entity;
    move_owner(&mut world, &mut owner, far, key, true, Some(pose)).unwrap();
    let original = *world.get::<Transform>(entity).unwrap();
    // The old near root survives but is no longer a legal cell translation. Withdrawal must
    // retain the source under its current holder when restoration cannot complete.
    world.get_mut::<Transform>(near).unwrap().rotation = Quat::from_rotation_y(0.25);
    retire_sources(
        &mut world,
        Some(fixture.package.clone()),
        HashMap::from([((cell_world(key).unwrap(), row.form_id), owner)]),
        HashMap::from([(key, far)]),
    );
    assert!(world.get_entity(near).is_ok());
    assert!(world.get_entity(far).is_ok());
    assert!(removal_blocked(&world, far));
    assert!(!world.resource::<FarStock>().failed.is_empty());
    assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), far);
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    assert_eq!(*world.get::<Transform>(entity).unwrap(), original);
    queue_despawn(&mut world.commands(), far);
    world.flush();
    assert!(world.get_entity(far).is_ok());
    assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), far);
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    assert!(world.get::<PendingModel>(entity).is_some());
    assert!(
        world
            .resource_mut::<Messages<AppExit>>()
            .drain()
            .any(|exit| exit.is_error())
    );
}

#[test]
fn desired_eviction_restores_hidden_borrowed_sources_and_removes_unborrowed_far_sources() {
    let fixture = Fixture::new();
    let (key, row, pose) = fixture.selected();
    for borrowed in [false, true] {
        let mut world = world();
        let near_key = exterior(cell_world(key).unwrap(), IVec2::ZERO);
        let near = root(&mut world, near_key, false);
        let far = root(&mut world, key, true);
        let (mut owner, model) = if borrowed {
            source(&mut world, near_key, near, &row, false)
        } else {
            source(&mut world, key, far, &row, true)
        };
        let entity = owner.entity;
        if borrowed {
            move_owner(&mut world, &mut owner, far, key, true, Some(pose)).unwrap();
        }
        world.entity_mut(near).insert(Visibility::Hidden);
        // Desired-set eviction passes no holder teardown list: other live sources can still
        // use the holder. Borrowed references return to their original near ownership anyway.
        retire_sources(
            &mut world,
            Some(fixture.package.clone()),
            HashMap::from([((cell_world(key).unwrap(), row.form_id), owner)]),
            HashMap::new(),
        );
        assert!(world.get_entity(far).is_ok());
        assert!(world.get_entity(near).is_ok());
        assert_eq!(world.get::<Visibility>(near), Some(&Visibility::Hidden));
        if borrowed {
            assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), near);
            assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
            assert!(world.get::<PendingModel>(entity).is_some());
            assert!(world.get::<ColliderDisabled>(entity).is_some());
            assert_placement(&world, entity, pose, near_key);
        } else {
            assert!(world.get_entity(entity).is_err());
            assert!(world.get_entity(model).is_err());
        }
        assert_no_exit(&world);
    }
}

#[test]
fn headless_destination_readiness_uses_exact_far_cell_coverage_and_hidden_holder_failures() {
    use crate::swap_bridge::{
        destination::{
            DestinationKey, DestinationReports, DestinationRequest, DestinationRequests,
            DestinationState, Failure, Generations, Handoff, InvalidationCause, Missing,
            PassFamilies, PassFamily, ViewKey, WorldId,
        },
        systems::DestinationReadinessPlugin,
    };
    use bevy::render::view::RetainedViewEntity;

    // No RenderApp: facts 3/4 are vacuous. This exercises residency/asset refusal only,
    // and makes no claim about drawable geometry, queued passes or renderer acceptance.
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_resource::<ActiveSpace>()
        .init_resource::<StreamingWorld>()
        .init_resource::<FarStock>()
        .add_plugins(DestinationReadinessPlugin);
    let cell = exterior(17, IVec2::new(-3, 4));
    let other_world = exterior(18, grid(cell));
    let near = app
        .world_mut()
        .spawn((
            StreamedCellRoot,
            StreamedCellKey(cell),
            ExteriorCellGrid(grid(cell)),
            Visibility::Inherited,
        ))
        .id();
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .set_resident_for_test(cell, near);
    let view = app.world_mut().spawn_empty().id();
    let world_id = WorldId::exterior(cell_world(cell).unwrap());
    let generation = app
        .world_mut()
        .resource_mut::<Generations>()
        .bump(world_id, InvalidationCause::Composition);
    let key = DestinationKey {
        view: ViewKey {
            view: RetainedViewEntity::new(view.into(), None, 0),
            incarnation: 0,
        },
        world: world_id,
        generation,
    };
    let request = app
        .world_mut()
        .resource_mut::<DestinationRequests>()
        .request(DestinationRequest {
            key,
            passes: PassFamilies::NONE.with(PassFamily::Forward),
            coverage: Arc::from(vec![cell]),
            handoff: Handoff::NotRequired,
        });
    app.world_mut()
        .resource_mut::<FarStock>()
        .set_queued(VecDeque::from([(cell, BTreeSet::from([1]))]));
    app.update();
    assert_eq!(
        app.world()
            .resource::<DestinationReports>()
            .get(&key)
            .unwrap()
            .state,
        DestinationState::Waiting(Missing::Assets)
    );
    {
        let mut far = app.world_mut().resource_mut::<FarStock>();
        far.set_queued(VecDeque::from([(other_world, BTreeSet::from([1]))]));
        far.failed
            .insert(other_world, "unrelated world's source failed".into());
    }
    app.update();
    assert!(
        app.world()
            .resource::<DestinationReports>()
            .is_ready(&key, app.world().resource::<Generations>())
    );
    let holder = app
        .world_mut()
        .spawn((
            DistantSourceRoot,
            StreamedCellKey(cell),
            ExteriorCellGrid(grid(cell)),
            Visibility::Hidden,
        ))
        .id();
    app.world_mut().spawn((
        crate::streaming::AssetFailed,
        Visibility::Hidden,
        ChildOf(holder),
    ));
    app.world_mut()
        .resource_mut::<FarStock>()
        .holders
        .insert(cell, holder);
    app.update();
    assert!(
        app.world()
            .resource::<DestinationReports>()
            .is_ready(&key, app.world().resource::<Generations>()),
        "ordinary hidden landing failures retain ordinary diagnostic behavior"
    );
    let key = app
        .world_mut()
        .resource_scope(|world, mut requests: Mut<DestinationRequests>| {
            requests
                .update(
                    request,
                    &mut world.resource_mut::<Generations>(),
                    key.view,
                    PassFamilies::NONE.with(PassFamily::Forward),
                    Arc::from(vec![cell]),
                    Handoff::Required,
                )
                .unwrap()
        });
    app.update();
    assert_eq!(
        app.world()
            .resource::<DestinationReports>()
            .get(&key)
            .unwrap()
            .state,
        DestinationState::Failed(Failure::AssetFailed(cell))
    );
    assert!(
        app.world().get::<Children>(near).is_none(),
        "near residency stayed quiet"
    );
}

#[test]
fn publish_work_rederives_existing_owned_failures_after_certification_and_failure_reset() {
    use crate::streaming::{AssetFailed, StreamingMetrics};

    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .init_resource::<FarStock>()
        .init_resource::<StreamingMetrics>()
        .add_systems(Update, publish_work);
    let holder = root(app.world_mut(), key, true);
    let (owner, _) = source(app.world_mut(), key, holder, &row, true);
    let entity = owner.entity;
    app.world_mut().entity_mut(entity).insert(AssetFailed);
    // The failure exists before certification/demand owns the reference; its Added tick
    // has elapsed by the time publish_work can associate it with a retained package.
    app.update();
    assert!(app.world().resource::<FarStock>().failed.is_empty());
    let pending_key = exterior(cell_world(key).unwrap(), grid(key) + IVec2::X);
    let (_responses, receiver) = crossbeam_channel::bounded(1);
    let mut far = state(&fixture, key, owner);
    far.holders.insert(key, holder);
    far.set_queued(VecDeque::from([(key, BTreeSet::from([row.form_id]))]));
    far.pending.insert(
        pending_key,
        Pending {
            ids: BTreeSet::from([row.form_id]),
            receiver,
            ready: None,
        },
    );
    app.insert_resource(far);
    for pass in 0..2 {
        app.update();
        let metrics = app.world().resource::<StreamingMetrics>();
        assert_eq!(metrics.queued_far_stock_queries, 1);
        assert_eq!(metrics.pending_far_stock_queries, 1);
        assert_eq!(metrics.failed_far_stock_sources, 1);
        let far = app.world().resource::<FarStock>();
        let cell = far.cell_state(key);
        assert_eq!(cell.pending, 1);
        assert!(cell.failed);
        assert_eq!(cell.holder, Some(holder));
        assert!(far.cell_state(pending_key).pending > 0);
        // The demand planner clears transient per-demand failures. The still-current marker
        // must be rediscovered, without requiring it to be Added again or claiming a draw proof.
        if pass == 0 {
            let mut far = app.world_mut().resource_mut::<FarStock>();
            far.generation = far.generation.wrapping_add(1);
            far.reset_failures();
        }
    }
}

#[test]
fn complete_near_admission_preserves_an_edited_source_through_retirement_and_recovered_feedback() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let key = *key;
    let placed = fixture.package.placed_reference(row.form_id).unwrap();
    assert_eq!(placed.coverage, CoverageState::Complete);
    assert!(!needs_stock(placed));
    let identity = (cell_world(key).unwrap(), row.form_id);
    let mut app = complete_plan_app(&fixture, key);
    app.update();
    let demand = app.world().resource::<FarStock>().demand;
    let generation = app.world().resource::<FarStock>().generation;
    assert!(demand.is_some());
    assert!(app.world().resource::<FarStock>().desired.is_empty());

    let near = root(app.world_mut(), key, false);
    let (source, model) = source(app.world_mut(), key, near, row, false);
    let entity = source.entity;
    let handle = app.world().get::<PendingModel>(entity).unwrap().handle.id();
    let started = app
        .world()
        .get::<PendingAssetProfile>(entity)
        .unwrap()
        .started;
    with_far(app.world_mut(), |world, far| {
        register_near(world, far, near, key)
    });
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.demand, demand);
    assert_eq!(far.generation, generation);
    assert_eq!(far.desired.get(&identity), Some(&key));
    assert_eq!(far.owners[&identity].entity, entity);
    assert!(
        far.owners[&identity].row.is_none(),
        "admission borrowed the existing source"
    );

    let mut edited = *app.world().get::<Transform>(entity).unwrap();
    edited.translation += Vec3::new(128.0, 256.0, -512.0);
    edited.rotation = Quat::from_rotation_y(0.7);
    edited.scale = Vec3::new(0.5, 1.5, 2.5);
    app.world_mut().entity_mut(entity).insert(edited);
    let before_release = app.world().entity_count();
    let mut retirement = bevy::ecs::system::SystemState::<(
        Commands,
        Query<(&Visibility, Option<&crate::streaming::RetireHidden>)>,
    )>::new(app.world_mut());
    {
        let (mut commands, roots) = retirement.get_mut(app.world_mut()).unwrap();
        queue_release(&mut commands, near);
        crate::streaming::hide_retiring_root(&mut commands, &roots, near);
    }
    retirement.apply(app.world_mut());
    let holder = app.world().resource::<FarStock>().holders[&key];
    assert_eq!(
        app.world().entity_count(),
        before_release + 1,
        "only the sparse owner root is new"
    );
    assert_eq!(
        app.world().get::<Visibility>(near),
        Some(&Visibility::Hidden)
    );
    assert_eq!(app.world().get::<ChildOf>(entity).unwrap().parent(), holder);
    assert_eq!(*app.world().get::<Transform>(entity).unwrap(), edited);
    assert!(app.world().get::<ColliderDisabled>(entity).is_some());
    despawn_subtree(app.world_mut(), near);
    assert!(app.world().get_entity(near).is_err());

    for negatives in [BTreeSet::from([row.form_id]), BTreeSet::new()] {
        app.world_mut()
            .resource_mut::<SourceFallbackDemand>()
            .update(Some(&fixture.package), negatives);
        app.update();
        let far = app.world().resource::<FarStock>();
        assert_eq!(far.demand, demand);
        assert_eq!(
            far.generation, generation,
            "effective owned-source demand did not change"
        );
        assert_eq!(far.desired.get(&identity), Some(&key));
        assert_eq!(far.owners[&identity].entity, entity);
        assert!(far.owners[&identity].far);
        assert_eq!(app.world().get::<ChildOf>(entity).unwrap().parent(), holder);
        assert_eq!(app.world().get::<ChildOf>(model).unwrap().parent(), entity);
        assert_eq!(*app.world().get::<Transform>(entity).unwrap(), edited);
        assert_eq!(
            app.world().get::<PendingModel>(entity).unwrap().handle.id(),
            handle
        );
        assert_eq!(
            app.world()
                .get::<PendingAssetProfile>(entity)
                .unwrap()
                .started,
            started
        );
        assert!(far.queued.is_empty());
        assert!(far.pending.is_empty());
        assert_no_exit(app.world());
    }
}

#[test]
fn complete_feedback_is_current_and_foreign_world_revisions_keep_inflight_work() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let key = *key;
    let world_id = cell_world(key).unwrap();
    let foreign_id = fixture
        .rows
        .iter()
        .find(|(other, _)| cell_world(*other) != Some(world_id))
        .unwrap()
        .1
        .form_id;
    assert_eq!(
        fixture
            .package
            .placed_reference(row.form_id)
            .unwrap()
            .coverage,
        CoverageState::Complete
    );
    let identity = (world_id, row.form_id);
    let mut app = complete_plan_app(&fixture, key);
    app.update();
    assert!(app.world().resource::<FarStock>().desired.is_empty());
    assert!(app.world().resource::<FarStock>().cell_state(key).pending > 0);
    assert_eq!(
        app.world()
            .resource::<crate::streaming::StreamingMetrics>()
            .pending_far_stock_qualification,
        1
    );

    let wrong_certificate = Arc::new(
        crate::cluster::overlay_fixture::open_fixture(fixture.directory.path(), false).unwrap(),
    );
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&wrong_certificate), BTreeSet::from([row.form_id]));
    app.update();
    assert!(
        app.world().resource::<FarStock>().desired.is_empty(),
        "identical bytes from another allocation are not current feedback"
    );
    assert_eq!(
        app.world()
            .resource::<crate::streaming::StreamingMetrics>()
            .pending_far_stock_qualification,
        1
    );
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::from([row.form_id]));
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.desired.get(&identity), Some(&key));
    assert_eq!(far.queued.len(), 1);
    assert_eq!(
        far.queued.front().unwrap(),
        &(key, BTreeSet::from([row.form_id]))
    );
    assert_eq!(
        app.world()
            .resource::<crate::streaming::StreamingMetrics>()
            .pending_far_stock_qualification,
        0
    );
    let generation = far.generation;
    let (responses, receiver) = crossbeam_channel::bounded(1);
    {
        let mut far = app.world_mut().resource_mut::<FarStock>();
        far.clear_queued();
        far.pending.insert(
            key,
            Pending {
                ids: BTreeSet::from([row.form_id]),
                receiver,
                ready: None,
            },
        );
    }
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(
            Some(&fixture.package),
            BTreeSet::from([row.form_id, foreign_id]),
        );
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.generation, generation);
    assert_eq!(far.desired, HashMap::from([(identity, key)]));
    assert_eq!(far.pending[&key].ids, BTreeSet::from([row.form_id]));
    assert!(far.queued.is_empty());
    assert_eq!(
        app.world()
            .resource::<crate::streaming::StreamingMetrics>()
            .pending_far_stock_qualification,
        0
    );
    responses
        .try_send(ReferenceResponse {
            generation,
            key,
            result: Ok(vec![row.clone()]),
            query_micros: 0,
            queue_wait_micros: 0,
            total_request_micros: 0,
        })
        .expect("the original in-flight response receiver survived the unrelated revision");
    let response = far.pending[&key].receiver.try_recv().unwrap();
    assert_eq!(response.generation, generation);
    assert_eq!(response.result.unwrap()[0].form_id, row.form_id);
    app.update(); // No new feedback: a retained set alone cannot satisfy current qualification.
    assert_eq!(app.world().resource::<FarStock>().generation, generation);
    assert_eq!(
        app.world()
            .resource::<crate::streaming::StreamingMetrics>()
            .pending_far_stock_qualification,
        1
    );
    assert!(app.world().resource::<FarStock>().cell_state(key).pending > 1);
    assert_no_exit(app.world());
}

#[test]
fn owner_indexes_follow_replacement_removal_retention_and_withdrawal() {
    let mut world = World::new();
    let near = world.spawn_empty().id();
    let holder = world.spawn_empty().id();
    let replacement = world.spawn_empty().id();
    let key = exterior(17, IVec2::new(3, -2));
    let other_key = exterior(18, grid(key));
    let id = (17, 42);
    let second = (18, 42);
    let owner = Owner {
        entity: world.spawn_empty().id(),
        parent: near,
        key,
        near_key: Some(key),
        near_root: Some(near),
        far: false,
        row: None,
        physics_requested: false,
    };
    let mut far = FarStock::default();
    far.put_owner(id, owner.clone());
    assert_eq!(far.by_root[&near], HashSet::from([id]));
    assert_bookkeeping(&far);

    // Replacement is a borrowed far source: both the holder and remembered near root matter.
    let mut borrowed = owner.clone();
    borrowed.parent = holder;
    borrowed.far = true;
    far.put_owner(id, borrowed.clone());
    assert_eq!(far.by_root[&near], HashSet::from([id]));
    assert_eq!(far.by_root[&holder], HashSet::from([id]));
    assert_eq!(far.by_near_key[&key], HashSet::from([id]));
    assert_bookkeeping(&far);

    // Reassociation must remove old buckets, even when parent and near_root used to coincide.
    borrowed.near_root = Some(replacement);
    borrowed.near_key = Some(other_key);
    far.put_owner(id, borrowed.clone());
    assert!(!far.by_root.contains_key(&near));
    assert!(!far.by_near_key.contains_key(&key));
    far.put_owner(second, borrowed);
    assert_bookkeeping(&far);
    assert_eq!(far.by_root[&holder], HashSet::from([id, second]));
    assert!(far.remove_owner(&(17, 999)).is_none());
    let retired = HashMap::from([(id, far.remove_owner(&id).unwrap())]);
    assert_eq!(
        retired.keys().copied().collect::<HashSet<_>>(),
        HashSet::from([id])
    );
    assert_eq!(far.by_root[&holder], HashSet::from([second]));
    assert_bookkeeping(&far);
    let detached = far.take_owners();
    assert_eq!(detached.len(), 1);
    assert!(far.owners.is_empty());
    assert_bookkeeping(&far);

    // Removing a source indexed twice under the same root must leave no empty buckets.
    far.put_owner(id, owner);
    far.remove_owner(&id).unwrap();
    assert_bookkeeping(&far);
    assert!(far.by_root.is_empty());
    assert!(far.by_near_key.is_empty());
}

#[test]
fn queued_membership_tracks_replan_pop_retry_clear_and_qualification() {
    let key = exterior(17, IVec2::new(3, -2));
    let foreign = exterior(18, grid(key));
    let mut far = FarStock {
        demand: Some(LodDemand {
            world: 17,
            center: grid(key),
        }),
        qualification_pending: true,
        ..Default::default()
    };
    far.set_queued(VecDeque::from([
        (key, BTreeSet::from([42])),
        (foreign, BTreeSet::from([42])),
    ]));
    assert_eq!(far.cell_state(key).pending, 2);
    assert_eq!(far.cell_state(foreign).pending, 1);
    assert_bookkeeping(&far);
    let (_sender, receiver) = crossbeam_channel::bounded(1);
    far.pending.insert(
        key,
        Pending {
            ids: BTreeSet::from([42]),
            receiver,
            ready: None,
        },
    );
    assert_eq!(far.cell_state(key).pending, 3);
    let (popped, ids) = far.pop_queued().unwrap();
    assert_eq!(popped, key);
    assert_eq!(far.cell_state(key).pending, 2);
    assert_bookkeeping(&far);
    far.requeue_front(popped, ids);
    assert_eq!(far.queued.front().unwrap().0, key);
    assert_eq!(far.cell_state(key).pending, 3);
    assert_bookkeeping(&far);
    far.set_queued(VecDeque::from([(foreign, BTreeSet::from([43]))]));
    assert_eq!(far.cell_state(key).pending, 2);
    assert_bookkeeping(&far);
    far.clear_queued();
    assert_eq!(far.cell_state(foreign).pending, 0);
    assert_bookkeeping(&far);
    far.pending.remove(&key);
    assert_eq!(far.cell_state(key).pending, 1);
    far.qualification_pending = false;
    assert_eq!(far.cell_state(key).pending, 0);
    assert!(far.pop_queued().is_none());
}

#[test]
fn revived_replacement_root_rebinds_borrowed_owner_without_touching_foreign_near_cell() {
    let fixture = Fixture::new();
    let (key, row, pose) = fixture.selected();
    let identity = (cell_world(key).unwrap(), row.form_id);
    let mut world = world();
    let old = root(&mut world, key, false);
    let holder = root(&mut world, key, true);
    let (mut owner, model) = source(&mut world, key, old, &row, false);
    let entity = owner.entity;
    move_owner(&mut world, &mut owner, holder, key, true, Some(pose)).unwrap();
    let mut far = state(&fixture, key, owner);
    far.holders.insert(key, holder);

    let foreign_key = exterior(cell_world(key).unwrap().wrapping_add(1), grid(key));
    let foreign_root = root(&mut world, foreign_key, false);
    let foreign_identity = (cell_world(foreign_key).unwrap(), row.form_id);
    let (foreign_owner, _) = source(&mut world, foreign_key, foreign_root, &row, false);
    let foreign_entity = foreign_owner.entity;
    far.put_owner(foreign_identity, foreign_owner);
    world.insert_resource(far);
    // The source has already moved to the distant holder, so replacing its empty near root is safe.
    world.entity_mut(old).despawn();
    let replacement = root(&mut world, key, false);
    queue_revive(&mut world.commands(), replacement, key);
    world.flush();

    let far = world.resource::<FarStock>();
    let owner = &far.owners[&identity];
    assert!(!owner.far);
    assert_eq!(owner.parent, replacement);
    assert_eq!(owner.near_root, Some(replacement));
    assert_eq!(owner.near_key, Some(key));
    assert!(!far.by_root.contains_key(&old));
    assert_eq!(far.owners[&foreign_identity].parent, foreign_root);
    assert_eq!(far.owners[&foreign_identity].near_root, Some(foreign_root));
    assert_eq!(
        world.get::<ChildOf>(foreign_entity).unwrap().parent(),
        foreign_root
    );
    assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), replacement);
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    assert_placement(&world, entity, pose, key);
    assert_no_exit(&world);
}

#[test]
fn failed_owner_keeps_existing_diagnostic_until_failure_reset_then_repopulates() {
    use crate::streaming::{AssetFailed, StreamingMetrics};

    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .init_resource::<StreamingMetrics>()
        .add_systems(Update, publish_work);
    let holder = root(app.world_mut(), key, true);
    let (owner, _) = source(app.world_mut(), key, holder, &row, true);
    app.world_mut().entity_mut(owner.entity).insert(AssetFailed);
    let mut far = state(&fixture, key, owner);
    far.failed.insert(key, "existing source failure".into());
    app.insert_resource(far);
    app.update();
    app.update();
    assert_eq!(
        app.world().resource::<FarStock>().failed[&key],
        "existing source failure"
    );
    app.world_mut().resource_mut::<FarStock>().reset_failures();
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.failed[&key],
        format!(
            "reference {:08X} failed ordinary asset validation",
            row.form_id
        )
    );
    assert_bookkeeping(far);
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .failed_far_stock_sources,
        1
    );
}

#[test]
fn full_model_demand_is_bounded_by_the_loaded_grid_and_leaves_interiors_empty() {
    let fixture = Fixture::new();
    let (key, _, _) = fixture.selected();
    let mut app = complete_plan_app(&fixture, key);
    app.world_mut().resource_mut::<EngineConfig>().stream_radius = 0;
    app.update();
    let far = app.world().resource::<FarStock>();
    assert!(!far.desired.is_empty());
    assert!(far.desired.values().all(|wanted| *wanted == key));
    assert_eq!(far.reach, Some(0));
    app.world_mut().insert_resource(super::super::ActiveSpace {
        worldspace_id: Some(cell_world(key).unwrap()),
        interior: Some(77),
    });
    app.update();
    assert!(app.world().resource::<FarStock>().desired.is_empty());
}

#[test]
fn loaded_grid_bounds_are_world_exact_and_inclusive() {
    let mut demand = LodDemand {
        world: 17,
        center: IVec2::new(-3, 4),
    };
    assert!(demand_contains(demand, 17, IVec2::new(-5, 6), 2));
    assert!(!demand_contains(demand, 17, IVec2::new(-6, 6), 2));
    assert!(!demand_contains(demand, 18, IVec2::new(-3, 4), 2));
    assert!(demand_contains(demand, 17, demand.center, -1));
    assert!(!demand_contains(demand, 17, demand.center + IVec2::X, -1));
    demand.world = 18;
    assert!(!demand_contains(demand, 17, demand.center, 2));
    assert!(demand_contains(demand, 18, demand.center, 0));
}

#[test]
fn owned_statics_desire_refused_models_beyond_stream_and_terrain_reach() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut app = complete_plan_app(&fixture, key);
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.cluster_own_statics = true;
        config.stream_radius = 0;
    }
    app.world_mut().resource_mut::<RenderOrigin>().0 = grid(key) + IVec2::new(200, 0);
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.desired.get(&(cell_world(key).unwrap(), row.form_id)),
        Some(&key)
    );
    let generation = far.generation;
    app.update();
    assert_eq!(app.world().resource::<FarStock>().generation, generation);
}

#[test]
fn owned_statics_do_not_request_complete_models_without_fallback() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, _) = fixture.rows.first().unwrap();
    let mut app = complete_plan_app(&fixture, *key);
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .cluster_own_statics = true;
    app.update();
    assert!(app.world().resource::<FarStock>().desired.is_empty());
}

#[test]
fn owned_statics_keep_unclassified_runtime_fallback_within_stream_radius() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let mut app = complete_plan_app(&fixture, *key);
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.cluster_own_statics = true;
        config.stream_radius = 0;
    }
    app.world_mut().resource_mut::<RenderOrigin>().0 = grid(*key) + IVec2::new(200, 0);
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::from([row.form_id]));
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.desired.get(&(cell_world(*key).unwrap(), row.form_id)),
        None
    );
    let generation = far.generation;
    app.update();
    assert_eq!(app.world().resource::<FarStock>().generation, generation);
    app.world_mut().resource_mut::<RenderOrigin>().0 = grid(*key);
    app.update();
    assert_eq!(
        app.world()
            .resource::<FarStock>()
            .desired
            .get(&(cell_world(*key).unwrap(), row.form_id)),
        Some(key)
    );
}

#[test]
fn owned_statics_desire_complete_initial_material_refusal_beyond_grid_with_exact_partial_selection()
{
    use crate::cluster::{
        ownership::OwnershipFacts,
        source_identity::{SourcePrimitive, SourcePrimitiveTransform},
    };
    use bevy::{
        asset::AssetPlugin,
        camera::visibility::VisibilityClass,
        transform::components::TransformTreeChanged,
        world_serialization::{WorldAssetRoot, WorldSerializationPlugin},
    };
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture
        .rows
        .iter()
        .find(|(_, row)| {
            fixture
                .package
                .placed_reference(row.form_id)
                .is_some_and(|reference| {
                    reference.coverage == CoverageState::Complete
                        && reference.is_static_scenery
                        && reference.model_path.is_some()
                        && reference
                            .stock_primitives
                            .as_ref()
                            .is_some_and(BTreeSet::is_empty)
                })
        })
        .expect("loader fixture has a complete static reference without convert refusals");
    let (record, occurrence) = fixture
        .package
        .records()
        .find(|(_, occurrence)| occurrence.reference == row.form_id)
        .unwrap();
    let refused_ordinal = occurrence.primitive;
    let mut facts = OwnershipFacts::default();
    facts.publish(
        &fixture.package,
        std::collections::BTreeMap::from([(*record, true)]),
    );
    let mut app = complete_plan_app(&fixture, *key);
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.cluster_own_statics = true;
        config.stream_radius = 0;
    }
    app.world_mut().resource_mut::<RenderOrigin>().0 = grid(*key) + IVec2::new(200, 0);
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), facts.permanent_refused_references());
    app.insert_resource(facts);
    app.update();
    assert_eq!(
        app.world()
            .resource::<FarStock>()
            .desired
            .get(&(cell_world(*key).unwrap(), row.form_id)),
        Some(key),
        "certified initial material refusal must inherit owned scenery reach"
    );

    // Exercise the actual pre-instancing subset selector. A same-bounds new
    // source occurrence is not permission to render anything outside the mask.
    app.add_plugins((AssetPlugin::default(), WorldSerializationPlugin))
        .init_asset::<Mesh>()
        .register_type::<Transform>()
        .register_type::<GlobalTransform>()
        .register_type::<TransformTreeChanged>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<Visibility>()
        .register_type::<InheritedVisibility>()
        .register_type::<ViewVisibility>()
        .register_type::<VisibilityClass>()
        .register_type::<Mesh3d>()
        .register_type::<MeshMaterial3d<StandardMaterial>>()
        .register_type::<SourcePrimitive>()
        .register_type::<SourcePrimitiveTransform>();
    super::super::partial_stock::install(&mut app);
    let mesh = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(2.0, 2.0, 2.0));
    let mut scene = World::new();
    for ordinal in [refused_ordinal, u32::MAX] {
        scene.spawn((
            Transform::IDENTITY,
            Mesh3d(mesh.clone()),
            MeshMaterial3d::<StandardMaterial>::default(),
            SourcePrimitive {
                scene_index: 0,
                ordinal,
                mesh_index: 0,
                primitive_index: ordinal,
                matches_default_scene: true,
            },
            SourcePrimitiveTransform(Mat4::IDENTITY),
        ));
    }
    let source = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(scene));
    let partial = super::super::partial_stock::PartialStock::for_demand(
        &fixture.package,
        row,
        true,
        true,
        Some(app.world().resource::<OwnershipFacts>()),
    )
    .unwrap();
    let root = app
        .world_mut()
        .spawn((
            Transform::IDENTITY,
            partial,
            ExpectedModelBounds::new(Vec3::splat(-1.0), Vec3::ONE).unwrap(),
        ))
        .id();
    super::super::partial_stock::arm(app.world_mut(), root, source.clone());
    let selected = &app.world().get::<WorldAssetRoot>(root).unwrap().0;
    assert_ne!(selected, &source);
    let selected_ordinals: BTreeSet<_> = app
        .world()
        .resource::<Assets<WorldAsset>>()
        .get(selected)
        .unwrap()
        .world
        .iter_entities()
        .filter(|node| node.contains::<Mesh3d>())
        .map(|node| node.get::<SourcePrimitive>().unwrap().ordinal)
        .collect();
    assert_eq!(selected_ordinals, BTreeSet::from([refused_ordinal]));

    // Runtime feedback withdrawal does not replace the certificate's fixed
    // refusal. A later camera demand must still include this distant source.
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(None, BTreeSet::new());
    app.world_mut().resource_mut::<RenderOrigin>().0 = grid(*key) + IVec2::new(201, 0);
    app.update();
    assert_eq!(
        app.world()
            .resource::<FarStock>()
            .desired
            .get(&(cell_world(*key).unwrap(), row.form_id)),
        Some(key),
        "feedback withdrawal cannot revoke fixed refusal reach on camera movement"
    );
    let generation = app.world().resource::<FarStock>().generation;
    app.update();
    assert_eq!(
        app.world().resource::<FarStock>().generation,
        generation,
        "settled fixed-refusal demand must not replan"
    );
}

#[test]
fn initially_disabled_runtime_refusals_stay_hidden_at_all_radii() {
    let fixture = Fixture::from_directory(published_for_runtime_inventory());
    let (key, row) = fixture
        .rows
        .iter()
        .find(|(_, row)| {
            fixture
                .package
                .placed_reference(row.form_id)
                .unwrap()
                .refusal_class
                == StockRefusalClass::Runtime
        })
        .unwrap();
    let mut app = complete_plan_app(&fixture, *key);
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.cluster_own_statics = true;
        config.stream_radius = 0;
    }
    app.world_mut().resource_mut::<RenderOrigin>().0 = grid(*key) + IVec2::new(200, 0);
    app.update();
    assert!(
        !app.world()
            .resource::<FarStock>()
            .desired
            .contains_key(&(cell_world(*key).unwrap(), row.form_id))
    );
    app.world_mut().resource_mut::<EngineConfig>().stream_radius = 200;
    app.update();
    assert_eq!(
        app.world()
            .resource::<FarStock>()
            .desired
            .get(&(cell_world(*key).unwrap(), row.form_id)),
        None
    );
    let generation = app.world().resource::<FarStock>().generation;
    app.update();
    assert_eq!(app.world().resource::<FarStock>().generation, generation);
}

#[test]
fn undrawn_paths_report_actual_missing_distant_static_paths_once_at_qualification() {
    let fixture = Fixture::from_directory(published_for_runtime_inventory());
    let key = exterior(0x70, IVec2::ZERO);
    let mut app = complete_plan_app(&fixture, key);
    app.edit_schedule(Update, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.cluster_own_statics = true;
        config.stream_radius = 0;
    }
    app.update();
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::new());
    app.update();
    // The look refusal has stock; runtime state hides 0x109. Only the unparsed static lacks a route.
    let far = app.world().resource::<FarStock>();
    assert!(far.desired.contains_key(&(0x70, 0x103)));
    assert!(!far.desired.contains_key(&(0x70, 0x105)));
    assert!(!far.desired.contains_key(&(0x70, 0x109)));
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::new());
    app.update();
    app.world_mut().resource_mut::<RenderOrigin>().0 += IVec2::X;
    app.update();
}

#[test]
fn undrawn_paths_record_zero_for_complete_runtime_feedback_without_stock_permission() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let mut app = complete_plan_app(&fixture, *key);
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.cluster_own_statics = true;
        config.stream_radius = 0;
    }
    app.world_mut().resource_mut::<RenderOrigin>().0 = grid(*key) + IVec2::new(200, 0);
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::from([row.form_id]));
    app.update();
    assert!(
        !app.world()
            .resource::<FarStock>()
            .desired
            .contains_key(&(cell_world(*key).unwrap(), row.form_id))
    );
}

#[test]
fn undrawn_paths_record_zero_when_every_refusal_is_inside_grid() {
    let fixture = Fixture::from_directory(published_for_runtime_inventory());
    let mut app = complete_plan_app(&fixture, exterior(0x70, IVec2::ZERO));
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .cluster_own_statics = true;
    app.world_mut().resource_mut::<EngineConfig>().stream_radius = 2;
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::new());
    app.update();
    assert!(
        app.world()
            .resource::<FarStock>()
            .desired
            .contains_key(&(0x70, 0x105))
    );
    assert!(
        !app.world()
            .resource::<FarStock>()
            .desired
            .contains_key(&(0x70, 0x109))
    );
}

#[test]
fn undrawn_paths_report_legacy_threshold_holes_without_adding_stock() {
    let fixture = Fixture::from_directory(published_for_legacy_threshold_inventory());
    let key = exterior(0x70, IVec2::new(200, 0));
    let mut app = complete_plan_app(&fixture, key);
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.cluster_own_statics = true;
        config.stream_radius = 0;
    }
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::new());
    app.update();
    let reference = fixture.package.placed_reference(0x100).unwrap();
    assert_eq!(reference.coverage, CoverageState::Partial);
    assert_eq!(reference.refusal_class, StockRefusalClass::Other);
    assert!(fixture.package.reference(0x100).is_none());
    assert!(
        !app.world()
            .resource::<FarStock>()
            .desired
            .contains_key(&(0x70, 0x100))
    );
    // One threshold hole and one unparsed static; the look refusal has stock.
}

#[test]
fn only_model_bearing_look_or_runtime_refused_static_scenery_has_full_stock_reach() {
    let fixture = Fixture::new();
    let (_, row, _) = fixture.selected();
    let mut reference = fixture
        .package
        .placed_reference(row.form_id)
        .unwrap()
        .clone();
    for (class, is_static, expected) in [
        (StockRefusalClass::Look, true, true),
        (StockRefusalClass::Look, false, false),
        // The reader projects mixed runtime/look reasons to Runtime as well.
        (StockRefusalClass::Runtime, true, true),
        (StockRefusalClass::Runtime, false, false),
        (StockRefusalClass::Other, true, false),
        (StockRefusalClass::None, true, false),
    ] {
        reference.refusal_class = class;
        reference.is_static_scenery = is_static;
        assert_eq!(
            full_stock_reach(&reference),
            expected,
            "{class:?}, static={is_static}"
        );
    }
    reference.refusal_class = StockRefusalClass::Look;
    reference.coverage = CoverageState::Complete;
    assert!(!full_stock_reach(&reference));
    reference.coverage = CoverageState::Partial;
    reference.model_path = None;
    assert!(!full_stock_reach(&reference));
}

#[test]
fn complete_sources_cannot_bypass_grid_reach_via_near_admission() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let reference = fixture.package.placed_reference(row.form_id).unwrap();
    let mut demand = LodDemand {
        world: cell_world(*key).unwrap(),
        center: grid(*key) + IVec2::X,
    };
    assert_eq!(
        complete_source_key(
            &fixture.package,
            demand,
            0,
            row.form_id,
            reference.snapshot.source_cell_id
        ),
        None
    );
    demand.center = grid(*key);
    assert_eq!(
        complete_source_key(
            &fixture.package,
            demand,
            0,
            row.form_id,
            reference.snapshot.source_cell_id
        ),
        Some(*key)
    );
}

#[test]
fn package_window_handles_sparse_unbounded_and_inclusive_bounded_queries() {
    let fixture = Fixture::new();
    let (key, _, _) = fixture.selected();
    let world = cell_world(key).unwrap();
    let cells = |center, reach| {
        fixture
            .package
            .placed_cells_in_window(world, center, reach)
            .map(|(grid, _)| grid.to_array())
            .collect::<Vec<_>>()
    };
    assert_eq!(cells(IVec2::splat(i32::MAX), None), vec![[-2, 1], [0, 0]]);
    assert_eq!(cells(IVec2::ZERO, Some(2)), vec![[-2, 1], [0, 0]]);
    assert_eq!(cells(IVec2::ZERO, Some(1)), vec![[0, 0]]);
    assert!(cells(IVec2::ZERO, Some(-1)).is_empty());
    assert!(cells(IVec2::splat(i32::MAX), Some(0)).is_empty());
}

#[path = "far_stock_refresh_tests.rs"]
mod refresh;

#[path = "far_stock_delta_tests.rs"]
mod delta_plan;

#[path = "far_stock_polling_tests.rs"]
mod polling;

#[path = "far_stock_full_state_tests.rs"]
mod full_state;

// Fixture seeding for isolated dispatch/state tests, not a planner implementation.
impl FarStock {
    fn set_queued(&mut self, queued: VecDeque<(CellKey, BTreeSet<u32>)>) {
        self.queued_keys = queued.iter().map(|(key, _)| *key).collect();
        self.queued = queued;
    }
}

#[test]
fn marker_policy_agrees_between_near_drawability_far_demand_and_row_authority() {
    use crate::cluster::reader::producer_fixture_tests::published_for_marker_policy;
    for flagged in [false, true] {
        for refuse_geometry in [false, true] {
            let fixture =
                Fixture::from_directory(published_for_marker_policy(flagged, refuse_geometry));
            for (_, row) in &fixture.rows {
                let placed = fixture.package.placed_reference(row.form_id).unwrap();
                assert_eq!(row.base_is_marker, flagged);
                assert_eq!(
                    super::super::spawned_model_path(row.model_path.clone(), row.base_is_marker)
                        .as_deref(),
                    if flagged {
                        None
                    } else {
                        Some("meshes/arch/rockmarker.glb")
                    }
                );
                assert_eq!(needs_stock(placed), !flagged && refuse_geometry);
                assert!(row_matches(&fixture.package, row));
                let mut stale = row.clone();
                stale.base_is_marker = !flagged;
                assert!(!row_matches(&fixture.package, &stale));
            }
        }
    }
}

#[path = "far_stock_event_gating_tests.rs"]
mod event_gating;
