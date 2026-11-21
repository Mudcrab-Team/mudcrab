//! Placed-reference links, activate parents, ownership, locks and map markers (#269 gap 3).
//!
//! Projected from all canonical winning placed records, so the legacy
//! and in-house readers publish the same rows (db-schema.md, "Reference Links and Details").
//! Layouts follow xEdit 4.1.5f `Core/wbDefinitionsTES5.pas` and the in-house schema.

use super::{
    extractors::{SubrecordView, serialize_subrecords},
    load_order::LoadOrder,
    records::RawRecord,
    types::ArchivedRecordData,
};
use crate::records::{DecodedRecord, framed_source_prefix, history::stored_order, source_identity};
use rusqlite::{OptionalExtension, Result, Transaction, params};
use std::collections::{HashMap, HashSet};

pub(super) const SUBSET_NONPLACEMENT_DELETIONS_SQL: &str =
    "CREATE TABLE IF NOT EXISTS subset_nonplacement_deletions (form_id INTEGER PRIMARY KEY)";

/// Tables this module owns. A complete export drops and recreates them.
pub(crate) const REFERENCE_LINK_TABLES: [&str; 3] = [
    "reference_linked_refs",
    "reference_activate_parents",
    "reference_details",
];

pub(crate) const REFERENCE_LINK_TABLES_SQL: &str = "
CREATE TABLE IF NOT EXISTS reference_linked_refs (
    reference_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    keyword_id INTEGER,
    linked_ref_id INTEGER,
    PRIMARY KEY (reference_id, position)
);
CREATE INDEX IF NOT EXISTS idx_reference_linked_refs_target ON reference_linked_refs(linked_ref_id);
CREATE TABLE IF NOT EXISTS reference_activate_parents (
    reference_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    parent_ref_id INTEGER,
    delay REAL,
    PRIMARY KEY (reference_id, position)
);
CREATE INDEX IF NOT EXISTS idx_reference_activate_parents_parent ON reference_activate_parents(parent_ref_id);
CREATE TABLE IF NOT EXISTS reference_details (
    reference_id INTEGER PRIMARY KEY,
    owner_id INTEGER,
    owner_rank INTEGER,
    activate_parent_flags INTEGER,
    lock_level INTEGER,
    lock_key_id INTEGER,
    lock_flags INTEGER,
    open_by_default INTEGER NOT NULL DEFAULT 0,
    map_marker INTEGER NOT NULL DEFAULT 0,
    map_marker_flags INTEGER,
    map_marker_type INTEGER,
    location_id INTEGER,
    location_ref_type_id INTEGER,
    encounter_zone_id INTEGER,
    attached_ref_id INTEGER
);
";

/// Record types an XLKR reference or XAPR parent may name.
const LINKED_REFERENCE: &[&[u8; 4]] = &[
    b"PLYR", b"ACHR", b"REFR", b"PGRE", b"PHZD", b"PMIS", b"PARW", b"PBAR", b"PBEA", b"PCON",
    b"PFLA",
];

/// XLKR's first field shares the linked-reference targets as well as accepting keywords.
const LINKED_KEYWORD: &[&[u8; 4]] = &[
    b"KYWD", b"PLYR", b"ACHR", b"REFR", b"PGRE", b"PHZD", b"PMIS", b"PARW", b"PBAR", b"PBEA",
    b"PCON", b"PFLA",
];

/// Record types an XATR attached reference may name.
const ATTACHED_REFERENCE: &[&[u8; 4]] = &[
    b"REFR", b"PGRE", b"PHZD", b"PMIS", b"PARW", b"PBAR", b"PBEA", b"PCON", b"PFLA",
];

const OWNER: &[&[u8; 4]] = &[b"FACT", b"ACHR", b"NPC_"];

enum StoredTarget {
    Absent,
    Unavailable,
    Live([u8; 4]),
}

