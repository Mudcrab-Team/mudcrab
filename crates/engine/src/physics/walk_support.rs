//! The retail walking support rule for the WALK controller (REA CC-1, registry RF-CC-013 to
//! RF-CC-017).
//!
//! Retail walks on a single state, OnGround, and only leaves it when nothing is under the player:
//!
//! 1. Each step where support (kSupport) is clear, no jump or other state request is pending and
//!    the unsupported cast is enabled, the whole character shape is swept straight down by
//!    `fUnsupportCastLength` (1.0 m = 69.99 units). RF-CC-013.
//! 2. A hit keeps the walking state. There is no fall state, no fall start height and no landing
//!    event. The step adds 4 x gravity to the vertical velocity as an acceleration
//!    (`fUnsupportedGravMult`), and the collision step moves the body. RF-CC-013, RF-CC-014.
//! 3. A miss starts the air state and stores the fall start height. RF-CC-013.
//! 4. The first step with support ends the air state and sets the landing flag (0x80).
//!    RF-CC-015.
//!
//! On a supported slope the wish is rotated into the support plane, so its length along the
//! surface stays W. If the result points up, the part along the support normal is removed until the
//! vertical part is zero, and the collision step slides the rest along the plane. The speed along
//! the surface is W up and down, and the horizontal speed is W x cos(slope) both ways.
//! RF-CC-016, RF-CC-017.
//!
//! Mudcrab runs the controller on a fixed 60 Hz tick, retail runs it once per rendered frame
//! (RF-PHYS-003). The rule is the same. Rapier's grounded flag stands in for kSupport.
//!
//! Not retail, kept as Mudcrab tuning: the step-up probe (`walk_step_rise`, RF-CC-019), the 28 unit
//! capsule radius (retail: about 18 units, RF-CC-020) and the air gravity of 900 units per second
//! squared (retail: 686.6, RF-CC-002). Not read in the binary, so not guessed: whether the vertical
//! velocity persists from one step to the next (the rule above assumes it does, RF-CC-014), the
//! Havok proxy's support tolerance and keep distance (RF-CC-001), and the normal threshold at
//! `0x141829400` (RF-CC-017).

use super::{MovementTuning, PLAYER_CONTROLLER_OFFSET};
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

/// RF-UNIT-001: one Havok metre in game units.
pub const HAVOK_METRE_UNITS: f32 = 69.991_25;
/// RF-CC-013: `fUnsupportCastLength:HAVOK`, 1.0 m (record `0x1420d2598`, value `0x1420d25a0`).
pub const UNSUPPORTED_CAST_LENGTH_METRES: f32 = 1.0;
/// RF-CC-013: the cast length in game units, 69.99.
pub const UNSUPPORTED_CAST_LENGTH: f32 = UNSUPPORTED_CAST_LENGTH_METRES * HAVOK_METRE_UNITS;
/// RF-CC-013, RF-CC-014: `fUnsupportedGravMult:HAVOK`, 4.0 (`0x1420d2588`).
pub const UNSUPPORTED_GRAVITY_MULTIPLIER: f32 = 4.0;
/// RF-CC-002: world gravity 9.81 m/s^2 in game units per second squared, 686.6.
pub const RETAIL_GRAVITY: f32 = 9.81 * HAVOK_METRE_UNITS;
/// RF-CC-017: the vertical part of the wish is flattened only above this (`0x141b5bdc4`).
const UPWARD_VELOCITY_EPSILON: f32 = 0.001;

/// The filter of the controller, without the player's own body.
fn controller_filter<'a>(
    controller: &KinematicCharacterController,
    entity: Entity,
) -> QueryFilter<'a> {
    QueryFilter {
        flags: controller.filter_flags,
        groups: controller.filter_groups,
        ..QueryFilter::default().exclude_rigid_body(entity)
    }
}

/// RF-CC-013: sweeps the whole capsule straight down by 69.99 units. True on a hit.
pub(super) fn unsupported_cast_hits(
    context: &RapierContext,
    entity: Entity,
    body: &Transform,
    capsule: &Collider,
    controller: &KinematicCharacterController,
) -> bool {
    context
        .cast_shape(
            body.translation,
            body.rotation,
            Vec3::NEG_Y,
            &*capsule.raw,
            ShapeCastOptions {
                target_distance: PLAYER_CONTROLLER_OFFSET,
                stop_at_penetration: false,
                max_time_of_impact: UNSUPPORTED_CAST_LENGTH,
                compute_impact_geometry_on_penetration: true,
            },
            controller_filter(controller, entity),
        )
        .is_some()
}

/// RF-CC-014: the vertical velocity after one step of an unsupported cast hit. The 4 x gravity is
/// an acceleration that adds to the velocity of the previous step, which is 0 on the first step.
pub(super) fn glide_vertical_velocity(previous: f32, dt: f32) -> f32 {
    previous.min(0.0) - UNSUPPORTED_GRAVITY_MULTIPLIER * RETAIL_GRAVITY * dt
}

