//! The scale of a placed reference and of an NPC.
//!
//! Rules, in our own words:
//!
//! - **RF-NPC-005, the sex index.** For an NPC base form, the sex index is bit 0 of the `ACBS`
//!   flags: 0 is male and 1 is female. Any other form type gives -1.
//! - **RF-NPC-004, the NPC scale.** NPC scale = race height for the sex × NPC height (`NAM6`).
//!   The race height is the male height for index 0 and the female height for index 1. Any other
//!   index gives a race height of 0. With no race, the race height is 1.0 for every index.
//! - **RF-REFR-002, the reference scale.** A reference stores its scale as a whole percent in a
//!   `u16`. Its scale is that percent × 0.01 in single precision. When the base form is an NPC, the
//!   result is then multiplied by the NPC scale. RF-CC-004 uses the same product for jump height.
//!
//! # The grouping, and assumptions still open
//!
//! - **The grouping (confirmed statically).** The reference scale of an NPC is
//!   `(f32(percent) × 0.01) × (race height × NPC height)`, with each product rounded to `f32`
//!   (round to nearest) and no other grouping. RF-REFR-002 and RF-NPC-004 state the factors. The
//!   grouping was confirmed from the retail machine code: it multiplies in single precision in this
//!   order. Other groupings differ in the last bit for some inputs (see the
//!   `grouping_changes_the_last_bit` cover and the `reference_scale_keeps_the_retail_grouping`
//!   test). A bitwise run of the retail code on these inputs is still pending, so this is a static
//!   confirmation, not an executed one.
//! - **A2, a missing `NAM6`.** An NPC record without `NAM6` has an NPC height of 1.0. The registry
//!   does not say what the loader stores in that case.
//! - **A3, the stored percent.** The registry does not say how the loader turns a plugin's `XSCL`
//!   float into the whole percent. [`reference_scale`] therefore takes the scale the caller already
//!   has, and [`stored_reference_scale`] covers the percent form.

/// The male and female heights of a race, from `RACE/DATA` bytes 16 and 20 (RF-NPC-004).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RaceHeights {
    /// The height for sex index 0.
    pub male: f32,
    /// The height for sex index 1.
    pub female: f32,
}

/// The sex index of a male NPC (RF-NPC-005).
pub const SEX_INDEX_MALE: i32 = 0;
/// The sex index of a female NPC (RF-NPC-005).
pub const SEX_INDEX_FEMALE: i32 = 1;
/// The sex index of a form that is not an NPC (RF-NPC-005).
pub const SEX_INDEX_NOT_AN_NPC: i32 = -1;

/// The race height when an NPC has no race (RF-NPC-004).
pub const NO_RACE_HEIGHT: f32 = 1.0;
/// The NPC height when the record has no `NAM6`. This is assumption A2.
pub const DEFAULT_NPC_HEIGHT: f32 = 1.0;
/// The factor that turns a stored whole percent into a scale (RF-REFR-002). As an `f32` it is
/// 0.00999999977648258 (bits `0x3c23d70a`), not exactly 0.01.
pub const PERCENT_TO_SCALE: f32 = 0.01;

/// The form type of an NPC base form (`NPC_`).
pub const NPC_FORM_TYPE: u8 = 0x2B;

/// Bit 0 of the `ACBS` flags marks a female NPC.
const ACBS_FEMALE: u32 = 0x1;

/// The sex index of a form (RF-NPC-005): bit 0 of the `ACBS` flags for an NPC, and
/// [`SEX_INDEX_NOT_AN_NPC`] for any other form type.
pub fn sex_index(form_is_npc: bool, acbs_flags: u32) -> i32 {
    if form_is_npc {
        if acbs_flags & ACBS_FEMALE != 0 {
            SEX_INDEX_FEMALE
        } else {
            SEX_INDEX_MALE
        }
    } else {
        SEX_INDEX_NOT_AN_NPC
    }
}

/// The sex index of a form from its form type and `ACBS` flags (RF-NPC-005).
pub fn sex_index_of_form_type(form_type: u8, acbs_flags: u32) -> i32 {
    sex_index(form_type == NPC_FORM_TYPE, acbs_flags)
}

/// The sex index of an NPC whose sex the caller already knows.
pub fn npc_sex_index(female: bool) -> i32 {
    if female {
        SEX_INDEX_FEMALE
    } else {
        SEX_INDEX_MALE
    }
}

/// The race height for a sex index (RF-NPC-004). With no race it is [`NO_RACE_HEIGHT`]. With a
/// race, index 0 gives the male height, index 1 the female height, and any other index 0.0.
pub fn race_height(race: Option<RaceHeights>, sex_index: i32) -> f32 {
    match race {
        None => NO_RACE_HEIGHT,
        Some(heights) => match sex_index {
            SEX_INDEX_MALE => heights.male,
            SEX_INDEX_FEMALE => heights.female,
            _ => 0.0,
        },
    }
}

