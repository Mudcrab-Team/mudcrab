//! Offline LAND diffuse blending. The bake matches `engine/shaders/terrain.wgsl`.

use super::terrain::TerrainCellInput;
use crate::{
    asset_path::{AssetKind, canonical_asset_path},
    cache::{hash_bytes, hash_file},
    texture::{TextureConverter, TextureEncoding},
    texture_gpu::{EncodedTexture, GpuUastc, PreparedTexture},
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
use rayon::prelude::*;
use rusqlite::Connection;
use shared::{TerrainLayer, lod::LodTier};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

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

pub(crate) fn terrain_diffuse_paths(connection: &Connection) -> Result<BTreeSet<PathBuf>> {
    Ok(diffuse_catalog(connection)?
        .into_values()
        .map(PathBuf::from)
        .collect())
}

#[derive(Default)]
pub struct TerrainTextures {
    textures: BTreeMap<u32, Arc<[LinearImage; 3]>>,
    sources: BTreeMap<u32, (String, String)>,
    /// Exact winning DDS bytes, included in the LOD build identity.
    pub source_hashes: BTreeMap<String, String>,
}

/// Stage-owned decoded images, shared by every world and FormID using a path.
#[derive(Default)]
pub(crate) struct TerrainTextureCache {
    decoded: BTreeMap<String, (Arc<[LinearImage; 3]>, String)>,
}

impl TerrainTextures {
    pub fn load(connection: &Connection, vfs: &Path, cells: &[TerrainCellInput]) -> Result<Self> {
        Self::load_with_cache(connection, vfs, cells, &mut TerrainTextureCache::default())
    }

    pub(crate) fn load_with_cache(
        connection: &Connection,
        vfs: &Path,
        cells: &[TerrainCellInput],
        cache: &mut TerrainTextureCache,
    ) -> Result<Self> {
        let catalog = diffuse_catalog(connection)?;
        let ids: BTreeSet<_> = cells
            .iter()
            .flat_map(|cell| &cell.layers)
            .map(|layer| layer.texture_form_id)
            .filter(|id| *id != 0)
            .collect();
        let mut result = Self::default();
        for id in ids {
            let path = catalog.get(&id).ok_or_else(|| {
                color_eyre::eyre::eyre!("LAND texture {id:08X} has no diffuse image")
            })?;
            if !cache.decoded.contains_key(path) {
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
                let images = LinearImage::decode_tiers(&dds)?;
                cache
                    .decoded
                    .insert(path.clone(), (Arc::new(images), hash_bytes(&bytes)));
            }
            let (images, hash) = &cache.decoded[path];
            result.textures.insert(id, Arc::clone(images));
            result.source_hashes.insert(path.clone(), hash.clone());
            result.sources.insert(id, (path.clone(), hash.clone()));
        }
        Ok(result)
    }

    pub(crate) fn sources_for(
        &self,
        cells: &[&TerrainCellInput],
    ) -> BTreeMap<u32, &(String, String)> {
        cells
            .iter()
            .flat_map(|cell| &cell.layers)
            .filter_map(|layer| {
                self.sources
                    .get(&layer.texture_form_id)
                    .map(|source| (layer.texture_form_id, source))
            })
            .collect()
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
    fn decode_tiers(dds: &Dds) -> Result<[Self; 3]> {
        Self::decode_tiers_with(dds, |mip| {
            image_dds::SurfaceRgba8::decode_layers_mipmaps_dds(dds, 0..1, mip..mip + 1)
                .wrap_err("terrain DDS mip cannot be decoded")
        })
    }

    fn decode_tiers_with(
        dds: &Dds,
        mut decode: impl FnMut(u32) -> Result<image_dds::SurfaceRgba8<Vec<u8>>>,
    ) -> Result<[Self; 3]> {
        let mut linear_mips = BTreeMap::new();
        let mut images = Vec::with_capacity(3);
        for size in [32, 16, 8] {
            let mut mip = 0;
            while mip + 1 < dds.get_num_mipmap_levels()
                && (dds.get_width() >> (mip + 1)).max(dds.get_height() >> (mip + 1)) >= size
            {
                mip += 1;
            }
            if let std::collections::btree_map::Entry::Vacant(entry) = linear_mips.entry(mip) {
                let surface = decode(mip)?;
                let image = surface
                    .get_image(0, 0, 0)
                    .ok_or_else(|| color_eyre::eyre::eyre!("terrain DDS mip is truncated"))?;
                entry.insert(ImageBuffer::<Rgb<f32>, Vec<f32>>::from_fn(
                    image.width(),
                    image.height(),
                    |x, y| {
                        let pixel = image.get_pixel(x, y);
                        Rgb([
                            srgb_to_linear(pixel[0]),
                            srgb_to_linear(pixel[1]),
                            srgb_to_linear(pixel[2]),
                        ])
                    },
                ));
            }
            let image = resize(&linear_mips[&mip], size, size, FilterType::Triangle);
            images.push(Self {
                width: image.width() as usize,
                height: image.height() as usize,
                pixels: image.pixels().map(|pixel| pixel.0).collect(),
            });
        }
        Ok(images
            .try_into()
            .unwrap_or_else(|_| unreachable!("three tier images")))
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
    static COLORS: OnceLock<[f32; 256]> = OnceLock::new();
    COLORS.get_or_init(|| {
        std::array::from_fn(|byte| {
            let color = byte as f32 / 255.0;
            if color <= 0.04045 {
                color / 12.92
            } else {
                ((color + 0.055) / 1.055).powf(2.4)
            }
        })
    })[usize::from(byte)]
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

    fn sample(
        &self,
        textures: &TerrainTextures,
        tier: usize,
        u: f32,
        v: f32,
        origin: (usize, usize),
    ) -> Result<[f32; 3]> {
        if self.layers.is_empty() {
            return Ok([1.0; 3]);
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
                [1.0; 3]
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

/// The actual encoding is returned so CPU fallback cannot certify GPU reuse.
pub(crate) struct EncodedTerrainAtlas {
    pub bytes: Vec<u8>,
    pub gpu_used: bool,
    pub fallback_reason: Option<String>,
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

    pub fn encode(self) -> Result<Vec<u8>> {
        TextureConverter::encode_rgba_mips(
            self.size as u32,
            self.size as u32,
            &mip_chain(self.size, self.rgba),
            TextureEncoding::ColorSrgb,
        )
    }

    /// Uses the same authored three-mip chain for both encoders. The caller
    /// bounds the atlas batch; each CPU copy is retained until GPU validation
    /// succeeds, so a device failure falls back without rebaking the atlas.
    pub(crate) fn encode_batch(
        atlases: Vec<Self>,
        gpu: Option<&GpuUastc>,
        post_threads: usize,
    ) -> Result<Vec<EncodedTerrainAtlas>> {
        if atlases.is_empty() {
            return Ok(Vec::new());
        }
        match gpu {
            Some(gpu) => encode_atlas_batch_with(
                atlases,
                Some(&|textures| {
                    gpu.encode_prepared_batch(textures, TextureEncoding::ColorSrgb, post_threads)
                }),
            ),
            None => encode_atlas_batch_with(atlases, None),
        }
    }
}

type EncodeAtlasBatch<'a> = dyn Fn(Vec<PreparedTexture>) -> Vec<Result<EncodedTexture>> + 'a;

fn encode_atlas_batch_with(
    atlases: Vec<TerrainAtlas>,
    gpu_encode: Option<&EncodeAtlasBatch<'_>>,
) -> Result<Vec<EncodedTerrainAtlas>> {
    let inputs: Vec<_> = atlases
        .into_par_iter()
        .map(|atlas| (atlas.size as u32, mip_chain(atlas.size, atlas.rgba)))
        .collect();
    let mut gpu_results: Vec<Option<Result<EncodedTexture>>> =
        (0..inputs.len()).map(|_| None).collect();
    if let Some(encode) = gpu_encode {
        let mut prepared = Vec::with_capacity(inputs.len());
        let mut indices = Vec::with_capacity(inputs.len());
        for (index, (size, levels)) in inputs.iter().enumerate() {
            match PreparedTexture::from_rgba_mips(*size, *size, levels) {
                Ok(texture) => {
                    prepared.push(texture);
                    indices.push(index);
                }
                Err(error) => gpu_results[index] = Some(Err(error)),
            }
        }
        let results = encode(prepared);
        if results.len() == indices.len() {
            for (index, result) in indices.into_iter().zip(results) {
                gpu_results[index] = Some(result);
            }
        } else {
            for index in indices {
                gpu_results[index] = Some(Err(color_eyre::eyre::eyre!(
                    "GPU atlas result count changed"
                )));
            }
        }
    }
    inputs
        .into_par_iter()
        .zip(gpu_results)
        .map(|((size, levels), gpu_result)| {
            let gpu_result = gpu_result.map(|result| {
                result.and_then(|encoded| {
                    let metadata =
                        crate::texture::inspect_ktx2(&encoded.bytes, TextureEncoding::ColorSrgb)?;
                    ensure!(
                        metadata.width == size
                            && metadata.height == size
                            && metadata.levels == 3
                            && metadata.faces == 1
                            && metadata.layers <= 1
                            && metadata.depth <= 1,
                        "GPU terrain atlas changed its dimensions or authored mip chain"
                    );
                    Ok(encoded.bytes)
                })
            });
            let fallback_reason = match gpu_result {
                Some(Ok(bytes)) => {
                    return Ok(EncodedTerrainAtlas {
                        bytes,
                        gpu_used: true,
                        fallback_reason: None,
                    });
                }
                Some(Err(error)) => Some(format!("{error:#}")),
                None => None,
            };
            let bytes = TextureConverter::encode_rgba_mips(
                size,
                size,
                &levels,
                TextureEncoding::ColorSrgb,
            )?;
            Ok(EncodedTerrainAtlas {
                bytes,
                gpu_used: false,
                fallback_reason,
            })
        })
        .collect()
}

fn mip_chain(size: usize, rgba: Vec<u8>) -> Vec<Vec<u8>> {
    let mut levels = Vec::with_capacity(3);
    levels.push(rgba);
    for _ in 1..3 {
        let previous = levels.last().expect("base atlas mip exists");
        let side = (size >> (levels.len() - 1)).max(1);
        levels.push(downsample_srgb_rgba(previous, side, side));
    }
    levels
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

    fn tiny_atlas(color: [u8; 4]) -> TerrainAtlas {
        TerrainAtlas {
            size: 8,
            tile_side: 4,
            tiles_axis: 2,
            rgba: color.repeat(64),
        }
    }

    #[test]
    fn gpu_atlas_failure_preserves_cpu_bytes_and_three_authored_mips() {
        let expected = tiny_atlas([23, 45, 67, 255]).encode().unwrap();
        let encoded = encode_atlas_batch_with(
            vec![tiny_atlas([23, 45, 67, 255])],
            Some(&|textures| {
                assert_eq!(textures[0].images.len(), 3);
                vec![Err(color_eyre::eyre::eyre!("injected GPU device loss"))]
            }),
        )
        .unwrap();
        assert_eq!(encoded[0].bytes, expected);
        assert!(!encoded[0].gpu_used);
        assert!(
            encoded[0]
                .fallback_reason
                .as_ref()
                .unwrap()
                .contains("device loss")
        );
        let reader = ktx2::Reader::new(&encoded[0].bytes[..]).unwrap();
        assert_eq!(reader.header().level_count, 3);
        assert_eq!(
            reader.transfer_function(),
            Some(ktx2::TransferFunction::SRGB)
        );
    }

    #[test]
    fn gpu_atlas_layout_error_falls_back_before_publication() {
        let levels = vec![[23, 45, 67, 255].repeat(64), [23, 45, 67, 255].repeat(16)];
        let wrong_mips =
            TextureConverter::encode_rgba_mips(8, 8, &levels, TextureEncoding::ColorSrgb).unwrap();
        let expected = tiny_atlas([23, 45, 67, 255]).encode().unwrap();
        let encoded = encode_atlas_batch_with(
            vec![tiny_atlas([23, 45, 67, 255])],
            Some(&|_| {
                vec![Ok(EncodedTexture {
                    bytes: wrong_mips.clone(),
                    sha256: String::new(),
                })]
            }),
        )
        .unwrap();
        assert_eq!(encoded[0].bytes, expected);
        assert!(!encoded[0].gpu_used);
        assert!(
            encoded[0]
                .fallback_reason
                .as_ref()
                .unwrap()
                .contains("authored mip chain")
        );
    }

    #[test]
    fn atlas_batch_retains_order_and_falls_back_only_failed_results() {
        let first = tiny_atlas([255, 0, 0, 255]).encode().unwrap();
        let second = tiny_atlas([0, 0, 255, 255]).encode().unwrap();
        let encoded = encode_atlas_batch_with(
            vec![tiny_atlas([255, 0, 0, 255]), tiny_atlas([0, 0, 255, 255])],
            Some(&|_| {
                vec![
                    Ok(EncodedTexture {
                        bytes: first.clone(),
                        sha256: String::new(),
                    }),
                    Err(color_eyre::eyre::eyre!("injected dispatch error")),
                ]
            }),
        )
        .unwrap();
        assert_eq!(encoded[0].bytes, first);
        assert!(encoded[0].gpu_used);
        assert_eq!(encoded[1].bytes, second);
        assert!(!encoded[1].gpu_used);
        assert_eq!(
            TerrainAtlas::encode_batch(vec![], None, 1).unwrap().len(),
            0
        );
    }

    fn decode_atlas_mip(bytes: &[u8], mip: usize) -> Vec<u8> {
        use basis_universal::{DecodeFlags, TranscoderBlockFormat, sys};
        let reader = ktx2::Reader::new(bytes).unwrap();
        let header = reader.header();
        assert!(header.supercompression_scheme.is_none());
        let width = (header.pixel_width >> mip).max(1);
        let height = (header.pixel_height >> mip).max(1);
        let data = reader.levels().nth(mip).unwrap().data;
        assert_eq!(
            data.len(),
            (width.div_ceil(4) * height.div_ceil(4) * 16) as usize
        );
        let mut rgba = vec![0; (width * height * 4) as usize];
        basis_universal::transcoder_init();
        // basis-universal 0.3.1's safe wrapper divides RGBA32 row pitch by 4.
        // Pass the correct pixel pitch directly so native unpacking stays in bounds.
        // SAFETY: the initialized transcoder is owned for this call. The input
        // contains every expected 16-byte block, and the output holds width *
        // height pixels with a four-byte stride and a width-pixel row pitch.
        let decoded = unsafe {
            let transcoder = sys::low_level_uastc_transcoder_new();
            assert!(!transcoder.is_null());
            let decoded = sys::low_level_uastc_transcoder_transcode_slice(
                transcoder,
                rgba.as_mut_ptr().cast(),
                width.div_ceil(4),
                height.div_ceil(4),
                data.as_ptr(),
                data.len().try_into().unwrap(),
                TranscoderBlockFormat::RGBA32.into(),
                4,
                false,
                true,
                width,
                height,
                width,
                std::ptr::null_mut(),
                height,
                0,
                3,
                DecodeFlags::HIGH_QUALITY.bits(),
            );
            sys::low_level_uastc_transcoder_delete(transcoder);
            decoded
        };
        assert!(decoded, "invalid UASTC blocks in atlas mip {mip}");
        rgba
    }

    #[test]
    fn atlas_quality_decoder_uses_full_pixel_rows() {
        let color = [21, 45, 87, 255];
        let bytes = tiny_atlas(color).encode().unwrap();
        for mip in 0..3 {
            let side = 8 >> mip;
            let decoded = decode_atlas_mip(&bytes, mip);
            assert_eq!(decoded.len(), side * side * 4);
            assert!(rgb_rmse(&color.repeat(side * side), &decoded) <= 1.0);
            assert!(
                decoded
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[3] == 255)
            );
        }
    }

    fn rgb_rmse(expected: &[u8], actual: &[u8]) -> f64 {
        assert_eq!(expected.len(), actual.len());
        let squared_error: f64 = expected
            .as_chunks::<4>()
            .0
            .iter()
            .zip(actual.as_chunks::<4>().0.iter())
            .flat_map(|(expected, actual)| {
                (0..3).map(move |channel| {
                    (f64::from(expected[channel]) - f64::from(actual[channel])).powi(2)
                })
            })
            .sum();
        (squared_error / (expected.len() / 4 * 3) as f64).sqrt()
    }

    #[test]
    #[ignore = "requires an idle hardware GPU; run with --ignored --nocapture"]
    fn gpu_atlas_quality_preserves_mips_gutters_padding_and_reusable_slots() {
        let mut inputs: Vec<_> = (0..3)
            .map(|index| {
                let mut input = cell(Vec::new());
                input.cell_id = index + 1;
                input.grid_x = index as i32;
                input.vertex_colors = (0..33 * 33)
                    .flat_map(|vertex| {
                        let x = vertex % 33;
                        let y = vertex / 33;
                        [64 + x as u8 * 5, 72 + y as u8 * 4, 192]
                    })
                    .collect();
                for quadrant in 0..4 {
                    let mut base = layer(1, true, 0.0);
                    base.quadrant = quadrant;
                    let mut overlay = layer(2, false, 0.0);
                    overlay.quadrant = quadrant;
                    for weight in &mut overlay.weights {
                        weight.opacity = f32::from(weight.vertex % 17) / 16.0;
                    }
                    input.layers.extend([base, overlay]);
                }
                input
            })
            .collect();
        inputs[1].vertex_colors.reverse();
        let bake = || {
            TerrainAtlas::bake(
                LodTier::Tier16,
                &inputs.iter().collect::<Vec<_>>(),
                &textures(),
            )
            .unwrap()
        };
        let atlas = bake();
        let size = atlas.size;
        let tile_side = atlas.tile_side;
        let levels = mip_chain(size, atlas.rgba.clone());
        let cpu = TextureConverter::encode_rgba_mips(
            size as u32,
            size as u32,
            &levels,
            TextureEncoding::ColorSrgb,
        )
        .unwrap();
        let gpu = GpuUastc::new(crate::texture_gpu::DEFAULT_QUALITY, 1).unwrap();
        let encoded = TerrainAtlas::encode_batch(vec![bake()], Some(&gpu), 1).unwrap();
        assert!(encoded[0].gpu_used, "{:?}", encoded[0].fallback_reason);
        let gpu_bytes = &encoded[0].bytes;
        let reader = ktx2::Reader::new(&gpu_bytes[..]).unwrap();
        assert_eq!(reader.header().level_count, 3);
        assert_eq!(
            reader.transfer_function(),
            Some(ktx2::TransferFunction::SRGB)
        );
        for (mip, expected) in levels.iter().enumerate() {
            let cpu_rgba = decode_atlas_mip(&cpu, mip);
            let gpu_rgba = decode_atlas_mip(gpu_bytes, mip);
            let cpu_rmse = rgb_rmse(expected, &cpu_rgba);
            let gpu_rmse = rgb_rmse(expected, &gpu_rgba);
            let psnr = |rmse: f64| 20.0 * (255.0 / rmse).log10();
            eprintln!(
                "terrain mip {mip}: CPU {:.2} dB, GPU {:.2} dB, RMSE {cpu_rmse:.3}/{gpu_rmse:.3}",
                psnr(cpu_rmse),
                psnr(gpu_rmse)
            );
            assert!(gpu_rmse <= 255.0 / 10.0_f64.powf(28.0 / 20.0));
            assert!(
                gpu_rmse <= cpu_rmse * 10.0_f64.powf(3.0 / 20.0) + 1.0,
                "GPU quality exceeded a 3 dB loss plus one byte RMSE allowance at mip {mip}"
            );
            for (expected, actual) in expected
                .as_chunks::<4>()
                .0
                .iter()
                .zip(gpu_rgba.as_chunks::<4>().0.iter())
            {
                assert_eq!(actual[3], expected[3], "alpha changed at mip {mip}");
            }
            let width = size >> mip;
            let tile_width = tile_side >> mip;
            for tile in 0..12 {
                for u in [0.0, 0.5, 1.0] {
                    for v in [0.0, 0.5, 1.0] {
                        let uv = atlas.uv(tile, u, v);
                        let expected = sample_bilinear(expected, width, uv);
                        let actual = sample_bilinear(&gpu_rgba, width, uv);
                        for channel in 0..3 {
                            assert!(
                                (expected[channel] - actual[channel]).abs() <= 16.0,
                                "GPU gutter sample changed at mip {mip}, tile {tile}, uv ({u}, {v})"
                            );
                        }
                    }
                }
            }
            // The unused last atlas row stays transparent, including its gutters.
            assert!(
                gpu_rgba[width * tile_width * 3 * 4..]
                    .iter()
                    .all(|byte| *byte == 0)
            );
        }
        // A partial final batch returns its slot to the shared encoder.
        let repeated = TerrainAtlas::encode_batch(vec![bake()], Some(&gpu), 1).unwrap();
        assert!(repeated[0].gpu_used);
        assert_eq!(repeated[0].bytes, *gpu_bytes);
    }

    /// A diagnostic kernel benchmark, not a full conversion benchmark. Run
    /// only after other GPU work finishes; the explicit pack/VFS inputs are read-only.
    #[test]
    #[ignore = "requires LOD_BENCH_PACK, LOD_BENCH_VFS and an idle hardware GPU"]
    fn gpu_real_pack_atlas_benchmark() {
        use crate::lod::terrain::{
            TerrainCellCache, exterior_terrain_cells, terrain_jobs, validate_terrain_glb,
        };
        use shared::lod::LodOrigin;
        use std::{collections::HashMap, time::Instant};

        let total_start = Instant::now();
        let pack = PathBuf::from(std::env::var_os("LOD_BENCH_PACK").expect("set LOD_BENCH_PACK"));
        let vfs = PathBuf::from(std::env::var_os("LOD_BENCH_VFS").expect("set LOD_BENCH_VFS"));
        let workers: usize = std::env::var("LOD_BENCH_CPU_JOBS")
            .unwrap_or_else(|_| "4".into())
            .parse()
            .unwrap();
        assert!(
            matches!(workers, 4 | 8),
            "use 4 or 8 CPU jobs for this comparison"
        );
        let quality: u32 = std::env::var("LOD_BENCH_GPU_QUALITY")
            .unwrap_or_else(|_| crate::texture_gpu::DEFAULT_QUALITY.to_string())
            .parse()
            .unwrap();
        let batch_mb: u64 = std::env::var("LOD_BENCH_GPU_BATCH_MB")
            .unwrap_or_else(|_| "16".into())
            .parse()
            .unwrap();
        assert!(quality <= 8 && (1..=4096).contains(&batch_mb));
        let block = std::env::var("LOD_BENCH_BLOCK").unwrap_or_else(|_| "tamriel".into());
        let (worldspace_id, origin, min_x, min_y) = match block.as_str() {
            "tamriel" => (60, LodOrigin::new(-96, -96), 0, -16),
            "ocean" => (67_110_912, LodOrigin::new(-64, -64), 96, 96),
            other => panic!("unknown LOD_BENCH_BLOCK {other}; use tamriel or ocean"),
        };
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let output = std::env::var_os("LOD_BENCH_JSON").map_or_else(
            || {
                repo.join(format!(
                    "target/lod-profile/gpu-atlas-{block}-{workers}-q{quality}-b{batch_mb}.json"
                ))
            },
            PathBuf::from,
        );
        let output = if output.is_absolute() {
            output
        } else {
            repo.join(output)
        };
        assert!(
            output.starts_with(repo.join("target")),
            "LOD_BENCH_JSON must be under target"
        );

        let load_start = Instant::now();
        let db_path = pack.join("skyrim_world.db");
        let cache_path = pack.join("cell_cache.rkyv");
        let db_hash = hash_file(&db_path).unwrap();
        let cache_hash = hash_file(&cache_path).unwrap();
        let connection =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let lookup: HashMap<_, _> = exterior_terrain_cells(&connection, worldspace_id)
            .unwrap()
            .into_iter()
            .filter(|(x, y, _)| (min_x..min_x + 16).contains(x) && (min_y..min_y + 16).contains(y))
            .map(|(x, y, id)| (id, (x, y)))
            .collect();
        let inputs = TerrainCellCache::read(&cache_path)
            .unwrap()
            .cells(&lookup)
            .unwrap();
        assert_eq!(
            inputs.len(),
            256,
            "the benchmark requires its complete 16x16 cell block"
        );
        let textures = TerrainTextures::load(&connection, &vfs, &inputs).unwrap();
        textures.verify_sources(&vfs).unwrap();
        let load_seconds = load_start.elapsed().as_secs_f64();
        let jobs = terrain_jobs(worldspace_id, origin, &inputs);
        assert_eq!(jobs.len(), 21);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap();
        let init_start = Instant::now();
        let gpu = GpuUastc::new(quality, batch_mb).unwrap();
        let init_seconds = init_start.elapsed().as_secs_f64();
        let mut prepare_seconds = 0.0;
        let mut mip_seconds = 0.0;
        let mut cpu_encode_seconds = 0.0;
        let mut gpu_prepare_seconds = 0.0;
        let mut gpu_encode_seconds = 0.0;
        let mut cpu_glb_seconds = 0.0;
        let mut gpu_glb_seconds = 0.0;
        let mut quality_seconds = 0.0;
        let mut chunk_reports = Vec::new();
        let mut accepted = true;
        let rgb_quality_limit = 10.0_f64.powf(3.0 / 20.0);

        for batch in jobs.chunks(workers) {
            let start = Instant::now();
            let mut prepared: Vec<_> = pool.install(|| {
                batch
                    .par_iter()
                    .map(|job| job.prepare(origin, &textures).unwrap())
                    .collect()
            });
            prepare_seconds += start.elapsed().as_secs_f64();
            let start = Instant::now();
            let sources: Vec<_> = pool.install(|| {
                prepared
                    .par_iter_mut()
                    .map(|(_, atlas)| mip_chain(atlas.size, std::mem::take(&mut atlas.rgba)))
                    .collect()
            });
            mip_seconds += start.elapsed().as_secs_f64();
            // CPU and GPU receive the same already-authored levels; no atlas is rebaked.
            let start = Instant::now();
            let cpu_bytes: Vec<_> = pool.install(|| {
                prepared
                    .par_iter()
                    .zip(&sources)
                    .map(|((_, atlas), levels)| {
                        TextureConverter::encode_rgba_mips(
                            atlas.size as u32,
                            atlas.size as u32,
                            levels,
                            TextureEncoding::ColorSrgb,
                        )
                        .unwrap()
                    })
                    .collect()
            });
            cpu_encode_seconds += start.elapsed().as_secs_f64();
            let start = Instant::now();
            let gpu_inputs: Vec<_> = pool.install(|| {
                prepared
                    .par_iter()
                    .zip(&sources)
                    .map(|((_, atlas), levels)| {
                        PreparedTexture::from_rgba_mips(
                            atlas.size as u32,
                            atlas.size as u32,
                            levels,
                        )
                        .unwrap()
                    })
                    .collect()
            });
            gpu_prepare_seconds += start.elapsed().as_secs_f64();
            let start = Instant::now();
            let gpu_bytes: Vec<_> = gpu
                .encode_prepared_batch(gpu_inputs, TextureEncoding::ColorSrgb, (workers / 2).max(1))
                .into_iter()
                .map(|result| result.unwrap().bytes)
                .collect();
            gpu_encode_seconds += start.elapsed().as_secs_f64();
            assert_eq!(gpu_bytes.len(), prepared.len());
            let start = Instant::now();
            let cpu_glbs: Vec<_> = pool.install(|| {
                prepared
                    .par_iter()
                    .zip(&cpu_bytes)
                    .map(|((geometry, _), encoded)| geometry.finish(encoded).unwrap().glb)
                    .collect()
            });
            cpu_glb_seconds += start.elapsed().as_secs_f64();
            let start = Instant::now();
            let gpu_glbs: Vec<_> = pool.install(|| {
                prepared
                    .par_iter()
                    .zip(&gpu_bytes)
                    .map(|((geometry, _), encoded)| geometry.finish(encoded).unwrap().glb)
                    .collect()
            });
            gpu_glb_seconds += start.elapsed().as_secs_f64();
            let start = Instant::now();
            let reports: Vec<_> = pool.install(|| (0..batch.len()).into_par_iter().map(|index| {
                let job = &batch[index];
                let atlas = &prepared[index].1;
                let mut mips = Vec::new();
                let mut chunk_accepted = true;
                for (mip, expected) in sources[index].iter().enumerate() {
                    let cpu = decode_atlas_mip(&cpu_bytes[index], mip);
                    let encoded = decode_atlas_mip(&gpu_bytes[index], mip);
                    assert_eq!(expected.len(), encoded.len());
                    let mut cpu_squared = 0.0;
                    let mut gpu_squared = 0.0;
                    let mut opaque_pixels = 0u64;
                    let mut alpha_changed = 0u64;
                    let mut padding_changed = 0u64;
                    for ((expected, cpu), actual) in expected.as_chunks::<4>().0.iter().zip(cpu.as_chunks::<4>().0.iter()).zip(encoded.as_chunks::<4>().0.iter()) {
                        alpha_changed += u64::from(expected[3] != actual[3]);
                        if expected[3] == 0 {
                            padding_changed += u64::from(expected != actual);
                        } else {
                            opaque_pixels += 1;
                            for channel in 0..3 {
                                cpu_squared += (f64::from(expected[channel]) - f64::from(cpu[channel])).powi(2);
                                gpu_squared += (f64::from(expected[channel]) - f64::from(actual[channel])).powi(2);
                            }
                        }
                    }
                    let cpu_rmse = (cpu_squared / (opaque_pixels * 3) as f64).sqrt();
                    let gpu_rmse = (gpu_squared / (opaque_pixels * 3) as f64).sqrt();
                    let psnr = |rmse: f64| if rmse == 0.0 { None } else { Some(20.0 * (255.0 / rmse).log10()) };
                    let width = atlas.size >> mip;
                    let mut cpu_gutter_error = 0.0_f32;
                    let mut gpu_gutter_error = 0.0_f32;
                    for tile in 0..job.members.len() * 4 {
                        for u in [0.0, 0.5, 1.0] {
                            for v in [0.0, 0.5, 1.0] {
                                let uv = atlas.uv(tile, u, v);
                                let expected = sample_bilinear(expected, width, uv);
                                let cpu = sample_bilinear(&cpu, width, uv);
                                let actual = sample_bilinear(&encoded, width, uv);
                                for channel in 0..3 {
                                    cpu_gutter_error = cpu_gutter_error.max((expected[channel] - cpu[channel]).abs());
                                    gpu_gutter_error = gpu_gutter_error.max((expected[channel] - actual[channel]).abs());
                                }
                            }
                        }
                    }
                    let quality_ok = gpu_rmse <= cpu_rmse * rgb_quality_limit + 1.0;
                    let gutters_ok = f64::from(gpu_gutter_error) <= f64::from(cpu_gutter_error) * rgb_quality_limit + 4.0;
                    let shape = crate::texture::inspect_ktx2(&gpu_bytes[index], TextureEncoding::ColorSrgb).unwrap();
                    assert_eq!((shape.width, shape.height, shape.levels, shape.faces), (atlas.size as u32, atlas.size as u32, 3, 1));
                    chunk_accepted &= quality_ok && gutters_ok && alpha_changed == 0 && padding_changed == 0;
                    mips.push(serde_json::json!({
                        "mip": mip, "width": width, "height": width,
                        "source_sha256": hash_bytes(expected), "opaque_pixels": opaque_pixels,
                        "cpu_rgb_rmse": cpu_rmse, "gpu_rgb_rmse": gpu_rmse,
                        "cpu_rgb_psnr_db": psnr(cpu_rmse), "gpu_rgb_psnr_db": psnr(gpu_rmse),
                        "cpu_rgb_lossless": cpu_rmse == 0.0, "gpu_rgb_lossless": gpu_rmse == 0.0,
                        "cpu_gutter_max_error": cpu_gutter_error, "gpu_gutter_max_error": gpu_gutter_error,
                        "alpha_changed_pixels": alpha_changed, "padding_changed_pixels": padding_changed,
                        "quality_accepted": quality_ok, "gutters_accepted": gutters_ok,
                    }));
                }
                validate_terrain_glb(&cpu_glbs[index]).unwrap();
                validate_terrain_glb(&gpu_glbs[index]).unwrap();
                serde_json::json!({
                    "path": shared::lod::chunk_payload_path(job.key), "tier": job.key.tier.side_cells(),
                    "source_cells": job.members.len(), "atlas_side": atlas.size,
                    "cpu_ktx2_bytes": cpu_bytes[index].len(), "gpu_ktx2_bytes": gpu_bytes[index].len(),
                    "cpu_ktx2_sha256": hash_bytes(&cpu_bytes[index]), "gpu_ktx2_sha256": hash_bytes(&gpu_bytes[index]),
                    "cpu_glb_sha256": hash_bytes(&cpu_glbs[index]), "gpu_glb_sha256": hash_bytes(&gpu_glbs[index]),
                    "accepted": chunk_accepted, "mips": mips,
                })
            }).collect());
            quality_seconds += start.elapsed().as_secs_f64();
            accepted &= reports
                .iter()
                .all(|report| report["accepted"].as_bool() == Some(true));
            chunk_reports.extend(reports);
        }
        textures.verify_sources(&vfs).unwrap();
        assert_eq!(
            hash_file(&db_path).unwrap(),
            db_hash,
            "benchmark database changed"
        );
        assert_eq!(
            hash_file(&cache_path).unwrap(),
            cache_hash,
            "benchmark cell cache changed"
        );
        let report = serde_json::json!({
            "variant": "real-pack atlas kernel comparison; excludes fingerprints, cache reuse, database publication and archive ingestion",
            "pack": pack, "vfs": vfs, "database_sha256": db_hash, "cell_cache_sha256": cache_hash,
            "texture_sources": textures.source_hashes,
            "source_sha256": {
                "albedo.rs": hash_bytes(include_bytes!("albedo.rs")),
                "terrain.rs": hash_bytes(include_bytes!("terrain.rs")),
                "texture.rs": hash_bytes(include_bytes!("../texture.rs")),
                "texture_gpu/mod.rs": hash_bytes(include_bytes!("../texture_gpu/mod.rs")),
                "texture_gpu/uastc_encode.wgsl": hash_bytes(include_bytes!("../texture_gpu/uastc_encode.wgsl")),
            },
            "worldspace_id": worldspace_id, "block_label": block,
            "cell_bounds": [min_x, min_x + 16, min_y, min_y + 16], "origin": [origin.grid_x, origin.grid_y],
            "cells": inputs.len(), "chunks": chunk_reports.len(), "cpu_jobs": workers,
            "gpu_adapter": gpu.adapter_name, "gpu_quality": quality, "gpu_batch_mb_requested": batch_mb,
            "gpu_batch_mb_actual": gpu.batch_bytes >> 20, "zstd_level": gpu.zstd_level,
            "timing_seconds": {
                "load_and_hash": load_seconds, "gpu_init": init_seconds,
                "prepare_geometry_and_bake": prepare_seconds, "mips": mip_seconds,
                "cpu_encode": cpu_encode_seconds, "gpu_prepare_uploads": gpu_prepare_seconds,
                "gpu_encode_and_readback": gpu_encode_seconds,
                "cpu_finish_glb": cpu_glb_seconds, "gpu_finish_glb": gpu_glb_seconds,
                "quality_and_validation": quality_seconds, "total_wall": total_start.elapsed().as_secs_f64(),
            },
            "quality_gate": { "max_cpu_loss_db": 3.0, "rmse_allowance": 1.0, "gutter_error_allowance": 4.0, "exact_alpha_and_padding": true },
            "accepted": accepted, "chunk_results": chunk_reports,
        });
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::write(&output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        eprintln!("real-pack atlas benchmark: {}", output.display());
        assert!(
            accepted,
            "GPU atlas quality gate failed; inspect {}",
            output.display()
        );
    }

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
        for (id, color) in [(1, [1.0, 0.0, 0.0]), (2, [0.0, 0.0, 1.0])] {
            result.textures.insert(
                id,
                Arc::new(std::array::from_fn(|_| LinearImage {
                    width: 1,
                    height: 1,
                    pixels: vec![color],
                })),
            );
        }
        result
    }

    #[test]
    fn srgb_lookup_preserves_every_source_byte() {
        for byte in 0..=u8::MAX {
            let color = f32::from(byte) / 255.0;
            let expected = if color <= 0.04045 {
                color / 12.92
            } else {
                ((color + 0.055) / 1.055).powf(2.4)
            };
            assert_eq!(srgb_to_linear(byte).to_bits(), expected.to_bits());
        }
    }

    fn test_dds() -> Vec<u8> {
        dummy_content::dds::generate(
            &dummy_content::dds::Spec::new(dummy_content::dds::Format::Bc1Unorm, 16, 16),
            &mut dummy_content::rng::Rng::new(7),
        )
        .unwrap()
    }

    #[test]
    fn v122_selected_dds_mip_is_decoded_once_for_all_tiers() {
        let dds = Dds::read(Cursor::new(test_dds())).unwrap();
        let mut calls = Vec::new();
        let images = LinearImage::decode_tiers_with(&dds, |mip| {
            calls.push(mip);
            Ok(
                image_dds::SurfaceRgba8::decode_layers_mipmaps_dds(&dds, 0..1, mip..mip + 1)
                    .unwrap(),
            )
        })
        .unwrap();
        assert_eq!(calls, [0]);
        assert_eq!(images.map(|image| image.width), [32, 16, 8]);
    }

    #[test]
    fn v122_diffuse_images_shared_across_formids_and_worlds_keep_source_proof() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE landscape_textures(id INTEGER, texture_set_id INTEGER); CREATE TABLE texture_sets(id INTEGER, diffuse_path TEXT); INSERT INTO texture_sets VALUES(1,'textures/shared.dds'); INSERT INTO landscape_textures VALUES(1,1),(2,1);").unwrap();
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("textures")).unwrap();
        let path = directory.path().join("textures/shared.dds");
        std::fs::write(&path, test_dds()).unwrap();
        let mut cache = TerrainTextureCache::default();
        let first = TerrainTextures::load_with_cache(
            &connection,
            directory.path(),
            &[cell(vec![layer(1, true, 0.0)])],
            &mut cache,
        )
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        let second = TerrainTextures::load_with_cache(
            &connection,
            directory.path(),
            &[cell(vec![layer(2, true, 0.0)])],
            &mut cache,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&first.textures[&1], &second.textures[&2]));
        assert_eq!(first.source_hashes, second.source_hashes);
        assert!(second.verify_sources(directory.path()).is_err());
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
            &[255; 4]
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
        let mips = mip_chain(atlas.size, atlas.rgba.clone());
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
    fn lod_v9_rejects_invalid_layer_inputs() {
        let mut source = cell(vec![layer(1, true, 0.0), layer(2, false, 0.5)]);
        let duplicate = source.layers[1].weights[0].clone();
        source.layers[1].weights.push(duplicate);
        assert!(TerrainAtlas::bake(LodTier::Tier4, &[&source], &textures()).is_err());
    }
}
