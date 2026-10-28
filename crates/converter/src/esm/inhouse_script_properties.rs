//! Scalar script-property projections for complete in-house database snapshots.
//!
//! Parsing, link remapping and malformed VMAD recovery remain with the caller.
//! The existing exact-name script key and its last-occurrence replacement policy
//! are retained. Native script order/status remain available in the unchanged VMAD.
use super::records::record_type::vmad::{ScriptEntry, ScriptPropertyValue};
use rusqlite::{Result, Statement, Transaction, params};

/// One typed scalar value while streaming a property or one of its array elements.
#[derive(Default)]
struct ScalarValue<'a> {
    object_form_id: Option<u32>,
    alias_id: Option<u16>,
    string_value: Option<&'a str>,
    integer_value: Option<i32>,
    float_bits: Option<u32>,
    boolean_value: Option<bool>,
}

/// Rebuild only the script projection tables in the caller's complete-snapshot transaction.
/// A rollback restores the old tables and rows, including a retained legacy JSON layout.
pub(crate) fn replace_snapshot_tables(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS script_property_values;
        DROP TABLE IF EXISTS script_properties;
        DROP TABLE IF EXISTS scripts;
        CREATE TABLE scripts(
            form_id INTEGER NOT NULL,script_name TEXT NOT NULL,vmad BLOB NOT NULL,
            PRIMARY KEY(form_id,script_name));
        CREATE TABLE script_properties(
            form_id INTEGER NOT NULL,script_name TEXT NOT NULL,
            property_index INTEGER NOT NULL,name TEXT NOT NULL,
            property_type INTEGER NOT NULL,status INTEGER NOT NULL,value_count INTEGER NOT NULL,
            PRIMARY KEY(form_id,script_name,property_index)) WITHOUT ROWID;
        CREATE TABLE script_property_values(
            form_id INTEGER NOT NULL,script_name TEXT NOT NULL,property_index INTEGER NOT NULL,
            value_index INTEGER NOT NULL,object_form_id INTEGER,alias_id INTEGER,
            string_value TEXT,integer_value INTEGER,float_bits INTEGER,boolean_value INTEGER,
            PRIMARY KEY(form_id,script_name,property_index,value_index)) WITHOUT ROWID;",
    )
}

