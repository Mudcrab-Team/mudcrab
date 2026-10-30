//! Full-state differential oracle for the retained planner.
//!
//! Wire this file as a child of far_stock_tests.rs. All helpers remain test-local.
//! Every comparison starts a fresh FarStock, scans the actual World (never the
//! retained near index), and rebuilds both projections and the complete work
//! partition. Owners and nonduplicate failures are historical inputs; pending
//! IDs, payloads and channels are immutable request receipts, not derived state.
use super::*;
use crate::world::reference_state::{self, SetReferenceEnabled};
use proptest::prelude::*;
use proptest::test_runner::{RngSeed, TestRunner};

type NearSnapshot = HashMap<Identity, HashMap<Entity, (Entity, CellKey)>>;

// Last checked authoritative inputs, captured AFTER the preceding update. This
// is test history, not the planner's cached demand/projection/revision state.
#[derive(Resource)]
struct OracleInputs {
    package: Arc<CertifiedPackage>,
    demand: Option<LodDemand>,
    radius: i32,
    owned: bool,
    enable_revision: Option<u64>,
    fallback: Option<(u64, BTreeSet<u32>)>,
    refusal_revision: Option<u64>,
    near: NearSnapshot,
}

fn oracle_inputs(world: &mut World) -> OracleInputs {
    let package = world.resource::<ValidatedPackage>().0.clone();
    let mut cameras = world
        .query_filtered::<(Entity, &Transform), With<crate::world::components::StreamingCamera>>();
    let (_, transform) = cameras.single(world).expect("one test streaming camera");
    assert_eq!(
        *transform,
        Transform::default(),
        "zero-local-transform harness"
    );
    let config = world.resource::<EngineConfig>();
    let space = world.resource::<ActiveSpace>();
    // Ordinary streaming keeps ActiveSpace even without ActiveCell/PrestreamCells.
    // Read its authority directly rather than sharing the planner's demand helper.
    let demand = (config.stream_radius >= 0 && space.interior.is_none()).then_some(LodDemand {
        world: space.worldspace_id.unwrap_or(config.worldspace_id),
        center: world.resource::<RenderOrigin>().0,
    });
    OracleInputs {
        package: package.clone(),
        demand,
        radius: config.stream_radius.max(0),
        owned: config.cluster_own_statics,
        enable_revision: world
            .get_resource::<ReferenceEnableState>()
            .map(ReferenceEnableState::revision),
        fallback: world
            .resource::<SourceFallbackDemand>()
            .for_package(&package)
            .map(|(revision, ids)| (revision, ids.clone())),
        refusal_revision: world
            .get_resource::<crate::cluster::ownership::OwnershipFacts>()
            .filter(|facts| facts.package_matches(&package))
            .map(|facts| facts.refusal_revision()),
        near: actual_near_sources(world, &package),
    }
}

fn square_contains(cell: IVec2, center: IVec2, radius: i32) -> bool {
    (i64::from(cell.x) - i64::from(center.x)).abs() <= i64::from(radius)
        && (i64::from(cell.y) - i64::from(center.y)).abs() <= i64::from(radius)
}

fn actual_near_sources(world: &mut World, package: &CertifiedPackage) -> NearSnapshot {
    // Use native queries for BOTH domains: world.get alone bypasses registered
    // disabling components on a source or its direct parent.
    let mut parents =
        world.query_filtered::<(Entity, &StreamedCellKey), (With<StreamedCellRoot>,)>();
    let parents: HashMap<_, _> = parents.iter(world).map(|(e, key)| (e, key.0)).collect();
    let mut sources =
        world.query_filtered::<(Entity, &FormId, &CellRef, &ChildOf), Without<StreamedCellKey>>();
    let mut result = NearSnapshot::new();
    for (entity, form, cell, parent) in sources.iter(world) {
        let parent = parent.parent();
        let Some(&key) = parents.get(&parent) else {
            continue;
        };
        let Some(placing_world) = cell_world(key) else {
            continue;
        };
        let Some(reference) = package.placed_reference(form.0) else {
            continue;
        };
        if reference.snapshot.source_cell_id == cell.0
            && reference.snapshot.placed_worldspace_id == placing_world
        {
            result
                .entry((placing_world, form.0))
                .or_default()
                .insert(entity, (parent, key));
        }
    }
    result
}

fn copy_response(response: &ReferenceResponse) -> ReferenceResponse {
    ReferenceResponse {
        generation: response.generation,
        key: response.key,
        result: response.result.clone(),
        query_micros: response.query_micros,
        queue_wait_micros: response.queue_wait_micros,
        total_request_micros: response.total_request_micros,
    }
}

fn copy_pending(pending: &Pending) -> Pending {
    Pending {
        ids: pending.ids.clone(),
        receiver: pending.receiver.clone(),
        ready: pending.ready.as_ref().map(copy_response),
    }
}

fn missing_for(far: &FarStock, key: CellKey) -> BTreeSet<u32> {
    let placing_world = cell_world(key).unwrap();
    far.desired_cells
        .get(&key)
        .into_iter()
        .flatten()
        .copied()
        .filter(|id| !far.owners.contains_key(&(placing_world, *id)))
        .collect()
}

