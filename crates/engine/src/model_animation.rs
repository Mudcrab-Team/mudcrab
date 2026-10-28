//! Authored model clips owned by their loaded scene, without a second glTF load.
use crate::{
    doors::LoadDoor,
    world::components::{FormId, MeshHandle},
};
use std::collections::{HashMap, HashSet};

use bevy::{
    animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex},
    asset::LoadContext,
    ecs::{
        lifecycle::{Discard, Insert},
        system::SystemParam,
    },
    gltf::{
        GltfLoaderSettings,
        extensions::{ErasedGltfExtensionHandler, GltfExtensionHandler, GltfExtensionHandlers},
    },
    prelude::*,
};

/// Document inventory copied with each scene instance. Empty is proof of no named clips;
/// absence means unresolved. Scene reload replaces this component and its instance entity.
/// Immutable in ECS: replace the inventory with `insert`, so every edit reaches the lifecycle
/// journal without checking every source's change tick on settled frames.
#[derive(Component, Reflect, Clone, Default, Debug)]
#[reflect(Component)]
#[component(immutable)]
pub struct SourceClips(pub Vec<(String, Handle<AnimationClip>)>);

#[derive(Default, Clone)]
struct ClipInventory {
    clips: SourceClips,
    complete: bool,
}
impl GltfExtensionHandler for ClipInventory {
    fn dyn_clone(&self) -> Box<dyn ErasedGltfExtensionHandler> {
        Box::new(self.clone())
    }
    fn on_root(
        &mut self,
        _context: &mut LoadContext<'_>,
        gltf: &bevy::gltf::gltf::Gltf,
        settings: &GltfLoaderSettings,
    ) {
        self.clips.0.clear();
        self.complete = settings.load_animations || gltf.animations().next().is_none();
        // An intentionally disabled animation loader is not proof of a static source.
        if !settings.load_animations && gltf.animations().next().is_some() {
            warn!("authored model animations disabled in loader settings");
        }
    }
    fn on_animations_collected(
        &mut self,
        _context: &mut LoadContext<'_>,
        _animations: &[Handle<AnimationClip>],
        named: &bevy::platform::collections::HashMap<Box<str>, Handle<AnimationClip>>,
        _roots: &bevy::platform::collections::HashSet<usize>,
    ) {
        self.clips.0 = named
            .iter()
            .map(|(name, clip)| (name.to_string(), clip.clone()))
            .collect();
    }
    fn on_scene_completed(
        &mut self,
        _context: &mut LoadContext<'_>,
        _scene: &bevy::gltf::gltf::Scene<'_>,
        root: Entity,
        world: &mut World,
    ) {
        if self.complete {
            world.entity_mut(root).insert(self.clips.clone());
        }
    }
}

/// Install authored playback for an interactive world.
pub fn add_ordinary_animation_plugins(app: &mut App) {
    if app
        .world()
        .resource::<crate::config::EngineConfig>()
        .interactive_world_physics()
    {
        if !app.is_plugin_added::<ModelAnimationPlugin>() {
            app.add_plugins(ModelAnimationPlugin);
        }
        if !app.is_plugin_added::<crate::door_animation::DoorAnimationPlugin>() {
            app.add_plugins(crate::door_animation::DoorAnimationPlugin);
        }
    }
}

pub struct ModelAnimationPlugin;
impl Plugin for ModelAnimationPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<SourceClips>()
            .init_resource::<SourceClipIndex>()
            .init_resource::<SourceClipJournal>()
            .add_observer(source_inserted)
            .add_observer(source_discarded);
        // Bootstrap once if playback is installed after references were spawned. Subsequent
        // discovery is entirely lifecycle-driven, including inventories arriving much later.
        let world = app.world_mut();
        let existing: Vec<_> = world
            .query_filtered::<Entity, With<MeshHandle>>()
            .iter(world)
            .collect();
        world.resource_mut::<SourceClipJournal>().0.extend(existing);
        app.world_mut()
            .resource_mut::<GltfExtensionHandlers>()
            .0
            .write_blocking()
            .push(Box::<ClipInventory>::default());
        app.add_systems(Update, (resolve_source_clips, play_idle_clips).chain());
    }
}

/// A resolved source, including a real static answer. Retains the graph while its scene lives.
#[derive(Component, Clone, Debug)]
pub struct SourceClipBinding {
    pub source: Entity,
    pub players: Vec<Entity>,
    pub graph: Option<Handle<AnimationGraph>>,
    pub open: Option<AnimationNodeIndex>,
    pub idle: Option<(AnimationNodeIndex, f32)>,
}
#[derive(Component)]
struct IdleStarted;
/// Permanent failure for current inputs (invalid duration or ambiguous inventory).
/// Input lifecycle events clear and revalidate it; readiness is not a timeout.
#[derive(Component, Debug)]
pub struct InvalidSourceClip;

