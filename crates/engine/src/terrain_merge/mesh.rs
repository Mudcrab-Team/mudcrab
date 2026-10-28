//! The render mesh of one merged terrain cell (#215).
//!
//! The mesh carries what today's four quadrant meshes carry, vertex for vertex: position, normal,
//! cell-wide UV and `COLOR_0`. In place of the packed tangent and second UV set, which hold the
//! fallback weights, it carries the quadrant's texture slots as two custom attribute families:
//! land texture ids (`Uint16x4` each) and slot weights (`Unorm8x4` each), two of each for a
//! stride of 8 slots and four of each for a stride of 16. The mesh keeps its four quadrants
//! unwelded, 4 x 17 x 17 vertices, because a seam vertex carries a different slot list on each
//! side.

use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, MeshVertexAttribute, PrimitiveTopology, VertexAttributeValues},
    prelude::*,
    render::render_resource::VertexFormat,
};
use shared::{
    coordinates::creation_to_runtime_vector,
    terrain_mesh::{
        CELL_MESH_VERTICES, QUADRANT_SIDE, QUADRANT_VERTICES, TerrainCellMesh, grid_position,
        quadrant_triangles, vertex_index, vertex_xy,
    },
};
use std::collections::HashMap;

/// Attribute ids are arbitrary numbers that must not collide with Bevy's own (which are small).
const ATTRIBUTE_ID_BASE: u64 = 0x7E44_A100;

/// Land texture ids of slots 0-3, 4-7, 8-11 and 12-15 of the vertex's quadrant.
pub const TERRAIN_SLOT_IDS: [MeshVertexAttribute; 4] = [
    MeshVertexAttribute::new("TerrainSlotIds0", ATTRIBUTE_ID_BASE, VertexFormat::Uint16x4),
    MeshVertexAttribute::new(
        "TerrainSlotIds1",
        ATTRIBUTE_ID_BASE + 1,
        VertexFormat::Uint16x4,
    ),
    MeshVertexAttribute::new(
        "TerrainSlotIds2",
        ATTRIBUTE_ID_BASE + 2,
        VertexFormat::Uint16x4,
    ),
    MeshVertexAttribute::new(
        "TerrainSlotIds3",
        ATTRIBUTE_ID_BASE + 3,
        VertexFormat::Uint16x4,
    ),
];

/// Weights of the same slots at this vertex. Slot 0's weight is implied and always stored as 0.
pub const TERRAIN_SLOT_WEIGHTS: [MeshVertexAttribute; 4] = [
    MeshVertexAttribute::new(
        "TerrainSlotWeights0",
        ATTRIBUTE_ID_BASE + 4,
        VertexFormat::Unorm8x4,
    ),
    MeshVertexAttribute::new(
        "TerrainSlotWeights1",
        ATTRIBUTE_ID_BASE + 5,
        VertexFormat::Unorm8x4,
    ),
    MeshVertexAttribute::new(
        "TerrainSlotWeights2",
        ATTRIBUTE_ID_BASE + 6,
        VertexFormat::Unorm8x4,
    ),
    MeshVertexAttribute::new(
        "TerrainSlotWeights3",
        ATTRIBUTE_ID_BASE + 7,
        VertexFormat::Unorm8x4,
    ),
];

/// The vertex shader input location of id attribute `group`; weights follow 4 locations later.
pub const SLOT_IDS_FIRST_LOCATION: u32 = 8;
pub const SLOT_WEIGHTS_FIRST_LOCATION: u32 = 12;

/// The slot attributes of one cell, in the order they are inserted into the mesh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotAttributes {
    /// `stride / 4` groups, each [`CELL_MESH_VERTICES`] long.
    pub ids: Vec<Vec<[u16; 4]>>,
    pub weights: Vec<Vec<[u8; 4]>>,
}

