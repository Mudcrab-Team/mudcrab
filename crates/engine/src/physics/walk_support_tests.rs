//! Headless tests of the retail walking support rule (REA CC-1, RF-CC-013 to RF-CC-017) on the
//! step profile of `WalkwayStairs15`, the stairs of Taylor's 2026-11-15 trace: 15 treads of 18.3
//! units rise on 36.6 units run (RF-CC-021, about 26.6 degrees), plus ledges, a ramp and a jump.

use super::{headless, walk_support::*, *};

const RISE: f32 = 18.3;
const RUN: f32 = 36.6;
const TREADS: usize = 15;
const Z: f32 = 850.0;
/// The speeds of Taylor's traced session: walk, run, and sprint at 1.5 x run.
const SPEEDS: [(&str, f32); 3] = [("walk", 80.1), ("run", 370.0), ("sprint", 555.0)];

fn fixed_box(app: &mut App, centre: Vec3, half: Vec3, rotation: Quat) {
    app.world_mut().spawn((
        RigidBody::Fixed,
        Collider::cuboid(half.x, half.y, half.z),
        world_collision_groups(),
        Transform::from_translation(centre).with_rotation(rotation),
    ));
}

/// The stepped flight rising toward +X from x = 0, with a landing at the top.
fn flight(app: &mut App) -> f32 {
    for index in 0..TREADS {
        let top = (index + 1) as f32 * RISE;
        fixed_box(
            app,
            Vec3::new((index as f32 + 0.5) * RUN, top * 0.5, Z),
            Vec3::new(RUN * 0.5, top * 0.5, 600.0),
            Quat::IDENTITY,
        );
    }
    let top = TREADS as f32 * RISE;
    fixed_box(
        app,
        Vec3::new(TREADS as f32 * RUN + 400.0, top * 0.5, Z),
        Vec3::new(400.0, top * 0.5, 600.0),
        Quat::IDENTITY,
    );
    top
}

/// One fixed tick: position and walking state after the tick.
#[derive(Clone, Copy, Debug)]
struct Tick {
    position: Vec3,
    state: WalkState,
}

#[derive(Default, Resource)]
struct Ticks(Vec<Tick>);

fn log_tick(mut log: ResMut<Ticks>, player: Query<(&Transform, &WalkState), With<PlayerBody>>) {
    if let Ok((body, state)) = player.single() {
        log.0.push(Tick {
            position: body.translation,
            state: *state,
        });
    }
}

fn app_with_ticks() -> App {
    let mut app = headless::fixture_app();
    app.init_resource::<Ticks>()
        .add_systems(FixedUpdate, log_tick.after(PhysicsSet::Writeback));
    app
}

fn settle(app: &mut App, position: Vec3) {
    headless::place_player(app, position);
    for _ in 0..150 {
        app.update();
    }
    let (pose, grounded) = headless::player_pose(app);
    assert!(grounded, "the player did not settle: {pose:?}");
    app.world_mut().resource_mut::<Ticks>().0.clear();
}

/// Walks along `direction` until `done`, and returns every tick.
fn walk(app: &mut App, direction: Vec3, speed: f32, done: impl Fn(Vec3) -> bool) -> Vec<Tick> {
    app.insert_resource(WalkIntent {
        wish_dir: direction,
        target_speed: speed,
        jump_pressed: false,
    });
    let mut finished = false;
    for _ in 0..1500 {
        app.update();
        if done(headless::player_pose(app).0) {
            finished = true;
            break;
        }
    }
    assert!(
        finished,
        "the walk never reached its end in 1500 ticks, the player is stuck at {:?}",
        headless::player_pose(app).0
    );
    std::mem::take(&mut app.world_mut().resource_mut::<Ticks>().0)
}