/// Reverse dependencies let topology and asset events invalidate both sides of a move.
#[derive(Resource, Default)]
pub(crate) struct SourceClipIndex {
    // Owned breadth-first scratch. Clear length, retain capacity across references/frames.
    // This is not an ownership index; unresolved/ambiguous references retain no node map.
    traversal: Vec<Entity>,
    node_users: HashMap<Entity, HashSet<Entity>>,
    nodes: HashMap<Entity, Vec<Entity>>,
    clip_users: HashMap<AssetId<AnimationClip>, HashSet<Entity>>,
    clips: HashMap<Entity, Vec<AssetId<AnimationClip>>>,
    #[cfg(test)]
    traversals: usize,
    #[cfg(test)]
    journal_entities: usize,
}

impl SourceClipIndex {
    fn forget(&mut self, reference: Entity) {
        for node in self.nodes.remove(&reference).unwrap_or_default() {
            if let Some(users) = self.node_users.get_mut(&node) {
                users.remove(&reference);
                if users.is_empty() {
                    self.node_users.remove(&node);
                }
            }
        }
        for clip in self.clips.remove(&reference).unwrap_or_default() {
            if let Some(users) = self.clip_users.get_mut(&clip) {
                users.remove(&reference);
                if users.is_empty() {
                    self.clip_users.remove(&clip);
                }
            }
        }
    }
}

/// Only component lifecycle events enqueue entities. MeshHandle and LoadDoor payloads are
/// not binding inputs: only their presence matters. ChildOf is Bevy's immutable hierarchy
/// authority; Children membership changes are driven by its insert/discard hooks. Player
/// playback mutation is deliberately not an ownership change. Supported sibling reorder
/// changes no membership: exactly one inventory is required, so order cannot select authority.
#[derive(Resource, Default)]
struct SourceClipJournal(HashSet<Entity>);

type SourceInputs = (
    SourceClips,
    MeshHandle,
    ChildOf,
    Children,
    AnimationPlayer,
    LoadDoor,
);

fn source_inserted(event: On<Insert, SourceInputs>, mut journal: ResMut<SourceClipJournal>) {
    journal.0.insert(event.entity);
}

fn source_discarded(
    event: On<Discard, SourceInputs>,
    parents: Query<&ChildOf>,
    mut journal: ResMut<SourceClipJournal>,
) {
    journal.0.insert(event.entity);
    // Discard also fires on replacement, before the old ChildOf disappears. Preserve ancestry for
    // ambiguous/inventory-free trees, which intentionally have no persistent reverse index.
    journal.0.extend(parents.iter_ancestors(event.entity));
}

#[derive(SystemParam)]
pub(crate) struct SourceChanges<'w, 's> {
    journal: ResMut<'w, SourceClipJournal>,
    clips: MessageReader<'w, 's, AssetEvent<AnimationClip>>,
}

type SourcePlayers<'w, 's> = Query<
    'w,
    's,
    (
        Option<&'static AnimationGraphHandle>,
        &'static mut AnimationPlayer,
    ),
>;

