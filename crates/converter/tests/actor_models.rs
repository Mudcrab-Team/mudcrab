//! The actor model catalog through the real ESM parser and database export.
//!
//! Every fixture is synthesized by `dummy-content`; the retail tests read a local
//! Skyrim install and are opt-in.

use converter::actor_models::{ActorModelCatalog, UnresolvedReason, resolve_actor_models};
use converter::esm::{EsmParser, load_order::LoadOrder};
use dummy_content::inhouse_items as items;
use rusqlite::Connection;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

const MALE_BODY: &str = "actors\\character\\character assets\\malebody_1.nif";
const MALE_HANDS: &str = "actors\\character\\character assets\\malehands_1.nif";
const MALE_FEET: &str = "actors\\character\\character assets\\malefeet_1.nif";
const FEMALE_BODY: &str = "actors\\character\\character assets\\femalebody_1.nif";
const OTHER_BODY: &str = "actors\\character\\custom\\otherbody.nif";

/// A plugin file: an empty `TES4` header followed by the fixture records.
fn plugin(records: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = items::record(b"TES4", 0, &[]);
    for record in records {
        bytes.extend(record);
    }
    bytes
}

/// Writes the fixture plugin under `name` and returns its path.
fn write_plugin(directory: &Path, name: &str, records: &[Vec<u8>]) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, plugin(records)).unwrap();
    path
}

/// Merges the plugin and resolves its catalog, mapping each `NPC_` to its plugin.
fn resolve(path: &Path) -> ActorModelCatalog {
    let paths = [path.to_path_buf()];
    let records = EsmParser::merge_plugins(&paths).unwrap();
    let order = LoadOrder::read(&paths).unwrap();
    let plugin_names: HashMap<u32, String> = records
        .iter()
        .filter(|(_, record)| &record.record_type == b"NPC_")
        .filter_map(|(id, _)| {
            order
                .identity(*id)
                .ok()
                .map(|identity| (*id, identity.plugin))
        })
        .collect();
    resolve_actor_models(&records, &plugin_names)
}

/// Converts the plugin into a fresh database and returns the open connection.
fn export(directory: &Path, path: &Path) -> Connection {
    let db = directory.join("skyrim_world.db");
    EsmParser::convert_plugins(std::slice::from_ref(&path.to_path_buf()), &db).unwrap();
    Connection::open(&db).unwrap()
}

/// A `NordRace` skin whose one armature fills the body slot.
fn nord_body_only(skin: u32) -> Vec<Vec<u8>> {
    vec![
        items::race(0x200, "NordRace", Some(skin), false),
        items::skin_armor(skin, &[0x400]),
        items::armature(
            0x400,
            items::DEFAULT_RACE_FORM_ID,
            items::BIPED_BODY,
            Some(MALE_BODY),
            Some(FEMALE_BODY),
        ),
    ]
}

#[test]
fn body_chain_follows_race_skin_armature() {
    let directory = tempfile::tempdir().unwrap();
    let mut records = vec![items::npc(0x100, 0x200, false)];
    records.extend(nord_body_only(0x300));
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let row = catalog.models.iter().find(|row| row.id == 0x100).unwrap();
    assert_eq!(row.race_id, 0x200);
    assert_eq!(row.sex, "male");
    assert_eq!(row.skin_armo_id, Some(0x300));
    assert_eq!(row.resolution, "body_head");

    let parts: Vec<_> = catalog
        .parts
        .iter()
        .filter(|part| part.npc_id == 0x100)
        .collect();
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert_eq!(parts[0].slot.as_str(), "body");
    assert_eq!(parts[0].model_path, MALE_BODY);
}

