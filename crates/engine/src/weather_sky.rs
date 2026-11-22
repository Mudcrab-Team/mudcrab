//! The active weather's sky and fog colours and fog distances, blended by part of the day (#891).
//!
//! Everything here comes from one `WTHR` record in the world database: the `NAM0` colours (Sky
//! Upper, Sky Lower, Horizon, Fog Near and Fog Far, for sunrise, day, sunset and night) and the
//! `FNAM` fog distances (near, far, power and max, for day and night). Nothing is filled in. A
//! colour the record lacks makes the weather unusable and the caller says so. A fog field the
//! record lacks removes the fog and is listed in [`WeatherSky::fog_omissions`].
//!
//! Which weather is active is a Mudcrab choice: the engine has no weather simulation, and its
//! sky has always been `SkyrimClear`, so that record is the default
//! ([`DEFAULT_WEATHER_FORM_ID`]). Retail picks the weather from the climate and region lists.
//!
//! ## Where the facts come from
//!
//! The retail facts are in `skyrim_decomp#812` (static reading of 1.7.104.0, independently
//! reviewed: https://github.com/Mudcrab-Team/skyrim_decomp/pull/812). Tags below follow it.
//!
//! - **Fields (verified).** The record holds eight float32 fog values: day and night near, far,
//!   power and max (`FNAM`). Fog near colour is colour row 1 and fog far colour is row 12 of
//!   `NAM0`, each with four day parts.
//! - **Fog distance blend (verified).** One day weight is shared by near, far, power and max, and
//!   each value is `weight * day + (1 - weight) * night`. With `A` = sunrise begin less
//!   `fDaytimeColorExtension` (floored at 0), `B` = sunrise end, `C` = sunset begin and `D` = sunset
//!   end plus the extension (capped at 23.99), the weight is `(h - A) / (B - A)` for `A < h < B`,
//!   1 for `B <= h <= C`, `(D - h) / (D - C)` for `C < h < D`, and 0 otherwise. The extension is
//!   0.5 hours.
//! - **Colour blend (verified).** The four colour slots mix by the weights of [`slot_weights`]:
//!   night to sunrise over the first half of `[A, B]`, sunrise to day over the second half, day
//!   through `C`, day to sunset over the first half of `[C, D]` and sunset to night over the
//!   second, night elsewhere. Colours mix as 8-bit sRGB values and cap at 1 per channel.
//! - **Weather transitions (not simulated).** During a change retail also mixes the previous
//!   weather with its own weight. The engine has no weather change, so the weight is always 1.
//!   Forced and interrupted transitions are unverified in retail.
//! - **Scene far override (not applied).** A gated override replaces far with a scene limit. The
//!   gate byte starts closed in the image and its runtime writer is unverified, so Mudcrab applies
//!   no override.
//!
//! This module supplies the values. `sky.rs` draws them and says which drawing steps are
//! unverified.

use crate::sky::{FogRamp, SkyPalette};
use crate::sun::SunTiming;
use crate::world::environment::{EnvironmentCatalog, TimeOfDay, Weather};
use bevy::prelude::Color;

/// `SkyrimClear` (`WTHR` 0x0000081A). The engine's sky has always been this weather. Retail picks
/// the weather from the climate and region weather lists, which the engine does not simulate.
pub const DEFAULT_WEATHER_FORM_ID: u32 = 0x0000_081A;

/// `fDaytimeColorExtension`, in hours: sunrise colours begin this long before the climate's
/// sunrise begin, and sunset colours end this long after its sunset end (RF-SUN-005).
pub const DAYTIME_COLOUR_EXTENSION_HOURS: f64 = 0.5;

/// The latest hour a colour or fog endpoint can take: retail caps the sunset end plus the extension
/// here (`skyrim_decomp#812`, verified).
const LAST_COLOUR_HOUR: f64 = 23.99;

/// The four colour slots, in the order of [`TimeOfDay::ALL`]: sunrise, day, sunset, night.
pub type SlotWeights = [f32; 4];

