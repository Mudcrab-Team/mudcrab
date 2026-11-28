//! How a placed actor reference is sized and turned.
//!
//! Two retail rules apply here (registry rows RF-REFR-001, RF-REFR-002, RF-NPC-004 and RF-NPC-005;
//! RF-CC-004 uses the same product for jump height). The pure formulas live in the
//! `retail-rules` crate, where Kani proves them. That includes the form-type choices and the
//! product grouping, because the engine calls the crate's composed functions
//! (`placed_reference_scale` and `placed_reference_rotation`). This module reads the data, maps
//! record types to form types, and adds the engine's own guard:
//!
//! - **Scale.** The drawn scale of an actor is the reference scale times the race height for the
//!   actor's sex times the `NPC_/NAM6` height (RF-REFR-002, RF-NPC-004). The sex is `ACBS` bit 0
//!   (RF-NPC-005). The Creation Engine stores the reference scale as a whole percent; the
//!   converter publishes it as `references.scale`, so this module takes it as given. A missing
//!   `RACE` gives a race height of 1.0, and a missing `NAM6` gives 1.0.
//! - **Rotation.** A reference whose form type is `ACHR` ignores its stored X and Y angles and
//!   keeps only Z, the heading. Every other reference keeps all three angles.
//!
//! The data for the scale is already in the converted world database. The `records` table holds
//! the winning `NPC_` and `RACE` records, so the engine reads them there and no conversion is
//! needed. The race and the sex come from the `actor_models` row, which follows the "Use Traits"
//! template chain (`docs/research/retail/q10-npc-templates.md` documents `0x1403c1e30` copying
//! `height` under `Use Traits` bit 0; no formal `RF-` registry row is published yet). This
//! module follows the same chain for `NAM6`, so the three factors always come from one NPC.

use retail_rules::{
    placement::ACHR_FORM_TYPE,
    scale::{NPC_FORM_TYPE, NpcHeights, RaceHeights, npc_scale, npc_sex_index},
};
use rusqlite::Connection;
use shared::records::ArchivedRecordData;
use std::collections::{HashMap, HashSet};

/// The `NPC_` base record type of every actor reference (`ACHR`) in the vanilla game.
const NPC_RECORD_TYPE: &str = "NPC_";

/// Offsets of the male and female race height in `RACE/DATA` (registry RF-NPC-004).
const RACE_DATA_MALE_HEIGHT: usize = 16;
const RACE_DATA_FEMALE_HEIGHT: usize = 20;
/// `RACE/DATA` is at least this long when it holds both heights.
const RACE_DATA_HEIGHT_END: usize = RACE_DATA_FEMALE_HEIGHT + 4;

/// `NPC_/ACBS` holds the template flags as a `u16` at this offset. Bit 0 is "Use Traits".
const ACBS_TEMPLATE_FLAGS: usize = 18;
const TEMPLATE_USE_TRAITS: u16 = 0x0001;

/// The base record type to map to form types. A database with an `actor_models` table but no
/// `records` table publishes no base type. An actor catalog row proves that the base is an
/// `NPC_`, so that case gives `NPC_`. An explicit type is kept unchanged, including a non-actor
/// one.
pub(crate) fn effective_base_record_type(
    base_record_type: Option<&str>,
    has_actor_model: bool,
) -> Option<&str> {
    match base_record_type {
        None if has_actor_model => Some(NPC_RECORD_TYPE),
        other => other,
    }
}

/// The form type of a form that no rule here treats specially. Form type 0 is `None` in the
/// Creation Engine. Only the NPC and `ACHR` types change a result, so this stands in for every
/// other type.
const OTHER_FORM_TYPE: u8 = 0;

/// The form type of a reference's base record, from its record type (RF-REFR-002). Only an `NPC_`
/// base adds a height factor.
pub(crate) fn base_form_type(base_record_type: Option<&str>) -> u8 {
    if base_record_type == Some(NPC_RECORD_TYPE) {
        NPC_FORM_TYPE
    } else {
        OTHER_FORM_TYPE
    }
}

/// The form type of the reference itself (RF-REFR-001): `ACHR` for an actor reference, from its
/// base record type.
///
/// Every `ACHR` has an `NPC_` base, and no other reference type has one in the vanilla game, so
/// the base type stands in for the reference's own form type, which the world database does not
/// publish per reference.
pub(crate) fn reference_form_type(base_record_type: Option<&str>) -> u8 {
    if base_record_type == Some(NPC_RECORD_TYPE) {
        ACHR_FORM_TYPE
    } else {
        OTHER_FORM_TYPE
    }
}

