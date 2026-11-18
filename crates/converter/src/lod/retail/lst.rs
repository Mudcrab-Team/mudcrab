//! Retail tree type list (`<world>.lst`).
//!
//! Layout, checked against the nine base-game lists (each is exactly `4 + 32 * count` bytes):
//! `i32` entry count, then per entry the `i32` tree type index, five `f32` (width, height and
//! the atlas rectangle `u0, v0, u1, v1`) and a `u32` that is zero in every shipped file and is
//! kept verbatim.

use std::fmt;

const ENTRY_BYTES: usize = 32;

/// One tree type of a `.lst`: how its billboard sits in the tree atlas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TreeTypeEntry {
    pub type_index: i32,
    pub width: f32,
    pub height: f32,
    /// Atlas rectangle in texture coordinates: `[u0, v0, u1, v1]`.
    pub atlas_rect: [f32; 4],
    /// Unknown word, zero in every shipped list.
    pub unknown: u32,
}

/// Why a `.lst` does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LstError {
    /// Fewer than the four count bytes.
    Truncated { len: usize },
    /// A negative entry count.
    NegativeCount(i32),
    /// The length is not exactly `4 + 32 * count`.
    LengthMismatch { expected: u64, actual: usize },
}

impl fmt::Display for LstError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { len } => write!(f, "tree list is {len} bytes, too short for a count"),
            Self::NegativeCount(count) => write!(f, "tree list declares a negative count {count}"),
            Self::LengthMismatch { expected, actual } => {
                write!(
                    f,
                    "tree list is {actual} bytes, expected {expected} for its count"
                )
            }
        }
    }
}

impl std::error::Error for LstError {}

/// Parses a `.lst`. The length must match the declared count exactly.
pub fn parse_tree_list(bytes: &[u8]) -> Result<Vec<TreeTypeEntry>, LstError> {
    let count_bytes: [u8; 4] = bytes
        .get(..4)
        .and_then(|head| head.try_into().ok())
        .ok_or(LstError::Truncated { len: bytes.len() })?;
    let count = i32::from_le_bytes(count_bytes);
    let count = usize::try_from(count).map_err(|_| LstError::NegativeCount(count))?;
    let expected = 4 + ENTRY_BYTES as u64 * count as u64;
    if expected != bytes.len() as u64 {
        return Err(LstError::LengthMismatch {
            expected,
            actual: bytes.len(),
        });
    }
    let f32_at =
        |entry: &[u8], at: usize| f32::from_le_bytes(entry[at..at + 4].try_into().unwrap());
    Ok(bytes[4..]
        .as_chunks::<ENTRY_BYTES>()
        .0
        .iter()
        .map(|entry| TreeTypeEntry {
            type_index: i32::from_le_bytes(entry[0..4].try_into().unwrap()),
            width: f32_at(entry, 4),
            height: f32_at(entry, 8),
            atlas_rect: [
                f32_at(entry, 12),
                f32_at(entry, 16),
                f32_at(entry, 20),
                f32_at(entry, 24),
            ],
            unknown: u32::from_le_bytes(entry[28..32].try_into().unwrap()),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(type_index: i32, values: [f32; 6]) -> Vec<u8> {
        let mut bytes = type_index.to_le_bytes().to_vec();
        for value in values {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(0u32.to_le_bytes());
        bytes
    }

    #[test]
    fn parses_counted_entries() {
        let mut bytes = 2i32.to_le_bytes().to_vec();
        bytes.extend(entry(0, [10.0, 20.0, 0.25, 0.5, 0.75, 1.0]));
        bytes.extend(entry(3, [11.0, 21.0, 0.0, 0.0, 0.5, 0.5]));
        let list = parse_tree_list(&bytes).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].type_index, 0);
        assert_eq!((list[0].width, list[0].height), (10.0, 20.0));
        assert_eq!(list[0].atlas_rect, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(list[1].type_index, 3);
        assert_eq!(list[1].unknown, 0);
    }

    #[test]
    fn an_empty_list_is_four_bytes() {
        assert!(parse_tree_list(&0i32.to_le_bytes()).unwrap().is_empty());
    }

    #[test]
    fn a_wrong_length_or_count_is_an_error_not_a_guess() {
        assert_eq!(
            parse_tree_list(&[1, 2]),
            Err(LstError::Truncated { len: 2 })
        );
        assert_eq!(
            parse_tree_list(&(-1i32).to_le_bytes()),
            Err(LstError::NegativeCount(-1))
        );
        let mut bytes = 1i32.to_le_bytes().to_vec();
        bytes.extend(entry(0, [0.0; 6]));
        bytes.push(0);
        assert!(matches!(
            parse_tree_list(&bytes),
            Err(LstError::LengthMismatch {
                expected: 36,
                actual: 37
            })
        ));
        // A huge count must not allocate: it fails the length check first.
        let huge = i32::MAX.to_le_bytes();
        assert!(matches!(
            parse_tree_list(&huge),
            Err(LstError::LengthMismatch { .. })
        ));
    }
}
