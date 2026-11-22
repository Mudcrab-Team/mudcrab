use crate::esm::{
    binary::{ParsedRecord, parse_plugin_file_with_warnings},
    exporter::validate_inhouse_record,
    records::RawRecord,
};
use crate::records::DiscardedFrameScope;
use color_eyre::{Result, eyre::WrapErr};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
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
mod gameplay_common;
pub mod grass_cache;
pub mod inhouse;
mod inhouse_script_properties;
mod inhouse_terrain_diagnostics;
mod legacy_text;
pub mod load_order;
pub mod lodsettings;
pub mod mmap_reader;
mod object_materials;
mod perks;
pub mod placement_edits;
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
mod perks_parity_tests;
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

struct LegacyPluginMerge {
    deleted_ids: HashSet<u32>,
    conversion: RecordConversion,
    damaged_cells: HashSet<u32>,
    room_link_layouts: HashMap<u32, rooms::RoomLinkLayout>,
    perk_rejections: HashMap<u32, perks::PerkRejectedSections>,
}

struct LegacyPlacementCandidate {
    source_id: u32,
    source_cell: Option<u32>,
    predecessor: Option<Box<LegacyPlacementPredecessor>>,
}

struct LegacyPlacementPredecessor {
    previous: Option<RawRecord>,
    previous_layout: Option<rooms::RoomLinkLayout>,
    previous_damage: Option<HashSet<Option<u32>>>,
    previous_candidate: Option<LegacyPlacementCandidate>,
}

/// A rejected override may name a different cell from its retained predecessor.
/// A later usable record or deletion supersedes damage with a proven identity.
#[derive(Default)]
struct PlacementDamage {
    cells_by_reference: HashMap<Option<u32>, HashSet<Option<u32>>>,
    unknown_scope: bool,
}

impl PlacementDamage {
    fn reject(
        &mut self,
        record_type: &[u8; 4],
        source_id: u32,
        source_cell: Option<u32>,
        plugin: usize,
        order: &load_order::LoadOrder,
    ) {
        use crate::records::source_identity::header_runtime_id;

        if !is_placed_record_type(record_type) {
            return;
        }
        self.reject_resolved(
            header_runtime_id(record_type, source_id, plugin, order),
            source_cell,
            plugin,
            order,
        );
    }

    fn reject_resolved(
        &mut self,
        id: Option<u32>,
        source_cell: Option<u32>,
        plugin: usize,
        order: &load_order::LoadOrder,
    ) {
        use crate::records::source_identity::header_runtime_id;

        let id = id.filter(|&id| id != 0);
        let cell = source_cell
            .and_then(|raw| header_runtime_id(b"CELL", raw, plugin, order))
            .filter(|&cell| cell != 0);
        if id.is_none() && cell.is_none() {
            self.unknown_scope = true;
        } else {
            let cells = self.cells_by_reference.entry(id).or_default();
            if source_cell.is_some() {
                cells.insert(cell);
            }
        }
    }

    fn accepted(&mut self, id: u32) {
        self.cells_by_reference.remove(&Some(id));
    }

