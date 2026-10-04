//! Endpoint quantization tables for the trit/quint ASTC ranges UASTC uses,
//! uploaded to the GPU encoder. Unquantization is a port of basisu's
//! `unquant_astc_endpoint` (basisu_transcoder.cpp), so the shader scores
//! blocks with exactly the values the transcoder will decode.

/// (bits, trits, quints) per ASTC range, as `g_astc_bise_range_table`.
const RANGES: [(u32, u32, u32); 21] = [
    (1, 0, 0),
    (0, 1, 0),
    (2, 0, 0),
    (0, 0, 1),
    (1, 1, 0),
    (3, 0, 0),
    (1, 0, 1),
    (2, 1, 0),
    (4, 0, 0),
    (2, 0, 1),
    (3, 1, 0),
    (5, 0, 0),
    (3, 0, 1),
    (4, 1, 0),
    (6, 0, 0),
    (4, 0, 1),
    (5, 1, 0),
    (7, 0, 0),
    (5, 0, 1),
    (6, 1, 0),
    (8, 0, 0),
];

/// `g_astc_endpoint_unquant_params` for the trit/quint ranges.
fn unquant_params(range: usize) -> (&'static [u8; 9], u32) {
    match range {
        4 => (b"000000000", 204),
        6 => (b"000000000", 113),
        7 => (b"b000b0bb0", 93),
        9 => (b"b0000bb00", 54),
        10 => (b"cb000cbcb", 44),
        12 => (b"cb0000cbc", 26),
        13 => (b"dcb000dcb", 22),
        15 => (b"dcb0000dc", 13),
        16 => (b"edcb000ed", 11),
        18 => (b"edcb0000e", 6),
        19 => (b"fedcb000f", 5),
        _ => panic!("range {range} has no trit/quint unquantization"),
    }
}

/// Number of values an ASTC range can represent.
pub fn levels(range: usize) -> u32 {
    let (bits, trits, quints) = RANGES[range];
    (1 << bits)
        * if trits > 0 {
            3
        } else if quints > 0 {
            5
        } else {
            1
        }
}

/// BISE value (trit/quint digit above the low bits) -> 0..255.
pub fn unquant_endpoint(value: u32, range: usize) -> u32 {
    let (bits, trits, quints) = RANGES[range];
    let packed_bits = value & ((1 << bits) - 1);
    if trits == 0 && quints == 0 {
        // Bit replication to 8 bits.
        let mut out = 0;
        let mut left = 8i32;
        while left > 0 {
            let n = left.min(bits as i32);
            let v = packed_bits >> (bits as i32 - n);
            out |= v << (left - n);
            left -= n;
        }
        return out;
    }
    let digit = value >> bits;
    let a = if packed_bits & 1 != 0 { 511 } else { 0 };
    let (pattern, c) = unquant_params(range);
    let mut b = 0;
    for &ch in pattern {
        b <<= 1;
        if ch != b'0' {
            b |= (packed_bits >> (ch - b'a')) & 1;
        }
    }
    let val = (digit * c + b) ^ a;
    (a & 0x80) | (val >> 2)
}

/// Shader table layout (u32 words); the `R*_QUANT` / `R*_UNQUANT` offsets in
/// uastc_encode.wgsl follow this order.
pub const BISE_RANGES: [usize; 4] = [13, 19, 7, 12];

/// For each range in `BISE_RANGES`: 256 entries "0..255 -> nearest BISE
/// value", then `levels` entries "BISE value -> 0..255".
pub fn shader_tables() -> Vec<u32> {
    let mut words = Vec::new();
    for range in BISE_RANGES {
        let unquant: Vec<u32> = (0..levels(range))
            .map(|v| unquant_endpoint(v, range))
            .collect();
        for target in 0..256i32 {
            let best = (0..unquant.len())
                .min_by_key(|&v| (unquant[v] as i32 - target).abs())
                .unwrap();
            words.push(best as u32);
        }
        words.extend_from_slice(&unquant);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every table range spans 0..=255 and the layout matches the shader's offsets.
    #[test]
    fn bise_ranges_cover_full_scale() {
        for range in BISE_RANGES {
            let values: Vec<u32> = (0..levels(range))
                .map(|v| unquant_endpoint(v, range))
                .collect();
            assert_eq!(values.iter().min(), Some(&0));
            assert_eq!(values.iter().max(), Some(&255));
        }
        assert_eq!(levels(13), 48);
        assert_eq!(levels(19), 192);
        assert_eq!(levels(7), 12);
        assert_eq!(levels(12), 40);
        // Offsets used by the shader.
        let offsets: Vec<usize> = BISE_RANGES
            .iter()
            .scan(0, |offset, &range| {
                let start = *offset;
                *offset += 256 + levels(range) as usize;
                Some(start)
            })
            .collect();
        assert_eq!(offsets, [0, 304, 752, 1020]);
        assert_eq!(shader_tables().len(), 1020 + 256 + 40);
    }
}