/// The time-of-day colour weights at a game hour (RF-SUN-005, verified again by
/// `skyrim_decomp#812`). They sum to 1. A non-finite hour counts as the retail `GameHour` record,
/// as in `sun.rs`.
pub fn slot_weights(hour: f64, timing: SunTiming) -> SlotWeights {
    let hour = if hour.is_finite() {
        hour.rem_euclid(24.0)
    } else {
        crate::sun::RETAIL_GAME_HOUR
    };
    let [rise_begin, rise_end, set_begin, set_end] = timing.0.map(|v| f64::from(v) / 6.0);
    let rise_start = (rise_begin - DAYTIME_COLOUR_EXTENSION_HOURS).max(0.0);
    let set_stop = (set_end + DAYTIME_COLOUR_EXTENSION_HOURS).min(LAST_COLOUR_HOUR);
    const SUNRISE: usize = 0;
    const DAY: usize = 1;
    const SUNSET: usize = 2;
    const NIGHT: usize = 3;
    let mut weights = [0.0_f32; 4];
    if rise_start < hour && hour < rise_end {
        let mid = (rise_start + rise_end) / 2.0;
        let peak = 1.0 - (hour - mid).abs() / ((rise_end - rise_start) / 2.0);
        weights[SUNRISE] = f32_from(peak);
        weights[if hour < mid { NIGHT } else { DAY }] = f32_from(1.0 - peak);
    } else if rise_end <= hour && hour <= set_begin {
        weights[DAY] = 1.0;
    } else if set_begin < hour && hour < set_stop {
        let mid = (set_begin + set_stop) / 2.0;
        let peak = 1.0 - (hour - mid).abs() / ((set_stop - set_begin) / 2.0);
        weights[SUNSET] = f32_from(peak);
        weights[if hour < mid { DAY } else { NIGHT }] = f32_from(1.0 - peak);
    } else {
        weights[NIGHT] = 1.0;
    }
    weights
}

/// The day weight that mixes the day and night fog scalars at a game hour (verified,
/// `skyrim_decomp#812`): the same weight for near, far, power and max. `None` when the endpoints
/// are not ordered and distinct, which retail leaves unverified.
pub fn fog_day_weight(hour: f64, timing: SunTiming) -> Option<f32> {
    let hour = if hour.is_finite() {
        hour.rem_euclid(24.0)
    } else {
        crate::sun::RETAIL_GAME_HOUR
    };
    let [rise_begin, rise_end, set_begin, set_end] = timing.0.map(|v| f64::from(v) / 6.0);
    let a = (rise_begin - DAYTIME_COLOUR_EXTENSION_HOURS).max(0.0);
    let b = rise_end;
    let c = set_begin;
    let d = (set_end + DAYTIME_COLOUR_EXTENSION_HOURS).min(LAST_COLOUR_HOUR);
    if !(a < b && b <= c && c < d) {
        return None;
    }
    let weight = if a < hour && hour < b {
        (hour - a) / (b - a)
    } else if b <= hour && hour <= c {
        1.0
    } else if c < hour && hour < d {
        (d - hour) / (d - c)
    } else {
        0.0
    };
    Some(f32_from(weight))
}

/// A weight in `[0, 1]` as an `f32`. The narrowing loses nothing a colour or a distance can show.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is a weight in [0, 1]; f32 keeps 7 digits"
)]
fn f32_from(value: f64) -> f32 {
    value as f32
}

/// The five sky and fog colours of one time-of-day slot, 8-bit sRGB.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SlotColours {
    upper: [u8; 3],
    lower: [u8; 3],
    horizon: [u8; 3],
    fog_near: [u8; 3],
    fog_far: [u8; 3],
}

/// One `FNAM` fog distance set (day or night).
#[derive(Debug, Clone, Copy, PartialEq)]
struct FogSet {
    near: f32,
    far: f32,
    power: f32,
    max: f32,
}

/// One weather's sky and fog, ready to blend at any game hour.
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherSky {
    pub form_id: u32,
    pub editor_id: Option<String>,
    slots: [SlotColours; 4],
    /// The day and night fog sets. `None` when the record lacks any of the eight values.
    fog: Option<[FogSet; 2]>,
    timing: SunTiming,
    /// The `FNAM` fields the record did not publish. Empty when `fog` is `Some`.
    pub fog_omissions: Vec<&'static str>,
}

