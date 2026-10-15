//! Differential SQL controls for the production canonical INSERT and its failure paths.

use super::{RECORDS_INSERT_SQL, check_record_insert, insert_canonical_record};
use rusqlite::types::{ToSqlOutput, Value};
use rusqlite::{Connection, Error, Result, Statement, ToSql, params};

/// Independent positional INSERT used as the pre-optimization SQL oracle.
const POSITIONAL_INSERT: &str = "INSERT OR REPLACE INTO records(form_id, record_type, cell_id, worldspace_id, load_order, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

/// Unequal values distinguish positional binding, optional zero, and absent values.
struct Row {
    id: u32,
    tag: &'static str,
    cell: Option<u32>,
    world: Option<u32>,
    priority: u32,
    data: Vec<u8>,
}

/// Use an independent schema with a constraint to exercise execution rollback.
fn database() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE records(form_id INTEGER PRIMARY KEY,record_type TEXT NOT NULL,cell_id INTEGER,worldspace_id INTEGER,load_order INTEGER NOT NULL,data BLOB NOT NULL CHECK(length(data)<32));CREATE INDEX idx_records_type ON records(record_type);CREATE INDEX idx_records_cell_id ON records(cell_id) WHERE cell_id IS NOT NULL;").unwrap();
    conn
}

/// Preserve the original positional-slice execute and successful binding cleanup.
fn positional_insert(statement: &mut Statement<'_>, row: &Row) -> Result<usize> {
    let changed = statement.execute(params![
        row.id,
        row.tag,
        row.cell,
        row.world,
        row.priority,
        row.data
    ])?;
    statement.clear_bindings();
    Ok(changed)
}

/// Exercise the same row-binding helper invoked by the real exporter loop.
fn production_insert(statement: &mut Statement<'_>, row: &Row) -> Result<usize> {
    insert_canonical_record(
        statement,
        row.id,
        row.tag,
        row.cell,
        row.world,
        row.priority,
        &row.data,
    )
}

/// Compare every cell alongside its SQLite storage class in deterministic ID order.
fn snapshot(conn: &Connection) -> Vec<Vec<Value>> {
    let mut query = conn.prepare("SELECT form_id,typeof(form_id),record_type,typeof(record_type),cell_id,typeof(cell_id),worldspace_id,typeof(worldspace_id),load_order,typeof(load_order),data,typeof(data) FROM records ORDER BY form_id").unwrap();
    let rows = query
        .query_map([], |row| (0..12).map(|index| row.get(index)).collect())
        .unwrap();
    rows.collect::<Result<Vec<_>>>().unwrap()
}

/// Full-width IDs, unequal fields, null/zero and replacement preserve exact SQL values/types.
#[test]
fn uneven_values_null_zero_empty_blob_and_replacement_match_exactly() {
    let mut left = database();
    let mut right = database();
    let rows = [
        Row {
            id: 0xff12_3456,
            tag: "ACTI",
            cell: None,
            world: Some(0),
            priority: 7,
            data: vec![],
        },
        Row {
            id: 19,
            tag: "INFO",
            cell: Some(0xfedc_ba98),
            world: None,
            priority: u32::MAX,
            data: vec![0, 255, 7, 19],
        },
        Row {
            id: 0xff12_3456,
            tag: "UNKN",
            cell: Some(0),
            world: Some(0xffab_cdef),
            priority: 3,
            data: vec![23, 0, 1],
        },
    ];
    let left_tx = left.transaction().unwrap();
    let right_tx = right.transaction().unwrap();
    {
        let mut positional = left_tx.prepare_cached(POSITIONAL_INSERT).unwrap();
        let mut production = right_tx.prepare_cached(RECORDS_INSERT_SQL).unwrap();
        check_record_insert(&production).unwrap();
        for row in rows {
            assert_eq!(
                positional_insert(&mut positional, &row).unwrap(),
                production_insert(&mut production, &row).unwrap()
            );
        }
    }
    left_tx.commit().unwrap();
    right_tx.commit().unwrap();
    assert_eq!(snapshot(&left), snapshot(&right));
    assert_eq!(right.query_row("SELECT count(*) FROM records WHERE form_id=19 AND worldspace_id IS NULL AND typeof(data)='blob'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
}

/// A zero-length payload remains a BLOB, with optional zero distinct from SQL NULL.
#[test]
fn empty_payload_is_blob_not_null_and_optional_zero_is_not_absent() {
    let conn = database();
    let mut statement = conn.prepare(RECORDS_INSERT_SQL).unwrap();
    check_record_insert(&statement).unwrap();
    production_insert(
        &mut statement,
        &Row {
            id: 1,
            tag: "GLOB",
            cell: Some(0),
            world: None,
            priority: 9,
            data: vec![],
        },
    )
    .unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT typeof(data),length(data),cell_id,worldspace_id IS NULL FROM records",
            [],
            |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, bool>(3)?
            ))
        )
        .unwrap(),
        ("blob".into(), 0, 0, true)
    );
}

