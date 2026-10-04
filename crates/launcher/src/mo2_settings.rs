//! Source selection; MO2 contributes paths, never a copied installation.
use crate::{
    LauncherSet,
    conversion::{
        CurrentConversion,
        panel::{BORDER, BUTTON_OFF, BUTTON_ON, PANEL, TEXT_COLOR},
        state,
    },
};
use bevy::prelude::*;
use std::path::Path;

#[derive(Resource, Default)]
pub struct SourceSettings {
    pub use_mo2: bool,
    pub selection: Option<mo2::Selection>,
    profiles: Vec<String>,
    pub open: bool,
    dropdown: bool,
    error: String,
}

impl SourceSettings {
    /// Loads an MO2 instance and its profiles, retaining the selected profile when still valid.
    ///
    /// Selects the first profile otherwise; an invalid instance clears the selection and records an error.
    pub fn set_instance(&mut self, path: &Path) {
        match mo2::Instance::open(path) {
            Ok(instance) => {
                let profile = self
                    .selection
                    .as_ref()
                    .filter(|old| old.instance_path == instance.instance_path)
                    .map(|old| old.profile.clone())
                    .filter(|name| instance.profiles.contains(name))
                    .unwrap_or_else(|| instance.profiles[0].clone());
                self.selection = Some(mo2::Selection {
                    instance_path: instance.instance_path,
                    profile,
                });
                self.profiles = instance.profiles;
                self.error.clear();
                self.dropdown = false;
            }
            Err(error) => {
                self.selection = None;
                self.profiles.clear();
                self.error = format!("{error:#}");
            }
        }
    }

    /// Applies the selected source to a pipeline config, revalidating an enabled MO2 instance and profile.
    pub fn apply(&self, config: &mut converter::PipelineConfig) -> Result<(), String> {
        config.mo2 = if self.use_mo2 {
            let selection = self.selection.as_ref().ok_or_else(|| {
                "Choose a valid MO2 instance and profile in MO2 settings first.".to_owned()
            })?;
            let instance =
                mo2::Instance::open(&selection.instance_path).map_err(|e| format!("{e:#}"))?;
            instance
                .profile_dir(&selection.profile)
                .map_err(|e| format!("{e:#}"))?;
            Some(selection.clone())
        } else {
            None
        };
        Ok(())
    }
}

#[derive(Component, Clone, Default)]
enum SourceButton {
    #[default]
    Skyrim,
    Mo2,
    Close,
    Dropdown,
    Profile(String),
}
#[derive(Component, Clone, Default)]
struct SourceLabel;
#[derive(Component)]
struct SettingsDialog;

/// Builds the source selector with Skyrim Data and MO2 buttons and a current-source label.
pub fn source_row() -> impl Scene {
    bsn! {
        Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(8.0), align_items: AlignItems::Center }
        Children [
            source_button("Skyrim Data", SourceButton::Skyrim),
            source_button("MO2 profile", SourceButton::Mo2),

            (SourceLabel Text::new("Source: Skyrim Data") TextFont { font_size: FontSize::Px(13.0) } TextColor(TEXT_COLOR))
        ]
    }
}

/// Builds a source-selection button carrying the requested action.
fn source_button(label: &'static str, action: SourceButton) -> impl Scene {
    bsn! {
        Button
        template_value(action)
        Node { padding: UiRect::all(Val::Px(6.0)) }
        BackgroundColor(BUTTON_OFF)
        Children [(Text::new(label) TextFont { font_size: FontSize::Px(14.0) } TextColor(TEXT_COLOR))]
    }
}

/// Formats a path for display, removing Windows extended-path prefixes while preserving UNC paths.
pub fn display_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    }
}

/// Reports whether source settings may be edited in the current conversion state.
pub fn editable(state: &state::ConversionState) -> bool {
    state::controls(state).paths || matches!(state, state::ConversionState::Stopped { .. })
}

/// Registers source-selection input and drawing systems in the launcher update schedule.
pub fn install(app: &mut App) {
    app.add_systems(Update, click_source.in_set(LauncherSet::Input))
        .add_systems(Update, draw_source.in_set(LauncherSet::Draw));
}

