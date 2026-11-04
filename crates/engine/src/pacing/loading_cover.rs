//! Neutral control and presentation state for explicit covered loading.
//! Admission owns readiness and refusal; physics only requests a transition.

use bevy::prelude::*;

#[derive(Component)]
struct TeleportLoadingCover;

/// Installed by the loading owner. Its absence leaves ordinary controls unchanged.
#[derive(Resource, Default)]
pub(crate) struct LoadingCover {
    startup_pending: bool,
    crossing_pending: bool,
    teleport: Option<Entity>,
    generation: u64,
}

impl LoadingCover {
    pub(crate) fn controls_blocked(&self) -> bool {
        self.startup_pending || self.crossing_pending || self.teleport_pending()
    }

    pub(crate) fn set_startup_pending(&mut self, pending: bool) {
        self.startup_pending = pending;
    }

    pub(crate) fn crossing_pending(&self) -> bool {
        self.crossing_pending
    }

    pub(crate) fn set_crossing_pending(&mut self, pending: bool) {
        self.crossing_pending = pending;
    }

    pub(crate) fn teleport_pending(&self) -> bool {
        self.teleport.is_some()
    }

    pub(crate) fn teleport_generation(&self) -> Option<u64> {
        self.teleport.map(|_| self.generation)
    }

    /// Raise the cover before the camera is moved. A newer request keeps the same
    /// presentation node but requires its own admission result.
    pub(crate) fn begin_teleport(&mut self, commands: &mut Commands) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        if self.teleport.is_none() {
            self.teleport = Some(
                commands
                    .spawn((
                        Name::new("Teleport loading cover"),
                        TeleportLoadingCover,
                        Node {
                            position_type: PositionType::Absolute,
                            width: Val::Percent(100.0),
                            height: Val::Percent(100.0),
                            ..default()
                        },
                        BackgroundColor(Color::BLACK),
                        GlobalZIndex(i32::MAX - 1),
                    ))
                    .id(),
            );
        }
        self.generation
    }

    /// Only the current request can release its cover. Readiness policy belongs
    /// to the caller; an older completion never releases a newer transition.
    pub(crate) fn finish_teleport(&mut self, generation: u64, commands: &mut Commands) -> bool {
        if self.teleport_generation() != Some(generation) {
            return false;
        }
        if let Some(cover) = self.teleport.take() {
            commands.entity(cover).try_despawn();
        }
        true
    }
}

pub(crate) fn loading_controls_allowed(cover: Option<Res<LoadingCover>>) -> bool {
    cover.is_none_or(|cover| !cover.controls_blocked())
}

#[cfg(test)]
pub(crate) fn teleport_cover_count(world: &mut World) -> usize {
    world
        .query_filtered::<Entity, With<TeleportLoadingCover>>()
        .iter(world)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn begin(app: &mut App) -> u64 {
        app.world_mut()
            .run_system_once(|mut cover: ResMut<LoadingCover>, mut commands: Commands| {
                cover.begin_teleport(&mut commands)
            })
            .unwrap()
    }

    fn finish(app: &mut App, generation: u64) -> bool {
        app.world_mut()
            .run_system_once(
                move |mut cover: ResMut<LoadingCover>, mut commands: Commands| {
                    cover.finish_teleport(generation, &mut commands)
                },
            )
            .unwrap()
    }

    #[test]
    fn ordinary_controls_remain_available_without_a_loading_owner() {
        let mut app = App::new();
        assert!(
            app.world_mut()
                .run_system_once(loading_controls_allowed)
                .unwrap()
        );
        app.init_resource::<LoadingCover>();
        assert!(
            app.world_mut()
                .run_system_once(loading_controls_allowed)
                .unwrap()
        );
    }

    #[test]
    fn an_older_completion_cannot_release_a_newer_teleport_cover() {
        let mut app = App::new();
        app.init_resource::<LoadingCover>();
        let first = begin(&mut app);
        let current = begin(&mut app);
        assert_ne!(first, current);
        assert!(!finish(&mut app, first));
        assert_eq!(
            app.world().resource::<LoadingCover>().teleport_generation(),
            Some(current)
        );
        assert_eq!(
            app.world_mut()
                .query_filtered::<Entity, With<TeleportLoadingCover>>()
                .iter(app.world())
                .count(),
            1
        );
        assert!(finish(&mut app, current));
        assert!(!app.world().resource::<LoadingCover>().controls_blocked());
        assert_eq!(
            app.world_mut()
                .query_filtered::<Entity, With<TeleportLoadingCover>>()
                .iter(app.world())
                .count(),
            0
        );
        let next = begin(&mut app);
        assert_ne!(next, current);
        assert!(!finish(&mut app, current));
        assert!(finish(&mut app, next));
    }

    #[test]
    fn startup_and_teleport_keep_controls_held_until_both_owners_release() {
        for finish_teleport_first in [false, true] {
            let mut app = App::new();
            app.init_resource::<LoadingCover>();
            app.world_mut()
                .resource_mut::<LoadingCover>()
                .set_startup_pending(true);
            let generation = begin(&mut app);
            assert!(
                !app.world_mut()
                    .run_system_once(loading_controls_allowed)
                    .unwrap()
            );
            if finish_teleport_first {
                assert!(finish(&mut app, generation));
            } else {
                app.world_mut()
                    .resource_mut::<LoadingCover>()
                    .set_startup_pending(false);
            }
            assert!(
                !app.world_mut()
                    .run_system_once(loading_controls_allowed)
                    .unwrap()
            );
            if finish_teleport_first {
                app.world_mut()
                    .resource_mut::<LoadingCover>()
                    .set_startup_pending(false);
            } else {
                assert!(finish(&mut app, generation));
            }
            assert!(
                app.world_mut()
                    .run_system_once(loading_controls_allowed)
                    .unwrap()
            );
        }
    }
}
