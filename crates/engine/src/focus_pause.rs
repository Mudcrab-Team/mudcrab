//! Focus is an independent pause reason. Menu policy remains with its owning UI.

use crate::{config::EngineConfig, console::ConsoleState, settings_ui::SettingsMenuState};
use bevy::{prelude::*, time::TimeSystems, window::PrimaryWindow};

/// Small focus-reason adapter for the existing menu pause owners.
/// A future shared pause authority can consume `active` directly.
#[derive(Resource, Default)]
pub(crate) struct FocusPause {
    pub(crate) active: bool,
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
