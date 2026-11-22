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
pub(crate) const REFERENCE_LINK_TABLES: [&str; 4] = [
    "reference_linked_refs",
    "reference_activate_parents",
    "reference_details",
    "reference_light_data",
];

pub(crate) const REFERENCE_LINK_TABLES_SQL: &str = "
CREATE TABLE IF NOT EXISTS reference_light_data (
    reference_id INTEGER PRIMARY KEY,
    data_size INTEGER NOT NULL,
    fov_offset REAL,
    fade_offset REAL,
    end_distance_cap REAL,
    shadow_depth_bias REAL
);
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
pub(super) fn ensure_subset_sources(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS reference_link_subset_sources (
            form_id INTEGER PRIMARY KEY, data BLOB
        )",
    )?;
    super::exporter::add_missing_columns(tx, "reference_link_subset_sources", &[("data", "BLOB")])
}

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
    changed_sources: &[u32],
) -> Result<()> {
    let indexed = has_table(tx, "reference_authored_targets")?;
    if indexed || !available.is_empty() {
        index_authored_targets(tx, changed_sources, !indexed)?;
    }
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
    let targets = serde_json::json!(kinds.keys().collect::<Vec<_>>()).to_string();
    let mut entries = tx.prepare_cached(
        "SELECT reference_id, position, slot, target_id FROM reference_authored_targets
         WHERE target_id IN (SELECT value FROM json_each(?1))",
    )?;
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
    let mut rows = entries.query([targets])?;
    while let Some(row) = rows.next()? {
        let id: u32 = row.get(0)?;
        let position: i64 = row.get(1)?;
        let slot: String = row.get(2)?;
        let target: u32 = row.get(3)?;
        let kind = &kinds[&target];
        match slot.as_str() {
            "XLKR.ref" if LINKED_REFERENCE.contains(&kind) => {
                linked.execute(params![id, position, target])?
            }
            "XLKR.keyword" if LINKED_KEYWORD.contains(&kind) => {
                keyword.execute(params![id, position, target])?
            }
            "XAPR" if LINKED_REFERENCE.contains(&kind) => {
                parent.execute(params![id, position, target])?
            }
            "XATR" if ATTACHED_REFERENCE.contains(&kind) => {
                attached.execute(params![id, target])?
            }
            "XOWN" if OWNER.contains(&kind) => owner.execute(params![id, target])?,
            _ => continue,
        };
    }
    Ok(())
}

pub(super) fn create_authored_index(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE reference_authored_targets (
            reference_id INTEGER NOT NULL, slot TEXT NOT NULL, position INTEGER NOT NULL,
            target_id INTEGER NOT NULL, PRIMARY KEY(reference_id,slot,position)
        );
        CREATE INDEX idx_reference_authored_target ON reference_authored_targets(target_id);
        CREATE TABLE reference_authored_changes (reference_id INTEGER PRIMARY KEY);
        CREATE TRIGGER changed_authored_placement AFTER UPDATE OF data ON [references] BEGIN
            INSERT OR IGNORE INTO reference_authored_changes VALUES(NEW.id);
        END;",
    )
}

fn index_slots<'a>(
    tx: &Transaction<'_>,
    id: u32,
    links: impl Iterator<Item = &'a [u8]>,
    parents: impl Iterator<Item = &'a [u8]>,
    owner: Option<u32>,
    attached: Option<u32>,
) -> Result<()> {
    let index = |slot: &str, position: usize, target: Option<u32>| -> Result<()> {
        if let Some(target) = target.filter(|&target| target != 0) {
            tx.prepare_cached("INSERT INTO reference_authored_targets(reference_id,slot,position,target_id) VALUES(?1,?2,?3,?4)")?
                .execute(params![id, slot, position, target])?;
        }
        Ok(())
    };
    for (position, (key, target)) in linked_refs(links).enumerate() {
        index("XLKR.keyword", position, key)?;
        index("XLKR.ref", position, target)?;
    }
    for (position, field) in activate_parents(parents).enumerate() {
        index("XAPR", position, u32_at(field, 0))?;
    }
    index("XOWN", 0, owner)?;
    index("XATR", 0, attached)
}

