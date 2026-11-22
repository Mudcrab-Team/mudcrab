//! Object LOD chunk compiler: retail `.bto` blocks into one GLB per chunk (D-03, first slice).
//!
//! Each retail block becomes one chunk. Levels 8 and 16 keep whole shapes. Level 4 uses the
//! stored segment index ranges to split shapes into cell-tagged pieces.
//!
//! The chunk is a separate file beside the terrain chunk of the same key
//! (`shared::lod::object_chunk_payload_path`), listed in the additive `lod_object_chunks` table.
//! Terrain chunk files, rows and hashes are untouched by it.
//!
//! Coordinates match the terrain chunks: chunk-local Creation units, the chunk root carries the
//! Creation-to-glTF axis rotation, and the runtime places the root at the chunk's world origin.
//! A retail shape stores block-local vertices plus a transform (translation to the block origin,
//! rotation, and a scale equal to the block level: 16 at level 16, measured on all 48 Tamriel
//! blocks); this applies the stored transform and subtracts the block origin, so a payload is
//! correct wherever its chunk is placed.
//!
//! Material, from the reviewed retail research (skyrim_decomp#675, static reading, not match
//! evidence): the `.bto` carries no alpha property, retail attaches one with alpha test on,
//! blending off and reference 128, and discards pixels strictly below it. The chunk material is
//! therefore alpha mask at 128/255 with no blending. B3 uses runtime grouped-draw flags, which
//! the BTO reader does not have. The importer uses back-face culling. Triangle winding and
//! vertex colors are kept as stored. Retail use of vertex alpha in the cutout is unverified.

use super::{
    retail::bto::{ObjectLodFile, ObjectLodShape},
    terrain::{assemble_glb, f32_slice_bytes, pad_to_4},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::{Connection, params};
use shared::lod::{
    ChunkKey, LodOrigin, LodTier, OBJECT_ATLAS_URI, nodes, object_atlas_path,
    object_chunk_payload_path,
};
use std::path::Path;

/// Explicit producer revision of the object chunk compiler. Terrain chunk reuse is unaffected.
/// Version 4 uses stored segment ranges, requires UVs and uses back-face culling.
pub const OBJECT_COMPILER_VERSION: u32 = 4;

/// Alpha-test reference of the retail object LOD material: 128 of 255, discard strictly below.
pub const ALPHA_CUTOFF: f64 = 128.0 / 255.0;

/// Exterior cells of 4096 Creation units.
const CELL_SIZE: f64 = 4096.0;

/// One compiled object chunk: its key, the cells it covers, its world bounds and the GLB bytes.
pub struct ObjectChunk {
    pub key: ChunkKey,
    /// Every cell of the block, as terrain chunks list theirs.
    pub cells: Vec<(i32, i32)>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub glb: Vec<u8>,
    pub triangles: usize,
    pub vertices: usize,
    /// Large-reference blocks of the source `.bto`, kept undecoded and not used (level 8 and 4).
    pub large_ref_blocks: usize,
    /// Cell-tagged pieces (level-4 segments), which the engine can hide per loaded cell.
    pub cell_pieces: usize,
}

/// The south-west cell of the chunk's block: the cell a retail `.bto` file name carries.
pub fn chunk_origin_cell(key: ChunkKey, origin: LodOrigin) -> (i32, i32) {
    let side = key.tier.side_cells();
    (
        origin.grid_x + key.anchor.x * side,
        origin.grid_y + key.anchor.y * side,
    )
}

/// The chunk key of a retail block at cell `(x, y)` and `level`, or an error when the block does
/// not sit on the worldspace's LOD grid.
pub fn chunk_key_for_block(
    worldspace_id: u32,
    origin: LodOrigin,
    level: u32,
    x: i32,
    y: i32,
) -> Result<ChunkKey> {
    let side = i32::try_from(level)
        .wrap_err_with(|| format!("object LOD level {level} is out of range"))?;
    let tier = LodTier::from_side_cells(side).ok_or_else(|| {
        color_eyre::eyre::eyre!("object LOD level {level} is not a compiled tier")
    })?;
    let anchor = origin.chunk_for_cell(tier, x, y);
    let key = ChunkKey::new(worldspace_id, tier, anchor);
    ensure!(
        chunk_origin_cell(key, origin) == (x, y),
        "object LOD block ({x}, {y}) level {level} is not aligned to the LOD origin ({}, {})",
        origin.grid_x,
        origin.grid_y
    );
    Ok(key)
}

/// Vertex streams of all shapes of a chunk, one run per shape.
#[derive(Default)]
struct Streams {
    positions: Vec<f32>,
    normals: Vec<f32>,
    uvs: Vec<f32>,
    colors: Vec<f32>,
    indices: Vec<u32>,
}

/// World units of a cell coordinate. A block origin is a multiple of 4,096 within a few hundred
/// thousand units, which `f32` holds exactly.
#[allow(
    clippy::cast_possible_truncation,
    reason = "block origins are exact multiples of 4,096 well inside f32's exact integer range"
)]
fn cell_to_units(cell: i32) -> f32 {
    (f64::from(cell) * CELL_SIZE) as f32
}

/// Applies a retail shape transform to a vector: `rotation * (scale * v) + translation`, with the
/// rotation given as three columns.
fn transform(shape: &ObjectLodShape, v: [f32; 3], translate: bool) -> [f32; 3] {
    let scaled = if translate {
        [v[0] * shape.scale, v[1] * shape.scale, v[2] * shape.scale]
    } else {
        v
    };
    let [c0, c1, c2] = shape.rotation_columns;
    let mut out = [0.0f32; 3];
    for axis in 0..3 {
        out[axis] = c0[axis] * scaled[0] + c1[axis] * scaled[1] + c2[axis] * scaled[2];
        if translate {
            out[axis] += shape.translation[axis];
        }
    }
    out
}

/// One drawn piece of a chunk: a triangle range of one shape, optionally tied to a cell.
struct Piece {
    shape: usize,
    vertex_start: usize,
    vertex_count: usize,
    normal_start: Option<usize>,
    color_start: Option<usize>,
    /// Start in the chunk's index stream, and the index count.
    index_start: usize,
    index_count: usize,
    /// The cell this piece covers, for a level-4 segment (see [`segment_cell`]).
    cell: Option<(i32, i32)>,
}

/// Level-4 shapes are split into 16 segments, one per cell of the 4 x 4 block: for a width-4 node
/// the ordinal is `i = 4 * dx + dy`, so segment `i` covers cell `(block_x + i / 4, block_y + i % 4)`.
/// Verified in the retail loader (static reading, independently reviewed:
/// <https://github.com/Mudcrab-Team/skyrim_decomp/pull/816>). Ordinals restart per shape; empty
/// slots keep their index and are skipped. Only a node whose width is the segmented level (4) is
/// split per cell. Shapes of width 8 and 16 are whole shapes. [`segment_cell_fit`] checks the
/// rule against the geometry.
fn segment_cell(block: (i32, i32), segment: usize) -> Option<(i32, i32)> {
    let (column, row) = match (i32::try_from(segment / 4), i32::try_from(segment % 4)) {
        (Ok(column), Ok(row)) => (column, row),
        _ => return None,
    };
    (column < 4)
        .then(|| Some((block.0.checked_add(column)?, block.1.checked_add(row)?)))
        .flatten()
}

/// The nonempty slots and their index-buffer ranges, in slot order (skyrim_decomp#816, G02).
fn segment_ranges(shape: &ObjectLodShape) -> Result<Vec<(usize, std::ops::Range<usize>)>> {
    let mut ranges = Vec::new();
    let limit = shape.triangles.len() * 3;
    for (slot, segment) in shape.segments.iter().enumerate() {
        if segment.num_primitives == 0 {
            continue;
        }
        let start = usize::try_from(segment.start_index)?;
        let count = usize::try_from(segment.num_primitives)?.checked_mul(3);
        let end = count.and_then(|count| start.checked_add(count));
        let end = end
            .filter(|end| start.is_multiple_of(3) && *end <= limit)
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "shape block {} slot {slot} has an invalid index range starting at {start}",
                    shape.block_index
                )
            })?;
        ranges.push((slot, start..end));
    }
    let mut sorted: Vec<_> = ranges.iter().map(|(_, range)| range).collect();
    sorted.sort_unstable_by_key(|range| range.start);
    let mut covered = 0;
    for range in sorted {
        ensure!(
            range.start == covered,
            "shape block {} has overlapping or missing segment indices",
            shape.block_index
        );
        covered = range.end;
    }
    ensure!(
        covered == limit,
        "shape block {} has uncovered indices",
        shape.block_index
    );
    Ok(ranges)
}

