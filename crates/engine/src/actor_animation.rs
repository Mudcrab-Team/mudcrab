//! Idle animation for spawned actors.
//!
//! The composed body scene has no animations of its own, so Bevy's glTF loader never marks its
//! bones as animation targets. The converter writes the idle clip separately over the body's exact
//! hierarchy (PR-3): every node name from the `Creation-to-glTF basis` wrapper down to a bone. This
//! module rebuilds that path over the instantiated body scene and gives every node the same
//! [`AnimationTargetId`] the clip writer used, then plays the clip from an [`AnimationPlayer`] on
//! the actor root.
//!
//! A clip that did not load, a body scene without the wrapper, or a loaded clip without an
//! animation is reported once and leaves the actor spawned in bind pose. Nothing here selects a
//! gait or drives movement.

use crate::actors::{Actor, ActorSex};
use bevy::{
    animation::{
        AnimatedBy, AnimationPlayer, AnimationTargetId,
        prelude::{AnimationGraph, AnimationGraphHandle},
    },
    asset::LoadState,
    gltf::Gltf,
    prelude::*,
    world_serialization::WorldInstanceReady,
};

/// The wrapper the converter puts between the scene root and the body's bone tree.
pub const ANIMATION_ROOT_NODE: &str = "Creation-to-glTF basis";

/// The animation name the converter gives the clip in `mt_idle.glb`.
pub const IDLE_ANIMATION: &str = "mt_idle";

/// The idle clip the converter publishes for `sex`.
pub fn idle_clip_scene(sex: ActorSex) -> &'static str {
    match sex {
        ActorSex::Male => "meshes/actors/character/animations/male/mt_idle.glb",
        ActorSex::Female => "meshes/actors/character/animations/female/mt_idle.glb",
    }
}

/// The idle clip an actor asked for, as a glTF document so its named animation can be looked up.
///
/// The actor root carries this until [`start_actor_idle_animations`] either installs the player or
/// reports that the clip is unusable.
#[derive(Component, Clone, Debug)]
pub struct ActorIdleClip {
    pub gltf: Handle<Gltf>,
}

impl ActorIdleClip {
    /// Loads `sex`'s idle clip through the asset server.
    pub fn load(asset_server: &AssetServer, sex: ActorSex) -> Self {
        Self {
            gltf: asset_server.load(idle_clip_scene(sex)),
        }
    }
}

/// A body scene instance that has been spawned below an actor and can be annotated.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct ActorBodyReady;

/// One node of the body scene and the animation target id the clip addresses it by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationTarget {
    pub entity: Entity,
    pub id: AnimationTargetId,
}

/// Why a body scene cannot be annotated with animation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorAnimationError {
    /// No descendant is named [`ANIMATION_ROOT_NODE`].
    MissingRoot,
}

impl ActorAnimationError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingRoot => "the body scene has no `Creation-to-glTF basis` node",
        }
    }
}

/// Every node from `instance_root`'s [`ANIMATION_ROOT_NODE`] down, paired with the id its full
/// name path produces.
///
/// The path starts at the wrapper and includes it, matching `AnimationTargetId::from_names` in
/// Bevy's glTF loader. A node without a [`Name`] cannot be addressed by the clip and is skipped
/// together with its subtree.
pub fn animation_targets(
    instance_root: Entity,
    names: &Query<&Name>,
    children: &Query<&Children>,
) -> Result<Vec<AnimationTarget>, ActorAnimationError> {
    let root = find_animation_root(instance_root, names, children)
        .ok_or(ActorAnimationError::MissingRoot)?;
    let mut path = Vec::new();
    let mut targets = Vec::new();
    collect_targets(root, &mut path, names, children, &mut targets);
    Ok(targets)
}

fn find_animation_root(
    entity: Entity,
    names: &Query<&Name>,
    children_query: &Query<&Children>,
) -> Option<Entity> {
    if names
        .get(entity)
        .is_ok_and(|name| name.as_str() == ANIMATION_ROOT_NODE)
    {
        return Some(entity);
    }
    let child_entities = children_query.get(entity).ok()?;
    child_entities
        .iter()
        .find_map(|child| find_animation_root(child, names, children_query))
}

