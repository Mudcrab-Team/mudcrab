//! Stream the complete typed diagnostic record without building a per-record JSON tree.
//! Field and component order are retained. Floats use the prior JSON DOM's f64 widening.

use super::{DecodedField, DecodedRecord, Value};
use serde::{Serialize, Serializer, ser::SerializeSeq, ser::SerializeStruct};
use std::{collections::BTreeMap, io::Write};

/// Write the existing twenty-field record envelope; the caller owns JSONL framing and flushes.
pub(crate) fn write_record<W: Write>(
    writer: &mut W,
    record: &DecodedRecord,
    localization: &BTreeMap<usize, serde_json::Value>,
) -> serde_json::Result<()> {
    serde_json::to_writer(
        writer,
        &RecordView {
            record,
            localization,
        },
    )
}

struct RecordView<'a> {
    record: &'a DecodedRecord,
    localization: &'a BTreeMap<usize, serde_json::Value>,
}

impl Serialize for RecordView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let record = self.record;
        let mut output = serializer.serialize_struct("TypedRecord", 20)?;
        output.serialize_field("form_id", &record.form_id)?;
        output.serialize_field("source_form_id", &record.source_form_id)?;
        output.serialize_field("record_type", &record.record_type)?;
        output.serialize_field("flags", &record.flags)?;
        output.serialize_field("load_order", &record.load_order)?;
        output.serialize_field("version_control", &record.version_control)?;
        output.serialize_field("form_version", &record.form_version)?;
        output.serialize_field("header_unknown", &record.header_unknown)?;
        output.serialize_field("cell_form_id", &record.cell_form_id)?;
        output.serialize_field("worldspace_form_id", &record.worldspace_form_id)?;
        output.serialize_field("topic_form_id", &record.topic_form_id)?;
        output.serialize_field("source_topic_form_id", &record.source_topic_form_id)?;
        output.serialize_field("topic_group_offset", &record.topic_group_offset)?;
        output.serialize_field("source_record_offset", &record.source_record_offset)?;
        output.serialize_field("fields", &FieldsView(&record.fields))?;
        output.serialize_field("rejected_fields", &record.rejected_fields)?;
        output.serialize_field("payload_complete", &record.payload_complete)?;
        output.serialize_field("supported", &record.supported)?;
        output.serialize_field("localization", self.localization)?;
        output.serialize_field(
            "source_payload_sha256",
            &crate::cache::hash_bytes(&record.raw_payload),
        )?;
        output.end()
    }
}

struct FieldsView<'a>(&'a [DecodedField]);

impl Serialize for FieldsView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(self.0.len()))?;
        for field in self.0 {
            output.serialize_element(&FieldView(field))?;
        }
        output.end()
    }
}

struct FieldView<'a>(&'a DecodedField);

impl Serialize for FieldView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let field = self.0;
        let mut output =
            serializer.serialize_struct("DecodedField", 4 + usize::from(field.opaque))?;
        output.serialize_field("signature", &field.signature)?;
        output.serialize_field("name", &field.name)?;
        output.serialize_field("value", &ValueView(&field.value))?;
        output.serialize_field("canonical_bytes", &field.canonical_bytes)?;
        if field.opaque {
            output.serialize_field("opaque", &true)?;
        }
        output.end()
    }
}

struct ValueView<'a>(&'a Value);

