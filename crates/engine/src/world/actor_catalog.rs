//! Read-only view over the converter's actor model catalog (db-schema.md §18).
//!
//! `actor_models` holds one row per referenced `NPC_` base; `actor_model_parts` holds one row per
//! biped slot the race skin resolves. Both tables are optional: a database converted before them,
//! or one with a table of another shape, gives an empty catalog and the engine keeps spawning
//! references exactly as before.
//!
//! The catalog also owns the converted-assets root (the world database's directory), because the
//! composed body is an asset file rather than a database row: the spawn plan prefers
//! `meshes/actors/character/composed/default/{sex}/body.glb` when that file exists and falls back
//! to the catalog's authored `body` part otherwise.

use crate::actor_skin_tone::ActorSkinTone;
use crate::actors::{
    Actor, ActorHead, ActorHeadSource, ActorOutfit, ActorPart, ActorParts, ActorResolution,
    ActorSex, ActorSlot, ActorSpawnDiagnostic, ActorSpawnSkip, composed_body_scene,
};
use bevy::prelude::*;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One `actor_models` row plus its `actor_model_parts` rows.
#[derive(Debug, Clone, PartialEq)]
pub struct ActorModel {
    pub base_form_id: u32,
    pub race_id: u32,
    pub sex: ActorSex,
    pub skin_armo_id: Option<u32>,
    pub head_model: Option<String>,
    pub head_source: Option<ActorHeadSource>,
    pub resolution: ActorResolution,
    pub parts: Vec<ActorPart>,
    /// `NPC_/QNAM` (`skin_tone_r/g/b`), absent from databases converted before those columns.
    pub skin_tone: Option<ActorSkinTone>,
    /// `NPC_/NAM7` (`weight`), the 0..100 body weight; absent from older databases.
    pub weight: Option<f32>,
}

impl ActorModel {
    pub fn part(&self, slot: ActorSlot) -> Option<&ActorPart> {
        self.parts.iter().find(|part| part.slot == slot)
    }

    /// Whether this row's parts are the shared humanoid set the default composed body is built
    /// from. A row with no body slot, or a race whose own armature points elsewhere, is not
    /// compatible: substituting the default body would replace its authored model.
    fn uses_shared_humanoid_body(&self) -> bool {
        self.part(ActorSlot::Body).is_some()
            && self.parts.iter().all(|part| {
                normalized_part_path(&part.model_path)
                    == normalized_part_path(&shared_humanoid_part(self.sex, part.slot))
            })
    }
}

/// The source NIF the converter composes into the shared humanoid body for `sex` and `slot`.
fn shared_humanoid_part(sex: ActorSex, slot: ActorSlot) -> String {
    format!(
        "meshes/actors/character/character assets/{}{}_1.nif",
        sex.as_str(),
        match slot {
            ActorSlot::Body => "body",
            ActorSlot::Hands => "hands",
            ActorSlot::Feet => "feet",
        }
    )
}

/// Normalizes separators, case, and the optional `meshes/` prefix for comparison.
fn normalized_part_path(path: &str) -> String {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized
        .strip_prefix("meshes/")
        .unwrap_or(&normalized)
        .to_owned()
}

/// Everything the spawn path needs for one actor: the components and the scene it draws.
#[derive(Debug, Clone, PartialEq)]
pub struct ActorSpawnPlan {
    pub actor: Actor,
    pub parts: ActorParts,
    pub head: Option<ActorHead>,
    pub skin_tone: Option<ActorSkinTone>,
    /// `NPC_/NAM7`, 0..100; `None` when the catalog has no weight and the body stays heavy.
    pub weight: Option<f32>,
    /// The composed body when it exists, otherwise the catalog's `body` part, converted.
    pub scene_path: String,
    /// The worn default outfit. Only an actor drawn with the shared composed humanoid body has
    /// one: its parts are skinned to that skeleton and hide its body regions by name.
    pub outfit: Option<ActorOutfit>,
}

impl ActorSpawnPlan {
    /// Whether [`Self::scene_path`] is the shared composed humanoid body rather than a catalog
    /// body part. Only such an actor can be driven by the shared gait clips.
    pub fn uses_composed_body(&self) -> bool {
        self.scene_path == composed_body_scene(self.actor.sex)
    }
}

/// A creature family whose retail idle layers are verified, with the clips those layers re-pick.
///
/// Only these families get an idle sequence. For every other creature the retail clip set is not
/// verified, so it keeps looping its default catalog clip, as before. A name filter was tried and
/// dropped: it let scene-only clips through (a goblin praying, a lich holding a staff) and missed
/// verified ones.
#[derive(Debug, Clone, Copy)]
pub struct VerifiedIdleLayers {
    /// The binding's default clip (the base loop). A binding qualifies only when its default clip
    /// is this one, which keeps wolves out: they share the canine folder but use `mt_idle_wolf`.
    pub base_source: &'static str,
    /// The clips the layers re-pick, besides the base. A clip a binding lacks is skipped.
    pub clips: &'static [&'static str],
    /// The registry rows that verify this family.
    pub rows: &'static str,
}

/// The creature idle layers that retail data verifies. Part of a stand-in; see
/// [`crate::actor_animation::IdleSequence`].
///
/// - Dog (`canine/behaviors/noncombatidle.hkx`, RF-IDLE-018): the head layer picks among bark,
///   head 1 to 4 and the base; the tail layer among tail 1 to 3 and the base.
/// - Cow (`cow/behaviors/noncombatidle.hkx`, RF-IDLE-019): the tail wag picks among three clips.
///   The look, call, feed, forage and lie-down clips come from the engine's `IDLE` tree
///   (RF-IDLE-020) and are not listed.
pub const VERIFIED_IDLE_LAYERS: [VerifiedIdleLayers; 2] = [
    VerifiedIdleLayers {
        base_source: "meshes/actors/canine/animations/mt_idle.hkx",
        clips: &[
            "meshes/actors/canine/animations/dog_spice_bark.hkx",
            "meshes/actors/canine/animations/dog_idle_spice_head1.hkx",
            "meshes/actors/canine/animations/dog_idle_spice_head2.hkx",
            "meshes/actors/canine/animations/dog_spice_head3.hkx",
            "meshes/actors/canine/animations/dog_spice_head4.hkx",
            "meshes/actors/canine/animations/dog_idle_spice_tail1.hkx",
            "meshes/actors/canine/animations/dog_idle_spice_tail2.hkx",
            "meshes/actors/canine/animations/dog_idle_spice_tail3.hkx",
        ],
        rows: "RF-IDLE-018",
    },
    VerifiedIdleLayers {
        base_source: "meshes/actors/cow/animations/mt_idle.hkx",
        clips: &[
            "meshes/actors/cow/animations/idle_tailwag.hkx",
            "meshes/actors/cow/animations/idle_tailwag1.hkx",
            "meshes/actors/cow/animations/idle_tailwag2.hkx",
        ],
        rows: "RF-IDLE-019",
    },
];

/// One clip of a creature's idle pool: the catalog's source key and the converted scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdlePoolClip {
    pub source: String,
    pub scene: String,
}

/// The animation set [`ActorCatalog::insert_actor_components`] gave an actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorAnimationSetup {
    /// The shared composed body: gait clips and an `ActorDrive`.
    Gait,
    /// A placed corpse: no idle.
    Corpse,
    /// A creature with an authored idle from the idle catalog (it may vary it).
    AuthoredIdle,
    /// A creature with the legacy default idle clip.
    CreatureIdle,
    /// No supported gait or idle for this body.
    None,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct ActorCatalog {
    models: HashMap<u32, ActorModel>,
    assets_root: Option<PathBuf>,
    idle_catalog: shared::actor_idle_catalog::ActorIdleCatalog,
    outfits: shared::actor_outfits::ActorOutfitCatalog,
}

