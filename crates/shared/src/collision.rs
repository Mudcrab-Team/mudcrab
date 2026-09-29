//! Collision data carried in converted GLB scene extras.

use serde::{Deserialize, Serialize};

pub const COLLISION_ASSET_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollisionAsset {
    pub version: u32,
    /// `true` distinguishes an authored absence from an older GLB with no collision metadata.
    pub authored: bool,
    pub shapes: Vec<CollisionShape>,
    /// Unsupported blocks are retained so coverage can identify missing collision.
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CollisionShape {
    Mesh {
        vertices: Vec<[f32; 3]>,
        triangles: Vec<[u32; 3]>,
    },
    Capsule {
        a: [f32; 3],
        b: [f32; 3],
        radius: f32,
    },
    Box {
        center: [f32; 3],
        half_extents: [f32; 3],
    },
    Hull {
        points: Vec<[f32; 3]>,
    },
}
