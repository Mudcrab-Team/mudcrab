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
//! | `[Display]` | `fDefaultWorldFOV` | horizontal field of view of every camera but the first-person one, in degrees (retail 80; accepted 30 to 140, a Mudcrab-only bound); converted to Bevy's vertical angle at the window's aspect ratio |
//! | `[Display]` | `fDefault1stPersonFOV` | horizontal field of view of the first-person (walking) camera, in degrees (retail 80) |
//! | `[Display]` | `fNearDistance` | camera near plane and room-lookup distance, in Creation units (retail 15 is observed; the key name is from the community default dump, unverified; accepted above 0 up to 1000, a Mudcrab-only bound) |
//! | `[Display]` | `bDrawLandShadows` | 1 makes terrain cast sun shadows; the default 0 matches [Skyrim](https://github.com/Mudcrab-Team/skyrim_decomp/blob/main/findings/sun-shadows-retail-facts.md#4-object-and-terrain-lod). In retail it affects near land only; here it also affects LOD land until a later change limits it |
//! | `[TerrainManager]` | `fSplitDistanceMult` | terrain LOD split rule: a block of `level` cells splits into its four children under `fSplitDistanceMult` x `level` x 2,048 x sqrt(2) units (34,756 / 69,512 at levels 8 / 16 with 1.5) |
//! | `[TerrainManager]` | `fBlockMaximumDistance` | outer reach of tier 16, times `fSplitDistanceMult`, until a level-32 tier exists (not a retail terrain cut) |
//! | `[TerrainManager]` | `fBlockLevel0Distance` | object LOD rule: level 8 hands over to level 4 (default 60,000, `Ultra.ini`); does not affect terrain |
//! | `[TerrainManager]` | `fBlockLevel1Distance` | object LOD rule: level 16 hands over to level 8 (default 90,000, `Ultra.ini`); does not affect terrain |
//! | `[TerrainManager]` | `fTreeLoadDistance` | parsed and kept for tree LOD (default 75,000, `Ultra.ini`); changes nothing on screen until tree LOD draws (#736, #901) |
//! | `[LOD]` | `fLODFadeOutMultObjects`, `fLODFadeOutMultActors`, `fLODFadeOutMultItems` | parsed and kept (defaults 30 / 15 / 10, `Ultra.ini`); changes nothing on screen until LOD fade-out exists (#716) |
//! | `[Display]` | `fMeshLODLevel1FadeDist`, `fMeshLODLevel2FadeDist` | parsed and kept (defaults 999999, `Ultra.ini`); changes nothing on screen until mesh LOD exists |
//! | `[General]` | `uLargeRefLODGridSize` | side of the large-reference grid in cells, an odd number (default 11, `Ultra.ini`) |
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

/// The largest `uLargeRefLODGridSize` accepted. The retail presets use 5 to 11; a larger grid
/// asks for more large-reference rows than the admission bound is sized for.
const MAX_LARGE_REF_GRID_SIZE: i32 = 31;

