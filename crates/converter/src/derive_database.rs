//! Logical preservation snapshot for extension-only derivation.
//! Caller provides a stable read transaction or dataset lock covering the scan.
//! Memory is proportional to schema/column metadata, never the number/size of rows.

use color_eyre::{Result, eyre::ensure};
use rusqlite::{Connection, types::ValueRef};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DatabaseSnapshot {
    schema: [u8; 32],
    tables: BTreeMap<String, TableSnapshot>,
}

#[derive(Debug, PartialEq, Eq)]
struct TableSnapshot {
    rows: u64,
    digest: [u8; 32],
}

struct Column {
    name: String,
    primary_key: u32,
}

/// Snapshot every non-owned main-schema object and table, including SQLite
/// sequence/statistics tables and virtual-table shadow tables. Exclusions are
/// exact ASCII case-insensitive names, matching SQLite identifier semantics.
/// The caller must explicitly list owned virtual-table shadows too.
pub(crate) fn snapshot(
    connection: &Connection,
    owned_tables: &BTreeSet<String>,
) -> Result<DatabaseSnapshot> {
    let owned: BTreeSet<_> = owned_tables
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let mut schema = Sha256::new();
    schema.update(b"mudcrab-db-preservation-schema-v1\0");
    let mut tables = Vec::new();
    let mut statement = connection.prepare(
        "SELECT type,name,tbl_name,sql FROM main.sqlite_master \
         ORDER BY type COLLATE BINARY,name COLLATE BINARY,tbl_name COLLATE BINARY,sql COLLATE BINARY",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        let table: String = row.get(2)?;
        if owned.contains(&name.to_ascii_lowercase()) || owned.contains(&table.to_ascii_lowercase())
        {
            continue;
        }
        schema.update(b"O");
        for index in 0..4 {
            hash_value(&mut schema, row.get_ref(index)?);
        }
        if kind == "table" {
            tables.push(name);
        }
    }
    drop(rows);
    drop(statement);

    // SQLite reports WITHOUT ROWID directly, avoiding a CREATE SQL substring
    // heuristic which can be fooled by comments, identifiers or quoted strings.
    let mut without_rowid = BTreeMap::new();
    let mut statement = connection.prepare("PRAGMA main.table_list")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let schema_name: String = row.get(0)?;
        if schema_name == "main" {
            without_rowid.insert(row.get::<_, String>(1)?, row.get::<_, bool>(4)?);
        }
    }

    let mut snapshots = BTreeMap::new();
    for table in tables {
        let without_rowid = *without_rowid
            .get(&table)
            .ok_or_else(|| color_eyre::eyre::eyre!("table {table:?} vanished during snapshot"))?;
        snapshots.insert(
            table.clone(),
            snapshot_table(connection, &table, without_rowid)?,
        );
    }
    Ok(DatabaseSnapshot {
        schema: schema.finalize().into(),
        tables: snapshots,
    })
}

