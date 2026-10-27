use project_wormhole_shared::glam::{self, Mat3, Vec3};

use crate::dev::*;

#[derive(Debug, Clone)]
pub struct SkeletalMesh {
    pub name: Option<String>,
    /// Block index of the shape this mesh was built from. The exporter records
    /// it so a consumer can associate the mesh with its source block instead of
    /// guessing from shape-type enumeration order.
    pub source_block: Option<u32>,
    pub mesh: Option<super::all::StaticMesh>,
    pub weights: Vec<glam::Vec4>,
    pub joints: Vec<glam::u8::U8Vec4>,
    pub bones: BoneTree,
    pub skin: Vec<String>,
    pub inverse_bind_matrices: Vec<BSMatrix4>,
    /// Per-vertex position and normal deltas applied on top of `mesh`.
    ///
    /// Each target must carry one entry per vertex. The deltas are already in
    /// the mesh's coordinate space; the writer emits them without transforming
    /// or re-encoding them.
    pub morph_targets: Vec<MorphTarget>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MorphTarget {
    pub name: String,
    pub positions: Vec<glam::Vec3>,
    pub normals: Vec<glam::Vec3>,
}

impl SkeletalMesh {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(mesh) = &self.mesh {
            mesh.validate()?;

            if mesh.positions.len() != self.weights.len() {
                let msg = format!(
                    "SkeletalMesh: Number of positions [{}] does not match numbers of weights [{}]",
                    mesh.positions.len(),
                    self.weights.len()
                );
                debug!("{}", msg);
                return Err(msg);
            }

            if mesh.positions.len() != self.joints.len() {
                let msg = format!(
                    "SkeletalMesh: Number of positions [{}] does not match numbers of joints [{}]",
                    mesh.positions.len(),
                    self.joints.len()
                );
                debug!("{}", msg);
                return Err(msg);
            }
        }

        Ok(())
    }

    pub fn weights_as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for weight in &self.weights {
            bytes.extend(weight.x.to_le_bytes().as_slice());
            bytes.extend(weight.y.to_le_bytes().as_slice());
            bytes.extend(weight.z.to_le_bytes().as_slice());
            bytes.extend(weight.w.to_le_bytes().as_slice());
        }
        bytes
    }

    pub fn joints_as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for joint in &self.joints {
            bytes.extend(joint.x.to_le_bytes().as_slice());
            bytes.extend(joint.y.to_le_bytes().as_slice());
            bytes.extend(joint.z.to_le_bytes().as_slice());
            bytes.extend(joint.w.to_le_bytes().as_slice());
        }
        bytes
    }

    pub fn inverse_bind_matrices_as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for mat in &self.inverse_bind_matrices {
            for val in &mat.0.to_cols_array() {
                bytes.extend(val.to_le_bytes().as_slice());
            }
        }
        bytes
    }
}

impl Default for SkeletalMesh {
    fn default() -> Self {
        Self {
            name: None,
            source_block: None,
            mesh: None,
            weights: Vec::new(),
            joints: Vec::new(),
            bones: BoneTree::new(),
            skin: Vec::new(),
            inverse_bind_matrices: Vec::new(),
            morph_targets: Vec::new(),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct BoneTree {
    pub bones: BTreeMap<String, BoneData>,
}

impl Default for BoneTree {
    fn default() -> Self {
        Self::new()
    }
}

impl BoneTree {
    pub fn new() -> Self {
        BoneTree {
            bones: BTreeMap::new(),
        }
    }

    pub fn has_bone(&self, name: &str) -> bool {
        self.bones.contains_key(name)
    }

    pub fn add_bone(&mut self, name: &str, bone_data: BoneData) -> Result<(), String> {
        if self.has_bone(name) {
            return Err(format!("Bone {} already exists", name));
        }

        self.bones.insert(name.to_string(), bone_data);
        Ok(())
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct BoneData {
    pub transform: BoneTransform,
    pub children: HashSet<String>,
}

#[derive(Debug, PartialEq, Clone, Default)]
pub struct BoneTransform {
    translation: Option<BSVec3>,
    rotation: Option<BSMatrix3>,
    scale: Option<f32>,
}

impl BoneTransform {
    pub fn new(
        translation: Option<BSVec3>,
        rotation: Option<BSMatrix3>,
        scale: Option<f32>,
    ) -> Self {
        // Check if the values are default and set them to None
        let checked_translation = if translation
            .as_ref()
            .is_some_and(|x| x == &BSVec3(Vec3::ZERO))
        {
            None
        } else {
            translation
        };

        // Check if the values are default and set them to None
        let checked_rotation = if rotation
            .as_ref()
            .is_some_and(|x| x == &BSMatrix3(Mat3::IDENTITY))
        {
            None
        } else {
            rotation
        };

        // Check if the values are default and set them to None
        let checked_scale = if scale.as_ref().is_some_and(|x| *x == 1.0) {
            None
        } else {
            scale
        };

        BoneTransform {
            translation: checked_translation,
            rotation: checked_rotation,
            scale: checked_scale,
        }
    }

    pub fn get_translation(&self) -> Option<&BSVec3> {
        self.translation.as_ref()
    }

    pub fn get_rotation(&self) -> Option<&BSMatrix3> {
        self.rotation.as_ref()
    }

    pub fn get_scale(&self) -> Option<f32> {
        self.scale
    }
}
