//! Grass appearance settings. File edits update uniforms, never pipelines.
use bevy::prelude::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{io::Read, path::PathBuf, time::Duration};

/// Maximum settings input; bounds both polling allocations and TOML parsing work.
const MAX_FILE_BYTES: u64 = 65_536;

/// A preset supplies defaults before explicit scalar overrides are applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GrassMode {
    #[default]
    D,
    E,
    F,
}

/// Validated, effective appearance parameters, independent of file syntax.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GrassTuning {
    pub mode: GrassMode,
    pub brightness: f32,
    pub ambient_strength: f32,
    pub sun_strength: f32,
    pub sun_angle_weight: f32,
    pub wrap: f32,
    pub shadow_strength: f32,
    pub shadow_floor: f32,
    pub backlight: f32,
    pub specular_strength: f32,
    pub specular_roughness: f32,
    pub saturation: f32,
    pub vclr_tint_strength: f32,
    pub color_range_strength: f32,
    pub root_darkening: f32,
}

impl Default for GrassTuning {
    /// Flat-b appearance; authored colour variation remains enabled.
    fn default() -> Self {
        Self {
            mode: GrassMode::D,
            brightness: 1.0,
            ambient_strength: 1.15,
            sun_strength: 0.1,
            sun_angle_weight: 0.0,
            wrap: 0.0,
            shadow_strength: 0.3,
            shadow_floor: 0.0,
            backlight: 0.0,
            specular_strength: 0.0,
            specular_roughness: 0.5,
            saturation: 0.75,
            vclr_tint_strength: 1.0,
            color_range_strength: 1.0,
            root_darkening: 0.0,
        }
    }
}

impl GrassTuning {
    /// Resolve the prior D/E/F switch into parameters, before user overrides.
    pub fn preset(mode: GrassMode) -> Self {
        let mut values = Self {
            mode,
            ..Self::default()
        };
        match mode {
            GrassMode::D => {}
            GrassMode::E => values.sun_angle_weight = 0.0,
            GrassMode::F => values.shadow_floor = 0.68,
        }
        values
    }

    /// Pack values in the shared Rust/WGSL layout; edits do not change this layout.
    pub fn as_uniforms(&self) -> [Vec4; 4] {
        [
            Vec4::new(
                self.brightness,
                self.ambient_strength,
                self.sun_strength,
                self.sun_angle_weight,
            ),
            Vec4::new(
                self.wrap,
                self.shadow_strength,
                self.shadow_floor,
                self.backlight,
            ),
            Vec4::new(
                self.saturation,
                self.vclr_tint_strength,
                self.color_range_strength,
                self.root_darkening,
            ),
            Vec4::new(
                self.specular_strength,
                self.specular_roughness,
                f32::from(self.mode == GrassMode::F),
                0.0,
            ),
        ]
    }

    /// Parse one flat TOML file atomically. Unknown keys warn; invalid scalars reject it.
    /// Finite out-of-range numbers clamp to the documented bounds with a warning.
    pub fn parse(source: &str) -> Result<(Self, Vec<String>), String> {
        let document = source
            .trim_start_matches('\u{feff}')
            .parse::<toml_edit::Document<String>>()
            .map_err(|error| format!("invalid grass tuning TOML: {error}"))?;
        let mode = match document.get("mode") {
            None => GrassMode::D,
            Some(value) => match value.as_str() {
                Some("d") => GrassMode::D,
                Some("e") => GrassMode::E,
                Some("f") => GrassMode::F,
                _ => return Err("mode must be \"d\", \"e\", or \"f\"".into()),
            },
        };
        let mut values = Self::preset(mode);
        let mut warnings = Vec::new();
        for (key, item) in document.iter() {
            let (target, min, max) = match key {
                "mode" => continue,
                "brightness" => (&mut values.brightness, 0.0, 8.0),
                "ambient_strength" => (&mut values.ambient_strength, 0.0, 8.0),
                "sun_strength" => (&mut values.sun_strength, 0.0, 8.0),
                "sun_angle_weight" => (&mut values.sun_angle_weight, 0.0, 1.0),
                "wrap" => (&mut values.wrap, 0.0, 1.0),
                "shadow_strength" => (&mut values.shadow_strength, 0.0, 1.0),
                "shadow_floor" => (&mut values.shadow_floor, 0.0, 1.0),
                "backlight" => (&mut values.backlight, 0.0, 1.0),
                "specular_strength" => (&mut values.specular_strength, 0.0, 1.0),
                "specular_roughness" => (&mut values.specular_roughness, 0.089, 1.0),
                "saturation" => (&mut values.saturation, 0.0, 4.0),
                "vclr_tint_strength" => (&mut values.vclr_tint_strength, 0.0, 1.0),
                "color_range_strength" => (&mut values.color_range_strength, 0.0, 1.0),
                "root_darkening" => (&mut values.root_darkening, 0.0, 1.0),
                _ => {
                    warnings.push(format!("unknown grass tuning key {key:?}; ignored"));
                    continue;
                }
            };
            let value = item
                .as_float()
                .or_else(|| item.as_integer().map(|value| value as f64))
                .ok_or_else(|| format!("{key} must be a finite number"))?;
            if !value.is_finite() {
                return Err(format!("{key} must be a finite number"));
            }
            let bounded = value.clamp(min, max);
            if value != bounded {
                warnings.push(format!("{key} clamped from {value} to {bounded}"));
            }
            *target = bounded as f32;
        }
        Ok((values, warnings))
    }
}

