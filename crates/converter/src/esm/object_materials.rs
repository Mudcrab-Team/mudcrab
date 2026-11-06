//! Optional per-base appearance projection. This first slice supports STAT MODS diffuse swaps.
//! NIF materials and MATO layers retain their existing owners; no shared mesh is specialized.

use super::{extractors::SubrecordView, records::RawRecord};
use rusqlite::{Connection, Result, Transaction, params};
use std::collections::HashMap;

pub(super) const TABLE: &str = "base_texture_swaps";

/// Checked xEdit MODS layout: count, then (name length, name, TXST, shape index).
/// Reject the whole occurrence instead of applying a valid prefix of a truncated array.
pub(super) fn form_offsets(data: &[u8]) -> Option<Vec<usize>> {
    let count = u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as usize;
    if count > data.len().saturating_sub(4) / 12 {
        return None;
    }
    let mut offset = 4usize;
    let mut offsets = Vec::with_capacity(count);
    for _ in 0..count {
        let length =
            u32::from_le_bytes(data.get(offset..offset.checked_add(4)?)?.try_into().ok()?) as usize;
        offset = offset.checked_add(4)?.checked_add(length)?;
        let end = offset.checked_add(8)?;
        data.get(offset..end)?;
        offsets.push(offset);
        offset = end;
    }
    (offset == data.len()).then_some(offsets)
}

pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS base_texture_swaps (
            base_id INTEGER NOT NULL,
            position INTEGER NOT NULL,
            shape_name TEXT NOT NULL,
            shape_index INTEGER NOT NULL,
            texture_set_id INTEGER NOT NULL,
            diffuse_path TEXT,
            disposition TEXT NOT NULL,
            PRIMARY KEY(base_id, position)
        );",
    )
}

