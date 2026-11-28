//! Record-backed movement speeds of one actor (#1065, #1066).
//!
//! Three records decide how fast an actor moves on the ground:
//!
//! - `RACE.WKMV` and `RACE.RNMV` link the actor's race to a `MOVT` for the walk slot and the run
//!   slot. An unset slot takes its default object (`DMWL` or `DMRN`).
//! - `MOVT.SPED` holds the directional speeds of that movement type, each as a walk and a run
//!   column. The forward walk column of the walk slot and the forward run column of the run slot
//!   give an actor's walk speed and run speed. This split is the engine rule that the player
//!   already used; see `docs/player-movement-record-map.md`.
//! - `NPC_.ACBS` bytes 14..16 hold the speed multiplier, a percentage of the base speed. UESP
//!   "Skyrim Mod:Mod File Format/NPC_" lists it in the ACBS table, xEdit
//!   (`wbDefinitionsTES5.pas`, NPC_ ACBS) names it "Speed Multiplier", and CommonLibSSE-NG
//!   (`TESActorBaseData.h`) exposes it as `speedMult`. When `ACBS` template flag bit 1
//!   (`Use Stats`, `0x0002`) is set, the speed multiplier comes from the `TPLT` chain
//!   (`docs/research/retail/q10-npc-templates.md`).
//!
//! This module reads those records and nothing else. A value that no record supplies is not
//! invented here: the actor gets no [`ActorMovementSpeeds`], and the caller keeps its labelled
//! stand-in. What the records do not establish is listed in `docs/player-sprint-research.md`: the
//! sprint equation, the column used in each actor state, and the order of the actor modifiers.

use crate::physics::{DefaultObjects, FORWARD_RUN, FORWARD_WALK, load_race_movement_type};
use bevy::prelude::*;
use color_eyre::{
    Result,
    eyre::{ensure, eyre},
};
use rusqlite::{Connection, OptionalExtension};
use shared::records::ArchivedRecordData;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Size of the `NPC_.ACBS` payload; source: <https://en.uesp.net/wiki/Skyrim_Mod:Mod_File_Format/NPC_>.
pub const ACBS_SIZE: usize = 24;
/// Offset of the `u16` speed multiplier in `NPC_.ACBS` (UESP NPC_ ACBS table).
const SPEED_MULTIPLIER_OFFSET: usize = 14;
/// Offset of the `u16` template flags in `NPC_.ACBS` (`docs/research/retail/q10-npc-templates.md`).
pub const ACBS_TEMPLATE_FLAGS_OFFSET: usize = 18;
/// `NPC_.ACBS` template flag bit 1 ("Use Stats"), which copies `speedMult` from `TPLT`
/// (`docs/research/retail/q10-npc-templates.md`).
pub const TEMPLATE_USE_STATS: u16 = 0x0002;

/// `NPC_.ACBS` speed multiplier. The stored value is a percentage: 100 leaves the speed as the
/// movement type authored it, 150 moves the actor one and a half times as fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeedMultiplier(u16);

impl SpeedMultiplier {
    /// The percentage that leaves every speed unchanged.
    pub const NEUTRAL: Self = Self(100);

    pub fn new(percent: u16) -> Self {
        Self(percent)
    }

    /// Reads bytes 14..16 of an `ACBS` payload. Any other payload size is not Skyrim SE's
    /// layout, so it is an error and no multiplier is guessed.
    pub fn from_acbs(acbs: &[u8]) -> Result<Self> {
        ensure!(
            acbs.len() == ACBS_SIZE,
            "ACBS length {} is unsupported; expected {ACBS_SIZE}",
            acbs.len()
        );
        let bytes = [
            acbs[SPEED_MULTIPLIER_OFFSET],
            acbs[SPEED_MULTIPLIER_OFFSET + 1],
        ];
        Ok(Self(u16::from_le_bytes(bytes)))
    }

    pub fn percent(self) -> u16 {
        self.0
    }

    /// The factor that scales a speed: `percent / 100`.
    pub fn factor(self) -> f32 {
        f32::from(self.0) / 100.0
    }
}

/// The walk and run speed of one actor in Creation units per second, with the actor's `ACBS`
/// speed multiplier already applied.
#[derive(Debug, Clone, Copy, PartialEq, Component)]
pub struct ActorMovementSpeeds {
    pub walk: f32,
    pub run: f32,
    pub multiplier: SpeedMultiplier,
}

