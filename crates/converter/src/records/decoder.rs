//! Ordered subrecord matching and generic values with typed FormID resolution.
use super::{
    DecodedField, DecodedRecord, PluginDiagnostics, ReadResult, Value,
    scanner::ScannedRecord,
    schema_format::{FieldSchema, Schema},
};
use crate::esm::load_order::LoadOrder;
use std::{collections::HashMap, sync::OnceLock};

/// Per-file interpretation supplied by the shared load-order authority.
pub(crate) struct Context<'a> {
    pub order: &'a LoadOrder,
    pub plugin_index: usize,
    pub localized: bool,
}

/// One flattened subrecord occurrence, including its enclosing repeat groups.
struct Entry {
    field: FieldSchema,
    repeat_ranges: Vec<(usize, usize)>,
}

/// Prepared schemas avoid re-expanding reusable definitions for every record.
struct PreparedRecord {
    entries: Vec<Entry>,
    unordered: bool,
}

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
                    entry.repeat_ranges.push((start, end));
                }
            }
        } else if field.signature.is_some() {
            output.push(Entry {
                field,
                repeat_ranges: Vec::new(),
            });
        }
    }
}

/// Prepare the build-validated embedded schema exactly once per process.
fn prepared() -> &'static HashMap<[u8; 4], PreparedRecord> {
    static PREPARED: OnceLock<HashMap<[u8; 4], PreparedRecord>> = OnceLock::new();
    PREPARED.get_or_init(|| {
        let schema = super::schema();
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
                    },
                )
            })
            .collect()
    })
}

