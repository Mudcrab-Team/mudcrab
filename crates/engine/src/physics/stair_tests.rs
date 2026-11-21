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
        assert!(!query.single(app.world()).unwrap().jump_active());
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
    /// The spring input of the frame: the body blended between the last two ticks.
    feet: Vec3,
    camera: Vec3,
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

/// Frame times like Taylor's WALK session of the combined build (trace
/// `evidence/user-test-20261115/s619-stairs-trace.csv`, 813 WALK frames): quantiles 0, 10, 50, 90
/// and 100 percent at 17.9, 20.4, 33.9, 74.6 and 121 ms, so 1 to 8 physics ticks per frame. The
/// fixed sequence draws from that distribution.
fn trace_like_frame_times(count: usize) -> Vec<f32> {
    const QUANTILES: [(f32, f32); 5] = [
        (0.0, 0.0179),
        (0.1, 0.0204),
        (0.5, 0.0339),
        (0.9, 0.0746),
        (1.0, 0.121),
    ];
    let mut seed = 0x9e37_79b9_u32;
    (0..count)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let unit = seed as f32 / u32::MAX as f32;
            let upper = QUANTILES
                .iter()
                .position(|(q, _)| *q >= unit)
                .unwrap()
                .max(1);
            let ((q0, t0), (q1, t1)) = (QUANTILES[upper - 1], QUANTILES[upper]);
            t0 + (t1 - t0) * (unit - q0) / (q1 - q0)
        })
        .collect()
}

/// Every fixed tick of a run: fixed-clock time and body position.
#[derive(Default, Resource)]
struct TickLog(Vec<(f64, Vec3)>);

fn log_tick(
    mut log: ResMut<TickLog>,
    fixed: Res<Time<Fixed>>,
    player: Query<&Transform, With<PlayerBody>>,
) {
    if let Ok(body) = player.single() {
        log.0.push((fixed.elapsed_secs_f64(), body.translation));
    }
}

