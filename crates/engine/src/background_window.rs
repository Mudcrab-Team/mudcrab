//! A `--background-window` run must not change the user's Space, focus or frontmost app.
//!
//! The window itself is already unfocused and at the bottom of the window stack (`app.rs`). Two
//! things still reach the user on macOS:
//!
//! - winit activates the app when it finishes launching, and the app was seen taking focus again
//!   later. The earlier fix called `NSApplication.deactivate()`, which lets macOS pick the next app
//!   to activate. That app can sit on another Space, and macOS then switches to it. This module
//!   instead remembers the app that was frontmost before the engine, and hands focus back to
//!   exactly that app whenever the engine finds itself frontmost.
//! - An app that is activated while it has no window on the current Space makes macOS switch to a
//!   Space where it has one. A window that joins every Space, including full-screen Spaces, is on
//!   the current Space whichever one that is.
//!
//! The decision of whom to hand focus to is plain data ([`FocusMemory`]) so tests can drive it
//! without AppKit.

use bevy::prelude::*;

/// Remembers the last frontmost app that was not this process.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct FocusMemory {
    last_other: Option<i32>,
}

impl FocusMemory {
    /// Records the frontmost app (`front`, a process id) and says whom to hand focus to.
    ///
    /// Another app in front is remembered and left alone. This process in front gives back the
    /// remembered app, if there is one. No frontmost app (the user is at a login window, say)
    /// gives nothing.
    pub(crate) fn observe(&mut self, front: Option<i32>, own: i32) -> Option<i32> {
        match front {
            Some(pid) if pid != own => {
                self.last_other = Some(pid);
                None
            }
            Some(_) => self.last_other,
            None => None,
        }
    }

    /// Forgets the remembered app, for one that has quit.
    pub(crate) fn forget(&mut self) {
        self.last_other = None;
    }
}

/// State for a background run: what the user had in front, and which windows were already placed.
#[derive(Resource, Default)]
pub(crate) struct BackgroundFocus {
    memory: FocusMemory,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    placed_windows: std::collections::HashSet<isize>,
}

impl BackgroundFocus {
    /// Reads the frontmost app now. Call it before the event loop starts, because winit activates
    /// the app while it launches and the user's app is the one to give focus back to.
    pub(crate) fn capture() -> Self {
        let mut focus = Self::default();
        focus
            .memory
            .observe(platform::frontmost_pid(), platform::own_pid());
        focus
    }
}

