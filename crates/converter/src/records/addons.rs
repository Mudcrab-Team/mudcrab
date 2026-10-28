//! Conversion-local schema add-ons with atomic validation and reproducible identities.
use super::{
    SCHEMA_BYTES,
    schema_format::{FieldSchema, RecordSchema, Schema},
};
use serde::{Deserialize, Serialize, de::MapAccess};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::PathBuf,
};

/// At most this many independently named add-ons may contribute to one conversion.
pub const MAX_SCHEMA_ADDONS: usize = 64;
/// Bound the exact JSON bytes retained for any one add-on.
pub const MAX_SCHEMA_ADDON_BYTES: usize = 8 * 1024 * 1024;
/// Bound the complete ordered add-on batch before parsing or retaining it.
pub const MAX_SCHEMA_ADDON_TOTAL_BYTES: usize = 32 * 1024 * 1024;

/// A portable identity for one exact add-on document, independent of its file path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaAddonIdentity {
    pub name: String,
    pub sha256: String,
}

/// Strict version-one add-on envelope; shared built-in hooks cannot be replaced.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddonDocument {
    version: u32,
    name: String,
    #[serde(default, deserialize_with = "unique_definitions")]
    definitions: BTreeMap<String, FieldSchema>,
    records: Vec<AddonRecord>,
}

/// Record declarations distinguish a new ordered type from additive independent fields.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddonRecord {
    signature: String,
    #[serde(default)]
    source: String,
    fields: Vec<FieldSchema>,
    #[serde(default)]
    allow_unordered: bool,
    #[serde(default)]
    extends: bool,
}

/// An immutable conversion-local bundle; constructing it never changes embedded singletons.
#[derive(Debug)]
pub struct RuntimeSchemas {
    schema: Schema,
    identities: Vec<SchemaAddonIdentity>,
    addon_bytes: Vec<Vec<u8>>,
    extensions: BTreeMap<[u8; 4], Vec<FieldSchema>>,
    schema_bytes: Vec<u8>,
    schema_sha256: String,
}

impl RuntimeSchemas {
    /// Read a bounded ordered batch completely before applying any schema declaration.
    pub fn load(paths: &[PathBuf]) -> Result<Self, String> {
        if paths.len() > MAX_SCHEMA_ADDONS {
            return Err(format!(
                "schema add-ons exceed the {MAX_SCHEMA_ADDONS}-file limit"
            ));
        }
        let mut bytes = Vec::with_capacity(paths.len());
        let mut total = 0usize;
        for path in paths {
            let file = File::open(path)
                .map_err(|error| format!("schema add-on {}: {error}", path.display()))?;
            let mut document = Vec::new();
            file.take((MAX_SCHEMA_ADDON_BYTES + 1) as u64)
                .read_to_end(&mut document)
                .map_err(|error| format!("schema add-on {}: {error}", path.display()))?;
            check_size(document.len(), &mut total)
                .map_err(|error| format!("schema add-on {}: {error}", path.display()))?;
            bytes.push(document);
        }
        let documents: Vec<_> = bytes.iter().map(Vec::as_slice).collect();
        Self::from_addon_bytes(&documents)
    }

