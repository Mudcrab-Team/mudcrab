//! DDS header reader for the retail LOD atlases.
//!
//! Object LOD draws from one `textures/terrain/<world>/objects/<world>.objects.dds` per world
//! (and a `_n` normal map); tree LOD from `textures/terrain/<world>/trees/<world>treelod.dds`.
//! This reads only the header: size, mip count, pixel format and the payload length that header
//! implies, so an importer can check a file before it decodes or re-encodes it. The header is
//! parsed by the converter's shared reader (`texture_gpu::read_header`, built on `ddsfile`); this
//! module adds the sizing and the layout checks.

use ddsfile::{Caps2, D3D10ResourceDimension, FourCC, MiscFlag, PixelFormatFlags};
use std::fmt;

/// Bytes before the pixel data without a DX10 extension: the magic and the 124-byte header.
const BASE_DATA_OFFSET: usize = 128;
/// The DX10 extension header that follows when the four-character code is `DX10`.
const DX10_BYTES: usize = 20;

/// Pixel format of a DDS texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdsFormat {
    /// Uncompressed with a bit count and channel masks (`R, G, B, A`).
    Uncompressed { bits: u32, masks: [u32; 4] },
    /// A block-compressed or other format named by its four-character code.
    FourCc([u8; 4]),
    /// A format named by a DXGI value in the DX10 extension header.
    Dxgi(u32),
}

/// What a DDS header declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DdsInfo {
    pub width: u32,
    pub height: u32,
    /// Mip levels; at least 1.
    pub mip_levels: u32,
    pub format: DdsFormat,
    /// Bytes before the pixel data (128, or 148 with a DX10 header).
    pub data_offset: usize,
}

impl DdsInfo {
    /// Bytes of pixel data the header implies for all mips, or `None` for a format this reader
    /// does not size (the caller must not guess) or a size that does not fit in `u64`. Every
    /// multiplication and addition is checked, so a header with huge dimensions cannot overflow.
    pub fn expected_payload_bytes(&self) -> Option<u64> {
        let block_bytes: u64 = match self.format {
            DdsFormat::Uncompressed { bits, .. } if bits % 8 == 0 && bits > 0 => {
                let pixel_bytes = u64::from(bits / 8);
                return self.sum_mips(|w, h| {
                    u64::from(w)
                        .checked_mul(u64::from(h))?
                        .checked_mul(pixel_bytes)
                });
            }
            DdsFormat::FourCc(code) => match &code {
                b"DXT1" | b"ATI1" | b"BC4U" => 8,
                b"DXT3" | b"DXT5" | b"ATI2" | b"BC5U" => 16,
                _ => return None,
            },
            // 71 BC1, 74 BC2, 77 BC3, 80 BC4, 83 BC5, 98 BC7 (the UNORM and SRGB variants).
            DdsFormat::Dxgi(71 | 72 | 80 | 81) => 8,
            DdsFormat::Dxgi(74 | 75 | 77 | 78 | 83 | 84 | 98 | 99) => 16,
            _ => return None,
        };
        self.sum_mips(|w, h| {
            u64::from(w.div_ceil(4))
                .checked_mul(u64::from(h.div_ceil(4)))?
                .checked_mul(block_bytes)
        })
    }

    /// Sums `level_bytes` over every mip level, or `None` if a level or the total overflows.
    fn sum_mips(&self, level_bytes: impl Fn(u32, u32) -> Option<u64>) -> Option<u64> {
        (0..self.mip_levels).try_fold(0u64, |total, level| {
            let bytes = level_bytes((self.width >> level).max(1), (self.height >> level).max(1))?;
            total.checked_add(bytes)
        })
    }
}

/// Why a DDS header does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DdsError {
    TooShort {
        len: usize,
    },
    BadMagic,
    /// The header failed the shared DDS reader (`texture_gpu::read_header`).
    InvalidHeader(String),
    ZeroSize {
        width: u32,
        height: u32,
    },
    /// More mip levels than the dimensions allow.
    TooManyMips {
        mip_levels: u32,
        max: u32,
    },
    UnsupportedPixelFormat {
        flags: u32,
    },
    /// More than one surface (an array, cube map or volume), which this reader does not size.
    UnsupportedLayout {
        reason: &'static str,
    },
}

