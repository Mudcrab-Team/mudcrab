//! The actor model catalog through the real ESM parser and database export.
//!
//! Every fixture is synthesized by `dummy-content`; the retail tests read a local
//! Skyrim install and are opt-in.

use converter::actor_models::{ActorModelCatalog, UnresolvedReason, resolve_actor_models};
use converter::esm::{
    EsmParser,
    load_order::{LoadOrder, StableId},
};
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

/// Writes a plugin whose `TES4` header declares `masters`, so its own records use the next index.
fn write_master_plugin(
    directory: &Path,
    name: &str,
    masters: &[&str],
    records: &[Vec<u8>],
) -> PathBuf {
    let mut fields: Vec<(&[u8; 4], Vec<u8>)> = Vec::new();
    for master in masters {
        fields.push((b"MAST", items::text(master)));
        fields.push((b"DATA", vec![0xa5; 8]));
    }
    let mut bytes = items::record(b"TES4", 0, &fields);
    for record in records {
        bytes.extend(record);
    }
    let path = directory.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// Merges the plugin and resolves its catalog.
fn resolve(path: &Path) -> ActorModelCatalog {
    let paths = [path.to_path_buf()];
    resolve_plugins(&paths)
}

/// Merges the plugins and resolves their catalog, mapping each `NPC_` to its stable identity.
fn resolve_plugins(paths: &[PathBuf]) -> ActorModelCatalog {
    let records = EsmParser::merge_plugins(paths).unwrap();
    let order = LoadOrder::read(paths).unwrap();
    let plugin_ids: HashMap<u32, StableId> = records
        .iter()
        .filter(|(_, record)| &record.record_type == b"NPC_")
        .filter_map(|(id, _)| order.identity(*id).ok().map(|identity| (*id, identity)))
        .collect();
    resolve_actor_models(&records, &plugin_ids)
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
fn female_without_mod3_falls_back_to_mod2() {
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
            None,
        ),
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
    assert!(
        !catalog
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.npc_id == 0x100),
        "{:?}",
        catalog.diagnostics
    );
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
fn facegen_path_uses_the_plugin_local_form_id() {
    let directory = tempfile::tempdir().unwrap();
    let skyrim = write_plugin(
        directory.path(),
        "Skyrim.esm",
        &[
            items::race(0x200, "NordRace", Some(0x300), true),
            items::skin_armor(0x300, &[0x400]),
            items::armature(
                0x400,
                items::DEFAULT_RACE_FORM_ID,
                items::BIPED_BODY,
                Some(MALE_BODY),
                None,
            ),
        ],
    );
    let patch = write_master_plugin(
        directory.path(),
        "Patch.esp",
        &["Skyrim.esm"],
        &[items::npc(0x0100_0D62, 0x200, false)],
    );

    let catalog = resolve_plugins(&[skyrim, patch]);
    let row = catalog
        .models
        .iter()
        .find(|row| row.id == 0x0100_0D62)
        .expect("the NPC_ under its load-order FormID");
    assert_eq!(
        row.head_model.as_deref(),
        Some("meshes/actors/character/facegendata/facegeom/patch.esp/00000d62.nif")
    );
    assert_eq!(row.head_source, Some("facegen"));
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

/// An `NPC_` like [`items::npc`] that also carries a `QNAM` skin tone.
fn npc_with_skin_tone(id: u32, race_id: u32, tone: [f32; 3]) -> Vec<u8> {
    items::record(
        b"NPC_",
        id,
        &[
            (b"EDID", items::text("TonedNpc")),
            (b"ACBS", vec![0u8; 24]),
            (b"RNAM", race_id.to_le_bytes().to_vec()),
            (
                b"QNAM",
                tone.iter()
                    .flat_map(|channel| channel.to_le_bytes())
                    .collect(),
            ),
        ],
    )
}

/// The `actor_models` skin tone columns of `id`.
fn stored_skin_tone(connection: &Connection, id: u32) -> [Option<f64>; 3] {
    connection
        .query_row(
            "SELECT skin_tone_r, skin_tone_g, skin_tone_b FROM actor_models WHERE id=?1",
            [id],
            |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?]),
        )
        .unwrap()
}

