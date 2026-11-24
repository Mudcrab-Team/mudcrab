//! Compiles retail terrain LOD blocks (`.btr`, colour map, normal map) into terrain LOD chunks.
//!
//! One retail block of level 4, 8 or 16 becomes one chunk of the same tier and anchor. The chunk
//! keeps the layout the runtime already reads: a node per source cell, a `terrain` group, and four
//! quadrant nodes with one mesh each, so handoff to full-detail cells works as before.
//!
//! Nothing here invents a value. Geometry is the retail `land` shape, cut along the quadrant
//! boundaries (a cut interpolates retail positions and UVs along retail triangle edges, so the
//! surface is the same surface). The colour map is the retail map. The normal map is the retail
//! map with its channels moved into the glTF tangent-space layout ([`RETAIL_NORMAL_AXES`]).
//!
//! Reviewed facts this follows, from <https://github.com/Mudcrab-Team/skyrim_decomp/pull/822>
//! (static analysis, independently reviewed; the finding is `UNREVIEWED` for transfer):
//! - The `.btr` vertex is position and UV0 only. There is no vertex normal, tangent or colour, so
//!   retail has no vertex-normal term: the lit normal comes from the per-block normal map alone.
//!   The importer computes no normal from geometry.
//! - The normal is `N = 2 * ((R, B, G) - 0.5)`: X from red, Y from blue, Z from green, model
//!   space, alpha unused. [`RETAIL_NORMAL_AXES`] is that decode with the model axes as the glTF
//!   frame (east, north, up), and the ignored real-data test measures it against the mesh.
//! - The diffuse map is slot 0 and the normal map slot 1, both sampled with linear min and mag,
//!   a point mip filter and MaxLOD 0, so only the first mip is sampled. Each map is therefore
//!   stored as its first mip alone.
//!
//! The `NORMAL` and `TANGENT` attributes are a constant reference frame so the engine's glTF
//! material can apply the map as a model-space normal: `NORMAL` is straight up and `TANGENT`
//! points east, which is how the retail UVs lay the map on the ground. They carry no lighting
//! data and no geometry. Not done here, and unverified: the retail lighting equation, noise
//! term, near-terrain overlap and the settings in the finding; the engine lights these chunks
//! with its ordinary material.

use super::{
    retail::btr::TerrainLodShape,
    terrain::{TerrainChunk, f32_slice_bytes, pad_to_4},
};
use crate::texture::{TextureConverter, TextureEncoding};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure, eyre},
};
use shared::lod::{ChunkAnchor, ChunkKey, LodOrigin, LodTier, RETAIL_TERRAIN_MATERIAL_NAME, nodes};
use std::collections::{BTreeMap, HashMap};

/// `asset.generator` of a chunk written by this module. The terrain GLB validator dispatches on it.
pub const GENERATOR: &str = "Mudcrab retail terrain LOD import";
/// Revision of this importer's output. It goes into the LOD manifest.
pub const IMPORT_VERSION: u32 = 2;

/// Exterior cells of 4096 Creation units.
const CELL_SIZE: f64 = 4096.0;
/// A quadrant is half a cell on each side.
const QUADRANT_SIZE: f64 = CELL_SIZE / 2.0;
/// `sw`, `se`, `nw`, `ne` as (column, row) inside the cell; the runtime names and orders them so.
const QUADRANTS: [(&str, &str, u32, u32); 4] = [
    ("sw", "southwest", 0, 0),
    ("se", "southeast", 1, 0),
    ("nw", "northwest", 0, 1),
    ("ne", "northeast", 1, 1),
];

/// Where one axis of the glTF tangent frame sits in a retail normal map texel: the channel and the
/// sign. The frame is east, north, up, with `TANGENT` east and `NORMAL` up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalAxes {
    pub east: (usize, i8),
    pub north: (usize, i8),
    pub up: (usize, i8),
}

/// The retail decode `N = 2 * ((R, B, G) - 0.5)` as a frame: red is east (X), blue is north (Y),
/// green is up (Z), all positive. It matches the reviewed finding (see the module documentation)
/// and the best fit against the retail mesh over Tamriel level 4 blocks (the ignored
/// `retail_normal_map_axes_match_the_mesh_slope` test). Unverified against retail code: the base
/// records use the value unnormalized and Bevy normalizes it.
pub const RETAIL_NORMAL_AXES: NormalAxes = NormalAxes {
    east: (0, 1),
    north: (2, 1),
    up: (1, 1),
};

/// One cut piece of a block: the terrain of one quadrant of one cell, in chunk-local Creation
/// units.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuadrantPatch {
    pub positions: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl QuadrantPatch {
    #[cfg(test)]
    fn area_xy(&self) -> f64 {
        self.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|triangle| {
                let p = |index: u32| self.positions[index as usize];
                let (a, b, c) = (p(triangle[0]), p(triangle[1]), p(triangle[2]));
                0.5 * ((f64::from(b[0]) - f64::from(a[0])) * (f64::from(c[1]) - f64::from(a[1]))
                    - (f64::from(c[0]) - f64::from(a[0])) * (f64::from(b[1]) - f64::from(a[1])))
                .abs()
            })
            .sum()
    }
}

/// Patches of one block by (cell column, cell row, quadrant) from the block's south-west cell.
pub type BlockPatches = BTreeMap<(u32, u32, u8), QuadrantPatch>;

type Point = [f64; 5]; // x, y, z, u, v

/// The point where segment `a`-`b` crosses `axis = bound`. The endpoints are put in a fixed order
/// first, so the two triangles that share an edge compute the same point to the last bit.
fn crossing(a: &Point, b: &Point, axis: usize, bound: f64) -> Point {
    let (p, q) = if a[..3].partial_cmp(&b[..3]) == Some(std::cmp::Ordering::Greater) {
        (b, a)
    } else {
        (a, b)
    };
    let t = (bound - p[axis]) / (q[axis] - p[axis]);
    let mut out: Point = std::array::from_fn(|i| p[i] + t * (q[i] - p[i]));
    out[axis] = bound;
    out
}

/// Keeps the part of the convex `polygon` with `axis >= bound` (or `<=` when `upper`).
fn clip_half(polygon: &[Point], axis: usize, bound: f64, upper: bool) -> Vec<Point> {
    let inside = |p: &Point| {
        if upper {
            p[axis] <= bound
        } else {
            p[axis] >= bound
        }
    };
    let mut out = Vec::with_capacity(polygon.len() + 2);
    for (index, current) in polygon.iter().enumerate() {
        let previous = &polygon[(index + polygon.len() - 1) % polygon.len()];
        match (inside(previous), inside(current)) {
            (true, true) => out.push(*current),
            (true, false) => out.push(crossing(previous, current, axis, bound)),
            (false, true) => {
                out.push(crossing(previous, current, axis, bound));
                out.push(*current);
            }
            (false, false) => {}
        }
    }
    out
}

