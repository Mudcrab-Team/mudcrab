//! Complete exports replace prior world state; subset writes keep unrelated records.
use converter::esm::{
    EsmParser,
    exporter::{create_tables, export_to_db, export_to_db_with_load_order},
    load_order::LoadOrder,
    records::RawRecord,
};
use rusqlite::Connection;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// Independently frame one native subrecord.
fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
}

/// Independently frame one native record with an opaque body.
fn record(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    [
        tag.as_slice(),
        &(payload.len() as u32).to_le_bytes(),
        &flags.to_le_bytes(),
        &id.to_le_bytes(),
        &[0; 8],
        payload,
    ]
    .concat()
}

/// Write a synthetic plugin whose native master indices differ from global slots.
fn plugin(root: &Path, name: &str, masters: &[&str], body: &[u8]) -> PathBuf {
    let header: Vec<u8> = masters
        .iter()
        .flat_map(|master| {
            [
                sub(b"MAST", format!("{master}\0").as_bytes()),
                sub(b"DATA", &[0; 8]),
            ]
            .concat()
        })
        .collect();
    let path = root.join(name);
    fs::write(
        &path,
        [record(b"TES4", 0, 0, &header), body.to_vec()].concat(),
    )
    .unwrap();
    path
}

/// Count an explicitly selected converter table.
fn count(db: &Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// Reusing the public database path removes removed plugins and real deletions.
#[test]
fn complete_public_exports_replace_prior_records_plugins_and_lod() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(
        dir.path(),
        "Base.esm",
        &[],
        &record(b"STAT", 0x1800, 0, &sub(b"EDID", b"BaseSentinel\0")),
    );
    let addon = plugin(
        dir.path(),
        "Addon.esp",
        &["Base.esm"],
        &record(b"STAT", 0x0100_1801, 0, &sub(b"EDID", b"RemovedSentinel\0")),
    );
    let path = dir.path().join("world.db");
    EsmParser::convert_plugins(&[base.clone(), addon], &path).unwrap();
    let db = Connection::open(&path).unwrap();
    assert_eq!(count(&db, "records"), 2);
    db.execute_batch(
        "CREATE TABLE caller_notes(value TEXT);
        INSERT INTO caller_notes VALUES ('preserve caller state');
        INSERT INTO conversion_cache VALUES ('asset proof', x'7291', 7);
        INSERT INTO lod VALUES (6144, 2, x'5927');
        UPDATE schema_info SET version=3;",
    )
    .unwrap();

    EsmParser::convert_plugins(std::slice::from_ref(&base), &path).unwrap();
    for table in ["records", "statics", "formid_map", "plugins"] {
        assert_eq!(count(&db, table), 1, "{table}");
    }
    let owner: String = db
        .query_row("SELECT plugin_name FROM formid_map", [], |row| row.get(0))
        .unwrap();
    assert_eq!(owner, "base.esm");
    assert_eq!(count(&db, "lod"), 0);
    assert_eq!(count(&db, "caller_notes"), 1);
    assert_eq!(count(&db, "conversion_cache"), 1);
    let schema: u32 = db
        .query_row("SELECT version FROM schema_info", [], |row| row.get(0))
        .unwrap();
    assert_eq!(schema, shared::WORLD_DATABASE_SCHEMA_VERSION);

    plugin(
        dir.path(),
        "Base.esm",
        &[],
        &record(b"STAT", 0x1800, 0x20, &[]),
    );
    EsmParser::convert_plugins(&[base], &path).unwrap();
    for table in ["records", "statics", "formid_map", "lod"] {
        assert_eq!(count(&db, table), 0, "{table}");
    }
    assert_eq!(count(&db, "plugins"), 1);
    assert_eq!(count(&db, "caller_notes"), 1);
    assert_eq!(count(&db, "conversion_cache"), 1);
}

/// Construct a direct exporter input without relying on either native reader.
fn raw(
    tag: &[u8; 4],
    id: u32,
    fields: Vec<(&[u8; 4], Vec<u8>)>,
    cell: Option<u32>,
    worldspace: Option<u32>,
) -> RawRecord {
    RawRecord {
        form_id: id,
        record_type: *tag,
        flags: 0,
        subrecords: fields
            .into_iter()
            .map(|(tag, bytes)| (tag.to_vec(), bytes))
            .collect(),
        cell_form_id: cell,
        worldspace_form_id: worldspace,
        load_order: 0,
    }
}

