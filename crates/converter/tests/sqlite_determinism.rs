//! SQL determinism checks include every table, including SQLite shadow tables.
use converter::esm::{exporter, records::RawRecord};
use rusqlite::Connection;
use std::collections::HashMap;

/// Quote an SQLite identifier without assuming application-owned names.
fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Capture all schema objects and sorted rows of every table, without implicit rowids.
fn logical_dump(conn: &Connection) -> Vec<String> {
    let mut dump = Vec::new();
    let mut schema = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name")
        .unwrap();
    let objects = schema
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for (kind, name, table, sql) in objects {
        dump.push(format!("{kind:?} {name:?} {table:?} {sql:?}"));
        if kind != "table" {
            continue;
        }
        let mut query = conn
            .prepare(&format!("SELECT * FROM {}", identifier(&name)))
            .unwrap();
        let columns = query.column_count();
        let mut rows = query.query([]).unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let fields: Vec<_> = (0..columns)
                .map(|index| format!("{:?}", row.get_ref(index).unwrap()))
                .collect();
            values.push(format!("{fields:?}"));
        }
        values.sort();
        dump.extend(values);
    }
    dump
}

/// Exercise fresh randomized maps, opposite insertion orders, high unsigned owner IDs,
/// and supplier priorities that deliberately disagree with identity order.
#[test]
fn cell_exports_have_identical_bytes_and_all_table_rows() {
    let temp = tempfile::tempdir().unwrap();
    let order = converter::esm::load_order::LoadOrder::from_metadata(
        vec![
            "Z.esm".into(),
            "First.esl".into(),
            "A.esm".into(),
            "Last.esl".into(),
        ],
        vec![converter::esm::binary::PluginMetadata::default(); 4],
    )
    .unwrap();
    let mut baseline = None;
    for pass in 0..12 {
        let path = temp.path().join(format!("{pass}.db"));
        let conn = Connection::open(&path).unwrap();
        exporter::create_tables(&conn).unwrap();
        let mut records = HashMap::new();
        for index in 0..256u32 {
            let index = if pass % 2 == 0 { index } else { 255 - index };
            let id = if index % 2 == 0 {
                0xfe001000 + index
            } else {
                index
            };
            records.insert(
                id,
                RawRecord {
                    form_id: id,
                    record_type: *b"CELL",
                    flags: 0,
                    subrecords: vec![(b"FULL".to_vec(), format!("Cell {index}\0").into_bytes())],
                    cell_form_id: None,
                    worldspace_form_id: None,
                    load_order: 255 - index,
                },
            );
        }
        exporter::export_to_db_with_load_order(&conn, &records, &order).unwrap();
        let logical = logical_dump(&conn);
        drop(conn);
        let current = (std::fs::read(path).unwrap(), logical);
        if let Some(expected) = &baseline {
            assert_eq!(&current, expected, "randomized export {pass}");
        } else {
            baseline = Some(current);
        }
    }
}

/// Full typed exports cover native history, terrain offsets/layers and every bundle payload.
#[test]
fn typed_history_terrain_bundles_are_reproducible() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("Data");
    dummy_content::inhouse_history::generate(&data).unwrap();
    let mut baseline = None;
    for pass in 0..4 {
        let output = temp.path().join(format!("bundle-{pass}"));
        converter::esm::inhouse::export_record_bundle(&data, &data.join("plugins.txt"), &output)
            .unwrap();
        let db = output.join("skyrim_world.db");
        let conn = Connection::open(&db).unwrap();
        for table in [
            "record_overrides",
            "inhouse_terrain_source_order",
            "inhouse_terrain_layers",
        ] {
            let count: u32 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert!(count > 0, "fixture must exercise {table}");
        }
        let logical = logical_dump(&conn);
        drop(conn);
        let mut files = std::collections::BTreeMap::new();
        collect_payloads(&output, &output, &mut files);
        let current = (files, logical);
        if let Some(expected) = &baseline {
            assert_eq!(&current, expected, "full typed bundle {pass}");
        } else {
            baseline = Some(current);
        }
    }
}

/// Include SQLite bytes and all serialized payloads; measured durations are diagnostic only.
fn collect_payloads(
    root: &std::path::Path,
    directory: &std::path::Path,
    files: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_payloads(root, &path, files);
        } else if path.file_name().unwrap() != "inhouse-database-profile.json" {
            files.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                std::fs::read(path).unwrap(),
            );
        }
    }
}