/// Last accepted file contents and current effective values; rejected edits never replace them.
#[derive(Resource, Debug)]
pub struct GrassTuningState {
    pub path: Option<PathBuf>,
    pub effective: GrassTuning,
    sha256: Option<String>,
    last_attempt: Option<String>,
    next_poll: Duration,
    warnings: Vec<String>,
    revision: u64,
}

impl GrassTuningState {
    /// Load once at startup. Missing/invalid files retain defaults and remain eligible for reload.
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut state = Self {
            path,
            effective: GrassTuning::default(),
            sha256: None,
            last_attempt: None,
            next_poll: Duration::ZERO,
            warnings: Vec::new(),
            revision: 0,
        };
        state.poll_at(Duration::ZERO);
        state
    }

    /// The appearance pipeline is active even without a selected file.
    pub fn enabled(&self) -> bool {
        true
    }

    /// Provenance for the actual accepted bytes, including the last good state after bad edits.
    pub fn receipt(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path,
            "sha256": self.sha256,
            "effective": self.effective,
            "revision": self.revision,
            "warnings": self.warnings,
            "enabled": self.enabled(),
            "accepted": self.sha256.is_some(),
        })
    }

    /// Poll at most once per second using elapsed app time; returns whether values were accepted.
    /// Reading bytes instead of mtime detects atomic saves and same-size/timestamp edits.
    pub fn poll_at(&mut self, now: Duration) -> bool {
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now.saturating_add(Duration::from_secs(1));
        let Some(path) = &self.path else {
            return false;
        };
        let read = || -> Result<Vec<u8>, String> {
            let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
            let mut bytes = Vec::new();
            file.take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err("grass tuning exceeds 64 KiB".into());
            }
            Ok(bytes)
        };
        let (fingerprint, parsed) = match read() {
            Ok(bytes) => {
                let hash = format!("{:x}", Sha256::digest(&bytes));
                let parsed = std::str::from_utf8(&bytes)
                    .map_err(|error| error.to_string())
                    .and_then(GrassTuning::parse);
                (hash, parsed)
            }
            Err(error) => (format!("read error: {error}"), Err(error)),
        };
        if self.last_attempt.as_ref() == Some(&fingerprint) {
            return false;
        }
        self.last_attempt = Some(fingerprint.clone());
        match parsed {
            Ok((effective, warnings)) => {
                self.effective = effective;
                self.sha256 = Some(fingerprint);
                self.warnings = warnings;
                self.revision = self.revision.saturating_add(1);
                for message in &self.warnings {
                    warn!(path = %path.display(), "{message}");
                }
                info!(path = %path.display(), revision = self.revision, "Grass tuning loaded");
                true
            }
            Err(error) => {
                warn!(path = %path.display(), "Ignoring grass tuning edit; retaining last good values: {error}");
                self.warnings = vec![error];
                false
            }
        }
    }
}

