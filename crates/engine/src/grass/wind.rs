//! Retail grass wind math.
//!
//! Registry sources: RF-GRASS-001, RF-GRASS-002, RF-GRASS-003 and RF-SHADER-002 (high
//! confidence). The retail displacement formula uses the literals 0.4, 1/128, 0.3, 0.2 and 0.5.
//! The engine has no blended `WTHR` wind speed and no wind direction yet, so the shader does not
//! displace blades: nothing calls these functions in a render path until a sourced `w` and a
//! sourced wind direction exist.

use std::f32::consts::TAU;

/// RF-GRASS-002: cap 60 on the wind constant, reached at w >= 0.4583.
pub const WIND_CONSTANT_CAP: f32 = 60.0;

/// RF-GRASS-002: `fWindGrassMultiplier:Display` default 1.0.
pub const WIND_GRASS_MULTIPLIER: f32 = 1.0;

/// RF-GRASS-003: phase scale `1/600` (stored as 0.00166667).
pub const WIND_PHASE_RATE: f32 = 1.0 / 600.0;

/// RF-GRASS-001: `A = fGrassWindMagnitudeMin + (fGrassWindMagnitudeMax - Min) * w`.
///
/// `w` is the blended weather wind speed in 0..1 (`WTHR` wind-speed byte / 255 blended across a
/// weather transition). The defaults 5 and 125 give `5 + 120 * w`.
pub fn wind_amplitude(magnitude_min: f32, magnitude_max: f32, w: f32) -> f32 {
    magnitude_min + (magnitude_max - magnitude_min) * w
}

/// RF-GRASS-002: the value the retail grass shader receives, `min(A, 60) * fWindGrassMultiplier`.
pub fn wind_shader_constant(amplitude: f32, wind_grass_multiplier: f32) -> f32 {
    amplitude.min(WIND_CONSTANT_CAP) * wind_grass_multiplier
}

/// RF-GRASS-003: `phase = TotalTime * (1/600) * 2*pi * value`.
///
/// The formula has high confidence. That `value` is the GRAS wave period (grass shader property
/// field) has medium confidence: the field is not named.
pub fn wind_phase(total_time_seconds: f32, wave_period: f32) -> f32 {
    total_time_seconds * WIND_PHASE_RATE * TAU * wave_period
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: f32 = 5.0;
    const MAX: f32 = 125.0;

    /// RF-GRASS-001: with the retail INI defaults the amplitude is `5 + 120 * w`.
    #[test]
    fn amplitude_is_min_plus_range_times_wind_speed() {
        assert_eq!(wind_amplitude(MIN, MAX, 0.0), 5.0);
        assert_eq!(wind_amplitude(MIN, MAX, 1.0), 125.0);
        assert_eq!(wind_amplitude(MIN, MAX, 0.5), 65.0);
    }

    /// RF-GRASS-002: the shader constant is capped at 60 before the multiplier of 1.0.
    #[test]
    fn shader_constant_caps_at_sixty_times_the_multiplier() {
        let capped = |w| wind_shader_constant(wind_amplitude(MIN, MAX, w), WIND_GRASS_MULTIPLIER);
        assert_eq!(capped(0.0), 5.0);
        assert_eq!(capped(1.0), 60.0);
        assert_eq!(capped(0.5), 60.0);
        assert_eq!(
            wind_shader_constant(65.0, 2.0),
            120.0,
            "the cap comes first"
        );
    }

    /// RF-GRASS-003: `phase = TotalTime / 600 * 2*pi * value`.
    #[test]
    fn phase_is_total_time_over_600_times_tau_times_the_wave_period() {
        assert_eq!(wind_phase(0.0, 1.0), 0.0);
        assert!((wind_phase(600.0, 1.0) - TAU).abs() < 1.0e-5);
        assert!((wind_phase(300.0, 2.0) - TAU).abs() < 1.0e-5);
    }
}
