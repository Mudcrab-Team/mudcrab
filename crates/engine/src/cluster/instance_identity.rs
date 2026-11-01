//! CPU identity carried alongside an instance through scene/GPU reorderings.
//!
//! An identity is descriptive only: it never authorizes drawing or stock suppression.
//! Object keys are local to the retained CertifiedPackage; never reuse them with another
//! package generation, even if its logical block coordinates happen to match.

use super::reader::{CertifiedPackage, RecordKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClusterInstanceIdentity {
    Object(RecordKey),
    Terrain {
        /// Index of the validated immutable terrain block in the same scene.
        block: u32,
        worldspace: u32,
        tier: u32,
        /// Logical MCI header-grid anchor, NOT TerrainBlock's absolute cell anchor.
        /// The metadata-to-header conversion belongs to the validated terrain recipe.
        anchor: [i32; 2],
    },
    /// Explicit invented scene input; no certified reference or placed world is implied.
    Fixture,
}

impl ClusterInstanceIdentity {
    /// Resolves ownership, not visibility/readiness. The caller must supply the SAME
    /// retained certificate that scoped this object's key when the scene was built.
    pub fn placed_worldspace(&self, certificate: Option<&CertifiedPackage>) -> Option<u32> {
        match self {
            Self::Object(key) => certificate?.record(key).map(|record| record.worldspace),
            Self::Terrain { worldspace, .. } => Some(*worldspace),
            Self::Fixture => None,
        }
    }
}
