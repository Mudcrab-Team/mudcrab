//! The builder identity of the cluster stage (ADR-0013 section 1).

use super::builder::builder_config_json;
use crate::cache::CONVERTER_SCHEMA_VERSION;
use shared::cluster::BuilderIdentityInputs;

/// SHA-256 of `vendor/meshoptimizer/demo/clusterlod.h`, computed by `build.rs`.
pub const CLUSTERLOD_SOURCE_SHA256: &str = env!("CLUSTERLOD_SOURCE_SHA256");
/// SHA-256 over the sorted sources of `vendor/meshoptimizer/src`, computed by `build.rs`.
pub const MESHOPTIMIZER_SOURCES_SHA256: &str = env!("MESHOPTIMIZER_SOURCES_SHA256");
/// SHA-256 of the C wrapper around clusterlod, computed by `build.rs`.
pub const MLC_WRAPPER_SHA256: &str = env!("MLC_WRAPPER_SHA256");
/// SHA-256 of the Rust sources that read, classify and encode geometry, computed by `build.rs`.
pub const CLUSTER_RUST_SOURCES_SHA256: &str = env!("CLUSTER_RUST_SOURCES_SHA256");

/// The identity inputs for a build with `triangle_threshold`. A change in any vendored source,
/// builder setting, vertex layout, eligibility rule, threshold or converter schema changes the
/// identity and so rebuilds every payload; plugins are deliberately absent.
pub fn identity_inputs(triangle_threshold: u32) -> BuilderIdentityInputs {
    BuilderIdentityInputs {
        clusterlod_source: CLUSTERLOD_SOURCE_SHA256.to_owned(),
        meshoptimizer: MESHOPTIMIZER_SOURCES_SHA256.to_owned(),
        builder_config: builder_config_json(MLC_WRAPPER_SHA256, CLUSTER_RUST_SOURCES_SHA256),
        triangle_threshold,
        converter_schema: CONVERTER_SCHEMA_VERSION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vendored_hashes_are_sha256_hex() {
        for hash in [
            CLUSTERLOD_SOURCE_SHA256,
            MESHOPTIMIZER_SOURCES_SHA256,
            MLC_WRAPPER_SHA256,
            CLUSTER_RUST_SOURCES_SHA256,
        ] {
            assert_eq!(hash.len(), 64);
            assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn the_threshold_is_part_of_the_identity() {
        assert_ne!(
            identity_inputs(1024).identity(),
            identity_inputs(512).identity()
        );
        assert_eq!(
            identity_inputs(1024).identity(),
            identity_inputs(1024).identity()
        );
    }

    #[test]
    fn shared_drawability_and_scope_sources_invalidate_builder_identity() {
        let listed: Vec<_> = include_str!("../../cluster-identity-sources.txt")
            .lines()
            .map(str::trim)
            .collect();
        for source in [
            "../shared/src/cluster_recipe/classify.rs",
            "../shared/src/cluster_recipe/scope.rs",
        ] {
            assert!(
                listed.contains(&source),
                "drawability contract source {source} must invalidate cluster identity"
            );
        }
    }

    #[test]
    fn stock_material_contract_sources_invalidate_overlay_identity() {
        let listed: Vec<_> = include_str!("../../cluster-identity-sources.txt")
            .lines()
            .map(str::trim)
            .collect();
        assert!(
            listed.iter().all(|path| !path.starts_with("../engine/")),
            "converter certification depends only on shared semantics, never engine implementation files"
        );
        // build.rs hashes the contents of every listed source and watches each path.
        // Requiring the stock resolver here prevents a material-only edit from reusing
        // an overlay certified against the previous material contract.
        for source in [
            "src/material.rs",
            "../shared/src/material_depth.rs",
            "../shared/src/cluster_material.rs",
        ] {
            assert!(
                listed.contains(&source),
                "material contract source {source} must invalidate cluster identity"
            );
        }
    }

    #[test]
    fn terrain_recipe_and_encoding_sources_invalidate_builder_identity() {
        let listed: Vec<_> = include_str!("../../cluster-identity-sources.txt")
            .lines()
            .map(str::trim)
            .collect();
        for source in [
            "../shared/src/lod.rs",
            "../shared/src/terrain.rs",
            "src/texture.rs",
            "src/texture_ktx2.rs",
            "src/basis_ktx2_bridge.cpp",
            "src/durability.rs",
        ] {
            assert!(
                listed.contains(&source),
                "terrain dependency {source} must invalidate cluster identity"
            );
        }
    }

    /// The modules of `src/cluster` that are not part of the identity, each with its reason.
    const EXEMPT: [(&str, &str); 5] = [
        (
            "mod.rs",
            "module list, settings plumbing and the pipeline hook",
        ),
        (
            "identity.rs",
            "this file derives the identity from the hashes",
        ),
        ("probe.rs", "the GPU probe, a run record"),
        ("report.rs", "the run report"),
        ("test_support.rs", "test builders"),
    ];

    #[test]
    fn every_cluster_module_is_in_the_identity_or_exempt() {
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let listed: Vec<&str> = include_str!("../../cluster-identity-sources.txt")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        for path in &listed {
            assert!(
                crate_dir.join(path).is_file(),
                "{path} is listed but missing"
            );
        }
        for entry in std::fs::read_dir(crate_dir.join("src/cluster")).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if !name.ends_with(".rs") {
                continue;
            }
            let listed_here = listed.contains(&format!("src/cluster/{name}").as_str());
            let exempt = EXEMPT.iter().any(|(exempt, _)| *exempt == name);
            assert!(
                listed_here ^ exempt,
                "{name} must be in cluster-identity-sources.txt or exempt (not both): a source that                  decides payloads, rejects, mapping or provenance has to change the builder identity"
            );
        }
    }

    #[test]
    fn every_shared_recipe_module_is_in_the_identity() {
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let listed = include_str!("../../cluster-identity-sources.txt");
        let recipe = crate_dir.join("../shared/src/cluster_recipe");
        let mut seen = 0;
        for entry in std::fs::read_dir(&recipe).expect("read shared cluster_recipe") {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            seen += 1;
            assert!(
                listed
                    .lines()
                    .any(|line| line.trim() == format!("../shared/src/cluster_recipe/{name}")),
                "shared/src/cluster_recipe/{name} decides the mapping and must be in cluster-identity-sources.txt"
            );
        }
        assert!(seen >= 7);
    }
}
