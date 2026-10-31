//! Reconcile native owner metadata after queued world edits, using raw component authority.

use super::*;

/// Keep lifecycle writers and ownership readers in the same admission layer.
/// The existing private domain journal remains installed while with_far borrows FarStock out.
pub(super) fn install(world: &mut World) {
    if world
        .resource::<domain::DomainChanges>()
        .owner_observers_installed
    {
        return;
    }
    let references = vec![
        world.register_component::<FormId>(),
        world.register_component::<CellRef>(),
        world.register_component::<ChildOf>(),
    ];
    let roots = vec![
        world.register_component::<StreamedCellKey>(),
        world.register_component::<StreamedCellRoot>(),
    ];
    let activity = vec![
        world.register_component::<Visibility>(),
        world.register_component::<RetireHidden>(),
    ];
    world.spawn(
        Observer::new(
            |event: On<Insert>, changes: Option<ResMut<domain::DomainChanges>>| {
                if let Some(mut changes) = changes {
                    changes.reference_changed(event.entity);
                }
            },
        )
        .with_components(references.clone()),
    );
    world.spawn(
        Observer::new(
            |event: On<Remove>, changes: Option<ResMut<domain::DomainChanges>>| {
                if let Some(mut changes) = changes {
                    changes.reference_changed(event.entity);
                    changes.removed.insert(event.entity);
                }
            },
        )
        .with_components(references),
    );
    world.spawn(
        Observer::new(
            |event: On<Insert>,
             distant: Query<(), With<DistantSourceRoot>>,
             changes: Option<ResMut<domain::DomainChanges>>| {
                if let Some(mut changes) = changes {
                    if distant.contains(event.entity) {
                        changes.roots.insert(event.entity);
                        changes.failure_dirty = true;
                    } else {
                        changes.root_changed(event.entity);
                    }
                }
            },
        )
        .with_components(roots.clone()),
    );
    world.spawn(
        Observer::new(
            |event: On<Remove>,
             distant: Query<(), With<DistantSourceRoot>>,
             changes: Option<ResMut<domain::DomainChanges>>| {
                if let Some(mut changes) = changes {
                    if distant.contains(event.entity) {
                        changes.roots.insert(event.entity);
                        changes.failure_dirty = true;
                    } else {
                        changes.root_changed(event.entity);
                    }
                    changes.removed.insert(event.entity);
                }
            },
        )
        .with_components(roots),
    );
    world.spawn(
        Observer::new(
            |event: On<Insert>,
             keyed: Query<(), (With<StreamedCellKey>, Without<DistantSourceRoot>)>,
             changes: Option<ResMut<domain::DomainChanges>>| {
                if keyed.contains(event.entity)
                    && let Some(mut changes) = changes
                {
                    changes.near_dirty = true;
                }
            },
        )
        .with_components(activity.clone()),
    );
    world.spawn(
        Observer::new(
            |event: On<Remove>,
             keyed: Query<(), (With<StreamedCellKey>, Without<DistantSourceRoot>)>,
             changes: Option<ResMut<domain::DomainChanges>>| {
                if keyed.contains(event.entity)
                    && let Some(mut changes) = changes
                {
                    changes.near_dirty = true;
                }
            },
        )
        .with_components(activity),
    );
    world.spawn(Observer::new(
        |_event: On<Insert, super::super::AssetFailed>,
         changes: Option<ResMut<domain::DomainChanges>>| {
            if let Some(mut changes) = changes {
                changes.failure_dirty = true;
            }
        },
    ));
    world.spawn(Observer::new(
        |_event: On<Remove, super::super::AssetFailed>,
         changes: Option<ResMut<domain::DomainChanges>>| {
            if let Some(mut changes) = changes {
                changes.failure_dirty = true;
            }
        },
    ));
    let mut changes = world.resource_mut::<domain::DomainChanges>();
    changes.owner_observers_installed = true;
    // Explicitly close the installation gap, including components predating installation.
    changes.baseline = true;
    changes.near_dirty = true;
    changes.failure_dirty = true;
}

pub(super) fn reconcile_changed_owner(world: &mut World, far: &mut FarStock, id: Identity) {
    let Some(owner) = far.owners.get(&id).cloned() else {
        return;
    };
    #[cfg(test)]
    {
        far.planning_work.owner_entries += 1;
    }
    let current = (|| {
        let package = far.package.as_ref()?;
        let reference = package.placed_reference(id.1)?;
        if world.get::<FormId>(owner.entity)?.0 != id.1
            || world.get::<CellRef>(owner.entity)?.0 != reference.snapshot.source_cell_id
            || world.get::<StreamedCellKey>(owner.entity).is_some()
        {
            return None;
        }
        let parent = world.get::<ChildOf>(owner.entity)?.parent();
        let key = world.get::<StreamedCellKey>(parent)?.0;
        if cell_world(key) != Some(id.0) || reference.snapshot.placed_worldspace_id != id.0 {
            return None;
        }
        if owner.far && parent == owner.parent && key == owner.key {
            return Some(owner.clone());
        }
        world.get::<crate::world::components::StreamedCellRoot>(parent)?;
        let mut next = owner.clone();
        next.parent = parent;
        next.key = key;
        next.near_root = Some(parent);
        next.near_key = Some(key);
        next.far = false;
        Some(next)
    })();
    if let Some(next) = current {
        if far.duplicate_conflicts.contains_key(&id) {
            far.remove_owner(&id);
            return;
        }
        if next.parent != owner.parent
            || next.key != owner.key
            || next.near_root != owner.near_root
            || next.near_key != owner.near_key
            || next.far != owner.far
        {
            if !next.far {
                far.near_roots
                    .retain(|key, root| *root != next.parent || *key == next.key);
                far.near_roots.insert(next.key, next.parent);
            }
            far.put_owner(id, next);
        }
    } else {
        far.remove_owner(&id);
        if let Some(package) = far.package.clone() {
            #[cfg(test)]
            {
                far.planning_work.bounded_entries += 1;
            }
            if let Some(key) = far.bounded_key(
                &package,
                far.demand,
                far.stream_radius,
                id,
                world.get_resource::<ReferenceEnableState>(),
                false,
            ) {
                far.bounded_desired.insert(id, key);
            } else {
                far.bounded_desired.remove(&id);
            }
            far.project_union(HashSet::from([id]));
            far.owner_dirty.remove(&id);
        }
        ownership_failed(
            world,
            far,
            &owner,
            "owned source identity or ancestry changed incompatibly".into(),
        );
    }
}
