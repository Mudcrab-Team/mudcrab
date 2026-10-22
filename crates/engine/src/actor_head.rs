//! FaceGen head attachment for spawned actors.
//!
//! The catalog's FaceGen head is a skinned mesh exported against the same shared humanoid
//! skeleton as the composed body. A skinned mesh's node transform is ignored by glTF, so parenting
//! the head scene under the body's `NPC Head [Head]` bone would leave it at its own skeleton's
//! origin: its placement comes from its joints. Instead, once both scenes are ready, every
//! [`SkinnedMesh`] in the head instance has its joint list rewritten to the body's bone entities of
//! the same [`Name`]. Both assets were exported against the same skeleton and bind pose, so the
//! inverse bind poses stay valid and the head follows the animated body.
//!
//! A skinless head (the deferred default-head case) has no joints to rehost, so its instance is
//! parented under the head bone instead. Actors whose [`ActorHead`] is not FaceGen are left exactly
//! as they are, and an actor without a [`ActorHead`] component is never touched.

use crate::actor_animation::{ActorBodyReady, animation_targets};
use crate::actors::{Actor, ActorHead, ActorHeadSource};
use bevy::{
    asset::LoadState,
    gltf::GltfAssetLabel,
    mesh::skinning::SkinnedMesh,
    prelude::*,
    world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady},
};
use std::{collections::HashMap, fmt};

/// The body skeleton node a FaceGen head is rehosted onto, and the parent of a skinless head.
pub const HEAD_BONE: &str = "NPC Head [Head]";

/// A FaceGen head scene the actor asked for and the asset server is loading.
#[derive(Component, Debug, Clone)]
struct ActorHeadRequest {
    asset: Handle<WorldAsset>,
    path: String,
}

/// The body skeleton a head binds to, captured while the body scene is the only scene under the
/// actor so the name walk cannot find the head's own nodes.
#[derive(Component, Debug, Clone)]
struct ActorHeadSkeleton {
    head_bone: Entity,
    bones: HashMap<String, Entity>,
}

/// One instantiated head scene waiting for its joints to be rehosted.
#[derive(Component, Debug, Clone, Copy)]
struct ActorHeadInstance {
    actor: Entity,
    base_form_id: u32,
}

/// An actor whose head decision is final: attached, skipped, or failed.
#[derive(Component, Debug, Clone, Copy)]
struct ActorHeadResolved;

/// A typed reason a FaceGen head was not attached to its actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorHeadDiagnostic {
    /// The body scene has no [`HEAD_BONE`] node.
    MissingHeadBone { base_form_id: u32 },
    /// The catalog's FaceGen path has no converted asset, or the asset failed to load.
    MissingAsset { base_form_id: u32, path: String },
    /// A head joint name has no body bone of the same name.
    UnresolvedJoints { base_form_id: u32, joints: usize },
}

impl ActorHeadDiagnostic {
    /// The stable identifier of this diagnostic.
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingHeadBone { .. } => "actor_head_bone_missing",
            Self::MissingAsset { .. } => "actor_head_asset_missing",
            Self::UnresolvedJoints { .. } => "actor_head_joint_missing",
        }
    }

    pub fn base_form_id(&self) -> u32 {
        match self {
            Self::MissingHeadBone { base_form_id }
            | Self::MissingAsset { base_form_id, .. }
            | Self::UnresolvedJoints { base_form_id, .. } => *base_form_id,
        }
    }
}

impl fmt::Display for ActorHeadDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeadBone { base_form_id } => write!(
                formatter,
                "NPC_ {base_form_id:08X}: the body has no `{HEAD_BONE}` bone"
            ),
            Self::MissingAsset { base_form_id, path } => write!(
                formatter,
                "NPC_ {base_form_id:08X}: FaceGen head `{path}` has no converted asset"
            ),
            Self::UnresolvedJoints {
                base_form_id,
                joints,
            } => write!(
                formatter,
                "NPC_ {base_form_id:08X}: {joints} head joint(s) have no body bone"
            ),
        }
    }
}

impl std::error::Error for ActorHeadDiagnostic {}

