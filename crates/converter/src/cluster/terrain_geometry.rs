//! Terrain cells as one cluster-ready triangle mesh, drawn exactly like the stock engine path.
//!
//! The stock engine builds every exterior cell as four 17x17 quadrant meshes
//! (`build_terrain_quadrant_mesh` in `crates/engine/src/streaming.rs`). This module reproduces that
//! geometry for a block of cells so the cluster builder can simplify it, with the same vertices,
//! triangulation and winding, in the cluster stage's block-local runtime axes.
//!
//! # Output layout
//!
//! For the cell at index `k` of the input slice and quadrant `q` (0 SW, 1 SE, 2 NW, 3 NE; the x half
//! is `q % 2`, the y half `q / 2`), the quadrant owns [`VERTICES_PER_QUADRANT`] vertices starting at
//! `(k * 4 + q) * 289`, in row-major order: local vertex `(local_x, local_y)` (both 0..=16, y grows
//! north) is [`vertex_index`]. Its height-field sample is `(16 * (q % 2) + local_x, 16 * (q / 2) +
//! local_y)` of the cell's 33x33 field. Quadrants never share vertices, so the atlas UVs of two
//! quadrants may differ at the same position. Indices are one `INDICES_PER_QUADRANT` run per quadrant
//! in the same order, and the quad at local `(x, y)` is `a = y * 17 + x`, `b = a + 1`, `c = a + 17`,
//! `d = c + 1` drawn as `[a, b, c]` and `[b, d, c]` (the stock diagonal runs from `b` to `c`).
//!
//! Positions are `[x, height, -y]` in block-local runtime axes: `x` and `y` count 128-unit sample
//! steps from the south-west corner of the block's anchor cell, so a vertex of the cell at grid
//! `(gx, gy)` sits at `x = ((gx - anchor.0) * 32 + sample_x) * 128` and `-y` likewise.
//!
//! # Canonical edge heights
//!
//! Cells that share an edge carry the same 33 samples twice, and the authored copies disagree by up
//! to a few tens of units. The engine welds them as the cells arrive, the resident neighbour winning,
//! so the stock result depends on streaming order. This module settles the same seams without any
//! order:
//!
//! * Every adjacent pair of cells (east and north neighbours) is a seam. A seam whose largest
//!   sample difference is at most [`MAX_WELDABLE_EDGE_DELTA`] (the engine's bound) is welded; a
//!   larger one is two landscapes authored apart (the city worldspaces) and is left exactly as
//!   authored, like the engine does, and reported in [`TerrainSeamReport::unwelded_edges`].
//! * The samples that sit on one lattice point (a point on a shared edge, or a corner shared by up
//!   to four cells) form groups joined through the welded seams. A group takes the authored height
//!   of its member cell with the smallest `(grid_x, grid_y)`, so the same input always gives the
//!   same heights whatever order the cells are listed or built in.
//! * A group whose heights spread by more than the bound (a chain of small welds across a corner)
//!   is not welded at all, and a lattice point where several groups remain with different heights
//!   (a corner between welded and unwelded seams) keeps them apart. Both are listed in
//!   [`TerrainSeamReport::conflicts`] and every vertex on such a point is locked with
//!   [`LOCK_SEAM_CONFLICT`].
//!
//! Hence: for every welded lattice point, all duplicated vertices have bit-identical heights. At a
//! conflict point they intentionally differ and stay separate topology (no triangle ever joins two
//! cells' vertices; each cell only uses its own), so no skirt or cap is needed.
//!
//! A welded sample that moved by more than the engine's match tolerance gets its normal recomputed
//! from the welded field with the engine's stencil, like `recompute_packed_normals`; all others keep
//! the authored normal. Without authored normals every normal comes from the canonical field.
//!
//! # Locks
//!
//! `locks` has one byte per vertex, a bit set of `LOCK_*`; a non-zero byte keeps the vertex where it
//! is at every level (`meshopt_cluster::Mesh::vertex_lock`). This first version locks the whole
//! boundary of every quadrant, because the engine clips the near terrain per quadrant and a clip may
//! cut any of them, so a seam there must stay identical at every level. That also covers the block's
//! outer edge and every hole, which always lie on quadrant boundaries; [`LOCK_BLOCK_OUTER`] only
//! says why.
//!
//! The price is a simplification floor: the 17x17 interior of a quadrant can collapse to nothing, but
//! the 64 boundary vertices of each quadrant stay, so a cell cannot go below 4 * 62 = 248 triangles
//! (about 12% of the stock 2048) at any level, and a block of `n` cells not below `248 * n`.

use super::model::PrimitiveGeometry;
use color_eyre::{
    Result,
    eyre::{bail, ensure},
};
pub use shared::terrain::{EDGE_MATCH_TOLERANCE, MAX_WELDABLE_EDGE_DELTA};
use std::collections::{BTreeMap, BTreeSet};

/// Samples along one side of a cell's height field.
pub const LAND_SIDE: usize = 33;
const LAND_INTERVALS: usize = LAND_SIDE - 1;
/// Samples along one side of a quadrant mesh.
pub const QUADRANT_SIDE: usize = 17;
const QUADRANT_INTERVALS: usize = QUADRANT_SIDE - 1;
/// Vertices of one quadrant mesh.
pub const VERTICES_PER_QUADRANT: usize = QUADRANT_SIDE * QUADRANT_SIDE;
/// Indices of one quadrant mesh (two triangles per quad).
pub const INDICES_PER_QUADRANT: usize = QUADRANT_INTERVALS * QUADRANT_INTERVALS * 6;
const SAMPLES_PER_CELL: usize = LAND_SIDE * LAND_SIDE;

/// Compute positions in f64 from the shared runtime/converter cell size.
const CELL_SIZE: f64 = shared::terrain::CELL_SIZE as f64;
/// Distance between two height-field samples, 128 units.
const SAMPLE_STEP: f64 = CELL_SIZE / LAND_INTERVALS as f64;

/// The vertex is on the boundary of its quadrant mesh.
pub const LOCK_QUADRANT_BOUNDARY: u8 = 1;
/// The vertex touches the block's outer edge or a hole: a neighbouring cell slot is absent.
pub const LOCK_BLOCK_OUTER: u8 = 2;
/// The vertex is on a lattice point whose duplicated copies keep different heights.
pub const LOCK_SEAM_CONFLICT: u8 = 4;

/// One cell's terrain as the builder reads it.
#[derive(Debug, Clone)]
pub struct TerrainGeometryCell {
    pub grid_x: i32,
    pub grid_y: i32,
    /// The 33x33 height field in Creation units, row-major with x east and y north (index
    /// `y * 33 + x`).
    pub heights: Vec<f32>,
    /// One runtime-axes (Y-up) normal per sample in the same order, or empty to derive them from the
    /// heights. They need not be unit length, only finite and non-zero.
    pub normals: Vec<[f32; 3]>,
}

