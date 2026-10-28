//! Bounded container and UASTC decode proof for the terrain atlas recipe.

use std::io::Read;

use basis_universal::{
    DecodeFlags, LowLevelUastcTranscoder, SliceParametersUastc, TranscoderBlockFormat,
};
use color_eyre::eyre::{Result, WrapErr, ensure, eyre};

use crate::texture::{Ktx2Metadata, TextureEncoding, inspect_ktx2};

/// Validate the three authored atlas mips, including their compressed payloads.
/// The caller derives `expected_side` from the independently checked recipe.
pub(crate) fn validate(bytes: &[u8], expected_side: u32) -> Result<Ktx2Metadata> {
    #[cfg(test)]
    PUBLICATION_DECODES.with(|count| count.set(count.get() + 1));
    ensure!(
        (1..=1024).contains(&expected_side),
        "terrain atlas recipe side must be in 1..=1024"
    );
    let reader =
        ktx2::Reader::new(bytes).map_err(|error| eyre!("invalid terrain atlas KTX2: {error:?}"))?;
    let header = reader.header();
    // Check these before inspect_ktx2 walks the declared mip count or computes
    // expanded sizes. KTX2 encodes absent depth/layers as zero.
    ensure!(
        header.pixel_width == expected_side && header.pixel_height == expected_side,
        "terrain atlas dimensions do not match its recipe"
    );
    ensure!(
        header.pixel_depth == 0 && header.layer_count == 0 && header.face_count == 1,
        "terrain atlas must be a non-array 2D texture with one face"
    );
    ensure!(
        header.level_count == 3,
        "terrain atlas must have three mips"
    );
    ensure!(
        reader.color_model() == Some(ktx2::ColorModel::UASTC),
        "terrain atlas must contain UASTC blocks"
    );
    ensure!(
        header.format.is_none() && header.type_size == 1,
        "terrain atlas must use the UASTC KTX2 format"
    );
    let metadata = inspect_ktx2(bytes, TextureEncoding::ColorSrgb)?;
    let mut levels_seen = 0;
    for (mip, level) in reader.levels().enumerate() {
        levels_seen += 1;
        let side = (expected_side >> mip).max(1);
        let blocks = side.div_ceil(4);
        // Both UASTC4x4 and BC7 use 16 bytes per block. This bound is computed
        // solely from the recipe dimensions, never a file's claimed size.
        let expected_bytes = blocks as usize * blocks as usize * 16;
        ensure!(
            level.uncompressed_byte_length == expected_bytes as u64,
            "terrain atlas mip {mip} has an invalid declared expanded size"
        );
        let uastc = match header.supercompression_scheme {
            None => {
                ensure!(
                    level.data.len() == expected_bytes,
                    "terrain atlas mip {mip} has an invalid UASTC block count"
                );
                level.data.to_vec()
            }
            Some(ktx2::SupercompressionScheme::Zstandard) => {
                // Bound the decoder's retained history too, not just output.
                // 128 KiB admits a zstd block window for the smaller mips.
                let decoder = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
                    level.data,
                    (expected_bytes as u64).max(128 * 1024),
                )
                .map_err(|error| eyre!("terrain atlas mip {mip} zstd: {error:?}"))?;
                expand_bounded(decoder, expected_bytes)
                    .wrap_err_with(|| format!("terrain atlas mip {mip} zstd"))?
            }
            Some(ktx2::SupercompressionScheme::ZLIB) => expand_bounded(
                flate2::bufread::ZlibDecoder::new(level.data),
                expected_bytes,
            )
            .wrap_err_with(|| format!("terrain atlas mip {mip} zlib"))?,
            other => {
                return Err(eyre!(
                    "unsupported terrain atlas supercompression {other:?}"
                ));
            }
        };
        let bc7 = LowLevelUastcTranscoder::new()
            .transcode_slice(
                &uastc,
                SliceParametersUastc {
                    num_blocks_x: blocks,
                    num_blocks_y: blocks,
                    has_alpha: true,
                    original_width: side,
                    original_height: side,
                },
                DecodeFlags::HIGH_QUALITY,
                TranscoderBlockFormat::BC7,
            )
            .map_err(|error| eyre!("terrain atlas mip {mip} UASTC decode: {error:?}"))?;
        ensure!(
            bc7.len() == expected_bytes,
            "terrain atlas mip {mip} decoded to an invalid BC7 block count"
        );
    }
    ensure!(levels_seen == 3, "terrain atlas mip index is incomplete");
    Ok(metadata)
}

fn expand_bounded(reader: impl Read, expected: usize) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(expected + 1);
    reader
        .take(expected as u64 + 1)
        .read_to_end(&mut output)
        .wrap_err("cannot expand terrain atlas mip")?;
    ensure!(
        output.len() == expected,
        "terrain atlas mip expanded to an unexpected byte count"
    );
    Ok(output)
}

#[cfg(test)]
thread_local! {
    pub(super) static PUBLICATION_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture::TextureConverter;

    fn encoded(side: u32, count: usize, encoding: TextureEncoding) -> Vec<u8> {
        let mips = (0..count)
            .map(|mip| {
                let side = (side >> mip).max(1) as usize;
                [64, 128, 192, 255].repeat(side * side)
            })
            .collect::<Vec<_>>();
        TextureConverter::encode_rgba_mips(side, side, &mips, encoding).unwrap()
    }

    #[test]
    fn atlas_decodes_all_three_authored_mips() {
        let bytes = encoded(64, 3, TextureEncoding::ColorSrgb);
        let metadata = validate(&bytes, 64).unwrap();
        assert_eq!(
            (metadata.width, metadata.height, metadata.levels),
            (64, 64, 3)
        );
    }

    #[test]
    fn atlas_rejects_wrong_recipe_container_and_truncation() {
        let bytes = encoded(64, 3, TextureEncoding::ColorSrgb);
        for side in [0, 32, 1025] {
            assert!(validate(&bytes, side).is_err());
        }
        assert!(validate(&encoded(64, 2, TextureEncoding::ColorSrgb), 64).is_err());
        assert!(validate(&encoded(64, 3, TextureEncoding::DataLinear), 64).is_err());
        assert!(validate(&bytes[..bytes.len() / 2], 64).is_err());
        for array in [false, true] {
            let mut header = ktx2::Reader::new(&bytes).unwrap().header();
            if array {
                header.layer_count = 1;
            } else {
                header.pixel_depth = 1;
            }
            let mut wrong_target = bytes.clone();
            wrong_target[..ktx2::Header::LENGTH].copy_from_slice(&header.as_bytes());
            assert!(validate(&wrong_target, 64).is_err());
        }
        let mut absurd_expansion = bytes.clone();
        let offset = ktx2::Header::LENGTH + 16;
        absurd_expansion[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(validate(&absurd_expansion, 64).is_err());
    }

    #[test]
    fn expanded_output_is_bounded_and_exact() {
        assert!(expand_bounded(&[0; 17][..], 16).is_err());
        assert!(expand_bounded(&[0; 15][..], 16).is_err());
        assert_eq!(expand_bounded(&[0; 16][..], 16).unwrap().len(), 16);
    }
}
