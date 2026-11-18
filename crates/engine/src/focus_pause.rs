//! Focus is an independent pause reason. Menu policy remains with its owning UI.

use crate::{config::EngineConfig, console::ConsoleState, settings_ui::SettingsMenuState};
use bevy::{prelude::*, time::TimeSystems, window::PrimaryWindow};

/// Small focus-reason adapter for the existing menu pause owners.
/// A future shared pause authority can consume `active` directly.
#[derive(Resource, Default)]
pub(crate) struct FocusPause {
    /// Whether primary-window focus currently owns an independent pause reason.
    pub(crate) active: bool,
    /// Pause that existed before focus was lost, restored on refocus.
    previous_pause: bool,
}

impl FocusPause {
    /// Snapshot the pause that exists independently of this focus reason.
    pub(crate) fn without_focus(&self, time: &Time<Virtual>) -> bool {
        if self.active {
            self.previous_pause
        } else {
            time.is_paused()
        }
    }
}

/// Pause on primary-window focus loss and consume background time before resuming.
pub(crate) struct FocusPausePlugin;

impl Plugin for FocusPausePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FocusPause>()
            .add_systems(First, acquire_focus_pause.before(TimeSystems))
            .add_systems(First, release_focus_pause.after(TimeSystems));
    }
}

/// Only the primary game window participates. Headless apps have no focus reason.
fn wants_pause(
    windows: &Query<&Window, With<PrimaryWindow>>,
    config: Option<&EngineConfig>,
) -> bool {
    // Collision measurement explicitly enables physics for its automated route.
    #[cfg(feature = "collision-measure")]
    if std::env::var_os("MUDCRAB_COLLISION_MEASURE").is_some() {
        return false;
    }
    // Automated/hidden capture modes retain their explicit simulation policy.
    !config.is_some_and(|config| config.always_active || !config.interactive_world_physics())
        && windows.single().is_ok_and(|window| !window.focused)
}

/// Existing menus own their pause until closed; closing-frame input ownership is separate.
fn menu_open(console: Option<&ConsoleState>, menu: Option<&SettingsMenuState>) -> bool {
    console.is_some_and(|console| console.open) || menu.is_some_and(|menu| menu.open)
}

/// Acquire before virtual time advances, including an initially unfocused primary window.
#[allow(clippy::too_many_arguments)]
fn acquire_focus_pause(
    windows: Query<&Window, With<PrimaryWindow>>,
    config: Option<Res<EngineConfig>>,
    console: Option<Res<ConsoleState>>,
    menu: Option<Res<SettingsMenuState>>,
    mut reason: ResMut<FocusPause>,
    mut time: ResMut<Time<Virtual>>,
    mut capture: Option<ResMut<crate::physics::CursorCapture>>,
    mut motion: crate::physics::MotionReset,
) {
    if windows.single().is_ok_and(|window| !window.focused) {
        if let Some(capture) = capture.as_deref_mut() {
            *capture = crate::physics::CursorCapture::Released;
        }
        motion.clear_input();
    }
    if wants_pause(&windows, config.as_deref()) && !reason.active {
        reason.previous_pause =
            if let Some(console) = console.as_deref().filter(|console| console.open) {
                console.was_paused
            } else if let Some(menu) = menu.as_deref().filter(|menu| menu.open) {
                menu.was_paused
            } else {
                time.is_paused()
            };
        reason.active = true;
        info!(
            "Paused: primary window is not focused; use --always-active to keep world simulation running"
        );
    }
    // Keep the returning frame paused through TimeSystems: its real delta includes background time.
    if reason.active {
        time.pause();
    }
}

/// Release only focus after the clock has consumed the returning frame with zero virtual delta.
fn release_focus_pause(
    windows: Query<&Window, With<PrimaryWindow>>,
    config: Option<Res<EngineConfig>>,
    console: Option<Res<ConsoleState>>,
    menu: Option<Res<SettingsMenuState>>,
    mut reason: ResMut<FocusPause>,
    mut time: ResMut<Time<Virtual>>,
) {
    if reason.active && !wants_pause(&windows, config.as_deref()) {
        reason.active = false;
        if !reason.previous_pause && !menu_open(console.as_deref(), menu.as_deref()) {
            time.unpause();
        }
    }
}

#[cfg(all(test, feature = "collision-measure"))]
mod tests {
    use super::*;

    /// Run the override in a subprocess so process-global environment never races other tests.
    #[test]
    fn pause704_collision_measure_keeps_unfocused_physics_active() {
        if std::env::var_os("MUDCRAB_PAUSE704_MEASURE_CHILD").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "focus_pause::tests::pause704_collision_measure_keeps_unfocused_physics_active",
                    "--nocapture",
                ])
                .env("MUDCRAB_PAUSE704_MEASURE_CHILD", "1")
                .env("MUDCRAB_COLLISION_MEASURE", "1")
                .status()
                .unwrap();
            assert!(
                result.success(),
                "collision measurement must stay active without window focus"
            );
            return;
        }
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(FocusPausePlugin)
            .insert_resource(EngineConfig::default());
        app.world_mut().spawn((
            Window {
                focused: false,
                ..default()
            },
            PrimaryWindow,
        ));
        app.update();
        assert!(
            !app.world().resource::<Time<Virtual>>().is_paused(),
            "automated collision measurement must not pause"
        );
    }
}
