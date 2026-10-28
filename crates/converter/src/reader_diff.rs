//! Bounded differential diagnostic for two world databases. It reports observed differences; it
//! does not verify shared source inputs, establish reader correctness, or prove source-record
//! completeness. Causal field labels require typed provenance that this comparison does not have.
//!
//! Shared table rows are compared by primary key. Canonical `records.data` archives are validated
//! and compared subrecord by subrecord. Other projected columns, including `cells.data`,
//! `references.data`, and `waters.data`, are compared directly.

use crate::esm::types::ArchivedRecordData;
use color_eyre::{Result, eyre::eyre};
use rusqlite::{Connection, OpenFlags, types::Value};
use serde::Serialize;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Tables whose content differs between the readers by design, or that only one reader writes.
/// They are still counted, but kept apart from the divergences.
const BY_DESIGN: &[&str] = &[
    "scripts",
    "script_properties",
    "script_property_values",
    "record_diagnostics",
    "record_overrides",
    "inhouse_plugin_masters",
    "inhouse_schema_addons",
    "inhouse_source_records",
    "inhouse_localized_fields",
    "inhouse_terrain_source_order",
    "inhouse_terrain_layers",
    "inhouse_unresolved_terrain_textures",
];

#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub key: String,
    pub legacy: String,
    pub inhouse: String,
}

#[derive(Debug, Default, Serialize)]
pub struct ColumnDiff {
    pub rows: u64,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Default, Serialize)]
pub struct TableDiff {
    pub by_design: bool,
    pub key: Vec<String>,
    pub legacy_rows: u64,
    pub inhouse_rows: u64,
    pub only_legacy: u64,
    pub only_inhouse: u64,
    pub differing_rows: u64,
    pub only_legacy_samples: Vec<String>,
    pub only_inhouse_samples: Vec<String>,
    /// Columns one database lacks; their values are not compared.
    pub columns_only_legacy: Vec<String>,
    pub columns_only_inhouse: Vec<String>,
    pub columns: BTreeMap<String, ColumnDiff>,
    /// Why the table was not compared row by row, if it was not.
    pub skipped: Option<String>,
}

/// How one subrecord occurrence differs between the readers.
///
/// Legacy causal labels remain in the public enum for report compatibility. The comparator emits
/// only observed presence/length/byte differences unless typed evidence supports a cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldClass {
    /// The legacy record has the occurrence; the in-house record does not.
    OmittedByInhouse,
    /// Only the in-house record has the occurrence.
    OnlyInhouse,
    /// Legacy causal label: a typed FormID was cleared by the in-house reader.
    LinkCleared,
    /// A FormID with the same local ID but a different load-order index: one reader remapped it.
    FormIdRemap,
    /// Both cleared and remapped words in one occurrence.
    LinkClearedAndRemap,
    /// Legacy Windows-1252 bytes, in-house UTF-8 of the same text.
    TextEncoding,
    /// Legacy four-byte string ID, in-house resolved (or empty) text.
    LocalizedString,
    /// Same subrecords in another order.
    Order,
    /// Different lengths, not explained above.
    OtherLength,
    /// Same length, not explained above.
    OtherBytes,
}

/// How one record differs between the readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordClass {
    OnlyLegacy,
    OnlyInhouse,
    /// Both have the record, with different publishing plugin/load-order values.
    DifferentWinner,
    RecordType,
    CellOrWorldspace,
    /// The subrecords differ; see the field classes.
    Fields,
}

#[derive(Debug, Default, Serialize)]
pub struct Bucket {
    pub count: u64,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Default, Serialize)]
pub struct RecordDiff {
    pub legacy_records: u64,
    pub inhouse_records: u64,
    pub identical: u64,
    /// `class` -> record type -> count and samples.
    pub records: BTreeMap<String, BTreeMap<String, Bucket>>,
    /// `class` -> "RTYP SIGN" -> count and samples, one per differing subrecord occurrence.
    pub fields: BTreeMap<String, BTreeMap<String, Bucket>>,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    /// Explicit limits on what this output establishes.
    pub diagnostic_notice: String,
    pub tables: BTreeMap<String, TableDiff>,
    pub tables_only_legacy: Vec<String>,
    pub tables_only_inhouse: Vec<String>,
    pub records: RecordDiff,
}

