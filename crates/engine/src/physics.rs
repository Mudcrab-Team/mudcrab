//! Player movement physics: one controller for Riverwood and the regression fixture.
//! All units are Creation units.

use bevy::{
    ecs::system::{SystemId, SystemParam},
    input::mouse::MouseMotion,
    prelude::*,
    window::{CursorGrabMode, CursorOptions, PrimaryWindow},
};
use bevy_rapier3d::prelude::*;
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use shared::{collision::CollisionBody, records::ArchivedRecordData};
use std::path::Path;

mod camera_follow;
mod camera_trace;
pub mod carry;
#[cfg(test)]
mod feel_measure;
#[cfg(test)]
mod stair_tests;
mod vertical;
#[cfg(test)]
mod view_interpolation_tests;
mod walk_support;
#[cfg(test)]
mod walk_support_tests;

use vertical::VerticalMotion;
pub use vertical::{JumpHeight, ResolvedWalk};
#[cfg(test)]
mod jump_replay_tests;

use crate::{
    console::{AppConsoleExt, ConsoleCommand, ConsoleState, console_closed},
    profiling::ProfilingState,
    streaming::{StreamingMetrics, TerrainCollider},
    world::components::StreamingCamera,
};

/// Fixed physics step: 60 Hz Rapier simulation (V15, V18).
pub const PHYSICS_TIMESTEP: f32 = 1.0 / 60.0;
const PLAYER_CONTROLLER_OFFSET: f32 = 2.0;
/// The movement type both default objects name in Skyrim.esm. It is used only for a package
/// whose `default_objects` table holds no rows, which was converted before that table existed.
const NPC_DEFAULT_MOVT_FORM_ID: u32 = 0x0003_580D;
/// Existing engine rigid-body policy, independent of character motion.
pub const RIGID_BODY_GRAVITY_CREATION: f32 = 900.0;
/// Collision groups: world surfaces (V17).
pub const GROUP_WORLD: Group = Group::GROUP_1;
/// Collision groups: player capsule (V17).
pub const GROUP_PLAYER: Group = Group::GROUP_2;
/// Collision groups: dynamic debug tankards (V17).
pub const GROUP_TANKARD: Group = Group::GROUP_3;
/// Collision groups: placed dynamic clutter from authored physics data (#104 phase a).
pub const GROUP_CLUTTER: Group = Group::GROUP_4;
/// Rapier's default linear speed cap is 400 units/s, which clips free fall under 900 units/s^2
/// gravity after 0.44 s. Authored clutter limits (about 7000 units/s) are enforced per body by
/// [`clamp_dynamic_clutter_velocities`], so the global cap only has to stay out of the way. The cap
/// is global, so debug tankards fall faster too; they and clutter both use continuous collision.
pub const MAX_LINEAR_SPEED: f32 = 20_000.0;
/// Live-cap on simultaneous dynamic clutter bodies, like [`MAX_LIVE_TANKARDS`]. The cap bounds
/// the contact-pair cost a cell arrival can create; bodies over it keep no collider at all
/// for as long as their cell stays resident (a refusal is not retried); they are considered again
/// when their cell loads next, if earlier bodies have unloaded by then.
pub const MAX_LIVE_DYNAMIC_CLUTTER: usize = 256;

/// Movement tuning with one source-attributed effective character jump height.
#[derive(Debug, Clone, Resource)]
pub struct MovementTuning {
    pub walk_speed: f32,
    pub run_speed: f32,
    /// Provisional sprint factor applied to the selected directional run speed.
    pub sprint_multiplier: f32,
    pub horizontal_acceleration: f32,
    pub jump_height: JumpHeight,
    pub capsule_radius: f32,
    pub capsule_standing_height: f32,
    pub eye_height: f32,
    pub slope_climb_degrees: f32,
    pub slope_slide_degrees: f32,
    pub autostep_height: f32,
    /// Selected MOVT's left/right/forward/back walk/run speeds, when packaged.
    pub record_speeds: Option<DirectionalMovementSpeeds>,
}

#[derive(Debug, Clone)]
pub struct DirectionalMovementSpeeds {
    /// The walk movement type's [walk, run] pairs, ordered left/right/forward/back.
    pub walk_directions: [[f32; 2]; 4],
    /// The run movement type's [walk, run] pairs, ordered left/right/forward/back.
    pub run_directions: [[f32; 2]; 4],
    pub source: String,
    /// Flagged conditions found while loading, logged once at startup. They are not errors, but
    /// they change what the player can do or show that the package is out of date.
    pub warnings: Vec<String>,
}

impl Default for MovementTuning {
    fn default() -> Self {
        Self {
            walk_speed: 160.0,
            run_speed: 300.0,
            sprint_multiplier: 1.5,
            horizontal_acceleration: 1800.0,
            jump_height: JumpHeight::synthetic(
                "synthetic unit actor; fJumpHeightMin=76; runtime reference scale=1 policy",
            ),
            capsule_radius: 28.0,
            capsule_standing_height: 100.8,
            eye_height: 89.6,
            slope_climb_degrees: 50.0,
            slope_slide_degrees: 55.0,
            autostep_height: 36.0,
            record_speeds: None,
        }
    }
}

/// One `MOVT` row: its eight directional speeds and where it came from.
struct MovementProfile {
    editor_id: String,
    /// Left/right/forward/back, each as walk then run.
    values: [f32; 8],
    plugin: String,
}

impl MovementProfile {
    fn directions(&self) -> [[f32; 2]; 4] {
        std::array::from_fn(|direction| {
            [self.values[direction * 2], self.values[direction * 2 + 1]]
        })
    }
}

/// The `DOBJ` default movement types that the converter exported.
enum DefaultObjects {
    /// The package has no `default_objects` table, so it was converted before the table existed.
    /// It is the only case that keeps the earlier `NPC_Default_MT` rule, and it is flagged.
    Absent,
    /// Default object use code and the `MOVT` it names. A key that is absent has no default,
    /// including in a package whose table is empty.
    Exported(Vec<(String, u32)>),
}

impl DefaultObjects {
    fn read(conn: &Connection) -> Result<Self> {
        let has_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='default_objects')",
            [],
            |row| row.get(0),
        )?;
        if !has_table {
            return Ok(Self::Absent);
        }
        let mut statement = conn.prepare("SELECT key, form_id FROM default_objects")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self::Exported(rows))
    }

    /// The movement type of the default object `key` (`DMWL` or `DMRN`) for the race slot
    /// `slot`, and how it was chosen.
    fn movement_type(&self, slot: &str, key: &str) -> Result<(u32, String)> {
        match self {
            Self::Absent => Ok((
                NPC_DEFAULT_MOVT_FORM_ID,
                "legacy package without default objects".to_owned(),
            )),
            Self::Exported(rows) => rows
                .iter()
                .find(|(row_key, _)| row_key == key)
                .map(|(_, id)| (*id, format!("DOBJ {key}")))
                .ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "RACE {slot} is unset and the package has a default_objects table but no row for {key}. Run a metadata rebuild (--reuse-assets). If the load order has no DOBJ with {key}, the data holds no default"
                    )
                }),
        }
    }
}

/// Load a MOVT's speeds. Retail profiles hold zero speeds in some directions, so every
/// speed must be finite and non-negative. A zero forward speed is data, not an error;
/// the caller flags it.
fn load_movement_profile(conn: &Connection, form_id: u32, via: &str) -> Result<MovementProfile> {
    let selected: Option<MovementProfile> = conn
        .query_row(
            "SELECT m.editor_id, m.left_walk, m.left_run, m.right_walk, m.right_run, \
                m.forward_walk, m.forward_run, m.back_walk, m.back_run, \
                COALESCE((SELECT MIN(p.name) FROM plugins p WHERE p.priority=m.load_order \
                          HAVING COUNT(*)=1), '<unknown plugin>') \
             FROM movement_types m \
             WHERE m.id=?1",
            params![form_id],
            |row| {
                Ok(MovementProfile {
                    editor_id: row.get(0)?,
                    values: [
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ],
                    plugin: row.get(9)?,
                })
            },
        )
        .optional()?;
    let profile = selected.ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "{via}: MOVT {form_id:08X} is missing from movement_types. Run a metadata rebuild (--reuse-assets)"
        )
    })?;
    for (index, value) in profile.values.iter().enumerate() {
        ensure!(
            value.is_finite() && *value >= 0.0,
            "{via}: MOVT {form_id:08X} SPED[{index}] is invalid: {value}"
        );
    }
    Ok(profile)
}

/// A movement type that a race slot selected.
struct SelectedMovementType {
    /// How the slot chose it, for the source report.
    via: String,
    profile: MovementProfile,
    /// The package has no `default_objects` table, so the earlier rule chose it.
    legacy_rule: bool,
    /// Flagged conditions, for the startup log.
    warnings: Vec<String>,
}

/// The `MOVT` for one race movement slot, with a label for the source report.
///
/// Skyrim's race movement-type getter returns the race's own slot when it is set. Otherwise it
/// returns the default object for that slot: `DMWL` ("Default MovementType: Walk") for `WKMV`
/// and `DMRN` ("Default MovementType: Run") for `RNMV`. The getter is at `0x1403e64a0`; the
/// slot-to-default mapping is at `0x140393860`. A default object that is missing is an
/// error. No movement type is chosen for it. The one exception is a package without the
/// `default_objects` table, which keeps the earlier rule and is flagged.
fn load_race_movement_type(
    conn: &Connection,
    link: Option<u32>,
    slot: &str,
    defaults: &DefaultObjects,
    forward: usize,
) -> Result<SelectedMovementType> {
    let key = if slot == "WKMV" { "DMWL" } else { "DMRN" };
    let legacy_rule = link.is_none() && matches!(defaults, DefaultObjects::Absent);
    let (id, via) = match link {
        Some(id) => (id, format!("{slot} {id:08X}")),
        None => {
            let (id, origin) = defaults.movement_type(slot, key)?;
            (id, format!("{slot} unset, {origin}"))
        }
    };
    let profile = load_movement_profile(conn, id, &via)?;
    if legacy_rule {
        ensure!(
            profile.editor_id == "NPC_Default_MT",
            "MOVT 0003580D EDID is {}, expected NPC_Default_MT",
            profile.editor_id
        );
    }
    let mut warnings = Vec::new();
    if profile.values[forward] == 0.0 {
        warnings.push(format!(
            "{slot} movement type {} ({id:08X}) has forward {} speed 0, so the player cannot move forward at that gait",
            profile.editor_id,
            if forward == 4 { "walk" } else { "run" }
        ));
    }
    Ok(SelectedMovementType {
        via,
        profile,
        legacy_rule,
        warnings,
    })
}

impl MovementTuning {
    pub fn capsule_half_height(&self) -> f32 {
        (self.capsule_standing_height * 0.5 - self.capsule_radius).max(1.0)
    }

    /// Legacy packages lack `movement_types`. A package with the table must contain valid
    /// profiles for the Player race's walk and run slots; otherwise it cannot claim record
    /// speeds. A slot that the race leaves unset takes its default object's movement type.
    pub fn from_world_database(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .wrap_err_with(|| format!("opening movement records in {}", path.display()))?;
        let conn = connection.unchecked_transaction()?;
        let has_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='movement_types')",
            [], |row| row.get(0),
        )?;
        if !has_table {
            return Ok(Self {
                jump_height: JumpHeight::synthetic(
                    "legacy package without movement_types; synthetic unit actor; fJumpHeightMin=76; runtime reference scale=1 policy",
                ),
                ..Self::default()
            });
        }
        let race_id: u32 = conn
            .query_row("SELECT race_id FROM npcs WHERE id=7", [], |row| row.get(0))
            .wrap_err("record-backed movement needs Player NPC_ 00000007 RNAM")?;
        let race_type: String = conn
            .query_row(
                "SELECT record_type FROM records WHERE form_id=?1",
                [race_id],
                |row| row.get(0),
            )
            .wrap_err_with(|| format!("Player NPC_ 00000007 RNAM {race_id:08X} is missing"))?;
        ensure!(
            race_type == "RACE",
            "Player NPC_ 00000007 RNAM {race_id:08X} is {race_type}, expected RACE"
        );
        let (walk_link, run_link): (Option<u32>, Option<u32>) = conn
            .query_row(
                "SELECT walk_movt_id, run_movt_id FROM race_movement_links WHERE id=?1",
                [race_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .wrap_err_with(|| {
                format!("RACE {race_id:08X} movement links missing; run a metadata rebuild (--reuse-assets)")
            })?;
        let defaults = DefaultObjects::read(&conn)?;
        let walk = load_race_movement_type(&conn, walk_link, "WKMV", &defaults, 4)?;
        let run = load_race_movement_type(&conn, run_link, "RNMV", &defaults, 5)?;
        let mut warnings = Vec::new();
        if walk.legacy_rule || run.legacy_rule {
            warnings.push(
                "package has no default_objects table (converted before default movement types were exported): an unset WKMV or RNMV uses NPC_Default_MT 0003580D by the earlier project rule. Run a metadata rebuild (--reuse-assets) to publish the table"
                    .to_owned(),
            );
        }
        warnings.extend(walk.warnings.iter().chain(&run.warnings).cloned());
        let jump_height = load_player_jump_height(&conn, race_id)
            .wrap_err("record-backed jump inputs are incomplete or unsupported")?;
        Ok(Self {
            jump_height,
            walk_speed: walk.profile.values[4],
            run_speed: run.profile.values[5],
            record_speeds: Some(DirectionalMovementSpeeds {
                walk_directions: walk.profile.directions(),
                run_directions: run.profile.directions(),
                source: format!(
                    "{}: {} ({}); {}: {} ({})",
                    walk.via,
                    walk.profile.editor_id,
                    walk.profile.plugin,
                    run.via,
                    run.profile.editor_id,
                    run.profile.plugin
                ),
                warnings,
            }),
            ..Self::default()
        })
    }

    fn target_speed(&self, strafe: Vec2, slow: bool, sprint: bool) -> f32 {
        let gait = if slow && !sprint { 0 } else { 1 };
        let Some(speeds) = &self.record_speeds else {
            let base = if gait == 0 {
                self.walk_speed
            } else {
                self.run_speed
            };
            return if sprint {
                base * self.sprint_multiplier
            } else {
                base
            };
        };
        let x = strafe.x.abs();
        let y = strafe.y.abs();
        let directions = if gait == 0 {
            &speeds.walk_directions
        } else {
            &speeds.run_directions
        };
        let lateral = directions[if strafe.x < 0.0 { 0 } else { 1 }][gait];
        let longitudinal = directions[if strafe.y < 0.0 { 2 } else { 3 }][gait];
        let base = (lateral * x + longitudinal * y) / (x + y).max(1.0);
        if sprint {
            base * self.sprint_multiplier
        } else {
            base
        }
    }
}

struct JumpInputRecord {
    id: u32,
    data: ArchivedRecordData,
    source: String,
}

fn winning_plugin(conn: &Connection, priority: u32) -> Result<String> {
    let mut statement = conn.prepare("SELECT name FROM plugins WHERE priority=?1")?;
    let plugins = statement
        .query_map([priority], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        plugins.len() <= 1,
        "winning priority {priority} matches multiple plugins"
    );
    Ok(plugins
        .into_iter()
        .next()
        .unwrap_or_else(|| "<unknown plugin>".into()))
}

fn jump_input_record(conn: &Connection, id: u32, expected: &str) -> Result<JumpInputRecord> {
    let (kind, priority, bytes): (String, u32, Vec<u8>) = conn
        .query_row(
            "SELECT record_type,load_order,data FROM records WHERE form_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .wrap_err_with(|| format!("missing canonical {expected} {id:08X}"))?;
    ensure!(
        kind == expected,
        "record {id:08X} is {kind}, expected {expected}"
    );
    let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(bytes.len());
    aligned.extend_from_slice(&bytes);
    let data = rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&aligned)
        .wrap_err_with(|| format!("invalid canonical archive for {expected} {id:08X}"))?;
    Ok(JumpInputRecord {
        id,
        data,
        source: format!(
            "{expected} {id:08X} priority={priority} ({})",
            winning_plugin(conn, priority)?
        ),
    })
}

impl JumpInputRecord {
    fn field(&self, tag: &[u8; 4]) -> Result<Option<&[u8]>> {
        let mut matches = self
            .data
            .subrecords
            .iter()
            .filter(|field| &field.tag == tag);
        let field = matches.next().map(|field| field.data.as_slice());
        ensure!(
            matches.next().is_none(),
            "record {:08X} has duplicate {} fields",
            self.id,
            String::from_utf8_lossy(tag)
        );
        Ok(field)
    }

    fn required(&self, tag: &[u8; 4], size: usize) -> Result<&[u8]> {
        let bytes = self.field(tag)?.ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "record {:08X} lacks {}",
                self.id,
                String::from_utf8_lossy(tag)
            )
        })?;
        ensure!(
            bytes.len() == size,
            "record {:08X} {} length {} is unsupported; expected {size}",
            self.id,
            String::from_utf8_lossy(tag),
            bytes.len()
        );
        Ok(bytes)
    }
}

fn load_player_jump_height(conn: &Connection, race_id: u32) -> Result<JumpHeight> {
    let player = jump_input_record(conn, 7, "NPC_")?;
    let acbs = player.required(b"ACBS", 24)?;
    let female = u32::from_le_bytes(acbs[..4].try_into()?) & 1 != 0;
    let template_flags = u16::from_le_bytes(acbs[18..20].try_into()?);
    let template = player
        .field(b"TPLT")?
        .map(|bytes| bytes.try_into().map(u32::from_le_bytes))
        .transpose()
        .wrap_err("Player NPC_ TPLT must be four bytes")?;
    const USE_TRAITS: u16 = 0x0001;
    if template_flags & USE_TRAITS != 0 {
        let template = template.filter(|id| *id != 0).ok_or_else(|| {
            color_eyre::eyre::eyre!("Player NPC_ sets Use Traits but has no nonzero TPLT")
        })?;
        bail!(
            "Player NPC_ inherits traits from TPLT {template:08X}; inherited jump inputs are unsupported"
        );
    }
    // Raw RNAM may be file-relative. Its normalized identity comes from npcs.
    player.required(b"RNAM", 4)?;
    let npc_height = f32::from_le_bytes(player.required(b"NAM6", 4)?.try_into()?);
    ensure!(
        npc_height.is_finite() && npc_height > 0.0,
        "Player NPC_ NAM6 height is invalid: {npc_height}"
    );
    let race = jump_input_record(conn, race_id, "RACE")?;
    let data = race
        .field(b"DATA")?
        .ok_or_else(|| color_eyre::eyre::eyre!("RACE {race_id:08X} lacks DATA heights"))?;
    ensure!(
        matches!(data.len(), 124 | 128 | 164),
        "RACE {race_id:08X} DATA length {} is unsupported; expected 124, 128, or 164 bytes",
        data.len()
    );
    let heights = [
        f32::from_le_bytes(data[16..20].try_into()?),
        f32::from_le_bytes(data[20..24].try_into()?),
    ];
    ensure!(
        heights
            .iter()
            .all(|height| height.is_finite() && *height > 0.0),
        "RACE {race_id:08X} DATA heights are invalid: {heights:?}"
    );
    let race_height = heights[usize::from(female)];
    let mut query = conn.prepare(
        "SELECT id,value,load_order FROM movement_game_settings WHERE editor_id='fJumpHeightMin' COLLATE NOCASE"
    ).wrap_err("missing movement_game_settings; reconvert assets")?;
    let settings = query
        .query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, f32>(1)?,
                row.get::<_, u32>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        !settings.is_empty(),
        "expected one effective fJumpHeightMin, found 0"
    );
    ensure!(
        settings.len() == 1,
        "expected one effective fJumpHeightMin, found {}; multiple GMST FormIDs with this EDID are unsupported; inspect plugin load order and settings",
        settings.len()
    );
    let (setting_id, setting, priority) = settings[0];
    ensure!(setting.is_finite(), "GMST fJumpHeightMin is nonfinite");
    let actor_scale = race_height * npc_height;
    ensure!(
        actor_scale.is_finite(),
        "Player NPC_/RACE height product overflows"
    );
    let reference_scale = 1.0;
    let effective = setting * (actor_scale * reference_scale);
    JumpHeight::new(
        effective,
        format!(
            "GMST {setting_id:08X} fJumpHeightMin={setting} priority={priority} ({}); {}; sex={}; NAM6={npc_height}; {}; selected DATA height={race_height}; runtime reference scale=1 policy (no save mutation or swimming)",
            winning_plugin(conn, priority)?,
            player.source,
            if female { "female" } else { "male" },
            race.source,
        ),
    )
}

/// Debug tankard fixture bounds in Creation units (V15).
pub const TANKARD_CUP_RADIUS: f32 = 14.0;
pub const TANKARD_CUP_HALF_HEIGHT: f32 = 18.0;
pub const TANKARD_HANDLE_RADIUS: f32 = 4.0;
pub const TANKARD_HANDLE_HALF_HEIGHT: f32 = 12.0;
/// P2+ cap on live spawned tankards (V15).
pub const MAX_LIVE_TANKARDS: usize = 32;

/// Marker for the asset-free debug tankard physics fixture (V15).
#[derive(Component, Debug, Clone, Copy)]
pub struct DebugTankard;

/// Marker for fixture arena geometry (P1 primitive hill/wall arena).
#[derive(Component, Debug, Clone, Copy)]
pub struct FixtureArena;

/// Groups every collider belongs to / collides with (V17).
pub fn world_collision_groups() -> CollisionGroups {
    CollisionGroups::new(GROUP_WORLD, GROUP_PLAYER | GROUP_TANKARD | GROUP_CLUTTER)
}

pub fn player_collision_groups() -> CollisionGroups {
    CollisionGroups::new(GROUP_PLAYER, GROUP_WORLD | GROUP_TANKARD | GROUP_CLUTTER)
}

pub fn tankard_collision_groups() -> CollisionGroups {
    CollisionGroups::new(
        GROUP_TANKARD,
        GROUP_WORLD | GROUP_PLAYER | GROUP_TANKARD | GROUP_CLUTTER,
    )
}

/// Placed dynamic clutter collides with the world, the player, tankards and other clutter.
pub fn clutter_collision_groups() -> CollisionGroups {
    CollisionGroups::new(
        GROUP_CLUTTER,
        GROUP_WORLD | GROUP_PLAYER | GROUP_TANKARD | GROUP_CLUTTER,
    )
}

/// A placed reference simulated as a dynamic Rapier body from its authored physics data (#104
/// phase a).
///
/// The object's stable ID is the [`crate::world::components::FormId`] already on the reference
/// entity. The resting pose is not persisted (#90), and the body despawns with its cell.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct DynamicClutter {
    /// Authored cap on linear speed, Creation units/s.
    pub max_linear_velocity: f32,
    /// Authored cap on angular speed, rad/s.
    pub max_angular_velocity: f32,
}

/// Mass properties for an authored body, or `None` when the mass or tensor is unusable (the
/// caller then falls back to collider density).
///
/// The tensor must be finite, symmetric and positive definite, the centre of mass finite and the
/// mass finite and above zero. Havok values come in the shape frame already (the converter bakes
/// the node and rigid-body transforms), in Creation units and kg x Creation units squared.
pub fn mass_properties_from_body(body: &CollisionBody) -> Option<MassProperties> {
    if !body.mass.is_finite() || body.mass <= 0.0 {
        return None;
    }
    if !body
        .inertia
        .iter()
        .chain(&body.center_of_mass)
        .all(|value| value.is_finite())
    {
        return None;
    }
    let i = body.inertia.map(f64::from);
    // Row-major 3x3. Sylvester's criterion on the leading minors plus a symmetry check.
    let symmetric = |a: f64, b: f64| (a - b).abs() <= 1e-4 * (a.abs() + b.abs()).max(1e-12);
    if !(symmetric(i[1], i[3]) && symmetric(i[2], i[6]) && symmetric(i[5], i[7])) {
        return None;
    }
    let minor2 = i[0] * i[4] - i[1] * i[3];
    let det = i[0] * (i[4] * i[8] - i[5] * i[7]) - i[1] * (i[3] * i[8] - i[5] * i[6])
        + i[2] * (i[3] * i[7] - i[4] * i[6]);
    if !(i[0] > 0.0 && minor2 > 0.0 && det > 0.0) {
        return None;
    }
    // Bevy's Mat3 is column-major; the tensor is symmetric, so rows and columns agree.
    let matrix = Mat3::from_cols_array(&body.inertia);
    Some(MassProperties::from_rapier(
        bevy_rapier3d::rapier::dynamics::MassProperties::with_inertia_matrix(
            Vec3::from_array(body.center_of_mass),
            body.mass,
            matrix,
        ),
    ))
}

/// Make `entity` a dynamic Rapier body from authored physics data (#104 phase a).
///
/// `parts` are the body's collider shapes and their translations in the entity's frame, already
/// built by the caller (a hull for a non-convex body). `mass` is the authored tensor; when it is
/// `None` (unusable values) collider density supplies the mass instead. `scale` is the
/// reference's uniform scale: Rapier scales the colliders from the `GlobalTransform` but not
/// [`AdditionalMassProperties`], so the authored centre of mass is scaled by it and the inertia by
/// its square, keeping them in step with the scaled shapes (the authored mass stays as it is). An
/// unusable scale falls back to collider density. Every collider carries
/// zero mass when the authored tensor is used, because collider mass is *added* to
/// [`AdditionalMassProperties`] rather than replacing it.
///
/// The body sits on the reference entity itself, which is a descendant of the cell root: it moves
/// with the root and despawns with the cell.
/// The character controller does not push dynamic bodies yet (`apply_impulse_to_dynamic_bodies`
/// is false), so the player collides with clutter but cannot shove it in phase (a).
pub fn spawn_dynamic_clutter<'a>(
    commands: &mut Commands,
    entity: Entity,
    body: &CollisionBody,
    parts: impl IntoIterator<Item = (Vec3, &'a Collider)>,
    mass: Option<MassProperties>,
    scale: f32,
) {
    let finite_or = |value: f32, default: f32| {
        if value.is_finite() && value >= 0.0 {
            value
        } else {
            default
        }
    };
    let mut entity_commands = commands.entity(entity);
    entity_commands.insert((
        RigidBody::Dynamic,
        DynamicClutter {
            max_linear_velocity: body.max_linear_velocity,
            max_angular_velocity: body.max_angular_velocity,
        },
        Damping {
            linear_damping: finite_or(body.linear_damping, 0.0),
            angular_damping: finite_or(body.angular_damping, 0.0),
        },
        Velocity::zero(),
        // Asleep until something touches it: each model becomes ready on its own, so the shelf or
        // terrain under a body may still be loading (or its collider disabled) on the first step,
        // and an awake body would fall through it. A contact, a grab or a set velocity wakes it.
        Sleeping {
            sleeping: true,
            ..default()
        },
        // The raised global speed cap lets a fast body cover more than a floor's thickness in
        // one 60 Hz step, so clutter gets continuous collision detection against the world.
        Ccd::enabled(),
    ));
    let collider_mass = match mass {
        Some(mut properties) if scale.is_finite() && scale > 0.0 => {
            properties.local_center_of_mass *= scale;
            properties.principal_inertia *= scale * scale;
            entity_commands.insert(AdditionalMassProperties::MassProperties(properties));
            ColliderMassProperties::Mass(0.0)
        }
        _ => ColliderMassProperties::Density(0.001),
    };
    // The Havok deactivator type is not mapped in phase (a): past the asleep start, Rapier's
    // default sleeping applies.
    let friction = Friction::coefficient(finite_or(body.friction, 0.5));
    let restitution = Restitution::coefficient(finite_or(body.restitution, 0.0));
    let parts: Vec<(Vec3, &Collider)> = parts.into_iter().collect();
    if let [(translation, collider)] = parts.as_slice()
        && *translation == Vec3::ZERO
    {
        commands.entity(entity).insert((
            (*collider).clone(),
            collider_mass,
            friction,
            restitution,
            clutter_collision_groups(),
        ));
        return;
    }
    for (translation, collider) in parts {
        commands.spawn((
            (*collider).clone(),
            collider_mass,
            friction,
            restitution,
            clutter_collision_groups(),
            Transform::from_translation(translation),
            ChildOf(entity),
        ));
    }
}

/// Clamp each dynamic clutter body to its authored max linear and angular speed.
///
/// Rapier has one global linear cap and no per-body or angular cap. Running after the Rapier
/// writeback means the corrected [`Velocity`] reaches the body at the next step. Clamps are
/// counted in `StreamingMetrics::dynamic_clutter_clamped`.
pub fn clamp_dynamic_clutter_velocities(
    mut bodies: Query<(&DynamicClutter, &mut Velocity)>,
    metrics: Option<ResMut<StreamingMetrics>>,
) {
    let mut clamped = 0u64;
    for (limits, mut velocity) in &mut bodies {
        let mut capped = *velocity;
        let linear = capped.linear.length();
        if limits.max_linear_velocity.is_finite()
            && limits.max_linear_velocity > 0.0
            && linear > limits.max_linear_velocity
        {
            capped.linear *= limits.max_linear_velocity / linear;
        }
        let angular = capped.angular.length();
        if limits.max_angular_velocity.is_finite()
            && limits.max_angular_velocity > 0.0
            && angular > limits.max_angular_velocity
        {
            capped.angular *= limits.max_angular_velocity / angular;
        }
        if capped != *velocity {
            *velocity = capped;
            clamped += 1;
        }
    }
    if clamped > 0
        && let Some(mut metrics) = metrics
    {
        metrics.dynamic_clutter_clamped = metrics.dynamic_clutter_clamped.saturating_add(clamped);
    }
}

/// Compound cup + handle collider for the debug tankard (R8, V15).
pub fn debug_tankard_collider() -> Collider {
    Collider::compound(vec![
        (
            Vect::ZERO,
            Rot::default(),
            Collider::cylinder(TANKARD_CUP_HALF_HEIGHT, TANKARD_CUP_RADIUS),
        ),
        (
            Vect::new(TANKARD_CUP_RADIUS + TANKARD_HANDLE_RADIUS, 0.0, 0.0),
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            Collider::cylinder(TANKARD_HANDLE_HALF_HEIGHT, TANKARD_HANDLE_RADIUS),
        ),
    ])
}

/// Rapier setup shared by the fixture and (later) streamed collision (V17).
pub struct PhysicsCorePlugin;

impl Plugin for PhysicsCorePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(TimestepMode::Fixed {
            dt: PHYSICS_TIMESTEP,
            substeps: 1,
        })
        .insert_resource(Time::<Fixed>::from_hz(60.0))
        .init_resource::<MovementTuning>()
        .add_plugins(RapierPhysicsPlugin::<NoUserData>::default().in_fixed_schedule())
        .add_systems(Startup, (configure_physics_gravity, report_movement_tuning))
        .add_systems(
            FixedUpdate,
            clamp_dynamic_clutter_velocities.after(PhysicsSet::Writeback),
        );
    }
}

fn report_movement_tuning(tuning: Res<MovementTuning>) {
    for warning in tuning
        .record_speeds
        .iter()
        .flat_map(|speeds| &speeds.warnings)
    {
        warn!("WALK movement records: {warning}");
    }
    let source = tuning
        .record_speeds
        .as_ref()
        .map_or("legacy provisional constants", |s| s.source.as_str());
    info!(
        "WALK speeds: forward walk={} run={} Creation units/s (first-pass factor 1); provisional sprint={} ({}x directional run); source={source}; effective jump height={} Creation; jump source={}; character gravity=9.81 Havok; rigid-body gravity=900 Creation",
        tuning.walk_speed,
        tuning.run_speed,
        tuning.target_speed(Vec2::new(0.0, -1.0), false, true),
        tuning.sprint_multiplier,
        tuning.jump_height.creation(),
        tuning.jump_height.source()
    );
}

fn configure_physics_gravity(
    mut configs: Query<&mut RapierConfiguration>,
    mut simulations: Query<&mut RapierContextSimulation>,
) {
    for mut config in &mut configs {
        config.gravity = Vect::new(0.0, -RIGID_BODY_GRAVITY_CREATION, 0.0);
    }
    // Raise Rapier's global linear cap (default 400 units/s) so falling clutter is not clipped;
    // the authored per-body limits are enforced by `clamp_dynamic_clutter_velocities`.
    for mut simulation in &mut simulations {
        simulation
            .integration_parameters
            .normalized_max_linear_velocity = MAX_LINEAR_SPEED;
    }
}

/// Asset-free `--physics-fixture`: primitive arena + auto-spawned tankards.
pub struct PhysicsFixturePlugin;

impl Plugin for PhysicsFixturePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PlayerControlsPlugin)
            .init_resource::<PhysicsFixtureState>()
            .add_systems(
                Startup,
                (setup_physics_fixture, setup_fixture_player).chain(),
            )
            .add_systems(FixedUpdate, validate_physics_fixture);
    }
}

/// Mouse and movement systems shared by Riverwood and the regression fixture.
pub struct PlayerControlsPlugin;

/// Complete camera/player update, before streaming reads or rebases their render positions.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PlayerControlsSystems;

