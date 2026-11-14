//! Per-spot grass selection follows spud (MIT, chasmlol/spud), revision e5f2de4.
//! Rust implementation and packed renderer adaptation; notice: grass/SPUD-MIT.txt.
use super::{GrassBatch, GrassInstance, GrassTerrain};
use crate::{config::EngineConfig, world::grass_cache::GrassCache};
use shared::grass::{ArchivedGrassQuadrant, GRASS_CELL_SIZE, GRASS_EXCLUSION_SIDE};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

#[path = "random.rs"]
mod random;
#[path = "sampling.rs"]
mod sampling;
use random::{CellRandom, signed};

/// The historical majority-support experiment is not part of per-spot selection.
pub const MAJORITY_GRASS_SUPPORT: bool = false;
/// An independent spatial fade rank fits the renderer's existing 18-bit contract.
const FADE_SLOTS: u32 = 1 << 18;

pub struct GeneratedGrass {
    pub batches: Vec<GrassBatch>,
    pub generation_micros: u64,
    pub bytes: usize,
}

struct Entry {
    id: u32,
    is_base: bool,
    weights: [f32; 9],
}

/// Preserve GNAM and layer order, including repeated nominations and zero-support base entries.
fn entries(
    quadrant: &ArchivedGrassQuadrant,
    x: usize,
    y: usize,
    config: &EngineConfig,
) -> Vec<Entry> {
    let mut result = Vec::with_capacity(16);
    let center = y * 17 + x;
    let nine = [
        center - 18,
        center - 17,
        center - 16,
        center - 1,
        center,
        center + 1,
        center + 16,
        center + 17,
        center + 18,
    ];
    for layer in quadrant.layers.iter() {
        let support = nine.map(|i| f32::from(layer.weights[i]));
        if !layer.is_base
            && !support
                .iter()
                .any(|weight| *weight > 25.0 / 255.0 && *weight >= config.grass_texture_threshold)
        {
            continue;
        }
        for id in layer
            .grass_ids
            .iter()
            .take(config.effective_grass_species_cap())
        {
            if result.len() == 16 {
                return result;
            }
            result.push(Entry {
                id: (*id).into(),
                is_base: layer.is_base,
                weights: support.map(|weight| if weight > 0.0 { 1.0 } else { 0.0 }),
            });
        }
    }
    result
}

/// Bilinear binary-presence support at a pre-jitter subgrid center within the 3x3 spot.
fn support(weights: &[f32; 9], x: f32, y: f32) -> f32 {
    let col = x as usize;
    let row = y as usize;
    let fx = x - col as f32;
    let fy = y - row as f32;
    let i = col + 3 * row;
    ((1.0 - fx) * weights[i] + weights[i + 1] * fx) * (1.0 - fy)
        + ((1.0 - fx) * weights[i + 3] + weights[i + 4] * fx) * fy
}

/// Water modes follow the reference, including its no-water sentinel and unconstrained modes 6/7.
pub fn water_allowed(height: f32, water: Option<f32>, distance: u16, mode: u32) -> bool {
    let w = water.unwrap_or(-f32::MAX);
    let u = f32::from(distance);
    match mode {
        0 => height >= w - u,
        1 => w <= height && height <= w + u,
        2 => height <= w - u,
        3 => w - u <= height && height <= w,
        4 => height >= w + u || height <= w - u,
        5 => w - u <= height && height <= w + u,
        _ => true,
    }
}

/// The authored range bounds grid spacing; zero range uses the configured minimum safely.
fn grid_side(position_range: f32, minimum: f32) -> usize {
    if position_range < 0.0 {
        return 0;
    }
    let authored = if position_range > 0.0 {
        (256.0 / position_range) as usize
    } else {
        usize::MAX
    };
    authored.min((256.0 / minimum) as usize)
}

/// Encode the range relative to a bounded batch span.
fn quantize(value: f32, span: f32) -> u16 {
    (value / span * 65535.0).round().clamp(0.0, 65535.0) as u16
}

