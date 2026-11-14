use super::*;
use crate::actor_models::{ActorModelRow, DEFAULT_RACE_FORM_ID};
use shared::actor_outfits::{BIPED_BODY, BIPED_FEET, BIPED_HANDS};

const NORD: u32 = 0x200;
const ARGONIAN: u32 = 0x201;

fn record(kind: &[u8; 4], id: u32, subrecords: Vec<(&[u8; 4], Vec<u8>)>) -> (u32, RawRecord) {
    (
        id,
        RawRecord {
            form_id: id,
            record_type: *kind,
            flags: 0,
            subrecords: subrecords
                .into_iter()
                .map(|(tag, data)| (tag.to_vec(), data))
                .collect(),
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        },
    )
}

fn text(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn ids(ids: &[u32]) -> Vec<u8> {
    ids.iter().flat_map(|id| id.to_le_bytes()).collect()
}

/// `NPC_` with an optional default outfit and an optional Use Inventory template.
fn npc(id: u32, outfit: Option<u32>, template: Option<u32>) -> (u32, RawRecord) {
    let mut fields = vec![(b"EDID", text("Npc"))];
    let mut acbs = vec![0u8; 24];
    if template.is_some() {
        acbs[18..20].copy_from_slice(&USE_INVENTORY.to_le_bytes());
    }
    fields.push((b"ACBS", acbs));
    if let Some(outfit) = outfit {
        fields.push((b"DOFT", ids(&[outfit])));
    }
    if let Some(template) = template {
        fields.push((b"TPLT", ids(&[template])));
    }
    record(b"NPC_", id, fields)
}

fn outfit(id: u32, items: &[u32]) -> (u32, RawRecord) {
    record(b"OTFT", id, vec![(b"INAM", ids(items))])
}

fn armor(id: u32, armatures: &[u32]) -> (u32, RawRecord) {
    record(
        b"ARMO",
        id,
        armatures.iter().map(|id| (b"MODL", ids(&[*id]))).collect(),
    )
}

fn armature(
    id: u32,
    race: u32,
    mask: u32,
    male: Option<&str>,
    female: Option<&str>,
) -> (u32, RawRecord) {
    let mut fields = vec![
        (b"BOD2", [mask.to_le_bytes(), 2u32.to_le_bytes()].concat()),
        (b"RNAM", ids(&[race])),
    ];
    if let Some(path) = male {
        fields.push((b"MOD2", text(path)));
    }
    if let Some(path) = female {
        fields.push((b"MOD3", text(path)));
    }
    record(b"ARMA", id, fields)
}

fn race(id: u32) -> (u32, RawRecord) {
    record(b"RACE", id, vec![(b"EDID", text("Race"))])
}

fn model(id: u32, race: u32, sex: &'static str, resolution: &'static str) -> ActorModelRow {
    ActorModelRow {
        id,
        race_id: race,
        sex,
        skin_armo_id: None,
        head_model: None,
        head_source: None,
        resolution,
        skin_tone: None,
        weight: None,
    }
}

fn catalog(models: Vec<ActorModelRow>) -> ActorModelCatalog {
    ActorModelCatalog {
        models,
        ..Default::default()
    }
}

fn resolve(
    records: Vec<(u32, RawRecord)>,
    models: Vec<ActorModelRow>,
    present: &[&str],
) -> ActorOutfitCatalog {
    let records: HashMap<_, _> = records.into_iter().collect();
    let present: Vec<String> = present.iter().map(|path| (*path).to_owned()).collect();
    resolve_actor_outfits(&records, &catalog(models), &|scene| {
        present.iter().any(|path| path == scene)
    })
}

const TORSO: &str = "meshes/clothes/blacksmith/m/torso_1.glb";
const SHOES: &str = "meshes/clothes/blacksmith/m/shoes_1.glb";

#[test]
fn the_default_outfit_becomes_race_and_sex_matched_parts() {
    let result = resolve(
        vec![
            npc(0x100, Some(0x300), None),
            outfit(0x300, &[0x400, 0x401]),
            armor(0x400, &[0x500]),
            armor(0x401, &[0x501]),
            armature(
                0x500,
                DEFAULT_RACE_FORM_ID,
                BIPED_BODY,
                Some("Clothes\\BlackSmith\\M\\Torso_1.nif"),
                Some("Clothes\\BlackSmith\\F\\Torso_1.nif"),
            ),
            armature(
                0x501,
                DEFAULT_RACE_FORM_ID,
                BIPED_FEET,
                Some("Clothes\\BlackSmith\\M\\Shoes_1.nif"),
                None,
            ),
            race(NORD),
        ],
        vec![model(0x100, NORD, "male", "body_head")],
        &[TORSO, SHOES],
    );
    assert_eq!(result.version, VERSION);
    assert_eq!(result.validate(), Ok(()));
    let outfit = result.find(0x100).unwrap();
    assert_eq!(outfit.outfit_id, 0x300);
    assert_eq!(
        outfit.parts,
        vec![
            OutfitPart {
                armo_id: 0x400,
                armature_id: 0x500,
                biped_mask: BIPED_BODY,
                armature_race_id: DEFAULT_RACE_FORM_ID,
                scene: TORSO.into(),
            },
            OutfitPart {
                armo_id: 0x401,
                armature_id: 0x501,
                biped_mask: BIPED_FEET,
                armature_race_id: DEFAULT_RACE_FORM_ID,
                scene: SHOES.into(),
            },
        ]
    );
}

#[test]
fn a_female_actor_uses_the_female_model_and_falls_back_to_the_male_one() {
    let records = || {
        vec![
            npc(0x100, Some(0x300), None),
            outfit(0x300, &[0x400, 0x401]),
            armor(0x400, &[0x500]),
            armor(0x401, &[0x501]),
            armature(
                0x500,
                DEFAULT_RACE_FORM_ID,
                BIPED_BODY,
                Some("a\\m.nif"),
                Some("a\\f.nif"),
            ),
            armature(
                0x501,
                DEFAULT_RACE_FORM_ID,
                BIPED_FEET,
                Some("a\\shoes.nif"),
                None,
            ),
        ]
    };
    let result = resolve(
        records(),
        vec![model(0x100, NORD, "female", "body")],
        &["meshes/a/f.glb", "meshes/a/shoes.glb"],
    );
    let scenes: Vec<_> = result
        .find(0x100)
        .unwrap()
        .parts
        .iter()
        .map(|p| p.scene.as_str())
        .collect();
    assert_eq!(scenes, ["meshes/a/f.glb", "meshes/a/shoes.glb"]);
}

#[test]
fn a_race_specific_armature_wins_its_slots_over_default_race() {
    let result = resolve(
        vec![
            npc(0x100, Some(0x300), None),
            outfit(0x300, &[0x400]),
            // The DefaultRace armature is listed first, so priority cannot be authored order.
            armor(0x400, &[0x500, 0x501, 0x502]),
            armature(
                0x500,
                DEFAULT_RACE_FORM_ID,
                BIPED_BODY,
                Some("d\\body.nif"),
                None,
            ),
            armature(0x501, ARGONIAN, BIPED_BODY, Some("a\\body.nif"), None),
            armature(
                0x502,
                DEFAULT_RACE_FORM_ID,
                BIPED_HANDS,
                Some("d\\hands.nif"),
                None,
            ),
            race(ARGONIAN),
        ],
        vec![model(0x100, ARGONIAN, "male", "body")],
        &[
            "meshes/d/body.glb",
            "meshes/a/body.glb",
            "meshes/d/hands.glb",
        ],
    );
    let parts = &result.find(0x100).unwrap().parts;
    let scenes: Vec<_> = parts.iter().map(|p| p.scene.as_str()).collect();
    assert_eq!(scenes, ["meshes/a/body.glb", "meshes/d/hands.glb"]);
    assert_eq!(parts[0].armature_race_id, ARGONIAN);
}

#[test]
fn an_armature_for_another_race_is_not_worn() {
    let result = resolve(
        vec![
            npc(0x100, Some(0x300), None),
            outfit(0x300, &[0x400]),
            armor(0x400, &[0x500]),
            armature(0x500, ARGONIAN, BIPED_BODY, Some("a\\body.nif"), None),
            race(ARGONIAN),
        ],
        vec![model(0x100, NORD, "male", "body")],
        &["meshes/a/body.glb"],
    );
    assert!(result.find(0x100).is_none());
    assert_eq!(result.skipped["no_compatible_armature"], 1);
}

#[test]
fn skipped_items_are_counted_and_never_fatal() {
    let result = resolve(
        vec![
            npc(0x100, Some(0x300), None),
            outfit(0x300, &[0x400, 0x401, 0x402, 0x403, 0x404, 0x405]),
            armor(0x400, &[0x500]),
            record(b"LVLI", 0x401, vec![]),
            record(b"WEAP", 0x402, vec![]),
            armor(0x403, &[]),
            armor(0x404, &[0x501]),
            // 0x405 has no record at all.
            armature(
                0x500,
                DEFAULT_RACE_FORM_ID,
                BIPED_BODY,
                Some("x\\gone.nif"),
                None,
            ),
            armature(
                0x501,
                DEFAULT_RACE_FORM_ID,
                BIPED_FEET,
                Some("x\\shoes.nif"),
                None,
            ),
        ],
        vec![model(0x100, NORD, "male", "body")],
        &["meshes/x/shoes.glb"],
    );
    assert_eq!(result.find(0x100).unwrap().parts.len(), 1);
    let expected: BTreeMap<String, usize> = [
        ("converted_model_missing", 1),
        ("leveled_list", 1),
        ("not_armor", 1),
        ("armor_without_armature", 1),
        ("item_missing", 1),
    ]
    .into_iter()
    .map(|(reason, count)| (reason.to_owned(), count))
    .collect();
    assert_eq!(result.skipped, expected);
}

#[test]
fn unresolved_and_raceless_actors_get_no_outfit() {
    let result = resolve(
        vec![
            npc(0x100, Some(0x300), None),
            npc(0x101, Some(0x300), None),
            outfit(0x300, &[0x400]),
            armor(0x400, &[0x500]),
            armature(
                0x500,
                DEFAULT_RACE_FORM_ID,
                BIPED_BODY,
                Some("a\\b.nif"),
                None,
            ),
        ],
        vec![
            model(0x100, NORD, "male", "unresolved"),
            model(0x101, 0, "male", "body"),
        ],
        &["meshes/a/b.glb"],
    );
    assert!(result.outfits.is_empty());
}

#[test]
fn a_use_inventory_template_supplies_a_missing_outfit_and_cycles_end() {
    let records = || {
        vec![
            npc(0x100, None, Some(0x101)),
            npc(0x101, Some(0x300), None),
            npc(0x102, None, Some(0x103)),
            npc(0x103, None, Some(0x102)),
            // Flag clear: the template is not followed.
            npc(0x104, None, None),
            outfit(0x300, &[0x400]),
            armor(0x400, &[0x500]),
            armature(
                0x500,
                DEFAULT_RACE_FORM_ID,
                BIPED_BODY,
                Some("a\\b.nif"),
                None,
            ),
        ]
    };
    let result = resolve(
        records(),
        vec![
            model(0x100, NORD, "male", "body"),
            model(0x102, NORD, "male", "body"),
            model(0x104, NORD, "male", "body"),
        ],
        &["meshes/a/b.glb"],
    );
    let wearers: Vec<u32> = result.outfits.iter().map(|outfit| outfit.npc_id).collect();
    assert_eq!(wearers, [0x100]);
}

#[test]
fn a_deleted_outfit_record_is_ignored() {
    let mut records: Vec<_> = vec![
        npc(0x100, Some(0x300), None),
        outfit(0x300, &[0x400]),
        armor(0x400, &[0x500]),
        armature(
            0x500,
            DEFAULT_RACE_FORM_ID,
            BIPED_BODY,
            Some("a\\b.nif"),
            None,
        ),
    ];
    records[1].1.flags = 0x20;
    let result = resolve(
        records,
        vec![model(0x100, NORD, "male", "body")],
        &["meshes/a/b.glb"],
    );
    assert!(result.outfits.is_empty());
}
