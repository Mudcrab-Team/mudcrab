//! Immutable fixed layouts replace per-element Value allocations, never source bytes.
//! Serialization visits borrowed slices and retains the existing Array/Struct/Bytes tags.

use super::{
    Value,
    schema_format::{FieldSchema, Schema},
};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::sync::Arc;

/// A validated, link-free array. Clones share immutable bytes and layout metadata.
/// Construction is restricted to the decoder so target validation can treat it as a leaf.
#[derive(Clone, Debug)]
pub struct CompactArray(Arc<Storage>);

#[derive(Debug)]
struct Storage {
    bytes: Box<[u8]>,
    layout: Layout,
    stride: usize,
    count: usize,
}

#[derive(Debug)]
enum Layout {
    Unsigned(usize),
    Signed(usize),
    Float,
    Bytes,
    Struct(Vec<Member>),
}

#[derive(Debug)]
struct Member {
    name: String,
    offset: usize,
    size: usize,
    layout: Layout,
}

impl CompactArray {
    /// The number of array elements, independent of padding and count prefixes.
    pub fn len(&self) -> usize {
        self.0.count
    }

    /// Whether there are no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Materialize one element for explicit inspection; serialization does not use this.
    pub fn get(&self, index: usize) -> Option<Value> {
        let start = index.checked_mul(self.0.stride)?;
        (index < self.len())
            .then(|| materialize(&self.0.layout, &self.0.bytes[start..start + self.0.stride]))
    }

    /// Explicit allocation for consumers requiring an owned legacy array representation.
    pub fn materialize(&self) -> Vec<Value> {
        (0..self.len())
            .map(|index| self.get(index).expect("validated index"))
            .collect()
    }

    /// Borrow the streaming body with the caller's existing float representation policy.
    pub(crate) fn elements(&self, widen_floats: bool) -> Elements<'_> {
        Elements {
            array: self,
            widen_floats,
        }
    }
}

impl PartialEq for CompactArray {
    /// Padding and storage layout are not typed values; preserve legacy element equality.
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && (0..self.len()).all(|index| self.get(index) == other.get(index))
    }
}

/// Unsupported layouts take the original decoder path. Eligible malformed values retain
/// its field-local error, including non-finite f32 rejection. Caller checked array extents.
pub(crate) fn try_decode(
    field: &FieldSchema,
    bytes: &[u8],
    count: usize,
    stride: usize,
    schema: &Schema,
    depth: usize,
) -> Result<Option<CompactArray>, String> {
    if count == 0 || stride == 0 || count.checked_mul(stride) != Some(bytes.len()) {
        return Ok(None);
    }
    let Some(layout) = layout(field, stride, schema, depth)? else {
        return Ok(None);
    };
    for element in bytes.chunks_exact(stride) {
        validate(&layout, element)?;
    }
    Ok(Some(CompactArray(Arc::new(Storage {
        bytes: bytes.into(),
        layout,
        stride,
        count,
    }))))
}