fn full_replan(world: &mut World, fixture: &Fixture, stepping: bool) -> (FarStock, bool) {
    let inputs = oracle_inputs(world);
    let package = world.resource::<ValidatedPackage>().0.clone();
    let near = &inputs.near;
    let actual = world.resource::<FarStock>();
    let config = world.resource::<EngineConfig>();
    let mut fresh = FarStock {
        package: Some(package.clone()),
        demand: inputs.demand,
        // This snapshot is taken BEFORE app.update in checked_update. A database
        // error is historical input, then independently retained/retried below.
        // Duplicate errors CAN be rebuilt from the authoritative scene contents.
        failed: actual
            .failed
            .iter()
            .filter(|(_, reason)| !reason.starts_with("duplicate existing reference "))
            .map(|(&key, reason)| (key, reason.clone()))
            .collect(),
        ..Default::default()
    };
    for (&id, sources) in near {
        for (&entity, &(parent, key)) in sources {
            fresh
                .near_sources
                .by_identity
                .entry(id)
                .or_default()
                .insert(entity, near::NearSource { parent, key });
        }
    }
    let Some(demand) = inputs.demand else {
        assert!(!stepping, "these sequences keep one exterior demand");
        return (fresh, false);
    };
    let enabled = |id| match world
        .get_resource::<ReferenceEnableState>()
        .filter(|state| state.is_bound())
    {
        Some(state) => state.enabled(id) == Some(true),
        None => package.enable_catalog().enabled(id) == Some(true),
    };
    let fallback = world
        .resource::<SourceFallbackDemand>()
        .for_package(&package)
        .map(|(_, ids)| ids);
    let refused = world
        .get_resource::<crate::cluster::ownership::OwnershipFacts>()
        .filter(|facts| facts.package_matches(&package))
        .map(|facts| facts.permanent_refused_references())
        .unwrap_or_default();
    let radius = config.stream_radius.max(0);
    // Enumerate database inventory, not the planner's window iterator or helpers.
    for (_, row) in &fixture.rows {
        let Some(reference) = package.placed_reference(row.form_id) else {
            continue;
        };
        let snapshot = &reference.snapshot;
        if snapshot.placed_worldspace_id != demand.world
            || reference.model_path.is_none()
            || !enabled(row.form_id)
        {
            continue;
        }
        let id = (demand.world, row.form_id);
        let cell = IVec2::new(snapshot.cell.0, snapshot.cell.1);
        let key = exterior(demand.world, cell);
        let incomplete = reference.coverage != CoverageState::Complete;
        if config.cluster_own_statics
            && reference.is_static_scenery
            && ((incomplete
                && matches!(
                    reference.refusal_class,
                    StockRefusalClass::Look | StockRefusalClass::Runtime
                ))
                || refused.contains(&row.form_id))
        {
            fresh.unbounded_desired.insert(id, key);
        }
        if square_contains(cell, demand.center, radius)
            && (incomplete
                || fallback.is_some_and(|ids| ids.contains(&row.form_id))
                || refused.contains(&row.form_id)
                || actual.owners.contains_key(&id)
                || near.contains_key(&id))
        {
            fresh.bounded_desired.insert(id, key);
        }
    }
    fresh.desired.clone_from(&fresh.bounded_desired);
    fresh
        .desired
        .extend(fresh.unbounded_desired.iter().map(|(&id, &key)| (id, key)));
    for (&id, &key) in &fresh.desired {
        fresh.desired_cells.entry(key).or_default().insert(id.1);
    }
    // Decide IO scope before the planner runs. Global input recovery may reset
    // all receipts only when the final target changes (certificate/world always
    // reset); an ordinary crossing/source/owner edit is cell-local.
    let previous = world.get_resource::<OracleInputs>();
    let reset_io = stepping
        && previous.is_none_or(|previous| {
            let package_changed = !Arc::ptr_eq(&previous.package, &inputs.package);
            let world_changed = previous.demand.map(|d| d.world) != inputs.demand.map(|d| d.world);
            let global_changed = previous.radius != inputs.radius
                || previous.owned != inputs.owned
                || previous.enable_revision != inputs.enable_revision
                || previous.fallback != inputs.fallback
                || previous.refusal_revision != inputs.refusal_revision;
            package_changed || world_changed || (global_changed && fresh.desired != actual.desired)
        });
    if reset_io {
        fresh.failed.clear();
    } else if stepping {
        // Pre-step explicit retry/registry commands are inputs. Independently
        // derive source and target changes instead of trusting post-step dirty
        // sets or copying the resulting failure map.
        let mut retry = actual.request_dirty.clone();
        for (&id, &key) in actual.desired.iter().chain(&fresh.desired) {
            if actual.desired.get(&id) != fresh.desired.get(&id) {
                retry.insert(key);
            }
        }
        if let Some(previous) = previous {
            for &id in previous.near.keys().chain(near.keys()) {
                if previous.near.get(&id) != near.get(&id)
                    && let Some(&key) = fresh.desired.get(&id)
                {
                    retry.insert(key);
                }
            }
        }
        fresh.failed.retain(|key, _| !retry.contains(key));
    }
    // Preserve valid ownership receipts, but validate physical identity/ancestry
    // from the SAME World. Missing owners are discovered from fresh raw sources.
    for (&id, owner) in &actual.owners {
        if !fresh.desired.contains_key(&id) {
            continue;
        }
        let reference = package.placed_reference(id.1).unwrap();
        let parent = world.get::<ChildOf>(owner.entity).map(ChildOf::parent);
        let key = parent
            .and_then(|parent| world.get::<StreamedCellKey>(parent))
            .map(|key| key.0);
        let raw_valid = world
            .get::<FormId>(owner.entity)
            .is_some_and(|form| form.0 == id.1)
            && world
                .get::<CellRef>(owner.entity)
                .is_some_and(|cell| cell.0 == reference.snapshot.source_cell_id)
            && world.get::<StreamedCellKey>(owner.entity).is_none()
            && key.is_some_and(|key| cell_world(key) == Some(id.0));
        let near_valid = near
            .get(&id)
            .is_some_and(|sources| sources.contains_key(&owner.entity));
        if raw_valid
            && ((owner.far && parent == Some(owner.parent) && key == Some(owner.key)) || near_valid)
        {
            let mut owner = owner.clone();
            if near_valid {
                let (parent, key) = near[&id][&owner.entity];
                owner.parent = parent;
                owner.key = key;
                owner.near_root = Some(parent);
                owner.near_key = Some(key);
                owner.far = false;
            }
            fresh.owners.insert(id, owner);
        }
    }
    let desired: Vec<_> = fresh.desired.iter().map(|(&id, &key)| (id, key)).collect();
    for (id, key) in desired {
        let sources = near.get(&id);
        let unique = sources
            .filter(|sources| sources.len() == 1)
            .and_then(|sources| sources.iter().next());
        if sources.is_some_and(|sources| sources.len() > 1)
            || unique.is_some_and(|(&entity, _)| {
                fresh
                    .owners
                    .get(&id)
                    .is_some_and(|owner| owner.entity != entity)
            })
        {
            fresh.owners.remove(&id);
            fresh.duplicate_conflicts.insert(id, key);
            fresh.failed.insert(
                key,
                format!(
                    "duplicate existing reference {:08X} in placing world {:08X}",
                    id.1, id.0
                ),
            );
        } else if !fresh.owners.contains_key(&id)
            && let Some((&entity, &(parent, near_key))) = unique
        {
            fresh.owners.insert(
                id,
                Owner {
                    entity,
                    parent,
                    key: near_key,
                    near_key: Some(near_key),
                    near_root: Some(parent),
                    far: false,
                    row: None,
                    physics_requested: config.interactive_world_physics(),
                },
            );
        }
    }
    // Rebuild every reverse index directly; calling put_owner here would share
    // the very indexing implementation this oracle is intended to check.
    for (&id, owner) in &fresh.owners {
        fresh.by_entity.entry(owner.entity).or_default().insert(id);
        for root in [Some(owner.parent), owner.near_root].into_iter().flatten() {
            fresh.by_root.entry(root).or_default().insert(id);
        }
        if let Some(key) = owner.near_key {
            fresh.by_near_key.entry(key).or_default().insert(id);
        }
    }
    // Reconstruct requests without set_target/project_union/reconcile_requests.
    // An unchanged receipt survives; changed required sets cancel the WHOLE receipt.
    for &key in fresh.desired_cells.keys() {
        if fresh.failed.contains_key(&key) {
            continue;
        }
        let needed = missing_for(&fresh, key);
        if needed.is_empty() {
            continue;
        }
        if let Some(pending) = actual
            .pending
            .get(&key)
            .filter(|p| !reset_io && p.ids == needed)
        {
            fresh.pending.insert(key, copy_pending(pending));
        } else {
            fresh.queued.push_back((key, needed));
            fresh.queued_keys.insert(key);
        }
    }
    (fresh, reset_io)
}