/// An odd whole grid side: the large-reference grid is centred on the camera's cell.
fn parse_large_ref_grid_size(raw: &str) -> Option<i32> {
    raw.parse::<i32>()
        .ok() // silent-ok: The INI merge reports invalid values before inserting them.
        .filter(|size| *size >= 1 && size % 2 == 1 && *size <= MAX_LARGE_REF_GRID_SIZE)
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

/// A horizontal field of view the engine accepts, in degrees.
fn parse_fov(raw: &str) -> Option<f32> {
    let (low, high) = crate::config::FOV_RANGE_DEGREES;
    raw.parse::<f32>()
        .ok() // silent-ok: The INI merge reports invalid values before inserting them.
        .filter(|value| value.is_finite() && (low..=high).contains(value))
}

/// A near-plane distance the engine accepts: above zero and at most
/// [`crate::config::MAX_NEAR_DISTANCE`] (a Mudcrab-only sanity bound, to keep the frustum valid).
fn parse_near_distance(raw: &str) -> Option<f32> {
    raw.parse::<f32>()
        .ok() // silent-ok: The INI merge reports invalid values before inserting them.
        .filter(|value| {
            value.is_finite() && *value > 0.0 && *value <= crate::config::MAX_NEAR_DISTANCE
        })
}

/// What a key accepts, for the warning about a value it rejects.
fn expected_range(section: &str, key: &str) -> Option<String> {
    match (section, key) {
        ("display", "fdefaultworldfov") | ("display", "fdefault1stpersonfov") => {
            let (low, high) = crate::config::FOV_RANGE_DEGREES;
            Some(format!("degrees from {low} to {high}"))
        }
        ("display", "fneardistance") => Some(format!(
            "a distance above 0 and at most {}",
            crate::config::MAX_NEAR_DISTANCE
        )),
        _ => None,
    }
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
        ("display", "fdefaultworldfov") | ("display", "fdefault1stpersonfov") => {
            Some(parse_fov(raw).is_some())
        }
        ("display", "fneardistance") => Some(parse_near_distance(raw).is_some()),
        ("terrainmanager", "fblocklevel0distance")
        | ("terrainmanager", "fblocklevel1distance")
        | ("terrainmanager", "fblockmaximumdistance")
        | ("terrainmanager", "fsplitdistancemult")
        | ("terrainmanager", "ftreeloaddistance")
        | ("lod", "flodfadeoutmultobjects")
        | ("lod", "flodfadeoutmultactors")
        | ("lod", "flodfadeoutmultitems")
        | ("display", "fmeshlodlevel1fadedist")
        | ("display", "fmeshlodlevel2fadedist") => Some(parse_positive(raw).is_some()),
        ("general", "ulargereflodgridsize") => Some(parse_large_ref_grid_size(raw).is_some()),
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

/// Decodes INI bytes. A line that is valid UTF-8 stays as it is. Any other line is
/// Windows-1252, the code page of Skyrim's own text files, and gets a warning with the
/// file name, the line number and the count of bytes decoded that way. Only that line
/// can change: the rest of the file applies as usual.
fn decode_ini(bytes: &[u8], source: &str) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let mut text = String::with_capacity(bytes.len());
    for (index, line) in bytes.split_inclusive(|&byte| byte == b'\n').enumerate() {
        match std::str::from_utf8(line) {
            Ok(valid) => text.push_str(valid),
            Err(_) => {
                let high = line.iter().filter(|byte| !byte.is_ascii()).count();
                eprintln!(
                    "warning: {source}:{}: line is not UTF-8; decoding {high} non-ASCII byte(s) as Windows-1252",
                    index + 1
                );
                text.push_str(&shared::plugin_selection::decode_windows_1252(line));
            }
        }
    }
    text
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
        let bytes = std::fs::read(path)?;
        let name = path.display().to_string();
        let text = decode_ini(&bytes, &name);
        self.merge_named_str(&text, &name);
        // The source hash covers the raw file bytes, not the decoded text.
        self.sources
            .push(Source::from_bytes(path.to_owned(), &bytes));
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
                let expected = expected_range(&slot.0, &slot.1)
                    .map_or_else(String::new, |range| format!(" (expected {range})"));
                eprintln!(
                    "warning: {source}:{}: ignoring {}={value:?}{expected}; {outcome}",
                    index + 1,
                    key.trim()
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
        for (key, setting) in [
            ("fDefaultWorldFOV", &mut config.world_fov_degrees),
            ("fDefault1stPersonFOV", &mut config.first_person_fov_degrees),
        ] {
            let Some(raw) = self.get("Display", key) else {
                continue;
            };
            match parse_fov(raw) {
                Some(value) => *setting = value,
                None => {
                    let (low, high) = crate::config::FOV_RANGE_DEGREES;
                    eprintln!(
                        "warning: ignoring {key}={raw:?}; expected degrees from {low} to {high}"
                    )
                }
            }
        }
        if let Some(raw) = self.get("Display", "fNearDistance") {
            match parse_near_distance(raw) {
                Some(value) => config.near_distance = value,
                None => eprintln!(
                    "warning: ignoring fNearDistance={raw:?}; expected a distance above 0 and at most {}",
                    crate::config::MAX_NEAR_DISTANCE
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
            ("fTreeLoadDistance", &mut lod.tree_load_distance),
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
        let fade = &mut config.lod_fade;
        for (section, key, setting) in [
            (
                "LOD",
                "fLODFadeOutMultObjects",
                &mut fade.fade_out_mult_objects,
            ),
            (
                "LOD",
                "fLODFadeOutMultActors",
                &mut fade.fade_out_mult_actors,
            ),
            ("LOD", "fLODFadeOutMultItems", &mut fade.fade_out_mult_items),
            (
                "Display",
                "fMeshLODLevel1FadeDist",
                &mut fade.mesh_lod_level1_fade_distance,
            ),
            (
                "Display",
                "fMeshLODLevel2FadeDist",
                &mut fade.mesh_lod_level2_fade_distance,
            ),
        ] {
            let Some(raw) = self.get(section, key) else {
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
        if let Some(raw) = self.get("General", "uLargeRefLODGridSize") {
            match parse_large_ref_grid_size(raw) {
                Some(size) => {
                    config.large_ref_grid_size = size;
                    config
                        .settings_overrides
                        .push("ini:uLargeRefLODGridSize".into());
                }
                None => eprintln!(
                    "warning: ignoring uLargeRefLODGridSize={raw:?}; expected an odd whole number from 1 to {MAX_LARGE_REF_GRID_SIZE}"
                ),
            }
        }
        self.apply_grass(config);
    }

    /// Marks the menu's field-of-view setting as launch-owned when this INI sets the key of the
    /// camera the run starts with. Call it once the command line has been read, because which
    /// camera that is (first-person or world) depends on the flags.
    pub fn claim_active_fov(&self, config: &mut EngineConfig) {
        let key = if config.interactive_world_physics() {
            "fDefault1stPersonFOV"
        } else {
            "fDefaultWorldFOV"
        };
        if self
            .get("Display", key)
            .is_some_and(|raw| parse_fov(raw).is_some())
        {
            config.settings_overrides.push("ini:fov".into());
        }
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

    /// One Windows-1252 byte in a comment must not drop the rest of the file. The source
    /// hash covers the raw bytes.
    #[test]
    fn non_utf8_comment_keeps_the_other_keys_and_hashes_the_raw_bytes() {
        let bytes: &[u8] = b"[General]\r\n; Jos\xe9's settings\r\nbAlwaysActive=1\r\n[Display]\r\nsName=Caf\xe9\r\n";
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Skyrim.ini");
        std::fs::write(&path, bytes).unwrap();
        let mut ini = SkyrimIni::default();
        ini.merge_file(&path).unwrap();
        assert_eq!(ini.get("General", "bAlwaysActive"), Some("1"));
        assert_eq!(ini.get("Display", "sName"), Some("Caf\u{e9}"));
        assert_eq!(
            ini.sources[0].sha256,
            Source::from_bytes(path.clone(), bytes).sha256
        );
    }

    /// Valid UTF-8 text decodes unchanged, and only invalid lines use Windows-1252.
    #[test]
    fn decode_ini_changes_only_the_invalid_lines() {
        let utf8 = "[A]\n; caf\u{e9}\nk=v\n";
        assert_eq!(decode_ini(utf8.as_bytes(), "t"), utf8);
        assert_eq!(
            decode_ini(b"[A]\n; caf\xe9\n; \xc3\xa9\nk=\x80\n", "t"),
            "[A]\n; caf\u{e9}\n; \u{e9}\nk=\u{20ac}\n"
        );
    }

    /// #904, #949: the camera keys default to 80, 80 and 15 and an INI overrides each one. The
    /// sources are cited at the constants in `config.rs`.
    #[test]
    fn camera_keys_default_to_retail_and_an_ini_overrides_them() {
        let config = apply("");
        assert_eq!(config.world_fov_degrees, 80.0);
        assert_eq!(config.first_person_fov_degrees, 80.0);
        assert_eq!(config.near_distance, 15.0);

        let config =
            apply("[Display]\nfDefaultWorldFOV=90\nfDefault1stPersonFOV=65.5\nfNearDistance=7.5\n");
        assert_eq!(config.world_fov_degrees, 90.0);
        assert_eq!(config.first_person_fov_degrees, 65.5);
        assert_eq!(config.near_distance, 7.5);

        // Unusable values leave the retail default; a later usable file value still applies.
        let config =
            apply("[Display]\nfDefaultWorldFOV=500\nfDefault1stPersonFOV=abc\nfNearDistance=-1\n");
        assert_eq!(config.world_fov_degrees, 80.0);
        assert_eq!(config.first_person_fov_degrees, 80.0);
        assert_eq!(config.near_distance, 15.0);
        // A near plane past the sanity bound is rejected, and the bound itself is accepted.
        assert_eq!(
            apply("[Display]\nfNearDistance=1000.5\n").near_distance,
            15.0
        );
        assert_eq!(
            apply("[Display]\nfNearDistance=1000\n").near_distance,
            1000.0
        );
        assert!(expected_range("display", "fneardistance").is_some_and(|r| r.contains("1000")));
        assert!(
            expected_range("display", "fdefaultworldfov").is_some_and(|r| r.contains("30 to 140"))
        );
        let mut ini = SkyrimIni::default();
        ini.merge_str("[Display]\nfDefaultWorldFOV=90\n");
        ini.merge_str("[Display]\nfDefaultWorldFOV=0\n");
        let mut config = EngineConfig::default();
        ini.apply(&mut config);
        assert_eq!(
            config.world_fov_degrees, 90.0,
            "the earlier usable value stands"
        );
    }

    /// The menu value is launch-owned only for the key of the camera the run starts with.
    #[test]
    fn only_the_active_cameras_field_of_view_key_owns_the_menu() {
        let mut ini = SkyrimIni::default();
        ini.merge_str("[Display]\nfDefaultWorldFOV=90\n");
        let mut walking = EngineConfig::default();
        ini.claim_active_fov(&mut walking);
        assert!(
            !walking
                .settings_overrides
                .iter()
                .any(|flag| flag == "ini:fov")
        );
        let mut automated = EngineConfig {
            headless: true,
            ..EngineConfig::default()
        };
        ini.claim_active_fov(&mut automated);
        assert!(
            automated
                .settings_overrides
                .iter()
                .any(|flag| flag == "ini:fov")
        );
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

    /// #949: an INI value replaces each Ultra default, and an unusable value leaves it alone.
    #[test]
    fn ini_overrides_each_lod_and_view_distance_default() {
        let config = apply(
            "[TerrainManager]\nfBlockLevel0Distance=35000\nfBlockLevel1Distance=70000\n\
             fTreeLoadDistance=12500\n[LOD]\nfLODFadeOutMultObjects=5\nfLODFadeOutMultActors=6\n\
             fLODFadeOutMultItems=3\n[Display]\nfMeshLODLevel1FadeDist=4000\n\
             fMeshLODLevel2FadeDist=1600\n[General]\nuLargeRefLODGridSize=9\n",
        );
        assert_eq!(config.terrain_lod.block_level0_distance, 35_000.0);
        assert_eq!(config.terrain_lod.block_level1_distance, 70_000.0);
        assert_eq!(config.terrain_lod.tree_load_distance, 12_500.0);
        assert_eq!(config.lod_fade.fade_out_mult_objects, 5.0);
        assert_eq!(config.lod_fade.fade_out_mult_actors, 6.0);
        assert_eq!(config.lod_fade.fade_out_mult_items, 3.0);
        assert_eq!(config.lod_fade.mesh_lod_level1_fade_distance, 4_000.0);
        assert_eq!(config.lod_fade.mesh_lod_level2_fade_distance, 1_600.0);
        assert_eq!(config.large_ref_grid_size, 9);
        assert_eq!(config.large_ref_grid_radius(), 4);

        let defaults = EngineConfig::default();
        for text in [
            "[TerrainManager]\nfTreeLoadDistance=0\n",
            "[LOD]\nfLODFadeOutMultObjects=-1\n",
            "[Display]\nfMeshLODLevel1FadeDist=nan\n",
            "[General]\nuLargeRefLODGridSize=10\n",
            "[General]\nuLargeRefLODGridSize=0\n",
            "[General]\nuLargeRefLODGridSize=33\n",
            // Keys outside their section change nothing.
            "[Display]\nfLODFadeOutMultObjects=5\n",
        ] {
            let config = apply(text);
            assert_eq!(config.terrain_lod, defaults.terrain_lod, "{text}");
            assert_eq!(config.lod_fade, defaults.lod_fade, "{text}");
            assert_eq!(
                config.large_ref_grid_size, defaults.large_ref_grid_size,
                "{text}"
            );
        }
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