impl ActorMovementSpeeds {
    /// Scales the movement type's forward walk and forward run speeds by the multiplier.
    pub fn new(walk_forward: f32, run_forward: f32, multiplier: SpeedMultiplier) -> Self {
        let factor = multiplier.factor();
        Self {
            walk: walk_forward * factor,
            run: run_forward * factor,
            multiplier,
        }
    }
}

/// The fields of one `NPC_` record that speed-multiplier resolution needs.
#[derive(Debug, Clone)]
struct NpcSpeedSource {
    multiplier: std::result::Result<SpeedMultiplier, String>,
    uses_stats: bool,
    template: std::result::Result<Option<u32>, String>,
}

impl NpcSpeedSource {
    fn from_record(form_id: u32, record: &ArchivedRecordData) -> Self {
        let mut acbs: Option<&[u8]> = None;
        let mut duplicate_acbs = false;
        let mut tplt: Option<&[u8]> = None;
        let mut duplicate_tplt = false;
        for field in &record.subrecords {
            match &field.tag {
                b"ACBS" => {
                    if acbs.is_some() {
                        duplicate_acbs = true;
                    } else {
                        acbs = Some(field.data.as_slice());
                    }
                }
                b"TPLT" => {
                    if tplt.is_some() {
                        duplicate_tplt = true;
                    } else {
                        tplt = Some(field.data.as_slice());
                    }
                }
                _ => {}
            }
        }
        let (multiplier, uses_stats) = if duplicate_acbs {
            (
                Err(format!("record {form_id:08X} has duplicate ACBS fields")),
                false,
            )
        } else if let Some(bytes) = acbs {
            match SpeedMultiplier::from_acbs(bytes) {
                Ok(mult) => {
                    let flags = u16::from_le_bytes([
                        bytes[ACBS_TEMPLATE_FLAGS_OFFSET],
                        bytes[ACBS_TEMPLATE_FLAGS_OFFSET + 1],
                    ]);
                    (Ok(mult), flags & TEMPLATE_USE_STATS != 0)
                }
                Err(error) => (Err(format!("record {form_id:08X} {error}")), false),
            }
        } else {
            (Err(format!("record {form_id:08X} lacks ACBS")), false)
        };
        let template = if duplicate_tplt {
            Err(format!("record {form_id:08X} has duplicate TPLT fields"))
        } else if let Some(bytes) = tplt {
            match <[u8; 4]>::try_from(bytes) {
                Ok(raw) => {
                    let id = u32::from_le_bytes(raw);
                    Ok((id != 0).then_some(id))
                }
                Err(_) => Err(format!(
                    "record {form_id:08X} TPLT length {} is unsupported; expected 4",
                    bytes.len()
                )),
            }
        } else {
            Ok(None)
        };
        Self {
            multiplier,
            uses_stats,
            template,
        }
    }
}

/// Batch-loaded `NPC_` speed-multiplier inputs from `records`, supporting `Use Stats` (`0x0002`)
/// template resolution across `NPC_` chains (`docs/research/retail/q10-npc-templates.md`).
#[derive(Debug, Default)]
struct NpcSpeedTable {
    entries: HashMap<u32, std::result::Result<NpcSpeedSource, String>>,
}

