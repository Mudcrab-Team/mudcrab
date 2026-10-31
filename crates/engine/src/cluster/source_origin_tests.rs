// Origin changes preserve source proof only when the cell follows the exact rebase.

include!("source_observation_tests.rs");
include!("source_retention_tests.rs");

#[test]
fn consecutive_exact_origin_rebases_reuse_source_qualification() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input.assert_bound(primitive);
    let refreshes = input.sources().qualification_refreshes;
    for origin in [IVec2::new(2, -3), IVec2::new(-4, 7), IVec2::ZERO] {
        input.app.insert_resource(RenderOrigin(origin));
        input
            .app
            .world_mut()
            .entity_mut(input.cell)
            .insert(Transform::from_translation(
                crate::streaming::grid_translation(IVec2::ZERO, origin),
            ));
        input.assert_bound(primitive);
        assert_eq!(
            input.sources().qualification_refreshes,
            refreshes,
            "an exact cell rebase must reuse source qualification"
        );
        assert_eq!(input.sources().last_work.roots_scanned, 0);
        assert_eq!(input.sources().last_work.meshes_scanned, 0);
        assert_eq!(input.sources().origin, origin);
    }
    // A full oracle refreshes qualification, so run it only after the sequence.
    assert_full_oracle(&mut input);
}

#[test]
fn cell_with_unrelated_form_id_still_requires_an_exact_origin_rebase() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input
        .app
        .world_mut()
        .entity_mut(input.cell)
        .insert(FormId(u32::MAX));
    input.assert_bound(primitive);
    assert_full_oracle(&mut input);
    input.app.insert_resource(RenderOrigin(IVec2::X));
    input.assert_blocked();
    assert_full_oracle(&mut input);
    input
        .app
        .world_mut()
        .entity_mut(input.cell)
        .insert(Transform::from_translation(
            crate::streaming::grid_translation(IVec2::ZERO, IVec2::X),
        ));
    input.assert_bound(primitive);
    assert_full_oracle(&mut input);
}

#[test]
fn primitive_with_bogus_cell_key_still_requires_its_authored_pose() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    let authored = input
        .app
        .world()
        .get::<SourceNodePose>(primitive)
        .unwrap()
        .0;
    // A cell tag on a descendant must not hide its independent authored-pose proof.
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .insert(StreamedCellKey(CellKey::Exterior {
            worldspace_id: u32::MAX,
            grid_x: 7,
            grid_y: -9,
        }));
    input.assert_bound(primitive);
    assert_full_oracle(&mut input);
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .insert(Transform {
            translation: authored.translation + Vec3::X,
            ..authored
        });
    input.assert_blocked();
    assert_full_oracle(&mut input);
    input.app.world_mut().entity_mut(primitive).insert(authored);
    input.assert_bound(primitive);
    assert_full_oracle(&mut input);
}

#[test]
fn reference_root_with_bogus_cell_key_still_requires_its_literal_pose() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    input
        .app
        .world_mut()
        .entity_mut(input.root)
        .insert(StreamedCellKey(CellKey::Exterior {
            worldspace_id: u32::MAX,
            grid_x: 7,
            grid_y: -9,
        }));
    input.assert_bound(primitive);
    assert_full_oracle(&mut input);
    input
        .app
        .world_mut()
        .entity_mut(input.root)
        .insert(Transform {
            translation: input.root_pose.translation + Vec3::X,
            ..input.root_pose
        });
    input.assert_blocked();
    assert_full_oracle(&mut input);
    input
        .app
        .world_mut()
        .entity_mut(input.root)
        .insert(input.root_pose);
    input.assert_bound(primitive);
    assert_full_oracle(&mut input);
}

#[test]
fn cell_pose_edits_at_a_nonzero_origin_still_revoke_and_recover() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    let origin = IVec2::new(2, -3);
    let expected =
        Transform::from_translation(crate::streaming::grid_translation(IVec2::ZERO, origin));
    input.app.insert_resource(RenderOrigin(origin));
    input
        .app
        .world_mut()
        .entity_mut(input.cell)
        .insert(expected);
    input.assert_bound(primitive);
    for bad in [
        Transform {
            translation: expected.translation + Vec3::X,
            ..expected
        },
        Transform {
            rotation: Quat::from_rotation_y(0.1),
            ..expected
        },
        Transform {
            scale: Vec3::splat(2.0),
            ..expected
        },
    ] {
        input.app.world_mut().entity_mut(input.cell).insert(bad);
        input.assert_blocked();
        assert_full_oracle(&mut input);
        input
            .app
            .world_mut()
            .entity_mut(input.cell)
            .insert(expected);
        input.assert_bound(primitive);
        assert_full_oracle(&mut input);
    }
}

