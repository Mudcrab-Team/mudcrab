//! Authored schema-driven Skyrim records, independent of the legacy field decoder.
pub mod addons;
pub mod compact_array;
pub mod deciders;
mod decoder;
pub mod diagnostics;
pub mod history;
pub mod history_source;
pub(crate) mod profile;
mod scanner;
pub mod schema_format;
pub mod source_identity;
pub(crate) mod typed_export;
pub mod vmad;

#[cfg(test)]
pub(crate) mod review_recovery_tests;

use crate::esm::{load_order::LoadOrder, records::RawRecord};
use color_eyre::Result;
use schema_format::Schema;
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::OnceLock,
};

/// The checked data schema; changing it changes the in-house producer identity.
pub const SCHEMA_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/records-schema.json"));

/// Numeric values keep their binary widths in the schema; floats retain raw f32 precision.
/// Arrays may use ordinary or compact storage. Tooling should use `array_element` or
/// `materialized_array` to inspect either representation; serialized tags remain unchanged.
#[derive(Clone, Debug)]
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
    /// Internal immutable fixed layout; serialization remains the legacy Array variant.
    CompactArray(compact_array::CompactArray),
}

impl Value {
    /// Inspect one array element without materializing the entire compact array.
    /// Ordinary arrays clone only the selected element; compact arrays decode it on demand.
    pub fn array_element(&self, index: usize) -> Option<Value> {
        match self {
            Self::Array(values) => values.get(index).cloned(),
            Self::CompactArray(values) => values.get(index),
            _ => None,
        }
    }

    /// Obtain owned array elements for explicit inspection.
    /// This allocates in proportion to the element count; streaming export does not use it.
    pub fn materialized_array(&self) -> Option<Vec<Value>> {
        match self {
            Self::Array(values) => Some(values.clone()),
            Self::CompactArray(values) => Some(values.materialize()),
            _ => None,
        }
    }
}

impl PartialEq for Value {
    /// Compare the legacy logical value independent of compact storage and padding.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unsigned(a), Self::Unsigned(b)) => a == b,
            (Self::Signed(a), Self::Signed(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::LocalizedString(a), Self::LocalizedString(b)) => a == b,
            (Self::FormId(a), Self::FormId(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) | (Self::Deferred(a), Self::Deferred(b)) => a == b,
            (Self::Struct(a), Self::Struct(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::CompactArray(a), Self::CompactArray(b)) => a == b,
            (Self::CompactArray(a), Self::Array(b)) | (Self::Array(b), Self::CompactArray(a)) => {
                a.len() == b.len()
                    && b.iter()
                        .enumerate()
                        .all(|(index, value)| a.get(index).as_ref() == Some(value))
            }
            _ => false,
        }
    }
}

impl Serialize for Value {
    /// Keep the derived enum's external tags, indices and ordinary f32 policy unchanged.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Unsigned(value) => {
                serializer.serialize_newtype_variant("Value", 0, "Unsigned", value)
            }
            Self::Signed(value) => {
                serializer.serialize_newtype_variant("Value", 1, "Signed", value)
            }
            Self::Float(value) => serializer.serialize_newtype_variant("Value", 2, "Float", value),
            Self::String(value) => {
                serializer.serialize_newtype_variant("Value", 3, "String", value)
            }
            Self::LocalizedString(value) => {
                serializer.serialize_newtype_variant("Value", 4, "LocalizedString", value)
            }
            Self::FormId(value) => {
                serializer.serialize_newtype_variant("Value", 5, "FormId", value)
            }
            Self::Bytes(value) => serializer.serialize_newtype_variant("Value", 6, "Bytes", value),
            Self::Deferred(value) => {
                serializer.serialize_newtype_variant("Value", 7, "Deferred", value)
            }
            Self::Struct(value) => {
                serializer.serialize_newtype_variant("Value", 8, "Struct", value)
            }
            Self::Array(value) => serializer.serialize_newtype_variant("Value", 9, "Array", value),
            Self::CompactArray(value) => {
                serializer.serialize_newtype_variant("Value", 9, "Array", &value.elements(false))
            }
        }
    }
}

