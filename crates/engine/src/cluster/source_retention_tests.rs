// Staged for inclusion inside handoff::extract_source_tests, alongside
// source_observation_tests.rs. Uses its real GLTF Fixture and observation helpers.
// Counter snapshots precede the oracle: a full extraction repairs cache state.

#[test]
fn retention_raw_baseline_change_restore_and_later_pose_edit_remain_observable() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input.assert_bound(primitive);
    input.assert_bound(primitive);
    let baseline = *input.app.world().get::<SourceNodePose>(primitive).unwrap();
    let pose = *input.app.world().get::<Transform>(primitive).unwrap();
    let transform_tick = input
        .app
        .world()
        .entity(primitive)
        .get_change_ticks::<Transform>()
        .unwrap()
        .changed;

    // SourceNodePose is a raw fact query input, not a hot-query Transform write.
    input
        .app
        .world_mut()
        .get_mut::<SourceNodePose>(primitive)
        .unwrap()
        .0
        .translation
        .x += 1.0;
    assert_eq!(
        input
            .app
            .world()
            .entity(primitive)
            .get_change_ticks::<Transform>()
            .unwrap()
            .changed,
        transform_tick
    );
    input.assert_blocked();
    let invalidated = input.sources().last_work;
    assert_delta_only(&input);
    assert!(invalidated.fact_entities_yielded > 0);
    assert!(
        invalidated.hot_fact_cache_reads > 0,
        "a dirty raw fact must prevent retaining the old hot snapshot"
    );

    *input
        .app
        .world_mut()
        .get_mut::<SourceNodePose>(primitive)
        .unwrap() = baseline;
    input.assert_bound(primitive);
    let restored = input.sources().last_work;
    assert_delta_only(&input);
    assert!(restored.hot_fact_cache_reads > 0);

    input
        .app
        .world_mut()
        .get_mut::<Transform>(primitive)
        .unwrap()
        .translation
        .x += 2.0;
    input.assert_blocked();
    let edited = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(edited.changed_entities, 1);
    assert_eq!(edited.hot_fact_change_reads, 1);
    assert_eq!(
        edited.hot_fact_cache_reads, 0,
        "reuse this extraction's observation; retain unchanged ancestors"
    );
    assert_eq!(edited.hot_fact_reads, 1);
    assert_full_oracle(&mut input);

    *input
        .app
        .world_mut()
        .get_mut::<Transform>(primitive)
        .unwrap() = pose;
    input.assert_bound(primitive);
    let recovered = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(recovered.hot_fact_reads, 1);
    assert_full_oracle(&mut input);
}

