//! Optional, typed worn outfits for `NPC_` bases, derived from their default outfit.
//!
//! The catalog is one sidecar file beside the world database. It holds, per `NPC_` base, the
//! armature models the default outfit (`NPC_/DOFT` -> `OTFT` -> `ARMO` -> `ARMA`) puts on the
//! actor, already matched to the actor's race and sex and already named as converted scene paths.
//! The converter resolves it once; the runtime only reads rows and never decodes records.
//! A pack without the file spawns actors exactly as before.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Where the catalog lives, relative to the converted-assets root.
pub const PATH: &str = "actor-outfits.json";
/// Catalog format version. A different version is rejected as a whole.
pub const VERSION: u32 = 1;

/// `ARMA/BOD2` biped slot bits. The slot number minus 30.
pub const BIPED_BODY: u32 = 1 << 2;
pub const BIPED_HANDS: u32 = 1 << 3;
pub const BIPED_FEET: u32 = 1 << 7;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActorOutfitCatalog {
    pub version: u32,
    /// One row per `NPC_` base with at least one worn part, sorted by `npc_id`.
    pub outfits: Vec<ActorOutfit>,
    /// Why items did not become parts, as `reason -> item count`. Never fatal.
    #[serde(default)]
    pub skipped: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActorOutfit {
    /// The `NPC_` FormID the outfit is worn by.
    pub npc_id: u32,
    /// The `OTFT` FormID the parts came from.
    pub outfit_id: u32,
    /// Parts in outfit order, then armature order.
    pub parts: Vec<OutfitPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutfitPart {
    /// The worn `ARMO` FormID.
    pub armo_id: u32,
    /// The selected `ARMA` FormID.
    pub armature_id: u32,
    /// `ARMA/BOD2` biped slot mask: the body regions this part covers.
    pub biped_mask: u32,
    /// The armature's own `RNAM`: the actor's race, or `DefaultRace`.
    pub armature_race_id: u32,
    /// The converted scene, a canonical `meshes/**.glb` key.
    pub scene: String,
}

impl OutfitPart {
    pub fn covers_body(&self) -> bool {
        self.biped_mask & BIPED_BODY != 0
    }

    pub fn covers_hands(&self) -> bool {
        self.biped_mask & BIPED_HANDS != 0
    }

    pub fn covers_feet(&self) -> bool {
        self.biped_mask & BIPED_FEET != 0
    }
}

impl ActorOutfitCatalog {
    /// Reject the whole catalog if a key or identity is ambiguous. An optional sidecar must not
    /// turn a malformed row into an unrelated actor's clothing.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != VERSION {
            return Err("unsupported actor outfit catalog version");
        }
        let mut seen = BTreeSet::new();
        for outfit in &self.outfits {
            if outfit.parts.is_empty() || !seen.insert(outfit.npc_id) {
                return Err("empty or duplicate actor outfit");
            }
            for part in &outfit.parts {
                if part.biped_mask == 0
                    || !crate::actor_idle_catalog::canonical_mesh_key(&part.scene, ".glb")
                {
                    return Err("invalid actor outfit part");
                }
            }
        }
        Ok(())
    }

    pub fn find(&self, npc_id: u32) -> Option<&ActorOutfit> {
        self.outfits
            .binary_search_by_key(&npc_id, |outfit| outfit.npc_id)
            .ok()
            .map(|index| &self.outfits[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(mask: u32, scene: &str) -> OutfitPart {
        OutfitPart {
            armo_id: 1,
            armature_id: 2,
            biped_mask: mask,
            armature_race_id: 25,
            scene: scene.into(),
        }
    }

    fn catalog(parts: Vec<OutfitPart>) -> ActorOutfitCatalog {
        ActorOutfitCatalog {
            version: VERSION,
            outfits: vec![ActorOutfit {
                npc_id: 7,
                outfit_id: 9,
                parts,
            }],
            skipped: BTreeMap::new(),
        }
    }

    #[test]
    fn a_well_formed_catalog_validates_and_is_found_by_npc() {
        let catalog = catalog(vec![part(BIPED_BODY, "meshes/clothes/a/torso_1.glb")]);
        assert_eq!(catalog.validate(), Ok(()));
        assert!(catalog.find(7).is_some());
        assert!(catalog.find(8).is_none());
    }

    #[test]
    fn a_malformed_part_rejects_the_catalog() {
        for bad in [
            part(0, "meshes/a.glb"),
            part(BIPED_BODY, "Meshes/A.glb"),
            part(BIPED_BODY, "meshes/../a.glb"),
            part(BIPED_BODY, "meshes/a.nif"),
        ] {
            assert!(catalog(vec![bad]).validate().is_err());
        }
        let mut wrong_version = catalog(vec![part(BIPED_BODY, "meshes/a.glb")]);
        wrong_version.version = VERSION + 1;
        assert!(wrong_version.validate().is_err());
        let mut duplicate = catalog(vec![part(BIPED_BODY, "meshes/a.glb")]);
        duplicate.outfits.push(duplicate.outfits[0].clone());
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn the_mask_names_the_regions_a_part_covers() {
        let part = part(BIPED_BODY | BIPED_FEET, "meshes/a.glb");
        assert!(part.covers_body() && part.covers_feet() && !part.covers_hands());
    }
}