impl NpcSpeedTable {
    fn read(connection: &Connection) -> Result<Self> {
        let mut statement =
            connection.prepare("SELECT form_id, data FROM records WHERE record_type='NPC_'")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut entries = HashMap::new();
        let mut aligned = rkyv::util::AlignedVec::<16>::new();
        for row in rows {
            let (form_id, bytes) = match row {
                Ok(row) => row,
                Err(error) => {
                    warn!(
                        "actors: failed reading NPC_ row for movement speeds ({error}); skipping"
                    );
                    continue;
                }
            };
            aligned.clear();
            aligned.extend_from_slice(&bytes);
            let parsed = match rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&aligned)
            {
                Ok(record) => Ok(NpcSpeedSource::from_record(form_id, &record)),
                Err(error) => Err(format!(
                    "invalid canonical archive for NPC_ {form_id:08X}: {error}"
                )),
            };
            entries.insert(form_id, parsed);
        }
        Ok(Self { entries })
    }

    /// Resolves one `NPC_`'s speed multiplier, following `Use Stats` (`TPLT`) chains across `NPC_`
    /// records. A missing record, broken/zero/cyclic `TPLT`, or non-`NPC_` (`LVLN`) target returns
    /// an error instead of inventing a value.
    fn resolve_speed_multiplier(&self, npc: u32) -> std::result::Result<SpeedMultiplier, String> {
        let mut current_id = npc;
        let mut visited = HashSet::from([npc]);
        loop {
            let entry = self
                .entries
                .get(&current_id)
                .ok_or_else(|| format!("missing canonical NPC_ {current_id:08X}"))?;
            let source = entry.as_ref().map_err(Clone::clone)?;
            if !source.uses_stats {
                return source.multiplier.clone();
            }
            let template_id = source
                .template
                .as_ref()
                .map_err(Clone::clone)?
                .ok_or_else(|| {
                    format!("NPC_ {current_id:08X} sets Use Stats but has no nonzero TPLT")
                })?;
            if !visited.insert(template_id) {
                return Err(format!(
                    "NPC_ {npc:08X} Use Stats template chain has a cycle at {template_id:08X}"
                ));
            }
            if !self.entries.contains_key(&template_id) {
                return Err(format!(
                    "NPC_ {current_id:08X} Use Stats target {template_id:08X} is not an NPC_ record"
                ));
            }
            current_id = template_id;
        }
    }
}

/// The resolved walk and run forward speeds of one race, plus any startup diagnostics.
#[derive(Debug, Clone)]
struct ResolvedRaceSpeeds {
    walk: f32,
    run: f32,
    default_objects_absent: bool,
    warnings: Vec<String>,
}

/// The record speeds of every actor base the world database lists, keyed by `NPC_` FormID.
#[derive(Debug, Clone, Default)]
pub struct ActorMovementCatalog {
    speeds: HashMap<u32, ActorMovementSpeeds>,
    /// Count of actor bases that no record resolved, by reason.
    unresolved: BTreeMap<String, usize>,
}

impl ActorMovementCatalog {
    /// Resolves each `(NPC_ FormID, RACE FormID)` pair against the world database.
    ///
    /// A database without the movement tables, the `records` table, or the default objects
    /// gives an empty catalog. A race whose movement type is missing from `movement_types`, and
    /// an `NPC_` whose `ACBS` or `Use Stats` template chain is unreadable, leave those actors
    /// unresolved and count in the one warning that this call logs.
    pub fn read(connection: &Connection, actors: impl IntoIterator<Item = (u32, u32)>) -> Self {
        let actors: Vec<(u32, u32)> = actors.into_iter().collect();
        let mut catalog = Self::default();
        if actors.is_empty() {
            return catalog;
        }
        let has = |table: &str| -> bool {
            match connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            ) {
                Ok(exists) => exists,
                Err(error) => {
                    warn!("actors: cannot check `{table}` table ({error}); treating as absent");
                    false
                }
            }
        };
        if !(has("movement_types") && has("race_movement_links") && has("records")) {
            info!(
                "actors: the world database has no movement tables; actors keep the stand-in gait thresholds"
            );
            return catalog;
        }
        let defaults = match DefaultObjects::read(connection) {
            Ok(defaults) => defaults,
            Err(error) => {
                info!("actors: default objects are unreadable ({error:#}); no record speeds");
                return catalog;
            }
        };
        let npcs = match NpcSpeedTable::read(connection) {
            Ok(npcs) => npcs,
            Err(error) => {
                warn!(
                    "actors: cannot read NPC_ records for movement speeds ({error:#}); no record speeds"
                );
                return catalog;
            }
        };
        let mut races: HashMap<u32, std::result::Result<ResolvedRaceSpeeds, String>> =
            HashMap::new();
        let mut default_objects_absent = false;
        for (base, race) in &actors {
            let race_speeds = races
                .entry(*race)
                .or_insert_with(|| {
                    let result =
                        race_walk_run(connection, *race, &defaults).map_err(|e| format!("{e:#}"));
                    if let Ok(resolved) = &result {
                        if resolved.default_objects_absent {
                            default_objects_absent = true;
                        }
                        for warning in &resolved.warnings {
                            warn!(race_id = format_args!("{race:08X}"), "actors: {warning}");
                        }
                    }
                    result
                })
                .clone();
            let resolved = race_speeds.and_then(|race_speeds| {
                npcs.resolve_speed_multiplier(*base).map(|multiplier| {
                    if multiplier.percent() == 0 {
                        warn!(
                            npc_id = format_args!("{base:08X}"),
                            race_id = format_args!("{race:08X}"),
                            "actors: NPC_ ACBS speed multiplier is 0%; actor speeds scale to 0"
                        );
                    }
                    ActorMovementSpeeds::new(race_speeds.walk, race_speeds.run, multiplier)
                })
            });
            match resolved {
                Ok(speeds) => {
                    catalog.speeds.insert(*base, speeds);
                }
                Err(reason) => *catalog.unresolved.entry(reason).or_default() += 1,
            }
        }
        if default_objects_absent {
            warn!(
                "actors: package has no default_objects table (converted before default movement types were exported): unset actor WKMV or RNMV slots use NPC_Default_MT 0003580D by the earlier project rule. Run a metadata rebuild (--reuse-assets) to publish the table"
            );
        }
        if let Some((reason, _)) = catalog.unresolved.iter().next() {
            warn!(
                resolved = catalog.speeds.len(),
                unresolved = catalog.unresolved_count(),
                reasons = catalog.unresolved.len(),
                "actors: {} of {} actor bases have no record movement speeds and keep the stand-in gait thresholds; first reason: {reason}. A metadata rebuild (--reuse-assets) publishes the movement types",
                catalog.unresolved_count(),
                actors.len()
            );
        }
        catalog
    }

    pub fn get(&self, base_form_id: u32) -> Option<&ActorMovementSpeeds> {
        self.speeds.get(&base_form_id)
    }

    pub fn resolved_count(&self) -> usize {
        self.speeds.len()
    }

    pub fn unresolved_count(&self) -> usize {
        self.unresolved.values().sum()
    }

    /// Unresolved actor counts by reason, for reports.
    pub fn unresolved_reasons(&self) -> &BTreeMap<String, usize> {
        &self.unresolved
    }
}