#[test]
fn retention_form_id_policy_changes_do_not_leave_a_literal_nonbaseline_pose() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input.assert_bound(primitive);
    let baseline = *input.app.world().get::<SourceNodePose>(primitive).unwrap();
    let pose = *input.app.world().get::<Transform>(primitive).unwrap();
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .remove::<SourceNodePose>();
    input
        .app
        .world_mut()
        .get_mut::<Transform>(primitive)
        .unwrap()
        .translation
        .x += 1.0;
    input.assert_blocked();

    // Every FormId is a native root barrier, even an unrelated/incomplete root.
    // Its pose is literal; an ordinary unbaselined mesh instead canonicalizes
    // every pose to None. Neither state can qualify this stock primitive.
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .insert(FormId(u32::MAX));
    // Same extraction changes role and pose. Compare under the previous role,
    // but publish a snapshot under the new role before the next raw edit.
    input
        .app
        .world_mut()
        .get_mut::<Transform>(primitive)
        .unwrap()
        .translation
        .x += 1.0;
    input.assert_blocked();
    let added = input.sources().last_work;
    assert_delta_only(&input);
    assert!(
        added.hot_fact_cache_reads > 0,
        "the old nonliteral snapshot cannot survive a root-role transition"
    );

    input
        .app
        .world_mut()
        .get_mut::<Transform>(primitive)
        .unwrap()
        .translation
        .x += 2.0;
    input.assert_blocked();
    let literal_edit = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(literal_edit.changed_entities, 1);
    assert!(literal_edit.subjects_updated > 0);

    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .remove::<FormId>();
    // Same extraction changes role and pose. Compare under the previous role,
    // but publish a snapshot under the new role before the next raw edit.
    input
        .app
        .world_mut()
        .get_mut::<Transform>(primitive)
        .unwrap()
        .translation
        .x += 1.0;
    input.assert_blocked();
    let removed = input.sources().last_work;
    assert_delta_only(&input);
    assert!(
        removed.hot_fact_cache_reads > 0,
        "removing the root role must install the nonliteral policy"
    );
    let refreshes = input.sources().qualification_refreshes;
    for offset in [3.0, 4.0] {
        input
            .app
            .world_mut()
            .get_mut::<Transform>(primitive)
            .unwrap()
            .translation
            .x += offset;
        input.assert_blocked();
        let refused = input.sources().last_work;
        assert_delta_only(&input);
        assert_eq!(refused.hot_fact_change_reads, 1);
        assert_eq!(
            refused.changed_entities, 0,
            "different bad poses share the semantic canonical None"
        );
        assert_eq!(refused.subjects_updated, 0);
        assert_eq!(refused.hot_fact_cache_reads, 0);
        assert_eq!(input.sources().qualification_refreshes, refreshes);
    }
    assert_full_oracle(&mut input);

    input.app.world_mut().entity_mut(primitive).insert(baseline);
    *input
        .app
        .world_mut()
        .get_mut::<Transform>(primitive)
        .unwrap() = pose;
    input.assert_bound(primitive);
    let recovered = input.sources().last_work;
    assert_delta_only(&input);
    assert!(recovered.changed_entities > 0);
    assert_full_oracle(&mut input);
}

#[test]
fn retention_reenabled_root_reads_raw_pose_identity_and_material_edits() {
    use bevy::ecs::entity_disabling::Disabled;

    for variant in 0..3 {
        let mut input = Fixture::pending();
        let primitive = input.spawn_stock();
        // This headless GLTF fixture does not install the PBR material loader.
        // The root-only material observation needs its own explicit asset.
        let original_material = MeshMaterial3d(
            input
                .app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial::default()),
        );
        let replacement = input
            .app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        input
            .app
            .world_mut()
            .entity_mut(input.root)
            .insert(original_material.clone());
        input.assert_bound(primitive);
        let cell_ref = *input.app.world().get::<CellRef>(input.root).unwrap();
        input
            .app
            .world_mut()
            .entity_mut(input.root)
            .insert(Disabled);
        input.assert_blocked();
        assert_delta_only(&input);

        // Raw World access sees a disabled entity; native dependency/fact/hot
        // queries do not. No App update or producer hook repairs these values.
        match variant {
            0 => {
                input
                    .app
                    .world_mut()
                    .get_mut::<Transform>(input.root)
                    .unwrap()
                    .translation
                    .x += 1.0
            }
            1 => {
                input
                    .app
                    .world_mut()
                    .get_mut::<CellRef>(input.root)
                    .unwrap()
                    .0 += 1
            }
            2 => {
                input
                    .app
                    .world_mut()
                    .get_mut::<MeshMaterial3d<StandardMaterial>>(input.root)
                    .unwrap()
                    .0 = replacement.clone()
            }
            _ => unreachable!(),
        }
        input.assert_blocked();
        let hidden = input.sources().last_work;
        assert_delta_only(&input);
        assert_eq!(hidden.changed_entities, 0, "hidden variant {variant}");

        input
            .app
            .world_mut()
            .entity_mut(input.root)
            .remove::<Disabled>();
        if variant == 2 {
            // Root material handles are hot facts, but do not revoke the
            // descendant's stock geometry proof by themselves. The equal raw
            // rewrite below proves the newly visible snapshot was installed.
            input.assert_bound(primitive);
        } else {
            input.assert_blocked();
        }
        let reenabled = input.sources().last_work;
        assert_delta_only(&input);
        assert!(reenabled.hot_fact_reads > 0, "reenabled variant {variant}");
        assert!(reenabled.subjects_updated > 0);

        if variant == 2 {
            let refreshes = input.sources().qualification_refreshes;
            input
                .app
                .world_mut()
                .get_mut::<MeshMaterial3d<StandardMaterial>>(input.root)
                .unwrap()
                .0 = replacement;
            input.assert_bound(primitive);
            let equal = input.sources().last_work;
            assert_delta_only(&input);
            assert_eq!(equal.changed_entities, 0);
            assert_eq!(equal.hot_fact_cache_reads, 0);
            assert_eq!(input.sources().qualification_refreshes, refreshes);
        }
        assert_full_oracle(&mut input);

        match variant {
            0 => {
                *input
                    .app
                    .world_mut()
                    .get_mut::<Transform>(input.root)
                    .unwrap() = input.root_pose
            }
            1 => {
                *input
                    .app
                    .world_mut()
                    .get_mut::<CellRef>(input.root)
                    .unwrap() = cell_ref
            }
            2 => {
                *input
                    .app
                    .world_mut()
                    .get_mut::<MeshMaterial3d<StandardMaterial>>(input.root)
                    .unwrap() = original_material
            }
            _ => unreachable!(),
        }
        input.assert_bound(primitive);
        let recovered = input.sources().last_work;
        assert_delta_only(&input);
        assert!(recovered.changed_entities > 0);
        assert_full_oracle(&mut input);
    }
}

