//! Programmatic cluster quality read by the package renderer.
use bevy::prelude::*;

/// Live cluster quality controls, read by their consumers each frame.
#[derive(Resource, Reflect, Debug, Clone, PartialEq)]
#[reflect(Resource)]
pub struct GraphicsSettings {
    /// Object cluster screen-space error threshold, in pixels.
    pub cluster_error_pixels: f32,
    /// Terrain cluster screen-space error threshold, in pixels.
    pub cluster_terrain_error_pixels: f32,
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            cluster_error_pixels: 1.0,
            cluster_terrain_error_pixels: 1.0,
        }
    }
}
