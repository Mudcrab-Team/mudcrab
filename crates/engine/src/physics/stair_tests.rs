//! Headless stair traversal through the production fixed-step WALK controller.

use super::{headless, *};
use bevy::ecs::system::RunSystemOnce;

fn fixed_box(app: &mut App, center: Vec3, half_extents: Vec3) {
    app.world_mut().spawn((
        RigidBody::Fixed,
        Collider::cuboid(half_extents.x, half_extents.y, half_extents.z),
        world_collision_groups(),
        Transform::from_translation(center),
    ));
}

fn probe_step(app: &mut App, position: Vec3, horizontal: Vec3) -> Option<f32> {
    app.world_mut()
        .run_system_once(
            move |context: ReadRapierContext,
                  tuning: Res<MovementTuning>,
                  player: WalkBodyQuery| {
                let (entity, body, capsule, controller, _, _) = player.single().unwrap();
                let pose = Transform {
                    translation: position,
                    ..*body
                };
                walk_step_rise(
                    &context.single().unwrap(),
                    entity,
                    &pose,
                    capsule,
                    controller,
                    &tuning,
                    horizontal,
                )
            },
        )
        .unwrap()
}

fn settle(app: &mut App, position: Vec3) -> Vec3 {
    headless::place_player(app, position);
    for _ in 0..150 {
        app.update();
    }
    let (pose, grounded) = headless::player_pose(app);
    assert!(grounded, "player did not settle: {pose:?}");
    pose
}

fn traverse(app: &mut App, direction: Vec3, speed: f32, target: impl Fn(Vec3) -> bool) -> Vec3 {
    app.insert_resource(WalkIntent {
        wish_dir: direction.normalize(),
        target_speed: speed,
        jump_pressed: false,
    });
    for _ in 0..900 {
        app.update();
        let pose = headless::player_pose(app).0;
        assert!(pose.is_finite());
        let mut query = app.world_mut().query::<&WalkState>();
        assert!(!query.single(app.world()).unwrap().jump_active);
        if target(pose) {
            app.insert_resource(WalkIntent::default());
            // Brake before inspecting support; the controller must stand on the landing.
            for _ in 0..30 {
                app.update();
            }
            let (pose, grounded) = headless::player_pose(app);
            assert!(grounded, "lost support on landing: {pose:?}");
            return pose;
        }
    }
    panic!(
        "stair traversal stalled at {:?}, direction={direction:?}, speed={speed}",
        headless::player_pose(app)
    );
}

#[test]
fn narrow_stair_flights_walk_run_oblique_and_descend() {
    // The BleakFallsTower stair meshes have approximately 24-unit rises/treads.
    // Exercise an entire flight, separate collider seams, and oblique approaches.
    for speed in [80.1, 370.0] {
        for angle in [0.0_f32, 20.0, -20.0, 45.0] {
            for seam in [0.0, 0.25] {
                let mut app = headless::fixture_app();
                for index in 0..16 {
                    let top = (index + 1) as f32 * 24.0;
                    fixed_box(
                        &mut app,
                        Vec3::new(index as f32 * 24.0 + 12.0, top * 0.5, 850.0),
                        Vec3::new((24.0 - seam) * 0.5, top * 0.5, 700.0),
                    );
                }
                fixed_box(
                    &mut app,
                    Vec3::new(642.0, 192.0, 850.0),
                    Vec3::new(258.0, 192.0, 700.0),
                );
                let start = settle(&mut app, Vec3::new(-100.0, 200.0, 600.0));
                let direction = Vec3::new(1.0, 0.0, angle.to_radians().tan());
                let top = traverse(&mut app, direction, speed, |p| p.x >= 440.0);
                assert!(
                    (top.y - start.y - 384.0).abs() < 3.0,
                    "angle={angle}, seam={seam}, {start:?} -> {top:?}"
                );
                let bottom = traverse(&mut app, -direction, speed, |p| p.x <= -100.0);
                assert!(
                    (bottom.y - start.y).abs() < 3.0,
                    "angle={angle}, seam={seam}, {top:?} -> {bottom:?}"
                );
            }
        }
    }
}

