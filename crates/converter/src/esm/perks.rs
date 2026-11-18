//! Perk and entry-point projections (`PERK`) into `perks`, `perk_effects`,
//! `perk_entry_points`, and `record_conditions` (#599).
//!
//! Projected from canonical winning records so the legacy and in-house frontends publish
//! identical rows (`docs/specs/converters/db-schema.md`).

use super::{
    extractors::SubrecordView,
    gameplay_common::{ConditionEntry, Links, decode_subrecord_text, insert_conditions},
    projection_omissions::{OmissionCategory, ProjectionOmissions},
    records::RawRecord,
};
use rusqlite::{Result, Transaction, params};

/// Tables owned by this module. A complete export drops and recreates them.
pub(crate) const PERK_TABLES: [&str; 3] = ["perks", "perk_effects", "perk_entry_points"];

/// DDL for perk tables and lookup indexes; `create_tables` runs this.
pub(crate) const PERK_TABLES_SQL: &str = "
CREATE TABLE IF NOT EXISTS perks (
    id INTEGER PRIMARY KEY,
    editor_id TEXT,
    full_name TEXT,
    description TEXT,
    large_icon TEXT,
    small_icon TEXT,
    is_trait INTEGER,
    level INTEGER,
    rank_count INTEGER,
    playable INTEGER,
    hidden INTEGER,
    next_perk_id INTEGER
);
CREATE INDEX IF NOT EXISTS idx_perks_editor_id ON perks(editor_id);
CREATE TABLE IF NOT EXISTS perk_effects (
    perk_id INTEGER NOT NULL,
    effect_index INTEGER NOT NULL,
    effect_type INTEGER NOT NULL,
    rank INTEGER NOT NULL,
    priority INTEGER NOT NULL,
    quest_id INTEGER,
    quest_stage INTEGER,
    ability_spell_id INTEGER,
    PRIMARY KEY (perk_id, effect_index)
);
CREATE TABLE IF NOT EXISTS perk_entry_points (
    perk_id INTEGER NOT NULL,
    effect_index INTEGER NOT NULL,
    entry_point INTEGER NOT NULL,
    function INTEGER NOT NULL,
    condition_tab_count INTEGER NOT NULL,
    parameter_type INTEGER,
    button_label TEXT,
    script_flags INTEGER,
    fragment_index INTEGER,
    value_float_1 REAL,
    value_float_2 REAL,
    actor_value INTEGER,
    target_form_id INTEGER,
    value_text TEXT,
    PRIMARY KEY (perk_id, effect_index)
);
CREATE INDEX IF NOT EXISTS idx_perk_entry_points_entry_point ON perk_entry_points(entry_point);
";

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn f32_at(bytes: &[u8], offset: usize) -> Option<f32> {
    let value = f32::from_le_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?);
    value.is_finite().then_some(value)
}

type Subrecord = (Vec<u8>, Vec<u8>);
type PerkSections<'a> = (&'a [Subrecord], Vec<&'a [Subrecord]>);

/// Reader-dropped fields within a winning `PERK` whose owning section cannot be projected whole.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PerkRejectedSections {
    /// True if top-level condition fields before the first `PRKE` were rejected.
    pub(crate) top_level: bool,
    /// `effect_index` values whose effect subrecords had a reader-level rejection.
    pub(crate) effect_indices: std::collections::BTreeSet<usize>,
}

impl PerkRejectedSections {
    pub(crate) fn is_empty(&self) -> bool {
        !self.top_level && self.effect_indices.is_empty()
    }
}

/// Partition `items` into top-level entries (before the first `PRKE`) and one slice per
/// `PRKE`..`PRKF` effect block.
fn split_sections<T>(items: &[T], tag_of: impl Fn(&T) -> &[u8]) -> (&[T], Vec<&[T]>) {
    let first_prke = items.iter().position(|item| tag_of(item) == b"PRKE");
    let Some(start) = first_prke else {
        return (items, Vec::new());
    };
    let top = &items[..start];
    let mut effects = Vec::new();
    let mut cursor = start;
    while cursor < items.len() {
        if tag_of(&items[cursor]) != b"PRKE" {
            cursor += 1;
            continue;
        }
        let mut end = cursor + 1;
        while end < items.len() {
            let tag = tag_of(&items[end]);
            if tag == b"PRKF" {
                end += 1;
                break;
            }
            if tag == b"PRKE" {
                break;
            }
            end += 1;
        }
        effects.push(&items[cursor..end]);
        cursor = end;
    }
    (top, effects)
}