    fn affected_cells(self, winners: &HashMap<u32, RawRecord>) -> HashSet<u32> {
        let all_cells = || {
            winners
                .values()
                .filter(|record| record.record_type == *b"CELL")
                .map(|record| record.form_id)
                .collect()
        };
        if self.unknown_scope {
            return all_cells();
        }
        let mut cells = HashSet::new();
        for (id, rejected) in self.cells_by_reference {
            let earlier_cell = id
                .and_then(|id| winners.get(&id))
                .filter(|record| is_placed_record_type(&record.record_type))
                .and_then(|record| record.cell_form_id);
            if earlier_cell.is_none() && rejected.contains(&None) {
                return all_cells();
            }
            cells.extend(rejected.into_iter().flatten());
            cells.extend(earlier_cell);
        }
        cells
    }
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
                Self::convert_legacy_plugins_report(plugin_paths, db_path, string_sources)
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
        // Without a staged archive VFS, string tables come from each plugin's loose folder.
        let loose = plugin_paths
            .first()
            .and_then(|path| path.parent())
            .unwrap_or(Path::new("."));
        Self::convert_legacy_plugins_report(
            plugin_paths,
            db_path,
            inhouse::StringSources::default_language(inhouse::StringRoots::PluginLooseFirst(loose)),
        )
    }

    /// Export legacy records, resolving localized text through the in-house string sources.
    fn convert_legacy_plugins_report(
        plugin_paths: &[PathBuf],
        db_path: &Path,
        string_sources: inhouse::StringSources<'_>,
    ) -> Result<RecordConversion> {
        let order = load_order::LoadOrder::read(plugin_paths)?;
        let mut merged = Self::merge_plugins_with_load_order(plugin_paths, &order)?;
        let (unresolved, localization_warnings, localization_notices) =
            legacy_text::resolve_localized_fields(
                &mut merged.conversion.records,
                plugin_paths,
                &order,
                string_sources,
                &mut merged.perk_rejections,
            );
        let conn = Connection::open(db_path)?;
        exporter::export_plugins_to_db(&conn, &merged, &order, plugin_paths, &unresolved)?;

        for diagnostic in localization_warnings.iter().chain(&localization_notices) {
            eprintln!("{diagnostic}");
        }
        let mut master = merged.conversion;
        master.warnings.extend(localization_warnings);
        master.notices.extend(localization_notices);
        Ok(master)
    }

    /// Merge plugin records with validated slots and EditorID-based game settings.
    pub fn merge_plugins(plugin_paths: &[PathBuf]) -> Result<HashMap<u32, RawRecord>> {
        let order = load_order::LoadOrder::read(plugin_paths)?;
        Self::merge_plugins_with_load_order(plugin_paths, &order)
            .map(|merged| merged.conversion.records)
    }

    /// Use one load-order mapping for merge/export and retain all printed reader summaries.
    fn merge_plugins_with_load_order(
        plugin_paths: &[PathBuf],
        order: &load_order::LoadOrder,
    ) -> Result<LegacyPluginMerge> {
        let mut merged = HashMap::new();
        let mut perk_rejections = HashMap::new();
        let mut damaged_placements = PlacementDamage::default();
        let mut damaged_cells = HashSet::new();
        let mut unknown_discarded_scope = false;
        let mut room_link_layouts = HashMap::new();
        let mut placement_candidates = BTreeMap::new();
        let mut deleted_ids = crate::records::DeletedBases::default();
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
            for candidate in records {
                let candidate = match candidate {
                    ParsedRecord::DamagedRecord(record) if record.record_type == *b"CELL" => {
                        ParsedRecord::DiscardedFrame(DiscardedFrameScope::Cell(record.form_id))
                    }
                    candidate => candidate,
                };
                let (mut record, damaged) = match candidate {
                    ParsedRecord::Usable(record) => (record, false),
                    ParsedRecord::DamagedRecord(record)
                        if is_placed_record_type(&record.record_type) =>
                    {
                        (record, true)
                    }
                    ParsedRecord::DamagedRecord(_) => continue,
                    ParsedRecord::TruncatedPlacement(placement) => {
                        if let Some(id) = placement.runtime_id(priority, order) {
                            damaged_placements
                                .cells_by_reference
                                .entry(Some(id))
                                .or_default();
                        } else {
                            unknown_discarded_scope = true;
                        }
                        continue;
                    }
                    ParsedRecord::DiscardedFrame(scope) => {
                        match scope.resolve(priority, order) {
                            DiscardedFrameScope::Cell(cell) => {
                                damaged_cells.insert(cell);
                            }
                            DiscardedFrameScope::Unknown => unknown_discarded_scope = true,
                        }
                        continue;
                    }
                };
                record.load_order = priority as u32;
                let source_id = record.form_id;
                let out_of_range = (source_id >> 24) as usize > masters.len();
                let mut rejected = None;
                // Optional-field remapping can remove a field between XLRMs. Capture the
                // source positions before that can make a noncontiguous run look contiguous.
                let room_link_layout = (!damaged
                    && !record.is_deleted()
                    && record.record_type == *b"REFR"
                    && record.cell_form_id.is_some()
                    && extractors::SubrecordView::new(&record.subrecords)
                        .unique_form_id(b"NAME")
                        .is_some_and(|base| base & 0x00ff_ffff == rooms::ROOM_MARKER_BASE))
                .then(|| rooms::RoomLinkLayout::from_raw(&record));
                let source_cell = record.cell_form_id;
                let record_type = record.record_type;
                let admission = remap_record_form_ids(
                    &mut record,
                    &order.names[priority],
                    masters,
                    &order.normal,
                    &order.light,
                    &warnings,
                )
                .and_then(|perk_rejected| {
                    rejected = perk_rejected;
                    if !damaged && !record.is_deleted() {
                        if is_placed_record_type(&record_type) {
                            validate_placement_parent(&record_type, record.cell_form_id, || None)
                                .map_err(color_eyre::eyre::Report::msg)?;
                            let view = extractors::SubrecordView::new(&record.subrecords);
                            let valid_transform = if merged.contains_key(&record.form_id) {
                                valid_placement_transform(view.find_all(b"DATA"))
                            } else {
                                valid_initial_placement_transform(view.find(b"DATA"))
                            };
                            color_eyre::eyre::ensure!(
                                valid_transform,
                                "invalid placement transform"
                            );
                            color_eyre::eyre::ensure!(
                                view.unique_form_id(b"NAME").is_some_and(|id| id != 0),
                                "placement requires exactly one nonzero NAME reference"
                            );
                        }
                        validate_inhouse_record(&record)?;
                        if record.record_type == *b"LAND" {
                            cell_cache::validate_inhouse_land(&record)?;
                        }
                    }
                    Ok(())
                });
                let resolved = (!out_of_range).then_some(record.form_id);
                let reject_placement = |damage: &mut PlacementDamage| {
                    if is_placed_record_type(&record_type) {
                        damage.reject_resolved(resolved, source_cell, priority, order);
                    }
                };
                if let Err(error) = admission {
                    reject_placement(&mut damaged_placements);
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
                        reject_placement(&mut damaged_placements);
                        warnings.skipped_record(
                            &order.names[priority],
                            &record,
                            source_id,
                            format!("records {earlier:08X} and {source_id:08X} both resolve to {:08X}; an out-of-range master index would replace another record", record.form_id),
                        );
                        continue;
                    }
                }
                if damaged {
                    reject_placement(&mut damaged_placements);
                    continue;
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
                    reject_placement(&mut damaged_placements);
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
                let id = record.form_id;
                let previous_layout = room_link_layouts.remove(&id);
                if let Some(layout) = room_link_layout {
                    room_link_layouts.insert(id, layout);
                }
                if record.is_deleted() {
                    let prior_kind = settle_legacy_predecessor(
                        id,
                        placement_candidates.get(&id),
                        &merged,
                        &mut deleted_ids,
                    );
                    deleted_ids.record(id, record.load_order, record.record_type, prior_kind);
                    perk_rejections.remove(&id);
                    merged.remove(&id);
                    placement_candidates.remove(&id);
                    damaged_placements.accepted(id);
                } else {
                    if let Some(rejected) = rejected {
                        perk_rejections.insert(id, rejected);
                    } else {
                        perk_rejections.remove(&id);
                    }
                    if record.record_type == *b"DOBJ"
                        && let Some(previous) = merged.get(&id)
                    {
                        merge_default_objects(previous, &mut record);
                    }
                    let placed = is_placed_record_type(&record.record_type);
                    if !placed {
                        // An earlier placement can close the interval once its targets are valid.
                        if deleted_ids.needs_restoration(id)
                            && placement_candidates.contains_key(&id)
                        {
                            settle_legacy_predecessor(
                                id,
                                placement_candidates.get(&id),
                                &merged,
                                &mut deleted_ids,
                            );
                        }
                        deleted_ids.restored(id, record.load_order);
                    }
                    let previous = merged.insert(id, record);
                    if placed {
                        let previous_candidate = placement_candidates.remove(&id);
                        let previous_damage =
                            damaged_placements.cells_by_reference.remove(&Some(id));
                        let predecessor =
                            (previous.is_some() || previous_damage.is_some()).then(|| {
                                Box::new(LegacyPlacementPredecessor {
                                    previous,
                                    previous_layout,
                                    previous_damage,
                                    previous_candidate,
                                })
                            });
                        placement_candidates.insert(
                            id,
                            LegacyPlacementCandidate {
                                source_id,
                                source_cell,
                                predecessor,
                            },
                        );
                    } else {
                        placement_candidates.remove(&id);
                        damaged_placements.accepted(id);
                    }
                }
            }
        }
        resolve_legacy_placements(
            &mut merged,
            &mut placement_candidates,
            &mut room_link_layouts,
            &mut damaged_placements,
            &deleted_ids,
            order,
            &warnings,
        );
        sanitize_legacy_perk_condition_links(&mut merged, order, &warnings);
        let (remap_warnings, notices) = warnings.report();
        summaries.extend(remap_warnings);
        damaged_placements.unknown_scope |= unknown_discarded_scope;
        damaged_cells.extend(damaged_placements.affected_cells(&merged));
        Ok(LegacyPluginMerge {
            deleted_ids: deleted_ids.absent_from(&merged),
            conversion: RecordConversion {
                records: merged,
                warnings: summaries,
                notices,
                phases: BTreeMap::new(),
            },
            damaged_cells,
            room_link_layouts,
            perk_rejections,
        })
    }
}