impl WeatherSky {
    /// Reads the sky and fog of `weather`. A colour the record lacks is an error naming every
    /// missing colour; a missing fog field is listed in [`Self::fog_omissions`] instead.
    pub fn from_weather(
        form_id: u32,
        weather: &Weather,
        timing: SunTiming,
    ) -> Result<Self, String> {
        let mut missing = Vec::new();
        let mut slots = Vec::with_capacity(4);
        for time in TimeOfDay::ALL {
            let mut colour = |name: &str| {
                weather.color(name, time).unwrap_or_else(|| {
                    missing.push(format!("{name} ({time:?})"));
                    [0; 3]
                })
            };
            slots.push(SlotColours {
                upper: colour("sky_upper"),
                lower: colour("sky_lower"),
                horizon: colour("horizon"),
                fog_near: colour("fog_near"),
                fog_far: colour("fog_far"),
            });
        }
        if !missing.is_empty() {
            return Err(format!(
                "the weather record has no NAM0 colour for {}",
                missing.join(", ")
            ));
        }
        let record = &weather.fog;
        let day = [
            ("fog_day_near", record.day_near),
            ("fog_day_far", record.day_far),
            ("fog_day_power", record.day_power),
            ("fog_day_max", record.day_max),
        ];
        let night = [
            ("fog_night_near", record.night_near),
            ("fog_night_far", record.night_far),
            ("fog_night_power", record.night_power),
            ("fog_night_max", record.night_max),
        ];
        let fog_omissions: Vec<&'static str> = day
            .iter()
            .chain(night.iter())
            .filter(|(_, value)| value.is_none_or(|value| !value.is_finite()))
            .map(|(name, _)| *name)
            .collect();
        let set = |values: [(&str, Option<f32>); 4]| {
            let [near, far, power, max] = values.map(|(_, value)| value);
            Some(FogSet {
                near: near?,
                far: far?,
                power: power?,
                max: max?,
            })
        };
        let fog = match (fog_omissions.is_empty(), set(day), set(night)) {
            (true, Some(day), Some(night)) => Some([day, night]),
            _ => None,
        };
        Ok(Self {
            form_id,
            editor_id: weather.editor_id.clone(),
            slots: [slots[0], slots[1], slots[2], slots[3]],
            fog,
            timing,
            fog_omissions,
        })
    }

    /// Reads [`DEFAULT_WEATHER_FORM_ID`] from the catalog, with the sun timing of the climate of
    /// `worldspace`. Every failure names the record or field that is missing.
    pub fn default_for_world(
        catalog: &EnvironmentCatalog,
        worldspace: u32,
    ) -> Result<Self, String> {
        let weather = catalog.weather(DEFAULT_WEATHER_FORM_ID).ok_or_else(|| {
            format!(
                "the world database has no weather {DEFAULT_WEATHER_FORM_ID:#010X} (SkyrimClear)"
            )
        })?;
        let timing = catalog
            .worldspace_climate(worldspace)
            .and_then(|climate| catalog.sun_timing(climate))
            .ok_or_else(|| {
                format!(
                    "worldspace {worldspace:#x} has no climate sunrise and sunset times, \
                     so the weather colours cannot be blended by time of day"
                )
            })?;
        Self::from_weather(DEFAULT_WEATHER_FORM_ID, weather, timing)
    }

    /// `SkyrimClear (0000081A)`, or the form id alone for a weather with no editor id.
    pub fn label(&self) -> String {
        match &self.editor_id {
            Some(name) => format!("{name} ({:08X})", self.form_id),
            None => format!("{:08X}", self.form_id),
        }
    }

    /// Whether the record published a complete set of fog distances.
    pub fn has_fog(&self) -> bool {
        self.fog.is_some()
    }

