//! Runtime texture aliases the converter publishes for the engine.
//!
//! An alias keeps the source texture's bytes and adds an infix before `.ktx2`. The transfer infix
//! marks a texture that must be sampled through an sRGB view; a sampler infix marks a texture whose
//! sampler is not the default one. Transfer comes first, sampler second, and nothing else is
//! reserved: `textures/tree.opensky-srgb.opensky-wrap0.ktx2`.
//!
//! The converter writes these names and the engine resolves them back to their source, so both
//! sides read the infixes from here instead of spelling them out.

/// Infix marking a texture that decodes as sRGB.
pub const SRGB_TEXTURE_SUFFIX: &str = ".opensky-srgb";

/// Sampler infixes, indexed by the clamp mode they stand for.
pub const WRAP_TEXTURE_SUFFIXES: [&str; 3] = [".opensky-wrap0", ".opensky-wrap1", ".opensky-wrap2"];

/// The clamp mode that means "default sampler", and so carries no sampler infix.
pub const WRAP_TEXTURE_SUFFIX_NONE: u8 = 3;

/// The sampler infix for a clamp mode, or `None` when the texture keeps the default sampler.
pub fn wrap_texture_suffix(clamp: u8) -> Option<&'static str> {
    WRAP_TEXTURE_SUFFIXES.get(usize::from(clamp)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_reserved_clamp_modes_have_a_sampler_infix() {
        for (mode, suffix) in WRAP_TEXTURE_SUFFIXES.into_iter().enumerate() {
            assert_eq!(wrap_texture_suffix(mode as u8), Some(suffix));
        }
        assert_eq!(wrap_texture_suffix(WRAP_TEXTURE_SUFFIX_NONE), None);
        assert_eq!(wrap_texture_suffix(4), None);
    }
}