    /// Rebuild a bundle from exact retained documents without needing their original paths.
    pub fn from_addon_bytes(documents: &[&[u8]]) -> Result<Self, String> {
        if documents.len() > MAX_SCHEMA_ADDONS {
            return Err(format!(
                "schema add-ons exceed the {MAX_SCHEMA_ADDONS}-file limit"
            ));
        }
        let mut total = 0usize;
        for (index, bytes) in documents.iter().enumerate() {
            check_size(bytes.len(), &mut total)
                .map_err(|error| format!("schema add-on {}: {error}", index + 1))?;
        }
        let mut parsed = Vec::with_capacity(documents.len());
        let mut names = BTreeSet::new();
        let mut identities = Vec::with_capacity(documents.len());
        for (index, &bytes) in documents.iter().enumerate() {
            let document: AddonDocument = serde_json::from_slice(bytes)
                .map_err(|error| format!("schema add-on {}: {error}", index + 1))?;
            check_name(&document.name)?;
            if document.version != 1 {
                return Err(format!(
                    "schema add-on {}: unsupported version {}",
                    document.name, document.version
                ));
            }
            if !names.insert(document.name.clone()) {
                return Err(format!("duplicate schema add-on name {}", document.name));
            }
            identities.push(SchemaAddonIdentity {
                name: document.name.clone(),
                sha256: digest(bytes),
            });
            parsed.push(document);
        }
        let addon_names = identities
            .iter()
            .map(|identity| identity.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let mut schema = Schema::parse(SCHEMA_BYTES)?;
        // Install the complete definitions namespace first: batch references may
        // point forward, while no declaration can replace an existing definition.
        for document in &mut parsed {
            for (name, field) in std::mem::take(&mut document.definitions) {
                if schema.definitions.contains_key(&name) {
                    return Err(format!(
                        "schema add-on {}: conflicting definition {name}",
                        document.name
                    ));
                }
                schema.definitions.insert(name, field);
            }
        }
        schema
            .validate()
            .map_err(|error| format!("invalid schema add-on batch [{addon_names}]: {error}"))?;
        let mut extensions = BTreeMap::<[u8; 4], Vec<FieldSchema>>::new();
        for document in parsed {
            let mut signatures = BTreeSet::new();
            for record in document.records {
                if !signatures.insert(record.signature.clone()) {
                    return Err(format!(
                        "schema add-on {}: duplicate record {}",
                        document.name, record.signature
                    ));
                }
                let existing = schema
                    .records
                    .iter()
                    .find(|known| known.signature == record.signature);
                if !record.extends {
                    if existing.is_some() {
                        return Err(format!(
                            "schema add-on {}: record {} already exists; use extends:true to add fields",
                            document.name, record.signature
                        ));
                    }
                    schema.records.push(RecordSchema {
                        signature: record.signature,
                        source: record.source,
                        fields: record.fields,
                        allow_unordered: record.allow_unordered,
                    });
                    continue;
                }
                let existing = existing.ok_or_else(|| {
                    format!(
                        "schema add-on {}: cannot extend unknown record {}",
                        document.name, record.signature
                    )
                })?;
                if record.allow_unordered {
                    return Err(format!(
                        "schema add-on {}: extends cannot change ordering for {}",
                        document.name, record.signature
                    ));
                }
                let signature: [u8; 4] = record
                    .signature
                    .as_bytes()
                    .try_into()
                    .map_err(|_| format!("invalid schema signature {:?}", record.signature))?;
                let mut occupied = BTreeMap::<String, String>::new();
                let mut roles = BTreeSet::new();
                collect_fields(&schema, &existing.fields, &mut occupied, &mut roles)?;
                if let Some(previous) = extensions.get(&signature) {
                    collect_fields(&schema, previous, &mut occupied, &mut roles)?;
                }
                let additions = extensions.entry(signature).or_default();
                for field in record.fields {
                    let mut field = resolve_field(&schema, &field)?;
                    let tag = field.signature.as_deref().ok_or_else(|| {
                        format!(
                            "schema add-on {}: extension field {} must have a signature",
                            document.name, field.name
                        )
                    })?;
                    if field.kind == "group" {
                        return Err(format!(
                            "schema add-on {}: extension field {} must be a standalone subrecord, not a group",
                            document.name, field.name
                        ));
                    }
                    if occupied.contains_key(tag)
                        || (!field.name.is_empty() && roles.contains(&field.name))
                    {
                        return Err(format!(
                            "schema add-on {}: conflicting extension {}.{} ({})",
                            document.name, record.signature, tag, field.name
                        ));
                    }
                    occupied.insert(tag.to_owned(), field.name.clone());
                    roles.insert(field.name.clone());
                    if field.source.is_empty() {
                        field.source.clone_from(&record.source);
                    }
                    additions.push(field);
                }
            }
        }

        // Validate the effective declarations with exactly the build script's
        // parser/validator before retaining a usable bundle. Keep extensions out
        // of the ordinary ordered matcher after this validation succeeds.
        let mut effective = schema.clone();
        for record in &mut effective.records {
            let signature: [u8; 4] = record
                .signature
                .as_bytes()
                .try_into()
                .map_err(|_| format!("invalid schema signature {:?}", record.signature))?;
            if let Some(fields) = extensions.get(&signature) {
                record.fields.extend(fields.iter().cloned());
            }
        }
        let schema_bytes = if documents.is_empty() {
            SCHEMA_BYTES.to_vec()
        } else {
            serde_json::to_vec_pretty(&effective).map_err(|error| error.to_string())?
        };
        Schema::parse(&schema_bytes)
            .map_err(|error| format!("invalid schema add-on batch [{addon_names}]: {error}"))?;
        let schema_sha256 = fingerprint(&identities);
        Ok(Self {
            schema,
            identities,
            addon_bytes: documents.iter().map(|bytes| bytes.to_vec()).collect(),
            extensions,
            schema_bytes,
            schema_sha256,
        })
    }

    /// Access validated ordinary records and definitions; extensions use their separate matcher.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Return portable identities in the exact declared add-on load order.
    pub fn identities(&self) -> &[SchemaAddonIdentity] {
        &self.identities
    }

    /// Iterate exact source documents in the same order as their portable identities.
    pub fn addon_bytes(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.addon_bytes.iter().map(Vec::as_slice)
    }

    /// Expose independent field declarations without changing any built-in matching cursor.
    pub fn extension_fields(&self, signature: &[u8; 4]) -> &[FieldSchema] {
        self.extensions.get(signature).map_or(&[], Vec::as_slice)
    }

    /// Access all additive declarations for per-conversion decoder preparation.
    pub fn extensions(&self) -> &BTreeMap<[u8; 4], Vec<FieldSchema>> {
        &self.extensions
    }

    /// Return the validated effective data schema; retain source documents to replay extensions.
    pub fn schema_bytes(&self) -> &[u8] {
        &self.schema_bytes
    }

    /// Identify built-ins and exact ordered add-on contents without including filesystem paths.
    pub fn schema_sha256(&self) -> &str {
        &self.schema_sha256
    }
}

/// Reject duplicate definition keys instead of allowing map deserialization to overwrite them.
fn unique_definitions<'de, D>(deserializer: D) -> Result<BTreeMap<String, FieldSchema>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct UniqueDefinitions;