    /// The weather's palette at a game hour: the five colours mixed by [`slot_weights`], and the
    /// fog distances mixed by [`fog_day_weight`]. Density and brightness scales are 1. The fog
    /// ramp is `None` when the record omitted a fog field or the day endpoints are unusable.
    pub fn palette_at(&self, hour: f64) -> SkyPalette {
        let weights = slot_weights(hour, self.timing);
        let mix = |pick: fn(&SlotColours) -> [u8; 3]| {
            let channel = |index: usize| {
                let sum: f32 = self
                    .slots
                    .iter()
                    .zip(weights)
                    .map(|(slot, weight)| weight * f32::from(pick(slot)[index]))
                    .sum();
                sum / 255.0
            };
            Color::srgb(channel(0), channel(1), channel(2))
        };
        // One day weight mixes all four fog scalars (verified, module docs).
        let day_weight = fog_day_weight(hour, self.timing);
        let fog_ramp = self.fog.zip(day_weight).map(|([day, night], weight)| {
            let lerp = |day: f32, night: f32| day * weight + night * (1.0 - weight);
            FogRamp {
                near: lerp(day.near, night.near),
                far: lerp(day.far, night.far),
                power: lerp(day.power, night.power),
                max: lerp(day.max, night.max),
            }
        });
        SkyPalette {
            upper: mix(|slot| slot.upper),
            lower: mix(|slot| slot.lower),
            horizon: mix(|slot| slot.horizon),
            fog_near: mix(|slot| slot.fog_near),
            fog_far: mix(|slot| slot.fog_far),
            fog_ramp,
            fog_density_scale: 1.0,
            brightness: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::environment::WeatherFog;
    use bevy::color::Srgba;

    /// The Skyrim.esm climate TNAM (CLMT 0x15F, 0x812): sunrise 5:30 to 10:00, sunset 16:00 to
    /// 20:30. Test data only: the code under test takes the timing from the pack.
    const TIMING: SunTiming = SunTiming([33, 60, 96, 123]);

    /// A weather whose every colour and distance is distinct, so a swapped field shows.
    fn weather() -> Weather {
        let mut weather = Weather {
            editor_id: Some("TestWeather".to_owned()),
            fog: WeatherFog {
                day_near: Some(1_000.0),
                day_far: Some(80_000.0),
                night_near: Some(500.0),
                night_far: Some(40_000.0),
                day_power: Some(0.4),
                night_power: Some(0.2),
                day_max: Some(0.85),
                night_max: Some(0.5),
            },
            ..Default::default()
        };
        for (base, time) in [
            (10_u8, TimeOfDay::Sunrise),
            (50, TimeOfDay::Day),
            (90, TimeOfDay::Sunset),
            (130, TimeOfDay::Night),
        ] {
            for (index, name) in ["sky_upper", "sky_lower", "horizon", "fog_near", "fog_far"]
                .into_iter()
                .enumerate()
            {
                let value = base + index as u8;
                weather
                    .colors
                    .insert((name.to_owned(), time), [value, value + 1, value + 2]);
            }
        }
        weather
    }

    fn sky() -> WeatherSky {
        WeatherSky::from_weather(0x81A, &weather(), TIMING).unwrap()
    }

    #[test]
    fn the_four_weights_follow_the_climate_times() {
        // Noon is day only; midnight is night only.
        assert_eq!(slot_weights(12.0, TIMING), [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(slot_weights(0.0, TIMING), [0.0, 0.0, 0.0, 1.0]);
        // Sunrise peaks at the middle of [riseBegin - 0.5, riseEnd] = [5.0, 10.0].
        assert_eq!(slot_weights(7.5, TIMING), [1.0, 0.0, 0.0, 0.0]);
        // Before the peak the other slot is night, after it day.
        let early = slot_weights(6.25, TIMING);
        assert!((early[0] - 0.5).abs() < 1e-6 && (early[3] - 0.5).abs() < 1e-6);
        let late = slot_weights(8.75, TIMING);
        assert!((late[0] - 0.5).abs() < 1e-6 && (late[1] - 0.5).abs() < 1e-6);
        // Sunset peaks at the middle of [setBegin, setEnd + 0.5] = [16.0, 21.0].
        assert_eq!(slot_weights(18.5, TIMING), [0.0, 0.0, 1.0, 0.0]);
        let evening = slot_weights(19.75, TIMING);
        assert!((evening[2] - 0.5).abs() < 1e-6 && (evening[3] - 0.5).abs() < 1e-6);
        for hour in [
            0.0, 4.9, 5.0, 7.0, 9.99, 10.0, 15.9, 16.0, 17.0, 20.99, 21.0, 23.9,
        ] {
            let weights = slot_weights(hour, TIMING);
            assert!(
                (weights.iter().sum::<f32>() - 1.0).abs() < 1e-5,
                "weights at {hour} sum to {}",
                weights.iter().sum::<f32>()
            );
        }
    }

    #[test]
    fn the_palette_and_fog_come_from_the_record() {
        let palette = sky().palette_at(12.0);
        // Day: the record's day colours and distances, unchanged.
        assert_eq!(palette.upper, Color::srgb_u8(50, 51, 52));
        assert_eq!(palette.horizon, Color::srgb_u8(52, 53, 54));
        assert_eq!(palette.fog_near, Color::srgb_u8(53, 54, 55));
        assert_eq!(palette.fog_far, Color::srgb_u8(54, 55, 56));
        assert_eq!(
            palette.fog_ramp,
            Some(FogRamp {
                near: 1_000.0,
                far: 80_000.0,
                power: 0.4,
                max: 0.85
            })
        );
        assert_eq!(palette.fog_density_scale, 1.0);
        // Night: the night colours and distances.
        let night = sky().palette_at(0.0);
        assert_eq!(night.fog_far, Color::srgb_u8(134, 135, 136));
        assert_eq!(night.fog_ramp.unwrap().far, 40_000.0);
        assert_eq!(night.fog_ramp.unwrap().max, 0.5);
    }

    #[test]
    fn colours_and_distances_blend_by_day_part() {
        // 6:15 is half sunrise, half night. Fog Far: sunrise (14,15,16), night (134,135,136).
        // The fog scalars use the day weight (6.25 - 5.0) / (10.0 - 5.0) = 0.25 instead.
        let dawn = sky().palette_at(6.25);
        let Srgba {
            red, green, blue, ..
        } = dawn.fog_far.to_srgba();
        assert!((red * 255.0 - 74.0).abs() < 1e-3, "{red}");
        assert!((green * 255.0 - 75.0).abs() < 1e-3);
        assert!((blue * 255.0 - 76.0).abs() < 1e-3);
        let ramp = dawn.fog_ramp.unwrap();
        assert!((ramp.far - 50_000.0).abs() < 1.0, "{}", ramp.far);
        assert!((ramp.near - 625.0).abs() < 1e-2);
        assert!((ramp.power - 0.25 * 0.4 - 0.75 * 0.2).abs() < 1e-6);
        assert!((ramp.max - 0.5875).abs() < 1e-6);
        // At the sunrise colour peak (7.5 h) the day weight is 0.5, not 1: the colour slots and
        // the fog scalars follow different weights.
        let peak = sky().palette_at(7.5);
        assert!((peak.fog_ramp.unwrap().far - 60_000.0).abs() < 1.0);
        assert_eq!(peak.fog_far, Color::srgb_u8(14, 15, 16));
    }

    #[test]
    fn the_day_weight_ramps_over_the_sunrise_and_sunset_windows() {
        // Endpoints A, B, C, D = 5, 10, 16, 21 hours for the climate times 33, 60, 96, 123.
        let weight = |hour| fog_day_weight(hour, TIMING).unwrap();
        for (hour, expected) in [
            (0.0, 0.0),
            (5.0, 0.0),
            (6.0, 0.2),
            (7.5, 0.5),
            (10.0, 1.0),
            (13.0, 1.0),
            (16.0, 1.0),
            (18.5, 0.5),
            (20.0, 0.2),
            (21.0, 0.0),
            (23.5, 0.0),
        ] {
            assert!(
                (weight(hour) - expected).abs() < 1e-6,
                "{hour}: {}",
                weight(hour)
            );
        }
        // Coincident endpoints are unverified in retail: no weight, so no fog distances.
        assert_eq!(fog_day_weight(12.0, SunTiming([0, 0, 96, 123])), None);
    }

    #[test]
    fn a_missing_colour_is_an_error_naming_it() {
        let mut record = weather();
        record
            .colors
            .remove(&("fog_far".to_owned(), TimeOfDay::Sunset));
        record
            .colors
            .remove(&("horizon".to_owned(), TimeOfDay::Day));
        let error = WeatherSky::from_weather(0x81A, &record, TIMING).unwrap_err();
        assert!(error.contains("fog_far (Sunset)"), "{error}");
        assert!(error.contains("horizon (Day)"), "{error}");
    }

    #[test]
    fn a_missing_fog_field_removes_the_fog_and_is_listed() {
        let mut record = weather();
        record.fog.day_far = None;
        record.fog.night_max = Some(f32::NAN);
        let sky = WeatherSky::from_weather(0x81A, &record, TIMING).unwrap();
        assert!(!sky.has_fog());
        assert_eq!(sky.fog_omissions, ["fog_day_far", "fog_night_max"]);
        // The colours still come from the record, and no distance is invented.
        let palette = sky.palette_at(12.0);
        assert_eq!(palette.fog_far, Color::srgb_u8(54, 55, 56));
        assert_eq!(palette.fog_ramp, None);
    }

    #[test]
    fn the_default_weather_needs_the_record_and_climate_times() {
        let empty = EnvironmentCatalog::default();
        let error = WeatherSky::default_for_world(&empty, 0x3c).unwrap_err();
        assert!(error.contains("no weather"), "{error}");
    }
}
