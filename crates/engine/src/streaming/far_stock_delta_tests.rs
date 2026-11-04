//! Full-recompute oracle and crossing work contracts for the scheduled stock planner.
//!
//! The oracle deliberately does not call needs_stock, full_stock_reach,
//! reference_enabled, complete_source_key, contains_cell or a planner delta helper.

use super::*;

#[derive(Debug, PartialEq, Eq)]
struct Recomputed {
    unbounded: HashMap<Identity, CellKey>,
    bounded: HashMap<Identity, CellKey>,
    desired: HashMap<Identity, CellKey>,
}

fn in_square(cell: IVec2, center: IVec2, radius: i32) -> bool {
    radius >= 0
        && (i64::from(cell.x) - i64::from(center.x)).abs() <= i64::from(radius)
        && (i64::from(cell.y) - i64::from(center.y)).abs() <= i64::from(radius)
}

fn recompute(world: &mut World, fixture: &Fixture) -> Recomputed {
    // Snapshot the actual ExistingReferences domain, including its direct-parent restriction.
    let mut query =
        world.query_filtered::<(&FormId, &CellRef, &ChildOf), Without<StreamedCellKey>>();
    let live: Vec<_> = query
        .iter(world)
        .filter_map(|(form, cell, parent)| {
            let parent = parent.parent();
            world.get::<StreamedCellRoot>(parent)?;
            world
                .get::<StreamedCellKey>(parent)
                .map(|key| (form.0, cell.0, key.0))
        })
        .collect();
    let far = world.resource::<FarStock>();
    let mut result = Recomputed {
        unbounded: HashMap::new(),
        bounded: HashMap::new(),
        desired: HashMap::new(),
    };
    let Some(package) = world.get_resource::<ValidatedPackage>() else {
        return result;
    };
    let Some(demand) = far.demand else {
        return result;
    };
    let config = world.resource::<EngineConfig>();
    let radius = config.stream_radius.max(0);
    let enabled = |id| {
        let resolved = match world
            .get_resource::<ReferenceEnableState>()
            .filter(|state| state.is_bound())
        {
            Some(state) => state.enabled(id),
            None => package.0.enable_catalog().enabled(id),
        };
        resolved == Some(true)
    };
    let fallback = world
        .get_resource::<SourceFallbackDemand>()
        .and_then(|feedback| feedback.for_package(&package.0))
        .map(|(_, ids)| ids);
    let refusals = world
        .get_resource::<crate::cluster::ownership::OwnershipFacts>()
        .filter(|facts| facts.package_matches(&package.0))
        .map(|facts| facts.permanent_refused_references())
        .unwrap_or_default();
    // Fixture::rows enumerates the database's exterior inventory, independent of the window index.
    for (_, row) in &fixture.rows {
        let Some(reference) = package.0.placed_reference(row.form_id) else {
            continue;
        };
        let snapshot = &reference.snapshot;
        if snapshot.placed_worldspace_id != demand.world
            || reference.model_path.is_none()
            || !enabled(row.form_id)
        {
            continue;
        }
        let cell = IVec2::new(snapshot.cell.0, snapshot.cell.1);
        let key = exterior(demand.world, cell);
        let id = (demand.world, row.form_id);
        let incomplete = reference.coverage != CoverageState::Complete;
        let refused = refusals.contains(&row.form_id);
        if config.cluster_own_statics
            && reference.is_static_scenery
            && ((incomplete
                && matches!(
                    reference.refusal_class,
                    StockRefusalClass::Look | StockRefusalClass::Runtime
                ))
                || refused)
        {
            result.unbounded.insert(id, key);
        }
        if in_square(cell, demand.center, radius)
            && (incomplete
                || fallback.is_some_and(|ids| ids.contains(&row.form_id))
                || refused
                || far.owners.contains_key(&id))
        {
            result.bounded.insert(id, key);
        }
    }
    for (id, source_cell, parent_key) in live {
        let Some(reference) = package.0.placed_reference(id) else {
            continue;
        };
        let snapshot = &reference.snapshot;
        // Owning CELL is validated against CellRef; placed grid comes from the certificate.
        // The direct parent's grid may differ, but its placing world must be exact.
        if reference.coverage == CoverageState::Complete
            && reference.model_path.is_some()
            && enabled(id)
            && snapshot.source_cell_id == source_cell
            && snapshot.placed_worldspace_id == demand.world
            && matches!(parent_key, CellKey::Exterior { worldspace_id, .. } if worldspace_id == demand.world)
            && in_square(
                IVec2::new(snapshot.cell.0, snapshot.cell.1),
                demand.center,
                radius,
            )
        {
            result.bounded.insert(
                (demand.world, id),
                exterior(demand.world, IVec2::new(snapshot.cell.0, snapshot.cell.1)),
            );
        }
    }
    result.desired.clone_from(&result.bounded);
    result
        .desired
        .extend(result.unbounded.iter().map(|(&id, &key)| (id, key)));
    result
}

