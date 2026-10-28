//! Deterministic CPU placement. All mapped reads and instance allocation run on a worker.
use super::{GrassBatch, GrassInstance, GrassTerrain};
use crate::{config::EngineConfig, world::grass_cache::GrassCache};
use shared::grass::{ArchivedGrassLayer, GRASS_BLEND_SIDE, GRASS_CELL_SIZE, GRASS_EXCLUSION_SIDE};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

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

/// Generate species randomness independently of density and jitter channels.
fn species_random(cell: u32, slot: u32, species: u32) -> f32 {
    // Species IDs are data, not random-channel selectors. Keep this domain separate
    // from density/jitter/scale channels even when a mod chooses a small FormID.
    unit(hash(hash(cell, slot, 10), species, 11))
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

// Apply texture suitability and the type cap at both the candidate and jittered
// position. Cache v6 retains each texture's GNAM order; native Skyrim selection is not
// reproduced by this deterministic placement algorithm.
/// Apply the texture threshold and species cap to sampled layer coverage.
fn layer_weight(layer: &ArchivedGrassLayer, x: f32, y: f32, config: &EngineConfig) -> f32 {
    let types = layer
        .grass_ids
        .len()
        .min(config.grass_max_types_per_texture);
    if types == 0 || layer.weights.len() != GRASS_BLEND_SIDE * GRASS_BLEND_SIDE {
        return 0.0;
    }
    let weight = sample_grid(GRASS_BLEND_SIDE, x, y, |i| layer.weights[i].into());
    if weight < config.grass_texture_threshold {
        0.0
    } else {
        weight / types as f32
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
        terrain.0.heights.len(),
        |index| terrain.0.heights[index],
        terrain.0.water_height,
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
    // Multiple layers can nominate the same species; accumulate their blended suitability.
    let mut weights = BTreeMap::<u32, f32>::new();
    for slot in 0..slots {
        let [x, y] = candidate(cell_id, slot);
        let quadrant = (x * 2.0) as u8 + (y * 2.0) as u8 * 2;
        let Some(mask) = cell.quadrants.iter().find(|q| q.quadrant == quadrant) else {
            continue;
        };
        let qx = (x * 2.0).fract();
        let qy = (y * 2.0).fract();
        let (height, normal, slope) = terrain(&height_at, x, y);
        weights.clear();
        for layer in mask.layers.iter() {
            let per_type = layer_weight(layer, qx, qy, config);
            if per_type <= 0.0 {
                continue;
            }
            for id in layer
                .grass_ids
                .iter()
                .take(config.grass_max_types_per_texture)
            {
                *weights.entry((*id).into()).or_default() += per_type;
            }
        }
        let mut winner = None;
        let mut winning_score = f32::NEG_INFINITY;
        for (&id, &weight) in &weights {
            let Some(species) = cache.species(id) else {
                continue;
            };
            if slope < f32::from(species.min_slope)
                || slope > f32::from(species.max_slope)
                || !water_allowed(
                    height,
                    water,
                    species.units_from_water.into(),
                    species.water_comparison.into(),
                )
            {
                continue;
            }
            let suitability =
                weight * f32::from(species.density) / 100.0 * config.grass_density_multiplier;
            if suitability <= 0.0 {
                continue;
            }
            // Exponential race: highest score wins, weighted by blended texture/density.
            // Its probability is proportional to suitability, without list-order bias.
            let score = suitability / -species_random(cell_id, slot, id).max(1.0e-7).ln();
            if score > winning_score {
                winning_score = score;
                winner = Some((id, species));
            }
        }
        let Some((id, species)) = winner else {
            continue;
        };
        let density: f32 = weights
            .iter()
            .filter_map(|(&id, &w)| cache.species(id).map(|s| (s, w)))
            .filter(|(s, _)| {
                slope >= f32::from(s.min_slope)
                    && slope <= f32::from(s.max_slope)
                    && water_allowed(
                        height,
                        water,
                        s.units_from_water.into(),
                        s.water_comparison.into(),
                    )
            })
            .map(|(s, w)| w * f32::from(s.density) / 100.0)
            .sum();
        if unit(hash(cell_id, slot, 4)) >= (density * config.grass_density_multiplier).min(1.0) {
            continue;
        }
        let position_range: f32 = species.position_range.into();
        // Match the precached placement's total jitter span; range is not a radius.
        let px = x * GRASS_CELL_SIZE + (unit(hash(cell_id, slot, 5)) - 0.5) * position_range;
        let py = y * GRASS_CELL_SIZE + (unit(hash(cell_id, slot, 6)) - 0.5) * position_range;
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
                    .take(config.grass_max_types_per_texture)
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
        if position_range != 0.0 {
            // Reuse the original occurrence threshold at the final position. Sum
            // every eligible species, rather than applying the winner's fraction
            // again: competition has already accounted for that fraction.
            let final_density: f32 = jmask
                .layers
                .iter()
                .filter(|layer| !layer.grass_ids.is_empty())
                .map(|layer| {
                    let weight =
                        layer_weight(layer, (jx * 2.0).fract(), (jy * 2.0).fract(), config);
                    layer
                        .grass_ids
                        .iter()
                        .take(config.grass_max_types_per_texture)
                        .filter_map(|id| cache.species((*id).into()))
                        .filter(|rule| {
                            jslope >= f32::from(rule.min_slope)
                                && jslope <= f32::from(rule.max_slope)
                                && water_allowed(
                                    jh,
                                    water,
                                    rule.units_from_water.into(),
                                    rule.water_comparison.into(),
                                )
                        })
                        .map(|rule| weight * f32::from(rule.density) / 100.0)
                        .sum::<f32>()
                })
                .sum();
            if unit(hash(cell_id, slot, 4))
                >= (final_density * config.grass_density_multiplier).min(1.0)
            {
                continue;
            }
        }
        // iMinGrassSize controls candidate density. The reference precache permits
        // nearby clumps; a second minimum-distance gate would thin density again.
        let height_range: f32 = species.height_range.into();
        // Runtime-only variation: cached species rules and placement are unchanged.
        let color_range = f32::from(species.color_range).clamp(0.0, 1.0);
        instances.entry(id).or_default().push(GrassInstance {
            position: [
                quantize(px, GRASS_CELL_SIZE),
                quantize(py, GRASS_CELL_SIZE),
                quantize(jh - height_min, height_span),
            ],
            rotation: (hash(cell_id, slot, 7) & 255) as u8,
            scale: ((1.0 + (unit(hash(cell_id, slot, 8)) * 2.0 - 1.0) * height_range)
                .clamp(0.0, 2.0)
                * 127.5)
                .round() as u8,
            color: ((1.0 - unit(hash(cell_id, slot, 9)) * color_range) * 255.0).round() as u8,
            normal: jnormal,
            slot,
        });
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
    /// LReachGrass01's authored first two links win the cap despite descending FormIDs.
    fn reach_gnam_order_selects_first_two_species() {
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
            (1, vec![342275]),
            (2, vec![341137, 342275]),
            (3, vec![175315, 341137, 342275]),
            (0, vec![]),
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
            (0.03, 0.97),
            (0.3, 0.7),
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
                grass_max_types_per_texture: 3,
                ..config.clone()
            },
        );
        assert_eq!(uncapped.batches.len(), 3);
        let disabled = generate(
            &cache,
            42,
            &EngineConfig {
                grass_max_types_per_texture: 0,
                ..config
            },
        );
        assert!(disabled.batches.is_empty());
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
                .all(|p| p[0].slot < p[1].slot)
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
    /// Verify blended layers and many species compete without list order bias.
    fn blended_layers_and_many_species_compete_without_list_order_bias() {
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
        assert!(counts[0].1 > counts[1].1 * 4, "{counts:?}");
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
            let original = candidate(42, instance.slot);
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
        // In each quadrant, x <= 896 has total density 0.5 and x >= 1024
        // has total density 0.01. This band stays painted on both sides.
        let crossed_candidates = (0..4096)
            .filter(|&slot| {
                let [x, _] = candidate(42, slot);
                let px = x * GRASS_CELL_SIZE + (unit(hash(42, slot, 5)) - 0.5) * 768.0;
                let threshold = unit(hash(42, slot, 4));
                (0.0..GRASS_CELL_SIZE).contains(&px)
                    && x * GRASS_CELL_SIZE % 2048.0 <= 896.0
                    && px % 2048.0 >= 1024.0
                    && (0.01..0.5).contains(&threshold)
            })
            .count();
        assert!(
            crossed_candidates > 30,
            "fixture must distinguish source/final density"
        );
        for batch in &generated.batches {
            for instance in batch.instances.iter() {
                let px = f32::from(instance.position[0]) / 65535.0 * GRASS_CELL_SIZE;
                if px % 2048.0 >= 1024.1 {
                    assert!(
                        unit(hash(42, instance.slot, 4)) < 0.01,
                        "slot {} survived on the sparse side using its source density",
                        instance.slot
                    );
                }
            }
        }
        // Species 7 receives only 0.1 individual suitability on the dense side;
        // occurrence must still use both species' combined 0.5 density.
        assert!(generated.batches[0].instances.iter().any(|instance| {
            let px = f32::from(instance.position[0]) / 65535.0 * GRASS_CELL_SIZE;
            px % 2048.0 <= 895.9 && unit(hash(42, instance.slot, 4)) > 0.15
        }));
    }
}