/// The spring input of the frame that just ran: the body blended between the last two ticks.
fn blended_feet(app: &App) -> Vec3 {
    let fraction = app.world().resource::<Time<Fixed>>().overstep_fraction();
    app.world().resource::<WalkViewPose>().shown(fraction)
}

/// Run one rendered frame and check the view against the retail follow spring evaluated with the
/// same inputs. Returns the spring input (the blended body position) and the vertical lag of the
/// view.
fn rendered_frame_follows_the_spring(
    app: &mut App,
    camera: Entity,
    delta_secs: f32,
) -> (Vec3, f32) {
    let eye_height = app.world().resource::<MovementTuning>().eye_height;
    let previous = app
        .world()
        .resource::<WalkCameraFollowState>()
        .spring
        .expect("the spring is seeded in WALK");
    app.update();
    let feet = blended_feet(app);
    let expected = camera_follow::follow_spring_step(
        previous,
        camera_follow::FollowInput {
            feet,
            delta_secs,
            controls_driven: true,
            is_on_mount: false,
            view: camera_follow::FollowView::FirstPerson,
        },
    );
    let view = app.world().get::<Transform>(camera).unwrap().translation;
    let eye = feet + Vec3::Y * eye_height;
    assert!(
        view.distance(eye + expected.lag) < 1.0e-3,
        "the view is not the eye node plus the spring lag: view={view:?} eye={eye:?} lag={:?}",
        expected.lag
    );
    assert_eq!(
        app.world().resource::<WalkCameraFollowState>().spring,
        Some(expected.state)
    );
    (feet, view.y - eye.y)
}

/// The WALK view is the eye node plus the retail follow spring's lag (RF-CAM-001 to RF-CAM-007),
/// evaluated once per rendered frame with the frame delta, at 60 and at 20 frames per second.
#[test]
fn camera_follows_the_retail_spring_on_a_production_stair_flight() {
    let mut app = headless::fixture_app();
    for index in 0..32 {
        let top = (index + 1) as f32 * 12.0;
        fixed_box(
            &mut app,
            Vec3::new(index as f32 * 24.0 + 12.0, top * 0.5, 850.0),
            Vec3::new(12.0, top * 0.5, 700.0),
        );
    }
    fixed_box(
        &mut app,
        Vec3::new(1026.0, 192.0, 850.0),
        Vec3::new(258.0, 192.0, 700.0),
    );
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        std::time::Duration::from_secs_f32(1.0 / 60.0),
    ));
    let start = settle(&mut app, Vec3::new(-100.0, 200.0, 850.0));
    let eye_height = MovementTuning::default().eye_height;
    let body = {
        let mut query = app.world_mut().query_filtered::<Entity, With<PlayerBody>>();
        query.single(app.world()).unwrap()
    };
    let camera = {
        let mut query = app
            .world_mut()
            .query_filtered::<Entity, With<StreamingCamera>>();
        query.single(app.world()).unwrap()
    };
    app.world_mut()
        .get_mut::<Transform>(camera)
        .unwrap()
        .translation = start + Vec3::Y * eye_height;
    app.update();

    app.insert_resource(WalkIntent {
        wish_dir: Vec3::X,
        target_speed: 370.0,
        jump_pressed: false,
    });
    let mut previous_body_y = start.y;
    let mut previous_camera_y = start.y + eye_height;
    let mut largest_rise = 0.0_f32;
    let mut deepest_lag = 0.0_f32;
    for _ in 0..900 {
        let (feet, lag) = rendered_frame_follows_the_spring(&mut app, camera, 1.0 / 60.0);
        let camera_y = feet.y + eye_height + lag;
        let rise = feet.y - previous_body_y;
        if rise > 10.0 {
            largest_rise = largest_rise.max(rise);
            assert!(
                camera_y - previous_camera_y < rise,
                "camera followed a physical stair rise instantly: body rise={rise}"
            );
        }
        deepest_lag = deepest_lag.min(lag);
        previous_body_y = feet.y;
        previous_camera_y = camera_y;
        if feet.x >= 820.0 {
            break;
        }
    }
    assert!(
        largest_rise > 1.0,
        "production controller produced no stair rises"
    );
    assert!(
        deepest_lag < -1.0,
        "the view never lagged below its eye target: {deepest_lag}"
    );
    app.insert_resource(WalkIntent::default());
    for _ in 0..60 {
        rendered_frame_follows_the_spring(&mut app, camera, 1.0 / 60.0);
    }
    let top = app.world().get::<Transform>(body).unwrap().translation;
    assert!(
        (top.y - start.y - 384.0).abs() < 3.0,
        "camera work changed the physical stair endpoint: {start:?} -> {top:?}"
    );
    let (_, settled_lag) = rendered_frame_follows_the_spring(&mut app, camera, 1.0 / 60.0);
    assert!(
        settled_lag.abs() < 0.5,
        "the view did not settle onto the eye target: {settled_lag}"
    );

    // Three fixed ticks per rendered frame: the spring still runs once per rendered frame.
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        std::time::Duration::from_secs_f32(1.0 / 20.0),
    ));
    app.insert_resource(WalkIntent {
        wish_dir: -Vec3::X,
        target_speed: 370.0,
        jump_pressed: false,
    });
    let mut descended = false;
    let mut previous_body_y = top.y;
    for _ in 0..300 {
        let (feet, _) = rendered_frame_follows_the_spring(&mut app, camera, 1.0 / 20.0);
        descended |= feet.y < previous_body_y - 6.0;
        previous_body_y = feet.y;
        if feet.x <= -100.0 {
            break;
        }
    }
    assert!(
        descended,
        "production controller produced no supported stair descent"
    );
    app.insert_resource(WalkIntent::default());
    for _ in 0..30 {
        rendered_frame_follows_the_spring(&mut app, camera, 1.0 / 20.0);
    }
    let bottom = app.world().get::<Transform>(body).unwrap().translation;
    assert!(
        (bottom.y - start.y).abs() < 3.0,
        "camera work changed the physical return endpoint: {start:?} -> {bottom:?}"
    );
}

