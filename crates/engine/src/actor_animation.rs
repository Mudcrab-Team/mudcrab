//! Gait animation for spawned actors.
//!
//! The composed body scene has no animations of its own, so Bevy's glTF loader never marks its
//! bones as animation targets. The converter writes each gait clip separately over the body's
//! exact hierarchy: every node name from the `Creation-to-glTF basis` wrapper down to a bone. This
//! module rebuilds that path over the instantiated body scene and gives every node the same
//! [`AnimationTargetId`] the clip writer used, then installs one [`AnimationPlayer`] on the actor
//! root whose graph holds the idle, walk, and run clips.
//!
//! A clip that did not load, a body scene without the wrapper, or a loaded clip without an
//! animation is reported once. A missing idle clip leaves the actor in bind pose; a missing walk or
//! run clip falls back to the nearest gait that did load. Nothing here selects a gait or drives
//! movement; see [`crate::actor_drive`].

use crate::{
    actor_drive::{ActorGait, ActorGaitNodes, Gait},
    actors::{Actor, ActorSex},
    world::actor_catalog::IdlePoolClip,
};
use bevy::{
    animation::{
        AnimatedBy, AnimationPlayer, AnimationTargetId,
        prelude::{AnimationGraph, AnimationGraphHandle, AnimationTransitions},
    },
    asset::LoadState,
    gltf::Gltf,
    prelude::*,
    world_serialization::WorldInstanceReady,
};
use std::time::Duration;

/// The wrapper the converter puts between the scene root and the body's bone tree.
pub const ANIMATION_ROOT_NODE: &str = "Creation-to-glTF basis";

/// The animation name the converter gives the idle clip in `mt_idle.glb`.
pub const IDLE_ANIMATION: &str = "mt_idle";

/// The animation name the converter gives the walk clip in `mt_walkforward.glb`.
pub const WALK_ANIMATION: &str = "mt_walkforward";

/// The animation name the converter gives the run clip in `mt_runforward.glb`.
pub const RUN_ANIMATION: &str = "mt_runforward";

/// The clip the converter publishes for `sex` and `gait`.
pub fn gait_clip_scene(sex: ActorSex, gait: Gait) -> &'static str {
    match (sex, gait) {
        (ActorSex::Male, Gait::Idle) => "meshes/actors/character/animations/male/mt_idle.glb",
        (ActorSex::Male, Gait::Walk) => {
            "meshes/actors/character/animations/male/mt_walkforward.glb"
        }
        (ActorSex::Male, Gait::Run) => "meshes/actors/character/animations/male/mt_runforward.glb",
        (ActorSex::Female, Gait::Idle) => "meshes/actors/character/animations/female/mt_idle.glb",
        (ActorSex::Female, Gait::Walk) => {
            "meshes/actors/character/animations/female/mt_walkforward.glb"
        }
        (ActorSex::Female, Gait::Run) => {
            "meshes/actors/character/animations/female/mt_runforward.glb"
        }
    }
}

/// The idle clip the converter publishes for `sex`.
pub fn idle_clip_scene(sex: ActorSex) -> &'static str {
    gait_clip_scene(sex, Gait::Idle)
}

/// The animation name the converter gives `gait`'s clip.
pub fn gait_animation_name(gait: Gait) -> &'static str {
    match gait {
        Gait::Idle => IDLE_ANIMATION,
        Gait::Walk => WALK_ANIMATION,
        Gait::Run => RUN_ANIMATION,
    }
}

/// The three gait clips an actor asked for, as glTF documents so their named animations can be
/// looked up.
///
/// The actor root carries this until [`start_actor_animations`] either installs the player or
/// reports which clips are unusable.
#[derive(Component, Clone, Debug)]
pub struct ActorClips {
    pub idle: Handle<Gltf>,
    pub walk: Handle<Gltf>,
    pub run: Handle<Gltf>,
}

/// Body identity for an actor whose converter-authenticated idle library is available.
#[derive(Component, Clone, Debug)]
pub struct ActorIdleLibrary {
    pub body_scene: String,
}

/// Select a canonical source HKX key from this creature's authored idle library.
/// This is a playback request, independent of behavior-graph events or AI policy.
#[derive(Component, Clone, Debug)]
pub struct AuthoredIdleRequest {
    pub source: String,
    pub repeat: bool,
}

#[derive(Component)]
struct IdlePlayback {
    repeat: bool,
}

/// A creature's stand-in for the retail behaviour graph's idle variety.
///
/// **This is a placeholder, not the retail rule.** Retail's engine walks the `ActionIdle` tree for
/// the stand idle and sends the chosen event to the actor's behaviour graph (RF-IDLE-005). Inside
/// the stand state the graph's own layers keep re-picking. The verified graph data is: the dog head
/// layer (bark, head 1 to 4, base) and tail layer (tail 1 to 3, base) re-pick with equal weights
/// when their clip ends (RF-IDLE-018), and so does the cow tail wag (3 clips, RF-IDLE-019). The
/// clips run 1.0 to 5.0 s, so the time between picks is the clip length and no separate timer
/// exists. Retail blends those layers over a base loop at the same time. Mudcrab plays one clip at
/// a time, so this component folds the layers into one sequence:
///
/// - Only the verified families get a sequence (`VERIFIED_IDLE_LAYERS` in the actor catalog: dogs
///   and cows). Every other creature keeps looping its default catalog clip.
/// - The actor starts on its catalog default clip, the base. Every clip plays once, to its end. No
///   timer decides when to change.
/// - When a clip finishes, [`advance_idle_sequences`] picks the next clip from the pool with equal
///   weights. The pick may be the clip that just finished, as it may be on the retail tail layers.
/// - The pick comes from a small generator seeded by the placed reference's FormID. The same
///   reference always plays the same sequence, whatever the frame rate.
/// - The pool's clips load when the actor spawns and stay loaded. If the next clip is not ready
///   when the current one ends, the creature holds its last pose and waits. A clip that fails to
///   load is dropped from the pool, and the sequence goes on with the rest.
///
/// Not done here, on purpose: the look, call, sit, lie-down, feed and forage variants of dogs and
/// cows. Retail takes those from the engine's `IDLE` tree (RF-IDLE-020): a depth-first first-match
/// walk with conditions (RF-IDLE-006), `GetRandomPercent` draws (RF-IDLE-004) and a replay-delay
/// list (RF-IDLE-007), not from the graph. That is a follow-up. Humans are a later step too: the
/// engine walks the `ActionIdle` tree for them (RF-IDLE-022) and their graph has no random
/// re-pick (RF-IDLE-023).
///
/// Known limit: each change still rebuilds the player's graph and the bone targets of the body. That
/// work happens once per clip end, every 1 to 5 s per creature. It has not been measured in a cell
/// with many creatures.
///
/// An actor with fewer than two pool clips gets no `IdleSequence` and keeps looping its default
/// clip, exactly as before.
#[derive(Component, Debug, Clone)]
pub struct IdleSequence {
    pool: Vec<IdlePoolClip>,
    /// The clips of `pool`, loaded at spawn and kept. Empty when nothing was preloaded.
    clips: Vec<Handle<Gltf>>,
    /// Clips that failed to load or to start.
    failed: Vec<bool>,
    current: usize,
    /// The pick that waits for its clip to finish loading.
    pending: Option<usize>,
    state: u64,
}