fn owner_value(owner: &Owner) -> String {
    // ReferenceRow has no PartialEq. Debug includes every row field (including
    // placement), rather than reducing ownership equality to the entity alone.
    format!(
        "{:?}",
        (
            owner.entity,
            owner.parent,
            owner.key,
            owner.near_key,
            owner.near_root,
            owner.far,
            &owner.row,
            owner.physics_requested
        )
    )
}

fn assert_pending_eq(actual: &Pending, expected: &Pending, context: &str) {
    assert_eq!(actual.ids, expected.ids, "{context}: original expected IDs");
    // Receiver clones share a channel; pointer/Debug identity is meaningless.
    // same_channel proves the immutable request was retained without consuming
    // either receiver (try_recv here would steal data from the real collector).
    assert!(
        actual.receiver.same_channel(&expected.receiver),
        "{context}: request channel changed"
    );
    assert_eq!(
        format!("{:?}", actual.ready),
        format!("{:?}", expected.ready),
        "{context}: complete held response receipt"
    );
}

fn failure_values(far: &FarStock) -> HashMap<CellKey, String> {
    far.failed
        .iter()
        .map(|(&key, reason)| {
            // If several identities conflict in one cell, HashMap discovery order
            // chooses the final diagnostic. The refusal and exact conflict identities
            // are semantic; which duplicate was reported last is not.
            let reason = if reason.starts_with("duplicate existing reference ") {
                assert!(far.duplicate_conflicts.values().any(|cell| *cell == key));
                "duplicate existing reference".to_owned()
            } else {
                reason.clone()
            };
            (key, reason)
        })
        .collect()
}

fn assert_full_state(app: &mut App, fixture: &Fixture, context: &str) {
    let (expected, _) = full_replan(app.world_mut(), fixture, false);
    assert_expected_state(app, &expected, context);
}

