mod door_inactivity {
    use super::*;
    use crate::streaming::failure::LoadingRefusal;
    use bevy::ecs::system::RunSystemOnce;

    #[derive(Clone, Copy, Debug)]
    enum RequiredPolicy {
        Resource,
        RetainedRequest,
        SelectedPackage,
    }

    fn start_real_restore(policy: RequiredPolicy) -> App {
        let (mut app, _) = app_with(interior_door());
        match policy {
            RequiredPolicy::Resource | RequiredPolicy::RetainedRequest => {
                app.insert_resource(RequiredHandoff);
            }
            RequiredPolicy::SelectedPackage => {
                app.world_mut()
                    .resource_mut::<crate::config::EngineConfig>()
                    .clusters = Some("door-inactivity-fixture".to_owned());
            }
        }
        press_e(&mut app);
        run_until_black(&mut app);
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(CellKey::Interior(77));
        app.update();
        let active = app
            .world()
            .resource::<DoorCrossing>()
            .active
            .as_ref()
            .unwrap();
        assert!(matches!(
            active.stage,
            Stage::Landing {
                restoring: true,
                ..
            }
        ));
        assert_eq!(
            app.world().resource::<Teleports>().0.len(),
            2,
            "the actual destination failure must switch back to the saved space"
        );
        let (id, _) = active
            .request
            .expect("the real restore created a destination request");
        assert_eq!(
            app.world()
                .resource::<DestinationRequests>()
                .iter()
                .find(|(held, _)| *held == id)
                .unwrap()
                .1
                .handoff,
            Handoff::Required
        );
        if matches!(policy, RequiredPolicy::RetainedRequest) {
            app.world_mut().remove_resource::<RequiredHandoff>();
        }
        assert!(!app.world().contains_resource::<LoadingRefusal>());
        app
    }

    fn resident_restore_with_work(app: &mut App, work: usize) -> (Entity, Vec<Entity>) {
        let restore = app
            .world()
            .resource::<DoorCrossing>()
            .active
            .as_ref()
            .unwrap()
            .restore
            .landing()
            .key;
        let root = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(restore, root);
        let markers = (0..work)
            .map(|_| {
                app.world_mut()
                    .spawn((pending_marker_for_test(), ChildOf(root)))
                    .id()
            })
            .collect();
        // Establish the first real residency/pending observation without changing Stage/time.
        app.update();
        (root, markers)
    }

    fn pending_count(app: &mut App, root: Entity) -> usize {
        app.world_mut()
            .run_system_once(move |pending: PendingUnder| pending.count(root))
            .unwrap()
    }

    fn publish_required_restore_ready(app: &mut App) {
        use crate::swap_bridge::{
            destination::{RenderFrame, RenderProof},
            systems::{EvaluatedProof, RenderResults},
        };
        let (id, key) = app
            .world()
            .resource::<DoorCrossing>()
            .active
            .as_ref()
            .unwrap()
            .request
            .expect("the restore still owns its request");
        let frame = RenderFrame(1);
        app.world_mut()
            .resource_mut::<RenderResults>()
            .insert_for_test(
                id,
                EvaluatedProof {
                    key,
                    frame,
                    extracted_generation: key.generation,
                    proof: RenderProof::default(),
                    handoff: Some(DestinationState::Ready),
                },
            );
    }

    fn assert_still_restoring(app: &mut App) {
        assert!(
            !app.world().contains_resource::<LoadingRefusal>(),
            "continuing restore progress cannot be refused by a total loading clock: {:?}",
            app.world().get_resource::<LoadingRefusal>()
        );
        assert!(matches!(
            app.world()
                .resource::<DoorCrossing>()
                .active
                .as_ref()
                .unwrap()
                .stage,
            Stage::Landing {
                restoring: true,
                ..
            }
        ));
        assert!(
            alpha(app) >= 0.99,
            "an unfinished Required restore remains covered"
        );
        assert!(!app.world_mut().run_system_once(no_door_crossing).unwrap());
        assert_eq!(
            budget(app),
            None,
            "the upload budget remains lifted while restoring"
        );
    }

    fn diagnosed_stalled_seconds(reason: &str) -> f64 {
        let value = reason
            .split_once("stalled_seconds=")
            .expect("the refusal must name its inactivity duration, not total loading time")
            .1;
        value
            .split_whitespace()
            .next()
            .unwrap()
            .trim_end_matches([',', ';'])
            .parse()
            .expect("stalled_seconds must be a numeric duration")
    }

