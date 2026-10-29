//! Per-World work evidence: no thread-local state crosses App::update.
use super::*;
use crate::streaming::{AssetFailed, StreamingMetrics};

fn failure_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .init_resource::<FarStock>()
        .init_resource::<StreamingMetrics>()
        .add_systems(Update, publish_work);
    app
}

fn failed_owner(app: &mut App, key: CellKey, form: u32, entity: Entity, parent: Entity) {
    let mut far = app.world_mut().resource_mut::<FarStock>();
    far.demand = Some(LodDemand {
        world: cell_world(key).unwrap(),
        center: grid(key),
    });
    far.put_owner(
        (cell_world(key).unwrap(), form),
        Owner {
            entity,
            parent,
            key,
            near_key: None,
            near_root: None,
            far: true,
            row: None,
            physics_requested: false,
        },
    );
}

#[test]
fn settled_populated_worlds_do_no_entity_discovery_or_near_reconciliation() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut apps: Vec<_> = [32, 1024]
        .into_iter()
        .map(|population| {
            let mut app = complete_plan_app(&fixture, key);
            let parent = root(app.world_mut(), key, false);
            app.world_mut()
                .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(parent)));
            for offset in 0..population {
                app.world_mut().spawn((
                    FormId(u32::MAX - offset),
                    CellRef(0),
                    ChildOf(parent),
                    AssetFailed,
                ));
            }
            for _ in 0..4 {
                app.update();
            }
            app
        })
        .collect();
    let before: Vec<_> = apps
        .iter()
        .map(|app| {
            let far = app.world().resource::<FarStock>();
            (far.planning_work, far.near_sources.polling_work)
        })
        .collect();
    for _ in 0..4 {
        for app in &mut apps {
            app.update();
        }
    }
    for (app, (work, polling)) in apps.iter().zip(before) {
        let far = app.world().resource::<FarStock>();
        assert_eq!(
            far.planning_work, work,
            "actual per-World iteration counters must stay flat"
        );
        assert_eq!(
            far.near_sources.polling_work.reference_candidates_upper,
            polling.reference_candidates_upper
        );
        assert_eq!(
            far.near_sources.polling_work.root_candidates_upper,
            polling.root_candidates_upper
        );
        assert!(
            !app.world()
                .contains_resource::<crate::streaming::failure::LoadingRefusal>()
        );
        assert_eq!(
            far.owners[&(cell_world(key).unwrap(), row.form_id)].parent,
            far.near_roots[&key]
        );
    }
}

#[test]
fn old_failure_is_discovered_once_when_ownership_arrives_and_then_stays_settled() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut app = failure_app();
    let parent = app.world_mut().spawn_empty().id();
    let entity = app
        .world_mut()
        .spawn((FormId(row.form_id), AssetFailed))
        .id();
    for _ in 0..3 {
        app.update();
    }
    assert!(
        !app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
    let before = app.world().resource::<FarStock>().planning_work;
    failed_owner(&mut app, key, row.form_id, entity, parent);
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.failed[&key],
        format!(
            "reference {:08X} failed ordinary asset validation",
            row.form_id
        )
    );
    assert_eq!(
        far.planning_work.failure_discoveries - before.failure_discoveries,
        1
    );
    assert_eq!(
        far.planning_work.failure_entity_entries - before.failure_entity_entries,
        1
    );
    let after = far.planning_work;
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(app.world().resource::<FarStock>().planning_work, after);
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .failed_far_stock_sources,
        1
    );
    assert_eq!(
        app.world_mut()
            .resource_mut::<Messages<AppExit>>()
            .drain()
            .filter(|exit| exit.is_error())
            .count(),
        1
    );
}

