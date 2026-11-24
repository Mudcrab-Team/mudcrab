//! Typed grass defaults and provenance. The engine's shared Skyrim INI reader
//! supplies imported values; this module does not read files.
//!
//! Every default is retail data (RF-GRASS-007). The field comments record
//! the default stored in each INI setting. The shipped INI files (`Skyrim_Default.ini`, `SkyrimPrefs.ini`)
//! omit most of these keys. A local `Skyrim.ini` with other values (`iMinGrassSize=40`,
//! `fGrassFadeRange=1766`, `bEnableGrassFade=0`) differs from the retail defaults; the retail
//! defaults are used.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf};

pub const SETTINGS_VERSION: u32 = 1;

/// Grass values honoured by the engine, including reported-only Skyrim settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GrassSettings {
    /// `iMinGrassSize`: Retail value (RF-GRASS-007): default 20, used directly as the candidate
    /// spacing divisor. Retail value (RF-GRASS-010): no lower clamp; MudCrab ignores values below 8
    /// as a safety guard.
    #[serde(rename = "iMinGrassSize")]
    pub min_grass_size: i32,
    /// `iMaxGrassTypesPerTexure`: Retail value (RF-GRASS-007, RF-GRASS-026): the collector checks
    /// the count already added before insertion, so the default 2 admits up to 3 records per
    /// texture list under the 16-per-tile cap.
    #[serde(rename = "iMaxGrassTypesPerTexure")]
    pub max_grass_types_per_texture: i32,
    /// `fTexturePctThreshold`: Retail value (RF-GRASS-016): default 0.0, the minimum blended
    /// texture weight. A value above 1 becomes 0.9 in retail; MudCrab rejects it at launch
    /// validation (`crates/engine/src/config.rs:981` accepts 0..1 only; tracked in #823).
    #[serde(rename = "fTexturePctThreshold")]
    pub texture_pct_threshold: f64,
    /// `iGrassCellRadius`: Retail value (RF-GRASS-007): 2 cells around the player.
    #[serde(rename = "iGrassCellRadius")]
    pub cell_radius: i32,
    /// `fGrassStartFadeDistance`: Retail value (RF-GRASS-007): compiled default 7000 (presets: Low 1000,
    /// Medium 2300, High and Ultra 7000; shipped `SkyrimPrefs.ini` 7000). Start of distance
    /// thinning.
    #[serde(rename = "fGrassStartFadeDistance")]
    pub start_fade_distance: f64,
    /// `fGrassFadeRange`: Retail value (RF-GRASS-007): 1000 Creation units. Retail's fade end is start plus range,
    /// clamped to uGridsToLoad x 4096, and the fade start used is min(fade end, start).
    #[serde(rename = "fGrassFadeRange")]
    pub fade_range: f64,
    /// `fGrassMinStartFadeDistance`: Retail value (RF-GRASS-007): shipped as 0.0 in `Skyrim_Default.ini` and `SkyrimPrefs.ini`
    /// (the compiled 400.0 is overridden); reported only.
    #[serde(rename = "fGrassMinStartFadeDistance")]
    pub min_start_fade_distance: f64,
    /// `fGrassMaxStartFadeDistance`: Retail value (RF-GRASS-007): 7000.0, also shipped in `Skyrim_Default.ini`; reported only.
    #[serde(rename = "fGrassMaxStartFadeDistance")]
    pub max_start_fade_distance: f64,
    /// `fGrassWindMagnitudeMin`: Retail value (RF-GRASS-007): 5.0; reported only.
    #[serde(rename = "fGrassWindMagnitudeMin")]
    pub wind_magnitude_min: f64,
    /// `fGrassWindMagnitudeMax`: Retail value (RF-GRASS-007): 125.0; reported only.
    #[serde(rename = "fGrassWindMagnitudeMax")]
    pub wind_magnitude_max: f64,
    /// `bDrawShaderGrass`: Retail value (RF-GRASS-008): true. Retail is on only when this is set
    /// and the start fade is at least the minimum start fade plus 0.5. MudCrab gates grass on this
    /// switch alone (`config/grass_ini.rs`), and reports the minimum start fade only. Launch-state
    /// resolution honors explicit CLI switches.
    #[serde(rename = "bDrawShaderGrass")]
    pub draw_shader_grass: bool,
    /// `bEnableGrassFade`: Retail value (RF-GRASS-007): true.
    #[serde(rename = "bEnableGrassFade")]
    pub enable_grass_fade: bool,
    /// `bAllowCreateGrass`: Retail value (RF-GRASS-007): true; reported legacy cache switch.
    #[serde(rename = "bAllowCreateGrass")]
    pub allow_create_grass: bool,
    /// `bAllowLoadGrass`: Retail value (RF-GRASS-007): false; reported legacy cache switch.
    #[serde(rename = "bAllowLoadGrass")]
    pub allow_load_grass: bool,
    /// `bGenerateGrassDataFiles`: Retail value (RF-GRASS-007): false; reported legacy cache switch.
    #[serde(rename = "bGenerateGrassDataFiles")]
    pub generate_grass_data_files: bool,
}

