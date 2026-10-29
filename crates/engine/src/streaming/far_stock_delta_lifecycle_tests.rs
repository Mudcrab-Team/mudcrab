//! Additional lifecycle, work-accounting and request-partition proofs.
use super::*;

#[test]
fn settled_planner_visits_no_semantic_entries() {
    let fixture = Fixture::new();
    let mut app = complete_plan_app(&fixture, fixture.selected().0);
    for _ in 0..3 {
        app.update();
    }
    let before = app.world().resource::<FarStock>().planning_work;
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(app.world().resource::<FarStock>().planning_work, before);
    assert_recomputed(&mut app, &fixture);
}

#[test]
fn clipped_extreme_windows_and_radius_changes_match_full_oracle() {
    let fixture = Fixture::new();
    let mut app = complete_plan_app(&fixture, fixture.selected().0);
    for owned in [false, true] {
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = owned;
        for radius in [0, 2, i32::MAX] {
            app.world_mut().resource_mut::<EngineConfig>().stream_radius = radius;
            for center in [
                IVec2::splat(i32::MAX),
                IVec2::splat(i32::MIN),
                IVec2::new(i32::MIN, i32::MAX),
                IVec2::new(i32::MAX, i32::MIN),
                IVec2::ZERO,
            ] {
                app.world_mut().resource_mut::<RenderOrigin>().0 = center;
                app.update();
                assert_recomputed(&mut app, &fixture);
                assert_no_exit(app.world());
            }
        }
    }
}

#[test]
fn same_cell_request_changes_replace_receiver_without_trimming_original_ids() {
    let fixture = Fixture::new();
    let key = fixture.selected().0;
    let rows: Vec<_> = fixture
        .rows
        .iter()
        .filter(|(cell, row)| *cell == key && row.model_path.is_some())
        .take(2)
        .map(|(_, row)| row)
        .collect();
    assert_eq!(rows.len(), 2);
    let world = cell_world(key).unwrap();
    for remove in [false, true] {
        let mut far = FarStock::default();
        let original = if remove {
            BTreeSet::from([rows[0].form_id, rows[1].form_id])
        } else {
            BTreeSet::from([rows[0].form_id])
        };
        for &id in &original {
            far.set_target((world, id), Some(key));
        }
        far.request_dirty.clear();
        let (sender, receiver) = crossbeam_channel::bounded(1);
        far.pending.insert(
            key,
            Pending {
                ids: original.clone(),
                receiver,
                ready: None,
            },
        );
        if remove {
            far.set_target((world, rows[0].form_id), None);
        } else {
            far.set_target((world, rows[1].form_id), Some(key));
        }
        assert_eq!(
            far.pending[&key].ids, original,
            "planning never edits the expected response set"
        );
        let keys = std::mem::take(&mut far.request_dirty);
        assert!(far.reconcile_requests(keys));
        assert!(far.pending.is_empty());
        assert!(
            sender
                .try_send(ReferenceResponse {
                    generation: far.generation,
                    key,
                    result: Ok(Vec::new()),
                    query_micros: 0,
                    queue_wait_micros: 0,
                    total_request_micros: 0
                })
                .is_err()
        );
        let expected = if remove {
            BTreeSet::from([rows[1].form_id])
        } else {
            BTreeSet::from([rows[0].form_id, rows[1].form_id])
        };
        assert_eq!(far.queued, VecDeque::from([(key, expected)]));
        assert_bookkeeping(&far);
    }
}

#[test]
fn one_cell_removal_preserves_backlog_order_and_counts_the_queue_walk() {
    let fixture = Fixture::new();
    let base = fixture.selected().0;
    let world = cell_world(base).unwrap();
    let first_id = fixture.rows[0].1.form_id;
    let mut far = FarStock::default();
    let queued: VecDeque<_> = (0..512_u32)
        .map(|offset| {
            let key = exterior(world, grid(base) + IVec2::new(offset as i32, 0));
            let id = first_id + offset;
            far.set_target((world, id), Some(key));
            (key, BTreeSet::from([id]))
        })
        .collect();
    far.set_queued(queued.clone());
    far.request_dirty.clear();
    far.set_target((world, first_id + 200), None);
    let before = far.planning_work;
    let keys = std::mem::take(&mut far.request_dirty);
    assert!(
        !far.reconcile_requests(keys),
        "removal does not require a priority sort"
    );
    let expected: VecDeque<_> = queued
        .into_iter()
        .enumerate()
        .filter_map(|(i, row)| (i != 200).then_some(row))
        .collect();
    assert_eq!(far.queued, expected);
    assert_eq!(far.planning_work.queue_entries - before.queue_entries, 512);
    assert_eq!(
        far.planning_work.priority_entries - before.priority_entries,
        0
    );
    assert_eq!(
        far.planning_work.request_entries - before.request_entries,
        0
    );
    assert_bookkeeping(&far);
}