// ---- Stair-climb jitter metric -----------------------------------------------------------------

/// The production stair flight of the spring test: 32 steps of `STAIR_RISE` over `STAIR_TREAD`.
const STAIR_RISE: f32 = 12.0;
const STAIR_TREAD: f32 = 24.0;

fn stair_flight_app() -> App {
    let mut app = headless::fixture_app();
    for index in 0..32 {
        let top = (index + 1) as f32 * STAIR_RISE;
        fixed_box(
            &mut app,
            Vec3::new(index as f32 * STAIR_TREAD + 12.0, top * 0.5, 850.0),
            Vec3::new(12.0, top * 0.5, 700.0),
        );
    }
    fixed_box(
        &mut app,
        Vec3::new(1026.0, 192.0, 850.0),
        Vec3::new(258.0, 192.0, 700.0),
    );
    app
}

/// One rendered frame of a climb.
#[derive(Clone, Copy, Debug)]
struct ClimbFrame {
    /// Time at the end of the frame, in seconds since the climb started.
    time: f64,
    delta: f32,
    /// The body at the latest tick.
    body: Vec3,
    camera: Vec3,
    /// Fixed-clock time of the latest tick.
    tick_time: f64,
    /// The spring weight W after this frame (RF-CAM-003).
    weight: f32,
}

/// Frame times of a display at `hz`. `uneven` varies each frame by up to 30 percent with a fixed
/// sequence (mean unchanged). That is the 7 to 10 ms spread of the frame gaps in the failed
/// #511 window log (`evidence/user-test-20261114/s511-jitter-analysis.md`).
fn frame_times(hz: f32, uneven: bool, count: usize) -> Vec<f32> {
    let mut seed = 0x2545_f491_u32;
    (0..count)
        .map(|_| {
            if !uneven {
                return 1.0 / hz;
            }
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let unit = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
            (1.0 + 0.3 * unit) / hz
        })
        .collect()
}

