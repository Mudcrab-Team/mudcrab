//! Retail tree LOD block (`<world>.4.<x>.<y>.btt`).
//!
//! A `.btt` is not a NIF. Layout, checked by parsing every base-game file: an `i32` group count;
//! per group an `i32` tree type index (an index into the world's `.lst`) and an `i32` billboard
//! count; then per billboard a 32-byte record: position `x, y, z` (`f32`), rotation about Z
//! (`f32`, radians), scale (`f32`), the placed reference's form id (`u32`) and eight more bytes.
//! The eight trailing bytes are NOT zero in most records (40,523 of 43,333 in the base game), so
//! they are kept verbatim and not interpreted.
//!
//! Do not assume the layout holds for every file. In the base game all 329 Tamriel blocks parse
//! to the exact length, but six Solstheim blocks (`dlc2solstheimworld.4.12.*` and `.4.16.*`)
//! carry more bytes than this layout explains. [`parse_tree_block`] therefore fails on any length
//! mismatch, and [`parse_tree_block_prefix`] returns how many bytes the layout consumed so a
//! caller can report the rest instead of silently reading it.

use std::fmt;

const RECORD_BYTES: usize = 32;
const GROUP_HEADER_BYTES: usize = 8;

/// One tree billboard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TreeBillboard {
    /// Block-local position as stored, in Creation units.
    pub position: [f32; 3],
    /// Rotation about Z, radians.
    pub rotation_z: f32,
    pub scale: f32,
    /// Form id of the placed reference (a `REFR`, not the `TREE` base).
    pub form_id: u32,
    /// Eight bytes after the form id, kept verbatim.
    pub tail: [u8; 8],
}

/// The billboards of one tree type in a block.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeGroup {
    /// Index into the world's `.lst`.
    pub type_index: i32,
    pub billboards: Vec<TreeBillboard>,
}

/// A parsed `.btt`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TreeBlock {
    pub groups: Vec<TreeGroup>,
}

impl TreeBlock {
    /// Total billboards over all groups.
    pub fn billboard_count(&self) -> usize {
        self.groups.iter().map(|group| group.billboards.len()).sum()
    }
}

/// Why a `.btt` does not parse with the known layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BttError {
    /// The bytes end inside a header or record.
    Truncated {
        at: usize,
        needed: usize,
        len: usize,
    },
    /// A negative group or billboard count.
    NegativeCount { at: usize, count: i32 },
    /// A declared count cannot fit in the remaining bytes (checked before allocating).
    CountExceedsBytes {
        at: usize,
        count: u64,
        remaining: usize,
    },
    /// The layout parsed, but `consumed` is not the whole file.
    TrailingBytes { consumed: usize, len: usize },
}

impl fmt::Display for BttError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { at, needed, len } => {
                write!(
                    f,
                    "tree block ends at {len} bytes, needs {needed} at offset {at}"
                )
            }
            Self::NegativeCount { at, count } => {
                write!(f, "tree block has a negative count {count} at offset {at}")
            }
            Self::CountExceedsBytes {
                at,
                count,
                remaining,
            } => write!(
                f,
                "tree block declares {count} entries at offset {at} but only {remaining} bytes remain"
            ),
            Self::TrailingBytes { consumed, len } => write!(
                f,
                "tree block layout consumed {consumed} of {len} bytes; the layout does not describe this file"
            ),
        }
    }
}

impl std::error::Error for BttError {}

fn read_i32(bytes: &[u8], at: usize) -> Result<i32, BttError> {
    bytes
        .get(at..at + 4)
        .map(|word| i32::from_le_bytes(word.try_into().unwrap()))
        .ok_or(BttError::Truncated {
            at,
            needed: at + 4,
            len: bytes.len(),
        })
}

fn count_at(bytes: &[u8], at: usize, unit: usize) -> Result<usize, BttError> {
    let count = read_i32(bytes, at)?;
    let count = usize::try_from(count).map_err(|_| BttError::NegativeCount { at, count })?;
    let remaining = bytes.len().saturating_sub(at + 4);
    if (count as u64).saturating_mul(unit as u64) > remaining as u64 {
        return Err(BttError::CountExceedsBytes {
            at,
            count: count as u64,
            remaining,
        });
    }
    Ok(count)
}