impl Default for GrassSettings {
    /// Use the retail defaults documented on each field.
    fn default() -> Self {
        Self {
            min_grass_size: 20,
            max_grass_types_per_texture: 2,
            texture_pct_threshold: 0.0,
            cell_radius: 2,
            start_fade_distance: 7000.0,
            fade_range: 1000.0,
            min_start_fade_distance: 0.0,
            max_start_fade_distance: 7000.0,
            wind_magnitude_min: 5.0,
            wind_magnitude_max: 125.0,
            draw_shader_grass: true,
            enable_grass_fade: true,
            allow_create_grass: true,
            allow_load_grass: false,
            generate_grass_data_files: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Placement,
    Runtime,
    LegacyCache,
}

#[derive(Debug, Clone, Serialize)]
pub struct Assignment {
    pub value: Value,
    pub source: String,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Setting {
    pub category: Category,
    pub default: Value,
    pub effective: Assignment,
    /// Default followed by assignments supplied by the shared INI reader.
    pub history: Vec<Assignment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Source {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub source: String,
    pub line: usize,
    pub key: String,
    pub value: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub default_profile: &'static str,
    pub settings: BTreeMap<String, Setting>,
    pub placement: BTreeMap<String, Value>,
    pub runtime: BTreeMap<String, Value>,
    pub legacy_cache: BTreeMap<String, Value>,
    /// These are settings-only hashes, not complete terrain/asset cache fingerprints.
    pub placement_settings_hash: String,
    pub runtime_settings_hash: String,
    pub sources: Vec<Source>,
    pub diagnostics: Vec<Diagnostic>,
    pub has_unhandled_settings: bool,
    pub notes: Vec<&'static str>,
}

impl Report {
    /// Effective assignment value by Skyrim's case-insensitive setting name.
    pub fn effective_value(&self, name: &str) -> Option<&Value> {
        self.settings
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, setting)| &setting.effective.value)
    }

    /// An effective integer setting, or `None` for an unknown name or other scalar type.
    pub fn integer(&self, name: &str) -> Option<i32> {
        i32::try_from(self.effective_value(name)?.as_i64()?).ok()
    }

    /// An effective numeric setting as a finite `f32`.
    pub fn float(&self, name: &str) -> Option<f32> {
        let value = self.effective_value(name)?.as_f64()?;
        (value.is_finite() && value.abs() <= f32::MAX as f64).then_some(value as f32)
    }

    /// An effective boolean setting, or `None` for an unknown name or other scalar type.
    pub fn boolean(&self, name: &str) -> Option<bool> {
        self.effective_value(name)?.as_bool()
    }
}

impl Source {
    /// Record bytes already read by the shared INI reader without opening the file again.
    pub fn from_bytes(path: PathBuf, bytes: &[u8]) -> Self {
        Self {
            path,
            sha256: hash(bytes),
        }
    }
}

impl GrassSettings {
    /// Describe these values against built-in defaults, before explicit CLI overrides.
    pub fn report(&self) -> Report {
        let defaults = serde_json::to_value(Self::default()).ok();
        let values = serde_json::to_value(self).ok();
        let settings = defaults
            .as_ref()
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(name, default)| {
                let assignment = Assignment {
                    value: default.clone(),
                    source: "default".into(),
                    line: None,
                };
                (
                    name.clone(),
                    Setting {
                        category: match name.as_str() {
                            "iMinGrassSize"
                            | "iMaxGrassTypesPerTexure"
                            | "fTexturePctThreshold" => Category::Placement,
                            "bAllowCreateGrass" | "bAllowLoadGrass" | "bGenerateGrassDataFiles" => {
                                Category::LegacyCache
                            }
                            _ => Category::Runtime,
                        },
                        default: default.clone(),
                        effective: assignment.clone(),
                        history: vec![assignment],
                    },
                )
            })
            .collect();
        let mut report = Report {
            schema_version: SETTINGS_VERSION,
            default_profile: "default",
            settings,
            placement: BTreeMap::new(),
            runtime: BTreeMap::new(),
            legacy_cache: BTreeMap::new(),
            placement_settings_hash: String::new(),
            runtime_settings_hash: String::new(),
            sources: Vec::new(),
            diagnostics: Vec::new(),
            has_unhandled_settings: false,
            notes: vec![
                "Built-in values are the retail defaults (fGrassMinStartFadeDistance: shipped INI).",
                "The shared --ini reader applies files in order; explicit numeric CLI options apply afterwards.",
                "Settings only: no grass placements, cache files or rendering have been generated.",
                "Legacy Skyrim GID creation/loading switches are reported but do not control MudCrab caching.",
                "Hashes cover effective values only; placement cache identity must also fingerprint terrain, grass definitions, obstructions and generator version.",
            ],
        };
        for (name, value) in values
            .as_ref()
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if report
                .settings
                .get(name)
                .is_some_and(|setting| *value != setting.default)
            {
                report.assign(name, value.clone(), "default".into(), None);
            }
        }
        report.refresh_hashes();
        report
    }
}

