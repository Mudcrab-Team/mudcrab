//! File-backed statement reuse retains NULLs, raw provenance and local recovery.
use converter::esm::{
    exporter::{create_tables, export_to_db},
    extractors::serialize_subrecords,
    records::RawRecord,
};
use rusqlite::{Connection, Result, Transaction, params};
use std::{collections::HashMap, path::Path};

const RECORDS: &str = "INSERT OR REPLACE INTO records(form_id, record_type, cell_id, worldspace_id, load_order, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
const SOURCE: &str = "INSERT INTO inhouse_source_records(form_id,source_form_id,load_order,record_type,flags,payload) VALUES (?1,?2,?3,?4,?5,?6)";
const LOCALIZED: &str = "INSERT INTO inhouse_localized_fields(form_id,field_index,signature,field_name,string_table,string_id,load_order,resolved_text,status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)";

/// Exercise identical bindings through cached and uncached SQLite statements.
fn execute(
    tx: &Transaction<'_>,
    cached: bool,
    sql: &str,
    p: impl rusqlite::Params,
) -> Result<usize> {
    if cached {
        tx.prepare_cached(sql)?.execute(p)
    } else {
        tx.execute(sql, p)
    }
}

/// Use an actual file with the converter schema and bounded statement cache.
fn connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.set_prepared_statement_cache_capacity(64);
    create_tables(&conn)?;
    Ok(conn)
}

/// Insert asymmetric NULL, empty, source and canonical values in a transaction.
fn populate(conn: &Connection, cached: bool) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE inhouse_source_records(form_id INTEGER PRIMARY KEY, source_form_id INTEGER NOT NULL, load_order INTEGER NOT NULL, record_type TEXT NOT NULL, flags INTEGER NOT NULL, payload BLOB NOT NULL);
        CREATE TABLE inhouse_localized_fields(form_id INTEGER NOT NULL, field_index INTEGER NOT NULL, signature TEXT NOT NULL, field_name TEXT NOT NULL, string_table TEXT NOT NULL, string_id INTEGER NOT NULL, load_order INTEGER NOT NULL, resolved_text TEXT, status TEXT NOT NULL CHECK(status IN ('resolved','null_id','missing')), PRIMARY KEY(form_id,field_index));")?;
    // Deliberate insertion order2,1 differs from the assertion's independently sorted order.
    // Different source and canonical BLOBs stop a shared-buffer mistake passing unnoticed.
    for (id, cell, world, canonical, original) in [
        (
            2u32,
            Some(71u32),
            Some(72u32),
            vec![9u8, 8, 7],
            vec![0xaau8, 0, 0xff],
        ),
        (
            1u32,
            None,
            None,
            vec![0u8, 127, 128, 255],
            vec![0xbbu8, 0xcc],
        ),
    ] {
        let archived = serialize_subrecords(&[(b"DATA".to_vec(), canonical)]);
        execute(
            &tx,
            cached,
            RECORDS,
            params![id, "STAT", cell, world, id + 10, archived],
        )?;
        execute(
            &tx,
            cached,
            SOURCE,
            params![
                id,
                0x01000000u32 + id,
                id + 10,
                "STAT",
                0x80u32 + id,
                original
            ],
        )?;
    }
    for (index, text, status, id) in [
        (0u32, Some("populated"), "resolved", 17u32),
        (1, None, "missing", 18),
        (2, Some(""), "resolved", 19),
        (3, None, "null_id", 0),
    ] {
        execute(
            &tx,
            cached,
            LOCALIZED,
            params![
                1u32, index, "FULL", "name", "strings", id, 11u32, text, status
            ],
        )?;
    }
    tx.commit()
}

type RecordRow = (u32, Option<u32>, Option<u32>, u32, Vec<u8>);
type SourceRow = (u32, u32, u32, u32, Vec<u8>);
type TextRow = (u32, Option<String>, String, u32);
/// Select typed values and exact BLOBs independently in primary-key order.
fn rows(conn: &Connection) -> Result<(Vec<RecordRow>, Vec<SourceRow>, Vec<TextRow>)> {
    let a = conn
        .prepare(
            "SELECT form_id,cell_id,worldspace_id,load_order,data FROM records ORDER BY form_id",
        )?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    let b=conn.prepare("SELECT form_id,source_form_id,load_order,flags,payload FROM inhouse_source_records ORDER BY form_id")?
        .query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?.collect::<Result<Vec<_>>>()?;
    let c=conn.prepare("SELECT field_index,resolved_text,status,string_id FROM inhouse_localized_fields ORDER BY field_index")?
        .query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<Result<Vec<_>>>()?;
    Ok((a, b, c))
}

