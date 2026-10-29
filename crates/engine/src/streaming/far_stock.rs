//! Exact source fallback for examined cluster references, using the ordinary reference loader.
//! Coverage selects demand only; render-world handoff still decides every primitive/view/pass.

use super::{
    ActiveSpace, PendingAssetProfile, RenderOrigin, RetireHidden, StreamingCommitBudget,
    StreamingWorld,
    lod::{LodDemand, LodDemandViews},
    reference_spawn::{ReferenceSpawnContext, spawn_reference},
};
use crate::cluster::{reader::StockRefusalClass, source_fallback::SourceFallbackDemand};
use crate::{
    cluster::{package::ValidatedPackage, reader::CertifiedPackage},
    config::EngineConfig,
    world::{
        components::{
            CellRef, ExpectedModelBounds, ExteriorCellGrid, FormId, InstanceBounds,
            StreamedCellKey, StreamedCellRoot, WorldTransform,
        },
        database::{CellKey, ReferenceResponse, ReferenceRow, WorldDatabase},
        reference_state::ReferenceEnableState,
    },
};
use bevy::{camera::visibility::RenderLayers, prelude::*};
use crossbeam_channel::{Receiver, TryRecvError};
use shared::cluster_mapping::CoverageState;
use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Instant,
};

#[path = "far_stock_domain.rs"]
mod domain;
#[path = "far_stock_near.rs"]
mod near;
#[path = "far_stock_owner_changes.rs"]
mod owner_changes;
#[path = "far_stock_planning.rs"]
mod planning;
use near::{NearChanges, NearSources};

type Identity = (u32, u32); // placing world, reference ID; package is retained once by the registry.

/// A failed ownership transfer must preserve its source until the failed run exits.
#[derive(Component)]
pub(super) struct FailedOwnerRoot;

#[derive(Clone)]
struct Owner {
    entity: Entity,
    parent: Entity,
    key: CellKey,
    near_key: Option<CellKey>,
    near_root: Option<Entity>,
    far: bool,
    row: Option<ReferenceRow>,
    physics_requested: bool,
}

struct Pending {
    ids: BTreeSet<u32>,
    receiver: Receiver<ReferenceResponse>,
    ready: Option<ReferenceResponse>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct UnboundedDemandKey {
    world: Option<u32>,
    owned: bool,
    enable_revision: Option<u64>,
    refusal_revision: Option<u64>,
}

/// Test-only evidence of actual planner work, independent of target generations.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PlanningWork {
    full_rebuilds: u64,
    incremental_updates: u64,
    bounded_entries: u64,
    unbounded_entries: u64,
    near_entries: u64,
    projection_entries: u64,
    owner_entries: u64,
    owner_groups: u64,
    request_entries: u64,
    queue_entries: u64,
    priority_entries: u64,
    window_cells: u64,
    near_root_entries: u64,
    failure_discoveries: u64,
    failure_entity_entries: u64,
}

#[cfg(test)]
impl PlanningWork {
    fn publish(self, before: Self) {
        let entries = [
            (
                "far_stock/plan_full_rebuilds",
                self.full_rebuilds - before.full_rebuilds,
            ),
            (
                "far_stock/plan_incremental_updates",
                self.incremental_updates - before.incremental_updates,
            ),
            (
                "far_stock/plan_bounded_entries",
                self.bounded_entries - before.bounded_entries,
            ),
            (
                "far_stock/plan_unbounded_entries",
                self.unbounded_entries - before.unbounded_entries,
            ),
            (
                "far_stock/plan_near_entries",
                self.near_entries - before.near_entries,
            ),
            (
                "far_stock/plan_projection_entries",
                self.projection_entries - before.projection_entries,
            ),
            (
                "far_stock/plan_owner_entries",
                self.owner_entries - before.owner_entries,
            ),
            (
                "far_stock/plan_request_entries",
                self.request_entries - before.request_entries,
            ),
            (
                "far_stock/plan_queue_entries",
                self.queue_entries - before.queue_entries,
            ),
            (
                "far_stock/plan_priority_entries",
                self.priority_entries - before.priority_entries,
            ),
            (
                "far_stock/plan_window_cells",
                self.window_cells - before.window_cells,
            ),
            (
                "far_stock/plan_owner_groups",
                self.owner_groups - before.owner_groups,
            ),
        ];
        for (name, value) in entries {
            if value != 0 {
                crate::work_counters::record_count(name, value);
            }
        }
    }
}

#[derive(Resource, Default)]
pub(crate) struct FarStock {
    package: Option<Arc<CertifiedPackage>>,
    demand: Option<LodDemand>,
    /// None scans the whole worldspace for enabled, refused static scenery.
    reach: Option<i32>,
    stream_radius: i32,
    generation: u64,
    desired: HashMap<Identity, CellKey>,
    /// Package allocation is checked separately; camera demand never invalidates this projection.
    unbounded_key: Option<UnboundedDemandKey>,
    unbounded_desired: HashMap<Identity, CellKey>,
    bounded_desired: HashMap<Identity, CellKey>,
    desired_cells: HashMap<CellKey, BTreeSet<u32>>,
    target_dirty: HashSet<Identity>,
    owner_dirty: HashSet<Identity>,
    request_dirty: HashSet<CellKey>,
    near_sources: NearSources,
    owners: HashMap<Identity, Owner>,
    by_entity: HashMap<Entity, HashSet<Identity>>,
    /// Parent and remembered near-root associations, including borrowed far owners.
    by_root: HashMap<Entity, HashSet<Identity>>,
    /// Remembered near-cell associations survive replacement of a near root.
    by_near_key: HashMap<CellKey, HashSet<Identity>>,
    holders: HashMap<CellKey, Entity>,
    pending: HashMap<CellKey, Pending>,
    failed: HashMap<CellKey, String>,
    duplicate_conflicts: HashMap<Identity, CellKey>,
    submission_failure: Option<String>,
    queued: VecDeque<(CellKey, BTreeSet<u32>)>,
    /// Planning groups requests by cell, so each queued cell occurs exactly once.
    queued_keys: HashSet<CellKey>,
    near_roots: HashMap<CellKey, Entity>,
    near_states: HashMap<Entity, bool>,
    near_reconcile_dirty: bool,
    near_context: Option<(Option<u32>, u32)>,
    failure_dirty: bool,
    fallback_revision: Option<u64>,
    refusal_revision: Option<u64>,
    permanent_refusals: BTreeSet<u32>,
    enable_revision: Option<u64>,
    fallback_references: BTreeSet<u32>,
    qualification_pending: bool,
    #[cfg(test)]
    planning_work: PlanningWork,
}

pub(crate) struct FarStockPlugin;
impl Plugin for FarStockPlugin {
    fn build(&self, app: &mut App) {
        crate::world::reference_state::install(app);
        super::partial_stock::install(app);
        domain::install(app.world_mut());
        app.init_resource::<FarStock>()
            .init_resource::<SourceFallbackDemand>()
            .add_systems(Update, plan.before(super::plan_cells))
            .add_systems(
                Update,
                collect
                    .after(super::lod::collect_lod_chunks)
                    .before(super::arm_pending_models),
            )
            .add_systems(
                Update,
                publish_work
                    .after(super::track_asset_readiness)
                    .before(crate::swap_bridge::systems::DestinationReportsUpdated)
                    .before(crate::pacing::track_world_ready),
            );
    }
}

fn cell_world(key: CellKey) -> Option<u32> {
    match key {
        CellKey::Exterior { worldspace_id, .. } => Some(worldspace_id),
        _ => None,
    }
}

// Identical near-first order for sparse request planning and ready publication.
fn sparse_cell_order(key: CellKey, center: IVec2) -> (i64, u32, i32, i32) {
    match key {
        CellKey::Exterior {
            worldspace_id,
            grid_x,
            grid_y,
        } => (
            (i64::from(grid_x) - i64::from(center.x))
                .abs()
                .max((i64::from(grid_y) - i64::from(center.y)).abs()),
            worldspace_id,
            grid_x,
            grid_y,
        ),
        CellKey::Interior(_) => unreachable!("sparse sources require exterior position grids"),
    }
}

/// A sparse source holder is a real exterior root, separate from resident cell accounting.
#[derive(Component)]
struct DistantSourceRoot;

fn holder_visibility(active: bool) -> Visibility {
    if active {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    }
}

/// A near root can receive sources only while the actual active exterior owns it.
/// Mere visibility is insufficient; require the loader's stamped identity and live root marker.
fn near_active(world: &World, root: Entity) -> bool {
    let (Some(key), Some(space), Some(config)) = (
        world.get::<StreamedCellKey>(root),
        world.get_resource::<ActiveSpace>(),
        world.get_resource::<EngineConfig>(),
    ) else {
        return false;
    };
    world.get::<StreamedCellRoot>(root).is_some()
        && world.get::<RetireHidden>(root).is_none()
        && world
            .get::<Visibility>(root)
            .is_some_and(|v| *v != Visibility::Hidden)
        && space.interior.is_none()
        && cell_world(key.0) == Some(space.exterior_worldspace(config.worldspace_id))
}

fn demand_contains(demand: LodDemand, world: u32, cell: IVec2, reach: i32) -> bool {
    world == demand.world
        && (i64::from(cell.x) - i64::from(demand.center.x)).abs() <= i64::from(reach.max(0))
        && (i64::from(cell.y) - i64::from(demand.center.y)).abs() <= i64::from(reach.max(0))
}

