//! Imports the retail terrain LOD of one worldspace into a pack directory: compiles each block
//! (see [`super::retail_terrain`]), writes the chunk files and database rows, and counts every
//! block it cannot import with its reason. Shared by `derive-terrain-lod` and the conversion
//! pipeline.

use super::{
    generation::StagedGeneration,
    retail::btr::{TerrainLodShapeKind, read_terrain_lod},
    retail_terrain::{
        IMPORT_VERSION, RETAIL_NORMAL_AXES, RetailBlock, compile_block, tier_for_level,
    },
    retail_terrain_source::{BlockId, RetailTerrainFiles, Skipped},
    terrain::publish_chunk,
};
use crate::cache::hash_bytes;
use color_eyre::{
    Result,
    eyre::{ContextCompat, ensure},
};
use rayon::prelude::*;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use shared::lod::{ChunkAnchor, ChunkKey, LodOrigin, LodTier, chunk_payload_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Blocks compiled together to bound memory.
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
    /// Retail chunks kept from the previous generation because no replacement compiled.
    pub kept_from_previous_generation: usize,
    pub(crate) kept_self_built: bool,
    pub(crate) replaced_paths: BTreeSet<String>,
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

/// The report and identity inputs for one staged world.
pub(crate) struct WorldImport {
    pub(crate) report: WorldReport,
    pub(crate) retail_inputs: BTreeMap<String, String>,
    pub(crate) payload_hashes: Vec<String>,
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
    check_stop: impl Fn() -> Result<()>,
) -> Result<WorldReport> {
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
    let mut staged = StagedGeneration::new(target)?;
    let imported = stage_world(
        &tx,
        &mut staged,
        world,
        files,
        ids,
        pool,
        &BTreeMap::new(),
        &check_stop,
    )?;
    if !imported.retail_inputs.is_empty() {
        check_stop()?;
        staged.publish(target, tx, check_stop)?;
        retail_inputs.extend(imported.retail_inputs);
        payload_hashes.extend(imported.payload_hashes);
    }
    Ok(imported.report)
}

/// Compiles one world into the caller's generation. No live files are changed here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stage_world(
    tx: &Transaction<'_>,
    staged: &mut StagedGeneration,
    world: &LodWorld,
    files: &RetailTerrainFiles,
    ids: &[BlockId],
    pool: &rayon::ThreadPool,
    previous_retail_inputs: &BTreeMap<String, String>,
    check_stop: impl Fn() -> Result<()>,
) -> Result<WorldImport> {
    let mut report = WorldReport {
        editor_id: world.editor_id.clone(),
        ..Default::default()
    };
    for (_, level, ..) in ids {
        *report.found.entry(*level).or_default() += 1;
    }
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
    if !plan.is_empty() {
        check_stop()?;
    }
    let previous_chunks = old_chunks(tx, world.id)?;
    let staging = staged.staging_root();
    let mut new_inputs = BTreeMap::new();
    let mut new_payload_hashes = Vec::new();

    for batch in plan.chunks(BATCH_BLOCKS) {
        check_stop()?;
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
            batch
                .par_iter()
                .map(|&id| (id.clone(), compile_one(world, files, id)))
                .collect()
        });
        check_stop()?;
        for (id, result) in results {
            match result {
                Ok((chunk, stats, input_hash)) => {
                    let relative = chunk_payload_path(chunk.key);
                    ensure!(
                        staged.paths.insert(PathBuf::from(&relative)),
                        "duplicate terrain LOD chunk {relative}"
                    );
                    publish_chunk(tx, &staging, &chunk)?;
                    new_payload_hashes.push(format!("{relative}:{}", hash_bytes(&chunk.glb)));
                    new_inputs.insert(relative, input_hash);
                    *report.compiled.entry(id.1).or_default() += 1;
                    report.triangles += stats.triangles;
                    report.vertices += stats.vertices;
                    report.bytes += stats.bytes as u64;
                }
                Err(skipped) => skip(&mut report, &id, skipped),
            }
        }
    }
    if !plan.is_empty() {
        files.check_archives(ids)?;
    }
    for (key, relative) in previous_chunks {
        let was_retail = previous_retail_inputs.contains_key(&relative);
        let replaced = new_inputs.contains_key(&relative);
        if !replaced && was_retail {
            report.kept_from_previous_generation += 1;
            continue;
        }
        // A world whose blocks all fail keeps its existing self-built chunks too.
        if new_inputs.is_empty() {
            report.kept_self_built = true;
            continue;
        }
        if !replaced {
            let params =
                rusqlite::params![world.id, key.tier.side_cells(), key.anchor.x, key.anchor.y];
            tx.execute(
                "DELETE FROM lod_chunks WHERE worldspace_id = ?1 AND tier = ?2 AND anchor_x = ?3 AND anchor_y = ?4",
                params,
            )?;
            tx.execute(
                "DELETE FROM lod_chunks_spatial WHERE worldspace_id = ?1 AND tier = ?2 AND anchor_x = ?3 AND anchor_y = ?4",
                params,
            )?;
            staged.stale.insert(PathBuf::from(&relative));
        }
        if !was_retail {
            report.old_chunks_removed += 1;
        }
        report.replaced_paths.insert(relative);
    }
    Ok(WorldImport {
        report,
        retail_inputs: new_inputs,
        payload_hashes: new_payload_hashes,
    })
}