/// One typed or opaque subrecord occurrence with its canonical bytes.
#[derive(Clone, Debug, Serialize)]
pub struct DecodedField {
    pub signature: [u8; 4],
    pub name: String,
    pub value: Value,
    pub canonical_bytes: Vec<u8>,
    /// True only for unrecognized bytes, never for an authored `bytes` descriptor.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub opaque: bool,
    /// The selected localized leaf's bank, retained only for runtime publication.
    #[serde(skip)]
    pub localized_bank: Option<String>,
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
    /// First-pass selectors awaiting the winning catalogue; excluded from every export.
    #[serde(skip)]
    pub(crate) deferred_fields: Vec<decoder::DeferredField>,
    /// Known fields rejected during decoding or ordered matching, distinct from source absence.
    pub rejected_fields: Vec<[u8; 4]>,
    /// False when a subrecord boundary prevented scanning the remaining payload safely.
    pub payload_complete: bool,
    pub raw_payload: Vec<u8>,
    pub supported: bool,
}

impl DecodedRecord {
    /// Adapt canonical bytes to the established record format, retaining opaque occurrences.
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

/// A bounded total order for one diagnostic, independent of its observation order.
/// Known physical offsets precede unknown provenance; record IDs, field paths and
/// observation details break ties. Full-text hashes distinguish truncated prefixes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DiagnosticSummaryKey {
    source_record_offset: (bool, u64),
    source_form_id: (bool, u32),
    form_id: (bool, u32),
    record_type: Option<[u8; 4]>,
    field: Option<String>,
    occurrence: u64,
    stage: &'static str,
    category: String,
    action: Option<&'static str>,
    link_value: Option<u32>,
    reason: String,
    full_text_hash: [u8; 32],
}

impl DiagnosticSummaryKey {
    /// Rank physical source coordinates before bounded field and observation details.
    pub(crate) fn new(context: &diagnostics::EventContext, category: &str, reason: &str) -> Self {
        let mut identity = Sha256::new();
        for text in [context.field.as_deref(), Some(category), Some(reason)] {
            identity.update([u8::from(text.is_some())]);
            if let Some(text) = text {
                identity.update((text.len() as u64).to_le_bytes());
                identity.update(text.as_bytes());
            }
        }
        Self {
            source_record_offset: (
                context.source_record_offset.is_none(),
                context.source_record_offset.unwrap_or_default(),
            ),
            source_form_id: (
                context.source_form_id.is_none(),
                context.source_form_id.unwrap_or_default(),
            ),
            form_id: (
                context.form_id.is_none(),
                context.form_id.unwrap_or_default(),
            ),
            record_type: context.record_type,
            field: context
                .field
                .as_deref()
                .map(|field| Self::bounded(field, 512)),
            occurrence: context.occurrence,
            stage: context.stage,
            category: Self::bounded(category, 2048),
            action: context.action,
            link_value: context.link_value,
            reason: Self::bounded(reason, 2048),
            full_text_hash: identity.finalize().into(),
        }
    }

    /// Authored examples without internal keys sort after physical provenance by text.
    fn fallback(reason: &str) -> Self {
        Self::new(&diagnostics::EventContext::default(), "", reason)
    }

    /// Retain a complete UTF-8 prefix without copying unbounded source text first.
    fn bounded(text: &str, limit: usize) -> String {
        let mut end = text.len().min(limit);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_owned()
    }
}

/// Bounded summaries accompany complete disk-backed diagnostic events.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PluginDiagnostics {
    pub skipped_records: u64,
    pub skipped_fields: u64,
    pub unexpected_subrecords: u64,
    pub invalid_links: u64,
    /// Successful required-header compatibility remaps, including later rejected candidates.
    pub compatibility_headers: u64,
    pub first_example: Option<String>,
    pub first_by_category: BTreeMap<String, String>,
    #[serde(skip)]
    pub(crate) first_summary_key: Option<DiagnosticSummaryKey>,
    #[serde(skip)]
    pub(crate) summary_keys: BTreeMap<String, DiagnosticSummaryKey>,
    #[serde(skip)]
    pub(crate) event_log: diagnostics::EventLog,
    #[serde(skip)]
    pub(crate) event_context: diagnostics::EventContext,
    /// Temporary first-pass contexts for unresolved leaves, moved into replay metadata.
    #[serde(skip)]
    pub(crate) deferred_contexts: Vec<diagnostics::EventContext>,
    /// First-pass link observations retained only for roots with deferred selectors.
    #[serde(skip)]
    pub(crate) deferred_link_events: Option<Vec<decoder::DeferredLinkEvent>>,
}

