pub mod actor_catalog;
pub mod cache;
pub mod components;
pub mod database;
mod enable_state;
pub mod environment;
pub mod imagespace;

pub mod reference_state;

/// Rejects a database-supplied relative path that could escape the assets
/// root once re-rooted under `textures/` or `meshes/` with `PathBuf::join`.
/// A `..` segment walks back out of the base directory, and a rooted or
/// drive-prefixed path makes `PathBuf::join` replace the base entirely
/// instead of appending to it (see the `std::path::PathBuf::push` docs).
/// Every component must therefore be a plain, non-empty path segment.
pub(crate) fn is_safe_relative_asset_path(path: &str) -> bool {
    std::path::Path::new(path)
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
}
