//! WRLD.RNAM projection from G01 of https://github.com/Mudcrab-Team/skyrim_decomp/pull/821.

use super::{load_order::LoadOrder, records::RawRecord};
use crate::records::source_identity::{header_runtime_id, resolve};
use rusqlite::{Connection, Result, params};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const TABLE_SQL: &str = "
    CREATE TABLE IF NOT EXISTS large_references (
        worldspace_id INTEGER NOT NULL,
        grid_x INTEGER NOT NULL,
        grid_y INTEGER NOT NULL,
        reference_id INTEGER NOT NULL,
        PRIMARY KEY(worldspace_id, grid_x, grid_y, reference_id)
    );
    CREATE TABLE IF NOT EXISTS large_reference_omissions (
        worldspace_id INTEGER NOT NULL,
        grid_x INTEGER,
        grid_y INTEGER,
        omitted_count INTEGER NOT NULL CHECK(omitted_count > 0),
        CHECK((grid_x IS NULL) = (grid_y IS NULL))
    )";

type Member = (i32, i32, u32);
type OmissionScope = (u32, Option<(i32, i32)>);

#[derive(Debug, Default)]
pub(crate) struct LargeReferences {
    members: BTreeSet<(u32, Member)>,
    omissions: BTreeMap<OmissionScope, usize>,
    malformed: usize,
    unresolved: usize,
    overrides: usize,
}

fn word(bytes: &[u8]) -> Option<u32> {
    // silent-ok: the caller counts an invalid RNAM byte length.
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn cell_key(bytes: &[u8]) -> Option<(i32, i32)> {
    let [lo_y, hi_y, lo_x, hi_x] = *bytes.first_chunk::<4>()?;
    Some((
        i32::from(i16::from_le_bytes([lo_x, hi_x])),
        i32::from(i16::from_le_bytes([lo_y, hi_y])),
    ))
}

fn members(bytes: &[u8]) -> Option<impl Iterator<Item = Member> + '_> {
    let (header, tuples) = bytes.split_at_checked(8)?;
    let (key, count) = header.split_at(4);
    // silent-ok: the caller counts an unrepresentable RNAM tuple count.
    let count = usize::try_from(word(count)?).ok()?;
    if count.checked_mul(8)? != tuples.len() {
        return None;
    }
    let (x, y) = cell_key(key)?;
    Some(tuples.as_chunks::<8>().0.iter().filter_map(move |tuple| {
        let (form, placement) = tuple.split_at(4);
        (placement == key).then_some((x, y, word(form)?))
    }))
}

impl LargeReferences {
    pub(crate) fn collect(
        &mut self,
        record: &RawRecord,
        source_id: u32,
        supplier: usize,
        order: &LoadOrder,
    ) {
        let source = resolve(b"WRLD", source_id, supplier, order);
        let defining = source.original_owner.as_deref()
            == Some(super::load_order::normalize_plugin_name(&order.names[supplier]).as_str());
        for (tag, bytes) in &record.subrecords {
            if tag != b"RNAM" {
                continue;
            }
            // STAND-IN(#913): Cross-plugin RNAM precedence is unverified. Publish defining
            // memberships and count omitted override subrecords.
            if !defining {
                self.overrides += 1;
                *self
                    .omissions
                    .entry((record.form_id, cell_key(bytes)))
                    .or_default() += 1;
                continue;
            }
            let Some(members) = members(bytes) else {
                self.malformed += 1;
                *self
                    .omissions
                    .entry((record.form_id, cell_key(bytes)))
                    .or_default() += 1;
                continue;
            };
            for (x, y, raw_id) in members {
                match header_runtime_id(b"REFR", raw_id, supplier, order) {
                    Some(id) if id != 0 => {
                        self.members.insert((record.form_id, (x, y, id)));
                    }
                    _ => {
                        self.unresolved += 1;
                        *self
                            .omissions
                            .entry((record.form_id, Some((x, y))))
                            .or_default() += 1;
                    }
                }
            }
        }
    }

