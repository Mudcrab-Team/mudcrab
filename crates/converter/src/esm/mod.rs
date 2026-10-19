use crate::esm::{
    binary::parse_plugin_file_with_warnings,
    exporter::{create_tables, validate_inhouse_record},
    records::RawRecord,
};
use color_eyre::{Result, eyre::WrapErr};
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
pub mod emittance;
pub mod exporter;
pub mod extractors;
pub mod inhouse;
mod inhouse_script_properties;
mod inhouse_terrain_diagnostics;
pub mod load_order;
pub mod lodsettings;
pub mod mmap_reader;
pub mod plugins_txt;
pub mod records;
pub mod rooms;
pub mod types;
pub use plugins_txt::{
    OFFICIAL_MASTERS, read_plugins_txt, read_plugins_txt_with_options, read_skyrim_ccc,
};
#[cfg(test)]
mod emittance_parity_tests;
#[cfg(test)]
mod plugin_table_tests;
#[cfg(test)]
mod report_warnings_official_tests;

pub struct EsmParser;

/// Winning records with bounded recovery warnings and harmless compatibility notices.
#[derive(Debug)]
pub(crate) struct RecordConversion {
    pub(crate) records: HashMap<u32, RawRecord>,
    pub(crate) warnings: Vec<String>,
    pub(crate) notices: Vec<String>,
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
    #[cfg(test)]
    pub(crate) fn convert_plugins_with_reader(
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
            None,
        )
        .map(|conversion| conversion.records)
    }

    /// Publish with one immutable schema batch and retain summaries for the pipeline report.
    pub(crate) fn convert_plugins_with_reader_and_schemas(
        plugin_paths: &[PathBuf],
        db_path: &Path,
        reader: crate::config::RecordReader,
        strings_root: &Path,
        schemas: Option<&crate::records::addons::RuntimeSchemas>,
    ) -> Result<RecordConversion> {
        Self::convert_plugins_with_reader_and_schemas_with_string_sources(
            plugin_paths,
            db_path,
            reader,
            inhouse::StringSources::PluginLooseFirst(strings_root),
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
                Self::convert_plugins_with_records_report(plugin_paths, db_path)
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
        let order = load_order::LoadOrder::read(plugin_paths)?;
        let master = Self::merge_plugins_with_load_order(plugin_paths, &order)?;
        let conn = Connection::open(db_path)?;
        create_tables(&conn)?;
        exporter::export_plugins_to_db(&conn, &master.records, &order, plugin_paths)?;

        Ok(master)
    }

    /// Merge plugin records with validated slots and EditorID-based game settings.
    pub fn merge_plugins(plugin_paths: &[PathBuf]) -> Result<HashMap<u32, RawRecord>> {
        let order = load_order::LoadOrder::read(plugin_paths)?;
        Self::merge_plugins_with_load_order(plugin_paths, &order)
            .map(|conversion| conversion.records)
    }

    /// Use one load-order mapping for merge/export and retain all printed reader summaries.
    fn merge_plugins_with_load_order(
        plugin_paths: &[PathBuf],
        order: &load_order::LoadOrder,
    ) -> Result<RecordConversion> {
        let mut merged = HashMap::new();
        let mut summaries = Vec::new();
        // Unlike ordinary forms, game settings override by EditorID. Retain
        // the first definition's key/owner while taking the last setting value.
        // Keep keys through deletions so a later restoration has the same ID.
        let mut game_settings = GameSettingIdentities::default();
        let warnings = RemapWarnings::default();
        for (priority, path) in plugin_paths.iter().enumerate() {
            let masters = &order.metadata[priority].masters;
            // Final ID -> (source ID, whether its master index was out of range). Two different
            // source IDs that resolve to one final ID would silently replace each other.
            let mut sources: HashMap<u32, (u32, bool)> = HashMap::new();
            let (records, parser_summaries) = parse_plugin_file_with_warnings(path)?;
            summaries.extend(parser_summaries);
            for mut record in records {
                record.load_order = priority as u32;
                let source_id = record.form_id;
                let out_of_range = (source_id >> 24) as usize > masters.len();
                let admission = remap_record_form_ids(
                    &mut record,
                    &order.names[priority],
                    masters,
                    &order.normal,
                    &order.light,
                    &warnings,
                )
                .and_then(|()| {
                    if !record.is_deleted() {
                        validate_inhouse_record(&record)?;
                        if record.record_type == *b"LAND" {
                            cell_cache::validate_inhouse_land(&record)?;
                        }
                    }
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
                let remapped_source_id = record.form_id;
                if let Some((earlier, earlier_out_of_range)) = sources.get(&record.form_id) {
                    // Light-ID truncation keeps its documented last-record-wins behaviour; only
                    // an out-of-range master index is rejected when it lands on another record.
                    if *earlier != source_id && (out_of_range || *earlier_out_of_range) {
                        warnings.skipped_record(
                            &order.names[priority],
                            &record,
                            source_id,
                            format!("records {earlier:08X} and {source_id:08X} both resolve to {:08X}; an out-of-range master index would replace another record", record.form_id),
                        );
                        continue;
                    }
                }
                if record.record_type == *b"GMST" {
                    let canonical =
                        match game_settings.resolve(&record, &order.names[priority], &merged) {
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
                    record.form_id = canonical;
                } else if game_settings.aliases.contains_key(&record.form_id) {
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
                sources.insert(remapped_source_id, (source_id, out_of_range));
                if record.is_deleted() {
                    merged.remove(&record.form_id);
                } else {
                    merged.insert(record.form_id, record);
                }
            }
        }
        let (remap_warnings, notices) = warnings.report();
        summaries.extend(remap_warnings);
        Ok(RecordConversion {
            records: merged,
            warnings: summaries,
            notices,
        })
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
    canonical_ids: HashMap<String, u32>,
    aliases: HashMap<u32, u32>,
}

impl GameSettingIdentities {
    /// Resolve a setting's persistent ID without silently deleting unrelated forms.
    fn resolve(
        &mut self,
        record: &RawRecord,
        plugin: &str,
        merged: &HashMap<u32, RawRecord>,
    ) -> Result<Option<u32>> {
        let source_id = record.form_id;
        let editor_id = extractors::SubrecordView::new(&record.subrecords)
            .get_string(b"EDID")
            .filter(|name| !name.is_empty())
            .map(|name| name.to_ascii_lowercase());
        let Some(editor_id) = editor_id else {
            if record.is_deleted() {
                if let Some(&canonical) = self.aliases.get(&source_id) {
                    return Ok(Some(canonical));
                }
                return Ok(None);
            }
            color_eyre::eyre::bail!("{plugin} GMST {source_id:08X} has no EditorID");
        };
        let known = self.canonical_ids.get(&editor_id).copied();
        let canonical = known.unwrap_or(source_id);
        color_eyre::eyre::ensure!(
            canonical != 0,
            "GMST {editor_id} has a null FormID; needs an EditorID-keyed database representation"
        );
        if source_id != 0 {
            color_eyre::eyre::ensure!(
                self.aliases
                    .get(&source_id)
                    .is_none_or(|id| known == Some(*id))
                    && merged
                        .get(&source_id)
                        .is_none_or(|previous| previous.record_type == *b"GMST"),
                "GMST {editor_id} collides with another record at {source_id:08X}"
            );
            self.aliases.insert(source_id, canonical);
        }
        self.canonical_ids.insert(editor_id, canonical);
        Ok(Some(canonical))
    }
}

/// Required placed base links cannot use the optional field's null/drop policy.
fn is_required_form_id_subrecord(record_type: &[u8; 4], tag: &[u8]) -> bool {
    tag == b"NAME"
        && matches!(
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

/// Field layouts per xEdit's TES5 definitions
/// (https://github.com/TES5Edit/TES5Edit/blob/dev-4.1.5/Core/wbDefinitionsTES5.pas).
fn form_id_layout(record_type: &[u8; 4], tag: &[u8]) -> Option<FormIdLayout> {
    use FormIdLayout::{Array, Fixed};
    if matches!(record_type, b"TES4" | b"CLFM" | b"AACT") {
        return None;
    }
    let tag: &[u8; 4] = tag.try_into().ok()?;
    let placed = matches!(
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
    );
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
        b"NAM2" | b"NAM3" | b"WNAM" | b"CNAM" if record_type == b"WRLD" => Fixed(SINGLE),
        b"RNAM" | b"CNAM" | b"INAM" if record_type == b"NPC_" => Fixed(SINGLE),
        b"WKMV" | b"RNMV" if record_type == b"RACE" => Fixed(SINGLE),
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
        _ => return None,
    })
}

/// Remaps local FormIDs within a record header, parent cell/worldspace references,
/// and relevant subrecords according to master plugin load-order indices.
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
    let remap = |form_id| remap_with_validation(form_id, strict);
    let remap_link = |form_id| remap_with_validation(form_id, true);
    let source_id = record.form_id;
    record.form_id = remap(record.form_id)?;
    if record.is_deleted() {
        // Only the resolved header identity (and a complete GMST EDID) is needed
        // for deletion. Unused parent/body links cannot invalidate a tombstone.
        record.cell_form_id = None;
        record.worldspace_form_id = None;
        return Ok(());
    }
    record.cell_form_id = record.cell_form_id.map(&remap_link).transpose()?;
    record.worldspace_form_id = record.worldspace_form_id.map(&remap_link).transpose()?;
    let mut malformed_subrecords = Vec::new();
    for (index, (tag, data)) in record.subrecords.iter_mut().enumerate() {
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
            color_eyre::eyre::ensure!(data.len() == 8, "invalid LAND texture reference length");
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
            // Primary script objects retain the record kind's established legacy
            // ownership compatibility. Malformed framing still rejects the candidate.
            records::record_type::vmad::remap_primary_form_ids(data, &remap)
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
                let resolved = match remap_with_validation(value, true) {
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
