//! Arbitrary bytes must give `Ok` or `Err`, never a panic or abort.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = converter::esm::extractors::extract_subrecords(data);
});
