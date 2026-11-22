//! Imports the retail terrain LOD of one worldspace into a pack directory: compiles each block
//! (see [`super::retail_terrain`]), writes the chunk files and database rows, and counts every
//! block it cannot import with its reason. Shared by `derive-terrain-lod` and the conversion
//! pipeline.

use super::{
    retail::btr::{TerrainLodShapeKind, read_terrain_lod},
    retail_terrain::{
        IMPORT_VERSION, RETAIL_NORMAL_AXES, RetailBlock, compile_block, tier_for_level,
    },
    retail_terrain_source::{self as source, BlockId, RetailTerrainFiles, Skipped},
    terrain::publish_chunk,
};
use crate::cache::hash_bytes;
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rayon::prelude::*;
use rusqlite::Connection;
use shared::lod::LodOrigin;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Blocks extracted, compiled and published together; bounds scratch space and memory.
const BATCH_BLOCKS: usize = 96;

/// Counts of one worldspace.
#[derive(Debug, Default, Clone)]
pub struct WorldReport {
    pub editor_id: String,
    /// Retail blocks found, by level.
    pub found: BTreeMap<u32, usize>,
    /// Chunks written, by tier.
    pub compiled: BTreeMap<u32, usize>,
    pub triangles: usize,
    pub vertices: usize,
    pub bytes: u64,
    /// Self-built chunks removed because a retail chunk replaced them or none exists.
    pub old_chunks_removed: usize,
    /// Skipped blocks by class.
    pub skipped_by_class: BTreeMap<&'static str, usize>,
    pub skipped: Vec<(BlockId, Skipped)>,
}

