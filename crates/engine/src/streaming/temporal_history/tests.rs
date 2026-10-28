use super::{test_support::*, *};
use crate::{
    streaming::lod::LodChunkGridOrigin,
    world::components::{CELL_SIZE, ExteriorCellGrid},
};

#[test]
fn uncorrected_position_history_leaves_screen_while_model_coordinates_cancel() {
    let mut f = Fixture::build(false);
    f.rebase(&[IVec2::X]);
    let point = f.model(f.mesh).w_axis.truncate();
    let current = uv(f.current_clip(point));
    let stock = uv(f.previous_view().clip_from_world * f.previous_model(f.mesh).w_axis);
    let position_history = uv(f.previous_view().clip_from_world * point.extend(1.0));
    assert!((stock - current).length() < 0.0001);
    assert!(
        position_history.x.abs() > 0.5,
        "position history UV lies outside the screen: {position_history:?}"
    );
}

#[test]
fn rebase_preserves_static_model_and_position_history() {
    let mut f = Fixture::new();
    let old_clip = f.previous_view().clip_from_world * f.model(f.mesh).w_axis;
    let old_world_from_clip = f.previous_view().world_from_clip;
    f.rebase(&[IVec2::X]);
    assert_matrix_close(f.previous_model(f.mesh), f.model(f.mesh));
    let point = f.model(f.mesh).w_axis.truncate();
    // A position-based temporal consumer supplies its rebased current position as previous.
    let position_previous_clip = f.previous_view().clip_from_world * point.extend(1.0);
    let current_clip = f.current_clip(point);
    assert!((uv(position_previous_clip) - uv(current_clip)).length() < 0.0001);
    assert!((uv(position_previous_clip) - uv(old_clip)).length() < 0.0001);
    // Multiplying inverse perspective matrices at cell-sized translations is
    // ill-conditioned in f32. Check the consumer's reconstructed positions in
    // render units instead of cancellation in unrelated matrix coefficients.
    for ndc in [
        Vec3::new(0.0, 0.0, 1.0),
        Vec3::new(0.5, -0.25, 0.1),
        Vec3::new(-0.2, 0.3, 0.001),
    ] {
        let expected = old_world_from_clip.project_point3(ndc) - Vec3::X * CELL_SIZE;
        let reconstructed = f.previous_view().world_from_clip.project_point3(ndc);
        assert!(reconstructed.abs_diff_eq(expected, 0.002));
    }
}

#[test]
fn net_double_rebase_repairs_models_once_and_leaves_interiors_alone() {
    let mut f = Fixture::new();
    let interior = f.previous_model(f.interior);
    f.rebase(&[IVec2::X, IVec2::Y]);
    assert_matrix_close(f.previous_model(f.mesh), f.model(f.mesh));
    assert_matrix_close(f.previous_model(f.interior), interior);
    f.app.update();
    assert_matrix_close(f.previous_model(f.mesh), f.model(f.mesh));
}

#[test]
fn camera_delta_snapshots_separate_the_cell_jump_from_real_frame_motion() {
    for motion in [Vec3::ZERO, Vec3::new(3.0, 4.0, 12.0)] {
        let mut f = Fixture::new();
        f.app
            .world_mut()
            .get_mut::<Transform>(f.camera)
            .unwrap()
            .translation += motion;
        f.rebase(&[IVec2::X]);
        let snapshot = f.app.world().resource::<TestSnapshot>();
        let before = snapshot.camera_delta_before.unwrap();
        let after = snapshot.camera_delta.unwrap();
        let frame_motion = snapshot.camera_frame_motion.unwrap();
        let expected_before = (Vec3::X * CELL_SIZE - motion).length();
        assert!((before - f64::from(expected_before)).abs() < 0.002);
        assert!((after - f64::from(motion.length())).abs() < 0.002);
        assert!((frame_motion - f64::from(motion.length())).abs() < 0.002);
        // These snapshots describe crossings; an unchanged-origin frame must not
        // overwrite the last measured crossing with a stationary-frame value.
        f.app.update();
        assert_eq!(
            f.app.world().resource::<TestSnapshot>().camera_delta,
            Some(after)
        );
    }
}

