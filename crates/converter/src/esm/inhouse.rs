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

/// Decoder/adapter behavior version for output changes outside the authored schema.
pub const ADAPTER_VERSION: u32 = 4;

/// Identity binds decoder behavior and schema bytes so stale output cannot prove reuse.
pub fn reader_identity(reader: RecordReader) -> serde_json::Value {
    match reader {
        RecordReader::Legacy => serde_json::json!({"mode": "legacy"}),
        RecordReader::Inhouse => serde_json::json!({
            "mode": "inhouse", "adapter_version": ADAPTER_VERSION,
            "schema_sha256": crate::cache::hash_bytes(records::SCHEMA_BYTES),
            "language": "english",
            "text_encoding": "windows-1252", "vmad_text_encoding": "utf-8",
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
    let count = write_terrain_caches(&records, &output.join("skyrim_world.db"), output)?;
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
    /// Count a local decode or lookup issue without accumulating every failing record.
    fn skipped(&mut self, plugin: &str, message: String) {
        let entry = self.0.entry(plugin.to_owned()).or_insert((0, message));
        entry.0 += 1;
    }

    /// Emit deterministic plugin summaries after usable neighbors are exported.
    fn report(&self) {
        for (plugin, (count, first)) in &self.0 {
            eprintln!(
                "warning: {plugin}: inhouse adapter reported {count} local decode/lookup issues (first: {first})"
            );
        }
    }
}

/// A localized occurrence keeps its original key separately from safe runtime text.
struct LocalizedField {
    form_id: u32,
    field_index: usize,
    signature: [u8; 4],
    field_name: String,
    string_table: &'static str,
    string_id: u32,
    load_order: u32,
    resolved_text: Option<String>,
    status: &'static str,
}

/// Recover winning LAND physical order from this reader's published provenance.
pub(crate) fn terrain_source_offsets(db_path: &Path) -> Result<HashMap<u32, u64>> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement =
        conn.prepare("SELECT form_id,source_record_offset FROM inhouse_terrain_source_order")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

/// Read normalized native layer geometry and globally remapped source keys for accepted LAND.
pub(crate) fn terrain_preserved_layers(
    db_path: &Path,
) -> Result<HashMap<u32, Vec<shared::TerrainLayer>>> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = conn.prepare("SELECT form_id,layer_data FROM inhouse_terrain_layers")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, u32>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    let mut output = HashMap::new();
    for row in rows {
        let (id, bytes) = row?;
        output.insert(
            id,
            rkyv::from_bytes::<Vec<shared::TerrainLayer>, rkyv::rancor::Error>(&bytes)?,
        );
    }
    Ok(output)
}

/// Keep every layer and blend weight; unresolved texture keys become explicit runtime placeholders.
pub(crate) fn terrain_runtime_layers(
    db_path: &Path,
    preserved: &HashMap<u32, Vec<shared::TerrainLayer>>,
) -> Result<HashMap<u32, Vec<shared::TerrainLayer>>> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = conn.prepare("SELECT form_id FROM records WHERE record_type='LTEX'")?;
    let textures = statement
        .query_map([], |row| row.get::<_, u32>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    let mut output = preserved.clone();
    for layers in output.values_mut() {
        for layer in layers {
            if layer.texture_form_id != 0 && !textures.contains(&layer.texture_form_id) {
                layer.texture_form_id = 0;
            }
        }
    }
    Ok(output)
}

/// Share source-order and layer preservation across conversion, diagnostics and metadata rebuild.
pub(crate) fn write_terrain_caches(
    records: &HashMap<u32, RawRecord>,
    db_path: &Path,
    output: &Path,
) -> Result<usize> {
    let offsets = terrain_source_offsets(db_path)?;
    let preserved = terrain_preserved_layers(db_path)?;
    let runtime = terrain_runtime_layers(db_path, &preserved)?;
    let count = crate::esm::cell_cache::write_cell_cache_with_preserved_layers(
        records,
        &offsets,
        &runtime,
        &output.join("cell_cache.rkyv"),
    )?;
    crate::esm::cell_cache::write_cell_cache_with_preserved_layers(
        records,
        &offsets,
        &preserved,
        &output.join("cell_cache_preserved.rkyv"),
    )?;
    Ok(count)
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
    let mut warnings = Warnings::default();
    let mut preserved_layers = HashMap::new();
    let decoded = records::read_plugins_with_validation_and_observer(
        plugin_paths,
        &order,
        validate_candidate,
        |accepted| {
            for record in accepted
                .records
                .values()
                .filter(|record| record.record_type == *b"LAND")
            {
                match crate::esm::cell_cache::preserved_texture_layers(&record.to_raw_record()) {
                    Ok(layers) => {
                        preserved_layers.insert(record.form_id, layers);
                    }
                    Err(error) => warnings.skipped(
                        &order.names[record.load_order as usize],
                        format!("{:08X} terrain layer provenance: {error}", record.form_id),
                    ),
                }
            }
        },
    )?;
    let mut localized_fields = Vec::new();
    let mut unresolved_fields = HashMap::new();
    let mut tables: HashMap<(usize, String), HashMap<u32, String>> = HashMap::new();
    let mut typed_output = dump
        .map(|(output, _)| fs::File::create(output.join("typed-records.jsonl")).map(BufWriter::new))
        .transpose()?;
    let mut master = HashMap::with_capacity(decoded.records.len());
    for record in decoded.records.values() {
        let priority = record.load_order as usize;
        let plugin = &order.names[priority];
        let mut raw = record.to_raw_record();
        let mut unresolved = BTreeSet::new();
        let mut localized = Vec::new();
        let mut localization = BTreeMap::new();
        for (index, field) in record.fields.iter().enumerate() {
            if let Value::String(text) = &field.value
                && let Some(bytes) = runtime_text(text, &raw.subrecords[index].1)
            {
                raw.subrecords[index].1 = bytes;
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
                // Zero is a null key even if a malformed table gives it text.
                let resolved = if *id == 0 { None } else { table.get(id) };
                let status = if *id == 0 {
                    "null_id"
                } else if resolved.is_some() {
                    "resolved"
                } else {
                    "missing"
                };
                localization.insert(
                    index,
                    serde_json::json!({"bank": bank, "id": id, "text": resolved, "status": status}),
                );
                localized.push(LocalizedField {
                    form_id: record.form_id,
                    field_index: index,
                    signature: field.signature,
                    field_name: field.name.clone(),
                    string_table: bank,
                    string_id: *id,
                    load_order: record.load_order,
                    resolved_text: resolved.cloned(),
                    status,
                });
                // Keep the occurrence and key; unresolved text is never invented.
                if let Some(text) = resolved {
                    raw.subrecords[index].1 = runtime_string(text);
                } else {
                    raw.subrecords[index].1 = runtime_string("");
                    unresolved.insert(index);
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
                &serde_json::json!({"form_id":record.form_id,"source_form_id":record.source_form_id,"record_type":record.record_type,"flags":record.flags,"load_order":record.load_order,"version_control":record.version_control,"form_version":record.form_version,"header_unknown":record.header_unknown,"cell_form_id":record.cell_form_id,"worldspace_form_id":record.worldspace_form_id,"topic_form_id":record.topic_form_id,"source_topic_form_id":record.source_topic_form_id,"topic_group_offset":record.topic_group_offset,"source_record_offset":record.source_record_offset,"fields":record.fields,"rejected_fields":record.rejected_fields,"payload_complete":record.payload_complete,"supported":record.supported,"localization":localization,"source_payload_sha256":crate::cache::hash_bytes(&record.raw_payload)}),
            )?;
            writer.write_all(b"\n")?;
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
        if !unresolved.is_empty() {
            unresolved_fields.insert(raw.form_id, unresolved);
        }
        localized_fields.extend(localized);
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
    let terrain_offsets = decoded
        .records
        .values()
        .filter(|record| record.record_type == *b"LAND" && master.contains_key(&record.form_id))
        .map(|record| (record.form_id, record.source_record_offset))
        .collect::<HashMap<_, _>>();
    export_inhouse_to_db(&conn, &master, &order, &unresolved_fields, &terrain_offsets)?;
    // Keep the established rkyv records.data contract for runtime tools.
    // Auxiliary provenance holds verbatim source framing and file-relative IDs.
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS inhouse_source_records(form_id INTEGER PRIMARY KEY, source_form_id INTEGER NOT NULL, load_order INTEGER NOT NULL, record_type TEXT NOT NULL, flags INTEGER NOT NULL, payload BLOB NOT NULL); DELETE FROM inhouse_source_records;")?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS inhouse_localized_fields(form_id INTEGER NOT NULL, field_index INTEGER NOT NULL, signature TEXT NOT NULL, field_name TEXT NOT NULL, string_table TEXT NOT NULL, string_id INTEGER NOT NULL, load_order INTEGER NOT NULL, resolved_text TEXT, status TEXT NOT NULL CHECK(status IN ('resolved','null_id','missing')), PRIMARY KEY(form_id,field_index)); DELETE FROM inhouse_localized_fields; CREATE TABLE IF NOT EXISTS inhouse_terrain_source_order(form_id INTEGER PRIMARY KEY, source_record_offset INTEGER NOT NULL); DELETE FROM inhouse_terrain_source_order;")?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS inhouse_terrain_layers(form_id INTEGER PRIMARY KEY, cell_id INTEGER NOT NULL, load_order INTEGER NOT NULL, layer_data BLOB NOT NULL, unresolved_texture_ids TEXT NOT NULL); DELETE FROM inhouse_terrain_layers;")?;
    for (id, layers) in preserved_layers {
        let Some(record) = master.get(&id) else {
            continue;
        };
        let unresolved = layers
            .iter()
            .map(|layer| layer.texture_form_id)
            .filter(|id| {
                *id != 0
                    && !master
                        .get(id)
                        .is_some_and(|record| record.record_type == *b"LTEX")
            })
            .collect::<BTreeSet<_>>();
        tx.execute(
            "INSERT INTO inhouse_terrain_layers(form_id,cell_id,load_order,layer_data,unresolved_texture_ids) VALUES (?1,?2,?3,?4,?5)",
            params![id, record.cell_form_id.unwrap_or(id), record.load_order, rkyv::to_bytes::<rkyv::rancor::Error>(&layers)?.as_slice(), serde_json::to_string(&unresolved)?],
        )?;
    }
    for field in localized_fields {
        tx.execute(
            "INSERT INTO inhouse_localized_fields(form_id,field_index,signature,field_name,string_table,string_id,load_order,resolved_text,status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![field.form_id, field.field_index as u64, String::from_utf8_lossy(&field.signature), field.field_name, field.string_table, field.string_id, field.load_order, field.resolved_text, field.status],
        )?;
    }
    for (id, offset) in terrain_offsets {
        tx.execute(
            "INSERT INTO inhouse_terrain_source_order(form_id,source_record_offset) VALUES (?1,?2)",
            params![id, offset],
        )?;
    }
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
/// Typed Windows-1252 decoding owns ordinary text; runtime bytes are UTF-8 with one NUL.
/// A fixed-width ASCII code stored without a terminator keeps its exact source width.
fn runtime_text(text: &str, source: &[u8]) -> Option<Vec<u8>> {
    (!(text.is_ascii() && source == text.as_bytes())).then(|| runtime_string(text))
}

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

    /// A four-byte event code without a terminator keeps its width; padded and accented
    /// text become canonical UTF-8 with one NUL.
    #[test]
    fn runtime_text_keeps_unterminated_codes_and_canonicalizes_other_text() {
        assert_eq!(runtime_text("ADIA", b"ADIA"), None);
        assert_eq!(runtime_text("Bool", b"Bool\0\0"), Some(b"Bool\0".to_vec()));
        assert_eq!(
            runtime_text("Caf\u{e9}", b"Caf\xe9\0"),
            Some("Caf\u{e9}\0".as_bytes().to_vec())
        );
    }

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