fn sanitize_legacy_perk_condition_links(
    merged: &mut HashMap<u32, RawRecord>,
    order: &load_order::LoadOrder,
    warnings: &RemapWarnings,
) {
    let mut perk_ids: Vec<u32> = merged
        .iter()
        .filter(|(_, record)| record.record_type == *b"PERK" && !record.is_deleted())
        .map(|(&id, _)| id)
        .collect();
    perk_ids.sort_unstable();
    let mut invalid_offsets: Vec<(u32, usize, usize)> = Vec::new();
    {
        let links = gameplay_common::Links { master: merged };
        for &perk_id in &perk_ids {
            let Some(record) = merged.get(&perk_id) else {
                continue;
            };
            let plugin = order
                .names
                .get(record.load_order as usize)
                .map(String::as_str)
                .unwrap_or("<unknown>");
            for (sub_index, (tag, data)) in record.subrecords.iter().enumerate() {
                if tag.as_slice() != b"CTDA" || data.len() != 32 {
                    continue;
                }
                for (offset, slot, targets) in gameplay_common::condition_link_slots(data) {
                    let raw = u32::from_le_bytes([
                        data[offset],
                        data[offset + 1],
                        data[offset + 2],
                        data[offset + 3],
                    ]);
                    if let Some(reason) = links.mismatch_reason(raw, targets) {
                        warnings.skipped_optional(
                            plugin,
                            format!("PERK {perk_id:08X} CTDA {slot} target {raw:08X} {reason}"),
                        );
                        invalid_offsets.push((perk_id, sub_index, offset));
                    }
                }
            }
        }
    }
    for (perk_id, sub_index, offset) in invalid_offsets {
        if let Some(record) = merged.get_mut(&perk_id)
            && let Some((_, data)) = record.subrecords.get_mut(sub_index)
            && data.len() >= offset + 4
        {
            data[offset..offset + 4].fill(0);
        }
    }
}

