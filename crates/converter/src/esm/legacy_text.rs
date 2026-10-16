//! Localized text for the legacy reader, matching what the in-house reader publishes.
//!
//! A plugin with the TES4 `Localized` flag (0x80) stores each localized string field (`FULL`,
//! `DESC`, ...) as a four-byte ID into its `Strings/<plugin>_english.<bank>` table; a plain
//! plugin stores Windows-1252 text. The in-house reader replaces both with UTF-8 text in its
//! canonical records and leaves a field it cannot resolve out of the runtime projections. This
//! pass does the same for the legacy reader, for the subrecords the in-house schema types as
//! localized strings without context (`records::localized_field_bank`), with the in-house table
//! lookup, so both readers publish the same names and the same `records.data` text.

use super::{
    inhouse::{StringSources, localized_text, read_plugin_strings, runtime_string},
    load_order::LoadOrder,
    records::RawRecord,
};
use crate::records;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
};

/// The TES4 header flag of a plugin whose localized fields are string-table IDs.
const LOCALIZED: u32 = 0x80;

/// Fields left without text, by record, as the exporter's runtime projection expects them.
pub(crate) type UnresolvedFields = HashMap<u32, BTreeSet<usize>>;

/// Resolve every localized string field of the winning `records` in place, returning the fields
/// that have no text. One warning per plugin, with a count and the first example, is printed.
pub(crate) fn resolve_localized_fields(
    records: &mut HashMap<u32, RawRecord>,
    plugin_paths: &[PathBuf],
    order: &LoadOrder,
    sources: StringSources<'_>,
) -> UnresolvedFields {
    let mut tables: HashMap<(usize, &'static str), Option<HashMap<u32, String>>> = HashMap::new();
    let mut unresolved = UnresolvedFields::new();
    let mut misses: BTreeMap<usize, (usize, String)> = BTreeMap::new();
    for record in records.values_mut().filter(|record| !record.is_deleted()) {
        let priority = record.load_order as usize;
        let localized = order
            .metadata
            .get(priority)
            .is_some_and(|metadata| metadata.flags & LOCALIZED != 0);
        for (index, (tag, data)) in record.subrecords.iter_mut().enumerate() {
            let Some(bank) = records::localized_field_bank(&record.record_type, tag) else {
                continue;
            };
            if !localized {
                let end = data
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(data.len());
                *data = runtime_string(&records::decode_text(&data[..end]));
                continue;
            }
            let id = <[u8; 4]>::try_from(data.as_slice())
                .ok()
                .map(u32::from_le_bytes);
            let table = tables.entry((priority, bank)).or_insert_with(|| {
                plugin_paths
                    .get(priority)
                    .and_then(|plugin| read_plugin_strings(plugin, sources, bank).ok())
            });
            match id.and_then(|id| localized_text(id, table.as_ref()?)) {
                Some(text) => *data = runtime_string(text),
                None => {
                    unresolved.entry(record.form_id).or_default().insert(index);
                    let miss = misses.entry(priority).or_insert_with(|| {
                        let reason = match id {
                            None => format!("{} bytes, not a four-byte ID", data.len()),
                            Some(0) => "ID 0".to_owned(),
                            Some(id) if table.is_none() => {
                                format!("ID {id}: no readable {bank} table")
                            }
                            Some(id) => format!("missing localized ID {id}"),
                        };
                        (
                            0,
                            format!(
                                "{:08X} {} {}",
                                record.form_id,
                                String::from_utf8_lossy(tag),
                                reason
                            ),
                        )
                    });
                    miss.0 += 1;
                    // The in-house reader stores an unresolved name as empty text.
                    *data = runtime_string("");
                }
            }
        }
    }
    for (priority, (count, first)) in misses {
        eprintln!(
            "warning: {}: legacy reader left {count} localized fields without text (first: {first})",
            order.names[priority]
        );
    }
    unresolved
}

#[cfg(test)]
mod tests {
    use crate::records::localized_field_bank;

    /// Only subrecords the in-house schema types as localized strings, in every occurrence, are
    /// resolved, and each from the bank the schema names.
    #[test]
    fn resolves_exactly_the_unambiguous_localized_fields() {
        assert_eq!(localized_field_bank(b"CELL", b"FULL"), Some("strings"));
        assert_eq!(localized_field_bank(b"NPC_", b"FULL"), Some("strings"));
        assert_eq!(localized_field_bank(b"BOOK", b"DESC"), Some("dlstrings"));
        assert_eq!(localized_field_bank(b"CELL", b"EDID"), None, "a zstring");
        assert_eq!(
            localized_field_bank(b"QUST", b"NNAM"),
            None,
            "NNAM is localized in one QUST occurrence and a zstring in another"
        );
        assert_eq!(localized_field_bank(b"XXXX", b"FULL"), None, "no schema");
    }
}
