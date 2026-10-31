//! Read ordered grass associations without migrating older package databases.

use color_eyre::Result;
use rusqlite::{Connection, OptionalExtension};
use std::collections::{BTreeMap, BTreeSet};

use crate::esm::{extractors::extract_subrecords, types::ArchivedRecordData};

type OrderedGrassLinks = BTreeMap<u32, Vec<u32>>;

/// Read explicit ordinals, or recover legacy order from winning remapped LTEX data.
/// Return the number and first texture with omitted legacy data, retaining usable fields.
pub(super) fn read_links(
    conn: &Connection,
    valid_types: &BTreeSet<u32>,
) -> Result<(OrderedGrassLinks, usize, Option<u32>)> {
    let mut columns = conn.prepare("PRAGMA table_info(landscape_texture_grasses)")?;
    let mut has_ordinal = false;
    for name in columns.query_map([], |row| row.get::<_, String>(1))? {
        has_ordinal |= name? == "ordinal";
    }
    let sql = if has_ordinal {
        "SELECT ltex_id,gras_id,ordinal FROM landscape_texture_grasses ORDER BY ltex_id,ordinal,gras_id"
    } else {
        "SELECT ltex_id,gras_id,-1 FROM landscape_texture_grasses ORDER BY ltex_id,gras_id"
    };
    let mut statement = conn.prepare(sql)?;
    let mut stored = BTreeMap::<u32, Vec<u32>>::new();
    let mut legacy = BTreeSet::new();
    for pair in statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })? {
        let (texture, grass, ordinal) = pair?;
        if valid_types.contains(&grass) {
            stored.entry(texture).or_default().push(grass);
            if ordinal < 0 {
                legacy.insert(texture);
            }
        }
    }
    if legacy.is_empty() {
        return Ok((stored, 0, None));
    }

    let mut raw =
        conn.prepare("SELECT data FROM records WHERE form_id=?1 AND record_type='LTEX'")?;
    let mut skipped = 0;
    let mut first_invalid = None;
    for texture in legacy {
        let grasses = stored.remove(&texture).unwrap_or_default();
        let data: Option<Vec<u8>> = raw.query_row([texture], |row| row.get(0)).optional()?;
        let Some((order, omitted_field)) = data.as_deref().and_then(legacy_order) else {
            skipped += 1;
            first_invalid.get_or_insert(texture);
            continue;
        };
        if omitted_field {
            skipped += 1;
            first_invalid.get_or_insert(texture);
        }
        let stored_ids: BTreeSet<_> = grasses.into_iter().collect();
        let mut seen = BTreeSet::new();
        let ordered: Vec<_> = order
            .into_iter()
            .filter(|grass| {
                stored_ids.contains(grass) && valid_types.contains(grass) && seen.insert(*grass)
            })
            .collect();
        if !ordered.is_empty() {
            stored.insert(texture, ordered);
        }
    }
    Ok((stored, skipped, first_invalid))
}

