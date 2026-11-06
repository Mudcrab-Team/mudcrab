//! Test-only bridge to the production WALK transition and real Rapier fixture.

use super::*;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayInput {
    schema: u32,
    profile: ReplayProfile,
    runs: Vec<ReplayRun>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayProfile {
    effective_jump_height_creation: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayRun {
    id: String,
    initial_floor_height: f32,
    controller_dt_seconds: Vec<f32>,
}

#[derive(Serialize)]
struct ReplayOutput {
    schema: u32,
    model: &'static str,
    profile_height_consumed: bool,
    runs: Vec<PredictedRun>,
}

#[derive(Serialize)]
struct PredictedRun {
    id: String,
    heights: Vec<f32>,
}

fn predict(run: &ReplayRun, tuning: &MovementTuning) -> PredictedRun {
    let mut state = WalkState::default();
    let mut height = run.initial_floor_height;
    let mut supported = true;
    let mut heights = vec![height];
    for (index, &dt) in run.controller_dt_seconds.iter().enumerate() {
        integrate_walk(
            &mut state,
            &WalkIntent {
                jump_pressed: index == 0,
                ..default()
            },
            tuning,
            dt,
            ResolvedWalk {
                y: height,
                grounded: supported,
            },
        );
        height += state.vertical_velocity() * dt;
        supported = height <= run.initial_floor_height && state.vertical_velocity() <= 0.0;
        if supported {
            height = run.initial_floor_height;
        }
        heights.push(height);
    }
    PredictedRun {
        id: run.id.clone(),
        heights,
    }
}

fn path_from_env(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {name} to an explicit artifact path"))
}

#[test]
#[ignore = "artifact bridge, run through scripts/check_skyrim_jump_runtime.py"]
fn export_production_jump_replay() {
    let input: ReplayInput =
        serde_json::from_slice(&fs::read(path_from_env("MUDCRAB_JUMP_REPLAY_INPUT")).unwrap())
            .expect("schedule-only jump replay input");
    assert_eq!(input.schema, 1);
    assert!(input.profile.effective_jump_height_creation.is_finite());
    assert!(!input.runs.is_empty());
    let mut ids = HashSet::new();
    for run in &input.runs {
        assert!(!run.id.is_empty() && ids.insert(&run.id));
        assert!(run.initial_floor_height.is_finite());
        assert!(!run.controller_dt_seconds.is_empty());
        assert!(
            run.controller_dt_seconds
                .iter()
                .all(|dt| dt.is_finite() && *dt > 0.0 && *dt <= 0.25)
        );
    }
    let tuning = MovementTuning {
        jump_height: JumpHeight::new(
            input.profile.effective_jump_height_creation,
            "independently verified native replay profile".into(),
        )
        .unwrap(),
        ..default()
    };
    let output = ReplayOutput {
        schema: 1,
        model: "production_native_vertical_motion_with_recorded_native_dt",
        profile_height_consumed: true,
        runs: input.runs.iter().map(|run| predict(run, &tuning)).collect(),
    };
    fs::write(
        path_from_env("MUDCRAB_JUMP_REPLAY_OUTPUT"),
        serde_json::to_vec_pretty(&output).unwrap(),
    )
    .unwrap();
}

#[test]
fn native_first_launch_step() {
    // Direct parameter snapshots from the three original training launches.
    let jump_height = 78.28_f32;
    let native_acceleration = 686.614_2_f32;
    let dt = 1.0 / 60.0;
    let expected_delta = ((2.0 * native_acceleration * jump_height).sqrt()
        - native_acceleration * 1.35_f32 * dt)
        * dt;
    let predicted = predict(
        &ReplayRun {
            id: "native-first-launch-step".into(),
            initial_floor_height: 0.0,
            controller_dt_seconds: vec![dt],
        },
        &MovementTuning {
            jump_height: JumpHeight::new(jump_height, "native training profile".into()).unwrap(),
            ..default()
        },
    );
    assert!(
        (predicted.heights[1] - expected_delta).abs() <= 0.0001,
        "first resolved free-flight rise {} differs from native {expected_delta}",
        predicted.heights[1]
    );
}

#[derive(Resource, Default)]
struct JumpCapture {
    armed: bool,
    jump_next: bool,
    walk: WalkIntent,
    samples: Vec<ResolvedSample>,
}

#[derive(Clone, Serialize)]
struct ResolvedSample {
    position: [f32; 3],
    kcc_supported: bool,
    desired_translation: [f32; 3],
    effective_translation: [f32; 3],
    collision_count: usize,
    vertical_velocity_creation: f32,
    observed_jump_apex: f32,
    fixed_dt_seconds: f32,
}

fn press_jump_once(mut capture: ResMut<JumpCapture>, mut intent: ResMut<WalkIntent>) {
    if capture.armed {
        *intent = capture.walk;
        intent.jump_pressed = capture.jump_next;
        capture.jump_next = false;
    }
}

fn record_after_writeback(
    mut capture: ResMut<JumpCapture>,
    fixed_time: Res<Time<Fixed>>,
    player: Query<(&Transform, &KinematicCharacterControllerOutput, &WalkState), With<PlayerBody>>,
) {
    if capture.armed {
        let (body, output, state) = player.single().expect("fixture player with KCC output");
        capture.samples.push(ResolvedSample {
            position: body.translation.to_array(),
            kcc_supported: output.grounded,
            desired_translation: output.desired_translation.to_array(),
            effective_translation: output.effective_translation.to_array(),
            collision_count: output.collisions.len(),
            vertical_velocity_creation: state.vertical_velocity(),
            observed_jump_apex: state.jump_apex(),
            fixed_dt_seconds: fixed_time.delta_secs(),
        });
    }
}

#[test]
#[ignore = "real headless Bevy/Rapier capture, requires MUDCRAB_JUMP_HEADLESS_OUTPUT"]
fn export_headless_jump() {
    let mut app = headless::fixture_app_with(|app| {
        app.init_resource::<JumpCapture>()
            .add_systems(FixedUpdate, press_jump_once.before(walk_movement_system))
            .add_systems(
                FixedUpdate,
                record_after_writeback.after(PhysicsSet::Writeback),
            );
    });
    headless::place_player(&mut app, Vec3::new(0.0, 200.0, 500.0));
    for _ in 0..240 {
        app.update();
    }
    let mut query = app
        .world_mut()
        .query_filtered::<(&Transform, &KinematicCharacterControllerOutput), With<PlayerBody>>();
    let (body, output) = query.single(app.world()).unwrap();
    let rest = body.translation;
    assert!(output.grounded, "raw KCC support missing before jump");
    {
        let mut capture = app.world_mut().resource_mut::<JumpCapture>();
        capture.armed = true;
        capture.jump_next = true;
    }
    for _ in 0..180 {
        app.update();
    }
    let capture = app.world().resource::<JumpCapture>();
    let samples = &capture.samples;
    assert!(!samples.is_empty());
    assert!(
        samples
            .iter()
            .all(|sample| sample.position.iter().all(|v| v.is_finite()))
    );
    let apex_index = samples
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.position[1].total_cmp(&b.position[1]))
        .unwrap()
        .0;
    let landing_index = samples
        .iter()
        .enumerate()
        .skip(apex_index + 1)
        .find(|(_, sample)| sample.kcc_supported && (sample.position[1] - rest.y).abs() < 2.0)
        .map(|(index, _)| index)
        .expect("raw KCC landing after apex");
    assert!(samples[apex_index].position[1] - rest.y > 10.0);
    assert!(samples.iter().any(|sample| !sample.kcc_supported));
    assert!(samples.last().unwrap().kcc_supported);
    let report = serde_json::json!({
        "schema": 1,
        "surface": "production PhysicsFixturePlugin flat floor; raw post-Writeback Transform/KCC",
        "dt_seconds": PHYSICS_TIMESTEP,
        "effective_jump_height_creation": app.world().resource::<MovementTuning>().jump_height.creation(),
        "jump_height_source": app.world().resource::<MovementTuning>().jump_height.source(),
        "rest_position": rest.to_array(),
        "first_delta_y": samples[0].position[1] - rest.y,
        "sampled_apex_rise": samples[apex_index].position[1] - rest.y,
        "apex_tick": apex_index + 1,
        "landing_tick": landing_index + 1,
        "landing_delta_y": samples[landing_index].position[1] - rest.y,
        "samples": samples,
    });
    fs::write(
        path_from_env("MUDCRAB_JUMP_HEADLESS_OUTPUT"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}

fn capture_app() -> App {
    headless::fixture_app_with(|app| {
        app.init_resource::<JumpCapture>()
            .add_systems(FixedUpdate, press_jump_once.before(walk_movement_system))
            .add_systems(
                FixedUpdate,
                record_after_writeback.after(PhysicsSet::Writeback),
            );
    })
}

fn body_entity(app: &mut App) -> Entity {
    app.world_mut()
        .query_filtered::<Entity, With<PlayerBody>>()
        .single(app.world())
        .unwrap()
}

fn settle(app: &mut App, position: Vec3) -> Vec3 {
    headless::place_player(app, position);
    for _ in 0..240 {
        app.update();
    }
    let entity = body_entity(app);
    assert!(
        app.world()
            .get::<KinematicCharacterControllerOutput>(entity)
            .unwrap()
            .grounded
    );
    app.world().get::<Transform>(entity).unwrap().translation
}

fn capture_jump(app: &mut App, ticks: usize) -> Vec<ResolvedSample> {
    {
        let mut capture = app.world_mut().resource_mut::<JumpCapture>();
        capture.armed = true;
        capture.jump_next = true;
        capture.samples.clear();
    }
    for _ in 0..ticks {
        app.update();
    }
    app.world().resource::<JumpCapture>().samples.clone()
}

#[test]
fn actual_rapier_launch_apex_landing_and_repeated_jump_match_production_transition() {
    let mut app = capture_app();
    let rest = settle(&mut app, Vec3::new(0.0, 200.0, 500.0));
    let tuning = app.world().resource::<MovementTuning>().clone();
    let predicted = predict(
        &ReplayRun {
            id: "fixed-60-flat-floor".into(),
            initial_floor_height: rest.y,
            controller_dt_seconds: vec![PHYSICS_TIMESTEP; 180],
        },
        &tuning,
    );
    let expected_apex = predicted.heights.iter().copied().reduce(f32::max).unwrap() - rest.y;
    for _ in 0..2 {
        let samples = capture_jump(&mut app, 180);
        assert!((samples[0].position[1] - predicted.heights[1]).abs() < 0.001);
        assert!(samples[0].desired_translation[1] > 0.0);
        let apex = samples
            .iter()
            .map(|sample| sample.position[1] - rest.y)
            .reduce(f32::max)
            .unwrap();
        assert!(
            (apex - expected_apex).abs() < 0.02,
            "actual apex {apex}, transition apex {expected_apex}"
        );
        assert!(samples.iter().any(|sample| !sample.kcc_supported));
        let last = samples.last().unwrap();
        assert!(last.kcc_supported && (last.position[1] - rest.y).abs() < 0.01);
        assert!((last.observed_jump_apex - apex).abs() < 0.01);
    }
    let mut query = app.world_mut().query::<&RapierConfiguration>();
    for config in query.iter(app.world()) {
        assert_eq!(config.gravity, Vec3::new(0.0, -900.0, 0.0));
    }
}

#[test]
fn actual_ceiling_plateau_applies_history_gravity_and_returns_to_support() {
    let mut app = capture_app();
    let rest = settle(&mut app, Vec3::new(0.0, 200.0, 500.0));
    let body_half_height = body_height_above_feet(app.world().resource::<MovementTuning>());
    app.world_mut().spawn((
        RigidBody::Fixed,
        Collider::cuboid(100.0, 5.0, 100.0),
        Transform::from_xyz(rest.x, rest.y + body_half_height + 35.0, rest.z),
        world_collision_groups(),
    ));
    // Publish the ceiling into the query pipeline before takeoff.
    app.update();
    let samples = capture_jump(&mut app, 180);
    let apex = samples
        .iter()
        .map(|sample| sample.position[1] - rest.y)
        .reduce(f32::max)
        .unwrap();
    assert!((25.0..=32.0).contains(&apex), "ceiling-clipped apex {apex}");
    let plateau = samples
        .windows(3)
        .find(|window| {
            window
                .iter()
                .all(|sample| sample.desired_translation[1] > 0.1 && sample.collision_count > 0)
                && (window[0].position[1] - window[1].position[1]).abs() < 0.0001
                && (window[1].position[1] - window[2].position[1]).abs() < 0.0001
        })
        .expect("resolved ceiling plateau while the controller still requests ascent");
    let expected_loss = 9.81_f32 * 1.35 * plateau[2].fixed_dt_seconds * f32::from_bits(0x428b_fb85);
    let actual_loss = plateau[1].vertical_velocity_creation - plateau[2].vertical_velocity_creation;
    assert!(
        (actual_loss - expected_loss).abs() < 0.001,
        "plateau gravity loss {actual_loss}, expected {expected_loss}"
    );
    let last = samples.last().unwrap();
    assert!(last.kcc_supported && (last.position[1] - rest.y).abs() < 0.01);
    assert!((last.observed_jump_apex - apex).abs() < 0.01);
}

#[test]
fn actual_ledge_fall_lands_lower_and_next_jump_starts_at_new_height() {
    let mut app = capture_app();
    app.world_mut().spawn((
        RigidBody::Fixed,
        Collider::cuboid(50.0, 10.0, 70.0),
        Transform::from_xyz(-400.0, 110.0, 500.0),
        world_collision_groups(),
    ));
    let upper_rest = settle(&mut app, Vec3::new(-400.0, 300.0, 500.0));
    {
        let mut capture = app.world_mut().resource_mut::<JumpCapture>();
        capture.armed = true;
        capture.walk = WalkIntent {
            wish_dir: Vec3::X,
            target_speed: 180.0,
            ..default()
        };
    }
    for _ in 0..80 {
        app.update();
    }
    let samples = &app.world().resource::<JumpCapture>().samples;
    assert!(
        samples
            .iter()
            .any(|sample| !sample.kcc_supported && sample.vertical_velocity_creation < -30.0)
    );
    assert!(
        samples
            .iter()
            .all(|sample| sample.observed_jump_apex == 0.0)
    );
    let lower_rest = samples.last().unwrap();
    assert!(lower_rest.kcc_supported);
    assert!((upper_rest.y - lower_rest.position[1] - 120.0).abs() < 0.1);
    let lower_y = lower_rest.position[1];
    app.world_mut().resource_mut::<JumpCapture>().walk = WalkIntent::default();
    let next = capture_jump(&mut app, 180);
    let apex = next
        .iter()
        .map(|sample| sample.position[1] - lower_y)
        .reduce(f32::max)
        .unwrap();
    assert!(apex > 60.0 && apex < 76.0, "new-origin apex {apex}");
    assert!((next.last().unwrap().observed_jump_apex - apex).abs() < 0.02);
    assert!(next.last().unwrap().kcc_supported);
}

#[test]
fn fixed_clock_pause_preserves_actual_flight_and_resume_continues_it() {
    let mut app = capture_app();
    settle(&mut app, Vec3::new(0.0, 200.0, 500.0));
    capture_jump(&mut app, 8);
    let entity = body_entity(&mut app);
    let before_position = app.world().get::<Transform>(entity).unwrap().translation;
    let before = *app.world().get::<WalkState>(entity).unwrap();
    assert!(before.jump_active() && before.jump_apex() > 0.0);
    let sample_count = app.world().resource::<JumpCapture>().samples.len();
    app.world_mut().resource_mut::<Time<Virtual>>().pause();
    for _ in 0..30 {
        app.update();
    }
    assert_eq!(
        app.world().resource::<JumpCapture>().samples.len(),
        sample_count
    );
    assert_eq!(
        app.world().get::<Transform>(entity).unwrap().translation,
        before_position
    );
    let paused = app.world().get::<WalkState>(entity).unwrap();
    assert_eq!(paused.vertical_velocity(), before.vertical_velocity());
    assert_eq!(paused.jump_apex(), before.jump_apex());
    app.world_mut().resource_mut::<Time<Virtual>>().unpause();
    for _ in 0..180 {
        app.update();
    }
    assert!(
        app.world()
            .get::<KinematicCharacterControllerOutput>(entity)
            .unwrap()
            .grounded
    );
    assert!(!app.world().get::<WalkState>(entity).unwrap().jump_active());
}

#[test]
fn actual_flight_history_clears_on_existing_motion_resets() {
    use bevy::ecs::system::RunSystemOnce;
    let mut app = capture_app();
    settle(&mut app, Vec3::new(0.0, 200.0, 500.0));
    capture_jump(&mut app, 8);
    let entity = body_entity(&mut app);
    let flight = *app.world().get::<WalkState>(entity).unwrap();
    assert!(flight.jump_apex() > 0.0);
    for reset in 0..4 {
        *app.world_mut().get_mut::<WalkState>(entity).unwrap() = flight;
        app.world_mut().resource_mut::<WalkSpawnHold>().active = false;
        app.insert_resource(MoveMode::Walk);
        match reset {
            0 => {
                crate::console::execute_line(app.world_mut(), "tcl");
            }
            1 => {
                app.insert_resource(CursorCapture::Captured);
                app.world_mut()
                    .resource_mut::<ButtonInput<KeyCode>>()
                    .press(KeyCode::F3);
                app.world_mut()
                    .run_system_once(cursor_lifecycle_system)
                    .unwrap();
                app.world_mut()
                    .resource_mut::<ButtonInput<KeyCode>>()
                    .clear();
            }
            2 => {
                app.world_mut().resource_mut::<WalkSpawnHold>().active = true;
                app.world_mut()
                    .run_system_once(walk_movement_system)
                    .unwrap();
            }
            3 => {
                app.world_mut().write_message(TeleportPlayer {
                    position: Vec3::new(0.0, 0.0, 500.0),
                    sampled_ground_y: None,
                    yaw: 0.0,
                });
                app.world_mut()
                    .run_system_once(apply_teleport_player)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let state = app.world().get::<WalkState>(entity).unwrap();
        assert_eq!(state.vertical_velocity(), 0.0, "reset {reset}");
        assert_eq!(state.jump_apex(), 0.0, "reset {reset}");
        assert!(!state.jump_active() && !state.grounded(), "reset {reset}");
    }
}

#[test]
fn signed_zero_heights_and_resolved_history_use_native_motion() {
    let tuning = |height| MovementTuning {
        jump_height: JumpHeight::new(height, "explicit behavior test".into()).unwrap(),
        ..default()
    };
    let run = ReplayRun {
        id: "nonuniform-native-steps".into(),
        initial_floor_height: 110.0,
        controller_dt_seconds: [1.0 / 30.0, 1.0 / 120.0, 1.0 / 60.0].repeat(50),
    };
    assert_eq!(
        predict(&run, &tuning(76.0)).heights,
        predict(&run, &tuning(-76.0)).heights
    );
    assert!(
        predict(&run, &tuning(0.0))
            .heights
            .iter()
            .all(|&height| height == 110.0)
    );
    assert!(JumpHeight::new(f32::INFINITY, "invalid".into()).is_err());
    assert!(JumpHeight::new(f32::NAN, "invalid".into()).is_err());

    let tuning = tuning(76.0);
    let mut state = WalkState::default();
    integrate_walk(
        &mut state,
        &WalkIntent {
            jump_pressed: true,
            ..default()
        },
        &tuning,
        1.0 / 60.0,
        ResolvedWalk {
            y: 110.0,
            grounded: true,
        },
    );
    let acceleration = 9.81_f32 * f32::from_bits(0x428b_fb85);
    for (y, dt, multiplier) in [
        (120.0, 1.0 / 120.0, 1.0),
        (120.0, 1.0 / 30.0, 1.35),
        (119.0, 1.0 / 60.0, 1.35),
    ] {
        let before = state.vertical_velocity();
        integrate_walk(
            &mut state,
            &WalkIntent::default(),
            &tuning,
            dt,
            ResolvedWalk { y, grounded: false },
        );
        assert!(
            (before - state.vertical_velocity() - acceleration * multiplier * dt).abs() < 0.0001
        );
        assert_eq!(state.jump_apex(), 10.0);
    }
}

#[test]
fn actual_walk_uses_fixed_time_delta_for_velocity_and_displacement() {
    use bevy::time::TimeUpdateStrategy;
    let mut app = capture_app();
    let rest = settle(&mut app, Vec3::new(0.0, 200.0, 500.0));
    app.insert_resource(Time::<Fixed>::from_hz(30.0));
    app.insert_resource(TimeUpdateStrategy::ManualDuration(
        std::time::Duration::from_secs_f32(1.0 / 30.0),
    ));
    let samples = capture_jump(&mut app, 1);
    assert_eq!(samples.len(), 1);
    let sample = &samples[0];
    assert!((sample.fixed_dt_seconds - 1.0 / 30.0).abs() < 0.000001);
    let prediction = predict(
        &ReplayRun {
            id: "actual-fixed-delta".into(),
            initial_floor_height: rest.y,
            controller_dt_seconds: vec![sample.fixed_dt_seconds],
        },
        app.world().resource::<MovementTuning>(),
    );
    let expected_rise = prediction.heights[1] - rest.y;
    assert!((sample.desired_translation[1] - expected_rise).abs() < 0.0001);
    assert!((sample.position[1] - prediction.heights[1]).abs() < 0.001);
}
