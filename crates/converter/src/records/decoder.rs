//! Ordered subrecord matching and generic values with typed FormID resolution.
#[cfg(test)]
#[path = "diagnostic_summary_tests.rs"]
mod diagnostic_summary_tests;

#[cfg(test)]
mod addon_controls {
    use super::*;

    /// Extension localization uses its declared bank instead of the embedded default.
    #[test]
    fn extensions_retain_their_declared_localized_bank() {
        let schemas = super::super::addons::RuntimeSchemas::from_addon_bytes(&[
            br#"{"version":1,"name":"bank-test","records":[{"signature":"KYWD","extends":true,"fields":[{"signature":"XTXT","name":"addon_text","kind":"lstring","string_table":"dlstrings"},{"signature":"XONE","kind":"lstring","string_table":"dlstrings"},{"signature":"XTWO","kind":"lstring","string_table":"ilstrings"}]}]}"#,
        ]).unwrap();
        let prepared = prepare_runtime_schema(&schemas);
        assert_eq!(
            string_table_with_prepared(&prepared, b"KYWD", b"XTXT", "addon_text"),
            "dlstrings"
        );
        assert_eq!(
            string_table_with_prepared(&prepared, b"KYWD", b"EDID", "editor_id"),
            "strings"
        );
        assert_eq!(
            string_table_with_prepared(&prepared, b"KYWD", b"XONE", ""),
            "dlstrings"
        );
        assert_eq!(
            string_table_with_prepared(&prepared, b"KYWD", b"XTWO", ""),
            "ilstrings"
        );
    }
}

use super::{
    DecodedField, DecodedRecord, PluginDiagnostics, ReadResult, Value,
    scanner::ScannedRecord,
    schema_format::{FieldSchema, Schema},
};
use crate::esm::load_order::LoadOrder;
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    sync::OnceLock,
};

#[cfg(test)]
thread_local! {
    /// Opt-in visits stay isolated to the regression's own test thread.
    static FIELD_DECODE_VISITS: std::cell::RefCell<Option<BTreeMap<(u32, String), usize>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Opt-in catalogue snapshots stay isolated to the regression's own test thread.
    static DEFERRED_CATALOGUE_BUILDS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Count deferred-recovery catalogue snapshots only while a focused test enables the probe.
#[cfg(test)]
fn record_deferred_catalogue_build() {
    DEFERRED_CATALOGUE_BUILDS.with(|builds| {
        if let Some(count) = builds.get() {
            builds.set(Some(count + 1));
        }
    });
}

/// Count actual value-decoder entries only while a focused test enables the probe.
#[cfg(test)]
fn record_decode_visit(field: &FieldSchema, diagnostics: &PluginDiagnostics) {
    FIELD_DECODE_VISITS.with(|visits| {
        if let Some(visits) = visits.borrow_mut().as_mut() {
            *visits
                .entry((
                    diagnostics.event_context.source_form_id.unwrap_or(0),
                    field.name.clone(),
                ))
                .or_default() += 1;
        }
    });
}

/// Per-file interpretation supplied by the shared load-order authority.
pub(crate) struct Context<'a> {
    pub order: &'a LoadOrder,
    pub plugin_index: usize,
    pub localized: bool,
    pub winning_types: Option<&'a HashMap<u32, [u8; 4]>>,
}

/// A first-pass link observation needed to preserve contextual replay event keys.
#[derive(Clone, Debug)]
pub(crate) struct DeferredLinkEvent {
    context: super::diagnostics::EventContext,
    reason: String,
}

/// Scalar snapshots preserve the original whole-record saturating difference policy.
#[derive(Clone, Copy, Debug, Default)]
struct DiagnosticCounts {
    record: u64,
    field: u64,
    unexpected: u64,
    link: u64,
    compatibility: u64,
}

impl DiagnosticCounts {
    /// Read counts without cloning examples, event journals or field bytes.
    fn read(diagnostics: &PluginDiagnostics) -> Self {
        Self {
            record: diagnostics.skipped_records,
            field: diagnostics.skipped_fields,
            unexpected: diagnostics.unexpected_subrecords,
            link: diagnostics.invalid_links,
            compatibility: diagnostics.compatibility_headers,
        }
    }

    /// Initial decoding only increments these counters.
    fn since(self, earlier: Self) -> Self {
        Self {
            record: self.record.saturating_sub(earlier.record),
            field: self.field.saturating_sub(earlier.field),
            unexpected: self.unexpected.saturating_sub(earlier.unexpected),
            link: self.link.saturating_sub(earlier.link),
            compatibility: self.compatibility.saturating_sub(earlier.compatibility),
        }
    }

    /// Replace one already-counted root with its contextual observations.
    fn replace_links(&mut self, original: u64, resolved: u64) {
        self.link = self.link.saturating_sub(original) + resolved;
    }

    /// Replace an earlier prefix contribution with its contextual observations.
    fn replace(&mut self, original: Self, resolved: Self) {
        self.record = self.record.saturating_sub(original.record) + resolved.record;
        self.field = self.field.saturating_sub(original.field) + resolved.field;
        self.unexpected = self.unexpected.saturating_sub(original.unexpected) + resolved.unexpected;
        self.link = self.link.saturating_sub(original.link) + resolved.link;
        self.compatibility =
            self.compatibility.saturating_sub(original.compatibility) + resolved.compatibility;
    }

    /// Sum cached or newly interpreted physical occurrences in source order.
    fn add(&mut self, other: Self) {
        self.record += other.record;
        self.field += other.field;
        self.unexpected += other.unexpected;
        self.link += other.link;
        self.compatibility += other.compatibility;
    }

    /// Install virtual full-record totals while retaining this replay's exact events.
    fn write(self, diagnostics: &mut PluginDiagnostics) {
        diagnostics.skipped_records = self.record;
        diagnostics.skipped_fields = self.field;
        diagnostics.unexpected_subrecords = self.unexpected;
        diagnostics.invalid_links = self.link;
        diagnostics.compatibility_headers = self.compatibility;
    }
}

/// Exact native and typed coordinates of a leaf left unresolved on its first visit.
#[derive(Clone, Debug)]
struct DeferredLeaf {
    field: FieldSchema,
    range: std::ops::Range<usize>,
    parent: Option<std::ops::Range<usize>>,
    path: Vec<usize>,
    depth: usize,
    context: super::diagnostics::EventContext,
}

/// Replay state is retained only for root fields that actually contain a deferred leaf.
#[derive(Clone, Debug)]
pub(crate) struct DeferredField {
    field_index: usize,
    source_position: usize,
    source_size: usize,
    field: FieldSchema,
    editor_id: String,
    leaves: Vec<DeferredLeaf>,
    links: Vec<DeferredLinkEvent>,
    before: DiagnosticCounts,
    baseline: DiagnosticCounts,
    rejections: Vec<(usize, [u8; 4])>,
    grammar: Option<Box<DeferredGrammar>>,
}

/// Descriptor identity includes extension singleton admission, which guards can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FieldBinding {
    Entry(usize),
    Extension(usize, bool),
}

/// Original outcomes allow grammar rematching without reinterpreting stable occurrences.
#[derive(Clone, Debug)]
struct GrammarOccurrence {
    binding: Option<FieldBinding>,
    field_index: Option<usize>,
    counts: DiagnosticCounts,
    interpreted: bool,
    editor_id: String,
}

/// Only deferred ownership anchors can alter later ordered descriptor bindings.
#[derive(Clone, Debug)]
struct DeferredGrammar {
    occurrences: Vec<GrammarOccurrence>,
    start: DiagnosticCounts,
    cache_from: usize,
}

/// One flattened subrecord occurrence, including its enclosing repeat groups.
struct Entry {
    field: FieldSchema,
    repeat_ranges: Vec<RepeatRange>,
    may_defer: bool,
}

/// An authored repeated group; explicit end markers prevent interior rewinds.
struct RepeatRange {
    start: usize,
    end: usize,
    terminated: bool,
}

/// Prepared schemas avoid re-expanding reusable definitions for every record.
pub(crate) struct PreparedRecord {
    entries: Vec<Entry>,
    unordered: bool,
    extensions: Vec<Entry>,
}

/// One prepared map shared by initial decoding, deferred replay and target validation.
pub(crate) type PreparedSchemas = HashMap<[u8; 4], PreparedRecord>;

/// Flatten only subrecord groups; byte structs and arrays stay typed and nested.
fn flatten(fields: &[FieldSchema], schema: &Schema, output: &mut Vec<Entry>) {
    for field in fields {
        let field = schema.resolve(field).expect("validated definition");
        if field.kind == "group" {
            let start = output.len();
            flatten(&field.fields, schema, output);
            let end = output.len();
            if field.repeat {
                for entry in &mut output[start..end] {
                    entry.repeat_ranges.push(RepeatRange {
                        start,
                        end,
                        terminated: field.repeat_terminated,
                    });
                }
            }
        } else if field.signature.is_some() {
            output.push(Entry {
                may_defer: schema_can_defer(&field, schema),
                field,
                repeat_ranges: Vec::new(),
            });
        }
    }
}

/// Prepare matched entries from a checked schema without changing the embedded singleton.
pub(crate) fn prepare_schema(schema: &Schema) -> PreparedSchemas {
    schema
        .records
        .iter()
        .map(|record| {
            let mut entries = Vec::new();
            flatten(&record.fields, schema, &mut entries);
            (
                record
                    .signature
                    .as_bytes()
                    .try_into()
                    .expect("checked signature"),
                PreparedRecord {
                    entries,
                    unordered: record.allow_unordered,
                    extensions: Vec::new(),
                },
            )
        })
        .collect()
}

/// Prepare independent extension leaves without changing ordinary field positions.
pub(crate) fn prepare_runtime_schema(schemas: &super::addons::RuntimeSchemas) -> PreparedSchemas {
    let mut prepared = prepare_schema(schemas.schema());
    for (signature, fields) in schemas.extensions() {
        let record = prepared
            .get_mut(signature)
            .expect("validated extension owner");
        flatten(fields, schemas.schema(), &mut record.extensions);
    }
    prepared
}

/// Prepare the build-validated embedded schema exactly once per process.
pub(crate) fn prepared() -> &'static PreparedSchemas {
    static PREPARED: OnceLock<PreparedSchemas> = OnceLock::new();
    PREPARED.get_or_init(|| prepare_schema(super::schema()))
}

/// The string bank of `signature` when the embedded schema types every occurrence of it in
/// `record_type` as a localized string, so the subrecord needs no context to resolve. The legacy
/// reader resolves exactly these subrecords to match the in-house reader's text.
pub(crate) fn localized_field_bank(
    record_type: &[u8; 4],
    signature: &[u8],
) -> Option<&'static str> {
    let record = prepared().get(record_type)?;
    let mut bank = None;
    for entry in record.entries.iter().filter(|entry| {
        entry
            .field
            .signature
            .as_deref()
            .is_some_and(|tag| tag.as_bytes() == signature)
    }) {
        if entry.field.kind != "lstring" {
            return None;
        }
        let this = match entry.field.string_table.as_deref() {
            Some("dlstrings") => "dlstrings",
            Some("ilstrings") => "ilstrings",
            _ => "strings",
        };
        if bank.is_some_and(|bank| bank != this) {
            return None;
        }
        bank = Some(this);
    }
    bank
}

/// Resolve a matched role's text bank from the same runtime schema as decoding.
pub(crate) fn string_table_with_prepared(
    prepared: &PreparedSchemas,
    record_type: &[u8; 4],
    signature: &[u8; 4],
    field_name: &str,
) -> &'static str {
    match prepared
        .get(record_type)
        .and_then(|record| {
            record
                .entries
                .iter()
                .chain(&record.extensions)
                .find(|entry| {
                    entry.field.name == field_name
                        && entry
                            .field
                            .signature
                            .as_deref()
                            .is_some_and(|tag| tag.as_bytes() == signature)
                })
        })
        .and_then(|entry| entry.field.string_table.as_deref())
        .unwrap_or("strings")
    {
        "dlstrings" => "dlstrings",
        "ilstrings" => "ilstrings",
        _ => "strings",
    }
}

/// Match a file-order occurrence without moving the cursor on an unknown tag.
fn match_field(
    record: &PreparedRecord,
    signature: &[u8; 4],
    cursor: usize,
    last: Option<usize>,
) -> Option<usize> {
    let matches = |index: usize| {
        let entry = &record.entries[index];
        entry
            .field
            .signature
            .as_deref()
            .is_some_and(|tag| tag.as_bytes() == signature)
            && entry.repeat_ranges.iter().all(|range| {
                !range.terminated
                    || index == range.start
                    || (cursor > range.start && cursor < range.end)
            })
    };
    if let Some(last) = last {
        for range in &record.entries[last].repeat_ranges {
            let (start, end) = (range.start, range.end);
            if cursor <= end {
                if range.terminated && cursor == end {
                    if matches(start) {
                        return Some(start);
                    }
                    continue;
                }
                if let Some(index) = (cursor..end).find(|&index| matches(index)) {
                    return Some(index);
                }
                if let Some(index) = (start..end).find(|&index| matches(index)) {
                    return Some(index);
                }
            }
        }
    }
    if let Some(index) = (cursor..record.entries.len()).find(|&index| matches(index)) {
        return Some(index);
    }
    if record.unordered {
        return (0..record.entries.len()).find(|&index| matches(index));
    }
    None
}

/// LAND's ATXT belongs inside its repeated layer group, rather than starting it.
fn layer_repeat_range(record: &PreparedRecord, index: usize) -> Option<(usize, usize)> {
    record.entries[index]
        .repeat_ranges
        .iter()
        .find(|range| index >= range.start && index < range.end)
        .map(|range| (range.start, range.end))
}

/// Framed prefix and whether its remaining payload was safe to scan.
struct FramedSubrecords<'a> {
    fields: Vec<([u8; 4], &'a [u8])>,
    complete: bool,
}

/// Frame subrecords with XXXX lengths; an unrecoverable boundary drops only this remainder.
fn subrecords<'a>(
    payload: &'a [u8],
    diagnostics: &mut PluginDiagnostics,
    id: u32,
) -> FramedSubrecords<'a> {
    let mut position = 0;
    let mut output = Vec::new();
    let mut extended = None;
    let mut complete = true;
    while position < payload.len() {
        diagnostics.set_field(format!("payload[byte:{position}]"));
        if payload.len() - position < 6 {
            complete = false;
            diagnostics.note("field", || {
                format!("record {id:08X}: incomplete subrecord header at {position}")
            });
            break;
        }
        let signature: [u8; 4] = payload[position..position + 4]
            .try_into()
            .expect("checked subrecord");
        diagnostics.set_field(format!(
            "{}[byte:{position}]",
            String::from_utf8_lossy(&signature)
        ));
        let short = u16::from_le_bytes(
            payload[position + 4..position + 6]
                .try_into()
                .expect("checked size"),
        ) as usize;
        position += 6;
        if signature == *b"XXXX" {
            if short != 4 || payload.len() - position < 4 || extended.is_some() {
                complete = false;
                diagnostics.note("field", || format!("record {id:08X}: malformed XXXX"));
                break;
            }
            extended = Some(u32::from_le_bytes(
                payload[position..position + 4]
                    .try_into()
                    .expect("checked XXXX"),
            ) as usize);
            position += 4;
            continue;
        }
        let size = extended.take().unwrap_or(short);
        if size > payload.len() - position {
            complete = false;
            diagnostics.note("field", || {
                format!(
                    "record {id:08X}: {} payload exceeds record boundary",
                    String::from_utf8_lossy(&signature)
                )
            });
            break;
        }
        output.push((signature, &payload[position..position + size]));
        position += size;
    }
    if extended.is_some() {
        complete = false;
        diagnostics.note("field", || format!("record {id:08X}: orphan XXXX"));
    }
    FramedSubrecords {
        fields: output,
        complete,
    }
}

/// Resolve a file-relative link; invalid optional values become null with one counted diagnostic.
fn remap(
    value: u32,
    context: &Context<'_>,
    optional: bool,
    diagnostics: &mut PluginDiagnostics,
) -> Result<u32, String> {
    diagnostics.link(value);
    if value == 0 {
        return Ok(0);
    }
    let masters = &context.order.metadata[context.plugin_index].masters;
    let index = (value >> 24) as usize;
    if optional && index > masters.len() {
        note_decode_link(
            diagnostics,
            format!("optional FormID {value:08X} exceeds master list"),
        );
        return Ok(0);
    }
    let owner = if index < masters.len() {
        crate::esm::load_order::normalize_plugin_name(&masters[index])
    } else {
        crate::esm::load_order::normalize_plugin_name(&context.order.names[context.plugin_index])
    };
    if let Some(&slot) = context.order.light.get(&owner) {
        if value & 0x00ff_ffff > 0xfff {
            if optional {
                note_decode_link(
                    diagnostics,
                    format!("optional light FormID {value:08X} is wider than 12 bits"),
                );
                return Ok(0);
            }
            return Err(format!(
                "record light FormID {value:08X} is wider than 12 bits"
            ));
        }
        return Ok(0xfe00_0000 | slot << 12 | value & 0xfff);
    }
    if let Some(slot) = context.order.normal.get(&owner) {
        return Ok(slot << 24 | value & 0x00ff_ffff);
    }
    if optional {
        note_decode_link(
            diagnostics,
            format!("optional FormID {value:08X} has absent owner {owner}"),
        );
        Ok(0)
    } else {
        Err(format!(
            "record FormID {value:08X} has absent owner {owner}"
        ))
    }
}

/// Cache catalogue dependency on prepared roots, including named nested definitions.
fn schema_can_defer(field: &FieldSchema, schema: &Schema) -> bool {
    if field.definition.is_some() {
        return schema_can_defer(
            &schema.resolve(field).expect("validated definition"),
            schema,
        );
    }
    field.decider.as_deref() == Some("inventory_owner_condition")
        || field
            .members
            .iter()
            .chain(&field.fields)
            .any(|child| schema_can_defer(child, schema))
}

/// A rematched EDID changes only selectors that explicitly consume the editor ID.
fn schema_uses_editor_id(field: &FieldSchema, schema: &Schema) -> bool {
    if field.definition.is_some() {
        return schema_uses_editor_id(
            &schema.resolve(field).expect("validated definition"),
            schema,
        );
    }
    field.decider.as_deref() == Some("gmst_value")
        || field
            .members
            .iter()
            .chain(&field.fields)
            .any(|child| schema_uses_editor_id(child, schema))
}

/// Trace native link failures only while a first-pass root is being interpreted.
fn note_decode_link(diagnostics: &mut PluginDiagnostics, reason: String) {
    if let Some(events) = &mut diagnostics.deferred_link_events {
        events.push(DeferredLinkEvent {
            context: diagnostics.event_context.clone(),
            reason: reason.clone(),
        });
    }
    diagnostics.note("link", || reason);
}

/// Use the same checked prepared entries for initial and catalogue-dependent decoding.
pub(crate) fn decode_with_prepared(
    scanned: ScannedRecord,
    schema: &Schema,
    prepared: &PreparedSchemas,
    context: &Context<'_>,
    diagnostics: &mut PluginDiagnostics,
) -> Result<DecodedRecord, String> {
    decode_with_identity(scanned, schema, prepared, context, diagnostics, &mut |_| {})
}

/// Observe only a successfully decoded GMST EDID, even if a later field rejects the record.
pub(crate) fn decode_with_identity(
    scanned: ScannedRecord,
    schema: &Schema,
    prepared: &PreparedSchemas,
    context: &Context<'_>,
    diagnostics: &mut PluginDiagnostics,
    identity: &mut impl FnMut(&str),
) -> Result<DecodedRecord, String> {
    let decoded =
        decode_with_identity_inner(scanned, schema, prepared, context, diagnostics, identity);
    diagnostics.deferred_contexts.clear();
    diagnostics.deferred_link_events = None;
    decoded
}