#[test]
fn descending_the_stairs_never_leaves_the_walking_state() {
    for (name, speed) in SPEEDS {
        let mut app = app_with_ticks();
        let top = flight(&mut app);
        settle(
            &mut app,
            Vec3::new(TREADS as f32 * RUN + 200.0, top + 60.0, Z),
        );
        let ticks = walk(&mut app, -Vec3::X, speed, |pose| pose.x < -150.0);
        let on_flight: Vec<_> = ticks
            .iter()
            .filter(|tick| (0.0..TREADS as f32 * RUN).contains(&tick.position.x))
            .collect();
        assert!(
            on_flight.len() >= 5,
            "{name}: the descent covered {} ticks of the flight",
            on_flight.len()
        );
        assert!(
            on_flight.iter().all(|tick| !tick.state.airborne),
            "{name}: the body entered the air state on a stair tread"
        );
        assert!(
            ticks.iter().all(|tick| !tick.state.landed),
            "{name}: a landing event fired on the stairs"
        );
        let end = ticks.last().unwrap().position;
        assert!(
            end.x < -150.0 && end.y < 60.0,
            "{name}: the descent stalled at {end:?}"
        );
    }
}

#[test]
fn climbing_the_stairs_still_works() {
    for (name, speed) in SPEEDS {
        let mut app = app_with_ticks();
        let top = flight(&mut app);
        settle(&mut app, Vec3::new(-200.0, 60.0, Z));
        let ticks = walk(&mut app, Vec3::X, speed, |pose| {
            pose.x > TREADS as f32 * RUN + 100.0
        });
        let end = ticks.last().unwrap().position;
        assert!(
            end.x > TREADS as f32 * RUN + 100.0 && end.y > top + 40.0,
            "{name}: the climb stalled at {end:?}, top at {top}"
        );
        assert!(
            ticks.iter().all(|tick| !tick.state.airborne),
            "{name}: the body entered the air state on the way up"
        );
    }
}

/// A ramp rising toward +X at `degrees`, with flat ground at both ends. Returns its top.
fn ramp(app: &mut App, degrees: f32) -> Vec3 {
    let slope = degrees.to_radians();
    let (length, half_thickness) = (2400.0_f32, 10.0_f32);
    let up = Vec3::new(-slope.sin(), slope.cos(), 0.0);
    let centre = Vec3::new(slope.cos(), slope.sin(), 0.0) * (length * 0.5) - up * half_thickness;
    fixed_box(
        app,
        centre + Vec3::Z * Z,
        Vec3::new(length * 0.5, half_thickness, 600.0),
        Quat::from_rotation_z(slope),
    );
    let top = Vec3::new(length * slope.cos(), length * slope.sin(), 0.0);
    fixed_box(
        app,
        top + Vec3::new(400.0, -half_thickness, Z),
        Vec3::new(400.0, half_thickness, 600.0),
        Quat::IDENTITY,
    );
    top
}

/// The mean horizontal speed on the middle of the ramp.
fn ramp_speed(degrees: f32, wish: f32, uphill: bool) -> f32 {
    let mut app = app_with_ticks();
    let top = ramp(&mut app, degrees);
    let start = if uphill {
        Vec3::new(-150.0, 60.0, Z)
    } else {
        Vec3::new(top.x + 150.0, top.y + 60.0, Z)
    };
    settle(&mut app, start);
    let direction = if uphill { Vec3::X } else { -Vec3::X };
    let ticks = walk(&mut app, direction, wish, |pose| {
        if uphill {
            pose.x > top.x - 200.0
        } else {
            pose.x < 200.0
        }
    });
    let middle: Vec<_> = ticks
        .iter()
        .filter(|tick| (350.0..top.x - 350.0).contains(&tick.position.x))
        .collect();
    assert!(
        middle.len() > 20,
        "the ramp walk covered {} ticks",
        middle.len()
    );
    (middle[middle.len() - 1].position.x - middle[0].position.x).abs()
        / ((middle.len() - 1) as f32 * PHYSICS_TIMESTEP)
}

