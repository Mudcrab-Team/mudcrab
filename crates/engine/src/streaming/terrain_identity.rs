use super::*;
use bevy::ecs::{change_detection::Tick, system::SystemChangeTick};
use bevy::render::sync_world::MainEntity;
/// One stock primitive's exact source cell/quadrant, at full detail or an LOD tier.
/// Far tiers still have ONE source cell/quadrant per mesh; never expand by tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerrainPrimitiveIdentity {
    pub worldspace_id: u32,
    pub grid: IVec2,
    pub quadrant: u8,
    pub tier: Option<LodTier>,
}

#[cfg(test)]
impl TerrainPrimitiveIdentity {
    /// Creates a real near-terrain source for extraction tests outside streaming.
    pub(crate) fn spawn_fixture(self, world: &mut World) -> Entity {
        assert!(
            self.tier.is_none(),
            "this fixture creates full-detail terrain"
        );
        world
            .spawn((
                Mesh3d(Handle::default()),
                TerrainCoverage {
                    grid: self.grid,
                    quadrant: self.quadrant,
                    tier: self.tier,
                },
                TerrainWorldspace(self.worldspace_id),
                Visibility::Inherited,
                InheritedVisibility::VISIBLE,
            ))
            .id()
    }
}

type TerrainNode = (
    Option<&'static TerrainCoverage>,
    Option<&'static TerrainWorldspace>,
    Option<&'static lod::LodChunkRoot>,
    Option<&'static ChildOf>,
);

/// Composable facts for a complete ancestry suffix, including incomplete but valid suffixes.
/// Caching only a final identity would lose descendant coverage and marker conflicts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TerrainAncestry {
    coverage: Option<TerrainCoverage>,
    near_world: Option<u32>,
    chunk: Option<shared::lod::ChunkKey>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TerrainNodeFacts {
    coverage: Option<TerrainCoverage>,
    near_world: Option<u32>,
    chunk: Option<shared::lod::ChunkKey>,
    parent: Option<Entity>,
}

impl TerrainAncestry {
    fn with_node(mut self, node: TerrainNodeFacts) -> Option<Self> {
        if let Some(coverage) = node.coverage
            && self.coverage.replace(coverage).is_some()
        {
            return None;
        }
        if let Some(world) = node.near_world
            && self
                .near_world
                .replace(world)
                .is_some_and(|old| old != world)
        {
            return None;
        }
        if let Some(chunk) = node.chunk
            && self.chunk.replace(chunk).is_some()
        {
            return None;
        }
        Some(self)
    }

    fn identity(self) -> Option<TerrainPrimitiveIdentity> {
        let coverage = self.coverage?;
        if coverage.quadrant >= 4 {
            return None;
        }
        let worldspace_id = match (coverage.tier, self.near_world, self.chunk) {
            (None, Some(world), None) => world,
            (Some(tier), None, Some(chunk)) if chunk.tier == tier => chunk.worldspace_id,
            _ => return None,
        };
        Some(TerrainPrimitiveIdentity {
            worldspace_id,
            grid: coverage.grid,
            quadrant: coverage.quadrant,
            tier: coverage.tier,
        })
    }
}

/// One immutable batch only: full generational entity keys, no cross-frame ancestry cache.
#[derive(Default)]
struct TerrainAncestryMemo {
    resolved: bevy::ecs::entity::EntityHashMap<TerrainMemoNode>,
    path: Vec<(Entity, Option<TerrainNodeFacts>)>,
    visiting: bevy::ecs::entity::EntityHashSet,
}

struct TerrainMemoNode {
    summary: Option<TerrainAncestry>,
    terrain: bool,
    facts: Option<TerrainNodeFacts>,
}

impl TerrainAncestryMemo {
    fn resolve(
        &mut self,
        entity: Entity,
        mut facts: impl FnMut(Entity) -> Option<TerrainNodeFacts>,
    ) -> Option<TerrainAncestry> {
        self.path.clear();
        debug_assert!(self.visiting.is_empty());
        let mut at = entity;
        let mut terrain = false;
        let mut summary = loop {
            if let Some(node) = self.resolved.get(&at) {
                terrain = node.terrain;
                break node.summary;
            }
            if !self.visiting.insert(at) {
                // Every node in the cycle reaches every marker on the cycle. Prefix markers
                // must not make an otherwise uncovered cycle look like a terrain dependency.
                terrain = self
                    .path
                    .iter()
                    .position(|(node, _)| *node == at)
                    .is_some_and(|start| {
                        self.path[start..]
                            .iter()
                            .any(|(_, facts)| facts.is_some_and(|facts| facts.coverage.is_some()))
                    });
                break None;
            }
            let node = facts(at);
            // Include missing entities in the path so every visiting marker is drained.
            self.path.push((at, node));
            let Some(node) = node else {
                break None;
            };
            let Some(parent) = node.parent else {
                break Some(TerrainAncestry::default());
            };
            at = parent;
        };
        // Parent summaries are cached before descendant markers are folded in. A duplicate
        // below an otherwise valid ancestor must not invalidate that ancestor's other meshes.
        for &(entity, node) in self.path.iter().rev() {
            let was_visiting = self.visiting.remove(&entity);
            debug_assert!(was_visiting);
            summary = summary.and_then(|parent| node.and_then(|node| parent.with_node(node)));
            terrain |= node.is_some_and(|node| node.coverage.is_some());
            self.resolved.insert(
                entity,
                TerrainMemoNode {
                    summary,
                    terrain,
                    facts: node,
                },
            );
        }
        debug_assert!(self.visiting.is_empty());
        summary
    }
}

/// Exceptional relationships only; ordinary hierarchy edges stay in Bevy's Children.
/// Install the detector before spawning the production hierarchy. The bootstrap mode also
/// checks memo-read ancestry when tracking is installed after an existing hierarchy.
#[derive(Resource, Default)]
pub(super) struct TerrainRelationshipChecks {
    state: std::sync::Mutex<TerrainRelationshipState>,
    bootstrap_hierarchy: bool,
    bootstrap_pending: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub(super) struct TerrainRelationshipStats {
    pub fast_checks: u64,
    pub linear_checks: u64,
    pub recovery_requests: u64,
    pub linear_siblings_examined: u64,
    pub max_linear_siblings: u64,
    #[cfg(test)]
    pub pending_suspects: u64,
    #[cfg(test)]
    pub confirmed_missing: u64,
}

#[derive(Default)]
struct TerrainRelationshipState {
    #[cfg(test)]
    stats: TerrainRelationshipStats,
    missing: bevy::ecs::entity::EntityHashMap<Entity>,
    suspects: bevy::ecs::entity::EntityHashMap<Entity>,
}

impl TerrainRelationshipChecks {
    fn state(&self) -> std::sync::MutexGuard<'_, TerrainRelationshipState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> TerrainRelationshipStats {
        let state = self.state();
        TerrainRelationshipStats {
            pending_suspects: state.suspects.len() as u64,
            confirmed_missing: state.missing.len() as u64,
            ..state.stats
        }
    }

    fn validate(&self, world: &World) {
        let candidates: bevy::ecs::entity::EntityHashSet = {
            let state = self.state();
            state
                .suspects
                .keys()
                .chain(state.missing.keys())
                .copied()
                .collect()
        };
        for child in candidates {
            self.validate_child(world, child);
        }
    }

    fn validate_child(&self, world: &World, child: Entity) {
        // Re-read the current ChildOf as well as Children. WorldAsset writes mapped
        // relationships and backlinks separately with skipped hooks; the observer may
        // see an intermediate state, or a later write may have changed the parent.
        check_relationship_backlink(world, child);
        // Promote under one lock: concurrent read-only consumers must never observe a
        // gap between removing a suspect and recording its confirmed missing backlink.
        let mut state = self.state();
        if let Some(parent) = state.suspects.remove(&child)
            && state.missing.get(&child) != Some(&parent)
        {
            state.missing.insert(child, parent);
            #[cfg(test)]
            {
                state.stats.recovery_requests += 1;
            }
            warn!(
                ?child,
                ?parent,
                reason = "relationship hooks skipped",
                "terrain identity discovery requires full recovery"
            );
        }
    }

    fn has_missing(&self) -> bool {
        !self.state().missing.is_empty()
    }

    pub(super) fn bootstrap(existing_hierarchy: bool) -> Self {
        Self {
            bootstrap_hierarchy: existing_hierarchy,
            bootstrap_pending: std::sync::atomic::AtomicBool::new(existing_hierarchy),
            ..default()
        }
    }
}

/// The deferred event check runs after the ordinary relationship hook's backlink command.
/// WorldAsset reflection can still be mid-copy, so a missing backlink is provisional until
/// validation at the consumer boundary or Last. Only this parent is inspected: append is
/// constant time and the fallback is bounded by siblings, never the world's edge count.
pub(super) fn check_relationship_backlink(world: &World, child: Entity) {
    let parent = world.get::<ChildOf>(child).map(ChildOf::parent);
    #[cfg(test)]
    let mut accounting = (false, 0, 0);
    let linked = parent.is_some_and(|parent| {
        let children = world.get::<Children>(parent);
        let fast = children.is_some_and(|children| children.last() == Some(&child));
        if fast {
            #[cfg(test)]
            {
                accounting = (
                    true,
                    0,
                    children.map_or(0, |children| children.len() as u64),
                );
            }
            return true;
        }
        let position =
            children.and_then(|children| children.iter().position(|sibling| sibling == child));
        #[cfg(test)]
        {
            let siblings = children.map_or(0, |children| children.len() as u64);
            accounting = (
                false,
                position.map_or(siblings, |position| position as u64 + 1),
                siblings,
            );
        }
        position.is_some()
    });
    let Some(checks) = world.get_resource::<TerrainRelationshipChecks>() else {
        return;
    };
    let mut state = checks.state();
    let Some(parent) = parent else {
        state.missing.remove(&child);
        state.suspects.remove(&child);
        return;
    };
    #[cfg(test)]
    {
        let (fast, examined, siblings) = accounting;
        if fast {
            state.stats.fast_checks += 1;
        } else {
            state.stats.linear_checks += 1;
            state.stats.linear_siblings_examined += examined;
            state.stats.max_linear_siblings = state.stats.max_linear_siblings.max(siblings);
        }
    }
    if linked {
        state.missing.remove(&child);
        state.suspects.remove(&child);
    } else {
        state.suspects.insert(child, parent);
    }
}