/// The heights of an NPC (RF-NPC-004), with one engine guard.
///
/// `race_data` is the `RACE/DATA` payload, `None` when the NPC has no race record, which gives a
/// race height of 1.0. `npc_height` is `NPC_/NAM6`, `None` when the record has none, which gives
/// 1.0. A payload too short to hold both heights counts as no race, which is an assumption: the
/// registry does not say what a malformed record does.
///
/// The NPC scale must be finite and above zero. Retail would draw a zero scale as nothing, but
/// the engine rejects a singular transform for the whole reference. A bad NPC scale therefore
/// gives the default heights, whose NPC scale is exactly 1.0. This guard is engine policy, not
/// retail behaviour, and `retail_rules::scale::npc_scale` has none. No vanilla race or NPC
/// reaches it.
pub(crate) fn guarded_heights(
    npc_id: u32,
    race_id: u32,
    race_data: Option<&[u8]>,
    female: bool,
    npc_height: Option<f32>,
) -> NpcHeights {
    let heights = NpcHeights {
        race: race_data.and_then(|data| race_heights(race_id, data)),
        sex_index: npc_sex_index(female),
        npc_height,
    };
    let factor = npc_scale(heights.race, heights.sex_index, heights.npc_height);
    if factor.is_finite() && factor > 0.0 {
        heights
    } else {
        bevy::log::warn!(
            npc_id = format_args!("{npc_id:08X}"),
            race_id = format_args!("{race_id:08X}"),
            female,
            ?npc_height,
            factor,
            "actor NPC height factor is non-finite or non-positive; falling back to default height 1.0"
        );
        NpcHeights::default()
    }
}

/// Whether a uniform reference scale clears `validate_transform`'s singularity floor
/// (`scale > 1e-6` and `|det| = |scale^3| > 1e-8`).
fn usable_uniform_scale(scale: f32) -> bool {
    scale.is_finite() && scale > 1.0e-6 && (scale * scale * scale).abs() > 1.0e-8
}

/// Computes the placed scale with `retail_rules::scale::placed_reference_scale` and keeps an
/// actor reference visible if its NPC height factor would push a usable reference scale past the
/// engine's transform validation floor or overflow it.
pub(crate) fn guarded_placed_scale(
    form_id: u32,
    base_form_id: u32,
    base_form_type: u8,
    reference_scale: f32,
    heights: NpcHeights,
) -> f32 {
    let scale =
        retail_rules::scale::placed_reference_scale(base_form_type, reference_scale, heights);
    if base_form_type == NPC_FORM_TYPE
        && !usable_uniform_scale(scale)
        && usable_uniform_scale(reference_scale)
    {
        bevy::log::warn!(
            form_id = format_args!("{form_id:08X}"),
            base_form_id = format_args!("{base_form_id:08X}"),
            reference_scale,
            composed_scale = scale,
            ?heights,
            "actor scale fell outside transform validation bounds; falling back to reference scale"
        );
        reference_scale
    } else {
        scale
    }
}

/// Parses the male and female heights from `RACE/DATA` (RF-NPC-004).
fn race_heights(race_id: u32, data: &[u8]) -> Option<RaceHeights> {
    if data.len() < RACE_DATA_HEIGHT_END {
        bevy::log::warn!(
            race_id = format_args!("{race_id:08X}"),
            len = data.len(),
            expected = RACE_DATA_HEIGHT_END,
            "RACE/DATA payload too short to hold male/female heights; omitting race height"
        );
        return None;
    }
    let height = |offset: usize| -> f32 {
        f32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    };
    Some(RaceHeights {
        male: height(RACE_DATA_MALE_HEIGHT),
        female: height(RACE_DATA_FEMALE_HEIGHT),
    })
}

/// The fields of one `NPC_` record that the height factor needs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct NpcHeightSource {
    uses_traits: bool,
    template: Option<u32>,
    height: Option<f32>,
}