/// Keep temporary leaf traces scoped to the public wrapper, including rejected records.
fn decode_with_identity_inner(
    scanned: ScannedRecord,
    schema: &Schema,
    prepared: &PreparedSchemas,
    context: &Context<'_>,
    diagnostics: &mut PluginDiagnostics,
    identity: &mut impl FnMut(&str),
) -> Result<DecodedRecord, String> {
    let initial_counts = DiagnosticCounts::read(diagnostics);
    diagnostics.deferred_contexts.clear();
    diagnostics.deferred_link_events = None;
    diagnostics.set_record(
        scanned.source_record_offset,
        Some(scanned.record_type),
        Some(scanned.source_form_id),
        None,
        "decode",
    );
    diagnostics.set_field("record_header".into());
    // Original ownership stays strict; required headers retain supplier-self runtime compatibility.
    let source_identity = super::source_identity::resolve(
        &scanned.record_type,
        scanned.source_form_id,
        context.plugin_index,
        context.order,
    );
    let id = remap(scanned.source_form_id, context, false, diagnostics)?;
    diagnostics.event_context.form_id = Some(id);
    if id == 0 && !matches!(&scanned.record_type, b"GMST" | b"TES4") {
        return Err("non-GMST record has null FormID".into());
    }
    if source_identity.state == super::source_identity::SourceIdentityState::MasterIndexOutOfRange {
        // Count header projection, not final admission; later body/collision checks remain independent.
        diagnostics.note("compatibility", || {
            format!(
                "{} {:08X}: original owner unresolved; supplier-self runtime projection {id:08X}",
                String::from_utf8_lossy(&scanned.record_type),
                scanned.source_form_id
            )
        });
    }
    let before_groups = DiagnosticCounts::read(diagnostics);
    let cell = scanned
        .cell_form_id
        .map(|id| {
            diagnostics.set_field("cell_group".into());
            remap(id, context, true, diagnostics)
        })
        .transpose()?
        .filter(|&id| id != 0);
    let world = scanned
        .worldspace_form_id
        .map(|id| {
            diagnostics.set_field("worldspace_group".into());
            remap(id, context, true, diagnostics)
        })
        .transpose()?
        .filter(|&id| id != 0);
    let group_counts = DiagnosticCounts::read(diagnostics).since(before_groups);
    let topic = scanned
        .source_topic_form_id
        .map(|id| {
            diagnostics.set_field("GRUP7".into());
            remap(id, context, true, diagnostics)
        })
        .transpose()?
        .filter(|&id| id != 0);
    let record_schema = prepared.get(&scanned.record_type);
    let potential_deferred = record_schema.is_some_and(|record| {
        record
            .entries
            .iter()
            .chain(&record.extensions)
            .any(|entry| entry.may_defer)
    }) || schema
        .common_fields
        .iter()
        .any(|field| schema_can_defer(field, schema));
    let capture_ownership = record_schema.is_some_and(|record| {
        record.entries.iter().any(|entry| {
            entry.may_defer
                && entry.field.signature.as_deref().is_some_and(|signature| {
                    signature == "CTDA"
                        || scanned.record_type == *b"INFO" && signature == "TRDT"
                        || scanned.record_type == *b"LAND" && matches!(signature, "ATXT" | "BTXT")
                })
        })
    });
    let mut cursor = 0;
    let mut last = None;
    let mut fields = Vec::new();
    let mut deferred_fields = Vec::new();
    let mut field_positions = Vec::new();
    let mut source_counts = Vec::new();
    let mut source_entries = Vec::new();
    let mut source_bindings = Vec::new();
    let mut source_interpreted = Vec::new();
    let mut source_editors = Vec::new();
    let mut rejections = Vec::new();
    let mut observed_rejections = 0;
    let mut seen_extensions = std::collections::BTreeSet::new();
    let mut rejected_fields = Vec::new();
    let mut editor_id = String::new();
    // CIS leaves belong to the preceding CTDA, not the preceding surviving one.
    let mut rejected_condition_strings = false;
    // Response members must not move beneath an earlier surviving TRDT.
    let mut rejected_info_response: Option<(usize, usize)> = None;
    // Weights belong to their physical layer anchor, including an anchor that was rejected.
    let mut rejected_land_layer: Option<(usize, usize)> = None;
    diagnostics.set_field("payload_framing".into());
    let framed = subrecords(&scanned.payload, diagnostics, scanned.source_form_id);
    let link_kind = |source_id| {
        let id = remap(source_id, context, true, &mut PluginDiagnostics::default()).ok()?;
        context.winning_types?.get(&id).copied()
    };
    for (position, &(signature, bytes)) in framed.fields.iter().enumerate() {
        if potential_deferred {
            for &signature in &rejected_fields[observed_rejections..] {
                rejections.push((position - 1, signature));
            }
            observed_rejections = rejected_fields.len();
        }
        if capture_ownership {
            source_counts.push(DiagnosticCounts::read(diagnostics));
            source_entries.push(None);
            source_bindings.push(None);
            source_interpreted.push(false);
            source_editors.push(editor_id.clone());
        }
        diagnostics.deferred_contexts.clear();
        diagnostics.deferred_link_events = None;
        let mut before = DiagnosticCounts::read(diagnostics).since(initial_counts);
        before.link = before.link.saturating_sub(group_counts.link);
        diagnostics.set_field(format!(
            "{}[source:{position}]",
            String::from_utf8_lossy(&signature)
        ));
        let land_anchor =
            scanned.record_type == *b"LAND" && matches!(&signature, b"BTXT" | b"ATXT");
        if land_anchor {
            rejected_land_layer = None;
        } else if signature == *b"VTXT" && rejected_land_layer.is_some() {
            if !rejected_fields.contains(&signature) {
                rejected_fields.push(signature);
            }
            diagnostics.note("field", || {
                format!("LAND {id:08X} VTXT: weights belong to a rejected physical layer anchor")
            });
            continue;
        }
        let condition_string = matches!(&signature, b"CIS1" | b"CIS2");
        if condition_string && rejected_condition_strings {
            if !rejected_fields.contains(&signature) {
                rejected_fields.push(signature);
            }
            diagnostics.note("field", || {
                format!(
                    "{} {id:08X} {}: string belongs to rejected CTDA",
                    String::from_utf8_lossy(&scanned.record_type),
                    String::from_utf8_lossy(&signature)
                )
            });
            continue;
        }
        if !condition_string {
            // A new source field ends this rejected condition's contiguous string block.
            rejected_condition_strings = false;
        }
        let selection = super::deciders::Context {
            record_type: scanned.record_type,
            form_version: scanned.form_version,
            editor_id: &editor_id,
            preceding_subrecords: &framed.fields[..position],
            parent_payload: None,
            link_record_type: context
                .winning_types
                .map(|_| &link_kind as &dyn Fn(u32) -> Option<[u8; 4]>),
        };
        let Some(record_schema) = record_schema else {
            if let Some(common) = schema.common_fields.iter().find(|field| {
                field
                    .signature
                    .as_deref()
                    .is_some_and(|tag| tag.as_bytes() == signature)
            }) {
                let common = schema.resolve(common)?;
                diagnostics.deferred_link_events = (context.winning_types.is_none()
                    && schema_can_defer(&common, schema))
                .then(Vec::new);
                match value(&common, bytes, schema, context, diagnostics, &selection, 0) {
                    Ok((value, canonical_bytes)) => {
                        capture_deferred_field(
                            &mut deferred_fields,
                            CaptureField {
                                field_index: fields.len(),
                                source_position: position,
                                field: &common,
                                bytes,
                                selection: &selection,
                                before,
                            },
                            &value,
                            schema,
                            diagnostics,
                        )?;
                        fields.push(DecodedField {
                            signature,
                            localized_bank: selected_localized_bank(
                                &common, bytes, schema, &selection, &value,
                            ),
                            name: common.name,
                            value,
                            canonical_bytes,
                            opaque: false,
                        });
                        if capture_ownership {
                            field_positions.push((position, fields.len() - 1));
                        }
                    }
                    Err(error) => {
                        if !rejected_fields.contains(&signature) {
                            rejected_fields.push(signature);
                        }
                        diagnostics.note("field", || {
                            format!(
                                "{} {id:08X} {}: {error}",
                                String::from_utf8_lossy(&scanned.record_type),
                                String::from_utf8_lossy(&signature)
                            )
                        });
                    }
                }
                continue;
            }
            fields.push(DecodedField {
                signature,
                name: String::from_utf8_lossy(&signature).into_owned(),
                value: Value::Bytes(bytes.to_vec()),
                canonical_bytes: bytes.to_vec(),
                opaque: true,
                localized_bank: None,
            });
            if capture_ownership {
                field_positions.push((position, fields.len() - 1));
            }
            continue;
        };
        if let Some((extension_index, entry)) =
            record_schema
                .extensions
                .iter()
                .enumerate()
                .find(|(_, entry)| {
                    entry
                        .field
                        .signature
                        .as_deref()
                        .is_some_and(|tag| tag.as_bytes() == signature)
                })
        {
            let field_schema = &entry.field;
            diagnostics.deferred_link_events =
                (context.winning_types.is_none() && entry.may_defer).then(Vec::new);
            let duplicate = !field_schema.repeat && !seen_extensions.insert(signature);
            if capture_ownership {
                source_bindings[position] =
                    Some(FieldBinding::Extension(extension_index, duplicate));
            }
            let decoded = if duplicate {
                Err("duplicate singleton extension".to_owned())
            } else {
                if capture_ownership {
                    source_interpreted[position] = true;
                }
                value(
                    field_schema,
                    bytes,
                    schema,
                    context,
                    diagnostics,
                    &selection,
                    0,
                )
            };
            match decoded {
                Ok((value, canonical_bytes)) => {
                    capture_deferred_field(
                        &mut deferred_fields,
                        CaptureField {
                            field_index: fields.len(),
                            source_position: position,
                            field: field_schema,
                            bytes,
                            selection: &selection,
                            before,
                        },
                        &value,
                        schema,
                        diagnostics,
                    )?;
                    let localized_bank =
                        selected_localized_bank(field_schema, bytes, schema, &selection, &value);
                    if signature == *b"EDID"
                        && let Value::String(name) = &value
                    {
                        editor_id.clone_from(name);
                    }
                    fields.push(DecodedField {
                        signature,
                        localized_bank,
                        name: field_schema.name.clone(),
                        value,
                        canonical_bytes,
                        opaque: false,
                    });
                    if capture_ownership {
                        field_positions.push((position, fields.len() - 1));
                    }
                }
                Err(error) => {
                    if field_schema.reject_record_on_error && scanned.flags & 0x20 == 0 {
                        return Err(format!(
                            "{} {id:08X} mandatory {}: {error}",
                            String::from_utf8_lossy(&scanned.record_type),
                            field_schema.name
                        ));
                    }
                    if !rejected_fields.contains(&signature) {
                        rejected_fields.push(signature);
                    }
                    diagnostics.note("field", || {
                        format!(
                            "{} {id:08X} {}: {error}",
                            String::from_utf8_lossy(&scanned.record_type),
                            String::from_utf8_lossy(&signature)
                        )
                    });
                }
            }
            continue;
        }
        let Some(index) = match_field(record_schema, &signature, cursor, last) else {
            if land_anchor {
                rejected_land_layer = record_schema
                    .entries
                    .iter()
                    .position(|entry| {
                        entry
                            .field
                            .signature
                            .as_deref()
                            .is_some_and(|tag| tag.as_bytes() == signature)
                    })
                    .and_then(|index| layer_repeat_range(record_schema, index));
            }
            let known = record_schema.entries.iter().any(|entry| {
                entry
                    .field
                    .signature
                    .as_deref()
                    .is_some_and(|tag| tag.as_bytes() == signature)
            });
            if known && signature == *b"CTDA" {
                rejected_condition_strings = true;
            }
            if known && !rejected_fields.contains(&signature) {
                rejected_fields.push(signature);
            }
            diagnostics.note("unexpected", || {
                format!(
                    "{} {id:08X}: unexpected/out-of-order {}",
                    String::from_utf8_lossy(&scanned.record_type),
                    String::from_utf8_lossy(&signature)
                )
            });
            if !known {
                fields.push(DecodedField {
                    signature,
                    name: String::from_utf8_lossy(&signature).into_owned(),
                    value: Value::Bytes(bytes.to_vec()),
                    canonical_bytes: bytes.to_vec(),
                    opaque: true,
                    localized_bank: None,
                });
                if capture_ownership {
                    field_positions.push((position, fields.len() - 1));
                }
            }
            continue;
        };
        let field_schema = &record_schema.entries[index].field;
        if capture_ownership {
            source_entries[position] = Some(index);
            source_bindings[position] = Some(FieldBinding::Entry(index));
        }
        diagnostics.deferred_link_events = (context.winning_types.is_none()
            && record_schema.entries[index].may_defer)
            .then(Vec::new);
        cursor = index + usize::from(!field_schema.repeat);
        last = Some(index);
        if let Some((start, end)) = rejected_land_layer
            && (index < start || index >= end)
        {
            // Only a real matched group boundary or a new physical anchor ends ownership.
            rejected_land_layer = None;
        }
        if let Some((start, end)) = rejected_info_response {
            if index > start && index < end {
                if !rejected_fields.contains(&signature) {
                    rejected_fields.push(signature);
                }
                diagnostics.note("field", || {
                    format!(
                        "{} {id:08X} {}: member belongs to rejected TRDT",
                        String::from_utf8_lossy(&scanned.record_type),
                        String::from_utf8_lossy(&signature)
                    )
                });
                continue;
            }
            // A new response anchor or matched field outside this group ends it.
            rejected_info_response = None;
        }
        if capture_ownership {
            source_interpreted[position] = true;
        }
        match value(
            field_schema,
            bytes,
            schema,
            context,
            diagnostics,
            &selection,
            0,
        ) {
            Ok((value, canonical_bytes)) => {
                capture_deferred_field(
                    &mut deferred_fields,
                    CaptureField {
                        field_index: fields.len(),
                        source_position: position,
                        field: field_schema,
                        bytes,
                        selection: &selection,
                        before,
                    },
                    &value,
                    schema,
                    diagnostics,
                )?;
                let localized_bank =
                    selected_localized_bank(field_schema, bytes, schema, &selection, &value);
                if signature == *b"EDID"
                    && let Value::String(name) = &value
                {
                    editor_id.clone_from(name);
                    if scanned.record_type == *b"GMST" {
                        identity(name);
                    }
                }
                fields.push(DecodedField {
                    signature,
                    localized_bank,
                    name: field_schema.name.clone(),
                    value,
                    canonical_bytes,
                    opaque: false,
                });
                if capture_ownership {
                    field_positions.push((position, fields.len() - 1));
                }
            }
            Err(error) => {
                if land_anchor {
                    rejected_land_layer = layer_repeat_range(record_schema, index);
                }
                if signature == *b"CTDA" {
                    rejected_condition_strings = true;
                }
                if scanned.record_type == *b"INFO" && signature == *b"TRDT" {
                    rejected_info_response = record_schema.entries[index]
                        .repeat_ranges
                        .iter()
                        .find(|range| range.start == index)
                        .map(|range| (range.start, range.end));
                }
                if field_schema.reject_record_on_error && scanned.flags & 0x20 == 0 {
                    return Err(format!(
                        "{} {id:08X} mandatory {}: {error}",
                        String::from_utf8_lossy(&scanned.record_type),
                        field_schema.name
                    ));
                }
                if !rejected_fields.contains(&signature) {
                    rejected_fields.push(signature);
                }
                diagnostics.note("field", || {
                    format!(
                        "{} {id:08X} {}: {error}",
                        String::from_utf8_lossy(&scanned.record_type),
                        String::from_utf8_lossy(&signature)
                    )
                });
            }
        }
    }
    diagnostics.deferred_contexts.clear();
    diagnostics.deferred_link_events = None;
    let mut baseline = DiagnosticCounts::read(diagnostics).since(initial_counts);
    baseline.link = baseline.link.saturating_sub(group_counts.link);
    for deferred in &mut deferred_fields {
        deferred.baseline = baseline;
    }
    if let Some(first) = deferred_fields.first_mut() {
        for &signature in &rejected_fields[observed_rejections..] {
            rejections.push((framed.fields.len() - 1, signature));
        }
        first.rejections = rejections;
    }
    if capture_ownership && let Some(prepared) = record_schema {
        source_counts.push(DiagnosticCounts::read(diagnostics));
        let cache_from = deferred_fields
            .iter()
            .filter(|deferred| {
                source_entries[deferred.source_position].is_some_and(|index| {
                    let signature = prepared.entries[index].field.signature.as_deref();
                    signature == Some("CTDA")
                        || scanned.record_type == *b"INFO" && signature == Some("TRDT")
                        || scanned.record_type == *b"LAND"
                            && matches!(signature, Some("ATXT" | "BTXT"))
                })
            })
            .map(|deferred| deferred.field_index)
            .min();
        if let Some(cache_from) = cache_from {
            let mut start = source_counts[0].since(initial_counts);
            start.link = start.link.saturating_sub(group_counts.link);
            let occurrences = source_bindings
                .into_iter()
                .enumerate()
                .map(|(position, binding)| GrammarOccurrence {
                    binding,
                    field_index: field_positions
                        .binary_search_by_key(&position, |(source, _)| *source)
                        .ok()
                        .map(|index| field_positions[index].1),
                    counts: source_counts[position + 1].since(source_counts[position]),
                    interpreted: source_interpreted[position],
                    editor_id: source_editors[position].clone(),
                })
                .collect();
            deferred_fields[0].grammar = Some(Box::new(DeferredGrammar {
                occurrences,
                start,
                cache_from,
            }));
        }
    }
    Ok(DecodedRecord {
        form_id: id,
        source_form_id: scanned.source_form_id,
        record_type: scanned.record_type,
        flags: scanned.flags,
        version_control: scanned.version_control,
        form_version: scanned.form_version,
        header_unknown: scanned.header_unknown,
        load_order: context.plugin_index as u32,
        cell_form_id: cell,
        worldspace_form_id: world,
        topic_form_id: topic,
        source_topic_form_id: scanned.source_topic_form_id,
        topic_group_offset: scanned.topic_group_offset,
        source_record_offset: scanned.source_record_offset,
        fields,
        deferred_fields,
        rejected_fields,
        payload_complete: framed.complete,
        raw_payload: scanned.payload,
        supported: record_schema.is_some(),
    })
}

/// Obtain a primitive's binary width; variable-sized kinds supply explicit lengths.
fn width(field: &FieldSchema) -> Option<usize> {
    super::schema_format::field_width(field)
}

/// Resolve each member before choosing its extent, including omitted optional tails.
fn member_span(
    member: &FieldSchema,
    payload_size: usize,
    schema: &Schema,
) -> Result<Option<(FieldSchema, std::ops::Range<usize>)>, String> {
    let member = schema.resolve(member)?;
    if member.optional && member.offset >= payload_size {
        return Ok(None);
    }
    let size = width(&member)
        .or_else(|| payload_size.checked_sub(member.offset))
        .ok_or("member offset out of bounds")?;
    let end = member
        .offset
        .checked_add(size)
        .ok_or("member extent overflow")?;
    if end > payload_size {
        return Err(format!("{} member out of bounds", member.name));
    }
    let range = member.offset..end;
    Ok(Some((member, range)))
}

/// Decode and target validation must agree on a singleton's offset and resolved width.
fn array_element(
    field: &FieldSchema,
    stride: usize,
    schema: &Schema,
) -> Result<FieldSchema, String> {
    if field.members.len() == 1 && field.members[0].offset == 0 {
        schema.resolve(&field.members[0])
    } else {
        Ok(FieldSchema {
            kind: "struct".into(),
            size: Some(stride),
            members: field.members.clone(),
            ..Default::default()
        })
    }
}

/// Infer an array stride only after expanding its first member definition.
fn array_stride(field: &FieldSchema, schema: &Schema) -> Result<usize, String> {
    let declared = field
        .count
        .as_ref()
        .and_then(|rule| rule.get("stride"))
        .and_then(|value| value.as_u64())
        .map(|value| usize::try_from(value).map_err(|_| "array stride overflow".to_owned()))
        .transpose()?;
    let stride = declared
        .or(field.size)
        .or_else(|| {
            field
                .members
                .first()
                .and_then(|member| schema.resolve(member).ok())
                .as_ref()
                .and_then(width)
        })
        .ok_or("array has no stride")?;
    if stride == 0 {
        return Err("zero array stride".into());
    }
    Ok(stride)
}

/// Retain the most-specific selected bank without changing the serialized value contract.
fn selected_localized_bank(
    field: &FieldSchema,
    bytes: &[u8],
    schema: &Schema,
    selection: &super::deciders::Context<'_>,
    decoded: &Value,
) -> Option<String> {
    if !matches!(decoded, Value::LocalizedString(_)) {
        return None;
    }
    let mut field = field.clone();
    let mut bank = None;
    for _ in 0..32 {
        field = schema.resolve(&field).ok()?;
        if field.string_table.is_some() {
            bank.clone_from(&field.string_table);
        }
        if field.kind == "union" {
            field = decide(&field, bytes, schema, selection).ok()??.into_owned();
        } else {
            return (field.kind == "lstring").then(|| bank.unwrap_or_else(|| "strings".into()));
        }
    }
    None
}

/// Choose a schema alternative using a small named Rust policy, never external tool code.
fn decide<'a>(
    field: &'a FieldSchema,
    bytes: &[u8],
    schema: &Schema,
    selection: &super::deciders::Context<'_>,
) -> Result<Option<Cow<'a, FieldSchema>>, String> {
    // Named selectors inspect kinds, names and widths too. Resolve alternatives
    // before dispatch, while ordinary built-in leaves remain borrowed.
    let field = if field
        .fields
        .iter()
        .any(|alternative| alternative.definition.is_some())
    {
        let mut resolved = field.clone();
        resolved.fields = field
            .fields
            .iter()
            .map(|alternative| schema.resolve(alternative))
            .collect::<Result<Vec<_>, _>>()?;
        Cow::Owned(resolved)
    } else {
        Cow::Borrowed(field)
    };
    let decider = field.decider.as_deref().ok_or("union has no decider")?;
    let target = match decider {
        "gmst_value" => match selection.editor_id.as_bytes().first() {
            Some(b'f' | b'F') => Some("f32"),
            Some(b'i' | b'I') => Some("i32"),
            Some(b'b' | b'B') => Some("u32"),
            Some(b's' | b'S') => Some("lstring"),
            _ => return Err("GMST EditorID does not select a value kind".into()),
        },
        "localized_string" => Some("lstring"),
        "legacy_linked_reference" | "cell_grid" | "movement_speeds" | "water_visual" | "size" => {
            None
        }
        _ => {
            let index = match super::deciders::select(decider, &field, bytes, selection)? {
                super::deciders::Selection::Alternative(index) => index,
                super::deciders::Selection::Deferred => return Ok(None),
            };
            if index >= field.fields.len() {
                return Err(format!("{decider} selected absent alternative {index}"));
            }
            return Ok(Some(match field {
                Cow::Borrowed(field) => Cow::Borrowed(&field.fields[index]),
                Cow::Owned(mut field) => Cow::Owned(field.fields.remove(index)),
            }));
        }
    };
    let index = field
        .fields
        .iter()
        .position(|alternative| {
            target.map_or_else(
                || {
                    alternative.size == Some(bytes.len())
                        || alternative.sizes.contains(&bytes.len())
                },
                |kind| alternative.kind == kind,
            )
        })
        .ok_or_else(|| format!("no {decider} alternative accepts {} bytes", bytes.len()))?;
    Ok(Some(match field {
        Cow::Borrowed(field) => Cow::Borrowed(&field.fields[index]),
        Cow::Owned(mut field) => Cow::Owned(field.fields.remove(index)),
    }))
}