/// How many non-empty segments of the multi-segment shapes of a level-4 `block` (whose south-west
/// cell is `block_cell`) have their mean vertex within 0.75 cell of their assigned cell
/// ([`segment_cell`]), and how many there are. A segment whose geometry reaches beyond its cell
/// is a possible misfit (an inference); the count is reported, not corrected.
pub fn segment_cell_fit(block: &ObjectLodFile, block_cell: (i32, i32)) -> (usize, usize) {
    let (mut fitted, mut total) = (0usize, 0usize);
    for shape in block.shapes.iter().filter(|shape| shape.segments.len() > 1) {
        let Ok(ranges) = segment_ranges(shape) else {
            // Compilation reports the invalid ranges and omits the block.
            continue;
        };
        for (index, range) in ranges {
            let triangles = &shape.triangles[range.start / 3..range.end / 3];
            let (mut sum_x, mut sum_y, mut vertices) = (0.0f64, 0.0f64, 0.0f64);
            for triangle in triangles {
                for &vertex in triangle {
                    if let Some(position) = shape.vertices.get(usize::from(vertex)) {
                        sum_x += f64::from(position.position[0]);
                        sum_y += f64::from(position.position[1]);
                        vertices += 1.0;
                    }
                }
            }
            let Some((cell_x, cell_y)) = segment_cell(block_cell, index) else {
                continue;
            };
            let scale = f64::from(shape.scale);
            let world_x = (f64::from(shape.translation[0]) + sum_x / vertices * scale) / CELL_SIZE;
            let world_y = (f64::from(shape.translation[1]) + sum_y / vertices * scale) / CELL_SIZE;
            total += 1;
            fitted += usize::from(
                world_x > f64::from(cell_x) - 0.75
                    && world_x < f64::from(cell_x) + 1.75
                    && world_y > f64::from(cell_y) - 0.75
                    && world_y < f64::from(cell_y) + 1.75,
            );
        }
    }
    (fitted, total)
}

/// Compiles one retail object block (level 16, 8 or 4) into an object chunk. `atlas_ktx2` is the
/// world's object atlas, already encoded as a KTX2 container, embedded as the single base-colour
/// texture.
///
/// At level 4 each nonempty segment becomes a mesh tagged with its cell. Large-reference data (`BSDistantObjectLargeRefExtraData`,
/// level 8 and 4) is kept undecoded and not applied: the engine hides a level-4 segment only for
/// cells it has loaded, and does not implement the large-reference takeover of cells beyond them.
///
/// Fails for a tier that is not 16, 8 or 4, for a block with no geometry, for a shape with several
/// segments outside level 4 (never seen), for a level-4 shape with more than 16 segments, for
/// large-reference data at level 16 (never seen), and for a non-finite vertex: such a file is not
/// what this compiler understands, so it is refused instead of guessed.
pub fn compile_object_chunk(
    key: ChunkKey,
    origin: LodOrigin,
    block: &ObjectLodFile,
    atlas_ktx2: &[u8],
) -> Result<ObjectChunk> {
    ensure!(
        !atlas_ktx2.is_empty(),
        "object chunk {key:?}: the atlas texture is empty"
    );
    compile_with_atlas(key, origin, block, AtlasSource::Embedded(atlas_ktx2))
}

/// Compiles a chunk that references the worldspace's shared atlas file ([`OBJECT_ATLAS_URI`])
/// instead of embedding the texture. The atlas itself is published once per worldspace with
/// [`publish_object_atlas`].
pub fn compile_object_chunk_shared_atlas(
    key: ChunkKey,
    origin: LodOrigin,
    block: &ObjectLodFile,
) -> Result<ObjectChunk> {
    compile_with_atlas(key, origin, block, AtlasSource::Shared)
}

/// Where a chunk's base-colour atlas lives.
#[derive(Clone, Copy)]
enum AtlasSource<'a> {
    /// The KTX2 bytes, embedded in the chunk (format 2 and the compiler tests).
    Embedded(&'a [u8]),
    /// The worldspace's atlas file, by relative URI (format 3).
    Shared,
}

#[derive(Debug)]
pub(crate) struct MissingObjectUv(usize);

impl std::fmt::Display for MissingObjectUv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "shape block {} lacks required TEXCOORD_0 data", self.0)
    }
}

impl std::error::Error for MissingObjectUv {}

fn compile_with_atlas(
    key: ChunkKey,
    origin: LodOrigin,
    block: &ObjectLodFile,
    atlas: AtlasSource<'_>,
) -> Result<ObjectChunk> {
    ensure!(
        !block.shapes.is_empty(),
        "object chunk {key:?}: the retail block has no geometry"
    );
    ensure!(
        key.tier != LodTier::Tier16 || block.large_ref_blocks.is_empty(),
        "object chunk {key:?}: a level-16 block carries large-reference markers, which are not read"
    );
    let (cell_x, cell_y) = chunk_origin_cell(key, origin);
    let block_origin = [cell_to_units(cell_x), cell_to_units(cell_y), 0.0];

    let mut streams = Streams::default();
    let mut pieces: Vec<Piece> = Vec::new();
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut triangles = 0usize;
    for (shape_index, shape) in block.shapes.iter().enumerate() {
        ensure!(
            shape.segments.len() == 1 || key.tier == LodTier::Tier4,
            "object chunk {key:?}: shape at block {} has {} segments, only a level-4 shape may have several",
            shape.block_index,
            shape.segments.len()
        );
        ensure!(
            shape.segments.len() <= 16,
            "object chunk {key:?}: shape at block {} has {} segments, a level-4 block has 16 cells",
            shape.block_index,
            shape.segments.len()
        );
        ensure!(
            !shape.vertices.is_empty() && !shape.triangles.is_empty(),
            "object chunk {key:?}: shape at block {} is empty",
            shape.block_index
        );
        let vertex_start = streams.positions.len() / 3;
        let index_start = streams.indices.len();
        let normal_start = optional_stream_start(
            shape,
            streams.normals.len(),
            |vertex| vertex.normal.is_some(),
            "NORMAL",
        )?;
        let color_start = optional_stream_start(
            shape,
            streams.colors.len(),
            |vertex| vertex.color.is_some(),
            "COLOR_0",
        )?;
        for vertex in &shape.vertices {
            let moved = transform(shape, vertex.position, true);
            let local = [
                moved[0] - block_origin[0],
                moved[1] - block_origin[1],
                moved[2] - block_origin[2],
            ];
            ensure!(
                local.iter().all(|value| value.is_finite()),
                "object chunk {key:?}: a vertex is not finite after the shape transform"
            );
            for axis in 0..3 {
                min[axis] = min[axis].min(local[axis] + block_origin[axis]);
                max[axis] = max[axis].max(local[axis] + block_origin[axis]);
            }
            streams.positions.extend(local);
            let uv = vertex.uv.ok_or(MissingObjectUv(shape.block_index))?;
            streams.uvs.extend(uv);
            if let Some(normal) = vertex.normal {
                streams.normals.extend(transform(shape, normal, false));
            }
            if let Some(color) = vertex.color {
                streams
                    .colors
                    .extend(color.map(|byte| f32::from(byte) / 255.0));
            }
        }
        for triangle in &shape.triangles {
            streams.indices.extend(triangle.map(u32::from));
        }
        triangles += shape.triangles.len();
        let ranges = if key.tier == LodTier::Tier4 {
            segment_ranges(shape)?
        } else {
            vec![(0, 0..shape.triangles.len() * 3)]
        };
        for (slot, range) in ranges {
            let cell = if key.tier == LodTier::Tier4 {
                Some(segment_cell((cell_x, cell_y), slot).ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "object chunk {key:?}: slot {slot} has no cell in its block"
                    )
                })?)
            } else {
                None
            };
            pieces.push(Piece {
                shape: shape_index,
                vertex_start,
                vertex_count: shape.vertices.len(),
                normal_start,
                color_start,
                index_start: index_start + range.start,
                index_count: range.len(),
                cell,
            });
        }
    }

    let cell_pieces = pieces.iter().filter(|piece| piece.cell.is_some()).count();
    let glb = emit_object_glb(key, &streams, &pieces, atlas, block)?;
    let side = key.tier.side_cells();
    let cells = (0..side)
        .flat_map(|dy| (0..side).map(move |dx| (cell_x + dx, cell_y + dy)))
        .collect();
    Ok(ObjectChunk {
        key,
        cells,
        bounds_min: min,
        bounds_max: max,
        glb,
        triangles,
        vertices: streams.positions.len() / 3,
        large_ref_blocks: block.large_ref_blocks.len(),
        cell_pieces,
    })
}

fn optional_stream_start(
    shape: &ObjectLodShape,
    start: usize,
    present: impl Fn(&super::retail::bto::ObjectLodVertex) -> bool,
    name: &str,
) -> Result<Option<usize>> {
    let count = shape
        .vertices
        .iter()
        .filter(|vertex| present(vertex))
        .count();
    ensure!(
        count == 0 || count == shape.vertices.len(),
        "shape block {} has an incomplete {name} stream",
        shape.block_index
    );
    Ok((count > 0).then_some(start))
}