impl Plugin for PlayerControlsPlugin {
    fn build(&self, app: &mut App) {
        register_move_mode_effect(app.world_mut());
        app.add_plugins((PhysicsCorePlugin, crate::focus_pause::FocusPausePlugin))
            .init_resource::<MoveMode>()
            .init_resource::<WalkIntent>()
            .init_resource::<SprintLatch>()
            .init_resource::<WalkViewPose>()
            .init_resource::<LookIntent>()
            .init_resource::<WalkEntryStatus>()
            .init_resource::<WalkSpawnHold>()
            .init_resource::<WalkCameraFollowState>()
            .init_resource::<CursorCapture>()
            .add_message::<TeleportPlayer>()
            .add_console_command(toggle_collision_command())
            .add_systems(
                Update,
                (
                    cursor_lifecycle_system,
                    apply_teleport_player,
                    // The latch must settle before the look system reads it in the same update.
                    // Not gated by a door crossing: the look system is not gated either, so a
                    // frozen latch would turn the mouse at the wrong rate after the crossing.
                    sprint_latch_system,
                    look_input_system,
                    noclip_flight_system.run_if(crate::door_crossing::no_door_crossing),
                    // No player wishes during a crossing; safe landing physics settles below.
                    walk_intent_system.run_if(crate::door_crossing::no_door_crossing),
                    toggle_mode_system.run_if(console_closed),
                    walk_camera_follow_system,
                    overlay_system,
                )
                    .chain()
                    .after(crate::settings_ui::SettingsUiSet)
                    .in_set(PlayerControlsSystems),
            )
            .add_systems(
                FixedUpdate,
                // Input stays paused throughout a crossing. Placement and gravity settle behind
                // black once destination assets are ready, before the crossing may fade in.
                (
                    neutralize_blocked_walk_input,
                    release_walk_spawn_hold,
                    walk_movement_system,
                )
                    .chain()
                    .run_if(crate::door_crossing::walk_physics_allowed)
                    .before(PhysicsSet::SyncBackend),
            )
            // Not gated: the blend must follow the body through pauses, crossings and the console.
            .add_systems(
                FixedUpdate,
                (
                    // After the safe placement: a body moved by `release_walk_spawn_hold` must snap the
                    // view pose, not be blended from the blocked pose.
                    walk_view_begin_tick
                        .after(release_walk_spawn_hold)
                        .before(walk_movement_system),
                    walk_view_end_tick.after(PhysicsSet::Writeback),
                ),
            )
            // `--walk-camera-trace`: off unless the option names a file.
            .add_systems(Startup, camera_trace::start_walk_camera_trace)
            .add_systems(
                FixedUpdate,
                camera_trace::count_walk_camera_trace_tick
                    .after(walk_view_end_tick)
                    .run_if(resource_exists::<camera_trace::WalkCameraTrace>),
            )
            .add_systems(
                Update,
                camera_trace::write_walk_camera_trace_row
                    .after(walk_camera_follow_system)
                    .run_if(resource_exists::<camera_trace::WalkCameraTrace>),
            );
    }
}

pub struct WorldPlayerPlugin;

impl Plugin for WorldPlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PlayerControlsPlugin)
            .add_plugins(
                RapierDebugRenderPlugin {
                    default_collider_debug: ColliderDebug::NeverRender,
                    mode: DebugRenderMode::COLLIDER_SHAPES,
                    ..default()
                }
                .disabled(),
            )
            .add_plugins(carry::CarryPlugin)
            .add_console_command(toggle_collision_geometry_command())
            .add_systems(PostStartup, setup_world_player)
            .add_systems(
                Update,
                update_collision_debug_visibility
                    .after(crate::settings_ui::SettingsUiSet)
                    .before(overlay_system),
            )
            .add_systems(
                Update,
                tankard_spawn_input
                    .run_if(console_closed)
                    .after(crate::settings_ui::SettingsUiSet),
            );
    }
}

#[derive(Resource)]
struct TankardVisuals {
    cup: Handle<Mesh>,
    handle: Handle<Mesh>,
    wood: Handle<StandardMaterial>,
}

fn tankard_visuals(
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> TankardVisuals {
    TankardVisuals {
        cup: meshes.add(Cylinder::new(
            TANKARD_CUP_RADIUS,
            TANKARD_CUP_HALF_HEIGHT * 2.0,
        )),
        handle: meshes.add(Cylinder::new(
            TANKARD_HANDLE_RADIUS,
            TANKARD_HANDLE_HALF_HEIGHT * 2.0,
        )),
        wood: materials.add(fixture_material(Color::srgb(0.5, 0.32, 0.14))),
    }
}

fn spawn_debug_tankard(commands: &mut Commands, visuals: &TankardVisuals, position: Vec3) {
    commands
        .spawn((
            Name::new("Debug tankard"),
            DebugTankard,
            RigidBody::Dynamic,
            debug_tankard_collider(),
            ActiveEvents::COLLISION_EVENTS,
            CollidingEntities::default(),
            tankard_collision_groups(),
            ColliderMassProperties::Density(0.001),
            Velocity::zero(),
            // The raised global speed cap (`MAX_LINEAR_SPEED`) no longer clips a falling tankard
            // at 400 units/s, so it gets continuous collision detection like clutter does.
            Ccd::enabled(),
            Transform::from_translation(position),
            Visibility::default(),
        ))
        .with_children(|parent| {
            parent.spawn((
                Mesh3d(visuals.cup.clone()),
                MeshMaterial3d(visuals.wood.clone()),
                Transform::IDENTITY,
            ));
            parent.spawn((
                Mesh3d(visuals.handle.clone()),
                MeshMaterial3d(visuals.wood.clone()),
                Transform::from_xyz(TANKARD_CUP_RADIUS + TANKARD_HANDLE_RADIUS, 0.0, 0.0)
                    .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)),
            ));
        });
}

#[derive(Resource, Default)]
pub struct PhysicsFixtureState {
    pub ticks: u64,
    pub finished: bool,
    pub tankards_settled: bool,
}

fn fixture_material(base_color: Color) -> StandardMaterial {
    StandardMaterial {
        base_color,
        perceptual_roughness: 0.9,
        ..default()
    }
}

fn setup_physics_fixture(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.init_resource::<PhysicsFixtureMetrics>();
    let stone = materials.add(fixture_material(Color::srgb(0.42, 0.44, 0.48)));
    let grass = materials.add(fixture_material(Color::srgb(0.24, 0.5, 0.2)));
    let hazard = materials.add(fixture_material(Color::srgb(0.85, 0.3, 0.12)));

    // Flat ground pad plus a sloped hill, a step, and a blocking wall.
    let ground_mesh = meshes.add(Cuboid::new(2400.0, 40.0, 2400.0));
    commands.spawn((
        Name::new("Physics fixture ground"),
        FixtureArena,
        Mesh3d(ground_mesh),
        MeshMaterial3d(grass),
        Transform::from_xyz(0.0, -20.0, 0.0),
        RigidBody::Fixed,
        Collider::cuboid(1200.0, 20.0, 1200.0),
        world_collision_groups(),
    ));
    let hill_mesh = meshes.add(Cuboid::new(1200.0, 40.0, 600.0));
    commands.spawn((
        Name::new("Physics fixture hill"),
        FixtureArena,
        Mesh3d(hill_mesh),
        MeshMaterial3d(stone.clone()),
        Transform::from_xyz(-700.0, 190.0, -500.0).with_rotation(Quat::from_rotation_z(-0.32)),
        RigidBody::Fixed,
        Collider::cuboid(600.0, 20.0, 300.0),
        world_collision_groups(),
    ));
    let step_mesh = meshes.add(Cuboid::new(220.0, 24.0, 220.0));
    commands.spawn((
        Name::new("Physics fixture step"),
        FixtureArena,
        Mesh3d(step_mesh),
        MeshMaterial3d(stone),
        Transform::from_xyz(320.0, 12.0, 240.0),
        RigidBody::Fixed,
        Collider::cuboid(110.0, 12.0, 110.0),
        world_collision_groups(),
    ));
    let wall_mesh = meshes.add(Cuboid::new(60.0, 400.0, 1200.0));
    commands.spawn((
        Name::new("Physics fixture wall"),
        FixtureArena,
        Mesh3d(wall_mesh),
        MeshMaterial3d(hazard),
        Transform::from_xyz(760.0, 200.0, 0.0),
        RigidBody::Fixed,
        Collider::cuboid(30.0, 200.0, 600.0),
        world_collision_groups(),
    ));

    // Visible cup + handle meshes; collider stays compound convex (V15).
    let visuals = tankard_visuals(&mut meshes, &mut materials);
    for position in [
        Vec3::new(-700.0, 420.0, -500.0),
        Vec3::new(-640.0, 480.0, -420.0),
        Vec3::new(120.0, 320.0, 120.0),
    ] {
        spawn_debug_tankard(&mut commands, &visuals, position);
    }

    commands.spawn((
        Camera3d::default(),
        crate::color_pipeline::SceneColorPipeline::default(),
        Transform::from_xyz(0.0, 900.0, 1900.0).looking_at(Vec3::new(0.0, 0.0, -200.0), Vec3::Y),
        StreamingCamera,
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 12_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.8, -0.5, 0.0)),
    ));
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.48, 0.55, 0.7),
        brightness: 160.0,
        ..default()
    });
}

#[derive(Resource, Default)]
pub struct PhysicsFixtureMetrics {
    pub tankards_spawned: usize,
    pub tankards_in_contact: usize,
    pub settled_ticks: u64,
}

fn validate_physics_fixture(
    mut state: ResMut<PhysicsFixtureState>,
    mut metrics: ResMut<PhysicsFixtureMetrics>,
    mut profiler: ResMut<ProfilingState>,
    tankards: Query<(Entity, &Transform, Option<&CollidingEntities>), With<DebugTankard>>,
    mut streaming: ResMut<StreamingMetrics>,
) {
    if state.finished {
        return;
    }
    state.ticks = state.ticks.saturating_add(1);
    metrics.tankards_spawned = tankards.iter().count();
    metrics.tankards_in_contact = tankards
        .iter()
        .filter(|(_, _, contacts)| contacts.is_some_and(|contacts| !contacts.is_empty()))
        .count();
    let settled = tankards.iter().all(|(_, transform, _)| {
        transform.translation.is_finite() && transform.translation.y > -400.0
    });
    if settled && metrics.tankards_in_contact == metrics.tankards_spawned {
        metrics.settled_ticks = metrics.settled_ticks.saturating_add(1);
    } else {
        metrics.settled_ticks = 0;
    }
    if metrics.settled_ticks >= 60 {
        state.tankards_settled = true;
        state.finished = true;
        streaming.physics_fixture_validated = true;
        profiler.event("physics-fixture", "tankards_settled", None);
    } else if state.ticks >= 60 * 30 {
        state.finished = true;
        streaming.physics_fixture_failures = streaming.physics_fixture_failures.saturating_add(1);
        profiler.event("physics-fixture", "failed", None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn movement_tuning_preserves_spec_speed_ordering() {
        let tuning = MovementTuning::default();
        let forward = Vec2::new(0.0, -1.0);
        let walk = tuning.target_speed(forward, true, false);
        let run = tuning.target_speed(forward, false, false);
        let sprint = tuning.target_speed(forward, false, true);
        assert!(walk < run && run < sprint);
        assert_eq!(sprint, run * tuning.sprint_multiplier);
        assert!(tuning.horizontal_acceleration > 0.0);
        assert_eq!(tuning.jump_height.creation(), 76.0);
        assert_eq!(tuning.capsule_standing_height, 100.8);
        assert_eq!(tuning.eye_height, 89.6);
        assert_eq!(tuning.capsule_radius, 28.0);
    }

    fn fixture_record(kind: &[u8; 4], id: u32, fields: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut payload = Vec::new();
        for (tag, data) in fields {
            payload.extend_from_slice(tag);
            payload.extend_from_slice(&(data.len() as u16).to_le_bytes());
            payload.extend_from_slice(data);
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&44_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }

    fn generated_player_database(directory: &Path) -> std::path::PathBuf {
        use dummy_content::{esm, layout};
        let base = directory.join("Skyrim.esm");
        std::fs::write(
            &base,
            esm::plugin(&esm::Plugin {
                author: layout::GENERATED_AUTHOR,
                worldspace: layout::GENERATED_WORLDSPACE,
                cells: &[esm::PRESET_EXTERIOR_CELL],
                model_path: layout::GENERATED_MODEL_PATH,
                diffuse: layout::GENERATED_DIFFUSE_PATH,
                normal_texture: layout::GENERATED_NORMAL_PATH,
            })
            .unwrap(),
        )
        .unwrap();
        let header = fixture_record(
            b"TES4",
            0,
            &[
                (
                    *b"HEDR",
                    [1.7_f32.to_le_bytes().as_slice(), &[0; 8]].concat(),
                ),
                (*b"MAST", b"Skyrim.esm\0".to_vec()),
                (*b"DATA", vec![0; 8]),
            ],
        );
        // The unreferenced middle plugin makes patch-local slot one differ
        // from the normalized global slot two of its newly authored race.
        let middle = directory.join("Middle.esp");
        std::fs::write(&middle, &header).unwrap();
        let mut patch = header;
        let mut acbs = vec![0; 24];
        acbs[..4].copy_from_slice(&1_u32.to_le_bytes());
        patch.extend(fixture_record(
            b"NPC_",
            7,
            &[
                (*b"EDID", b"Player\0".to_vec()),
                (*b"RNAM", 0x0100_0811_u32.to_le_bytes().to_vec()),
                (*b"ACBS", acbs),
                (*b"NAM6", 1.2_f32.to_le_bytes().to_vec()),
            ],
        ));
        let mut data = vec![0; 128];
        data[16..20].copy_from_slice(&0.8_f32.to_le_bytes());
        data[20..24].copy_from_slice(&1.25_f32.to_le_bytes());
        patch.extend(fixture_record(
            b"RACE",
            0x0100_0811,
            &[(*b"EDID", b"OverrideRace\0".to_vec()), (*b"DATA", data)],
        ));
        patch.extend(fixture_record(
            b"GMST",
            0x0100_0810,
            &[
                (*b"EDID", b"FJUMPHEIGHTMIN\0".to_vec()),
                (*b"DATA", 100_f32.to_le_bytes().to_vec()),
            ],
        ));
        let speeds = [
            80.09_f32, 370.0, 79.75, 370.0, 80.1, 370.0, 71.93, 205.25, 3.0, 3.0, 3.0,
        ];
        patch.extend(fixture_record(
            b"MOVT",
            NPC_DEFAULT_MOVT_FORM_ID,
            &[
                (*b"EDID", b"NPC_Default_MT\0".to_vec()),
                (
                    *b"SPED",
                    speeds
                        .iter()
                        .flat_map(|value| value.to_le_bytes())
                        .collect(),
                ),
            ],
        ));
        let patch_path = directory.join("JumpOverride.esp");
        std::fs::write(&patch_path, patch).unwrap();
        let database = directory.join("skyrim_world.db");
        converter::esm::EsmParser::convert_plugins(&[base, middle, patch_path], &database).unwrap();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute("UPDATE plugins SET id=id+100", [])
            .unwrap();
        database
    }

    fn rewrite_jump_record(
        conn: &Connection,
        id: u32,
        change: impl FnOnce(&mut ArchivedRecordData),
    ) {
        let bytes: Vec<u8> = conn
            .query_row("SELECT data FROM records WHERE form_id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        let mut aligned = rkyv::util::AlignedVec::<16>::new();
        aligned.extend_from_slice(&bytes);
        let mut record =
            rkyv::from_bytes::<ArchivedRecordData, rkyv::rancor::Error>(&aligned).unwrap();
        change(&mut record);
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&record).unwrap();
        conn.execute(
            "UPDATE records SET data=?1 WHERE form_id=?2",
            params![bytes.as_slice(), id],
        )
        .unwrap();
    }

    #[test]
    fn packaged_jump_uses_winning_setting_sex_heights_and_normalized_race() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        let race: u32 = conn
            .query_row("SELECT race_id FROM npcs WHERE id=7", [], |row| row.get(0))
            .unwrap();
        assert_eq!(race, 0x0200_0811);
        let tuning = MovementTuning::from_world_database(&path).unwrap();
        assert_eq!(tuning.jump_height.creation(), 100.0 * (1.25_f32 * 1.2_f32));
        let source = tuning.jump_height.source();
        assert!(source.contains("fJumpHeightMin=100"), "{source}");
        assert!(source.contains("sex=female"), "{source}");
        assert!(source.contains("NAM6=1.2"), "{source}");
        assert!(source.contains("RACE 02000811"), "{source}");
        assert!(source.contains("priority=2 (JumpOverride.esp)"), "{source}");
        assert!(
            tuning
                .record_speeds
                .as_ref()
                .unwrap()
                .source
                .contains("JumpOverride.esp")
        );
        rewrite_jump_record(&conn, 7, |record| {
            record
                .subrecords
                .iter_mut()
                .find(|field| field.tag == *b"ACBS")
                .unwrap()
                .data[..4]
                .copy_from_slice(&0_u32.to_le_bytes());
        });
        let male = MovementTuning::from_world_database(&path).unwrap();
        assert_eq!(male.jump_height.creation(), 100.0 * (0.8_f32 * 1.2_f32));
        for setting in [-100.0_f32, 0.0] {
            conn.execute(
                "UPDATE movement_game_settings SET value=?1 WHERE editor_id='fJumpHeightMin'",
                [setting],
            )
            .unwrap();
            assert_eq!(
                MovementTuning::from_world_database(&path)
                    .unwrap()
                    .jump_height
                    .creation(),
                setting * (0.8_f32 * 1.2_f32)
            );
        }
    }

    #[test]
    fn ambiguous_movt_priority_keeps_speeds_without_attributing_a_plugin() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE plugins SET priority=1 WHERE priority=0", [])
            .unwrap();
        conn.execute(
            "UPDATE movement_types SET load_order=1 WHERE id=?1",
            [NPC_DEFAULT_MOVT_FORM_ID],
        )
        .unwrap();
        let tuning = MovementTuning::from_world_database(&path).unwrap();
        assert_eq!((tuning.walk_speed, tuning.run_speed), (80.1, 370.0));
        assert_eq!(
            tuning.record_speeds.unwrap().source,
            "WKMV unset, DOBJ DMWL: NPC_Default_MT (<unknown plugin>); \
             RNMV unset, DOBJ DMRN: NPC_Default_MT (<unknown plugin>)"
        );
    }

    #[test]
    fn packaged_jump_requires_a_supported_race_data_layout() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        for size in [124, 128, 164, 24, 123, 129] {
            rewrite_jump_record(&conn, 0x0200_0811, |record| {
                record
                    .subrecords
                    .iter_mut()
                    .find(|field| field.tag == *b"DATA")
                    .unwrap()
                    .data
                    .resize(size, 0);
            });
            let result = MovementTuning::from_world_database(&path);
            if matches!(size, 124 | 128 | 164) {
                assert_eq!(result.unwrap().jump_height.creation(), 150.0);
            } else {
                let error = format!("{:#}", result.unwrap_err());
                assert!(
                    error.contains(&format!("DATA length {size} is unsupported")),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn packaged_jump_only_requires_local_traits() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        for (flags, template, expected_error) in [
            (0x0100_u16, Some(99_u32), None),
            (0x0001, None, Some("has no nonzero TPLT")),
            (0x0001, Some(0), Some("has no nonzero TPLT")),
            (0x0101, Some(99), Some("inherits traits from TPLT 00000063")),
        ] {
            rewrite_jump_record(&conn, 7, |record| {
                let acbs = record
                    .subrecords
                    .iter_mut()
                    .find(|field| field.tag == *b"ACBS")
                    .unwrap();
                acbs.data[18..20].copy_from_slice(&flags.to_le_bytes());
                record.subrecords.retain(|field| field.tag != *b"TPLT");
                if let Some(id) = template {
                    record.subrecords.push(shared::records::ArchivedSubRecord {
                        tag: *b"TPLT",
                        data: id.to_le_bytes().to_vec(),
                    });
                }
            });
            let result = MovementTuning::from_world_database(&path);
            if let Some(message) = expected_error {
                let error = format!("{:#}", result.unwrap_err());
                assert!(error.contains(message), "{error}");
            } else {
                assert_eq!(result.unwrap().jump_height.creation(), 150.0);
            }
        }
    }

    #[test]
    fn duplicate_jump_setting_edids_report_unsupported_plugin_inputs() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO movement_game_settings(id,editor_id,value,load_order) VALUES (?1,?2,?3,?4)",
            params![0x0300_0810_u32, "fjumpheightmin", 64.0, 3],
        )
        .unwrap();
        let error = format!(
            "{:#}",
            MovementTuning::from_world_database(&path).unwrap_err()
        );
        assert!(error.contains("found 2"), "{error}");
        assert!(error.contains("multiple GMST FormIDs"), "{error}");
        assert!(error.contains("inspect plugin load order"), "{error}");
        assert!(!error.contains("reconvert"), "{error}");
    }

    #[test]
    fn modern_jump_package_rejects_partial_malformed_duplicate_and_inherited_inputs() {
        use shared::records::ArchivedSubRecord;
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        let original: Vec<u8> = conn
            .query_row("SELECT data FROM records WHERE form_id=7", [], |row| {
                row.get(0)
            })
            .unwrap();
        let assert_invalid = || assert!(MovementTuning::from_world_database(&path).is_err());
        for failure in 0..8 {
            rewrite_jump_record(&conn, 7, |record| match failure {
                0 => record.subrecords.retain(|field| field.tag != *b"NAM6"),
                1 => {
                    record
                        .subrecords
                        .iter_mut()
                        .find(|field| field.tag == *b"NAM6")
                        .unwrap()
                        .data = vec![0; 3]
                }
                2 => record.subrecords.push(ArchivedSubRecord {
                    tag: *b"NAM6",
                    data: 1_f32.to_le_bytes().to_vec(),
                }),
                3 => {
                    record
                        .subrecords
                        .iter_mut()
                        .find(|field| field.tag == *b"NAM6")
                        .unwrap()
                        .data = f32::NAN.to_le_bytes().to_vec()
                }
                4 => {
                    record
                        .subrecords
                        .iter_mut()
                        .find(|field| field.tag == *b"ACBS")
                        .unwrap()
                        .data = vec![0; 16]
                }
                5 => {
                    record
                        .subrecords
                        .iter_mut()
                        .find(|field| field.tag == *b"ACBS")
                        .unwrap()
                        .data[18..20]
                        .copy_from_slice(&1_u16.to_le_bytes());
                    record.subrecords.push(ArchivedSubRecord {
                        tag: *b"TPLT",
                        data: 99_u32.to_le_bytes().to_vec(),
                    });
                }
                6 => {
                    record
                        .subrecords
                        .iter_mut()
                        .find(|field| field.tag == *b"RNAM")
                        .unwrap()
                        .data = vec![0; 3]
                }
                7 => {
                    record
                        .subrecords
                        .iter_mut()
                        .find(|field| field.tag == *b"NAM6")
                        .unwrap()
                        .data = 0_f32.to_le_bytes().to_vec()
                }
                _ => unreachable!(),
            });
            assert_invalid();
            conn.execute("UPDATE records SET data=?1 WHERE form_id=7", [&original])
                .unwrap();
        }
        rewrite_jump_record(&conn, 7, |record| {
            record.subrecords.push(ArchivedSubRecord {
                tag: *b"TPLT",
                data: 99_u32.to_le_bytes().to_vec(),
            })
        });
        assert!(
            MovementTuning::from_world_database(&path).is_ok(),
            "inactive template pointer can retain complete own fields"
        );
        conn.execute("UPDATE records SET data=x'000102' WHERE form_id=7", [])
            .unwrap();
        assert_invalid();
        conn.execute("UPDATE records SET data=?1 WHERE form_id=7", [&original])
            .unwrap();
        let race_original: Vec<u8> = conn
            .query_row(
                "SELECT data FROM records WHERE form_id=33556497",
                [],
                |row| row.get(0),
            )
            .unwrap();
        for failure in 0..4 {
            rewrite_jump_record(&conn, 0x0200_0811, |record| {
                let data = &mut record
                    .subrecords
                    .iter_mut()
                    .find(|field| field.tag == *b"DATA")
                    .unwrap()
                    .data;
                match failure {
                    0 => data.truncate(23),
                    1 => data[20..24].copy_from_slice(&f32::INFINITY.to_le_bytes()),
                    2 => data[16..20].copy_from_slice(&(-1_f32).to_le_bytes()),
                    3 => record.subrecords.push(ArchivedSubRecord {
                        tag: *b"DATA",
                        data: vec![0; 128],
                    }),
                    _ => unreachable!(),
                }
            });
            assert_invalid();
            conn.execute(
                "UPDATE records SET data=?1 WHERE form_id=?2",
                params![&race_original, 0x0200_0811_u32],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO movement_game_settings VALUES(999,'FJUMPHEIGHTMIN',76,2)",
            [],
        )
        .unwrap();
        assert_invalid();
        conn.execute("DELETE FROM movement_game_settings WHERE id=999", [])
            .unwrap();
        conn.execute(
            "UPDATE movement_game_settings SET value=?1",
            [f64::INFINITY],
        )
        .unwrap();
        assert_invalid();
        conn.execute("DELETE FROM movement_game_settings", [])
            .unwrap();
        assert_invalid();
    }

    #[test]
    fn packaged_movement_profile_resolves_and_legacy_package_falls_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("skyrim_world.db");
        let conn = Connection::open(&path).unwrap();
        let legacy = MovementTuning::from_world_database(&path).unwrap();
        assert_eq!(legacy.run_speed, 300.0);
        assert!(legacy.record_speeds.is_none());
        drop(conn);
        std::fs::remove_file(&path).unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        let tuning = MovementTuning::from_world_database(&path).unwrap();
        assert_eq!(tuning.walk_speed, 80.1);
        assert_eq!(tuning.run_speed, 370.0);
        assert_eq!(
            tuning.target_speed(Vec2::new(0.0, -1.0), false, true),
            555.0
        );
        assert_eq!(
            tuning.target_speed(Vec2::new(0.0, 1.0), false, false),
            205.25
        );
        assert_eq!(
            tuning.target_speed(Vec2::new(-1.0, 0.0), true, false),
            80.09
        );
        assert_eq!(
            tuning.target_speed(Vec2::new(1.0, -1.0), false, false),
            370.0
        );
        assert!(
            tuning
                .record_speeds
                .unwrap()
                .source
                .contains("JumpOverride.esp")
        );
        conn.execute(
            "UPDATE records SET record_type='STAT' WHERE form_id=33556497",
            [],
        )
        .unwrap();
        assert!(
            MovementTuning::from_world_database(&path)
                .unwrap_err()
                .to_string()
                .contains("expected RACE")
        );
        conn.execute(
            "UPDATE records SET record_type='RACE' WHERE form_id=33556497",
            [],
        )
        .unwrap();
        // The generated package exports its default objects, and the source says so.
        let speeds = MovementTuning::from_world_database(&path)
            .unwrap()
            .record_speeds
            .unwrap();
        assert!(
            speeds
                .source
                .contains("WKMV unset, DOBJ DMWL: NPC_Default_MT"),
            "{}",
            speeds.source
        );
        assert!(speeds.warnings.is_empty(), "{:?}", speeds.warnings);
        // A package from before the table keeps the earlier rule, labelled and flagged.
        conn.execute("DROP TABLE default_objects", []).unwrap();
        let legacy = MovementTuning::from_world_database(&path)
            .unwrap()
            .record_speeds
            .unwrap();
        assert!(
            legacy
                .source
                .contains("WKMV unset, legacy package without default objects"),
            "{}",
            legacy.source
        );
        let [warning] = legacy.warnings.as_slice() else {
            panic!("expected one flagged warning: {:?}", legacy.warnings);
        };
        assert!(warning.contains("no default_objects table"), "{warning}");
        assert!(warning.contains("NPC_Default_MT 0003580D"), "{warning}");
        assert!(
            warning.contains("metadata rebuild (--reuse-assets)"),
            "{warning}"
        );
        conn.execute("DELETE FROM movement_types", []).unwrap();
        assert!(
            MovementTuning::from_world_database(&path)
                .unwrap_err()
                .to_string()
                .contains("MOVT 0003580D")
        );
    }

    /// Adds a second movement type to a generated package: forward walk 50, forward run 250.
    fn add_slow_movement_type(conn: &Connection) {
        conn.execute(
            "INSERT INTO movement_types VALUES(847823, 'NPC_Slow_MT', 40, 200, 40, 200, 50, 250, 30, 150, 0)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn a_race_without_links_takes_the_default_movement_types() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        add_slow_movement_type(&conn);
        // The race has no WKMV or RNMV. Each slot uses its own default object.
        conn.execute_batch(
            "INSERT OR REPLACE INTO default_objects VALUES('DMWL', 847823), ('DMRN', 219149);",
        )
        .unwrap();
        let tuning = MovementTuning::from_world_database(&path).unwrap();
        assert_eq!((tuning.walk_speed, tuning.run_speed), (50.0, 370.0));
        let speeds = tuning.record_speeds.as_ref().unwrap();
        assert_eq!(speeds.walk_directions[2], [50.0, 250.0]);
        assert_eq!(speeds.run_directions[2], [80.1, 370.0]);
        assert!(speeds.source.contains("WKMV unset, DOBJ DMWL: NPC_Slow_MT"));
        assert!(
            speeds
                .source
                .contains("RNMV unset, DOBJ DMRN: NPC_Default_MT")
        );
        // Walking forward uses the walk profile, and running forward uses the run profile.
        assert_eq!(tuning.target_speed(Vec2::new(0.0, -1.0), true, false), 50.0);
        assert_eq!(
            tuning.target_speed(Vec2::new(0.0, -1.0), false, false),
            370.0
        );
    }

    #[test]
    fn a_race_with_links_uses_its_own_movement_types_not_the_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        add_slow_movement_type(&conn);
        // Both defaults name the Default movement type, but the race links its own.
        conn.execute_batch(
            "INSERT OR REPLACE INTO default_objects VALUES('DMWL', 219149), ('DMRN', 219149);
             UPDATE race_movement_links SET walk_movt_id=847823, run_movt_id=847823
                 WHERE id=33556497;",
        )
        .unwrap();
        let tuning = MovementTuning::from_world_database(&path).unwrap();
        assert_eq!((tuning.walk_speed, tuning.run_speed), (50.0, 250.0));
        let source = tuning.record_speeds.unwrap().source;
        assert!(source.contains("WKMV 000CEFCF: NPC_Slow_MT"), "{source}");
        assert!(source.contains("RNMV 000CEFCF: NPC_Slow_MT"), "{source}");
        // One link set and one unset: only the unset slot uses its default.
        conn.execute("UPDATE race_movement_links SET run_movt_id=NULL", [])
            .unwrap();
        let mixed = MovementTuning::from_world_database(&path).unwrap();
        assert_eq!((mixed.walk_speed, mixed.run_speed), (50.0, 370.0));
        // A link to a movement type the package does not hold is an error, not a default.
        conn.execute("UPDATE race_movement_links SET walk_movt_id=999", [])
            .unwrap();
        let error = MovementTuning::from_world_database(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("WKMV 000003E7: MOVT 000003E7 is missing"),
            "{error}"
        );
        assert!(error.to_string().contains("metadata rebuild"), "{error}");
    }

    #[test]
    fn a_missing_or_unusable_default_object_invents_no_movement_type() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        // The package has the table, but no default for the unset run slot.
        conn.execute("DELETE FROM default_objects WHERE key='DMRN'", [])
            .unwrap();
        let error = format!(
            "{:#}",
            MovementTuning::from_world_database(&path).unwrap_err()
        );
        assert!(error.contains("RNMV is unset"), "{error}");
        assert!(error.contains("no row for DMRN"), "{error}");
        assert!(error.contains("metadata rebuild"), "{error}");
        // An empty table is not a legacy package: the defaults are missing, so it is an error.
        conn.execute("DELETE FROM default_objects", []).unwrap();
        let error = format!(
            "{:#}",
            MovementTuning::from_world_database(&path).unwrap_err()
        );
        assert!(error.contains("no row for DMWL"), "{error}");
        // A default that names a movement type the package lacks is also an error.
        conn.execute("INSERT INTO default_objects VALUES('DMWL', 219149)", [])
            .unwrap();
        conn.execute("INSERT INTO default_objects VALUES('DMRN', 999)", [])
            .unwrap();
        let error = MovementTuning::from_world_database(&path).unwrap_err();
        assert!(error.to_string().contains("MOVT 000003E7 is missing"));
    }

    #[test]
    fn a_zero_forward_speed_loads_and_is_flagged() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        add_slow_movement_type(&conn);
        conn.execute(
            "UPDATE movement_types SET forward_run=0 WHERE id=847823",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE default_objects SET form_id=847823 WHERE key='DMRN'",
            [],
        )
        .unwrap();
        let tuning = MovementTuning::from_world_database(&path).unwrap();
        assert_eq!(tuning.run_speed, 0.0);
        assert_eq!(tuning.target_speed(Vec2::new(0.0, -1.0), false, false), 0.0);
        let speeds = tuning.record_speeds.unwrap();
        let [warning] = speeds.warnings.as_slice() else {
            panic!("expected one flagged warning: {:?}", speeds.warnings);
        };
        assert!(warning.contains("forward run speed 0"), "{warning}");
        // A negative speed is still an error.
        conn.execute(
            "UPDATE movement_types SET forward_run=-1 WHERE id=847823",
            [],
        )
        .unwrap();
        let error = MovementTuning::from_world_database(&path).unwrap_err();
        assert!(error.to_string().contains("SPED[5]"), "{error}");
    }

    #[test]
    fn the_legacy_name_check_applies_only_to_the_fallback_not_to_links() {
        let directory = tempfile::tempdir().unwrap();
        let path = generated_player_database(directory.path());
        let conn = Connection::open(&path).unwrap();
        add_slow_movement_type(&conn);
        // A package from before the table: both slots link explicitly. One link names
        // 0003580D under another editor ID, which is data and must load.
        conn.execute_batch(
            "DROP TABLE default_objects;
             UPDATE movement_types SET editor_id='RenamedDefault_MT' WHERE id=219149;
             UPDATE race_movement_links SET walk_movt_id=219149, run_movt_id=847823
                 WHERE id=33556497;",
        )
        .unwrap();
        let tuning = MovementTuning::from_world_database(&path).unwrap();
        let speeds = tuning.record_speeds.unwrap();
        assert!(speeds.source.contains("WKMV 0003580D: RenamedDefault_MT"));
        assert!(speeds.warnings.is_empty(), "{:?}", speeds.warnings);
        // An unset slot that falls back to the renamed 0003580D is rejected.
        conn.execute("UPDATE race_movement_links SET run_movt_id=NULL", [])
            .unwrap();
        let error = MovementTuning::from_world_database(&path).unwrap_err();
        assert!(
            error.to_string().contains("expected NPC_Default_MT"),
            "{error}"
        );
    }

    #[test]
    fn tankard_collider_is_compound_cup_and_handle() {
        let collider = debug_tankard_collider();
        let compound = collider.as_compound().expect("compound tankard");
        assert_eq!(compound.shapes().len(), 2);
    }

    #[test]
    fn collision_groups_keep_world_player_tankards_in_contact() {
        let world = world_collision_groups();
        let player = player_collision_groups();
        let tankard = tankard_collision_groups();
        assert!(world.filters.contains(GROUP_PLAYER));
        assert!(world.filters.contains(GROUP_TANKARD));
        assert!(player.filters.contains(GROUP_WORLD));
        assert!(tankard.filters.contains(GROUP_WORLD));
        assert!(tankard.filters.contains(GROUP_PLAYER));
    }

    #[test]
    fn clutter_group_collides_with_world_player_tankards_and_clutter() {
        let clutter = clutter_collision_groups();
        for group in [GROUP_WORLD, GROUP_PLAYER, GROUP_TANKARD, GROUP_CLUTTER] {
            assert!(clutter.filters.contains(group));
        }
        assert!(world_collision_groups().filters.contains(GROUP_CLUTTER));
        assert!(player_collision_groups().filters.contains(GROUP_CLUTTER));
        assert!(tankard_collision_groups().filters.contains(GROUP_CLUTTER));
        assert_eq!(clutter.memberships, GROUP_CLUTTER);
    }

    fn test_body(mass: f32, inertia: [f32; 9]) -> CollisionBody {
        CollisionBody {
            node: 0,
            target: String::new(),
            shapes: vec![0],
            kind: shared::collision::BodyKind::Dynamic,
            havok: shared::collision::HavokBodyInfo {
                motion_system: 3,
                quality_type: 4,
                deactivator_type: 1,
                collision_layer: 4,
            },
            mass,
            inertia,
            center_of_mass: [0.0, 2.0, 0.0],
            linear_damping: 0.0,
            angular_damping: 0.0,
            friction: 0.5,
            restitution: 0.0,
            max_linear_velocity: 100.0,
            max_angular_velocity: 5.0,
            convex: true,
        }
    }

    #[test]
    fn mass_properties_follow_the_authored_tensor_and_reject_unusable_ones() {
        let good = mass_properties_from_body(&test_body(
            2.0,
            [3.0, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 5.0],
        ))
        .expect("positive definite tensor");
        assert!((good.mass - 2.0).abs() < 1.0e-6);
        assert!((good.local_center_of_mass.y - 2.0).abs() < 1.0e-6);
        let mut principal = [
            good.principal_inertia.x,
            good.principal_inertia.y,
            good.principal_inertia.z,
        ];
        principal.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for (got, want) in principal.iter().zip([3.0, 4.0, 5.0]) {
            assert!((got - want).abs() < 1.0e-4, "{principal:?}");
        }
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        assert!(mass_properties_from_body(&test_body(0.0, identity)).is_none());
        assert!(mass_properties_from_body(&test_body(f32::NAN, identity)).is_none());
        assert!(mass_properties_from_body(&test_body(1.0, [0.0; 9])).is_none());
        // Not positive definite: a negative principal moment.
        assert!(
            mass_properties_from_body(&test_body(
                1.0,
                [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0]
            ))
            .is_none()
        );
        // Not symmetric.
        assert!(
            mass_properties_from_body(&test_body(
                1.0,
                [1.0, 0.5, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
            ))
            .is_none()
        );
    }

    #[test]
    fn clamp_caps_authored_max_velocity_and_counts_it() {
        let mut world = World::new();
        world.init_resource::<StreamingMetrics>();
        let limits = DynamicClutter {
            max_linear_velocity: 100.0,
            max_angular_velocity: 5.0,
        };
        let fast = world
            .spawn((
                limits,
                Velocity {
                    linear: Vec3::new(300.0, 400.0, 0.0),
                    angular: Vec3::new(0.0, 20.0, 0.0),
                },
            ))
            .id();
        let slow = world
            .spawn((
                limits,
                Velocity {
                    linear: Vec3::new(10.0, 0.0, 0.0),
                    angular: Vec3::new(0.0, 1.0, 0.0),
                },
            ))
            .id();
        world
            .run_system_once(clamp_dynamic_clutter_velocities)
            .unwrap();
        let fast = *world.get::<Velocity>(fast).unwrap();
        assert!((fast.linear.length() - 100.0).abs() < 1.0e-3);
        assert!((fast.angular.length() - 5.0).abs() < 1.0e-3);
        // Direction is kept.
        assert!((fast.linear.normalize() - Vec3::new(0.6, 0.8, 0.0)).length() < 1.0e-4);
        let slow = *world.get::<Velocity>(slow).unwrap();
        assert_eq!(slow.linear, Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(
            world.resource::<StreamingMetrics>().dynamic_clutter_clamped,
            1
        );
    }

    #[test]
    fn physics_core_raises_the_global_linear_speed_cap() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, PhysicsCorePlugin));
        app.update();
        let mut query = app.world_mut().query::<&RapierContextSimulation>();
        let simulation = query.single(app.world()).expect("rapier context");
        assert_eq!(
            simulation
                .integration_parameters
                .normalized_max_linear_velocity,
            MAX_LINEAR_SPEED
        );
    }

    #[test]
    fn physics_core_uses_fixed_sixty_hertz_step() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, PhysicsCorePlugin));
        app.update();
        let mode = *app.world().resource::<TimestepMode>();
        assert_eq!(
            mode,
            TimestepMode::Fixed {
                dt: PHYSICS_TIMESTEP,
                substeps: 1,
            }
        );
        assert!((PHYSICS_TIMESTEP - 1.0 / 60.0).abs() < f32::EPSILON);
        assert_eq!(
            app.world().resource::<Time<Fixed>>().timestep(),
            std::time::Duration::from_secs_f64(1.0 / 60.0)
        );
    }
}