impl PluginDiagnostics {
    /// Build bounded data-loss warnings for terminal output and conversion reports.
    pub(crate) fn warning_summary_lines(&self, plugin: &str) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(example) = ["record", "field", "link", "unexpected"]
            .iter()
            .find_map(|category| self.first_by_category.get(*category))
        {
            lines.push(format!(
                "warning: {plugin}: in-house reader skipped {} records, {} fields, {} unexpected subrecords; cleared {} invalid links (first: {example})",
                self.skipped_records,
                self.skipped_fields,
                self.unexpected_subrecords,
                self.invalid_links
            ));
        }
        lines
    }

    /// Successful supplier-self remaps are advisory, even when another issue loses data.
    pub(crate) fn notice_summary_lines(&self, plugin: &str) -> Vec<String> {
        let mut lines = Vec::new();
        if self.compatibility_headers != 0
            && let Some(example) = self.first_by_category.get("compatibility")
        {
            lines.push(format!(
                "notice: {plugin}: used supplier-self runtime compatibility for {} headers (original owners unresolved; first: {example})",
                self.compatibility_headers
            ));
        }
        lines
    }

    /// Journal every observation while retaining the least source example per category.
    pub(crate) fn note(&mut self, category: &str, example: impl FnOnce() -> String) {
        match category {
            "record" => self.skipped_records += 1,
            "field" => self.skipped_fields += 1,
            "unexpected" => self.unexpected_subrecords += 1,
            "compatibility" => self.compatibility_headers += 1,
            _ => self.invalid_links += 1,
        }
        let example = example();
        let mut context = self.event_context.clone();
        if category != "link" {
            context.link_value = None;
            context.occurrence = 0;
        }
        self.event_log.append(&context, category, &example);
        let key = DiagnosticSummaryKey::new(&context, category, &example);
        self.remember_summary(category, key, diagnostics::bounded_summary(example));
    }

    /// Keep category and global examples under the same total order, without changing counts.
    fn remember_summary(&mut self, category: &str, key: DiagnosticSummaryKey, example: String) {
        self.remember_first(&key, &example);
        let replace = self.first_by_category.get(category).is_none_or(|current| {
            match self.summary_keys.get(category) {
                Some(current_key) => &key < current_key,
                None => key < DiagnosticSummaryKey::fallback(current),
            }
        });
        if replace {
            self.first_by_category.insert(category.to_owned(), example);
            self.summary_keys.insert(category.to_owned(), key);
        }
    }

    /// Select the global example, including authored summaries with no category entry.
    fn remember_first(&mut self, key: &DiagnosticSummaryKey, example: &str) {
        let replace =
            self.first_example
                .as_deref()
                .is_none_or(|current| match &self.first_summary_key {
                    Some(current_key) => key < current_key,
                    None => key < &DiagnosticSummaryKey::fallback(current),
                });
        if replace {
            self.first_example = Some(example.to_owned());
            self.first_summary_key = Some(key.clone());
        }
    }

    /// Merge one category's bounded example and provenance, leaving counts and journals alone.
    pub(crate) fn merge_summary_category(&mut self, source: &Self, category: &str) {
        if let Some(example) = source.first_by_category.get(category) {
            let key = source
                .summary_keys
                .get(category)
                .cloned()
                .unwrap_or_else(|| DiagnosticSummaryKey::fallback(example));
            self.remember_summary(category, key, diagnostics::bounded_summary(example.clone()));
        }
    }

    /// Preserve ordering metadata whenever replay or test oracles copy bounded summaries.
    pub(crate) fn merge_summaries(&mut self, source: &Self) {
        if let Some(example) = &source.first_example {
            let key = source
                .first_summary_key
                .clone()
                .unwrap_or_else(|| DiagnosticSummaryKey::fallback(example));
            self.remember_first(&key, &diagnostics::bounded_summary(example.clone()));
        }
        for category in source.first_by_category.keys() {
            self.merge_summary_category(source, category);
        }
    }

    /// Reset context at a physical record or framing boundary.
    pub(crate) fn set_record(
        &mut self,
        offset: u64,
        signature: Option<[u8; 4]>,
        source_id: Option<u32>,
        form_id: Option<u32>,
        stage: &'static str,
    ) {
        self.event_context = diagnostics::EventContext {
            source_record_offset: Some(offset),
            source_form_id: source_id,
            form_id,
            record_type: signature,
            stage,
            ..Default::default()
        };
    }

    /// Identify a native or canonical field occurrence and restart its link ordinal.
    pub(crate) fn set_field(&mut self, field: String) {
        self.event_context.field = Some(field);
        self.event_context.link_value = None;
        self.event_context.occurrence = 0;
        self.event_context.action = None;
    }

    /// Keep nested member and array paths without retaining payload copies.
    pub(crate) fn push_field(&mut self, member: &str) -> Option<String> {
        let previous = self.event_context.field.clone();
        self.event_context.field = Some(match &previous {
            Some(parent) => format!("{parent}.{member}"),
            None => member.to_owned(),
        });
        previous
    }

    /// Restore the parent path after a bounded recursive decode or validation.
    pub(crate) fn pop_field(&mut self, previous: Option<String>) {
        self.event_context.field = previous;
        self.event_context.link_value = None;
    }

    /// Distinguish equal link values at different positions within one field.
    pub(crate) fn link(&mut self, value: u32) {
        self.event_context.link_value = Some(value);
        self.event_context.occurrence += 1;
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
    /// Physical header provenance includes accepted, deleted and rejected versions.
    pub history: Vec<history::HistoryEntry>,
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

pub(crate) use decoder::{PreparedSchemas, prepare_runtime_schema, string_table_with_prepared};

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
    validate: impl FnMut(&DecodedRecord) -> std::result::Result<(), String>,
    observe: impl FnMut(&ReadResult),
) -> Result<ReadResult> {
    read_plugins_with_prepared(
        plugin_paths,
        order,
        schema(),
        decoder::prepared(),
        validate,
        observe,
    )
}

