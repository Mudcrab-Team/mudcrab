//! Explicit numeric grass options apply after the shared Skyrim INI reader.

use super::EngineConfig;
use shared::grass_settings::{GrassSettings, Report};
use std::sync::Arc;

#[derive(Default)]
pub(super) struct Options {
    pub requested: bool,
    pub explicit_stream_radius: bool,
    numeric: Vec<(String, String)>,
    error: Option<String>,
}

impl Options {
    /// Retain a numeric CLI override without consuming another option as its value.
    pub fn add_numeric(&mut self, flag: String, value: Option<String>) {
        match value {
            Some(value) => self.numeric.push((flag, value)),
            None => {
                self.error
                    .get_or_insert_with(|| format!("missing value for {flag}"));
            }
        }
    }

    /// Apply numeric options after INI settings and record runtime support diagnostics.
    pub fn apply(self, config: &mut EngineConfig) {
        if let Some(error) = self.error {
            config.grass_settings_error.get_or_insert(error);
        }
        if !self.requested && self.numeric.is_empty() {
            return;
        }
        if config.grass_settings_report.is_none() {
            config.apply_grass_settings(GrassSettings::default().report());
        }
        config.grass = self.requested
            && config
                .grass_settings_report
                .as_ref()
                .and_then(|report| report.boolean("bDrawShaderGrass"))
                .unwrap_or(true);
        if config.grass {
            let report = Arc::clone(config.grass_settings_report.as_ref().unwrap());
            config.map_grass_settings(&report);
        }
        for (flag, value) in self.numeric {
            let valid = match flag.as_str() {
                "--grass-radius" => value.parse().map(|v| config.grass_radius = v).is_ok(),
                "--grass-uploads-per-frame" => value
                    .parse()
                    .map(|v| config.grass_uploads_per_frame = v)
                    .is_ok(),
                "--grass-density" => value
                    .parse()
                    .map(|v| config.grass_density_multiplier = v)
                    .is_ok(),
                "--grass-min-spacing" => {
                    value.parse().map(|v| config.grass_min_spacing = v).is_ok()
                }
                "--grass-fade-start" => value.parse().map(|v| config.grass_fade_start = v).is_ok(),
                "--grass-fade-end" => value.parse().map(|v| config.grass_fade_end = v).is_ok(),
                "--grass-cache-mib" => value.parse().map(|v| config.grass_cache_mib = v).is_ok(),
                _ => unreachable!(),
            };
            if !valid {
                config
                    .grass_settings_error
                    .get_or_insert_with(|| format!("invalid {flag} value {value:?}"));
            }
            config.grass_settings_diagnostics.push(format!(
                "explicit CLI {flag}={value} overrides the imported settings report"
            ));
        }
        let world_grass = config.grass && config.loads_world();
        if world_grass && !self.explicit_stream_radius && config.grass_radius != i32::MAX {
            config.stream_radius = config
                .stream_radius
                .max(config.grass_radius.saturating_add(1));
            config.unload_radius = config.stream_radius.saturating_add(1);
            // Saved renderer preferences may widen this ring, but must retain grass's minimum.
            config.settings_overrides.push("grass:stream-radius".into());
        }
        if world_grass && config.effective_grass_radius() < config.grass_radius {
            config.grass_settings_diagnostics.push(format!(
                "requested grass radius {} is clamped to {} by terrain stream radius {}",
                config.grass_radius,
                config.effective_grass_radius(),
                config.stream_radius
            ));
        }
        if world_grass && !config.grass_enable_fade {
            config.grass_settings_diagnostics.push(
                "grass fading is disabled; grass still unloads at the generated cell ring".into(),
            );
        }
        if world_grass
            && config.grass_enable_fade
            && config.effective_grass_fade().1 < config.grass_fade_end
        {
            config.grass_settings_diagnostics.push(format!(
                "requested grass fade {}..{} is clamped to {:?} by the generated cell ring; exact Skyrim distance is not honored",
                config.grass_fade_start, config.grass_fade_end, config.effective_grass_fade()
            ));
        }
    }
}

impl EngineConfig {
    /// Retain the shared INI report and apply its runtime values only to enabled grass.
    pub(crate) fn apply_grass_settings(&mut self, report: Report) {
        let defaults = GrassSettings::default();
        self.grass &= report
            .boolean("bDrawShaderGrass")
            .unwrap_or(defaults.draw_shader_grass);
        if self.grass {
            self.map_grass_settings(&report);
        }
        self.grass_settings_diagnostics.push(
            "imported grass wind magnitude settings are reported but not mapped to MudCrab's wind model".into(),
        );
        self.grass_settings_diagnostics.push(
            "bAllowCreateGrass, bAllowLoadGrass and bGenerateGrassDataFiles are reported; Skyrim legacy grass-cache behavior is not implemented".into(),
        );
        self.grass_settings_diagnostics.push(
            "fGrassMinStartFadeDistance and fGrassMaxStartFadeDistance are slider bounds; runtime fade uses fGrassStartFadeDistance + fGrassFadeRange".into(),
        );
        self.grass_settings_report = Some(Arc::new(report));
    }

    /// Apply accepted INI assignments while preserving unmentioned or malformed values.
    fn map_grass_settings(&mut self, report: &Report) {
        // A source named "default" can be a real file; line provenance identifies imports.
        let imported = |key: &str| report.settings[key].effective.line.is_some();
        if imported("iMinGrassSize") {
            self.grass_min_spacing = report.integer("iMinGrassSize").unwrap() as f32;
        }
        if imported("iGrassCellRadius") {
            self.grass_radius = report.integer("iGrassCellRadius").unwrap();
        }
        if imported("iMaxGrassTypesPerTexure") {
            match usize::try_from(report.integer("iMaxGrassTypesPerTexure").unwrap()) {
                Ok(value) => self.grass_max_types_per_texture = value,
                Err(_) => {
                    self.grass_settings_error.get_or_insert_with(|| {
                        "negative iMaxGrassTypesPerTexure cannot be applied by MudCrab".into()
                    });
                }
            }
        }
        if imported("fTexturePctThreshold") {
            self.grass_texture_threshold = report.float("fTexturePctThreshold").unwrap();
        }
        if imported("bEnableGrassFade") {
            self.grass_enable_fade = report.boolean("bEnableGrassFade").unwrap();
        }
        if imported("fGrassStartFadeDistance") || imported("fGrassFadeRange") {
            let range = if imported("fGrassFadeRange") {
                report.float("fGrassFadeRange").unwrap()
            } else {
                self.grass_fade_end - self.grass_fade_start
            };
            if imported("fGrassStartFadeDistance") {
                self.grass_fade_start = report.float("fGrassStartFadeDistance").unwrap();
            }
            self.grass_fade_end = self.grass_fade_start + range;
        }
    }
}
