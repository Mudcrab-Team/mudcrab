//! Live cluster quality controls.

use bevy::prelude::*;

/// Live cluster quality controls, read by their consumers each frame.
#[derive(Resource, Reflect, Debug, Clone, PartialEq)]
#[reflect(Resource)]
pub struct GraphicsSettings {
    /// Object cluster screen-space error threshold, in pixels.
    pub cluster_error_pixels: f32,
    /// Terrain cluster screen-space error threshold, in pixels.
    pub cluster_terrain_error_pixels: f32,
}

/// The coarsest cluster error threshold a setting accepts, in pixels.
pub const MAX_CLUSTER_ERROR_PIXELS: f32 = 16.0;

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            cluster_error_pixels: 1.0,
            cluster_terrain_error_pixels: 1.0,
        }
    }
}

impl GraphicsSettings {
    /// Apply a graphics control, refusing unknown keys and invalid values.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        if matches!(
            name,
            "cluster-error-pixels" | "cluster-terrain-error-pixels"
        ) {
            let pixels = cluster_error(value).map_err(|expected| {
                format!("invalid --{name} value {value:?}; expected {expected}")
            })?;
            match name {
                "cluster-error-pixels" => self.cluster_error_pixels = pixels,
                _ => self.cluster_terrain_error_pixels = pixels,
            }
            return Ok(());
        }
        Err(format!("unknown graphics setting {name:?}"))
    }
}

fn finite(value: &str) -> Result<f32, String> {
    value
        .parse::<f32>()
        .ok()
        .filter(|number| number.is_finite())
        .ok_or_else(|| "a number".to_owned())
}

fn cluster_error(value: &str) -> Result<f32, String> {
    finite(value)
        .ok()
        .filter(|pixels| *pixels > 0.0 && *pixels <= MAX_CLUSTER_ERROR_PIXELS)
        .ok_or_else(|| format!("a pixel count above 0, at most {MAX_CLUSTER_ERROR_PIXELS}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_thresholds_default_to_one_and_are_independent() {
        let mut settings = GraphicsSettings::default();
        assert_eq!(settings.cluster_error_pixels, 1.0);
        assert_eq!(settings.cluster_terrain_error_pixels, 1.0);
        settings.set("cluster-error-pixels", "0.25").unwrap();
        assert_eq!(settings.cluster_error_pixels, 0.25);
        assert_eq!(settings.cluster_terrain_error_pixels, 1.0);
        settings.set("cluster-terrain-error-pixels", "16").unwrap();
        assert_eq!(
            settings.cluster_terrain_error_pixels,
            MAX_CLUSTER_ERROR_PIXELS
        );
        assert_eq!(settings.cluster_error_pixels, 0.25);
    }

    #[test]
    fn cluster_thresholds_refuse_invalid_values_without_mutation() {
        let mut settings = GraphicsSettings::default();
        settings.set("cluster-error-pixels", "2").unwrap();
        settings.set("cluster-terrain-error-pixels", "4").unwrap();
        let before = settings.clone();
        for name in ["cluster-error-pixels", "cluster-terrain-error-pixels"] {
            for value in [
                "0", "-0", "-1", "NaN", "inf", "-inf", "16.001", "1e99", "bad", "",
            ] {
                assert!(
                    settings.set(name, value).is_err(),
                    "{name} accepted {value}"
                );
                assert_eq!(settings, before);
            }
        }
    }

    #[test]
    fn cluster_threshold_limit_is_sixteen_pixels() {
        assert_eq!(MAX_CLUSTER_ERROR_PIXELS, 16.0);
    }

    #[test]
    fn invalid_controls_do_not_mutate_settings() {
        let mut settings = GraphicsSettings::default();
        assert!(settings.set("unknown", "off").is_err());
        assert_eq!(settings, GraphicsSettings::default());
    }
}