fn emit_object_glb(
    key: ChunkKey,
    streams: &Streams,
    pieces: &[Piece],
    atlas: AtlasSource<'_>,
    block: &ObjectLodFile,
) -> Result<Vec<u8>> {
    let mut binary: Vec<u8> = Vec::new();
    let mut buffer_views = Vec::new();
    let mut add_view = |bytes: &[u8], target: Option<u32>| {
        let view = buffer_views.len();
        let mut row = serde_json::json!({
            "buffer": 0, "byteOffset": binary.len(), "byteLength": bytes.len()
        });
        if let Some(target) = target {
            row["target"] = serde_json::json!(target);
        }
        buffer_views.push(row);
        binary.extend_from_slice(bytes);
        pad_to_4(&mut binary);
        view
    };
    let positions_view = add_view(f32_slice_bytes(&streams.positions), Some(34962));
    let uv_view = add_view(f32_slice_bytes(&streams.uvs), Some(34962));
    let normals_view = (!streams.normals.is_empty())
        .then(|| add_view(f32_slice_bytes(&streams.normals), Some(34962)));
    let colors_view = (!streams.colors.is_empty())
        .then(|| add_view(f32_slice_bytes(&streams.colors), Some(34962)));
    let index_bytes: Vec<_> = streams
        .indices
        .iter()
        .flat_map(|index| index.to_le_bytes())
        .collect();
    let indices_view = add_view(&index_bytes, Some(34963));
    let image_json = match atlas {
        AtlasSource::Embedded(atlas) => serde_json::json!({
            "bufferView": add_view(atlas, None), "mimeType": "image/ktx2"
        }),
        AtlasSource::Shared => serde_json::json!({
            "uri": OBJECT_ATLAS_URI, "mimeType": "image/ktx2"
        }),
    };

    let chunk_name = format!(
        "objects_chunk_{}_{}_{}",
        key.tier.side_cells(),
        key.anchor.x,
        key.anchor.y
    );
    let shape_count = pieces.len();
    // Node 0 is the scene root with the Creation-to-glTF axis rotation (as terrain chunks), node
    // 1 the `objects` group, then one mesh node per retail shape.
    let mut nodes_json = vec![
        serde_json::json!({
            "name": chunk_name,
            "matrix": [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
            "children": [1],
        }),
        serde_json::json!({
            "name": nodes::objects_group(),
            "children": (0..shape_count).map(|i| 2 + i).collect::<Vec<_>>(),
        }),
    ];
    let mut meshes_json = Vec::new();
    let mut accessors_json = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let shape = &block.shapes[piece.shape];
        let vertex_start = piece.vertex_start;
        let vertex_count = piece.vertex_count;
        let positions = &streams.positions[vertex_start * 3..(vertex_start + vertex_count) * 3];
        let (shape_min, shape_max) = bounds(positions);
        nodes_json.push(match piece.cell {
            Some((cell_x, cell_y)) => serde_json::json!({
                "name": format!("object_cell_{cell_x}_{cell_y}_{index}"),
                "extras": { "source_block": shape.block_index, "cell": [cell_x, cell_y] },
                "mesh": index,
            }),
            None => serde_json::json!({
                "name": format!("object_batch_{index}"),
                "extras": { "source_block": shape.block_index },
                "mesh": index,
            }),
        });
        let mut attributes = serde_json::Map::new();
        for (semantic, view, offset, kind) in [
            (
                "POSITION",
                Some(positions_view),
                Some(vertex_start * 12),
                "VEC3",
            ),
            ("TEXCOORD_0", Some(uv_view), Some(vertex_start * 8), "VEC2"),
            (
                "NORMAL",
                normals_view,
                piece.normal_start.map(|start| start * 4),
                "VEC3",
            ),
            (
                "COLOR_0",
                colors_view,
                piece.color_start.map(|start| start * 4),
                "VEC4",
            ),
        ] {
            let (Some(view), Some(offset)) = (view, offset) else {
                continue;
            };
            let mut accessor = serde_json::json!({
                "bufferView": view, "byteOffset": offset, "componentType": 5126,
                "count": vertex_count, "type": kind,
            });
            if semantic == "POSITION" {
                accessor["min"] = serde_json::json!(shape_min);
                accessor["max"] = serde_json::json!(shape_max);
            }
            attributes.insert(semantic.into(), serde_json::json!(accessors_json.len()));
            accessors_json.push(accessor);
        }
        let indices = accessors_json.len();
        accessors_json.push(serde_json::json!({
            "bufferView": indices_view,
            "byteOffset": piece.index_start * 4,
            "componentType": 5125,
            "count": piece.index_count,
            "type": "SCALAR",
        }));
        meshes_json.push(serde_json::json!({
            "name": format!("objects_{}_{}_piece_{index}", key.anchor.x, key.anchor.y),
            "primitives": [{
                "attributes": attributes,
                "indices": indices,
                "material": 0,
            }],
        }));
    }
    let json = serde_json::json!({
        "asset": { "version": "2.0", "generator": "OpenSky LOD object compiler" },
        "extensionsUsed": ["KHR_texture_basisu"],
        "scene": 0,
        "scenes": [{ "name": chunk_name, "nodes": [0] }],
        "nodes": nodes_json,
        "meshes": meshes_json,
        "accessors": accessors_json,
        "bufferViews": buffer_views,
        "buffers": [{ "byteLength": binary.len() }],
        "images": [image_json],
        "samplers": [{ "magFilter": 9729, "minFilter": 9987, "wrapS": 33071, "wrapT": 33071 }],
        "textures": [{ "source": 0, "sampler": 0, "extensions": { "KHR_texture_basisu": { "source": 0 } } }],
        // STAND-IN(#911): back-face culling for unavailable runtime grouped-draw flags.
        // skyrim_decomp#675 B3 uses those flags. BTO stores node flags.
        "materials": [object_material("lod_objects", false)],
    });
    assemble_glb(&json, &binary)
}

/// The retail object LOD material: alpha test at [`ALPHA_CUTOFF`], no blending.
fn object_material(name: &str, double_sided: bool) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "pbrMetallicRoughness": { "baseColorFactor": [1.0, 1.0, 1.0, 1.0], "baseColorTexture": { "index": 0 }, "metallicFactor": 0.0, "roughnessFactor": 1.0 },
        "alphaMode": "MASK",
        "alphaCutoff": ALPHA_CUTOFF,
        "doubleSided": double_sided,
    })
}

fn bounds(positions: &[f32]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in positions.as_chunks::<3>().0 {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex[axis]);
            max[axis] = max[axis].max(vertex[axis]);
        }
    }
    (min, max)
}

/// Creates the additive object LOD tables. They are made only when object chunks are published,
/// so a pack without object LOD has no extra table and its database is unchanged. The columns and
/// the R-tree mirror `lod_chunks` and `lod_chunks_spatial`; the primary key and the payload path
/// differ so object rows never collide with terrain rows.
pub fn create_object_chunk_tables(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS lod_object_chunks (
                 worldspace_id INTEGER NOT NULL, tier INTEGER NOT NULL,
                 anchor_x INTEGER NOT NULL, anchor_y INTEGER NOT NULL,
                 payload_path TEXT NOT NULL, content_hash TEXT NOT NULL,
                 bounds_min_x REAL NOT NULL, bounds_min_y REAL NOT NULL, bounds_min_z REAL NOT NULL,
                 bounds_max_x REAL NOT NULL, bounds_max_y REAL NOT NULL, bounds_max_z REAL NOT NULL,
                 source_cells TEXT NOT NULL DEFAULT '',
                 PRIMARY KEY (worldspace_id, tier, anchor_x, anchor_y)
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS lod_object_chunks_spatial USING rtree(
                 id, minX, maxX, minY, maxY, +worldspace_id, +tier, +anchor_x, +anchor_y
             );
             CREATE TABLE IF NOT EXISTS lod_object_atlases (
                 worldspace_id INTEGER PRIMARY KEY,
                 payload_path TEXT NOT NULL, content_hash TEXT NOT NULL
             );",
        )
        .wrap_err("failed to create the object LOD tables")
}

/// Reads a JSON number as `usize`, naming `what` in the error.
fn json_usize(value: &serde_json::Value, what: &str) -> Result<usize> {
    let number = value
        .as_u64()
        .ok_or_else(|| color_eyre::eyre::eyre!("{what} is missing or not a number"))?;
    usize::try_from(number).wrap_err_with(|| format!("{what} does not fit in memory"))
}

/// A JSON number as an index, or `None` when it is not a number or does not fit.
fn json_index(value: &serde_json::Value) -> Option<usize> {
    let number = value.as_u64()?;
    // silent-ok: a value that does not fit finds no element, and every caller reports that.
    usize::try_from(number).ok()
}

