//! Localized text for the legacy reader, matching what the in-house reader publishes.
//!
//! A plugin with the TES4 `Localized` flag (0x80) stores each localized string field (`FULL`,
//! `DESC`, ...) as a four-byte ID into its `Strings/<plugin>_<language>.<bank>` table; a plain
//! plugin stores Windows-1252 text. The in-house reader replaces both with UTF-8 text in its
//! canonical records and leaves a field it cannot resolve out of the runtime projections. This
//! pass does the same for the legacy reader, for the subrecords the in-house schema types as
//! localized strings without context (`records::localized_field_bank`), with the in-house table
//! lookup, so both readers publish the same names and the same `records.data` text.

use super::{
    inhouse::{
        ParsedTextTable, StringSources, localized_text, read_plugin_strings, runtime_string,
    },
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

type LoadedStringTable = (Option<ParsedTextTable>, Option<String>);

/// Resolve every localized string field of the winning `records` in place, returning fields
/// without text and bounded warnings/notices using the in-house reader's severity policy.
pub(crate) fn resolve_localized_fields(
    records: &mut HashMap<u32, RawRecord>,
    plugin_paths: &[PathBuf],
    order: &LoadOrder,
    sources: StringSources<'_>,
) -> (UnresolvedFields, Vec<String>, Vec<String>) {
    let mut tables: HashMap<(usize, &'static str), LoadedStringTable> = HashMap::new();
    let mut unresolved = UnresolvedFields::new();
    let mut misses: BTreeMap<usize, (usize, String)> = BTreeMap::new();
    let mut notices: BTreeMap<usize, (usize, String)> = BTreeMap::new();
    let mut record_ids = records.keys().copied().collect::<Vec<_>>();
    record_ids.sort_unstable();
    for record_id in record_ids {
        let record = records
            .get_mut(&record_id)
            .expect("sorted winning record exists");
        if record.is_deleted() {
            continue;
        }
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
            let (table, table_error) = tables.entry((priority, bank)).or_insert_with(|| {
                match plugin_paths
                    .get(priority)
                    .map(|plugin| read_plugin_strings(plugin, sources, bank))
                {
                    Some(Ok(Some(table))) => (Some(table), None),
                    Some(Ok(None)) => (None, Some(format!("missing {bank} string table"))),
                    Some(Err(error)) => (None, Some(error.to_string())),
                    None => (None, Some("plugin path unavailable".to_owned())),
                }
            });
            match id.and_then(|id| localized_text(id, &table.as_ref()?.values)) {
                Some(text) => *data = runtime_string(text),
                None => {
                    unresolved.entry(record.form_id).or_default().insert(index);
                    // Zero is the reader's null-key sentinel, not a failed lookup.
                    if id != Some(0) {
                        let malformed = id.is_some_and(|id| {
                            table
                                .as_ref()
                                .is_some_and(|table| table.malformed_ids.contains(&id))
                        });
                        let reason = match id {
                            None => format!("{} bytes, not a four-byte ID", data.len()),
                            Some(id) if table.is_none() => format!(
                                "ID {id}: unreadable {bank} table ({})",
                                table_error.as_deref().unwrap_or("unavailable")
                            ),
                            Some(id) if malformed => {
                                format!("references malformed {bank} entry for localized ID {id}")
                            }
                            Some(id) => format!("missing localized ID {id}"),
                        };
                        // An absent key in a readable bank is a faithful data gap; a
                        // malformed field, referenced entry or unreadable bank is data loss.
                        let summaries = if id.is_some() && table.is_some() && !malformed {
                            &mut notices
                        } else {
                            &mut misses
                        };
                        let miss = summaries.entry(priority).or_insert_with(|| {
                            (
                                0,
                                format!(
                                    "{:08X} {} {reason}",
                                    record.form_id,
                                    String::from_utf8_lossy(tag),
                                ),
                            )
                        });
                        miss.0 += 1;
                    }
                    // The in-house reader stores an unresolved name as empty text.
                    *data = runtime_string("");
                }
            }
        }
    }
    let mut warnings = Vec::with_capacity(misses.len());
    for (priority, (count, first)) in misses {
        warnings.push(format!(
            "warning: {}: legacy reader left {count} localized fields without text (first: {first})",
            order.names[priority]
        ));
    }
    let notices = notices
        .into_iter()
        .map(|(priority, (count, first))| {
            format!(
                "notice: {}: {count} localized IDs missing from readable string tables (first: {first})",
                order.names[priority]
            )
        })
        .collect();
    (unresolved, warnings, notices)
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