/// The terrain mesh of a block and the per-vertex locks that go with it.
#[derive(Debug, Clone)]
pub struct TerrainGeometry {
    pub geometry: PrimitiveGeometry,
    /// One byte per vertex, a set of `LOCK_*` bits; see the module documentation.
    pub locks: Vec<u8>,
}

/// Which side of a cell a seam is on, seen from the cell with the smaller coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeamSide {
    East,
    North,
}

/// A seam left as authored because its copies differ by more than [`MAX_WELDABLE_EDGE_DELTA`].
#[derive(Debug, Clone, PartialEq)]
pub struct UnweldedEdge {
    pub cell: (i32, i32),
    pub neighbour: (i32, i32),
    pub side: SeamSide,
    pub max_delta: f32,
}

/// Why a lattice point keeps different heights for its duplicated vertices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictCause {
    /// The point lies on a seam left as authored.
    EdgeLeftAsAuthored,
    /// The welded copies of the point spread by more than the bound, so none of them was moved.
    GroupSpread,
}

/// A lattice point where duplicated vertices intentionally keep different heights.
#[derive(Debug, Clone, PartialEq)]
pub struct SeamConflict {
    /// Absolute lattice coordinates: `grid * 32 + sample`.
    pub point: (i64, i64),
    pub cause: ConflictCause,
    /// The cells meeting at the point, in ascending `(grid_x, grid_y)` order.
    pub cells: Vec<(i32, i32)>,
    /// Their heights at the point after welding, in the same order.
    pub heights: Vec<f32>,
}

/// What the seam settling did, for the stage report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerrainSeamReport {
    /// Adjacent cell pairs whose seam was welded.
    pub welded_edges: u32,
    /// Samples whose height changed by more than [`EDGE_MATCH_TOLERANCE`].
    pub moved_samples: u32,
    /// The largest such change.
    pub max_moved: f32,
    /// Normals recomputed because their sample moved.
    pub normals_recomputed: u32,
    pub unwelded_edges: Vec<UnweldedEdge>,
    pub conflicts: Vec<SeamConflict>,
}

/// Index of a quadrant's vertex in [`TerrainGeometry::geometry`]: `cell_index` is the position of the
/// cell in the input slice, `quadrant` 0..4 (SW SE NW NE), `local_x` and `local_y` 0..=16.
pub const fn vertex_index(
    cell_index: usize,
    quadrant: usize,
    local_x: usize,
    local_y: usize,
) -> u32 {
    ((cell_index * 4 + quadrant) * VERTICES_PER_QUADRANT + local_y * QUADRANT_SIDE + local_x) as u32
}

/// Builds the terrain mesh of a block. See the module documentation for the layout.
///
/// `anchor` is the grid coordinate of the block's south-west cell and `side_cells` its width and
/// height in cells; every cell must lie inside. `cells` must be sorted by `(grid_x, grid_y)` without
/// duplicates (checked), since `atlas_uv` is called with the tile `cell_index * 4 + quadrant` and the
/// local position `(local_x / 16, local_y / 16)` of each vertex, like the LOD terrain compiler. Cells
/// that are absent get no triangles.
pub fn build_terrain_geometry(
    cells: &[TerrainGeometryCell],
    anchor: (i32, i32),
    side_cells: u32,
    atlas_uv: impl Fn(usize, f32, f32) -> [f32; 2],
) -> Result<TerrainGeometry> {
    build_terrain_geometry_with_report(cells, anchor, side_cells, atlas_uv).map(|(built, _)| built)
}

/// As [`build_terrain_geometry`], and says what the seam settling did.
pub fn build_terrain_geometry_with_report(
    cells: &[TerrainGeometryCell],
    anchor: (i32, i32),
    side_cells: u32,
    atlas_uv: impl Fn(usize, f32, f32) -> [f32; 2],
) -> Result<(TerrainGeometry, TerrainSeamReport)> {
    build_geometry(cells, anchor, side_cells, atlas_uv, true)
}

/// Builds a block from a finalized worldspace height field. Never re-weld a subset: local
/// corner decisions can differ from the complete world's conflict decisions.
pub fn build_terrain_geometry_canonical(
    cells: &[TerrainGeometryCell],
    anchor: (i32, i32),
    side_cells: u32,
    atlas_uv: impl Fn(usize, f32, f32) -> [f32; 2],
) -> Result<TerrainGeometry> {
    build_geometry(cells, anchor, side_cells, atlas_uv, false).map(|(geometry, _)| geometry)
}

