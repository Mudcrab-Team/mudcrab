//! Actor model catalog: `NPC_` -> race skin -> armature -> biped parts, plus a head path.
//!
//! The exporter publishes this catalog to the world database (`actor_models`,
//! `actor_model_parts`). The tables are additive and optional: the engine probes
//! for them and keeps working from the legacy base-object model tables when they
//! are absent. Nothing here reads raw `records.data`; the caller hands the merged
//! record map in and gets rows back.

use crate::esm::{extractors::SubrecordView, records::RawRecord};
use std::collections::{BTreeSet, HashMap};

/// `DefaultRace`, the armature race every humanoid body is shared through.
pub const DEFAULT_RACE_FORM_ID: u32 = 25;

/// `RACE/DATA` flag bit for a race whose heads come from FaceGen.
const RACE_FACEGEN_HEAD: u32 = 0x2;

/// `ARMA/BOD2` biped slot bits, the slot number minus 30.
const BIPED_BODY: u32 = 1 << 2;
const BIPED_HANDS: u32 = 1 << 3;
const BIPED_FEET: u32 = 1 << 7;

/// Directory the FaceGen geometry lives in.
const FACEGEN_DIR: &str = "meshes/actors/character/facegendata/facegeom/";
/// Directory the race/sex default head NIFs live in.
const DEFAULT_HEAD_DIR: &str = "meshes/actors/character/character assets/";

/// The biped slot a resolved armature fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActorModelSlot {
    /// The main body (`BOD2` bit 2).
    Body,
    /// The hands (`BOD2` bit 3).
    Hands,
    /// The feet (`BOD2` bit 7).
    Feet,
}

impl ActorModelSlot {
    /// The slot string stored in `actor_model_parts.slot`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Hands => "hands",
            Self::Feet => "feet",
        }
    }
}

/// Why a base has no complete model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnresolvedReason {
    /// The `NPC_` has no `ACBS`; sex defaults to male.
    MissingAcbs,
    /// The `NPC_` has no `RNAM` race link.
    NoRnam,
    /// The `RNAM` race has no record in the load order.
    NoRaceRecord,
    /// The race has no `WNAM` skin armor.
    RaceNoWnam,
    /// The `WNAM` armor has no record in the load order.
    NoArmoRecord,
    /// The armor has no `MODL` armature list.
    ArmoNoModl,
    /// No armature matched the race, exactly or through `DefaultRace`.
    NoMatchingArma,
    /// No head is available: the race has no FaceGen flag and no default head.
    NoFacegenAndNoDefault,
    /// An armature's `BOD2` has no biped slot this catalog stores.
    UnrecognizedSlot,
}

impl UnresolvedReason {
    /// The reason string used in diagnostics and the retail breakdown.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingAcbs => "missing_acbs",
            Self::NoRnam => "no_rnam",
            Self::NoRaceRecord => "no_race_record",
            Self::RaceNoWnam => "race_no_wnam",
            Self::NoArmoRecord => "no_armo_record",
            Self::ArmoNoModl => "armo_no_modl",
            Self::NoMatchingArma => "no_matching_arma",
            Self::NoFacegenAndNoDefault => "no_facegen_and_no_default",
            Self::UnrecognizedSlot => "unrecognized_slot",
        }
    }
}

/// One `actor_models` row: the body/head summary for a referenced `NPC_` base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorModelRow {
    /// The `NPC_` FormID.
    pub id: u32,
    /// The race FormID, or `0` when the base carries no `RNAM`.
    pub race_id: u32,
    /// `male` or `female`, from `NPC_/ACBS` bit 0.
    pub sex: &'static str,
    /// The `RACE/WNAM` skin armor FormID.
    pub skin_armo_id: Option<u32>,
    /// FaceGen geometry or the race/sex default head NIF.
    pub head_model: Option<String>,
    /// `facegen` or `default`.
    pub head_source: Option<&'static str>,
    /// `body`, `body_head` or `unresolved`.
    pub resolution: &'static str,
}

/// One `actor_model_parts` row: a biped slot's model path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorModelPartRow {
    /// The `NPC_` FormID the part belongs to.
    pub npc_id: u32,
    /// The biped slot.
    pub slot: ActorModelSlot,
    /// The `MOD2`/`MOD3` path, as the record wrote it.
    pub model_path: String,
}

/// One unresolved-or-notable base, with the reason it was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorModelDiagnostic {
    /// The `NPC_` FormID the diagnostic is about.
    pub npc_id: u32,
    /// The reason.
    pub reason: UnresolvedReason,
}