/// Handles source, profile, dropdown, and close actions while respecting conversion edit locks.
fn click_source(
    buttons: Query<(&Interaction, &SourceButton), Changed<Interaction>>,
    state: Res<CurrentConversion>,
    mut source: ResMut<SourceSettings>,
) {
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if matches!(button, SourceButton::Close) {
            source.open = false;
            continue;
        }
        if !editable(&state.0) {
            continue;
        }
        match button {
            SourceButton::Skyrim => {
                source.use_mo2 = false;
                source.open = false;
            }
            SourceButton::Mo2 => {
                source.use_mo2 = true;
                if source.selection.is_none() {
                    source.open = true;
                }
            }

            SourceButton::Dropdown => source.dropdown = !source.dropdown,
            SourceButton::Profile(name) => {
                if let Some(selection) = &mut source.selection {
                    selection.profile = name.clone();
                }
                source.dropdown = false;
            }
            SourceButton::Close => {}
        }
    }
}

/// Refreshes source labels and button colors and rebuilds the settings dialog when its state changes.
fn draw_source(
    mut commands: Commands,
    source: Res<SourceSettings>,
    state: Res<CurrentConversion>,
    mut buttons: Query<(&SourceButton, &mut BackgroundColor)>,
    mut labels: Query<&mut Text, With<SourceLabel>>,
    dialogs: Query<Entity, With<SettingsDialog>>,
) {
    let editable = editable(&state.0);
    for (button, mut color) in &mut buttons {
        let selected = match button {
            SourceButton::Skyrim => !source.use_mo2,
            SourceButton::Mo2 => source.use_mo2,
            _ => false,
        };
        color.0 = if editable && selected {
            BUTTON_ON
        } else {
            BUTTON_OFF
        };
    }
    for mut text in &mut labels {
        text.0 = if !source.use_mo2 {
            "Source: Skyrim Data".into()
        } else if let Some(selection) = &source.selection {
            format!("MO2: {}", selection.profile)
        } else {
            "MO2: not configured".into()
        };
    }
    if !source.is_changed() {
        return;
    }
    for dialog in &dialogs {
        commands.entity(dialog).despawn();
    }
    if !source.open || !source.use_mo2 {
        return;
    }
    commands.spawn((SettingsDialog, GlobalZIndex(100), Node {
        position_type: PositionType::Absolute,
        width: Val::Percent(100.0), height: Val::Percent(100.0),
        justify_content: JustifyContent::Center, align_items: AlignItems::Center,
        ..default()
    }, BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.85))))
    .with_children(|root| {
        root.spawn((bevy::ui_widgets::ScrollArea, Node {
            width: Val::Px(720.0), max_height: Val::Percent(90.0),
            overflow: Overflow::scroll_y(),
            flex_direction: FlexDirection::Column, row_gap: Val::Px(10.0),
            padding: UiRect::all(Val::Px(20.0)), border: UiRect::all(Val::Px(1.0)),
            ..default()
        }, BackgroundColor(PANEL), BorderColor::all(BORDER)))
        .with_children(|panel| {
            panel.spawn((Text::new("MO2 instance and profile"), TextFont { font_size: FontSize::Px(20.0), ..default() }, TextColor(TEXT_COLOR)));
            panel.spawn((Text::new("Drop your MO2 instance folder here (the folder containing ModOrganizer.ini)."), TextFont { font_size: FontSize::Px(14.0), ..default() }, TextColor(TEXT_COLOR)));
            let path = source.selection.as_ref().map(|s| display_path(&s.instance_path)).unwrap_or_else(|| "No valid instance selected".into());
            panel.spawn((Text::new(path), TextFont { font_size: FontSize::Px(14.0), ..default() }, TextColor(TEXT_COLOR)));
            if !source.error.is_empty() {
                panel.spawn((Text::new(&source.error), TextFont { font_size: FontSize::Px(14.0), ..default() }, TextColor(Color::srgb(1.0, 0.5, 0.4))));
            }
            if let Some(selection) = &source.selection {
                spawn_dialog_button(panel, format!("Profile: {}", selection.profile), SourceButton::Dropdown);
                if source.dropdown {
                    for name in &source.profiles {
                        spawn_dialog_button(panel, name.clone(), SourceButton::Profile(name.clone()));
                    }
                }
            }
            panel.spawn((Text::new("MO2 overlays the Skyrim Data folder selected in the launcher. Native SKSE plugins are not supported."), TextFont { font_size: FontSize::Px(13.0), ..default() }, TextColor(TEXT_COLOR)));
            spawn_dialog_button(panel, "Close".into(), SourceButton::Close);
        });
    });
}