fn build_geometry(
    cells: &[TerrainGeometryCell],
    anchor: (i32, i32),
    side_cells: u32,
    atlas_uv: impl Fn(usize, f32, f32) -> [f32; 2],
    settle: bool,
) -> Result<(TerrainGeometry, TerrainSeamReport)> {
    validate(cells, anchor, side_cells)?;
    let index_of: BTreeMap<(i64, i64), usize> = cells
        .iter()
        .enumerate()
        .map(|(index, cell)| ((i64::from(cell.grid_x), i64::from(cell.grid_y)), index))
        .collect();
    let settled = if settle {
        settle_seams(cells, &index_of)
    } else {
        SettledSeams {
            heights: cells
                .iter()
                .flat_map(|cell| cell.heights.iter().copied())
                .collect(),
            moved: vec![false; cells.len() * SAMPLES_PER_CELL],
            conflict: vec![false; cells.len() * SAMPLES_PER_CELL],
            report: TerrainSeamReport::default(),
        }
    };

    let vertex_count = cells.len() * 4 * VERTICES_PER_QUADRANT;
    let mut positions = Vec::with_capacity(vertex_count);
    let mut normals = Vec::with_capacity(vertex_count);
    let mut uvs = Vec::with_capacity(vertex_count);
    let mut locks = Vec::with_capacity(vertex_count);
    let mut indices = Vec::with_capacity(cells.len() * 4 * INDICES_PER_QUADRANT);
    let mut normals_recomputed = 0u32;
    for (k, cell) in cells.iter().enumerate() {
        let heights = &settled.heights[k * SAMPLES_PER_CELL..(k + 1) * SAMPLES_PER_CELL];
        let sample_normals: Vec<[f32; 3]> = (0..SAMPLES_PER_CELL)
            .map(|sample| {
                let global = k * SAMPLES_PER_CELL + sample;
                if cell.normals.is_empty() || settled.moved[global] {
                    if !cell.normals.is_empty() {
                        normals_recomputed += 1;
                    }
                    height_normal(heights, sample % LAND_SIDE, sample / LAND_SIDE)
                } else {
                    normalize(cell.normals[sample])
                }
            })
            .collect();
        let rel_x = i64::from(cell.grid_x) - i64::from(anchor.0);
        let rel_y = i64::from(cell.grid_y) - i64::from(anchor.1);
        for quadrant in 0..4 {
            let origin_x = (quadrant % 2) * QUADRANT_INTERVALS;
            let origin_y = (quadrant / 2) * QUADRANT_INTERVALS;
            let base = ((k * 4 + quadrant) * VERTICES_PER_QUADRANT) as u32;
            for local_y in 0..QUADRANT_SIDE {
                for local_x in 0..QUADRANT_SIDE {
                    let (sample_x, sample_y) = (origin_x + local_x, origin_y + local_y);
                    let sample = sample_y * LAND_SIDE + sample_x;
                    positions.push([
                        ((rel_x * LAND_INTERVALS as i64 + sample_x as i64) as f64 * SAMPLE_STEP)
                            as f32,
                        heights[sample],
                        -((rel_y * LAND_INTERVALS as i64 + sample_y as i64) as f64 * SAMPLE_STEP)
                            as f32,
                    ]);
                    normals.push(sample_normals[sample]);
                    let uv = atlas_uv(
                        k * 4 + quadrant,
                        local_x as f32 / QUADRANT_INTERVALS as f32,
                        local_y as f32 / QUADRANT_INTERVALS as f32,
                    );
                    ensure!(
                        uv.iter().all(|value| value.is_finite()),
                        "atlas UV of cell ({}, {}) quadrant {quadrant} vertex ({local_x}, {local_y}) is not finite",
                        cell.grid_x,
                        cell.grid_y
                    );
                    uvs.push(uv);
                    let mut lock = 0;
                    if local_x == 0
                        || local_x == QUADRANT_INTERVALS
                        || local_y == 0
                        || local_y == QUADRANT_INTERVALS
                    {
                        lock |= LOCK_QUADRANT_BOUNDARY;
                    }
                    if touches_absent_cell(&index_of, cell, sample_x, sample_y) {
                        lock |= LOCK_BLOCK_OUTER;
                    }
                    if settled.conflict[k * SAMPLES_PER_CELL + sample] {
                        lock |= LOCK_SEAM_CONFLICT;
                    }
                    locks.push(lock);
                }
            }
            for y in 0..QUADRANT_INTERVALS {
                for x in 0..QUADRANT_INTERVALS {
                    let a = base + (y * QUADRANT_SIDE + x) as u32;
                    let b = a + 1;
                    let c = a + QUADRANT_SIDE as u32;
                    let d = c + 1;
                    indices.extend_from_slice(&[a, b, c, b, d, c]);
                }
            }
        }
    }
    let mut report = settled.report;
    report.normals_recomputed = normals_recomputed;
    let white = vec![[1.0; 4]; positions.len()];
    Ok((
        TerrainGeometry {
            geometry: PrimitiveGeometry {
                positions,
                normals,
                uvs,
                tangents: None,
                colours: Some(white),
                indices,
            },
            locks,
        },
        report,
    ))
}

/// Canonicalizes a complete worldspace before block partitioning, so neighbouring blocks
/// and the engine's near patches receive bit-identical shared heights.
pub fn canonicalize_terrain_cells(cells: &mut [TerrainGeometryCell]) -> Result<TerrainSeamReport> {
    let mut index = BTreeMap::new();
    let mut previous = None;
    for (k, cell) in cells.iter().enumerate() {
        let key = (cell.grid_x, cell.grid_y);
        color_eyre::eyre::ensure!(
            previous.is_none_or(|old| old < key),
            "terrain cells must be sorted and unique"
        );
        color_eyre::eyre::ensure!(
            cell.heights.len() == SAMPLES_PER_CELL && cell.heights.iter().all(|h| h.is_finite()),
            "invalid terrain height field at {key:?}"
        );
        color_eyre::eyre::ensure!(
            cell.normals.is_empty() || cell.normals.len() == SAMPLES_PER_CELL,
            "invalid terrain normal field at {key:?}"
        );
        ensure!(
            cell.normals
                .iter()
                .all(|normal| normal.iter().all(|v| v.is_finite()) && length(*normal) > 0.0),
            "invalid terrain normal at {key:?}"
        );
        index.insert((i64::from(cell.grid_x), i64::from(cell.grid_y)), k);
        previous = Some(key);
    }
    let mut settled = settle_seams(cells, &index);
    for (k, cell) in cells.iter_mut().enumerate() {
        cell.heights
            .copy_from_slice(&settled.heights[k * SAMPLES_PER_CELL..(k + 1) * SAMPLES_PER_CELL]);
        if !cell.normals.is_empty() {
            for y in 0..LAND_SIDE {
                for x in 0..LAND_SIDE {
                    if settled.moved[k * SAMPLES_PER_CELL + y * LAND_SIDE + x] {
                        cell.normals[y * LAND_SIDE + x] = height_normal(&cell.heights, x, y);
                        settled.report.normals_recomputed += 1;
                    }
                }
            }
        }
    }
    Ok(settled.report)
}

fn validate(cells: &[TerrainGeometryCell], anchor: (i32, i32), side_cells: u32) -> Result<()> {
    ensure!(
        side_cells > 0,
        "terrain block must be at least one cell wide"
    );
    ensure!(!cells.is_empty(), "terrain block covers no cells");
    let vertex_total = cells.len().checked_mul(4 * VERTICES_PER_QUADRANT);
    ensure!(
        vertex_total.is_some_and(|total| u32::try_from(total).is_ok()),
        "{} terrain cells need more vertices than a u32 index addresses",
        cells.len()
    );
    for (index, cell) in cells.iter().enumerate() {
        let (grid_x, grid_y) = (cell.grid_x, cell.grid_y);
        if let Some(previous) = index.checked_sub(1).map(|previous| &cells[previous]) {
            let before = (previous.grid_x, previous.grid_y);
            ensure!(
                before != (grid_x, grid_y),
                "terrain cell ({grid_x}, {grid_y}) is listed twice"
            );
            ensure!(
                before < (grid_x, grid_y),
                "terrain cells are not sorted by (grid_x, grid_y): ({}, {}) precedes ({grid_x}, {grid_y})",
                before.0,
                before.1
            );
        }
        let rel_x = i64::from(grid_x) - i64::from(anchor.0);
        let rel_y = i64::from(grid_y) - i64::from(anchor.1);
        ensure!(
            (0..i64::from(side_cells)).contains(&rel_x)
                && (0..i64::from(side_cells)).contains(&rel_y),
            "terrain cell ({grid_x}, {grid_y}) is outside the {side_cells}x{side_cells} block anchored at ({}, {})",
            anchor.0,
            anchor.1
        );
        ensure!(
            cell.heights.len() == SAMPLES_PER_CELL,
            "terrain cell ({grid_x}, {grid_y}) has {} heights, expected {SAMPLES_PER_CELL}",
            cell.heights.len()
        );
        ensure!(
            cell.heights.iter().all(|height| height.is_finite()),
            "terrain cell ({grid_x}, {grid_y}) has a non-finite height"
        );
        if !cell.normals.is_empty() {
            ensure!(
                cell.normals.len() == SAMPLES_PER_CELL,
                "terrain cell ({grid_x}, {grid_y}) has {} normals, expected 0 or {SAMPLES_PER_CELL}",
                cell.normals.len()
            );
            for normal in &cell.normals {
                let length = length(*normal);
                if !length.is_finite() || length <= 0.0 {
                    bail!(
                        "terrain cell ({grid_x}, {grid_y}) has a normal that is not finite or has no length: {normal:?}"
                    );
                }
            }
        }
    }
    Ok(())
}

