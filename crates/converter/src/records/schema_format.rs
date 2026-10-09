//! The authored, versioned schema contract shared by build validation and decoding.
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// A complete schema, with reusable authored definitions and source citations.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    pub version: u32,
    #[serde(default)]
    pub sources: serde_json::Value,
    #[serde(default)]
    pub definitions: BTreeMap<String, FieldSchema>,
    #[serde(default)]
    pub common_fields: Vec<FieldSchema>,
    pub records: Vec<RecordSchema>,
}

/// File-order subrecord definitions for one supported record signature.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordSchema {
    pub signature: String,
    #[serde(default)]
    pub source: String,
    pub fields: Vec<FieldSchema>,
    #[serde(default)]
    pub allow_unordered: bool,
}

/// A typed subrecord, struct member, array element, union or subrecord group.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSchema {
    pub signature: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    pub size: Option<usize>,
    #[serde(default)]
    pub sizes: Vec<usize>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub repeat: bool,
    /// A repeating subrecord group ends at its final explicit empty marker.
    #[serde(default)]
    pub repeat_terminated: bool,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub members: Vec<FieldSchema>,
    #[serde(default)]
    pub fields: Vec<FieldSchema>,
    pub group: Option<String>,
    pub definition: Option<String>,
    pub decider: Option<String>,
    /// A whole trailing struct member may be absent at a declared valid size.
    #[serde(default)]
    pub optional: bool,
    /// A present but malformed mandatory field invalidates only its candidate record.
    #[serde(default)]
    pub reject_record_on_error: bool,
    /// Localized text bank; absent means the ordinary STRINGS bank.
    pub string_table: Option<String>,
    pub count: Option<serde_json::Value>,
    #[serde(default)]
    pub flags: serde_json::Value,
    #[serde(default, rename = "enum")]
    pub enumeration: serde_json::Value,
}

impl Schema {
    /// Assemble independently owned family overlays and validate their combined definitions.
    pub fn assemble(base: &[u8], modules: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
        let mut combined: serde_json::Value =
            serde_json::from_slice(base).map_err(|error| error.to_string())?;
        let mut owned = BTreeSet::new();
        let mut conditions_loaded = false;
        for &(family, bytes) in modules {
            let scope = match family {
                "items" => {
                    "WEAP ARMO ARMA AMMO BOOK MISC KEYM INGR ALCH SLGM SCRL APPA CONT COBJ EQUP"
                }
                "actors" => "NPC_ RACE CLAS FACT OTFT HDPT EYES VTYP CSTY BPTD RELA ASTP LCRT MOVT",
                "magic" => {
                    "SPEL MGEF ENCH SHOU WOOP LVLI LVLN LVSP FLST KYWD GLOB GMST EXPL PROJ HAZD ARTO EFSH DUAL"
                }
                "dialogue" => "QUST DIAL INFO DLBR DLVW SCEN SMBN SMQN SMEN MESG LCTN",
                "conditions" => "",
                "ai" => "PACK IDLE IDLM AACT",
                "perks" => "PERK",
                _ => return Err(format!("unknown schema family {family}")),
            };
            let module: serde_json::Value =
                serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
            let parsed: Self =
                serde_json::from_value(module.clone()).map_err(|error| error.to_string())?;
            if parsed.version != 1 || !parsed.common_fields.is_empty() {
                return Err(format!(
                    "{family}: unsupported version or shared-field ownership"
                ));
            }
            if family == "conditions" {
                if conditions_loaded
                    || parsed
                        .definitions
                        .keys()
                        .any(|name| !matches!(name.as_str(), "condition" | "conditions"))
                    || !parsed.definitions.contains_key("condition")
                    || !parsed.definitions.contains_key("conditions")
                {
                    return Err(
                        "conditions: missing, duplicate or unowned shared definitions".into(),
                    );
                }
                conditions_loaded = true;
            }
            for key in ["definitions", "sources"] {
                if let Some(additions) = module.get(key).and_then(|value| value.as_object()) {
                    let destination = combined
                        .get_mut(key)
                        .and_then(|value| value.as_object_mut())
                        .ok_or_else(|| format!("base {key} must be an object"))?;
                    for (name, value) in additions {
                        if let Some(existing) = destination.get(name) {
                            if existing != value {
                                return Err(format!("{family}: conflicting shared {key} {name}"));
                            }
                        } else {
                            destination.insert(name.clone(), value.clone());
                        }
                    }
                }
            }
            for record in module["records"].as_array().ok_or("family lacks records")? {
                let signature = record["signature"]
                    .as_str()
                    .ok_or("record lacks signature")?;
                if !scope.split_whitespace().any(|allowed| allowed == signature)
                    || !owned.insert(signature.to_owned())
                {
                    return Err(format!("{family}: duplicate or unowned record {signature}"));
                }
                let records = combined["records"]
                    .as_array_mut()
                    .ok_or("base lacks records")?;
                if let Some(existing) = records
                    .iter_mut()
                    .find(|entry| entry["signature"] == signature)
                {
                    *existing = record.clone();
                } else {
                    records.push(record.clone());
                }
            }
        }
        if conditions_loaded {
            activate_conditions(&mut combined);
            combined["definitions"]
                .as_object_mut()
                .expect("checked definitions")
                .remove("pending_conditions");
        }
        let bytes = serde_json::to_vec_pretty(&combined).map_err(|error| error.to_string())?;
        Self::parse(&bytes)?;
        Ok(bytes)
    }

