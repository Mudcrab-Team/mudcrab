//! Deterministic CPU placement. All mapped reads and instance allocation run on a worker.
use super::{GrassBatch, GrassInstance, GrassTerrain};
use crate::{config::EngineConfig, world::grass_cache::GrassCache};
use shared::grass::{ArchivedGrassLayer, GRASS_BLEND_SIDE, GRASS_CELL_SIZE, GRASS_EXCLUSION_SIDE};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

/// Whether the separately prepared majority-support policy is compiled into placement.
pub const MAJORITY_GRASS_SUPPORT: bool = false;

pub struct GeneratedGrass {
    pub batches: Vec<GrassBatch>,
    pub generation_micros: u64,
    pub bytes: usize,
}

/// R2 points have an even spatial distribution in every prefix, with a cell-specific shift.
pub fn candidate(cell_id: u32, slot: u32) -> [f32; 2] {
    let shift_x = unit(hash(cell_id, 0, 0));
    let shift_y = unit(hash(cell_id, 0, 1));
    let n = f64::from(slot) + 1.0;
    [
        (f64::from(shift_x) + n * 0.754_877_666_246_692_7).fract() as f32,
        (f64::from(shift_y) + n * 0.569_840_290_998_053_2).fract() as f32,
    ]
}

/// Mix cell, candidate slot and random channel into a deterministic integer.
fn hash(cell: u32, slot: u32, channel: u32) -> u32 {
    let mut x = cell.wrapping_mul(0x9e37_79b9)
        ^ slot.wrapping_mul(0x85eb_ca6b)
        ^ channel.wrapping_mul(0xc2b2_ae35);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

/// Map the high 24 random bits into the half-open unit interval.
fn unit(x: u32) -> f32 {
    (x >> 8) as f32 / 16_777_216.0
}

/// Evaluate the documented comparison modes. Modes 6/7 constrain the named side
/// and allow the other side; this comparison is MudCrab placement policy.
pub fn water_allowed(height: f32, water: Option<f32>, distance: u16, mode: u32) -> bool {
    let Some(water) = water else {
        return true;
    };
    let delta = height - water;
    let d = f32::from(distance);
    match mode {
        0 => delta >= d,
        1 => delta >= 0.0 && delta <= d,
        2 => delta <= -d,
        3 => delta <= 0.0 && delta >= -d,
        4 => delta.abs() >= d,
        5 => delta.abs() <= d,
        6 => delta <= d,
        7 => delta >= -d,
        _ => false,
    }
}

/// Bilinearly sample a square field at normalized coordinates.
fn sample_grid(side: usize, x: f32, y: f32, mut value: impl FnMut(usize) -> f32) -> f32 {
    let px = x.clamp(0.0, 1.0) * (side - 1) as f32;
    let py = y.clamp(0.0, 1.0) * (side - 1) as f32;
    let ix = (px as usize).min(side - 2);
    let iy = (py as usize).min(side - 2);
    let tx = px - ix as f32;
    let ty = py - iy as f32;
    let a = value(iy * side + ix);
    let b = value(iy * side + ix + 1);
    let c = value((iy + 1) * side + ix);
    let d = value((iy + 1) * side + ix + 1);
    (a + (b - a) * tx) * (1.0 - ty) + (c + (d - c) * tx) * ty
}

/// Sample the rendered terrain triangles and derive their normal and slope.
fn terrain(height_at: &impl Fn(usize) -> f32, x: f32, y: f32) -> (f32, [i8; 3], f32) {
    // Match build_terrain_quadrant_mesh's [a,b,c, b,d,c] triangles.
    // Bilinear interpolation can bury short grass on nonplanar terrain quads.
    let px = x.clamp(0.0, 1.0) * 32.0;
    let py = y.clamp(0.0, 1.0) * 32.0;
    let ix = (px as usize).min(31);
    let iy = (py as usize).min(31);
    let tx = px - ix as f32;
    let ty = py - iy as f32;
    let a = height_at(iy * 33 + ix);
    let b = height_at(iy * 33 + ix + 1);
    let c = height_at((iy + 1) * 33 + ix);
    let d = height_at((iy + 1) * 33 + ix + 1);
    let (height, dx, dy) = if tx + ty <= 1.0 {
        (a + tx * (b - a) + ty * (c - a), b - a, c - a)
    } else {
        (
            d + (1.0 - tx) * (c - d) + (1.0 - ty) * (b - d),
            d - c,
            d - b,
        )
    };
    let dx = dx / (GRASS_CELL_SIZE / 32.0);
    let dy = dy / (GRASS_CELL_SIZE / 32.0);
    let length = (dx * dx + dy * dy + 1.0).sqrt();
    let normal = [-dx / length, -dy / length, 1.0 / length];
    let slope = normal[2].clamp(-1.0, 1.0).acos().to_degrees();
    (height, normal.map(|v| (v * 127.0).round() as i8), slope)
}

/// Encode a bounded span-relative value into an unsigned 16-bit field.
fn quantize(value: f32, span: f32) -> u16 {
    (value / span * f32::from(u16::MAX))
        .round()
        .clamp(0.0, f32::from(u16::MAX)) as u16
}

/// Encode the attributed FasterNGIO luminance/range law; 255 remains unit brightness.
fn brightness(range: f32, random: f32, luminance: f32) -> u8 {
    let range = range.clamp(0.0, 1.0);
    // Independently expressed formula from panthuncia/FasterNGIO
    // (GPL v3), 5a1bff6ad3e44b5b9c4b5acdd0663ae20860ca40, EmitBlade:
    // https://github.com/panthuncia/FasterNGIO/blob/5a1bff6ad3e44b5b9c4b5acdd0663ae20860ca40/src/Grass/Internal/PlacementCommon.h
    // u is our unchanged channel 9; s=2u-1 lies in [-1,1). This REPLACES
    // 1-r*u, rather than applying Color Range a second time. RNG is MudCrab's.
    let signed = random * 2.0 - 1.0;
    ((luminance * (1.0 - range + signed * range)).clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Sample the renderer's fixed [a,b,c,b,d,c] triangles at the encoded final root.
fn root_luminance(field: Option<&[f32]>, position: [u16; 2]) -> f32 {
    let Some(field) = field.filter(|field| field.len() == 33 * 33) else {
        return 1.0;
    };
    let [px, py] = position.map(|v| f32::from(v) / f32::from(u16::MAX) * 32.0);
    let ix = (px as usize).min(31);
    let iy = (py as usize).min(31);
    let tx = px - ix as f32;
    let ty = py - iy as f32;
    let a = field[iy * 33 + ix];
    let b = field[iy * 33 + ix + 1];
    let c = field[(iy + 1) * 33 + ix];
    let d = field[(iy + 1) * 33 + ix + 1];
    if tx + ty <= 1.0 {
        a + tx * (b - a) + ty * (c - a)
    } else {
        d + (1.0 - tx) * (c - d) + (1.0 - ty) * (b - d)
    }
}

/// Apply texture suitability and the type cap at both the candidate and jittered
/// position. The cache retains each texture's GNAM order; native Skyrim selection is not
/// reproduced by this deterministic placement algorithm.
fn layer_weight(layer: &ArchivedGrassLayer, x: f32, y: f32, config: &EngineConfig) -> f32 {
    if layer.weights.len() != GRASS_BLEND_SIDE * GRASS_BLEND_SIDE {
        return 0.0;
    }
    let weight = sample_grid(GRASS_BLEND_SIDE, x, y, |i| layer.weights[i].into());
    if weight < config.grass_texture_threshold {
        0.0
    } else {
        weight
    }
}

/// Generate one cell's species batches from the immutable cache with no main-thread terrain copy.
pub fn generate(cache: &GrassCache, cell_id: u32, config: &EngineConfig) -> GeneratedGrass {
    let Some(cell) = cache.cell(cell_id) else {
        return GeneratedGrass {
            batches: vec![],
            generation_micros: 0,
            bytes: 0,
        };
    };
    generate_field(
        cache,
        cell_id,
        config,
        cell.heights.len(),
        |index| f32::from(cell.heights[index]),
        cell.water_height.as_ref().map(|height| f32::from(*height)),
        None,
    )
}

/// Sample the immutable committed surface, retaining mapped blend fields and exclusions.
pub(super) fn generate_on_terrain(
    cache: &GrassCache,
    cell_id: u32,
    config: &EngineConfig,
    terrain: &GrassTerrain,
) -> GeneratedGrass {
    generate_field(
        cache,
        cell_id,
        config,
        terrain.heights().len(),
        |index| terrain.heights()[index],
        terrain.0.water_height,
        terrain.luminance(),
    )
}

/// Share placement rules between mapped speculative generation and the committed terrain field.
fn generate_field(
    cache: &GrassCache,
    cell_id: u32,
    config: &EngineConfig,
    height_count: usize,
    height_at: impl Fn(usize) -> f32,
    water: Option<f32>,
    luminance: Option<&[f32]>,
) -> GeneratedGrass {
    let started = Instant::now();
    let mut result = GeneratedGrass {
        batches: vec![],
        generation_micros: 0,
        bytes: 0,
    };
    if config.validate_grass().is_err() {
        return result;
    }
    let Some(cell) = cache.cell(cell_id) else {
        return result;
    };
    // Independent archive semantic validation is also performed when opening the cache.
    if height_count != 33 * 33 || (0..height_count).any(|index| !height_at(index).is_finite()) {
        return result;
    }
    let height_min = (0..height_count)
        .map(&height_at)
        .fold(f32::INFINITY, f32::min);
    let height_max = (0..height_count)
        .map(&height_at)
        .fold(f32::NEG_INFINITY, f32::max);
    let height_span = (height_max - height_min).max(1.0);
    let slots = ((GRASS_CELL_SIZE / config.grass_min_spacing).powi(2)).ceil() as u32;
    let mut instances: BTreeMap<u32, Vec<GrassInstance>> = BTreeMap::new();
    // Each admitted species owns a deterministic stream. Other species cannot
    // steal its roots or dilute its blend support.
    let ids: std::collections::BTreeSet<u32> = cell
        .quadrants
        .iter()
        .flat_map(|q| q.layers.iter())
        .flat_map(|layer| {
            layer
                .grass_ids
                .iter()
                .take(config.effective_grass_species_cap())
        })
        .map(|id| u32::from(*id))
        .collect();
    for id in ids {
        let Some(species) = cache.species(id) else {
            continue;
        };
        let seed = hash(cell_id, id, 12);
        for slot in 0..slots {
            let [x, y] = candidate(seed, slot);
            let quadrant = (x * 2.0) as u8 + (y * 2.0) as u8 * 2;
            let Some(mask) = cell.quadrants.iter().find(|q| q.quadrant == quadrant) else {
                continue;
            };
            let support = |mask: &shared::grass::ArchivedGrassQuadrant, x, y| -> f32 {
                mask.layers
                    .iter()
                    .filter(|layer| {
                        layer
                            .grass_ids
                            .iter()
                            .take(config.effective_grass_species_cap())
                            .any(|grass| u32::from(*grass) == id)
                    })
                    .map(|layer| layer_weight(layer, x, y, config))
                    .sum()
            };
            let weight = support(mask, (x * 2.0).fract(), (y * 2.0).fract());
            let (height, normal, slope) = terrain(&height_at, x, y);
            if weight <= 0.0
                || slope < f32::from(species.min_slope)
                || slope > f32::from(species.max_slope)
                || !water_allowed(
                    height,
                    water,
                    species.units_from_water.into(),
                    species.water_comparison.into(),
                )
                || unit(hash(seed, slot, 4))
                    >= (weight * f32::from(species.density) / 100.0
                        * config.grass_density_multiplier)
                        .min(1.0)
            {
                continue;
            }
            let position_range: f32 = species.position_range.into();
            // Authored Position Range is a total jitter span, not a radius.
            let px = x * GRASS_CELL_SIZE + (unit(hash(seed, slot, 5)) - 0.5) * position_range;
            let py = y * GRASS_CELL_SIZE + (unit(hash(seed, slot, 6)) - 0.5) * position_range;
            // Clamping makes an artificial line of clumps on the cell boundary.
            if !(0.0..GRASS_CELL_SIZE).contains(&px) || !(0.0..GRASS_CELL_SIZE).contains(&py) {
                continue;
            }
            // Test exclusion only where the clump is placed, not before and after jitter.
            let jx = px / GRASS_CELL_SIZE;
            let jy = py / GRASS_CELL_SIZE;
            let jq = (jx * 2.0) as u8 + (jy * 2.0) as u8 * 2;
            let Some(jmask) = cell.quadrants.iter().find(|q| q.quadrant == jq) else {
                continue;
            };
            let jbit = ((jy * 2.0).fract() * GRASS_EXCLUSION_SIDE as f32) as usize
                * GRASS_EXCLUSION_SIDE
                + ((jx * 2.0).fract() * GRASS_EXCLUSION_SIDE as f32) as usize;
            if jmask
                .exclusion
                .get(jbit / 8)
                .is_none_or(|byte| byte & (1 << (jbit % 8)) != 0)
            {
                continue;
            }
            let final_weight: f32 = jmask
                .layers
                .iter()
                .filter(|layer| {
                    layer
                        .grass_ids
                        .iter()
                        .take(config.effective_grass_species_cap())
                        .any(|grass| u32::from(*grass) == id)
                })
                .map(|layer| layer_weight(layer, (jx * 2.0).fract(), (jy * 2.0).fract(), config))
                .sum();
            if final_weight <= 0.0 {
                continue;
            }
            let (jh, jnormal, jslope) = if position_range != 0.0 {
                terrain(&height_at, jx, jy)
            } else {
                (height, normal, slope)
            };
            if jslope < f32::from(species.min_slope)
                || jslope > f32::from(species.max_slope)
                || !water_allowed(
                    jh,
                    water,
                    species.units_from_water.into(),
                    species.water_comparison.into(),
                )
            {
                continue;
            }
            if unit(hash(seed, slot, 4))
                >= (final_weight * f32::from(species.density) / 100.0
                    * config.grass_density_multiplier)
                    .min(1.0)
            {
                continue;
            }
            let height_range: f32 = species.height_range.into();
            // Runtime-only brightness: the cache format and species rules are unchanged.
            let color_range = f32::from(species.color_range).clamp(0.0, 1.0);
            let position_xy = [quantize(px, GRASS_CELL_SIZE), quantize(py, GRASS_CELL_SIZE)];
            instances.entry(id).or_default().push(GrassInstance {
                position: [
                    position_xy[0],
                    position_xy[1],
                    quantize(jh - height_min, height_span),
                ],
                rotation: (hash(seed, slot, 7) & 255) as u8,
                scale: ((1.0 + (unit(hash(seed, slot, 8)) * 2.0 - 1.0) * height_range)
                    .clamp(0.0, 2.0)
                    * 127.5)
                    .round() as u8,
                color: brightness(
                    color_range,
                    unit(hash(seed, slot, 9)),
                    root_luminance(luminance, position_xy),
                ),
                normal: jnormal,
                slot: {
                    let land = (root_luminance(luminance, position_xy).clamp(0.0, 1.0) * 127.0)
                        .round() as u32;
                    let factor =
                        1.0 - 2.0 * color_range + 2.0 * unit(hash(seed, slot, 9)) * color_range;
                    slot | (land << 18) | ((((factor + 1.0) * 63.5).round() as u32) << 25)
                },
            });
        }
    }
    for (id, instances) in instances {
        let species = cache
            .species(id)
            .expect("generated species exists in immutable mapping");
        result.bytes += instances.capacity() * std::mem::size_of::<GrassInstance>()
            + species.model_path.len()
            + std::mem::size_of::<Vec<GrassInstance>>()
            + 2 * std::mem::size_of::<usize>();
        result.batches.push(GrassBatch {
            cell_id,
            grid: [cell.grid_x.into(), cell.grid_y.into()],
            grass_id: id,
            model_path: species.model_path.as_str().to_owned(),
            instances: Arc::new(instances),
            height_min,
            height_span,
            slot_count: slots,
            wave_period: species.wave_period.into(),
            flags: species.flags,
        });
    }
    result.bytes += result.batches.capacity() * std::mem::size_of::<GrassBatch>()
        + std::mem::size_of::<GeneratedGrass>();
    result.generation_micros = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::grass::{GRASS_CACHE_VERSION, GrassCell, GrassLayer, GrassQuadrant, GrassType};

    /// Build one synthetic cell with supplied texture layers and flat terrain.
    fn fixture(layers: Vec<GrassLayer>) -> GrassCell {
        GrassCell {
            cell_id: 42,
            worldspace_id: 3,
            grid_x: -5,
            grid_y: 7,
            input_hash: [7; 32],
            obstruction_failures: vec![],
            expanded_sampling: 0,
            heights: vec![120.0; 33 * 33],
            water_height: Some(20.0),
            quadrants: (0..4)
                .map(|quadrant| GrassQuadrant {
                    quadrant,
                    layers: layers.clone(),
                    exclusion: vec![0; GRASS_EXCLUSION_SIDE * GRASS_EXCLUSION_SIDE / 8],
                })
                .collect(),
        }
    }

    /// Build a valid synthetic species rule with a chosen stable identity.
    fn species(id: u32) -> GrassType {
        GrassType {
            form_id: id,
            model_path: format!("meshes/grass/{id}.glb"),
            density: 100,
            min_slope: 0,
            max_slope: 90,
            units_from_water: 30,
            water_comparison: 0,
            position_range: 0.0,
            height_range: 0.35,
            color_range: 0.15,
            wave_period: 2.5,
            flags: 4,
        }
    }

    /// Serialize synthetic rules and a cell into a mapped runtime cache.
    fn mapped(types: Vec<GrassType>, cell: GrassCell) -> GrassCache {
        let file = tempfile::NamedTempFile::new().unwrap();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&shared::grass::GrassCache {
            version: GRASS_CACHE_VERSION,
            types,
            cells: vec![cell],
        })
        .unwrap();
        std::fs::write(file.path(), bytes).unwrap();
        GrassCache::open(file.path()).unwrap()
    }

    /// Build one synthetic layer with constant coverage and supplied species links.
    fn layer(id: u32, grass_ids: Vec<u32>, weight: f32) -> GrassLayer {
        GrassLayer {
            texture_form_id: id,
            grass_ids,
            weights: vec![weight; GRASS_BLEND_SIDE * GRASS_BLEND_SIDE],
        }
    }

    #[test]
    fn unrelated_species_do_not_change_target_population() {
        let config = EngineConfig {
            grass_max_types_per_texture: 4,
            ..Default::default()
        };
        let a = mapped(vec![species(7)], fixture(vec![layer(9, vec![7], 0.45)]));
        let b = mapped(
            vec![species(7), species(11)],
            fixture(vec![layer(9, vec![11, 7], 0.45)]),
        );
        let a = generate(&a, 42, &config);
        let b = generate(&b, 42, &config);
        assert!(!a.batches[0].instances.is_empty());
        assert_eq!(
            a.batches[0].instances,
            b.batches
                .iter()
                .find(|b| b.grass_id == 7)
                .unwrap()
                .instances
        );
    }

    #[test]
    /// Tuning metadata preserves every original byte except high slot bits, with exact replay.
    fn grass_tuning_metadata_preserves_packing_and_precision() {
        let mut rule = species(7);
        rule.color_range = 0.8;
        let cache = mapped(vec![rule], fixture(vec![layer(9, vec![7], 1.0)]));
        let snapshot = crate::world::cache::TerrainSnapshot {
            cell_id: 42,
            width: 33,
            height: 33,
            heights: vec![120.0; 1089],
            normals: vec![0; 3267],
            vertex_colors: vec![102; 3267],
            layers: vec![],
            water_height: Some(20.0),
            water_type_form_id: None,
        };
        let terrain = GrassTerrain::from_snapshot(&snapshot);
        let base = EngineConfig {
            grass_min_spacing: 8.0,
            ..Default::default()
        };
        let tuned = EngineConfig {
            grass_tuning: Some("unused.toml".into()),
            ..base.clone()
        };
        let original = generate_on_terrain(&cache, 42, &base, &terrain);
        let generated = generate_on_terrain(&cache, 42, &tuned, &terrain);
        let replay = generate_on_terrain(&cache, 42, &tuned, &terrain);
        assert_eq!(generated.bytes, original.bytes);
        assert_eq!(generated.batches[0].slot_count, 262144);
        assert_eq!(std::mem::size_of::<GrassInstance>(), 16);
        assert_eq!(generated.batches[0].instances, replay.batches[0].instances);
        assert_eq!(
            original.batches[0].instances.len(),
            generated.batches[0].instances.len()
        );
        for (old, new) in original.batches[0]
            .instances
            .iter()
            .zip(generated.batches[0].instances.iter())
        {
            let mut decoded = *new;
            decoded.slot = new.candidate_slot();
            let mut old = *old;
            old.slot = old.candidate_slot();
            assert_eq!(
                old, decoded,
                "all default brightness and placement bytes survive"
            );
            let land = ((new.slot >> 18) & 127) as f32 / 127.0;
            let range = (new.slot >> 25) as f32 * (2.0 / 127.0) - 1.0;
            assert!((land - 0.4).abs() <= 1.0 / 254.0 + 1e-6);
            let expected = 1.0 - 1.6 + 1.6 * unit(hash(hash(42, 7, 12), new.candidate_slot(), 9));
            assert!((range - expected).abs() <= 1.0 / 127.0 + 1e-6);
        }
        assert!(
            generated.batches[0]
                .instances
                .windows(2)
                .all(|pair| pair[0].candidate_slot() < pair[1].candidate_slot())
        );
    }

    #[test]
    /// LReachGrass01 admits exactly its configured resolved links despite descending FormIDs.
    fn reach_gnam_order_selects_exact_cap_edges() {
        // Literal official IDs: ReachGrass02, ReachGrass01, TundraGrass03.
        let order = vec![342275, 341137, 175315];
        let file = tempfile::NamedTempFile::new().unwrap();
        let data = shared::grass::GrassCache {
            version: GRASS_CACHE_VERSION,
            types: vec![species(175315), species(341137), species(342275)],
            cells: vec![fixture(vec![layer(117831, order.clone(), 1.0)])],
        };
        std::fs::write(
            file.path(),
            rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap(),
        )
        .unwrap();
        let opened = GrassCache::open(file.path());
        assert!(
            opened.is_ok(),
            "GNAM-ordered layer must load: {:?}",
            opened.err()
        );
        let cache = GrassCache::open(file.path()).unwrap();
        let cached: Vec<u32> = cache.cell(42).unwrap().quadrants[0].layers[0]
            .grass_ids
            .iter()
            .map(|id| (*id).into())
            .collect();
        assert_eq!(cached, order);
        let config = EngineConfig::default();
        assert_eq!(config.grass_max_types_per_texture, 2);
        for (cap, expected) in [
            (2, vec![341137, 342275]),
            (0, vec![]),
            (1, vec![342275]),
            (3, vec![175315, 341137, 342275]),
            (usize::MAX, vec![175315, 341137, 342275]),
        ] {
            let generated = generate(
                &cache,
                42,
                &EngineConfig {
                    grass_max_types_per_texture: cap,
                    ..config.clone()
                },
            );
            let ids: Vec<_> = generated
                .batches
                .iter()
                .map(|batch| batch.grass_id)
                .collect();
            assert_eq!(ids, expected, "GNAM cap {cap}");
            assert!(
                generated
                    .batches
                    .iter()
                    .all(|batch| !batch.instances.is_empty())
            );
        }
    }

    #[test]
    /// Exercise actual generated bytes, deterministic replay and both ends of the darkening range.
    fn g3_colour_range_only_darkens_without_changing_placement() {
        let config = EngineConfig::default();
        let mut reference = None;
        for (range, lower) in [
            (0.0, 1.0),
            (0.03, 0.94),
            (0.3, 0.4),
            (1.0, 0.0),
            (-1.0, 1.0),
            (2.0, 0.0),
        ] {
            let mut rule = species(7);
            rule.color_range = range;
            let cache = mapped(vec![rule], fixture(vec![layer(9, vec![7], 1.0)]));
            let generated = generate(&cache, 42, &config);
            let repeated = generate(&cache, 42, &config);
            let instances = &generated.batches[0].instances;
            assert_eq!(instances.len(), 41944);
            assert_eq!(instances, &repeated.batches[0].instances);
            let mut minimum = f32::INFINITY;
            let mut maximum = f32::NEG_INFINITY;
            for instance in instances.iter() {
                // The unit-range byte is decoded directly, independently of the generator.
                let brightness = f32::from(instance.color) / 255.0;
                assert!(brightness <= 1.0, "range {range}: brightness {brightness}");
                assert!(brightness >= lower - 0.5 / 255.0);
                minimum = minimum.min(brightness);
                maximum = maximum.max(brightness);
            }
            assert!(
                (minimum - lower).abs() <= 1.0 / 255.0,
                "range {range}: lower endpoint {minimum}"
            );
            assert!(
                (maximum - 1.0).abs() <= 1.0 / 255.0,
                "range {range}: upper endpoint {maximum}"
            );
            let placement: Vec<_> = instances
                .iter()
                .map(|instance| {
                    let mut instance = *instance;
                    instance.color = 0;
                    instance.slot = instance.candidate_slot();
                    instance
                })
                .collect();
            if let Some(reference) = &reference {
                assert_eq!(
                    &placement, reference,
                    "Colour Range must not change counts or placement"
                );
            } else {
                reference = Some(placement);
            }
        }
    }

    #[test]
    /// Literal scalar inputs distinguish the replacement formula from multiplying the old range.
    fn vclr_formula_known_inputs() {
        assert_eq!(
            brightness(0.2, 0.0, 1.0),
            153,
            "signed range lower endpoint"
        );
        assert_eq!(brightness(0.2, 0.25, 0.5), 89);
        assert_eq!(brightness(0.0, 0.75, 1.0), 255);
        assert_eq!(brightness(0.8, 0.0, 0.5), 0);
        assert_eq!(brightness(0.0, 0.25, 0.299), 76);
        assert_eq!(brightness(0.0, 0.25, 0.587), 150);
        assert_eq!(brightness(0.0, 0.25, 0.114), 29);
        assert_eq!(brightness(0.2, 0.5, 1.0), 204);
        assert_eq!(brightness(0.0, 0.25, 0.5), 128);
        assert_eq!(brightness(0.2, 0.75, 0.0), 0);
    }

    #[test]
    /// A single nonplanar corner distinguishes fixed triangles from bilinear/checkerboard fields.
    fn vclr_samples_fixed_triangles_and_white_fallback() {
        let mut field = vec![0.0; 33 * 33];
        field[34] = 1.0;
        let lower = [quantize(32.0, 4096.0); 2];
        let upper = [quantize(96.0, 4096.0); 2];
        assert_eq!(root_luminance(Some(&field), lower), 0.0);
        assert!((root_luminance(Some(&field), upper) - 0.5).abs() < 0.0001);
        field.fill(0.0);
        field[35] = 1.0;
        // The adjacent square has opposite parity: a checkerboard diagonal must also fail here.
        let odd_lower = [quantize(160.0, 4096.0), quantize(32.0, 4096.0)];
        let odd_upper = [quantize(224.0, 4096.0), quantize(96.0, 4096.0)];
        assert_eq!(root_luminance(Some(&field), odd_lower), 0.0);
        assert!((root_luminance(Some(&field), odd_upper) - 0.5).abs() < 0.0001);
        field[1088] = 0.25;
        assert_eq!(root_luminance(Some(&field), [u16::MAX; 2]), 0.25);
        assert_eq!(root_luminance(None, lower), 1.0);
        assert_eq!(root_luminance(Some(&field[..12]), lower), 1.0);
    }

    #[test]
    /// Committed LAND tint samples final encoded jittered roots without changing placement.
    fn vclr_committed_surface_samples_encoded_jittered_roots() {
        let mut rule = species(7);
        rule.color_range = 0.0;
        rule.position_range = 64.0;
        let cache = mapped(vec![rule], fixture(vec![layer(9, vec![7], 1.0)]));
        let snapshot = crate::world::cache::TerrainSnapshot {
            cell_id: 42,
            width: 33,
            height: 33,
            heights: vec![120.0; 1089],
            normals: vec![0; 3267],
            // A linear gray ramp yields byte round(encoded_x * 224 / 65535),
            // independently of the sampled triangle and brightness implementation.
            vertex_colors: (0..1089).flat_map(|i| [(i % 33 * 7) as u8; 3]).collect(),
            layers: vec![],
            water_height: Some(20.0),
            water_type_form_id: None,
        };
        let config = EngineConfig::default();
        let plain = generate(&cache, 42, &config);
        let enabled =
            generate_on_terrain(&cache, 42, &config, &GrassTerrain::from_snapshot(&snapshot));
        assert_eq!(
            plain.batches[0].instances.len(),
            enabled.batches[0].instances.len()
        );
        let mut jitter_matters = false;
        let mut quantization_matters = false;
        for (old, new) in plain.batches[0]
            .instances
            .iter()
            .zip(enabled.batches[0].instances.iter())
        {
            assert_eq!(old.color, 255);
            let expected_byte = (f64::from(new.position[0]) * 224.0 / 65535.0).round() as u8;
            assert_eq!(new.color, expected_byte, "slot {} encoded root", new.slot);
            let candidate_x = candidate(hash(42, 7, 12), new.candidate_slot())[0];
            jitter_matters |= (candidate_x * 224.0).round() as u8 != expected_byte;
            let raw_x = candidate_x * 4096.0
                + (unit(hash(hash(42, 7, 12), new.candidate_slot(), 5)) - 0.5) * 64.0;
            quantization_matters |=
                (f64::from(raw_x) * 224.0 / 4096.0).round() as u8 != expected_byte;
            let mut expected = *old;
            expected.color = expected_byte;
            assert_eq!(*new, expected);
        }
        assert!(jitter_matters, "fixture must reject pre-jitter sampling");
        assert!(
            quantization_matters,
            "fixture must reject pre-quantization sampling"
        );
    }

    #[test]
    /// Real LAND colours exercise literal luminance/Color Range cases, including white fallback.
    fn adopted_d_vclr_formula_on_committed_white_absent_and_coloured_land() {
        let config = EngineConfig::default();
        for (rgb, range, luminance) in [
            (Some([255, 255, 255]), 0.2, 1.0_f64),
            (None, 0.2, 1.0),
            (Some([255, 0, 0]), 0.0, 0.299),
            (Some([0, 255, 0]), 0.0, 0.587),
            (Some([0, 0, 255]), 0.0, 0.114),
            (Some([128, 128, 128]), 0.2, 128.0 / 255.0),
            (Some([64, 128, 192]), 0.8, 116.16 / 255.0),
            (Some([0, 0, 0]), 0.2, 0.0),
            (Some([255, 255, 255]), 0.0, 1.0),
        ] {
            let mut rule = species(7);
            rule.color_range = range;
            let cache = mapped(vec![rule], fixture(vec![layer(9, vec![7], 1.0)]));
            let snapshot = crate::world::cache::TerrainSnapshot {
                cell_id: 42,
                width: 33,
                height: 33,
                heights: vec![120.0; 1089],
                normals: vec![0; 3267],
                vertex_colors: rgb.map_or_else(Vec::new, |rgb| rgb.repeat(1089)),
                layers: vec![],
                water_height: Some(20.0),
                water_type_form_id: None,
            };
            let generated =
                generate_on_terrain(&cache, 42, &config, &GrassTerrain::from_snapshot(&snapshot));
            assert_eq!(generated.batches[0].instances.len(), 41944);
            for instance in generated.batches[0].instances.iter() {
                // An independently expanded double-precision expression pins the law.
                let u = f64::from(unit(hash(hash(42, 7, 12), instance.candidate_slot(), 9)));
                let r = f64::from(range);
                let expected = (255.0 * (luminance * (1.0 - 2.0 * r + 2.0 * u * r)).clamp(0.0, 1.0))
                    .round() as u8;
                assert!(
                    instance.color.abs_diff(expected) <= 1,
                    "rgb={rgb:?} range={range} slot={}: actual={} expected={expected}",
                    instance.candidate_slot(),
                    instance.color
                );
                if range == 0.0 {
                    assert_eq!(instance.color, expected);
                }
            }
            if rgb.is_none() {
                assert_eq!(
                    generated.batches[0].instances,
                    generate(&cache, 42, &config).batches[0].instances
                );
            }
        }
    }

    #[test]
    #[ignore = "performance"]
    /// Measure retained VCLR interpolation with eight species under the existing worker ceilings.
    fn performance_coloured_terrain_generation() {
        use std::{hint::black_box, time::Duration};

        let mut rules: Vec<_> = (7..15).map(species).collect();
        for rule in &mut rules {
            rule.position_range = 64.0;
        }
        let cache = mapped(
            rules,
            fixture(vec![
                layer(9, vec![7, 8, 9, 10], 1.0),
                layer(10, vec![11, 12, 13, 14], 0.4),
            ]),
        );
        let snapshot = crate::world::cache::TerrainSnapshot {
            cell_id: 42,
            width: 33,
            height: 33,
            heights: vec![120.0; 1089],
            normals: vec![0; 3267],
            vertex_colors: (0..1089).flat_map(|i| [(i % 33 * 7) as u8; 3]).collect(),
            layers: vec![],
            water_height: Some(20.0),
            water_type_form_id: None,
        };
        let terrain = GrassTerrain::from_snapshot(&snapshot);
        assert_eq!(
            terrain.bytes() - GrassTerrain::new(vec![120.0; 1089], Some(20.0)).bytes(),
            4356
        );
        for (spacing, slots, milliseconds) in
            [(64.0, 4096, 10), (32.0, 16384, 50), (20.0, 41944, 100)]
        {
            let config = EngineConfig {
                grass_min_spacing: spacing,
                grass_max_types_per_texture: 4,
                ..EngineConfig::default()
            };
            let warm = generate_on_terrain(&cache, 42, &config, &terrain);
            assert_eq!(
                warm.batches
                    .iter()
                    .map(|batch| batch.grass_id)
                    .collect::<Vec<_>>(),
                vec![7, 8, 9, 10, 11, 12, 13, 14]
            );
            let count: usize = warm.batches.iter().map(|batch| batch.instances.len()).sum();
            assert!(count > 500 && count <= slots * 8);
            assert!(warm.bytes + terrain.bytes() <= slots * 8 * 32 + 32 * 1024);
            assert!(
                warm.batches
                    .iter()
                    .flat_map(|batch| batch.instances.iter())
                    .any(|instance| instance.color < 100)
            );
            let mut samples = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                let result = black_box(generate_on_terrain(
                    &cache,
                    42,
                    black_box(&config),
                    black_box(&terrain),
                ));
                samples.push(start.elapsed());
                assert_eq!(warm.batches.len(), result.batches.len());
                for (expected, actual) in warm.batches.iter().zip(&result.batches) {
                    assert_eq!(expected.grass_id, actual.grass_id);
                    assert_eq!(expected.instances, actual.instances);
                }
            }
            samples.sort_unstable();
            let ceiling = Duration::from_millis(milliseconds);
            assert!(
                samples[1] < ceiling,
                "spacing={spacing}: {:?} exceeds {ceiling:?}",
                samples[1]
            );
            eprintln!(
                "coloured grass: spacing={spacing}, instances={count}, median={:?}, budget={ceiling:?}, retained_surface_bytes={}",
                samples[1],
                terrain.bytes()
            );
        }
    }

    #[test]
    /// Verify grass roots follow rendered triangles on nonplanar terrain.
    fn grass_roots_follow_rendered_triangles_on_nonplanar_terrain() {
        let mut cell = fixture(vec![layer(9, vec![7], 1.0)]);
        // Asymmetric Riverwood quad: bilinear midpoint is -104, rendered -56.
        for (index, height) in [
            (9 * 33 + 11, -152.0),
            (9 * 33 + 12, 32.0),
            (10 * 33 + 11, -144.0),
            (10 * 33 + 12, -152.0),
        ] {
            cell.heights[index] = height;
        }
        let cache = mapped(vec![species(7)], cell);
        let cell = cache.cell(42).unwrap();
        let height_at = |index| f32::from(cell.heights[index]);
        for (tx, ty, expected) in [
            (0.5, 0.5, -56.0),
            (0.25, 0.25, -104.0),
            (0.75, 0.75, -104.0),
            (1.0, 0.0, 32.0),
            (0.0, 1.0, -144.0),
        ] {
            let (height, _, _) = terrain(&height_at, (11.0 + tx) / 32.0, (9.0 + ty) / 32.0);
            assert!((height - expected).abs() < 0.001, "({tx},{ty}): {height}");
        }
        let (_, lower_normal, lower_slope) = terrain(&height_at, 11.25 / 32.0, 9.25 / 32.0);
        let (_, upper_normal, upper_slope) = terrain(&height_at, 11.75 / 32.0, 9.75 / 32.0);
        assert!(lower_normal[0] < -100 && lower_normal[1] < 0);
        assert!(upper_normal[0] > 0 && upper_normal[1] > 100);
        assert!((lower_slope - upper_slope).abs() < 0.001);
    }

    #[test]
    /// Verify skyrim default density preserves authored height.
    fn skyrim_default_density_preserves_authored_height() {
        let mut authored = species(7);
        authored.height_range = 0.0;
        let cache = mapped(vec![authored], fixture(vec![layer(9, vec![7], 1.0)]));
        let native = generate(&cache, 42, &EngineConfig::default());
        let sparse = generate(
            &cache,
            42,
            &EngineConfig {
                grass_min_spacing: 64.0,
                ..EngineConfig::default()
            },
        );
        assert_eq!(native.batches[0].slot_count, 41944);
        assert_eq!(
            native.batches[0].instances.len(),
            41944,
            "100% density must not be thinned a second time by neighbor separation"
        );
        assert!(native.batches[0].instances.len() > sparse.batches[0].instances.len() * 8);
        assert!(
            native.batches[0]
                .instances
                .iter()
                .all(|instance| instance.scale == 128)
        );
    }

    #[test]
    /// Verify texture threshold and type limit apply after jitter.
    fn texture_threshold_and_type_limit_apply_after_jitter() {
        let mut rules = vec![species(7), species(11), species(13)];
        for rule in &mut rules {
            rule.position_range = 768.0;
        }
        let mut painted = layer(9, vec![7, 11, 13], 0.2);
        for y in 0..GRASS_BLEND_SIDE {
            for x in 0..8 {
                painted.weights[y * GRASS_BLEND_SIDE + x] = 1.0;
            }
        }
        let cache = mapped(rules, fixture(vec![painted]));
        let config = EngineConfig {
            grass_texture_threshold: 0.5,
            grass_max_types_per_texture: 1,
            ..EngineConfig::default()
        };
        let generated = generate(&cache, 42, &config);
        assert_eq!(generated.batches.len(), 1);
        assert_eq!(generated.batches[0].grass_id, 7);
        assert!(generated.batches[0].instances.len() > 100);
        for instance in generated.batches[0].instances.iter() {
            let x = f32::from(instance.position[0]) / 65535.0 * GRASS_CELL_SIZE;
            // Blend declines from 1 at x=896 to .2 at x=1024; .5 at x=976.
            assert!(x % 2048.0 <= 976.1, "threshold not enforced at {x}");
        }
        let uncapped = generate(
            &cache,
            42,
            &EngineConfig {
                grass_max_types_per_texture: 4,
                ..config.clone()
            },
        );
        assert_eq!(uncapped.batches.len(), 3);
        let two = generate(
            &cache,
            42,
            &EngineConfig {
                grass_max_types_per_texture: 2,
                ..config
            },
        );
        assert_eq!(two.batches.len(), 2);
        assert_eq!(two.batches[0].grass_id, 7);
        assert_eq!(two.batches[1].grass_id, 11);
    }

    #[test]
    /// Verify generation is repeatable and prefixes are evenly distributed.
    fn generation_is_repeatable_and_prefixes_are_evenly_distributed() {
        let cache = mapped(vec![species(7)], fixture(vec![layer(9, vec![7], 1.0)]));
        let config = EngineConfig::default();
        let a = generate(&cache, 42, &config);
        let b = generate(&cache, 42, &config);
        assert_eq!(a.batches.len(), 1);
        assert_eq!(a.batches[0].instances, b.batches[0].instances);
        assert_eq!(a.batches[0].grid, [-5, 7]);
        assert!(a.batches[0].instances.len() > 1000);
        assert!(
            a.batches[0]
                .instances
                .windows(2)
                .all(|p| p[0].candidate_slot() < p[1].candidate_slot())
        );
        let mut bins = [0u32; 16];
        for slot in 0..256 {
            let [x, y] = candidate(42, slot);
            bins[(y * 4.0) as usize * 4 + (x * 4.0) as usize] += 1;
        }
        assert!(
            bins.iter().all(|&count| (10..=22).contains(&count)),
            "{bins:?}"
        );
        assert_ne!(candidate(42, 1), candidate(43, 1));
    }

    #[test]
    /// Verify each species receives its layer support without species averaging.
    fn blended_layers_and_many_species_keep_independent_support() {
        let cache = mapped(
            vec![species(7), species(11), species(13)],
            fixture(vec![layer(9, vec![7], 0.8), layer(10, vec![11, 13], 0.2)]),
        );
        let generated = generate(&cache, 42, &EngineConfig::default());
        let counts: Vec<_> = generated
            .batches
            .iter()
            .map(|b| (b.grass_id, b.instances.len()))
            .collect();
        assert_eq!(counts.len(), 3, "{counts:?}");
        assert!(
            counts[0].1 > counts[1].1 * 3 && counts[0].1 < counts[1].1 * 5,
            "{counts:?}"
        );
        assert!(counts[1].1 > 50 && counts[2].1 > 50, "{counts:?}");
    }

    #[test]
    /// Verify species ids cannot alias the density or jitter random channels.
    fn species_ids_cannot_alias_the_density_or_jitter_random_channels() {
        let cache = mapped(
            vec![species(7), species(9)],
            fixture(vec![layer(17, vec![7, 9], 0.5)]),
        );
        let generated = generate(&cache, 42, &EngineConfig::default());
        assert_eq!(generated.batches.len(), 2);
        let a = generated.batches[0].instances.len();
        let b = generated.batches[1].instances.len();
        assert!(a > 500 && b > 500);
        assert!(
            a.max(b) as f32 / (a.min(b) as f32) < 1.3,
            "equal suitability produced biased counts {a}, {b}"
        );
    }

    #[test]
    /// Verify final positions respect exclusions water slope and jitter span.
    fn final_positions_respect_exclusions_water_slope_and_jitter_span() {
        let mut rule = species(7);
        rule.position_range = 160.0;
        let mut cell = fixture(vec![layer(9, vec![7], 1.0)]);
        // Entire southwest quadrant excluded, including candidates jittered in from elsewhere.
        cell.quadrants[0].exclusion.fill(255);
        let cache = mapped(vec![rule], cell);
        let config = EngineConfig {
            grass_min_spacing: 64.0,
            ..EngineConfig::default()
        };
        let generated = generate(&cache, 42, &config);
        let points: Vec<_> = generated.batches[0]
            .instances
            .iter()
            .map(|i| {
                [
                    f32::from(i.position[0]) / 65535.0 * GRASS_CELL_SIZE,
                    f32::from(i.position[1]) / 65535.0 * GRASS_CELL_SIZE,
                ]
            })
            .collect();
        assert!(points.iter().all(|p| p[0] >= 2048.0 || p[1] >= 2048.0));
        for (point, instance) in points.iter().zip(generated.batches[0].instances.iter()) {
            let original = candidate(hash(42, 7, 12), instance.candidate_slot());
            for axis in 0..2 {
                assert!(
                    (point[axis] - original[axis] * GRASS_CELL_SIZE).abs() <= 80.05,
                    "position_range 160 is a total span, not a radius"
                );
            }
        }
        let mut steep_rule = species(7);
        steep_rule.min_slope = 45;
        let cache = mapped(vec![steep_rule], fixture(vec![layer(9, vec![7], 1.0)]));
        assert!(generate(&cache, 42, &config).batches.is_empty());
        let mut submerged = fixture(vec![layer(9, vec![7], 1.0)]);
        submerged.water_height = Some(150.0);
        let cache = mapped(vec![species(7)], submerged);
        assert!(generate(&cache, 42, &config).batches.is_empty());
    }

    #[test]
    /// Verify density zero emits nothing and water modes follow signed distance.
    fn density_zero_emits_nothing_and_water_modes_follow_signed_distance() {
        let cache = mapped(vec![species(7)], fixture(vec![layer(9, vec![7], 1.0)]));
        let config = EngineConfig {
            grass_density_multiplier: 0.0,
            ..EngineConfig::default()
        };
        assert!(generate(&cache, 42, &config).batches.is_empty());
        let above = [true, false, false, false, true, false, false, true];
        let below = [false, false, true, false, true, false, true, false];
        for mode in 0..8 {
            assert_eq!(
                water_allowed(50.0, Some(10.0), 20, mode),
                above[mode as usize]
            );
            assert_eq!(
                water_allowed(-30.0, Some(10.0), 20, mode),
                below[mode as usize]
            );
            assert!(water_allowed(50.0, None, 20, mode));
        }
    }

    #[test]
    /// Verify position jitter cannot move a species outside its painted texture.
    fn position_jitter_cannot_move_a_species_outside_its_painted_texture() {
        let mut rule = species(7);
        rule.position_range = 512.0;
        let mut painted = layer(9, vec![7], 0.0);
        for y in 0..GRASS_BLEND_SIDE {
            for x in 0..4 {
                painted.weights[y * GRASS_BLEND_SIDE + x] = 1.0;
            }
        }
        let cache = mapped(vec![rule], fixture(vec![painted]));
        let generated = generate(&cache, 42, &EngineConfig::default());
        assert_eq!(generated.batches.len(), 1);
        assert!(generated.batches[0].instances.len() > 30);
        for instance in generated.batches[0].instances.iter() {
            let local_x = f32::from(instance.position[0]) / 65535.0 * GRASS_CELL_SIZE;
            assert!(local_x % 2048.0 <= 512.05, "{local_x}");
        }
    }

    #[test]
    /// Verify jitter rechecks final density without applying species fraction twice.
    fn jitter_rechecks_final_density_without_applying_species_fraction_twice() {
        let mut sparse_species = species(7);
        sparse_species.density = 20;
        sparse_species.position_range = 768.0;
        let mut dense_species = species(11);
        dense_species.density = 80;
        dense_species.position_range = 768.0;
        let mut painted = layer(9, vec![7, 11], 0.02);
        for y in 0..GRASS_BLEND_SIDE {
            for x in 0..8 {
                painted.weights[y * GRASS_BLEND_SIDE + x] = 1.0;
            }
        }
        let cache = mapped(vec![sparse_species, dense_species], fixture(vec![painted]));
        let config = EngineConfig::default();
        let generated = generate(&cache, 42, &config);
        assert_eq!(generated.batches.len(), 2);
        assert!(
            generated
                .batches
                .iter()
                .map(|b| b.instances.len())
                .sum::<usize>()
                > 100
        );
        // Species 7 has occurrence .2 on the dense side and .004 on the sparse side.
        let crossed_candidates = (0..4096)
            .filter(|&slot| {
                let [x, _] = candidate(hash(42, 7, 12), slot);
                let px = x * GRASS_CELL_SIZE + (unit(hash(hash(42, 7, 12), slot, 5)) - 0.5) * 768.0;
                let threshold = unit(hash(hash(42, 7, 12), slot, 4));
                (0.0..GRASS_CELL_SIZE).contains(&px)
                    && x * GRASS_CELL_SIZE % 2048.0 <= 896.0
                    && px % 2048.0 >= 1024.0
                    && (0.004..0.2).contains(&threshold)
            })
            .count();
        assert!(
            crossed_candidates > 5,
            "fixture must distinguish source/final density"
        );
        for batch in &generated.batches {
            for instance in batch.instances.iter() {
                let px = f32::from(instance.position[0]) / 65535.0 * GRASS_CELL_SIZE;
                if px % 2048.0 >= 1024.1 {
                    assert!(
                        unit(hash(
                            hash(42, batch.grass_id, 12),
                            instance.candidate_slot(),
                            4
                        )) < 0.02 * if batch.grass_id == 7 { 0.2 } else { 0.8 },
                        "slot {} survived on the sparse side using its source density",
                        instance.candidate_slot()
                    );
                }
            }
        }
        // Species 7 retains .2 suitability, with no second independent density draw.
        assert!(generated.batches[0].instances.iter().any(|instance| {
            let px = f32::from(instance.position[0]) / 65535.0 * GRASS_CELL_SIZE;
            px % 2048.0 <= 895.9 && unit(hash(hash(42, 7, 12), instance.candidate_slot(), 4)) > 0.15
        }));
    }
}
