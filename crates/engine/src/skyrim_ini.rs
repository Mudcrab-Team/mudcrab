//! Skyrim INI settings the engine honours.
//!
//! Skyrim reads its view distances from `Skyrim.ini` and `SkyrimPrefs.ini`, and
//! mod managers, LOD generators and players tune the same keys. `--ini <path>`
//! reads files in that format so a load order's tuned values carry over: files
//! apply in the order given, later files override earlier ones, and explicit
//! command-line options override every file. Keys without an engine consumer
//! are ignored, as Skyrim ignores keys it does not know.
//!
//! Honoured keys:
//!
//! | Section | Key | Engine setting |
//! |---|---|---|
//! | `[General]` | `bAlwaysActive` | `0` (default) pauses on focus loss; `1` keeps simulation active |
//! | `[Sky]` | `fSunXExtreme`, `fSunYExtreme`, `fSunZExtreme`, `fSunDirXExtreme`, `fSunAlphaTransTime` | explicit sun-path overrides over the pack's GMST (retail exe default for a missing GMST) |
//! | `[General]` | `uGridsToLoad` | full-detail grid: `stream_radius = (uGridsToLoad - 1) / 2` |
//! | `[Display]` | `fShadowDistance` | upper bound for sun shadow cascades, in Creation units; the default is Ultra's 10000 |
//! | `[Display]` | `iShadowMapResolution` | sun shadow map edge: 512, 1024, 2048 or 4096; the default is Ultra's 4096 |
//! | `[Display]` | `iNumSplits` | sun shadow cascade count, 1 to 4; the default is retail's compiled 2 |
//! | `[Display]` | `fFirstSliceDistance` | first slice input of the retail cascade split, in Creation units; the default is the template's 2000 |
//! | `[Display]` | `bDrawLandShadows` | 1 makes terrain cast sun shadows; the default 0 matches [Skyrim](https://github.com/Mudcrab-Team/skyrim_decomp/blob/main/findings/sun-shadows-retail-facts.md#4-object-and-terrain-lod). In retail it affects near land only; here it also affects LOD land until a later change limits it |
//! | `[TerrainManager]` | `fSplitDistanceMult` | terrain LOD split rule: a block of `level` cells splits into its four children under `fSplitDistanceMult` x `level` x 2,048 x sqrt(2) units (34,756 / 69,512 at levels 8 / 16 with 1.5) |
//! | `[TerrainManager]` | `fBlockMaximumDistance` | outer reach of tier 16, times `fSplitDistanceMult`, until a level-32 tier exists (not a retail terrain cut) |
//! | `[TerrainManager]` | `fBlockLevel0Distance` | parsed and kept for the object LOD rule (level 8 hands over to level 4); does not affect terrain, so it changes nothing on screen until object LOD lands |
//! | `[TerrainManager]` | `fBlockLevel1Distance` | parsed and kept for the object LOD rule (level 16 hands over to level 8); does not affect terrain, so it changes nothing on screen until object LOD lands |
//! | `[Grass]` | `iMinGrassSize` | candidate-count spacing |
//! | `[Grass]` | `iMaxGrassTypesPerTexure` | species cap per texture |
//! | `[Grass]` | `fTexturePctThreshold` | minimum blended texture weight |
//! | `[Grass]` | `iGrassCellRadius` | generated cell radius |
//! | `[Grass]` | `fGrassStartFadeDistance` | fade start |
//! | `[Grass]` | `fGrassFadeRange` | fade end minus fade start |
//! | `[Grass]` | `fGrassMinStartFadeDistance` | reported slider bound |
//! | `[Grass]` | `fGrassMaxStartFadeDistance` | reported slider bound |
//! | `[Grass]` | `fGrassWindMagnitudeMin` | reported wind setting |
//! | `[Grass]` | `fGrassWindMagnitudeMax` | reported wind setting |
//! | `[Grass]` | `bDrawShaderGrass` | launch grass state unless a CLI switch overrides it |
//! | `[Grass]` | `bEnableGrassFade` | distance thinning |
//! | `[Grass]` | `bAllowCreateGrass` | reported legacy cache setting |
//! | `[Grass]` | `bAllowLoadGrass` | reported legacy cache setting |
//! | `[Grass]` | `bGenerateGrassDataFiles` | reported legacy cache setting |