/// Placed dynamic clutter from authored physics data (#104 phase a), on the fixture arena.
#[cfg(test)]
mod clutter_tests {
    use super::headless;
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use shared::collision::{BodyKind, HavokBodyInfo};

    fn authored_body(mass: f32, inertia: [f32; 9], center_of_mass: [f32; 3]) -> CollisionBody {
        CollisionBody {
            node: 1,
            target: "Clutter01".to_owned(),
            shapes: vec![0],
            kind: BodyKind::Dynamic,
            havok: HavokBodyInfo {
                motion_system: 3,
                quality_type: 4,
                deactivator_type: 1,
                collision_layer: 4,
            },
            mass,
            inertia,
            center_of_mass,
            linear_damping: 0.1,
            angular_damping: 0.05,
            friction: 0.6,
            restitution: 0.3,
            max_linear_velocity: 7_000.0,
            max_angular_velocity: 31.0,
            convex: true,
        }
    }

    /// A 20-unit box whose collider sits 10 units above its reference origin, as the converter
    /// writes a NIF box shape.
    fn box_body() -> CollisionBody {
        authored_body(
            2.0,
            [30.0, 0.0, 0.0, 0.0, 40.0, 0.0, 0.0, 0.0, 50.0],
            [0.0, 10.0, 0.0],
        )
    }

    /// A 100-unit square platform, heavy enough that a tankard does not move it.
    fn platform_body() -> CollisionBody {
        authored_body(
            200.0,
            [20_000.0, 0.0, 0.0, 0.0, 20_000.0, 0.0, 0.0, 0.0, 20_000.0],
            [0.0, 10.0, 0.0],
        )
    }

    /// Spawn a placed clutter reference (optionally under a cell root) with one authored collider.
    fn spawn_clutter(
        app: &mut App,
        body: CollisionBody,
        collider: Collider,
        center: Vec3,
        position: Vec3,
        velocity: Vec3,
        parent: Option<Entity>,
    ) -> Entity {
        app.world_mut()
            .run_system_once(move |mut commands: Commands| {
                let mut entity = commands.spawn(Transform::from_translation(position));
                if let Some(parent) = parent {
                    entity.insert(ChildOf(parent));
                }
                let entity = entity.id();
                let mass = mass_properties_from_body(&body);
                spawn_dynamic_clutter(
                    &mut commands,
                    entity,
                    &body,
                    [(center, &collider)],
                    mass,
                    1.0,
                );
                commands.entity(entity).insert(Velocity {
                    linear: velocity,
                    angular: Vec3::ZERO,
                });
                entity
            })
            .unwrap()
    }

    /// Clutter starts asleep, so it stays where it was placed (its support may still be loading);
    /// once woken it falls and settles on the floor.
    #[test]
    fn dynamic_clutter_body_falls_and_settles_on_the_fixture_arena() {
        let mut app = headless::fixture_app();
        let entity = spawn_clutter(
            &mut app,
            box_body(),
            Collider::cuboid(10.0, 10.0, 10.0),
            Vec3::new(0.0, 10.0, 0.0),
            Vec3::new(-300.0, 400.0, 400.0),
            Vec3::ZERO,
            None,
        );
        for _ in 0..60 {
            app.update();
        }
        let y = app.world().get::<Transform>(entity).unwrap().translation.y;
        assert!((y - 400.0).abs() < 1.0e-3, "asleep clutter moved to {y}");
        app.world_mut()
            .get_mut::<Sleeping>(entity)
            .expect("clutter carries Sleeping")
            .sleeping = false;
        for _ in 0..600 {
            app.update();
        }
        let y = app.world().get::<Transform>(entity).unwrap().translation.y;
        // The collider's centre is 10 above the reference origin and the floor top is y = 0.
        assert!(y.abs() < 3.0, "clutter rests at {y}");
        let velocity = app.world().get::<Velocity>(entity).unwrap();
        assert!(
            velocity.linear.length() < 5.0,
            "clutter still moving: {velocity:?}"
        );
    }

    #[test]
    fn dynamic_clutter_body_takes_the_authored_mass_tensor_and_surface_values() {
        let mut app = headless::fixture_app();
        let entity = spawn_clutter(
            &mut app,
            box_body(),
            Collider::cuboid(10.0, 10.0, 10.0),
            Vec3::new(0.0, 10.0, 0.0),
            Vec3::new(-300.0, 500.0, 400.0),
            Vec3::ZERO,
            None,
        );
        for _ in 0..3 {
            app.update();
        }
        app.world_mut()
            .run_system_once(move |context: ReadRapierContext| {
                let context = context.single().expect("rapier context");
                let handle = context.entity2body()[&entity];
                let body = context.rigidbody_set.bodies.get(handle).expect("body");
                let props = body.mass_properties().local_mprops;
                assert!((props.mass() - 2.0).abs() < 1.0e-3, "mass {}", props.mass());
                let inertia = props.principal_inertia();
                let mut sorted = [inertia.x, inertia.y, inertia.z];
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                for (got, want) in sorted.iter().zip([30.0, 40.0, 50.0]) {
                    assert!((got - want).abs() < 0.01, "inertia {sorted:?}");
                }
                assert!((props.local_com.y - 10.0).abs() < 1.0e-3);
                assert!((body.linear_damping() - 0.1).abs() < 1.0e-6);
                assert!((body.angular_damping() - 0.05).abs() < 1.0e-6);
            })
            .unwrap();
        // The surface values sit on the colliders: the body entity itself for a single centred
        // shape, otherwise its collider children.
        let world = app.world_mut();
        let mut colliders = world.query::<(Entity, Option<&ChildOf>, &Friction, &Restitution)>();
        let surfaces: Vec<(f32, f32)> = colliders
            .iter(world)
            .filter(|(collider, parent, _, _)| {
                *collider == entity || parent.is_some_and(|parent| parent.parent() == entity)
            })
            .map(|(_, _, friction, restitution)| (friction.coefficient, restitution.coefficient))
            .collect();
        assert!(!surfaces.is_empty(), "the body has colliders");
        for (friction, restitution) in surfaces {
            assert_eq!(friction, 0.6);
            assert_eq!(restitution, 0.3);
        }
    }

    /// A reference placed at scale 2 keeps its authored mass; the centre of mass moves with the
    /// scaled shape and the inertia grows with the square of the scale.
    #[test]
    fn a_scaled_reference_scales_the_authored_centre_of_mass_and_inertia() {
        let mut app = headless::fixture_app();
        let entity = app
            .world_mut()
            .run_system_once(move |mut commands: Commands| {
                let entity = commands
                    .spawn(
                        Transform::from_translation(Vec3::new(-300.0, 500.0, 400.0))
                            .with_scale(Vec3::splat(2.0)),
                    )
                    .id();
                let body = box_body();
                let mass = mass_properties_from_body(&body);
                let collider = Collider::cuboid(10.0, 10.0, 10.0);
                spawn_dynamic_clutter(
                    &mut commands,
                    entity,
                    &body,
                    [(Vec3::new(0.0, 10.0, 0.0), &collider)],
                    mass,
                    2.0,
                );
                entity
            })
            .unwrap();
        for _ in 0..3 {
            app.update();
        }
        app.world_mut()
            .run_system_once(move |context: ReadRapierContext| {
                let context = context.single().expect("rapier context");
                let handle = context.entity2body()[&entity];
                let body = context.rigidbody_set.bodies.get(handle).expect("body");
                let props = body.mass_properties().local_mprops;
                assert!((props.mass() - 2.0).abs() < 1.0e-3, "mass {}", props.mass());
                let inertia = props.principal_inertia();
                let mut sorted = [inertia.x, inertia.y, inertia.z];
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                for (got, want) in sorted.iter().zip([120.0, 160.0, 200.0]) {
                    assert!((got - want).abs() < 0.05, "inertia {sorted:?}");
                }
                assert!((props.local_com.y - 20.0).abs() < 1.0e-3);
            })
            .unwrap();
    }

    #[test]
    fn dynamic_clutter_body_collides_with_a_tankard() {
        let mut app = headless::fixture_app();
        let base = Vec3::new(-300.0, 0.0, 400.0);
        let platform = spawn_clutter(
            &mut app,
            platform_body(),
            Collider::cuboid(50.0, 10.0, 50.0),
            Vec3::new(0.0, 10.0, 0.0),
            base,
            Vec3::ZERO,
            None,
        );
        let tankard = app
            .world_mut()
            .spawn((
                DebugTankard,
                RigidBody::Dynamic,
                debug_tankard_collider(),
                tankard_collision_groups(),
                ColliderMassProperties::Density(0.001),
                Velocity::zero(),
                Transform::from_translation(base + Vec3::Y * 300.0),
            ))
            .id();
        for _ in 0..400 {
            app.update();
        }
        let platform_top = app
            .world()
            .get::<Transform>(platform)
            .unwrap()
            .translation
            .y
            + 20.0;
        let tankard_y = app.world().get::<Transform>(tankard).unwrap().translation.y;
        // A tankard that fell through the clutter would rest on the floor, about 14-18 above it.
        assert!(
            tankard_y > platform_top + 10.0,
            "tankard {tankard_y} passed through the clutter top {platform_top}"
        );
    }

    #[test]
    fn a_falling_tankard_knocks_over_a_light_clutter_body() {
        let mut app = headless::fixture_app();
        let base = Vec3::new(-300.0, 0.0, 400.0);
        // A tall light body (1 kg) balanced on one end; the tankard drops onto its top corner.
        let mut light = authored_body(
            1.0,
            [400.0, 0.0, 0.0, 0.0, 400.0, 0.0, 0.0, 0.0, 400.0],
            [0.0, 40.0, 0.0],
        );
        light.restitution = 0.0;
        let clutter = spawn_clutter(
            &mut app,
            light,
            Collider::cuboid(5.0, 40.0, 5.0),
            Vec3::new(0.0, 40.0, 0.0),
            base,
            Vec3::ZERO,
            None,
        );
        for _ in 0..30 {
            app.update();
        }
        // Control: left alone for as long as the tankard gets below, the body stays standing, so
        // the rotation the assertion sees comes from the tankard, not from solver jitter.
        let settled = app.world().get::<Transform>(clutter).unwrap().rotation;
        for _ in 0..240 {
            app.update();
        }
        let before = app.world().get::<Transform>(clutter).unwrap().rotation;
        assert!(
            settled.angle_between(before) < 0.05,
            "the body tipped over on its own (rotation changed {})",
            settled.angle_between(before)
        );
        app.world_mut().spawn((
            DebugTankard,
            RigidBody::Dynamic,
            debug_tankard_collider(),
            tankard_collision_groups(),
            ColliderMassProperties::Density(0.01),
            Velocity {
                linear: Vec3::new(120.0, -50.0, 0.0),
                angular: Vec3::ZERO,
            },
            Transform::from_translation(base + Vec3::new(-40.0, 70.0, 0.0)),
        ));
        for _ in 0..240 {
            app.update();
        }
        let after = app.world().get::<Transform>(clutter).unwrap().rotation;
        assert!(
            before.angle_between(after) > 0.2,
            "the tankard did not move the clutter body (rotation changed {})",
            before.angle_between(after)
        );
    }

    #[test]
    fn dynamic_clutter_body_collides_with_the_player_capsule() {
        let mut app = headless::fixture_app();
        let base = Vec3::new(-300.0, 0.0, 400.0);
        let platform = spawn_clutter(
            &mut app,
            platform_body(),
            Collider::cuboid(50.0, 10.0, 50.0),
            Vec3::new(0.0, 10.0, 0.0),
            base,
            Vec3::ZERO,
            None,
        );
        for _ in 0..120 {
            app.update();
        }
        let top = app
            .world()
            .get::<Transform>(platform)
            .unwrap()
            .translation
            .y
            + 20.0;
        headless::place_player(&mut app, base + Vec3::Y * (top + 150.0));
        for _ in 0..240 {
            app.update();
        }
        let (pose, grounded) = headless::player_pose(&mut app);
        let tuning = MovementTuning::default();
        let rest = top + tuning.capsule_half_height() + tuning.capsule_radius;
        assert!(grounded, "player not grounded at {pose:?}");
        assert!(
            pose.y > rest - 4.0,
            "player {pose:?} sank through the clutter (rest {rest})"
        );
    }
}

// ---------------------------------------------------------------------------
// T2: WALK capsule, movement integration, collision-safe NOCLIP<->WALK toggle.
// ---------------------------------------------------------------------------

/// Player movement mode (V8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Resource)]
pub enum MoveMode {
    /// Flying camera, no collision.
    #[default]
    Noclip,
    /// Grounded kinematic capsule.
    Walk,
}

/// Reason the last NOCLIP->WALK entry failed (V8), surfaced in the overlay.
#[derive(Debug, Clone, Default, Resource)]
pub struct WalkEntryStatus {
    pub blocked_reason: Option<String>,
}

/// Bounded startup and destination protection while world collision reaches Rapier. A failed
/// load must leave debug flight available rather than freeze the player forever.
#[derive(Debug, Default, Resource)]
pub struct WalkSpawnHold {
    active: bool,
    elapsed: f32,
    sampled_ground_y: Option<f32>,
    destination: bool,
}

impl WalkSpawnHold {
    /// Whether safe placement still owns the walking body.
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }
}

/// Validated local terrain height for the initial interactive player pose.
#[derive(Debug, Default, Resource)]
pub struct WalkSpawnGroundHeight(pub Option<f32>);

const WALK_SPAWN_TIMEOUT_SECONDS: f32 = 30.0;
const WALK_SPAWN_MAX_PROBE_DISTANCE: f32 = 65_536.0;
const WALK_SPAWN_SURFACE_TOLERANCE: f32 = 64.0;

/// Read actual Rapier collision, not render/streaming completion counters. Run
/// before movement; colliders enabled by Update reach Rapier on a later tick.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn release_walk_spawn_hold(
    mut hold: ResMut<WalkSpawnHold>,
    mut mode: ResMut<MoveMode>,
    tuning: Res<MovementTuning>,
    context: ReadRapierContext,
    mut status: ResMut<WalkEntryStatus>,
    mut player: Query<(Entity, &mut Transform, &Collider), With<PlayerBody>>,
    mut reset: MotionReset,
    mut commands: Commands,
    crossing: Option<Res<crate::door_crossing::DoorCrossing>>,
) {
    if !hold.active || *mode != MoveMode::Walk {
        return;
    }
    hold.elapsed += PHYSICS_TIMESTEP;
    let Ok((entity, mut body, collider)) = player.single_mut() else {
        return;
    };
    let probe_distance =
        walk_spawn_probe_distance(body.translation.y, hold.sampled_ground_y, &tuning);
    let placement = context.single().ok().and_then(|context| {
        context
            .cast_ray(
                body.translation,
                Vec3::NEG_Y,
                probe_distance,
                true,
                QueryFilter::default().groups(CollisionGroups::new(GROUP_PLAYER, GROUP_WORLD)),
            )
            .filter(|hit| {
                hold.sampled_ground_y
                    .filter(|ground_y| ground_y.is_finite())
                    .is_none_or(|ground_y| {
                        // A loaded bridge or platform above the terrain is valid support too.
                        body.translation.y - hit.1 >= ground_y - WALK_SPAWN_SURFACE_TOLERANCE
                    })
            })
            .map(|_| {
                try_enter_walk(
                    &context,
                    &tuning,
                    body.translation + Vec3::Y * tuning.eye_height,
                    collider,
                )
            })
    });
    let result = match placement {
        Some(result) => result,
        None if hold.elapsed >= WALK_SPAWN_TIMEOUT_SECONDS => Err(if hold.destination {
            "WALK destination collision unavailable after 30 seconds; use V or tcl to retry WALK"
                .into()
        } else {
            "startup world collision unavailable after 30 seconds; use V or tcl to retry WALK"
                .into()
        }),
        None => return,
    };
    match result {
        Ok(position) => {
            body.translation = position;
            // Support from before placement cannot prove the new capsule is grounded.
            commands
                .entity(entity)
                .remove::<KinematicCharacterControllerOutput>();
            status.blocked_reason = None;
        }
        Err(reason) => {
            // A door owns its bounded rollback/recovery. Do not turn a failed WALK landing
            // into a successful NOCLIP arrival while the screen is still black.
            if crossing.is_some_and(|crossing| crossing.is_active()) {
                status.blocked_reason = Some(reason);
                return;
            }
            commands.entity(entity).insert(RigidBodyDisabled);
            *mode = MoveMode::Noclip;
            warn!("WALK collision fallback: {reason}");
            status.blocked_reason = Some(reason);
        }
    }
    *hold = WalkSpawnHold::default();
    reset.clear_input();
}

fn walk_spawn_probe_distance(
    body_y: f32,
    sampled_ground_y: Option<f32>,
    tuning: &MovementTuning,
) -> f32 {
    let fallback = tuning.capsule_standing_height * 2.0;
    let Some(ground_y) = sampled_ground_y.filter(|height| height.is_finite()) else {
        return fallback;
    };
    let distance_to_ground = body_y - ground_y;
    if !distance_to_ground.is_finite() {
        return 0.0;
    }
    (distance_to_ground + WALK_SPAWN_SURFACE_TOLERANCE).clamp(0.0, WALK_SPAWN_MAX_PROBE_DISTANCE)
}

/// Marker for the player body entity (V9).
#[derive(Component, Debug, Clone, Copy)]
pub struct PlayerBody;

/// Per-frame sampled movement intent; integrated once per physics tick (V18).
#[derive(Debug, Clone, Copy, Default, Resource)]
pub struct WalkIntent {
    /// Camera-yaw-relative wish direction, normalized, horizontal plane.
    pub wish_dir: Vec3,
    /// Desired top speed for current gait (walk/run/sprint).
    pub target_speed: f32,
    /// Fresh jump press this frame.
    pub jump_pressed: bool,
}

/// WALK sprint toggle; cleared whenever movement or controls stop.
#[derive(Debug, Clone, Copy, Default, Resource)]
pub(crate) struct SprintLatch(pub(crate) bool);

/// Horizontal motion and one owner of character flight history.
#[derive(Debug, Clone, Copy, Default, Component)]
pub struct WalkState {
    pub horizontal_velocity: Vec3,
    vertical: VerticalMotion,
    /// Retail's InAir state (RF-CC-013). False while walking, including while the unsupported cast
    /// keeps hitting (a step down), so no fall state and no landing begin on a stair tread.
    pub airborne: bool,
    /// The height where the air state began (RF-CC-013, retail stores it for fall damage).
    pub fall_start_height: f32,
    /// True for the one tick in which support returned after the air state (retail flag 0x80,
    /// RF-CC-015).
    pub landed: bool,
}

impl WalkState {
    pub fn vertical_velocity(&self) -> f32 {
        self.vertical.velocity_creation()
    }

    pub fn grounded(&self) -> bool {
        self.vertical.grounded()
    }

    pub fn jump_active(&self) -> bool {
        self.vertical.jump_active()
    }

    fn jump_launched(&self) -> bool {
        self.vertical.just_launched()
    }

    /// Apex from actual resolved positions, retained after landing.
    pub fn jump_apex(&self) -> f32 {
        self.vertical.jump_apex()
    }

    pub fn velocity(&self) -> Vec3 {
        self.horizontal_velocity + Vec3::Y * self.vertical_velocity()
    }
}

/// Upward search budget when entering WALK inside geometry (V8).
pub const WALK_ENTRY_SEARCH_STEPS: u32 = 12;
pub const WALK_ENTRY_STEP_HEIGHT: f32 = 28.0;

/// Build the upright player capsule + controller from tuning (V9, V12).
pub fn player_controller_bundle(tuning: &MovementTuning) -> impl Bundle {
    (
        PlayerBody,
        WalkState::default(),
        RigidBody::KinematicPositionBased,
        Collider::capsule_y(tuning.capsule_half_height(), tuning.capsule_radius),
        player_collision_groups(),
        KinematicCharacterController {
            up: Vect::Y,
            offset: CharacterLength::Absolute(PLAYER_CONTROLLER_OFFSET),
            slide: true,
            // WALK resolves steps with bounded capsule sweeps before SyncBackend.
            autostep: None,
            max_slope_climb_angle: tuning.slope_climb_degrees.to_radians(),
            min_slope_slide_angle: tuning.slope_slide_degrees.to_radians(),
            // NOTE: upstream Rapier 0.35 manifold-transfer panic (see T4 gate
            // hill test); re-enable with push validation once upgraded.
            apply_impulse_to_dynamic_bodies: false,
            // Retail has no ground snap. A step down is the unsupported cast of `walk_support`
            // (RF-CC-013, RF-CC-014), which keeps the player walking and pulls it onto the surface.
            snap_to_ground: None,
            ..default()
        },
    )
}

/// Pure movement integration: accelerate horizontal velocity toward wish dir,
/// apply gravity, handle jump from grounded + fresh press (V10, V11).
pub fn integrate_walk(
    state: &mut WalkState,
    intent: &WalkIntent,
    tuning: &MovementTuning,
    dt: f32,
    resolved: ResolvedWalk,
) {
    let wish = intent.wish_dir * intent.target_speed;
    let current = state.horizontal_velocity;
    let max_delta = tuning.horizontal_acceleration * dt;
    let delta = wish - current;
    state.horizontal_velocity = if delta.length() <= max_delta {
        wish
    } else {
        current + delta.normalize_or_zero() * max_delta
    };
    state
        .vertical
        .advance(&tuning.jump_height, resolved, intent.jump_pressed, dt);
}

type WalkBodyQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut Transform,
        &'static Collider,
        &'static mut KinematicCharacterController,
        &'static mut WalkState,
        Option<&'static KinematicCharacterControllerOutput>,
    ),
>;

/// Input ownership never gates gravity or safe placement. Clear stale wishes before fixed ticks,
/// which run before the Update input sampler, including on menu opening/closing frames.
fn neutralize_blocked_walk_input(
    console: Option<Res<ConsoleState>>,
    menu: Option<Res<crate::settings_ui::SettingsMenuState>>,
    mut reset: MotionReset,
) {
    if !console_closed(console, menu) {
        reset.clear_input();
    }
}

