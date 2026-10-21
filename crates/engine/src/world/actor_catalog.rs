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

use crate::actors::{
    Actor, ActorHead, ActorHeadSource, ActorPart, ActorParts, ActorResolution, ActorSex, ActorSlot,
    ActorSpawnDiagnostic, ActorSpawnSkip, composed_body_scene,
};
use bevy::prelude::*;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One `actor_models` row plus its `actor_model_parts` rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorModel {
    pub base_form_id: u32,
    pub race_id: u32,
    pub sex: ActorSex,
    pub skin_armo_id: Option<u32>,
    pub head_model: Option<String>,
    pub head_source: Option<ActorHeadSource>,
    pub resolution: ActorResolution,
    pub parts: Vec<ActorPart>,
}

impl ActorModel {
    pub fn part(&self, slot: ActorSlot) -> Option<&ActorPart> {
        self.parts.iter().find(|part| part.slot == slot)
    }
}

/// Everything the spawn path needs for one actor: the components and the scene it draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorSpawnPlan {
    pub actor: Actor,
    pub parts: ActorParts,
    pub head: Option<ActorHead>,
    /// The composed body when it exists, otherwise the catalog's `body` part, converted.
    pub scene_path: String,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct ActorCatalog {
    models: HashMap<u32, ActorModel>,
    assets_root: Option<PathBuf>,
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
        let mut models = HashMap::new();
        let query = "SELECT id, race_id, sex, skin_armo_id, head_model, head_source, resolution \
                     FROM actor_models";
        match connection.prepare(query) {
            Ok(mut statement) => match statement.query_map([], |row| {
                let base_form_id: u32 = row.get(0)?;
                let race_id: u32 = row.get(1)?;
                let sex: String = row.get(2)?;
                let skin_armo_id: Option<u32> = row.get(3)?;
                let head_model: Option<String> = row.get(4)?;
                let head_source: Option<String> = row.get(5)?;
                let resolution: String = row.get(6)?;
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
                    info!(
                        "actors: no usable `actor_model_parts` table ({error}); actors keep no parts"
                    );
                }
            },
            Err(error) => {
                info!(
                    "actors: no usable `actor_model_parts` table ({error}); actors keep no parts"
                );
            }
        }

        for model in models.values_mut() {
            model.parts.sort_by_key(|part| part.slot.index());
        }

        Self {
            models,
            assets_root,
        }
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
    /// The composed body wins when its file exists; otherwise the catalog's `body` part is
    /// converted. A row the catalog recorded as unresolved, or one with no resolvable body at all,
    /// yields one typed diagnostic and no plan.
    pub fn spawn_plan(&self, model: &ActorModel) -> Result<ActorSpawnPlan, ActorSpawnDiagnostic> {
        if !model.resolution.is_resolved() {
            return Err(ActorSpawnDiagnostic {
                base_form_id: model.base_form_id,
                reason: ActorSpawnSkip::Unresolved,
            });
        }
        let scene_path = self
            .composed_body_scene(model.sex)
            .map(str::to_owned)
            .or_else(|| {
                model.part(ActorSlot::Body).and_then(|part| {
                    crate::streaming::converted_model_path(part.model_path.clone())
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
        Ok(ActorSpawnPlan {
            actor: Actor::new(
                model.base_form_id,
                model.race_id,
                model.sex,
                model.resolution,
            ),
            parts: ActorParts::new(model.parts.iter().cloned()),
            head,
            scene_path,
        })
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
            resolution TEXT NOT NULL
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
}