fn split_perk_sections(subrecords: &[Subrecord]) -> PerkSections<'_> {
    split_sections(subrecords, |(tag, _)| tag.as_slice())
}

type RawSubrecord<'a> = ([u8; 4], &'a [u8]);

/// Frame uncompressed `raw_payload` subrecord `(tag, bytes)` slices (`XXXX`-aware) plus any
/// truncated trailing tag whose declared payload ran past `payload.len()`.
fn extract_raw_subrecords(payload: &[u8]) -> (Vec<RawSubrecord<'_>>, Option<[u8; 4]>, bool) {
    let mut pos = 0;
    let mut subrecords = Vec::new();
    let mut extended: Option<usize> = None;
    let mut truncated_tag: Option<[u8; 4]> = None;
    let mut complete = true;
    while pos < payload.len() {
        if payload.len() - pos < 6 {
            complete = false;
            break;
        }
        let tag = [
            payload[pos],
            payload[pos + 1],
            payload[pos + 2],
            payload[pos + 3],
        ];
        let short = u16::from_le_bytes([payload[pos + 4], payload[pos + 5]]) as usize;
        pos += 6;
        if tag == *b"XXXX" {
            if short != 4 || payload.len() - pos < 4 || extended.is_some() {
                complete = false;
                break;
            }
            extended = Some(u32::from_le_bytes([
                payload[pos],
                payload[pos + 1],
                payload[pos + 2],
                payload[pos + 3],
            ]) as usize);
            pos += 4;
            continue;
        }
        let size = extended.take().unwrap_or(short);
        if payload.len() - pos < size {
            truncated_tag = Some(tag);
            complete = false;
            break;
        }
        subrecords.push((tag, &payload[pos..pos + size]));
        pos += size;
    }
    if extended.is_some() {
        complete = false;
    }
    (subrecords, truncated_tag, complete)
}