/// An optional JSON byte offset: absent is zero, present must be a number that fits.
fn json_offset(value: &serde_json::Value) -> Result<usize> {
    if value.is_null() {
        return Ok(0);
    }
    json_usize(value, "byteOffset")
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let word = offset
        .checked_add(4)
        .and_then(|end| bytes.get(offset..end))
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB is truncated"))?;
    Ok(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

/// Checks a written object chunk GLB end to end, so a damaged file never passes for a chunk:
/// the GLB container lengths, every buffer view and accessor range, the vertex formats, finite
/// vertex data, every triangle index, the single material and the embedded KTX2 atlas (read back
/// with the texture stage's own inspector). The `objects` group must list exactly the meshes.
pub fn validate_object_glb(bytes: &[u8]) -> Result<()> {
    let document = crate::mesh::glb_json_from_bytes(bytes)?;
    let json_length = le_u32(bytes, 12)? as usize;
    ensure!(
        json_length.is_multiple_of(4),
        "the GLB JSON chunk is misaligned"
    );
    let json_end = 20usize
        .checked_add(json_length)
        .ok_or_else(|| color_eyre::eyre::eyre!("GLB JSON range overflow"))?;
    let binary_length = le_u32(bytes, json_end)? as usize;
    ensure!(
        bytes.get(json_end + 4..json_end + 8) == Some(b"BIN\0".as_slice()),
        "the second GLB chunk is not BIN"
    );
    let binary_start = json_end + 8;
    ensure!(
        binary_start.checked_add(binary_length) == Some(bytes.len()),
        "the GLB binary chunk length is inconsistent"
    );
    let binary = &bytes[binary_start..];

    let buffers = document["buffers"]
        .as_array()
        .filter(|buffers| buffers.len() == 1)
        .ok_or_else(|| color_eyre::eyre::eyre!("an object chunk uses one embedded buffer"))?;
    let buffer_length = json_usize(&buffers[0]["byteLength"], "buffer byteLength")?;
    ensure!(
        buffer_length > 0 && buffer_length <= binary.len(),
        "the buffer length is zero or exceeds the BIN chunk"
    );
    let mut views = Vec::new();
    for (index, view) in document["bufferViews"]
        .as_array()
        .ok_or_else(|| color_eyre::eyre::eyre!("no bufferViews"))?
        .iter()
        .enumerate()
    {
        let offset = json_offset(&view["byteOffset"])?;
        let length = json_usize(&view["byteLength"], "bufferView byteLength")?;
        ensure!(
            json_usize(&view["buffer"], "bufferView buffer")? == 0
                && offset
                    .checked_add(length)
                    .is_some_and(|end| end <= buffer_length),
            "bufferView {index} leaves the buffer"
        );
        views.push((offset, length));
    }
    let accessors = document["accessors"]
        .as_array()
        .ok_or_else(|| color_eyre::eyre::eyre!("no accessors"))?;
    // Per accessor: (start in the BIN chunk, count, component type, components per element).
    let mut spans = Vec::with_capacity(accessors.len());
    for (index, accessor) in accessors.iter().enumerate() {
        let (view_offset, view_length) = *views
            .get(json_usize(&accessor["bufferView"], "accessor bufferView")?)
            .ok_or_else(|| color_eyre::eyre::eyre!("accessor {index} has no bufferView"))?;
        let component_type = json_usize(&accessor["componentType"], "accessor componentType")?;
        ensure!(
            matches!(component_type, 5125 | 5126),
            "accessor {index} has an unsupported component type"
        );
        let components = match accessor["type"].as_str() {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") => 4,
            _ => color_eyre::eyre::bail!("accessor {index} has an invalid type"),
        };
        let count = json_usize(&accessor["count"], "accessor count")?;
        ensure!(count > 0, "accessor {index} is empty");
        let offset = json_offset(&accessor["byteOffset"])?;
        ensure!(
            offset.is_multiple_of(4)
                && count
                    .checked_mul(components * 4)
                    .and_then(|length| offset.checked_add(length))
                    .is_some_and(|end| end <= view_length),
            "accessor {index} is misaligned or exceeds its bufferView"
        );
        spans.push((view_offset + offset, count, component_type, components));
    }
    let read_f32 = |start: usize, index: usize| -> f32 {
        let at = start + index * 4;
        f32::from_le_bytes([binary[at], binary[at + 1], binary[at + 2], binary[at + 3]])
    };

    let nodes_json = document["nodes"]
        .as_array()
        .ok_or_else(|| color_eyre::eyre::eyre!("no nodes"))?;
    let group = nodes_json
        .iter()
        .find(|node| node["name"] == nodes::objects_group())
        .ok_or_else(|| color_eyre::eyre::eyre!("object chunk has no `objects` group"))?;
    let children = group["children"]
        .as_array()
        .filter(|children| !children.is_empty())
        .ok_or_else(|| color_eyre::eyre::eyre!("the `objects` group is empty"))?;
    let meshes = document["meshes"]
        .as_array()
        .ok_or_else(|| color_eyre::eyre::eyre!("no meshes"))?;
    ensure!(
        meshes.len() == children.len(),
        "the `objects` group lists {} batches for {} meshes",
        children.len(),
        meshes.len()
    );
    for child in children {
        let node = json_index(child)
            .and_then(|index| nodes_json.get(index))
            .ok_or_else(|| color_eyre::eyre::eyre!("a group child is not a node"))?;
        ensure!(
            node["name"].as_str().is_some_and(
                |name| name.starts_with("object_batch_") || name.starts_with("object_cell_")
            ),
            "a node under `objects` is not an object batch or cell piece: {node}"
        );
        let mesh = json_index(&node["mesh"])
            .and_then(|index| meshes.get(index))
            .ok_or_else(|| color_eyre::eyre::eyre!("an object batch has no mesh"))?;
        let primitives = mesh["primitives"]
            .as_array()
            .filter(|primitives| primitives.len() == 1)
            .ok_or_else(|| color_eyre::eyre::eyre!("an object batch has one primitive"))?;
        let primitive = &primitives[0];
        ensure!(
            primitive["mode"].as_u64().unwrap_or(4) == 4
                && json_index(&primitive["material"])
                    .is_some_and(|material| document["materials"].get(material).is_some()),
            "an object batch is not a triangle list on an object material"
        );
        let attributes = primitive["attributes"]
            .as_object()
            .filter(|attributes| {
                attributes.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "POSITION" | "TEXCOORD_0" | "NORMAL" | "COLOR_0"
                    )
                })
            })
            .ok_or_else(|| color_eyre::eyre::eyre!("an object batch has unexpected attributes"))?;
        let mut vertex_count = None;
        for (semantic, components, required) in [
            ("POSITION", 3, true),
            ("TEXCOORD_0", 2, true),
            ("NORMAL", 3, false),
            ("COLOR_0", 4, false),
        ] {
            if !required && !attributes.contains_key(semantic) {
                continue;
            }
            let (start, count, component_type, found) = attributes
                .get(semantic)
                .and_then(json_index)
                .and_then(|index| spans.get(index))
                .copied()
                .ok_or_else(|| color_eyre::eyre::eyre!("an object batch lacks {semantic}"))?;
            ensure!(
                component_type == 5126 && found == components,
                "an object batch has a wrong {semantic} format"
            );
            ensure!(
                *vertex_count.get_or_insert(count) == count,
                "an object batch has streams of different lengths"
            );
            for value in 0..count * components {
                ensure!(
                    read_f32(start, value).is_finite(),
                    "an object batch has a non-finite {semantic} value"
                );
            }
        }
        let (start, count, component_type, components) = json_index(&primitive["indices"])
            .and_then(|index| spans.get(index))
            .copied()
            .ok_or_else(|| color_eyre::eyre::eyre!("an object batch has no indices"))?;
        ensure!(
            component_type == 5125 && components == 1 && count.is_multiple_of(3),
            "an object batch has indices that are not whole u32 triangles"
        );
        let vertices = vertex_count.unwrap_or(0);
        for index in 0..count {
            let at = start + index * 4;
            let value =
                u32::from_le_bytes([binary[at], binary[at + 1], binary[at + 2], binary[at + 3]])
                    as usize;
            ensure!(
                value < vertices,
                "an object batch index {value} exceeds its {vertices} vertices"
            );
        }
    }

    let materials = document["materials"].as_array();
    ensure!(
        materials.is_some_and(|materials| (1..=2).contains(&materials.len())
            && [("lod_objects", false), ("lod_objects_double_sided", true)]
                .iter()
                .zip(materials)
                .all(|((name, double_sided), material)| {
                    material["name"] == *name
                        && material["alphaMode"] == "MASK"
                        && material["alphaCutoff"].as_f64() == Some(ALPHA_CUTOFF)
                        && material["doubleSided"] == *double_sided
                        && material["pbrMetallicRoughness"]["baseColorTexture"]["index"] == 0
                })),
        "an object chunk has the alpha-mask `lod_objects` materials with the atlas as base colour"
    );
    let images = document["images"]
        .as_array()
        .filter(|images| images.len() == 1)
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("an object chunk carries exactly one atlas image")
        })?;
    ensure!(
        images[0]["mimeType"] == "image/ktx2",
        "the atlas image is not KTX2"
    );
    if let Some(uri) = images[0].get("uri") {
        // Format 3: the shared atlas file. Its own checks are the package's (the file, its row and
        // its hash), because a chunk cannot see it.
        ensure!(
            uri == OBJECT_ATLAS_URI && images[0].get("bufferView").is_none(),
            "an object chunk references the atlas by `{OBJECT_ATLAS_URI}` only, found {uri}"
        );
        return Ok(());
    }
    let (atlas_offset, atlas_length) = *views
        .get(json_usize(&images[0]["bufferView"], "image bufferView")?)
        .ok_or_else(|| color_eyre::eyre::eyre!("the atlas image has no bufferView"))?;
    let atlas = &binary[atlas_offset..atlas_offset + atlas_length];
    let metadata = crate::texture::inspect_ktx2(atlas, crate::texture::TextureEncoding::ColorSrgb)
        .wrap_err("the embedded atlas is not a valid sRGB KTX2")?;
    ensure!(
        metadata.layers <= 1 && metadata.faces == 1 && metadata.depth <= 1,
        "the embedded atlas is not a 2D texture"
    );
    Ok(())
}

/// Whether a chunk GLB references the shared atlas file (format 3) instead of embedding the atlas
/// (format 2).
pub fn chunk_uses_shared_atlas(bytes: &[u8]) -> Result<bool> {
    let document = crate::mesh::glb_json_from_bytes(bytes)?;
    Ok(document["images"][0].get("uri").is_some())
}

/// Checks a KTX2 atlas file: a valid sRGB 2D texture.
pub fn validate_atlas_ktx2(bytes: &[u8]) -> Result<()> {
    let metadata = crate::texture::inspect_ktx2(bytes, crate::texture::TextureEncoding::ColorSrgb)
        .wrap_err("the object atlas is not a valid sRGB KTX2")?;
    ensure!(
        metadata.layers <= 1 && metadata.faces == 1 && metadata.depth <= 1,
        "the object atlas is not a 2D texture"
    );
    Ok(())
}