/// Climb the flight from the bottom or, with `descend`, walk down it from the top landing, with
/// the given frame times. The player stands in WALK first until the spring weight is full
/// (RF-CAM-003: W ramps at 1.5 per second). Returns each rendered frame and each fixed tick.
fn traverse_flight(
    speed: f32,
    deltas: &[f32],
    descend: bool,
) -> (Vec<ClimbFrame>, Vec<(f64, Vec3)>) {
    let mut app = stair_flight_app();
    app.init_resource::<TickLog>()
        .add_systems(FixedUpdate, log_tick.after(walk_view_end_tick));
    let start = if descend {
        settle(&mut app, Vec3::new(1000.0, 500.0, 850.0))
    } else {
        settle(&mut app, Vec3::new(-100.0, 200.0, 850.0))
    };
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
        wish_dir: if descend { -Vec3::X } else { Vec3::X },
        target_speed: speed,
        jump_pressed: false,
    });
    app.world_mut().resource_mut::<TickLog>().0.clear();
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
            feet: blended_feet(&app),
            camera,
            weight: world
                .resource::<WalkCameraFollowState>()
                .spring
                .map_or(0.0, |spring| spring.weight),
        });
        if (descend && body.x < -STAIR_TREAD) || (!descend && body.x > 32.0 * STAIR_TREAD) {
            break;
        }
    }
    let end = frames.last().unwrap().body.y;
    assert!(
        if descend {
            end < start.y - 30.0 * STAIR_RISE
        } else {
            end > start.y + 30.0 * STAIR_RISE
        },
        "the flight was not traversed (descend={descend}): {start:?} -> {:?}",
        frames.last().unwrap().body
    );
    let ticks = std::mem::take(&mut app.world_mut().resource_mut::<TickLog>().0);
    (frames, ticks)
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
fn retail_reference(
    frames: &[ClimbFrame],
    ticks: &[(f64, Vec3)],
    continuous_lift: bool,
) -> Vec<ClimbFrame> {
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
/// The production controller climbs and descends a 12/24 flight at the run speed with 60 Hz
/// physics, at a steady and an uneven 120 Hz frame rate and at the frame times of Taylor's trace
/// (about 27 Hz, 1 to 8 ticks per frame, [`trace_like_frame_times`]). Each rendered frame records the camera. The metric is
/// the RMS of the camera height and of its forward position about a straight line in time, over
/// the middle of the flight, in game units.
///
/// Bound: the retail spring model ([`retail_reference`]). It is the ripple that the registry spring
/// (RF-CAM-001 K and c, RF-CAM-003 weight, RF-CAM-007 camera clock) leaves when it follows this
/// controller's path as retail's per-frame controller would present it (RF-PHYS-003). Retail has no
/// other smoothing (RF-CAM-005), so a correct camera keeps this ripple and no more. Because the
/// retail lift shape is not settled, the bound is the larger of the whole-step and the continuous
/// lift. The view may exceed it only by f32 rounding: the engine runs in f32 and the reference in
/// f64, so the tolerance is 0.1 percent (1e-3 relative, `F32_ROUNDING` below).
///
/// The printed table also shows how much ripple a per-frame controller at a constant speed would
/// leave (the part of the ripple that the fixed-tick step-up adds belongs to #590), the camera of
/// #511 alone (the spring fed with the latest tick) and a rigid camera.
#[test]
fn stair_climb_camera_ripple_stays_within_the_retail_spring_model() {
    const F32_ROUNDING: f64 = 1.0e-3;
    let tuning = MovementTuning::default();
    let eye = Vec3::Y * tuning.eye_height;
    // The third value says whether the spring must beat a rigid camera. At the trace pacing it
    // need not: the momentum term `c * T * V_prev` scales with `T / T_prev` (RF-CAM-001), so the
    // registry spring itself leaves a ripple as large as a rigid camera's at those frame times,
    // even for a per-frame controller at a constant speed.
    let pacings = [
        ("steady 120 Hz", frame_times(120.0, false, 2400), true),
        ("uneven 120 Hz", frame_times(120.0, true, 2400), true),
        ("Taylor's trace pacing", trace_like_frame_times(600), false),
    ];
    for descend in [false, true] {
        for (pacing, deltas, beats_rigid) in &pacings {
            let label = format!("{} {pacing}", if descend { "descent" } else { "climb" });
            let (frames, ticks) = traverse_flight(tuning.run_speed, deltas, descend);
            let view = ripple(&frames, |frame| frame.camera - eye);
            let whole_step = ripple(&retail_reference(&frames, &ticks, false), |frame| {
                frame.camera
            });
            let continuous = ripple(&retail_reference(&frames, &ticks, true), |frame| {
                frame.camera
            });
            let tick_fed = ripple(&replay_spring(&frames, |frame| frame.body), |frame| {
                frame.camera
            });
            let constant_speed = ripple(&constant_speed_reference(&frames), |frame| frame.camera);
            let rigid = ripple(&frames, |frame| frame.body);
            println!(
                "{label}: view {view:?}; retail model whole-step {whole_step:?}, continuous \
                 {continuous:?}; constant-speed controller {constant_speed:?}; spring fed with \
                 the latest tick (#511 alone) {tick_fed:?}; rigid camera {rigid:?}"
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
                !beats_rigid || view.height < rigid.height,
                "{label}: the spring did not reduce the stair ripple of a rigid camera"
            );
        }
    }
}

/// Prints what the controller does at each tick of a descent of the 12/24 flight at the run speed:
/// the body height, the grounded flag and the vertical velocity. It answers whether the body steps
/// down while it stays grounded, or falls and lands on every tread (Taylor's "falling down the
/// stairs", #590). Retail keeps the player on the ground across a down-step by casting 70 units
/// down and pulling it onto the surface (RF-CC-006, `fUnsupportCastLength`); Mudcrab's
/// the old `ground_snap` was 12 units, a tuning value (REA m1 section 5); the controller has none.
#[test]
#[ignore = "measure: prints the per-tick descent profile; run with --ignored --nocapture"]
fn measure_stair_descent_per_tick_profile() {
    let tuning = MovementTuning::default();
    let mut app = stair_flight_app();
    settle(&mut app, Vec3::new(1000.0, 500.0, 850.0));
    app.insert_resource(WalkIntent {
        wish_dir: -Vec3::X,
        target_speed: tuning.run_speed,
        jump_pressed: false,
    });
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        std::time::Duration::from_secs_f32(PHYSICS_TIMESTEP),
    ));
    println!("tick,x,y,dy,grounded,vertical_velocity");
    let mut previous_y = None;
    let (mut airborne_ticks, mut grounded_ticks, mut deepest_fall) = (0, 0, 0.0_f32);
    for tick in 0..400 {
        app.update();
        let (pose, _) = headless::player_pose(&mut app);
        let state = *app
            .world_mut()
            .query::<&WalkState>()
            .single(app.world())
            .unwrap();
        let dy = previous_y.map_or(0.0, |y| pose.y - y);
        previous_y = Some(pose.y);
        if (0.0..=32.0 * STAIR_TREAD).contains(&pose.x) && pose.y < 32.0 * STAIR_RISE + 100.0 {
            if state.grounded() {
                grounded_ticks += 1;
            } else {
                airborne_ticks += 1;
                deepest_fall = deepest_fall.min(state.vertical_velocity());
            }
            println!(
                "{tick},{:.1},{:.2},{dy:.2},{},{:.1}",
                pose.x,
                pose.y,
                u8::from(state.grounded()),
                state.vertical_velocity()
            );
        }
        if pose.x < -STAIR_TREAD {
            break;
        }
    }
    println!(
        "on the flight: {grounded_ticks} grounded ticks, {airborne_ticks} airborne ticks, fastest fall {deepest_fall:.0} u/s"
    );
}