#[test]
fn failure_clear_and_cell_retry_each_rediscover_an_old_marker_exactly_once() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    for reset_all in [true, false] {
        let mut app = failure_app();
        let parent = app.world_mut().spawn_empty().id();
        let entity = app
            .world_mut()
            .spawn((FormId(row.form_id), AssetFailed))
            .id();
        failed_owner(&mut app, key, row.form_id, entity, parent);
        app.world_mut()
            .resource_mut::<FarStock>()
            .failed
            .insert(key, "first diagnostic".into());
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(
            app.world().resource::<FarStock>().failed[&key],
            "first diagnostic"
        );
        let before = app.world().resource::<FarStock>().planning_work;
        {
            let mut far = app.world_mut().resource_mut::<FarStock>();
            if reset_all {
                far.reset_failures();
            } else {
                far.retry_failure(&key);
            }
        }
        app.update();
        let far = app.world().resource::<FarStock>();
        assert_eq!(
            far.failed[&key],
            format!(
                "reference {:08X} failed ordinary asset validation",
                row.form_id
            )
        );
        assert_eq!(
            far.planning_work.failure_discoveries - before.failure_discoveries,
            1
        );
        assert_eq!(
            far.planning_work.failure_entity_entries - before.failure_entity_entries,
            1
        );
        let after = far.planning_work;
        app.update();
        assert_eq!(app.world().resource::<FarStock>().planning_work, after);
        assert!(
            app.world()
                .resource::<crate::streaming::failure::LoadingRefusal>()
                .0
                .contains("first diagnostic")
        );
    }
}

#[test]
fn near_consumer_does_not_consume_asset_failure_lifecycle_wakeup() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut app = complete_plan_app(&fixture, key);
    let parent = root(app.world_mut(), key, false);
    let entity = app
        .world_mut()
        .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(parent)))
        .id();
    for _ in 0..4 {
        app.update();
    }
    let before = app.world().resource::<FarStock>().planning_work;
    app.world_mut().entity_mut(entity).insert(AssetFailed);
    app.update(); // plan consumes near changes before publish_work reads failure changes.
    let far = app.world().resource::<FarStock>();
    let diagnostic = far.failed[&key].clone();
    assert_eq!(
        far.planning_work.failure_discoveries - before.failure_discoveries,
        1
    );
    assert_eq!(
        far.planning_work.failure_entity_entries - before.failure_entity_entries,
        1
    );
    let inserted = far.planning_work;
    app.update();
    assert_eq!(app.world().resource::<FarStock>().planning_work, inserted);
    app.world_mut().entity_mut(entity).remove::<AssetFailed>();
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.planning_work.failure_discoveries - inserted.failure_discoveries,
        1
    );
    assert_eq!(
        far.planning_work.failure_entity_entries,
        inserted.failure_entity_entries
    );
    assert_eq!(
        far.failed[&key], diagnostic,
        "removing a marker must not erase terminal evidence"
    );
    let removed = far.planning_work;
    app.update();
    assert_eq!(app.world().resource::<FarStock>().planning_work, removed);
}

#[test]
fn late_near_owner_reconciles_even_when_the_root_activity_value_is_unchanged() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let id = (cell_world(key).unwrap(), row.form_id);
    let mut world = world();
    let parent = root(&mut world, key, false);
    world.entity_mut(parent).insert(Visibility::Hidden);
    {
        let mut far = world.resource_mut::<FarStock>();
        far.package = Some(fixture.package.clone());
        far.demand = Some(LodDemand {
            world: id.0,
            center: grid(key),
        });
        far.set_target(id, Some(key));
        far.near_roots.insert(key, parent);
    }
    with_far(&mut world, reconcile_near); // Cache the inactive root before this source exists.
    let (owner, model) = source(&mut world, key, parent, &row, false);
    let entity = owner.entity;
    let pending = world.get::<PendingModel>(entity).unwrap().handle.id();
    world.resource_mut::<FarStock>().put_owner(id, owner);
    let before = world.resource::<FarStock>().planning_work;
    with_far(&mut world, reconcile_near);
    let far = world.resource::<FarStock>();
    assert!(far.owners[&id].far);
    assert_ne!(far.owners[&id].parent, parent);
    assert_eq!(
        far.planning_work.near_root_entries - before.near_root_entries,
        1
    );
    let after = far.planning_work;
    with_far(&mut world, reconcile_near);
    assert_eq!(world.resource::<FarStock>().planning_work, after);
    assert_eq!(
        world.get::<PendingModel>(entity).unwrap().handle.id(),
        pending
    );
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    assert_no_exit(&world);
}