/// Build a fixed layout only when each node has the same bounded extent as legacy decoding.
fn layout(
    field: &FieldSchema,
    size: usize,
    schema: &Schema,
    depth: usize,
) -> Result<Option<Layout>, String> {
    if depth >= 32 || field.optional || field.decider.is_some() || !field.targets.is_empty() {
        return Ok(None);
    }
    if field.definition.is_some() {
        return layout(&schema.resolve(field)?, size, schema, depth + 1);
    }
    if field.size.is_some_and(|expected| expected != size)
        || !field.sizes.is_empty() && !field.sizes.contains(&size)
    {
        return Ok(None);
    }
    let result = match field.kind.as_str() {
        "u8" if size == 1 => Layout::Unsigned(1),
        "u16" if size == 2 => Layout::Unsigned(2),
        "u32" if size == 4 => Layout::Unsigned(4),
        "u64" if size == 8 => Layout::Unsigned(8),
        "i8" if size == 1 => Layout::Signed(1),
        "i16" if size == 2 => Layout::Signed(2),
        "i32" if size == 4 => Layout::Signed(4),
        "i64" if size == 8 => Layout::Signed(8),
        "f32" if size == 4 => Layout::Float,
        "bytes" => Layout::Bytes,
        "struct" => {
            let mut members = Vec::with_capacity(field.members.len());
            for member in &field.members {
                // Match the original struct's resolve-before-width and member order.
                let member = schema.resolve(member)?;
                let member_size = match member.kind.as_str() {
                    "u8" | "i8" => Some(1),
                    "u16" | "i16" => Some(2),
                    "u32" | "i32" | "f32" | "form_id" => Some(4),
                    "u64" | "i64" => Some(8),
                    _ => member.size,
                }
                .or_else(|| size.checked_sub(member.offset));
                let Some(member_size) = member_size else {
                    return Ok(None);
                };
                if member
                    .offset
                    .checked_add(member_size)
                    .is_none_or(|end| end > size)
                {
                    return Ok(None);
                }
                let Some(child) = layout(&member, member_size, schema, depth + 1)? else {
                    return Ok(None);
                };
                members.push(Member {
                    name: member.name,
                    offset: member.offset,
                    size: member_size,
                    layout: child,
                });
            }
            Layout::Struct(members)
        }
        // Links, strings, arrays, deferred unions and selectors are never packed.
        _ => return Ok(None),
    };
    Ok(Some(result))
}

/// Check data-dependent float validity before retaining any compact array.
fn validate(layout: &Layout, bytes: &[u8]) -> Result<(), String> {
    match layout {
        Layout::Float if !float(bytes).is_finite() => Err("non-finite float".into()),
        Layout::Struct(members) => {
            for member in members {
                validate(
                    &member.layout,
                    &bytes[member.offset..member.offset + member.size],
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Zero-extend a checked little-endian unsigned scalar without changing its width tag.
fn unsigned(bytes: &[u8]) -> u64 {
    let mut padded = [0; 8];
    padded[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(padded)
}

/// Sign-extend a checked little-endian signed scalar, including i8 and i64 extrema.
fn signed(bytes: &[u8]) -> i64 {
    let mut padded = [if bytes[bytes.len() - 1] & 0x80 == 0 {
        0
    } else {
        255
    }; 8];
    padded[..bytes.len()].copy_from_slice(bytes);
    i64::from_le_bytes(padded)
}

/// Reconstruct exact f32 bits; validation and float widening are separate operations.
fn float(bytes: &[u8]) -> f32 {
    f32::from_le_bytes(bytes.try_into().expect("validated f32 width"))
}

/// Build a legacy element only for explicit inspection or semantic equality.
fn materialize(layout: &Layout, bytes: &[u8]) -> Value {
    match layout {
        Layout::Unsigned(_) => Value::Unsigned(unsigned(bytes)),
        Layout::Signed(_) => Value::Signed(signed(bytes)),
        Layout::Float => Value::Float(float(bytes)),
        Layout::Bytes => Value::Bytes(bytes.to_vec()),
        Layout::Struct(members) => Value::Struct(
            members
                .iter()
                .map(|member| {
                    (
                        member.name.clone(),
                        materialize(
                            &member.layout,
                            &bytes[member.offset..member.offset + member.size],
                        ),
                    )
                })
                .collect(),
        ),
    }
}

/// Two float policies intentionally mirror ordinary Value and typed JSON separately.
pub(crate) struct Elements<'a> {
    array: &'a CompactArray,
    widen_floats: bool,
}

impl Serialize for Elements<'_> {
    /// Emit one borrowed element at a time without allocating a Value vector.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let storage = &self.array.0;
        let mut output = serializer.serialize_seq(Some(storage.count))?;
        for bytes in storage.bytes.chunks_exact(storage.stride) {
            output.serialize_element(&Element {
                layout: &storage.layout,
                bytes,
                widen_floats: self.widen_floats,
            })?;
        }
        output.end()
    }
}

struct Element<'a> {
    layout: &'a Layout,
    bytes: &'a [u8],
    widen_floats: bool,
}

impl Serialize for Element<'_> {
    /// Retain every legacy tag and its serializer index; float policy belongs to the caller.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.layout {
            Layout::Unsigned(size) => serializer.serialize_newtype_variant(
                "Value",
                0,
                "Unsigned",
                &unsigned(&self.bytes[..*size]),
            ),
            Layout::Signed(size) => serializer.serialize_newtype_variant(
                "Value",
                1,
                "Signed",
                &signed(&self.bytes[..*size]),
            ),
            Layout::Float if self.widen_floats => serializer.serialize_newtype_variant(
                "Value",
                2,
                "Float",
                &f64::from(float(self.bytes)),
            ),
            Layout::Float => {
                serializer.serialize_newtype_variant("Value", 2, "Float", &float(self.bytes))
            }
            Layout::Bytes => serializer.serialize_newtype_variant("Value", 6, "Bytes", self.bytes),
            Layout::Struct(members) => serializer.serialize_newtype_variant(
                "Value",
                8,
                "Struct",
                &Members {
                    members,
                    bytes: self.bytes,
                    widen_floats: self.widen_floats,
                },
            ),
        }
    }
}