/// Walks up and down a steep ramp (the trace of Taylor's stair descent has a slope of about 45
/// degrees, 5.4 to 8 units of drop per tick for 2.3 to 6.2 units forward) at the walk, run and
/// sprint speeds, and prints the horizontal speed, the speed along the surface and the grounded
/// ticks. The controller holds the horizontal speed, so the speed along the surface grows with the
/// slope. Whether the retail character proxy holds the horizontal or the surface speed is not in
/// the registry (REA queue Q-CC-1); this only reports Mudcrab's behaviour (#590).
#[test]
#[ignore = "measure: prints speeds on a steep ramp; run with --ignored --nocapture"]
fn measure_steep_ramp_speed_along_the_surface() {
    let tuning = MovementTuning::default();
    let slope = 45.0_f32.to_radians();
    for descend in [false, true] {
        for (label, speed) in [
            ("walk", tuning.walk_speed),
            ("run", tuning.run_speed),
            ("sprint", tuning.run_speed * tuning.sprint_multiplier),
        ] {
            let mut app = headless::fixture_app();
            // A 2000-unit long, 20-unit thick ramp that rises toward +X, and a flat landing at each end.
            let (length, half_thickness) = (2000.0_f32, 10.0_f32);
            let up = Vec3::new(-slope.sin(), slope.cos(), 0.0);
            let centre =
                Vec3::new(slope.cos(), slope.sin(), 0.0) * (length * 0.5) - up * half_thickness;
            app.world_mut().spawn((
                RigidBody::Fixed,
                Collider::cuboid(length * 0.5, half_thickness, 700.0),
                world_collision_groups(),
                Transform {
                    translation: centre,
                    rotation: Quat::from_rotation_z(slope),
                    ..default()
                },
            ));
            let top = Vec3::new(length * slope.cos(), length * slope.sin(), 0.0);
            fixed_box(
                &mut app,
                top + Vec3::new(300.0, -half_thickness, 0.0),
                Vec3::new(300.0, half_thickness, 700.0),
            );
            fixed_box(
                &mut app,
                Vec3::new(-300.0, -half_thickness, 0.0),
                Vec3::new(300.0, half_thickness, 700.0),
            );
            let start = if descend {
                Vec3::new(top.x + 100.0, top.y + 80.0, 0.0)
            } else {
                Vec3::new(-100.0, 80.0, 0.0)
            };
            settle(&mut app, start);
            app.insert_resource(WalkIntent {
                wish_dir: if descend { -Vec3::X } else { Vec3::X },
                target_speed: speed,
                jump_pressed: false,
            });
            let (mut grounded, mut airborne) = (0, 0);
            let (mut horizontal, mut surface, mut vertical) = (Vec::new(), Vec::new(), Vec::new());
            let mut previous = headless::player_pose(&mut app).0;
            for _ in 0..240 {
                app.update();
                let (pose, _) = headless::player_pose(&mut app);
                let state = *app
                    .world_mut()
                    .query::<&WalkState>()
                    .single(app.world())
                    .unwrap();
                let on_ramp = (300.0..top.x - 300.0).contains(&pose.x);
                if on_ramp {
                    if state.grounded() {
                        grounded += 1;
                    } else {
                        airborne += 1;
                    }
                    let step = (pose - previous) / PHYSICS_TIMESTEP;
                    horizontal.push(step.x.abs());
                    vertical.push(step.y.abs());
                    surface.push((step.x * step.x + step.y * step.y).sqrt());
                }
                previous = pose;
            }
            // A walk that stalls before the ramp window has no samples: say so, instead of printing
            // zero speeds that were never measured. A zero speed of a body that did enter the
            // window is a real result and is printed.
            if surface.is_empty() {
                println!(
                    "{} {label}: wish {speed:.0} u/s: NOT MEASURED, the player never reached the ramp window",
                    if descend { "descent" } else { "climb" }
                );
                continue;
            }
            let mean = |values: &[f32]| values.iter().sum::<f32>() / values.len() as f32;
            println!(
                "{} {label}: wish {speed:.0} u/s, on the ramp: horizontal {:.0}, vertical {:.0}, along the surface {:.0} u/s; {grounded} grounded and {airborne} airborne ticks",
                if descend { "descent" } else { "climb" },
                mean(&horizontal),
                mean(&vertical),
                mean(&surface),
            );
        }
    }
}