fn needs_stock(reference: &crate::cluster::reader::PlacedReference) -> bool {
    // Complete has no missing source primitive. All other examined verdicts retain the source;
    // unknown expected sets are never interpreted as empty. The retained path uses the ordinary
    // loader's resolver policy: marker/effect/no-model references request no scene on either path.
    // A known path to a missing converted file still demands stock and reports its load failure.
    reference.coverage != CoverageState::Complete && reference.model_path.is_some()
}

fn full_stock_reach(reference: &crate::cluster::reader::PlacedReference) -> bool {
    needs_stock(reference)
        && reference.is_static_scenery
        && matches!(
            reference.refusal_class,
            StockRefusalClass::Look | StockRefusalClass::Runtime
        )
}

fn reference_enabled(
    package: &CertifiedPackage,
    reference: u32,
    state: Option<&ReferenceEnableState>,
) -> Option<bool> {
    state.filter(|state| state.is_bound()).map_or_else(
        || package.enable_catalog().enabled(reference),
        |state| state.enabled(reference),
    )
}

fn complete_source_key(
    package: &CertifiedPackage,
    demand: LodDemand,
    stream_radius: i32,
    reference_id: u32,
    source_cell: u32,
) -> Option<CellKey> {
    let reference = package.placed_reference(reference_id)?;
    let snapshot = &reference.snapshot;
    (reference.coverage == CoverageState::Complete
        && reference.model_path.is_some()
        && snapshot.source_cell_id == source_cell
        && snapshot.placed_worldspace_id == demand.world
        && demand_contains(
            demand,
            snapshot.placed_worldspace_id,
            IVec2::new(snapshot.cell.0, snapshot.cell.1),
            stream_radius,
        ))
    .then_some(CellKey::Exterior {
        worldspace_id: demand.world,
        grid_x: snapshot.cell.0,
        grid_y: snapshot.cell.1,
    })
}

#[derive(bevy::ecs::system::SystemParam)]
struct EnableStateContext<'w> {
    state: Option<ResMut<'w, ReferenceEnableState>>,
    proxies: Option<ResMut<'w, crate::cluster::runtime_state::RuntimeReferenceState>>,
}