#[test]
fn real_motion_survives_rebase_and_next_prepass_capture() {
    let mut f = Fixture::new();
    let before = f.model(f.mesh);
    // Faster than the draft's half-cell threshold: ownership, not speed, decides.
    let motion = Vec3::X * (CELL_SIZE * 0.75);
    f.app
        .world_mut()
        .get_mut::<Transform>(f.mesh)
        .unwrap()
        .translation += motion;
    f.rebase(&[IVec2::X]);
    let implied = f.model(f.mesh).w_axis.truncate() - f.previous_model(f.mesh).w_axis.truncate();
    assert!(implied.abs_diff_eq(motion, 0.001));
    assert_matrix_close(
        f.previous_model(f.mesh),
        Mat4::from_translation(Vec3::new(-CELL_SIZE, 0.0, 0.0)) * before,
    );
    // Bevy only copies a GlobalTransform newer than PreviousGlobalTransform.
    // Repairing after propagation would leave the real motion stuck for a frame.
    f.app.update();
    assert_matrix_close(f.previous_model(f.mesh), f.model(f.mesh));
}

#[test]
fn mesh_and_camera_rotation_survive_rebase_then_stop_without_false_motion() {
    let mut f = Fixture::new();
    let old_model = f.model(f.mesh);
    let old_view = f.previous_view().view_from_world;
    let turn = Quat::from_rotation_y(0.21);
    for entity in [f.mesh, f.camera] {
        f.app
            .world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .rotation *= turn;
    }
    let shift = grid_translation(IVec2::new(1, -1), IVec2::ZERO);
    f.rebase(&[IVec2::new(1, -1)]);
    assert_matrix_close(
        f.previous_model(f.mesh),
        Mat4::from_translation(-shift) * old_model,
    );
    assert_matrix_close(
        f.previous_view().view_from_world,
        old_view * Mat4::from_translation(shift),
    );
    assert!(!f.previous_model(f.mesh).abs_diff_eq(f.model(f.mesh), 0.002));
    f.app.update();
    assert_matrix_close(f.previous_model(f.mesh), f.model(f.mesh));
    assert_matrix_close(
        f.previous_view().view_from_world,
        f.model(f.camera).inverse(),
    );
}

#[test]
fn rebase_history_repair_preserves_current_global_change_ticks() {
    assert_current_global_ticks(Fixture::new(), false);
}

#[test]
fn lod_camera_children_and_explicit_participants_share_the_rebase_frame() {
    let mut f = Fixture::new();
    let lod = f
        .app
        .world_mut()
        .spawn((
            LodChunkGridOrigin::world_origin(),
            Transform::IDENTITY,
            ShiftWithOrigin,
        ))
        .id();
    let lod_mesh = f
        .app
        .world_mut()
        .spawn((Mesh3d::default(), Transform::IDENTITY, ChildOf(lod)))
        .id();
    let camera_mesh = f
        .app
        .world_mut()
        .spawn((Mesh3d::default(), Transform::IDENTITY, ChildOf(f.camera)))
        .id();
    let body = f
        .app
        .world_mut()
        .spawn((
            crate::physics::PlayerBody,
            Mesh3d::default(),
            Transform::IDENTITY,
            ShiftWithOrigin,
        ))
        .id();
    let tankard = f
        .app
        .world_mut()
        .spawn((
            crate::physics::DebugTankard,
            Mesh3d::default(),
            Transform::IDENTITY,
            ShiftWithOrigin,
        ))
        .id();
    f.app.update();
    f.app.update();
    f.rebase(&[IVec2::Y]);
    for entity in [lod_mesh, camera_mesh, body, tankard] {
        assert_matrix_close(f.previous_model(entity), f.model(entity));
    }
}

#[test]
fn teleport_motion_remains_after_origin_compensation() {
    let mut f = Fixture::new();
    let camera_before = f.model(f.camera).w_axis.truncate();
    let teleport = Vec3::new(10000.0, 100.0, 5000.0);
    f.app
        .world_mut()
        .get_mut::<Transform>(f.camera)
        .unwrap()
        .translation += teleport;
    f.rebase(&[IVec2::X]);
    let previous_camera = f
        .previous_view()
        .view_from_world
        .inverse()
        .w_axis
        .truncate();
    assert!(previous_camera.abs_diff_eq(camera_before - Vec3::X * CELL_SIZE, 0.002));
    let camera_now = f.model(f.camera).w_axis.truncate();
    assert!((camera_now - previous_camera).abs_diff_eq(teleport, 0.002));
}