fn report(diagnostic: &ActorHeadDiagnostic) {
    warn!(
        code = diagnostic.code(),
        form_id = format_args!("{:08X}", diagnostic.base_form_id()),
        "{diagnostic}"
    );
}

/// Attaches catalog FaceGen heads to their actors' body skeletons.
pub struct ActorHeadPlugin;

impl Plugin for ActorHeadPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(attach_ready_actor_heads)
            .add_systems(Update, (queue_actor_heads, spawn_ready_head_scenes).chain());
    }
}

type PendingHeadActors<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static Actor, &'static ActorHead),
    (With<ActorBodyReady>, Without<ActorHeadResolved>),
>;

/// Captures the body skeleton a head will bind to and asks the asset server for its scene.
fn queue_actor_heads(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    names: Query<&Name>,
    children: Query<&Children>,
    actors: PendingHeadActors,
) {
    for (actor, actor_data, head) in &actors {
        if head.source != ActorHeadSource::Facegen {
            // A default head is a separate slice; nothing here may treat it as FaceGen.
            commands.entity(actor).insert(ActorHeadResolved);
            continue;
        }
        let Some((bones, head_bone)) = body_skeleton(actor, &names, &children) else {
            report(&ActorHeadDiagnostic::MissingHeadBone {
                base_form_id: actor_data.base_form_id,
            });
            commands.entity(actor).insert(ActorHeadResolved);
            continue;
        };
        let Some(path) = crate::streaming::converted_model_path(head.model_path.clone()) else {
            report(&ActorHeadDiagnostic::MissingAsset {
                base_form_id: actor_data.base_form_id,
                path: head.model_path.clone(),
            });
            commands.entity(actor).insert(ActorHeadResolved);
            continue;
        };
        let asset = asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.clone()));
        commands.entity(actor).insert((
            ActorHeadRequest { asset, path },
            ActorHeadSkeleton { head_bone, bones },
            ActorHeadResolved,
        ));
    }
}

/// Instantiates the head scene once the asset server has it, and reports a failed load once.
fn spawn_ready_head_scenes(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    requests: Query<(Entity, &Actor, &ActorHeadRequest)>,
) {
    for (actor, actor_data, request) in &requests {
        match asset_server.load_state(request.asset.id()) {
            LoadState::Loaded => {
                commands.entity(actor).remove::<ActorHeadRequest>();
                commands.spawn((
                    Name::new(format!("Head {:08X}", actor_data.base_form_id)),
                    ActorHeadInstance {
                        actor,
                        base_form_id: actor_data.base_form_id,
                    },
                    WorldAssetRoot(request.asset.clone()),
                    ChildOf(actor),
                ));
            }
            LoadState::Failed(error) => {
                let diagnostic = ActorHeadDiagnostic::MissingAsset {
                    base_form_id: actor_data.base_form_id,
                    path: request.path.clone(),
                };
                warn!(
                    code = diagnostic.code(),
                    form_id = format_args!("{:08X}", diagnostic.base_form_id()),
                    %error,
                    "{diagnostic}: {error}"
                );
                commands.entity(actor).remove::<ActorHeadRequest>();
            }
            LoadState::NotLoaded | LoadState::Loading => {}
        }
    }
}