/// Keep distance thinning spatially distributed without advancing the placement random stream.
fn fade_rank(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    (value ^ (value >> 16)) & (FADE_SLOTS - 1)
}

/// Apply the reference brightness range before adapting binary16 intensity to our byte contract.
fn brightness(luma: f32, range: f32, word: u32) -> (u8, f32) {
    let factor = (word as f32 * 1.164_153_2e-10 - 0.25) * range + (1.0 - range);
    let value = luma * factor;
    let value = if value >= 1.0 { 0.99 } else { value.max(0.0) };
    (
        (sampling::half_round(value) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8,
        factor,
    )
}

/// Adapt the reference sine/cosine orientation to the existing byte-angle contract.
fn rotation(word: u32, fit_to_slope: bool) -> u8 {
    let sine = signed(word).clamp(-1.0, 1.0);
    let cosine = (1.0 - sine * sine).max(0.0).sqrt();
    let angle = if fit_to_slope {
        (-sine).atan2(cosine)
    } else {
        cosine.atan2(sine)
    };
    ((angle.rem_euclid(std::f32::consts::TAU) * (256.0 / std::f32::consts::TAU)).round() as u32
        & 255) as u8
}

/// Generate using the baked original LAND fields (also usable for CPU species censuses).
pub fn generate(cache: &GrassCache, cell_id: u32, config: &EngineConfig) -> GeneratedGrass {
    let Some(cell) = cache.cell(cell_id) else {
        return empty();
    };
    generate_field(
        cache,
        cell_id,
        config,
        cell.heights.len(),
        |i| cell.heights[i].into(),
        cell.water_height.as_ref().map(|h| (*h).into()),
        None,
        &mut |_| {},
    )
}

/// A production selection event, before the optional obstruction rejection and renderer packing.
/// Appearance words are retained so CPU diagnostics can decode the reference frame exactly.
pub struct Placement {
    pub form_id: u32,
    pub is_base: bool,
    pub obstructed: bool,
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub luma: f32,
    pub appearance: [u32; 3],
    pub instance: GrassInstance,
    pub height_min: f32,
    pub height_span: f32,
}

/// Observe the same baked-LAND production generator without starting Bevy or a renderer.
pub fn generate_observed(
    cache: &GrassCache,
    cell_id: u32,
    config: &EngineConfig,
    mut observe: impl FnMut(Placement),
) -> GeneratedGrass {
    let Some(cell) = cache.cell(cell_id) else {
        return empty();
    };
    generate_field(
        cache,
        cell_id,
        config,
        cell.heights.len(),
        |i| cell.heights[i].into(),
        cell.water_height.as_ref().map(|h| (*h).into()),
        None,
        &mut observe,
    )
}

/// Keep committed terrain lifetime checks while sampling its selected height and colour fields.
pub(super) fn generate_on_terrain(
    cache: &GrassCache,
    cell_id: u32,
    config: &EngineConfig,
    terrain: &GrassTerrain,
) -> GeneratedGrass {
    let white = [1.0; 1089];
    generate_field(
        cache,
        cell_id,
        config,
        terrain.heights().len(),
        |i| terrain.heights()[i],
        terrain.0.water_height,
        Some(terrain.luminance().unwrap_or(&white)),
        &mut |_| {},
    )
}

/// A rejected or absent cell leaves the world usable without grass batches.
fn empty() -> GeneratedGrass {
    GeneratedGrass {
        batches: vec![],
        generation_micros: 0,
        bytes: 0,
    }
}

/// Roll entries in quadrant / odd-X / odd-Y / entry / grid-X / grid-Y order.
#[expect(
    clippy::too_many_arguments,
    reason = "observer extends the existing surface sampling inputs without changing their behavior"
)]
fn generate_field(
    cache: &GrassCache,
    cell_id: u32,
    config: &EngineConfig,
    height_count: usize,
    height_at: impl Fn(usize) -> f32,
    water: Option<f32>,
    luminance: Option<&[f32]>,
    observe: &mut impl FnMut(Placement),
) -> GeneratedGrass {
    let start = Instant::now();
    let mut result = empty();
    let Some(cell) = cache.cell(cell_id) else {
        return result;
    };
    if config.validate_grass().is_err()
        || height_count != 1089
        || (0..height_count).any(|i| !height_at(i).is_finite())
    {
        return result;
    }
    let height_min = (0..height_count)
        .map(&height_at)
        .fold(f32::INFINITY, f32::min);
    let height_max = (0..height_count)
        .map(&height_at)
        .fold(f32::NEG_INFINITY, f32::max);
    let height_span = (height_max - height_min).max(1.0);
    let cx = i32::from(cell.grid_x);
    let cy = i32::from(cell.grid_y);
    let cell_origin = [cx as f32 * 4096.0, cy as f32 * 4096.0];
    let block_origin = [
        (cx / 12 * 12) as f32 * 4096.0,
        (cy / 12 * 12) as f32 * 4096.0,
    ];
    let mut rng = CellRandom::new(cx.wrapping_mul(cy) as u32);
    let mut instances: BTreeMap<u32, Vec<GrassInstance>> = BTreeMap::new();
    let mut ordinal = 0u32;
    for quadrant in cell.quadrants.iter() {
        let q = usize::from(quadrant.quadrant);
        for x in (1..16).step_by(2) {
            for y in (1..16).step_by(2) {
                let origin = [
                    (x * 128 + (q & 1) * 2048) as f32 + cell_origin[0] - 128.0,
                    (y * 128 + (q >> 1) * 2048) as f32 + cell_origin[1] - 128.0,
                ];
                for entry in entries(quadrant, x, y, config) {
                    let Some(rule) = cache.species(entry.id) else {
                        continue;
                    };
                    let n = grid_side(rule.position_range.into(), config.grass_min_spacing);
                    if n == 0 {
                        continue;
                    }
                    let inv = 1.0 / n as f32;
                    let half = 256.0 * inv * 0.5;
                    let two = inv + inv;
                    for j in 0..n {
                        let jf = j as f32 + 0.5;
                        let xoff = (half + half) * jf;
                        for i in 0..n {
                            let identity = ordinal;
                            ordinal = ordinal.wrapping_add(1);
                            let if_ = i as f32 + 0.5;
                            let density = f32::from(rule.density) * 0.01;
                            let chance = (support(
                                &entry.weights.map(|weight| weight * density),
                                two * jf,
                                two * if_,
                            ) * 32768.0
                                * config.grass_density_multiplier)
                                as i32;
                            if chance == 0 || (rng.next() & 0x7fff) as i32 >= chance {
                                continue;
                            }
                            let u1 = rng.next();
                            let u2 = rng.next();
                            let px = block_origin[0]
                                + sampling::half_round(
                                    (signed(u1) * half + (xoff + origin[0])) - block_origin[0],
                                )
                                - cell_origin[0];
                            let py = block_origin[1]
                                + sampling::half_round(
                                    (signed(u2) * half + ((if_ * half) + (if_ * half) + origin[1]))
                                        - block_origin[1],
                                )
                                - cell_origin[1];
                            let normal_at = |vertex: usize| {
                                (cell.normals.len() == 3267).then(|| {
                                    std::array::from_fn(|axis| {
                                        f32::from(cell.normals[vertex * 3 + axis]) * 0.007_874_016
                                    })
                                })
                            };
                            let Some(sample) = sampling::sample(px, py, &height_at, &normal_at)
                            else {
                                continue;
                            };
                            if !water_allowed(
                                sample.height,
                                water,
                                rule.units_from_water.into(),
                                rule.water_comparison.into(),
                            ) || !sampling::slope_allowed(
                                sample.normal[2],
                                rule.min_slope,
                                rule.max_slope,
                            ) {
                                continue;
                            }
                            // Preserve the reference stream: appearance rolls happen after LAND filters,
                            // even when MudCrab's separately baked obstruction mask rejects this blade.
                            let u3 = rng.next();
                            let u4 = rng.next();
                            let u5 = rng.next();
                            let qx = ((px / 2048.0) as usize).min(1);
                            let qy = ((py / 2048.0) as usize).min(1);
                            let mask = &cell.quadrants[qx + 2 * qy];
                            let bit_x = (((px - qx as f32 * 2048.0) / 32.0) as usize).min(63);
                            let bit_y = (((py - qy as f32 * 2048.0) / 32.0) as usize).min(63);
                            let bit = bit_y * GRASS_EXCLUSION_SIDE + bit_x;
                            let obstructed = mask.exclusion[bit / 8] & (1 << (bit % 8)) != 0;
                            let luma = luminance
                                .filter(|field| field.len() == 1089)
                                .map(|field| field[sample.nearest])
                                .unwrap_or_else(|| {
                                    if cell.vertex_colors.len() == 3267 {
                                        let i = sample.nearest * 3;
                                        (f32::from(cell.vertex_colors[i]) * 0.003_921_569 * 0.31
                                            + f32::from(cell.vertex_colors[i + 1])
                                                * 0.003_921_569
                                                * 0.37)
                                            + f32::from(cell.vertex_colors[i + 2])
                                                * 0.003_921_569
                                                * 0.32
                                    } else {
                                        1.0
                                    }
                                });
                            let (color, factor) = brightness(luma, rule.color_range.into(), u3);
                            let range: f32 = rule.height_range.into();
                            let instance = GrassInstance {
                                position: [
                                    quantize(px, GRASS_CELL_SIZE),
                                    quantize(py, GRASS_CELL_SIZE),
                                    quantize(
                                        sampling::half_round(sample.height) - height_min,
                                        height_span,
                                    ),
                                ],
                                rotation: rotation(u4, rule.flags & 4 != 0),
                                scale: ((1.0 + signed(u5) * range).clamp(0.0, 2.0) * 127.5).round()
                                    as u8,
                                color,
                                normal: sample.normal.map(|value| (value * 127.0).round() as i8),
                                slot: fade_rank(identity)
                                    | ((luma.clamp(0.0, 1.0) * 127.0).round() as u32) << 18
                                    | ((((factor.clamp(-1.0, 1.0) + 1.0) * 63.5).round() as u32)
                                        << 25),
                            };
                            observe(Placement {
                                form_id: entry.id,
                                is_base: entry.is_base,
                                obstructed,
                                position: [
                                    cell_origin[0] + px,
                                    cell_origin[1] + py,
                                    sampling::half_round(sample.height),
                                ],
                                normal: sample.normal,
                                luma,
                                appearance: [u3, u4, u5],
                                instance,
                                height_min,
                                height_span,
                            });
                            if !obstructed {
                                instances.entry(entry.id).or_default().push(instance);
                            }
                        }
                    }
                }
            }
        }
    }
    for (id, mut values) in instances {
        values.sort_by_key(GrassInstance::candidate_slot);
        let rule = cache.species(id).expect("generated species is mapped");
        result.bytes += values.capacity() * std::mem::size_of::<GrassInstance>()
            + rule.model_path.len()
            + std::mem::size_of::<Vec<GrassInstance>>()
            + 2 * std::mem::size_of::<usize>();
        result.batches.push(GrassBatch {
            cell_id,
            grid: [cx, cy],
            grass_id: id,
            model_path: rule.model_path.as_str().to_owned(),
            instances: Arc::new(values),
            height_min,
            height_span,
            slot_count: FADE_SLOTS,
            wave_period: rule.wave_period.into(),
            flags: rule.flags,
        });
    }
    result.bytes += result.batches.capacity() * std::mem::size_of::<GrassBatch>()
        + std::mem::size_of::<GeneratedGrass>();
    result.generation_micros = start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    result
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
