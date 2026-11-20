use crate::esm::{
    binary::parse_plugin_file_with_warnings,
    exporter::{create_tables, validate_inhouse_record},
    records::RawRecord,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
pub mod binary;
pub mod cell_cache;
pub mod doors;
pub mod emittance;
mod environment;
pub mod exporter;
pub mod extractors;
pub mod form_key_merge;
pub mod form_keys;
pub mod grass_cache;
pub mod inhouse;
mod inhouse_script_properties;
mod inhouse_terrain_diagnostics;
pub(crate) mod legacy_projection;
mod legacy_text;
pub mod load_order;
pub mod lodsettings;
pub mod mmap_reader;
mod object_materials;
pub mod plugins_txt;
pub mod projection_omissions;
pub mod records;
mod reference_links;
pub mod rooms;
pub mod terrain_mesh;
pub mod types;
pub use plugins_txt::{
    OFFICIAL_MASTERS, read_plugins_txt, read_plugins_txt_with_options, read_skyrim_ccc,
};
#[cfg(test)]
mod base_model_parity_tests;
#[cfg(test)]
mod default_objects_parity_tests;
#[cfg(test)]
mod display_name_parity_tests;
#[cfg(test)]
mod emittance_parity_tests;
#[cfg(test)]
mod environment_parity_tests;
#[cfg(test)]
mod form_key_contract_tests;
#[cfg(test)]
mod light_details_parity_tests;
#[cfg(test)]
mod plugin_table_tests;
#[cfg(test)]
mod reference_links_parity_tests;
#[cfg(test)]
mod report_warnings_official_tests;

pub struct EsmParser;

/// Winning records with bounded data-loss warnings and faithful-outcome notices.
#[derive(Debug)]
pub(crate) struct RecordConversion {
    pub(crate) records: HashMap<u32, RawRecord>,
    pub(crate) warnings: Vec<String>,
    pub(crate) notices: Vec<String>,
    /// In-house stage seconds for the database profile; legacy exports record none.
    pub(crate) phases: std::collections::BTreeMap<String, f64>,
}

/// Populate the common plugin metadata contract without allocating each complete input.
fn insert_plugins(conn: &Connection, plugin_paths: &[PathBuf]) -> Result<()> {
    let mut statement = conn.prepare_cached(
        "INSERT OR REPLACE INTO plugins (id, name, priority, checksum) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (priority, path) in plugin_paths.iter().enumerate() {
        let mut file = File::open(path)
            .wrap_err_with(|| format!("failed to open {} for plugin checksum", path.display()))?;
        let checksum = plugin_checksum(&mut file)
            .wrap_err_with(|| format!("failed to hash plugin {}", path.display()))?;
        statement.execute(params![
            priority as i64,
            path.file_name().unwrap_or_default().to_string_lossy(),
            priority as i64,
            checksum.as_slice()
        ])?;
    }
    Ok(())
}

/// Hash every source byte with bounded scratch space and preserve the raw SHA-256 BLOB.
fn plugin_checksum(reader: &mut impl Read) -> std::io::Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}

impl EsmParser {
    /// Export using the selected frontend, returning exactly the projection input.
    pub fn convert_plugins_with_reader(
        plugin_paths: &[PathBuf],
        db_path: &Path,
        reader: crate::config::RecordReader,
        strings_root: &Path,
    ) -> Result<HashMap<u32, RawRecord>> {
        Self::convert_plugins_with_reader_and_schemas(
            plugin_paths,
            db_path,
            reader,
            strings_root,
            crate::config::DEFAULT_LANGUAGE,
            None,
        )
        .map(|conversion| conversion.records)
    }

    /// Publish with one immutable schema batch and retain summaries for the pipeline report.
    /// Localized names come from `strings_root`'s tables in `language`.
    pub(crate) fn convert_plugins_with_reader_and_schemas(
        plugin_paths: &[PathBuf],
        db_path: &Path,
        reader: crate::config::RecordReader,
        strings_root: &Path,
        language: &str,
        schemas: Option<&crate::records::addons::RuntimeSchemas>,
    ) -> Result<RecordConversion> {
        Self::convert_plugins_with_reader_and_schemas_with_string_sources(
            plugin_paths,
            db_path,
            reader,
            inhouse::StringSources {
                roots: inhouse::StringRoots::PluginLooseFirst(strings_root),
                language,
            },
            schemas,
        )
    }

    /// Select loose string inputs independently of the physical parent and collect reader summaries.
    pub(crate) fn convert_plugins_with_reader_and_schemas_with_string_sources(
        plugin_paths: &[PathBuf],
        db_path: &Path,
        reader: crate::config::RecordReader,
        string_sources: inhouse::StringSources<'_>,
        schemas: Option<&crate::records::addons::RuntimeSchemas>,
    ) -> Result<RecordConversion> {
        match reader {
            crate::config::RecordReader::Legacy => {
                Self::convert_legacy_plugins_report(plugin_paths, db_path, Some(string_sources))
            }
            crate::config::RecordReader::Inhouse => {
                inhouse::convert_plugins_with_string_sources_report(
                    plugin_paths,
                    db_path,
                    string_sources,
                    schemas,
                )
            }
        }
    }
    /// Parses .esm files and exports world data to skyrim_world.db
    pub fn convert_plugins(plugin_paths: &[PathBuf], db_path: &Path) -> Result<()> {
        Self::convert_plugins_with_records(plugin_paths, db_path).map(|_| ())
    }

    /// Export and return the same merged records used for the database, so the
    /// terrain cache cannot observe a second merge with a different load order.
    pub(crate) fn convert_plugins_with_records(
        plugin_paths: &[PathBuf],
        db_path: &Path,
    ) -> Result<HashMap<u32, RawRecord>> {
        Self::convert_plugins_with_records_report(plugin_paths, db_path)
            .map(|conversion| conversion.records)
    }

    /// Export legacy records with each printed recovery warning and compatibility notice once.
    pub(crate) fn convert_plugins_with_records_report(
        plugin_paths: &[PathBuf],
        db_path: &Path,
    ) -> Result<RecordConversion> {
        Self::convert_legacy_plugins_report(plugin_paths, db_path, None)
    }

    /// Export legacy records, resolving localized text through the in-house string sources.
    fn convert_legacy_plugins_report(
        plugin_paths: &[PathBuf],
        db_path: &Path,
        string_sources: Option<inhouse::StringSources<'_>>,
    ) -> Result<RecordConversion> {
        let (accepted_paths, name_warnings) = form_keys::recover_plugin_paths(plugin_paths)?;
        let plugin_paths = accepted_paths.as_slice();
        let string_sources = string_sources.unwrap_or_else(|| {
            let loose = plugin_paths
                .first()
                .and_then(|path| path.parent())
                .unwrap_or(Path::new("."));
            inhouse::StringSources::default_language(inhouse::StringRoots::PluginLooseFirst(loose))
        });
        let order = load_order::LoadOrder::read(plugin_paths)?;
        let mut master = Self::merge_plugins_with_load_order(plugin_paths, &order)?;
        master.warnings.extend(name_warnings);
        let (unresolved, localization_warnings, localization_notices) =
            legacy_text::resolve_localized_fields(
                &mut master.records,
                plugin_paths,
                &order,
                string_sources,
            );
        let conn = Connection::open(db_path)?;
        create_tables(&conn)?;
        exporter::export_plugins_to_db(&conn, &master.records, &order, plugin_paths, &unresolved)?;

        for diagnostic in localization_warnings.iter().chain(&localization_notices) {
            eprintln!("{diagnostic}");
        }
        master.warnings.extend(localization_warnings);
        master.notices.extend(localization_notices);
        Ok(master)
    }

    /// Merge plugin records with validated slots and EditorID-based game settings.
    pub fn merge_plugins(plugin_paths: &[PathBuf]) -> Result<HashMap<u32, RawRecord>> {
        let (accepted_paths, _) = form_keys::recover_plugin_paths(plugin_paths)?;
        let plugin_paths = accepted_paths.as_slice();
        let order = load_order::LoadOrder::read(plugin_paths)?;
        Self::merge_plugins_with_load_order(plugin_paths, &order)
            .map(|conversion| conversion.records)
    }

