//! Declared record fields and observed source-to-canonical retention (#498).
//! This is evidence about framing and storage, not runtime semantic support.

use super::schema_format::{FieldSchema, Schema};
use crate::{cache::hash_bytes, esm::types::ArchivedRecordData};
use color_eyre::{Result, eyre::ensure};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Serialize)]
pub struct Declaration {
    /// Ordinals distinguish repeated names, signatures and union alternatives.
    pub id: String,
    pub source: String,
    pub contract_sha256: String,
    pub descriptor: FieldSchema,
    pub semantic_support: &'static str,
}

fn walk_declaration(
    schema: &Schema,
    field: &FieldSchema,
    parent: &str,
    ordinal: usize,
    source: &str,
    output: &mut Vec<Declaration>,
) -> Result<()> {
    let field = schema
        .resolve(field)
        .map_err(color_eyre::eyre::Error::msg)?;
    let id = format!(
        "{parent}/{ordinal}:{}:{}",
        field.signature.as_deref().unwrap_or("-"),
        field.name
    );
    let source = if field.source.is_empty() {
        source
    } else {
        &field.source
    };
    let mut descriptor = field.clone();
    // Children have their own rows. Keeping their order in their IDs also records
    // decoder-significant insertion, removal and reordering without quadratic output.
    descriptor.members.clear();
    descriptor.fields.clear();
    output.push(Declaration {
        id: id.clone(),
        source: source.to_owned(),
        contract_sha256: hash_bytes(&serde_json::to_vec(&(source, &descriptor))?),
        descriptor,
        semantic_support: "unverified",
    });
    for (index, child) in field.members.iter().enumerate() {
        walk_declaration(
            schema,
            child,
            &format!("{id}/members"),
            index,
            source,
            output,
        )?;
    }
    for (index, child) in field.fields.iter().enumerate() {
        walk_declaration(
            schema,
            child,
            &format!("{id}/fields"),
            index,
            source,
            output,
        )?;
    }
    Ok(())
}

/// Expand definitions at each use, including containers, alternatives and array members.
/// Standalone definitions and common hooks have separate namespaces and denominators.
pub fn declarations(schema: &Schema) -> Result<Vec<Declaration>> {
    let mut output = Vec::new();
    for record in &schema.records {
        // Ordering is a decoding contract even for a record with no declared fields.
        let id = format!("record/{}", record.signature);
        output.push(Declaration {
            id: id.clone(),
            source: record.source.clone(),
            contract_sha256: hash_bytes(&serde_json::to_vec(&(
                &record.signature,
                &record.source,
                record.allow_unordered,
            ))?),
            descriptor: FieldSchema {
                kind: "record".into(),
                ..Default::default()
            },
            semantic_support: "unverified",
        });
        for (index, field) in record.fields.iter().enumerate() {
            walk_declaration(schema, field, &id, index, &record.source, &mut output)?;
        }
    }
    for (index, field) in schema.common_fields.iter().enumerate() {
        walk_declaration(schema, field, "common", index, "", &mut output)?;
    }
    for (name, field) in &schema.definitions {
        walk_declaration(
            schema,
            field,
            &format!("definition/{name}"),
            0,
            "",
            &mut output,
        )?;
    }
    output.sort_by(|left, right| left.id.cmp(&right.id));
    ensure!(
        output.windows(2).all(|pair| pair[0].id != pair[1].id),
        "duplicate declaration ID"
    );
    Ok(output)
}

/// Extensions have independent matching cursors; never present them as ordered suffix fields.
pub fn extension_declarations(
    schema: &Schema,
    extensions: &BTreeMap<[u8; 4], Vec<FieldSchema>>,
) -> Result<Vec<Declaration>> {
    let mut output = Vec::new();
    for (signature, fields) in extensions {
        let root = format!("extension/{}", std::str::from_utf8(signature)?);
        for (index, field) in fields.iter().enumerate() {
            walk_declaration(schema, field, &root, index, "", &mut output)?;
        }
    }
    output.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(output)
}

