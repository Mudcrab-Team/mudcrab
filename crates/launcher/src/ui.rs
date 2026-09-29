use bevy::prelude::*;

use crate::{
    components::{PlayButton, PlayHintText},
    conversion::panel::{BACKGROUND, BORDER, LABEL_COLOR, PANEL, TITLE_COLOR, conversion_panel},
};

pub fn ui_header() -> impl Scene {
    bsn! {
        #HeaderBanner
        Text::new("OPENSKYRIM")
        TextFont { font_size: FontSize::Px(30.0) }
        TextColor(TITLE_COLOR)
        Node { align_self: AlignSelf::Center }
    }
}

pub fn ui_drag_drop_zone() -> impl Scene {
    bsn! {
        #DragDropZone
        Node {
            width: Val::Percent(100.0),
            height: Val::Px(44.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border: UiRect::all(Val::Px(2.0)),
        }
        BorderColor::all(Color::srgb(0.30, 0.35, 0.45))
        Children [
            (
                Text::new("Drag & Drop Mod Archives (.zip / .7z / .esp) Here")
                TextFont { font_size: FontSize::Px(14.0) }
                TextColor(LABEL_COLOR)
            )
        ]
    }
}

/// The mod manager, still a stub: the load order's title and the drop zone for mod files.
pub fn ui_mod_manager_panel() -> impl Scene {
    bsn! {
        #ModManagerPanel
        Node {
            width: Val::Percent(100.0),
            border: UiRect::all(Val::Px(1.0)),
            padding: UiRect::all(Val::Px(10.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(6.0),
        }
        BackgroundColor(PANEL)
        BorderColor::all(BORDER)
        Children [
            (
                Text::new("Active Mod Load Order (0 Plugins Enabled)")
                TextFont { font_size: FontSize::Px(15.0) }
                TextColor(Color::srgb(0.85, 0.85, 0.85))
            ),
            ui_drag_drop_zone()
        ]
    }
}

pub fn ui_play_button() -> impl Scene {
    bsn! {
        #PlayButton
        PlayButton
        Button
        Node {
            width: Val::Px(240.0),
            height: Val::Px(44.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
        }
        BackgroundColor(Color::srgb(0.16, 0.18, 0.22))
        Children [
            (
                Text::new("PLAY OPENSKYRIM")
                TextFont { font_size: FontSize::Px(18.0) }
                TextColor(Color::WHITE)
            )
        ]
    }
}

/// The bottom row: why Play is or is not ready, and the Play button.
pub fn ui_footer_controls() -> impl Scene {
    bsn! {
        #FooterControls
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(12.0),
        }
        Children [
            (
                #PlayHint
                PlayHintText
                Text::new("")
                TextFont { font_size: FontSize::Px(14.0) }
                TextColor(Color::srgb(0.75, 0.80, 0.90))
                Node { flex_grow: 1.0 }
            ),
            ui_play_button()
        ]
    }
}

/// The launcher's window: the header, the conversion panel, the mod manager and the Play row, top
/// to bottom. The conversion panel takes the height the others leave.
pub fn launcher_scene_list() -> impl SceneList {
    bsn_list![
        Camera2d,
        (
            #RootWindow
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(16.0)),
                row_gap: Val::Px(10.0),
            }
            BackgroundColor(BACKGROUND)
            Children [
                ui_header(),
                conversion_panel(),
                ui_mod_manager_panel(),
                ui_footer_controls()
            ]
        )
    ]
}

pub fn setup_ui(mut commands: Commands) {
    commands.spawn_scene_list(launcher_scene_list());
}
