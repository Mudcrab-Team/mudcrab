use crate::esm::{
    binary::{parse_plugin_file, parse_plugin_metadata},
    exporter::{create_tables, export_to_db},
    records::RawRecord,
};
use color_eyre::Result;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
pub mod binary;
pub mod cell_cache;
pub mod exporter;
pub mod extractors;
pub mod mmap_reader;
pub mod records;
pub mod types;

pub struct EsmParser;

impl EsmParser {
    /// Parses .esm files and exports world data to skyrim_world.db
    pub fn convert_plugins(plugin_paths: &[PathBuf], db_path: &Path) -> Result<()> {
        let conn = Connection::open(db_path)?;
        create_tables(&conn)?;
        for (priority, path) in plugin_paths.iter().enumerate() {
            let checksum = Sha256::digest(std::fs::read(path)?);
            conn.execute(
                "INSERT OR REPLACE INTO plugins (id, name, priority, checksum) VALUES (?1, ?2, ?3, ?4)",
                params![priority as i64, path.file_name().unwrap_or_default().to_string_lossy(), priority as i64, checksum.as_slice()],
            )?;
        }
        let master = Self::merge_plugins(plugin_paths)?;
        export_to_db(&conn, &master)?;

        Ok(())
    }

    pub fn merge_plugins(plugin_paths: &[PathBuf]) -> Result<HashMap<u32, RawRecord>> {
        let names: Vec<String> = plugin_paths
            .iter()
            .map(|path| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase()
            })
            .collect();
        let mut normal_indices = HashMap::new();
        let mut light_indices = HashMap::new();
        let mut next_normal = 0u32;
        let mut next_light = 0u32;
        for (path, name) in plugin_paths.iter().zip(&names) {
            let metadata = parse_plugin_metadata(path)?;
            if path
                .extension()
                .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("esl"))
                || metadata.flags & 0x0000_0200 != 0
            {
                light_indices.insert(name.clone(), next_light);
                next_light += 1;
            } else {
                normal_indices.insert(name.clone(), next_normal);
                next_normal += 1;
            }
        }
        let mut merged = HashMap::new();
        for (priority, path) in plugin_paths.iter().enumerate() {
            let metadata = parse_plugin_metadata(path)?;
            for mut record in parse_plugin_file(path)? {
                record.load_order = priority as u32;
                remap_record_form_ids(
                    &mut record,
                    &names[priority],
                    &metadata.masters,
                    &normal_indices,
                    &light_indices,
                )?;
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

pub fn read_plugins_txt(path: &Path, data_dir: &Path) -> Result<Vec<PathBuf>> {
    let contents = std::fs::read_to_string(path)?;
    let mut plugins = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let enabled = line.starts_with('*');
        let name = line.strip_prefix('*').unwrap_or(line).trim();
        if !matches!(
            Path::new(name)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase())
                .as_deref(),
            Some("esm" | "esp" | "esl")
        ) {
            continue;
        }
        if !enabled && !name.to_ascii_lowercase().ends_with(".esm") {
            continue;
        }
        let exact = data_dir.join(name);
        if exact.is_file() {
            plugins.push(exact);
            continue;
        }
        if let Some(found) = std::fs::read_dir(data_dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|candidate| {
                candidate
                    .file_name()
                    .is_some_and(|file| file.to_string_lossy().eq_ignore_ascii_case(name))
            })
        {
            plugins.push(found);
        } else {
            color_eyre::eyre::bail!("active plugin not found: {name}");
        }
    }
    Ok(plugins)
}

/// Determines whether a subrecord within a given parent record type represents
/// a 32-bit FormID that requires load-order remapping.
///
/// This record-aware check ensures that subrecords with shared tag names (such as
/// `CNAM` or `SNAM`) containing strings (e.g. `TES4` author/description), float
/// physics arrays (`TREE` trunk flexibility), or RGBA color structures (`CLFM`/`AACT`)
/// are not inadvertently overwritten as 4-byte FormIDs.
fn is_form_id_subrecord(record_type: &[u8; 4], tag: &[u8], len: usize) -> bool {
    if len != 4 || tag.len() < 4 {
        return false;
    }
    let tag_4: &[u8; 4] = tag[..4].try_into().unwrap();
    match (record_type, tag_4) {
        (b"TES4" | b"CLFM" | b"AACT", _) => false,
        (b"TREE", b"CNAM") => false,
        (b"TREE", b"SNAM" | b"PFIG") if len == 4 => true,
        (b"WRLD", b"WNAM" | b"CNAM" | b"RNAM" | b"TNAM") if len == 4 => true,
        (b"CELL", b"XOWN" | b"XGLB" | b"XEZN" | b"XLCN" | b"XLRL") if len == 4 => true,
        (b"NPC_", b"RNAM" | b"CNAM" | b"INAM") if len == 4 => true,
        (b"RACE", b"WKMV" | b"RNMV") if len == 4 => true,
        (b"NPC_", b"SNAM") if len >= 4 => true,
        (
            b"REFR" | b"ACHR" | b"ACRE" | b"PGRE" | b"PMIS",
            b"NAME" | b"XOWN" | b"XGLB" | b"XEZN" | b"XLCN" | b"XLRL",
        ) if len == 4 => true,
        (_, b"XOWN" | b"XGLB" | b"XEZN" | b"XLCN" | b"XLRL") if len == 4 => true,
        _ => false,
    }
}

/// Remaps local FormIDs within a record header, parent cell/worldspace references,
/// and relevant subrecords according to master plugin load-order indices.
fn remap_record_form_ids(
    record: &mut RawRecord,
    plugin_name: &str,
    masters: &[String],
    normal_indices: &HashMap<String, u32>,
    light_indices: &HashMap<String, u32>,
) -> Result<()> {
    let remap = |form_id: u32| -> Result<u32> {
        if form_id == 0 {
            return Ok(0);
        }
        let local_index = (form_id >> 24) as usize;
        let owner = if local_index < masters.len() {
            masters[local_index].to_ascii_lowercase()
        } else {
            plugin_name.to_owned()
        };
        if let Some(index) = light_indices.get(&owner) {
            return Ok(0xFE00_0000 | (index << 12) | (form_id & 0xFFF));
        }
        let index = normal_indices.get(&owner).ok_or_else(|| {
            color_eyre::eyre::eyre!("master {owner} is not present in load order")
        })?;
        Ok((index << 24) | (form_id & 0x00FF_FFFF))
    };
    record.form_id = remap(record.form_id)?;
    record.cell_form_id = record.cell_form_id.map(&remap).transpose()?;
    record.worldspace_form_id = record.worldspace_form_id.map(&remap).transpose()?;
    for (tag, data) in &mut record.subrecords {
        if tag.as_slice() == b"VMAD" {
            records::record_type::vmad::remap_primary_form_ids(data, &remap)?;
            continue;
        }
        if is_form_id_subrecord(&record.record_type, tag, data.len()) {
            let value = u32::from_le_bytes(data[..4].try_into().unwrap());
            data[..4].copy_from_slice(&remap(value)?.to_le_bytes());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        )
        .unwrap();
        assert_eq!(race.form_id, 0x0301_3746);
        assert_eq!(
            u32::from_le_bytes(race.subrecords[0].1[..4].try_into().unwrap()),
            0x0300_1234
        );
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