/// Writes the worldspace's shared atlas, validates what was written and records its row in
/// `lod_object_atlases`. Returns the payload path and the SHA-256 of the file.
pub fn publish_object_atlas(
    connection: &Connection,
    staging_root: &Path,
    worldspace_id: u32,
    ktx2: &[u8],
) -> Result<(String, String)> {
    validate_atlas_ktx2(ktx2)?;
    let relative = object_atlas_path(worldspace_id);
    let path = staging_root.join(&relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, ktx2)?;
    let written = std::fs::read(&path)?;
    validate_atlas_ktx2(&written).wrap_err_with(|| format!("invalid object atlas {relative}"))?;
    let hash = crate::cache::hash_bytes(&written);
    ensure!(
        hash == crate::cache::hash_bytes(ktx2),
        "object atlas changed while being written: {relative}"
    );
    create_object_chunk_tables(connection)?;
    connection.execute(
        "INSERT OR REPLACE INTO lod_object_atlases(worldspace_id, payload_path, content_hash) VALUES (?1, ?2, ?3)",
        params![worldspace_id, relative, hash],
    )?;
    Ok((relative, hash))
}

/// Identity of the inputs of one object chunk, recorded as `object_chunk_inputs` in
/// `lod-manifest.json`: the compiler revision, the chunk key and LOD origin, the retail `.bto`
/// bytes and the retail atlas bytes. Equal inputs and compiler give the same payload.
pub fn object_chunk_input_hash(
    key: ChunkKey,
    origin: LodOrigin,
    block_sha256: &str,
    atlas_sha256: &str,
) -> Result<String> {
    Ok(crate::cache::hash_bytes(&serde_json::to_vec(
        &serde_json::json!({
            "compiler": OBJECT_COMPILER_VERSION,
            "worldspace": key.worldspace_id,
            "tier": key.tier.side_cells(),
            "anchor": [key.anchor.x, key.anchor.y],
            "origin": [origin.grid_x, origin.grid_y],
            "block": block_sha256,
            "atlas": atlas_sha256,
        }),
    )?))
}

/// Checks the object LOD of a pack against what `lod-manifest.json` lists, and fails when a
/// listed object chunk is missing, damaged or unlisted.
///
/// A pack without object LOD (no object rows, no `object_chunks` in the manifest) passes without
/// reading anything else. Otherwise the manifest count, the row count, the spatial index count and
/// the input proofs must agree, and every row must name the canonical payload path of its key, a
/// file that exists, matches its recorded SHA-256 and passes [`validate_object_glb`].
pub(crate) fn validate_object_lod_package(root: &Path) -> Result<()> {
    let database_path = root.join("skyrim_world.db");
    let manifest_path = root.join("lod-manifest.json");
    let manifest: Option<serde_json::Value> = if manifest_path.is_file() {
        Some(
            serde_json::from_slice(&std::fs::read(&manifest_path)?)
                .wrap_err("lod-manifest.json is not valid JSON")?,
        )
    } else {
        None
    };
    if !database_path.is_file() {
        return validate_no_object_table(manifest.as_ref(), "skyrim_world.db");
    }
    let connection =
        Connection::open_with_flags(&database_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .wrap_err("failed to open the world database to check object LOD")?;
    validate_object_generation(&connection, manifest.as_ref(), &|path| root.join(path))
}

fn validate_no_object_table(manifest: Option<&serde_json::Value>, missing: &str) -> Result<()> {
    ensure!(
        manifest.is_none_or(|manifest| {
            manifest["object_chunks"].as_u64().unwrap_or(0) == 0
                && manifest["object_chunk_inputs"]
                    .as_object()
                    .is_none_or(serde_json::Map::is_empty)
                && manifest["object_atlases"]
                    .as_object()
                    .is_none_or(serde_json::Map::is_empty)
        }),
        "lod-manifest.json lists object LOD but the pack has no {missing}"
    );
    Ok(())
}

/// Validates unpublished rows and payloads before a derive can replace the active files.
pub(crate) fn validate_object_generation(
    connection: &Connection,
    manifest: Option<&serde_json::Value>,
    payload: &impl Fn(&str) -> std::path::PathBuf,
) -> Result<()> {
    let listed = manifest
        .and_then(|manifest| manifest["object_chunks"].as_u64())
        .unwrap_or(0);
    let inputs = manifest
        .and_then(|manifest| manifest["object_chunk_inputs"].as_object())
        .map_or_else(serde_json::Map::new, Clone::clone);
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lod_object_chunks')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return validate_no_object_table(manifest, "lod_object_chunks table");
    }
    let rows: u64 = connection.query_row("SELECT count(*) FROM lod_object_chunks", [], |row| {
        row.get(0)
    })?;
    if rows == 0 && listed == 0 && inputs.is_empty() {
        return Ok(());
    }
    ensure!(
        manifest.is_some(),
        "the database lists {rows} object chunks but lod-manifest.json is missing"
    );
    ensure!(
        listed == rows && inputs.len() as u64 == rows,
        "object LOD disagrees: lod-manifest.json lists {listed} chunks and {} input proofs, the database has {rows} rows",
        inputs.len()
    );
    ensure!(
        manifest
            .and_then(|manifest| manifest["objects_compiler_version"].as_u64())
            .is_some_and(|version| version > 0),
        "lod-manifest.json has no objects_compiler_version for its object chunks"
    );
    let spatial: u64 = connection
        .query_row(
            "SELECT count(*) FROM lod_object_chunks_spatial",
            [],
            |row| row.get(0),
        )
        .wrap_err("the database has object chunks but no spatial index for them")?;
    ensure!(
        spatial == rows,
        "the object chunk spatial index has {spatial} entries for {rows} chunks"
    );
    let mut statement = connection.prepare(
        "SELECT worldspace_id, tier, anchor_x, anchor_y, payload_path, content_hash          FROM lod_object_chunks ORDER BY worldspace_id, tier, anchor_x, anchor_y",
    )?;
    let listed_rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut shared_atlas_worlds = std::collections::BTreeSet::new();
    for (worldspace, tier, anchor_x, anchor_y, path, hash) in listed_rows {
        let tier = LodTier::from_side_cells(tier)
            .ok_or_else(|| color_eyre::eyre::eyre!("object chunk row {path} has tier {tier}"))?;
        let key = ChunkKey::new(
            worldspace,
            tier,
            shared::lod::ChunkAnchor::new(anchor_x, anchor_y),
        );
        ensure!(
            path == object_chunk_payload_path(key),
            "object chunk row {path} is not the canonical path of its key"
        );
        ensure!(
            inputs.contains_key(&path),
            "object chunk {path} has no input proof in lod-manifest.json"
        );
        let bytes = std::fs::read(payload(&path))
            .wrap_err_with(|| format!("listed object chunk {path} is missing or unreadable"))?;
        ensure!(
            crate::cache::hash_bytes(&bytes) == hash,
            "listed object chunk {path} does not match its recorded SHA-256"
        );
        validate_object_glb(&bytes)
            .wrap_err_with(|| format!("listed object chunk {path} is damaged"))?;
        if chunk_uses_shared_atlas(&bytes)? {
            shared_atlas_worlds.insert(worldspace);
        }
    }
    validate_object_atlases(payload, connection, manifest, &shared_atlas_worlds)
}

/// Checks the shared atlas files: every worldspace whose chunks reference the shared atlas has a
/// row, a file with the recorded SHA-256 that is a valid sRGB KTX2, and a manifest entry; and no
/// atlas row or manifest entry stands without chunks that use it.
fn validate_object_atlases(
    payload: &impl Fn(&str) -> std::path::PathBuf,
    connection: &Connection,
    manifest: Option<&serde_json::Value>,
    needed: &std::collections::BTreeSet<u32>,
) -> Result<()> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lod_object_atlases')",
        [],
        |row| row.get(0),
    )?;
    let rows: Vec<(u32, String, String)> = if has_table {
        connection
            .prepare("SELECT worldspace_id, payload_path, content_hash FROM lod_object_atlases ORDER BY worldspace_id")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?
    } else {
        Vec::new()
    };
    let listed = manifest
        .and_then(|manifest| manifest["object_atlases"].as_object())
        .map_or_else(serde_json::Map::new, Clone::clone);
    for world in needed {
        ensure!(
            rows.iter().any(|(id, _, _)| id == world),
            "object chunks of worldspace {world:08x} reference the shared atlas but the database has no atlas row for it"
        );
    }
    ensure!(
        rows.len() == listed.len(),
        "object LOD disagrees: lod-manifest.json lists {} atlases, the database has {}",
        listed.len(),
        rows.len()
    );
    for (world, path, hash) in rows {
        ensure!(
            needed.contains(&world),
            "the shared atlas of worldspace {world:08x} has no chunk that uses it"
        );
        ensure!(
            path == object_atlas_path(world),
            "atlas row {path} is not the canonical path of worldspace {world:08x}"
        );
        ensure!(
            listed.get(&path).and_then(serde_json::Value::as_str) == Some(hash.as_str()),
            "atlas {path} is not listed in lod-manifest.json with its SHA-256"
        );
        let bytes = std::fs::read(payload(&path))
            .wrap_err_with(|| format!("listed object atlas {path} is missing or unreadable"))?;
        ensure!(
            crate::cache::hash_bytes(&bytes) == hash,
            "listed object atlas {path} does not match its recorded SHA-256"
        );
        validate_atlas_ktx2(&bytes)
            .wrap_err_with(|| format!("listed object atlas {path} is damaged"))?;
    }
    Ok(())
}