#[allow(clippy::too_many_arguments)]
fn plan(
    mut commands: Commands,
    config: Res<EngineConfig>,
    origin: Res<RenderOrigin>,
    views: LodDemandViews,
    space: Option<Res<ActiveSpace>>,
    package: Option<Res<ValidatedPackage>>,
    mut fallback: Option<ResMut<SourceFallbackDemand>>,
    ownership: Option<Res<crate::cluster::ownership::OwnershipFacts>>,
    mut far: ResMut<FarStock>,
    mut enable: EnableStateContext,
    mut near: NearChanges,
) {
    let changed_package = match (package.as_ref(), far.package.as_ref()) {
        (Some(package), Some(previous)) => !Arc::ptr_eq(previous, &package.0),
        (None, None) => false,
        _ => true,
    };
    let context = space.as_deref().map(|space| {
        (
            space.interior,
            space.exterior_worldspace(config.worldspace_id),
        )
    });
    // Absent-package mode has no camera demand to reconcile. Inspect the retained
    // journal and inputs before borrowing the index or queuing exclusive FarStock work.
    let demand = package
        .as_ref()
        .and_then(|_| views.current(&config, origin.0));
    let stream_radius = config.stream_radius.max(0);
    let reach = (!config.cluster_own_statics).then_some(stream_radius);
    let fresh_revision = package.as_ref().and_then(|package| {
        fallback
            .as_deref()
            .filter(|feedback| feedback.has_fresh())
            .and_then(|feedback| feedback.for_package(&package.0))
            .map(|(revision, _)| revision)
    });
    let demand_unchanged = package.as_ref().is_none_or(|package| {
        far.demand == demand
            && far.reach == reach
            && far.stream_radius == stream_radius
            && far.enable_revision == enable.state.as_deref().map(|state| state.revision())
            && far.refusal_revision
                == ownership
                    .as_deref()
                    .filter(|facts| facts.package_matches(&package.0))
                    .map(|facts| facts.refusal_revision())
            && far.qualification_pending == fresh_revision.is_none()
            && fresh_revision.is_none_or(|revision| far.fallback_revision == Some(revision))
    });
    if !changed_package
        && far.near_context == context
        && demand_unchanged
        && !near.has_work()
        && far.owner_dirty.is_empty()
        && far.request_dirty.is_empty()
        && far.target_dirty.is_empty()
        && !far.near_reconcile_dirty
    {
        // Equal delivery still needs its one-use acknowledgement. Do this without
        // mutating the settled FarStock index or mistaking retained demand for freshness.
        if let (Some(package), Some(feedback)) = (package.as_ref(), fallback.as_mut())
            && feedback.has_fresh()
        {
            let _ = feedback.consume_for_package(&package.0);
        }
        return;
    }
    #[cfg(test)]
    let work_before = far.planning_work;
    if far.near_context != context {
        far.near_context = context;
        far.invalidate_near();
    }
    let (near_dirty, changed, reconcile) = near.refresh(
        &mut far.near_sources,
        package.as_ref().map(|package| package.0.as_ref()),
        changed_package,
        &mut commands,
    );
    far.near_reconcile_dirty |= reconcile;
    #[cfg(test)]
    {
        far.planning_work.near_entries += changed.len() as u64;
    }
    // Check affected owners directly; ordinary metadata traffic must not walk every owner.
    let owned: HashSet<_> = changed
        .iter()
        .flat_map(|entity| {
            far.by_entity
                .get(entity)
                .into_iter()
                .chain(far.by_root.get(entity))
                .flat_map(|ids| ids.iter().copied())
        })
        .collect();
    let validating_owners = owned.clone();
    if !owned.is_empty() {
        commands.queue(move |world: &mut World| {
            with_far(world, |world, far| {
                #[cfg(test)]
                let before = far.planning_work;
                #[cfg(test)]
                {
                    far.planning_work.owner_groups += 1;
                }
                for id in owned {
                    owner_changes::reconcile_changed_owner(world, far, id);
                }
                #[cfg(test)]
                far.planning_work.publish(before);
            });
        });
    }
    commands.queue(|world: &mut World| {
        let dirty = world.resource::<FarStock>().near_reconcile_dirty
            || world
                .get_resource::<domain::DomainChanges>()
                .is_some_and(|changes| changes.near_dirty);
        if dirty {
            with_far(world, reconcile_near);
        }
    });
    let Some(package) = package else {
        if far.package.is_some() {
            let owners = far.take_owners();
            let holders = std::mem::take(&mut far.holders);
            let package = far.package.take();
            commands
                .queue(move |world: &mut World| retire_sources(world, package, owners, holders));
            far.near_roots.clear();
            far.near_states.clear();
            far.desired.clear();
            far.desired_cells.clear();
            far.target_dirty.clear();
            far.owner_dirty.clear();
            far.request_dirty.clear();
            far.unbounded_key = None;
            far.unbounded_desired.clear();
            far.bounded_desired.clear();
            far.pending.clear();
            far.clear_queued();
            far.reset_failures();
            far.duplicate_conflicts.clear();
            far.submission_failure = None;
            far.demand = None;
            far.fallback_revision = None;
            far.refusal_revision = None;
            far.permanent_refusals.clear();
            far.enable_revision = None;
            far.fallback_references.clear();
            far.qualification_pending = false;
        }
        return;
    };
    if changed_package {
        if let Some(state) = enable.state.as_mut()
            && state.bind(package.0.enable_catalog().clone())
        {
            if let Some(proxies) = enable.proxies.as_mut() {
                proxies.clear_enable_overrides();
            }
            commands.queue(crate::world::reference_state::reconcile_bound_roots);
        }
        if let Some(proxies) = enable.proxies.as_mut() {
            proxies.reproject_enable_overrides(&package.0);
        }
        far.fallback_revision = None;
        far.fallback_references.clear();
    }
    let enable_revision = enable.state.as_deref().map(|state| state.revision());
    let enable_changed = far.enable_revision != enable_revision;
    far.enable_revision = enable_revision;
    let refusal_revision = ownership
        .as_deref()
        .filter(|facts| facts.package_matches(&package.0))
        .map(|facts| facts.refusal_revision());
    let refusal_changed = changed_package || far.refusal_revision != refusal_revision;
    let mut fallback_changed = refusal_changed;
    far.refusal_revision = refusal_revision;
    if refusal_changed {
        far.permanent_refusals = ownership
            .as_deref()
            .filter(|facts| facts.package_matches(&package.0))
            .map(|facts| facts.permanent_refused_references())
            .unwrap_or_default();
    }
    far.qualification_pending = true;
    if let Some((revision, references)) = fallback
        .as_mut()
        .filter(|feedback| feedback.has_fresh())
        .and_then(|feedback| feedback.consume_for_package(&package.0))
    {
        far.qualification_pending = false;
        if far.fallback_revision != Some(revision) {
            far.fallback_revision = Some(revision);
            far.fallback_references.clone_from(references);
            fallback_changed = true;
        }
    }
    if changed_package || far.demand != demand {
        far.invalidate_near();
        far.failure_dirty = true;
    }
    // Owned clusters and enabled refused static scenery have no distance cutoff.
    // Unknown source feedback keeps the grid; refusal classes do not replace enable state.
    // Native LOD stays disabled in owned mode, including for grid-bound references.
    if !changed_package
        && !fallback_changed
        && !enable_changed
        && far.demand == demand
        && far.reach == reach
        && far.stream_radius == stream_radius
        && near_dirty.is_empty()
        && far.owner_dirty.is_empty()
        && far.request_dirty.is_empty()
        && far.target_dirty.is_empty()
    {
        #[cfg(test)]
        if far.planning_work != work_before {
            far.planning_work.publish(work_before);
        }
        return;
    }
    let incremental = !changed_package
        && !fallback_changed
        && !enable_changed
        && far.reach == reach
        && far.stream_radius == stream_radius
        && far.demand.map(|d| d.world) == demand.map(|d| d.world);
    #[cfg(test)]
    if incremental {
        far.planning_work.incremental_updates += 1;
    } else {
        far.planning_work.full_rebuilds += 1;
    }
    let unbounded_key = UnboundedDemandKey {
        world: demand.map(|d| d.world),
        owned: config.cluster_own_statics,
        enable_revision,
        refusal_revision,
    };
    let unbounded = if changed_package || far.unbounded_key != Some(unbounded_key) {
        let mut desired = HashMap::new();
        // Only owned mode grants whole-world reach, and ownership alone never grants it.
        if config.cluster_own_statics
            && let Some(demand) = demand
        {
            for (grid, ids) in package
                .0
                .placed_cells_in_window(demand.world, demand.center, None)
            {
                #[cfg(test)]
                {
                    far.planning_work.window_cells += 1;
                }
                #[cfg(test)]
                {
                    far.planning_work.unbounded_entries += ids.len() as u64;
                }
                let key = CellKey::Exterior {
                    worldspace_id: demand.world,
                    grid_x: grid.x,
                    grid_y: grid.y,
                };
                for id in ids {
                    if package.0.placed_reference(*id).is_some_and(|reference| {
                        reference.is_static_scenery
                            && reference.model_path.is_some()
                            && (full_stock_reach(reference) || far.permanent_refusals.contains(id))
                            && reference_enabled(&package.0, *id, enable.state.as_deref())
                                == Some(true)
                    }) {
                        desired.insert((demand.world, *id), key);
                    }
                }
            }
        }
        far.unbounded_key = Some(unbounded_key);
        Some(desired)
    } else {
        None
    };
    let owner_dirty = std::mem::take(&mut far.owner_dirty);
    let mut touched = near_dirty.clone();
    touched.extend(owner_dirty.iter().copied());
    if incremental {
        // The native index walks sparse cell metadata. Reference values are read only in
        // the symmetric difference, never the unchanged overlap or intermediate teleport cells.
        let moved = far.demand.map(|d| d.center) != demand.map(|d| d.center);
        for (window, other) in [(far.demand, demand), (demand, far.demand)]
            .into_iter()
            .filter(|_| moved)
        {
            if let Some(window) = window {
                for (grid, ids) in package.0.placed_cells_in_window(
                    window.world,
                    window.center,
                    Some(stream_radius),
                ) {
                    #[cfg(test)]
                    {
                        far.planning_work.window_cells += 1;
                    }
                    if other
                        .is_some_and(|other| other.contains_cell(window.world, grid, stream_radius))
                    {
                        continue;
                    }
                    touched.extend(ids.iter().map(|id| (window.world, *id)));
                }
            }
        }
        #[cfg(test)]
        {
            far.planning_work.bounded_entries += touched.len() as u64;
        }
        for &id in &touched {
            if let Some(key) = far.bounded_key(
                &package.0,
                demand,
                stream_radius,
                id,
                enable.state.as_deref(),
                false,
            ) {
                far.bounded_desired.insert(id, key);
            } else {
                far.bounded_desired.remove(&id);
            }
        }
    } else {
        touched.extend(far.bounded_desired.keys().copied());
        let mut bounded = HashMap::new();
        if let Some(demand) = demand {
            for (_, ids) in
                package
                    .0
                    .placed_cells_in_window(demand.world, demand.center, Some(stream_radius))
            {
                #[cfg(test)]
                {
                    far.planning_work.window_cells += 1;
                }
                #[cfg(test)]
                {
                    far.planning_work.bounded_entries += ids.len() as u64;
                }
                for id in ids {
                    let id = (demand.world, *id);
                    touched.insert(id);
                    if let Some(key) = far.bounded_key(
                        &package.0,
                        Some(demand),
                        stream_radius,
                        id,
                        enable.state.as_deref(),
                        changed_package,
                    ) {
                        bounded.insert(id, key);
                    }
                }
            }
        }
        far.bounded_desired = bounded;
    }
    if let Some(unbounded) = unbounded {
        touched.extend(far.unbounded_desired.keys().copied());
        touched.extend(unbounded.keys().copied());
        far.unbounded_desired = unbounded;
    }
    let target_changed = far.project_union(touched);
    let same_world = far.demand.map(|d| d.world) == demand.map(|d| d.world);
    let center_changed = far.demand.map(|d| d.center) != demand.map(|d| d.center);
    let role_changed = far.demand.is_some() != demand.is_some();
    far.demand = demand;
    far.reach = reach;
    far.stream_radius = stream_radius;
    // Generation is an IO scope, not a crossing counter. Retained receivers must remain
    // valid; requests whose exact required set changes are canceled individually below.
    let reset_io = changed_package || !same_world || (!incremental && target_changed);
    if reset_io {
        far.generation = far.generation.wrapping_add(1);
        far.pending.clear();
        far.clear_queued();
        far.reset_failures();
        far.duplicate_conflicts.clear();
        far.submission_failure = None;
    }
    if changed_package {
        let owners = far.take_owners();
        let holders = std::mem::take(&mut far.holders);
        let old_package = far.package.take();
        commands
            .queue(move |world: &mut World| retire_sources(world, old_package, owners, holders));
        far.near_roots.clear();
        far.near_states.clear();
        far.package = Some(package.0.clone());
    }
    let mut changed_targets = std::mem::take(&mut far.target_dirty);
    let mut discover = near_dirty;
    // Removal can leave an unchanged target with a valid, unchanged near source.
    // Rediscover before requesting another copy, even without native metadata changes.
    discover.extend(
        owner_dirty
            .into_iter()
            .filter(|id| far.desired.contains_key(id) && !far.owners.contains_key(id)),
    );
    discover.extend(changed_targets.iter().copied());
    if reset_io {
        discover.extend(far.desired.keys().copied());
    }
    // Revisit failures only where a source or target changed, before duplicate checks.
    for id in &discover {
        if let Some(&key) = far.desired.get(id) {
            far.request_dirty.insert(key);
        }
    }
    let retry_keys = far.request_dirty.clone();
    for key in retry_keys {
        far.retry_failure(&key);
    }
    let mut retired = HashMap::new();
    for id in changed_targets.drain() {
        #[cfg(test)]
        {
            far.planning_work.owner_entries += 1;
        }
        if !far.desired.contains_key(&id)
            && let Some(owner) = far.remove_owner(&id)
            && owner.far
        {
            retired.insert(id, owner);
        }
    }
    if !retired.is_empty() {
        let old_package = far.package.clone();
        commands.queue(move |world: &mut World| {
            retire_sources(world, old_package, retired, HashMap::new())
        });
    }
    // Discovery is indexed by the affected identity, including all duplicates. Never choose
    // an arbitrary surviving source when two direct near roots claim the same reference.
    for id in discover {
        far.duplicate_conflicts.remove(&id);
        if !far.desired.contains_key(&id) {
            continue;
        }
        let sources: Vec<_> = far
            .near_sources
            .by_identity
            .get(&id)
            .into_iter()
            .flat_map(|sources| sources.iter().map(|(&entity, &source)| (entity, source)))
            .collect();
        #[cfg(test)]
        {
            far.planning_work.near_entries += sources.len() as u64;
        }
        if sources.len() > 1
            || sources.first().is_some_and(|(entity, _)| {
                far.owners
                    .get(&id)
                    .is_some_and(|owner| owner.entity != *entity)
            })
        {
            // A pending raw validation must still see this owner. It removes the
            // registry entry after checking the physical identity, including hidden entities.
            if !validating_owners.contains(&id) {
                far.remove_owner(&id);
            }
            let key = far.desired[&id];
            far.duplicate_conflicts.insert(id, key);
            far.failed.insert(
                key,
                format!(
                    "duplicate existing reference {:08X} in placing world {:08X}",
                    id.1, id.0
                ),
            );
            continue;
        }
        if far.owners.contains_key(&id) {
            continue;
        }
        if let Some((entity, source)) = sources.first().copied() {
            far.near_roots.insert(source.key, source.parent);
            far.put_owner(
                id,
                Owner {
                    entity,
                    parent: source.parent,
                    key: source.key,
                    near_key: Some(source.key),
                    near_root: Some(source.parent),
                    far: false,
                    row: None,
                    physics_requested: config.interactive_world_physics(),
                },
            );
        }
    }
    if reset_io {
        let keys: Vec<_> = far.desired_cells.keys().copied().collect();
        far.request_dirty.extend(keys);
    }
    let keys = std::mem::take(&mut far.request_dirty);
    let queued_added = far.reconcile_requests(keys);
    if center_changed || queued_added {
        far.reorder_queued(demand.map_or(IVec2::ZERO, |d| d.center));
    }
    if role_changed {
        commands.queue(|world: &mut World| with_far(world, refresh_holders));
    } else if target_changed {
        commands.queue(|world: &mut World| with_far(world, prune_empty_holders));
    }
    debug!(
        generation = far.generation,
        desired = far.desired.len(),
        resident = far.owners.len(),
        failed_cells = far.failed.len(),
        ?demand,
        incremental,
        center_changed,
        role_changed,
        "exact distant source demand reconciled"
    );
    #[cfg(test)]
    far.planning_work.publish(work_before);
}