#[test]
fn exact_flat_rebases_reuse_proof_but_draw_motion_still_revokes() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    let global = flatten_stock(&mut input, primitive);
    input.assert_bound(primitive);
    let refreshes = input.sources().qualification_refreshes;
    let origin = IVec2::new(3, -2);
    input.app.insert_resource(RenderOrigin(origin));
    let shift = crate::streaming::grid_translation(IVec2::ZERO, origin);
    input
        .app
        .world_mut()
        .entity_mut(input.cell)
        .insert(Transform::from_translation(shift));
    let rebased = GlobalTransform::from(Mat4::from_translation(shift) * global.to_matrix());
    input.app.world_mut().entity_mut(primitive).insert(rebased);
    input
        .app
        .world_mut()
        .get_mut::<FlatSourcePrimitive>(primitive)
        .unwrap()
        .expected_global = rebased;
    input.assert_bound(primitive);
    assert_eq!(
        input.sources().qualification_refreshes,
        refreshes,
        "flat metadata and draw rebasing together must reuse model-space proof"
    );
    assert_full_oracle(&mut input);
    input
        .app
        .world_mut()
        .entity_mut(primitive)
        .insert(GlobalTransform::IDENTITY);
    input.assert_blocked();
    assert_full_oracle(&mut input);
    input.app.world_mut().entity_mut(primitive).insert(rebased);
    input.assert_bound(primitive);
    assert_full_oracle(&mut input);
}

#[test]
fn flat_metadata_affine_mismatch_alone_revokes_and_repair_recovers() {
    let mut input = Fixture::pending();
    let primitive = input.spawn_stock();
    let global = flatten_stock(&mut input, primitive);
    input.assert_bound(primitive);
    // Keep the real draw affine fixed; only its expected metadata changes.
    input
        .app
        .world_mut()
        .get_mut::<FlatSourcePrimitive>(primitive)
        .unwrap()
        .expected_global =
        GlobalTransform::from(Mat4::from_translation(Vec3::X) * global.to_matrix());
    input.assert_blocked();
    assert_full_oracle(&mut input);
    input
        .app
        .world_mut()
        .get_mut::<FlatSourcePrimitive>(primitive)
        .unwrap()
        .expected_global = global;
    input.assert_bound(primitive);
    assert_full_oracle(&mut input);
}

#[test]
fn repeated_nonfinite_flat_model_writes_stay_refused_without_requalification() {
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
    for nonfinite in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut invalid = metadata.clone();
        invalid.model_matrix.x_axis.x = nonfinite;
        let before = input.sources().qualification_refreshes;
        input
            .app
            .world_mut()
            .entity_mut(primitive)
            .insert(invalid.clone());
        input.assert_blocked();
        assert!(input.sources().qualification_refreshes > before);
        let refreshes = input.sources().qualification_refreshes;
        for _ in 0..3 {
            input
                .app
                .world_mut()
                .entity_mut(primitive)
                .insert(invalid.clone());
            input.assert_blocked();
            assert_eq!(
                input.sources().qualification_refreshes,
                refreshes,
                "rewriting a refused nonfinite model must not repeat qualification"
            );
            assert_eq!(input.sources().last_work.roots_scanned, 0);
            assert_eq!(input.sources().last_work.meshes_scanned, 0);
        }
        assert_full_oracle(&mut input);
        input
            .app
            .world_mut()
            .entity_mut(primitive)
            .insert(metadata.clone());
        input.assert_bound(primitive);
        assert_full_oracle(&mut input);
    }
}

#[test]
fn repeated_nonfinite_flat_affine_writes_stay_refused_without_requalification() {
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
    for nonfinite in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut matrix = global.to_matrix();
        matrix.w_axis.x = nonfinite;
        let mut invalid = metadata.clone();
        invalid.expected_global = GlobalTransform::from(matrix);
        let before = input.sources().qualification_refreshes;
        // Even matching draw and expected affines must be refused when nonfinite.
        input
            .app
            .world_mut()
            .entity_mut(primitive)
            .insert((invalid.clone(), invalid.expected_global));
        input.assert_blocked();
        assert!(input.sources().qualification_refreshes > before);
        let refreshes = input.sources().qualification_refreshes;
        for _ in 0..3 {
            input
                .app
                .world_mut()
                .entity_mut(primitive)
                .insert((invalid.clone(), invalid.expected_global));
            input.assert_blocked();
            assert_eq!(
                input.sources().qualification_refreshes,
                refreshes,
                "rewriting a refused nonfinite affine must not repeat qualification"
            );
            assert_eq!(input.sources().last_work.roots_scanned, 0);
            assert_eq!(input.sources().last_work.meshes_scanned, 0);
        }
        assert_full_oracle(&mut input);
        input
            .app
            .world_mut()
            .entity_mut(primitive)
            .insert((metadata.clone(), global));
        input.assert_bound(primitive);
        assert_full_oracle(&mut input);
    }
}