/// Use a validated conversion-local schema for initial decode, replay and target checks.
pub(crate) fn read_plugins_with_runtime_schemas(
    plugin_paths: &[PathBuf],
    order: &LoadOrder,
    schemas: &addons::RuntimeSchemas,
    validate: impl FnMut(&DecodedRecord) -> std::result::Result<(), String>,
    observe: impl FnMut(&ReadResult),
) -> Result<ReadResult> {
    let prepared = decoder::prepare_runtime_schema(schemas);
    read_plugins_with_prepared(
        plugin_paths,
        order,
        schemas.schema(),
        &prepared,
        validate,
        observe,
    )
}

/// Exercise the real reader with an independently checked schema in internal regressions only.
#[cfg(test)]
pub(crate) fn read_plugins_with_validation_and_observer_with_schema(
    plugin_paths: &[PathBuf],
    order: &LoadOrder,
    schema: &Schema,
    validate: impl FnMut(&DecodedRecord) -> std::result::Result<(), String>,
    observe: impl FnMut(&ReadResult),
) -> Result<ReadResult> {
    let prepared = decoder::prepare_schema(schema);
    read_plugins_with_prepared(plugin_paths, order, schema, &prepared, validate, observe)
}

/// Share winner selection across embedded publication and the private schema regression path.
fn read_plugins_with_prepared(
    plugin_paths: &[PathBuf],
    order: &LoadOrder,
    schema: &Schema,
    prepared: &decoder::PreparedSchemas,
    mut validate: impl FnMut(&DecodedRecord) -> std::result::Result<(), String>,
    mut observe: impl FnMut(&ReadResult),
) -> Result<ReadResult> {
    color_eyre::eyre::ensure!(
        plugin_paths.len() == order.names.len(),
        "plugin/order length mismatch"
    );
    let mut result = ReadResult::default();
    // Retain payloads only while an admitted winner still needs catalogue replay.
    let mut deferred_predecessors: HashMap<u32, Vec<DecodedRecord>> = HashMap::new();
    let mut gmst_ids: HashMap<String, u32> = HashMap::new();
    let mut gmst_aliases: HashMap<u32, u32> = HashMap::new();
    let scan_stage = profile::ProfileStage::start("scan_decode_merge");
    for (index, path) in plugin_paths.iter().enumerate() {
        let name = &order.names[index];
        let mut diagnostics = PluginDiagnostics::default();
        let mut seen = HashMap::new();
        let history = std::cell::RefCell::new(Vec::<history::HistoryEntry>::new());
        let observe = |header| {
            history
                .borrow_mut()
                .push(history::header_entry(header, order, index));
        };
        scanner::scan_plugin(path, &mut diagnostics, observe, |scanned, diagnostics| {
            let offset = scanned.source_record_offset;
            let context = decoder::Context {
                order,
                plugin_index: index,
                localized: order.metadata[index].flags & 0x80 != 0,
                winning_types: None,
            };
            let mut identity = |name: &str| {
                if let Some(entry) = history.borrow_mut().last_mut() {
                    entry.editor_id = Some(name.to_ascii_lowercase());
                }
            };
            let mut record = match decoder::decode_with_identity(
                scanned,
                schema,
                prepared,
                &context,
                diagnostics,
                &mut identity,
            ) {
                Ok(record) => record,
                Err(error) => {
                    if let Some(entry) = history.borrow_mut().last_mut() {
                        entry.reason = Some(error.clone());
                    }
                    if diagnostics
                        .event_context
                        .form_id
                        .is_some_and(|id| result.records.contains_key(&id))
                    {
                        diagnostics.event_context.action = Some("kept_earlier_version");
                    }
                    diagnostics.note("record", || error);
                    return;
                }
            };
            if record.record_type == *b"TES4" {
                result.headers.push(record);
                return;
            }
            diagnostics.set_record(
                record.source_record_offset,
                Some(record.record_type),
                Some(record.source_form_id),
                Some(record.form_id),
                "admission",
            );
            if let Some(entry) = history.borrow_mut().last_mut() {
                entry.form_id = Some(record.form_id);
                entry.editor_id = (record.record_type == *b"GMST")
                    .then(|| {
                        record.fields.iter().find_map(|field| {
                            if field.signature == *b"EDID"
                                && let Value::String(name) = &field.value
                            {
                                Some(name.to_ascii_lowercase())
                            } else {
                                None
                            }
                        })
                    })
                    .flatten();
            }
            if record.flags & 0x20 == 0
                && let Err(error) = validate(&record)
            {
                if let Some(entry) = history.borrow_mut().last_mut() {
                    entry.reason = Some(format!("unusable runtime candidate: {error}"));
                }
                if result.records.contains_key(&record.form_id) {
                    diagnostics.event_context.action = Some("kept_earlier_version");
                }
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
            // GMST winners use EditorID identity even when a later occurrence has a
            // different native header ID. Resolve it without publishing a new alias.
            let candidate_id = if record.record_type == *b"GMST" {
                record
                    .fields
                    .iter()
                    .find_map(|field| {
                        if field.signature == *b"EDID"
                            && let Value::String(name) = &field.value
                        {
                            gmst_ids.get(&name.to_ascii_lowercase()).copied()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(record.form_id)
            } else {
                record.form_id
            };
            if record.flags & 0x20 == 0
                && !record.payload_complete
                && result
                    .records
                    .get(&candidate_id)
                    .is_some_and(|previous| previous.payload_complete)
            {
                if let Some(entry) = history.borrow_mut().last_mut() {
                    entry.reason =
                        Some("incomplete override keeps the earlier complete version".into());
                }
                diagnostics.event_context.action = Some("kept_earlier_version");
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
            // Zero-header GMST occurrences share no native identity. Only a known
            // EditorID supplies a duplicate key; an unknown name cannot claim zero.
            let duplicate_key = if record.record_type == *b"GMST" && record.form_id == 0 {
                (candidate_id != 0).then_some(candidate_id)
            } else {
                Some(record.form_id)
            };
            if let Some(seen_id) = duplicate_key
                && let Some(&previous) = seen.get(&seen_id)
            {
                if let Some(entry) = history.borrow_mut().last_mut() {
                    entry.reason = Some(format!(
                        "duplicate/colliding record {:08X}: source {previous:08X}, {source_id:08X}",
                        seen_id
                    ));
                }
                diagnostics.note("record", || {
                    format!(
                        "duplicate/colliding record {:08X}: source {previous:08X}, {source_id:08X}",
                        seen_id
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
                        || record.form_id != 0
                            && (gmst_aliases
                                .get(&record.form_id)
                                .is_some_and(|&previous| known != Some(previous))
                                || result
                                    .records
                                    .get(&record.form_id)
                                    .is_some_and(|previous| previous.record_type != *b"GMST"))
                    {
                        if let Some(entry) = history.borrow_mut().last_mut() {
                            entry.reason =
                                Some(format!("GMST {source_id:08X} has null/colliding identity"));
                        }
                        diagnostics.note("record", || {
                            format!("GMST {source_id:08X} has null/colliding identity")
                        });
                        return;
                    }
                    // Null native words cannot identify later header-only tombstones.
                    if record.form_id != 0 {
                        gmst_aliases.insert(record.form_id, canonical);
                    }
                    gmst_ids.insert(editor_id, canonical);
                    record.form_id = canonical;
                } else if record.flags & 0x20 != 0 {
                    let Some(&canonical) = gmst_aliases.get(&record.form_id) else {
                        if let Some(entry) = history.borrow_mut().last_mut() {
                            entry.reason = Some(format!(
                                "deleted GMST {source_id:08X} has no known identity"
                            ));
                        }
                        diagnostics.note("record", || {
                            format!("deleted GMST {source_id:08X} has no known identity")
                        });
                        return;
                    };
                    record.form_id = canonical;
                } else {
                    if let Some(entry) = history.borrow_mut().last_mut() {
                        entry.reason = Some(format!("GMST {source_id:08X} has no EditorID"));
                    }
                    diagnostics.note("record", || format!("GMST {source_id:08X} has no EditorID"));
                    return;
                }
            } else if gmst_aliases.contains_key(&record.form_id) {
                if let Some(entry) = history.borrow_mut().last_mut() {
                    entry.reason = Some(format!(
                        "record {:08X} collides with GMST identity",
                        record.form_id
                    ));
                }
                diagnostics.note("record", || {
                    format!("record {:08X} collides with GMST identity", record.form_id)
                });
                return;
            }
            // Rejected identities cannot claim a later usable candidate's duplicate key.
            if let Some(seen_id) = duplicate_key {
                seen.insert(seen_id, source_id);
            }
            let source = RecordSource {
                plugin_index: index as u32,
                source_form_id: source_id,
                deleted: record.flags & 0x20 != 0,
            };
            if let Some(entry) = history.borrow_mut().last_mut() {
                debug_assert_eq!(entry.source_record_offset, offset);
                entry.form_id = Some(record.form_id);
                entry.accepted = true;
                entry.reason = None;
            }
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
                deferred_predecessors.remove(&record.form_id);
            } else {
                let id = record.form_id;
                let tentative = decoder::record_contains_deferred(&record);
                let previous = result.records.insert(id, record);
                if tentative {
                    if let Some(previous) = previous {
                        deferred_predecessors.entry(id).or_default().push(previous);
                    }
                } else {
                    deferred_predecessors.remove(&id);
                }
            }
        })?;
        result.history.extend(history.into_inner());
        result.diagnostics.insert(name.clone(), diagnostics);
    }
    scan_stage.finish(result.records.len());
    let deferred_stage = profile::ProfileStage::start("resolve_deferred");
    history::reconcile_gmst(&mut result.history, &gmst_ids);
    decoder::resolve_deferred(
        &mut result,
        schema,
        prepared,
        order,
        &mut deferred_predecessors,
        &mut validate,
    );
    deferred_stage.finish(result.records.len());
    let preservation_stage = profile::ProfileStage::start("preservation_observer");
    observe(&result);
    preservation_stage.finish(result.records.len());
    let targets_stage = profile::ProfileStage::start("validate_targets");
    decoder::validate_targets_with_prepared(&mut result, schema, prepared, order);
    targets_stage.finish(result.records.len());
    for (name, diagnostics) in &result.diagnostics {
        for line in diagnostics
            .warning_summary_lines(name)
            .into_iter()
            .chain(diagnostics.notice_summary_lines(name))
        {
            eprintln!("{line}");
        }
    }
    Ok(result)
}
