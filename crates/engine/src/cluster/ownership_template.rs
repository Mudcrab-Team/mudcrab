//! Nonrender source geometry retained for certified draw qualification.
use bevy::prelude::*;

/// Nonrender source data for strict bounds/material checks and physics.
/// The embedded values are not ECS render components and are never extracted.
#[derive(Component, Clone, Reflect)]
#[reflect(Component)]
pub(crate) struct OwnedPrimitiveSource {
    pub(crate) mesh: Mesh3d,
    pub(crate) material: Option<MeshMaterial3d<StandardMaterial>>,
}