#[test]
fn custom_disabled_parent_withdraws_and_recovers_its_direct_source() {
    #[derive(Component)]
    struct HiddenParent;
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let key = *key;
    let id = (cell_world(key).unwrap(), row.form_id);
    let mut app = complete_plan_app(&fixture, key);
    app.world_mut()
        .register_disabling_component::<HiddenParent>();
    let first_root = root(app.world_mut(), key, false);
    let second_root = root(app.world_mut(), key, false);
    let first = app
        .world_mut()
        .spawn((FormId(id.1), CellRef(row.cell_id), ChildOf(first_root)))
        .id();
    app.world_mut()
        .spawn((FormId(id.1), CellRef(row.cell_id), ChildOf(second_root)));
    for _ in 0..2 {
        app.update();
    }
    assert!(app.world().resource::<FarStock>().failed.contains_key(&key));
    assert_one_terminal_exit(&mut app, "duplicate");
    app.world_mut().entity_mut(second_root).insert(HiddenParent);
    app.update();
    assert_eq!(app.world().resource::<FarStock>().owners[&id].entity, first);
    assert_recomputed(&mut app, &fixture);
    app.world_mut()
        .entity_mut(second_root)
        .remove::<HiddenParent>();
    app.update();
    assert!(!app.world().resource::<FarStock>().owners.contains_key(&id));
    assert!(app.world().resource::<FarStock>().failed.contains_key(&key));
    assert_no_exit(app.world());
}

#[test]
fn raw_in_place_cell_write_admits_the_exact_existing_entity() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let key = *key;
    let id = (cell_world(key).unwrap(), row.form_id);
    let mut app = complete_plan_app(&fixture, key);
    let near = root(app.world_mut(), key, false);
    let entity = app
        .world_mut()
        .spawn((FormId(id.1), CellRef(row.cell_id ^ 1), ChildOf(near)))
        .id();
    app.update();
    assert!(!app.world().resource::<FarStock>().owners.contains_key(&id));
    app.world_mut().get_mut::<CellRef>(entity).unwrap().0 = row.cell_id;
    app.world_mut()
        .resource_mut::<domain::DomainChanges>()
        .reference_changed(entity);
    app.update();
    assert_eq!(
        app.world().resource::<FarStock>().owners[&id].entity,
        entity
    );
    assert_recomputed(&mut app, &fixture);
    assert_no_exit(app.world());
}

#[test]
fn multiple_owned_metadata_writes_use_one_reconciliation_group() {
    let fixture = Fixture::new();
    let key = fixture.selected().0;
    let world = cell_world(key).unwrap();
    let mut app = complete_plan_app(&fixture, key);
    app.world_mut().resource_mut::<EngineConfig>().stream_radius = 4;
    let near = root(app.world_mut(), key, false);
    let entities: Vec<_> = fixture
        .rows
        .iter()
        .filter(|(cell, row)| {
            cell_world(*cell) == Some(world)
                && fixture
                    .package
                    .placed_reference(row.form_id)
                    .is_some_and(|r| r.model_path.is_some())
        })
        .map(|(_, row)| {
            (
                app.world_mut()
                    .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(near)))
                    .id(),
                row.form_id,
            )
        })
        .collect();
    assert!(entities.len() > 1);
    for _ in 0..3 {
        app.update();
    }
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.owners.len(), entities.len());
    let before = far.planning_work;
    let desired = far.desired.clone();
    for &(entity, id) in &entities {
        app.world_mut().get_mut::<FormId>(entity).unwrap().0 = id;
        app.world_mut()
            .resource_mut::<domain::DomainChanges>()
            .reference_changed(entity);
    }
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.planning_work.owner_entries - before.owner_entries,
        entities.len() as u64
    );
    assert_eq!(
        far.planning_work.owner_groups - before.owner_groups,
        1,
        "one update must validate all changed owners together"
    );
    assert_eq!(far.planning_work.full_rebuilds, before.full_rebuilds);
    assert_eq!(far.desired, desired);
    for (entity, id) in entities {
        assert_eq!(far.owners[&(world, id)].entity, entity);
    }
    assert_recomputed(&mut app, &fixture);
    assert_no_exit(app.world());
}
