//! The terrain package stage of the cluster path: whole terrain blocks as cluster payloads.
//!
//! It runs after the runtime object stage (it needs the `cluster_build` row and
//! uses its builder identity), reads the source conversion without writing to it, and adds to the
//! package:
//!
//! * one `.mcl` per terrain block (a cluster mesh of every land cell of a 16x16-cell block, primitive
//!   0, `model_path` empty, opaque) plus its `cluster_meshes` row,
//! * one standalone `.mci` per block holding that one terrain instance, under `clusters/terrain`
//!   (it is not a `cluster_chunks` row: the object block of the same anchor owns that key),
//! * one baked albedo atlas per block (KTX2, the same bake as the terrain LOD chunks),
//! * `cluster_terrain` (block to instance and atlas), `cluster_terrain_heights` (the canonical 33x33
//!   height field of every land cell in each emitted worldspace) and `cluster_terrain_build` (the hash of
//!   the exact cell cache the heights came from, so the engine can tell when they are stale).
//!
//! # Seams
//!
//! Neighbouring cells author the same edge twice and the copies differ by up to tens of units. The
//! seams of the whole worldspace are settled once, before the cells are split into blocks, by
//! [`canonicalize_terrain_cells`] (blocks are then built with `build_terrain_geometry_canonical`,
//! which never settles again): every welded lattice point gets one bit-identical height in every
//! cell that holds it, so two blocks side by side agree on their shared edge, and so do the stock
//! near patches that read the same heights from `cluster_terrain_heights`. Seams that differ by more
//! than the engine's weld bound are left as authored and reported, never averaged or hidden.
//!
//! # Coordinates
//!
//! The block mesh is block-local runtime axes (`[x, height, -y]`, origin at the south-west corner of
//! the block's anchor cell). The instance transform translates it to the anchor cell's world
//! position, `[x, 0, -y]` in runtime axes, with no rotation or scale. Block bounds in the `.mci`
//! are in Creation axes like every other block. `cluster_terrain.anchor_x/anchor_y` are absolute cell-grid
//! coordinates of the block origin; the MCI header retains normal LOD-relative block anchors.
//!
//! # Rules
//!
//! Invalid data fails the stage; nothing falls back to a flat or partial mesh. A block always takes
//! every land cell it has (absent cells get no terrain); an area limits which blocks are emitted
//! (those with at least one land cell inside it), never what a block contains.