/// Fixed-step walk integration + controller feed (V10, V11, V18).
pub fn walk_movement_system(
    fixed_time: Res<Time<Fixed>>,
    mode: Res<MoveMode>,
    hold: Option<Res<WalkSpawnHold>>,
    mut intent: ResMut<WalkIntent>,
    tuning: Res<MovementTuning>,
    context: ReadRapierContext,
    mut player: WalkBodyQuery,
) {
    if *mode != MoveMode::Walk {
        return;
    }
    let Ok((entity, mut body, capsule, mut controller, mut state, output)) = player.single_mut()
    else {
        return;
    };
    controller.custom_shape = None;
    if hold.as_ref().is_some_and(|hold| hold.active) {
        *state = WalkState::default();
        intent.jump_pressed = false;
        // Even a zero move lets Rapier depenetrate an overlapping capsule. Only the bounded
        // placement search may move a held body; do not run the controller until it succeeds.
        controller.translation = None;
        return;
    }
    let dt = fixed_time.delta_secs();
    // Rapier's grounded flag stands in for retail's kSupport.
    let supported = output.map(|o| o.grounded).unwrap_or(false);
    let rapier = context.single().ok();
    // Walking state without support: a pending jump skips the unsupported cast, both the hit and
    // the miss (RF-CC-013), so the state stays walking and the jump launches from it. Without a
    // pending jump, the cast decides (RF-CC-013): a hit keeps walking and glides (RF-CC-014), a
    // miss starts the air state.
    let jump_pending = intent.jump_pressed;
    let walking_unsupported = !supported && !state.airborne && !state.jump_active();
    let glide = walking_unsupported
        && !jump_pending
        && rapier.as_ref().is_some_and(|context| {
            walk_support::unsupported_cast_hits(context, entity, &body, capsule, &controller)
        });
    let jump_from_walking = walking_unsupported && jump_pending;
    let previous_vertical = state.vertical_velocity();
    // RF-CC-015: landing is the first supported step after the air state. It is decided from the
    // state before this tick's integration, so a jump launched on the same tick keeps it: the
    // integration clears the old jump and starts the new one.
    let landing =
        supported && state.airborne && !(state.jump_active() && state.vertical_velocity() > 0.0);
    integrate_walk(
        &mut state,
        &intent,
        &tuning,
        dt,
        ResolvedWalk {
            y: body.translation.y,
            grounded: supported || glide || jump_from_walking,
        },
    );
    intent.jump_pressed = false;
    state.landed = false;
    // The tick that launches a jump starts the air state at the launch height (RF-CC-013), before
    // this tick's displacement. Rapier can keep reporting support for a few ticks after take-off,
    // so support does not count while the jump is rising (RF-CC-015: landing needs support).
    // VerticalMotion records accepted launches, including a relaunch on the landing tick.
    let launched = state.jump_launched();
    let rising = state.jump_active() && state.vertical_velocity() > 0.0;
    if launched {
        state.airborne = true;
        state.fall_start_height = body.translation.y;
        state.landed = landing;
    } else if supported && !rising {
        // RF-CC-015: the first supported step ends the air state and sets the landing flag.
        state.airborne = false;
        state.landed = landing;
    } else if supported {
        // Still rising right after take-off: the air state continues.
    } else if glide {
        // RF-CC-013, RF-CC-014: a hit keeps the walking state; 4 x gravity accelerates the body
        // onto the surface. No fall state, no fall start height, no landing.
        state
            .vertical
            .glide(body.translation.y, previous_vertical, dt);
    } else if !state.airborne {
        // RF-CC-013: a miss, or a jump launched from a cast hit, starts the air state.
        state.airborne = true;
        state.fall_start_height = body.translation.y;
    }
    if glide && let Some(context) = rapier.as_ref() {
        // RF-CC-014: the 4 x gravity pull of a cast hit moves the body straight down. Resolve
        // it from the glide velocity alone (separate from any downhill slope wish below), and
        // override Rapier's query pose because the character controller runs before transform
        // sync.
        let glide_drop = (-state.vertical_velocity() * dt).max(0.0);
        let drop = walk_support::drop_straight_down(
            context,
            entity,
            &body,
            capsule,
            &controller,
            glide_drop,
        );
        if drop.distance > 0.0 {
            body.translation.y -= drop.distance;
            controller.custom_shape = Some((
                capsule.clone(),
                body.rotation.inverse() * (Vec3::NEG_Y * drop.distance),
                Quat::IDENTITY,
            ));
        }
        if drop.landed {
            // STAND-IN(#1108): inferred, unverified in the binary (see `walk_support` module doc):
            // the downward sweep reached a surface, approximating Havok's proxy contact solve
            // arresting downward velocity. Leaving the old 4 x gravity speed in `Phase::Gliding`
            // would accumulate unbounded fall speed across stair treads or downhill ticks.
            state.vertical.set_supported(body.translation.y);
        }
    }
    // RF-CC-016, RF-CC-017: on a supported slope the wish lies in the support plane. A cast hit
    // runs the same walking step (RF-CC-014), so the surface below gives the normal there too
    // (sampled after any glide drop so the ray reaches the surface just landed on). While
    // gliding without a landing, projecting the wish also adds `wish.y * dt` to the free drop
    // (existing design choice, bounded by Rapier's collision step).
    let mut wish = state.horizontal_velocity;
    if (supported || glide)
        && !state.jump_active()
        && let Some(context) = rapier.as_ref()
        && let Some(normal) =
            walk_support::support_normal(context, entity, &body, &controller, &tuning)
    {
        wish = walk_support::slope_velocity(wish, normal, controller.max_slope_climb_angle.cos());
    }
    let mut displacement = if glide {
        // The vertical glide pull was applied straight down above; preserve `wish.y * dt`
        // (negative only when moving downhill on a walkable slope, zero on flat treads and when
        // standing still) so Rapier follows the slope instead of stepping horizontally off it and
        // looping back into `glide` every tick.
        wish * dt
    } else {
        (wish + Vec3::Y * state.vertical_velocity()) * dt
    };
    if supported
        && !state.jump_active()
        && let Some(context) = rapier.as_ref()
        && let Some(rise) = walk_step_rise(
            context,
            entity,
            &body,
            capsule,
            &controller,
            &tuning,
            displacement.reject_from_normalized(Vec3::Y),
        )
    {
        // The upward sweep validated this segment. Rapier runs the controller
        // before syncing transforms, so override this tick's query pose too.
        // Do not subtract the lift from the horizontal tick: built-in autostep
        // does so, undoing the step at narrow treads and coplanar landing joins.
        body.translation.y += rise;
        controller.custom_shape = Some((
            capsule.clone(),
            body.rotation.inverse() * (Vec3::Y * rise),
            Quat::IDENTITY,
        ));
        state.vertical.leave_support();
        displacement.y = 0.0;
    }
    controller.translation = Some(displacement);
}

/// Find a supported step without changing the requested horizontal distance.
/// All casts use the full capsule and exclude dynamic bodies only for stepping.
#[allow(clippy::too_many_arguments)]
fn walk_step_rise(
    context: &RapierContext,
    entity: Entity,
    body: &Transform,
    capsule: &Collider,
    controller: &KinematicCharacterController,
    tuning: &MovementTuning,
    horizontal: Vec3,
) -> Option<f32> {
    let distance = horizontal.length();
    if distance <= f32::EPSILON {
        return None;
    }
    let direction = horizontal / distance;
    let filter = QueryFilter {
        flags: controller.filter_flags,
        groups: controller.filter_groups,
        ..QueryFilter::default().exclude_rigid_body(entity)
    };
    let static_filter = QueryFilter {
        flags: filter.flags | QueryFilterFlags::EXCLUDE_DYNAMIC,
        ..filter
    };
    let cast = |position, direction, distance, filter| {
        context
            .cast_shape(
                position,
                body.rotation,
                direction,
                &*capsule.raw,
                ShapeCastOptions {
                    target_distance: PLAYER_CONTROLLER_OFFSET,
                    stop_at_penetration: false,
                    max_time_of_impact: distance,
                    compute_impact_geometry_on_penetration: true,
                },
                filter,
            )
            .map(|(_, hit)| hit)
    };
    let blocking_hit = cast(body.translation, direction, distance, static_filter)?;
    let obstacle = blocking_hit.details?;
    let walkable = controller.max_slope_climb_angle.cos();
    if obstacle.normal1.y > walkable {
        // A zero-distance floor hit can consume all 20 move-and-slide iterations
        // while the default 1e-4 normal nudge cannot recover the lost skin gap.
        // Repair only measured skin overlap, with an upward clearance sweep;
        // ordinary slope contacts continue through Rapier without a step.
        let capsule_point = body.translation + body.rotation * obstacle.witness2;
        let gap = (capsule_point - obstacle.witness1).dot(obstacle.normal1);
        if blocking_hit.time_of_impact > f32::EPSILON
            || !(-PLAYER_CONTROLLER_OFFSET..PLAYER_CONTROLLER_OFFSET).contains(&gap)
        {
            // Recover at most one skin width of overlap. Deeper penetration
            // needs placement recovery, not a full autostep-height relocation.
            return None;
        }
        let lift = (PLAYER_CONTROLLER_OFFSET + 0.05 - gap) / obstacle.normal1.y;
        return (lift > 0.0
            && lift <= tuning.autostep_height
            && cast(body.translation, Vec3::Y, lift, filter).is_none())
        .then_some(lift);
    }
    if obstacle.normal1.y < 0.0 {
        return None;
    }
    let approach = -direction.dot(obstacle.normal1.with_y(0.0).normalize_or_zero());
    if approach <= f32::EPSILON {
        return None;
    }
    // Advance far enough to probe a walkable capsule-edge contact, including an
    // oblique approach. A radius-wide probe instead hits the next 24-unit riser.
    let corner_advance = tuning.capsule_radius * (1.0 - controller.max_slope_climb_angle.sin());
    let probe = (corner_advance / approach + PLAYER_CONTROLLER_OFFSET).max(distance);
    if probe > tuning.capsule_radius + distance + PLAYER_CONTROLLER_OFFSET {
        return None; // A glancing contact cannot borrow support beyond the capsule's reach.
    }
    let budget = tuning.autostep_height;
    if cast(body.translation, Vec3::Y, budget, filter).is_some() {
        return None;
    }
    let raised = body.translation + Vec3::Y * budget;
    if cast(raised, direction, probe, filter).is_some() {
        return None;
    }
    let landing = cast(
        raised + direction * probe,
        Vec3::NEG_Y,
        budget,
        static_filter,
    )?
    .details?;
    if landing.normal1.y < walkable {
        return None;
    }
    // Use the obstacle surface height, not the rounded capsule's hypothetical
    // position at the lookahead point. Rapier 0.35 predicts ground at offset+0.05;
    // that same slop keeps a new landing outside numerical skin penetration.
    let rise = landing.witness1.y + tuning.capsule_standing_height * 0.5 + PLAYER_CONTROLLER_OFFSET
        - body.translation.y;
    if rise <= 0.05 || rise > budget {
        return None;
    }
    let lift = (rise + 0.05).min(budget);
    if cast(
        body.translation + Vec3::Y * lift,
        direction,
        distance,
        filter,
    )
    .is_some()
    {
        return None;
    }
    Some(lift)
}

/// Body position at the previous and the latest fixed tick, used to show the WALK view between ticks.
///
/// Physics runs at 60 Hz and a display can run faster. A camera that follows the body only
/// repeats its position on every frame that ran no tick: on 50% of frames at 120 Hz, 59% at 144 Hz
/// and 75% at 240 Hz in the controller-feel measurements (`physics/feel_measure.rs`). The view now
/// shows the point between the previous and the latest tick that matches how far the frame is into
/// the next tick (`Time<Fixed>::overstep_fraction`).
///
/// This is presentation only. The body, its collision and every physics query keep the latest tick
/// position (the tankard spawn ray, the carry hold point and the door return point read the body,
/// not this pose). The view trails it by 0 to one tick, at most 16.7 ms. Look rotation is not interpolated, so turning has no
/// added latency.
///
/// The blended position is the input of the first-person follow spring
/// ([`walk_camera_follow_system`]). Retail needs no blend: its player controller moves once per
/// rendered frame with that frame's time, and the camera follows that position (RF-PHYS-003,
/// RF-CAM-018). Mudcrab's WALK controller runs on a fixed 60 Hz tick, so the blend is a Mudcrab
/// stand-in that gives the spring a position that moves on every frame, as retail's does. Without
/// it the spring samples a staircase of tick positions and an uneven frame rate aliases into the
/// view. The long-term fix is a per-frame controller; this blend is not a retail mechanism.
///
/// Behaviour source: the Mudcrab controller-feel report, which takes the idea of showing a blend of
/// the last two tick poses from the spud re-audit (chasmlol/spud, MIT, used as a map). This code is
/// written from that written description, not from spud source.
#[derive(Debug, Clone, Copy, Default, Resource)]
pub struct WalkViewPose {
    previous: Vec3,
    latest: Vec3,
    valid: bool,
    /// The floating origin the two positions are stored in.
    origin: Option<IVec2>,
}

impl WalkViewPose {
    fn snap(&mut self, position: Vec3) {
        *self = Self {
            previous: position,
            latest: position,
            valid: true,
            origin: self.origin,
        };
    }

    /// Moves both stored positions into the space of `origin`. A floating-origin rebase moves the
    /// body by whole cells outside the tick. The streamer subtracts the same displacement from the
    /// body, so the blend continues across a cell border instead of snapping.
    fn rebase(&mut self, origin: Option<IVec2>) {
        if let (Some(old), Some(new)) = (self.origin, origin)
            && old != new
        {
            let displacement = render_origin_displacement(old, new);
            self.previous -= displacement;
            self.latest -= displacement;
        }
        self.origin = origin;
    }

    /// Whether `body` is where the last tick left it. Anything else moved the body outside the
    /// fixed tick (teleport, door arrival, origin rebase, entering WALK): show it where it is.
    fn follows(&self, body: Vec3) -> bool {
        self.valid && body == self.latest
    }

    fn shown(&self, fraction: f32) -> Vec3 {
        self.previous.lerp(self.latest, fraction.clamp(0.0, 1.0))
    }
}

/// Before a tick moves the body: drop a stale pose if something else moved the body since.
fn walk_view_begin_tick(
    mut pose: ResMut<WalkViewPose>,
    render_origin: Option<Res<crate::streaming::RenderOrigin>>,
    player: Query<&Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
) {
    let Ok(body) = player.single() else {
        return;
    };
    pose.rebase(render_origin.as_ref().map(|origin| origin.0));
    if !pose.follows(body.translation) {
        pose.snap(body.translation);
    }
}

/// After physics wrote the body back: the latest tick becomes the end of the blend.
fn walk_view_end_tick(
    mut pose: ResMut<WalkViewPose>,
    render_origin: Option<Res<crate::streaming::RenderOrigin>>,
    player: Query<&Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
) {
    let Ok(body) = player.single() else {
        return;
    };
    pose.rebase(render_origin.as_ref().map(|origin| origin.0));
    pose.previous = pose.latest;
    pose.latest = body.translation;
    pose.valid = true;
}

/// State of the first-person camera follow spring between rendered frames.
#[derive(Debug, Default, Resource)]
struct WalkCameraFollowState {
    /// `None` until the first WALK frame, and again after leaving WALK.
    spring: Option<camera_follow::FollowSpring>,
    initialized: bool,
    previous_origin: Option<IVec2>,
}

/// Camera follows the capsule at eye height; yaw on body, pitch on view (V9).
///
/// The view position is the eye node plus the retail follow spring's lag (RF-CAM-001 to
/// RF-CAM-007). It is evaluated once per rendered frame with the frame delta. This system runs in
/// `Update`, so the spring is not tied to the fixed physics step. The spring itself is
/// [`camera_follow::follow_spring_step`]. Its input is the body position blended between the last
/// two ticks ([`WalkViewPose`]), not the latest tick, so it moves on every rendered frame.
///
/// Pause also holds a door-staged camera pose until its queued body placement can commit. The
/// spring state is kept unchanged while it holds.
#[allow(clippy::too_many_arguments)]
fn walk_camera_follow_system(
    virtual_time: Res<Time<Virtual>>,
    mode: Res<MoveMode>,
    tuning: Res<MovementTuning>,
    look: Res<LookIntent>,
    time: Res<Time>,
    fixed: Res<Time<Fixed>>,
    mut pose: ResMut<WalkViewPose>,
    camera_space: Option<Res<crate::sky::CameraSpace>>,
    render_origin: Option<Res<crate::streaming::RenderOrigin>>,
    mut teleports: MessageReader<TeleportPlayer>,
    crossing: Option<Res<crate::door_crossing::DoorCrossing>>,
    hold: Option<Res<WalkSpawnHold>>,
    mut follow: ResMut<WalkCameraFollowState>,
    player: Query<&Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
    mut camera: Query<&mut Transform, With<StreamingCamera>>,
) {
    let mut teleported = false;
    for _ in teleports.read() {
        teleported = true;
    }
    let space_changed = camera_space
        .as_ref()
        .is_some_and(|space| space.is_changed());
    let current_origin = render_origin.as_ref().map(|origin| origin.0);
    // A floating-origin rebase shifts the body and the view by whole cells. It is not a retail
    // event. The spring state is shifted by the same displacement below, so the lag is unchanged.
    let rebase = follow
        .previous_origin
        .zip(current_origin)
        .filter(|(old, new)| old != new)
        .map(|(old, new)| render_origin_displacement(old, new));
    follow.previous_origin = current_origin;

    if virtual_time.is_paused()
        && crossing
            .as_ref()
            .is_some_and(|crossing| crossing.is_active())
    {
        return;
    }
    if *mode != MoveMode::Walk {
        follow.spring = None;
        return;
    }
    let (Ok(body), Ok(mut view)) = (player.single(), camera.single_mut()) else {
        return;
    };
    pose.rebase(current_origin);
    // A reset frame (below) shows the body itself, so the pose is snapped to it: a tick that moved
    // the body while it settles must not leave the view up to one tick behind. Ordinary frames keep
    // the blend.
    let placing = hold.as_ref().is_some_and(|hold| hold.is_active())
        || crossing
            .as_ref()
            .is_some_and(|crossing| crossing.is_active());
    let reset = teleported || space_changed || placing;
    if reset || !pose.follows(body.translation) {
        pose.snap(body.translation);
    }
    let feet = pose.shown(fixed.overstep_fraction());
    let target = feet + Vec3::Y * tuning.eye_height;
    if !follow.initialized {
        // The authored startup camera pose is authoritative for the first frame.
        follow.initialized = true;
        follow.spring = Some(camera_follow::FollowSpring::at_rest(feet, 0.0));
        set_walk_camera_rotation(&mut view, body, look.pitch);
        return;
    }

    // Engine plumbing, not retail behaviour. A teleport and a camera space change move the body
    // and the view into a new frame, so the old smoothed point is meaningless there. Retail
    // reaches the same result for a teleport over 300 units through the snap (RF-CAM-002). A door
    // crossing and the safe-placement hold place the body without player control. They are
    // plumbing for Mudcrab-only mechanisms that retail does not have, so the view follows the
    // body directly until they end. The spring weight carries over. Entering WALK needs no reset:
    // the spring is `None` outside WALK and starts at rest on the feet.
    let mut previous = follow
        .spring
        .unwrap_or(camera_follow::FollowSpring::at_rest(feet, 0.0));
    if let Some(displacement) = rebase {
        // The smoothed point lives in the same render space as the feet. Move it with them.
        previous.smoothed -= displacement;
    }
    let step = if reset {
        camera_follow::FollowStep {
            state: camera_follow::FollowSpring::at_rest(feet, previous.weight),
            lag: Vec3::ZERO,
        }
    } else if time.delta_secs() <= 0.0 {
        // Paused (focus pause, console, a held frame): no time passes, so the spring keeps its
        // state. A step of zero length would scale the lag by the weight again on every paused
        // frame, and clear the velocity (the momentum term would be lost on resume).
        camera_follow::FollowStep {
            state: previous,
            lag: previous.smoothed - feet,
        }
    } else {
        camera_follow::follow_spring_step(
            previous,
            camera_follow::FollowInput {
                feet,
                delta_secs: time.delta_secs(),
                // RF-CAM-004: the player is controls-driven in normal play. Mudcrab has no
                // AI-driven player movement.
                controls_driven: true,
                // RF-CAMW-001: Mudcrab has no mount state yet, so this stays false.
                is_on_mount: false,
                // The engine has no third-person camera. RF-CAM-006: it would get no spring.
                view: camera_follow::FollowView::FirstPerson,
            },
        )
    };
    follow.spring = Some(step.state);
    view.translation = target + step.lag;
    set_walk_camera_rotation(&mut view, body, look.pitch);
}

/// Shift Rapier rigid bodies by `-displacement` during a floating-origin rebase, propagate their
/// new poses to attached colliders, and immediately refresh the broad-phase BVH leaves.
///
/// `propagate_modified_body_positions_to_colliders` only updates `ColliderSet` poses; Rapier's
/// `broad_phase` is otherwise not refreshed until `PhysicsSet::StepSimulation`. Without an
/// immediate BVH update, the first `FixedUpdate` tick after a rebase runs `walk_movement_system`
/// spatial queries against stale AABBs 4096 units away.
/// external-name: bevy_rapier3d `update_character_controls` system
/// Likewise, `update_character_controls` (`SyncBackend`) runs before `PhysicsSet::StepSimulation`,
/// causing a one-tick collision dropout that can drop the player through thin terrain.
pub(crate) fn rebase_rapier_bodies(
    context: &mut bevy_rapier3d::prelude::RapierContextMut<'_>,
    entities: impl IntoIterator<Item = Entity>,
    displacement: Vec3,
) {
    let mut moved_colliders = Vec::new();
    for entity in entities {
        if let Some(handle) = context.entity2body().get(&entity).copied()
            && let Some(body) = context.rigidbody_set.bodies.get_mut(handle)
        {
            body.set_translation(body.translation() - displacement, false);
            moved_colliders.extend_from_slice(body.colliders());
        }
    }
    context.propagate_modified_body_positions_to_colliders();
    let params = context.simulation.integration_parameters;
    for handle in moved_colliders {
        if let Some(collider) = context.colliders.colliders.get(handle)
            && collider.is_enabled()
        {
            let aabb = collider.compute_broad_phase_aabb(&params, &context.rigidbody_set.bodies);
            if aabb.mins.is_finite() && aabb.maxs.is_finite() {
                context
                    .simulation
                    .broad_phase
                    .set_aabb(&params, handle, aabb);
            }
        }
    }
}

/// The displacement of the floating origin from `old` to `new`, in render-space units. The
/// streamer subtracts it from the body and the view when it rebases.
fn render_origin_displacement(old: IVec2, new: IVec2) -> Vec3 {
    let shift = new - old;
    Vec3::new(
        shift.x as f32 * crate::world::components::CELL_SIZE,
        0.0,
        -(shift.y as f32) * crate::world::components::CELL_SIZE,
    )
}

fn set_walk_camera_rotation(view: &mut Transform, body: &Transform, pitch: f32) {
    let (yaw, _, _) = body.rotation.to_euler(EulerRot::YXZ);
    view.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
}

#[cfg(test)]
mod walk_camera_presentation_tests {
    use super::*;

    #[test]
    fn camera_placement_resets_drop_the_smoothed_point_and_follow_full_position() {
        let mut app = headless::fixture_app();
        let (body, camera) = {
            let mut bodies = app.world_mut().query_filtered::<Entity, With<PlayerBody>>();
            let body = bodies.single(app.world()).unwrap();
            let mut cameras = app
                .world_mut()
                .query_filtered::<Entity, With<StreamingCamera>>();
            (body, cameras.single(app.world()).unwrap())
        };
        app.insert_resource(MoveMode::Walk);
        app.world_mut().run_schedule(Update);
        for case in 0..4 {
            {
                let mut follow = app.world_mut().resource_mut::<WalkCameraFollowState>();
                follow.initialized = true;
                follow.spring = Some(camera_follow::FollowSpring::at_rest(Vec3::ZERO, 1.0));
            }
            let destination = Vec3::new(500.0 + case as f32 * 100.0, 800.0, -900.0);
            app.world_mut()
                .get_mut::<Transform>(body)
                .unwrap()
                .translation = destination;
            match case {
                0 => {
                    app.world_mut().write_message(TeleportPlayer {
                        position: destination,
                        sampled_ground_y: Some(destination.y),
                        yaw: 0.4,
                    });
                }
                1 => {
                    app.insert_resource(crate::sky::CameraSpace::Interior);
                }
                2 => {
                    app.insert_resource(MoveMode::Noclip);
                    app.world_mut().run_schedule(Update);
                    app.insert_resource(MoveMode::Walk);
                }
                _ => {
                    // Engine plumbing: the safe-placement hold places the body without control.
                    app.world_mut().resource_mut::<WalkSpawnHold>().active = true;
                }
            }
            app.world_mut().run_schedule(Update);
            let position = app.world().get::<Transform>(body).unwrap().translation;
            let view = app.world().get::<Transform>(camera).unwrap().translation;
            assert!(
                view.distance(position + Vec3::Y * MovementTuning::default().eye_height) < 1.0e-3,
                "reset case {case} retained an old camera coordinate: {view:?} vs {position:?}"
            );
            let spring = app
                .world()
                .resource::<WalkCameraFollowState>()
                .spring
                .expect("the spring is seeded after a reset");
            assert_eq!(spring.smoothed, position);
            assert_eq!(spring.velocity, Vec3::ZERO);
            app.world_mut().resource_mut::<WalkSpawnHold>().active = false;
        }
    }

    /// While no time passes (focus pause, console), the follow spring keeps its lag, velocity and
    /// weight. A zero-length step would scale the lag by the weight on every paused frame.
    #[test]
    fn a_paused_frame_keeps_the_follow_spring() {
        let mut app = headless::fixture_app();
        let (body, camera) = {
            let mut bodies = app.world_mut().query_filtered::<Entity, With<PlayerBody>>();
            let body = bodies.single(app.world()).unwrap();
            let mut cameras = app
                .world_mut()
                .query_filtered::<Entity, With<StreamingCamera>>();
            (body, cameras.single(app.world()).unwrap())
        };
        let eye = Vec3::Y * MovementTuning::default().eye_height;
        app.insert_resource(MoveMode::Walk);
        app.insert_resource(crate::streaming::RenderOrigin(IVec2::ZERO));
        app.world_mut().run_schedule(Update);
        let feet = Vec3::new(1_000.0, 300.0, -500.0);
        app.world_mut()
            .get_mut::<Transform>(body)
            .unwrap()
            .translation = feet;
        // A spring that has not reached full weight, with a lag and a velocity.
        let running = camera_follow::FollowSpring {
            smoothed: feet - Vec3::new(20.0, -6.0, 0.0),
            velocity: Vec3::new(370.0, -100.0, 0.0),
            weight: 0.5,
        };
        {
            let mut follow = app.world_mut().resource_mut::<WalkCameraFollowState>();
            follow.initialized = true;
            follow.spring = Some(running);
        }
        for _ in 0..5 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::ZERO);
            app.world_mut().run_schedule(Update);
        }
        let follow = app.world().resource::<WalkCameraFollowState>();
        assert_eq!(
            follow.spring,
            Some(running),
            "a paused frame changed the spring"
        );
        let view = app.world().get::<Transform>(camera).unwrap().translation;
        let lag = running.smoothed - feet;
        assert!(
            (view - (feet + eye + lag)).length() < 1.0e-3,
            "the paused view moved: view={view:?} expected lag {lag:?}"
        );
    }

    /// A floating-origin rebase moves the body, the view and the smoothed point by the same
    /// displacement. The lag, which is about 20 units while running, must not change in the world
    /// frame, so the view does not pop forward at a cell border.
    #[test]
    fn a_render_origin_rebase_while_running_keeps_the_camera_offset() {
        let mut app = headless::fixture_app();
        let (body, camera) = {
            let mut bodies = app.world_mut().query_filtered::<Entity, With<PlayerBody>>();
            let body = bodies.single(app.world()).unwrap();
            let mut cameras = app
                .world_mut()
                .query_filtered::<Entity, With<StreamingCamera>>();
            (body, cameras.single(app.world()).unwrap())
        };
        let eye = Vec3::Y * MovementTuning::default().eye_height;
        app.insert_resource(MoveMode::Walk);
        app.insert_resource(crate::streaming::RenderOrigin(IVec2::ZERO));
        app.world_mut().run_schedule(Update);
        let feet = Vec3::new(1_000.0, 300.0, -500.0);
        app.world_mut()
            .get_mut::<Transform>(body)
            .unwrap()
            .translation = feet;
        // A running spring: the smoothed point trails the feet and moves with them.
        let running = camera_follow::FollowSpring {
            smoothed: feet - Vec3::new(20.0, 0.0, 0.0),
            velocity: Vec3::new(370.0, 0.0, 0.0),
            weight: 1.0,
        };
        {
            let mut follow = app.world_mut().resource_mut::<WalkCameraFollowState>();
            follow.initialized = true;
            follow.spring = Some(running);
        }
        // A running frame: the fixture's first frame has no elapsed time, and a frame with none is
        // a paused frame that keeps the spring (see `a_paused_frame_keeps_the_follow_spring`).
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(PHYSICS_TIMESTEP));
        // The same frame without a rebase is the control.
        let delta_secs = app.world().resource::<Time>().delta_secs();
        let expected = camera_follow::follow_spring_step(
            running,
            camera_follow::FollowInput {
                feet,
                delta_secs,
                controls_driven: true,
                is_on_mount: false,
                view: camera_follow::FollowView::FirstPerson,
            },
        );
        assert!(
            expected.lag.x.abs() > 1.0,
            "the fixture needs a visible lag: {:?}",
            expected.lag
        );
        // The streamer rebases: the origin moves one cell in X and in Y, and the body and the view
        // are shifted by the displacement.
        let new_origin = IVec2::new(1, 1);
        let displacement = render_origin_displacement(IVec2::ZERO, new_origin);
        assert_ne!(displacement, Vec3::ZERO);
        app.world_mut()
            .resource_mut::<crate::streaming::RenderOrigin>()
            .0 = new_origin;
        for entity in [body, camera] {
            app.world_mut()
                .get_mut::<Transform>(entity)
                .unwrap()
                .translation -= displacement;
        }
        app.world_mut().run_schedule(Update);
        let view = app.world().get::<Transform>(camera).unwrap().translation;
        let shifted_feet = feet - displacement;
        assert!(
            (view - (shifted_feet + eye) - expected.lag).length() < 1.0e-3,
            "the rebase changed the camera offset: view={view:?} lag={:?}",
            expected.lag
        );
        let spring = app
            .world()
            .resource::<WalkCameraFollowState>()
            .spring
            .unwrap();
        assert!((spring.smoothed - (shifted_feet + expected.lag)).length() < 1.0e-3);
        assert_eq!(spring.velocity, expected.state.velocity);
        assert_eq!(spring.weight, expected.state.weight);
    }
}

/// Attempt NOCLIP->WALK at the camera pose; overlap pushes the search upward
/// until the capsule fits or leaves NOCLIP active with a visible reason (V8).
pub fn try_enter_walk(
    context: &RapierContext,
    tuning: &MovementTuning,
    camera_position: Vec3,
    capsule: &Collider,
) -> Result<Vec3, String> {
    let shape = &*capsule.raw;
    let mut candidate = camera_position - Vec3::Y * tuning.eye_height;
    for _ in 0..=WALK_ENTRY_SEARCH_STEPS {
        let mut overlapping = false;
        context.intersect_shape(
            candidate,
            Quat::IDENTITY,
            shape,
            QueryFilter::default().groups(CollisionGroups::new(
                GROUP_PLAYER,
                GROUP_WORLD | GROUP_CLUTTER,
            )),
            |_| {
                overlapping = true;
                false
            },
        );
        if !overlapping {
            return Ok(candidate);
        }
        candidate.y += WALK_ENTRY_STEP_HEIGHT;
    }
    Err("no free capsule placement nearby".to_owned())
}

/// The pending-input state a pause drops, optional so the console works in partial apps.
#[derive(SystemParam)]
pub(crate) struct MotionReset<'w, 's> {
    intent: Option<ResMut<'w, WalkIntent>>,
    sprint: Option<ResMut<'w, SprintLatch>>,
    controllers: Query<'w, 's, &'static mut KinematicCharacterController>,
}

impl MotionReset<'_, '_> {
    /// Drops pending input only: walk intent, the sprint latch and the queued controller
    /// translation. Velocity and jump state stay, so a pause resumes the motion where it stopped.
    pub(crate) fn clear_input(&mut self) {
        if let Some(intent) = self.intent.as_deref_mut() {
            *intent = WalkIntent::default();
        }
        if let Some(sprint) = self.sprint.as_deref_mut() {
            sprint.0 = false;
        }
        for mut controller in &mut self.controllers {
            controller.translation = None;
        }
    }
}

/// Clear stale velocities + jump state on every toggle (V14).
fn clear_motion_state(
    intent: &mut WalkIntent,
    sprint: &mut SprintLatch,
    states: &mut Query<&mut WalkState>,
    controllers: &mut Query<&mut KinematicCharacterController>,
) {
    *intent = WalkIntent::default();
    sprint.0 = false;
    for mut state in states {
        *state = WalkState::default();
    }
    for mut controller in controllers {
        controller.translation = None;
    }
}

// ---------------------------------------------------------------------------
// Teleport: a one-shot placement of the player, used by load-door crossings.
// ---------------------------------------------------------------------------

/// Moves the player to `position` facing `yaw`.
///
/// `position` is where the player's FEET end up, in RENDER space (the same space as the player's
/// `Transform`, relative to the current render origin, not Creation or world coordinates). The
/// body's `Transform` goes one capsule half-extent above it and the camera one eye height above
/// the body, as in [`walk_camera_follow_system`]. The sender converts and rebases first.
#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub struct TeleportPlayer {
    pub position: Vec3,
    /// Known destination terrain height in render space, when the sender sampled it.
    pub sampled_ground_y: Option<f32>,
    /// Heading in radians about +Y, as `Quat::from_euler(EulerRot::YXZ, yaw, ..)`.
    pub yaw: f32,
}

/// How far the body's origin (the capsule centre) sits above the feet.
pub fn body_height_above_feet(tuning: &MovementTuning) -> f32 {
    tuning.capsule_half_height() + tuning.capsule_radius
}

/// Where the body goes and where the camera goes for feet at `feet`, in the feet's space.
pub fn body_and_camera_for_feet(tuning: &MovementTuning, feet: Vec3) -> (Vec3, Vec3) {
    let body = feet + Vec3::Y * body_height_above_feet(tuning);
    (body, body + Vec3::Y * tuning.eye_height)
}

type TeleportBodyQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut Transform,
        Option<&'static mut WalkState>,
        Option<&'static mut KinematicCharacterController>,
        Option<&'static mut Velocity>,
    ),
    (With<PlayerBody>, Without<StreamingCamera>),
>;

/// Applies [`TeleportPlayer`]: places the body, zeroes its velocity and walk state, and holds WALK
/// until bounded destination collision is available. In NOCLIP the body stays disabled and only
/// the camera moves. Door placements queued before a pause are retained until world resume;
/// explicit console teleports outside a crossing remain intentional paused-world edits.
#[allow(clippy::too_many_arguments)]
fn apply_teleport_player(
    mut teleports: MessageReader<TeleportPlayer>,
    mut pending: Local<Option<TeleportPlayer>>,
    time: Res<Time<Virtual>>,
    crossing: Option<Res<crate::door_crossing::DoorCrossing>>,
    mut hold: ResMut<WalkSpawnHold>,
    mode: Res<MoveMode>,
    tuning: Res<MovementTuning>,
    mut look: ResMut<LookIntent>,
    mut intent: ResMut<WalkIntent>,
    mut body: TeleportBodyQuery,
    mut camera: Query<&mut Transform, (With<StreamingCamera>, Without<PlayerBody>)>,
    mut commands: Commands,
) {
    let deferred = pending.is_some();
    if let Some(teleport) = teleports.read().last().copied() {
        *pending = Some(teleport);
    }
    if time.is_paused() && (deferred || crossing.is_some_and(|crossing| crossing.is_active())) {
        return;
    }
    let Some(teleport) = pending.take() else {
        return;
    };
    *hold = WalkSpawnHold::default();
    if *mode == MoveMode::Walk {
        hold.active = true;
        hold.sampled_ground_y = teleport
            .sampled_ground_y
            .filter(|height| height.is_finite());
        hold.destination = true;
    }
    look.yaw = teleport.yaw;
    *intent = WalkIntent::default();
    let rotation = Quat::from_euler(EulerRot::YXZ, teleport.yaw, 0.0, 0.0);
    let (body_position, camera_position) = body_and_camera_for_feet(&tuning, teleport.position);
    if *mode == MoveMode::Walk
        && let Ok((entity, mut transform, state, controller, velocity)) = body.single_mut()
    {
        transform.translation = body_position;
        transform.rotation = rotation;
        commands
            .entity(entity)
            .remove::<KinematicCharacterControllerOutput>();
        if let Some(mut state) = state {
            *state = WalkState::default();
        }
        if let Some(mut controller) = controller {
            controller.translation = None;
        }
        if let Some(mut velocity) = velocity {
            *velocity = Velocity::zero();
        }
    }
    if let Ok(mut view) = camera.single_mut() {
        view.translation = camera_position;
        view.rotation = Quat::from_euler(EulerRot::YXZ, teleport.yaw, look.pitch, 0.0);
    }
}

