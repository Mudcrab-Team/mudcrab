//! Decode-only support for Skyrim FaceFX `.lip` files.

use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use std::{fs, path::Path};

pub const SLOTS_PER_FRAME: usize = 33;
pub const PHONEME_SLOT_BASE: usize = 6;
pub const FPS: f32 = 30.0;
pub const HEADER_SIZE: usize = 24;
pub const VISEME_NAMES: [&str; 16] = [
    "Aah", "BigAah", "BMP", "ChJSh", "DST", "Eee", "Eh", "FV", "I", "K", "N", "Oh", "OohQ", "R",
    "Th", "W",
];
const MAX_EXTRA_HEADER_BYTES: usize = 8;
const MAX_DECODED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LipHeader {
    pub version: u32,
    pub gridsize: u32,
    pub num_curves: u32,
    pub frames: u16,
    pub const14: u16,
    pub first: i32,
    pub const20: u16,
    pub u22: u16,
}

#[derive(Debug, Clone)]
pub struct LipFile {
    pub header: LipHeader,
    /// Reconstructed rows of all 33 FaceFX curve slots.
    pub grid: Vec<[f32; SLOTS_PER_FRAME]>,
    /// Reliable pre-roll offset, if the record header provides one.
    pub timing_first: Option<i32>,
    /// Offset of the RLE stream in the original file.
    pub payload_offset: usize,
    /// Number of float32 values stored in the decoded payload window.
    pub payload_float_count: usize,
}

impl LipFile {
    /// Extracts the 16 FaceGen viseme curves from each frame, in `VISEME_NAMES` order.
    pub fn visemes(&self) -> Vec<[f32; 16]> {
        self.grid
            .iter()
            .map(|row| std::array::from_fn(|index| row[PHONEME_SLOT_BASE + index]))
            .collect()
    }

    pub fn frame_time(&self, frame: usize) -> Option<f32> {
        self.timing_first
            .map(|first| (frame as f32 + first as f32) / FPS)
    }
}

pub fn decode(path: &Path) -> Result<LipFile> {
    let bytes = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    decode_bytes(&bytes).wrap_err_with(|| format!("failed to decode {}", path.display()))
}

pub fn decode_bytes(data: &[u8]) -> Result<LipFile> {
    ensure!(
        data.len() >= HEADER_SIZE,
        "file is shorter than the .lip header"
    );
    let header = parse_header(data)?;

    let mut candidates = Vec::new();
    for extra in 0..=MAX_EXTRA_HEADER_BYTES {
        let payload_offset = HEADER_SIZE + extra;
        let Some(payload) = data.get(payload_offset..) else {
            continue;
        };
        let Ok(raw) = unrle(payload) else {
            continue;
        };
        if raw.is_empty() || raw.len() % 4 != 0 {
            continue;
        }
        let (chunks, []) = raw.as_chunks::<4>() else {
            continue;
        };
        let values: Vec<f32> = chunks
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        if !values.iter().all(|value| (-2.0..=2.0).contains(value)) {
            continue;
        }
        let count = values.len();
        let p0 = (PHONEME_SLOT_BASE + SLOTS_PER_FRAME - count % SLOTS_PER_FRAME) % SLOTS_PER_FRAME;
        let rows = (p0 + count).div_ceil(SLOTS_PER_FRAME);
        if rows != header.frames as usize {
            continue;
        }
        candidates.push((
            rows.abs_diff(header.frames as usize),
            extra,
            payload_offset,
            values,
        ));
    }
    let (_, _, payload_offset, values) = candidates
        .into_iter()
        .min_by_key(|(distance, extra, _, _)| (*distance, *extra))
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "no payload offset in [{HEADER_SIZE}, {}] has plausible float values and frame geometry",
                HEADER_SIZE + MAX_EXTRA_HEADER_BYTES
            )
        })?;

    let (timing_first, _) = timing_fields(data, header);
    let count = values.len();
    let p0 = (PHONEME_SLOT_BASE + SLOTS_PER_FRAME - count % SLOTS_PER_FRAME) % SLOTS_PER_FRAME;
    let row_count = (p0 + count).div_ceil(SLOTS_PER_FRAME);
    let mut grid = vec![[0.0; SLOTS_PER_FRAME]; row_count];
    for (index, value) in values.into_iter().enumerate() {
        let slot = p0 + index;
        grid[slot / SLOTS_PER_FRAME][slot % SLOTS_PER_FRAME] = value;
    }

    Ok(LipFile {
        header,
        grid,
        timing_first,
        payload_offset,
        payload_float_count: count,
    })
}

fn parse_header(data: &[u8]) -> Result<LipHeader> {
    Ok(LipHeader {
        version: u32_at(data, 0)?,
        gridsize: u32_at(data, 4)?,
        num_curves: u32_at(data, 8)?,
        frames: u16_at(data, 12)?,
        const14: u16_at(data, 14)?,
        first: i32_at(data, 16)?,
        const20: u16_at(data, 20)?,
        u22: u16_at(data, 22)?,
    })
}

