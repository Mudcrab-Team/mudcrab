//! World-aware stock terrain LOD selection maintained incrementally.
//!
//! For each `(grid, quadrant)`, counts of ready full-detail and LOD patches in the
//! demanded world select the stock tier. Duplicate patches contribute separately.
//! Cached ancestry records map ownership nodes to dependent patches, so structural
//! journal messages and terrain-restricted in-place change queries update only the
//! affected facts. Camera, demand and configuration changes reselect cached counts.
//!
//! Full snapshots are limited to initialization, journal loss, and compatibility
//! refreshes without a journal. Native Changed/Added filters still check ticks in
//! their matching terrain domains. Unchanged inputs perform no geometry rebuild
//! and do not mark LodStreaming changed. Tests also compare actual selected state
//! and ECS visibility with an independent full pass.
//!
//! If a default query disabler was registered after terrain tracking but before
//! these queries initialized, supplemental component-scoped observers feed the same
//! journal. Installation takes an initial snapshot and one recovery snapshot on the
//! next invocation, covering mutations before deferred observer installation. Settled
//! frames then use journal invalidation. Bevy requires disablers to be registered
//! before queries initialize; changing the filter registry at runtime is unsupported
//! by Bevy itself. A registry change after query initialization requests
//! AppExit::Error once and refuses further index work, rather than using stale queries.
//!
//! Only LOD patches carrying coverage are queried for in-place Visibility overrides.
//! They are repaired on compatibility refresh triggers. Generic tier selection does
//! not inspect ancestor visibility or replacement draw receipts; current-view
//! replacement qualification belongs to the renderer's owning pass.

use super::super::{
    MergedTerrainCoverage, RenderOrigin, StreamingCommitBudget, StreamingMetrics, TerrainCoverage,
    TerrainPatch, TerrainSurfaceReady, TerrainWorldspace,
    identity_changes::{TerrainNodeChanged, TerrainTracking},
};
use super::{
    LodChunkRoot, LodDemandViews, LodStreaming, chebyshev_grid_distance,
    select_ready_terrain_lod_tier, terrain_batching, terrain_handover_budget_available,
};
use crate::{
    config::{EngineConfig, TerrainLodDistances},
    profiling::ProfilingState,
};
use bevy::{
    ecs::{
        component::ComponentId,
        entity_disabling::DefaultQueryFilters,
        message::{MessageCursor, Messages},
        system::SystemParam,
    },
    prelude::*,
};
use shared::lod::{ChunkKey, LodTier};
use std::collections::{HashMap, HashSet};

type PatchKey = (IVec2, u8);

/// What one entity contributes to the owning-world walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OwnerFacts {
    world: Option<u32>,
    chunk: Option<ChunkKey>,
    parent: Option<Entity>,
}

impl OwnerFacts {
    /// No ownership facts. This alone does not make a node unobserved: a patch with coverage
    /// or readiness still announces its removal through the journal.
    const UNOBSERVED: Self = Self {
        world: None,
        chunk: None,
        parent: None,
    };
}

type LodTerrainOwner = (
    Option<&'static TerrainWorldspace>,
    Option<&'static LodChunkRoot>,
    Option<&'static ChildOf>,
);