#[test]
fn active_space_change_with_no_camera_demand_still_wakes_near_roles() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut app = complete_plan_app(&fixture, key);
    let parent = root(app.world_mut(), key, false);
    app.world_mut()
        .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(parent)));
    for _ in 0..4 {
        app.update();
    }
    // Remove the camera: demand remains None across the two following active-space writes.
    let camera = app
        .world_mut()
        .query_filtered::<Entity, With<crate::world::components::StreamingCamera>>()
        .single(app.world())
        .unwrap();
    app.world_mut().despawn(camera);
    for _ in 0..3 {
        app.update();
    }
    let before = app
        .world()
        .resource::<FarStock>()
        .planning_work
        .near_root_entries;
    app.world_mut().resource_mut::<ActiveSpace>().interior = Some(row.cell_id);
    app.update();
    assert_eq!(
        app.world()
            .resource::<FarStock>()
            .planning_work
            .near_root_entries
            - before,
        1
    );
    assert!(!app.world().resource::<FarStock>().near_states[&parent]);
    let after = app.world().resource::<FarStock>().planning_work;
    app.update();
    assert_eq!(app.world().resource::<FarStock>().planning_work, after);
    app.world_mut().resource_mut::<ActiveSpace>().interior = None;
    app.update();
    assert!(app.world().resource::<FarStock>().near_states[&parent]);
    assert_eq!(
        app.world()
            .resource::<FarStock>()
            .planning_work
            .near_root_entries
            - after.near_root_entries,
        1
    );
}

#[test]
fn package_arrival_indexes_sources_that_predate_observer_installation() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let id = (cell_world(key).unwrap(), row.form_id);
    let mut app = complete_plan_app(&fixture, key);
    app.world_mut().remove_resource::<ValidatedPackage>();
    let parent = root(app.world_mut(), key, false);
    let entity = app
        .world_mut()
        .spawn((
            FormId(id.1),
            CellRef(row.cell_id),
            ChildOf(parent),
            AssetFailed,
        ))
        .id();
    for _ in 0..3 {
        app.update();
    }
    assert!(!app.world().resource::<FarStock>().owners.contains_key(&id));
    assert!(
        !app.world()
            .contains_resource::<crate::streaming::failure::LoadingRefusal>()
    );
    app.insert_resource(ValidatedPackage(fixture.package.clone()));
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.owners[&id].entity, entity);
    assert!(far.failed[&key].contains("ordinary asset validation"));
    for _ in 0..3 {
        app.update();
    }
    let after = app.world().resource::<FarStock>().planning_work;
    app.update();
    assert_eq!(app.world().resource::<FarStock>().planning_work, after);
}

#[test]
fn missing_journal_recovery_discovers_current_failures_and_then_returns_to_idle() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    let mut app = complete_plan_app(&fixture, key);
    let parent = root(app.world_mut(), key, false);
    let entity = app
        .world_mut()
        .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(parent)))
        .id();
    for _ in 0..4 {
        app.update();
    }
    // Model an explicit journal gap: the mutation occurs while the journal is absent.
    app.world_mut().remove_resource::<domain::DomainChanges>();
    app.world_mut().entity_mut(entity).insert(AssetFailed);
    app.update();
    assert!(app.world().resource::<FarStock>().failed[&key].contains("ordinary asset validation"));
    assert_eq!(
        app.world().resource::<FarStock>().owners[&(cell_world(key).unwrap(), row.form_id)].entity,
        entity
    );
    for _ in 0..3 {
        app.update();
    }
    let after = app.world().resource::<FarStock>().planning_work;
    let polling = app.world().resource::<FarStock>().near_sources.polling_work;
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.planning_work, after);
    assert_eq!(
        far.near_sources.polling_work.reference_candidates_upper,
        polling.reference_candidates_upper
    );
    assert_eq!(
        far.near_sources.polling_work.root_candidates_upper,
        polling.root_candidates_upper
    );
}