    #[test]
    fn a_slow_required_restore_keeps_progressing_beyond_the_total_landing_limit_then_finishes() {
        for policy in [
            RequiredPolicy::Resource,
            RequiredPolicy::RetainedRequest,
            RequiredPolicy::SelectedPackage,
        ] {
            let mut app = start_real_restore(policy);
            let (root, markers) = resident_restore_with_work(&mut app, 13);
            let started = app.world().resource::<Time<Real>>().elapsed_secs_f64();
            let mut exits = app
                .world()
                .resource::<Messages<AppExit>>()
                .get_cursor_current();
            // A real pending component completes every two seconds. The cell remains
            // unfinished for 24 seconds, but no inactivity interval approaches ten.
            let interval_frames = ((LANDING_TIMEOUT_SECONDS / 5.0) / DT).ceil() as usize;
            for (completed, entity) in markers.iter().take(12).enumerate() {
                assert_eq!(pending_count(&mut app, root), 13 - completed);
                for _ in 0..interval_frames {
                    app.update();
                    assert_still_restoring(&mut app);
                    assert!(
                        exits
                            .read(app.world().resource::<Messages<AppExit>>())
                            .next()
                            .is_none(),
                        "a progressing restore must not emit an exit: {policy:?}"
                    );
                }
                app.world_mut().entity_mut(*entity).despawn();
                assert_eq!(
                    pending_count(&mut app, root),
                    12 - completed,
                    "the progress event must remove real work under this restore root"
                );
            }
            assert!(
                app.world().resource::<Time<Real>>().elapsed_secs_f64() - started
                    > f64::from(2.0 * LANDING_TIMEOUT_SECONDS)
            );
            assert_eq!(pending_count(&mut app, root), 1);
            assert_still_restoring(&mut app);
            app.world_mut()
                .entity_mut(*markers.last().unwrap())
                .despawn();
            publish_required_restore_ready(&mut app);
            for _ in 0..16 {
                app.update();
                assert!(
                    exits
                        .read(app.world().resource::<Messages<AppExit>>())
                        .next()
                        .is_none()
                );
            }
            assert_eq!(pending_count(&mut app, root), 0);
            assert!(!app.world().contains_resource::<LoadingRefusal>());
            assert!(!app.world().resource::<DoorCrossing>().is_active());
            assert!(app.world_mut().run_system_once(no_door_crossing).unwrap());
            assert!(alpha(&mut app) < 0.01);
            assert_eq!(budget(&app), configured_budget());
            assert!(app.world().resource::<DestinationRequests>().is_empty());
        }
    }

    #[test]
    fn a_stalled_required_restore_refuses_once_with_its_actual_progress_and_inactivity_duration() {
        for policy in [
            RequiredPolicy::Resource,
            RequiredPolicy::RetainedRequest,
            RequiredPolicy::SelectedPackage,
        ] {
            let mut app = start_real_restore(policy);
            let (root, _) = resident_restore_with_work(&mut app, 3);
            let mut exits = app
                .world()
                .resource::<Messages<AppExit>>()
                .get_cursor_current();
            let mut error_exits = 0;
            for _ in 0..((LANDING_TIMEOUT_SECONDS + 3.0 * DT) / DT).ceil() as usize {
                app.update();
                error_exits += exits
                    .read(app.world().resource::<Messages<AppExit>>())
                    .filter(|exit| exit.is_error())
                    .count();
            }
            assert_eq!(
                pending_count(&mut app, root),
                3,
                "the workload truly stalled"
            );
            let reason = app
                .world()
                .get_resource::<LoadingRefusal>()
                .expect("ten seconds without progress must refuse a Required restore")
                .0
                .clone();
            // These are intentional diagnostic contracts: the reason must expose
            // the measured inactivity interval and the current causal pending count.
            assert!(
                reason.contains("restore") && reason.contains("progress"),
                "{reason}"
            );
            assert!(reason.contains("pending_components=3"), "{reason}");
            let stalled = diagnosed_stalled_seconds(&reason);
            assert!(
                stalled.is_finite() && stalled >= f64::from(LANDING_TIMEOUT_SECONDS),
                "{reason}"
            );
            assert!(
                stalled <= f64::from(LANDING_TIMEOUT_SECONDS + 2.0 * DT),
                "{reason}"
            );
            assert_eq!(error_exits, 1, "one terminal error exit: {policy:?}");
            assert!(matches!(
                app.world()
                    .resource::<DoorCrossing>()
                    .active
                    .as_ref()
                    .unwrap()
                    .stage,
                Stage::Refused
            ));
            assert!(alpha(&mut app) >= 0.99);
            assert!(!app.world_mut().run_system_once(no_door_crossing).unwrap());
            assert_eq!(budget(&app), configured_budget());
            assert!(app.world().resource::<DestinationRequests>().is_empty());
            for _ in 0..4 {
                app.update();
                assert_eq!(app.world().resource::<LoadingRefusal>().0, reason);
                assert_eq!(
                    exits
                        .read(app.world().resource::<Messages<AppExit>>())
                        .count(),
                    0
                );
            }
        }
    }

