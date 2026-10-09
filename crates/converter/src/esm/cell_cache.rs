use crate::esm::{extractors::SubrecordView, records::RawRecord};
use color_eyre::{Result, eyre::WrapErr};
use memmap2::Mmap;
use rkyv::rancor::Error;
use shared::{CELL_CACHE_VERSION, CachedLand, CellCache, LAND_SIDE, TerrainLayer, TerrainWeight};
use std::{collections::HashMap, fs::File, io::Write, path::Path};

/// Check one decoded terrain record before the tolerant reader publishes it.
/// Errors stay local to this record; the adapter counts and omits it.
/// Texture/color-only LAND may omit VHGT and uses the shared cache's default heights.
pub(crate) fn validate_inhouse_land(record: &RawRecord) -> Result<()> {
    let view = SubrecordView::new(&record.subrecords);
    let authored_heightmap = view.find(b"VHGT");
    let heightmap = authored_heightmap.unwrap_or_default();
    let heights = decode_vhgt(heightmap);
    let count = usize::from(LAND_SIDE).pow(2);
    color_eyre::eyre::ensure!(
        authored_heightmap.is_none()
            || (!heightmap.is_empty()
                && heights.len() == count
                && heights.iter().all(|h| h.is_finite())),
        "incomplete or non-finite authored VHGT"
    );
    let colors = view.find(b"VCLR").unwrap_or_default();
    color_eyre::eyre::ensure!(
        colors.is_empty() || colors.len() == count * 3,
        "incomplete VCLR"
    );
    let (mut layers, _) = extract_texture_layers(&record.subrecords)?;
    normalize_texture_layers(&mut layers);
    for quadrant in 0..4 {
        let entries: Vec<_> = layers
            .iter()
            .filter(|layer| layer.quadrant == quadrant)
            .collect();
        color_eyre::eyre::ensure!(
            entries.is_empty()
                || (entries.len() <= 6
                    && entries.iter().filter(|layer| layer.is_base).count() == 1),
            "invalid terrain layer count"
        );
    }
    Ok(())
}

/// Write merged terrain deterministically when native source positions are unavailable.
/// Later winning load order takes precedence; same-plugin ties use FormID.
/// In-house publication supplies native positions through `write_cell_cache_with_source_order`.
pub fn write_cell_cache(records: &HashMap<u32, RawRecord>, path: &Path) -> Result<usize> {
    write_cell_cache_with_source_order(records, &HashMap::new(), path)
}

/// Rank a winning LAND by plugin priority, native position, and a deterministic fallback.
fn land_source_priority(record: &RawRecord, source_offsets: &HashMap<u32, u64>) -> (u32, u64, u32) {
    (
        record.load_order,
        source_offsets.get(&record.form_id).copied().unwrap_or(0),
        record.form_id,
    )
}

/// Select the complete LAND projection for each parent CELL, excluding tombstones.
/// SQLite terrain rows and the archived cache must use this same selection rule.
/// Native offsets belong to each accepted winner's supplying plugin, not its ID owner.
pub(crate) fn land_winners_by_cell(
    records: &HashMap<u32, RawRecord>,
    source_offsets: &HashMap<u32, u64>,
) -> HashMap<u32, u32> {
    let mut winners = HashMap::new();
    for record in records
        .values()
        .filter(|record| &record.record_type == b"LAND" && !record.is_deleted())
    {
        let cell_id = record.cell_form_id.unwrap_or(record.form_id);
        let replaces = winners.get(&cell_id).is_none_or(|selected| {
            land_source_priority(record, source_offsets)
                > land_source_priority(&records[selected], source_offsets)
        });
        if replaces {
            winners.insert(cell_id, record.form_id);
        }
    }
    winners
}

/// Validate and serialize terrain using winning plugin priority and native record order.
/// Different LAND identities may share a parent CELL. The later winning plugin, then
/// the later physical record in that plugin, supplies the complete cached terrain.
/// FormID breaks ties only when source positions are missing or identical; it does
/// not give full or light plugin identities precedence over their load order.
/// All candidates retain validation before any previous cache is replaced.
pub fn write_cell_cache_with_source_order(
    records: &HashMap<u32, RawRecord>,
    source_offsets: &HashMap<u32, u64>,
    path: &Path,
) -> Result<usize> {
    write_cell_cache_with_preserved_layers(records, source_offsets, &HashMap::new(), path)
}

/// Capture decoded global texture identities before missing-target clearing.
/// Resolve native duplicate slots and NULL assignments once, while their distinction
/// remains available. The caller may then clear unusable texture IDs without losing
/// the retained assignment's weights.
pub(crate) fn preserved_texture_layers(record: &RawRecord) -> Result<Vec<TerrainLayer>> {
    let (mut layers, _) = extract_texture_layers(&record.subrecords)?;
    normalize_texture_layers(&mut layers);
    validate_preserved_layers(&layers)?;
    Ok(layers)
}

