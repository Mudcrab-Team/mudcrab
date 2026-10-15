//! Builds the merged terrain mesh of one exterior cell from its LAND record (#215).
//!
//! The mesh is the in-memory `shared::terrain_mesh::TerrainCellMesh` (format v3): four unwelded
//! 17x17 quadrants, each with its own list of LTEX FormID slots and per-vertex weights. Writing
//! it to disk, the texture arrays and the pipeline wiring are separate steps.
//!
//! Nothing is dropped silently. A layer that cannot be used (a null or unknown LTEX, or no
//! opacity anywhere), a duplicate layer slot that the later record overrides, a quadrant over
//! today's 6-layer cap and a cell that needs 16 slots per quadrant are all listed in the
//! [`CellMeshReport`]. A quadrant that needs more than 16 slots is an error naming the cell.

use crate::esm::{
    cell_cache::{decode_normals, decode_vhgt, extract_texture_layers},
    extractors::SubrecordView,
    records::RawRecord,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use shared::{
    LAND_SIDE,
    terrain_mesh::{
        CELL_MESH_VERTICES, DEFAULT_LAND_TEXTURE_FORM_ID, MAX_TERRAIN_SLOTS, QUADRANT_SIDE,
        QUADRANT_VERTICES, TerrainCellMesh, grid_position, vertex_index,
    },
};
use std::collections::{HashMap, HashSet};

/// The slot stride of a cell whose quadrants all fit in 8 slots.
const NARROW_STRIDE: usize = 8;
/// Today's per-quadrant layer cap, which the merged mesh deliberately exceeds.
const TODAY_LAYER_CAP: usize = 6;

/// Why a LAND layer did not get a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The layer names no texture (LTEX FormID 0, which official plugins contain).
    NullTexture,
    /// The layer's LTEX is not among the known land textures.
    UnknownTexture,
    /// An ATXT layer whose VTXT gives no vertex any opacity: it would blend nothing.
    NoOpacity,
}

/// A BTXT or ATXT layer that was left out of the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedLayer {
    pub quadrant: u8,
    /// The ATXT layer index; 0 for a base.
    pub layer: u16,
    pub is_base: bool,
    pub texture_form_id: u32,
    pub reason: SkipReason,
}

/// An ATXT assignment replaced by a later record for the same quadrant and layer index (the
/// later one wins, as today).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverriddenLayer {
    pub quadrant: u8,
    pub layer: u16,
    pub texture_form_id: u32,
}

/// What the cell build changed or left out, for the converter report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellMeshReport {
    /// Quadrants with more than 6 slots: the deliberate change from today's 6-layer cap.
    pub quadrants_over_six_slots: Vec<u8>,
    /// Quadrants whose slot 0 is the default texture: no BTXT, a null BTXT texture, or a BTXT
    /// whose LTEX is unknown (the last also appears in `skipped_layers`).
    pub quadrants_with_default_texture: Vec<u8>,
    /// Layers left out, with the reason.
    pub skipped_layers: Vec<SkippedLayer>,
    /// ATXT entries replaced by a later entry for the same quadrant and layer index.
    pub overridden_layers: Vec<OverriddenLayer>,
    /// The cell has 16 slots per quadrant, because a quadrant needs more than 8.
    pub needed_stride_16: bool,
}

/// A quadrant's slot list and, per slot 1.., the sparse weights.
struct QuadrantSlots {
    textures: Vec<u32>,
    /// (slot, VTXT position, opacity) for slots 1 and up.
    weights: Vec<(usize, u16, f32)>,
}