/// Validate the whole payload before trusting any GNAM order; never publish a prefix.
fn legacy_order(data: &[u8]) -> Option<(Vec<u32>, bool)> {
    if data.is_empty() {
        return None;
    }
    // The exporter stores serialize_subrecords' rkyv archive, while raw plugin
    // payloads use tag/u16/payload framing. Validate either complete representation.
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(data);
    let subs =
        if let Ok(record) = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&aligned) {
            record
                .subrecords
                .into_iter()
                .map(|sub| (sub.tag.to_vec(), sub.data))
                .collect()
        } else {
            extract_subrecords(data).ok()?
        };
    let mut order = Vec::new();
    let mut omitted_field = false;
    for (tag, payload) in subs {
        if tag == b"GNAM" {
            if let Ok(bytes) = <[u8; 4]>::try_from(payload.as_slice()) {
                order.push(u32::from_le_bytes(bytes));
            } else {
                omitted_field = true;
            }
        }
    }
    Some((order, omitted_field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm::{extractors::serialize_subrecords, grass_cache::tests::fixture};

    /// Store real canonical GNAM payloads with deliberately non-FormID order.
    fn set_order(conn: &Connection, texture: u32, order: &[u32]) {
        let subs: Vec<_> = order
            .iter()
            .map(|id| (b"GNAM".to_vec(), id.to_le_bytes().to_vec()))
            .collect();
        conn.execute(
            "INSERT OR REPLACE INTO records(form_id,record_type,load_order,data) VALUES(?1,'LTEX',7,?2)",
            rusqlite::params![texture, serialize_subrecords(&subs)],
        )
        .unwrap();
    }

    /// New exporters use explicit ordinals even if canonical data is unavailable.
    #[test]
    fn explicit_ordinal_remains_authoritative() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        conn.execute_batch("UPDATE landscape_texture_grasses SET ordinal=CASE gras_id WHEN 22 THEN 0 WHEN 999 THEN 1 ELSE 2 END; PRAGMA query_only=ON;").unwrap();
        let (links, skipped, first) = read_links(&conn, &BTreeSet::from([21, 22])).unwrap();
        assert_eq!(links, BTreeMap::from([(30, vec![22, 21])]));
        assert_eq!((skipped, first), (0, None));
    }

    /// Old schemas recover winning source order, intersect links/rules and deduplicate.
    #[test]
    fn legacy_archive_recovers_order_on_read_only_database() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        set_order(&conn, 30, &[21, 22]);
        set_order(&conn, 30, &[22, 999, 22, 77, 21]);
        conn.execute_batch(
            "ALTER TABLE landscape_texture_grasses DROP COLUMN ordinal; PRAGMA query_only=ON;",
        )
        .unwrap();
        let (links, skipped, first) = read_links(&conn, &BTreeSet::from([21, 22, 77])).unwrap();
        assert_eq!(links, BTreeMap::from([(30, vec![22, 21])]));
        assert_eq!((skipped, first), (0, None));
    }

    /// Corrupt framing and absent canonical records omit only their own texture lists.
    #[test]
    fn corrupt_legacy_payload_keeps_valid_neighbor() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        set_order(&conn, 30, &[22, 21]);
        set_order(&conn, 31, &[21, 22]);
        conn.execute_batch("INSERT INTO landscape_texture_grasses(ltex_id,gras_id) VALUES(31,21),(31,22),(32,21); UPDATE records SET data=x'474E414D04001500000045444944040001' WHERE form_id=30; ALTER TABLE landscape_texture_grasses DROP COLUMN ordinal; PRAGMA query_only=ON;").unwrap();
        let (links, skipped, first) = read_links(&conn, &BTreeSet::from([21, 22])).unwrap();
        assert_eq!(links, BTreeMap::from([(31, vec![21, 22])]));
        assert_eq!((skipped, first), (2, Some(30)));
    }

    /// A malformed GNAM field omits only that field, retaining its list and neighbors.
    #[test]
    fn malformed_archived_gnam_keeps_valid_neighbor() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        set_order(&conn, 31, &[22, 21]);
        let data = serialize_subrecords(&[
            (b"GNAM".to_vec(), 22u32.to_le_bytes().to_vec()),
            (b"GNAM".to_vec(), vec![21, 0, 0]),
            (b"GNAM".to_vec(), 21u32.to_le_bytes().to_vec()),
        ]);
        conn.execute("UPDATE records SET data=?1 WHERE form_id=30", [data])
            .unwrap();
        conn.execute_batch("INSERT INTO landscape_texture_grasses(ltex_id,gras_id) VALUES(31,21),(31,22); ALTER TABLE landscape_texture_grasses DROP COLUMN ordinal;").unwrap();
        let (links, skipped, first) = read_links(&conn, &BTreeSet::from([21, 22])).unwrap();
        assert_eq!(
            links,
            BTreeMap::from([(30, vec![22, 21]), (31, vec![22, 21])])
        );
        assert_eq!((skipped, first), (1, Some(30)));
    }

    /// Raw tag/u16/payload records also preserve source order without a FormID sort.
    #[test]
    fn raw_subrecord_order_is_supported() {
        let bytes = b"GNAM\x04\x00\x16\x00\x00\x00GNAM\x04\x00\x15\x00\x00\x00";
        assert_eq!(legacy_order(bytes), Some((vec![22, 21], false)));
        assert_eq!(legacy_order(&bytes[..bytes.len() - 1]), None);
    }

    /// A subset migration marks untouched old rows rather than inventing an order.
    #[test]
    fn migrated_legacy_ordinals_recover_from_winning_payload() {
        let dir = tempfile::tempdir().unwrap();
        let conn = fixture(dir.path());
        set_order(&conn, 30, &[22, 21]);
        conn.execute_batch("ALTER TABLE landscape_texture_grasses DROP COLUMN ordinal")
            .unwrap();
        crate::esm::exporter::create_tables(&conn).unwrap();
        conn.execute_batch("PRAGMA query_only=ON").unwrap();
        let (links, skipped, first) = read_links(&conn, &BTreeSet::from([21, 22])).unwrap();
        assert_eq!(links, BTreeMap::from([(30, vec![22, 21])]));
        assert_eq!((skipped, first), (0, None));
    }
}