/// One fixed tick on a real flight: the body after the tick, the controller state after the tick,
/// and whether Rapier found support at the end of this same move. `grounded` is the state flag: it
/// is also false after a step-up lift and carries the support result of the tick before.
#[derive(Clone, Copy, Debug)]
struct WalkTick {
    position: Vec3,
    grounded: bool,
    /// Rapier's grounded result of this tick's move, read after the body was written back.
    supported: bool,
    vertical_velocity: f32,
}

#[derive(Default, Resource)]
struct WalkTickLog(Vec<WalkTick>);

fn log_walk_tick(
    mut log: ResMut<WalkTickLog>,
    player: Query<
        (
            &Transform,
            &WalkState,
            Option<&KinematicCharacterControllerOutput>,
        ),
        With<PlayerBody>,
    >,
) {
    if let Ok((body, state, output)) = player.single() {
        log.0.push(WalkTick {
            position: body.translation,
            grounded: state.grounded(),
            supported: output.is_some_and(|output| output.grounded),
            vertical_velocity: state.vertical_velocity(),
        });
    }
}

/// The converted collision of `WalkwayStairs15` (reference 0x342183, the stairs of Taylor's
/// 2026-11-15 trace, `evidence/user-test-20261115/stairs-location.md`): a stepped mesh with 18 to
/// 19 unit rises on 38 unit treads. The model runs along local X from -276 (foot) to +279 (top).
/// A floor is added at the foot and at the top landing. The foot floor is at `foot_y`; the model's
/// first tread is at 18. Returns the app and the top landing height.
fn walkway_stairs_app(assets: &Path, foot_y: f32) -> (App, f32) {
    let path = assets.join("meshes/architecture/farmhouse/walkway/walkwaystairs15.glb");
    let bytes = std::fs::read(&path).unwrap();
    let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let glb: serde_json::Value = serde_json::from_slice(&bytes[20..20 + length]).unwrap();
    let collision: shared::collision::CollisionAsset =
        serde_json::from_value(glb["scenes"][0]["extras"]["mudcrabCollision"].clone()).unwrap();
    assert!(collision.authored && collision.skipped.is_empty());
    let mut app = headless::fixture_app();
    // The fixture's own ground (top at Y = 0), hill, step and wall would sit under or beside the
    // stairs and mask the floors added below, so a lower foot floor would never be reached.
    let mut arena_query = app
        .world_mut()
        .query_filtered::<Entity, With<FixtureArena>>();
    let arena: Vec<Entity> = arena_query.iter(app.world()).collect();
    for entity in arena {
        app.world_mut().entity_mut(entity).despawn();
    }
    let mut top_levels: std::collections::BTreeMap<i32, f32> = Default::default();
    for shape in &collision.shapes {
        let shared::collision::CollisionShape::Mesh {
            vertices,
            triangles,
        } = shape
        else {
            panic!("unexpected stairs shape")
        };
        let points: Vec<Vec3> = vertices.iter().copied().map(Vec3::from_array).collect();
        for triangle in triangles {
            let [a, b, c] = triangle.map(|index| points[index as usize]);
            let normal = (b - a).cross(c - a);
            if normal.y > 0.0 && (normal.normalize().y > 0.99) && a.y > 200.0 {
                *top_levels.entry((a.y * 2.0).round() as i32).or_default() += normal.length();
            }
        }
        app.world_mut().spawn((
            RigidBody::Fixed,
            Collider::trimesh(points, triangles.clone()).unwrap(),
            world_collision_groups(),
            Transform::IDENTITY,
        ));
    }
    let top_y = top_levels
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(level, _)| *level as f32 / 2.0)
        .expect("a top landing");
    fixed_box(
        &mut app,
        Vec3::new(-276.0 - 600.0, foot_y - 10.0, 0.0),
        Vec3::new(600.0, 10.0, 600.0),
    );
    fixed_box(
        &mut app,
        Vec3::new(279.0 + 600.0, top_y - 10.0, 0.0),
        Vec3::new(600.0, 10.0, 600.0),
    );
    app.init_resource::<WalkTickLog>()
        .add_systems(FixedUpdate, log_walk_tick.after(walk_view_end_tick));
    (app, top_y)
}