fn length(vector: [f32; 3]) -> f64 {
    vector
        .iter()
        .map(|component| f64::from(*component) * f64::from(*component))
        .sum::<f64>()
        .sqrt()
}

/// The vector scaled to unit length; validated to have a finite non-zero length.
fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let length = length(vector);
    vector.map(|component| (f64::from(component) / length) as f32)
}

/// The normal of a height field sample, in runtime axes: the engine's stencil (the height difference
/// of each axis over the distance it spans, two intervals inside the field and one at an edge), as
/// Creation `(-dx, -dy, 1)` turned into runtime `(x, z, -y)`. `heights` is one cell's 33x33 field.
fn height_normal(heights: &[f32], x: usize, y: usize) -> [f32; 3] {
    let at = |x: usize, y: usize| f64::from(heights[y * LAND_SIDE + x]);
    let (west, east) = (x.saturating_sub(1), (x + 1).min(LAND_INTERVALS));
    let (south, north) = (y.saturating_sub(1), (y + 1).min(LAND_INTERVALS));
    let dx = (at(east, y) - at(west, y)) / ((east - west) as f64 * SAMPLE_STEP);
    let dy = (at(x, north) - at(x, south)) / ((north - south) as f64 * SAMPLE_STEP);
    let length = (dx * dx + dy * dy + 1.0).sqrt();
    [
        (-dx / length) as f32,
        (1.0 / length) as f32,
        (dy / length) as f32,
    ]
}

/// Whether one of the cell slots around a sample is absent from the block. A sample inside a cell
/// has only its own slot; one on an edge also the neighbour's, one on a corner the three others.
fn touches_absent_cell(
    index_of: &BTreeMap<(i64, i64), usize>,
    cell: &TerrainGeometryCell,
    sample_x: usize,
    sample_y: usize,
) -> bool {
    let slots = |grid: i32, sample: usize| -> [Option<i64>; 2] {
        let grid = i64::from(grid);
        match sample {
            0 => [Some(grid - 1), Some(grid)],
            LAND_INTERVALS => [Some(grid), Some(grid + 1)],
            _ => [Some(grid), None],
        }
    };
    for x in slots(cell.grid_x, sample_x).into_iter().flatten() {
        for y in slots(cell.grid_y, sample_y).into_iter().flatten() {
            if !index_of.contains_key(&(x, y)) {
                return true;
            }
        }
    }
    false
}

/// The settled heights of every sample (flat, `cell * 1089 + y * 33 + x`) with the flags the build
/// needs.
struct SettledSeams {
    heights: Vec<f32>,
    /// The sample moved by more than the match tolerance.
    moved: Vec<bool>,
    /// The sample sits on a conflict point.
    conflict: Vec<bool>,
    report: TerrainSeamReport,
}

/// One sample of a lattice point: the owning cell's index and the flat sample index.
type Member = (usize, usize);

fn settle_seams(
    cells: &[TerrainGeometryCell],
    index_of: &BTreeMap<(i64, i64), usize>,
) -> SettledSeams {
    let mut settled = SettledSeams {
        heights: cells
            .iter()
            .flat_map(|cell| cell.heights.iter().copied())
            .collect(),
        moved: vec![false; cells.len() * SAMPLES_PER_CELL],
        conflict: vec![false; cells.len() * SAMPLES_PER_CELL],
        report: TerrainSeamReport::default(),
    };
    let sample = |k: usize, x: usize, y: usize| k * SAMPLES_PER_CELL + y * LAND_SIDE + x;

    // Decide every seam once: each cell against its east and north neighbours.
    let mut welded: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut seams = Vec::new();
    for (k, cell) in cells.iter().enumerate() {
        let (grid_x, grid_y) = (i64::from(cell.grid_x), i64::from(cell.grid_y));
        for side in [SeamSide::East, SeamSide::North] {
            let neighbour = match side {
                SeamSide::East => (grid_x + 1, grid_y),
                SeamSide::North => (grid_x, grid_y + 1),
            };
            let Some(&n) = index_of.get(&neighbour) else {
                continue;
            };
            let pair = |i: usize| match side {
                SeamSide::East => (sample(k, LAND_INTERVALS, i), sample(n, 0, i)),
                SeamSide::North => (sample(k, i, LAND_INTERVALS), sample(n, i, 0)),
            };
            let max_delta = (0..LAND_SIDE)
                .map(|i| {
                    let (own, other) = pair(i);
                    (settled.heights[own] - settled.heights[other]).abs()
                })
                .fold(0.0_f32, f32::max);
            if max_delta > MAX_WELDABLE_EDGE_DELTA {
                settled.report.unwelded_edges.push(UnweldedEdge {
                    cell: (cell.grid_x, cell.grid_y),
                    neighbour: (cells[n].grid_x, cells[n].grid_y),
                    side,
                    max_delta,
                });
            } else {
                welded.insert((k, n));
                settled.report.welded_edges += 1;
            }
            seams.push((k, n, side));
        }
    }

    // Points inside a shared edge belong to two cells.
    for &(k, n, side) in &seams {
        let (grid_x, grid_y) = (i64::from(cells[k].grid_x), i64::from(cells[k].grid_y));
        for i in 1..LAND_INTERVALS {
            let (members, point) = match side {
                SeamSide::East => (
                    [(k, sample(k, LAND_INTERVALS, i)), (n, sample(n, 0, i))],
                    (
                        (grid_x + 1) * LAND_INTERVALS as i64,
                        grid_y * LAND_INTERVALS as i64 + i as i64,
                    ),
                ),
                SeamSide::North => (
                    [(k, sample(k, i, LAND_INTERVALS)), (n, sample(n, i, 0))],
                    (
                        grid_x * LAND_INTERVALS as i64 + i as i64,
                        (grid_y + 1) * LAND_INTERVALS as i64,
                    ),
                ),
            };
            settle_point(&mut settled, cells, &welded, &members, point);
        }
    }

    // Cell corners belong to up to four cells: list each corner once, in order.
    let mut corners = BTreeSet::new();
    for cell in cells {
        let (grid_x, grid_y) = (i64::from(cell.grid_x), i64::from(cell.grid_y));
        for corner_x in [grid_x, grid_x + 1] {
            for corner_y in [grid_y, grid_y + 1] {
                corners.insert((corner_x, corner_y));
            }
        }
    }
    for (corner_x, corner_y) in corners {
        // Ascending (grid_x, grid_y): SW, NW, SE, NE of the corner.
        let slots = [
            (
                (corner_x - 1, corner_y - 1),
                (LAND_INTERVALS, LAND_INTERVALS),
            ),
            ((corner_x - 1, corner_y), (LAND_INTERVALS, 0)),
            ((corner_x, corner_y - 1), (0, LAND_INTERVALS)),
            ((corner_x, corner_y), (0, 0)),
        ];
        let members: Vec<Member> = slots
            .iter()
            .filter_map(|(grid, (x, y))| index_of.get(grid).map(|&k| (k, sample(k, *x, *y))))
            .collect();
        let point = (
            corner_x * LAND_INTERVALS as i64,
            corner_y * LAND_INTERVALS as i64,
        );
        settle_point(&mut settled, cells, &welded, &members, point);
    }
    settled
}