#[test]
fn qnam_skin_tone_is_published_and_absent_qnam_stays_null() {
    let directory = tempfile::tempdir().unwrap();
    let tone = [198.0 / 255.0, 176.0 / 255.0, 168.0 / 255.0];
    let mut records = vec![
        npc_with_skin_tone(0x100, 0x200, tone),
        items::npc(0x101, 0x200, false),
    ];
    records.extend(nord_body_only(0x300));
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let toned = catalog.models.iter().find(|row| row.id == 0x100).unwrap();
    assert_eq!(toned.skin_tone, Some(tone));
    let plain = catalog.models.iter().find(|row| row.id == 0x101).unwrap();
    assert_eq!(plain.skin_tone, None);

    let connection = export(directory.path(), &path);
    assert_eq!(
        stored_skin_tone(&connection, 0x100),
        tone.map(|channel| Some(f64::from(channel)))
    );
    assert_eq!(stored_skin_tone(&connection, 0x101), [None; 3]);
}

/// The `actor_models.weight` of `id`.
fn stored_weight(connection: &Connection, id: u32) -> Option<f64> {
    connection
        .query_row("SELECT weight FROM actor_models WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn nam7_body_weight_is_published_and_an_absent_nam7_stays_null() {
    let directory = tempfile::tempdir().unwrap();
    let mut records = vec![
        items::npc_with_weight(0x100, 0x200, false, 20.0),
        items::npc(0x101, 0x200, false),
        items::npc_with_weight(0x102, 0x200, false, 100.0),
    ];
    records.extend(nord_body_only(0x300));
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let light = catalog.models.iter().find(|row| row.id == 0x100).unwrap();
    assert_eq!(light.weight, Some(20.0));
    let plain = catalog.models.iter().find(|row| row.id == 0x101).unwrap();
    assert_eq!(plain.weight, None);
    let heavy = catalog.models.iter().find(|row| row.id == 0x102).unwrap();
    assert_eq!(heavy.weight, Some(100.0));

    let connection = export(directory.path(), &path);
    assert_eq!(stored_weight(&connection, 0x100), Some(20.0));
    assert_eq!(stored_weight(&connection, 0x101), None);
    assert_eq!(stored_weight(&connection, 0x102), Some(100.0));
}

#[test]
fn an_out_of_range_or_non_finite_nam7_stays_null() {
    let directory = tempfile::tempdir().unwrap();
    let weights = [100.5_f32, -0.5, f32::NAN, f32::INFINITY];
    let mut records: Vec<Vec<u8>> = weights
        .iter()
        .enumerate()
        .map(|(index, weight)| items::npc_with_weight(0x100 + index as u32, 0x200, false, *weight))
        .collect();
    records.extend(nord_body_only(0x300));
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let connection = export(directory.path(), &path);
    for (index, weight) in weights.iter().enumerate() {
        let id = 0x100 + index as u32;
        let row = catalog.models.iter().find(|row| row.id == id).unwrap();
        assert_eq!(row.weight, None, "{weight}");
        assert_eq!(stored_weight(&connection, id), None, "{weight}");
    }
}

#[test]
fn a_templated_npc_keeps_its_own_nam7_and_never_inherits_one() {
    let directory = tempfile::tempdir().unwrap();
    let mut records = vec![
        // The template itself is light, but a templated base reads only its own NAM7.
        items::npc_with_weight(0x900, 0x200, false, 80.0),
        items::npc_with_template(0x100, 0x200, false, 0x900, Some(20.0)),
        items::npc_with_template(0x101, 0x200, false, 0x900, None),
    ];
    records.extend(nord_body_only(0x300));
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);

    let catalog = resolve(&path);
    let own = catalog.models.iter().find(|row| row.id == 0x100).unwrap();
    assert_eq!(own.weight, Some(20.0));
    let no_nam7 = catalog.models.iter().find(|row| row.id == 0x101).unwrap();
    assert_eq!(no_nam7.weight, None);

    let connection = export(directory.path(), &path);
    assert_eq!(stored_weight(&connection, 0x100), Some(20.0));
    assert_eq!(stored_weight(&connection, 0x101), None);
}

