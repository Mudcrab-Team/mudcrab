//! Scalar unresolved-texture diagnostics for the owned preserved-terrain snapshot.

use rusqlite::{Transaction, params};
use std::collections::BTreeSet;

/// Replace only the terrain tooling tables inside their owner's export transaction.
///
/// Recreating the parent also removes the legacy JSON list column when a caller
/// reuses a database. Failure leaves the previous snapshot intact on rollback.
pub(super) fn replace_snapshot_tables(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS inhouse_unresolved_terrain_textures;
         DROP TABLE IF EXISTS inhouse_terrain_layers;
         CREATE TABLE inhouse_terrain_layers(
             form_id INTEGER PRIMARY KEY,
             cell_id INTEGER NOT NULL,
             load_order INTEGER NOT NULL,
             layer_data BLOB NOT NULL);
         CREATE TABLE inhouse_unresolved_terrain_textures(
             form_id INTEGER NOT NULL,
             texture_form_id INTEGER NOT NULL,
             PRIMARY KEY(form_id,texture_form_id)) WITHOUT ROWID;",
    )
}

/// Publish exactly the already filtered, distinct unresolved IDs for one LAND owner.
///
/// The existing caller excludes zero and keeps the authored IDs whose final
/// target is missing or is not LTEX. Scalar rows preserve that set unchanged.
pub(super) fn write_unresolved_texture_ids(
    tx: &Transaction<'_>,
    form_id: u32,
    ids: &BTreeSet<u32>,
) -> rusqlite::Result<()> {
    let mut insert = tx.prepare_cached(
        "INSERT INTO inhouse_unresolved_terrain_textures(form_id,texture_form_id) VALUES (?1,?2)",
    )?;
    for id in ids {
        insert.execute(params![form_id, id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Install a retained legacy snapshot and an independent table for replacement checks.
    fn legacy_snapshot(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE inhouse_terrain_layers(
                 form_id INTEGER PRIMARY KEY,cell_id INTEGER NOT NULL,load_order INTEGER NOT NULL,
                 layer_data BLOB NOT NULL,unresolved_texture_ids TEXT NOT NULL);
             INSERT INTO inhouse_terrain_layers VALUES(4369,8738,3,X'716293','[1291,2147483657]');
             CREATE TABLE inhouse_terrain_source_order(form_id INTEGER PRIMARY KEY,source_record_offset INTEGER NOT NULL);
             INSERT INTO inhouse_terrain_source_order VALUES(4369,59873);",
        )
        .unwrap();
    }

    /// Read the sorted scalar membership independently of insertion order.
    fn unresolved_rows(conn: &Connection) -> Vec<(u32, u32)> {
        conn.prepare(
            "SELECT form_id,texture_form_id FROM inhouse_unresolved_terrain_textures ORDER BY form_id,texture_form_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    }

    /// Replacement drops the old JSON column while preserving unrelated terrain provenance.
    #[test]
    fn legacy_json_column_is_replaced_by_empty_scalar_snapshot() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_snapshot(&conn);
        let tx = conn.unchecked_transaction().unwrap();
        replace_snapshot_tables(&tx).unwrap();
        tx.commit().unwrap();
        let columns = conn
            .prepare(
                "SELECT name,type FROM pragma_table_info('inhouse_terrain_layers') ORDER BY cid",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            columns,
            vec![
                ("form_id".into(), "INTEGER".into()),
                ("cell_id".into(), "INTEGER".into()),
                ("load_order".into(), "INTEGER".into()),
                ("layer_data".into(), "BLOB".into()),
            ]
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM inhouse_terrain_layers", [], |row| row
                .get::<_, u32>(0))
                .unwrap(),
            0
        );
        assert!(unresolved_rows(&conn).is_empty());
        assert_eq!(
            conn.query_row(
                "SELECT source_record_offset FROM inhouse_terrain_source_order WHERE form_id=4369",
                [],
                |row| row.get::<_, u64>(0),
            )
            .unwrap(),
            59873
        );
    }

    /// Distinct scalar memberships remain separate for two unequal LAND identities.
    #[test]
    fn unresolved_members_are_distinct_sorted_and_scoped_to_the_land_owner() {
        let conn = Connection::open_in_memory().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        replace_snapshot_tables(&tx).unwrap();
        let first = [0x9200_0b81, 0x1473, 0x9200_0b81]
            .into_iter()
            .collect::<BTreeSet<_>>();
        write_unresolved_texture_ids(&tx, 0x1200_08c3, &first).unwrap();
        write_unresolved_texture_ids(&tx, 0x2300_08d4, &BTreeSet::from([0x1473])).unwrap();
        write_unresolved_texture_ids(&tx, 0x3400_08e5, &BTreeSet::new()).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            unresolved_rows(&conn),
            vec![
                (0x1200_08c3, 0x1473),
                (0x1200_08c3, 0x9200_0b81),
                (0x2300_08d4, 0x1473),
            ]
        );
        let tx = conn.unchecked_transaction().unwrap();
        replace_snapshot_tables(&tx).unwrap();
        write_unresolved_texture_ids(&tx, 0x4500_08f6, &BTreeSet::from([0x7261])).unwrap();
        tx.commit().unwrap();
        assert_eq!(unresolved_rows(&conn), vec![(0x4500_08f6, 0x7261)]);
    }

    /// A dropped transaction restores the complete legacy snapshot rather than partial DDL.
    #[test]
    fn rollback_restores_the_previous_parent_schema_and_rows() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_snapshot(&conn);
        {
            let tx = conn.unchecked_transaction().unwrap();
            replace_snapshot_tables(&tx).unwrap();
            tx.execute(
                "INSERT INTO inhouse_terrain_layers VALUES(?1,?2,?3,?4)",
                params![
                    0x6200_09a7u32,
                    0x7321u32,
                    5u32,
                    [0x19u8, 0x27, 0x38].as_slice()
                ],
            )
            .unwrap();
            write_unresolved_texture_ids(&tx, 0x6200_09a7, &BTreeSet::from([0x9263])).unwrap();
        }
        assert_eq!(
            conn.query_row(
                "SELECT cell_id,load_order,layer_data,unresolved_texture_ids FROM inhouse_terrain_layers WHERE form_id=4369",
                [],
                |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?, row.get::<_, Vec<u8>>(2)?, row.get::<_, String>(3)?)),
            )
            .unwrap(),
            (8738, 3, vec![0x71, 0x62, 0x93], "[1291,2147483657]".into())
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='inhouse_unresolved_terrain_textures'",
                [],
                |row| row.get::<_, u32>(0),
            )
            .unwrap(),
            0
        );
    }
}