    /// Use one load-order mapping for merge/export and retain all printed reader summaries.
    fn merge_plugins_with_load_order(
        plugin_paths: &[PathBuf],
        order: &load_order::LoadOrder,
    ) -> Result<RecordConversion> {
        let mut merged = HashMap::<shared::form_key::FormKey, RawRecord>::new();
        let identities = order
            .form_key_context()
            .map_err(|error| color_eyre::eyre::eyre!(error))?;
        identities.start_resolution_run();
        let legacy = identities.legacy_view()?;
        let mut summaries = Vec::new();
        // Unlike ordinary forms, game settings override by EditorID. Retain
        // the first definition's key/owner while taking the last setting value.
        // Keep keys through deletions so a later restoration has the same ID.
        let mut game_settings = GameSettingIdentities::default();
        let warnings = RemapWarnings::default();
        let mut source_ids = std::collections::HashSet::new();
        for (priority, path) in plugin_paths.iter().enumerate() {
            crate::records::scanner::scan_plugin_headers(
                path,
                &mut Default::default(),
                |header| {
                    if let Some(id) = crate::records::source_identity::header_runtime_id(
                        &header.record_type,
                        header.source_form_id,
                        priority,
                        order,
                    ) {
                        source_ids.insert(id);
                    }
                },
            )?;
        }
        let pack_wide_light = |key: shared::form_key::FormKey| -> Result<u32> {
            Ok(legacy.to_legacy_u32(shared::form_key::FormKey::new(
                key.plugin(),
                key.local() & 0xfff,
            )?)?)
        };
        let mut admitted_exact_lights = std::collections::HashSet::new();
        let mut wide_candidates = BTreeMap::<usize, Vec<RawRecord>>::new();
        // A header locates potential conflicts, but only an admitted exact record can
        // reject a fallback. Replay just the contested wide records in supplier order.
        let suppliers = [false, true].into_iter().flat_map(|replay| {
            plugin_paths
                .iter()
                .enumerate()
                .map(move |(priority, path)| (priority, path, replay))
        });
        for (priority, path, replay) in suppliers {
            let mut sources = HashMap::<shared::form_key::FormKey, (u32, bool)>::new();
            let records = if replay {
                let Some(records) = wide_candidates.remove(&priority) else {
                    continue;
                };
                records
            } else {
                let (records, parser_summaries) = parse_plugin_file_with_warnings(path)?;
                summaries.extend(parser_summaries);
                records
            };
            for mut record in records {
                record.load_order = priority as u32;
                let source_id = record.form_id;
                let key = if record.record_type == *b"GMST" {
                    identities.gmst_header_key(priority, source_id)
                } else {
                    identities.resolve(
                        priority,
                        source_id,
                        shared::source_form_id::ZeroPolicy::Null,
                    )
                };
                let mut key = match key {
                    Ok(key) => key,
                    Err(error) => {
                        warnings.skipped_record(
                            &order.names[priority],
                            &record,
                            source_id,
                            error.to_string(),
                        );
                        continue;
                    }
                };
                let wide_light = key
                    .filter(|key| key.local() > 0xfff && identities.table.is_light(key.plugin()));
                if !replay
                    && let Some(wide) = wide_light
                    && let Ok(packed) = pack_wide_light(wide)
                    && source_ids.contains(&packed)
                {
                    wide_candidates.entry(priority).or_default().push(record);
                    continue;
                }
                let kind = record.record_type;
                let remap_word = |raw: u32, strict: bool, field: bool| -> Result<u32> {
                    let key = if !strict && kind == *b"GMST" {
                        identities.gmst_header_key(priority, raw)?
                    } else {
                        identities.resolve(
                            priority,
                            raw,
                            shared::source_form_id::ZeroPolicy::Null,
                        )?
                    };
                    let value: Result<u32> = match legacy.to_nullable_legacy_u32(key) {
                        Ok(value) => Ok(value),
                        Err(shared::legacy_id_view::LegacyError::LightLocalTooWide(_))
                            if !strict =>
                        {
                            let key = key.ok_or_else(|| {
                                eyre!("wide light FormID {raw:08X} has no identity")
                            })?;
                            let owner = &identities
                                .table
                                .entry(key.plugin())
                                .ok_or_else(|| {
                                    eyre!("wide light FormID {raw:08X} has no allocated owner")
                                })?
                                .normalized_name;
                            let packed = pack_wide_light(key)?;
                            warnings.truncated(owner, raw);
                            Ok(packed)
                        }
                        Err(error) => Err(error.into()),
                    };
                    let value = value?;
                    let past_master = (raw >> 24) as usize > order.metadata[priority].masters.len();
                    // Every field layout (including NAME and VMAD) uses this decision.
                    // Headers/group labels retain identity admission without a target lookup.
                    if field && past_master && value >= 0x800 && !source_ids.contains(&value) {
                        warnings.cleared_past_master(
                            &order.names[priority],
                            format!(
                                "{} record source {source_id:08X}: optional FormID {raw:08X} exceeds master list",
                                String::from_utf8_lossy(&kind)
                            ),
                        );
                        return Ok(0);
                    }
                    if value != 0 && past_master {
                        warnings.out_of_range(&order.names[priority], raw, kind);
                    }
                    Ok(value)
                };
                let remap = |raw, strict| remap_word(raw, strict, false);
                let optional = |raw, strict| remap_word(raw, strict, true);
                let admission = remap_record_using_optional(
                    &mut record,
                    &order.names[priority],
                    &warnings,
                    remap,
                    optional,
                    matches!(
                        &kind,
                        b"GRAS" | b"LTEX" | b"TXST" | b"LAND" | b"CELL" | b"WRLD"
                    ),
                )
                .and_then(|()| {
                    if !record.is_deleted() {
                        validate_inhouse_record(&record)?;
                        if record.record_type == *b"LAND" {
                            cell_cache::validate_inhouse_land(&record)?;
                        }
                    }
                    // Reject a surviving header collision after the usual remap diagnostics.
                    color_eyre::eyre::ensure!(
                        wide_light.is_none() || !admitted_exact_lights.contains(&record.form_id),
                        "wide light FormID {source_id:08X} would collide with exact record {:08X}",
                        record.form_id
                    );
                    Ok(())
                });
                if let Err(error) = admission {
                    warnings.skipped_record(
                        &order.names[priority],
                        &record,
                        source_id,
                        error.to_string(),
                    );
                    continue;
                }
                // Preserve the old explicit malformed-light recovery at the legacy boundary;
                // the native FormKey path retains the original full local without masking.
                if wide_light.is_some() {
                    key = match legacy.from_legacy_u32(record.form_id) {
                        Ok(key) => key,
                        Err(error) => {
                            warnings.skipped_record(
                                &order.names[priority],
                                &record,
                                source_id,
                                error.to_string(),
                            );
                            continue;
                        }
                    };
                }
                if let Some(key) = key {
                    let projected =
                        (source_id >> 24) as usize > order.metadata[priority].masters.len();
                    if let Some(&(earlier, earlier_projected)) = sources.get(&key)
                        && earlier != source_id
                        && (projected || earlier_projected)
                    {
                        warnings.skipped_record(&order.names[priority], &record, source_id,
                            format!("records {earlier:08X} and {source_id:08X} resolve to the same key ({:08X}); an out-of-range master index would replace another record", record.form_id));
                        continue;
                    }
                }
                let source_key = key;
                let packed_source_id = record.form_id;
                if record.record_type == *b"GMST" {
                    let canonical = match game_settings.resolve(
                        &record,
                        key,
                        &order.names[priority],
                        &merged,
                    ) {
                        Ok(Some(canonical)) => canonical,
                        Ok(None) => {
                            warnings.skipped_record(
                                &order.names[priority],
                                &record,
                                source_id,
                                "deleted GMST has no EditorID or known setting identity".into(),
                            );
                            continue;
                        }
                        Err(error) => {
                            warnings.skipped_record(
                                &order.names[priority],
                                &record,
                                source_id,
                                error.to_string(),
                            );
                            continue;
                        }
                    };
                    record.form_id = match legacy.to_legacy_u32(canonical) {
                        Ok(id) => id,
                        Err(error) => {
                            warnings.skipped_record(
                                &order.names[priority],
                                &record,
                                source_id,
                                error.to_string(),
                            );
                            continue;
                        }
                    };
                    key = Some(canonical);
                } else if key.is_some_and(|key| game_settings.aliases.contains_key(&key)) {
                    warnings.skipped_record(
                        &order.names[priority],
                        &record,
                        source_id,
                        format!(
                            "record {:08X} collides with a GMST identity",
                            record.form_id
                        ),
                    );
                    continue;
                }
                let Some(key) = key else {
                    warnings.skipped_record(
                        &order.names[priority],
                        &record,
                        source_id,
                        "record has a null identity".into(),
                    );
                    continue;
                };
                // Reserve admitted tombstones too, using the source identity before EDID canonicalization.
                if wide_light.is_none() && packed_source_id >> 24 == 0xfe {
                    admitted_exact_lights.insert(packed_source_id);
                }
                if let Some(source_key) = source_key {
                    sources.insert(
                        source_key,
                        (
                            source_id,
                            (source_id >> 24) as usize > order.metadata[priority].masters.len(),
                        ),
                    );
                }
                if record.is_deleted() {
                    merged.remove(&key);
                } else {
                    if record.record_type == *b"DOBJ"
                        && let Some(previous) = merged.get(&key)
                        && previous.record_type == *b"DOBJ"
                    {
                        merge_default_objects(previous, &mut record);
                    }
                    merged.insert(key, record);
                }
            }
        }
        let mut records: HashMap<_, _> = merged
            .into_values()
            .map(|record| (record.form_id, record))
            .collect();
        // Check live winners after admission, including forward links, deleted targets and
        // rejected MOVTs. Packed IDs here are the checked projection of the FormKey winners.
        let mut link_records: Vec<_> = records
            .values()
            .filter(|record| matches!(&record.record_type, b"DOBJ" | b"RACE"))
            .map(|record| record.form_id)
            .collect();
        link_records.sort_unstable();
        for id in link_records {
            let Some(mut record) = records.remove(&id) else {
                continue;
            };
            let kind = record.record_type;
            let plugin = &order.names[record.load_order as usize];
            clear_missing_movement_targets(
                &mut record,
                |target| {
                    if target == id {
                        Some(kind)
                    } else {
                        records.get(&target).map(|target| target.record_type)
                    }
                },
                |field, target| {
                    warnings.skipped_optional(
                        plugin,
                        format!(
                            "record {id:08X} {field} target {target:08X} is absent or disallowed"
                        ),
                    );
                },
            );
            records.insert(id, record);
        }
        let (remap_warnings, notices) = warnings.report();
        summaries.extend(remap_warnings);
        Ok(RecordConversion {
            records,
            warnings: summaries,
            notices,
            phases: std::collections::BTreeMap::new(),
        })
    }
}