use crate::config::{EngineConfig, MAX_STREAM_RADIUS};
use serde_json::{Value, json};
use shared::grass_settings::{GrassSettings, Source};
use std::{collections::HashMap, path::Path};

/// The shortest sun shadow range the engine accepts (700 Creation units, 10 metres). It is a
/// Mudcrab-only floor, to keep every cascade bound above the cascade near distance.
const MIN_SUN_SHADOW_DISTANCE: f32 = 700.0;
/// Raw merged value and provenance from the shared INI reader.
#[derive(Debug)]
struct IniValue {
    value: String,
    source: String,
    line: usize,
}

/// `0` or `1`.
fn parse_flag(raw: &str) -> Option<bool> {
    match raw {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// An odd whole number of grids that the streamer accepts.
fn parse_grids(raw: &str) -> Option<i32> {
    raw.parse::<i32>()
        .ok() // silent-ok: The INI merge reports invalid values before inserting them.
        // Skyrim centres the grid on the player's cell, so only odd sizes are a grid.
        .filter(|grids| *grids >= 1 && grids % 2 == 1 && (grids - 1) / 2 <= MAX_STREAM_RADIUS)
}

/// A shadow distance the cascade builder accepts.
fn parse_shadow_distance(raw: &str) -> Option<f32> {
    raw.parse::<f32>()
        .ok() // silent-ok: The INI merge reports invalid values before inserting them.
        .filter(|value| value.is_finite() && *value >= MIN_SUN_SHADOW_DISTANCE)
}

/// A sun shadow map edge the settings menu offers.
fn parse_shadow_map_size(raw: &str) -> Option<usize> {
    raw.parse::<usize>()
        .ok() // silent-ok: The INI merge reports invalid values before inserting them.
        .filter(|size| crate::settings::SHADOW_SIZES.contains(size))
}

/// A sun shadow cascade count Bevy supports for one light.
fn parse_cascade_count(raw: &str) -> Option<usize> {
    raw.parse::<usize>()
        .ok() // silent-ok: The INI merge reports invalid values before inserting them.
        .filter(|count| (1..=4).contains(count))
}

/// A finite number above zero.
fn parse_positive(raw: &str) -> Option<f32> {
    raw.parse::<f32>()
        .ok() // silent-ok: The INI merge reports invalid values before inserting them.
        .filter(|value| value.is_finite() && *value > 0.0)
}

/// Whether a value of a key `apply` validates is usable: `Some(true)` or `Some(false)`, or `None`
/// for a key `apply` does not validate. `section` and `key` are lower case. The same functions
/// that `apply` uses decide it, so the two cannot disagree.
fn is_usable(section: &str, key: &str, raw: &str) -> Option<bool> {
    match (section, key) {
        ("general", "balwaysactive") | ("display", "bdrawlandshadows") => {
            Some(parse_flag(raw).is_some())
        }
        ("general", "ugridstoload") => Some(parse_grids(raw).is_some()),
        ("display", "fshadowdistance") => Some(parse_shadow_distance(raw).is_some()),
        ("display", "ishadowmapresolution") => Some(parse_shadow_map_size(raw).is_some()),
        ("display", "inumsplits") => Some(parse_cascade_count(raw).is_some()),
        ("display", "ffirstslicedistance") => Some(parse_positive(raw).is_some()),
        ("terrainmanager", "fblocklevel0distance")
        | ("terrainmanager", "fblocklevel1distance")
        | ("terrainmanager", "fblockmaximumdistance")
        | ("terrainmanager", "fsplitdistancemult") => Some(parse_positive(raw).is_some()),
        ("sky", _) => crate::sun::SUN_KEYS
            .iter()
            .position(|name| name.eq_ignore_ascii_case(key))
            .map(|index| {
                raw.parse::<f32>()
                    .is_ok_and(|value| crate::sun::SunSettings::valid(index, value))
            }),
        _ => None,
    }
}

/// Values from one or more INI files, keyed by lower-cased section and key.
#[derive(Debug, Default)]
pub struct SkyrimIni {
    values: HashMap<(String, String), IniValue>,
    sources: Vec<Source>,
}

impl SkyrimIni {
    /// Reads `path` over the values already read, as Skyrim layers its INI files.
    pub fn merge_file(&mut self, path: &Path) -> std::io::Result<()> {
        let text = std::fs::read_to_string(path)?;
        self.merge_named_str(&text, &path.display().to_string());
        self.sources
            .push(Source::from_bytes(path.to_owned(), text.as_bytes()));
        Ok(())
    }

    /// Reads INI text over the values already read. Sections and keys are
    /// case-insensitive; `;` and `#` start a comment line; a key outside any
    /// section and a line without `=` are skipped.
    pub fn merge_str(&mut self, text: &str) {
        self.merge_named_str(text, "<INI text>");
    }

    /// Merge already read text while retaining its source for supported grass keys.
    fn merge_named_str(&mut self, text: &str, source: &str) {
        let mut section = None;
        for (index, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
            {
                section = Some(name.trim().to_ascii_lowercase());
                continue;
            }
            let (Some(section), Some((key, value))) = (&section, line.split_once('=')) else {
                continue;
            };
            let slot = (section.clone(), key.trim().to_ascii_lowercase());
            let value = value.trim();
            if is_usable(&slot.0, &slot.1, value) == Some(false) {
                let outcome = if self.values.contains_key(&slot) {
                    "keeping the earlier usable value"
                } else {
                    "leaving the setting unchanged"
                };
                eprintln!(
                    "warning: {source}:{}: ignoring {}={value:?}; {outcome}",
                    index + 1,
                    slot.1
                );
                continue;
            }
            self.values.insert(
                slot,
                IniValue {
                    value: value.to_owned(),
                    source: source.to_owned(),
                    line: index + 1,
                },
            );
        }
    }

    /// Retrieve a merged scalar without changing the shared reader's parsing rules.
    fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.values
            .get(&(section.to_ascii_lowercase(), key.to_ascii_lowercase()))
            .map(|entry| entry.value.as_str())
    }

    /// Apply terrain keys and retain the grass report. Grass runtime values apply
    /// only while grass is enabled and its imported draw flag permits it. Malformed
    /// values are reported and leave the corresponding configuration fields unchanged.
    pub fn apply(&self, config: &mut EngineConfig) {
        if let Some(raw) = self.get("General", "bAlwaysActive") {
            match parse_flag(raw) {
                Some(value) => config.always_active = value,
                None => eprintln!("warning: ignoring bAlwaysActive={raw:?}; expected 0 or 1"),
            }
        }
        if let Some(raw) = self.get("General", "uGridsToLoad") {
            match parse_grids(raw) {
                Some(grids) => {
                    config.stream_radius = (grids - 1) / 2;
                    config.unload_radius = config.stream_radius + 1;
                    config.settings_overrides.push("ini:stream-radius".into());
                }
                None => eprintln!(
                    "warning: ignoring uGridsToLoad={raw:?}; expected an odd whole number from 1 to {}",
                    MAX_STREAM_RADIUS * 2 + 1
                ),
            }
        }
        if let Some(raw) = self.get("Display", "fShadowDistance") {
            match parse_shadow_distance(raw) {
                Some(value) => config.sun_shadow_distance_limit = Some(value),
                None => eprintln!(
                    "warning: ignoring fShadowDistance={raw:?}; expected at least {MIN_SUN_SHADOW_DISTANCE} Creation units"
                ),
            }
        }
        if let Some(raw) = self.get("Display", "iShadowMapResolution") {
            match parse_shadow_map_size(raw) {
                Some(value) => {
                    config.sun_shadow_map_size = value;
                    config
                        .settings_overrides
                        .push("ini:iShadowMapResolution".into());
                }
                None => eprintln!(
                    "warning: ignoring iShadowMapResolution={raw:?}; expected 512, 1024, 2048 or 4096"
                ),
            }
        }
        if let Some(raw) = self.get("Display", "iNumSplits") {
            match parse_cascade_count(raw) {
                Some(value) => {
                    config.sun_shadow_cascades = value;
                    config.settings_overrides.push("ini:iNumSplits".into());
                }
                None => eprintln!("warning: ignoring iNumSplits={raw:?}; expected 1 to 4"),
            }
        }
        if let Some(raw) = self.get("Display", "fFirstSliceDistance") {
            match parse_positive(raw) {
                Some(value) => config.sun_first_slice_distance = value,
                None => eprintln!(
                    "warning: ignoring fFirstSliceDistance={raw:?}; expected a positive number"
                ),
            }
        }
        if let Some(raw) = self.get("Display", "bDrawLandShadows") {
            match parse_flag(raw) {
                Some(value) => config.land_shadows = value,
                None => eprintln!("warning: ignoring bDrawLandShadows={raw:?}; expected 0 or 1"),
            }
        }
        for (index, key) in crate::sun::SUN_KEYS.into_iter().enumerate() {
            let Some(raw) = self.get("Sky", key) else {
                continue;
            };
            match raw.parse::<f32>() {
                Ok(value) if crate::sun::SunSettings::valid(index, value) => {
                    config.sun_settings_overrides[index] = Some(value)
                }
                _ => eprintln!(
                    "warning: ignoring [Sky] {key}={raw:?}; unusable sun-path value, retaining the pack's GMST"
                ),
            }
        }
        let lod = &mut config.terrain_lod;
        for (key, setting) in [
            ("fBlockLevel0Distance", &mut lod.block_level0_distance),
            ("fBlockLevel1Distance", &mut lod.block_level1_distance),
            ("fBlockMaximumDistance", &mut lod.block_maximum_distance),
            ("fSplitDistanceMult", &mut lod.split_distance_mult),
        ] {
            let Some(raw) = self.get("TerrainManager", key) else {
                continue;
            };
            match parse_positive(raw) {
                Some(value) => {
                    *setting = value;
                    config.settings_overrides.push(format!("ini:{key}"));
                }
                None => eprintln!("warning: ignoring {key}={raw:?}; expected a positive number"),
            }
        }
        self.apply_grass(config);
    }

    /// Apply supported grass scalars from the same merged values used for terrain.
    fn apply_grass(&self, config: &mut EngineConfig) {
        if !self.values.keys().any(|(section, _)| section == "grass") {
            return;
        }
        let mut report = GrassSettings::default().report();
        let keys: Vec<_> = report.settings.keys().cloned().collect();
        let mut found = false;
        for key in keys {
            let Some(entry) = self.values.get(&("grass".into(), key.to_ascii_lowercase())) else {
                continue;
            };
            found = true;
            match parse_grass_value(&key, &report.settings[&key].default, &entry.value) {
                Ok(value) => {
                    report.assign(&key, value, entry.source.clone(), Some(entry.line));
                }
                Err(reason) => {
                    eprintln!("warning: ignoring {key}={:?}; {reason}", entry.value);
                }
            }
        }
        if found {
            report.sources = self.sources.clone();
            config.apply_grass_settings(report);
        }
    }
}

/// Validate a supported grass scalar; malformed values follow the shared warn-and-ignore policy.
fn parse_grass_value(key: &str, default: &Value, raw: &str) -> Result<Value, &'static str> {
    if default.is_boolean() {
        return match raw.to_ascii_lowercase().as_str() {
            "1" | "true" => Ok(json!(true)),
            "0" | "false" => Ok(json!(false)),
            _ => Err("expected 0, 1, true or false"),
        };
    }
    if default.is_i64() {
        let value = raw
            .parse::<i32>()
            .map_err(|_| "expected a 32-bit integer")?;
        if key == "iMinGrassSize" && value < 0 {
            return Err("negative iMinGrassSize is unsupported");
        }
        return Ok(json!(value));
    }
    let value = raw.parse::<f64>().map_err(|_| "expected a finite number")?;
    if !value.is_finite() || value < 0.0 || value > f32::MAX as f64 {
        return Err("expected a finite, nonnegative f32-range number");
    }
    if key == "fTexturePctThreshold" && value > 1.0 {
        return Err("texture threshold must be between 0 and 1");
    }
    Ok(json!(if value == 0.0 { 0.0 } else { value }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::lod::LodTier;

    fn apply(text: &str) -> EngineConfig {
        let mut ini = SkyrimIni::default();
        ini.merge_str(text);
        let mut config = EngineConfig::default();
        ini.apply(&mut config);
        config
    }

    /// Explicit values layer by key; bad values cannot poison authored settings.
    #[test]
    fn sun_ini_overrides_validate_and_layer_without_aborting() {
        let mut ini = SkyrimIni::default();
        ini.merge_str("[sKy]\nfSunXExtreme=-300\nfSunYExtreme=-70\nfSunZExtreme=-120\nfSunDirXExtreme=0\nfSunAlphaTransTime=1.5\n");
        ini.merge_str("[Sky]\nfSunYExtreme=80\n");
        let mut config = EngineConfig::default();
        ini.apply(&mut config);
        assert_eq!(
            config.sun_settings_overrides,
            [Some(-300.0), Some(80.0), Some(-120.0), Some(0.0), Some(1.5)]
        );
        for (key, value) in [
            ("fSunXExtreme", "0"),
            ("fSunYExtreme", "NaN"),
            ("fSunZExtreme", "inf"),
            ("fSunDirXExtreme", "bad"),
            ("fSunAlphaTransTime", "24"),
        ] {
            let config = apply(&format!("[Sky]\n{key}={value}\n"));
            assert_eq!(config.sun_settings_overrides, [None; 5]);
        }
    }

    #[test]
    fn ini_v88_preserves_the_cli_stream_radius_bound() {
        let widest = apply(&format!(
            "[General]\nuGridsToLoad={}\n",
            MAX_STREAM_RADIUS * 2 + 1
        ));
        assert_eq!(widest.stream_radius, MAX_STREAM_RADIUS);
        let overflowing = apply("[General]\nuGridsToLoad=2147483647\n");
        assert_eq!(
            overflowing.stream_radius,
            EngineConfig::default().stream_radius
        );
    }

    #[test]
    fn reads_skyrim_prefs_keys_case_insensitively_with_a_bom() {
        let config = apply(
            "\u{feff}[General]\nuGridsToLoad=7\n; comment\n[DISPLAY]\nfShadowDistance=8000.0000\n[TERRAINMANAGER]\n\
             fblocklevel0distance=40000.0000\nfBlockLevel1Distance = 80000.0000\n\
             fBlockMaximumDistance=300000.0000\nfSplitDistanceMult=2.0000\n",
        );
        assert_eq!((config.stream_radius, config.unload_radius), (3, 4));
        assert_eq!(config.sun_shadow_distance_limit, Some(8_000.0));
        let lod = config.terrain_lod;
        assert_eq!(
            (
                lod.block_level0_distance,
                lod.block_level1_distance,
                lod.block_maximum_distance,
                lod.split_distance_mult
            ),
            (40_000.0, 80_000.0, 300_000.0, 2.0)
        );
        // Query radius: tier 4 = ceil(2 * 8 * 2048 * sqrt(2) / 4096) + 8 = 12 + 8; tier 8 =
        // ceil(2 * 16 * 2048 * sqrt(2) / 4096) + 16 = 23 + 16; tier 16 = the outer reach,
        // 300000 * 2 / 4096 = 146.5 cells.
        assert_eq!(
            LodTier::ALL.map(|tier| lod.reach_cells(tier)),
            [20, 39, 146]
        );
    }

    #[test]
    fn later_files_override_earlier_ones_key_by_key() {
        let mut ini = SkyrimIni::default();
        ini.merge_str("[TerrainManager]\nfBlockLevel0Distance=20000\nfSplitDistanceMult=2\n");
        ini.merge_str("[TerrainManager]\nfBlockLevel0Distance=30000\n");
        let mut config = EngineConfig::default();
        ini.apply(&mut config);
        assert_eq!(config.terrain_lod.block_level0_distance, 30_000.0);
        assert_eq!(config.terrain_lod.split_distance_mult, 2.0);
    }

    #[test]
    fn malformed_values_and_keys_outside_their_section_change_nothing() {
        let defaults = EngineConfig::default();
        for text in [
            "[General]\nuGridsToLoad=4\n",
            "[General]\nuGridsToLoad=0\n",
            "[General]\nuGridsToLoad=five\n",
            "[TerrainManager]\nfSplitDistanceMult=0\n",
            "[TerrainManager]\nfBlockLevel0Distance=-1\n",
            "[TerrainManager]\nfBlockMaximumDistance=inf\n",
            "uGridsToLoad=9\nfSplitDistanceMult=3\n",
            "[Display]\nfSplitDistanceMult=3\n",
        ] {
            let config = apply(text);
            assert_eq!(config.stream_radius, defaults.stream_radius, "{text}");
            assert_eq!(config.terrain_lod, defaults.terrain_lod, "{text}");
        }
    }

    /// Layered files: a later unusable value must not replace an earlier usable one, and a later
    /// usable value (even 0) still overrides it.
    #[test]
    fn a_later_unusable_value_keeps_the_earlier_usable_one() {
        let layered = |first: &str, second: &str| {
            let mut ini = SkyrimIni::default();
            ini.merge_str(first);
            ini.merge_str(second);
            let mut config = EngineConfig::default();
            ini.apply(&mut config);
            config
        };
        // bDrawLandShadows: an invalid 2 keeps the 1; a valid 0 overrides it.
        let kept = layered(
            "[Display]\nbDrawLandShadows=1\n",
            "[Display]\nbDrawLandShadows=2\n",
        );
        assert!(kept.land_shadows);
        let overridden = layered(
            "[Display]\nbDrawLandShadows=1\n",
            "[Display]\nbDrawLandShadows=0\n",
        );
        assert!(!overridden.land_shadows);
        // fShadowDistance (below the cascade minimum and malformed keep 9000; 12000 overrides).
        for bad in ["0", "699", "far", "NaN"] {
            let config = layered(
                "[Display]\nfShadowDistance=9000\n",
                &format!("[Display]\nfShadowDistance={bad}\n"),
            );
            assert_eq!(config.sun_shadow_distance_limit, Some(9_000.0), "{bad}");
        }
        let config = layered(
            "[Display]\nfShadowDistance=9000\n",
            "[Display]\nfShadowDistance=12000\n",
        );
        assert_eq!(config.sun_shadow_distance_limit, Some(12_000.0));
        // uGridsToLoad: an even grid count keeps 7 (radius 3); 9 overrides.
        let config = layered("[General]\nuGridsToLoad=7\n", "[General]\nuGridsToLoad=6\n");
        assert_eq!(config.stream_radius, 3);
        let config = layered("[General]\nuGridsToLoad=7\n", "[General]\nuGridsToLoad=9\n");
        assert_eq!(config.stream_radius, 4);
        // bAlwaysActive.
        assert!(
            layered(
                "[General]\nbAlwaysActive=1\n",
                "[General]\nbAlwaysActive=x\n"
            )
            .always_active
        );
        assert!(
            !layered(
                "[General]\nbAlwaysActive=1\n",
                "[General]\nbAlwaysActive=0\n"
            )
            .always_active
        );
        // A TerrainManager distance: a negative later value keeps 1.75; 2.0 overrides.
        let config = layered(
            "[TerrainManager]\nfSplitDistanceMult=1.75\n",
            "[TerrainManager]\nfSplitDistanceMult=-1\n",
        );
        assert_eq!(config.terrain_lod.split_distance_mult, 1.75);
        let config = layered(
            "[TerrainManager]\nfSplitDistanceMult=1.75\n",
            "[TerrainManager]\nfSplitDistanceMult=2.0\n",
        );
        assert_eq!(config.terrain_lod.split_distance_mult, 2.0);
        // A Sky key: an unusable later value keeps the earlier one.
        let key = crate::sun::SUN_KEYS[4];
        let config = layered(&format!("[Sky]\n{key}=12\n"), &format!("[Sky]\n{key}=99\n"));
        assert_eq!(config.sun_settings_overrides[4], Some(12.0));
        // With no earlier value, an unusable one changes nothing.
        let config = layered("", "[Display]\nbDrawLandShadows=2\n");
        assert!(!config.land_shadows);
    }

    /// A duplicate key inside one file behaves like two layered files: a later unusable line keeps
    /// the earlier usable value, a later usable one overrides it.
    #[test]
    fn a_duplicate_key_in_one_file_keeps_the_last_usable_value() {
        assert!(apply("[Display]\nbDrawLandShadows=1\nbDrawLandShadows=2\n").land_shadows);
        assert!(!apply("[Display]\nbDrawLandShadows=1\nbDrawLandShadows=0\n").land_shadows);
        assert!(apply("[Display]\nbDrawLandShadows=0\nbDrawLandShadows=1\n").land_shadows);
    }

    /// Terrain casts no sun shadow by default (retail `bDrawLandShadows` = 0);
    /// the ini can turn it on, and a bad value changes nothing.
    #[test]
    fn draw_land_shadows_reads_zero_or_one() {
        assert!(!apply("").land_shadows);
        assert!(apply("[Display]\nbDrawLandShadows=1\n").land_shadows);
        assert!(!apply("[Display]\nbDrawLandShadows=0\n").land_shadows);
        assert!(!apply("[Display]\nbDrawLandShadows=yes\n").land_shadows);
        assert!(!apply("[Display]\nbDrawLandShadows=2\n").land_shadows);
    }

    #[test]
    fn malformed_or_unusable_shadow_distances_leave_the_default() {
        for value in ["0", "699", "NaN", "inf", "far"] {
            let config = apply(&format!("[Display]\nfShadowDistance={value}\n"));
            assert_eq!(
                config.sun_shadow_distance_limit,
                EngineConfig::default().sun_shadow_distance_limit,
                "{value}"
            );
        }
    }

    /// With no INI the sun shadow settings are Mudcrab's defaults, each equal to the value its
    /// source gives (#949, #857): `Ultra.ini` for the map size and the distance, `Skyrim_Default.ini`
    /// for the first slice, and the compiled default for the count.
    #[test]
    fn sun_shadow_defaults_are_the_cited_retail_values_without_an_ini() {
        let config = apply("");
        assert_eq!(
            config.sun_shadow_map_size, 4096,
            "Ultra.ini iShadowMapResolution"
        );
        assert_eq!(
            config.sun_shadow_distance_limit,
            Some(10_000.0),
            "Ultra.ini fShadowDistance"
        );
        assert_eq!(config.sun_shadow_cascades, 2, "compiled iNumSplits");
        assert_eq!(
            config.sun_first_slice_distance, 2_000.0,
            "Skyrim_Default.ini fFirstSliceDistance (the compiled 1250 loses)"
        );
        assert!(
            !config
                .settings_overrides
                .iter()
                .any(|flag| flag.starts_with("ini:i")),
            "a default is not a launch override"
        );
    }

    /// An INI value replaces each default, and a malformed value keeps the earlier one.
    #[test]
    fn ini_values_override_the_sun_shadow_defaults() {
        let config = apply(
            "[Display]\niShadowMapResolution=1024\niNumSplits=3\nfFirstSliceDistance=1250\n\
             fShadowDistance=8000\n",
        );
        assert_eq!(config.sun_shadow_map_size, 1024);
        assert_eq!(config.sun_shadow_cascades, 3);
        assert_eq!(config.sun_first_slice_distance, 1_250.0);
        assert_eq!(config.sun_shadow_distance_limit, Some(8_000.0));
        assert!(
            config
                .settings_overrides
                .contains(&"ini:iShadowMapResolution".to_owned())
        );
        assert!(
            config
                .settings_overrides
                .contains(&"ini:iNumSplits".to_owned())
        );

        for (key, bad) in [
            ("iShadowMapResolution", "3000"),
            ("iShadowMapResolution", "0"),
            ("iNumSplits", "0"),
            ("iNumSplits", "5"),
            ("fFirstSliceDistance", "0"),
            ("fFirstSliceDistance", "NaN"),
        ] {
            let config = apply(&format!("[Display]\n{key}={bad}\n"));
            let defaults = EngineConfig::default();
            assert_eq!(
                config.sun_shadow_map_size, defaults.sun_shadow_map_size,
                "{key}={bad}"
            );
            assert_eq!(
                config.sun_shadow_cascades, defaults.sun_shadow_cascades,
                "{key}={bad}"
            );
            assert_eq!(
                config.sun_first_slice_distance, defaults.sun_first_slice_distance,
                "{key}={bad}"
            );
        }
    }
}