#[cfg(test)]
mod walk_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn alt_sprint_latches_until_movement_stops_or_controls_leave_walk() {
        let mut app = App::new();
        app.insert_resource(MoveMode::Walk)
            .insert_resource(CursorCapture::Captured)
            .insert_resource(ButtonInput::<KeyCode>::default())
            .insert_resource(MovementTuning::default())
            .insert_resource(LookIntent::default())
            .insert_resource(WalkIntent::default())
            .insert_resource(SprintLatch::default());
        app.world_mut().spawn((PlayerBody, Transform::default()));
        let sample = |app: &mut App| {
            app.world_mut()
                .run_system_once(sprint_latch_system)
                .unwrap();
            app.world_mut().run_system_once(walk_intent_system).unwrap();
            app.world().resource::<WalkIntent>().target_speed
        };
        let tuning = app.world().resource::<MovementTuning>().clone();
        let sprint_speed = tuning.target_speed(Vec2::new(0.0, -1.0), false, true);

        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.press(KeyCode::KeyW);
            keys.press(KeyCode::AltLeft);
        }
        assert_eq!(sample(&mut app), sprint_speed);
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.clear();
            keys.release(KeyCode::AltLeft);
        }
        assert_eq!(sample(&mut app), sprint_speed);
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.clear();
            keys.press(KeyCode::AltLeft);
        }
        assert_eq!(sample(&mut app), tuning.run_speed);
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.clear();
            keys.release(KeyCode::AltLeft);
            keys.press(KeyCode::AltRight);
        }
        assert_eq!(sample(&mut app), sprint_speed);
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.clear();
            keys.release(KeyCode::AltRight);
            keys.release(KeyCode::KeyW);
        }
        assert_eq!(sample(&mut app), 0.0);
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.clear();
            keys.press(KeyCode::KeyW);
        }
        assert_eq!(sample(&mut app), tuning.run_speed);

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::AltLeft);
        assert_eq!(sample(&mut app), sprint_speed);
        app.insert_resource(MoveMode::Noclip);
        sample(&mut app);
        app.insert_resource(MoveMode::Walk);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();
        assert_eq!(sample(&mut app), tuning.run_speed);

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(KeyCode::AltLeft);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::AltRight);
        assert_eq!(sample(&mut app), sprint_speed);
        app.insert_resource(CursorCapture::Released);
        sample(&mut app);
        app.insert_resource(CursorCapture::Captured);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();
        assert_eq!(sample(&mut app), tuning.run_speed);
    }

    #[test]
    fn walk_integrates_toward_wish_speed_with_bounded_acceleration() {
        let tuning = MovementTuning::default();
        let mut state = WalkState::default();
        let intent = WalkIntent {
            wish_dir: Vec3::X,
            target_speed: tuning.run_speed,
            jump_pressed: false,
        };
        integrate_walk(
            &mut state,
            &intent,
            &tuning,
            PHYSICS_TIMESTEP,
            ResolvedWalk {
                y: 0.0,
                grounded: true,
            },
        );
        let expected = tuning.horizontal_acceleration * PHYSICS_TIMESTEP;
        assert!((state.horizontal_velocity.x - expected).abs() < 0.01);
        for _ in 0..600 {
            integrate_walk(
                &mut state,
                &intent,
                &tuning,
                PHYSICS_TIMESTEP,
                ResolvedWalk {
                    y: 0.0,
                    grounded: true,
                },
            );
        }
        assert!((state.horizontal_velocity.length() - tuning.run_speed).abs() < 0.5);
    }

    #[test]
    fn jump_requires_grounded_fresh_press_and_applies_gravity() {
        let tuning = MovementTuning::default();
        let mut state = WalkState::default();
        let air_jump = WalkIntent {
            jump_pressed: true,
            ..default()
        };
        integrate_walk(
            &mut state,
            &air_jump,
            &tuning,
            PHYSICS_TIMESTEP,
            ResolvedWalk {
                y: 0.0,
                grounded: false,
            },
        );
        assert!(state.vertical_velocity() < 0.0);
        let grounded_jump = WalkIntent {
            jump_pressed: true,
            ..default()
        };
        integrate_walk(
            &mut state,
            &grounded_jump,
            &tuning,
            PHYSICS_TIMESTEP,
            ResolvedWalk {
                y: 0.0,
                grounded: true,
            },
        );
        let expected = ((2.0_f32 * 9.81 * (76.0 * f32::from_bits(0x3c6a_161e))).sqrt()
            - 9.81 * 1.35 * PHYSICS_TIMESTEP)
            * f32::from_bits(0x428b_fb85);
        assert!((state.vertical_velocity() - expected).abs() < 0.0001);
    }

    #[test]
    fn capsule_bundle_matches_provisional_spec_dimensions() {
        let tuning = MovementTuning::default();
        let app_bundle = player_controller_bundle(&tuning);
        let mut world = World::new();
        let entity = world.spawn(app_bundle).id();
        let collider = world.get::<Collider>(entity).expect("capsule collider");
        let capsule = collider.as_capsule().expect("capsule shape");
        assert!((capsule.radius() - tuning.capsule_radius).abs() < f32::EPSILON);
        let controller = world
            .get::<KinematicCharacterController>(entity)
            .expect("controller");
        assert!(
            (controller.max_slope_climb_angle - tuning.slope_climb_degrees.to_radians()).abs()
                < 1.0e-6
        );
        assert_eq!(controller.snap_to_ground, None, "retail has no ground snap");
    }
}

#[cfg(test)]
pub(crate) mod headless {
    use super::*;
    use bevy::mesh::MeshPlugin;
    use bevy::time::TimeUpdateStrategy;

    pub(crate) fn fixture_app() -> App {
        fixture_app_with(|_| {})
    }

    /// The fixture app, with `extra` run on it before plugins finish (to add more plugins).
    pub(crate) fn fixture_app_with(extra: impl FnOnce(&mut App)) -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            TransformPlugin,
            bevy::input::InputPlugin,
            AssetPlugin::default(),
            MeshPlugin,
            MaterialPlugin::<StandardMaterial>::default(),
        ));
        app.init_resource::<ProfilingState>()
            .init_resource::<StreamingMetrics>();
        app.add_plugins(PhysicsFixturePlugin);
        extra(&mut app);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs_f32(PHYSICS_TIMESTEP),
        ));
        app.finish();
        app.update();
        app
    }

    /// Place the fixture player body, enable its rigid body, enter WALK.
    pub(crate) fn place_player(app: &mut App, position: Vec3) {
        let mut query = app.world_mut().query_filtered::<(
            Entity,
            &mut Transform,
            Option<&RigidBodyDisabled>,
        ), With<PlayerBody>>();
        let (entity, mut transform, disabled) = query
            .single_mut(app.world_mut())
            .expect("fixture player body");
        transform.translation = position;
        if disabled.is_some() {
            app.world_mut()
                .entity_mut(entity)
                .remove::<RigidBodyDisabled>();
        }
        app.insert_resource(MoveMode::Walk);
        app.insert_resource(WalkIntent::default());
    }

    pub(crate) fn player_pose(app: &mut App) -> (Vec3, bool) {
        let mut query = app.world_mut().query_filtered::<(
            &Transform,
            &WalkState,
            Option<&KinematicCharacterControllerOutput>,
        ), With<PlayerBody>>();
        let (transform, state, output) = query.single(app.world()).expect("player body");
        (
            transform.translation,
            state.grounded() || output.map(|o| o.grounded).unwrap_or(false),
        )
    }
}

#[cfg(test)]
mod world_startup_tests {
    use super::*;
    use bevy::{ecs::system::RunSystemOnce, mesh::MeshPlugin, time::TimeUpdateStrategy};

    fn world_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            TransformPlugin,
            bevy::input::InputPlugin,
            AssetPlugin::default(),
            MeshPlugin,
            MaterialPlugin::<StandardMaterial>::default(),
        ))
        .init_resource::<ProfilingState>()
        .init_resource::<StreamingMetrics>()
        // Rapier's debug-render system validates Gizmos even when disabled.
        .add_plugins(bevy::gizmos::GizmoPlugin)
        .add_plugins((WorldPlayerPlugin, crate::console::ConsolePlugin))
        .add_systems(Startup, |mut commands: Commands| {
            commands.spawn((
                StreamingCamera,
                Transform::from_translation(Vec3::new(0.0, 200.0, 0.0))
                    .with_rotation(Quat::from_euler(EulerRot::YXZ, 0.7, -0.2, 0.0)),
            ));
        })
        .insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs_f32(PHYSICS_TIMESTEP),
        ));
        app.finish();
        app.update();
        app
    }

    fn body(app: &mut App) -> Entity {
        app.world_mut()
            .query_filtered::<Entity, With<PlayerBody>>()
            .single(app.world())
            .unwrap()
    }

    fn tick(app: &mut App, count: usize) {
        for _ in 0..count {
            app.update();
        }
    }

    fn floor(app: &mut App, disabled: bool) -> Entity {
        let mut entity = app.world_mut().spawn((
            RigidBody::Fixed,
            Collider::cuboid(1000.0, 10.0, 1000.0),
            Transform::from_xyz(0.0, -10.0, 0.0),
            world_collision_groups(),
        ));
        if disabled {
            entity.insert(ColliderDisabled);
        }
        entity.id()
    }

    fn assert_mode(app: &mut App, expected: MoveMode) {
        assert_eq!(*app.world().resource::<MoveMode>(), expected);
        let entity = body(app);
        assert_eq!(
            app.world().get::<RigidBodyDisabled>(entity).is_some(),
            expected == MoveMode::Noclip
        );
    }

    #[test]
    fn world_starts_in_walk_with_active_body_and_copied_view() {
        let mut app = world_app();
        assert_mode(&mut app, MoveMode::Walk);
        let entity = body(&mut app);
        let transform = app.world().get::<Transform>(entity).unwrap();
        let tuning = app.world().resource::<MovementTuning>();
        assert!((transform.translation.y - (200.0 - tuning.eye_height)).abs() < 0.001);
        let (yaw, pitch, _) = transform.rotation.to_euler(EulerRot::YXZ);
        assert!((yaw - 0.7).abs() < 0.001);
        assert!(pitch.abs() < 0.001);
        let look = app.world().resource::<LookIntent>();
        assert!((look.yaw - 0.7).abs() < 0.001 && (look.pitch + 0.2).abs() < 0.001);
        assert_eq!(
            app.world_mut()
                .query_filtered::<Entity, (With<StreamingCamera>, With<ControlledCamera>)>()
                .iter(app.world())
                .count(),
            1
        );
        let text = app
            .world_mut()
            .query_filtered::<&Text, With<NoclipOverlay>>()
            .single(app.world())
            .unwrap();
        assert!(text.0.starts_with("NOCLIP: OFF"));
        assert!(text.0.contains("waiting for WALK collision"));
    }

    #[test]
    fn hold_waits_for_enabled_rapier_collision_then_grounds() {
        let mut app = world_app();
        let entity = body(&mut app);
        let start = app.world().get::<Transform>(entity).unwrap().translation;
        let ground = floor(&mut app, true);
        {
            let mut state = app.world_mut().get_mut::<WalkState>(entity).unwrap();
            integrate_walk(
                &mut state,
                &WalkIntent::default(),
                &MovementTuning::default(),
                1.0,
                ResolvedWalk {
                    y: 500.0,
                    grounded: false,
                },
            );
            state.horizontal_velocity = Vec3::X * 300.0;
        }
        tick(&mut app, 20);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        assert_eq!(
            app.world().get::<Transform>(entity).unwrap().translation,
            start
        );
        app.world_mut()
            .entity_mut(ground)
            .remove::<ColliderDisabled>();
        tick(&mut app, 120);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        assert_mode(&mut app, MoveMode::Walk);
        let (pose, grounded) = headless::player_pose(&mut app);
        assert!(grounded, "{pose:?}");
        let view = app
            .world_mut()
            .query_filtered::<&Transform, With<StreamingCamera>>()
            .single(app.world())
            .unwrap();
        assert!((view.translation.y - pose.y - MovementTuning::default().eye_height).abs() < 0.01);
        let (yaw, pitch, _) = view.rotation.to_euler(EulerRot::YXZ);
        assert!((yaw - 0.7).abs() < 0.001 && (pitch + 0.2).abs() < 0.001);
    }

    #[test]
    fn distant_world_floor_does_not_release_startup_hold() {
        let mut app = world_app();
        let ground = floor(&mut app, false);
        app.world_mut()
            .get_mut::<Transform>(ground)
            .unwrap()
            .translation
            .y = -10_000.0;
        tick(&mut app, 10);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        assert_mode(&mut app, MoveMode::Walk);
    }

    #[test]
    fn sampled_elevated_start_probe_releases_only_after_rapier_collision() {
        let mut app = world_app();
        let entity = body(&mut app);
        let authored_rotation = app.world().get::<Transform>(entity).unwrap().rotation;
        let camera = app
            .world_mut()
            .query_filtered::<&Transform, With<StreamingCamera>>()
            .single(app.world())
            .unwrap()
            .rotation;
        let authored_camera_y = 5_000.0;
        app.world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .translation
            .y = authored_camera_y - MovementTuning::default().eye_height;
        app.world_mut()
            .resource_mut::<WalkSpawnHold>()
            .sampled_ground_y = Some(0.0);
        let ground = floor(&mut app, true);
        tick(&mut app, 10);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        assert!(
            (walk_spawn_probe_distance(
                authored_camera_y - MovementTuning::default().eye_height,
                Some(0.0),
                &MovementTuning::default(),
            ) - MovementTuning::default().capsule_standing_height * 2.0)
                > 4_000.0
        );

        app.world_mut()
            .entity_mut(ground)
            .remove::<ColliderDisabled>();
        tick(&mut app, 2);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        assert_mode(&mut app, MoveMode::Walk);
        let body_transform = app.world().get::<Transform>(entity).unwrap();
        assert!(
            (body_transform.translation.y + MovementTuning::default().eye_height
                - authored_camera_y)
                .abs()
                < 1.0
        );
        assert!(body_transform.rotation.dot(authored_rotation).abs() > 0.999_99);
        let current_camera_rotation = app
            .world_mut()
            .query_filtered::<&Transform, With<StreamingCamera>>()
            .single(app.world())
            .unwrap()
            .rotation;
        assert!(current_camera_rotation.dot(camera).abs() > 0.999_99);
    }

    #[test]
    fn sampled_elevated_start_rejects_floor_far_below_local_surface() {
        let mut app = world_app();
        let entity = body(&mut app);
        app.world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .translation
            .y = 5_000.0;
        app.world_mut()
            .resource_mut::<WalkSpawnHold>()
            .sampled_ground_y = Some(0.0);
        let ground = floor(&mut app, false);
        app.world_mut()
            .get_mut::<Transform>(ground)
            .unwrap()
            .translation
            .y = -10_000.0;
        tick(&mut app, 10);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        assert_mode(&mut app, MoveMode::Walk);
    }

    #[test]
    fn sampled_elevated_start_accepts_loaded_platform_above_terrain() {
        let mut app = world_app();
        let entity = body(&mut app);
        app.world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .translation
            .y = 5_000.0;
        app.world_mut()
            .resource_mut::<WalkSpawnHold>()
            .sampled_ground_y = Some(0.0);
        let platform = floor(&mut app, true);
        app.world_mut()
            .get_mut::<Transform>(platform)
            .unwrap()
            .translation
            .y = 990.0;
        tick(&mut app, 5);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        app.world_mut()
            .entity_mut(platform)
            .remove::<ColliderDisabled>();
        tick(&mut app, 2);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        assert_mode(&mut app, MoveMode::Walk);
    }

    #[test]
    fn sampled_probe_keeps_tolerance_below_sample_and_stays_bounded() {
        let tuning = MovementTuning::default();
        assert_eq!(walk_spawn_probe_distance(-8.0, Some(0.0), &tuning), 56.0);
        assert_eq!(walk_spawn_probe_distance(-64.0, Some(0.0), &tuning), 0.0);
        assert_eq!(walk_spawn_probe_distance(-80.0, Some(0.0), &tuning), 0.0);
        assert_eq!(
            walk_spawn_probe_distance(100_000.0, Some(0.0), &tuning),
            65_536.0
        );
        assert_eq!(
            walk_spawn_probe_distance(100.0, Some(f32::NAN), &tuning),
            tuning.capsule_standing_height * 2.0
        );
        assert_eq!(walk_spawn_probe_distance(f32::NAN, Some(0.0), &tuning), 0.0);
    }

    #[test]
    fn sampled_start_below_sample_releases_on_loaded_lower_surface() {
        let mut app = world_app();
        let entity = body(&mut app);
        app.world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .translation
            .y = -10.0;
        app.world_mut()
            .resource_mut::<WalkSpawnHold>()
            .sampled_ground_y = Some(0.0);
        let ground = floor(&mut app, false);
        app.world_mut()
            .get_mut::<Transform>(ground)
            .unwrap()
            .translation
            .y = -50.0;
        tick(&mut app, 10);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        assert_mode(&mut app, MoveMode::Walk);
        assert!(
            app.world().get::<Transform>(entity).unwrap().translation.y
                >= -40.0 + body_height_above_feet(&MovementTuning::default()) - 2.0
        );
    }

    #[test]
    fn startup_overlap_uses_existing_failed_placement_fallback() {
        let mut app = world_app();
        app.world_mut().spawn((
            RigidBody::Fixed,
            Collider::cuboid(1000.0, 1000.0, 1000.0),
            Transform::default(),
            world_collision_groups(),
        ));
        tick(&mut app, 5);
        assert_mode(&mut app, MoveMode::Noclip);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        assert_eq!(
            app.world()
                .resource::<WalkEntryStatus>()
                .blocked_reason
                .as_deref(),
            Some("no free capsule placement nearby")
        );
    }

    #[test]
    fn missing_collision_times_out_safely_even_with_pending_streaming() {
        let mut app = world_app();
        app.world_mut()
            .resource_mut::<StreamingMetrics>()
            .loading_cells = 1;
        tick(&mut app, 10);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        app.world_mut().resource_mut::<WalkSpawnHold>().elapsed = WALK_SPAWN_TIMEOUT_SECONDS;
        tick(&mut app, 2);
        assert_mode(&mut app, MoveMode::Noclip);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        assert!(
            app.world()
                .resource::<WalkEntryStatus>()
                .blocked_reason
                .as_ref()
                .unwrap()
                .contains("startup world collision unavailable")
        );
    }

    #[test]
    fn hold_also_times_out_without_streaming_metrics() {
        let mut app = world_app();
        app.world_mut().remove_resource::<StreamingMetrics>();
        tick(&mut app, 5);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        app.world_mut().resource_mut::<WalkSpawnHold>().elapsed = WALK_SPAWN_TIMEOUT_SECONDS;
        tick(&mut app, 2);
        assert_mode(&mut app, MoveMode::Noclip);
    }

    #[test]
    fn explicit_walk_teleport_rearms_destination_hold() {
        let mut app = world_app();
        app.world_mut().write_message(TeleportPlayer {
            position: Vec3::new(20.0, 300.0, 40.0),
            sampled_ground_y: None,
            yaw: 1.0,
        });
        app.update();
        assert_mode(&mut app, MoveMode::Walk);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        assert!(app.world().resource::<WalkSpawnHold>().elapsed <= PHYSICS_TIMESTEP);
        assert!(app.world().resource::<WalkSpawnHold>().destination);
    }

    fn send_state(app: &mut App, state: bevy::input::ButtonState) {
        app.world_mut()
            .write_message(bevy::input::keyboard::KeyboardInput {
                key_code: KeyCode::KeyV,
                logical_key: bevy::input::keyboard::Key::Character("v".into()),
                state,
                text: None,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
        app.update();
    }

    #[test]
    fn v_cancels_hold_and_round_trips_without_rearming_it() {
        use bevy::input::ButtonState::{Pressed, Released};
        let mut app = world_app();
        app.insert_resource(CursorCapture::Captured);
        send_state(&mut app, Pressed);
        assert_mode(&mut app, MoveMode::Noclip);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        send_state(&mut app, Released);
        send_state(&mut app, Pressed);
        assert_mode(&mut app, MoveMode::Walk);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        let entity = body(&mut app);
        let state = app.world().get::<WalkState>(entity).unwrap();
        assert_eq!(state.horizontal_velocity, Vec3::ZERO);
        assert_eq!(state.vertical_velocity(), 0.0);
        assert!(!app.world().resource::<SprintLatch>().0);
    }

    #[test]
    fn tcl_cancels_hold_and_round_trips() {
        let mut app = world_app();
        let entity = body(&mut app);
        integrate_walk(
            &mut app.world_mut().get_mut::<WalkState>(entity).unwrap(),
            &WalkIntent::default(),
            &MovementTuning::default(),
            1.0,
            ResolvedWalk {
                y: 500.0,
                grounded: false,
            },
        );
        app.world_mut().resource_mut::<SprintLatch>().0 = true;
        crate::console::execute_line(app.world_mut(), "tcl");
        assert_eq!(
            app.world()
                .get::<WalkState>(entity)
                .unwrap()
                .vertical_velocity(),
            0.0
        );
        assert!(!app.world().resource::<SprintLatch>().0);
        assert_mode(&mut app, MoveMode::Noclip);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        crate::console::execute_line(app.world_mut(), "tcl");
        assert_mode(&mut app, MoveMode::Walk);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
    }

    /// A live menu owns input but cannot suspend safe fixed-step placement.
    #[test]
    fn pause704_live_menu_releases_spawn_hold_but_unsafe_door_does_not() {
        let mut app = world_app();
        let elapsed = WALK_SPAWN_TIMEOUT_SECONDS - PHYSICS_TIMESTEP * 0.5;
        app.world_mut().resource_mut::<WalkSpawnHold>().elapsed = elapsed;
        app.insert_resource(crate::console::ConsoleState {
            open: true,
            ..default()
        });
        app.insert_resource(crate::door_crossing::DoorCrossing::holding_for_test());
        tick(&mut app, 1);
        assert_eq!(app.world().resource::<WalkSpawnHold>().elapsed, elapsed);
        app.insert_resource(crate::door_crossing::DoorCrossing::default());
        tick(&mut app, 1);
        assert_mode(&mut app, MoveMode::Noclip);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
    }

    /// Seed through the public walking system, which owns the integrator's resolved-input API.
    fn seed_falling_motion(app: &mut App, entity: Entity, jumping: bool) {
        app.world_mut()
            .resource_mut::<Time<Fixed>>()
            .advance_by(std::time::Duration::from_secs_f32(PHYSICS_TIMESTEP));
        app.world_mut()
            .entity_mut(entity)
            .remove::<KinematicCharacterControllerOutput>();
        if jumping {
            app.world_mut()
                .entity_mut(entity)
                .insert(KinematicCharacterControllerOutput {
                    grounded: true,
                    desired_translation: Vec3::ZERO,
                    effective_translation: Vec3::ZERO,
                    collisions: Vec::new(),
                    is_sliding_down_slope: false,
                });
            *app.world_mut().resource_mut::<WalkIntent>() = WalkIntent {
                jump_pressed: true,
                ..default()
            };
            app.world_mut()
                .run_system_once(walk_movement_system)
                .unwrap();
            app.world_mut()
                .entity_mut(entity)
                .remove::<KinematicCharacterControllerOutput>();
        }
        *app.world_mut().resource_mut::<WalkIntent>() = WalkIntent {
            wish_dir: if jumping { Vec3::X } else { Vec3::ZERO },
            target_speed: if jumping { 200.0 } else { 0.0 },
            ..default()
        };
        for _ in 0..if jumping { 120 } else { 6 } {
            app.world_mut()
                .run_system_once(walk_movement_system)
                .unwrap();
        }
    }

    /// Exercise the production schedule with stale input and an airborne body.
    #[test]
    fn pause704_live_menu_integrates_neutral_input_without_erasing_motion() {
        let mut app = world_app();
        app.world_mut().resource_mut::<WalkSpawnHold>().active = false;
        let entity = body(&mut app);
        app.world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .translation
            .y = 2000.0;
        seed_falling_motion(&mut app, entity, true);
        *app.world_mut().resource_mut::<WalkIntent>() = WalkIntent {
            wish_dir: Vec3::Z,
            target_speed: 300.0,
            jump_pressed: true,
        };
        app.world_mut().resource_mut::<SprintLatch>().0 = true;
        app.insert_resource(crate::console::ConsoleState {
            open: true,
            ..default()
        });
        app.insert_resource(CursorCapture::Released);
        tick(&mut app, 2); // The first fixed tick registers the body with Rapier.
        let state = app.world().get::<WalkState>(entity).unwrap();
        let fallen_to = app.world().get::<Transform>(entity).unwrap().translation;
        let falling_step = app
            .world()
            .get::<KinematicCharacterControllerOutput>(entity)
            .unwrap()
            .effective_translation
            .y;
        assert!(falling_step < 0.0, "menu must not suspend gravity");
        assert!(state.horizontal_velocity.x > 0.0, "preserve inertia");
        assert_eq!(state.horizontal_velocity.z, 0.0, "neutral wish");
        assert!(!app.world().resource::<WalkIntent>().jump_pressed);
        assert!(!app.world().resource::<SprintLatch>().0);
        // Focus release must preserve the airborne state on a paused frame.
        app.insert_resource(crate::console::ConsoleState::default());
        app.insert_resource(CursorCapture::Captured);
        let window = app
            .world_mut()
            .spawn((
                Window {
                    focused: false,
                    ..default()
                },
                bevy::window::PrimaryWindow,
            ))
            .id();
        app.world_mut().resource_mut::<Time<Virtual>>().pause();
        tick(&mut app, 1);
        assert_eq!(
            app.world().get::<Transform>(entity).unwrap().translation,
            fallen_to
        );
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Released
        );
        assert!(!app.world().get::<Window>(window).unwrap().focused);
        app.world_mut().get_mut::<Window>(window).unwrap().focused = true;
        tick(&mut app, 1); // Release focus while preserving the earlier manual pause.
        app.world_mut().resource_mut::<Time<Virtual>>().unpause();
        tick(&mut app, 1);
        let resumed_step = app
            .world()
            .get::<KinematicCharacterControllerOutput>(entity)
            .unwrap()
            .effective_translation
            .y;
        assert!(
            resumed_step <= falling_step,
            "focus release must preserve falling speed"
        );
    }

    /// Paused console edits still synchronize the view when no crossing owns its staged pose.
    #[test]
    fn pause704_camera_follows_paused_console_edits_outside_crossings() {
        let mut app = world_app();
        let entity = body(&mut app);
        let position = Vec3::new(123.0, 456.0, 789.0);
        app.world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .translation = position;
        app.world_mut().resource_mut::<Time<Virtual>>().pause();
        app.world_mut()
            .run_system_once(walk_camera_follow_system)
            .unwrap();
        let view = app
            .world_mut()
            .query_filtered::<&Transform, With<StreamingCamera>>()
            .single(app.world())
            .unwrap();
        assert_eq!(
            view.translation,
            position + Vec3::Y * app.world().resource::<MovementTuning>().eye_height
        );
    }

    /// Door commands are consumed a frame after space switching; pause can start in between.
    #[test]
    fn pause704_queued_door_teleport_survives_message_expiry_without_committing() {
        let mut app = world_app();
        app.insert_resource(crate::door_crossing::DoorCrossing::holding_for_test());
        let entity = body(&mut app);
        let before = app.world().get::<Transform>(entity).unwrap().translation;
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<StreamingCamera>>()
            .single(app.world())
            .unwrap();
        // switch_space stages the camera before the next Update consumes the body teleport.
        let staged_view = Vec3::new(123.0, 626.0, 789.0);
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation = staged_view;
        app.world_mut().resource_mut::<Time<Virtual>>().pause();
        app.world_mut().write_message(TeleportPlayer {
            position: Vec3::new(123.0, 456.0, 789.0),
            sampled_ground_y: None,
            yaw: 0.6,
        });
        tick(&mut app, 10);
        assert_eq!(
            app.world().get::<Transform>(entity).unwrap().translation,
            before
        );
        assert_eq!(
            app.world().get::<Transform>(camera).unwrap().translation,
            staged_view,
            "camera follow must not undo the door's staged view during pause"
        );
        app.world_mut().resource_mut::<Time<Virtual>>().unpause();
        tick(&mut app, 1);
        let expected = body_and_camera_for_feet(
            app.world().resource::<MovementTuning>(),
            Vec3::new(123.0, 456.0, 789.0),
        )
        .0;
        assert_eq!(
            app.world().get::<Transform>(entity).unwrap().translation,
            expected
        );
    }

    /// Secondary windows never own game focus; opting out releases input while physics runs.
    #[test]
    fn pause704_always_active_and_secondary_window_focus() {
        let mut app = world_app();
        let primary = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow, CursorOptions::default()))
            .id();
        let secondary = app
            .world_mut()
            .spawn((
                Window {
                    focused: false,
                    ..default()
                },
                CursorOptions::default(),
            ))
            .id();
        app.insert_resource(CursorCapture::Captured);
        tick(&mut app, 1);
        assert!(!app.world().resource::<Time<Virtual>>().is_paused());
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Captured
        );
        assert_eq!(
            app.world()
                .get::<CursorOptions>(secondary)
                .unwrap()
                .grab_mode,
            CursorGrabMode::None
        );
        assert!(app.world().get::<CursorOptions>(secondary).unwrap().visible);
        assert_eq!(
            app.world().get::<CursorOptions>(primary).unwrap().grab_mode,
            CursorGrabMode::Locked
        );
        app.insert_resource(crate::config::EngineConfig {
            always_active: true,
            ..default()
        });
        app.world_mut().get_mut::<Window>(primary).unwrap().focused = false;
        let entity = body(&mut app);
        app.world_mut().resource_mut::<WalkSpawnHold>().active = false;
        app.world_mut()
            .get_mut::<Transform>(entity)
            .unwrap()
            .translation
            .y = 2000.0;
        seed_falling_motion(&mut app, entity, false);
        tick(&mut app, 1);
        assert!(!app.world().resource::<Time<Virtual>>().is_paused());
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Released
        );
        assert!(app.world().get::<Transform>(entity).unwrap().translation.y < 2000.0);
        app.world_mut()
            .resource_mut::<crate::config::EngineConfig>()
            .always_active = false;
        tick(&mut app, 1);
        assert!(app.world().resource::<Time<Virtual>>().is_paused());
    }

    /// The first background frame must be paused before the time system runs.
    #[test]
    fn pause704_primary_focus_pauses_before_fixed_ticks_and_discards_background_time() {
        let mut app = world_app();
        let window = app
            .world_mut()
            .spawn((
                Window {
                    focused: false,
                    ..default()
                },
                bevy::window::PrimaryWindow,
            ))
            .id();
        let before = app.world().resource::<Time<Virtual>>().elapsed();
        tick(&mut app, 1);
        assert!(
            app.world().resource::<Time<Virtual>>().is_paused(),
            "focus loss must pause"
        );
        assert_eq!(app.world().resource::<Time<Virtual>>().elapsed(), before);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs(90),
        ));
        tick(&mut app, 1);
        app.world_mut().get_mut::<Window>(window).unwrap().focused = true;
        tick(&mut app, 1);
        assert_eq!(
            app.world().resource::<Time<Virtual>>().elapsed(),
            before,
            "no resume catch-up"
        );
        app.insert_resource(TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs_f32(PHYSICS_TIMESTEP),
        ));
        tick(&mut app, 1);
        assert!(app.world().resource::<Time<Virtual>>().elapsed() > before);
    }

    #[test]
    fn a_door_crossing_does_not_freeze_the_sprint_latch_under_the_mouse() {
        // Look input is not paused by a crossing, so the latch it reads must keep updating.
        let mut app = world_app();
        app.insert_resource(CursorCapture::Captured);
        app.insert_resource(crate::door_crossing::DoorCrossing::holding_for_test());
        app.world_mut().resource_mut::<SprintLatch>().0 = true;
        let yaw = |app: &App| app.world().resource::<LookIntent>().yaw;
        let before = yaw(&app);
        app.world_mut()
            .write_message(bevy::input::mouse::MouseMotion {
                delta: Vec2::new(100.0, 0.0),
            });
        tick(&mut app, 1);
        // No movement key is held, so the latch is off and yaw uses the full walk rate.
        assert!(!app.world().resource::<SprintLatch>().0);
        assert!((before - yaw(&app) - 100.0 * 0.000_785).abs() < 1.0e-4);
    }

    #[test]
    fn tcl_cancels_a_walk_destination_hold() {
        let mut app = world_app();
        app.world_mut().write_message(TeleportPlayer {
            position: Vec3::new(0.0, 16.0, 0.0),
            sampled_ground_y: Some(0.0),
            yaw: 0.0,
        });
        tick(&mut app, 1);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        crate::console::execute_line(app.world_mut(), "tcl");
        assert_mode(&mut app, MoveMode::Noclip);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        crate::console::execute_line(app.world_mut(), "tcl");
        assert_mode(&mut app, MoveMode::Walk);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
    }

    #[test]
    fn walk_teleport_waits_for_destination_rapier_collision_then_grounds() {
        let mut app = world_app();
        let ground = floor(&mut app, true);
        app.world_mut().write_message(TeleportPlayer {
            position: Vec3::new(0.0, 16.0, 0.0),
            sampled_ground_y: Some(0.0),
            yaw: 0.0,
        });
        tick(&mut app, 10);
        let entity = body(&mut app);
        let held_at = app.world().get::<Transform>(entity).unwrap().translation;
        assert!(app.world().resource::<WalkSpawnHold>().active);
        assert!(app.world().resource::<WalkSpawnHold>().destination);
        assert_eq!(
            held_at,
            body_and_camera_for_feet(&MovementTuning::default(), Vec3::new(0.0, 16.0, 0.0),).0
        );

        app.world_mut()
            .entity_mut(ground)
            .remove::<ColliderDisabled>();
        tick(&mut app, 120);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        assert_mode(&mut app, MoveMode::Walk);
        assert!(headless::player_pose(&mut app).1);
    }

    #[test]
    fn repeated_walk_teleport_replaces_sample_and_restarts_timeout() {
        let mut app = world_app();
        app.world_mut().write_message(TeleportPlayer {
            position: Vec3::new(0.0, 16.0, 0.0),
            sampled_ground_y: Some(0.0),
            yaw: 0.0,
        });
        tick(&mut app, 1);
        app.world_mut().resource_mut::<WalkSpawnHold>().elapsed = 12.0;
        app.world_mut().write_message(TeleportPlayer {
            position: Vec3::new(100.0, 516.0, 100.0),
            sampled_ground_y: Some(500.0),
            yaw: 0.5,
        });
        tick(&mut app, 1);
        let hold = app.world().resource::<WalkSpawnHold>();
        assert!(hold.active && hold.destination);
        assert_eq!(hold.sampled_ground_y, Some(500.0));
        assert!(hold.elapsed <= PHYSICS_TIMESTEP);
        assert_mode(&mut app, MoveMode::Walk);
    }

    #[test]
    fn noclip_teleport_does_not_arm_destination_hold() {
        let mut app = world_app();
        crate::console::execute_line(app.world_mut(), "tcl");
        assert_mode(&mut app, MoveMode::Noclip);
        app.world_mut().write_message(TeleportPlayer {
            position: Vec3::new(0.0, 16.0, 0.0),
            sampled_ground_y: Some(0.0),
            yaw: 0.0,
        });
        tick(&mut app, 1);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        assert_mode(&mut app, MoveMode::Noclip);
    }

    #[test]
    fn destination_hold_times_out_to_visible_noclip_fallback() {
        let mut app = world_app();
        app.world_mut().write_message(TeleportPlayer {
            position: Vec3::new(0.0, 16.0, 0.0),
            sampled_ground_y: Some(0.0),
            yaw: 0.0,
        });
        tick(&mut app, 1);
        app.world_mut().resource_mut::<WalkSpawnHold>().elapsed = WALK_SPAWN_TIMEOUT_SECONDS;
        tick(&mut app, 2);
        assert_mode(&mut app, MoveMode::Noclip);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
        assert!(
            app.world()
                .resource::<WalkEntryStatus>()
                .blocked_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("destination collision unavailable"))
        );
    }

    #[test]
    fn destination_hold_is_paused_during_door_crossing_and_recovery_clears_it() {
        let mut app = world_app();
        app.insert_resource(crate::door_crossing::DoorCrossing::recovery_for_test());
        app.world_mut().write_message(TeleportPlayer {
            position: Vec3::new(0.0, 16.0, 0.0),
            sampled_ground_y: Some(0.0),
            yaw: 0.0,
        });
        tick(&mut app, 1);
        let elapsed = app.world().resource::<WalkSpawnHold>().elapsed;
        tick(&mut app, 5);
        assert!(app.world().resource::<WalkSpawnHold>().active);
        assert_eq!(app.world().resource::<WalkSpawnHold>().elapsed, elapsed);

        crate::console::execute_line(app.world_mut(), "tcl");
        assert_mode(&mut app, MoveMode::Noclip);
        assert!(!app.world().resource::<WalkSpawnHold>().active);
    }
}