#[test]
fn the_horizontal_speed_on_a_slope_is_the_wish_times_cos_up_and_down() {
    let wish = MovementTuning::default().run_speed;
    // 26.6 degrees is the slope of the stairs. Steeper uphill slopes fall short by a few percent
    // more, because Rapier's slide keeps part of the horizontal motion (measured, not retail).
    for (degrees, tolerance) in [(26.6_f32, 0.06), (30.0, 0.10), (35.0, 0.10)] {
        let expected = wish * degrees.to_radians().cos();
        for uphill in [true, false] {
            let speed = ramp_speed(degrees, wish, uphill);
            assert!(
                (speed - expected).abs() <= tolerance * expected,
                "{degrees} degrees {}: horizontal speed {speed:.0}, expected W x cos = {expected:.0}",
                if uphill { "uphill" } else { "downhill" }
            );
        }
    }
}

/// Walks off a ledge of `height` units onto the ground and returns the ticks.
fn ledge(height: f32) -> Vec<Tick> {
    let mut app = app_with_ticks();
    fixed_box(
        &mut app,
        Vec3::new(-600.0, height * 0.5, Z),
        Vec3::new(600.0, height * 0.5, 600.0),
        Quat::IDENTITY,
    );
    settle(&mut app, Vec3::new(-200.0, height + 60.0, Z));
    walk(&mut app, Vec3::X, 300.0, |pose| pose.x > 300.0)
}

#[test]
fn a_step_down_within_the_cast_length_keeps_walking() {
    for height in [40.0_f32, UNSUPPORTED_CAST_LENGTH - 5.0] {
        let ticks = ledge(height);
        assert!(
            ticks
                .iter()
                .all(|tick| !tick.state.airborne && !tick.state.landed),
            "a {height} unit drop entered the air state"
        );
        let end = ticks.last().unwrap().position;
        assert!(end.y < 60.0, "the body did not reach the ground: {end:?}");
    }
}

#[test]
fn a_ledge_beyond_the_cast_length_still_falls() {
    let height = UNSUPPORTED_CAST_LENGTH + 40.0;
    let ticks = ledge(height);
    let first_air = ticks
        .iter()
        .position(|tick| tick.state.airborne)
        .expect("a drop beyond the cast length must start the air state");
    // The cast keeps hitting the ledge rim while the capsule slides off it, so the body sinks a
    // little (RF-CC-013, a hit keeps the walking state) before the cast misses and the air state
    // stores the height. It lies at or below the standing height and well above the ground.
    let standing = height + 52.4;
    let start = ticks[first_air].state.fall_start_height;
    assert!(
        start <= standing + 1.0 && start > standing - 40.0,
        "fall start height {start} for a {height} unit drop (standing height {standing})"
    );
    assert_eq!(
        ticks.iter().filter(|tick| tick.state.landed).count(),
        1,
        "exactly one landing event"
    );
    let landed = ticks.iter().position(|tick| tick.state.landed).unwrap();
    assert!(landed > first_air, "the landing follows the air state");
    assert!(!ticks[landed].state.airborne, "support ends the air state");
}

#[test]
fn a_jump_still_leaves_the_ground_and_lands_once() {
    let mut app = app_with_ticks();
    settle(&mut app, Vec3::new(0.0, 60.0, Z));
    app.insert_resource(WalkIntent {
        wish_dir: Vec3::ZERO,
        target_speed: 0.0,
        jump_pressed: true,
    });
    let mut apex = 0.0_f32;
    let ground = headless::player_pose(&mut app).0.y;
    for _ in 0..240 {
        app.update();
        apex = apex.max(headless::player_pose(&mut app).0.y - ground);
    }
    let ticks = std::mem::take(&mut app.world_mut().resource_mut::<Ticks>().0);
    // The retained 60 Hz headless run with the default 76-unit height rose 71.48 units.
    assert!((apex - 71.48).abs() < 0.5, "sampled jump apex {apex:.2}");
    assert!((ticks.last().unwrap().state.jump_apex() - apex).abs() < 0.1);
    assert!(
        ticks.iter().any(|tick| tick.state.airborne),
        "a jump is the air state"
    );
    assert_eq!(ticks.iter().filter(|tick| tick.state.landed).count(), 1);
    let last = ticks.last().unwrap();
    assert!(
        !last.state.airborne && last.state.grounded(),
        "standing again after the jump"
    );
}