impl NpcHeightSource {
    fn from_subrecords<'a>(subrecords: impl IntoIterator<Item = (&'a [u8; 4], &'a [u8])>) -> Self {
        // Match `SubrecordView::find`: latch on the first occurrence of each tag regardless of
        // payload length, then validate length only on that latched payload.
        let mut acbs: Option<&[u8]> = None;
        let mut tplt: Option<&[u8]> = None;
        let mut nam6: Option<&[u8]> = None;
        for (tag, data) in subrecords {
            match tag {
                b"ACBS" if acbs.is_none() => acbs = Some(data),
                b"TPLT" if tplt.is_none() => tplt = Some(data),
                b"NAM6" if nam6.is_none() => nam6 = Some(data),
                _ => {}
            }
        }
        let uses_traits = acbs
            .and_then(|data| data.get(ACBS_TEMPLATE_FLAGS..ACBS_TEMPLATE_FLAGS + 2))
            .is_some_and(|bytes| {
                u16::from_le_bytes([bytes[0], bytes[1]]) & TEMPLATE_USE_TRAITS != 0
            });
        let template = tplt
            .and_then(|data| {
                (data.len() >= 4).then(|| u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
            })
            .filter(|id| *id != 0);
        let height = nam6.and_then(|data| {
            (data.len() == 4).then(|| f32::from_le_bytes([data[0], data[1], data[2], data[3]]))
        });
        Self {
            uses_traits,
            template,
            height,
        }
    }
}

/// Follows "Use Traits" templates from `npc_id` to the NPC whose traits apply, the same chain the
/// converter follows for race and sex. Returns `None` on a missing record, a broken or `LVLN`
/// template target, or a cycle so no mid-chain `NAM6` leaks through.
fn traits_root(npc_id: u32, npcs: &HashMap<u32, NpcHeightSource>) -> Option<NpcHeightSource> {
    let Some(mut current) = npcs.get(&npc_id).copied() else {
        bevy::log::debug!(
            npc_id = format_args!("{npc_id:08X}"),
            "actors: missing NPC_ record while resolving height"
        );
        return None;
    };
    let mut visited = HashSet::from([npc_id]);
    let mut current_id = npc_id;
    while current.uses_traits {
        let Some(template_id) = current.template else {
            bevy::log::debug!(
                npc_id = format_args!("{npc_id:08X}"),
                at = format_args!("{current_id:08X}"),
                "actors: Use Traits NPC_ template chain has no valid TPLT; omitting NAM6 height"
            );
            return None;
        };
        let Some(template) = npcs.get(&template_id).copied() else {
            bevy::log::debug!(
                npc_id = format_args!("{npc_id:08X}"),
                template_id = format_args!("{template_id:08X}"),
                "actors: Use Traits template target is missing or not an NPC_ (e.g. LVLN); omitting NAM6 height"
            );
            return None;
        };
        if !visited.insert(template_id) {
            bevy::log::debug!(
                npc_id = format_args!("{npc_id:08X}"),
                template_id = format_args!("{template_id:08X}"),
                "actors: Use Traits NPC_ template chain has a cycle; omitting NAM6 height"
            );
            return None;
        }
        current_id = template_id;
        current = template;
    }
    Some(current)
}

/// One actor to size: its `NPC_` id, race and sex.
#[derive(Debug, Clone, Copy)]
pub(super) struct ActorHeightQuery {
    pub npc_id: u32,
    pub race_id: u32,
    pub female: bool,
}