#[cfg(test)]
mod simulation_tests {
    use super::headless;
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn walk_falls_when_loaded_ground_is_far_below() {
        let mut app = headless::fixture_app();
        let start = Vec3::new(120.0, 1200.0, 120.0);
        headless::place_player(&mut app, start);
        for _ in 0..20 {
            app.update();
        }
        let (position, _) = headless::player_pose(&mut app);
        assert!(
            position.y < start.y - 10.0,
            "long fall froze at {position:?}"
        );
        assert!(
            app.world()
                .resource::<WalkEntryStatus>()
                .blocked_reason
                .is_none()
        );
    }

    #[test]
    fn walk_falls_without_a_downward_ground_ray() {
        let mut app = headless::fixture_app();
        let start = Vec3::new(2000.0, 1200.0, 2000.0);
        headless::place_player(&mut app, start);
        for _ in 0..20 {
            app.update();
        }
        let (position, _) = headless::player_pose(&mut app);
        assert!(
            position.y < start.y - 10.0,
            "airborne WALK froze at {position:?}"
        );
        assert!(
            app.world()
                .resource::<WalkEntryStatus>()
                .blocked_reason
                .is_none()
        );
    }

    #[test]
    fn noclip_can_enter_walk_and_fall_from_far_above_ground() {
        let mut app = headless::fixture_app();
        let camera_height = 8000.0;
        let start = Vec3::new(120.0, camera_height, 120.0);
        let camera = {
            let mut query = app
                .world_mut()
                .query_filtered::<Entity, With<ControlledCamera>>();
            query.single(app.world()).expect("controlled camera")
        };
        app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap()
            .translation = start;
        app.insert_resource(CursorCapture::Captured);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyV);
        app.world_mut().run_system_once(toggle_mode_system).unwrap();
        assert_eq!(*app.world().resource::<MoveMode>(), MoveMode::Walk);
        let initial = headless::player_pose(&mut app).0;
        assert!(
            (initial.y - (camera_height - app.world().resource::<MovementTuning>().eye_height))
                .abs()
                < 0.01
        );
        for _ in 0..20 {
            app.update();
        }
        let after = headless::player_pose(&mut app).0;
        assert!(
            after.y < initial.y - 10.0,
            "high-altitude WALK froze at {after:?}"
        );
    }

    #[test]
    fn noclip_stays_enabled_when_no_free_capsule_placement_exists() {
        let mut app = headless::fixture_app();
        let tuning = app.world().resource::<MovementTuning>().clone();
        let camera_position = Vec3::new(2000.0, 300.0 + tuning.eye_height, 2000.0);
        app.world_mut().spawn((
            RigidBody::Fixed,
            Collider::cuboid(500.0, 500.0, 500.0),
            world_collision_groups(),
            Transform::from_xyz(2000.0, 300.0, 2000.0),
        ));
        app.update();
        let camera = {
            let mut query = app
                .world_mut()
                .query_filtered::<Entity, With<ControlledCamera>>();
            query.single(app.world()).expect("controlled camera")
        };
        app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap()
            .translation = camera_position;
        app.insert_resource(CursorCapture::Captured);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyV);
        app.world_mut().run_system_once(toggle_mode_system).unwrap();
        assert_eq!(*app.world().resource::<MoveMode>(), MoveMode::Noclip);
        assert_eq!(
            app.world()
                .resource::<WalkEntryStatus>()
                .blocked_reason
                .as_deref(),
            Some("no free capsule placement nearby")
        );
    }

    #[test]
    fn sprint_increases_collision_resolved_speed_and_idle_reads_zero() {
        let mut app = headless::fixture_app();
        app.world_mut().resource_mut::<MovementTuning>().run_speed = 370.0;
        headless::place_player(&mut app, Vec3::new(0.0, 300.0, 700.0));
        for _ in 0..150 {
            app.update();
        }
        let measure = |app: &mut App| {
            let mut query = app
                .world_mut()
                .query_filtered::<&KinematicCharacterControllerOutput, With<PlayerBody>>();
            actual_walk_speed(query.single(app.world()).ok())
        };
        assert_eq!(measure(&mut app), 0.0);
        let tuning = app.world().resource::<MovementTuning>().clone();
        let forward = Vec2::new(0.0, -1.0);
        let run_target = tuning.target_speed(forward, false, false);
        let sprint_target = tuning.target_speed(forward, false, true);
        for target_speed in [run_target, sprint_target] {
            app.insert_resource(WalkIntent {
                wish_dir: Vec3::NEG_Z,
                target_speed,
                jump_pressed: false,
            });
            for _ in 0..45 {
                app.update();
            }
            let actual = measure(&mut app);
            assert!(
                (actual - target_speed).abs() < 2.0,
                "target {target_speed}, actual {actual}"
            );
        }
        assert_eq!(run_target, 370.0);
        assert_eq!(sprint_target, 555.0);
        app.insert_resource(WalkIntent::default());
        for _ in 0..30 {
            app.update();
        }
        assert!(measure(&mut app) < 0.5);
    }

    #[test]
    fn blocked_player_reports_zero_actual_speed_despite_sprint_target() {
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(500.0, 300.0, 0.0));
        for _ in 0..150 {
            app.update();
        }
        let sprint_target =
            app.world()
                .resource::<MovementTuning>()
                .target_speed(Vec2::X, false, true);
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: sprint_target,
            jump_pressed: false,
        });
        for _ in 0..120 {
            app.update();
        }
        let mut query = app
            .world_mut()
            .query_filtered::<&KinematicCharacterControllerOutput, With<PlayerBody>>();
        let actual = actual_walk_speed(query.single(app.world()).ok());
        assert!(sprint_target > 0.0 && actual < 1.0, "actual {actual}");
    }

    #[test]
    fn v1_physics_fixture_camera_has_explicit_hdr_output() {
        let mut app = headless::fixture_app();
        crate::color_pipeline::assert_scene_camera_output(app.world_mut(), 1);
    }

    #[test]
    fn tankards_fall_and_settle_on_fixture_ground() {
        let mut app = headless::fixture_app();
        let start: Vec<f32> = {
            let mut query = app
                .world_mut()
                .query_filtered::<&Transform, With<DebugTankard>>();
            query.iter(app.world()).map(|t| t.translation.y).collect()
        };
        assert_eq!(start.len(), 3);
        assert!(start.iter().all(|y| *y > 200.0));
        let continuous = app
            .world_mut()
            .query_filtered::<&Ccd, With<DebugTankard>>()
            .iter(app.world())
            .filter(|ccd| ccd.enabled)
            .count();
        assert_eq!(continuous, 3, "every tankard has continuous collision");
        for _ in 0..600 {
            app.update();
        }
        let settled: Vec<Vec3> = {
            let mut query = app
                .world_mut()
                .query_filtered::<&Transform, With<DebugTankard>>();
            query.iter(app.world()).map(|t| t.translation).collect()
        };
        assert_eq!(settled.len(), 3);
        for position in &settled {
            assert!(position.is_finite());
            // Ground top is y=0; cup half-height 18 + handle radius margin.
            assert!(
                position.y > -40.0 && position.y < 400.0,
                "tankard height {position:?}"
            );
        }
        assert!(
            app.world()
                .resource::<PhysicsFixtureState>()
                .tankards_settled
                || app
                    .world()
                    .resource::<PhysicsFixtureMetrics>()
                    .tankards_in_contact
                    > 0,
            "tankards never touched the arena"
        );
    }

    #[test]
    fn walk_capsule_spawns_grounds_and_steps_forward() {
        let mut app = headless::fixture_app();
        let tuning = app.world().resource::<MovementTuning>().clone();
        let spawn = Vec3::new(120.0, 300.0, 120.0);
        headless::place_player(&mut app, spawn);
        for _ in 0..240 {
            app.update();
        }
        let (rest, grounded) = headless::player_pose(&mut app);
        assert!(grounded, "capsule never grounded at {rest:?}");
        // Capsule center rests at half-height + radius above the ground pad.
        let expected_rest = tuning.capsule_half_height() + tuning.capsule_radius;
        assert!(
            (rest.y - expected_rest).abs() < 12.0,
            "rest height {} vs {expected_rest}",
            rest.y
        );
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: tuning.run_speed,
            jump_pressed: false,
        });
        for _ in 0..120 {
            app.update();
        }
        let moved = {
            let mut query = app.world_mut().query::<&Transform>();
            query
                .iter(app.world())
                .any(|t| (t.translation.x - spawn.x) > 100.0)
        };
        assert!(moved, "walk capsule did not advance under run intent");
    }

    #[test]
    fn jump_gravity_and_apex_follow_the_highest_resolved_position() {
        let tuning = MovementTuning::default();
        let mut state = WalkState::default();
        integrate_walk(
            &mut state,
            &WalkIntent {
                jump_pressed: true,
                ..default()
            },
            &tuning,
            PHYSICS_TIMESTEP,
            ResolvedWalk {
                y: 100.0,
                grounded: true,
            },
        );
        // Each sample is the position returned by collision resolution on the preceding tick.
        for (y, gravity_multiplier, apex) in [
            (104.0, 1.0, 4.0),
            (104.0, 1.35, 4.0),
            (101.0, 1.35, 4.0),
            (103.0, 1.35, 4.0),
            (105.0, 1.0, 5.0),
        ] {
            let before = state.vertical_velocity();
            integrate_walk(
                &mut state,
                &WalkIntent::default(),
                &tuning,
                PHYSICS_TIMESTEP,
                ResolvedWalk { y, grounded: false },
            );
            let expected_drop = 9.81 * 69.99125 * PHYSICS_TIMESTEP * gravity_multiplier;
            assert!(
                (before - state.vertical_velocity() - expected_drop).abs() < 0.001,
                "resolved y={y}"
            );
            assert_eq!(state.jump_apex(), apex, "resolved y={y}");
        }
    }

    #[test]
    fn jump_ascent_survives_ground_probe_and_lands_on_descent() {
        let tuning = MovementTuning::default();
        let mut state = WalkState::default();
        let mut y = 0.0;
        integrate_walk(
            &mut state,
            &WalkIntent {
                jump_pressed: true,
                ..default()
            },
            &tuning,
            PHYSICS_TIMESTEP,
            ResolvedWalk { y, grounded: true },
        );
        for _ in 0..10 {
            y += state.vertical_velocity() * PHYSICS_TIMESTEP;
            let before = state.vertical_velocity();
            integrate_walk(
                &mut state,
                &WalkIntent {
                    jump_pressed: true,
                    ..default()
                },
                &tuning,
                PHYSICS_TIMESTEP,
                ResolvedWalk { y, grounded: true },
            );
            assert!(state.jump_active());
            assert!(!state.grounded());
            assert!(
                state.vertical_velocity() < before,
                "proximity support relaunched ascent"
            );
        }
        while state.vertical_velocity() > 0.0 {
            y += state.vertical_velocity() * PHYSICS_TIMESTEP;
            integrate_walk(
                &mut state,
                &WalkIntent::default(),
                &tuning,
                PHYSICS_TIMESTEP,
                ResolvedWalk { y, grounded: false },
            );
        }
        integrate_walk(
            &mut state,
            &WalkIntent::default(),
            &tuning,
            PHYSICS_TIMESTEP,
            ResolvedWalk {
                y: 0.0,
                grounded: true,
            },
        );
        assert!(state.grounded());
        assert!(!state.jump_active());
        assert_eq!(state.vertical_velocity(), 0.0);
    }

    #[test]
    fn jump_launches_and_returns_to_ground() {
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(120.0, 200.0, 120.0));
        for _ in 0..240 {
            app.update();
        }
        let (rest, grounded) = headless::player_pose(&mut app);
        assert!(grounded, "player did not settle before jump: {rest:?}");
        let rest_y = rest.y;
        app.insert_resource(WalkIntent {
            jump_pressed: true,
            ..default()
        });
        let mut apex = rest_y;
        for _ in 0..6 {
            app.update();
            apex = apex.max(headless::player_pose(&mut app).0.y);
        }
        app.insert_resource(WalkIntent::default());
        for _ in 0..240 {
            app.update();
            apex = apex.max(headless::player_pose(&mut app).0.y);
        }
        let (landing, grounded) = headless::player_pose(&mut app);
        assert!(
            apex - rest_y > 30.0,
            "jump apex {apex} too low above rest {rest_y}"
        );
        assert!(grounded, "player did not land: {landing:?}");
        assert!(
            (landing.y - rest_y).abs() < 2.0,
            "player landed at {} instead of {rest_y}",
            landing.y
        );
    }
}

// ---------------------------------------------------------------------------
// T3: mouse-look NOCLIP, V toggle, overlay, cursor lifecycle.
// ---------------------------------------------------------------------------

/// Noclip flight tuning (V6).
pub const NOCLIP_SPEED: f32 = 900.0;
pub const NOCLIP_FAST_MULTIPLIER: f32 = 4.0;
/// Retail INI `fMouseHeadingSensitivity` (executable default 0.0125; RF-CAM-010, RF-CAM-011;
/// scaling at `0x1407aeba0`).
pub const MOUSE_HEADING_SENSITIVITY: f32 = 0.0125;
/// Retail INI `fMouseHeadingXScale` (executable default 0.02; RF-CAM-010; `0x1407aeba0`).
pub const MOUSE_HEADING_X_SCALE: f32 = 0.02;
/// Retail INI `fMouseHeadingYScale` (executable default 0.85; RF-CAM-011; `0x1407aeba0`).
pub const MOUSE_HEADING_Y_SCALE: f32 = 0.85;
/// Retail GMST `fLookingSpeed:Camera` (default 0.1; RF-CAM-011; `0x1408fb2e0`).
pub const LOOKING_SPEED: f32 = 0.1;
/// Yaw speed (rad/s) of MOVT `0003580D` NPC_Default_MT, the walk and run movement type: the
/// SPED rotate-in-place and rotate-while-moving values are all pi (RF-CAM-010, RF-MOVE-003; yaw
/// at `0x1407a3fd0`). The registered pack keeps the record bytes, not a parsed column.
pub const ROTATE_SPEED_DEFAULT_MT: f32 = std::f32::consts::PI;
/// Yaw speed (rad/s) of MOVT `00034D9C` NPC_Sprinting_MT: all three SPED rotate values are pi/2
/// (RF-MOVE-003, RF-CAM-010; yaw at `0x1407a3fd0`). The registry rates the link from the yaw
/// factor to the MOVT rotate speed as medium confidence (RF-CAM-010: formula high, factor
/// source medium).
pub const ROTATE_SPEED_SPRINTING_MT: f32 = std::f32::consts::FRAC_PI_2;
/// Standing pitch limit, up and down: +-pi/2 (RF-CAM-012; `0x140693050`). The retail sitting,
/// mounted and flying limits are not used: the engine has no such states yet.
pub const MAX_PITCH_RADIANS: f32 = std::f32::consts::FRAC_PI_2;
const SPEED_DISPLAY_DEADZONE: f32 = 5.0;

/// Sampled look intent (V6, V18).
#[derive(Debug, Clone, Copy, Default, Resource)]
pub struct LookIntent {
    pub yaw: f32,
    pub pitch: f32,
}

/// Marker for the NOCLIP status overlay text (V7).
#[derive(Component, Debug, Clone, Copy)]
pub struct NoclipOverlay;

/// Marker for the camera owned by the NOCLIP/WALK controller.
#[derive(Component, Debug, Clone, Copy)]
pub struct ControlledCamera;

/// Cursor capture state machine (V6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Resource)]
pub enum CursorCapture {
    #[default]
    Released,
    Captured,
}

/// Clamp pitch into the bounded look range (V6).
pub fn clamp_pitch(pitch: f32) -> f32 {
    pitch.clamp(-MAX_PITCH_RADIANS, MAX_PITCH_RADIANS)
}

/// Mouse yaw in radians per count: `fMouseHeadingSensitivity * fMouseHeadingXScale` times the
/// movement type's rotate speed. 0.000785 for walk and run, half of that while sprinting
/// (RF-CAM-010), at 1.0x. The multiplier scales only `fMouseHeadingSensitivity`.
pub fn yaw_radians_per_count(sprinting: bool, sensitivity_multiplier: f32) -> f32 {
    let rotate_speed = if sprinting {
        ROTATE_SPEED_SPRINTING_MT
    } else {
        ROTATE_SPEED_DEFAULT_MT
    };
    let heading_sensitivity = MOUSE_HEADING_SENSITIVITY * sensitivity_multiplier;
    heading_sensitivity * MOUSE_HEADING_X_SCALE * rotate_speed
}

/// Mouse pitch in radians per count: `fMouseHeadingSensitivity * fMouseHeadingYScale *
/// fLookingSpeed`, 0.0010625 (RF-CAM-011). Retail also multiplies by a zoom ratio, which is 1
/// outside zoom; the engine has no zoom state. The multiplier scales `fMouseHeadingSensitivity`.
pub fn pitch_radians_per_count(sensitivity_multiplier: f32) -> f32 {
    let heading_sensitivity = MOUSE_HEADING_SENSITIVITY * sensitivity_multiplier;
    heading_sensitivity * MOUSE_HEADING_Y_SCALE * LOOKING_SPEED
}

/// Sample mouse motion into look intent; uncaptured pointer yields nothing (V6). Retail applies
/// counts directly: no smoothing, no acceleration and no frame-time scaling (RF-CAM-010,
/// RF-CAM-011). The sensitivity multiplier scales the heading setting before axis factors.
pub fn sample_look_intent(
    capture: &CursorCapture,
    mouse_delta: Vec2,
    sprinting: bool,
    sensitivity_multiplier: f32,
    intent: &mut LookIntent,
) {
    if *capture != CursorCapture::Captured {
        return;
    }
    intent.yaw -= mouse_delta.x * yaw_radians_per_count(sprinting, sensitivity_multiplier);
    intent.pitch =
        clamp_pitch(intent.pitch - mouse_delta.y * pitch_radians_per_count(sensitivity_multiplier));
}

/// Noclip displacement from held keys relative to the view (V6).
pub fn noclip_displacement(
    forward: Vec3,
    right: Vec3,
    fly_up: bool,
    fly_down: bool,
    strafe: Vec2,
    fast: bool,
    dt: f32,
) -> Vec3 {
    let mut direction = forward * -strafe.y + right * strafe.x;
    if fly_up {
        direction += Vec3::Y;
    }
    if fly_down {
        direction -= Vec3::Y;
    }
    let speed = if fast {
        NOCLIP_SPEED * NOCLIP_FAST_MULTIPLIER
    } else {
        NOCLIP_SPEED
    };
    direction.normalize_or_zero() * speed * dt
}

fn noclip_overlay_text(mode: MoveMode, status: &WalkEntryStatus) -> String {
    let base = match mode {
        MoveMode::Noclip => "NOCLIP: ON",
        MoveMode::Walk => "NOCLIP: OFF",
    };
    match &status.blocked_reason {
        Some(reason) => format!("{base}  WALK blocked: {reason}"),
        None => base.to_owned(),
    }
}

/// Flip the Rapier collider outlines for the `tcg` command.
fn flip_collision_debug(debug: &mut DebugRenderContext) {
    debug.enabled = !debug.enabled;
}

/// Keep a collider when any part of its world-space AABB reaches the camera's
/// forward half-space. Large terrain pieces crossing the camera plane stay visible.
fn debug_collider_in_front(camera: &Transform, mins: Vec3, maxs: Vec3) -> bool {
    let forward = camera.forward().as_vec3();
    let center = (mins + maxs) * 0.5;
    let half_extents = (maxs - mins) * 0.5;
    forward.dot(center - camera.translation) + forward.abs().dot(half_extents) >= 0.0
}

fn update_collision_debug_visibility(
    debug: Res<DebugRenderContext>,
    camera: Query<&Transform, (With<StreamingCamera>, With<ControlledCamera>)>,
    context: ReadRapierContext,
    overrides: Query<Option<&ColliderDebug>, With<Collider>>,
    mut commands: Commands,
) {
    if !debug.enabled {
        return;
    }
    let (Ok(camera), Ok(context)) = (camera.single(), context.single()) else {
        return;
    };
    for (_, collider) in context.colliders.colliders.iter() {
        let entity = Entity::from_bits(collider.user_data as u64);
        let Ok(current) = overrides.get(entity) else {
            continue;
        };
        let bounds = collider.compute_aabb();
        let mins = Vec3::new(bounds.mins.x, bounds.mins.y, bounds.mins.z);
        let maxs = Vec3::new(bounds.maxs.x, bounds.maxs.y, bounds.maxs.z);
        let wanted = if debug_collider_in_front(camera, mins, maxs) {
            ColliderDebug::AlwaysRender
        } else {
            ColliderDebug::NeverRender
        };
        if current.copied() != Some(wanted) {
            commands.entity(entity).insert(wanted);
        }
    }
}

fn setup_fixture_player(
    mut commands: Commands,
    tuning: Res<MovementTuning>,
    mut look: ResMut<LookIntent>,
    camera: Query<(Entity, &Transform), With<StreamingCamera>>,
) {
    let Ok((camera, view)) = camera.single() else {
        return;
    };
    setup_controlled_player(
        &mut commands,
        &tuning,
        &mut look,
        camera,
        view,
        MoveMode::Noclip,
    );
}

fn setup_world_player(
    mut commands: Commands,
    tuning: Res<MovementTuning>,
    ground_height: Option<Res<WalkSpawnGroundHeight>>,
    mut look: ResMut<LookIntent>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    camera: Query<(Entity, &Transform), With<StreamingCamera>>,
) {
    let Ok((camera, view)) = camera.single() else {
        return;
    };
    setup_controlled_player(
        &mut commands,
        &tuning,
        &mut look,
        camera,
        view,
        MoveMode::Walk,
    );
    commands.insert_resource(WalkSpawnHold {
        active: true,
        elapsed: 0.0,
        sampled_ground_y: ground_height.as_deref().and_then(|height| height.0),
        destination: false,
    });
    commands.insert_resource(tankard_visuals(&mut meshes, &mut materials));
}

fn setup_controlled_player(
    commands: &mut Commands,
    tuning: &MovementTuning,
    look: &mut LookIntent,
    camera: Entity,
    view: &Transform,
    initial_mode: MoveMode,
) {
    let (yaw, pitch, _) = view.rotation.to_euler(EulerRot::YXZ);
    *look = LookIntent { yaw, pitch };
    let body = commands
        .spawn((
            player_controller_bundle(tuning),
            Transform {
                translation: view.translation - Vec3::Y * tuning.eye_height,
                rotation: if initial_mode == MoveMode::Walk {
                    Quat::from_rotation_y(yaw)
                } else {
                    Quat::IDENTITY
                },
                ..default()
            },
        ))
        .id();
    commands.insert_resource(initial_mode);
    if initial_mode == MoveMode::Noclip {
        commands.entity(body).insert(RigidBodyDisabled);
    }
    commands.entity(camera).insert(ControlledCamera);
    commands.spawn((
        Name::new("Noclip overlay"),
        NoclipOverlay,
        Text::new(noclip_overlay_text(
            initial_mode,
            &WalkEntryStatus::default(),
        )),
        TextFont::from_font_size(18.0),
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(12.0),
            left: Val::Px(12.0),
            ..default()
        },
    ));
}

/// T spawns a test tankard in front of the player, only while the cursor is captured. Carrying
/// (`E`) is in [`carry`]. Debug tankards stay in world coordinates and are never children of a
/// streamed cell.
#[allow(clippy::too_many_arguments)]
fn tankard_spawn_input(
    keyboard: Res<ButtonInput<KeyCode>>,
    capture: Res<CursorCapture>,
    mode: Res<MoveMode>,
    tuning: Res<MovementTuning>,
    player: Query<&Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
    camera: Query<&Transform, (With<StreamingCamera>, With<ControlledCamera>)>,
    terrain: Query<(), With<TerrainCollider>>,
    tankards: Query<(), With<DebugTankard>>,
    visuals: Option<Res<TankardVisuals>>,
    context: ReadRapierContext,
    mut commands: Commands,
) {
    if *capture != CursorCapture::Captured || !keyboard.just_pressed(KeyCode::KeyT) {
        return;
    }
    let (Ok(camera), Ok(context)) = (camera.single(), context.single()) else {
        return;
    };
    if tankards.iter().count() >= MAX_LIVE_TANKARDS {
        return;
    }
    let Some(visuals) = visuals else { return };
    let horizontal = camera.forward().as_vec3().with_y(0.0).normalize_or_zero();
    // The ray starts at the eye of the body, not at the blended or lagging view, so the spawn
    // point does not depend on the display's refresh rate. The body is shifted by a floating-origin
    // rebase at once, unlike the view pose, which follows on the next sample. In NOCLIP the camera
    // is authoritative.
    let eye = match (*mode, player.single()) {
        (MoveMode::Walk, Ok(body)) => body.translation + Vec3::Y * tuning.eye_height,
        _ => camera.translation,
    };
    let origin = eye + horizontal * 140.0 + Vec3::Y * 300.0;
    let is_terrain = |entity| terrain.get(entity).is_ok();
    if let Some((_, distance)) = context.cast_ray(
        origin,
        Vec3::NEG_Y,
        700.0,
        true,
        QueryFilter::default().predicate(&is_terrain),
    ) {
        spawn_debug_tankard(
            &mut commands,
            &visuals,
            origin - Vec3::Y * distance + Vec3::Y * 120.0,
        );
    }
}

/// Release cursor/input ownership without altering the physical walking state.
#[allow(clippy::too_many_arguments)]
fn cursor_lifecycle_system(
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    keyboard: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut capture: ResMut<CursorCapture>,
    mut cursor_options: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut motion: MotionReset,
    console: Option<Res<ConsoleState>>,
    menu: Option<Res<crate::settings_ui::SettingsMenuState>>,
) {
    // Focus loss or F3 releases; click recaptures (V6). The console and settings menu own capture
    // while open and on their closing frame; the cursor options below still follow that state.
    let focused = windows.single().map_or(true, |window| window.focused);
    let reporting_capture = console
        .as_ref()
        .is_some_and(|console| console.capture_suppressed);
    let console_owns_capture = console
        .is_some_and(|console| console.open || console.closing || console.capture_suppressed)
        || menu.is_some_and(|menu| menu.blocks_gameplay());
    if reporting_capture || !focused {
        *capture = CursorCapture::Released;
        motion.clear_input();
    }
    if !console_owns_capture {
        if !focused || keyboard.just_pressed(KeyCode::F3) {
            if *capture == CursorCapture::Captured {
                *capture = CursorCapture::Released;
                motion.clear_input();
            }
        } else if mouse_buttons.just_pressed(MouseButton::Left)
            && *capture == CursorCapture::Released
        {
            *capture = CursorCapture::Captured;
        }
    }
    let (grab_mode, visible) = match *capture {
        CursorCapture::Captured => (CursorGrabMode::Locked, false),
        CursorCapture::Released => (CursorGrabMode::None, true),
    };
    for mut options in &mut cursor_options {
        options.grab_mode = grab_mode;
        options.visible = visible;
    }
}

/// Applies live heading sensitivity while retaining WALK sprint and NOCLIP movement types.
fn look_input_system(
    capture: Res<CursorCapture>,
    mode: Res<MoveMode>,
    sprint: Res<SprintLatch>,
    settings: Option<Res<crate::settings::RenderingSettings>>,
    mut mouse_motion: MessageReader<MouseMotion>,
    mut intent: ResMut<LookIntent>,
) {
    let mut delta = Vec2::ZERO;
    for motion in mouse_motion.read() {
        delta += motion.delta;
    }
    // Only WALK has a sprinting movement type; NOCLIP flight uses the default one.
    let sprinting = *mode == MoveMode::Walk && sprint.0;
    let sensitivity = settings
        .as_deref()
        .map_or(1.0, |settings| settings.mouse_sensitivity);
    sample_look_intent(&capture, delta, sprinting, sensitivity, &mut intent);
}