/// Climb the flight at `speed` with the given frame times. The player stands in WALK first until
/// the spring weight is full (RF-CAM-003: W ramps at 1.5 per second).
fn climb(speed: f32, deltas: &[f32]) -> Vec<ClimbFrame> {
    let mut app = stair_flight_app();
    let start = settle(&mut app, Vec3::new(-100.0, 200.0, 850.0));
    let weight = app
        .world()
        .resource::<WalkCameraFollowState>()
        .spring
        .map(|spring| spring.weight);
    assert_eq!(
        weight,
        Some(1.0),
        "the spring weight is not full before the climb"
    );
    let (body, camera) = {
        let mut bodies = app.world_mut().query_filtered::<Entity, With<PlayerBody>>();
        let mut cameras = app
            .world_mut()
            .query_filtered::<Entity, With<StreamingCamera>>();
        (
            bodies.single(app.world()).unwrap(),
            cameras.single(app.world()).unwrap(),
        )
    };
    app.insert_resource(WalkIntent {
        wish_dir: Vec3::X,
        target_speed: speed,
        jump_pressed: false,
    });
    let mut frames = Vec::new();
    for &delta in deltas {
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs_f32(delta),
        ));
        app.update();
        let world = app.world();
        let body = world.get::<Transform>(body).unwrap().translation;
        let camera = world.get::<Transform>(camera).unwrap().translation;
        frames.push(ClimbFrame {
            time: world.resource::<Time<Virtual>>().elapsed_secs_f64(),
            delta: world.resource::<Time>().delta_secs(),
            body,
            camera,
            tick_time: world.resource::<Time<Fixed>>().elapsed_secs_f64(),
            weight: world
                .resource::<WalkCameraFollowState>()
                .spring
                .map_or(0.0, |spring| spring.weight),
        });
        if body.x > 32.0 * STAIR_TREAD {
            break;
        }
    }
    assert!(
        frames.last().unwrap().body.y > start.y + 30.0 * STAIR_RISE,
        "the climb did not reach the top: {start:?} -> {:?}",
        frames.last().unwrap().body
    );
    frames
}

/// RMS of `values` about their least-squares line over `times`.
fn detrended_rms(times: &[f64], values: &[f64]) -> f64 {
    let n = times.len() as f64;
    let (mean_t, mean_v) = (
        times.iter().sum::<f64>() / n,
        values.iter().sum::<f64>() / n,
    );
    let covariance: f64 = times
        .iter()
        .zip(values)
        .map(|(t, v)| (t - mean_t) * (v - mean_v))
        .sum();
    let variance: f64 = times.iter().map(|t| (t - mean_t).powi(2)).sum();
    let slope = covariance / variance;
    let squares: f64 = times
        .iter()
        .zip(values)
        .map(|(t, v)| (v - mean_v - slope * (t - mean_t)).powi(2))
        .sum();
    (squares / n).sqrt()
}

/// Ripple of the height and of the forward position over the steady middle of the flight, away
/// from the speed-up at the foot and the landing at the top.
#[derive(Clone, Copy, Debug)]
struct Ripple {
    height: f64,
    forward: f64,
}

fn ripple(frames: &[ClimbFrame], position: impl Fn(&ClimbFrame) -> Vec3) -> Ripple {
    let middle: Vec<_> = frames
        .iter()
        .filter(|frame| (4.0 * STAIR_TREAD..28.0 * STAIR_TREAD).contains(&frame.body.x))
        .collect();
    let times: Vec<f64> = middle.iter().map(|frame| frame.time).collect();
    let axis = |pick: fn(Vec3) -> f32| -> Vec<f64> {
        middle
            .iter()
            .map(|frame| f64::from(pick(position(frame))))
            .collect()
    };
    Ripple {
        height: detrended_rms(&times, &axis(|p| p.y)),
        forward: detrended_rms(&times, &axis(|p| p.x)),
    }
}

/// The registry spring (RF-CAM-001 to RF-CAM-007, [`camera_follow::follow_spring_step`]) fed with
/// `feet` once per rendered frame with the frame delta. Returns the camera node plus the lag.
fn replay_spring(frames: &[ClimbFrame], feet: impl Fn(&ClimbFrame) -> Vec3) -> Vec<ClimbFrame> {
    let mut state = camera_follow::FollowSpring::at_rest(feet(&frames[0]), 1.0);
    frames
        .iter()
        .map(|frame| {
            let position = feet(frame);
            let step = camera_follow::follow_spring_step(
                state,
                camera_follow::FollowInput {
                    feet: position,
                    delta_secs: frame.delta,
                    controls_driven: true,
                    is_on_mount: false,
                    view: camera_follow::FollowView::FirstPerson,
                },
            );
            state = step.state;
            ClimbFrame {
                camera: position + step.lag,
                ..*frame
            }
        })
        .collect()
}