/// Expands the cell's per-quadrant FormID slots and per-vertex weights into the vertex
/// attributes: every vertex gets its quadrant's ids (FormIDs mapped through `ids`, unused slots
/// id 0) and its own weights. Fails on a FormID the table lacks or a wrongly sized mesh, which
/// [`TerrainCellMesh::validate`] refuses first.
pub fn expand_slots(
    cell: &TerrainCellMesh,
    ids: &HashMap<u32, u16>,
) -> Result<SlotAttributes, String> {
    let stride = usize::from(cell.slot_stride);
    if !shared::terrain_mesh::TERRAIN_SLOT_STRIDES.contains(&cell.slot_stride)
        || cell.quadrant_slots.len() != 4 * stride
        || cell.weights.len() != CELL_MESH_VERTICES * stride
    {
        return Err(format!(
            "slot stride {stride} does not match the cell's data"
        ));
    }
    let groups = stride / 4;
    let mut quadrant_ids = vec![0u16; 4 * stride];
    for quadrant in 0..4 {
        let count = usize::from(cell.quadrant_slot_counts[quadrant]);
        for slot in 0..count.min(stride) {
            let form_id = cell.quadrant_slots[quadrant * stride + slot];
            quadrant_ids[quadrant * stride + slot] = *ids
                .get(&form_id)
                .ok_or_else(|| format!("land texture {form_id:08X} is not in the table"))?;
        }
    }
    let mut id_groups = vec![Vec::with_capacity(CELL_MESH_VERTICES); groups];
    let mut weight_groups = vec![Vec::with_capacity(CELL_MESH_VERTICES); groups];
    for vertex in 0..CELL_MESH_VERTICES {
        let quadrant = vertex / QUADRANT_VERTICES;
        for group in 0..groups {
            let first = group * 4;
            id_groups[group].push(std::array::from_fn(|lane| {
                quadrant_ids[quadrant * stride + first + lane]
            }));
            weight_groups[group].push(std::array::from_fn(|lane| {
                // Slot 0 is implied by the others, so it never reaches the shader as a weight.
                if first + lane == 0 {
                    0
                } else {
                    cell.weights[vertex * stride + first + lane]
                }
            }));
        }
    }
    Ok(SlotAttributes {
        ids: id_groups,
        weights: weight_groups,
    })
}

/// The 33 x 33 height and normal grid a merged cell's four quadrants cover: every grid sample
/// once, with the duplicated seam vertices checked to agree. A mesh whose duplicates disagree
/// cannot come from one height field, so the caller falls back to the runtime build for it.
pub fn cell_to_grid(cell: &TerrainCellMesh) -> Result<(Vec<f32>, Vec<i8>), String> {
    const SIDE: usize = 2 * (QUADRANT_SIDE - 1) + 1;
    let mut heights: Vec<Option<f32>> = vec![None; SIDE * SIDE];
    let mut normals: Vec<Option<[i8; 3]>> = vec![None; SIDE * SIDE];
    for quadrant in 0..4 {
        for y in 0..QUADRANT_SIDE {
            for x in 0..QUADRANT_SIDE {
                let (gx, gy) = grid_position(quadrant, x, y);
                let vertex = vertex_index(quadrant, x, y);
                let height = cell.heights[vertex];
                let normal = [
                    cell.normals[vertex * 3],
                    cell.normals[vertex * 3 + 1],
                    cell.normals[vertex * 3 + 2],
                ];
                let slot = gy * SIDE + gx;
                match (heights[slot], normals[slot]) {
                    (None, _) => {
                        heights[slot] = Some(height);
                        normals[slot] = Some(normal);
                    }
                    (Some(seen), Some(seen_normal)) => {
                        if seen.to_bits() != height.to_bits() || seen_normal != normal {
                            return Err(format!(
                                "seam vertex at grid ({gx}, {gy}) differs between quadrants"
                            ));
                        }
                    }
                    (Some(_), None) => unreachable!("height and normal are set together"),
                }
            }
        }
    }
    let heights = heights
        .into_iter()
        .map(|height| height.expect("every grid sample belongs to a quadrant"))
        .collect();
    let normals = normals
        .into_iter()
        .flat_map(|normal| normal.expect("every grid sample belongs to a quadrant"))
        .collect();
    Ok((heights, normals))
}