/// The full resolver output: rows, part rows and diagnostics.
#[derive(Debug, Default, Clone)]
pub struct ActorModelCatalog {
    /// One row per `NPC_` base, sorted by FormID.
    pub models: Vec<ActorModelRow>,
    /// Part rows, sorted by `(npc_id, slot)`.
    pub parts: Vec<ActorModelPartRow>,
    /// Diagnostics, sorted by `(npc_id, reason)`.
    pub diagnostics: Vec<ActorModelDiagnostic>,
}

/// Resolves every `NPC_` base in `records` into a body/head catalog.
///
/// `plugin_names` maps an `NPC_` FormID to its owning plugin filename
/// (`formid_map.plugin_name`), which the FaceGen path needs. A base with no
/// entry still resolves a body, but its head falls back to the default NIF.
#[must_use]
pub fn resolve_actor_models(
    records: &HashMap<u32, RawRecord>,
    plugin_names: &HashMap<u32, String>,
) -> ActorModelCatalog {
    let mut catalog = ActorModelCatalog::default();
    let mut npc_ids: Vec<u32> = records
        .iter()
        .filter(|(_, record)| &record.record_type == b"NPC_" && !record.is_deleted())
        .map(|(id, _)| *id)
        .collect();
    npc_ids.sort_unstable();

    for id in npc_ids {
        let record = &records[&id];
        let view = SubrecordView::new(&record.subrecords);
        let (female, acbs_missing) = npc_sex(&view);
        if acbs_missing {
            catalog.diagnostics.push(ActorModelDiagnostic {
                npc_id: id,
                reason: UnresolvedReason::MissingAcbs,
            });
        }
        let mut row = ActorModelRow {
            id,
            race_id: 0,
            sex: if female { "female" } else { "male" },
            skin_armo_id: None,
            head_model: None,
            head_source: None,
            resolution: "unresolved",
        };

        let Some(race_id) = view.get_form_id(b"RNAM") else {
            catalog.diagnostics.push(ActorModelDiagnostic {
                npc_id: id,
                reason: UnresolvedReason::NoRnam,
            });
            catalog.models.push(row);
            continue;
        };
        row.race_id = race_id;

        let Some(race) = records
            .get(&race_id)
            .filter(|race| &race.record_type == b"RACE" && !race.is_deleted())
        else {
            catalog.diagnostics.push(ActorModelDiagnostic {
                npc_id: id,
                reason: UnresolvedReason::NoRaceRecord,
            });
            catalog.models.push(row);
            continue;
        };
        let race_view = SubrecordView::new(&race.subrecords);

        let mut parts = resolve_body(records, &race_view, race_id, female, id, &mut catalog);
        let head = resolve_head(
            &race_view,
            female,
            plugin_names.get(&id).map(String::as_str),
            id,
            &parts,
        );
        if head.is_none() {
            catalog.diagnostics.push(ActorModelDiagnostic {
                npc_id: id,
                reason: UnresolvedReason::NoFacegenAndNoDefault,
            });
        }
        if let Some((path, source)) = head {
            row.head_model = Some(path);
            row.head_source = Some(source);
        }
        row.resolution = if parts.is_empty() {
            "unresolved"
        } else if row.head_model.is_some() {
            "body_head"
        } else {
            "body"
        };
        row.skin_armo_id = race_view.get_form_id(b"WNAM");
        catalog.models.push(row);
        catalog.parts.append(&mut parts);
    }

    catalog.models.sort_by_key(|row| row.id);
    catalog.parts.sort_by_key(|part| (part.npc_id, part.slot));
    catalog
        .diagnostics
        .sort_by_key(|diagnostic| (diagnostic.npc_id, diagnostic.reason));
    catalog
}

/// `NPC_/ACBS` bit 0 is female; a missing `ACBS` defaults to male.
fn npc_sex(view: &SubrecordView<'_>) -> (bool, bool) {
    match view.find(b"ACBS") {
        Some(data) if data.len() >= 4 => (
            u32::from_le_bytes([data[0], data[1], data[2], data[3]]) & 1 == 1,
            false,
        ),
        _ => (false, true),
    }
}

