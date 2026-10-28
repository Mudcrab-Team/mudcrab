// Flat stock extraction regressions, included in handoff::extract_source_tests.

fn flatten_stock(input: &mut Fixture, primitive: Entity) -> GlobalTransform {
    let matrix = input
        .app
        .world()
        .get::<SourcePrimitiveTransform>(primitive)
        .unwrap()
        .0;
    // Deliberately sheared: preserving this affine must never decompose it into TRS.
    let world_matrix = Mat4::from_cols(
        Vec4::new(1.0, 0.25, 0.0, 0.0),
        Vec4::new(0.5, 1.0, 0.0, 0.0),
        Vec4::Z,
        Vec4::new(100.0, 10.0, -100.0, 1.0),
    ) * matrix;
    let global = GlobalTransform::from(world_matrix);
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .remove::<(ChildOf, Transform)>()
        .insert((
            FlatSourceOwner(input.root),
            FlatSourcePrimitive {
                model_matrix: matrix,
                expected_global: global,
            },
            global,
        ));
    global
}

#[test]
fn flat_source_exact_affine_binds_and_global_edits_invalidate_incrementally() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input.assert_bound(primitive);
    let global = flatten_stock(&mut input, primitive);
    input.assert_bound(primitive);
    assert_delta_only(&input);
    assert_full_oracle(&mut input);
    assert!(input.app.world().get::<Transform>(primitive).is_none());
    assert!(input.app.world().get::<ChildOf>(primitive).is_none());

    // Mutate the component in place: lifecycle messages alone cannot catch this.
    *input
        .app
        .world_mut()
        .get_mut::<GlobalTransform>(primitive)
        .unwrap() = GlobalTransform::from_translation(Vec3::X);
    input.assert_blocked();
    assert_delta_only(&input);
    assert_full_oracle(&mut input);
    input.app.world_mut().entity_mut(primitive).insert(global);
    input.assert_bound(primitive);
    assert_delta_only(&input);
}

#[test]
fn flat_source_requires_original_model_matrix_and_normal_root_pose() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    flatten_stock(&mut input, primitive);
    input.assert_bound(primitive);
    let metadata = input
        .app
        .world()
        .get::<FlatSourcePrimitive>(primitive)
        .unwrap()
        .clone();
    input
        .app
        .world_mut()
        .get_mut::<FlatSourcePrimitive>(primitive)
        .unwrap()
        .model_matrix = metadata.model_matrix * Mat4::from_translation(Vec3::X);
    input.assert_blocked();
    assert_delta_only(&input);
    input.app.world_mut().entity_mut(primitive).insert(metadata);
    input.assert_bound(primitive);
    input
        .app
        .world_mut()
        .get_mut::<Transform>(input.root)
        .unwrap()
        .translation
        .x += 1.0;
    input.assert_blocked();
    assert_delta_only(&input);
    assert_full_oracle(&mut input);
}

#[test]
fn flat_source_owner_reparent_and_removal_require_a_valid_new_root() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    flatten_stock(&mut input, primitive);
    input.assert_bound(primitive);
    let other = input
        .app
        .world_mut()
        .spawn((FormId(REFERENCE + 1), Transform::IDENTITY))
        .id();
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .insert(FlatSourceOwner(other));
    input.assert_blocked();
    assert_delta_only(&input);
    assert_full_oracle(&mut input);
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .insert(FlatSourceOwner(input.root));
    input.assert_bound(primitive);
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .remove::<FlatSourceOwner>();
    input.assert_blocked();
    assert_delta_only(&input);
    assert_full_oracle(&mut input);
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .insert(FlatSourceOwner(input.root));
    input.assert_bound(primitive);
}

#[test]
fn flat_source_cannot_override_child_of_or_keep_a_transform() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    flatten_stock(&mut input, primitive);
    input.assert_bound(primitive);
    let baseline = input
        .app
        .world()
        .get::<SourceNodePose>(primitive)
        .unwrap()
        .0;
    input.app.world_mut().entity_mut(primitive).insert(baseline);
    input.assert_blocked();
    assert_delta_only(&input);
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .remove::<Transform>();
    input.assert_bound(primitive);
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .insert(ChildOf(input.root));
    input.assert_blocked();
    assert_delta_only(&input);
    assert_full_oracle(&mut input);
}

#[test]
fn flat_source_metadata_and_global_removal_revoke_binding() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    let global = flatten_stock(&mut input, primitive);
    input.assert_bound(primitive);
    let metadata = input
        .app
        .world()
        .get::<FlatSourcePrimitive>(primitive)
        .unwrap()
        .clone();
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .remove::<FlatSourcePrimitive>();
    input.assert_blocked();
    assert_delta_only(&input);
    input.app.world_mut().entity_mut(primitive).insert(metadata);
    input.assert_bound(primitive);
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .remove::<GlobalTransform>();
    input.assert_blocked();
    assert_delta_only(&input);
    assert_full_oracle(&mut input);
    input.app.world_mut().entity_mut(primitive).insert(global);
    input.assert_bound(primitive);
}

#[test]
fn generated_scene_root_qualifies_only_with_its_original_source_path() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    flatten_stock(&mut input, primitive);
    input.assert_bound(primitive);
    let original = input
        .app
        .world()
        .get::<WorldAssetRoot>(input.root)
        .unwrap()
        .0
        .clone();
    let generated = input
        .app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .add(WorldAsset::new(World::new()));
    assert!(generated.path().is_none());
    input
        .app
        .world_mut()
        .entity_mut(input.root)
        .insert(WorldAssetRoot(generated));
    input.assert_blocked();
    input
        .app
        .world_mut()
        .entity_mut(input.root)
        .insert(OriginalStockScene(original.clone()));
    input.assert_bound(primitive);
    assert_delta_only(&input);
    assert_full_oracle(&mut input);
    input
        .app
        .world_mut()
        .entity_mut(input.root)
        .remove::<OriginalStockScene>();
    input.assert_blocked();
    assert_delta_only(&input);
    // An original-handle edit is observed even when the component remains present.
    input
        .app
        .world_mut()
        .entity_mut(input.root)
        .insert(OriginalStockScene(Handle::default()));
    input.assert_blocked();
    input
        .app
        .world_mut()
        .get_mut::<OriginalStockScene>(input.root)
        .unwrap()
        .0 = original;
    input.assert_bound(primitive);
    assert_delta_only(&input);
}
