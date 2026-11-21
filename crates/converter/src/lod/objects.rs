//! Object LOD chunk compiler: retail `.bto` blocks into one GLB per chunk (D-03, first slice).
//!
//! Slice S1 is Tamriel level 16: one retail block is one chunk of tier 16, and every level-16
//! shape has exactly one segment and no large-reference marker (verified), so the whole shape is
//! one mesh and no segment or marker logic is needed. Levels 8 and 4 are not compiled here:
//! level 8 follows the same path, and level 4 needs the per-cell segment split.
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
//! Unverified, and therefore conservative: the material is opaque and double-sided (the retail
//! object LOD shader's alpha handling is not decoded), and triangle winding is kept as stored.

use super::{
    retail::bto::{ObjectLodFile, ObjectLodShape},
    terrain::{assemble_glb, f32_slice_bytes, pad_to_4},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use rusqlite::{Connection, params};
use shared::lod::{ChunkKey, LodOrigin, LodTier, nodes, object_chunk_payload_path};
use std::path::Path;

/// Explicit producer revision of the object chunk compiler. Terrain chunk reuse is unaffected.
pub const OBJECT_COMPILER_VERSION: u32 = 1;

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
    let tier = i32::try_from(level)
        .ok()
        .and_then(LodTier::from_side_cells)
        .ok_or_else(|| {
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

/// Compiles one retail level-16 block into an object chunk. `atlas_ktx2` is the world's object
/// atlas, already encoded as a KTX2 container, embedded as the single base-colour texture.
///
/// Fails for a tier other than 16 (not compiled yet), for a block with no geometry, and for a
/// shape that has more than one segment or any large-reference block, which a level-16 shape never
/// has: such a file is not what this slice understands, so it is refused instead of guessed.
pub fn compile_object_chunk(
    key: ChunkKey,
    origin: LodOrigin,
    block: &ObjectLodFile,
    atlas_ktx2: &[u8],
) -> Result<ObjectChunk> {
    ensure!(
        key.tier == LodTier::Tier16,
        "object chunk {key:?}: only tier 16 is compiled in this slice (lower tiers need the per-cell segment split)"
    );
    ensure!(
        !block.shapes.is_empty(),
        "object chunk {key:?}: the retail block has no geometry"
    );
    ensure!(
        block.large_ref_blocks.is_empty(),
        "object chunk {key:?}: a level-16 block carries large-reference markers, which this slice does not read"
    );
    ensure!(
        !atlas_ktx2.is_empty(),
        "object chunk {key:?}: the atlas texture is empty"
    );
    let (cell_x, cell_y) = chunk_origin_cell(key, origin);
    let block_origin = [
        (f64::from(cell_x) * CELL_SIZE) as f32,
        (f64::from(cell_y) * CELL_SIZE) as f32,
        0.0,
    ];

    let mut streams = Streams::default();
    let mut shape_ranges = Vec::new(); // (vertex start, vertex count, index start, index count)
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut triangles = 0usize;
    for shape in &block.shapes {
        ensure!(
            shape.segments.len() == 1,
            "object chunk {key:?}: shape at block {} has {} segments, a level-16 shape has one",
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
            streams.normals.extend(transform(
                shape,
                vertex.normal.unwrap_or([0.0, 0.0, 1.0]),
                false,
            ));
            streams.uvs.extend(vertex.uv.unwrap_or([0.0, 0.0]));
            streams.colors.extend(
                vertex
                    .color
                    .map_or([1.0; 4], |rgba| rgba.map(|byte| f32::from(byte) / 255.0)),
            );
        }
        for triangle in &shape.triangles {
            streams.indices.extend(triangle.map(u32::from));
        }
        triangles += shape.triangles.len();
        shape_ranges.push((
            vertex_start,
            shape.vertices.len(),
            index_start,
            shape.triangles.len() * 3,
        ));
    }

    let glb = emit_object_glb(key, &streams, &shape_ranges, atlas_ktx2, block)?;
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
    })
}

fn emit_object_glb(
    key: ChunkKey,
    streams: &Streams,
    shape_ranges: &[(usize, usize, usize, usize)],
    atlas: &[u8],
    block: &ObjectLodFile,
) -> Result<Vec<u8>> {
    let mut binary: Vec<u8> = Vec::new();
    binary.extend_from_slice(f32_slice_bytes(&streams.positions));
    let normal_offset = binary.len();
    binary.extend_from_slice(f32_slice_bytes(&streams.normals));
    let uv_offset = binary.len();
    binary.extend_from_slice(f32_slice_bytes(&streams.uvs));
    let color_offset = binary.len();
    binary.extend_from_slice(f32_slice_bytes(&streams.colors));
    pad_to_4(&mut binary);
    let index_offset = binary.len();
    for index in &streams.indices {
        binary.extend_from_slice(&index.to_le_bytes());
    }
    pad_to_4(&mut binary);
    let atlas_offset = binary.len();
    binary.extend_from_slice(atlas);
    pad_to_4(&mut binary);

    let chunk_name = format!(
        "objects_chunk_{}_{}_{}",
        key.tier.side_cells(),
        key.anchor.x,
        key.anchor.y
    );
    let shape_count = shape_ranges.len();
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
    for (index, ((vertex_start, vertex_count, index_start, index_count), shape)) in
        shape_ranges.iter().zip(&block.shapes).enumerate()
    {
        let base = index * 5;
        let positions = &streams.positions[vertex_start * 3..(vertex_start + vertex_count) * 3];
        let (shape_min, shape_max) = bounds(positions);
        nodes_json.push(serde_json::json!({
            "name": format!("object_batch_{index}"),
            "extras": { "source_block": shape.block_index },
            "mesh": index,
        }));
        for (view, offset, count, kind, extra) in [
            (
                0,
                vertex_start * 12,
                *vertex_count,
                "VEC3",
                Some((shape_min, shape_max)),
            ),
            (1, vertex_start * 12, *vertex_count, "VEC3", None),
            (2, vertex_start * 8, *vertex_count, "VEC2", None),
            (3, vertex_start * 16, *vertex_count, "VEC4", None),
        ] {
            let mut accessor = serde_json::json!({
                "bufferView": view,
                "byteOffset": offset,
                "componentType": 5126,
                "count": count,
                "type": kind,
            });
            if let Some((low, high)) = extra {
                accessor["min"] = serde_json::json!(low);
                accessor["max"] = serde_json::json!(high);
            }
            accessors_json.push(accessor);
        }
        accessors_json.push(serde_json::json!({
            "bufferView": 4,
            "byteOffset": index_start * 4,
            "componentType": 5125,
            "count": index_count,
            "type": "SCALAR",
        }));
        meshes_json.push(serde_json::json!({
            "name": format!("objects_{}_{}_shape_{index}", key.anchor.x, key.anchor.y),
            "primitives": [{
                "attributes": {
                    "POSITION": base,
                    "NORMAL": base + 1,
                    "TEXCOORD_0": base + 2,
                    "COLOR_0": base + 3,
                },
                "indices": base + 4,
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
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": streams.positions.len() * 4, "target": 34962 },
            { "buffer": 0, "byteOffset": normal_offset, "byteLength": streams.normals.len() * 4, "target": 34962 },
            { "buffer": 0, "byteOffset": uv_offset, "byteLength": streams.uvs.len() * 4, "target": 34962 },
            { "buffer": 0, "byteOffset": color_offset, "byteLength": streams.colors.len() * 4, "target": 34962 },
            { "buffer": 0, "byteOffset": index_offset, "byteLength": streams.indices.len() * 4, "target": 34963 },
            { "buffer": 0, "byteOffset": atlas_offset, "byteLength": atlas.len() },
        ],
        "buffers": [{ "byteLength": binary.len() }],
        "images": [{ "bufferView": 5, "mimeType": "image/ktx2" }],
        "samplers": [{ "magFilter": 9729, "minFilter": 9987, "wrapS": 33071, "wrapT": 33071 }],
        "textures": [{ "source": 0, "sampler": 0, "extensions": { "KHR_texture_basisu": { "source": 0 } } }],
        "materials": [{
            "name": "lod_objects",
            "pbrMetallicRoughness": { "baseColorFactor": [1.0, 1.0, 1.0, 1.0], "baseColorTexture": { "index": 0 }, "metallicFactor": 0.0, "roughnessFactor": 1.0 },
            "doubleSided": true,
        }],
    });
    assemble_glb(&json, &binary)
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
             );",
        )
        .wrap_err("failed to create the object LOD tables")
}

/// Checks the structure of a written object chunk GLB: the `objects` group exists, every mesh
/// node under it names a mesh with a position, normal, texture-coordinate and colour stream and
/// an index stream whose accessors stay inside the buffer, and the single atlas image is present.
pub fn validate_object_glb(bytes: &[u8]) -> Result<()> {
    let document = crate::mesh::glb_json_from_bytes(bytes)?;
    let nodes_json = document["nodes"]
        .as_array()
        .ok_or_else(|| color_eyre::eyre::eyre!("no nodes"))?;
    let group = nodes_json
        .iter()
        .find(|node| node["name"] == nodes::objects_group())
        .ok_or_else(|| color_eyre::eyre::eyre!("object chunk has no `objects` group"))?;
    let children = group["children"].as_array().filter(|c| !c.is_empty());
    let children =
        children.ok_or_else(|| color_eyre::eyre::eyre!("the `objects` group is empty"))?;
    let accessors = document["accessors"]
        .as_array()
        .ok_or_else(|| color_eyre::eyre::eyre!("no accessors"))?;
    let meshes = document["meshes"]
        .as_array()
        .ok_or_else(|| color_eyre::eyre::eyre!("no meshes"))?;
    let buffer_length = document["buffers"][0]["byteLength"].as_u64().unwrap_or(0);
    for child in children {
        let node = child
            .as_u64()
            .and_then(|index| nodes_json.get(index as usize))
            .ok_or_else(|| color_eyre::eyre::eyre!("a group child is not a node"))?;
        ensure!(
            node["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("object_batch_")),
            "a node under `objects` is not an object batch: {node}"
        );
        let mesh = node["mesh"]
            .as_u64()
            .and_then(|index| meshes.get(index as usize))
            .ok_or_else(|| color_eyre::eyre::eyre!("an object batch has no mesh"))?;
        let primitive = &mesh["primitives"][0];
        for attribute in ["POSITION", "NORMAL", "TEXCOORD_0", "COLOR_0"] {
            ensure!(
                primitive["attributes"][attribute].is_u64(),
                "an object batch lacks {attribute}"
            );
        }
        for accessor in primitive["attributes"]
            .as_object()
            .into_iter()
            .flat_map(|attributes| attributes.values())
            .chain(std::iter::once(&primitive["indices"]))
        {
            let accessor = accessor
                .as_u64()
                .and_then(|index| accessors.get(index as usize))
                .ok_or_else(|| color_eyre::eyre::eyre!("an accessor index is out of range"))?;
            ensure!(
                accessor["count"].as_u64().is_some_and(|count| count > 0),
                "an accessor is empty"
            );
        }
    }
    ensure!(
        document["images"]
            .as_array()
            .is_some_and(|images| images.len() == 1),
        "an object chunk carries exactly one atlas image"
    );
    ensure!(buffer_length > 0, "the object chunk has no binary data");
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
                value: 0,
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
    fn a_level_16_block_compiles_to_a_chunk_local_glb() {
        let chunk =
            compile_object_chunk(key(), ORIGIN, &block(vec![shape(3, [0.0; 3])]), b"KTX2").unwrap();
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
        let chunk = compile_object_chunk(key(), ORIGIN, &block(vec![moved]), b"KTX2").unwrap();
        let positions = floats(&binary(&chunk.glb)[..12]);
        assert_eq!(positions, vec![4096.0, 8192.0, 100.0]);
    }

    #[test]
    fn several_shapes_become_one_batch_each_under_the_objects_group() {
        let chunk = compile_object_chunk(
            key(),
            ORIGIN,
            &block(vec![shape(1, [0.0; 3]), shape(5, [4096.0, 0.0, 0.0])]),
            b"KTX2",
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
    fn what_this_slice_does_not_understand_is_refused_not_guessed() {
        let good = block(vec![shape(1, [0.0; 3])]);
        let tier8 = ChunkKey::new(60, LodTier::Tier8, ChunkAnchor::new(12, 10));
        assert!(compile_object_chunk(tier8, ORIGIN, &good, b"KTX2").is_err());
        assert!(compile_object_chunk(key(), ORIGIN, &block(Vec::new()), b"KTX2").is_err());
        assert!(compile_object_chunk(key(), ORIGIN, &good, b"").is_err());
        let mut two_segments = shape(1, [0.0; 3]);
        two_segments.segments.push(two_segments.segments[0]);
        assert!(compile_object_chunk(key(), ORIGIN, &block(vec![two_segments]), b"KTX2").is_err());
        let mut marked = good.clone();
        marked.large_ref_blocks.push(OpaqueBlock {
            block_index: 0,
            block_type: "BSDistantObjectLargeRefExtraData".into(),
            bytes: vec![1],
        });
        assert!(compile_object_chunk(key(), ORIGIN, &marked, b"KTX2").is_err());
        let mut nan = shape(1, [0.0; 3]);
        nan.vertices[0].position[0] = f32::NAN;
        assert!(compile_object_chunk(key(), ORIGIN, &block(vec![nan]), b"KTX2").is_err());
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
        let chunk =
            compile_object_chunk(key(), ORIGIN, &block(vec![shape(1, [0.0; 3])]), b"KTX2").unwrap();
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
        let chunk =
            compile_object_chunk(key(), ORIGIN, &block(vec![shape(1, [0.0; 3])]), b"KTX2").unwrap();
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
}