/// A database whose `actor_models` predates the skin tone and weight columns gains them when the
/// tables are created again, so a subset export into it can still insert its rows.
#[test]
fn an_older_actor_models_table_gains_the_skin_tone_and_weight_columns() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE actor_models (
                 id INTEGER PRIMARY KEY, race_id INTEGER NOT NULL, sex TEXT NOT NULL,
                 skin_armo_id INTEGER, head_model TEXT, head_source TEXT,
                 resolution TEXT NOT NULL
             );
             INSERT INTO actor_models(id, race_id, sex, resolution)
                 VALUES (0x100, 0x200, 'male', 'body');",
        )
        .unwrap();
    converter::esm::exporter::create_tables(&connection).unwrap();
    converter::esm::exporter::create_tables(&connection).unwrap();
    assert_eq!(stored_skin_tone(&connection, 0x100), [None; 3]);
    assert_eq!(stored_weight(&connection, 0x100), None);
}

const FEMALE_HANDS: &str = "actors\\character\\character assets\\femalehands_1.nif";

/// A `NordRace` whose skin holds an exact-race body and a `DefaultRace` hands armature, each with
/// a male and a female skin texture set, and one NPC of each sex.
fn nord_with_skin_texture_sets() -> Vec<Vec<u8>> {
    vec![
        items::npc(0x100, 0x200, false),
        items::npc(0x101, 0x200, true),
        items::race(0x200, "NordRace", Some(0x300), false),
        items::skin_armor(0x300, &[0x400, 0x401]),
        items::armature_with_skins(
            0x400,
            0x200,
            items::BIPED_BODY,
            (Some(MALE_BODY), Some(FEMALE_BODY)),
            (Some(0x500), Some(0x501)),
        ),
        items::armature_with_skins(
            0x401,
            items::DEFAULT_RACE_FORM_ID,
            items::BIPED_HANDS,
            (Some(MALE_HANDS), Some(FEMALE_HANDS)),
            (Some(0x502), None),
        ),
    ]
}