/// A failed complete write restores the old rows, schema stamp and derived data.
#[test]
fn failed_complete_export_rolls_back_snapshot_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let base = plugin(
        dir.path(),
        "Base.esm",
        &[],
        &record(b"STAT", 0x1800, 0, &sub(b"EDID", b"RollbackSentinel\0")),
    );
    let path = dir.path().join("world.db");
    EsmParser::convert_plugins(std::slice::from_ref(&base), &path).unwrap();
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "UPDATE schema_info SET version=3;
        INSERT INTO lod VALUES (6144, 2, x'5927');",
    )
    .unwrap();
    let order = LoadOrder::read(&[base]).unwrap();
    // Global slot 1 has no owner in this order; failure occurs after reset and row insertion.
    let bad = raw(
        b"STAT",
        0x0100_1802,
        vec![(b"EDID", b"Unowned\0".to_vec())],
        None,
        None,
    );
    let records = HashMap::from([(bad.form_id, bad)]);
    assert!(export_to_db_with_load_order(&db, &records, &order).is_err());
    assert_eq!(count(&db, "records"), 1);
    assert_eq!(count(&db, "statics"), 1);
    assert_eq!(count(&db, "formid_map"), 1);
    assert_eq!(count(&db, "plugins"), 1);
    assert_eq!(count(&db, "lod"), 1);
    let sentinel: String = db
        .query_row("SELECT editor_id FROM statics WHERE id=6144", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(sentinel, "RollbackSentinel");
    let schema: u32 = db
        .query_row("SELECT version FROM schema_info", [], |row| row.get(0))
        .unwrap();
    assert_eq!(schema, 3);
}

/// Moving one reference indoors removes its old R-Tree entry and preserves its neighbor.
#[test]
fn subset_reference_updates_remove_obsolete_exterior_entries() {
    let db = Connection::open_in_memory().unwrap();
    create_tables(&db).unwrap();
    let exterior = raw(
        b"CELL",
        0x1800,
        vec![(
            b"XCLC",
            [3i32.to_le_bytes(), (-2i32).to_le_bytes()].concat(),
        )],
        None,
        Some(0x3c),
    );
    let interior = raw(
        b"CELL",
        0x1810,
        vec![(b"EDID", b"InteriorSentinel\0".to_vec())],
        None,
        None,
    );
    let transform: Vec<u8> = [12289.0f32, -8191.0, 31.0, 0.0, 0.0, 0.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    let fields = vec![
        (b"NAME", 0x1802u32.to_le_bytes().to_vec()),
        (b"DATA", transform),
    ];
    let reference = raw(b"REFR", 0x1801, fields.clone(), Some(0x1800), Some(0x3c));
    let neighbor = raw(b"REFR", 0x1803, fields.clone(), Some(0x1800), Some(0x3c));
    let first: HashMap<_, _> = [exterior, interior, reference, neighbor]
        .into_iter()
        .map(|record| (record.form_id, record))
        .collect();
    export_to_db(&db, &first).unwrap();
    assert_eq!(count(&db, "exterior_spatial"), 2);

    let indoor = raw(b"REFR", 0x1801, fields, Some(0x1810), None);
    // Include the owning CELL in the subset so its metadata establishes interior placement.
    let interior = first[&0x1810].clone();
    export_to_db(
        &db,
        &HashMap::from([(indoor.form_id, indoor), (interior.form_id, interior)]),
    )
    .unwrap();
    assert_eq!(count(&db, "records"), 4);
    assert_eq!(count(&db, "exterior_spatial"), 1);
    let surviving: u32 = db
        .query_row("SELECT id FROM exterior_spatial", [], |row| row.get(0))
        .unwrap();
    assert_eq!(surviving, 0x1803);
    let placement: (u32, bool) = db
        .query_row(
            "SELECT cell_id,is_exterior FROM \"references\" WHERE id=6145",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(placement, (0x1810, false));
}