/// Interpret one value within checked bytes, rewriting only typed link positions.
fn value(
    field: &FieldSchema,
    bytes: &[u8],
    schema: &Schema,
    context: &Context<'_>,
    diagnostics: &mut PluginDiagnostics,
    selection: &super::deciders::Context<'_>,
    depth: usize,
) -> Result<(Value, Vec<u8>), String> {
    let previous = diagnostics.push_field(&field.name);
    let result = value_inner(field, bytes, schema, context, diagnostics, selection, depth);
    diagnostics.pop_field(previous);
    result
}

/// Decode a scoped member; the wrapper restores diagnostic paths on every return.
fn value_inner(
    field: &FieldSchema,
    bytes: &[u8],
    schema: &Schema,
    context: &Context<'_>,
    diagnostics: &mut PluginDiagnostics,
    selection: &super::deciders::Context<'_>,
    depth: usize,
) -> Result<(Value, Vec<u8>), String> {
    #[cfg(test)]
    record_decode_visit(field, diagnostics);
    if depth >= 32 {
        return Err("value nesting exceeds 32".into());
    }
    if field.definition.is_some() {
        return value(
            schema.resolve_borrowed(field)?.as_ref(),
            bytes,
            schema,
            context,
            diagnostics,
            selection,
            depth + 1,
        );
    }
    if field.size.is_some_and(|size| size != bytes.len()) && field.kind != "array"
        || !field.sizes.is_empty() && !field.sizes.contains(&bytes.len())
    {
        return Err(format!(
            "length {} does not match schema size {:?}/{:?}",
            bytes.len(),
            field.size,
            field.sizes
        ));
    }
    if let Some(layout) = super::deciders::layout(field, bytes)? {
        return value(
            &layout,
            bytes,
            schema,
            context,
            diagnostics,
            selection,
            depth + 1,
        );
    }
    if field.kind == "union" {
        let Some(alternative) = decide(field, bytes, schema, selection)? else {
            if context.winning_types.is_none() {
                diagnostics
                    .deferred_contexts
                    .push(diagnostics.event_context.clone());
            }
            return Ok((Value::Deferred(bytes.to_vec()), bytes.to_vec()));
        };
        return value(
            &alternative,
            bytes,
            schema,
            context,
            diagnostics,
            selection,
            depth + 1,
        );
    }
    if field.decider.as_deref() == Some("alternate_textures") {
        return alternate_textures(bytes, context, diagnostics);
    }
    if field.decider.as_deref() == Some("vmad") {
        return super::vmad::decode(bytes, &selection.record_type, |id| {
            remap(id, context, true, diagnostics)
        });
    }
    if let Some(width) = width(field)
        && matches!(
            field.kind.as_str(),
            "u8" | "i8" | "u16" | "i16" | "u32" | "i32" | "u64" | "i64" | "f32" | "form_id"
        )
        && bytes.len() != width
    {
        return Err(format!("expected {width} bytes, got {}", bytes.len()));
    }
    let unsigned = || {
        let mut padded = [0u8; 8];
        padded[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(padded)
    };
    let decoded = match field.kind.as_str() {
        "u8" | "u16" | "u32" | "u64" => Value::Unsigned(unsigned()),
        "i8" => Value::Signed(i64::from(bytes[0] as i8)),
        "i16" => Value::Signed(i64::from(i16::from_le_bytes(
            bytes.try_into().expect("checked i16"),
        ))),
        "i32" => Value::Signed(i64::from(i32::from_le_bytes(
            bytes.try_into().expect("checked i32"),
        ))),
        "i64" => Value::Signed(i64::from_le_bytes(bytes.try_into().expect("checked i64"))),
        "f32" => {
            let number = f32::from_le_bytes(bytes.try_into().expect("checked f32"));
            if !number.is_finite() {
                return Err("non-finite float".into());
            }
            Value::Float(number)
        }
        "form_id" => {
            let resolved = remap(
                u32::from_le_bytes(bytes.try_into().expect("checked FormID")),
                context,
                true,
                diagnostics,
            )?;
            return Ok((Value::FormId(resolved), resolved.to_le_bytes().to_vec()));
        }
        "zstring" => {
            let end = bytes
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(bytes.len());
            Value::String(decode_string(&bytes[..end]))
        }
        "lstring" => {
            if context.localized {
                if bytes.len() != 4 {
                    return Err("localized string ID must be four bytes".into());
                }
                Value::LocalizedString(u32::from_le_bytes(
                    bytes.try_into().expect("checked localized ID"),
                ))
            } else {
                let end = bytes
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(bytes.len());
                Value::String(decode_string(&bytes[..end]))
            }
        }
        "bytes" => Value::Bytes(bytes.to_vec()),
        "struct" => {
            schema.check_member_overlaps(field, Some(bytes.len()))?;
            let mut values = Vec::new();
            let mut canonical = bytes.to_vec();
            for member in &field.members {
                let Some((member, range)) = member_span(member, bytes.len(), schema)? else {
                    continue;
                };
                let slice = &bytes[range.clone()];
                let (decoded, encoded) = value(
                    &member,
                    slice,
                    schema,
                    context,
                    diagnostics,
                    &super::deciders::Context {
                        parent_payload: Some(bytes),
                        ..*selection
                    },
                    depth + 1,
                )?;
                canonical[range].copy_from_slice(&encoded);
                values.push((member.name, decoded));
            }
            return Ok((Value::Struct(values), canonical));
        }
        "array" => {
            return array(
                field,
                bytes,
                schema,
                context,
                diagnostics,
                selection,
                depth + 1,
            );
        }
        kind => return Err(format!("field kind {kind} is not a value")),
    };
    Ok((decoded, bytes.to_vec()))
}

/// Ordinary Skyrim plugin text uses Windows-1252; VMAD owns its separate UTF-8 decoder.
pub(crate) fn decode_string(bytes: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '€', '\u{0081}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{008d}', 'Ž',
        '\u{008f}', '\u{0090}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ',
        '\u{009d}', 'ž', 'Ÿ',
    ];
    bytes
        .iter()
        .map(|&byte| {
            if (0x80..0xa0).contains(&byte) {
                HIGH[usize::from(byte - 0x80)]
            } else {
                char::from(byte)
            }
        })
        .collect()
}

/// Decode fixed, remaining or prefix-counted arrays with checked arithmetic.
fn array(
    field: &FieldSchema,
    bytes: &[u8],
    schema: &Schema,
    context: &Context<'_>,
    diagnostics: &mut PluginDiagnostics,
    selection: &super::deciders::Context<'_>,
    depth: usize,
) -> Result<(Value, Vec<u8>), String> {
    if field.decider.as_deref() == Some("alternate_textures") {
        return alternate_textures(bytes, context, diagnostics);
    }
    let count_rule = field.count.as_ref();
    let kind = count_rule
        .and_then(|rule| rule.get("kind"))
        .and_then(|value| value.as_str())
        .unwrap_or("remaining");
    let stride = array_stride(field, schema)?;
    let (offset, count) = match kind {
        "remaining" => (0, bytes.len() / stride),
        "fixed" => (
            0,
            count_rule
                .and_then(|rule| rule.get("value"))
                .and_then(|value| value.as_u64())
                .ok_or("fixed array lacks count")? as usize,
        ),
        "prefix_u32" => {
            let prefix = bytes.get(..4).ok_or("array lacks count prefix")?;
            (
                4,
                u32::from_le_bytes(prefix.try_into().expect("checked count")) as usize,
            )
        }
        _ => return Err(format!("unsupported array count rule {kind}")),
    };
    if count > super::schema_format::MAX_ARRAY_ELEMENTS {
        return Err("array exceeds one million elements".into());
    }
    if count
        .checked_mul(stride)
        .and_then(|size| size.checked_add(offset))
        != Some(bytes.len())
    {
        return Err("array extent/count mismatch".into());
    }
    let element = array_element(field, stride, schema)?;
    if field.decider.is_none()
        && let Some(compact) = super::compact_array::try_decode(
            &element,
            &bytes[offset..],
            count,
            stride,
            schema,
            depth,
        )?
    {
        return Ok((Value::CompactArray(compact), bytes.to_vec()));
    }
    let mut canonical = bytes.to_vec();
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let previous = diagnostics.push_field(&format!("[{index}]"));
        let start = offset + index * stride;
        let (decoded, encoded) = value(
            &element,
            &bytes[start..start + stride],
            schema,
            context,
            diagnostics,
            selection,
            depth,
        )?;
        canonical[start..start + stride].copy_from_slice(&encoded);
        diagnostics.pop_field(previous);
        values.push(decoded);
    }
    Ok((Value::Array(values), canonical))
}

/// Alternate textures have counted length-prefixed names, then a TXST link and index.
fn alternate_textures(
    bytes: &[u8],
    context: &Context<'_>,
    diagnostics: &mut PluginDiagnostics,
) -> Result<(Value, Vec<u8>), String> {
    let read_u32 = |position: usize| -> Result<u32, String> {
        bytes
            .get(
                position
                    ..position
                        .checked_add(4)
                        .ok_or("alternate texture offset overflow")?,
            )
            .map(|slice| {
                u32::from_le_bytes(slice.try_into().expect("checked alternate texture integer"))
            })
            .ok_or_else(|| "truncated alternate textures".into())
    };
    let count = read_u32(0)? as usize;
    if count > bytes.len() / 12 {
        return Err("alternate texture count exceeds payload".into());
    }
    let mut position = 4;
    let mut canonical = bytes.to_vec();
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        let size = read_u32(position)? as usize;
        position += 4;
        let end = position
            .checked_add(size)
            .ok_or("alternate texture name overflow")?;
        let name_bytes = bytes
            .get(position..end)
            .ok_or("truncated alternate texture name")?;
        // The counted name is NUL-terminated inside its declared length.
        let name_end = name_bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name_bytes.len());
        let name = decode_string(&name_bytes[..name_end]);
        position = end;
        let link = remap(read_u32(position)?, context, true, diagnostics)?;
        canonical[position..position + 4].copy_from_slice(&link.to_le_bytes());
        let index = read_u32(position + 4)?;
        position += 8;
        values.push(Value::Struct(vec![
            ("name".into(), Value::String(name)),
            ("texture_set".into(), Value::FormId(link)),
            ("index".into(), Value::Unsigned(u64::from(index))),
        ]));
    }
    if position != bytes.len() {
        return Err("trailing alternate texture bytes".into());
    }
    Ok((Value::Array(values), canonical))
}

/// Add diagnostics newly discovered by contextual replay with the least source examples.
fn merge_new_diagnostics(
    target: &mut PluginDiagnostics,
    baseline: &PluginDiagnostics,
    resolved: PluginDiagnostics,
) {
    target
        .event_log
        .merge_new_from(&resolved.event_log, &baseline.event_log);
    let additions = [
        (
            "record",
            resolved
                .skipped_records
                .saturating_sub(baseline.skipped_records),
        ),
        (
            "field",
            resolved
                .skipped_fields
                .saturating_sub(baseline.skipped_fields),
        ),
        (
            "unexpected",
            resolved
                .unexpected_subrecords
                .saturating_sub(baseline.unexpected_subrecords),
        ),
        (
            "link",
            resolved
                .invalid_links
                .saturating_sub(baseline.invalid_links),
        ),
        (
            "compatibility",
            resolved
                .compatibility_headers
                .saturating_sub(baseline.compatibility_headers),
        ),
    ];
    for (category, count) in additions {
        if count == 0 {
            continue;
        }
        match category {
            "record" => target.skipped_records += count,
            "field" => target.skipped_fields += count,
            "unexpected" => target.unexpected_subrecords += count,
            "compatibility" => target.compatibility_headers += count,
            _ => target.invalid_links += count,
        }
        target.merge_summary_category(&resolved, category);
    }
}

/// Borrow the exact first-pass selector state while recording only deferred coordinates.
struct CaptureField<'a, 'b> {
    field_index: usize,
    source_position: usize,
    field: &'a FieldSchema,
    bytes: &'a [u8],
    selection: &'b super::deciders::Context<'a>,
    before: DiagnosticCounts,
}

/// Keep leaf paths and tiny selector metadata; ordinary fields retain no replay state.
fn capture_deferred_field(
    output: &mut Vec<DeferredField>,
    capture: CaptureField<'_, '_>,
    decoded: &Value,
    schema: &Schema,
    diagnostics: &mut PluginDiagnostics,
) -> Result<(), String> {
    if !value_contains_deferred(decoded) {
        return Ok(());
    }
    let contexts = std::mem::take(&mut diagnostics.deferred_contexts);
    let mut collector = DeferredCollector {
        schema,
        bytes: capture.bytes,
        selection: *capture.selection,
        contexts: contexts.iter(),
        leaves: Vec::new(),
        path: Vec::new(),
    };
    collector.walk(decoded, capture.field, 0..capture.bytes.len(), None, 0)?;
    if collector.contexts.next().is_some() || collector.leaves.is_empty() {
        return Err("deferred leaf context mismatch".into());
    }
    output.push(DeferredField {
        field_index: capture.field_index,
        source_position: capture.source_position,
        source_size: capture.bytes.len(),
        field: capture.field.clone(),
        editor_id: capture.selection.editor_id.into(),
        leaves: collector.leaves,
        links: diagnostics.deferred_link_events.take().unwrap_or_default(),
        before: capture.before,
        baseline: DiagnosticCounts::default(),
        rejections: Vec::new(),
        grammar: None,
    });
    Ok(())
}

/// Follow the already-decoded shape without entering the value decoder a second time.
struct DeferredCollector<'a, 'b> {
    schema: &'a Schema,
    bytes: &'a [u8],
    selection: super::deciders::Context<'a>,
    contexts: std::slice::Iter<'b, super::diagnostics::EventContext>,
    leaves: Vec<DeferredLeaf>,
    path: Vec<usize>,
}

impl DeferredCollector<'_, '_> {
    /// Mirror definition, layout, union and ordered member spans used on the first visit.
    fn walk(
        &mut self,
        decoded: &Value,
        field: &FieldSchema,
        range: std::ops::Range<usize>,
        parent: Option<std::ops::Range<usize>>,
        depth: usize,
    ) -> Result<(), String> {
        if !value_contains_deferred(decoded) {
            return Ok(());
        }
        if depth >= 32 {
            return Err("value nesting exceeds 32".into());
        }
        let source = self.bytes;
        let bytes = &source[range.clone()];
        if field.definition.is_some() {
            return self.walk(
                decoded,
                &self.schema.resolve(field)?,
                range,
                parent,
                depth + 1,
            );
        }
        if let Some(layout) = super::deciders::layout(field, bytes)? {
            return self.walk(decoded, &layout, range, parent, depth + 1);
        }
        if let Value::Deferred(_) = decoded {
            self.leaves.push(DeferredLeaf {
                field: field.clone(),
                range,
                parent,
                path: self.path.clone(),
                depth,
                context: self
                    .contexts
                    .next()
                    .ok_or("missing deferred leaf context")?
                    .clone(),
            });
            return Ok(());
        }
        let selection = super::deciders::Context {
            parent_payload: parent.as_ref().map(|range| &self.bytes[range.clone()]),
            ..self.selection
        };
        if field.kind == "union" {
            let alternative = decide(field, bytes, self.schema, &selection)?
                .ok_or("deferred union shape mismatch")?;
            return self.walk(decoded, &alternative, range, parent, depth + 1);
        }
        match decoded {
            Value::Struct(values) => {
                let mut values = values.iter().enumerate();
                for member in &field.members {
                    if let Some((member, span)) = member_span(member, bytes.len(), self.schema)? {
                        let (index, (_, value)) =
                            values.next().ok_or("deferred member shape mismatch")?;
                        self.path.push(index);
                        self.walk(
                            value,
                            &member,
                            range.start + span.start..range.start + span.end,
                            Some(range.clone()),
                            depth + 1,
                        )?;
                        self.path.pop();
                    }
                }
            }
            Value::Array(values) => {
                let stride = array_stride(field, self.schema)?;
                let offset = array_prefix(field);
                let element = array_element(field, stride, self.schema)?;
                for (index, value) in values.iter().enumerate() {
                    let start = range.start + offset + index * stride;
                    self.path.push(index);
                    self.walk(
                        value,
                        &element,
                        start..start + stride,
                        parent.clone(),
                        depth + 1,
                    )?;
                    self.path.pop();
                }
            }
            _ => return Err("deferred value shape mismatch".into()),
        }
        Ok(())
    }
}

/// Both materialized arrays and target traversal use the same count-prefix width.
fn array_prefix(field: &FieldSchema) -> usize {
    usize::from(
        field
            .count
            .as_ref()
            .and_then(|rule| rule.get("kind"))
            .and_then(|value| value.as_str())
            == Some("prefix_u32"),
    ) * 4
}

#[cfg(test)]
#[path = "master_index_compatibility_diagnostic_tests.rs"]
mod master_index_compatibility_diagnostic_tests;

/// Catalogue replay must never expose unresolved members in a canonical field.
pub(crate) fn value_contains_deferred(value: &Value) -> bool {
    match value {
        Value::Deferred(_) => true,
        Value::Struct(members) => members
            .iter()
            .any(|(_, value)| value_contains_deferred(value)),
        Value::Array(values) => values.iter().any(value_contains_deferred),
        _ => false,
    }
}

/// Retain recovery state only for winners with an unresolved canonical member.
pub(crate) fn record_contains_deferred(record: &DecodedRecord) -> bool {
    record
        .fields
        .iter()
        .any(|field| value_contains_deferred(&field.value))
}

/// Retain deferred roots and grammar-sensitive tails; other fields move between views.
struct DeferredSource {
    fields: Vec<(usize, DecodedField)>,
    positions: Vec<usize>,
    deferred: Vec<DeferredField>,
    rejected_fields: Vec<[u8; 4]>,
}

impl DeferredSource {
    /// Move deferred prototypes aside without copying normal fields or native payloads.
    fn take(record: &mut DecodedRecord) -> Self {
        let deferred = std::mem::take(&mut record.deferred_fields);
        let cache_from = deferred
            .first()
            .and_then(|deferred| deferred.grammar.as_ref())
            .map(|grammar| grammar.cache_from);
        let mut source = Self {
            fields: Vec::new(),
            positions: Vec::new(),
            deferred,
            rejected_fields: record.rejected_fields.clone(),
        };
        for (position, field) in std::mem::take(&mut record.fields).into_iter().enumerate() {
            if value_contains_deferred(&field.value)
                || cache_from.is_some_and(|start| position >= start)
            {
                source.fields.push((position, field));
            } else {
                source.positions.push(position);
                record.fields.push(field);
            }
        }
        source
    }

    /// Refresh deferred roots and dependent prototypes when a fallback changes the catalogue.
    fn reset(&mut self, record: &mut DecodedRecord) {
        let fields = std::mem::take(&mut record.fields);
        let cache_from = self
            .deferred
            .first()
            .and_then(|deferred| deferred.grammar.as_ref())
            .map(|grammar| grammar.cache_from);
        let mut slots: Vec<_> = std::mem::take(&mut self.positions)
            .into_iter()
            .zip(fields)
            .filter(|(position, _)| {
                !cache_from.is_some_and(|start| *position >= start)
                    && self
                        .fields
                        .binary_search_by_key(position, |(deferred, _)| *deferred)
                        .is_err()
            })
            .collect();
        slots.extend(self.fields.iter().cloned());
        slots.sort_by_key(|(position, _)| *position);
        for (position, field) in slots {
            self.positions.push(position);
            record.fields.push(field);
        }
        record.rejected_fields.clone_from(&self.rejected_fields);
    }

    /// A failed first definition keeps its original normal prefix and omits every deferred root.
    fn safe_prefix(self, record: &mut DecodedRecord) -> Vec<[u8; 4]> {
        let fields = std::mem::take(&mut record.fields);
        let cache_from = self
            .deferred
            .first()
            .and_then(|deferred| deferred.grammar.as_ref())
            .map(|grammar| grammar.cache_from);
        let mut normal: Vec<_> = self
            .positions
            .into_iter()
            .zip(fields)
            .filter(|(position, _)| {
                !cache_from.is_some_and(|start| *position >= start)
                    && self
                        .fields
                        .binary_search_by_key(position, |(cached, _)| *cached)
                        .is_err()
            })
            .collect();
        let mut omitted = Vec::new();
        for (position, field) in self.fields {
            if value_contains_deferred(&field.value) {
                omitted.push(field.signature);
            } else {
                normal.push((position, field));
            }
        }
        normal.sort_by_key(|(position, _)| *position);
        record.fields = normal.into_iter().map(|(_, field)| field).collect();
        record.rejected_fields = self.rejected_fields;
        omitted
    }
}