#[test]
fn door_origin_assignment_leaves_unshifted_tankard_history_alone() {
    let mut f = Fixture::new();
    let tankard = f
        .app
        .world_mut()
        .spawn((
            crate::physics::DebugTankard,
            Mesh3d::default(),
            Transform::from_xyz(1.0, 2.0, 3.0),
        ))
        .id();
    f.app.update();
    f.app.update();
    let previous = f.previous_model(tankard);
    f.rebase(&[IVec2::X]);
    assert_matrix_close(f.previous_model(tankard), previous);
}

#[test]
fn cancelling_rebases_leave_previous_coordinates_unchanged() {
    let mut f = Fixture::new();
    let previous = f.previous_model(f.mesh);
    f.rebase(&[IVec2::X, -IVec2::X]);
    assert_matrix_close(f.previous_model(f.mesh), previous);
    assert_matrix_close(f.previous_model(f.mesh), f.model(f.mesh));
}

#[test]
fn changing_space_keeps_the_old_view_as_a_camera_cut() {
    let mut f = Fixture::new();
    f.app.world_mut().insert_resource(super::ActiveSpace {
        worldspace_id: Some(7),
        interior: Some(9),
    });
    f.app.update();
    let old_view = f.previous_view().view_from_world;
    // Space changes in Update, after the previous pose and domain were captured.
    f.app
        .add_systems(Update, |mut active: ResMut<super::ActiveSpace>| {
            active.interior = None;
        });
    f.rebase(&[IVec2::X]);
    assert_matrix_close(f.previous_view().view_from_world, old_view);
}

#[test]
fn disabled_ancestor_that_did_not_move_keeps_its_model_history() {
    let mut f = Fixture::new();
    let before = f.previous_model(f.mesh);
    f.app
        .world_mut()
        .entity_mut(f.root)
        .insert(bevy::ecs::entity_disabling::Disabled);
    f.rebase(&[IVec2::X]);
    assert_matrix_close(f.model(f.mesh), before);
    assert_matrix_close(f.previous_model(f.mesh), before);
}

#[test]
fn reflection_previous_views_follow_the_main_render_coordinate_domain() {
    let mut f = Fixture::new();
    let reflection = f
        .app
        .world_mut()
        .spawn((
            WaterReflectionCamera,
            Camera3d::default(),
            Camera::default(),
            Transform::from_xyz(20.0, -30.0, 40.0),
            ShiftWithOrigin,
        ))
        .id();
    f.app.update();
    f.app.update();
    f.rebase(&[IVec2::X]);
    let previous = f.app.world().get::<PreviousViewData>(reflection).unwrap();
    let current_view = f.model(reflection).inverse();
    assert_matrix_close(previous.view_from_world, current_view);
    // Each previous inverse must describe the same coordinate frame.
    assert_matrix_close(
        previous.world_from_clip,
        f.model(reflection) * previous.view_from_clip,
    );
    let snapshot = f.app.world().resource::<TestSnapshot>();
    assert!(snapshot.camera_delta.unwrap() < 0.002);
}

#[test]
fn complete_active_space_identity_preserves_worldspace_and_interior_cuts() {
    for (before, after) in [
        ((None, None), (Some(7), None)),
        ((Some(7), None), (None, None)),
        ((Some(7), None), (Some(8), None)),
        ((Some(7), None), (Some(7), Some(9))),
        ((Some(7), Some(9)), (Some(7), None)),
        ((Some(7), Some(9)), (Some(7), Some(10))),
        ((Some(7), Some(9)), (Some(7), Some(9))),
    ] {
        let mut f = Fixture::new();
        f.app.world_mut().insert_resource(ActiveSpace {
            worldspace_id: before.0,
            interior: before.1,
        });
        f.app.update();
        let old_view = f.previous_view().view_from_world;
        let old_model = f.previous_model(f.mesh);
        f.app
            .add_systems(Update, move |mut active: ResMut<ActiveSpace>| {
                *active = ActiveSpace {
                    worldspace_id: after.0,
                    interior: after.1,
                };
            });
        f.rebase(&[IVec2::X]);
        assert_matrix_close(f.previous_view().view_from_world, old_view);
        assert_matrix_close(f.previous_model(f.mesh), old_model);
    }
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "RenderOrigin changed outside Update")]
fn a_late_origin_writer_is_rejected_before_history_repair() {
    let mut f = Fixture::new();
    f.app.add_systems(
        PostUpdate,
        (|mut origin: ResMut<RenderOrigin>| {
            origin.0 += IVec2::X;
        })
        .before(rebase_previous_frame_data),
    );
    f.app.update();
}

