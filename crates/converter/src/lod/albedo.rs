//! Offline LAND diffuse blending. The bake matches `engine/shaders/terrain.wgsl`.

use super::terrain::TerrainCellInput;
use crate::{
    asset_path::{AssetKind, canonical_asset_path},
    cache::{hash_bytes, hash_file},
    texture::{TextureConverter, TextureEncoding},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use ddsfile::{Caps2, Dds};
use image_dds::image::{
    ImageBuffer, Rgb,
    imageops::{FilterType, resize},
};
use rusqlite::Connection;
use shared::{TerrainLayer, lod::LodTier};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    path::{Path, PathBuf},
};

/// The landscape texture Skyrim paints where a quadrant names no texture of its own, as a
/// Bethesda path. A null `BTXT` falls back to `dirt02.dds`, which Skyrim.ini's
/// `[Landscape] sDefaultLandDiffuseTexture` also names.
const DEFAULT_LAND_DIFFUSE: &str = "textures/landscape/Dirt02.dds";

/// The key the default texture is stored under. Form id 0 is a null reference, so no landscape
/// texture record can collide with it.
const DEFAULT_TEXTURE_KEY: u32 = 0;

const GUTTER: usize = 2;
const WEIGHT_SIDE: usize = 17;
const QUADRANT_ORIGINS: [(usize, usize); 4] = [(0, 0), (16, 0), (0, 16), (16, 16)];

fn diffuse_catalog(connection: &Connection) -> Result<BTreeMap<u32, String>> {
    connection.prepare(
        "SELECT l.id, t.diffuse_path FROM landscape_textures l JOIN texture_sets t ON t.id=l.texture_set_id \
         WHERE t.diffuse_path IS NOT NULL AND t.diffuse_path <> '' ORDER BY l.id"
    )?.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .map(|row| {
            let (id, path): (u32, String) = row?;
            Ok((id, canonical_asset_path(&path, AssetKind::Texture, "dds")?))
        }).collect()
}

fn default_diffuse_path() -> Result<String> {
    canonical_asset_path(DEFAULT_LAND_DIFFUSE, AssetKind::Texture, "dds")
}

/// Every diffuse a terrain bake may read: each landscape texture's and the default one, so a
/// conversion extracts the default even when no LTEX record names it.
pub(crate) fn terrain_diffuse_paths(connection: &Connection) -> Result<BTreeSet<PathBuf>> {
    let mut paths: BTreeSet<_> = diffuse_catalog(connection)?
        .into_values()
        .map(PathBuf::from)
        .collect();
    paths.insert(PathBuf::from(default_diffuse_path()?));
    Ok(paths)
}

/// Whether a quadrant is painted with the default texture: it has no layers at all, or its base
/// layer names no texture (the converter writes a null `BTXT` as form id 0).
fn quadrant_uses_default(cell: &TerrainCellInput, quadrant: u8) -> bool {
    let mut layers = cell
        .layers
        .iter()
        .filter(|layer| layer.quadrant == quadrant);
    match layers.clone().next() {
        None => true,
        Some(_) => layers.any(|layer| layer.is_base && layer.texture_form_id == 0),
    }
}

#[derive(Default)]
pub struct TerrainTextures {
    textures: BTreeMap<u32, [LinearImage; 3]>,
    /// Exact winning DDS bytes, included in the LOD build identity.
    pub source_hashes: BTreeMap<String, String>,
}

