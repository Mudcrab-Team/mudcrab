//! A record of every surface that renders with a stand-in for a texture.
//!
//! The renderer never fails a draw for a missing texture. A material with no base-colour image
//! draws its flat base colour (white), a primitive with no UV set samples one texel, and a
//! terrain layer with no diffuse image draws Bevy's stand-in image. Each of those looks like
//! "a white building" and none of them raises an error, so an in-game miss could stay silent.
//!
//! [`record`] is called where the engine meets such a surface. It logs the first sighting of
//! each (kind, mesh, slot) at `warn` level and counts it, so the log names every miss exactly
//! once and [`counts`] gives the total for a run. The offline `texture-audit` tool finds the
//! same classes from the pack and the game data; this module is the in-engine check.

use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};

use bevy::log::warn;

/// What kind of stand-in the renderer uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FallbackKind {
    /// A lighting material with no base-colour image: the surface draws its flat colour.
    UntexturedMaterial,
    /// A primitive whose material has a texture but whose mesh has no `TEXCOORD_0`: every pixel
    /// samples the same texel.
    PrimitiveWithoutUvs,
    /// A terrain layer whose texture set has no converted diffuse image.
    LandTextureWithoutImage,
}

impl FallbackKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::UntexturedMaterial => "untextured_material",
            Self::PrimitiveWithoutUvs => "primitive_without_uvs",
            Self::LandTextureWithoutImage => "land_texture_without_image",
        }
    }
}

/// One recorded substitution.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FallbackSite {
    pub kind: FallbackKind,
    /// The model asset path, or the record for terrain.
    pub mesh: String,
    /// The material or texture slot that fell back.
    pub slot: String,
}

#[derive(Default)]
struct Registry {
    sites: BTreeMap<FallbackSite, u64>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// Records a substitution. Returns true the first time this (kind, mesh, slot) is seen, which is
/// also the only time it is logged.
pub fn record(kind: FallbackKind, mesh: &str, slot: &str) -> bool {
    let site = FallbackSite {
        kind,
        mesh: mesh.to_owned(),
        slot: slot.to_owned(),
    };
    let Ok(mut registry) = registry().lock() else {
        return false;
    };
    let seen = registry.sites.entry(site).or_insert(0);
    *seen += 1;
    let first = *seen == 1;
    if first {
        warn!(
            kind = kind.label(),
            mesh = mesh,
            slot = slot,
            "texture fallback: this surface renders without its texture"
        );
    }
    first
}

/// Distinct substitution sites per kind since the process started.
pub fn counts() -> BTreeMap<FallbackKind, u64> {
    let mut counts = BTreeMap::new();
    if let Ok(registry) = registry().lock() {
        for site in registry.sites.keys() {
            *counts.entry(site.kind).or_insert(0) += 1;
        }
    }
    counts
}

/// Every site recorded so far, in a stable order.
pub fn sites() -> Vec<FallbackSite> {
    registry()
        .lock()
        .map(|registry| registry.sites.keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_site_is_new_once_and_counted_by_kind() {
        // Distinct names keep this independent of other tests sharing the process registry.
        let mesh = "meshes/texture_fallback_test_unique.glb";
        assert!(record(FallbackKind::UntexturedMaterial, mesh, "base_color"));
        assert!(!record(
            FallbackKind::UntexturedMaterial,
            mesh,
            "base_color"
        ));
        assert!(record(
            FallbackKind::PrimitiveWithoutUvs,
            mesh,
            "base_color"
        ));
        assert!(record(FallbackKind::UntexturedMaterial, mesh, "normal"));
        let ours: Vec<_> = sites()
            .into_iter()
            .filter(|site| site.mesh == mesh)
            .collect();
        assert_eq!(ours.len(), 3);
        assert!(counts()[&FallbackKind::UntexturedMaterial] >= 2);
    }
}
