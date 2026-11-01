//! Geometry products selected when a converted base was built.
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

/// The geometry contract of a conversion; old manifests describe baked products.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GeometryMode {
    #[default]
    Baked,
    Cluster,
}

fn is_baked(mode: &GeometryMode) -> bool {
    *mode == GeometryMode::Baked
}

/// Flattened into the conversion manifest before the base fingerprint is computed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryManifest {
    #[serde(default, skip_serializing_if = "is_baked")]
    pub geometry_mode: GeometryMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_package: Option<String>,
}

impl GeometryManifest {
    /// Reads just the geometry fields and validates the portable overlay selector.
    /// Overlay membership, fingerprints and certificates are checked by the resolver.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("conversion-manifest.json");
        let manifest: Self = serde_json::from_slice(
            &fs::read(&path).wrap_err_with(|| format!("failed to read {}", path.display()))?,
        )
        .wrap_err("invalid conversion geometry manifest")?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Rejects ambiguous modes and selectors outside the settings-addressed namespace.
    pub fn validate(&self) -> Result<()> {
        match self.geometry_mode {
            GeometryMode::Baked => ensure!(
                self.cluster_package.is_none(),
                "baked geometry must not select a cluster package"
            ),
            GeometryMode::Cluster => {
                let package = self.cluster_package.as_deref().ok_or_else(|| {
                    color_eyre::eyre::eyre!("cluster geometry requires a cluster package")
                })?;
                let prefix = format!(
                    "{}/{}",
                    crate::cluster_overlay::DERIVED_DIR,
                    crate::cluster_overlay::OVERLAY_DIR_PREFIX
                );
                let id = package.strip_prefix(&prefix).unwrap_or("");
                ensure!(
                    id.len() == crate::cluster_overlay::SETTINGS_ID_LEN
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
                    "unsafe cluster package selector: {package}"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_manifest_defaults_to_baked_without_changing_serialized_fields() {
        let geometry: GeometryManifest = serde_json::from_str(r#"{"schema_version":28}"#).unwrap();
        assert_eq!(geometry, GeometryManifest::default());
        assert_eq!(serde_json::to_string(&geometry).unwrap(), "{}");
    }

    #[test]
    fn geometry_contract_rejects_missing_ambiguous_and_unsafe_packages() {
        for package in [
            None,
            Some("default"),
            Some("/derived/clusters-ab"),
            Some("derived/clusters-../bad"),
            Some("derived/clusters-ab/../bad"),
            Some("derived\\clusters-ab"),
            Some("Q:/derived/clusters-ab"),
        ] {
            assert!(
                GeometryManifest {
                    geometry_mode: GeometryMode::Cluster,
                    cluster_package: package.map(str::to_owned)
                }
                .validate()
                .is_err(),
                "{package:?}"
            );
        }
        assert!(
            GeometryManifest {
                geometry_mode: GeometryMode::Baked,
                cluster_package: Some("derived/clusters-ab".into())
            }
            .validate()
            .is_err()
        );
        assert!(
            GeometryManifest {
                geometry_mode: GeometryMode::Cluster,
                cluster_package: Some("derived/clusters-0123456789abcdef".into())
            }
            .validate()
            .is_ok()
        );
    }
}