/// The retail reference for this climb: the registry spring following this controller's path as
/// retail's per-frame controller (RF-PHYS-003) would present it, on every rendered frame. The
/// horizontal position is the tick path at the frame time. The registry does not settle how a
/// retail stair lift looks (REA queue Q-CC-1 and Q-CC-2): with `continuous_lift` the height moves
/// along the tick path too (a helper ramp, REA m1 section 7), otherwise each lift is one whole step
/// (retail has no smoothing other than the spring, RF-CAM-005). Both see the path one physics tick
/// late, as the fixed-tick controller shows it. Frames before the second tick are dropped.
fn retail_reference(frames: &[ClimbFrame], continuous_lift: bool) -> Vec<ClimbFrame> {
    let mut ticks: Vec<(f64, Vec3)> = Vec::new();
    for frame in frames {
        if ticks.last().is_none_or(|(time, _)| *time < frame.tick_time) {
            ticks.push((frame.tick_time, frame.body));
        }
    }
    let tick = f64::from(PHYSICS_TIMESTEP);
    let usable: Vec<ClimbFrame> = frames
        .iter()
        .copied()
        .filter(|frame| frame.time - tick >= ticks[0].0)
        .collect();
    replay_spring(&usable, |frame| {
        let shown = frame.time - tick;
        let next = ticks
            .partition_point(|(time, _)| *time <= shown)
            .min(ticks.len() - 1);
        let ((t0, p0), (t1, p1)) = (ticks[next - 1], ticks[next]);
        let along = ((shown - t0) / (t1 - t0)).clamp(0.0, 1.0) as f32;
        let path = p0.lerp(p1, along);
        let height = if continuous_lift { path.y } else { p0.y };
        Vec3::new(path.x, height, path.z)
    })
}

/// A per-frame controller on the same flight at a constant speed: the feet rise one whole
/// `STAIR_RISE` at each tread edge. Reported only, to show how much of the ripple comes from the
/// fixed-tick step-up and its speed loss (#590) rather than from the camera.
fn constant_speed_reference(frames: &[ClimbFrame]) -> Vec<ClimbFrame> {
    let middle: Vec<_> = frames
        .iter()
        .filter(|frame| (4.0 * STAIR_TREAD..28.0 * STAIR_TREAD).contains(&frame.body.x))
        .collect();
    let (first, last) = (middle[0], middle[middle.len() - 1]);
    let speed = f64::from(last.body.x - first.body.x) / (last.time - first.time);
    let tread = f64::from(STAIR_TREAD);
    let base =
        f64::from(first.body.y) - f64::from(STAIR_RISE) * (f64::from(first.body.x) / tread).floor();
    let origin = f64::from(first.body.x) - speed * first.time;
    replay_spring(frames, |frame| {
        let x = origin + speed * frame.time;
        let y = base + (x / tread).floor() * f64::from(STAIR_RISE);
        Vec3::new(x as f32, y as f32, frame.body.z)
    })
}