#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct CollectParams<'w, 's> {
    commands: Commands<'w, 's>,
    config: Res<'w, EngineConfig>,
    ownership: Option<Res<'w, crate::cluster::ownership::OwnershipFacts>>,
    enable_state: Option<Res<'w, ReferenceEnableState>>,
    database: Res<'w, WorldDatabase>,
    asset_server: Res<'w, AssetServer>,
    origin: Res<'w, RenderOrigin>,
    far: ResMut<'w, FarStock>,
    streaming: ResMut<'w, StreamingWorld>,
    budget: ResMut<'w, StreamingCommitBudget>,
    runtime: Option<Res<'w, super::runtime::StreamingRuntime>>,
}

/// IO responses are polled independently of publication. Publication applies one
/// exact response before admitting the next, so the shared clock includes hooks
/// and command application as well as validation and command construction.
pub(super) fn collect(
    world: &mut World,
    state: &mut bevy::ecs::system::SystemState<CollectParams>,
) {
    let ready_keys = {
        let CollectParams { mut far, .. } = state
            .get_mut(world)
            .expect("sparse collector resources must remain present");
        if far.package.is_none() {
            return;
        }
        let generation = far.generation;
        let mut ready = Vec::new();
        for (&key, pending) in &mut far.pending {
            if pending.ready.is_none() {
                match pending.receiver.try_recv() {
                    Ok(response) => {
                        pending.ready = Some(response);
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        // Service a disconnected request as failed work under
                        // the same publication budget, without losing its key.
                        pending.ready = Some(ReferenceResponse {
                            generation,
                            key,
                            result: Err("sparse database response disconnected".into()),
                            query_micros: 0,
                            queue_wait_micros: 0,
                            total_request_micros: 0,
                        });
                    }
                }
            }
            if pending.ready.is_some() {
                ready.push(key);
            }
        }
        let center = far.demand.map_or(IVec2::ZERO, |demand| demand.center);
        ready.sort_by_key(|key| sparse_cell_order(*key, center));
        ready
    };
    for key in ready_keys {
        {
            let CollectParams {
                mut commands,
                config,
                ownership,
                enable_state,
                asset_server,
                origin,
                mut far,
                mut streaming,
                mut budget,
                runtime,
                ..
            } = state
                .get_mut(world)
                .expect("sparse collector resources must remain present");
            if !far
                .pending
                .get(&key)
                .is_some_and(|pending| pending.ready.is_some())
            {
                continue;
            }
            if (budget.commits > 0
                && config.max_commit_micros_per_frame > 0
                && budget.frame_started.elapsed().as_micros()
                    >= u128::from(config.max_commit_micros_per_frame))
                || budget.elapsed_limit_reached(runtime.as_deref())
            {
                break;
            }
            // Invalid and failed responses cost real work too. Full-cell slots
            // belong only to full-cell commits and are not spent here.
            budget.commits = budget.commits.saturating_add(1);
            let pending = far.pending.remove(&key).unwrap();
            let response = pending.ready.unwrap();
            if response.generation != far.generation || response.key != key {
                far.failed.insert(
                    key,
                    "sparse response generation or cell differs from request".into(),
                );
                continue;
            }
            let rows = match response.result {
                Ok(rows) => rows,
                Err(reason) => {
                    error!(?key,%reason,"exact distant source query failed");
                    far.failed.insert(key, reason);
                    continue;
                }
            };
            let ids: BTreeSet<_> = rows.iter().map(|r| r.form_id).collect();
            if ids != pending.ids || rows.len() != ids.len() {
                far.failed
                    .insert(key, "sparse response identities differ from request".into());
                continue;
            }
            let package = far.package.as_ref().unwrap().clone();
            let verified = rows.iter().all(|row| row_matches(&package, row));
            if !verified {
                far.failed.insert(
                    key,
                    "sparse response disagrees with retained authoritative placement".into(),
                );
                continue;
            }
            let active = far.demand.is_some();
            let holder = *far.holders.entry(key).or_insert_with(|| {
                let CellKey::Exterior { grid_x, grid_y, .. } = key else {
                    unreachable!()
                };
                commands
                    .spawn((
                        Name::new(format!("Exact distant sources {key:?}")),
                        DistantSourceRoot,
                        StreamedCellKey(key),
                        ExteriorCellGrid(IVec2::new(grid_x, grid_y)),
                        Transform::from_translation(super::grid_translation(
                            IVec2::new(grid_x, grid_y),
                            origin.0,
                        )),
                        holder_visibility(active),
                    ))
                    .id()
            });
            commands.queue(move |world: &mut World| {
                if let Err(reason) = validate_holder(world, holder, key) {
                    fatal(world, reason);
                }
            });
            let mut spawned = Vec::new();
            let mut missing_models = Vec::new();
            commands.entity(holder).with_children(|parent| {
                let mut context = ReferenceSpawnContext {
                    key,
                    model_sequence: &mut streaming.next_model_sequence,
                    asset_server: &asset_server,
                    actor_catalog: &crate::world::actor_catalog::ActorCatalog::default(),
                    world_physics: false,
                    lights: config.lights,
                    schedule_scenes: super::requests::enabled(&config),
                };
                for mut row in rows {
                    let id = (cell_world(key).unwrap(), row.form_id);
                    if far.owners.contains_key(&id) || !far.desired.contains_key(&id) {
                        continue;
                    }
                    // A response carries a new-game snapshot. Resolve session state at publication,
                    // including state changes while this exact-ID request was in flight.
                    if let Some(state) = enable_state.as_deref().filter(|state| state.is_bound()) {
                        row.initially_enabled = state.enabled(row.form_id) == Some(true);
                    }
                    let Some((entity, requested)) =
                        spawn_reference(parent, row.clone(), &mut context)
                    else {
                        continue;
                    };
                    let beyond_grid = far.demand.is_some_and(|demand| {
                        let CellKey::Exterior { grid_x, grid_y, .. } = key else {
                            return false;
                        };
                        !demand_contains(
                            demand,
                            demand.world,
                            IVec2::new(grid_x, grid_y),
                            far.stream_radius,
                        )
                    });
                    if let Some(partial) = super::partial_stock::PartialStock::for_demand(
                        &package,
                        &row,
                        config.cluster_own_statics,
                        beyond_grid,
                        ownership.as_deref(),
                    ) {
                        parent.commands().entity(entity).insert(partial);
                    }
                    if !requested {
                        warn!(
                            reference = row.form_id,
                            ?key,
                            "distant stock source has no ordinary model request"
                        );
                        missing_models.push(entity);
                        far.failed.insert(
                            key,
                            format!(
                                "reference {:08X} has no ordinary model request",
                                row.form_id
                            ),
                        );
                    }
                    spawned.push((
                        id,
                        Owner {
                            entity,
                            parent: holder,
                            key,
                            near_key: None,
                            near_root: None,
                            far: true,
                            row: Some(row),
                            physics_requested: false,
                        },
                    ));
                }
            });
            for entity in missing_models {
                commands.entity(entity).insert(super::AssetFailed);
            }
            for (id, owner) in spawned {
                far.put_owner(id, owner);
            }
        }
        // All SystemState borrows end above. Every resource, including FarStock
        // and StreamingCommitBudget, stays installed while commands and observers
        // run. Reacquire the live clock only after this response has applied.
        state.apply(world);
    }
    let CollectParams {
        config,
        database,
        mut far,
        streaming,
        runtime,
        ..
    } = state
        .get_mut(world)
        .expect("sparse collector resources must remain present");
    let dispatch_started = Instant::now();
    let mut dispatch_considered = 0_u64;
    while !far.queued.is_empty() {
        // Reuse existing controlled admission and backlog policy. Ready replies
        // retain their outstanding place until publication, not just worker IO.
        let backlog_limit = config.streaming_backlog_limit();
        if runtime
            .as_deref()
            .is_some_and(|runtime| !runtime.cell_requests_allowed())
            || (backlog_limit != 0
                && far
                    .pending
                    .len()
                    .saturating_add(streaming.outstanding_cells.len())
                    >= backlog_limit)
        {
            break;
        }
        if dispatch_considered > 0
            && runtime
                .as_deref()
                .and_then(|runtime| runtime.decision)
                .is_some_and(|decision| {
                    dispatch_started.elapsed().as_micros()
                        >= u128::from(decision.budgets.max_admission_micros)
                })
        {
            break;
        }
        let Some((key, mut ids)) = far.pop_queued() else {
            break;
        };
        dispatch_considered += 1;
        let placing_world = cell_world(key).unwrap();
        ids.retain(|id| {
            !far.owners.contains_key(&(placing_world, *id))
                && far.desired.get(&(placing_world, *id)) == Some(&key)
        });
        if far.pending.contains_key(&key) {
            // A receiver owns this cell until exact reconciliation explicitly replaces it.
            continue;
        }
        if ids.is_empty() {
            continue;
        }
        match database.request_references(far.generation, key, ids.clone()) {
            Ok(receiver) => {
                far.pending.insert(
                    key,
                    Pending {
                        ids,
                        receiver,
                        ready: None,
                    },
                );
                far.submission_failure = None;
            }
            Err(error) => {
                let reason = format!("{error:#}");
                if far.submission_failure.as_ref() != Some(&reason) {
                    warn!(%reason,"exact distant source request remains pending");
                    far.submission_failure = Some(reason);
                }
                far.requeue_front(key, ids);
                break;
            }
        }
    }
}