    /// Parse and validate the data before it can drive any binary reads.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let schema: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        schema.validate()?;
        Ok(schema)
    }

    /// Refuse unknown kinds, cycles, malformed signatures and unsupported deciders.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!(
                "unsupported record schema version {}",
                self.version
            ));
        }
        let mut signatures = BTreeSet::new();
        for record in &self.records {
            check_signature(&record.signature)?;
            if !signatures.insert(&record.signature) {
                return Err(format!("duplicate record schema {}", record.signature));
            }
            for field in &record.fields {
                self.validate_field(field, 0)?;
                if record.allow_unordered && self.has_terminated_repeat(field) {
                    return Err(format!(
                        "{}: terminated repeats require ordered matching",
                        record.signature
                    ));
                }
            }
        }
        for field in self.definitions.values() {
            self.validate_field(field, 0)?;
        }
        for field in &self.common_fields {
            self.validate_field(field, 0)?;
        }
        Ok(())
    }

    /// Ordered boundaries cannot be bypassed by the unordered fallback matcher.
    fn has_terminated_repeat(&self, field: &FieldSchema) -> bool {
        let field = self.resolve(field).expect("validated definition");
        field.repeat_terminated
            || field
                .fields
                .iter()
                .chain(&field.members)
                .any(|child| self.has_terminated_repeat(child))
    }

    /// Expand one shared entry, preserving its occurrence's signature and repetition.
    pub fn resolve(&self, field: &FieldSchema) -> Result<FieldSchema, String> {
        let Some(name) = &field.definition else {
            return Ok(field.clone());
        };
        let mut result = self
            .definitions
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown schema definition {name}"))?;
        result.signature.clone_from(&field.signature);
        result.repeat |= field.repeat;
        result.repeat_terminated |= field.repeat_terminated;
        result.optional |= field.optional;
        result.reject_record_on_error |= field.reject_record_on_error;
        if field.string_table.is_some() {
            result.string_table.clone_from(&field.string_table);
        }
        result.offset = field.offset;
        if !field.name.is_empty() {
            result.name.clone_from(&field.name);
        }
        if !field.source.is_empty() {
            result.source.clone_from(&field.source);
        }
        Ok(result)
    }

    /// Validate nested definitions with a depth cap, including cyclic references.
    fn validate_field(&self, field: &FieldSchema, depth: usize) -> Result<(), String> {
        if depth >= 32 {
            return Err("schema nesting or definition cycle exceeds 32 levels".into());
        }
        if let Some(signature) = &field.signature {
            check_signature(signature)?;
        }
        for target in &field.targets {
            if target != "*" {
                check_signature(target)?;
            }
        }
        if field.targets.iter().any(|target| target == "*") && field.targets.len() != 1 {
            return Err(format!(
                "wildcard target must stand alone for {}",
                field.name
            ));
        }
        if field
            .string_table
            .as_deref()
            .is_some_and(|bank| !matches!(bank, "strings" | "dlstrings" | "ilstrings"))
        {
            return Err(format!("unknown string bank for {}", field.name));
        }
        if field.definition.is_some() {
            return self.validate_field(&self.resolve(field)?, depth + 1);
        }
        if !matches!(
            field.kind.as_str(),
            "u8" | "i8"
                | "u16"
                | "i16"
                | "u32"
                | "i32"
                | "u64"
                | "i64"
                | "f32"
                | "zstring"
                | "lstring"
                | "form_id"
                | "bytes"
                | "struct"
                | "array"
                | "union"
                | "group"
        ) {
            return Err(format!(
                "unknown field kind {:?} for {}",
                field.kind, field.name
            ));
        }
        if let Some(decider) = &field.decider
            && !matches!(
                decider.as_str(),
                "gmst_value"
                    | "localized_string"
                    | "legacy_linked_reference"
                    | "cell_grid"
                    | "movement_speeds"
                    | "water_visual"
                    | "alternate_textures"
                    | "size"
                    | "vmad"
            )
            && !super::deciders::known(decider)
        {
            return Err(format!("unknown decider {decider}"));
        }
        if (field.size == Some(0) || field.sizes.contains(&0))
            && field.kind != "bytes"
            && !(field.kind == "struct" && field.members.iter().all(|member| member.optional))
        {
            return Err(format!("zero field size for {}", field.name));
        }
        if field.kind == "form_id" && field.targets.is_empty() {
            return Err(format!("link field {} has no declared targets", field.name));
        }
        if field.repeat_terminated {
            let valid_marker = field.fields.last().is_some_and(|marker| {
                self.resolve(marker).is_ok_and(|marker| {
                    marker.signature.is_some()
                        && marker.kind == "bytes"
                        && marker.size == Some(0)
                        && !marker.repeat
                })
            });
            let valid_start = field.fields.first().is_some_and(|start| {
                self.resolve(start).is_ok_and(|start| {
                    start.signature.is_some() && start.kind != "group" && !start.repeat
                })
            });
            if field.kind != "group"
                || !field.repeat
                || field.fields.len() < 2
                || !valid_start
                || !valid_marker
            {
                return Err(format!(
                    "terminated repeat {} requires a repeating group, initial signature and final empty marker",
                    field.name
                ));
            }
        }
        for child in field.members.iter().chain(&field.fields) {
            self.validate_field(child, depth + 1)?;
        }
        Ok(())
    }
}

