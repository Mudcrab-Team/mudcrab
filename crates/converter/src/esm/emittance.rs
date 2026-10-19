//! The colour a reference's `XEMI` lends to the `External_Emittance` surfaces of its model.
//!
//! `XEMI` names a `LIGH` or a `REGN`. A light lends its own colour at every hour. A region lends
//! its weather's Effect Lighting colour, which follows the time of day: the window regions
//! (`FXLightRegionInvertWindowWhiterun` and its kin) are black by day and amber at night, which is
//! how Skyrim keeps a building's windows dark in daylight (#242).

/// Effect Lighting's place among a weather's `NAM0` colour types, in xEdit's TES5 order: Sky-Upper,
/// Fog Near, Unknown, Ambient, Sunlight, Sun, Stars, Sky-Lower, Horizon, Effect Lighting, and so on.
/// The official plugins store 13, 14 or 17 types; every one of those layouts includes it.
const EFFECT_LIGHTING: usize = 9;

/// Bytes per `NAM0` colour type: sunrise, day, sunset and night, each RGB and an unused byte.
const NAM0_COLOUR_TYPE: usize = 16;

/// Bytes per `RDWT` entry: the weather, its chance, then a global (or zero).
const RDWT_ENTRY: usize = 12;

/// A colour at sunrise, day, sunset and night, as 8-bit sRGB.
pub type DayColours = [[u8; 3]; 4];

/// The weather a region's `RDWT` list gives the best chance, the first of them on a tie.
///
/// The window regions list one weather at 100%. A region with several weathers changes between
/// them with the climate, which the runtime does not model; its likeliest weather stands in.
/// `None` when the list is malformed or names no weather.
pub fn region_weather(rdwt: &[u8]) -> Option<u32> {
    let (entries, []) = rdwt.as_chunks::<RDWT_ENTRY>() else {
        return None;
    };
    let mut best: Option<(u32, u32)> = None;
    for entry in entries {
        let weather = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
        let chance = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);
        if weather != 0 && best.is_none_or(|(_, best_chance)| chance > best_chance) {
            best = Some((weather, chance));
        }
    }
    best.map(|(weather, _)| weather)
}

/// A weather's Effect Lighting colour at each time of day, from its `NAM0`. `None` when the
/// subrecord is not a whole number of colour types or stops before Effect Lighting.
pub fn weather_effect_lighting(nam0: &[u8]) -> Option<DayColours> {
    let (types, []) = nam0.as_chunks::<NAM0_COLOUR_TYPE>() else {
        return None;
    };
    let colour = types.get(EFFECT_LIGHTING)?;
    Some(std::array::from_fn(|time| {
        [colour[time * 4], colour[time * 4 + 1], colour[time * 4 + 2]]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rdwt(entries: &[(u32, u32, u32)]) -> Vec<u8> {
        entries
            .iter()
            .flat_map(|(weather, chance, global)| {
                [
                    weather.to_le_bytes(),
                    chance.to_le_bytes(),
                    global.to_le_bytes(),
                ]
                .concat()
            })
            .collect()
    }

    /// A `NAM0` of `types` colour types where only Effect Lighting has colour.
    fn nam0(types: usize, effect_lighting: DayColours) -> Vec<u8> {
        let mut bytes = vec![0u8; types * NAM0_COLOUR_TYPE];
        for (time, rgb) in effect_lighting.iter().enumerate() {
            let start = EFFECT_LIGHTING * NAM0_COLOUR_TYPE + time * 4;
            bytes[start..start + 3].copy_from_slice(rgb);
        }
        bytes
    }

    #[test]
    fn a_region_takes_its_likeliest_weather_and_the_first_on_a_tie() {
        assert_eq!(region_weather(&rdwt(&[(0x8277A, 100, 0)])), Some(0x8277A));
        assert_eq!(
            region_weather(&rdwt(&[(1, 20, 0), (2, 60, 7), (3, 20, 0)])),
            Some(2)
        );
        assert_eq!(region_weather(&rdwt(&[(4, 50, 0), (5, 50, 0)])), Some(4));
        // A weather with no chance is still the region's weather when it is the only one.
        assert_eq!(region_weather(&rdwt(&[(6, 0, 0)])), Some(6));
    }

    #[test]
    fn a_region_without_a_usable_weather_has_none() {
        assert_eq!(region_weather(&[]), None);
        assert_eq!(region_weather(&rdwt(&[(0, 100, 0)])), None);
        assert_eq!(region_weather(&rdwt(&[(1, 100, 0)])[..11]), None);
    }

    /// `FXWthrInvertWindowsWhiterun` (`WTHR` 0x0008277A in Skyrim.esm): dark by day.
    #[test]
    fn reads_effect_lighting_from_every_official_nam0_layout() {
        let invert_window = [[124, 70, 16], [0, 0, 0], [243, 118, 54], [242, 176, 55]];
        for types in [13, 14, 17] {
            assert_eq!(
                weather_effect_lighting(&nam0(types, invert_window)),
                Some(invert_window),
                "{types} colour types"
            );
        }
    }

    #[test]
    fn a_nam0_without_effect_lighting_has_none() {
        assert_eq!(
            weather_effect_lighting(&[0; EFFECT_LIGHTING * NAM0_COLOUR_TYPE]),
            None
        );
        assert_eq!(
            weather_effect_lighting(&nam0(17, [[1, 2, 3]; 4])[..271]),
            None
        );
        assert_eq!(weather_effect_lighting(&[]), None);
    }
}