/// Cuts the retail `land` shape of a block of `level` cells into quadrant patches.
///
/// Positions are the stored positions times the stored scale (which must equal `level`). A
/// quadrant that no retail triangle reaches gets no patch: the retail mesh has no terrain there
/// (the edge of a worldspace), and nothing is invented to fill it. A block with no terrain at all
/// is an error. The second return value counts collapsed triangles discarded during triangulation.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    reason = "geometry is cut in f64 and stored as f32 on purpose; quadrant indices are bounded by the block level; the stored scale is compared exactly"
)]
pub fn cut_land(land: &TerrainLodShape, level: u32) -> Result<(BlockPatches, usize)> {
    if let Some(ancestor) = land.transformed_ancestor {
        bail!("land shape ancestor block {ancestor} has a non-identity transform");
    }
    ensure!(
        land.transformed_ancestor.is_none(),
        "land shape has a non-identity ancestor transform"
    );
    ensure!(
        land.scale == level as f32,
        "land shape scale {} is not the block level {level}",
        land.scale
    );
    ensure!(
        land.translation == [0.0; 3] && land.is_axis_aligned_identity_rotation(),
        "land shape has a transform; only the identity is understood"
    );
    let quadrants_per_side = i64::from(level) * 2;
    let scale = f64::from(land.scale);
    let corners: Vec<Point> = land
        .vertices
        .iter()
        .map(|vertex| {
            let uv = vertex.uv.ok_or_else(|| eyre!("a land vertex has no UV"))?;
            Ok([
                f64::from(vertex.position[0]) * scale,
                f64::from(vertex.position[1]) * scale,
                f64::from(vertex.position[2]) * scale,
                f64::from(uv[0]),
                f64::from(uv[1]),
            ])
        })
        .collect::<Result<_>>()?;

    let mut pieces: BTreeMap<(i64, i64), Vec<Vec<Point>>> = BTreeMap::new();
    for triangle in &land.triangles {
        let points: Vec<Point> = triangle.iter().map(|i| corners[usize::from(*i)]).collect();
        let span = |axis: usize| {
            let low = points.iter().map(|p| p[axis]).fold(f64::INFINITY, f64::min);
            let high = points
                .iter()
                .map(|p| p[axis])
                .fold(f64::NEG_INFINITY, f64::max);
            (
                ((low / QUADRANT_SIZE).floor() as i64).clamp(0, quadrants_per_side - 1),
                ((high / QUADRANT_SIZE).floor() as i64).min(quadrants_per_side - 1),
            )
        };
        let ((x0, x1), (y0, y1)) = (span(0), span(1));
        for qy in y0..=y1 {
            for qx in x0..=x1 {
                let mut polygon = points.clone();
                for (axis, low, high) in [
                    (
                        0,
                        qx as f64 * QUADRANT_SIZE,
                        (qx + 1) as f64 * QUADRANT_SIZE,
                    ),
                    (
                        1,
                        qy as f64 * QUADRANT_SIZE,
                        (qy + 1) as f64 * QUADRANT_SIZE,
                    ),
                ] {
                    polygon = clip_half(&polygon, axis, low, false);
                    if polygon.len() >= 3 {
                        polygon = clip_half(&polygon, axis, high, true);
                    }
                }
                if polygon.len() < 3 {
                    continue;
                }
                pieces.entry((qx, qy)).or_default().push(polygon);
            }
        }
    }

    let mut patches = BlockPatches::new();
    let mut collapsed_triangles = 0;
    for cell_y in 0..level {
        for cell_x in 0..level {
            for (quadrant, (_, _, column, row)) in QUADRANTS.iter().enumerate() {
                let key = (
                    i64::from(cell_x) * 2 + i64::from(*column),
                    i64::from(cell_y) * 2 + i64::from(*row),
                );
                let Some(polygons) = pieces.get(&key) else {
                    continue;
                };
                let mut patch = QuadrantPatch::default();
                let mut seen: HashMap<[u32; 5], u32> = HashMap::new();
                for polygon in polygons {
                    for fan in 1..polygon.len() - 1 {
                        let points = [polygon[0], polygon[fan], polygon[fan + 1]];
                        let positions = points.map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]);
                        let [a, b, c] = positions.map(|p| p.map(f64::from));
                        let ab = std::array::from_fn::<_, 3, _>(|i| b[i] - a[i]);
                        let ac = std::array::from_fn::<_, 3, _>(|i| c[i] - a[i]);
                        // Retail BTR land includes vertical faces (Mudcrab #948).
                        // Test the stored triangle in 3D, including faces on a block boundary.
                        if (0..3).all(|i| {
                            let j = (i + 1) % 3;
                            let k = (i + 2) % 3;
                            ab[j] * ac[k] == ab[k] * ac[j]
                        }) {
                            collapsed_triangles += 1;
                            continue;
                        }
                        for (p, position) in points.into_iter().zip(positions) {
                            let uv = [p[3].clamp(0.0, 1.0) as f32, p[4].clamp(0.0, 1.0) as f32];
                            let bits = [
                                position[0].to_bits(),
                                position[1].to_bits(),
                                position[2].to_bits(),
                                uv[0].to_bits(),
                                uv[1].to_bits(),
                            ];
                            let index = *seen.entry(bits).or_insert_with(|| {
                                patch.positions.push(position);
                                patch.uvs.push(uv);
                                (patch.positions.len() - 1) as u32
                            });
                            patch.indices.push(index);
                        }
                    }
                }
                if !patch.indices.is_empty() {
                    patches.insert((cell_x, cell_y, quadrant as u8), patch);
                }
            }
        }
    }
    ensure!(
        !patches.is_empty(),
        "the block has no terrain after clipping ({collapsed_triangles} collapsed fan triangles)"
    );
    Ok((patches, collapsed_triangles))
}

/// Moves the channels of a retail object-space normal map texel into the glTF tangent frame
/// (east, north, up as red, green, blue) and sets alpha to opaque.
pub fn remap_normal_rgba(rgba: &mut [u8], axes: NormalAxes) {
    let channel = |texel: &[u8], (index, sign): (usize, i8)| -> u8 {
        if sign >= 0 {
            texel[index]
        } else {
            255 - texel[index]
        }
    };
    for texel in rgba.as_chunks_mut::<4>().0 {
        let out = [
            channel(texel, axes.east),
            channel(texel, axes.north),
            channel(texel, axes.up),
            255,
        ];
        texel.copy_from_slice(&out);
    }
}

/// Decodes a retail normal map DDS (first mip), moves its channels and encodes it as KTX2.
pub fn convert_normal_map(dds_bytes: &[u8], axes: NormalAxes) -> Result<Vec<u8>> {
    convert_first_mip(dds_bytes, TextureEncoding::NormalLinear, Some(axes))
        .wrap_err("the normal map cannot be converted")
}

/// Decodes mip 0 of a DDS, optionally moves its normal channels, and encodes it as a KTX2 with
/// that one level: the reviewed finding says both maps are sampled at the first mip only.
fn convert_first_mip(
    dds_bytes: &[u8],
    encoding: TextureEncoding,
    axes: Option<NormalAxes>,
) -> Result<Vec<u8>> {
    let dds = ddsfile::Dds::read(std::io::Cursor::new(dds_bytes)).wrap_err("invalid DDS")?;
    // Packed layouts (the colour maps are 32-bit X8R8G8B8) are read by the texture stage's own
    // decoder; compressed layouts (the normal maps are BC3) by `image_dds`.
    let (width, height, mut rgba) = match crate::texture::decode_packed_rgba8_mips(&dds) {
        Ok(mut levels) if !levels.is_empty() => levels.swap_remove(0),
        _ => {
            let image =
                image_dds::image_from_dds(&dds, 0).wrap_err("the first mip cannot be decoded")?;
            (image.width(), image.height(), image.into_raw())
        }
    };
    if let Some(axes) = axes {
        remap_normal_rgba(&mut rgba, axes);
    }
    TextureConverter::encode_rgba8(width, height, &rgba, encoding, false)
}

/// What one block needs to become a chunk.
pub struct RetailBlock<'a> {
    pub world: u32,
    pub origin: LodOrigin,
    pub level: u32,
    /// South-west cell of the block, from the file name.
    pub cell: (i32, i32),
    pub land: &'a TerrainLodShape,
    /// The retail colour map (`<world>.<level>.<x>.<y>.dds`).
    pub colour_dds: &'a [u8],
    /// The retail normal map (`..._n.dds`).
    pub normal_dds: &'a [u8],
}

/// Counts of one compiled block, for the receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockStats {
    pub cells: usize,
    /// Cells of the block that received no terrain and are not source cells of the chunk.
    pub empty_cells: usize,
    /// Source cells with fewer than four quadrants of terrain.
    pub partial_cells: usize,
    pub vertices: usize,
    pub triangles: usize,
    /// Fan triangles with exactly collinear stored positions, dropped because they have no surface.
    pub collapsed_triangles: usize,
    pub bytes: usize,
}

/// The tier of a retail block level, or `None` when the runtime has no such tier (level 32).
pub fn tier_for_level(level: u32) -> Option<LodTier> {
    // silent-ok: a level that does not fit i32 has no tier, which callers count as no_runtime_tier
    i32::try_from(level).ok().and_then(LodTier::from_side_cells)
}