/// RACE fields that address a movement type.
pub(crate) fn is_race_movement_link(tag: &[u8]) -> bool {
    matches!(
        tag,
        b"WKMV" | b"RNMV" | b"SWMV" | b"FLMV" | b"SNMV" | b"SPMV"
    )
}

/// Clear dangling links in packed movement records and merged default objects.
pub(crate) fn clear_missing_movement_targets(
    record: &mut RawRecord,
    target_kind: impl Fn(u32) -> Option<[u8; 4]>,
    mut warn: impl FnMut(&str, u32),
) {
    let mut check = |field: &str, word: &mut [u8; 4], movement_only: bool| {
        let target = u32::from_le_bytes(*word);
        if target != 0
            && !target_kind(target).is_some_and(|kind| !movement_only || kind == *b"MOVT")
        {
            warn(field, target);
            word.fill(0);
        }
    };
    for (tag, data) in &mut record.subrecords {
        match &record.record_type {
            b"DOBJ" if tag == b"DNAM" => {
                for pair in data.as_chunks_mut::<8>().0 {
                    let [a, b, c, d, target @ ..] = pair;
                    let key = [*a, *b, *c, *d];
                    if key != [0; 4] {
                        check("DNAM", target, matches!(&key, b"DMWL" | b"DMRN"));
                    }
                }
            }
            b"RACE" if is_race_movement_link(tag) => {
                if let Ok(word) = <&mut [u8; 4]>::try_from(data.as_mut_slice()) {
                    // The remap pass has already checked field framing.
                    let field = std::str::from_utf8(tag).unwrap_or("movement link");
                    check(field, word, true);
                }
            }
            _ => {}
        }
    }
}

/// The `(use code, FormID)` pairs of a `DOBJ`'s `DNAM` subrecords, in stored order.
/// A trailing partial pair and a pair with a zero key are ignored.
pub(crate) fn default_object_pairs(subrecords: &[(Vec<u8>, Vec<u8>)]) -> Vec<([u8; 4], u32)> {
    subrecords
        .iter()
        .filter(|(tag, _)| tag.as_slice() == b"DNAM")
        .flat_map(|(_, data)| data.as_chunks::<8>().0)
        .map(|pair| {
            (
                [pair[0], pair[1], pair[2], pair[3]],
                u32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]]),
            )
        })
        // The game's loader skips a zero key. Retail Update.esm holds hundreds of all-zero
        // padding pairs, and none of them names a default object.
        .filter(|(key, _)| *key != [0; 4])
        .collect()
}

/// Merge a `DOBJ` override into the earlier winner key by key.
///
/// The game's default-object loader sets only the slots that a record lists and
/// never clears the table, so an override changes just its own keys. Update.esm
/// and the DLC override `DOBJ` `00000031` with partial lists; replacing the whole
/// record would drop, for example, the walk and run defaults that Skyrim.esm sets.
/// The merged pairs replace the `DNAM` of `record` in place, so the positions of its other
/// subrecords do not move: the in-house reader indexes them before it merges. A record
/// without a `DNAM` gets one at the end. Both record readers use this rule, so they publish
/// the same `default_objects` rows.
pub(crate) fn merge_default_objects(previous: &RawRecord, record: &mut RawRecord) {
    let mut merged = default_object_pairs(&previous.subrecords);
    for (key, target) in default_object_pairs(&record.subrecords) {
        match merged.iter_mut().find(|(earlier, _)| *earlier == key) {
            Some(slot) => slot.1 = target,
            None => merged.push((key, target)),
        }
    }
    replace_default_object_pairs(record, &merged);
}

/// Replace DNAM without moving any existing subrecord positions.
pub(crate) fn replace_default_object_pairs(record: &mut RawRecord, pairs: &[([u8; 4], u32)]) {
    let bytes: Vec<u8> = pairs
        .iter()
        .flat_map(|(key, target)| [key.as_slice(), &target.to_le_bytes()].concat())
        .collect();
    let mut bytes = Some(bytes);
    for (tag, data) in &mut record.subrecords {
        if tag.as_slice() == b"DNAM" {
            // Any extra `DNAM` is blanked, not removed, so later indices stay valid.
            *data = bytes.take().unwrap_or_default();
        }
    }
    if let Some(bytes) = bytes {
        record.subrecords.push((b"DNAM".to_vec(), bytes));
    }
}

/// Per-plugin count of out-of-range master indices, with the first ID and its record type.
type OutOfRangeCounts = BTreeMap<String, (u64, u32, [u8; 4])>;

/// Remap diagnostics counted per plugin (including references), each keeping its first
/// example, so memory and log volume scale with plugins, not records.
#[derive(Default)]
struct RemapWarnings {
    /// Light-plugin local IDs wider than 12 bits, keyed by owning plugin.
    truncations: RefCell<BTreeMap<String, (u64, u32)>>,
    /// IDs beyond the master list observed on the permissive supplier-self path.
    /// Keyed by the containing plugin, retaining the first record kind.
    /// Any separate truncation or candidate rejection remains a recovery warning.
    out_of_range: RefCell<OutOfRangeCounts>,
    /// Optional links past the master list whose self target is absent. They point at nothing
    /// in the source data too, so clearing them loses nothing and is reported as a notice.
    cleared_past_master: RefCell<BTreeMap<String, (u64, String)>>,
    /// Invalid optional links, counted per containing plugin with the first diagnostic.
    skipped_optional: RefCell<BTreeMap<String, (u64, String)>>,
    /// Unusable candidates never replace an earlier complete record.
    skipped_records: RefCell<BTreeMap<String, (u64, String)>>,
}

impl RemapWarnings {
    /// Accumulate one truncation without emitting a per-reference diagnostic.
    fn truncated(&self, owner: &str, form_id: u32) {
        let mut counts = self.truncations.borrow_mut();
        let entry = counts.entry(owner.to_owned()).or_insert((0, form_id));
        entry.0 += 1;
    }

    /// Accumulate one permissive supplier-self remap without changing recovery diagnostics.
    fn out_of_range(&self, plugin: &str, form_id: u32, record_type: [u8; 4]) {
        let mut counts = self.out_of_range.borrow_mut();
        let entry = counts
            .entry(plugin.to_owned())
            .or_insert((0, form_id, record_type));
        entry.0 += 1;
    }

    /// Count one cleared past-master link per plugin, keeping the first example.
    fn cleared_past_master(&self, plugin: &str, example: String) {
        let mut counts = self.cleared_past_master.borrow_mut();
        let entry = counts.entry(plugin.to_owned()).or_insert((0, example));
        entry.0 += 1;
    }

    /// Retain one example per plugin rather than logging every broken optional link.
    fn skipped_optional(&self, plugin: &str, example: String) {
        let mut counts = self.skipped_optional.borrow_mut();
        let entry = counts.entry(plugin.to_owned()).or_insert((0, example));
        entry.0 += 1;
    }

    fn skipped_record(&self, plugin: &str, record: &RawRecord, source_id: u32, reason: String) {
        let mut counts = self.skipped_records.borrow_mut();
        let entry = counts.entry(plugin.to_owned()).or_insert_with(|| {
            (
                0,
                format!(
                    "{} source {source_id:08X} (load-order {:08X}): {reason}",
                    String::from_utf8_lossy(&record.record_type),
                    record.form_id,
                ),
            )
        });
        entry.0 += 1;
    }