/// The face normal of the surface under the player, from a ray through the capsule centre. A ray
/// finds the face and not the rim of a tread edge, which a swept capsule reports as a slanted
/// contact. The ray is long enough to reach a slope at the controller's climb limit.
pub(super) fn support_normal(
    context: &RapierContext,
    entity: Entity,
    body: &Transform,
    controller: &KinematicCharacterController,
    tuning: &MovementTuning,
) -> Option<Vec3> {
    let reach = tuning.capsule_half_height()
        + tuning.capsule_radius / controller.max_slope_climb_angle.cos().max(0.1)
        + 2.0 * PLAYER_CONTROLLER_OFFSET;
    let (_, hit) = context.cast_ray_and_get_normal(
        body.translation,
        Vec3::NEG_Y,
        reach,
        true,
        controller_filter(controller, entity),
    )?;
    (hit.normal.is_finite() && hit.normal.y > UPWARD_VELOCITY_EPSILON)
        .then(|| hit.normal.normalize())
}

/// RF-CC-016, RF-CC-017: the velocity handed to the collision step on a supported surface.
///
/// `wish` is the horizontal wish velocity of length W. It is rotated into the plane of
/// `support_normal`, so its length along the surface stays W. Going up, the part along the normal
/// is removed until the vertical part is 0, which makes the translation horizontal and longer than
/// W. The collision step slides it along the plane, which leaves W along the surface. Going down,
/// the rotated velocity points down the plane and is used as is.
pub(super) fn slope_velocity(wish: Vec3, support_normal: Vec3) -> Vec3 {
    if wish.length_squared() <= f32::EPSILON || support_normal.y >= 1.0 - 1.0e-6 {
        return wish;
    }
    let mut velocity = Quat::from_rotation_arc(Vec3::Y, support_normal) * wish;
    if velocity.y > UPWARD_VELOCITY_EPSILON && support_normal.y.abs() > UPWARD_VELOCITY_EPSILON {
        velocity -= support_normal * (velocity.y / support_normal.y);
    }
    velocity
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal_of(slope_degrees: f32, uphill: Vec3) -> Vec3 {
        // A plane that rises toward `uphill`.
        let slope = slope_degrees.to_radians();
        (Vec3::Y * slope.cos() - uphill * slope.sin()).normalize()
    }

    /// The part of the translation that survives the collision step: the part along the plane.
    fn along_the_plane(velocity: Vec3, normal: Vec3) -> Vec3 {
        velocity - normal * velocity.dot(normal)
    }

    #[test]
    fn the_constants_are_the_registry_values() {
        assert!((UNSUPPORTED_CAST_LENGTH - 69.99).abs() < 0.01);
        assert_eq!(UNSUPPORTED_GRAVITY_MULTIPLIER, 4.0);
        assert!((RETAIL_GRAVITY - 686.6).abs() < 0.1);
        assert!(
            (UNSUPPORTED_GRAVITY_MULTIPLIER * RETAIL_GRAVITY - 2746.0).abs() < 1.0,
            "4 x 686.6 is 2746 units per second squared"
        );
    }

    #[test]
    fn a_cast_hit_adds_four_times_gravity_each_step() {
        let dt = 1.0 / 60.0;
        let first = glide_vertical_velocity(0.0, dt);
        assert!((first + 45.8).abs() < 0.1, "45.8 units per second per tick");
        let second = glide_vertical_velocity(first, dt);
        assert!(
            (second - 2.0 * first).abs() < 1.0e-3,
            "the velocity accumulates"
        );
        assert_eq!(
            glide_vertical_velocity(5.0, dt),
            first,
            "no upward carry-over"
        );
    }

    #[test]
    fn flat_ground_keeps_the_wish() {
        let wish = Vec3::new(300.0, 0.0, 0.0);
        assert_eq!(slope_velocity(wish, Vec3::Y), wish);
    }

    #[test]
    fn the_speed_along_a_slope_is_the_wish_up_and_down() {
        for degrees in [10.0_f32, 26.6, 40.0] {
            let slope = degrees.to_radians();
            for (name, wish_x) in [("up", 300.0_f32), ("down", -300.0)] {
                // The plane rises toward +X.
                let normal = normal_of(degrees, Vec3::X);
                let wish = Vec3::new(wish_x, 0.0, 0.0);
                let velocity = slope_velocity(wish, normal);
                let slid = along_the_plane(velocity, normal);
                assert!(
                    (slid.length() - 300.0).abs() < 0.5,
                    "{degrees} degrees {name}: speed along the surface {} is not 300",
                    slid.length()
                );
                assert!(
                    (slid.x.abs() - 300.0 * slope.cos()).abs() < 0.5,
                    "{degrees} degrees {name}: horizontal speed {} is not W x cos",
                    slid.x.abs()
                );
            }
        }
    }

    #[test]
    fn going_up_the_translation_is_horizontal_and_going_down_it_follows_the_plane() {
        let normal = normal_of(26.6, Vec3::X);
        let up = slope_velocity(Vec3::new(300.0, 0.0, 0.0), normal);
        assert!(up.y.abs() < 1.0e-3, "vertical part is 0 going up: {up:?}");
        assert!(up.x > 300.0, "longer than W before the slide: {up:?}");
        let down = slope_velocity(Vec3::new(-300.0, 0.0, 0.0), normal);
        assert!(down.y < 0.0, "points down the plane: {down:?}");
        assert!(
            down.dot(normal).abs() < 1.0e-2,
            "lies in the plane: {down:?}"
        );
    }

    #[test]
    fn a_sideways_wish_on_a_slope_keeps_its_length_in_the_plane() {
        let normal = normal_of(30.0, Vec3::X);
        let wish = Vec3::new(0.0, 0.0, 300.0);
        let velocity = slope_velocity(wish, normal);
        assert!(
            (velocity - wish).length() < 1.0e-2,
            "a contour wish is unchanged"
        );
    }
}
