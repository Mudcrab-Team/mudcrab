//! Verified terrain reuse from a prior package held under an asset lock.

use super::terrain::{TerrainChunk, TerrainChunkInput};
use crate::{
    asset_path::resolve_asset_uri,
    cache::{CONVERTER_SCHEMA_VERSION, ConversionManifest, hash_bytes},
};
use color_eyre::{Result, eyre::ensure};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use shared::lod::LodOrigin;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// The published `lod-manifest.json`: the identity the runtime checks before
/// trusting any chunk row or payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LodManifest {
    pub build_identity: String,
    pub converter_schema: u32,
    pub world_database_schema: u32,
    pub chunks: u64,
    #[serde(default)]
    pub land_texture_repeats_per_cell: f32,
    pub terrain_sources: BTreeMap<String, String>,
    #[serde(default)]
    pub compiler_version: u32,
    #[serde(default)]
    pub chunk_inputs: BTreeMap<String, String>,
}

/// Prior-package proof is optional. Legacy, incomplete or damaged proof is a
/// cache miss, while the current source/compiler/publication checks remain fatal.
pub(crate) struct LodReuse {
    root: PathBuf,
    manifest: LodManifest,
    chunks: BTreeMap<(u32, i32, i32, i32), CachedLodChunk>,
}

struct CachedLodChunk {
    path: String,
    hash: String,
    min: [f32; 3],
    max: [f32; 3],
    source_cells: String,
}

impl LodReuse {
    pub(crate) fn open(root: &Path) -> Result<Self> {
        let conversion: ConversionManifest =
            serde_json::from_slice(&fs::read(root.join("conversion-manifest.json"))?)?;
        ensure!(
            conversion.complete
                && conversion.failures.is_empty()
                && conversion.schema_version == CONVERTER_SCHEMA_VERSION,
            "LOD reuse requires a complete current producer package"
        );
        let manifest: LodManifest =
            serde_json::from_slice(&fs::read(root.join("lod-manifest.json"))?)?;
        ensure!(
            manifest.compiler_version == crate::lod::TERRAIN_COMPILER_VERSION,
            "LOD compiler identity changed"
        );
        shared::world_assets::validate_lod_build_contract(root, CONVERTER_SCHEMA_VERSION)?;
        let connection = Connection::open_with_flags(
            root.join("skyrim_world.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let chunks = connection.prepare("SELECT worldspace_id, tier, anchor_x, anchor_y, payload_path, content_hash, bounds_min_x, bounds_min_y, bounds_min_z, bounds_max_x, bounds_max_y, bounds_max_z, source_cells FROM lod_chunks")?
            .query_map([], |row| Ok(((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?), CachedLodChunk {
                path: row.get(4)?, hash: row.get(5)?, min: [row.get(6)?, row.get(7)?, row.get(8)?], max: [row.get(9)?, row.get(10)?, row.get(11)?], source_cells: row.get(12)?,
            })))?.collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(Self {
            root: root.to_owned(),
            manifest,
            chunks,
        })
    }

    pub(crate) fn chunk(
        &self,
        job: &TerrainChunkInput<'_>,
        origin: LodOrigin,
        input_hash: &str,
    ) -> Option<TerrainChunk> {
        self.checked_chunk(job, origin, input_hash).ok()
    }

    fn checked_chunk(
        &self,
        job: &TerrainChunkInput<'_>,
        origin: LodOrigin,
        input_hash: &str,
    ) -> Result<TerrainChunk> {
        let relative = shared::lod::chunk_payload_path(job.key);
        ensure!(
            self.manifest
                .chunk_inputs
                .get(&relative)
                .is_some_and(|hash| hash == input_hash),
            "LOD chunk inputs changed or lack proof"
        );
        let cached = self
            .chunks
            .get(&(
                job.key.worldspace_id,
                job.key.tier.side_cells(),
                job.key.anchor.x,
                job.key.anchor.y,
            ))
            .ok_or_else(|| color_eyre::eyre::eyre!("cached LOD chunk row missing"))?;
        let cells: Vec<_> = job
            .members
            .iter()
            .map(|cell| (cell.grid_x, cell.grid_y))
            .collect();
        let expected_cells = cells
            .iter()
            .map(|(x, y)| format!("{x},{y}"))
            .collect::<Vec<_>>()
            .join(";");
        let (bounds_min, bounds_max) = job.bounds(origin);
        ensure!(
            cached.path == relative
                && cached.source_cells == expected_cells
                && cached.min == bounds_min
                && cached.max == bounds_max,
            "cached LOD chunk index differs from current inputs"
        );
        let resolved =
            resolve_asset_uri(&self.root, &self.root.join("lod-manifest.json"), &relative)?;
        let glb = fs::read(resolved)?;
        ensure!(
            hash_bytes(&glb) == cached.hash,
            "cached LOD payload hash mismatch"
        );
        crate::lod::terrain::validate_terrain_glb(&glb)?;
        Ok(TerrainChunk {
            key: job.key,
            cells,
            bounds_min,
            bounds_max,
            glb,
        })
    }
}