/// The full-detail patches that count: ready ones. The filters are exactly the pass's original
/// ones, because a component registered as a default disabler is only matched by a query that
/// names it; an optional read or a `Has` would change which patches the pass sees.
type NearPatches = (Entity, &'static TerrainCoverage);
type NearPatchFilter = (With<TerrainPatch>, With<TerrainSurfaceReady>);

/// The LOD patches: the pass writes `Visibility` on each, ready or not.
type LodPatches = (
    Entity,
    &'static TerrainCoverage,
    Option<&'static TerrainSurfaceReady>,
    &'static mut Visibility,
);
type LodPatchFilter = Without<TerrainPatch>;

/// The same patches read without their `Visibility`, so reading them does not conflict with
/// writing it. `With<Visibility>` names the component exactly as the write does, which keeps the
/// default-disabler domain of the original query.
type LodPatchFacts = (
    Entity,
    &'static TerrainCoverage,
    Option<&'static TerrainSurfaceReady>,
);
type LodPatchFactsFilter = (With<Visibility>, Without<TerrainPatch>);

/// The LOD patches whose `Visibility` changed since the pass last ran, in the same domain.
type LodVisibilityChanged = (Entity, &'static Visibility);
type LodVisibilityChangedFilter = (
    Changed<Visibility>,
    With<TerrainCoverage>,
    Without<TerrainPatch>,
);

/// The ownership query keeps its original domain. This separate membership check only decides
/// whether an otherwise empty ancestor needs liveness polling; it never discovers patches.
type JournalObserved = Or<(
    With<Mesh3d>,
    With<Children>,
    With<TerrainCoverage>,
    With<TerrainSurfaceReady>,
    With<TerrainPatch>,
    With<Visibility>,
)>;

type LodVisibilityQueries<'w, 's> = (
    Query<'w, 's, LodPatches, LodPatchFilter>,
    Query<'w, 's, LodVisibilityChanged, LodVisibilityChangedFilter>,
);

type DrawnMergedTerrain = (
    With<TerrainPatch>,
    With<TerrainSurfaceReady>,
    Without<crate::swap_bridge::SwapWaiting>,
);

/// Everything the pass reads from the world. Ordinary stock tier selection needs the owning
/// world, not primitive draw certification: coverage may live on a glTF node with primitive
/// children.
#[derive(SystemParam)]
pub(in crate::streaming) struct TerrainLodInputs<'w, 's> {
    owners: Query<'w, 's, LodTerrainOwner>,
    readiness_changed: Query<'w, 's, Entity, Changed<TerrainSurfaceReady>>,
    merged_changed: Query<'w, 's, Entity, Changed<MergedTerrainCoverage>>,
    removed_merged: RemovedComponents<'w, 's, MergedTerrainCoverage>,
    removed_patches: RemovedComponents<'w, 's, TerrainPatch>,
    added_merged_patches: Query<'w, 's, Entity, (With<MergedTerrainCoverage>, Added<TerrainPatch>)>,
    drawn_swaps: RemovedComponents<'w, 's, crate::swap_bridge::SwapWaiting>,
    merged: Query<
        'w,
        's,
        (
            Entity,
            &'static MergedTerrainCoverage,
            &'static TerrainWorldspace,
        ),
        DrawnMergedTerrain,
    >,
    journal_observed: Query<'w, 's, (), JournalObserved>,
    near: Query<'w, 's, NearPatches, NearPatchFilter>,
    lod: Query<'w, 's, LodPatchFacts, LodPatchFactsFilter>,
    // Writing `Visibility` and asking which ones changed cannot be two plain queries.
    lod_visibility: ParamSet<'w, 's, LodVisibilityQueries<'w, 's>>,
    // The components this pass reads that are also changed in place, which no insert observer
    // sees. Each query only walks the archetypes that carry its component.
    // Keep each original query's opt-ins for components registered as default disablers.
    // A bare Changed<TerrainCoverage> would omit ready near patches or visible LOD patches.
    near_coverage_changed: Query<'w, 's, Entity, (Changed<TerrainCoverage>, NearPatchFilter)>,
    lod_coverage_changed: Query<'w, 's, Entity, (Changed<TerrainCoverage>, LodPatchFactsFilter)>,
    worldspace_changed: Query<'w, 's, Entity, Changed<TerrainWorldspace>>,
    root_changed: Query<'w, 's, Entity, Changed<LodChunkRoot>>,
    removed_roots: RemovedComponents<'w, 's, LodChunkRoot>,
    removed_worlds: RemovedComponents<'w, 's, TerrainWorldspace>,
    journal: Option<Res<'w, Messages<TerrainNodeChanged>>>,
    tracking: Option<Res<'w, TerrainTracking>>,
    filters: Option<Res<'w, DefaultQueryFilters>>,
    commands: Commands<'w, 's>,
    query_domain: Local<'s, VisibilityQueryDomain>,
    // Without the journal these are the only removal signal.
    removed_coverage: RemovedComponents<'w, 's, TerrainCoverage>,
    removed_readiness: RemovedComponents<'w, 's, TerrainSurfaceReady>,
}

/// Capture the registry when SystemParam initializes, not on the first system run.
/// A setup system can otherwise change filters after QueryState was already baked.
struct VisibilityQueryDomain {
    disablers: HashSet<ComponentId>,
}

impl FromWorld for VisibilityQueryDomain {
    fn from_world(world: &mut World) -> Self {
        Self {
            disablers: world
                .resource::<DefaultQueryFilters>()
                .disabling_ids()
                .collect(),
        }
    }
}

impl VisibilityQueryDomain {
    fn changed(&self, filters: Option<&DefaultQueryFilters>) -> bool {
        let Some(filters) = filters else {
            return !self.disablers.is_empty();
        };
        let mut count = 0;
        for id in filters.disabling_ids() {
            count += 1;
            if !self.disablers.contains(&id) {
                return true;
            }
        }
        count != self.disablers.len()
    }
}

impl TerrainLodInputs<'_, '_> {
    fn owner(&self, entity: Entity) -> Option<OwnerFacts> {
        let (world, root, parent) = self.owners.get(entity).ok()?;
        Some(OwnerFacts {
            world: world.map(|world| world.0),
            chunk: root.map(|root| root.terrain_key()),
            parent: parent.map(|parent| parent.parent()),
        })
    }

    /// An empty ancestry node needs polling only when none of the journal's observed
    /// components announces its removal. Ownership components are already in `owner`.
    fn needs_liveness_poll(&self, entity: Entity, owner: Option<OwnerFacts>) -> bool {
        owner == Some(OwnerFacts::UNOBSERVED) && !self.journal_observed.contains(entity)
    }

    /// Whether the pass reads this entity as a patch.
    fn is_patch(&self, entity: Entity) -> bool {
        self.near.contains(entity)
            || self
                .lod
                .get(entity)
                .is_ok_and(|(_, _, ready)| ready.is_some())
    }

    /// Every entity the pass reads as a patch.
    fn patches(&self) -> Vec<Entity> {
        self.near
            .iter()
            .map(|(entity, _)| entity)
            .chain(
                self.lod
                    .iter()
                    .filter_map(|(entity, _, ready)| ready.is_some().then_some(entity)),
            )
            .collect()
    }

    /// The world a patch belongs to, with every entity the walk read recorded in `steps`.
    ///
    /// A full-detail patch (`tier` `None`) must resolve to exactly one worldspace and no chunk
    /// root; a LOD patch of `tier` to exactly one chunk root of that tier and no worldspace.
    /// Anything else - a conflicting second worldspace, two chunk roots, a cycle, a missing or
    /// disabled ancestor - gives no world. Every refusal depends only on the entities in `steps`,
    /// including the one that stopped the walk.
    fn resolve(
        &self,
        patch: Entity,
        tier: Option<LodTier>,
        steps: &mut Vec<(Entity, Option<OwnerFacts>)>,
    ) -> Option<u32> {
        let mut at = patch;
        let mut near_world = None;
        let mut chunk = None;
        // Cycle detection stays linear in the depth of the ancestry, as the original walk's was.
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(at) {
                return None;
            }
            let owner = self.owner(at);
            steps.push((at, owner));
            let owner = owner?;
            if let Some(world) = owner.world
                && near_world.replace(world).is_some_and(|old| old != world)
            {
                return None;
            }
            if let Some(root) = owner.chunk
                && chunk.replace(root).is_some()
            {
                return None;
            }
            match owner.parent {
                Some(parent) => at = parent,
                None => break,
            }
        }
        match (tier, near_world, chunk) {
            (None, Some(world), None) => Some(world),
            (Some(tier), None, Some(chunk)) if chunk.tier == tier => Some(chunk.worldspace_id),
            _ => None,
        }
    }

    fn merged_snapshot(&self, world: Option<u32>) -> (HashSet<Entity>, HashSet<PatchKey>) {
        let mut entities = HashSet::new();
        let mut coverage = HashSet::new();
        for (entity, merged, owner) in &self.merged {
            entities.insert(entity);
            if Some(owner.0) == world {
                coverage.extend((0..4).map(|quadrant| (merged.grid, quadrant)));
            }
        }
        (entities, coverage)
    }

    /// Whether the terrain journal covers every input of the pass, so changes can be applied
    /// from it instead of rebuilt.
    fn journaled(&self) -> bool {
        self.journal.is_some()
            && self
                .tracking
                .as_deref()
                .zip(self.filters.as_deref())
                .is_some_and(|(tracking, filters)| tracking.covered(filters))
    }

    /// Cover pre-start disablers missing from the shared journal's original observers.
    /// Return true only when observers were queued, requiring a next-frame recovery
    /// snapshot to cover the interval before deferred commands install them.
    fn supplement_disablers(&mut self, covered: &mut HashSet<ComponentId>) -> bool {
        if self.journal.is_none() || self.tracking.is_none() || self.journaled() {
            return false;
        }
        let Some(filters) = self.filters.as_deref() else {
            return false;
        };
        let missing: Vec<_> = filters
            .disabling_ids()
            .filter(|id| covered.insert(*id))
            .collect();
        if missing.is_empty() {
            return false;
        }
        self.commands.spawn(
            Observer::new(
                |event: On<Insert>, mut changes: ResMut<Messages<TerrainNodeChanged>>| {
                    changes.write(TerrainNodeChanged(event.entity));
                },
            )
            .with_components(missing.clone()),
        );
        self.commands.spawn(
            Observer::new(
                |event: On<Remove>, mut changes: ResMut<Messages<TerrainNodeChanged>>| {
                    changes.write(TerrainNodeChanged(event.entity));
                },
            )
            .with_components(missing),
        );
        true
    }

    /// Appends every `Visibility` of a LOD patch that changed since the last call.
    fn changed_visibility(&mut self, out: &mut Vec<(Entity, Visibility)>) {
        out.clear();
        out.extend(
            self.lod_visibility
                .p1()
                .iter()
                .map(|(entity, visibility, ..)| (entity, *visibility)),
        );
    }

    /// Appends every entity changed since the last call. `false` if journal messages were lost
    /// before they were read: the caller must then rebuild.
    fn collect_changes(
        &mut self,
        out: &mut Vec<Entity>,
        cursor: &mut MessageCursor<TerrainNodeChanged>,
    ) -> bool {
        let Some(journal) = self.journal.as_deref() else {
            return false;
        };
        if cursor.missed_messages(journal) > 0 {
            cursor.clear(journal);
            return false;
        }
        out.extend(cursor.read(journal).map(|message| message.0));
        out.extend(self.near_coverage_changed.iter());
        out.extend(self.lod_coverage_changed.iter());
        out.extend(self.worldspace_changed.iter());
        out.extend(self.root_changed.iter());
        true
    }

    /// Marks every journal message so far as seen. A rebuild reads the world directly.
    fn skip_journal(&mut self, cursor: &mut MessageCursor<TerrainNodeChanged>) {
        if let Some(journal) = self.journal.as_deref() {
            cursor.clear(journal);
        }
    }

    /// Whether any patch lost its coverage or readiness. Without the journal that is the only
    /// signal of a removal.
    fn removals(&mut self, replaced: &HashSet<Entity>) -> bool {
        // Drain both streams before expiring exact generational transfer identities.
        let coverage = self
            .removed_coverage
            .read()
            .filter(|entity| !replaced.contains(entity))
            .count()
            != 0;
        let readiness = self
            .removed_readiness
            .read()
            .filter(|entity| !replaced.contains(entity))
            .count()
            != 0;
        coverage || readiness
    }
}

/// The inputs of tier selection that are not patches. A change of the world recounts every patch
/// from its cached world and applies every key; a config change applies every key; a camera or
/// render-origin move reselects every key and applies those whose tier changed.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Selection {
    world: Option<u32>,
    origin: IVec2,
    camera_grid: IVec2,
    distances: TerrainLodDistances,
}