fn noclip_flight_system(
    mode: Res<MoveMode>,
    capture: Res<CursorCapture>,
    time: Res<Time>,
    keyboard: Res<ButtonInput<KeyCode>>,
    look: Res<LookIntent>,
    mut camera: Query<&mut Transform, (With<StreamingCamera>, With<ControlledCamera>)>,
) {
    if *mode != MoveMode::Noclip || *capture != CursorCapture::Captured {
        return;
    }
    let Ok(mut transform) = camera.single_mut() else {
        return;
    };
    transform.rotation = Quat::from_euler(EulerRot::YXZ, look.yaw, look.pitch, 0.0);
    let forward = transform.forward().as_vec3();
    let right = transform.right().as_vec3();
    let strafe = Vec2::new(
        (keyboard.pressed(KeyCode::KeyD) as i8 - keyboard.pressed(KeyCode::KeyA) as i8) as f32,
        (keyboard.pressed(KeyCode::KeyS) as i8 - keyboard.pressed(KeyCode::KeyW) as i8) as f32,
    );
    transform.translation += noclip_displacement(
        forward,
        right,
        keyboard.pressed(KeyCode::Space),
        keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight),
        strafe,
        keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight),
        time.delta_secs(),
    );
}

/// WASD movement input as (strafe, forward-back), each -1, 0 or 1; opposite keys cancel.
fn movement_keys(keyboard: &ButtonInput<KeyCode>) -> Vec2 {
    Vec2::new(
        (keyboard.pressed(KeyCode::KeyD) as i8 - keyboard.pressed(KeyCode::KeyA) as i8) as f32,
        (keyboard.pressed(KeyCode::KeyS) as i8 - keyboard.pressed(KeyCode::KeyW) as i8) as f32,
    )
}

/// Updates the sprint latch. It runs before [`look_input_system`], so the mouse yaw of an
/// update uses the sprint state of that same update (RF-CAM-010).
fn sprint_latch_system(
    mode: Res<MoveMode>,
    capture: Res<CursorCapture>,
    keyboard: Res<ButtonInput<KeyCode>>,
    mut sprint: ResMut<SprintLatch>,
    player: Query<(), (With<PlayerBody>, Without<StreamingCamera>)>,
) {
    if *mode != MoveMode::Walk || *capture != CursorCapture::Captured || player.single().is_err() {
        sprint.0 = false;
        return;
    }
    if movement_keys(&keyboard) == Vec2::ZERO {
        sprint.0 = false;
    } else if keyboard.just_pressed(KeyCode::AltLeft) || keyboard.just_pressed(KeyCode::AltRight) {
        sprint.0 = !sprint.0;
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system parameters are supplied independently.
fn walk_intent_system(
    mode: Res<MoveMode>,
    capture: Res<CursorCapture>,
    keyboard: Res<ButtonInput<KeyCode>>,
    tuning: Res<MovementTuning>,
    look: Res<LookIntent>,
    mut intent: ResMut<WalkIntent>,
    sprint: Res<SprintLatch>,
    mut player: Query<&mut Transform, (With<PlayerBody>, Without<StreamingCamera>)>,
    held: Option<Res<carry::HeldObject>>,
    carry_tuning: Option<Res<carry::CarryTuning>>,
) {
    if *mode != MoveMode::Walk || *capture != CursorCapture::Captured {
        return;
    }
    let Ok(mut body) = player.single_mut() else {
        return;
    };
    // Yaw moves body, pitch moves view (V9).
    body.rotation = Quat::from_axis_angle(Vec3::Y, look.yaw);
    let forward = Vec3::new(-look.yaw.sin(), 0.0, -look.yaw.cos());
    let right = Vec3::new(look.yaw.cos(), 0.0, -look.yaw.sin());
    let strafe = movement_keys(&keyboard);
    let wish = (right * strafe.x - forward * strafe.y).normalize_or_zero();
    let slow = keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight);
    let moving = wish.length_squared() > 0.0;
    intent.wish_dir = wish;
    // Holding a body over `fZKeyHeavyWeight` makes the run speed equal the walk speed (the same
    // block as over-encumbrance, `RF-CARRY-012`). Sprint is not traced and is left unchanged.
    let heavy = held
        .as_deref()
        .and_then(|held| held.held)
        .zip(carry_tuning.as_deref())
        .is_some_and(|(held, carry)| carry.blocks_running(held.mass));
    intent.target_speed = if moving {
        tuning.target_speed(strafe, slow || heavy, sprint.0)
    } else {
        0.0
    };
    intent.jump_pressed |= keyboard.just_pressed(KeyCode::Space);
}

/// One-shot id of the NOCLIP/WALK switch, shared by the V key and the `tcl` command.
#[derive(Resource, Clone, Copy)]
struct MoveModeEffect(SystemId);

fn register_move_mode_effect(world: &mut World) {
    let id = world.register_system(toggle_move_mode_effect);
    world.insert_resource(MoveModeEffect(id));
}

/// V, only while the cursor is captured.
fn toggle_mode_system(
    keyboard: Res<ButtonInput<KeyCode>>,
    capture: Res<CursorCapture>,
    effect: Res<MoveModeEffect>,
    mut commands: Commands,
) {
    if keyboard.just_pressed(KeyCode::KeyV) && *capture == CursorCapture::Captured {
        commands.run_system(effect.0);
    }
}

/// Switch between NOCLIP and WALK, entering WALK only at a collision-free spot.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn toggle_move_mode_effect(
    mut hold: ResMut<WalkSpawnHold>,
    tuning: Res<MovementTuning>,
    mut crossing: Option<ResMut<crate::door_crossing::DoorCrossing>>,
    context: ReadRapierContext,
    mut mode: ResMut<MoveMode>,
    mut status: ResMut<WalkEntryStatus>,
    mut intent: ResMut<WalkIntent>,
    mut sprint: ResMut<SprintLatch>,
    mut look: ResMut<LookIntent>,
    camera: Query<&Transform, (With<StreamingCamera>, With<ControlledCamera>)>,
    mut player: Query<
        (Entity, &mut Transform, &Collider),
        (With<PlayerBody>, Without<StreamingCamera>),
    >,
    mut states: Query<&mut WalkState>,
    mut controllers: Query<&mut KinematicCharacterController>,
    mut commands: Commands,
) {
    // A reason from an earlier attempt must not outlive it: if this attempt stops early
    // (no camera, player or physics context), the caller reports "no player" instead.
    status.blocked_reason = None;
    if let Some(crossing) = crossing.as_mut().filter(|crossing| crossing.is_active()) {
        if crossing.needs_recovery() {
            let Ok((entity, _, _)) = player.single() else {
                return;
            };
            commands.entity(entity).insert(RigidBodyDisabled);
            *mode = MoveMode::Noclip;
            *hold = WalkSpawnHold::default();
            clear_motion_state(&mut intent, &mut sprint, &mut states, &mut controllers);
            crossing.finish_recovery();
            return;
        }
        status.blocked_reason = Some("door crossing in progress".to_owned());
        return;
    }
    match *mode {
        MoveMode::Noclip => {
            let (Ok(camera), Ok((entity, mut body, collider)), Ok(context)) =
                (camera.single(), player.single_mut(), context.single())
            else {
                return;
            };
            match try_enter_walk(&context, &tuning, camera.translation, collider) {
                Ok(spawn) => {
                    body.translation = spawn;
                    let (yaw, _, _) = camera.rotation.to_euler(EulerRot::YXZ);
                    body.rotation = Quat::from_axis_angle(Vec3::Y, yaw);
                    look.yaw = yaw;
                    commands.entity(entity).remove::<RigidBodyDisabled>();
                    status.blocked_reason = None;
                    *mode = MoveMode::Walk;
                }
                Err(reason) => {
                    status.blocked_reason = Some(reason);
                }
            }
        }
        MoveMode::Walk => {
            let Ok((entity, _, _)) = player.single() else {
                return;
            };
            commands.entity(entity).insert(RigidBodyDisabled);
            status.blocked_reason = None;
            *mode = MoveMode::Noclip;
        }
    }
    *hold = WalkSpawnHold::default();
    clear_motion_state(&mut intent, &mut sprint, &mut states, &mut controllers);
}

// ---------------------------------------------------------------------------
// Console commands for the debug keys, under Skyrim's names. Each runs the same
// effect as its key. The `T` (tankard) and `E` (grab) keys have no console
// command: Skyrim has neither.
// ---------------------------------------------------------------------------

fn no_args(args: &[&str], usage: &str) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("usage: {usage}"))
    }
}

/// Skyrim's `ToggleCollision` / `tcl`; the `V` key runs the same effect.
fn toggle_collision_command() -> ConsoleCommand {
    ConsoleCommand {
        name: "ToggleCollision",
        short: Some("tcl"),
        usage: "ToggleCollision",
        help: "toggle noclip flight and walking (key V)",
        handler: Box::new(|world, args| {
            no_args(args, "ToggleCollision")?;
            if !world.contains_resource::<MoveMode>() {
                return Err("noclip is not available in this run".to_owned());
            }
            let before = *world.resource::<MoveMode>();
            let recovering = world
                .get_resource::<crate::door_crossing::DoorCrossing>()
                .is_some_and(|crossing| crossing.needs_recovery());
            let Some(effect) = world.get_resource::<MoveModeEffect>().map(|e| e.0) else {
                return Err("noclip is not available in this run".to_owned());
            };
            world.run_system(effect).map_err(|e| e.to_string())?;
            let after = *world.resource::<MoveMode>();
            let recovered = recovering
                && world
                    .get_resource::<crate::door_crossing::DoorCrossing>()
                    .is_some_and(|crossing| !crossing.is_active());
            if after == before && !recovered {
                let reason = world
                    .get_resource::<WalkEntryStatus>()
                    .and_then(|status| status.blocked_reason.clone());
                return Err(match reason {
                    Some(reason) => format!("cannot walk here: {reason}"),
                    None => "no player to move in this run".to_owned(),
                });
            }
            Ok(match after {
                MoveMode::Noclip => "noclip ON".to_owned(),
                MoveMode::Walk => "noclip OFF (walking)".to_owned(),
            })
        }),
    }
}

/// Skyrim's `ToggleCollisionGeometry` / `tcg`; also available in the Graphics settings tab.
fn toggle_collision_geometry_command() -> ConsoleCommand {
    ConsoleCommand {
        name: "ToggleCollisionGeometry",
        short: Some("tcg"),
        usage: "ToggleCollisionGeometry",
        help: "toggle the collision outlines (also in Graphics settings)",
        handler: Box::new(|world, args| {
            no_args(args, "ToggleCollisionGeometry")?;
            let Some(mut debug) = world.get_resource_mut::<DebugRenderContext>() else {
                return Err("collision outlines are not available in this run".to_owned());
            };
            flip_collision_debug(&mut debug);
            Ok(format!(
                "collision outlines {}",
                if debug.enabled { "ON" } else { "OFF" }
            ))
        }),
    }
}

#[allow(clippy::too_many_arguments)] // Bevy system parameters are supplied independently.
fn overlay_system(
    mode: Res<MoveMode>,
    hold: Option<Res<WalkSpawnHold>>,
    status: Res<WalkEntryStatus>,
    sprint: Res<SprintLatch>,
    crossing: Option<Res<crate::door_crossing::DoorCrossing>>,
    collision_debug: Option<Res<DebugRenderContext>>,
    player: Query<Option<&KinematicCharacterControllerOutput>, With<PlayerBody>>,
    mut overlay: Query<&mut Text, With<NoclipOverlay>>,
) {
    let Ok(mut text) = overlay.single_mut() else {
        return;
    };
    let mut label = noclip_overlay_text(*mode, &status);
    if hold.as_ref().is_some_and(|hold| hold.active) {
        label.push_str("  waiting for WALK collision");
    }
    if *mode == MoveMode::Walk {
        let speed = actual_walk_speed(player.single().ok().flatten());
        let sprint_status = if sprint.0 { "ON" } else { "OFF" };
        label.push_str(&format!("  SPRINT: {sprint_status}  SPEED: {speed:.0} u/s"));
    }
    if let Some(debug) = collision_debug {
        label.push_str(if debug.enabled {
            "  COLLISION: ON"
        } else {
            "  COLLISION: OFF"
        });
    }
    if crossing.is_some_and(|crossing| crossing.needs_recovery()) {
        label.push_str(
            "\nDoor crossing failed. Movement paused. Press V or use tcl to recover in noclip.",
        );
    }
    **text = label;
}

/// Horizontal, collision-resolved player speed from the last fixed physics step.
fn actual_walk_speed(output: Option<&KinematicCharacterControllerOutput>) -> f32 {
    let Some(output) = output else { return 0.0 };
    let movement = output.effective_translation;
    let speed = Vec2::new(movement.x, movement.z).length() / PHYSICS_TIMESTEP;
    if speed < SPEED_DISPLAY_DEADZONE {
        0.0
    } else {
        speed
    }
}

#[cfg(test)]
mod noclip_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn collision_debug_keeps_shapes_crossing_view_plane() {
        let camera = Transform::IDENTITY;
        assert!(debug_collider_in_front(
            &camera,
            Vec3::new(-2.0, -2.0, -10.0),
            Vec3::new(2.0, 2.0, -2.0)
        ));
        assert!(debug_collider_in_front(
            &camera,
            Vec3::new(-2.0, -2.0, -2.0),
            Vec3::new(2.0, 2.0, 2.0)
        ));
        assert!(!debug_collider_in_front(
            &camera,
            Vec3::new(-2.0, -2.0, 2.0),
            Vec3::new(2.0, 2.0, 10.0)
        ));
    }

    #[test]
    fn walk_view_pitch_tracks_mouse_look_without_tilting_body() {
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
        app.world_mut().resource_mut::<LookIntent>().pitch = 0.35;
        app.world_mut()
            .run_system_once(walk_camera_follow_system)
            .unwrap();
        let mut camera = app
            .world_mut()
            .query_filtered::<&Transform, With<StreamingCamera>>();
        let view = camera.single(app.world()).unwrap();
        let (_, pitch, _) = view.rotation.to_euler(EulerRot::YXZ);
        assert!((pitch - 0.35).abs() < 1.0e-5);
        let mut player = app
            .world_mut()
            .query_filtered::<&Transform, With<PlayerBody>>();
        let body = player.single(app.world()).unwrap();
        assert!(body.rotation.to_euler(EulerRot::YXZ).1.abs() < 1.0e-5);
    }

    #[test]
    fn jump_press_survives_input_updates_until_fixed_tick() {
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
        app.insert_resource(CursorCapture::Captured);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Space);
        app.world_mut().run_system_once(walk_intent_system).unwrap();
        assert!(app.world().resource::<WalkIntent>().jump_pressed);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();
        app.world_mut().run_system_once(walk_intent_system).unwrap();
        assert!(app.world().resource::<WalkIntent>().jump_pressed);
    }

    #[test]
    fn pitch_clamps_inside_bounded_range() {
        // RF-CAM-012: the standing limit is +-pi/2.
        assert_eq!(MAX_PITCH_RADIANS, std::f32::consts::FRAC_PI_2);
        assert_eq!(clamp_pitch(5.0), MAX_PITCH_RADIANS);
        assert_eq!(clamp_pitch(-5.0), -MAX_PITCH_RADIANS);
        assert_eq!(clamp_pitch(0.3), 0.3);
    }

    /// The default multiplier preserves main's walk/run and sprint movement-type yaw rates.
    #[test]
    fn mouse_yaw_per_count_follows_retail_rates() {
        // RF-CAM-010: 0.0125 * 0.02 * pi = 0.000785 rad per count walking and running,
        // 0.0125 * 0.02 * pi / 2 = 0.0003925 sprinting.
        assert!((yaw_radians_per_count(false, 1.0) - 0.000_785).abs() < 1.0e-6);
        assert!((yaw_radians_per_count(true, 1.0) - 0.000_392_5).abs() < 1.0e-6);
        assert!(
            (yaw_radians_per_count(true, 1.0) * 2.0 - yaw_radians_per_count(false, 1.0)).abs()
                < 1.0e-9
        );
    }

    /// The default multiplier preserves main's retail vertical rate.
    #[test]
    fn mouse_pitch_per_count_follows_retail_rate() {
        // RF-CAM-011: 0.0125 * 0.85 * 0.1 = 0.0010625 rad per count.
        assert!((pitch_radians_per_count(1.0) - 0.001_062_5).abs() < 1.0e-7);
    }

    /// Default-sensitivity counts retain distinct normal and sprint yaw rates.
    #[test]
    fn sampled_counts_turn_by_the_retail_rates() {
        let mut intent = LookIntent::default();
        sample_look_intent(
            &CursorCapture::Captured,
            Vec2::new(100.0, 200.0),
            false,
            1.0,
            &mut intent,
        );
        assert!((intent.yaw + 100.0 * 0.000_785).abs() < 1.0e-4);
        assert!((intent.pitch + 200.0 * 0.001_062_5).abs() < 1.0e-4);
        let mut sprinting = LookIntent::default();
        sample_look_intent(
            &CursorCapture::Captured,
            Vec2::new(100.0, 0.0),
            true,
            1.0,
            &mut sprinting,
        );
        assert!((sprinting.yaw + 100.0 * 0.000_392_5).abs() < 1.0e-4);
    }

    /// Default-sensitivity input retains both exact standing pitch bounds.
    #[test]
    fn sampled_pitch_stops_at_plus_or_minus_ninety_degrees() {
        let mut intent = LookIntent::default();
        sample_look_intent(
            &CursorCapture::Captured,
            Vec2::new(0.0, 1.0e6),
            false,
            1.0,
            &mut intent,
        );
        assert_eq!(intent.pitch, -std::f32::consts::FRAC_PI_2);
        sample_look_intent(
            &CursorCapture::Captured,
            Vec2::new(0.0, -2.0e6),
            false,
            1.0,
            &mut intent,
        );
        assert_eq!(intent.pitch, std::f32::consts::FRAC_PI_2);
    }

    #[test]
    fn look_input_system_has_no_smoothing_or_frame_time_scaling() {
        // RF-CAM-010, RF-CAM-011: the same counts give the same rotation at any frame time.
        let turn = |frame_secs: f32, mode: MoveMode, sprinting: bool| {
            let mut app = App::new();
            app.add_message::<bevy::input::mouse::MouseMotion>()
                .insert_resource(CursorCapture::Captured)
                .insert_resource(mode)
                .insert_resource(SprintLatch(sprinting))
                .insert_resource(LookIntent::default())
                .insert_resource(Time::<()>::default());
            app.world_mut()
                .resource_mut::<Time<()>>()
                .advance_by(std::time::Duration::from_secs_f32(frame_secs));
            app.world_mut()
                .write_message(bevy::input::mouse::MouseMotion {
                    delta: Vec2::new(1.0, 1.0),
                });
            app.world_mut().run_system_once(look_input_system).unwrap();
            let look = app.world().resource::<LookIntent>();
            (look.yaw, look.pitch)
        };
        let fast = turn(1.0 / 240.0, MoveMode::Walk, false);
        let slow = turn(1.0 / 20.0, MoveMode::Walk, false);
        assert_eq!(fast, slow);
        assert!((fast.0 + 0.000_785).abs() < 1.0e-6);
        assert!((fast.1 + 0.001_062_5).abs() < 1.0e-7);
        // Sprinting halves yaw only, and only in WALK.
        let sprint = turn(1.0 / 60.0, MoveMode::Walk, true);
        assert!((sprint.0 + 0.000_392_5).abs() < 1.0e-6);
        assert_eq!(sprint.1, fast.1);
        assert_eq!(turn(1.0 / 60.0, MoveMode::Noclip, true), fast);
    }

    #[test]
    fn the_first_update_of_a_sprint_change_uses_that_updates_yaw_rate() {
        use bevy::input::mouse::MouseMotion;
        let mut app = App::new();
        app.add_message::<MouseMotion>()
            .insert_resource(MoveMode::Walk)
            .insert_resource(CursorCapture::Captured)
            .insert_resource(ButtonInput::<KeyCode>::default())
            .insert_resource(SprintLatch::default())
            .insert_resource(LookIntent::default())
            .add_systems(Update, (sprint_latch_system, look_input_system).chain());
        app.world_mut().spawn((PlayerBody, Transform::default()));
        let update_with_counts = |app: &mut App| {
            let before = app.world().resource::<LookIntent>().yaw;
            app.world_mut().write_message(MouseMotion {
                delta: Vec2::new(100.0, 0.0),
            });
            app.update();
            before - app.world().resource::<LookIntent>().yaw
        };
        // Sprint starts (Alt pressed with W held) in this update: half rate at once.
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.press(KeyCode::KeyW);
            keys.press(KeyCode::AltLeft);
        }
        let starting = update_with_counts(&mut app);
        assert!(
            (starting - 100.0 * 0.000_392_5).abs() < 1.0e-4,
            "{starting}"
        );
        // Sprint continues.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();
        let continuing = update_with_counts(&mut app);
        assert!(
            (continuing - 100.0 * 0.000_392_5).abs() < 1.0e-4,
            "{continuing}"
        );
        // The last movement key is released in this update: full rate at once.
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.clear();
            keys.release(KeyCode::KeyW);
        }
        let stopping = update_with_counts(&mut app);
        assert!((stopping - 100.0 * 0.000_785).abs() < 1.0e-4, "{stopping}");
    }

    /// Cursor release blocks look input even when sensitivity is explicitly supplied.
    #[test]
    fn uncaptured_pointer_yields_no_look_intent() {
        let mut intent = LookIntent::default();
        sample_look_intent(
            &CursorCapture::Released,
            Vec2::new(40.0, 20.0),
            false,
            1.0,
            &mut intent,
        );
        assert_eq!(intent.yaw, 0.0);
        assert_eq!(intent.pitch, 0.0);
        sample_look_intent(
            &CursorCapture::Captured,
            Vec2::new(40.0, 20.0),
            false,
            1.0,
            &mut intent,
        );
        assert!(intent.yaw < 0.0);
        assert!(intent.pitch < 0.0);
    }

    /// The default look input preserves the unrounded retail rates used by #709.
    #[test]
    fn mouse_sensitivity_default_uses_retail_axis_rates() {
        let mut intent = LookIntent::default();
        sample_look_intent(
            &CursorCapture::Captured,
            Vec2::new(100.0, 200.0),
            false,
            1.0,
            &mut intent,
        );
        assert!((intent.yaw + 0.078_539_82).abs() < 1.0e-7, "{}", intent.yaw);
        assert!((intent.pitch + 0.212_5).abs() < 1.0e-7, "{}", intent.pitch);
    }

    /// Scaling heading sensitivity retains sprint's half yaw and NOCLIP's default movement type.
    #[test]
    fn mouse_sensitivity_scales_heading_for_each_movement_type() {
        let turn = |mode: MoveMode, sprinting: bool, mouse_sensitivity: f32| {
            let mut app = App::new();
            app.add_message::<MouseMotion>()
                .insert_resource(CursorCapture::Captured)
                .insert_resource(mode)
                .insert_resource(SprintLatch(sprinting))
                .insert_resource(LookIntent::default())
                .insert_resource(crate::settings::RenderingSettings {
                    mouse_sensitivity,
                    ..default()
                });
            app.world_mut().write_message(MouseMotion {
                delta: Vec2::new(100.0, 200.0),
            });
            app.world_mut().run_system_once(look_input_system).unwrap();
            let look = app.world().resource::<LookIntent>();
            Vec2::new(look.yaw, look.pitch)
        };
        for multiplier in [0.8, 1.0, 2.0, 4.0] {
            let walk = turn(MoveMode::Walk, false, multiplier);
            let sprint = turn(MoveMode::Walk, true, multiplier);
            let noclip = turn(MoveMode::Noclip, true, multiplier);
            let expected_walk = Vec2::new(-0.078_539_82, -0.212_5) * multiplier;
            let expected_sprint = Vec2::new(-0.039_269_91, -0.212_5) * multiplier;
            assert!((walk - expected_walk).length() < 1.0e-6, "{walk:?}");
            assert!((sprint - expected_sprint).length() < 1.0e-6, "{sprint:?}");
            assert_eq!(sprint.y, walk.y);
            assert_eq!(noclip, walk, "NOCLIP must not use the WALK sprint latch");
        }
    }

    /// Every multiplier retains the same exact retail standing pitch bounds.
    #[test]
    fn mouse_sensitivity_keeps_retail_pitch_limits_at_every_scale() {
        assert_eq!(MAX_PITCH_RADIANS, std::f32::consts::FRAC_PI_2);
        for multiplier in [0.8, 1.0, 2.0, 4.0] {
            for sprinting in [false, true] {
                let mut intent = LookIntent::default();
                sample_look_intent(
                    &CursorCapture::Captured,
                    Vec2::new(0.0, 1.0e6),
                    sprinting,
                    multiplier,
                    &mut intent,
                );
                assert_eq!(intent.pitch, -std::f32::consts::FRAC_PI_2);
                sample_look_intent(
                    &CursorCapture::Captured,
                    Vec2::new(0.0, -2.0e6),
                    sprinting,
                    multiplier,
                    &mut intent,
                );
                assert_eq!(intent.pitch, std::f32::consts::FRAC_PI_2);
            }
        }
    }

    /// Actual mouse input reads live preferences without a renderer or frame-time scaling.
    #[test]
    fn mouse_sensitivity_live_edits_scale_both_axes_without_time_scaling() {
        let turn = |sensitivity: Option<f32>, seconds: f32| {
            let mut app = App::new();
            app.add_message::<MouseMotion>()
                .insert_resource(CursorCapture::Captured)
                .insert_resource(MoveMode::Walk)
                .insert_resource(SprintLatch(false))
                .insert_resource(LookIntent::default())
                .insert_resource(Time::<()>::default());
            if let Some(mouse_sensitivity) = sensitivity {
                app.insert_resource(crate::settings::RenderingSettings {
                    mouse_sensitivity,
                    ..default()
                });
            }
            app.world_mut()
                .resource_mut::<Time<()>>()
                .advance_by(std::time::Duration::from_secs_f32(seconds));
            app.world_mut().write_message(MouseMotion {
                delta: Vec2::new(100.0, 200.0),
            });
            app.world_mut().run_system_once(look_input_system).unwrap();
            let intent = app.world().resource::<LookIntent>();
            Vec2::new(intent.yaw, intent.pitch)
        };
        let baseline = turn(Some(1.0), 1.0 / 240.0);
        assert_eq!(baseline, turn(Some(1.0), 1.0 / 20.0));
        assert_eq!(baseline, turn(None, 1.0 / 60.0));
        let double = turn(Some(2.0), 1.0 / 60.0);
        assert!((double.x + 0.157_079_64).abs() < 1.0e-7);
        assert!((double.y + 0.425).abs() < 1.0e-7);
        assert!((turn(Some(0.8), 1.0 / 60.0) - baseline * 0.8).length() < 1.0e-7);

        let mut app = App::new();
        app.add_message::<MouseMotion>()
            .insert_resource(CursorCapture::Captured)
            .insert_resource(MoveMode::Walk)
            .insert_resource(SprintLatch(false))
            .insert_resource(LookIntent::default())
            .insert_resource(crate::settings::RenderingSettings::default())
            .add_systems(Update, look_input_system);
        app.world_mut().write_message(MouseMotion {
            delta: Vec2::new(100.0, 200.0),
        });
        app.update();
        app.world_mut()
            .resource_mut::<crate::settings::RenderingSettings>()
            .mouse_sensitivity = 2.0;
        app.world_mut().write_message(MouseMotion {
            delta: Vec2::new(100.0, 200.0),
        });
        app.update();
        let intent = app.world().resource::<LookIntent>();
        assert!((Vec2::new(intent.yaw, intent.pitch) - baseline * 3.0).length() < 1.0e-7);
        app.insert_resource(CursorCapture::Released);
        app.world_mut().write_message(MouseMotion {
            delta: Vec2::splat(100.0),
        });
        app.update();
        let stopped = app.world().resource::<LookIntent>();
        assert!((Vec2::new(stopped.yaw, stopped.pitch) - baseline * 3.0).length() < 1.0e-7);
    }

    #[test]
    fn noclip_flies_relative_to_view_with_fast_multiplier() {
        let slow = noclip_displacement(
            Vec3::NEG_Z,
            Vec3::X,
            false,
            false,
            Vec2::new(0.0, -1.0),
            false,
            1.0,
        );
        assert!((slow - Vec3::NEG_Z * NOCLIP_SPEED).length() < 0.01);
        let fast = noclip_displacement(
            Vec3::NEG_Z,
            Vec3::X,
            false,
            false,
            Vec2::new(0.0, -1.0),
            true,
            1.0,
        );
        assert!((fast.length() - NOCLIP_SPEED * NOCLIP_FAST_MULTIPLIER).abs() < 0.01);
        let rise = noclip_displacement(Vec3::NEG_Z, Vec3::X, true, false, Vec2::ZERO, false, 1.0);
        assert!((rise - Vec3::Y * NOCLIP_SPEED).length() < 0.01);
    }

    #[test]
    fn overlay_reports_mode_and_block_reason_without_control_hints() {
        let status = WalkEntryStatus::default();
        assert_eq!(noclip_overlay_text(MoveMode::Noclip, &status), "NOCLIP: ON");
        assert_eq!(noclip_overlay_text(MoveMode::Walk, &status), "NOCLIP: OFF");
        let blocked = WalkEntryStatus {
            blocked_reason: Some("no free capsule placement nearby".to_owned()),
        };
        assert!(noclip_overlay_text(MoveMode::Noclip, &blocked).contains("WALK blocked"));
    }
}

