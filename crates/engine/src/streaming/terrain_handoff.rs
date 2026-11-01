//! Exact stock terrain proof input used by current-view cluster clipping.
use bevy::{prelude::*, render::sync_world::MainEntity};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SurfaceKey {
    pub world: u32,
    pub grid: IVec2,
    pub quadrant: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Surface {
    pub keys: Vec<SurfaceKey>,
    pub(crate) mesh: bevy::asset::AssetId<Mesh>,
}

#[derive(Resource, Default)]
pub(crate) struct RenderTerrain {
    pub surfaces: HashMap<MainEntity, Surface>,
}

#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TerrainHandoffSystems {
    Cluster,
    Filter,
}