/// Builds the merged mesh of `record` (a LAND record) for cell `cell_id`.
///
/// `known_ltex` holds the LTEX FormIDs (load-order numbering) that exist; a layer naming
/// anything else is skipped and reported. `key` is the derived-data key of the cell (#185),
/// stored as given. The mesh passes [`TerrainCellMesh::validate`] against its own slot
/// textures before it is returned.
pub fn build_cell_mesh(
    cell_id: u32,
    record: &RawRecord,
    known_ltex: &HashSet<u32>,
    key: [u8; 32],
) -> Result<(TerrainCellMesh, CellMeshReport)> {
    let side = usize::from(LAND_SIDE);
    let view = SubrecordView::new(&record.subrecords);

    let vhgt = view.find(b"VHGT").unwrap_or_default();
    if let Some(bytes) = vhgt.get(..4) {
        let offset = f32::from_le_bytes(bytes.try_into().expect("four-byte VHGT offset"));
        ensure!(
            offset.is_finite() && (offset * 8.0).is_finite(),
            "LAND of cell {cell_id:08X} has an invalid VHGT offset"
        );
    }
    let grid_heights = decode_vhgt(vhgt);
    ensure!(
        grid_heights.len() == side * side,
        "LAND of cell {cell_id:08X} has an incomplete VHGT height field"
    );
    ensure!(
        grid_heights.iter().all(|height| height.is_finite()),
        "LAND of cell {cell_id:08X} contains a non-finite VHGT height"
    );
    let grid_normals = decode_normals(view.find(b"VNML").unwrap_or_default(), &grid_heights);
    ensure!(
        grid_normals.len() == side * side * 3,
        "LAND of cell {cell_id:08X} has an incomplete VNML normal field"
    );
    let grid_colors = view.find(b"VCLR").unwrap_or_default();
    ensure!(
        grid_colors.is_empty() || grid_colors.len() == side * side * 3,
        "LAND of cell {cell_id:08X} has an incomplete VCLR field"
    );

    let mut heights = vec![0.0f32; CELL_MESH_VERTICES];
    let mut normals = vec![0i8; 3 * CELL_MESH_VERTICES];
    let mut vertex_colors = vec![
        0u8;
        if grid_colors.is_empty() {
            0
        } else {
            3 * CELL_MESH_VERTICES
        }
    ];
    for quadrant in 0..4 {
        for y in 0..QUADRANT_SIDE {
            for x in 0..QUADRANT_SIDE {
                let (gx, gy) = grid_position(quadrant, x, y);
                let source = gy * side + gx;
                let target = vertex_index(quadrant, x, y);
                heights[target] = grid_heights[source];
                normals[target * 3..target * 3 + 3]
                    .copy_from_slice(&grid_normals[source * 3..source * 3 + 3]);
                if !grid_colors.is_empty() {
                    vertex_colors[target * 3..target * 3 + 3]
                        .copy_from_slice(&grid_colors[source * 3..source * 3 + 3]);
                }
            }
        }
    }

    let (layers, duplicates) = extract_texture_layers(&record.subrecords)
        .wrap_err_with(|| format!("invalid LAND layers for cell {cell_id:08X}"))?;
    let mut report = CellMeshReport {
        overridden_layers: duplicates
            .iter()
            .map(|dropped| OverriddenLayer {
                quadrant: dropped.quadrant,
                layer: dropped.layer,
                texture_form_id: dropped.texture_form_id,
            })
            .collect(),
        ..CellMeshReport::default()
    };

    // `extract_texture_layers` orders layers by quadrant, base first, then layer index, then
    // texture FormID: the order the engine blends in today.
    let mut quadrants = Vec::with_capacity(4);
    for quadrant in 0u8..4 {
        let mut slots = QuadrantSlots {
            textures: Vec::new(),
            weights: Vec::new(),
        };
        let mut base = DEFAULT_LAND_TEXTURE_FORM_ID;
        for layer in layers
            .iter()
            .filter(|layer| layer.quadrant == quadrant && layer.is_base)
        {
            if layer.texture_form_id == 0 {
                report.skipped_layers.push(SkippedLayer {
                    quadrant,
                    layer: 0,
                    is_base: true,
                    texture_form_id: 0,
                    reason: SkipReason::NullTexture,
                });
            } else if known_ltex.contains(&layer.texture_form_id) {
                base = layer.texture_form_id;
            } else {
                report.skipped_layers.push(SkippedLayer {
                    quadrant,
                    layer: 0,
                    is_base: true,
                    texture_form_id: layer.texture_form_id,
                    reason: SkipReason::UnknownTexture,
                });
            }
        }
        if base == DEFAULT_LAND_TEXTURE_FORM_ID {
            report.quadrants_with_default_texture.push(quadrant);
        }
        slots.textures.push(base);
        for layer in layers
            .iter()
            .filter(|layer| layer.quadrant == quadrant && !layer.is_base)
        {
            let skip = |reason| SkippedLayer {
                quadrant,
                layer: layer.layer,
                is_base: false,
                texture_form_id: layer.texture_form_id,
                reason,
            };
            if layer.texture_form_id == 0 {
                report.skipped_layers.push(skip(SkipReason::NullTexture));
                continue;
            }
            if !known_ltex.contains(&layer.texture_form_id) {
                report.skipped_layers.push(skip(SkipReason::UnknownTexture));
                continue;
            }
            if !layer.weights.iter().any(|weight| weight.opacity > 0.0) {
                report.skipped_layers.push(skip(SkipReason::NoOpacity));
                continue;
            }
            let slot = slots.textures.len();
            slots.textures.push(layer.texture_form_id);
            slots.weights.extend(
                layer
                    .weights
                    .iter()
                    .map(|weight| (slot, weight.vertex, weight.opacity)),
            );
        }
        ensure!(
            slots.textures.len() <= MAX_TERRAIN_SLOTS,
            "LAND of cell {cell_id:08X} quadrant {quadrant} needs {} texture slots, more than the {MAX_TERRAIN_SLOTS} a cell mesh holds",
            slots.textures.len()
        );
        if slots.textures.len() > TODAY_LAYER_CAP {
            report.quadrants_over_six_slots.push(quadrant);
        }
        quadrants.push(slots);
    }

    let widest = quadrants
        .iter()
        .map(|quadrant| quadrant.textures.len())
        .max()
        .unwrap_or(1);
    let stride = if widest > NARROW_STRIDE {
        MAX_TERRAIN_SLOTS
    } else {
        NARROW_STRIDE
    };
    report.needed_stride_16 = stride > NARROW_STRIDE;

    let mut quadrant_slots = vec![0u32; 4 * stride];
    let mut quadrant_slot_counts = [0u8; 4];
    let mut weights = vec![0u8; stride * CELL_MESH_VERTICES];
    for (quadrant, slots) in quadrants.iter().enumerate() {
        quadrant_slot_counts[quadrant] = slots.textures.len() as u8;
        quadrant_slots[quadrant * stride..quadrant * stride + slots.textures.len()]
            .copy_from_slice(&slots.textures);
        for &(slot, position, opacity) in &slots.weights {
            let position = usize::from(position);
            ensure!(
                position < QUADRANT_VERTICES,
                "LAND of cell {cell_id:08X} has a VTXT vertex outside its quadrant"
            );
            let vertex = quadrant * QUADRANT_VERTICES + position;
            weights[vertex * stride + slot] = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }

    let mesh = TerrainCellMesh {
        cell_id,
        key,
        heights,
        normals,
        vertex_colors,
        slot_stride: stride as u8,
        quadrant_slots,
        quadrant_slot_counts,
        weights,
    };
    // Every slot texture is known by construction; the map only lets the shared check run.
    let ids: HashMap<u32, u16> = mesh
        .quadrant_slots
        .iter()
        .map(|&ltex| (ltex, 0))
        .chain(std::iter::once((DEFAULT_LAND_TEXTURE_FORM_ID, 0)))
        .collect();
    mesh.validate(&ids)
        .map_err(|error| color_eyre::eyre::eyre!("terrain mesh of cell {cell_id:08X}: {error}"))?;
    Ok((mesh, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm::cell_cache::{decode_normals, decode_vhgt};

    const CELL: u32 = 0x0000_1234;

    /// One ATXT layer to put in a synthetic LAND: (quadrant, layer index, LTEX, VTXT entries
    /// of (position, opacity)).
    type Atxt = (u8, u16, u32, Vec<(u16, f32)>);

    fn layer_bytes(ltex: u32, quadrant: u8, layer: u16) -> Vec<u8> {
        let mut data = ltex.to_le_bytes().to_vec();
        data.extend([quadrant, 0]);
        data.extend(layer.to_le_bytes());
        data
    }

    /// A LAND record with deltas that make the height `16 * (y + 1) + 8 * x` at grid (x, y),
    /// normals `[x + 1, y + 1, 100]` and colours `[7x, 7y, 9]`.
    fn land(bases: &[(u8, u32)], atxt: &[Atxt], with_colors: bool) -> RawRecord {
        let side = usize::from(LAND_SIDE);
        let mut vhgt = 0.0f32.to_le_bytes().to_vec();
        for _row in 0..side {
            vhgt.push(2); // row origin step: +16 per row
            vhgt.extend(std::iter::repeat_n(1u8, side - 1)); // +8 per column
        }
        vhgt.extend([0; 3]);
        let mut vnml = Vec::new();
        let mut vclr = Vec::new();
        for y in 0..side {
            for x in 0..side {
                vnml.extend([(x + 1) as u8, (y + 1) as u8, 100]);
                vclr.extend([(7 * x) as u8, (7 * y) as u8, 9]);
            }
        }
        let mut subrecords = vec![(b"VHGT".to_vec(), vhgt), (b"VNML".to_vec(), vnml)];
        if with_colors {
            subrecords.push((b"VCLR".to_vec(), vclr));
        }
        for &(quadrant, ltex) in bases {
            subrecords.push((b"BTXT".to_vec(), layer_bytes(ltex, quadrant, 0)));
        }
        for (quadrant, layer, ltex, entries) in atxt {
            subrecords.push((b"ATXT".to_vec(), layer_bytes(*ltex, *quadrant, *layer)));
            let mut vtxt = Vec::new();
            for (position, opacity) in entries {
                vtxt.extend(position.to_le_bytes());
                vtxt.extend([0, 0]);
                vtxt.extend(opacity.to_le_bytes());
            }
            subrecords.push((b"VTXT".to_vec(), vtxt));
        }
        RawRecord {
            form_id: 0x0000_5678,
            record_type: *b"LAND",
            flags: 0,
            subrecords,
            cell_form_id: Some(CELL),
            worldspace_form_id: None,
            load_order: 0,
        }
    }

    fn known(ids: impl IntoIterator<Item = u32>) -> HashSet<u32> {
        ids.into_iter().collect()
    }

    fn all_bases() -> Vec<(u8, u32)> {
        (0..4).map(|q| (q, 0x100 + u32::from(q))).collect()
    }

    fn known_all() -> HashSet<u32> {
        known(0x100..0x200)
    }

    /// `count` ATXT layers in quadrant 0 with distinct layer indices, each at vertex 0.
    fn many_layers(count: u16) -> Vec<Atxt> {
        (0..count)
            .map(|layer| (0, layer, 0x110 + u32::from(layer), vec![(0u16, 1.0f32)]))
            .collect()
    }

    #[test]
    fn heights_normals_and_colours_land_at_the_right_vertices() {
        let record = land(&all_bases(), &[], true);
        let (mesh, report) = build_cell_mesh(CELL, &record, &known_all(), [7; 32]).unwrap();
        assert_eq!(mesh.cell_id, CELL);
        assert_eq!(mesh.key, [7; 32]);
        assert_eq!(mesh.slot_stride, 8);
        assert!(!report.needed_stride_16);
        for quadrant in 0..4 {
            for (x, y) in [(0, 0), (16, 0), (0, 16), (16, 16), (5, 11)] {
                let (gx, gy) = grid_position(quadrant, x, y);
                let index = vertex_index(quadrant, x, y);
                assert_eq!(mesh.heights[index], (16 * (gy + 1) + 8 * gx) as f32);
                assert_eq!(
                    mesh.normals[index * 3..index * 3 + 3],
                    [(gx + 1) as i8, (gy + 1) as i8, 100]
                );
                assert_eq!(
                    mesh.vertex_colors[index * 3..index * 3 + 3],
                    [(7 * gx) as u8, (7 * gy) as u8, 9]
                );
            }
        }
        // The inner seams are duplicated: grid (16, y) is in quadrants 0 and 1, (x, 16) in 0
        // and 2, and the centre in all four.
        for y in 0..QUADRANT_SIDE {
            assert_eq!(
                mesh.heights[vertex_index(0, 16, y)],
                mesh.heights[vertex_index(1, 0, y)]
            );
        }
        for x in 0..QUADRANT_SIDE {
            assert_eq!(
                mesh.heights[vertex_index(0, x, 16)],
                mesh.heights[vertex_index(2, x, 0)]
            );
        }
        let centre = mesh.heights[vertex_index(0, 16, 16)];
        assert_eq!(centre, mesh.heights[vertex_index(1, 0, 16)]);
        assert_eq!(centre, mesh.heights[vertex_index(2, 16, 0)]);
        assert_eq!(centre, mesh.heights[vertex_index(3, 0, 0)]);
    }

    #[test]
    fn missing_vclr_stays_empty() {
        let record = land(&all_bases(), &[], false);
        let (mesh, _) = build_cell_mesh(CELL, &record, &known_all(), [0; 32]).unwrap();
        assert!(mesh.vertex_colors.is_empty());
    }

    #[test]
    fn slots_follow_layer_order_with_the_right_weights() {
        let atxt: Vec<Atxt> = vec![
            (2, 5, 0x103, vec![(20, 0.2), (21, 1.0)]),
            (2, 1, 0x101, vec![(20, 0.5)]),
            (2, 3, 0x104, vec![(20, 0.25), (22, 0.0)]),
        ];
        let record = land(&all_bases(), &atxt, false);
        let (mesh, report) = build_cell_mesh(CELL, &record, &known_all(), [0; 32]).unwrap();
        assert_eq!(mesh.slot_stride, 8);
        assert_eq!(mesh.quadrant_slot_counts, [1, 1, 4, 1]);
        // 0x102 is quadrant 2's base (BTXT 0x100 + 2), then layers 1, 3, 5 follow.
        assert_eq!(&mesh.quadrant_slots[16..20], &[0x102, 0x101, 0x104, 0x103]);
        assert!(mesh.quadrant_slots[20..24].iter().all(|&slot| slot == 0));
        let weight = |position: usize, slot: usize| {
            mesh.weights[(2 * QUADRANT_VERTICES + position) * 8 + slot]
        };
        assert_eq!(weight(20, 0), 0, "slot 0 is implied");
        assert_eq!(weight(20, 1), 128, "0.5 rounds to 128");
        assert_eq!(weight(20, 2), 64, "0.25 rounds to 64");
        assert_eq!(weight(20, 3), 51, "0.2 rounds to 51");
        assert_eq!(weight(21, 3), 255);
        assert_eq!(weight(21, 1), 0);
        assert_eq!(weight(22, 1), 0);
        assert!(report.skipped_layers.is_empty());
        assert!(report.quadrants_over_six_slots.is_empty());
    }

    #[test]
    fn seven_alpha_layers_fit_stride_8_and_eight_need_16() {
        let (mesh, report) = build_cell_mesh(
            CELL,
            &land(&all_bases(), &many_layers(7), false),
            &known_all(),
            [0; 32],
        )
        .unwrap();
        assert_eq!(mesh.slot_stride, 8);
        assert_eq!(mesh.quadrant_slot_counts[0], 8);
        assert!(!report.needed_stride_16);
        assert_eq!(report.quadrants_over_six_slots, vec![0]);

        let (mesh, report) = build_cell_mesh(
            CELL,
            &land(&all_bases(), &many_layers(8), false),
            &known_all(),
            [0; 32],
        )
        .unwrap();
        assert_eq!(mesh.slot_stride, 16);
        assert_eq!(mesh.quadrant_slot_counts, [9, 1, 1, 1]);
        assert_eq!(mesh.quadrant_slots.len(), 64);
        assert_eq!(mesh.weights.len(), 16 * CELL_MESH_VERTICES);
        assert!(report.needed_stride_16);
        assert_eq!(
            mesh.quadrant_slots[16], 0x101,
            "quadrant 1 starts at stride 16"
        );
        // Layer 7 is slot 8 and carries its weight at vertex 0.
        assert_eq!(mesh.weights[8], 255);

        let (mesh, _) = build_cell_mesh(
            CELL,
            &land(&all_bases(), &many_layers(15), false),
            &known_all(),
            [0; 32],
        )
        .unwrap();
        assert_eq!(mesh.quadrant_slot_counts[0], 16);
    }

    #[test]
    fn sixteen_alpha_layers_are_an_error_naming_the_cell() {
        let error = build_cell_mesh(
            CELL,
            &land(&all_bases(), &many_layers(16), false),
            &known_all(),
            [0; 32],
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("00001234"), "{message}");
        assert!(message.contains("quadrant 0"), "{message}");
    }

    #[test]
    fn a_quadrant_without_btxt_gets_the_default_texture_and_is_reported() {
        let bases = [(0u8, 0x100u32), (1, 0x101), (3, 0x103)];
        let atxt: Vec<Atxt> = vec![(2, 0, 0x110, vec![(0, 1.0)])];
        let (mesh, report) =
            build_cell_mesh(CELL, &land(&bases, &atxt, false), &known_all(), [0; 32]).unwrap();
        assert_eq!(mesh.quadrant_slots[2 * 8], DEFAULT_LAND_TEXTURE_FORM_ID);
        assert_eq!(mesh.quadrant_slots[2 * 8 + 1], 0x110);
        assert_eq!(mesh.quadrant_slot_counts, [1, 1, 2, 1]);
        assert_eq!(report.quadrants_with_default_texture, vec![2]);

        // A cell with no layers at all uses the default everywhere.
        let (mesh, report) =
            build_cell_mesh(CELL, &land(&[], &[], false), &known_all(), [0; 32]).unwrap();
        assert_eq!(mesh.quadrant_slot_counts, [1; 4]);
        assert_eq!(report.quadrants_with_default_texture, vec![0, 1, 2, 3]);
    }

    #[test]
    fn unknown_null_and_empty_layers_are_skipped_and_reported() {
        let bases = [(0u8, 0x100u32), (1, 0x999), (2, 0), (3, 0x103)];
        let atxt: Vec<Atxt> = vec![
            (0, 1, 0x888, vec![(0, 1.0)]),
            (0, 2, 0, vec![(0, 1.0)]),
            (0, 3, 0x111, vec![(0, 0.0)]),
            (0, 4, 0x112, vec![(0, 0.5)]),
        ];
        let (mesh, report) =
            build_cell_mesh(CELL, &land(&bases, &atxt, false), &known_all(), [0; 32]).unwrap();
        assert_eq!(mesh.quadrant_slot_counts, [2, 1, 1, 1]);
        assert_eq!(&mesh.quadrant_slots[..2], &[0x100, 0x112]);
        assert_eq!(report.quadrants_with_default_texture, vec![1, 2]);
        let reasons: Vec<_> = report
            .skipped_layers
            .iter()
            .map(|skipped| (skipped.quadrant, skipped.is_base, skipped.reason))
            .collect();
        assert!(reasons.contains(&(1, true, SkipReason::UnknownTexture)));
        assert!(reasons.contains(&(2, true, SkipReason::NullTexture)));
        assert!(reasons.contains(&(0, false, SkipReason::UnknownTexture)));
        assert!(reasons.contains(&(0, false, SkipReason::NullTexture)));
        assert!(reasons.contains(&(0, false, SkipReason::NoOpacity)));
        assert_eq!(report.skipped_layers.len(), 5);
    }

    #[test]
    fn a_later_atxt_for_the_same_slot_wins_and_is_reported() {
        let atxt: Vec<Atxt> = vec![(0, 1, 0x110, vec![(0, 1.0)]), (0, 1, 0x111, vec![(1, 1.0)])];
        let (mesh, report) = build_cell_mesh(
            CELL,
            &land(&all_bases(), &atxt, false),
            &known_all(),
            [0; 32],
        )
        .unwrap();
        assert_eq!(&mesh.quadrant_slots[..2], &[0x100, 0x111]);
        assert_eq!(
            mesh.weights[1], 0,
            "the overridden layer's weight at vertex 0 is gone"
        );
        assert_eq!(mesh.weights[8 + 1], 255);
        assert_eq!(
            report.overridden_layers,
            vec![OverriddenLayer {
                quadrant: 0,
                layer: 1,
                texture_form_id: 0x110
            }]
        );
    }

    /// The new mesh carries what today's quadrant builder reads: for quadrant q the vertex
    /// (x, y) takes the cell-grid sample (16 * (q % 2) + x, 16 * (q / 2) + y) of the cached
    /// land's heights, normals and colours (`build_terrain_quadrant_mesh`, streaming.rs).
    #[test]
    fn quadrants_match_what_the_runtime_quadrant_builder_reads() {
        let record = land(&all_bases(), &[], true);
        let view = SubrecordView::new(&record.subrecords);
        let heights = decode_vhgt(view.find(b"VHGT").unwrap());
        let normals = decode_normals(view.find(b"VNML").unwrap(), &heights);
        let colors = view.find(b"VCLR").unwrap();
        let (mesh, _) = build_cell_mesh(CELL, &record, &known_all(), [0; 32]).unwrap();
        let width = usize::from(LAND_SIDE);
        for quadrant in 0..4usize {
            let origin_x = (quadrant % 2) * 16;
            let origin_y = (quadrant / 2) * 16;
            for local_y in 0..17 {
                for local_x in 0..17 {
                    let index = (origin_y + local_y) * width + origin_x + local_x;
                    let target = quadrant * QUADRANT_VERTICES + local_y * 17 + local_x;
                    assert_eq!(mesh.heights[target], heights[index]);
                    assert_eq!(
                        mesh.normals[target * 3..target * 3 + 3],
                        normals[index * 3..index * 3 + 3]
                    );
                    assert_eq!(
                        mesh.vertex_colors[target * 3..target * 3 + 3],
                        colors[index * 3..index * 3 + 3]
                    );
                }
            }
        }
    }

    #[test]
    fn missing_vnml_is_rebuilt_like_today_and_bad_geometry_is_an_error() {
        let mut record = land(&all_bases(), &[], false);
        record.subrecords.retain(|(tag, _)| tag != b"VNML");
        let (mesh, _) = build_cell_mesh(CELL, &record, &known_all(), [0; 32]).unwrap();
        let heights = decode_vhgt(
            SubrecordView::new(&record.subrecords)
                .find(b"VHGT")
                .unwrap(),
        );
        let normals = decode_normals(&[], &heights);
        let side = usize::from(LAND_SIDE);
        let target = vertex_index(3, 4, 6);
        let source = (16 + 6) * side + 16 + 4;
        assert_eq!(
            mesh.normals[target * 3..target * 3 + 3],
            normals[source * 3..source * 3 + 3]
        );

        let mut truncated = land(&all_bases(), &[], false);
        for (tag, data) in &mut truncated.subrecords {
            if tag == b"VHGT" {
                data.truncate(16);
            }
        }
        let error = build_cell_mesh(CELL, &truncated, &known_all(), [0; 32]).unwrap_err();
        assert!(format!("{error:#}").contains("00001234"));
    }
}