#[derive(Clone, Debug, PartialEq)]
struct PatchEntry {
    coverage: TerrainCoverage,
    /// A full-detail patch rather than a LOD patch.
    near: bool,
    ready: bool,
    /// The owning world, resolved through `path`; `None` when the ancestry is refused or was
    /// not walked.
    world: Option<u32>,
    /// Every entity the ancestry walk read, the patch first. Empty when no walk was needed.
    path: Vec<Entity>,
    /// Whether this patch is counted in its key: ready, in the demanded world and, for a LOD
    /// patch, of a tier.
    counted: bool,
    /// Whether the last application selected this LOD patch.
    visible: bool,
}

impl PatchEntry {
    fn key(&self) -> PatchKey {
        (self.coverage.grid, self.coverage.quadrant)
    }

    /// The `Visibility` the last application wrote on this LOD patch.
    fn written_visibility(&self) -> Visibility {
        if self.visible {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        }
    }
}

/// An entity some patch's ancestry walk read, with what the walk saw of it.
#[derive(Debug)]
struct PathNode {
    owner: Option<OwnerFacts>,
    patches: HashSet<Entity>,
}

#[derive(Debug, Default)]
struct KeyState {
    /// Counted full-detail patches: the key is covered by full detail when positive.
    near_ready: u32,
    /// Counted LOD patches per tier.
    tier_ready: [u32; LodTier::ALL.len()],
    /// Every LOD patch of the key that carries `Visibility`, counted or not.
    members: HashSet<Entity>,
    /// The tier selected at the last application.
    selected: Option<LodTier>,
}

impl KeyState {
    fn is_empty(&self) -> bool {
        self.near_ready == 0 && self.members.is_empty()
    }
}

fn near_covers(state: &KeyState) -> bool {
    state.near_ready > 0
}

fn tier_slot(tier: LodTier) -> usize {
    LodTier::ALL
        .iter()
        .position(|candidate| *candidate == tier)
        .expect("LodTier::ALL lists every tier")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Unsynced,
    /// Changes come from the terrain journal.
    Journal,
    /// No usable journal: the index is rebuilt whenever a refresh trigger fires.
    Legacy,
    /// Query domains changed after initialization; AppExit has been requested.
    Refused,
}

/// Cumulative work performed by this visibility index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TerrainLodStats {
    rebuilds: u64,
    ancestry_walks: u64,
    patches_evaluated: u64,
    keys_applied: u64,
    desyncs: u64,
}

/// A failed decrement keeps the count at zero and reports the broken index invariant.
fn checked_count<T: Default>(
    next: Option<T>,
    stats: &mut TerrainLodStats,
    counter: &str,
    key: PatchKey,
    entity: Entity,
) -> T {
    next.unwrap_or_else(|| {
        count_desync(stats, counter, key, entity);
        T::default()
    })
}

fn count_desync(stats: &mut TerrainLodStats, reason: &str, key: PatchKey, entity: Entity) {
    stats.desyncs += 1;
    error!(?key, %entity, reason, "terrain LOD visibility index desynchronized");
}

/// Test-owned snapshot of the production index work statistics.
#[cfg(test)]
#[derive(Resource, Default)]
pub(in crate::streaming) struct TerrainLodObservation {
    stats: TerrainLodStats,
    journal_lost: bool,
}

#[cfg(test)]
impl TerrainLodObservation {
    pub(in crate::streaming) fn work(&self) -> [f64; 4] {
        [
            self.stats.rebuilds,
            self.stats.ancestry_walks,
            self.stats.patches_evaluated,
            self.stats.keys_applied,
        ]
        .map(|count| count as f64)
    }
}

#[derive(Resource, Default)]
pub(in crate::streaming) struct TerrainLodIndex {
    // This consumer's cursor travels with its cache so the outer condition can
    // inspect unread/lost input without acknowledging it or mutating the index.
    cursor: MessageCursor<TerrainNodeChanged>,
    mode: Mode,
    /// Default disablers followed by this consumer's supplemental journal observers.
    supplemental_disablers: HashSet<ComponentId>,
    selection: Option<Selection>,
    /// The demanded world counts are taken for.
    world: Option<u32>,
    /// Retained batches and confirmed merged cells feed the same tier decision as source patches.
    retained: HashMap<PatchKey, u8>,
    merged: HashSet<PatchKey>,
    /// Query members whose removals must invalidate external full-detail coverage.
    merged_entities: HashSet<Entity>,
    batch_selection: HashMap<PatchKey, Option<LodTier>>,
    batched_visible: usize,
    patches: HashMap<Entity, PatchEntry>,
    nodes: HashMap<Entity, PathNode>,
    /// Empty ancestry nodes carrying no journal-observed component. Only these need polling
    /// because no message announces their despawn, including hook-skipped relationships.
    unobserved: HashSet<Entity>,
    keys: HashMap<PatchKey, KeyState>,
    /// Keys whose counts changed: their members are applied again.
    dirty: HashSet<PatchKey>,
    /// LOD patches whose `Visibility` was changed from outside and not yet written again. They
    /// wait for the next frame the old pass would have refreshed on, which tracks compatibility
    /// with that pass and is not a time slice: the indices are exact on every frame.
    overrides: HashSet<Entity>,
    apply_all: bool,
    reselect_all: bool,
    ready_patches: usize,
    visible_patches: usize,
    touched: Vec<Entity>,
    /// Formerly selected sources whose readiness was withdrawn. Never-ready sources remain
    /// outside the writer domain; their chunk root owns visibility while they load.
    withdrawn: Vec<Entity>,
    /// Scratch for the `Visibility` values reported changed this frame.
    changed: Vec<(Entity, Visibility)>,
    stats: TerrainLodStats,
    /// Consecutive missed journal reads form one warning episode.
    journal_lost: bool,
}

