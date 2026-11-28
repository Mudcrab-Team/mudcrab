//! The authored NIF material values that glTF cannot hold exactly.
//!
//! The converter publishes one [`MaterialValues`] per lighting material, in the GLB at
//! `materials[].extras.openSkyrim.materialValues`. A pack converted before that field existed
//! takes the same values from [`MATERIAL_VALUES_FILE`] at its root, which
//! `converter derive-material-values` writes from the source NIFs. The engine reads the GLB
//! field first.
//!
//! Every value is the float the NIF stores. A value that no NIF supplies is absent, never filled
//! in.

use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The sidecar's name at the root of a pack.
pub const MATERIAL_VALUES_FILE: &str = "material-values.json";
/// The format name the sidecar states.
pub const MATERIAL_VALUES_FORMAT: &str = "mudcrab-material-values/1";

/// The values of one `BSLightingShaderProperty`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterialValues {
    /// Specular colour, unscaled and unclamped.
    pub specular_color: [f32; 3],
    /// Specular strength, unclamped.
    pub specular_strength: f32,
    /// Glossiness, the specular exponent.
    pub glossiness: f32,
    /// The environment map scale of an environment or eye shader. Other shaders have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_map_scale: Option<f32>,
}

impl MaterialValues {
    /// Whether every number is finite.
    pub fn is_finite(&self) -> bool {
        self.specular_color
            .into_iter()
            .chain([self.specular_strength, self.glossiness])
            .chain(self.environment_map_scale)
            .all(f32::is_finite)
    }
}

/// The sidecar: GLB path in the pack, then shader block index of the source NIF.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MaterialValuesFile {
    pub format: String,
    pub meshes: BTreeMap<String, BTreeMap<u32, MaterialValues>>,
}

impl MaterialValuesFile {
    /// An empty sidecar of the current format.
    pub fn new() -> Self {
        Self {
            format: MATERIAL_VALUES_FORMAT.to_owned(),
            meshes: BTreeMap::new(),
        }
    }

    /// Reads a sidecar and refuses another format.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let file: Self =
            serde_json::from_slice(bytes).wrap_err("the material values file is not valid")?;
        ensure!(
            file.format == MATERIAL_VALUES_FORMAT,
            "the material values file has format {:?}, this build reads {MATERIAL_VALUES_FORMAT:?}",
            file.format
        );
        Ok(file)
    }

    /// The key of a GLB path: lowercase with forward slashes, as the converter writes it.
    pub fn key(mesh: &str) -> String {
        mesh.replace('\\', "/").to_ascii_lowercase()
    }

    /// The values of shader block `shader_block` of the GLB at `mesh`.
    pub fn get(&self, mesh: &str, shader_block: u32) -> Option<MaterialValues> {
        self.meshes
            .get(&Self::key(mesh))?
            .get(&shader_block)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sidecar_round_trips_and_looks_up_by_normalised_path() {
        let values = MaterialValues {
            specular_color: [1.5, 0.5, 0.0],
            specular_strength: 7.0,
            glossiness: 80.0,
            environment_map_scale: Some(0.25),
        };
        let mut file = MaterialValuesFile::new();
        file.meshes
            .entry("meshes/a/b.glb".to_owned())
            .or_default()
            .insert(12, values);
        let read = MaterialValuesFile::parse(&serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(read, file);
        assert_eq!(read.get("Meshes\\A\\b.glb", 12), Some(values));
        assert_eq!(read.get("meshes/a/b.glb", 13), None);
        assert_eq!(read.get("meshes/a/c.glb", 12), None);
    }

    #[test]
    fn an_absent_environment_scale_stays_absent() {
        let text = serde_json::json!({"specularColor": [1.0, 1.0, 1.0],
            "specularStrength": 1.0, "glossiness": 30.0});
        let values: MaterialValues = serde_json::from_value(text.clone()).unwrap();
        assert_eq!(values.environment_map_scale, None);
        assert_eq!(serde_json::to_value(values).unwrap(), text);
        assert!(values.is_finite());
        let infinite = MaterialValues {
            glossiness: f32::INFINITY,
            ..values
        };
        assert!(!infinite.is_finite());
    }

    #[test]
    fn another_format_is_refused() {
        let bytes = br#"{"format":"mudcrab-material-values/2","meshes":{}}"#;
        assert!(MaterialValuesFile::parse(bytes).is_err());
        assert!(MaterialValuesFile::parse(b"{}").is_err());
    }
}