fn stored_target(tx: &Transaction<'_>, id: u32) -> Result<StoredTarget> {
    let (kind, placed, deleted): (Option<String>, bool, bool) = tx
        .prepare_cached(
            "SELECT (SELECT record_type FROM records WHERE form_id=?1),
                    EXISTS(SELECT 1 FROM [references] WHERE id=?1),
                    EXISTS(SELECT 1 FROM subset_nonplacement_deletions WHERE form_id=?1)",
        )?
        .query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    Ok(match (deleted, kind, placed) {
        (true, _, _) => StoredTarget::Unavailable,
        (false, Some(kind), placed) => match <[u8; 4]>::try_from(kind.as_bytes()) {
            Ok(kind) if placed || !super::is_placed_record_type(&kind) => StoredTarget::Live(kind),
            _ => StoredTarget::Unavailable,
        },
        // A placement import can add a REFR without modifying the canonical archive.
        (false, None, true) => StoredTarget::Live(*b"REFR"),
        (false, None, false) => StoredTarget::Absent,
    })
}

pub(super) fn accepts_link_target(kind: &[u8; 4]) -> bool {
    LINKED_KEYWORD.contains(&kind) || OWNER.contains(&kind)
}

pub(super) fn target_kind_changed(
    tx: &Transaction<'_>,
    id: u32,
    replacement: &[u8; 4],
) -> Result<bool> {
    Ok(match stored_target(tx, id)? {
        StoredTarget::Live(kind) => {
            kind != *replacement && (accepts_link_target(&kind) || accepts_link_target(replacement))
        }
        StoredTarget::Absent if id < 0x800 => true,
        _ => accepts_link_target(replacement),
    })
}

/// What the projection needs beyond the record itself.
pub(crate) struct Context<'a> {
    pub(crate) master: &'a HashMap<u32, RawRecord>,
    /// The load order, to remap the `XLOC` key that neither reader remaps; `None` in subset
    /// exports, where the key stays NULL.
    pub(crate) order: Option<&'a LoadOrder>,
    pub(crate) subset: bool,
    pub(crate) deleted_ids: Option<&'a HashSet<u32>>,
}

impl Context<'_> {
    /// Keep typed winning records and hardcoded nonzero IDs without plugin records. The in-house
    /// reader clears invalid links and the legacy reader keeps them, so this makes both agree.
    fn resolve(
        &self,
        tx: &Transaction<'_>,
        id: Option<u32>,
        targets: &[&[u8; 4]],
    ) -> Result<Option<u32>> {
        let Some(id) = id.filter(|&id| id != 0) else {
            return Ok(None);
        };
        if let Some(record) = self.master.get(&id) {
            if record.is_deleted() || !targets.contains(&&record.record_type) {
                return Ok(None);
            }
            if !self.subset || !super::is_placed_record_type(&record.record_type) {
                return Ok(Some(id));
            }
        }
        if self.subset {
            match stored_target(tx, id)? {
                StoredTarget::Unavailable => return Ok(None),
                StoredTarget::Live(kind) => return Ok(targets.contains(&&kind).then_some(id)),
                StoredTarget::Absent => {}
            }
            if id < 0x800 {
                // Subset deletions remove the placement and archive row. Owning identities
                // survive; edits without an identity leave a separate marker.
                let known: bool = tx
                    .prepare_cached(
                        "SELECT EXISTS(
                             SELECT 1 FROM formid_map WHERE form_id=?1
                             UNION ALL
                             SELECT 1 FROM subset_placement_deletions WHERE form_id=?1
                         )",
                    )?
                    .query_row([id], |row| row.get(0))?;
                if known {
                    return Ok(None);
                }
            }
        }
        Ok((id < 0x800 && !self.deleted_ids.is_some_and(|ids| ids.contains(&id))).then_some(id))
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

/// The subrecord when its length is one the in-house schema accepts; the in-house reader leaves a
/// field of another length out of its runtime projection, so both readers skip it here.
fn sized<'a>(bytes: Option<&'a [u8]>, sizes: &[usize]) -> Option<&'a [u8]> {
    bytes.filter(|bytes| sizes.contains(&bytes.len()))
}

fn linked_refs<'a>(
    fields: impl Iterator<Item = &'a [u8]>,
) -> impl Iterator<Item = (Option<u32>, Option<u32>)> {
    fields.filter_map(|field| match field.len() {
        4 => Some((None, u32_at(field, 0))),
        8 => Some((u32_at(field, 0), u32_at(field, 4))),
        _ => None,
    })
}

