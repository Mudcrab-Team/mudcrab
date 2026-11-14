//! Worn outfit catalog: `NPC_/DOFT` -> `OTFT` -> `ARMO` -> `ARMA` -> converted scene.
//!
//! The actor model catalog ([`crate::actor_models`]) names the skin an actor is drawn with. This
//! module adds what the actor wears: the default outfit's armor, each armor's armatures matched to
//! the actor's race and sex, and the converted scene each armature draws. It reads the merged
//! record map and the already-resolved actor models, so race and sex come from one place, and the
//! race match is the same rule skin armor uses ([`crate::actor_models::partition_armatures`]).
//!
//! Skipped items never fail the catalog. They are counted by reason, so a leveled list or a model
//! that is not in the pack is visible without stopping the actor from spawning.

use crate::actor_models::{ActorModelCatalog, partition_armatures};
use crate::esm::{extractors::SubrecordView, records::RawRecord};
use shared::actor_outfits::{ActorOutfit, ActorOutfitCatalog, OutfitPart, VERSION};
use std::collections::{BTreeMap, HashMap, HashSet};

/// `NPC_/ACBS` template flag: the actor uses its template's inventory and outfit.
const USE_INVENTORY: u16 = 0x0100;

/// Resolves the default outfit of every resolved `NPC_` base.
///
/// `scene_exists` receives a canonical `meshes/**.glb` key and says whether the converted pack
/// holds it. A part whose scene is absent is skipped, so the engine is never told to draw a model
/// the pack does not have.
#[must_use]
pub fn resolve_actor_outfits(
    records: &HashMap<u32, RawRecord>,
    models: &ActorModelCatalog,
    scene_exists: &dyn Fn(&str) -> bool,
) -> ActorOutfitCatalog {
    let mut catalog = ActorOutfitCatalog {
        version: VERSION,
        ..Default::default()
    };
    for model in &models.models {
        // An unresolved row never spawns, and a row without a race cannot match armatures.
        if model.resolution == "unresolved" || model.race_id == 0 {
            continue;
        }
        let Some(outfit_id) = effective_outfit(records, model.id) else {
            continue;
        };
        let female = model.sex == "female";
        let mut parts = Vec::new();
        let mut skipped = BTreeMap::new();
        for item in outfit_items(records, outfit_id) {
            resolve_item(
                records,
                item,
                model.race_id,
                female,
                scene_exists,
                &mut parts,
                &mut skipped,
            );
        }
        for (reason, count) in skipped {
            *catalog.skipped.entry(reason.to_owned()).or_default() += count;
        }
        if !parts.is_empty() {
            catalog.outfits.push(ActorOutfit {
                npc_id: model.id,
                outfit_id,
                parts,
            });
        }
    }
    catalog.outfits.sort_by_key(|outfit| outfit.npc_id);
    catalog
}

/// The `OTFT` an actor wears by default: its own `DOFT`, else the one its Use Inventory template
/// chain supplies. A cycle or a non-`NPC_` template ends the chain.
fn effective_outfit(records: &HashMap<u32, RawRecord>, npc_id: u32) -> Option<u32> {
    let mut current = npc_id;
    let mut visited = HashSet::from([npc_id]);
    loop {
        let record = records
            .get(&current)
            .filter(|record| &record.record_type == b"NPC_" && !record.is_deleted())?;
        let view = SubrecordView::new(&record.subrecords);
        if let Some(outfit) = view.get_form_id(b"DOFT").filter(|id| {
            records
                .get(id)
                .is_some_and(|record| &record.record_type == b"OTFT" && !record.is_deleted())
        }) {
            return Some(outfit);
        }
        let uses_inventory = view
            .find(b"ACBS")
            .filter(|data| data.len() >= 20)
            .is_some_and(|data| u16::from_le_bytes([data[18], data[19]]) & USE_INVENTORY != 0);
        let template = view.get_form_id(b"TPLT").filter(|id| *id != 0);
        match template {
            Some(template) if uses_inventory && visited.insert(template) => current = template,
            _ => return None,
        }
    }
}

/// Every item FormID of an `OTFT`, in order. `INAM` holds the whole list.
fn outfit_items(records: &HashMap<u32, RawRecord>, outfit_id: u32) -> Vec<u32> {
    let Some(record) = records.get(&outfit_id) else {
        return Vec::new();
    };
    SubrecordView::new(&record.subrecords)
        .find_all(b"INAM")
        .flat_map(|data| data.as_chunks::<4>().0.iter())
        .map(|chunk| u32::from_le_bytes(*chunk))
        .collect()
}

fn resolve_item(
    records: &HashMap<u32, RawRecord>,
    item: u32,
    race_id: u32,
    female: bool,
    scene_exists: &dyn Fn(&str) -> bool,
    parts: &mut Vec<OutfitPart>,
    skipped: &mut BTreeMap<&'static str, usize>,
) {
    let mut skip = |reason: &'static str| *skipped.entry(reason).or_default() += 1;
    let Some(record) = records.get(&item).filter(|record| !record.is_deleted()) else {
        return skip("item_missing");
    };
    match &record.record_type {
        b"ARMO" => {}
        b"LVLI" => return skip("leveled_list"),
        _ => return skip("not_armor"),
    }
    let armatures = SubrecordView::new(&record.subrecords).get_form_ids(b"MODL");
    if armatures.is_empty() {
        return skip("armor_without_armature");
    }
    let (compatible, default) = partition_armatures(records, &armatures, race_id);
    let mut covered = 0u32;
    let mut found = false;
    for (armature_id, armature_race_id) in compatible.into_iter().chain(default) {
        found = true;
        let view = SubrecordView::new(&records[&armature_id].subrecords);
        let Some(mask) = view
            .find(b"BOD2")
            .or_else(|| view.find(b"BODT"))
            .filter(|data| data.len() >= 4)
            .map(|data| u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
            .filter(|mask| *mask != 0)
        else {
            skip("armature_without_slots");
            continue;
        };
        // A race-specific armature wins its slots over the DefaultRace one, as for skin armor.
        if covered & mask != 0 {
            continue;
        }
        // An armature without a female model uses MOD2 for both sexes.
        let male = || view.get_string(b"MOD2").filter(|path| !path.is_empty());
        let path = if female {
            view.get_string(b"MOD3")
                .filter(|path| !path.is_empty())
                .or_else(male)
        } else {
            male()
        };
        let Some(path) = path else {
            skip("armature_without_model");
            continue;
        };
        let Ok(scene) = shared::actor_idle_catalog::canonical_body_path(&path, "glb") else {
            skip("invalid_model_path");
            continue;
        };
        if !scene_exists(&scene) {
            skip("converted_model_missing");
            continue;
        }
        covered |= mask;
        parts.push(OutfitPart {
            armo_id: item,
            armature_id,
            biped_mask: mask,
            armature_race_id,
            scene,
        });
    }
    if !found {
        skip("no_compatible_armature");
    }
}

#[cfg(test)]
mod tests;
