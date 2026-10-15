use crate::esm::{
    binary::parse_plugin_file_skipping_malformed,
    exporter::{create_tables, export_to_db_with_load_order},
    records::RawRecord,
};
use color_eyre::{Result, eyre::WrapErr};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};
pub mod binary;
pub mod cell_cache;
pub mod exporter;
pub mod extractors;
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

pub struct EsmParser;

impl EsmParser {
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
        let order = load_order::LoadOrder::read(plugin_paths)?;
        let master = Self::merge_plugins_with_load_order(plugin_paths, &order)?;
        let conn = Connection::open(db_path)?;
        create_tables(&conn)?;
        for (priority, path) in plugin_paths.iter().enumerate() {
            let checksum = Sha256::digest(std::fs::read(path)?);
            conn.execute(
                "INSERT OR REPLACE INTO plugins (id, name, priority, checksum) VALUES (?1, ?2, ?3, ?4)",
                params![priority as i64, path.file_name().unwrap_or_default().to_string_lossy(), priority as i64, checksum.as_slice()],
            )?;
        }
        export_to_db_with_load_order(&conn, &master, &order)?;

        Ok(master)
    }

    /// Merge plugin records with validated slots and EditorID-based game settings.
    pub fn merge_plugins(plugin_paths: &[PathBuf]) -> Result<HashMap<u32, RawRecord>> {
        let order = load_order::LoadOrder::read(plugin_paths)?;
        Self::merge_plugins_with_load_order(plugin_paths, &order)
    }

    /// Use one load-order mapping for both merging and ownership export.
    fn merge_plugins_with_load_order(
        plugin_paths: &[PathBuf],
        order: &load_order::LoadOrder,
    ) -> Result<HashMap<u32, RawRecord>> {
        let warnings = RemapWarnings::default();
        let merged = Self::merge_with_warnings(plugin_paths, order, &warnings)?;
        warnings.report();
        Ok(merged)
    }

    /// Merge into `warnings` without reporting them, so the caller decides when to emit.
    fn merge_with_warnings(
        plugin_paths: &[PathBuf],
        order: &load_order::LoadOrder,
        warnings: &RemapWarnings,
    ) -> Result<HashMap<u32, RawRecord>> {
        let mut merged = HashMap::new();
        // Unlike ordinary forms, game settings override by EditorID. Retain
        // the first definition's key/owner while taking the last setting value.
        // Keep keys through deletions so a later restoration has the same ID.
        let mut game_settings = GameSettingIdentities::default();
        for (priority, path) in plugin_paths.iter().enumerate() {
            let masters = &order.metadata[priority].masters;
            // Final ID -> (source ID, whether its master index was out of range). Two different
            // source IDs that resolve to one final ID would silently replace each other.
            let mut sources: HashMap<u32, (u32, bool)> = HashMap::new();
            // One record that cannot be decoded is left out, not fatal (Mudcrab #182).
            let (records, skipped) = parse_plugin_file_skipping_malformed(path)?;
            if let Some(first) = skipped.first {
                warnings.skipped_records(&order.names[priority], skipped.count, first);
            }
            for mut record in records {
                record.load_order = priority as u32;
                let source_id = record.form_id;
                let out_of_range = (source_id >> 24) as usize > masters.len();
                let remapped = remap_record_form_ids(
                    &mut record,
                    &order.names[priority],
                    masters,
                    &order.normal,
                    &order.light,
                    warnings,
                )
                .wrap_err_with(|| {
                    format!(
                        "{} record {} source {source_id:08X} (load-order {:08X})",
                        order.names[priority],
                        String::from_utf8_lossy(&record.record_type),
                        record.form_id
                    )
                })?;
                if remapped == Remapped::Skipped {
                    continue;
                }
                if let Some((earlier, earlier_out_of_range)) =
                    sources.insert(record.form_id, (source_id, out_of_range))
                {
                    // Light-ID truncation keeps its documented last-record-wins behaviour; only
                    // an out-of-range master index is rejected when it lands on another record.
                    color_eyre::eyre::ensure!(
                        earlier == source_id || !(out_of_range || earlier_out_of_range),
                        "{}: records {earlier:08X} and {source_id:08X} both resolve to {:08X}; an out-of-range master index would replace another record",
                        order.names[priority],
                        record.form_id
                    );
                }
                if record.record_type == *b"GMST" {
                    let Some(canonical) = game_settings.resolve(
                        &record,
                        &order.names[priority],
                        &merged,
                        warnings,
                    )?
                    else {
                        continue;
                    };
                    record.form_id = canonical;
                } else {
                    color_eyre::eyre::ensure!(
                        !game_settings.aliases.contains_key(&record.form_id),
                        "record {:08X} collides with a GMST identity",
                        record.form_id
                    );
                }
                if record.is_deleted() {
                    merged.remove(&record.form_id);
                } else {
                    merged.insert(record.form_id, record);
                }
            }
        }
        Ok(merged)
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
    /// IDs whose master index lies past the plugin's master list, keyed by the plugin that
    /// contains them; the example keeps its record type.
    out_of_range: RefCell<OutOfRangeCounts>,
    /// Invalid optional links, counted per containing plugin with the first diagnostic.
    skipped_optional: RefCell<BTreeMap<String, (u64, String)>>,
    /// Records left out of the merge, counted per containing plugin with the first reason.
    skipped_records: RefCell<BTreeMap<String, (u64, String)>>,
}