/// Compiles one block into a chunk.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    reason = "cell offsets are bounded by the tier side (4, 8 or 16); bounds are stored as f32"
)]
pub fn compile_block(block: &RetailBlock<'_>) -> Result<(TerrainChunk, BlockStats)> {
    let tier = tier_for_level(block.level)
        .ok_or_else(|| eyre!("level {} has no runtime tier", block.level))?;
    let side = tier.side_cells();
    let (rel_x, rel_y) = (
        i64::from(block.cell.0) - i64::from(block.origin.grid_x),
        i64::from(block.cell.1) - i64::from(block.origin.grid_y),
    );
    let grid = i64::from(side);
    ensure!(
        rel_x.rem_euclid(grid) == 0 && rel_y.rem_euclid(grid) == 0,
        "block cell {:?} is not on the {side}-cell grid of LOD origin ({}, {})",
        block.cell,
        block.origin.grid_x,
        block.origin.grid_y
    );
    ensure!(
        block.cell.0.checked_add(side - 1).is_some()
            && block.cell.1.checked_add(side - 1).is_some(),
        "block cell {:?} extends past the coordinate range",
        block.cell
    );
    // The runtime computes relative cell bounds in i32, including the far edge.
    let anchor_axis = |axis: i64| -> Result<i32> {
        i32::try_from(axis).wrap_err("block start is outside the relative LOD cell range")?;
        i32::try_from(axis + grid - 1)
            .wrap_err("block end is outside the relative LOD cell range")?;
        Ok(i32::try_from(axis.div_euclid(grid))?)
    };
    let anchor = ChunkAnchor::new(anchor_axis(rel_x)?, anchor_axis(rel_y)?);
    let key = ChunkKey::new(block.world, tier, anchor);
    let (patches, collapsed_triangles) = cut_land(block.land, block.level)?;
    let colour = convert_first_mip(block.colour_dds, TextureEncoding::ColorSrgb, None)
        .wrap_err("the colour map cannot be converted")?;
    let normal = convert_normal_map(block.normal_dds, RETAIL_NORMAL_AXES)?;
    // Only cells that received terrain are source cells; the rest lie outside the worldspace.
    let cells: Vec<(i32, i32)> = (0..side as u32)
        .flat_map(|y| (0..side as u32).map(move |x| (x, y)))
        .filter(|(x, y)| (0..4u8).any(|q| patches.contains_key(&(*x, *y, q))))
        .map(|(x, y)| (block.cell.0 + x as i32, block.cell.1 + y as i32))
        .collect();
    let partial_cells = cells
        .iter()
        .filter(|(x, y)| {
            let (column, row) = ((x - block.cell.0) as u32, (y - block.cell.1) as u32);
            (0..4u8).any(|q| !patches.contains_key(&(column, row, q)))
        })
        .count();
    let glb = emit_glb(key, block.cell, &cells, &patches, &colour, &normal)?;
    let (mut min, mut max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for patch in patches.values() {
        for position in &patch.positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
        }
    }
    // Database bounds are world-space; the vertices stay chunk-local.
    let world_x = block.cell.0 as f64 * CELL_SIZE;
    let world_y = block.cell.1 as f64 * CELL_SIZE;
    let bounds_min = [
        (f64::from(min[0]) + world_x) as f32,
        (f64::from(min[1]) + world_y) as f32,
        min[2],
    ];
    let bounds_max = [
        (f64::from(max[0]) + world_x) as f32,
        (f64::from(max[1]) + world_y) as f32,
        max[2],
    ];
    let stats = BlockStats {
        cells: cells.len(),
        empty_cells: (side * side) as usize - cells.len(),
        partial_cells,
        vertices: patches.values().map(|patch| patch.positions.len()).sum(),
        triangles: patches.values().map(|patch| patch.indices.len() / 3).sum(),
        collapsed_triangles,
        bytes: glb.len(),
    };
    Ok((
        TerrainChunk {
            key,
            cells,
            bounds_min,
            bounds_max,
            glb,
        },
        stats,
    ))
}

/// Writes the chunk GLB. Node 0 is the scene root with the Creation-to-glTF axis rotation (the
/// rotation full-detail terrain uses); then per cell a cell node, a `terrain` group and four
/// quadrant nodes. Buffer views: 0 positions, 1 normals, 2 UVs, 3 tangents, 4 indices, 5 colour
/// KTX2, 6 normal KTX2.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    reason = "cell offsets are bounded by the tier side; a GLB is far below 4 GiB"
)]
fn emit_glb(
    key: ChunkKey,
    south_west: (i32, i32),
    cells: &[(i32, i32)],
    patches: &BlockPatches,
    colour: &[u8],
    normal: &[u8],
) -> Result<Vec<u8>> {
    let side = key.tier.side_cells() as u32;
    ensure!(
        !cells.is_empty() && patches.len() >= cells.len() && patches.len() <= cells.len() * 4,
        "chunk {key:?} has {} patches for {} cells",
        patches.len(),
        cells.len()
    );
    let (mut positions, mut uvs, mut indices): (Vec<f32>, Vec<f32>, Vec<u32>) = Default::default();
    let mut ranges = Vec::new(); // vertex start, vertex count, index start, index count, min, max
    for patch in patches.values() {
        let (vertex_start, index_start) = (positions.len() / 3, indices.len());
        let (mut min, mut max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for position in &patch.positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
            positions.extend(position);
        }
        for uv in &patch.uvs {
            uvs.extend(uv);
        }
        indices.extend(&patch.indices);
        ranges.push((
            vertex_start,
            patch.positions.len(),
            index_start,
            patch.indices.len(),
            min,
            max,
        ));
    }
    let vertex_total = positions.len() / 3;
    // The reference frame of the normal map: up, and east with a positive handedness.
    let normals: Vec<f32> = (0..vertex_total).flat_map(|_| [0.0, 0.0, 1.0]).collect();
    let tangents: Vec<f32> = (0..vertex_total)
        .flat_map(|_| [1.0, 0.0, 0.0, 1.0])
        .collect();

    let mut binary: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut add_view = |binary: &mut Vec<u8>, bytes: &[u8], target: Option<u32>| {
        pad_to_4(binary);
        let offset = binary.len();
        binary.extend_from_slice(bytes);
        let mut view =
            serde_json::json!({ "buffer": 0, "byteOffset": offset, "byteLength": bytes.len() });
        if let Some(target) = target {
            view["target"] = target.into();
        }
        views.push(view);
    };
    add_view(&mut binary, f32_slice_bytes(&positions), Some(34962));
    add_view(&mut binary, f32_slice_bytes(&normals), Some(34962));
    add_view(&mut binary, f32_slice_bytes(&uvs), Some(34962));
    add_view(&mut binary, f32_slice_bytes(&tangents), Some(34962));
    let index_bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
    add_view(&mut binary, &index_bytes, Some(34963));
    add_view(&mut binary, colour, None);
    add_view(&mut binary, normal, None);
    pad_to_4(&mut binary);

    let patch_order: HashMap<(u32, u32, u8), usize> = patches
        .keys()
        .copied()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect();
    let chunk_name = format!("chunk_{}_{}_{}", side, key.anchor.x, key.anchor.y);
    let mut node_list = vec![serde_json::json!({
        "name": chunk_name,
        "matrix": [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
        "children": [],
    })];
    let (mut meshes, mut accessors) = (Vec::new(), Vec::new());
    let mut cell_nodes = Vec::new();
    for (grid_x, grid_y) in cells {
        let (column, row) = (
            (grid_x - south_west.0) as u32,
            (grid_y - south_west.1) as u32,
        );
        ensure!(
            column < side && row < side,
            "cell ({grid_x}, {grid_y}) is outside chunk {key:?}"
        );
        let cell_node = node_list.len();
        cell_nodes.push(cell_node);
        let group = cell_node + 1;
        node_list.push(serde_json::json!({
            "name": nodes::source_cell(*grid_x, *grid_y),
            "extras": { "grid": [grid_x, grid_y] },
            "children": [group],
        }));
        node_list.push(serde_json::json!({ "name": nodes::terrain_group(), "children": [] }));
        let mut quadrant_nodes = Vec::new();
        for (quadrant, (short, full, _, _)) in QUADRANTS.iter().enumerate() {
            // A quadrant the retail mesh does not reach has no node.
            let Some(patch_index) = patch_order.get(&(column, row, quadrant as u8)).copied() else {
                continue;
            };
            let (vertex_start, vertex_count, index_start, index_count, min, max) =
                ranges[patch_index];
            let base = accessors.len();
            accessors.push(serde_json::json!({ "bufferView": 0, "byteOffset": vertex_start * 12, "componentType": 5126, "count": vertex_count, "type": "VEC3", "min": min, "max": max }));
            accessors.push(serde_json::json!({ "bufferView": 1, "byteOffset": vertex_start * 12, "componentType": 5126, "count": vertex_count, "type": "VEC3" }));
            accessors.push(serde_json::json!({ "bufferView": 2, "byteOffset": vertex_start * 8, "componentType": 5126, "count": vertex_count, "type": "VEC2" }));
            accessors.push(serde_json::json!({ "bufferView": 3, "byteOffset": vertex_start * 16, "componentType": 5126, "count": vertex_count, "type": "VEC4" }));
            accessors.push(serde_json::json!({ "bufferView": 4, "byteOffset": index_start * 4, "componentType": 5125, "count": index_count, "type": "SCALAR" }));
            quadrant_nodes.push(node_list.len());
            node_list.push(serde_json::json!({
                "name": format!("terrain_quadrant_{short}"),
                "extras": { "quadrant": full },
                "mesh": meshes.len(),
            }));
            meshes.push(serde_json::json!({
                "name": format!("terrain_{grid_x}_{grid_y}_quadrant_{short}"),
                "primitives": [{
                    "attributes": { "POSITION": base, "NORMAL": base + 1, "TEXCOORD_0": base + 2, "TANGENT": base + 3 },
                    "indices": base + 4,
                    "material": 0,
                }],
            }));
        }
        ensure!(
            !quadrant_nodes.is_empty(),
            "cell ({grid_x}, {grid_y}) has no terrain"
        );
        node_list[group]["children"] = serde_json::json!(quadrant_nodes);
    }
    ensure!(
        meshes.len() == patches.len(),
        "chunk {key:?} left patches without a cell"
    );
    node_list[0]["children"] = serde_json::json!(cell_nodes);
    let json = serde_json::json!({
        "asset": { "version": "2.0", "generator": GENERATOR },
        "extensionsUsed": ["KHR_texture_basisu"],
        "scene": 0,
        "scenes": [{ "name": chunk_name, "nodes": [0] }],
        "nodes": node_list,
        "meshes": meshes,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{ "byteLength": binary.len() }],
        "images": [
            { "bufferView": 5, "mimeType": "image/ktx2" },
            { "bufferView": 6, "mimeType": "image/ktx2" },
        ],
        "samplers": [{ "magFilter": 9729, "minFilter": 9729, "wrapS": 33071, "wrapT": 33071 }],
        "textures": [
            { "source": 0, "sampler": 0, "extensions": { "KHR_texture_basisu": { "source": 0 } } },
            { "source": 1, "sampler": 0, "extensions": { "KHR_texture_basisu": { "source": 1 } } },
        ],
        "materials": [{
            "name": RETAIL_TERRAIN_MATERIAL_NAME,
            "pbrMetallicRoughness": { "baseColorFactor": [1.0, 1.0, 1.0, 1.0], "baseColorTexture": { "index": 0 }, "metallicFactor": 0.0, "roughnessFactor": 0.92 },
            "normalTexture": { "index": 1 },
            "doubleSided": true,
        }],
    });
    let mut json_bytes = serde_json::to_vec(&json)?;
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }
    let total = 12 + 8 + json_bytes.len() + 8 + binary.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json_bytes);
    glb.extend_from_slice(&(binary.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&binary);
    Ok(glb)
}

