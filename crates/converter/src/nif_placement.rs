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

    fn new(path: &Path, skeletal: bool) -> Self {
        let distant_lod = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("btr") || extension.eq_ignore_ascii_case("bto")
            });
        Self {
            replace_root: !distant_lod && !skeletal,
        }
    }

    /// A reference replaces block 0's local transform in an ordinary static scene.
    /// LOD uses a grid placement; skeletal export takes its transforms from the bone tree.
    /// Collision applies this before composing the target/body frame, so shapes, COM and
    /// inertia all receive the same policy through their existing frame conversion.
    pub(crate) fn replaces_block(self, block: usize) -> bool {
        self.replace_root && block == 0
    }

    pub(crate) fn place_model(self, model: &mut Model) {
        model.scene_root_rotation = Some(shared::coordinates::CREATION_TO_RUNTIME_ROTATION);
        for root in &mut model.static_nodes {
            if self.replaces_block(root.block_index as usize) {
                root.translation = Default::default();
                root.rotation = Default::default();
                root.scale = 1.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ordinary_static_block_zero_is_replaced() {
        for extension in ["nif", "NIF", "btr", "BTR", "bto", "BTO"] {
            for skeletal in [false, true] {
                let frame = PlacementFrame::new(Path::new(&format!("model.{extension}")), skeletal);
                let lod = !extension.eq_ignore_ascii_case("nif");
                assert_eq!(frame.replaces_block(0), !lod && !skeletal);
                assert!(!frame.replaces_block(1));
            }
        }
    }
}