/// Enumerate only add-on declarations; built-in usage classifications stay with their owner.
pub fn addon_declarations(
    base: &Schema,
    schemas: &super::addons::RuntimeSchemas,
) -> Result<Vec<Declaration>> {
    let builtins: BTreeSet<_> = declarations(base)?
        .into_iter()
        .map(|field| field.id)
        .collect();
    let mut output: Vec<_> = declarations(schemas.schema())?
        .into_iter()
        .filter(|field| !builtins.contains(&field.id))
        .collect();
    output.extend(extension_declarations(
        schemas.schema(),
        schemas.extensions(),
    )?);
    output.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(output)
}

#[derive(Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct BaselineEntry {
    pub declarations: usize,
    pub sha256: String,
}

/// Compact review denominator. Each root hashes every ordered field path and contract.
/// Full field enumeration is emitted by `record-fidelity schema`, without a multi-megabyte
/// checked-in duplicate of the authored schema. Updating this is not completed support.
pub fn baseline(schema: &Schema) -> Result<BTreeMap<String, BaselineEntry>> {
    let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for field in declarations(schema)? {
        let root = if field.id.starts_with("common/") {
            "common".into()
        } else {
            field.id.split('/').take(2).collect::<Vec<_>>().join("/")
        };
        groups
            .entry(root)
            .or_default()
            .push((field.id, field.contract_sha256));
    }
    let mut baseline: BTreeMap<_, _> = groups
        .into_iter()
        .map(|(root, fields)| {
            let entry = BaselineEntry {
                declarations: fields.len(),
                sha256: hash_bytes(&serde_json::to_vec(&fields).expect("string pairs serialize")),
            };
            (root, entry)
        })
        .collect();
    baseline.insert(
        "$schema/version_and_sources".into(),
        BaselineEntry {
            declarations: 0,
            sha256: hash_bytes(&serde_json::to_vec(&(schema.version, &schema.sources))?),
        },
    );
    Ok(baseline)
}

