//! Initial enable state from a generated plugin, through converter export and
//! the database worker used by cell streaming.

use converter::esm::{
    binary::parse_plugin_file,
    exporter::{create_tables, export_to_db},
    records::RawRecord,
};
use dummy_content::{
    esm::{self, Cell, Light, Plugin},
    layout,
};
use engine::world::database::{CellKey, CellPayload, DatabaseRequest, ReferenceRow, WorldDatabase};
use rusqlite::Connection;
use std::{
    collections::HashMap,
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

const CELL_SIZE: f32 = 4096.0;
const INITIALLY_DISABLED: u32 = 0x0000_0800;

fn generated_plugin(path: &Path) -> (HashMap<u32, RawRecord>, [u32; 2], u32) {
    let cells = [
        Cell {
            grid_x: 0,
            grid_y: 0,
        },
        Cell {
            grid_x: 1,
            grid_y: 0,
        },
    ];
    let plugin = Plugin {
        author: layout::GENERATED_AUTHOR,
        worldspace: layout::GENERATED_WORLDSPACE,
        cells: &cells,
        model_path: layout::GENERATED_MODEL_PATH,
        diffuse: layout::GENERATED_DIFFUSE_PATH,
        normal_texture: layout::GENERATED_NORMAL_PATH,
    };
    let light = Light {
        editor_id: "GeneratedEnableStateLight",
        model_path: None,
        cell: cells[0],
        time: -1,
        radius: 512,
        color: [216, 128, 39],
        flags: 0,
        falloff: 1.0,
        fade: 1.0,
        position: [CELL_SIZE * 0.5, CELL_SIZE * 0.5, 0.0],
        rotation: [0.0; 3],
        radius_override: 256.0,
        reference_flags: 0,
        enable_parent: None,
    };
    fs::write(path, esm::plugin_with_lights(&plugin, &light).unwrap()).unwrap();

    let parsed = parse_plugin_file(path).unwrap();
    let mut cell_ids: Vec<u32> = parsed
        .iter()
        .filter(|record| &record.record_type == b"CELL")
        .map(|record| record.form_id)
        .collect();
    cell_ids.sort_unstable();
    assert_eq!(cell_ids.len(), 2, "the generated plugin has two cells");
    let worldspace_id = parsed
        .iter()
        .find(|record| &record.record_type == b"CELL")
        .and_then(|record| record.worldspace_form_id)
        .expect("generated exterior cells belong to a worldspace");

    let records = parsed
        .into_iter()
        .map(|record| (record.form_id, record))
        .collect();
    (records, [cell_ids[0], cell_ids[1]], worldspace_id)
}

fn subrecord_u32(record: &RawRecord, tag: &[u8; 4]) -> Option<u32> {
    record
        .subrecords
        .iter()
        .find(|(candidate, _)| candidate.as_slice() == tag)
        .and_then(|(_, data)| <[u8; 4]>::try_from(data.as_slice()).ok())
        .map(u32::from_le_bytes)
}

fn with_enable_state(
    template: &RawRecord,
    form_id: u32,
    cell_id: u32,
    grid_x: i32,
    header_flags: u32,
    parent: Option<(u32, u32)>,
) -> RawRecord {
    let mut record = template.clone();
    record.form_id = form_id;
    record.cell_form_id = Some(cell_id);
    record.flags = header_flags;

    let mut found_position = false;
    record
        .subrecords
        .retain(|(tag, _)| tag.as_slice() != b"XESP");
    for (tag, data) in &mut record.subrecords {
        if tag.as_slice() == b"DATA" {
            assert!(data.len() >= 12, "a reference DATA contains its position");
            data[0..4]
                .copy_from_slice(&(grid_x as f32 * CELL_SIZE + CELL_SIZE * 0.5).to_le_bytes());
            data[4..8].copy_from_slice(&(CELL_SIZE * 0.5).to_le_bytes());
            data[8..12].copy_from_slice(&0.0f32.to_le_bytes());
            found_position = true;
        }
    }
    assert!(
        found_position,
        "the generated reference has a DATA transform"
    );

    if let Some((parent_id, flags)) = parent {
        let mut xesp = Vec::with_capacity(8);
        xesp.extend_from_slice(&parent_id.to_le_bytes());
        xesp.extend_from_slice(&flags.to_le_bytes());
        record.subrecords.push((b"XESP".to_vec(), xesp));
    }
    record
}

fn load_cell(database: &WorldDatabase, generation: u64, key: CellKey) -> CellPayload {
    database
        .request(DatabaseRequest::Load {
            generation,
            key,
            queued_at: Instant::now(),
        })
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(response) = database.try_response() {
            assert_eq!(response.generation, generation);
            assert_eq!(response.key, key);
            return response.result.unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "database worker returned no cell response"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn reference(payloads: &[CellPayload], form_id: u32) -> &ReferenceRow {
    payloads
        .iter()
        .flat_map(|payload| &payload.references)
        .find(|reference| reference.form_id == form_id)
        .unwrap_or_else(|| panic!("reference {form_id:08X} was not retained in a cell payload"))
}

#[test]
fn database_worker_resolves_initial_enable_state_across_exterior_cells() {
    const A: u32 = 0x0000_1000;
    const B: u32 = 0x0000_1001;
    const C: u32 = 0x0000_1002;
    const D: u32 = 0x0000_1003;
    const E: u32 = 0x0000_1004;
    const F: u32 = 0x0000_1005;

    let directory = tempfile::tempdir().unwrap();
    let plugin_path = directory.path().join("Skyrim.esm");
    let (mut records, cell_ids, worldspace_id) = generated_plugin(&plugin_path);
    let template = records
        .values()
        .find(|record| {
            &record.record_type == b"REFR" && subrecord_u32(record, b"NAME") == Some(0x0000_0009)
        })
        .expect("generated light reference template")
        .clone();

    let synthetic_references = [
        (A, cell_ids[0], 0, INITIALLY_DISABLED, None),
        (B, cell_ids[1], 1, 0, Some((A, 0))),
        (C, cell_ids[1], 1, 0, Some((A, 1))),
        (D, cell_ids[0], 0, 0, None),
        // Only bit zero in the XESP flags inverts a parent's state. The upper
        // three bytes are reserved and remain nonzero in this fixture.
        (E, cell_ids[1], 1, 0, Some((D, 0xA1B2_C300))),
        (F, cell_ids[1], 1, INITIALLY_DISABLED, Some((D, 0))),
    ];
    for (form_id, cell_id, grid_x, flags, parent) in synthetic_references {
        let record = with_enable_state(&template, form_id, cell_id, grid_x, flags, parent);
        records.insert(form_id, record);
    }

    let database_path = directory.path().join("world.db");
    let connection = Connection::open(&database_path).unwrap();
    create_tables(&connection).unwrap();
    export_to_db(&connection, &records).unwrap();
    drop(connection);

    let database = WorldDatabase::open(&database_path).unwrap();
    let cell_zero = load_cell(
        &database,
        1,
        CellKey::Exterior {
            worldspace_id,
            grid_x: 0,
            grid_y: 0,
        },
    );
    let cell_one = load_cell(
        &database,
        2,
        CellKey::Exterior {
            worldspace_id,
            grid_x: 1,
            grid_y: 0,
        },
    );
    let payloads = [cell_zero, cell_one];

    for form_id in [A, B, C, D, E, F] {
        reference(&payloads, form_id);
    }
    assert!(
        !reference(&payloads, A).initially_enabled,
        "A starts disabled"
    );
    assert!(
        !reference(&payloads, B).initially_enabled,
        "B follows disabled A"
    );
    assert!(
        reference(&payloads, C).initially_enabled,
        "C inverts disabled A"
    );
    assert!(
        reference(&payloads, D).initially_enabled,
        "D has no disabled flag or parent"
    );
    assert!(
        reference(&payloads, E).initially_enabled,
        "E follows D despite reserved XESP bits"
    );
    assert!(
        reference(&payloads, F).initially_enabled,
        "an enabled parent enables initially disabled F"
    );
    assert!(reference(&payloads, A).model_path.is_none());
    assert!(reference(&payloads, D).model_path.is_none());
}