/// Whether the clip of a pick can start now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipReadiness {
    Ready,
    Loading,
    Failed,
}

impl IdleSequence {
    /// A sequence over `pool` (the base clip first) for one placed reference.
    ///
    /// Returns `None` when `pool` has fewer than two clips, so there is nothing to vary.
    pub fn new(pool: Vec<IdlePoolClip>, reference_form_id: u32) -> Option<Self> {
        (pool.len() >= 2).then(|| Self {
            failed: vec![false; pool.len()],
            pool,
            clips: Vec::new(),
            current: 0,
            pending: None,
            // A fixed salt keeps FormID 0 away from the generator's all-zero state.
            state: u64::from(reference_form_id) ^ 0x1D1E_5EED_0000_0000,
        })
    }

    /// Starts loading every clip of the pool and keeps the handles, so a change never waits on disk.
    pub fn preload(mut self, asset_server: &AssetServer) -> Self {
        self.clips = self
            .pool
            .iter()
            .map(|clip| asset_server.load(clip.scene.clone()))
            .collect();
        self
    }

    /// The source key of the clip that is playing now.
    pub fn current_source(&self) -> &str {
        &self.pool[self.current].source
    }

    /// The position of the playing clip in the pool, and the pool size.
    pub fn position(&self) -> (usize, usize) {
        (self.current, self.pool.len())
    }

    /// True while the next clip is still loading and the creature holds its last pose.
    pub fn is_waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// SplitMix64: one 64-bit step of a small, well-mixed generator.
    fn next_random(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The next clip: uniform over the clips that did not fail, the playing clip included.
    /// `None` when every clip failed.
    fn choose(&mut self) -> Option<usize> {
        let usable: Vec<usize> = (0..self.pool.len())
            .filter(|index| !self.failed[*index])
            .collect();
        if usable.is_empty() {
            return None;
        }
        Some(usable[(self.next_random() % usable.len() as u64) as usize])
    }

    fn readiness(&self, index: usize, asset_server: &AssetServer) -> ClipReadiness {
        let Some(handle) = self.clips.get(index) else {
            return ClipReadiness::Ready;
        };
        match asset_server.load_state(handle.id()) {
            LoadState::Loaded => ClipReadiness::Ready,
            LoadState::Failed(_) => ClipReadiness::Failed,
            LoadState::NotLoaded | LoadState::Loading => ClipReadiness::Loading,
        }
    }

    /// Test hook for sibling modules: moves to the next pick without a running player.
    #[cfg(test)]
    pub(crate) fn advance_for_test(&mut self) {
        if let Some(next) = self.choose() {
            self.current = next;
        }
    }
}

/// Asks for the next clip of every [`IdleSequence`] whose current clip has finished.
///
/// A new request replaces the player, so this waits until the previous request has loaded
/// (`Without<ActorClips>`) and the new player exists. The next clip is already loaded in the usual
/// case. If it is not, the creature holds its last pose until it is. A clip that failed to load is
/// marked and another pick is made.
fn advance_idle_sequences(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut sequences: Query<
        (Entity, &Actor, &mut IdleSequence, &AnimationPlayer),
        Without<ActorClips>,
    >,
) {
    for (entity, actor, mut sequence, player) in &mut sequences {
        if !player.all_finished() {
            continue;
        }
        let Some(next) = sequence.pending.or_else(|| sequence.choose()) else {
            warn!(
                base_form_id = actor.base_form_id,
                "idle sequence: no clip could be loaded; the actor keeps looping its last clip"
            );
            commands.entity(entity).remove::<IdleSequence>();
            continue;
        };
        match sequence.readiness(next, &asset_server) {
            ClipReadiness::Loading => sequence.pending = Some(next),
            ClipReadiness::Failed => {
                warn!(
                    base_form_id = actor.base_form_id,
                    source = %sequence.pool[next].source,
                    "idle sequence: a clip failed to load; it leaves the pool"
                );
                sequence.failed[next] = true;
                sequence.pending = None;
            }
            ClipReadiness::Ready => {
                sequence.pending = None;
                sequence.current = next;
                let source = sequence.pool[next].source.clone();
                debug!(
                    base_form_id = actor.base_form_id,
                    %source,
                    "idle sequence: next clip"
                );
                commands.entity(entity).insert(AuthoredIdleRequest {
                    source,
                    repeat: false,
                });
            }
        }
    }
}

/// Sequences whose body is ready but that have neither a player nor a clip request pending.
type StalledIdleSequences<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static Actor, &'static mut IdleSequence),
    (
        With<ActorBodyReady>,
        Without<AnimationPlayer>,
        Without<ActorClips>,
    ),
>;

/// Continues a sequence whose clip did not start.
///
/// `start_actor_animations` removes the clip request without installing a player when the clip has
/// no animation or the body has no animation root. Without a player [`advance_idle_sequences`]
/// never runs again. This marks the clip as failed and asks for another one, so one bad clip cannot
/// stop the sequence. When every clip failed, the sequence is removed.
fn recover_idle_sequences(mut commands: Commands, mut sequences: StalledIdleSequences) {
    for (entity, actor, mut sequence) in &mut sequences {
        let failed = sequence.current;
        warn!(
            base_form_id = actor.base_form_id,
            source = %sequence.pool[failed].source,
            "idle sequence: a clip did not start; it leaves the pool"
        );
        sequence.failed[failed] = true;
        sequence.pending = None;
        let Some(next) = sequence.choose() else {
            warn!(
                base_form_id = actor.base_form_id,
                "idle sequence: no clip could start; the actor keeps its last pose"
            );
            commands.entity(entity).remove::<IdleSequence>();
            continue;
        };
        sequence.current = next;
        let source = sequence.pool[next].source.clone();
        commands.entity(entity).insert(AuthoredIdleRequest {
            source,
            repeat: false,
        });
    }
}

type PendingIdleRequests<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Actor,
        &'static ActorIdleLibrary,
        &'static AuthoredIdleRequest,
    ),
    Changed<AuthoredIdleRequest>,