#[test]
fn parts_carry_the_armature_race_and_the_sex_skin_texture_set() {
    let directory = tempfile::tempdir().unwrap();
    let path = write_plugin(
        directory.path(),
        "Skyrim.esm",
        &nord_with_skin_texture_sets(),
    );

    let catalog = resolve(&path);
    let parts: Vec<_> = catalog
        .parts
        .iter()
        .map(|part| {
            (
                part.npc_id,
                part.slot.as_str(),
                part.armature_race_id,
                part.skin_texture_set_id,
            )
        })
        .collect();
    assert_eq!(
        parts,
        [
            (0x100, "body", 0x200, Some(0x500)),
            (0x100, "hands", items::DEFAULT_RACE_FORM_ID, Some(0x502)),
            (0x101, "body", 0x200, Some(0x501)),
            // NAM1 is absent: the female hands keep the NIF's own textures.
            (0x101, "hands", items::DEFAULT_RACE_FORM_ID, None),
        ]
    );

    let connection = export(directory.path(), &path);
    let stored: Vec<(u32, String, u32, Option<u32>)> = connection
        .prepare(
            "SELECT npc_id, slot, armature_race_id, skin_texture_set_id FROM actor_model_parts \
             ORDER BY npc_id, slot",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let expected: Vec<(u32, String, u32, Option<u32>)> = parts
        .iter()
        .map(|(npc, slot, race, set)| (*npc, (*slot).to_owned(), *race, *set))
        .collect();
    assert_eq!(stored, expected);
}

#[test]
fn the_default_body_takes_the_default_race_armature_skin_texture_sets() {
    let directory = tempfile::tempdir().unwrap();
    let mut records = nord_with_skin_texture_sets();
    records.push(
        dummy_content::esm::texture_set(
            0x502,
            [
                Some("actors/character/male/malehands_1.dds"),
                Some("actors/character/male/malehands_1_msn.dds"),
                None,
                Some("actors/character/male/malehands_1_sk.dds"),
                None,
                None,
                None,
                Some("actors/character/male/malehands_1_s.dds"),
            ],
        )
        .unwrap(),
    );
    let path = write_plugin(directory.path(), "Skyrim.esm", &records);
    let database = directory.path().join("skyrim_world.db");
    drop(export(directory.path(), &path));

    let male = converter::compose::default_skin_texture_sets(&database, "male").unwrap();
    assert_eq!(
        male.len(),
        1,
        "only the hands armature is DefaultRace's: {male:?}"
    );
    assert_eq!(male[0].kind, converter::compose::BodyPartKind::Hands);
    assert_eq!(male[0].model_path, MALE_HANDS);
    let slots: Vec<Option<&str>> = male[0]
        .textures
        .slots
        .iter()
        .map(Option::as_deref)
        .collect();
    // TX03 is NIF slot 2 and TX07 slot 7.
    assert_eq!(
        slots,
        [
            Some("actors/character/male/malehands_1.dds"),
            Some("actors/character/male/malehands_1_msn.dds"),
            Some("actors/character/male/malehands_1_sk.dds"),
            None,
            None,
            None,
            None,
            Some("actors/character/male/malehands_1_s.dds"),
        ]
    );
    assert!(
        converter::compose::skin_texture_set_for(
            &male,
            converter::compose::BodyPartKind::Hands,
            Path::new("/data/meshes/actors/character/character assets/MaleHands_1.nif"),
        )
        .is_some()
    );
    assert!(
        converter::compose::skin_texture_set_for(
            &male,
            converter::compose::BodyPartKind::Hands,
            Path::new("/data/meshes/actors/character/character assets/malehandsargonian_1.nif"),
        )
        .is_none()
    );
    assert!(
        converter::compose::default_skin_texture_sets(&database, "female")
            .unwrap()
            .is_empty()
    );
}

/// A database whose `actor_model_parts` predates the skin columns gains them when the tables are
/// created again.
#[test]
fn an_older_actor_model_parts_table_gains_the_skin_columns() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE actor_model_parts (
                 npc_id INTEGER NOT NULL, slot TEXT NOT NULL, model_path TEXT NOT NULL,
                 PRIMARY KEY (npc_id, slot)
             );
             INSERT INTO actor_model_parts VALUES (0x100, 'hands', 'hands.nif');",
        )
        .unwrap();
    converter::esm::exporter::create_tables(&connection).unwrap();
    converter::esm::exporter::create_tables(&connection).unwrap();
    let stored: (Option<u32>, Option<u32>) = connection
        .query_row(
            "SELECT armature_race_id, skin_texture_set_id FROM actor_model_parts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, (None, None));
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
    // Retail `QNAM` bytes are whole steps of 1/255: Alvor 198,176,168 and Sigrid 221,221,221.
    for (row, encoded) in [(alvor, [198.0_f32, 176.0, 168.0]), (sigrid, [221.0; 3])] {
        let tone = row.skin_tone.expect("retail NPC_ carries QNAM");
        for (channel, expected) in tone.iter().zip(encoded) {
            assert!(
                (channel * 255.0 - expected).abs() < 1e-3,
                "{:08X}: {tone:?}",
                row.id
            );
        }
        assert_eq!(
            stored_skin_tone(&connection, row.id),
            tone.map(|channel| Some(f64::from(channel)))
        );
    }
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
