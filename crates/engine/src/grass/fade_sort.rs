//! Not retail: MudCrab uses 18-bit spatial fade ranks instead, so that equal ranks keep a stable
//! order; retail fades per vertex through the alpha test.
//! Stable ordering for the renderer's 18-bit spatial fade ranks.
use super::GrassInstance;

/// Preserve every equal-rank tie through three stable six-bit counting passes.
pub(super) fn sort(values: &mut [GrassInstance]) {
    if values.len() < 128 {
        values.sort_by_key(GrassInstance::candidate_slot);
        return;
    }
    // Retain one owned scratch instance per value while distributing buckets.
    let mut scratch = vec![values[0]; values.len()];
    let pass = |input: &[GrassInstance], output: &mut [GrassInstance], shift: u32| {
        let mut next = [0usize; 64];
        for instance in input {
            next[((instance.candidate_slot() >> shift) & 63) as usize] += 1;
        }
        let mut prefix = 0;
        for count in &mut next {
            let size = *count;
            *count = prefix;
            prefix += size;
        }
        for instance in input {
            let bucket = ((instance.candidate_slot() >> shift) & 63) as usize;
            output[next[bucket]] = *instance;
            next[bucket] += 1;
        }
    };
    pass(values, &mut scratch, 0);
    pass(&scratch, values, 6);
    pass(values, &mut scratch, 12);
    values.copy_from_slice(&scratch);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whole-instance identity against the original stable sort covers ties, metadata and cutovers.
    #[test]
    fn stable_ranks_preserve_all_instance_bytes() {
        for len in [0usize, 1, 2, 63, 127, 128, 129, 1024, 8192, 65536] {
            for pattern in 0..4 {
                let source: Vec<_> = (0..len)
                    .map(|i| {
                        let key = match pattern {
                            0 => 63,
                            1 => [0, 1, 63, 64, 4095, 4096, 262143, 63][i % 8],
                            2 => (i as u32).wrapping_mul(7919) & 262143,
                            _ => ((i as u32).wrapping_mul(747796405) ^ ((i as u32) >> 3)) & 1023,
                        };
                        GrassInstance {
                            position: [
                                i as u16,
                                (i as u32).rotate_left(13) as u16,
                                (len - i - 1) as u16,
                            ],
                            rotation: (i.wrapping_mul(29) >> 3) as u8,
                            scale: 64 + (i % 128) as u8,
                            color: i.wrapping_mul(37) as u8,
                            normal: [(i % 127) as i8, -((i % 63) as i8), 127],
                            slot: key | (((i as u32).wrapping_mul(0x9e37_79b9) & 0x3fff) << 18),
                        }
                    })
                    .collect();
                let mut expected = source.clone();
                let mut actual = source;
                expected.sort_by_key(GrassInstance::candidate_slot);
                sort(&mut actual);
                assert_eq!(
                    bytemuck::cast_slice::<GrassInstance, u8>(&actual),
                    bytemuck::cast_slice::<GrassInstance, u8>(&expected),
                    "length={len} pattern={pattern}"
                );
            }
        }
    }
}