#[test]
fn retention_missed_cell_rebase_and_same_tick_authored_draw_need_separate_repairs() {
    let (mut input, primitive, mut draws) = observation_population(16);
    observation_rebase(&mut input, &mut draws, IVec2::new(-1, 1));
    input.assert_bound(primitive);
    let exact = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(exact.changed_entities, 0);
    assert_eq!(exact.hot_fact_reads, draws.len() as u64 + 1);
    assert_eq!(
        exact.hot_fact_origin_reads, 0,
        "the native cell observation also serves the origin guard"
    );
    assert_eq!(exact.hot_fact_cache_reads, 0);

    let origin = IVec2::new(2, -3);
    let shift = crate::streaming::grid_translation(input.sources().origin, origin);
    input.app.insert_resource(RenderOrigin(origin));
    // Unlike observation_rebase, deliberately leave the cell untouched: only
    // the RenderOrigin guard can notice its missed rebase without a cell tick.
    for (entity, global) in &mut draws {
        *global = GlobalTransform::from(Mat4::from_translation(shift) * global.to_matrix());
        *input
            .app
            .world_mut()
            .get_mut::<GlobalTransform>(*entity)
            .unwrap() = *global;
        input
            .app
            .world_mut()
            .get_mut::<FlatSourcePrimitive>(*entity)
            .unwrap()
            .expected_global = *global;
    }
    let rebase_tick = input
        .app
        .world()
        .entity(primitive)
        .get_change_ticks::<GlobalTransform>()
        .unwrap()
        .changed;
    *input
        .app
        .world_mut()
        .get_mut::<GlobalTransform>(primitive)
        .unwrap() = GlobalTransform::from_translation(Vec3::X) * draws[0].1;
    let authored_tick = input
        .app
        .world()
        .entity(primitive)
        .get_change_ticks::<GlobalTransform>()
        .unwrap()
        .changed;
    assert_eq!(
        rebase_tick, authored_tick,
        "identical native ticks do not certify the final authored value"
    );
    input.assert_blocked();
    let mixed = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(mixed.changed_entities, 2);
    assert_eq!(mixed.hot_fact_change_reads, draws.len() as u64);
    assert_eq!(mixed.hot_fact_origin_reads, 1);
    assert_eq!(mixed.hot_fact_cache_reads, 0);
    assert_eq!(mixed.hot_fact_reads, draws.len() as u64 + 1);

    *input
        .app
        .world_mut()
        .get_mut::<GlobalTransform>(primitive)
        .unwrap() = draws[0].1;
    input.assert_blocked();
    let draw_repaired = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(draw_repaired.changed_entities, 1);
    assert_eq!(draw_repaired.hot_fact_reads, 1);
    assert_eq!(draw_repaired.hot_fact_cache_reads, 0);

    *input
        .app
        .world_mut()
        .get_mut::<Transform>(input.cell)
        .unwrap() =
        Transform::from_translation(crate::streaming::grid_translation(IVec2::ZERO, origin));
    input.assert_bound(primitive);
    let cell_repaired = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(cell_repaired.changed_entities, 1);
    assert_eq!(cell_repaired.hot_fact_reads, 1);
    assert_eq!(cell_repaired.hot_fact_cache_reads, 0);
    assert_full_oracle(&mut input);
}