#[expect(
    clippy::too_many_arguments,
    reason = "Source ownership and playback resolve in one ordered transaction."
)]
pub(crate) fn resolve_source_clips(
    mut commands: Commands,
    references: Query<Option<&LoadDoor>, With<MeshHandle>>,
    bindings: Query<&SourceClipBinding>,
    children: Query<&Children>,
    parents: Query<&ChildOf>,
    inventories: Query<&SourceClips>,
    mut source_access: ParamSet<(SourceChanges, SourcePlayers)>,
    clips: Res<Assets<AnimationClip>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    invalid: Query<(), With<InvalidSourceClip>>,
    mut index: ResMut<SourceClipIndex>,
) {
    let mut dirty = HashSet::new();
    // Keep event consumption separate from player mutation. No change-tick query scans
    // animation-free references or settled sources, and no subtree is polled for readiness.
    {
        let mut changes = source_access.p0();
        for entity in changes.journal.0.drain() {
            #[cfg(test)]
            {
                index.journal_entities += 1;
            }
            if let Some(previous) = index.node_users.get(&entity) {
                dirty.extend(previous.iter().copied());
            }
            // New ancestry matters even when the subtree had no inventory before.
            for ancestor in std::iter::once(entity).chain(parents.iter_ancestors(entity)) {
                if references.contains(ancestor) {
                    dirty.insert(ancestor);
                }
            }
        }
        for event in changes.clips.read() {
            let (id, invalidates) = match event {
                AssetEvent::Added { id } | AssetEvent::LoadedWithDependencies { id } => (id, false),
                AssetEvent::Modified { id }
                | AssetEvent::Removed { id }
                | AssetEvent::Unused { id } => (id, true),
            };
            if let Some(users) = index.clip_users.get(id) {
                // Readiness can arrive after topology already bound and started this clip.
                // Wake unresolved users without stopping a consumed OpenDoor or ambient phase.
                dirty.extend(
                    users
                        .iter()
                        .copied()
                        .filter(|reference| invalidates || !bindings.contains(*reference)),
                );
            }
        }
    }
    let mut players = source_access.p1();
    // No settled hierarchy traversal, including references whose scene has not loaded.
    // Withdraw every old owner before installing any new one. Reparented
    // players must not lose their replacement graph to deferred old-owner cleanup.
    for &reference in &dirty {
        if let Ok(binding) = bindings.get(reference) {
            // Stop only the graph this binding installed. Another playback owner
            // may have replaced it while retaining the same player entity.
            if let Some(graph) = &binding.graph {
                for player in &binding.players {
                    if let Ok((Some(installed), mut playback)) = players.get_mut(*player)
                        && installed.0 == *graph
                    {
                        playback.stop_all();
                        commands
                            .entity(*player)
                            .try_remove::<AnimationGraphHandle>();
                    }
                }
            }
            commands
                .entity(reference)
                .try_remove::<(SourceClipBinding, IdleStarted)>();
        }
        // A new ownership/input event can turn a formerly invalid duration into delayed
        // loading. Revalidate below instead of retaining a stale permanent-failure marker.
        commands.entity(reference).try_remove::<InvalidSourceClip>();
        index.forget(reference);
    }
    for reference in dirty {
        let Ok(door) = references.get(reference) else {
            continue;
        };
        #[cfg(test)]
        {
            index.traversals += 1;
        }
        // Scan every inventory, independent of sibling order. Reuse one owned queue-like
        // vector (cursor + append) instead of allocating Bevy's DescendantIter VecDeque.
        index.traversal.clear();
        index.traversal.push(reference);
        let mut cursor = 0;
        let mut source = None;
        let mut ambiguous = false;
        while cursor < index.traversal.len() {
            let entity = index.traversal[cursor];
            cursor += 1;
            if inventories.contains(entity) && source.replace(entity).is_some() {
                ambiguous = true;
            }
            if let Ok(descendants) = children.get(entity) {
                index.traversal.extend(descendants.iter());
            }
        }
        if ambiguous {
            if !invalid.contains(reference) {
                warn!(
                    ?reference,
                    "multiple authored clip inventories under one reference; no source selected"
                );
            }
            commands.entity(reference).try_insert(InvalidSourceClip);
            continue;
        }
        let Some(source) = source else {
            continue;
        };
        let Ok(inventory) = inventories.get(source) else {
            continue;
        };
        // Only a unique completed inventory permits persistent ownership allocation.
        // Clone after validation: the owned scratch must keep its capacity for the next tree.
        let nodes = index.traversal.clone();
        for node in &nodes {
            index.node_users.entry(*node).or_default().insert(reference);
        }
        index.nodes.insert(reference, nodes);
        let named = |wanted: &str| {
            inventory
                .0
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
                .map(|(_, handle)| handle.clone())
        };
        let selected = if door.is_some() {
            [named("Open"), None]
        } else {
            [None, named("Idle")]
        };
        for handle in selected.iter().flatten() {
            index
                .clip_users
                .entry(handle.id())
                .or_default()
                .insert(reference);
            index.clips.entry(reference).or_default().push(handle.id());
        }
        let mut duration = 0.0;
        let mut ready = true;
        for handle in selected.iter().flatten() {
            let Some(clip) = clips.get(handle) else {
                ready = false;
                break;
            };
            if !clip.duration().is_finite() || clip.duration() <= 0.0 {
                if !invalid.contains(reference) {
                    error!(
                        ?reference,
                        "authored source clip has invalid duration; animation remains unresolved"
                    );
                }
                commands.entity(reference).try_insert(InvalidSourceClip);
                ready = false;
                break;
            }
            duration = clip.duration();
        }
        if !ready {
            continue;
        }
        let animated = selected.iter().any(Option::is_some);
        let graph_players: Vec<_> = if animated {
            children
                .iter_descendants(source)
                .chain(std::iter::once(source))
                .filter(|e| players.contains(*e))
                .collect()
        } else {
            Vec::new()
        };
        if animated && graph_players.is_empty() {
            continue;
        }
        let mut nodes = [None; 2];
        let graph = animated.then(|| {
            let mut graph = AnimationGraph::new();
            for (slot, handle) in selected.into_iter().enumerate() {
                if let Some(handle) = handle {
                    nodes[slot] = Some(graph.add_clip(handle, 1.0, graph.root));
                }
            }
            graphs.add(graph)
        });
        if let Some(graph) = &graph {
            for player in &graph_players {
                commands
                    .entity(*player)
                    .try_insert(AnimationGraphHandle(graph.clone()));
            }
        }
        commands
            .entity(reference)
            .try_remove::<(IdleStarted, InvalidSourceClip)>();
        commands.entity(reference).try_insert(SourceClipBinding {
            source,
            players: graph_players,
            graph,
            open: nodes[0],
            idle: nodes[1].map(|n| (n, duration)),
        });
    }
}