/// Per frame, in a background run: put the window on every Space, and give focus back if the
/// engine has it.
pub(crate) fn hold_focus_with_the_user(
    config: Res<crate::config::EngineConfig>,
    mut focus: ResMut<BackgroundFocus>,
    _main_thread: bevy::ecs::system::NonSendMarker,
) {
    if !config.background_window {
        return;
    }
    let focus = &mut *focus;
    platform::place_windows_on_every_space(&mut focus.placed_windows);
    if let Some(pid) = focus
        .memory
        .observe(platform::frontmost_pid(), platform::own_pid())
        && !platform::hand_focus_to(pid)
    {
        focus.memory.forget();
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationOptions, NSRunningApplication,
        NSWindowCollectionBehavior, NSWorkspace,
    };
    use objc2_foundation::MainThreadMarker;
    use std::collections::HashSet;

    /// On every Space (also full-screen ones), out of Mission Control and window cycling, and not
    /// moving when the user changes Space. `CanJoinAllSpaces` and `MoveToActiveSpace` exclude each
    /// other, so only the first is set.
    pub(super) const SPACE_BEHAVIOUR: NSWindowCollectionBehavior = NSWindowCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces.0
            | NSWindowCollectionBehavior::Stationary.0
            | NSWindowCollectionBehavior::IgnoresCycle.0
            | NSWindowCollectionBehavior::FullScreenAuxiliary.0,
    );

    pub(super) fn own_pid() -> i32 {
        std::process::id() as i32
    }

    pub(super) fn frontmost_pid() -> Option<i32> {
        // SAFETY: `frontmostApplication` only reads the workspace's current state.
        unsafe {
            NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .map(|app| app.processIdentifier())
        }
    }

    /// Activates the app with this process id without asking to ignore other apps. The engine is
    /// the active app when this runs, so macOS lets it pass activation on. False when the app is
    /// gone.
    pub(super) fn hand_focus_to(pid: i32) -> bool {
        // SAFETY: both calls only message a running application object.
        unsafe {
            match NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
                Some(app) => {
                    app.activateWithOptions(NSApplicationActivationOptions(0));
                    true
                }
                None => false,
            }
        }
    }

    pub(super) fn place_windows_on_every_space(placed: &mut HashSet<isize>) {
        let Some(main_thread) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(main_thread);
        for window in app.windows() {
            // SAFETY: main-thread-only window calls, and the marker above proves the main thread.
            unsafe {
                if placed.insert(window.windowNumber()) {
                    window.setCollectionBehavior(SPACE_BEHAVIOUR);
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::collections::HashSet;

    pub(super) fn own_pid() -> i32 {
        std::process::id() as i32
    }

    /// Other platforms honour the unfocused window attribute, so there is no focus to give back.
    pub(super) fn frontmost_pid() -> Option<i32> {
        None
    }

    pub(super) fn hand_focus_to(_pid: i32) -> bool {
        false
    }

    pub(super) fn place_windows_on_every_space(_placed: &mut HashSet<isize>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: i32 = 100;

    #[test]
    fn another_app_in_front_is_remembered_and_left_alone() {
        let mut memory = FocusMemory::default();
        assert_eq!(memory.observe(Some(7), ME), None);
        assert_eq!(memory.observe(Some(8), ME), None);
    }

    #[test]
    fn the_engine_in_front_gives_focus_back_to_the_app_that_had_it() {
        let mut memory = FocusMemory::default();
        memory.observe(Some(7), ME);
        assert_eq!(memory.observe(Some(ME), ME), Some(7));
        // Still in front on the next frame: the same app again, not an arbitrary one.
        assert_eq!(memory.observe(Some(ME), ME), Some(7));
    }

    #[test]
    fn the_app_the_user_switched_to_in_the_meantime_is_the_one_to_return_to() {
        let mut memory = FocusMemory::default();
        memory.observe(Some(7), ME);
        memory.observe(Some(9), ME);
        assert_eq!(memory.observe(Some(ME), ME), Some(9));
    }

    #[test]
    fn nothing_is_remembered_nothing_is_handed_back() {
        let mut memory = FocusMemory::default();
        assert_eq!(memory.observe(Some(ME), ME), None);
        assert_eq!(memory.observe(None, ME), None);
        memory.observe(Some(7), ME);
        assert_eq!(memory.observe(None, ME), None);
    }

    #[test]
    fn a_remembered_app_that_quit_is_forgotten() {
        let mut memory = FocusMemory::default();
        memory.observe(Some(7), ME);
        memory.forget();
        assert_eq!(memory.observe(Some(ME), ME), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_space_behaviour_joins_every_space_without_moving() {
        use objc2_app_kit::NSWindowCollectionBehavior as Behaviour;
        let behaviour = platform::SPACE_BEHAVIOUR;
        for wanted in [
            Behaviour::CanJoinAllSpaces,
            Behaviour::Stationary,
            Behaviour::IgnoresCycle,
            Behaviour::FullScreenAuxiliary,
        ] {
            assert!(behaviour.contains(wanted), "{wanted:?} missing");
        }
        // The system refuses a window that asks for both.
        assert!(!behaviour.contains(Behaviour::MoveToActiveSpace));
    }

    /// Reads the real frontmost app and checks that a call that must not change it does not.
    /// It opens no window and activates nothing: it only queries the workspace twice and runs the
    /// decision on the answer. The live check, with a real `--background-window` launch, is in
    /// `docs/contributing/perf-loop.md`.
    #[cfg(target_os = "macos")]
    #[test]
    fn querying_the_frontmost_app_does_not_change_it() {
        let before = platform::frontmost_pid();
        let mut memory = FocusMemory::default();
        let _ = memory.observe(before, platform::own_pid());
        assert_eq!(platform::frontmost_pid(), before);
    }
}