fn count_raw_tag(subs: &[RawSubrecord<'_>], needle: &[u8; 4]) -> usize {
    subs.iter().filter(|(tag, _)| tag == needle).count()
}

fn count_subrecord_tag(subs: &[Subrecord], needle: &[u8; 4]) -> usize {
    subs.iter()
        .filter(|(tag, _)| tag.as_slice() == needle)
        .count()
}

fn block_epft_epfd<'a, T>(
    block: &'a [T],
    tag_of: impl Fn(&T) -> &[u8],
    data_of: impl Fn(&'a T) -> &'a [u8],
) -> (Option<&'a [u8]>, Option<&'a [u8]>) {
    let mut epft = None;
    let mut epfd = None;
    for item in block.get(1..).unwrap_or(&[]) {
        let tag = tag_of(item);
        if tag == b"EPFT" && epfd.is_none() {
            epft = Some(data_of(item));
        } else if tag == b"EPFD" && epfd.is_none() {
            epfd = Some(data_of(item));
        }
    }
    (epft, epfd)
}

/// Returns true if any entry-point effect holds a four-byte zero `EPFD` under a link-valued `EPFT`.
pub(crate) fn has_zero_link_epfd(canonical: &RawRecord) -> bool {
    let (_, blocks) = split_perk_sections(&canonical.subrecords);
    blocks.into_iter().any(|block| {
        let (epft, epfd) = block_epft_epfd(block, |(tag, _)| tag.as_slice(), |(_, b)| b.as_slice());
        matches!(epft, Some([3..=5])) && epfd == Some([0, 0, 0, 0].as_slice())
    })
}

/// Map in-house `rejected_fields` (already stripped from `DecodedRecord::fields`), truncated
/// payloads, and link-cleared `EPFD` references back to `PERK` sections.
pub(crate) fn inhouse_rejected_sections(
    raw_payload: &[u8],
    canonical: &RawRecord,
    rejected_fields: &[[u8; 4]],
    payload_complete: bool,
) -> PerkRejectedSections {
    let mut sections = PerkRejectedSections::default();
    let has_zero_epfd = has_zero_link_epfd(canonical);
    if rejected_fields.is_empty() && payload_complete && !has_zero_epfd {
        return sections;
    }
    let (raw_subs, truncated_tag, raw_complete) = extract_raw_subrecords(raw_payload);
    let (raw_top, raw_blocks) = split_sections(&raw_subs, |(tag, _)| tag.as_slice());
    let (canon_top, canon_blocks) = split_perk_sections(&canonical.subrecords);

    sections.top_level = rejected_fields.iter().any(|tag| {
        matches!(tag, b"CTDA" | b"CIS1" | b"CIS2")
            && count_raw_tag(raw_top, tag) > count_subrecord_tag(canon_top, tag)
    }) || (raw_blocks.is_empty()
        && matches!(truncated_tag, Some(tag) if matches!(&tag, b"CTDA" | b"CIS1" | b"CIS2")));
    for (effect_index, raw_block) in raw_blocks.iter().enumerate() {
        let canon_block = canon_blocks.get(effect_index).copied().unwrap_or(&[]);
        if rejected_fields
            .iter()
            .any(|tag| count_raw_tag(raw_block, tag) > count_subrecord_tag(canon_block, tag))
        {
            sections.effect_indices.insert(effect_index);
            continue;
        }
        if has_zero_epfd {
            let (raw_epft, raw_epfd) =
                block_epft_epfd(raw_block, |(tag, _)| tag.as_slice(), |(_, b)| *b);
            let (canon_epft, canon_epfd) = block_epft_epfd(
                canon_block,
                |(tag, _)| tag.as_slice(),
                |(_, b)| b.as_slice(),
            );
            if matches!(raw_epft, Some([3..=5]))
                && matches!(canon_epft, Some([3..=5]))
                && let (Some(raw_b), Some(canon_b)) = (raw_epfd, canon_epfd)
                && raw_b.len() == 4
                && u32_at(raw_b, 0).is_some_and(|id| id != 0)
                && canon_b == [0, 0, 0, 0]
            {
                sections.effect_indices.insert(effect_index);
            }
        }
    }
    for effect_index in raw_blocks.len()..canon_blocks.len() {
        sections.effect_indices.insert(effect_index);
    }
    if (!payload_complete || !raw_complete || truncated_tag.is_some())
        && let Some(last_block) = raw_blocks.last()
        && last_block.last().map(|(tag, _)| tag) != Some(b"PRKF")
    {
        sections.effect_indices.insert(raw_blocks.len() - 1);
    }
    sections
}

/// Map legacy `malformed_subrecords` indices and link-rejected effects to `PERK` sections.
pub(crate) fn legacy_rejected_sections(
    subrecords: &[Subrecord],
    malformed_indices: &[usize],
    rejected_link_effects: &std::collections::BTreeSet<usize>,
) -> PerkRejectedSections {
    let mut sections = PerkRejectedSections::default();
    sections
        .effect_indices
        .extend(rejected_link_effects.iter().copied());
    if malformed_indices.is_empty() {
        return sections;
    }
    let mut current_effect: Option<usize> = None;
    let mut next_effect = 0usize;
    for (index, (tag, _)) in subrecords.iter().enumerate() {
        match tag.as_slice() {
            b"PRKE" => {
                current_effect = Some(next_effect);
                next_effect += 1;
            }
            b"PRKF" => {
                if let Some(effect_index) = current_effect.take()
                    && malformed_indices.binary_search(&index).is_ok()
                {
                    sections.effect_indices.insert(effect_index);
                }
                continue;
            }
            _ => {}
        }
        if malformed_indices.binary_search(&index).is_ok() {
            match current_effect {
                Some(effect_index) => {
                    sections.effect_indices.insert(effect_index);
                }
                None if matches!(tag.as_slice(), b"CTDA" | b"CIS1" | b"CIS2") => {
                    sections.top_level = true;
                }
                None => {}
            }
        }
    }
    sections
}

fn function_allows_parameter_type(function: u8, parameter_type: Option<u8>) -> bool {
    match function {
        0 | 6 | 7 => matches!(parameter_type, None | Some(0)),
        1..=3 => parameter_type == Some(1),
        4 | 5 | 12..=14 => parameter_type == Some(2),
        8 => parameter_type == Some(3),
        9 => parameter_type == Some(4),
        10 => parameter_type == Some(5),
        11 => parameter_type == Some(6),
        15 => parameter_type == Some(7),
        _ => false,
    }
}

/// Collect `CTDA` (+ `CIS1`/`CIS2`, and optional `PRKC` tab) entries from a subrecord slice.
/// Returns `Err(reason)` if any `CTDA` is not 32 bytes.
fn collect_conditions<'a>(
    subs: &'a [(Vec<u8>, Vec<u8>)],
) -> std::result::Result<Vec<ConditionEntry<'a>>, String> {
    let mut entries: Vec<ConditionEntry<'a>> = Vec::new();
    let mut current_tab: i8 = 0;
    let mut active_condition = false;
    for (tag, data) in subs {
        match tag.as_slice() {
            b"PRKC" => {
                if data.len() != 1 {
                    return Err(format!("PRKC has {} bytes, expected 1", data.len()));
                }
                current_tab = data[0] as i8;
                active_condition = false;
            }
            b"CTDA" => {
                if data.len() != 32 {
                    return Err(format!("CTDA has {} bytes, expected 32", data.len()));
                }
                if data[0] & 0x04 == 0
                    && !f32::from_bits(u32::from_le_bytes([data[4], data[5], data[6], data[7]]))
                        .is_finite()
                {
                    return Err("CTDA has non-finite literal comparison".into());
                }
                entries.push(ConditionEntry {
                    condition_tab: current_tab,
                    ctda: data.as_slice(),
                    param1_string: None,
                    param2_string: None,
                });
                active_condition = true;
            }
            b"CIS1" if active_condition => {
                if let Some(last) = entries.last_mut() {
                    last.param1_string = Some(decode_subrecord_text(data));
                }
            }
            b"CIS2" if active_condition => {
                if let Some(last) = entries.last_mut() {
                    last.param2_string = Some(decode_subrecord_text(data));
                }
            }
            _ => {
                active_condition = false;
            }
        }
    }
    Ok(entries)
}

