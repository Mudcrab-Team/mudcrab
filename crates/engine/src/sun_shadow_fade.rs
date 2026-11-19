//! The retail distance fade of the sun shadow.
//!
//! Retail does not cut the sun shadow at the end of its range. The shadow mask shader multiplies
//! the shadow in by a fade (`RF-LIGHT-GPU-020`): the mask is `m = 1 + fade * (vis - 1)` with
//! `fade = 1 - (d / S)^16`, where `d` is the distance from the camera to the pixel and `S` is the
//! live sun shadow distance (`fShadowDistance`, 8000 by default in retail, `RF-LIGHT-GPU-017`). For
//! `S` = 8000 a shadow keeps nearly full strength to about three quarters of `S` (0.990 at 6000),
//! falls steeply after that (0.815 at 7200, 0.560 at 7600) and is gone at `S`. The shader squares
//! `saturate(d^2 / S^2)` three times, so the exponent on `d / S` is 16, not 8.
//!
//! Bevy has no such fade: its sun shadow ends abruptly at the last cascade's far bound. Mudcrab
//! carries the fade as a marked hunk in the vendored `bevy_pbr`
//! (`vendor/bevy_pbr/src/render/shadows.wgsl`, `fetch_directional_shadow`; see
//! `vendor/BEVY_PATCHES.md`), and `crate::vendored_shadow_shader` fails CI if a re-vendor drops it.
//!
//! `S` in that hunk is the last cascade's far bound, that is the effective cascade range:
//! `sun_shadow_cascades` sets it to the smaller of the drawn-grid fit (about 23,200 units at the
//! default radius) and `[Display] fShadowDistance`. A run without the INI key therefore fades over
//! about 17,400 to 23,200 units, not over 8000.

/// The exponent on `d / S` in the retail fade (`RF-LIGHT-GPU-020`). The retail shader squares
/// `saturate(d^2 / S^2)` three times, which is `(d / S)^16`. It is a power of two: the vendored
/// shader squares the ratio `log2` times, and `crate::vendored_shadow_shader` checks the count.
pub const SUN_SHADOW_FADE_EXPONENT: u32 = 16;

/// The shadow strength at distance `distance` for a shadow range `shadow_distance`:
/// `1 - saturate(d / S)^16`. Zero for a range that is not positive.
///
/// This is the CPU copy of the vendored shader's formula, which the tests pin to the retail values
/// of `RF-LIGHT-GPU-020`.
pub fn sun_shadow_fade(distance: f32, shadow_distance: f32) -> f32 {
    if shadow_distance <= 0.0 || !distance.is_finite() {
        return 0.0;
    }
    1.0 - (distance / shadow_distance)
        .clamp(0.0, 1.0)
        .powi(SUN_SHADOW_FADE_EXPONENT as i32)
}

/// The visible shadow: `m = 1 + fade * (visibility - 1)`, where `visibility` is 1 for a lit pixel
/// and 0 for a fully shadowed one (`RF-LIGHT-GPU-020`).
pub fn faded_visibility(visibility: f32, fade: f32) -> f32 {
    1.0 + fade * (visibility - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RF-LIGHT-GPU-020`: for S = 8000 the fade is 0.990 at 6000, 0.815 at 7200, 0.560 at 7600 and
    /// 0 at 8000 and beyond; 0.5 at d = 0.9576 S.
    #[test]
    fn the_fade_follows_the_retail_curve() {
        assert_eq!(SUN_SHADOW_FADE_EXPONENT, 16, "RF-LIGHT-GPU-020, not 8");
        for (distance, expected) in [
            (0.0, 1.0),
            (6000.0, 0.990),
            (6800.0, 0.926),
            (7200.0, 0.815),
            (7600.0, 0.560),
            (7800.0, 0.333),
            (8000.0, 0.0),
            (8100.0, 0.0),
            (30000.0, 0.0),
        ] {
            let fade = sun_shadow_fade(distance, 8000.0);
            assert!(
                (fade - expected).abs() < 0.001,
                "d = {distance}: {fade} against {expected}"
            );
        }
        // The half way point and the interior values quoted with the row.
        assert!((sun_shadow_fade(0.9576 * 8000.0, 8000.0) - 0.5).abs() < 0.002);
        assert!((sun_shadow_fade(2500.0, 3000.0) - 0.946).abs() < 0.001);
        assert!((sun_shadow_fade(2800.0, 3000.0) - 0.668).abs() < 0.001);
        // The exponent 8 of the first reading would give 0.570 at 7200.
        assert!((sun_shadow_fade(7200.0, 8000.0) - 0.570).abs() > 0.2);
        // A range that is not usable gives no shadow.
        assert_eq!(sun_shadow_fade(100.0, 0.0), 0.0);
        assert_eq!(sun_shadow_fade(f32::NAN, 8000.0), 0.0);
    }

    /// The effective range is the cascade range, not 8000: with no INI key it is the grid fit, so
    /// a shadow at 8000 units is still at full strength there.
    #[test]
    fn the_fade_follows_the_cascade_range() {
        let grid_fit = 23_200.0;
        assert!(sun_shadow_fade(8_000.0, grid_fit) > 0.999);
        assert!((sun_shadow_fade(0.75 * grid_fit, grid_fit) - 0.990).abs() < 0.001);
        assert_eq!(sun_shadow_fade(grid_fit, grid_fit), 0.0);
    }

    /// The mask value: a lit pixel stays lit, a shadowed one fades toward lit with the fade.
    #[test]
    fn the_faded_mask_blends_toward_lit() {
        assert_eq!(faded_visibility(1.0, 0.3), 1.0);
        assert_eq!(faded_visibility(0.0, 1.0), 0.0);
        assert_eq!(faded_visibility(0.0, 0.0), 1.0);
        assert!((faded_visibility(0.25, 0.5) - 0.625).abs() < 1.0e-6);
    }
}
