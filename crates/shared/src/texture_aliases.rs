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

/// Maps a texture path stored in a record (a `TXST` or `WATR` column, an alternate texture) to the
/// converted `.ktx2` the engine loads, for example `Textures\\Land\\Grass01.dds` becomes
/// `textures/land/grass01.ktx2`.
///
/// The database is input the loader did not write, so a path that could leave `textures/` - a `..`
/// or `.` segment, an absolute or drive-prefixed path, an empty segment, or a `:` (a Windows drive
/// or stream, or an asset source such as `embedded://`) - gives `None` and is never loaded. The
/// engine and the texture audit both read record paths through this function, so they cannot
/// disagree about what a record path means.
pub fn converted_record_texture_path(path: &str) -> Option<String> {
    // Converted assets are published with lowercase canonical paths, so the lookup lowercases too.
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let without_prefix = normalized.strip_prefix("textures/").unwrap_or(&normalized);
    if without_prefix.is_empty()
        || without_prefix.contains(':')
        || without_prefix
            .split('/')
            .any(|segment| matches!(segment, "" | "." | ".."))
        || !std::path::Path::new(without_prefix)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let mut converted = std::path::PathBuf::from("textures").join(without_prefix);
    converted.set_extension("ktx2");
    Some(converted.to_string_lossy().replace('\\', "/"))
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

    #[test]
    fn record_texture_paths_map_to_converted_paths_and_refuse_unsafe_ones() {
        assert_eq!(
            converted_record_texture_path("Textures\\Land\\Grass01.dds").as_deref(),
            Some("textures/land/grass01.ktx2")
        );
        assert_eq!(
            converted_record_texture_path("land/grass01.dds").as_deref(),
            Some("textures/land/grass01.ktx2")
        );
        for path in [
            "",
            "textures/",
            "textures/./stone.dds",
            "textures/../../secrets.dds",
            "textures//etc/passwd",
            "textures/C:/evil.dds",
            "/abs/path.dds",
        ] {
            assert_eq!(converted_record_texture_path(path), None, "{path}");
        }
    }
}