fn assert_recomputed(app: &mut App, fixture: &Fixture) {
    let expected = recompute(app.world_mut(), fixture);
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.unbounded_desired, expected.unbounded,
        "unbounded projection"
    );
    assert_eq!(far.bounded_desired, expected.bounded, "bounded projection");
    assert_eq!(far.desired, expected.desired, "final desired union");
    assert_bookkeeping(far);
}

fn inventory_world(fixture: &Fixture) -> u32 {
    cell_world(fixture.selected().0).unwrap()
}

fn bounded_only_key(fixture: &Fixture) -> CellKey {
    let world = inventory_world(fixture);
    fixture
        .rows
        .iter()
        .find(|(key, row)| {
            cell_world(*key) == Some(world)
                && fixture
                    .package
                    .placed_reference(row.form_id)
                    .is_some_and(|reference| {
                        reference.model_path.is_some()
                            && reference.coverage != CoverageState::Complete
                            && reference.refusal_class == StockRefusalClass::Other
                    })
        })
        .expect("inventory has a bounded-only partial source")
        .0
}
#[test]
fn populated_positive_negative_diagonal_and_multicell_crossings_match_full_recompute() {
    let fixture = Fixture::new();
    let world_id = inventory_world(&fixture);
    for owned in [false, true] {
        let mut app = complete_plan_app(&fixture, exterior(world_id, IVec2::ZERO));
        {
            let mut config = app.world_mut().resource_mut::<EngineConfig>();
            config.stream_radius = 2;
            config.cluster_own_statics = owned;
        }
        for center in [
            IVec2::ZERO,
            IVec2::X,
            IVec2::new(-1, 0),
            IVec2::new(-2, 1),
            IVec2::new(-1, 2),
            IVec2::new(3, -2),
            IVec2::ZERO,
        ] {
            app.world_mut().resource_mut::<RenderOrigin>().0 = center;
            app.update();
            let demand = app.world().resource::<FarStock>().demand.unwrap();
            assert_eq!((demand.world, demand.center), (world_id, center));
            assert_recomputed(&mut app, &fixture);
            assert_no_exit(app.world());
        }
    }
}