    /// Print diagnostics in kind/plugin order and return warnings separately from notices.
    fn report(&self) -> (Vec<String>, Vec<String>) {
        let mut warning_lines = Vec::new();
        let mut notice_lines = Vec::new();
        for (owner, (count, example)) in self.truncations.borrow().iter() {
            let summary = format!(
                "warning: {owner}: {count} light-plugin ID occurrences exceeded 12 bits and were truncated (first: {example:08X}); compact the plugin's FormIDs before ESL-flagging it"
            );
            eprintln!("{summary}");
            warning_lines.push(summary);
        }
        for (plugin, (count, example, record_type)) in self.out_of_range.borrow().iter() {
            let summary = format!(
                "notice: {plugin}: {count} FormID occurrences name a master index past the plugin's master list and were treated as the plugin's own (first: {example:08X} in {})",
                String::from_utf8_lossy(record_type)
            );
            eprintln!("{summary}");
            notice_lines.push(summary);
        }
        for (plugin, (count, example)) in self.cleared_past_master.borrow().iter() {
            let summary = format!(
                "notice: {plugin}: cleared {count} optional FormID links past the master list whose self target is absent (they point at no record; first: {example})"
            );
            eprintln!("{summary}");
            notice_lines.push(summary);
        }
        for (plugin, (count, example)) in self.skipped_optional.borrow().iter() {
            let summary = format!(
                "warning: {plugin}: skipped {count} invalid optional FormID links (cleared or removed; first: {example})"
            );
            eprintln!("{summary}");
            warning_lines.push(summary);
        }
        for (plugin, (count, example)) in self.skipped_records.borrow().iter() {
            let summary = format!(
                "warning: {plugin}: skipped {count} unusable record candidate(s); first: {example}; earlier complete candidates and neighboring records retained"
            );
            eprintln!("{summary}");
            warning_lines.push(summary);
        }
        (warning_lines, notice_lines)
    }
}

/// Game settings override by name, but header-only deletions refer to any
/// earlier definition's FormID. Keep every non-null alias, even after deletion.
#[derive(Default)]
struct GameSettingIdentities {
    canonical_ids: HashMap<String, shared::form_key::FormKey>,
    aliases: HashMap<shared::form_key::FormKey, shared::form_key::FormKey>,
}

impl GameSettingIdentities {
    /// Resolve a setting's persistent ID without silently deleting unrelated forms.
    fn resolve(
        &mut self,
        record: &RawRecord,
        key: Option<shared::form_key::FormKey>,
        plugin: &str,
        merged: &HashMap<shared::form_key::FormKey, impl std::borrow::Borrow<RawRecord>>,
    ) -> Result<Option<shared::form_key::FormKey>> {
        let source_id = record.form_id;
        let editor_id = extractors::SubrecordView::new(&record.subrecords)
            .get_string(b"EDID")
            .filter(|name| !name.is_empty())
            .map(|name| name.to_ascii_lowercase());
        let Some(editor_id) = editor_id else {
            if record.is_deleted() {
                if let Some(canonical) = key.and_then(|key| self.aliases.get(&key)).copied() {
                    return Ok(Some(canonical));
                }
                return Ok(None);
            }
            color_eyre::eyre::bail!("{plugin} GMST {source_id:08X} has no EditorID");
        };
        let known = self.canonical_ids.get(&editor_id).copied();
        let canonical = known.or(key).ok_or_else(|| color_eyre::eyre::eyre!(
            "GMST {editor_id} has a null FormID; needs an EditorID-keyed database representation"))?;
        if let Some(key) = key {
            color_eyre::eyre::ensure!(
                self.aliases.get(&key).is_none_or(|id| known == Some(*id))
                    && merged
                        .get(&key)
                        .is_none_or(|previous| previous.borrow().record_type == *b"GMST"),
                "GMST {editor_id} collides with another record at {source_id:08X}"
            );
            self.aliases.insert(key, canonical);
        }
        self.canonical_ids.insert(editor_id, canonical);
        Ok(Some(canonical))
    }
}

/// Placed NAME requires valid framing/packing; absent targets still clear like typed fields.
fn is_required_form_id_subrecord(record_type: &[u8; 4], tag: &[u8]) -> bool {
    tag == b"NAME" && is_placed_record_type(record_type)
}

/// Native records with a placed base and transform. Keep publication, required-link
/// validation and room membership on the same set; schema coverage is checked in tests.
pub(crate) fn is_placed_record_type(record_type: &[u8; 4]) -> bool {
    matches!(
        record_type,
        b"REFR"
            | b"ACHR"
            | b"ACRE"
            | b"PGRE"
            | b"PMIS"
            | b"PHZD"
            | b"PARW"
            | b"PBAR"
            | b"PBEA"
            | b"PCON"
            | b"PFLA"
    )
}

/// Explicit offsets for optional scalar, structure and array FormID fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormIdLayout {
    /// A fixed-size structure: each accepted byte length with its FormID offsets.
    Fixed(&'static [(usize, &'static [usize])]),
    /// A packed array of `stride`-byte entries, with a FormID at each of `form_ids` in every entry.
    Array {
        stride: usize,
        form_ids: &'static [usize],
    },
}

/// Field layouts per xEdit 9fb016884bec138ea6c7b872cec831537d464c3e's TES5 definitions.
fn form_id_layout(record_type: &[u8; 4], tag: &[u8]) -> Option<FormIdLayout> {
    use FormIdLayout::{Array, Fixed};
    if matches!(record_type, b"TES4" | b"CLFM" | b"AACT") {
        return None;
    }
    let tag: &[u8; 4] = tag.try_into().ok()?;
    let placed = is_placed_record_type(record_type);
    const SINGLE: &[(usize, &[usize])] = &[(4, &[0])];
    Some(match tag {
        // Faction membership: FormID, signed rank byte, then three unused bytes.
        b"SNAM" if record_type == b"NPC_" => Fixed(&[(8, &[0])]),
        // Scalar optional links share the same clear/drop policy as compound links.
        b"SNAM" | b"PFIG" if record_type == b"TREE" => Fixed(SINGLE),
        b"TNAM" | b"GNAM" | b"MNAM" if record_type == b"LTEX" => Fixed(SINGLE),
        b"XCWT" if record_type == b"CELL" => Fixed(SINGLE),
        // RNAM is a compound large-reference payload and TNAM is a texture path;
        // preserve those bytes under the existing unsupported-layout policy.
        // Music (`ZNAM`) as well, which the in-house schema remaps too.
        b"NAM2" | b"NAM3" | b"WNAM" | b"CNAM" | b"ZNAM" if record_type == b"WRLD" => Fixed(SINGLE),
        b"RNAM" | b"CNAM" | b"INAM" if record_type == b"NPC_" => Fixed(SINGLE),
        // NPC Traits and default outfit are optional FormIDs in the actor schema.
        // Outfit layouts follow xEdit xedit-4.1.5f's TES5 definitions (OTFT:9964-9967).
        b"WNAM" | b"TPLT" | b"DOFT" if record_type == b"NPC_" => Fixed(SINGLE),
        // INAM holds all outfit items; each word resolves independently, including NULLs.
        b"INAM" if record_type == b"OTFT" => Array {
            stride: 4,
            form_ids: &[0],
        },
        // xEdit 9fb016884bec138ea6c7b872cec831537d464c3e,
        // wbDefinitionsTES5.pas:9463-9468. Match the in-house actors schema.
        tag if record_type == b"RACE" && is_race_movement_link(tag) => Fixed(SINGLE),
        // The body chain the actor model catalog follows: a race's skin armor, each armature
        // an armor lists (`MODL` repeats, one FormID per subrecord, so each malformed entry is
        // dropped alone), and an armature's race. The in-house actors and items schemas remap
        // all three, and the catalog reads them as load-order FormIDs.
        b"WNAM" if record_type == b"RACE" => Fixed(SINGLE),
        b"MODL" if record_type == b"ARMO" => Fixed(SINGLE),
        // xEdit 4.1.5f / items.json: race and male/female skin texture sets are scalar links.
        b"RNAM" | b"NAM0" | b"NAM1" if record_type == b"ARMA" => Fixed(SINGLE),
        // Additional races repeat as one FormID per MODL subrecord; malformed entries
        // are discarded independently, matching ARMO/MODL's existing clear policy.
        b"MODL" if record_type == b"ARMA" => Fixed(SINGLE),
        b"XOWN" | b"XGLB" | b"XEZN" | b"XLCN" | b"XLRL" => Fixed(SINGLE),
        // Door destination reference, then position, rotation and flags.
        b"XTEL" if placed => Fixed(&[(32, &[0])]),
        // Enable parent, followed by flags and three unused bytes.
        b"XESP" if placed => Fixed(&[(8, &[0])]),
        // Linked-reference keyword, then the linked reference. 14 references in the
        // official plugins use the older 4-byte form holding only the linked reference.
        b"XLKR" if placed => Fixed(&[(8, &[0, 4]), (4, &[0])]),
        // Navmesh, then a u16 teleport-marker triangle and padding.
        b"XNDP" if placed => Fixed(&[(8, &[0])]),
        // Emitted light or region.
        b"XEMI" if placed => Fixed(SINGLE),
        // Attached reference, which the in-house schema remaps too.
        b"XATR" if placed => Fixed(SINGLE),
        // A light's sound and lens flare, which the in-house schema remaps too.
        b"SNAM" | b"LNAM" if record_type == b"LIGH" => Fixed(SINGLE),
        // Each activate parent is a separate subrecord: reference plus f32 delay.
        b"XAPR" if placed => Fixed(&[(8, &[0])]),
        // Location reference types.
        b"XLRT" if placed => Array {
            stride: 4,
            form_ids: &[0],
        },
        // A rider's horse.
        b"XHOR" if record_type == b"ACHR" => Fixed(SINGLE),
        // Lighting template, on cells and on worldspaces.
        b"LTMP" if matches!(record_type, b"CELL" | b"WRLD") => Fixed(SINGLE),
        // Image space, music, acoustic space, and the region a cell takes its sky and weather
        // from (`XCCM` names a REGN, not a climate).
        b"XCIM" | b"XCMO" | b"XCAS" | b"XCCM" if record_type == b"CELL" => Fixed(SINGLE),
        // Room marker references (REFR only; `LNAM` and `INAM` mean other things on other
        // record types): the rooms a portal joins, links between rooms, and a room's own
        // lighting template and image space. `XPOD` is an array of (origin, destination)
        // FormID pairs (xEdit 4.1.5f defs: `wbArray(XPOD, 'Portal Data', wbStruct('References',
        // [wbFormIDCk('Origin'), wbFormIDCk('Destination')]))`), so every u32 is a FormID.
        // The official masters only ever store one pair (8 bytes). Like every `Array`, a
        // length that is not a multiple of the stride is dropped and reported.
        b"XPOD" if record_type == b"REFR" => Array {
            stride: 4,
            form_ids: &[0],
        },
        b"XLRM" | b"LNAM" | b"INAM" if record_type == b"REFR" => Fixed(SINGLE),
        // Regions.
        b"XCLR" if record_type == b"CELL" => Array {
            stride: 4,
            form_ids: &[0],
        },
        // A region's weathers: the weather, its chance, then an optional global.
        b"RDWT" if record_type == b"REGN" => Array {
            stride: 12,
            form_ids: &[0, 8],
        },
        // A climate's weathers, laid out like a region's.
        b"WLST" if record_type == b"CLMT" => Array {
            stride: 12,
            form_ids: &[0, 8],
        },
        // Default objects: (four-character use code, FormID) pairs.
        b"DNAM" if record_type == b"DOBJ" => Array {
            stride: 8,
            form_ids: &[4],
        },
        // A weather's precipitation, visual effect, sky static and sun glare lens.
        b"MNAM" | b"NNAM" | b"TNAM" | b"GNAM" if record_type == b"WTHR" => Fixed(SINGLE),
        // Its image spaces and volumetric lighting at sunrise, day, sunset and night.
        b"IMSP" | b"HNAM" if record_type == b"WTHR" => Fixed(&[(16, &[0, 4, 8, 12])]),
        // Each sound is its own subrecord: descriptor, then a u32 kind.
        b"SNAM" if record_type == b"WTHR" => Fixed(&[(8, &[0])]),
        _ => return None,
    })
}