/// Find a captured leaf by ordered typed coordinates, including duplicate member labels.
fn deferred_value_mut<'a>(mut value: &'a mut Value, path: &[usize]) -> Option<&'a mut Value> {
    for &index in path {
        value = match value {
            Value::Struct(members) => &mut members.get_mut(index)?.1,
            Value::Array(values) => values.get_mut(index)?,
            _ => return None,
        };
    }
    Some(value)
}

/// Interpret only saved leaves, retaining exact diagnostic visits of ordinary siblings.
fn replay_deferred_field(
    field: &mut DecodedField,
    deferred: &DeferredField,
    bytes: &[u8],
    schema: &Schema,
    context: &Context<'_>,
    selection: &super::deciders::Context<'_>,
) -> (PluginDiagnostics, Option<String>) {
    let mut local = PluginDiagnostics::default();
    let mut shifts = Vec::new();
    let mut shift = 0;
    let mut cutoff = None;
    let mut failure = None;
    for leaf in &deferred.leaves {
        local.event_context = leaf.context.clone();
        local.event_context.occurrence += shift;
        let before = local.event_context.occurrence;
        let selected = super::deciders::Context {
            parent_payload: leaf.parent.as_ref().map(|range| &bytes[range.clone()]),
            ..*selection
        };
        let resolved = value_inner(
            &leaf.field,
            &bytes[leaf.range.clone()],
            schema,
            context,
            &mut local,
            &selected,
            leaf.depth,
        );
        let visits = local.event_context.occurrence - before;
        shifts.push((leaf.context.occurrence, visits));
        shift += visits;
        match resolved {
            Ok((value, canonical)) => {
                if let Some(target) = deferred_value_mut(&mut field.value, &leaf.path) {
                    *target = value;
                } else {
                    cutoff = Some(leaf.context.occurrence);
                    failure = Some("deferred typed path mismatch".into());
                    break;
                }
                field.canonical_bytes[leaf.range.clone()].copy_from_slice(&canonical);
            }
            Err(error) => {
                cutoff = Some(leaf.context.occurrence);
                failure = Some(error);
                break;
            }
        }
    }
    for event in &deferred.links {
        if cutoff.is_some_and(|cutoff| event.context.occurrence > cutoff) {
            continue;
        }
        let mut adjusted = event.context.clone();
        adjusted.occurrence += shifts
            .iter()
            .filter(|(ordinal, _)| *ordinal < event.context.occurrence)
            .map(|(_, visits)| visits)
            .sum::<u64>();
        local.event_context = adjusted;
        local.note("link", || event.reason.clone());
    }
    (local, failure)
}

/// Add contextual observations while retaining the least physical source examples.
fn merge_replay_observations(replay: &mut PluginDiagnostics, local: PluginDiagnostics) {
    replay.event_log.merge_from(&local.event_log);
    replay.merge_summaries(&local);
}

/// Grammar guards can change later descriptor admission or an EDID-based selector.
/// The matcher runs over native framing, while unchanged outcomes move from the
/// first pass and only newly bound or context-dependent values enter the decoder.
struct GrammarReplay<'a, 'b> {
    schema: &'a Schema,
    context: &'a Context<'b>,
    prepared: &'a PreparedRecord,
    fields: &'a [([u8; 4], &'a [u8])],
    grammar: &'a DeferredGrammar,
    deferred: &'a [DeferredField],
}

/// All inputs for one authoritative physical field binding.
struct GrammarField<'a, 'b> {
    occurrence: &'a GrammarOccurrence,
    binding: FieldBinding,
    descriptor: &'a FieldSchema,
    source_position: usize,
    selection: &'b super::deciders::Context<'a>,
}

impl GrammarReplay<'_, '_> {
    /// Reuse both accepted values and prior optional failures under a stable binding.
    fn interpret(
        &self,
        providers: &mut HashMap<usize, DecodedField>,
        field: GrammarField<'_, '_>,
        local: &mut PluginDiagnostics,
    ) -> Result<Option<(usize, DecodedField)>, String> {
        let stable = field.occurrence.binding == Some(field.binding)
            && field.occurrence.interpreted
            && (field.occurrence.editor_id == field.selection.editor_id
                || !schema_uses_editor_id(field.descriptor, self.schema));
        let (signature, bytes) = self.fields[field.source_position];
        if stable {
            if let Some(index) = field.occurrence.field_index {
                let mut decoded = providers
                    .remove(&index)
                    .ok_or("deferred cached field missing")?;
                if let Some(deferred) = self
                    .deferred
                    .iter()
                    .find(|deferred| deferred.source_position == field.source_position)
                {
                    let (observations, failure) = replay_deferred_field(
                        &mut decoded,
                        deferred,
                        bytes,
                        self.schema,
                        self.context,
                        field.selection,
                    );
                    *local = observations;
                    if let Some(error) = failure {
                        return Err(error);
                    }
                    decoded.localized_bank = selected_localized_bank(
                        field.descriptor,
                        bytes,
                        self.schema,
                        field.selection,
                        &decoded.value,
                    );
                } else {
                    field.occurrence.counts.write(local);
                }
                return Ok(Some((index, decoded)));
            }
            // The first pass already diagnosed this optional failure. Its scalar
            // delta participates in virtual full-replay counts, with no retry.
            field.occurrence.counts.write(local);
            return Ok(None);
        }
        if matches!(field.binding, FieldBinding::Extension(_, true)) {
            return Err("duplicate singleton extension".into());
        }
        let (value, canonical_bytes) = value(
            field.descriptor,
            bytes,
            self.schema,
            self.context,
            local,
            field.selection,
            0,
        )?;
        let index = field.occurrence.field_index.unwrap_or_else(|| {
            self.grammar
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.field_index)
                .max()
                .map_or(0, |last| last + 1)
                + field.source_position
        });
        Ok(Some((
            index,
            DecodedField {
                signature,
                name: field.descriptor.name.clone(),
                localized_bank: selected_localized_bank(
                    field.descriptor,
                    bytes,
                    self.schema,
                    field.selection,
                    &value,
                ),
                value,
                canonical_bytes,
                opaque: false,
            },
        )))
    }

    /// Mirror the initial grammar's pre-match and post-match ownership guards.
    fn run(
        &self,
        record: &mut DecodedRecord,
        positions: &mut Vec<usize>,
        replay: &mut PluginDiagnostics,
    ) -> Result<(), String> {
        let decoded_id = self.deferred[0].leaves[0]
            .context
            .form_id
            .unwrap_or(record.form_id);
        let mut providers: HashMap<_, _> = std::mem::take(positions)
            .into_iter()
            .zip(std::mem::take(&mut record.fields))
            .collect();
        let mut output = Vec::new();
        let mut rejected = Vec::new();
        let mut totals = self.grammar.start;
        let mut cursor = 0;
        let mut last = None;
        let mut seen_extensions = std::collections::BTreeSet::new();
        let mut editor_id = String::new();
        let mut rejected_condition_strings = false;
        let mut rejected_info_response: Option<(usize, usize)> = None;
        let mut rejected_land_layer: Option<(usize, usize)> = None;
        let link_kind = |source_id| {
            let id = remap(
                source_id,
                self.context,
                true,
                &mut PluginDiagnostics::default(),
            )
            .ok()?;
            self.context.winning_types?.get(&id).copied()
        };
        let mut failure = None;
        for (position, &(signature, bytes)) in self.fields.iter().enumerate() {
            let occurrence = &self.grammar.occurrences[position];
            let mut local = PluginDiagnostics::default();
            local.set_record(
                record.source_record_offset,
                Some(record.record_type),
                Some(record.source_form_id),
                Some(decoded_id),
                "decode",
            );
            local.set_field(format!(
                "{}[source:{position}]",
                String::from_utf8_lossy(&signature)
            ));
            let land_anchor =
                record.record_type == *b"LAND" && matches!(&signature, b"BTXT" | b"ATXT");
            let pre_rejection = if land_anchor {
                rejected_land_layer = None;
                None
            } else if signature == *b"VTXT" && rejected_land_layer.is_some() {
                Some(format!(
                    "LAND {decoded_id:08X} VTXT: weights belong to a rejected physical layer anchor"
                ))
            } else {
                None
            };
            let condition_string = matches!(&signature, b"CIS1" | b"CIS2");
            let pre_rejection = pre_rejection.or_else(|| {
                (condition_string && rejected_condition_strings).then(|| {
                    format!(
                        "{} {decoded_id:08X} {}: string belongs to rejected CTDA",
                        String::from_utf8_lossy(&record.record_type),
                        String::from_utf8_lossy(&signature)
                    )
                })
            });
            if let Some(reason) = pre_rejection {
                if !rejected.contains(&signature) {
                    rejected.push(signature);
                }
                local.note("field", || reason);
                totals.add(DiagnosticCounts::read(&local));
                merge_replay_observations(replay, local);
                continue;
            }
            if !condition_string {
                rejected_condition_strings = false;
            }
            let extension = self
                .prepared
                .extensions
                .iter()
                .enumerate()
                .find(|(_, entry)| {
                    entry
                        .field
                        .signature
                        .as_deref()
                        .is_some_and(|tag| tag.as_bytes() == signature)
                });
            let binding = if let Some((index, entry)) = extension {
                Some(FieldBinding::Extension(
                    index,
                    !entry.field.repeat && !seen_extensions.insert(signature),
                ))
            } else {
                match_field(self.prepared, &signature, cursor, last).map(FieldBinding::Entry)
            };
            let Some(binding) = binding else {
                if land_anchor {
                    rejected_land_layer = self
                        .prepared
                        .entries
                        .iter()
                        .position(|entry| {
                            entry
                                .field
                                .signature
                                .as_deref()
                                .is_some_and(|tag| tag.as_bytes() == signature)
                        })
                        .and_then(|index| layer_repeat_range(self.prepared, index));
                }
                let known = self.prepared.entries.iter().any(|entry| {
                    entry
                        .field
                        .signature
                        .as_deref()
                        .is_some_and(|tag| tag.as_bytes() == signature)
                });
                if known && signature == *b"CTDA" {
                    rejected_condition_strings = true;
                }
                if known && !rejected.contains(&signature) {
                    rejected.push(signature);
                }
                local.note("unexpected", || {
                    format!(
                        "{} {decoded_id:08X}: unexpected/out-of-order {}",
                        String::from_utf8_lossy(&record.record_type),
                        String::from_utf8_lossy(&signature)
                    )
                });
                if !known {
                    let index = occurrence.field_index.unwrap_or_else(|| {
                        self.grammar
                            .occurrences
                            .iter()
                            .filter_map(|occurrence| occurrence.field_index)
                            .max()
                            .map_or(0, |last| last + 1)
                            + position
                    });
                    let decoded = providers.remove(&index).unwrap_or_else(|| DecodedField {
                        signature,
                        name: String::from_utf8_lossy(&signature).into_owned(),
                        value: Value::Bytes(bytes.to_vec()),
                        canonical_bytes: bytes.to_vec(),
                        opaque: true,
                        localized_bank: None,
                    });
                    output.push((index, decoded));
                }
                totals.add(DiagnosticCounts::read(&local));
                merge_replay_observations(replay, local);
                continue;
            };
            let descriptor = match binding {
                FieldBinding::Extension(index, _) => &self.prepared.extensions[index].field,
                FieldBinding::Entry(index) => {
                    let descriptor = &self.prepared.entries[index].field;
                    cursor = index + usize::from(!descriptor.repeat);
                    last = Some(index);
                    if let Some((start, end)) = rejected_land_layer
                        && (index < start || index >= end)
                    {
                        rejected_land_layer = None;
                    }
                    if let Some((start, end)) = rejected_info_response {
                        if index > start && index < end {
                            if !rejected.contains(&signature) {
                                rejected.push(signature);
                            }
                            local.note("field", || {
                                format!(
                                    "{} {decoded_id:08X} {}: member belongs to rejected TRDT",
                                    String::from_utf8_lossy(&record.record_type),
                                    String::from_utf8_lossy(&signature)
                                )
                            });
                            totals.add(DiagnosticCounts::read(&local));
                            merge_replay_observations(replay, local);
                            continue;
                        }
                        rejected_info_response = None;
                    }
                    descriptor
                }
            };
            let selection = super::deciders::Context {
                record_type: record.record_type,
                form_version: record.form_version,
                editor_id: &editor_id,
                preceding_subrecords: &self.fields[..position],
                parent_payload: None,
                link_record_type: Some(&link_kind),
            };
            let interpreted = self.interpret(
                &mut providers,
                GrammarField {
                    occurrence,
                    binding,
                    descriptor,
                    source_position: position,
                    selection: &selection,
                },
                &mut local,
            );
            match interpreted {
                Ok(Some((index, decoded))) => {
                    if signature == *b"EDID"
                        && let Value::String(name) = &decoded.value
                    {
                        editor_id.clone_from(name);
                    }
                    output.push((index, decoded));
                }
                outcome => {
                    if let FieldBinding::Entry(index) = binding {
                        if land_anchor {
                            rejected_land_layer = layer_repeat_range(self.prepared, index);
                        }
                        if signature == *b"CTDA" {
                            rejected_condition_strings = true;
                        }
                        if record.record_type == *b"INFO" && signature == *b"TRDT" {
                            rejected_info_response = self.prepared.entries[index]
                                .repeat_ranges
                                .iter()
                                .find(|range| range.start == index)
                                .map(|range| (range.start, range.end));
                        }
                    }
                    if let Err(error) = outcome {
                        if descriptor.reject_record_on_error && record.flags & 0x20 == 0 {
                            failure = Some(format!(
                                "deferred winner {:08X}: {} {decoded_id:08X} mandatory {}: {error}",
                                record.form_id,
                                String::from_utf8_lossy(&record.record_type),
                                descriptor.name
                            ));
                        } else {
                            // The initial root wrapper reports errors outside any nested leaf scope.
                            local.set_record(
                                record.source_record_offset,
                                Some(record.record_type),
                                Some(record.source_form_id),
                                Some(decoded_id),
                                "decode",
                            );
                            local.set_field(format!(
                                "{}[source:{position}]",
                                String::from_utf8_lossy(&signature)
                            ));
                            local.note("field", || {
                                format!(
                                    "{} {decoded_id:08X} {}: {error}",
                                    String::from_utf8_lossy(&record.record_type),
                                    String::from_utf8_lossy(&signature)
                                )
                            });
                        }
                    }
                    if !rejected.contains(&signature) {
                        rejected.push(signature);
                    }
                }
            }
            totals.add(DiagnosticCounts::read(&local));
            merge_replay_observations(replay, local);
            if failure.is_some() {
                break;
            }
        }
        if failure.is_some() {
            // Recovery needs every untouched normal prefix. Deferred prototypes
            // and grammar tails are restored separately by DeferredSource.
            output.extend(providers);
            output.sort_by_key(|(index, _)| *index);
        }
        for (index, field) in output {
            positions.push(index);
            record.fields.push(field);
        }
        record.rejected_fields = rejected;
        totals.write(replay);
        if let Some(reason) = failure {
            return Err(reason);
        }
        if record_contains_deferred(record) {
            return Err(format!(
                "deferred winner {:08X}: retained unresolved values",
                record.form_id
            ));
        }
        Ok(())
    }
}

/// Replay captured leaves against borrowed source framing, preserving normal siblings.
fn replay_deferred(
    record: &mut DecodedRecord,
    source: &mut DeferredSource,
    schema: &Schema,
    context: &Context<'_>,
    baseline: &mut PluginDiagnostics,
    replay: &mut PluginDiagnostics,
    prepared: &PreparedSchemas,
) -> Result<(), String> {
    source.reset(record);
    let Some(first) = source.deferred.first() else {
        return Err(format!(
            "deferred baseline {:08X}: missing first-pass replay metadata",
            record.form_id
        ));
    };
    first.baseline.write(baseline);
    for field in &source.deferred {
        for event in &field.links {
            baseline.event_context = event.context.clone();
            baseline
                .event_log
                .append(&event.context, "link", &event.reason);
        }
    }
    if let Some(grammar) = &first.grammar {
        let record_schema = prepared
            .get(&record.record_type)
            .ok_or("deferred grammar schema missing")?;
        // Moving the native buffer permits mutable field publication while all
        // grammar and selector slices borrow that same allocation. Restore it
        // before every return, including defensive recovery errors.
        let native = std::mem::take(&mut record.raw_payload);
        let result = {
            let mut framing = PluginDiagnostics::default();
            let framed = subrecords(&native, &mut framing, record.source_form_id);
            (|| {
                if framed.fields.len() != grammar.occurrences.len() {
                    return Err(format!(
                        "deferred baseline {:08X}: changed source framing",
                        record.form_id
                    ));
                }
                for deferred in &source.deferred {
                    let &(signature, bytes) =
                        framed.fields.get(deferred.source_position).ok_or_else(|| {
                            format!(
                                "deferred baseline {:08X}: missing deferred source occurrence",
                                record.form_id
                            )
                        })?;
                    verify_deferred_source(record, deferred, signature, bytes)?;
                }
                GrammarReplay {
                    schema,
                    context,
                    prepared: record_schema,
                    fields: &framed.fields,
                    grammar,
                    deferred: &source.deferred,
                }
                .run(record, &mut source.positions, replay)
            })()
        };
        record.raw_payload = native;
        return result;
    }
    let mut framing = PluginDiagnostics::default();
    let framed = subrecords(&record.raw_payload, &mut framing, record.source_form_id);
    let link_kind = |source_id| {
        let id = remap(source_id, context, true, &mut PluginDiagnostics::default()).ok()?;
        context.winning_types?.get(&id).copied()
    };
    let mut totals = first.baseline;
    let mut previous = Vec::new();
    let mut omitted = Vec::new();
    let mut rejections = first.rejections.clone();
    for deferred in &source.deferred {
        let position = source
            .positions
            .binary_search(&deferred.field_index)
            .map_err(|_| "deferred root position mismatch")?;
        let Some(&(signature, bytes)) = framed.fields.get(deferred.source_position) else {
            return Err(format!(
                "deferred baseline {:08X}: missing deferred source occurrence",
                record.form_id
            ));
        };
        verify_deferred_source(record, deferred, signature, bytes)?;
        let field = &mut record.fields[position];
        let selection = super::deciders::Context {
            record_type: record.record_type,
            form_version: record.form_version,
            editor_id: &deferred.editor_id,
            preceding_subrecords: &framed.fields[..deferred.source_position],
            parent_payload: None,
            link_record_type: Some(&link_kind),
        };
        let (mut local, failure) =
            replay_deferred_field(field, deferred, bytes, schema, context, &selection);
        let original = deferred.links.len() as u64;
        totals.replace_links(original, local.invalid_links);
        let original_counts = DiagnosticCounts {
            link: original,
            ..DiagnosticCounts::default()
        };
        if let Some(error) = failure {
            let decoded_id = deferred.leaves[0].context.form_id.unwrap_or(record.form_id);
            if deferred.field.reject_record_on_error && record.flags & 0x20 == 0 {
                let mut prefix = deferred.before;
                for &(old, new) in &previous {
                    prefix.replace(old, new);
                }
                prefix.link += local.invalid_links;
                replay.event_log.merge_from(&local.event_log);
                replay.merge_summaries(&local);
                prefix.write(replay);
                return Err(format!(
                    "deferred winner {:08X}: {} {decoded_id:08X} mandatory {}: {error}",
                    record.form_id,
                    String::from_utf8_lossy(&record.record_type),
                    deferred.field.name
                ));
            }
            local.set_record(
                record.source_record_offset,
                Some(record.record_type),
                Some(record.source_form_id),
                Some(decoded_id),
                "decode",
            );
            local.set_field(format!(
                "{}[source:{}]",
                String::from_utf8_lossy(&signature),
                deferred.source_position
            ));
            local.note("field", || {
                format!(
                    "{} {decoded_id:08X} {}: {error}",
                    String::from_utf8_lossy(&record.record_type),
                    String::from_utf8_lossy(&signature)
                )
            });
            totals.field += 1;
            omitted.push(position);
            rejections.push((deferred.source_position, signature));
            previous.push((original_counts, DiagnosticCounts::read(&local)));
        } else {
            field.localized_bank =
                selected_localized_bank(&deferred.field, bytes, schema, &selection, &field.value);
            previous.push((original_counts, DiagnosticCounts::read(&local)));
        }
        replay.event_log.merge_from(&local.event_log);
        replay.merge_summaries(&local);
    }
    omitted.sort_unstable();
    omitted.dedup();
    for position in omitted.into_iter().rev() {
        record.fields.remove(position);
        source.positions.remove(position);
    }
    rejections.sort_by_key(|(position, _)| *position);
    record.rejected_fields.clear();
    for (_, signature) in rejections {
        if !record.rejected_fields.contains(&signature) {
            record.rejected_fields.push(signature);
        }
    }
    totals.write(replay);
    if record_contains_deferred(record) {
        return Err(format!(
            "deferred winner {:08X}: retained unresolved values",
            record.form_id
        ));
    }
    Ok(())
}