/// The tick that launches a jump is the start of the air state: `airborne` is set at once and the
/// fall start height is the launch height (RF-CC-013 stores it when the air state begins). The
/// ascent must not end the air state or set the landing flag while Rapier still reports support
/// right after take-off, and the jump lands exactly once, with the launch height unchanged.
#[test]
fn the_jump_launch_tick_starts_the_air_state_at_the_launch_height() {
    for stair_top in [false, true] {
        let place = if stair_top {
            "stair top"
        } else {
            "flat ground"
        };
        let mut app = app_with_ticks();
        let start = if stair_top {
            let top = flight(&mut app);
            Vec3::new(TREADS as f32 * RUN + 200.0, top + 60.0, Z)
        } else {
            Vec3::new(0.0, 60.0, Z)
        };
        settle(&mut app, start);
        let launch_height = headless::player_pose(&mut app).0.y;
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::ZERO,
            target_speed: 0.0,
            jump_pressed: true,
        });
        app.update();
        let first = app.world().resource::<Ticks>().0[0];
        assert!(
            first.state.jump_active() && first.state.vertical_velocity() > 0.0,
            "{place}: the press did not launch: {:?}",
            first.state
        );
        assert!(
            first.state.airborne,
            "{place}: the air state did not start on the launch tick: {:?}",
            first.state
        );
        assert!(
            (first.state.fall_start_height - launch_height).abs() < 0.5,
            "{place}: fall start height {} is not the launch height {launch_height}",
            first.state.fall_start_height
        );
        assert!(
            !first.state.landed,
            "{place}: the launch tick set the landing flag"
        );
        for _ in 0..240 {
            app.update();
        }
        let ticks = std::mem::take(&mut app.world_mut().resource_mut::<Ticks>().0);
        for tick in &ticks {
            if tick.state.airborne {
                assert!(
                    (tick.state.fall_start_height - launch_height).abs() < 0.5,
                    "{place}: the fall start height changed during the jump: {:?}",
                    tick.state
                );
            }
            assert!(
                !(tick.state.landed
                    && tick.state.jump_active()
                    && tick.state.vertical_velocity() > 0.0),
                "{place}: a landing was flagged while rising: {:?}",
                tick.state
            );
        }
        assert_eq!(
            ticks.iter().filter(|tick| tick.state.landed).count(),
            1,
            "{place}: exactly one landing per jump"
        );
        let last = ticks.last().unwrap();
        assert!(!last.state.airborne, "{place}: standing again");
    }
}

fn walk_state(app: &mut App) -> WalkState {
    *app.world_mut()
        .query::<&WalkState>()
        .single(app.world())
        .unwrap()
}

/// A tick that accepted a jump launch, including a relaunch on landing.
fn launched(tick: &Tick) -> bool {
    tick.state.jump_launched()
}