impl fmt::Display for DdsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { len } => write!(f, "DDS is {len} bytes, shorter than its header"),
            Self::BadMagic => write!(f, "DDS magic is missing"),
            Self::InvalidHeader(reason) => write!(f, "DDS header is invalid: {reason}"),
            Self::ZeroSize { width, height } => write!(f, "DDS size is {width}x{height}"),
            Self::TooManyMips { mip_levels, max } => {
                write!(f, "DDS declares {mip_levels} mips, at most {max} fit")
            }
            Self::UnsupportedPixelFormat { flags } => {
                write!(f, "DDS pixel format flags {flags:#x} are not supported")
            }
            Self::UnsupportedLayout { reason } => {
                write!(f, "DDS layout is not supported: {reason}")
            }
        }
    }
}

impl std::error::Error for DdsError {}

/// Reads the header of a DDS file with the converter's shared header reader. The pixel data is not
/// read or checked.
pub fn read_dds_info(bytes: &[u8]) -> Result<DdsInfo, DdsError> {
    if bytes.len() < BASE_DATA_OFFSET {
        return Err(DdsError::TooShort { len: bytes.len() });
    }
    if &bytes[..4] != b"DDS " {
        return Err(DdsError::BadMagic);
    }
    if bytes[84..88] == *b"DX10" && bytes.len() < BASE_DATA_OFFSET + DX10_BYTES {
        return Err(DdsError::TooShort { len: bytes.len() });
    }
    let dds = crate::texture_gpu::read_header(bytes)
        .map_err(|error| DdsError::InvalidHeader(format!("{error:#}")))?;
    let (width, height) = (dds.get_width(), dds.get_height());
    if width == 0 || height == 0 {
        return Err(DdsError::ZeroSize { width, height });
    }
    // Only a single 2D surface is sized. A cube map, volume or array has more surfaces.
    if dds.header.caps2.contains(Caps2::CUBEMAP) {
        return Err(DdsError::UnsupportedLayout { reason: "cube map" });
    }
    if dds.header.caps2.contains(Caps2::VOLUME) {
        return Err(DdsError::UnsupportedLayout {
            reason: "volume texture",
        });
    }
    let mip_levels = dds.get_num_mipmap_levels().max(1);
    let max = 32 - width.max(height).leading_zeros();
    if mip_levels > max {
        return Err(DdsError::TooManyMips { mip_levels, max });
    }
    let spf = &dds.header.spf;
    let mut data_offset = BASE_DATA_OFFSET;
    let format = if let Some(header10) = &dds.header10 {
        if header10.resource_dimension != D3D10ResourceDimension::Texture2D {
            return Err(DdsError::UnsupportedLayout {
                reason: "DX10 resource is not a 2D texture",
            });
        }
        if header10.misc_flag.contains(MiscFlag::TEXTURECUBE) {
            return Err(DdsError::UnsupportedLayout {
                reason: "DX10 cube map",
            });
        }
        if header10.array_size != 1 {
            return Err(DdsError::UnsupportedLayout {
                reason: "DX10 texture array",
            });
        }
        data_offset += DX10_BYTES;
        DdsFormat::Dxgi(header10.dxgi_format as u32)
    } else if let Some(FourCC(code)) = spf.fourcc {
        DdsFormat::FourCc(code.to_le_bytes())
    } else if spf.flags.contains(PixelFormatFlags::RGB) {
        DdsFormat::Uncompressed {
            bits: spf.rgb_bit_count.unwrap_or(0),
            masks: [
                spf.r_bit_mask.unwrap_or(0),
                spf.g_bit_mask.unwrap_or(0),
                spf.b_bit_mask.unwrap_or(0),
                spf.a_bit_mask.unwrap_or(0),
            ],
        }
    } else {
        return Err(DdsError::UnsupportedPixelFormat {
            flags: spf.flags.bits(),
        });
    };
    Ok(DdsInfo {
        width,
        height,
        mip_levels,
        format,
        data_offset,
    })
}