#[test]
fn parented_ownership_ancestry_is_resolved_once_per_crossing() {
    use bevy::ecs::system::SystemState;
    let mut world = World::new();
    let root = world.spawn(ExteriorCellGrid(IVec2::ZERO)).id();
    let mut parent = root;
    for _ in 0..8 {
        parent = world.spawn(ChildOf(parent)).id();
    }
    let leaves = (0..1000)
        .map(|_| world.spawn(ChildOf(parent)).id())
        .collect::<Vec<_>>();
    let mut state = SystemState::<OriginOwnership>::new(&mut world);
    let mut memo = OwnershipMemo::default();
    {
        let ownership = state.get(&world).unwrap();
        for &leaf in &leaves {
            assert_eq!(
                ownership.displacement(leaf, Vec3::X * CELL_SIZE, Vec3::ZERO, &mut memo),
                Some(Vec3::X * CELL_SIZE)
            );
        }
    }
    // One visit per mesh leaf, plus each of the nine shared ancestor nodes once.
    assert_eq!(memo.visits, 1009);
    assert_eq!(memo.hits, 999);
    assert_eq!(memo.ancestors.len(), 9);
    // A later crossing must resolve new ownership, rather than reuse stale paths.
    world.entity_mut(parent).insert(Disabled);
    memo.clear();
    let ownership = state.get(&world).unwrap();
    assert_eq!(
        ownership.displacement(leaves[0], Vec3::Z * CELL_SIZE, Vec3::ZERO, &mut memo),
        None
    );
    assert_eq!(memo.visits, 2);
}

#[test]
fn real_reflection_writer_uses_the_rebased_current_pose_and_preserves_real_motion() {
    let mut f = Fixture::new();
    f.app.init_resource::<crate::profiling::ProfilingState>();
    crate::render::install_reflection_update_for_test(&mut f.app);
    f.app.world_mut().spawn((
        crate::world::components::WaterSurface,
        Transform::IDENTITY,
        ChildOf(f.root),
    ));
    let reflection = f
        .app
        .world_mut()
        .spawn((
            WaterReflectionCamera,
            Camera3d::default(),
            Camera::default(),
            Transform::from_xyz(100.0, -30.0, 50.0),
        ))
        .id();
    f.app.update();
    f.app.update();
    let before = f.model(reflection).w_axis.truncate();
    f.app
        .world_mut()
        .get_mut::<Transform>(f.camera)
        .unwrap()
        .translation
        .x += 1.0;
    f.rebase(&[IVec2::X]);
    let current = f.model(reflection).w_axis.truncate();
    let expected = before + Vec3::X - Vec3::X * CELL_SIZE;
    assert!(
        current.abs_diff_eq(expected, 0.002),
        "real reflection current pose stayed in the previous origin: {current:?}, expected {expected:?}"
    );
    let previous = f
        .app
        .world()
        .get::<PreviousViewData>(reflection)
        .unwrap()
        .view_from_world
        .inverse()
        .w_axis
        .truncate();
    assert!(previous.abs_diff_eq(before - Vec3::X * CELL_SIZE, 0.002));
    assert!(
        (current - previous).abs_diff_eq(Vec3::X, 0.002),
        "retain actual one-unit camera motion"
    );
    f.app.update();
    let next = f
        .app
        .world()
        .get::<PreviousViewData>(reflection)
        .unwrap()
        .view_from_world
        .inverse()
        .w_axis
        .truncate();
    assert!(
        next.abs_diff_eq(f.model(reflection).w_axis.truncate(), 0.002),
        "next snapshot settles once"
    );
}

