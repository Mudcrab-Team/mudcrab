//! A compressed record payload must stay within the 64 MiB ceiling or return `Err`.
#![no_main]

use libfuzzer_sys::fuzz_target;

/// Matches the ceiling in `records::scanner::inflate`.
const MAX_INFLATED: usize = 64 * 1024 * 1024;

fuzz_target!(|data: &[u8]| match converter::records::fuzz_inflate(data) {
    Ok(bytes) => assert!(
        bytes.len() <= MAX_INFLATED,
        "inflate returned {} bytes above the {MAX_INFLATED} byte ceiling",
        bytes.len()
    ),
    Err(_) => {}
});