/// Settles the copies of one lattice point. `members` are in ascending cell order.
fn settle_point(
    settled: &mut SettledSeams,
    cells: &[TerrainGeometryCell],
    welded: &BTreeSet<(usize, usize)>,
    members: &[Member],
    point: (i64, i64),
) {
    let count = members.len();
    if count < 2 {
        return;
    }
    // Join the copies through welded seams: `group[i]` is the first member of i's group.
    let mut group: Vec<usize> = (0..count).collect();
    for (i, left) in members.iter().enumerate() {
        for (j, right) in members.iter().enumerate().skip(i + 1) {
            if welded.contains(&(left.0, right.0)) {
                let (keep, absorbed) = (group[i].min(group[j]), group[i].max(group[j]));
                for entry in &mut group {
                    if *entry == absorbed {
                        *entry = keep;
                    }
                }
            }
        }
    }
    let authored: Vec<f32> = members.iter().map(|&(_, at)| settled.heights[at]).collect();
    let mut spread_too_wide = false;
    let canonical: Vec<f32> = (0..count)
        .map(|i| {
            let siblings = || (0..count).filter(|&j| group[j] == group[i]);
            let low = siblings()
                .map(|j| authored[j])
                .fold(f32::INFINITY, f32::min);
            let high = siblings()
                .map(|j| authored[j])
                .fold(f32::NEG_INFINITY, f32::max);
            if high - low > MAX_WELDABLE_EDGE_DELTA {
                spread_too_wide = true;
                authored[i]
            } else {
                authored[group[i]]
            }
        })
        .collect();
    for (i, &(_, at)) in members.iter().enumerate() {
        let change = (canonical[i] - authored[i]).abs();
        settled.heights[at] = canonical[i];
        if change > EDGE_MATCH_TOLERANCE {
            settled.moved[at] = true;
            settled.report.moved_samples += 1;
            settled.report.max_moved = settled.report.max_moved.max(change);
        }
    }
    if canonical.iter().any(|height| *height != canonical[0]) {
        for &(_, at) in members {
            settled.conflict[at] = true;
        }
        settled.report.conflicts.push(SeamConflict {
            point,
            cause: if spread_too_wide {
                ConflictCause::GroupSpread
            } else {
                ConflictCause::EdgeLeftAsAuthored
            },
            cells: members
                .iter()
                .map(|&(k, _)| (cells[k].grid_x, cells[k].grid_y))
                .collect(),
            heights: canonical,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(grid_x: i32, grid_y: i32, height: impl Fn(usize, usize) -> f32) -> TerrainGeometryCell {
        TerrainGeometryCell {
            grid_x,
            grid_y,
            heights: (0..SAMPLES_PER_CELL)
                .map(|sample| height(sample % LAND_SIDE, sample / LAND_SIDE))
                .collect(),
            normals: Vec::new(),
        }
    }

    fn flat(grid_x: i32, grid_y: i32, height: f32) -> TerrainGeometryCell {
        cell(grid_x, grid_y, |_, _| height)
    }

    /// Tile `t` at `(u, v)` as `[t * 10 + u, v]`, so every call is recognisable in the output.
    fn tile_uv(tile: usize, u: f32, v: f32) -> [f32; 2] {
        [tile as f32 * 10.0 + u, v]
    }

    fn build(
        cells: &[TerrainGeometryCell],
        anchor: (i32, i32),
        side: u32,
    ) -> (TerrainGeometry, TerrainSeamReport) {
        build_terrain_geometry_with_report(cells, anchor, side, tile_uv).unwrap()
    }

    fn position(built: &TerrainGeometry, k: usize, q: usize, x: usize, y: usize) -> [f32; 3] {
        built.geometry.positions[vertex_index(k, q, x, y) as usize]
    }

    fn lock(built: &TerrainGeometry, k: usize, q: usize, x: usize, y: usize) -> u8 {
        built.locks[vertex_index(k, q, x, y) as usize]
    }

    /// Every xz position carries one height, so the duplicated edge vertices are watertight.
    fn assert_shared_positions_identical(built: &TerrainGeometry) {
        let mut seen: BTreeMap<(u32, u32), f32> = BTreeMap::new();
        for position in &built.geometry.positions {
            let previous = seen
                .entry((position[0].to_bits(), position[2].to_bits()))
                .or_insert(position[1]);
            assert_eq!(*previous, position[1], "two heights at {position:?}");
        }
    }

    #[test]
    fn single_cell_matches_the_stock_mesh() {
        let (built, report) = build(&[flat(0, 0, 5.0)], (0, 0), 1);
        let geometry = &built.geometry;
        assert_eq!(geometry.positions.len(), 4 * 289);
        assert_eq!(geometry.normals.len(), 4 * 289);
        assert_eq!(geometry.uvs.len(), 4 * 289);
        assert_eq!(built.locks.len(), 4 * 289);
        assert_eq!(geometry.indices.len(), 4 * 16 * 16 * 6);
        assert_eq!(geometry.triangle_count(), 2048);
        assert_eq!(report, TerrainSeamReport::default());
        // The stock quad: a, b, c then b, d, c, with a = y * 17 + x.
        assert_eq!(geometry.indices[..6], [0, 1, 17, 1, 18, 17]);
        assert_eq!(
            geometry.indices[INDICES_PER_QUADRANT + 6..INDICES_PER_QUADRANT + 12],
            [289 + 1, 289 + 2, 289 + 18, 289 + 2, 289 + 19, 289 + 18]
        );
        assert!(
            geometry
                .indices
                .iter()
                .all(|index| (*index as usize) < 4 * 289)
        );
        // x east, y up, -y south-to-north, each quadrant offset by 16 steps of 128.
        assert_eq!(position(&built, 0, 0, 0, 0), [0.0, 5.0, 0.0]);
        assert_eq!(position(&built, 0, 0, 1, 2), [128.0, 5.0, -256.0]);
        assert_eq!(position(&built, 0, 1, 0, 0), [2048.0, 5.0, 0.0]);
        assert_eq!(position(&built, 0, 2, 0, 0), [0.0, 5.0, -2048.0]);
        assert_eq!(position(&built, 0, 3, 16, 16), [4096.0, 5.0, -4096.0]);
        assert!(geometry.tangents.is_none());
        assert_eq!(
            geometry.colours.as_deref(),
            Some(&vec![[1.0; 4]; 4 * 289][..])
        );
    }

    #[test]
    fn every_triangle_faces_up() {
        let hilly = cell(0, 0, |x, y| (x * 7 % 13) as f32 + (y * 5 % 11) as f32);
        let (built, _) = build(&[hilly], (0, 0), 1);
        let positions = &built.geometry.positions;
        for triangle in built.geometry.indices.as_chunks::<3>().0 {
            let [a, b, c] = [0, 1, 2].map(|i| positions[triangle[i] as usize]);
            let (e1, e2) = ([b[0] - a[0], b[2] - a[2]], [c[0] - a[0], c[2] - a[2]]);
            // The y component of (b - a) x (c - a): positive is counter-clockwise from above.
            assert!(e1[1] * e2[0] - e1[0] * e2[1] > 0.0);
        }
    }

    #[test]
    fn adjacent_cells_weld_their_shared_edge_to_the_lower_cell() {
        let west = cell(0, 0, |x, _| if x == 32 { 10.0 } else { 0.0 });
        let east = cell(1, 0, |x, _| if x == 0 { 12.0 } else { 0.0 });
        let (built, report) = build(&[west, east], (0, 0), 2);
        assert_eq!(report.welded_edges, 1);
        assert!(report.unwelded_edges.is_empty());
        assert!(report.conflicts.is_empty());
        // 33 samples of the east cell moved by 2.
        assert_eq!(report.moved_samples, 33);
        assert_eq!(report.max_moved, 2.0);
        for y in 0..=16 {
            let own = position(&built, 0, 1, 16, y);
            let other = position(&built, 1, 0, 0, y);
            assert_eq!(own, other);
            assert_eq!(own[1], 10.0);
            assert_eq!(own[0], 4096.0);
        }
        assert_shared_positions_identical(&built);
    }

    #[test]
    fn the_canonical_height_follows_the_cell_coordinate_not_the_value() {
        // The same seam with the authored values swapped: the lower cell still wins.
        let west = cell(0, 0, |x, _| if x == 32 { 12.0 } else { 0.0 });
        let east = cell(1, 0, |x, _| if x == 0 { 10.0 } else { 0.0 });
        let (built, _) = build(&[west, east], (0, 0), 2);
        assert_eq!(position(&built, 1, 0, 0, 7)[1], 12.0);
        assert_eq!(position(&built, 0, 1, 16, 7)[1], 12.0);
    }

    #[test]
    fn a_four_cell_corner_takes_one_height() {
        let cells = [
            flat(0, 0, 1.0),
            flat(0, 1, 2.0),
            flat(1, 0, 3.0),
            flat(1, 1, 4.0),
        ];
        let (built, report) = build(&cells, (0, 0), 2);
        assert_eq!(report.welded_edges, 4);
        assert!(report.conflicts.is_empty());
        // The corner at (32, 32): SW's NE vertex, NW's SE, SE's NW, NE's SW.
        let corner = [
            position(&built, 0, 3, 16, 16),
            position(&built, 1, 1, 16, 0),
            position(&built, 2, 2, 0, 16),
            position(&built, 3, 0, 0, 0),
        ];
        assert!(corner.iter().all(|point| *point == [4096.0, 1.0, -4096.0]));
        assert_shared_positions_identical(&built);
    }

    #[test]
    fn translating_the_block_does_not_change_the_mesh() {
        let make = |dx: i32, dy: i32| {
            [
                cell(dx, dy, |x, y| (x + y) as f32),
                cell(dx, dy + 1, |x, y| (x * 2 + y) as f32 + 3.0),
                cell(dx + 1, dy, |x, y| (x + y * 3) as f32 - 4.0),
                // Keep this translation fixture's four seams within 64 units.
                cell(dx + 1, dy + 1, |x, _| x as f32 + 40.0),
            ]
        };
        let (a, report_a) = build(&make(0, 0), (0, 0), 2);
        let (b, report_b) = build(&make(-5, 7), (-5, 7), 2);
        assert_eq!(a.geometry.positions, b.geometry.positions);
        assert_eq!(a.geometry.normals, b.geometry.normals);
        assert_eq!(a.geometry.indices, b.geometry.indices);
        assert_eq!(a.locks, b.locks);
        assert_eq!(report_a.moved_samples, report_b.moved_samples);
        assert_shared_positions_identical(&a);
    }

    #[test]
    fn negative_coordinates_are_block_local() {
        let cells = [flat(-3, -2, 1.0), flat(-2, -2, 1.0)];
        let (built, _) = build(&cells, (-3, -2), 2);
        assert_eq!(position(&built, 0, 0, 0, 0), [0.0, 1.0, 0.0]);
        assert_eq!(position(&built, 1, 0, 0, 0), [4096.0, 1.0, 0.0]);
        assert_eq!(position(&built, 1, 3, 16, 16), [8192.0, 1.0, -4096.0]);
    }

    #[test]
    fn a_hole_gets_no_triangles_and_its_rim_is_locked() {
        let cells = [flat(0, 0, 0.0), flat(0, 1, 0.0), flat(1, 0, 0.0)];
        let (built, _) = build(&cells, (0, 0), 2);
        let positions = &built.geometry.positions;
        for triangle in built.geometry.indices.as_chunks::<3>().0 {
            let centre_x: f32 = triangle
                .iter()
                .map(|i| positions[*i as usize][0])
                .sum::<f32>()
                / 3.0;
            let centre_z: f32 = triangle
                .iter()
                .map(|i| positions[*i as usize][2])
                .sum::<f32>()
                / 3.0;
            // The missing cell (1, 1) covers x > 4096 and z < -4096.
            assert!(!(centre_x > 4096.0 && centre_z < -4096.0));
        }
        // The corner shared with the hole, an inner-cell interior, a shared edge, a block edge.
        let corner = lock(&built, 0, 3, 16, 16);
        assert_eq!(corner, LOCK_QUADRANT_BOUNDARY | LOCK_BLOCK_OUTER);
        assert_eq!(lock(&built, 0, 0, 8, 8), 0);
        assert_eq!(lock(&built, 0, 1, 16, 8), LOCK_QUADRANT_BOUNDARY);
        assert_eq!(
            lock(&built, 0, 0, 0, 8),
            LOCK_QUADRANT_BOUNDARY | LOCK_BLOCK_OUTER
        );
        // The rim of the hole on the cell above and beside it.
        assert_eq!(
            lock(&built, 1, 1, 16, 8),
            LOCK_QUADRANT_BOUNDARY | LOCK_BLOCK_OUTER
        );
        assert_eq!(
            lock(&built, 2, 2, 8, 16),
            LOCK_QUADRANT_BOUNDARY | LOCK_BLOCK_OUTER
        );
    }

    #[test]
    fn every_quadrant_boundary_vertex_is_locked_and_no_other() {
        let (built, _) = build(&[flat(0, 0, 0.0)], (0, 0), 1);
        let locked = built.locks.iter().filter(|lock| **lock != 0).count();
        assert_eq!(locked, 4 * (17 * 17 - 15 * 15));
        for quadrant in 0..4 {
            for y in 0..17 {
                for x in 0..17 {
                    let edge = [0, 16].contains(&x) || [0, 16].contains(&y);
                    assert_eq!(lock(&built, 0, quadrant, x, y) != 0, edge);
                }
            }
        }
    }

    #[test]
    fn atlas_tiles_follow_cell_index_and_quadrant() {
        let cells = [flat(0, 0, 0.0), flat(1, 0, 0.0)];
        let (built, _) = build(&cells, (0, 0), 2);
        let uv = |k: usize, q: usize, x: usize, y: usize| {
            built.geometry.uvs[vertex_index(k, q, x, y) as usize]
        };
        // Cell 1, NW quadrant is tile 6; its (16, 0) vertex is u = 1, v = 0.
        assert_eq!(uv(1, 2, 16, 0), [61.0, 0.0]);
        assert_eq!(uv(0, 3, 4, 8), [30.25, 0.5]);
        assert_eq!(uv(1, 0, 0, 16), [40.0, 1.0]);
        // The same position in two quadrants keeps two UVs.
        assert_ne!(uv(0, 0, 16, 0), uv(0, 1, 0, 0));
    }

    #[test]
    fn bad_input_is_an_error() {
        let ok = flat(0, 0, 0.0);
        let attempt = |cells: &[TerrainGeometryCell], anchor, side| {
            build_terrain_geometry(cells, anchor, side, tile_uv).map(|_| ())
        };
        assert!(attempt(std::slice::from_ref(&ok), (0, 0), 1).is_ok());
        assert!(attempt(&[], (0, 0), 1).is_err());
        assert!(attempt(std::slice::from_ref(&ok), (0, 0), 0).is_err());

        let unsorted = attempt(&[flat(1, 0, 0.0), flat(0, 0, 0.0)], (0, 0), 2);
        assert!(unsorted.unwrap_err().to_string().contains("not sorted"));
        let duplicate = attempt(&[ok.clone(), ok.clone()], (0, 0), 2);
        assert!(duplicate.unwrap_err().to_string().contains("twice"));
        let outside = attempt(&[ok.clone(), flat(1, 0, 0.0)], (0, 0), 1);
        assert!(outside.unwrap_err().to_string().contains("outside"));
        assert!(attempt(std::slice::from_ref(&ok), (1, 0), 1).is_err());

        let mut short = ok.clone();
        short.heights.pop();
        assert!(attempt(&[short], (0, 0), 1).is_err());
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut cell = ok.clone();
            cell.heights[500] = bad;
            assert!(attempt(&[cell], (0, 0), 1).is_err());
        }

        let mut few_normals = ok.clone();
        few_normals.normals = vec![[0.0, 1.0, 0.0]; 10];
        assert!(attempt(&[few_normals], (0, 0), 1).is_err());
        let mut nan_normal = ok.clone();
        nan_normal.normals = vec![[0.0, 1.0, 0.0]; SAMPLES_PER_CELL];
        nan_normal.normals[3] = [f32::NAN, 1.0, 0.0];
        assert!(attempt(&[nan_normal], (0, 0), 1).is_err());
        let mut zero_normal = ok.clone();
        zero_normal.normals = vec![[0.0, 1.0, 0.0]; SAMPLES_PER_CELL];
        zero_normal.normals[3] = [0.0; 3];
        assert!(attempt(&[zero_normal], (0, 0), 1).is_err());

        let bad_uv = build_terrain_geometry(&[ok], (0, 0), 1, |_, _, _| [f32::NAN, 0.0]);
        assert!(bad_uv.is_err());
    }

    #[test]
    fn derived_normals_point_up_and_away_from_the_slope() {
        // Rising east by 2 per sample: the surface faces west (-x), upward, no z.
        let ramp_east = cell(0, 0, |x, _| 2.0 * x as f32);
        // Rising north by 3 per sample: it faces south, which is +z in runtime axes.
        let ramp_north = cell(1, 0, |_, y| 3.0 * y as f32);
        let (built, _) = build(&[ramp_east, ramp_north], (0, 0), 2);
        let normal = |k: usize, x: usize, y: usize| {
            built.geometry.normals[vertex_index(k, 0, x, y) as usize]
        };
        let east = normal(0, 5, 5);
        assert!(east[0] < 0.0 && east[1] > 0.0 && east[2].abs() < 1e-6);
        let slope = 2.0 / 128.0;
        assert!((east[0] / east[1] + slope).abs() < 1e-6);
        // A planar slope keeps its normal at the field's edge (one interval, not two).
        assert_eq!(normal(0, 0, 0), east);
        let north = normal(1, 5, 5);
        assert!(north[2] > 0.0 && north[1] > 0.0 && north[0].abs() < 1e-6);
        assert!((north[2] / north[1] - 3.0 / 128.0).abs() < 1e-6);
        for vector in &built.geometry.normals {
            assert!((length(*vector) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn authored_normals_are_kept_unless_the_sample_moved() {
        let mut flat_west = flat(0, 0, 0.0);
        flat_west.normals = vec![[0.0, 2.0, 0.0]; SAMPLES_PER_CELL];
        // The east cell's west edge sits 5 above and must be pulled down onto the west cell.
        let mut east = cell(1, 0, |x, _| if x == 0 { 5.0 } else { 0.0 });
        let tilted = [1.0, 1.0, 0.0];
        east.normals = vec![tilted; SAMPLES_PER_CELL];
        let (built, report) = build(&[flat_west, east], (0, 0), 2);
        let normal = |k: usize, x: usize, y: usize| {
            built.geometry.normals[vertex_index(k, 0, x, y) as usize]
        };
        // Authored, normalised.
        assert_eq!(normal(0, 4, 4), [0.0, 1.0, 0.0]);
        let kept = normal(1, 5, 4);
        assert!(
            (kept[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6
                && (kept[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6
        );
        // Moved samples are recomputed from the welded field: flat again.
        assert_eq!(normal(1, 0, 4), [0.0, 1.0, 0.0]);
        assert_eq!(report.normals_recomputed, report.moved_samples);
    }

    #[test]
    fn weld_policy_preserves_authored_edges_above_64_units() {
        for delta in [65.0, 512.0] {
            for (x, y) in [(1, 0), (0, 1)] {
                let cells = [flat(0, 0, 0.0), flat(x, y, delta)];
                let (built, report) = build(&cells, (0, 0), 2);
                assert_eq!(report.welded_edges, 0, "delta {delta}");
                assert_eq!(report.moved_samples, 0, "delta {delta}");
                assert_eq!(report.unwelded_edges.len(), 1, "delta {delta}");
                for sample in 0..=16 {
                    let (q, own_x, own_y, other_x, other_y) = if x == 1 {
                        (1, 16, sample, 0, sample)
                    } else {
                        (2, sample, 16, sample, 0)
                    };
                    assert_eq!(position(&built, 0, q, own_x, own_y)[1], 0.0);
                    assert_eq!(position(&built, 1, 0, other_x, other_y)[1], delta);
                    assert_ne!(lock(&built, 0, q, own_x, own_y) & LOCK_SEAM_CONFLICT, 0);
                    assert_ne!(lock(&built, 1, 0, other_x, other_y) & LOCK_SEAM_CONFLICT, 0);
                }
            }
        }
    }

    #[test]
    fn weld_policy_closes_edges_through_64_units_in_both_directions() {
        for delta in [0.0, 0.01, 24.0, 63.0, 64.0] {
            for (x, y) in [(1, 0), (0, 1)] {
                let cells = [flat(0, 0, 0.0), flat(x, y, delta)];
                let (built, report) = build(&cells, (0, 0), 2);
                assert_eq!(report.welded_edges, 1, "delta {delta}, ({x}, {y})");
                assert!(report.unwelded_edges.is_empty());
                assert!(report.conflicts.is_empty());
                assert_shared_positions_identical(&built);
            }
        }
    }

    #[test]
    fn a_cliff_seam_stays_as_authored_and_locked() {
        let cells = [flat(0, 0, 0.0), flat(1, 0, 1000.0)];
        let (built, report) = build(&cells, (0, 0), 2);
        assert_eq!(report.welded_edges, 0);
        assert_eq!(report.moved_samples, 0);
        assert_eq!(
            report.unwelded_edges,
            vec![UnweldedEdge {
                cell: (0, 0),
                neighbour: (1, 0),
                side: SeamSide::East,
                max_delta: 1000.0,
            }]
        );
        // Two vertices at one position keep two heights, neither pulled toward the other.
        let own = position(&built, 0, 1, 16, 8);
        let other = position(&built, 1, 0, 0, 8);
        assert_eq!((own[0], own[2]), (other[0], other[2]));
        assert_eq!((own[1], other[1]), (0.0, 1000.0));
        assert_ne!(lock(&built, 0, 1, 16, 8) & LOCK_SEAM_CONFLICT, 0);
        assert_ne!(lock(&built, 1, 0, 0, 8) & LOCK_SEAM_CONFLICT, 0);
        // An interior vertex and the far edge are not part of the conflict.
        assert_eq!(lock(&built, 0, 0, 8, 8), 0);
        assert_eq!(lock(&built, 0, 0, 0, 8) & LOCK_SEAM_CONFLICT, 0);
        assert!(
            report
                .conflicts
                .iter()
                .all(|conflict| conflict.cause == ConflictCause::EdgeLeftAsAuthored)
        );
        // 31 points inside the edge and the two corners.
        assert_eq!(report.conflicts.len(), 33);
    }

    #[test]
    fn a_corner_between_welded_and_unwelded_seams_keeps_both_heights() {
        // South row flat 0, north row flat 1000: the east-west seams weld, the north-south do not.
        let cells = [
            flat(0, 0, 0.0),
            flat(0, 1, 1000.0),
            flat(1, 0, 0.0),
            flat(1, 1, 1000.0),
        ];
        let (built, report) = build(&cells, (0, 0), 2);
        assert_eq!(report.welded_edges, 2);
        assert_eq!(report.unwelded_edges.len(), 2);
        let corner = report
            .conflicts
            .iter()
            .find(|conflict| conflict.point == (32, 32))
            .expect("the shared corner is reported");
        assert_eq!(corner.cause, ConflictCause::EdgeLeftAsAuthored);
        assert_eq!(corner.cells, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
        assert_eq!(corner.heights, vec![0.0, 1000.0, 0.0, 1000.0]);
        for (k, q, x, y) in [(0, 3, 16, 16), (1, 1, 16, 0), (2, 2, 0, 16), (3, 0, 0, 0)] {
            assert_ne!(lock(&built, k, q, x, y) & LOCK_SEAM_CONFLICT, 0);
        }
    }

    #[test]
    fn a_chain_of_small_welds_that_spreads_too_far_is_not_welded() {
        // SW 0 - NW 40 - NE 80 are each within the 64-unit bound of the next, and SE (0)
        // joins SW: the corner spans 80, though each seam in that chain is weldable.
        let cells = [
            flat(0, 0, 0.0),
            flat(0, 1, 40.0),
            flat(1, 0, 0.0),
            flat(1, 1, 80.0),
        ];
        let (_, report) = build(&cells, (0, 0), 2);
        let corner = report
            .conflicts
            .iter()
            .find(|conflict| conflict.point == (32, 32))
            .expect("the shared corner is reported");
        assert_eq!(corner.cause, ConflictCause::GroupSpread);
        // Nothing at the corner moved: the authored heights stay.
        assert_eq!(corner.heights, vec![0.0, 40.0, 0.0, 80.0]);
    }
    #[test]
    fn finalized_world_corners_are_not_rewelded_when_partitioned() {
        let mut cells: Vec<_> = [(0, 0, 0.0), (0, 1, 40.0), (1, 0, 0.0), (1, 1, 80.0)]
            .into_iter()
            .map(|(grid_x, grid_y, h)| TerrainGeometryCell {
                grid_x,
                grid_y,
                heights: vec![h; SAMPLES_PER_CELL],
                normals: Vec::new(),
            })
            .collect();
        canonicalize_terrain_cells(&mut cells).unwrap();
        for cell in &cells {
            let block = build_terrain_geometry_canonical(
                std::slice::from_ref(cell),
                (cell.grid_x, cell.grid_y),
                1,
                |_, u, v| [u, v],
            )
            .unwrap();
            for quadrant in 0..4 {
                for y in 0..=16 {
                    for x in 0..=16 {
                        let offset_x = (quadrant % 2) * 16;
                        let offset_y = (quadrant / 2) * 16;
                        assert_eq!(
                            block.geometry.positions[vertex_index(0, quadrant, x, y) as usize][1],
                            cell.heights[(offset_y + y) * LAND_SIDE + offset_x + x]
                        );
                    }
                }
            }
        }
    }
}