/// Resolve only event-detected candidates after world spawning and queued relationship work.
/// The identity consumer also invokes this before returning, so no result waits another frame.
/// Last keeps transient candidate storage bounded even when identities have no consumer.
pub(super) fn validate_relationships(world: &World) {
    if let Some(checks) = world.get_resource::<TerrainRelationshipChecks>() {
        checks.validate(world);
    }
}

/// Immutable results and exact invalidation membership survive between invocations. Ancestry
/// facts are always read from the current ECS queries when a dependency changes.
#[derive(Default)]
struct TerrainIdentityCache {
    evaluated: Option<Tick>,
    changed: Vec<MainEntity>,
    recovered: bool,
    entities: Vec<Entity>,
    next_entities: Vec<Entity>,
    identities: std::sync::Arc<HashMap<MainEntity, TerrainPrimitiveIdentity>>,
    structure_epoch: Option<u64>,
    incremental: TerrainIdentityIndex,
    #[cfg(test)]
    rebuilds: usize,
}

/// Only terrain candidates retain dependency paths, including refused candidates. Keeping
/// refusals is necessary: removing a conflicting marker can make a primitive valid again.
#[derive(Default)]
struct TerrainIdentityIndex {
    initialized: bool,
    paths: bevy::ecs::entity::EntityHashMap<Vec<Entity>>,
    dependents: bevy::ecs::entity::EntityHashMap<bevy::ecs::entity::EntityHashSet>,
    facts: bevy::ecs::entity::EntityHashMap<(Option<TerrainNodeFacts>, bool)>,
    journal_lost: bool,
    cursor: bevy::ecs::message::MessageCursor<identity_changes::TerrainNodeChanged>,
    #[cfg(test)]
    resolved: usize,
}

impl TerrainIdentityIndex {
    fn forget(&mut self, mesh: Entity) {
        if let Some(path) = self.paths.remove(&mesh) {
            for node in path {
                if let Some(meshes) = self.dependents.get_mut(&node) {
                    meshes.remove(&mesh);
                    if meshes.is_empty() {
                        self.dependents.remove(&node);
                        self.facts.remove(&node);
                    }
                }
            }
        }
    }
}

/// Consume every removal cursor, including messages lost while extraction was skipped.
fn terrain_removals<T: Component>(removed: &mut RemovedComponents<T>) -> bool {
    let unread = !removed.is_empty();
    let missed = removed
        .messages()
        .is_some_and(|messages| removed.reader().missed_messages(messages) != 0);
    removed.clear();
    unread || missed
}

type TerrainIdentityChanges = Or<(
    Added<Mesh3d>,
    Changed<ChildOf>,
    Changed<TerrainCoverage>,
    Changed<TerrainWorldspace>,
    Changed<lod::LodChunkRoot>,
)>;

type TerrainInputMutations = Or<(
    Changed<TerrainCoverage>,
    Changed<TerrainWorldspace>,
    Changed<lod::LodChunkRoot>,
)>;

/// Read-only access to the quiescent source world during render extraction.
/// Backlink recovery deliberately bypasses arbitrary default query disablers:
/// a filtered relationship query could hide the malformed edge being checked.
/// The ordinary identity queries retain their original component/filter domain.
/// This broad read conflicts with source-world writers; extraction already runs
/// while main-world simulation is stopped, and no simulation writes are deferred
/// or weakened to make this parameter run concurrently.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct TerrainIdentityAccess<'w, 's> {
    world: &'w World,
    structure: Option<Res<'w, identity_changes::StructureEpoch>>,
    filters: Res<'w, bevy::ecs::entity_disabling::DefaultQueryFilters>,
    meshes: Query<'w, 's, Entity, With<Mesh3d>>,
    nodes: Query<'w, 's, TerrainNode>,
    // Use identical query data for exact default-filter membership. Optional component reads
    // do not opt into disabling components; Allow would incorrectly broaden this domain.
    node_entities: Query<'w, 's, (Entity, TerrainNode)>,
    changed: Query<'w, 's, Entity, TerrainIdentityChanges>,
    tracking: Option<Res<'w, identity_changes::TerrainTracking>>,
    events: Option<Res<'w, Messages<identity_changes::TerrainNodeChanged>>>,
    children: Query<'w, 's, &'static Children>,
    relationships: Option<Res<'w, TerrainRelationshipChecks>>,
    terrain_mutations: Query<'w, 's, Entity, TerrainInputMutations>,
    removed_meshes: RemovedComponents<'w, 's, Mesh3d>,
    removed_parents: RemovedComponents<'w, 's, ChildOf>,
    removed_coverage: RemovedComponents<'w, 's, TerrainCoverage>,
    removed_worlds: RemovedComponents<'w, 's, TerrainWorldspace>,
    removed_chunks: RemovedComponents<'w, 's, lod::LodChunkRoot>,
    ticks: SystemChangeTick,
    cache: Local<'s, TerrainIdentityCache>,
}

impl TerrainIdentityAccess<'_, '_> {
    #[cfg(test)]
    pub(crate) fn identities(&mut self) -> HashMap<Entity, TerrainPrimitiveIdentity> {
        self.identities_shared()
            .iter()
            .map(|(entity, identity)| (entity.id(), *identity))
            .collect()
    }

    /// Snapshot consumers call this unconditionally on each parameter fetch. Change
    /// filters advance on fetch, so a fetch-only gap also forces a rebuild. This uses Bevy's
    /// ordinary change-tick window, not an unlimited-history proof across arbitrary tick wraps.
    /// The cache always retains its own Arc clone. Pointer equality indicates freshness only
    /// while the consumer retains its previous snapshot across the next fetch; without an
    /// external holder, Arc::make_mut may update the allocation in place.
    pub(crate) fn identities_shared(
        &mut self,
    ) -> std::sync::Arc<HashMap<MainEntity, TerrainPrimitiveIdentity>> {
        self.identities_snapshot(false)
    }

    fn identities_snapshot(
        &mut self,
        track_changes: bool,
    ) -> std::sync::Arc<HashMap<MainEntity, TerrainPrimitiveIdentity>> {
        self.cache.changed.clear();
        self.cache.recovered = false;
        validate_relationships(self.world);
        if self.events.is_some() && self.tracking.is_some() {
            self.update_incremental(track_changes);
            self.audit_identities();
            return std::sync::Arc::clone(&self.cache.identities);
        }
        self.cache.incremental.initialized = false;
        let epoch = self
            .structure
            .as_ref()
            .and_then(|epoch| epoch.covered_version(&self.filters));
        let scan_membership = epoch.is_none()
            || epoch != self.cache.structure_epoch
            || self.filters.is_changed()
            || self.cache.evaluated != Some(self.ticks.last_run());
        if scan_membership {
            self.cache.next_entities.clear();
            self.cache
                .next_entities
                .extend(self.node_entities.iter().map(|(entity, _)| entity));
        }
        let changed = self.changed.iter().next().is_some();
        let mut dirty = self.cache.evaluated != Some(self.ticks.last_run())
            || (scan_membership && self.cache.entities != self.cache.next_entities)
            || changed;
        // Do not short-circuit these readers: every invocation advances all five streams.
        dirty |= terrain_removals(&mut self.removed_meshes);
        dirty |= terrain_removals(&mut self.removed_parents);
        dirty |= terrain_removals(&mut self.removed_coverage);
        dirty |= terrain_removals(&mut self.removed_worlds);
        dirty |= terrain_removals(&mut self.removed_chunks);
        if dirty {
            let identities = self.uncached_identities();
            self.cache.recovered = true;
            if track_changes {
                self.cache.changed = self
                    .cache
                    .identities
                    .keys()
                    .filter(|entity| !identities.contains_key(&entity.id()))
                    .copied()
                    .chain(identities.iter().filter_map(|(entity, identity)| {
                        let main = MainEntity::from(*entity);
                        (self.cache.identities.get(&main) != Some(identity)).then_some(main)
                    }))
                    .collect();
            }
            let cache = &mut *self.cache;
            cache.identities = std::sync::Arc::new(
                identities
                    .into_iter()
                    .map(|(entity, identity)| (entity.into(), identity))
                    .collect(),
            );
            if scan_membership {
                std::mem::swap(&mut cache.entities, &mut cache.next_entities);
            }
            #[cfg(test)]
            {
                cache.rebuilds += 1;
            }
        }
        self.cache.evaluated = Some(self.ticks.this_run());
        self.cache.structure_epoch = epoch;
        self.audit_identities();
        std::sync::Arc::clone(&self.cache.identities)
    }

    fn facts(&self, entity: Entity) -> Option<TerrainNodeFacts> {
        let (coverage, world, chunk, parent) = self.nodes.get(entity).ok()?;
        Some(TerrainNodeFacts {
            coverage: coverage.copied(),
            near_world: world.map(|world| world.0),
            chunk: chunk.map(lod::LodChunkRoot::terrain_key),
            parent: parent.map(ChildOf::parent),
        })
    }

    #[cfg(test)]
    fn compare_identities(&self) -> Result<(), String> {
        let expected = self.uncached_identities();
        if expected.len() == self.cache.identities.len()
            && expected.iter().all(|(entity, identity)| {
                self.cache.identities.get(&MainEntity::from(*entity)) == Some(identity)
            })
        {
            Ok(())
        } else {
            Err(format!(
                "incremental terrain identities differ from a full rebuild: incremental={}, full={}",
                self.cache.identities.len(),
                expected.len()
            ))
        }
    }

    fn audit_identities(&self) {
        #[cfg(test)]
        debug_assert_eq!(self.compare_identities(), Ok(()));
    }

    fn seed_backlinks(&mut self, nodes: impl IntoIterator<Item = (Entity, TerrainNodeFacts)>) {
        let Some(checks) = self.relationships.as_ref() else {
            return;
        };
        for (child, facts) in nodes {
            if facts.parent.is_some() {
                checks.validate_child(self.world, child);
            }
        }
    }