pub struct Options {
    /// Examples kept per bucket.
    pub samples: usize,
}

const DIAGNOSTIC_NOTICE: &str = "bounded diagnostic only; source/plugin/load-order/schema/language identity is not validated as matching, and cause or source completeness is not established";

impl Default for Options {
    fn default() -> Self {
        Self { samples: 5 }
    }
}

/// Compare two world databases without asserting that their source inputs are equivalent.
pub fn compare(legacy_path: &Path, inhouse_path: &Path, options: &Options) -> Result<Report> {
    let open = |path: &Path| {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| eyre!("{}: {error}", path.display()))
    };
    let legacy = open(legacy_path)?;
    let inhouse = open(inhouse_path)?;
    let legacy_tables = tables(&legacy)?;
    let inhouse_tables = tables(&inhouse)?;
    let mut report = Report {
        diagnostic_notice: DIAGNOSTIC_NOTICE.to_owned(),
        tables_only_legacy: legacy_tables.difference(&inhouse_tables).cloned().collect(),
        tables_only_inhouse: inhouse_tables.difference(&legacy_tables).cloned().collect(),
        ..Default::default()
    };
    for table in legacy_tables.intersection(&inhouse_tables) {
        report.tables.insert(
            table.clone(),
            compare_table(&legacy, &inhouse, table, options)?,
        );
    }
    report.records = compare_records(&legacy, legacy_path, &inhouse, inhouse_path, options)?;
    Ok(report)
}

/// Ordinary tables, without SQLite's own and R-Tree shadow tables.
fn tables(conn: &Connection) -> Result<BTreeSet<String>> {
    let names = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    let shadows: BTreeSet<String> = names
        .iter()
        .filter_map(|name| {
            ["_node", "_parent", "_rowid"]
                .iter()
                .find_map(|suffix| name.strip_suffix(suffix))
                .filter(|base| names.contains(*base))
                .map(|_| name.clone())
        })
        .collect();
    Ok(names.difference(&shadows).cloned().collect())
}

/// Column names in declaration order, and the primary key columns in key order.
fn columns(conn: &Connection, table: &str) -> Result<(Vec<String>, Vec<String>)> {
    let mut statement = conn.prepare("SELECT name, pk FROM pragma_table_info(?1) ORDER BY cid")?;
    let rows = statement
        .query_map([table], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut key: Vec<(i64, String)> = rows
        .iter()
        .filter(|(_, pk)| *pk > 0)
        .map(|(name, pk)| (*pk, name.clone()))
        .collect();
    key.sort();
    Ok((
        rows.into_iter().map(|(name, _)| name).collect(),
        key.into_iter().map(|(_, name)| name).collect(),
    ))
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Integer(_) | Value::Real(_) => 1,
        Value::Text(_) => 2,
        Value::Blob(_) => 3,
    }
}

/// SQLite's own ordering across storage classes, so a merge join over `ORDER BY` stays in step.
fn compare_values(left: &Value, right: &Value) -> Ordering {
    match (left, right) {
        (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
        (Value::Integer(a), Value::Real(b)) => (*a as f64).total_cmp(b),
        (Value::Real(a), Value::Integer(b)) => a.total_cmp(&(*b as f64)),
        (Value::Real(a), Value::Real(b)) => a.total_cmp(b),
        (Value::Text(a), Value::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
        (Value::Blob(a), Value::Blob(b)) => a.cmp(b),
        _ => rank(left).cmp(&rank(right)),
    }
}

fn compare_keys(left: &[Value], right: &[Value]) -> Ordering {
    left.iter()
        .zip(right)
        .map(|(a, b)| compare_values(a, b))
        .find(|ordering| ordering.is_ne())
        .unwrap_or(Ordering::Equal)
}

fn same(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Real(a), Value::Real(b)) => a.to_bits() == b.to_bits(),
        _ => left == right,
    }
}

fn show(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Integer(value) if *value >= 0 && *value <= u32::MAX.into() => {
            format!("{value} (0x{value:08X})")
        }
        Value::Integer(value) => value.to_string(),
        Value::Real(value) => value.to_string(),
        Value::Text(text) => format!("{text:?}"),
        Value::Blob(bytes) => format!(
            "blob[{}] {}",
            bytes.len(),
            hex(&bytes[..bytes.len().min(32)])
        ),
    }
}