/// Frozen pre-FormKey remapper for differential controls on the old policy boundaries.
/// Production uses FormKey resolution and `remap_record_using` instead.
#[cfg(test)]
fn remap_record_form_ids(
    record: &mut RawRecord,
    plugin_name: &str,
    masters: &[String],
    normal_indices: &HashMap<String, u32>,
    light_indices: &HashMap<String, u32>,
    warnings: &RemapWarnings,
) -> Result<()> {
    // Native record identities retain the documented legacy compatibility outside
    // these kinds, including shipped GMST IDs beyond the master table. All known
    // fixed optional links use strict ownership/width checks, while VMAD follows
    // record-kind compatibility. Optional layouts clear invalid IDs or drop malformed
    // occurrences instead of replacing an entire candidate.
    let strict = matches!(
        &record.record_type,
        b"GRAS" | b"LTEX" | b"TXST" | b"LAND" | b"CELL" | b"WRLD"
    );
    let record_type = record.record_type;
    let remap_with_validation = |form_id: u32, strict: bool| -> Result<u32> {
        if form_id == 0 {
            return Ok(0);
        }
        let local_index = (form_id >> 24) as usize;
        color_eyre::eyre::ensure!(
            !strict || local_index <= masters.len(),
            "{plugin_name}: {form_id:08X} has invalid master index {local_index}"
        );
        if local_index > masters.len() {
            // Treated as the plugin's own, which shipped data relies on (Skyrim.esm and
            // Dawnguard.esm each carry one such ID). Reported, and the merge rejects it when
            // it would replace another record.
            warnings.out_of_range(plugin_name, form_id, record_type);
        }
        let owner = if local_index < masters.len() {
            shared::plugin_selection::normalize_plugin_name(&masters[local_index])
        } else {
            shared::plugin_selection::normalize_plugin_name(plugin_name)
        };
        if let Some(index) = light_indices.get(&owner) {
            if form_id & 0x00ff_ffff > 0xfff {
                color_eyre::eyre::ensure!(
                    !strict,
                    "{owner}: light-plugin local ID exceeds 12 bits: {form_id:08X}"
                );
                warnings.truncated(&owner, form_id);
            }
            return Ok(0xFE00_0000 | (index << 12) | (form_id & 0xFFF));
        }
        let index = normal_indices.get(&owner).ok_or_else(|| {
            color_eyre::eyre::eyre!("master {owner} is not present in load order")
        })?;
        Ok((index << 24) | (form_id & 0x00FF_FFFF))
    };
    remap_record_using(record, plugin_name, warnings, remap_with_validation, strict)
}

/// Adapt resolved identities to the unchanged four-byte projection fields.
/// Caller errors reject only this candidate; optional link failures clear only that link.
fn remap_record_using(
    record: &mut RawRecord,
    plugin_name: &str,
    warnings: &RemapWarnings,
    remap_with_validation: impl Fn(u32, bool) -> Result<u32>,
    strict: bool,
) -> Result<()> {
    remap_record_using_optional(
        record,
        plugin_name,
        warnings,
        &remap_with_validation,
        &remap_with_validation,
        strict,
    )
}