    /// Trace the complete current path even after a semantic refusal, so all ancestors that
    /// could repair it remain dependencies. Query exclusions and cycles still refuse identity.
    fn trace(&self, mesh: Entity) -> (Option<TerrainPrimitiveIdentity>, Vec<Entity>, bool) {
        let mut path = Vec::new();
        let mut seen = bevy::ecs::entity::EntityHashSet::default();
        let mut summary = Some(TerrainAncestry::default());
        let mut terrain = false;
        let mut at = mesh;
        loop {
            if !seen.insert(at) {
                return (None, path, terrain);
            }
            path.push(at);
            let Ok((coverage, world, chunk, parent)) = self.nodes.get(at) else {
                return (None, path, terrain);
            };
            terrain |= coverage.is_some();
            summary = summary.and_then(|summary| {
                summary.with_node(TerrainNodeFacts {
                    coverage: coverage.copied(),
                    near_world: world.map(|world| world.0),
                    chunk: chunk.map(lod::LodChunkRoot::terrain_key),
                    parent: None,
                })
            });
            match parent {
                Some(parent) => at = parent.parent(),
                None => return (summary.and_then(TerrainAncestry::identity), path, terrain),
            }
        }
    }

    fn update_incremental(&mut self, track_changes: bool) {
        let Some(events) = self.events.as_ref() else {
            return;
        };
        let missed = self.cache.incremental.cursor.missed_messages(events) != 0;
        let mut changed: bevy::ecs::entity::EntityHashSet = self
            .cache
            .incremental
            .cursor
            .read(events)
            .map(|event| event.0)
            .collect();
        changed.extend(self.terrain_mutations.iter());
        let covered = self
            .tracking
            .as_ref()
            .is_some_and(|tracking| tracking.covered(&self.filters));
        if self.cache.incremental.initialized && missed && !self.cache.incremental.journal_lost {
            warn!(
                "terrain journal messages expired; rebuilding terrain identities from current ECS state"
            );
        }
        self.cache.incremental.journal_lost = missed;
        if !covered {
            warn_once!(
                "terrain journal does not cover a late default disabler; terrain identities use full query-domain recovery"
            );
        }
        let full = !self.cache.incremental.initialized
            || missed
            || self.cache.evaluated != Some(self.ticks.last_run())
            || self.filters.is_changed()
            || !covered
            || self
                .relationships
                .as_ref()
                .is_some_and(|checks| checks.has_missing());
        self.cache.recovered = full;
        let mut dirty = bevy::ecs::entity::EntityHashSet::default();
        let mut rebuilt = None;
        if full {
            let mut memo = TerrainAncestryMemo::default();
            let mut identities = HashMap::new();
            for mesh in &self.meshes {
                if let Some(identity) = memo
                    .resolve(mesh, |at| self.facts(at))
                    .and_then(TerrainAncestry::identity)
                {
                    identities.insert(mesh, identity);
                }
            }
            if self.relationships.as_ref().is_some_and(|checks| {
                checks
                    .bootstrap_pending
                    .swap(false, std::sync::atomic::Ordering::Relaxed)
            }) {
                let nodes = memo
                    .resolved
                    .iter()
                    .filter_map(|(&entity, node)| node.facts.map(|facts| (entity, facts)))
                    .collect::<Vec<_>>();
                self.seed_backlinks(nodes);
            }
            dirty.extend(self.cache.incremental.paths.keys().copied());
            dirty.extend(memo.resolved.iter().filter_map(|(&entity, node)| {
                (node.terrain && self.meshes.contains(entity)).then_some(entity)
            }));
            self.cache.incremental.paths.clear();
            self.cache.incremental.dependents.clear();
            self.cache.incremental.facts.clear();
            rebuilt = Some(identities);
        } else {
            let mut discovered = bevy::ecs::entity::EntityHashSet::default();
            for node in changed {
                let current = self.facts(node);
                let current = (current, self.meshes.contains(node));
                let known_same = self
                    .cache
                    .incremental
                    .facts
                    .get(&node)
                    .is_some_and(|old| *old == current);
                if known_same {
                    continue;
                }
                if let Some(dependents) = self.cache.incremental.dependents.get(&node) {
                    dirty.extend(dependents);
                }
                if self.meshes.contains(node) {
                    dirty.insert(node);
                }
                // Children owns ordinary discovery. Hook-skipped backlinks are detected by
                // event-local checks and explicitly switch to the memoized full recovery.
                if !discovered.contains(&node) && self.trace(node).2 {
                    let mut stack = vec![node];
                    while let Some(at) = stack.pop() {
                        if !discovered.insert(at) {
                            continue;
                        }
                        if self.meshes.contains(at) {
                            dirty.insert(at);
                        }
                        if let Ok(children) = self.children.get(at) {
                            stack.extend(children.iter());
                        }
                    }
                }
            }
        }
        let mut updates = Vec::new();
        for mesh in dirty {
            let (identity, path, terrain) = if self.meshes.contains(mesh) {
                let traced = self.trace(mesh);
                if let Some(identities) = &rebuilt {
                    (identities.get(&mesh).copied(), traced.1, traced.2)
                } else {
                    traced
                }
            } else {
                (None, Vec::new(), false)
            };
            if self
                .relationships
                .as_ref()
                .is_some_and(|checks| checks.bootstrap_hierarchy)
                && !full
            {
                let nodes = path
                    .iter()
                    .filter_map(|&node| self.facts(node).map(|facts| (node, facts)))
                    .collect::<Vec<_>>();
                self.seed_backlinks(nodes);
            }
            self.cache.incremental.forget(mesh);
            if terrain {
                for &node in &path {
                    let facts = (self.facts(node), self.meshes.contains(node));
                    self.cache
                        .incremental
                        .dependents
                        .entry(node)
                        .or_default()
                        .insert(mesh);
                    self.cache.incremental.facts.insert(node, facts);
                }
                self.cache.incremental.paths.insert(mesh, path);
            }
            if self.cache.identities.get(&MainEntity::from(mesh)).copied() != identity {
                updates.push((mesh, identity));
            }
            #[cfg(test)]
            {
                self.cache.incremental.resolved += 1;
            }
        }
        if let Some(identities) = rebuilt {
            // Include removals even for formerly valid entities that no longer have a terrain path.
            for entity in self.cache.identities.keys() {
                if !identities.contains_key(&entity.id()) {
                    updates.push((entity.id(), None));
                }
            }
            for (entity, identity) in identities {
                if self
                    .cache
                    .identities
                    .get(&MainEntity::from(entity))
                    .copied()
                    != Some(identity)
                {
                    updates.push((entity, Some(identity)));
                }
            }
        }
        // Unrelated object streaming preserves the exact snapshot. Copy-on-write happens once
        // only when a terrain identity changes while an earlier snapshot is still borrowed.
        if !updates.is_empty() {
            if track_changes {
                self.cache
                    .changed
                    .extend(updates.iter().map(|(entity, _)| MainEntity::from(*entity)));
            }
            let identities = std::sync::Arc::make_mut(&mut self.cache.identities);
            for (mesh, identity) in updates {
                match identity {
                    Some(identity) => {
                        identities.insert(mesh.into(), identity);
                    }
                    None => {
                        identities.remove(&MainEntity::from(mesh));
                    }
                }
            }
        }
        // Advance the legacy removal readers too, so switching query-domain recovery modes
        // cannot replay old removals or conceal an expired stream.
        terrain_removals(&mut self.removed_meshes);
        terrain_removals(&mut self.removed_parents);
        terrain_removals(&mut self.removed_coverage);
        terrain_removals(&mut self.removed_worlds);
        terrain_removals(&mut self.removed_chunks);
        self.cache.evaluated = Some(self.ticks.this_run());
        self.cache.incremental.initialized = true;
        #[cfg(test)]
        {
            self.cache.rebuilds += usize::from(full);
        }
    }

    /// Resolve the same actual Mesh3d domain as identity(), sharing exact ancestry suffixes
    /// only during this immutable invocation. Missing facts and cycles remain refusals.
    fn uncached_identities(&self) -> HashMap<Entity, TerrainPrimitiveIdentity> {
        let mut memo = TerrainAncestryMemo::default();
        let mut identities = HashMap::new();
        for entity in &self.meshes {
            let Some(summary) = memo.resolve(entity, |at| {
                let (coverage, world, chunk, parent) = self.nodes.get(at).ok()?;
                Some(TerrainNodeFacts {
                    coverage: coverage.copied(),
                    near_world: world.map(|world| world.0),
                    chunk: chunk.map(lod::LodChunkRoot::terrain_key),
                    parent: parent.map(ChildOf::parent),
                })
            }) else {
                continue;
            };
            if let Some(identity) = summary.identity() {
                identities.insert(entity, identity);
            }
        }
        identities
    }