#[test]
fn exact_race_arma_wins_over_default_race() {
    let directory = tempfile::tempdir().unwrap();
    let records = vec![
        items::npc(0x100, 0x200, false),
        items::race(0x200, "NordRace", Some(0x300), false),
        // The default-race armature is listed first, so priority cannot be file order.
        items::skin_armor(0x300, &[0x400, 0x401]),
        items::armature(
            0x400,
            items::DEFAULT_RACE_FORM_ID,
            items::BIPED_BODY,
            Some(OTHER_BODY),
            None,
        ),
        items::armature(0x401, 0x200, items::BIPED_BODY, Some(MALE_BODY), None),
    ];
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let parts: Vec<_> = catalog
        .parts
        .iter()
        .filter(|part| part.npc_id == 0x100)
        .collect();
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert_eq!(parts[0].model_path, MALE_BODY);
}

#[test]
fn default_race_is_the_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let mut records = vec![items::npc(0x100, 0x200, false)];
    records.extend(nord_body_only(0x300));
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let parts: Vec<_> = catalog
        .parts
        .iter()
        .filter(|part| part.npc_id == 0x100)
        .collect();
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert_eq!(parts[0].model_path, MALE_BODY);
    assert!(
        catalog
            .models
            .iter()
            .any(|row| row.id == 0x100 && row.resolution == "body_head")
    );
}

#[test]
fn female_acbs_bit_selects_mod3() {
    let directory = tempfile::tempdir().unwrap();
    let records = vec![
        items::npc(0x100, 0x200, true),
        items::race(0x200, "NordRace", Some(0x300), false),
        items::skin_armor(0x300, &[0x400]),
        items::armature(
            0x400,
            items::DEFAULT_RACE_FORM_ID,
            items::BIPED_BODY,
            Some(MALE_BODY),
            Some(FEMALE_BODY),
        ),
    ];
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let row = catalog.models.iter().find(|row| row.id == 0x100).unwrap();
    assert_eq!(row.sex, "female");
    let parts: Vec<_> = catalog
        .parts
        .iter()
        .filter(|part| part.npc_id == 0x100)
        .collect();
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert_eq!(parts[0].model_path, FEMALE_BODY);
    assert_eq!(
        row.head_model.as_deref(),
        Some("meshes/actors/character/character assets/femalehead.nif")
    );
    assert_eq!(row.head_source, Some("default"));
}

