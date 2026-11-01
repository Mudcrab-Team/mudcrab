//! The converter's handle on the shared `CoreAuthority` (`shared::cluster_recipe::authority`),
//! opened from the stage settings and the conversion manifest.

use super::{resolve::published_files, scope::request_from_settings, stage::StageSettings};
use crate::cache::ConversionManifest;
use color_eyre::Result;
use rusqlite::Connection;
use shared::cluster_mapping::{AuthoritativeInputs, MaterialSource, ReferenceSnapshot};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub struct CoreAuthority(shared::cluster_recipe::authority::CoreAuthority);

impl CoreAuthority {
    /// Opens the authority over the dataset's world database, the published files under
    /// `assets_root` and the conversion `manifest`, for the scope the run's `settings` and this
    /// database declare.
    pub fn open(
        connection: Connection,
        assets_root: &Path,
        manifest: &ConversionManifest,
        settings: &StageSettings,
        builder_identity: String,
    ) -> Result<Self> {
        shared::cluster_recipe::authority::CoreAuthority::open(
            connection,
            assets_root,
            published_files(manifest),
            &request_from_settings(settings),
            builder_identity,
        )
        .map(Self)
    }
}

impl AuthoritativeInputs for CoreAuthority {
    fn core_build_identity(&self) -> Option<String> {
        self.0.core_build_identity()
    }

    fn builder_identity(&self) -> String {
        self.0.builder_identity()
    }

    fn reference(&self, reference_id: u32) -> Option<ReferenceSnapshot> {
        self.0.reference(reference_id)
    }

    fn classifier_scope(&self) -> String {
        self.0.classifier_scope()
    }

    fn lod_origin(&self, worldspace_id: u32) -> Option<(i32, i32)> {
        self.0.lod_origin(worldspace_id)
    }

    fn model_primitives(&self, model_hash: &str) -> Option<BTreeSet<u32>> {
        self.0.model_primitives(model_hash)
    }

    fn model_materials(&self, model_hash: &str) -> Option<BTreeMap<u32, MaterialSource>> {
        self.0.model_materials(model_hash)
    }

    fn examined_references(&self) -> BTreeSet<u32> {
        self.0.examined_references()
    }
}
