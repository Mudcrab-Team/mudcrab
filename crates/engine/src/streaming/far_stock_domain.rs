//! Native default-query-domain changes for the far-stock near-source index.
//! Marker insertion/removal can change query membership without changing reference metadata.

use super::*;
use bevy::ecs::{component::ComponentId, entity_disabling::DefaultQueryFilters};

#[derive(Resource, Default)]
pub(super) struct DomainChanges {
    covered: HashSet<ComponentId>,
    pub(super) dirty: HashSet<Entity>,
    pub(super) baseline: bool,
    pub(super) owner_observers_installed: bool,
    pub(super) references: HashSet<Entity>,
    pub(super) roots: HashSet<Entity>,
    pub(super) removed: HashSet<Entity>,
    // Independent consumers: near refresh must never acknowledge a failure wakeup.
    pub(super) near_dirty: bool,
    pub(super) failure_dirty: bool,
}

impl DomainChanges {
    /// In-place metadata writers must notify here; Insert/Remove writers are observed.
    pub(super) fn reference_changed(&mut self, entity: Entity) {
        self.references.insert(entity);
        self.failure_dirty = true;
    }

    pub(super) fn root_changed(&mut self, entity: Entity) {
        self.roots.insert(entity);
        self.near_dirty = true;
        self.failure_dirty = true;
    }

    pub(super) fn covers(&self, filters: &DefaultQueryFilters) -> bool {
        self.owner_observers_installed
            && filters.disabling_ids().all(|id| self.covered.contains(&id))
    }
}

pub(super) fn install(world: &mut World) {
    world.init_resource::<DomainChanges>();
    owner_changes::install(world);
    let ids: Vec<_> = world
        .resource::<DefaultQueryFilters>()
        .disabling_ids()
        .filter(|id| !world.resource::<DomainChanges>().covered.contains(id))
        .collect();
    if ids.is_empty() {
        return;
    }
    world.spawn(
        Observer::new(
            |event: On<Insert>, changes: Option<ResMut<DomainChanges>>| {
                if let Some(mut changes) = changes {
                    changes.dirty.insert(event.entity);
                    changes.near_dirty = true;
                    changes.failure_dirty = true;
                }
            },
        )
        .with_components(ids.clone()),
    );
    world.spawn(
        Observer::new(
            |event: On<Remove>, changes: Option<ResMut<DomainChanges>>| {
                if let Some(mut changes) = changes {
                    changes.dirty.insert(event.entity);
                    changes.near_dirty = true;
                    changes.failure_dirty = true;
                }
            },
        )
        .with_components(ids.clone()),
    );
    let mut changes = world.resource_mut::<DomainChanges>();
    changes.covered.extend(ids);
    // Snapshot again after installation: no mutation between the earlier planner read and
    // this deferred observer registration may escape the first retained baseline.
    changes.baseline = true;
    changes.near_dirty = true;
    changes.failure_dirty = true;
}
