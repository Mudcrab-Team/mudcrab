use super::*;
use crate::streaming::failure::LoadingRefusal;

fn package() -> (tempfile::TempDir, Arc<CertifiedPackage>) {
    let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
    let package =
        Arc::new(crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap());
    (directory, package)
}

fn ready() -> ClusterWorldReadiness {
    ClusterWorldReadiness {
        applicable_cameras: 1,
        needed_batches: 3,
        ..default()
    }
}

#[test]
fn absent_camera_and_every_missing_dependency_hold_the_gate() {
    let (_directory, package) = package();
    let mut gate = ClusterLoadingGate::default();
    assert!(!gate.world_ready(42));
    let mut variants = vec![ClusterWorldReadiness::default()];
    for field in 0..7 {
        let mut facts = ready();
        match field {
            0 => facts.missing_views = 1,
            1 => facts.pending_materials = 1,
            2 => facts.pending_templates = 1,
            3 => facts.pending_compute = 1,
            4 => facts.pending_pipelines = 1,
            5 => facts.failed_batches = 1,
            _ => facts.applicable_cameras = 0,
        }
        variants.push(facts);
    }
    for (frame, facts) in variants.into_iter().enumerate() {
        assert!(gate.publish(
            &package,
            7,
            gate.generation(),
            frame as u64,
            HashMap::from([(42, facts)])
        ));
        assert!(!gate.world_ready(42));
        assert!(gate.owned_not_drawable(42) > 0);
    }
    assert!(gate.publish(
        &package,
        7,
        gate.generation(),
        20,
        HashMap::from([(42, ready())])
    ));
    assert!(gate.world_ready(42));
    assert_eq!(gate.owned_not_drawable(42), 0);
}

#[test]
fn teleport_invalidation_rejects_old_render_snapshots_even_in_the_same_world() {
    let (_directory, package) = package();
    let mut gate = ClusterLoadingGate::default();
    let before = gate.generation();
    gate.publish(&package, 7, before, 5, HashMap::from([(42, ready())]));
    assert!(gate.world_ready(42));
    let current = gate.invalidate();
    assert!(!gate.world_ready(42));
    assert!(!gate.publish(&package, 7, before, 6, HashMap::from([(42, ready())])));
    assert!(!gate.world_ready(42));
    assert!(gate.publish(&package, 7, current, 7, HashMap::from([(42, ready())])));
    assert!(gate.world_ready(42));
    assert!(!gate.publish(
        &package,
        7,
        current,
        6,
        HashMap::from([(42, ClusterWorldReadiness::default())])
    ));
    assert!(gate.world_ready(42));
}

#[test]
fn changed_world_scope_withdraws_previous_positive_facts() {
    let (_directory, package) = package();
    let mut gate = ClusterLoadingGate::default();
    gate.publish(
        &package,
        7,
        gate.generation(),
        1,
        HashMap::from([(42, ready())]),
    );
    assert!(gate.world_ready(42));
    gate.publish(
        &package,
        8,
        gate.generation(),
        2,
        HashMap::from([(43, ready())]),
    );
    assert!(!gate.world_ready(42));
    assert!(gate.world_ready(43));
}

#[test]
fn ordinary_frames_never_raise_a_teleport_cover() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(crate::config::EngineConfig::default())
        .init_resource::<crate::streaming::ActiveSpace>()
        .insert_resource(crate::streaming::RenderOrigin(IVec2::ZERO))
        .init_resource::<crate::streaming::StreamingWorld>()
        .init_resource::<crate::streaming::StreamingMetrics>();
    install(&mut app);
    for _ in 0..3 {
        app.update();
    }
    assert!(!app.world().resource::<LoadingCover>().teleport_pending());
    assert_eq!(crate::pacing::teleport_cover_count(app.world_mut()), 0);
}