/// Retained metadata may only address the same framed native occurrence.
fn verify_deferred_source(
    record: &DecodedRecord,
    deferred: &DeferredField,
    signature: [u8; 4],
    bytes: &[u8],
) -> Result<(), String> {
    if deferred
        .field
        .signature
        .as_deref()
        .is_some_and(|tag| tag.as_bytes() == signature)
        && bytes.len() == deferred.source_size
    {
        return Ok(());
    }
    let error = if deferred.field.size.is_some_and(|size| size != bytes.len())
        || !deferred.field.sizes.is_empty() && !deferred.field.sizes.contains(&bytes.len())
    {
        format!(
            "length {} does not match schema size {:?}/{:?}",
            bytes.len(),
            deferred.field.size,
            deferred.field.sizes
        )
    } else {
        "deferred source occurrence changed".into()
    };
    let decoded_id = deferred.leaves[0].context.form_id.unwrap_or(record.form_id);
    Err(format!(
        "deferred baseline {:08X}: {} {decoded_id:08X} mandatory {}: {error}",
        record.form_id,
        String::from_utf8_lossy(&record.record_type),
        deferred.field.name
    ))
}

/// Change the exact physical occurrence while retaining its native source metadata.
fn set_deferred_history(
    result: &mut ReadResult,
    history_index: &HashMap<(u32, u64), usize>,
    record: &DecodedRecord,
    accepted: bool,
    reason: String,
) {
    if let Some(&position) = history_index.get(&(record.load_order, record.source_record_offset))
        && let Some(entry) = result.history.get_mut(position)
    {
        entry.accepted = accepted;
        entry.reason = Some(reason);
    }
}

/// Diagnose local candidate failure and revoke that occurrence's earlier admission.
fn reject_deferred_candidate(
    result: &mut ReadResult,
    history_index: &HashMap<(u32, u64), usize>,
    record: &DecodedRecord,
    order: &LoadOrder,
    reason: String,
) {
    set_deferred_history(result, history_index, record, false, reason.clone());
    let diagnostics = result
        .diagnostics
        .entry(order.names[record.load_order as usize].clone())
        .or_default();
    diagnostics.set_record(
        record.source_record_offset,
        Some(record.record_type),
        Some(record.source_form_id),
        Some(record.form_id),
        "admission",
    );
    result
        .diagnostics
        .entry(order.names[record.load_order as usize].clone())
        .or_default()
        .note("record", || {
            format!(
                "{} {:08X}: {reason}",
                String::from_utf8_lossy(&record.record_type),
                record.form_id
            )
        });
}

/// Finalize tentative winners against a stable catalogue, preserving only selective fallback chains.
pub(crate) fn resolve_deferred(
    result: &mut ReadResult,
    schema: &Schema,
    prepared: &PreparedSchemas,
    order: &LoadOrder,
    predecessors: &mut HashMap<u32, Vec<DecodedRecord>>,
    validate: &mut impl FnMut(&DecodedRecord) -> Result<(), String>,
) {
    let mut pending = BTreeMap::new();
    for (&id, record) in &mut result.records {
        if record_contains_deferred(record) {
            pending.insert(id, DeferredSource::take(record));
        }
    }
    let affected: Vec<_> = pending.keys().copied().collect();
    let candidate_count = pending.len()
        + affected
            .iter()
            .filter_map(|id| predecessors.get(id))
            .map(Vec::len)
            .sum::<usize>();
    let mut replay_diagnostics = HashMap::new();
    let mut changed_chains: HashMap<u32, Vec<super::RecordSource>> = HashMap::new();
    // Only a fallback replaces a record, so the winning-kind map follows each
    // catalogue mutation instead of being rebuilt at the start of every round.
    let mut kinds: HashMap<u32, [u8; 4]> = HashMap::new();
    // History is complete before resolution and only its entries change here, so
    // one key index serves every acceptance and rejection without rescanning it.
    let mut history_index: HashMap<(u32, u64), usize> = HashMap::new();
    // A conversion with nothing deferred pays neither pass over its records.
    if !pending.is_empty() {
        kinds.extend(
            result
                .records
                .iter()
                .map(|(&id, record)| (id, record.record_type)),
        );
        for (index, entry) in result.history.iter().enumerate() {
            history_index
                .entry((entry.plugin_index, entry.source_record_offset))
                .or_insert(index);
        }
        #[cfg(test)]
        record_deferred_catalogue_build();
    }
    // Every failure consumes a candidate or finishes its safe prefix. Stable
    // winners visit only their originally deferred leaves once more. A changed
    // winning kind necessarily revisits those leaves against the new catalogue;
    // stable normal fields and siblings are not decoded again. A failed deferred
    // ownership anchor can change a later grammar binding or EDID selector
    // context; only those dependent occurrences require reinterpretation.
    for _ in 0..=candidate_count {
        if pending.is_empty() {
            break;
        }
        let ids: Vec<_> = pending.keys().copied().collect();
        let mut recovered = false;
        for id in ids {
            let Some(mut record) = result.records.remove(&id) else {
                pending.remove(&id);
                continue;
            };
            let priority = record.load_order as usize;
            let context = Context {
                order,
                plugin_index: priority,
                localized: order.metadata[priority].flags & 0x80 != 0,
                winning_types: Some(&kinds),
            };
            let mut baseline = PluginDiagnostics::default();
            let mut replay = PluginDiagnostics::default();
            let failure = replay_deferred(
                &mut record,
                pending.get_mut(&id).expect("selected deferred source"),
                schema,
                &context,
                &mut baseline,
                &mut replay,
                prepared,
            )
            .err()
            .or_else(|| {
                validate(&record)
                    .err()
                    .map(|error| format!("deferred resolved candidate {id:08X} unusable: {error}"))
            });
            let Some(reason) = failure else {
                replay_diagnostics.insert(id, (baseline, replay));
                let record_type = record.record_type;
                result.records.insert(id, record);
                kinds.insert(id, record_type);
                continue;
            };
            merge_new_diagnostics(
                result
                    .diagnostics
                    .entry(order.names[priority].clone())
                    .or_default(),
                &baseline,
                replay,
            );
            replay_diagnostics.remove(&id);
            changed_chains.entry(id).or_default();
            let source = pending.remove(&id).expect("selected deferred source");
            if let Some(mut previous) = predecessors.get_mut(&id).and_then(|chain| chain.pop()) {
                reject_deferred_candidate(result, &history_index, &record, order, reason);
                loop {
                    let failure = if !previous.payload_complete {
                        Some("deferred recovery predecessor has incomplete payload".to_owned())
                    } else if record_contains_deferred(&previous) {
                        None
                    } else {
                        validate(&previous)
                            .err()
                            .map(|error| format!("deferred recovery predecessor unusable: {error}"))
                    };
                    if let Some(reason) = failure {
                        reject_deferred_candidate(result, &history_index, &previous, order, reason);
                        if let Some(earlier) =
                            predecessors.get_mut(&id).and_then(|chain| chain.pop())
                        {
                            previous = earlier;
                            continue;
                        }
                    } else {
                        if record_contains_deferred(&previous) {
                            pending.insert(id, DeferredSource::take(&mut previous));
                        }
                        let record_type = previous.record_type;
                        result.records.insert(id, previous);
                        kinds.insert(id, record_type);
                    }
                    break;
                }
            } else {
                let omitted = source.safe_prefix(&mut record);
                for (position, signature) in omitted.iter().enumerate() {
                    let diagnostics = result
                        .diagnostics
                        .entry(order.names[priority].clone())
                        .or_default();
                    diagnostics.set_record(
                        record.source_record_offset,
                        Some(record.record_type),
                        Some(record.source_form_id),
                        Some(record.form_id),
                        "admission",
                    );
                    diagnostics.set_field(format!(
                        "{}[omitted:{position}]",
                        String::from_utf8_lossy(signature)
                    ));
                    if !record.rejected_fields.contains(signature) {
                        record.rejected_fields.push(*signature);
                    }
                    result
                        .diagnostics
                        .entry(order.names[priority].clone())
                        .or_default()
                        .note("field", || {
                            format!(
                                "{} {id:08X} {}: omitted after {reason}",
                                String::from_utf8_lossy(&record.record_type),
                                String::from_utf8_lossy(signature)
                            )
                        });
                }
                let fields = omitted
                    .iter()
                    .map(|signature| String::from_utf8_lossy(signature))
                    .collect::<Vec<_>>()
                    .join(", ");
                let reason = format!("deferred recovery omitted {fields}: {reason}");
                if let Err(error) = validate(&record) {
                    reject_deferred_candidate(
                        result,
                        &history_index,
                        &record,
                        order,
                        format!("{reason}; recovered prefix unusable: {error}"),
                    );
                } else {
                    set_deferred_history(result, &history_index, &record, true, reason);
                    let record_type = record.record_type;
                    result.records.insert(id, record);
                    kinds.insert(id, record_type);
                }
            }
            // A record rejected without a successor leaves the winning catalogue.
            if !result.records.contains_key(&id) {
                kinds.remove(&id);
            }
            recovered = true;
            // A changed kind can affect a previously successful dependent. Restart
            // before rejecting any other candidate against the superseded catalogue.
            break;
        }
        if !recovered {
            pending.clear();
            break;
        }
    }
    debug_assert!(
        pending.is_empty(),
        "finite deferred candidate population exhausted"
    );
    for (id, (baseline, replay)) in replay_diagnostics {
        if let Some(record) = result.records.get(&id) {
            merge_new_diagnostics(
                result
                    .diagnostics
                    .entry(order.names[record.load_order as usize].clone())
                    .or_default(),
                &baseline,
                replay,
            );
        }
    }
    // Metadata follows final admission; full physical history keeps every rejected version.
    for entry in &result.history {
        if entry.accepted
            && let Some(id) = entry.form_id
            && let Some(chain) = changed_chains.get_mut(&id)
        {
            chain.push(super::RecordSource {
                plugin_index: entry.plugin_index,
                source_form_id: entry.source_form_id,
                deleted: entry.flags & 0x20 != 0,
            });
        }
    }
    for (id, chain) in changed_chains {
        if chain.len() > 1 || chain.iter().any(|source| source.deleted) {
            result.overrides.insert(id, chain);
        } else {
            result.overrides.remove(&id);
        }
    }
    debug_assert!(
        result
            .records
            .values()
            .all(|record| !record_contains_deferred(record))
    );
    // Header selectors are not part of winner replay. Their temporary capture
    // state is also unnecessary once scanning and catalogue admission finish.
    for header in &mut result.headers {
        header.deferred_fields.clear();
    }
}

/// Resolve named definitions with occurrence overrides; plain fields are already complete.
fn validation_schema<'a>(
    schema: &Schema,
    field: &'a FieldSchema,
) -> Result<Cow<'a, FieldSchema>, String> {
    if field.definition.is_none() {
        Ok(Cow::Borrowed(field))
    } else {
        schema.resolve(field).map(Cow::Owned)
    }
}

/// Only nonzero typed links can require generic target validation and selector contexts.
fn contains_target_link(value: &Value) -> bool {
    match value {
        Value::FormId(id) => *id != 0,
        Value::Struct(members) => members.iter().any(|(_, value)| contains_target_link(value)),
        Value::Array(values) => values.iter().any(contains_target_link),
        _ => false,
    }
}

/// Keep direct validator probes on the embedded prepared schema.
#[cfg(test)]
pub(crate) fn validate_targets(result: &mut ReadResult, schema: &Schema, order: &LoadOrder) {
    validate_targets_with_prepared(result, schema, prepared(), order);
}