#[test]
fn missing_race_is_recorded_not_dropped() {
    let directory = tempfile::tempdir().unwrap();
    let records = vec![
        items::record(
            b"NPC_",
            0x100,
            &[
                (b"EDID", items::text("NoRaceLink")),
                (b"ACBS", vec![0u8; 24]),
            ],
        ),
        items::npc(0x101, 0x999, false),
    ];
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let no_rnam = catalog.models.iter().find(|row| row.id == 0x100).unwrap();
    assert_eq!(no_rnam.race_id, 0);
    assert_eq!(no_rnam.resolution, "unresolved");
    let no_record = catalog.models.iter().find(|row| row.id == 0x101).unwrap();
    assert_eq!(no_record.race_id, 0x999);
    assert_eq!(no_record.resolution, "unresolved");
    assert!(
        catalog
            .diagnostics
            .contains(&converter::actor_models::ActorModelDiagnostic {
                npc_id: 0x100,
                reason: UnresolvedReason::NoRnam,
            })
    );
    assert!(
        catalog
            .diagnostics
            .contains(&converter::actor_models::ActorModelDiagnostic {
                npc_id: 0x101,
                reason: UnresolvedReason::NoRaceRecord,
            })
    );

    let connection = export(directory.path(), &path);
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM actor_models", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2, "both unresolved bases are recorded");
    let resolution: String = connection
        .query_row(
            "SELECT resolution FROM actor_models WHERE id=0x100",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(resolution, "unresolved");
}

#[test]
fn facegen_path_wins_over_default_head() {
    let directory = tempfile::tempdir().unwrap();
    let records = vec![
        items::npc(0x100, 0x200, false),
        items::race(0x200, "NordRace", Some(0x300), true),
        items::npc(0x101, 0x201, false),
        items::race(0x201, "NordRace", Some(0x300), false),
        items::skin_armor(0x300, &[0x400]),
        items::armature(
            0x400,
            items::DEFAULT_RACE_FORM_ID,
            items::BIPED_BODY,
            Some(MALE_BODY),
            None,
        ),
    ];
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let facegen = catalog.models.iter().find(|row| row.id == 0x100).unwrap();
    assert_eq!(
        facegen.head_model.as_deref(),
        Some("meshes/actors/character/facegendata/facegeom/skyrim.esm/00000100.nif")
    );
    assert_eq!(facegen.head_source, Some("facegen"));
    let default = catalog.models.iter().find(|row| row.id == 0x101).unwrap();
    assert_eq!(
        default.head_model.as_deref(),
        Some("meshes/actors/character/character assets/malehead.nif")
    );
    assert_eq!(default.head_source, Some("default"));
}

#[test]
fn alvor_resolves_body_hands_feet_and_facegen_head() {
    let directory = tempfile::tempdir().unwrap();
    let records = vec![
        items::npc(0x0001_3475, 79686, false),
        items::npc(0x0001_3476, 79686, true),
        items::race(79686, "NordRace", Some(3428), true),
        items::skin_armor(3428, &[0x500, 0x501, 0x502]),
        items::armature(
            0x500,
            items::DEFAULT_RACE_FORM_ID,
            items::BIPED_BODY,
            Some(MALE_BODY),
            Some(FEMALE_BODY),
        ),
        items::armature(
            0x501,
            items::DEFAULT_RACE_FORM_ID,
            items::BIPED_HANDS,
            Some(MALE_HANDS),
            None,
        ),
        items::armature(
            0x502,
            items::DEFAULT_RACE_FORM_ID,
            items::BIPED_FEET,
            Some(MALE_FEET),
            None,
        ),
    ];
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let alvor = catalog
        .models
        .iter()
        .find(|row| row.id == 0x0001_3475)
        .expect("Alvor's row");
    assert_eq!(alvor.race_id, 79686);
    assert_eq!(alvor.sex, "male");
    assert_eq!(alvor.skin_armo_id, Some(3428));
    assert_eq!(alvor.resolution, "body_head");
    assert_eq!(
        alvor.head_model.as_deref(),
        Some("meshes/actors/character/facegendata/facegeom/skyrim.esm/00013475.nif")
    );
    assert_eq!(alvor.head_source, Some("facegen"));

    let mut parts: Vec<_> = catalog
        .parts
        .iter()
        .filter(|part| part.npc_id == 0x0001_3475)
        .collect();
    parts.sort_by_key(|part| part.slot);
    assert_eq!(parts.len(), 3, "{parts:?}");
    assert_eq!(parts[0].slot.as_str(), "body");
    assert_eq!(parts[0].model_path, MALE_BODY);
    assert_eq!(parts[1].slot.as_str(), "hands");
    assert_eq!(parts[1].model_path, MALE_HANDS);
    assert_eq!(parts[2].slot.as_str(), "feet");
    assert_eq!(parts[2].model_path, MALE_FEET);

    let sigrid = catalog
        .models
        .iter()
        .find(|row| row.id == 0x0001_3476)
        .expect("Sigrid's row");
    println!("Sigrid (0x00013476) ACBS sex: {}", sigrid.sex);
}

/// Retail bodies: the full resolved/unresolved breakdown, Alvor's row and the
/// `statics` invariant. The 2,392 count is printed, not asserted.
#[test]
#[ignore = "requires MUDCRAB_SKYRIM_DATA with locally installed game assets"]
fn referenced_npc_bases_resolve_bodies() {
    let Some(directory) = std::env::var_os("MUDCRAB_SKYRIM_DATA").map(PathBuf::from) else {
        return;
    };
    let plugin = directory.join("Skyrim.esm");
    let temp = tempfile::tempdir().unwrap();
    let connection = export(temp.path(), &plugin);

    let overlap: i64 = connection
        .query_row(
            "SELECT count(*) FROM statics WHERE id IN (SELECT id FROM npcs)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(overlap, 0, "no NPC_ row is written into statics");

    let referenced: HashSet<u32> = connection
        .prepare(
            "SELECT DISTINCT base_form_id FROM \"references\" WHERE base_form_id IN (SELECT id FROM npcs)",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(!referenced.is_empty());

    let catalog = resolve(&plugin);
    let alvor = catalog
        .models
        .iter()
        .find(|row| row.id == 0x0001_3475)
        .expect("Alvor's row");
    assert_eq!(alvor.race_id, 79686);
    assert_eq!(alvor.sex, "male");
    assert_eq!(alvor.skin_armo_id, Some(3428));
    assert_eq!(alvor.head_source, Some("facegen"));
    assert_eq!(
        alvor.head_model.as_deref(),
        Some("meshes/actors/character/facegendata/facegeom/skyrim.esm/00013475.nif")
    );

    let mut parts: Vec<_> = catalog
        .parts
        .iter()
        .filter(|part| part.npc_id == 0x0001_3475)
        .collect();
    parts.sort_by_key(|part| part.slot);
    assert_eq!(parts.len(), 3, "Alvor has body, hands and feet: {parts:?}");
    for (part, expected) in
        parts
            .iter()
            .zip(["malebody_1.nif", "malehands_1.nif", "malefeet_1.nif"])
    {
        assert!(
            part.model_path.to_lowercase().ends_with(expected),
            "{}: {}",
            part.slot.as_str(),
            part.model_path
        );
    }

    let sigrid = catalog
        .models
        .iter()
        .find(|row| row.id == 0x0001_3476)
        .expect("Sigrid's row");
    println!("Sigrid (0x00013476) ACBS sex: {}", sigrid.sex);
    println!("Alvor: {alvor:?}");
    for part in &parts {
        println!("Alvor part {}: {}", part.slot.as_str(), part.model_path);
    }

    let referenced_models: Vec<_> = catalog
        .models
        .iter()
        .filter(|row| referenced.contains(&row.id))
        .collect();
    let resolved = referenced_models
        .iter()
        .filter(|row| row.resolution != "unresolved")
        .count();
    let mut reasons: HashMap<&str, usize> = HashMap::new();
    for diagnostic in &catalog.diagnostics {
        if referenced.contains(&diagnostic.npc_id)
            && catalog
                .models
                .iter()
                .any(|row| row.id == diagnostic.npc_id && row.resolution == "unresolved")
        {
            *reasons.entry(diagnostic.reason.as_str()).or_default() += 1;
        }
    }
    println!(
        "referenced NPC bases: {} total, {resolved} resolved, {} unresolved",
        referenced_models.len(),
        referenced_models.len() - resolved
    );
    let mut reasons: Vec<_> = reasons.into_iter().collect();
    reasons.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(right.0)));
    for (reason, count) in reasons {
        println!("  unresolved {reason}: {count}");
    }
}

/// Retail FaceGen heads: the count is printed, not asserted.
#[test]
#[ignore = "requires MUDCRAB_SKYRIM_DATA with locally installed game assets"]
fn referenced_npc_bases_resolve_facegen_heads() {
    let Some(directory) = std::env::var_os("MUDCRAB_SKYRIM_DATA").map(PathBuf::from) else {
        return;
    };
    let plugin = directory.join("Skyrim.esm");
    let temp = tempfile::tempdir().unwrap();
    let connection = export(temp.path(), &plugin);
    let referenced: HashSet<u32> = connection
        .prepare(
            "SELECT DISTINCT base_form_id FROM \"references\" WHERE base_form_id IN (SELECT id FROM npcs)",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    let catalog = resolve(&plugin);
    let facegen = catalog
        .models
        .iter()
        .filter(|row| referenced.contains(&row.id) && row.head_source == Some("facegen"))
        .count();
    println!(
        "referenced NPC bases with a FaceGen head: {facegen} of {}",
        referenced.len()
    );
    assert!(facegen > 0);
}
