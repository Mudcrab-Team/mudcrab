//! Record framing must skip damaged records and keep valid neighbours without panicking.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    converter::records::fuzz_scan(data);
});