/// Returns the first subrecord matching `tag` inside one `PRKE..PRKF` effect block.
fn first_block_subrecord<'a>(block: &'a [Subrecord], tag: &[u8; 4]) -> Option<&'a [u8]> {
    block
        .get(1..)?
        .iter()
        .find(|(sub_tag, _)| sub_tag.as_slice() == tag)
        .map(|(_, payload)| payload.as_slice())
}

/// Writes the `perks`, `perk_effects`, `perk_entry_points`, and `record_conditions` rows for
/// one winning `PERK` record (projected with unresolved localized text cleared, alongside its
/// `canonical` winning record), recording undecodable effects in `omissions`.
pub(crate) fn insert_record(
    tx: &Transaction<'_>,
    links: &Links<'_>,
    record: &RawRecord,
    canonical: &RawRecord,
    rejected: Option<&PerkRejectedSections>,
    omissions: &mut ProjectionOmissions,
) -> Result<()> {
    let perk_id = record.form_id;
    tx.prepare_cached("DELETE FROM perk_effects WHERE perk_id=?1")?
        .execute([perk_id])?;
    tx.prepare_cached("DELETE FROM perk_entry_points WHERE perk_id=?1")?
        .execute([perk_id])?;
    tx.prepare_cached(
        "DELETE FROM record_conditions WHERE owner_id=?1 AND owner_kind IN ('PERK', 'PERK_EFFECT')",
    )?
    .execute([perk_id])?;
    if record.is_deleted() {
        tx.prepare_cached("DELETE FROM perks WHERE id=?1")?
            .execute([perk_id])?;
        return Ok(());
    }

    let (top_subs, effect_blocks) = split_perk_sections(&record.subrecords);
    let (_, canonical_blocks) = split_perk_sections(&canonical.subrecords);
    let top_view = SubrecordView::new(top_subs);
    let data = top_view.find(b"DATA").filter(|bytes| bytes.len() == 5);
    let is_trait = data.map(|bytes| i64::from(bytes[0]));
    let level = data.map(|bytes| i64::from(bytes[1]));
    let rank_count = data.map(|bytes| i64::from(bytes[2]));
    let playable = data.map(|bytes| i64::from(bytes[3]));
    let hidden = data.map(|bytes| i64::from(bytes[4]));
    let next_perk_id = links.resolve(top_view.get_form_id(b"NNAM"), &[b"PERK"]);

    tx.prepare_cached(
        "INSERT OR REPLACE INTO perks(
            id, editor_id, full_name, description, large_icon, small_icon,
            is_trait, level, rank_count, playable, hidden, next_perk_id
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?
    .execute(params![
        perk_id,
        top_view.get_string(b"EDID"),
        top_view.get_string(b"FULL"),
        top_view.get_string(b"DESC"),
        top_view.get_string(b"ICON"),
        top_view.get_string(b"MICO"),
        is_trait,
        level,
        rank_count,
        playable,
        hidden,
        next_perk_id,
    ])?;

    if rejected.is_some_and(|r| r.top_level) {
        omissions.record(
            OmissionCategory::PerkEffectUndecoded,
            perk_id,
            format!("PERK {perk_id:08X} top-level conditions: reader rejected subrecord"),
        );
    } else {
        match collect_conditions(top_subs) {
            Ok(top_conditions) => {
                insert_conditions(tx, links, perk_id, "PERK", 0, &top_conditions)?;
            }
            Err(reason) => {
                omissions.record(
                    OmissionCategory::PerkEffectUndecoded,
                    perk_id,
                    format!("PERK {perk_id:08X} top-level conditions: {reason}"),
                );
            }
        }
    }

    for (effect_index, block) in effect_blocks.iter().enumerate() {
        let Some((_, prke)) = block.first().filter(|(tag, _)| tag.as_slice() == b"PRKE") else {
            continue;
        };
        if rejected.is_some_and(|r| r.effect_indices.contains(&effect_index)) {
            omissions.record(
                OmissionCategory::PerkEffectUndecoded,
                perk_id,
                format!("PERK {perk_id:08X} effect {effect_index}: reader rejected subrecord"),
            );
            continue;
        }
        if prke.len() != 3 {
            omissions.record(
                OmissionCategory::PerkEffectUndecoded,
                perk_id,
                format!(
                    "PERK {perk_id:08X} effect {effect_index}: PRKE is {} bytes, expected 3",
                    prke.len()
                ),
            );
            continue;
        }
        let effect_type = prke[0];
        let rank = prke[1];
        let priority = prke[2];

        let mut effect_data: Option<&[u8]> = None;
        let mut epft: Option<&[u8]> = None;
        let mut epf2: Option<&[u8]> = None;
        let mut epf3: Option<&[u8]> = None;
        let mut epfd: Option<&[u8]> = None;
        let mut bad_prkf: Option<usize> = None;
        for (tag, payload) in &block[1..] {
            match tag.as_slice() {
                b"DATA" if effect_data.is_none() => effect_data = Some(payload.as_slice()),
                b"EPFT" if epfd.is_none() => epft = Some(payload.as_slice()),
                b"EPF2" if epf2.is_none() => epf2 = Some(payload.as_slice()),
                b"EPF3" if epf3.is_none() => epf3 = Some(payload.as_slice()),
                b"EPFD" if epfd.is_none() => epfd = Some(payload.as_slice()),
                b"PRKF" if !payload.is_empty() && bad_prkf.is_none() => {
                    bad_prkf = Some(payload.len());
                }
                _ => {}
            }
        }
        if let Some(len) = bad_prkf {
            omissions.record(
                OmissionCategory::PerkEffectUndecoded,
                perk_id,
                format!(
                    "PERK {perk_id:08X} effect {effect_index}: PRKF is {len} bytes, expected 0"
                ),
            );
            continue;
        }

        match effect_type {
            0 | 1 => {
                let (quest_id, quest_stage, ability_spell_id) = if effect_type == 0 {
                    let Some(bytes) = effect_data.filter(|b| b.len() == 8) else {
                        omissions.record(
                            OmissionCategory::PerkEffectUndecoded,
                            perk_id,
                            format!(
                                "PERK {perk_id:08X} effect {effect_index}: quest_stage missing 8-byte DATA"
                            ),
                        );
                        continue;
                    };
                    (
                        links.resolve(u32_at(bytes, 0), &[b"QUST"]),
                        Some(i64::from(bytes[4])),
                        None,
                    )
                } else {
                    let Some(bytes) = effect_data.filter(|b| b.len() == 4) else {
                        omissions.record(
                            OmissionCategory::PerkEffectUndecoded,
                            perk_id,
                            format!(
                                "PERK {perk_id:08X} effect {effect_index}: ability missing 4-byte DATA"
                            ),
                        );
                        continue;
                    };
                    (None, None, links.resolve(u32_at(bytes, 0), &[b"SPEL"]))
                };
                let conditions = match collect_conditions(block) {
                    Ok(conditions) => conditions,
                    Err(reason) => {
                        omissions.record(
                            OmissionCategory::PerkEffectUndecoded,
                            perk_id,
                            format!(
                                "PERK {perk_id:08X} effect {effect_index} conditions: {reason}"
                            ),
                        );
                        continue;
                    }
                };
                tx.prepare_cached(
                    "INSERT INTO perk_effects(
                        perk_id, effect_index, effect_type, rank, priority,
                        quest_id, quest_stage, ability_spell_id
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?
                .execute(params![
                    perk_id,
                    effect_index as i64,
                    i64::from(effect_type),
                    i64::from(rank),
                    i64::from(priority),
                    quest_id,
                    quest_stage,
                    ability_spell_id,
                ])?;
                insert_conditions(
                    tx,
                    links,
                    perk_id,
                    "PERK_EFFECT",
                    effect_index as i64,
                    &conditions,
                )?;
            }
            2 => {
                let Some(bytes) = effect_data.filter(|b| b.len() == 3) else {
                    omissions.record(
                        OmissionCategory::PerkEffectUndecoded,
                        perk_id,
                        format!("PERK {perk_id:08X} effect {effect_index}: entry_point missing 3-byte DATA"),
                    );
                    continue;
                };
                let entry_point = bytes[0];
                let function = bytes[1];
                let condition_tab_count = bytes[2];

                let parameter_type = match epft {
                    None => None,
                    Some([kind]) if *kind <= 7 => Some(*kind),
                    Some(other) => {
                        omissions.record(
                            OmissionCategory::PerkEffectUndecoded,
                            perk_id,
                            format!(
                                "PERK {perk_id:08X} effect {effect_index}: invalid EPFT {other:?}"
                            ),
                        );
                        continue;
                    }
                };

                let (script_flags, fragment_index) = match epf3 {
                    None => (None, None),
                    Some(bytes) if bytes.len() == 4 => (
                        u16_at(bytes, 0).map(i64::from),
                        u16_at(bytes, 2).map(i64::from),
                    ),
                    Some(other) => {
                        omissions.record(
                            OmissionCategory::PerkEffectUndecoded,
                            perk_id,
                            format!(
                                "PERK {perk_id:08X} effect {effect_index}: EPF3 is {} bytes, expected 4",
                                other.len()
                            ),
                        );
                        continue;
                    }
                };

                let mut value_float_1: Option<f64> = None;
                let mut value_float_2: Option<f64> = None;
                let mut actor_value: Option<i64> = None;
                let mut target_form_id: Option<u32> = None;
                let mut value_text: Option<String> = None;

                let activate_choice_spell_optional = parameter_type == Some(4)
                    && function == 9
                    && (epf2.is_some() || epf3.is_some());
                let canonical_epfd = canonical_blocks
                    .get(effect_index)
                    .and_then(|canonical_block| first_block_subrecord(canonical_block, b"EPFD"));
                let decoded_ok = function_allows_parameter_type(function, parameter_type)
                    && match (parameter_type, epfd) {
                        (None, None) | (Some(0), _) => true,
                        // `add_activate_choice` (fn=9, EPFT=4) uses button label/flags in vanilla plugins with no spell (`None` or null FormID `0`).
                        (Some(4), None) => activate_choice_spell_optional,
                        // Intentionally unresolved EPFT=7 localized text is stripped by runtime_projection
                        // while remaining non-empty on the canonical winning record.
                        (Some(7), None) => canonical_epfd.is_some_and(|b| !b.is_empty()),
                        (None, Some(_)) | (Some(1..=6), None) => false,
                        (Some(1), Some(b)) => {
                            if let Some(v) = (b.len() == 4).then(|| f32_at(b, 0)).flatten() {
                                value_float_1 = Some(f64::from(v));
                                true
                            } else {
                                false
                            }
                        }
                        (Some(2), Some(b)) if b.len() == 8 => {
                            if matches!(function, 5 | 12 | 13 | 14) {
                                if let (Some(av), Some(mult)) = (u32_at(b, 0), f32_at(b, 4)) {
                                    actor_value = Some(i64::from(av));
                                    value_float_1 = Some(f64::from(mult));
                                    true
                                } else {
                                    false
                                }
                            } else if let (Some(first), Some(second)) = (f32_at(b, 0), f32_at(b, 4))
                            {
                                value_float_1 = Some(f64::from(first));
                                value_float_2 = Some(f64::from(second));
                                true
                            } else {
                                false
                            }
                        }
                        (Some(2), Some(_)) => false,
                        (Some(tag @ 3..=5), Some(b)) => {
                            let expected: &[&[u8; 4]] =
                                if tag == 3 { &[b"LVLI"] } else { &[b"SPEL"] };
                            match (b.len() == 4).then(|| u32_at(b, 0)).flatten() {
                                Some(0) => activate_choice_spell_optional,
                                Some(id) => {
                                    target_form_id = links.resolve(Some(id), expected);
                                    target_form_id.is_some()
                                }
                                None => false,
                            }
                        }
                        (Some(6), Some(b)) => {
                            value_text = Some(decode_subrecord_text(b));
                            true
                        }
                        (Some(7), Some(b)) if !b.is_empty() => {
                            value_text = Some(decode_subrecord_text(b));
                            true
                        }
                        (Some(_), _) => false,
                    };

                if !decoded_ok {
                    omissions.record(
                        OmissionCategory::PerkEffectUndecoded,
                        perk_id,
                        format!(
                            "PERK {perk_id:08X} effect {effect_index}: EPFT={parameter_type:?} cannot decode EPFD"
                        ),
                    );
                    continue;
                }

                let conditions = match collect_conditions(block) {
                    Ok(conditions) => conditions,
                    Err(reason) => {
                        omissions.record(
                            OmissionCategory::PerkEffectUndecoded,
                            perk_id,
                            format!(
                                "PERK {perk_id:08X} effect {effect_index} conditions: {reason}"
                            ),
                        );
                        continue;
                    }
                };

                tx.prepare_cached(
                    "INSERT INTO perk_effects(
                        perk_id, effect_index, effect_type, rank, priority,
                        quest_id, quest_stage, ability_spell_id
                    ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL)",
                )?
                .execute(params![
                    perk_id,
                    effect_index as i64,
                    i64::from(effect_type),
                    i64::from(rank),
                    i64::from(priority),
                ])?;

                let button_label = epf2.map(decode_subrecord_text);
                tx.prepare_cached(
                    "INSERT INTO perk_entry_points(
                        perk_id, effect_index, entry_point, function, condition_tab_count,
                        parameter_type, button_label, script_flags, fragment_index,
                        value_float_1, value_float_2, actor_value, target_form_id, value_text
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                )?
                .execute(params![
                    perk_id,
                    effect_index as i64,
                    i64::from(entry_point),
                    i64::from(function),
                    i64::from(condition_tab_count),
                    parameter_type.map(i64::from),
                    button_label.as_deref(),
                    script_flags,
                    fragment_index,
                    value_float_1,
                    value_float_2,
                    actor_value,
                    target_form_id,
                    value_text.as_deref(),
                ])?;

                insert_conditions(
                    tx,
                    links,
                    perk_id,
                    "PERK_EFFECT",
                    effect_index as i64,
                    &conditions,
                )?;
            }
            other => {
                omissions.record(
                    OmissionCategory::PerkEffectUndecoded,
                    perk_id,
                    format!(
                        "PERK {perk_id:08X} effect {effect_index}: unknown PRKE effect_type {other}"
                    ),
                );
            }
        }
    }
    Ok(())
}