/// Rehosts a ready head scene's skins onto the actor's body bones, or parents a skinless head under
/// the body's head bone.
fn attach_ready_actor_heads(
    ready: On<WorldInstanceReady>,
    mut commands: Commands,
    names: Query<&Name>,
    children: Query<&Children>,
    skeletons: Query<&ActorHeadSkeleton>,
    instances: Query<&ActorHeadInstance>,
    skinned: Query<&SkinnedMesh>,
) {
    let Ok(instance) = instances.get(ready.entity) else {
        return;
    };
    let Ok(skeleton) = skeletons.get(instance.actor) else {
        commands.entity(ready.entity).despawn();
        return;
    };
    let scene_entities: Vec<Entity> = std::iter::once(ready.entity)
        .chain(children.iter_descendants(ready.entity))
        .collect();
    let meshes: Vec<(Entity, SkinnedMesh)> = scene_entities
        .iter()
        .filter_map(|entity| {
            skinned
                .get(*entity)
                .ok()
                .map(|skinned| (*entity, skinned.clone()))
        })
        .collect();
    if meshes.is_empty() {
        commands
            .entity(ready.entity)
            .insert(ChildOf(skeleton.head_bone));
        return;
    }

    let mut missing = 0usize;
    let mut rehosted = Vec::with_capacity(meshes.len());
    for (entity, skin) in meshes {
        let mut joints = Vec::with_capacity(skin.joints.len());
        for joint in &skin.joints {
            match names
                .get(*joint)
                .ok()
                .and_then(|name| skeleton.bones.get(name.as_str()))
            {
                Some(body_bone) => joints.push(*body_bone),
                None => missing += 1,
            }
        }
        rehosted.push((
            entity,
            SkinnedMesh {
                inverse_bindposes: skin.inverse_bindposes,
                joints,
            },
        ));
    }
    if missing > 0 {
        report(&ActorHeadDiagnostic::UnresolvedJoints {
            base_form_id: instance.base_form_id,
            joints: missing,
        });
        // Never bind a joint to a wrong bone, and never leave a skinless-looking copy drawing at
        // the body's origin: the actor stays alive without a head.
        commands.entity(ready.entity).despawn();
        return;
    }
    for (entity, skin) in rehosted {
        commands.entity(entity).insert(skin);
    }
}