/// The NPC scale (RF-NPC-004): the race height for the sex × the NPC height (`NAM6`).
///
/// `npc_height` is `None` when the record has no `NAM6`; that gives [`DEFAULT_NPC_HEIGHT`]
/// (assumption A2). The result is the retail value. It can be zero, negative or not finite when
/// the inputs are, and the caller decides what to draw in that case.
pub fn npc_scale(race: Option<RaceHeights>, sex_index: i32, npc_height: Option<f32>) -> f32 {
    race_height(race, sex_index) * npc_height.unwrap_or(DEFAULT_NPC_HEIGHT)
}

/// The scale that a stored whole percent gives (RF-REFR-002): `percent × 0.01` in `f32`.
pub fn stored_reference_scale(percent: u16) -> f32 {
    f32::from(percent) * PERCENT_TO_SCALE
}

/// The scale of a placed reference (RF-REFR-002). `base_scale` is the reference's own scale.
/// `npc_scale` is the [`npc_scale`] of the base form when that form is an NPC, and `None`
/// otherwise. Pass the NPC scale as one value: the retail grouping multiplies the race and NPC
/// heights first (see the module notes).
pub fn reference_scale(base_scale: f32, npc_scale: Option<f32>) -> f32 {
    match npc_scale {
        Some(npc_scale) => base_scale * npc_scale,
        None => base_scale,
    }
}