fn assert_expected_state(app: &App, expected: &FarStock, context: &str) {
    let actual = app.world().resource::<FarStock>();
    assert_eq!(
        actual.demand, expected.demand,
        "{context}: authoritative demand"
    );
    assert_eq!(
        actual.unbounded_desired, expected.unbounded_desired,
        "{context}: unbounded"
    );
    assert_eq!(
        actual.bounded_desired, expected.bounded_desired,
        "{context}: bounded"
    );
    assert_eq!(actual.desired, expected.desired, "{context}: desired");
    assert_eq!(
        actual.desired_cells, expected.desired_cells,
        "{context}: desired_cells"
    );
    let actual_owners: HashMap<_, _> = actual
        .owners
        .iter()
        .map(|(&id, owner)| (id, owner_value(owner)))
        .collect();
    let expected_owners: HashMap<_, _> = expected
        .owners
        .iter()
        .map(|(&id, owner)| (id, owner_value(owner)))
        .collect();
    assert_eq!(
        actual_owners, expected_owners,
        "{context}: physical owners / raw re-discovery"
    );
    assert!(
        actual
            .owners
            .keys()
            .all(|id| actual.desired.contains_key(id)),
        "{context}: owners subset of desired"
    );
    assert_eq!(actual.by_entity, expected.by_entity, "{context}: by_entity");
    assert_eq!(actual.by_root, expected.by_root, "{context}: by_root");
    assert_eq!(
        actual.by_near_key, expected.by_near_key,
        "{context}: by_near_key"
    );
    let index = |far: &FarStock| -> NearSnapshot {
        far.near_sources
            .by_identity
            .iter()
            .map(|(&id, sources)| {
                (
                    id,
                    sources
                        .iter()
                        .map(|(&e, source)| (e, (source.parent, source.key)))
                        .collect(),
                )
            })
            .collect()
    };
    assert_eq!(
        index(actual),
        index(expected),
        "{context}: raw near metadata"
    );
    let queued = |far: &FarStock| -> HashMap<CellKey, BTreeSet<u32>> {
        far.queued.iter().cloned().collect()
    };
    // Queue order is history/priority state, not set membership. Do not require a
    // fresh HashMap traversal to reproduce an unchanged backlog's stable order.
    assert_eq!(
        queued(actual),
        queued(expected),
        "{context}: queued desired minus owners/failed/pending"
    );
    assert_eq!(
        actual.queued.len(),
        expected.queued.len(),
        "{context}: duplicate queued cell"
    );
    assert_eq!(
        actual.queued_keys, expected.queued_keys,
        "{context}: queued_keys"
    );
    assert_eq!(
        actual.pending.keys().copied().collect::<HashSet<_>>(),
        expected.pending.keys().copied().collect::<HashSet<_>>(),
        "{context}: pending cells"
    );
    for (&key, pending) in &expected.pending {
        assert_pending_eq(&actual.pending[&key], pending, context);
    }
    assert_eq!(
        failure_values(actual),
        failure_values(expected),
        "{context}: failed cells / diagnostics"
    );
    assert_eq!(
        actual.duplicate_conflicts, expected.duplicate_conflicts,
        "{context}: duplicate identities"
    );
    for (&key, ids) in &actual.desired_cells {
        let placing_world = cell_world(key).unwrap();
        let owned: BTreeSet<_> = ids
            .iter()
            .copied()
            .filter(|id| actual.owners.contains_key(&(placing_world, *id)))
            .collect();
        if actual.failed.contains_key(&key) {
            assert!(
                !actual.pending.contains_key(&key) && !actual.queued_keys.contains(&key),
                "{context}: failed cell has IO"
            );
            continue;
        }
        let pending = actual
            .pending
            .get(&key)
            .map(|p| p.ids.clone())
            .unwrap_or_default();
        let queued = actual
            .queued
            .iter()
            .find(|(cell, _)| *cell == key)
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default();
        assert!(
            owned.is_disjoint(&pending)
                && owned.is_disjoint(&queued)
                && pending.is_disjoint(&queued),
            "{context}: owner/pending/queued overlap"
        );
        assert_eq!(
            owned
                .union(&pending)
                .copied()
                .chain(queued)
                .collect::<BTreeSet<_>>(),
            *ids,
            "{context}: desired partition has missing/extra work"
        );
    }
    assert_bookkeeping(actual);
    assert!(
        app.world().resource::<Messages<AppExit>>().is_empty(),
        "{context}: unexpected fatal exit"
    );
}

fn checked_update(app: &mut App, fixture: &Fixture, context: &str) {
    let generation = app.world().resource::<FarStock>().generation;
    let receipts: HashMap<_, _> = app
        .world()
        .resource::<FarStock>()
        .pending
        .iter()
        .map(|(&key, pending)| (key, copy_pending(pending)))
        .collect();
    let (expected, reset_io) = full_replan(app.world_mut(), fixture, true);
    let earlier = app
        .world()
        .get_resource::<crate::streaming::failure::LoadingRefusal>()
        .map(|refusal| refusal.0.clone());
    app.update();
    let exits = app
        .world_mut()
        .resource_mut::<Messages<AppExit>>()
        .drain()
        .collect::<Vec<_>>();
    if let Some(earlier) = earlier {
        assert_eq!(
            app.world()
                .resource::<crate::streaming::failure::LoadingRefusal>()
                .0,
            earlier,
            "{context}: recovery/bookkeeping never replaces the process's first terminal reason"
        );
        assert!(
            exits.is_empty(),
            "{context}: later writers must not emit another exit"
        );
    } else if !expected.failed.is_empty() {
        let refusal = &app
            .world()
            .resource::<crate::streaming::failure::LoadingRefusal>()
            .0;
        assert!(
            expected
                .failed
                .values()
                .any(|reason| refusal.contains(reason)),
            "{context}: the independent model's failure must reach the terminal owner: {refusal}"
        );
        assert_eq!(
            exits,
            [AppExit::error()],
            "{context}: exactly one first error exit"
        );
    } else {
        assert!(
            !app.world()
                .contains_resource::<crate::streaming::failure::LoadingRefusal>()
        );
        assert!(
            exits.is_empty(),
            "{context}: no modeled failure permits an exit"
        );
    }
    let actual = app.world().resource::<FarStock>();
    assert_eq!(
        actual.generation,
        if reset_io {
            generation.wrapping_add(1)
        } else {
            generation
        },
        "{context}: independently allowed IO generation reset={reset_io}"
    );
    if !reset_io {
        for (key, receipt) in &receipts {
            if !expected.failed.contains_key(key) && missing_for(&expected, *key) == receipt.ids {
                assert_pending_eq(
                    actual
                        .pending
                        .get(key)
                        .expect("unchanged request must survive"),
                    receipt,
                    context,
                );
            }
        }
    }
    assert_expected_state(app, &expected, context);
    // Historical transitions were verified against pre-step inputs above. Also
    // rebuild from the post-step World so deferred edits cannot hide stale state.
    assert_full_state(app, fixture, context);
    let inputs = oracle_inputs(app.world_mut());
    app.world_mut().insert_resource(inputs);
}

fn full_state_app(fixture: &Fixture, key: CellKey) -> App {
    let mut app = complete_plan_app(fixture, key);
    app.world_mut().resource_mut::<EngineConfig>().stream_radius = 1;
    reference_state::install(&mut app);
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::new());
    checked_update(&mut app, fixture, "initial full plan");
    app
}

fn near_entity(app: &mut App, key: CellKey, row: &ReferenceRow) -> (Entity, Entity) {
    let parent = root(app.world_mut(), key, false);
    let entity = app
        .world_mut()
        .spawn((FormId(row.form_id), CellRef(row.cell_id), ChildOf(parent)))
        .id();
    (parent, entity)
}

