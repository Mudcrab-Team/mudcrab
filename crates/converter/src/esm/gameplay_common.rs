//! Shared gameplay projection helpers and normalized condition storage (#599).
//!
//! Projected from the canonical winning records so both the legacy and in-house
//! readers produce identical rows (`docs/specs/converters/db-schema.md`).

use super::records::RawRecord;
use crate::records::deciders::conditions::parameter_kinds;
use rusqlite::{Result, Transaction, params};
use std::collections::HashMap;

/// Tables owned by this module. A complete snapshot drops and recreates them.
pub(crate) const GAMEPLAY_COMMON_TABLES: [&str; 1] = ["record_conditions"];

/// DDL for shared condition storage; `create_tables` executes this.
pub(crate) const GAMEPLAY_COMMON_TABLES_SQL: &str = "
CREATE TABLE IF NOT EXISTS record_conditions (
    owner_id INTEGER NOT NULL,
    owner_kind TEXT NOT NULL,
    parent_index INTEGER NOT NULL,
    condition_tab INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL,
    operator INTEGER NOT NULL,
    flags INTEGER NOT NULL,
    comparison_value REAL,
    comparison_global_id INTEGER,
    function_index INTEGER NOT NULL,
    param1_raw INTEGER NOT NULL,
    param1_form_id INTEGER,
    param2_raw INTEGER NOT NULL,
    param2_form_id INTEGER,
    run_on INTEGER NOT NULL,
    reference_id INTEGER,
    param3 INTEGER NOT NULL,
    param1_string TEXT,
    param2_string TEXT,
    PRIMARY KEY (owner_id, owner_kind, parent_index, position)
);
CREATE INDEX IF NOT EXISTS idx_record_conditions_owner ON record_conditions(owner_id, owner_kind);
";

/// Target-checked FormID resolver matching the in-house schema's link-retention rules.
pub(crate) struct Links<'a> {
    pub(crate) master: &'a HashMap<u32, RawRecord>,
}

impl Links<'_> {
    /// Keep a non-zero FormID when it names an existing non-deleted record of an allowed type,
    /// or when it is a hardcoded engine ID (`< 0x800`, e.g. `0x14` for the player) with no
    /// overriding plugin record.
    pub(crate) fn resolve(&self, id: Option<u32>, targets: &[&[u8; 4]]) -> Option<u32> {
        id.filter(|id| {
            *id != 0
                && self.master.get(id).map_or(*id < 0x800, |record| {
                    !record.is_deleted()
                        && (targets.contains(&b"ANY_") || targets.contains(&&record.record_type))
                })
        })
    }
    /// Explain why a non-zero FormID failed `resolve`, for bounded warning diagnostics.
    pub(crate) fn mismatch_reason(&self, id: u32, targets: &[&[u8; 4]]) -> Option<String> {
        if id == 0 || self.resolve(Some(id), targets).is_some() {
            return None;
        }
        Some(match self.master.get(&id) {
            Some(record) if record.is_deleted() => "references a deleted record".to_owned(),
            Some(record) => format!(
                "has wrong record type {} (expected {})",
                String::from_utf8_lossy(&record.record_type),
                targets
                    .iter()
                    .map(|tag| String::from_utf8_lossy(*tag))
                    .collect::<Vec<_>>()
                    .join("|")
            ),
            None => "references an absent record".to_owned(),
        })
    }
}
///
/// The in-house frontend stores UTF-8 in `canonical_bytes`, whereas the legacy frontend leaves
/// Windows-1252 bytes in non-localized subrecords (`EDID`, `EPFD` text, `CIS1`, `CIS2`).
pub(crate) fn decode_subrecord_text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    let slice = &bytes[..end];
    match std::str::from_utf8(slice) {
        Ok(text) => text.to_owned(),
        Err(_) => crate::records::decode_text(slice),
    }
}