/// Publishes one compiled object chunk: writes the payload, validates what was written, and
/// inserts the row and its R-tree entry within the caller's transaction.
pub fn publish_object_chunk(
    connection: &Connection,
    staging_root: &Path,
    chunk: &ObjectChunk,
) -> Result<()> {
    let relative = object_chunk_payload_path(chunk.key);
    let path = staging_root.join(&relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &chunk.glb)?;
    let written = std::fs::read(&path)?;
    validate_object_glb(&written)
        .wrap_err_with(|| format!("invalid object chunk GLB {relative}"))?;
    let content_hash = crate::cache::hash_bytes(&written);
    ensure!(
        content_hash == crate::cache::hash_bytes(&chunk.glb),
        "object chunk payload changed while being written: {relative}"
    );
    let source_cells = chunk
        .cells
        .iter()
        .map(|(x, y)| format!("{x},{y}"))
        .collect::<Vec<_>>()
        .join(";");
    create_object_chunk_tables(connection)?;
    connection.execute(
        "INSERT OR REPLACE INTO lod_object_chunks(worldspace_id, tier, anchor_x, anchor_y, payload_path, content_hash, \
         bounds_min_x, bounds_min_y, bounds_min_z, bounds_max_x, bounds_max_y, bounds_max_z, source_cells) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            chunk.key.worldspace_id,
            chunk.key.tier.side_cells(),
            chunk.key.anchor.x,
            chunk.key.anchor.y,
            relative,
            content_hash,
            chunk.bounds_min[0],
            chunk.bounds_min[1],
            chunk.bounds_min[2],
            chunk.bounds_max[0],
            chunk.bounds_max[1],
            chunk.bounds_max[2],
            source_cells,
        ],
    )?;
    connection.execute(
        "DELETE FROM lod_object_chunks_spatial WHERE worldspace_id = ?1 AND tier = ?2 AND anchor_x = ?3 AND anchor_y = ?4",
        params![
            chunk.key.worldspace_id,
            chunk.key.tier.side_cells(),
            chunk.key.anchor.x,
            chunk.key.anchor.y,
        ],
    )?;
    connection
        .execute(
            "INSERT OR REPLACE INTO lod_object_chunks_spatial(id, minX, maxX, minY, maxY, worldspace_id, tier, anchor_x, anchor_y) \
             VALUES ((SELECT COALESCE(MAX(id), 0) + 1 FROM lod_object_chunks_spatial), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                chunk.bounds_min[0],
                chunk.bounds_max[0],
                chunk.bounds_min[1],
                chunk.bounds_max[1],
                chunk.key.worldspace_id,
                chunk.key.tier.side_cells(),
                chunk.key.anchor.x,
                chunk.key.anchor.y,
            ],
        )
        .wrap_err("failed to index object chunk bounds")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lod::retail::bto::{ObjectLodSegment, ObjectLodVertex, OpaqueBlock};
    use shared::lod::ChunkAnchor;
    use std::collections::BTreeMap;

    /// A valid sRGB KTX2 atlas, encoded once by the texture stage from a tiny generated DDS.
    fn atlas() -> Vec<u8> {
        static ATLAS: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        ATLAS
            .get_or_init(|| {
                let dds = dummy_content::dds::generate(
                    &dummy_content::dds::Spec::new(dummy_content::dds::Format::X8R8G8B8, 8, 8),
                    &mut dummy_content::rng::Rng::new(0),
                )
                .unwrap();
                crate::texture::TextureConverter::convert(
                    &dds,
                    crate::texture::TextureEncoding::ColorSrgb,
                )
                .unwrap()
            })
            .clone()
    }

    const ORIGIN: LodOrigin = LodOrigin::new(-96, -96);
    /// Anchor (6, 5) from origin -96 is the block whose south-west cell is (0, -16).
    fn key() -> ChunkKey {
        ChunkKey::new(60, LodTier::Tier16, ChunkAnchor::new(6, 5))
    }

    fn vertex(position: [f32; 3]) -> ObjectLodVertex {
        ObjectLodVertex {
            position,
            normal: Some([0.0, 0.0, 1.0]),
            uv: Some([0.25, 0.75]),
            color: Some([255, 128, 0, 255]),
        }
    }

    /// A level-16 shape: block-local vertices, scale 4 (any scale works), translation at the block origin plus
    /// `extra`, one segment holding every triangle.
    fn shape(block_index: usize, extra: [f32; 3]) -> ObjectLodShape {
        ObjectLodShape {
            block_index,
            flags: 0,
            translation: [0.0 + extra[0], -16.0 * 4096.0 + extra[1], extra[2]],
            rotation_columns: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            scale: 4.0,
            shader_property: u32::MAX,
            vertices: vec![
                vertex([0.0, 0.0, 0.0]),
                vertex([10.0, 0.0, 0.0]),
                vertex([0.0, 20.0, 5.0]),
            ],
            triangles: vec![[0, 1, 2]],
            segments: vec![ObjectLodSegment {
                flag: 0,
                start_index: 0,
                num_primitives: 1,
            }],
        }
    }

    fn block(shapes: Vec<ObjectLodShape>) -> ObjectLodFile {
        ObjectLodFile {
            user_version: 12,
            bethesda_version: 100,
            block_types: BTreeMap::new(),
            unhandled_blocks: BTreeMap::new(),
            shapes,
            large_ref_blocks: Vec::new(),
        }
    }

    /// The binary chunk of a GLB.
    fn binary(glb: &[u8]) -> &[u8] {
        let json_length = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let start = 20 + json_length + 8;
        &glb[start..]
    }

    fn floats(bytes: &[u8]) -> Vec<f32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| f32::from_le_bytes(*word))
            .collect()
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "the compiled values are exact in f32")]
    fn a_level_16_block_compiles_to_a_chunk_local_glb() {
        let chunk = compile_object_chunk(key(), ORIGIN, &block(vec![shape(3, [0.0; 3])]), &atlas())
            .unwrap();
        validate_object_glb(&chunk.glb).unwrap();
        assert_eq!(chunk.triangles, 1);
        assert_eq!(chunk.cells.len(), 256);
        assert_eq!(chunk.cells[0], (0, -16));
        assert_eq!(chunk.cells[255], (15, -1));
        // Chunk-local: the shape transform is applied and the block origin is not baked in.
        let positions = floats(&binary(&chunk.glb)[..36]);
        assert_eq!(
            positions,
            vec![0.0, 0.0, 0.0, 40.0, 0.0, 0.0, 0.0, 80.0, 20.0]
        );
        // Bounds are world-space Creation units: block origin (0, -65,536) plus the local box.
        assert_eq!(chunk.bounds_min, [0.0, -65_536.0, 0.0]);
        assert_eq!(chunk.bounds_max, [40.0, -65_456.0, 20.0]);
    }

    #[test]
    fn the_shape_translation_beyond_the_block_origin_is_kept() {
        let moved = shape(1, [4096.0, 8192.0, 100.0]);
        let chunk = compile_object_chunk(key(), ORIGIN, &block(vec![moved]), &atlas()).unwrap();
        let positions = floats(&binary(&chunk.glb)[..12]);
        assert_eq!(positions, vec![4096.0, 8192.0, 100.0]);
    }

    #[test]
    fn several_shapes_become_one_batch_each_under_the_objects_group() {
        let chunk = compile_object_chunk(
            key(),
            ORIGIN,
            &block(vec![shape(1, [0.0; 3]), shape(5, [4096.0, 0.0, 0.0])]),
            &atlas(),
        )
        .unwrap();
        validate_object_glb(&chunk.glb).unwrap();
        let document = crate::mesh::glb_json_from_bytes(&chunk.glb).unwrap();
        let names: Vec<_> = document["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|node| node["name"].as_str())
            .collect();
        assert!(names.contains(&"objects"));
        assert!(names.contains(&"object_batch_0") && names.contains(&"object_batch_1"));
        assert_eq!(document["meshes"].as_array().unwrap().len(), 2);
        assert_eq!(document["nodes"][3]["extras"]["source_block"], 5);
    }

    #[test]
    fn what_this_compiler_does_not_understand_is_refused_not_guessed() {
        let good = block(vec![shape(1, [0.0; 3])]);
        assert!(compile_object_chunk(key(), ORIGIN, &block(Vec::new()), &atlas()).is_err());
        assert!(compile_object_chunk(key(), ORIGIN, &good, b"").is_err());
        let mut two_segments = shape(1, [0.0; 3]);
        two_segments.segments.push(two_segments.segments[0]);
        assert!(
            compile_object_chunk(key(), ORIGIN, &block(vec![two_segments.clone()]), &atlas())
                .is_err()
        );
        let tier8 = ChunkKey::new(60, LodTier::Tier8, ChunkAnchor::new(12, 10));
        assert!(
            compile_object_chunk(tier8, ORIGIN, &block(vec![two_segments]), &atlas()).is_err(),
            "only level 4 splits a shape into cells"
        );
        let mut marked = good.clone();
        marked.large_ref_blocks.push(OpaqueBlock {
            block_index: 0,
            block_type: "BSDistantObjectLargeRefExtraData".into(),
            bytes: vec![1],
        });
        assert!(compile_object_chunk(key(), ORIGIN, &marked, &atlas()).is_err());
        let mut nan = shape(1, [0.0; 3]);
        nan.vertices[0].position[0] = f32::NAN;
        assert!(compile_object_chunk(key(), ORIGIN, &block(vec![nan]), &atlas()).is_err());
    }

    #[test]
    fn level_8_compiles_like_level_16_and_keeps_large_reference_data_undecoded() {
        let tier8 = ChunkKey::new(60, LodTier::Tier8, ChunkAnchor::new(12, 10));
        let mut marked = block(vec![shape(1, [0.0; 3])]);
        marked.large_ref_blocks.push(OpaqueBlock {
            block_index: 0,
            block_type: "BSDistantObjectLargeRefExtraData".into(),
            bytes: vec![1],
        });
        let chunk = compile_object_chunk(tier8, ORIGIN, &marked, &atlas()).unwrap();
        validate_object_glb(&chunk.glb).unwrap();
        assert_eq!(chunk.large_ref_blocks, 1);
        assert_eq!(chunk.cells.len(), 64);
        assert_eq!(chunk.cell_pieces, 0);
    }

    /// A level-4 shape of `triangles` triangles split into segments of the given sizes.
    fn segmented(triangles: usize, sizes: &[u32]) -> ObjectLodShape {
        let mut shape = shape(1, [0.0; 3]);
        shape.triangles = vec![[0, 1, 2]; triangles];
        shape.segments = sizes
            .iter()
            .scan(0, |start, &num_primitives| {
                let segment = ObjectLodSegment {
                    flag: 0,
                    start_index: *start,
                    num_primitives,
                };
                *start += num_primitives * 3;
                Some(segment)
            })
            .collect();
        shape
    }

    #[test]
    fn a_level_4_shape_becomes_one_cell_tagged_piece_per_non_empty_segment() {
        // Anchor (24, 20) from origin -96 is the 4-cell block whose south-west cell is (0, -16).
        let tier4 = ChunkKey::new(60, LodTier::Tier4, ChunkAnchor::new(24, 20));
        let chunk = compile_object_chunk(
            tier4,
            ORIGIN,
            &block(vec![segmented(4, &[1, 0, 2, 1])]),
            &atlas(),
        )
        .unwrap();
        validate_object_glb(&chunk.glb).unwrap();
        assert_eq!(chunk.triangles, 4);
        assert_eq!(chunk.cell_pieces, 3);
        let document = json_of(&chunk.glb);
        let nodes = document["nodes"].as_array().unwrap();
        let cells: Vec<_> = nodes
            .iter()
            .filter(|node| {
                node["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("object_cell_"))
            })
            .map(|node| node["extras"]["cell"].clone())
            .collect();
        // Segment i covers cell (x + i / 4, y + i % 4): segments 0, 2 and 3 are non-empty.
        assert_eq!(
            cells,
            vec![
                serde_json::json!([0, -16]),
                serde_json::json!([0, -14]),
                serde_json::json!([0, -13])
            ]
        );
        // The pieces share one vertex run and index their own triangles.
        let accessors = document["accessors"].as_array().unwrap();
        let counts: Vec<_> = accessors
            .iter()
            .filter(|accessor| accessor["type"] == "SCALAR")
            .map(|accessor| accessor["count"].as_u64().unwrap())
            .collect();
        assert_eq!(counts, vec![3, 6, 3]);
        // A shape with one segment covers slot 0.
        let plain = compile_object_chunk(tier4, ORIGIN, &block(vec![shape(1, [0.0; 3])]), &atlas())
            .unwrap();
        assert_eq!(plain.cell_pieces, 1);
        assert_eq!(
            json_of(&plain.glb)["nodes"][2]["extras"]["cell"],
            serde_json::json!([0, -16])
        );
        // More than 16 segments is refused.
        let too_many = segmented(17, &[1; 17]);
        assert!(compile_object_chunk(tier4, ORIGIN, &block(vec![too_many]), &atlas()).is_err());
    }

    #[test]
    fn a_block_must_sit_on_the_lod_grid() {
        let aligned = chunk_key_for_block(60, ORIGIN, 16, 0, -16).unwrap();
        assert_eq!(aligned, key());
        assert!(chunk_key_for_block(60, ORIGIN, 16, 1, -16).is_err());
        assert!(
            chunk_key_for_block(60, ORIGIN, 32, 0, -32).is_err(),
            "level 32 is not a tier"
        );
        // A world whose LOD origin is not a multiple of 16 still aligns to its own origin.
        let odd = LodOrigin::new(-3, 0);
        assert_eq!(
            chunk_key_for_block(61, odd, 16, 13, 16).unwrap().anchor,
            ChunkAnchor::new(1, 1)
        );
    }

    #[test]
    fn publishing_writes_the_file_and_the_rows_and_replaces_on_repeat() {
        let chunk = compile_object_chunk(key(), ORIGIN, &block(vec![shape(1, [0.0; 3])]), &atlas())
            .unwrap();
        let connection = Connection::open_in_memory().unwrap();
        let staging = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            publish_object_chunk(&connection, staging.path(), &chunk).unwrap();
        }
        let path = object_chunk_payload_path(key());
        let written = std::fs::read(staging.path().join(&path)).unwrap();
        let (stored_path, stored_hash, cells): (String, String, String) = connection
            .query_row(
                "SELECT payload_path, content_hash, source_cells FROM lod_object_chunks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored_path, path);
        assert_eq!(stored_hash, crate::cache::hash_bytes(&written));
        assert_eq!(cells.split(';').count(), 256);
        for table in ["lod_object_chunks", "lod_object_chunks_spatial"] {
            let rows: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(rows, 1, "{table} holds one row after a repeat publish");
        }
    }

    #[test]
    fn a_database_without_object_chunks_has_no_object_tables() {
        let connection = Connection::open_in_memory().unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name LIKE 'lod_object%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            tables, 0,
            "the shared schema is unchanged for packs without object LOD"
        );
    }

    #[test]
    fn a_damaged_payload_fails_validation() {
        let chunk = compile_object_chunk(key(), ORIGIN, &block(vec![shape(1, [0.0; 3])]), &atlas())
            .unwrap();
        assert!(validate_object_glb(&chunk.glb[..20]).is_err());
        assert!(validate_object_glb(b"not a glb at all, really").is_err());
        // A terrain-shaped GLB has no `objects` group.
        let mut renamed = chunk.glb.clone();
        let needle = b"\"objects\"";
        let at = renamed
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap();
        renamed[at + 1..at + 8].copy_from_slice(b"terrain");
        assert!(validate_object_glb(&renamed).is_err());
    }

    fn json_of(glb: &[u8]) -> serde_json::Value {
        crate::mesh::glb_json_from_bytes(glb).unwrap()
    }

    #[test]
    fn the_alpha_cutout_culls_even_when_node_flags_include_0x108() {
        let mut flagged = shape(2, [0.0; 3]);
        flagged.flags = 0x108;
        let chunk = compile_object_chunk(
            key(),
            ORIGIN,
            &block(vec![shape(1, [0.0; 3]), flagged]),
            &atlas(),
        )
        .unwrap();
        validate_object_glb(&chunk.glb).unwrap();
        let document = json_of(&chunk.glb);
        let materials = document["materials"].as_array().unwrap();
        assert_eq!(materials.len(), 1);
        assert_eq!(materials[0]["alphaMode"], "MASK");
        assert_eq!(materials[0]["alphaCutoff"].as_f64(), Some(128.0 / 255.0));
        assert_eq!(materials[0]["doubleSided"], false);
        assert!(
            document["meshes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|mesh| { mesh["primitives"][0]["material"] == 0 })
        );
    }

    #[test]
    fn tier_four_uses_stored_index_ranges_and_rejects_bad_ranges() {
        let tier4 = ChunkKey::new(60, LodTier::Tier4, ChunkAnchor::new(24, 20));
        let mut shape = segmented(2, &[1, 1]);
        shape.triangles[1] = [2, 1, 0];
        shape.segments[0].start_index = 3;
        shape.segments[1].start_index = 0;
        let compile = |shape| compile_object_chunk(tier4, ORIGIN, &block(vec![shape]), &atlas());
        let chunk = compile(shape.clone()).unwrap();
        let doc = json_of(&chunk.glb);
        let primitive = &doc["meshes"][0]["primitives"][0];
        let accessor = &doc["accessors"][json_index(&primitive["indices"]).unwrap()];
        let view = &doc["bufferViews"][json_index(&accessor["bufferView"]).unwrap()];
        let start = json_offset(&view["byteOffset"]).unwrap()
            + json_offset(&accessor["byteOffset"]).unwrap();
        assert_eq!(&binary(&chunk.glb)[start..start + 4], &2u32.to_le_bytes());
        for invalid in [1, 3, 6, u32::MAX] {
            shape.segments[1].start_index = invalid;
            assert!(
                compile(shape.clone()).is_err(),
                "accepted start index {invalid}"
            );
        }
    }

    #[test]
    fn missing_uv_refuses_the_block_and_missing_optional_streams_are_omitted() {
        let mut undetailed = shape(1, [0.0; 3]);
        for vertex in &mut undetailed.vertices {
            vertex.color = None;
            vertex.normal = None;
        }
        let compile = |shapes| compile_object_chunk(key(), ORIGIN, &block(shapes), &atlas());
        let mixed = compile(vec![undetailed.clone(), shape(2, [0.0; 3])]).unwrap();
        validate_object_glb(&mixed.glb).unwrap();
        let doc = json_of(&mixed.glb);
        let attrs = |mesh: usize| {
            doc["meshes"][mesh]["primitives"][0]["attributes"]
                .as_object()
                .unwrap()
        };
        assert_eq!(attrs(0).len(), 2);
        assert!(!attrs(0).contains_key("NORMAL"));
        assert!(!attrs(0).contains_key("COLOR_0"));
        assert!(attrs(1).contains_key("NORMAL"));
        assert!(attrs(1).contains_key("COLOR_0"));
        undetailed.vertices[1].uv = None;
        assert!(
            compile(vec![undetailed])
                .err()
                .unwrap()
                .is::<MissingObjectUv>()
        );
    }

    /// Patches `from` to `to` (same length) in the JSON chunk of a GLB.
    fn patch(glb: &[u8], from: &str, to: &str) -> Vec<u8> {
        assert_eq!(from.len(), to.len());
        let mut out = glb.to_vec();
        let at = out
            .windows(from.len())
            .position(|window| window == from.as_bytes())
            .unwrap();
        out[at..at + from.len()].copy_from_slice(to.as_bytes());
        out
    }

    #[test]
    fn every_kind_of_damage_is_refused_with_a_valid_atlas_as_baseline() {
        let good = compile_object_chunk(key(), ORIGIN, &block(vec![shape(1, [0.0; 3])]), &atlas())
            .unwrap();
        validate_object_glb(&good.glb).unwrap();
        // A stream that points outside its buffer view.
        assert!(validate_object_glb(&patch(&good.glb, "\"count\":3", "\"count\":9")).is_err());
        // An atlas that is not a KTX2 file.
        let fake =
            compile_object_chunk(key(), ORIGIN, &block(vec![shape(1, [0.0; 3])]), b"KTX2").unwrap();
        assert!(validate_object_glb(&fake.glb).is_err());
        // A truncated file (the declared length no longer matches).
        assert!(validate_object_glb(&good.glb[..good.glb.len() - 4]).is_err());
        // An index past the vertices: overwrite the first index word.
        let json_length = u32::from_le_bytes(good.glb[12..16].try_into().unwrap()) as usize;
        let binary_start = 20 + json_length + 8;
        let index_at = binary_start + 3 * 12 * 2 + 3 * 8 + 3 * 16; // after P, N, UV, colour
        let mut bad_index = good.glb.clone();
        bad_index[index_at..index_at + 4].copy_from_slice(&99u32.to_le_bytes());
        assert!(validate_object_glb(&bad_index).is_err());
        // A non-finite vertex.
        let mut nan = good.glb.clone();
        nan[binary_start..binary_start + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(validate_object_glb(&nan).is_err());
        // An opaque or blended material is not the retail cutout.
        for mode in ["OPAQ", "BLEN"] {
            assert!(
                validate_object_glb(&patch(&good.glb, "\"MASK\"", &format!("\"{mode}\""))).is_err()
            );
        }
    }

    /// A pack root with a database, `lod-manifest.json` and one published object chunk.
    fn published_pack(root: &Path) -> (ObjectChunk, String) {
        let connection = Connection::open(root.join("skyrim_world.db")).unwrap();
        let chunk = compile_object_chunk(key(), ORIGIN, &block(vec![shape(1, [0.0; 3])]), &atlas())
            .unwrap();
        publish_object_chunk(&connection, root, &chunk).unwrap();
        let path = object_chunk_payload_path(chunk.key);
        let manifest = serde_json::json!({
            "object_chunks": 1,
            "objects_compiler_version": OBJECT_COMPILER_VERSION,
            "object_chunk_inputs": { path.clone(): "proof" },
        });
        std::fs::write(root.join("lod-manifest.json"), manifest.to_string()).unwrap();
        (chunk, path)
    }

    #[test]
    fn the_package_check_fails_a_missing_damaged_or_unlisted_object_chunk() {
        // A pack without object LOD passes and reads nothing else.
        let plain = tempfile::tempdir().unwrap();
        validate_object_lod_package(plain.path()).unwrap();

        let root = tempfile::tempdir().unwrap();
        let (chunk, path) = published_pack(root.path());
        validate_object_lod_package(root.path()).unwrap();

        // Damaged: a flipped byte changes the hash.
        let file = root.path().join(&path);
        let mut bytes = std::fs::read(&file).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&file, &bytes).unwrap();
        assert!(
            format!(
                "{:#}",
                validate_object_lod_package(root.path()).unwrap_err()
            )
            .contains("SHA-256")
        );
        // Missing.
        std::fs::remove_file(&file).unwrap();
        assert!(
            format!(
                "{:#}",
                validate_object_lod_package(root.path()).unwrap_err()
            )
            .contains("missing")
        );
        // Restored, then a manifest that does not list it.
        std::fs::write(&file, &chunk.glb).unwrap();
        validate_object_lod_package(root.path()).unwrap();
        std::fs::write(root.path().join("lod-manifest.json"), "{}").unwrap();
        assert!(validate_object_lod_package(root.path()).is_err());
    }

    #[test]
    fn the_input_hash_depends_on_every_input() {
        let base = object_chunk_input_hash(key(), ORIGIN, "a", "b").unwrap();
        assert_eq!(
            base,
            object_chunk_input_hash(key(), ORIGIN, "a", "b").unwrap()
        );
        assert_ne!(
            base,
            object_chunk_input_hash(key(), ORIGIN, "c", "b").unwrap()
        );
        assert_ne!(
            base,
            object_chunk_input_hash(key(), ORIGIN, "a", "c").unwrap()
        );
        assert_ne!(
            base,
            object_chunk_input_hash(key(), LodOrigin::new(-95, -96), "a", "b").unwrap()
        );
    }

    #[test]
    fn a_shared_atlas_chunk_references_the_atlas_file_and_embeds_nothing() {
        let block = block(vec![shape(1, [0.0; 3])]);
        let embedded = compile_object_chunk(key(), ORIGIN, &block, &atlas()).unwrap();
        let shared = compile_object_chunk_shared_atlas(key(), ORIGIN, &block).unwrap();
        validate_object_glb(&shared.glb).unwrap();
        assert!(chunk_uses_shared_atlas(&shared.glb).unwrap());
        assert!(!chunk_uses_shared_atlas(&embedded.glb).unwrap());
        assert!(shared.glb.len() < embedded.glb.len());
        let document = json_of(&shared.glb);
        assert_eq!(
            document["images"],
            serde_json::json!([{ "uri": OBJECT_ATLAS_URI, "mimeType": "image/ktx2" }])
        );
        assert_eq!(document["bufferViews"].as_array().unwrap().len(), 5);
        // Any other URI is refused.
        assert!(
            validate_object_glb(&patch(
                &shared.glb,
                OBJECT_ATLAS_URI,
                "../objects_atlaz.opensky-srgb.ktx2"
            ))
            .is_err()
        );
    }

    /// A pack root with a shared atlas, one chunk that references it, and a manifest that lists both.
    fn shared_pack(root: &Path) -> (String, String) {
        let connection = Connection::open(root.join("skyrim_world.db")).unwrap();
        let (atlas_path, atlas_hash) =
            publish_object_atlas(&connection, root, 60, &atlas()).unwrap();
        let chunk =
            compile_object_chunk_shared_atlas(key(), ORIGIN, &block(vec![shape(1, [0.0; 3])]))
                .unwrap();
        publish_object_chunk(&connection, root, &chunk).unwrap();
        let chunk_path = object_chunk_payload_path(chunk.key);
        let manifest = serde_json::json!({
            "object_chunks": 1,
            "objects_compiler_version": OBJECT_COMPILER_VERSION,
            "object_chunk_inputs": { chunk_path.clone(): "proof" },
            "object_atlases": { atlas_path.clone(): atlas_hash },
        });
        std::fs::write(root.join("lod-manifest.json"), manifest.to_string()).unwrap();
        (atlas_path, chunk_path)
    }

    #[test]
    fn the_package_check_covers_the_shared_atlas() {
        let root = tempfile::tempdir().unwrap();
        let (atlas_path, _) = shared_pack(root.path());
        assert_eq!(atlas_path, object_atlas_path(60));
        validate_object_lod_package(root.path()).unwrap();
        let file = root.path().join(&atlas_path);
        let original = std::fs::read(&file).unwrap();
        // Damaged: the hash no longer matches.
        let mut damaged = original.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 0xff;
        std::fs::write(&file, &damaged).unwrap();
        let message = format!(
            "{:#}",
            validate_object_lod_package(root.path()).unwrap_err()
        );
        assert!(
            message.contains("SHA-256") && message.contains("atlas"),
            "{message}"
        );
        // Missing.
        std::fs::remove_file(&file).unwrap();
        let message = format!(
            "{:#}",
            validate_object_lod_package(root.path()).unwrap_err()
        );
        assert!(
            message.contains("atlas") && message.contains("missing"),
            "{message}"
        );
        std::fs::write(&file, &original).unwrap();
        validate_object_lod_package(root.path()).unwrap();
        // The manifest does not list the atlas.
        let manifest_path = root.path().join("lod-manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["object_atlases"] = serde_json::json!({});
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        assert!(validate_object_lod_package(root.path()).is_err());
        // A chunk that references the atlas has no atlas row.
        let connection = Connection::open(root.path().join("skyrim_world.db")).unwrap();
        connection
            .execute("DELETE FROM lod_object_atlases", [])
            .unwrap();
        manifest["object_atlases"] = serde_json::json!({});
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        let message = format!(
            "{:#}",
            validate_object_lod_package(root.path()).unwrap_err()
        );
        assert!(message.contains("no atlas row"), "{message}");
    }
}