fn activate_parents<'a>(fields: impl Iterator<Item = &'a [u8]>) -> impl Iterator<Item = &'a [u8]> {
    fields.filter(|field| field.len() == 8)
}

pub(super) fn accepts_placement_target(kind: &[u8]) -> bool {
    LINKED_REFERENCE
        .iter()
        .any(|target| target.as_slice() == kind)
}

pub(super) fn has_live_target(tx: &Transaction<'_>, id: u32) -> Result<bool> {
    let kind: Option<String> = tx
        .prepare_cached(
            "SELECT coalesce(raw.record_type,'REFR')
             FROM [references] r LEFT JOIN records raw ON raw.form_id=r.id WHERE r.id=?1",
        )?
        .query_row([id], |row| row.get(0))
        .optional()?;
    Ok(kind.is_some_and(|kind| accepts_placement_target(kind.as_bytes())))
}

// Subset exports preserve the original conversion's provenance. A row marks a
// replacement, with recovered runtime link words when it came from a typed source.
pub(super) const SUBSET_SOURCES_SQL: &str = "
    CREATE TABLE IF NOT EXISTS reference_link_subset_sources (
        form_id INTEGER PRIMARY KEY, data BLOB
    );
";

pub(super) fn forget_subset_source(tx: &Transaction<'_>, id: u32) -> Result<()> {
    tx.prepare_cached("DELETE FROM reference_link_subset_sources WHERE form_id=?1")?
        .execute([id])?;
    Ok(())
}

/// Recover authored words for surviving canonical occurrences. The typed decoder
/// drops malformed widths and nonfinite delays. For other dropped occurrences,
/// compare the earliest and latest possible order-preserving matches.
fn authored_link_fields<'a>(
    canonical: impl Iterator<Item = ([u8; 4], &'a [u8])> + Clone,
    payload: &[u8],
    supplier: usize,
    order: &LoadOrder,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut restored = Vec::new();
    let mut native = None;
    for (tag, widths) in [
        (*b"XLKR", &[4, 8][..]),
        (*b"XAPR", &[8][..]),
        (*b"XOWN", &[4][..]),
        (*b"XATR", &[4][..]),
    ] {
        let canonical: Vec<_> = canonical
            .clone()
            .filter_map(|(sig, data)| (sig == tag && widths.contains(&data.len())).then_some(data))
            .collect();
        if canonical.is_empty() {
            continue;
        }
        let word_end = |data: &[u8]| if tag == *b"XAPR" { 4 } else { data.len() };
        let authored: Vec<_> = native
            .get_or_insert_with(|| framed_source_prefix(payload))
            .iter()
            .filter_map(|&(sig, bytes)| {
                if sig != tag || !widths.contains(&bytes.len()) {
                    return None;
                }
                if tag == *b"XAPR" && !f32::from_bits(u32_at(bytes, 4)?).is_finite() {
                    return None;
                }
                let mut data = bytes.to_vec();
                for word in data[..word_end(bytes)].as_chunks_mut::<4>().0 {
                    let raw = u32::from_le_bytes(*word);
                    let mapped = source_identity::header_runtime_id(b"REFR", raw, supplier, order)
                        .unwrap_or(0);
                    *word = mapped.to_le_bytes();
                }
                Some(data)
            })
            .collect();
        let compatible = |projected: &[u8], source: &[u8]| {
            projected.len() == source.len()
                && projected[word_end(projected)..] == source[word_end(source)..]
                && (0..word_end(projected)).step_by(4).all(|offset| {
                    u32_at(projected, offset) == Some(0)
                        || u32_at(projected, offset) == u32_at(source, offset)
                })
        };
        let mut occurrences = authored.iter().enumerate();
        let earliest: Option<Vec<_>> = canonical
            .iter()
            .map(|projected| {
                occurrences
                    .find(|(_, source)| compatible(projected, source))
                    .map(|(index, _)| index)
            })
            .collect();
        let mut occurrences = authored.iter().enumerate().rev();
        let latest: Option<Vec<_>> = canonical
            .iter()
            .rev()
            .map(|projected| {
                occurrences
                    .find(|(_, source)| compatible(projected, source))
                    .map(|(index, _)| index)
            })
            .collect();
        for (position, projected) in canonical.into_iter().enumerate() {
            let data = match (&earliest, &latest) {
                (Some(first), Some(last)) if first[position] == last[last.len() - 1 - position] => {
                    authored[first[position]].clone()
                }
                _ => projected.to_vec(),
            };
            restored.push((tag.to_vec(), data));
        }
    }
    restored
}