struct Members<'a> {
    members: &'a [Member],
    bytes: &'a [u8],
    widen_floats: bool,
}

impl Serialize for Members<'_> {
    /// Preserve tuple shape, schema order and duplicate labels, including overlapping fields.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(self.members.len()))?;
        for member in self.members {
            output.serialize_element(&(
                &member.name,
                Element {
                    layout: &member.layout,
                    bytes: &self.bytes[member.offset..member.offset + member.size],
                    widen_floats: self.widen_floats,
                },
            ))?;
        }
        output.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No family-specific schema is needed for fixed element controls.
    fn schema() -> Schema {
        Schema::parse(br#"{"version":1,"records":[]}"#).unwrap()
    }

    /// Build asymmetric named members without borrowing a production decoder oracle.
    fn field(kind: &str, offset: usize, name: &str) -> FieldSchema {
        FieldSchema {
            kind: kind.into(),
            offset,
            name: name.into(),
            ..Default::default()
        }
    }

    /// Cover asymmetric integer extrema and widths in compact storage and legacy values.
    #[test]
    fn integer_widths_preserve_unsigned_and_sign_extension_without_aliasing() {
        for (kind, bytes, expected) in [
            ("u8", vec![255], Value::Unsigned(255)),
            ("i8", vec![128], Value::Signed(-128)),
            ("u16", vec![0x34, 0x82], Value::Unsigned(0x8234)),
            ("i16", vec![0, 128], Value::Signed(-32768)),
            (
                "u32",
                u32::MAX.to_le_bytes().to_vec(),
                Value::Unsigned(u64::from(u32::MAX)),
            ),
            (
                "i32",
                i32::MIN.to_le_bytes().to_vec(),
                Value::Signed(i64::from(i32::MIN)),
            ),
            (
                "u64",
                u64::MAX.to_le_bytes().to_vec(),
                Value::Unsigned(u64::MAX),
            ),
            (
                "i64",
                i64::MIN.to_le_bytes().to_vec(),
                Value::Signed(i64::MIN),
            ),
        ] {
            let array = try_decode(&field(kind, 0, ""), &bytes, 1, bytes.len(), &schema(), 0)
                .unwrap()
                .unwrap();
            let compact = Value::CompactArray(array.clone());
            let ordinary = Value::Array(vec![expected]);
            assert_eq!(compact, ordinary);
            assert_eq!(
                serde_json::to_vec(&compact).unwrap(),
                serde_json::to_vec(&ordinary).unwrap()
            );
            assert!(array.get(1).is_none());
            assert!(array.get(usize::MAX).is_none());
            assert!(Arc::ptr_eq(&array.0, &array.clone().0));
        }
    }

    /// Preserve ordered nested members, duplicate labels and untouched source padding.
    #[test]
    fn nested_members_keep_schema_order_duplicate_labels_bytes_and_padding() {
        let nested = FieldSchema {
            kind: "struct".into(),
            offset: 4,
            size: Some(4),
            name: "nested".into(),
            members: vec![field("i16", 2, "duplicate"), field("u8", 0, "duplicate")],
            ..Default::default()
        };
        let layout = FieldSchema {
            kind: "struct".into(),
            size: Some(10),
            members: vec![
                nested,
                field("u16", 1, "overlap"),
                FieldSchema {
                    kind: "bytes".into(),
                    offset: 1,
                    size: Some(2),
                    name: "raw".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let bytes = [99, 0, 255, 88, 7, 77, 0, 128, 66, 55];
        let packed = try_decode(&layout, &bytes, 1, 10, &schema(), 0)
            .unwrap()
            .unwrap();
        let expected = Value::Array(vec![Value::Struct(vec![
            (
                "nested".into(),
                Value::Struct(vec![
                    ("duplicate".into(), Value::Signed(-32768)),
                    ("duplicate".into(), Value::Unsigned(7)),
                ]),
            ),
            ("overlap".into(), Value::Unsigned(0xff00)),
            ("raw".into(), Value::Bytes(vec![0, 255])),
        ])]);
        let compact = Value::CompactArray(packed.clone());
        assert_eq!(compact, expected);
        assert_eq!(
            serde_json::to_vec(&compact).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        // Unmapped padding is retained physically but was never a typed member.
        let mut changed = bytes;
        changed[9] ^= 255;
        let other = try_decode(&layout, &changed, 1, 10, &schema(), 0)
            .unwrap()
            .unwrap();
        assert_ne!(packed.0.bytes, other.0.bytes);
        assert_eq!(packed, other);
    }

    /// Keep unsupported layouts ordinary and reject malformed floats within the field.
    #[test]
    fn unsupported_layouts_and_depth_keep_legacy_path_and_bad_floats_reject() {
        for kind in ["form_id", "lstring", "zstring", "union", "array"] {
            assert!(
                try_decode(&field(kind, 0, ""), &[0; 4], 1, 4, &schema(), 0)
                    .unwrap()
                    .is_none()
            );
        }
        let mut optional = field("u8", 0, "optional");
        optional.optional = true;
        assert!(
            try_decode(&optional, &[0], 1, 1, &schema(), 0)
                .unwrap()
                .is_none()
        );
        let mut selected = field("u8", 0, "selected");
        selected.decider = Some("size".into());
        assert!(
            try_decode(&selected, &[0], 1, 1, &schema(), 0)
                .unwrap()
                .is_none()
        );
        let nested_link = FieldSchema {
            kind: "struct".into(),
            members: vec![field("form_id", 0, "link")],
            ..Default::default()
        };
        assert!(
            try_decode(&nested_link, &[0; 4], 1, 4, &schema(), 0)
                .unwrap()
                .is_none()
        );
        assert!(
            try_decode(&field("u8", 0, ""), &[0], 1, 1, &schema(), 32)
                .unwrap()
                .is_none()
        );
        assert!(
            try_decode(&field("u8", 0, ""), &[], 0, 1, &schema(), 0)
                .unwrap()
                .is_none()
        );
        for bits in [0x7f80_0000_u32, 0xff80_0000, 0x7fc0_0001] {
            assert_eq!(
                try_decode(
                    &field("f32", 0, ""),
                    &bits.to_le_bytes(),
                    1,
                    4,
                    &schema(),
                    0
                )
                .unwrap_err(),
                "non-finite float"
            );
        }
    }

    /// Compare ordinary f32 tokens and the typed writer's widened f64 tokens independently.
    #[test]
    fn float_bits_and_both_serialization_policies_match_ordinary_values() {
        for bits in [0_u32, 0x8000_0000, 1, 0x8000_0001, 0x3dcccccd, 0x7f7f_ffff] {
            let value = f32::from_bits(bits);
            let packed = try_decode(
                &field("f32", 0, ""),
                &bits.to_le_bytes(),
                1,
                4,
                &schema(),
                0,
            )
            .unwrap()
            .unwrap();
            let Some(Value::Float(actual)) = packed.get(0) else {
                panic!("float element");
            };
            assert_eq!(actual.to_bits(), bits);
            assert_eq!(
                serde_json::to_vec(&Value::CompactArray(packed.clone())).unwrap(),
                serde_json::to_vec(&Value::Array(vec![Value::Float(value)])).unwrap()
            );
            let token = serde_json::to_string(&packed.elements(true)).unwrap();
            assert_eq!(
                token,
                format!(
                    "[{{\"Float\":{}}}]",
                    serde_json::to_string(&f64::from(value)).unwrap()
                )
            );
            let number = token
                .strip_prefix("[{\"Float\":")
                .unwrap()
                .strip_suffix("}]")
                .unwrap();
            assert_eq!(
                number.parse::<f64>().unwrap().to_bits(),
                f64::from(value).to_bits()
            );
        }
    }

    /// Exercise the public records-only publisher with a malformed packed float between
    /// usable records. Direct decoding drops VTXT; publication rejects the unusable LAND
    /// while authenticated physical history retains its exact source payload.
    #[test]
    fn broken_compact_float_publishes_neighbors_and_retains_original_payload() {
        use super::super::read_plugins;
        use crate::esm::{inhouse::export_record_bundle_typed, load_order::LoadOrder};
        use std::fs;

        /// Frame a short fixture field with a checked native size.
        fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
            let mut output = tag.to_vec();
            output.extend(u16::try_from(bytes.len()).unwrap().to_le_bytes());
            output.extend(bytes);
            output
        }
        /// Keep each original record boundary distinct from typed recovery.
        fn frame(tag: &[u8; 4], id: u32, payload: &[u8]) -> Vec<u8> {
            let mut output = tag.to_vec();
            output.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
            output.extend(0_u32.to_le_bytes());
            output.extend(id.to_le_bytes());
            output.extend([0; 4]);
            output.extend(44_u16.to_le_bytes());
            output.extend([0; 2]);
            output.extend(payload);
            output
        }
        /// Supply a real LAND parent without inventing a runtime ownership relation.
        fn group(kind: u32, label: u32, payload: &[u8]) -> Vec<u8> {
            let mut output = b"GRUP".to_vec();
            output.extend(u32::try_from(24 + payload.len()).unwrap().to_le_bytes());
            output.extend(label.to_le_bytes());
            output.extend(kind.to_le_bytes());
            output.extend([0; 8]);
            output.extend(payload);
            output
        }
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("Data");
        fs::create_dir(&data).unwrap();
        let path = data.join("ArrayNeighbors.esp");
        let mut plugin = frame(b"TES4", 0, &sub(b"HEDR", &[0; 12]));
        plugin.extend(frame(b"STAT", 0x810, &sub(b"EDID", b"Before\0")));
        plugin.extend(frame(b"LTEX", 0x814, &sub(b"EDID", b"Texture\0")));
        let mut height = 0_f32.to_le_bytes().to_vec();
        height.extend([0; 1092]);
        let mut layer = 0x814_u32.to_le_bytes().to_vec();
        layer.extend([0; 4]);
        let mut overlay = layer.clone();
        overlay[6..8].copy_from_slice(&1_i16.to_le_bytes());
        let mut weights = vec![0; 4];
        weights.extend(0.25_f32.to_le_bytes());
        weights.extend([1, 0, 9, 8]);
        weights.extend(f32::NAN.to_le_bytes());
        let payload = [
            sub(b"DATA", &[0; 4]),
            sub(b"VHGT", &height),
            sub(b"BTXT", &layer),
            sub(b"ATXT", &overlay),
            sub(b"VTXT", &weights),
        ]
        .concat();
        let cell_payload = [sub(b"DATA", &[0]), sub(b"XCLC", &[0; 12])].concat();
        plugin.extend(frame(b"CELL", 0x820, &cell_payload));
        plugin.extend(group(8, 0x820, &frame(b"LAND", 0x821, &payload)));
        plugin.extend(frame(b"STAT", 0x811, &sub(b"EDID", b"After\0")));
        fs::write(&path, plugin).unwrap();
        let paths = [path];
        let read = read_plugins(&paths, &LoadOrder::read(&paths).unwrap()).unwrap();
        let land = read
            .records
            .values()
            .find(|record| record.record_type == *b"LAND")
            .unwrap();
        assert_eq!(land.raw_payload, payload);
        assert!(land.rejected_fields.contains(b"VTXT"));
        assert!(!land.fields.iter().any(|field| field.signature == *b"VTXT"));
        let height_field = land
            .fields
            .iter()
            .find(|field| field.signature == *b"VHGT")
            .unwrap();
        let Value::Struct(members) = &height_field.value else {
            panic!("height struct");
        };
        assert!(
            members
                .iter()
                .any(|(name, value)| name == "deltas" && matches!(value, Value::CompactArray(_)))
        );
        let ids: Vec<_> = read
            .records
            .values()
            .filter(|record| record.record_type == *b"STAT")
            .map(|record| record.form_id)
            .collect();
        assert_eq!(ids.len(), 2);
        let order = temp.path().join("plugins.txt");
        fs::write(&order, "*ArrayNeighbors.esp\n").unwrap();
        let output = temp.path().join("pack");
        export_record_bundle_typed(&data, &order, &output, &[*b"LAND", *b"STAT"]).unwrap();
        let db = rusqlite::Connection::open(output.join("skyrim_world.db")).unwrap();
        for id in ids {
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM records WHERE form_id=?1",
                    [id],
                    |row| row.get::<_, u32>(0)
                )
                .unwrap(),
                1
            );
        }
        // RV-A rejects a LAND with authored VTXT failure; R5 retains skipped physical provenance.
        let source_count: u32 = db
            .query_row(
                "SELECT COUNT(*) FROM inhouse_source_records WHERE form_id=?1",
                [land.form_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_count, 0);
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM records WHERE form_id=?1 AND record_type='LAND'",
                [land.form_id],
                |row| row.get::<_, u32>(0),
            )
            .unwrap(),
            0,
        );
        let (status, reason): (String, String) = db
            .query_row(
                "SELECT status,reason FROM record_overrides WHERE form_id=?1",
                [land.form_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "skipped");
        assert!(reason.contains("rejected authored terrain field VTXT"));
        assert_eq!(db.query_row(
            "SELECT COUNT(*) FROM record_diagnostics WHERE source_form_id=?1 AND category='record' AND reason LIKE '%rejected authored terrain field VTXT%'",
            [land.source_form_id], |row| row.get::<_, u32>(0),
        ).unwrap(), 1);
        let mut raw_versions = Vec::new();
        let summary = super::super::history_source::visit_source_history(
            &output,
            None,
            super::super::history_source::SourceQuery::Source {
                original_owner: "arrayneighbors.esp".into(),
                full_local: 0x821,
            },
            |version| {
                raw_versions.push(version);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(summary.versions, 1);
        assert!(summary.fully_raw_available);
        assert!(!summary.typed_runtime_decoding_attempted);
        assert_eq!(raw_versions[0].status, "skipped");
        assert!(!raw_versions[0].runtime_record_present);
        assert!(!raw_versions[0].accepted_source_winner);
        let raw = raw_versions[0].raw.as_ref().unwrap();
        assert!(raw.frame_complete);
        assert_eq!(raw.stored_payload.as_deref(), Some(payload.as_slice()));
        for name in ["cell_cache.rkyv", "cell_cache_preserved.rkyv"] {
            let path = output.join(name);
            assert!(path.is_file());
            let cache = rkyv::from_bytes::<shared::CellCache, rkyv::rancor::Error>(
                &fs::read(path).unwrap(),
            )
            .unwrap();
            assert!(cache.cells.is_empty(), "unusable LAND entered {name}");
        }
        let typed: Vec<serde_json::Value> = fs::read_to_string(output.join("typed-records.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(typed.len(), 2);
        let mut typed_ids = typed
            .iter()
            .map(|record| record["form_id"].as_u64().unwrap())
            .collect::<Vec<_>>();
        typed_ids.sort_unstable();
        assert_eq!(typed_ids, [0x810, 0x811]);
        assert!(
            typed
                .iter()
                .all(|record| record["record_type"] == serde_json::json!(*b"STAT"))
        );
        assert!(
            !serde_json::to_string(land)
                .unwrap()
                .contains("CompactArray")
        );
    }
}
