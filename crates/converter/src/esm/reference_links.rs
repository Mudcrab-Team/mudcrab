//! Placed-reference links, activate parents, ownership, locks and map markers (#269 gap 3).
//!
//! Projected from all canonical winning placed records, so the legacy
//! and in-house readers publish the same rows (db-schema.md, "Reference Links and Details").
//! Layouts follow xEdit 4.1.5f `Core/wbDefinitionsTES5.pas` and the in-house schema.

use super::{
    extractors::SubrecordView, load_order::LoadOrder, records::RawRecord, types::ArchivedRecordData,
};
use rusqlite::{OptionalExtension, Result, Transaction, params};
use std::collections::{HashMap, HashSet};

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

/// What the projection needs beyond the record itself.
pub(crate) struct Context<'a> {
    pub(crate) master: &'a HashMap<u32, RawRecord>,
    /// The load order, to remap the `XLOC` key that neither reader remaps; `None` in subset
    /// exports, where the key stays NULL.
    pub(crate) order: Option<&'a LoadOrder>,
    pub(crate) subset: bool,
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
            let (kind, placed, deleted): (Option<String>, bool, bool) = tx
                .prepare_cached(
                    "SELECT (SELECT record_type FROM records WHERE form_id=?1),
                            EXISTS(SELECT 1 FROM [references] WHERE id=?1),
                            EXISTS(SELECT 1 FROM subset_nonplacement_deletions WHERE form_id=?1)",
                )?
                .query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            if deleted {
                return Ok(None);
            }
            if let Some(kind) = kind {
                // A deleted placement can still have a canonical row in an edited pack.
                let valid = <&[u8; 4]>::try_from(kind.as_bytes()).is_ok_and(|kind| {
                    targets.contains(&kind) && (placed || !super::is_placed_record_type(kind))
                });
                return Ok(valid.then_some(id));
            }
            if placed {
                // Placement imports can add REFRs without modifying the canonical archive.
                return Ok(targets.contains(&b"REFR").then_some(id));
            }
        }
        Ok((id < 0x800).then_some(id))
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

/// Refresh only the target columns. In particular, a source outside the edit retains its delay,
/// other links and details (including any XLOC key resolved by the full export).
pub(super) fn refresh_incoming(
    tx: &Transaction<'_>,
    unavailable: &[u32],
    available: &HashSet<u32>,
) -> Result<()> {
    if !unavailable.is_empty() {
        let ids = serde_json::json!(unavailable).to_string();
        for (table, column) in [
            ("reference_linked_refs", "linked_ref_id"),
            ("reference_linked_refs", "keyword_id"),
            ("reference_activate_parents", "parent_ref_id"),
            ("reference_details", "attached_ref_id"),
        ] {
            tx.prepare_cached(&format!(
                "UPDATE {table} SET {column}=NULL WHERE {column} IN (SELECT value FROM json_each(?1))"
            ))?
            .execute([&ids])?;
        }
    }
    if available.is_empty() {
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
    let restored_target = |value: Option<u32>, allowed: &[&[u8; 4]]| {
        value.filter(|id| kinds.get(id).is_some_and(|kind| allowed.contains(&kind)))
    };

    // Older packs may have no placement mirror. For layered sources the original archive
    // can instead be stale. The history lookup uses the resolved FormID, not source_form_id.
    let has_layers: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema
         WHERE type='table' AND name='room_placement_edits')",
        [],
        |row| row.get(0),
    )?;
    let data = if has_layers {
        "CASE WHEN EXISTS(SELECT 1 FROM room_placement_edits WHERE form_id=r.id)
              THEN r.data ELSE coalesce(raw.data,r.data) END"
    } else {
        "coalesce(raw.data,r.data)"
    };
    // A NULL projection carries no target ID. Scan only sources with nullable link slots
    // once per restoring batch. Materialize their IDs before updating the link tables.
    let mut sources = tx.prepare(&format!(
        "WITH candidates(id) AS MATERIALIZED (
             SELECT reference_id FROM reference_linked_refs WHERE linked_ref_id IS NULL
             UNION SELECT reference_id FROM reference_linked_refs WHERE keyword_id IS NULL
             UNION SELECT reference_id FROM reference_activate_parents WHERE parent_ref_id IS NULL
             UNION SELECT reference_id FROM reference_details WHERE attached_ref_id IS NULL
         )
         SELECT r.id, {data} FROM candidates
         JOIN [references] r ON r.id=candidates.id
         LEFT JOIN records raw ON raw.form_id=r.id"
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
        "UPDATE reference_details SET attached_ref_id=?2 WHERE reference_id=?1 AND attached_ref_id IS NULL",
    )?;
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
        let fields = |tag| {
            record
                .subrecords
                .iter()
                .filter_map(move |sub| (sub.tag == tag).then_some(sub.data.as_slice()))
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
        let attachment = sized(fields(*b"XATR").next(), &[4]).and_then(|field| u32_at(field, 0));
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
    view: &SubrecordView<'_>,
) -> Result<()> {
    let id = record.form_id;
    for table in REFERENCE_LINK_TABLES {
        tx.prepare_cached(&format!("DELETE FROM {table} WHERE reference_id=?1"))?
            .execute([id])?;
    }

    // XLKR: one subrecord per link, a keyword and a reference, or (14 official references) the
    // reference alone.
    for (position, (keyword, reference)) in linked_refs(view.find_all(b"XLKR")).enumerate() {
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
    for (position, parent) in activate_parents(view.find_all(b"XAPR")).enumerate() {
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

    let link = |tag: &[u8; 4], targets: &[&[u8; 4]]| {
        context.resolve(
            tx,
            sized(view.find(tag), &[4]).and_then(|bytes| u32_at(bytes, 0)),
            targets,
        )
    };
    let owner = link(b"XOWN", &[b"FACT", b"ACHR", b"NPC_"])?;
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
    let location = link(b"XLCN", &[b"LCTN"])?;
    let location_ref_type = link(b"XLRL", &[b"LCRT", b"LCTN"])?;
    let encounter_zone = link(b"XEZN", &[b"ECZN"])?;
    let attached = link(b"XATR", ATTACHED_REFERENCE)?;
    let any = owner.is_some()
        || owner_rank.is_some()
        || activate_parent_flags.is_some()
        || lock.is_some()
        || open_by_default
        || map_marker
        || location.is_some()
        || location_ref_type.is_some()
        || encounter_zone.is_some()
        || attached.is_some();
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