fn settle_legacy_predecessor(
    id: u32,
    candidate: Option<&LegacyPlacementCandidate>,
    winners: &HashMap<u32, RawRecord>,
    deleted: &mut crate::records::DeletedBases,
) -> Option<[u8; 4]> {
    let latest = winners.get(&id)?;
    let versions = std::iter::successors(Some((latest, candidate)), |&(_, candidate)| {
        let predecessor = candidate?.predecessor.as_ref()?;
        Some((
            predecessor.previous.as_ref()?,
            predecessor.previous_candidate.as_ref(),
        ))
    });
    let mut accepted = versions.filter(|&(record, candidate)| {
        candidate.is_none_or(|placement| {
            let result = crate::records::validate_placement_targets(
                None,
                &record.record_type,
                record.load_order,
                &legacy_dependencies(record, placement),
                None,
                |target| winners.get(&target).map(|record| record.record_type),
                deleted,
            );
            matches!(
                result,
                Ok(()) | Err(crate::records::PlacementFailure::RetiredBase(_))
            )
        })
    });
    let (latest, _) = accepted.next()?;
    if deleted.needs_restoration(id) {
        let earliest = accepted.last().map_or(latest, |(record, _)| record);
        deleted.restored(id, earliest.load_order);
    }
    Some(latest.record_type)
}

fn legacy_dependencies(
    record: &RawRecord,
    candidate: &LegacyPlacementCandidate,
) -> PlacementDependencies {
    PlacementDependencies {
        own: legacy_placement_links(record),
        grouped: legacy_grouped_predecessors(record.cell_form_id, candidate)
            .map(|prior| (prior.record_type, legacy_placement_links(prior)))
            .collect(),
    }
}

/// Restore the record, link layout and damage together when late admission fails.
fn resolve_legacy_placements(
    winners: &mut HashMap<u32, RawRecord>,
    candidates: &mut BTreeMap<u32, LegacyPlacementCandidate>,
    layouts: &mut HashMap<u32, rooms::RoomLinkLayout>,
    damage: &mut PlacementDamage,
    deleted_ids: &crate::records::DeletedBases,
    order: &load_order::LoadOrder,
    warnings: &RemapWarnings,
) {
    loop {
        let rejected = invalid_placement_candidates(
            candidates.keys().copied(),
            |id| candidates[&id].predecessor.is_some(),
            |id| {
                legacy_predecessors(&candidates[&id])
                    .any(|record| !is_placed_record_type(&record.record_type))
            },
            |id| {
                candidates
                    .get(&id)
                    .map(|candidate| legacy_dependencies(&winners[&id], candidate))
            },
            |id, dependencies, cycle, absent| {
                crate::records::validate_placement_targets(
                    None,
                    &winners[&id].record_type,
                    winners[&id].load_order,
                    dependencies,
                    cycle,
                    |target| {
                        winners
                            .get(&target)
                            .filter(|_| !absent.contains(&target))
                            .map(|record| record.record_type)
                    },
                    deleted_ids,
                )
            },
        );
        if rejected.is_empty() {
            break;
        }
        for (id, reason) in rejected {
            let Some(candidate) = candidates.remove(&id) else {
                continue;
            };
            let record = &winners[&id];
            let priority = record.load_order as usize;
            match reason {
                crate::records::PlacementFailure::Invalid(reason) => {
                    let source_slot = (candidate.source_id >> 24) as usize;
                    let resolved =
                        (source_slot <= order.metadata[priority].masters.len()).then_some(id);
                    damage.reject_resolved(resolved, candidate.source_cell, priority, order);
                    warnings.skipped_record(
                        &order.names[priority],
                        record,
                        candidate.source_id,
                        reason,
                    );
                }
                crate::records::PlacementFailure::RetiredBase(base) => {
                    let mut counts = warnings.retired_base_placements.borrow_mut();
                    let entry = counts
                        .entry(order.names[priority].clone())
                        .or_insert((0, id, base));
                    entry.0 += 1;
                }
            }
            winners.remove(&id);
            layouts.remove(&id);
            if let Some(predecessor) = candidate.predecessor {
                if let Some(prior_damage) = predecessor.previous_damage {
                    damage
                        .cells_by_reference
                        .entry(Some(id))
                        .or_default()
                        .extend(prior_damage);
                }
                if let Some(layout) = predecessor.previous_layout {
                    layouts.insert(id, layout);
                }
                if let Some(prior_candidate) = predecessor.previous_candidate {
                    candidates.insert(id, prior_candidate);
                }
                if let Some(previous) = predecessor.previous {
                    winners.insert(id, previous);
                }
            }
        }
    }
}