fn fixture_cell(fixture: &Fixture, reference: u32) -> CellKey {
    let snapshot = &fixture
        .package
        .placed_reference(reference)
        .unwrap()
        .snapshot;
    exterior(
        snapshot.placed_worldspace_id,
        IVec2::new(snapshot.cell.0, snapshot.cell.1),
    )
}

fn sequence_cell(fixture: &Fixture) -> CellKey {
    if fixture.package.placed_reference(0x111).is_some() {
        // Documented runtime fixture: 0x111 belongs to the demanded parent
        // world; selected() instead picks a partial row in the child world.
        fixture_cell(fixture, 0x111)
    } else {
        // Preserve the original inventory family's negative partial selection
        // and action indices, while allowing the loader-only Complete family.
        fixture
            .rows
            .iter()
            .find(|(key, row)| {
                matches!(key, CellKey::Exterior { grid_x, .. } if *grid_x < 0)
                    && row.model_path.is_some()
                    && fixture
                        .package
                        .placed_reference(row.form_id)
                        .unwrap()
                        .coverage
                        == CoverageState::Partial
            })
            .unwrap_or_else(|| fixture.rows.first().unwrap())
            .0
    }
}

#[test]
fn full_state_active_space_controls_exterior_demand_independently_of_configuration() {
    let fixture = Fixture::new();
    let key = fixture.selected().0;
    let placing_world = cell_world(key).unwrap();
    let configured_world = fixture
        .rows
        .iter()
        .find_map(|(key, _)| cell_world(*key).filter(|world| *world != placing_world))
        .expect("producer inventory has a distinct placing world");
    let mut app = full_state_app(&fixture, key);
    app.world_mut().resource_mut::<ActiveSpace>().worldspace_id = Some(placing_world);
    app.world_mut().resource_mut::<EngineConfig>().worldspace_id = configured_world;
    checked_update(
        &mut app,
        &fixture,
        "active exterior overrides configuration",
    );
    assert_eq!(
        app.world().resource::<FarStock>().demand.unwrap().world,
        placing_world,
    );

    app.world_mut().resource_mut::<ActiveSpace>().worldspace_id = None;
    checked_update(
        &mut app,
        &fixture,
        "unset active exterior uses configuration",
    );
    assert_eq!(
        app.world().resource::<FarStock>().demand.unwrap().world,
        configured_world,
    );

    // The interleaving oracle below deliberately steps only exterior sequences;
    // independently check its input boundary for ordinary no-exterior states.
    app.world_mut().resource_mut::<ActiveSpace>().interior = Some(fixture.rows[0].1.cell_id);
    assert!(oracle_inputs(app.world_mut()).demand.is_none());
    app.world_mut().resource_mut::<ActiveSpace>().interior = None;
    app.world_mut().resource_mut::<EngineConfig>().stream_radius = -1;
    assert!(oracle_inputs(app.world_mut()).demand.is_none());
}

#[test]
fn full_state_removed_owner_is_rediscovered_without_source_change_or_duplicate_io() {
    let fixture = Fixture::from_directory(published_for_loader());
    let (key, row) = fixture.rows.first().unwrap();
    let id = (cell_world(*key).unwrap(), row.form_id);
    let mut app = full_state_app(&fixture, *key);
    let (_, entity) = near_entity(&mut app, *key, row);
    checked_update(&mut app, &fixture, "near source admission");
    checked_update(&mut app, &fixture, "settle owner dirtiness");
    let before = app.world().resource::<FarStock>();
    assert_eq!(before.owners[&id].entity, entity);
    assert!(!before.pending.contains_key(key) && !before.queued_keys.contains(key));
    let near_work = before.planning_work.near_entries;
    let desired = before.desired.clone();
    let generation = before.generation;
    assert!(
        app.world_mut()
            .resource_mut::<FarStock>()
            .remove_owner(&id)
            .is_some()
    );
    // Deliberately NO FormId/CellRef/ChildOf/root write, no index reset and no
    // set_target. Only the ownership registry changed under an unchanged target.
    checked_update(&mut app, &fixture, "registry-only owner removal");
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.owners[&id].entity, entity);
    assert_eq!(far.desired, desired);
    assert_eq!(far.generation, generation);
    assert_eq!(
        far.planning_work.near_entries - near_work,
        1,
        "only the affected indexed source is rediscovered"
    );
    assert!(!far.pending.contains_key(key) && !far.queued_keys.contains(key));
}

fn install_receipt(
    app: &mut App,
    fixture: &Fixture,
    key: CellKey,
    ready: bool,
) -> crossbeam_channel::Sender<ReferenceResponse> {
    let ids = app
        .world()
        .resource::<FarStock>()
        .queued
        .iter()
        .find(|(cell, _)| *cell == key)
        .unwrap()
        .1
        .clone();
    let generation = app.world().resource::<FarStock>().generation;
    let response = ReferenceResponse {
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
    };
    let (sender, receiver) = crossbeam_channel::bounded(1);
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
            ids,
            receiver,
            ready: ready.then_some(response),
        },
    );
    sender
}

