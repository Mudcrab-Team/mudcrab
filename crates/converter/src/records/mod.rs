//! Authored schema-driven Skyrim records, independent of the legacy field decoder.
pub mod deciders;
mod decoder;
mod scanner;
pub mod schema_format;
pub mod vmad;

use crate::esm::{load_order::LoadOrder, records::RawRecord};
use color_eyre::Result;
use schema_format::Schema;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::OnceLock,
};

/// The checked data schema; changing it changes the in-house producer identity.
pub const SCHEMA_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/records-schema.json"));

/// Numeric values keep their binary widths in the schema; floats retain raw f32 precision.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Value {
    Unsigned(u64),
    Signed(i64),
    Float(f32),
    String(String),
    LocalizedString(u32),
    FormId(u32),
    Bytes(Vec<u8>),
    /// Internal first-pass bytes resolved after accepted winner kinds are known.
    Deferred(Vec<u8>),
    Struct(Vec<(String, Value)>),
    Array(Vec<Value>),
}

/// One matched subrecord and the validated bytes used by runtime projections.
#[derive(Clone, Debug, Serialize)]
pub struct DecodedField {
    pub signature: [u8; 4],
    pub name: String,
    pub value: Value,
    pub canonical_bytes: Vec<u8>,
}

/// A winning record with source bytes distinct from resolved typed values.
#[derive(Clone, Debug, Serialize)]
pub struct DecodedRecord {
    pub form_id: u32,
    pub source_form_id: u32,
    pub record_type: [u8; 4],
    pub flags: u32,
    pub version_control: u32,
    pub form_version: u16,
    pub header_unknown: u16,
    pub load_order: u32,
    pub cell_form_id: Option<u32>,
    pub worldspace_form_id: Option<u32>,
    /// Resolved DIAL owner from an enclosing native GRUP7, independent of optional TPIC.
    pub topic_form_id: Option<u32>,
    /// Unmodified file-relative label of that source topic group, even if invalid.
    pub source_topic_form_id: Option<u32>,
    /// Source GRUP7 header offset; this does not infer merged INFO execution order.
    pub topic_group_offset: Option<u64>,
    /// Source record-header offset, including for records requiring deferred replay.
    pub source_record_offset: u64,
    pub fields: Vec<DecodedField>,
    /// Known fields rejected during decoding or ordered matching, distinct from source absence.
    pub rejected_fields: Vec<[u8; 4]>,
    /// False when a subrecord boundary prevented scanning the remaining payload safely.
    pub payload_complete: bool,
    pub raw_payload: Vec<u8>,
    pub supported: bool,
}

impl DecodedRecord {
    /// Adapt checked typed fields to the existing runtime-table and terrain projections.
    pub fn to_raw_record(&self) -> RawRecord {
        RawRecord {
            form_id: self.form_id,
            record_type: self.record_type,
            flags: self.flags,
            subrecords: self
                .fields
                .iter()
                .map(|field| (field.signature.to_vec(), field.canonical_bytes.clone()))
                .collect(),
            cell_form_id: self.cell_form_id,
            worldspace_form_id: self.worldspace_form_id,
            load_order: self.load_order,
        }
    }
}

/// Bounded per-plugin diagnostics retain one example, independent of record count.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PluginDiagnostics {
    pub skipped_records: u64,
    pub skipped_fields: u64,
    pub unexpected_subrecords: u64,
    pub invalid_links: u64,
    pub first_example: Option<String>,
    pub first_by_category: BTreeMap<String, String>,
}

impl PluginDiagnostics {
    /// Count a failure without allocating another retained example.
    pub(crate) fn note(&mut self, category: &str, example: impl FnOnce() -> String) {
        match category {
            "record" => self.skipped_records += 1,
            "field" => self.skipped_fields += 1,
            "unexpected" => self.unexpected_subrecords += 1,
            _ => self.invalid_links += 1,
        }
        if !self.first_by_category.contains_key(category) {
            let example = example();
            if self.first_example.is_none() {
                self.first_example = Some(example.clone());
            }
            self.first_by_category.insert(category.to_owned(), example);
        }
    }
}