impl Serialize for ValueView<'_> {
    /// Stream legacy tags and widened float values from ordinary or compact storage.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Unsigned(value) => {
                serializer.serialize_newtype_variant("Value", 0, "Unsigned", value)
            }
            Value::Signed(value) => {
                serializer.serialize_newtype_variant("Value", 1, "Signed", value)
            }
            Value::Float(value) => {
                // json! first converted f32 to an owned Number. Keep its widened numeric
                // value, signed zero and non-finite -> null policy when writing directly.
                serializer.serialize_newtype_variant("Value", 2, "Float", &f64::from(*value))
            }
            Value::String(value) => {
                serializer.serialize_newtype_variant("Value", 3, "String", value)
            }
            Value::LocalizedString(value) => {
                serializer.serialize_newtype_variant("Value", 4, "LocalizedString", value)
            }
            Value::FormId(value) => {
                serializer.serialize_newtype_variant("Value", 5, "FormId", value)
            }
            Value::Bytes(value) => serializer.serialize_newtype_variant("Value", 6, "Bytes", value),
            Value::Deferred(value) => {
                serializer.serialize_newtype_variant("Value", 7, "Deferred", value)
            }
            Value::Struct(value) => {
                serializer.serialize_newtype_variant("Value", 8, "Struct", &MembersView(value))
            }
            Value::Array(value) => {
                serializer.serialize_newtype_variant("Value", 9, "Array", &ValuesView(value))
            }
            Value::CompactArray(value) => {
                serializer.serialize_newtype_variant("Value", 9, "Array", &value.elements(true))
            }
        }
    }
}

struct MembersView<'a>(&'a [(String, Value)]);

impl Serialize for MembersView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(self.0.len()))?;
        for (name, value) in self.0 {
            output.serialize_element(&(name, ValueView(value)))?;
        }
        output.end()
    }
}

struct ValuesView<'a>(&'a [Value]);