impl TerrainTextures {
    pub fn load(connection: &Connection, vfs: &Path, cells: &[TerrainCellInput]) -> Result<Self> {
        let catalog = diffuse_catalog(connection)?;
        let mut ids: BTreeSet<_> = cells
            .iter()
            .flat_map(|cell| &cell.layers)
            .map(|layer| layer.texture_form_id)
            .filter(|id| *id != DEFAULT_TEXTURE_KEY)
            .collect();
        if cells
            .iter()
            .any(|cell| (0..4).any(|quadrant| quadrant_uses_default(cell, quadrant)))
        {
            ids.insert(DEFAULT_TEXTURE_KEY);
        }
        let mut decoded = BTreeMap::new();
        let mut result = Self::default();
        for id in ids {
            let path = if id == DEFAULT_TEXTURE_KEY {
                default_diffuse_path()?
            } else {
                catalog
                    .get(&id)
                    .ok_or_else(|| {
                        color_eyre::eyre::eyre!("LAND texture {id:08X} has no diffuse image")
                    })?
                    .clone()
            };
            let path = &path;
            if !decoded.contains_key(path) {
                let bytes = std::fs::read(vfs.join(path))
                    .wrap_err_with(|| format!("missing terrain diffuse {path}"))?;
                let dds = Dds::read(Cursor::new(&bytes))
                    .wrap_err_with(|| format!("invalid terrain DDS {path}"))?;
                ensure!(
                    dds.get_depth() == 1
                        && dds.get_num_array_layers() == 1
                        && !dds.header.caps2.contains(Caps2::CUBEMAP),
                    "terrain diffuse {path} must be a 2D image"
                );
                let images = [32, 16, 8].map(|size| LinearImage::decode(&dds, size));
                let [a, b, c] = images;
                decoded.insert(path.clone(), [a?, b?, c?]);
                result
                    .source_hashes
                    .insert(path.clone(), hash_bytes(&bytes));
            }
            result.textures.insert(id, decoded[path].clone());
        }
        Ok(result)
    }

    /// Textures holding only a flat default, for tests whose cells carry no layers.
    #[cfg(test)]
    pub(crate) fn with_flat_default(color: [f32; 3]) -> Self {
        let mut result = Self::default();
        result.textures.insert(
            DEFAULT_TEXTURE_KEY,
            std::array::from_fn(|_| LinearImage {
                width: 1,
                height: 1,
                pixels: vec![color],
            }),
        );
        result
    }

    pub fn verify_sources(&self, vfs: &Path) -> Result<()> {
        for (path, hash) in &self.source_hashes {
            ensure!(
                hash_file(&vfs.join(path))? == *hash,
                "terrain input changed during bake: {path}"
            );
        }
        Ok(())
    }
}

#[derive(Clone)]
struct LinearImage {
    width: usize,
    height: usize,
    pixels: Vec<[f32; 3]>,
}

impl LinearImage {
    fn decode(dds: &Dds, size: u32) -> Result<Self> {
        let mut mip = 0;
        while mip + 1 < dds.get_num_mipmap_levels()
            && (dds.get_width() >> (mip + 1)).max(dds.get_height() >> (mip + 1)) >= size
        {
            mip += 1;
        }
        let surface = image_dds::SurfaceRgba8::decode_layers_mipmaps_dds(dds, 0..1, mip..mip + 1)
            .wrap_err("terrain DDS mip cannot be decoded")?;
        let image = surface
            .get_image(0, 0, 0)
            .ok_or_else(|| color_eyre::eyre::eyre!("terrain DDS mip is truncated"))?;
        let linear =
            ImageBuffer::<Rgb<f32>, Vec<f32>>::from_fn(image.width(), image.height(), |x, y| {
                let pixel = image.get_pixel(x, y);
                Rgb([
                    srgb_to_linear(pixel[0]),
                    srgb_to_linear(pixel[1]),
                    srgb_to_linear(pixel[2]),
                ])
            });
        let image = resize(&linear, size, size, FilterType::Triangle);
        Ok(Self {
            width: image.width() as usize,
            height: image.height() as usize,
            pixels: image.pixels().map(|pixel| pixel.0).collect(),
        })
    }