fn timing_fields(data: &[u8], header: LipHeader) -> (Option<i32>, usize) {
    if header.const20 == 16 && (-120..=0).contains(&header.first) {
        return (Some(header.first), HEADER_SIZE);
    }
    let Some(repaired_bytes) = data.get(..HEADER_SIZE + 1) else {
        return (None, HEADER_SIZE);
    };
    let mut repaired = [0; HEADER_SIZE];
    repaired[..14].copy_from_slice(&repaired_bytes[..14]);
    repaired[14..].copy_from_slice(&repaired_bytes[15..]);
    let Ok(repaired_header) = parse_header(&repaired) else {
        return (None, HEADER_SIZE);
    };
    if repaired_header.version == header.version
        && repaired_header.gridsize == header.gridsize
        && repaired_header.num_curves == header.num_curves
        && repaired_header.frames == header.frames
        && repaired_header.const20 == 16
        && (-120..=0).contains(&repaired_header.first)
    {
        (Some(repaired_header.first), HEADER_SIZE + 1)
    } else {
        (None, HEADER_SIZE)
    }
}

fn unrle(payload: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < payload.len() {
        if payload[index] == 0 {
            ensure!(index + 2 < payload.len(), "truncated zero-run packet");
            let count = u16::from_le_bytes([payload[index + 1], payload[index + 2]]) as usize;
            ensure!(count > 0, "zero-length zero-run packet");
            ensure!(
                output.len().saturating_add(count) <= MAX_DECODED_BYTES,
                "decoded .lip payload exceeds {MAX_DECODED_BYTES} bytes"
            );
            output.resize(output.len() + count, 0);
            index += 3;
        } else {
            ensure!(
                output.len() < MAX_DECODED_BYTES,
                "decoded .lip payload exceeds {MAX_DECODED_BYTES} bytes"
            );
            output.push(payload[index]);
            index += 1;
        }
    }
    Ok(output)
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| color_eyre::eyre::eyre!("truncated .lip header"))?;
    Ok(u16::from_le_bytes(
        bytes.try_into().expect("two-byte slice"),
    ))
}

fn u32_at(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| color_eyre::eyre::eyre!("truncated .lip header"))?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("four-byte slice"),
    ))
}

fn i32_at(data: &[u8], offset: usize) -> Result<i32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| color_eyre::eyre::eyre!("truncated .lip header"))?;
    Ok(i32::from_le_bytes(
        bytes.try_into().expect("four-byte slice"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&(132_u32 * 2 + 28).to_le_bytes());
        bytes.extend_from_slice(&13_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&3_u16.to_le_bytes());
        bytes.extend_from_slice(&(-9_i32).to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(&111_u16.to_le_bytes());
        let mut raw = vec![0_u8; 39 * 4];
        raw[6 * 4..7 * 4].copy_from_slice(&0.75_f32.to_le_bytes());
        raw[7 * 4..8 * 4].copy_from_slice(&0.25_f32.to_le_bytes());
        bytes.extend_from_slice(&rle(&raw));
        bytes
    }

    fn rle(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut index = 0;
        while index < raw.len() {
            if raw[index] == 0 {
                let start = index;
                while index < raw.len() && raw[index] == 0 {
                    index += 1;
                }
                let mut count = index - start;
                while count > 0 {
                    let run = count.min(u16::MAX as usize);
                    out.push(0);
                    out.extend_from_slice(&(run as u16).to_le_bytes());
                    count -= run;
                }
            } else {
                out.push(raw[index]);
                index += 1;
            }
        }
        out
    }

    #[test]
    fn decodes_grid_and_viseme_channels() {
        let lip = decode_bytes(&fixture()).unwrap();
        assert_eq!(lip.grid.len(), 2);
        assert_eq!(lip.visemes()[0][0], 0.75);
        assert_eq!(lip.visemes()[0][1], 0.25);
        assert_eq!(lip.frame_time(0), Some(-0.3));
    }

    #[test]
    fn repairs_one_byte_timing_header_shift() {
        let standard = fixture();
        let mut shifted = standard[..14].to_vec();
        shifted.push(0);
        shifted.extend_from_slice(&standard[14..]);
        let lip = decode_bytes(&shifted).unwrap();
        assert_eq!(lip.timing_first, Some(-9));
        assert_eq!(lip.payload_offset, HEADER_SIZE + 1);
    }

    #[test]
    fn rejects_truncated_zero_run() {
        let mut bytes = fixture()[..HEADER_SIZE].to_vec();
        bytes.extend_from_slice(&[0, 1]);
        assert!(decode_bytes(&bytes).is_err());
    }

    #[test]
    fn ambiguous_timing_does_not_prevent_curve_decode() {
        let mut bytes = fixture();
        bytes[20..22].copy_from_slice(&0_u16.to_le_bytes());
        let lip = decode_bytes(&bytes).unwrap();
        assert_eq!(lip.timing_first, None);
        assert_eq!(lip.grid.len(), 2);
    }
}