/// Walks the real stairs up or down with the given frame times. Returns the rendered frames and
/// the ticks that ran on the flight.
fn walk_real_stairs(
    assets: &Path,
    foot_y: f32,
    speed: f32,
    deltas: &[f32],
    descend: bool,
    ground_snap: f32,
) -> (Vec<ClimbFrame>, Vec<WalkTick>) {
    let (mut app, top_y) = walkway_stairs_app(assets, foot_y);
    let start = if descend {
        Vec3::new(279.0 + 200.0, top_y + 60.0, 0.0)
    } else {
        Vec3::new(-276.0 - 200.0, foot_y + 60.0, 0.0)
    };
    settle(&mut app, start);
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
    // The snap length is the only thing a what-if run changes (test only, not the engine).
    app.world_mut()
        .get_mut::<KinematicCharacterController>(body)
        .unwrap()
        .snap_to_ground = (ground_snap > 0.0).then_some(CharacterLength::Absolute(ground_snap));
    app.insert_resource(WalkIntent {
        wish_dir: if descend { -Vec3::X } else { Vec3::X },
        target_speed: speed,
        jump_pressed: false,
    });
    app.world_mut().resource_mut::<WalkTickLog>().0.clear();
    let mut frames = Vec::new();
    let mut reached = false;
    for &delta in deltas {
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs_f32(delta),
        ));
        app.update();
        let world = app.world();
        let position = world.get::<Transform>(body).unwrap().translation;
        frames.push(ClimbFrame {
            time: world.resource::<Time<Virtual>>().elapsed_secs_f64(),
            delta: world.resource::<Time>().delta_secs(),
            body: position,
            feet: blended_feet(&app),
            camera: world.get::<Transform>(camera).unwrap().translation,
            weight: world
                .resource::<WalkCameraFollowState>()
                .spring
                .map_or(0.0, |spring| spring.weight),
        });
        if (descend && position.x < -276.0 - 100.0) || (!descend && position.x > 279.0 + 100.0) {
            reached = true;
            break;
        }
    }
    assert!(
        reached,
        "the walk ended before the player crossed the flight (descend={descend}, speed={speed}): \
         the frame limit is too short for these frame times, so the measures would describe an \
         unfinished flight"
    );
    let ticks = std::mem::take(&mut app.world_mut().resource_mut::<WalkTickLog>().0)
        .into_iter()
        .filter(|tick| (-276.0..279.0).contains(&tick.position.x))
        .collect();
    (frames, ticks)
}