fn legacy_placement_links(record: &RawRecord) -> [Option<u32>; 2] {
    [
        record.cell_form_id,
        extractors::SubrecordView::new(&record.subrecords).unique_form_id(b"NAME"),
    ]
}

fn legacy_predecessors(candidate: &LegacyPlacementCandidate) -> impl Iterator<Item = &RawRecord> {
    std::iter::successors(candidate.predecessor.as_deref(), |predecessor| {
        predecessor
            .previous_candidate
            .as_ref()?
            .predecessor
            .as_deref()
    })
    .filter_map(|predecessor| predecessor.previous.as_ref())
}

fn legacy_grouped_predecessors(
    cell: Option<u32>,
    candidate: &LegacyPlacementCandidate,
) -> impl Iterator<Item = &RawRecord> {
    cell.is_none()
        .then_some(candidate)
        .into_iter()
        .flat_map(legacy_predecessors)
        .take_while(|previous| is_placed_record_type(&previous.record_type))
        .filter(|previous| {
            previous.cell_form_id.is_some()
                && valid_placement_transform(
                    extractors::SubrecordView::new(&previous.subrecords).find_all(b"DATA"),
                )
        })
}

/// Select failures against the current catalogue in dependency order. A caller
/// restores predecessors and starts another round before checking their dependents.
pub(crate) fn invalid_placement_candidates(
    placements: impl IntoIterator<Item = u32>,
    has_recovery: impl Fn(u32) -> bool,
    restores_nonplacement: impl Fn(u32) -> bool,
    links: impl Fn(u32) -> Option<PlacementDependencies>,
    validate: impl Fn(
        u32,
        &PlacementDependencies,
        Option<&HashSet<u32>>,
        &HashSet<u32>,
    ) -> std::result::Result<(), crate::records::PlacementFailure>,
) -> Vec<(u32, crate::records::PlacementFailure)> {
    // Gabow's path-based strongly connected components algorithm. Discovery indices
    // are retained while a placement is active. Edges to active nodes merge roots.
    let mut visited = HashMap::new();
    let mut traversal = Vec::new();
    let mut active: Vec<(u32, PlacementDependencies)> = Vec::new();
    let mut roots: Vec<(u32, usize, usize)> = Vec::new();
    let mut absent = HashSet::new();
    let mut unresolved = HashSet::new();
    let mut rejected = Vec::new();
    for id in placements {
        traversal.push((id, false));
        while let Some((id, explored)) = traversal.pop() {
            if explored {
                let Some(&(root, _, start)) = roots.last() else {
                    continue;
                };
                if root != id {
                    continue;
                }
                roots.pop();
                let component = &active[start..];
                if component.iter().any(|(id, targets)| {
                    targets.all().any(|target| {
                        unresolved.contains(&target)
                            || (absent.contains(&target)
                                && (component.len() > 1 || has_recovery(*id)))
                    })
                }) {
                    unresolved.extend(component.iter().map(|&(member, _)| member));
                } else if let Some(failure) = placement_component_failure(
                    component,
                    &restores_nonplacement,
                    &|id, deps, cycle| validate(id, deps, cycle, &absent),
                ) {
                    if component.len() == 1 && !has_recovery(failure.0) {
                        // Dependents can validate against this final removal during the same walk.
                        absent.insert(failure.0);
                    } else {
                        unresolved.extend(component.iter().map(|&(member, _)| member));
                    }
                    rejected.push(failure);
                }
                for &(member, _) in component {
                    visited.insert(member, None);
                }
                active.truncate(start);
            } else if let Some(&index) = visited.get(&id) {
                if let Some(index) = index {
                    while roots
                        .last()
                        .is_some_and(|&(_, root_index, _)| root_index > index)
                    {
                        roots.pop();
                    }
                }
            } else if let Some(targets) = links(id) {
                let index = visited.len();
                visited.insert(id, Some(index));
                roots.push((id, index, active.len()));
                traversal.push((id, true));
                traversal.extend(targets.all().map(|target| (target, false)));
                active.push((id, targets));
            }
        }
    }
    rejected
}

#[test]
fn terminal_placement_chain_is_rejected_in_one_walk() {
    let rejected = invalid_placement_candidates(
        1..=128,
        |_| false,
        |_| false,
        |id| {
            (id <= 128).then(|| PlacementDependencies {
                own: [None, Some(id + 1)],
                grouped: Vec::new(),
            })
        },
        |_, dependencies, _, absent| {
            let target = dependencies.own[1].unwrap();
            if target > 128 || absent.contains(&target) {
                return Err("missing base".to_owned().into());
            }
            Ok(())
        },
    );
    assert_eq!(
        rejected.into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
        (1..=128).rev().collect::<Vec<_>>()
    );
}

type PlacementLinks = [Option<u32>; 2];

pub(crate) struct PlacementDependencies {
    pub(crate) own: PlacementLinks,
    pub(crate) grouped: Vec<([u8; 4], PlacementLinks)>,
}

impl PlacementDependencies {
    fn all(&self) -> impl Iterator<Item = u32> + '_ {
        self.own
            .into_iter()
            .chain(self.grouped.iter().flat_map(|(_, links)| *links))
            .flatten()
    }
}