/// Checks a chunk written by [`compile_block`]: the container, every accessor and view range, every
/// index, the node layout the runtime looks up by name, and the two embedded KTX2 images.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    reason = "the reference frame values are compared exactly; counts index into a validated buffer"
)]
pub(crate) fn validate_glb(json: &serde_json::Value, binary: &[u8]) -> Result<()> {
    let array = |name: &str| {
        json[name]
            .as_array()
            .ok_or_else(|| eyre!("retail terrain GLB has no {name}"))
    };
    let number = |value: &serde_json::Value, what: &str| {
        value
            .as_u64()
            // silent-ok: the closure's caller turns None into an error on the next line
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| eyre!("retail terrain GLB {what} is not a number"))
    };
    ensure!(
        json["extensionsUsed"] == serde_json::json!(["KHR_texture_basisu"]),
        "retail terrain GLB must declare exactly KHR_texture_basisu"
    );
    let views = array("bufferViews")?;
    ensure!(
        views.len() == 7,
        "retail terrain GLB must have 7 buffer views"
    );
    let mut view_data = Vec::new();
    for view in views {
        let offset = number(&view["byteOffset"], "view offset")?;
        let length = number(&view["byteLength"], "view length")?;
        view_data.push(
            offset
                .checked_add(length)
                .and_then(|end| binary.get(offset..end))
                .ok_or_else(|| eyre!("a buffer view lies outside the BIN chunk"))?,
        );
    }
    let accessor_data = |view: usize, offset: usize, count: usize, stride: usize| {
        count
            .checked_mul(stride)
            .and_then(|length| offset.checked_add(length))
            .and_then(|end| view_data[view].get(offset..end))
            .ok_or_else(|| eyre!("accessor lies outside its view"))
    };
    for (view, encoding) in [
        (5usize, TextureEncoding::ColorSrgb),
        (6, TextureEncoding::NormalLinear),
    ] {
        let image = view_data[view];
        ensure!(
            image.starts_with(&[
                0xAB, b'K', b'T', b'X', b' ', b'2', b'0', 0xBB, b'\r', b'\n', 0x1A, b'\n'
            ]),
            "image view {view} is not KTX2"
        );
        let reader = ktx2::Reader::new(image).map_err(|e| eyre!("image view {view}: {e:?}"))?;
        let (width, height) = (reader.header().pixel_width, reader.header().pixel_height);
        ensure!(
            width == height && width.is_power_of_two() && (4..=1024).contains(&width),
            "image view {view} is {width}x{height}, expected a square power of two up to 1024"
        );
        // Retail samples the first mip only, so only the first mip is stored.
        ensure!(
            reader.levels().count() == 1,
            "image view {view} must hold the first mip alone"
        );
        let expected = if encoding == TextureEncoding::ColorSrgb {
            ktx2::TransferFunction::SRGB
        } else {
            ktx2::TransferFunction::Linear
        };
        ensure!(
            reader.transfer_function() == Some(expected),
            "image view {view} has the wrong transfer function"
        );
    }
    let accessors = array("accessors")?;
    let meshes = array("meshes")?;
    ensure!(
        accessors.len() == meshes.len() * 5,
        "retail terrain GLB must have five accessors per mesh"
    );
    for (mesh_index, mesh) in meshes.iter().enumerate() {
        let primitives = mesh["primitives"].as_array().filter(|p| p.len() == 1);
        let primitive = primitives
            .ok_or_else(|| eyre!("mesh {mesh_index} must have one primitive"))?[0]
            .clone();
        ensure!(
            primitive["material"] == 0,
            "mesh {mesh_index} must use the terrain material"
        );
        let attributes = primitive["attributes"]
            .as_object()
            .ok_or_else(|| eyre!("mesh {mesh_index} has no attributes"))?;
        ensure!(
            attributes.len() == 4,
            "mesh {mesh_index} has unexpected attributes"
        );
        let base = mesh_index * 5;
        let mut vertex_count = None;
        for (slot, (semantic, view, components, bytes)) in [
            ("POSITION", 0usize, 3usize, 4usize),
            ("NORMAL", 1, 3, 4),
            ("TEXCOORD_0", 2, 2, 4),
            ("TANGENT", 3, 4, 4),
        ]
        .into_iter()
        .enumerate()
        {
            ensure!(
                attributes.get(semantic).and_then(|v| v.as_u64()) == Some((base + slot) as u64),
                "mesh {mesh_index} {semantic} is not accessor {}",
                base + slot
            );
            let accessor = &accessors[base + slot];
            let (count, offset) = (
                number(&accessor["count"], "count")?,
                number(&accessor["byteOffset"], "offset")?,
            );
            ensure!(
                number(&accessor["bufferView"], "bufferView")? == view
                    && accessor["componentType"] == 5126,
                "mesh {mesh_index} {semantic} accessor has the wrong view or type"
            );
            ensure!(count >= 3, "mesh {mesh_index} has fewer than 3 vertices");
            ensure!(
                *vertex_count.get_or_insert(count) == count,
                "mesh {mesh_index} attribute counts differ"
            );
            let data = accessor_data(view, offset, count, components * bytes)
                .wrap_err_with(|| format!("mesh {mesh_index} {semantic}"))?;
            let values = data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c));
            for (index, value) in values.enumerate() {
                ensure!(
                    value.is_finite(),
                    "mesh {mesh_index} {semantic} has a non-finite value"
                );
                if semantic == "TEXCOORD_0" {
                    ensure!(
                        (0.0..=1.0).contains(&value),
                        "mesh {mesh_index} has a UV outside 0..1"
                    );
                }
                if semantic == "NORMAL" {
                    ensure!(
                        value == [0.0, 0.0, 1.0][index % 3],
                        "mesh {mesh_index} normal is not the up frame"
                    );
                }
                if semantic == "TANGENT" {
                    ensure!(
                        value == [1.0, 0.0, 0.0, 1.0][index % 4],
                        "mesh {mesh_index} tangent is not the east frame"
                    );
                }
            }
        }
        let accessor = &accessors[base + 4];
        let (count, offset) = (
            number(&accessor["count"], "count")?,
            number(&accessor["byteOffset"], "offset")?,
        );
        ensure!(
            accessor["componentType"] == 5125
                && number(&accessor["bufferView"], "bufferView")? == 4,
            "mesh {mesh_index} index accessor has the wrong view or type"
        );
        ensure!(
            count >= 3 && count % 3 == 0,
            "mesh {mesh_index} indices do not form triangles"
        );
        let vertices = vertex_count.unwrap_or(0);
        let data = accessor_data(4, offset, count, 4)
            .wrap_err_with(|| format!("mesh {mesh_index} indices"))?;
        for chunk in data.as_chunks::<4>().0 {
            let index = u32::from_le_bytes(*chunk) as usize;
            ensure!(
                index < vertices,
                "mesh {mesh_index} has an index past its {vertices} vertices"
            );
        }
    }
    // Node layout: the root lists cell nodes; a cell node has one `terrain` group; the group
    // lists one to four quadrant nodes, each with a mesh. Every mesh is used exactly once.
    let scenes = array("scenes")?;
    ensure!(
        json["scene"] == 0
            && scenes
                .first()
                .is_some_and(|scene| scene["nodes"] == serde_json::json!([0])),
        "the default scene must reference only the chunk root"
    );
    let node_list = array("nodes")?;
    let child_list = |node: &serde_json::Value| -> Result<Vec<usize>> {
        node["children"]
            .as_array()
            .ok_or_else(|| eyre!("a node has no children"))?
            .iter()
            .map(|child| {
                number(child, "child").and_then(|index| {
                    if index < node_list.len() {
                        Ok(index)
                    } else {
                        bail!("a child index is out of range")
                    }
                })
            })
            .collect()
    };
    let mut seen_cells = std::collections::BTreeSet::new();
    let mut used_meshes = vec![false; meshes.len()];
    for cell in child_list(node_list.first().ok_or_else(|| eyre!("no nodes"))?)? {
        // silent-ok: a missing node name fails the next parse, which names the node
        let name = node_list[cell]["name"].as_str().unwrap_or_default();
        let grid = nodes::parse_source_cell(name)
            .ok_or_else(|| eyre!("node {cell} is not a source cell"))?;
        ensure!(seen_cells.insert(grid), "cell {grid:?} appears twice");
        let groups = child_list(&node_list[cell])?;
        ensure!(
            groups.len() == 1 && node_list[groups[0]]["name"] == nodes::terrain_group(),
            "cell {grid:?} must have exactly one terrain group"
        );
        let quadrants = child_list(&node_list[groups[0]])?;
        ensure!(
            (1..=4).contains(&quadrants.len()),
            "cell {grid:?} must have one to four quadrants"
        );
        let mut shorts = std::collections::BTreeSet::new();
        for quadrant in quadrants {
            let node = &node_list[quadrant];
            let short = node["name"]
                .as_str()
                .and_then(|n| n.strip_prefix("terrain_quadrant_"))
                // silent-ok: a missing quadrant name fails the next check, which names the cell
                .unwrap_or_default();
            ensure!(
                QUADRANTS.iter().any(|q| q.0 == short) && shorts.insert(short),
                "cell {grid:?} has a bad or repeated quadrant {short:?}"
            );
            let mesh = number(&node["mesh"], "mesh")?;
            ensure!(
                mesh < meshes.len() && !std::mem::replace(&mut used_meshes[mesh], true),
                "mesh {mesh} is out of range or used twice"
            );
        }
    }
    ensure!(
        !seen_cells.is_empty() && used_meshes.iter().all(|used| *used),
        "a mesh is not reachable from a cell"
    );
    let materials = array("materials")?;
    let material = materials
        .first()
        .ok_or_else(|| eyre!("retail terrain GLB has no material"))?;
    let textures = array("textures")?;
    let images = array("images")?;
    for (slot, view) in [
        (&material["pbrMetallicRoughness"]["baseColorTexture"], 5),
        (&material["normalTexture"], 6),
    ] {
        let texture = textures
            .get(number(&slot["index"], "material texture index")?)
            .ok_or_else(|| eyre!("material references a missing texture"))?;
        let image_index = number(
            &texture["extensions"]["KHR_texture_basisu"]["source"],
            "texture image index",
        )?;
        ensure!(
            texture["source"].as_u64() == Some(u64::try_from(image_index)?),
            "core texture source and KHR_texture_basisu must reference the same image"
        );
        let image = images
            .get(image_index)
            .ok_or_else(|| eyre!("texture references a missing image"))?;
        ensure!(
            image["bufferView"] == view && image["mimeType"] == "image/ktx2",
            "material texture must reference KTX2 image view {view}"
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    reason = "test grids use small counts and exact expected values"
)]
mod tests {
    use super::*;
    use crate::lod::retail::btr::{TerrainLodShapeKind, TerrainLodVertex};

