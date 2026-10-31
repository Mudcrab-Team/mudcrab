//! Per-view instance eligibility appended to the cluster batch-table buffer.
//! Each vec4<u32> carries 128 instance bits, serialized explicitly little-endian.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstanceMask {
    capacity: u32,
    vectors: Vec<[u32; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InstanceMaskOutOfRange {
    pub index: u32,
    pub capacity: u32,
}

impl std::fmt::Display for InstanceMaskOutOfRange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "instance mask index {} is outside capacity {}",
            self.index, self.capacity
        )
    }
}

impl std::error::Error for InstanceMaskOutOfRange {}

impl InstanceMask {
    /// Every instance starts disabled, including unused tail bits.
    pub(crate) fn new(capacity: u32) -> Self {
        Self {
            capacity,
            vectors: vec![[0; 4]; capacity.div_ceil(128) as usize],
        }
    }

    pub(crate) fn capacity(&self) -> u32 {
        self.capacity
    }

    #[cfg(test)]
    fn enabled_count(&self) -> u32 {
        self.vectors
            .iter()
            .flatten()
            .map(|word| word.count_ones())
            .sum()
    }

    pub(crate) fn set(&mut self, index: u32, enabled: bool) -> Result<(), InstanceMaskOutOfRange> {
        if index >= self.capacity {
            return Err(InstanceMaskOutOfRange {
                index,
                capacity: self.capacity,
            });
        }
        let vector = (index >> 7) as usize;
        let lane = ((index >> 5) & 3) as usize;
        let bit = 1u32 << (index & 31);
        let word = &mut self.vectors[vector][lane];
        if enabled {
            *word |= bit;
        } else {
            *word &= !bit;
        }
        Ok(())
    }

    /// Whether `index` is enabled; an index past the capacity is not.
    pub(crate) fn get(&self, index: u32) -> bool {
        index < self.capacity
            && self.vectors[(index >> 7) as usize][((index >> 5) & 3) as usize]
                & (1 << (index & 31))
                != 0
    }

    /// Append without changing the buffer's existing batch-table prefix.
    pub(crate) fn append_le_bytes(&self, output: &mut Vec<u8>) {
        for vector in &self.vectors {
            for word in vector {
                output.extend_from_slice(&word.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_indices_match_independent_fixed_little_endian_bytes() {
        let mut mask = InstanceMask::new(129);
        assert_eq!(mask.capacity(), 129);
        assert_eq!(mask.enabled_count(), 0);
        let mut empty = Vec::new();
        mask.append_le_bytes(&mut empty);
        assert_eq!(empty, vec![0; 32]);
        for index in [0, 31, 32, 127, 128] {
            mask.set(index, true).unwrap();
        }
        // Fixed words: [0x80000001, 1, 0, 0x80000000], [1, 0, 0, 0].
        // Do not compute these bytes through the production index/shift logic.
        let expected = [
            0x01, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x80, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        let mut bytes = vec![0xaa, 0xbb];
        mask.append_le_bytes(&mut bytes);
        assert_eq!(&bytes[..2], &[0xaa, 0xbb]);
        assert_eq!(&bytes[2..], expected);
        assert_eq!(mask.enabled_count(), 5);
    }

    #[test]
    fn toggles_are_idempotent_and_clearing_preserves_other_bits() {
        let mut mask = InstanceMask::new(128);
        mask.set(31, true).unwrap();
        mask.set(32, true).unwrap();
        mask.set(31, true).unwrap();
        assert_eq!(mask.enabled_count(), 2);
        mask.set(31, false).unwrap();
        mask.set(31, false).unwrap();
        assert_eq!(mask.enabled_count(), 1);
        let mut bytes = Vec::new();
        mask.append_le_bytes(&mut bytes);
        assert_eq!(bytes, [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        mask.set(32, false).unwrap();
        assert_eq!(mask.enabled_count(), 0);
        bytes.clear();
        mask.append_le_bytes(&mut bytes);
        assert_eq!(bytes, [0; 16]);
    }

    #[test]
    fn empty_and_out_of_range_writes_are_refused_without_mutation() {
        let mut empty = InstanceMask::new(0);
        let mut bytes = vec![0xaa];
        empty.append_le_bytes(&mut bytes);
        assert_eq!(bytes, [0xaa]);
        assert_eq!(
            empty.set(0, true),
            Err(InstanceMaskOutOfRange {
                index: 0,
                capacity: 0
            })
        );
        let mut mask = InstanceMask::new(129);
        mask.set(128, true).unwrap();
        let before = mask.clone();
        for index in [129, 255, u32::MAX] {
            for enabled in [false, true] {
                assert_eq!(
                    mask.set(index, enabled),
                    Err(InstanceMaskOutOfRange {
                        index,
                        capacity: 129
                    })
                );
                assert_eq!(mask, before);
            }
        }
    }
}