#[cfg(test)]
mod tests {
    const MAGIC: &[u8; 4] = b"DDS ";
    const HEADER_BYTES: usize = 124;
    const DDSD_REQUIRED: u32 = 0x1 | 0x2 | 0x4 | 0x1000 | 0x2_0000;
    const DDPF_FOURCC: u32 = 0x4;
    const DDPF_RGB: u32 = 0x40;
    const DDSCAPS2_CUBEMAP: u32 = 0x200;
    const DDSCAPS2_VOLUME: u32 = 0x20_0000;
    const DX10_MISC_TEXTURECUBE: u32 = 0x4;

    use super::*;

    fn header(
        width: u32,
        height: u32,
        mips: u32,
        flags: u32,
        fourcc: &[u8; 4],
        bits: u32,
    ) -> Vec<u8> {
        let mut bytes = vec![0u8; 4 + HEADER_BYTES];
        bytes[..4].copy_from_slice(MAGIC);
        // Caps, height, width, pixel format and mip-count flags, as real files set them.
        bytes[8..12].copy_from_slice(&DDSD_REQUIRED.to_le_bytes());
        bytes[4..8].copy_from_slice(&124u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&height.to_le_bytes());
        bytes[16..20].copy_from_slice(&width.to_le_bytes());
        bytes[28..32].copy_from_slice(&mips.to_le_bytes());
        bytes[76..80].copy_from_slice(&32u32.to_le_bytes());
        bytes[80..84].copy_from_slice(&flags.to_le_bytes());
        bytes[84..88].copy_from_slice(fourcc);
        bytes[88..92].copy_from_slice(&bits.to_le_bytes());
        for (index, mask) in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000]
            .into_iter()
            .enumerate()
        {
            bytes[92 + index * 4..96 + index * 4].copy_from_slice(&mask.to_le_bytes());
        }
        bytes
    }

    /// The 20-byte DX10 extension: DXGI format, resource dimension, misc flag, array size and
    /// the second misc flag.
    fn push_dx10(bytes: &mut Vec<u8>, dxgi: u32, dimension: u32, misc: u32, array_size: u32) {
        for value in [dxgi, dimension, misc, array_size, 0] {
            bytes.extend(value.to_le_bytes());
        }
    }

    #[test]
    fn an_uncompressed_bgra_atlas_with_one_mip() {
        let info = read_dds_info(&header(2048, 2048, 1, DDPF_RGB, &[0; 4], 32)).unwrap();
        assert_eq!((info.width, info.height, info.mip_levels), (2048, 2048, 1));
        assert!(matches!(
            info.format,
            DdsFormat::Uncompressed { bits: 32, .. }
        ));
        assert_eq!(info.data_offset, 128);
        assert_eq!(info.expected_payload_bytes(), Some(2048 * 2048 * 4));
    }

    #[test]
    fn a_dxt5_normal_map_with_a_full_mip_chain() {
        let info = read_dds_info(&header(2048, 2048, 12, DDPF_FOURCC, b"DXT5", 0)).unwrap();
        assert_eq!(info.format, DdsFormat::FourCc(*b"DXT5"));
        // Levels 2048 down to 1: 16 bytes per 4x4 block, the last three levels are one block.
        let expected: u64 = (0..12u32)
            .map(|level| {
                let side = (2048u32 >> level).max(1).div_ceil(4) as u64;
                side * side * 16
            })
            .sum();
        assert_eq!(info.expected_payload_bytes(), Some(expected));
    }

    #[test]
    fn a_dx10_header_moves_the_data_offset() {
        let mut bytes = header(64, 64, 1, DDPF_FOURCC, b"DX10", 0);
        push_dx10(&mut bytes, 98, 3, 0, 1);
        let info = read_dds_info(&bytes).unwrap();
        assert_eq!(info.format, DdsFormat::Dxgi(98));
        assert_eq!(info.data_offset, 148);
        assert_eq!(info.expected_payload_bytes(), Some(16 * 16 * 16));
    }

    #[test]
    fn a_huge_header_sizes_to_none_instead_of_overflowing() {
        // u32::MAX x u32::MAX x 4 bytes is more than u64 holds: the header reads, the size does
        // not, and nothing panics with overflow checks on.
        let info = read_dds_info(&header(u32::MAX, u32::MAX, 1, DDPF_RGB, &[0; 4], 32)).unwrap();
        assert_eq!((info.width, info.height), (u32::MAX, u32::MAX));
        assert_eq!(info.expected_payload_bytes(), None);
        // The compressed path: (2^30)^2 blocks of 16 bytes is exactly 2^64, one past u64::MAX.
        let compressed =
            read_dds_info(&header(u32::MAX, u32::MAX, 1, DDPF_FOURCC, b"DXT5", 0)).unwrap();
        assert_eq!(compressed.expected_payload_bytes(), None);
        // A size that fits still sizes: 65,536 x 65,536 x 4 bytes is 2^34.
        let fits = read_dds_info(&header(65_536, 65_536, 1, DDPF_RGB, &[0; 4], 32)).unwrap();
        assert_eq!(fits.expected_payload_bytes(), Some(1 << 34));
        // The mip total is checked too: one 1-byte-per-pixel level of (2^32 - 1)^2 bytes fits in
        // u64, but adding the second level (about 2^62 bytes) does not.
        let level = |mip_levels| DdsInfo {
            width: u32::MAX,
            height: u32::MAX,
            mip_levels,
            format: DdsFormat::Uncompressed {
                bits: 8,
                masks: [0; 4],
            },
            data_offset: 128,
        };
        assert_eq!(
            level(1).expected_payload_bytes(),
            Some(u64::from(u32::MAX) * u64::from(u32::MAX))
        );
        assert_eq!(level(2).expected_payload_bytes(), None);
    }

    #[test]
    fn layouts_with_more_than_one_surface_are_rejected() {
        let dx10 = |dimension, misc, array_size| {
            let mut bytes = header(64, 64, 1, DDPF_FOURCC, b"DX10", 0);
            push_dx10(&mut bytes, 98, dimension, misc, array_size);
            bytes.extend([0u8; 4096]);
            read_dds_info(&bytes)
        };
        // A 64x64 BC7 array of two would otherwise size as one surface (4,096 bytes, not 8,192).
        assert!(matches!(
            dx10(3, 0, 2),
            Err(DdsError::UnsupportedLayout { .. })
        ));
        assert!(matches!(
            dx10(3, DX10_MISC_TEXTURECUBE, 1),
            Err(DdsError::UnsupportedLayout { .. })
        ));
        assert!(matches!(
            dx10(4, 0, 1),
            Err(DdsError::UnsupportedLayout { .. })
        ));
        assert!(matches!(
            dx10(3, 0, 0),
            Err(DdsError::UnsupportedLayout { .. })
        ));
        assert!(dx10(3, 0, 1).is_ok());
        // Legacy cube map and volume caps.
        for caps2 in [DDSCAPS2_CUBEMAP, DDSCAPS2_VOLUME] {
            let mut bytes = header(64, 64, 1, DDPF_FOURCC, b"DXT5", 0);
            bytes[112..116].copy_from_slice(&caps2.to_le_bytes());
            assert!(
                matches!(
                    read_dds_info(&bytes),
                    Err(DdsError::UnsupportedLayout { .. })
                ),
                "{caps2:#x}"
            );
        }
    }

    #[test]
    fn bad_headers_are_errors() {
        assert!(matches!(
            read_dds_info(&[0; 10]),
            Err(DdsError::TooShort { .. })
        ));
        let mut bytes = header(8, 8, 1, DDPF_RGB, &[0; 4], 32);
        bytes[0] = b'X';
        assert_eq!(read_dds_info(&bytes), Err(DdsError::BadMagic));
        assert!(matches!(
            read_dds_info(&header(0, 8, 1, DDPF_RGB, &[0; 4], 32)),
            Err(DdsError::ZeroSize { .. })
        ));
        assert!(matches!(
            read_dds_info(&header(8, 8, 9, DDPF_RGB, &[0; 4], 32)),
            Err(DdsError::TooManyMips { max: 4, .. })
        ));
        assert!(matches!(
            read_dds_info(&header(8, 8, 1, 0, &[0; 4], 0)),
            Err(DdsError::UnsupportedPixelFormat { .. })
        ));
        // An unknown four-character code reads, but is not sized.
        let info = read_dds_info(&header(8, 8, 1, DDPF_FOURCC, b"ZZZZ", 0)).unwrap();
        assert_eq!(info.expected_payload_bytes(), None);
    }
}