fn publish_work(world: &mut World) {
    let far = world.resource::<FarStock>();
    let covered = world
        .get_resource::<domain::DomainChanges>()
        .is_some_and(|changes| {
            changes.covers(world.resource::<bevy::ecs::entity_disabling::DefaultQueryFilters>())
        });
    let discover = far.failure_dirty
        || !covered
        || world
            .get_resource::<domain::DomainChanges>()
            .is_some_and(|changes| changes.failure_dirty);
    let output = (
        far.queued.len(),
        far.pending.len(),
        usize::from(far.package.is_some() && far.demand.is_some() && far.qualification_pending),
        far.failed.len(),
    );
    let metrics = world.resource::<super::StreamingMetrics>();
    let previous = (
        metrics.queued_far_stock_queries,
        metrics.pending_far_stock_queries,
        metrics.pending_far_stock_qualification,
        metrics.failed_far_stock_sources,
    );
    let first_refusal = !far.failed.is_empty()
        && !world.contains_resource::<crate::streaming::failure::LoadingRefusal>();
    if !discover && output == previous && !first_refusal {
        return;
    }
    if discover {
        world.resource_scope(|world, mut far: Mut<FarStock>| {
            if !covered {
                // Explicit journal/domain recovery, with the existing observer owner.
                domain::install(world);
            }
            if far.failure_dirty {
                far.failure_dirty = false;
            }
            if let Some(mut changes) = world.get_resource_mut::<domain::DomainChanges>()
                && changes.failure_dirty
            {
                changes.failure_dirty = false;
            }
            if let Some(placing_world) = far.demand.map(|demand| demand.world) {
                #[cfg(test)]
                {
                    far.planning_work.failure_discoveries += 1;
                }
                #[cfg(test)]
                let mut failed_entries = 0;
                let mut failed = Vec::new();
                // Real failure producers mark the placed reference, already indexed by ownership.
                // Check those exact retained IDs without applying native default query disablers.
                // This loop runs only for a failure, ownership/reset or journal-recovery wake.
                for (&identity, owner) in &far.owners {
                    if identity.0 != placing_world {
                        continue;
                    }
                    if world.get::<super::AssetFailed>(owner.entity).is_some()
                        && world
                            .get::<FormId>(owner.entity)
                            .is_some_and(|form| form.0 == identity.1)
                    {
                        #[cfg(test)]
                        {
                            failed_entries += 1;
                        }
                        let key = far.desired.get(&identity).copied().unwrap_or(owner.key);
                        failed.push((
                            key,
                            format!(
                                "reference {:08X} failed ordinary asset validation",
                                identity.1
                            ),
                        ));
                    }
                }
                #[cfg(test)]
                {
                    far.planning_work.failure_entity_entries += failed_entries;
                }
                for (key, reason) in failed {
                    far.failed.entry(key).or_insert(reason);
                }
            }
        });
    }
    let far = world.resource::<FarStock>();
    let output = (
        far.queued.len(),
        far.pending.len(),
        usize::from(far.package.is_some() && far.demand.is_some() && far.qualification_pending),
        far.failed.len(),
    );
    let reason = if !world.contains_resource::<crate::streaming::failure::LoadingRefusal>() {
        far.failed
            .iter()
            .next()
            .map(|(key, reason)| format!("required stock source in {key:?} failed: {reason}"))
    } else {
        None
    };
    if output != previous {
        let mut metrics = world.resource_mut::<super::StreamingMetrics>();
        if metrics.queued_far_stock_queries != output.0 {
            metrics.queued_far_stock_queries = output.0;
        }
        if metrics.pending_far_stock_queries != output.1 {
            metrics.pending_far_stock_queries = output.1;
        }
        if metrics.pending_far_stock_qualification != output.2 {
            metrics.pending_far_stock_qualification = output.2;
        }
        if metrics.failed_far_stock_sources != output.3 {
            metrics.failed_far_stock_sources = output.3;
        }
    }
    if let Some(reason) = reason {
        fatal(world, reason);
    }
}

/// Extra residency facts only. Drawable geometry/material/pass proof remains in the render world.
#[derive(Default)]
pub(crate) struct FarCellState {
    pub(crate) pending: usize,
    pub(crate) failed: bool,
    pub(crate) holder: Option<Entity>,
}

fn row_matches(package: &CertifiedPackage, row: &ReferenceRow) -> bool {
    let Some(placed) = package.placed_reference(row.form_id) else {
        return false;
    };
    row.base_form_id == placed.snapshot.base_form_id
        && row.cell_id == placed.snapshot.source_cell_id
        && row.position.map(f32::to_bits) == placed.pose.position.map(f32::to_bits)
        && row.rotation.map(f32::to_bits) == placed.pose.rotation.map(f32::to_bits)
        && row.scale.to_bits() == placed.pose.scale.to_bits()
        && shared::cluster_recipe::classify::spawned_model_path(
            row.model_path.as_deref(),
            row.base_is_marker,
        ) == placed.model_path
}

fn fatal(world: &mut World, reason: String) {
    crate::streaming::failure::refuse_loading(
        world,
        format!("exact distant source refused: {reason}"),
    );
}

fn ownership_failed(world: &mut World, far: &mut FarStock, owner: &Owner, reason: String) {
    let mut seen = HashSet::new();
    let mut protect = Vec::new();
    let mut current = Some(owner.entity);
    // The current topology can differ from the remembered association that just failed.
    // Preserve both, including a newly nested source, before any queued ancestor disposal.
    while let Some(entity) = current {
        if !seen.insert(entity) {
            break;
        }
        protect.push(entity);
        current = world.get::<ChildOf>(entity).map(ChildOf::parent);
    }
    if seen.insert(owner.parent) {
        protect.push(owner.parent);
    }
    for root in protect {
        if let Ok(mut root) = world.get_entity_mut(root) {
            root.insert(FailedOwnerRoot);
        }
    }
    far.failed.insert(owner.key, reason.clone());
    fatal(world, reason);
}

pub(super) fn removal_blocked(world: &World, root: Entity) -> bool {
    world.get::<FailedOwnerRoot>(root).is_some()
}

#[cfg(test)]
pub(super) fn queue_despawn(commands: &mut Commands, root: Entity) {
    commands.queue(move |world: &mut World| {
        super::despawn_subtree(world, root);
    });
}

fn with_far(world: &mut World, action: impl FnOnce(&mut World, &mut FarStock)) {
    let Some(mut far) = world.remove_resource::<FarStock>() else {
        return;
    };
    action(world, &mut far);
    world.insert_resource(far);
}

impl FarStock {
    fn invalidate_near(&mut self) {
        self.near_reconcile_dirty = true;
        self.near_states.clear();
    }

    fn reset_failures(&mut self) {
        self.failed.clear();
        self.failure_dirty = true;
    }

    fn retry_failure(&mut self, key: &CellKey) {
        if self.failed.remove(key).is_some() {
            self.failure_dirty = true;
        }
    }

    fn enroll_bounded(&mut self, id: Identity, key: CellKey) {
        self.bounded_desired.insert(id, key);
        self.set_target(id, self.unbounded_desired.get(&id).copied().or(Some(key)));
    }

    fn put_owner(&mut self, id: Identity, owner: Owner) {
        let association_changed = self.owners.get(&id).is_none_or(|old| {
            old.entity != owner.entity
                || old.parent != owner.parent
                || old.key != owner.key
                || old.near_key != owner.near_key
                || old.near_root != owner.near_root
                || old.far != owner.far
        });
        if association_changed {
            self.invalidate_near();
            self.failure_dirty = true;
        }
        let inserted = self.unindex_owner(&id).is_none();
        self.by_entity.entry(owner.entity).or_default().insert(id);
        if inserted {
            self.owner_dirty.insert(id);
            if let Some(&key) = self.desired.get(&id) {
                self.request_dirty.insert(key);
            }
        }
        for root in [Some(owner.parent), owner.near_root].into_iter().flatten() {
            self.by_root.entry(root).or_default().insert(id);
        }
        if let Some(key) = owner.near_key {
            self.by_near_key.entry(key).or_default().insert(id);
        }
        self.owners.insert(id, owner);
    }

    fn remove_owner(&mut self, id: &Identity) -> Option<Owner> {
        let owner = self.unindex_owner(id)?;
        self.invalidate_near();
        self.failure_dirty = true;
        self.owner_dirty.insert(*id);
        if let Some(&key) = self.desired.get(id) {
            self.request_dirty.insert(key);
        }
        Some(owner)
    }

    fn unindex_owner(&mut self, id: &Identity) -> Option<Owner> {
        let owner = self.owners.remove(id)?;
        if let Some(ids) = self.by_entity.get_mut(&owner.entity) {
            ids.remove(id);
            if ids.is_empty() {
                self.by_entity.remove(&owner.entity);
            }
        }
        for root in [Some(owner.parent), owner.near_root].into_iter().flatten() {
            if let Some(ids) = self.by_root.get_mut(&root) {
                ids.remove(id);
                if ids.is_empty() {
                    self.by_root.remove(&root);
                }
            }
        }
        if let Some(key) = owner.near_key
            && let Some(ids) = self.by_near_key.get_mut(&key)
        {
            ids.remove(id);
            if ids.is_empty() {
                self.by_near_key.remove(&key);
            }
        }
        Some(owner)
    }