/// Spawns a labeled button with a source-selection action inside the settings dialog.
fn spawn_dialog_button(
    parent: &mut bevy::ecs::relationship::RelatedSpawnerCommands<'_, ChildOf>,
    label: String,
    action: SourceButton,
) {
    parent
        .spawn((
            Button,
            action,
            Node {
                padding: UiRect::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(BUTTON_OFF),
        ))
        .with_children(|button| {
            button.spawn((
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(14.0),
                    ..default()
                },
                TextColor(TEXT_COLOR),
            ));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies source controls open settings, select profiles, and prevent source changes during conversion.
    #[test]
    fn source_buttons_open_settings_select_profiles_and_lock_during_conversion() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<SourceSettings>()
            .init_resource::<CurrentConversion>()
            .add_systems(Update, (click_source, draw_source).chain());
        let button = app
            .world_mut()
            .spawn((
                SourceButton::Mo2,
                Interaction::Pressed,
                BackgroundColor(BUTTON_OFF),
            ))
            .id();
        app.update();
        assert!(app.world().resource::<SourceSettings>().use_mo2);
        assert!(app.world().resource::<SourceSettings>().open);
        assert_eq!(
            app.world_mut()
                .query_filtered::<Entity, With<SettingsDialog>>()
                .iter(app.world())
                .count(),
            1
        );
        {
            let mut source = app.world_mut().resource_mut::<SourceSettings>();
            source.selection = Some(mo2::Selection {
                instance_path: "instance".into(),
                profile: "Alpha".into(),
            });
            source.profiles = vec!["Alpha".into(), "Zed".into()];
        }
        app.world_mut()
            .entity_mut(button)
            .insert((SourceButton::Profile("Zed".into()), Interaction::Pressed));
        app.update();
        assert_eq!(
            app.world()
                .resource::<SourceSettings>()
                .selection
                .as_ref()
                .unwrap()
                .profile,
            "Zed"
        );
        app.world_mut().resource_mut::<CurrentConversion>().0 = state::ConversionState::Running;
        app.world_mut()
            .entity_mut(button)
            .insert((SourceButton::Skyrim, Interaction::Pressed));
        app.update();
        assert!(app.world().resource::<SourceSettings>().use_mo2);
        app.world_mut().resource_mut::<CurrentConversion>().0 = state::ConversionState::Idle;
        app.world_mut()
            .entity_mut(button)
            .insert(Interaction::Pressed);
        app.update();
        assert!(!app.world().resource::<SourceSettings>().use_mo2);
    }

    /// Verifies profile defaults, config application, source switching, and invalid-instance handling.
    #[test]
    fn validates_defaults_and_switches_conversion_source() {
        let root = std::env::temp_dir().join(format!("mudcrab-source-{}", std::process::id()));
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::create_dir_all(root.join("overwrite")).unwrap();
        for name in ["Zed", "Alpha"] {
            std::fs::create_dir_all(root.join("profiles").join(name)).unwrap();
        }
        std::fs::write(
            root.join("ModOrganizer.ini"),
            "[General]\ngameName=Skyrim\n",
        )
        .unwrap();
        let mut settings = SourceSettings::default();
        let mut config = converter::PipelineConfig::new("Data", "out");
        settings.use_mo2 = true;
        assert!(settings.apply(&mut config).is_err());
        settings.set_instance(&root);
        assert_eq!(settings.selection.as_ref().unwrap().profile, "Alpha");
        settings.selection.as_mut().unwrap().profile = "Zed".into();
        settings.apply(&mut config).unwrap();
        assert_eq!(config.mo2.as_ref().unwrap().profile, "Zed");
        settings.use_mo2 = false;
        settings.apply(&mut config).unwrap();
        assert!(config.mo2.is_none());
        settings.set_instance(&root.join("missing"));
        assert!(settings.selection.is_none() && !settings.error.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
