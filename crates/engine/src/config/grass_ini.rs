//! Explicit numeric grass options apply after the shared Skyrim INI reader.

use super::EngineConfig;
use shared::grass_settings::{GrassSettings, Report};
use std::sync::Arc;

#[derive(Default)]
pub(super) struct Options {
    pub requested: Option<bool>,
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
        if config.grass_settings_report.is_none() {
            config.apply_grass_settings(GrassSettings::default().report());
        }
        config.grass = self.requested.unwrap_or_else(|| {
            config
                .grass_settings_report
                .as_ref()
                .and_then(|report| report.boolean("bDrawShaderGrass"))
                .unwrap_or(true)
        });
        // Radius is a next-start preference even when grass rendering is disabled.
        if let Some(report) = &config.grass_settings_report
            && report.settings["iGrassCellRadius"].effective.line.is_some()
        {
            let report = Arc::clone(report);
            config.map_grass_radius(&report);
        }
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
        config.disable_grass_outside_explicit_terrain();
        let world_grass = world_grass && config.grass;
        // Bound automatic terrain expansion before settings or the planner sees it.
        // Explicit terrain radii retain the caller's deliberate coverage limit.
        if world_grass && !self.explicit_stream_radius && config.grass_radius > 32 {
            config.grass_settings_error.get_or_insert_with(||
                "automatic grass radius exceeds 32 cells; supply an explicit --stream-radius to control terrain coverage".into());
            return;
        }
        if world_grass && !self.explicit_stream_radius && config.grass_radius != i32::MAX {
            config.grass_terrain_radii = Some((config.stream_radius, config.unload_radius));
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
    /// Honor a terrain-only launch limit without replacing the next-start grass request.
    pub(crate) fn disable_grass_outside_explicit_terrain(&mut self) {
        if self.grass
            && self.loads_world()
            && self.stream_radius < 2
            && self
                .settings_overrides
                .iter()
                .any(|flag| flag == "--stream-radius")
        {
            self.grass = false;
            self.grass_disabled_by_terrain = true;
            let diagnostic = format!(
                "grass disabled for this session: explicit --stream-radius {} cannot cover grass (requires at least 2)",
                self.stream_radius
            );
            if !self.grass_settings_diagnostics.contains(&diagnostic) {
                self.grass_settings_diagnostics.push(diagnostic);
            }
        }
    }

    /// Saved-off grass must ignore generator imports just as an explicit off switch does.
    /// Numeric CLI fields retain their values and validation for this session.
    pub(crate) fn discard_inactive_grass_imports(&mut self) {
        if self.grass {
            return;
        }
        let Some(report) = &self.grass_settings_report else {
            return;
        };
        let imported = |keys: &[&str], flag: Option<&str>| {
            flag.is_none_or(|flag| !self.settings_overrides.iter().any(|value| value == flag))
                && keys
                    .iter()
                    .any(|key| report.settings[*key].effective.line.is_some())
        };
        let defaults = EngineConfig::default();
        if imported(&["iMinGrassSize"], Some("--grass-min-spacing")) {
            self.grass_min_spacing = defaults.grass_min_spacing;
        }
        if imported(&["fGrassStartFadeDistance"], Some("--grass-fade-start")) {
            self.grass_fade_start = defaults.grass_fade_start;
        }
        if imported(
            &["fGrassStartFadeDistance", "fGrassFadeRange"],
            Some("--grass-fade-end"),
        ) {
            self.grass_fade_end = defaults.grass_fade_end;
        }
        if imported(&["bEnableGrassFade"], None) {
            self.grass_enable_fade = defaults.grass_enable_fade;
        }
        if imported(&["fTexturePctThreshold"], None) {
            self.grass_texture_threshold = defaults.grass_texture_threshold;
        }
        if imported(&["iMaxGrassTypesPerTexure"], None) {
            self.grass_max_types_per_texture = defaults.grass_max_types_per_texture;
        }
    }

    /// Retain the shared INI report and apply its runtime values only to enabled grass.
    pub(crate) fn apply_grass_settings(&mut self, report: Report) {
        let defaults = GrassSettings::default();
        // Explicit INI assignments are launch overrides, just like numeric CLI values.
        if report.settings["bDrawShaderGrass"].effective.line.is_some() {
            self.settings_overrides.push("ini:grass".into());
        }
        self.grass &= report
            .boolean("bDrawShaderGrass")
            .unwrap_or(defaults.draw_shader_grass);
        if self.grass {
            self.map_grass_settings(&report);
        }
        self.grass_settings_diagnostics.push(
            "imported grass wind magnitude settings are reported only: grass wind is not rendered"
                .into(),
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
            let value = report.integer("iMinGrassSize").unwrap() as f32;
            if value >= 8.0 {
                self.grass_min_spacing = value;
            } else {
                self.grass_settings_diagnostics.push(
                    "iMinGrassSize below generator minimum 8 ignored; retaining supported spacing"
                        .into(),
                );
            }
        }
        self.map_grass_radius(report);
        if imported("iMaxGrassTypesPerTexure") {
            match usize::try_from(report.integer("iMaxGrassTypesPerTexure").unwrap()) {
                Ok(value) => self.grass_max_types_per_texture = value,
                Err(_) => self.grass_settings_diagnostics.push(
                    "negative iMaxGrassTypesPerTexure ignored; retaining supported type limit"
                        .into(),
                ),
            }
        }
        if imported("fTexturePctThreshold") {
            self.grass_texture_threshold = report.float("fTexturePctThreshold").unwrap();
        }
        if imported("bEnableGrassFade") {
            self.grass_enable_fade = report.boolean("bEnableGrassFade").unwrap();
        }
        if imported("fGrassStartFadeDistance") || imported("fGrassFadeRange") {
            let mut range = self.grass_fade_end - self.grass_fade_start;
            if imported("fGrassFadeRange") {
                let value = report.float("fGrassFadeRange").unwrap();
                if value.is_finite() && value >= 0.0 && (!self.grass_enable_fade || value > 0.0) {
                    range = value;
                } else {
                    self.grass_settings_diagnostics.push(
                        "unsupported fGrassFadeRange ignored; retaining the supported range".into(),
                    );
                }
            }
            let mut start = self.grass_fade_start;
            if imported("fGrassStartFadeDistance") {
                let value = report.float("fGrassStartFadeDistance").unwrap();
                if value.is_finite() && value >= 0.0 {
                    start = value;
                } else {
                    self.grass_settings_diagnostics.push(
                        "unsupported fGrassStartFadeDistance ignored; retaining the supported start".into(),
                    );
                }
            }
            let end = start + range;
            if start >= 0.0
                && start.is_finite()
                && range >= 0.0
                && end >= start
                && (!self.grass_enable_fade || end > start)
                && end.is_finite()
            {
                self.grass_fade_start = start;
                self.grass_fade_end = end;
            } else {
                self.grass_settings_diagnostics.push("unsupported fGrassStartFadeDistance/fGrassFadeRange ignored; retaining supported fade distances".into());
            }
        }
    }

    /// Ignore unsupported imported coverage while keeping explicit CLI validation strict.
    fn map_grass_radius(&mut self, report: &Report) {
        if report.settings["iGrassCellRadius"].effective.line.is_none() {
            return;
        }
        let radius = report.integer("iGrassCellRadius").unwrap();
        let explicit_terrain = self
            .settings_overrides
            .iter()
            .any(|flag| flag == "--stream-radius");
        let ceiling = if explicit_terrain {
            super::MAX_STREAM_RADIUS - 1
        } else {
            32
        };
        if (1..=ceiling).contains(&radius) {
            self.grass_radius = radius;
            if !self
                .settings_overrides
                .iter()
                .any(|flag| flag == "ini:grass-radius")
            {
                self.settings_overrides.push("ini:grass-radius".into());
            }
        } else {
            let diagnostic = format!(
                "unsupported iGrassCellRadius={radius} ignored; supported imported radius is 1..{ceiling}"
            );
            if !self.grass_settings_diagnostics.contains(&diagnostic) {
                self.grass_settings_diagnostics.push(diagnostic);
            }
        }
    }
}
