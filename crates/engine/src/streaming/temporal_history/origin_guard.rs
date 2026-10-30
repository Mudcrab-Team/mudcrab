//! Debug-only schedule boundary checks for temporal coordinate ownership.

use super::RenderOrigin;
use bevy::{app::MainScheduleOrder, ecs::schedule::ScheduleLabel, prelude::*};

#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
struct BeforeOriginUpdate;

#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
struct AfterOriginUpdate;

#[derive(Resource, Default)]
pub(super) struct SealedOrigin(Option<IVec2>);

pub(super) fn install(app: &mut App) {
    app.init_resource::<SealedOrigin>()
        .add_systems(BeforeOriginUpdate, check_before_update)
        .add_systems(AfterOriginUpdate, seal_after_update);
    let mut order = app.world_mut().resource_mut::<MainScheduleOrder>();
    order.insert_before(Update, BeforeOriginUpdate);
    order.insert_after(Update, AfterOriginUpdate);
}

fn check_before_update(origin: Res<RenderOrigin>, sealed: Res<SealedOrigin>) {
    // The first frame permits Startup initialization. Later frames cover First,
    // PreUpdate and fixed schedules as well as the preceding frame's Last.
    assert_origin(&origin, Some(&sealed));
}

fn seal_after_update(origin: Res<RenderOrigin>, mut sealed: ResMut<SealedOrigin>) {
    sealed.0 = Some(origin.0);
}

pub(super) fn assert_origin(origin: &RenderOrigin, sealed: Option<&SealedOrigin>) {
    if let Some(expected) = sealed.and_then(|sealed| sealed.0) {
        debug_assert_eq!(
            origin.0, expected,
            "RenderOrigin changed outside Update; queue the complete rebase into Update"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change_origin(mut origin: ResMut<RenderOrigin>) {
        origin.0 += IVec2::X;
    }

    fn app() -> App {
        let mut app = App::new();
        app.insert_resource(RenderOrigin(IVec2::ZERO));
        install(&mut app);
        app.update();
        app
    }

    #[test]
    fn update_origin_changes_and_unchanged_frames_pass_the_boundary_guard() {
        let mut app = app();
        app.update();
        app.add_systems(Update, change_origin);
        app.update();
        app.update();
        assert_eq!(app.world().resource::<RenderOrigin>().0, IVec2::new(2, 0));
    }

    #[test]
    #[should_panic(expected = "RenderOrigin changed outside Update")]
    fn preupdate_origin_change_is_rejected() {
        let mut app = app();
        app.add_systems(PreUpdate, change_origin);
        app.update();
    }

    #[test]
    #[should_panic(expected = "RenderOrigin changed outside Update")]
    fn postupdate_origin_change_is_rejected_before_the_next_update() {
        let mut app = app();
        app.add_systems(PostUpdate, change_origin);
        app.update();
        app.update();
    }

    #[test]
    #[should_panic(expected = "RenderOrigin changed outside Update")]
    fn last_origin_change_is_rejected_before_the_next_update() {
        let mut app = app();
        app.add_systems(Last, change_origin);
        app.update();
        app.update();
    }

    #[test]
    #[should_panic(expected = "RenderOrigin changed outside Update")]
    fn fixed_schedule_origin_change_is_rejected() {
        let mut app = app();
        app.add_systems(FixedUpdate, change_origin);
        app.world_mut().run_schedule(FixedUpdate);
        app.update();
    }
}