/// Follows `RACE/WNAM` -> `ARMO/MODL` -> each `ARMA`, and fills the biped slots.
fn resolve_body(
    records: &HashMap<u32, RawRecord>,
    race_view: &SubrecordView<'_>,
    race_id: u32,
    female: bool,
    npc_id: u32,
    catalog: &mut ActorModelCatalog,
) -> Vec<ActorModelPartRow> {
    let Some(skin_id) = race_view.get_form_id(b"WNAM") else {
        catalog.diagnostics.push(ActorModelDiagnostic {
            npc_id,
            reason: UnresolvedReason::RaceNoWnam,
        });
        return Vec::new();
    };
    let Some(armor) = records
        .get(&skin_id)
        .filter(|armor| &armor.record_type == b"ARMO" && !armor.is_deleted())
    else {
        catalog.diagnostics.push(ActorModelDiagnostic {
            npc_id,
            reason: UnresolvedReason::NoArmoRecord,
        });
        return Vec::new();
    };
    let armatures = SubrecordView::new(&armor.subrecords).get_form_ids(b"MODL");
    if armatures.is_empty() {
        catalog.diagnostics.push(ActorModelDiagnostic {
            npc_id,
            reason: UnresolvedReason::ArmoNoModl,
        });
        return Vec::new();
    }

    // Exact-race armatures take priority over `DefaultRace`, whatever order the
    // armor lists them in, so the shared humanoid body never shadows a race's own.
    let mut exact = Vec::new();
    let mut default = Vec::new();
    for armature_id in armatures {
        let Some(armature) = records
            .get(&armature_id)
            .filter(|armature| &armature.record_type == b"ARMA" && !armature.is_deleted())
        else {
            continue;
        };
        match SubrecordView::new(&armature.subrecords).get_form_id(b"RNAM") {
            Some(id) if id == race_id => exact.push(armature_id),
            Some(DEFAULT_RACE_FORM_ID) => default.push(armature_id),
            _ => {}
        }
    }

    let mut parts: Vec<ActorModelPartRow> = Vec::new();
    let mut seen_paths: BTreeSet<String> = BTreeSet::new();
    for armature_id in exact.into_iter().chain(default) {
        let armature = &records[&armature_id];
        let view = SubrecordView::new(&armature.subrecords);
        let slot = match classify_slot(view.find(b"BOD2").or_else(|| view.find(b"BODT"))) {
            Some(slot) => slot,
            None => {
                catalog.diagnostics.push(ActorModelDiagnostic {
                    npc_id,
                    reason: UnresolvedReason::UnrecognizedSlot,
                });
                continue;
            }
        };
        let tag = if female { b"MOD3" } else { b"MOD2" };
        let Some(path) = view.get_string(tag).filter(|path| !path.is_empty()) else {
            continue;
        };
        if parts.iter().any(|part| part.slot == slot) || !seen_paths.insert(path.to_lowercase()) {
            continue;
        }
        parts.push(ActorModelPartRow {
            npc_id,
            slot,
            model_path: path,
        });
    }

    if parts.is_empty() {
        catalog.diagnostics.push(ActorModelDiagnostic {
            npc_id,
            reason: UnresolvedReason::NoMatchingArma,
        });
    }
    parts
}

/// The first stored biped slot in `BOD2`, or in retail `ARMA`'s `BODT`, in body, hands, feet order.
fn classify_slot(bod: Option<&[u8]>) -> Option<ActorModelSlot> {
    let data = bod.filter(|data| data.len() >= 4)?;
    let slots = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if slots & BIPED_BODY != 0 {
        Some(ActorModelSlot::Body)
    } else if slots & BIPED_HANDS != 0 {
        Some(ActorModelSlot::Hands)
    } else if slots & BIPED_FEET != 0 {
        Some(ActorModelSlot::Feet)
    } else {
        None
    }
}

/// FaceGen geometry when the race uses it, else the race/sex default head.
fn resolve_head(
    race_view: &SubrecordView<'_>,
    female: bool,
    plugin: Option<&str>,
    npc_id: u32,
    parts: &[ActorModelPartRow],
) -> Option<(String, &'static str)> {
    if race_uses_facegen(race_view)
        && let Some(plugin) = plugin
    {
        return Some((
            format!("{FACEGEN_DIR}{}/{npc_id:08x}.nif", plugin.to_lowercase()),
            "facegen",
        ));
    }
    if !is_humanoid(parts) {
        return None;
    }
    let variant = match race_view
        .get_string(b"EDID")
        .unwrap_or_default()
        .to_lowercase()
    {
        name if name.contains("argonian") => "argonian",
        name if name.contains("khajiit") => "khajiit",
        _ => "",
    };
    let sex = if female { "female" } else { "male" };
    Some((
        format!("{DEFAULT_HEAD_DIR}{sex}head{variant}.nif"),
        "default",
    ))
}

/// Whether the race's `DATA` marks it as using FaceGen heads.
fn race_uses_facegen(race_view: &SubrecordView<'_>) -> bool {
    race_view
        .find(b"DATA")
        .filter(|data| data.len() >= 36)
        .is_some_and(|data| {
            u32::from_le_bytes([data[32], data[33], data[34], data[35]]) & RACE_FACEGEN_HEAD != 0
        })
}

/// A default head only fits the shared humanoid body, never a creature body.
fn is_humanoid(parts: &[ActorModelPartRow]) -> bool {
    parts.iter().any(|part| {
        let path = part.model_path.to_lowercase();
        path.contains("malebody") || path.contains("femalebody")
    })
}
