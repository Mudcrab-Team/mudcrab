//! Retained demand projection and affected-cell request reconciliation.

use super::*;

impl FarStock {
    /// Update the final union and its cell index together. Callers never publish a transient
    /// removal when a reference moves between bounded and unbounded contributions.
    pub(super) fn project_union(&mut self, touched: HashSet<Identity>) -> bool {
        #[cfg(test)]
        {
            self.planning_work.projection_entries += touched.len() as u64;
        }
        let mut changed = false;
        for id in touched {
            let next = self
                .unbounded_desired
                .get(&id)
                .or_else(|| self.bounded_desired.get(&id))
                .copied();
            changed |= self.set_target(id, next);
        }
        changed
    }

    pub(super) fn set_target(&mut self, id: Identity, next: Option<CellKey>) -> bool {
        if self.desired.get(&id).copied() == next {
            return false;
        }
        if let Some(old) = self.desired.remove(&id) {
            self.request_dirty.insert(old);
            if let Some(ids) = self.desired_cells.get_mut(&old) {
                ids.remove(&id.1);
                if ids.is_empty() {
                    self.desired_cells.remove(&old);
                }
            }
        }
        if let Some(key) = next {
            self.desired.insert(id, key);
            self.desired_cells.entry(key).or_default().insert(id.1);
            self.request_dirty.insert(key);
        }
        self.target_dirty.insert(id);
        self.invalidate_near();
        self.failure_dirty = true;
        true
    }

    pub(super) fn bounded_key(
        &self,
        package: &CertifiedPackage,
        demand: Option<LodDemand>,
        radius: i32,
        id: Identity,
        enable: Option<&ReferenceEnableState>,
        changed_package: bool,
    ) -> Option<CellKey> {
        let demand = demand.filter(|demand| demand.world == id.0)?;
        let reference = package.placed_reference(id.1)?;
        let snapshot = &reference.snapshot;
        if snapshot.placed_worldspace_id != id.0
            || reference.model_path.is_none()
            || reference_enabled(package, id.1, enable) != Some(true)
            || !demand.contains_cell(id.0, IVec2::new(snapshot.cell.0, snapshot.cell.1), radius)
        {
            return None;
        }
        let retained_complete = reference.coverage == CoverageState::Complete
            && self.near_sources.by_identity.contains_key(&id);
        (needs_stock(reference)
            || self.fallback_references.contains(&id.1)
            || self.permanent_refusals.contains(&id.1)
            || (!changed_package && self.owners.contains_key(&id))
            || retained_complete)
            .then_some(CellKey::Exterior {
                worldspace_id: id.0,
                grid_x: snapshot.cell.0,
                grid_y: snapshot.cell.1,
            })
    }

    /// Only these cells lost or gained missing work. Pending IDs are never trimmed: a canceled
    /// receiver is replaced, while an unchanged receiver retains its original validation set.
    pub(super) fn reconcile_requests(&mut self, keys: HashSet<CellKey>) -> bool {
        if keys.is_empty() {
            return false;
        }

        let mut replacements = HashMap::new();
        for key in keys {
            let mut needed = BTreeSet::new();
            if let Some(ids) = self.desired_cells.get(&key) {
                let world = cell_world(key).expect("stock demand has an exterior cell");
                #[cfg(test)]
                {
                    self.planning_work.request_entries += ids.len() as u64;
                }
                // A cell retry cannot erase a conflict belonging to another identity.
                for &id in ids {
                    if self.duplicate_conflicts.contains_key(&(world, id)) {
                        self.failed.insert(
                            key,
                            format!(
                                "duplicate existing reference {id:08X} in placing world {world:08X}"
                            ),
                        );
                    }
                    if !self.owners.contains_key(&(world, id)) {
                        needed.insert(id);
                    }
                }
            }
            if self.failed.contains_key(&key) {
                self.pending.remove(&key);
                replacements.insert(key, BTreeSet::new());
                continue;
            }
            if self
                .pending
                .get(&key)
                .is_some_and(|pending| pending.ids == needed)
            {
                continue;
            }
            self.pending.remove(&key);
            replacements.insert(key, needed);
        }
        if replacements
            .keys()
            .any(|key| self.queued_keys.contains(key))
        {
            #[cfg(test)]
            {
                self.planning_work.queue_entries += self.queued.len() as u64;
            }
            self.queued.retain_mut(|(key, ids)| {
                if let Some(next) = replacements.remove(key) {
                    *ids = next;
                }
                if ids.is_empty() {
                    self.queued_keys.remove(key);
                    return false;
                }
                true
            });
        }
        let mut added = false;
        for (key, ids) in replacements {
            if !ids.is_empty() {
                self.queued_keys.insert(key);
                self.queued.push_back((key, ids));
                added = true;
            }
        }
        added
    }
}
