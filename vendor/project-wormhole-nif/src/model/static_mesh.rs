use project_wormhole_shared::glam::{self, Mat3, Vec3};

use crate::dev::*;

#[derive(Debug, Clone)]
pub struct StaticMesh {
    pub name: Option<String>,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<glam::Vec2>,
    pub triangles: Vec<glam::u16::U16Vec3>,
    pub colors: Vec<BSVec4>,
}

#[derive(Debug, Clone)]
pub struct StaticSceneNode {
    pub block_index: u32,
    pub name: Option<String>,
    pub translation: Vec3,
    pub rotation: Mat3,
    pub scale: f32,
    pub children: Vec<u32>,
    pub mesh: Option<usize>,
    /// The `NiBillboardNode` mode (nif.xml `BillboardMode`) when this node turns to face the
    /// camera; `None` for an ordinary node.
    pub billboard_mode: Option<u16>,
}

impl StaticSceneNode {
    pub fn validate(&self) -> Result<(), String> {
        if !self.translation.is_finite()
            || !self.rotation.is_finite()
            || !self.scale.is_finite()
            || self.scale == 0.0
        {
            return Err(format!(
                "static scene block {} has an invalid transform: translation={:?}, rotation={:?}, scale={}",
                self.block_index,
                self.translation,
                self.rotation.to_cols_array(),
                self.scale
            ));
        }
        Ok(())
    }
}

impl StaticMesh {
    pub fn validate(&self) -> Result<(), String> {
        if self.positions.is_empty() {
            return Err(format!("StaticMesh [{:?}] has no positions", self.name));
        }
        if self.positions.iter().any(|value| !value.is_finite()) {
            return Err(format!(
                "StaticMesh [{:?}] contains a non-finite position",
                self.name
            ));
        }
        if self.normals.iter().any(|value| !value.is_finite()) {
            return Err(format!(
                "StaticMesh [{:?}] contains a non-finite normal",
                self.name
            ));
        }
        if let Some((index, value)) = self
            .uvs
            .iter()
            .enumerate()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(format!(
                "StaticMesh [{:?}] contains a non-finite UV at vertex {index}: {value:?}",
                self.name,
            ));
        }
        if self.colors.iter().any(|value| !value.0.is_finite()) {
            return Err(format!(
                "StaticMesh [{:?}] contains a non-finite color",
                self.name
            ));
        }
        if self.positions.len() != self.normals.len() && self.normals.len() != 0 {
            let msg = format!("StaticMesh [{:?}]: Number of positions [{}] does not match numbers of normals [{}]", self.name, self.positions.len(), self.normals.len());
            debug!("{}", msg);
            return Err(msg);
        }
        if self.positions.len() != self.uvs.len() && self.uvs.len() != 0 {
            let msg = format!(
                "StaticMesh [{:?}]: Number of positions [{}] does not match numbers of uvs [{}]",
                self.name,
                self.positions.len(),
                self.uvs.len()
            );
            debug!("{}", msg);
            return Err(msg);
        }
        if self.positions.len() != self.colors.len() && self.colors.len() != 0 {
            let msg = format!(
                "StaticMesh [{:?}]: Number of positions [{}] does not match numbers of colors [{}]",
                self.name,
                self.positions.len(),
                self.colors.len()
            );
            debug!("{}", msg);
            return Err(msg);
        }
        if self.triangles.iter().any(|triangle| {
            usize::from(triangle.x) >= self.positions.len()
                || usize::from(triangle.y) >= self.positions.len()
                || usize::from(triangle.z) >= self.positions.len()
        }) {
            return Err(format!(
                "StaticMesh [{:?}] contains an out-of-range triangle index",
                self.name
            ));
        }
        Ok(())
    }

    pub fn positions_as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for pos in &self.positions {
            bytes.extend(pos.x.to_le_bytes().as_slice());
            bytes.extend(pos.y.to_le_bytes().as_slice());
            bytes.extend(pos.z.to_le_bytes().as_slice());
        }
        bytes
    }

    pub fn positions_min_max(&self) -> (Vec3, Vec3) {
        let mut min = Vec3::new(f32::MAX, f32::MAX, f32::MAX);
        let mut max = Vec3::new(f32::MIN, f32::MIN, f32::MIN);
        for pos in &self.positions {
            min.x = min.x.min(pos.x);
            min.y = min.y.min(pos.y);
            min.z = min.z.min(pos.z);
            max.x = max.x.max(pos.x);
            max.y = max.y.max(pos.y);
            max.z = max.z.max(pos.z);
        }
        (min, max)
    }

    pub fn normals_as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for normal in &self.normals {
            bytes.extend(normal.x.to_le_bytes().as_slice());
            bytes.extend(normal.y.to_le_bytes().as_slice());
            bytes.extend(normal.z.to_le_bytes().as_slice());
        }
        bytes
    }

    pub fn uvs_as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for uv in &self.uvs {
            bytes.extend(uv.x.to_le_bytes().as_slice());
            bytes.extend(uv.y.to_le_bytes().as_slice());
        }
        bytes
    }

    pub fn triangles_as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for tri in &self.triangles {
            bytes.extend(tri.x.to_le_bytes().as_slice());
            bytes.extend(tri.y.to_le_bytes().as_slice());
            bytes.extend(tri.z.to_le_bytes().as_slice());
        }
        bytes
    }

    pub fn colors_as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for color in &self.colors {
            bytes.extend(color.0.x.to_le_bytes().as_slice());
            bytes.extend(color.0.y.to_le_bytes().as_slice());
            bytes.extend(color.0.z.to_le_bytes().as_slice());
            bytes.extend(color.0.w.to_le_bytes().as_slice());
        }
        bytes
    }
}

impl Default for StaticMesh {
    fn default() -> Self {
        Self {
            name: None,
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            triangles: Vec::new(),
            colors: Vec::new(),
        }
    }
}