fn has_table(tx: &Transaction<'_>, name: &str) -> Result<bool> {
    tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)")?
        .query_row([name], |row| row.get(0))
}

/// Refresh only the target columns. In particular, a source outside the edit retains its delay,
/// other links and details (including any XLOC key resolved by the full export).
pub(super) fn refresh_incoming(
    tx: &Transaction<'_>,
    unavailable: &[u32],
    available: &HashSet<u32>,
) -> Result<()> {
    if unavailable.is_empty() && available.is_empty() {
        return Ok(());
    }
    // Placement-layer additions may have only a placement row. A restored keyword
    // can fill the first XLKR word, while an actor cannot fill XATR.
    let mut kinds = HashMap::new();
    let mut kind = tx.prepare_cached(
        "SELECT coalesce((SELECT record_type FROM records WHERE form_id=?1),'REFR')",
    )?;
    for &id in available {
        if id != 0 {
            let name: String = kind.query_row([id], |row| row.get(0))?;
            if let Ok(signature) = <[u8; 4]>::try_from(name.as_bytes()) {
                kinds.insert(id, signature);
            }
        }
    }
    // A direct kind change invalidates only the fields that reject its new type.
    // Compatible values can survive even in packs without recoverable source bytes.
    for (table, column, allowed) in [
        ("reference_linked_refs", "linked_ref_id", LINKED_REFERENCE),
        ("reference_linked_refs", "keyword_id", LINKED_KEYWORD),
        (
            "reference_activate_parents",
            "parent_ref_id",
            LINKED_REFERENCE,
        ),
        ("reference_details", "attached_ref_id", ATTACHED_REFERENCE),
        ("reference_details", "owner_id", OWNER),
    ] {
        let invalid: Vec<_> = unavailable
            .iter()
            .copied()
            .chain(
                kinds
                    .iter()
                    .filter_map(|(&id, kind)| (!allowed.contains(&kind)).then_some(id)),
            )
            .collect();
        if !invalid.is_empty() {
            tx.prepare_cached(&format!(
                "UPDATE {table} SET {column}=NULL WHERE {column} IN (SELECT value FROM json_each(?1))"
            ))?
            .execute([serde_json::json!(invalid).to_string()])?;
        }
    }
    if kinds.is_empty() {
        return Ok(());
    }
    let restored_target = |value: Option<u32>, allowed: &[&[u8; 4]]| {
        value.filter(|id| kinds.get(id).is_some_and(|kind| allowed.contains(&kind)))
    };

    let layered = if has_table(tx, "room_placement_edits")? {
        "EXISTS(SELECT 1 FROM room_placement_edits WHERE form_id=r.id)"
    } else {
        "0"
    };
    let order = if has_table(tx, "inhouse_source_records")? && has_table(tx, "record_overrides")? {
        let (order, _) = stored_order(tx).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                error.to_string(),
            )))
        })?;
        let plugin_count: usize =
            tx.query_row("SELECT count(*) FROM plugins", [], |row| row.get(0))?;
        (order.names.len() == plugin_count).then_some(order)
    } else {
        None
    };
    let (native_join, native_data) = if order.is_some() {
        (
            format!(
                "LEFT JOIN inhouse_source_records native ON native.form_id=r.id
          AND native.load_order=raw.load_order AND native.record_type=raw.record_type
          AND subset.form_id IS NULL AND NOT ({layered})"
            ),
            "native.payload, native.load_order, native.source_form_id, native.record_type",
        )
    } else {
        (String::new(), "NULL, NULL, NULL, NULL")
    };
    // A source with only an unresolved detail can have no detail row.
    // Materialize the candidate IDs before updating these same tables.
    let mut sources = tx.prepare(&format!(
        "WITH candidates(id) AS MATERIALIZED (
             SELECT reference_id FROM reference_linked_refs WHERE ?1 AND linked_ref_id IS NULL
             UNION SELECT reference_id FROM reference_linked_refs WHERE ?2 AND keyword_id IS NULL
             UNION SELECT reference_id FROM reference_activate_parents WHERE ?1 AND parent_ref_id IS NULL
             UNION SELECT reference_id FROM reference_details
                   WHERE (?3 AND attached_ref_id IS NULL) OR (?4 AND owner_id IS NULL)
             UNION SELECT id FROM [references] r WHERE (?3 OR ?4)
                   AND NOT EXISTS(SELECT 1 FROM reference_details WHERE reference_id=r.id)
         )
         SELECT r.id,
                CASE WHEN subset.form_id IS NOT NULL THEN coalesce(subset.data,r.data,raw.data)
                     WHEN {layered} THEN r.data ELSE coalesce(raw.data,r.data) END,
                {native_data},
                CASE WHEN subset.data IS NOT NULL THEN coalesce(r.data,raw.data) END
         FROM candidates JOIN [references] r ON r.id=candidates.id
         LEFT JOIN records raw ON raw.form_id=r.id
         LEFT JOIN reference_link_subset_sources subset ON subset.form_id=r.id
         {native_join}"
    ))?;
    let mut linked = tx.prepare_cached(
        "UPDATE reference_linked_refs SET linked_ref_id=?3
         WHERE reference_id=?1 AND position=?2 AND linked_ref_id IS NULL",
    )?;
    let mut keyword = tx.prepare_cached(
        "UPDATE reference_linked_refs SET keyword_id=?3
         WHERE reference_id=?1 AND position=?2 AND keyword_id IS NULL",
    )?;
    let mut parent = tx.prepare_cached(
        "UPDATE reference_activate_parents SET parent_ref_id=?3
         WHERE reference_id=?1 AND position=?2 AND parent_ref_id IS NULL",
    )?;
    let mut attached = tx.prepare_cached(
        "INSERT INTO reference_details(reference_id,attached_ref_id) VALUES(?1,?2)
         ON CONFLICT(reference_id) DO UPDATE SET attached_ref_id=excluded.attached_ref_id
         WHERE reference_details.attached_ref_id IS NULL",
    )?;
    let mut owner = tx.prepare_cached(
        "INSERT INTO reference_details(reference_id,owner_id) VALUES(?1,?2)
         ON CONFLICT(reference_id) DO UPDATE SET owner_id=excluded.owner_id
         WHERE reference_details.owner_id IS NULL",
    )?;
    let can_restore = |allowed: &[&[u8; 4]]| kinds.values().any(|kind| allowed.contains(&kind));
    let mut sources = sources.query(params![
        can_restore(LINKED_REFERENCE),
        can_restore(LINKED_KEYWORD),
        can_restore(ATTACHED_REFERENCE),
        can_restore(OWNER),
    ])?;
    while let Some(row) = sources.next()? {
        let id: u32 = row.get(0)?;
        let Some(blob): Option<Vec<u8>> = row.get(1)? else {
            continue;
        };
        let record = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&blob).map_err(
            |error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Blob,
                    Box::new(error),
                )
            },
        )?;
        let canonical = record
            .subrecords
            .iter()
            .map(|sub| (sub.tag, sub.data.as_slice()));
        let mut restored = None;
        if let Some(order) = &order
            && let Some(payload) = row.get::<_, Option<Vec<u8>>>(2)?
        {
            let supplier = row.get::<_, usize>(3)?;
            let source_id = row.get(4)?;
            let kind: String = row.get(5)?;
            if let Ok(kind) = <&[u8; 4]>::try_from(kind.as_bytes())
                && supplier < order.names.len()
                && source_identity::header_runtime_id(kind, source_id, supplier, order) == Some(id)
            {
                restored = Some(authored_link_fields(
                    canonical.clone(),
                    &payload,
                    supplier,
                    order,
                ));
            }
        }
        let fields = |tag| {
            let recovered = restored
                .iter()
                .flatten()
                .filter_map(move |(sig, data)| (sig == &tag).then_some(data.as_slice()));
            let use_canonical = restored.is_none();
            let effective = canonical
                .clone()
                .filter_map(move |(sig, data)| (sig == tag && use_canonical).then_some(data));
            recovered.chain(effective)
        };
        for (position, (key, target)) in linked_refs(fields(*b"XLKR")).enumerate() {
            if let Some(value) = restored_target(key, LINKED_KEYWORD) {
                keyword.execute(params![id, position as i64, value])?;
            }
            if let Some(value) = restored_target(target, LINKED_REFERENCE) {
                linked.execute(params![id, position as i64, value])?;
            }
        }
        for (position, field) in activate_parents(fields(*b"XAPR")).enumerate() {
            if let Some(value) = restored_target(u32_at(field, 0), LINKED_REFERENCE) {
                parent.execute(params![id, position as i64, value])?;
            }
        }
        // Older typed subset blobs contain only XLKR and XAPR. Recover XOWN and
        // XATR from their placement payload when the auxiliary blob lacks them.
        let placement = row
            .get::<_, Option<Vec<u8>>>(6)?
            .map(|blob| rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&blob))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Blob,
                    Box::new(error),
                )
            })?;
        let form_id = |tag| {
            let data = fields(tag).next().or_else(|| {
                placement
                    .as_ref()?
                    .subrecords
                    .iter()
                    .find(|sub| sub.tag == tag)
                    .map(|sub| sub.data.as_slice())
            });
            sized(data, &[4]).and_then(|field| u32_at(field, 0))
        };
        if let Some(value) = restored_target(form_id(*b"XOWN"), OWNER) {
            owner.execute(params![id, value])?;
        }
        let attachment = form_id(*b"XATR");
        if let Some(value) = restored_target(attachment, ATTACHED_REFERENCE) {
            attached.execute(params![id, value])?;
        }
    }
    Ok(())
}