    pub(super) fn write(&self, conn: &Connection) -> Result<()> {
        let mut insert = conn.prepare_cached(
            "INSERT INTO large_references
             SELECT ?1,?2,?3,?4
             WHERE EXISTS (SELECT 1 FROM worldspaces WHERE id=?1)
               AND EXISTS (SELECT 1 FROM \"references\" WHERE id=?4)",
        )?;
        let mut missing = 0;
        let mut omissions = self.omissions.clone();
        for &(world, (x, y, id)) in &self.members {
            if insert.execute(params![world, x, y, id])? == 0 {
                missing += 1;
                *omissions.entry((world, Some((x, y)))).or_default() += 1;
            }
        }
        let mut insert_omission = conn.prepare_cached(
            "INSERT INTO large_reference_omissions(worldspace_id,grid_x,grid_y,omitted_count)
             VALUES (?1,?2,?3,?4)",
        )?;
        for ((world, cell), count) in omissions {
            insert_omission.execute(params![
                world,
                cell.map(|(x, _)| x),
                cell.map(|(_, y)| y),
                count
            ])?;
        }
        if self.malformed + self.unresolved + self.overrides + missing > 0 {
            eprintln!(
                "large references: omitted {} malformed RNAMs, {} unresolved FormIDs, \
                 {} override RNAMs, and {missing} members without a projected world or reference",
                self.malformed, self.unresolved, self.overrides
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rnam(key: [u8; 4], tuples: &[(u32, [u8; 4])]) -> Vec<u8> {
        let mut bytes = key.to_vec();
        bytes.extend(u32::try_from(tuples.len()).unwrap().to_le_bytes());
        for (id, placement) in tuples {
            bytes.extend(id.to_le_bytes());
            bytes.extend(placement);
        }
        bytes
    }

    #[test]
    fn only_equal_keys_qualify_and_both_coordinates_are_signed() {
        let key = [0xfe, 0xff, 0xfd, 0xff];
        let bytes = rnam(key, &[(7, key), (8, [0; 4]), (7, key)]);
        assert_eq!(
            members(&bytes).unwrap().collect::<Vec<_>>(),
            [(-3, -2, 7); 2]
        );
        assert!(members(&bytes[..bytes.len() - 1]).is_none());
        assert!(members(&bytes[..7]).is_none());
    }

    #[test]
    fn remaps_in_supplier_context_and_persists_projection_omissions() {
        use super::super::binary::PluginMetadata;
        let order = LoadOrder::from_metadata(
            vec!["Base.esm".into(), "Other.esm".into(), "World.esm".into()],
            vec![
                PluginMetadata::default(),
                PluginMetadata::default(),
                PluginMetadata {
                    masters: vec!["Other.esm".into()],
                    ..Default::default()
                },
            ],
        )
        .unwrap();
        let key = [0; 4];
        let mut record = RawRecord {
            form_id: 0x0200_0001,
            record_type: *b"WRLD",
            flags: 0,
            subrecords: vec![(
                b"RNAM".to_vec(),
                rnam(key, &[(7, key), (7, key), (8, key), (0x0300_0007, key)]),
            )],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 2,
        };
        let mut projection = LargeReferences::default();
        projection.collect(&record, 0x0100_0001, 2, &order);
        assert_eq!(
            projection.members,
            [
                (record.form_id, (0, 0, 0x0100_0007)),
                (record.form_id, (0, 0, 0x0100_0008))
            ]
            .into()
        );
        assert_eq!(projection.unresolved, 1);
        projection.collect(&record, 1, 2, &order);
        assert_eq!(projection.overrides, 1);
        record.subrecords = vec![
            (b"RNAM".to_vec(), vec![0; 7]),
            (b"RNAM".to_vec(), vec![0; 3]),
        ];
        projection.collect(&record, 0x0100_0001, 2, &order);
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            r#"CREATE TABLE worldspaces(id INTEGER PRIMARY KEY);
               INSERT INTO worldspaces VALUES(33554433);
               CREATE TABLE "references"(id INTEGER PRIMARY KEY);
               INSERT INTO "references" VALUES(16777223);"#,
        )
        .unwrap();
        db.execute_batch(TABLE_SQL).unwrap();
        projection.write(&db).unwrap();
        assert_eq!(
            db.query_row("SELECT reference_id FROM large_references", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            0x0100_0007
        );
        let mut query = db.prepare("SELECT worldspace_id,grid_x,grid_y,omitted_count FROM large_reference_omissions ORDER BY grid_x").unwrap();
        let counts = query
            .query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, Option<i32>>(1)?,
                    row.get::<_, Option<i32>>(2)?,
                    row.get::<_, usize>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            counts,
            [
                (record.form_id, None, None, 1),
                (record.form_id, Some(0), Some(0), 4)
            ]
        );
    }
}