pub(crate) fn skip(report: &mut WorldReport, id: &BlockId, skipped: Skipped) {
    *report.skipped_by_class.entry(skipped.class).or_default() += 1;
    report.skipped.push((id.clone(), skipped));
}

fn compile_one(
    world: &LodWorld,
    files: &RetailTerrainFiles,
    id: &BlockId,
) -> std::result::Result<
    (
        crate::lod::terrain::TerrainChunk,
        crate::lod::retail_terrain::BlockStats,
        String,
    ),
    Skipped,
> {
    let bytes = files.read(id)?;
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
    if let Some(ancestor) = land.transformed_ancestor {
        return Err(Skipped {
            class: "non_identity_land_ancestor",
            detail: format!("land shape ancestor block {ancestor} has a non-identity transform"),
        });
    }
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

/// Validate every old path before using it to move a payload.
fn old_chunks(connection: &Connection, world: u32) -> Result<Vec<(ChunkKey, String)>> {
    let mut statement = connection.prepare(
        "SELECT tier, anchor_x, anchor_y, payload_path FROM lod_chunks WHERE worldspace_id = ?1",
    )?;
    let rows = statement.query_map([world], |row| {
        Ok((
            row.get::<_, i32>(0)?,
            row.get::<_, i32>(1)?,
            row.get::<_, i32>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (side, x, y, relative) = row?;
        let tier = LodTier::from_side_cells(side).wrap_err("invalid old LOD tier")?;
        let key = ChunkKey::new(world, tier, ChunkAnchor::new(x, y));
        ensure!(
            relative == chunk_payload_path(key),
            "refused: chunk payload path {relative} is not canonical for world {world:08x}"
        );
        Ok((key, relative))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::super::retail_terrain_source;
    use super::*;
    use std::fs;

    #[test]
    fn a_world_with_all_failed_blocks_keeps_its_rows_and_payload_for_retry() {
        let target = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE lod_chunks(worldspace_id, tier, anchor_x, anchor_y, payload_path);
             CREATE TABLE lod_chunks_spatial(worldspace_id);
             INSERT INTO lod_chunks_spatial VALUES (60);",
            )
            .unwrap();
        let path = chunk_payload_path(ChunkKey::new(60, LodTier::Tier4, ChunkAnchor::new(0, 0)));
        connection
            .execute("INSERT INTO lod_chunks VALUES (60, 4, 0, 0, ?1)", [&path])
            .unwrap();
        let payload = target.path().join(&path);
        fs::create_dir_all(payload.parent().unwrap()).unwrap();
        fs::write(&payload, b"previous payload").unwrap();

        let data = tempfile::tempdir().unwrap();
        let mesh_dir = data.path().join("meshes/terrain/w");
        fs::create_dir_all(&mesh_dir).unwrap();
        fs::write(mesh_dir.join("w.4.0.0.btr"), b"missing both maps").unwrap();
        let files = retail_terrain_source::list(data.path()).unwrap();
        let ids: Vec<_> = files.blocks.keys().cloned().collect();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let world = LodWorld {
            id: 60,
            editor_id: "W".to_owned(),
            file_name: "w".to_owned(),
            origin: LodOrigin::new(0, 0),
        };
        let checks = std::cell::Cell::new(0);
        let mut inputs = BTreeMap::new();
        let mut hashes = Vec::new();
        let cancelled = import_world(
            &connection,
            target.path(),
            &world,
            &files,
            &ids,
            &pool,
            &mut inputs,
            &mut hashes,
            || {
                checks.set(checks.get() + 1);
                ensure!(checks.get() < 3, "test cancellation after compilation");
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(cancelled.to_string(), "test cancellation after compilation");
        assert!(inputs.is_empty() && hashes.is_empty());
        for _ in 0..2 {
            let mut inputs = BTreeMap::new();
            let mut hashes = Vec::new();
            let report = import_world(
                &connection,
                target.path(),
                &world,
                &files,
                &ids,
                &pool,
                &mut inputs,
                &mut hashes,
                || Ok(()),
            )
            .unwrap();
            assert_eq!(report.skipped_by_class["missing_colour_map"], 1);
            assert_eq!(report.old_chunks_removed, 0);
            assert!(inputs.is_empty() && hashes.is_empty());
            let saved: String = connection
                .query_row(
                    "SELECT payload_path FROM lod_chunks WHERE worldspace_id = 60",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(saved, path);
            let indexed: i32 = connection
                .query_row("SELECT worldspace_id FROM lod_chunks_spatial", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(indexed, 60);
            assert_eq!(fs::read(&payload).unwrap(), b"previous payload");
        }
    }
}
