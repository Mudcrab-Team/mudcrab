//! Cell-local random stream following spud (MIT, chasmlol/spud), revision e5f2de4.
//! This is deliberately not standard MT19937. See SPUD-MIT.txt for its notice.

pub(super) struct CellRandom {
    words: [u32; 625],
    index: usize,
}

impl CellRandom {
    /// Seed in the reference's arithmetic order, retaining the first untwisted draw.
    pub(super) fn new(seed: u32) -> Self {
        let mut words = [0u32; 625];
        words[0] = seed;
        for i in 1..624 {
            let previous = words[i - 1];
            words[i] =
                1_812_433_253u32.wrapping_mul(previous ^ (i as u32).wrapping_add(previous >> 30));
        }
        Self { words, index: 623 }
    }

    /// Advance one word, with the ascending in-place twist and reference tempering.
    pub(super) fn next(&mut self) -> u32 {
        if self.index == 0 {
            self.words[624] = 0;
            for i in 0..624 {
                let mixed = self.words[i + 1] % 624 + (self.words[i] & 1);
                self.words[i] = self.words[(i + 397) % 624]
                    ^ (mixed * 2)
                    ^ if mixed & 1 == 1 { 0x9908_b0df } else { 0 };
            }
        }
        let mut value = self.words[self.index];
        self.index = (self.index + 1) % 624;
        value ^= value >> 11;
        value ^= (value & 0xff3a_58ad) << 7;
        value ^= (value & 0xffff_df8c) << 15;
        value ^ (value >> 18)
    }
}

/// Map unsigned words with reference float operations; rounding can reach both -1 and 1.
pub(super) fn signed(word: u32) -> f32 {
    word as f32 * 4.656_613e-10 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin words on both sides of two twists with independently generated literals.
    #[test]
    fn reference_stream_crosses_twists() {
        let mut random = CellRandom::new(35);
        let checkpoints = [
            (0, 551378824),
            (1, 1154292349),
            (623, 473545308),
            (624, 238354514),
            (625, 4056029068),
            (1248, 1328648776),
        ];
        for index in 0..=1248 {
            let word = random.next();
            if let Some((_, expected)) = checkpoints.iter().find(|(i, _)| *i == index) {
                assert_eq!(word, *expected, "draw {index}");
            }
        }
    }
}