#[test]
/// Statement reuse rebinds nullable values and never substitutes runtime for source bytes.
fn cached_and_plain_dml_rebind_nulls_and_keep_canonical_vs_source_bytes() -> Result<()> {
    let root = tempfile::tempdir().unwrap();
    let plain = connection(&root.path().join("plain.sqlite"))?;
    let cached = connection(&root.path().join("cached.sqlite"))?;
    populate(&plain, false)?;
    populate(&cached, true)?;
    let a = rows(&plain)?;
    let b = rows(&cached)?;
    assert_eq!(a, b);
    assert_eq!(b.0[0].0, 1);
    assert_eq!((b.0[0].1, b.0[0].2), (None, None));
    assert_eq!((b.0[1].1, b.0[1].2), (Some(71), Some(72)));
    assert_eq!(
        b.0[0].4,
        serialize_subrecords(&[(b"DATA".to_vec(), vec![0, 127, 128, 255])])
    );
    assert_eq!(b.1[0], (1, 0x01000001, 11, 0x81, vec![0xbb, 0xcc]));
    assert_ne!(b.0[0].4, b.1[0].4);
    assert_eq!(
        b.2,
        vec![
            (0, Some("populated".into()), "resolved".into(), 17),
            (1, None, "missing".into(), 18),
            (2, Some("".into()), "resolved".into(), 19),
            (3, None, "null_id".into(), 0)
        ]
    );
    Ok(())
}

#[test]
/// A failed transaction can roll back while the cached statement remains reusable.
fn cached_constraint_failure_rolls_back_and_same_connection_can_reuse_statement() -> Result<()> {
    for cached in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let conn = connection(&root.path().join("rollback.sqlite"))?;
        conn.execute_batch("CREATE TABLE guard(id INTEGER PRIMARY KEY,value TEXT NOT NULL CHECK(value <> 'bad')); INSERT INTO guard VALUES(77,'original');")?;
        {
            let tx = conn.unchecked_transaction()?;
            execute(
                &tx,
                cached,
                "INSERT INTO guard(id,value) VALUES (?1,?2)",
                params![1, "good"],
            )?;
            assert!(
                execute(
                    &tx,
                    cached,
                    "INSERT INTO guard(id,value) VALUES (?1,?2)",
                    params![2, "bad"]
                )
                .is_err()
            );
            // Explicitly exercise the existing default-drop rollback after a local error.
        }
        assert_eq!(
            conn.query_row("SELECT count(*) FROM guard", [], |r| r.get::<_, u32>(0))?,
            1
        );
        {
            let tx = conn.unchecked_transaction()?;
            execute(
                &tx,
                cached,
                "INSERT INTO guard(id,value) VALUES (?1,?2)",
                params![3, "next"],
            )?;
            tx.commit()?;
        }
        assert_eq!(
            conn.query_row("SELECT value FROM guard WHERE id=77", [], |r| r
                .get::<_, String>(0))?,
            "original"
        );
        assert_eq!(
            conn.query_row("SELECT value FROM guard WHERE id=3", [], |r| r
                .get::<_, String>(0))?,
            "next"
        );
    }
    Ok(())
}

/// Construct records independently of the exporter projections.
fn raw(id: u32, kind: [u8; 4], subrecords: Vec<(Vec<u8>, Vec<u8>)>) -> RawRecord {
    RawRecord {
        form_id: id,
        record_type: kind,
        flags: 0,
        subrecords,
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    }
}

#[test]
/// An unusable light projection preserves its raw record and valid neighboring light.
fn actual_subset_export_keeps_malformed_light_warning_and_valid_neighbor_projection() -> Result<()>
{
    let root = tempfile::tempdir().unwrap();
    let conn = connection(&root.path().join("neighbors.sqlite"))?;
    let bad = raw(1, *b"LIGH", vec![(b"EDID".to_vec(), b"bad\0".to_vec())]);
    let mut data = vec![0; 20];
    data[4..8].copy_from_slice(&25u32.to_le_bytes());
    data[8..11].copy_from_slice(&[9, 10, 11]);
    data[12..16].copy_from_slice(&7u32.to_le_bytes());
    data[16..20].copy_from_slice(&0.5f32.to_le_bytes());
    let good = raw(
        2,
        *b"LIGH",
        vec![
            (b"EDID".to_vec(), b"good\0".to_vec()),
            (b"DATA".to_vec(), data),
        ],
    );
    export_to_db(&conn, &HashMap::from([(1, bad), (2, good)]))?;
    assert_eq!(
        conn.query_row("SELECT count(*) FROM records", [], |r| r.get::<_, u32>(0))?,
        2
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM lights", [], |r| r.get::<_, u32>(0))?,
        1
    );
    assert_eq!(
        conn.query_row("SELECT radius FROM lights WHERE id=2", [], |r| r
            .get::<_, f64>(0))?,
        25.0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM formid_map", [], |r| r
            .get::<_, u32>(0))?,
        0
    );
    // Root retained stderr should contain the existing missing-DATA warning for only id1.
    Ok(())
}