#[test]
fn one_rebase_counts_actual_rewritten_mesh_components_and_pending_recapture() {
    let mut f = Fixture::new();
    let mut owned = vec![f.mesh];
    for i in 0..32 {
        owned.push(
            f.app
                .world_mut()
                .spawn((
                    Mesh3d::default(),
                    Transform::from_xyz(i as f32, 2.0, 3.0),
                    ChildOf(f.root),
                ))
                .id(),
        );
    }
    f.app.update();
    f.app.update();
    let before = owned
        .iter()
        .copied()
        .chain([f.interior])
        .map(|entity| (entity, f.previous_model(entity)))
        .collect::<Vec<_>>();
    f.rebase(&[IVec2::X]);
    let touched = before
        .iter()
        .filter(|&&(entity, old)| f.previous_model(entity) != old)
        .map(|&(entity, _)| entity)
        .collect::<Vec<_>>();
    assert_eq!(
        touched.len(),
        33,
        "count actual PreviousGlobalTransform values changed"
    );
    let pending = &f.app.world().resource::<PendingPreviousMeshes>().0;
    assert_eq!(pending.len(), touched.len());
    for &(entity, old) in &before {
        if owned.contains(&entity) {
            assert!(pending.contains(&entity));
            assert_matrix_close(
                f.previous_model(entity),
                Mat4::from_translation(-Vec3::X * CELL_SIZE) * old,
            );
        } else {
            assert!(!pending.contains(&entity));
            assert_matrix_close(f.previous_model(entity), old);
        }
    }
    f.app.update();
    assert!(
        f.app
            .world()
            .resource::<PendingPreviousMeshes>()
            .0
            .is_empty()
    );
    for entity in owned {
        assert_matrix_close(f.previous_model(entity), f.model(entity));
    }
}

#[test]
fn no_temporal_consumer_or_only_inactive_consumer_skips_mesh_and_view_history() {
    for inactive in [false, true] {
        let mut f = Fixture::new();
        if inactive {
            f.app
                .world_mut()
                .get_mut::<Camera>(f.camera)
                .unwrap()
                .is_active = false;
        } else {
            f.app
                .world_mut()
                .entity_mut(f.camera)
                .remove::<bevy::core_pipeline::prepass::MotionVectorPrepass>();
        }
        f.app.update();
        let model = f.previous_model(f.mesh);
        let view = f.previous_view().view_from_world;
        f.rebase(&[IVec2::X]);
        assert_matrix_close(f.previous_model(f.mesh), model);
        assert_matrix_close(f.previous_view().view_from_world, view);
        assert!(
            f.app
                .world()
                .resource::<PendingPreviousMeshes>()
                .0
                .is_empty()
        );
    }
}

#[test]
fn depth_occlusion_reflection_repairs_view_without_cpu_motion_history() {
    let mut f = Fixture::new();
    f.app
        .world_mut()
        .entity_mut(f.camera)
        .remove::<bevy::core_pipeline::prepass::MotionVectorPrepass>();
    let reflection = f
        .app
        .world_mut()
        .spawn((
            WaterReflectionCamera,
            Camera3d::default(),
            Camera::default(),
            bevy::core_pipeline::prepass::DepthPrepass,
            bevy::render::occlusion_culling::OcclusionCulling,
            Transform::from_xyz(20.0, -30.0, 40.0),
            ShiftWithOrigin,
        ))
        .id();
    f.app.update();
    f.app.update();
    let model_history = f.previous_model(f.mesh);
    f.rebase(&[IVec2::X]);
    assert_matrix_close(f.previous_model(f.mesh), model_history);
    let previous = f.app.world().get::<PreviousViewData>(reflection).unwrap();
    assert_matrix_close(previous.view_from_world, f.model(reflection).inverse());
    assert!(
        f.app
            .world()
            .resource::<PendingPreviousMeshes>()
            .0
            .is_empty()
    );
}

#[test]
fn motion_vectors_and_taa_enable_cpu_model_history_repair() {
    for taa in [false, true] {
        let mut f = Fixture::new();
        f.app
            .world_mut()
            .entity_mut(f.camera)
            .remove::<bevy::core_pipeline::prepass::MotionVectorPrepass>();
        if taa {
            f.app
                .world_mut()
                .entity_mut(f.camera)
                .insert(bevy::anti_alias::taa::TemporalAntiAliasing::default());
        } else {
            f.app
                .world_mut()
                .entity_mut(f.camera)
                .insert(bevy::core_pipeline::prepass::MotionVectorPrepass);
        }
        assert!(
            f.app
                .world()
                .get::<bevy::core_pipeline::prepass::MotionVectorPrepass>(f.camera)
                .is_some()
        );
        f.rebase(&[IVec2::X]);
        assert_matrix_close(f.previous_model(f.mesh), f.model(f.mesh));
        assert_eq!(
            f.app.world().resource::<PendingPreviousMeshes>().0,
            vec![f.mesh]
        );
    }
}

