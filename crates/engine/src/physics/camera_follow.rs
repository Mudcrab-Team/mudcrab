//! Retail first-person camera follow spring.
//!
//! Skyrim's first-person camera does not sit rigidly on the camera node. It keeps a smoothed
//! "tracked point" `S` with a velocity `V` and a weight `W`, and offsets the camera by a lag `e`.
//! Once per rendered frame, with `T` the camera clock and `P` the feet position:
//!
//! ```text
//! d = S_prev - P_now
//! e = W * (d * K^T + c * T * V_prev)        per axis
//! V = (e - d) / T
//! S = P_now + e
//! camera = camera_node + e
//! ```
//!
//! Source: `SkyrimSE.exe` 1.7.104.0 at `0x1408f47a0`; registry entries RF-CAM-001 to RF-CAM-007
//! in the retail-facts registry. The spring has no slope threshold, no slack, no per-tick clamp
//! and no vertical-speed gate (RF-CAM-005). Third person has no vertical spring (RF-CAM-006).
//!
//! The function in this module is pure. The caller owns the state and feeds it the feet position
//! of the rendered frame, so view interpolation between physics ticks can feed it later.
//!
//! Not implemented (RF-CAM-003 marks these flags unresolved): the state byte at `+0x88`, the
//! reference handle at player `+0x1F0` and the `0x1401573e0(player+0x70, 0xA9)` check. Also not
//! implemented: the optional N-frame average of the camera clock (RF-CAM-007).

use bevy::prelude::*;

/// RF-CAM-001: vertical spring constant K. The fraction of the lag that remains after 1 s.
pub const VERTICAL_SPRING_CONSTANT: f32 = 0.001; // RF-CAM-001: vertical K = 0.001
/// RF-CAM-001: vertical velocity damping c. This is the momentum factor on `T * V`.
pub const VERTICAL_VELOCITY_DAMPING: f32 = 0.6; // RF-CAM-001: vertical c = 0.6
/// RF-CAM-001: horizontal spring constant K.
pub const HORIZONTAL_SPRING_CONSTANT: f32 = 0.0001; // RF-CAM-001: horizontal K = 0.0001
/// RF-CAM-001: horizontal velocity damping c.
pub const HORIZONTAL_VELOCITY_DAMPING: f32 = 0.4; // RF-CAM-001: horizontal c = 0.4
/// RF-CAM-002: when `|S_prev - P_now|` is above this, in units, the lag and velocity reset to 0.
pub const SNAP_DISTANCE: f32 = 300.0; // RF-CAM-002: discontinuity limit 300 units
/// RF-CAM-003: the weight W moves by this much per second, inside [0, 1].
pub const WEIGHT_RATE_PER_SECOND: f32 = 1.5; // RF-CAM-003: W ramps 1.5/s
/// RF-CAM-003: W fades out while the smoothed point moves faster than this, in units per second.
pub const FADE_OUT_SPEED: f32 = 500.0; // RF-CAM-003: |V| > 500 u/s fades W out
/// RF-CAM-007: the camera clock T is clamped to this many seconds (166.67 ms).
pub const CAMERA_CLOCK_MAX_SECONDS: f32 = 0.1667; // RF-CAM-007: T clamp 166.67 ms = 0.1667 s

/// Which camera follows the actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowView {
    /// The first-person camera uses the spring.
    FirstPerson,
    /// RF-CAM-006: the third-person camera has no vertical spring. It follows its node directly.
    ThirdPerson,
}

/// State carried from one rendered frame to the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FollowSpring {
    /// `S`: the smoothed tracked point, in the same space as the feet position.
    pub smoothed: Vec3,
    /// `V`: the velocity of the smoothed point, in units per second.
    pub velocity: Vec3,
    /// `W`: the spring weight in [0, 1].
    pub weight: f32,
}

impl FollowSpring {
    /// A spring at rest on `feet` with the given weight. It produces no lag on its first frame.
    pub fn at_rest(feet: Vec3, weight: f32) -> Self {
        Self {
            smoothed: feet,
            velocity: Vec3::ZERO,
            weight: weight.clamp(0.0, 1.0),
        }
    }
}

/// Inputs of one rendered frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FollowInput {
    /// `P_now`: the feet position of this frame.
    pub feet: Vec3,
    /// The frame delta in seconds, before the camera clock clamp.
    pub delta_secs: f32,
    /// RF-CAM-004: whether the movement controller is controls-driven. The spring fades out when
    /// it is not.
    pub controls_driven: bool,
    pub view: FollowView,
}