#[expect(
    clippy::type_complexity,
    reason = "The ambient-idle filter names the two exclusions a door and a started idle impose."
)]
fn play_idle_clips(
    mut commands: Commands,
    references: Query<
        (Entity, &FormId, &SourceClipBinding),
        (Without<LoadDoor>, Without<IdleStarted>),
    >,
    mut players: Query<&mut AnimationPlayer>,
) {
    for (entity, form, binding) in &references {
        if let Some((node, duration)) = binding.idle {
            for player in &binding.players {
                if let Ok(mut player) = players.get_mut(*player) {
                    player
                        .play(node)
                        .repeat()
                        .seek_to(start_offset(form.0, duration));
                }
            }
        }
        commands.entity(entity).try_insert(IdleStarted);
    }
}

fn start_offset(form_id: u32, seconds: f32) -> f32 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0.0;
    }
    let offset = (f64::from(form_id) % f64::from(seconds)) as f32;
    if offset.is_finite() && offset < seconds {
        offset
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interactive_world_installs_playback_and_automated_cameras_do_not() {
        for automated in 0..3 {
            let mut config = crate::config::EngineConfig::default();
            if automated == 1 {
                config.benchmark_frames = Some(10);
            }
            if automated == 2 {
                config.auto_fly_speed = 100.0;
            }
            let mut app = App::new();
            app.init_resource::<GltfExtensionHandlers>()
                .insert_resource(config);
            add_ordinary_animation_plugins(&mut app);
            add_ordinary_animation_plugins(&mut app);
            assert_eq!(
                app.is_plugin_added::<ModelAnimationPlugin>(),
                automated == 0
            );
            assert_eq!(
                app.is_plugin_added::<crate::door_animation::DoorAnimationPlugin>(),
                automated == 0
            );
            assert!(
                !app.world()
                    .contains_resource::<crate::streaming::StreamingWorld>()
            );
        }
    }
    #[test]
    fn deterministic_ambient_phase_preserves_low_form_id_bits() {
        assert_eq!(start_offset(u32::MAX, 2.0), 1.0);
        assert_eq!(start_offset(17, 2.0), 1.0);
        assert_eq!(start_offset(17, 0.0), 0.0);
    }
    fn resolver_app() -> App {
        let mut app = App::new();
        app.init_resource::<GltfExtensionHandlers>()
            .init_resource::<Assets<AnimationClip>>()
            .init_resource::<Assets<AnimationGraph>>()
            .add_message::<AssetEvent<AnimationClip>>()
            .add_plugins(ModelAnimationPlugin);
        app
    }

    #[test]
    fn unresolved_references_do_no_settled_traversal_and_late_inventory_reparents() {
        let mut app = resolver_app();
        let first = app.world_mut().spawn(MeshHandle("first.glb".into())).id();
        let second = app.world_mut().spawn(MeshHandle("second.glb".into())).id();
        let branch = app.world_mut().spawn(ChildOf(first)).id();
        app.update();
        let work = app.world().resource::<SourceClipIndex>().traversals;
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(app.world().resource::<SourceClipIndex>().traversals, work);
        assert!(app.world().get::<SourceClipBinding>(first).is_none());
        // The source arrives below an already settled, unbound branch.
        let source = app
            .world_mut()
            .spawn((SourceClips::default(), ChildOf(branch)))
            .id();
        app.update();
        assert_eq!(
            app.world().get::<SourceClipBinding>(first).unwrap().source,
            source
        );
        app.world_mut().entity_mut(branch).insert(ChildOf(second));
        app.update();
        assert!(app.world().get::<SourceClipBinding>(first).is_none());
        assert_eq!(
            app.world().get::<SourceClipBinding>(second).unwrap().source,
            source
        );
        app.world_mut().entity_mut(source).remove::<SourceClips>();
        app.update();
        assert!(app.world().get::<SourceClipBinding>(second).is_none());
        app.world_mut().entity_mut(first).despawn();
        app.world_mut().entity_mut(second).despawn();
        app.update();
        let index = app.world().resource::<SourceClipIndex>();
        assert!(index.nodes.is_empty() && index.node_users.is_empty());
    }

    #[test]
    fn late_clip_and_player_bind_case_insensitive_idle_without_polling() {
        let mut app = resolver_app();
        let reference = app
            .world_mut()
            .spawn((MeshHandle("idle.glb".into()), FormId(17)))
            .id();
        let clip = app
            .world()
            .resource::<Assets<AnimationClip>>()
            .reserve_handle();
        let source = app
            .world_mut()
            .spawn((
                SourceClips(vec![("iDlE".into(), clip.clone())]),
                ChildOf(reference),
            ))
            .id();
        app.update();
        assert!(app.world().get::<SourceClipBinding>(reference).is_none());
        let work = app.world().resource::<SourceClipIndex>().traversals;
        app.update();
        assert_eq!(app.world().resource::<SourceClipIndex>().traversals, work);
        let mut loaded = AnimationClip::default();
        loaded.set_duration(2.0);
        app.world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .insert(clip.id(), loaded)
            .unwrap();
        app.world_mut()
            .write_message(AssetEvent::<AnimationClip>::LoadedWithDependencies { id: clip.id() });
        app.update();
        assert!(
            app.world().get::<SourceClipBinding>(reference).is_none(),
            "player has not arrived"
        );
        app.world_mut()
            .entity_mut(source)
            .insert(AnimationPlayer::default());
        app.update();
        let binding = app.world().get::<SourceClipBinding>(reference).unwrap();
        let node = binding.idle.unwrap().0;
        assert!(
            app.world()
                .get::<AnimationPlayer>(source)
                .unwrap()
                .is_playing_animation(node)
        );
        app.world_mut()
            .entity_mut(source)
            .remove::<AnimationPlayer>();
        app.update();
        assert!(app.world().get::<SourceClipBinding>(reference).is_none());
        app.world_mut().entity_mut(reference).remove::<MeshHandle>();
        app.update();
        let index = app.world().resource::<SourceClipIndex>();
        assert!(index.nodes.is_empty() && index.clip_users.is_empty());
    }

    #[test]
    fn readiness_events_wake_unresolved_users_without_revisiting_bound_users() {
        for loaded_with_dependencies in [false, true] {
            let mut app = resolver_app();
            let reference = app
                .world_mut()
                .spawn((MeshHandle("idle.glb".into()), FormId(17)))
                .id();
            let clip = app
                .world()
                .resource::<Assets<AnimationClip>>()
                .reserve_handle();
            let source = app
                .world_mut()
                .spawn((
                    SourceClips(vec![("Idle".into(), clip.clone())]),
                    AnimationPlayer::default(),
                    ChildOf(reference),
                ))
                .id();
            app.update();
            assert!(app.world().get::<SourceClipBinding>(reference).is_none());
            let work = app.world().resource::<SourceClipIndex>().traversals;
            let mut loaded = AnimationClip::default();
            loaded.set_duration(2.0);
            app.world_mut()
                .resource_mut::<Assets<AnimationClip>>()
                .insert(clip.id(), loaded)
                .unwrap();
            // This resolver-only harness has no AssetPlugin event publisher. Asset presence
            // alone must not cause a scan; publish each real readiness variant explicitly.
            app.update();
            assert!(app.world().get::<SourceClipBinding>(reference).is_none());
            assert_eq!(app.world().resource::<SourceClipIndex>().traversals, work);
            let event = || {
                if loaded_with_dependencies {
                    AssetEvent::<AnimationClip>::LoadedWithDependencies { id: clip.id() }
                } else {
                    AssetEvent::<AnimationClip>::Added { id: clip.id() }
                }
            };
            app.world_mut().write_message(event());
            app.update();
            let binding = app
                .world()
                .get::<SourceClipBinding>(reference)
                .unwrap()
                .clone();
            let node = binding.idle.unwrap().0;
            assert_eq!(binding.source, source);
            assert!(
                app.world()
                    .get::<AnimationPlayer>(source)
                    .unwrap()
                    .is_playing_animation(node)
            );
            assert_eq!(
                app.world().resource::<SourceClipIndex>().traversals,
                work + 1
            );
            app.world_mut().write_message(event());
            app.update();
            assert_eq!(
                app.world()
                    .get::<SourceClipBinding>(reference)
                    .unwrap()
                    .graph,
                binding.graph
            );
            assert!(
                app.world()
                    .get::<AnimationPlayer>(source)
                    .unwrap()
                    .is_playing_animation(node)
            );
            assert_eq!(
                app.world().resource::<SourceClipIndex>().traversals,
                work + 1
            );
            // Unused is an invalidation notification, unlike readiness. Live inventory
            // retains a strong handle, so inject this protocol case in the resolver harness.
            app.world_mut()
                .write_message(AssetEvent::<AnimationClip>::Unused { id: clip.id() });
            app.update();
            assert_ne!(
                app.world()
                    .get::<SourceClipBinding>(reference)
                    .unwrap()
                    .graph,
                binding.graph
            );
            assert_eq!(
                app.world().resource::<SourceClipIndex>().traversals,
                work + 2
            );
        }
    }

    #[test]
    fn reparented_player_keeps_the_new_owners_graph() {
        let mut app = resolver_app();
        let first = app.world_mut().spawn(MeshHandle("first.glb".into())).id();
        let second = app.world_mut().spawn(MeshHandle("second.glb".into())).id();
        let mut clip = AnimationClip::default();
        clip.set_duration(2.0);
        let clip = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(clip);
        let source = app
            .world_mut()
            .spawn((
                SourceClips(vec![("Idle".into(), clip)]),
                AnimationPlayer::default(),
                ChildOf(first),
            ))
            .id();
        app.update();
        let old = app
            .world()
            .get::<SourceClipBinding>(first)
            .unwrap()
            .graph
            .clone();
        app.world_mut().entity_mut(source).insert(ChildOf(second));
        app.update();
        assert!(app.world().get::<SourceClipBinding>(first).is_none());
        let new = app
            .world()
            .get::<SourceClipBinding>(second)
            .unwrap()
            .graph
            .as_ref()
            .unwrap();
        assert_ne!(Some(new), old.as_ref());
        assert_eq!(
            &app.world().get::<AnimationGraphHandle>(source).unwrap().0,
            new
        );
    }

    #[test]
    fn settled_journal_does_zero_entity_work_and_unresolved_trees_have_no_index() {
        let mut app = resolver_app();
        let mut references = Vec::new();
        for _ in 0..256 {
            let reference = app.world_mut().spawn(MeshHandle("late.glb".into())).id();
            app.world_mut().spawn(ChildOf(reference));
            references.push(reference);
        }
        app.update();
        let work = {
            let index = app.world().resource::<SourceClipIndex>();
            assert!(index.nodes.is_empty() && index.node_users.is_empty());
            (index.journal_entities, index.traversals)
        };
        for _ in 0..8 {
            app.update();
        }
        let index = app.world().resource::<SourceClipIndex>();
        assert_eq!((index.journal_entities, index.traversals), work);
        let reference = references[0];
        let source = app
            .world_mut()
            .spawn((SourceClips::default(), ChildOf(reference)))
            .id();
        app.update();
        assert_eq!(
            app.world()
                .get::<SourceClipBinding>(reference)
                .unwrap()
                .source,
            source
        );
        assert_eq!(app.world().resource::<SourceClipIndex>().nodes.len(), 1);
        let bound_work = {
            let index = app.world().resource::<SourceClipIndex>();
            (index.journal_entities, index.traversals)
        };
        for _ in 0..8 {
            app.update();
        }
        let index = app.world().resource::<SourceClipIndex>();
        assert_eq!((index.journal_entities, index.traversals), bound_work);
        // Replacement, removal and return are lifecycle events, including empty inventories.
        app.world_mut()
            .entity_mut(source)
            .insert(SourceClips::default());
        app.update();
        assert!(app.world().get::<SourceClipBinding>(reference).is_some());
        app.world_mut().entity_mut(source).remove::<SourceClips>();
        app.update();
        assert!(app.world().get::<SourceClipBinding>(reference).is_none());
        let index = app.world().resource::<SourceClipIndex>();
        assert!(index.nodes.is_empty() && index.node_users.is_empty());
        assert!(index.journal_entities > work.0 && index.traversals > work.1);
        let work = (index.journal_entities, index.traversals);
        app.update();
        let index = app.world().resource::<SourceClipIndex>();
        assert_eq!((index.journal_entities, index.traversals), work);
    }

    #[test]
    fn lifecycle_journal_rebinds_mesh_and_door_roles_without_polling() {
        let mut app = resolver_app();
        let reference = app
            .world_mut()
            .spawn((MeshHandle("roles.glb".into()), FormId(7)))
            .id();
        let mut clip = AnimationClip::default();
        clip.set_duration(2.0);
        let clip = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(clip);
        let source = app
            .world_mut()
            .spawn((
                SourceClips(vec![("Idle".into(), clip.clone()), ("Open".into(), clip)]),
                AnimationPlayer::default(),
                ChildOf(reference),
            ))
            .id();
        app.update();
        assert!(
            app.world()
                .get::<SourceClipBinding>(reference)
                .unwrap()
                .idle
                .is_some()
        );
        app.world_mut().entity_mut(reference).insert(LoadDoor {
            ref_id: 7,
            destination: crate::doors::DoorDestination {
                destination_ref_id: 8,
                interior_cell_id: Some(9),
                worldspace_id: None,
                arrival_position: [0.0; 3],
                arrival_rotation: [0.0; 3],
            },
        });
        app.update();
        let binding = app.world().get::<SourceClipBinding>(reference).unwrap();
        assert!(binding.open.is_some() && binding.idle.is_none());
        let old = binding.graph.clone();
        app.world_mut()
            .entity_mut(reference)
            .insert(MeshHandle("replacement.glb".into()));
        app.update();
        assert_ne!(
            app.world()
                .get::<SourceClipBinding>(reference)
                .unwrap()
                .graph,
            old
        );
        app.world_mut().entity_mut(reference).remove::<LoadDoor>();
        app.update();
        let binding = app.world().get::<SourceClipBinding>(reference).unwrap();
        assert!(binding.open.is_none() && binding.idle.is_some());
        app.world_mut().entity_mut(reference).remove::<MeshHandle>();
        app.update();
        assert!(app.world().get::<SourceClipBinding>(reference).is_none());
        assert!(app.world().get::<AnimationGraphHandle>(source).is_none());
        app.world_mut()
            .entity_mut(reference)
            .insert(MeshHandle("restored.glb".into()));
        app.update();
        assert!(
            app.world()
                .get::<SourceClipBinding>(reference)
                .unwrap()
                .idle
                .is_some()
        );
        app.world_mut().entity_mut(source).despawn();
        app.update();
        assert!(app.world().get::<SourceClipBinding>(reference).is_none());
        let index = app.world().resource::<SourceClipIndex>();
        assert!(
            index.nodes.is_empty() && index.node_users.is_empty() && index.clip_users.is_empty()
        );
    }
    #[test]
    fn ambiguous_inventories_never_choose_by_child_order_and_recover_on_membership_changes() {
        let mut app = resolver_app();
        app.add_plugins(crate::door_animation::DoorAnimationPlugin);
        let reference = app
            .world_mut()
            .spawn((
                MeshHandle("ambiguous.glb".into()),
                LoadDoor {
                    ref_id: 7,
                    destination: crate::doors::DoorDestination {
                        destination_ref_id: 8,
                        interior_cell_id: Some(9),
                        worldspace_id: None,
                        arrival_position: [0.0; 3],
                        arrival_rotation: [0.0; 3],
                    },
                },
            ))
            .id();
        let other = app.world_mut().spawn(MeshHandle("other.glb".into())).id();
        app.world_mut()
            .write_message(crate::doors::OpenDoor { door: reference });
        app.update();
        assert_eq!(
            app.world()
                .resource::<crate::door_animation::PendingDoorOpens>()
                .0,
            vec![reference]
        );

        let empty = app
            .world_mut()
            .spawn((SourceClips::default(), ChildOf(reference)))
            .id();
        let mut clip = AnimationClip::default();
        clip.set_duration(2.0);
        let clip = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(clip);
        let animated = app
            .world_mut()
            .spawn((
                SourceClips(vec![("Open".into(), clip)]),
                AnimationPlayer::default(),
                ChildOf(reference),
            ))
            .id();
        // Permanent failure must wake and retire the ALREADY pending request without a new press.
        app.update();
        assert!(app.world().get::<InvalidSourceClip>(reference).is_some());
        assert!(app.world().get::<SourceClipBinding>(reference).is_none());
        assert!(app.world().get::<AnimationGraphHandle>(animated).is_none());
        assert!(
            app.world()
                .resource::<crate::door_animation::PendingDoorOpens>()
                .0
                .is_empty()
        );
        let work = app.world().resource::<SourceClipIndex>().traversals;
        // Supported membership-preserving APIs deliberately produce no lifecycle event.
        app.world_mut()
            .get_mut::<Children>(reference)
            .unwrap()
            .swap(0, 1);
        app.update();
        assert_eq!(app.world().resource::<SourceClipIndex>().traversals, work);
        assert!(app.world().get::<InvalidSourceClip>(reference).is_some());
        assert!(app.world().get::<SourceClipBinding>(reference).is_none());
        app.world_mut()
            .get_mut::<Children>(reference)
            .unwrap()
            .sort_by_key(|entity| entity.to_bits());
        // Also revalidate after an input event with the new order: neither source is authority.
        app.world_mut()
            .entity_mut(empty)
            .insert(SourceClips::default());
        app.update();
        assert!(app.world().get::<InvalidSourceClip>(reference).is_some());
        assert!(app.world().get::<SourceClipBinding>(reference).is_none());
        assert!(app.world().resource::<SourceClipIndex>().nodes.is_empty());
        app.world_mut().entity_mut(empty).remove::<SourceClips>();
        app.update();
        let binding = app.world().get::<SourceClipBinding>(reference).unwrap();
        assert_eq!(binding.source, animated);
        assert!(binding.open.is_some());
        assert!(app.world().get::<InvalidSourceClip>(reference).is_none());
        app.world_mut()
            .entity_mut(empty)
            .insert(SourceClips::default());
        app.update();
        assert!(app.world().get::<SourceClipBinding>(reference).is_none());
        assert!(app.world().get::<InvalidSourceClip>(reference).is_some());
        assert!(app.world().get::<AnimationGraphHandle>(animated).is_none());
        // No ambiguous node index exists: removal must remember ancestry before reparenting.
        app.world_mut().entity_mut(empty).insert(ChildOf(other));
        app.update();
        assert_eq!(
            app.world()
                .get::<SourceClipBinding>(reference)
                .unwrap()
                .source,
            animated
        );
        assert_eq!(
            app.world().get::<SourceClipBinding>(other).unwrap().source,
            empty
        );
        assert!(app.world().get::<InvalidSourceClip>(reference).is_none());
        app.world_mut().entity_mut(empty).insert(ChildOf(reference));
        app.update();
        assert!(app.world().get::<SourceClipBinding>(other).is_none());
        assert!(app.world().get::<InvalidSourceClip>(reference).is_some());
        app.world_mut().entity_mut(animated).remove::<ChildOf>();
        app.update();
        assert_eq!(
            app.world()
                .get::<SourceClipBinding>(reference)
                .unwrap()
                .source,
            empty
        );
        assert!(app.world().get::<InvalidSourceClip>(reference).is_none());
    }

    #[test]
    fn inventory_search_reuses_owned_scratch_across_unresolved_references_and_dirty_frames() {
        let mut app = resolver_app();
        let mut references = Vec::new();
        for _ in 0..32 {
            let reference = app.world_mut().spawn(MeshHandle("scratch.glb".into())).id();
            for _ in 0..64 {
                app.world_mut().spawn(ChildOf(reference));
            }
            references.push(reference);
        }
        app.update();
        let (capacity, pointer, traversals) = {
            let index = app.world().resource::<SourceClipIndex>();
            assert!(index.nodes.is_empty() && index.node_users.is_empty());
            assert!(index.traversal.capacity() >= 65);
            assert_eq!(index.traversals, references.len());
            (
                index.traversal.capacity(),
                index.traversal.as_ptr(),
                index.traversals,
            )
        };
        for reference in &references {
            app.world_mut()
                .entity_mut(*reference)
                .insert(MeshHandle("dirty.glb".into()));
        }
        app.update();
        let index = app.world().resource::<SourceClipIndex>();
        assert!(index.nodes.is_empty() && index.node_users.is_empty());
        assert_eq!(index.traversal.capacity(), capacity);
        assert_eq!(index.traversal.as_ptr(), pointer);
        assert_eq!(index.traversals, traversals + references.len());
        let reference = references[0];
        app.world_mut()
            .entity_mut(reference)
            .insert(SourceClips::default());
        app.update();
        let index = app.world().resource::<SourceClipIndex>();
        assert_eq!(index.traversal.capacity(), capacity);
        assert_eq!(index.traversal.as_ptr(), pointer);
        assert_eq!(index.nodes[&reference].len(), 65);
        assert_eq!(index.nodes.len(), 1);
        assert_eq!(
            app.world()
                .get::<SourceClipBinding>(reference)
                .unwrap()
                .source,
            reference
        );
    }
}