>;

fn select_authored_idle(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    catalog: Option<Res<crate::world::actor_catalog::ActorCatalog>>,
    requests: PendingIdleRequests,
) {
    let Some(catalog) = catalog else { return };
    for (entity, actor, library, request) in &requests {
        let Some(scene) = catalog.authored_idle_scene(actor, &library.body_scene, &request.source)
        else {
            warn!(base_form_id = actor.base_form_id, source = %request.source,
                "authored idle request has no compatible published clip");
            continue;
        };
        commands
            .entity(entity)
            .remove::<(
                AnimationPlayer,
                AnimationGraphHandle,
                AnimationTransitions,
                ActorGaitNodes,
                ActorGait,
            )>()
            .insert((
                ActorClips::load_idle(&asset_server, scene),
                IdlePlayback {
                    repeat: request.repeat,
                },
            ));
    }
}

impl ActorClips {
    /// Loads one verified creature idle. All gait slots use that clip; spawning does not give
    /// these actors an ActorDrive, so this introduces no creature locomotion policy.
    pub fn load_idle(asset_server: &AssetServer, scene: &str) -> Self {
        let idle = asset_server.load(scene.to_owned());
        Self {
            idle: idle.clone(),
            walk: idle.clone(),
            run: idle,
        }
    }

    /// Loads `sex`'s three gait clips through the asset server.
    pub fn load(asset_server: &AssetServer, sex: ActorSex) -> Self {
        Self {
            idle: asset_server.load(gait_clip_scene(sex, Gait::Idle)),
            walk: asset_server.load(gait_clip_scene(sex, Gait::Walk)),
            run: asset_server.load(gait_clip_scene(sex, Gait::Run)),
        }
    }