#[test]
fn consumed_rebase_recapture_survives_temporal_consumer_removal() {
    let mut f = Fixture::new();
    // Integer coordinates make parent displacement and child motion cancel
    // bit-for-bit, so native change ticks cannot hide a lost recapture.
    *f.app.world_mut().get_mut::<Transform>(f.mesh).unwrap() = Transform::from_xyz(16.0, 8.0, 32.0);
    f.app.update();
    f.app.update();
    let current = f.model(f.mesh);
    let original_tick = f
        .app
        .world()
        .entity(f.mesh)
        .get_ref::<GlobalTransform>()
        .unwrap()
        .last_changed();
    f.app
        .world_mut()
        .get_mut::<Transform>(f.mesh)
        .unwrap()
        .translation
        .x += CELL_SIZE;
    f.rebase(&[IVec2::X]);
    assert_eq!(
        f.model(f.mesh),
        current,
        "the cancellation fixture must be exact"
    );
    assert_eq!(
        f.app
            .world()
            .entity(f.mesh)
            .get_ref::<GlobalTransform>()
            .unwrap()
            .last_changed(),
        original_tick,
        "exact cancellation must leave native current-global ticks unchanged"
    );
    assert!(
        f.app
            .world()
            .resource::<PendingPreviousMeshes>()
            .0
            .contains(&f.mesh)
    );
    let tick = f
        .app
        .world()
        .entity(f.mesh)
        .get_ref::<GlobalTransform>()
        .unwrap()
        .last_changed();
    f.app.world_mut().entity_mut(f.camera).remove::<(
        bevy::core_pipeline::prepass::MotionVectorPrepass,
        bevy::core_pipeline::prepass::DepthPrepass,
        bevy::core_pipeline::prepass::NormalPrepass,
        bevy::core_pipeline::prepass::DeferredPrepass,
        bevy::render::occlusion_culling::OcclusionCulling,
    )>();
    f.app.update();
    assert!(
        f.app
            .world()
            .resource::<PendingPreviousMeshes>()
            .0
            .is_empty()
    );
    assert_matrix_close(f.previous_model(f.mesh), f.model(f.mesh));
    assert_eq!(
        f.app
            .world()
            .entity(f.mesh)
            .get_ref::<GlobalTransform>()
            .unwrap()
            .last_changed(),
        tick
    );
    f.app
        .world_mut()
        .entity_mut(f.camera)
        .insert(bevy::core_pipeline::prepass::MotionVectorPrepass);
    f.app.update();
    assert_matrix_close(f.previous_model(f.mesh), f.model(f.mesh));
    assert_eq!(
        f.app
            .world()
            .entity(f.mesh)
            .get_ref::<GlobalTransform>()
            .unwrap()
            .last_changed(),
        tick
    );
}

#[test]
fn unrelated_prepass_markers_do_not_consume_temporal_history() {
    for kind in 0..5 {
        let mut f = Fixture::new();
        f.app.world_mut().entity_mut(f.camera).remove::<(
            bevy::core_pipeline::prepass::MotionVectorPrepass,
            bevy::core_pipeline::prepass::DepthPrepass,
            bevy::core_pipeline::prepass::NormalPrepass,
            bevy::core_pipeline::prepass::DeferredPrepass,
            bevy::render::occlusion_culling::OcclusionCulling,
        )>();
        match kind {
            0 => {
                f.app
                    .world_mut()
                    .entity_mut(f.camera)
                    .insert(bevy::core_pipeline::prepass::DepthPrepass);
            }
            1 => {
                f.app
                    .world_mut()
                    .entity_mut(f.camera)
                    .insert(bevy::core_pipeline::prepass::NormalPrepass);
            }
            2 => {
                f.app
                    .world_mut()
                    .entity_mut(f.camera)
                    .insert(bevy::core_pipeline::prepass::DeferredPrepass);
            }
            3 => {
                f.app
                    .world_mut()
                    .entity_mut(f.camera)
                    .insert(bevy::render::occlusion_culling::OcclusionCulling);
            }
            _ => {
                f.app.world_mut().entity_mut(f.camera).insert((
                    bevy::core_pipeline::prepass::DepthPrepass,
                    bevy::render::occlusion_culling::OcclusionCulling,
                    bevy::render::view::NoIndirectDrawing,
                ));
            }
        }
        f.app.update();
        let previous = f.previous_model(f.mesh);
        let view = f.previous_view().view_from_world;
        f.rebase(&[IVec2::X]);
        assert_matrix_close(f.previous_model(f.mesh), previous);
        assert_matrix_close(f.previous_view().view_from_world, view);
        assert!(
            f.app
                .world()
                .resource::<PendingPreviousMeshes>()
                .0
                .is_empty(),
            "marker-only configuration {kind} has no temporal consumer"
        );
    }
}