/// Replace one exact-name script and its children without changing neighboring owners/scripts.
/// Type zero and empty arrays keep property headers without value rows; scalars have one row.
pub(crate) fn write_script(
    tx: &Transaction<'_>,
    form_id: u32,
    script: &ScriptEntry,
    vmad: &[u8],
) -> Result<()> {
    tx.prepare_cached("DELETE FROM script_property_values WHERE form_id=?1 AND script_name=?2")?
        .execute(params![form_id, script.name])?;
    tx.prepare_cached("DELETE FROM script_properties WHERE form_id=?1 AND script_name=?2")?
        .execute(params![form_id, script.name])?;
    tx.prepare_cached("INSERT OR REPLACE INTO scripts(form_id,script_name,vmad) VALUES(?1,?2,?3)")?
        .execute(params![form_id, script.name, vmad])?;
    let mut properties =
        tx.prepare_cached("INSERT INTO script_properties VALUES(?1,?2,?3,?4,?5,?6,?7)")?;
    let mut values = tx.prepare_cached(
        "INSERT INTO script_property_values VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?;
    for (property_index, property) in script.properties.iter().enumerate() {
        let count = match &property.value {
            ScriptPropertyValue::None => 0,
            ScriptPropertyValue::ArrayOfObject(values) => values.len(),
            ScriptPropertyValue::ArrayOfWString(values) => values.len(),
            ScriptPropertyValue::ArrayOfInt(values) => values.len(),
            ScriptPropertyValue::ArrayOfFloat(values) => values.len(),
            ScriptPropertyValue::ArrayOfBool(values) => values.len(),
            _ => 1,
        };
        // Native property ordinals/counts are u16/u32, respectively, and fit SQLite i64.
        let property_index = property_index as i64;
        properties.execute(params![
            form_id,
            script.name,
            property_index,
            property.name,
            property.property_type,
            property.status,
            count as i64
        ])?;
        match &property.value {
            ScriptPropertyValue::None => {}
            ScriptPropertyValue::Object(value) => insert_value(
                &mut values,
                form_id,
                &script.name,
                property_index,
                0,
                ScalarValue {
                    object_form_id: Some(value.form_id),
                    alias_id: Some(value.alias_id),
                    ..Default::default()
                },
            )?,
            ScriptPropertyValue::WString(value) => insert_value(
                &mut values,
                form_id,
                &script.name,
                property_index,
                0,
                ScalarValue {
                    string_value: Some(value),
                    ..Default::default()
                },
            )?,
            ScriptPropertyValue::Int(value) => insert_value(
                &mut values,
                form_id,
                &script.name,
                property_index,
                0,
                ScalarValue {
                    integer_value: Some(*value),
                    ..Default::default()
                },
            )?,
            ScriptPropertyValue::Float(value) => insert_value(
                &mut values,
                form_id,
                &script.name,
                property_index,
                0,
                ScalarValue {
                    float_bits: Some(value.to_bits()),
                    ..Default::default()
                },
            )?,
            ScriptPropertyValue::Bool(value) => insert_value(
                &mut values,
                form_id,
                &script.name,
                property_index,
                0,
                ScalarValue {
                    boolean_value: Some(*value),
                    ..Default::default()
                },
            )?,
            ScriptPropertyValue::ArrayOfObject(items) => {
                for (index, value) in items.iter().enumerate() {
                    insert_value(
                        &mut values,
                        form_id,
                        &script.name,
                        property_index,
                        index as i64,
                        ScalarValue {
                            object_form_id: Some(value.form_id),
                            alias_id: Some(value.alias_id),
                            ..Default::default()
                        },
                    )?;
                }
            }
            ScriptPropertyValue::ArrayOfWString(items) => {
                for (index, value) in items.iter().enumerate() {
                    insert_value(
                        &mut values,
                        form_id,
                        &script.name,
                        property_index,
                        index as i64,
                        ScalarValue {
                            string_value: Some(value),
                            ..Default::default()
                        },
                    )?;
                }
            }
            ScriptPropertyValue::ArrayOfInt(items) => {
                for (index, value) in items.iter().enumerate() {
                    insert_value(
                        &mut values,
                        form_id,
                        &script.name,
                        property_index,
                        index as i64,
                        ScalarValue {
                            integer_value: Some(*value),
                            ..Default::default()
                        },
                    )?;
                }
            }
            ScriptPropertyValue::ArrayOfFloat(items) => {
                for (index, value) in items.iter().enumerate() {
                    insert_value(
                        &mut values,
                        form_id,
                        &script.name,
                        property_index,
                        index as i64,
                        ScalarValue {
                            float_bits: Some(value.to_bits()),
                            ..Default::default()
                        },
                    )?;
                }
            }
            ScriptPropertyValue::ArrayOfBool(items) => {
                for (index, value) in items.iter().enumerate() {
                    insert_value(
                        &mut values,
                        form_id,
                        &script.name,
                        property_index,
                        index as i64,
                        ScalarValue {
                            boolean_value: Some(*value),
                            ..Default::default()
                        },
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Insert a scalar typed cell, retaining f32 bits without an SQLite REAL round trip.
fn insert_value(
    statement: &mut Statement<'_>,
    form_id: u32,
    script_name: &str,
    property_index: i64,
    value_index: i64,
    value: ScalarValue<'_>,
) -> Result<()> {
    statement.execute(params![
        form_id,
        script_name,
        property_index,
        value_index,
        value.object_form_id,
        value.alias_id,
        value.string_value,
        value.integer_value,
        value.float_bits,
        value.boolean_value
    ])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm::records::record_type::vmad::{ScriptObjectProperty, ScriptProperty};
    use rusqlite::Connection;

    /// Give each fixture property a distinct status while preserving its authored ordinal.
    fn script(name: &str, values: Vec<(u8, ScriptPropertyValue)>) -> ScriptEntry {
        ScriptEntry {
            name: name.into(),
            status: 37,
            properties: values
                .into_iter()
                .enumerate()
                .map(|(index, (property_type, value))| ScriptProperty {
                    name: format!("property-{index}"),
                    property_type,
                    status: (index + 2) as u8,
                    value,
                })
                .collect(),
        }
    }

    /// Every currently parsed VMAD value variant keeps its type, value and array order.
    #[test]
    fn every_typed_variant_and_float_bit_pattern_is_scalar() {
        let conn = Connection::open_in_memory().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        replace_snapshot_tables(&tx).unwrap();
        let entry = script(
            "Typed",
            vec![
                (0, ScriptPropertyValue::None),
                (
                    1,
                    ScriptPropertyValue::Object(ScriptObjectProperty {
                        form_id: 0xfe002abc,
                        alias_id: 17,
                    }),
                ),
                (2, ScriptPropertyValue::WString(String::new())),
                (3, ScriptPropertyValue::Int(-7391)),
                (4, ScriptPropertyValue::Float(f32::from_bits(0x80000000))),
                (5, ScriptPropertyValue::Bool(true)),
                (
                    11,
                    ScriptPropertyValue::ArrayOfObject(vec![
                        ScriptObjectProperty {
                            form_id: 0x07000123,
                            alias_id: 5,
                        },
                        ScriptObjectProperty {
                            form_id: 0,
                            alias_id: u16::MAX,
                        },
                    ]),
                ),
                (
                    12,
                    ScriptPropertyValue::ArrayOfWString(vec!["".into(), "omega".into()]),
                ),
                (13, ScriptPropertyValue::ArrayOfInt(vec![-7, 42])),
                (
                    14,
                    ScriptPropertyValue::ArrayOfFloat(vec![
                        f32::from_bits(0x7fc12345),
                        f32::INFINITY,
                        -0.0,
                    ]),
                ),
                (15, ScriptPropertyValue::ArrayOfBool(vec![false, true])),
            ],
        );
        write_script(&tx, 0x07000021, &entry, b"unchanged-native-vmad").unwrap();
        let headers = tx.prepare("SELECT property_index,property_type,status,value_count FROM script_properties ORDER BY property_index").unwrap()
            .query_map([], |r| Ok((r.get::<_,u32>(0)?,r.get::<_,u8>(1)?,r.get::<_,u8>(2)?,r.get::<_,u32>(3)?))).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(
            headers,
            [
                (0, 0, 2, 0),
                (1, 1, 3, 1),
                (2, 2, 4, 1),
                (3, 3, 5, 1),
                (4, 4, 6, 1),
                (5, 5, 7, 1),
                (6, 11, 8, 2),
                (7, 12, 9, 2),
                (8, 13, 10, 2),
                (9, 14, 11, 3),
                (10, 15, 12, 2)
            ]
        );
        let scalar_object: (u32, u16) = tx
            .query_row(
                "SELECT object_form_id,alias_id FROM script_property_values WHERE property_index=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(scalar_object, (0xfe002abc, 17));
        let scalar_text: String = tx
            .query_row(
                "SELECT string_value FROM script_property_values WHERE property_index=2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scalar_text, "");
        let scalar_integer: i32 = tx
            .query_row(
                "SELECT integer_value FROM script_property_values WHERE property_index=3",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scalar_integer, -7391);
        let float_bits: Vec<u32> = tx.prepare("SELECT float_bits FROM script_property_values WHERE float_bits IS NOT NULL ORDER BY property_index,value_index").unwrap()
            .query_map([], |r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(float_bits, [0x80000000, 0x7fc12345, 0x7f800000, 0x80000000]);
        let objects = tx.prepare("SELECT object_form_id,alias_id FROM script_property_values WHERE property_index=6 ORDER BY value_index").unwrap()
            .query_map([], |r| Ok((r.get::<_,u32>(0)?,r.get::<_,u16>(1)?))).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(objects, [(0x07000123, 5), (0, u16::MAX)]);
        let strings: Vec<String> = tx.prepare("SELECT string_value FROM script_property_values WHERE property_index=7 ORDER BY value_index").unwrap()
            .query_map([], |r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(strings, ["", "omega"]);
        let integers: Vec<i32> = tx.prepare("SELECT integer_value FROM script_property_values WHERE property_index=8 ORDER BY value_index").unwrap()
            .query_map([], |r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(integers, [-7, 42]);
        let booleans: Vec<bool> = tx.prepare("SELECT boolean_value FROM script_property_values WHERE boolean_value IS NOT NULL ORDER BY property_index,value_index").unwrap()
            .query_map([], |r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(booleans, [true, false, true]);
        let vmad: Vec<u8> = tx
            .query_row("SELECT vmad FROM scripts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(vmad, b"unchanged-native-vmad");
        let json_columns: i64 = tx
            .query_row(
                "SELECT count(*) FROM pragma_table_info('scripts') WHERE name='properties_json'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(json_columns, 0);
    }

    /// Empty arrays differ from no-value type zero through their retained property type.
    #[test]
    fn no_value_and_empty_arrays_keep_headers_and_empty_scripts_survive() {
        let conn = Connection::open_in_memory().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        replace_snapshot_tables(&tx).unwrap();
        let entry = script(
            "EmptyValues",
            vec![
                (0, ScriptPropertyValue::None),
                (11, ScriptPropertyValue::ArrayOfObject(vec![])),
                (12, ScriptPropertyValue::ArrayOfWString(vec![])),
                (13, ScriptPropertyValue::ArrayOfInt(vec![])),
                (14, ScriptPropertyValue::ArrayOfFloat(vec![])),
                (15, ScriptPropertyValue::ArrayOfBool(vec![])),
            ],
        );
        write_script(&tx, 17, &entry, b"empty-values").unwrap();
        write_script(&tx, 17, &script("NoProperties", vec![]), b"no-properties").unwrap();
        let headers: (i64, i64) = tx
            .query_row(
                "SELECT count(*),sum(value_count) FROM script_properties",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let values: i64 = tx
            .query_row("SELECT count(*) FROM script_property_values", [], |r| {
                r.get(0)
            })
            .unwrap();
        let scripts: i64 = tx
            .query_row("SELECT count(*) FROM scripts", [], |r| r.get(0))
            .unwrap();
        assert_eq!((headers, values, scripts), ((6, 0), 0, 2));
    }

    /// Repeated script names replace their whole projection; property-name duplicates stay ordered.
    #[test]
    fn exact_name_overwrite_clears_only_matching_children() {
        let conn = Connection::open_in_memory().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        replace_snapshot_tables(&tx).unwrap();
        let previous = script(
            "Same",
            vec![
                (13, ScriptPropertyValue::ArrayOfInt(vec![1, 2, 3])),
                (3, ScriptPropertyValue::Int(8)),
            ],
        );
        write_script(&tx, 19, &previous, b"old").unwrap();
        write_script(
            &tx,
            19,
            &script("same", vec![(3, ScriptPropertyValue::Int(88))]),
            b"case-neighbor",
        )
        .unwrap();
        write_script(&tx, 20, &previous, b"owner-neighbor").unwrap();
        let mut replacement = script(
            "Same",
            vec![
                (3, ScriptPropertyValue::Int(-31)),
                (3, ScriptPropertyValue::Int(57)),
            ],
        );
        replacement.properties[0].name = "Duplicated".into();
        replacement.properties[1].name = "Duplicated".into();
        write_script(&tx, 19, &replacement, b"new").unwrap();
        let values: Vec<i32> = tx.prepare("SELECT integer_value FROM script_property_values WHERE form_id=19 AND script_name='Same' ORDER BY property_index,value_index").unwrap()
            .query_map([], |r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(values, [-31, 57]);
        let names: Vec<String> = tx.prepare("SELECT name FROM script_properties WHERE form_id=19 AND script_name='Same' ORDER BY property_index").unwrap()
            .query_map([], |r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(names, ["Duplicated", "Duplicated"]);
        let neighbor: i32 = tx.query_row("SELECT integer_value FROM script_property_values WHERE form_id=19 AND script_name='same'", [], |r|r.get(0)).unwrap();
        assert_eq!(neighbor, 88);
        let other_owner: i64 = tx
            .query_row(
                "SELECT count(*) FROM script_property_values WHERE form_id=20",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(other_owner, 4);
        let vmad: Vec<u8> = tx
            .query_row(
                "SELECT vmad FROM scripts WHERE form_id=19 AND script_name='Same'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(vmad, b"new");
    }

    /// Schema replacement is rolled back with publication if a later infrastructure step fails.
    #[test]
    fn caller_rollback_restores_legacy_schema_and_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE scripts(form_id INTEGER,script_name TEXT,vmad BLOB,properties_json TEXT);
            INSERT INTO scripts VALUES(41,'Previous',X'1234','[]');",
        )
        .unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        replace_snapshot_tables(&tx).unwrap();
        tx.execute_batch(
            "CREATE TRIGGER fail_script_value BEFORE INSERT ON script_property_values
            BEGIN SELECT RAISE(ABORT,'synthetic publication failure'); END;",
        )
        .unwrap();
        let error = write_script(
            &tx,
            42,
            &script("New", vec![(3, ScriptPropertyValue::Int(19))]),
            b"new",
        )
        .unwrap_err();
        assert!(error.to_string().contains("synthetic publication failure"));
        tx.rollback().unwrap();
        let old: (u32, String, Vec<u8>, String) = conn
            .query_row("SELECT * FROM scripts", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!(old, (41, "Previous".into(), vec![0x12, 0x34], "[]".into()));
        let children: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('script_properties','script_property_values')", [], |r|r.get(0)).unwrap();
        assert_eq!(children, 0);
    }

    /// The public partial exporter follows an existing scalar layout and retains unrelated rows.
    #[test]
    fn partial_public_export_supports_scalar_and_fresh_legacy_layouts() {
        use crate::esm::{exporter, records::RawRecord};
        use std::collections::HashMap;

        // Independently frame VMAD v5/format2 with one script, one int property and no tail.
        let mut vmad = Vec::new();
        vmad.extend(5i16.to_le_bytes());
        vmad.extend(2i16.to_le_bytes());
        vmad.extend(1u16.to_le_bytes());
        vmad.extend(6u16.to_le_bytes());
        vmad.extend(b"Scalar");
        vmad.push(7);
        vmad.extend(1u16.to_le_bytes());
        vmad.extend(3u16.to_le_bytes());
        vmad.extend(b"Int");
        vmad.extend([3, 9]);
        vmad.extend((-731i32).to_le_bytes());
        let records = HashMap::from([(
            0x07000231,
            RawRecord {
                form_id: 0x07000231,
                record_type: *b"KYWD",
                flags: 0,
                subrecords: vec![(b"VMAD".to_vec(), vmad.clone())],
                cell_form_id: None,
                worldspace_form_id: None,
                load_order: 7,
            },
        )]);
        let scalar = Connection::open_in_memory().unwrap();
        exporter::create_tables(&scalar).unwrap();
        let tx = scalar.unchecked_transaction().unwrap();
        replace_snapshot_tables(&tx).unwrap();
        write_script(
            &tx,
            0x03000819,
            &script("Unrelated", vec![(3, ScriptPropertyValue::Int(93))]),
            b"preserved-vmad",
        )
        .unwrap();
        tx.commit().unwrap();
        exporter::export_to_db(&scalar, &records).unwrap();
        let projected: (u8,u8,i32) = scalar.query_row("SELECT p.property_type,p.status,v.integer_value
            FROM script_properties p JOIN script_property_values v USING(form_id,script_name,property_index)
            WHERE p.form_id=?1 AND p.script_name='Scalar' AND p.property_index=0 AND v.value_index=0",
            [0x07000231u32], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(projected, (3, 9, -731));
        let unrelated: (u32, String, Vec<u8>) = scalar
            .query_row(
                "SELECT form_id,script_name,vmad FROM scripts WHERE form_id=?1",
                [0x03000819u32],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            unrelated,
            (0x03000819, "Unrelated".into(), b"preserved-vmad".to_vec())
        );
        let untouched: i32 = scalar
            .query_row(
                "SELECT integer_value FROM script_property_values WHERE form_id=?1",
                [0x03000819u32],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(untouched, 93);
        let legacy = Connection::open_in_memory().unwrap();
        exporter::create_tables(&legacy).unwrap();
        exporter::export_to_db(&legacy, &records).unwrap();
        let properties: String = legacy
            .query_row(
                "SELECT properties_json FROM scripts WHERE form_id=?1 AND script_name='Scalar'",
                [0x07000231u32],
                |r| r.get(0),
            )
            .unwrap();
        let properties: serde_json::Value = serde_json::from_str(&properties).unwrap();
        assert_eq!(properties[0]["value"]["Int"], -731);
        let scalar_children: i64 = legacy.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('script_properties','script_property_values')", [], |r|r.get(0)).unwrap();
        assert_eq!(scalar_children, 0);
    }
}
