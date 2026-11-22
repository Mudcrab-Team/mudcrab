//! WRLD.RNAM projection from G01 of https://github.com/Mudcrab-Team/skyrim_decomp/pull/821.

use super::{load_order::LoadOrder, records::RawRecord};
use crate::records::source_identity::{header_runtime_id, resolve};
use rusqlite::{Connection, Result, params};
use std::collections::BTreeSet;

pub(super) const TABLE_SQL: &str = "
    CREATE TABLE IF NOT EXISTS large_references (
        worldspace_id INTEGER NOT NULL,
        grid_x INTEGER NOT NULL,
        grid_y INTEGER NOT NULL,
        reference_id INTEGER NOT NULL,
        PRIMARY KEY(worldspace_id, grid_x, grid_y, reference_id)
    )";

type Member = (i32, i32, u32);

#[derive(Debug, Default)]
pub(crate) struct LargeReferences {
    members: BTreeSet<(u32, Member)>,
    malformed: usize,
    unresolved: usize,
    overrides: usize,
}

fn word(bytes: &[u8]) -> Option<u32> {
    // silent-ok: the caller counts an invalid RNAM byte length.
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn members(bytes: &[u8]) -> Option<impl Iterator<Item = (i32, i32, u32)> + '_> {
    let (header, tuples) = bytes.split_at_checked(8)?;
    let (key, count) = header.split_at(4);
    // silent-ok: the caller counts an unrepresentable RNAM tuple count.
    let count = usize::try_from(word(count)?).ok()?;
    if count.checked_mul(8)? != tuples.len() {
        return None;
    }
    // silent-ok: the caller counts malformed RNAM headers.
    let [lo_y, hi_y, lo_x, hi_x] = <[u8; 4]>::try_from(key).ok()?;
    let x = i32::from(i16::from_le_bytes([lo_x, hi_x]));
    let y = i32::from(i16::from_le_bytes([lo_y, hi_y]));
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
                continue;
            }
            let Some(members) = members(bytes) else {
                self.malformed += 1;
                continue;
            };
            for (x, y, raw_id) in members {
                match header_runtime_id(b"REFR", raw_id, supplier, order) {
                    Some(id) if id != 0 => {
                        self.members.insert((record.form_id, (x, y, id)));
                    }
                    _ => self.unresolved += 1,
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
        for &(world, (x, y, id)) in &self.members {
            missing += 1 - insert.execute(params![world, x, y, id])?;
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
    fn remaps_in_supplier_context_and_deduplicates() {
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
        let record = RawRecord {
            form_id: 0x0200_0001,
            record_type: *b"WRLD",
            flags: 0,
            subrecords: vec![(
                b"RNAM".to_vec(),
                rnam(key, &[(7, key), (7, key), (0x0300_0007, key)]),
            )],
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 2,
        };
        let mut projection = LargeReferences::default();
        projection.collect(&record, 0x0100_0001, 2, &order);
        assert_eq!(
            projection.members,
            [(record.form_id, (0, 0, 0x0100_0007))].into()
        );
        assert_eq!(projection.unresolved, 1);
        projection.collect(&record, 1, 2, &order);
        assert_eq!(projection.overrides, 1);
    }
}