    /// Resolve actual Mesh3d -> optional quadrant parent -> owning world root.
    /// Refuse malformed/ambiguous ancestry instead of inferring from AABBs,
    /// active worldspace, material, or the LOD chunk's coarse rectangle.
    #[cfg(test)]
    pub(crate) fn identity(&self, entity: Entity) -> Option<TerrainPrimitiveIdentity> {
        self.meshes.get(entity).ok()?;
        let mut at = entity;
        let mut seen = std::collections::HashSet::new();
        let mut coverage = None;
        let mut near_world = None;
        let mut chunk = None;
        loop {
            if !seen.insert(at) {
                return None;
            }
            let (candidate, world, root, parent) = self.nodes.get(at).ok()?;
            if let Some(candidate) = candidate
                && coverage.replace(*candidate).is_some()
            {
                return None;
            }
            if let Some(world) = world
                && near_world
                    .replace(world.0)
                    .is_some_and(|old| old != world.0)
            {
                return None;
            }
            if let Some(root) = root
                && chunk.replace(root.terrain_key()).is_some()
            {
                return None;
            }
            match parent {
                Some(parent) => at = parent.parent(),
                None => break,
            }
        }
        let coverage: TerrainCoverage = coverage?;
        if coverage.quadrant >= 4 {
            return None;
        }
        let worldspace_id = match (coverage.tier, near_world, chunk) {
            (None, Some(world), None) => world,
            (Some(tier), None, Some(chunk)) if chunk.tier == tier => chunk.worldspace_id,
            _ => return None,
        };
        Some(TerrainPrimitiveIdentity {
            worldspace_id,
            grid: coverage.grid,
            quadrant: coverage.quadrant,
            tier: coverage.tier,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::{
        entity_disabling::Disabled, relationship::RelationshipHookMode, system::SystemState,
    };

    #[test]
    fn unrelated_visibility_under_a_broad_parent_does_not_check_relationships() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let owner = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        mesh(&mut world, owner);
        let parent = world.spawn_empty().id();
        let children: Vec<_> = (0..4096)
            .map(|_| {
                world
                    .spawn((
                        Mesh3d(Handle::default()),
                        Visibility::Inherited,
                        ChildOf(parent),
                    ))
                    .id()
            })
            .collect();
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let initial = state.get(&world).unwrap().identities_shared();
        let checks = world.resource::<TerrainRelationshipChecks>();
        let before = (
            checks.snapshot().fast_checks,
            checks.snapshot().linear_checks,
            checks.snapshot().recovery_requests,
        );
        world.entity_mut(children[0]).insert(Visibility::Hidden);
        let current = state.get(&world).unwrap().identities_shared();
        assert!(std::sync::Arc::ptr_eq(&initial, &current));
        let checks = world.resource::<TerrainRelationshipChecks>();
        assert_eq!(
            (
                checks.snapshot().fast_checks,
                checks.snapshot().linear_checks,
                checks.snapshot().recovery_requests
            ),
            before
        );
        assert_eq!(checks.snapshot().recovery_requests, 0);
    }

    #[test]
    fn reflected_world_asset_instantiation_does_not_report_transient_missing_backlinks() {
        let registry = bevy::ecs::reflect::AppTypeRegistry::default();
        {
            let mut types = registry.write();
            types.register::<ChildOf>();
            types.register::<Children>();
        }
        let mut template = World::new();
        let root = template.spawn_empty().id();
        let neutral = template.spawn(ChildOf(root)).id();
        let primitive = template.spawn(ChildOf(neutral)).id();
        let asset = bevy::world_serialization::WorldAsset::new(template);
        let mut world = World::new();
        identity_changes::install(&mut world);
        let mut entities = bevy::ecs::entity::EntityHashMap::default();
        // The real serializer copies mapped ChildOf and Children separately with skipped
        // hooks. The intermediate missing backlink is not a completed-world exception.
        asset
            .write_to_world_with(&mut world, &mut entities, &registry)
            .unwrap();
        let root = entities[&root];
        let neutral = entities[&neutral];
        let primitive = entities[&primitive];
        assert_eq!(world.get::<ChildOf>(primitive).unwrap().parent(), neutral);
        assert!(world.get::<Children>(neutral).unwrap().contains(&primitive));
        world
            .entity_mut(root)
            .insert((coverage(1, None), TerrainWorldspace(7)));
        world
            .entity_mut(primitive)
            .insert(Mesh3d(Handle::default()));
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let mut access = state.get(&world).unwrap();
        let expected = access.uncached_identities();
        let actual = access.identities();
        assert_eq!(actual, expected);
        assert_eq!(actual[&primitive].quadrant, 1);
        assert_eq!(
            world
                .resource::<TerrainRelationshipChecks>()
                .snapshot()
                .recovery_requests,
            0
        );
    }

    #[test]
    fn last_clears_transient_world_asset_candidates_without_an_identity_consumer() {
        let registry = bevy::ecs::reflect::AppTypeRegistry::default();
        {
            let mut types = registry.write();
            types.register::<ChildOf>();
            types.register::<Children>();
        }
        let mut template = World::new();
        let root = template.spawn_empty().id();
        let neutral = template.spawn(ChildOf(root)).id();
        template.spawn(ChildOf(neutral));
        let asset = bevy::world_serialization::WorldAsset::new(template);
        let mut app = App::new();
        identity_changes::install(app.world_mut());
        app.add_systems(Last, validate_relationships);
        asset
            .write_to_world_with(app.world_mut(), &mut default(), &registry)
            .unwrap();
        assert!(
            app.world()
                .resource::<TerrainRelationshipChecks>()
                .snapshot()
                .pending_suspects
                > 0
        );
        app.update();
        let stats = app
            .world()
            .resource::<TerrainRelationshipChecks>()
            .snapshot();
        assert_eq!(stats.pending_suspects, 0);
        assert_eq!(stats.confirmed_missing, 0);
        assert_eq!(stats.recovery_requests, 0);
    }

    #[test]
    fn ordinary_streaming_never_requests_hook_skip_recovery() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let owner = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        for _ in 0..32 {
            let primitive = mesh(&mut world, owner);
            assert!(
                state
                    .get(&world)
                    .unwrap()
                    .identities()
                    .contains_key(&primitive)
            );
            world.despawn(primitive);
            assert!(state.get(&world).unwrap().identities().is_empty());
        }
        let checks = world.resource::<TerrainRelationshipChecks>();
        assert_eq!(checks.snapshot().recovery_requests, 0);
        assert_eq!(checks.snapshot().linear_checks, 0);
        assert_eq!(checks.snapshot().fast_checks, 32);
    }

    #[test]
    fn late_tracking_install_detects_uncovered_skipped_hierarchy_during_bootstrap() {
        let mut world = World::new();
        let root = world.spawn_empty().id();
        let neutral = world.spawn_empty().id();
        let primitive = world.spawn(Mesh3d(Handle::default())).id();
        world
            .entity_mut(neutral)
            .insert_with_relationship_hook_mode(ChildOf(root), RelationshipHookMode::Skip);
        world
            .entity_mut(primitive)
            .insert_with_relationship_hook_mode(ChildOf(neutral), RelationshipHookMode::Skip);
        identity_changes::install(&mut world);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert!(state.get(&world).unwrap().identities().is_empty());
        assert_eq!(
            world
                .resource::<TerrainRelationshipChecks>()
                .snapshot()
                .recovery_requests,
            2
        );
        world
            .entity_mut(root)
            .insert((coverage(1, None), TerrainWorldspace(7)));
        assert_eq!(
            state.get(&world).unwrap().identities()[&primitive].quadrant,
            1
        );
    }

    #[test]
    fn late_tracking_install_keeps_filtered_bootstrap_edges_exact() {
        let mut world = World::new();
        let root = world.spawn(Disabled).id();
        let primitive = world.spawn(Mesh3d(Handle::default())).id();
        world
            .entity_mut(primitive)
            .insert_with_relationship_hook_mode(ChildOf(root), RelationshipHookMode::Skip);
        identity_changes::install(&mut world);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert!(state.get(&world).unwrap().identities().is_empty());
        assert_eq!(
            world
                .resource::<TerrainRelationshipChecks>()
                .state()
                .missing[&primitive],
            root
        );
        world.entity_mut(root).remove::<Disabled>();
        world
            .entity_mut(root)
            .insert((coverage(1, None), TerrainWorldspace(7)));
        assert_eq!(
            state.get(&world).unwrap().identities()[&primitive].quadrant,
            1
        );
    }

    #[test]
    fn unrelated_mesh_streaming_reuses_terrain_snapshot() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let owner = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        mesh(&mut world, owner);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let initial = state.get(&world).unwrap().identities_shared();
        for _ in 0..32 {
            let object = world.spawn(Mesh3d(Handle::default())).id();
            let current = state.get(&world).unwrap().identities_shared();
            assert!(
                std::sync::Arc::ptr_eq(&initial, &current),
                "object arrivals must not rebuild or copy the terrain snapshot"
            );
            world.despawn(object);
            let current = state.get(&world).unwrap().identities_shared();
            assert!(
                std::sync::Arc::ptr_eq(&initial, &current),
                "object removals must not rebuild or copy the terrain snapshot"
            );
        }
    }