/// Receives winning records after reader-specific FormID remapping. Clear owner rows on every
/// subset update, including removal of MODS, so an old swap cannot survive an override.
pub(super) fn insert(
    tx: &Transaction<'_>,
    master: &HashMap<u32, RawRecord>,
    record: &RawRecord,
    view: &SubrecordView<'_>,
) -> Result<()> {
    tx.execute(
        "DELETE FROM base_texture_swaps WHERE base_id=?1",
        [record.form_id],
    )?;
    if record.is_deleted() || record.record_type != *b"STAT" {
        return Ok(());
    }
    if view
        .find(b"MODS")
        .is_some_and(|data| form_offsets(data).is_none())
        || view.find_all(b"MODS").count() > 1
    {
        log::warn!("STAT {:08X}: malformed MODS omitted", record.form_id);
        return Ok(());
    }
    for (position, swap) in view.get_alternate_textures(b"MODS").iter().enumerate() {
        let target = master
            .get(&swap.texture_form_id)
            .filter(|target| !target.is_deleted() && target.record_type == *b"TXST");
        let target_view = target.map(|target| SubrecordView::new(&target.subrecords));
        let diffuse = target_view
            .as_ref()
            .and_then(|view| view.get_string(b"TX00"))
            .filter(|path| !path.is_empty());
        // Do not claim to replace a full texture set while applying only its diffuse. All eight
        // existing TXST projections and terrain/skin encoding consumers remain unchanged.
        let other_slots = target_view.as_ref().is_some_and(|view| {
            [
                b"TX01", b"TX02", b"TX03", b"TX04", b"TX05", b"TX06", b"TX07",
            ]
            .iter()
            .any(|tag| view.get_string(tag).is_some_and(|path| !path.is_empty()))
        });
        let disposition = if target.is_none() {
            "missing_texture_set"
        } else if diffuse.is_none() {
            "missing_diffuse"
        } else if other_slots {
            "unsupported_texture_slots"
        } else {
            "diffuse_only"
        };
        tx.execute(
            "INSERT INTO base_texture_swaps VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                record.form_id,
                position as u32,
                swap.name_3d,
                swap.index_3d,
                swap.texture_form_id,
                diffuse,
                disposition
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::RecordReader, esm::EsmParser};
    use dummy_content::inhouse_items::{record, text};
    use std::{fs, path::Path};

    fn mods(texture: u32) -> Vec<u8> {
        [
            1u32.to_le_bytes(),
            4u32.to_le_bytes(),
            *b"Rock",
            texture.to_le_bytes(),
            0u32.to_le_bytes(),
        ]
        .concat()
    }

    fn group(tag: &[u8; 4], records: &[Vec<u8>]) -> Vec<u8> {
        let body = records.concat();
        [
            b"GRUP".as_slice(),
            &(24 + body.len() as u32).to_le_bytes(),
            tag,
            &[0; 12],
            &body,
        ]
        .concat()
    }

    fn plugin(
        root: &Path,
        name: &str,
        masters: &[&str],
        flags: u32,
        body: Vec<u8>,
    ) -> std::path::PathBuf {
        let mut fields = vec![(
            b"HEDR" as &[u8; 4],
            [
                1.7f32.to_le_bytes(),
                0u32.to_le_bytes(),
                0x800u32.to_le_bytes(),
            ]
            .concat(),
        )];
        for master in masters {
            fields.push((b"MAST", text(master)));
            fields.push((b"DATA", vec![0; 8]));
        }
        let mut header = record(b"TES4", 0, &fields);
        header[8..12].copy_from_slice(&flags.to_le_bytes());
        let path = root.join(name);
        fs::write(&path, [header, body].concat()).unwrap();
        path
    }

    #[test]
    fn object_diffuse_projection_remaps_full_and_light_override_links_in_both_readers() {
        for light in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let base = plugin(
                root.path(),
                "Base.esm",
                &[],
                0,
                group(
                    b"STAT",
                    &[record(
                        b"STAT",
                        0x800,
                        &[(b"MODL", text("meshes/shared.nif"))],
                    )],
                ),
            );
            let filler = plugin(root.path(), "Filler.esm", &[], 0, vec![]);
            let mut deleted = record(b"TXST", 0x01000804, &[(b"TX00", text("deleted.dds"))]);
            deleted[8..12].copy_from_slice(&0x20u32.to_le_bytes());
            let body = [
                group(
                    b"TXST",
                    &[
                        record(b"TXST", 0x01000801, &[(b"TX00", text("textures/red.dds"))]),
                        deleted,
                    ],
                ),
                group(b"MISC", &[record(b"MISC", 0x01000805, &[])]),
                group(
                    b"STAT",
                    &[
                        record(
                            b"STAT",
                            0x800,
                            &[
                                (b"MODL", text("meshes/shared.nif")),
                                (b"MODS", mods(0x01000801)),
                            ],
                        ),
                        record(
                            b"STAT",
                            0x01000802,
                            &[
                                (b"MODL", text("meshes/shared.nif")),
                                (b"MODS", mods(0x01000801)),
                            ],
                        ),
                        record(
                            b"STAT",
                            0x810,
                            &[
                                (b"MODL", text("meshes/shared.nif")),
                                (b"MODS", mods(0x01000804)),
                            ],
                        ),
                        record(
                            b"STAT",
                            0x811,
                            &[
                                (b"MODL", text("meshes/shared.nif")),
                                (b"MODS", mods(0x01000805)),
                            ],
                        ),
                        record(
                            b"STAT",
                            0x812,
                            &[
                                (b"MODL", text("meshes/shared.nif")),
                                (b"MODS", mods(0x01000FFF)),
                            ],
                        ),
                    ],
                ),
            ]
            .concat();
            let module = plugin(
                root.path(),
                "Mod.esp",
                &["Base.esm"],
                if light { 0x200 } else { 0 },
                body,
            );
            let expected_texture = if light { 0xFE000801u32 } else { 0x02000801u32 };
            let expected_base = if light { 0xFE000802u32 } else { 0x02000802u32 };
            for reader in [RecordReader::Legacy, RecordReader::Inhouse] {
                let database = root.path().join(format!("{reader:?}.db"));
                EsmParser::convert_plugins_with_reader(
                    &[base.clone(), filler.clone(), module.clone()],
                    &database,
                    reader,
                    root.path(),
                )
                .unwrap();
                let conn = Connection::open(database).unwrap();
                for base in [0x800, expected_base] {
                    let row: (u32, String, String) = conn.query_row(
                        "SELECT texture_set_id,diffuse_path,disposition FROM base_texture_swaps WHERE base_id=?1", [base],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
                    assert_eq!(
                        row,
                        (
                            expected_texture,
                            "textures/red.dds".into(),
                            "diffuse_only".into()
                        ),
                        "{reader:?}, light={light}"
                    );
                }
                let invalid: u32 = conn.query_row("SELECT count(*) FROM base_texture_swaps WHERE disposition='missing_texture_set'", [], |row| row.get(0)).unwrap();
                assert_eq!(
                    invalid, 3,
                    "deleted, wrong-type and missing targets: {reader:?}"
                );
            }
        }
    }

    #[test]
    fn object_diffuse_mods_boundaries_reject_partial_arrays() {
        let valid = mods(0x800);
        assert_eq!(form_offsets(&valid), Some(vec![12]));
        for end in 0..valid.len() {
            assert!(form_offsets(&valid[..end]).is_none());
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(form_offsets(&trailing).is_none());
        let mut overflow = valid;
        overflow[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(form_offsets(&overflow).is_none());
    }
}
