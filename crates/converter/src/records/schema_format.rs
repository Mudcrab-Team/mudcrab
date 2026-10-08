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
        for &(family, bytes) in modules {
            let scope = match family {
                "items" => {
                    "WEAP ARMO ARMA AMMO BOOK MISC KEYM INGR ALCH SLGM SCRL APPA CONT COBJ EQUP"
                }
                "actors" => "NPC_ RACE CLAS FACT OTFT HDPT EYES VTYP CSTY BPTD RELA ASTP LCRT MOVT",
                "magic" => {
                    "SPEL MGEF ENCH SHOU WOOP LVLI LVLN LVSP FLST KYWD GLOB GMST EXPL PROJ HAZD ARTO EFSH DUAL"
                }
                "world_extras" => "NAVM NAVI REGN ECZN SCOL PLYR CLDC HAIR PWAT RGDL SCPT",
                "visual_extras" => {
                    "WTHR CLMT IMGS IMAD LGTM MATO MATT IPCT IPDS CAMS CPTH VOLI LENS SPGD RFCT"
                }
                "audio_extras" => {
                    "SOUN SNDR SOPM SNCT MUSC MUST ASPC REVB FSTP FSTS DOBJ DEBR ADDN AVIF CLFM COLL ANIO TACT LSCR"
                }
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
            check_subrecord_signature(signature)?;
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
        for child in field.members.iter().chain(&field.fields) {
            self.validate_field(child, depth + 1)?;
        }
        Ok(())
    }
}

/// Preserve four native ASCII bytes, including binary weather and image curve tags.
fn check_subrecord_signature(signature: &str) -> Result<(), String> {
    if signature.len() != 4 || !signature.is_ascii() {
        return Err(format!("invalid subrecord schema signature {signature:?}"));
    }
    Ok(())
}

/// Accept four printable ASCII signature bytes, never variable-width text.
fn check_signature(signature: &str) -> Result<(), String> {
    if signature.len() != 4 || !signature.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(format!("invalid schema signature {signature:?}"));
    }
    Ok(())
}
