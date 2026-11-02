//! The visual profile picks a whole color pipeline at startup: inputs, shading, fog,
//! post-processing and output. Stages are never mixed across profiles, so options that only
//! steer one pipeline are refused under the other instead of being ignored.
//!
//! The specification is `docs/specs/engine/visual-profiles.md`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

pub const VISUAL_PROFILE_VALUES: &str = "mudcrab or skyrim";

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VisualProfile {
    /// Bevy PBR with the approved TonyMcMapface look and grade.
    #[default]
    Mudcrab,
    /// Retail Skyrim SE's color pipeline evaluated on raw authored values.
    Skyrim,
}

/// Launch options that steer only the `mudcrab` pipeline. Each names why it has no meaning
/// under `skyrim`, whose look comes from the selected image space and authored records.
pub const MUDCRAB_ONLY_OPTIONS: [(&str, &str); 5] = [
    (
        "--tone-map",
        "skyrim uses the retail image-space tone curve",
    ),
    (
        "--tone-map-params",
        "skyrim uses the retail image-space tone curve",
    ),
    (
        "--color-grade",
        "skyrim grades with the authored image space",
    ),
    (
        "--grade-preset",
        "skyrim grades with the authored image space",
    ),
    (
        "--visual-preset",
        "skyrim takes sky and fog from weather records",
    ),
];

impl VisualProfile {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "mudcrab" => Some(Self::Mudcrab),
            "skyrim" => Some(Self::Skyrim),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Mudcrab => "mudcrab",
            Self::Skyrim => "skyrim",
        }
    }

    /// Stable persisted index; `mudcrab` is 0 so settings files written before profiles existed
    /// keep the approved look.
    pub fn index(self) -> usize {
        match self {
            Self::Mudcrab => 0,
            Self::Skyrim => 1,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Mudcrab),
            1 => Some(Self::Skyrim),
            _ => None,
        }
    }

    /// The first explicit launch option that belongs to another profile, with the reason.
    pub fn foreign_option<'a>(
        self,
        options: impl IntoIterator<Item = &'a String>,
    ) -> Option<(&'static str, &'static str)> {
        if self != Self::Skyrim {
            return None;
        }
        options.into_iter().find_map(|given| {
            MUDCRAB_ONLY_OPTIONS
                .iter()
                .find(|(option, _)| given == option)
                .copied()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse_back_and_indices_are_stable() {
        for profile in [VisualProfile::Mudcrab, VisualProfile::Skyrim] {
            assert_eq!(VisualProfile::parse(profile.name()), Some(profile));
            assert_eq!(VisualProfile::from_index(profile.index()), Some(profile));
        }
        assert_eq!(VisualProfile::parse("SKYRIM"), Some(VisualProfile::Skyrim));
        assert_eq!(VisualProfile::parse("vanilla"), None);
        assert_eq!(VisualProfile::from_index(2), None);
        assert_eq!(VisualProfile::default(), VisualProfile::Mudcrab);
        assert_eq!(VisualProfile::Mudcrab.index(), 0);
    }

    #[test]
    fn only_skyrim_refuses_mudcrab_options() {
        for (option, _) in MUDCRAB_ONLY_OPTIONS {
            let given = [option.to_owned()];
            assert_eq!(VisualProfile::Mudcrab.foreign_option(&given), None);
            assert_eq!(
                VisualProfile::Skyrim.foreign_option(&given).map(|(o, _)| o),
                Some(option)
            );
        }
        let unrelated = ["--lights".to_owned(), "--image-space".to_owned()];
        assert_eq!(VisualProfile::Skyrim.foreign_option(&unrelated), None);
    }
}