    fn take_owners(&mut self) -> HashMap<Identity, Owner> {
        self.invalidate_near();
        self.failure_dirty = true;
        self.owner_dirty.extend(self.owners.keys().copied());
        self.by_entity.clear();
        self.by_root.clear();
        self.by_near_key.clear();
        std::mem::take(&mut self.owners)
    }

    fn ids_for_root(&self, root: Entity) -> impl Iterator<Item = Identity> + '_ {
        self.by_root
            .get(&root)
            .into_iter()
            .flat_map(|ids| ids.iter().copied())
    }

    fn clear_queued(&mut self) {
        self.queued.clear();
        self.queued_keys.clear();
    }

    fn reorder_queued(&mut self, center: IVec2) {
        #[cfg(test)]
        {
            self.planning_work.priority_entries += self.queued.len() as u64;
        }
        self.queued
            .make_contiguous()
            .sort_by_key(|(key, _)| sparse_cell_order(*key, center));
    }

    fn pop_queued(&mut self) -> Option<(CellKey, BTreeSet<u32>)> {
        let next = self.queued.pop_front()?;
        self.queued_keys.remove(&next.0);
        Some(next)
    }

    fn requeue_front(&mut self, key: CellKey, ids: BTreeSet<u32>) {
        self.queued.push_front((key, ids));
        self.queued_keys.insert(key);
    }

    pub(crate) fn cell_state(&self, key: CellKey) -> FarCellState {
        FarCellState {
            pending: usize::from(self.pending.contains_key(&key))
                + usize::from(
                    self.qualification_pending
                        && self
                            .demand
                            .is_some_and(|demand| cell_world(key) == Some(demand.world)),
                )
                + usize::from(self.queued_keys.contains(&key)),
            failed: self.failed.contains_key(&key),
            holder: self.holders.get(&key).copied(),
        }
    }
    /// Reserve an already existing source before the ordinary spawn loop; no second scene load.
    pub(super) fn take_adoptions(
        &mut self,
        key: CellKey,
        references: &mut Vec<ReferenceRow>,
        alive: impl Fn(Entity) -> bool,
    ) -> Vec<u32> {
        let Some(world) = cell_world(key) else {
            return Vec::new();
        };
        let mut adopted = Vec::new();
        references.retain(|row| {
            let id = (world, row.form_id);
            if let Some(owner) = self.owners.get(&id) {
                if alive(owner.entity) {
                    adopted.push(row.form_id);
                    return false;
                }
                self.remove_owner(&id);
            }
            true
        });
        adopted
    }
}

fn holder(world: &mut World, far: &mut FarStock, key: CellKey) -> Result<Entity, String> {
    if let Some(&root) = far.holders.get(&key)
        && world.get_entity(root).is_ok()
    {
        validate_holder(world, root, key)?;
        return Ok(root);
    }
    let CellKey::Exterior { grid_x, grid_y, .. } = key else {
        return Err("interior sparse holder".into());
    };
    let origin = world.resource::<RenderOrigin>().0;
    let root = world
        .spawn((
            Name::new(format!("Exact distant sources {key:?}")),
            DistantSourceRoot,
            StreamedCellKey(key),
            ExteriorCellGrid(IVec2::new(grid_x, grid_y)),
            Transform::from_translation(super::grid_translation(
                IVec2::new(grid_x, grid_y),
                origin,
            )),
            holder_visibility(far.demand.is_some()),
        ))
        .id();
    far.holders.insert(key, root);
    Ok(root)
}

fn validate_holder(world: &World, root: Entity, key: CellKey) -> Result<(), String> {
    if world.get::<DistantSourceRoot>(root).is_none()
        || world.get::<StreamedCellRoot>(root).is_some()
        || world.get::<ChildOf>(root).is_some()
        || world.get::<StreamedCellKey>(root).map(|stamp| stamp.0) != Some(key)
        || !matches!(key, CellKey::Exterior { .. })
    {
        return Err("sparse source holder lost its exact unparented exterior identity".into());
    }
    Ok(())
}

fn sync_holder_layers(world: &mut World, root: Entity) {
    if world
        .get::<bevy::app::Propagate<RenderLayers>>(root)
        .is_none_or(|layers| layers.0 != super::PLACED_OBJECT_RENDER_LAYERS)
    {
        world
            .entity_mut(root)
            .insert(bevy::app::Propagate(super::PLACED_OBJECT_RENDER_LAYERS));
    }
}

/// All fallible topology checks precede transfer writes, including physics changes.
fn transfer_domain(
    world: &World,
    owner: &Owner,
    to: Entity,
    key: CellKey,
) -> Result<Vec<Entity>, String> {
    for (root, expected) in [(owner.parent, owner.key), (to, key)] {
        if world.get::<StreamedCellKey>(root).map(|stamp| stamp.0) != Some(expected)
            || (world.get::<StreamedCellRoot>(root).is_none()
                && world.get::<DistantSourceRoot>(root).is_none())
            || (world.get::<StreamedCellRoot>(root).is_some()
                && world.get::<DistantSourceRoot>(root).is_some())
            || world.get::<ChildOf>(root).is_some()
        {
            return Err(
                "source transfer requires unparented owners with exact cell identities".into(),
            );
        }
    }
    if owner.parent == to
        || world.get::<ChildOf>(owner.entity).map(ChildOf::parent) != Some(owner.parent)
    {
        return Err("source transfer requires distinct owners and a direct source child".into());
    }
    let mut nodes = Vec::new();
    let mut seen = HashSet::new();
    let mut pending = vec![owner.entity];
    while let Some(entity) = pending.pop() {
        if !seen.insert(entity)
            || entity == to
            || world.get::<StreamedCellRoot>(entity).is_some()
            || world.get::<DistantSourceRoot>(entity).is_some()
        {
            return Err("source transfer would cycle or move a nested owner root".into());
        }
        nodes.push(entity);
        if let Some(children) = world.get::<Children>(entity) {
            pending.extend(children.iter());
        }
    }
    Ok(nodes)
}

fn move_owner(
    world: &mut World,
    owner: &mut Owner,
    to: Entity,
    key: CellKey,
    far: bool,
    authoritative_pose: Option<crate::cluster::reader::ReferencePose>,
) -> Result<(), String> {
    let old_parent = *world
        .get::<Transform>(owner.parent)
        .ok_or("old source parent has no transform")?;
    let new_parent = *world
        .get::<Transform>(to)
        .ok_or("new source parent has no transform")?;
    if old_parent.rotation != Quat::IDENTITY
        || new_parent.rotation != Quat::IDENTITY
        || old_parent.scale != Vec3::ONE
        || new_parent.scale != Vec3::ONE
    {
        return Err("source owner is not an unrotated cell translation".into());
    }
    let mut pose = *world
        .get::<Transform>(owner.entity)
        .ok_or("source root disappeared")?;
    let original_pose =
        authoritative_pose.ok_or("source reference lost its authoritative placement")?;
    let expected_old = super::reference_placement(
        original_pose.position,
        original_pose.rotation,
        original_pose.scale,
        owner.key,
    )
    .1;
    if pose == expected_old {
        // Recompute from raw placement instead of adding a rounded grid delta: handoff requires
        // the identical ordinary-loader bits even for persistent references and large origins.
        pose = super::reference_placement(
            original_pose.position,
            original_pose.rotation,
            original_pose.scale,
            key,
        )
        .1;
    } else {
        // Preserve an edited source as stock; moving ownership must not reset runtime edits.
        pose.translation += old_parent.translation - new_parent.translation;
    }
    // Validate the public hierarchy contract before any write. Source layers belong
    // to the reference/model/mod and survive reparenting without being replaced.
    let nodes = transfer_domain(world, owner, to, key)?;
    let enable_colliders = !far && near_active(world, to);
    for entity in nodes {
        if world
            .get::<bevy_rapier3d::prelude::Collider>(entity)
            .is_some()
        {
            if enable_colliders {
                world
                    .entity_mut(entity)
                    .remove::<bevy_rapier3d::prelude::ColliderDisabled>();
            } else {
                world
                    .entity_mut(entity)
                    .insert(bevy_rapier3d::prelude::ColliderDisabled);
            }
        }
    }
    world.entity_mut(owner.entity).insert(ChildOf(to));
    if !far {
        super::partial_stock::promote(world, owner.entity);
    }
    let expected = world.get::<ExpectedModelBounds>(owner.entity).copied();
    let matrix = pose.to_matrix();
    world
        .entity_mut(owner.entity)
        .insert((pose, WorldTransform(matrix)));
    if let Some(expected) = expected {
        world
            .entity_mut(owner.entity)
            .insert(InstanceBounds::transformed(
                expected.min,
                expected.max,
                matrix,
            ));
    }
    owner.parent = to;
    owner.key = key;
    owner.far = far;
    if !far {
        owner.near_key = Some(key);
        owner.near_root = Some(to);
        if !owner.physics_requested && world.resource::<EngineConfig>().interactive_world_physics()
        {
            if let Some(mut pending) = world.get_mut::<PendingAssetProfile>(owner.entity) {
                pending.static_physics = true;
            } else if let Some(row) = &owner.row
                && let Some(path) = row.model_path.clone().and_then(super::converted_model_path)
            {
                world.entity_mut(owner.entity).insert(PendingAssetProfile {
                    started: Instant::now(),
                    scene_spawned: true,
                    path,
                    form_id: row.form_id,
                    base_form_id: row.base_form_id,
                    base_record_type: row.base_record_type.clone(),
                    static_physics: true,
                    cell_id: row.cell_id,
                    actor_scene: false,
                });
            }
            owner.physics_requested = true;
        }
    }
    Ok(())
}