    fn handle(&self, gait: Gait) -> &Handle<Gltf> {
        match gait {
            Gait::Idle => &self.idle,
            Gait::Walk => &self.walk,
            Gait::Run => &self.run,
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
    (
        Entity,
        &'static ActorClips,
        &'static Actor,
        Option<&'static IdlePlayback>,
        Option<&'static IdleSequence>,
    ),
    (With<ActorBodyReady>, Without<AnimationPlayer>),
>;

/// The named animation of one loaded gait clip, or `None` when it has no animation.
fn clip_animation(gltf: Option<&Gltf>, gait: Gait) -> Option<Handle<AnimationClip>> {
    let gltf = gltf?;
    let name = gait_animation_name(gait);
    gltf.named_animations
        .get(name)
        .or_else(|| gltf.animations.first())
        .cloned()
}

/// Installs every bone's target id and starts the idle clip once the body scene and the clips are
/// available.
///
/// An unusable idle clip is reported once and removes the request. Bone transforms keep
/// their last evaluated pose; switching a request does not restore the bind pose.
/// An unusable walk or run clip is reported once and falls back to the nearest loaded gait.
fn start_actor_animations(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    gltfs: Res<Assets<Gltf>>,
    names: Query<&Name>,
    children: Query<&Children>,
    pending: PendingActorAnimations,
    mut graphs: ResMut<Assets<AnimationGraph>>,
) {
    for (actor, clips, actor_data, playback, sequence) in &pending {
        let form_id = format_args!("{:08X}", actor_data.base_form_id);
        let [idle_state, walk_state, run_state] =
            Gait::ALL.map(|gait| asset_server.load_state(clips.handle(gait).id()));
        if [&idle_state, &walk_state, &run_state]
            .iter()
            .any(|state| matches!(state, LoadState::NotLoaded | LoadState::Loading))
        {
            continue;
        }
        let idle_gltf = gltfs.get(&clips.idle);
        let walk_gltf = gltfs.get(&clips.walk);
        let run_gltf = gltfs.get(&clips.run);
        let loaded_but_missing = (matches!(&idle_state, LoadState::Loaded) && idle_gltf.is_none())
            || (matches!(&walk_state, LoadState::Loaded) && walk_gltf.is_none())
            || (matches!(&run_state, LoadState::Loaded) && run_gltf.is_none());
        if loaded_but_missing {
            continue;
        }

        if let LoadState::Failed(error) = idle_state {
            warn!(
                form_id,
                %error, "actor idle clip failed to load; the actor keeps its current pose"
            );
            commands.entity(actor).remove::<ActorClips>();
            continue;
        }
        let Some(idle_clip) = clip_animation(idle_gltf, Gait::Idle) else {
            warn!(
                form_id,
                "actor idle clip has no animation; the actor keeps its current pose"
            );
            commands.entity(actor).remove::<ActorClips>();
            continue;
        };
        let targets = match animation_targets(actor, &names, &children) {
            Ok(targets) => targets,
            Err(error) => {
                warn!(
                    form_id,
                    reason = error.as_str(),
                    "actor idle animation skipped; the actor keeps its current pose"
                );
                commands.entity(actor).remove::<ActorClips>();
                continue;
            }
        };

        let walk_clip = optional_gait_clip(walk_gltf, &walk_state, form_id, Gait::Walk);
        let run_clip = optional_gait_clip(run_gltf, &run_state, form_id, Gait::Run);

        for target in &targets {
            commands
                .entity(target.entity)
                .insert((target.id, AnimatedBy(actor)));
        }
        let mut graph = AnimationGraph::new();
        let root = graph.root;
        let idle_node = graph.add_clip(idle_clip, 1.0, root);
        let walk_node = walk_clip.map_or(idle_node, |clip| graph.add_clip(clip, 1.0, root));
        let run_node = run_clip.map_or(walk_node, |clip| graph.add_clip(clip, 1.0, root));
        let graph = graphs.add(graph);
        let mut player = AnimationPlayer::default();
        let mut transitions = AnimationTransitions::new();
        let active = transitions.play(&mut player, idle_node, Duration::ZERO);
        // A sequence plays each clip once, so it can see the clip finish and pick the next.
        if sequence.is_none() && playback.is_none_or(|playback| playback.repeat) {
            active.repeat();
        }
        commands
            .entity(actor)
            .insert((
                player,
                AnimationGraphHandle(graph),
                transitions,
                ActorGaitNodes::new(idle_node, walk_node, run_node),
                ActorGait(Gait::Idle),
            ))
            .remove::<ActorClips>();
    }
}

/// Resolves a walk or run clip, reporting a failed load or missing animation once.
fn optional_gait_clip(
    gltf: Option<&Gltf>,
    state: &LoadState,
    form_id: std::fmt::Arguments<'_>,
    gait: Gait,
) -> Option<Handle<AnimationClip>> {
    if let LoadState::Failed(error) = state {
        warn!(
            form_id,
            gait = gait.as_str(),
            %error, "actor gait clip failed to load; falling back"
        );
        return None;
    }
    let clip = clip_animation(gltf, gait);
    if clip.is_none() {
        warn!(
            form_id,
            gait = gait.as_str(),
            "actor gait clip has no animation; falling back"
        );
    }
    clip
}

/// Plays the gait clips on every actor whose body scene and clips are ready.
pub struct ActorAnimationPlugin;

impl Plugin for ActorAnimationPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(mark_actor_body_ready).add_systems(
            Update,
            (
                recover_idle_sequences,
                advance_idle_sequences,
                select_authored_idle,
                start_actor_animations,
            )
                .chain(),
        );
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
        mesh::skinning::SkinnedMeshInverseBindposes,
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

    fn gltf_with_animation(animation: &str, clip: Handle<AnimationClip>) -> Gltf {
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
            named_animations: HashMap::from([(animation.into(), clip)]),
            source: None,
        }
    }

    fn clips_from_one(gltf: Handle<Gltf>) -> ActorClips {
        ActorClips {
            idle: gltf.clone(),
            walk: gltf.clone(),
            run: gltf,
        }
    }

    fn spawn_actor(app: &mut App, scene: Handle<WorldAsset>, clips: ActorClips) -> Entity {
        app.world_mut()
            .spawn((
                Actor::new(
                    0x13475,
                    79686,
                    ActorSex::Male,
                    crate::actors::ActorResolution::Body,
                ),
                clips,
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
    fn creature_bone_names_bind_to_the_actor_player() {
        let (_directory, mut app) = fixture_app();
        let clip = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(AnimationClip::default());
        let clip_gltf = app
            .world()
            .resource::<AssetServer>()
            .add(gltf_with_animation(IDLE_ANIMATION, clip));
        app.update();
        let mut world = World::new();
        let basis = world
            .spawn((Name::new(ANIMATION_ROOT_NODE), Transform::default()))
            .id();
        let pelvis = world
            .spawn((Name::new("Pelvis"), Transform::default(), ChildOf(basis)))
            .id();
        world.spawn((Name::new("HEAD"), Transform::default(), ChildOf(pelvis)));
        let scene = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(world));
        app.update();
        let actor = spawn_actor(&mut app, scene, clips_from_one(clip_gltf));
        for _ in 0..8 {
            app.update();
        }
        let player = app
            .world()
            .get::<AnimationPlayer>(actor)
            .expect("idle player installed");
        assert!(player.playing_animations().next().is_some());
        let mut query = app
            .world_mut()
            .query::<(&Name, &AnimationTargetId, &AnimatedBy)>();
        let (_, id, owner) = query
            .iter(app.world())
            .find(|(name, _, _)| name.as_str() == "HEAD")
            .unwrap();
        assert_eq!(*id, expected_id(&[ANIMATION_ROOT_NODE, "Pelvis", "HEAD"]));
        assert_eq!(owner.0, actor);
        assert!(
            app.world()
                .get::<crate::actor_drive::ActorDrive>(actor)
                .is_none()
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
            .add(gltf_with_animation(IDLE_ANIMATION, clip));
        app.update();
        let scene = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(fixture_body_world()));
        app.update();

        let actor = spawn_actor(&mut app, scene, clips_from_one(clip_gltf));
        for _ in 0..8 {
            app.update();
        }

        assert!(app.world().get::<AnimationPlayer>(actor).is_some());
        assert!(app.world().get::<AnimationGraphHandle>(actor).is_some());
        assert!(app.world().get::<ActorClips>(actor).is_none());

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
        let actor = spawn_actor(
            &mut app,
            scene,
            ActorClips {
                idle: missing.clone(),
                walk: missing.clone(),
                run: missing,
            },
        );

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
        assert!(app.world().get::<ActorClips>(actor).is_none());
    }

    #[test]
    fn a_missing_walk_clip_keeps_idle_playing_and_warns_once() {
        let (_directory, mut app) = fixture_app();
        app.edit_schedule(Update, |schedule| {
            schedule.set_executor(SingleThreadedExecutor::new());
        });
        let clip = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(AnimationClip::default());
        let asset_server = app.world().resource::<AssetServer>().clone();
        let idle = asset_server.add(gltf_with_animation(IDLE_ANIMATION, clip.clone()));
        let run = asset_server.add(gltf_with_animation(RUN_ANIMATION, clip));
        let missing: Handle<Gltf> = asset_server.load(gait_clip_scene(ActorSex::Male, Gait::Walk));
        let scene = asset_server.add(WorldAsset::new(fixture_body_world()));
        app.update();
        let actor = spawn_actor(
            &mut app,
            scene,
            ActorClips {
                idle,
                walk: missing,
                run,
            },
        );

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
            "the missing walk clip is reported exactly once"
        );
        assert!(
            app.world().get::<AnimationPlayer>(actor).is_some(),
            "the idle clip still plays"
        );
        let nodes = *app
            .world()
            .get::<ActorGaitNodes>(actor)
            .expect("the gait nodes");
        assert_eq!(
            nodes.node(Gait::Walk),
            nodes.node(Gait::Idle),
            "a missing walk clip falls back to the idle node"
        );
        assert_eq!(
            app.world().get::<ActorGait>(actor).copied(),
            Some(ActorGait(Gait::Idle))
        );
        assert!(app.world().get_entity(actor).is_ok());
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
            bevy::transform::TransformPlugin,
            WorldSerializationPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>()
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

    fn body_world_from_glb(path: &Path) -> World {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[..4], b"glTF");
        let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let json: serde_json::Value = serde_json::from_slice(&bytes[20..20 + length]).unwrap();
        let nodes = json["nodes"].as_array().unwrap();
        let scene = json["scene"].as_u64().unwrap_or(0) as usize;
        let root = json["scenes"][scene]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node.as_u64().unwrap() as usize)
            .find(|&node| nodes[node]["name"] == ANIMATION_ROOT_NODE)
            .unwrap();
        let mut world = World::new();
        let mut pending = vec![(root, None)];
        let mut visited = HashSet::new();
        while let Some((index, parent)) = pending.pop() {
            assert!(visited.insert(index));
            let node = &nodes[index];
            let floats = |key: &str, default: Vec<f32>| {
                node[key]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .map(|value| value.as_f64().unwrap() as f32)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or(default)
            };
            let transform = if node["matrix"].is_array() {
                Transform::from_matrix(Mat4::from_cols_array(
                    &floats("matrix", Vec::new()).try_into().unwrap(),
                ))
            } else {
                Transform {
                    translation: Vec3::from_slice(&floats("translation", vec![0.; 3])),
                    rotation: Quat::from_array(
                        floats("rotation", vec![0., 0., 0., 1.]).try_into().unwrap(),
                    ),
                    scale: Vec3::from_slice(&floats("scale", vec![1.; 3])),
                }
            };
            let mut entity = world.spawn((
                Name::new(node["name"].as_str().unwrap().to_owned()),
                transform,
            ));
            if let Some(parent) = parent {
                entity.insert(ChildOf(parent));
            }
            let entity = entity.id();
            if let Some(children) = node["children"].as_array() {
                pending.extend(
                    children
                        .iter()
                        .map(|child| (child.as_u64().unwrap() as usize, Some(entity))),
                );
            }
        }
        world
    }

    #[test]
    #[ignore = "requires scoped MUDCRAB_IDLE_LIBRARY_ASSETS and new MUDCRAB_IDLE_PLAYBACK_RECEIPT"]
    fn retail_authored_idle_library_binds_and_plays() {
        use shared::actor_idle_catalog::{ActorIdleCatalog, PATH};
        let assets =
            std::path::PathBuf::from(std::env::var_os("MUDCRAB_IDLE_LIBRARY_ASSETS").unwrap());
        let receipt =
            std::path::PathBuf::from(std::env::var_os("MUDCRAB_IDLE_PLAYBACK_RECEIPT").unwrap());
        let catalog: ActorIdleCatalog =
            serde_json::from_slice(&std::fs::read(assets.join(PATH)).unwrap()).unwrap();
        catalog.validate().unwrap();
        let mut app = retail_app(&assets);
        app.insert_resource(crate::world::actor_catalog::ActorCatalog::open(
            &assets.join("skyrim_world.db"),
        ));
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::ZERO,
        ));
        app.insert_resource(Time::<Fixed>::from_seconds(10000.0));
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .set_max_delta(Duration::from_secs(3600));
        let mut motion_sources = HashSet::new();
        let mut played = 0;
        let mut static_clips = 0;
        let mut one_shot_checked = false;
        let mut records = Vec::new();
        for (index, row) in catalog.bindings.iter().enumerate() {
            let scene =
                app.world()
                    .resource::<AssetServer>()
                    .add(WorldAsset::new(body_world_from_glb(
                        &assets.join(&row.body_scene),
                    )));
            let clips = ActorClips::load_idle(app.world().resource::<AssetServer>(), &row.scene);
            let actor = spawn_actor(&mut app, scene, clips);
            app.world_mut().entity_mut(actor).insert((
                Actor::new(
                    index as u32 + 1,
                    row.race_id,
                    ActorSex::parse(&row.sex).unwrap(),
                    crate::actors::ActorResolution::Body,
                ),
                ActorIdleLibrary {
                    body_scene: row.body_scene.clone(),
                },
            ));
            for (source, expected) in &row.clips {
                app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                    Duration::ZERO,
                ));
                app.world_mut()
                    .entity_mut(actor)
                    .insert(AuthoredIdleRequest {
                        source: source.clone(),
                        repeat: true,
                    });
                app.update();
                wait_until(&mut app, "the authored idle player", |app| {
                    app.world().get::<AnimationPlayer>(actor).is_some()
                });
                // A newly installed graph is threaded after its asset event reaches
                // the next frame. Evaluate time zero before capturing the baseline;
                // otherwise equal clips (for example storm atronach rootb/rootf)
                // can compare the previous clip's sampled pose with the same pose.
                app.update();
                let targets: HashSet<_> = {
                    // Startup consumes ActorClips. Inspect the clip installed on the
                    // actual player rather than the completed loading request.
                    let world = app.world();
                    let player = world.get::<AnimationPlayer>(actor).unwrap();
                    let (node, _) = player.playing_animations().next().unwrap();
                    let graph = world
                        .resource::<Assets<AnimationGraph>>()
                        .get(&world.get::<AnimationGraphHandle>(actor).unwrap().0)
                        .unwrap();
                    let bevy::animation::graph::AnimationNodeType::Clip(handle) =
                        &graph.graph[*node].node_type
                    else {
                        panic!("idle player must select a clip node");
                    };
                    world
                        .resource::<Assets<AnimationClip>>()
                        .get(handle)
                        .unwrap()
                        .curves()
                        .keys()
                        .copied()
                        .collect()
                };
                let mut query =
                    app.world_mut()
                        .query::<(Entity, &AnimatedBy, &AnimationTargetId, &Transform)>();
                let initial: Vec<_> = query
                    .iter(app.world())
                    .filter(|(_, owner, _, _)| owner.0 == actor)
                    .map(|(entity, _, id, transform)| (entity, *id, *transform))
                    .collect();
                let bound: HashSet<_> = initial.iter().map(|(_, id, _)| *id).collect();
                assert!(
                    targets.is_subset(&bound),
                    "unbound targets: {} {source}",
                    row.body_scene
                );
                let mut moved = false;
                if let Some(frame) = expected.motion_frame {
                    // Sample inside the first moving source interval. Sampling the
                    // final key exactly would wrap a repeating clip back to zero.
                    if motion_sources.insert((row.skeleton.clone(), source.clone())) {
                        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                            Duration::from_secs_f64(
                                (f64::from(frame) - 0.25) / f64::from(expected.frames_per_second),
                            ),
                        ));
                        app.update();
                        moved = initial.iter().any(|(entity, _, before)| {
                            app.world()
                                .get::<Transform>(*entity)
                                .is_some_and(|after| after != before)
                        });
                        assert!(
                            moved,
                            "idle did not move at its source motion frame: {} {source}",
                            row.body_scene
                        );
                    }
                } else {
                    static_clips += 1;
                }
                if !one_shot_checked {
                    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                        Duration::ZERO,
                    ));
                    app.world_mut()
                        .entity_mut(actor)
                        .insert(AuthoredIdleRequest {
                            source: source.clone(),
                            repeat: false,
                        });
                    app.update();
                    wait_until(&mut app, "one-shot idle player", |app| {
                        app.world().get::<AnimationPlayer>(actor).is_some()
                    });
                    app.update();
                    let duration = f64::from(expected.frame_count.max(2))
                        / f64::from(expected.frames_per_second);
                    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                        Duration::from_secs_f64(duration * 2.0),
                    ));
                    app.update();
                    assert!(
                        app.world()
                            .get::<AnimationPlayer>(actor)
                            .unwrap()
                            .all_finished(),
                        "one-shot idle must finish"
                    );
                    one_shot_checked = true;
                }
                played += 1;
                records.push(serde_json::json!({"race":row.race_id,"sex":row.sex,"body":row.body_scene,"source":source,"scene":expected.scene,"targets":targets.len(),"motion_checked":moved,"authored_static":expected.motion_frame.is_none()}));
            }
            app.world_mut().entity_mut(actor).despawn();
            if index % 20 == 0 {
                println!("{} bodies, {played} idle clips played", index + 1);
            }
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(receipt)
            .unwrap();
        serde_json::to_writer_pretty(file,&serde_json::json!({"bodies":catalog.bindings.len(),"clips_played":played,"distinct_moving_sources":motion_sources.len(),"static_clip_bindings":static_clips,"one_shot_checked":one_shot_checked,"records":records})).unwrap();
        println!(
            "{} bodies; {played} idle clips bound/played; {} distinct moving sources; {static_clips} static bindings",
            catalog.bindings.len(),
            motion_sources.len()
        );
        assert!(played > 0 && one_shot_checked);
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
        let clip_paths = Gait::ALL.map(|gait| gait_clip_scene(ActorSex::Male, gait));
        if !assets.join(body_path).is_file()
            || !clip_paths.iter().all(|path| assets.join(path).is_file())
        {
            return;
        }

        let warnings = WarningCount::default();
        let subscriber = Registry::default().with(warnings.clone());
        tracing::subscriber::with_default(subscriber, || {
            let mut app = retail_app(&assets);
            let body: Handle<Gltf> = app.world().resource::<AssetServer>().load(body_path);
            let clips = ActorClips::load(app.world().resource::<AssetServer>(), ActorSex::Male);
            let idle = clips.idle.clone();
            wait_until(&mut app, "the composed body", |app| {
                matches!(
                    app.world().resource::<AssetServer>().load_state(body.id()),
                    LoadState::Loaded
                )
            });
            wait_until(&mut app, "the idle clip", |app| {
                matches!(
                    app.world().resource::<AssetServer>().load_state(idle.id()),
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
            let actor = spawn_actor(&mut app, scene, clips.clone());

            wait_until(&mut app, "the actor's animation player", |app| {
                app.world().get::<AnimationPlayer>(actor).is_some()
            });

            let clip_targets: HashSet<AnimationTargetId> = {
                let clips = app.world().resource::<Assets<AnimationClip>>();
                let gltfs = app.world().resource::<Assets<Gltf>>();
                let handle = gltfs
                    .get(&idle)
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
            println!(
                "idle: {} clip targets, {} bound animation targets",
                clip_targets.len(),
                bound.len()
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
            let moved: Vec<String> = initial
                .iter()
                .filter(|(entity, before)| {
                    app.world()
                        .get::<Transform>(*entity)
                        .is_some_and(|after| after != before)
                })
                .filter_map(|(entity, _)| {
                    app.world()
                        .get::<Name>(*entity)
                        .map(|name| name.as_str().to_owned())
                })
                .collect();
            assert!(
                !moved.is_empty(),
                "the idle clip changes at least one bone transform"
            );
            println!(
                "idle: {} bones moved, first {:?}",
                moved.len(),
                &moved[..moved.len().min(8)]
            );

            let warnings_seen = warnings.0.load(Ordering::Relaxed);
            assert_eq!(warnings_seen, 0, "no animation warning is emitted");
        });
    }

    #[test]
    #[ignore = "requires MUDCRAB_CONVERTED_ASSETS with the three converted gait clips"]
    fn retail_gait_clips_load_with_nonzero_durations() {
        let Some(assets) =
            std::env::var_os("MUDCRAB_CONVERTED_ASSETS").map(std::path::PathBuf::from)
        else {
            return;
        };
        let clip_paths = Gait::ALL.map(|gait| gait_clip_scene(ActorSex::Male, gait));
        if !clip_paths.iter().all(|path| assets.join(path).is_file()) {
            return;
        }

        let mut app = retail_app(&assets);
        let clips = ActorClips::load(app.world().resource::<AssetServer>(), ActorSex::Male);
        for gait in Gait::ALL {
            let path = gait_clip_scene(ActorSex::Male, gait);
            let handle = clips.handle(gait);
            wait_until(&mut app, path, |app| {
                matches!(
                    app.world()
                        .resource::<AssetServer>()
                        .load_state(handle.id()),
                    LoadState::Loaded
                )
            });
        }

        let durations: HashMap<Gait, f32> = Gait::ALL
            .into_iter()
            .map(|gait| {
                let gltfs = app.world().resource::<Assets<Gltf>>();
                let handle = clip_animation(gltfs.get(clips.handle(gait)), gait)
                    .expect("the loaded gait clip");
                let clips = app.world().resource::<Assets<AnimationClip>>();
                let duration = clips.get(&handle).expect("the clip asset").duration();
                (gait, duration)
            })
            .collect();
        println!("gait durations: {durations:?}");
        assert!(
            durations.values().all(|duration| *duration > 0.0),
            "every gait clip has a duration: {durations:?}"
        );
        assert!(
            durations[&Gait::Walk] < durations[&Gait::Idle],
            "the walk clip is shorter than idle: {durations:?}"
        );
    }

    fn pool(count: usize) -> Vec<IdlePoolClip> {
        (0..count)
            .map(|index| IdlePoolClip {
                source: format!("clip{index}"),
                scene: format!("meshes/actor-idles/clips/clip{index}.glb"),
            })
            .collect()
    }

    #[test]
    fn idle_sequence_needs_at_least_two_clips() {
        assert!(IdleSequence::new(pool(0), 1).is_none());
        assert!(IdleSequence::new(pool(1), 1).is_none());
        assert!(IdleSequence::new(pool(2), 1).is_some());
    }

    #[test]
    fn idle_sequence_starts_on_the_base_and_stays_inside_the_pool() {
        for reference in [0, 1, 0x13475, 0xA91A0, u32::MAX] {
            let mut sequence = IdleSequence::new(pool(5), reference).unwrap();
            assert_eq!(sequence.position(), (0, 5));
            assert_eq!(sequence.current_source(), "clip0");
            for _ in 0..200 {
                sequence.advance_for_test();
                let (current, size) = sequence.position();
                assert!(current < size, "reference {reference:#x}");
            }
        }
    }

    #[test]
    fn idle_sequence_picks_with_equal_weights_and_may_repeat_a_clip() {
        let mut sequence = IdleSequence::new(pool(4), 0x23A92).unwrap();
        let mut counts = [0_u32; 4];
        let mut repeats = 0;
        let mut previous = 0;
        for _ in 0..4000 {
            sequence.advance_for_test();
            let current = sequence.position().0;
            counts[current] += 1;
            repeats += u32::from(current == previous);
            previous = current;
        }
        // 1000 expected per clip; a fair generator stays well inside 15 % of that.
        for count in counts {
            assert!((850..=1150).contains(&count), "unequal weights: {counts:?}");
        }
        // Retail's tail layers can pick the clip that just ended: about a quarter of the draws.
        assert!((800..=1200).contains(&repeats), "repeats: {repeats}");
    }

    #[test]
    fn idle_sequence_is_reproducible_and_reaches_every_clip() {
        let run = |reference| {
            let mut sequence = IdleSequence::new(pool(4), reference).unwrap();
            (0..64)
                .map(|_| {
                    sequence.advance_for_test();
                    sequence.position().0
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(run(0x23A92), run(0x23A92));
        assert_ne!(run(0x23A92), run(0x23A8A));
        let seen: HashSet<_> = run(0x23A92).into_iter().collect();
        assert_eq!(seen.len(), 4, "every clip of the pool is reached: {seen:?}");
    }

    #[test]
    fn a_finished_clip_asks_for_the_next_clip_once_and_never_loops() {
        let (_directory, mut app) = fixture_app();
        let actor = app
            .world_mut()
            .spawn((
                Actor::new(
                    0x23A92,
                    0x131EE,
                    ActorSex::Male,
                    crate::actors::ActorResolution::Body,
                ),
                IdleSequence::new(pool(3), 0x23A92).unwrap(),
                // A player with nothing playing counts as finished.
                AnimationPlayer::default(),
            ))
            .id();
        app.update();
        let sequence = app.world().get::<IdleSequence>(actor).unwrap();
        let request = app.world().get::<AuthoredIdleRequest>(actor).unwrap();
        assert_eq!(request.source, sequence.current_source());
        assert!(!request.repeat, "each clip plays to its end");
        assert!(
            !sequence.is_waiting(),
            "a clip with no pending load starts at once"
        );
    }

    #[test]
    fn a_pick_never_lands_on_a_clip_that_failed() {
        let mut sequence = IdleSequence::new(pool(4), 0x23A92).unwrap();
        sequence.failed = vec![true, false, true, false];
        for _ in 0..200 {
            let next = sequence.choose().unwrap();
            assert!(next == 1 || next == 3, "picked failed clip {next}");
        }
        sequence.failed = vec![true; 4];
        assert!(sequence.choose().is_none());
    }

    #[test]
    fn a_clip_that_did_not_start_is_dropped_and_the_sequence_goes_on() {
        let (_directory, mut app) = fixture_app();
        // The body is ready, but the clip request was consumed and no player was installed:
        // what `start_actor_animations` leaves behind when a clip has no animation.
        let actor = app
            .world_mut()
            .spawn((
                Actor::new(
                    0x23A92,
                    0x131EE,
                    ActorSex::Male,
                    crate::actors::ActorResolution::Body,
                ),
                IdleSequence::new(pool(3), 0x23A92).unwrap(),
                ActorBodyReady,
            ))
            .id();
        app.update();
        let sequence = app.world().get::<IdleSequence>(actor).unwrap();
        assert!(sequence.failed[0], "the clip that did not start is marked");
        assert_ne!(sequence.position().0, 0, "the next clip is another one");
        let request = app.world().get::<AuthoredIdleRequest>(actor).unwrap();
        assert_eq!(request.source, sequence.current_source());
        assert!(!request.repeat);
    }

    #[test]
    fn a_sequence_whose_clips_all_fail_stops_and_leaves_the_actor_alone() {
        let (_directory, mut app) = fixture_app();
        let actor = app
            .world_mut()
            .spawn((
                Actor::new(
                    0x23A92,
                    0x131EE,
                    ActorSex::Male,
                    crate::actors::ActorResolution::Body,
                ),
                IdleSequence::new(pool(2), 0x23A92).unwrap(),
                ActorBodyReady,
            ))
            .id();
        // Clip 0 fails, clip 1 is asked for and fails too: the third pass finds nothing left.
        for _ in 0..3 {
            app.update();
        }
        assert!(
            app.world().get::<IdleSequence>(actor).is_none(),
            "no usable clip is left, so the component is removed"
        );
    }

    /// Plays a creature's idle sequence for a simulated minute (no window, no GPU) and prints the
    /// clip changes. Run it with the pack that has the idle catalog:
    /// `MUDCRAB_IDLE_SEQUENCE_ASSETS=<pack> cargo test -p engine --lib idle_sequence_plays -- --ignored --nocapture`.
    #[test]
    #[ignore = "requires MUDCRAB_IDLE_SEQUENCE_ASSETS, a pack with the actor idle catalog"]
    fn idle_sequence_plays_several_clips_in_a_minute() {
        use crate::world::actor_catalog::ActorCatalog;
        let assets =
            std::path::PathBuf::from(std::env::var_os("MUDCRAB_IDLE_SEQUENCE_ASSETS").unwrap());
        let seconds: f64 = std::env::var("MUDCRAB_IDLE_SEQUENCE_SECONDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(60.0);
        let catalog = ActorCatalog::open(&assets.join("skyrim_world.db"));
        let mut app = retail_app(&assets);
        app.insert_resource(catalog.clone());
        let mut most_distinct = 0;
        // Base NPC_ FormIDs of a dog, a cow, a bear and a draugr; each is also its reference id.
        // The dog and the cow must get a sequence. The bear and the draugr are controls: they are
        // outside the verified families, so they must keep their one default clip.
        for (name, form_id, expect_sequence) in [
            ("dog", 0x23A92_u32, true),
            ("cow", 0x23A90, true),
            ("bear", 0x23A8A, false),
            ("draugr", 0x1FB8F, false),
        ] {
            let plan = catalog
                .spawn_plan(
                    catalog
                        .get(form_id)
                        .expect("the creature is in the catalog"),
                )
                .unwrap();
            let pool = catalog.idle_sequence_pool(&plan);
            let Some(sequence) = IdleSequence::new(pool.clone(), form_id)
                .map(|sequence| sequence.preload(app.world().resource::<AssetServer>()))
            else {
                assert!(
                    !expect_sequence,
                    "the {name} must get an idle sequence, but its pool has {} clip(s)",
                    pool.len()
                );
                assert_eq!(
                    pool.len(),
                    1,
                    "the {name} control keeps only its default clip"
                );
                println!(
                    "IDLE_SEQUENCE {name}: pool of {} clip(s), nothing to vary (control)",
                    pool.len()
                );
                continue;
            };
            assert!(
                expect_sequence,
                "the {name} is outside the verified families but got a sequence"
            );
            let scene = catalog
                .idle_scene(&plan)
                .expect("the default clip exists")
                .to_owned();
            let body =
                app.world()
                    .resource::<AssetServer>()
                    .add(WorldAsset::new(body_world_from_glb(
                        &assets.join(&plan.scene_path),
                    )));
            let clips = ActorClips::load_idle(app.world().resource::<AssetServer>(), &scene);
            let actor = app
                .world_mut()
                .spawn((
                    plan.actor,
                    clips,
                    WorldAssetRoot(body),
                    ActorIdleLibrary {
                        body_scene: plan.scene_path.clone(),
                    },
                    sequence,
                ))
                .id();
            app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                Duration::ZERO,
            ));
            wait_until(&mut app, "the base idle player", |app| {
                app.world().get::<AnimationPlayer>(actor).is_some()
            });
            let step = Duration::from_secs_f64(1.0 / 30.0);
            let mut elapsed = 0.0;
            let mut played = vec![(
                0.0,
                app.world()
                    .get::<IdleSequence>(actor)
                    .unwrap()
                    .current_source()
                    .to_owned(),
            )];
            println!("IDLE_SEQUENCE {name}: pool of {} clips", pool.len());
            println!("IDLE_SEQUENCE {name} t=0.00 {}", played[0].1);
            let deadline = Instant::now() + Duration::from_secs(120);
            // Updates in which the creature had no running player or waited for a clip to load,
            // and the longest run of them. A clip change must not freeze the creature.
            // A clip change installs a new player. Bevy threads the new graph on the next update,
            // so the pose is not evaluated for one update: that is counted separately.
            let (mut stalled, mut run, mut longest_run, mut switch_updates) =
                (0_u32, 0_u32, 0_u32, 0_u32);
            while elapsed < seconds {
                assert!(
                    Instant::now() < deadline,
                    "the {name} sequence stalled at {elapsed:.1} s"
                );
                // Time stands still while a clip is not running, so every stalled update is
                // counted and none is hidden. A short sleep lets the preload make progress.
                let ready = {
                    let world = app.world();
                    world.get::<ActorClips>(actor).is_none()
                        && world.get::<AnimationPlayer>(actor).is_some()
                        && !world.get::<IdleSequence>(actor).unwrap().is_waiting()
                };
                if ready {
                    run = 0;
                } else {
                    stalled += 1;
                    run += 1;
                    longest_run = longest_run.max(run);
                }
                app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(if ready {
                    step
                } else {
                    Duration::ZERO
                }));
                app.update();
                if ready {
                    elapsed += step.as_secs_f64();
                }
                std::thread::sleep(Duration::from_millis(1));
                let current = app
                    .world()
                    .get::<IdleSequence>(actor)
                    .unwrap()
                    .current_source();
                if played.last().is_some_and(|(_, last)| last != current) {
                    println!("IDLE_SEQUENCE {name} t={elapsed:.2} {current}");
                    played.push((elapsed, current.to_owned()));
                    switch_updates += 1;
                }
            }
            let distinct: HashSet<_> = played.iter().map(|(_, source)| source.as_str()).collect();
            println!(
                "IDLE_SEQUENCE {name}: {} clip changes, {} distinct clips in {elapsed:.0} s; {stalled} stalled updates (longest run {longest_run}), {switch_updates} one-update graph threadings",
                played.len() - 1,
                distinct.len()
            );
            assert!(
                longest_run <= 10,
                "the {name} stood still for {longest_run} updates in a row"
            );
            most_distinct = most_distinct.max(distinct.len());
            app.world_mut().entity_mut(actor).despawn();
        }
        assert!(
            most_distinct >= 3,
            "no creature showed three different idles in {seconds} s"
        );
    }
}