#[test]
fn teleport_cover_waits_for_current_generation_and_withdrawn_view_then_releases() {
    use crate::streaming::{ActiveSpace, RenderOrigin, StreamingMetrics, StreamingWorld};
    use crate::world::database::CellKey;
    use bevy::ecs::system::RunSystemOnce;

    let (_directory, package) = package();
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(EngineConfig {
            clusters: Some("selected".into()),
            cluster_own_statics: true,
            worldspace_id: 42,
            stream_radius: 0,
            ..default()
        })
        .init_resource::<ActiveSpace>()
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .init_resource::<StreamingWorld>()
        .init_resource::<StreamingMetrics>();
    install(&mut app);
    app.world_mut()
        .spawn((StreamingCamera, Transform::default()));
    let root = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .set_resident_for_test(
            CellKey::Exterior {
                worldspace_id: 42,
                grid_x: 0,
                grid_y: 0,
            },
            root,
        );
    let before = app.world().resource::<ClusterLoadingGate>().generation();
    app.world_mut()
        .resource_mut::<ClusterLoadingGate>()
        .publish(&package, 7, before, 1, HashMap::from([(42, ready())]));
    app.world_mut()
        .run_system_once(|mut cover: ResMut<LoadingCover>, mut commands: Commands| {
            cover.begin_teleport(&mut commands);
        })
        .unwrap();
    app.update();
    assert!(app.world().resource::<LoadingCover>().teleport_pending());
    assert_eq!(crate::pacing::teleport_cover_count(app.world_mut()), 1);
    let current = app.world().resource::<ClusterLoadingGate>().generation();
    let mut missing = ready();
    missing.missing_views = 1;
    app.world_mut()
        .resource_mut::<ClusterLoadingGate>()
        .publish(&package, 7, current, 2, HashMap::from([(42, missing)]));
    app.update();
    assert!(app.world().resource::<LoadingCover>().teleport_pending());
    app.world_mut()
        .resource_mut::<ClusterLoadingGate>()
        .publish(&package, 7, current, 3, HashMap::from([(42, ready())]));
    let mut camera = app
        .world_mut()
        .query_filtered::<&mut Transform, With<StreamingCamera>>();
    camera.single_mut(app.world_mut()).unwrap().translation.y += 10.0;
    app.update();
    assert!(
        app.world().resource::<LoadingCover>().teleport_pending(),
        "a pose change must invalidate ready proof before uncovering"
    );
    let final_pose_generation = app.world().resource::<ClusterLoadingGate>().generation();
    assert_ne!(final_pose_generation, current);
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(
        app.world().resource::<ClusterLoadingGate>().generation(),
        final_pose_generation
    );
    app.world_mut()
        .resource_mut::<ClusterLoadingGate>()
        .publish(
            &package,
            7,
            final_pose_generation,
            4,
            HashMap::from([(42, ready())]),
        );
    app.update();
    assert!(!app.world().resource::<LoadingCover>().teleport_pending());
    assert_eq!(crate::pacing::teleport_cover_count(app.world_mut()), 0);
}

#[test]
fn a_teleport_that_keeps_loading_can_exceed_the_idle_deadline_and_complete() {
    use crate::world::database::CellKey;
    use bevy::{ecs::system::RunSystemOnce, time::TimeUpdateStrategy};
    let (_directory, package) = package();
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs(1),
        ))
        .insert_resource(EngineConfig {
            clusters: Some("selected".into()),
            worldspace_id: 42,
            stream_radius: 0,
            ..default()
        })
        .init_resource::<ActiveSpace>()
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .init_resource::<StreamingWorld>()
        .init_resource::<StreamingMetrics>();
    install(&mut app);
    app.world_mut()
        .spawn((StreamingCamera, Transform::default()));
    let root = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<StreamingWorld>()
        .set_resident_for_test(
            CellKey::Exterior {
                worldspace_id: 42,
                grid_x: 0,
                grid_y: 0,
            },
            root,
        );
    app.world_mut()
        .run_system_once(|mut cover: ResMut<LoadingCover>, mut commands: Commands| {
            cover.begin_teleport(&mut commands);
        })
        .unwrap();
    // Let the real policy consume the neutral transition before publishing its proof.
    app.update();
    let generation = app.world().resource::<ClusterLoadingGate>().generation();
    app.world_mut()
        .resource_mut::<ClusterLoadingGate>()
        .publish(&package, 7, generation, 1, HashMap::from([(42, ready())]));
    for pending in (1..=180).rev() {
        app.world_mut()
            .resource_mut::<StreamingMetrics>()
            .pending_cluster_batches = pending;
        app.update();
        assert!(
            !app.world().contains_resource::<LoadingRefusal>(),
            "progressing load refused"
        );
        assert!(app.world().resource::<LoadingCover>().teleport_pending());
    }
    app.world_mut()
        .resource_mut::<StreamingMetrics>()
        .pending_cluster_batches = 0;
    app.update();
    assert!(!app.world().resource::<LoadingCover>().teleport_pending());
    assert!(!app.world().contains_resource::<LoadingRefusal>());
}

#[test]
fn a_missing_teleport_camera_refuses_after_the_loading_deadline() {
    use bevy::{ecs::system::RunSystemOnce, time::TimeUpdateStrategy};
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs(1),
        ))
        .insert_resource(EngineConfig {
            clusters: Some("selected".into()),
            ..default()
        })
        .init_resource::<ActiveSpace>()
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .init_resource::<StreamingWorld>()
        .init_resource::<StreamingMetrics>();
    install(&mut app);
    app.world_mut()
        .run_system_once(|mut cover: ResMut<LoadingCover>, mut commands: Commands| {
            cover.begin_teleport(&mut commands);
        })
        .unwrap();
    let mut refused = false;
    for _ in 0..65 {
        app.update();
        refused |= app
            .world()
            .resource::<Messages<AppExit>>()
            .iter_current_update_messages()
            .any(AppExit::is_error);
    }
    assert!(
        refused,
        "missing camera cannot strand a covered teleport forever"
    );
    assert!(app.world().resource::<LoadingCover>().teleport_pending());
}