#[test]
fn populated_overlap_crossing_does_not_reevaluate_the_unchanged_window() {
    let fixture = Fixture::new();
    let world_id = inventory_world(&fixture);
    let mut app = complete_plan_app(&fixture, exterior(world_id, IVec2::ZERO));
    {
        let mut config = app.world_mut().resource_mut::<EngineConfig>();
        config.cluster_own_statics = true;
        config.stream_radius = 4;
    }
    app.update();
    let far = app.world().resource::<FarStock>();
    assert!(!far.bounded_desired.is_empty());
    assert!(!far.unbounded_desired.is_empty());
    let desired = far.desired.clone();
    let before = (
        far.planning_work.full_rebuilds,
        far.planning_work.incremental_updates,
        far.planning_work.bounded_entries,
        far.planning_work.unbounded_entries,
        far.planning_work.projection_entries,
        far.planning_work.request_entries,
    );
    // Both populated grids remain in the overlap; the geometric strips are empty.
    app.world_mut().resource_mut::<RenderOrigin>().0 += IVec2::ONE;
    app.update();
    assert_recomputed(&mut app, &fixture);
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.desired, desired);
    assert_eq!(
        far.planning_work.bounded_entries - before.2,
        0,
        "populated overlap is retained, not reevaluated on a crossing"
    );
    assert_eq!(far.planning_work.unbounded_entries - before.3, 0);
    assert_eq!(far.planning_work.projection_entries - before.4, 0);
    assert_eq!(far.planning_work.request_entries - before.5, 0);
    assert_eq!(far.planning_work.full_rebuilds, before.0);
    assert!(far.planning_work.incremental_updates > before.1);

    // Now remove a populated cell while retaining the other populated cell.
    // Count certificate entries in the symmetric difference, not the complete new window.
    let old_center = far.demand.unwrap().center;
    let new_center = IVec2::new(-5, 1);
    let boundary_entries = fixture
        .rows
        .iter()
        .filter(|(_, row)| {
            let reference = fixture.package.placed_reference(row.form_id).unwrap();
            let snapshot = &reference.snapshot;
            let cell = IVec2::new(snapshot.cell.0, snapshot.cell.1);
            snapshot.placed_worldspace_id == world_id
                && in_square(cell, old_center, 4) != in_square(cell, new_center, 4)
        })
        .count() as u64;
    assert!(
        boundary_entries > 0,
        "this crossing has a populated leaving strip"
    );
    let before = (
        far.planning_work.bounded_entries,
        far.planning_work.projection_entries,
    );
    app.world_mut().resource_mut::<RenderOrigin>().0 = new_center;
    app.update();
    assert_recomputed(&mut app, &fixture);
    let far = app.world().resource::<FarStock>();
    assert!(
        far.planning_work.bounded_entries - before.0 <= boundary_entries,
        "only changed strips may reevaluate certificate entries"
    );
    assert!(
        far.planning_work.projection_entries - before.1 <= boundary_entries,
        "the union projection must not walk the unchanged overlap"
    );
}

#[test]
fn crossing_does_not_scan_unchanged_irrelevant_near_sources() {
    let fixture = Fixture::new();
    let world_id = inventory_world(&fixture);
    let mut app = complete_plan_app(&fixture, exterior(world_id, IVec2::ZERO));
    app.world_mut().resource_mut::<EngineConfig>().stream_radius = 4;
    let near = root(
        app.world_mut(),
        exterior(world_id, IVec2::new(100, 100)),
        false,
    );
    for id in 0..1024 {
        // Real ExistingReferences rows under a real streamed root, absent from this certificate.
        app.world_mut()
            .spawn((FormId(0xF000_0000 + id), CellRef(0), ChildOf(near)));
    }
    app.update();
    app.update(); // Consume Added/Changed ticks before measuring a demand-only crossing.
    let far = app.world().resource::<FarStock>();
    assert!(far.owners.is_empty());
    let before = (
        far.planning_work.near_entries,
        far.planning_work.owner_entries,
    );
    app.world_mut().resource_mut::<RenderOrigin>().0 += IVec2::X;
    app.update();
    assert_recomputed(&mut app, &fixture);
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.planning_work.near_entries - before.0,
        0,
        "a camera crossing must not revisit all unchanged near reference roots"
    );
    assert_eq!(far.planning_work.owner_entries - before.1, 0);
    assert_no_exit(app.world());
}

#[test]
fn radius_enable_fallback_and_certificate_changes_recover_full_recompute() {
    use crate::world::reference_state::{self, SetReferenceEnabled};

    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let key = *key;
    let id = (cell_world(key).unwrap(), row.form_id);
    let mut app = complete_plan_app(&fixture, key);
    reference_state::install(&mut app);
    app.world_mut().resource_mut::<EngineConfig>().stream_radius = 0;
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert!(!app.world().resource::<FarStock>().desired.contains_key(&id));
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::from([row.form_id]));
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert_eq!(
        app.world().resource::<FarStock>().desired.get(&id),
        Some(&key)
    );
    for enabled in [false, true] {
        app.world_mut().trigger(SetReferenceEnabled {
            reference: row.form_id,
            enabled,
        });
        app.update();
        assert_recomputed(&mut app, &fixture);
        assert_eq!(
            app.world().resource::<FarStock>().desired.contains_key(&id),
            enabled
        );
    }
    app.world_mut().resource_mut::<RenderOrigin>().0 = grid(key) + IVec2::X;
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert!(!app.world().resource::<FarStock>().desired.contains_key(&id));
    app.world_mut().resource_mut::<EngineConfig>().stream_radius = 1;
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert!(app.world().resource::<FarStock>().desired.contains_key(&id));
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::new());
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert!(!app.world().resource::<FarStock>().desired.contains_key(&id));
    app.world_mut().remove_resource::<ValidatedPackage>();
    app.update();
    assert_recomputed(&mut app, &fixture);
    let replacement = Arc::new(
        crate::cluster::overlay_fixture::open_fixture(fixture.directory.path(), false).unwrap(),
    );
    assert!(!Arc::ptr_eq(&replacement, &fixture.package));
    app.world_mut()
        .insert_resource(ValidatedPackage(replacement.clone()));
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::from([row.form_id]));
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert!(
        app.world().resource::<FarStock>().desired.is_empty(),
        "old certificate feedback is rejected"
    );
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&replacement), BTreeSet::from([row.form_id]));
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert_eq!(
        app.world().resource::<FarStock>().desired.get(&id),
        Some(&key)
    );
    assert_no_exit(app.world());
}