/// Parses the layout from the start of `bytes` and returns the block and the bytes consumed.
/// Use this to see how much of an unusual file the layout explains.
pub fn parse_tree_block_prefix(bytes: &[u8]) -> Result<(TreeBlock, usize), BttError> {
    let group_count = count_at(bytes, 0, GROUP_HEADER_BYTES)?;
    let mut offset = 4;
    let mut groups = Vec::with_capacity(group_count);
    for _ in 0..group_count {
        let type_index = read_i32(bytes, offset)?;
        let billboard_count = count_at(bytes, offset + 4, RECORD_BYTES)?;
        offset += GROUP_HEADER_BYTES;
        let mut billboards = Vec::with_capacity(billboard_count);
        for _ in 0..billboard_count {
            let record = &bytes[offset..offset + RECORD_BYTES];
            let float = |at: usize| f32::from_le_bytes(record[at..at + 4].try_into().unwrap());
            billboards.push(TreeBillboard {
                position: [float(0), float(4), float(8)],
                rotation_z: float(12),
                scale: float(16),
                form_id: u32::from_le_bytes(record[20..24].try_into().unwrap()),
                tail: record[24..32].try_into().unwrap(),
            });
            offset += RECORD_BYTES;
        }
        groups.push(TreeGroup {
            type_index,
            billboards,
        });
    }
    Ok((TreeBlock { groups }, offset))
}

/// Parses a `.btt`. The layout must consume the whole file; otherwise this is an error, because
/// the format is only verified for the base-game files that parse exactly.
pub fn parse_tree_block(bytes: &[u8]) -> Result<TreeBlock, BttError> {
    let (block, consumed) = parse_tree_block_prefix(bytes)?;
    if consumed != bytes.len() {
        return Err(BttError::TrailingBytes {
            consumed,
            len: bytes.len(),
        });
    }
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(x: f32, form_id: u32, tail: [u8; 8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [x, 2.0, 3.0, 0.5, 1.25] {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(form_id.to_le_bytes());
        bytes.extend(tail);
        bytes
    }

    fn block() -> Vec<u8> {
        let mut bytes = 2i32.to_le_bytes().to_vec();
        bytes.extend(33i32.to_le_bytes());
        bytes.extend(2i32.to_le_bytes());
        bytes.extend(record(1.0, 0xC27AB, [0, 0, 0, 0, 0, 0xBC, 0xB1, 0x13]));
        bytes.extend(record(4.0, 0xC27AC, [0; 8]));
        bytes.extend(30i32.to_le_bytes());
        bytes.extend(1i32.to_le_bytes());
        bytes.extend(record(9.0, 7, [1, 2, 3, 4, 5, 6, 7, 8]));
        bytes
    }

    #[test]
    fn parses_groups_and_billboards_and_keeps_the_tail_verbatim() {
        let parsed = parse_tree_block(&block()).unwrap();
        assert_eq!(parsed.groups.len(), 2);
        assert_eq!(parsed.billboard_count(), 3);
        assert_eq!(parsed.groups[0].type_index, 33);
        let first = parsed.groups[0].billboards[0];
        assert_eq!(first.position, [1.0, 2.0, 3.0]);
        assert_eq!((first.rotation_z, first.scale), (0.5, 1.25));
        assert_eq!(first.form_id, 0xC27AB);
        assert_eq!(first.tail, [0, 0, 0, 0, 0, 0xBC, 0xB1, 0x13]);
        assert_eq!(
            parsed.groups[1].billboards[0].tail,
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn an_empty_block_is_a_zero_count() {
        let parsed = parse_tree_block(&0i32.to_le_bytes()).unwrap();
        assert!(parsed.groups.is_empty());
    }

    #[test]
    fn extra_bytes_are_reported_not_ignored() {
        let mut bytes = block();
        let layout_len = bytes.len();
        bytes.extend([0xAA; 12]);
        assert_eq!(
            parse_tree_block(&bytes),
            Err(BttError::TrailingBytes {
                consumed: layout_len,
                len: layout_len + 12
            })
        );
        // The prefix reader still shows what the layout explains.
        let (prefix, consumed) = parse_tree_block_prefix(&bytes).unwrap();
        assert_eq!((prefix.billboard_count(), consumed), (3, layout_len));
    }

    #[test]
    fn truncated_negative_and_oversized_counts_fail_without_allocating() {
        let bytes = block();
        assert!(matches!(
            parse_tree_block(&bytes[..bytes.len() - 5]),
            Err(BttError::CountExceedsBytes { .. })
        ));
        assert!(matches!(
            parse_tree_block(&[1, 0]),
            Err(BttError::Truncated { .. })
        ));
        assert!(matches!(
            parse_tree_block(&(-3i32).to_le_bytes()),
            Err(BttError::NegativeCount { count: -3, .. })
        ));
        let mut huge = 1i32.to_le_bytes().to_vec();
        huge.extend(0i32.to_le_bytes());
        huge.extend(i32::MAX.to_le_bytes());
        assert!(matches!(
            parse_tree_block(&huge),
            Err(BttError::CountExceedsBytes { .. })
        ));
        assert!(matches!(
            parse_tree_block(&i32::MAX.to_le_bytes()),
            Err(BttError::CountExceedsBytes { .. })
        ));
    }
}