fn show_key(values: &[Value]) -> String {
    values.iter().map(show).collect::<Vec<_>>().join(", ")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn compare_table(
    legacy: &Connection,
    inhouse: &Connection,
    table: &str,
    options: &Options,
) -> Result<TableDiff> {
    let (legacy_columns, legacy_key) = columns(legacy, table)?;
    let (inhouse_columns, inhouse_key) = columns(inhouse, table)?;
    let mut diff = TableDiff {
        by_design: BY_DESIGN.contains(&table),
        columns_only_legacy: legacy_columns
            .iter()
            .filter(|name| !inhouse_columns.contains(name))
            .cloned()
            .collect(),
        columns_only_inhouse: inhouse_columns
            .iter()
            .filter(|name| !legacy_columns.contains(name))
            .cloned()
            .collect(),
        ..Default::default()
    };
    let count = |conn: &Connection| -> Result<u64> {
        Ok(conn.query_row(
            &format!("SELECT count(*) FROM {}", quote(table)),
            [],
            |row| row.get::<_, i64>(0),
        )? as u64)
    };
    diff.legacy_rows = count(legacy)?;
    diff.inhouse_rows = count(inhouse)?;
    // R-Tree tables declare no primary key; their first column is the row id.
    let key = if !legacy_key.is_empty() {
        legacy_key
    } else if table.ends_with("_spatial") {
        vec![legacy_columns[0].clone()]
    } else {
        diff.skipped = Some("no primary key".into());
        return Ok(diff);
    };
    if key != inhouse_key && !(inhouse_key.is_empty() && table.ends_with("_spatial")) {
        diff.skipped = Some(format!("primary keys differ: {inhouse_key:?}"));
        return Ok(diff);
    }
    diff.key.clone_from(&key);
    let compared: Vec<String> = legacy_columns
        .iter()
        .filter(|name| inhouse_columns.contains(name) && !key.contains(name))
        .filter(|name| !(table == "records" && name.as_str() == "data"))
        .cloned()
        .collect();
    let select = key
        .iter()
        .chain(&compared)
        .map(|name| quote(name))
        .collect::<Vec<_>>()
        .join(", ");
    let order = key
        .iter()
        .map(|name| quote(name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT {select} FROM {} ORDER BY {order}", quote(table));
    let width = key.len() + compared.len();
    let read = |row: &rusqlite::Row<'_>| {
        (0..width)
            .map(|index| row.get::<_, Value>(index))
            .collect::<rusqlite::Result<Vec<_>>>()
    };
    let mut legacy_statement = legacy.prepare(&sql)?;
    let mut inhouse_statement = inhouse.prepare(&sql)?;
    let mut legacy_rows = legacy_statement.query_map([], read)?;
    let mut inhouse_rows = inhouse_statement.query_map([], read)?;
    let mut left = legacy_rows.next().transpose()?;
    let mut right = inhouse_rows.next().transpose()?;
    loop {
        match (&left, &right) {
            (None, None) => break,
            (Some(row), None) => {
                diff.only_legacy += 1;
                push(&mut diff.only_legacy_samples, options, || {
                    show_key(&row[..key.len()])
                });
                left = legacy_rows.next().transpose()?;
            }
            (None, Some(row)) => {
                diff.only_inhouse += 1;
                push(&mut diff.only_inhouse_samples, options, || {
                    show_key(&row[..key.len()])
                });
                right = inhouse_rows.next().transpose()?;
            }
            (Some(a), Some(b)) => match compare_keys(&a[..key.len()], &b[..key.len()]) {
                Ordering::Less => {
                    diff.only_legacy += 1;
                    push(&mut diff.only_legacy_samples, options, || {
                        show_key(&a[..key.len()])
                    });
                    left = legacy_rows.next().transpose()?;
                }
                Ordering::Greater => {
                    diff.only_inhouse += 1;
                    push(&mut diff.only_inhouse_samples, options, || {
                        show_key(&b[..key.len()])
                    });
                    right = inhouse_rows.next().transpose()?;
                }
                Ordering::Equal => {
                    let mut differs = false;
                    for (index, name) in compared.iter().enumerate() {
                        let (x, y) = (&a[key.len() + index], &b[key.len() + index]);
                        if !same(x, y) {
                            differs = true;
                            let column = diff.columns.entry(name.clone()).or_default();
                            column.rows += 1;
                            if column.samples.len() < options.samples {
                                column.samples.push(Sample {
                                    key: show_key(&a[..key.len()]),
                                    legacy: show(x),
                                    inhouse: show(y),
                                });
                            }
                        }
                    }
                    diff.differing_rows += u64::from(differs);
                    left = legacy_rows.next().transpose()?;
                    right = inhouse_rows.next().transpose()?;
                }
            },
        }
    }
    Ok(diff)
}

fn push(samples: &mut Vec<String>, options: &Options, sample: impl FnOnce() -> String) {
    if samples.len() < options.samples {
        samples.push(sample());
    }
}

/// One canonical record row.
struct RecordRow {
    form_id: u32,
    record_type: String,
    cell_id: Option<u32>,
    worldspace_id: Option<u32>,
    load_order: u32,
    subrecords: Vec<([u8; 4], Vec<u8>)>,
}

fn record_rows(conn: &Connection) -> Result<rusqlite::Statement<'_>> {
    Ok(conn.prepare(
        "SELECT form_id, record_type, cell_id, worldspace_id, load_order, data FROM records ORDER BY form_id",
    )?)
}

fn decode_row(row: &rusqlite::Row<'_>, path: &Path) -> rusqlite::Result<RecordRow> {
    let form_id: u32 = row.get(0)?;
    let data: Vec<u8> = row.get(5)?;
    let subrecords = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&data)
        .map(|record| {
            record
                .subrecords
                .into_iter()
                .map(|sub| (sub.tag, sub.data))
                .collect()
        })
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Blob,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "{}: record {form_id:08X} has invalid records.data archive: {error}",
                        path.display()
                    ),
                )),
            )
        })?;
    Ok(RecordRow {
        form_id,
        record_type: row.get(1)?,
        cell_id: row.get(2)?,
        worldspace_id: row.get(3)?,
        load_order: row.get(4)?,
        subrecords,
    })
}

