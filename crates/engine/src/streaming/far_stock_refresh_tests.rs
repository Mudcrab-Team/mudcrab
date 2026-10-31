//! Include as a child module of `far_stock::tests` to reuse its producer-backed fixtures.

use super::*;

fn holder_changes(world: &World, holder: Entity) -> (bool, bool) {
    let entity = world.entity(holder);
    (
        entity.get_ref::<Visibility>().unwrap().is_changed(),
        entity
            .get_ref::<Propagate<RenderLayers>>()
            .unwrap()
            .is_changed(),
    )
}

#[test]
fn adopting_one_source_preserves_its_payload_without_changing_an_unrelated_filled_holder() {
    let fixture = Fixture::new();
    let (key, row, pose) = fixture.selected();
    let (other_key, other_row) = fixture
        .rows
        .iter()
        .find(|(other_key, other_row)| {
            *other_key != key
                && cell_world(*other_key) == cell_world(key)
                && other_row.model_path.is_some()
        })
        .expect("producer inventory has another model-bearing exterior cell");
    let other_key = *other_key;
    let identity = (cell_world(key).unwrap(), row.form_id);
    let other_identity = (cell_world(other_key).unwrap(), other_row.form_id);

    let mut world = world();
    let old_holder = root(&mut world, key, true);
    sync_holder_layers(&mut world, old_holder);
    let (owner, model) = source(&mut world, key, old_holder, &row, true);
    let entity = owner.entity;
    let handle = world.get::<PendingModel>(entity).unwrap().handle.id();
    let started = world.get::<PendingAssetProfile>(entity).unwrap().started;
    let path = world
        .get::<PendingAssetProfile>(entity)
        .unwrap()
        .path
        .clone();
    let collider = world.get::<Collider>(entity).unwrap().raw.clone();
    assert!(world.get::<ColliderDisabled>(entity).is_some());

    let other_holder = root(&mut world, other_key, true);
    sync_holder_layers(&mut world, other_holder);
    let (other_owner, other_model) = source(&mut world, other_key, other_holder, other_row, true);
    let other_entity = other_owner.entity;
    let other_visibility = *world.get::<Visibility>(other_holder).unwrap();
    let other_layers = world
        .get::<Propagate<RenderLayers>>(other_holder)
        .unwrap()
        .0
        .clone();
    let near = root(&mut world, key, false);
    let mut far = state(&fixture, key, owner);
    far.holders.insert(key, old_holder);
    far.holders.insert(other_key, other_holder);
    far.desired.insert(other_identity, other_key);
    far.put_owner(other_identity, other_owner);

    // Reserve through the same payload-filtering path used by near-cell admission.
    let mut payload = vec![row.clone()];
    let adoptions =
        far.take_adoptions(key, &mut payload, |entity| world.get_entity(entity).is_ok());
    assert_eq!(adoptions, [row.form_id]);
    assert!(payload.is_empty());
    world.insert_resource(far);
    world.flush();
    world.clear_trackers();
    assert_eq!(holder_changes(&world, other_holder), (false, false));
    let entity_count = world.entity_count();

    queue_adopt(&mut world.commands(), near, key, adoptions);
    world.flush();

    let far = world.resource::<FarStock>();
    assert_eq!(far.owners.len(), 2);
    let owner = &far.owners[&identity];
    assert_eq!(owner.entity, entity);
    assert_eq!(owner.parent, near);
    assert_eq!(owner.key, key);
    assert_eq!(owner.near_key, Some(key));
    assert_eq!(owner.near_root, Some(near));
    assert!(!owner.far);
    assert_eq!(world.get::<FormId>(entity).unwrap().0, row.form_id);
    assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), near);
    assert_eq!(world.get::<ChildOf>(model).unwrap().parent(), entity);
    assert_eq!(
        world.get::<PendingModel>(entity).unwrap().handle.id(),
        handle
    );
    let profile = world.get::<PendingAssetProfile>(entity).unwrap();
    assert_eq!(profile.started, started);
    assert_eq!(profile.path, path);
    assert!(Arc::ptr_eq(
        &world.get::<Collider>(entity).unwrap().raw.0,
        &collider.0,
    ));
    // Near ownership enables the retained collider rather than replacing it.
    assert!(world.get::<ColliderDisabled>(entity).is_none());
    assert_placement(&world, entity, pose, key);
    assert!(world.get_entity(old_holder).is_err());
    assert!(!far.holders.contains_key(&key));
    assert_eq!(world.entity_count(), entity_count - 1);

    let other_owner = &far.owners[&other_identity];
    assert_eq!(other_owner.entity, other_entity);
    assert_eq!(other_owner.parent, other_holder);
    assert!(other_owner.far);
    assert_eq!(far.holders[&other_key], other_holder);
    assert_eq!(
        world.get::<ChildOf>(other_entity).unwrap().parent(),
        other_holder
    );
    assert_eq!(
        world.get::<ChildOf>(other_model).unwrap().parent(),
        other_entity
    );
    assert_no_exit(&world);

    assert_eq!(
        *world.get::<Visibility>(other_holder).unwrap(),
        other_visibility
    );
    assert_eq!(
        world
            .get::<Propagate<RenderLayers>>(other_holder)
            .unwrap()
            .0,
        other_layers
    );
    // Equal values still dirty downstream consumers if a full refresh reinserts them.
    assert_eq!(
        holder_changes(&world, other_holder),
        (false, false),
        "adopting one source must not mark an unrelated filled holder's Visibility or Propagate<RenderLayers> Changed",
    );
}