impl ActorCatalog {
    /// Reads both tables. A missing or differently shaped table is not an error: the catalog is
    /// optional, so the engine behaves exactly as it did before it existed.
    pub fn open(path: &Path) -> Self {
        let connection = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(connection) => connection,
            Err(error) => {
                info!("actors: cannot open the world database ({error}); no actors spawn");
                return Self::default();
            }
        };
        let assets_root = path.parent().unwrap_or_else(|| Path::new(""));
        Self::from_connection_with_assets_root(&connection, assets_root)
    }

    /// Reads both tables without an assets root, so composed-body existence is always false and
    /// every spawn plan falls back to the catalog's body part.
    pub fn from_connection(connection: &Connection) -> Self {
        Self::read(connection, None)
    }

    /// Reads both tables with the converted-assets root used to test for the composed body file.
    pub fn from_connection_with_assets_root(
        connection: &Connection,
        assets_root: impl Into<PathBuf>,
    ) -> Self {
        Self::read(connection, Some(assets_root.into()))
    }

    fn read(connection: &Connection, assets_root: Option<PathBuf>) -> Self {
        let mut idle_catalog = assets_root
            .as_ref()
            .and_then(|root| {
                let path = root.join(shared::actor_idle_catalog::PATH);
                let bytes = match std::fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
                    Err(error) => {
                        warn!("actors: cannot read idle catalog: {error}");
                        return None;
                    }
                };
                let parsed =
                    serde_json::from_slice::<shared::actor_idle_catalog::ActorIdleCatalog>(&bytes);
                match parsed {
                    Ok(catalog) => match catalog.validate() {
                        Ok(()) => Some(catalog),
                        Err(error) => {
                            warn!("actors: invalid idle catalog: {error}");
                            None
                        }
                    },
                    Err(error) => {
                        warn!("actors: invalid idle catalog: {error}");
                        None
                    }
                }
            })
            .unwrap_or_default();
        let outfits = assets_root
            .as_deref()
            .map(Self::read_outfits)
            .unwrap_or_default();
        let mut race_sources = HashMap::new();
        if let Ok(mut statement) = connection
            .prepare("SELECT race_id,sex,skeleton_nif,project_hkx FROM race_animation_sources")
            && let Ok(rows) = statement.query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
        {
            for (race, sex, skeleton, project) in rows.flatten() {
                race_sources.insert((race, sex), (skeleton, project));
            }
        }
        idle_catalog.bindings.retain(|row| {
            let valid = race_sources.get(&(row.race_id,row.sex.clone()))
                .is_some_and(|(skeleton,project)| skeleton == &row.race_skeleton_nif && project == &row.project);
            if !valid { warn!(race_id=row.race_id, body=%row.body_scene, "actors: idle catalog disagrees with current race authoring; binding omitted"); }
            valid
        });
        let mut models = HashMap::new();
        // The skin tone and weight columns arrived after the table, without a schema bump: a
        // database converted before them is read without those values.
        let has_weight = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('actor_models') WHERE name='weight')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false);
        let weight_column = if has_weight {
            "weight"
        } else {
            "NULL AS weight"
        };
        let toned = format!(
            "SELECT id, race_id, sex, skin_armo_id, head_model, head_source, resolution, \
             skin_tone_r, skin_tone_g, skin_tone_b, {weight_column} FROM actor_models"
        );
        let untoned = format!(
            "SELECT id, race_id, sex, skin_armo_id, head_model, head_source, resolution, \
             NULL, NULL, NULL, {weight_column} FROM actor_models"
        );
        match connection
            .prepare(&toned)
            .or_else(|_| connection.prepare(&untoned))
        {
            Ok(mut statement) => match statement.query_map([], |row| {
                let base_form_id: u32 = row.get(0)?;
                let race_id: u32 = row.get(1)?;
                let sex: String = row.get(2)?;
                let skin_armo_id: Option<u32> = row.get(3)?;
                let head_model: Option<String> = row.get(4)?;
                let head_source: Option<String> = row.get(5)?;
                let resolution: String = row.get(6)?;
                let skin_tone = match (
                    row.get::<_, Option<f64>>(7)?,
                    row.get::<_, Option<f64>>(8)?,
                    row.get::<_, Option<f64>>(9)?,
                ) {
                    (Some(red), Some(green), Some(blue)) => {
                        ActorSkinTone::new([red as f32, green as f32, blue as f32])
                    }
                    _ => None,
                };
                let weight = row
                    .get::<_, Option<f64>>(10)?
                    .map(|weight| weight as f32)
                    .filter(|weight| weight.is_finite() && (0.0..=100.0).contains(weight));
                let (Some(sex), Some(resolution)) =
                    (ActorSex::parse(&sex), ActorResolution::parse(&resolution))
                else {
                    debug!(
                        "actors: skipping NPC_ {base_form_id:08X} with unknown sex or resolution"
                    );
                    return Ok(None);
                };
                Ok(Some(ActorModel {
                    base_form_id,
                    race_id,
                    sex,
                    skin_armo_id,
                    head_model: head_model.filter(|path| !path.trim().is_empty()),
                    head_source: head_source.as_deref().and_then(ActorHeadSource::parse),
                    resolution,
                    parts: Vec::new(),
                    skin_tone,
                    weight,
                }))
            }) {
                Ok(rows) => {
                    for model in rows.flatten().flatten() {
                        models.insert(model.base_form_id, model);
                    }
                }
                Err(error) => {
                    info!("actors: no usable `actor_models` table ({error}); no actors spawn");
                }
            },
            Err(error) => {
                info!("actors: no usable `actor_models` table ({error}); no actors spawn");
            }
        }

        match connection.prepare("SELECT npc_id, slot, model_path FROM actor_model_parts") {
            Ok(mut statement) => match statement.query_map([], |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            }) {
                Ok(rows) => {
                    for (base_form_id, slot, model_path) in rows.flatten() {
                        let Some(slot) = ActorSlot::parse(&slot) else {
                            debug!(
                                "actors: skipping unknown part slot `{slot}` for NPC_ {base_form_id:08X}"
                            );
                            continue;
                        };
                        let Some(model) = models.get_mut(&base_form_id) else {
                            continue;
                        };
                        if let Some(part) = ActorPart::new(slot, model_path) {
                            model.parts.push(part);
                        }
                    }
                }
                Err(error) => {
                    info!("actors: no usable `actor_model_parts` table ({error}); no actors spawn");
                    return Self {
                        models: HashMap::new(),
                        assets_root,
                        idle_catalog,
                        outfits,
                    };
                }
            },
            Err(error) => {
                info!("actors: no usable `actor_model_parts` table ({error}); no actors spawn");
                return Self {
                    models: HashMap::new(),
                    assets_root,
                    idle_catalog,
                    outfits,
                };
            }
        }

        for model in models.values_mut() {
            model.parts.sort_by_key(|part| part.slot.index());
        }

        Self {
            models,
            assets_root,
            idle_catalog,
            outfits,
        }
    }

    /// The optional worn-outfit sidecar. A pack without it, or with an unreadable or invalid one,
    /// spawns actors in their skin exactly as before.
    fn read_outfits(root: &Path) -> shared::actor_outfits::ActorOutfitCatalog {
        let path = root.join(shared::actor_outfits::PATH);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Default::default();
            }
            Err(error) => {
                warn!("actors: cannot read outfit catalog: {error}");
                return Default::default();
            }
        };
        match serde_json::from_slice::<shared::actor_outfits::ActorOutfitCatalog>(&bytes) {
            Ok(catalog) => match catalog.validate() {
                Ok(()) => catalog,
                Err(error) => {
                    warn!("actors: invalid outfit catalog: {error}");
                    Default::default()
                }
            },
            Err(error) => {
                warn!("actors: invalid outfit catalog: {error}");
                Default::default()
            }
        }
    }

    /// Full race, sex, and converted-body identity must agree with the producer's binding.
    pub fn idle_scene(&self, plan: &ActorSpawnPlan) -> Option<&str> {
        let row = self.idle_catalog.find(
            plan.actor.race_id,
            plan.actor.sex.as_str(),
            &plan.scene_path,
        )?;
        self.assets_root
            .as_ref()?
            .join(&row.scene)
            .is_file()
            .then_some(row.scene.as_str())
    }

    /// The explicit cow/chicken contract for packs without a generic idle catalog binding.
    /// Like [`Self::idle_scene`], it requires the published clip, so an older pack stays silent.
    pub fn legacy_creature_idle_scene(&self, plan: &ActorSpawnPlan) -> Option<&'static str> {
        let profile = shared::creature_idle::for_body_scene(&plan.scene_path)?;
        self.assets_root
            .as_ref()?
            .join(profile.clip_scene)
            .is_file()
            .then_some(profile.clip_scene)
    }

    /// Resolve an authored idle source only inside this exact race/sex/body library.
    pub fn authored_idle_scene(
        &self,
        actor: &Actor,
        body_scene: &str,
        source: &str,
    ) -> Option<&str> {
        let row = self
            .idle_catalog
            .find(actor.race_id, actor.sex.as_str(), body_scene)?;
        let scene = if row.source == source {
            &row.scene
        } else {
            &row.clips.get(source)?.scene
        };
        self.assets_root
            .as_ref()?
            .join(scene)
            .is_file()
            .then_some(scene.as_str())
    }

    /// The clips one creature may play in turn, the default clip first.
    ///
    /// Only a binding whose default clip starts a [`VERIFIED_IDLE_LAYERS`] family gets more than
    /// the default: it then adds that family's clips that the binding has, with at least two
    /// frames and a file on disk, in the order of the list. An empty list means the actor has no
    /// catalog binding. A single entry means nothing to vary, so the caller keeps the looping
    /// default clip. Part of the stand-in described on [`crate::actor_animation::IdleSequence`].
    pub fn idle_sequence_pool(&self, plan: &ActorSpawnPlan) -> Vec<IdlePoolClip> {
        let Some(row) = self.idle_catalog.find(
            plan.actor.race_id,
            plan.actor.sex.as_str(),
            &plan.scene_path,
        ) else {
            return Vec::new();
        };
        let Some(root) = self.assets_root.as_ref() else {
            return Vec::new();
        };
        if !root.join(&row.scene).is_file() {
            return Vec::new();
        }
        let mut pool = vec![IdlePoolClip {
            source: row.source.clone(),
            scene: row.scene.clone(),
        }];
        let Some(layers) = VERIFIED_IDLE_LAYERS
            .iter()
            .find(|layers| layers.base_source == row.source)
        else {
            return pool;
        };
        for source in layers.clips {
            if let Some(clip) = row.clips.get(*source)
                && clip.frame_count >= 2
                && root.join(&clip.scene).is_file()
            {
                pool.push(IdlePoolClip {
                    source: (*source).to_owned(),
                    scene: clip.scene.clone(),
                });
            }
        }
        pool
    }

    /// Puts every component a spawned actor needs on `entity`: the plan's parts, head, outfit,
    /// skin tone and body weight, and the animation set that fits the body.
    ///
    /// This is the one place that decides it. The streamed world calls it for each placed
    /// reference and the `npc_viewer` example calls it for each NPC it shows, so what the viewer
    /// shows is what the game draws. `reference_form_id` is the placed reference's FormID; it seeds
    /// the creature idle sequence. A corpse (`starts_dead`) gets no idle.
    pub fn insert_actor_components(
        &self,
        entity: &mut EntityCommands,
        plan: &ActorSpawnPlan,
        asset_server: &AssetServer,
        reference_form_id: u32,
        starts_dead: bool,
    ) -> ActorAnimationSetup {
        entity.insert(plan.actor);
        if !plan.parts.is_empty() {
            entity.insert(plan.parts.clone());
        }
        if let Some(head) = &plan.head {
            entity.insert(head.clone());
        }
        if let Some(outfit) = &plan.outfit {
            entity.insert(outfit.clone());
        }
        if let Some(tone) = plan.skin_tone {
            entity.insert(tone);
        }
        if let Some(weight) = plan.weight {
            entity.insert(crate::actor_body_weight::ActorBodyWeight(
                (1.0 - weight / 100.0).clamp(0.0, 1.0),
            ));
        }
        if plan.uses_composed_body() {
            entity.insert((
                crate::actor_animation::ActorClips::load(asset_server, plan.actor.sex),
                crate::actor_drive::ActorDrive::default(),
            ));
            ActorAnimationSetup::Gait
        } else if starts_dead {
            // No death pose or ragdoll exists yet. A placed corpse keeps its bind pose; a standing
            // idle loop would make it look alive.
            debug!(
                reference_form_id = format_args!("{reference_form_id:08X}"),
                base_form_id = format_args!("{:08X}", plan.actor.base_form_id),
                body_model = %plan.scene_path,
                reason = "starts_dead",
                "actor spawned without idle playback"
            );
            ActorAnimationSetup::Corpse
        } else if let Some(scene) = self.idle_scene(plan) {
            entity.insert((
                crate::actor_animation::ActorClips::load_idle(asset_server, scene),
                crate::actor_animation::ActorIdleLibrary {
                    body_scene: plan.scene_path.clone(),
                },
            ));
            // Stand-in for the retail behaviour graph's idle variety; see `IdleSequence`.
            if let Some(sequence) = crate::actor_animation::IdleSequence::new(
                self.idle_sequence_pool(plan),
                reference_form_id,
            ) {
                entity.insert(sequence.preload(asset_server));
            }
            ActorAnimationSetup::AuthoredIdle
        } else if let Some(scene) = self.legacy_creature_idle_scene(plan) {
            entity.insert(crate::actor_animation::ActorClips::load_idle(
                asset_server,
                scene,
            ));
            ActorAnimationSetup::CreatureIdle
        } else {
            debug!(
                reference_form_id = format_args!("{reference_form_id:08X}"),
                base_form_id = format_args!("{:08X}", plan.actor.base_form_id),
                race_id = format_args!("{:08X}", plan.actor.race_id),
                sex = plan.actor.sex.as_str(),
                body_model = %plan.scene_path,
                reason = "no_supported_gait_mapping",
                "actor spawned without gait setup"
            );
            ActorAnimationSetup::None
        }
    }

    /// The worn-outfit row for a base `NPC_` FormID, when the pack's `actor-outfits.json` has one.
    /// A row can exist for an actor that does not wear it: only the shared composed body does.
    pub fn outfit_for(&self, base_form_id: u32) -> Option<&shared::actor_outfits::ActorOutfit> {
        self.outfits
            .outfits
            .iter()
            .find(|row| row.npc_id == base_form_id)
    }

    /// How many worn-outfit rows the pack's `actor-outfits.json` gave. Zero means the file is
    /// missing or was rejected (the reason is logged when the catalog opens).
    pub fn outfit_count(&self) -> usize {
        self.outfits.outfits.len()
    }

    /// The catalog row for a base `NPC_` FormID, if this world has one.
    pub fn get(&self, base_form_id: u32) -> Option<&ActorModel> {
        self.models.get(&base_form_id)
    }

    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// The composed body scene for `sex`, only when the converted asset exists.
    pub fn composed_body_scene(&self, sex: ActorSex) -> Option<&'static str> {
        let path = composed_body_scene(sex);
        self.asset_exists(path).then_some(path)
    }

    pub fn asset_exists(&self, relative: &str) -> bool {
        self.assets_root
            .as_ref()
            .is_some_and(|root| root.join(relative).is_file())
    }

    /// Turns a catalog row into the components and scene path a placed reference spawns.
    ///
    /// The composed body wins when its file exists and the row's parts are the shared humanoid
    /// set; otherwise the catalog's `body` part is converted. A row the catalog recorded as
    /// unresolved, or one with no resolvable body at all, yields one typed diagnostic and no
    /// plan.
    pub fn spawn_plan(&self, model: &ActorModel) -> Result<ActorSpawnPlan, ActorSpawnDiagnostic> {
        if !model.resolution.is_resolved() {
            return Err(ActorSpawnDiagnostic {
                base_form_id: model.base_form_id,
                reason: ActorSpawnSkip::Unresolved,
            });
        }
        let scene_path = model
            .uses_shared_humanoid_body()
            .then(|| self.composed_body_scene(model.sex))
            .flatten()
            .map(str::to_owned)
            .or_else(|| {
                model.part(ActorSlot::Body).and_then(|part| {
                    let canonical =
                        shared::actor_idle_catalog::canonical_body_path(&part.model_path, "glb")
                            .ok()?;
                    // Preserve the existing sky/marker/effect exclusions after normalization.
                    crate::streaming::converted_model_path(canonical)
                })
            })
            .ok_or(ActorSpawnDiagnostic {
                base_form_id: model.base_form_id,
                reason: ActorSpawnSkip::NoBody,
            })?;
        let head = model
            .head_model
            .as_deref()
            .zip(model.head_source)
            .and_then(|(path, source)| ActorHead::new(path, source));
        let mut plan = ActorSpawnPlan {
            actor: Actor::new(
                model.base_form_id,
                model.race_id,
                model.sex,
                model.resolution,
            ),
            parts: ActorParts::new(model.parts.iter().cloned()),
            head,
            skin_tone: model.skin_tone,
            weight: model.weight,
            scene_path,
            outfit: None,
        };
        if plan.uses_composed_body() {
            plan.outfit = self
                .outfits
                .find(model.base_form_id)
                .map(|outfit| ActorOutfit(outfit.parts.clone()));
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    /// The documented DDL, verbatim (db-schema.md §18).
    const DDL: &str = "
        CREATE TABLE IF NOT EXISTS actor_models (
            id INTEGER PRIMARY KEY,
            race_id INTEGER NOT NULL,
            sex TEXT NOT NULL,
            skin_armo_id INTEGER,
            head_model TEXT,
            head_source TEXT,
            resolution TEXT NOT NULL,
            skin_tone_r REAL,
            skin_tone_g REAL,
            skin_tone_b REAL,
            weight REAL
        );
        CREATE TABLE IF NOT EXISTS actor_model_parts (
            npc_id INTEGER NOT NULL,
            slot TEXT NOT NULL,
            model_path TEXT NOT NULL,
            PRIMARY KEY (npc_id, slot)
        );";

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(DDL).unwrap();
        connection
    }

    fn insert_actor(
        connection: &Connection,
        id: u32,
        race: u32,
        sex: &str,
        resolution: &str,
        head_model: Option<&str>,
        head_source: Option<&str>,
    ) {
        connection
            .execute(
                "INSERT INTO actor_models(id, race_id, sex, head_model, head_source, resolution) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, race, sex, head_model, head_source, resolution],
            )
            .unwrap();
    }

    fn insert_part(connection: &Connection, npc: u32, slot: &str, path: &str) {
        connection
            .execute(
                "INSERT INTO actor_model_parts(npc_id, slot, model_path) VALUES (?1, ?2, ?3)",
                params![npc, slot, path],
            )
            .unwrap();
    }

    #[test]
    fn idle_catalog_requires_current_typed_race_identity_and_complete_body_key() {
        use shared::actor_idle_catalog::{ActorIdleBinding, ActorIdleCatalog, PATH, VERSION};
        let connection = connection();
        insert_actor(&connection, 77, 13, "female", "body", None, None);
        insert_part(&connection, 77, "body", "Actors/Custom/body.nif");
        connection.execute_batch("CREATE TABLE race_animation_sources(race_id INTEGER,sex TEXT,skeleton_nif TEXT,project_hkx TEXT); INSERT INTO race_animation_sources VALUES (13,'female','meshes/custom/skeleton.nif','meshes/custom/project.hkx');").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let clip = "meshes/actor-idles/clips/idle.glb";
        std::fs::create_dir_all(directory.path().join("meshes/actor-idles/clips")).unwrap();
        std::fs::write(directory.path().join(clip), b"clip").unwrap();
        let variant = "meshes/actor-idles/clips/variant.glb";
        std::fs::write(directory.path().join(variant), b"clip").unwrap();
        let row = ActorIdleBinding {
            race_id: 13,
            sex: "female".into(),
            body_scene: "meshes/actors/custom/body.glb".into(),
            project: "meshes/custom/project.hkx".into(),
            skeleton: "meshes/custom/rig.hkx".into(),
            race_skeleton_nif: "meshes/custom/skeleton.nif".into(),
            source: "meshes/custom/unusually_named_idle.hkx".into(),
            scene: clip.into(),
            bound_tracks: 5,
            moving_bound_tracks: 2,
            skipped_tracks: Vec::new(),
            clips: std::collections::BTreeMap::from([(
                "meshes/custom/breathe.hkx".into(),
                shared::actor_idle_catalog::ActorIdleClip {
                    scene: variant.into(),
                    frame_count: 2,
                    frames_per_second: 30.0,
                    motion_frame: Some(1),
                    bound_tracks: 5,
                    moving_bound_tracks: 2,
                    skipped_tracks: Vec::new(),
                },
            )]),
        };
        let catalog = ActorIdleCatalog {
            version: VERSION,
            bindings: vec![row],
            unresolved: Vec::new(),
            ..Default::default()
        };
        std::fs::write(
            directory.path().join(PATH),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        let read = || ActorCatalog::from_connection_with_assets_root(&connection, directory.path());
        let loaded = read();
        let plan = loaded.spawn_plan(loaded.get(77).unwrap()).unwrap();
        assert_eq!(loaded.idle_scene(&plan), Some(clip));
        for authored in [
            r"Actors\\Custom\Body.nif",
            " ./Actors//Custom/./Body.NIF ",
            "Data/Meshes/Data/Meshes/Actors/Custom/Body.nif",
            "Actors/%43ustom/Body.nif",
        ] {
            connection
                .execute(
                    "UPDATE actor_model_parts SET model_path=?1 WHERE npc_id=77 AND slot='body'",
                    [authored],
                )
                .unwrap();
            let normalized = read();
            let normalized_plan = normalized.spawn_plan(normalized.get(77).unwrap()).unwrap();
            assert_eq!(
                normalized_plan.scene_path, "meshes/actors/custom/body.glb",
                "{authored}"
            );
            assert_eq!(
                normalized.idle_scene(&normalized_plan),
                Some(clip),
                "{authored}"
            );
        }
        assert_eq!(
            loaded.authored_idle_scene(&plan.actor, &plan.scene_path, "meshes/custom/breathe.hkx"),
            Some(variant)
        );
        assert!(
            loaded
                .authored_idle_scene(&plan.actor, &plan.scene_path, "meshes/other/breathe.hkx")
                .is_none()
        );
        let mut wrong = plan.clone();
        wrong.actor.race_id = 14;
        assert!(loaded.idle_scene(&wrong).is_none());
        wrong = plan.clone();
        wrong.scene_path = "meshes/actors/different/body.glb".into();
        assert!(loaded.idle_scene(&wrong).is_none());
        connection
            .execute(
                "UPDATE race_animation_sources SET project_hkx='meshes/custom/replaced.hkx'",
                [],
            )
            .unwrap();
        assert!(
            read().idle_scene(&plan).is_none(),
            "metadata-only RACE changes must reject the old binding"
        );
    }

    fn alvor(connection: &Connection) {
        insert_actor(
            connection,
            0x13475,
            79686,
            "male",
            "body_head",
            Some("meshes/actors/character/facegendata/facegeom/skyrim.esm/00013475.nif"),
            Some("facegen"),
        );
        insert_part(
            connection,
            0x13475,
            "body",
            r"Meshes\Actors\Character\Character Assets\MaleBody_1.NIF",
        );
        insert_part(
            connection,
            0x13475,
            "hands",
            r"Meshes\Actors\Character\Character Assets\MaleHands_1.nif",
        );
        insert_part(
            connection,
            0x13475,
            "feet",
            r"Meshes\Actors\Character\Character Assets\MaleFeet_1.nif",
        );
    }

    #[test]
    fn reads_models_and_parts_keyed_by_form_id() {
        let connection = connection();
        alvor(&connection);
        insert_actor(
            &connection,
            0x13476,
            79686,
            "female",
            "body_head",
            None,
            None,
        );
        insert_part(&connection, 0x13476, "body", "FemaleBody_1.NIF");
        let catalog = ActorCatalog::from_connection(&connection);

        assert_eq!(catalog.len(), 2);
        let model = catalog.get(0x13475).expect("Alvor's catalog row");
        assert_eq!(model.base_form_id, 0x13475);
        assert_eq!(model.race_id, 79686);
        assert_eq!(model.sex, ActorSex::Male);
        assert_eq!(model.resolution, ActorResolution::BodyHead);
        assert_eq!(model.head_source, Some(ActorHeadSource::Facegen));
        assert_eq!(
            model
                .part(ActorSlot::Body)
                .map(|part| part.model_path.as_str()),
            Some(r"Meshes\Actors\Character\Character Assets\MaleBody_1.NIF")
        );
        assert_eq!(
            model
                .part(ActorSlot::Hands)
                .map(|part| part.model_path.as_str()),
            Some(r"Meshes\Actors\Character\Character Assets\MaleHands_1.nif")
        );
        assert_eq!(
            model
                .part(ActorSlot::Feet)
                .map(|part| part.model_path.as_str()),
            Some(r"Meshes\Actors\Character\Character Assets\MaleFeet_1.nif")
        );
        let female = catalog.get(0x13476).expect("Sigrid's catalog row");
        assert_eq!(female.sex, ActorSex::Female);
        assert!(catalog.get(0x99999).is_none());
    }

    #[test]
    fn the_skin_tone_reaches_the_spawn_plan() {
        let connection = connection();
        alvor(&connection);
        connection
            .execute(
                "UPDATE actor_models SET skin_tone_r=?1, skin_tone_g=?2, skin_tone_b=?3 WHERE id=0x13475",
                params![198.0 / 255.0, 176.0 / 255.0, 168.0 / 255.0],
            )
            .unwrap();
        insert_actor(&connection, 0x13476, 79686, "female", "body", None, None);
        insert_part(&connection, 0x13476, "body", "FemaleBody_1.NIF");
        let catalog = ActorCatalog::from_connection(&connection);

        let tone = Vec3::new(198.0 / 255.0, 176.0 / 255.0, 168.0 / 255.0);
        let model = catalog.get(0x13475).unwrap();
        assert_eq!(model.skin_tone, Some(ActorSkinTone(tone)));
        assert_eq!(
            catalog.spawn_plan(model).unwrap().skin_tone,
            Some(ActorSkinTone(tone))
        );
        let untoned = catalog.get(0x13476).unwrap();
        assert_eq!(untoned.skin_tone, None);
        assert_eq!(catalog.spawn_plan(untoned).unwrap().skin_tone, None);
    }

    #[test]
    fn the_body_weight_reaches_the_spawn_plan() {
        let connection = connection();
        alvor(&connection);
        insert_actor(&connection, 0x13476, 79686, "female", "body", None, None);
        insert_part(&connection, 0x13476, "body", "FemaleBody_1.NIF");
        connection
            .execute("UPDATE actor_models SET weight=20.0 WHERE id=0x13475", [])
            .unwrap();
        let catalog = ActorCatalog::from_connection(&connection);

        let weighted = catalog.get(0x13475).unwrap();
        assert_eq!(weighted.weight, Some(20.0));
        assert_eq!(catalog.spawn_plan(weighted).unwrap().weight, Some(20.0));

        let unweighted = catalog.get(0x13476).unwrap();
        assert_eq!(unweighted.weight, None);
        assert_eq!(catalog.spawn_plan(unweighted).unwrap().weight, None);
    }

    /// A database with the skin tone columns but not the weight keeps its tones.
    #[test]
    fn a_table_without_the_weight_column_keeps_its_skin_tone() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE actor_models (
                     id INTEGER PRIMARY KEY, race_id INTEGER NOT NULL, sex TEXT NOT NULL,
                     skin_armo_id INTEGER, head_model TEXT, head_source TEXT,
                     resolution TEXT NOT NULL, skin_tone_r REAL, skin_tone_g REAL,
                     skin_tone_b REAL
                 );
                 CREATE TABLE actor_model_parts (
                     npc_id INTEGER NOT NULL, slot TEXT NOT NULL, model_path TEXT NOT NULL,
                     PRIMARY KEY (npc_id, slot)
                 );",
            )
            .unwrap();
        alvor(&connection);
        connection
            .execute(
                "UPDATE actor_models SET skin_tone_r=0.75, skin_tone_g=0.5, skin_tone_b=0.25",
                [],
            )
            .unwrap();
        let catalog = ActorCatalog::from_connection(&connection);
        let model = catalog.get(0x13475).expect("the row still reads");
        assert_eq!(
            model.skin_tone,
            Some(ActorSkinTone(Vec3::new(0.75, 0.5, 0.25)))
        );
        assert_eq!(model.weight, None);
    }

    /// A database converted before the skin tone columns still reads, without tones.
    #[test]
    fn a_table_without_the_skin_tone_columns_reads_without_tones() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE actor_models (
                     id INTEGER PRIMARY KEY, race_id INTEGER NOT NULL, sex TEXT NOT NULL,
                     skin_armo_id INTEGER, head_model TEXT, head_source TEXT,
                     resolution TEXT NOT NULL
                 );
                 CREATE TABLE actor_model_parts (
                     npc_id INTEGER NOT NULL, slot TEXT NOT NULL, model_path TEXT NOT NULL,
                     PRIMARY KEY (npc_id, slot)
                 );",
            )
            .unwrap();
        alvor(&connection);
        let catalog = ActorCatalog::from_connection(&connection);
        let model = catalog.get(0x13475).expect("the row still reads");
        assert_eq!(model.skin_tone, None);
        assert_eq!(model.parts.len(), 3);
    }

    #[test]
    fn missing_parts_table_makes_catalog_unavailable_atomically() {
        let connection = connection();
        insert_actor(&connection, 0x13475, 79686, "male", "body", None, None);
        connection
            .execute_batch("DROP TABLE actor_model_parts;")
            .unwrap();
        let assets_root = PathBuf::from("converted-assets");

        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, &assets_root);

        assert!(catalog.is_empty());
        assert!(catalog.get(0x13475).is_none());
        assert_eq!(catalog.assets_root.as_deref(), Some(assets_root.as_path()));
    }

    #[test]
    fn incompatible_parts_columns_make_catalog_unavailable_atomically() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE actor_models (
                    id INTEGER PRIMARY KEY, race_id INTEGER NOT NULL, sex TEXT NOT NULL,
                    skin_armo_id INTEGER, head_model TEXT, head_source TEXT,
                    resolution TEXT NOT NULL
                 );
                 INSERT INTO actor_models VALUES (0x13475, 79686, 'male', NULL, NULL, NULL, 'body');
                 CREATE TABLE actor_model_parts (npc_id INTEGER, slot TEXT, wrong_column TEXT);",
            )
            .unwrap();
        let catalog = ActorCatalog::from_connection(&connection);

        assert!(catalog.is_empty());
        assert!(catalog.get(0x13475).is_none());
    }

    #[test]
    fn usable_parts_schema_keeps_rows_with_no_parts_and_skips_malformed_parts() {
        let connection = connection();
        insert_actor(&connection, 0x1, 1, "male", "body", None, None);
        insert_actor(&connection, 0x2, 1, "female", "unresolved", None, None);
        connection
            .execute(
                "INSERT INTO actor_model_parts(npc_id, slot, model_path) VALUES (?1, ?2, ?3)",
                params![0x1_u32, 42_i64, "malformed-slot"],
            )
            .unwrap();

        let catalog = ActorCatalog::from_connection(&connection);

        assert_eq!(catalog.len(), 2);
        let resolved = catalog.get(0x1).expect("resolved row with no usable parts");
        assert!(resolved.parts.is_empty());
        assert_eq!(
            catalog.spawn_plan(resolved).unwrap_err().reason,
            ActorSpawnSkip::NoBody
        );
        let unresolved = catalog.get(0x2).expect("unresolved row");
        assert_eq!(
            catalog.spawn_plan(unresolved).unwrap_err().reason,
            ActorSpawnSkip::Unresolved
        );
    }

    #[test]
    fn a_database_without_the_tables_is_an_empty_catalog() {
        let connection = Connection::open_in_memory().unwrap();
        let catalog = ActorCatalog::from_connection(&connection);
        assert!(catalog.is_empty());
        assert!(catalog.get(0x13475).is_none());
    }

    #[test]
    fn a_table_of_another_shape_is_an_empty_catalog() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE actor_models(id INTEGER PRIMARY KEY, sex TEXT);
                 INSERT INTO actor_models VALUES(0x13475, 'male');",
            )
            .unwrap();
        let catalog = ActorCatalog::from_connection(&connection);
        assert!(catalog.is_empty());
    }

    #[test]
    fn unknown_sex_or_resolution_rows_are_skipped_without_poisoning_the_rest() {
        let connection = connection();
        alvor(&connection);
        insert_actor(&connection, 0x1, 1, "neuter", "body", None, None);
        insert_actor(&connection, 0x2, 1, "male", "half_body", None, None);
        let catalog = ActorCatalog::from_connection(&connection);
        assert_eq!(catalog.len(), 1);
        assert!(catalog.get(0x13475).is_some());
        assert!(catalog.get(0x1).is_none());
        assert!(catalog.get(0x2).is_none());
    }

    #[test]
    fn unresolved_rows_yield_one_typed_diagnostic() {
        let connection = connection();
        insert_actor(
            &connection,
            0x13475,
            79686,
            "male",
            "unresolved",
            None,
            None,
        );
        let catalog = ActorCatalog::from_connection(&connection);
        let model = catalog.get(0x13475).unwrap();
        let diagnostic = catalog.spawn_plan(model).unwrap_err();
        assert_eq!(
            diagnostic,
            ActorSpawnDiagnostic {
                base_form_id: 0x13475,
                reason: ActorSpawnSkip::Unresolved,
            }
        );
        assert_eq!(
            diagnostic.to_string(),
            "NPC_ 00013475: resolution is unresolved"
        );
    }

    #[test]
    fn a_resolved_actor_without_a_body_yields_the_no_body_diagnostic() {
        let connection = connection();
        insert_actor(&connection, 0x13475, 79686, "male", "body", None, None);
        insert_part(&connection, 0x13475, "hands", "MaleHands_1.nif");
        let catalog = ActorCatalog::from_connection(&connection);
        let model = catalog.get(0x13475).unwrap();
        let diagnostic = catalog.spawn_plan(model).unwrap_err();
        assert_eq!(diagnostic.reason, ActorSpawnSkip::NoBody);
        assert_eq!(diagnostic.base_form_id, 0x13475);
    }

    #[test]
    fn the_composed_body_wins_when_its_asset_exists() {
        let connection = connection();
        alvor(&connection);
        let directory = tempfile::tempdir().unwrap();
        let composed = directory
            .path()
            .join("meshes/actors/character/composed/default/male/body.glb");
        std::fs::create_dir_all(composed.parent().unwrap()).unwrap();
        std::fs::write(&composed, b"glTF").unwrap();
        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());

        let model = catalog.get(0x13475).unwrap();
        let plan = catalog.spawn_plan(model).unwrap();
        assert_eq!(
            plan.scene_path,
            "meshes/actors/character/composed/default/male/body.glb"
        );
        assert_eq!(plan.actor.base_form_id, 0x13475);
        assert_eq!(plan.actor.sex, ActorSex::Male);
        assert_eq!(plan.parts.len(), 3);
        assert_eq!(
            plan.head.as_ref().map(|head| head.source),
            Some(ActorHeadSource::Facegen)
        );
    }

    fn write_outfit_catalog(directory: &Path, catalog: &shared::actor_outfits::ActorOutfitCatalog) {
        std::fs::write(
            directory.join(shared::actor_outfits::PATH),
            serde_json::to_vec(catalog).unwrap(),
        )
        .unwrap();
    }

    fn blacksmith_outfit(npc: u32) -> shared::actor_outfits::ActorOutfitCatalog {
        shared::actor_outfits::ActorOutfitCatalog {
            version: shared::actor_outfits::VERSION,
            outfits: vec![shared::actor_outfits::ActorOutfit {
                npc_id: npc,
                outfit_id: 0x57c4c,
                parts: vec![shared::actor_outfits::OutfitPart {
                    armo_id: 0x5b69f,
                    armature_id: 0x5b6a2,
                    biped_mask: shared::actor_outfits::BIPED_BODY,
                    armature_race_id: 25,
                    scene: "meshes/clothes/blacksmith/m/torso_1.glb".into(),
                }],
            }],
            skipped: Default::default(),
        }
    }

    fn composed_male_body(directory: &Path) {
        let composed = directory.join("meshes/actors/character/composed/default/male/body.glb");
        std::fs::create_dir_all(composed.parent().unwrap()).unwrap();
        std::fs::write(&composed, b"glTF").unwrap();
    }

    #[test]
    fn a_composed_body_actor_carries_its_worn_outfit() {
        let connection = connection();
        alvor(&connection);
        let directory = tempfile::tempdir().unwrap();
        composed_male_body(directory.path());
        write_outfit_catalog(directory.path(), &blacksmith_outfit(0x13475));
        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());

        let plan = catalog.spawn_plan(catalog.get(0x13475).unwrap()).unwrap();
        let outfit = plan.outfit.expect("the worn outfit");
        assert_eq!(outfit.0.len(), 1);
        assert_eq!(outfit.0[0].scene, "meshes/clothes/blacksmith/m/torso_1.glb");
    }

    #[test]
    fn an_actor_without_an_outfit_row_or_a_composed_body_wears_nothing() {
        let connection = connection();
        alvor(&connection);
        let directory = tempfile::tempdir().unwrap();
        write_outfit_catalog(directory.path(), &blacksmith_outfit(0x13475));
        // No composed body: the actor draws its catalog body part, which the outfit does not fit.
        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());
        let plan = catalog.spawn_plan(catalog.get(0x13475).unwrap()).unwrap();
        assert!(plan.outfit.is_none());

        // A composed body but a different actor in the catalog: no row, no outfit.
        composed_male_body(directory.path());
        write_outfit_catalog(directory.path(), &blacksmith_outfit(0x999));
        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());
        let plan = catalog.spawn_plan(catalog.get(0x13475).unwrap()).unwrap();
        assert!(plan.outfit.is_none());
    }

    #[test]
    fn a_missing_or_invalid_outfit_catalog_leaves_actors_in_their_skin() {
        let connection = connection();
        alvor(&connection);
        let directory = tempfile::tempdir().unwrap();
        composed_male_body(directory.path());
        let plan_outfit = |directory: &Path| {
            let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory);
            catalog
                .spawn_plan(catalog.get(0x13475).unwrap())
                .unwrap()
                .outfit
        };
        assert!(
            plan_outfit(directory.path()).is_none(),
            "older pack: no file"
        );

        std::fs::write(
            directory.path().join(shared::actor_outfits::PATH),
            b"not json",
        )
        .unwrap();
        assert!(
            plan_outfit(directory.path()).is_none(),
            "unreadable catalog"
        );

        let mut invalid = blacksmith_outfit(0x13475);
        invalid.outfits[0].parts[0].scene = "Meshes/../escape.glb".into();
        write_outfit_catalog(directory.path(), &invalid);
        assert!(
            plan_outfit(directory.path()).is_none(),
            "invalid catalog is rejected whole"
        );
    }

    #[test]
    fn bare_retail_actor_paths_use_the_composed_body() {
        let connection = connection();
        insert_actor(&connection, 0x13475, 79686, "male", "body", None, None);
        insert_part(
            &connection,
            0x13475,
            "body",
            r"Actors\Character\Character Assets\MaleBody_1.nif",
        );
        insert_part(
            &connection,
            0x13475,
            "hands",
            r"Actors\Character\Character Assets\MaleHands_1.nif",
        );
        insert_part(
            &connection,
            0x13475,
            "feet",
            r"Actors\Character\Character Assets\MaleFeet_1.nif",
        );
        let directory = tempfile::tempdir().unwrap();
        let composed = directory
            .path()
            .join("meshes/actors/character/composed/default/male/body.glb");
        std::fs::create_dir_all(composed.parent().unwrap()).unwrap();
        std::fs::write(&composed, b"glTF").unwrap();
        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());

        let model = catalog.get(0x13475).unwrap();
        let plan = catalog.spawn_plan(model).unwrap();
        assert_eq!(
            plan.scene_path,
            "meshes/actors/character/composed/default/male/body.glb"
        );
        assert!(plan.uses_composed_body());
    }

    #[test]
    fn a_custom_race_body_keeps_its_own_path_when_the_composed_body_exists() {
        let connection = connection();
        insert_actor(&connection, 0x2000, 0x1000, "male", "body", None, None);
        insert_part(
            &connection,
            0x2000,
            "body",
            r"Meshes\Actors\CustomRace\Character Assets\MaleBody_1.NIF",
        );
        insert_part(
            &connection,
            0x2000,
            "hands",
            r"Meshes\Actors\CustomRace\Character Assets\CustomHands.NIF",
        );
        insert_part(
            &connection,
            0x2000,
            "feet",
            r"Meshes\Actors\CustomRace\Character Assets\CustomFeet.NIF",
        );
        let directory = tempfile::tempdir().unwrap();
        let composed = directory
            .path()
            .join("meshes/actors/character/composed/default/male/body.glb");
        std::fs::create_dir_all(composed.parent().unwrap()).unwrap();
        std::fs::write(&composed, b"glTF").unwrap();
        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());

        let model = catalog.get(0x2000).unwrap();
        let plan = catalog.spawn_plan(model).unwrap();
        assert_eq!(
            plan.scene_path, "meshes/actors/customrace/character assets/malebody_1.glb",
            "a race with its own armature keeps its authored body"
        );
        assert!(!plan.uses_composed_body());
    }

    #[test]
    fn a_row_without_a_body_slot_does_not_borrow_the_composed_body() {
        let connection = connection();
        insert_actor(&connection, 0x13476, 79686, "male", "body", None, None);
        insert_part(
            &connection,
            0x13476,
            "hands",
            r"Meshes\Actors\Character\Character Assets\MaleHands_1.nif",
        );
        let directory = tempfile::tempdir().unwrap();
        let composed = directory
            .path()
            .join("meshes/actors/character/composed/default/male/body.glb");
        std::fs::create_dir_all(composed.parent().unwrap()).unwrap();
        std::fs::write(&composed, b"glTF").unwrap();
        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());

        let model = catalog.get(0x13476).unwrap();
        let diagnostic = catalog.spawn_plan(model).unwrap_err();
        assert_eq!(diagnostic.reason, ActorSpawnSkip::NoBody);
        assert_eq!(diagnostic.base_form_id, 0x13476);
    }

    #[test]
    fn the_body_part_is_the_fallback_before_a_composed_body_exists() {
        let connection = connection();
        insert_actor(&connection, 0x13476, 79686, "female", "body", None, None);
        insert_part(
            &connection,
            0x13476,
            "body",
            r"Meshes\Actors\Character\Character Assets\FemaleBody_1.NIF",
        );
        let directory = tempfile::tempdir().unwrap();
        let catalog = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());

        let model = catalog.get(0x13476).unwrap();
        let plan = catalog.spawn_plan(model).unwrap();
        assert_eq!(
            plan.scene_path,
            "meshes/actors/character/character assets/femalebody_1.glb"
        );
        assert!(plan.head.is_none());
    }

    #[test]
    fn an_actor_without_a_head_path_keeps_no_head_component() {
        let connection = connection();
        alvor(&connection);
        let catalog = ActorCatalog::from_connection(&connection);
        let mut model = catalog.get(0x13475).unwrap().clone();
        model.head_model = None;
        let plan = catalog.spawn_plan(&model).unwrap();
        assert!(plan.head.is_none());
    }

    /// A creature binding with the given clips: `(source key, frame count, file on disk)`.
    fn creature_catalog(
        clips: &[(&str, u32, bool)],
    ) -> (tempfile::TempDir, ActorCatalog, ActorSpawnPlan) {
        use shared::actor_idle_catalog::{
            ActorIdleBinding, ActorIdleCatalog, ActorIdleClip, PATH, VERSION,
        };
        let connection = connection();
        insert_actor(&connection, 77, 13, "male", "body", None, None);
        insert_part(&connection, 77, "body", "Actors/Custom/body.nif");
        connection.execute_batch("CREATE TABLE race_animation_sources(race_id INTEGER,sex TEXT,skeleton_nif TEXT,project_hkx TEXT); INSERT INTO race_animation_sources VALUES (13,'male','meshes/custom/skeleton.nif','meshes/custom/project.hkx');").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let clip_directory = directory.path().join("meshes/actor-idles/clips");
        std::fs::create_dir_all(&clip_directory).unwrap();
        let mut map = std::collections::BTreeMap::new();
        for (index, (source, frames, on_disk)) in clips.iter().enumerate() {
            let scene = format!("meshes/actor-idles/clips/clip{index}.glb");
            if *on_disk {
                std::fs::write(directory.path().join(&scene), b"clip").unwrap();
            }
            map.insert(
                (*source).to_owned(),
                ActorIdleClip {
                    scene,
                    frame_count: *frames,
                    frames_per_second: 30.0,
                    motion_frame: (*frames >= 2).then_some(1),
                    bound_tracks: 5,
                    moving_bound_tracks: 2,
                    skipped_tracks: Vec::new(),
                },
            );
        }
        let base = clips[0].0;
        let binding = ActorIdleBinding {
            race_id: 13,
            sex: "male".into(),
            body_scene: "meshes/actors/custom/body.glb".into(),
            project: "meshes/custom/project.hkx".into(),
            skeleton: "meshes/custom/rig.hkx".into(),
            race_skeleton_nif: "meshes/custom/skeleton.nif".into(),
            source: base.into(),
            scene: map[base].scene.clone(),
            bound_tracks: 5,
            moving_bound_tracks: 2,
            skipped_tracks: Vec::new(),
            clips: map,
        };
        let catalog = ActorIdleCatalog {
            version: VERSION,
            bindings: vec![binding],
            ..Default::default()
        };
        std::fs::write(
            directory.path().join(PATH),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        let loaded = ActorCatalog::from_connection_with_assets_root(&connection, directory.path());
        let plan = loaded.spawn_plan(loaded.get(77).unwrap()).unwrap();
        (directory, loaded, plan)
    }

    const DOG: &str = "meshes/actors/canine/animations/";
    const COW: &str = "meshes/actors/cow/animations/";

    fn sources(pool: &[IdlePoolClip]) -> Vec<&str> {
        pool.iter().map(|clip| clip.source.as_str()).collect()
    }

    #[test]
    fn the_dog_pool_is_the_verified_layer_clips_in_list_order() {
        let dog = |name: &str| format!("{DOG}{name}.hkx");
        let keys = [
            dog("mt_idle"),
            dog("dog_idle_spice_tail1"),
            dog("dog_spice_head3"),
            dog("dog_idle_spice_head1"),
            dog("dog_spice_bark"),
            dog("dog_idle_spice_head2"),
            // Listed, but skipped: one frame, and no file on disk.
            dog("dog_spice_head4"),
            dog("dog_idle_spice_tail2"),
            // Not listed: postures, a fight clip and an idle-looking name.
            dog("dog_spice_sit_head1"),
            dog("dog_spice_lay_tail1"),
            dog("attack1"),
            dog("idle_foraging"),
        ];
        let frames = |key: &str| if key.ends_with("head4.hkx") { 1 } else { 30 };
        let clips: Vec<(&str, u32, bool)> = keys
            .iter()
            .map(|key| (key.as_str(), frames(key), !key.ends_with("tail2.hkx")))
            .collect();
        let (_directory, catalog, plan) = creature_catalog(&clips);
        let pool = catalog.idle_sequence_pool(&plan);
        assert_eq!(
            sources(&pool),
            [
                dog("mt_idle"),
                dog("dog_spice_bark"),
                dog("dog_idle_spice_head1"),
                dog("dog_idle_spice_head2"),
                dog("dog_spice_head3"),
                dog("dog_idle_spice_tail1"),
            ]
        );
        assert!(pool.iter().all(|clip| clip.scene.ends_with(".glb")));
    }

    #[test]
    fn the_cow_pool_is_the_three_tail_wags_and_nothing_else() {
        let cow = |name: &str| format!("{COW}{name}.hkx");
        let keys = [
            cow("mt_idle"),
            cow("idle_tailwag2"),
            cow("idle_tailwag"),
            cow("idle_tailwag1"),
            cow("idle_lookleft"),
            cow("idle_mooo"),
            cow("idle_feed"),
            cow("idle_foraging"),
            cow("idle_enterfeed"),
        ];
        let clips: Vec<(&str, u32, bool)> =
            keys.iter().map(|key| (key.as_str(), 30, true)).collect();
        let (_directory, catalog, plan) = creature_catalog(&clips);
        assert_eq!(
            sources(&catalog.idle_sequence_pool(&plan)),
            [
                cow("mt_idle"),
                cow("idle_tailwag"),
                cow("idle_tailwag1"),
                cow("idle_tailwag2"),
            ]
        );
    }

    #[test]
    fn a_creature_outside_the_verified_families_keeps_its_default_clip() {
        // A goblin-like binding with scene-only idles, and a wolf: it shares the canine clips
        // but its default clip is `mt_idle_wolf`.
        for (base, others) in [
            (
                "meshes/actors/goblin/animations/mt_idle.hkx".to_owned(),
                vec![
                    "meshes/actors/goblin/animations/idleprayingbow.hkx".to_owned(),
                    "meshes/actors/goblin/animations/idlechisel.hkx".to_owned(),
                    "meshes/actors/goblin/animations/chieftain_idle_cheer.hkx".to_owned(),
                ],
            ),
            (
                format!("{DOG}mt_idle_wolf.hkx"),
                vec![
                    format!("{DOG}dog_idle_spice_head1.hkx"),
                    format!("{DOG}dog_idle_spice_tail1.hkx"),
                ],
            ),
        ] {
            let mut keys = vec![base.clone()];
            keys.extend(others);
            let clips: Vec<(&str, u32, bool)> =
                keys.iter().map(|key| (key.as_str(), 30, true)).collect();
            let (_directory, catalog, plan) = creature_catalog(&clips);
            let pool = catalog.idle_sequence_pool(&plan);
            assert_eq!(sources(&pool), [base.as_str()], "{base}");
            assert!(
                crate::actor_animation::IdleSequence::new(pool, 1).is_none(),
                "no variety means no sequence: the default clip keeps looping"
            );
        }
    }

    #[test]
    fn a_race_without_a_binding_has_an_empty_pool() {
        let (_directory, catalog, plan) =
            creature_catalog(&[("meshes/custom/mtidle.hkx", 30, true)]);
        let mut elsewhere = plan.clone();
        elsewhere.actor.race_id = 14;
        assert!(catalog.idle_sequence_pool(&elsewhere).is_empty());
    }

    #[test]
    fn the_pool_is_deterministic_and_seeded_sequences_repeat() {
        let dog = |name: &str| format!("{DOG}{name}.hkx");
        let keys = [
            dog("mt_idle"),
            dog("dog_idle_spice_head1"),
            dog("dog_idle_spice_head2"),
            dog("dog_idle_spice_tail1"),
        ];
        let clips: Vec<(&str, u32, bool)> =
            keys.iter().map(|key| (key.as_str(), 30, true)).collect();
        let (_directory, catalog, plan) = creature_catalog(&clips);
        let first = catalog.idle_sequence_pool(&plan);
        assert_eq!(first, catalog.idle_sequence_pool(&plan));
        let play = |reference: u32| {
            let mut sequence =
                crate::actor_animation::IdleSequence::new(first.clone(), reference).unwrap();
            (0..12)
                .map(|_| {
                    sequence.advance_for_test();
                    sequence.current_source().to_owned()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(play(0x000A_91A0), play(0x000A_91A0));
        assert_ne!(play(0x000A_91A0), play(0x000A_91A1));
    }
}