#[test]
fn retention_newly_indexed_wrapper_is_sampled_then_observed_before_any_oracle() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input.assert_bound(primitive);
    let parent = input
        .app
        .world()
        .get::<ChildOf>(primitive)
        .unwrap()
        .parent();
    let indexed = input.sources().last_work.indexed_nodes;
    let wrapper = input
        .app
        .world_mut()
        .spawn((
            Transform::IDENTITY,
            SourceNodePose(Transform::IDENTITY),
            ChildOf(parent),
        ))
        .id();
    input.assert_bound(primitive);
    assert_eq!(
        input.sources().last_work.indexed_nodes,
        indexed,
        "a meshless nonroot wrapper is not yet an indexed dependency"
    );

    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .insert(ChildOf(wrapper));
    // Same known identity-wrapper pattern as the existing ancestry regression.
    input.assert_bound(primitive);
    let reparented = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(reparented.indexed_nodes, indexed + 1);
    assert!(
        reparented.hot_fact_cache_reads > 0,
        "unobserved ancestors without an existing snapshot need a fresh read"
    );

    input
        .app
        .world_mut()
        .get_mut::<Transform>(wrapper)
        .unwrap()
        .translation
        .x += 1.0;
    input.assert_blocked();
    let edited = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(edited.changed_entities, 1);
    assert_eq!(edited.hot_fact_reads, 1);
    assert_eq!(edited.hot_fact_cache_reads, 0);
    let refreshes = input.sources().qualification_refreshes;
    input
        .app
        .world_mut()
        .get_mut::<Transform>(wrapper)
        .unwrap()
        .translation
        .x += 2.0;
    input.assert_blocked();
    let refused = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(refused.changed_entities, 0);
    assert_eq!(input.sources().qualification_refreshes, refreshes);

    *input.app.world_mut().get_mut::<Transform>(wrapper).unwrap() = Transform::IDENTITY;
    input.assert_bound(primitive);
    let recovered = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(recovered.hot_fact_reads, 1);
    assert_full_oracle(&mut input);
}

#[test]
fn retention_replaced_journal_recovers_hidden_raw_fact_with_unrelated_suffix() {
    use crate::streaming::identity_changes::{SourceNodeChanged, StructureEpoch};
    use bevy::ecs::entity_disabling::Disabled;

    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input.assert_bound(primitive);
    let baseline = *input.app.world().get::<SourceNodePose>(primitive).unwrap();
    input.app.world_mut().entity_mut(primitive).insert(Disabled);
    input.assert_blocked();
    let mut stale = input
        .app
        .world()
        .resource::<Messages<SourceNodeChanged>>()
        .get_cursor_current();
    input
        .app
        .world_mut()
        .get_mut::<SourceNodePose>(primitive)
        .unwrap()
        .0
        .translation
        .x += 1.0;
    input.assert_blocked();
    assert_delta_only(&input);

    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .remove::<Disabled>();
    let unrelated =
        usize::try_from(input.app.world().resource::<StructureEpoch>().version).unwrap() + 1;
    input
        .app
        .world_mut()
        .insert_resource(Messages::<SourceNodeChanged>::default());
    // Follow the existing replacement-journal test: the suffix is long enough
    // that message-count checks alone cannot detect the missing epoch edge.
    for _ in 0..unrelated {
        input.app.world_mut().spawn(Transform::IDENTITY);
    }
    let journal = input.app.world().resource::<Messages<SourceNodeChanged>>();
    assert_eq!(stale.missed_messages(journal), 0);
    let surviving: Vec<_> = stale.read(journal).copied().collect();
    assert!(!surviving.is_empty());
    assert!(surviving.iter().all(|event| event.entity != primitive));
    input.assert_blocked();
    let recovery = input.sources().last_work;
    assert!(recovery.qualification_full);
    assert!(!recovery.journal_complete);
    assert!(recovery.journal_events > 0);
    assert!(
        recovery.hot_fact_cache_reads > 0,
        "full recovery must freshly sample unobserved cached ancestors"
    );

    // Recovery must resynchronize before another delta, without an oracle.
    *input
        .app
        .world_mut()
        .get_mut::<SourceNodePose>(primitive)
        .unwrap() = baseline;
    input.assert_bound(primitive);
    let restored = input.sources().last_work;
    assert_delta_only(&input);
    assert!(restored.journal_complete);
    assert!(restored.hot_fact_cache_reads > 0);
    input
        .app
        .world_mut()
        .get_mut::<Transform>(primitive)
        .unwrap()
        .translation
        .x += 1.0;
    input.assert_blocked();
    let edited = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(edited.hot_fact_reads, 1);
    assert_eq!(edited.hot_fact_cache_reads, 0);
    assert_full_oracle(&mut input);
}