impl RemapWarnings {
    /// Accumulate one truncation without emitting a per-reference diagnostic.
    fn truncated(&self, owner: &str, form_id: u32) {
        let mut counts = self.truncations.borrow_mut();
        let entry = counts.entry(owner.to_owned()).or_insert((0, form_id));
        entry.0 += 1;
    }

    /// Accumulate one ID that names a master the plugin does not have.
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

    /// Count `count` records left out of the merge, keeping the first reason per plugin.
    fn skipped_records(&self, plugin: &str, count: u64, example: String) {
        let mut counts = self.skipped_records.borrow_mut();
        let entry = counts.entry(plugin.to_owned()).or_insert((0, example));
        entry.0 += count;
    }

    /// Emit one deterministic summary per affected plugin after a successful merge.
    fn report(&self) {
        for (owner, (count, example)) in self.truncations.borrow().iter() {
            eprintln!(
                "warning: {owner}: {count} light-plugin ID occurrences exceeded 12 bits and were truncated (first: {example:08X}); compact the plugin's FormIDs before ESL-flagging it"
            );
        }
        for (plugin, (count, example, record_type)) in self.out_of_range.borrow().iter() {
            eprintln!(
                "warning: {plugin}: {count} FormID occurrences name a master index past the plugin's master list and were treated as the plugin's own (first: {example:08X} in {})",
                String::from_utf8_lossy(record_type)
            );
        }
        for (plugin, (count, example)) in self.skipped_optional.borrow().iter() {
            eprintln!(
                "warning: {plugin}: skipped {count} invalid optional FormID links (cleared or removed; first: {example})"
            );
        }
        for (plugin, (count, example)) in self.skipped_records.borrow().iter() {
            eprintln!(
                "warning: {plugin}: skipped {count} invalid records (left out of the conversion; first: {example})"
            );
        }
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
        warnings: &RemapWarnings,
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
                eprintln!(
                    "warning: {plugin} deleted GMST {source_id:08X} has no EditorID or known setting identity; skipping"
                );
                return Ok(None);
            }
            warnings.skipped_records(plugin, 1, format!("GMST {source_id:08X} has no EditorID"));
            return Ok(None);
        };
        let known = self.canonical_ids.get(&editor_id).copied();
        let canonical = known.unwrap_or(source_id);
        if canonical == 0 {
            warnings.skipped_records(plugin, 1, format!("GMST {editor_id} has a null FormID"));
            return Ok(None);
        }
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