impl Serialize for ValuesView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            output.serialize_element(&ValueView(value))?;
        }
        output.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// Full-envelope equivalence compares an independently built legacy value to the
    /// compact source bytes, including float tokens, canonical padding and raw metadata.
    #[test]
    fn compact_complete_record_matches_legacy_array_and_widened_float_wire() {
        use super::super::{
            compact_array::try_decode,
            schema_format::{FieldSchema, Schema},
        };
        let schema = Schema::parse(br#"{"version":1,"records":[]}"#).unwrap();
        let layout = FieldSchema {
            kind: "struct".into(),
            size: Some(8),
            members: vec![
                FieldSchema {
                    kind: "f32".into(),
                    offset: 4,
                    name: "same".into(),
                    ..Default::default()
                },
                FieldSchema {
                    kind: "i16".into(),
                    name: "same".into(),
                    ..Default::default()
                },
                FieldSchema {
                    kind: "bytes".into(),
                    offset: 2,
                    size: Some(2),
                    name: "padding".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut bytes = Vec::new();
        let mut values = Vec::new();
        for bits in [0_u32, 0x8000_0000, 1, 0x8000_0001, 0x3dcccccd, 0x7f7f_ffff] {
            bytes.extend([0, 128, 127, 255]);
            bytes.extend(bits.to_le_bytes());
            values.push(Value::Struct(vec![
                ("same".into(), Value::Float(f32::from_bits(bits))),
                ("same".into(), Value::Signed(-32768)),
                ("padding".into(), Value::Bytes(vec![127, 255])),
            ]));
        }
        let compact = Value::CompactArray(
            try_decode(&layout, &bytes, values.len(), 8, &schema, 0)
                .unwrap()
                .unwrap(),
        );
        let ordinary = Value::Array(values);
        assert_eq!(
            serde_json::to_vec(&compact).unwrap(),
            serde_json::to_vec(&ordinary).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&ValueView(&compact)).unwrap(),
            serde_json::to_vec(&ValueView(&ordinary)).unwrap()
        );
        let mut compact_record = record(vec![field(compact)]);
        compact_record.fields[0].canonical_bytes = bytes.clone();
        let mut ordinary_record = record(vec![field(ordinary)]);
        ordinary_record.fields[0].canonical_bytes = bytes;
        let localization = BTreeMap::from([(0, serde_json::json!({"status":"absent","id":null}))]);
        let mut left = Vec::new();
        let mut right = Vec::new();
        write_record(&mut left, &compact_record, &localization).unwrap();
        write_record(&mut right, &ordinary_record, &localization).unwrap();
        assert_eq!(left, right);
        assert_eq!(
            written(&compact_record, &localization),
            written(&ordinary_record, &localization)
        );
        struct BrokenBody {
            remaining: usize,
        }
        impl io::Write for BrokenBody {
            /// Accept a bounded prefix, then fail while the compact body is being streamed.
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.remaining == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "compact body failure",
                    ));
                }
                let count = bytes.len().min(self.remaining);
                self.remaining -= count;
                Ok(count)
            }
            /// Keep flushing independent of the injected body-write failure.
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let prefix = left
            .windows(b"\"Array\":[".len())
            .position(|part| part == b"\"Array\":[")
            .unwrap();
        let error = write_record(
            &mut BrokenBody {
                remaining: prefix + 30,
            },
            &compact_record,
            &localization,
        )
        .unwrap_err();
        assert!(error.is_io());
    }

    /// Construct independent header and physical-source metadata for writer controls.
    fn record(fields: Vec<DecodedField>) -> DecodedRecord {
        DecodedRecord {
            form_id: 0xfe00_0045,
            source_form_id: 0x0100_0045,
            record_type: *b"INFO",
            flags: 0x40080,
            version_control: 0x1234_5678,
            form_version: 44,
            header_unknown: 0xabcd,
            load_order: 9,
            cell_form_id: None,
            worldspace_form_id: Some(17),
            topic_form_id: Some(23),
            source_topic_form_id: Some(0x0100_0017),
            topic_group_offset: Some(u64::MAX),
            source_record_offset: 0x1_0000_0001,
            fields,
            deferred_fields: Vec::new(),
            rejected_fields: vec![*b"TRDT", *b"DATA"],
            payload_complete: false,
            raw_payload: b"source".to_vec(),
            supported: false,
        }
    }

    /// Keep source canonical bytes distinct from the typed value under test.
    fn field(value: Value) -> DecodedField {
        DecodedField {
            opaque: false,
            localized_bank: None,
            signature: *b"DATA",
            name: "same_role".into(),
            value,
            canonical_bytes: vec![0, 127, 128, 255],
        }
    }

    /// The retained materialized production expression is the shape/value oracle.
    fn materialized(
        record: &DecodedRecord,
        localization: &BTreeMap<usize, serde_json::Value>,
    ) -> serde_json::Value {
        serde_json::json!({"form_id":record.form_id,"source_form_id":record.source_form_id,"record_type":record.record_type,"flags":record.flags,"load_order":record.load_order,"version_control":record.version_control,"form_version":record.form_version,"header_unknown":record.header_unknown,"cell_form_id":record.cell_form_id,"worldspace_form_id":record.worldspace_form_id,"topic_form_id":record.topic_form_id,"source_topic_form_id":record.source_topic_form_id,"topic_group_offset":record.topic_group_offset,"source_record_offset":record.source_record_offset,"fields":record.fields,"rejected_fields":record.rejected_fields,"payload_complete":record.payload_complete,"supported":record.supported,"localization":localization,"source_payload_sha256":crate::cache::hash_bytes(&record.raw_payload)})
    }

    /// Parse the streamed representation for exact shape and numeric equivalence.
    fn written(
        record: &DecodedRecord,
        localization: &BTreeMap<usize, serde_json::Value>,
    ) -> serde_json::Value {
        let mut output = Vec::new();
        write_record(&mut output, record, localization).unwrap();
        // The outer caller, not this function, inserts the JSONL newline.
        assert!(!output.ends_with(b"\n"));
        serde_json::from_slice(&output).unwrap()
    }

    /// Check emitted numeric tokens independently of serde_json's default float parser.
    fn check_value(expected: &Value, actual: &serde_json::Value) {
        match expected {
            Value::Float(value) => {
                if value.is_finite() {
                    let streamed = serde_json::to_string(&ValueView(expected)).unwrap();
                    let old =
                        serde_json::to_string(&serde_json::to_value(expected).unwrap()).unwrap();
                    assert_eq!(streamed, old, "numeric token must match the prior DOM");
                    let number = streamed
                        .strip_prefix("{\"Float\":")
                        .unwrap()
                        .strip_suffix('}')
                        .unwrap();
                    assert_eq!(
                        number.parse::<f64>().unwrap().to_bits(),
                        f64::from(*value).to_bits()
                    );
                    // Without float_roundtrip, serde_json may round an extreme token
                    // differently. Both whole-record paths must still parse equally.
                    assert_eq!(
                        actual["Float"],
                        serde_json::from_str::<serde_json::Value>(&old).unwrap()["Float"]
                    );
                } else {
                    assert!(actual["Float"].is_null());
                }
            }
            Value::Struct(members) => {
                let rows = actual["Struct"].as_array().unwrap();
                assert_eq!(rows.len(), members.len());
                for ((name, value), row) in members.iter().zip(rows) {
                    assert_eq!(row[0].as_str(), Some(name.as_str()));
                    check_value(value, &row[1]);
                }
            }
            Value::Array(values) => {
                let rows = actual["Array"].as_array().unwrap();
                assert_eq!(rows.len(), values.len());
                for (value, row) in values.iter().zip(rows) {
                    check_value(value, row);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn complete_envelope_and_nested_enum_shapes_match_materialized_export() {
        let nested = Value::Struct(vec![
            (
                "duplicate".into(),
                Value::Array(vec![
                    Value::Float(0.1),
                    Value::Float(-0.0),
                    Value::Bytes(vec![0, 255]),
                ]),
            ),
            (
                "duplicate".into(),
                Value::Struct(vec![("text".into(), Value::String("\0\n\t\\\"éΩ".into()))]),
            ),
        ]);
        let values = vec![
            Value::Unsigned(u64::MAX),
            Value::Signed(i64::MIN),
            Value::String("".into()),
            Value::LocalizedString(0),
            Value::FormId(0xfe00_0000),
            Value::Bytes(vec![0, 127, 128, 255]),
            Value::Deferred(vec![]),
            Value::Array(vec![]),
            nested,
        ];
        let record = record(values.into_iter().map(field).collect());
        let localization = BTreeMap::from([
            (
                0,
                serde_json::json!({"bank":"strings","id":0,"text":null,"status":"null_id"}),
            ),
            (
                3,
                serde_json::json!({"bank":"dlstrings","id":7,"text":"","status":"resolved"}),
            ),
            (
                8,
                serde_json::json!({"bank":"ilstrings","id":19,"text":null,"status":"missing"}),
            ),
        ]);
        let actual = written(&record, &localization);
        // Compare serialized/parsing paths, including the original DOM's numeric output.
        let old = serde_json::to_vec(&materialized(&record, &localization)).unwrap();
        assert_eq!(
            actual,
            serde_json::from_slice::<serde_json::Value>(&old).unwrap()
        );
        let expected_keys = [
            "form_id",
            "source_form_id",
            "record_type",
            "flags",
            "load_order",
            "version_control",
            "form_version",
            "header_unknown",
            "cell_form_id",
            "worldspace_form_id",
            "topic_form_id",
            "source_topic_form_id",
            "topic_group_offset",
            "source_record_offset",
            "fields",
            "rejected_fields",
            "payload_complete",
            "supported",
            "localization",
            "source_payload_sha256",
        ];
        let object = actual.as_object().unwrap();
        assert_eq!(object.len(), expected_keys.len());
        assert!(expected_keys.iter().all(|key| object.contains_key(*key)));
        assert_eq!(actual["form_id"].as_u64(), Some(0xfe00_0045));
        assert_eq!(actual["source_form_id"].as_u64(), Some(0x0100_0045));
        assert_eq!(actual["topic_group_offset"].as_u64(), Some(u64::MAX));
        assert_eq!(actual["source_record_offset"].as_u64(), Some(0x1_0000_0001));
        assert_eq!(
            actual["source_payload_sha256"].as_str(),
            Some("41cf6794ba4200b839c53531555f0f3998df4cbb01a4d5cb0b94e3ca5e23947d")
        );
        for (index, source) in record.fields.iter().enumerate() {
            assert_eq!(
                actual["fields"][index]["canonical_bytes"],
                serde_json::json!([0, 127, 128, 255])
            );
            check_value(&source.value, &actual["fields"][index]["value"]);
        }
    }

    /// Unknown byte occurrences keep their opaque marker; authored bytes omit it.
    #[test]
    fn opaque_fields_match_materialized_classification_and_preserve_bytes() {
        let bytes = vec![0x19, 0xa7, 0, 0x53];
        let mut authored = field(Value::Bytes(bytes.clone()));
        authored.signature = *b"DATA";
        authored.canonical_bytes = bytes.clone();
        let mut opaque = field(Value::Bytes(bytes.clone()));
        opaque.signature = *b"ZZ01";
        opaque.canonical_bytes = bytes;
        opaque.opaque = true;
        let mut record = record(vec![authored, opaque]);
        record.supported = true;
        let actual = written(&record, &BTreeMap::new());
        assert_eq!(actual, materialized(&record, &BTreeMap::new()));
        assert!(actual["fields"][0].get("opaque").is_none());
        assert_eq!(actual["fields"][0].as_object().unwrap().len(), 4);
        assert_eq!(actual["fields"][1]["opaque"], true);
        assert_eq!(actual["fields"][1].as_object().unwrap().len(), 5);
        for index in 0..2 {
            assert_eq!(
                actual["fields"][index]["value"]["Bytes"],
                serde_json::json!([0x19, 0xa7, 0, 0x53])
            );
            assert_eq!(
                actual["fields"][index]["canonical_bytes"],
                serde_json::json!([0x19, 0xa7, 0, 0x53])
            );
        }
    }

    #[test]
    fn floats_preserve_exact_widening_signed_zero_and_nonfinite_null_at_every_depth() {
        for bits in [
            0u32,
            0x8000_0000,
            0x3dcc_cccd,
            1,
            0x8000_0001,
            0x7f7f_ffff,
            0xff7f_ffff,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_0000,
        ] {
            let value = Value::Float(f32::from_bits(bits));
            let record = record(vec![field(Value::Array(vec![Value::Struct(vec![(
                "value".into(),
                value,
            )])]))]);
            let actual = written(&record, &BTreeMap::new());
            let old = serde_json::to_vec(&materialized(&record, &BTreeMap::new())).unwrap();
            assert_eq!(
                actual,
                serde_json::from_slice::<serde_json::Value>(&old).unwrap()
            );
            check_value(&record.fields[0].value, &actual["fields"][0]["value"]);
        }
    }

    #[test]
    fn empty_fields_and_nullable_physical_provenance_remain_explicit() {
        let mut record = record(Vec::new());
        record.worldspace_form_id = None;
        record.topic_form_id = None;
        record.source_topic_form_id = Some(0);
        record.topic_group_offset = None;
        record.rejected_fields.clear();
        record.payload_complete = true;
        record.supported = true;
        let actual = written(&record, &BTreeMap::new());
        assert_eq!(actual, materialized(&record, &BTreeMap::new()));
        assert_eq!(actual["fields"], serde_json::json!([]));
        assert_eq!(actual["localization"], serde_json::json!({}));
        assert!(actual["topic_form_id"].is_null() && actual["topic_group_offset"].is_null());
        assert_eq!(actual["source_topic_form_id"].as_u64(), Some(0));
    }

    #[test]
    fn writer_errors_propagate_after_partial_output() {
        struct Broken {
            remaining: usize,
            written: usize,
        }
        impl Write for Broken {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.remaining == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "synthetic writer failure",
                    ));
                }
                let length = bytes.len().min(self.remaining);
                self.remaining -= length;
                self.written += length;
                Ok(length)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut broken = Broken {
            remaining: 32,
            written: 0,
        };
        assert!(
            write_record(&mut broken, &record(Vec::new()), &BTreeMap::new())
                .unwrap_err()
                .is_io()
        );
        assert_eq!(broken.written, 32);
    }
}
