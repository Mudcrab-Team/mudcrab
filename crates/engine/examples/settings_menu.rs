//! Asset-free native preview of the actual settings menu and rendering systems.
//! Set MUDCRAB_UI_PREVIEW_DIR to capture all three tabs and exit after capture completes.

use bevy::{
    camera::visibility::RenderLayers,
    input::{
        ButtonState,
        keyboard::{Key, KeyboardInput},
    },
    prelude::*,
    render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk},
    window::PrimaryWindow,
};
use engine::{
    color_pipeline::{SceneColorPipeline, SkyrimCinematicPostPlugin},
    config::EngineConfig,
    console::ConsolePlugin,
    lights::LightsPlugin,
    profiling::ProfilingState,
    render::{
        VercidiumRendererPlugin, WATER_LAYER, WaterExtension, WaterMaterial, WaterReflectionTexture,
    },
    settings::{RenderingSettings, SettingsPage},
    settings_ui::{SettingsMenuState, SettingsPlugin},
    sky::{FogCamera, SkyCamera, SkyPlugin},
    world::components::{StreamingCamera, WaterSurface},
};

#[derive(Resource)]
struct PreviewCapture {
    directory: Option<std::path::PathBuf>,
    stage: usize,
    last_capture: f32,
    in_flight: bool,
}

fn main() {
    let directory: Option<std::path::PathBuf> =
        std::env::var_os("MUDCRAB_UI_PREVIEW_DIR").map(Into::into);
    if let Some(path) = &directory {
        std::fs::create_dir_all(path).expect("create preview capture directory");
    }
    App::new()
        .add_plugins(DefaultPlugins)
        .insert_resource(EngineConfig::default())
        .insert_resource(RenderingSettings::default())
        .init_resource::<ProfilingState>()
        .init_resource::<engine::color_pipeline::SceneGradeOverrides>()
        .insert_resource(PreviewCapture {
            directory,
            stage: 0,
            last_capture: 0.0,
            in_flight: false,
        })
        .add_plugins((
            VercidiumRendererPlugin,
            SkyPlugin,
            ConsolePlugin,
            LightsPlugin,
            SkyrimCinematicPostPlugin,
            SettingsPlugin,
        ))
        .add_systems(PostStartup, setup)
        .add_systems(Update, capture_tabs)
        .run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut water_materials: ResMut<Assets<WaterMaterial>>,
    reflection: Res<WaterReflectionTexture>,
    windows: Query<Entity, With<PrimaryWindow>>,
    mut keys: MessageWriter<KeyboardInput>,
) {
    commands.spawn((
        Camera3d::default(),
        SceneColorPipeline::default(),
        StreamingCamera,
        SkyCamera,
        FogCamera,
        Msaa::Off,
        IsDefaultUiCamera,
        Transform::from_xyz(0.0, 320.0, 700.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight::default(),
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.8, -0.5, 0.0)),
    ));
    commands.insert_resource(GlobalAmbientLight::default());
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(2000.0, 2000.0))),
        MeshMaterial3d(materials.add(Color::srgb(0.18, 0.25, 0.16))),
        Transform::from_xyz(0.0, -20.0, 0.0),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(180.0, 240.0, 180.0))),
        MeshMaterial3d(materials.add(Color::srgb(0.5, 0.22, 0.12))),
        Transform::from_xyz(-180.0, 100.0, 0.0),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(600.0, 600.0))),
        MeshMaterial3d(water_materials.add(WaterMaterial {
            base: StandardMaterial {
                base_color: Color::srgba(0.02, 0.06, 0.09, 0.7),
                alpha_mode: AlphaMode::Blend,
                ..default()
            },
            extension: WaterExtension::with_reflection(reflection.0.clone(), None),
        })),
        Transform::from_xyz(240.0, 0.0, 0.0),
        WaterSurface,
        RenderLayers::layer(WATER_LAYER),
    ));
    // Open through the same keyboard path as interactive play, including pause and capture.
    keys.write(KeyboardInput {
        key_code: KeyCode::Escape,
        logical_key: Key::Escape,
        state: ButtonState::Pressed,
        text: None,
        repeat: false,
        window: windows.single().unwrap(),
    });
}

fn capture_tabs(
    time: Res<Time<Real>>,
    mut capture: ResMut<PreviewCapture>,
    mut commands: Commands,
) {
    let Some(directory) = capture.directory.as_ref() else {
        return;
    };
    if capture.stage >= 3 || capture.in_flight || time.elapsed_secs() - capture.last_capture < 2.0 {
        return;
    }
    let name = ["graphics.png", "streaming.png", "controls.png"][capture.stage];
    let path = directory.join(name);
    capture.in_flight = true;
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path))
        .observe(
            |_: On<ScreenshotCaptured>,
             time: Res<Time<Real>>,
             mut capture: ResMut<PreviewCapture>,
             mut menu: ResMut<SettingsMenuState>,
             mut exit: MessageWriter<AppExit>| {
                capture.stage += 1;
                capture.last_capture = time.elapsed_secs();
                capture.in_flight = false;
                match capture.stage {
                    1 => menu.page = SettingsPage::Streaming,
                    2 => menu.page = SettingsPage::Controls,
                    _ => {
                        exit.write(AppExit::Success);
                    }
                }
            },
        );
}