/// Determines whether a subrecord within a given parent record type represents
/// a 32-bit FormID that requires load-order remapping.
///
/// This record-aware check ensures that subrecords with shared tag names (such as
/// `CNAM` or `SNAM`) containing strings (e.g. `TES4` author/description), float
/// physics arrays (`TREE` trunk flexibility), or RGBA color structures (`CLFM`/`AACT`)
/// are not inadvertently overwritten as 4-byte FormIDs.
fn is_form_id_subrecord(record_type: &[u8; 4], tag: &[u8], len: usize) -> bool {
    let Ok(tag_4) = <&[u8; 4]>::try_from(tag) else {
        return false;
    };
    // xEdit wbFaction: FormID, signed rank, three unused bytes. Only the
    // leading FormID changes; the same SNAM tag on TES4/TREE has another layout.
    if record_type == b"NPC_" && tag_4 == b"SNAM" {
        return len == 8;
    }
    if len != 4 {
        return false;
    }
    match (record_type, tag_4) {
        (b"TES4" | b"CLFM" | b"AACT", _) => false,
        (b"TREE", b"SNAM" | b"PFIG") => true,
        (b"LTEX", b"TNAM" | b"GNAM" | b"MNAM") => true,
        (b"CELL", b"XCWT") => true,
        (b"WRLD", b"NAM2" | b"NAM3") => true,
        // xEdit TES5Edit @ 9fb016884bec138ea6c7b872cec831537d464c3e defines
        // WRLD.RNAM as mixed coordinate/reference rows (Common:10011-10038)
        // and TNAM as a string (TES5:10708). Neither is a single FormID;
        // nested RNAM reference remapping remains separate work.
        (b"WRLD", b"WNAM" | b"CNAM") => true,
        (b"CELL", b"XOWN" | b"XGLB" | b"XEZN" | b"XLCN" | b"XLRL") => true,
        (b"NPC_", b"RNAM" | b"CNAM" | b"INAM") => true,
        (b"RACE", b"WKMV" | b"RNMV") => true,
        (
            b"REFR" | b"ACHR" | b"ACRE" | b"PGRE" | b"PMIS" | b"PHZD" | b"PARW" | b"PBAR" | b"PBEA"
            | b"PCON" | b"PFLA",
            b"NAME" | b"XOWN" | b"XGLB" | b"XEZN" | b"XLCN" | b"XLRL",
        ) => true,
        (_, b"XOWN" | b"XGLB" | b"XEZN" | b"XLCN" | b"XLRL") => true,
        _ => false,
    }
}

/// Where FormIDs sit inside a reference or cell subrecord that `is_form_id_subrecord`
/// does not cover (it only matches single four-byte fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormIdLayout {
    /// A fixed-size structure: each accepted byte length with its FormID offsets.
    Fixed(&'static [(usize, &'static [usize])]),
    /// A packed array of `stride`-byte entries, each starting with a FormID.
    Array { stride: usize },
}

/// Field layouts per xEdit's TES5 definitions
/// (https://github.com/TES5Edit/TES5Edit/blob/dev-4.1.5/Core/wbDefinitionsTES5.pas).
fn form_id_layout(record_type: &[u8; 4], tag: &[u8]) -> Option<FormIdLayout> {
    use FormIdLayout::{Array, Fixed};
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
        b"XLRT" if placed => Array { stride: 4 },
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
        b"XPOD" if record_type == b"REFR" => Array { stride: 4 },
        b"XLRM" | b"LNAM" | b"INAM" if record_type == b"REFR" => Fixed(SINGLE),
        // Regions.
        b"XCLR" if record_type == b"CELL" => Array { stride: 4 },
        _ => return None,
    })
}

/// Whether a record survived FormID remapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Remapped {
    Kept,
    /// Left out of the merge and counted in `RemapWarnings::skipped_records`.
    Skipped,
}

/// A link that resolved into load-order numbering, or why it could not be resolved
/// without guessing its owner or truncating it onto another ID.
type Link = std::result::Result<u32, String>;