/// Jumps held down between platforms of different height: every accepted launch, including the one
/// on the tick where the previous jump lands, records its own launch height (RF-CC-013).
#[test]
fn consecutive_jumps_between_platforms_record_each_launch_height() {
    let mut app = app_with_ticks();
    // A platform with its top 40 units up, from x = 100.
    fixed_box(
        &mut app,
        Vec3::new(700.0, 20.0, Z),
        Vec3::new(600.0, 20.0, 600.0),
        Quat::IDENTITY,
    );
    settle(&mut app, Vec3::new(0.0, 60.0, Z));
    let start_height = headless::player_pose(&mut app).0.y;
    app.insert_resource(WalkIntent {
        wish_dir: Vec3::X,
        target_speed: 200.0,
        jump_pressed: true,
    });
    for _ in 0..400 {
        app.world_mut().resource_mut::<WalkIntent>().jump_pressed = true;
        app.update();
    }
    let ticks = std::mem::take(&mut app.world_mut().resource_mut::<Ticks>().0);
    let mut launches = Vec::new();
    for (index, tick) in ticks.iter().enumerate() {
        if launched(tick) {
            let launch_height = index
                .checked_sub(1)
                .map_or(start_height, |before| ticks[before].position.y);
            assert!(
                (tick.state.fall_start_height - launch_height).abs() < 0.5,
                "launch {} at height {launch_height}: fall start height is {}",
                launches.len() + 1,
                tick.state.fall_start_height
            );
            launches.push(launch_height);
        }
    }
    assert!(launches.len() >= 3, "{} launches", launches.len());
    assert!(
        launches.iter().any(|height| *height > 80.0)
            && launches.iter().any(|height| *height < 60.0),
        "the launches did not cover both heights: {launches:?}"
    );
}

/// A jump pressed on the tick where support returns after a ledge fall: the landing is reported
/// and the new jump starts its own air state (RF-CC-015).
#[test]
fn a_jump_pressed_on_the_landing_tick_after_a_ledge_fall_reports_the_landing() {
    let height = UNSUPPORTED_CAST_LENGTH + 40.0;
    let mut app = app_with_ticks();
    fixed_box(
        &mut app,
        Vec3::new(-600.0, height * 0.5, Z),
        Vec3::new(600.0, height * 0.5, 600.0),
        Quat::IDENTITY,
    );
    settle(&mut app, Vec3::new(-200.0, height + 60.0, Z));
    app.insert_resource(WalkIntent {
        wish_dir: Vec3::X,
        target_speed: 300.0,
        jump_pressed: false,
    });
    let mut relaunch = None;
    for _ in 0..400 {
        // Press only while falling, so the press is waiting on the landing tick.
        let falling = walk_state(&mut app).airborne;
        app.world_mut().resource_mut::<WalkIntent>().jump_pressed = falling;
        app.update();
        let tick = *app.world().resource::<Ticks>().0.last().unwrap();
        if falling && launched(&tick) {
            relaunch = Some(tick);
            break;
        }
    }
    let tick = relaunch.expect("the press on the landing tick launched a jump");
    assert!(
        tick.state.landed,
        "the landing was not reported: {:?}",
        tick.state
    );
    assert!(
        tick.state.airborne,
        "the new jump is the air state: {:?}",
        tick.state
    );
}

/// A jump pressed while the player walks down a stair tread, between two contacts, must launch. In
/// retail a pending jump only skips the unsupported cast (RF-CC-013); the state is still walking,
/// so the jump is allowed. The press is made on a tick where Rapier reports no support and the
/// previous tick was part of a cast-hit descent, the case where a press used to be dropped.
#[test]
fn a_jump_pressed_mid_glide_down_the_stairs_still_launches() {
    for (name, speed) in SPEEDS {
        let mut app = app_with_ticks();
        let top = flight(&mut app);
        settle(
            &mut app,
            Vec3::new(TREADS as f32 * RUN + 200.0, top + 60.0, Z),
        );
        app.insert_resource(WalkIntent {
            wish_dir: -Vec3::X,
            target_speed: speed,
            jump_pressed: false,
        });
        let mut pressed = None;
        for _ in 0..600 {
            let (gliding, x) = {
                let world = app.world_mut();
                let mut query = world.query_filtered::<(
                    &Transform,
                    &WalkState,
                    Option<&KinematicCharacterControllerOutput>,
                ), With<PlayerBody>>();
                let (body, state, output) = query.single(world).unwrap();
                let unsupported = output.is_some_and(|output| !output.grounded);
                (
                    unsupported
                        && !state.airborne
                        && !state.jump_active()
                        && state.vertical_velocity() < -1.0,
                    body.translation.x,
                )
            };
            if gliding && (RUN..TREADS as f32 * RUN - RUN).contains(&x) {
                app.world_mut().resource_mut::<WalkIntent>().jump_pressed = true;
                app.update();
                let state = *app
                    .world_mut()
                    .query::<&WalkState>()
                    .single(app.world())
                    .unwrap();
                pressed = Some(state);
                break;
            }
            app.update();
        }
        let state = pressed.unwrap_or_else(|| panic!("{name}: no unsupported glide tick found"));
        assert!(
            state.jump_launched() && state.jump_active() && state.vertical_velocity() > 0.0,
            "{name}: a jump pressed mid-glide did not launch: {state:?}"
        );
    }
}

