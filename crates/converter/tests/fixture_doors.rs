//! The generated interior cell and its reciprocal load door pair, through the
//! real ESM parser and the database export.

use converter::esm::{
    binary::{parse_group, parse_plugin_file, parse_record_header},
    records::RawRecord,
};
use dummy_content::{
    esm::{self, Plugin},
    layout,
};
use std::{fs, path::Path};

/// The bytes `dummy-content gen --with-interior` writes: one exterior cell,
/// its auto-load door into one interior cell, and the return door. The spec is
/// assembled from the same `layout` constants the command uses, so this file
/// cannot drift from the plugin the command publishes.
fn preset_plugin() -> Vec<u8> {
    let cells = [esm::PRESET_EXTERIOR_CELL];
    esm::plugin_with_interior(
        &Plugin {
            author: layout::GENERATED_AUTHOR,
            worldspace: layout::GENERATED_WORLDSPACE,
            cells: &cells,
            model_path: layout::GENERATED_MODEL_PATH,
            diffuse: layout::GENERATED_DIFFUSE_PATH,
            normal_texture: layout::GENERATED_NORMAL_PATH,
        },
        &esm::PRESET_INTERIOR,
    )
    .unwrap()
}

fn write_plugin(directory: &Path) -> std::path::PathBuf {
    let path = directory.join("Skyrim.esm");
    fs::write(&path, preset_plugin()).unwrap();
    path
}

fn subrecord<'a>(record: &'a RawRecord, tag: &[u8; 4]) -> Option<&'a [u8]> {
    record
        .subrecords
        .iter()
        .find(|(candidate, _)| candidate.as_slice() == tag)
        .map(|(_, data)| data.as_slice())
}

fn subrecord_or_panic<'a>(record: &'a RawRecord, tag: &[u8; 4]) -> &'a [u8] {
    subrecord(record, tag).unwrap_or_else(|| {
        panic!(
            "{} record {:08X} has no {}",
            String::from_utf8_lossy(&record.record_type),
            record.form_id,
            String::from_utf8_lossy(tag)
        )
    })
}

/// An `EDID` as the parser hands it back: the bytes as they were written, NUL
/// terminator included.
fn editor_id(record: &RawRecord) -> String {
    String::from_utf8_lossy(subrecord(record, b"EDID").unwrap_or_default()).into_owned()
}

fn form_id(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

fn floats(bytes: &[u8]) -> [f32; 3] {
    let mut values = [0.0f32; 3];
    for (index, value) in values.iter_mut().enumerate() {
        *value = f32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap());
    }
    values
}

/// What the real parser reads out of the generated plugin: an interior `CELL`
/// with no grid and no worldspace, two `DOOR` base records whose `FNAM` carries
/// the auto-load bit, and a reciprocal pair of `XTEL`s whose arrival frames are
/// each door's own.
#[test]
fn generated_interior_plugin_holds_an_interior_cell_and_a_door_pair() {
    let directory = tempfile::tempdir().unwrap();
    let records = parse_plugin_file(&write_plugin(directory.path())).unwrap();

    let interior = records
        .iter()
        .find(|record| &record.record_type == b"CELL" && record.worldspace_form_id.is_none())
        .expect("the interior CELL record");
    assert!(
        interior.cell_form_id.is_none(),
        "an interior cell is no group's child"
    );
    assert_eq!(editor_id(interior), "GeneratedInterior\0");
    assert_eq!(
        subrecord(interior, b"FULL"),
        Some(b"Generated Interior\0".as_slice())
    );
    // `DATA`'s 0x01 bit marks the cell as an interior, and an interior has no
    // `XCLC` grid square.
    assert_eq!(subrecord(interior, b"DATA"), Some([0x01].as_slice()));
    assert_eq!(subrecord(interior, b"XCLC"), None);

    let exterior = records
        .iter()
        .find(|record| &record.record_type == b"CELL" && record.worldspace_form_id.is_some())
        .expect("the exterior CELL record");
    assert!(subrecord(exterior, b"XCLC").is_some());

    let doors: Vec<&RawRecord> = records
        .iter()
        .filter(|record| &record.record_type == b"DOOR")
        .collect();
    assert_eq!(doors.len(), 2, "one DOOR base record per door");
    let auto_load = doors
        .iter()
        .find(|record| editor_id(record) == "AutoLoadDoor01\0")
        .expect("the auto-load door's base record");
    let ordinary = doors
        .iter()
        .find(|record| editor_id(record) == "GeneratedDoor01\0")
        .expect("the ordinary door's base record");
    assert_eq!(
        subrecord(auto_load, b"FNAM"),
        Some([esm::AUTO_LOAD_FLAG].as_slice()),
        "the auto-load bit survives the write"
    );
    assert_eq!(subrecord(ordinary, b"FNAM"), Some([0x00].as_slice()));
    assert_eq!(
        subrecord(auto_load, b"MODL"),
        Some(b"meshes/generated.nif\0".as_slice())
    );

    let references: Vec<&RawRecord> = records
        .iter()
        .filter(|record| &record.record_type == b"REFR")
        .collect();
    assert_eq!(
        references.len(),
        3,
        "the static placement and the two doors"
    );
    let inside_ref = references
        .iter()
        .find(|record| record.cell_form_id == Some(interior.form_id))
        .expect("the interior door's reference");
    let outside_ref = references
        .iter()
        .find(|record| {
            record.cell_form_id == Some(exterior.form_id) && subrecord(record, b"XTEL").is_some()
        })
        .expect("the exterior door's reference");
    assert_eq!(
        form_id(subrecord_or_panic(inside_ref, b"NAME")),
        ordinary.form_id
    );
    assert_eq!(
        form_id(subrecord_or_panic(outside_ref, b"NAME")),
        auto_load.form_id
    );

    // The load-order remap rewrites `XTEL`'s destination FormID like the
    // reference's own (`form_id_layout` in `crates/converter/src/esm/mod.rs`),
    // so the doors keep pointing at each other in any load-order slot. The
    // fixture is the only plugin here, at index 0, so the values are unchanged.
    // The exporter also projects these records into `door_links`.
    let inside_xtel = subrecord_or_panic(inside_ref, b"XTEL");
    let outside_xtel = subrecord_or_panic(outside_ref, b"XTEL");
    assert_eq!(inside_xtel.len(), 32, "Skyrim SE's XTEL");
    assert_eq!(outside_xtel.len(), 32);
    assert_eq!(
        form_id(inside_xtel),
        outside_ref.form_id,
        "the doors point at each other"
    );
    assert_eq!(form_id(outside_xtel), inside_ref.form_id);
    // The arrival frame is the door's own, not the position of the door it
    // leads to, and no `XTEL` flag bits are set.
    assert_eq!(floats(&inside_xtel[4..16]), [2048.0, 512.0, 0.0]);
    assert_eq!(floats(&inside_xtel[16..28]), [0.0, 0.0, 0.0]);
    assert_eq!(floats(&outside_xtel[4..16]), [128.0, 256.0, 0.0]);
    assert_eq!(&inside_xtel[28..], &[0, 0, 0, 0]);
    assert_eq!(
        floats(subrecord_or_panic(inside_ref, b"DATA")),
        [128.0, 512.0, 0.0],
        "the arrival point is not the door's own position"
    );
}

