//! Launch-time geometry selection. The conversion owns its required renderer inputs;
//! explicit legacy overlay selection remains available for baked conversions.

use crate::config::EngineConfig;
use color_eyre::eyre::{Result, WrapErr, ensure};
use shared::geometry::{GeometryManifest, GeometryMode};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Resolve before opening runtime sources or installing streaming plugins. This only
/// selects the package; its existing asynchronous certificate still authorizes every read.
pub(crate) fn prepare(config: &mut EngineConfig) -> Result<Option<PathBuf>> {
    // Incomplete legacy fixtures may deliberately have no producer manifest. A present
    // manifest is always checked, including with --allow-incomplete-assets.
    let manifest = if config.allow_incomplete_assets
        && !config.assets_dir.join("conversion-manifest.json").exists()
    {
        GeometryManifest::default()
    } else {
        GeometryManifest::load(&config.assets_dir)?
    };
    let recorded = if manifest.geometry_mode == GeometryMode::Cluster {
        let package = manifest.cluster_package.as_deref().ok_or_else(|| {
            color_eyre::eyre::eyre!("cluster conversion requires its recorded cluster package")
        })?;
        // The manifest owns an exact path, never a label search. Even an absolute selector
        // could otherwise match another overlay's label before the resolver tries the path.
        let required = fs::canonicalize(&config.assets_dir)
            .wrap_err("cannot resolve the converted asset root")?
            .join(package);
        shared::cluster_overlay::read_metadata(&required)
            .wrap_err("recorded cluster package is unavailable")?;
        if let Some(selector) = config
            .clusters
            .as_deref()
            .filter(|selector| !selector.trim().is_empty())
        {
            let selector = selector.trim();
            let explicit = if selector == package {
                required.clone()
            } else if Path::new(selector).is_absolute() {
                PathBuf::from(selector)
            } else {
                shared::cluster_overlay::resolve_overlay(&config.assets_dir, selector)
                    .wrap_err("--clusters")?
            };
            ensure!(
                fs::canonicalize(explicit)? == fs::canonicalize(&required)?,
                "--clusters conflicts with the conversion's required package {package}"
            );
        }
        config.clusters = Some(package.to_owned());
        Some(required)
    } else {
        config
            .clusters
            .as_deref()
            .map(|selector| {
                shared::cluster_overlay::resolve_overlay(&config.assets_dir, selector)
                    .wrap_err("--clusters")
            })
            .transpose()?
    };
    if let Some(overlay) = &recorded {
        let metadata = shared::cluster_overlay::read_metadata(overlay)?;
        ensure!(
            metadata.build_settings.terrain && metadata.build_settings.include_trees,
            "cluster mode requires a package built with terrain and trees; rebuild with --terrain \
             --include-trees, or reconvert with --lod-mode cluster"
        );
        ensure!(
            metadata
                .extensions
                .iter()
                .any(|entry| entry.id == shared::extensions::CLUSTER_TERRAIN),
            "cluster mode requires the cluster terrain extension; {}",
            metadata.rebuild_hint(&config.assets_dir)
        );
        config.cluster_terrain = true;
        config.cluster_own_statics = true;
    }
    Ok(recorded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{
        cluster_overlay::*,
        extensions::{ExtensionEntry, ExtensionOwns},
    };

    // Selection fixture only: no payload certificate, renderer, baked tables or files.
    fn fixture(terrain: bool, trees: bool) -> (tempfile::TempDir, String) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("OWNER.txt"), "engine geometry selection fixture").unwrap();
        rusqlite::Connection::open(root.join("skyrim_world.db")).unwrap();
        let settings = ResolvedBuildSettings::new("fixture", "ab".repeat(32), 1, terrain, trees);
        let package = format!("derived/clusters-{}", settings.settings_id());
        let overlay = root.join(&package);
        fs::create_dir_all(&overlay).unwrap();
        for path in [root.join("derived"), overlay.clone()] {
            fs::write(path.join("OWNER.txt"), "engine geometry selection fixture").unwrap();
        }
        let metadata = OverlayMetadata {
            schema_version: OVERLAY_SCHEMA_VERSION,
            kind: OVERLAY_KIND.into(),
            label: "fixture".into(),
            settings_id: settings.settings_id(),
            build_settings: settings,
            terrain_sources: None,
            base_core_identity: "cd".repeat(32),
            base_fingerprint: "ef".repeat(32),
            base_failure_count: 0,
            builder_identity: "ab".repeat(32),
            created: "fixture".into(),
            owner: "engine geometry selection fixture".into(),
            extensions: std::iter::once(shared::extensions::CLUSTERS)
                .chain(terrain.then_some(shared::extensions::CLUSTER_TERRAIN))
                .map(|id| ExtensionEntry {
                    id: id.into(),
                    version: 1,
                    owns: ExtensionOwns::default(),
                    sha256: "ab".repeat(32),
                    built_from: "cd".repeat(32),
                    stats: Default::default(),
                })
                .collect(),
        };
        fs::write(
            overlay.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        write_manifest(
            root,
            serde_json::json!({"geometry_mode": "cluster", "cluster_package": package}),
        );
        (directory, package)
    }

    fn write_manifest(root: &Path, manifest: serde_json::Value) {
        fs::write(root.join("conversion-manifest.json"), manifest.to_string()).unwrap();
    }

    #[test]
    fn direct_startup_selects_recorded_geometry_without_baked_sources() {
        let (directory, package) = fixture(true, true);
        let mut config = EngineConfig {
            assets_dir: directory.path().into(),
            ..Default::default()
        };
        assert_eq!(
            prepare(&mut config).unwrap(),
            Some(fs::canonicalize(directory.path()).unwrap().join(&package))
        );
        assert_eq!(config.clusters.as_deref(), Some(package.as_str()));
        assert!(config.cluster_terrain && config.cluster_own_statics);
    }

    #[test]
    fn explicit_matching_selector_is_accepted_but_different_overlay_is_rejected() {
        let (directory, package) = fixture(true, true);
        let mut config = EngineConfig {
            assets_dir: directory.path().into(),
            clusters: Some("fixture".into()),
            ..Default::default()
        };
        prepare(&mut config).unwrap();
        let other_settings = ResolvedBuildSettings::new("fixture", "ab".repeat(32), 2, true, true);
        let other = overlay_dir(directory.path(), &other_settings.settings_id());
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("OWNER.txt"), "engine geometry selection fixture").unwrap();
        let mut metadata = read_metadata(&directory.path().join(package)).unwrap();
        metadata.settings_id = other_settings.settings_id();
        metadata.build_settings = other_settings;
        metadata.label = "other".into();
        fs::write(
            other.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        config.clusters = Some("other".into());
        let error = prepare(&mut config).unwrap_err().to_string();
        assert!(error.contains("conflicts"), "{error}");
    }

    #[test]
    fn recorded_package_cannot_be_shadowed_by_an_overlay_label() {
        let (directory, package) = fixture(true, true);
        let root = directory.path();
        let required = fs::canonicalize(root).unwrap().join(&package);
        let other_settings = ResolvedBuildSettings::new("fixture", "ab".repeat(32), 2, true, true);
        let other = overlay_dir(root, &other_settings.settings_id());
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("OWNER.txt"), "engine geometry selection fixture").unwrap();
        let mut metadata = read_metadata(&required).unwrap();
        metadata.settings_id = other_settings.settings_id();
        metadata.build_settings = other_settings;
        metadata.label = required.to_string_lossy().into_owned();
        fs::write(
            other.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        for selector in [
            String::new(),
            package,
            required.to_string_lossy().into_owned(),
        ] {
            let mut config = EngineConfig {
                assets_dir: root.into(),
                clusters: Some(selector),
                ..Default::default()
            };
            assert_eq!(prepare(&mut config).unwrap(), Some(required.clone()));
        }
    }

    #[test]
    fn missing_required_package_terrain_and_trees_are_rejected() {
        for (terrain, trees) in [(false, true), (true, false)] {
            let (directory, _) = fixture(terrain, trees);
            let mut config = EngineConfig {
                assets_dir: directory.path().into(),
                ..Default::default()
            };
            assert!(
                prepare(&mut config)
                    .unwrap_err()
                    .to_string()
                    .contains("terrain and trees")
            );
        }
        let (directory, package) = fixture(true, true);
        let mut config = EngineConfig {
            assets_dir: directory.path().into(),
            ..Default::default()
        };
        fs::remove_file(directory.path().join(package).join(OVERLAY_METADATA_FILE)).unwrap();
        assert!(prepare(&mut config).is_err());
    }

    #[test]
    fn missing_required_terrain_extension_is_rejected_even_with_complete_settings() {
        let (directory, package) = fixture(true, true);
        let overlay = directory.path().join(package);
        let mut metadata = read_metadata(&overlay).unwrap();
        metadata
            .extensions
            .retain(|entry| entry.id != shared::extensions::CLUSTER_TERRAIN);
        fs::write(
            overlay.join(OVERLAY_METADATA_FILE),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        let mut config = EngineConfig {
            assets_dir: directory.path().into(),
            ..Default::default()
        };
        let error = format!("{:#}", prepare(&mut config).unwrap_err());
        assert!(error.contains("terrain extension"), "{error}");
    }

    #[test]
    fn legacy_baked_and_explicit_legacy_overlay_keep_their_selection() {
        let (directory, _) = fixture(true, true);
        write_manifest(directory.path(), serde_json::json!({"schema_version": 1}));
        let mut config = EngineConfig {
            assets_dir: directory.path().into(),
            ..Default::default()
        };
        assert!(prepare(&mut config).unwrap().is_none());
        assert!(config.clusters.is_none() && !config.cluster_terrain);
        config.clusters = Some("fixture".into());
        assert!(prepare(&mut config).unwrap().is_some());
        assert!(config.cluster_terrain);
        assert!(config.cluster_own_statics);
    }

    #[test]
    fn legacy_object_only_overlay_is_rejected_instead_of_losing_distant_geometry() {
        let (directory, _) = fixture(false, false);
        write_manifest(directory.path(), serde_json::json!({"schema_version": 1}));
        let mut config = EngineConfig {
            assets_dir: directory.path().into(),
            clusters: Some("fixture".into()),
            ..Default::default()
        };
        let error = prepare(&mut config).unwrap_err().to_string();
        assert!(
            error.contains("terrain and trees") && error.contains("rebuild"),
            "{error}"
        );
    }

    #[test]
    fn incomplete_override_cannot_hide_a_malformed_geometry_manifest() {
        let (directory, _) = fixture(true, true);
        write_manifest(
            directory.path(),
            serde_json::json!({"geometry_mode": "cluster"}),
        );
        let mut config = EngineConfig {
            assets_dir: directory.path().into(),
            allow_incomplete_assets: true,
            ..Default::default()
        };
        assert!(prepare(&mut config).is_err());
    }
}