    #[test]
    fn required_restore_inactivity_uses_real_time_and_does_not_age_during_deliberate_pause() {
        let mut app = start_real_restore(RequiredPolicy::RetainedRequest);
        let (root, _) = resident_restore_with_work(&mut app, 2);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
        app.world_mut()
            .resource_mut::<Time<bevy::time::Virtual>>()
            .set_max_delta(Duration::from_millis(250));
        app.world_mut()
            .resource_mut::<Time<bevy::time::Virtual>>()
            .pause();
        let paused_real = app.world().resource::<Time<Real>>().elapsed_secs_f64();
        let paused_virtual = app
            .world()
            .resource::<Time<bevy::time::Virtual>>()
            .elapsed_secs_f64();
        let mut exits = app
            .world()
            .resource::<Messages<AppExit>>()
            .get_cursor_current();
        for _ in 0..11 {
            app.update();
            assert_still_restoring(&mut app);
            assert!(
                exits
                    .read(app.world().resource::<Messages<AppExit>>())
                    .next()
                    .is_none()
            );
        }
        assert!(
            app.world().resource::<Time<Real>>().elapsed_secs_f64() - paused_real
                > f64::from(LANDING_TIMEOUT_SECONDS)
        );
        assert_eq!(
            app.world()
                .resource::<Time<bevy::time::Virtual>>()
                .elapsed_secs_f64(),
            paused_virtual
        );
        app.world_mut()
            .resource_mut::<Time<bevy::time::Virtual>>()
            .unpause();
        let unpaused_virtual = app
            .world()
            .resource::<Time<bevy::time::Virtual>>()
            .elapsed_secs_f64();
        let mut error_exits = 0;
        for _ in 0..11 {
            app.update();
            error_exits += exits
                .read(app.world().resource::<Messages<AppExit>>())
                .filter(|exit| exit.is_error())
                .count();
        }
        assert!(
            app.world()
                .resource::<Time<bevy::time::Virtual>>()
                .elapsed_secs_f64()
                - unpaused_virtual
                < f64::from(LANDING_TIMEOUT_SECONDS),
            "virtual clamping must not hide real inactivity"
        );
        assert_eq!(pending_count(&mut app, root), 2);
        assert!(
            app.world().contains_resource::<LoadingRefusal>(),
            "ten unpaused real seconds, even when virtual time is clamped, must refuse"
        );
        assert_eq!(error_exits, 1);
    }

    #[test]
    fn ordinary_restore_retains_its_recovery_control_without_a_required_terminal_exit() {
        let (mut app, _) = app_with(interior_door());
        press_e(&mut app);
        run_until_black(&mut app);
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .set_failed_for_test(CellKey::Interior(77));
        app.update();
        let (_, _) = resident_restore_with_work(&mut app, 3);
        let mut exits = app
            .world()
            .resource::<Messages<AppExit>>()
            .get_cursor_current();
        for _ in 0..((LANDING_TIMEOUT_SECONDS + FADE_SECONDS + 1.0) / DT).ceil() as usize {
            app.update();
            assert!(
                exits
                    .read(app.world().resource::<Messages<AppExit>>())
                    .next()
                    .is_none()
            );
        }
        assert!(!app.world().contains_resource::<LoadingRefusal>());
        assert!(app.world().resource::<DoorCrossing>().needs_recovery());
        assert!(!app.world_mut().run_system_once(no_door_crossing).unwrap());
        assert!(alpha(&mut app) < 0.01);
        assert_eq!(budget(&app), configured_budget());
        assert!(app.world().resource::<DestinationRequests>().is_empty());
    }

    #[test]
    fn required_restore_retains_its_immediate_asset_failure_control_and_first_terminal_reason() {
        for earlier in [None, Some("earlier ordinary cell failure")] {
            let mut app = start_real_restore(RequiredPolicy::RetainedRequest);
            let (root, _) = resident_restore_with_work(&mut app, 3);
            let mut exits = app
                .world()
                .resource::<Messages<AppExit>>()
                .get_cursor_current();
            if let Some(reason) = earlier {
                crate::streaming::failure::refuse_loading(app.world_mut(), reason.to_owned());
            }
            app.world_mut().spawn((
                crate::streaming::AssetFailed,
                Visibility::Hidden,
                ChildOf(root),
            ));
            app.update();
            let reason = app
                .world()
                .get_resource::<LoadingRefusal>()
                .expect("real Required asset failure refuses immediately")
                .0
                .clone();
            if let Some(earlier) = earlier {
                assert_eq!(reason, earlier);
            } else {
                assert!(
                    reason.contains("restore") && reason.contains("failed to reload"),
                    "{reason}"
                );
            }
            assert_eq!(
                exits
                    .read(app.world().resource::<Messages<AppExit>>())
                    .filter(|exit| exit.is_error())
                    .count(),
                1
            );
            assert!(matches!(
                app.world()
                    .resource::<DoorCrossing>()
                    .active
                    .as_ref()
                    .unwrap()
                    .stage,
                Stage::Refused
            ));
            assert!(alpha(&mut app) >= 0.99);
            assert!(!app.world_mut().run_system_once(no_door_crossing).unwrap());
            assert_eq!(budget(&app), configured_budget());
            assert!(app.world().resource::<DestinationRequests>().is_empty());
            for _ in 0..4 {
                app.update();
                assert_eq!(app.world().resource::<LoadingRefusal>().0, reason);
                assert_eq!(
                    exits
                        .read(app.world().resource::<Messages<AppExit>>())
                        .count(),
                    0
                );
            }
        }
    }
}
