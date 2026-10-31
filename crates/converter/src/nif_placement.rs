//! Placement policy shared by render export and authored collision extraction.
use project_wormhole_nif::{model::all::Model, nif_file::NifFile};
use std::path::Path;

#[derive(Clone, Copy)]
pub(crate) struct PlacementFrame {
    replace_root: bool,
}

impl PlacementFrame {
    pub(crate) fn from_nif(path: &Path, nif: &NifFile) -> Self {
        Self::new(path, nif.has_skeleton())
    }

    fn new(_path: &Path, _skeletal: bool) -> Self {
        Self { replace_root: true }
    }

    /// Reference placement replaces block 0's local transform regardless of file extension.
    /// Skeletal export still takes bone transforms from the bone tree; static scene nodes use
    /// the same root normalization, including rigid fallback nodes in a skinned NIF.
    /// Collision applies this before composing the target/body frame, so shapes, COM and
    /// inertia all receive the same policy through their existing frame conversion.
    pub(crate) fn replaces_block(self, block: usize) -> bool {
        self.replace_root && block == 0
    }

    pub(crate) fn place_model(self, model: &mut Model) {
        model.scene_root_rotation = Some(shared::coordinates::CREATION_TO_RUNTIME_ROTATION);
        if self.replace_root {
            crate::mesh::clear_root_transform(model);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_zero_placement_is_independent_of_extension_and_skin_declaration() {
        for extension in ["nif", "NIF", "btr", "BTR", "bto", "BTO"] {
            for skeletal in [false, true] {
                let frame = PlacementFrame::new(Path::new(&format!("model.{extension}")), skeletal);
                assert!(frame.replaces_block(0));
                assert!(!frame.replaces_block(1));
            }
        }
    }
}
