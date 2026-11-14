//! Recover selection slots that terrain's separate NULL policy intentionally removes.
//! Behavior follows spud (MIT, chasmlol/spud); see third-party/spud/LICENSE.
use super::links;
use color_eyre::{
    Result,
    eyre::{ensure, eyre},
};
use shared::{CachedLand, TerrainLayer, TerrainWeight};
use std::collections::{BTreeMap, BTreeSet};

/// Restore original slots only when normalization loses NULL, repeated or clamped assignments.
pub(super) fn restore_layers(land: &mut CachedLand, records: &[Vec<u8>]) -> Result<bool> {
    let decoded: Vec<_> = records
        .iter()
        .map(|bytes| {
            links::stored_subrecords(bytes)
                .ok_or_else(|| eyre!("unreadable LAND selection payload"))
        })
        .collect::<Result<_>>()?;
    let mut slots = BTreeSet::new();
    let mut needed = false;
    for subs in &decoded {
        for (tag, data) in subs {
            if tag == b"ATXT" && data.len() >= 8 {
                let slot = u16::from_le_bytes([data[6], data[7]]);
                needed |= data[..4] == [0; 4] || slot > 5 || !slots.insert((data[4], slot.min(5)));
            }
        }
    }
    if !needed {
        return Ok(false);
    }
    let mut layers = BTreeMap::new();
    for subs in decoded {
        for (index, (tag, data)) in subs.iter().enumerate() {
            if tag != b"BTXT" && tag != b"ATXT" {
                continue;
            }
            let base = tag == b"BTXT";
            ensure!(
                data.len() >= if base { 6 } else { 8 },
                "short LAND texture assignment"
            );
            let quadrant = data[4];
            ensure!(quadrant < 4, "invalid LAND selection quadrant");
            let texture = u32::from_le_bytes(data[..4].try_into().expect("checked length"));
            let slot = if base {
                0
            } else {
                u16::from_le_bytes([data[6], data[7]]).min(5)
            };
            let texture = if !base && texture == 0 {
                u32::MAX
            } else {
                texture
            };
            let entry = layers
                .entry((quadrant, !base, slot))
                .or_insert_with(|| TerrainLayer {
                    texture_form_id: texture,
                    quadrant,
                    layer: slot,
                    is_base: base,
                    weights: vec![],
                });
            entry.texture_form_id = texture;
            if !base
                && slot < 5
                && let Some((next_tag, weights)) = subs.get(index + 1)
                && next_tag == b"VTXT"
            {
                ensure!(
                    weights.len().is_multiple_of(8),
                    "partial LAND selection weight"
                );
                let mut values: BTreeMap<_, _> = entry
                    .weights
                    .iter()
                    .map(|weight| (weight.vertex, weight.opacity))
                    .collect();
                for bytes in weights.as_chunks::<8>().0 {
                    let vertex = u16::from_le_bytes([bytes[0], bytes[1]]);
                    let opacity = f32::from_le_bytes(bytes[4..8].try_into().expect("fixed chunk"));
                    ensure!(
                        vertex < 289 && opacity.is_finite() && (0.0..=1.0).contains(&opacity),
                        "invalid LAND selection weight"
                    );
                    values.insert(vertex, opacity);
                }
                entry.weights = values
                    .into_iter()
                    .map(|(vertex, opacity)| TerrainWeight { vertex, opacity })
                    .collect();
            }
        }
    }
    land.layers = layers.into_values().collect();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NULL paint keeps its slot and opacity independently of terrain's normalized projection.
    #[test]
    fn null_paint_and_last_slot_assignment_survive() {
        let weight = |opacity: f32| {
            [
                0u16.to_le_bytes().as_slice(),
                &[0; 2],
                &opacity.to_le_bytes(),
            ]
            .concat()
        };
        let fields = vec![
            (b"BTXT".to_vec(), [30u32.to_le_bytes(), [0; 4]].concat()),
            (b"ATXT".to_vec(), [0u32.to_le_bytes(), [0; 4]].concat()),
            (b"VTXT".to_vec(), weight(0.75)),
        ];
        let raw = crate::esm::extractors::serialize_subrecords(&fields);
        let mut land = CachedLand {
            cell_id: 1,
            width: 33,
            height: 33,
            heights: vec![],
            normals: vec![],
            vertex_colors: vec![],
            water_height: None,
            water_type_form_id: None,
            layers: vec![],
        };
        assert!(restore_layers(&mut land, &[raw]).unwrap());
        assert_eq!(land.layers.len(), 2);
        assert_eq!(land.layers[1].texture_form_id, u32::MAX);
        assert_eq!(land.layers[1].weights[0].opacity, 0.75);
    }
}