/// The body's name-to-entity map and its [`HEAD_BONE`] entity, from the same walk the animation
/// targets use.
///
/// The walk starts at the body's `Creation-to-glTF basis` wrapper and only sees the body while the
/// head scene has not been instantiated yet, which is why the result is cached on the actor.
pub(crate) fn body_skeleton(
    actor: Entity,
    names: &Query<&Name>,
    children: &Query<&Children>,
) -> Option<(HashMap<String, Entity>, Entity)> {
    let targets = animation_targets(actor, names, children).ok()?;
    let bones: HashMap<String, Entity> = targets
        .into_iter()
        .filter_map(|target| {
            names
                .get(target.entity)
                .ok()
                .map(|name| (name.as_str().to_owned(), target.entity))
        })
        .collect();
    let head_bone = *bones.get(HEAD_BONE)?;
    Some((bones, head_bone))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::{ActorResolution, ActorSex};
    use bevy::{
        asset::AssetPlugin,
        ecs::{schedule::SingleThreadedExecutor, system::RunSystemOnce},
        log::tracing_subscriber::{
            Registry,
            layer::{Layer, SubscriberExt},
        },
        mesh::skinning::SkinnedMeshInverseBindposes,
        world_serialization::WorldSerializationPlugin,
    };
    use std::{
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };
    use tracing::{Event, Level, Subscriber};

    const FACE_NIF: &str = "meshes/actors/character/facegendata/facegeom/skyrim.esm/00013475.nif";

    fn fixture_app() -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin {
                file_path: directory.path().to_string_lossy().into_owned(),
                ..default()
            },
            WorldSerializationPlugin,
        ))
        .init_asset::<SkinnedMeshInverseBindposes>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<GlobalTransform>()
        .register_type::<Name>()
        .register_type::<Transform>()
        .register_type::<SkinnedMesh>()
        .add_plugins(ActorHeadPlugin);
        (directory, app)
    }

    /// A body scene as the converter lays it out: wrapper, spine, and head bone.
    fn fixture_body_world(with_head: bool) -> World {
        let mut world = World::new();
        let wrapper = world
            .spawn((Name::new("Creation-to-glTF basis"), Transform::default()))
            .id();
        let spine = world
            .spawn((
                Name::new("NPC Spine [Spn0]"),
                Transform::default(),
                ChildOf(wrapper),
            ))
            .id();
        if with_head {
            world.spawn((Name::new(HEAD_BONE), Transform::default(), ChildOf(spine)));
        }
        world
    }

    fn skinned_head_world(inverse_bindposes: Handle<SkinnedMeshInverseBindposes>) -> World {
        let mut world = World::new();
        let joints: Vec<Entity> = [HEAD_BONE, "NPC Spine [Spn0]"]
            .into_iter()
            .map(|name| world.spawn((Name::new(name), Transform::default())).id())
            .collect();
        world.spawn((
            Name::new("Head Mesh"),
            SkinnedMesh {
                inverse_bindposes,
                joints,
            },
            Transform::default(),
        ));
        world
    }

    fn joint_missing_from_body_head_world(
        inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    ) -> World {
        let mut world = World::new();
        let joints: Vec<Entity> = [HEAD_BONE, "NPC Missing [Bone]"]
            .into_iter()
            .map(|name| world.spawn((Name::new(name), Transform::default())).id())
            .collect();
        world.spawn((
            Name::new("Head Mesh"),
            SkinnedMesh {
                inverse_bindposes,
                joints,
            },
            Transform::default(),
        ));
        world
    }

    fn skinless_head_world() -> World {
        let mut world = World::new();
        let root = world
            .spawn((Name::new("00013475.NIF"), Transform::default()))
            .id();
        world.spawn((Name::new("Head Mesh"), Transform::default(), ChildOf(root)));
        world
    }

    fn actor_bundle(head: ActorHead) -> impl Bundle {
        (
            Actor::new(0x13475, 79686, ActorSex::Male, ActorResolution::BodyHead),
            head,
        )
    }

    fn spawn_actor_with_body(app: &mut App, body: Handle<WorldAsset>, head: ActorHead) -> Entity {
        let actor = app.world_mut().spawn(actor_bundle(head)).id();
        app.world_mut()
            .entity_mut(actor)
            .insert(WorldAssetRoot(body));
        app.update();
        actor
    }

    fn skeleton_of(app: &mut App, actor: Entity) -> (HashMap<String, Entity>, Entity) {
        app.world_mut()
            .run_system_once(move |names: Query<&Name>, children: Query<&Children>| {
                body_skeleton(actor, &names, &children)
            })
            .unwrap()
            .expect("the body skeleton")
    }

    fn head_instance(app: &mut App, actor: Entity) -> Option<Entity> {
        let mut query = app.world_mut().query::<(Entity, &ActorHeadInstance)>();
        query
            .iter(app.world())
            .find(|(_, instance)| instance.actor == actor)
            .map(|(entity, _)| entity)
    }

    fn spawn_head_instance(app: &mut App, actor: Entity, head_world: World) -> Handle<WorldAsset> {
        let head_asset = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(head_world));
        let (bones, head_bone) = skeleton_of(app, actor);
        app.world_mut().entity_mut(actor).insert((
            ActorHeadSkeleton { head_bone, bones },
            ActorHeadRequest {
                asset: head_asset.clone(),
                path: "test/head.glb".to_owned(),
            },
            ActorHeadResolved,
        ));
        head_asset
    }

    fn all_skins(app: &mut App) -> Vec<(Entity, SkinnedMesh)> {
        let mut query = app.world_mut().query::<(Entity, &SkinnedMesh)>();
        query
            .iter(app.world())
            .map(|(entity, skin)| (entity, skin.clone()))
            .collect()
    }

    /// Every skinned mesh under the actor's head instance, at any depth.
    fn instance_skins(app: &mut App, actor: Entity) -> Vec<(Entity, SkinnedMesh)> {
        let Some(instance) = head_instance(app, actor) else {
            return Vec::new();
        };
        let mut children = {
            let world = app.world_mut();
            world.query::<&Children>()
        };
        let mut skins = {
            let world = app.world_mut();
            world.query::<(Entity, &SkinnedMesh)>()
        };
        let world = app.world();
        let mut stack = vec![instance];
        let mut found = Vec::new();
        while let Some(entity) = stack.pop() {
            if let Ok((entity, skin)) = skins.get(world, entity) {
                found.push((entity, skin.clone()));
            }
            if let Ok(kids) = children.get(world, entity) {
                stack.extend(kids.iter());
            }
        }
        found
    }

    #[derive(Clone, Default)]
    struct WarningCount(Arc<AtomicUsize>);

    impl<S: Subscriber> Layer<S> for WarningCount {
        fn on_event(
            &self,
            event: &Event<'_>,
            _context: bevy::log::tracing_subscriber::layer::Context<'_, S>,
        ) {
            let metadata = event.metadata();
            if *metadata.level() == Level::WARN && metadata.target() == "engine::actor_head" {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn count_warnings(app: &mut App, frames: usize) -> usize {
        app.edit_schedule(Update, |schedule| {
            schedule.set_executor(SingleThreadedExecutor::new());
        });
        let warnings = WarningCount::default();
        let subscriber = Registry::default().with(warnings.clone());
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..frames {
                app.update();
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        warnings.0.load(Ordering::Relaxed)
    }

    #[test]
    fn a_skinned_head_is_rehosted_onto_the_body_bones() {
        let (_directory, mut app) = fixture_app();
        let body = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(fixture_body_world(true)));
        let actor = spawn_actor_with_body(
            &mut app,
            body,
            ActorHead::new(FACE_NIF, ActorHeadSource::Facegen).unwrap(),
        );
        let (bones, head_bone) = skeleton_of(&mut app, actor);
        let spine = *bones.get("NPC Spine [Spn0]").unwrap();
        assert_eq!(head_bone, *bones.get(HEAD_BONE).unwrap());

        let inverse_bindposes = app
            .world_mut()
            .resource_mut::<Assets<SkinnedMeshInverseBindposes>>()
            .add(SkinnedMeshInverseBindposes::from(vec![Mat4::IDENTITY; 2]));
        let head_asset = spawn_head_instance(
            &mut app,
            actor,
            skinned_head_world(inverse_bindposes.clone()),
        );
        for _ in 0..4 {
            app.update();
        }

        let instance = head_instance(&mut app, actor).expect("the head instance");
        assert!(
            app.world()
                .get::<WorldAssetRoot>(instance)
                .is_some_and(|root| root.0 == head_asset),
            "the instance draws the head asset"
        );
        let skins = instance_skins(&mut app, actor);
        assert_eq!(skins.len(), 1, "the head carries one skin");
        assert_eq!(
            skins[0].1.joints,
            vec![head_bone, spine],
            "each head joint is the body bone of the same name"
        );
        assert_eq!(
            skins[0].1.inverse_bindposes, inverse_bindposes,
            "the inverse bind poses are untouched"
        );
        assert_eq!(
            app.world().get::<ChildOf>(instance).map(ChildOf::parent),
            Some(actor),
            "a skinned head stays a child of the actor root"
        );
    }

    #[test]
    fn a_skinless_head_is_parented_under_the_head_bone() {
        let (_directory, mut app) = fixture_app();
        let body = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(fixture_body_world(true)));
        let actor = spawn_actor_with_body(
            &mut app,
            body,
            ActorHead::new(FACE_NIF, ActorHeadSource::Facegen).unwrap(),
        );
        let (_, head_bone) = skeleton_of(&mut app, actor);

        spawn_head_instance(&mut app, actor, skinless_head_world());
        for _ in 0..4 {
            app.update();
        }

        let instance = head_instance(&mut app, actor).expect("the head instance");
        assert_eq!(
            app.world().get::<ChildOf>(instance).map(ChildOf::parent),
            Some(head_bone),
            "a skinless head is parented under NPC Head [Head]"
        );
    }

    #[test]
    fn a_body_without_the_head_bone_is_a_typed_diagnostic() {
        let (_directory, mut app) = fixture_app();
        let body = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(fixture_body_world(false)));
        let actor = spawn_actor_with_body(
            &mut app,
            body,
            ActorHead::new(FACE_NIF, ActorHeadSource::Facegen).unwrap(),
        );
        app.world_mut().entity_mut(actor).insert(ActorBodyReady);

        let warnings = count_warnings(&mut app, 4);

        assert_eq!(warnings, 1, "the missing bone is reported once");
        assert!(app.world().get::<ActorHeadRequest>(actor).is_none());
        assert!(app.world().get::<ActorHeadResolved>(actor).is_some());
        assert!(
            app.world().get_entity(actor).is_ok(),
            "the actor stays alive without a head"
        );
        assert!(
            head_instance(&mut app, actor).is_none(),
            "no head scene is spawned"
        );
    }

    #[test]
    fn a_default_head_is_not_treated_as_facegen() {
        let (_directory, mut app) = fixture_app();
        let body = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(fixture_body_world(true)));
        let actor = spawn_actor_with_body(
            &mut app,
            body,
            ActorHead::new(
                "meshes/actors/character/character assets/malehead.nif",
                ActorHeadSource::Default,
            )
            .unwrap(),
        );
        app.world_mut().entity_mut(actor).insert(ActorBodyReady);

        let warnings = count_warnings(&mut app, 4);

        assert_eq!(warnings, 0);
        assert!(app.world().get::<ActorHeadRequest>(actor).is_none());
        assert!(app.world().get::<ActorHeadResolved>(actor).is_some());
        assert!(head_instance(&mut app, actor).is_none());
    }

    #[test]
    fn a_head_joint_without_a_body_bone_is_reported_once_and_left_unattached() {
        let (_directory, mut app) = fixture_app();
        let body = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(fixture_body_world(true)));
        let actor = spawn_actor_with_body(
            &mut app,
            body,
            ActorHead::new(FACE_NIF, ActorHeadSource::Facegen).unwrap(),
        );
        let inverse_bindposes = app
            .world_mut()
            .resource_mut::<Assets<SkinnedMeshInverseBindposes>>()
            .add(SkinnedMeshInverseBindposes::from(vec![Mat4::IDENTITY; 2]));
        spawn_head_instance(
            &mut app,
            actor,
            joint_missing_from_body_head_world(inverse_bindposes),
        );

        let warnings = count_warnings(&mut app, 4);

        assert_eq!(
            warnings, 1,
            "the unresolved joint is reported once per head"
        );
        assert!(
            head_instance(&mut app, actor).is_none(),
            "the head is not bound to a wrong bone"
        );
        assert!(
            all_skins(&mut app).is_empty(),
            "nothing of the discarded head remains"
        );
        assert!(app.world().get_entity(actor).is_ok());
    }

    #[test]
    fn a_missing_head_asset_is_skipped_with_one_notice() {
        let (_directory, mut app) = fixture_app();
        let body = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(fixture_body_world(true)));
        let actor = spawn_actor_with_body(
            &mut app,
            body,
            ActorHead::new(
                "meshes/actors/character/facegendata/facegeom/skyrim.esm/ffffffff.nif",
                ActorHeadSource::Facegen,
            )
            .unwrap(),
        );
        app.world_mut().entity_mut(actor).insert(ActorBodyReady);

        let warnings = count_warnings(&mut app, 60);

        assert_eq!(warnings, 1, "the missing asset is reported once");
        assert!(app.world().get::<ActorHeadRequest>(actor).is_none());
        assert!(head_instance(&mut app, actor).is_none());
        assert!(app.world().get_entity(actor).is_ok());
    }

    #[test]
    fn the_diagnostic_codes_are_stable() {
        let bone = ActorHeadDiagnostic::MissingHeadBone {
            base_form_id: 0x13475,
        };
        assert_eq!(bone.code(), "actor_head_bone_missing");
        assert_eq!(
            bone.to_string(),
            "NPC_ 00013475: the body has no `NPC Head [Head]` bone"
        );
        let asset = ActorHeadDiagnostic::MissingAsset {
            base_form_id: 0x13476,
            path: "missing.nif".to_owned(),
        };
        assert_eq!(asset.code(), "actor_head_asset_missing");
        assert_eq!(
            asset.to_string(),
            "NPC_ 00013476: FaceGen head `missing.nif` has no converted asset"
        );
        let joints = ActorHeadDiagnostic::UnresolvedJoints {
            base_form_id: 0x13477,
            joints: 3,
        };
        assert_eq!(joints.code(), "actor_head_joint_missing");
        assert_eq!(
            joints.to_string(),
            "NPC_ 00013477: 3 head joint(s) have no body bone"
        );
    }

    // ---- Retail asset check ----

    fn retail_app(assets: &Path) -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin {
                file_path: assets.to_string_lossy().into_owned(),
                ..default()
            },
            bevy::gltf::GltfPlugin::default(),
            bevy::animation::AnimationPlugin,
            WorldSerializationPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<GlobalTransform>()
        .register_type::<Name>()
        .register_type::<Transform>()
        .add_plugins((
            crate::actor_animation::ActorAnimationPlugin,
            ActorHeadPlugin,
        ));
        app.finish();
        app.cleanup();
        app
    }

    fn wait_until(app: &mut App, what: &str, mut ready: impl FnMut(&mut App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready(app) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires MUDCRAB_CONVERTED_ASSETS with a composed body and a skinned FaceGen head"]
    fn retail_head_follows_the_animated_body_bone() {
        let Some(assets) =
            std::env::var_os("MUDCRAB_CONVERTED_ASSETS").map(std::path::PathBuf::from)
        else {
            return;
        };
        let body_path = crate::actors::composed_body_scene(ActorSex::Male);
        let head_nif = "meshes/actors/character/facegendata/facegeom/skyrim.esm/0010f5a1.nif";
        let head_path = crate::streaming::converted_model_path(head_nif.to_owned())
            .expect("the FaceGen path converts");
        if !assets.join(body_path).is_file() || !assets.join(&head_path).is_file() {
            return;
        }

        let mut app = retail_app(&assets);
        let body_scene: Handle<WorldAsset> = app
            .world()
            .resource::<AssetServer>()
            .load(GltfAssetLabel::Scene(0).from_asset(body_path));
        let clips = crate::actor_animation::ActorClips::load(
            app.world().resource::<AssetServer>(),
            ActorSex::Male,
        );
        wait_until(&mut app, "the composed body", |app| {
            matches!(
                app.world()
                    .resource::<AssetServer>()
                    .load_state(body_scene.id()),
                LoadState::Loaded
            )
        });
        let actor = app
            .world_mut()
            .spawn((
                Actor::new(0x10f5a1, 79686, ActorSex::Male, ActorResolution::BodyHead),
                ActorHead::new(head_nif, ActorHeadSource::Facegen).unwrap(),
                clips,
                WorldAssetRoot(body_scene),
            ))
            .id();

        wait_until(&mut app, "the head instance", |app| {
            head_instance(app, actor).is_some()
        });
        wait_until(&mut app, "the rehosted skin", |app| {
            !instance_skins(app, actor).is_empty()
        });

        let (bones, head_bone) = skeleton_of(&mut app, actor);
        let head_before = app.world().get::<GlobalTransform>(head_bone).copied();
        // Give the animation a chance to move the body and the rehosted head with it.
        for _ in 0..30 {
            app.update();
            std::thread::sleep(Duration::from_millis(10));
        }

        let body_entities: Vec<Entity> = bones.values().copied().collect();
        let skins = instance_skins(&mut app, actor);
        assert!(!skins.is_empty(), "the head asset carries skinned meshes");
        for (_, skin) in &skins {
            assert!(
                skin.joints
                    .iter()
                    .all(|joint| body_entities.contains(joint)),
                "every head joint is a body bone entity"
            );
            assert!(
                skin.joints.contains(&head_bone),
                "each head skin binds the body's head bone"
            );
        }

        let (skin, head_joint) = skins
            .iter()
            .find_map(|(_, skin)| {
                skin.joints
                    .iter()
                    .position(|joint| *joint == head_bone)
                    .map(|index| (skin, index))
            })
            .expect("a head joint bound to the head bone");
        let joint = skin.joints[head_joint];
        let body_transform = app.world().get::<GlobalTransform>(head_bone).copied();
        let joint_transform = app.world().get::<GlobalTransform>(joint).copied();
        assert_ne!(
            head_before, body_transform,
            "the idle clip moves the body's head bone"
        );
        assert_eq!(
            body_transform, joint_transform,
            "the head mesh's joint transform is the animated head bone transform"
        );
    }
}