/// Allowed target record signatures for every `form_id` condition parameter alternative in
/// `crates/converter/src/records/families/conditions.json`.
pub(crate) fn condition_param_targets(kind: &str) -> Option<&'static [&'static [u8; 4]]> {
    const EQUP: &[&[u8; 4]] = &[b"EQUP"];
    const REFR: &[&[u8; 4]] = &[
        b"PLYR", b"ACHR", b"REFR", b"PGRE", b"PHZD", b"PMIS", b"PARW", b"PBAR", b"PBEA", b"PCON",
        b"PFLA",
    ];
    const INVENTORY: &[&[u8; 4]] = &[
        b"ARMO", b"BOOK", b"MISC", b"WEAP", b"AMMO", b"KEYM", b"ALCH", b"SCRL", b"SLGM", b"INGR",
        b"FLST", b"LIGH", b"LVLI", b"COBJ",
    ];
    const ACTOR: &[&[u8; 4]] = &[b"PLYR", b"ACHR", b"REFR"];
    const VTYP: &[&[u8; 4]] = &[b"VTYP", b"FLST"];
    const IDLE: &[&[u8; 4]] = &[b"IDLE"];
    const FLST: &[&[u8; 4]] = &[b"FLST"];
    const QUST: &[&[u8; 4]] = &[b"QUST"];
    const FACT: &[&[u8; 4]] = &[b"FACT"];
    const CELL: &[&[u8; 4]] = &[b"CELL"];
    const CLAS: &[&[u8; 4]] = &[b"CLAS"];
    const RACE: &[&[u8; 4]] = &[b"RACE"];
    const NPC: &[&[u8; 4]] = &[b"NPC_"];
    const GLOB: &[&[u8; 4]] = &[b"GLOB"];
    const WTHR: &[&[u8; 4]] = &[b"WTHR"];
    const PACK: &[&[u8; 4]] = &[b"PACK"];
    const ECZN: &[&[u8; 4]] = &[b"ECZN"];
    const PERK: &[&[u8; 4]] = &[b"PERK"];
    const OWNER: &[&[u8; 4]] = &[b"FACT", b"NPC_"];
    const FURN: &[&[u8; 4]] = &[b"FURN", b"FLST"];
    const MAGIC_ITEM: &[&[u8; 4]] = &[b"SPEL", b"ENCH", b"ALCH", b"INGR", b"SCRL"];
    const MGEF: &[&[u8; 4]] = &[b"MGEF"];
    const WRLD: &[&[u8; 4]] = &[b"WRLD", b"FLST"];
    const BASE_OBJECT: &[&[u8; 4]] = &[
        b"NPC_", b"PROJ", b"TREE", b"SOUN", b"ACTI", b"DOOR", b"STAT", b"FURN", b"CONT", b"ARMO",
        b"AMMO", b"MISC", b"WEAP", b"BOOK", b"KEYM", b"ALCH", b"LIGH", b"GRAS", b"ASPC", b"IDLM",
        b"ARMA", b"MSTT", b"TACT", b"LVLI", b"LVSP", b"SPEL", b"SCRL", b"SHOU", b"SLGM", b"ENCH",
        b"FLOR", b"HAZD", b"FLST",
    ];
    const REGN: &[&[u8; 4]] = &[b"REGN"];
    const KYWD: &[&[u8; 4]] = &[b"KYWD"];
    const SHOU: &[&[u8; 4]] = &[b"SHOU"];
    const LCTN: &[&[u8; 4]] = &[b"LCTN"];
    const LCRT: &[&[u8; 4]] = &[b"LCRT"];
    const ASTP: &[&[u8; 4]] = &[b"ASTP"];
    const SCEN: &[&[u8; 4]] = &[b"SCEN"];
    const ANY: &[&[u8; 4]] = &[b"ANY_"];
    const KNOWABLE: &[&[u8; 4]] = &[b"MGEF", b"WOOP", b"ENCH"];
    const WEAP: &[&[u8; 4]] = &[b"WEAP"];
    const SPEL: &[&[u8; 4]] = &[b"SPEL"];

    Some(match kind {
        "equip_type" => EQUP,
        "object_reference" => REFR,
        "inventory_object" => INVENTORY,
        "actor" => ACTOR,
        "voice_type" => VTYP,
        "idle" => IDLE,
        "form_list" | "vats_weapon_list" | "vats_target_list" | "vats_critical_effect_list" => FLST,
        "quest" => QUST,
        "faction" | "optional_faction" => FACT,
        "cell" => CELL,
        "class" => CLAS,
        "race" => RACE,
        "actor_base" | "vats_target" => NPC,
        "global" => GLOB,
        "weather" => WTHR,
        "package" => PACK,
        "encounter_zone" => ECZN,
        "perk" => PERK,
        "owner" => OWNER,
        "furniture" => FURN,
        "magic_item" => MAGIC_ITEM,
        "magic_effect" => MGEF,
        "worldspace" => WRLD,
        "base_object" => BASE_OBJECT,
        "region" => REGN,
        "keyword" => KYWD,
        "shout" => SHOU,
        "location" => LCTN,
        "ref_type" => LCRT,
        "association_type" => ASTP,
        "scene" => SCEN,
        "event_data" => ANY,
        "knowable" => KNOWABLE,
        "vats_weapon" => WEAP,
        "vats_critical_effect" => SPEL,
        _ => return None,
    })
}