    impl<'de> serde::de::Visitor<'de> for UniqueDefinitions {
        type Value = BTreeMap<String, FieldSchema>;

        /// Describe the required JSON object in parser diagnostics.
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a definitions object with unique names")
        }

        /// Retain each definition exactly once and reject duplicate JSON member names.
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((name, field)) = map.next_entry::<String, FieldSchema>()? {
                if result.insert(name.clone(), field).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate definition {name}"
                    )));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(UniqueDefinitions)
}

/// Bound each source independently and prevent cumulative size arithmetic from overflowing.
fn check_size(size: usize, total: &mut usize) -> Result<(), String> {
    if size > MAX_SCHEMA_ADDON_BYTES {
        return Err(format!("document exceeds {MAX_SCHEMA_ADDON_BYTES} bytes"));
    }
    if size > MAX_SCHEMA_ADDON_TOTAL_BYTES - *total {
        return Err(format!(
            "batch exceeds {MAX_SCHEMA_ADDON_TOTAL_BYTES} bytes"
        ));
    }
    *total += size;
    Ok(())
}

/// Require a compact portable name that can never encode an absolute or relative source path.
fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(
            "schema add-on name must contain 1-128 ASCII letters, digits, '.', '_' or '-'".into(),
        );
    }
    Ok(())
}