fn placement_component_failure(
    component: &[(u32, PlacementDependencies)],
    restores_nonplacement: &impl Fn(u32) -> bool,
    validate: &impl Fn(
        u32,
        &PlacementDependencies,
        Option<&HashSet<u32>>,
    ) -> std::result::Result<(), crate::records::PlacementFailure>,
) -> Option<(u32, crate::records::PlacementFailure)> {
    if component.len() > 1
        || component
            .first()
            .is_some_and(|(id, targets)| targets.all().any(|target| target == *id))
    {
        let members: HashSet<_> = component.iter().map(|&(id, _)| id).collect();
        // A failure outside the cycle can restore a usable target for another member.
        for (id, targets) in component.iter().rev() {
            if let Err(reason) = validate(*id, targets, Some(&members)) {
                return Some((*id, reason));
            }
        }
    }
    // When only internal links fail, restore a shadowed catalogue entry first. A dependent
    // placement may become usable as soon as its base or CELL reappears.
    component
        .iter()
        .rev()
        .filter_map(|(id, targets)| {
            validate(*id, targets, None)
                .err()
                .map(|reason| (*id, reason))
        })
        .min_by_key(|(id, _)| !restores_nonplacement(*id))
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
    let bytes: Vec<u8> = merged
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
    /// Invalid optional links, counted per containing plugin with the first diagnostic.
    skipped_optional: RefCell<BTreeMap<String, (u64, String)>>,
    retired_base_placements: RefCell<BTreeMap<String, (u64, u32, u32)>>,
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
        for (plugin, (count, id, base)) in self.retired_base_placements.borrow().iter() {
            let summary = format!(
                "notice: {plugin}: omitted {count} placement candidate(s) whose base was deleted by a later plugin (first: {id:08X}, required NAME {base:08X})"
            );
            eprintln!("{summary}");
            notice_lines.push(summary);
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
    tag == b"NAME" && is_placed_record_type(record_type)
}

/// A first definition with no projectable DATA retains its canonical identity.
fn valid_initial_placement_transform(data: Option<&[u8]>) -> bool {
    data.and_then(|bytes| bytes.first_chunk::<24>())
        .is_none_or(|transform| {
            transform
                .as_chunks::<4>()
                .0
                .iter()
                .all(|word| f32::from_le_bytes(*word).is_finite())
        })
}

/// Replacing a placement requires exactly one position/rotation tuple.
pub(crate) fn valid_placement_transform<'a>(mut fields: impl Iterator<Item = &'a [u8]>) -> bool {
    let Some(data) = fields.next().filter(|bytes| bytes.len() == 24) else {
        return false;
    };
    fields.next().is_none() && valid_initial_placement_transform(Some(data))
}

