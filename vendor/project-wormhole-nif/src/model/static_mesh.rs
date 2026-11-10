use project_wormhole_shared::glam::{self, Mat3, Vec3};

use crate::dev::*;

#[derive(Debug, Clone)]
pub struct StaticMesh {
    pub name: Option<String>,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    /// Native UV-derived tangent direction and handedness; empty when unavailable.
    pub tangents: Vec<glam::Vec4>,
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
            let msg = format!(
                "StaticMesh [{:?}]: Number of positions [{}] does not match numbers of normals [{}]",
                self.name,
                self.positions.len(),
                self.normals.len()
            );
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

    /// Build an area-weighted UV frame from retained geometry and authored normals.
    /// Packed NIF axis labels do not establish their relationship to texture U/V.
    /// Match the native Bevy convention: B is opposite increasing texture V;
    /// the existing DirectX material tag applies the corresponding Y decode.
    pub fn rebuild_uv_tangents(&mut self) {
        self.tangents.clear();
        let count = self.positions.len();
        if count == 0 || self.normals.len() != count || self.uvs.len() != count {
            return;
        }
        let mut along_u = vec![Vec3::ZERO; count];
        let mut along_v = vec![Vec3::ZERO; count];
        for triangle in &self.triangles {
            let ids = [
                triangle.x as usize,
                triangle.y as usize,
                triangle.z as usize,
            ];
            if ids.iter().any(|&index| index >= count) {
                continue;
            }
            let [a, b, c] = ids;
            let p = self.positions[b] - self.positions[a];
            let q = self.positions[c] - self.positions[a];
            let u = self.uvs[b] - self.uvs[a];
            let v = self.uvs[c] - self.uvs[a];
            let det = u.x * v.y - u.y * v.x;
            let area = p.cross(q).length();
            if !det.is_finite() || det.abs() < 1e-8 || !area.is_finite() || area <= 0.0 {
                continue;
            }
            let tangent = (p * v.y - q * u.y) * (area / det);
            let bitangent = (q * u.x - p * v.x) * (area / det);
            for index in ids {
                along_u[index] += tangent;
                along_v[index] += bitangent;
            }
        }
        let frames: Option<Vec<_>> = (0..count)
            .map(|index| {
                let normal = self.normals[index].try_normalize()?;
                let tangent = along_u[index];
                let tangent = (tangent - normal * normal.dot(tangent)).try_normalize()?;
                let orientation = normal.cross(tangent).dot(along_v[index]);
                if !orientation.is_finite() || orientation.abs() < 1e-8 {
                    return None;
                }
                Some(tangent.extend(if orientation < 0.0 { 1.0 } else { -1.0 }))
            })
            .collect();
        if let Some(frames) = frames {
            self.tangents = frames;
        } else {
            log::warn!(
                "NIF shape {:?}: omitted degenerate optional UV tangent frame",
                self.name
            );
        }
    }

    pub fn tangents_as_bytes(&self) -> Vec<u8> {
        self.tangents
            .iter()
            .flat_map(|t| t.to_array().into_iter().flat_map(f32::to_le_bytes))
            .collect()
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
            tangents: Vec::new(),
            uvs: Vec::new(),
            triangles: Vec::new(),
            colors: Vec::new(),
        }
    }
}