/// `parse_plugin_file` without file IO, so a prefix of the fixture can be swept.
fn parse_prefix(bytes: &[u8]) {
    let Ok((rest, header)) = parse_record_header(bytes) else {
        return;
    };
    if &header.type_tag != b"TES4" || header.data_size as usize > rest.len() {
        return;
    }
    let mut records = Vec::new();
    let _ = parse_group(&rest[header.data_size as usize..], None, None, &mut records);
}

#[test]
fn generated_interior_plugin_never_panics_under_truncation_or_mutation() {
    let bytes = preset_plugin();
    for length in 0..bytes.len() {
        assert!(
            std::panic::catch_unwind(|| parse_prefix(&bytes[..length])).is_ok(),
            "ESM parser panicked at length {length}"
        );
    }
    let mut rng = dummy_content::rng::Rng::new(39);
    for _ in 0..256 {
        let mut mutated = bytes.clone();
        let index = rng.next_u64() as usize % mutated.len();
        mutated[index] ^= 0xff;
        assert!(
            std::panic::catch_unwind(|| parse_prefix(&mutated)).is_ok(),
            "ESM parser panicked on mutation at {index}"
        );
    }
}

#[test]
fn real_plugin_export_projects_reciprocal_door_arrivals() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = write_plugin(directory.path());
    let database = directory.path().join("world.db");
    converter::esm::EsmParser::convert_plugins(&[plugin], &database).unwrap();
    let conn = rusqlite::Connection::open(database).unwrap();
    let links: Vec<(u32, u32, f32, f32, f32, Option<u32>)> = conn
        .prepare("SELECT ref_id,destination_ref_id,pos_x,pos_y,pos_z,destination_worldspace_id FROM door_links ORDER BY ref_id")
        .unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)))
        .unwrap().map(Result::unwrap).collect();
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].0, links[1].1);
    assert_eq!(links[0].1, links[1].0);
    let interior = links.iter().find(|row| row.5.is_none()).unwrap();
    let exterior = links.iter().find(|row| row.5.is_some()).unwrap();
    assert_eq!([interior.2, interior.3, interior.4], [128.0, 256.0, 0.0]);
    assert_eq!([exterior.2, exterior.3, exterior.4], [2048.0, 512.0, 0.0]);
    assert_eq!(
        conn.query_row(
            "SELECT destination_name FROM door_links WHERE ref_id=?1",
            [interior.0],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "Generated Interior"
    );
    assert_eq!(converter::esm::doors::rebuild_door_links(&conn).unwrap(), 2);

    // The winning override removes XTEL: an older link must not survive.
    let mut records =
        converter::esm::binary::parse_plugin_file(&write_plugin(directory.path())).unwrap();
    let source = records
        .iter_mut()
        .find(|r| r.form_id == interior.0)
        .unwrap();
    source.subrecords.retain(|(tag, _)| tag != b"XTEL");
    let blob = converter::esm::extractors::serialize_subrecords(&source.subrecords);
    conn.execute(
        "UPDATE records SET data=?1 WHERE form_id=?2",
        rusqlite::params![blob, source.form_id],
    )
    .unwrap();
    assert_eq!(converter::esm::doors::rebuild_door_links(&conn).unwrap(), 1);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM door_links WHERE ref_id=?1",
            [interior.0],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );

    // A corrupt canonical archive must not erase the previous valid projection.
    conn.execute(
        "UPDATE records SET data=x'00' WHERE form_id=?1",
        [exterior.0],
    )
    .unwrap();
    assert!(converter::esm::doors::rebuild_door_links(&conn).is_err());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM door_links", [], |row| row
            .get::<_, u32>(0))
            .unwrap(),
        1
    );
}