fn collect_targets(
    entity: Entity,
    path: &mut Vec<Name>,
    names: &Query<&Name>,
    children_query: &Query<&Children>,
    targets: &mut Vec<AnimationTarget>,
) {
    let Ok(name) = names.get(entity) else {
        return;
    };
    path.push(name.clone());
    targets.push(AnimationTarget {
        entity,
        id: AnimationTargetId::from_names(path.iter()),
    });
    if let Ok(child_entities) = children_query.get(entity) {
        for child in child_entities.iter() {
            collect_targets(child, path, names, children_query, targets);
        }
    }
    path.pop();
}

/// Notes that an actor's body scene instance is ready to be annotated with animation targets.
fn mark_actor_body_ready(
    ready: On<WorldInstanceReady>,
    actors: Query<&Actor>,
    mut commands: Commands,
) {
    if actors.contains(ready.entity) {
        commands.entity(ready.entity).insert(ActorBodyReady);
    }
}

type PendingActorAnimations<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static ActorIdleClip, &'static Actor),
    (With<ActorBodyReady>, Without<AnimationPlayer>),
>;

/// Installs every bone's target id and starts the idle clip once both the body scene and the clip
/// are available.
///
/// A failure is reported once and removes the request, leaving the actor in bind pose.
fn start_actor_idle_animations(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    gltfs: Res<Assets<Gltf>>,
    names: Query<&Name>,
    children: Query<&Children>,
    pending: PendingActorAnimations,
    mut graphs: ResMut<Assets<AnimationGraph>>,
) {
    for (actor, clip, actor_data) in &pending {
        let form_id = format_args!("{:08X}", actor_data.base_form_id);
        match asset_server.load_state(clip.gltf.id()) {
            LoadState::Loaded => {}
            LoadState::Failed(error) => {
                warn!(
                    form_id,
                    %error, "actor idle clip failed to load; the actor stays in bind pose"
                );
                commands.entity(actor).remove::<ActorIdleClip>();
                continue;
            }
            LoadState::NotLoaded | LoadState::Loading => continue,
        }
        let Some(gltf) = gltfs.get(&clip.gltf) else {
            continue;
        };
        let Some(clip_handle) = gltf
            .named_animations
            .get(IDLE_ANIMATION)
            .or_else(|| gltf.animations.first())
        else {
            warn!(
                form_id,
                "actor idle clip has no animation; the actor stays in bind pose"
            );
            commands.entity(actor).remove::<ActorIdleClip>();
            continue;
        };
        let targets = match animation_targets(actor, &names, &children) {
            Ok(targets) => targets,
            Err(error) => {
                warn!(
                    form_id,
                    reason = error.as_str(),
                    "actor idle animation skipped; the actor stays in bind pose"
                );
                commands.entity(actor).remove::<ActorIdleClip>();
                continue;
            }
        };
        for target in &targets {
            commands
                .entity(target.entity)
                .insert((target.id, AnimatedBy(actor)));
        }
        let (graph, animation) = AnimationGraph::from_clip(clip_handle.clone());
        let graph = graphs.add(graph);
        let mut player = AnimationPlayer::default();
        player.play(animation).repeat();
        commands
            .entity(actor)
            .insert((player, AnimationGraphHandle(graph)))
            .remove::<ActorIdleClip>();
    }
}

/// Plays the idle clip on every actor whose body scene and clip are ready.
pub struct ActorAnimationPlugin;