/// Runs the controller up and down the real stairs of Taylor's trace at the walk, run and sprint
/// speeds of the traced session (80.1, 370 and 1.5 x 370 units per second) and prints what the
/// body does on each tread (grounded or falling), the fastest fall and the camera lag. Needs the
/// local asset pack: `MUDCRAB_STAIR_ASSETS=<pack> cargo test ... --ignored --nocapture`.
#[test]
#[ignore = "measure: needs the local Skyrim asset pack; set MUDCRAB_STAIR_ASSETS"]
fn measure_real_walkway_stairs15() {
    let assets = std::env::var_os("MUDCRAB_STAIR_ASSETS").expect("set MUDCRAB_STAIR_ASSETS");
    let assets = Path::new(&assets);
    let tuning = MovementTuning::default();
    let eye = tuning.eye_height;
    println!(
        "controller: autostep budget {} units, no ground snap, jump height {} units, capsule radius {}, centre {} above the ground",
        tuning.autostep_height,
        tuning.jump_height.creation(),
        tuning.capsule_radius,
        tuning.capsule_standing_height * 0.5 + PLAYER_CONTROLLER_OFFSET
    );
    for foot_y in [0.0_f32, -16.0] {
        for descend in [true, false] {
            for (label, speed) in [("walk", 80.1_f32), ("run", 370.0), ("sprint", 555.0)] {
                for (pacing, deltas) in [
                    ("steady 60 Hz", frame_times(60.0, false, 4000)),
                    ("trace pacing", trace_like_frame_times(1500)),
                ] {
                    let (frames, ticks) =
                        walk_real_stairs(assets, foot_y, speed, &deltas, descend, 0.0);
                    let airborne = ticks.iter().filter(|tick| !tick.supported).count();
                    let fall = ticks
                        .iter()
                        .map(|tick| tick.vertical_velocity)
                        .fold(0.0_f32, f32::min);
                    let rise = ticks
                        .iter()
                        .map(|tick| tick.vertical_velocity)
                        .fold(0.0_f32, f32::max);
                    let lags: Vec<f32> = frames
                        .iter()
                        .filter(|frame| (-276.0..279.0).contains(&frame.body.x))
                        .map(|frame| frame.camera.y - frame.feet.y - eye)
                        .collect();
                    let lag_min = lags.iter().copied().fold(f32::MAX, f32::min);
                    let lag_max = lags.iter().copied().fold(f32::MIN, f32::max);
                    let middle: Vec<_> = frames
                        .iter()
                        .filter(|frame| (-216.0..219.0).contains(&frame.body.x))
                        .collect();
                    let times: Vec<f64> = middle.iter().map(|frame| frame.time).collect();
                    let rms = |pick: &dyn Fn(&ClimbFrame) -> f32| {
                        let values: Vec<f64> =
                            middle.iter().map(|frame| f64::from(pick(frame))).collect();
                        detrended_rms(&times, &values)
                    };
                    println!(
                        "foot {foot_y:>5} {} {label} {speed} u/s, {pacing}: {} ticks on the flight, {airborne} unsupported ({:.0}%), fastest fall {fall:.0}, fastest rise {rise:.0} u/s; camera lag {lag_min:.1}..{lag_max:.1}; height ripple view {:.2}, rigid {:.2}",
                        if descend { "descent" } else { "climb  " },
                        ticks.len(),
                        100.0 * airborne as f64 / ticks.len().max(1) as f64,
                        rms(&|frame| frame.camera.y - eye),
                        rms(&|frame| frame.body.y),
                    );
                    if pacing == "steady 60 Hz" && label == "run" && foot_y == -16.0 {
                        println!(
                            "  per tick (x, y, grounded, vertical velocity), first 40 ticks of the flight:"
                        );
                        for tick in ticks.iter().take(40) {
                            println!(
                                "    {:7.1} {:7.2} {} {:7.1}",
                                tick.position.x,
                                tick.position.y,
                                u8::from(tick.grounded),
                                tick.vertical_velocity
                            );
                        }
                    }
                }
            }
        }
    }
}