fn bucket(
    map: &mut BTreeMap<String, BTreeMap<String, Bucket>>,
    class: impl Serialize,
    group: String,
) -> &mut Bucket {
    let class = serde_json::to_value(class)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    map.entry(class).or_default().entry(group).or_default()
}

fn compare_records(
    legacy: &Connection,
    legacy_path: &Path,
    inhouse: &Connection,
    inhouse_path: &Path,
    options: &Options,
) -> Result<RecordDiff> {
    let mut diff = RecordDiff::default();
    let mut legacy_statement = record_rows(legacy)?;
    let mut inhouse_statement = record_rows(inhouse)?;
    let mut legacy_rows = legacy_statement.query_map([], |row| decode_row(row, legacy_path))?;
    let mut inhouse_rows = inhouse_statement.query_map([], |row| decode_row(row, inhouse_path))?;
    let mut left = legacy_rows.next().transpose()?;
    let mut right = inhouse_rows.next().transpose()?;
    let record = |diff: &mut RecordDiff,
                  class: RecordClass,
                  row: &RecordRow,
                  legacy: String,
                  inhouse: String| {
        let bucket = bucket(&mut diff.records, class, row.record_type.clone());
        bucket.count += 1;
        if bucket.samples.len() < options.samples {
            bucket.samples.push(Sample {
                key: format!("{:08X}", row.form_id),
                legacy,
                inhouse,
            });
        }
    };
    loop {
        match (&left, &right) {
            (None, None) => break,
            (Some(a), b) if b.as_ref().is_none_or(|b| a.form_id < b.form_id) => {
                diff.legacy_records += 1;
                record(
                    &mut diff,
                    RecordClass::OnlyLegacy,
                    a,
                    format!("plugin {}", a.load_order),
                    "-".into(),
                );
                left = legacy_rows.next().transpose()?;
            }
            (_, Some(b)) if left.as_ref().is_none_or(|a| b.form_id < a.form_id) => {
                diff.inhouse_records += 1;
                record(
                    &mut diff,
                    RecordClass::OnlyInhouse,
                    b,
                    "-".into(),
                    format!("plugin {}", b.load_order),
                );
                right = inhouse_rows.next().transpose()?;
            }
            (Some(a), Some(b)) => {
                diff.legacy_records += 1;
                diff.inhouse_records += 1;
                if a.record_type != b.record_type {
                    record(
                        &mut diff,
                        RecordClass::RecordType,
                        a,
                        a.record_type.clone(),
                        b.record_type.clone(),
                    );
                } else if a.load_order != b.load_order {
                    record(
                        &mut diff,
                        RecordClass::DifferentWinner,
                        a,
                        format!("plugin {}", a.load_order),
                        format!("plugin {}", b.load_order),
                    );
                } else if (a.cell_id, a.worldspace_id) != (b.cell_id, b.worldspace_id) {
                    record(
                        &mut diff,
                        RecordClass::CellOrWorldspace,
                        a,
                        format!("{:?}/{:?}", a.cell_id, a.worldspace_id),
                        format!("{:?}/{:?}", b.cell_id, b.worldspace_id),
                    );
                } else if a.subrecords == b.subrecords {
                    diff.identical += 1;
                } else {
                    record(
                        &mut diff,
                        RecordClass::Fields,
                        a,
                        String::new(),
                        String::new(),
                    );
                    for (class, signature, legacy_bytes, inhouse_bytes) in
                        classify_fields(&a.subrecords, &b.subrecords)
                    {
                        let bucket = bucket(
                            &mut diff.fields,
                            class,
                            format!("{} {}", a.record_type, String::from_utf8_lossy(&signature)),
                        );
                        bucket.count += 1;
                        if bucket.samples.len() < options.samples {
                            bucket.samples.push(Sample {
                                key: format!("{:08X}", a.form_id),
                                legacy: legacy_bytes.map_or("-".into(), |bytes| show_field(&bytes)),
                                inhouse: inhouse_bytes
                                    .map_or("-".into(), |bytes| show_field(&bytes)),
                            });
                        }
                    }
                }
                left = legacy_rows.next().transpose()?;
                right = inhouse_rows.next().transpose()?;
            }
            (Some(_), None) | (None, Some(_)) => unreachable!("handled by the guarded arms"),
        }
    }
    Ok(diff)
}