#[test]
fn full_state_crossings_preserve_inflight_and_held_receipts_and_sticky_failures() {
    let fixture = Fixture::from_directory(
        crate::cluster::reader::producer_fixture_tests::published_for_runtime_draw_states(),
    );
    for held in [false, true] {
        // Parent reference 0x100 is at (0,0); enabled runtime/Look stock
        // including 0x111 is in this same world's distinct cell (-2,1).
        let key = fixture_cell(&fixture, 0x100);
        let request_key = fixture_cell(&fixture, 0x111);
        assert_eq!(cell_world(key), cell_world(request_key));
        assert_ne!(key, request_key);
        let mut app = full_state_app(&fixture, key);
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = true;
        checked_update(&mut app, &fixture, "unbounded policy");
        assert_eq!(
            app.world()
                .resource::<FarStock>()
                .unbounded_desired
                .get(&(cell_world(request_key).unwrap(), 0x111)),
            Some(&request_key)
        );
        let sender = install_receipt(&mut app, &fixture, request_key, held);
        assert_full_state(&mut app, &fixture, "installed immutable request");
        let row = fixture
            .rows
            .iter()
            .find(|(_, row)| row.form_id == 0x100)
            .unwrap()
            .1
            .clone();
        let (_, entity) = near_entity(&mut app, key, &row);
        checked_update(&mut app, &fixture, "unrelated near edit preserves request");
        checked_update(&mut app, &fixture, "settle unrelated owner");
        let id = (cell_world(key).unwrap(), row.form_id);
        assert_eq!(
            app.world().resource::<FarStock>().owners[&id].entity,
            entity
        );
        app.world_mut().resource_mut::<FarStock>().remove_owner(&id);
        checked_update(
            &mut app,
            &fixture,
            "unrelated owner churn preserves request",
        );
        for offset in [IVec2::X, -IVec2::ONE, IVec2::new(20, -13), IVec2::ZERO] {
            app.world_mut().resource_mut::<RenderOrigin>().0 = grid(key) + offset;
            checked_update(&mut app, &fixture, "crossing with receipt");
            assert!(
                app.world()
                    .resource::<FarStock>()
                    .pending
                    .contains_key(&request_key)
            );
        }
        // Prove this is the original live receiver, not an equal-looking new one.
        sender
            .try_send(ReferenceResponse {
                generation: app.world().resource::<FarStock>().generation,
                key: request_key,
                result: Err("test-owned channel marker".to_owned()),
                query_micros: 19,
                queue_wait_micros: 23,
                total_request_micros: 29,
            })
            .unwrap();
        assert_full_state(
            &mut app,
            &fixture,
            "channel payload is never consumed by oracle",
        );
        let response = app.world().resource::<FarStock>().pending[&request_key]
            .receiver
            .try_recv()
            .unwrap();
        assert_eq!(response.result.unwrap_err(), "test-owned channel marker");
        {
            let mut far = app.world_mut().resource_mut::<FarStock>();
            far.pending.remove(&request_key);
            far.failed
                .insert(request_key, "test database refusal receipt".to_owned());
        }
        checked_update(
            &mut app,
            &fixture,
            "failure is sticky without affected input",
        );
        assert_eq!(
            app.world().resource::<FarStock>().failed[&request_key],
            "test database refusal receipt"
        );
        app.world_mut().resource_mut::<RenderOrigin>().0 += IVec2::new(7, -5);
        checked_update(
            &mut app,
            &fixture,
            "unrelated crossing retains database failure",
        );
        assert_eq!(
            app.world().resource::<FarStock>().failed[&request_key],
            "test database refusal receipt"
        );
        // A real source change in the failed cell retries that cell, even when
        // the unbounded target and generation do not change.
        let row = fixture
            .rows
            .iter()
            .find(|(cell, row)| {
                *cell == request_key
                    && app
                        .world()
                        .resource::<FarStock>()
                        .desired
                        .contains_key(&(cell_world(*cell).unwrap(), row.form_id))
            })
            .unwrap()
            .1
            .clone();
        near_entity(&mut app, request_key, &row);
        checked_update(
            &mut app,
            &fixture,
            "affected source mutation retries database failure",
        );
        assert!(
            !app.world()
                .resource::<FarStock>()
                .failed
                .contains_key(&request_key)
        );
        // Seed another historical database receipt after settling owner dirtiness
        // so the explicit retry below has an independent failure to clear.
        checked_update(&mut app, &fixture, "settle affected owner admission");
        {
            let mut far = app.world_mut().resource_mut::<FarStock>();
            let queued = far
                .queued
                .iter()
                .filter(|(cell, _)| *cell != request_key)
                .cloned()
                .collect();
            far.set_queued(queued);
            far.failed
                .insert(request_key, "explicit retry refusal receipt".to_owned());
        }
        app.world_mut()
            .resource_mut::<FarStock>()
            .request_dirty
            .insert(request_key);
        checked_update(&mut app, &fixture, "explicit cell retry");
        assert!(
            !app.world()
                .resource::<FarStock>()
                .failed
                .contains_key(&request_key)
        );
        assert_full_state(
            &mut app,
            &fixture,
            "explicit retry restores exact missing partition",
        );
    }
}

#[test]
fn full_state_global_target_change_clears_failures_and_cancels_even_compatible_receipts() {
    let fixture = Fixture::from_directory(
        crate::cluster::reader::producer_fixture_tests::published_for_runtime_draw_states(),
    );
    let key = fixture_cell(&fixture, 0x100);
    for receipt in [None, Some(false), Some(true)] {
        let mut app = full_state_app(&fixture, key);
        let desired = app.world().resource::<FarStock>().desired.clone();
        let sender = receipt.map(|ready| install_receipt(&mut app, &fixture, key, ready));
        if receipt.is_none() {
            let mut far = app.world_mut().resource_mut::<FarStock>();
            let queued = far
                .queued
                .iter()
                .filter(|(cell, _)| *cell != key)
                .cloned()
                .collect();
            far.set_queued(queued);
            far.failed
                .insert(key, "global reset refusal receipt".to_owned());
        }
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = true;
        checked_update(
            &mut app,
            &fixture,
            "global policy adds distant targets and resets IO",
        );
        let far = app.world().resource::<FarStock>();
        assert_ne!(
            far.desired, desired,
            "policy really changes the final target"
        );
        assert!(far.failed.is_empty());
        assert!(!far.pending.contains_key(&key));
        assert!(far.queued_keys.contains(&key));
        drop(sender);
    }
}

