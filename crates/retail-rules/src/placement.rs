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

/// The form type of an actor reference (`ACHR`, which includes the player).
pub const ACHR_FORM_TYPE: u8 = 0x3E;

/// The Euler angles (X, Y, Z) a reference is placed with (RF-REFR-001). An actor reference keeps
/// only Z. Any other reference keeps every angle unchanged.
///
/// The registry says only that X and Y are replaced by 0. Positive zero is our modelling choice;
/// its sign in retail is unverified.
pub fn reference_rotation(euler: [f32; 3], is_actor_reference: bool) -> [f32; 3] {
    if is_actor_reference {
        [0.0, 0.0, euler[2]]
    } else {
        euler
    }
}

/// [`reference_rotation`] from the reference's own form type: only [`ACHR_FORM_TYPE`] drops X and
/// Y. Callers should use this function, so that the form-type choice is part of the proof.
pub fn placed_reference_rotation(reference_form_type: u8, euler: [f32; 3]) -> [f32; 3] {
    reference_rotation(euler, reference_form_type == ACHR_FORM_TYPE)
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
    fn only_the_achr_form_type_drops_x_and_y() {
        assert_eq!(
            placed_reference_rotation(ACHR_FORM_TYPE, [0.5, -0.25, 1.5]),
            [0.0, 0.0, 1.5]
        );
        assert_eq!(
            placed_reference_rotation(0x3D, [0.5, -0.25, 1.5]),
            [0.5, -0.25, 1.5]
        );
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

    /// RF-REFR-001: only form type 0x3E drops X and Y. Every other form type keeps all three
    /// angles, bit for bit.
    #[kani::proof]
    fn only_an_achr_drops_x_and_y() {
        let form_type: u8 = kani::any();
        let euler: [f32; 3] = kani::any();
        kani::cover!(form_type == ACHR_FORM_TYPE, "an ACHR is reached");
        let expected = if form_type == ACHR_FORM_TYPE {
            [0.0, 0.0, euler[2]]
        } else {
            euler
        };
        assert_eq!(
            bits(placed_reference_rotation(form_type, euler)),
            bits(expected)
        );
    }

    /// RF-REFR-001: any other reference keeps all three angles bit for bit.
    #[kani::proof]
    fn any_other_reference_keeps_every_angle() {
        let euler: [f32; 3] = kani::any();
        assert_eq!(bits(reference_rotation(euler, false)), bits(euler));
    }
}