/// Activate the shared owner's definitions without changing separately owned family files.
fn activate_conditions(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            if object.get("definition").and_then(|value| value.as_str())
                == Some("pending_conditions")
            {
                object.insert("definition".into(), "conditions".into());
            }
            let pending = object.get("kind").and_then(|value| value.as_str()) == Some("bytes")
                && (object.get("source").and_then(|value| value.as_str())
                    == Some("pending-component:D")
                    || object
                        .get("name")
                        .and_then(|value| value.as_str())
                        .is_some_and(|name| name.starts_with("pending_condition")));
            if pending {
                let signature = object
                    .get("signature")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_owned();
                match signature.as_str() {
                    "CTDA" => {
                        object.remove("kind");
                        object.insert("definition".into(), "condition".into());
                        object.insert("name".into(), "condition".into());
                        object.insert("source".into(), "conditions-native".into());
                    }
                    "CIS1" | "CIS2" => {
                        object.insert("kind".into(), "zstring".into());
                        object.insert(
                            "name".into(),
                            format!("condition_string_{}", &signature[3..]).into(),
                        );
                        object.insert("source".into(), "conditions-native".into());
                    }
                    _ => {}
                }
            }
            for child in object.values_mut() {
                activate_conditions(child);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                activate_conditions(child);
            }
        }
        _ => {}
    }
}

/// Accept four printable ASCII signature bytes, never variable-width text.
fn check_signature(signature: &str) -> Result<(), String> {
    if signature.len() != 4 || !signature.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(format!("invalid schema signature {signature:?}"));
    }
    Ok(())
}
