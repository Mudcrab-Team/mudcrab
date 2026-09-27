//! Input strategies shared by the parsers' property tests.
//!
//! Every binary parser in the converter reads files it did not write, so each
//! one is checked to return an error, never panic or abort, on arbitrary bytes
//! and on corrupted copies of a valid fixture.

use proptest::{prelude::*, test_runner::RngSeed};

/// `cases` runs from a fixed seed, so the sweep is deterministic like the
/// suite's other truncation and mutation sweeps and a failure reproduces
/// without a regressions file.
pub(crate) fn config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        rng_seed: RngSeed::Fixed(0x0005_4b59),
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

/// Arbitrary bytes, up to `max_len` long.
pub(crate) fn arbitrary_bytes(max_len: usize) -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..=max_len)
}

/// A copy of `fixture` with up to 16 bytes or little-endian words overwritten
/// (words favour 0 and `u32::MAX`, the usual size and count extremes), and
/// half the time cut to a random length.
pub(crate) fn corrupted(fixture: Vec<u8>) -> impl Strategy<Value = Vec<u8>> {
    let len = fixture.len();
    assert!(len >= 4, "fixture is too short to corrupt");
    let value = prop_oneof![Just(0), Just(u32::MAX), any::<u32>()];
    let edits = proptest::collection::vec((0..len, value, any::<bool>()), 1..16);
    let keep = prop_oneof![Just(len), 0..len];
    (edits, keep).prop_map(move |(edits, keep)| {
        let mut bytes = fixture.clone();
        for (index, value, word) in edits {
            if word {
                let start = index.min(len - 4);
                bytes[start..start + 4].copy_from_slice(&value.to_le_bytes());
            } else {
                bytes[index] = value as u8;
            }
        }
        bytes.truncate(keep);
        bytes
    })
}