    fn vertex(x: f32, y: f32, z: f32, side: f32) -> TerrainLodVertex {
        TerrainLodVertex {
            position: [x, y, z],
            uv: Some([x / side, 1.0 - y / side]),
            normal: None,
            color: None,
        }
    }

    /// A land shape of `grid` x `grid` quads over a block of `level` cells. Local positions run
    /// 0 to 4096; heights follow `z(x, y)`.
    fn land(level: u32, grid: u32, z: impl Fn(f32, f32) -> f32, shift: f32) -> TerrainLodShape {
        let step = 4096.0 / grid as f32;
        let mut vertices = Vec::new();
        for j in 0..=grid {
            for i in 0..=grid {
                // `shift` makes the interior grid lines miss the cell boundaries.
                let edge = |n: u32| if n == 0 || n == grid { 0.0 } else { shift };
                let x = i as f32 * step + edge(i);
                let y = j as f32 * step + edge(j);
                vertices.push(vertex(x, y, z(x, y), 4096.0));
            }
        }
        let mut triangles = Vec::new();
        for j in 0..grid {
            for i in 0..grid {
                let a = (j * (grid + 1) + i) as u16;
                let b = a + 1;
                let c = a + grid as u16 + 1;
                let d = c + 1;
                triangles.push([a, b, d]);
                triangles.push([a, d, c]);
            }
        }
        TerrainLodShape {
            block_index: 1,
            name: Some("land".into()),
            kind: TerrainLodShapeKind::Terrain,
            parent_name: Some("chunk".into()),
            transformed_ancestor: None,
            block_type: "BSTriShape",
            translation: [0.0; 3],
            rotation_columns: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            scale: level as f32,
            vertices,
            triangles,
            segments: Vec::new(),
            shader: None,
        }
    }

    #[test]
    fn cutting_keeps_the_surface_and_stays_inside_each_quadrant() {
        // A level 4 block: 4096 local units times 4 is 16384, so a quadrant is 512 local units.
        // A 5x5 grid with shifted lines puts triangle edges across the quadrant boundaries.
        let shape = land(4, 5, |x, y| 100.0 + 0.5 * x - 0.25 * y, 37.0);
        let (patches, _) = cut_land(&shape, 4).unwrap();
        assert_eq!(patches.len(), 4 * 4 * 4);
        // The whole block is covered exactly once.
        let area: f64 = patches.values().map(QuadrantPatch::area_xy).sum();
        assert!((area - 16384.0 * 16384.0).abs() < 4096.0, "area {area}");
        for ((cell_x, cell_y, quadrant), patch) in &patches {
            let (_, _, column, row) = QUADRANTS[usize::from(*quadrant)];
            let (x0, y0) = (
                (f64::from(*cell_x) * 2.0 + f64::from(column)) * QUADRANT_SIZE,
                (f64::from(*cell_y) * 2.0 + f64::from(row)) * QUADRANT_SIZE,
            );
            assert!((patch.area_xy() - QUADRANT_SIZE * QUADRANT_SIZE).abs() < 1.0);
            for (position, uv) in patch.positions.iter().zip(&patch.uvs) {
                let (x, y) = (f64::from(position[0]), f64::from(position[1]));
                assert!(
                    x >= x0 - 0.01 && x <= x0 + QUADRANT_SIZE + 0.01,
                    "x {x} in {x0}"
                );
                assert!(
                    y >= y0 - 0.01 && y <= y0 + QUADRANT_SIZE + 0.01,
                    "y {y} in {y0}"
                );
                // The height is the retail plane, scaled: z = 4 * (100 + 0.5 x/4 - 0.25 y/4).
                let expected = 4.0 * (100.0 + 0.5 * (x / 4.0) - 0.25 * (y / 4.0));
                assert!(
                    (f64::from(position[2]) - expected).abs() < 0.5,
                    "z {} vs {expected}",
                    position[2]
                );
                // The UV is the retail planar projection.
                assert!((f64::from(uv[0]) - x / 16384.0).abs() < 1.0e-4);
                assert!((f64::from(uv[1]) - (1.0 - y / 16384.0)).abs() < 1.0e-4);
            }
        }
    }