    #[test]
    fn shared_ancestor_semantic_noise_does_not_resolve_terrain_again() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let root = chunk(&mut world, 91);
        let owner = world
            .spawn((coverage(0, Some(LodTier::Tier16)), ChildOf(root)))
            .id();
        for _ in 0..32 {
            mesh(&mut world, owner);
        }
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let (initial, resolved) = {
            let mut access = state.get(&world).unwrap();
            (
                access.identities_shared(),
                access.cache.incremental.resolved,
            )
        };
        for noise in 0..5 {
            match noise {
                0 => {
                    world.entity_mut(owner).insert(Visibility::Hidden);
                }
                1 => {
                    world.entity_mut(owner).insert(TerrainPatch);
                }
                2 => {
                    world.entity_mut(owner).insert(TerrainSurfaceReady);
                }
                3 => {
                    let children = std::mem::take(&mut *world.get_mut::<Children>(owner).unwrap());
                    world
                        .entity_mut(owner)
                        .insert_with_relationship_hook_mode(children, RelationshipHookMode::Skip);
                }
                _ => {
                    world
                        .get_mut::<lod::LodChunkRoot>(root)
                        .unwrap()
                        .set_changed();
                }
            }
            let mut access = state.get(&world).unwrap();
            let current = access.identities_shared();
            assert!(std::sync::Arc::ptr_eq(&initial, &current));
            assert_eq!(access.cache.incremental.resolved, resolved, "noise={noise}");
        }
    }

    #[test]
    fn shared_snapshot_refreshes_when_cache_is_its_only_arc_holder() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let owner = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        let primitive = mesh(&mut world, owner);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        drop(state.get(&world).unwrap().identities_shared());
        assert_eq!(
            std::sync::Arc::strong_count(&state.get(&world).unwrap().cache.identities),
            1
        );
        world.get_mut::<TerrainCoverage>(owner).unwrap().quadrant = 2;
        let previous = state.get(&world).unwrap().identities_shared();
        assert_eq!(previous[&MainEntity::from(primitive)].quadrant, 2);
        world.get_mut::<TerrainCoverage>(owner).unwrap().quadrant = 3;
        let current = state.get(&world).unwrap().identities_shared();
        assert!(!std::sync::Arc::ptr_eq(&previous, &current));
        assert_eq!(previous[&MainEntity::from(primitive)].quadrant, 2);
        assert_eq!(current[&MainEntity::from(primitive)].quadrant, 3);
    }

    #[test]
    fn incremental_identities_match_full_rebuild_after_randomized_mutations() {
        // Deterministic seeds make every failing transition reproducible without a new dependency.
        for seed in [1u64, 0x1234_5678_9abc_def0, 0xffff_ffff_ffff_ffff, 73] {
            for hook_skip in [false, true] {
                let mut random = seed;
                let mut next = || {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    random as usize
                };
                let mut world = World::new();
                world.register_disabling_component::<CustomDisabled>();
                identity_changes::install(&mut world);
                let roots = [
                    world.spawn(TerrainWorldspace(7)).id(),
                    chunk(&mut world, 91),
                ];
                let mut nodes: Vec<_> = (0..24).map(|_| world.spawn_empty().id()).collect();
                for (i, &node) in nodes.iter().enumerate() {
                    world.entity_mut(node).insert((
                        Mesh3d(Handle::default()),
                        ChildOf(roots[i % 2]),
                        coverage((i % 4) as u8, (i % 2 == 1).then_some(LodTier::Tier16)),
                    ));
                }
                let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
                assert_eq!(state.get(&world).unwrap().identities().len(), 24);
                for step in 0..512 {
                    for _ in 0..(next() % 5 + 1) {
                        let i = next() % nodes.len();
                        let node = nodes[i];
                        if world.get_entity(node).is_err() {
                            nodes[i] = world.spawn_empty().id();
                            continue;
                        }
                        match next() % 20 {
                            0 => {
                                world.entity_mut(node).insert(Mesh3d(Handle::default()));
                            }
                            1 => {
                                world.entity_mut(node).remove::<Mesh3d>();
                            }
                            2 => {
                                world.entity_mut(node).insert(coverage(
                                    (next() % 6) as u8,
                                    (next() % 2 == 0).then_some(LodTier::Tier16),
                                ));
                            }
                            3 => {
                                world.entity_mut(node).remove::<TerrainCoverage>();
                            }
                            4 => {
                                // Parents have lower indices, so ordinary relationship hooks never see a cycle.
                                let parent = if i == 0 || next() % 2 == 0 {
                                    roots[next() % 2]
                                } else {
                                    nodes[next() % i]
                                };
                                if world.get_entity(parent).is_ok() {
                                    world.entity_mut(node).insert(ChildOf(parent));
                                }
                            }
                            5 => {
                                world.entity_mut(node).remove::<ChildOf>();
                            }
                            6 => {
                                world
                                    .entity_mut(node)
                                    .insert(TerrainWorldspace((next() % 3 + 7) as u32));
                            }
                            7 => {
                                world.entity_mut(node).remove::<TerrainWorldspace>();
                            }
                            8 => {
                                if world.get::<CustomDisabled>(node).is_some() {
                                    world.entity_mut(node).remove::<CustomDisabled>();
                                } else {
                                    world.entity_mut(node).insert(CustomDisabled);
                                }
                            }
                            9 => {
                                if let Some(mut value) = world.get_mut::<TerrainCoverage>(node) {
                                    value.grid.x = (next() % 100) as i32 - 50;
                                    value.quadrant = (next() % 6) as u8;
                                }
                            }
                            10 => {
                                if let Some(mut value) = world.get_mut::<TerrainWorldspace>(node) {
                                    value.0 = (next() % 3 + 7) as u32;
                                }
                            }
                            11 => {
                                if world.get::<Disabled>(node).is_some() {
                                    world.entity_mut(node).remove::<Disabled>();
                                } else {
                                    world.entity_mut(node).insert(Disabled);
                                }
                            }
                            12 => {
                                let donor = chunk(&mut world, (next() % 3 + 90) as u32);
                                let marker =
                                    world.entity_mut(donor).take::<lod::LodChunkRoot>().unwrap();
                                world.entity_mut(node).insert(marker);
                            }
                            13 => {
                                world.entity_mut(node).remove::<lod::LodChunkRoot>();
                            }
                            14 => {
                                if let Some(mut marker) = world.get_mut::<lod::LodChunkRoot>(node) {
                                    marker.set_changed();
                                }
                            }
                            15 => {
                                let parent = roots[next() % 2];
                                if hook_skip {
                                    world.entity_mut(node).insert_with_relationship_hook_mode(
                                        ChildOf(parent),
                                        RelationshipHookMode::Skip,
                                    );
                                } else {
                                    world.entity_mut(node).insert(ChildOf(parent));
                                }
                            }
                            16 => {
                                world.entity_mut(roots[0]).insert(Visibility::Hidden);
                            }
                            17 => {
                                if let Some(mut children) = world.get_mut::<Children>(roots[0]) {
                                    let value = std::mem::take(&mut *children);
                                    world
                                        .entity_mut(roots[0])
                                        .insert_with_relationship_hook_mode(
                                            value,
                                            RelationshipHookMode::Skip,
                                        );
                                }
                            }
                            18 => {
                                let donor = chunk(&mut world, (next() % 3 + 90) as u32);
                                let marker =
                                    world.entity_mut(donor).take::<lod::LodChunkRoot>().unwrap();
                                *world.get_mut::<lod::LodChunkRoot>(roots[1]).unwrap() = marker;
                            }
                            _ => {
                                world.despawn(node);
                                nodes[i] = world.spawn_empty().id();
                            }
                        }
                    }
                    {
                        let mut access = state.get(&world).unwrap();
                        let expected = access.uncached_identities();
                        let actual = access.identities();
                        assert_eq!(
                            actual, expected,
                            "seed={seed:#x} hook_skip={hook_skip} step={step}"
                        );
                    }
                    if step % 17 == 0 {
                        // Rotate consumed messages as the application's frame schedule does.
                        world
                            .resource_mut::<Messages<identity_changes::TerrainNodeChanged>>()
                            .update();
                        world
                            .resource_mut::<Messages<identity_changes::TerrainNodeChanged>>()
                            .update();
                    }
                }
                if !hook_skip {
                    assert_eq!(
                        world
                            .resource::<TerrainRelationshipChecks>()
                            .snapshot()
                            .recovery_requests,
                        0
                    );
                }
            }
        }
    }

    #[test]
    fn expired_journal_and_fetch_gaps_recover_exact_current_identities() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let owner = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        let primitive = mesh(&mut world, owner);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert_eq!(
            state.get(&world).unwrap().identities()[&primitive].worldspace_id,
            7
        );
        world.entity_mut(owner).remove::<TerrainWorldspace>();
        world
            .resource_mut::<Messages<identity_changes::TerrainNodeChanged>>()
            .update();
        world
            .resource_mut::<Messages<identity_changes::TerrainNodeChanged>>()
            .update();
        {
            let mut access = state.get(&world).unwrap();
            assert!(
                access
                    .cache
                    .incremental
                    .cursor
                    .missed_messages(access.events.as_ref().unwrap())
                    > 0
            );
            let rebuilds = access.cache.rebuilds;
            assert!(access.identities().is_empty());
            assert_eq!(access.cache.rebuilds, rebuilds + 1);
        }
        world.entity_mut(owner).insert(TerrainWorldspace(8));
        {
            let _fetch_only = state.get(&world).unwrap();
        }
        let mut access = state.get(&world).unwrap();
        let rebuilds = access.cache.rebuilds;
        assert_eq!(access.identities()[&primitive].worldspace_id, 8);
        assert_eq!(access.cache.rebuilds, rebuilds + 1);
    }

    #[test]
    fn incremental_index_preserves_refused_paths_and_empty_root_despawn() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let root = world.spawn_empty().id();
        let primitive = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(0, None),
                TerrainWorldspace(7),
            ))
            .id();
        world
            .entity_mut(primitive)
            .insert_with_relationship_hook_mode(ChildOf(root), RelationshipHookMode::Skip);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert!(
            state
                .get(&world)
                .unwrap()
                .identities()
                .contains_key(&primitive)
        );
        world.despawn(root);
        assert!(state.get(&world).unwrap().identities().is_empty());
        let owner = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        world.entity_mut(primitive).insert(ChildOf(owner));
        assert!(state.get(&world).unwrap().identities().is_empty());
        world.entity_mut(primitive).remove::<TerrainCoverage>();
        assert!(
            state
                .get(&world)
                .unwrap()
                .identities()
                .contains_key(&primitive)
        );
        let mut access = state.get(&world).unwrap();
        let resolved = access.cache.incremental.resolved;
        access.identities();
        assert_eq!(
            access.cache.incremental.resolved, resolved,
            "unchanged terrain must not resolve ancestry again"
        );
    }

    #[test]
    fn replacing_children_with_skipped_hooks_preserves_uncovered_discovery() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let root = world.spawn_empty().id();
        let neutral = world.spawn(ChildOf(root)).id();
        let primitive = mesh(&mut world, neutral);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert!(state.get(&world).unwrap().identities().is_empty());
        let before = world
            .resource::<TerrainRelationshipChecks>()
            .snapshot()
            .recovery_requests;
        world
            .entity_mut(root)
            .insert_with_relationship_hook_mode(Children::default(), RelationshipHookMode::Skip);
        assert_eq!(world.get::<ChildOf>(neutral).unwrap().parent(), root);
        world
            .entity_mut(root)
            .insert((coverage(1, None), TerrainWorldspace(7)));
        let mut access = state.get(&world).unwrap();
        let expected = access.uncached_identities();
        let actual = access.identities();
        assert_eq!(actual, expected);
        assert_eq!(actual[&primitive].quadrant, 1);
        assert!(
            world
                .resource::<TerrainRelationshipChecks>()
                .snapshot()
                .recovery_requests
                > before
        );
    }

    #[test]
    fn newly_covered_ancestor_discovers_children_with_skipped_relationship_hooks() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let root = world.spawn_empty().id();
        let neutral = world.spawn_empty().id();
        let primitive = world.spawn(Mesh3d(Handle::default())).id();
        world
            .entity_mut(neutral)
            .insert_with_relationship_hook_mode(ChildOf(root), RelationshipHookMode::Skip);
        world
            .entity_mut(primitive)
            .insert_with_relationship_hook_mode(ChildOf(neutral), RelationshipHookMode::Skip);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert!(state.get(&world).unwrap().identities().is_empty());
        assert_eq!(
            world
                .resource::<TerrainRelationshipChecks>()
                .snapshot()
                .recovery_requests,
            2
        );
        world
            .entity_mut(root)
            .insert((coverage(1, None), TerrainWorldspace(7)));
        assert_eq!(
            state.get(&world).unwrap().identities()[&primitive].quadrant,
            1
        );
        let replacement = world.spawn((coverage(2, None), TerrainWorldspace(8))).id();
        world
            .entity_mut(neutral)
            .insert_with_relationship_hook_mode(ChildOf(replacement), RelationshipHookMode::Skip);
        let actual = state.get(&world).unwrap().identities();
        assert_eq!(actual[&primitive].worldspace_id, 8);
        assert_eq!(actual[&primitive].quadrant, 2);
        world.entity_mut(neutral).remove::<ChildOf>();
        assert!(state.get(&world).unwrap().identities().is_empty());
    }

    #[test]
    fn installing_tracking_after_a_legacy_snapshot_initializes_dependencies() {
        let mut world = World::new();
        let root = world.spawn(TerrainWorldspace(7)).id();
        let owner = world.spawn((coverage(0, None), ChildOf(root))).id();
        let primitive = mesh(&mut world, owner);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert_eq!(
            state.get(&world).unwrap().identities()[&primitive].worldspace_id,
            7
        );
        identity_changes::install(&mut world);
        assert_eq!(
            state.get(&world).unwrap().identities()[&primitive].worldspace_id,
            7
        );
        world.get_mut::<TerrainWorldspace>(root).unwrap().0 = 8;
        assert_eq!(
            state.get(&world).unwrap().identities()[&primitive].worldspace_id,
            8
        );
    }

    #[test]
    fn incremental_chunk_mutations_cycles_and_conflicts_preserve_refusals() {
        let mut world = World::new();
        identity_changes::install(&mut world);
        let root = chunk(&mut world, 91);
        let owner = world
            .spawn((coverage(0, Some(LodTier::Tier16)), ChildOf(root)))
            .id();
        let primitive = mesh(&mut world, owner);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert_eq!(
            state.get(&world).unwrap().identities()[&primitive].worldspace_id,
            91
        );
        let donor = chunk(&mut world, 92);
        let marker = world.entity_mut(donor).take::<lod::LodChunkRoot>().unwrap();
        // This changes the component in place, so no insertion observer fires on the root.
        *world.get_mut::<lod::LodChunkRoot>(root).unwrap() = marker;
        assert_eq!(
            state.get(&world).unwrap().identities()[&primitive].worldspace_id,
            92
        );
        world.entity_mut(root).insert(ChildOf(owner));
        assert!(state.get(&world).unwrap().identities().is_empty());
        world.entity_mut(root).remove::<ChildOf>();
        assert!(
            state
                .get(&world)
                .unwrap()
                .identities()
                .contains_key(&primitive)
        );
        world
            .entity_mut(primitive)
            .insert(coverage(0, Some(LodTier::Tier16)));
        assert!(state.get(&world).unwrap().identities().is_empty());
        world.entity_mut(primitive).remove::<TerrainCoverage>();
        assert!(
            state
                .get(&world)
                .unwrap()
                .identities()
                .contains_key(&primitive)
        );
        world.entity_mut(root).insert(TerrainWorldspace(92));
        assert!(state.get(&world).unwrap().identities().is_empty());
        world.entity_mut(root).remove::<TerrainWorldspace>();
        assert!(
            state
                .get(&world)
                .unwrap()
                .identities()
                .contains_key(&primitive)
        );
    }

    fn coverage(quadrant: u8, tier: Option<LodTier>) -> TerrainCoverage {
        TerrainCoverage {
            grid: IVec2::new(-19, 37),
            quadrant,
            tier,
        }
    }

    fn mesh(world: &mut World, parent: Entity) -> Entity {
        world
            .spawn((Mesh3d(Handle::default()), ChildOf(parent)))
            .id()
    }

    fn chunk(world: &mut World, worldspace: u32) -> Entity {
        world.init_resource::<lod::LodStreaming>();
        let origin = shared::lod::LodOrigin::new(13, -7);
        let anchor = origin.chunk_for_cell(LodTier::Tier16, -19, 37);
        lod::LodStreaming::spawn_ready_chunk_for_test(world, worldspace, anchor, origin)
    }

    /// The unchanged per-mesh method is the oracle, using the same real ECS queries.
    fn compare(world: &mut World) -> HashMap<Entity, TerrainPrimitiveIdentity> {
        let mut state = SystemState::<TerrainIdentityAccess>::new(world);
        cached_compare(&mut state, world).0
    }

    fn cached_compare(
        state: &mut SystemState<TerrainIdentityAccess>,
        world: &World,
    ) -> (HashMap<Entity, TerrainPrimitiveIdentity>, usize) {
        let mut access = state.get(world).expect("terrain identity queries");
        assert_node_domain(&access, world);
        let expected: HashMap<_, _> = access
            .meshes
            .iter()
            .filter_map(|entity| access.identity(entity).map(|identity| (entity, identity)))
            .collect();
        assert_eq!(access.uncached_identities(), expected);
        let batch = access.identities();
        assert_eq!(batch, expected);
        assert!(access.removed_meshes.is_empty());
        assert!(access.removed_parents.is_empty());
        assert!(access.removed_coverage.is_empty());
        assert!(access.removed_worlds.is_empty());
        assert!(access.removed_chunks.is_empty());
        (batch, access.cache.rebuilds)
    }

    fn assert_node_domain(access: &TerrainIdentityAccess, world: &World) {
        // Independently check the actual ancestry query against every live entity, including
        // disabled entities which a plain Entity query would itself omit.
        let actual: bevy::ecs::entity::EntityHashSet = world
            .iter_entities()
            .map(|entity| entity.id())
            .filter(|entity| access.nodes.contains(*entity))
            .collect();
        let snapshot: bevy::ecs::entity::EntityHashSet = access
            .node_entities
            .iter()
            .map(|(entity, _)| entity)
            .collect();
        assert_eq!(snapshot, actual);
    }

    fn rebuilt(
        state: &mut SystemState<TerrainIdentityAccess>,
        world: &World,
        rebuilds: &mut usize,
    ) -> HashMap<Entity, TerrainPrimitiveIdentity> {
        let (batch, actual) = cached_compare(state, world);
        assert_eq!(actual, *rebuilds + 1);
        *rebuilds = actual;
        batch
    }

    #[test]
    fn tracked_shared_snapshot_handles_custom_and_late_disablers() {
        #[derive(Component)]
        struct LateDisabled;
        let mut world = World::new();
        world.register_disabling_component::<CustomDisabled>();
        identity_changes::install(&mut world);
        let owner = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        let neutral = world.spawn(ChildOf(owner)).id();
        let primitive = mesh(&mut world, neutral);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let initial = state.get(&world).unwrap().identities_shared();
        let warm = state.get(&world).unwrap().identities_shared();
        assert!(std::sync::Arc::ptr_eq(&initial, &warm));
        world.entity_mut(neutral).insert(CustomDisabled);
        assert!(state.get(&world).unwrap().identities_shared().is_empty());
        world.entity_mut(neutral).remove::<CustomDisabled>();
        assert!(
            state
                .get(&world)
                .unwrap()
                .identities_shared()
                .contains_key(&MainEntity::from(primitive))
        );
        world.register_disabling_component::<LateDisabled>();
        assert!(
            world
                .resource::<identity_changes::StructureEpoch>()
                .covered_version(
                    world.resource::<bevy::ecs::entity_disabling::DefaultQueryFilters>()
                )
                .is_none()
        );
        // Bevy fixes default filters when a query is initialized. A new query
        // sees the late registration; the older query keeps its original domain.
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        world.entity_mut(neutral).insert(LateDisabled);
        assert!(state.get(&world).unwrap().identities_shared().is_empty());
        world.entity_mut(neutral).remove::<LateDisabled>();
        assert!(
            state
                .get(&world)
                .unwrap()
                .identities_shared()
                .contains_key(&MainEntity::from(primitive))
        );
    }

    #[test]
    fn warm_results_reuse_without_changes_and_for_unrelated_transform_changes() {
        let mut world = World::new();
        let primitive = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(0, None),
                TerrainWorldspace(7),
            ))
            .id();
        let unrelated = world.spawn(Transform::default()).id();
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let (expected, rebuilds) = cached_compare(&mut state, &world);
        assert_eq!(expected[&primitive].worldspace_id, 7);
        assert_eq!(rebuilds, 1);
        assert_eq!(cached_compare(&mut state, &world), (expected.clone(), 1));
        world.get_mut::<Transform>(unrelated).unwrap().translation.x = 12.0;
        assert_eq!(cached_compare(&mut state, &world), (expected, 1));
    }

    #[test]
    fn warm_cache_tracks_mesh_addition_and_removal() {
        let mut world = World::new();
        let primitive = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let mut rebuilds = 0;
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
        world
            .entity_mut(primitive)
            .insert(Mesh3d(Handle::default()));
        assert!(rebuilt(&mut state, &world, &mut rebuilds).contains_key(&primitive));
        world.entity_mut(primitive).remove::<Mesh3d>();
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
    }

    #[test]
    fn warm_cache_tracks_parent_add_change_remove_and_cycles() {
        let mut world = World::new();
        let first = world.spawn(TerrainWorldspace(7)).id();
        let second = world.spawn(TerrainWorldspace(8)).id();
        let primitive = world
            .spawn((Mesh3d(Handle::default()), coverage(0, None)))
            .id();
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let mut rebuilds = 0;
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
        world.entity_mut(primitive).insert(ChildOf(first));
        assert_eq!(
            rebuilt(&mut state, &world, &mut rebuilds)[&primitive].worldspace_id,
            7
        );
        world.entity_mut(primitive).insert(ChildOf(second));
        assert_eq!(
            rebuilt(&mut state, &world, &mut rebuilds)[&primitive].worldspace_id,
            8
        );
        world.entity_mut(second).insert(ChildOf(primitive));
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
        world.entity_mut(second).remove::<ChildOf>();
        assert!(rebuilt(&mut state, &world, &mut rebuilds).contains_key(&primitive));
        world.entity_mut(primitive).remove::<ChildOf>();
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
    }

    #[test]
    fn warm_cache_tracks_coverage_world_and_nested_marker_changes() {
        let mut world = World::new();
        let owner = world.spawn_empty().id();
        let primitive = mesh(&mut world, owner);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let mut rebuilds = 0;
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
        world.entity_mut(owner).insert(coverage(0, None));
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
        world.entity_mut(owner).insert(TerrainWorldspace(7));
        assert_eq!(
            rebuilt(&mut state, &world, &mut rebuilds)[&primitive].worldspace_id,
            7
        );
        world.get_mut::<TerrainWorldspace>(owner).unwrap().0 = 8;
        assert_eq!(
            rebuilt(&mut state, &world, &mut rebuilds)[&primitive].worldspace_id,
            8
        );
        world.get_mut::<TerrainCoverage>(owner).unwrap().quadrant = 3;
        assert_eq!(
            rebuilt(&mut state, &world, &mut rebuilds)[&primitive].quadrant,
            3
        );
        // Equal nested markers are still ambiguous, not a reason to stop ancestry early.
        world.entity_mut(primitive).insert(coverage(3, None));
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
        world.entity_mut(primitive).remove::<TerrainCoverage>();
        assert!(rebuilt(&mut state, &world, &mut rebuilds).contains_key(&primitive));
        world.entity_mut(owner).remove::<TerrainCoverage>();
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
        world.entity_mut(owner).insert(coverage(0, None));
        assert!(rebuilt(&mut state, &world, &mut rebuilds).contains_key(&primitive));
        world.entity_mut(owner).remove::<TerrainWorldspace>();
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
    }

    #[test]
    fn warm_cache_tracks_chunk_add_change_and_removal() {
        let mut world = World::new();
        let first = chunk(&mut world, 91);
        let first_marker = world.entity_mut(first).take::<lod::LodChunkRoot>().unwrap();
        let second = chunk(&mut world, 92);
        let second_marker = world
            .entity_mut(second)
            .take::<lod::LodChunkRoot>()
            .unwrap();
        let owner = world.spawn(coverage(0, Some(LodTier::Tier16))).id();
        let primitive = mesh(&mut world, owner);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let mut rebuilds = 0;
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
        world.entity_mut(owner).insert(first_marker);
        assert_eq!(
            rebuilt(&mut state, &world, &mut rebuilds)[&primitive].worldspace_id,
            91
        );
        world.entity_mut(owner).insert(second_marker);
        assert_eq!(
            rebuilt(&mut state, &world, &mut rebuilds)[&primitive].worldspace_id,
            92
        );
        world.entity_mut(owner).remove::<lod::LodChunkRoot>();
        assert!(rebuilt(&mut state, &world, &mut rebuilds).is_empty());
    }

    #[derive(Component, Default)]
    struct CustomDisabled;

    #[test]
    fn warm_cache_observes_default_disablers_on_mesh_and_neutral_ancestors() {
        fn roundtrip<T: Component + Default>(
            state: &mut SystemState<TerrainIdentityAccess>,
            world: &mut World,
            entity: Entity,
            primitive: Entity,
            rebuilds: &mut usize,
        ) {
            world.entity_mut(entity).insert(T::default());
            assert!(rebuilt(state, world, rebuilds).is_empty());
            world.entity_mut(entity).remove::<T>();
            assert!(rebuilt(state, world, rebuilds).contains_key(&primitive));
        }
        let mut world = World::new();
        world.register_disabling_component::<CustomDisabled>();
        let owner = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        let neutral = world.spawn(ChildOf(owner)).id();
        let primitive = mesh(&mut world, neutral);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        let mut rebuilds = 0;
        assert!(rebuilt(&mut state, &world, &mut rebuilds).contains_key(&primitive));
        for entity in [primitive, neutral] {
            roundtrip::<Disabled>(&mut state, &mut world, entity, primitive, &mut rebuilds);
            roundtrip::<CustomDisabled>(&mut state, &mut world, entity, primitive, &mut rebuilds);
        }
    }

    #[test]
    fn node_membership_preserves_optional_exclusion_for_a_disabling_terrain_marker() {
        let mut world = World::new();
        world.register_disabling_component::<TerrainWorldspace>();
        let owner = world.spawn((coverage(0, None), TerrainWorldspace(7))).id();
        let primitive = mesh(&mut world, owner);
        let far_root = chunk(&mut world, 91);
        let far_owner = world
            .spawn((coverage(1, Some(LodTier::Tier16)), ChildOf(far_root)))
            .id();
        let far_primitive = mesh(&mut world, far_owner);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        for (step, (enabled, rebuilds)) in
            [(false, 1), (false, 1), (true, 2), (true, 2), (false, 3)]
                .iter()
                .copied()
                .enumerate()
        {
            match step {
                2 => {
                    world.entity_mut(owner).remove::<TerrainWorldspace>();
                }
                4 => {
                    world.entity_mut(owner).insert(TerrainWorldspace(7));
                }
                _ => {}
            }
            let mut access = state.get(&world).unwrap();
            assert_node_domain(&access, &world);
            assert_eq!(access.nodes.contains(owner), enabled);
            assert_eq!(access.node_entities.contains(owner), enabled);
            // The optional world read does not bypass its disabling registration. With the
            // marker removed the parent becomes queryable, but still supplies no near world.
            assert_eq!(access.identity(primitive), None);
            let expected: HashMap<_, _> = access
                .meshes
                .iter()
                .filter_map(|entity| access.identity(entity).map(|identity| (entity, identity)))
                .collect();
            assert_eq!(access.uncached_identities(), expected);
            let batch = access.identities();
            assert_eq!(batch, expected);
            assert!(!batch.contains_key(&primitive));
            assert_eq!(batch[&far_primitive].worldspace_id, 91);
            assert_eq!(access.cache.rebuilds, rebuilds);
        }
    }

    #[test]
    fn exact_membership_detects_neutral_parent_despawn_with_count_preserving_churn() {
        let mut world = World::new();
        let neutral = world.spawn_empty().id();
        let primitive = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(0, None),
                TerrainWorldspace(7),
            ))
            .id();
        // A safe hook bypass leaves valid ChildOf ancestry absent from Children.
        world
            .entity_mut(primitive)
            .insert_with_relationship_hook_mode(ChildOf(neutral), RelationshipHookMode::Skip);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert!(
            cached_compare(&mut state, &world)
                .0
                .contains_key(&primitive)
        );
        world.despawn(neutral);
        let replacement = world.spawn_empty().id();
        assert_ne!(neutral, replacement);
        {
            let mut access = state.get(&world).unwrap();
            assert_eq!(
                access.node_entities.iter().count(),
                access.cache.entities.len()
            );
            assert!(!access.node_entities.contains(neutral));
            assert!(access.node_entities.contains(replacement));
            assert!(access.changed.is_empty());
            assert!(access.removed_parents.is_empty());
            assert_eq!(access.identity(primitive), None);
            assert!(access.identities().is_empty());
            assert_eq!(access.cache.rebuilds, 2);
        }
    }

    #[test]
    fn expired_removal_messages_force_rebuild_even_without_unread_messages() {
        let mut world = World::new();
        let primitive = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(0, None),
                TerrainWorldspace(7),
            ))
            .id();
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert!(
            cached_compare(&mut state, &world)
                .0
                .contains_key(&primitive)
        );
        world.entity_mut(primitive).remove::<TerrainWorldspace>();
        world.clear_trackers();
        world.clear_trackers();
        world.clear_trackers();
        let mut access = state.get(&world).unwrap();
        assert!(access.removed_worlds.is_empty());
        let messages = access.removed_worlds.messages().unwrap();
        assert!(access.removed_worlds.reader().missed_messages(messages) > 0);
        assert!(access.changed.is_empty());
        assert_eq!(
            access
                .node_entities
                .iter()
                .map(|(entity, _)| entity)
                .collect::<Vec<_>>(),
            access.cache.entities
        );
        assert_eq!(access.cache.evaluated, Some(access.ticks.last_run()));
        assert_eq!(access.identity(primitive), None);
        assert!(access.identities().is_empty());
        assert_eq!(access.cache.rebuilds, 2);
        let messages = access.removed_worlds.messages().unwrap();
        assert_eq!(access.removed_worlds.reader().missed_messages(messages), 0);
    }

    #[test]
    fn fetch_without_evaluation_cannot_hide_a_changed_world() {
        let mut world = World::new();
        let primitive = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(0, None),
                TerrainWorldspace(7),
            ))
            .id();
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert_eq!(
            cached_compare(&mut state, &world).0[&primitive].worldspace_id,
            7
        );
        world.get_mut::<TerrainWorldspace>(primitive).unwrap().0 = 8;
        {
            let access = state.get(&world).unwrap();
            assert!(!access.changed.is_empty());
        }
        {
            let mut access = state.get(&world).unwrap();
            assert!(access.changed.is_empty());
            assert_eq!(
                access
                    .node_entities
                    .iter()
                    .map(|(entity, _)| entity)
                    .collect::<Vec<_>>(),
                access.cache.entities
            );
            assert_eq!(access.identity(primitive).unwrap().worldspace_id, 8);
            assert_eq!(access.identities()[&primitive].worldspace_id, 8);
            assert_eq!(access.cache.rebuilds, 2);
        }
    }

    #[test]
    fn mesh_and_ancestor_markers_compose_without_early_stopping_or_nonmesh_output() {
        let mut world = World::new();
        let root = world.spawn(TerrainWorldspace(71)).id();
        let quadrant = world.spawn((coverage(3, None), ChildOf(root))).id();
        let inherited = mesh(&mut world, quadrant);
        let direct = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(1, None),
                TerrainWorldspace(72),
            ))
            .id();
        let repeated_world = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(0, None),
                TerrainWorldspace(71),
                ChildOf(root),
            ))
            .id();
        let conflicting_world = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(0, None),
                TerrainWorldspace(72),
                ChildOf(root),
            ))
            .id();
        let batch = compare(&mut world);
        assert_eq!(batch[&inherited].worldspace_id, 71);
        assert_eq!(batch[&inherited].quadrant, 3);
        assert_eq!(batch[&direct].worldspace_id, 72);
        assert!(batch.contains_key(&repeated_world));
        assert!(!batch.contains_key(&conflicting_world));
        assert!(!batch.contains_key(&quadrant));
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert_eq!(state.get(&world).unwrap().identity(quadrant), None);
    }

    #[test]
    fn equal_or_conflicting_duplicate_coverage_does_not_poison_valid_siblings() {
        let mut world = World::new();
        let parent = world.spawn((coverage(2, None), TerrainWorldspace(71))).id();
        let valid = mesh(&mut world, parent);
        let equal = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(2, None),
                ChildOf(parent),
            ))
            .id();
        let conflicting = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(1, None),
                ChildOf(parent),
            ))
            .id();
        let batch = compare(&mut world);
        assert_eq!(batch.len(), 1);
        assert!(batch.contains_key(&valid));
        assert!(!batch.contains_key(&equal));
        assert!(!batch.contains_key(&conflicting));
        // Removing descendant ambiguity must qualify on the next fresh invocation.
        world.entity_mut(equal).remove::<TerrainCoverage>();
        assert!(compare(&mut world).contains_key(&equal));
    }

    #[test]
    fn far_ownership_requires_one_chunk_exact_tier_and_valid_quadrant() {
        let mut world = World::new();
        let root = chunk(&mut world, 91);
        let mut expected = Vec::new();
        for tier in [
            None,
            Some(LodTier::Tier4),
            Some(LodTier::Tier8),
            Some(LodTier::Tier16),
        ] {
            for quadrant in [0, 3, 4, u8::MAX] {
                let owner = world.spawn((coverage(quadrant, tier), ChildOf(root))).id();
                let primitive = mesh(&mut world, owner);
                expected.push((primitive, tier == Some(LodTier::Tier16) && quadrant < 4));
            }
        }
        let batch = compare(&mut world);
        for (primitive, valid) in expected {
            assert_eq!(batch.contains_key(&primitive), valid);
            if valid {
                assert_eq!(batch[&primitive].worldspace_id, 91);
                assert_eq!(batch[&primitive].grid, IVec2::new(-19, 37));
                assert_eq!(batch[&primitive].tier, Some(LodTier::Tier16));
            }
        }
        // A chunk marker on the actual mesh is also part of the same ancestry.
        let donor = chunk(&mut world, 93);
        let marker = world.entity_mut(donor).take::<lod::LodChunkRoot>().unwrap();
        let direct = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(1, Some(LodTier::Tier16)),
                marker,
            ))
            .id();
        assert_eq!(compare(&mut world)[&direct].worldspace_id, 93);
    }

    #[test]
    fn equal_duplicate_chunks_and_mixed_near_far_ancestry_are_always_invalid() {
        for outer_world in [71, 72] {
            let mut world = World::new();
            let outer = chunk(&mut world, outer_world);
            let inner = chunk(&mut world, 71);
            world.entity_mut(inner).insert(ChildOf(outer));
            let primitive = world
                .spawn((
                    Mesh3d(Handle::default()),
                    coverage(0, Some(LodTier::Tier16)),
                    ChildOf(inner),
                ))
                .id();
            assert!(!compare(&mut world).contains_key(&primitive));
        }
        for tier in [None, Some(LodTier::Tier16)] {
            let mut world = World::new();
            let near = world.spawn(TerrainWorldspace(71)).id();
            let far = chunk(&mut world, 71);
            world.entity_mut(far).insert(ChildOf(near));
            let primitive = world
                .spawn((Mesh3d(Handle::default()), coverage(0, tier), ChildOf(far)))
                .id();
            assert!(!compare(&mut world).contains_key(&primitive));
        }
    }

    #[test]
    fn shared_incomplete_ancestry_can_be_completed_by_only_some_descendants() {
        let mut world = World::new();
        let mut ancestor = world.spawn_empty().id();
        let root = ancestor;
        for _ in 0..64 {
            ancestor = world.spawn(ChildOf(ancestor)).id();
        }
        let children: Vec<_> = (0..32).map(|_| mesh(&mut world, ancestor)).collect();
        assert!(compare(&mut world).is_empty());
        world.entity_mut(root).insert(TerrainWorldspace(7));
        world.entity_mut(children[0]).insert(coverage(2, None));
        let batch = compare(&mut world);
        assert_eq!(batch.len(), 1);
        assert!(batch.contains_key(&children[0]));
        world.entity_mut(ancestor).insert(coverage(1, None));
        let batch = compare(&mut world);
        assert_eq!(batch.len(), children.len() - 1);
        assert!(!batch.contains_key(&children[0]));
    }

    #[test]
    fn real_ecs_cycles_invalidate_even_an_apparently_complete_descendant() {
        let mut world = World::new();
        let first = world.spawn(TerrainWorldspace(7)).id();
        let second = world.spawn(ChildOf(first)).id();
        world.entity_mut(first).insert(ChildOf(second));
        let primitive = world
            .spawn((
                Mesh3d(Handle::default()),
                coverage(0, None),
                ChildOf(second),
            ))
            .id();
        assert!(!compare(&mut world).contains_key(&primitive));
        world.entity_mut(first).remove::<ChildOf>();
        assert!(compare(&mut world).contains_key(&primitive));
    }

    #[test]
    fn fresh_batches_observe_reparenting_marker_mesh_removal_and_entity_generation() {
        let mut world = World::new();
        let first = world.spawn(TerrainWorldspace(7)).id();
        let second = world.spawn(TerrainWorldspace(8)).id();
        let original = world
            .spawn((Mesh3d(Handle::default()), coverage(2, None), ChildOf(first)))
            .id();
        assert_eq!(compare(&mut world)[&original].worldspace_id, 7);
        world.entity_mut(original).insert(ChildOf(second));
        assert_eq!(compare(&mut world)[&original].worldspace_id, 8);
        world.entity_mut(second).remove::<TerrainWorldspace>();
        assert!(!compare(&mut world).contains_key(&original));
        world.entity_mut(second).insert(TerrainWorldspace(8));
        assert!(compare(&mut world).contains_key(&original));
        world.entity_mut(original).remove::<Mesh3d>();
        assert!(!compare(&mut world).contains_key(&original));
        world.entity_mut(original).insert(Mesh3d(Handle::default()));
        assert!(compare(&mut world).contains_key(&original));
        world.despawn(original);
        let replacement = world
            .spawn((Mesh3d(Handle::default()), coverage(1, None), ChildOf(first)))
            .id();
        assert_ne!(original, replacement);
        let batch = compare(&mut world);
        assert!(!batch.contains_key(&original));
        assert_eq!(batch[&replacement].worldspace_id, 7);
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert_eq!(state.get(&world).unwrap().identity(original), None);
    }

    #[test]
    fn one_memo_distinguishes_same_index_entities_with_different_generations() {
        let original = Entity::from_raw_u32(1).unwrap();
        let replacement = Entity::from_index_and_generation(
            original.index(),
            original.generation().after_versions(1),
        );
        assert_eq!(original.index(), replacement.index());
        assert_ne!(original.generation(), replacement.generation());
        let facts = HashMap::from([
            (
                original,
                TerrainNodeFacts {
                    coverage: Some(coverage(1, None)),
                    near_world: Some(7),
                    ..default()
                },
            ),
            (
                replacement,
                TerrainNodeFacts {
                    coverage: Some(coverage(2, None)),
                    near_world: Some(8),
                    ..default()
                },
            ),
        ]);
        let mut memo = TerrainAncestryMemo::default();
        for entity in [original, replacement, original, replacement] {
            let identity = memo
                .resolve(entity, |at| facts.get(&at).copied())
                .unwrap()
                .identity()
                .unwrap();
            let expected = if entity == original { (7, 1) } else { (8, 2) };
            assert_eq!((identity.worldspace_id, identity.quadrant), expected);
            assert!(memo.visiting.is_empty());
        }
        assert_eq!(memo.resolved.len(), 2);
    }

    #[test]
    fn missing_and_cyclic_shared_tails_are_cached_once_and_drain_all_markers() {
        let entity = |id| Entity::from_raw_u32(id).unwrap();
        let missing = entity(1);
        let negative = entity(2);
        let cycle_a = entity(3);
        let cycle_b = entity(4);
        let mut nodes = HashMap::from([
            (
                negative,
                TerrainNodeFacts {
                    parent: Some(missing),
                    ..default()
                },
            ),
            (
                cycle_a,
                TerrainNodeFacts {
                    parent: Some(cycle_b),
                    ..default()
                },
            ),
            (
                cycle_b,
                TerrainNodeFacts {
                    parent: Some(cycle_a),
                    ..default()
                },
            ),
        ]);
        for id in 10..42 {
            nodes.insert(
                entity(id),
                TerrainNodeFacts {
                    parent: Some(negative),
                    ..default()
                },
            );
        }
        let mut visited = Vec::new();
        let mut memo = TerrainAncestryMemo::default();
        let mut lookup = |memo: &mut TerrainAncestryMemo, start| {
            memo.resolve(start, |at| {
                visited.push(at);
                nodes.get(&at).copied()
            })
        };
        for id in 10..42 {
            assert_eq!(lookup(&mut memo, entity(id)), None);
            assert!(memo.visiting.is_empty());
        }
        assert_eq!(lookup(&mut memo, cycle_a), None);
        assert_eq!(lookup(&mut memo, cycle_b), None);
        assert!(memo.visiting.is_empty());
        assert_eq!(visited.len(), 32 + 2 + 2);
        assert_eq!(visited.iter().filter(|&&at| at == missing).count(), 1);
        assert_eq!(visited.iter().filter(|&&at| at == negative).count(), 1);
        let mut world = World::new();
        let mut state = SystemState::<TerrainIdentityAccess>::new(&mut world);
        assert_eq!(state.get(&world).unwrap().identity(missing), None);
        assert!(compare(&mut world).is_empty());
    }
}