/// Must execute before hiding or removing a near root.
pub(super) fn release(world: &mut World, root: Entity) {
    with_far(world, |world, far| {
        let ids: Vec<_> = far
            .ids_for_root(root)
            .filter(|id| {
                far.owners
                    .get(id)
                    .is_some_and(|owner| owner.parent == root && !owner.far)
            })
            .collect();
        for id in ids {
            let Some(&key) = far.desired.get(&id) else {
                far.remove_owner(&id);
                continue;
            };
            let target = match holder(world, far, key) {
                Ok(root) => root,
                Err(reason) => {
                    if let Some(owner) = far.owners.get(&id).cloned() {
                        ownership_failed(world, far, &owner, reason);
                    }
                    continue;
                }
            };
            let Some(mut owner) = far.remove_owner(&id) else {
                continue;
            };
            if let Err(reason) = move_owner(
                world,
                &mut owner,
                target,
                key,
                true,
                far.package
                    .as_ref()
                    .and_then(|p| p.placed_reference(id.1))
                    .map(|p| p.pose),
            ) {
                ownership_failed(world, far, &owner, reason);
            }
            far.put_owner(id, owner);
        }
    });
}

pub(super) fn queue_release(commands: &mut Commands, root: Entity) {
    commands.queue(move |world: &mut World| release(world, root));
}

pub(super) fn queue_adopt(commands: &mut Commands, root: Entity, key: CellKey, ids: Vec<u32>) {
    commands.queue(move |world: &mut World| {
        with_far(world, |world, far| {
            let Some(placing_world) = cell_world(key) else {
                return;
            };
            for id in ids {
                let identity = (placing_world, id);
                let Some(mut owner) = far.remove_owner(&identity) else {
                    fatal(world, "reserved source owner vanished".into());
                    continue;
                };
                owner.near_key = Some(key);
                owner.near_root = Some(root);
                if near_active(world, root)
                    && far.demand.is_some()
                    && let Err(reason) = move_owner(
                        world,
                        &mut owner,
                        root,
                        key,
                        false,
                        far.package
                            .as_ref()
                            .and_then(|p| p.placed_reference(id))
                            .map(|p| p.pose),
                    )
                {
                    ownership_failed(world, far, &owner, reason);
                }
                far.put_owner(identity, owner);
            }
            register_near(world, far, root, key);
            reconcile_near(world, far);
            prune_empty_holders(world, far);
        })
    });
}

fn register_near(world: &mut World, far: &mut FarStock, root: Entity, key: CellKey) {
    far.near_roots.insert(key, root);
    far.near_states.remove(&root);
    far.near_reconcile_dirty = true;
    let Some(placing_world) = cell_world(key) else {
        return;
    };
    let children: Vec<_> = world
        .get::<Children>(root)
        .map(|c| c.iter().collect())
        .unwrap_or_default();
    for entity in children {
        let Some(form) = world.get::<FormId>(entity) else {
            continue;
        };
        let id = (placing_world, form.0);
        // Ordinary admission can occur without camera/report movement. Enroll
        // Complete arrivals here rather than waiting for the planner's next scan.
        if let (Some(package), Some(demand), Some(cell)) =
            (&far.package, far.demand, world.get::<CellRef>(entity))
            && placing_world == demand.world
            && reference_enabled(
                package,
                form.0,
                world.get_resource::<ReferenceEnableState>(),
            ) == Some(true)
            && let Some(key) =
                complete_source_key(package, demand, far.stream_radius, form.0, cell.0)
        {
            far.enroll_bounded(id, key);
        }
        if !far.desired.contains_key(&id) {
            continue;
        }
        if let Some(existing) = far.owners.get(&id) {
            if existing.entity != entity {
                fatal(
                    world,
                    "ordinary source spawn duplicated an exact distant owner".into(),
                );
            }
            continue;
        }
        far.put_owner(
            id,
            Owner {
                entity,
                parent: root,
                key,
                near_key: Some(key),
                near_root: Some(root),
                far: false,
                row: None,
                physics_requested: world.resource::<EngineConfig>().interactive_world_physics(),
            },
        );
    }
}

pub(super) fn queue_revive(commands: &mut Commands, root: Entity, key: CellKey) {
    commands.queue(move |world: &mut World| {
        with_far(world, |world, far| {
            far.near_roots.insert(key, root);
            far.near_states.remove(&root);
            far.near_reconcile_dirty = true;
            let ids: Vec<_> = far
                .by_near_key
                .get(&key)
                .into_iter()
                .flat_map(|ids| ids.iter().copied())
                .collect();
            for id in ids {
                let mut owner = far.remove_owner(&id).unwrap();
                owner.near_root = Some(root);
                far.put_owner(id, owner);
            }
            reconcile_near(world, far);
        })
    });
}

fn source_pose(far: &FarStock, id: u32) -> Option<crate::cluster::reader::ReferencePose> {
    far.package
        .as_ref()
        .and_then(|p| p.placed_reference(id))
        .map(|p| p.pose)
}

fn prune_empty_holders(world: &mut World, far: &mut FarStock) {
    #[cfg(test)]
    crate::work_counters::record_count("far_stock/pruned_holder_visits", far.holders.len() as u64);
    // Read the actual post-transfer children before selecting roots for role writes.
    far.holders.retain(|_, root| {
        let empty = world.get::<Children>(*root).is_none_or(|c| c.is_empty());
        if empty && let Ok(entity) = world.get_entity_mut(*root) {
            entity.despawn();
        }
        !empty
    });
}

fn refresh_holders(world: &mut World, far: &mut FarStock) {
    prune_empty_holders(world, far);
    #[cfg(test)]
    crate::work_counters::record_count("far_stock/role_holder_visits", far.holders.len() as u64);
    for &root in far.holders.values() {
        let visible = holder_visibility(far.demand.is_some());
        if world.get::<Visibility>(root) != Some(&visible) {
            world.entity_mut(root).insert(visible);
        }
        sync_holder_layers(world, root);
    }
}

fn reconcile_near(world: &mut World, far: &mut FarStock) {
    let lifecycle_dirty = world
        .get_resource_mut::<domain::DomainChanges>()
        .is_some_and(|mut changes| std::mem::take(&mut changes.near_dirty));
    if !std::mem::take(&mut far.near_reconcile_dirty) && !lifecycle_dirty {
        return;
    }
    let roots: Vec<_> = far
        .near_roots
        .iter()
        .map(|(&key, &root)| (key, root))
        .collect();
    let drawing = far.demand.is_some();
    let mut moved = false;
    for (key, root) in roots {
        #[cfg(test)]
        {
            far.planning_work.near_root_entries += 1;
        }
        if world
            .get::<crate::world::components::StreamedCellRoot>(root)
            .is_none()
            || world.get::<StreamedCellKey>(root).map(|stamp| stamp.0) != Some(key)
        {
            far.near_roots.remove(&key);
            far.near_states.remove(&root);
            continue;
        }
        let current = near_active(world, root);
        if far.near_states.insert(root, current) == Some(current) {
            continue;
        }
        if !drawing {
            continue;
        }
        let ids: Vec<_> = far
            .ids_for_root(root)
            .filter(|id| far.desired.contains_key(id))
            .collect();
        for id in ids {
            let owner = far.owners.get(&id).unwrap();
            let target_near = current;
            if (target_near && owner.parent == root) || (!target_near && owner.far) {
                continue;
            }
            let target = if target_near {
                Ok((root, key))
            } else {
                let key = far.desired[&id];
                holder(world, far, key).map(|root| (root, key))
            };
            let (target, target_key) = match target {
                Ok(target) => target,
                Err(reason) => {
                    let owner = far.owners[&id].clone();
                    ownership_failed(world, far, &owner, reason);
                    continue;
                }
            };
            let mut owner = far.remove_owner(&id).unwrap();
            if let Err(reason) = move_owner(
                world,
                &mut owner,
                target,
                target_key,
                !target_near,
                source_pose(far, id.1),
            ) {
                ownership_failed(world, far, &owner, reason);
            } else {
                moved = true;
            }
            far.put_owner(id, owner);
        }
    }
    // Transfers above have already reconciled their own association writes. Keep failure
    // discovery dirty, and leave independently journaled metadata for its next reader.
    far.near_reconcile_dirty = false;
    if moved {
        // move_owner validates and rebinds the transferred sources. Other
        // holders retain their role; only now-empty holders need cleanup.
        prune_empty_holders(world, far);
    }
}

fn retire_sources(
    world: &mut World,
    package: Option<Arc<CertifiedPackage>>,
    mut owners: HashMap<Identity, Owner>,
    holders: HashMap<CellKey, Entity>,
) {
    let mut restored = HashMap::new();
    for (id, owner) in &mut owners {
        if !owner.far {
            continue;
        }
        let near = owner.near_root.zip(owner.near_key).filter(|(root, _)| {
            world
                .get::<crate::world::components::StreamedCellRoot>(*root)
                .is_some()
        });
        let Some((root, key)) = near else {
            if let Ok(entity) = world.get_entity_mut(owner.entity) {
                entity.despawn();
            }
            continue;
        };
        let pose = package
            .as_ref()
            .and_then(|p| p.placed_reference(id.1))
            .map(|p| p.pose);
        if let Err(reason) = move_owner(world, owner, root, key, false, pose) {
            with_far(world, |world, far| {
                ownership_failed(world, far, owner, reason)
            });
            return;
        }
        restored.insert(key, root);
    }
    for (_, root) in holders {
        if let Ok(entity) = world.get_entity_mut(root) {
            entity.despawn();
        }
    }
    with_far(world, |world, far| {
        for (key, root) in restored {
            register_near(world, far, root, key);
        }
        reconcile_near(world, far);
    });
}