#[test]
fn world_switch_interior_and_absent_camera_clear_then_recover_exact_demand() {
    let fixture = Fixture::new();
    let worlds: BTreeSet<_> = fixture
        .rows
        .iter()
        .filter_map(|(key, _)| cell_world(*key))
        .collect();
    assert!(worlds.len() >= 2);
    let mut app = complete_plan_app(&fixture, fixture.selected().0);
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .cluster_own_statics = true;
    for world_id in worlds.iter().copied().chain(worlds.iter().rev().copied()) {
        app.world_mut().resource_mut::<EngineConfig>().worldspace_id = world_id;
        app.world_mut().resource_mut::<RenderOrigin>().0 = IVec2::ZERO;
        app.update();
        assert_eq!(
            app.world().resource::<FarStock>().demand.unwrap().world,
            world_id
        );
        assert_recomputed(&mut app, &fixture);
        assert!(!app.world().resource::<FarStock>().desired.is_empty());
    }
    app.world_mut()
        .insert_resource(super::super::super::ActiveSpace {
            worldspace_id: Some(*worlds.first().unwrap()),
            interior: Some(77),
        });
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert!(app.world().resource::<FarStock>().demand.is_none());
    app.world_mut()
        .remove_resource::<super::super::super::ActiveSpace>();
    let camera = app
        .world_mut()
        .query_filtered::<Entity, With<crate::world::components::StreamingCamera>>()
        .single(app.world())
        .unwrap();
    app.world_mut()
        .entity_mut(camera)
        .remove::<crate::world::components::StreamingCamera>();
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert!(app.world().resource::<FarStock>().desired.is_empty());
    app.world_mut()
        .entity_mut(camera)
        .insert(crate::world::components::StreamingCamera);
    app.update();
    assert_recomputed(&mut app, &fixture);
    assert!(!app.world().resource::<FarStock>().desired.is_empty());
    assert_no_exit(app.world());
}

