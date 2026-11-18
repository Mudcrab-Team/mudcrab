//! The rotation of a placed reference.
//!
//! Rule, in our own words:
//!
//! - **RF-REFR-001, the reference rotation.** A reference stores Euler angles X, Y and Z. When the
//!   reference is an actor reference (`ACHR`, which includes the player), X and Y become 0 and only
//!   Z, the heading, is kept. Any other reference keeps all three angles.
//!
//! The angles are in the Creation Engine's own axes and units. The caller converts them to the
//! engine's axes after this rule.

/// The Euler angles (X, Y, Z) a reference is placed with (RF-REFR-001). An actor reference keeps
/// only Z, and its X and Y are positive zero. Any other reference keeps every angle unchanged.
pub fn reference_rotation(euler: [f32; 3], is_actor_reference: bool) -> [f32; 3] {
    if is_actor_reference {
        [0.0, 0.0, euler[2]]
    } else {
        euler
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_actor_reference_keeps_only_its_heading() {
        assert_eq!(reference_rotation([0.5, -0.25, 1.5], true), [0.0, 0.0, 1.5]);
        assert_eq!(reference_rotation([-0.0, -0.0, -0.0], true)[0].to_bits(), 0);
    }

    #[test]
    fn any_other_reference_keeps_all_three_angles() {
        assert_eq!(
            reference_rotation([0.5, -0.25, 1.5], false),
            [0.5, -0.25, 1.5]
        );
    }
}

/// Kani proofs, for every input, including NaN, infinities and negative zero.
#[cfg(kani)]
mod proofs {
    use super::*;

    fn bits(angles: [f32; 3]) -> [u32; 3] {
        angles.map(f32::to_bits)
    }

    /// RF-REFR-001: Z is kept bit for bit for every reference.
    #[kani::proof]
    fn the_heading_is_always_kept() {
        let euler: [f32; 3] = kani::any();
        let actor: bool = kani::any();
        assert_eq!(
            reference_rotation(euler, actor)[2].to_bits(),
            euler[2].to_bits()
        );
    }

    /// RF-REFR-001: an actor reference has X and Y of positive zero.
    #[kani::proof]
    fn an_actor_reference_drops_x_and_y() {
        let euler: [f32; 3] = kani::any();
        let rotation = reference_rotation(euler, true);
        assert_eq!(rotation[0].to_bits(), 0);
        assert_eq!(rotation[1].to_bits(), 0);
    }

    /// RF-REFR-001: any other reference keeps all three angles bit for bit.
    #[kani::proof]
    fn any_other_reference_keeps_every_angle() {
        let euler: [f32; 3] = kani::any();
        assert_eq!(bits(reference_rotation(euler, false)), bits(euler));
    }
}