/// The same SQL failure rolls back both the failed row and a preceding usable row.
#[test]
fn sql_constraint_error_and_failed_transaction_rollback_match() {
    let mut left = database();
    let mut right = database();
    let usable = Row {
        id: 7,
        tag: "ACTI",
        cell: Some(3),
        world: None,
        priority: 2,
        data: vec![4, 5],
    };
    let broken = Row {
        id: 8,
        tag: "STAT",
        cell: None,
        world: Some(5),
        priority: 1,
        data: vec![7; 32],
    };
    let left_tx = left.transaction().unwrap();
    let right_tx = right.transaction().unwrap();
    {
        let mut positional = left_tx.prepare_cached(POSITIONAL_INSERT).unwrap();
        let mut production = right_tx.prepare_cached(RECORDS_INSERT_SQL).unwrap();
        check_record_insert(&production).unwrap();
        positional_insert(&mut positional, &usable).unwrap();
        production_insert(&mut production, &usable).unwrap();
        let positional_error = positional_insert(&mut positional, &broken).unwrap_err();
        let production_error = production_insert(&mut production, &broken).unwrap_err();
        assert_eq!(
            positional_error.sqlite_error_code(),
            production_error.sqlite_error_code()
        );
        assert_eq!(positional_error.to_string(), production_error.to_string());
    }
    drop(left_tx);
    drop(right_tx);
    assert!(snapshot(&left).is_empty());
    assert!(snapshot(&right).is_empty());
}

/// A drifted statement is rejected before stepping with the old slice diagnostic.
#[test]
fn wrong_statement_cardinality_does_not_step_and_keeps_slice_diagnostic() {
    let conn = database();
    let row = Row {
        id: 9,
        tag: "MATT",
        cell: None,
        world: None,
        priority: 0,
        data: vec![4],
    };
    for sql in [
        "SELECT ?1,?2,?3",
        "SELECT ?1,?2,?3,?4,?5",
        "SELECT ?1,?2,?3,?4,?5,?6,?7",
    ] {
        let mut positional = conn.prepare(sql).unwrap();
        let production = conn.prepare(sql).unwrap();
        let positional_error = positional_insert(&mut positional, &row).unwrap_err();
        let production_error = check_record_insert(&production).unwrap_err();
        assert_eq!(positional_error.to_string(), production_error.to_string());
        assert!(matches!(production_error, Error::InvalidParameterCount(..)));
    }
    assert!(snapshot(&conn).is_empty());
}

/// Independently fail ToSql conversion before SQLite can execute a row.
struct BrokenValue;

impl ToSql for BrokenValue {
    /// Return a stable synthetic conversion error for both binding APIs.
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        Err(Error::ToSqlConversionFailure(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "synthetic conversion failure",
            ),
        )))
    }
}

/// The production helper returns before stepping, and cached drop permits a clean next row.
#[test]
fn bind_conversion_failure_returns_before_execute_and_cached_drop_cleans() {
    let conn = database();
    {
        let mut positional = conn.prepare_cached(POSITIONAL_INSERT).unwrap();
        let positional_error = positional
            .execute(params![
                1_u32,
                BrokenValue,
                None::<u32>,
                None::<u32>,
                0_u32,
                vec![7_u8]
            ])
            .unwrap_err();
        drop(positional);
        let mut production = conn.prepare_cached(RECORDS_INSERT_SQL).unwrap();
        check_record_insert(&production).unwrap();
        let production_error =
            insert_canonical_record(&mut production, 1, BrokenValue, None, None, 0, &[7])
                .unwrap_err();
        assert_eq!(positional_error.to_string(), production_error.to_string());
    }
    assert!(snapshot(&conn).is_empty());
    let mut reused = conn.prepare_cached(RECORDS_INSERT_SQL).unwrap();
    check_record_insert(&reused).unwrap();
    production_insert(
        &mut reused,
        &Row {
            id: 23,
            tag: "CAMS",
            cell: Some(4),
            world: None,
            priority: 11,
            data: vec![19, 2],
        },
    )
    .unwrap();
    assert_eq!(snapshot(&conn).len(), 1);
    assert_eq!(
        conn.query_row(
            "SELECT form_id,record_type,cell_id,worldspace_id,load_order,data FROM records",
            [],
            |row| Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, Option<u32>>(3)?,
                row.get::<_, u32>(4)?,
                row.get::<_, Vec<u8>>(5)?
            ))
        )
        .unwrap(),
        (23, "CAMS".into(), 4, None, 11, vec![19, 2])
    );
}