/// The retail reference scale from the stored percent: [`stored_reference_scale`] then
/// [`reference_scale`].
pub fn retail_reference_scale(percent: u16, npc_scale: Option<f32>) -> f32 {
    reference_scale(stored_reference_scale(percent), npc_scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RACE: RaceHeights = RaceHeights {
        male: 1.25,
        female: 0.75,
    };

    /// Synthetic inputs only. The heights are dyadic, so each expected product is exact.
    #[test]
    fn npc_scale_table() {
        // (race, sex index, NAM6, expected)
        let rows: [(Option<RaceHeights>, i32, Option<f32>, f32); 12] = [
            (Some(RACE), SEX_INDEX_MALE, Some(2.0), 2.5),
            (Some(RACE), SEX_INDEX_FEMALE, Some(2.0), 1.5),
            (Some(RACE), SEX_INDEX_MALE, None, 1.25),
            (Some(RACE), SEX_INDEX_FEMALE, None, 0.75),
            (None, SEX_INDEX_MALE, Some(0.5), 0.5),
            (None, SEX_INDEX_FEMALE, Some(0.5), 0.5),
            (None, SEX_INDEX_NOT_AN_NPC, Some(0.5), 0.5),
            (None, SEX_INDEX_MALE, None, 1.0),
            (Some(RACE), 2, Some(2.0), 0.0),
            (Some(RACE), 255, Some(2.0), 0.0),
            (Some(RACE), SEX_INDEX_NOT_AN_NPC, Some(2.0), 0.0),
            (Some(RACE), SEX_INDEX_MALE, Some(0.0), 0.0),
        ];
        for (race, sex, height, expected) in rows {
            assert_eq!(
                npc_scale(race, sex, height).to_bits(),
                expected.to_bits(),
                "{race:?} {sex} {height:?}"
            );
        }
    }

    #[test]
    fn sex_index_reads_acbs_bit_zero_for_an_npc_only() {
        assert_eq!(sex_index(true, 0), SEX_INDEX_MALE);
        assert_eq!(sex_index(true, 1), SEX_INDEX_FEMALE);
        // Other flag bits do not change the sex.
        assert_eq!(sex_index(true, 0xFFFF_FFFE), SEX_INDEX_MALE);
        assert_eq!(sex_index(true, 0x8000_0001), SEX_INDEX_FEMALE);
        assert_eq!(sex_index(false, 1), SEX_INDEX_NOT_AN_NPC);
        assert_eq!(npc_sex_index(true), SEX_INDEX_FEMALE);
        assert_eq!(npc_sex_index(false), SEX_INDEX_MALE);
    }

    #[test]
    fn stored_percent_table() {
        // 0.01 is not exact in f32, but these whole percents still round to the exact scale.
        for (percent, expected) in [
            (0u16, 0.0f32),
            (50, 0.5),
            (100, 1.0),
            (200, 2.0),
            (400, 4.0),
        ] {
            assert_eq!(
                stored_reference_scale(percent).to_bits(),
                expected.to_bits(),
                "{percent}"
            );
        }
    }

    /// All 65,536 stored percents: the scale is within one part in 2^23 of `percent / 100`, and it
    /// never falls as the percent rises.
    #[test]
    fn every_stored_percent_is_close_to_its_exact_value_and_monotonic() {
        let mut previous = 0.0f32;
        for percent in 0..=u16::MAX {
            let scale = stored_reference_scale(percent);
            let exact = f64::from(percent) / 100.0;
            assert!(
                (f64::from(scale) - exact).abs() <= exact / f64::from(1u32 << 23),
                "{percent}"
            );
            assert!(scale >= previous, "{percent}");
            previous = scale;
        }
    }

    /// Synthetic inputs where the two groupings differ in the last bit. The retail grouping is
    /// `(percent × 0.01) × (race × NAM6)`; the other grouping, `((percent × 0.01) × race) × NAM6`,
    /// gives `0x3eb1205c`.
    #[test]
    fn reference_scale_keeps_the_retail_grouping() {
        assert_eq!(PERCENT_TO_SCALE.to_bits(), 0x3c23_d70a);
        let race = RaceHeights {
            male: f32::from_bits(0x3f8c_cccd), // 1.1
            female: 1.0,
        };
        let height = f32::from_bits(0x3f59_999a); // 0.85
        let npc = npc_scale(Some(race), SEX_INDEX_MALE, Some(height));
        assert_eq!(retail_reference_scale(37, Some(npc)).to_bits(), 0x3eb1_205d);
        let other = (stored_reference_scale(37) * race.male) * height;
        assert_eq!(other.to_bits(), 0x3eb1_205c);
    }

    #[test]
    fn reference_scale_multiplies_only_for_an_npc_base() {
        assert_eq!(reference_scale(2.0, None), 2.0);
        assert_eq!(reference_scale(2.0, Some(1.5)), 3.0);
        assert_eq!(retail_reference_scale(100, None), 1.0);
        assert_eq!(
            retail_reference_scale(200, Some(npc_scale(Some(RACE), SEX_INDEX_MALE, Some(2.0)))),
            5.0
        );
    }
}

/// Kani proofs. Each harness states its range. "Same value" means the same bits, or both NaN.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// The largest height magnitude that the finiteness proofs cover. Retail heights are near 1.0;
    /// this bound is far above them and keeps every product below `f32::MAX`.
    const HEIGHT_RANGE: f32 = 1.0e6;

    fn same(a: f32, b: f32) -> bool {
        a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
    }

    fn any_race() -> Option<RaceHeights> {
        if kani::any() {
            Some(RaceHeights {
                male: kani::any(),
                female: kani::any(),
            })
        } else {
            None
        }
    }

    fn in_range(value: f32) -> bool {
        value.is_finite() && value.abs() <= HEIGHT_RANGE
    }

    /// True unless `a × b` is zero times an infinity. IEEE arithmetic, and so retail, gives NaN
    /// for that product. Kani reports every NaN that a multiplication makes from non-NaN inputs as
    /// a failure, so a harness over all floats excludes exactly this case and no other.
    fn not_zero_times_infinity(a: f32, b: f32) -> bool {
        !((a == 0.0 && b.is_infinite()) || (a.is_infinite() && b == 0.0))
    }

    /// RF-NPC-005: an NPC's index is bit 0 of its flags. Any other form gives -1.
    #[kani::proof]
    fn sex_index_is_acbs_bit_zero() {
        let form_is_npc: bool = kani::any();
        let flags: u32 = kani::any();
        let index = sex_index(form_is_npc, flags);
        if form_is_npc {
            assert_eq!(index, (flags & 1) as i32);
        } else {
            assert_eq!(index, -1);
        }
    }

    /// RF-NPC-004: with no race, the scale is the NPC height, for every sex index.
    #[kani::proof]
    fn missing_race_gives_the_npc_height() {
        let sex: i32 = kani::any();
        let height: f32 = kani::any();
        assert!(same(npc_scale(None, sex, Some(height)), height));
    }

    /// RF-NPC-004 with A2: with no `NAM6`, the scale is the race height for the sex.
    #[kani::proof]
    fn missing_npc_height_gives_the_race_height() {
        let race = any_race();
        let sex: i32 = kani::any();
        assert!(same(npc_scale(race, sex, None), race_height(race, sex)));
    }

    /// RF-NPC-004: with no race and no `NAM6`, the scale is exactly 1.0.
    #[kani::proof]
    fn missing_race_and_npc_height_give_one() {
        let sex: i32 = kani::any();
        assert_eq!(npc_scale(None, sex, None).to_bits(), 1.0f32.to_bits());
    }

    /// RF-NPC-004: index 0 takes the male height, index 1 the female height, and any other
    /// index 0.0. The expected value is written out here, apart from the code under proof.
    #[kani::proof]
    fn sex_index_selects_the_race_height() {
        let race = RaceHeights {
            male: kani::any(),
            female: kani::any(),
        };
        let sex: i32 = kani::any();
        let height: f32 = kani::any();
        let selected = if sex == 0 {
            race.male
        } else if sex == 1 {
            race.female
        } else {
            0.0
        };
        kani::assume(not_zero_times_infinity(selected, height));
        assert!(same(
            npc_scale(Some(race), sex, Some(height)),
            selected * height
        ));
    }

    /// RF-NPC-004 and RF-NPC-005 together: a female NPC (flag bit 0 set) gets the female height.
    #[kani::proof]
    fn a_female_npc_gets_the_female_height() {
        let race = RaceHeights {
            male: kani::any(),
            female: kani::any(),
        };
        let flags: u32 = kani::any();
        let expected = if flags & 1 == 1 {
            race.female
        } else {
            race.male
        };
        assert!(same(
            race_height(Some(race), sex_index(true, flags)),
            expected
        ));
    }

    /// For finite inputs in range, no step gives NaN or an infinity, for every stored percent and
    /// every sex index.
    #[kani::proof]
    fn finite_inputs_in_range_give_a_finite_scale() {
        let race = any_race();
        if let Some(heights) = race {
            kani::assume(in_range(heights.male) && in_range(heights.female));
        }
        let height: Option<f32> = if kani::any() { Some(kani::any()) } else { None };
        if let Some(height) = height {
            kani::assume(in_range(height));
        }
        let sex: i32 = kani::any();
        let percent: u16 = kani::any();
        let npc = npc_scale(race, sex, height);
        assert!(npc.is_finite());
        let npc_base: Option<f32> = if kani::any() { Some(npc) } else { None };
        assert!(retail_reference_scale(percent, npc_base).is_finite());
    }

    /// Without the range, finite heights can overflow to infinity, so the range above is needed.
    #[kani::proof]
    fn large_finite_heights_can_overflow() {
        let race = RaceHeights {
            male: kani::any(),
            female: kani::any(),
        };
        let height: f32 = kani::any();
        kani::assume(race.male.is_finite() && height.is_finite());
        let scale = npc_scale(Some(race), SEX_INDEX_MALE, Some(height));
        kani::cover!(scale.is_infinite(), "two finite heights overflow");
    }

    /// RF-REFR-002 with the retail grouping, outer step: the reference scale is
    /// `(percent × 0.01) × npc scale`, bit for bit, with the NPC scale as one value. Together with
    /// `sex_index_selects_the_race_height` (NPC scale = race height × NPC height, bit for bit),
    /// this proves `(percent × 0.01) × (race height × NPC height)` for every input except zero
    /// times an infinity.
    ///
    /// The two steps are proved apart on purpose. Each has one multiplication on each side of
    /// the comparison. One harness over the whole chain, with two on each side, also passes, but
    /// it took 13 minutes on a loaded laptop, against about 3 minutes for this step and under one
    /// minute for the other.
    #[kani::proof]
    fn reference_scale_uses_the_retail_grouping() {
        let percent: u16 = kani::any();
        let npc: f32 = kani::any();
        let stored = f32::from(percent) * 0.01;
        kani::assume(not_zero_times_infinity(stored, npc));
        assert!(same(
            retail_reference_scale(percent, Some(npc)),
            stored * npc
        ));
    }

    /// The grouping matters: for some inputs in range, another grouping gives different bits.
    #[kani::proof]
    fn grouping_changes_the_last_bit() {
        let percent: u16 = kani::any();
        let race_height: f32 = kani::any();
        let height: f32 = kani::any();
        kani::assume(in_range(race_height) && in_range(height));
        let stored = f32::from(percent) * 0.01;
        let stated = stored * (race_height * height);
        let other = (stored * race_height) * height;
        kani::cover!(
            stated.to_bits() != other.to_bits(),
            "the grouping is observable"
        );
    }

    /// RF-REFR-002: a reference whose base is not an NPC keeps exactly its own scale.
    #[kani::proof]
    fn a_reference_without_an_npc_base_keeps_its_scale() {
        let base: f32 = kani::any();
        assert!(same(reference_scale(base, None), base));
    }

    /// RF-REFR-002: a stored percent never gives a smaller scale than a smaller percent.
    #[kani::proof]
    fn stored_scale_is_monotonic() {
        let a: u16 = kani::any();
        let b: u16 = kani::any();
        kani::assume(a <= b);
        assert!(stored_reference_scale(a) <= stored_reference_scale(b));
    }

    /// RF-REFR-002: 100 percent is exactly 1.0, although 0.01 is not exact in `f32`.
    #[kani::proof]
    fn one_hundred_percent_is_exactly_one() {
        assert_eq!(stored_reference_scale(100).to_bits(), 1.0f32.to_bits());
    }
}