/// Remap a `GRAS.MODS` alternate-texture list, dropping each entry whose texture cannot
/// be resolved. Returns the dropped entries' reasons, or the reason the whole list is
/// malformed (truncated or trailing bytes), in which case `data` is left unchanged.
fn remap_alternate_textures(
    data: &mut Vec<u8>,
    resolve: impl Fn(u32) -> Result<Link>,
) -> Result<std::result::Result<Vec<String>, String>> {
    let Some((count, mut rest)) = data.split_first_chunk::<4>() else {
        return Ok(Err("truncated GRAS alternate textures".to_owned()));
    };
    let mut entries = Vec::new();
    for _ in 0..u32::from_le_bytes(*count) {
        let Some((length, after)) = rest.split_first_chunk::<4>() else {
            return Ok(Err("truncated MODS name length".to_owned()));
        };
        let length = u32::from_le_bytes(*length) as usize;
        if after.len() < length.saturating_add(8) {
            return Ok(Err("truncated MODS entry".to_owned()));
        }
        let (name, after) = after.split_at(length);
        let (texture, after) = after.split_at(4);
        let (index, after) = after.split_at(4);
        entries.push((name, texture, index));
        rest = after;
    }
    if !rest.is_empty() {
        return Ok(Err(format!("{} trailing bytes in GRAS MODS", rest.len())));
    }
    let mut kept = Vec::with_capacity(entries.len());
    let mut dropped = Vec::new();
    for (name, texture, index) in entries {
        let mut value = [0; 4];
        value.copy_from_slice(texture);
        match resolve(u32::from_le_bytes(value))? {
            Ok(texture) => kept.push((name, texture, index)),
            Err(reason) => dropped.push(format!(
                "MODS alternate texture for {}: {reason}",
                String::from_utf8_lossy(name).trim_end_matches('\0')
            )),
        }
    }
    let mut rebuilt = (kept.len() as u32).to_le_bytes().to_vec();
    for (name, texture, index) in kept {
        rebuilt.extend((name.len() as u32).to_le_bytes());
        rebuilt.extend_from_slice(name);
        rebuilt.extend(texture.to_le_bytes());
        rebuilt.extend_from_slice(index);
    }
    *data = rebuilt;
    Ok(Ok(dropped))
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
) -> Result<Remapped> {
    // Landscape, grass and texture records validate every FormID strictly: an ID with a
    // master index past the master list, or a light-plugin ID wider than 12 bits, is never
    // treated as the plugin's own or truncated, because either could name another record.
    // Such a link is removed instead, and a record whose own identity (or parent cell or
    // worldspace) is unresolvable is left out (Mudcrab #182). Other record kinds keep the
    // legacy handling until their record-specific exceptions (including shipped GMST IDs
    // outside the master table) have been audited. Optional link fields in
    // `form_id_layout` clear invalid IDs or drop malformed subrecords on every record kind.
    let strict = matches!(
        &record.record_type,
        b"GRAS" | b"LTEX" | b"TXST" | b"LAND" | b"CELL" | b"WRLD"
    );
    let record_type = record.record_type;
    let type_name = String::from_utf8_lossy(&record_type).into_owned();
    // Why `form_id` cannot be resolved without guessing, or `None` when it can.
    let unresolvable = |form_id: u32| -> Option<String> {
        if form_id == 0 {
            return None;
        }
        let local_index = (form_id >> 24) as usize;
        if local_index > masters.len() {
            return Some(format!(
                "{plugin_name}: {form_id:08X} has invalid master index {local_index}"
            ));
        }
        let owner = masters.get(local_index).map_or_else(
            || plugin_name.to_owned(),
            |master| master.to_ascii_lowercase(),
        );
        (light_indices.contains_key(&owner) && form_id & 0x00ff_ffff > 0xfff)
            .then(|| format!("{owner}: light-plugin local ID exceeds 12 bits: {form_id:08X}"))
    };
    let remap_with_validation = |form_id: u32, strict: bool| -> Result<u32> {
        if form_id == 0 {
            return Ok(0);
        }
        if strict && let Some(reason) = unresolvable(form_id) {
            color_eyre::eyre::bail!(reason);
        }
        let local_index = (form_id >> 24) as usize;
        if local_index > masters.len() {
            // Treated as the plugin's own, which shipped data relies on (Skyrim.esm and
            // Dawnguard.esm each carry one such ID). Reported, and the merge rejects it when
            // it would replace another record.
            warnings.out_of_range(plugin_name, form_id, record_type);
        }
        let owner = if local_index < masters.len() {
            shared::plugin_selection::normalize_plugin_name(&masters[local_index])
        } else {
            plugin_name.to_owned()
        };
        if let Some(index) = light_indices.get(&owner) {
            if form_id & 0x00ff_ffff > 0xfff {
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
    // On a strict record an unresolvable link is reported to the caller, which removes
    // it; elsewhere it follows the legacy handling. A master missing from the load order
    // is still an error.
    let resolve_link = |form_id: u32| -> Result<Link> {
        if strict && let Some(reason) = unresolvable(form_id) {
            return Ok(Err(reason));
        }
        remap(form_id).map(Ok)
    };
    let source_id = record.form_id;
    if strict
        && let Some(reason) = [
            Some(record.form_id),
            record.cell_form_id,
            record.worldspace_form_id,
        ]
        .into_iter()
        .flatten()
        .find_map(&unresolvable)
    {
        warnings.skipped_records(
            plugin_name,
            1,
            format!("{type_name} record source {source_id:08X}: {reason}"),
        );
        return Ok(Remapped::Skipped);
    }
    record.form_id = remap(record.form_id)?;
    record.cell_form_id = record.cell_form_id.map(&remap).transpose()?;
    record.worldspace_form_id = record.worldspace_form_id.map(&remap).transpose()?;
    let load_order_id = record.form_id;
    let skip_field = |reason: String| {
        warnings.skipped_optional(
            plugin_name,
            format!(
                "{type_name} record source {source_id:08X} (load-order {load_order_id:08X}) {reason}"
            ),
        );
    };
    let mut malformed_subrecords = Vec::new();
    // Set after an ATXT layer is dropped, so its VTXT weights go with it rather than
    // attaching to whichever layer precedes them.
    let mut dropped_layer = false;
    for (index, (tag, data)) in record.subrecords.iter_mut().enumerate() {
        let after_dropped_layer = std::mem::take(&mut dropped_layer);
        if record_type == *b"LAND" && tag.as_slice() == b"VTXT" && after_dropped_layer {
            malformed_subrecords.push(index);
            dropped_layer = true;
            continue;
        }
        if record_type == *b"GRAS" && tag.as_slice() == b"MODS" {
            match remap_alternate_textures(data, resolve_link)? {
                Ok(dropped) => dropped.into_iter().for_each(&skip_field),
                Err(reason) => {
                    skip_field(reason);
                    malformed_subrecords.push(index);
                }
            }
            continue;
        }
        if record_type == *b"LAND" && matches!(tag.as_slice(), b"BTXT" | b"ATXT") {
            let name = String::from_utf8_lossy(tag).into_owned();
            let link = match data.first_chunk::<4>() {
                Some(value) if data.len() == 8 => resolve_link(u32::from_le_bytes(*value))?
                    .map_err(|reason| format!("{name} texture {reason}")),
                _ => Err(format!("{name} must be 8 bytes, found {}", data.len())),
            };
            match link {
                Ok(texture) => data[..4].copy_from_slice(&texture.to_le_bytes()),
                Err(reason) => {
                    // Without its texture the layer cannot be drawn; a null base or
                    // overlay would paint the default texture instead.
                    skip_field(reason);
                    malformed_subrecords.push(index);
                    dropped_layer = tag.as_slice() == b"ATXT";
                }
            }
            continue;
        }
        if record_type == *b"LAND" && tag.as_slice() == b"VTEX" {
            // xEdit's TES5 LAND definition uses an array of wbFormIDCk(LTEX,
            // NULL), unlike the 8-byte BTXT/ATXT layer structures above:
            // https://github.com/TES5Edit/TES5Edit/blob/dev-4.1.5/Core/wbDefinitionsTES5.pas
            if !data.len().is_multiple_of(4) {
                skip_field(format!("VTEX length {} is not a multiple of 4", data.len()));
                malformed_subrecords.push(index);
                continue;
            }
            let mut kept = Vec::with_capacity(data.len());
            for value in data.as_chunks::<4>().0 {
                match resolve_link(u32::from_le_bytes(*value))? {
                    Ok(texture) => kept.extend(texture.to_le_bytes()),
                    Err(reason) => skip_field(format!("VTEX entry {reason}")),
                }
            }
            *data = kept;
            continue;
        }
        if tag.as_slice() == b"VMAD" {
            records::record_type::vmad::remap_primary_form_ids(data, &remap)
                .wrap_err("VMAD references")?;
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
                FormIdLayout::Array { stride } => {
                    if data.len().is_multiple_of(stride) {
                        Ok((0..data.len()).step_by(stride).collect())
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
        if is_form_id_subrecord(&record.record_type, tag, data.len())
            && let Some(value) = data.first_chunk::<4>()
        {
            let name = String::from_utf8_lossy(tag).into_owned();
            match resolve_link(u32::from_le_bytes(*value))
                .wrap_err_with(|| format!("{name} reference"))?
            {
                Ok(resolved) => data[..4].copy_from_slice(&resolved.to_le_bytes()),
                Err(reason) => {
                    // Removing the link reads as "not set", as the game treats a
                    // missing field; a null value could be read as an explicit choice.
                    skip_field(format!("{name} reference {reason}"));
                    malformed_subrecords.push(index);
                }
            }
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
    Ok(Remapped::Kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
        [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
    }

    fn record_bytes(tag: &[u8; 4], id: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
        [
            tag.as_slice(),
            &(payload.len() as u32).to_le_bytes(),
            &flags.to_le_bytes(),
            &id.to_le_bytes(),
            &[0; 8],
            payload,
        ]
        .concat()
    }

    fn write_plugin(
        root: &Path,
        name: &str,
        masters: &[&str],
        flags: u32,
        records: &[u8],
    ) -> PathBuf {
        let mut header = Vec::new();
        for master in masters {
            header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
            header.extend(sub(b"DATA", &[0; 8]));
        }
        let path = root.join(name);
        std::fs::write(
            &path,
            [record_bytes(b"TES4", 0, flags, &header), records.to_vec()].concat(),
        )
        .unwrap();
        path
    }

    /// Merge with a caller-owned warning collector so a test can inspect the aggregates.
    fn merge_collecting(paths: &[PathBuf]) -> (HashMap<u32, RawRecord>, RemapWarnings) {
        let order = load_order::LoadOrder::read(paths).unwrap();
        let warnings = RemapWarnings::default();
        let merged = EsmParser::merge_with_warnings(paths, &order, &warnings).unwrap();
        (merged, warnings)
    }

    fn stat(id: u32) -> Vec<u8> {
        record_bytes(b"STAT", id, 0, &sub(b"EDID", b"Valid\0"))
    }

    /// Mudcrab #182 row 1: a record whose payload cannot be decoded is skipped by itself.
    /// Its length comes from the record header, so the records after it still parse.
    #[test]
    fn undecodable_records_are_skipped_and_counted_per_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let truncated_subrecord = record_bytes(b"STAT", 0x801, 0, b"DATA\x04\x00\x01");
        let mut bad_zlib = 6u32.to_le_bytes().to_vec();
        bad_zlib.extend_from_slice(b"not a zlib stream");
        let undecompressable = record_bytes(b"STAT", 0x802, 0x0004_0000, &bad_zlib);
        let short_compressed = record_bytes(b"STAT", 0x804, 0x0004_0000, &[1, 2]);
        let base = write_plugin(
            dir.path(),
            "Base.esm",
            &[],
            0,
            &[
                stat(0x800),
                truncated_subrecord,
                undecompressable,
                short_compressed,
                stat(0x803),
            ]
            .concat(),
        );
        let (merged, warnings) = merge_collecting(&[base]);
        let mut ids: Vec<_> = merged.keys().copied().collect();
        ids.sort_unstable();
        assert_eq!(ids, [0x800, 0x803]);
        let skipped = warnings.skipped_records.borrow();
        let (count, example) = &skipped["base.esm"];
        assert_eq!(*count, 3);
        assert!(
            example.contains("00000801") && example.contains("truncated DATA"),
            "{example}"
        );
    }

    fn group_bytes(kind: i32, label: u32, payload: &[u8]) -> Vec<u8> {
        [
            b"GRUP".as_slice(),
            &(payload.len() as u32 + 24).to_le_bytes(),
            &label.to_le_bytes(),
            &kind.to_le_bytes(),
            &[0; 8],
            payload,
        ]
        .concat()
    }

    fn layer(tag: &[u8; 4], texture: u32, quadrant: u8, index: u16) -> Vec<u8> {
        sub(
            tag,
            &[
                texture.to_le_bytes().as_slice(),
                &[quadrant, 0],
                &index.to_le_bytes(),
            ]
            .concat(),
        )
    }

    fn weights(vertex: u16, opacity: f32) -> Vec<u8> {
        sub(
            b"VTXT",
            &[
                vertex.to_le_bytes().as_slice(),
                &[0, 0],
                &opacity.to_le_bytes(),
            ]
            .concat(),
        )
    }

    fn ids(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    fn tags(record: &RawRecord) -> Vec<String> {
        record
            .subrecords
            .iter()
            .map(|(tag, _)| String::from_utf8_lossy(tag).into_owned())
            .collect()
    }

    /// Mudcrab #182 row 2: an unresolvable FormID on a terrain, grass or texture record no
    /// longer stops the merge. A link field is removed (never truncated or guessed, which
    /// could name another record); an unresolvable record identity drops that record.
    #[test]
    fn unresolvable_terrain_and_grass_form_ids_are_dropped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let base = write_plugin(dir.path(), "Base.esm", &[], 0, &[]);
        let records = [
            record_bytes(
                b"LTEX",
                0x0100_0810,
                0,
                &[
                    sub(b"TNAM", &0x0000_0900u32.to_le_bytes()),
                    sub(b"GNAM", &0x0500_0901u32.to_le_bytes()),
                    sub(b"GNAM", &0x0000_0902u32.to_le_bytes()),
                    sub(b"MNAM", &0x0700_0000u32.to_le_bytes()),
                ]
                .concat(),
            ),
            // The record's own master index is past the master list.
            record_bytes(b"GRAS", 0x0500_0820, 0, &sub(b"EDID", b"Lost\0")),
            record_bytes(
                b"CELL",
                0x0100_0830,
                0,
                &[
                    sub(b"EDID", b"Cell\0"),
                    sub(b"XCWT", &0x0900_0001u32.to_le_bytes()),
                ]
                .concat(),
            ),
            record_bytes(
                b"LAND",
                0x0100_0840,
                0,
                &[
                    layer(b"BTXT", 0x0500_0001, 0, 0),
                    layer(b"BTXT", 0x0000_0006, 1, 0),
                    layer(b"ATXT", 0x0500_0002, 1, 0),
                    weights(3, 0.5),
                    layer(b"ATXT", 0x0000_0003, 1, 1),
                    weights(4, 0.25),
                    sub(b"VTEX", &ids(&[0x0000_0004, 0x0500_0005, 0x0100_0006])),
                ]
                .concat(),
            ),
            record_bytes(
                b"WRLD",
                0x0100_0850,
                0,
                &sub(b"NAM2", &0x0600_0000u32.to_le_bytes()),
            ),
            // A LAND whose parent cell (the group label) is unresolvable.
            group_bytes(9, 0x0500_0999, &record_bytes(b"LAND", 0x0100_0860, 0, &[])),
        ]
        .concat();
        let patch = write_plugin(dir.path(), "Patch.esp", &["Base.esm"], 0, &records);
        // A light plugin's local ID wider than 12 bits would be truncated onto another ID.
        let light = write_plugin(
            dir.path(),
            "Light.esl",
            &[],
            0x200,
            &record_bytes(b"GRAS", 0x1800, 0, &sub(b"EDID", b"Wide\0")),
        );
        let (merged, warnings) = merge_collecting(&[base, patch, light]);
        let mut found: Vec<_> = merged.keys().copied().collect();
        found.sort_unstable();
        assert_eq!(found, [0x0100_0810, 0x0100_0830, 0x0100_0840, 0x0100_0850]);

        let ltex = &merged[&0x0100_0810];
        assert_eq!(
            ltex.subrecords,
            [
                (b"TNAM".to_vec(), 0x0000_0900u32.to_le_bytes().to_vec()),
                (b"GNAM".to_vec(), 0x0000_0902u32.to_le_bytes().to_vec()),
            ]
        );
        assert_eq!(tags(&merged[&0x0100_0830]), ["EDID"]);
        assert!(merged[&0x0100_0850].subrecords.is_empty());
        let land = &merged[&0x0100_0840];
        assert_eq!(
            land.subrecords,
            [
                (
                    b"BTXT".to_vec(),
                    [0x0000_0006u32.to_le_bytes().as_slice(), &[1, 0, 0, 0]].concat()
                ),
                (
                    b"ATXT".to_vec(),
                    [0x0000_0003u32.to_le_bytes().as_slice(), &[1, 0, 1, 0]].concat()
                ),
                (
                    b"VTXT".to_vec(),
                    [
                        4u16.to_le_bytes().as_slice(),
                        &[0, 0],
                        &0.25f32.to_le_bytes()
                    ]
                    .concat()
                ),
                (b"VTEX".to_vec(), ids(&[0x0000_0004, 0x0100_0006])),
            ]
        );

        let records = warnings.skipped_records.borrow();
        assert_eq!(records["patch.esp"].0, 2, "{:?}", records["patch.esp"]);
        assert!(
            records["patch.esp"].1.contains("05000820"),
            "{}",
            records["patch.esp"].1
        );
        assert_eq!(records["light.esl"].0, 1);
        assert!(records["light.esl"].1.contains("12 bits"));
        let fields = warnings.skipped_optional.borrow();
        // GNAM, MNAM, XCWT, a BTXT, an ATXT with its VTXT, one VTEX entry and NAM2.
        assert_eq!(fields["patch.esp"].0, 7, "{:?}", fields["patch.esp"]);
        assert!(warnings.out_of_range.borrow().is_empty());
        assert!(warnings.truncations.borrow().is_empty());
    }

    fn alternate_textures(entries: &[(&[u8], u32, u32)]) -> Vec<u8> {
        let mut bytes = (entries.len() as u32).to_le_bytes().to_vec();
        for (name, texture, index) in entries {
            bytes.extend((name.len() as u32).to_le_bytes());
            bytes.extend_from_slice(name);
            bytes.extend(texture.to_le_bytes());
            bytes.extend(index.to_le_bytes());
        }
        bytes
    }

    /// Mudcrab #182 row 3: malformed `GRAS.MODS` drops that subrecord; an unresolvable
    /// alternate texture drops only its own entry.
    #[test]
    fn malformed_grass_alternate_textures_are_dropped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let base = write_plugin(dir.path(), "Base.esm", &[], 0, &[]);
        let grass = |id: u32, mods: Vec<u8>| {
            record_bytes(
                b"GRAS",
                id,
                0,
                &[sub(b"EDID", b"Grass\0"), sub(b"MODS", &mods)].concat(),
            )
        };
        let mut trailing = alternate_textures(&[(b"a", 0x0000_0001, 0)]);
        trailing.push(0);
        let records = [
            grass(
                0x0100_0870,
                alternate_textures(&[(b"a", 0x0500_0001, 0), (b"bb", 0x0100_0002, 1)]),
            ),
            grass(0x0100_0871, 1u32.to_le_bytes().to_vec()),
            grass(0x0100_0872, vec![0; 3]),
            grass(0x0100_0873, trailing),
        ]
        .concat();
        let patch = write_plugin(dir.path(), "Patch.esp", &["Base.esm"], 0, &records);
        let (merged, warnings) = merge_collecting(&[base, patch]);
        assert_eq!(merged.len(), 4);
        assert_eq!(
            extractors::SubrecordView::new(&merged[&0x0100_0870].subrecords).find(b"MODS"),
            Some(alternate_textures(&[(b"bb", 0x0100_0002, 1)]).as_slice())
        );
        for id in [0x0100_0871, 0x0100_0872, 0x0100_0873] {
            assert_eq!(tags(&merged[&id]), ["EDID"], "{id:08X}");
        }
        let fields = warnings.skipped_optional.borrow();
        assert_eq!(fields["patch.esp"].0, 4, "{:?}", fields["patch.esp"]);
        assert!(warnings.skipped_records.borrow().is_empty());
    }

    /// Mudcrab #182 row 4: a LAND texture layer or VTEX list with the wrong length is
    /// dropped (an ATXT with its VTXT weights); the rest of the LAND survives.
    #[test]
    fn malformed_land_texture_subrecords_are_dropped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let land = record_bytes(
            b"LAND",
            0x800,
            0,
            &[
                sub(b"DATA", &[1, 0, 0, 0]),
                sub(b"BTXT", &[0; 7]),
                sub(b"ATXT", &[0; 9]),
                weights(1, 0.5),
                layer(b"ATXT", 0x0000_0003, 2, 1),
                weights(2, 0.75),
                sub(b"VTEX", &[0; 3]),
            ]
            .concat(),
        );
        let base = write_plugin(
            dir.path(),
            "Base.esm",
            &[],
            0,
            &[land, stat(0x801)].concat(),
        );
        let (merged, warnings) = merge_collecting(&[base]);
        assert_eq!(merged.len(), 2);
        assert_eq!(tags(&merged[&0x800]), ["DATA", "ATXT", "VTXT"]);
        assert_eq!(
            extractors::SubrecordView::new(&merged[&0x800].subrecords).find(b"VTXT"),
            Some(
                [
                    2u16.to_le_bytes().as_slice(),
                    &[0, 0],
                    &0.75f32.to_le_bytes()
                ]
                .concat()
                .as_slice()
            )
        );
        let fields = warnings.skipped_optional.borrow();
        assert_eq!(fields["base.esm"].0, 3, "{:?}", fields["base.esm"]);
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