    fn sample(&self, u: f32, v: f32) -> [f32; 3] {
        // Match normalized repeating GPU sampling, including the half-texel offset.
        let x = u.rem_euclid(1.0) * self.width as f32 - 0.5;
        let y = v.rem_euclid(1.0) * self.height as f32 - 0.5;
        let at = |x: i32, y: i32| {
            self.pixels[y.rem_euclid(self.height as i32) as usize * self.width
                + x.rem_euclid(self.width as i32) as usize]
        };
        let (west, north) = (x.floor() as i32, y.floor() as i32);
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let (a, b, c, d) = (
            at(west, north),
            at(west + 1, north),
            at(west, north + 1),
            at(west + 1, north + 1),
        );
        std::array::from_fn(|channel| {
            mix(
                mix(a[channel], b[channel], fx),
                mix(c[channel], d[channel], fx),
                fy,
            )
        })
    }
}

fn srgb_to_linear(byte: u8) -> f32 {
    let color = f32::from(byte) / 255.0;
    if color <= 0.04045 {
        color / 12.92
    } else {
        ((color + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(color: f32) -> u8 {
    let color = color.clamp(0.0, 1.0);
    let srgb = if color <= 0.0031308 {
        color * 12.92
    } else {
        1.055 * color.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round() as u8
}

fn mix(a: f32, b: f32, weight: f32) -> f32 {
    a + (b - a) * weight
}

struct QuadrantBlend<'a> {
    layers: Vec<&'a TerrainLayer>,
    weights: Vec<[f32; WEIGHT_SIDE * WEIGHT_SIDE]>,
}

impl<'a> QuadrantBlend<'a> {
    fn new(cell: &'a TerrainCellInput, quadrant: u8) -> Result<Self> {
        let mut layers: Vec<_> = cell
            .layers
            .iter()
            .filter(|layer| layer.quadrant == quadrant)
            .collect();
        layers.sort_by_key(|layer| (!layer.is_base, layer.layer, layer.texture_form_id));
        ensure!(
            layers.is_empty()
                || (layers.len() <= 6 && layers.iter().filter(|layer| layer.is_base).count() == 1),
            "LAND {:08X} quadrant {quadrant} must have one base and at most six layers",
            cell.cell_id
        );
        let mut ids = BTreeSet::new();
        let mut weights = Vec::new();
        for layer in layers.iter().filter(|layer| !layer.is_base) {
            ensure!(
                ids.insert(layer.layer),
                "duplicate LAND overlay layer {}",
                layer.layer
            );
            let mut samples = [0.0; WEIGHT_SIDE * WEIGHT_SIDE];
            let mut seen = BTreeSet::new();
            for weight in &layer.weights {
                let index = usize::from(weight.vertex);
                ensure!(
                    index < samples.len()
                        && weight.opacity.is_finite()
                        && (0.0..=1.0).contains(&weight.opacity)
                        && seen.insert(index),
                    "invalid or duplicate LAND opacity in cell {:08X}",
                    cell.cell_id
                );
                samples[index] = weight.opacity;
            }
            weights.push(samples);
        }
        Ok(Self { layers, weights })
    }

    /// The default texture where the quadrant at `origin` is at `(u, v)`, repeated across the cell
    /// like any layer so the default reads as terrain rather than a flat colour.
    fn default_sample(
        &self,
        textures: &TerrainTextures,
        tier: usize,
        origin: (usize, usize),
        u: f32,
        v: f32,
    ) -> Result<[f32; 3]> {
        let image =
            &textures.textures.get(&DEFAULT_TEXTURE_KEY).ok_or_else(|| {
                color_eyre::eyre::eyre!("the default terrain diffuse is not loaded")
            })?[tier];
        let cell_uv = [
            (origin.0 as f32 + u * 16.0) / 32.0,
            (origin.1 as f32 + v * 16.0) / 32.0,
        ];
        Ok(image.sample(
            cell_uv[0] * shared::LAND_TEXTURE_REPEATS_PER_CELL,
            cell_uv[1] * shared::LAND_TEXTURE_REPEATS_PER_CELL,
        ))
    }

    fn sample(
        &self,
        textures: &TerrainTextures,
        tier: usize,
        u: f32,
        v: f32,
        origin: (usize, usize),
    ) -> Result<[f32; 3]> {
        if self.layers.is_empty() {
            return self.default_sample(textures, tier, origin, u, v);
        }
        let mut overlays = [0.0; 5];
        for (weight, grid) in overlays.iter_mut().zip(&self.weights) {
            *weight = sample_grid(grid, u * 16.0, v * 16.0, WEIGHT_SIDE);
        }
        let overlay_sum: f32 = overlays.iter().sum();
        let base = (1.0 - overlay_sum).max(0.0);
        let total = (base + overlay_sum).max(0.0001);
        let cell_uv = [
            (origin.0 as f32 + u * 16.0) / 32.0,
            (origin.1 as f32 + v * 16.0) / 32.0,
        ];
        let mut color = [0.0; 3];
        for (index, layer) in self.layers.iter().enumerate() {
            let weight = if index == 0 {
                base
            } else {
                overlays[index - 1]
            } / total;
            let sample = if layer.is_base && layer.texture_form_id == 0 {
                self.default_sample(textures, tier, origin, u, v)?
            } else {
                textures
                    .textures
                    .get(&layer.texture_form_id)
                    .ok_or_else(|| {
                        color_eyre::eyre::eyre!(
                            "unresolved terrain diffuse {:08X}",
                            layer.texture_form_id
                        )
                    })?[tier]
                    .sample(
                        cell_uv[0] * shared::LAND_TEXTURE_REPEATS_PER_CELL,
                        cell_uv[1] * shared::LAND_TEXTURE_REPEATS_PER_CELL,
                    )
            };
            for channel in 0..3 {
                color[channel] += sample[channel] * weight;
            }
        }
        Ok(color)
    }
}

fn sample_grid(grid: &[f32], x: f32, y: f32, side: usize) -> f32 {
    let x = x.clamp(0.0, (side - 1) as f32);
    let y = y.clamp(0.0, (side - 1) as f32);
    let (west, north) = (x.floor() as usize, y.floor() as usize);
    let (east, south) = ((west + 1).min(side - 1), (north + 1).min(side - 1));
    mix(
        mix(
            grid[north * side + west],
            grid[north * side + east],
            x - west as f32,
        ),
        mix(
            grid[south * side + west],
            grid[south * side + east],
            x - west as f32,
        ),
        y - north as f32,
    )
}

fn terrain_tint(cell: &TerrainCellInput, x: f32, y: f32) -> [f32; 3] {
    if cell.vertex_colors.is_empty() {
        return [1.0; 3];
    }
    // Near terrain linearly interpolates VCLR across its two triangles per grid square.
    let (west, north) = (x.floor().min(31.0) as usize, y.floor().min(31.0) as usize);
    let (fx, fy) = (x - west as f32, y - north as f32);
    let at = |x: usize, y: usize, channel| {
        f32::from(cell.vertex_colors[(y * 33 + x) * 3 + channel]) / 255.0
    };
    std::array::from_fn(|channel| {
        if fx + fy <= 1.0 {
            at(west, north, channel) * (1.0 - fx - fy)
                + at(west + 1, north, channel) * fx
                + at(west, north + 1, channel) * fy
        } else {
            at(west + 1, north + 1, channel) * (fx + fy - 1.0)
                + at(west + 1, north, channel) * (1.0 - fy)
                + at(west, north + 1, channel) * (1.0 - fx)
        }
    })
}

pub(crate) struct TerrainAtlas {
    pub size: usize,
    tile_side: usize,
    tiles_axis: usize,
    rgba: Vec<u8>,
}

impl TerrainAtlas {
    pub fn bake(
        tier: LodTier,
        cells: &[&TerrainCellInput],
        textures: &TerrainTextures,
    ) -> Result<Self> {
        ensure!(!cells.is_empty(), "cannot bake empty terrain atlas");
        let tile_side = 512 / tier.side_cells() as usize;
        let tile_count = cells.len() * 4;
        let mut tiles_axis = 1usize;
        while tiles_axis * tiles_axis < tile_count {
            tiles_axis *= 2;
        }
        let size = tiles_axis * tile_side;
        ensure!(size <= 1024, "terrain atlas exceeds chunk coverage");
        let mut atlas = Self {
            size,
            tile_side,
            tiles_axis,
            rgba: vec![0; size * size * 4],
        };
        let tier_index = match tier {
            LodTier::Tier4 => 0,
            LodTier::Tier8 => 1,
            LodTier::Tier16 => 2,
        };
        let interior = tile_side - 2 * GUTTER;
        for (cell_index, cell) in cells.iter().enumerate() {
            ensure!(
                cell.vertex_colors.is_empty() || cell.vertex_colors.len() == 33 * 33 * 3,
                "invalid LAND tint length in {:08X}",
                cell.cell_id
            );
            for (quadrant, origin) in QUADRANT_ORIGINS.iter().copied().enumerate() {
                let blend = QuadrantBlend::new(cell, quadrant as u8)?;
                let tile = cell_index * 4 + quadrant;
                let (base_x, base_y) = (
                    (tile % tiles_axis) * tile_side,
                    (tile / tiles_axis) * tile_side,
                );
                for y in 0..tile_side {
                    let v =
                        y.saturating_sub(GUTTER).min(interior - 1) as f32 / (interior - 1) as f32;
                    for x in 0..tile_side {
                        let u = x.saturating_sub(GUTTER).min(interior - 1) as f32
                            / (interior - 1) as f32;
                        let diffuse = blend.sample(textures, tier_index, u, v, origin)?;
                        let tint = terrain_tint(
                            cell,
                            origin.0 as f32 + u * 16.0,
                            origin.1 as f32 + v * 16.0,
                        );
                        let offset = ((base_y + y) * size + base_x + x) * 4;
                        for channel in 0..3 {
                            atlas.rgba[offset + channel] =
                                linear_to_srgb(diffuse[channel] * tint[channel]);
                        }
                        atlas.rgba[offset + 3] = 255;
                    }
                }
            }
        }
        Ok(atlas)
    }

    pub fn uv(&self, tile: usize, u: f32, v: f32) -> [f32; 2] {
        let inset = GUTTER as f32 + 0.5;
        let span = (self.tile_side - 2 * GUTTER - 1) as f32;
        [
            ((tile % self.tiles_axis * self.tile_side) as f32 + inset + u * span)
                / self.size as f32,
            ((tile / self.tiles_axis * self.tile_side) as f32 + inset + v * span)
                / self.size as f32,
        ]
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        TextureConverter::encode_rgba_mips(
            self.size as u32,
            self.size as u32,
            &self.mip_chain(),
            TextureEncoding::ColorSrgb,
        )
    }

    fn mip_chain(&self) -> Vec<Vec<u8>> {
        let mut levels = Vec::with_capacity(3);
        levels.push(self.rgba.clone());
        for _ in 1..3 {
            let previous = levels.last().expect("base atlas mip exists");
            let side = (self.size >> (levels.len() - 1)).max(1);
            levels.push(downsample_srgb_rgba(previous, side, side));
        }
        levels
    }
}

fn downsample_srgb_rgba(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let next_width = width / 2;
    let next_height = height / 2;
    let mut output = vec![0; next_width * next_height * 4];
    for y in 0..next_height {
        for x in 0..next_width {
            let samples = [
                ((y * 2) * width + x * 2) * 4,
                ((y * 2) * width + x * 2 + 1) * 4,
                (((y * 2 + 1) * width) + x * 2) * 4,
                (((y * 2 + 1) * width) + x * 2 + 1) * 4,
            ];
            let destination = (y * next_width + x) * 4;
            for channel in 0..3 {
                let linear = samples
                    .iter()
                    .map(|&offset| srgb_to_linear(rgba[offset + channel]))
                    .sum::<f32>()
                    * 0.25;
                output[destination + channel] = linear_to_srgb(linear);
            }
            let alpha = samples
                .iter()
                .map(|&offset| u16::from(rgba[offset + 3]))
                .sum::<u16>();
            output[destination + 3] = ((alpha + 2) / 4) as u8;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::TerrainWeight;

    fn cell(layers: Vec<TerrainLayer>) -> TerrainCellInput {
        TerrainCellInput {
            cell_id: 1,
            grid_x: 0,
            grid_y: 0,
            heights: vec![0.0; 33 * 33],
            vertex_colors: vec![255; 33 * 33 * 3],
            layers,
        }
    }

    fn layer(id: u32, is_base: bool, opacity: f32) -> TerrainLayer {
        TerrainLayer {
            texture_form_id: id,
            quadrant: 0,
            layer: id as u16,
            is_base,
            weights: if is_base {
                vec![]
            } else {
                (0..289)
                    .map(|vertex| TerrainWeight { vertex, opacity })
                    .collect()
            },
        }
    }

    fn textures() -> TerrainTextures {
        let mut result = TerrainTextures::default();
        for (id, color) in [
            (1, [1.0, 0.0, 0.0]),
            (2, [0.0, 0.0, 1.0]),
            (DEFAULT_TEXTURE_KEY, [0.0, 1.0, 0.0]),
        ] {
            result.textures.insert(
                id,
                std::array::from_fn(|_| LinearImage {
                    width: 1,
                    height: 1,
                    pixels: vec![color],
                }),
            );
        }
        result
    }

    #[test]
    fn lod_v9_blends_diffuse_in_linear_light_and_tints_once() {
        let mut source = cell(vec![layer(1, true, 0.0), layer(2, false, 0.5)]);
        source.vertex_colors.fill(128);
        let atlas = TerrainAtlas::bake(LodTier::Tier4, &[&source], &textures()).unwrap();
        let offset = (GUTTER * atlas.size + GUTTER) * 4;
        let expected = linear_to_srgb(0.5 * 128.0 / 255.0);
        assert_eq!(
            &atlas.rgba[offset..offset + 4],
            &[expected, 0, expected, 255]
        );
        assert!(
            expected > 128,
            "sRGB encoding must happen after linear blending"
        );
    }

    #[test]
    fn lod_v9_uses_bilinear_opacity_and_normalizes_overlays() {
        let mut source = cell(vec![layer(1, true, 0.0), layer(2, false, 0.0)]);
        source.layers[1].weights[1].opacity = 1.0;
        let blend = QuadrantBlend::new(&source, 0).unwrap();
        assert_eq!(
            blend
                .sample(&textures(), 0, 0.5 / 16.0, 0.0, (0, 0))
                .unwrap(),
            [0.5, 0.0, 0.5]
        );
        source.layers.push(layer(1, false, 0.75));
        source.layers[1]
            .weights
            .iter_mut()
            .for_each(|weight| weight.opacity = 0.75);
        let blend = QuadrantBlend::new(&source, 0).unwrap();
        assert_eq!(
            blend.sample(&textures(), 0, 0.2, 0.8, (0, 0)).unwrap(),
            [0.5, 0.0, 0.5]
        );
    }

    #[test]
    fn lod_v9_repeats_textures_and_clamps_only_weight_grids() {
        let image = LinearImage {
            width: 2,
            height: 1,
            pixels: vec![[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        };
        assert_eq!(image.sample(0.25, 0.0), image.sample(8.25, 0.0));
        assert_eq!(image.sample(0.0, 0.0), [0.5, 0.0, 0.5]);
        assert_eq!(sample_grid(&[0.0, 1.0, 0.0, 1.0], 2.0, 0.0, 2), 1.0);
    }

    #[test]
    fn lod_v9_quadrant_gutters_and_uvs_keep_tiles_separate() {
        let source = cell(vec![layer(1, true, 0.0)]);
        let atlas = TerrainAtlas::bake(LodTier::Tier4, &[&source], &textures()).unwrap();
        assert_eq!(atlas.size, 256);
        assert_eq!(&atlas.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(
            &atlas.rgba[atlas.tile_side * 4..atlas.tile_side * 4 + 4],
            &[0, 255, 0, 255],
            "a quadrant with no layers is painted with the default texture, not white"
        );
        assert!(atlas.uv(0, 1.0, 1.0)[0] < atlas.uv(1, 0.0, 0.0)[0]);
        assert!(
            TerrainAtlas::bake(LodTier::Tier4, &[&source], &TerrainTextures::default()).is_err()
        );
    }

    fn sample_bilinear(rgba: &[u8], width: usize, uv: [f32; 2]) -> [f32; 4] {
        let x = uv[0] * width as f32 - 0.5;
        let y = uv[1] * width as f32 - 0.5;
        let x0 = x.floor() as isize;
        let y0 = y.floor() as isize;
        let fx = x - x.floor();
        let fy = y - y.floor();
        let pixel = |x: isize, y: isize| -> [f32; 4] {
            let offset = (y as usize * width + x as usize) * 4;
            std::array::from_fn(|channel| f32::from(rgba[offset + channel]))
        };
        let a = pixel(x0, y0);
        let b = pixel(x0 + 1, y0);
        let c = pixel(x0, y0 + 1);
        let d = pixel(x0 + 1, y0 + 1);
        std::array::from_fn(|channel| {
            mix(
                mix(a[channel], b[channel], fx),
                mix(c[channel], d[channel], fx),
                fy,
            )
        })
    }

    #[test]
    fn atlas_mip_chain_stops_before_padded_tiles_can_bleed() {
        let size = 128;
        let tile_side = 32;
        let tiles_axis = 4;
        let colors = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
            [255, 0, 255, 255],
            [0, 255, 255, 255],
            [255, 128, 0, 255],
            [128, 0, 255, 255],
        ];
        let mut rgba = vec![0; size * size * 4];
        for (tile, color) in colors.iter().enumerate() {
            let base_x = tile % tiles_axis * tile_side;
            let base_y = tile / tiles_axis * tile_side;
            for y in 0..tile_side {
                for x in 0..tile_side {
                    let offset = ((base_y + y) * size + base_x + x) * 4;
                    rgba[offset..offset + 4].copy_from_slice(color);
                }
            }
        }
        let atlas = TerrainAtlas {
            size,
            tile_side,
            tiles_axis,
            rgba,
        };
        let mips = atlas.mip_chain();
        assert_eq!(mips.len(), 3);

        for (mip, rgba) in mips.iter().enumerate() {
            let width = size >> mip;
            for (tile, expected) in colors.iter().enumerate() {
                for u in [0.0, 0.02, 0.5, 0.98, 1.0] {
                    for v in [0.0, 0.02, 0.5, 0.98, 1.0] {
                        let uv = atlas.uv(tile, u, v);
                        let actual = sample_bilinear(rgba, width, uv);
                        for channel in 0..4 {
                            assert!(
                                (actual[channel] - f32::from(expected[channel])).abs() <= 1.0,
                                "mip {mip}, tile {tile}, uv ({u}, {v}), channel {channel}: {actual:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn atlas_mips_average_srgb_colors_in_linear_light() {
        let rgba = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let mip = downsample_srgb_rgba(&rgba, 2, 2);
        assert_eq!(mip, [188, 188, 188, 255]);
    }

    #[test]
    fn a_null_base_paints_the_default_texture_where_no_overlay_covers_it() {
        // The converter writes a null BTXT as a base with form id 0. Where the overlay's opacity is
        // 0.5 the other half of the colour is the default texture, not white.
        let source = cell(vec![layer(0, true, 0.0), layer(2, false, 0.5)]);
        let blend = QuadrantBlend::new(&source, 0).unwrap();
        assert_eq!(
            blend.sample(&textures(), 0, 0.5, 0.5, (0, 0)).unwrap(),
            [0.0, 0.5, 0.5]
        );
        assert!(
            blend
                .sample(&TerrainTextures::default(), 0, 0.5, 0.5, (0, 0))
                .is_err(),
            "a missing default texture is an error, not a silent white"
        );
        assert!(quadrant_uses_default(&source, 0));
        assert!(quadrant_uses_default(&source, 1), "no layers at all");
        assert!(!quadrant_uses_default(&cell(vec![layer(1, true, 0.0)]), 0));
    }

    fn texture_database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE texture_sets (id INTEGER PRIMARY KEY, diffuse_path TEXT);
                 CREATE TABLE landscape_textures (id INTEGER PRIMARY KEY, texture_set_id INTEGER);
                 INSERT INTO texture_sets VALUES (1, 'textures/landscape/grass.dds');
                 INSERT INTO landscape_textures VALUES (2, 1);",
            )
            .unwrap();
        connection
    }

    #[test]
    fn the_default_texture_is_extracted_loaded_and_hashed_when_a_quadrant_needs_it() {
        let connection = texture_database();
        let default_path = default_diffuse_path().unwrap();
        assert!(
            terrain_diffuse_paths(&connection)
                .unwrap()
                .contains(&PathBuf::from(&default_path)),
            "the conversion extracts the default even though no LTEX names it"
        );

        let vfs = tempfile::tempdir().unwrap();
        let spec = dummy_content::dds::Spec::new(dummy_content::dds::Format::Bc1Unorm, 64, 64)
            .with_mip_levels(7);
        let bytes =
            dummy_content::dds::generate(&spec, &mut dummy_content::rng::Rng::new(1)).unwrap();
        let file = vfs.path().join(&default_path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &bytes).unwrap();

        let needs_default = cell(vec![layer(0, true, 0.0)]);
        let loaded = TerrainTextures::load(&connection, vfs.path(), &[needs_default]).unwrap();
        assert!(loaded.textures.contains_key(&DEFAULT_TEXTURE_KEY));
        assert_eq!(loaded.source_hashes[&default_path], hash_bytes(&bytes));

        // A cell whose quadrants all name a texture never reads the default, so a data set
        // without it still loads.
        let mut covered = cell(Vec::new());
        covered.layers = (0..4)
            .map(|quadrant| TerrainLayer {
                quadrant,
                ..layer(2, true, 0.0)
            })
            .collect();
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir_all(vfs.path().join("textures/landscape")).unwrap();
        std::fs::write(vfs.path().join("textures/landscape/grass.dds"), &bytes).unwrap();
        let loaded = TerrainTextures::load(&connection, vfs.path(), &[covered]).unwrap();
        assert!(!loaded.textures.contains_key(&DEFAULT_TEXTURE_KEY));

        // And a quadrant that needs it with the file missing fails naming the file.
        let error = TerrainTextures::load(&connection, vfs.path(), &[cell(Vec::new())])
            .err()
            .expect("a missing default texture is an error");
        assert!(format!("{error:#}").contains("dirt02.dds"), "{error:#}");
    }

    #[test]
    fn lod_v9_rejects_invalid_layer_inputs() {
        let mut source = cell(vec![layer(1, true, 0.0), layer(2, false, 0.5)]);
        let duplicate = source.layers[1].weights[0].clone();
        source.layers[1].weights.push(duplicate);
        assert!(TerrainAtlas::bake(LodTier::Tier4, &[&source], &textures()).is_err());
    }
}