/// A missing parent disqualifies an override only if a usable predecessor owns a cell.
/// Callers defer that lookup until the placement targets can be checked.
pub(crate) fn validate_placement_parent(
    kind: &[u8; 4],
    cell: Option<u32>,
    previous_cell: impl FnOnce() -> Option<u32>,
) -> std::result::Result<(), String> {
    if is_placed_record_type(kind) {
        match cell {
            Some(0) => return Err("invalid placement parent cell".into()),
            None if previous_cell().is_some() => {
                return Err("missing parent cell on placement override".into());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Native records with a placed base and transform. Keep publication, required-link
/// validation and room membership on the same set; schema coverage is checked in tests.
pub(crate) const PLACED_RECORD_TYPES: &[[u8; 4]] = &[
    *b"REFR", *b"ACHR", *b"ACRE", *b"PGRE", *b"PMIS", *b"PHZD", *b"PARW", *b"PBAR", *b"PBEA",
    *b"PCON", *b"PFLA",
];

pub(crate) fn is_placed_record_type(record_type: &[u8; 4]) -> bool {
    PLACED_RECORD_TYPES.contains(record_type)
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
        // NPC Traits links: both are optional FormIDs in the actor schema.
        b"WNAM" | b"TPLT" if record_type == b"NPC_" => Fixed(SINGLE),
        // xEdit 9fb016884bec138ea6c7b872cec831537d464c3e,
        // wbDefinitionsTES5.pas:9463-9468. Match the in-house actors schema.
        b"WKMV" | b"RNMV" | b"SWMV" | b"FLMV" | b"SNMV" | b"SPMV" if record_type == b"RACE" => {
            Fixed(SINGLE)
        }
        // The body chain the actor model catalog follows: a race's skin armor, each armature
        // an armor lists (`MODL` repeats, one FormID per subrecord, so each malformed entry is
        // dropped alone), and an armature's race. The in-house actors and items schemas remap
        // all three, and the catalog reads them as load-order FormIDs.
        b"WNAM" if record_type == b"RACE" => Fixed(SINGLE),
        b"MODL" if record_type == b"ARMO" => Fixed(SINGLE),
        b"RNAM" if record_type == b"ARMA" => Fixed(SINGLE),
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
        b"XMBR" if is_placed_record_type(record_type) => Fixed(SINGLE),
        // xEdit 4.1.5f defines four nullable REFR FormIDs, each an itU32 via wbFormIDCk.
        // https://github.com/TES5Edit/TES5Edit/blob/xedit-4.1.5f/Core/wbDefinitionsTES5.pas#L12272-L12278
        // https://github.com/TES5Edit/TES5Edit/blob/xedit-4.1.5f/Core/wbInterface.pas#L9585-L9595
        b"XORD" if record_type == b"REFR" => Fixed(&[(16, &[0, 4, 8, 12])]),
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
        // A perk's next-rank link.
        b"NNAM" if record_type == b"PERK" => Fixed(SINGLE),
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
) -> Result<Option<perks::PerkRejectedSections>> {
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
    // An error before header remapping leaves no runtime identity for damage tracking.
    let source_id = std::mem::take(&mut record.form_id);
    record.form_id = remap(source_id)?;
    if record.is_deleted() {
        // Only the resolved header identity (and a complete GMST EDID) is needed
        // for deletion. Unused parent/body links cannot invalidate a tombstone.
        record.cell_form_id = None;
        record.worldspace_form_id = None;
        return Ok(None);
    }
    let occluder = record_type == *b"REFR"
        && extractors::SubrecordView::new(&record.subrecords)
            .find_all(b"NAME")
            // silent-ok: Only a complete NAME can identify the marker during this probe.
            .filter_map(|data| <[u8; 4]>::try_from(data).ok())
            .any(|word| {
                remap_link(u32::from_le_bytes(word))
                    .is_ok_and(|id| id == rooms::OCCLUSION_MARKER_BASE)
            });
    record.cell_form_id = record.cell_form_id.map(&remap_link).transpose()?;
    record.worldspace_form_id = record.worldspace_form_id.map(&remap_link).transpose()?;
    if record.record_type == *b"LAND" {
        let omitted = cell_cache::malformed_texture_items(&record.subrecords);
        if omitted != 0 {
            warnings.skipped_optional(plugin_name, format!("LAND source {source_id:08X} omitted {omitted} malformed texture items; usable layers retained"));
        }
    }
    let mut malformed_subrecords = Vec::new();
    let mut rejected_perk_effects = std::collections::BTreeSet::new();
    let mut perk_effect_index: Option<usize> = None;
    let mut next_perk_effect = 0usize;
    let mut perk_effect_type: Option<u8> = None;
    let mut perk_epft: Option<u8> = None;
    for (index, (tag, data)) in record.subrecords.iter_mut().enumerate() {
        if record_type == *b"PERK" {
            match tag.as_slice() {
                b"PRKE" => {
                    perk_effect_index = Some(next_perk_effect);
                    next_perk_effect += 1;
                    perk_effect_type = (data.len() == 3).then(|| data[0]);
                    perk_epft = None;
                    continue;
                }
                b"PRKF" => {
                    perk_effect_index = None;
                    perk_effect_type = None;
                    perk_epft = None;
                    continue;
                }
                b"EPFT" => {
                    perk_epft = (data.len() == 1).then(|| data[0]);
                    continue;
                }
                b"DATA" if perk_effect_type.is_some() => {
                    let has_link =
                        matches!((perk_effect_type, data.len()), (Some(0), 8) | (Some(1), 4));
                    if has_link {
                        let value = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                        let resolved = remap_with_validation(value, true).unwrap_or_else(|error| {
                            warnings.skipped_optional(
                                plugin_name,
                                format!("PERK source {source_id:08X} (load-order {:08X}) effect DATA reference {value:08X}: {error}", record.form_id),
                            );
                            0
                        });
                        data[..4].copy_from_slice(&resolved.to_le_bytes());
                    }
                    continue;
                }
                b"EPFD" if perk_effect_type == Some(2) && matches!(perk_epft, Some(3..=5)) => {
                    if data.len() == 4 {
                        let value = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                        let resolved = remap_with_validation(value, true).unwrap_or_else(|error| {
                            warnings.skipped_optional(
                                plugin_name,
                                format!("PERK source {source_id:08X} (load-order {:08X}) EPFD reference {value:08X}: {error}", record.form_id),
                            );
                            if value != 0
                                && let Some(effect_index) = perk_effect_index
                            {
                                rejected_perk_effects.insert(effect_index);
                            }
                            0
                        });
                        data[..4].copy_from_slice(&resolved.to_le_bytes());
                    } else {
                        warnings.skipped_optional(
                            plugin_name,
                            format!("PERK source {source_id:08X} (load-order {:08X}) EPFD must be 4 bytes, found {}", record.form_id, data.len()),
                        );
                        malformed_subrecords.push(index);
                    }
                    continue;
                }
                b"CTDA" if data.len() == 32 => {
                    for offset in gameplay_common::condition_link_offsets(data) {
                        let value = u32::from_le_bytes([
                            data[offset],
                            data[offset + 1],
                            data[offset + 2],
                            data[offset + 3],
                        ]);
                        let resolved = remap_with_validation(value, true).unwrap_or_else(|error| {
                            warnings.skipped_optional(
                                plugin_name,
                                format!(
                                    "{} record source {source_id:08X} (load-order {:08X}) CTDA reference {value:08X}: {error}",
                                    String::from_utf8_lossy(&record_type),
                                    record.form_id
                                ),
                            );
                            0
                        });
                        data[offset..offset + 4].copy_from_slice(&resolved.to_le_bytes());
                    }
                    continue;
                }
                _ => {}
            }
        }
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
                    if (record_type == *b"REFR" && tag.as_slice() == b"XORD")
                        || (occluder && matches!(tag.as_slice(), b"XESP" | b"XMBR"))
                    {
                        // Presence marks uncertain occlusion state. Dropping it could
                        // promote this marker to an independently usable occluder.
                        data.clear();
                    } else {
                        malformed_subrecords.push(index);
                    }
                    continue;
                }
            };
            for offset in offsets {
                let value = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                // Keep valid payload bytes, but publish null for an invalid FormID rather
                // than retaining a plugin-local value or guessing its owner.
                let resolved = match remap_with_validation(value, true).and_then(|resolved| {
                    color_eyre::eyre::ensure!(
                        resolved != 0
                            || value == 0
                            || record_type != *b"REFR"
                            || tag.as_slice() != b"XPOD",
                        "non-null portal endpoint resolved to zero"
                    );
                    Ok(resolved)
                }) {
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
                        // A null portal endpoint means unbounded space. An invalid link must
                        // remain unknown, so empty this occurrence rather than inventing one.
                        if record_type == *b"REFR"
                            && tag.as_slice() == b"XPOD"
                            && malformed_subrecords.last() != Some(&index)
                        {
                            malformed_subrecords.push(index);
                        }
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
    let rejected = if (!malformed_subrecords.is_empty() || !rejected_perk_effects.is_empty())
        && record.record_type == *b"PERK"
    {
        let sections = perks::remap_rejected_sections(
            &record.subrecords,
            &malformed_subrecords,
            &rejected_perk_effects,
        );
        (!sections.is_empty()).then_some(sections)
    } else {
        None
    };
    if !malformed_subrecords.is_empty() {
        // Keep empty authored room fields: absence would turn unusable assignments into
        // automatic/NONE ownership or hide broken room links and conflicting portals.
        // Other optional fields retain their valid siblings and their relative order.
        let mut index = 0;
        record.subrecords.retain_mut(|(tag, data)| {
            let malformed = malformed_subrecords.binary_search(&index).is_ok();
            index += 1;
            if malformed
                && ((record_type == *b"REFR" && matches!(tag.as_slice(), b"XPOD" | b"XLRM"))
                    || (tag.as_slice() == b"XMBR" && is_placed_record_type(&record_type)))
            {
                data.clear();
                true
            } else {
                !malformed
            }
        });
    }
    Ok(rejected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occlusion_links_remap_all_four_slots_and_keep_malformed_presence() {
        let indices = HashMap::from([("base.esm".into(), 0), ("patch.esp".into(), 2)]);
        let warnings = RemapWarnings::default();
        let mut record = RawRecord {
            form_id: 0x0100_0800,
            record_type: *b"REFR",
            flags: 0,
            subrecords: vec![
                (
                    b"XORD".to_vec(),
                    [0x0100_0900u32, 0x0000_0901, 0x0100_0902, 0x0300_0903]
                        .into_iter()
                        .flat_map(u32::to_le_bytes)
                        .collect(),
                ),
                (b"XORD".to_vec(), vec![1; 12]),
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
            [0x0200_0900u32, 0x0000_0901, 0x0200_0902, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
        );
        assert_eq!(record.subrecords[1], (b"XORD".to_vec(), Vec::new()));
    }

    #[test]
    fn malformed_occlusion_state_preserves_enable_parent_and_override_presence() {
        let indices = HashMap::from([
            ("other.esm".into(), 1),
            ("skyrim.esm".into(), 0),
            ("patch.esp".into(), 2),
        ]);
        for (raw_base, retained) in [(0x0100_0017u32, true), (0x17, false)] {
            let mut record = RawRecord {
                form_id: 0x0200_0800,
                record_type: *b"REFR",
                flags: 0,
                subrecords: vec![
                    (b"NAME".to_vec(), raw_base.to_le_bytes().to_vec()),
                    (b"XESP".to_vec(), vec![0; 3]),
                    (b"XMBR".to_vec(), vec![0; 3]),
                ],
                cell_form_id: None,
                worldspace_form_id: None,
                load_order: 0,
            };
            remap_record_form_ids(
                &mut record,
                "patch.esp",
                &["other.esm".into(), "skyrim.esm".into()],
                &indices,
                &HashMap::new(),
                &RemapWarnings::default(),
            )
            .unwrap();
            let view = extractors::SubrecordView::new(&record.subrecords);
            assert_eq!(view.find(b"XMBR"), Some([].as_slice()));
            assert_eq!(view.find(b"XESP"), retained.then_some([].as_slice()));
        }
    }

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