impl Plugin for ActorAnimationPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(mark_actor_body_ready)
            .add_systems(Update, start_actor_idle_animations);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        asset::{AssetPlugin, AssetServer},
        ecs::{schedule::SingleThreadedExecutor, system::RunSystemOnce},
        gltf::GltfNode,
        log::tracing_subscriber::{
            Registry,
            layer::{Layer, SubscriberExt},
        },
        platform::collections::HashMap,
        world_serialization::{WorldAsset, WorldAssetRoot, WorldSerializationPlugin},
    };
    use std::{
        collections::HashSet,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };
    use tracing::{Event, Level, Subscriber};

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
        .init_asset::<Gltf>()
        .init_asset::<AnimationClip>()
        .init_asset::<AnimationGraph>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<GlobalTransform>()
        .register_type::<Name>()
        .register_type::<Transform>()
        .add_plugins(ActorAnimationPlugin);
        (directory, app)
    }

    /// A body scene as the converter lays it out: the wrapper, a bone, and a child bone.
    fn fixture_body_world() -> World {
        let mut world = World::new();
        let wrapper = world
            .spawn((Name::new(ANIMATION_ROOT_NODE), Transform::default()))
            .id();
        let spine = world
            .spawn((
                Name::new("NPC Spine [Spn0]"),
                Transform::default(),
                ChildOf(wrapper),
            ))
            .id();
        world.spawn((
            Name::new("NPC Head [Head]"),
            Transform::default(),
            ChildOf(spine),
        ));
        world
    }

    fn gltf_with_animation(clip: Handle<AnimationClip>) -> Gltf {
        Gltf {
            scenes: Vec::new(),
            named_scenes: HashMap::new(),
            meshes: Vec::new(),
            named_meshes: HashMap::new(),
            materials: Vec::new(),
            named_materials: HashMap::new(),
            nodes: Vec::new(),
            named_nodes: HashMap::new(),
            skins: Vec::new(),
            named_skins: HashMap::new(),
            default_scene: None,
            animations: vec![clip.clone()],
            named_animations: HashMap::from([(IDLE_ANIMATION.into(), clip)]),
            source: None,
        }
    }

    fn spawn_actor(app: &mut App, scene: Handle<WorldAsset>, clip: Handle<Gltf>) -> Entity {
        app.world_mut()
            .spawn((
                Actor::new(
                    0x13475,
                    79686,
                    ActorSex::Male,
                    crate::actors::ActorResolution::Body,
                ),
                ActorIdleClip { gltf: clip },
                WorldAssetRoot(scene),
            ))
            .id()
    }

    fn expected_id(names: &[&str]) -> AnimationTargetId {
        let names: Vec<Name> = names
            .iter()
            .map(|name| Name::new((*name).to_owned()))
            .collect();
        AnimationTargetId::from_names(names.iter())
    }

    #[test]
    fn target_paths_start_at_the_wrapper_and_match_loader_ids() {
        let (_directory, mut app) = fixture_app();
        let reference = app.world_mut().spawn(Name::new("Reference 00013475")).id();
        let decoy = app
            .world_mut()
            .spawn((
                Name::new("Other basis"),
                Transform::default(),
                ChildOf(reference),
            ))
            .id();
        app.world_mut().spawn((
            Name::new("NPC Spine [Spn0]"),
            Transform::default(),
            ChildOf(decoy),
        ));
        let wrapper = app
            .world_mut()
            .spawn((
                Name::new(ANIMATION_ROOT_NODE),
                Transform::default(),
                ChildOf(reference),
            ))
            .id();
        let spine = app
            .world_mut()
            .spawn((
                Name::new("NPC Spine [Spn0]"),
                Transform::default(),
                ChildOf(wrapper),
            ))
            .id();
        let head = app
            .world_mut()
            .spawn((
                Name::new("NPC Head [Head]"),
                Transform::default(),
                ChildOf(spine),
            ))
            .id();

        let targets = app
            .world_mut()
            .run_system_once(move |names: Query<&Name>, children: Query<&Children>| {
                animation_targets(reference, &names, &children)
            })
            .unwrap()
            .expect("the fixture has an animation root");

        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].entity, wrapper);
        assert_eq!(targets[0].id, expected_id(&[ANIMATION_ROOT_NODE]));
        assert_eq!(
            targets[1].id,
            expected_id(&[ANIMATION_ROOT_NODE, "NPC Spine [Spn0]"])
        );
        assert_eq!(
            targets[2].id,
            expected_id(&[ANIMATION_ROOT_NODE, "NPC Spine [Spn0]", "NPC Head [Head]"])
        );
        assert_eq!(targets[1].entity, spine);
        assert_eq!(targets[2].entity, head);
    }

    #[test]
    fn a_missing_wrapper_is_a_typed_error() {
        let (_directory, mut app) = fixture_app();
        let reference = app.world_mut().spawn(Name::new("Reference")).id();
        app.world_mut().spawn((
            Name::new("NPC Spine [Spn0]"),
            Transform::default(),
            ChildOf(reference),
        ));

        let error = app
            .world_mut()
            .run_system_once(move |names: Query<&Name>, children: Query<&Children>| {
                animation_targets(reference, &names, &children)
            })
            .unwrap()
            .unwrap_err();
        assert_eq!(error, ActorAnimationError::MissingRoot);
        assert_eq!(
            error.as_str(),
            "the body scene has no `Creation-to-glTF basis` node"
        );
    }

    #[test]
    fn a_ready_body_scene_gets_a_player_and_targets_on_every_bone() {
        let (_directory, mut app) = fixture_app();
        let clip = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(AnimationClip::default());
        let clip_gltf = app
            .world()
            .resource::<AssetServer>()
            .add(gltf_with_animation(clip));
        app.update();
        let scene = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(fixture_body_world()));
        app.update();

        let actor = spawn_actor(&mut app, scene, clip_gltf);
        for _ in 0..8 {
            app.update();
        }

        assert!(app.world().get::<AnimationPlayer>(actor).is_some());
        assert!(app.world().get::<AnimationGraphHandle>(actor).is_some());
        assert!(app.world().get::<ActorIdleClip>(actor).is_none());

        let mut query = app
            .world_mut()
            .query::<(&Name, &AnimationTargetId, &AnimatedBy)>();
        let targets: Vec<_> = query
            .iter(app.world())
            .map(|(name, id, animated_by)| (name.to_string(), *id, animated_by.0))
            .collect();
        assert_eq!(targets.len(), 3, "wrapper and both bones are targets");
        assert!(
            targets.iter().all(|(_, _, player)| *player == actor),
            "every target names the actor carrying the player"
        );
        assert_eq!(
            targets
                .iter()
                .find(|(name, _, _)| name == "NPC Head [Head]"),
            Some(&(
                "NPC Head [Head]".to_owned(),
                expected_id(&[ANIMATION_ROOT_NODE, "NPC Spine [Spn0]", "NPC Head [Head]"]),
                actor
            ))
        );
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
            if *metadata.level() == Level::WARN && metadata.target() == "engine::actor_animation" {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn a_missing_clip_warns_once_and_leaves_the_actor_in_bind_pose() {
        let (_directory, mut app) = fixture_app();
        app.edit_schedule(Update, |schedule| {
            schedule.set_executor(SingleThreadedExecutor::new());
        });
        let scene = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(fixture_body_world()));
        app.update();
        let missing: Handle<Gltf> = app
            .world()
            .resource::<AssetServer>()
            .load(idle_clip_scene(ActorSex::Male));
        let actor = spawn_actor(&mut app, scene, missing);

        let warnings = WarningCount::default();
        let subscriber = Registry::default().with(warnings.clone());
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..60 {
                app.update();
                std::thread::sleep(Duration::from_millis(2));
            }
        });

        assert_eq!(
            warnings.0.load(Ordering::Relaxed),
            1,
            "the missing clip is reported exactly once"
        );
        assert!(
            app.world().get_entity(actor).is_ok(),
            "the actor stays alive"
        );
        assert!(
            app.world().get::<AnimationPlayer>(actor).is_none(),
            "no player is installed without a clip"
        );
        assert!(app.world().get::<ActorIdleClip>(actor).is_none());
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
        .add_plugins(ActorAnimationPlugin);
        app.finish();
        app.cleanup();
        app
    }

    fn wait_until(app: &mut App, what: &str, mut ready: impl FnMut(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready(app) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Copies the body's node hierarchy into a fresh world, keeping only what animation needs:
    /// names, local transforms, and parent/child relations.
    fn copy_node(
        nodes: &Assets<GltfNode>,
        handle: &Handle<GltfNode>,
        world: &mut World,
        parent: Option<Entity>,
    ) -> Option<Entity> {
        let node = nodes.get(handle)?;
        let mut entity = world.spawn((Name::new(node.name.clone()), node.transform));
        if let Some(parent) = parent {
            entity.insert(ChildOf(parent));
        }
        let entity = entity.id();
        for child in &node.children {
            let _ = copy_node(nodes, child, world, Some(entity));
        }
        Some(entity)
    }

    #[test]
    #[ignore = "requires MUDCRAB_CONVERTED_ASSETS with a composed body and mt_idle.glb"]
    fn retail_idle_clip_moves_a_bone_without_unbound_targets() {
        let Some(assets) =
            std::env::var_os("MUDCRAB_CONVERTED_ASSETS").map(std::path::PathBuf::from)
        else {
            return;
        };
        let body_path = crate::actors::composed_body_scene(ActorSex::Male);
        let clip_path = idle_clip_scene(ActorSex::Male);
        if !assets.join(body_path).is_file() || !assets.join(clip_path).is_file() {
            return;
        }

        let warnings = WarningCount::default();
        let subscriber = Registry::default().with(warnings.clone());
        tracing::subscriber::with_default(subscriber, || {
            let mut app = retail_app(&assets);
            let body: Handle<Gltf> = app.world().resource::<AssetServer>().load(body_path);
            let clip: Handle<Gltf> = app.world().resource::<AssetServer>().load(clip_path);
            wait_until(&mut app, "the composed body", |app| {
                matches!(
                    app.world().resource::<AssetServer>().load_state(body.id()),
                    LoadState::Loaded
                )
            });
            wait_until(&mut app, "the idle clip", |app| {
                matches!(
                    app.world().resource::<AssetServer>().load_state(clip.id()),
                    LoadState::Loaded
                )
            });

            // Build the animation-relevant part of the real body hierarchy from the loaded nodes.
            let wrapper = {
                let gltfs = app.world().resource::<Assets<Gltf>>();
                let gltf = gltfs.get(&body).expect("the body glTF");
                gltf.named_nodes
                    .get(ANIMATION_ROOT_NODE)
                    .cloned()
                    .expect("the composed body carries the animation root")
            };
            let mut body_world = World::new();
            {
                let nodes = app.world().resource::<Assets<GltfNode>>();
                copy_node(nodes, &wrapper, &mut body_world, None)
                    .expect("the body's animation root node");
            }
            let scene = app
                .world()
                .resource::<AssetServer>()
                .add(WorldAsset::new(body_world));
            app.update();
            let actor = spawn_actor(&mut app, scene, clip.clone());

            wait_until(&mut app, "the actor's animation player", |app| {
                app.world().get::<AnimationPlayer>(actor).is_some()
            });

            let clip_targets: HashSet<AnimationTargetId> = {
                let clips = app.world().resource::<Assets<AnimationClip>>();
                let gltfs = app.world().resource::<Assets<Gltf>>();
                let handle = gltfs
                    .get(&clip)
                    .and_then(|gltf| {
                        gltf.named_animations
                            .get(IDLE_ANIMATION)
                            .or_else(|| gltf.animations.first())
                    })
                    .expect("the loaded idle clip");
                clips
                    .get(handle)
                    .expect("the idle clip asset")
                    .curves()
                    .keys()
                    .copied()
                    .collect()
            };

            let mut targets = app.world_mut().query::<&AnimationTargetId>();
            let bound: HashSet<AnimationTargetId> = targets.iter(app.world()).copied().collect();
            assert!(
                clip_targets.is_subset(&bound),
                "every clip target is bound to a bone"
            );

            let mut transform_query = app
                .world_mut()
                .query_filtered::<(Entity, &Transform), With<AnimationTargetId>>();
            let initial: Vec<(Entity, Transform)> = transform_query
                .iter(app.world())
                .map(|(entity, transform)| (entity, *transform))
                .collect();
            assert!(!initial.is_empty(), "the body has animation targets");

            for _ in 0..30 {
                app.update();
                std::thread::sleep(Duration::from_millis(10));
            }
            let moved = initial.iter().any(|(entity, before)| {
                app.world()
                    .get::<Transform>(*entity)
                    .is_some_and(|after| after != before)
            });
            assert!(moved, "the idle clip changes at least one bone transform");

            let warnings_seen = warnings.0.load(Ordering::Relaxed);
            assert_eq!(warnings_seen, 0, "no animation warning is emitted");
        });
    }
}
