//! Adapt schema-decoded fields to the established runtime projections.
//! Source payloads remain separate from resolved canonical projection fields.

use crate::{
    config::RecordReader,
    esm::{
        cell_cache::validate_inhouse_land,
        exporter::{create_tables, export_inhouse_to_db, validate_inhouse_record},
        load_order::LoadOrder,
        records::RawRecord,
    },
    records::{self, Value},
};
use color_eyre::Result;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

/// Identity includes schema bytes so an authored layout change invalidates reuse.
pub fn reader_identity(reader: RecordReader) -> serde_json::Value {
    match reader {
        RecordReader::Legacy => serde_json::json!({"mode": "legacy"}),
        RecordReader::Inhouse => serde_json::json!({
            "mode": "inhouse", "adapter_version": 1,
            "schema_sha256": crate::cache::hash_bytes(records::SCHEMA_BYTES),
            "language": "english",
        }),
    }
}

/// Export a fresh diagnostic record bundle without converting meshes, textures or scripts.
/// Inputs stay read-only; retained STRINGS extraction is evidence workspace data.
pub fn export_record_bundle(data: &Path, plugins_file: &Path, output: &Path) -> Result<usize> {
    export_record_bundle_typed(data, plugins_file, output, &[])
}

/// Include ordered typed family fields and their localized lookup evidence in JSONL.
pub fn export_record_bundle_typed(
    data: &Path,
    plugins_file: &Path,
    output: &Path,
    signatures: &[[u8; 4]],
) -> Result<usize> {
    let config = crate::config::PipelineConfig::new(data, output);
    config.validate()?;
    color_eyre::eyre::ensure!(!output.exists(), "record bundle output must be new");
    let plugins = crate::esm::read_plugins_txt(plugins_file, data)?;
    fs::create_dir(output)?;
    let strings_root = output.join(".strings-input");
    fs::create_dir(&strings_root)?;
    let paths = localized_string_paths(&plugins);
    let mut archives = fs::read_dir(data)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bsa") || extension.eq_ignore_ascii_case("ba2")
            })
        })
        .filter(|path| crate::pipeline::archive_load_order_priority(path, &plugins).is_some())
        .collect::<Vec<_>>();
    crate::pipeline::sort_archives_by_load_order(&mut archives, &plugins);
    for archive in archives {
        if let Err(error) =
            crate::archive::ArchiveExtractor::extract_paths(&archive, &strings_root, &paths)
        {
            eprintln!(
                "warning: {}: unable to extract localized names: {error}",
                archive.display()
            );
        }
    }
    let records = convert_plugins_with_dump(
        &plugins,
        &output.join("skyrim_world.db"),
        &strings_root,
        (!signatures.is_empty()).then_some((output, signatures)),
    )?;
    let count =
        crate::esm::cell_cache::write_cell_cache(&records, &output.join("cell_cache.rkyv"))?;
    write_reader_identity(output, RecordReader::Inhouse)?;
    fs::write(output.join("records-schema.json"), records::SCHEMA_BYTES)?;
    Ok(count)
}

/// Stamp generated pack metadata after database/cache regeneration.
pub(crate) fn write_reader_identity(staging: &Path, reader: RecordReader) -> Result<()> {
    let path = staging.join("record-reader.json");
    if path.is_file() {
        fs::remove_file(&path)?;
    }
    fs::write(path, serde_json::to_vec_pretty(&reader_identity(reader))?)?;
    Ok(())
}

/// English localized-name tables needed by the selected plugins.
pub(crate) fn localized_string_paths(plugins: &[PathBuf]) -> BTreeSet<PathBuf> {
    plugins
        .iter()
        .filter_map(|path| path.file_stem())
        .flat_map(|stem| {
            ["strings", "dlstrings", "ilstrings"].map(|bank| {
                PathBuf::from(format!(
                    "strings/{}_english.{bank}",
                    stem.to_string_lossy().to_ascii_lowercase()
                ))
            })
        })
        .collect()
}