#[test]
fn full_state_extreme_crossings_and_radius_recovery_preserve_the_exact_partition() {
    let fixture = Fixture::new();
    let mut app = full_state_app(&fixture, fixture.selected().0);
    for owned in [false, true] {
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = owned;
        checked_update(&mut app, &fixture, "change distance policy");
        for radius in [0, 2, i32::MAX] {
            app.world_mut().resource_mut::<EngineConfig>().stream_radius = radius;
            checked_update(&mut app, &fixture, "change radius");
            for center in [
                IVec2::splat(i32::MIN),
                IVec2::splat(i32::MAX),
                IVec2::new(i32::MIN, i32::MAX),
                IVec2::ZERO,
            ] {
                app.world_mut().resource_mut::<RenderOrigin>().0 = center;
                checked_update(&mut app, &fixture, "extreme clipped crossing");
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Action {
    Cross(i32, i32),
    Near(usize, bool),
    Duplicate(usize, bool),
    RemoveOwner(usize),
    Fallback(usize, bool),
    Enable(usize, bool),
    Radius(i32),
    Owned(bool),
    Request(bool),
    Fail,
    Retry,
    Settle,
}

fn actions() -> impl Strategy<Value = Vec<Action>> {
    proptest::collection::vec(
        prop_oneof![
            5 => (-6_i32..=6, -6_i32..=6).prop_map(|(x, y)| Action::Cross(x, y)),
            4 => (0_usize..32, any::<bool>()).prop_map(|(i, on)| Action::Near(i, on)),
            3 => (0_usize..32, any::<bool>()).prop_map(|(i, on)| Action::Duplicate(i, on)),
            4 => (0_usize..32).prop_map(Action::RemoveOwner),
            3 => (0_usize..32, any::<bool>()).prop_map(|(i, on)| Action::Fallback(i, on)),
            2 => (0_usize..32, any::<bool>()).prop_map(|(i, on)| Action::Enable(i, on)),
            2 => (0_i32..=4).prop_map(Action::Radius),
            1 => any::<bool>().prop_map(Action::Owned),
            3 => any::<bool>().prop_map(Action::Request),
            1 => Just(Action::Fail),
            1 => Just(Action::Retry),
            1 => Just(Action::Settle),
        ],
        1..33,
    )
}

fn remove_near(app: &mut App, id: Identity, source: (Entity, Entity)) {
    // Model the registry withdrawal that accompanies near teardown. Raw deletion
    // of a still-owned entity is a deliberately fatal ownership violation.
    if app
        .world()
        .resource::<FarStock>()
        .owners
        .get(&id)
        .is_some_and(|owner| owner.entity == source.1)
    {
        app.world_mut().resource_mut::<FarStock>().remove_owner(&id);
    }
    app.world_mut().despawn(source.0);
}

fn run_sequence(fixture: &Fixture, sequence: &[Action]) {
    let key = sequence_cell(fixture);
    let placing_world = cell_world(key).unwrap();
    let rows: Vec<_> = fixture
        .rows
        .iter()
        .filter(|(cell, row)| {
            cell_world(*cell) == Some(placing_world)
                && fixture
                    .package
                    .placed_reference(row.form_id)
                    .is_some_and(|reference| reference.model_path.is_some())
        })
        .cloned()
        .collect();
    assert!(!rows.is_empty());
    let mut app = full_state_app(fixture, key);
    let mut near = HashMap::<usize, (Entity, Entity)>::new();
    let mut duplicates = HashMap::<usize, (Entity, Entity)>::new();
    let mut fallback = BTreeSet::new();
    let mut senders = Vec::new();
    for (step, action) in sequence.iter().enumerate() {
        let row_at = |index: usize| &rows[index % rows.len()];
        match *action {
            Action::Cross(x, y) => {
                app.world_mut().resource_mut::<RenderOrigin>().0 = grid(key) + IVec2::new(x, y)
            }
            Action::Near(index, on) | Action::Duplicate(index, on) => {
                let index = index % rows.len();
                let (cell, row) = row_at(index);
                let selected = if matches!(action, Action::Near(..)) {
                    &mut near
                } else {
                    &mut duplicates
                };
                if on {
                    selected
                        .entry(index)
                        .or_insert_with(|| near_entity(&mut app, *cell, row));
                } else if let Some(source) = selected.remove(&index) {
                    remove_near(&mut app, (placing_world, row.form_id), source);
                }
            }
            Action::RemoveOwner(index) => {
                let (_, row) = row_at(index);
                app.world_mut()
                    .resource_mut::<FarStock>()
                    .remove_owner(&(placing_world, row.form_id));
            }
            Action::Fallback(index, on) => {
                let id = row_at(index).1.form_id;
                if on {
                    fallback.insert(id);
                } else {
                    fallback.remove(&id);
                }
                app.world_mut()
                    .resource_mut::<SourceFallbackDemand>()
                    .update(Some(&fixture.package), fallback.clone());
            }
            Action::Enable(index, enabled) => {
                app.world_mut().trigger(SetReferenceEnabled {
                    reference: row_at(index).1.form_id,
                    enabled,
                });
            }
            Action::Radius(radius) => {
                app.world_mut().resource_mut::<EngineConfig>().stream_radius = radius
            }
            Action::Owned(owned) => {
                app.world_mut()
                    .resource_mut::<EngineConfig>()
                    .cluster_own_statics = owned
            }
            Action::Request(ready) => {
                let cell = app
                    .world()
                    .resource::<FarStock>()
                    .queued
                    .front()
                    .map(|(key, _)| *key);
                if let Some(cell) = cell {
                    senders.push(install_receipt(&mut app, fixture, cell, ready));
                }
            }
            Action::Fail => {
                let far = app.world().resource::<FarStock>();
                // Only a real outstanding request can deliver a database error.
                // Never manufacture a DB failure over an existing duplicate refusal.
                let cell = far.queued.front().map(|(key, _)| *key).or_else(|| {
                    far.pending.keys().copied().min_by_key(|key| {
                        let cell = grid(*key);
                        (cell.x, cell.y)
                    })
                });
                if let Some(cell) = cell {
                    let mut far = app.world_mut().resource_mut::<FarStock>();
                    far.pending.remove(&cell);
                    let queued = far
                        .queued
                        .iter()
                        .filter(|(key, _)| *key != cell)
                        .cloned()
                        .collect();
                    far.set_queued(queued);
                    far.failed
                        .insert(cell, "test database refusal receipt".to_owned());
                }
            }
            Action::Retry => {
                let cells: Vec<_> = app
                    .world()
                    .resource::<FarStock>()
                    .failed
                    .keys()
                    .copied()
                    .collect();
                app.world_mut()
                    .resource_mut::<FarStock>()
                    .request_dirty
                    .extend(cells);
            }
            Action::Settle => {}
        }
        checked_update(
            &mut app,
            fixture,
            &format!("step {step}: {action:?}; sequence={sequence:?}"),
        );
    }
    drop(senders);
}

#[test]
fn full_state_interleavings_match_fresh_world_replan() {
    // Immutable converter/DB inventory is built once, outside generation AND
    // shrinking. run_sequence still creates a fresh App/World on every attempt.
    let fixtures = [
        Fixture::new(),
        Fixture::from_directory(
            crate::cluster::reader::producer_fixture_tests::published_for_runtime_draw_states(),
        ),
        Fixture::from_directory(published_for_loader()),
    ];
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 32,
        max_shrink_iters: 512,
        rng_seed: RngSeed::Fixed(0x2151_0051),
        source_file: Some(file!()),
        test_name: Some(concat!(
            module_path!(),
            "::full_state_interleavings_match_fresh_world_replan"
        )),
        // Preserve default regression-file persistence, unlike the converter's
        // seed-only config: previously discovered failures must be replayed too.
        ..ProptestConfig::default()
    });
    runner
        .run(
            &(actions(), 0_usize..fixtures.len()),
            |(sequence, family)| {
                run_sequence(&fixtures[family], &sequence);
                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn full_state_runtime_parent_opposite_steps_match_fresh_world_replan() {
    let fixture = Fixture::from_directory(
        crate::cluster::reader::producer_fixture_tests::published_for_runtime_draw_states(),
    );
    let key = fixture_cell(&fixture, 0x111);
    let placing_world = cell_world(key).unwrap();
    let mut app = full_state_app(&fixture, key);
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .cluster_own_statics = true;
    checked_update(&mut app, &fixture, "runtime unbounded policy");
    // These are the documented runtime fixture's model-less enable parent and
    // opposite-state child, not assumptions about any real worldspace inventory.
    for (parent_enabled, offset) in [
        (false, IVec2::X),
        (true, -IVec2::ONE),
        (false, IVec2::new(12, -9)),
        (true, IVec2::ZERO),
    ] {
        app.world_mut().trigger(SetReferenceEnabled {
            reference: 0x110,
            enabled: parent_enabled,
        });
        checked_update(&mut app, &fixture, "enable-parent / opposite state");
        let far = app.world().resource::<FarStock>();
        assert_eq!(
            far.desired.contains_key(&(placing_world, 0x111)),
            parent_enabled
        );
        assert_eq!(
            far.desired.contains_key(&(placing_world, 0x112)),
            !parent_enabled
        );
        app.world_mut().resource_mut::<RenderOrigin>().0 = grid(key) + offset;
        checked_update(&mut app, &fixture, "cross after enable-parent mutation");
    }
}

#[test]
fn full_state_complete_fallback_without_near_source_adds_and_withdraws_demand() {
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
                        && reference.model_path.is_some()
                        && fixture.package.enable_catalog().enabled(row.form_id) == Some(true)
                })
        })
        .expect("loader fixture has enabled, model-bearing Complete coverage");
    let id = (cell_world(*key).unwrap(), row.form_id);
    let mut app = full_state_app(&fixture, *key);
    assert!(!app.world().resource::<FarStock>().desired.contains_key(&id));
    assert!(!actual_near_sources(app.world_mut(), &fixture.package).contains_key(&id));
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::from([row.form_id]));
    checked_update(
        &mut app,
        &fixture,
        "Complete fallback without source adds target",
    );
    let far = app.world().resource::<FarStock>();
    assert_eq!(far.desired.get(&id), Some(key));
    assert!(!far.owners.contains_key(&id));
    assert!(
        far.queued
            .iter()
            .any(|(cell, ids)| cell == key && ids.contains(&id.1))
    );
    app.world_mut()
        .resource_mut::<SourceFallbackDemand>()
        .update(Some(&fixture.package), BTreeSet::new());
    checked_update(
        &mut app,
        &fixture,
        "Complete fallback withdrawal removes unowned target",
    );
    assert!(!app.world().resource::<FarStock>().desired.contains_key(&id));
}