/// Reads `NPC_` and `RACE` records from the world database and returns the heights of each
/// queried actor. An actor with no usable record gets the default heights (a factor of 1.0), so a
/// database without a `records` table, or one converted before it held these types, sizes every
/// actor as before.
pub(super) fn read_npc_heights(
    connection: &Connection,
    actors: &[ActorHeightQuery],
) -> HashMap<u32, NpcHeights> {
    let has_records = match connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='records')",
        [],
        |row| row.get::<_, bool>(0),
    ) {
        Ok(exists) => exists,
        Err(error) => {
            bevy::log::info!(
                "actors: cannot check `records` table ({error}); actors keep their reference scale"
            );
            return HashMap::new();
        }
    };
    if !has_records {
        bevy::log::info!("actors: no `records` table; actors keep their reference scale");
        return HashMap::new();
    }
    let mut npcs = HashMap::new();
    let mut races: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut statement = match connection.prepare(
        "SELECT form_id, record_type, data FROM records WHERE record_type IN ('NPC_','RACE')",
    ) {
        Ok(statement) => statement,
        Err(error) => {
            bevy::log::info!(
                "actors: cannot read NPC_ and RACE records ({error}); no height scale"
            );
            return HashMap::new();
        }
    };
    let rows = match statement.query_map([], |row| {
        Ok((
            row.get::<_, u32>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Vec<u8>>(2)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(error) => {
            bevy::log::warn!(
                "actors: failed to query NPC_ and RACE records ({error}); no height scale"
            );
            return HashMap::new();
        }
    };
    for row in rows {
        let (form_id, record_type, data) = match row {
            Ok(row) => row,
            Err(error) => {
                bevy::log::warn!("actors: failed reading NPC_/RACE row ({error}); skipping");
                continue;
            }
        };
        let Ok(record) = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&data) else {
            bevy::log::debug!("actors: {record_type} {form_id:08X} has an unreadable payload");
            continue;
        };
        let fields = || {
            record
                .subrecords
                .iter()
                .map(|sub| (&sub.tag, sub.data.as_slice()))
        };
        if record_type == NPC_RECORD_TYPE {
            npcs.insert(form_id, NpcHeightSource::from_subrecords(fields()));
        } else if let Some((_, data)) = fields().find(|(tag, _)| *tag == b"DATA") {
            races.insert(form_id, data.to_vec());
        } else {
            bevy::log::debug!("actors: RACE {form_id:08X} has no DATA subrecord");
        }
    }
    actors
        .iter()
        .map(|actor| {
            let height = traits_root(actor.npc_id, &npcs).and_then(|root| root.height);
            let race = races.get(&actor.race_id).map(Vec::as_slice);
            if race.is_none() && actor.race_id != 0 {
                bevy::log::debug!(
                    npc_id = format_args!("{:08X}", actor.npc_id),
                    race_id = format_args!("{:08X}", actor.race_id),
                    "actors: missing RACE/DATA record for actor height"
                );
            }
            (
                actor.npc_id,
                guarded_heights(actor.npc_id, actor.race_id, race, actor.female, height),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use retail_rules::scale::placed_reference_scale;
    use shared::records::ArchivedSubRecord;

    /// The placed scale of a reference with scale 1.0 and these heights: the NPC factor, through
    /// the same composed function that placement calls.
    fn factor(heights: NpcHeights) -> f32 {
        placed_reference_scale(NPC_FORM_TYPE, 1.0, heights)
    }

    fn factor_of(race: Option<&[u8]>, female: bool, npc_height: Option<f32>) -> f32 {
        factor(guarded_heights(0x200, 0x100, race, female, npc_height))
    }

    fn race_data(male: f32, female: f32) -> Vec<u8> {
        let mut data = vec![0u8; 36];
        data[RACE_DATA_MALE_HEIGHT..RACE_DATA_MALE_HEIGHT + 4].copy_from_slice(&male.to_le_bytes());
        data[RACE_DATA_FEMALE_HEIGHT..RACE_DATA_FEMALE_HEIGHT + 4]
            .copy_from_slice(&female.to_le_bytes());
        data
    }

    #[test]
    fn scale_is_race_height_for_the_sex_times_npc_height() {
        let race = race_data(1.03, 0.9);
        // Male: 1.03 * 1.5. Female: 0.9 * 1.5. The two sexes read different race fields.
        assert!((factor_of(Some(&race), false, Some(1.5)) - 1.545).abs() < 1e-5);
        assert!((factor_of(Some(&race), true, Some(1.5)) - 1.35).abs() < 1e-5);
    }

    #[test]
    fn dun_hunter_mudcrab_is_the_race_height_times_the_npc_height() {
        // Example from the verified report: MudcrabRace 0.75 x dunHunterMudcrab NAM6 3.3 = 2.475.
        let race = race_data(0.75, 0.75);
        assert!((factor_of(Some(&race), false, Some(3.3)) - 2.475).abs() < 1e-5);
    }

    #[test]
    fn a_missing_race_gives_one_times_the_npc_height() {
        assert_eq!(factor_of(None, false, Some(0.8)), 0.8);
        assert_eq!(factor_of(None, true, Some(0.8)), 0.8);
    }

    #[test]
    fn a_missing_npc_height_gives_one_times_the_race_height() {
        let race = race_data(0.65, 0.65);
        assert_eq!(factor_of(Some(&race), false, None), 0.65);
        assert_eq!(factor_of(None, false, None), 1.0);
    }

    #[test]
    fn a_short_race_payload_is_no_race_height() {
        assert_eq!(factor_of(Some(&[0u8; 20]), false, Some(2.0)), 2.0);
    }

    #[test]
    fn a_non_finite_or_non_positive_product_is_one() {
        let race = race_data(f32::NAN, 0.0);
        assert_eq!(factor_of(Some(&race), false, Some(2.0)), 1.0);
        assert_eq!(factor_of(Some(&race), true, Some(2.0)), 1.0);
        assert_eq!(factor_of(None, false, Some(-1.0)), 1.0);
        assert_eq!(factor_of(None, false, Some(f32::INFINITY)), 1.0);
    }

    #[test]
    fn only_an_npc_base_gets_the_npc_and_achr_form_types() {
        assert_eq!(base_form_type(Some("NPC_")), NPC_FORM_TYPE);
        assert_eq!(reference_form_type(Some("NPC_")), ACHR_FORM_TYPE);
        for other in [Some("STAT"), Some("MISC"), Some("ACTI"), Some("REFR"), None] {
            assert_ne!(base_form_type(other), NPC_FORM_TYPE, "{other:?}");
            assert_ne!(reference_form_type(other), ACHR_FORM_TYPE, "{other:?}");
        }
    }

    #[test]
    fn an_actor_row_stands_in_for_a_missing_base_type() {
        // A legacy database has `actor_models` but no `records` table, so no base type.
        assert_eq!(effective_base_record_type(None, true), Some("NPC_"));
        assert_eq!(effective_base_record_type(None, false), None);
        // An explicit type is never replaced.
        assert_eq!(effective_base_record_type(Some("STAT"), true), Some("STAT"));
        assert_eq!(
            effective_base_record_type(Some("NPC_"), false),
            Some("NPC_")
        );
        // So an actor in such a database still keeps only its heading.
        assert_eq!(
            reference_form_type(effective_base_record_type(None, true)),
            ACHR_FORM_TYPE
        );
    }

    #[test]
    fn placement_calls_compose_the_form_type_choices() {
        use retail_rules::placement::placed_reference_rotation;
        // An actor reference: scale 1.61 x (0.75 x 3.3), and only the heading is kept.
        let heights = guarded_heights(0x200, 0x100, Some(&race_data(0.75, 0.75)), false, Some(3.3));
        let scale = placed_reference_scale(base_form_type(Some("NPC_")), 1.61, heights);
        assert!((scale - 1.61 * 2.475).abs() < 1e-4);
        assert_eq!(
            placed_reference_rotation(reference_form_type(Some("NPC_")), [0.4, -0.2, 1.25]),
            [0.0, 0.0, 1.25]
        );
        // A static reference keeps its scale and all three angles, whatever heights it is given.
        assert_eq!(
            placed_reference_scale(base_form_type(Some("STAT")), 1.61, heights),
            1.61
        );
        assert_eq!(
            placed_reference_rotation(reference_form_type(Some("STAT")), [0.4, -0.2, 1.25]),
            [0.4, -0.2, 1.25]
        );
    }

    fn acbs(template_flags: u16) -> Vec<u8> {
        let mut data = vec![0u8; 24];
        data[ACBS_TEMPLATE_FLAGS..ACBS_TEMPLATE_FLAGS + 2]
            .copy_from_slice(&template_flags.to_le_bytes());
        data
    }

    fn npc_record(template_flags: u16, template: Option<u32>, height: Option<f32>) -> Vec<u8> {
        let mut subrecords = vec![ArchivedSubRecord {
            tag: *b"ACBS",
            data: acbs(template_flags),
        }];
        if let Some(id) = template {
            subrecords.push(ArchivedSubRecord {
                tag: *b"TPLT",
                data: id.to_le_bytes().to_vec(),
            });
        }
        if let Some(height) = height {
            subrecords.push(ArchivedSubRecord {
                tag: *b"NAM6",
                data: height.to_le_bytes().to_vec(),
            });
        }
        rkyv::to_bytes::<rkyv::rancor::Error>(&ArchivedRecordData { subrecords })
            .unwrap()
            .to_vec()
    }

    fn race_record(male: f32, female: f32) -> Vec<u8> {
        rkyv::to_bytes::<rkyv::rancor::Error>(&ArchivedRecordData {
            subrecords: vec![ArchivedSubRecord {
                tag: *b"DATA",
                data: race_data(male, female),
            }],
        })
        .unwrap()
        .to_vec()
    }

    fn database(records: &[(u32, &str, Vec<u8>)]) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE records(form_id INTEGER PRIMARY KEY, record_type TEXT NOT NULL, \
                 data BLOB NOT NULL)",
            )
            .unwrap();
        for (form_id, record_type, data) in records {
            connection
                .execute(
                    "INSERT INTO records(form_id, record_type, data) VALUES (?1, ?2, ?3)",
                    rusqlite::params![form_id, record_type, data],
                )
                .unwrap();
        }
        connection
    }

    #[test]
    fn the_database_read_applies_race_sex_and_npc_height() {
        let connection = database(&[
            (0x100, "RACE", race_record(0.75, 0.5)),
            (0x200, "NPC_", npc_record(0, None, Some(3.3))),
            (0x201, "NPC_", npc_record(0, None, None)),
            (0x202, "NPC_", npc_record(0, None, Some(0.8))),
        ]);
        let query = |npc_id, race_id, female| ActorHeightQuery {
            npc_id,
            race_id,
            female,
        };
        let factors = read_npc_heights(
            &connection,
            &[
                query(0x200, 0x100, false),
                query(0x201, 0x100, true),
                // No race record: 1.0 times the NPC height.
                query(0x202, 0x999, false),
            ],
        );
        assert!((factor(factors[&0x200]) - 2.475).abs() < 1e-5);
        assert!((factor(factors[&0x201]) - 0.5).abs() < 1e-5);
        assert!((factor(factors[&0x202]) - 0.8).abs() < 1e-5);
    }

    #[test]
    fn a_use_traits_npc_takes_the_height_of_its_template() {
        let connection = database(&[
            (0x100, "RACE", race_record(1.0, 1.0)),
            (0x200, "NPC_", npc_record(1, Some(0x201), Some(9.0))),
            (0x201, "NPC_", npc_record(0, None, Some(1.1))),
            // Without the flag the template is ignored and the NPC's own height applies.
            (0x202, "NPC_", npc_record(0, Some(0x201), Some(1.4))),
        ]);
        let query = |npc_id| ActorHeightQuery {
            npc_id,
            race_id: 0x100,
            female: false,
        };
        let factors = read_npc_heights(&connection, &[query(0x200), query(0x202)]);
        assert!((factor(factors[&0x200]) - 1.1).abs() < 1e-5);
        assert!((factor(factors[&0x202]) - 1.4).abs() < 1e-5);
    }

    #[test]
    fn a_template_cycle_ends_without_looping() {
        let connection = database(&[
            (0x200, "NPC_", npc_record(1, Some(0x201), Some(1.2))),
            (0x201, "NPC_", npc_record(1, Some(0x200), Some(1.3))),
        ]);
        let factors = read_npc_heights(
            &connection,
            &[ActorHeightQuery {
                npc_id: 0x200,
                race_id: 0,
                female: false,
            }],
        );
        assert_eq!(factors[&0x200].npc_height, None);
        assert!((factor(factors[&0x200]) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_database_without_records_gives_no_factors() {
        let connection = Connection::open_in_memory().unwrap();
        let queries = [ActorHeightQuery {
            npc_id: 0x200,
            race_id: 0x100,
            female: false,
        }];
        assert!(read_npc_heights(&connection, &queries).is_empty());
    }

    #[test]
    fn retail_race_heights_match_nord_altmer_bosmer_and_khajiit() {
        let connection = database(&[
            // Skyrim SE RACE/DATA male/female heights (verified in skyrim_world.db).
            (0x0001_3746, "RACE", race_record(1.03, 1.03)), // NordRace
            (0x0001_3743, "RACE", race_record(1.08, 1.08)), // HighElfRace (Altmer)
            (0x0001_3749, "RACE", race_record(0.98, 1.00)), // WoodElfRace (Bosmer)
            (0x0001_3745, "RACE", race_record(1.00, 0.95)), // KhajiitRace
            (0x1001, "NPC_", npc_record(0, None, None)),
            (0x1002, "NPC_", npc_record(0, None, None)),
            (0x1003, "NPC_", npc_record(0, None, None)),
            (0x1004, "NPC_", npc_record(0, None, None)),
            (0x1005, "NPC_", npc_record(0, None, Some(1.15))),
        ]);
        let query = |npc_id, race_id, female| ActorHeightQuery {
            npc_id,
            race_id,
            female,
        };
        let heights = read_npc_heights(
            &connection,
            &[
                query(0x1001, 0x0001_3746, false), // Nord male
                query(0x1002, 0x0001_3743, true),  // Altmer female
                query(0x1003, 0x0001_3749, false), // Bosmer male
                query(0x1004, 0x0001_3745, true),  // Khajiit female
                query(0x1005, 0x0001_3746, false), // Nord male with own NAM6 1.15
            ],
        );
        assert!(
            (guarded_placed_scale(1, 0x1001, NPC_FORM_TYPE, 1.0, heights[&0x1001]) - 1.03).abs()
                < 1e-5,
            "Nord"
        );
        assert!(
            (guarded_placed_scale(2, 0x1002, NPC_FORM_TYPE, 1.0, heights[&0x1002]) - 1.08).abs()
                < 1e-5,
            "Altmer"
        );
        assert!(
            (guarded_placed_scale(3, 0x1003, NPC_FORM_TYPE, 1.0, heights[&0x1003]) - 0.98).abs()
                < 1e-5,
            "Bosmer"
        );
        assert!(
            (guarded_placed_scale(4, 0x1004, NPC_FORM_TYPE, 1.0, heights[&0x1004]) - 0.95).abs()
                < 1e-5,
            "Khajiit"
        );
        assert!(
            (guarded_placed_scale(5, 0x1005, NPC_FORM_TYPE, 1.2, heights[&0x1005])
                - (1.2 * (1.03 * 1.15)))
                .abs()
                < 1e-5,
            "NPC with own NAM6 and XSCL"
        );
    }

    #[test]
    fn duplicate_acbs_and_tplt_take_the_first_occurrence() {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&ArchivedRecordData {
            subrecords: vec![
                ArchivedSubRecord {
                    tag: *b"ACBS",
                    data: acbs(TEMPLATE_USE_TRAITS),
                },
                ArchivedSubRecord {
                    tag: *b"ACBS",
                    data: acbs(0),
                },
                ArchivedSubRecord {
                    tag: *b"TPLT",
                    data: 0x201u32.to_le_bytes().to_vec(),
                },
                ArchivedSubRecord {
                    tag: *b"TPLT",
                    data: 0x202u32.to_le_bytes().to_vec(),
                },
                ArchivedSubRecord {
                    tag: *b"NAM6",
                    data: 1.5f32.to_le_bytes().to_vec(),
                },
                ArchivedSubRecord {
                    tag: *b"NAM6",
                    data: 9.0f32.to_le_bytes().to_vec(),
                },
            ],
        })
        .unwrap()
        .to_vec();
        let connection = database(&[
            (0x100, "RACE", race_record(1.0, 1.0)),
            (0x200, "NPC_", bytes),
            (0x201, "NPC_", npc_record(0, None, Some(1.25))),
            (0x202, "NPC_", npc_record(0, None, Some(2.5))),
        ]);
        let heights = read_npc_heights(
            &connection,
            &[ActorHeightQuery {
                npc_id: 0x200,
                race_id: 0x100,
                female: false,
            }],
        );
        assert!((factor(heights[&0x200]) - 1.25).abs() < 1e-5);
    }

    #[test]
    fn multi_hop_use_traits_and_non_traits_leveled_template() {
        let connection = database(&[
            (0x100, "RACE", race_record(1.0, 1.0)),
            // Two-hop Use Traits chain: 0x200 -> 0x201 -> 0x202.
            (0x200, "NPC_", npc_record(1, Some(0x201), Some(0.5))),
            (0x201, "NPC_", npc_record(1, Some(0x202), Some(0.75))),
            (0x202, "NPC_", npc_record(0, None, Some(1.35))),
            // TPLT points to an LVLN (0x300, absent from NPC_ rows), without Use Traits:
            // keeps the NPC's own NAM6 height.
            (0x203, "NPC_", npc_record(0, Some(0x300), Some(1.12))),
        ]);
        let query = |npc_id| ActorHeightQuery {
            npc_id,
            race_id: 0x100,
            female: false,
        };
        let heights = read_npc_heights(&connection, &[query(0x200), query(0x203)]);
        assert!((factor(heights[&0x200]) - 1.35).abs() < 1e-5);
        assert!((factor(heights[&0x203]) - 1.12).abs() < 1e-5);
    }

    #[test]
    fn guarded_placed_scale_keeps_actor_above_transform_validation_floor() {
        let tiny = guarded_heights(0x200, 0x100, Some(&race_data(1.0, 1.0)), false, Some(0.2));
        // 0.01 * 0.2 = 0.002 has |det| = 8e-9 <= 1e-8; fallback keeps reference_scale 0.01.
        assert_eq!(
            guarded_placed_scale(1, 0x200, NPC_FORM_TYPE, 0.01, tiny),
            0.01
        );

        let huge = guarded_heights(
            0x200,
            0x100,
            Some(&race_data(1.0, 1.0)),
            false,
            Some(1.0e35),
        );
        // 1e10 * 1e35 overflows f32; fallback keeps reference_scale 1e10.
        assert_eq!(
            guarded_placed_scale(2, 0x200, NPC_FORM_TYPE, 1.0e10, huge),
            1.0e10
        );

        let normal = guarded_heights(0x200, 0x100, Some(&race_data(1.08, 1.08)), false, Some(1.1));
        assert!(
            (guarded_placed_scale(3, 0x200, NPC_FORM_TYPE, 1.5, normal) - 1.5 * (1.08 * 1.1)).abs()
                < 1e-5
        );
        assert_eq!(
            guarded_placed_scale(4, 0x200, OTHER_FORM_TYPE, 0.01, tiny),
            0.01
        );
    }

    #[test]
    fn broken_and_leveled_use_traits_chains_omit_npc_height() {
        let connection = database(&[
            (0x100, "RACE", race_record(1.0, 1.0)),
            // Use Traits set with no TPLT: do not fall back to own NAM6 1.4.
            (0x200, "NPC_", npc_record(1, None, Some(1.4))),
            // Use Traits points to an LVLN or missing record 0x300 through 0x201: omit NAM6.
            (0x201, "NPC_", npc_record(1, Some(0x300), Some(1.6))),
            (0x202, "NPC_", npc_record(1, Some(0x201), Some(1.8))),
        ]);
        let query = |npc_id| ActorHeightQuery {
            npc_id,
            race_id: 0x100,
            female: false,
        };
        let heights = read_npc_heights(&connection, &[query(0x200), query(0x202)]);
        assert_eq!(heights[&0x200].npc_height, None);
        assert_eq!(heights[&0x202].npc_height, None);
        assert!((factor(heights[&0x200]) - 1.0).abs() < 1e-5);
        assert!((factor(heights[&0x202]) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn duplicate_nam6_without_traits_takes_the_first_occurrence() {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&ArchivedRecordData {
            subrecords: vec![
                ArchivedSubRecord {
                    tag: *b"ACBS",
                    data: acbs(0),
                },
                ArchivedSubRecord {
                    tag: *b"NAM6",
                    data: 1.35f32.to_le_bytes().to_vec(),
                },
                ArchivedSubRecord {
                    tag: *b"NAM6",
                    data: 2.5f32.to_le_bytes().to_vec(),
                },
            ],
        })
        .unwrap()
        .to_vec();
        let connection = database(&[
            (0x100, "RACE", race_record(1.0, 1.0)),
            (0x200, "NPC_", bytes),
        ]);
        let heights = read_npc_heights(
            &connection,
            &[ActorHeightQuery {
                npc_id: 0x200,
                race_id: 0x100,
                female: false,
            }],
        );
        assert!((factor(heights[&0x200]) - 1.35).abs() < 1e-5);
    }

    #[test]
    fn short_first_subrecord_gives_none_instead_of_falling_through() {
        let short_then_valid = rkyv::to_bytes::<rkyv::rancor::Error>(&ArchivedRecordData {
            subrecords: vec![
                ArchivedSubRecord {
                    tag: *b"ACBS",
                    data: vec![0u8; 4],
                },
                ArchivedSubRecord {
                    tag: *b"ACBS",
                    data: acbs(TEMPLATE_USE_TRAITS),
                },
                ArchivedSubRecord {
                    tag: *b"TPLT",
                    data: vec![0u8; 2],
                },
                ArchivedSubRecord {
                    tag: *b"TPLT",
                    data: 0x201u32.to_le_bytes().to_vec(),
                },
                ArchivedSubRecord {
                    tag: *b"NAM6",
                    data: vec![0u8; 2],
                },
                ArchivedSubRecord {
                    tag: *b"NAM6",
                    data: 2.0f32.to_le_bytes().to_vec(),
                },
            ],
        })
        .unwrap()
        .to_vec();
        let parsed = NpcHeightSource::from_subrecords(
            rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&short_then_valid)
                .unwrap()
                .subrecords
                .iter()
                .map(|sub| (&sub.tag, sub.data.as_slice())),
        );
        assert_eq!(
            parsed,
            NpcHeightSource {
                uses_traits: false,
                template: None,
                height: None,
            }
        );
    }

    #[test]
    fn vanilla_actor_scale_range_with_smallest_child_clears_transform_floor() {
        // Minimum vanilla XSCL (0.5) x RACE height (0.5) x NPC NAM6 (0.5) = 0.125.
        // Minimum converted actor child node scale across the 201 actor glTFs is 0.999999.
        let min_heights =
            guarded_heights(0x200, 0x100, Some(&race_data(0.5, 0.5)), false, Some(0.5));
        let root_scale = guarded_placed_scale(1, 0x200, NPC_FORM_TYPE, 0.5, min_heights);
        assert!((root_scale - 0.125).abs() < 1e-6);
        let child_global_scale = root_scale * 0.999_999;
        assert!(child_global_scale > 1.0e-6);
        assert!((child_global_scale * child_global_scale * child_global_scale) > 1.0e-8);
    }
}