/// What-if for the descent of the real stairs: only the ground snap length changes (12 is the
/// engine's tuning). It shows whether the snap, which is shorter than the 18 to 19 unit drop of a
/// tread, is what lets the body fall. This is not retail evidence: retail casts 70 units down and
/// pulls the player onto the surface with 4 x gravity (RF-CC-006), which a longer Rapier snap does
/// not reproduce (it moves the body down at once).
#[test]
#[ignore = "measure: needs the local Skyrim asset pack; set MUDCRAB_STAIR_ASSETS"]
fn measure_real_walkway_stairs15_descent_ground_snap_what_if() {
    let assets = std::env::var_os("MUDCRAB_STAIR_ASSETS").expect("set MUDCRAB_STAIR_ASSETS");
    let assets = Path::new(&assets);
    let eye = MovementTuning::default().eye_height;
    for (label, speed) in [("run", 370.0_f32), ("sprint", 555.0)] {
        for snap in [0.0_f32, 12.0, 24.0, 70.0] {
            for (pacing, deltas) in [
                ("steady 60 Hz", frame_times(60.0, false, 4000)),
                ("trace pacing", trace_like_frame_times(1500)),
            ] {
                let (frames, ticks) = walk_real_stairs(assets, -16.0, speed, &deltas, true, snap);
                let airborne = ticks.iter().filter(|tick| !tick.supported).count();
                let fall = ticks
                    .iter()
                    .map(|tick| tick.vertical_velocity)
                    .fold(0.0_f32, f32::min);
                let drop = ticks
                    .windows(2)
                    .map(|pair| pair[1].position.y - pair[0].position.y)
                    .fold(0.0_f32, f32::min);
                let lags: Vec<f32> = frames
                    .iter()
                    .filter(|frame| (-276.0..279.0).contains(&frame.body.x))
                    .map(|frame| frame.camera.y - frame.feet.y - eye)
                    .collect();
                let middle: Vec<_> = frames
                    .iter()
                    .filter(|frame| (-216.0..219.0).contains(&frame.body.x))
                    .collect();
                let times: Vec<f64> = middle.iter().map(|frame| frame.time).collect();
                let rms = |pick: &dyn Fn(&ClimbFrame) -> f32| {
                    let values: Vec<f64> =
                        middle.iter().map(|frame| f64::from(pick(frame))).collect();
                    detrended_rms(&times, &values)
                };
                println!(
                    "descent {label} {speed} u/s, snap {snap:>4}, {pacing}: {} ticks, {airborne} unsupported, fastest fall {fall:.0} u/s, largest drop in one tick {drop:.1}; camera lag {:.1}..{:.1}; height ripple view {:.2}, rigid {:.2}",
                    ticks.len(),
                    lags.iter().copied().fold(f32::MAX, f32::min),
                    lags.iter().copied().fold(f32::MIN, f32::max),
                    rms(&|frame| frame.camera.y - eye),
                    rms(&|frame| frame.body.y),
                );
            }
        }
    }
}

/// Reports the spring weight during a stair climb and a descent at the walk, run and sprint
/// speeds. RF-CAM-003 fades the spring out while the smoothed point moves faster than 500 units
/// per second in 3D, so a fast flight can turn the spring off. Prints only; the weight is retail
/// behaviour.
#[test]
#[ignore = "measure: prints the spring weight per speed and direction; run with --ignored --nocapture"]
fn measure_stair_spring_weight_per_speed() {
    let tuning = MovementTuning::default();
    let eye = Vec3::Y * tuning.eye_height;
    for descend in [false, true] {
        for (label, speed) in [
            ("walk", tuning.walk_speed),
            ("run", tuning.run_speed),
            ("sprint", tuning.run_speed * tuning.sprint_multiplier),
        ] {
            for (pacing, deltas) in [
                ("uneven 120 Hz", frame_times(120.0, true, 2400)),
                ("trace pacing", trace_like_frame_times(600)),
            ] {
                let (frames, _) = traverse_flight(speed, &deltas, descend);
                let middle: Vec<_> = frames
                    .iter()
                    .filter(|frame| (4.0 * STAIR_TREAD..28.0 * STAIR_TREAD).contains(&frame.body.x))
                    .collect();
                let weight = middle.iter().map(|frame| frame.weight).fold(1.0, f32::min);
                let faded = middle.iter().filter(|frame| frame.weight < 1.0).count();
                println!(
                    "{} {label} {speed} u/s, {pacing}: lowest weight {weight:.3} ({faded} of {} frames below 1), view {:?}, rigid {:?}",
                    if descend { "descent" } else { "climb" },
                    middle.len(),
                    ripple(&frames, |frame| frame.camera - eye),
                    ripple(&frames, |frame| frame.body)
                );
            }
        }
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