/// The forward walk speed of a race's walk slot and the forward run speed of its run slot.
fn race_walk_run(
    connection: &Connection,
    race: u32,
    defaults: &DefaultObjects,
) -> Result<ResolvedRaceSpeeds> {
    let (walk_link, run_link): (Option<u32>, Option<u32>) = connection
        .query_row(
            "SELECT walk_movt_id, run_movt_id FROM race_movement_links WHERE id=?1",
            [race],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| eyre!("RACE {race:08X} has no race_movement_links row"))?;
    let walk = load_race_movement_type(connection, walk_link, "WKMV", defaults, FORWARD_WALK)?;
    let run = load_race_movement_type(connection, run_link, "RNMV", defaults, FORWARD_RUN)?;
    let mut warnings = walk.warnings;
    warnings.extend(run.warnings);
    Ok(ResolvedRaceSpeeds {
        walk: walk.profile.values[FORWARD_WALK],
        run: run.profile.values[FORWARD_RUN],
        default_objects_absent: walk.legacy_rule || run.legacy_rule,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::records::{ArchivedRecordData, ArchivedSubRecord};

    const NPC_DEFAULT_MT: u32 = 0x0003_580D;
    const HORSE_MT: u32 = 0x0001_CF24;
    const NORD: u32 = 0x0001_3746;
    const HORSE_RACE: u32 = 0x0001_0001;

    fn archive(fields: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let record = ArchivedRecordData {
            subrecords: fields
                .iter()
                .map(|(tag, data)| ArchivedSubRecord {
                    tag: *tag,
                    data: data.clone(),
                })
                .collect(),
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&record)
            .unwrap()
            .to_vec()
    }

    fn acbs(percent: u16) -> Vec<u8> {
        acbs_with_flags(percent, 0)
    }

    fn acbs_with_flags(percent: u16, template_flags: u16) -> Vec<u8> {
        let mut bytes = vec![0; ACBS_SIZE];
        bytes[SPEED_MULTIPLIER_OFFSET..SPEED_MULTIPLIER_OFFSET + 2]
            .copy_from_slice(&percent.to_le_bytes());
        bytes[ACBS_TEMPLATE_FLAGS_OFFSET..ACBS_TEMPLATE_FLAGS_OFFSET + 2]
            .copy_from_slice(&template_flags.to_le_bytes());
        bytes
    }

    /// A database with a human race on the default movement type and a horse race on its own.
    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE records (form_id INTEGER PRIMARY KEY, record_type TEXT NOT NULL,
                cell_id INTEGER, worldspace_id INTEGER, load_order INTEGER NOT NULL, data BLOB NOT NULL);
             CREATE TABLE plugins (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
                priority INTEGER NOT NULL, checksum BLOB NOT NULL);
             CREATE TABLE movement_types (id INTEGER PRIMARY KEY, editor_id TEXT NOT NULL,
                left_walk REAL NOT NULL, left_run REAL NOT NULL, right_walk REAL NOT NULL,
                right_run REAL NOT NULL, forward_walk REAL NOT NULL, forward_run REAL NOT NULL,
                back_walk REAL NOT NULL, back_run REAL NOT NULL, load_order INTEGER NOT NULL);
             CREATE TABLE race_movement_links (id INTEGER PRIMARY KEY, walk_movt_id INTEGER,
                run_movt_id INTEGER, load_order INTEGER NOT NULL);
             CREATE TABLE default_objects (key TEXT PRIMARY KEY, form_id INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO movement_types VALUES (?1,'NPC_Default_MT',80,370,80,370,80.1,370,72,205,0)",
            [NPC_DEFAULT_MT],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO movement_types VALUES (?1,'Horse_Default_MT',125.11,450,125.11,450,125.11,450,100,200,0)",
            [HORSE_MT],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO default_objects VALUES ('DMWL', ?1), ('DMRN', ?1)",
            [NPC_DEFAULT_MT],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO race_movement_links VALUES (?1, NULL, NULL, 0)",
            [NORD],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO race_movement_links VALUES (?1, ?2, ?2, 0)",
            [HORSE_RACE, HORSE_MT],
        )
        .unwrap();
        conn
    }

    fn add_npc(conn: &Connection, id: u32, data: Vec<u8>) {
        conn.execute(
            "INSERT INTO records VALUES (?1,'NPC_',NULL,NULL,0,?2)",
            rusqlite::params![id, data],
        )
        .unwrap();
    }

    #[test]
    fn the_multiplier_is_the_acbs_percentage() {
        assert_eq!(
            SpeedMultiplier::from_acbs(&acbs(100)).unwrap().factor(),
            1.0
        );
        assert_eq!(
            SpeedMultiplier::from_acbs(&acbs(150)).unwrap().factor(),
            1.5
        );
        assert_eq!(
            SpeedMultiplier::from_acbs(&acbs(200)).unwrap().percent(),
            200
        );
        assert_eq!(SpeedMultiplier::from_acbs(&acbs(50)).unwrap().factor(), 0.5);
        assert_eq!(SpeedMultiplier::NEUTRAL.factor(), 1.0);
        // Only the two bytes at 14..16 count. Neighbouring bytes must not leak in.
        let mut noisy = vec![0xFF; ACBS_SIZE];
        noisy[14..16].copy_from_slice(&120_u16.to_le_bytes());
        assert_eq!(SpeedMultiplier::from_acbs(&noisy).unwrap().percent(), 120);
        for size in [0, 14, 16, 23, 25] {
            assert!(
                SpeedMultiplier::from_acbs(&vec![0; size]).is_err(),
                "{size}"
            );
        }
    }

    #[test]
    fn speeds_scale_walk_and_run_by_the_multiplier() {
        let speeds = ActorMovementSpeeds::new(80.0, 370.0, SpeedMultiplier::new(150));
        assert_eq!((speeds.walk, speeds.run), (120.0, 555.0));
        let neutral = ActorMovementSpeeds::new(80.0, 370.0, SpeedMultiplier::NEUTRAL);
        assert_eq!((neutral.walk, neutral.run), (80.0, 370.0));
    }

    #[test]
    fn each_actor_gets_its_races_movement_types_and_its_own_multiplier() {
        let conn = database();
        add_npc(&conn, 0x100, archive(&[(*b"ACBS", acbs(100))]));
        add_npc(&conn, 0x101, archive(&[(*b"ACBS", acbs(150))]));
        add_npc(&conn, 0x102, archive(&[(*b"ACBS", acbs(100))]));
        let catalog =
            ActorMovementCatalog::read(&conn, [(0x100, NORD), (0x101, NORD), (0x102, HORSE_RACE)]);
        let plain = catalog.get(0x100).unwrap();
        assert!(
            (plain.walk - 80.1).abs() < 1e-4 && plain.run == 370.0,
            "{plain:?}"
        );
        let fast = catalog.get(0x101).unwrap();
        assert!(
            (fast.walk - 80.1 * 1.5).abs() < 1e-3 && fast.run == 555.0,
            "{fast:?}"
        );
        let horse = catalog.get(0x102).unwrap();
        assert!(
            (horse.walk - 125.11).abs() < 1e-3 && horse.run == 450.0,
            "{horse:?}"
        );
        assert_eq!(catalog.resolved_count(), 3);
        assert_eq!(catalog.unresolved_count(), 0);
    }

    #[test]
    fn use_stats_template_chains_inherit_root_speed_multiplier_and_reject_broken_chains() {
        let conn = database();
        // Root NPC with 150% speed multiplier.
        add_npc(&conn, 0x200, archive(&[(*b"ACBS", acbs(150))]));
        // Middle NPC inherits Use Stats from 0x200 (its own ACBS says 100%).
        add_npc(
            &conn,
            0x201,
            archive(&[
                (*b"ACBS", acbs_with_flags(100, TEMPLATE_USE_STATS)),
                (*b"TPLT", 0x200_u32.to_le_bytes().to_vec()),
            ]),
        );
        // Leaf NPC inherits Use Stats from 0x201 (two-hop chain -> 150%).
        add_npc(
            &conn,
            0x202,
            archive(&[
                (*b"ACBS", acbs_with_flags(80, TEMPLATE_USE_STATS)),
                (*b"TPLT", 0x201_u32.to_le_bytes().to_vec()),
            ]),
        );
        // NPC with TPLT pointing to 0x200 but Use Stats clear keeps its own 75%.
        add_npc(
            &conn,
            0x203,
            archive(&[
                (*b"ACBS", acbs_with_flags(75, 0x0001)),
                (*b"TPLT", 0x200_u32.to_le_bytes().to_vec()),
            ]),
        );
        // Broken Use Stats: missing TPLT, non-NPC (LVLN) target, and a 2-node cycle.
        add_npc(
            &conn,
            0x204,
            archive(&[(*b"ACBS", acbs_with_flags(100, TEMPLATE_USE_STATS))]),
        );
        add_npc(
            &conn,
            0x205,
            archive(&[
                (*b"ACBS", acbs_with_flags(100, TEMPLATE_USE_STATS)),
                (*b"TPLT", 0x9999_u32.to_le_bytes().to_vec()),
            ]),
        );
        add_npc(
            &conn,
            0x206,
            archive(&[
                (*b"ACBS", acbs_with_flags(100, TEMPLATE_USE_STATS)),
                (*b"TPLT", 0x207_u32.to_le_bytes().to_vec()),
            ]),
        );
        add_npc(
            &conn,
            0x207,
            archive(&[
                (*b"ACBS", acbs_with_flags(100, TEMPLATE_USE_STATS)),
                (*b"TPLT", 0x206_u32.to_le_bytes().to_vec()),
            ]),
        );
        let catalog = ActorMovementCatalog::read(
            &conn,
            [
                (0x201, NORD),
                (0x202, NORD),
                (0x203, NORD),
                (0x204, NORD),
                (0x205, NORD),
                (0x206, NORD),
            ],
        );
        assert_eq!(catalog.get(0x201).unwrap().multiplier.percent(), 150);
        assert_eq!(catalog.get(0x202).unwrap().multiplier.percent(), 150);
        assert_eq!(catalog.get(0x202).unwrap().run, 555.0);
        assert_eq!(catalog.get(0x203).unwrap().multiplier.percent(), 75);
        for broken in [0x204, 0x205, 0x206] {
            assert!(catalog.get(broken).is_none(), "{broken:x}");
        }
        assert_eq!(catalog.resolved_count(), 3);
        assert_eq!(catalog.unresolved_count(), 3);
    }

    #[test]
    fn an_actor_with_no_record_speeds_stays_unresolved_for_every_cause() {
        let conn = database();
        add_npc(&conn, 0x100, archive(&[(*b"ACBS", acbs(100))]));
        add_npc(&conn, 0x103, archive(&[(*b"ACBS", vec![0; 20])]));
        add_npc(&conn, 0x104, archive(&[(*b"EDID", b"x\0".to_vec())]));
        // A race with a link to a movement type that the database lacks.
        conn.execute(
            "INSERT INTO race_movement_links VALUES (0x2000, 0x9999, 0x9999, 0)",
            [],
        )
        .unwrap();
        add_npc(&conn, 0x105, archive(&[(*b"ACBS", acbs(100))]));
        let catalog = ActorMovementCatalog::read(
            &conn,
            [
                (0x100, 0x7777), // race has no race_movement_links row
                (0x103, NORD),   // ACBS has the wrong size
                (0x104, NORD),   // no ACBS
                (0x105, 0x2000), // linked MOVT missing from movement_types
                (0x106, NORD),   // no NPC_ record at all
            ],
        );
        assert_eq!(catalog.resolved_count(), 0);
        assert_eq!(catalog.unresolved_count(), 5);
        for base in [0x100, 0x103, 0x104, 0x105, 0x106] {
            assert!(catalog.get(base).is_none(), "{base:x}");
        }
        let reasons: Vec<&String> = catalog.unresolved_reasons().keys().collect();
        assert!(
            reasons
                .iter()
                .any(|r| r.contains("no race_movement_links row"))
        );
        assert!(
            reasons
                .iter()
                .any(|r| r.contains("is missing from movement_types"))
        );
    }

    #[test]
    fn a_database_without_movement_tables_gives_an_empty_catalog() {
        let conn = Connection::open_in_memory().unwrap();
        let catalog = ActorMovementCatalog::read(&conn, [(0x100, NORD)]);
        assert_eq!(catalog.resolved_count() + catalog.unresolved_count(), 0);
        assert!(catalog.get(0x100).is_none());
    }

    #[test]
    fn an_unset_slot_without_a_default_object_is_unresolved_not_invented() {
        let conn = database();
        conn.execute("DELETE FROM default_objects", []).unwrap();
        add_npc(&conn, 0x100, archive(&[(*b"ACBS", acbs(100))]));
        add_npc(&conn, 0x101, archive(&[(*b"ACBS", acbs(100))]));
        let catalog = ActorMovementCatalog::read(&conn, [(0x100, NORD), (0x101, HORSE_RACE)]);
        assert!(catalog.get(0x100).is_none());
        let horse = catalog
            .get(0x101)
            .expect("explicit race links still resolve without default_objects rows");
        assert!((horse.walk - 125.11).abs() < 1e-3 && horse.run == 450.0);
        assert_eq!(catalog.resolved_count(), 1);
        assert_eq!(catalog.unresolved_count(), 1);
    }

    /// Opt-in receipt over a real world database:
    /// `MUDCRAB_MOVEMENT_DB=<skyrim_world.db> cargo test ... real_world_movement_inventory
    /// -- --ignored --nocapture`. It opens the database read-only and writes nothing.
    #[test]
    #[ignore = "reads a converted world database from MUDCRAB_MOVEMENT_DB"]
    fn real_world_movement_inventory() {
        let Some(path) = std::env::var_os("MUDCRAB_MOVEMENT_DB") else {
            panic!("set MUDCRAB_MOVEMENT_DB to a skyrim_world.db path");
        };
        let conn =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut statement = conn.prepare("SELECT id, race_id FROM npcs").unwrap();
        let actors: Vec<(u32, u32)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .flatten()
            .collect();
        let catalog = ActorMovementCatalog::read(&conn, actors.iter().copied());
        println!("npcs={}", actors.len());
        println!("resolved={}", catalog.resolved_count());
        println!("unresolved={}", catalog.unresolved_count());
        for (reason, count) in catalog.unresolved_reasons() {
            println!("unresolved_reason count={count} {reason}");
        }
        let table = NpcSpeedTable::read(&conn).unwrap();
        let mut multipliers: BTreeMap<u16, usize> = BTreeMap::new();
        let mut unreadable = 0_usize;
        for (id, _) in &actors {
            match table.resolve_speed_multiplier(*id) {
                Ok(multiplier) => *multipliers.entry(multiplier.percent()).or_default() += 1,
                Err(_) => unreadable += 1,
            }
        }
        println!("acbs_unreadable={unreadable}");
        for (percent, count) in &multipliers {
            println!("acbs_speed_multiplier percent={percent} count={count}");
        }
        for id in [0x0000_0EB3_u32, 0x0001_3642, 0x0003_9B3E, 0x0001_3475] {
            if let Some(speeds) = catalog.get(id) {
                println!(
                    "npc {id:08X} walk={} run={} percent={}",
                    speeds.walk,
                    speeds.run,
                    speeds.multiplier.percent()
                );
            } else {
                println!("npc {id:08X} unresolved");
            }
        }
    }
}