/// Compute the lowercase SHA-256 of exact source bytes, including insignificant whitespace.
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Keep the original built-in hash for an empty batch and include exact order otherwise.
fn fingerprint(identities: &[SchemaAddonIdentity]) -> String {
    if identities.is_empty() {
        return digest(SCHEMA_BYTES);
    }
    let mut hash = Sha256::new();
    hash.update(b"MudCrab runtime schema add-ons v1\0");
    hash.update(Sha256::digest(SCHEMA_BYTES));
    hash.update((identities.len() as u64).to_le_bytes());
    for identity in identities {
        hash.update((identity.name.len() as u64).to_le_bytes());
        hash.update(identity.name.as_bytes());
        hash.update(identity.sha256.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

/// Fully expand a definition occurrence with the validator's existing recursion bound.
fn resolve_field(schema: &Schema, field: &FieldSchema) -> Result<FieldSchema, String> {
    schema.resolve(field)
}

/// Index all existing subrecord leaves so no extension can change a built-in byte role.
fn collect_fields(
    schema: &Schema,
    fields: &[FieldSchema],
    signatures: &mut BTreeMap<String, String>,
    roles: &mut BTreeSet<String>,
) -> Result<(), String> {
    for field in fields {
        let field = resolve_field(schema, field)?;
        if field.kind == "group" {
            collect_fields(schema, &field.fields, signatures, roles)?;
        } else if let Some(signature) = &field.signature {
            signatures.insert(signature.clone(), field.name.clone());
            if !field.name.is_empty() {
                roles.insert(field.name);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Author a complete add-on envelope with caller-owned asymmetric declarations.
    fn addon(name: &str, records: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"version":1,"name":name,"definitions":{},"records":records}))
            .unwrap()
    }

    /// Author a standalone synthetic link on a custom record type.
    fn custom(signature: &str) -> serde_json::Value {
        json!({"signature":signature,"fields":[{"signature":"XLNK","name":"custom_link","kind":"form_id","targets":["STAT"]}]})
    }

    /// Empty batches retain the exact embedded schema and its existing identity.
    #[test]
    fn empty_batch_keeps_builtin_bytes_and_hash() {
        let schemas = RuntimeSchemas::from_addon_bytes(&[]).unwrap();
        assert_eq!(schemas.schema_bytes(), SCHEMA_BYTES);
        assert_eq!(schemas.schema_sha256(), digest(SCHEMA_BYTES));
        assert!(schemas.identities().is_empty());
        assert!(schemas.extensions().is_empty());
    }

    /// New records and additive fields retain their complete typed link descriptor.
    #[test]
    fn new_type_and_extension_preserve_builtin_order() {
        let base = Schema::parse(SCHEMA_BYTES).unwrap();
        let stat = base
            .records
            .iter()
            .find(|record| record.signature == "STAT")
            .unwrap();
        let bytes = addon(
            "synthetic-links",
            json!([
                custom("ZNEW"),
                {"signature":"STAT","extends":true,"fields":[{"signature":"XLNK","name":"addon_link","kind":"form_id","targets":["ZNEW"]}]}
            ]),
        );
        let schemas = RuntimeSchemas::from_addon_bytes(&[&bytes]).unwrap();
        let runtime = schemas
            .schema()
            .records
            .iter()
            .find(|record| record.signature == "STAT")
            .unwrap();
        assert_eq!(
            serde_json::to_value(&runtime.fields).unwrap(),
            serde_json::to_value(&stat.fields).unwrap()
        );
        assert_eq!(schemas.extension_fields(b"STAT")[0].targets, ["ZNEW"]);
        assert_eq!(schemas.identities()[0].sha256, digest(&bytes));
        assert_eq!(schemas.addon_bytes().next(), Some(bytes.as_slice()));
        assert!(
            schemas
                .schema()
                .records
                .iter()
                .any(|record| record.signature == "ZNEW")
        );
        Schema::parse(schemas.schema_bytes()).unwrap();
    }

    /// Loading the same content from unrelated paths yields equal producer identities.
    #[test]
    fn paths_do_not_affect_identity_and_source_bytes_do() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = addon("path-independent", json!([custom("ZNEW")]));
        let first = dir.path().join("first.json");
        let second = dir.path().join("unrelated.json");
        std::fs::write(&first, &bytes).unwrap();
        std::fs::write(&second, &bytes).unwrap();
        let a = RuntimeSchemas::load(&[first]).unwrap();
        let b = RuntimeSchemas::load(&[second]).unwrap();
        assert_eq!(a.identities(), b.identities());
        assert_eq!(a.schema_sha256(), b.schema_sha256());
        let mut changed = bytes;
        changed.push(b'\n');
        let c = RuntimeSchemas::from_addon_bytes(&[&changed]).unwrap();
        assert_ne!(a.schema_sha256(), c.schema_sha256());
    }

    /// Portable identities retain declared order even when effective declarations commute.
    #[test]
    fn ordered_batch_has_distinct_fingerprint() {
        let a = addon("first", json!([custom("ZAAA")]));
        let b = addon("second", json!([custom("ZBBB")]));
        let forward = RuntimeSchemas::from_addon_bytes(&[&a, &b]).unwrap();
        let reverse = RuntimeSchemas::from_addon_bytes(&[&b, &a]).unwrap();
        assert_ne!(forward.schema_sha256(), reverse.schema_sha256());
        assert_eq!(forward.identities()[0].name, "first");
        assert_eq!(reverse.identities()[0].name, "second");
    }

    /// An ordinary declaration cannot shadow an existing built-in record.
    #[test]
    fn shadowing_requires_explicit_extends() {
        let bytes = addon("shadow", json!([custom("STAT")]));
        assert!(
            RuntimeSchemas::from_addon_bytes(&[&bytes])
                .unwrap_err()
                .contains("use extends:true")
        );
        let bytes = addon(
            "missing",
            json!([{"signature":"ZNEW","extends":true,"fields":[]}]),
        );
        assert!(
            RuntimeSchemas::from_addon_bytes(&[&bytes])
                .unwrap_err()
                .contains("cannot extend unknown record")
        );
    }

    /// Extensions cannot replace built-in fields, roles, or earlier add-on declarations.
    #[test]
    fn extensions_reject_signature_role_and_batch_conflicts() {
        let base = Schema::parse(SCHEMA_BYTES).unwrap();
        let stat = base
            .records
            .iter()
            .find(|record| record.signature == "STAT")
            .unwrap();
        let first = stat
            .fields
            .iter()
            .map(|field| resolve_field(&base, field).unwrap())
            .find(|field| field.signature.is_some())
            .unwrap();
        for (tag, role) in [
            (first.signature.unwrap(), "unrelated_name".to_owned()),
            ("ZTAG".to_owned(), first.name),
        ] {
            let bytes = addon(
                "conflict",
                json!([{"signature":"STAT","extends":true,"fields":[{"signature":tag,"name":role,"kind":"u32"}]}]),
            );
            assert!(
                RuntimeSchemas::from_addon_bytes(&[&bytes])
                    .unwrap_err()
                    .contains("conflicting extension")
            );
        }
        let a = addon(
            "one",
            json!([{"signature":"STAT","extends":true,"fields":[{"signature":"ZTAG","name":"mod_tag","kind":"u32"}]}]),
        );
        let b = addon(
            "two",
            json!([{"signature":"STAT","extends":true,"fields":[{"signature":"ZTAG","name":"other_tag","kind":"u16"}]}]),
        );
        assert!(
            RuntimeSchemas::from_addon_bytes(&[&a, &b])
                .unwrap_err()
                .contains("conflicting extension")
        );
    }

    /// Failed batches never mutate a previously loaded bundle or the embedded schema.
    #[test]
    fn invalid_batch_is_atomic_and_uses_build_validation() {
        let valid = addon("valid", json!([custom("ZNEW")]));
        let before = RuntimeSchemas::from_addon_bytes(&[&valid]).unwrap();
        let invalid = addon(
            "invalid",
            json!([{"signature":"ZBAD","fields":[{"signature":"XLNK","name":"bad_link","kind":"form_id"}]}]),
        );
        let error = RuntimeSchemas::from_addon_bytes(&[&valid, &invalid]).unwrap_err();
        assert!(error.contains("has no declared targets"), "{error}");
        assert!(error.contains("batch [valid, invalid]"), "{error}");
        assert_eq!(before.identities().len(), 1);
        assert!(
            !before
                .schema()
                .records
                .iter()
                .any(|record| record.signature == "ZBAD")
        );
        assert!(
            !Schema::parse(SCHEMA_BYTES)
                .unwrap()
                .records
                .iter()
                .any(|record| record.signature == "ZNEW")
        );
    }

    /// Every envelope key and declaration namespace has one unambiguous meaning.
    #[test]
    fn strict_envelopes_reject_unknown_duplicate_and_unsupported_inputs() {
        let unknown = br#"{"version":1,"name":"bad","records":[],"common_fields":[]}"#;
        assert!(
            RuntimeSchemas::from_addon_bytes(&[unknown])
                .unwrap_err()
                .contains("unknown field")
        );
        let duplicate = br#"{"version":1,"name":"bad","definitions":{"d":{"kind":"u32"},"d":{"kind":"u16"}},"records":[]}"#;
        assert!(
            RuntimeSchemas::from_addon_bytes(&[duplicate])
                .unwrap_err()
                .contains("duplicate definition d")
        );
        let unsupported = br#"{"version":2,"name":"bad","records":[]}"#;
        assert!(
            RuntimeSchemas::from_addon_bytes(&[unsupported])
                .unwrap_err()
                .contains("unsupported version 2")
        );
        let one = addon("same", json!([]));
        assert!(
            RuntimeSchemas::from_addon_bytes(&[&one, &one])
                .unwrap_err()
                .contains("duplicate schema add-on name same")
        );
    }

    /// Definitions can resolve forward across the atomic batch without gaining replacement rights.
    #[test]
    fn batch_definitions_resolve_forward_and_reject_replacement() {
        let a = addon(
            "reference",
            json!([{"signature":"ZNEW","fields":[{"signature":"XLNK","definition":"mod_target"}]}]),
        );
        let b = br#"{"version":1,"name":"definition","definitions":{"mod_target":{"name":"target","kind":"form_id","targets":["ZNEW"]}},"records":[]}"#;
        RuntimeSchemas::from_addon_bytes(&[&a, b]).unwrap();
        let base = Schema::parse(SCHEMA_BYTES).unwrap();
        let builtin = base.definitions.keys().next().unwrap();
        let conflict = serde_json::to_vec(&json!({"version":1,"name":"replace","definitions":{(builtin):{"kind":"u32"}},"records":[]})).unwrap();
        assert!(
            RuntimeSchemas::from_addon_bytes(&[&conflict])
                .unwrap_err()
                .contains("conflicting definition")
        );
        let other = br#"{"version":1,"name":"other","definitions":{"mod_target":{"kind":"u32"}},"records":[]}"#;
        assert!(
            RuntimeSchemas::from_addon_bytes(&[b, other])
                .unwrap_err()
                .contains("conflicting definition mod_target")
        );
    }

    /// Input bounds reject excessive counts and bytes before malformed JSON is parsed.
    #[test]
    fn addon_input_limits_are_checked_before_parsing() {
        let empty = b"{}".as_slice();
        let count = vec![empty; MAX_SCHEMA_ADDONS + 1];
        assert!(
            RuntimeSchemas::from_addon_bytes(&count)
                .unwrap_err()
                .contains("file limit")
        );
        let oversized = vec![b' '; MAX_SCHEMA_ADDON_BYTES + 1];
        assert!(
            RuntimeSchemas::from_addon_bytes(&[&oversized])
                .unwrap_err()
                .contains("document exceeds")
        );
        let large = vec![b' '; MAX_SCHEMA_ADDON_BYTES];
        let batch =
            vec![large.as_slice(); MAX_SCHEMA_ADDON_TOTAL_BYTES / MAX_SCHEMA_ADDON_BYTES + 1];
        assert!(
            RuntimeSchemas::from_addon_bytes(&batch)
                .unwrap_err()
                .contains("batch exceeds")
        );
    }

    /// The runtime loader rejects the same malformed descriptors as build-time parsing.
    #[test]
    fn addon_fields_use_the_shared_validator_rules() {
        for (field, expected) in [
            (
                json!({"signature":"ZTAG","kind":"invented"}),
                "unknown field kind",
            ),
            (
                json!({"signature":"ZTAG","kind":"u32","decider":"invented"}),
                "unknown decider",
            ),
            (
                json!({"signature":"ZTAG","kind":"form_id","targets":["*","STAT"]}),
                "wildcard target must stand alone",
            ),
            (
                json!({"signature":"short","kind":"u32"}),
                "invalid subrecord schema signature",
            ),
            (
                json!({"signature":"ZTAG","kind":"u32","size":0}),
                "zero field size",
            ),
        ] {
            let bytes = addon(
                "bad-descriptor",
                json!([{"signature":"ZNEW","fields":[field]}]),
            );
            let error = RuntimeSchemas::from_addon_bytes(&[&bytes]).unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
        let cycle = br#"{"version":1,"name":"cycle","definitions":{"cycle":{"kind":"group","fields":[{"definition":"cycle"}]}},"records":[]}"#;
        assert!(
            RuntimeSchemas::from_addon_bytes(&[cycle])
                .unwrap_err()
                .contains("cycle exceeds 32 levels")
        );
    }

    /// Existing-record ordering and group membership cannot be rewritten by an extension.
    #[test]
    fn extensions_require_standalone_fields_and_preserve_ordering_policy() {
        let group = addon(
            "group",
            json!([{"signature":"STAT","extends":true,"fields":[{"name":"extension_group","kind":"group","signature":"ZGRP","fields":[]}]}]),
        );
        assert!(
            RuntimeSchemas::from_addon_bytes(&[&group])
                .unwrap_err()
                .contains("standalone subrecord")
        );
        let unordered = addon(
            "unordered",
            json!([{"signature":"STAT","extends":true,"allow_unordered":true,"fields":[]}]),
        );
        assert!(
            RuntimeSchemas::from_addon_bytes(&[&unordered])
                .unwrap_err()
                .contains("cannot change ordering")
        );
    }
}
