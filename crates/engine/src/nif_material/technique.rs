//! Which retail `Lighting` technique a property's flags select (SPEC-1 section 9.1,
//! [near-lighting.md](../../../../docs/specs/engine/near-lighting.md)).
//!
//! One technique draws a surface. When several flags would select one, the highest in a fixed
//! order wins. Property flag `n` of 32 or more is bit `n - 32` of the second flag word.

/// A property flag by its number, 0 to 63.
const fn flag(flags_1: u32, flags_2: u32, number: u32) -> bool {
    if number < 32 {
        flags_1 & (1 << number) != 0
    } else {
        flags_2 & (1 << (number - 32)) != 0
    }
}

/// The technique number of the pixel key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Technique {
    None = 0,
    Environment = 1,
    Glow = 2,
    Parallax = 3,
    Face = 4,
    FaceTint = 5,
    Hair = 6,
    Landscape = 8,
    MultilayerParallax = 11,
    TreeAnim = 12,
    LodObjects = 13,
    ProjectedEnvironment = 14,
    LodHd = 15,
    Eye = 16,
    LodLandNoise = 18,
    LandscapeLodBlend = 19,
}

/// The technique of a property with these flag words.
///
/// Technique 7, parallax occlusion, becomes none while `bEnableParallaxOcclusion` is 0, which is
/// its compiled value. `bSimpleLighting`, also 0, would force none.
pub(crate) fn technique(flags_1: u32, flags_2: u32) -> Technique {
    let has = |number| flag(flags_1, flags_2, number);
    if has(17) {
        Technique::Eye
    } else if has(23) && has(41) {
        Technique::ProjectedEnvironment
    } else if has(61) {
        Technique::TreeAnim
    } else if has(56) {
        Technique::MultilayerParallax
    } else if has(63) {
        Technique::LodHd
    } else if has(34) {
        Technique::LodObjects
    } else if has(33) {
        Technique::LodLandNoise
    } else if has(46) {
        Technique::LandscapeLodBlend
    } else if has(14) {
        Technique::Landscape
    } else if has(28) {
        // Parallax occlusion, off by default.
        Technique::None
    } else if has(18) {
        Technique::Hair
    } else if has(21) {
        Technique::FaceTint
    } else if has(10) {
        Technique::Face
    } else if has(11) {
        Technique::Parallax
    } else if has(38) {
        Technique::Glow
    } else if has(7) {
        Technique::Environment
    } else {
        Technique::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const fn words(numbers: &[u32]) -> (u32, u32) {
        let (mut first, mut second) = (0, 0);
        let mut index = 0;
        while index < numbers.len() {
            let number = numbers[index];
            if number < 32 {
                first |= 1 << number;
            } else {
                second |= 1 << (number - 32);
            }
            index += 1;
        }
        (first, second)
    }

    /// The order of section 9.1, highest first, with the flags that select each technique.
    const ORDER: [(&[u32], Technique); 16] = [
        (&[17], Technique::Eye),
        (&[23, 41], Technique::ProjectedEnvironment),
        (&[61], Technique::TreeAnim),
        (&[56], Technique::MultilayerParallax),
        (&[63], Technique::LodHd),
        (&[34], Technique::LodObjects),
        (&[33], Technique::LodLandNoise),
        (&[46], Technique::LandscapeLodBlend),
        (&[14], Technique::Landscape),
        (&[28], Technique::None),
        (&[18], Technique::Hair),
        (&[21], Technique::FaceTint),
        (&[10], Technique::Face),
        (&[11], Technique::Parallax),
        (&[38], Technique::Glow),
        (&[7], Technique::Environment),
    ];

    /// The flag numbers that a source reads: `has(n)` calls, and the bits of the `SLSF1_` and
    /// `SLSF2_` constants.
    fn flags_read(source: &str, read: &mut BTreeSet<u32>) {
        let code = source.split("#[cfg(test)]").next().unwrap_or(source);
        for call in code.split("has(").skip(1) {
            let digits: String = call.chars().take_while(char::is_ascii_digit).collect();
            read.extend(digits.parse::<u32>().ok());
        }
        for line in code.lines().map(str::trim) {
            let line = line.strip_prefix("pub(crate) ").unwrap_or(line);
            let Some(rest) = line.strip_prefix("const SLSF") else {
                continue;
            };
            let base = if rest.starts_with("2_") { 32 } else { 0 };
            let Some((_, value)) = rest.split_once('=') else {
                continue;
            };
            if value.trim() == "1;" {
                read.insert(base);
            }
            for shifted in value.split("1 << ").skip(1) {
                let digits: String = shifted.chars().take_while(char::is_ascii_digit).collect();
                read.extend(digits.parse::<u32>().ok().map(|bit| base + bit));
            }
        }
    }

    /// The flag inventory may not deny a flag that the technique order or the retail surface
    /// reads. The inventory's own check cannot see a read by flag number. This one covers the
    /// three files below only.
    #[test]
    fn the_flag_inventory_does_not_deny_a_flag_read_here() {
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tools/flag_inventory/dispositions.json"
        ))
        .expect("the flag inventory is JSON");
        let mut read = BTreeSet::new();
        for source in [
            include_str!("technique.rs"),
            include_str!("retail.rs"),
            include_str!("../nif_material.rs"),
        ] {
            flags_read(source, &mut read);
        }
        for known in [0, 10, 11, 20, 21, 29, 30, 33, 41, 60, 63] {
            assert!(read.contains(&known), "the scan lost flag {known}");
        }
        let denied: Vec<_> = read
            .into_iter()
            .filter_map(|number| {
                let key = format!("NIF.SLSF{}.bit{}", number / 32 + 1, number % 32);
                let entry = &inventory["dispositions"][&key];
                let reason = entry["reason"].as_str().unwrap_or_default();
                let honest = match entry["disposition"].as_str() {
                    Some("respected" | "partial") => true,
                    Some("system-not-implemented") => reason.contains("reads it"),
                    _ => false,
                };
                (!honest).then(|| format!("{key} (flag {number}): {}", entry["disposition"]))
            })
            .collect();
        assert!(
            denied.is_empty(),
            "the engine reads these flags and the inventory does not say so: {denied:#?}"
        );
    }

    #[test]
    fn the_higher_technique_wins_for_every_pair_of_selectors() {
        for (higher, (first_flags, wins)) in ORDER.iter().enumerate() {
            let (first, second) = words(first_flags);
            assert_eq!(technique(first, second), *wins, "{first_flags:?} alone");
            for (lower_flags, _) in &ORDER[higher + 1..] {
                let (low_first, low_second) = words(lower_flags);
                assert_eq!(
                    technique(first | low_first, second | low_second),
                    *wins,
                    "{first_flags:?} over {lower_flags:?}"
                );
            }
        }
        assert_eq!(technique(0, 0), Technique::None);
        // Projected environment needs both of its flags.
        let (first, second) = words(&[23]);
        assert_eq!(technique(first, second), Technique::None);
    }
}
