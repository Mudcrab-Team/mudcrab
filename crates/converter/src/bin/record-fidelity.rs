//! Read-only declared-schema and winning-payload accounting. No completeness certification.
use color_eyre::{
    Result,
    eyre::{bail, ensure},
};
use converter::{
    cache::{hash_bytes, hash_file},
    records::{
        SCHEMA_BYTES,
        addons::{
            MAX_SCHEMA_ADDON_BYTES, MAX_SCHEMA_ADDON_TOTAL_BYTES, MAX_SCHEMA_ADDONS,
            RuntimeSchemas, SchemaAddonIdentity,
        },
        fidelity,
        schema_format::Schema,
    },
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::{io::Write, path::Path};

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let schema_bytes = if args.len() >= 3 && args[args.len() - 2] == "--schema" {
        let bytes = std::fs::read(args.last().unwrap())?;
        args.truncate(args.len() - 2);
        bytes
    } else {
        SCHEMA_BYTES.to_vec()
    };
    let schema = Schema::parse(&schema_bytes).map_err(color_eyre::eyre::Error::msg)?;
    let output = match args.as_slice() {
        [command] if command == "baseline" => serde_json::to_value(fidelity::baseline(&schema)?)?,
        [command] if command == "schema" => json!({
            "format_version": 1, "scope": "declared_record_schema",
            "schema_sha256": hash_bytes(&schema_bytes),
            "declarations": fidelity::declarations(&schema)?,
            "semantic_support": "unverified",
        }),
        [command, pack] if command == "audit" => audit(Path::new(pack), &schema, &schema_bytes)?,
        _ => bail!(
            "usage: record-fidelity (schema | baseline | audit <immutable pack>) [--schema <exact producing schema.json>]"
        ),
    };
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    serde_json::to_writer_pretty(&mut stdout, &output)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn audit(pack: &Path, schema: &Schema, schema_bytes: &[u8]) -> Result<serde_json::Value> {
    audit_before_final_check(pack, schema, schema_bytes, || {})
}

fn require_settled_database(database: &Path) -> Result<()> {
    for (suffix, name) in [("-wal", "WAL"), ("-journal", "rollback journal")] {
        let mut sidecar = database.as_os_str().to_owned();
        sidecar.push(suffix);
        match std::fs::symlink_metadata(Path::new(&sidecar)) {
            Ok(metadata) => ensure!(
                metadata.is_file() && metadata.len() == 0,
                "database has a {name}; audit a settled immutable snapshot"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn audit_before_final_check(
    pack: &Path,
    schema: &Schema,
    schema_bytes: &[u8],
    before_final_check: impl FnOnce(),
) -> Result<serde_json::Value> {
    let database = pack.join("skyrim_world.db").canonicalize()?;
    // immutable=1 prevents SQLite from creating journals/SHM in a registered pack.
    // It also ignores recovery state. Check both boundaries, without recovery writes.
    require_settled_database(&database)?;
    let path = database
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("non-UTF-8 database path"))?
        .as_bytes()
        .iter()
        .map(|&byte| {
            if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    let conn = Connection::open_with_flags(
        format!("file:{path}?immutable=1&mode=ro"),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let reader_bytes = std::fs::read(pack.join("record-reader.json"))?;
    let reader: serde_json::Value = serde_json::from_slice(&reader_bytes)?;
    ensure!(
        reader["mode"] == "inhouse",
        "source-payload audit requires an in-house pack; legacy coverage is unavailable"
    );
    let schemas = retained_schemas(&conn, schema_bytes, &reader)?;
    let addon_declarations = fidelity::addon_declarations(schema, &schemas)?;
    let manifest = std::fs::read(pack.join("conversion-manifest.json"))?;
    let manifest_value: serde_json::Value = serde_json::from_slice(&manifest)?;
    let plugins = conn.prepare("SELECT id,name,priority,hex(checksum) FROM plugins ORDER BY priority,id")?
        .query_map([], |row| Ok(json!({"id": row.get::<_, i64>(0)?, "name": row.get::<_, String>(1)?, "priority": row.get::<_, i64>(2)?, "checksum_hex": row.get::<_, String>(3)?})))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let before = hash_file(&database)?;
    let observed = fidelity::audit_runtime_storage(&conn, &schemas)?;
    ensure!(
        observed.canonical_records_without_source == 0,
        "incomplete source evidence: {} canonical records have no retained source payload",
        observed.canonical_records_without_source
    );
    before_final_check();
    ensure!(
        before == hash_file(&database)?,
        "database changed during audit"
    );
    ensure!(
        reader_bytes == std::fs::read(pack.join("record-reader.json"))?,
        "reader identity changed during audit"
    );
    ensure!(
        hash_bytes(&manifest) == hash_file(&pack.join("conversion-manifest.json"))?,
        "conversion manifest changed during audit"
    );
    let audit_binary_sha256 = hash_file(&std::env::current_exe()?)?;
    // This boundary check is not an atomic snapshot or a concurrent-writer lock.
    require_settled_database(&database)?;
    Ok(json!({
        "format_version": 3,
        "scope": "winning_inhouse_source_payloads_to_canonical_subrecords",
        "database_sha256": before,
        "reader": reader,
        "reader_identity_sha256": hash_bytes(&reader_bytes),
        "audit_binary_sha256": audit_binary_sha256,
        "schema_sha256": schemas.schema_sha256(),
        "builtin_schema_sha256": hash_bytes(schema_bytes),
        "schema_addons": schemas.identities(),
        "addon_declarations": addon_declarations,
        "conversion_manifest_sha256": hash_bytes(&manifest),
        "converter_schema_version": manifest_value["schema_version"],
        "configuration_hash": manifest_value["configuration_hash"],
        "plugins": plugins,
        "coverage": observed.coverage(),
        "storage": observed,
        "semantic_support": "unverified",
        "not_covered": ["nested source field occurrences", "record headers and GRUP framing", "overridden, deleted and skipped source versions", "runtime semantic use", "NIF and animation", "textures and mip/channel topology", "terrain and LOD transformations", "PEX and audio/interface resources", "archive membership and precedence"],
    }))
}

/// Rebuild only from the pack's exact ordered documents, never configured add-on paths.
fn retained_schemas(
    conn: &Connection,
    base: &[u8],
    reader: &serde_json::Value,
) -> Result<RuntimeSchemas> {
    let count: u64 = conn.query_row("SELECT count(*) FROM inhouse_schema_addons", [], |row| {
        row.get(0)
    })?;
    ensure!(
        count <= MAX_SCHEMA_ADDONS as u64,
        "retained schema add-ons exceed the file limit"
    );
    let mut statement = conn.prepare("SELECT ordinal,name,sha256,length(schema_json),schema_json FROM inhouse_schema_addons ORDER BY ordinal")?;
    let mut rows = statement.query([])?;
    let mut documents = Vec::new();
    let mut identities = Vec::new();
    let mut total = 0usize;
    while let Some(row) = rows.next()? {
        let ordinal: u64 = row.get(0)?;
        ensure!(
            ordinal == documents.len() as u64,
            "retained schema add-on ordinals must be contiguous from zero"
        );
        let name: String = row.get(1)?;
        let sha256: String = row.get(2)?;
        let size: usize = row.get(3)?;
        ensure!(
            size <= MAX_SCHEMA_ADDON_BYTES,
            "retained schema add-on {name} exceeds the byte limit"
        );
        total = total
            .checked_add(size)
            .ok_or_else(|| color_eyre::eyre::eyre!("retained schema add-on size overflow"))?;
        ensure!(
            total <= MAX_SCHEMA_ADDON_TOTAL_BYTES,
            "retained schema add-ons exceed the batch byte limit"
        );
        let bytes: Vec<u8> = row.get(4)?;
        ensure!(
            hash_bytes(&bytes) == sha256,
            "retained schema add-on {name} hash mismatch"
        );
        documents.push(bytes);
        identities.push(SchemaAddonIdentity { name, sha256 });
    }
    let bytes: Vec<_> = documents.iter().map(Vec::as_slice).collect();
    let schemas = RuntimeSchemas::from_base_and_addon_bytes(base, &bytes)
        .map_err(color_eyre::eyre::Error::msg)?;
    ensure!(
        schemas.identities() == identities,
        "retained schema add-on names or hashes disagree with document contents"
    );
    let recorded = reader
        .get("schema_addons")
        .cloned()
        .unwrap_or_else(|| json!([]));
    ensure!(
        recorded == serde_json::to_value(schemas.identities())?,
        "reader schema add-on identities or order disagree with retained documents"
    );
    ensure!(
        reader["schema_sha256"].as_str() == Some(schemas.schema_sha256()),
        "pack schema differs from selected schema and retained add-ons; supply its exact producing built-in schema with --schema"
    );
    Ok(schemas)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(root: &Path, schema_bytes: &[u8]) -> std::path::PathBuf {
        let path = root.join("pack # ? % ü");
        std::fs::create_dir(&path).unwrap();
        let conn = Connection::open(path.join("skyrim_world.db")).unwrap();
        conn.execute_batch("CREATE TABLE records(form_id INTEGER PRIMARY KEY,record_type TEXT,data BLOB);
            CREATE TABLE inhouse_source_records(form_id INTEGER PRIMARY KEY,record_type TEXT,payload BLOB);
            CREATE TABLE inhouse_schema_addons(ordinal INTEGER,name TEXT,sha256 TEXT,schema_json BLOB);
            CREATE TABLE plugins(id INTEGER,name TEXT,priority INTEGER,checksum BLOB);").unwrap();
        drop(conn);
        std::fs::write(
            path.join("record-reader.json"),
            serde_json::to_vec(&json!({"mode":"inhouse","schema_sha256":hash_bytes(schema_bytes)}))
                .unwrap(),
        )
        .unwrap();
        std::fs::write(
            path.join("conversion-manifest.json"),
            br#"{"schema_version":28,"configuration_hash":"fixture"}"#,
        )
        .unwrap();
        path
    }

    #[test]
    fn old_pack_requires_exact_producing_schema_and_remains_unchanged() {
        let root = tempfile::tempdir().unwrap();
        // Even semantically identical but byte-different schemas require explicit selection.
        let mut old_bytes = SCHEMA_BYTES.to_vec();
        old_bytes.push(b' ');
        let schema = Schema::parse(&old_bytes).unwrap();
        let path = pack(root.path(), &old_bytes);
        let before: BTreeMap<_, _> = std::fs::read_dir(&path)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.file_name().unwrap().to_owned(),
                    hash_file(&path).unwrap(),
                )
            })
            .collect();
        assert!(
            audit(&path, &schema, SCHEMA_BYTES)
                .unwrap_err()
                .to_string()
                .contains("pack schema differs")
        );
        let report = audit(&path, &schema, &old_bytes).unwrap();
        assert_eq!(report["schema_sha256"], hash_bytes(&old_bytes));
        assert_eq!(report["storage"]["source_records"], 0);
        let after: BTreeMap<_, _> = std::fs::read_dir(&path)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.file_name().unwrap().to_owned(),
                    hash_file(&path).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            before, after,
            "audit must create no journal/SHM or change any pack file"
        );
    }

    #[test]
    fn sidecars_created_or_populated_during_audit_are_rejected_without_recovery() {
        for suffix in ["-wal", "-journal"] {
            for initially_empty in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let path = pack(root.path(), SCHEMA_BYTES);
                let sidecar = path.join(format!("skyrim_world.db{suffix}"));
                if initially_empty {
                    std::fs::write(&sidecar, []).unwrap();
                }
                let before = hash_file(&path.join("skyrim_world.db")).unwrap();
                let error = audit_before_final_check(
                    &path,
                    &Schema::parse(SCHEMA_BYTES).unwrap(),
                    SCHEMA_BYTES,
                    || std::fs::write(&sidecar, b"unsettled recovery state").unwrap(),
                )
                .unwrap_err();
                assert!(error.to_string().contains("settled immutable snapshot"));
                assert_eq!(hash_file(&path.join("skyrim_world.db")).unwrap(), before);
                assert_eq!(
                    std::fs::read(&sidecar).unwrap(),
                    b"unsettled recovery state"
                );
            }
        }
    }

    #[test]
    fn rollback_journal_is_rejected_before_opening_the_database() {
        let root = tempfile::tempdir().unwrap();
        let path = pack(root.path(), SCHEMA_BYTES);
        let journal = path.join("skyrim_world.db-journal");
        std::fs::write(&journal, b"pending rollback").unwrap();
        let before = hash_file(&path.join("skyrim_world.db")).unwrap();
        let error = audit(&path, &Schema::parse(SCHEMA_BYTES).unwrap(), SCHEMA_BYTES).unwrap_err();
        assert!(error.to_string().contains("rollback journal"));
        assert_eq!(hash_file(&path.join("skyrim_world.db")).unwrap(), before);
        assert_eq!(std::fs::read(journal).unwrap(), b"pending rollback");
    }

    #[test]
    fn empty_packs_are_explicit_and_canonical_only_evidence_fails() {
        let root = tempfile::tempdir().unwrap();
        let path = pack(root.path(), SCHEMA_BYTES);
        let schema = Schema::parse(SCHEMA_BYTES).unwrap();
        let report = audit(&path, &schema, SCHEMA_BYTES).unwrap();
        assert_eq!(report["coverage"]["status"], "empty");
        assert_eq!(report["coverage"]["matched_records"], 0);
        let conn = Connection::open(path.join("skyrim_world.db")).unwrap();
        conn.execute("INSERT INTO records VALUES(1,'KYWD',X'')", [])
            .unwrap();
        drop(conn);
        let before = hash_file(&path.join("skyrim_world.db")).unwrap();
        let error = audit(&path, &schema, SCHEMA_BYTES).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("1 canonical records have no retained source payload")
        );
        assert_eq!(hash_file(&path.join("skyrim_world.db")).unwrap(), before);
    }

    #[test]
    fn live_wal_and_corrupt_addons_cannot_report_a_clean_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let path = pack(root.path(), SCHEMA_BYTES);
        let schema = Schema::parse(SCHEMA_BYTES).unwrap();
        let wal = path.join("skyrim_world.db-wal");
        std::fs::write(&wal, b"live WAL bytes").unwrap();
        assert!(
            audit(&path, &schema, SCHEMA_BYTES)
                .unwrap_err()
                .to_string()
                .contains("database has a WAL")
        );
        assert_eq!(std::fs::read(&wal).unwrap(), b"live WAL bytes");
        std::fs::remove_file(wal).unwrap();
        Connection::open(path.join("skyrim_world.db"))
            .unwrap()
            .execute(
                "INSERT INTO inhouse_schema_addons VALUES(0,'bad',?1,?2)",
                rusqlite::params![hash_bytes(b"{}"), b"{}".as_slice()],
            )
            .unwrap();
        assert!(
            audit(&path, &schema, SCHEMA_BYTES)
                .unwrap_err()
                .to_string()
                .contains("schema add-on")
        );
    }

    fn addon(name: &str, signature: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"version":1,"name":name,"records":[{"signature":signature,"fields":[{"signature":"ZVAL","name":"value","kind":"u32"}]}]})).unwrap()
    }

    fn store_addons(path: &Path, base: &[u8], documents: &[&[u8]]) -> RuntimeSchemas {
        let schemas = RuntimeSchemas::from_base_and_addon_bytes(base, documents).unwrap();
        let conn = Connection::open(path.join("skyrim_world.db")).unwrap();
        for (index, (identity, bytes)) in schemas.identities().iter().zip(documents).enumerate() {
            conn.execute(
                "INSERT INTO inhouse_schema_addons VALUES(?1,?2,?3,?4)",
                rusqlite::params![index as u64, identity.name, identity.sha256, bytes],
            )
            .unwrap();
        }
        std::fs::write(path.join("record-reader.json"), serde_json::to_vec(&json!({"mode":"inhouse","schema_sha256":schemas.schema_sha256(),"schema_addons":schemas.identities()})).unwrap()).unwrap();
        schemas
    }

    #[test]
    fn retained_addon_identity_detects_corruption_names_order_and_missing_documents() {
        for alteration in ["bytes", "name", "order", "ordinal", "missing"] {
            let root = tempfile::tempdir().unwrap();
            let path = pack(root.path(), SCHEMA_BYTES);
            let a = addon("first", "ZAAA");
            let b = addon("second", "ZBBB");
            store_addons(&path, SCHEMA_BYTES, &[&a, &b]);
            let conn = Connection::open(path.join("skyrim_world.db")).unwrap();
            match alteration {
                "bytes" => {
                    conn.execute(
                        "UPDATE inhouse_schema_addons SET schema_json=?1 WHERE ordinal=0",
                        [b"{}".as_slice()],
                    )
                    .unwrap();
                }
                "name" => {
                    conn.execute(
                        "UPDATE inhouse_schema_addons SET name='wrong' WHERE ordinal=0",
                        [],
                    )
                    .unwrap();
                }
                "order" => {
                    conn.execute_batch("UPDATE inhouse_schema_addons SET ordinal=1-ordinal")
                        .unwrap();
                }
                "ordinal" => {
                    conn.execute(
                        "UPDATE inhouse_schema_addons SET ordinal=3 WHERE ordinal=1",
                        [],
                    )
                    .unwrap();
                }
                "missing" => {
                    conn.execute("DELETE FROM inhouse_schema_addons WHERE ordinal=1", [])
                        .unwrap();
                }
                _ => unreachable!(),
            }
            drop(conn);
            let error =
                audit(&path, &Schema::parse(SCHEMA_BYTES).unwrap(), SCHEMA_BYTES).unwrap_err();
            assert!(
                error.to_string().contains("schema add-on"),
                "{alteration}: {error}"
            );
        }
    }

    #[test]
    fn old_builtin_and_retained_addons_require_the_exact_combined_producer_identity() {
        let root = tempfile::tempdir().unwrap();
        let mut base = SCHEMA_BYTES.to_vec();
        base.push(b'\n');
        let path = pack(root.path(), &base);
        let addon = addon("retained", "ZNEW");
        let schemas = store_addons(&path, &base, &[&addon]);
        let before = hash_file(&path.join("skyrim_world.db")).unwrap();
        assert!(audit(&path, &Schema::parse(SCHEMA_BYTES).unwrap(), SCHEMA_BYTES).is_err());
        let report = audit(&path, &Schema::parse(&base).unwrap(), &base).unwrap();
        assert_eq!(report["schema_sha256"], schemas.schema_sha256());
        assert_eq!(report["builtin_schema_sha256"], hash_bytes(&base));
        assert_eq!(report["schema_addons"][0]["name"], "retained");
        assert!(
            report["addon_declarations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field["id"] == "record/ZNEW/0:ZVAL:value")
        );
        assert_eq!(hash_file(&path.join("skyrim_world.db")).unwrap(), before);
    }

    use std::collections::BTreeMap;
}