/// Headless stair-climb jitter metric for the WALK camera (#511, #619), with no window.
///
/// The production controller climbs a 12/24 flight at the run speed with 60 Hz physics, at a
/// steady and at an uneven 120 Hz frame rate. Each rendered frame records the camera. The metric is
/// the RMS of the camera height and of its forward position about a straight line in time, over
/// the middle of the flight, in game units.
///
/// Bound: the retail spring model ([`retail_reference`]). It is the ripple that the registry spring
/// (RF-CAM-001 K and c, RF-CAM-003 weight, RF-CAM-007 camera clock) leaves when it follows this
/// controller's path as retail's per-frame controller would present it (RF-PHYS-003). Retail has no
/// other smoothing (RF-CAM-005), so a correct camera keeps this ripple and no more. Because the
/// retail lift shape is not settled, the bound is the larger of the whole-step and the continuous
/// lift. The view may exceed it only by f32 rounding: the engine runs in f32 and the reference in
/// f64, so the tolerance is 1e-4 relative.
///
/// The printed table also shows how much ripple a per-frame controller at a constant speed would
/// leave (the part of the ripple that the fixed-tick step-up adds belongs to #590), the camera of
/// #511 alone (the spring fed with the latest tick) and a rigid camera.
#[test]
fn stair_climb_camera_ripple_stays_within_the_retail_spring_model() {
    const F32_ROUNDING: f64 = 1.0e-4;
    let tuning = MovementTuning::default();
    let eye = Vec3::Y * tuning.eye_height;
    for (label, uneven) in [("steady 120 Hz", false), ("uneven 120 Hz", true)] {
        let frames = climb(tuning.run_speed, &frame_times(120.0, uneven, 2400));
        let view = ripple(&frames, |frame| frame.camera - eye);
        let whole_step = ripple(&retail_reference(&frames, false), |frame| frame.camera);
        let continuous = ripple(&retail_reference(&frames, true), |frame| frame.camera);
        let tick_fed = ripple(&replay_spring(&frames, |frame| frame.body), |frame| {
            frame.camera
        });
        let constant_speed = ripple(&constant_speed_reference(&frames), |frame| frame.camera);
        let rigid = ripple(&frames, |frame| frame.body);
        println!(
            "{label}: view {view:?}; retail model whole-step {whole_step:?}, continuous \
             {continuous:?}; constant-speed controller {constant_speed:?}; spring fed with the \
             latest tick (#511 alone) {tick_fed:?}; rigid camera {rigid:?}"
        );
        let bound = Ripple {
            height: whole_step.height.max(continuous.height) * (1.0 + F32_ROUNDING),
            forward: whole_step.forward.max(continuous.forward) * (1.0 + F32_ROUNDING),
        };
        assert!(
            view.height <= bound.height,
            "{label}: camera height ripple {:.4} is above the retail spring model {:.4}",
            view.height,
            bound.height
        );
        assert!(
            view.forward <= bound.forward,
            "{label}: camera forward ripple {:.4} is above the retail spring model {:.4}",
            view.forward,
            bound.forward
        );
        assert!(
            view.height < rigid.height,
            "{label}: the spring did not reduce the stair ripple of a rigid camera"
        );
    }
}

/// Reports the spring weight during a stair climb at the walk, run and sprint speeds. RF-CAM-003
/// fades the spring out while the smoothed point moves faster than 500 units per second, so a
/// sprint up a stair flight can turn the spring off. Prints only; the weight is retail behaviour.
#[test]
#[ignore = "measure: prints the spring weight per climb speed; run with --ignored --nocapture"]
fn measure_stair_climb_spring_weight_per_speed() {
    let tuning = MovementTuning::default();
    for (label, speed) in [
        ("walk", tuning.walk_speed),
        ("run", tuning.run_speed),
        ("sprint", tuning.run_speed * tuning.sprint_multiplier),
    ] {
        let frames = climb(speed, &frame_times(120.0, true, 2400));
        let eye = Vec3::Y * tuning.eye_height;
        let middle: Vec<_> = frames
            .iter()
            .filter(|frame| (4.0 * STAIR_TREAD..28.0 * STAIR_TREAD).contains(&frame.body.x))
            .collect();
        let weight = middle.iter().map(|frame| frame.weight).fold(1.0, f32::min);
        println!(
            "{label} {speed} u/s: lowest weight {weight:.3}, view {:?}, rigid {:?}",
            ripple(&frames, |frame| frame.camera - eye),
            ripple(&frames, |frame| frame.body)
        );
    }
}

#[test]
fn stairs_keep_rise_and_overhead_clearance_limits() {
    for (rise, ceiling) in [(48.0, false), (24.0, true)] {
        let mut app = headless::fixture_app();
        fixed_box(
            &mut app,
            Vec3::new(100.0, rise * 0.5, 850.0),
            Vec3::new(100.0, rise * 0.5, 100.0),
        );
        if ceiling {
            fixed_box(
                &mut app,
                Vec3::new(0.0, 120.0, 850.0),
                Vec3::new(300.0, 10.0, 100.0),
            );
        }
        let start = settle(&mut app, Vec3::new(-100.0, 55.0, 850.0));
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: 370.0,
            jump_pressed: false,
        });
        for _ in 0..180 {
            app.update();
        }
        let end = headless::player_pose(&mut app).0;
        assert!(
            end.x < -25.0 && end.y < start.y + 3.0,
            "rise={rise}, ceiling={ceiling}, {start:?} -> {end:?}"
        );
    }
}