impl TerrainLodIndex {
    /// Takes the selection inputs of this frame: a new world recounts every patch from its cached
    /// world, any other change reselects every key. Returns whether anything differs from the
    /// last frame, which is a refresh trigger of the old pass.
    fn sync_selection(&mut self, selection: Selection) -> bool {
        let Some(previous) = self.selection.replace(selection) else {
            self.world = selection.world;
            self.apply_all = true;
            return true;
        };
        if previous.world != selection.world {
            self.world = selection.world;
            self.recount();
            self.apply_all = true;
        } else if previous.distances != selection.distances {
            // A config change writes every patch again, so a `Visibility` changed from outside is
            // restored by it as it always was.
            self.apply_all = true;
        } else if previous != selection {
            self.reselect_all = true;
        }
        previous != selection
    }

    /// Records which LOD patches carry a `Visibility` other than the one the index wrote, from
    /// the entities reported changed. A value equal to the written one is the pass's own write
    /// (or an outside write that restored it) and is not an override.
    fn note_overrides(&mut self) {
        for &(entity, actual) in &self.changed {
            match self.patches.get(&entity) {
                Some(entry) if !entry.near && actual != entry.written_visibility() => {
                    self.overrides.insert(entity);
                }
                _ => {
                    self.overrides.remove(&entity);
                }
            }
        }
    }

    /// Queues the keys of every overridden patch, the repair the old pass did by writing every
    /// patch on a refresh. Only these keys are applied; the rest of the world is untouched.
    fn restore_overrides(&mut self) {
        let Self {
            overrides,
            patches,
            dirty,
            ..
        } = self;
        for entity in overrides.drain() {
            if let Some(entry) = patches.get(&entity).filter(|entry| !entry.near) {
                dirty.insert(entry.key());
            }
        }
    }

    fn has_work(&self) -> bool {
        self.apply_all || self.reselect_all || !self.dirty.is_empty() || !self.withdrawn.is_empty()
    }

    /// Forgets everything and reads every terrain patch again.
    fn rebuild(&mut self, inputs: &TerrainLodInputs) {
        self.stats.rebuilds += 1;
        // Journal recovery still owns the last transition of sources it previously selected.
        self.withdrawn
            .extend(self.patches.iter().filter_map(|(entity, old)| {
                (!old.near && !inputs.is_patch(*entity)).then_some(*entity)
            }));
        self.patches.clear();
        self.nodes.clear();
        self.unobserved.clear();
        self.keys.clear();
        self.dirty.clear();
        self.overrides.clear();
        self.touched.clear();
        self.ready_patches = 0;
        self.visible_patches = 0;
        self.apply_all = true;
        for entity in inputs.patches() {
            self.evaluate(entity, true, inputs);
        }
    }

    /// Applies the entities collected in `touched`.
    ///
    /// A touched node whose ownership facts differ from what the walks saw sends the patches
    /// below it through the walk again. A touched patch, or an entity that is one now, is read
    /// again without a walk unless its own node changed.
    fn absorb(&mut self, inputs: &TerrainLodInputs) {
        let mut touched = std::mem::take(&mut self.touched);
        touched.extend(self.unobserved.iter().copied());
        let mut rewalk = HashSet::new();
        let mut reread = HashSet::new();
        for &entity in &touched {
            if let Some(node) = self.nodes.get_mut(&entity) {
                let live = inputs.owner(entity);
                if inputs.needs_liveness_poll(entity, live) {
                    self.unobserved.insert(entity);
                } else {
                    self.unobserved.remove(&entity);
                }
                if node.owner != live {
                    node.owner = live;
                    rewalk.extend(node.patches.iter().copied());
                }
            }
            if self.patches.contains_key(&entity) || inputs.is_patch(entity) {
                reread.insert(entity);
            }
        }
        for &patch in &rewalk {
            self.evaluate(patch, true, inputs);
        }
        for &patch in reread.difference(&rewalk) {
            self.evaluate(patch, false, inputs);
        }
        self.touched = touched;
        self.touched.clear();
    }

    /// Reads one entity as a terrain patch, replacing what the index knew of it.
    fn evaluate(&mut self, entity: Entity, rewalk: bool, inputs: &TerrainLodInputs) {
        self.stats.patches_evaluated += 1;
        let old = self.patches.remove(&entity);
        if let Some(old) = &old {
            self.uncount(entity, old);
        }
        // The same two queries the pass always read: a ready full-detail patch, or a LOD patch
        // that carries the `Visibility` the pass writes.
        let live = match inputs.near.get(entity) {
            Ok((_, coverage)) => Some((*coverage, true, true)),
            Err(_) => inputs
                .lod
                .get(entity)
                .ok()
                .map(|(_, coverage, ready)| (*coverage, false, ready.is_some())),
        };
        let Some((coverage, near, ready)) = live.filter(|(_, _, ready)| *ready) else {
            self.overrides.remove(&entity);
            if let Some(old) = old {
                if !old.near {
                    self.withdrawn.push(entity);
                }
                self.unlink(entity, &old.path);
            }
            return;
        };
        if near {
            // A full-detail patch is not written by the pass.
            self.overrides.remove(&entity);
        }
        let tier = if near { None } else { coverage.tier };
        let (world, path) = match old {
            Some(old) if !rewalk && old.near == near && old.coverage.tier == coverage.tier => {
                (old.world, old.path)
            }
            old => {
                if let Some(old) = old {
                    self.unlink(entity, &old.path);
                }
                if near || tier.is_some() {
                    self.walk(entity, tier, inputs)
                } else {
                    (None, Vec::new())
                }
            }
        };
        let mut entry = PatchEntry {
            coverage,
            near,
            ready,
            world,
            path,
            counted: false,
            visible: false,
        };
        self.count(entity, &mut entry);
        self.patches.insert(entity, entry);
    }

    fn walk(
        &mut self,
        patch: Entity,
        tier: Option<LodTier>,
        inputs: &TerrainLodInputs,
    ) -> (Option<u32>, Vec<Entity>) {
        self.stats.ancestry_walks += 1;
        let mut steps = Vec::new();
        let world = inputs.resolve(patch, tier, &mut steps);
        for &(id, owner) in &steps {
            let node = self.nodes.entry(id).or_insert_with(|| PathNode {
                owner,
                patches: HashSet::new(),
            });
            // Every node a walk read has been refreshed by its own journal entry first.
            debug_assert_eq!(node.owner, owner, "a terrain ancestor changed unannounced");
            node.owner = owner;
            node.patches.insert(patch);
            if inputs.needs_liveness_poll(id, owner) {
                self.unobserved.insert(id);
            } else {
                self.unobserved.remove(&id);
            }
        }
        (world, steps.into_iter().map(|(node, _)| node).collect())
    }