impl Report {
    /// Record an already validated assignment. Returns whether the key is known;
    /// scalar validation belongs to the shared INI reader.
    pub fn assign(
        &mut self,
        name: &str,
        value: Value,
        source: String,
        line: Option<usize>,
    ) -> bool {
        let Some((_, setting)) = self
            .settings
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        else {
            return false;
        };
        let assignment = Assignment {
            value,
            source,
            line,
        };
        setting.effective = assignment.clone();
        setting.history.push(assignment);
        self.refresh_hashes();
        true
    }

    /// Rebuild deterministic category maps and settings-only hashes from effective values.
    fn refresh_hashes(&mut self) {
        self.placement.clear();
        self.runtime.clear();
        self.legacy_cache.clear();
        for (name, setting) in &self.settings {
            let category = match setting.category {
                Category::Placement => &mut self.placement,
                Category::Runtime => &mut self.runtime,
                Category::LegacyCache => &mut self.legacy_cache,
            };
            category.insert(name.clone(), setting.effective.value.clone());
        }
        self.placement_settings_hash = settings_hash(&self.placement);
        self.runtime_settings_hash = settings_hash(&self.runtime);
    }
}

/// Encode the SHA-256 digest of input bytes as lowercase hexadecimal.
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Hash the settings schema version and deterministically ordered effective values.
fn settings_hash(settings: &BTreeMap<String, Value>) -> String {
    hash(json!((SETTINGS_VERSION, settings)).to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults equal the retail compiled values,
    /// not the values of a local `Skyrim.ini` (40, 1766 and 0 for the first three), which differs
    /// from the retail defaults.
    #[test]
    fn defaults_equal_the_retail_compiled_values() {
        let d = GrassSettings::default();
        assert_eq!(d.min_grass_size, 20, "iMinGrassSize");
        assert_eq!(d.fade_range, 1000.0, "fGrassFadeRange");
        assert!(d.enable_grass_fade, "bEnableGrassFade");
        assert_eq!(d.max_grass_types_per_texture, 2, "iMaxGrassTypesPerTexure");
        assert_eq!(d.texture_pct_threshold, 0.0, "fTexturePctThreshold");
        assert_eq!(d.cell_radius, 2, "iGrassCellRadius");
        assert_eq!(d.start_fade_distance, 7000.0, "fGrassStartFadeDistance");
        assert_eq!(
            d.max_start_fade_distance, 7000.0,
            "fGrassMaxStartFadeDistance"
        );
        assert_eq!(d.min_start_fade_distance, 0.0, "shipped INI value");
        assert_eq!((d.wind_magnitude_min, d.wind_magnitude_max), (5.0, 125.0));
        assert!(d.draw_shader_grass && d.allow_create_grass);
        assert!(!d.allow_load_grass && !d.generate_grass_data_files);
    }
}
