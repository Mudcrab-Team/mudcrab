//! Re-export of the shared recipe (`shared::cluster_recipe::resolve`) plus the converter's
//! adapter from its conversion manifest to the published files the catalog is built from.

use crate::cache::ConversionManifest;
pub use shared::cluster_recipe::resolve::*;

/// The (path, content hash) pairs the manifest lists, which is what a catalog is built from.
pub fn published_files(manifest: &ConversionManifest) -> impl Iterator<Item = (String, String)> {
    manifest
        .entries
        .values()
        .map(|entry| (entry.output.clone(), entry.output_hash.clone()))
}