    fn unlink(&mut self, patch: Entity, path: &[Entity]) {
        for node in path {
            if let Some(entry) = self.nodes.get_mut(node) {
                entry.patches.remove(&patch);
                if entry.patches.is_empty() {
                    self.nodes.remove(node);
                    self.unobserved.remove(node);
                }
            }
        }
    }

    /// Adds a patch to its key's counts and marks the key for application.
    fn count(&mut self, entity: Entity, entry: &mut PatchEntry) {
        entry.counted = entry.ready
            && entry.world.is_some()
            && entry.world == self.world
            && (entry.near || entry.coverage.tier.is_some());
        if entry.near && !entry.counted {
            return;
        }
        let key = entry.key();
        let state = self.keys.entry(key).or_default();
        if entry.near {
            state.near_ready += 1;
        } else {
            state.members.insert(entity);
            if let (true, Some(tier)) = (entry.counted, entry.coverage.tier) {
                state.tier_ready[tier_slot(tier)] += 1;
                self.ready_patches += 1;
            }
        }
        self.dirty.insert(key);
    }

    /// Takes a patch out of its key's counts, the inverse of [`Self::count`].
    fn uncount(&mut self, entity: Entity, entry: &PatchEntry) {
        let key = entry.key();
        if entry.visible {
            self.visible_patches = checked_count(
                self.visible_patches.checked_sub(1),
                &mut self.stats,
                "visible_patches",
                key,
                entity,
            );
        }
        let Some(state) = self.keys.get_mut(&key) else {
            // An uncounted near patch never creates a key; every other patch does.
            if !entry.near || entry.counted {
                count_desync(&mut self.stats, "missing key", key, entity);
            }
            return;
        };
        if entry.near {
            if entry.counted {
                let count = &mut state.near_ready;
                *count = checked_count(
                    count.checked_sub(1),
                    &mut self.stats,
                    "near_ready",
                    key,
                    entity,
                );
            }
        } else {
            if !state.members.remove(&entity) {
                count_desync(&mut self.stats, "missing key member", key, entity);
            }
            if let (true, Some(tier)) = (entry.counted, entry.coverage.tier) {
                let count = &mut state.tier_ready[tier_slot(tier)];
                *count = checked_count(
                    count.checked_sub(1),
                    &mut self.stats,
                    "tier_ready",
                    key,
                    entity,
                );
                self.ready_patches = checked_count(
                    self.ready_patches.checked_sub(1),
                    &mut self.stats,
                    "ready_patches",
                    key,
                    entity,
                );
            }
        }
        if state.is_empty() {
            self.keys.remove(&key);
        }
        self.dirty.insert(key);
    }

    /// Counts every patch again for a new demanded world, from the worlds already resolved.
    fn recount(&mut self) {
        self.keys.clear();
        self.dirty.clear();
        self.ready_patches = 0;
        let mut patches = std::mem::take(&mut self.patches);
        for (entity, entry) in &mut patches {
            self.count(*entity, entry);
        }
        self.patches = patches;
    }

    /// Selects the tier of every key that changed, or of every key when the camera, config or
    /// demand moved, and writes `Visibility` on the patches of each key whose answer changed.
    fn reselect(&mut self, lod: &mut Query<LodPatches, LodPatchFilter>, selection: &Selection) {
        for entity in self.withdrawn.drain(..) {
            if let Ok((_, _, _, mut visibility)) = lod.get_mut(entity) {
                visibility.set_if_neq(Visibility::Hidden);
            }
        }
        let Self {
            keys,
            patches,
            dirty,
            retained,
            merged,
            overrides,
            visible_patches,
            stats,
            apply_all,
            reselect_all,
            ..
        } = self;
        let everything = *apply_all || *reselect_all;
        let mut settle = |key: PatchKey, state: &mut KeyState, forced: bool| {
            let tier = select_ready_terrain_lod_tier(
                chebyshev_grid_distance(key.0, selection.camera_grid),
                near_covers(state) || merged.contains(&key),
                |tier| {
                    state.tier_ready[tier_slot(tier)] > 0
                        || retained
                            .get(&key)
                            .is_some_and(|bits| bits & (tier as u8) != 0)
                },
                &selection.distances,
            );
            if !forced && state.selected == tier {
                return;
            }
            state.selected = tier;
            stats.keys_applied += 1;
            for entity in &state.members {
                let Some(entry) = patches.get_mut(entity) else {
                    debug_assert!(false, "key member {entity} has no patch entry");
                    continue;
                };
                // Main-world tier selection retains stock for extraction; current-view
                // Replacement rendering is decided by its owning pass.
                let visible = tier.is_some() && entry.counted && entry.coverage.tier == tier;
                if visible != entry.visible {
                    entry.visible = visible;
                    if visible {
                        *visible_patches += 1;
                    } else {
                        *visible_patches = checked_count(
                            visible_patches.checked_sub(1),
                            stats,
                            "visible_patches",
                            key,
                            *entity,
                        );
                    }
                }
                if let Ok((_, _, _, mut current)) = lod.get_mut(*entity) {
                    let next = entry.written_visibility();
                    if *current != next {
                        *current = next;
                    }
                }
                overrides.remove(entity);
            }
        };
        if everything {
            for (&key, state) in keys.iter_mut() {
                settle(key, state, *apply_all || dirty.contains(&key));
            }
        } else {
            for key in dirty.iter() {
                if let Some(state) = keys.get_mut(key) {
                    settle(*key, state, true);
                }
            }
        }
        dirty.clear();
        *apply_all = false;
        *reselect_all = false;
    }

    /// Returns whether a missed read starts a new consecutive-loss warning episode.
    fn note_journal_read(&mut self, complete: bool) -> bool {
        let warn = !complete && !self.journal_lost;
        self.journal_lost = !complete;
        warn
    }

    #[cfg(test)]
    fn audit_if_requested(&self, inputs: &mut TerrainLodInputs) {
        let result = self.audit(inputs);
        assert!(result.is_ok(), "LOD visibility audit failed: {result:?}");
        tests::assert_production_ready_count(self, inputs);
    }

