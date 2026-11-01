//! Placement policy shared by render export and authored collision extraction.
use project_wormhole_nif::model::all::Model;

/// The NIF root node, whose local transform the placing reference supplies instead.
const ROOT_BLOCK: usize = crate::mesh::NIF_ROOT_BLOCK as usize;

/// Reference placement shared by render export and collision extraction.
///
/// A placed reference supplies the root transform, so the authored local transform of block 0 is
/// replaced for every NIF: `.nif`, `.btr` and `.bto` alike, with or without a skin. The rule has
/// no file-extension or skin input because the engine places every model through its reference,
/// and because `dlc01/lod/soulcairn/sc_spire04_lod.nif` (90 degree root) only matches the footprint
/// of `sc_spire04.nif` (identity root) without that rotation.
#[derive(Clone, Copy)]
pub(crate) struct PlacementFrame;

impl PlacementFrame {
    pub(crate) fn new() -> Self {
        Self
    }

    /// A reference replaces block 0's local transform in every scene.
    /// A skinned mesh keeps its separate bone trees: they carry their own transforms and are not
    /// static nodes, so `place_model` never touches them.
    /// Collision applies this before composing the target/body frame, so shapes, COM and
    /// inertia all receive the same policy through their existing frame conversion.
    pub(crate) fn replaces_block(self, block: usize) -> bool {
        block == ROOT_BLOCK
    }

    pub(crate) fn place_model(self, model: &mut Model) {
        model.scene_root_rotation = Some(shared::coordinates::CREATION_TO_RUNTIME_ROTATION);
        crate::mesh::clear_root_transform(model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_block_zero_is_replaced() {
        let frame = PlacementFrame::new();
        assert!(frame.replaces_block(0));
        assert!(!frame.replaces_block(1));
    }
}