/// Match a file-order occurrence without moving the cursor on an unknown tag.
fn match_field(
    record: &PreparedRecord,
    signature: &[u8; 4],
    cursor: usize,
    last: Option<usize>,
) -> Option<usize> {
    let matches = |index: usize| {
        record.entries[index]
            .field
            .signature
            .as_deref()
            .is_some_and(|tag| tag.as_bytes() == signature)
    };
    if let Some(index) = (cursor..record.entries.len()).find(|&index| matches(index)) {
        return Some(index);
    }
    if let Some(last) = last {
        for &(start, end) in &record.entries[last].repeat_ranges {
            if cursor <= end
                && let Some(index) = (start..end).find(|&index| matches(index))
            {
                return Some(index);
            }
        }
    }
    if record.unordered {
        return (0..record.entries.len()).find(|&index| matches(index));
    }
    None
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
    if value == 0 {
        return Ok(0);
    }
    let masters = &context.order.metadata[context.plugin_index].masters;
    let index = (value >> 24) as usize;
    if optional && index > masters.len() {
        diagnostics.note("link", || {
            format!("optional FormID {value:08X} exceeds master list")
        });
        return Ok(0);
    }
    let owner = if index < masters.len() {
        masters[index].to_ascii_lowercase()
    } else {
        context.order.names[context.plugin_index].clone()
    };
    if let Some(&slot) = context.order.light.get(&owner) {
        if value & 0x00ff_ffff > 0xfff {
            if optional {
                diagnostics.note("link", || {
                    format!("optional light FormID {value:08X} is wider than 12 bits")
                });
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
        diagnostics.note("link", || {
            format!("optional FormID {value:08X} has absent owner {owner}")
        });
        Ok(0)
    } else {
        Err(format!(
            "record FormID {value:08X} has absent owner {owner}"
        ))
    }
}

/// Decode only matched fields; retain source bytes separately for raw-record preservation.
pub(crate) fn decode(
    scanned: ScannedRecord,
    schema: &Schema,
    context: &Context<'_>,
    diagnostics: &mut PluginDiagnostics,
) -> Result<DecodedRecord, String> {
    let id = remap(scanned.source_form_id, context, false, diagnostics)?;
    if id == 0 && !matches!(&scanned.record_type, b"GMST" | b"TES4") {
        return Err("non-GMST record has null FormID".into());
    }
    let cell = scanned
        .cell_form_id
        .map(|id| remap(id, context, true, diagnostics))
        .transpose()?
        .filter(|&id| id != 0);
    let world = scanned
        .worldspace_form_id
        .map(|id| remap(id, context, true, diagnostics))
        .transpose()?
        .filter(|&id| id != 0);
    let record_schema = prepared().get(&scanned.record_type);
    let mut cursor = 0;
    let mut last = None;
    let mut fields = Vec::new();
    let mut rejected_fields = Vec::new();
    let mut editor_id = String::new();
    let framed = subrecords(&scanned.payload, diagnostics, scanned.source_form_id);
    for (signature, bytes) in framed.fields {
        let Some(record_schema) = record_schema else {
            if let Some(common) = schema.common_fields.iter().find(|field| {
                field
                    .signature
                    .as_deref()
                    .is_some_and(|tag| tag.as_bytes() == signature)
            }) {
                let common = schema.resolve(common)?;
                match value(&common, bytes, schema, context, diagnostics, &editor_id, 0) {
                    Ok((value, canonical_bytes)) => fields.push(DecodedField {
                        signature,
                        name: common.name,
                        value,
                        canonical_bytes,
                    }),
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
            });
            continue;
        };
        let Some(index) = match_field(record_schema, &signature, cursor, last) else {
            if record_schema.entries.iter().any(|entry| {
                entry
                    .field
                    .signature
                    .as_deref()
                    .is_some_and(|tag| tag.as_bytes() == signature)
            }) && !rejected_fields.contains(&signature)
            {
                rejected_fields.push(signature);
            }
            diagnostics.note("unexpected", || {
                format!(
                    "{} {id:08X}: unexpected/out-of-order {}",
                    String::from_utf8_lossy(&scanned.record_type),
                    String::from_utf8_lossy(&signature)
                )
            });
            continue;
        };
        let field_schema = &record_schema.entries[index].field;
        cursor = index + usize::from(!field_schema.repeat);
        last = Some(index);
        match value(
            field_schema,
            bytes,
            schema,
            context,
            diagnostics,
            &editor_id,
            0,
        ) {
            Ok((value, canonical_bytes)) => {
                if signature == *b"EDID"
                    && let Value::String(name) = &value
                {
                    editor_id.clone_from(name);
                }
                fields.push(DecodedField {
                    signature,
                    name: field_schema.name.clone(),
                    value,
                    canonical_bytes,
                });
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
    }
    Ok(DecodedRecord {
        form_id: id,
        source_form_id: scanned.source_form_id,
        record_type: scanned.record_type,
        flags: scanned.flags,
        load_order: context.plugin_index as u32,
        cell_form_id: cell,
        worldspace_form_id: world,
        fields,
        rejected_fields,
        payload_complete: framed.complete,
        raw_payload: scanned.payload,
        supported: record_schema.is_some(),
    })
}

/// Obtain a primitive's binary width; variable-sized kinds supply explicit lengths.
fn width(field: &FieldSchema) -> Option<usize> {
    match field.kind.as_str() {
        "u8" | "i8" => Some(1),
        "u16" | "i16" => Some(2),
        "u32" | "i32" | "f32" | "form_id" => Some(4),
        "u64" | "i64" => Some(8),
        _ => field.size,
    }
}

/// Choose a schema alternative using a small named Rust policy, never external tool code.
fn decide<'a>(
    field: &'a FieldSchema,
    bytes: &[u8],
    editor_id: &str,
) -> Result<&'a FieldSchema, String> {
    let decider = field.decider.as_deref().ok_or("union has no decider")?;
    let target = match decider {
        "gmst_value" => match editor_id.as_bytes().first() {
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
        _ => return Err(format!("unsupported union decider {decider}")),
    };
    field
        .fields
        .iter()
        .find(|alternative| {
            target.map_or_else(
                || {
                    alternative.size == Some(bytes.len())
                        || alternative.sizes.contains(&bytes.len())
                },
                |kind| alternative.kind == kind,
            )
        })
        .ok_or_else(|| format!("no {decider} alternative accepts {} bytes", bytes.len()))
}

/// Interpret one value within checked bytes, rewriting only typed link positions.
fn value(
    field: &FieldSchema,
    bytes: &[u8],
    schema: &Schema,
    context: &Context<'_>,
    diagnostics: &mut PluginDiagnostics,
    editor_id: &str,
    depth: usize,
) -> Result<(Value, Vec<u8>), String> {
    if depth >= 32 {
        return Err("value nesting exceeds 32".into());
    }
    if field.definition.is_some() {
        return value(
            &schema.resolve(field)?,
            bytes,
            schema,
            context,
            diagnostics,
            editor_id,
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
    if field.kind == "union" {
        return value(
            decide(field, bytes, editor_id)?,
            bytes,
            schema,
            context,
            diagnostics,
            editor_id,
            depth + 1,
        );
    }
    if field.decider.as_deref() == Some("alternate_textures") {
        return alternate_textures(bytes, context, diagnostics);
    }
    if field.decider.as_deref() == Some("vmad") {
        let mut canonical = bytes.to_vec();
        crate::esm::records::record_type::vmad::remap_primary_form_ids(&mut canonical, |id| {
            remap(id, context, true, diagnostics).map_err(color_eyre::eyre::Report::msg)
        })
        .map_err(|error| format!("invalid VMAD primary scripts: {error}"))?;
        return Ok((Value::Bytes(canonical.clone()), canonical));
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
            let mut values = Vec::new();
            let mut canonical = bytes.to_vec();
            for member in &field.members {
                let member = schema.resolve(member)?;
                let size = width(&member)
                    .or_else(|| bytes.len().checked_sub(member.offset))
                    .ok_or("member offset out of bounds")?;
                let end = member
                    .offset
                    .checked_add(size)
                    .ok_or("member extent overflow")?;
                let slice = bytes
                    .get(member.offset..end)
                    .ok_or_else(|| format!("{} member out of bounds", member.name))?;
                let (decoded, encoded) = value(
                    &member,
                    slice,
                    schema,
                    context,
                    diagnostics,
                    editor_id,
                    depth + 1,
                )?;
                canonical[member.offset..end].copy_from_slice(&encoded);
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
                editor_id,
                depth + 1,
            );
        }
        kind => return Err(format!("field kind {kind} is not a value")),
    };
    Ok((decoded, bytes.to_vec()))
}

/// Decode Windows-1252 plugin text while preserving already valid UTF-8.
pub(crate) fn decode_string(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
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
    editor_id: &str,
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
    let stride = count_rule
        .and_then(|rule| rule.get("stride"))
        .and_then(|value| value.as_u64())
        .map(|value| value as usize)
        .or(field.size)
        .or_else(|| field.members.first().and_then(width))
        .ok_or("array has no stride")?;
    if stride == 0 {
        return Err("zero array stride".into());
    }
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
    if count > 1_000_000 {
        return Err("array exceeds one million elements".into());
    }
    if count
        .checked_mul(stride)
        .and_then(|size| size.checked_add(offset))
        != Some(bytes.len())
    {
        return Err("array extent/count mismatch".into());
    }
    let element = if field.members.len() == 1 && field.members[0].offset == 0 {
        schema.resolve(&field.members[0])?
    } else {
        FieldSchema {
            kind: "struct".into(),
            size: Some(stride),
            members: field.members.clone(),
            ..Default::default()
        }
    };
    let mut canonical = bytes.to_vec();
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let start = offset + index * stride;
        let (decoded, encoded) = value(
            &element,
            &bytes[start..start + stride],
            schema,
            context,
            diagnostics,
            editor_id,
            depth,
        )?;
        canonical[start..start + stride].copy_from_slice(&encoded);
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
        let name = decode_string(
            bytes
                .get(position..end)
                .ok_or("truncated alternate texture name")?,
        );
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

/// Validate declared target kinds after all winners exist; dangling optional links are nulled.
pub(crate) fn validate_targets(result: &mut ReadResult, schema: &Schema, order: &LoadOrder) {
    let kinds: HashMap<u32, [u8; 4]> = result
        .records
        .iter()
        .map(|(&id, record)| (id, record.record_type))
        .collect();
    for record in result.records.values_mut() {
        let record_schema = prepared().get(&record.record_type);
        let plugin = order.names.get(record.load_order as usize).cloned();
        let mut diagnostics = PluginDiagnostics::default();
        for field in &mut record.fields {
            let field_schema = record_schema
                .and_then(|prepared| {
                    prepared
                        .entries
                        .iter()
                        .find(|entry| {
                            entry
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
            let resolved = schema
                .resolve(field_schema)
                .expect("validated common field");
            if resolved.decider.as_deref() == Some("vmad") {
                // Canonical VMAD links are already global; this pass checks winners.
                let validation = crate::esm::records::record_type::vmad::remap_primary_form_ids(
                    &mut field.canonical_bytes,
                    |id| {
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
                if validation.is_ok() {
                    field.value = Value::Bytes(field.canonical_bytes.clone());
                }
                continue;
            }
            validate_value(
                &mut field.value,
                &mut field.canonical_bytes,
                field_schema,
                schema,
                &kinds,
                &mut diagnostics,
            );
        }
        if diagnostics.invalid_links != 0
            && let Some(plugin) = plugin
        {
            let target = result.diagnostics.entry(plugin).or_default();
            target.invalid_links += diagnostics.invalid_links;
            if target.first_example.is_none() {
                target.first_example = diagnostics.first_example;
            }
            for (category, example) in diagnostics.first_by_category {
                target.first_by_category.entry(category).or_insert(example);
            }
        }
    }
}

/// Walk the same schema offsets used by decoding when nulling a mistyped or dangling link.
fn validate_value(
    decoded: &mut Value,
    bytes: &mut [u8],
    field: &FieldSchema,
    schema: &Schema,
    kinds: &HashMap<u32, [u8; 4]>,
    diagnostics: &mut PluginDiagnostics,
) {
    let Ok(field) = schema.resolve(field) else {
        return;
    };
    if field.kind == "union" {
        if let Some(alternative) = field.fields.iter().find(|alternative| {
            alternative.size == Some(bytes.len()) || alternative.sizes.contains(&bytes.len())
        }) {
            validate_value(decoded, bytes, alternative, schema, kinds, diagnostics);
        }
        return;
    }
    match decoded {
        Value::FormId(id) if *id != 0 => {
            // Hardcoded IDs (including the player) need not have a plugin record.
            let allowed = kinds.get(id).map_or(*id < 0x800, |kind| {
                field
                    .targets
                    .iter()
                    .any(|target| target == "ANY_" || target.as_bytes() == kind)
            });
            if !allowed {
                diagnostics.note("link", || {
                    format!(
                        "{} link {:08X} is absent or has a disallowed target type",
                        field.name, *id
                    )
                });
                *id = 0;
                if bytes.len() == 4 {
                    bytes.copy_from_slice(&0u32.to_le_bytes());
                }
            }
        }
        Value::Struct(values) => {
            for (name, value) in values {
                if let Some(member) = field.members.iter().find(|member| member.name == *name) {
                    let size = width(member).unwrap_or(bytes.len().saturating_sub(member.offset));
                    if let Some(slice) =
                        bytes.get_mut(member.offset..member.offset.saturating_add(size))
                    {
                        validate_value(value, slice, member, schema, kinds, diagnostics);
                    }
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
                    && let Some(link_bytes) = bytes.get_mut(position..position.saturating_add(4))
                {
                    validate_value(link, link_bytes, &target_schema, schema, kinds, diagnostics);
                }
                position = position.saturating_add(8);
            }
        }
        Value::Array(values) => {
            let stride = field
                .count
                .as_ref()
                .and_then(|rule| rule.get("stride"))
                .and_then(|value| value.as_u64())
                .map(|value| value as usize)
                .or(field.size)
                .or_else(|| field.members.first().and_then(width));
            if let Some(stride) = stride.filter(|&stride| stride != 0) {
                let offset = usize::from(
                    field
                        .count
                        .as_ref()
                        .and_then(|rule| rule.get("kind"))
                        .and_then(|value| value.as_str())
                        == Some("prefix_u32"),
                ) * 4;
                let element = if field.members.len() == 1 {
                    field.members[0].clone()
                } else {
                    FieldSchema {
                        kind: "struct".into(),
                        members: field.members.clone(),
                        ..Default::default()
                    }
                };
                for (index, value) in values.iter_mut().enumerate() {
                    let start = offset + index * stride;
                    if let Some(slice) = bytes.get_mut(start..start + stride) {
                        validate_value(value, slice, &element, schema, kinds, diagnostics);
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