/// Apply one field-target policy to all layouts, separate from header/group identities.
fn remap_record_using_optional(
    record: &mut RawRecord,
    plugin_name: &str,
    warnings: &RemapWarnings,
    remap_with_validation: impl Fn(u32, bool) -> Result<u32>,
    remap_optional: impl Fn(u32, bool) -> Result<u32>,
    strict: bool,
) -> Result<()> {
    let record_type = record.record_type;
    let remap = |form_id| remap_with_validation(form_id, strict);
    let remap_group = |form_id| remap_with_validation(form_id, true);
    let remap_link = |form_id| remap_optional(form_id, true);
    let source_id = record.form_id;
    record.form_id = remap(record.form_id)?;
    if record.is_deleted() {
        // Only the resolved header identity (and a complete GMST EDID) is needed
        // for deletion. Unused parent/body links cannot invalidate a tombstone.
        record.cell_form_id = None;
        record.worldspace_form_id = None;
        return Ok(());
    }
    record.cell_form_id = record.cell_form_id.map(&remap_group).transpose()?;
    record.worldspace_form_id = record.worldspace_form_id.map(&remap_group).transpose()?;
    if record.record_type == *b"LAND" {
        let omitted = cell_cache::malformed_texture_items(&record.subrecords);
        if omitted != 0 {
            warnings.skipped_optional(plugin_name, format!("LAND source {source_id:08X} omitted {omitted} malformed texture items; usable layers retained"));
        }
    }
    let mut malformed_subrecords = Vec::new();
    for (index, (tag, data)) in record.subrecords.iter_mut().enumerate() {
        if record.record_type == *b"STAT" && tag.as_slice() == b"MODS" {
            let Some(offsets) = object_materials::form_offsets(data) else {
                warnings
                    .skipped_optional(plugin_name, format!("STAT {source_id:08X}: malformed MODS"));
                malformed_subrecords.push(index);
                continue;
            };
            for offset in offsets {
                let value = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                let resolved = remap_link(value).unwrap_or_else(|error| {
                    warnings.skipped_optional(
                        plugin_name,
                        format!("STAT {source_id:08X} MODS reference {value:08X}: {error}"),
                    );
                    0
                });
                data[offset..offset + 4].copy_from_slice(&resolved.to_le_bytes());
            }
            continue;
        }
        if record.record_type == *b"GRAS" && tag.as_slice() == b"MODS" {
            color_eyre::eyre::ensure!(data.len() >= 4, "truncated GRAS alternate textures");
            let count = u32::from_le_bytes(data[..4].try_into().unwrap());
            let mut offset = 4usize;
            for _ in 0..count {
                color_eyre::eyre::ensure!(
                    data.len().saturating_sub(offset) >= 4,
                    "truncated MODS name length"
                );
                let length =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                color_eyre::eyre::ensure!(
                    data.len().saturating_sub(offset) >= length.saturating_add(8),
                    "truncated MODS entry"
                );
                offset += length;
                let value = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                data[offset..offset + 4].copy_from_slice(&remap_link(value)?.to_le_bytes());
                offset += 8;
            }
            color_eyre::eyre::ensure!(offset == data.len(), "trailing bytes in GRAS MODS");
            continue;
        }
        if record.record_type == *b"LAND" && matches!(tag.as_slice(), b"BTXT" | b"ATXT") {
            // Preserve malformed assignment bytes for bounded grass diagnostics.
            // Only a complete identity can be remapped; terrain drops short items.
            if data.len() < 4 {
                continue;
            }
            let value = u32::from_le_bytes(data[..4].try_into().unwrap());
            data[..4].copy_from_slice(&remap_link(value)?.to_le_bytes());
            continue;
        }
        if record.record_type == *b"LAND" && tag.as_slice() == b"VTEX" {
            // xEdit's TES5 LAND definition uses an array of wbFormIDCk(LTEX,
            // NULL), unlike the 8-byte BTXT/ATXT layer structures above:
            // https://github.com/TES5Edit/TES5Edit/blob/dev-4.1.5/Core/wbDefinitionsTES5.pas
            color_eyre::eyre::ensure!(data.len().is_multiple_of(4), "invalid LAND VTEX length");
            for value in data.as_chunks_mut::<4>().0 {
                *value = remap_link(u32::from_le_bytes(*value))?.to_le_bytes();
            }
            continue;
        }
        if tag.as_slice() == b"VMAD" {
            // Object links use the same supplier-self selector rule as every field.
            // The existing record-kind light-width policy and framing recovery are retained.
            records::record_type::vmad::remap_primary_form_ids(data, |id| {
                remap_optional(id, strict)
            })
            .wrap_err("VMAD references")?;
            continue;
        }
        // Legacy faction remapping recognizes only the exact eight-byte layout.
        // Preserve other widths without guessing offsets, as required by main;
        // the independent in-house decoder rejects malformed canonical fields.
        if record_type == *b"NPC_" && tag.as_slice() == b"SNAM" && data.len() != 8 {
            continue;
        }
        if let Some(layout) = form_id_layout(&record.record_type, tag) {
            // Broken optional links, including wrong lengths, must not block unrelated
            // content. Drop a malformed subrecord: its FormID offsets cannot be trusted.
            let name = String::from_utf8_lossy(tag).into_owned();
            let offsets = match layout {
                FormIdLayout::Fixed(variants) => variants
                    .iter()
                    .find(|(len, _)| *len == data.len())
                    .map(|(_, offsets)| offsets.to_vec())
                    .ok_or_else(|| {
                        let sizes: Vec<String> =
                            variants.iter().map(|(len, _)| len.to_string()).collect();
                        format!(
                            "{name} must be {} bytes, found {}",
                            sizes.join(" or "),
                            data.len()
                        )
                    }),
                FormIdLayout::Array { stride, form_ids } => {
                    if data.len().is_multiple_of(stride) {
                        Ok((0..data.len())
                            .step_by(stride)
                            .flat_map(|entry| form_ids.iter().map(move |offset| entry + offset))
                            .collect())
                    } else {
                        Err(format!(
                            "{name} length {} is not a multiple of {stride}",
                            data.len()
                        ))
                    }
                }
            };
            let offsets = match offsets {
                Ok(offsets) => offsets,
                Err(reason) => {
                    warnings.skipped_optional(
                        plugin_name,
                        format!(
                            "{} record source {source_id:08X} (load-order {:08X}) {reason}",
                            String::from_utf8_lossy(&record_type),
                            record.form_id
                        ),
                    );
                    malformed_subrecords.push(index);
                    continue;
                }
            };
            for offset in offsets {
                let value = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                // Keep valid payload bytes, but publish null for an invalid FormID rather
                // than retaining a plugin-local value or guessing its owner.
                let resolved = match remap_link(value) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        warnings.skipped_optional(
                            plugin_name,
                            format!(
                                "{} record source {source_id:08X} (load-order {:08X}) {name} reference {value:08X}: {error}",
                                String::from_utf8_lossy(&record_type),
                                record.form_id
                            ),
                        );
                        0
                    }
                };
                data[offset..offset + 4].copy_from_slice(&resolved.to_le_bytes());
            }
            continue;
        }
        if is_required_form_id_subrecord(&record.record_type, tag) {
            color_eyre::eyre::ensure!(
                data.len() == 4,
                "invalid required {} reference length",
                String::from_utf8_lossy(tag)
            );
            let value = u32::from_le_bytes(data[..4].try_into().unwrap());
            data[..4].copy_from_slice(
                &remap_link(value)
                    .wrap_err_with(|| format!("{} reference", String::from_utf8_lossy(tag)))?
                    .to_le_bytes(),
            );
        }
    }
    if !malformed_subrecords.is_empty() {
        // Indices are sorted by traversal order. Remove only those occurrences, so a
        // repeated field such as XAPR keeps its valid siblings and their relative order.
        let mut index = 0;
        record.subrecords.retain(|_| {
            let keep = malformed_subrecords.binary_search(&index).is_err();
            index += 1;
            keep
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_optional_links_are_cleared_and_warnings_are_aggregated() {
        let indices = HashMap::from([("base.esm".into(), 0), ("patch.esp".into(), 2)]);
        let warnings = RemapWarnings::default();
        let mut record = RawRecord {
            form_id: 0x0100_0800,
            record_type: *b"REFR",
            flags: 0,
            subrecords: vec![
                (
                    b"XESP".to_vec(),
                    [0x0200_0900u32.to_le_bytes(), [1, 2, 3, 4]].concat(),
                ),
                (
                    b"XAPR".to_vec(),
                    [0x0300_0901u32.to_le_bytes(), 1.25f32.to_le_bytes()].concat(),
                ),
            ],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        remap_record_form_ids(
            &mut record,
            "patch.esp",
            &["base.esm".into()],
            &indices,
            &HashMap::new(),
            &warnings,
        )
        .unwrap();
        assert_eq!(
            record.subrecords[0].1,
            [0u32.to_le_bytes(), [1, 2, 3, 4]].concat()
        );
        assert_eq!(
            record.subrecords[1].1,
            [0u32.to_le_bytes(), 1.25f32.to_le_bytes()].concat()
        );
        let counts = warnings.skipped_optional.borrow();
        assert_eq!(counts.len(), 1);
        let (count, example) = &counts["patch.esp"];
        assert_eq!(*count, 2);
        for expected in [
            "REFR",
            "source 01000800",
            "load-order 02000800",
            "XESP",
            "02000900",
            "invalid master index",
        ] {
            assert!(example.contains(expected), "{example}");
        }
    }

    /// Bad lengths and bad values share one plugin summary with one contextual example.
    #[test]
    fn malformed_optional_links_are_removed_and_share_value_warnings() {
        for (tag, length, expected) in [
            (b"XTEL", 28, "XTEL must be 32 bytes, found 28"),
            (b"XLRT", 6, "XLRT length 6 is not a multiple of 4"),
        ] {
            let indices = HashMap::from([("base.esm".into(), 0), ("patch.esp".into(), 2)]);
            let warnings = RemapWarnings::default();
            let mut record = RawRecord {
                form_id: 0x0100_0800,
                record_type: *b"PHZD",
                flags: 0,
                subrecords: vec![
                    (tag.to_vec(), vec![0xAB; length]),
                    (
                        b"XESP".to_vec(),
                        [0x0200_0900u32.to_le_bytes(), [1, 2, 3, 4]].concat(),
                    ),
                    (b"XEMI".to_vec(), 0x0100_0901u32.to_le_bytes().to_vec()),
                ],
                cell_form_id: None,
                worldspace_form_id: None,
                load_order: 0,
            };
            remap_record_form_ids(
                &mut record,
                "patch.esp",
                &["base.esm".into()],
                &indices,
                &HashMap::new(),
                &warnings,
            )
            .unwrap();
            assert_eq!(
                record.subrecords,
                [
                    (
                        b"XESP".to_vec(),
                        [0u32.to_le_bytes(), [1, 2, 3, 4]].concat()
                    ),
                    (b"XEMI".to_vec(), 0x0200_0901u32.to_le_bytes().to_vec()),
                ]
            );
            let counts = warnings.skipped_optional.borrow();
            assert_eq!(counts.len(), 1);
            let (count, example) = &counts["patch.esp"];
            assert_eq!(*count, 2);
            assert_eq!(
                example,
                &format!("PHZD record source 01000800 (load-order 02000800) {expected}")
            );
            assert!(warnings.out_of_range.borrow().is_empty());
            assert!(warnings.truncations.borrow().is_empty());
        }
    }

    /// The body chain is read as load-order FormIDs, so a patch's own `01` index must become
    /// its slot. Each `MODL` is its own occurrence: a malformed one is dropped alone and an
    /// out-of-range one is nulled, while the valid siblings keep their order.
    #[test]
    fn remaps_actor_body_chain_links_from_the_plugin_slot() {
        let indices = HashMap::from([("skyrim.esm".into(), 0), ("patch.esp".into(), 2)]);
        let warnings = RemapWarnings::default();
        let masters = ["skyrim.esm".to_owned()];
        let record = |form_id, record_type: &[u8; 4], subrecords| RawRecord {
            form_id,
            record_type: *record_type,
            flags: 0,
            subrecords,
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        let mut records = vec![
            record(
                0x0100_0200,
                b"RACE",
                vec![(b"WNAM".to_vec(), 0x0100_0300u32.to_le_bytes().to_vec())],
            ),
            record(
                0x0100_0300,
                b"ARMO",
                vec![
                    (b"EDID".to_vec(), b"Skin\0".to_vec()),
                    (b"MODL".to_vec(), 0x0100_0400u32.to_le_bytes().to_vec()),
                    (b"MODL".to_vec(), vec![0xAB; 3]),
                    (b"MODL".to_vec(), 0x0500_0401u32.to_le_bytes().to_vec()),
                ],
            ),
            record(
                0x0100_0400,
                b"ARMA",
                vec![
                    (b"RNAM".to_vec(), 0x0100_0200u32.to_le_bytes().to_vec()),
                    (b"MODL".to_vec(), 0x0100_0200u32.to_le_bytes().to_vec()),
                    (b"MODL".to_vec(), vec![0xAB; 3]),
                    (b"MODL".to_vec(), 0x0100_0201u32.to_le_bytes().to_vec()),
                ],
            ),
            record(
                0x0100_0401,
                b"ARMA",
                vec![(b"RNAM".to_vec(), 0x0000_0019u32.to_le_bytes().to_vec())],
            ),
            record(
                0x0100_0500,
                b"NPC_",
                vec![
                    (b"WNAM".to_vec(), 0x0100_0300u32.to_le_bytes().to_vec()),
                    (b"WNAM".to_vec(), vec![0xAB; 3]),
                    (b"TPLT".to_vec(), 0x0100_0501u32.to_le_bytes().to_vec()),
                    (b"TPLT".to_vec(), vec![0xAB; 3]),
                ],
            ),
        ];
        for record in &mut records {
            remap_record_form_ids(
                record,
                "patch.esp",
                &masters,
                &indices,
                &HashMap::new(),
                &warnings,
            )
            .unwrap();
        }

        assert_eq!(
            records[0].subrecords[0].1,
            0x0200_0300u32.to_le_bytes().to_vec()
        );
        assert_eq!(
            records[1].subrecords,
            [
                (b"EDID".to_vec(), b"Skin\0".to_vec()),
                (b"MODL".to_vec(), 0x0200_0400u32.to_le_bytes().to_vec()),
                (b"MODL".to_vec(), 0u32.to_le_bytes().to_vec()),
            ]
        );
        assert_eq!(
            records[2].subrecords[0].1,
            0x0200_0200u32.to_le_bytes().to_vec()
        );
        assert_eq!(
            records[2].subrecords,
            [
                (b"RNAM".to_vec(), 0x0200_0200u32.to_le_bytes().to_vec()),
                (b"MODL".to_vec(), 0x0200_0200u32.to_le_bytes().to_vec()),
                (b"MODL".to_vec(), 0x0200_0201u32.to_le_bytes().to_vec()),
            ]
        );
        // Index 00 is the master's own DefaultRace, which the catalog compares as `0x19`.
        assert_eq!(
            records[3].subrecords[0].1,
            0x0000_0019u32.to_le_bytes().to_vec()
        );
        assert_eq!(
            records[4].subrecords,
            [
                (b"WNAM".to_vec(), 0x0200_0300u32.to_le_bytes().to_vec()),
                (b"TPLT".to_vec(), 0x0200_0501u32.to_le_bytes().to_vec()),
            ]
        );

        let counts = warnings.skipped_optional.borrow();
        let (count, example) = &counts["patch.esp"];
        assert_eq!(*count, 5);
        assert_eq!(
            example,
            "ARMO record source 01000300 (load-order 02000300) MODL must be 4 bytes, found 3"
        );
    }

    /// Each `RDWT` entry names a weather and, optionally, a global; its chance is not a FormID.
    #[test]
    fn remaps_region_weathers_and_their_globals() {
        let indices = HashMap::from([("base.esm".into(), 0), ("patch.esp".into(), 2)]);
        let entry = |weather: u32, chance: u32, global: u32| {
            [
                weather.to_le_bytes(),
                chance.to_le_bytes(),
                global.to_le_bytes(),
            ]
            .concat()
        };
        let mut record = RawRecord {
            form_id: 0x0100_0800,
            record_type: *b"REGN",
            flags: 0,
            subrecords: vec![(
                b"RDWT".to_vec(),
                [
                    entry(0x0000_0901, 60, 0),
                    entry(0x0100_0902, 40, 0x0100_0903),
                ]
                .concat(),
            )],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        remap_record_form_ids(
            &mut record,
            "patch.esp",
            &["base.esm".into()],
            &indices,
            &HashMap::new(),
            &RemapWarnings::default(),
        )
        .unwrap();
        assert_eq!(
            record.subrecords[0].1,
            [
                entry(0x0000_0901, 60, 0),
                entry(0x0200_0902, 40, 0x0200_0903)
            ]
            .concat()
        );
    }

    #[test]
    fn remaps_all_race_base_movement_links_for_normal_and_light_plugins() {
        for light in [false, true] {
            for global_index in [3, 7] {
                let normal = HashMap::from([
                    ("skyrim.esm".to_owned(), 0),
                    ("movement.esp".to_owned(), global_index),
                ]);
                let lights = if light {
                    HashMap::from([("movement.esp".to_owned(), global_index)])
                } else {
                    HashMap::new()
                };
                let tags = [b"WKMV", b"RNMV", b"SWMV", b"FLMV", b"SNMV", b"SPMV"];
                // The patch owns each referenced MOVT at local index 1.
                // Also retain a null link and unrelated authored bytes.
                let mut subrecords: Vec<_> = tags
                    .iter()
                    .enumerate()
                    .map(|(i, tag)| {
                        (
                            tag.to_vec(),
                            (0x0100_0340u32 + i as u32).to_le_bytes().to_vec(),
                        )
                    })
                    .collect();
                subrecords.push((b"SPMV".to_vec(), 0u32.to_le_bytes().to_vec()));
                subrecords.push((b"DATA".to_vec(), vec![1, 2, 3, 4]));
                let mut race = RawRecord {
                    form_id: 0x0001_3746,
                    record_type: *b"RACE",
                    flags: 0,
                    subrecords,
                    cell_form_id: None,
                    worldspace_form_id: None,
                    load_order: global_index,
                };
                remap_record_form_ids(
                    &mut race,
                    "movement.esp",
                    &["skyrim.esm".to_owned()],
                    &normal,
                    &lights,
                    &RemapWarnings::default(),
                )
                .unwrap();
                assert_eq!(race.form_id, 0x0001_3746);
                for (i, (_, bytes)) in race.subrecords[..6].iter().enumerate() {
                    let expected = if light {
                        0xFE00_0000 | (global_index << 12) | (0x340 + i as u32)
                    } else {
                        (global_index << 24) | (0x340 + i as u32)
                    };
                    assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), expected);
                }
                assert_eq!(race.subrecords[6].1, 0u32.to_le_bytes());
                assert_eq!(race.subrecords[7].1, [1, 2, 3, 4]);
                // Exercise the actual stored raw-record path, not just the
                // classifier. Future resolvers must see canonical identities.
                let conn = rusqlite::Connection::open_in_memory().unwrap();
                exporter::create_tables(&conn).unwrap();
                exporter::export_to_db(&conn, &HashMap::from([(race.form_id, race.clone())]))
                    .unwrap();
                let (blob, source): (Vec<u8>, u32) = conn
                    .query_row(
                        "SELECT data, load_order FROM records WHERE form_id=?1",
                        [race.form_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap();
                let stored =
                    rkyv::from_bytes::<types::ArchivedRecordData, rkyv::rancor::Error>(&blob)
                        .unwrap();
                assert_eq!(source, global_index);
                for (stored, (tag, bytes)) in stored.subrecords.iter().zip(&race.subrecords) {
                    assert_eq!(stored.tag.as_slice(), tag);
                    assert_eq!(&stored.data, bytes);
                }
            }
        }
    }

    #[test]
    fn remaps_optional_race_movement_links() {
        let normal_indices = HashMap::from([
            ("skyrim.esm".to_string(), 0),
            ("movement.esp".to_string(), 3),
        ]);
        let mut race = RawRecord {
            form_id: 0x0101_3746,
            record_type: *b"RACE",
            flags: 0,
            subrecords: vec![(b"WKMV".to_vec(), 0x0100_1234u32.to_le_bytes().to_vec())],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        remap_record_form_ids(
            &mut race,
            "movement.esp",
            &["skyrim.esm".to_string()],
            &normal_indices,
            &HashMap::new(),
            &RemapWarnings::default(),
        )
        .unwrap();
        assert_eq!(race.form_id, 0x0301_3746);
        assert_eq!(
            u32::from_le_bytes(race.subrecords[0].1[..4].try_into().unwrap()),
            0x0300_1234
        );
    }

    fn default_objects(pairs: &[(&[u8; 4], u32)]) -> RawRecord {
        RawRecord {
            form_id: 0x0000_0031,
            record_type: *b"DOBJ",
            flags: 0,
            subrecords: vec![(
                b"DNAM".to_vec(),
                pairs
                    .iter()
                    .flat_map(|(key, id)| [key.as_slice(), &id.to_le_bytes()].concat())
                    .collect(),
            )],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        }
    }

    #[test]
    fn remaps_default_object_form_ids_but_not_keys() {
        let normal_indices = HashMap::from([
            ("skyrim.esm".to_string(), 0),
            ("update.esm".to_string(), 3),
            ("dawnguard.esm".to_string(), 5),
        ]);
        // Master 1 is update.esm (load order 3); index 2 is the plugin itself (5).
        let mut record = default_objects(&[
            (b"DMWL", 0x0003_580D),
            (b"DMRN", 0x0100_0800),
            (b"DMSW", 0x0200_0900),
        ]);
        remap_record_form_ids(
            &mut record,
            "dawnguard.esm",
            &["skyrim.esm".to_string(), "update.esm".to_string()],
            &normal_indices,
            &HashMap::new(),
            &RemapWarnings::default(),
        )
        .unwrap();
        assert_eq!(
            record.subrecords,
            default_objects(&[
                (b"DMWL", 0x0003_580D),
                (b"DMRN", 0x0300_0800),
                (b"DMSW", 0x0500_0900),
            ])
            .subrecords
        );
    }

    #[test]
    fn default_object_overrides_merge_by_key() {
        let base = default_objects(&[(b"DMWL", 0x0003_580D), (b"DMRN", 0x0003_580D)]);
        let mut patch = default_objects(&[(b"DMRN", 0x0200_0001), (b"DMSW", 0x0003_6D42)]);
        merge_default_objects(&base, &mut patch);
        assert_eq!(
            patch.subrecords,
            default_objects(&[
                (b"DMWL", 0x0003_580D),
                (b"DMRN", 0x0200_0001),
                (b"DMSW", 0x0003_6D42),
            ])
            .subrecords
        );
        // A record without a usable DNAM keeps the earlier defaults.
        let mut empty = default_objects(&[]);
        empty.subrecords.clear();
        merge_default_objects(&base, &mut empty);
        assert_eq!(empty.subrecords, base.subrecords);
    }

    #[test]
    fn zero_keys_are_not_default_objects() {
        let record = default_objects(&[(b"\0\0\0\0", 0), (b"\0\0\0\0", 5), (b"DMWL", 7)]);
        assert_eq!(default_object_pairs(&record.subrecords), [(*b"DMWL", 7)]);
    }

    #[test]
    fn merging_default_objects_keeps_every_subrecord_position() {
        let base = default_objects(&[(b"DMWL", 0x0003_580D)]);
        let mut patch = default_objects(&[(b"DMRN", 0x0200_0001)]);
        patch.subrecords.push((b"ZZZZ".to_vec(), vec![1, 2, 3]));
        patch
            .subrecords
            .insert(0, (b"EDID".to_vec(), b"Defaults\0".to_vec()));
        let before: Vec<Vec<u8>> = patch
            .subrecords
            .iter()
            .map(|(tag, _)| tag.clone())
            .collect();
        merge_default_objects(&base, &mut patch);
        let after: Vec<Vec<u8>> = patch
            .subrecords
            .iter()
            .map(|(tag, _)| tag.clone())
            .collect();
        assert_eq!(before, after, "no subrecord moved");
        assert_eq!(
            default_object_pairs(&patch.subrecords),
            [(*b"DMWL", 0x0003_580D), (*b"DMRN", 0x0200_0001),]
        );
    }

    #[test]
    fn remaps_xesp_parents_for_normal_and_light_plugins_without_changing_flags() {
        for light in [false, true] {
            let normal = HashMap::from([("skyrim.esm".to_owned(), 0), ("mod.esm".to_owned(), 5)]);
            let lights = if light {
                HashMap::from([("mod.esm".to_owned(), 2)])
            } else {
                HashMap::new()
            };
            let mut xesp = 0x0100_0345u32.to_le_bytes().to_vec();
            xesp.extend_from_slice(&1u32.to_le_bytes());
            let mut reference = RawRecord {
                form_id: 0x0100_1234,
                record_type: *b"REFR",
                flags: 0x800,
                subrecords: vec![(b"XESP".to_vec(), xesp)],
                cell_form_id: None,
                worldspace_form_id: None,
                load_order: 5,
            };
            remap_record_form_ids(
                &mut reference,
                "mod.esm",
                &["skyrim.esm".into()],
                &normal,
                &lights,
                &RemapWarnings::default(),
            )
            .unwrap();
            let parent = u32::from_le_bytes(reference.subrecords[0].1[..4].try_into().unwrap());
            assert_eq!(parent, if light { 0xFE00_2345 } else { 0x0500_0345 });
            assert_eq!(&reference.subrecords[0].1[4..], &1u32.to_le_bytes());
            assert_eq!(reference.flags, 0x800);
        }
    }

    #[test]
    fn does_not_corrupt_tes4_author_strings_or_clfm_colors() {
        let normal_indices = HashMap::from([("skyrim.esm".to_string(), 0)]);
        let light_indices = HashMap::new();

        let mut tes4 = RawRecord {
            form_id: 0,
            record_type: *b"TES4",
            flags: 0,
            subrecords: vec![
                (b"CNAM".to_vec(), b"Bethesda Game Studios\0".to_vec()),
                (b"SNAM".to_vec(), b"Master description\0".to_vec()),
            ],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        remap_record_form_ids(
            &mut tes4,
            "skyrim.esm",
            &[],
            &normal_indices,
            &light_indices,
            &RemapWarnings::default(),
        )
        .unwrap();
        assert_eq!(tes4.subrecords[0].1, b"Bethesda Game Studios\0");
        assert_eq!(tes4.subrecords[1].1, b"Master description\0");

        let mut clfm = RawRecord {
            form_id: 0x00012345,
            record_type: *b"CLFM",
            flags: 0,
            subrecords: vec![(b"CNAM".to_vec(), vec![128, 64, 32, 255])],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        };
        remap_record_form_ids(
            &mut clfm,
            "skyrim.esm",
            &[],
            &normal_indices,
            &light_indices,
            &RemapWarnings::default(),
        )
        .unwrap();
        assert_eq!(clfm.subrecords[0].1, vec![128, 64, 32, 255]);
    }

    #[test]
    fn remaps_actual_form_id_subrecords() {
        let normal_indices =
            HashMap::from([("skyrim.esm".to_string(), 0), ("update.esm".to_string(), 1)]);
        let light_indices = HashMap::new();

        let mut refr = RawRecord {
            form_id: 0x00000001,
            record_type: *b"REFR",
            flags: 0,
            subrecords: vec![
                (b"NAME".to_vec(), 0x00000020u32.to_le_bytes().to_vec()),
                (b"XOWN".to_vec(), 0x00000030u32.to_le_bytes().to_vec()),
            ],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 1,
        };
        remap_record_form_ids(
            &mut refr,
            "update.esm",
            &["skyrim.esm".to_string()],
            &normal_indices,
            &light_indices,
            &RemapWarnings::default(),
        )
        .unwrap();
        assert_eq!(
            u32::from_le_bytes(refr.subrecords[0].1[..4].try_into().unwrap()),
            0x00000020
        );
        assert_eq!(
            u32::from_le_bytes(refr.subrecords[1].1[..4].try_into().unwrap()),
            0x00000030
        );
    }
}