/// A worldspace to import: its database id, names and LOD origin.
pub(crate) struct LodWorld {
    pub id: u32,
    pub editor_id: String,
    /// Lowercase editor id, the world name in the `.btr` file names.
    pub file_name: String,
    pub origin: LodOrigin,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn import_world(
    connection: &Connection,
    target: &Path,
    world: &LodWorld,
    files: &RetailTerrainFiles,
    ids: &[BlockId],
    pool: &rayon::ThreadPool,
    retail_inputs: &mut BTreeMap<String, String>,
    payload_hashes: &mut Vec<String>,
) -> Result<WorldReport> {
    let mut report = WorldReport {
        editor_id: world.editor_id.clone(),
        ..Default::default()
    };
    for (_, level, ..) in ids {
        *report.found.entry(*level).or_default() += 1;
    }
    // The self-built chunks of this world go: rows, index and payload files. Object LOD files
    // (`objects_*.glb`) and everything else in the folder stay.
    report.old_chunks_removed = remove_self_built(connection, target, world.id)?;

    let (plan, no_tier): (Vec<&BlockId>, Vec<&BlockId>) = ids
        .iter()
        .partition(|(_, level, ..)| tier_for_level(*level).is_some());
    for id in no_tier {
        skip(
            &mut report,
            id,
            Skipped {
                class: "no_runtime_tier",
                detail: format!(
                    "level {} has no runtime tier; the runtime draws tier 16 there",
                    id.1
                ),
            },
        );
    }
    for batch in plan.chunks(BATCH_BLOCKS) {
        let scratch = tempfile::tempdir().wrap_err("failed to create a scratch folder")?;
        let batch_ids: Vec<BlockId> = batch.iter().map(|id| (*id).clone()).collect();
        let extracted = source::extract(files, &batch_ids, scratch.path())?;
        // Compile in parallel; the database is written from this thread only.
        type Compiled = std::result::Result<
            (
                crate::lod::terrain::TerrainChunk,
                crate::lod::retail_terrain::BlockStats,
                String,
            ),
            Skipped,
        >;
        let results: Vec<(BlockId, Compiled)> = pool.install(|| {
            batch_ids
                .par_iter()
                .map(|id| (id.clone(), compile_one(world, files, &extracted, id)))
                .collect()
        });
        let tx = connection.unchecked_transaction()?;
        for (id, result) in results {
            match result {
                Ok((chunk, stats, input_hash)) => {
                    publish_chunk(&tx, target, &chunk)?;
                    let relative = shared::lod::chunk_payload_path(chunk.key);
                    payload_hashes.push(format!("{relative}:{}", hash_bytes(&chunk.glb)));
                    retail_inputs.insert(relative, input_hash);
                    *report.compiled.entry(id.1).or_default() += 1;
                    report.triangles += stats.triangles;
                    report.vertices += stats.vertices;
                    report.bytes += stats.bytes as u64;
                }
                Err(skipped) => skip(&mut report, &id, skipped),
            }
        }
        tx.commit()?;
    }
    Ok(report)
}

pub(crate) fn skip(report: &mut WorldReport, id: &BlockId, skipped: Skipped) {
    *report.skipped_by_class.entry(skipped.class).or_default() += 1;
    report.skipped.push((id.clone(), skipped));
}

fn compile_one(
    world: &LodWorld,
    files: &RetailTerrainFiles,
    extracted: &source::Extracted,
    id: &BlockId,
) -> std::result::Result<
    (
        crate::lod::terrain::TerrainChunk,
        crate::lod::retail_terrain::BlockStats,
        String,
    ),
    Skipped,
> {
    let bytes = extracted.read(files, id)?;
    let label = PathBuf::from(format!("{}.{}.{}.{}.btr", id.0, id.1, id.2, id.3));
    let parsed = read_terrain_lod(&bytes.mesh, &label).map_err(|error| Skipped {
        class: "unreadable_mesh",
        detail: format!("{error:#}"),
    })?;
    let lands: Vec<_> = parsed.shapes_of(TerrainLodShapeKind::Terrain).collect();
    let [land] = lands.as_slice() else {
        return Err(Skipped {
            class: "no_single_land_shape",
            detail: format!("the file has {} land shapes, expected 1", lands.len()),
        });
    };
    let (chunk, stats) = compile_block(&RetailBlock {
        world: world.id,
        origin: world.origin,
        level: id.1,
        cell: (id.2, id.3),
        land,
        colour_dds: &bytes.colour,
        normal_dds: &bytes.normal,
    })
    .map_err(|error| Skipped {
        class: "compile_failed",
        detail: format!("{error:#}"),
    })?;
    // Check what will be written, so a bad chunk is a counted skip and not a failed run.
    crate::lod::terrain::validate_terrain_glb(&chunk.glb).map_err(|error| Skipped {
        class: "invalid_chunk",
        detail: format!("{error:#}"),
    })?;
    let input_hash = hash_bytes(
        format!(
            "{IMPORT_VERSION}:{:?}:{}:{}:{}",
            RETAIL_NORMAL_AXES,
            hash_bytes(&bytes.mesh),
            hash_bytes(&bytes.colour),
            hash_bytes(&bytes.normal)
        )
        .as_bytes(),
    );
    Ok((chunk, stats, input_hash))
}

/// Removes the world's terrain chunk rows, index rows and `cell_*.glb` payload files.
fn remove_self_built(connection: &Connection, target: &Path, world: u32) -> Result<usize> {
    let paths: Vec<String> = connection
        .prepare("SELECT payload_path FROM lod_chunks WHERE worldspace_id = ?1")?
        .query_map([world], |row| row.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for relative in &paths {
        ensure!(
            relative.starts_with(&format!("lod/{world:08x}/")) && !relative.contains(".."),
            "refused: chunk payload path {relative} is outside the world's LOD folder"
        );
        match fs::remove_file(target.join(relative)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => bail!("cannot remove {relative}: {error}"),
        }
    }
    connection.execute("DELETE FROM lod_chunks WHERE worldspace_id = ?1", [world])?;
    connection.execute(
        "DELETE FROM lod_chunks_spatial WHERE worldspace_id = ?1",
        [world],
    )?;
    Ok(paths.len())
}
