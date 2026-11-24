//! Authored NiAlphaProperty state in converted glTF material extras.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NifAlphaProperty {
    pub blend_enabled: bool,
    pub source_factor: u8,
    pub destination_factor: u8,
    pub test_enabled: bool,
    pub test_function: u8,
    pub threshold: u8,
}

impl NifAlphaProperty {
    /// Decode nif.xml AlphaFlags, as documented in vendor/project-wormhole-nif/src/nif_flags.rs.
    pub fn from_nif(flags: u16, threshold: u8) -> Self {
        let [low, high] = flags.to_le_bytes();
        Self {
            blend_enabled: low & 1 != 0,
            source_factor: (low >> 1) & 0x0f,
            destination_factor: ((low >> 5) | (high << 3)) & 0x0f,
            test_enabled: high & 2 != 0,
            test_function: (high >> 2) & 7,
            threshold,
        }
    }
}