/// Validate declared target kinds after all winners exist; dangling optional links are nulled.
pub(crate) fn validate_targets_with_prepared(
    result: &mut ReadResult,
    schema: &Schema,
    prepared: &PreparedSchemas,
    order: &LoadOrder,
) {
    let kinds: HashMap<u32, [u8; 4]> = result
        .records
        .iter()
        .map(|(&id, record)| (id, record.record_type))
        .collect();
    for record in result.records.values_mut() {
        let record_schema = prepared.get(&record.record_type);
        let plugin = order.names.get(record.load_order as usize).cloned();
        let mut diagnostics = PluginDiagnostics::default();
        diagnostics.set_record(
            record.source_record_offset,
            Some(record.record_type),
            Some(record.source_form_id),
            Some(record.form_id),
            "targets",
        );
        diagnostics.set_field("GRUP7".into());
        if let Some(topic) = record.topic_form_id {
            diagnostics.link(topic);
            if kinds.get(&topic) != Some(b"DIAL") {
                diagnostics.note("link", || {
                    format!(
                        "INFO {:08X} GRUP7 owner {topic:08X} is absent or is not DIAL",
                        record.form_id
                    )
                });
                record.topic_form_id = None;
            }
        } else if record.source_topic_form_id == Some(0) {
            diagnostics.link(0);
            diagnostics.note("link", || {
                format!("INFO {:08X} GRUP7 has a null owner", record.form_id)
            });
        }
        // Move ownership temporarily so selector slices borrow the original buffers
        // throughout the walk. Apply planned nulls only after those borrows end.
        let needs_context = record
            .fields
            .iter()
            .any(|field| contains_target_link(&field.value));
        let mut original: Vec<_> = if needs_context {
            record
                .fields
                .iter_mut()
                .map(|field| std::mem::take(&mut field.canonical_bytes))
                .collect()
        } else {
            Vec::new()
        };
        let preceding: Vec<_> = record
            .fields
            .iter()
            .zip(&original)
            .map(|(field, bytes)| (field.signature, bytes.as_slice()))
            .collect();
        let mut edits: Vec<Vec<std::ops::Range<usize>>> = vec![Vec::new(); record.fields.len()];
        let mut replaced = vec![false; record.fields.len()];
        let editor_id = if needs_context {
            record
                .fields
                .iter()
                .find_map(|field| {
                    if field.signature == *b"EDID" {
                        if let Value::String(name) = &field.value {
                            Some(name.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        let link_kind = |id| kinds.get(&id).copied();
        for (position, field) in record.fields.iter_mut().enumerate() {
            // Opaque occurrences have no authoritative descriptor and must remain exact bytes.
            if field.opaque {
                continue;
            }
            diagnostics.set_field(format!(
                "{}[canonical:{position}].{}",
                String::from_utf8_lossy(&field.signature),
                field.name
            ));
            let field_schema = record_schema
                .and_then(|prepared| {
                    prepared
                        .entries
                        .iter()
                        .chain(&prepared.extensions)
                        .find(|entry| {
                            entry.field.name == field.name
                                && entry
                                    .field
                                    .signature
                                    .as_deref()
                                    .is_some_and(|tag| tag.as_bytes() == field.signature)
                        })
                        .map(|entry| &entry.field)
                })
                .or_else(|| {
                    schema.common_fields.iter().find(|common| {
                        common
                            .signature
                            .as_deref()
                            .is_some_and(|signature| signature.as_bytes() == field.signature)
                    })
                });
            let Some(field_schema) = field_schema else {
                continue;
            };
            let resolved = validation_schema(schema, field_schema).expect("validated common field");
            if resolved.decider.as_deref() == Some("vmad") {
                // Canonical VMAD links are already global; check all primary and alias objects.
                let validation = super::vmad::decode(
                    if needs_context {
                        &original[position]
                    } else {
                        &field.canonical_bytes
                    },
                    &record.record_type,
                    |id| {
                        diagnostics.link(id);
                        if id == 0 || id < 0x800 || kinds.contains_key(&id) {
                            return Ok(id);
                        }
                        diagnostics.note("link", || {
                            format!(
                                "record {:08X} VMAD object {id:08X} is absent",
                                record.form_id
                            )
                        });
                        Ok(0)
                    },
                );
                if let Ok((value, canonical)) = validation {
                    field.value = value;
                    field.canonical_bytes = canonical;
                    replaced[position] = true;
                }
                continue;
            }
            TargetValidation {
                schema,
                kinds: &kinds,
                diagnostics: &mut diagnostics,
                nulls: &mut edits[position],
            }
            .walk(
                &mut field.value,
                if needs_context {
                    &original[position]
                } else {
                    &field.canonical_bytes
                },
                field_schema,
                &super::deciders::Context {
                    record_type: record.record_type,
                    form_version: record.form_version,
                    editor_id: &editor_id,
                    preceding_subrecords: if needs_context {
                        &preceding[..position]
                    } else {
                        &[]
                    },
                    parent_payload: None,
                    link_record_type: Some(&link_kind),
                },
                0,
            );
        }
        drop(preceding);
        for (position, field) in record
            .fields
            .iter_mut()
            .enumerate()
            .filter(|_| needs_context)
        {
            if replaced[position] {
                continue;
            }
            let mut bytes = std::mem::take(&mut original[position]);
            for range in &edits[position] {
                bytes[range.clone()].fill(0);
            }
            field.canonical_bytes = bytes;
        }
        if diagnostics.invalid_links != 0
            && let Some(plugin) = plugin
        {
            let target = result.diagnostics.entry(plugin).or_default();
            target.event_log.merge_from(&diagnostics.event_log);
            target.invalid_links += diagnostics.invalid_links;
            target.merge_summaries(&diagnostics);
        }
    }
}

/// Direct test probes use the production planner and apply edits after selector borrows end.
#[cfg(test)]
fn validate_value(
    decoded: &mut Value,
    bytes: &mut [u8],
    field: &FieldSchema,
    schema: &Schema,
    kinds: &HashMap<u32, [u8; 4]>,
    diagnostics: &mut PluginDiagnostics,
    selection: &super::deciders::Context<'_>,
) {
    let mut nulls = Vec::new();
    TargetValidation {
        schema,
        kinds,
        diagnostics,
        nulls: &mut nulls,
    }
    .walk(decoded, bytes, field, selection, 0);
    for range in nulls {
        bytes[range].fill(0);
    }
}

/// Typed links change immediately; immutable source bytes remain available to selectors.
struct TargetValidation<'a> {
    schema: &'a Schema,
    kinds: &'a HashMap<u32, [u8; 4]>,
    diagnostics: &'a mut PluginDiagnostics,
    nulls: &'a mut Vec<std::ops::Range<usize>>,
}

impl TargetValidation<'_> {
    /// Preserve diagnostic scope while borrowing every enclosing and preceding buffer.
    fn walk(
        &mut self,
        decoded: &mut Value,
        bytes: &[u8],
        field: &FieldSchema,
        selection: &super::deciders::Context<'_>,
        base: usize,
    ) {
        let previous = self.diagnostics.push_field(&field.name);
        self.inner(decoded, bytes, field, selection, base);
        self.diagnostics.pop_field(previous);
    }

    /// Record canonical null spans without mutating selector bytes during traversal.
    fn inner(
        &mut self,
        decoded: &mut Value,
        bytes: &[u8],
        field: &FieldSchema,
        selection: &super::deciders::Context<'_>,
        base: usize,
    ) {
        // Nonlink leaves never mutate bytes or emit target diagnostics. Keep this
        // inside the generic walker, after the independent outer VMAD dispatch.
        match decoded {
            Value::FormId(0)
            | Value::Unsigned(_)
            | Value::Signed(_)
            | Value::Float(_)
            | Value::String(_)
            | Value::LocalizedString(_)
            | Value::Bytes(_)
            | Value::Deferred(_)
            | Value::CompactArray(_) => return,
            _ => {}
        }
        let Ok(field) = validation_schema(self.schema, field) else {
            return;
        };
        if let Ok(Some(layout)) = super::deciders::layout(&field, bytes) {
            self.walk(decoded, bytes, &layout, selection, base);
            return;
        }
        if field.kind == "union" {
            if let Ok(Some(alternative)) = decide(&field, bytes, self.schema, selection) {
                self.walk(decoded, bytes, &alternative, selection, base);
            }
            return;
        }
        match decoded {
            Value::FormId(id) if *id != 0 => {
                self.diagnostics.link(*id);
                // Hardcoded IDs (including the player) need not have a plugin record.
                let allowed = self.kinds.get(id).map_or(*id < 0x800, |kind| {
                    field.targets.iter().any(|target| {
                        target == "*" || target == "ANY_" || target.as_bytes() == kind
                    })
                });
                if !allowed {
                    self.diagnostics.note("link", || {
                        format!(
                            "{} link {:08X} is absent or has a disallowed target type",
                            field.name, *id
                        )
                    });
                    *id = 0;
                    if bytes.len() == 4 {
                        self.nulls.push(base..base + 4);
                    }
                }
            }
            Value::Struct(values) => {
                let mut values = values.iter_mut();
                for member in &field.members {
                    if let Ok(Some((member, range))) = member_span(member, bytes.len(), self.schema)
                        && let Some((_, value)) = values.next()
                    {
                        self.walk(
                            value,
                            &bytes[range.clone()],
                            &member,
                            &super::deciders::Context {
                                parent_payload: Some(bytes),
                                ..*selection
                            },
                            base + range.start,
                        );
                    }
                }
            }
            Value::Array(values) if field.decider.as_deref() == Some("alternate_textures") => {
                let mut position: usize = 4;
                let target_schema = FieldSchema {
                    kind: "form_id".into(),
                    name: "alternate_texture".into(),
                    targets: vec!["TXST".into()],
                    ..Default::default()
                };
                for item in values {
                    let Some(length_bytes) = bytes.get(position..position.saturating_add(4)) else {
                        break;
                    };
                    let length =
                        u32::from_le_bytes(length_bytes.try_into().expect("checked name length"))
                            as usize;
                    position = position.saturating_add(4).saturating_add(length);
                    if let Value::Struct(members) = item
                        && let Some((_, link)) =
                            members.iter_mut().find(|(name, _)| name == "texture_set")
                        && let Some(link_bytes) = bytes.get(position..position.saturating_add(4))
                    {
                        self.walk(link, link_bytes, &target_schema, selection, base + position);
                    }
                    position = position.saturating_add(8);
                }
            }
            Value::Array(values) => {
                if let Ok(stride) = array_stride(&field, self.schema) {
                    let offset = array_prefix(&field);
                    let Ok(element) = array_element(&field, stride, self.schema) else {
                        return;
                    };
                    for (index, value) in values.iter_mut().enumerate() {
                        let previous = self.diagnostics.push_field(&format!("[{index}]"));
                        let range = index
                            .checked_mul(stride)
                            .and_then(|start| start.checked_add(offset))
                            .and_then(|start| start.checked_add(stride).map(|end| start..end));
                        if let Some(range) = range.filter(|range| range.end <= bytes.len()) {
                            self.walk(
                                value,
                                &bytes[range.clone()],
                                &element,
                                selection,
                                base + range.start,
                            );
                        }
                        self.diagnostics.pop_field(previous);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "target_validation_tests.rs"]
mod target_validation_tests;

#[cfg(test)]
#[path = "core_audit_regression_tests.rs"]
mod core_audit_regression_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame the synthetic source independently of canonical decoding.
    fn replay_probe_subrecord(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
        [
            tag.to_vec(),
            u16::try_from(bytes.len()).unwrap().to_le_bytes().to_vec(),
            bytes.to_vec(),
        ]
        .concat()
    }

    /// Include distinct native version-control and padding words in every header.
    fn replay_probe_record(tag: &[u8; 4], id: u32, flags: u32, bytes: &[u8]) -> Vec<u8> {
        [
            tag.to_vec(),
            u32::try_from(bytes.len()).unwrap().to_le_bytes().to_vec(),
            flags.to_le_bytes().to_vec(),
            id.to_le_bytes().to_vec(),
            0x1357_2468u32.to_le_bytes().to_vec(),
            44u16.to_le_bytes().to_vec(),
            0xA571u16.to_le_bytes().to_vec(),
            bytes.to_vec(),
        ]
        .concat()
    }

    /// Copy diagnostic summaries into an independent journal for the legacy oracle.
    fn replay_probe_diagnostics(source: &PluginDiagnostics) -> PluginDiagnostics {
        let mut copied = PluginDiagnostics::default();
        DiagnosticCounts::read(source).write(&mut copied);
        copied.merge_summaries(source);
        copied.event_log.merge_from(&source.event_log);
        copied
    }

    /// Create the real empty history snapshot required by diagnostic publication.
    fn replay_probe_database() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let order = LoadOrder::from_metadata(Vec::new(), Vec::new()).unwrap();
        super::super::history::write_tables(&conn, &[], &order, &[]).unwrap();
        conn
    }

    /// Compare all published diagnostic identity, provenance, action and text columns.
    fn replay_probe_events(source: &PluginDiagnostics) -> Vec<Vec<rusqlite::types::Value>> {
        let conn = replay_probe_database();
        super::super::diagnostics::write_tables(&conn, [(0, &source.event_log)]).unwrap();
        let mut query = conn
            .prepare("SELECT * FROM record_diagnostics ORDER BY diagnostic_id")
            .unwrap();
        let columns = query.column_count();
        query
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Match full baseline/contextual diagnostics when a leaf shifts later link
    /// ordinals, rejects its optional root, or rejects its mandatory candidate.
    #[test]
    fn deferred_replay_matches_full_decode_ordinals_and_error_cutoffs() {
        for (bad_alternative, mandatory) in [(false, false), (true, false), (true, true)] {
            let schema = Schema::parse(format!(r#"{{"version":1,"records":[{{"signature":"TEST","fields":[
                {{"signature":"EDID","name":"ordinary","kind":"zstring"}},
                {{"signature":"COED","name":"ownership","kind":"struct","size":16,"reject_record_on_error":{mandatory},"members":[
                    {{"name":"owner","kind":"form_id","targets":["NPC_","FACT"]}},
                    {{"name":"condition","kind":"union","offset":4,"size":4,"decider":"inventory_owner_condition","fields":[
                        {{"name":"unused","kind":"bytes","size":4}},
                        {{"name":"global","kind":"form_id","size":{},"targets":["GLOB"]}},
                        {{"name":"rank","kind":"i32"}}]}},
                    {{"name":"later_link","kind":"form_id","offset":8,"targets":["NPC_"]}},
                    {{"name":"weight","kind":"f32","offset":12}}]}},
                {{"signature":"LINK","name":"after_root","kind":"form_id","targets":["NPC_"]}},
                {{"signature":"DATA","name":"damaged_after_root","kind":"u32"}}
            ]}}]}}"#, if bad_alternative { 8 } else { 4 }).as_bytes()).unwrap();
            let prepared = prepare_schema(&schema);
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("Owner.esm");
            let header = replay_probe_record(
                b"TES4",
                0,
                1,
                &replay_probe_subrecord(
                    b"HEDR",
                    &[
                        1.7f32.to_le_bytes(),
                        1u32.to_le_bytes(),
                        0x800u32.to_le_bytes(),
                    ]
                    .concat(),
                ),
            );
            let payload = [
                replay_probe_subrecord(b"EDID", b"OrdinalProbe\0"),
                replay_probe_subrecord(
                    b"COED",
                    &[
                        0x1900u32.to_le_bytes(),
                        0x0200_1899u32.to_le_bytes(),
                        0x0300_18ABu32.to_le_bytes(),
                        0.375f32.to_le_bytes(),
                    ]
                    .concat(),
                ),
                replay_probe_subrecord(b"LINK", &0x0400_18CDu32.to_le_bytes()),
                replay_probe_subrecord(b"DATA", &[0x5A]),
            ]
            .concat();
            let source_bytes = [
                header.clone(),
                replay_probe_record(b"TEST", 0x1880, 0, &payload),
            ]
            .concat();
            std::fs::write(&path, &source_bytes).unwrap();
            let order = LoadOrder::read(std::slice::from_ref(&path)).unwrap();
            let kinds = HashMap::from([(0x1900, *b"NPC_"), (0x1880, *b"TEST")]);
            let scan = || ScannedRecord {
                record_type: *b"TEST",
                flags: 0,
                version_control: 0x1357_2468,
                form_version: 44,
                header_unknown: 0xA571,
                source_form_id: 0x1880,
                cell_form_id: None,
                worldspace_form_id: None,
                source_topic_form_id: None,
                topic_group_offset: None,
                source_record_offset: header.len() as u64,
                payload: payload.clone(),
            };
            let first_context = Context {
                order: &order,
                plugin_index: 0,
                localized: false,
                winning_types: None,
            };
            let resolved_context = Context {
                winning_types: Some(&kinds),
                ..first_context
            };
            let mut original_diagnostics = PluginDiagnostics::default();
            let mut selected = decode_with_prepared(
                scan(),
                &schema,
                &prepared,
                &first_context,
                &mut original_diagnostics,
            )
            .unwrap();
            let mut source = DeferredSource::take(&mut selected);
            let mut baseline = PluginDiagnostics::default();
            let mut replay = PluginDiagnostics::default();
            let actual = replay_deferred(
                &mut selected,
                &mut source,
                &schema,
                &resolved_context,
                &mut baseline,
                &mut replay,
                &prepared,
            );
            let mut actual_diagnostics = replay_probe_diagnostics(&original_diagnostics);
            merge_new_diagnostics(&mut actual_diagnostics, &baseline, replay);
            let mut old_baseline = PluginDiagnostics::default();
            decode_with_prepared(
                scan(),
                &schema,
                &prepared,
                &first_context,
                &mut old_baseline,
            )
            .unwrap();
            let mut old_replay = PluginDiagnostics::default();
            let expected = decode_with_prepared(
                scan(),
                &schema,
                &prepared,
                &resolved_context,
                &mut old_replay,
            );
            let mut expected_diagnostics = replay_probe_diagnostics(&original_diagnostics);
            merge_new_diagnostics(&mut expected_diagnostics, &old_baseline, old_replay);
            match expected {
                Ok(expected) => {
                    actual.unwrap();
                    assert_eq!(
                        serde_json::to_vec(&selected.fields).unwrap(),
                        serde_json::to_vec(&expected.fields).unwrap()
                    );
                    assert_eq!(selected.rejected_fields, expected.rejected_fields);
                    assert!(!record_contains_deferred(&selected));
                }
                Err(error) => assert_eq!(
                    actual.unwrap_err(),
                    format!("deferred winner 00001880: {error}")
                ),
            }
            assert_eq!(
                serde_json::to_vec(&actual_diagnostics).unwrap(),
                serde_json::to_vec(&expected_diagnostics).unwrap()
            );
            assert_eq!(
                replay_probe_events(&actual_diagnostics),
                replay_probe_events(&expected_diagnostics)
            );
            if !bad_alternative {
                assert_eq!(actual_diagnostics.invalid_links, 3);
                let conn = replay_probe_database();
                super::super::diagnostics::write_tables(
                    &conn,
                    [(0, &actual_diagnostics.event_log)],
                )
                .unwrap();
                let ordinals = conn.prepare("SELECT occurrence FROM record_diagnostics WHERE field LIKE '%later_link' ORDER BY occurrence").unwrap().query_map([], |row| row.get::<_, u64>(0)).unwrap().map(Result::unwrap).collect::<Vec<_>>();
                assert_eq!(
                    ordinals,
                    vec![2, 3],
                    "the old event-key policy retains original and shifted later-link observations"
                );
            } else {
                assert_eq!(
                    actual_diagnostics.invalid_links, 2,
                    "saturating full-record subtraction suppresses failures beyond the selected-leaf cutoff"
                );
            }
            assert_eq!(std::fs::read(&path).unwrap(), source_bytes);
        }
    }

    /// Rejected deferred anchors retain exact physical companion omissions, and
    /// later valid anchors keep their own companions after the catalogue is known.
    #[test]
    fn deferred_replay_matches_full_decode_physical_ownership() {
        for (record_type, anchor, members, first, second) in [
            (
                *b"TEST",
                *b"CTDA",
                vec![
                    serde_json::json!({"signature":"CIS1","name":"first_string","kind":"zstring"}),
                    serde_json::json!({"signature":"CIS2","name":"second_word","kind":"u32"}),
                ],
                vec![
                    replay_probe_subrecord(b"CIS1", b"Discard\0"),
                    replay_probe_subrecord(b"CIS2", &[0x5A]),
                ],
                vec![
                    replay_probe_subrecord(b"CIS1", b"Keep\0"),
                    replay_probe_subrecord(b"CIS2", &0x2468_1379u32.to_le_bytes()),
                ],
            ),
            (
                *b"INFO",
                *b"TRDT",
                vec![
                    serde_json::json!({"signature":"NAM1","name":"response","kind":"zstring"}),
                    serde_json::json!({"signature":"NAM2","name":"response_link","kind":"form_id","targets":["NPC_"]}),
                ],
                vec![
                    replay_probe_subrecord(b"NAM1", b"Discard\0"),
                    replay_probe_subrecord(b"NAM2", &0x0200_1899u32.to_le_bytes()),
                ],
                vec![
                    replay_probe_subrecord(b"NAM1", b"Keep\0"),
                    replay_probe_subrecord(b"NAM2", &1u32.to_le_bytes()),
                ],
            ),
            (
                *b"LAND",
                *b"ATXT",
                vec![
                    serde_json::json!({"signature":"VTXT","name":"weights","kind":"bytes","repeat":true}),
                ],
                vec![
                    replay_probe_subrecord(b"VTXT", &[0xA5, 0xA6]),
                    replay_probe_subrecord(b"VTXT", &[0xA7]),
                ],
                vec![
                    replay_probe_subrecord(b"VTXT", &[0x51, 0x52]),
                    replay_probe_subrecord(b"VTXT", &[0x53]),
                ],
            ),
        ] {
            let mut group = vec![
                serde_json::json!({"signature":String::from_utf8_lossy(&anchor),"name":"anchor","kind":"struct","size":8,"members":[
                {"name":"owner","kind":"form_id","targets":["NPC_","FACT"]},
                {"name":"condition","kind":"union","offset":4,"size":4,"decider":"inventory_owner_condition","fields":[
                    {"name":"unused","kind":"bytes","size":4}, {"name":"rank","kind":"i32"}]}]}),
            ];
            group.extend(members);
            let schema = Schema::parse(&serde_json::to_vec(&serde_json::json!({"version":1,"records":[{"signature":String::from_utf8_lossy(&record_type),"fields":[
                {"kind":"group","name":"owned","repeat":true,"fields":group},
                {"signature":"DATA","name":"later_damaged","kind":"u32"}]}]})).unwrap()).unwrap();
            let prepared = prepare_schema(&schema);
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("Owner.esm");
            let header = replay_probe_record(
                b"TES4",
                0,
                1,
                &replay_probe_subrecord(
                    b"HEDR",
                    &[
                        1.7f32.to_le_bytes(),
                        1u32.to_le_bytes(),
                        0x800u32.to_le_bytes(),
                    ]
                    .concat(),
                ),
            );
            let mut payload = replay_probe_subrecord(
                &anchor,
                &[0x1900u32.to_le_bytes(), 0x1234_5678u32.to_le_bytes()].concat(),
            );
            payload.extend(first.concat());
            payload.extend(replay_probe_subrecord(
                &anchor,
                &[0x1901u32.to_le_bytes(), (-7i32).to_le_bytes()].concat(),
            ));
            payload.extend(second.concat());
            payload.extend(replay_probe_subrecord(b"DATA", &[0x5A]));
            let source_bytes = [
                header.clone(),
                replay_probe_record(&record_type, 0x1880, 0, &payload),
            ]
            .concat();
            std::fs::write(&path, &source_bytes).unwrap();
            let order = LoadOrder::read(std::slice::from_ref(&path)).unwrap();
            let kinds = HashMap::from([
                (0x1900, *b"NPC_"),
                (0x1901, *b"FACT"),
                (0x1880, record_type),
            ]);
            let scan = |groups: bool| ScannedRecord {
                record_type,
                flags: 0,
                version_control: 0x1357_2468,
                form_version: 44,
                header_unknown: 0xA571,
                source_form_id: 0x1880,
                cell_form_id: groups.then_some(0x0500_1990),
                worldspace_form_id: groups.then_some(0x0600_1991),
                source_topic_form_id: None,
                topic_group_offset: None,
                source_record_offset: header.len() as u64,
                payload: payload.clone(),
            };
            let first_context = Context {
                order: &order,
                plugin_index: 0,
                localized: false,
                winning_types: None,
            };
            let resolved_context = Context {
                winning_types: Some(&kinds),
                ..first_context
            };
            let mut original_diagnostics = PluginDiagnostics::default();
            let mut selected = decode_with_prepared(
                scan(true),
                &schema,
                &prepared,
                &first_context,
                &mut original_diagnostics,
            )
            .unwrap();
            let mut source = DeferredSource::take(&mut selected);
            let mut baseline = PluginDiagnostics::default();
            let mut replay = PluginDiagnostics::default();
            replay_deferred(
                &mut selected,
                &mut source,
                &schema,
                &resolved_context,
                &mut baseline,
                &mut replay,
                &prepared,
            )
            .unwrap();
            let mut actual_diagnostics = replay_probe_diagnostics(&original_diagnostics);
            merge_new_diagnostics(&mut actual_diagnostics, &baseline, replay);
            let mut old_baseline = PluginDiagnostics::default();
            decode_with_prepared(
                scan(false),
                &schema,
                &prepared,
                &first_context,
                &mut old_baseline,
            )
            .unwrap();
            let mut old_replay = PluginDiagnostics::default();
            let expected = decode_with_prepared(
                scan(false),
                &schema,
                &prepared,
                &resolved_context,
                &mut old_replay,
            )
            .unwrap();
            let mut expected_diagnostics = replay_probe_diagnostics(&original_diagnostics);
            merge_new_diagnostics(&mut expected_diagnostics, &old_baseline, old_replay);
            assert_eq!(
                serde_json::to_vec(&selected.fields).unwrap(),
                serde_json::to_vec(&expected.fields).unwrap(),
                "{:?} companions",
                record_type
            );
            assert_eq!(selected.rejected_fields, expected.rejected_fields);
            assert!(!record_contains_deferred(&selected));
            assert_eq!(selected.fields[0].signature, anchor);
            assert_eq!(
                selected.fields[0].canonical_bytes,
                [0x1901u32.to_le_bytes(), (-7i32).to_le_bytes()].concat()
            );
            assert_eq!(
                serde_json::to_vec(&actual_diagnostics).unwrap(),
                serde_json::to_vec(&expected_diagnostics).unwrap()
            );
            assert_eq!(
                replay_probe_events(&actual_diagnostics),
                replay_probe_events(&expected_diagnostics)
            );
            // A later candidate failure restores the exact normal first-pass
            // prefix, even when an earlier optional anchor suppressed companions.
            let omitted = source.safe_prefix(&mut selected);
            assert_eq!(omitted, vec![anchor, anchor]);
            assert_eq!(
                selected
                    .fields
                    .iter()
                    .map(|field| field.signature)
                    .collect::<Vec<_>>(),
                if record_type == *b"TEST" {
                    vec![*b"CIS1", *b"CIS1", *b"CIS2"]
                } else if record_type == *b"INFO" {
                    vec![*b"NAM1", *b"NAM2", *b"NAM1", *b"NAM2"]
                } else {
                    vec![*b"VTXT", *b"VTXT", *b"VTXT", *b"VTXT"]
                }
            );
            assert_eq!(std::fs::read(&path).unwrap(), source_bytes);
        }
    }

    /// Companion suppression can rematch a duplicate tag and change the editor
    /// context of a later selector, including an originally rejected occurrence.
    #[test]
    fn deferred_replay_matches_full_decode_changed_bindings_and_editor_context() {
        for mode in ["tail", "editor_error", "editor_success"] {
            let anchor = serde_json::json!({"signature":"CTDA","name":"anchor","kind":"struct","size":8,"members":[
                {"name":"owner","kind":"form_id","targets":["NPC_","FACT"]},
                {"name":"condition","kind":"union","offset":4,"size":4,"decider":"inventory_owner_condition","fields":[
                    {"name":"unused","kind":"bytes","size":4}, {"name":"rank","kind":"i32"}]}]});
            let mut fields = Vec::new();
            if mode == "editor_success" {
                fields.push(serde_json::json!({"signature":"EDID","name":"earlier_editor","kind":"zstring"}));
            }
            fields.push(serde_json::json!({"name":"first_group","kind":"group","repeat":true,"fields":[anchor,
                if mode == "tail" { serde_json::json!({"signature":"TAIL","name":"integer_tail","kind":"u32"}) }
                else { serde_json::json!({"signature":"EDID","name":"rematched_editor","kind":"zstring"}) }]}));
            fields.push(serde_json::json!({"name":"second_group","kind":"group","repeat":true,"fields":[
                {"signature":"CIS1","name":"condition_string","kind":"zstring"},
                if mode == "tail" { serde_json::json!({"signature":"TAIL","name":"link_tail","kind":"form_id","targets":["NPC_"]}) }
                else { serde_json::json!({"signature":"EDID","name":"opaque_editor","kind":"bytes"}) }]}));
            if mode != "tail" {
                fields.push(serde_json::json!({"signature":"DATA","name":"selected_value","kind":"union","decider":"gmst_value","fields":[
                {"name":"float","kind":"f32"}, {"name":"integer","kind":"i32"}, {"name":"boolean","kind":"u32"}, {"name":"string","kind":"lstring"}]}));
            }
            fields.push(serde_json::json!({"signature":"LAST","name":"stable_after","kind":"u32"}));
            let schema = Schema::parse(&serde_json::to_vec(&serde_json::json!({"version":1,"records":[{"signature":"TEST","fields":fields}]})).unwrap()).unwrap();
            let prepared = prepare_schema(&schema);
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("Owner.esm");
            let header = replay_probe_record(
                b"TES4",
                0,
                1,
                &replay_probe_subrecord(
                    b"HEDR",
                    &[
                        1.7f32.to_le_bytes(),
                        1u32.to_le_bytes(),
                        0x800u32.to_le_bytes(),
                    ]
                    .concat(),
                ),
            );
            let mut payload = Vec::new();
            if mode == "editor_success" {
                payload.extend(replay_probe_subrecord(b"EDID", b"fEarlier\0"));
            }
            payload.extend(replay_probe_subrecord(
                b"CTDA",
                &[0x1900u32.to_le_bytes(), 0x1234_5678u32.to_le_bytes()].concat(),
            ));
            payload.extend(replay_probe_subrecord(b"CIS1", b"Discard\0"));
            if mode == "tail" {
                payload.extend(replay_probe_subrecord(
                    b"TAIL",
                    &0x0200_1899u32.to_le_bytes(),
                ));
            } else {
                payload.extend(replay_probe_subrecord(b"EDID", b"iProbe\0"));
                payload.extend(replay_probe_subrecord(b"DATA", &0.75f32.to_le_bytes()));
            }
            payload.extend(replay_probe_subrecord(
                b"LAST",
                &0x2468_1379u32.to_le_bytes(),
            ));
            let source_bytes = [
                header.clone(),
                replay_probe_record(b"TEST", 0x1880, 0, &payload),
            ]
            .concat();
            std::fs::write(&path, &source_bytes).unwrap();
            let order = LoadOrder::read(std::slice::from_ref(&path)).unwrap();
            let kinds = HashMap::from([(0x1900, *b"NPC_"), (0x1880, *b"TEST")]);
            let scan = || ScannedRecord {
                record_type: *b"TEST",
                flags: 0,
                version_control: 0x1357_2468,
                form_version: 44,
                header_unknown: 0xA571,
                source_form_id: 0x1880,
                cell_form_id: None,
                worldspace_form_id: None,
                source_topic_form_id: None,
                topic_group_offset: None,
                source_record_offset: header.len() as u64,
                payload: payload.clone(),
            };
            let first_context = Context {
                order: &order,
                plugin_index: 0,
                localized: false,
                winning_types: None,
            };
            let resolved_context = Context {
                winning_types: Some(&kinds),
                ..first_context
            };
            let mut original_diagnostics = PluginDiagnostics::default();
            FIELD_DECODE_VISITS.with(|visits| *visits.borrow_mut() = Some(BTreeMap::new()));
            let mut selected = decode_with_prepared(
                scan(),
                &schema,
                &prepared,
                &first_context,
                &mut original_diagnostics,
            )
            .unwrap();
            if mode == "editor_error" {
                assert_eq!(selected.rejected_fields, vec![*b"DATA"]);
            }
            if mode == "editor_success" {
                assert_eq!(
                    selected
                        .fields
                        .iter()
                        .find(|field| field.signature == *b"DATA")
                        .unwrap()
                        .value,
                    Value::Float(0.75)
                );
            }
            let mut source = DeferredSource::take(&mut selected);
            let mut baseline = PluginDiagnostics::default();
            let mut replay = PluginDiagnostics::default();
            replay_deferred(
                &mut selected,
                &mut source,
                &schema,
                &resolved_context,
                &mut baseline,
                &mut replay,
                &prepared,
            )
            .unwrap();
            let visits = FIELD_DECODE_VISITS.with(|visits| visits.borrow_mut().take().unwrap());
            assert_eq!(
                visits[&(0x1880, "owner".into())],
                1,
                "ordinary siblings stay cached during grammar rematching"
            );
            assert_eq!(visits[&(0x1880, "condition".into())], 2);
            assert_eq!(
                visits[&(0x1880, "stable_after".into())],
                1,
                "an EDID change cannot retry a context-independent descriptor"
            );
            let mut actual_diagnostics = replay_probe_diagnostics(&original_diagnostics);
            merge_new_diagnostics(&mut actual_diagnostics, &baseline, replay);
            let mut old_baseline = PluginDiagnostics::default();
            decode_with_prepared(
                scan(),
                &schema,
                &prepared,
                &first_context,
                &mut old_baseline,
            )
            .unwrap();
            let mut old_replay = PluginDiagnostics::default();
            let expected = decode_with_prepared(
                scan(),
                &schema,
                &prepared,
                &resolved_context,
                &mut old_replay,
            )
            .unwrap();
            let mut expected_diagnostics = replay_probe_diagnostics(&original_diagnostics);
            merge_new_diagnostics(&mut expected_diagnostics, &old_baseline, old_replay);
            assert_eq!(
                serde_json::to_vec(&selected).unwrap(),
                serde_json::to_vec(&expected).unwrap(),
                "{mode}: complete typed/native/canonical view"
            );
            assert_eq!(
                serde_json::to_vec(&actual_diagnostics).unwrap(),
                serde_json::to_vec(&expected_diagnostics).unwrap(),
                "{mode}: scalar/example parity"
            );
            assert_eq!(
                replay_probe_events(&actual_diagnostics),
                replay_probe_events(&expected_diagnostics),
                "{mode}: every diagnostic row"
            );
            if mode == "tail" {
                let tail = selected
                    .fields
                    .iter()
                    .find(|field| field.signature == *b"TAIL")
                    .unwrap();
                assert_eq!(tail.name, "integer_tail");
                assert_eq!(tail.value, Value::Unsigned(0x0200_1899));
                assert_eq!(tail.canonical_bytes, 0x0200_1899u32.to_le_bytes());
            } else {
                let data = selected
                    .fields
                    .iter()
                    .find(|field| field.signature == *b"DATA")
                    .unwrap();
                assert_eq!(
                    data.value,
                    Value::Signed(i64::from(i32::from_le_bytes(0.75f32.to_le_bytes())))
                );
                assert_eq!(data.canonical_bytes, 0.75f32.to_le_bytes());
                assert_eq!(
                    visits[&(0x1880, "selected_value".into())],
                    2,
                    "the changed editor context requires reinterpretation under the same descriptor"
                );
                assert!(!selected.rejected_fields.contains(b"DATA"));
            }
            assert!(!record_contains_deferred(&selected));
            assert_eq!(selected.raw_payload, payload);
            assert_eq!(std::fs::read(path).unwrap(), source_bytes);
        }
    }

    /// Earlier fields and enclosing structs keep their original selector bytes
    /// while typed links are cleared, and their owned canonical buffers are reused.
    #[test]
    fn target_validation_borrows_original_preceding_and_parent_buffers() {
        let schema = Schema::parse(br#"{"version":1,"records":[{"signature":"PACK","fields":[
            {"signature":"ANAM","name":"input_type","kind":"form_id","targets":["NPC_"]},
            {"signature":"CNAM","name":"input_value","kind":"union","decider":"ai_package_value","fields":[
                {"name":"unknown","kind":"bytes","targets":["ANY_"]}, {"name":"boolean","kind":"u32"},
                {"name":"integer_link","kind":"form_id","targets":["GLOB"]}, {"name":"float","kind":"f32"}]},
            {"signature":"COED","name":"ownership","kind":"struct","size":12,"members":[
                {"name":"owner","kind":"form_id","targets":["FACT"]},
                {"name":"condition","kind":"union","size":4,"offset":4,"decider":"inventory_owner_condition","fields":[
                    {"name":"unused","kind":"bytes","size":4,"targets":["ANY_"]},
                    {"name":"global","kind":"form_id","targets":["GLOB"]}, {"name":"rank","kind":"i32"}]},
                {"name":"weight","kind":"f32","offset":8}]}]}]}"#).unwrap();
        let prepared = prepare_schema(&schema);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Owner.esm");
        let header = replay_probe_record(
            b"TES4",
            0,
            1,
            &replay_probe_subrecord(
                b"HEDR",
                &[
                    1.7f32.to_le_bytes(),
                    1u32.to_le_bytes(),
                    0x800u32.to_le_bytes(),
                ]
                .concat(),
            ),
        );
        let payload = [
            replay_probe_subrecord(b"ANAM", b"Int\0"),
            replay_probe_subrecord(b"CNAM", &0x1801u32.to_le_bytes()),
            replay_probe_subrecord(
                b"COED",
                &[
                    0x1802u32.to_le_bytes(),
                    0x1801u32.to_le_bytes(),
                    0.375f32.to_le_bytes(),
                ]
                .concat(),
            ),
            replay_probe_subrecord(b"UNKN", &[0x71, 0xA5, 0xFF, 0, 0x91]),
        ]
        .concat();
        let source_bytes = [
            header.clone(),
            replay_probe_record(b"PACK", 0x1880, 0, &payload),
        ]
        .concat();
        std::fs::write(&path, &source_bytes).unwrap();
        let order = LoadOrder::read(std::slice::from_ref(&path)).unwrap();
        let kinds = HashMap::from([(0x1801, *b"FACT"), (0x1802, *b"NPC_")]);
        let mut diagnostics = PluginDiagnostics::default();
        let selected = decode_with_prepared(
            ScannedRecord {
                record_type: *b"PACK",
                flags: 0,
                version_control: 0x1357_2468,
                form_version: 44,
                header_unknown: 0xA571,
                source_form_id: 0x1880,
                cell_form_id: None,
                worldspace_form_id: None,
                source_topic_form_id: None,
                topic_group_offset: None,
                source_record_offset: header.len() as u64,
                payload: payload.clone(),
            },
            &schema,
            &prepared,
            &Context {
                order: &order,
                plugin_index: 0,
                localized: false,
                winning_types: Some(&kinds),
            },
            &mut diagnostics,
        )
        .unwrap();
        let pointers = selected
            .fields
            .iter()
            .map(|field| field.canonical_bytes.as_ptr())
            .collect::<Vec<_>>();
        let mut fact = selected.clone();
        fact.form_id = 0x1801;
        fact.record_type = *b"FACT";
        fact.fields.clear();
        let mut npc = selected.clone();
        npc.form_id = 0x1802;
        npc.record_type = *b"NPC_";
        npc.fields.clear();
        let mut result = ReadResult::default();
        result
            .records
            .extend([(0x1880, selected), (0x1801, fact), (0x1802, npc)]);
        validate_targets_with_prepared(&mut result, &schema, &prepared, &order);
        let selected = &result.records[&0x1880];
        assert_eq!(
            selected
                .fields
                .iter()
                .map(|field| field.canonical_bytes.as_ptr())
                .collect::<Vec<_>>(),
            pointers
        );
        assert_eq!(selected.raw_payload, payload);
        assert_eq!(selected.fields[0].canonical_bytes, 0u32.to_le_bytes());
        assert_eq!(selected.fields[1].value, Value::FormId(0));
        assert_eq!(selected.fields[1].canonical_bytes, 0u32.to_le_bytes());
        assert_eq!(
            selected.fields[2].canonical_bytes,
            [
                0u32.to_le_bytes(),
                0u32.to_le_bytes(),
                0.375f32.to_le_bytes()
            ]
            .concat()
        );
        assert_eq!(
            selected.fields[3].canonical_bytes,
            [0x71, 0xA5, 0xFF, 0, 0x91]
        );
        assert!(selected.fields[3].opaque);
        let diagnostics = &result.diagnostics[&order.names[selected.load_order as usize]];
        assert_eq!(diagnostics.invalid_links, 4);
        let conn = replay_probe_database();
        super::super::diagnostics::write_tables(&conn, [(0, &diagnostics.event_log)]).unwrap();
        let paths = conn
            .prepare("SELECT field FROM record_diagnostics ORDER BY field")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "ANAM[canonical:0].input_type.input_type",
                "CNAM[canonical:1].input_value.input_value.integer_link",
                "COED[canonical:2].ownership.ownership.condition.global",
                "COED[canonical:2].ownership.ownership.owner"
            ]
        );
        assert_eq!(std::fs::read(path).unwrap(), source_bytes);
    }

    /// Stable catalogue replay decodes normal fields once and the deferred leaf twice,
    /// without recounting earlier damaged fields or changing exact diagnostic events.
    #[test]
    fn deferred_replay_visits_only_unresolved_fields_and_preserves_diagnostics() {
        let schema = Schema::parse(br#"{
            "version":1,"records":[
                {"signature":"NPC_","fields":[{"signature":"EDID","name":"owner_editor","kind":"zstring"}]},
                {"signature":"TEST","fields":[
                    {"signature":"EDID","name":"probe_normal","kind":"zstring"},
                    {"signature":"DATA","name":"probe_number","kind":"u32"},
                    {"signature":"LINK","name":"probe_link","kind":"form_id","targets":["NPC_"]},
                    {"signature":"COED","name":"probe_ownership","kind":"struct","size":12,"members":[
                        {"name":"probe_owner","kind":"form_id","targets":["NPC_","FACT"]},
                        {"name":"probe_deferred","kind":"union","offset":4,"size":4,"decider":"inventory_owner_condition","fields":[
                            {"name":"unused","kind":"bytes","size":4},
                            {"name":"global","kind":"form_id","targets":["GLOB"]},
                            {"name":"rank","kind":"i32"}
                        ]},
                        {"name":"probe_weight","kind":"f32","offset":8}
                    ]}
                ]}
            ]}"#).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let header = replay_probe_record(
            b"TES4",
            0,
            1,
            &replay_probe_subrecord(
                b"HEDR",
                &[
                    1.7f32.to_le_bytes(),
                    3u32.to_le_bytes(),
                    0x800u32.to_le_bytes(),
                ]
                .concat(),
            ),
        );
        let spacer = directory.path().join("Spacer.esm");
        std::fs::write(&spacer, &header).unwrap();
        let owner_record = replay_probe_record(
            b"NPC_",
            0x1900,
            0,
            &replay_probe_subrecord(b"EDID", b"Owner\0"),
        );
        let ownership = [
            0x1900u32.to_le_bytes(),
            0x0200_1899u32.to_le_bytes(),
            0.375f32.to_le_bytes(),
        ]
        .concat();
        let probe_payload = [
            replay_probe_subrecord(b"EDID", b"DeferredProbe\0"),
            replay_probe_subrecord(b"DATA", &[0x5A]),
            replay_probe_subrecord(b"LINK", &0x0200_1888u32.to_le_bytes()),
            replay_probe_subrecord(b"COED", &ownership),
        ]
        .concat();
        let neighbor_payload = [
            replay_probe_subrecord(b"EDID", b"UsableNeighbor\0"),
            replay_probe_subrecord(b"DATA", &0x2468_1379u32.to_le_bytes()),
        ]
        .concat();
        let source = [
            header,
            owner_record,
            replay_probe_record(b"TEST", 0x1880, 0, &probe_payload),
            replay_probe_record(b"TEST", 0x1881, 0, &neighbor_payload),
        ]
        .concat();
        let owner_path = directory.path().join("Owner.esm");
        std::fs::write(&owner_path, &source).unwrap();
        let paths = vec![spacer, owner_path.clone()];
        let order = LoadOrder::read(&paths).unwrap();
        FIELD_DECODE_VISITS.with(|visits| *visits.borrow_mut() = Some(BTreeMap::new()));
        let result = super::super::read_plugins_with_validation_and_observer_with_schema(
            &paths,
            &order,
            &schema,
            |_| Ok(()),
            |_| {},
        )
        .unwrap();
        let visits = FIELD_DECODE_VISITS.with(|visits| visits.borrow_mut().take().unwrap());
        assert_eq!(
            visits[&(0x1880, "probe_normal".into())],
            1,
            "ordinary fields must not be decoded by a baseline or replay"
        );
        assert_eq!(
            visits[&(0x1880, "probe_number".into())],
            1,
            "an already rejected field must not be retried"
        );
        assert_eq!(
            visits[&(0x1880, "probe_owner".into())],
            1,
            "an already remapped sibling must not be decoded again"
        );
        assert_eq!(
            visits[&(0x1880, "probe_deferred".into())],
            2,
            "only the unresolved leaf needs a second decode"
        );
        let record = &result.records[&0x0100_1880];
        assert_eq!(record.raw_payload, probe_payload);
        assert_eq!(
            record
                .fields
                .iter()
                .map(|field| field.signature)
                .collect::<Vec<_>>(),
            vec![*b"EDID", *b"LINK", *b"COED"]
        );
        assert_eq!(record.fields[1].canonical_bytes, 0u32.to_le_bytes());
        assert_eq!(
            record.fields[2].canonical_bytes,
            [
                0x0100_1900u32.to_le_bytes(),
                0u32.to_le_bytes(),
                0.375f32.to_le_bytes()
            ]
            .concat()
        );
        assert!(!record_contains_deferred(record));
        assert!(result.records.contains_key(&0x0100_1900));
        assert_eq!(
            result.records[&0x0100_1881].fields[1].value,
            Value::Unsigned(0x2468_1379)
        );
        let diagnostics = &result.diagnostics[&order.names[record.load_order as usize]];
        assert_eq!(
            (
                diagnostics.skipped_records,
                diagnostics.skipped_fields,
                diagnostics.invalid_links,
                diagnostics.unexpected_subrecords,
                diagnostics.compatibility_headers
            ),
            (0, 1, 2, 0, 0)
        );
        assert!(diagnostics.first_by_category["field"].contains("expected 4 bytes, got 1"));
        // At the same source record, the lexical COED path precedes LINK even though
        // deferred replay observes it later. Both events remain in the complete journal.
        assert!(diagnostics.first_by_category["link"].contains("02001899"));
        let conn = replay_probe_database();
        super::super::diagnostics::write_tables(&conn, [(1, &diagnostics.event_log)]).unwrap();
        let events: Vec<(String, i64, Option<u32>, String)> = conn.prepare("SELECT field,occurrence,link_value,category FROM record_diagnostics WHERE source_form_id=6272 ORDER BY field").unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(
            events,
            vec![
                (
                    "COED[source:3].probe_ownership.probe_deferred.global".into(),
                    2,
                    Some(0x0200_1899),
                    "link".into()
                ),
                ("DATA[source:1]".into(), 0, None, "field".into()),
                (
                    "LINK[source:2].probe_link".into(),
                    1,
                    Some(0x0200_1888),
                    "link".into()
                ),
            ]
        );
        // Independent legacy oracle: decode the entire source without and with the
        // final catalogue, then apply the original diagnostic difference policy.
        let prepared = prepare_schema(&schema);
        let kinds = result
            .records
            .iter()
            .map(|(id, record)| (*id, record.record_type))
            .collect::<HashMap<_, _>>();
        let scan = || ScannedRecord {
            record_type: record.record_type,
            flags: record.flags,
            version_control: record.version_control,
            form_version: record.form_version,
            header_unknown: record.header_unknown,
            source_form_id: record.source_form_id,
            cell_form_id: None,
            worldspace_form_id: None,
            source_topic_form_id: record.source_topic_form_id,
            topic_group_offset: record.topic_group_offset,
            source_record_offset: record.source_record_offset,
            payload: probe_payload.clone(),
        };
        let mut baseline = PluginDiagnostics::default();
        decode_with_prepared(
            scan(),
            &schema,
            &prepared,
            &Context {
                order: &order,
                plugin_index: 1,
                localized: false,
                winning_types: None,
            },
            &mut baseline,
        )
        .unwrap();
        let mut replay = PluginDiagnostics::default();
        let expected = decode_with_prepared(
            scan(),
            &schema,
            &prepared,
            &Context {
                order: &order,
                plugin_index: 1,
                localized: false,
                winning_types: Some(&kinds),
            },
            &mut replay,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_vec(&record.fields).unwrap(),
            serde_json::to_vec(&expected.fields).unwrap()
        );
        let mut oracle = PluginDiagnostics::default();
        oracle.event_log.merge_from(&baseline.event_log);
        oracle.skipped_fields = baseline.skipped_fields;
        oracle.invalid_links = baseline.invalid_links;
        oracle.merge_summaries(&baseline);
        merge_new_diagnostics(&mut oracle, &baseline, replay);
        assert_eq!(
            (
                diagnostics.skipped_records,
                diagnostics.skipped_fields,
                diagnostics.invalid_links,
                diagnostics.unexpected_subrecords,
                diagnostics.compatibility_headers
            ),
            (
                oracle.skipped_records,
                oracle.skipped_fields,
                oracle.invalid_links,
                oracle.unexpected_subrecords,
                oracle.compatibility_headers
            )
        );
        assert_eq!(diagnostics.first_example, oracle.first_example);
        assert_eq!(diagnostics.first_by_category, oracle.first_by_category);
        let oracle_conn = replay_probe_database();
        super::super::diagnostics::write_tables(&oracle_conn, [(1, &oracle.event_log)]).unwrap();
        // IDs are PK-ordered by the private event key; comparing every published
        // column also checks path, ordinal, original link, reason and provenance.
        let rows = |conn: &rusqlite::Connection| {
            let mut query = conn.prepare("SELECT * FROM record_diagnostics WHERE source_form_id=6272 ORDER BY diagnostic_id").unwrap();
            let columns = query.column_count();
            query
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get::<_, rusqlite::types::Value>(column))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        };
        assert_eq!(rows(&conn), rows(&oracle_conn));
        assert_eq!(std::fs::read(owner_path).unwrap(), source);
    }

    /// Count prefixes and mixed structs retain every canonical byte; an optional policy
    /// marker forces the unchanged materialized path for an asymmetric decoder oracle.
    #[test]
    fn compact_arrays_match_materialized_counts_padding_and_field_local_errors() {
        let schema = Schema::parse(br#"{"version":1,"records":[]}"#).unwrap();
        let order = LoadOrder::read(&[]).unwrap();
        let context = Context {
            order: &order,
            plugin_index: 0,
            localized: false,
            winning_types: None,
        };
        let selection = super::super::deciders::Context {
            record_type: *b"TEST",
            form_version: 44,
            editor_id: "",
            preceding_subrecords: &[],
            parent_payload: None,
            link_record_type: None,
        };
        let mut diagnostics = PluginDiagnostics::default();
        let mut field = FieldSchema {
            kind: "array".into(),
            count: Some(serde_json::json!({"kind":"prefix_u32","stride":8})),
            members: vec![
                FieldSchema {
                    name: "weight".into(),
                    kind: "f32".into(),
                    offset: 4,
                    ..Default::default()
                },
                FieldSchema {
                    name: "index".into(),
                    kind: "u16".into(),
                    ..Default::default()
                },
                FieldSchema {
                    name: "padding".into(),
                    kind: "bytes".into(),
                    offset: 2,
                    size: Some(2),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut bytes = 2_u32.to_le_bytes().to_vec();
        bytes.extend([1, 128, 99, 255]);
        bytes.extend((-0.0_f32).to_le_bytes());
        bytes.extend([7, 0, 66, 77]);
        bytes.extend(f32::from_bits(1).to_le_bytes());
        let (compact, canonical) = array(
            &field,
            &bytes,
            &schema,
            &context,
            &mut diagnostics,
            &selection,
            1,
        )
        .unwrap();
        assert!(matches!(&compact, Value::CompactArray(_)));
        assert_eq!(canonical, bytes);
        field.decider = Some("size".into());
        let (old, old_canonical) = array(
            &field,
            &bytes,
            &schema,
            &context,
            &mut diagnostics,
            &selection,
            1,
        )
        .unwrap();
        assert!(matches!(&old, Value::Array(_)));
        assert_eq!(compact, old);
        assert_eq!(canonical, old_canonical);
        assert_eq!(
            serde_json::to_vec(&compact).unwrap(),
            serde_json::to_vec(&old).unwrap()
        );
        field.decider = None;
        // An invalid second element must reject the field, never publish a prefix array.
        let mut bad = bytes.clone();
        bad[16..20].copy_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(
            array(
                &field,
                &bad,
                &schema,
                &context,
                &mut diagnostics,
                &selection,
                1
            )
            .unwrap_err(),
            "non-finite float"
        );
        assert!(
            array(
                &field,
                &bytes,
                &schema,
                &context,
                &mut diagnostics,
                &selection,
                1
            )
            .is_ok()
        );
        // Framing/extent failures still use the preexisting path and diagnostic boundary.
        assert_eq!(
            array(
                &field,
                &bytes[..19],
                &schema,
                &context,
                &mut diagnostics,
                &selection,
                1
            )
            .unwrap_err(),
            "array extent/count mismatch"
        );
        assert_eq!(diagnostics.invalid_links, 0);
        assert!(!contains_target_link(&compact));
        let mut after = canonical.clone();
        let mut compact = compact;
        validate_value(
            &mut compact,
            &mut after,
            &field,
            &schema,
            &HashMap::new(),
            &mut diagnostics,
            &selection,
        );
        assert_eq!(after, canonical);
    }

    /// Preserve fixed, remaining-byte and empty-array counts across both storage paths.
    #[test]
    fn compact_fixed_remaining_and_empty_arrays_preserve_count_authority() {
        let schema = Schema::parse(br#"{"version":1,"records":[]}"#).unwrap();
        let order = LoadOrder::read(&[]).unwrap();
        let context = Context {
            order: &order,
            plugin_index: 0,
            localized: false,
            winning_types: None,
        };
        let selection = super::super::deciders::Context {
            record_type: *b"TEST",
            form_version: 44,
            editor_id: "",
            preceding_subrecords: &[],
            parent_payload: None,
            link_record_type: None,
        };
        let mut diagnostics = PluginDiagnostics::default();
        let mut field = FieldSchema {
            kind: "array".into(),
            size: Some(1),
            members: vec![FieldSchema {
                kind: "i8".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        for rule in [
            serde_json::json!({"kind":"remaining"}),
            serde_json::json!({"kind":"fixed","value":3}),
        ] {
            field.count = Some(rule);
            let (actual, bytes) = array(
                &field,
                &[128, 0, 127],
                &schema,
                &context,
                &mut diagnostics,
                &selection,
                1,
            )
            .unwrap();
            assert_eq!(
                actual,
                Value::Array(vec![
                    Value::Signed(-128),
                    Value::Signed(0),
                    Value::Signed(127)
                ])
            );
            assert_eq!(bytes, [128, 0, 127]);
        }
        field.count = Some(serde_json::json!({"kind":"fixed","value":0}));
        let (empty, _) = array(
            &field,
            &[],
            &schema,
            &context,
            &mut diagnostics,
            &selection,
            1,
        )
        .unwrap();
        assert!(matches!(empty, Value::Array(values) if values.is_empty()));
        field.count = Some(serde_json::json!({"kind":"fixed","value":1_000_001}));
        assert_eq!(
            array(
                &field,
                &[],
                &schema,
                &context,
                &mut diagnostics,
                &selection,
                1
            )
            .unwrap_err(),
            "array exceeds one million elements"
        );
        field.count = None;
        field.size = Some(0);
        assert_eq!(
            array(
                &field,
                &[],
                &schema,
                &context,
                &mut diagnostics,
                &selection,
                1
            )
            .unwrap_err(),
            "zero array stride"
        );
    }

    /// Unknown and out-of-order fields cannot rewind a nonrepeating schema cursor.
    #[test]
    fn matching_respects_order_and_repeat_groups() {
        let fields = vec![
            FieldSchema {
                signature: Some("EDID".into()),
                kind: "zstring".into(),
                ..Default::default()
            },
            FieldSchema {
                kind: "group".into(),
                repeat: true,
                fields: vec![
                    FieldSchema {
                        signature: Some("BTXT".into()),
                        kind: "bytes".into(),
                        ..Default::default()
                    },
                    FieldSchema {
                        signature: Some("ATXT".into()),
                        kind: "bytes".into(),
                        repeat: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            FieldSchema {
                signature: Some("DATA".into()),
                kind: "bytes".into(),
                ..Default::default()
            },
        ];
        let schema = Schema {
            version: 1,
            sources: Default::default(),
            definitions: Default::default(),
            common_fields: Default::default(),
            records: vec![],
        };
        let mut entries = Vec::new();
        flatten(&fields, &schema, &mut entries);
        let record = PreparedRecord {
            entries,
            unordered: false,
            extensions: Vec::new(),
        };
        assert_eq!(match_field(&record, b"BTXT", 2, Some(1)), Some(1));
        assert_eq!(match_field(&record, b"EDID", 2, Some(1)), None);
        assert_eq!(match_field(&record, b"ZZZZ", 1, Some(0)), None);
        assert_eq!(match_field(&record, b"DATA", 1, Some(0)), Some(3));
    }

    /// XXXX frames large fields, while a corrupt neighbor preserves the established prefix.
    #[test]
    fn extended_sizes_and_truncation_are_bounded() {
        let mut payload = b"XXXX\x04\x00".to_vec();
        payload.extend(70_000u32.to_le_bytes());
        payload.extend(b"TEST\x00\x00");
        payload.resize(payload.len() + 70_000, 19);
        payload.extend(b"BAD_\x20\x00x");
        let mut diagnostics = PluginDiagnostics::default();
        let fields = subrecords(&payload, &mut diagnostics, 123);
        assert_eq!(fields.fields.len(), 1);
        assert_eq!(fields.fields[0].1.len(), 70_000);
        assert!(!fields.complete);
        assert_eq!(diagnostics.skipped_fields, 1);
    }

    /// Matching a repeated male child must not jump to a later female role with the same tag.
    #[test]
    fn active_repeat_precedes_later_duplicate_signature() {
        let tag = |signature: &str| FieldSchema {
            signature: Some(signature.into()),
            kind: "bytes".into(),
            ..Default::default()
        };
        let fields = vec![
            FieldSchema {
                kind: "group".into(),
                repeat: true,
                fields: vec![tag("INDX"), tag("MODL")],
                ..Default::default()
            },
            tag("FNAM"),
            FieldSchema {
                kind: "group".into(),
                repeat: true,
                fields: vec![tag("INDX"), tag("MODL")],
                ..Default::default()
            },
        ];
        let schema = Schema::parse(br#"{"version":1,"records":[]}"#).unwrap();
        let mut entries = Vec::new();
        flatten(&fields, &schema, &mut entries);
        let prepared = PreparedRecord {
            entries,
            unordered: false,
            extensions: Vec::new(),
        };
        assert_eq!(match_field(&prepared, b"INDX", 2, Some(1)), Some(0));
        assert_eq!(match_field(&prepared, b"FNAM", 2, Some(1)), Some(2));
        assert_eq!(match_field(&prepared, b"INDX", 3, Some(2)), Some(3));
    }

    /// Accepted optional tails omit whole members and reject a partial trailing float.
    #[test]
    fn optional_members_are_whole_checked_tails() {
        let schema = Schema::parse(br#"{"version":1,"records":[]}"#).unwrap();
        let field = FieldSchema {
            kind: "struct".into(),
            sizes: vec![4, 6, 8],
            members: vec![
                FieldSchema {
                    name: "flags".into(),
                    kind: "u32".into(),
                    ..Default::default()
                },
                FieldSchema {
                    name: "tail".into(),
                    kind: "f32".into(),
                    offset: 4,
                    optional: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let order = LoadOrder::read(&[]).unwrap();
        let context = Context {
            order: &order,
            plugin_index: 0,
            localized: false,
            winning_types: None,
        };
        let selection = super::super::deciders::Context {
            record_type: *b"TEST",
            form_version: 44,
            editor_id: "",
            preceding_subrecords: &[],
            parent_payload: None,
            link_record_type: None,
        };
        let mut diagnostics = PluginDiagnostics::default();
        let (decoded, _) = value(
            &field,
            &[3, 0, 0, 0],
            &schema,
            &context,
            &mut diagnostics,
            &selection,
            0,
        )
        .unwrap();
        assert_eq!(
            decoded,
            Value::Struct(vec![("flags".into(), Value::Unsigned(3))])
        );
        assert!(
            value(
                &field,
                &[3, 0, 0, 0, 0, 0],
                &schema,
                &context,
                &mut diagnostics,
                &selection,
                0
            )
            .is_err()
        );
        let mut extended = 3u32.to_le_bytes().to_vec();
        extended.extend(1.25f32.to_le_bytes());
        assert!(
            value(
                &field,
                &extended,
                &schema,
                &context,
                &mut diagnostics,
                &selection,
                0
            )
            .is_ok()
        );
    }

    /// Counted alternate-texture names end at their first zero like every other
    /// plugin string, whether or not the counted span includes the terminator.
    #[test]
    fn alternate_texture_names_stop_at_the_first_zero() {
        let schema = Schema::parse(
            br#"{"version":1,"definitions":{
                "alternate_textures":{"name":"alternate_textures","kind":"array","decider":"alternate_textures"}},
                "records":[{"signature":"STAT","fields":[
                    {"signature":"MODS","definition":"alternate_textures"}]}]}"#,
        )
        .unwrap();
        let prepared = prepare_schema(&schema);
        let directory = tempfile::tempdir().unwrap();
        let owner = directory.path().join("Owner.esm");
        std::fs::write(
            &owner,
            replay_probe_record(
                b"TES4",
                0,
                1,
                &replay_probe_subrecord(
                    b"HEDR",
                    &[
                        1.7f32.to_le_bytes(),
                        1u32.to_le_bytes(),
                        0x800u32.to_le_bytes(),
                    ]
                    .concat(),
                ),
            ),
        )
        .unwrap();
        let order = LoadOrder::read(std::slice::from_ref(&owner)).unwrap();
        // The first name counts its terminator, the second omits it, and the
        // third carries bytes after the zero.
        let names: [&[u8]; 3] = [b"OddName\0", b"Uncounted", b"Cut\0tail"];
        let mut payload = u32::try_from(names.len()).unwrap().to_le_bytes().to_vec();
        for (index, name) in names.iter().enumerate() {
            payload.extend(u32::try_from(name.len()).unwrap().to_le_bytes());
            payload.extend(*name);
            payload.extend(0x1401u32.to_le_bytes());
            payload.extend(u32::try_from(index).unwrap().to_le_bytes());
        }
        let scan = ScannedRecord {
            record_type: *b"STAT",
            flags: 0,
            version_control: 0x1357_2468,
            form_version: 44,
            header_unknown: 0xA571,
            source_form_id: 0x1800,
            cell_form_id: None,
            worldspace_form_id: None,
            source_topic_form_id: None,
            topic_group_offset: None,
            source_record_offset: 0,
            payload: replay_probe_subrecord(b"MODS", &payload),
        };
        let context = Context {
            order: &order,
            plugin_index: 0,
            localized: false,
            winning_types: None,
        };
        let mut diagnostics = PluginDiagnostics::default();
        let record =
            decode_with_prepared(scan, &schema, &prepared, &context, &mut diagnostics).unwrap();
        let Value::Array(textures) = &record.fields[0].value else {
            panic!("alternate textures did not decode as an array");
        };
        let decoded = textures
            .iter()
            .map(|texture| {
                let Value::Struct(members) = texture else {
                    panic!("alternate texture is not a struct");
                };
                members
                    .iter()
                    .find(|(name, _)| name == "name")
                    .expect("alternate texture name")
                    .1
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            decoded,
            vec![
                Value::String("OddName".into()),
                Value::String("Uncounted".into()),
                Value::String("Cut".into()),
            ]
        );
    }

    /// A failed replay restarts the pending scan, so the winning-kind catalogue and
    /// the history keys are built once per resolution instead of once per round.
    #[test]
    fn deferred_recovery_builds_the_catalogue_once_per_resolution() {
        let schema = Schema::parse(
            br#"{"version":1,"records":[
                {"signature":"NPC_","fields":[{"signature":"EDID","name":"owner_editor","kind":"zstring"}]},
                {"signature":"TEST","fields":[
                    {"signature":"EDID","name":"probe_editor","kind":"zstring"},
                    {"signature":"COED","name":"probe_ownership","kind":"struct","size":16,"reject_record_on_error":true,"members":[
                        {"name":"probe_owner","kind":"form_id","targets":["NPC_","FACT"]},
                        {"name":"probe_deferred","kind":"union","offset":4,"size":4,"decider":"inventory_owner_condition","fields":[
                            {"name":"unused","kind":"bytes","size":4},
                            {"name":"global","kind":"form_id","size":8,"targets":["GLOB"]},
                            {"name":"rank","kind":"i32"}]},
                        {"name":"probe_later","kind":"form_id","offset":8,"targets":["NPC_"]},
                        {"name":"probe_weight","kind":"f32","offset":12}]}
                ]}
            ]}"#,
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Owner.esm");
        let ownership = |owner: u32| {
            [
                owner.to_le_bytes(),
                0u32.to_le_bytes(),
                0u32.to_le_bytes(),
                0.375f32.to_le_bytes(),
            ]
            .concat()
        };
        let mut source = replay_probe_record(
            b"TES4",
            0,
            1,
            &replay_probe_subrecord(
                b"HEDR",
                &[
                    1.7f32.to_le_bytes(),
                    3u32.to_le_bytes(),
                    0x800u32.to_le_bytes(),
                ]
                .concat(),
            ),
        );
        source.extend(replay_probe_record(
            b"NPC_",
            0x1900,
            0,
            &replay_probe_subrecord(b"EDID", b"Owner\0"),
        ));
        // Stable winners sort first, so every failing round re-replays them.
        let stable_count = 10u32;
        for index in 0..stable_count {
            let payload = [
                replay_probe_subrecord(b"EDID", format!("Stable{index}\0").as_bytes()),
                replay_probe_subrecord(b"COED", &ownership(0)),
            ]
            .concat();
            source.extend(replay_probe_record(b"TEST", 0x1800 + index, 0, &payload));
        }
        let failing_count = 40u32;
        let first_failing = 0x1810;
        for index in 0..failing_count {
            let payload = [
                replay_probe_subrecord(b"EDID", format!("Failing{index}\0").as_bytes()),
                replay_probe_subrecord(b"COED", &ownership(0x1900)),
            ]
            .concat();
            source.extend(replay_probe_record(
                b"TEST",
                first_failing + index,
                0,
                &payload,
            ));
        }
        std::fs::write(&path, &source).unwrap();
        let paths = vec![path];
        let order = LoadOrder::read(&paths).unwrap();
        DEFERRED_CATALOGUE_BUILDS.with(|builds| builds.set(Some(0)));
        let result = super::super::read_plugins_with_validation_and_observer_with_schema(
            &paths,
            &order,
            &schema,
            |_| Ok(()),
            |_| {},
        )
        .unwrap();
        let builds = DEFERRED_CATALOGUE_BUILDS.with(|builds| builds.take().unwrap());
        assert_eq!(
            builds, 1,
            "deferred recovery rebuilt the winning catalogue once per failed round"
        );
        assert_eq!(
            result.records.len(),
            (stable_count + failing_count + 1) as usize
        );
        for index in 0..stable_count {
            let record = &result.records[&(0x1800 + index)];
            assert_eq!(
                record
                    .fields
                    .iter()
                    .map(|field| field.signature)
                    .collect::<Vec<_>>(),
                vec![*b"EDID", *b"COED"]
            );
            assert!(!record_contains_deferred(record));
        }
        for index in 0..failing_count {
            let id = first_failing + index;
            let record = &result.records[&id];
            assert_eq!(
                record
                    .fields
                    .iter()
                    .map(|field| field.signature)
                    .collect::<Vec<_>>(),
                vec![*b"EDID"],
                "the mandatory anchor is omitted from {id:08X}"
            );
            assert_eq!(record.rejected_fields, vec![*b"COED"]);
            let history = result
                .history
                .iter()
                .find(|entry| entry.form_id == Some(id) && entry.record_type == *b"TEST")
                .expect("recovered occurrence history");
            assert!(history.accepted, "recovered winner {id:08X} is admitted");
            assert!(
                history
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("deferred recovery omitted COED")),
                "recovered winner {id:08X} records its omission"
            );
        }
    }

    /// A winner rejected without a successor leaves the winning-kind catalogue
    /// before the next replay, so a dependent naming it no longer selects the
    /// alternative its former kind implied.
    #[test]
    fn deferred_recovery_drops_a_removed_winner_before_its_dependents_replay() {
        let ownership = r#"{"signature":"COED","name":"probe_ownership","kind":"struct","size":16,"reject_record_on_error":true,"members":[
                        {"name":"probe_owner","kind":"form_id","targets":["NPC_","FACT"]},
                        {"name":"probe_deferred","kind":"union","offset":4,"size":4,"decider":"inventory_owner_condition","fields":[
                            {"name":"unused","kind":"bytes","size":4},
                            {"name":"global","kind":"form_id","size":8,"targets":["GLOB"]},
                            {"name":"rank","kind":"i32"}]},
                        {"name":"probe_later","kind":"form_id","offset":8,"targets":["NPC_"]},
                        {"name":"probe_weight","kind":"f32","offset":12}]}"#;
        let schema = Schema::parse(
            format!(
                r#"{{"version":1,"records":[
                {{"signature":"NPC_","fields":[{{"signature":"EDID","name":"owner_editor","kind":"zstring"}},{ownership}]}},
                {{"signature":"TEST","fields":[{{"signature":"EDID","name":"probe_editor","kind":"zstring"}},{ownership}]}}
            ]}}"#
            )
            .as_bytes(),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Owner.esm");
        let coed = |owner: u32| {
            replay_probe_subrecord(
                b"COED",
                &[
                    owner.to_le_bytes(),
                    0u32.to_le_bytes(),
                    0u32.to_le_bytes(),
                    0.375f32.to_le_bytes(),
                ]
                .concat(),
            )
        };
        let mut source = replay_probe_record(
            b"TES4",
            0,
            1,
            &replay_probe_subrecord(
                b"HEDR",
                &[
                    1.7f32.to_le_bytes(),
                    3u32.to_le_bytes(),
                    0x800u32.to_le_bytes(),
                ]
                .concat(),
            ),
        );
        let anchor = 0x1900;
        let removed = 0x1820;
        let dependent = 0x1830;
        source.extend(replay_probe_record(
            b"NPC_",
            anchor,
            0,
            &replay_probe_subrecord(b"EDID", b"Anchor\0"),
        ));
        // An NPC_ owner selects the oversized global alternative, so this winner
        // fails its replay; it has no predecessor and its prefix is refused below.
        source.extend(replay_probe_record(
            b"NPC_",
            removed,
            0,
            &[replay_probe_subrecord(b"EDID", b"Removed\0"), coed(anchor)].concat(),
        ));
        // Replayed after the removal, the dependent must see its owner as absent.
        source.extend(replay_probe_record(
            b"TEST",
            dependent,
            0,
            &[
                replay_probe_subrecord(b"EDID", b"Dependent\0"),
                coed(removed),
            ]
            .concat(),
        ));
        std::fs::write(&path, &source).unwrap();
        let paths = vec![path];
        let order = LoadOrder::read(&paths).unwrap();
        let result = super::super::read_plugins_with_validation_and_observer_with_schema(
            &paths,
            &order,
            &schema,
            |record| {
                let owned = record
                    .fields
                    .iter()
                    .any(|field| field.signature == *b"COED");
                if record.form_id == removed && !owned {
                    Err("probe owner needs its ownership".to_owned())
                } else {
                    Ok(())
                }
            },
            |_| {},
        )
        .unwrap();
        assert!(
            !result.records.contains_key(&removed),
            "the refused prefix leaves no winner"
        );
        let history = result
            .history
            .iter()
            .find(|entry| entry.form_id == Some(removed))
            .expect("removed occurrence history");
        assert!(!history.accepted);
        assert!(
            history
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("recovered prefix unusable")),
            "the removal records why its prefix was refused"
        );
        let record = &result.records[&dependent];
        assert_eq!(
            record
                .fields
                .iter()
                .map(|field| field.signature)
                .collect::<Vec<_>>(),
            vec![*b"EDID", *b"COED"],
            "the dependent replays against the catalogue without its removed owner"
        );
        assert!(record.rejected_fields.is_empty());
        assert!(!record_contains_deferred(record));
    }
}