/// Writes the link and detail rows of one winning placed reference.
pub(crate) fn insert(
    tx: &Transaction<'_>,
    context: &Context<'_>,
    record: &RawRecord,
    source: Option<(&DecodedRecord, &LoadOrder)>,
) -> Result<()> {
    let id = record.form_id;
    let view = SubrecordView::new(&record.subrecords);
    let recovered = source.map(|(source, order)| {
        let fields = record.subrecords.iter().filter_map(|(tag, bytes)| {
            // silent-ok: Only four-byte tags can identify authored link fields.
            Some((<[u8; 4]>::try_from(tag.as_slice()).ok()?, bytes.as_slice()))
        });
        authored_link_fields(
            fields,
            &source.raw_payload,
            source.load_order as usize,
            order,
        )
    });
    if context.subset {
        let data = recovered
            .as_ref()
            .filter(|fields| !fields.is_empty())
            .map(|fields| serialize_subrecords(fields));
        tx.prepare_cached(
            "INSERT OR REPLACE INTO reference_link_subset_sources(form_id,data) VALUES (?1,?2)",
        )?
        .execute(params![id, data])?;
    }
    let link_fields = SubrecordView::new(recovered.as_deref().unwrap_or(&record.subrecords));
    for table in REFERENCE_LINK_TABLES {
        tx.prepare_cached(&format!("DELETE FROM {table} WHERE reference_id=?1"))?
            .execute([id])?;
    }

    // XLKR: one subrecord per link, a keyword and a reference, or (14 official references) the
    // reference alone.
    for (position, (keyword, reference)) in linked_refs(link_fields.find_all(b"XLKR")).enumerate() {
        tx.prepare_cached(
            "INSERT INTO reference_linked_refs(reference_id, position, keyword_id, linked_ref_id) VALUES (?1, ?2, ?3, ?4)",
        )?
        .execute(params![
            id,
            position as i64,
            context.resolve(tx, keyword, LINKED_KEYWORD)?,
            context.resolve(tx, reference, LINKED_REFERENCE)?,
        ])?;
    }

    // XAPR: one subrecord per activate parent, a reference then an f32 delay.
    for (position, parent) in activate_parents(link_fields.find_all(b"XAPR")).enumerate() {
        let delay = f32::from_le_bytes(parent[4..8].try_into().expect("eight-byte XAPR"));
        tx.prepare_cached(
            "INSERT INTO reference_activate_parents(reference_id, position, parent_ref_id, delay) VALUES (?1, ?2, ?3, ?4)",
        )?
        .execute(params![
            id,
            position as i64,
            context.resolve(tx, u32_at(parent, 0), LINKED_REFERENCE)?,
            delay.is_finite().then_some(f64::from(delay)),
        ])?;
    }

    let link = |fields: &SubrecordView<'_>, tag: &[u8; 4], targets: &[&[u8; 4]]| {
        context.resolve(
            tx,
            sized(fields.find(tag), &[4]).and_then(|bytes| u32_at(bytes, 0)),
            targets,
        )
    };
    let owner = link(&link_fields, b"XOWN", OWNER)?;
    let owner_rank = sized(view.find(b"XRNK"), &[4]).and_then(|bytes| u32_at(bytes, 0));
    let activate_parent_flags = sized(view.find(b"XAPD"), &[1]).map(|bytes| bytes[0]);
    // XLOC: level byte, three unused, key FormID, flags byte (0x04 leveled lock), padding. It is
    // untyped bytes in the in-house schema, so neither reader remaps the key; it is remapped here
    // through the supplying plugin's master list.
    let lock = view.find(b"XLOC").filter(|bytes| !bytes.is_empty());
    let remapped_lock_key = lock.and_then(|bytes| u32_at(bytes, 4)).and_then(|raw| {
        let order = context.order?;
        crate::records::source_identity::header_runtime_id(
            b"KEYM",
            raw,
            record.load_order as usize,
            order,
        )
    });
    let lock_key = context.resolve(tx, remapped_lock_key, &[b"KEYM"])?;
    let open_by_default = view.find(b"ONAM").is_some();
    let map_marker = view.find(b"XMRK").is_some();
    let location = link(&view, b"XLCN", &[b"LCTN"])?;
    let location_ref_type = link(&view, b"XLRL", &[b"LCRT", b"LCTN"])?;
    let encounter_zone = link(&view, b"XEZN", &[b"ECZN"])?;
    let attached = link(&link_fields, b"XATR", ATTACHED_REFERENCE)?;
    // A later target-only edit can fill an authored slot on a subset source.
    // Complete snapshots retain their sparse projection for invalid detail links.
    let pending_detail = context.subset
        && [b"XATR", b"XOWN"]
            .iter()
            .any(|tag| sized(link_fields.find(tag), &[4]).is_some());
    let any = owner.is_some()
        || owner_rank.is_some()
        || activate_parent_flags.is_some()
        || lock.is_some()
        || open_by_default
        || map_marker
        || location.is_some()
        || location_ref_type.is_some()
        || encounter_zone.is_some()
        || attached.is_some()
        || pending_detail;
    if any {
        tx.prepare_cached(
            "INSERT INTO reference_details(reference_id, owner_id, owner_rank, activate_parent_flags, lock_level, lock_key_id, lock_flags, open_by_default, map_marker, map_marker_flags, map_marker_type, location_id, location_ref_type_id, encounter_zone_id, attached_ref_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )?
        .execute(params![
            id,
            owner,
            owner_rank.map(|rank| rank as i32),
            activate_parent_flags,
            lock.and_then(|bytes| bytes.first()).copied(),
            lock_key,
            lock.and_then(|bytes| bytes.get(8)).copied(),
            open_by_default,
            map_marker,
            map_marker
                .then(|| sized(view.find(b"FNAM"), &[1]).map(|bytes| bytes[0]))
                .flatten(),
            map_marker
                .then(|| sized(view.find(b"TNAM"), &[2]).map(|bytes| bytes[0]))
                .flatten(),
            location,
            location_ref_type,
            encounter_zone,
            attached,
        ])?;
    }
    Ok(())
}