/// Publish source-normalized layers for selected LAND identities, including cleared
/// zero-ID placeholders. Overrides must come from accepted decoded source records;
/// they are not normalized again. Malformed auxiliary layers fall back locally to
/// the ordinary record layers and cannot discard valid neighboring terrain.
pub(crate) fn write_cell_cache_with_preserved_layers(
    records: &HashMap<u32, RawRecord>,
    source_offsets: &HashMap<u32, u64>,
    layer_overrides: &HashMap<u32, Vec<TerrainLayer>>,
    path: &Path,
) -> Result<usize> {
    let water_by_cell = water_by_cell(records);
    let winners = land_winners_by_cell(records, source_offsets);
    let mut rejected_overrides: HashMap<u32, (usize, String)> = HashMap::new();
    let mut cells_by_id = HashMap::new();
    let mut land_records: Vec<_> = records
        .values()
        .filter(|record| &record.record_type == b"LAND" && !record.is_deleted())
        .collect();
    land_records.sort_unstable_by_key(|record| land_source_priority(record, source_offsets));
    for record in land_records {
        let view = SubrecordView::new(&record.subrecords);
        let heightmap = view.find(b"VHGT").unwrap_or_default();
        let cell_id = record.cell_form_id.unwrap_or(record.form_id);
        let (water_height, water_type_form_id) =
            water_by_cell.get(&cell_id).copied().unwrap_or((None, None));
        if let Some(bytes) = heightmap.get(..4) {
            let offset = f32::from_le_bytes(bytes.try_into().expect("four-byte VHGT offset"));
            color_eyre::eyre::ensure!(
                offset.is_finite(),
                "LAND {cell_id:08X} contains a non-finite VHGT offset"
            );
            color_eyre::eyre::ensure!(
                (offset * 8.0).is_finite(),
                "LAND {cell_id:08X} VHGT offset is out of range"
            );
        }
        let heights = decode_vhgt(heightmap);
        color_eyre::eyre::ensure!(
            heights.iter().all(|height| height.is_finite()),
            "LAND {cell_id:08X} contains a non-finite VHGT height"
        );
        let normals = decode_normals(view.find(b"VNML").unwrap_or_default(), &heights);
        let vertex_colors = view.find(b"VCLR").unwrap_or_default().to_vec();
        let (mut layers, dropped_layers) = extract_texture_layers(&record.subrecords)
            .wrap_err_with(|| format!("invalid LAND layers for cell {cell_id:08X}"))?;
        for DroppedLayer {
            quadrant: q,
            layer: s,
            texture_form_id: id,
        } in dropped_layers
        {
            eprintln!(
                "warning: LAND {cell_id:08X} quadrant {q} ATXT layer slot {s}: dropped texture FormID {id:08X}; later entry kept"
            );
        }
        normalize_texture_layers(&mut layers);
        let vertex_count = usize::from(LAND_SIDE) * usize::from(LAND_SIDE);
        color_eyre::eyre::ensure!(
            heights.len() == vertex_count,
            "LAND {cell_id:08X} has an incomplete VHGT height field"
        );
        color_eyre::eyre::ensure!(
            normals.len() == vertex_count * 3,
            "LAND {cell_id:08X} has an incomplete VNML normal field"
        );
        color_eyre::eyre::ensure!(
            vertex_colors.is_empty() || vertex_colors.len() == vertex_count * 3,
            "LAND {cell_id:08X} has an incomplete VCLR field"
        );
        for quadrant in 0..4 {
            let quadrant_layers: Vec<_> = layers
                .iter()
                .filter(|layer| layer.quadrant == quadrant)
                .collect();
            let bases = quadrant_layers.iter().filter(|layer| layer.is_base).count();
            color_eyre::eyre::ensure!(
                quadrant_layers.is_empty() || (bases == 1 && quadrant_layers.len() <= 6),
                "LAND {cell_id:08X} quadrant {quadrant} must be empty or have one BTXT and at most five ATXT layers"
            );
        }
        if winners[&cell_id] != record.form_id {
            continue;
        }
        if let Some(preserved) = layer_overrides.get(&record.form_id) {
            match validate_preserved_layers(preserved) {
                Ok(()) => layers = copy_texture_layers(preserved),
                Err(error) => {
                    let (count, _) = rejected_overrides
                        .entry(record.load_order)
                        .or_insert_with(|| (0, format!("LAND {:08X}: {error}", record.form_id)));
                    *count += 1;
                }
            }
        }
        cells_by_id.insert(
            cell_id,
            CachedLand {
                cell_id,
                width: LAND_SIDE,
                height: LAND_SIDE,
                heights,
                normals,
                vertex_colors,
                layers,
                water_height,
                water_type_form_id,
            },
        );
    }
    let mut rejected_overrides: Vec<_> = rejected_overrides.into_iter().collect();
    rejected_overrides.sort_unstable_by_key(|(load_order, _)| *load_order);
    for (load_order, (count, first)) in rejected_overrides {
        eprintln!(
            "warning: terrain source priority {load_order}: ignored {count} invalid preserved layer override(s); first {first}; ordinary decoded layers retained"
        );
    }
    let mut cells: Vec<_> = cells_by_id.into_values().collect();
    cells.sort_unstable_by_key(|cell| cell.cell_id);
    let count = cells.len();
    let bytes = rkyv::to_bytes::<Error>(&CellCache {
        version: CELL_CACHE_VERSION,
        cells,
    })
    .wrap_err("failed to serialize cell cache")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Staged outputs may share an inode with a previous pack via
    // hard link; replace the path instead of writing through it.
    if path.is_file() {
        std::fs::remove_file(path)?;
    }
    let mut file = File::create(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    validate_cell_cache(path)?;
    Ok(count)
}

/// Each cell's water height and water type, as the game resolves them.
///
/// A cell's `XCLW` is its water height, and `XCWT` its water type. Most exterior cells don't
/// state a height: they carry `XCLW` = FLT_MAX (or none), meaning "the worldspace's default".
/// That default is the second float of the worldspace's `WRLD/DNAM` (Tamriel: -14000, the sea
/// and the marshes around Morthal), with `WRLD/NAM2` as its water type. Without it every such
/// cell had no water: 10,630 of Tamriel's 11,187 exterior cells, including the open sea. A cell
/// whose terrain stays above the default draws no surface anyway (the engine only spawns water
/// below it).
fn water_by_cell(records: &HashMap<u32, RawRecord>) -> HashMap<u32, (Option<f32>, Option<u32>)> {
    let defaults: HashMap<u32, (Option<f32>, Option<u32>)> = records
        .values()
        .filter(|record| &record.record_type == b"WRLD")
        .map(|record| {
            let view = SubrecordView::new(&record.subrecords);
            let height = view
                .find(b"DNAM")
                .filter(|bytes| bytes.len() >= 8)
                .map(|bytes| {
                    f32::from_le_bytes(bytes[4..8].try_into().expect("four-byte water height"))
                })
                .and_then(normalize_default_water_height);
            (record.form_id, (height, view.get_form_id(b"NAM2")))
        })
        .collect();
    records
        .values()
        .filter(|record| &record.record_type == b"CELL")
        .map(|record| {
            let view = SubrecordView::new(&record.subrecords);
            let own = view
                .find(b"XCLW")
                .filter(|bytes| bytes.len() >= 4)
                .map(|bytes| {
                    f32::from_le_bytes(bytes[..4].try_into().expect("four-byte water height"))
                })
                .and_then(normalize_water_height);
            let default = record
                .worldspace_form_id
                .and_then(|worldspace| defaults.get(&worldspace).copied())
                .unwrap_or((None, None));
            let height = own.or(default.0);
            let water_type = view.get_form_id(b"XCWT").or(default.1);
            (record.form_id, (height, water_type))
        })
        .collect()
}

/// A worldspace's default water height, or `None` for the markers some worldspaces use to
/// mean "no water" (`MossMotherCavernWorld` 9,999,999, `DeepwoodRedoubtWorld` -500,000). No
/// real Skyrim water lies further than 100,000 units from zero. The bound is tighter than
/// `normalize_water_height`'s 1e7, which only has to reject a cell's `FLT_MAX` sentinel and would let
/// `MossMotherCavernWorld`'s 9,999,999 through.
fn normalize_default_water_height(height: f32) -> Option<f32> {
    (height.is_finite() && height.abs() < 1.0e5).then_some(height)
}

fn normalize_water_height(height: f32) -> Option<f32> {
    // Skyrim uses FLT_MAX as the exterior-cell "no water" sentinel. Persisting
    // it as a real height creates non-finite render transforms downstream.
    (height.is_finite() && height.abs() < 1.0e7).then_some(height)
}

fn decode_vhgt(bytes: &[u8]) -> Vec<f32> {
    let count = usize::from(LAND_SIDE) * usize::from(LAND_SIDE);
    if bytes.is_empty() {
        return vec![0.0; count];
    }
    if bytes.len() < 4 + count {
        return Vec::new();
    }
    let offset = f32::from_le_bytes(bytes[..4].try_into().expect("four-byte VHGT offset")) * 8.0;
    let deltas = &bytes[4..4 + count];
    let side = usize::from(LAND_SIDE);
    let mut heights = vec![0.0; count];
    let mut row_origin = offset;
    for row in 0..side {
        row_origin += (deltas[row * side] as i8 as f32) * 8.0;
        let mut height = row_origin;
        heights[row * side] = height;
        for column in 1..side {
            height += (deltas[row * side + column] as i8 as f32) * 8.0;
            heights[row * side + column] = height;
        }
    }
    heights
}

/// Preserve valid authored VNML samples; reconstruct missing or zero vectors from terrain heights.
fn decode_normals(bytes: &[u8], heights: &[f32]) -> Vec<i8> {
    let side = usize::from(LAND_SIDE);
    let count = side * side;
    if bytes.len() == count * 3
        && bytes
            .as_chunks::<3>()
            .0
            .iter()
            .all(|normal| normal != &[0, 0, 0])
    {
        return bytes.iter().map(|value| *value as i8).collect();
    }
    if (!bytes.is_empty() && bytes.len() != count * 3) || heights.len() != count {
        return Vec::new();
    }
    let mut normals = Vec::with_capacity(count * 3);
    for y in 0..side {
        for x in 0..side {
            let west = x.saturating_sub(1);
            let east = (x + 1).min(side - 1);
            let south = y.saturating_sub(1);
            let north = (y + 1).min(side - 1);
            // At a border the stencil spans one 128-unit interval, not two.
            // Divide each axis by its actual distance so a planar slope keeps
            // the same normal at edges/corners. f64 avoids squared overflow.
            let dx = (f64::from(heights[y * side + east]) - f64::from(heights[y * side + west]))
                / ((east - west) as f64 * 128.0);
            let dy = (f64::from(heights[north * side + x]) - f64::from(heights[south * side + x]))
                / ((north - south) as f64 * 128.0);
            let normal = [-dx, -dy, 1.0];
            let length =
                (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
            normals.extend([
                (normal[0] / length * 127.0).round() as i8,
                (normal[1] / length * 127.0).round() as i8,
                (normal[2] / length * 127.0).round() as i8,
            ]);
        }
    }
    // Some shipped LAND records contain zero vectors. Reconstruct only those
    // invalid vertices from the height field; preserve every valid authored byte.
    for (normal, authored) in normals
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(bytes.as_chunks::<3>().0)
    {
        if authored != &[0, 0, 0] {
            *normal = authored.map(|value| value as i8);
        }
    }
    normals
}

/// An earlier non-null ATXT assignment discarded because a later entry uses the same slot.
#[derive(Debug, PartialEq, Eq)]
struct DroppedLayer {
    quadrant: u8,
    layer: u16,
    texture_form_id: u32,
}

/// Validate every LAND payload and return retained layers plus discarded duplicate assignments.
fn extract_texture_layers(
    subrecords: &[(Vec<u8>, Vec<u8>)],
) -> Result<(Vec<TerrainLayer>, Vec<DroppedLayer>)> {
    let mut layers = Vec::new();
    let mut active: Option<usize> = None;
    for (tag, data) in subrecords {
        match tag.as_slice() {
            b"BTXT" | b"ATXT" => {
                color_eyre::eyre::ensure!(
                    data.len() >= 8,
                    "{} subrecord is truncated",
                    String::from_utf8_lossy(tag)
                );
                let texture_form_id = u32::from_le_bytes(data[..4].try_into().unwrap());
                let quadrant = data[4];
                color_eyre::eyre::ensure!(
                    quadrant < 4,
                    "terrain quadrant {quadrant} is outside 0..=3"
                );
                let is_base = tag.as_slice() == b"BTXT";
                let layer = if is_base {
                    0
                } else {
                    u16::from_le_bytes(data[6..8].try_into().unwrap())
                };
                layers.push(TerrainLayer {
                    texture_form_id,
                    quadrant,
                    layer,
                    is_base,
                    weights: Vec::new(),
                });
                active = (!is_base).then_some(layers.len() - 1);
            }
            b"VTXT" => {
                color_eyre::eyre::ensure!(data.len() % 8 == 0, "VTXT payload is truncated");
                let index =
                    active.ok_or_else(|| color_eyre::eyre::eyre!("VTXT has no preceding ATXT"))?;
                for entry in data.as_chunks::<8>().0 {
                    let vertex = u16::from_le_bytes(entry[..2].try_into().unwrap());
                    let opacity = f32::from_le_bytes(entry[4..8].try_into().unwrap());
                    color_eyre::eyre::ensure!(
                        vertex < 17 * 17,
                        "VTXT vertex {vertex} is outside a LAND quadrant"
                    );
                    color_eyre::eyre::ensure!(
                        opacity.is_finite() && (0.0..=1.0).contains(&opacity),
                        "VTXT opacity {opacity} is invalid"
                    );
                    layers[index]
                        .weights
                        .push(TerrainWeight { vertex, opacity });
                }
            }
            _ => active = None,
        }
    }
    for layer in layers.iter().filter(|layer| !layer.is_base) {
        let mut vertices = std::collections::HashSet::new();
        color_eyre::eyre::ensure!(
            layer
                .weights
                .iter()
                .all(|weight| vertices.insert(weight.vertex)),
            "quadrant {} layer {} repeats a VTXT vertex",
            layer.quadrant,
            layer.layer
        );
    }
    // Validate even discarded payloads above. Resolve slots in reverse record order before
    // sorting, so the later non-null assignment (including its weights) always wins.
    let mut slots = std::collections::HashSet::new();
    let mut dropped_layers = Vec::new();
    layers.reverse();
    layers.retain(|layer| {
        if layer.is_base
            || layer.texture_form_id == 0
            || slots.insert((layer.quadrant, layer.layer))
        {
            return true;
        }
        dropped_layers.push(DroppedLayer {
            quadrant: layer.quadrant,
            layer: layer.layer,
            texture_form_id: layer.texture_form_id,
        });
        false
    });
    layers.reverse();
    layers.sort_by_key(|layer| {
        (
            layer.quadrant,
            !layer.is_base,
            layer.layer,
            layer.texture_form_id,
        )
    });
    for quadrant in 0..4 {
        let bases = layers
            .iter()
            .filter(|layer| layer.quadrant == quadrant && layer.is_base)
            .count();
        color_eyre::eyre::ensure!(
            bases <= 1,
            "quadrant {quadrant} has {bases} BTXT base layers"
        );
    }
    Ok((layers, dropped_layers))
}

fn normalize_texture_layers(layers: &mut Vec<TerrainLayer>) {
    // Official plugins contain null BTXT/ATXT references. They mean that no
    // texture is assigned, not that form 00000000 must resolve at runtime.
    // Keep parsing their VTXT payloads for structural validation, then remove
    // the null layers before synthesizing the implicit base used by overlays.
    layers.retain(|layer| layer.texture_form_id != 0);
    for quadrant in 0..4 {
        let has_layers = layers.iter().any(|layer| layer.quadrant == quadrant);
        let has_base = layers
            .iter()
            .any(|layer| layer.quadrant == quadrant && layer.is_base);
        if has_layers && !has_base {
            layers.push(TerrainLayer {
                texture_form_id: 0,
                quadrant,
                layer: 0,
                is_base: true,
                weights: Vec::new(),
            });
        }
        while layers
            .iter()
            .filter(|layer| layer.quadrant == quadrant)
            .count()
            > 6
        {
            let weakest = layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| layer.quadrant == quadrant && !layer.is_base)
                .min_by(|(_, left), (_, right)| {
                    let left_weight: f32 = left.weights.iter().map(|weight| weight.opacity).sum();
                    let right_weight: f32 = right.weights.iter().map(|weight| weight.opacity).sum();
                    left_weight
                        .total_cmp(&right_weight)
                        .then_with(|| right.layer.cmp(&left.layer))
                })
                .map(|(index, _)| index)
                .expect("an over-capacity quadrant must contain an overlay");
            layers.remove(weakest);
        }
    }
    layers.sort_by_key(|layer| {
        (
            layer.quadrant,
            !layer.is_base,
            layer.layer,
            layer.texture_form_id,
        )
    });
}

/// Validate already normalized source layers without treating cleared IDs as NULL.
fn validate_preserved_layers(layers: &[TerrainLayer]) -> Result<()> {
    let sort_key = |layer: &TerrainLayer| {
        (
            layer.quadrant,
            !layer.is_base,
            layer.layer,
            layer.texture_form_id,
        )
    };
    color_eyre::eyre::ensure!(
        layers
            .windows(2)
            .all(|pair| sort_key(&pair[0]) <= sort_key(&pair[1])),
        "terrain layers are not in normalized quadrant/slot order"
    );
    let mut slots = std::collections::HashSet::new();
    for layer in layers {
        color_eyre::eyre::ensure!(layer.quadrant < 4, "terrain quadrant is outside 0..=3");
        if layer.is_base {
            color_eyre::eyre::ensure!(
                layer.layer == 0 && layer.weights.is_empty(),
                "base terrain layer has an overlay slot or weights"
            );
        } else {
            color_eyre::eyre::ensure!(
                slots.insert((layer.quadrant, layer.layer)),
                "terrain overlay repeats a quadrant/slot"
            );
        }
        let mut vertices = std::collections::HashSet::new();
        for weight in &layer.weights {
            color_eyre::eyre::ensure!(
                weight.vertex < 17 * 17,
                "terrain weight vertex is outside a LAND quadrant"
            );
            color_eyre::eyre::ensure!(
                weight.opacity.is_finite() && (0.0..=1.0).contains(&weight.opacity),
                "terrain weight opacity is invalid"
            );
            color_eyre::eyre::ensure!(
                vertices.insert(weight.vertex),
                "terrain overlay repeats a weight vertex"
            );
        }
    }
    for quadrant in 0..4 {
        let entries: Vec<_> = layers
            .iter()
            .filter(|layer| layer.quadrant == quadrant)
            .collect();
        color_eyre::eyre::ensure!(
            entries.is_empty()
                || (entries.len() <= 6
                    && entries.iter().filter(|layer| layer.is_base).count() == 1),
            "terrain quadrant must be empty or have one base and at most five overlays"
        );
    }
    Ok(())
}

fn copy_texture_layers(layers: &[TerrainLayer]) -> Vec<TerrainLayer> {
    layers
        .iter()
        .map(|layer| TerrainLayer {
            texture_form_id: layer.texture_form_id,
            quadrant: layer.quadrant,
            layer: layer.layer,
            is_base: layer.is_base,
            weights: layer
                .weights
                .iter()
                .map(|weight| TerrainWeight {
                    vertex: weight.vertex,
                    opacity: weight.opacity,
                })
                .collect(),
        })
        .collect()
}

pub fn validate_cell_cache(path: &Path) -> Result<Mmap> {
    let file = File::open(path)?;
    let mmap = unsafe { Mmap::map(&file)? };
    let archived =
        rkyv::access::<shared::ArchivedCellCache, Error>(&mmap).wrap_err("invalid cell cache")?;
    color_eyre::eyre::ensure!(
        archived.version == CELL_CACHE_VERSION,
        "unsupported cell cache version"
    );
    Ok(mmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_strategies::{arbitrary_bytes, config};
    use proptest::prelude::*;

    fn preservation_land() -> RawRecord {
        let assignment = |tag: &[u8; 4], texture: u32, slot: u16| {
            let mut bytes = texture.to_le_bytes().to_vec();
            bytes.extend([0, 0]);
            bytes.extend(slot.to_le_bytes());
            (tag.to_vec(), bytes)
        };
        let weight = |vertex: u16, opacity: f32| {
            let mut bytes = vertex.to_le_bytes().to_vec();
            bytes.extend([0, 0]);
            bytes.extend(opacity.to_le_bytes());
            (b"VTXT".to_vec(), bytes)
        };
        let mut land = record(0x1234, b"LAND", None, &[]);
        land.cell_form_id = Some(0x5678);
        // These independent native assignments distinguish absent nonzero targets
        // from declared NULL, and distinguish the later slot's weights from earlier ones.
        land.subrecords = vec![
            assignment(b"BTXT", 0x2000, 0),
            assignment(b"ATXT", 0x2200, 2),
            weight(5, 0.25),
            assignment(b"ATXT", 0x844, 2),
            weight(9, 0.75),
            assignment(b"ATXT", 0, 2),
            weight(11, 0.125),
        ];
        land
    }

    #[test]
    fn preserved_layers_resolve_missing_texture_duplicate_slot_before_target_clearing() {
        let source = preservation_land();
        let preserved = preserved_texture_layers(&source).unwrap();
        assert_eq!(preserved.len(), 2);
        assert_eq!(preserved[0].texture_form_id, 0x2000);
        assert_eq!(preserved[1].texture_form_id, 0x844);
        assert_eq!(preserved[1].layer, 2);
        assert_eq!(preserved[1].weights.len(), 1);
        assert_eq!(preserved[1].weights[0].vertex, 9);
        assert_eq!(preserved[1].weights[0].opacity.to_bits(), 0.75f32.to_bits());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.rkyv");
        let records = HashMap::from([(source.form_id, source.clone())]);
        write_cell_cache(&records, &path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut overrides = HashMap::from([(source.form_id, copy_texture_layers(&preserved))]);
        write_cell_cache_with_preserved_layers(&records, &HashMap::new(), &overrides, &path)
            .unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "source cache preserves every byte"
        );

        let mut cleared = source;
        for (tag, bytes) in &mut cleared.subrecords {
            if tag.as_slice() == b"ATXT" && bytes[..4] == 0x844u32.to_le_bytes() {
                bytes[..4].fill(0);
            }
        }
        let records = HashMap::from([(cleared.form_id, cleared)]);
        write_cell_cache(&records, &path).unwrap();
        let ordinary =
            rkyv::from_bytes::<CellCache, Error>(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(ordinary.cells[0].layers[1].texture_form_id, 0x2200);
        assert_eq!(ordinary.cells[0].layers[1].weights[0].vertex, 5);

        overrides.get_mut(&0x1234).unwrap()[1].texture_form_id = 0;
        write_cell_cache_with_preserved_layers(&records, &HashMap::new(), &overrides, &path)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let cache = rkyv::from_bytes::<CellCache, Error>(&bytes).unwrap();
        let layer = &cache.cells[0].layers[1];
        assert_eq!(cache.cells[0].layers.len(), 2);
        assert!(!layer.is_base);
        assert_eq!(layer.texture_form_id, 0);
        assert_eq!(layer.layer, 2);
        assert_eq!(layer.weights.len(), 1);
        assert_eq!(layer.weights[0].vertex, 9);
        assert_eq!(layer.weights[0].opacity.to_bits(), 0.75f32.to_bits());
        write_cell_cache_with_preserved_layers(&records, &HashMap::new(), &overrides, &path)
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn preserved_layers_drop_native_null_assignments_but_validate_their_weights() {
        let mut land = preservation_land();
        land.subrecords.drain(..5);
        assert!(preserved_texture_layers(&land).unwrap().is_empty());
        land.subrecords[1].1[4..8].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(preserved_texture_layers(&land).is_err());
    }

    #[test]
    fn preserved_layers_invalid_auxiliary_data_falls_back_and_publishes_neighbors() {
        let land = preservation_land();
        let neighbor = record(0x9876, b"LAND", None, &[]);
        let records = HashMap::from([(land.form_id, land.clone()), (neighbor.form_id, neighbor)]);
        let valid = preserved_texture_layers(&land).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.rkyv");
        write_cell_cache(&records, &path).unwrap();
        let ordinary = std::fs::read(&path).unwrap();
        for malformed in 0..10 {
            let mut layers = copy_texture_layers(&valid);
            match malformed {
                0 => layers[1].weights[0].opacity = f32::NAN,
                1 => layers[1].quadrant = 4,
                2 => layers[1].weights[0].vertex = 289,
                3 => layers.insert(0, copy_texture_layers(&layers[..1]).remove(0)),
                4 => layers.push(copy_texture_layers(&layers[1..]).remove(0)),
                5 => layers[1].weights.push(TerrainWeight {
                    vertex: 9,
                    opacity: 0.5,
                }),
                6 => {
                    layers.remove(0);
                }
                7 => {
                    for slot in 3..8 {
                        let mut extra = copy_texture_layers(&layers[1..2]).remove(0);
                        extra.layer = slot;
                        layers.push(extra);
                    }
                }
                8 => layers[1].weights[0].opacity = 1.25,
                9 => layers.reverse(),
                _ => unreachable!(),
            }
            assert!(validate_preserved_layers(&layers).is_err());
            let overrides = HashMap::from([(land.form_id, layers)]);
            assert_eq!(
                write_cell_cache_with_preserved_layers(
                    &records,
                    &HashMap::new(),
                    &overrides,
                    &path
                )
                .unwrap(),
                2,
                "invalid auxiliary case {malformed} must retain both cells"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                ordinary,
                "auxiliary case {malformed}"
            );
        }
    }

    /// Distinct authored channels reveal selection of a complete LAND rather than a mixture.
    fn selection_land(form_id: u32, load_order: u32, offset: f32, marker: u8) -> RawRecord {
        let mut heightmap = offset.to_le_bytes().to_vec();
        heightmap.extend(vec![0; 33 * 33 + 3]);
        let mut land = record(
            form_id,
            b"LAND",
            None,
            &[
                (b"VNML", vec![marker; 33 * 33 * 3]),
                (b"VHGT", heightmap),
                (b"VCLR", vec![marker + 1; 33 * 33 * 3]),
            ],
        );
        land.cell_form_id = Some(0x7654);
        land.load_order = load_order;
        land
    }

    /// Old hash iteration necessarily loses the first candidate despite its later priority.
    #[test]
    fn same_parent_land_uses_winning_load_order_instead_of_hash_iteration() {
        let mut records = HashMap::from([
            (0x0200_8011, selection_land(0x0200_8011, 0, 3.0, 7)),
            (0xFE00_1821, selection_land(0xFE00_1821, 0, 11.0, 19)),
        ]);
        let first = records.values().next().unwrap().form_id;
        for land in records.values_mut() {
            land.load_order = if land.form_id == first { 17 } else { 5 };
        }
        let (expected_height, expected_marker) = if first == 0x0200_8011 {
            (24.0, 7u8)
        } else {
            (88.0, 19u8)
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.rkyv");
        assert_eq!(write_cell_cache(&records, &path).unwrap(), 1);
        let cache = rkyv::from_bytes::<CellCache, Error>(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(cache.cells[0].heights, vec![expected_height; 33 * 33]);
        assert_eq!(
            cache.cells[0].normals,
            vec![expected_marker as i8; 33 * 33 * 3]
        );
        assert_eq!(
            cache.cells[0].vertex_colors,
            vec![expected_marker + 1; 33 * 33 * 3]
        );
    }

    /// Physical order wins a same-plugin tie even when the later record has a smaller ID.
    #[test]
    fn same_parent_land_uses_native_offset_before_form_id() {
        let earlier = 0xFE00_1821;
        let later = 0x0200_8011;
        let source_offsets = HashMap::from([(earlier, 100), (later, 900)]);
        let directory = tempfile::tempdir().unwrap();
        let mut expected_bytes = None;
        for reverse in [false, true].into_iter().cycle().take(32) {
            let lands = [
                selection_land(earlier, 17, 3.0, 7),
                selection_land(later, 17, 11.0, 19),
            ];
            let mut records = HashMap::new();
            for index in if reverse { [1, 0] } else { [0, 1] } {
                let land = &lands[index];
                records.insert(land.form_id, land.clone());
            }
            let path = directory.path().join("cache.rkyv");
            assert_eq!(
                write_cell_cache_with_source_order(&records, &source_offsets, &path).unwrap(),
                1
            );
            let bytes = std::fs::read(&path).unwrap();
            let cache = rkyv::from_bytes::<CellCache, Error>(&bytes).unwrap();
            assert_eq!(cache.cells[0].heights, vec![88.0; 33 * 33]);
            assert_eq!(cache.cells[0].normals, vec![19; 33 * 33 * 3]);
            assert_eq!(cache.cells[0].vertex_colors, vec![20; 33 * 33 * 3]);
            if let Some(expected) = &expected_bytes {
                assert_eq!(&bytes, expected);
            } else {
                expected_bytes = Some(bytes);
            }
        }
    }

    /// Full/light ID magnitudes and physical offsets cannot override winning plugin priority.
    #[test]
    fn same_parent_land_priority_precedes_offset_and_light_identity() {
        let light = 0xFE00_1821;
        let full = 0x0200_8011;
        let source_offsets = HashMap::from([(light, 90_000), (full, 100)]);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.rkyv");
        for light_later in [false, true] {
            let records = HashMap::from([
                (
                    light,
                    selection_land(light, if light_later { 17 } else { 5 }, 3.0, 7),
                ),
                (
                    full,
                    selection_land(full, if light_later { 5 } else { 17 }, 11.0, 19),
                ),
            ]);
            write_cell_cache_with_source_order(&records, &source_offsets, &path).unwrap();
            let cache =
                rkyv::from_bytes::<CellCache, Error>(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                cache.cells[0].heights,
                vec![if light_later { 24.0 } else { 88.0 }; 33 * 33]
            );
        }
    }

    /// A tombstone carries no terrain and cannot displace a usable same-parent candidate.
    #[test]
    fn same_parent_land_ignores_deleted_candidates() {
        let live = selection_land(0x0200_8011, 5, 3.0, 7);
        let mut deleted = selection_land(0xFE00_1821, 17, f32::NAN, 19);
        deleted.flags = 0x20;
        let records = HashMap::from([(live.form_id, live), (deleted.form_id, deleted)]);
        let source_offsets = HashMap::from([(0x0200_8011, 100), (0xFE00_1821, 900)]);
        assert_eq!(
            land_winners_by_cell(&records, &source_offsets),
            HashMap::from([(0x7654, 0x0200_8011)])
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.rkyv");
        write_cell_cache_with_source_order(&records, &source_offsets, &path).unwrap();
        let cache = rkyv::from_bytes::<CellCache, Error>(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(cache.cells[0].heights, vec![24.0; 33 * 33]);
    }

    /// Validate an explicitly supplied local plugin set without publishing into its asset pack.
    #[test]
    #[ignore = "requires explicit MUDCRAB_GRASS_DATA and MUDCRAB_GRASS_PLUGINS paths"]
    fn local_load_order_terrain_cache_passes_validation() {
        let data = std::path::PathBuf::from(std::env::var_os("MUDCRAB_GRASS_DATA").unwrap());
        let list = std::path::PathBuf::from(std::env::var_os("MUDCRAB_GRASS_PLUGINS").unwrap());
        let paths = crate::esm::read_plugins_txt(&list, &data).unwrap();
        let records = crate::esm::EsmParser::merge_plugins(&paths).unwrap();
        let output = tempfile::tempdir().unwrap();
        let count = write_cell_cache(&records, &output.path().join("cell_cache.rkyv")).unwrap();
        assert!(count > 0);
        eprintln!(
            "Validated {count} terrain cells from {} plugins",
            paths.len()
        );
    }

    #[test]
    fn decodes_vhgt_deltas_into_absolute_heights() {
        let count = usize::from(LAND_SIDE) * usize::from(LAND_SIDE);
        let mut bytes = 2.0f32.to_le_bytes().to_vec();
        bytes.extend(std::iter::repeat_n(1, count));
        let heights = decode_vhgt(&bytes);
        assert_eq!(heights.len(), count);
        assert_eq!(heights[0], 24.0);
        assert_eq!(heights[1], 32.0);
        assert_eq!(heights[usize::from(LAND_SIDE)], 32.0);
    }

    #[test]
    fn supplies_flat_geometry_when_land_omits_vhgt_and_vnml() {
        let heights = decode_vhgt(&[]);
        let normals = decode_normals(&[], &heights);
        assert_eq!(heights.len(), usize::from(LAND_SIDE).pow(2));
        assert!(heights.iter().all(|height| *height == 0.0));
        assert_eq!(normals.len(), heights.len() * 3);
        assert!(
            normals
                .as_chunks::<3>()
                .0
                .iter()
                .all(|normal| normal == &[0, 0, 127])
        );
    }

    #[test]
    fn rejects_truncated_land_geometry_payloads() {
        let heights = vec![0.0; usize::from(LAND_SIDE).pow(2)];
        assert!(decode_vhgt(&[0; 16]).is_empty());
        assert!(decode_normals(&[0; 16], &heights).is_empty());
    }

    /// Reconstructed border normals must match the plane, while authored samples stay untouched.
    #[test]
    fn generated_normals_preserve_planar_slopes_at_edges_and_corners() {
        for (dx, dy) in [(64.0, 0.0), (0.0, -32.0), (64.0, -32.0)] {
            let heights: Vec<_> = (0..33)
                .flat_map(|y| (0..33).map(move |x| x as f32 * dx + y as f32 * dy))
                .collect();
            let normals = decode_normals(&[], &heights);
            let interior = &normals[(16 * 33 + 16) * 3..(16 * 33 + 16) * 3 + 3];
            assert!(
                normals
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .all(|normal| normal == interior)
            );
            assert!(interior[2] > 0);
            // Authored VNML must not be regenerated or normalized.
            let authored = vec![42; 33 * 33 * 3];
            assert_eq!(decode_normals(&authored, &heights), vec![42i8; 33 * 33 * 3]);
        }
    }

    /// Invalid geometry must fail before any bytes of a previous valid cache are replaced.
    #[test]
    fn invalid_land_fails_before_replacing_an_existing_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cell_cache.rkyv");
        let valid = record(0x1234, b"LAND", None, &[]);
        let mut records = HashMap::from([(valid.form_id, valid)]);
        write_cell_cache(&records, &path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut bad_fields = Vec::new();
        for offset in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX] {
            let mut height = offset.to_le_bytes().to_vec();
            height.extend(vec![0; 33 * 33]);
            bad_fields.push((b"VHGT", height));
        }
        bad_fields.push((b"VNML", vec![127; 33 * 33 * 3 - 1]));
        for (tag, bytes) in bad_fields {
            let expected = if tag == b"VNML" {
                "incomplete VNML"
            } else if f32::from_le_bytes(bytes[..4].try_into().unwrap()).is_finite() {
                "out of range"
            } else {
                "non-finite"
            };
            records.get_mut(&0x1234).unwrap().subrecords = vec![(tag.to_vec(), bytes)];
            let error = write_cell_cache(&records, &path).unwrap_err().to_string();
            assert!(error.contains("00001234"), "{error}");
            assert!(error.contains(expected), "{error}");
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    /// Assigned overlay indices are unique within a quadrant, not across the entire cell.
    #[test]
    fn overlay_indices_are_unique_within_each_quadrant() {
        let overlay = |texture: u32, quadrant: u8| {
            let mut bytes = texture.to_le_bytes().to_vec();
            bytes.extend([quadrant, 0, 2, 0]);
            (b"ATXT".to_vec(), bytes)
        };
        let (layers, dropped) = extract_texture_layers(&[overlay(1, 0), overlay(2, 0)]).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].texture_form_id, 2);
        assert_eq!(
            dropped,
            [DroppedLayer {
                quadrant: 0,
                layer: 2,
                texture_form_id: 1,
            }]
        );
        for subrecords in [
            [overlay(1, 0), overlay(1, 1)],
            [overlay(0, 0), overlay(1, 0)],
        ] {
            let (_, dropped) = extract_texture_layers(&subrecords).unwrap();
            assert!(dropped.is_empty());
        }
        let (_, dropped) =
            extract_texture_layers(&[overlay(1, 0), overlay(2, 1), overlay(3, 1), overlay(4, 0)])
                .unwrap();
        assert_eq!(
            dropped,
            [
                DroppedLayer {
                    quadrant: 1,
                    layer: 2,
                    texture_form_id: 2,
                },
                DroppedLayer {
                    quadrant: 0,
                    layer: 2,
                    texture_form_id: 1,
                },
            ]
        );
    }

    /// Publication retains the later record's texture and weights regardless of FormID order.
    #[test]
    fn duplicate_overlay_slots_publish_the_later_entry_deterministically() {
        let overlay = |texture: u32, quadrant: u8| {
            let mut bytes = texture.to_le_bytes().to_vec();
            bytes.extend([quadrant, 0, 2, 0]);
            (b"ATXT".to_vec(), bytes)
        };
        let weight = |vertex: u16, opacity: f32| {
            let mut bytes = vertex.to_le_bytes().to_vec();
            bytes.extend([0, 0]);
            bytes.extend(opacity.to_le_bytes());
            (b"VTXT".to_vec(), bytes)
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cell_cache.rkyv");
        for textures in [[1, 2, 3], [3, 2, 1], [1, 1, 1]] {
            let mut land = record(0x1234, b"LAND", None, &[]);
            land.cell_form_id = Some(0x5678);
            land.subrecords = vec![
                overlay(textures[0], 0),
                weight(5, 0.25),
                overlay(textures[1], 0),
                weight(6, 0.5),
                overlay(textures[2], 0),
                weight(7, 0.75),
                overlay(0, 0), // A later null assignment must not replace a real texture.
                overlay(4, 1), // The same slot in another quadrant is independent.
            ];
            let (_, dropped) = extract_texture_layers(&land.subrecords).unwrap();
            assert_eq!(
                dropped,
                [
                    DroppedLayer {
                        quadrant: 0,
                        layer: 2,
                        texture_form_id: textures[1],
                    },
                    DroppedLayer {
                        quadrant: 0,
                        layer: 2,
                        texture_form_id: textures[0],
                    },
                ]
            );
            let records = HashMap::from([(land.form_id, land)]);
            assert_eq!(write_cell_cache(&records, &path).unwrap(), 1);
            let original = std::fs::read(&path).unwrap();
            {
                let mmap = validate_cell_cache(&path).unwrap();
                let cache = rkyv::access::<shared::ArchivedCellCache, Error>(&mmap).unwrap();
                assert_eq!(cache.cells[0].cell_id, 0x5678);
                let layers = &cache.cells[0].layers;
                assert_eq!(layers.len(), 4, "two implicit bases and two overlays");
                let kept = &layers[1];
                assert!(!kept.is_base);
                assert_eq!(kept.quadrant, 0);
                assert_eq!(kept.layer, 2);
                assert_eq!(kept.texture_form_id, textures[2]);
                assert_eq!(kept.weights.len(), 1);
                assert_eq!(kept.weights[0].vertex, 7);
                assert_eq!(kept.weights[0].opacity, 0.75);
                assert_eq!(layers[3].texture_form_id, 4);
                assert_eq!(layers[3].quadrant, 1);
            }
            write_cell_cache(&records, &path).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    /// Dropping a duplicate must not hide a malformed payload on the earlier assignment.
    #[test]
    fn discarded_duplicate_overlay_payloads_still_fail_validation() {
        let overlay = (b"ATXT".to_vec(), vec![1, 0, 0, 0, 0, 0, 2, 0]);
        let weight = [0u8; 8];
        assert!(
            extract_texture_layers(&[
                overlay.clone(),
                (b"VTXT".to_vec(), [weight, weight].concat()),
                overlay,
            ])
            .is_err()
        );
    }

    /// Null overlays may reuse an assigned layer index, but their payloads must still be valid.
    #[test]
    fn null_overlay_indices_are_ignored_when_publishing_the_cache() {
        let overlay = |texture: u32| {
            let mut bytes = texture.to_le_bytes().to_vec();
            bytes.extend([0, 0, 2, 0]);
            (b"ATXT".to_vec(), bytes)
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cell_cache.rkyv");
        for textures in [[0, 1, 0], [1, 0, 0], [0, 0, 1]] {
            let mut land = record(0x1234, b"LAND", None, &[]);
            land.subrecords = textures.into_iter().map(overlay).collect();
            write_cell_cache(&HashMap::from([(land.form_id, land)]), &path).unwrap();
            let mmap = validate_cell_cache(&path).unwrap();
            let cache = rkyv::access::<shared::ArchivedCellCache, Error>(&mmap).unwrap();
            let layers = &cache.cells[0].layers;
            assert_eq!(layers.len(), 2, "implicit base plus the assigned overlay");
            assert!(layers[0].is_base);
            assert_eq!(layers[0].texture_form_id, 0);
            assert!(!layers[1].is_base);
            assert_eq!(layers[1].texture_form_id, 1);
            assert_eq!(layers[1].layer, 2);
        }
        let original = std::fs::read(&path).unwrap();
        // Malformed null assignments remain fatal even though normalization drops them.
        for subrecords in [
            vec![overlay(0), (b"VTXT".to_vec(), vec![0; 7])],
            vec![overlay(0), (b"VTXT".to_vec(), vec![0; 16])],
        ] {
            let mut land = record(0x1234, b"LAND", None, &[]);
            land.subrecords = subrecords;
            assert!(write_cell_cache(&HashMap::from([(land.form_id, land)]), &path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    /// Repair individual invalid normals without overwriting neighboring authored values.
    #[test]
    fn zero_authored_normals_are_repaired_without_changing_valid_neighbors() {
        let heights = vec![0.0; 33 * 33];
        let mut authored = vec![42; 33 * 33 * 3];
        authored[300..303].fill(0);
        let repaired = decode_normals(&authored, &heights);
        assert_eq!(&repaired[300..303], &[0, 0, 127]);
        for (index, &normal) in repaired.iter().enumerate() {
            if !(300..303).contains(&index) {
                assert_eq!(normal, 42);
            }
        }
        let output = tempfile::tempdir().unwrap();
        let land = record(0x1234, b"LAND", None, &[(b"VNML", authored)]);
        assert_eq!(
            write_cell_cache(
                &HashMap::from([(land.form_id, land)]),
                &output.path().join("cache.rkyv")
            )
            .unwrap(),
            1
        );
    }

    fn record(
        form_id: u32,
        record_type: &[u8; 4],
        worldspace: Option<u32>,
        subrecords: &[(&[u8; 4], Vec<u8>)],
    ) -> RawRecord {
        RawRecord {
            form_id,
            record_type: *record_type,
            flags: 0,
            subrecords: subrecords
                .iter()
                .map(|(tag, bytes)| (tag.to_vec(), bytes.clone()))
                .collect(),
            cell_form_id: None,
            worldspace_form_id: worldspace,
            load_order: 0,
        }
    }

    fn floats(values: &[f32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    #[test]
    fn exterior_cells_without_their_own_water_height_take_the_worldspace_default() {
        let tamriel = record(
            0x3C,
            b"WRLD",
            None,
            &[
                (b"DNAM", floats(&[-27000.0, -14000.0])),
                // An arbitrary water type id, not retail data (Tamriel's real NAM2 is 0x18).
                (b"NAM2", 0x0001_8F2Cu32.to_le_bytes().to_vec()),
            ],
        );
        let no_water_world = record(
            0x0001_1111,
            b"WRLD",
            None,
            &[(b"DNAM", floats(&[0.0, 9_999_999.0]))],
        );
        // Morthal's marsh: FLT_MAX ("use the default") with its own water type.
        let marsh = record(
            0x937C,
            b"CELL",
            Some(0x3C),
            &[
                (b"XCLW", f32::MAX.to_le_bytes().to_vec()),
                (b"XCWT", 0x0010_5CC3u32.to_le_bytes().to_vec()),
            ],
        );
        // No XCLW at all, no XCWT: the default height and the worldspace's water type.
        let sea = record(0x9000, b"CELL", Some(0x3C), &[]);
        // Riverwood's river states its own height.
        let river = record(
            0x9712,
            b"CELL",
            Some(0x3C),
            &[(b"XCLW", (-250.0f32).to_le_bytes().to_vec())],
        );
        let cave = record(0x2222, b"CELL", Some(0x0001_1111), &[]);
        let interior = record(0x3333, b"CELL", None, &[]);
        let records: HashMap<u32, RawRecord> =
            [tamriel, no_water_world, marsh, sea, river, cave, interior]
                .into_iter()
                .map(|record| (record.form_id, record))
                .collect();

        let water = water_by_cell(&records);
        assert_eq!(water[&0x937C], (Some(-14000.0), Some(0x0010_5CC3)));
        assert_eq!(water[&0x9000], (Some(-14000.0), Some(0x0001_8F2C)));
        assert_eq!(water[&0x9712], (Some(-250.0), Some(0x0001_8F2C)));
        assert_eq!(
            water[&0x2222],
            (None, None),
            "a 'no water' marker default is not a height"
        );
        assert_eq!(water[&0x3333], (None, None));
    }

    #[test]
    fn rejects_skyrim_no_water_sentinel() {
        assert_eq!(normalize_water_height(f32::MAX), None);
        assert_eq!(normalize_water_height(f32::INFINITY), None);
        assert_eq!(normalize_water_height(-11592.0), Some(-11592.0));
    }

    #[test]
    fn preserves_btxt_base_and_reads_atxt_u16_layer() {
        let mut base = 0x1234u32.to_le_bytes().to_vec();
        base.extend([2, 0, 0, 0]);
        let mut alpha = 0x5678u32.to_le_bytes().to_vec();
        alpha.extend([2, 0]);
        alpha.extend(0x0102u16.to_le_bytes());
        let mut weight = 18u16.to_le_bytes().to_vec();
        weight.extend([0, 0]);
        weight.extend(0.75f32.to_le_bytes());
        let (layers, dropped) = extract_texture_layers(&[
            (b"BTXT".to_vec(), base),
            (b"ATXT".to_vec(), alpha),
            (b"VTXT".to_vec(), weight),
        ])
        .unwrap();
        assert!(dropped.is_empty());
        assert!(layers[0].is_base);
        assert_eq!(layers[1].layer, 0x0102);
        assert_eq!(layers[1].weights[0].vertex, 18);
        assert_eq!(layers[1].weights[0].opacity, 0.75);
    }

    #[test]
    fn rejects_invalid_land_layer_payloads() {
        assert!(extract_texture_layers(&[(b"BTXT".to_vec(), vec![0; 7])]).is_err());
        let mut alpha = 1u32.to_le_bytes().to_vec();
        alpha.extend([4, 0, 0, 0]);
        assert!(extract_texture_layers(&[(b"ATXT".to_vec(), alpha)]).is_err());
    }

    #[test]
    fn supplies_implicit_base_and_drops_the_weakest_excess_overlay() {
        let mut layers = (0..6)
            .map(|layer| TerrainLayer {
                texture_form_id: u32::from(layer) + 1,
                quadrant: 2,
                layer,
                is_base: false,
                weights: vec![TerrainWeight {
                    vertex: layer,
                    opacity: if layer == 4 { 0.01 } else { 0.5 },
                }],
            })
            .collect::<Vec<_>>();
        normalize_texture_layers(&mut layers);
        assert_eq!(layers.len(), 6);
        assert!(layers[0].is_base);
        assert_eq!(layers[0].texture_form_id, 0);
        assert!(!layers.iter().any(|layer| layer.texture_form_id == 5));
    }

    #[test]
    fn drops_null_official_layers_without_creating_a_texture_reference() {
        let mut layers = vec![
            TerrainLayer {
                texture_form_id: 0,
                quadrant: 1,
                layer: 0,
                is_base: true,
                weights: Vec::new(),
            },
            TerrainLayer {
                texture_form_id: 0,
                quadrant: 2,
                layer: 4,
                is_base: false,
                weights: vec![TerrainWeight {
                    vertex: 7,
                    opacity: 0.5,
                }],
            },
        ];

        normalize_texture_layers(&mut layers);

        assert!(layers.is_empty());
    }

    #[test]
    fn replaces_a_null_base_when_the_quadrant_has_real_overlays() {
        let mut layers = vec![
            TerrainLayer {
                texture_form_id: 0,
                quadrant: 3,
                layer: 0,
                is_base: true,
                weights: Vec::new(),
            },
            TerrainLayer {
                texture_form_id: 0x1234,
                quadrant: 3,
                layer: 2,
                is_base: false,
                weights: vec![TerrainWeight {
                    vertex: 9,
                    opacity: 0.75,
                }],
            },
        ];

        normalize_texture_layers(&mut layers);

        assert_eq!(layers.len(), 2);
        assert!(layers[0].is_base);
        assert_eq!(layers[0].texture_form_id, 0);
        assert_eq!(layers[1].texture_form_id, 0x1234);
    }

    proptest! {
        #![proptest_config(config(256))]

        #[test]
        fn land_decoders_never_panic_on_arbitrary_bytes(
            // Arbitrary lengths reach only the decoders' length checks, so half the cases are
            // arbitrary bytes at the lengths the decoders accept: a VHGT record (with up to its
            // three padding bytes) and a raw VNML grid.
            heights in prop_oneof![
                arbitrary_bytes(2 * 4 + 33 * 33),
                proptest::collection::vec(any::<u8>(), 4 + 33 * 33..=4 + 33 * 33 + 3),
            ],
            normals in prop_oneof![
                arbitrary_bytes(3 * 33 * 33),
                proptest::collection::vec(any::<u8>(), 3 * 33 * 33),
            ],
            subrecords in arbitrary_bytes(512),
        ) {
            let decoded = decode_vhgt(&heights);
            let _ = decode_normals(&normals, &decoded);
            let _ = decode_normals(&[], &decoded);
            if let Ok(decoded) = crate::esm::extractors::extract_subrecords(&subrecords) {
                let _ = extract_texture_layers(&decoded);
            }
        }
    }
}
