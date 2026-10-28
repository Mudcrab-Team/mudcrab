//! Source namespaces omitted by cluster-only conversion.
//! Shared DDS used by full models, records or LAND are restored by those consumers.

use std::path::Path;

fn is_inventory_input(relative: &Path) -> bool {
    let lowered = relative
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let parts: Vec<&str> = lowered.split('/').collect();
    fn extension(name: &str) -> Option<&str> {
        name.rsplit_once('.').map(|(_, extension)| extension)
    }
    match parts.as_slice() {
        ["meshes", "terrain", .., file] => {
            parts.len() >= 4 && matches!(extension(file), Some("btr" | "bto" | "btt" | "lst"))
        }
        ["textures", "terrain", _, "trees", file] => extension(file) == Some("dds"),
        _ => false,
    }
}

/// Candidate baked sources, including native mesh containers outside the inventory
/// namespace and native terrain/object texture namespaces. DDS named by surviving
/// near models, records or LAND are shared sources and are restored by their consumers.
pub(crate) fn is_baked_geometry_input(relative: &Path) -> bool {
    let lowered = relative
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let path = Path::new(&lowered);
    let extension = path.extension().and_then(|extension| extension.to_str());
    matches!(extension, Some("btr" | "bto"))
        || (extension == Some("dds")
            && (lowered.starts_with("textures/terrain/") || lowered.starts_with("textures/lod/")))
        || is_inventory_input(path)
}
