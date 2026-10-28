//! First terminal outcome shared by cell and destination loading.

use bevy::prelude::*;

/// A terminal loading failure retained for diagnostics until the process exits.
/// Cell and destination loading share this outcome independently of the draw path.
#[derive(Resource, Debug)]
pub(crate) struct LoadingRefusal(pub String);

/// Publish the first terminal loading outcome at command application time.
/// Controllers still perform their own pacing and cover cleanup before calling this owner.
pub(crate) fn refuse_loading(world: &mut World, reason: String) {
    if world.contains_resource::<LoadingRefusal>() {
        return;
    }
    world.insert_resource(LoadingRefusal(reason));
    error!("{}", world.resource::<LoadingRefusal>().0);
    world.write_message(AppExit::error());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_refusal_preserves_the_first_reason_and_emits_one_error_exit() {
        let mut app = App::new();
        let mut exits = app
            .world()
            .resource::<bevy::ecs::message::Messages<AppExit>>()
            .get_cursor_current();
        refuse_loading(app.world_mut(), "first destination failed".to_owned());
        refuse_loading(app.world_mut(), "later timeout".to_owned());
        assert_eq!(
            app.world().resource::<LoadingRefusal>().0,
            "first destination failed"
        );
        let emitted: Vec<_> = exits
            .read(
                app.world()
                    .resource::<bevy::ecs::message::Messages<AppExit>>(),
            )
            .collect();
        assert_eq!(emitted.len(), 1);
        assert!(emitted[0].is_error());
    }
}