    /// Checks the index against current world inputs in the exhaustive test oracle.
    #[cfg(test)]
    fn audit(&self, inputs: &mut TerrainLodInputs) -> Result<(), String> {
        let mut fresh = TerrainLodIndex {
            world: self.world,
            retained: self.retained.clone(),
            merged: inputs.merged_snapshot(self.world).1,
            ..default()
        };
        if self.merged != fresh.merged {
            return Err(
                "merged full-detail coverage differs from current query membership".to_owned(),
            );
        }
        fresh.rebuild(inputs);
        if self.patches.len() != fresh.patches.len() {
            return Err(format!(
                "tracked patch count {} differs from full pass {}",
                self.patches.len(),
                fresh.patches.len()
            ));
        }
        for (entity, entry) in &self.patches {
            let full = fresh
                .patches
                .get(entity)
                .ok_or_else(|| format!("{entity} is tracked but a full pass drops it"))?;
            let actual = (
                entry.coverage,
                entry.near,
                entry.ready,
                entry.world,
                &entry.path,
                entry.counted,
            );
            let expected = (
                full.coverage,
                full.near,
                full.ready,
                full.world,
                &full.path,
                full.counted,
            );
            if actual != expected {
                return Err(format!(
                    "patch {entity} differs from full pass: {actual:?}, expected {expected:?}"
                ));
            }
        }
        if self.keys.len() != fresh.keys.len() {
            return Err(format!(
                "key count {} differs from full pass {}",
                self.keys.len(),
                fresh.keys.len()
            ));
        }
        for (key, state) in &self.keys {
            let full = fresh
                .keys
                .get(key)
                .ok_or_else(|| format!("key {key:?} is tracked but a full pass drops it"))?;
            let actual = (state.near_ready, state.tier_ready, &state.members);
            let expected = (full.near_ready, full.tier_ready, &full.members);
            if actual != expected {
                return Err(format!(
                    "key {key:?} differs from full pass: {actual:?}, expected {expected:?}"
                ));
            }
            if let Some(selection) = self.selection {
                let expected_tier = select_ready_terrain_lod_tier(
                    chebyshev_grid_distance(key.0, selection.camera_grid),
                    near_covers(full) || self.merged.contains(key),
                    |tier| {
                        full.tier_ready[tier_slot(tier)] > 0
                            || self
                                .retained
                                .get(key)
                                .is_some_and(|bits| bits & (tier as u8) != 0)
                    },
                    &selection.distances,
                );
                if state.selected != expected_tier {
                    return Err(format!(
                        "key {key:?} selects {:?}, expected {expected_tier:?}",
                        state.selected
                    ));
                }
                for entity in &state.members {
                    let entry = self
                        .patches
                        .get(entity)
                        .ok_or_else(|| format!("key {key:?} member {entity} has no patch entry"))?;
                    let expected_visible = expected_tier.is_some()
                        && entry.counted
                        && entry.coverage.tier == expected_tier;
                    if entry.visible != expected_visible {
                        return Err(format!(
                            "patch {entity} visibility {} differs from selected {expected_visible}",
                            entry.visible
                        ));
                    }
                }
            }
        }
        if self.nodes.len() != fresh.nodes.len() {
            return Err(format!(
                "walked node count {} differs from full pass {}",
                self.nodes.len(),
                fresh.nodes.len()
            ));
        }
        for (entity, node) in &self.nodes {
            let full = fresh
                .nodes
                .get(entity)
                .ok_or_else(|| format!("node {entity} is tracked but a full pass drops it"))?;
            if (node.owner, &node.patches) != (full.owner, &full.patches) {
                return Err(format!(
                    "node {entity} differs from full pass: {:?}, expected {:?}",
                    (node.owner, &node.patches),
                    (full.owner, &full.patches)
                ));
            }
        }
        if self.unobserved != fresh.unobserved {
            return Err(format!(
                "unobserved node set {:?} differs from full pass {:?}",
                self.unobserved, fresh.unobserved
            ));
        }
        if self.ready_patches != fresh.ready_patches {
            return Err(format!(
                "ready_patches {} differs from full pass {}",
                self.ready_patches, fresh.ready_patches
            ));
        }
        for entity in &self.overrides {
            if !self.patches.get(entity).is_some_and(|entry| !entry.near) {
                return Err(format!(
                    "{entity} is held as an override but is not a LOD patch"
                ));
            }
        }
        let visible = self.patches.values().filter(|entry| entry.visible).count();
        if self.visible_patches != visible {
            return Err(format!(
                "visible_patches {} differs from tracked visible count {visible}",
                self.visible_patches
            ));
        }
        // The cached selection is only half the output invariant. Read the original write
        // query's actual component too, without widening its default-disabler domain or marking
        // it changed. Outside edits in `overrides` intentionally wait for a refresh trigger.
        let lod = inputs.lod_visibility.p0();
        for (entity, entry) in &self.patches {
            if entry.near || self.overrides.contains(entity) {
                continue;
            }
            let (_, _, _, actual) = lod
                .get(*entity)
                .map_err(|error| format!("patch {entity} has no ECS Visibility: {error}"))?;
            let expected = entry.written_visibility();
            if *actual != expected {
                return Err(format!(
                    "patch {entity} ECS Visibility {actual:?} differs from selected {expected:?}"
                ));
            }
        }
        Ok(())
    }
}

/// Read-only wake inputs. Native filters still inspect matching component ticks;
/// this condition skips index maintenance and mutable selector parameters, not those scans.
#[derive(SystemParam)]
pub(in crate::streaming) struct TerrainLodWakeInputs<'w, 's> {
    owners: Query<'w, 's, LodTerrainOwner>,
    visibility: Query<'w, 's, LodVisibilityChanged, LodVisibilityChangedFilter>,
    readiness: Query<'w, 's, Entity, Changed<TerrainSurfaceReady>>,
    merged: Query<'w, 's, Entity, Changed<MergedTerrainCoverage>>,
    added_merged: Query<'w, 's, Entity, (With<MergedTerrainCoverage>, Added<TerrainPatch>)>,
    near_coverage: Query<'w, 's, Entity, (Changed<TerrainCoverage>, NearPatchFilter)>,
    lod_coverage: Query<'w, 's, Entity, (Changed<TerrainCoverage>, LodPatchFactsFilter)>,
    worldspace: Query<'w, 's, Entity, Changed<TerrainWorldspace>>,
    roots: Query<'w, 's, Entity, Changed<LodChunkRoot>>,
    removed_coverage: RemovedComponents<'w, 's, TerrainCoverage>,
    removed_readiness: RemovedComponents<'w, 's, TerrainSurfaceReady>,
    removed_roots: RemovedComponents<'w, 's, LodChunkRoot>,
    removed_worlds: RemovedComponents<'w, 's, TerrainWorldspace>,
    removed_merged: RemovedComponents<'w, 's, MergedTerrainCoverage>,
    removed_patches: RemovedComponents<'w, 's, TerrainPatch>,
    drawn_swaps: RemovedComponents<'w, 's, crate::swap_bridge::SwapWaiting>,
    journal: Option<Res<'w, Messages<TerrainNodeChanged>>>,
    tracking: Option<Res<'w, TerrainTracking>>,
    filters: Option<Res<'w, DefaultQueryFilters>>,
    query_domain: Local<'s, VisibilityQueryDomain>,
}

