//! Effective stock draw-pass selection from authored NIF shader flags.

/// SkyrimShaderPropertyFlags1::ZBuffer_Test.
pub const DEPTH_TEST: u32 = 1 << 31;
/// SkyrimShaderPropertyFlags2::ZBuffer_Write.
pub const DEPTH_WRITE: u32 = 1;
/// SkyrimShaderPropertyFlags1::Decal.
pub const DECAL: u32 = 1 << 26;
/// SkyrimShaderPropertyFlags1::Dynamic_Decal.
pub const DYNAMIC_DECAL: u32 = 1 << 27;

/// These materials use the stock sorted color pass, with their authored depth state.
/// An opaque cluster draw must not substitute its ordinary depth/prepass policy.
pub fn requires_sorted_depth_pass(flags_1: u32, flags_2: u32, blended: bool) -> bool {
    let depth_test = flags_1 & DEPTH_TEST != 0;
    let depth_write = depth_test && flags_2 & DEPTH_WRITE != 0;
    let decal = flags_1 & (DECAL | DYNAMIC_DECAL) != 0;
    decal || !depth_test || (!depth_write && !blended)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_depth_and_authored_sorted_passes_are_distinct() {
        assert!(!requires_sorted_depth_pass(DEPTH_TEST, DEPTH_WRITE, false));
        assert!(!requires_sorted_depth_pass(DEPTH_TEST, 0, true));
        for flags in [0, DEPTH_TEST | DECAL, DEPTH_TEST | DYNAMIC_DECAL] {
            for blended in [false, true] {
                assert!(requires_sorted_depth_pass(flags, DEPTH_WRITE, blended));
            }
        }
        assert!(requires_sorted_depth_pass(DEPTH_TEST, 0, false));
    }
}
