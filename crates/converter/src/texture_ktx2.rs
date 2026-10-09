//! Writes UASTC KTX2 files with the same header and data format descriptor
//! Basis Universal emits; shared by the CPU atlas and GPU texture encoders.

// KHR Data Format constants (KTX2 / Khronos Data Format spec).
const KHR_DF_MODEL_UASTC: u32 = 166;
const KHR_DF_PRIMARIES_BT709: u32 = 1;
const KHR_DF_TRANSFER_LINEAR: u32 = 1;
const KHR_DF_TRANSFER_SRGB: u32 = 2;
const KHR_DF_CHANNEL_UASTC_RGB: u32 = 0;
const KHR_DF_CHANNEL_UASTC_RGBA: u32 = 3;

const HEADER_LEN: usize = 80;
const LEVEL_INDEX_LEN: usize = 24;
const IDENTIFIER: &[u8; 12] = b"\xABKTX 20\xBB\r\n\x1A\n";

/// Writes a little-endian `u32` at `offset`.
fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes a little-endian `u64` at `offset`.
fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Data format descriptor for UASTC 4x4: one 128-bit sample.
fn uastc_dfd(srgb: bool, has_alpha: bool) -> Vec<u8> {
    let transfer = if srgb {
        KHR_DF_TRANSFER_SRGB
    } else {
        KHR_DF_TRANSFER_LINEAR
    };
    let channel = if has_alpha {
        KHR_DF_CHANNEL_UASTC_RGBA
    } else {
        KHR_DF_CHANNEL_UASTC_RGB
    };
    let block_size = 24 + 16; // basic descriptor block + one sample
    let words = [
        4 + block_size,                                                        // dfdTotalSize
        0,                      // vendorId 0 | descriptorType 0
        2 | (block_size << 16), // versionNumber 2 | blockSize
        KHR_DF_MODEL_UASTC | (KHR_DF_PRIMARIES_BT709 << 8) | (transfer << 16), // straight alpha
        3 | (3 << 8),           // 4x4x1x1 texel block (minus one)
        16,                     // bytesPlane0
        0,                      // bytesPlane4..7
        (127 << 16) | (channel << 24), // bitOffset 0, bitLength 128 - 1
        0,                      // samplePosition
        0,                      // sampleLower
        u32::MAX,               // sampleUpper
    ];
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// `levels[mip]` already holds every face of that mip, face after face.
pub fn write_uastc(
    width: u32,
    height: u32,
    faces: u32,
    levels: &[Vec<u8>],
    srgb: bool,
    has_alpha: bool,
    writer: &str,
) -> Vec<u8> {
    let dfd = uastc_dfd(srgb, has_alpha);
    let kvd = kvd(writer);
    let total = levels.iter().map(Vec::len).sum::<usize>();
    let mut out = vec![0u8; HEADER_LEN + levels.len() * LEVEL_INDEX_LEN];
    out.reserve(dfd.len() + kvd.len() + total + 16 * levels.len());
    out[..12].copy_from_slice(IDENTIFIER);
    put_u32(&mut out, 12, 0); // VK_FORMAT_UNDEFINED: format given by the DFD
    put_u32(&mut out, 16, 1); // typeSize
    put_u32(&mut out, 20, width);
    put_u32(&mut out, 24, height);
    put_u32(&mut out, 36, faces);
    put_u32(&mut out, 40, levels.len() as u32);
    let dfd_offset = out.len() as u32;
    put_u32(&mut out, 48, dfd_offset);
    put_u32(&mut out, 52, dfd.len() as u32);
    out.extend_from_slice(&dfd);
    let kvd_offset = out.len() as u32;
    put_u32(&mut out, 56, kvd_offset);
    put_u32(&mut out, 60, kvd.len() as u32);
    out.extend_from_slice(&kvd);
    for (level, data) in levels.iter().enumerate() {
        while !out.len().is_multiple_of(16) {
            out.push(0);
        }
        let start = HEADER_LEN + level * LEVEL_INDEX_LEN;
        let offset = out.len() as u64;
        put_u64(&mut out, start, offset);
        put_u64(&mut out, start + 8, data.len() as u64);
        put_u64(&mut out, start + 16, data.len() as u64);
        out.extend_from_slice(data);
    }
    out
}

/// Key/value data: the writer name, padded to 4 bytes as the spec requires.
fn kvd(writer: &str) -> Vec<u8> {
    let entry = format!("KTXwriter\0{writer}\0");
    let mut kvd = (entry.len() as u32).to_le_bytes().to_vec();
    kvd.extend_from_slice(entry.as_bytes());
    while !kvd.len().is_multiple_of(4) {
        kvd.push(0);
    }
    kvd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture::{TextureEncoding, inspect_ktx2, supercompress_ktx2_levels};

    /// A cubemap mip chain survives the writer, Zstandard supercompression
    /// and `inspect_ktx2`: every level index points at its own blocks.
    #[test]
    fn uastc_cubemap_mips_round_trip_through_supercompression() {
        // 8x8 -> 2x2 blocks, 4x4 and 2x2 -> 1 block each; six faces per level.
        let blocks_per_face = [4usize, 1, 1];
        let levels: Vec<Vec<u8>> = blocks_per_face
            .iter()
            .enumerate()
            .map(|(mip, blocks)| {
                (0..6 * blocks * 16)
                    .map(|byte| (mip * 64 + byte) as u8)
                    .collect()
            })
            .collect();
        let raw = write_uastc(8, 8, 6, &levels, true, true, "mudcrab texture_gpu");
        let packed = supercompress_ktx2_levels(&raw, 3).unwrap();
        let metadata = inspect_ktx2(&packed, TextureEncoding::ColorSrgb).unwrap();
        assert_eq!(
            (
                metadata.width,
                metadata.height,
                metadata.faces,
                metadata.levels
            ),
            (8, 8, 6, 3)
        );
        let reader = ::ktx2::Reader::new(&packed[..]).unwrap();
        assert_eq!(
            reader.header().supercompression_scheme,
            Some(::ktx2::SupercompressionScheme::Zstandard)
        );
        for (level, expected) in reader.levels().zip(&levels) {
            assert_eq!(level.uncompressed_byte_length, expected.len() as u64);
            assert_eq!(&zstd::decode_all(level.data).unwrap(), expected);
        }
        // Uncompressed, the levels are stored as written.
        let reader = ::ktx2::Reader::new(&raw[..]).unwrap();
        for (level, expected) in reader.levels().zip(&levels) {
            assert_eq!(level.data, &expected[..]);
        }
    }
}