/// Result of one rendered frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FollowStep {
    /// The state to carry to the next frame.
    pub state: FollowSpring,
    /// `e`: add this to the camera node position.
    pub lag: Vec3,
}

/// RF-CAM-007: the camera clock. The frame delta in seconds, clamped to 0.1667 s.
/// A negative or non-finite delta counts as 0.
pub fn camera_clock_seconds(delta_secs: f32) -> f32 {
    if delta_secs.is_finite() {
        delta_secs.clamp(0.0, CAMERA_CLOCK_MAX_SECONDS)
    } else {
        0.0
    }
}

/// Evaluate the retail follow spring for one rendered frame (RF-CAM-001 to RF-CAM-007).
/// The vertical axis is Y. The horizontal axes are X and Z.
pub fn follow_spring_step(prev: FollowSpring, input: FollowInput) -> FollowStep {
    let t = camera_clock_seconds(input.delta_secs);
    if input.view == FollowView::ThirdPerson {
        return FollowStep {
            state: FollowSpring::at_rest(input.feet, 0.0),
            lag: Vec3::ZERO,
        };
    }

    // RF-CAM-003: the weight fades out above the speed limit or when not controls-driven.
    // The order of the weight update and the lag evaluation is an assumption (unresolved).
    let fades_out = !input.controls_driven || prev.velocity.length() > FADE_OUT_SPEED;
    let weight_step = WEIGHT_RATE_PER_SECOND * t;
    let weight = if fades_out {
        prev.weight - weight_step
    } else {
        prev.weight + weight_step
    }
    .clamp(0.0, 1.0);

    let d = prev.smoothed - input.feet;
    if d.length() > SNAP_DISTANCE {
        // RF-CAM-002: no lag and no velocity on this frame.
        return FollowStep {
            state: FollowSpring::at_rest(input.feet, weight),
            lag: Vec3::ZERO,
        };
    }

    let vertical_decay = VERTICAL_SPRING_CONSTANT.powf(t);
    let horizontal_decay = HORIZONTAL_SPRING_CONSTANT.powf(t);
    let lag = Vec3::new(
        weight * (d.x * horizontal_decay + HORIZONTAL_VELOCITY_DAMPING * t * prev.velocity.x),
        weight * (d.y * vertical_decay + VERTICAL_VELOCITY_DAMPING * t * prev.velocity.y),
        weight * (d.z * horizontal_decay + HORIZONTAL_VELOCITY_DAMPING * t * prev.velocity.z),
    );
    let velocity = if t > 0.0 { (lag - d) / t } else { Vec3::ZERO };
    FollowStep {
        state: FollowSpring {
            smoothed: input.feet + lag,
            velocity,
            weight,
        },
        lag,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: f32 = 1.0;

    fn input(feet: Vec3, delta_secs: f32) -> FollowInput {
        FollowInput {
            feet,
            delta_secs,
            controls_driven: true,
            view: FollowView::FirstPerson,
        }
    }

    /// A step of `rise` units at `fps` with W = 1. Returns the lag of each frame.
    fn step_response(fps: f32, rise: f32, frames: usize) -> Vec<f32> {
        let dt = 1.0 / fps;
        let feet = Vec3::Y * rise;
        let mut state = FollowSpring::at_rest(Vec3::ZERO, FULL);
        let mut lags = Vec::new();
        for _ in 0..frames {
            let step = follow_spring_step(state, input(feet, dt));
            state = step.state;
            lags.push(step.lag.y);
        }
        lags
    }

    /// The REA table counts the settle time as the index of the first frame whose camera is
    /// within 5 percent of the target, divided by the frame rate (the first frame is index 0).
    fn settle_seconds(fps: f32, rise: f32) -> f32 {
        let lags = step_response(fps, rise, 600);
        let index = lags
            .iter()
            .position(|lag| *lag > -0.05 * rise)
            .expect("the spring settles");
        index as f32 / fps
    }

    fn overshoot(fps: f32, rise: f32) -> f32 {
        step_response(fps, rise, 600)
            .into_iter()
            .fold(0.0_f32, f32::max)
    }

    #[test]
    fn rea_step_response_first_frame_rise() {
        // REA table, 24-unit rise, W = 1: camera rise in the first frame.
        for (fps, expected) in [(30.0, 4.9), (60.0, 2.6), (144.0, 1.1)] {
            let first_frame_rise = 24.0 + step_response(fps, 24.0, 1)[0];
            assert!(
                (first_frame_rise - expected).abs() < 0.05,
                "{fps} fps: first frame rise {first_frame_rise}, REA {expected}"
            );
        }
    }

    #[test]
    fn rea_step_response_settles_to_95_percent() {
        for (fps, expected) in [(30.0, 0.100), (60.0, 0.117), (144.0, 0.139)] {
            let seconds = settle_seconds(fps, 24.0);
            assert!(
                (seconds - expected).abs() < 0.0015,
                "{fps} fps: settled at {seconds} s, REA {expected} s"
            );
        }
    }

    #[test]
    fn rea_step_response_overshoot() {
        for (fps, expected) in [(30.0, 4.1), (60.0, 1.3), (144.0, 0.0)] {
            let units = overshoot(fps, 24.0);
            assert!(
                (units - expected).abs() < 0.05,
                "{fps} fps: overshoot {units}, REA {expected}"
            );
        }
    }

    #[test]
    fn the_lag_starts_at_the_formula_value() {
        // e = W * d * K^T with V = 0 on the first frame.
        let dt = 1.0 / 60.0;
        let step = follow_spring_step(
            FollowSpring::at_rest(Vec3::ZERO, FULL),
            input(Vec3::Y * 24.0, dt),
        );
        let expected = -24.0 * VERTICAL_SPRING_CONSTANT.powf(dt);
        assert!((step.lag.y - expected).abs() < 1.0e-5);
        // V = (e - d) / T and S = P + e.
        assert!((step.state.velocity.y - (expected + 24.0) / dt).abs() < 1.0e-3);
        assert!((step.state.smoothed.y - (24.0 + expected)).abs() < 1.0e-5);
    }

    #[test]
    fn horizontal_axes_use_the_horizontal_constants() {
        let dt = 1.0 / 60.0;
        let step = follow_spring_step(
            FollowSpring::at_rest(Vec3::ZERO, FULL),
            input(Vec3::new(10.0, 0.0, -20.0), dt),
        );
        let decay = HORIZONTAL_SPRING_CONSTANT.powf(dt);
        assert!((step.lag.x - (-10.0 * decay)).abs() < 1.0e-5);
        assert!((step.lag.z - (20.0 * decay)).abs() < 1.0e-5);
        assert_eq!(step.lag.y, 0.0);
        // With a velocity, the horizontal damping 0.4 applies, not the vertical 0.6.
        let moving = FollowSpring {
            smoothed: Vec3::ZERO,
            velocity: Vec3::new(100.0, 100.0, 100.0),
            weight: FULL,
        };
        let step = follow_spring_step(moving, input(Vec3::ZERO, dt));
        assert!((step.lag.x - 0.4 * dt * 100.0).abs() < 1.0e-4);
        assert!((step.lag.y - 0.6 * dt * 100.0).abs() < 1.0e-4);
        assert!((step.lag.z - 0.4 * dt * 100.0).abs() < 1.0e-4);
        // The pure-spring time constants are 0.145 s and 0.109 s (RF-CAM-001).
        assert!((-1.0 / VERTICAL_SPRING_CONSTANT.ln() - 0.145).abs() < 0.001);
        assert!((-1.0 / HORIZONTAL_SPRING_CONSTANT.ln() - 0.109).abs() < 0.001);
    }

    #[test]
    fn a_jump_over_300_units_snaps_with_no_lag() {
        let state = FollowSpring {
            smoothed: Vec3::ZERO,
            velocity: Vec3::new(10.0, 20.0, 30.0),
            weight: FULL,
        };
        let dt = 1.0 / 60.0;
        let just_over = follow_spring_step(state, input(Vec3::new(0.0, 300.01, 0.0), dt));
        assert_eq!(just_over.lag, Vec3::ZERO);
        assert_eq!(just_over.state.velocity, Vec3::ZERO);
        assert_eq!(just_over.state.smoothed, Vec3::new(0.0, 300.01, 0.0));
        // The limit is a 3D length. A diagonal offset of about 303 units snaps too.
        let diagonal = follow_spring_step(state, input(Vec3::new(175.0, 175.0, 175.0), dt));
        assert_eq!(diagonal.lag, Vec3::ZERO);
        // At exactly 300 units the spring still runs.
        let at_limit = follow_spring_step(state, input(Vec3::new(0.0, 300.0, 0.0), dt));
        assert!(at_limit.lag.y < 0.0);
    }

    #[test]
    fn the_weight_ramps_at_1_5_per_second_and_clamps() {
        let dt = 1.0 / 60.0;
        let feet = Vec3::ZERO;
        let mut state = FollowSpring::at_rest(feet, 0.0);
        for _ in 0..30 {
            state = follow_spring_step(state, input(feet, dt)).state;
        }
        assert!((state.weight - 0.75).abs() < 1.0e-4, "{}", state.weight);
        for _ in 0..60 {
            state = follow_spring_step(state, input(feet, dt)).state;
        }
        assert_eq!(state.weight, 1.0);
        let not_driven = FollowInput {
            controls_driven: false,
            ..input(feet, dt)
        };
        for _ in 0..30 {
            state = follow_spring_step(state, not_driven).state;
        }
        assert!((state.weight - 0.25).abs() < 1.0e-4, "{}", state.weight);
        for _ in 0..60 {
            state = follow_spring_step(state, not_driven).state;
        }
        assert_eq!(state.weight, 0.0);
        // A weight of 0 gives no lag. The weight stays 0 while the spring is faded out.
        let step = follow_spring_step(
            state,
            FollowInput {
                feet: Vec3::Y * 24.0,
                ..not_driven
            },
        );
        assert_eq!(step.lag, Vec3::ZERO);
    }

    #[test]
    fn the_weight_fades_out_above_500_units_per_second() {
        let dt = 1.0 / 60.0;
        let mut state = FollowSpring {
            smoothed: Vec3::ZERO,
            velocity: Vec3::new(0.0, 500.5, 0.0),
            weight: FULL,
        };
        let step = follow_spring_step(state, input(Vec3::ZERO, dt));
        assert!((step.state.weight - (1.0 - 1.5 * dt)).abs() < 1.0e-5);
        // The speed is 3D, not vertical only: a horizontal speed fades the weight too.
        state.velocity = Vec3::new(400.0, 0.0, 400.0);
        let step = follow_spring_step(state, input(Vec3::ZERO, dt));
        assert!(step.state.weight < 1.0);
        // At exactly 500 u/s the weight holds at 1.
        state.velocity = Vec3::new(0.0, 500.0, 0.0);
        let step = follow_spring_step(state, input(Vec3::ZERO, dt));
        assert_eq!(step.state.weight, 1.0);
    }

    #[test]
    fn there_is_no_slope_threshold_slack_or_vertical_speed_gate() {
        // RF-CAM-005: a 1-unit rise lags like a 24-unit rise, in proportion. No slack hides it.
        let one = step_response(60.0, 1.0, 1)[0];
        let twenty_four = step_response(60.0, 24.0, 1)[0];
        assert!(one < 0.0);
        assert!((twenty_four / one - 24.0).abs() < 1.0e-3);
        // A fast vertical rise below the 500 u/s gate is still smoothed.
        assert!(step_response(60.0, 6.0, 1)[0] < -5.0);
    }

    #[test]
    fn third_person_has_no_vertical_spring() {
        let state = FollowSpring {
            smoothed: Vec3::ZERO,
            velocity: Vec3::new(0.0, 100.0, 0.0),
            weight: FULL,
        };
        let step = follow_spring_step(
            state,
            FollowInput {
                view: FollowView::ThirdPerson,
                ..input(Vec3::Y * 24.0, 1.0 / 60.0)
            },
        );
        assert_eq!(step.lag, Vec3::ZERO);
        assert_eq!(step.state.smoothed, Vec3::Y * 24.0);
        assert_eq!(step.state.velocity, Vec3::ZERO);
    }

    #[test]
    fn the_camera_clock_clamps_at_0_1667_seconds() {
        assert_eq!(camera_clock_seconds(1.0 / 60.0), 1.0 / 60.0);
        assert_eq!(camera_clock_seconds(0.1667), 0.1667);
        assert_eq!(camera_clock_seconds(0.5), 0.1667);
        assert_eq!(camera_clock_seconds(-1.0), 0.0);
        assert_eq!(camera_clock_seconds(f32::NAN), 0.0);
        // A long frame evaluates like a 0.1667 s frame.
        let state = FollowSpring::at_rest(Vec3::ZERO, FULL);
        let long = follow_spring_step(state, input(Vec3::Y * 24.0, 2.0));
        let clamped = follow_spring_step(state, input(Vec3::Y * 24.0, 0.1667));
        assert_eq!(long, clamped);
    }

    #[test]
    fn a_zero_delta_keeps_the_formula_and_zeroes_the_velocity() {
        let state = FollowSpring {
            smoothed: Vec3::ZERO,
            velocity: Vec3::new(0.0, 50.0, 0.0),
            weight: FULL,
        };
        let step = follow_spring_step(state, input(Vec3::Y * 4.0, 0.0));
        // K^0 = 1, so e = W * d.
        assert!((step.lag.y - (-4.0)).abs() < 1.0e-6);
        assert_eq!(step.state.velocity, Vec3::ZERO);
        assert_eq!(step.state.weight, 1.0);
    }
}