#[derive(Component)]
struct HiddenRequiredSource;

#[test]
fn retained_failed_owners_refuse_under_builtin_and_custom_default_filters() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    for disabler in [0, 1, 2] {
        let mut app = failure_app();
        app.world_mut()
            .register_disabling_component::<HiddenRequiredSource>();
        let parent = app.world_mut().spawn_empty().id();
        let source = app
            .world_mut()
            .spawn((FormId(row.form_id), AssetFailed, Visibility::Hidden))
            .id();
        match disabler {
            1 => {
                app.world_mut()
                    .entity_mut(source)
                    .insert(bevy::ecs::entity_disabling::Disabled);
            }
            2 => {
                app.world_mut()
                    .entity_mut(source)
                    .insert(HiddenRequiredSource);
            }
            _ => {}
        }
        // A failed source without a current owner is not a terminal obligation.
        for _ in 0..3 {
            app.update();
        }
        assert!(
            !app.world()
                .contains_resource::<crate::streaming::failure::LoadingRefusal>()
        );
        let mut exits = app
            .world()
            .resource::<Messages<AppExit>>()
            .get_cursor_current();
        failed_owner(&mut app, key, row.form_id, source, parent);
        app.update();
        assert!(
            app.world().resource::<FarStock>().failed.contains_key(&key),
            "default query filters cannot erase an exact retained owner's failure: {disabler}"
        );
        assert!(
            app.world()
                .contains_resource::<crate::streaming::failure::LoadingRefusal>()
        );
        assert_eq!(
            exits
                .read(app.world().resource::<Messages<AppExit>>())
                .cloned()
                .collect::<Vec<_>>(),
            [AppExit::error()]
        );
        let settled = app.world().resource::<FarStock>().planning_work;
        for _ in 0..4 {
            app.update();
            assert!(
                exits
                    .read(app.world().resource::<Messages<AppExit>>())
                    .next()
                    .is_none()
            );
        }
        assert_eq!(app.world().resource::<FarStock>().planning_work, settled);
    }
}

#[test]
fn incompatible_reparent_preserves_the_actual_source_root_until_terminal_exit() {
    let fixture = Fixture::new();
    let (key, row, _) = fixture.selected();
    for earlier in [None, Some("earlier required failure")] {
        let mut app = complete_plan_app(&fixture, key);
        let previous = root(app.world_mut(), key, false);
        let source = app
            .world_mut()
            .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(previous)))
            .id();
        app.update();
        app.update();
        let id = (cell_world(key).unwrap(), row.form_id);
        assert_eq!(
            app.world().resource::<FarStock>().owners[&id].entity,
            source
        );
        let incompatible = exterior(cell_world(key).unwrap() + 1, grid(key));
        let current = root(app.world_mut(), incompatible, false);
        let mut exits = app
            .world()
            .resource::<Messages<AppExit>>()
            .get_cursor_current();
        if let Some(reason) = earlier {
            crate::streaming::failure::refuse_loading(app.world_mut(), reason.into());
        }
        app.world_mut().entity_mut(source).insert(ChildOf(current));
        app.update();
        assert!(app.world().get::<FailedOwnerRoot>(previous).is_some());
        let removed = crate::streaming::despawn_subtree(app.world_mut(), current);
        assert_eq!(
            removed, 0,
            "incompatible ownership must protect the source's current ancestor before removal"
        );
        assert!(app.world().get_entity(current).is_ok());
        assert!(app.world().get_entity(source).is_ok());
        assert!(app.world().get::<FailedOwnerRoot>(current).is_some());
        let reason = &app
            .world()
            .resource::<crate::streaming::failure::LoadingRefusal>()
            .0;
        if let Some(earlier) = earlier {
            assert_eq!(reason, earlier);
        } else {
            assert!(reason.contains("incompatibly"));
        }
        assert_eq!(
            exits
                .read(app.world().resource::<Messages<AppExit>>())
                .cloned()
                .collect::<Vec<_>>(),
            [AppExit::error()]
        );
    }
}

include!("far_stock_settled_write_tests.rs");
