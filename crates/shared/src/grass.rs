//! Versioned offline grass rules and masks. Instances are generated only at runtime.

use rkyv::{Archive, Deserialize, Serialize};

pub const GRASS_CACHE_VERSION: u32 = 1;
pub const GRASS_BLEND_SIDE: usize = 17;
pub const GRASS_EXCLUSION_SIDE: usize = 64;
pub const GRASS_CELL_SIZE: f32 = 4096.0;

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct GrassType {
    pub form_id: u32,
    pub model_path: String,
    pub density: u8,
    pub min_slope: u8,
    pub max_slope: u8,
    pub units_from_water: u16,
    pub water_comparison: u32,
    pub position_range: f32,
    pub height_range: f32,
    pub color_range: f32,
    pub wave_period: f32,
    pub flags: u8,
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct GrassLayer {
    pub texture_form_id: u32,
    pub grass_ids: Vec<u32>,
    /// Effective blend weights, row-major 17 by 17, including base residual.
    pub weights: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct GrassQuadrant {
    /// Creation XY: 0 southwest, 1 southeast, 2 northwest, 3 northeast.
    pub quadrant: u8,
    pub layers: Vec<GrassLayer>,
    /// Solid triangle obstruction samples, 64 by 64 bits (32 Creation units per bit),
    /// row-major, least-significant bit first.
    pub exclusion: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct GrassCell {
    pub cell_id: u32,
    pub worldspace_id: u32,
    pub grid_x: i32,
    pub grid_y: i32,
    pub input_hash: [u8; 32],
    /// Absolute Creation Z heights, row-major 33 by 33.
    pub heights: Vec<f32>,
    pub water_height: Option<f32>,
    pub quadrants: Vec<GrassQuadrant>,
}

#[derive(Debug, Clone, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct GrassCache {
    pub version: u32,
    /// Sorted by FormID for direct archived binary search.
    pub types: Vec<GrassType>,
    /// Sorted by cell FormID for direct archived binary search.
    pub cells: Vec<GrassCell>,
}

/// Check semantic invariants after rkyv byte validation, before worker sampling.
pub fn validate_archived_cache(cache: &ArchivedGrassCache) -> Result<(), &'static str> {
    if cache.version != GRASS_CACHE_VERSION {
        return Err("unsupported grass cache version");
    }
    let mut previous = None;
    for rule in cache.types.iter() {
        let id = rule.form_id.to_native();
        if previous.is_some_and(|last| last >= id) || id == 0 {
            return Err("grass types must have sorted unique nonzero IDs");
        }
        previous = Some(id);
        if rule.model_path.is_empty()
            || rule.water_comparison > 7
            || [
                rule.position_range,
                rule.height_range,
                rule.color_range,
                rule.wave_period,
            ]
            .iter()
            .any(|value| !value.to_native().is_finite())
        {
            return Err("invalid grass rule");
        }
    }
    previous = None;
    for cell in cache.cells.iter() {
        let id = cell.cell_id.to_native();
        if previous.is_some_and(|last| last >= id) {
            return Err("grass cells must have sorted unique IDs");
        }
        previous = Some(id);
        if cell.heights.len() != 33 * 33
            || cell.quadrants.len() != 4
            || cell
                .heights
                .iter()
                .any(|height| !height.to_native().is_finite())
            || cell
                .water_height
                .as_ref()
                .is_some_and(|height| !height.to_native().is_finite())
        {
            return Err("invalid grass terrain field");
        }
        for (index, quadrant) in cell.quadrants.iter().enumerate() {
            if usize::from(quadrant.quadrant) != index
                || quadrant.exclusion.len() != GRASS_EXCLUSION_SIDE * GRASS_EXCLUSION_SIDE / 8
            {
                return Err("invalid grass quadrant");
            }
            for layer in quadrant.layers.iter() {
                if layer.weights.len() != GRASS_BLEND_SIDE * GRASS_BLEND_SIDE
                    || layer.weights.iter().any(|weight| {
                        let value = weight.to_native();
                        !value.is_finite() || !(0.0..=1.0).contains(&value)
                    })
                {
                    return Err("invalid grass blend field");
                }
                let mut last = None;
                for grass_id in layer.grass_ids.iter() {
                    let id = grass_id.to_native();
                    if last.is_some_and(|previous| previous >= id)
                        || cache
                            .types
                            .binary_search_by_key(&id, |rule| rule.form_id.to_native())
                            .is_err()
                    {
                        return Err("invalid grass layer species");
                    }
                    last = Some(id);
                }
            }
        }
    }
    Ok(())
}