    #[test]
    fn cutting_preserves_vertical_faces_on_quadrant_and_block_boundaries() {
        let mut shape = land(4, 1, |_, _| 0.0, 0.0);
        // Each face spans two quadrants along its horizontal axis.
        for (axis, boundary) in [(0, 512.0), (0, 4096.0), (1, 4096.0)] {
            let start = u16::try_from(shape.vertices.len()).unwrap();
            for (along, z) in [(0.0, 0.0), (1024.0, 0.0), (0.0, -128.0)] {
                let [x, y] = if axis == 0 {
                    [boundary, along]
                } else {
                    [along, boundary]
                };
                shape.vertices.push(vertex(x, y, z, 4096.0));
            }
            shape.triangles.push([start, start + 1, start + 2]);
        }
        let (patches, _) = cut_land(&shape, 4).unwrap();
        for (axis, boundary) in [(0, 2048.0), (0, 16384.0), (1, 16384.0)] {
            let mut area = 0.0;
            let mut quadrants = 0;
            for patch in patches.values() {
                let mut contains_face = false;
                for triangle in patch.indices.as_chunks::<3>().0 {
                    let [a, b, c] = std::array::from_fn(|i| patch.positions[triangle[i] as usize]);
                    if [a, b, c].iter().all(|p| p[axis] == boundary) {
                        let along = 1 - axis;
                        let doubled_area = (b[along] - a[along]) * (c[2] - a[2])
                            - (c[along] - a[along]) * (b[2] - a[2]);
                        assert!(doubled_area < 0.0);
                        area -= doubled_area / 2.0;
                        contains_face = true;
                    }
                }
                quadrants += usize::from(contains_face);
            }
            assert_eq!(quadrants, 2);
            assert_eq!(area, 4096.0 * 512.0 / 2.0);
        }
    }

    #[test]
    fn cutting_keeps_an_interior_vertical_face_and_counts_a_collapsed_triangle() {
        let mut shape = land(4, 1, |_, _| 0.0, 0.0);
        shape.vertices = [
            (128.0, 128.0, 0.0),
            (128.0, 256.0, 0.0),
            (128.0, 128.0, -128.0),
            (128.0, 192.0, 0.0),
        ]
        .map(|(x, y, z)| vertex(x, y, z, 4096.0))
        .to_vec();
        shape.triangles = vec![[0, 1, 2], [0, 3, 1]];
        let (patches, collapsed_triangles) = cut_land(&shape, 4).unwrap();

        assert_eq!(collapsed_triangles, 1);
        assert_eq!(patches.len(), 1);
        let patch = &patches[&(0, 0, 0)];
        assert_eq!(patch.indices, [0, 1, 2]);
        assert_eq!(
            patch.positions,
            [
                [512.0, 512.0, 0.0],
                [512.0, 1024.0, 0.0],
                [512.0, 512.0, -512.0]
            ]
        );
    }

    #[test]
    fn neighbouring_patches_share_their_boundary_points_exactly() {
        let shape = land(4, 5, |x, y| (x * 0.37).sin() * 50.0 + y * 0.1, 41.0);
        let (patches, _) = cut_land(&shape, 4).unwrap();
        // Quadrant sw of cell (0,0) meets se along x = 2048; compare the points on that line.
        let on_line = |key: (u32, u32, u8), side_x: f32| {
            let mut points: Vec<[u32; 3]> = patches[&key]
                .positions
                .iter()
                .filter(|p| p[0] == side_x)
                .map(|p| [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()])
                .collect();
            points.sort();
            points.dedup();
            points
        };
        let (west, east) = (on_line((0, 0, 0), 2048.0), on_line((0, 0, 1), 2048.0));
        assert!(!west.is_empty());
        assert_eq!(
            west, east,
            "a crack: the two sides disagree on the boundary points"
        );
    }

    #[test]
    fn terrain_the_mesh_does_not_reach_is_left_out_not_invented() {
        let mut shape = land(4, 4, |_, _| 0.0, 0.0);
        // Drop every triangle of the first grid row: no terrain in the south of the block.
        shape.triangles.drain(0..8);
        let (patches, _) = cut_land(&shape, 4).unwrap();
        assert_eq!(patches.len(), 4 * 4 * 4 - 4 * 2 * 2);
        assert!(patches.keys().all(|(_, cell_y, _)| *cell_y >= 1));
        // The chunk lists only the cells that have terrain, and validates.
        let dds = bgra_dds([128, 250, 120, 255]);
        let (chunk, stats) = compile_block(&RetailBlock {
            world: 60,
            origin: LodOrigin::new(-96, -96),
            level: 4,
            cell: (-96, -96),
            land: &shape,
            colour_dds: &dds,
            normal_dds: &dds,
        })
        .unwrap();
        assert_eq!(
            (stats.cells, stats.empty_cells, stats.partial_cells),
            (12, 4, 0)
        );
        assert!(chunk.cells.iter().all(|(_, y)| *y >= -95));
        crate::lod::terrain::validate_terrain_glb(&chunk.glb).unwrap();
        // A cell with some quadrants only is a partial cell.
        // An 8x8 grid: each grid square is one quadrant, so dropping two triangles drops one.
        let mut shape = land(4, 8, |_, _| 0.0, 0.0);
        shape.triangles.drain(0..2);
        let (_, stats) = compile_block(&RetailBlock {
            world: 60,
            origin: LodOrigin::new(-96, -96),
            level: 4,
            cell: (-96, -96),
            land: &shape,
            colour_dds: &dds,
            normal_dds: &dds,
        })
        .unwrap();
        assert!(stats.partial_cells > 0, "{stats:?}");
        // A block with no terrain at all is an error.
        shape.triangles.clear();
        assert!(cut_land(&shape, 4).is_err());
    }

    #[test]
    fn a_scale_that_is_not_the_level_or_a_transform_is_refused() {
        let mut shape = land(4, 4, |_, _| 0.0, 0.0);
        shape.scale = 8.0;
        assert!(cut_land(&shape, 4).is_err());
        let mut shape = land(4, 4, |_, _| 0.0, 0.0);
        shape.translation = [1.0, 0.0, 0.0];
        assert!(cut_land(&shape, 4).is_err());
    }

    #[test]
    fn normal_channels_move_into_the_tangent_frame() {
        let axes = NormalAxes {
            east: (0, 1),
            north: (2, -1),
            up: (1, 1),
        };
        // Retail texel (r, g, b) = (10, 200, 30), alpha 7.
        let mut texel = [10u8, 200, 30, 7];
        remap_normal_rgba(&mut texel, axes);
        assert_eq!(texel, [10, 225, 200, 255]);
    }

    #[test]
    fn tiers_exist_for_levels_4_8_16_only() {
        assert_eq!(tier_for_level(4), Some(LodTier::Tier4));
        assert_eq!(tier_for_level(16), Some(LodTier::Tier16));
        assert_eq!(tier_for_level(32), None);
        assert_eq!(tier_for_level(0), None);
    }

    /// A 256x256 uncompressed BGRA DDS with 8 mips (the layout of the retail colour maps).
    fn bgra_dds(fill: [u8; 4]) -> Vec<u8> {
        let mut bytes = b"DDS ".to_vec();
        let mut header = [0u32; 31];
        header[0] = 124; // size
        header[1] = 0x1 | 0x2 | 0x4 | 0x1000 | 0x20000 | 0x8; // caps, height, width, pixel format, mips, pitch
        header[2] = 256;
        header[3] = 256;
        header[4] = 256 * 4;
        header[6] = 9; // mips 256 .. 1
        header[18] = 32; // pixel format size
        header[19] = 0x1 | 0x40; // alpha pixels, rgb
        header[21] = 32;
        header[22] = 0x00ff_0000;
        header[23] = 0x0000_ff00;
        header[24] = 0x0000_00ff;
        header[25] = 0xff00_0000;
        header[26] = 0x1000 | 0x8 | 0x400000; // caps: texture, complex, mipmap
        for value in header {
            bytes.extend(value.to_le_bytes());
        }
        let mut size = 256usize;
        for _ in 0..9 {
            for _ in 0..size * size {
                bytes.extend(fill);
            }
            size = (size / 2).max(1);
        }
        bytes
    }

    #[test]
    fn a_block_compiles_to_a_chunk_that_validates_and_carries_both_maps() {
        let shape = land(4, 6, |x, y| 10.0 + x * 0.01 + y * 0.02, 23.0);
        let colour = bgra_dds([10, 20, 30, 255]);
        let normal = bgra_dds([128, 250, 120, 255]);
        let block = RetailBlock {
            world: 60,
            origin: LodOrigin::new(-96, -96),
            level: 4,
            cell: (-96 + 24, -96 + 24),
            land: &shape,
            colour_dds: &colour,
            normal_dds: &normal,
        };
        let (chunk, stats) = compile_block(&block).unwrap();
        assert_eq!(
            chunk.key,
            ChunkKey::new(60, LodTier::Tier4, ChunkAnchor::new(6, 6))
        );
        assert_eq!(chunk.cells.len(), 16);
        assert_eq!(chunk.cells[0], (-72, -72));
        assert_eq!(stats.cells, 16);
        assert!(stats.triangles >= 2 * 36);
        // World bounds: the block's south-west corner is cell (-72, -72).
        assert!((chunk.bounds_min[0] - (-72.0 * 4096.0)).abs() < 1.0);
        assert!((chunk.bounds_max[0] - (-72.0 * 4096.0 + 16384.0)).abs() < 1.0);
        crate::lod::terrain::validate_terrain_glb(&chunk.glb).unwrap();
    }

