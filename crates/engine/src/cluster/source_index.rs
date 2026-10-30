//! Exact, live-entity-bounded ancestry dependencies for stock qualification.
//! Index every root and mesh, even outside a certified subtree: a later FormId or
//! ChildOf edit can introduce it without a Children walk or a global mesh scan.
use super::*;

#[derive(Default)]
pub(super) struct SourceIndex {
    subjects: EntityHashMap<Subject>,
    dependents: EntityHashMap<EntityHashSet>,
    by_reference: HashMap<u32, EntityHashSet>,
    pub(super) initialized: bool,
}

struct Subject {
    path: Vec<Entity>,
    references: HashSet<u32>,
}

#[derive(Default)]
pub(super) struct Affected {
    pub(super) references: HashSet<u32>,
    pub(super) roots: EntityHashSet,
    pub(super) meshes: EntityHashSet,
    pub(super) history: EntityHashSet,
    pub(super) nodes: EntityHashSet,
    #[cfg(test)]
    pub(super) subjects_updated: u64,
    #[cfg(test)]
    pub(super) ancestry_edges: u64,
    #[cfg(test)]
    pub(super) discovery_roots: u64,
    #[cfg(test)]
    pub(super) discovery_meshes: u64,
}

impl SourceIndex {
    fn remove(&mut self, entity: Entity, affected: &mut Affected) {
        let Some(old) = self.subjects.remove(&entity) else {
            return;
        };
        for node in old.path {
            affected.nodes.insert(node);
            if let Some(subjects) = self.dependents.get_mut(&node) {
                subjects.remove(&entity);
                if subjects.is_empty() {
                    self.dependents.remove(&node);
                }
            }
        }
        for reference in old.references {
            affected.references.insert(reference);
            if let Some(subjects) = self.by_reference.get_mut(&reference) {
                subjects.remove(&entity);
                if subjects.is_empty() {
                    self.by_reference.remove(&reference);
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn sizes(&self) -> (usize, usize, usize) {
        (
            self.subjects.len(),
            self.dependents.len(),
            self.by_reference.len(),
        )
    }

    pub(super) fn contains_node(&self, entity: Entity) -> bool {
        self.dependents.contains_key(&entity)
    }

    pub(super) fn update(
        &mut self,
        queries: &SourceQueries,
        seen: &HashMap<Entity, BTreeSet<u32>>,
        changed: &EntityHashSet,
        full: bool,
        certificate: Option<&crate::cluster::reader::CertifiedPackage>,
    ) -> Affected {
        let mut affected = Affected::default();
        let mut dirty = changed.clone();
        if full {
            dirty.extend(self.subjects.keys().copied());
            for (entity, ..) in &queries.roots {
                #[cfg(test)]
                {
                    affected.discovery_roots += 1;
                }
                // Independent full/oracle inputs, never derived from group membership.
                affected.roots.insert(entity);
                dirty.insert(entity);
            }
            for entity in &queries.all_meshes {
                #[cfg(test)]
                {
                    affected.discovery_meshes += 1;
                }
                affected.meshes.insert(entity);
                dirty.insert(entity);
            }
            dirty.extend(seen.keys().copied());
        } else {
            // Snapshot all old memberships before changing ANY edge. This is essential
            // for simultaneous remove/reparent/identity edits and entity despawns.
            for node in changed {
                if let Some(subjects) = self.dependents.get(node) {
                    dirty.extend(subjects.iter().copied());
                }
            }
        }
        for &entity in &dirty {
            self.remove(entity, &mut affected);
        }
        for entity in dirty {
            #[cfg(test)]
            {
                affected.subjects_updated += 1;
            }
            // Role membership is authoritative even if a generic query excludes
            // this entity (e.g. FormId or Mesh3d registered as a disabler).
            // Only retained history needs independent, unfiltered liveness.
            if !(queries.roots.contains(entity)
                || queries.all_meshes.contains(entity)
                || (queries.entities.contains_spawned(entity) && seen.contains_key(&entity)))
            {
                continue;
            }
            let mut path = Vec::new();
            let mut visited = EntityHashSet::default();
            let mut at = entity;
            let mut nearest = None;
            loop {
                if !visited.insert(at) {
                    break;
                }
                path.push(at);
                affected.nodes.insert(at);
                if nearest.is_none()
                    && let Ok((_, form, ..)) = queries.roots.get(at)
                {
                    nearest = Some(form.0);
                }
                let Some(parent) =
                    flat_source::logical_parent(at, &queries.parents, &queries.flat_owners)
                else {
                    break;
                };
                #[cfg(test)]
                {
                    affected.ancestry_edges += 1;
                }
                at = parent;
            }
            let mut references = HashSet::new();
            references.extend(nearest);
            if let Some(history) = seen.get(&entity) {
                references.extend(history);
            }
            for &node in &path {
                self.dependents.entry(node).or_default().insert(entity);
            }
            for &reference in &references {
                affected.references.insert(reference);
                self.by_reference
                    .entry(reference)
                    .or_default()
                    .insert(entity);
            }
            self.subjects.insert(entity, Subject { path, references });
        }
        // Close over ALL roots with the affected reference, not only the edited root.
        // This preserves duplicate-root refusal and survivor promotion on removal.
        affected
            .references
            .retain(|reference| certificate.is_some_and(|c| c.reference(*reference).is_some()));
        for reference in &affected.references {
            if let Some(subjects) = self.by_reference.get(reference) {
                for &entity in subjects {
                    if queries.roots.contains(entity) {
                        affected.roots.insert(entity);
                    }
                    if queries.all_meshes.contains(entity) {
                        affected.meshes.insert(entity);
                    }
                    if seen.contains_key(&entity) {
                        affected.history.insert(entity);
                    }
                }
            }
        }
        self.initialized = true;
        affected
    }
}