/// Source identity of each entry in an actual override chain, including deletions.
#[derive(Clone, Debug, Serialize)]
pub struct RecordSource {
    pub plugin_index: u32,
    pub source_form_id: u32,
    pub deleted: bool,
}

/// Winners, nontrivial override chains and bounded scan/decode diagnostics.
#[derive(Debug, Default)]
pub struct ReadResult {
    pub records: HashMap<u32, DecodedRecord>,
    pub headers: Vec<DecodedRecord>,
    pub diagnostics: BTreeMap<String, PluginDiagnostics>,
    pub overrides: HashMap<u32, Vec<RecordSource>>,
}

/// Lazily parse the schema already validated by the build script.
fn schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| Schema::parse(SCHEMA_BYTES).expect("build-validated record schema"))
}

/// Decode English string-bank text with the ordinary plugin Windows-1252 encoding.
pub(crate) fn decode_text(bytes: &[u8]) -> String {
    decoder::decode_string(bytes)
}

/// Lookup uses the matched role name so repeated gender/rank signatures retain their text bank.
pub(crate) fn string_table(record_type: &[u8; 4], field_name: &str) -> &'static str {
    decoder::string_table(record_type, field_name)
}

/// Scan in load order using the converter's single slot/master authority.
pub fn read_plugins(plugin_paths: &[PathBuf], order: &LoadOrder) -> Result<ReadResult> {
    read_plugins_with_validation(plugin_paths, order, |_| Ok(()))
}

/// Allow consumers to reject an unusable candidate before it replaces a usable winner.
/// Header-only deletions still apply without requiring runtime fields.
pub(crate) fn read_plugins_with_validation(
    plugin_paths: &[PathBuf],
    order: &LoadOrder,
    validate: impl FnMut(&DecodedRecord) -> std::result::Result<(), String>,
) -> Result<ReadResult> {
    read_plugins_with_validation_and_observer(plugin_paths, order, validate, |_| {})
}