#[derive(Debug, Serialize)]
pub struct Sample {
    pub form_id: u32,
    /// Offset in the original decompressed payload, not the canonical rkyv buffer.
    pub payload_offset: usize,
    pub byte_length: usize,
    pub sha256: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Occurrences {
    pub source_occurrences: u64,
    pub canonical_occurrences: u64,
    pub exact_byte_matches: u64,
    /// Sum of per-record deficits. Surplus occurrences in another record cannot cancel a loss.
    pub canonical_occurrence_deficit: u64,
    /// Includes intentional remaps, string decoding and validation; not a loss count.
    pub changed_or_omitted_source_occurrences: u64,
    pub source_bytes: u64,
    pub canonical_bytes: u64,
    pub declared_signature: bool,
    pub samples: Vec<Sample>,
}

#[derive(Debug, Default, Serialize)]
pub struct StorageAudit {
    pub source_records: u64,
    pub canonical_records_without_source: u64,
    pub source_records_without_canonical: u64,
    pub canonical_sequence_equal_records: u64,
    /// Includes order changes as well as remaps, validation and omissions.
    pub canonical_sequence_changed_records: u64,
    pub malformed_payloads: u64,
    pub unparsed_bytes: u64,
    pub malformed_samples: Vec<Sample>,
    /// Hex signatures preserve even non-ASCII unknown tags without replacement collisions.
    pub occurrences: BTreeMap<String, Occurrences>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct StorageCoverage {
    pub status: &'static str,
    pub matched_records: u64,
    pub source_only_records: u64,
    pub canonical_only_records: u64,
}

impl StorageAudit {
    /// Row coverage is separate from byte equality and runtime semantics.
    pub fn coverage(&self) -> StorageCoverage {
        StorageCoverage {
            status: if self.canonical_records_without_source > 0 {
                "incomplete_source_evidence"
            } else if self.source_records == 0 {
                "empty"
            } else if self.source_records_without_canonical > 0 {
                "source_only_records"
            } else {
                "matched_input_rows"
            },
            matched_records: self.source_records - self.source_records_without_canonical,
            source_only_records: self.source_records_without_canonical,
            canonical_only_records: self.canonical_records_without_source,
        }
    }
}

fn hex_signature(tag: &[u8]) -> String {
    tag.iter().map(|byte| format!("{byte:02X}")).collect()
}

struct SourceField<'a> {
    tag: [u8; 4],
    offset: usize,
    bytes: &'a [u8],
}

/// Retain every safely framed prefix occurrence and the entire malformed suffix.
/// XXXX framing remains part of source provenance; its length controls the next payload.
fn frame_source(payload: &[u8]) -> (Vec<SourceField<'_>>, Option<usize>) {
    let mut fields = Vec::new();
    let mut cursor = 0usize;
    while cursor < payload.len() {
        let start = cursor;
        if payload.len() - cursor < 6 {
            return (fields, Some(start));
        }
        let mut tag: [u8; 4] = payload[cursor..cursor + 4].try_into().unwrap();
        let mut length = usize::from(u16::from_le_bytes(
            payload[cursor + 4..cursor + 6].try_into().unwrap(),
        ));
        cursor += 6;
        if &tag == b"XXXX" {
            if length != 4 || payload.len() - cursor < 10 {
                return (fields, Some(start));
            }
            length = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
            cursor += 4;
            tag = payload[cursor..cursor + 4].try_into().unwrap();
            if &tag == b"XXXX" {
                return (fields, Some(start));
            }
            cursor += 6;
        }
        if length > payload.len() - cursor {
            return (fields, Some(start));
        }
        fields.push(SourceField {
            tag,
            offset: start,
            bytes: &payload[cursor..cursor + length],
        });
        cursor += length;
    }
    (fields, None)
}

fn declared_signatures(
    schema: &Schema,
    extensions: &BTreeMap<[u8; 4], Vec<FieldSchema>>,
) -> Result<BTreeMap<String, BTreeSet<[u8; 4]>>> {
    fn visit(schema: &Schema, field: &FieldSchema, tags: &mut BTreeSet<[u8; 4]>) -> Result<()> {
        let field = schema
            .resolve(field)
            .map_err(color_eyre::eyre::Error::msg)?;
        if let Some(tag) = &field.signature {
            tags.insert(tag.as_bytes().try_into()?);
        }
        for child in field.fields.iter().chain(&field.members) {
            visit(schema, child, tags)?;
        }
        Ok(())
    }
    let mut signatures = BTreeMap::new();
    for record in &schema.records {
        let mut tags = BTreeSet::new();
        for field in record.fields.iter().chain(&schema.common_fields) {
            visit(schema, field, &mut tags)?;
        }
        if let Some(fields) = extensions.get(record.signature.as_bytes()) {
            for field in fields {
                visit(schema, field, &mut tags)?;
            }
        }
        signatures.insert(record.signature.clone(), tags);
    }
    let mut common = BTreeSet::new();
    for field in &schema.common_fields {
        visit(schema, field, &mut common)?;
    }
    signatures.insert("*".into(), common);
    Ok(signatures)
}

/// Inspect a caller-owned read-only snapshot. No SQL rows or assets are changed.
/// Only winning preserved payloads are covered; history, deleted records and header bytes
/// require independent source inspection and are never inferred from this denominator.
pub fn audit_storage(conn: &Connection, schema: &Schema) -> Result<StorageAudit> {
    audit_storage_with_extensions(conn, schema, &BTreeMap::new())
}

/// Include new record types and independently matched extension fields from a validated bundle.
pub fn audit_runtime_storage(
    conn: &Connection,
    schemas: &super::addons::RuntimeSchemas,
) -> Result<StorageAudit> {
    audit_storage_with_extensions(conn, schemas.schema(), schemas.extensions())
}

fn audit_storage_with_extensions(
    conn: &Connection,
    schema: &Schema,
    extensions: &BTreeMap<[u8; 4], Vec<FieldSchema>>,
) -> Result<StorageAudit> {
    let declared = declared_signatures(schema, extensions)?;
    let mut result = StorageAudit {
        canonical_records_without_source: conn.query_row(
            "SELECT count(*) FROM records r LEFT JOIN inhouse_source_records s ON s.form_id=r.form_id WHERE s.form_id IS NULL", [], |row| row.get(0),
        )?,
        ..Default::default()
    };
    let mut query = conn.prepare(
        "SELECT s.form_id,s.record_type,s.payload,r.data,r.record_type FROM inhouse_source_records s LEFT JOIN records r ON r.form_id=s.form_id ORDER BY s.form_id",
    )?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let id: u32 = row.get(0)?;
        let record_type: String = row.get(1)?;
        let payload: Vec<u8> = row.get(2)?;
        let canonical: Option<Vec<u8>> = row.get(3)?;
        let canonical_type: Option<String> = row.get(4)?;
        ensure!(
            canonical_type
                .as_ref()
                .is_none_or(|kind| kind == &record_type),
            "record type mismatch for {id:08X}"
        );
        result.source_records += 1;
        result.source_records_without_canonical += u64::from(canonical.is_none());
        let canonical = canonical
            .map(|bytes| rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&bytes))
            .transpose()?;
        let (fields, malformed) = frame_source(&payload);
        if let Some(canonical) = &canonical {
            let equal = malformed.is_none()
                && fields.len() == canonical.subrecords.len()
                && fields
                    .iter()
                    .zip(&canonical.subrecords)
                    .all(|(source, canonical)| {
                        source.tag == canonical.tag && source.bytes == canonical.data
                    });
            result.canonical_sequence_equal_records += u64::from(equal);
            result.canonical_sequence_changed_records += u64::from(!equal);
        }
        if let Some(offset) = malformed {
            result.malformed_payloads += 1;
            result.unparsed_bytes += (payload.len() - offset) as u64;
            if result.malformed_samples.len() < 8 {
                result.malformed_samples.push(Sample {
                    form_id: id,
                    payload_offset: offset,
                    byte_length: payload.len() - offset,
                    sha256: hash_bytes(&payload[offset..]),
                });
            }
        }
        let mut source_counts: BTreeMap<[u8; 4], u64> = BTreeMap::new();
        let mut canonical_counts: BTreeMap<[u8; 4], u64> = BTreeMap::new();
        let mut matching: BTreeMap<([u8; 4], String), u64> = BTreeMap::new();
        let tags = declared.get(&record_type).unwrap_or(&declared["*"]);
        if let Some(canonical) = canonical {
            for field in canonical.subrecords {
                *canonical_counts.entry(field.tag).or_default() += 1;
                *matching
                    .entry((field.tag, hash_bytes(&field.data)))
                    .or_default() += 1;
                let entry = result
                    .occurrences
                    .entry(format!(
                        "{}/{}",
                        hex_signature(record_type.as_bytes()),
                        hex_signature(&field.tag)
                    ))
                    .or_default();
                entry.declared_signature = tags.contains(&field.tag);
                entry.canonical_occurrences += 1;
                entry.canonical_bytes += field.data.len() as u64;
            }
        }
        for field in fields {
            *source_counts.entry(field.tag).or_default() += 1;
            let entry = result
                .occurrences
                .entry(format!(
                    "{}/{}",
                    hex_signature(record_type.as_bytes()),
                    hex_signature(&field.tag)
                ))
                .or_default();
            entry.declared_signature = tags.contains(&field.tag);
            entry.source_occurrences += 1;
            entry.source_bytes += field.bytes.len() as u64;
            let digest = hash_bytes(field.bytes);
            if let Some(count) = matching
                .get_mut(&(field.tag, digest.clone()))
                .filter(|count| **count > 0)
            {
                *count -= 1;
                entry.exact_byte_matches += 1;
            } else {
                entry.changed_or_omitted_source_occurrences += 1;
            }
            if entry.samples.len() < 8 {
                entry.samples.push(Sample {
                    form_id: id,
                    payload_offset: field.offset,
                    byte_length: field.bytes.len(),
                    sha256: digest,
                });
            }
        }
        for (tag, count) in source_counts {
            let deficit = count.saturating_sub(canonical_counts.get(&tag).copied().unwrap_or(0));
            result
                .occurrences
                .get_mut(&format!(
                    "{}/{}",
                    hex_signature(record_type.as_bytes()),
                    hex_signature(&tag)
                ))
                .unwrap()
                .canonical_occurrence_deficit += deficit;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
