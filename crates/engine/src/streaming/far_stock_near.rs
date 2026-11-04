//! Direct near-source metadata index owned by the far-stock planner.
//!
//! Lifecycle events select exact candidates. In-place metadata writers notify the private
//! domain journal explicitly; retained values suppress equal-write projection work.

use super::*;
use crate::world::components::StreamedCellRoot;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct NearSource {
    pub(super) parent: Entity,
    pub(super) key: CellKey,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Record {
    form: u32,
    cell: u32,
    parent: Entity,
    source: Option<(Identity, NearSource)>,
}

#[derive(Default)]
pub(super) struct NearSources {
    #[cfg(test)]
    pub(super) polling_work: PollingWork,
    records: HashMap<Entity, Record>,
    by_parent: HashMap<Entity, HashSet<Entity>>,
    pub(super) by_identity: HashMap<Identity, HashMap<Entity, NearSource>>,
}

/// Per-World discovery evidence. Candidate counters now count journal-selected probes
/// and explicit recovery snapshots; settled refreshes perform no population polling.
#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub(super) struct PollingWork {
    pub(super) poll_frames: u64,
    pub(super) removal_stream_polls: u64,
    pub(super) reference_candidates_upper: u64,
    pub(super) reference_changed_yields: u64,
    pub(super) root_candidates_upper: u64,
    pub(super) root_changed_yields: u64,
    pub(super) removed_events: u64,
    pub(super) index_inserts: u64,
    pub(super) index_replacements: u64,
    pub(super) index_removals: u64,
}

impl NearSources {
    fn replace(
        &mut self,
        entity: Entity,
        next: Option<Record>,
        dirty: &mut HashSet<Identity>,
        #[cfg(test)] work: &mut PollingWork,
    ) {
        let previous = self.records.get(&entity).copied();
        if previous == next {
            return;
        }
        #[cfg(test)]
        if previous.is_none() {
            work.index_inserts += 1;
        } else if next.is_none() {
            work.index_removals += 1;
        } else {
            work.index_replacements += 1;
        }
        if let Some(old) = self.records.remove(&entity) {
            if let Some(children) = self.by_parent.get_mut(&old.parent) {
                children.remove(&entity);
                if children.is_empty() {
                    self.by_parent.remove(&old.parent);
                }
            }
            if let Some((id, _)) = old.source {
                dirty.insert(id);
                if let Some(sources) = self.by_identity.get_mut(&id) {
                    sources.remove(&entity);
                    if sources.is_empty() {
                        self.by_identity.remove(&id);
                    }
                }
            }
        }
        if let Some(next) = next {
            self.by_parent
                .entry(next.parent)
                .or_default()
                .insert(entity);
            if let Some((id, source)) = next.source {
                dirty.insert(id);
                self.by_identity
                    .entry(id)
                    .or_default()
                    .insert(entity, source);
            }
            self.records.insert(entity, next);
        }
    }
}

#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct NearChanges<'w, 's> {
    references: Query<
        'w,
        's,
        (
            Entity,
            &'static FormId,
            &'static CellRef,
            &'static ChildOf,
            Has<StreamedCellKey>,
        ),
    >,
    filters: Res<'w, bevy::ecs::entity_disabling::DefaultQueryFilters>,
    domain: Option<ResMut<'w, domain::DomainChanges>>,
    parents: Query<'w, 's, &'static StreamedCellKey, (With<StreamedCellRoot>,)>,
}

impl NearChanges<'_, '_> {
    /// Inspect the existing journal without acknowledging either consumer's work.
    pub(super) fn has_work(&self) -> bool {
        self.domain.as_deref().is_none_or(|domain| {
            !domain.covers(&self.filters)
                || domain.baseline
                || domain.near_dirty
                || !domain.references.is_empty()
                || !domain.roots.is_empty()
                || !domain.dirty.is_empty()
                || !domain.removed.is_empty()
        })
    }

    /// Retain and drain exact journal entries even without demand. Only installation,
    /// package replacement, or uncovered query-domain changes request a full snapshot.
    pub(super) fn refresh(
        &mut self,
        index: &mut NearSources,
        package: Option<&CertifiedPackage>,
        mut reset: bool,
        commands: &mut Commands,
    ) -> (HashSet<Identity>, HashSet<Entity>, bool) {
        #[cfg(test)]
        let mut work = std::mem::take(&mut index.polling_work);
        #[cfg(test)]
        {
            work.poll_frames += 1;
        }
        let covered = self
            .domain
            .as_ref()
            .is_some_and(|domain| domain.covers(&self.filters));
        if !covered {
            commands.queue(domain::install);
        }
        reset |= !covered;
        let mut roots = HashSet::new();
        let mut entities = HashSet::new();
        let mut reconcile = reset;
        if let Some(domain) = self.domain.as_mut() {
            reset |= std::mem::take(&mut domain.baseline);
            reconcile |= std::mem::take(&mut domain.near_dirty);
            entities = std::mem::take(&mut domain.references);
            roots = std::mem::take(&mut domain.roots);
            roots.extend(std::mem::take(&mut domain.dirty));
            #[cfg(test)]
            {
                work.removed_events += domain.removed.len() as u64;
            }
            domain.removed.clear();
            // failure_dirty belongs exclusively to publish_work, not this consumer.
        }
        reconcile |= reset;
        #[cfg(test)]
        {
            work.root_candidates_upper += roots.len() as u64;
            work.root_changed_yields += roots.len() as u64;
        }
        // A keyed entity can be either a parent or a newly forbidden child root.
        entities.extend(roots.iter().copied());
        for root in roots {
            if let Some(children) = index.by_parent.get(&root) {
                entities.extend(children.iter().copied());
            }
        }
        if reset {
            entities.extend(index.records.keys().copied());
            entities.extend(self.references.iter().map(|(entity, ..)| {
                #[cfg(test)]
                {
                    work.reference_candidates_upper += 1;
                }
                entity
            }));
        }
        let mut dirty = HashSet::new();
        if !entities.is_empty() {
            for &entity in &entities {
                #[cfg(test)]
                {
                    work.reference_candidates_upper += 1;
                }
                let next =
                    self.references
                        .get(entity)
                        .ok()
                        .map(|(_, form, cell, parent, keyed)| {
                            #[cfg(test)]
                            {
                                work.reference_changed_yields += 1;
                            }
                            let parent = parent.parent();
                            let source = (!keyed)
                                .then(|| {
                                    let key = self.parents.get(parent).ok()?.0;
                                    let world = cell_world(key)?;
                                    let reference = package?.placed_reference(form.0)?;
                                    (reference.snapshot.source_cell_id == cell.0
                                        && reference.snapshot.placed_worldspace_id == world)
                                        .then_some(((world, form.0), NearSource { parent, key }))
                                })
                                .flatten();
                            Record {
                                form: form.0,
                                cell: cell.0,
                                parent,
                                source,
                            }
                        });
                index.replace(
                    entity,
                    next,
                    &mut dirty,
                    #[cfg(test)]
                    &mut work,
                );
            }
        }
        #[cfg(test)]
        {
            index.polling_work = work;
        }
        (dirty, entities, reconcile)
    }
}