#[test]
fn retention_raw_cell_key_change_and_following_pose_edits_stay_observable() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input.assert_bound(primitive);
    input.assert_bound(primitive);
    let key = *input
        .app
        .world()
        .get::<StreamedCellKey>(input.cell)
        .unwrap();
    let pose = *input.app.world().get::<Transform>(input.cell).unwrap();
    let pose_tick = input
        .app
        .world()
        .entity(input.cell)
        .get_change_ticks::<Transform>()
        .unwrap()
        .changed;
    let mut other = key;
    let CellKey::Exterior { grid_x, .. } = &mut other.0 else {
        panic!("fixture must be exterior");
    };
    *grid_x += 1;
    *input
        .app
        .world_mut()
        .get_mut::<StreamedCellKey>(input.cell)
        .unwrap() = other;
    assert_eq!(
        input
            .app
            .world()
            .entity(input.cell)
            .get_change_ticks::<Transform>()
            .unwrap()
            .changed,
        pose_tick
    );
    input.assert_blocked();
    let changed = input.sources().last_work;
    assert_delta_only(&input);
    assert!(changed.fact_entities_yielded > 0);
    assert_eq!(changed.hot_fact_change_reads, 0);
    assert_eq!(changed.hot_fact_cache_reads, 1);

    *input
        .app
        .world_mut()
        .get_mut::<StreamedCellKey>(input.cell)
        .unwrap() = key;
    input.assert_bound(primitive);
    assert_eq!(input.sources().last_work.hot_fact_cache_reads, 1);
    input
        .app
        .world_mut()
        .get_mut::<Transform>(input.cell)
        .unwrap()
        .rotation = Quat::from_rotation_z(0.5);
    input.assert_blocked();
    assert_eq!(input.sources().last_work.hot_fact_reads, 1);
    *input
        .app
        .world_mut()
        .get_mut::<Transform>(input.cell)
        .unwrap() = pose;
    input.assert_bound(primitive);
    assert_eq!(input.sources().last_work.hot_fact_reads, 1);
    assert_full_oracle(&mut input);
}

#[test]
fn crossing_observation_rebase_preserves_authored_cell_rotation_failure() {
    let (mut input, primitive, mut draws) = observation_population(16);
    input
        .app
        .world_mut()
        .get_mut::<Transform>(input.cell)
        .unwrap()
        .rotation = Quat::from_rotation_y(0.5);
    observation_rebase(&mut input, &mut draws, IVec2::new(-3, 2));
    input.assert_blocked();
    let work = input.sources().last_work;
    assert_delta_only(&input);
    assert_eq!(work.changed_entities, 1);
    assert_eq!(work.hot_fact_reads, draws.len() as u64 + 1);
    assert_eq!(work.hot_fact_cache_reads, 0);
    input
        .app
        .world_mut()
        .get_mut::<Transform>(input.cell)
        .unwrap()
        .rotation = Quat::IDENTITY;
    input.assert_bound(primitive);
    assert_eq!(input.sources().last_work.hot_fact_reads, 1);
    assert_full_oracle(&mut input);
}