#[test]
fn membership_crossing_preserves_unrelated_inflight_and_held_ready_requests() {
    let fixture = Fixture::new();
    let world_id = inventory_world(&fixture);
    let tree_key = bounded_only_key(&fixture);
    for held_ready in [false, true] {
        let mut app = complete_plan_app(&fixture, tree_key);
        {
            let mut config = app.world_mut().resource_mut::<EngineConfig>();
            config.stream_radius = 0;
            config.cluster_own_statics = true;
        }
        app.update();
        let far = app.world().resource::<FarStock>();
        let before = far.desired.clone();
        let generation = far.generation;
        let (key, ids) = far
            .queued
            .iter()
            .find(|(key, _)| *key != tree_key)
            .cloned()
            .expect("the other populated cell has unbounded demand");
        let rows: Vec<_> = fixture
            .rows
            .iter()
            .filter(|(cell, row)| *cell == key && ids.contains(&row.form_id))
            .map(|(_, row)| row.clone())
            .collect();
        assert_eq!(rows.len(), ids.len());
        let response = ReferenceResponse {
            generation,
            key,
            result: Ok(rows),
            query_micros: 11,
            queue_wait_micros: 13,
            total_request_micros: 17,
        };
        let (sender, receiver) = crossbeam_channel::bounded(1);
        {
            let mut far = app.world_mut().resource_mut::<FarStock>();
            let remaining = far
                .queued
                .iter()
                .filter(|(cell, _)| *cell != key)
                .cloned()
                .collect();
            far.set_queued(remaining);
            far.pending.insert(
                key,
                Pending {
                    ids: ids.clone(),
                    receiver,
                    ready: held_ready.then_some(response),
                },
            );
        }
        app.world_mut().resource_mut::<RenderOrigin>().0 = grid(tree_key) + IVec2::splat(10);
        app.update();
        assert_recomputed(&mut app, &fixture);
        let far = app.world().resource::<FarStock>();
        assert_ne!(
            far.desired, before,
            "bounded-only source leaving the bounded window changes membership"
        );
        assert_eq!(cell_world(key), Some(world_id));
        let pending = far
            .pending
            .get(&key)
            .expect("an unrelated exact request survives a membership-changing crossing");
        assert_eq!(pending.ids, ids, "original sparse request IDs are retained");
        assert!(!far.queued_keys.contains(&key));
        assert!(far.queued.iter().all(|(cell, _)| *cell != key));
        let check_response = |response: &ReferenceResponse| {
            assert_eq!(response.key, key);
            assert_eq!(
                response
                    .result
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|row| row.form_id)
                    .collect::<BTreeSet<_>>(),
                ids
            );
        };
        if held_ready {
            let ready = pending.ready.as_ref().expect("held payload survives");
            check_response(ready);
            assert_eq!(
                ready.generation, generation,
                "the original payload is retained"
            );
            assert_eq!(
                (
                    ready.query_micros,
                    ready.queue_wait_micros,
                    ready.total_request_micros
                ),
                (11, 13, 17)
            );
        } else {
            sender
                .try_send(ReferenceResponse {
                    generation,
                    key,
                    result: Ok(fixture
                        .rows
                        .iter()
                        .filter(|(cell, row)| *cell == key && ids.contains(&row.form_id))
                        .map(|(_, row)| row.clone())
                        .collect()),
                    query_micros: 11,
                    queue_wait_micros: 13,
                    total_request_micros: 17,
                })
                .expect("original in-flight receiver remains connected");
            let response = pending.receiver.try_recv().unwrap();
            assert_eq!(response.generation, generation);
            check_response(&response);
        }
        assert_no_exit(app.world());
    }
}

#[test]
fn raw_complete_near_facts_use_owning_cell_direct_parent_and_placed_world() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let key = *key;
    let world_id = cell_world(key).unwrap();
    let foreign = fixture
        .rows
        .iter()
        .find(|(other, _)| cell_world(*other) != Some(world_id))
        .unwrap()
        .0;
    let identity = (world_id, row.form_id);
    for mutation in [
        "add",
        "cell",
        "identity",
        "reparent",
        "nested",
        "root-key",
        "duplicate",
        "remove",
    ] {
        let mut app = complete_plan_app(&fixture, key);
        app.world_mut().resource_mut::<EngineConfig>().stream_radius = 0;
        // Parent's grid intentionally differs from the certificate's placed grid.
        let near = root(
            app.world_mut(),
            exterior(world_id, grid(key) + IVec2::new(8, -3)),
            false,
        );
        let other = root(app.world_mut(), foreign, false);
        let nested = app.world_mut().spawn(ChildOf(near)).id();
        let entity = if mutation == "add" {
            None
        } else {
            Some(
                app.world_mut()
                    .spawn((
                        FormId(if mutation == "identity" {
                            row.form_id ^ 0x8000_0000
                        } else {
                            row.form_id
                        }),
                        CellRef(if mutation == "cell" {
                            row.cell_id ^ 1
                        } else {
                            row.cell_id
                        }),
                        ChildOf(match mutation {
                            "reparent" => other,
                            "nested" => nested,
                            _ => near,
                        }),
                    ))
                    .id(),
            )
        };
        if mutation == "root-key" {
            app.world_mut()
                .entity_mut(near)
                .insert(StreamedCellKey(foreign));
        }
        app.update();
        assert_recomputed(&mut app, &fixture);
        if !matches!(mutation, "duplicate" | "remove") {
            assert!(
                !app.world()
                    .resource::<FarStock>()
                    .desired
                    .contains_key(&identity)
            );
        }
        match mutation {
            "add" => {
                app.world_mut()
                    .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(near)));
            }
            "cell" => {
                app.world_mut()
                    .entity_mut(entity.unwrap())
                    .insert(CellRef(row.cell_id));
            }
            "identity" => {
                app.world_mut()
                    .entity_mut(entity.unwrap())
                    .insert(FormId(row.form_id));
            }
            "reparent" | "nested" => {
                app.world_mut()
                    .entity_mut(entity.unwrap())
                    .insert(ChildOf(near));
            }
            "root-key" => {
                app.world_mut()
                    .entity_mut(near)
                    .insert(StreamedCellKey(exterior(world_id, grid(key) + IVec2::X)));
            }
            "duplicate" => {
                app.world_mut()
                    .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(near)));
            }
            "remove" => {
                app.world_mut()
                    .entity_mut(entity.unwrap())
                    .remove::<FormId>();
            }
            _ => unreachable!(),
        }
        app.update();
        let far = app.world().resource::<FarStock>();
        if mutation == "remove" {
            assert!(
                !far.owners.contains_key(&identity),
                "removed identity loses ownership"
            );
            assert!(!app.world().resource::<Messages<AppExit>>().is_empty());
            assert_recomputed(&mut app, &fixture);
        } else if mutation == "duplicate" {
            assert!(
                !far.owners.contains_key(&identity),
                "duplicate roots cannot keep a winner"
            );
            assert!(
                far.failed
                    .get(&key)
                    .is_some_and(|reason| reason.contains("duplicate"))
            );
            assert_bookkeeping(far);
            assert_recomputed(&mut app, &fixture);
        } else {
            assert_eq!(
                far.desired.get(&identity),
                Some(&key),
                "raw {mutation} invalidates admission"
            );
            assert_eq!(far.owners[&identity].parent, near);
            assert_recomputed(&mut app, &fixture);
            assert_no_exit(app.world());
        }
    }
}