/// Resolve the effective parameter kinds for a 32-byte `CTDA`, delegating to
/// `crates/converter/src/records/deciders/conditions.rs`.
pub(crate) fn condition_param_kinds(ctda: &[u8]) -> (&'static str, &'static str) {
    parameter_kinds(ctda)
}

/// Byte offsets, labels, and allowed target record types for FormID fields inside a 32-byte `CTDA`.
pub(crate) fn condition_link_slots(
    ctda: &[u8],
) -> Vec<(usize, &'static str, &'static [&'static [u8; 4]])> {
    const GLOB: &[&[u8; 4]] = &[b"GLOB"];
    const REFR: &[&[u8; 4]] = &[
        b"PLYR", b"ACHR", b"REFR", b"PGRE", b"PHZD", b"PMIS", b"PARW", b"PBAR", b"PBEA", b"PCON",
        b"PFLA",
    ];
    if ctda.len() != 32 {
        return Vec::new();
    }
    let mut slots = Vec::new();
    if ctda[0] & 0x04 != 0 {
        slots.push((4, "comparison_global", GLOB));
    }
    let (first, second) = condition_param_kinds(ctda);
    if let Some(targets) = condition_param_targets(first) {
        slots.push((12, "param1", targets));
    }
    if let Some(targets) = condition_param_targets(second) {
        slots.push((16, "param2", targets));
    }
    if u32::from_le_bytes([ctda[20], ctda[21], ctda[22], ctda[23]]) == 2 {
        slots.push((24, "reference", REFR));
    }
    slots
}

/// Byte offsets of FormID fields inside a 32-byte `CTDA` subrecord, for legacy remapping parity.
pub(crate) fn condition_link_offsets(ctda: &[u8]) -> Vec<usize> {
    condition_link_slots(ctda)
        .into_iter()
        .map(|(offset, _, _)| offset)
        .collect()
}

/// A decoded condition paired with its optional `PRKC` tab and `CIS1`/`CIS2` strings.
pub(crate) struct ConditionEntry<'a> {
    pub(crate) condition_tab: i8,
    pub(crate) ctda: &'a [u8],
    pub(crate) param1_string: Option<String>,
    pub(crate) param2_string: Option<String>,
}

fn resolve_condition_param(
    links: &Links<'_>,
    owner_kind: &str,
    owner_id: u32,
    position: usize,
    slot: &str,
    raw: u32,
    kind: &str,
) -> (u32, Option<u32>) {
    match condition_param_targets(kind) {
        Some(targets) => {
            let resolved = links.resolve(Some(raw), targets);
            if resolved.is_none()
                && let Some(reason) = links.mismatch_reason(raw, targets)
            {
                eprintln!(
                    "warning: {owner_kind} {owner_id:08X} CTDA[{position}] {slot} target {raw:08X} {reason}; cleared to 0/NULL"
                );
            }
            (resolved.unwrap_or(0), resolved)
        }
        None => (raw, None),
    }
}

/// Convert a `usize` ordinal into a SQLite `INTEGER` without lossy wrapping.
pub(crate) fn sql_index(index: usize) -> Result<i64> {
    i64::try_from(index).map_err(|_| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
            "index exceeds SQLite INTEGER range",
        )))
    })
}