/// Bounded diagnostics retain one concrete example per plugin.
#[derive(Default)]
struct Warnings(BTreeMap<String, (u64, String)>);

impl Warnings {
    /// Count a local omission without accumulating every failing record.
    fn skipped(&mut self, plugin: &str, message: String) {
        let entry = self.0.entry(plugin.to_owned()).or_insert((0, message));
        entry.0 += 1;
    }

    /// Emit deterministic plugin summaries after usable neighbors are exported.
    fn report(&self) {
        for (plugin, (count, first)) in &self.0 {
            eprintln!(
                "warning: {plugin}: inhouse adapter omitted {count} unusable fields/records (first: {first})"
            );
        }
    }
}

/// Decode and export with one shared load-order mapping; plugin data failures stay local.
pub(crate) fn convert_plugins(
    plugin_paths: &[PathBuf],
    db_path: &Path,
    strings_root: &Path,
) -> Result<HashMap<u32, RawRecord>> {
    convert_plugins_with_dump(plugin_paths, db_path, strings_root, None)
}

/// Diagnostic export shares the exact decoder, winner and text lookup paths with publication.
fn convert_plugins_with_dump(
    plugin_paths: &[PathBuf],
    db_path: &Path,
    strings_root: &Path,
    dump: Option<(&Path, &[[u8; 4]])>,
) -> Result<HashMap<u32, RawRecord>> {
    let order = LoadOrder::read(plugin_paths)?;
    let decoded = records::read_plugins_with_validation(plugin_paths, &order, validate_candidate)?;
    let mut warnings = Warnings::default();
    let mut tables: HashMap<(usize, String), HashMap<u32, String>> = HashMap::new();
    let mut typed_output = dump
        .map(|(output, _)| fs::File::create(output.join("typed-records.jsonl")).map(BufWriter::new))
        .transpose()?;
    let mut master = HashMap::with_capacity(decoded.records.len());
    for record in decoded.records.values() {
        let priority = record.load_order as usize;
        let plugin = &order.names[priority];
        let mut raw = record.to_raw_record();
        let mut omitted = BTreeSet::new();
        let mut localization = BTreeMap::new();
        for (index, field) in record.fields.iter().enumerate() {
            if let Value::String(text) = &field.value {
                // Typed CP1252/UTF-8 decoding owns the text; exporters consume UTF-8.
                raw.subrecords[index].1 = runtime_string(text);
            }
            if let Value::LocalizedString(id) = &field.value {
                let bank = records::string_table(&record.record_type, &field.name);
                let table = tables
                    .entry((priority, bank.to_owned()))
                    .or_insert_with(|| {
                        read_plugin_strings(&plugin_paths[priority], strings_root, bank)
                            .unwrap_or_else(|error| {
                                warnings.skipped(plugin, format!("localized table: {error}"));
                                HashMap::new()
                            })
                    });
                localization.insert(
                    index,
                    serde_json::json!({"bank": bank, "id": id, "text": table.get(id)}),
                );
                // A missing ID means NULL, never the decimal key or its four raw bytes.
                if let Some(text) = table.get(id) {
                    raw.subrecords[index].1 = runtime_string(text);
                } else {
                    omitted.insert(index);
                    if *id != 0 {
                        warnings.skipped(
                            plugin,
                            format!(
                                "{:08X} {} missing localized ID {id}",
                                record.form_id, field.name
                            ),
                        );
                    }
                }
            }
        }
        if let Some(writer) = &mut typed_output
            && dump.is_some_and(|(_, signatures)| signatures.contains(&record.record_type))
        {
            serde_json::to_writer(
                &mut *writer,
                &serde_json::json!({"form_id":record.form_id,"source_form_id":record.source_form_id,"record_type":record.record_type,"flags":record.flags,"load_order":record.load_order,"version_control":record.version_control,"form_version":record.form_version,"header_unknown":record.header_unknown,"cell_form_id":record.cell_form_id,"worldspace_form_id":record.worldspace_form_id,"fields":record.fields,"rejected_fields":record.rejected_fields,"payload_complete":record.payload_complete,"supported":record.supported,"localization":localization,"source_payload_sha256":crate::cache::hash_bytes(&record.raw_payload)}),
            )?;
            writer.write_all(b"\n")?;
        }
        if !omitted.is_empty() {
            raw.subrecords = raw
                .subrecords
                .into_iter()
                .enumerate()
                .filter_map(|(index, field)| (!omitted.contains(&index)).then_some(field))
                .collect();
        }
        let invalid = validate_inhouse_record(&raw)
            .map_err(|error| error.to_string())
            .and_then(|_| {
                if raw.record_type == *b"LAND" {
                    validate_inhouse_land(&raw).map_err(|error| error.to_string())
                } else {
                    Ok(())
                }
            });
        if let Err(error) = invalid {
            warnings.skipped(
                plugin,
                format!(
                    "{:08X} {}: {error}",
                    raw.form_id,
                    String::from_utf8_lossy(&raw.record_type)
                ),
            );
            continue;
        }
        master.insert(raw.form_id, raw);
    }
    if let Some(writer) = &mut typed_output {
        writer.flush()?;
    }
    let conn = Connection::open(db_path)?;
    create_tables(&conn)?;
    for (priority, path) in plugin_paths.iter().enumerate() {
        let checksum = Sha256::digest(fs::read(path)?);
        conn.execute(
            "INSERT OR REPLACE INTO plugins(id,name,priority,checksum) VALUES (?1,?2,?3,?4)",
            params![
                priority as i64,
                path.file_name().unwrap_or_default().to_string_lossy(),
                priority as i64,
                checksum.as_slice()
            ],
        )?;
    }
    export_inhouse_to_db(&conn, &master, &order)?;
    // Keep the established rkyv records.data contract for runtime tools.
    // Auxiliary provenance holds verbatim source framing and file-relative IDs.
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS inhouse_source_records(form_id INTEGER PRIMARY KEY, source_form_id INTEGER NOT NULL, load_order INTEGER NOT NULL, record_type TEXT NOT NULL, flags INTEGER NOT NULL, payload BLOB NOT NULL); DELETE FROM inhouse_source_records;")?;
    for (&id, record) in &decoded.records {
        tx.execute(
            "INSERT INTO inhouse_source_records(form_id,source_form_id,load_order,record_type,flags,payload) VALUES (?1,?2,?3,?4,?5,?6)",
            params![id, record.source_form_id, record.load_order, String::from_utf8_lossy(&record.record_type), record.flags, record.raw_payload],
        )?;
    }
    tx.commit()?;
    if let Some(staging) = db_path.parent() {
        let path = staging.join("inhouse-reader-diagnostics.json");
        if path.is_file() {
            fs::remove_file(&path)?;
        }
        fs::write(
            path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "decoder": decoded.diagnostics, "adapter": warnings.0,
            }))?,
        )?;
    }
    warnings.report();
    Ok(master)
}