#[test]
fn disabling_a_duplicate_source_recovers_without_a_population_scan() {
    #[derive(Component)]
    struct CustomDisabled;
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let key = *key;
    let id = (cell_world(key).unwrap(), row.form_id);
    let mut app = complete_plan_app(&fixture, key);
    app.world_mut()
        .register_disabling_component::<CustomDisabled>();
    let near = root(app.world_mut(), key, false);
    let first = app
        .world_mut()
        .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(near)))
        .id();
    let duplicate = app
        .world_mut()
        .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(near)))
        .id();
    app.update();
    assert!(!app.world().resource::<FarStock>().owners.contains_key(&id));
    assert!(app.world().resource::<FarStock>().failed.contains_key(&key));
    assert_one_terminal_exit(&mut app, "duplicate");
    app.world_mut().entity_mut(duplicate).insert(CustomDisabled);
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(
        far.owners.get(&id).map(|owner| owner.entity),
        Some(first),
        "native query-domain withdrawal must remove the duplicate from the retained index"
    );
    assert!(!far.failed.contains_key(&key));
    assert_recomputed(&mut app, &fixture);
    app.world_mut()
        .entity_mut(duplicate)
        .remove::<CustomDisabled>();
    app.update();
    let far = app.world().resource::<FarStock>();
    assert!(!far.owners.contains_key(&id));
    assert!(
        far.failed
            .get(&key)
            .is_some_and(|reason| reason.contains("duplicate"))
    );
    assert_no_exit(app.world());
}

#[test]
fn root_membership_and_standin_transitions_restore_indexed_sources() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let key = *key;
    let id = (cell_world(key).unwrap(), row.form_id);
    for change in ["root", "child-key"] {
        let mut app = complete_plan_app(&fixture, key);
        let near = root(app.world_mut(), key, false);
        let entity = app
            .world_mut()
            .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(near)))
            .id();
        match change {
            "root" => {
                app.world_mut()
                    .entity_mut(near)
                    .remove::<StreamedCellRoot>();
            }
            "child-key" => {
                app.world_mut()
                    .entity_mut(entity)
                    .insert(StreamedCellKey(key));
            }
            _ => unreachable!(),
        }
        app.update();
        assert!(!app.world().resource::<FarStock>().desired.contains_key(&id));
        match change {
            "root" => {
                app.world_mut().entity_mut(near).insert(StreamedCellRoot);
            }
            "child-key" => {
                app.world_mut()
                    .entity_mut(entity)
                    .remove::<StreamedCellKey>();
            }
            _ => unreachable!(),
        }
        app.update();
        assert_eq!(
            app.world()
                .resource::<FarStock>()
                .owners
                .get(&id)
                .map(|owner| owner.entity),
            Some(entity),
            "{change}"
        );
        assert_recomputed(&mut app, &fixture);
        assert_no_exit(app.world());
    }
}