/// Write a slice of decoded conditions into `record_conditions`.
pub(crate) fn insert_conditions(
    tx: &Transaction<'_>,
    links: &Links<'_>,
    owner_id: u32,
    owner_kind: &str,
    parent_index: i64,
    entries: &[ConditionEntry<'_>],
) -> Result<()> {
    tx.prepare_cached(
        "DELETE FROM record_conditions WHERE owner_id=?1 AND owner_kind=?2 AND parent_index=?3",
    )?
    .execute(params![owner_id, owner_kind, parent_index])?;
    if entries.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare_cached(
        "INSERT INTO record_conditions(
            owner_id, owner_kind, parent_index, condition_tab, position,
            operator, flags, comparison_value, comparison_global_id,
            function_index, param1_raw, param1_form_id, param2_raw, param2_form_id,
            run_on, reference_id, param3, param1_string, param2_string
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
    )?;
    for (position, entry) in entries.iter().enumerate() {
        let ctda = entry.ctda;
        debug_assert_eq!(ctda.len(), 32);
        let raw_flags = ctda[0];
        let operator = (raw_flags >> 5) & 0x07;
        let flags = raw_flags & 0x1F;
        let comparison_word = u32::from_le_bytes([ctda[4], ctda[5], ctda[6], ctda[7]]);
        let (comparison_value, comparison_global_id) = if flags & 0x04 != 0 {
            let resolved = links.resolve(Some(comparison_word), &[b"GLOB"]);
            if resolved.is_none()
                && let Some(reason) = links.mismatch_reason(comparison_word, &[b"GLOB"])
            {
                eprintln!(
                    "warning: {owner_kind} {owner_id:08X} CTDA[{position}] comparison_global target {comparison_word:08X} {reason}; cleared to NULL"
                );
            }
            (None, resolved)
        } else {
            let value = f32::from_bits(comparison_word);
            (value.is_finite().then_some(f64::from(value)), None)
        };
        let function_index = u16::from_le_bytes([ctda[8], ctda[9]]);
        let (first_kind, second_kind) = condition_param_kinds(ctda);
        let (param1_raw, param1_form_id) = resolve_condition_param(
            links,
            owner_kind,
            owner_id,
            position,
            "param1",
            u32::from_le_bytes([ctda[12], ctda[13], ctda[14], ctda[15]]),
            first_kind,
        );
        let (param2_raw, param2_form_id) = resolve_condition_param(
            links,
            owner_kind,
            owner_id,
            position,
            "param2",
            u32::from_le_bytes([ctda[16], ctda[17], ctda[18], ctda[19]]),
            second_kind,
        );
        let run_on = u32::from_le_bytes([ctda[20], ctda[21], ctda[22], ctda[23]]);
        let reference_raw = u32::from_le_bytes([ctda[24], ctda[25], ctda[26], ctda[27]]);
        let reference_id = if run_on == 2 {
            const REFR_TARGETS: &[&[u8; 4]] = &[
                b"PLYR", b"ACHR", b"REFR", b"PGRE", b"PHZD", b"PMIS", b"PARW", b"PBAR", b"PBEA",
                b"PCON", b"PFLA",
            ];
            let resolved = links.resolve(Some(reference_raw), REFR_TARGETS);
            if resolved.is_none()
                && let Some(reason) = links.mismatch_reason(reference_raw, REFR_TARGETS)
            {
                eprintln!(
                    "warning: {owner_kind} {owner_id:08X} CTDA[{position}] reference target {reference_raw:08X} {reason}; cleared to NULL"
                );
            }
            resolved
        } else {
            None
        };
        let param3 = i32::from_le_bytes([ctda[28], ctda[29], ctda[30], ctda[31]]);
        stmt.execute(params![
            owner_id,
            owner_kind,
            parent_index,
            i64::from(entry.condition_tab),
            sql_index(position)?,
            i64::from(operator),
            i64::from(flags),
            comparison_value,
            comparison_global_id,
            i64::from(function_index),
            i64::from(param1_raw),
            param1_form_id,
            i64::from(param2_raw),
            param2_form_id,
            i64::from(run_on),
            reference_id,
            i64::from(param3),
            entry.param1_string.as_deref(),
            entry.param2_string.as_deref(),
        ])?;
    }
    Ok(())
}