/// Writes a (possibly welded) grid back to every vertex of the cell, seam duplicates included.
pub fn write_grid(cell: &mut TerrainCellMesh, heights: &[f32], normals: &[i8]) {
    const SIDE: usize = 2 * (QUADRANT_SIDE - 1) + 1;
    for quadrant in 0..4 {
        for y in 0..QUADRANT_SIDE {
            for x in 0..QUADRANT_SIDE {
                let (gx, gy) = grid_position(quadrant, x, y);
                let vertex = vertex_index(quadrant, x, y);
                let slot = gy * SIDE + gx;
                cell.heights[vertex] = heights[slot];
                cell.normals[vertex * 3..vertex * 3 + 3]
                    .copy_from_slice(&normals[slot * 3..slot * 3 + 3]);
            }
        }
    }
}

/// Whether any packed normal is the zero vector, which today's validation refuses.
pub fn has_zero_normal(cell: &TerrainCellMesh) -> bool {
    cell.normals
        .as_chunks::<3>()
        .0
        .iter()
        .any(|normal| normal == &[0, 0, 0])
}

/// Builds the render mesh of a validated, welded cell. Positions, normals, UVs, colours and
/// triangles reproduce today's four quadrant meshes (`build_terrain_quadrant_mesh`) exactly,
/// stacked in quadrant order; indices are `u32` so the collider reads the same buffers it does
/// for a quadrant.
pub fn build_merged_mesh(cell: &TerrainCellMesh, ids: &HashMap<u32, u16>) -> Result<Mesh, String> {
    let slots = expand_slots(cell, ids)?;
    let mut positions = Vec::with_capacity(CELL_MESH_VERTICES);
    let mut normals = Vec::with_capacity(CELL_MESH_VERTICES);
    let mut uvs = Vec::with_capacity(CELL_MESH_VERTICES);
    let mut colors = Vec::with_capacity(CELL_MESH_VERTICES);
    for vertex in 0..CELL_MESH_VERTICES {
        let [x, y] = vertex_xy(vertex);
        positions.push(creation_to_runtime_vector([x, y, cell.heights[vertex]]));
        normals.push(
            Vec3::from_array(creation_to_runtime_vector([
                f32::from(cell.normals[vertex * 3]),
                f32::from(cell.normals[vertex * 3 + 1]),
                f32::from(cell.normals[vertex * 3 + 2]),
            ]))
            .normalize_or(Vec3::Y)
            .to_array(),
        );
        // The same cell-wide texture coordinate today's quadrants use: grid position over 32.
        let grid_steps = (2 * (QUADRANT_SIDE - 1)) as f32;
        uvs.push([
            x / shared::terrain_mesh::TERRAIN_VERTEX_SPACING / grid_steps,
            y / shared::terrain_mesh::TERRAIN_VERTEX_SPACING / grid_steps,
        ]);
        colors.push(if cell.vertex_colors.is_empty() {
            [1.0; 4]
        } else {
            [
                f32::from(cell.vertex_colors[vertex * 3]) / 255.0,
                f32::from(cell.vertex_colors[vertex * 3 + 1]) / 255.0,
                f32::from(cell.vertex_colors[vertex * 3 + 2]) / 255.0,
                1.0,
            ]
        });
    }
    let indices: Vec<u32> = (0..4)
        .flat_map(quadrant_triangles)
        .flatten()
        .map(u32::from)
        .collect();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    for (group, values) in slots.ids.into_iter().enumerate() {
        mesh.insert_attribute(
            TERRAIN_SLOT_IDS[group],
            VertexAttributeValues::Uint16x4(values),
        );
    }
    for (group, values) in slots.weights.into_iter().enumerate() {
        mesh.insert_attribute(
            TERRAIN_SLOT_WEIGHTS[group],
            VertexAttributeValues::Unorm8x4(values),
        );
    }
    mesh.insert_indices(Indices::U32(indices));
    Ok(mesh)
}