fn snapshot_table(
    connection: &Connection,
    table: &str,
    without_rowid: bool,
) -> Result<TableSnapshot> {
    let quoted = quote_identifier(table);
    let mut info = connection.prepare(&format!("PRAGMA main.table_xinfo({quoted})"))?;
    let columns: Vec<Column> = info
        .query_map([], |row| {
            Ok(Column {
                name: row.get(1)?,
                primary_key: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    ensure!(!columns.is_empty(), "table {table:?} has no columns");
    let query = if without_rowid {
        let mut primary: Vec<_> = columns
            .iter()
            .filter(|column| column.primary_key != 0)
            .collect();
        primary.sort_unstable_by_key(|column| column.primary_key);
        ensure!(
            !primary.is_empty(),
            "WITHOUT ROWID table {table:?} has no primary key"
        );
        let order = primary
            .iter()
            .map(|column| quote_identifier(&column.name))
            .collect::<Vec<_>>()
            .join(",");
        format!("SELECT * FROM main.{quoted} ORDER BY {order}")
    } else {
        // Explicitly hash logical rowids as well as values. A user column can
        // shadow rowid, so select another genuine SQLite alias when necessary.
        let alias = ["rowid", "_rowid_", "oid"].into_iter().find(|alias| {
            columns
                .iter()
                .all(|column| !column.name.eq_ignore_ascii_case(alias))
        });
        let alias = alias.ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "table {table:?} shadows every rowid alias; cannot snapshot its logical rowids"
            )
        })?;
        let alias = quote_identifier(alias);
        format!("SELECT {alias},* FROM main.{quoted} ORDER BY {alias}")
    };
    let mut statement = connection.prepare(&query)?;
    let count = statement.column_count();
    let mut hash = Sha256::new();
    hash.update(b"mudcrab-db-preservation-table-v1\0");
    hash_bytes(&mut hash, table.as_bytes());
    hash.update((count as u64).to_le_bytes());
    for name in statement.column_names() {
        hash_bytes(&mut hash, name.as_bytes());
    }
    let mut rows = statement.query([])?;
    let mut row_count = 0u64;
    while let Some(row) = rows.next()? {
        hash.update(b"R");
        for index in 0..count {
            hash_value(&mut hash, row.get_ref(index)?);
        }
        row_count = row_count
            .checked_add(1)
            .ok_or_else(|| color_eyre::eyre::eyre!("row count overflow"))?;
    }
    Ok(TableSnapshot {
        rows: row_count,
        digest: hash.finalize().into(),
    })
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn hash_bytes(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn hash_value(hash: &mut Sha256, value: ValueRef<'_>) {
    match value {
        ValueRef::Null => {
            hash.update([0]);
            hash_bytes(hash, &[]);
        }
        ValueRef::Integer(value) => {
            hash.update([1]);
            hash_bytes(hash, &value.to_le_bytes());
        }
        ValueRef::Real(value) => {
            hash.update([2]);
            hash_bytes(hash, &value.to_bits().to_le_bytes());
        }
        ValueRef::Text(bytes) => {
            hash.update([3]);
            hash_bytes(hash, bytes);
        }
        ValueRef::Blob(bytes) => {
            hash.update([4]);
            hash_bytes(hash, bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(connection: &Connection) -> DatabaseSnapshot {
        snapshot(connection, &BTreeSet::new()).unwrap()
    }

    fn database(sql: &str) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(sql).unwrap();
        connection
    }

    #[test]
    fn equal_row_counts_do_not_certify_changed_values_or_rowids() {
        let connection = database("CREATE TABLE core(value); INSERT INTO core VALUES('a'),('b');");
        let before = snap(&connection);
        connection
            .execute("UPDATE core SET value='c' WHERE rowid=2", [])
            .unwrap();
        assert_ne!(before, snap(&connection));
        connection
            .execute("UPDATE core SET value='b' WHERE rowid=2", [])
            .unwrap();
        assert_eq!(before, snap(&connection));
        connection
            .execute("UPDATE core SET rowid=3 WHERE rowid=2", [])
            .unwrap();
        assert_ne!(before, snap(&connection));
    }

    #[test]
    fn constraints_defaults_triggers_and_indexes_are_preserved_as_full_ddl() {
        let original = database("CREATE TABLE core(value INTEGER DEFAULT 1 CHECK(value>0));");
        for sql in [
            "CREATE TABLE core(value INTEGER DEFAULT 2 CHECK(value>0));",
            "CREATE TABLE core(value INTEGER DEFAULT 1 CHECK(value>=0));",
            "CREATE TABLE core(value INTEGER DEFAULT 1 CHECK(value>0)); CREATE INDEX value_index ON core(value);",
            "CREATE TABLE core(value INTEGER DEFAULT 1 CHECK(value>0)); CREATE TRIGGER changed AFTER INSERT ON core BEGIN SELECT 1; END;",
            "CREATE TABLE core(value INTEGER DEFAULT 1 CHECK(value>0)); CREATE VIEW projected AS SELECT value FROM core;",
        ] {
            assert_ne!(
                snap(&original),
                snap(&database(sql)),
                "DDL difference: {sql}"
            );
        }
        let unique = database("CREATE TABLE core(value INTEGER UNIQUE);");
        let plain = database("CREATE TABLE core(value INTEGER);");
        assert_ne!(snap(&unique), snap(&plain)); // Includes automatic index's NULL SQL.
    }

    #[test]
    fn without_rowid_orders_by_composite_primary_key_not_insertion_order() {
        let schema = "CREATE TABLE core(value, b TEXT, a INTEGER, PRIMARY KEY(a,b)) WITHOUT ROWID;";
        let first = database(schema);
        let second = database(schema);
        first
            .execute_batch("INSERT INTO core VALUES(X'00FF','z',2),('text','b',1), (NULL,'a',1);")
            .unwrap();
        second
            .execute_batch("INSERT INTO core VALUES(NULL,'a',1),('text','b',1), (X'00FF','z',2);")
            .unwrap();
        assert_eq!(snap(&first), snap(&second));
        second
            .execute("UPDATE core SET value='changed' WHERE a=1 AND b='a'", [])
            .unwrap();
        assert_ne!(snap(&first), snap(&second));
    }

    #[test]
    fn typed_values_keep_blob_text_null_integer_and_float_distinct() {
        let connection = database(
            "CREATE TABLE core(value); INSERT INTO core VALUES(NULL),(X'80FF'),(CAST(X'80FF' AS TEXT)),(1),(1.0);",
        );
        let original = snap(&connection);
        connection
            .execute(
                "UPDATE core SET value=CAST(value AS TEXT) WHERE rowid=2",
                [],
            )
            .unwrap();
        assert_ne!(original, snap(&connection));
        connection
            .execute("UPDATE core SET value=X'80FF' WHERE rowid=2", [])
            .unwrap();
        assert_eq!(original, snap(&connection));
        connection
            .execute("UPDATE core SET value=1 WHERE rowid=5", [])
            .unwrap();
        assert_ne!(original, snap(&connection));
        let null = database("CREATE TABLE core(value); INSERT INTO core VALUES(NULL);");
        let empty = database("CREATE TABLE core(value); INSERT INTO core VALUES(X'');");
        assert_ne!(snap(&null), snap(&empty));
    }

    #[test]
    fn exact_owned_tables_allow_additions_but_never_new_core_objects() {
        let connection = database("CREATE TABLE core(value); INSERT INTO core VALUES(1);");
        let owned = BTreeSet::from(["clusters".to_owned()]);
        let before = snapshot(&connection, &owned).unwrap();
        connection.execute_batch("CREATE TABLE CLUSTERS(id INTEGER UNIQUE); INSERT INTO CLUSTERS VALUES(2); CREATE INDEX cluster_index ON CLUSTERS(id); CREATE TRIGGER cluster_trigger AFTER INSERT ON CLUSTERS BEGIN SELECT 1; END;").unwrap();
        assert_eq!(before, snapshot(&connection, &owned).unwrap());
        connection
            .execute_batch("CREATE TABLE unexpected(value);")
            .unwrap();
        assert_ne!(before, snapshot(&connection, &owned).unwrap());
        connection
            .execute_batch("DROP TABLE unexpected; CREATE INDEX changed_core ON core(value);")
            .unwrap();
        assert_ne!(before, snapshot(&connection, &owned).unwrap());
    }

    #[test]
    fn virtual_rtree_and_shadow_rows_remain_part_of_core() {
        let connection = database(
            "CREATE VIRTUAL TABLE spatial USING rtree(id,min_x,max_x,min_y,max_y); INSERT INTO spatial VALUES(1,0,10,0,10);",
        );
        let before = snap(&connection);
        assert!(before.tables.contains_key("spatial"));
        assert!(before.tables.contains_key("spatial_node"));
        assert!(before.tables.contains_key("spatial_parent"));
        assert!(before.tables.contains_key("spatial_rowid"));
        let allowed = BTreeSet::from([
            "cluster_spatial".into(),
            "cluster_spatial_node".into(),
            "cluster_spatial_parent".into(),
            "cluster_spatial_rowid".into(),
        ]);
        let before_owned_addition = snapshot(&connection, &allowed).unwrap();
        connection.execute_batch("CREATE VIRTUAL TABLE cluster_spatial USING rtree(id,min_x,max_x,min_y,max_y); INSERT INTO cluster_spatial VALUES(9,1,2,3,4);").unwrap();
        assert_eq!(
            before_owned_addition,
            snapshot(&connection, &allowed).unwrap()
        );
        // Listing only the virtual parent never implicitly exempts shadow tables.
        assert_ne!(
            before,
            snapshot(&connection, &BTreeSet::from(["cluster_spatial".into()])).unwrap()
        );
        connection
            .execute("UPDATE spatial SET max_x=11 WHERE id=1", [])
            .unwrap();
        assert_ne!(
            before_owned_addition,
            snapshot(&connection, &allowed).unwrap()
        );
    }

    #[test]
    fn sqlite_sequence_and_statistics_are_not_silently_exempted() {
        let connection = database(
            "CREATE TABLE core(id INTEGER PRIMARY KEY AUTOINCREMENT,value); INSERT INTO core(value) VALUES('a'); CREATE INDEX core_value ON core(value); ANALYZE;",
        );
        let before = snap(&connection);
        assert!(before.tables.contains_key("sqlite_sequence"));
        assert!(before.tables.contains_key("sqlite_stat1"));
        connection
            .execute("UPDATE sqlite_sequence SET seq=100 WHERE name='core'", [])
            .unwrap();
        assert_ne!(before, snap(&connection));
        connection
            .execute("UPDATE sqlite_sequence SET seq=1 WHERE name='core'", [])
            .unwrap();
        assert_eq!(before, snap(&connection));
        connection
            .execute("UPDATE sqlite_stat1 SET stat='999 1' WHERE tbl='core'", [])
            .unwrap();
        assert_ne!(before, snap(&connection));
    }

    #[test]
    fn quoted_identifiers_and_user_rowid_column_do_not_hide_real_rowid_changes() {
        let connection = database(
            "CREATE TABLE \"odd\"\"name\"(\"rowid\" TEXT,\"v\"\"x\" BLOB); INSERT INTO \"odd\"\"name\" VALUES('authored',X'0102');",
        );
        let before = snap(&connection);
        connection
            .execute("UPDATE \"odd\"\"name\" SET _rowid_=8", [])
            .unwrap();
        assert_ne!(before, snap(&connection));
        let inaccessible = database("CREATE TABLE core(rowid,_rowid_,oid);");
        assert!(snapshot(&inaccessible, &BTreeSet::new()).is_err());
    }
}