#[test]
fn unsupported_capsule_does_not_mantle_a_step() {
    let mut app = headless::fixture_app();
    fixed_box(
        &mut app,
        Vec3::new(100.0, 12.0, 850.0),
        Vec3::new(100.0, 12.0, 100.0),
    );
    let start = Vec3::new(-29.0, 62.4, 850.0);
    headless::place_player(&mut app, start);
    // Sync the obstacle and query pose without producing a grounded WALK tick.
    app.insert_resource(MoveMode::Noclip);
    app.update();
    let synced_start = headless::player_pose(&mut app).0;
    assert!(synced_start.distance(start) < 1.0e-4);
    // Prove the geometry offers a step before testing the caller's support gate.
    // Without that gate this pose could rise, despite having no floor support.
    assert!(probe_step(&mut app, start, Vec3::X * (370.0 * PHYSICS_TIMESTEP)).is_some());
    app.insert_resource(MoveMode::Walk);
    let mut query = app.world_mut().query::<&mut WalkState>();
    query
        .single_mut(app.world_mut())
        .unwrap()
        .horizontal_velocity = Vec3::X * 370.0;
    app.insert_resource(WalkIntent {
        wish_dir: Vec3::X,
        target_speed: 370.0,
        jump_pressed: false,
    });
    app.update();
    let end = headless::player_pose(&mut app).0;
    assert!(
        end.y <= start.y + 0.1,
        "unsupported mantle: {start:?} -> {end:?}"
    );
}

#[test]
fn walking_recovers_shallow_floor_skin_gaps() {
    // Cover lost skin, contact at the floor, float-scale overlap, and one unit
    // of solid overlap. All must recover without a multi-step-height pop.
    for start_y in [51.4, 50.4, 50.39, 49.4] {
        let mut app = headless::fixture_app();
        let rest = settle(&mut app, Vec3::new(-100.0, 200.0, 850.0));
        let start = Vec3::new(-100.0, start_y, 850.0);
        // Preserve support output while syncing the disturbed pose without WALK.
        headless::place_player(&mut app, start);
        app.insert_resource(MoveMode::Noclip);
        app.update();
        assert!(headless::player_pose(&mut app).0.distance(start) < 1.0e-4);
        app.insert_resource(MoveMode::Walk);
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: 80.1,
            jump_pressed: false,
        });
        // Exact floor contact can allow tangential motion without a blocking
        // hit. Only the lost-skin case requires immediate clearance recovery;
        // other shallow contacts must not sink further or produce a height pop.
        let contact_y = MovementTuning::default().capsule_standing_height * 0.5;
        let minimum_y = if start_y > contact_y {
            52.0
        } else {
            start_y - 0.05
        };
        for _ in 0..30 {
            app.update();
            let pose = headless::player_pose(&mut app).0;
            assert!(
                pose.y > minimum_y && pose.y < rest.y + 0.5,
                "start_y={start_y}: skin contact sank or produced a height pop: {pose:?}"
            );
        }
        let end = headless::player_pose(&mut app).0;
        assert!(
            end.x > -70.0,
            "start_y={start_y}: skin contact stalled WALK: {end:?}"
        );
    }
}

#[test]
fn skin_recovery_does_not_lift_deep_floor_penetration() {
    let mut app = headless::fixture_app();
    settle(&mut app, Vec3::new(-100.0, 200.0, 850.0));
    assert!(
        probe_step(&mut app, Vec3::new(-100.0, 51.4, 850.0), Vec3::X).is_some(),
        "control: the same floor must offer a shallow skin repair"
    );
    for y in [40.0, 20.0] {
        assert!(
            probe_step(&mut app, Vec3::new(-100.0, y, 850.0), Vec3::X).is_none(),
            "deep floor penetration at y={y} was treated as lost skin clearance"
        );
    }
}