#[cfg(test)]
#[path = "far_stock_tests.rs"]
mod tests;

#[cfg(test)]
mod public_vanilla_tests {
    use super::*;
    use crate::cluster::reader::ReferencePose;
    use bevy::{app::Propagate, camera::visibility::RenderLayers};
    use bevy_rapier3d::prelude::{Collider, ColliderDisabled};

    fn key(x: i32) -> CellKey {
        CellKey::Exterior {
            worldspace_id: 7,
            grid_x: x,
            grid_y: -2,
        }
    }

    fn root(world: &mut World, key: CellKey, distant: bool) -> Entity {
        let CellKey::Exterior { grid_x, grid_y, .. } = key else {
            unreachable!()
        };
        let mut entity = world.spawn((
            StreamedCellKey(key),
            Transform::from_translation(super::super::grid_translation(
                IVec2::new(grid_x, grid_y),
                IVec2::ZERO,
            )),
            Visibility::Inherited,
        ));
        if distant {
            entity.insert(DistantSourceRoot);
        } else {
            entity.insert(StreamedCellRoot);
        }
        entity.id()
    }

    fn owner(world: &mut World, parent: Entity, key: CellKey, pose: ReferencePose) -> Owner {
        let (_, transform) =
            super::super::reference_placement(pose.position, pose.rotation, pose.scale, key);
        let entity = world.spawn((transform, ChildOf(parent))).id();
        Owner {
            entity,
            parent,
            key,
            near_key: Some(key),
            near_root: Some(parent),
            far: false,
            row: None,
            physics_requested: true,
        }
    }

    #[test]
    fn vanilla_active_source_uses_configured_or_explicit_exterior_and_actual_retirement() {
        let mut world = World::new();
        world.insert_resource(EngineConfig {
            worldspace_id: 7,
            ..default()
        });
        world.init_resource::<ActiveSpace>();
        let root = root(&mut world, key(0), false);
        assert!(near_active(&world, root));
        world.resource_mut::<ActiveSpace>().worldspace_id = Some(8);
        assert!(!near_active(&world, root));
        world.resource_mut::<ActiveSpace>().worldspace_id = Some(7);
        world.resource_mut::<ActiveSpace>().interior = Some(9);
        assert!(!near_active(&world, root));
        world.resource_mut::<ActiveSpace>().interior = None;
        world
            .entity_mut(root)
            .insert(RetireHidden(Visibility::Inherited));
        assert!(!near_active(&world, root));
        world.entity_mut(root).remove::<RetireHidden>();
        assert!(near_active(&world, root));
        world.entity_mut(root).remove::<StreamedCellKey>();
        assert!(!near_active(&world, root));
    }

    #[test]
    fn vanilla_transfer_retains_placement_layers_entity_and_collision_authority() {
        let mut world = World::new();
        world.insert_resource(EngineConfig {
            worldspace_id: 7,
            ..default()
        });
        world.init_resource::<ActiveSpace>();
        let near = root(&mut world, key(-1), false);
        let distant = root(&mut world, key(2), true);
        let pose = ReferencePose {
            position: [-4000.25, -7000.5, 12.0],
            rotation: [0.1, 0.2, 0.3],
            scale: 1.25,
        };
        let mut owner = owner(&mut world, near, key(-1), pose);
        let entity = owner.entity;
        let layers = RenderLayers::layer(23);
        world
            .entity_mut(entity)
            .insert((Propagate(layers.clone()), Collider::ball(1.0)));
        move_owner(&mut world, &mut owner, distant, key(2), true, Some(pose)).unwrap();
        assert_eq!(owner.entity, entity);
        assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), distant);
        assert_eq!(
            world.get::<Transform>(entity),
            Some(
                &super::super::reference_placement(
                    pose.position,
                    pose.rotation,
                    pose.scale,
                    key(2)
                )
                .1
            )
        );
        assert_eq!(
            world.get::<Propagate<RenderLayers>>(entity).unwrap().0,
            layers
        );
        assert!(world.get::<ColliderDisabled>(entity).is_some());
        move_owner(&mut world, &mut owner, near, key(-1), false, Some(pose)).unwrap();
        assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), near);
        assert!(world.get::<ColliderDisabled>(entity).is_none());
        assert_eq!(
            world.get::<Propagate<RenderLayers>>(entity).unwrap().0,
            layers
        );
    }

    #[test]
    fn vanilla_transfer_rejects_wrong_stamp_and_nested_owner_before_writes() {
        let mut world = World::new();
        let near = root(&mut world, key(0), false);
        let distant = root(&mut world, key(1), true);
        let pose = ReferencePose {
            position: [0.0; 3],
            rotation: [0.0; 3],
            scale: 1.0,
        };
        let mut owner = owner(&mut world, near, key(0), pose);
        world.entity_mut(owner.entity).insert(Collider::ball(1.0));
        let before = *world.get::<Transform>(owner.entity).unwrap();
        world.entity_mut(distant).insert(StreamedCellKey(key(2)));
        assert!(move_owner(&mut world, &mut owner, distant, key(1), true, Some(pose)).is_err());
        world.entity_mut(distant).insert(StreamedCellKey(key(1)));
        world.spawn((StreamedCellRoot, ChildOf(owner.entity)));
        assert!(move_owner(&mut world, &mut owner, distant, key(1), true, Some(pose)).is_err());
        assert_eq!(world.get::<ChildOf>(owner.entity).unwrap().parent(), near);
        assert_eq!(world.get::<Transform>(owner.entity), Some(&before));
        assert!(world.get::<ColliderDisabled>(owner.entity).is_none());
        assert_eq!(owner.parent, near);
    }
}

#[cfg(test)]
mod terminal_refusal_tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<FarStock>()
            .init_resource::<super::super::StreamingMetrics>()
            .add_systems(Update, publish_work);
        app
    }

    #[test]
    fn source_failure_after_uncovered_frames_refuses_once_with_the_original_reason() {
        let mut app = app();
        app.update();
        assert!(
            !app.world()
                .contains_resource::<crate::streaming::failure::LoadingRefusal>()
        );
        let key = CellKey::Exterior {
            worldspace_id: 17,
            grid_x: -3,
            grid_y: 6,
        };
        app.world_mut()
            .resource_mut::<FarStock>()
            .failed
            .insert(key, "sparse identity mismatch".into());
        let mut exits = app
            .world()
            .resource::<Messages<AppExit>>()
            .get_cursor_current();
        app.update();
        let refusal = &app
            .world()
            .resource::<crate::streaming::failure::LoadingRefusal>()
            .0;
        assert!(refusal.contains("sparse identity mismatch"), "{refusal}");
        assert_eq!(
            app.world()
                .resource::<super::super::StreamingMetrics>()
                .failed_far_stock_sources,
            1
        );
        assert_eq!(
            exits
                .read(app.world().resource::<Messages<AppExit>>())
                .filter(|exit| exit.is_error())
                .count(),
            1
        );
        for _ in 0..4 {
            app.update();
            assert_eq!(
                exits
                    .read(app.world().resource::<Messages<AppExit>>())
                    .count(),
                0
            );
        }
    }

    #[test]
    fn an_old_asset_failure_refuses_when_ownership_arrives_after_its_added_tick() {
        let mut app = app();
        let key = CellKey::Exterior {
            worldspace_id: 17,
            grid_x: 2,
            grid_y: -4,
        };
        let parent = app.world_mut().spawn_empty().id();
        let entity = app
            .world_mut()
            .spawn((FormId(23), super::super::AssetFailed))
            .id();
        app.update();
        assert!(
            !app.world()
                .contains_resource::<crate::streaming::failure::LoadingRefusal>()
        );
        {
            let mut far = app.world_mut().resource_mut::<FarStock>();
            far.demand = Some(LodDemand {
                world: 17,
                center: IVec2::ZERO,
            });
            far.put_owner(
                (17, 23),
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
        let mut exits = app
            .world()
            .resource::<Messages<AppExit>>()
            .get_cursor_current();
        app.update();
        let refusal = &app
            .world()
            .resource::<crate::streaming::failure::LoadingRefusal>()
            .0;
        assert!(
            refusal.contains("00000017") && refusal.contains("ordinary asset validation"),
            "{refusal}"
        );
        assert_eq!(
            exits
                .read(app.world().resource::<Messages<AppExit>>())
                .filter(|exit| exit.is_error())
                .count(),
            1
        );
    }

    #[test]
    fn repeated_ownership_fatal_preserves_the_first_refusal_and_emits_only_one_exit() {
        let mut world = World::new();
        world.init_resource::<Messages<AppExit>>();
        fatal(&mut world, "first source reason".into());
        fatal(&mut world, "second source reason".into());
        assert!(
            world
                .resource::<crate::streaming::failure::LoadingRefusal>()
                .0
                .contains("first source reason")
        );
        assert_eq!(
            world
                .resource_mut::<Messages<AppExit>>()
                .drain()
                .filter(|exit| exit.is_error())
                .count(),
            1
        );
    }
}