    #[test]
    fn a_colour_map_without_an_alpha_channel_converts() {
        // The retail colour maps are X8R8G8B8: 32 bits with no alpha mask.
        let mut dds = bgra_dds([10, 20, 30, 0]);
        dds[4 + 19 * 4..4 + 20 * 4].copy_from_slice(&0x40u32.to_le_bytes()); // rgb only
        dds[4 + 25 * 4..4 + 26 * 4].copy_from_slice(&0u32.to_le_bytes()); // no alpha mask
        let ktx2 = convert_first_mip(&dds, TextureEncoding::ColorSrgb, None).unwrap();
        let reader = ktx2::Reader::new(&ktx2).unwrap();
        assert_eq!(
            (reader.header().pixel_width, reader.levels().count()),
            (256, 1)
        );
    }

    #[test]
    fn a_block_off_the_lod_grid_is_refused() {
        let shape = land(4, 4, |_, _| 0.0, 0.0);
        let dds = bgra_dds([0, 0, 0, 255]);
        let block = RetailBlock {
            world: 60,
            origin: LodOrigin::new(-96, -96),
            level: 4,
            cell: (-95, -96),
            land: &shape,
            colour_dds: &dds,
            normal_dds: &dds,
        };
        assert!(format!("{:#}", compile_block(&block).err().unwrap()).contains("grid"));
    }

    #[test]
    fn grid_offsets_and_block_extents_must_fit_runtime_coordinates() {
        let shape = land(4, 4, |_, _| 0.0, 0.0);
        for (x, origin_x, expected) in [
            (0, i32::MIN, "outside the relative LOD cell range"),
            (i32::MAX, 3, "extends past the coordinate range"),
        ] {
            let block = RetailBlock {
                world: 60,
                origin: LodOrigin::new(origin_x, 0),
                level: 4,
                cell: (x, 0),
                land: &shape,
                colour_dds: &[],
                normal_dds: &[],
            };
            assert!(
                compile_block(&block)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains(expected)
            );
        }
    }

    #[test]
    fn a_retail_chunk_rejects_invalid_glb_references_and_accessor_ranges() {
        let shape = land(4, 4, |_, _| 0.0, 0.0);
        let dds = bgra_dds([128, 250, 120, 255]);
        let (chunk, _) = compile_block(&RetailBlock {
            world: 60,
            origin: LodOrigin::new(0, 0),
            level: 4,
            cell: (0, 0),
            land: &shape,
            colour_dds: &dds,
            normal_dds: &dds,
        })
        .unwrap();
        let glb = gltf::binary::Glb::from_slice(&chunk.glb).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&glb.json).unwrap();
        let mut external_buffer = json["buffers"][0].clone();
        external_buffer["uri"] = serde_json::json!("external.bin");
        for (pointer, value, expected) in [
            ("/asset/version", serde_json::json!("1.0"), "asset version"),
            ("/buffers/0", external_buffer, "one embedded buffer"),
            (
                "/bufferViews/0/buffer",
                serde_json::json!(1),
                "bufferView 0 does not reference buffer 0",
            ),
            (
                "/accessors/0/count",
                serde_json::json!(usize::MAX),
                "accessor lies outside",
            ),
            ("/scenes/0/nodes", serde_json::json!([1]), "default scene"),
            ("/images/1/bufferView", serde_json::json!(5), "image"),
        ] {
            let mut invalid = json.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            let damaged = gltf::binary::Glb {
                json: serde_json::to_vec(&invalid).unwrap().into(),
                bin: glb.bin.clone(),
                header: glb.header,
            }
            .to_vec()
            .unwrap();
            let error = crate::lod::terrain::validate_terrain_glb(&damaged).unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "{pointer}: {error:#}"
            );
        }
    }

    #[test]
    fn a_damaged_chunk_is_refused() {
        let shape = land(4, 4, |_, _| 0.0, 0.0);
        let dds = bgra_dds([128, 250, 120, 255]);
        let block = RetailBlock {
            world: 60,
            origin: LodOrigin::new(-96, -96),
            level: 4,
            cell: (-96, -96),
            land: &shape,
            colour_dds: &dds,
            normal_dds: &dds,
        };
        let (chunk, _) = compile_block(&block).unwrap();
        // Truncated, and a flipped bit in the index data region.
        assert!(
            crate::lod::terrain::validate_terrain_glb(&chunk.glb[..chunk.glb.len() - 4]).is_err()
        );
        let mut damaged = chunk.glb.clone();
        let json_length = u32::from_le_bytes(damaged[12..16].try_into().unwrap()) as usize;
        let binary_start = 20 + json_length + 8;
        // Positions come first; turn the first vertex x into NaN.
        damaged[binary_start..binary_start + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(crate::lod::terrain::validate_terrain_glb(&damaged).is_err());
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    reason = "real-data checks use bounded counts and exact expected values"
)]
mod real_data {
    use super::*;
    use crate::{
        archive::ArchiveExtractor,
        lod::retail::{btr::read_terrain_lod, name::parse_lod_file_name},
    };
    use std::{collections::BTreeSet, path::PathBuf};

    fn data() -> PathBuf {
        PathBuf::from(
            std::env::var_os("MUDCRAB_SKYRIM_DATA")
                .expect("set MUDCRAB_SKYRIM_DATA to run the ignored retail terrain tests"),
        )
    }

