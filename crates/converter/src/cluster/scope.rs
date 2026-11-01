//! Re-export of the shared scope (`shared::cluster_recipe::scope`) plus the converter's adapter
//! from its stage settings.

use super::stage::StageSettings;
use shared::cluster_recipe::classify::SnowSource;
pub use shared::cluster_recipe::scope::*;

/// What the stage settings ask to examine.
pub fn request_from_settings(settings: &StageSettings) -> ScopeRequest {
    ScopeRequest {
        include_trees: settings.include_trees,
        child_worldspaces: settings.child_worldspaces.clone(),
        area: settings.area,
    }
}

/// The scope of the stage settings over a database with this snow source.
pub fn scope_from_settings(settings: &StageSettings, snow_source: SnowSource) -> Scope {
    Scope::from_request(&request_from_settings(settings), snow_source)
}