fn show_field(bytes: &[u8]) -> String {
    let text = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if !text.is_empty() && text.iter().all(|byte| (0x20..0x7f).contains(byte)) {
        format!("{:?}", String::from_utf8_lossy(text))
    } else {
        format!("[{}] {}", bytes.len(), hex(&bytes[..bytes.len().min(40)]))
    }
}

type FieldDifference = (FieldClass, [u8; 4], Option<Vec<u8>>, Option<Vec<u8>>);

/// Pair the occurrences of each signature in order and classify every difference.
/// Compare untyped field bytes. Causal classes are not inferred without field schema context.
pub fn classify_fields(
    legacy: &[([u8; 4], Vec<u8>)],
    inhouse: &[([u8; 4], Vec<u8>)],
) -> Vec<FieldDifference> {
    let group = |fields: &[([u8; 4], Vec<u8>)]| {
        let mut groups: BTreeMap<[u8; 4], Vec<Vec<u8>>> = BTreeMap::new();
        for (tag, data) in fields {
            groups.entry(*tag).or_default().push(data.clone());
        }
        groups
    };
    let legacy_groups = group(legacy);
    let inhouse_groups = group(inhouse);
    let mut differences = Vec::new();
    let signatures: BTreeSet<[u8; 4]> = legacy_groups
        .keys()
        .chain(inhouse_groups.keys())
        .copied()
        .collect();
    for signature in signatures {
        let empty = Vec::new();
        let left = legacy_groups.get(&signature).unwrap_or(&empty);
        let right = inhouse_groups.get(&signature).unwrap_or(&empty);
        for index in 0..left.len().max(right.len()) {
            match (left.get(index), right.get(index)) {
                (Some(a), Some(b)) if a == b => {}
                (Some(a), Some(b)) => {
                    differences.push((
                        classify_pair(a, b),
                        signature,
                        Some(a.clone()),
                        Some(b.clone()),
                    ));
                }
                (Some(a), None) => differences.push((
                    FieldClass::OmittedByInhouse,
                    signature,
                    Some(a.clone()),
                    None,
                )),
                (None, Some(b)) => {
                    differences.push((FieldClass::OnlyInhouse, signature, None, Some(b.clone())))
                }
                (None, None) => {}
            }
        }
    }
    if differences.is_empty() && legacy != inhouse {
        differences.push((FieldClass::Order, *b"----", None, None));
    }
    differences
}

