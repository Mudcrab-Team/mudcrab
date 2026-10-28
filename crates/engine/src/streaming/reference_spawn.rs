//! Shared placement math used by certified source qualification.
use super::{creation_rotation_to_bevy, creation_to_bevy};
use crate::world::{components::WorldPosition, database::CellKey};
use bevy::prelude::*;

/// The ordinary loader's exact placement, also used when a retained source changes cell owner.
pub(crate) fn reference_placement(
    position: [f32; 3],
    rotation: [f32; 3],
    scale: f32,
    key: CellKey,
) -> (WorldPosition, Transform) {
    let creation_position = Vec3::from_array(position);
    let world_position = WorldPosition::from_creation_units(creation_position);
    let translation = match key {
        CellKey::Exterior { grid_x, grid_y, .. } => {
            creation_to_bevy(world_position.relative_to(IVec2::new(grid_x, grid_y)))
        }
        CellKey::Interior(_) => creation_to_bevy(creation_position),
    };
    let transform = Transform::from_translation(translation)
        .with_rotation(creation_rotation_to_bevy(rotation))
        .with_scale(Vec3::splat(scale));
    (world_position, transform)
}