use super::{
    builder::build_mesh_payload_locked,
    classify::{CELL_SIZE, WorldspaceOrigin, block_of},
    model::{AlphaMode, ModelPrimitive},
    publish,
    stage::CellArea,
    terrain_geometry::{
        LAND_SIDE, TerrainGeometry, TerrainGeometryCell, TerrainSeamReport,
        build_terrain_geometry_canonical, canonicalize_terrain_cells,
    },
};
use crate::{
    cache::{hash_bytes, hash_file},
    lod::{
        albedo::{TerrainAtlas, TerrainTextures},
        terrain::{TerrainCellCache, TerrainCellInput, exterior_terrain_cells},
    },
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shared::{
    asset_lock::resolve_asset_path,
    cluster::{
        BLOCK_SIDE_CELLS, InstanceInput, MciInputs, MeshRef, canonical_json, hex,
        mesh_payload_path, parse_hash, read_mci, read_mcl, write_mci,
    },
    lod::{LodOrigin, LodTier},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::Path,
};

/// Version of the block recipe (what is built from the cells and how). It is part of every block's
/// content hash, so a change to the recipe changes every payload name.
pub const TERRAIN_RECIPE_VERSION: u32 = 1;
/// `cluster_terrain_build.version`: the layout of the three terrain tables.
pub const TERRAIN_TABLES_VERSION: u32 = 1;
/// The cell cache the converter writes (and the engine and `pipeline.rs` read) in an output folder.
const CELL_CACHE_FILE: &str = "cell_cache.rkyv";
/// The most entries of the seam lists the report keeps; the counts are always complete.
const REPORT_LIST_LIMIT: usize = 1000;

/// The three optional terrain tables. Additive, like `CLUSTER_TABLES_DDL`.
pub const TERRAIN_TABLES_DDL: &str = "
CREATE TABLE IF NOT EXISTS cluster_terrain (
    mesh_hash TEXT NOT NULL, primitive_index INTEGER NOT NULL,
    worldspace_id INTEGER NOT NULL, anchor_x INTEGER NOT NULL, anchor_y INTEGER NOT NULL,
    side_cells INTEGER NOT NULL, instance_path TEXT NOT NULL, atlas_path TEXT NOT NULL,
    source_cells TEXT NOT NULL,
    PRIMARY KEY (mesh_hash, primitive_index)
);
CREATE TABLE IF NOT EXISTS cluster_terrain_heights (
    worldspace_id INTEGER NOT NULL, grid_x INTEGER NOT NULL, grid_y INTEGER NOT NULL,
    heights BLOB NOT NULL,
    PRIMARY KEY (worldspace_id, grid_x, grid_y)
);
CREATE TABLE IF NOT EXISTS cluster_terrain_build (
    id INTEGER PRIMARY KEY CHECK (id = 1), version INTEGER NOT NULL,
    cell_cache_hash TEXT NOT NULL
);
";

const TERRAIN_TABLES: [&str; 3] = [
    "cluster_terrain",
    "cluster_terrain_heights",
    "cluster_terrain_build",
];

/// What one run did, for the command-line tool and the handoff notes.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TerrainReport {
    /// Hash of the exact bytes of the source cell cache, as stored in `cluster_terrain_build`.
    pub cell_cache_hash: String,
    pub worldspaces: Vec<WorldspaceReport>,
    pub blocks: usize,
    /// Land cells inside the emitted blocks.
    pub cells: usize,
    /// Rows of `cluster_terrain_heights`.
    pub height_rows: usize,
    pub source_triangles: u64,
    pub mesh_payload_bytes: u64,
    pub instance_bytes: u64,
    pub atlas_bytes: u64,
    pub notices: Vec<String>,
    /// Source-verification receipt, retained only through this publication.
    #[serde(skip)]
    pub(crate) publication: Option<super::terrain_publish::TerrainProof>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WorldspaceReport {
    pub worldspace_id: u32,
    pub land_cells: usize,
    pub blocks_total: usize,
    pub blocks_emitted: usize,
    pub height_rows: usize,
    pub welded_edges: u32,
    pub moved_samples: u32,
    pub max_moved: f32,
    pub normals_recomputed: u32,
    /// Seams left as authored (copies differ by more than the weld bound), capped for size.
    pub unwelded_edge_count: usize,
    pub unwelded_edges: Vec<Value>,
    /// Lattice points whose duplicated vertices keep different heights, capped for size.
    pub conflict_count: usize,
    pub conflicts: Vec<Value>,
}

/// Adds the terrain of the source conversion `converted` to the staging package in `context` and returns the
/// number of blocks written. `area` limits the stage to one worldspace and to the blocks that have
/// a land cell inside the rectangle.
pub fn run(
    converted: &Path,
    context: &publish::PublishContext<'_>,
    area: Option<CellArea>,
) -> Result<usize> {
    run_with_report(converted, context, area).map(|report| report.blocks)
}

/// As [`run`], and returns the full report.
pub fn run_with_report(
    converted: &Path,
    context: &publish::PublishContext<'_>,
    area: Option<CellArea>,
) -> Result<TerrainReport> {
    run_with_report_and_jobs(converted, context, area, rayon::current_num_threads())
}

/// Certified derivation uses the same explicit worker bound as the object stage.
pub(super) fn run_with_report_and_jobs(
    converted: &Path,
    context: &publish::PublishContext<'_>,
    area: Option<CellArea>,
    jobs: usize,
) -> Result<TerrainReport> {
    run_internal(converted, context, area, true, jobs)
}

/// Standalone experiments add geometry without minting a certified terrain extension.
#[cfg(test)]
pub fn run_benchmark_with_report(
    converted: &Path,
    context: &publish::PublishContext<'_>,
    area: Option<CellArea>,
) -> Result<TerrainReport> {
    run_internal(
        converted,
        context,
        area,
        false,
        rayon::current_num_threads(),
    )
}

fn run_internal(
    converted: &Path,
    context: &publish::PublishContext<'_>,
    area: Option<CellArea>,
    certify: bool,
    jobs: usize,
) -> Result<TerrainReport> {
    let source_contract = certify
        .then(|| super::terrain_sources::TerrainSourceContract::open(converted))
        .transpose()?;
    let package = context.staging();
    if let Some(base) = context.base() {
        // An overlay reads exactly its base conversion, in place.
        ensure!(
            resolve_asset_path(converted)? == resolve_asset_path(base)?,
            "the terrain stage of an overlay reads its base {}, not {}",
            base.display(),
            converted.display()
        );
    }
    let converted_database = converted.join(publish::CORE_DATABASE);
    let cache_path = converted.join(CELL_CACHE_FILE);
    // The raw landscape DDS files: the core's `vfs`, or the verified root an overlay was given.
    let vfs = if context.is_overlay() {
        context.terrain_sources()
    } else {
        converted.join("vfs")
    };
    // Where the cluster tables are written: `clusters.db` of an overlay, else the package database.
    let package_database = context.store_path();
    for required in [&converted_database, &cache_path, &package_database] {
        ensure!(required.is_file(), "{} does not exist", required.display());
    }
    ensure!(
        vfs.is_dir(),
        "{} does not exist: the terrain atlas bake needs the source landscape textures",
        vfs.display()
    );

    // The package is checked first: a package without the object stage's tables, or one that
    // already holds terrain, is refused before any work is done.
    let mut package_connection = crate::derive_sqlite::open(&package_database, context.space())
        .wrap_err_with(|| format!("cannot open {}", package_database.display()))?;
    let identity_hex: String = package_connection
        .query_row(
            "SELECT builder_identity FROM cluster_build WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .wrap_err("the package has no cluster_build row: derive object clusters first")?;
    let identity = parse_hash(&identity_hex)
        .ok_or_else(|| eyre!("cluster_build.builder_identity is not SHA-256 hex"))?;
    let threshold: u32 = package_connection.query_row(
        "SELECT triangle_threshold FROM cluster_build WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    let expected = super::identity::identity_inputs(threshold).identity();
    ensure!(
        identity == expected,
        "cluster package builder identity differs from the current converter; rebuild its object stage first"
    );
    refuse_existing_terrain(&package_connection)?;

    // Read-only: the source conversion is never modified.
    let source = Connection::open_with_flags(&converted_database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .wrap_err_with(|| format!("cannot open {}", converted_database.display()))?;
    let worlds = selected_worlds(&source, area)?;
    ensure!(
        !worlds.is_empty(),
        "the source conversion has no worldspace with a LOD origin"
    );

    // One owned immutable snapshot for all height, tint, layer and normal reads.
    let cache = TerrainCellCache::read(&cache_path).wrap_err("cannot read the cell cache")?;
    let cache_hash = cache.hash();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .wrap_err("failed to create the terrain stage worker pool")?;

    let mut report = TerrainReport {
        cell_cache_hash: cache_hash.clone(),
        ..Default::default()
    };
    let mut outputs = Vec::new();
    for world in &worlds {
        let world = process_world(
            &source,
            &cache,
            &pool,
            &vfs,
            package,
            identity,
            world,
            area,
            source_contract.as_ref(),
            context.space(),
        )?;
        if let Some(world) = world {
            outputs.push(world);
        }
    }

    ensure!(
        hash_file(&cache_path)? == cache_hash,
        "the cell cache changed while the terrain stage read it"
    );
    drop(cache);

    for world in &outputs {
        report.worldspaces.push(world.report.clone());
        report.blocks += world.blocks.len();
        report.cells += world
            .blocks
            .iter()
            .map(|block| block.cell_count)
            .sum::<usize>();
        report.height_rows += world.heights.len();
        for block in &world.blocks {
            report.source_triangles += block.source_triangles;
            report.mesh_payload_bytes += block.mesh.payload_bytes;
            report.instance_bytes += block.instance_bytes;
            report.atlas_bytes += block.atlas_bytes;
        }
    }
    if report.blocks == 0 {
        report
            .notices
            .push("no terrain block was selected: nothing was added to the package".into());
        return Ok(report);
    }

    // Every file is on disk and verified; only now do the rows go in, in one transaction.
    for world in &outputs {
        for block in &world.blocks {
            for (path, length) in &block.files {
                let metadata = fs::metadata(package.join(path))
                    .wrap_err_with(|| format!("{path} is missing from the package"))?;
                ensure!(
                    metadata.len() == *length,
                    "{path} has {} bytes, expected {length}",
                    metadata.len()
                );
            }
        }
    }
    write_rows(&mut package_connection, &cache_hash, &outputs)?;
    if certify {
        report.publication = Some(super::terrain_publish::TerrainProof::capture(
            context,
            &cache_hash,
            &identity_hex,
        )?);
    }
    Ok(report)
}

/// Fails when the package already carries terrain rows: a rerun needs a new package copy.
fn refuse_existing_terrain(connection: &Connection) -> Result<()> {
    for table in TERRAIN_TABLES {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            continue;
        }
        let rows: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        ensure!(
            rows == 0,
            "the package already has {rows} rows in {table}: run the terrain stage on a fresh copy of the package"
        );
    }
    Ok(())
}

struct TerrainWorld {
    origin: WorldspaceOrigin,
    cells: Vec<(i32, i32, u32)>,
}

/// The stage and overlay settings share this terrain domain. Read every selected world so a
/// malformed later world cannot be hidden by an earlier nonempty one. Area selection retains
/// the whole world's cells: the stage settles seams before selecting whole blocks.
fn selected_worlds(source: &Connection, area: Option<CellArea>) -> Result<Vec<TerrainWorld>> {
    let mut origins = worldspace_origins(source)?;
    if let Some(area) = area {
        origins.retain(|origin| origin.worldspace_id == area.worldspace_id);
        ensure!(
            !origins.is_empty(),
            "worldspace {} has no LOD origin in the source conversion",
            area.worldspace_id
        );
    }
    origins
        .into_iter()
        .map(|origin| {
            let cells = exterior_terrain_cells(source, origin.worldspace_id)?;
            Ok(TerrainWorld { origin, cells })
        })
        .collect()
}

/// Whether a whole-base terrain derivation has any applicable LAND. The caller retains its
/// source lock; cache, source-recipe and texture validation remain required by the derive path.
pub(crate) fn has_selected_terrain(source: &Connection) -> Result<bool> {
    Ok(selected_worlds(source, None)?
        .iter()
        .any(|world| !world.cells.is_empty()))
}

/// The worldspaces that have a LOD origin, ordered by id (the terrain LOD compiler's own rule).
fn worldspace_origins(connection: &Connection) -> Result<Vec<WorldspaceOrigin>> {
    let mut statement = connection.prepare(
        "SELECT id, lod_origin_x, lod_origin_y FROM worldspaces
         WHERE lod_origin_x IS NOT NULL AND lod_origin_y IS NOT NULL ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(WorldspaceOrigin {
                worldspace_id: row.get(0)?,
                origin: LodOrigin::new(row.get(1)?, row.get(2)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// One worldspace -----------------------------------------------------------------------------

struct MeshRow {
    mesh_hash: String,
    payload_path: String,
    content_hash: String,
    source_triangles: u64,
    cluster_count: u32,
    vertex_count: u32,
    payload_bytes: u64,
    material_key: String,
}

struct BlockOutput {
    mesh: MeshRow,
    anchor: (i32, i32),
    instance_path: String,
    atlas_path: String,
    source_cells: String,
    cell_count: usize,
    source_triangles: u64,
    instance_bytes: u64,
    atlas_bytes: u64,
    /// Package-relative path and length of every file the block wrote.
    files: Vec<(String, u64)>,
}

struct WorldOutput {
    worldspace_id: u32,
    blocks: Vec<BlockOutput>,
    /// `(grid_x, grid_y)` and the canonical heights as little-endian f32 bytes.
    heights: Vec<((i32, i32), Vec<u8>)>,
    report: WorldspaceReport,
}

#[allow(clippy::too_many_arguments)]
fn process_world(
    source: &Connection,
    cache: &TerrainCellCache,
    pool: &rayon::ThreadPool,
    vfs: &Path,
    package: &Path,
    identity: [u8; 32],
    world: &TerrainWorld,
    area: Option<CellArea>,
    source_contract: Option<&super::terrain_sources::TerrainSourceContract>,
    space: Option<&std::sync::Arc<crate::derive_space::SpaceGuard>>,
) -> Result<Option<WorldOutput>> {
    let worldspace_id = world.origin.worldspace_id;
    let origin = world.origin.origin;
    let listed = &world.cells;
    if listed.is_empty() {
        return Ok(None);
    }
    let mut lookup: HashMap<u32, (i32, i32)> = HashMap::with_capacity(listed.len());
    let mut grids = BTreeSet::new();
    for &(grid_x, grid_y, cell_id) in listed {
        ensure!(
            grids.insert((grid_x, grid_y)),
            "worldspace {worldspace_id:08X} lists the land cell ({grid_x}, {grid_y}) twice"
        );
        ensure!(
            lookup.insert(cell_id, (grid_x, grid_y)).is_none(),
            "worldspace {worldspace_id:08X} lists the cell {cell_id:08X} twice"
        );
    }

    // The existing LOD compiler's reader: heights, tints and layers of every land cell.
    let mut inputs = cache.cells(&lookup)?;
    inputs.sort_by_key(|input| (input.grid_x, input.grid_y));
    let mut normals = read_cached_normals(cache.archived(), &lookup)?;

    let mut cells: Vec<TerrainGeometryCell> = inputs
        .iter_mut()
        .map(|input| {
            let key = (input.grid_x, input.grid_y);
            let cell_normals = normals
                .remove(&key)
                .ok_or_else(|| eyre!("the cell cache has no normals for cell {key:?}"))?;
            Ok(TerrainGeometryCell {
                grid_x: input.grid_x,
                grid_y: input.grid_y,
                heights: std::mem::take(&mut input.heights),
                normals: cell_normals,
            })
        })
        .collect::<Result<_>>()?;

    // One settle across the whole worldspace, before any partition into blocks.
    let seams = canonicalize_terrain_cells(&mut cells)
        .wrap_err_with(|| format!("worldspace {worldspace_id:08X}: terrain seams"))?;

    let partition = partition_blocks(
        origin,
        &cells
            .iter()
            .map(|cell| (cell.grid_x, cell.grid_y))
            .collect::<Vec<_>>(),
    );
    let emitted: Vec<(&(i32, i32), &Vec<usize>)> = partition
        .iter()
        .filter(|(_, members)| {
            area.is_none_or(|area| {
                members.iter().any(|&index| {
                    area_contains(
                        &area,
                        worldspace_id,
                        cells[index].grid_x,
                        cells[index].grid_y,
                    )
                })
            })
        })
        .collect();
    let mut world_report = seam_report(worldspace_id, listed.len(), partition.len(), &seams);
    world_report.blocks_emitted = emitted.len();
    if emitted.is_empty() {
        return Ok(Some(WorldOutput {
            worldspace_id,
            blocks: Vec::new(),
            heights: Vec::new(),
            report: world_report,
        }));
    }

    // Split off the inputs of the emitted blocks: the atlas bake and the texture load need them,
    // the rest of the worldspace does not.
    let emitted_cells: BTreeSet<(i32, i32)> = emitted
        .iter()
        .flat_map(|(_, members)| {
            members
                .iter()
                .map(|&index| (cells[index].grid_x, cells[index].grid_y))
        })
        .collect();
    let emitted_inputs: Vec<TerrainCellInput> = inputs
        .into_iter()
        .filter(|input| emitted_cells.contains(&(input.grid_x, input.grid_y)))
        .collect();
    let textures = TerrainTextures::load(source, vfs, &emitted_inputs)
        .wrap_err_with(|| format!("worldspace {worldspace_id:08X}: terrain textures"))?;
    if let Some(contract) = source_contract {
        contract.validate_actual(&textures.source_hashes)?;
    }
    let by_cell: BTreeMap<(i32, i32), &TerrainCellInput> = emitted_inputs
        .iter()
        .map(|input| ((input.grid_x, input.grid_y), input))
        .collect();
    let texture_paths = crate::lod::albedo::diffuse_catalog(source)?;

    let context = WorldContext {
        worldspace_id,
        origin,
        identity,
        package,
        textures: &textures,
        texture_paths: &texture_paths,
        by_cell: &by_cell,
        space,
    };
    let blocks: Vec<BlockOutput> = pool.install(|| {
        emitted
            .par_iter()
            .map(|(anchor, members)| {
                let block_cells: Vec<TerrainGeometryCell> =
                    members.iter().map(|&index| cells[index].clone()).collect();
                build_block(&context, **anchor, &block_cells)
                    .wrap_err_with(|| format!("worldspace {worldspace_id:08X} block {anchor:?}"))
            })
            .collect::<Result<_>>()
    })?;
    textures.verify_sources(vfs)?;

    // All land cells use the same finalized worldspace policy. A guard ring would merely
    // move an arrival-order weld boundary outward and could still leave a crack there.
    let heights: Vec<((i32, i32), Vec<u8>)> = cells
        .iter()
        .map(|cell| ((cell.grid_x, cell.grid_y), heights_blob(&cell.heights)))
        .collect();
    world_report.height_rows = heights.len();
    Ok(Some(WorldOutput {
        worldspace_id,
        blocks,
        heights,
        report: world_report,
    }))
}

fn seam_report(
    worldspace_id: u32,
    land_cells: usize,
    blocks_total: usize,
    seams: &TerrainSeamReport,
) -> WorldspaceReport {
    WorldspaceReport {
        worldspace_id,
        land_cells,
        blocks_total,
        blocks_emitted: 0,
        height_rows: 0,
        welded_edges: seams.welded_edges,
        moved_samples: seams.moved_samples,
        max_moved: seams.max_moved,
        normals_recomputed: seams.normals_recomputed,
        unwelded_edge_count: seams.unwelded_edges.len(),
        unwelded_edges: seams
            .unwelded_edges
            .iter()
            .take(REPORT_LIST_LIMIT)
            .map(|edge| {
                json!({
                    "cell": [edge.cell.0, edge.cell.1],
                    "neighbour": [edge.neighbour.0, edge.neighbour.1],
                    "side": format!("{:?}", edge.side),
                    "max_delta": edge.max_delta,
                })
            })
            .collect(),
        conflict_count: seams.conflicts.len(),
        conflicts: seams
            .conflicts
            .iter()
            .take(REPORT_LIST_LIMIT)
            .map(|conflict| {
                json!({
                    "point": [conflict.point.0, conflict.point.1],
                    "cause": format!("{:?}", conflict.cause),
                    "cells": conflict.cells.iter().map(|cell| [cell.0, cell.1]).collect::<Vec<_>>(),
                    "heights": conflict.heights,
                })
            })
            .collect(),
    }
}

fn area_contains(area: &CellArea, worldspace_id: u32, cell_x: i32, cell_y: i32) -> bool {
    worldspace_id == area.worldspace_id
        && (area.min.0..=area.max.0).contains(&cell_x)
        && (area.min.1..=area.max.1).contains(&cell_y)
}

/// The cells of each block, as indices into `grids` (which must be sorted by `(grid_x, grid_y)`), in
/// block-grid anchor order. Indices stay ascending inside a block, so a block's cells stay sorted.
fn partition_blocks(origin: LodOrigin, grids: &[(i32, i32)]) -> BTreeMap<(i32, i32), Vec<usize>> {
    let mut blocks: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
    for (index, &(grid_x, grid_y)) in grids.iter().enumerate() {
        blocks
            .entry(block_of(origin, grid_x, grid_y))
            .or_default()
            .push(index);
    }
    blocks
}

/// The packed `VNML` normals of the requested cells as runtime-axes vectors (the engine's near
/// terrain maps Creation `(x, y, z)` to runtime `(x, z, -y)`), keyed by grid.
type CellNormals = BTreeMap<(i32, i32), Vec<[f32; 3]>>;

fn read_cached_normals(
    archived: &shared::ArchivedCellCache,
    lookup: &HashMap<u32, (i32, i32)>,
) -> Result<CellNormals> {
    let mut normals = BTreeMap::new();
    for cell in archived.cells.iter() {
        let cell_id: u32 = cell.cell_id.into();
        let Some(&grid) = lookup.get(&cell_id) else {
            continue;
        };
        let packed: Vec<i8> = cell.normals.iter().copied().collect();
        normals.insert(grid, runtime_normals(&packed, grid)?);
    }
    ensure!(
        normals.len() == lookup.len(),
        "the cell cache holds normals for {} of {} land cells",
        normals.len(),
        lookup.len()
    );
    Ok(normals)
}

/// Unpacks three signed bytes per sample into runtime-axes vectors. A zero vector is invalid data
/// (the engine refuses it too), not something to patch over.
fn runtime_normals(packed: &[i8], grid: (i32, i32)) -> Result<Vec<[f32; 3]>> {
    ensure!(
        packed.len() == LAND_SIDE * LAND_SIDE * 3,
        "cell {grid:?} has {} packed normal bytes, expected {}",
        packed.len(),
        LAND_SIDE * LAND_SIDE * 3
    );
    packed
        .as_chunks::<3>()
        .0
        .iter()
        .map(|normal| {
            ensure!(
                *normal != [0, 0, 0],
                "cell {grid:?} has a zero-length packed normal"
            );
            Ok([
                f32::from(normal[0]),
                f32::from(normal[2]),
                -f32::from(normal[1]),
            ])
        })
        .collect()
}

fn heights_blob(heights: &[f32]) -> Vec<u8> {
    heights
        .iter()
        .flat_map(|height| height.to_le_bytes())
        .collect()
}

// One block ----------------------------------------------------------------------------------

struct WorldContext<'a> {
    worldspace_id: u32,
    origin: LodOrigin,
    identity: [u8; 32],
    package: &'a Path,
    textures: &'a TerrainTextures,
    texture_paths: &'a BTreeMap<u32, String>,
    by_cell: &'a BTreeMap<(i32, i32), &'a TerrainCellInput>,
    space: Option<&'a std::sync::Arc<crate::derive_space::SpaceGuard>>,
}

impl WorldContext<'_> {
    fn write_asset(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let write = || publish::write_asset(self.package, path, bytes);
        match self.space {
            Some(space) => space.with_file(&self.package.join(path), bytes.len() as u64, write),
            None => write(),
        }
    }
}

fn build_block(
    context: &WorldContext<'_>,
    anchor: (i32, i32),
    cells: &[TerrainGeometryCell],
) -> Result<BlockOutput> {
    #[cfg(test)]
    worker_pool_observer::record(context.package);
    ensure!(!cells.is_empty(), "block covers no land cells");
    let side = BLOCK_SIDE_CELLS as i32;
    // The south-west cell of the block: the origin of the block-local mesh.
    let anchor_cell = (
        context.origin.grid_x + anchor.0 * side,
        context.origin.grid_y + anchor.1 * side,
    );
    let inputs: Vec<&TerrainCellInput> = cells
        .iter()
        .map(|cell| {
            context
                .by_cell
                .get(&(cell.grid_x, cell.grid_y))
                .copied()
                .ok_or_else(|| {
                    eyre!(
                        "no source input for cell ({}, {})",
                        cell.grid_x,
                        cell.grid_y
                    )
                })
        })
        .collect::<Result<_>>()?;

    // The atlas tile of cell `k` quadrant `q` is `k * 4 + q` over the same sorted cells.
    let atlas = TerrainAtlas::bake(LodTier::Tier16, &inputs, context.textures)?;
    // The heights are already the worldspace-canonical ones: the block build must not settle again.
    let TerrainGeometry { geometry, locks } =
        build_terrain_geometry_canonical(cells, anchor_cell, BLOCK_SIDE_CELLS, |tile, u, v| {
            atlas.uv(tile, u, v)
        })?;
    let atlas_bytes = atlas.encode()?;
    let atlas_hash = hash_bytes(&atlas_bytes);

    let mesh_hash = block_mesh_hash(
        context,
        anchor,
        cells,
        &inputs,
        &atlas_hash,
        &geometry,
        &locks,
    );
    let mesh_hex = hex(&mesh_hash);
    let material_key = hex(&Sha256::digest(canonical_json(&json!({
        "kind": "terrain",
        "recipe": TERRAIN_RECIPE_VERSION,
        "atlas": atlas_hash,
    }))));

    let (bounds_min, bounds_max) = position_bounds(&geometry.positions)?;
    let primitive = ModelPrimitive {
        index: 0,
        geometry,
        alpha: AlphaMode::Opaque,
        alpha_cutoff: None,
        double_sided: false,
        material_key: material_key.clone(),
        material_index: None,
        bounds: (bounds_min, bounds_max),
        reject: None,
    };
    let built = build_mesh_payload_locked(&primitive, context.identity, mesh_hash, &locks)
        .map_err(|reject| eyre!("{}: {}", reject.reason, reject.detail))?;
    let payload_path = mesh_payload_path(&mesh_hex, 0);
    context.write_asset(&payload_path, &built.bytes)?;
    read_mcl(&fs::read(context.package.join(&payload_path))?)
        .map_err(|error| eyre!("{payload_path} on disk: {error}"))?;
    let content_hash = hash_bytes(&built.bytes);

    let atlas_path = format!(
        "clusters/terrain/atlases/{}/{atlas_hash}.ktx2",
        &atlas_hash[..2]
    );
    context.write_asset(&atlas_path, &atlas_bytes)?;

    // The block's one instance: the mesh translated to the anchor cell, in runtime axes.
    let translation = [
        (f64::from(anchor_cell.0) * f64::from(CELL_SIZE)) as f32,
        0.0,
        -((f64::from(anchor_cell.1) * f64::from(CELL_SIZE)) as f32),
    ];
    let world_min_rt = [
        translation[0] + bounds_min[0],
        translation[1] + bounds_min[1],
        translation[2] + bounds_min[2],
    ];
    let world_max_rt = [
        translation[0] + bounds_max[0],
        translation[1] + bounds_max[1],
        translation[2] + bounds_max[2],
    ];
    let instance = InstanceInput {
        mesh: MeshRef {
            mesh_hash,
            primitive_index: 0,
        },
        transform: [
            1.0,
            0.0,
            0.0,
            translation[0],
            0.0,
            1.0,
            0.0,
            translation[1],
            0.0,
            0.0,
            1.0,
            translation[2],
        ],
        cell_x: anchor_cell.0,
        cell_y: anchor_cell.1,
        stable_id: [0; 16],
        base_form_id: 0,
        snow_material: 0,
        source_worldspace_id: 0,
        // Creation axes: x east, y north (runtime -z), z up (runtime y).
        world_min: [world_min_rt[0], -world_max_rt[2], world_min_rt[1]],
        world_max: [world_max_rt[0], -world_min_rt[2], world_max_rt[1]],
    };
    let instance_bytes = write_mci(&MciInputs {
        builder_identity: context.identity,
        worldspace_id: context.worldspace_id,
        tier: BLOCK_SIDE_CELLS,
        anchor_x: anchor.0,
        anchor_y: anchor.1,
        instances: &[instance],
    })
    .map_err(|error| eyre!("instance block: {error}"))?;
    read_mci(&instance_bytes).map_err(|error| eyre!("instance block: {error}"))?;
    let instance_hash = hash_bytes(&instance_bytes);
    let instance_path = format!(
        "clusters/terrain/instances/{}/{instance_hash}.mci",
        &instance_hash[..2]
    );
    context.write_asset(&instance_path, &instance_bytes)?;
    let written = fs::read(context.package.join(&instance_path))?;
    read_mci(&written).map_err(|error| eyre!("{instance_path} on disk: {error}"))?;

    Ok(BlockOutput {
        mesh: MeshRow {
            mesh_hash: mesh_hex,
            payload_path: payload_path.clone(),
            content_hash,
            source_triangles: built.stats.source_triangles,
            cluster_count: built.stats.cluster_count,
            vertex_count: built.stats.vertex_count,
            payload_bytes: built.stats.payload_bytes,
            material_key,
        },
        anchor: anchor_cell,
        source_cells: source_cells_json(cells),
        cell_count: cells.len(),
        source_triangles: built.stats.source_triangles,
        instance_bytes: instance_bytes.len() as u64,
        atlas_bytes: atlas_bytes.len() as u64,
        files: vec![
            (payload_path, built.bytes.len() as u64),
            (atlas_path.clone(), atlas_bytes.len() as u64),
            (instance_path.clone(), instance_bytes.len() as u64),
        ],
        instance_path,
        atlas_path,
    })
}

/// `[[grid_x,grid_y],...]` of the land cells the block was built from, in sorted order.
fn source_cells_json(cells: &[TerrainGeometryCell]) -> String {
    let list: Vec<[i32; 2]> = cells
        .iter()
        .map(|cell| [cell.grid_x, cell.grid_y])
        .collect();
    serde_json::to_string(&list).expect("a list of integers serialises")
}

fn position_bounds(positions: &[[f32; 3]]) -> Result<([f32; 3], [f32; 3])> {
    ensure!(!positions.is_empty(), "terrain geometry has no vertices");
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for position in positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    Ok((min, max))
}

/// Digest of the source files the atlas bake read, folded into every block's content hash.
#[cfg(test)]
fn texture_sources_digest(textures: &TerrainTextures) -> [u8; 32] {
    let mut digest = Sha256::new();
    for (path, hash) in &textures.source_hashes {
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update(hash.as_bytes());
    }
    digest.finalize().into()
}

/// The content hash of a block: the recipe, the builder identity, the block key, every source
/// cell's canonical heights, normals, tint and layers, the texture sources, the baked atlas and the
/// geometry and locks that were built from them. Nothing here is a hard-coded id.
fn block_mesh_hash(
    context: &WorldContext<'_>,
    anchor: (i32, i32),
    cells: &[TerrainGeometryCell],
    inputs: &[&TerrainCellInput],
    atlas_hash: &str,
    geometry: &super::model::PrimitiveGeometry,
    locks: &[u8],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"mudcrab-terrain-block");
    digest.update(TERRAIN_RECIPE_VERSION.to_le_bytes());
    digest.update(context.identity);
    digest.update(context.worldspace_id.to_le_bytes());
    digest.update(anchor.0.to_le_bytes());
    digest.update(anchor.1.to_le_bytes());
    digest.update(BLOCK_SIDE_CELLS.to_le_bytes());
    digest.update(context.origin.grid_x.to_le_bytes());
    digest.update(context.origin.grid_y.to_le_bytes());
    let texture_ids: BTreeSet<_> = inputs
        .iter()
        .flat_map(|cell| &cell.layers)
        .map(|layer| layer.texture_form_id)
        .filter(|id| *id != 0)
        .collect();
    for id in texture_ids {
        let path = context
            .texture_paths
            .get(&id)
            .expect("the terrain texture loader validated this ID");
        let hash = context
            .textures
            .source_hashes
            .get(path)
            .expect("the texture loader retained its source hash");
        digest.update(id.to_le_bytes());
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update(hash.as_bytes());
    }
    for (cell, input) in cells.iter().zip(inputs) {
        digest.update(cell.grid_x.to_le_bytes());
        digest.update(cell.grid_y.to_le_bytes());
        digest.update(input.cell_id.to_le_bytes());
        for height in &cell.heights {
            digest.update(height.to_bits().to_le_bytes());
        }
        for normal in &cell.normals {
            for component in normal {
                digest.update(component.to_bits().to_le_bytes());
            }
        }
        digest.update((input.vertex_colors.len() as u64).to_le_bytes());
        digest.update(&input.vertex_colors);
        digest.update((input.layers.len() as u64).to_le_bytes());
        for layer in &input.layers {
            digest.update(layer.texture_form_id.to_le_bytes());
            digest.update([layer.quadrant, u8::from(layer.is_base)]);
            digest.update(layer.layer.to_le_bytes());
            digest.update((layer.weights.len() as u64).to_le_bytes());
            for weight in &layer.weights {
                digest.update(weight.vertex.to_le_bytes());
                digest.update(weight.opacity.to_bits().to_le_bytes());
            }
        }
    }
    digest.update(atlas_hash.as_bytes());
    for position in &geometry.positions {
        for component in position {
            digest.update(component.to_bits().to_le_bytes());
        }
    }
    for normal in &geometry.normals {
        for component in normal {
            digest.update(component.to_bits().to_le_bytes());
        }
    }
    for uv in &geometry.uvs {
        for component in uv {
            digest.update(component.to_bits().to_le_bytes());
        }
    }
    for index in &geometry.indices {
        digest.update(index.to_le_bytes());
    }
    digest.update(locks);
    digest.finalize().into()
}

// Rows ---------------------------------------------------------------------------------------

fn write_rows(
    connection: &mut Connection,
    cell_cache_hash: &str,
    worlds: &[WorldOutput],
) -> Result<()> {
    connection.execute_batch(TERRAIN_TABLES_DDL)?;
    let transaction = connection.transaction()?;
    for world in worlds {
        for block in &world.blocks {
            publish::insert_mesh_row(
                &transaction,
                &publish::MeshRow {
                    mesh_hash: &block.mesh.mesh_hash,
                    primitive_index: 0,
                    payload_path: &block.mesh.payload_path,
                    content_hash: &block.mesh.content_hash,
                    source_triangles: block.mesh.source_triangles,
                    cluster_count: block.mesh.cluster_count,
                    vertex_count: block.mesh.vertex_count,
                    payload_bytes: block.mesh.payload_bytes,
                    material_key: &block.mesh.material_key,
                    model_path: "",
                    material_index: None,
                    alpha_mode: "opaque",
                    alpha_cutoff: None,
                    double_sided: false,
                },
                false,
            )?;
            transaction
                .execute(
                    "INSERT INTO cluster_terrain(mesh_hash, primitive_index, worldspace_id, anchor_x,
                         anchor_y, side_cells, instance_path, atlas_path, source_cells)
                     VALUES (?1, 0, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        block.mesh.mesh_hash,
                        world.worldspace_id,
                        block.anchor.0,
                        block.anchor.1,
                        BLOCK_SIDE_CELLS,
                        block.instance_path,
                        block.atlas_path,
                        block.source_cells,
                    ],
                )
                .wrap_err_with(|| format!("cluster_terrain row of {}", block.mesh.mesh_hash))?;
        }
        for ((grid_x, grid_y), heights) in &world.heights {
            transaction
                .execute(
                    "INSERT INTO cluster_terrain_heights(worldspace_id, grid_x, grid_y, heights)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![world.worldspace_id, grid_x, grid_y, heights],
                )
                .wrap_err_with(|| {
                    format!(
                        "cluster_terrain_heights row of {:08X} ({grid_x}, {grid_y})",
                        world.worldspace_id
                    )
                })?;
        }
    }
    transaction.execute(
        "INSERT INTO cluster_terrain_build(id, version, cell_cache_hash) VALUES (1, ?1, ?2)",
        params![TERRAIN_TABLES_VERSION, cell_cache_hash],
    )?;
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_cell(grid_x: i32, grid_y: i32, height: f32) -> TerrainGeometryCell {
        TerrainGeometryCell {
            grid_x,
            grid_y,
            heights: vec![height; LAND_SIDE * LAND_SIDE],
            normals: Vec::new(),
        }
    }

    #[test]
    fn packed_normals_map_to_runtime_axes_and_zero_is_rejected() {
        let mut packed = vec![0i8; LAND_SIDE * LAND_SIDE * 3];
        for sample in packed.as_chunks_mut::<3>().0 {
            sample.copy_from_slice(&[10, 20, 127]);
        }
        let normals = runtime_normals(&packed, (3, 4)).unwrap();
        assert_eq!(normals.len(), LAND_SIDE * LAND_SIDE);
        // Creation (x, y, z) is runtime (x, z, -y).
        assert_eq!(normals[0], [10.0, 127.0, -20.0]);

        packed[3..6].copy_from_slice(&[0, 0, 0]);
        let error = runtime_normals(&packed, (3, 4)).unwrap_err().to_string();
        assert!(error.contains("zero-length"), "{error}");
        assert!(runtime_normals(&packed[..30], (3, 4)).is_err());
    }

    #[test]
    fn blocks_follow_the_origin_and_keep_cells_sorted() {
        // With origin (-3, 0), cells -3..=12 are block 0 and 13..=28 block 1 along x.
        let origin = LodOrigin::new(-3, 0);
        let grids = [(-4, 0), (-3, 0), (12, 0), (13, 0), (13, 1)];
        let blocks = partition_blocks(origin, &grids);
        assert_eq!(blocks[&(-1, 0)], vec![0]);
        assert_eq!(blocks[&(0, 0)], vec![1, 2]);
        assert_eq!(blocks[&(1, 0)], vec![3, 4]);
    }

    #[test]
    fn an_area_selects_whole_blocks_by_any_cell_inside() {
        let area = CellArea {
            worldspace_id: 7,
            min: (13, 0),
            max: (13, 0),
        };
        assert!(area_contains(&area, 7, 13, 0));
        assert!(!area_contains(&area, 7, 12, 0));
        assert!(!area_contains(&area, 8, 13, 0));
        let origin = LodOrigin::new(-3, 0);
        let grids = [(-3, 0), (12, 0), (13, 0), (13, 1)];
        let blocks = partition_blocks(origin, &grids);
        let selected: Vec<_> = blocks
            .iter()
            .filter(|(_, members)| {
                members
                    .iter()
                    .any(|&index| area_contains(&area, 7, grids[index].0, grids[index].1))
            })
            .map(|(anchor, members)| (*anchor, members.len()))
            .collect();
        // Only block (1, 0) has the cell; it keeps its other cell outside the area too.
        assert_eq!(selected, vec![((1, 0), 2)]);
    }

    #[test]
    fn source_cells_list_exactly_the_land_cells_present() {
        // A hole at (1, 0): the block lists the cells it has and nothing for the absent one.
        let cells = [
            flat_cell(0, 0, 0.0),
            flat_cell(2, 0, 0.0),
            flat_cell(0, 1, 0.0),
        ];
        assert_eq!(source_cells_json(&cells), "[[0,0],[2,0],[0,1]]");
    }

    #[test]
    fn heights_are_stored_as_little_endian_f32_in_field_order() {
        let mut cell = flat_cell(0, 0, 1.5);
        cell.heights[1] = -2.25;
        let blob = heights_blob(&cell.heights);
        assert_eq!(blob.len(), LAND_SIDE * LAND_SIDE * 4);
        assert_eq!(f32::from_le_bytes(blob[0..4].try_into().unwrap()), 1.5);
        assert_eq!(f32::from_le_bytes(blob[4..8].try_into().unwrap()), -2.25);
    }

    #[test]
    fn a_seam_between_two_blocks_is_identical_on_both_sides_after_one_world_settle() {
        // Cells 15 and 16 along x are in different blocks of origin (0, 0).
        let origin = LodOrigin::new(0, 0);
        let mut left = flat_cell(15, 0, 100.0);
        let mut right = flat_cell(16, 0, 100.0);
        for row in 0..LAND_SIDE {
            // The shared edge: the left cell's east column against the right cell's west column.
            left.heights[row * LAND_SIDE + 32] = 100.0 + row as f32;
            right.heights[row * LAND_SIDE] = 110.0 + row as f32;
        }
        let mut cells = vec![left, right];
        let report = canonicalize_terrain_cells(&mut cells).unwrap();
        assert!(report.welded_edges >= 1);

        let grids: Vec<_> = cells.iter().map(|c| (c.grid_x, c.grid_y)).collect();
        let blocks = partition_blocks(origin, &grids);
        assert_eq!(blocks.len(), 2);
        let (left_block, right_block) = (&cells[blocks[&(0, 0)][0]], &cells[blocks[&(1, 0)][0]]);
        for row in 0..LAND_SIDE {
            assert_eq!(
                left_block.heights[row * LAND_SIDE + 32].to_bits(),
                right_block.heights[row * LAND_SIDE].to_bits(),
                "row {row}"
            );
        }
    }

    #[test]
    fn existing_terrain_rows_are_refused_and_empty_tables_are_not() {
        let connection = Connection::open_in_memory().unwrap();
        refuse_existing_terrain(&connection).unwrap();
        connection.execute_batch(TERRAIN_TABLES_DDL).unwrap();
        refuse_existing_terrain(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO cluster_terrain_build(id, version, cell_cache_hash) VALUES (1, 1, 'x')",
                [],
            )
            .unwrap();
        let error = refuse_existing_terrain(&connection)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cluster_terrain_build"), "{error}");
    }

    #[test]
    fn package_files_are_never_overwritten_with_different_bytes() {
        let directory = tempfile::tempdir().unwrap();
        publish::write_asset(directory.path(), "clusters/terrain/a.bin", b"one").unwrap();
        publish::write_asset(directory.path(), "clusters/terrain/a.bin", b"one").unwrap();
        assert!(publish::write_asset(directory.path(), "clusters/terrain/a.bin", b"two").is_err());
        assert!(publish::write_asset(directory.path(), "../escape.bin", b"x").is_err());
    }

    #[test]
    fn the_content_hash_changes_with_the_recipe_inputs() {
        let a = texture_sources_digest(&TerrainTextures::default());
        let mut textures = TerrainTextures::default();
        textures
            .source_hashes
            .insert("textures/landscape/x.dds".into(), "00".repeat(32));
        assert_ne!(a, texture_sources_digest(&textures));
    }
}

#[cfg(test)]
pub(super) mod worker_pool_observer {
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::{Path, PathBuf},
        sync::Mutex,
    };

    static POOLS: Mutex<BTreeMap<PathBuf, BTreeSet<usize>>> = Mutex::new(BTreeMap::new());

    pub(crate) fn record(package: &Path) {
        POOLS
            .lock()
            .unwrap()
            .entry(package.to_owned())
            .or_default()
            .insert(rayon::current_num_threads());
    }

    pub(crate) fn take(package: &Path) -> BTreeSet<usize> {
        POOLS.lock().unwrap().remove(package).unwrap_or_default()
    }
}
