//! Execute the real publication and query builders; never maintain a second DDL/SQL copy.
use super::*;
use converter::{config::RecordReader, esm::inhouse};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn schema(connection: &Connection) -> Value {
    let objects = connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")
        .unwrap()
        .query_map([], |row| {
            Ok(json!({"type": row.get::<_, String>(0)?, "name": row.get::<_, String>(1)?,
                "table": row.get::<_, String>(2)?, "sql": row.get::<_, Option<String>>(3)?}))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let shadow_tables = connection
        .prepare("SELECT name FROM pragma_table_list WHERE type='shadow' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let mut table_info = serde_json::Map::new();
    for object in &objects {
        if object["type"] != "table" {
            continue;
        }
        let name = object["name"].as_str().unwrap();
        let columns = connection
            .prepare("SELECT cid,name,type,\"notnull\",dflt_value,pk FROM pragma_table_info(?1)")
            .unwrap()
            .query_map([name], |row| {
                Ok(json!([
                    row.get::<_, u32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, u32>(5)?
                ]))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        table_info.insert(name.to_owned(), json!(columns));
    }
    json!({"objects": objects, "shadow_tables": shadow_tables,
        "table_info": table_info,
        "schema_stamp": connection.query_row("SELECT version FROM schema_info", [], |row| row.get::<_, u32>(0)).unwrap(),
        "population": "not_observed"})
}

fn query_result(connection: &Connection) -> Value {
    match ReferenceQuery::for_connection(connection) {
        Ok(query) => {
            let statements = json!({"exterior": query.exterior_sql(), "interior": query.interior_sql(),
                "parent": query.parent_sql, "exterior_cell": EXTERIOR_CELL_ID_SQL});
            for sql in statements.as_object().unwrap().values() {
                connection.prepare(sql.as_str().unwrap()).unwrap();
            }
            json!({"status": "prepared", "statements": statements})
        }
        Err(error) => json!({"status": "rejected", "error": error.to_string()}),
    }
}

fn variant(changes: Vec<String>, id: String) -> Value {
    let connection = Connection::open_in_memory().unwrap();
    converter::esm::exporter::create_tables(&connection).unwrap();
    for sql in &changes {
        connection.execute_batch(sql).unwrap();
    }
    json!({"id": id, "schema": "create_tables", "changes": changes, "result": query_result(&connection)})
}

/// The only filesystem writes are disposable synthetic inputs and an optional requested report.
#[test]
fn world_data_inventory_extraction() {
    let temporary = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let fingerprint = Command::new(&python)
        .arg(root.join("tools/world_data_inventory/inventory.py"))
        .arg("--source-identity")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        fingerprint.status.success(),
        "{}",
        String::from_utf8_lossy(&fingerprint.stderr)
    );
    let base = Connection::open_in_memory().unwrap();
    converter::esm::exporter::create_tables(&base).unwrap();

    // A real schema-driven publication includes tables that create_tables does not create.
    // No mesh, texture, script conversion or engine launch occurs.
    let data = temporary.path().join("data");
    fs::create_dir(&data).unwrap();
    let mut plugin = b"TES4".to_vec();
    plugin.extend_from_slice(&18u32.to_le_bytes());
    plugin.extend_from_slice(&[0; 16]);
    plugin.extend_from_slice(b"HEDR");
    plugin.extend_from_slice(&12u16.to_le_bytes());
    plugin.extend_from_slice(&1.7f32.to_le_bytes());
    plugin.extend_from_slice(&0u32.to_le_bytes());
    plugin.extend_from_slice(&0x800u32.to_le_bytes());
    // Include one ordinary STAT so the publication has a decoded winning record.
    let payload = b"EDID\x0a\x00Inventory\0";
    plugin.extend_from_slice(b"STAT");
    plugin.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    plugin.extend_from_slice(&0u32.to_le_bytes());
    plugin.extend_from_slice(&0x800u32.to_le_bytes());
    plugin.extend_from_slice(&[0; 8]);
    plugin.extend_from_slice(payload);
    fs::write(data.join("Skyrim.esm"), plugin).unwrap();
    let plugins = temporary.path().join("plugins.txt");
    fs::write(&plugins, "*Skyrim.esm\n").unwrap();
    let bundle = temporary.path().join("bundle");
    inhouse::export_record_bundle(&data, &plugins, &bundle).unwrap();
    let published = Connection::open(bundle.join("skyrim_world.db")).unwrap();
    let reader: Value =
        serde_json::from_slice(&fs::read(bundle.join("record-reader.json")).unwrap()).unwrap();
    let legacy_path = temporary.path().join("legacy.db");
    converter::esm::EsmParser::convert_plugins(&[data.join("Skyrim.esm")], &legacy_path).unwrap();
    let legacy = Connection::open(legacy_path).unwrap();

    let mut variants = Vec::new();
    // All 64 independent selectors are cheap empty-schema preparations, not native coverage.
    for mask in 0..64 {
        let mut changes = Vec::new();
        for (bit, sql) in [
            (0, "DROP TABLE records"),
            (1, "DROP TABLE lights"),
            (2, "ALTER TABLE \"references\" DROP COLUMN radius_override"),
            (3, "DROP TABLE door_links"),
            (
                4,
                "DROP TABLE emittance; ALTER TABLE \"references\" DROP COLUMN emittance_id",
            ),
            (
                5,
                "ALTER TABLE \"references\" DROP COLUMN header_flags; ALTER TABLE \"references\" DROP COLUMN enable_parent_id; ALTER TABLE \"references\" DROP COLUMN enable_parent_flags",
            ),
        ] {
            if mask & (1 << bit) == 0 {
                changes.push(sql.to_owned());
            }
        }
        let entry = variant(changes, format!("selectors-{mask:02}"));
        assert_eq!(entry["result"]["status"], "prepared");
        variants.push(entry);
    }
    for present in 1..7 {
        let mut changes = Vec::new();
        for (bit, column) in ["header_flags", "enable_parent_id", "enable_parent_flags"]
            .iter()
            .enumerate()
        {
            if present & (1 << bit) == 0 {
                changes.push(format!("ALTER TABLE \"references\" DROP COLUMN {column}"));
            }
        }
        let entry = variant(changes, format!("partial-enable-{present}"));
        assert_eq!(entry["result"]["status"], "rejected");
        variants.push(entry);
    }
    for (id, sql) in [
        (
            "emittance-table-only",
            "ALTER TABLE \"references\" DROP COLUMN emittance_id",
        ),
        ("emittance-column-only", "DROP TABLE emittance"),
        (
            "incomplete-door",
            "ALTER TABLE door_links DROP COLUMN destination_ref_id",
        ),
    ] {
        variants.push(variant(vec![sql.to_owned()], id.to_owned()));
    }
    for (name, connection) in [("legacy", &legacy), ("inhouse", &published)] {
        variants.push(json!({"id": format!("{name}-current"), "schema": name,
            "changes": [], "result": query_result(connection)}));
    }
    let source_head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(source_head.status.success());
    let extraction = json!({"format_version": 1,
        "reviewed_source_guard_sha256": String::from_utf8(fingerprint.stdout).unwrap().trim(),
        "source_commit": String::from_utf8(source_head.stdout).unwrap().trim(),
        "world_schema": shared::WORLD_DATABASE_SCHEMA_VERSION,
        "producer_schema": converter::cache::CONVERTER_SCHEMA_VERSION,
        "cell_cache_version": shared::CELL_CACHE_VERSION,
        "rusqlite_version": rusqlite::version(),
        "reader_identities": {"legacy": inhouse::reader_identity(RecordReader::Legacy), "inhouse": reader},
        "schemas": {"create_tables": schema(&base), "legacy": schema(&legacy), "inhouse": schema(&published)},
        "selector_denominator": 64, "variants": variants});
    let input = temporary.path().join("extraction.json");
    fs::write(&input, serde_json::to_vec_pretty(&extraction).unwrap()).unwrap();
    let output = std::env::var_os("WORLD_DATA_INVENTORY_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().join("report"));
    let result = Command::new(python)
        .arg(root.join("tools/world_data_inventory/inventory.py"))
        .args(["--extraction"])
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    if std::env::var_os("WORLD_DATA_INVENTORY_OUTPUT").is_some() {
        fs::copy(input, output.join("extraction.json")).unwrap();
    }
}
