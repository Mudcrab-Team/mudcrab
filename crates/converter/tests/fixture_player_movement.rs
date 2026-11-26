//! The generated plugin carries what the engine's record-backed movement reads
//! from the world database: the player `NPC_` (form ID 7), its `RACE` with no
//! movement links, and the `NPC_Default_MT` movement type. The queries here are
//! the ones the engine runs at startup.

use converter::esm::exporter::{create_tables, export_to_db};
use dummy_content::{
    esm::{self, Plugin},
    layout,
};
use rusqlite::Connection;
use std::{collections::HashMap, fs};

fn exported_fixture() -> Connection {
    let spec = Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &[esm::PRESET_EXTERIOR_CELL],
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Skyrim.esm");
    fs::write(&path, esm::plugin(&spec).unwrap()).unwrap();
    let paths = [path];
    let order = converter::esm::load_order::LoadOrder::read(&paths).unwrap();
    let decoded = converter::records::read_plugins(&paths, &order).unwrap();
    for diagnostic in decoded.diagnostics.values() {
        assert_eq!(diagnostic.skipped_records, 0);
        assert_eq!(diagnostic.skipped_fields, 0);
        assert_eq!(diagnostic.unexpected_subrecords, 0, "{diagnostic:?}");
    }
    let player = &decoded.records[&7];
    for tag in [*b"ACBS", *b"RNAM", *b"NAM6"] {
        assert!(
            player.fields.iter().any(|field| field.signature == tag),
            "missing {tag:?}; decoded fields: {:?}",
            player
                .fields
                .iter()
                .map(|field| field.signature)
                .collect::<Vec<_>>()
        );
    }
    let connection = Connection::open_in_memory().unwrap();
    create_tables(&connection).unwrap();
    let master: HashMap<_, _> = decoded
        .records
        .iter()
        .map(|(&id, record)| (id, record.to_raw_record()))
        .collect();
    export_to_db(&connection, &master).unwrap();
    connection
}

#[test]
fn fixture_has_the_player_race_and_default_movement_type_the_engine_needs() {
    let connection = exported_fixture();

    let race_id: u32 = connection
        .query_row("SELECT race_id FROM npcs WHERE id=7", [], |row| row.get(0))
        .expect("Player NPC_ 00000007 with an RNAM");
    let race_type: String = connection
        .query_row(
            "SELECT record_type FROM records WHERE form_id=?1",
            [race_id],
            |row| row.get(0),
        )
        .expect("the player's race is a record");
    assert_eq!(race_type, "RACE");

    let (walk, run): (Option<u32>, Option<u32>) = connection
        .query_row(
            "SELECT walk_movt_id, run_movt_id FROM race_movement_links WHERE id=?1",
            [race_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the race has a movement-links row");
    assert_eq!((walk, run), (None, None), "race links are unset");

    let editor_id: String = connection
        .query_row(
            "SELECT editor_id FROM movement_types WHERE id=?1",
            [0x0003_580Du32],
            |row| row.get(0),
        )
        .expect("MOVT 0003580D");
    assert_eq!(editor_id, "NPC_Default_MT");

    // The unset race links resolve through the default objects, like the player race in Skyrim.esm.
    for key in ["DMWL", "DMRN"] {
        let target: u32 = connection
            .query_row(
                "SELECT form_id FROM default_objects WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("default object {key}: {error}"));
        assert_eq!(target, 0x0003_580D, "{key}");
    }
}

#[test]
fn fixture_has_complete_canonical_jump_inputs() {
    use shared::records::ArchivedRecordData;
    let connection = exported_fixture();
    let read = |id: u32| {
        let bytes: Vec<u8> = connection
            .query_row("SELECT data FROM records WHERE form_id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        let mut aligned = rkyv::util::AlignedVec::<16>::new();
        aligned.extend_from_slice(&bytes);
        rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&aligned).unwrap()
    };
    let player = read(7);
    let field = |record: &ArchivedRecordData, tag: [u8; 4]| {
        let fields: Vec<_> = record
            .subrecords
            .iter()
            .filter(|field| field.tag == tag)
            .collect();
        assert_eq!(fields.len(), 1);
        fields[0].data.clone()
    };
    let acbs = field(&player, *b"ACBS");
    assert_eq!(acbs.len(), 24);
    assert_eq!(u32::from_le_bytes(acbs[..4].try_into().unwrap()) & 1, 0);
    assert_eq!(
        f32::from_le_bytes(field(&player, *b"NAM6").try_into().unwrap()),
        1.1
    );
    assert_eq!(u16::from_le_bytes(acbs[14..16].try_into().unwrap()), 100);
    let race_id: u32 = connection
        .query_row("SELECT race_id FROM npcs WHERE id=7", [], |row| row.get(0))
        .unwrap();
    let data = field(&read(race_id), *b"DATA");
    assert!(data.len() >= 24);
    assert_eq!(f32::from_le_bytes(data[16..20].try_into().unwrap()), 1.03);
    assert_eq!(f32::from_le_bytes(data[20..24].try_into().unwrap()), 1.0);
    let setting: f32 = connection
        .query_row(
            "SELECT value FROM movement_game_settings WHERE editor_id='fJumpHeightMin'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(setting, 76.0);
}
