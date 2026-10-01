//! The generated plugin carries what the engine's record-backed movement reads
//! from the world database: the player `NPC_` (form ID 7), its `RACE` with no
//! movement links, and the `NPC_Default_MT` movement type. The queries here are
//! the ones the engine runs at startup.

use converter::esm::{
    binary::parse_plugin_file,
    exporter::{create_tables, export_to_db},
    records::RawRecord,
};
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
    let records = parse_plugin_file(&path).unwrap();
    let connection = Connection::open_in_memory().unwrap();
    create_tables(&connection).unwrap();
    let master: HashMap<u32, RawRecord> = records
        .into_iter()
        .map(|record| (record.form_id, record))
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
}
