//! Optional per-base appearance projection of STAT MODS (alternate texture) entries.
//!
//! Retail applies every entry of a MODS list at reference 3D load, keyed by the entry's 3D Index
//! (RF-TEX-001, RF-TEX-002 in `evidence/rea-20261114/registry.md`). This projection therefore keeps
//! every entry, in record order, with its 3D Index and its TXST link. It applies no gate: the
//! engine resolves the TXST slots from `texture_sets` and decides which shape each index names.

use super::{extractors::SubrecordView, records::RawRecord};
use rusqlite::{Connection, Result, Transaction, params};
use std::collections::HashMap;

/// FormID of the engine's built-in `NullTextureSet`. An entry that names it hides the shape
/// instead of retexturing it (RF-TEX-005; the hide is high confidence, the FormID value medium).
/// The form is created by the executable, so no TXST record exists for it in any plugin; the
/// registered pack holds 66 STAT MODS entries that name it.
pub(super) const NULL_TEXTURE_SET_FORM_ID: u32 = 0x28;

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
            disposition TEXT NOT NULL,
            PRIMARY KEY(base_id, position)
        );",
    )
}

/// The base record types whose world model `export_to_db` publishes in `statics`, so a placed
/// reference can draw one.
const MODEL_BASES: [&[u8; 4]; 25] = [
    b"STAT", b"MSTT", b"FURN", b"TREE", b"FLOR", b"CONT", b"DOOR", b"ACTI", b"TACT", b"WEAP",
    b"MISC", b"BOOK", b"AMMO", b"ALCH", b"INGR", b"SLGM", b"KEYM", b"SCRL", b"ARMO", b"APPA",
    b"ADDN", b"ARTO", b"PROJ", b"HAZD", b"LIGH",
];

/// Whether a record of `record_type` can name alternate textures for its world model.
pub(super) fn is_model_base(record_type: &[u8; 4]) -> bool {
    MODEL_BASES.contains(&record_type)
}

/// Whether `tag` holds alternate textures of a world model of `record_type`. `ARMO` has no
/// `MODL` path: its world models are the male `MOD2` with `MO2S` and the female `MOD4` with
/// `MO4S`. Every other base pairs `MODL` with `MODS`.
pub(super) fn is_world_swap(record_type: &[u8; 4], tag: &[u8]) -> bool {
    if record_type == b"ARMO" {
        tag == b"MO2S" || tag == b"MO4S"
    } else {
        is_model_base(record_type) && tag == b"MODS"
    }
}