/// The selector is the only writer of its private index. In particular, a noisy
/// LodStreaming change tick is not a promise that selection inputs changed.
#[allow(clippy::too_many_arguments)]
pub(in crate::streaming) fn terrain_lod_visibility_has_work(
    config: Res<EngineConfig>,
    origin: Res<RenderOrigin>,
    views: LodDemandViews,
    mut inputs: TerrainLodWakeInputs,
    streaming: Res<LodStreaming>,
    index: Res<TerrainLodIndex>,
    profiler: Res<ProfilingState>,
    mut batches: terrain_batching::LodBatchVisibilityWake,
) -> bool {
    if index.mode == Mode::Refused {
        return false;
    }
    // Wake the existing refusal path before touching stale query domains.
    if inputs.query_domain.changed(inputs.filters.as_deref()) {
        return true;
    }
    let demand = views.current(&config, origin.0);
    let selection = Selection {
        world: demand.map(|demand| demand.world),
        origin: origin.0,
        camera_grid: demand.map_or(IVec2::ZERO, |demand| demand.center),
        distances: config.terrain_lod,
    };
    let journaled = journal_is_covered(
        inputs.journal.is_some(),
        inputs.tracking.as_deref(),
        inputs.filters.as_deref(),
        &index.supplemental_disablers,
    );
    // Removal readers are condition-local. Drain every stream even when another
    // stream wakes us; the selector retains its own independent removal readers.
    let removed = [
        inputs.removed_coverage.read().count(),
        inputs.removed_readiness.read().count(),
        inputs.removed_roots.read().count(),
        inputs.removed_worlds.read().count(),
        inputs.removed_merged.read().count(),
        inputs.removed_patches.read().count(),
        inputs.drawn_swaps.read().count(),
    ]
    .into_iter()
    .any(|count| count != 0);
    let batch_work = batches.has_work();
    index.mode == Mode::Unsynced
        || index.has_work()
        || index.selection != Some(selection)
        || (journaled && index.mode != Mode::Journal)
        || (journaled && index.journal_lost)
        || (!journaled && index.mode != Mode::Legacy)
        || journaled && inputs.journal.as_deref().is_some_and(|journal| {
            index.cursor.missed_messages(journal) > 0 || !index.cursor.is_empty(journal)
        })
        || streaming.visibility_dirty
        || streaming.submission_dirty
        || !streaming.replaced_source_quadrants.is_empty()
        || removed
        || !inputs.visibility.is_empty()
        || !inputs.readiness.is_empty()
        || !inputs.merged.is_empty()
        || (!journaled && !inputs.added_merged.is_empty())
        || !inputs.near_coverage.is_empty()
        || !inputs.lod_coverage.is_empty()
        || !inputs.worldspace.is_empty()
        || !inputs.roots.is_empty()
        // Only these cached empty ancestors lack structural removal messages.
        || index.unobserved.iter().any(|entity| !inputs.owners.contains(*entity))
        || batch_work
        || profiler.gauge("lod/pending_terrain_selection_uploads").is_none()
}