    /// Extracts `wanted` (canonical paths) from the base-game archives into `out`.
    fn extract(prefix: &str, out: &std::path::Path, wanted: &BTreeSet<PathBuf>) {
        for entry in std::fs::read_dir(data()).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with(prefix) && name.ends_with(".bsa") {
                ArchiveExtractor::extract_paths(&path, out, wanted).unwrap();
            }
        }
    }

    /// Level, south-west cell, `.btr`, colour DDS and normal DDS bytes of one block.
    type BlockFiles = (u32, (i32, i32), Vec<u8>, Vec<u8>, Vec<u8>);

    fn block_files(out: &std::path::Path, blocks: &[(u32, i32, i32)]) -> Vec<BlockFiles> {
        let mut wanted_meshes = BTreeSet::new();
        let mut wanted_textures = BTreeSet::new();
        for (level, x, y) in blocks {
            let stem = format!("tamriel.{level}.{x}.{y}");
            wanted_meshes.insert(PathBuf::from(format!("meshes/terrain/tamriel/{stem}.btr")));
            wanted_textures.insert(PathBuf::from(format!(
                "textures/terrain/tamriel/{stem}.dds"
            )));
            wanted_textures.insert(PathBuf::from(format!(
                "textures/terrain/tamriel/{stem}_n.dds"
            )));
        }
        extract("Skyrim - Meshes", out, &wanted_meshes);
        extract("Skyrim - Textures", out, &wanted_textures);
        blocks
            .iter()
            .map(|(level, x, y)| {
                let stem = format!("tamriel.{level}.{x}.{y}");
                let read = |kind: &str, ext: &str| {
                    std::fs::read(out.join(format!("{kind}/terrain/tamriel/{stem}{ext}"))).unwrap()
                };
                (
                    *level,
                    (*x, *y),
                    read("meshes", ".btr"),
                    read("textures", ".dds"),
                    read("textures", "_n.dds"),
                )
            })
            .collect()
    }

    /// Which channel layout of the retail normal map agrees with the slope of the retail mesh?
    /// Compares every permutation and sign of the three channels against geometric normals taken
    /// from the retail triangles at 32 x 32 texel positions per block.
    #[test]
    #[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
    fn retail_normal_map_axes_match_the_mesh_slope() {
        let temp = tempfile::tempdir().unwrap();
        let blocks: Vec<(u32, i32, i32)> = (0..6)
            .flat_map(|i| (0..6).map(move |j| (4u32, -96 + 4 * (20 + i), -96 + 4 * (20 + j))))
            .collect();
        let files = block_files(temp.path(), &blocks);
        // Score of each (channel for east, north, up; signs): mean cosine over all samples.
        type Layout = (usize, usize, usize, i8, i8, i8);
        let mut scores: BTreeMap<Layout, (f64, usize)> = BTreeMap::new();
        for (level, _, btr, _, normal_dds) in &files {
            let file = read_terrain_lod(btr, std::path::Path::new("x.btr")).unwrap();
            let land = file
                .shapes_of(crate::lod::retail::btr::TerrainLodShapeKind::Terrain)
                .next()
                .unwrap();
            let dds = ddsfile::Dds::read(std::io::Cursor::new(normal_dds)).unwrap();
            let image = image_dds::image_from_dds(&dds, 0).unwrap();
            let scale = f64::from(*level);
            let world = |i: u16| {
                let p = land.vertices[usize::from(i)].position;
                [
                    f64::from(p[0]) * scale,
                    f64::from(p[1]) * scale,
                    f64::from(p[2]) * scale,
                ]
            };
            for row in (4..256).step_by(8) {
                for column in (4..256).step_by(8) {
                    let (x, y) = (
                        (column as f64 + 0.5) / 256.0 * 4096.0 * scale,
                        (1.0 - (row as f64 + 0.5) / 256.0) * 4096.0 * scale,
                    );
                    let Some(normal) = land.triangles.iter().find_map(|t| {
                        let (a, b, c) = (world(t[0]), world(t[1]), world(t[2]));
                        let side = |p: [f64; 3], q: [f64; 3]| {
                            (q[0] - p[0]) * (y - p[1]) - (q[1] - p[1]) * (x - p[0])
                        };
                        let (s0, s1, s2) = (side(a, b), side(b, c), side(c, a));
                        if !((s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0)
                            || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0))
                        {
                            return None;
                        }
                        let (u, v) = (
                            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
                        );
                        let mut n = [
                            u[1] * v[2] - u[2] * v[1],
                            u[2] * v[0] - u[0] * v[2],
                            u[0] * v[1] - u[1] * v[0],
                        ];
                        if n[2] < 0.0 {
                            n = [-n[0], -n[1], -n[2]];
                        }
                        let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                        Some([n[0] / length, n[1] / length, n[2] / length])
                    }) else {
                        continue;
                    };
                    let texel = image.get_pixel(column, row).0;
                    let decoded: [f64; 3] =
                        std::array::from_fn(|i| f64::from(texel[i]) / 255.0 * 2.0 - 1.0);
                    for permutation in [
                        [0, 1, 2],
                        [0, 2, 1],
                        [1, 0, 2],
                        [1, 2, 0],
                        [2, 0, 1],
                        [2, 1, 0],
                    ] {
                        for signs in 0..8u8 {
                            let sign = |bit: u8| if signs & (1 << bit) != 0 { -1i8 } else { 1 };
                            let mapped = [
                                f64::from(sign(0)) * decoded[permutation[0]],
                                f64::from(sign(1)) * decoded[permutation[1]],
                                f64::from(sign(2)) * decoded[permutation[2]],
                            ];
                            let length = (mapped[0] * mapped[0]
                                + mapped[1] * mapped[1]
                                + mapped[2] * mapped[2])
                                .sqrt();
                            let cosine = (mapped[0] * normal[0]
                                + mapped[1] * normal[1]
                                + mapped[2] * normal[2])
                                / length;
                            let entry = scores
                                .entry((
                                    permutation[0],
                                    permutation[1],
                                    permutation[2],
                                    sign(0),
                                    sign(1),
                                    sign(2),
                                ))
                                .or_default();
                            entry.0 += cosine;
                            entry.1 += 1;
                        }
                    }
                }
            }
        }
        let mut ranked: Vec<_> = scores
            .iter()
            .map(|(k, (sum, n))| (*k, sum / *n as f64))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        for (key, score) in ranked.iter().take(4) {
            println!(
                "east/north/up = channel {} / {} / {} signs {} {} {}: mean cosine {score:.4}",
                key.0, key.1, key.2, key.3, key.4, key.5
            );
        }
        let best = ranked[0].0;
        let expected = RETAIL_NORMAL_AXES;
        assert_eq!(
            (best.0, best.3, best.1, best.4, best.2, best.5),
            (
                expected.east.0,
                expected.east.1,
                expected.north.0,
                expected.north.1,
                expected.up.0,
                expected.up.1
            ),
            "the channel layout that fits the mesh best is not RETAIL_NORMAL_AXES"
        );
        // A clear winner, not a tie.
        assert!(ranked[0].1 - ranked[1].1 > 0.01, "{ranked:?}");
        let _ = parse_lod_file_name;
    }

    #[test]
    #[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
    fn retail_terrain_colour_and_normal_transfer_are_measured_on_real_maps() {
        use crate::texture::tests::decode_uastc_level;
        let temp = tempfile::tempdir().unwrap();
        let blocks = [(4, 0, 0), (8, -32, -32), (16, 32, 32)];
        let srgb_to_linear = |value: f64| {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        let means = |rgba: &[u8]| {
            let texels = rgba.as_chunks::<4>().0;
            std::array::from_fn::<_, 3, _>(|channel| {
                texels.iter().map(|p| f64::from(p[channel])).sum::<f64>() / texels.len() as f64
            })
        };
        let up_cosine = |rgba: &[u8], transfer: fn(f64) -> f64| {
            let texels = rgba.as_chunks::<4>().0;
            texels
                .iter()
                .map(|p| {
                    let [x, y, z] =
                        std::array::from_fn(|c| 2.0 * transfer(f64::from(p[c]) / 255.0) - 1.0);
                    z / (x * x + y * y + z * z).sqrt()
                })
                .sum::<f64>()
                / texels.len() as f64
        };
        for (level, cell, _, colour, normal) in block_files(temp.path(), &blocks) {
            let colour_dds = ddsfile::Dds::read(std::io::Cursor::new(&colour)).unwrap();
            let (_, _, source_colour) = crate::texture::decode_packed_rgba8_mips(&colour_dds)
                .unwrap()
                .swap_remove(0);
            let colour_ktx2 = convert_first_mip(&colour, TextureEncoding::ColorSrgb, None).unwrap();
            assert_eq!(
                ktx2::Reader::new(&colour_ktx2).unwrap().transfer_function(),
                Some(ktx2::TransferFunction::SRGB)
            );
            let converted_colour = decode_uastc_level(&colour_ktx2, 0);
            assert_eq!(source_colour.len(), converted_colour.len());
            let first = means(&source_colour);
            let last = means(&converted_colour);
            let channels = converted_colour
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| &p[..3]);
            let linear = channels
                .clone()
                .map(|&c| srgb_to_linear(f64::from(c) / 255.0))
                .sum::<f64>()
                / channels.count() as f64;
            println!(
                "level {level} block {cell:?}: {} texels, DDS RGB {first:.3?}, KTX2 RGB {last:.3?}, sampler mean raw {:.5} / sRGB {:.5}",
                source_colour.len() / 4,
                last.iter().sum::<f64>() / (3.0 * 255.0),
                linear
            );

            let normal_dds = ddsfile::Dds::read(std::io::Cursor::new(&normal)).unwrap();
            let mut source_normal = image_dds::image_from_dds(&normal_dds, 0)
                .unwrap()
                .into_raw();
            remap_normal_rgba(&mut source_normal, RETAIL_NORMAL_AXES);
            let normal_ktx2 = convert_normal_map(&normal, RETAIL_NORMAL_AXES).unwrap();
            assert_eq!(
                ktx2::Reader::new(&normal_ktx2).unwrap().transfer_function(),
                Some(ktx2::TransferFunction::Linear)
            );
            let converted_normal = decode_uastc_level(&normal_ktx2, 0);
            println!(
                "level {level} block {cell:?}: mean normal up cosine DDS {:.5}, KTX2 {:.5}, incorrect sRGB KTX2 {:.5}",
                up_cosine(&source_normal, |x| x),
                up_cosine(&converted_normal, |x| x),
                up_cosine(&converted_normal, srgb_to_linear)
            );
        }
    }

    /// Real blocks of levels 4, 8 and 16 compile, validate and cover their block.
    #[test]
    #[ignore = "requires MUDCRAB_SKYRIM_DATA with a locally installed Skyrim Special Edition"]
    fn retail_terrain_lod_real_blocks_compile_and_validate() {
        let temp = tempfile::tempdir().unwrap();
        let blocks = [
            (4u32, -72i32, -72i32),
            (4, 0, 0),
            (8, -32, -32),
            (16, 32, 32),
            (16, -96, -96),
        ];
        for (level, cell, btr, colour, normal) in block_files(temp.path(), &blocks) {
            let file = read_terrain_lod(&btr, std::path::Path::new("x.btr")).unwrap();
            let land = file
                .shapes_of(crate::lod::retail::btr::TerrainLodShapeKind::Terrain)
                .next()
                .unwrap();
            let started = std::time::Instant::now();
            let (chunk, stats) = compile_block(&RetailBlock {
                world: 60,
                origin: LodOrigin::new(-96, -96),
                level,
                cell,
                land,
                colour_dds: &colour,
                normal_dds: &normal,
            })
            .unwrap();
            crate::lod::terrain::validate_terrain_glb(&chunk.glb).unwrap();
            println!(
                "level {level} block {cell:?}: retail {} triangles -> {} cells, {} vertices, {} triangles, {} bytes in {:?}",
                land.triangles.len(),
                stats.cells,
                stats.vertices,
                stats.triangles,
                stats.bytes,
                started.elapsed()
            );
            assert_eq!(stats.cells, (level * level) as usize);
            assert!(stats.triangles >= land.triangles.len());
        }
    }
}
