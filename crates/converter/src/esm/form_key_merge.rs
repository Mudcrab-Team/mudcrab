//! Native record admission and winner identity before the compatibility writer packs IDs.
//! FormKey follows Mutagen's owner/local identity; source payload words remain source-local.
use super::{
    EsmParser, GameSettingIdentities, RemapWarnings, binary::parse_plugin_file_with_warnings,
    form_keys::FormKeyOrder, records::RawRecord, remap_record_using,
};
use color_eyre::Result;
use shared::{form_key::FormKey, source_form_id::ZeroPolicy};
use std::{cell::RefCell, collections::HashMap, path::PathBuf};

/// A recognized native link resolved without any runtime slot allocation.
#[derive(Debug, Clone)]
pub struct ResolvedLink {
    pub source_word: u32,
    pub key: Option<FormKey>,
}

/// Native supplier bytes and resolved identities are separate from legacy writer projections.
#[derive(Debug)]
pub struct FormKeyRecord {
    pub source: RawRecord,
    pub cell: Option<FormKey>,
    pub worldspace: Option<FormKey>,
    pub links: Vec<ResolvedLink>,
}

/// A cap-free native winner set. Numeric keys have meaning only with this run's plugin table.
#[derive(Debug)]
pub struct FormKeyConversion {
    pub order: FormKeyOrder,
    pub records: HashMap<FormKey, FormKeyRecord>,
    pub warnings: Vec<String>,
    pub notices: Vec<String>,
}

/// Explicit result of requesting the unchanged u32 writer after cap-free native resolution.
#[derive(Debug, PartialEq, Eq)]
pub enum LegacyWriterOutcome {
    Published,
    Unavailable { notice: String },
}

impl EsmParser {
    /// Run native FormKey resolution and request the legacy writer only when all owners fit.
    /// Extended orders retain their complete native winner set and leave the destination untouched.
    pub fn convert_form_key_plugins(
        paths: &[PathBuf],
        database: &std::path::Path,
    ) -> Result<(FormKeyConversion, LegacyWriterOutcome)> {
        let conversion = Self::merge_plugins_form_keys(paths)?;
        if let Some(notice) = conversion.order.notice() {
            eprintln!("notice: {notice}");
            return Ok((conversion, LegacyWriterOutcome::Unavailable { notice }));
        }
        Self::convert_plugins(paths, database)?;
        Ok((conversion, LegacyWriterOutcome::Published))
    }

    /// Resolve native records before packing, retaining every accepted plugin beyond legacy limits.
    /// Inputs must be in their recovered load order; this API neither selects nor reorders plugins.
    /// The old writer remains a separate checked boundary; no stand-in numbers are allocated.
    pub fn merge_plugins_form_keys(paths: &[PathBuf]) -> Result<FormKeyConversion> {
        let order = FormKeyOrder::read(paths)?;
        let warnings = RemapWarnings::default();
        let mut summaries = Vec::new();
        let mut records = HashMap::new();
        let mut settings = GameSettingIdentities::default();
        for (priority, path) in paths.iter().enumerate() {
            let name = &order
                .table
                .entry(order.order[priority])
                .expect("allocated supplier")
                .display_name;
            let (native, parser_warnings) = parse_plugin_file_with_warnings(path)?;
            summaries.extend(parser_warnings);
            for mut source in native {
                source.load_order = priority as u32;
                let raw = source.form_id;
                let result = if source.record_type == *b"GMST" {
                    order.gmst_header_key(priority, raw)
                } else {
                    order.resolve(priority, raw, ZeroPolicy::Null)
                };
                let mut key = match result {
                    Ok(key) => key,
                    Err(error) => {
                        warnings.skipped_record(name, &source, raw, error.to_string());
                        continue;
                    }
                };
                let links = RefCell::new(Vec::new());
                let mut checked = source.clone();
                let admission = remap_record_using(
                    &mut checked,
                    name,
                    &warnings,
                    |word, strict| {
                        let resolved = if !strict && source.record_type == *b"GMST" {
                            order.gmst_header_key(priority, word)?
                        } else {
                            order.resolve(priority, word, ZeroPolicy::Null)?
                        };
                        if strict {
                            links.borrow_mut().push(ResolvedLink {
                                source_word: word,
                                key: resolved,
                            });
                        }
                        // Validate framing and link ownership without packing or changing native words.
                        Ok(word)
                    },
                    false,
                );
                if let Err(error) = admission {
                    warnings.skipped_record(name, &source, raw, error.to_string());
                    continue;
                }
                if source.record_type == *b"GMST" {
                    match settings.resolve(&source, key, name, &records) {
                        Ok(canonical) => key = canonical,
                        Err(error) => {
                            warnings.skipped_record(name, &source, raw, error.to_string());
                            continue;
                        }
                    }
                } else if key.is_some_and(|key| settings.aliases.contains_key(&key)) {
                    warnings.skipped_record(
                        name,
                        &source,
                        raw,
                        "record collides with GMST identity".into(),
                    );
                    continue;
                }
                let Some(key) = key else {
                    warnings.skipped_record(
                        name,
                        &source,
                        raw,
                        "record has no publishable identity".into(),
                    );
                    continue;
                };
                if source.is_deleted() {
                    records.remove(&key);
                    continue;
                }
                let cell = source
                    .cell_form_id
                    .map(|id| order.resolve(priority, id, ZeroPolicy::Null))
                    .transpose();
                let world = source
                    .worldspace_form_id
                    .map(|id| order.resolve(priority, id, ZeroPolicy::Null))
                    .transpose();
                match (cell, world) {
                    (Ok(cell), Ok(world)) => {
                        records.insert(
                            key,
                            FormKeyRecord {
                                source,
                                cell: cell.flatten(),
                                worldspace: world.flatten(),
                                links: links.into_inner(),
                            },
                        );
                    }
                    (cell, world) => warnings.skipped_record(
                        name,
                        &source,
                        raw,
                        format!(
                            "invalid containing group: {:?} {:?}",
                            cell.err(),
                            world.err()
                        ),
                    ),
                }
            }
        }
        let (remap_warnings, mut notices) = warnings.report();
        summaries.extend(remap_warnings);
        if let Some(notice) = order.notice() {
            notices.push(notice);
        }
        Ok(FormKeyConversion {
            order,
            records,
            warnings: summaries,
            notices,
        })
    }
}

impl std::borrow::Borrow<RawRecord> for FormKeyRecord {
    /// Allow shared GMST collision validation to inspect the source signature.
    fn borrow(&self) -> &RawRecord {
        &self.source
    }
}