/// The alternate textures of the world model the record publishes: for `ARMO` the male slot
/// first and the female slot as its fallback, as the exporter picks the model.
fn world_swap_tag(record_type: &[u8; 4], view: &SubrecordView<'_>) -> &'static [u8; 4] {
    if record_type != b"ARMO" {
        return b"MODS";
    }
    if view
        .get_string(b"MOD2")
        .is_some_and(|path| !path.is_empty())
    {
        b"MO2S"
    } else {
        b"MO4S"
    }
}

/// Receives winning records after reader-specific FormID remapping. Clear owner rows on every
/// subset update, including removal of the swaps, so an old swap cannot survive an override.
///
/// `disposition` is `texture_set` (the target is a live TXST; the engine reads its slots from
/// `texture_sets`), `null_texture_set` (RF-TEX-005) or `missing_texture_set` (the target is
/// deleted, of another type or absent; retail behaviour for a null link is not traced, so the
/// engine reports it and keeps the model's material). `shape_index` keeps the signed 3D Index,
/// so -1 stays -1 (RF-TEX-001 skips it at runtime). `shape_name` is record data only: the
/// retail apply path does not read the 3D Name or its `:N` suffix (RF-TEX-002).
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
    if record.is_deleted() || !is_model_base(&record.record_type) {
        return Ok(());
    }
    let tag = world_swap_tag(&record.record_type, view);
    if view
        .find(tag)
        .is_some_and(|data| form_offsets(data).is_none())
        || view.find_all(tag).count() > 1
    {
        log::warn!(
            "{} {:08X}: malformed {} omitted",
            record.record_type.escape_ascii(),
            record.form_id,
            tag.escape_ascii()
        );
        return Ok(());
    }
    for (position, swap) in view.get_alternate_textures(tag).iter().enumerate() {
        let target = master
            .get(&swap.texture_form_id)
            .filter(|target| !target.is_deleted() && target.record_type == *b"TXST");
        let disposition = if target.is_some() {
            "texture_set"
        } else if swap.texture_form_id == NULL_TEXTURE_SET_FORM_ID {
            "null_texture_set"
        } else {
            "missing_texture_set"
        };
        tx.execute(
            "INSERT INTO base_texture_swaps VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                record.form_id,
                position as u32,
                swap.name_3d,
                swap.index_3d as i32,
                swap.texture_form_id,
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
    fn object_texture_projection_remaps_full_and_light_override_links_in_both_readers() {
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
                    let row: (u32, String) = conn.query_row(
                        "SELECT texture_set_id,disposition FROM base_texture_swaps WHERE base_id=?1", [base],
                        |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
                    assert_eq!(
                        row,
                        (expected_texture, "texture_set".into()),
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

    fn entries(list: &[(&str, u32, i32)]) -> Vec<u8> {
        let mut data = (list.len() as u32).to_le_bytes().to_vec();
        for (name, texture, index) in list {
            data.extend_from_slice(&(name.len() as u32).to_le_bytes());
            data.extend_from_slice(name.as_bytes());
            data.extend_from_slice(&texture.to_le_bytes());
            data.extend_from_slice(&index.to_le_bytes());
        }
        data
    }

    fn raw(id: u32, tag: &[u8; 4], fields: &[(&[u8; 4], Vec<u8>)]) -> RawRecord {
        RawRecord {
            form_id: id,
            record_type: *tag,
            flags: 0,
            subrecords: fields
                .iter()
                .map(|(tag, bytes)| (tag.to_vec(), bytes.clone()))
                .collect(),
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        }
    }

    /// RF-TEX-001, RF-TEX-002, RF-TEX-005: every entry of a list is kept in record order with its
    /// signed 3D Index and its own disposition. The `:N` name suffix is stored but never needed.
    /// The multi-entry shape, the `:N` names and the NullTextureSet link follow retail STATs of the
    /// registered pack: 101534 `RockPileS04:0`, 126810 `RockCliff07:0` and 126829 `RockCliff05:0`
    /// carry one entry each, and 655 of its 1,607 STATs with MODS carry several.
    #[test]
    fn object_texture_projection_keeps_every_entry_and_marks_null_and_missing_targets() {
        let conn = Connection::open_in_memory().unwrap();
        crate::esm::exporter::create_tables(&conn).unwrap();
        let txst = |id| raw(id, b"TXST", &[(b"TX00", b"textures/a.dds\0".to_vec())]);
        let stat = |id, list: &[(&str, u32, i32)]| {
            raw(
                id,
                b"STAT",
                &[
                    (b"MODL", b"meshes/a.nif\0".to_vec()),
                    (b"MODS", entries(list)),
                ],
            )
        };
        let records: HashMap<u32, RawRecord> = [
            txst(0x101),
            txst(0x102),
            // Three real retail shapes (single entry, `:0` suffix), one multi-entry list with a
            // repeated index, a skipped -1 index, a NullTextureSet link and an unresolved link.
            stat(101534, &[("RockPileS04:0", 0x101, 0)]),
            stat(126810, &[("RockCliff07:0", 0x101, 0)]),
            stat(126829, &[("RockCliff05:0", 0x102, 0)]),
            stat(
                7,
                &[
                    ("A:0", 0x101, 2),
                    ("B:1", 0x102, 0),
                    ("C:2", NULL_TEXTURE_SET_FORM_ID, 1),
                    ("D:3", 0x101, -1),
                    ("E:4", 0x102, 2),
                    ("F:5", 0x0000_0FFF, 3),
                ],
            ),
        ]
        .into_iter()
        .map(|record| (record.form_id, record))
        .collect();
        crate::esm::exporter::export_to_db(&conn, &records).unwrap();
        let rows = |base: u32| -> Vec<(i64, String, i32, u32, String)> {
            let mut statement = conn
                .prepare(
                    "SELECT position,shape_name,shape_index,texture_set_id,disposition \
                     FROM base_texture_swaps WHERE base_id=?1 ORDER BY position",
                )
                .unwrap();
            statement
                .query_map([base], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(
            rows(101534),
            [(0, "RockPileS04:0".into(), 0, 0x101, "texture_set".into())]
        );
        assert_eq!(rows(126810).len(), 1);
        assert_eq!(rows(126829)[0].3, 0x102);
        assert_eq!(
            rows(7),
            [
                (0, "A:0".into(), 2, 0x101, "texture_set".into()),
                (1, "B:1".into(), 0, 0x102, "texture_set".into()),
                (2, "C:2".into(), 1, 0x28, "null_texture_set".into()),
                (3, "D:3".into(), -1, 0x101, "texture_set".into()),
                (4, "E:4".into(), 2, 0x102, "texture_set".into()),
                (5, "F:5".into(), 3, 0xFFF, "missing_texture_set".into()),
            ]
        );
    }

    /// SPEC-1 section 2.1: every base with a world model keeps its alternate textures, `ARMO`
    /// from the slot of the model it publishes, and a record without a world model keeps none.
    #[test]
    fn object_texture_projection_covers_every_model_base() {
        let conn = Connection::open_in_memory().unwrap();
        crate::esm::exporter::create_tables(&conn).unwrap();
        let txst = |id| raw(id, b"TXST", &[(b"TX00", b"textures/a.dds\0".to_vec())]);
        let model = || (b"MODL", b"meshes/a.nif\0".to_vec());
        let list = |texture| entries(&[("A:0", texture, 0)]);
        let mut records = vec![txst(0x101), txst(0x102)];
        for (id, tag) in (0x200u32..).zip(MODEL_BASES) {
            if tag == b"ARMO" {
                continue;
            }
            records.push(raw(id, tag, &[model(), (b"MODS", list(0x101))]));
        }
        let world = |path: &str| format!("{path}\0").into_bytes();
        records.push(raw(
            0x300,
            b"ARMO",
            &[
                (b"MOD2", world("meshes/male.nif")),
                (b"MO2S", list(0x101)),
                (b"MOD4", world("meshes/female.nif")),
                (b"MO4S", list(0x102)),
            ],
        ));
        records.push(raw(
            0x301,
            b"ARMO",
            &[
                (b"MOD4", world("meshes/female.nif")),
                (b"MO4S", list(0x102)),
            ],
        ));
        // A head part has no world model of its own.
        records.push(raw(0x302, b"HDPT", &[model(), (b"MODS", list(0x101))]));
        let records: HashMap<u32, RawRecord> = records
            .into_iter()
            .map(|record| (record.form_id, record))
            .collect();
        crate::esm::exporter::export_to_db(&conn, &records).unwrap();
        let sets = |base: u32| -> Vec<u32> {
            conn.prepare(
                "SELECT texture_set_id FROM base_texture_swaps WHERE base_id=?1 ORDER BY position",
            )
            .unwrap()
            .query_map([base], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
        };
        for (id, tag) in (0x200u32..).zip(MODEL_BASES) {
            if tag != b"ARMO" {
                assert_eq!(sets(id), [0x101], "{}", tag.escape_ascii());
            }
        }
        assert_eq!(sets(0x300), [0x101]);
        assert_eq!(sets(0x301), [0x102]);
        assert!(sets(0x302).is_empty());
    }

    #[test]
    fn object_texture_mods_boundaries_reject_partial_arrays() {
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