/// P1 gate evidence (T4, V16): slope/wall/step behavior, toggle state
/// agreement, fixed-step determinism, and render-origin rebase alignment.
#[cfg(test)]
mod gate_tests {
    use super::headless;
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn walk_crosses_narrow_raised_floor_join() {
        let mut app = headless::fixture_app();
        for (x, width, top) in [(0.0, 12.0, 15.0), (206.0, 400.0, 30.0)] {
            app.world_mut().spawn((
                RigidBody::Fixed,
                Collider::cuboid(width * 0.5, top * 0.5, 80.0),
                world_collision_groups(),
                Transform::from_xyz(x, top * 0.5, 850.0),
            ));
        }
        headless::place_player(&mut app, Vec3::new(-150.0, 200.0, 850.0));
        for _ in 0..150 {
            app.update();
        }
        let start = headless::player_pose(&mut app).0;
        assert!(start.y < 80.0, "player never grounded: {start:?}");
        let tuning = app.world().resource::<MovementTuning>();
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: tuning.run_speed,
            jump_pressed: false,
        });
        let mut peak = start.y;
        for _ in 0..180 {
            app.update();
            peak = peak.max(headless::player_pose(&mut app).0.y);
        }
        let (end, grounded) = headless::player_pose(&mut app);
        assert!(
            end.x > 120.0 && peak > start.y + 25.0 && grounded,
            "player stuck at raised floor join: {start:?} -> {end:?}, peak={peak}, grounded={grounded}"
        );
    }

    #[test]
    fn walk_camera_pitch_uses_current_mouse_intent() {
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
        app.world_mut().resource_mut::<LookIntent>().pitch = 0.6;
        app.update();
        let mut camera = app
            .world_mut()
            .query_filtered::<&Transform, With<ControlledCamera>>();
        let view = camera.single(app.world()).expect("controlled camera");
        let (_, pitch, _) = view.rotation.to_euler(EulerRot::YXZ);
        assert!((pitch - 0.6).abs() < 0.001, "WALK pitch was {pitch}");
        let mut body = app
            .world_mut()
            .query_filtered::<&Transform, With<PlayerBody>>();
        let pose = body.single(app.world()).expect("player body");
        assert!(pose.rotation.x.abs() < 0.001 && pose.rotation.z.abs() < 0.001);
    }

    #[test]
    fn riverwood_tankard_spawns_on_the_terrain() {
        let mut app = headless::fixture_app();
        let visuals = {
            let world = app.world_mut();
            world.resource_scope(|world, mut meshes: Mut<Assets<Mesh>>| {
                tankard_visuals(
                    &mut meshes,
                    &mut world.resource_mut::<Assets<StandardMaterial>>(),
                )
            })
        };
        app.insert_resource(visuals);
        let ground = {
            let mut query = app
                .world_mut()
                .query_filtered::<(Entity, &Transform), With<FixtureArena>>();
            query
                .iter(app.world())
                .find(|(_, pose)| pose.translation.y == -20.0)
                .unwrap()
                .0
        };
        app.world_mut().entity_mut(ground).insert(TerrainCollider);
        let camera = {
            let mut query = app
                .world_mut()
                .query_filtered::<Entity, With<ControlledCamera>>();
            query.single(app.world()).unwrap()
        };
        app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap()
            .clone_from(
                &Transform::from_xyz(0.0, 200.0, 0.0)
                    .looking_at(Vec3::new(0.0, 200.0, -1.0), Vec3::Y),
            );
        app.update();
        app.insert_resource(CursorCapture::Captured);
        // The Riverwood test tankards already exist; the one T spawns is the one not listed here.
        let existing: Vec<Entity> = {
            let mut query = app
                .world_mut()
                .query_filtered::<Entity, With<DebugTankard>>();
            query.iter(app.world()).collect()
        };
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyT);
        let ground_hit = app
            .world_mut()
            .run_system_once(
                |context: ReadRapierContext, terrain: Query<(), With<TerrainCollider>>| {
                    let is_terrain = |entity| terrain.get(entity).is_ok();
                    context.single().unwrap().cast_ray(
                        Vec3::new(0.0, 500.0, -140.0),
                        Vec3::NEG_Y,
                        700.0,
                        true,
                        QueryFilter::default().predicate(&is_terrain),
                    )
                },
            )
            .unwrap();
        let any_hit = app
            .world_mut()
            .run_system_once(|context: ReadRapierContext| {
                context.single().unwrap().cast_ray(
                    Vec3::new(0.0, 500.0, -140.0),
                    Vec3::NEG_Y,
                    700.0,
                    true,
                    QueryFilter::default(),
                )
            })
            .unwrap();
        assert!(
            ground_hit.is_some(),
            "fixture terrain ray must hit; nearest={any_hit:?}, ground={ground:?}"
        );
        app.world_mut()
            .run_system_once(tankard_spawn_input)
            .unwrap();
        let spawned = {
            let mut query = app
                .world_mut()
                .query_filtered::<(Entity, &Transform), With<DebugTankard>>();
            query
                .iter(app.world())
                .find(|(entity, _)| !existing.contains(entity))
                .map(|(entity, pose)| (entity, pose.translation))
                .expect("spawned tankard above terrain")
        };
        let mut query = app
            .world_mut()
            .query_filtered::<Entity, With<DebugTankard>>();
        assert_eq!(query.iter(app.world()).count(), 4);
        app.update();
        let spawned_pose = app
            .world()
            .entity(spawned.0)
            .get::<Transform>()
            .unwrap()
            .translation;
        assert!(
            spawned_pose.y > 0.0 && spawned_pose.is_finite(),
            "spawned tankard pose {spawned_pose:?}"
        );
    }

    fn run_speed_intent(app: &mut App, wish_dir: Vec3) {
        let speed = app.world().resource::<MovementTuning>().run_speed;
        app.insert_resource(WalkIntent {
            wish_dir,
            target_speed: speed,
            jump_pressed: false,
        });
    }

    #[test]
    fn wall_blocks_step_mounts_slope_climbs() {
        let mut app = headless::fixture_app();
        // Wall face sits at x=730 (center 760, half-width 30).
        headless::place_player(&mut app, Vec3::new(500.0, 300.0, 0.0));
        for _ in 0..120 {
            app.update();
        }
        run_speed_intent(&mut app, Vec3::X);
        for _ in 0..600 {
            app.update();
        }
        let (blocked, _) = headless::player_pose(&mut app);
        let tuning = app.world().resource::<MovementTuning>().clone();
        assert!(
            blocked.x < 730.0 - tuning.capsule_radius,
            "capsule tunneled the wall: {blocked:?}"
        );

        // Step top is y=24; the 36-unit autostep budget includes it (V12).
        headless::place_player(&mut app, Vec3::new(100.0, 300.0, 240.0));
        for _ in 0..120 {
            app.update();
        }
        run_speed_intent(&mut app, Vec3::X);
        for _ in 0..600 {
            app.update();
        }
        let (stepped, step_grounded) = headless::player_pose(&mut app);
        assert!(step_grounded, "capsule never grounded near step");
        let rest = tuning.capsule_half_height() + tuning.capsule_radius;
        // Walk the step again tracking peak: autostep must lift the capsule
        // onto the 24-unit block, not stall against it (V12).
        headless::place_player(&mut app, Vec3::new(100.0, 300.0, 240.0));
        for _ in 0..120 {
            app.update();
        }
        run_speed_intent(&mut app, Vec3::X);
        let mut step_peak = 0.0f32;
        for _ in 0..600 {
            app.update();
            let (pose, _) = headless::player_pose(&mut app);
            step_peak = step_peak.max(pose.y);
        }
        assert!(
            step_peak > rest + 18.0,
            "capsule did not mount the 24-unit step: peak {step_peak}, rest {rest}, end {stepped:?}"
        );

        // Hill rises toward -x; climbing +x-to--x gains height (V12).
        // Slope is ~18 degrees, under the 50-degree climb limit.
        headless::place_player(&mut app, Vec3::new(100.0, 300.0, -500.0));
        for _ in 0..120 {
            app.update();
        }
        run_speed_intent(&mut app, Vec3::NEG_X);
        let mut peak = 0.0f32;
        for _ in 0..600 {
            app.update();
            let (pose, _) = headless::player_pose(&mut app);
            peak = peak.max(pose.y);
        }
        assert!(peak > 200.0, "capsule did not climb the hill: peak {peak}");
    }

    #[test]
    fn tankards_contact_wall_and_settle_without_penetration() {
        let mut app = headless::fixture_app();
        // Hurl one tankard at the wall; it must collide, not tunnel (V15).
        let hurled = {
            let mut query = app
                .world_mut()
                .query_filtered::<Entity, With<DebugTankard>>();
            let first = query.iter(app.world()).next().expect("tankard");
            app.world_mut().entity_mut(first).insert((
                Velocity::linear(Vec3::X * 1500.0),
                Transform::from_xyz(0.0, 300.0, 0.0),
            ));
            first
        };
        for _ in 0..900 {
            app.update();
        }
        let position = app
            .world()
            .get::<Transform>(hurled)
            .expect("tankard transform")
            .translation;
        assert!(position.is_finite());
        // Wall spans x in [730, 790]; the cup radius keeps contact outside it.
        assert!(
            position.x < 730.0 - TANKARD_CUP_RADIUS * 0.5 || position.y > 420.0,
            "tankard tunneled the wall: {position:?}"
        );
        // Every tankard rests at or above the ground pad (no sinking).
        let mut query = app
            .world_mut()
            .query_filtered::<&Transform, With<DebugTankard>>();
        for transform in query.iter(app.world()) {
            assert!(
                transform.translation.y > -TANKARD_CUP_HALF_HEIGHT,
                "tankard sank through ground: {:?}",
                transform.translation
            );
        }
    }

    #[test]
    fn toggle_clears_state_and_overlay_matches_mode() {
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
        for _ in 0..120 {
            app.update();
        }
        // Simulate WALK->NOCLIP: stale intent must clear (V14).
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: 300.0,
            jump_pressed: true,
        });
        let _ = app.world_mut().run_system_once(
            |mut intent: ResMut<WalkIntent>,
             mut sprint: ResMut<SprintLatch>,
             mut states: Query<&mut WalkState>,
             mut controllers: Query<&mut KinematicCharacterController>| {
                clear_motion_state(&mut intent, &mut sprint, &mut states, &mut controllers);
            },
        );
        let intent = app.world().resource::<WalkIntent>();
        assert_eq!(intent.wish_dir, Vec3::ZERO);
        assert!(!intent.jump_pressed);
        // Overlay text tracks the mode resource every frame (V7).
        for _ in 0..5 {
            app.update();
        }
        let mut query = app
            .world_mut()
            .query_filtered::<&Text, With<NoclipOverlay>>();
        let text = query.single(app.world()).expect("overlay").clone();
        assert_eq!(text.as_str(), "NOCLIP: OFF  SPRINT: OFF  SPEED: 0 u/s");
        app.insert_resource(MoveMode::Noclip);
        for _ in 0..5 {
            app.update();
        }
        let mut query = app
            .world_mut()
            .query_filtered::<&Text, With<NoclipOverlay>>();
        let text = query.single(app.world()).expect("overlay").clone();
        assert_eq!(text.as_str(), "NOCLIP: ON");
    }

    #[test]
    fn a_failed_crossing_holds_the_walking_body_and_shows_how_to_recover() {
        let mut app = headless::fixture_app();
        let start = Vec3::new(0.0, 5_000.0, 0.0);
        headless::place_player(&mut app, start);
        app.insert_resource(crate::door_crossing::DoorCrossing::recovery_for_test());
        for _ in 0..60 {
            app.update();
        }
        let (held, _) = headless::player_pose(&mut app);
        assert!((held - start).length() < 1.0, "unguarded fall: {held}");
        let mut query = app
            .world_mut()
            .query_filtered::<&Text, With<NoclipOverlay>>();
        let text = query.single(app.world()).unwrap();
        assert!(text.contains("Movement paused"));
        assert!(text.contains("Press V or use tcl to recover in noclip"));
    }

    #[test]
    fn fixed_step_walk_is_frame_rate_independent() {
        // Two simulated seconds at 30/60/120 render fps must all run the same
        // 120 fixed physics ticks and reach the same point (V18).
        fn drive(render_hz: u32) -> Vec3 {
            let mut app = headless::fixture_app();
            headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
            for _ in 0..120 {
                app.update();
            }
            let speed = app.world().resource::<MovementTuning>().run_speed;
            app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_secs_f64(1.0 / render_hz as f64),
            ));
            for _ in 0..render_hz * 2 {
                app.insert_resource(WalkIntent {
                    wish_dir: Vec3::X,
                    target_speed: speed,
                    jump_pressed: false,
                });
                app.update();
            }
            headless::player_pose(&mut app).0
        }
        let reference = drive(60);
        for render_hz in [30, 120] {
            let pose = drive(render_hz);
            assert!(
                (pose - reference).length() < 10.0,
                "frame-rate divergence at {render_hz} fps: {pose:?} vs {reference:?}"
            );
        }
    }

    #[test]
    fn origin_rebase_keeps_body_camera_and_tankards_aligned() {
        use crate::streaming::RenderOrigin;
        use crate::world::components::CELL_SIZE;

        let mut app = headless::fixture_app();
        app.insert_resource(RenderOrigin(IVec2::ZERO));
        headless::place_player(&mut app, Vec3::new(120.0, 300.0, 120.0));
        for _ in 0..120 {
            app.update();
        }
        // Simulate render-origin rebase: every physics participant shifts
        // by the same cell offset, Rapier tracks Bevy transforms, velocity
        // preserved (V9).
        let shift = Vec3::new(CELL_SIZE, 0.0, CELL_SIZE);
        let before_velocity = {
            let mut query = app
                .world_mut()
                .query_filtered::<&Velocity, With<DebugTankard>>();
            query.iter(app.world()).next().cloned()
        };
        {
            let mut query = app.world_mut().query_filtered::<&mut Transform, Or<(
                With<PlayerBody>,
                With<DebugTankard>,
                With<StreamingCamera>,
                With<FixtureArena>,
            )>>();
            for mut transform in query.iter_mut(app.world_mut()) {
                transform.translation -= shift;
            }
        }
        app.world_mut().resource_mut::<RenderOrigin>().0 += IVec2::new(1, 1);
        for _ in 0..30 {
            app.update();
        }
        let (pose, grounded) = headless::player_pose(&mut app);
        assert!(grounded, "rebase ungrounded the capsule at {pose:?}");
        assert!(pose.is_finite());
        if let Some(before) = before_velocity {
            let after = {
                let mut query = app
                    .world_mut()
                    .query_filtered::<&Velocity, With<DebugTankard>>();
                query.iter(app.world()).next().cloned().unwrap_or_default()
            };
            assert!(
                (after.linear - before.linear).length() < 400.0,
                "rebase injected tankard velocity: {before:?} -> {after:?}"
            );
        }
    }
}

#[cfg(test)]
mod console_tests {
    use super::headless;
    use super::*;
    use crate::console::{ConsolePlugin, execute_line};
    use bevy::ecs::system::RunSystemOnce;

    fn console_fixture() -> App {
        let mut app = headless::fixture_app_with(|app| {
            // `ToggleCollision`/`tcl` comes in with `PlayerControlsPlugin` through the fixture.
            app.add_plugins(ConsolePlugin)
                .add_console_command(toggle_collision_geometry_command())
                .insert_resource(DebugRenderContext::default());
        });
        app.update();
        app
    }

    fn scrollback(app: &App) -> String {
        app.world().resource::<ConsoleState>().scrollback.join("\n")
    }

    fn press(app: &mut App, key: KeyCode) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
    }

    /// Send a key press the way winit does; `InputPlugin` turns it into `ButtonInput` on update.
    fn send_key(app: &mut App, key_code: KeyCode) {
        app.world_mut()
            .write_message(bevy::input::keyboard::KeyboardInput {
                key_code,
                logical_key: bevy::input::keyboard::Key::Character("x".into()),
                state: bevy::input::ButtonState::Pressed,
                text: None,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
    }

    fn mode(app: &App) -> MoveMode {
        *app.world().resource::<MoveMode>()
    }

    #[test]
    fn help_lists_every_physics_command() {
        let mut app = console_fixture();
        execute_line(app.world_mut(), "Help");
        let text = scrollback(&app);
        for line in [
            "Help [text] - ",
            "ClearConsole - ",
            "ToggleCollision [tcl] - toggle noclip flight and walking (key V)",
            "ToggleCollisionGeometry [tcg] - toggle the collision outlines (also in Graphics settings)",
        ] {
            assert!(text.contains(line), "Help is missing {line}: {text}");
        }
        // The filter finds a command by its short form and by its description.
        execute_line(app.world_mut(), "ClearConsole");
        execute_line(app.world_mut(), "help TCG");
        let text = scrollback(&app);
        assert!(text.contains("ToggleCollisionGeometry [tcg]"), "{text}");
        assert!(!text.contains("ToggleCollision [tcl]"), "{text}");
        execute_line(app.world_mut(), "ClearConsole");
        execute_line(app.world_mut(), "help outlines");
        let text = scrollback(&app);
        assert!(text.contains("ToggleCollisionGeometry [tcg]"), "{text}");
        assert!(!text.contains("ToggleCollision [tcl]"), "{text}");
    }

    #[test]
    fn tcl_twice_flips_and_flips_back() {
        let mut app = console_fixture();
        assert_eq!(mode(&app), MoveMode::Noclip);
        execute_line(app.world_mut(), "tcl");
        assert_eq!(mode(&app), MoveMode::Walk, "{}", scrollback(&app));
        execute_line(app.world_mut(), "TCL");
        assert_eq!(mode(&app), MoveMode::Noclip);
        execute_line(app.world_mut(), "togglecollision");
        assert_eq!(mode(&app), MoveMode::Walk);
    }

    #[test]
    fn crossing_blocks_both_mode_switch_entry_points_in_both_modes() {
        for initial in [MoveMode::Noclip, MoveMode::Walk] {
            let mut app = console_fixture();
            app.insert_resource(initial);
            app.insert_resource(CursorCapture::Captured);
            app.insert_resource(crate::door_crossing::DoorCrossing::holding_for_test());
            press(&mut app, KeyCode::KeyV);
            app.world_mut().run_system_once(toggle_mode_system).unwrap();
            assert_eq!(mode(&app), initial, "V must preserve mode during crossing");
            execute_line(app.world_mut(), "tcl");
            assert_eq!(
                mode(&app),
                initial,
                "tcl must preserve mode during crossing"
            );
            assert!(scrollback(&app).contains("door crossing in progress"));
            app.insert_resource(crate::door_crossing::DoorCrossing::default());
            execute_line(app.world_mut(), "tcl");
            assert_ne!(mode(&app), initial, "switching resumes after crossing");
        }
    }

    #[test]
    fn failed_crossing_recovery_uses_noclip_from_both_modes_and_entry_points() {
        for initial in [MoveMode::Noclip, MoveMode::Walk] {
            for keyed in [false, true] {
                let mut app = console_fixture();
                app.insert_resource(initial);
                app.insert_resource(CursorCapture::Captured);
                app.insert_resource(crate::door_crossing::DoorCrossing::recovery_for_test());
                app.world_mut().resource_mut::<WalkSpawnHold>().active = true;
                if keyed {
                    press(&mut app, KeyCode::KeyV);
                    app.world_mut().run_system_once(toggle_mode_system).unwrap();
                } else {
                    execute_line(app.world_mut(), "tcl");
                    assert!(scrollback(&app).contains("noclip ON"));
                }
                assert_eq!(mode(&app), MoveMode::Noclip);
                assert!(!app.world().resource::<WalkSpawnHold>().active);
                assert!(
                    !app.world()
                        .resource::<crate::door_crossing::DoorCrossing>()
                        .is_active()
                );
                let mut players = app
                    .world_mut()
                    .query_filtered::<&RigidBodyDisabled, With<PlayerBody>>();
                assert_eq!(players.iter(app.world()).count(), 1);
                assert!(
                    app.world_mut()
                        .run_system_once(crate::door_crossing::no_door_crossing)
                        .unwrap()
                );
            }
        }
    }

    #[test]
    fn tcl_without_a_player_does_not_repeat_an_old_block_reason() {
        let mut app = console_fixture();
        app.world_mut()
            .resource_mut::<WalkEntryStatus>()
            .blocked_reason = Some("no free capsule placement nearby".to_owned());
        let players: Vec<Entity> = app
            .world_mut()
            .query_filtered::<Entity, With<PlayerBody>>()
            .iter(app.world())
            .collect();
        assert!(!players.is_empty(), "the fixture spawns a player");
        for player in players {
            app.world_mut().despawn(player);
        }
        execute_line(app.world_mut(), "tcl");
        let text = scrollback(&app);
        assert!(text.contains("no player to move in this run"), "{text}");
        assert!(!text.contains("cannot walk here"), "{text}");
        assert_eq!(mode(&app), MoveMode::Noclip);
    }

    #[test]
    fn key_and_command_agree_on_move_mode() {
        let mut keyed = console_fixture();
        keyed.insert_resource(CursorCapture::Captured);
        press(&mut keyed, KeyCode::KeyV);
        keyed
            .world_mut()
            .run_system_once(toggle_mode_system)
            .unwrap();
        let mut commanded = console_fixture();
        // The removed `v` alias is unknown; `tcl` runs the key's effect.
        execute_line(commanded.world_mut(), "v");
        assert!(scrollback(&commanded).contains("unknown command \"v\""));
        assert_eq!(mode(&commanded), MoveMode::Noclip);
        execute_line(commanded.world_mut(), "tcl");
        assert_eq!(mode(&keyed), MoveMode::Walk);
        assert_eq!(mode(&keyed), mode(&commanded));
        execute_line(commanded.world_mut(), "ToggleCollision");
        assert_eq!(mode(&commanded), MoveMode::Noclip);
    }

    #[test]
    fn command_toggles_collision_outlines() {
        let mut commanded = console_fixture();
        let before = commanded.world().resource::<DebugRenderContext>().enabled;
        // F3 releases the mouse; Skyrim's `tcg` controls collision outlines.
        execute_line(commanded.world_mut(), "f3");
        assert!(scrollback(&commanded).contains("unknown command \"f3\""));
        assert_eq!(
            commanded.world().resource::<DebugRenderContext>().enabled,
            before
        );
        execute_line(commanded.world_mut(), "tcg");
        assert_ne!(
            commanded.world().resource::<DebugRenderContext>().enabled,
            before
        );
        execute_line(commanded.world_mut(), "ToggleCollisionGeometry");
        assert_eq!(
            commanded.world().resource::<DebugRenderContext>().enabled,
            before
        );
    }

    #[test]
    fn tankard_and_grab_have_no_console_command() {
        let mut app = console_fixture();
        for line in ["tankard", "grab", "t", "e"] {
            execute_line(app.world_mut(), line);
        }
        let text = scrollback(&app);
        for line in ["tankard", "grab", "t", "e"] {
            assert!(
                text.contains(&format!("unknown command \"{line}\"")),
                "unknown missing for {line}: {text}"
            );
            assert!(
                !text.contains(&format!("unknown command \"{line}\", did you mean")),
                "unexpected suggestion for {line}: {text}"
            );
        }
    }

    #[test]
    fn open_console_blocks_the_debug_keys_look_and_recapture() {
        let mut app = console_fixture();
        // Open the console the way a player does, from a captured cursor.
        app.insert_resource(CursorCapture::Captured);
        send_key(&mut app, KeyCode::Backquote);
        app.update();
        assert!(app.world().resource::<ConsoleState>().open);
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Released
        );
        let camera_before = {
            let mut query = app
                .world_mut()
                .query_filtered::<&Transform, With<ControlledCamera>>();
            *query.single(app.world()).unwrap()
        };
        // V, a movement key, a click and a mouse move while open.
        send_key(&mut app, KeyCode::KeyV);
        send_key(&mut app, KeyCode::KeyW);
        app.world_mut()
            .write_message(bevy::input::mouse::MouseButtonInput {
                button: MouseButton::Left,
                state: bevy::input::ButtonState::Pressed,
                window: Entity::PLACEHOLDER,
            });
        app.world_mut().write_message(MouseMotion {
            delta: Vec2::new(40.0, 10.0),
        });
        app.update();
        assert_eq!(mode(&app), MoveMode::Noclip);
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Released
        );
        assert_eq!(app.world().resource::<LookIntent>().yaw, 0.0);
        let mut query = app
            .world_mut()
            .query_filtered::<&Transform, With<ControlledCamera>>();
        assert_eq!(
            query.single(app.world()).unwrap().translation,
            camera_before.translation
        );
        // F3 is inert too.
        let before = app.world().resource::<DebugRenderContext>().enabled;
        send_key(&mut app, KeyCode::F3);
        app.update();
        assert_eq!(app.world().resource::<DebugRenderContext>().enabled, before);
    }

    /// Type a character into the console the way winit delivers it, then run one frame.
    fn type_text(app: &mut App, key_code: KeyCode, c: &str) {
        app.world_mut()
            .write_message(bevy::input::keyboard::KeyboardInput {
                key_code,
                logical_key: bevy::input::keyboard::Key::Character(c.into()),
                state: bevy::input::ButtonState::Pressed,
                text: Some(c.into()),
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
        app.update();
    }

    #[test]
    fn typed_tcl_runs_through_the_schedule() {
        // Drives the real path: the line is typed, Enter is pressed and the exclusive
        // dispatch system runs the handler, which runs the move-mode effect as a system.
        let mut app = console_fixture();
        app.insert_resource(CursorCapture::Captured);
        send_key(&mut app, KeyCode::Backquote);
        app.update();
        assert!(app.world().resource::<ConsoleState>().open);
        for (code, c) in [
            (KeyCode::KeyT, "t"),
            (KeyCode::KeyC, "c"),
            (KeyCode::KeyL, "l"),
        ] {
            type_text(&mut app, code, c);
        }
        assert_eq!(app.world().resource::<ConsoleState>().buffer, "tcl");
        assert_eq!(mode(&app), MoveMode::Noclip);
        app.world_mut()
            .write_message(bevy::input::keyboard::KeyboardInput {
                key_code: KeyCode::Enter,
                logical_key: bevy::input::keyboard::Key::Enter,
                state: bevy::input::ButtonState::Pressed,
                text: None,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
        app.update();
        app.update();
        assert_eq!(mode(&app), MoveMode::Walk, "{}", scrollback(&app));
        assert!(app.world().resource::<ConsoleState>().buffer.is_empty());
    }

    #[test]
    fn keys_still_work_through_the_schedule_with_the_console_closed() {
        let mut app = console_fixture();
        // The real schedule reads the capture state, so the cursor must be captured.
        app.insert_resource(CursorCapture::Captured);
        let before = app.world().resource::<DebugRenderContext>().enabled;
        send_key(&mut app, KeyCode::KeyV);
        app.update();
        assert_eq!(mode(&app), MoveMode::Walk, "{}", scrollback(&app));
        send_key(&mut app, KeyCode::F3);
        app.update();
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Released
        );
        assert_eq!(app.world().resource::<DebugRenderContext>().enabled, before);
    }

    fn send_state(app: &mut App, key_code: KeyCode, state: bevy::input::ButtonState) {
        app.world_mut()
            .write_message(bevy::input::keyboard::KeyboardInput {
                key_code,
                logical_key: bevy::input::keyboard::Key::Character("x".into()),
                state,
                text: None,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
        app.update();
    }

    #[test]
    fn keys_in_the_closing_frame_do_not_reach_the_game() {
        use bevy::input::ButtonState::{Pressed, Released};
        let mut app = console_fixture();
        app.insert_resource(CursorCapture::Captured);
        send_state(&mut app, KeyCode::Backquote, Pressed);
        send_state(&mut app, KeyCode::Backquote, Released);
        assert!(app.world().resource::<ConsoleState>().open);
        // V and the closing backtick land in the same update.
        send_key(&mut app, KeyCode::KeyV);
        send_key(&mut app, KeyCode::Backquote);
        // A selection click delivered with the closing key must not recapture the cursor.
        app.world_mut()
            .write_message(bevy::input::mouse::MouseButtonInput {
                button: MouseButton::Left,
                state: bevy::input::ButtonState::Pressed,
                window: Entity::PLACEHOLDER,
            });
        app.update();
        assert!(!app.world().resource::<ConsoleState>().open);
        // The capture that movement and look read stays released for the closing frame, so they
        // ignore that frame's keys and mouse motion (this checks the gate, not a moving body).
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Released
        );
        assert_eq!(mode(&app), MoveMode::Noclip, "{}", scrollback(&app));
        // One update later the capture is back and V works again.
        send_state(&mut app, KeyCode::KeyV, Released);
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Captured
        );
        send_state(&mut app, KeyCode::Backquote, Released);
        send_state(&mut app, KeyCode::KeyV, Pressed);
        assert_eq!(mode(&app), MoveMode::Walk);
    }

    #[test]
    fn bug_report_capture_keeps_input_blocked_after_console_dismissal() {
        use bevy::input::ButtonState::{Pressed, Released};
        let mut app = console_fixture();
        app.insert_resource(CursorCapture::Captured);
        send_state(&mut app, KeyCode::Backquote, Pressed);
        send_state(&mut app, KeyCode::Backquote, Released);
        app.world_mut()
            .resource_mut::<ConsoleState>()
            .capture_suppressed = true;
        send_state(&mut app, KeyCode::Backquote, Pressed);
        send_state(&mut app, KeyCode::Backquote, Released);
        assert!(!app.world().resource::<ConsoleState>().open);
        assert!(!app.world().resource::<Time<Virtual>>().is_paused());
        let camera_before = {
            let mut query = app
                .world_mut()
                .query_filtered::<&Transform, With<ControlledCamera>>();
            *query.single(app.world()).unwrap()
        };
        send_key(&mut app, KeyCode::KeyW);
        send_key(&mut app, KeyCode::KeyV);
        app.world_mut()
            .write_message(bevy::input::mouse::MouseButtonInput {
                button: MouseButton::Left,
                state: Pressed,
                window: Entity::PLACEHOLDER,
            });
        app.world_mut().write_message(MouseMotion {
            delta: Vec2::new(40.0, 10.0),
        });
        app.update();
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Released
        );
        assert_eq!(mode(&app), MoveMode::Noclip);
        let mut query = app
            .world_mut()
            .query_filtered::<&Transform, With<ControlledCamera>>();
        assert_eq!(*query.single(app.world()).unwrap(), camera_before);
        send_state(&mut app, KeyCode::KeyW, Released);
        send_state(&mut app, KeyCode::KeyV, Released);
        app.world_mut()
            .resource_mut::<ConsoleState>()
            .capture_suppressed = false;
        app.update();
        assert_eq!(
            *app.world().resource::<CursorCapture>(),
            CursorCapture::Captured
        );
        assert!(!app.world().resource::<ConsoleState>().open);
    }

    #[test]
    fn opening_clears_held_motion() {
        let mut app = console_fixture();
        app.insert_resource(WalkIntent {
            wish_dir: Vec3::X,
            target_speed: 300.0,
            jump_pressed: true,
        });
        send_key(&mut app, KeyCode::Backquote);
        app.update();
        let intent = *app.world().resource::<WalkIntent>();
        assert_eq!(intent.wish_dir, Vec3::ZERO);
        assert_eq!(intent.target_speed, 0.0);
    }
}

#[cfg(test)]
mod teleport_tests {
    use super::*;

    #[test]
    fn noclip_flight_pauses_during_a_door_crossing_and_resumes_afterward() {
        let mut app = headless::fixture_app();
        app.insert_resource(MoveMode::Noclip);
        app.insert_resource(CursorCapture::Captured);
        app.insert_resource(crate::door_crossing::DoorCrossing::holding_for_test());
        let camera_position = |app: &mut App| {
            app.world_mut()
                .query_filtered::<&Transform, With<StreamingCamera>>()
                .single(app.world())
                .unwrap()
                .translation
        };
        let start = camera_position(&mut app);
        app.world_mut()
            .write_message(bevy::input::keyboard::KeyboardInput {
                key_code: KeyCode::KeyW,
                logical_key: bevy::input::keyboard::Key::Character("w".into()),
                state: bevy::input::ButtonState::Pressed,
                text: None,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
        for _ in 0..360 {
            app.update();
        }
        assert_eq!(camera_position(&mut app), start);
        app.insert_resource(crate::door_crossing::DoorCrossing::default());
        for _ in 0..360 {
            app.update();
        }
        assert!((camera_position(&mut app) - start).length() > 4096.0);
    }

    /// A WALK body over nothing does not fall, or walk, while a door crossing holds the screen
    /// black, and falls again once the crossing is over.
    #[test]
    fn walking_pauses_during_a_door_crossing() {
        let mut app = headless::fixture_app();
        let start = Vec3::new(0.0, 5_000.0, 0.0);
        headless::place_player(&mut app, start);
        app.insert_resource(crate::door_crossing::DoorCrossing::holding_for_test());
        {
            let mut intent = app.world_mut().resource_mut::<WalkIntent>();
            intent.wish_dir = Vec3::NEG_Z;
            intent.target_speed = 300.0;
        }
        for _ in 0..30 {
            app.update();
        }
        let (held, _) = headless::player_pose(&mut app);
        assert!(
            (held - start).length() < 1.0,
            "moved during the crossing: {held}"
        );
        app.insert_resource(crate::door_crossing::DoorCrossing::default());
        for _ in 0..30 {
            app.update();
        }
        let (after, _) = headless::player_pose(&mut app);
        assert!(
            after.y < start.y - 10.0,
            "did not fall after the crossing: {after}"
        );
    }

    #[test]
    fn teleport_player_moves_the_body_and_zeroes_its_velocity() {
        let mut app = headless::fixture_app();
        headless::place_player(&mut app, Vec3::new(0.0, 500.0, 0.0));
        let mut query = app
            .world_mut()
            .query_filtered::<&mut WalkState, With<PlayerBody>>();
        {
            let mut state = query.single_mut(app.world_mut()).unwrap();
            integrate_walk(
                &mut state,
                &WalkIntent::default(),
                &MovementTuning::default(),
                1.0,
                ResolvedWalk {
                    y: 500.0,
                    grounded: false,
                },
            );
            state.horizontal_velocity = Vec3::new(300.0, 0.0, 0.0);
        }
        let target = Vec3::new(40_000.0, 900.0, -2_000.0);
        app.world_mut().write_message(TeleportPlayer {
            position: target,
            sampled_ground_y: None,
            yaw: 1.0,
        });
        app.update();
        let state = *query.single(app.world()).unwrap();
        assert_eq!(state.vertical_velocity(), 0.0);
        assert_eq!(state.horizontal_velocity, Vec3::ZERO);
        assert!(!state.grounded());
        let mut body = app
            .world_mut()
            .query_filtered::<&Transform, With<PlayerBody>>();
        let at = body.single(app.world()).unwrap().translation;
        // One physics tick of gravity may already have moved it; it is where it was sent.
        assert!((at.x - target.x).abs() < 1.0 && (at.z - target.z).abs() < 1.0);
        let tuning = MovementTuning::default();
        // The target is the feet: the body's origin is a capsule half-extent above them.
        assert!((at.y - (target.y + body_height_above_feet(&tuning))).abs() < 20.0);
        let look = app.world().resource::<LookIntent>();
        assert_eq!(look.yaw, 1.0);
    }

    #[test]
    fn teleport_puts_the_feet_at_the_target_and_the_camera_at_eye_height() {
        let tuning = MovementTuning::default();
        let feet = Vec3::new(10.0, 300.0, -20.0);
        let (body, camera) = body_and_camera_for_feet(&tuning, feet);
        // The capsule's lowest point is the feet.
        let lowest = body.y - tuning.capsule_half_height() - tuning.capsule_radius;
        assert!((lowest - feet.y).abs() < 1e-3);
        assert!((camera.y - body.y - tuning.eye_height).abs() < 1e-3);
        // Noclip moves only the camera, to the same place the walk follow would put it.
        let mut app = headless::fixture_app();
        app.insert_resource(MoveMode::Noclip);
        app.world_mut().write_message(TeleportPlayer {
            position: feet,
            sampled_ground_y: None,
            yaw: 0.0,
        });
        app.update();
        let mut view = app
            .world_mut()
            .query_filtered::<&Transform, With<StreamingCamera>>();
        let at = view.single(app.world()).unwrap().translation;
        assert!((at - camera).length() < 1e-3);
    }
}
