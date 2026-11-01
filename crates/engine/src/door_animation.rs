//! Ordinary load-screen door swings. Crossing owns loading; animation owns only source clips.
use crate::{
    doors::{LoadDoor, OpenDoor},
    model_animation::{InvalidSourceClip, SourceClipBinding},
};
use bevy::{
    ecs::lifecycle::{Insert, Remove},
    prelude::*,
};

pub struct DoorAnimationPlugin;
/// At most the most recently accepted press may wait for source loading. A subsequent
/// press supersedes it, so doors that never resolve cannot accumulate retained requests.
#[derive(Resource, Default)]
pub struct PendingDoorOpens(pub Vec<Entity>);
#[derive(Resource, Default)]
struct DoorPlaybackChanged(bool);

fn door_playback_ready(
    _: On<Insert, (SourceClipBinding, InvalidSourceClip)>,
    mut changed: ResMut<DoorPlaybackChanged>,
) {
    changed.0 = true;
}

fn door_unloaded(_: On<Remove, LoadDoor>, mut changed: ResMut<DoorPlaybackChanged>) {
    changed.0 = true;
}
impl Plugin for DoorAnimationPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<OpenDoor>()
            .init_resource::<PendingDoorOpens>()
            .init_resource::<DoorPlaybackChanged>()
            .add_observer(door_playback_ready)
            .add_observer(door_unloaded)
            // Consume accepted keyboard requests after targeting and source binding,
            // without delaying crossing or requiring far-side residency.
            .add_systems(
                Update,
                activate_doors.after(crate::model_animation::resolve_source_clips),
            );
    }
}
fn activate_doors(
    mut open: MessageReader<OpenDoor>,
    mut pending: ResMut<PendingDoorOpens>,
    doors: Query<(Option<&SourceClipBinding>, Has<InvalidSourceClip>), With<LoadDoor>>,
    mut changed: ResMut<DoorPlaybackChanged>,
    mut players: Query<&mut AnimationPlayer>,
) {
    let ready = std::mem::take(&mut changed.0);
    if open.is_empty() && !ready {
        // Empty event lists do no per-door work, including a delayed loading request.
        return;
    }
    let mut keep_pending = |door: Entity| {
        let Ok((binding, invalid)) = doors.get(door) else {
            return false;
        };
        if invalid {
            return false;
        }
        let Some(binding) = binding else {
            return true;
        };
        // A resolved inventory with no Open clip is an authored static leaf.
        let Some(node) = binding.open else {
            return false;
        };
        if binding.players.is_empty() || binding.players.iter().any(|p| players.get(*p).is_err()) {
            return true;
        }
        for player in &binding.players {
            if let Ok(mut player) = players.get_mut(*player) {
                player.stop_all();
                player.start(node);
            }
        }
        false
    };
    let mut requested = false;
    for request in open.read() {
        requested = true;
        pending.0.clear();
        if keep_pending(request.door) {
            pending.0.push(request.door);
        }
    }
    if !requested {
        pending.0.retain(|door| keep_pending(*door));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        doors::DoorDestination,
        model_animation::{ModelAnimationPlugin, SourceClips},
        world::components::{FormId, MeshHandle},
    };
    use bevy::{
        animation::{
            AnimatedBy, AnimationPlugin, AnimationTargetId, animated_field,
            animation_curves::{AnimatableCurve, AnimatableKeyframeCurve},
            graph::{AnimationGraph, AnimationGraphHandle},
        },
        gltf::extensions::GltfExtensionHandlers,
        time::TimeUpdateStrategy,
    };
    use std::time::Duration;
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            TransformPlugin,
            AnimationPlugin,
        ))
        .init_resource::<GltfExtensionHandlers>()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            0.1,
        )))
        .add_plugins((ModelAnimationPlugin, DoorAnimationPlugin));
        app
    }
    fn door(app: &mut App) -> Entity {
        app.world_mut()
            .spawn((
                LoadDoor {
                    ref_id: 7,
                    destination: DoorDestination {
                        destination_ref_id: 8,
                        interior_cell_id: Some(9),
                        worldspace_id: None,
                        arrival_position: [0.0; 3],
                        arrival_rotation: [0.0; 3],
                    },
                },
                MeshHandle("meshes/test.glb".into()),
                FormId(7),
                Transform::default(),
            ))
            .id()
    }
    fn source(app: &mut App, reference: Entity, name: &str) -> (Entity, Entity) {
        let target = AnimationTargetId::from_name(&Name::new("leaf"));
        let mut clip = AnimationClip::default();
        clip.add_curve_to_target(
            target,
            AnimatableCurve::new(
                animated_field!(Transform::rotation),
                AnimatableKeyframeCurve::new([
                    (0.0, Quat::IDENTITY),
                    (1.0, Quat::from_rotation_y(1.0)),
                ])
                .unwrap(),
            ),
        );
        let handle = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .add(clip);
        let source = app
            .world_mut()
            .spawn((
                SourceClips(vec![(name.into(), handle)]),
                AnimationPlayer::default(),
                Transform::default(),
                ChildOf(reference),
            ))
            .id();
        let leaf = app
            .world_mut()
            .spawn((
                Name::new("leaf"),
                target,
                AnimatedBy(source),
                Transform::default(),
                ChildOf(source),
            ))
            .id();
        (source, leaf)
    }

    #[test]
    fn delayed_readiness_events_preserve_active_open_graph_time_and_transform() {
        for loaded_with_dependencies in [false, true] {
            let mut app = app();
            let door = door(&mut app);
            let (player, leaf) = source(&mut app, door, "Open");
            app.world_mut().write_message(OpenDoor { door });
            app.update();
            let binding = app.world().get::<SourceClipBinding>(door).unwrap().clone();
            let node = binding.open.unwrap();
            let elapsed = app
                .world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .animation(node)
                .unwrap()
                .elapsed();
            let rotation = app.world().get::<Transform>(leaf).unwrap().rotation;
            let id = app.world().get::<SourceClips>(player).unwrap().0[0].1.id();
            // Added from Assets::add was published in the preceding PostUpdate. Also
            // exercise a late dependency-ready notification after OpenDoor was consumed.
            let event = if loaded_with_dependencies {
                AssetEvent::<AnimationClip>::LoadedWithDependencies { id }
            } else {
                AssetEvent::<AnimationClip>::Added { id }
            };
            app.world_mut().write_message(event);
            app.update();
            let current = app.world().get::<SourceClipBinding>(door).unwrap();
            assert_eq!(current.graph, binding.graph);
            assert_eq!(current.open, Some(node));
            assert_eq!(
                app.world().get::<AnimationGraphHandle>(player).unwrap().0,
                *binding.graph.as_ref().unwrap()
            );
            let active = app
                .world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .animation(node)
                .unwrap();
            assert!(active.elapsed() > elapsed);
            assert_ne!(
                app.world().get::<Transform>(leaf).unwrap().rotation,
                rotation
            );
            assert_ne!(
                app.world().get::<Transform>(leaf).unwrap().rotation,
                Quat::IDENTITY
            );
            assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
        }
    }

    #[test]
    fn actual_clip_modification_and_removal_invalidate_before_event_driven_replacement() {
        let mut app = app();
        let door = door(&mut app);
        let (player, leaf) = source(&mut app, door, "Open");
        app.world_mut().write_message(OpenDoor { door });
        app.update();
        let original = app.world().get::<SourceClipBinding>(door).unwrap().clone();
        let handle = app.world().get::<SourceClips>(player).unwrap().0[0]
            .1
            .clone();
        let mut events = app
            .world()
            .resource::<Messages<AssetEvent<AnimationClip>>>()
            .get_cursor_current();
        // Actual mutation queues Modified. Update has not received it on this frame.
        app.world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .get_mut(&handle)
            .unwrap()
            .set_duration(2.0);
        app.update();
        assert!(
            events
                .read(
                    app.world()
                        .resource::<Messages<AssetEvent<AnimationClip>>>()
                )
                .any(|event| event.is_modified(handle.id()))
        );
        assert_eq!(
            app.world().get::<SourceClipBinding>(door).unwrap().graph,
            original.graph
        );
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .is_playing_animation(original.open.unwrap())
        );
        // The next Update consumes Modified, retires the old graph, and binds current data.
        app.update();
        let modified = app.world().get::<SourceClipBinding>(door).unwrap().clone();
        assert_ne!(modified.graph, original.graph);
        assert_eq!(
            app.world().get::<AnimationGraphHandle>(player).unwrap().0,
            *modified.graph.as_ref().unwrap()
        );
        assert_eq!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .playing_animations()
                .count(),
            0
        );
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
        app.world_mut().write_message(OpenDoor { door });
        app.update();
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .is_playing_animation(modified.open.unwrap())
        );
        let clip = app
            .world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .remove(handle.id())
            .unwrap();
        app.update();
        assert!(
            events
                .read(
                    app.world()
                        .resource::<Messages<AssetEvent<AnimationClip>>>()
                )
                .any(|event| event.is_removed(handle.id()))
        );
        assert_eq!(
            app.world().get::<SourceClipBinding>(door).unwrap().graph,
            modified.graph
        );
        app.world_mut().write_message(OpenDoor { door });
        app.update();
        assert!(app.world().get::<SourceClipBinding>(door).is_none());
        assert!(app.world().get::<AnimationGraphHandle>(player).is_none());
        assert_eq!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .playing_animations()
                .count(),
            0
        );
        assert_eq!(app.world().resource::<PendingDoorOpens>().0, vec![door]);
        app.world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .insert(handle.id(), clip)
            .unwrap();
        app.update();
        assert!(
            events
                .read(
                    app.world()
                        .resource::<Messages<AssetEvent<AnimationClip>>>()
                )
                .any(|event| event.is_added(handle.id()))
        );
        assert!(app.world().get::<SourceClipBinding>(door).is_none());
        assert!(app.world().get::<AnimationGraphHandle>(player).is_none());
        assert_eq!(app.world().resource::<PendingDoorOpens>().0, vec![door]);
        // Readiness now reaches Update: the queued request starts once on a fresh graph.
        app.update();
        let replacement = app.world().get::<SourceClipBinding>(door).unwrap().clone();
        assert_ne!(replacement.graph, modified.graph);
        assert_eq!(replacement.source, player);
        let node = replacement.open.unwrap();
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .is_playing_animation(node)
        );
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
        let elapsed = app
            .world()
            .get::<AnimationPlayer>(player)
            .unwrap()
            .animation(node)
            .unwrap()
            .elapsed();
        let rotation = app.world().get::<Transform>(leaf).unwrap().rotation;
        // Next graph-threading pass sees its Added event, then applies the real curve.
        app.update();
        assert_eq!(
            app.world().get::<SourceClipBinding>(door).unwrap().graph,
            replacement.graph
        );
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .animation(node)
                .unwrap()
                .elapsed()
                > elapsed
        );
        assert_ne!(
            app.world().get::<Transform>(leaf).unwrap().rotation,
            rotation
        );
    }

    #[test]
    fn ordinary_source_open_plays_a_real_clip_without_destination_residency() {
        let mut app = app();
        let door = door(&mut app);
        let (player, leaf) = source(&mut app, door, "Open");
        app.world_mut().write_message(OpenDoor { door });
        for _ in 0..4 {
            app.update();
        }
        let binding = app.world().get::<SourceClipBinding>(door).unwrap();
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .is_playing_animation(binding.open.unwrap())
        );
        assert_ne!(
            app.world().get::<Transform>(leaf).unwrap().rotation,
            Quat::IDENTITY
        );
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
        assert!(
            !app.world()
                .contains_resource::<crate::streaming::StreamingWorld>()
        );
    }
    #[test]
    fn unresolved_source_queues_without_static_answer_then_plays_once() {
        let mut app = app();
        let door = door(&mut app);
        app.world_mut().write_message(OpenDoor { door });
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(app.world().resource::<PendingDoorOpens>().0, vec![door]);
        assert!(app.world().get::<SourceClipBinding>(door).is_none());
        let (player, _) = source(&mut app, door, "open");
        app.update();
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
        let node = app
            .world()
            .get::<SourceClipBinding>(door)
            .unwrap()
            .open
            .unwrap();
        for _ in 0..3 {
            app.update();
        }
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .animation(node)
                .unwrap()
                .elapsed()
                > 0.15
        );
    }
    #[test]
    fn same_source_inventory_change_withdraws_playback_until_replacement_is_ready() {
        let mut app = app();
        let door = door(&mut app);
        let (player, _) = source(&mut app, door, "Open");
        app.world_mut().write_message(OpenDoor { door });
        app.update();
        let old = app.world().get::<SourceClipBinding>(door).unwrap().clone();
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .is_playing_animation(old.open.unwrap())
        );
        let clip = app
            .world()
            .resource::<Assets<AnimationClip>>()
            .get(&app.world().get::<SourceClips>(player).unwrap().0[0].1)
            .unwrap()
            .clone();
        let replacement = app
            .world()
            .resource::<Assets<AnimationClip>>()
            .reserve_handle();
        app.world_mut()
            .entity_mut(player)
            .insert(SourceClips(vec![("Open".into(), replacement.clone())]));
        app.world_mut().write_message(OpenDoor { door });
        app.update();
        assert!(app.world().get::<SourceClipBinding>(door).is_none());
        assert!(app.world().get::<AnimationGraphHandle>(player).is_none());
        assert_eq!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .playing_animations()
                .count(),
            0
        );
        assert_eq!(app.world().resource::<PendingDoorOpens>().0, vec![door]);
        let mut events = app
            .world()
            .resource::<Messages<AssetEvent<AnimationClip>>>()
            .get_cursor_current();
        app.world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .insert(replacement.id(), clip)
            .unwrap();
        // Assets::insert queues Added; AssetEventSystems publishes it in PostUpdate,
        // after this frame's Update resolver. Presence alone must not poll the subtree.
        app.update();
        assert!(
            events
                .read(
                    app.world()
                        .resource::<Messages<AssetEvent<AnimationClip>>>()
                )
                .any(|event| event.is_added(replacement.id()))
        );
        assert!(app.world().get::<SourceClipBinding>(door).is_none());
        assert!(app.world().get::<AnimationGraphHandle>(player).is_none());
        assert_eq!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .playing_animations()
                .count(),
            0
        );
        assert_eq!(app.world().resource::<PendingDoorOpens>().0, vec![door]);
        // The resolver consumes the actual readiness event in the next Update.
        app.update();
        let binding = app.world().get::<SourceClipBinding>(door).unwrap();
        assert_eq!(binding.source, player);
        assert_ne!(binding.graph, old.graph);
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .is_playing_animation(binding.open.unwrap())
        );
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
    }
    #[test]
    fn same_source_becoming_static_releases_its_authored_graph_and_playback() {
        let mut app = app();
        let door = door(&mut app);
        let (player, _) = source(&mut app, door, "Open");
        app.world_mut().write_message(OpenDoor { door });
        app.update();
        assert_eq!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .playing_animations()
                .count(),
            1
        );
        app.world_mut()
            .entity_mut(player)
            .insert(SourceClips::default());
        app.world_mut().write_message(OpenDoor { door });
        app.update();
        let binding = app.world().get::<SourceClipBinding>(door).unwrap();
        assert_eq!(binding.source, player);
        assert!(binding.graph.is_none() && binding.open.is_none() && binding.players.is_empty());
        assert!(app.world().get::<AnimationGraphHandle>(player).is_none());
        assert_eq!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .playing_animations()
                .count(),
            0
        );
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
    }
    #[test]
    fn detached_source_withdraws_binding_and_only_stops_its_authored_graph() {
        // A door animation handler may replace the graph on the same player. Public cleanup
        // must leave that graph and its playback alone when the source detaches.
        for derived in [false, true] {
            let mut app = app();
            let door = door(&mut app);
            let (player, leaf) = source(&mut app, door, "Open");
            app.world_mut().write_message(OpenDoor { door });
            app.update();
            let binding = app.world().get::<SourceClipBinding>(door).unwrap().clone();
            let graph = if derived {
                let clip = app.world().get::<SourceClips>(player).unwrap().0[0]
                    .1
                    .clone();
                let (graph, node) = AnimationGraph::from_clip(clip);
                let graph = app
                    .world_mut()
                    .resource_mut::<Assets<AnimationGraph>>()
                    .add(graph);
                app.world_mut()
                    .entity_mut(player)
                    .insert(AnimationGraphHandle(graph.clone()));
                app.world_mut()
                    .get_mut::<AnimationPlayer>(player)
                    .unwrap()
                    .stop_all()
                    .start(node);
                graph
            } else {
                binding.graph.unwrap()
            };
            app.world_mut().entity_mut(player).remove::<ChildOf>();
            app.world_mut().write_message(OpenDoor { door });
            app.update();
            assert!(app.world().get::<SourceClipBinding>(door).is_none());
            assert_eq!(app.world().resource::<PendingDoorOpens>().0, vec![door]);
            assert_eq!(
                app.world()
                    .get::<AnimationGraphHandle>(player)
                    .map(|h| &h.0),
                derived.then_some(&graph)
            );
            assert_eq!(
                app.world()
                    .get::<AnimationPlayer>(player)
                    .unwrap()
                    .playing_animations()
                    .count(),
                usize::from(derived)
            );
            let rotation = app.world().get::<Transform>(leaf).unwrap().rotation;
            app.update();
            if derived {
                assert_ne!(
                    app.world().get::<Transform>(leaf).unwrap().rotation,
                    rotation
                );
            } else {
                assert_eq!(
                    app.world().get::<Transform>(leaf).unwrap().rotation,
                    rotation
                );
            }
        }
    }
    #[test]
    fn unload_discards_pending_and_only_inventory_proves_a_static_leaf() {
        let mut app = app();
        let first = door(&mut app);
        app.world_mut().write_message(OpenDoor { door: first });
        app.update();
        app.world_mut().entity_mut(first).despawn();
        app.update();
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
        let second = door(&mut app);
        app.world_mut()
            .spawn((SourceClips::default(), ChildOf(second)));
        app.world_mut().write_message(OpenDoor { door: second });
        app.update();
        assert!(app.world().get::<SourceClipBinding>(second).is_some());
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
    }
    #[test]
    fn scene_replacement_rebinds_and_ambient_idle_uses_its_own_clip() {
        let mut app = app();
        let reference = app
            .world_mut()
            .spawn((
                MeshHandle("meshes/wheel.glb".into()),
                FormId(17),
                Transform::default(),
            ))
            .id();
        let (old, _) = source(&mut app, reference, "Idle");
        app.update();
        assert!(
            app.world()
                .get::<SourceClipBinding>(reference)
                .unwrap()
                .idle
                .is_some()
        );
        app.world_mut().entity_mut(old).despawn();
        let (new, leaf) = source(&mut app, reference, "Idle");
        for _ in 0..4 {
            app.update();
        }
        let binding = app.world().get::<SourceClipBinding>(reference).unwrap();
        assert_eq!(binding.players, vec![new]);
        assert_ne!(
            app.world().get::<Transform>(leaf).unwrap().rotation,
            Quat::IDENTITY
        );
    }

    #[test]
    fn next_press_replaces_unresolved_request_and_delayed_loading_still_plays() {
        let mut app = app();
        let first = door(&mut app);
        let second = door(&mut app);
        app.world_mut().write_message(OpenDoor { door: first });
        app.update();
        assert_eq!(app.world().resource::<PendingDoorOpens>().0, vec![first]);
        app.world_mut().write_message(OpenDoor { door: second });
        app.update();
        assert_eq!(app.world().resource::<PendingDoorOpens>().0, vec![second]);
        let (old_player, _) = source(&mut app, first, "Open");
        let (player, _) = source(&mut app, second, "Open");
        app.update();
        let node = app
            .world()
            .get::<SourceClipBinding>(second)
            .unwrap()
            .open
            .unwrap();
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .is_playing_animation(node)
        );
        assert_eq!(
            app.world()
                .get::<AnimationPlayer>(old_player)
                .unwrap()
                .playing_animations()
                .count(),
            0
        );
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
    }

    #[test]
    fn invalid_source_duration_discards_request_but_can_recover_on_a_new_press() {
        let mut app = app();
        let door = door(&mut app);
        let (player, _) = source(&mut app, door, "Open");
        let handle = app.world().get::<SourceClips>(player).unwrap().0[0]
            .1
            .clone();
        app.world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .get_mut(&handle)
            .unwrap()
            .set_duration(0.0);
        app.world_mut().write_message(OpenDoor { door });
        app.update();
        assert!(
            app.world()
                .get::<crate::model_animation::InvalidSourceClip>(door)
                .is_some()
        );
        assert!(app.world().get::<SourceClipBinding>(door).is_none());
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
        app.world_mut()
            .resource_mut::<Assets<AnimationClip>>()
            .get_mut(&handle)
            .unwrap()
            .set_duration(1.0);
        // Publish the invalidating event directly; this is not a frame-count readiness claim.
        app.world_mut()
            .write_message(AssetEvent::<AnimationClip>::Modified { id: handle.id() });
        app.update();
        let node = app
            .world()
            .get::<SourceClipBinding>(door)
            .unwrap()
            .open
            .unwrap();
        assert_eq!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .playing_animations()
                .count(),
            0
        );
        app.world_mut().write_message(OpenDoor { door });
        app.update();
        assert!(
            app.world()
                .get::<AnimationPlayer>(player)
                .unwrap()
                .is_playing_animation(node)
        );
        assert!(app.world().resource::<PendingDoorOpens>().0.is_empty());
    }
    #[test]
    fn ready_requests_in_one_message_batch_all_play_before_pending_is_superseded() {
        let mut app = app();
        let first = door(&mut app);
        let second = door(&mut app);
        let unresolved = door(&mut app);
        let (first_player, _) = source(&mut app, first, "Open");
        let (second_player, _) = source(&mut app, second, "Open");
        for door in [first, second, unresolved] {
            app.world_mut().write_message(OpenDoor { door });
        }
        app.update();
        for (door, player) in [(first, first_player), (second, second_player)] {
            let node = app
                .world()
                .get::<SourceClipBinding>(door)
                .unwrap()
                .open
                .unwrap();
            assert!(
                app.world()
                    .get::<AnimationPlayer>(player)
                    .unwrap()
                    .is_playing_animation(node)
            );
        }
        assert_eq!(
            app.world().resource::<PendingDoorOpens>().0,
            vec![unresolved]
        );
    }
}