#[test]
fn step_clearance_uses_the_controller_collision_filter() {
    let mut app = headless::fixture_app();
    fixed_box(
        &mut app,
        Vec3::new(100.0, 12.0, 850.0),
        Vec3::new(100.0, 12.0, 100.0),
    );
    // The default controller queries every group. A separate ceiling group must
    // still block the lift, even though player_collision_groups omits GROUP_5.
    app.world_mut().spawn((
        RigidBody::Fixed,
        Collider::cuboid(300.0, 10.0, 100.0),
        CollisionGroups::new(Group::GROUP_5, Group::ALL),
        Transform::from_xyz(0.0, 120.0, 850.0),
    ));
    settle(&mut app, Vec3::new(-100.0, 55.0, 850.0));
    let position = Vec3::new(-29.0, 52.45, 850.0);
    let horizontal = Vec3::X * (370.0 * PHYSICS_TIMESTEP);
    let mut query = app.world_mut().query::<&mut KinematicCharacterController>();
    query.single_mut(app.world_mut()).unwrap().filter_groups = Some(player_collision_groups());
    assert!(
        probe_step(&mut app, position, horizontal).is_some(),
        "control: excluding the separate ceiling group should expose the step"
    );
    query.single_mut(app.world_mut()).unwrap().filter_groups = None;
    assert!(
        probe_step(&mut app, position, horizontal).is_none(),
        "step clearance ignored a ceiling included by the controller filter"
    );
}

#[test]
#[ignore = "requires the immutable local Skyrim asset pack; set MUDCRAB_STAIR_ASSETS"]
fn packed_bleak_falls_tower_stair_flights() {
    let assets = std::env::var_os("MUDCRAB_STAIR_ASSETS").expect("set MUDCRAB_STAIR_ASSETS");
    for (model, x, base, start_z, end_z, top_y, sign) in [
        (
            "nortowerruinsinttopfloorstairs01",
            -272.0,
            896.14,
            208.0,
            -320.0,
            1280.16,
            -1.0,
        ),
        (
            "nortowerruinsinttopfloorstairs02",
            163.0,
            1280.16,
            -320.0,
            90.0,
            1534.68,
            1.0,
        ),
    ] {
        let path = Path::new(&assets).join(format!(
            "meshes/dungeons/nordic/exterior/towers/{model}.glb"
        ));
        let bytes = std::fs::read(&path).unwrap();
        let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let glb: serde_json::Value = serde_json::from_slice(&bytes[20..20 + length]).unwrap();
        let collision: shared::collision::CollisionAsset =
            serde_json::from_value(glb["scenes"][0]["extras"]["mudcrabCollision"].clone()).unwrap();
        assert!(collision.authored && collision.skipped.is_empty());
        for speed in [80.1, 370.0] {
            for angle in [0.0_f32, 5.0, -5.0] {
                let mut app = headless::fixture_app();
                for shape in &collision.shapes {
                    let shared::collision::CollisionShape::Mesh {
                        vertices,
                        triangles,
                    } = shape
                    else {
                        panic!("unexpected tower shape")
                    };
                    app.world_mut().spawn((
                        RigidBody::Fixed,
                        Collider::trimesh(
                            vertices.iter().copied().map(Vec3::from_array).collect(),
                            triangles.clone(),
                        )
                        .unwrap(),
                        world_collision_groups(),
                        Transform::IDENTITY,
                    ));
                }
                fixed_box(
                    &mut app,
                    Vec3::new(x, base - 10.0, start_z),
                    Vec3::new(63.0, 10.0, 80.0),
                );
                fixed_box(
                    &mut app,
                    Vec3::new(x, top_y - 10.0, end_z + sign * 15.0),
                    Vec3::new(63.0, 10.0, 95.0),
                );
                let angle_radians = angle.to_radians();
                let start_x = x - angle_radians.tan() * (end_z - start_z).abs() * 0.5;
                let direction = Vec3::new(angle_radians.sin(), 0.0, sign * angle_radians.cos());
                let start = settle(&mut app, Vec3::new(start_x, base + 200.0, start_z));
                let top = traverse(&mut app, direction, speed, |p| sign * (p.z - end_z) >= 0.0);
                assert!(
                    (top.y - top_y - 52.4).abs() < 3.0,
                    "{model}: {start:?} -> {top:?}"
                );
                let bottom = traverse(&mut app, -direction, speed, |p| {
                    sign * (p.z - start_z) <= 0.0
                });
                assert!(
                    (bottom.y - start.y).abs() < 3.0,
                    "{model}: {top:?} -> {bottom:?}"
                );
                eprintln!(
                    "{model}, speed={speed}, angle={angle}, {start:?} -> {top:?} -> {bottom:?}"
                );
            }
        }
    }
}