#[test]
fn full_state_replays_shrunk_inventory_registry_removal_regression() {
    let fixture = Fixture::new();
    assert_eq!(sequence_cell(&fixture), fixture.selected().0);
    // Original persisted RED: world 0x71, reference 0x104 has an unchanged raw
    // source after its only owner entry is removed. Keep the historical action
    // indices even though adding a fixture family changes seed interpretation.
    run_sequence(
        &fixture,
        &[Action::Duplicate(2, true), Action::RemoveOwner(8)],
    );
}

#[test]
fn full_state_pinned_interleaving_exercises_all_partition_transitions() {
    let fixture = Fixture::new();
    run_sequence(
        &fixture,
        &[
            Action::Owned(true),
            Action::Request(false),
            Action::Cross(1, -1),
            Action::Near(0, true),
            Action::Settle,
            Action::RemoveOwner(0),
            Action::Duplicate(0, true),
            Action::Settle,
            Action::Duplicate(0, false),
            Action::Near(0, false),
            Action::Request(true),
            Action::Fallback(1, true),
            Action::Cross(-3, 2),
            Action::Radius(4),
            Action::Enable(1, false),
            Action::Enable(1, true),
            Action::Fail,
            Action::Settle,
            Action::Retry,
            Action::Fallback(1, false),
            Action::Owned(false),
            Action::Cross(0, 0),
        ],
    );
}