#[test]
fn raw_valid_reparent_and_rekey_update_owner_before_release() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let key = *key;
    let id = (cell_world(key).unwrap(), row.form_id);
    for rekey in [false, true] {
        let mut app = complete_plan_app(&fixture, key);
        let old = root(app.world_mut(), key, false);
        let next_key = exterior(id.0, grid(key) + IVec2::new(8, -3));
        let next = if rekey {
            old
        } else {
            root(app.world_mut(), next_key, false)
        };
        let entity = app
            .world_mut()
            .spawn((
                FormId(id.1),
                CellRef(row.cell_id),
                Transform::default(),
                ChildOf(old),
            ))
            .id();
        app.update();
        if rekey {
            app.world_mut().get_mut::<StreamedCellKey>(old).unwrap().0 = next_key;
            app.world_mut()
                .resource_mut::<domain::DomainChanges>()
                .root_changed(old);
        } else {
            app.world_mut().entity_mut(entity).insert(ChildOf(next));
        }
        app.update();
        let far = app.world().resource::<FarStock>();
        let owner = &far.owners[&id];
        assert_eq!(
            (owner.parent, owner.key, owner.near_root, owner.near_key),
            (next, next_key, Some(next), Some(next_key)),
            "rekey={rekey}"
        );
        assert_bookkeeping(far);
        release(app.world_mut(), next);
        let far = app.world().resource::<FarStock>();
        assert!(far.owners[&id].far);
        assert_ne!(far.owners[&id].parent, next);
        assert_eq!(
            app.world().get::<ChildOf>(entity).unwrap().parent(),
            far.owners[&id].parent
        );
        assert_no_exit(app.world());
    }
}

#[test]
fn an_unrelated_same_cell_edit_cannot_clear_duplicate_refusal() {
    let fixture = Fixture::new();
    let key = fixture.selected().0;
    // Both references must share a real authored cell, so this exercises cell-scoped retry.
    let row = fixture
        .rows
        .iter()
        .find(|(cell, row)| {
            *cell == key
                && fixture
                    .package
                    .placed_reference(row.form_id)
                    .is_some_and(|r| r.model_path.is_some())
        })
        .unwrap()
        .1
        .clone();
    let id = (cell_world(key).unwrap(), row.form_id);
    let mut app = complete_plan_app(&fixture, key);
    let near = root(app.world_mut(), key, false);
    let first = app
        .world_mut()
        .spawn((FormId(id.1), CellRef(row.cell_id), ChildOf(near)))
        .id();
    let duplicate = app
        .world_mut()
        .spawn((FormId(id.1), CellRef(row.cell_id), ChildOf(near)))
        .id();
    app.update();
    assert!(app.world().resource::<FarStock>().failed.contains_key(&key));
    assert_one_terminal_exit(&mut app, "duplicate");
    // Near enrollment of another qualified identity retries this cell, not the duplicate.
    let other = fixture
        .rows
        .iter()
        .find(|(cell, other)| {
            *cell == key
                && other.form_id != id.1
                && fixture
                    .package
                    .placed_reference(other.form_id)
                    .is_some_and(|r| r.model_path.is_some())
        })
        .unwrap()
        .1
        .clone();
    app.world_mut()
        .spawn((FormId(other.form_id), CellRef(other.cell_id), ChildOf(near)));
    app.update();
    let far = app.world().resource::<FarStock>();
    assert!(!far.owners.contains_key(&id));
    assert!(
        far.failed
            .get(&key)
            .is_some_and(|reason| reason.contains("duplicate")),
        "another identity must not erase the retained duplicate refusal"
    );
    assert!(!far.queued_keys.contains(&key));
    app.world_mut().despawn(duplicate);
    app.update();
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.owners.get(&id).map(|owner| owner.entity), Some(first));
    assert!(!far.failed.contains_key(&key));
    assert_recomputed(&mut app, &fixture);
    assert_no_exit(app.world());
}

#[path = "far_stock_delta_lifecycle_tests.rs"]
mod lifecycle;