/// Observe accepted, globally remapped winners before optional target links are cleared.
/// Preservation consumers may retain explicitly unresolved keys without changing runtime links.
pub(crate) fn read_plugins_with_validation_and_observer(
    plugin_paths: &[PathBuf],
    order: &LoadOrder,
    mut validate: impl FnMut(&DecodedRecord) -> std::result::Result<(), String>,
    mut observe: impl FnMut(&ReadResult),
) -> Result<ReadResult> {
    color_eyre::eyre::ensure!(
        plugin_paths.len() == order.names.len(),
        "plugin/order length mismatch"
    );
    let schema = schema();
    let mut result = ReadResult::default();
    let mut gmst_ids: HashMap<String, u32> = HashMap::new();
    let mut gmst_aliases: HashMap<u32, u32> = HashMap::new();
    for (index, path) in plugin_paths.iter().enumerate() {
        let name = &order.names[index];
        let mut diagnostics = PluginDiagnostics::default();
        let mut seen = HashMap::new();
        scanner::scan_plugin(path, &mut diagnostics, |scanned, diagnostics| {
            let context = decoder::Context {
                order,
                plugin_index: index,
                localized: order.metadata[index].flags & 0x80 != 0,
                winning_types: None,
            };
            let mut record = match decoder::decode(scanned, schema, &context, diagnostics) {
                Ok(record) => record,
                Err(error) => {
                    diagnostics.note("record", || error);
                    return;
                }
            };
            if record.record_type == *b"TES4" {
                result.headers.push(record);
                return;
            }
            if record.flags & 0x20 == 0
                && let Err(error) = validate(&record)
            {
                diagnostics.note("record", || {
                    format!(
                        "{} {:08X}: unusable runtime candidate: {error}",
                        String::from_utf8_lossy(&record.record_type),
                        record.form_id
                    )
                });
                return;
            }
            // A torn override cannot show what its missing tail held, so it never
            // replaces a complete earlier version; a first definition keeps its prefix.
            if !record.payload_complete
                && result
                    .records
                    .get(&record.form_id)
                    .is_some_and(|previous| previous.payload_complete)
            {
                diagnostics.note("record", || {
                    format!(
                        "{} {:08X}: incomplete override keeps the earlier complete version",
                        String::from_utf8_lossy(&record.record_type),
                        record.form_id
                    )
                });
                return;
            }
            let source_id = record.source_form_id;
            if let Some(previous) = seen.insert(record.form_id, source_id) {
                diagnostics.note("record", || {
                    format!(
                        "duplicate/colliding record {:08X}: source {previous:08X}, {source_id:08X}",
                        record.form_id
                    )
                });
                return;
            }
            if record.record_type == *b"GMST" {
                let editor_id = record.fields.iter().find_map(|field| {
                    if field.signature == *b"EDID"
                        && let Value::String(name) = &field.value
                    {
                        Some(name.to_ascii_lowercase())
                    } else {
                        None
                    }
                });
                if let Some(editor_id) = editor_id.filter(|name| !name.is_empty()) {
                    let known = gmst_ids.get(&editor_id).copied();
                    let canonical = known.unwrap_or(record.form_id);
                    if canonical == 0
                        || gmst_aliases
                            .get(&record.form_id)
                            .is_some_and(|&previous| known != Some(previous))
                        || result
                            .records
                            .get(&record.form_id)
                            .is_some_and(|previous| previous.record_type != *b"GMST")
                    {
                        diagnostics.note("record", || {
                            format!("GMST {source_id:08X} has null/colliding identity")
                        });
                        return;
                    }
                    gmst_aliases.insert(record.form_id, canonical);
                    gmst_ids.insert(editor_id, canonical);
                    record.form_id = canonical;
                } else if record.flags & 0x20 != 0 {
                    let Some(&canonical) = gmst_aliases.get(&record.form_id) else {
                        diagnostics.note("record", || {
                            format!("deleted GMST {source_id:08X} has no known identity")
                        });
                        return;
                    };
                    record.form_id = canonical;
                } else {
                    diagnostics.note("record", || format!("GMST {source_id:08X} has no EditorID"));
                    return;
                }
            } else if gmst_aliases.contains_key(&record.form_id) {
                diagnostics.note("record", || {
                    format!("record {:08X} collides with GMST identity", record.form_id)
                });
                return;
            }
            let source = RecordSource {
                plugin_index: index as u32,
                source_form_id: source_id,
                deleted: record.flags & 0x20 != 0,
            };
            if let Some(previous) = result.records.get(&record.form_id) {
                result
                    .overrides
                    .entry(record.form_id)
                    .or_insert_with(|| {
                        vec![RecordSource {
                            plugin_index: previous.load_order,
                            source_form_id: previous.source_form_id,
                            deleted: false,
                        }]
                    })
                    .push(source);
            } else if let Some(chain) = result.overrides.get_mut(&record.form_id) {
                chain.push(source);
            } else if source.deleted {
                result.overrides.insert(record.form_id, vec![source]);
            }
            if record.flags & 0x20 != 0 {
                result.records.remove(&record.form_id);
            } else {
                result.records.insert(record.form_id, record);
            }
        })?;
        result.diagnostics.insert(name.clone(), diagnostics);
    }
    decoder::resolve_deferred(&mut result, schema, order);
    observe(&result);
    decoder::validate_targets(&mut result, schema, order);
    for (name, diagnostics) in &result.diagnostics {
        if let Some(example) = ["record", "field", "link", "unexpected"]
            .iter()
            .find_map(|category| diagnostics.first_by_category.get(*category))
        {
            eprintln!(
                "warning: {name}: in-house reader skipped {} records, {} fields, {} unexpected subrecords; cleared {} invalid links (first: {example})",
                diagnostics.skipped_records,
                diagnostics.skipped_fields,
                diagnostics.unexpected_subrecords,
                diagnostics.invalid_links
            );
        }
    }
    Ok(result)
}