/// Validate required runtime semantics before merge so a bad override leaves its predecessor.
fn validate_candidate(record: &records::DecodedRecord) -> std::result::Result<(), String> {
    if !matches!(&record.record_type, b"LAND" | b"MOVT" | b"GMST" | b"RACE") {
        return Ok(());
    }
    if record.record_type == *b"LAND"
        && (!record.payload_complete || record.rejected_fields.contains(b"VHGT"))
    {
        return Err("incomplete terrain payload or rejected authored VHGT".into());
    }
    let raw = record.to_raw_record();
    validate_inhouse_record(&raw).map_err(|error| error.to_string())?;
    if record.record_type == *b"LAND" {
        validate_inhouse_land(&raw).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Encode decoded text for the shared runtime extractor without changing original source bytes.
fn runtime_string(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Find a plugin's loose STRINGS override first, then the effective staged archive VFS.
fn read_plugin_strings(
    plugin: &Path,
    strings_root: &Path,
    bank: &str,
) -> Result<HashMap<u32, String>> {
    let file_name = format!(
        "{}_english.{bank}",
        plugin.file_stem().unwrap_or_default().to_string_lossy()
    );
    for root in [plugin.parent().unwrap_or(Path::new(".")), strings_root] {
        if let Some(folder) = child_case_insensitive(root, "strings")
            && let Some(path) = child_case_insensitive(&folder, &file_name)
        {
            return parse_text_table(&fs::read(path)?, bank != "strings");
        }
    }
    Ok(HashMap::new())
}

/// Resolve a single path component on both case-sensitive and Windows filesystems.
fn child_case_insensitive(parent: &Path, name: &str) -> Option<PathBuf> {
    let direct = parent.join(name);
    if direct.exists() {
        return Some(direct);
    }
    fs::read_dir(parent)
        .ok()?
        .filter_map(|entry| entry.ok())
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(name)
        })
        .map(|entry| entry.path())
}

/// Read STRINGS offsets within their declared data region; one bad entry is skipped.
/// DLSTRINGS/ILSTRINGS directory offsets point to a checked length prefix followed by text.
fn parse_text_table(bytes: &[u8], length_prefixed: bool) -> Result<HashMap<u32, String>> {
    let word = |offset: usize| {
        bytes
            .get(offset..offset + 4)
            .and_then(|data| data.try_into().ok())
            .map(u32::from_le_bytes)
    };
    let count =
        word(0).ok_or_else(|| color_eyre::eyre::eyre!("truncated STRINGS header"))? as usize;
    let size = word(4).ok_or_else(|| color_eyre::eyre::eyre!("truncated STRINGS header"))? as usize;
    let data_start = count
        .checked_mul(8)
        .and_then(|n| n.checked_add(8))
        .ok_or_else(|| color_eyre::eyre::eyre!("STRINGS directory overflow"))?;
    let data_end = data_start
        .checked_add(size)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| color_eyre::eyre::eyre!("truncated STRINGS directory/data"))?;
    let data = &bytes[data_start..data_end];
    let mut strings = HashMap::with_capacity(count);
    for index in 0..count {
        let id = word(8 + index * 8).expect("bounded directory");
        let offset = word(12 + index * 8).expect("bounded directory") as usize;
        let tail = data.get(offset..);
        let tail = if length_prefixed {
            tail.and_then(|tail| {
                let size = u32::from_le_bytes(tail.get(..4)?.try_into().ok()?) as usize;
                tail.get(4..4usize.checked_add(size)?)
            })
        } else {
            tail
        };
        if let Some(tail) = tail
            && let Some(end) = tail.iter().position(|byte| *byte == 0)
        {
            strings.insert(id, records::decode_text(&tail[..end]));
        }
    }
    Ok(strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A broken length-prefixed entry cannot consume its valid table neighbor.
    #[test]
    fn dlstrings_and_ilstrings_bound_each_entry() {
        let mut table = Vec::new();
        table.extend(2u32.to_le_bytes());
        table.extend(12u32.to_le_bytes());
        table.extend(41u32.to_le_bytes());
        table.extend(0u32.to_le_bytes());
        table.extend(42u32.to_le_bytes());
        table.extend(4u32.to_le_bytes());
        table.extend(u32::MAX.to_le_bytes());
        table.extend(4u32.to_le_bytes());
        table.extend(b"yes\0");
        let decoded = parse_text_table(&table, true).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[&42], "yes");
        assert!(!decoded.contains_key(&41));
        table[4..8].copy_from_slice(&13u32.to_le_bytes());
        assert!(parse_text_table(&table, true).is_err());
    }
}