/// Run in the main world before grass material extraction; requires no GPU.
pub fn reload_grass_tuning(time: Res<Time>, mut state: ResMut<GrassTuningState>) {
    state.poll_at(time.elapsed());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty input and no selected file retain the literal flat-b defaults.
    #[test]
    fn grass_tuning_default_and_presets() {
        let d = GrassTuning::parse("").unwrap().0;
        assert_eq!(d, GrassTuning::default());
        assert_eq!(GrassTuningState::new(None).effective, d);
        assert_eq!(
            d.as_uniforms(),
            [
                Vec4::new(1., 1.15, 0.1, 0.),
                Vec4::new(0., 0.3, 0., 0.),
                Vec4::new(0.75, 1., 1., 0.),
                Vec4::new(0., 0.5, 0., 0.)
            ]
        );
        assert_eq!(
            GrassTuning::parse("mode='e'").unwrap().0.sun_angle_weight,
            0.0
        );
        let f = GrassTuning::parse("mode='f'").unwrap().0;
        assert_eq!(f.shadow_floor, 0.68);
        assert_eq!(f.as_uniforms()[3].z, 1.0);
        assert!(GrassTuning::parse("brightness=.5\nmode='e'").is_err());
        assert_eq!(
            GrassTuning::parse("brightness=0.5\nmode='e'")
                .unwrap()
                .0
                .brightness,
            0.5
        );
    }

    /// Every numeric field rejects nonfinite/types and clamps finite bounds with diagnostics.
    #[test]
    fn grass_tuning_validation_and_unknown_keys() {
        for key in [
            "brightness",
            "ambient_strength",
            "sun_strength",
            "sun_angle_weight",
            "wrap",
            "shadow_strength",
            "shadow_floor",
            "backlight",
            "specular_strength",
            "specular_roughness",
            "saturation",
            "vclr_tint_strength",
            "color_range_strength",
            "root_darkening",
        ] {
            for value in ["nan", "inf", "-inf", "'bad'", "true", "[]"] {
                assert!(
                    GrassTuning::parse(&format!("{key}={value}")).is_err(),
                    "{key}={value}"
                );
            }
            assert_eq!(
                GrassTuning::parse(&format!("{key}=-10")).unwrap().1.len(),
                1
            );
            assert_eq!(
                GrassTuning::parse(&format!("{key}=100")).unwrap().1.len(),
                1
            );
        }
        let (values, warnings) =
            GrassTuning::parse("brightness=0.25\nfuture=2\n[unknown]\nx=1").unwrap();
        assert_eq!(values.brightness, 0.25);
        assert_eq!(warnings.len(), 2);
        for source in [
            "mode='z'",
            "brightness=0.4\nbrightness=0.5",
            "[",
            "sun_strength={x=1}",
        ] {
            assert!(GrassTuning::parse(source).is_err());
        }
    }

    /// The real ECS reload system sees valid edits and retains good values/hash on invalid edits.
    #[test]
    fn grass_tuning_hot_reload_cpu_system() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tuning.toml");
        std::fs::write(&path, "brightness=0.8").unwrap();
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(GrassTuningState::new(Some(path.clone())))
            .add_systems(Update, reload_grass_tuning);
        std::fs::write(&path, "brightness=0.3").unwrap();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.update();
        let state = app.world().resource::<GrassTuningState>();
        assert_eq!(state.effective.brightness, 0.3);
        let hash = state.receipt()["sha256"].clone();
        std::fs::write(&path, "brightness='partial save'").unwrap();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.update();
        let state = app.world().resource::<GrassTuningState>();
        assert_eq!(state.effective.brightness, 0.3);
        assert_eq!(state.receipt()["sha256"], hash);
        assert_eq!(state.receipt()["revision"], 2);
        std::fs::write(&path, "mode='f'").unwrap();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(1));
        app.update();
        assert_eq!(
            app.world().resource::<GrassTuningState>().effective.mode,
            GrassMode::F
        );
    }

    /// Missing and oversized inputs stay recoverable; polling frequency is bounded.
    #[test]
    fn grass_tuning_missing_oversize_and_poll_interval() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("later.toml");
        let mut state = GrassTuningState::new(Some(path.clone()));
        assert_eq!(state.effective.brightness, 1.0);
        std::fs::write(&path, "brightness=0.6").unwrap();
        assert!(!state.poll_at(Duration::from_millis(999)));
        assert!(state.poll_at(Duration::from_secs(1)));
        std::fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(!state.poll_at(Duration::from_secs(2)));
        assert_eq!(state.effective.brightness, 0.6);
        std::fs::write(&path, "brightness=0.4").unwrap();
        assert!(state.poll_at(Duration::from_secs(3)));
        assert_eq!(state.effective.brightness, 0.4);
    }
}
