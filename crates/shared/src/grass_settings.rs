//! Typed grass defaults and provenance. The engine's shared Skyrim INI reader
//! supplies imported values; this module does not read files.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf};

pub const SETTINGS_VERSION: u32 = 1;

/// Grass values honoured by the engine, including reported-only Skyrim settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GrassSettings {
    /// `iMinGrassSize`: MudCrab default 20 Creation units for the candidate-count calculation.
    #[serde(rename = "iMinGrassSize")]
    pub min_grass_size: i32,
    /// `iMaxGrassTypesPerTexure`: MudCrab default 2 species per texture.
    #[serde(rename = "iMaxGrassTypesPerTexure")]
    pub max_grass_types_per_texture: i32,
    /// `fTexturePctThreshold`: MudCrab default 0.0 minimum texture contribution.
    #[serde(rename = "fTexturePctThreshold")]
    pub texture_pct_threshold: f64,
    /// `iGrassCellRadius`: MudCrab default 2 cells around the player.
    #[serde(rename = "iGrassCellRadius")]
    pub cell_radius: i32,
    /// `fGrassStartFadeDistance`: Shipped SkyrimPrefs.ini default 7000.0.
    #[serde(rename = "fGrassStartFadeDistance")]
    pub start_fade_distance: f64,
    /// `fGrassFadeRange`: MudCrab default 1000.0 Creation units.
    #[serde(rename = "fGrassFadeRange")]
    pub fade_range: f64,
    /// `fGrassMinStartFadeDistance`: Shipped SkyrimPrefs.ini default 0.0; reported only.
    #[serde(rename = "fGrassMinStartFadeDistance")]
    pub min_start_fade_distance: f64,
    /// `fGrassMaxStartFadeDistance`: Shipped SkyrimPrefs.ini default 7000.0; reported only.
    #[serde(rename = "fGrassMaxStartFadeDistance")]
    pub max_start_fade_distance: f64,
    /// `fGrassWindMagnitudeMin`: MudCrab default 5.0; reported only.
    #[serde(rename = "fGrassWindMagnitudeMin")]
    pub wind_magnitude_min: f64,
    /// `fGrassWindMagnitudeMax`: MudCrab default 125.0; reported only.
    #[serde(rename = "fGrassWindMagnitudeMax")]
    pub wind_magnitude_max: f64,
    /// `bDrawShaderGrass`: MudCrab default true; grass still requires --grass.
    #[serde(rename = "bDrawShaderGrass")]
    pub draw_shader_grass: bool,
    /// `bEnableGrassFade`: MudCrab default true.
    #[serde(rename = "bEnableGrassFade")]
    pub enable_grass_fade: bool,
    /// `bAllowCreateGrass`: MudCrab default true; reported legacy cache switch.
    #[serde(rename = "bAllowCreateGrass")]
    pub allow_create_grass: bool,
    /// `bAllowLoadGrass`: MudCrab default false; reported legacy cache switch.
    #[serde(rename = "bAllowLoadGrass")]
    pub allow_load_grass: bool,
    /// `bGenerateGrassDataFiles`: MudCrab default false; reported legacy cache switch.
    #[serde(rename = "bGenerateGrassDataFiles")]
    pub generate_grass_data_files: bool,
}

impl Default for GrassSettings {
    /// Use shipped SkyrimPrefs.ini fade-start values and documented MudCrab fallbacks.
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
        let defaults = serde_json::to_value(Self::default()).expect("grass defaults serialize");
        let values = serde_json::to_value(self).expect("grass values serialize");
        let settings = defaults
            .as_object()
            .expect("grass defaults are an object")
            .iter()
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
                "Fade-start defaults match shipped SkyrimPrefs.ini; other built-in values are MudCrab defaults.",
                "The shared --ini reader applies files in order; explicit numeric CLI options apply afterwards.",
                "Settings only: no grass placements, cache files or rendering have been generated.",
                "Legacy Skyrim GID creation/loading switches are reported but do not control MudCrab caching.",
                "Hashes cover effective values only; placement cache identity must also fingerprint terrain, grass definitions, obstructions and generator version.",
            ],
        };
        for (name, value) in values.as_object().expect("grass values are an object") {
            if value != &report.settings[name].default {
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
    hash(&serde_json::to_vec(&json!((SETTINGS_VERSION, settings))).expect("JSON values serialize"))
}