/// The same descent and climb on the converted collision of `WalkwayStairs15`, read-only from the
/// local pack: `MUDCRAB_STAIR_ASSETS=<pack> cargo test -p engine --lib --ignored real_walkway`.
#[test]
#[ignore = "requires the immutable local Skyrim asset pack; set MUDCRAB_STAIR_ASSETS"]
fn real_walkway_stairs15_descent_and_climb_stay_walking() {
    let assets = std::env::var_os("MUDCRAB_STAIR_ASSETS").expect("set MUDCRAB_STAIR_ASSETS");
    let path = Path::new(&assets).join("meshes/architecture/farmhouse/walkway/walkwaystairs15.glb");
    let bytes = std::fs::read(&path).unwrap();
    let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let glb: serde_json::Value = serde_json::from_slice(&bytes[20..20 + length]).unwrap();
    let collision: shared::collision::CollisionAsset =
        serde_json::from_value(glb["scenes"][0]["extras"]["mudcrabCollision"].clone()).unwrap();
    assert!(collision.authored && collision.skipped.is_empty());
    for descend in [true, false] {
        for (name, speed) in SPEEDS {
            let mut app = app_with_ticks();
            for shape in &collision.shapes {
                let shared::collision::CollisionShape::Mesh {
                    vertices,
                    triangles,
                } = shape
                else {
                    panic!("unexpected stairs shape")
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
            // The model runs along X from -276 (foot, first tread at 18) to 279 (top, 256).
            fixed_box(
                &mut app,
                Vec3::new(-876.0, -26.0, 0.0),
                Vec3::new(600.0, 10.0, 600.0),
                Quat::IDENTITY,
            );
            fixed_box(
                &mut app,
                Vec3::new(879.0, 246.0, 0.0),
                Vec3::new(600.0, 10.0, 600.0),
                Quat::IDENTITY,
            );
            let start = if descend {
                Vec3::new(479.0, 316.0, 0.0)
            } else {
                Vec3::new(-476.0, 40.0, 0.0)
            };
            settle(&mut app, start);
            let ticks = walk(
                &mut app,
                if descend { -Vec3::X } else { Vec3::X },
                speed,
                |pose| {
                    if descend {
                        pose.x < -376.0
                    } else {
                        pose.x > 379.0
                    }
                },
            );
            assert!(
                ticks
                    .iter()
                    .all(|tick| !tick.state.airborne && !tick.state.landed),
                "{} {name}: the body left the walking state on the real stairs",
                if descend { "descent" } else { "climb" }
            );
            let end = ticks.last().unwrap().position;
            // The same limits as the `done` test of the walk above: the player must have crossed
            // the whole flight, not only reached a height.
            assert!(
                if descend {
                    end.x < -376.0
                } else {
                    end.x > 379.0
                },
                "{} {name}: the player stopped at x = {} before the end of the flight",
                if descend { "descent" } else { "climb" },
                end.x
            );
            assert!(
                if descend { end.y < 60.0 } else { end.y > 290.0 },
                "{name}: stalled at {end:?}"
            );
        }
    }
}