/// Older snapshots can recover typed links only after their plugin provenance is installed.
/// Backfill once, in the first edit transaction that needs recovery. Later edits replace
/// index entries only for the sources they changed.
fn index_authored_targets(tx: &Transaction<'_>, changed: &[u32], backfill: bool) -> Result<()> {
    if backfill {
        create_authored_index(tx)?;
    } else {
        // Placement writers can update their archived bytes independently of the canonical record.
        tx.prepare_cached(
            "INSERT OR IGNORE INTO reference_authored_changes SELECT value FROM json_each(?1)",
        )?
        .execute([serde_json::json!(changed).to_string()])?;
        let empty: bool = tx.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM reference_authored_changes)",
            [],
            |row| row.get(0),
        )?;
        if empty {
            return Ok(());
        }
        tx.execute("DELETE FROM reference_authored_targets WHERE reference_id IN reference_authored_changes", [])?;
    }
    let source_filter = if backfill {
        ""
    } else {
        "WHERE r.id IN reference_authored_changes"
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
    let mut sources = tx.prepare(&format!(
        "SELECT r.id,
                CASE WHEN subset.form_id IS NOT NULL THEN coalesce(subset.data,r.data,raw.data)
                     WHEN {layered} THEN r.data ELSE coalesce(raw.data,r.data) END,
                {native_data},
                CASE WHEN subset.data IS NOT NULL THEN coalesce(r.data,raw.data) END
         FROM [references] r
         LEFT JOIN records raw ON raw.form_id=r.id
         LEFT JOIN reference_link_subset_sources subset ON subset.form_id=r.id
         {native_join} {source_filter}"
    ))?;
    let mut sources = sources.query([])?;
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
        index_slots(
            tx,
            id,
            fields(*b"XLKR"),
            fields(*b"XAPR"),
            form_id(*b"XOWN"),
            form_id(*b"XATR"),
        )?;
    }
    tx.execute("DELETE FROM reference_authored_changes", [])?;
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
    let source = source.filter(|(source, order)| {
        let supplier = source.load_order as usize;
        supplier < order.names.len()
            && source_identity::header_runtime_id(
                &source.record_type,
                source.source_form_id,
                supplier,
                order,
            ) == Some(id)
    });
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
    let authored = SubrecordView::new(recovered.as_deref().unwrap_or(&record.subrecords));
    if !context.subset {
        let id_field = |tag| sized(authored.find(tag), &[4]).and_then(|data| u32_at(data, 0));
        index_slots(
            tx,
            id,
            authored.find_all(b"XLKR"),
            authored.find_all(b"XAPR"),
            id_field(b"XOWN"),
            id_field(b"XATR"),
        )?;
    }
    let link_fields = if context.subset { &authored } else { &view };
    for table in REFERENCE_LINK_TABLES {
        tx.prepare_cached(&format!("DELETE FROM {table} WHERE reference_id=?1"))?
            .execute([id])?;
    }

    // xEdit cce1c42403b279957e1e78bfe38100b29f39f72c, wbDefinitionsTES5.pas,
    // REFR/XLIG: four little-endian floats and an optional four-byte tail (16/20 bytes).
    // Preserve presence and malformed size separately from absent XLIG. Non-finite fields
    // are NULL; the canonical subrecord archive retains the original bits.
    if let Some(bytes) = view.find(b"XLIG") {
        // The validated XLIG layout is 16 or 20 bytes, so the length fits a SQLite integer.
        #[allow(
            clippy::cast_possible_wrap,
            reason = "sized() admits only 16 or 20 bytes"
        )]
        let data_size = bytes.len() as i64;
        let real = |offset: usize| {
            sized(Some(bytes), &[16, 20])
                .and_then(|bytes| bytes.get(offset..offset + 4))
                // silent-ok: sized() admits only the 16/20-byte XLIG layouts, so the four-byte slice converts.
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                .map(f32::from_le_bytes)
                .filter(|value| value.is_finite())
        };
        tx.prepare_cached("INSERT INTO reference_light_data VALUES (?1,?2,?3,?4,?5,?6)")?
            .execute(params![id, data_size, real(0), real(4), real(8), real(12)])?;
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
    let owner = link(link_fields, b"XOWN", OWNER)?;
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
    let attached = link(link_fields, b"XATR", ATTACHED_REFERENCE)?;
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