fn journal_is_covered(
    present: bool,
    tracking: Option<&TerrainTracking>,
    filters: Option<&DefaultQueryFilters>,
    supplemental: &HashSet<ComponentId>,
) -> bool {
    present
        && tracking.zip(filters).is_some_and(|(tracking, filters)| {
            tracking.covered(filters)
                || filters.disabling_ids().all(|id| supplemental.contains(&id))
        })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::streaming) fn update_terrain_lod_visibility(
    config: Res<EngineConfig>,
    origin: Res<RenderOrigin>,
    views: LodDemandViews,
    mut inputs: TerrainLodInputs,
    mut streaming: ParamSet<(Res<LodStreaming>, ResMut<LodStreaming>)>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
    mut index: ResMut<TerrainLodIndex>,
    #[cfg(test)] observation: Option<ResMut<TerrainLodObservation>>,
    mut batches: terrain_batching::LodBatchVisibility,
    mut budget: Option<ResMut<StreamingCommitBudget>>,
) {
    if index.mode == Mode::Refused {
        return;
    }
    let domain_changed = inputs.query_domain.changed(inputs.filters.as_deref());
    if domain_changed {
        index.mode = Mode::Refused;
        inputs.commands.queue(|world: &mut World| {
            crate::streaming::failure::refuse_loading(
                world,
                "default query disablers changed after terrain LOD queries initialized; exiting because Bevy cannot update their domains".to_owned(),
            );
        });
        return;
    }
    let demand = views.current(&config, origin.0);
    let selection = Selection {
        world: demand.map(|demand| demand.world),
        origin: origin.0,
        camera_grid: demand.map_or(IVec2::ZERO, |demand| demand.center),
        distances: config.terrain_lod,
    };
    let replaced = if streaming.p0().replaced_source_quadrants.is_empty() {
        HashSet::new()
    } else {
        std::mem::take(&mut streaming.p1().replaced_source_quadrants)
    };
    let removals = inputs.removals(&replaced);
    let batches_removed = batches.removed();
    let roots_removed = inputs.removed_roots.read().count() != 0;
    let worlds_removed = inputs.removed_worlds.read().count() != 0;
    let batch_inputs_changed = !inputs.root_changed.is_empty()
        || roots_removed
        || !inputs.readiness_changed.is_empty()
        || removals;
    let upload_completed = batches.upload_completed();
    let submission_dirty = streaming.p0().submission_dirty;
    if submission_dirty {
        streaming.p1().submission_dirty = false;
    }
    let merged_removed = inputs.removed_merged.read().count() != 0;
    let surfaces_drawn = inputs.drawn_swaps.read().count() != 0;
    let installing_disablers = inputs.supplement_disablers(&mut index.supplemental_disablers);
    let journaled = journal_is_covered(
        inputs.journal.is_some(),
        inputs.tracking.as_deref(),
        inputs.filters.as_deref(),
        &index.supplemental_disablers,
    );
    let journal_rebuild = if journaled {
        let TerrainLodIndex {
            touched, cursor, ..
        } = &mut *index;
        let complete = inputs.collect_changes(touched, cursor);
        if index.note_journal_read(complete) {
            warn!("terrain journal messages were lost; rebuilding the LOD visibility index");
        }
        index.mode != Mode::Journal || !complete
    } else {
        false
    };
    let removed_merged_patch = inputs
        .removed_patches
        .read()
        .filter(|entity| index.merged_entities.contains(entity))
        .count()
        != 0;
    let merged_membership_changed = removed_merged_patch
        || (!journaled && !inputs.added_merged_patches.is_empty())
        || journaled
            && index.touched.iter().any(|entity| {
                index.merged_entities.contains(entity) || inputs.merged.contains(*entity)
            });
    let external_refresh = journal_rebuild
        || merged_membership_changed
        || batches_removed
        || submission_dirty
        || !replaced.is_empty()
        || removals
        || selection.world != index.world
        || batch_inputs_changed
        || !inputs.worldspace_changed.is_empty()
        || worlds_removed
        || !inputs.readiness_changed.is_empty()
        || !inputs.merged_changed.is_empty()
        || merged_removed
        || surfaces_drawn
        || index.selection != Some(selection);
    let external_changed = if external_refresh {
        let mut retained = HashMap::new();
        for coverage in batches.coverage_for_world(selection.world) {
            if let Some(tier) = coverage.tier {
                *retained
                    .entry((coverage.grid, coverage.quadrant))
                    .or_insert(0u8) |= tier as u8;
            }
        }
        let (merged_entities, merged) = inputs.merged_snapshot(selection.world);
        index.merged_entities = merged_entities;
        let external_changed = index.retained != retained || index.merged != merged;
        if external_changed {
            index.retained = retained;
            index.merged = merged;
            index.reselect_all = true;
        }
        external_changed
    } else {
        false
    };
    // The flag predates the journal: each place that sets it changes an input the journal
    // reports (a patch added or removed, a surface ready), so it only matters without one.
    let flagged = streaming.p0().visibility_dirty;
    if flagged {
        streaming.p1().visibility_dirty = false;
    }
    let selection_moved = index.sync_selection(selection);
    let rebuild = if journaled {
        journal_rebuild
    } else {
        // Without a journal nothing says which patch changed, so a refresh trigger rebuilds. A
        // changed selection is a trigger too: `sync_selection` has just queued its work.
        // A newly ready patch is a trigger even when retained/merged coverage did not change.
        index.mode != Mode::Legacy
            || roots_removed
            || worlds_removed
            || !inputs.root_changed.is_empty()
            || !inputs.worldspace_changed.is_empty()
            || !inputs.near_coverage_changed.is_empty()
            || !inputs.lod_coverage_changed.is_empty()
            || flagged
            || removals
            || inputs
                .readiness_changed
                .iter()
                .any(|entity| inputs.is_patch(entity))
            || batches_removed
            || external_changed
            || index.has_work()
    };
    if rebuild {
        {
            index.rebuild(&inputs);
        }
        if journaled {
            inputs.skip_journal(&mut index.cursor);
        }
    } else if journaled {
        index.absorb(&inputs);
    }
    index.mode = if installing_disablers {
        // Deferred installation may follow a mutation later in this frame. Force
        // exactly one recovery snapshot after the observers become active.
        Mode::Unsynced
    } else if journaled {
        Mode::Journal
    } else {
        Mode::Legacy
    };

    // A pass that rewrites every patch needs no report of what was changed from outside. Any
    // other frame notes the differences, and the frames the old pass refreshed on apply them.
    if !index.apply_all {
        let mut changed = std::mem::take(&mut index.changed);
        inputs.changed_visibility(&mut changed);
        index.changed = changed;
        index.note_overrides();
        if selection_moved || flagged || removals || external_changed || batches_removed {
            index.restore_overrides();
        }
    }

    let refresh = index.has_work();
    if refresh {
        {
            index.reselect(&mut inputs.lod_visibility.p0(), &selection);
        }
    }
    // Build the semantic mask from this one index. Batch-only keys retain their tier inputs;
    // Source patches and retained batches feed the same stock tier decision.
    if refresh
        || submission_dirty
        || external_changed
        || batches_removed
        || selection_moved
        || removals
    {
        let mut selected: HashMap<_, _> = index
            .keys
            .iter()
            .map(|(&key, state)| (key, state.selected))
            .collect();
        for (&key, &available) in &index.retained {
            selected.entry(key).or_insert_with(|| {
                select_ready_terrain_lod_tier(
                    chebyshev_grid_distance(key.0, selection.camera_grid),
                    index.merged.contains(&key),
                    |tier| available & (tier as u8) != 0,
                    &selection.distances,
                )
            });
        }
        let semantic_changed = index.batch_selection != selected;
        if semantic_changed || selection_moved || removals || batches_removed {
            let mut state = streaming.p1();
            state.visibility_revision = state.visibility_revision.wrapping_add(1);
        }
        index.batch_selection = selected;
    }
    let batch_work = refresh
        || submission_dirty
        || external_changed
        || batches_removed
        || selection_moved
        || batch_inputs_changed;
    let (batched_visible, _, _) = if batch_work {
        batches.update_for_world(&index.batch_selection, selection.world)
    } else {
        if upload_completed {
            batches.finish_uploads();
        }
        (index.batched_visible, 0, 0)
    };
    index.batched_visible = batched_visible;
    let can_transfer = budget.as_deref().is_some_and(|budget| {
        !streaming.p0().terrain_batch_commit_used
            && terrain_handover_budget_available(budget, config.max_commit_micros_per_frame)
    });
    let transferred = can_transfer && {
        let mut state = streaming.p1();
        // finalize_one writes this output only after it has a real ready transfer.
        // Publish the resource change below exactly when that transaction succeeds.
        batches.finalize_one(&mut state.bypass_change_detection().replaced_source_quadrants)
    };
    if transferred {
        let mut state = streaming.p1();
        state.terrain_batch_commit_used = true;
        state.terrain_batch_activated_chunks += 1;
        if let Some(budget) = budget.as_deref_mut() {
            budget.remaining -= 1;
            budget.commits = budget.commits.saturating_add(1);
        }
    }
    // This is a live settle/receipt contract, not an inventory counter. Unchanged
    // frames do not enumerate batches or mark the profiling resource changed.
    if batch_work
        || upload_completed
        || transferred
        || profiler
            .gauge("lod/pending_terrain_selection_uploads")
            .is_none()
    {
        let pending = batches.pending_selection_uploads() as f64;
        if profiler.gauge("lod/pending_terrain_selection_uploads") != Some(pending) {
            profiler.set_gauge("lod/pending_terrain_selection_uploads", pending);
        }
    }
    // The no-journal contract permits cached coverage between refreshes.
    // Journal mode remains exact and is audited on every invocation. Audit a
    // compatibility refresh triggers.
    if journaled || rebuild {
        #[cfg(test)]
        index.audit_if_requested(&mut inputs);
    }
    let visible = index.visible_patches + batched_visible;
    if metrics.visible_lod_terrain_patches != visible {
        metrics.visible_lod_terrain_patches = visible;
    }
    #[cfg(test)]
    if let Some(mut observation) = observation {
        observation.stats = index.stats;
        observation.journal_lost = index.journal_lost;
    }
}

#[cfg(test)]
pub(super) fn assert_removed_sources_leave_no_ghost_keys() {
    let world = 60;
    let key = (IVec2::new(-2, 3), 2);
    let entity = Entity::from_raw_u32(1).unwrap();
    let mut index = TerrainLodIndex {
        world: Some(world),
        ..default()
    };
    let mut entry = PatchEntry {
        coverage: TerrainCoverage {
            grid: key.0,
            quadrant: key.1,
            tier: Some(LodTier::Tier4),
        },
        near: false,
        ready: true,
        world: Some(world),
        path: Vec::new(),
        counted: false,
        visible: false,
    };
    index.count(entity, &mut entry);
    entry.visible = true;
    index.visible_patches = 1;
    index.patches.insert(entity, entry.clone());
    assert_eq!(index.ready_patches, 1);
    index.patches.remove(&entity);
    index.uncount(entity, &entry);
    assert!(
        index.keys.is_empty(),
        "a transferred source leaves no ghost key"
    );
    assert!(index.patches.is_empty());
    assert_eq!((index.ready_patches, index.visible_patches), (0, 0));
    assert!(
        index.dirty.contains(&key),
        "the shared batch mask is reselected after transfer"
    );
}

#[cfg(test)]
mod tests;