fn classify_pair(legacy: &[u8], inhouse: &[u8]) -> FieldClass {
    if legacy.len() != inhouse.len() {
        FieldClass::OtherLength
    } else {
        // This layer has no field schema or localization provenance. Equal-length values stay
        // unexplained; byte shape alone cannot establish string, link, or FormID semantics.
        FieldClass::OtherBytes
    }
}

/// A plain-text summary: every divergent table and field class, largest first.
pub fn summary(report: &Report) -> String {
    let mut out = format!("{}\n", report.diagnostic_notice);
    let records = &report.records;
    out += &format!(
        "records: legacy {}, in-house {}, identical {}\n",
        records.legacy_records, records.inhouse_records, records.identical
    );
    for (title, map) in [
        ("record classes", &records.records),
        ("field classes", &records.fields),
    ] {
        out += &format!("{title}:\n");
        let mut rows: Vec<(u64, &String, &String)> = map
            .iter()
            .flat_map(|(class, groups)| {
                groups
                    .iter()
                    .map(move |(group, bucket)| (bucket.count, class, group))
            })
            .collect();
        rows.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.cmp(b.1))
                .then_with(|| a.2.cmp(b.2))
        });
        for (count, class, group) in rows {
            out += &format!("  {count:>9}  {class:<24} {group}\n");
        }
    }
    out += "tables:\n";
    for (name, table) in &report.tables {
        let changed = table.only_legacy + table.only_inhouse + table.differing_rows > 0
            || table.skipped.is_some()
            || !table.columns_only_legacy.is_empty()
            || !table.columns_only_inhouse.is_empty();
        if !changed {
            continue;
        }
        out += &format!(
            "  {name}{}: rows {}/{}, only legacy {}, only in-house {}, differing {}{}\n",
            if table.by_design { " (by design)" } else { "" },
            table.legacy_rows,
            table.inhouse_rows,
            table.only_legacy,
            table.only_inhouse,
            table.differing_rows,
            table
                .skipped
                .as_ref()
                .map_or(String::new(), |why| format!(", skipped: {why}")),
        );
        for (column, diff) in &table.columns {
            out += &format!("      {column}: {} rows\n", diff.rows);
        }
    }
    for (side, names) in [
        ("only legacy", &report.tables_only_legacy),
        ("only in-house", &report.tables_only_inhouse),
    ] {
        if !names.is_empty() {
            out += &format!("tables {side}: {}\n", names.join(", "));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn fields(list: &[(&[u8; 4], &[u8])]) -> Vec<([u8; 4], Vec<u8>)> {
        list.iter()
            .map(|(tag, data)| (**tag, data.to_vec()))
            .collect()
    }

    fn minimal_database(path: &Path, record_data: &[u8], projection: Option<(&str, &[u8])>) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE records(
                    form_id INTEGER PRIMARY KEY, record_type TEXT, cell_id INTEGER,
                    worldspace_id INTEGER, load_order INTEGER, data BLOB
                );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO records VALUES(4660, 'STAT', NULL, NULL, 0, ?1)",
                [record_data],
            )
            .unwrap();
        for table in ["cells", "references", "waters"] {
            connection
                .execute_batch(&format!(
                    "CREATE TABLE \"{table}\"(id INTEGER PRIMARY KEY, data BLOB);"
                ))
                .unwrap();
            let payload = projection
                .filter(|(changed, _)| *changed == table)
                .map_or(&b"same"[..], |(_, value)| value);
            connection
                .execute(
                    &format!("INSERT INTO \"{table}\"(id,data) VALUES(1,?1)"),
                    params![payload],
                )
                .unwrap();
        }
    }

    fn empty_record_archive() -> Vec<u8> {
        rkyv::to_bytes::<rkyv::rancor::Error>(&ArchivedRecordData {
            subrecords: Vec::new(),
        })
        .unwrap()
        .to_vec()
    }

    #[test]
    fn projected_payload_blobs_are_compared_independently_of_canonical_records() {
        let canonical = empty_record_archive();
        for table in ["cells", "references", "waters"] {
            let directory = tempfile::tempdir().unwrap();
            let legacy_path = directory.path().join("legacy.db");
            let inhouse_path = directory.path().join("inhouse.db");
            minimal_database(&legacy_path, &canonical, Some((table, b"left")));
            minimal_database(&inhouse_path, &canonical, Some((table, b"right")));

            let report = compare(&legacy_path, &inhouse_path, &Options::default()).unwrap();
            assert_eq!(report.records.identical, 1, "{table}");
            assert_eq!(report.records.records.len(), 0, "{table}");
            assert_eq!(report.tables[table].differing_rows, 1, "{table}");
            assert_eq!(report.tables[table].columns["data"].rows, 1, "{table}");
            for unchanged in ["cells", "references", "waters"] {
                if unchanged != table {
                    assert_eq!(report.tables[unchanged].differing_rows, 0, "{unchanged}");
                }
            }
        }
    }

    #[test]
    fn corrupt_record_archives_fail_with_path_and_form_id_context() {
        let valid = empty_record_archive();
        let cases = [
            (b"corrupt".as_slice(), &valid[..]),
            (&valid[..], b"corrupt"),
        ];
        for (legacy_data, inhouse_data) in cases {
            let directory = tempfile::tempdir().unwrap();
            let legacy_path = directory.path().join("legacy.db");
            let inhouse_path = directory.path().join("inhouse.db");
            minimal_database(&legacy_path, legacy_data, None);
            minimal_database(&inhouse_path, inhouse_data, None);
            let error = compare(&legacy_path, &inhouse_path, &Options::default()).unwrap_err();
            let detail = format!("{error:#}");
            let bad_path = if legacy_data == b"corrupt" {
                &legacy_path
            } else {
                &inhouse_path
            };
            assert!(detail.contains(&bad_path.display().to_string()), "{detail}");
            assert!(detail.contains("record 00001234"), "{detail}");
            assert!(detail.contains("invalid records.data archive"), "{detail}");
        }

        let directory = tempfile::tempdir().unwrap();
        let legacy_path = directory.path().join("both-corrupt-legacy.db");
        let inhouse_path = directory.path().join("both-corrupt-inhouse.db");
        minimal_database(&legacy_path, b"corrupt", None);
        minimal_database(&inhouse_path, b"corrupt", None);
        assert!(compare(&legacy_path, &inhouse_path, &Options::default()).is_err());
    }

    #[test]
    fn untyped_field_shapes_do_not_claim_causal_mechanisms() {
        let legacy = fields(&[
            (b"LSTR", &7u32.to_le_bytes()),
            (b"ZERO", &[b'A', b'B', b'C', 0]),
            (b"LINK", &0x0100_0800u32.to_le_bytes()),
            (b"NULL", &0xDEADu32.to_le_bytes()),
            (b"FLOT", &1.0f32.to_le_bytes()),
            (b"RAND", &[0x91, 0x25, 0xF0, 0x33]),
            (b"TEXT", b"Caf\xe9\0"),
        ]);
        let inhouse = fields(&[
            (b"LSTR", b"Rock\0"),
            (b"ZERO", b"XYZ\0"),
            (b"LINK", &0x0200_0800u32.to_le_bytes()),
            (b"NULL", &0u32.to_le_bytes()),
            (b"FLOT", &0.0f32.to_le_bytes()),
            (b"RAND", &[0x81, 0x25, 0xF0, 0x33]),
            (b"TEXT", "Café\0".as_bytes()),
        ]);
        let classes: BTreeMap<String, FieldClass> = classify_fields(&legacy, &inhouse)
            .into_iter()
            .map(|(class, tag, _, _)| (String::from_utf8_lossy(&tag).into_owned(), class))
            .collect();
        for tag in ["ZERO", "LINK", "NULL", "FLOT", "RAND"] {
            assert_eq!(classes[tag], FieldClass::OtherBytes, "{tag}");
        }
        assert_eq!(classes["LSTR"], FieldClass::OtherLength);
        assert_eq!(classes["TEXT"], FieldClass::OtherLength);
    }

    #[test]
    fn reordered_fields_are_an_order_difference() {
        let a = fields(&[(b"EDID", b"A\0"), (b"MODL", b"m.nif\0")]);
        let b = fields(&[(b"MODL", b"m.nif\0"), (b"EDID", b"A\0")]);
        let classes: Vec<_> = classify_fields(&a, &b)
            .into_iter()
            .map(|(class, ..)| class)
            .collect();
        assert_eq!(classes, vec![FieldClass::Order]);
    }
}
