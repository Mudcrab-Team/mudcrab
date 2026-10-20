//! Native GPU/readback check with no Skyrim assets or browser handoff.
//!
//! cargo run -p engine --profile quick --example bug_report_smoke -- /tmp/mudcrab-reports
//! Produces one local report, verifies its image exists and pause survived, then exits.

use bevy::{
    log::{Level, LogPlugin},
    prelude::*,
    window::{PresentMode, WindowResolution},
};
use engine::{
    bug_report::{
        BugReportPlugin, BugReportService, ReportResult, logging::report_log_layer, request_report,
    },
    config::EngineConfig,
    console::{ConsolePlugin, ConsoleState},
    streaming::{RenderOrigin, StreamingMetrics},
    world::components::StreamingCamera,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Resource)]
struct Smoke {
    started: Instant,
    requested: bool,
}

fn main() {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("mudcrab-bug-report-smoke"));
    let mut app = App::new();
    app.insert_resource(EngineConfig {
        bug_report_dir: Some(output),
        renderer_fixture: true,
        ..default()
    })
    .insert_resource(RenderOrigin(IVec2::new(-2, -3)))
    .insert_resource(StreamingMetrics {
        failed_cells: 1,
        asset_load_failures: 1,
        diagnostic_fallbacks: 1,
        ..default()
    })
    .insert_resource(Smoke {
        started: Instant::now(),
        requested: false,
    })
    .add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Mudcrab bug report smoke".into(),
                    resolution: WindowResolution::new(640, 360),
                    present_mode: PresentMode::AutoVsync,
                    ..default()
                }),
                ..default()
            })
            .set(LogPlugin {
                level: Level::INFO,
                custom_layer: report_log_layer,
                ..default()
            }),
    )
    .add_plugins((ConsolePlugin, BugReportPlugin))
    .add_systems(Startup, setup)
    .add_systems(Update, verify);
    app.run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    server: Res<AssetServer>,
    mut console: ResMut<ConsoleState>,
    mut time: ResMut<Time<Virtual>>,
) {
    commands.spawn((
        Camera3d::default(),
        StreamingCamera,
        Transform::from_xyz(4.0, 3.0, 6.0).looking_at(Vec3::new(0.0, 0.5, 0.0), Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(1.4, 1.4, 1.4))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.1, 0.65, 0.8),
            ..default()
        })),
        Transform::from_xyz(0.0, 0.7, 0.0),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(8.0, 8.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.2, 0.22, 0.25),
            ..default()
        })),
    ));
    commands.spawn((
        PointLight {
            intensity: 1_500_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(4.0, 8.0, 4.0),
    ));
    // A real missing asset plus nonzero failure/fallback counters exercises defect reporting.
    let missing: Handle<WorldAsset> =
        server.load("meshes/bug-report-intentionally-missing.glb#Scene0");
    commands.spawn(WorldAssetRoot(missing));
    warn!(target:"engine::bug_report_smoke", "intentional missing asset; /Users/private-player/secret-machine/meshes/missing.glb");
    console.open = true;
    console
        .scrollback
        .push("THIS CONSOLE TEXT MUST NOT APPEAR IN THE SCREENSHOT".into());
    time.pause();
}

fn verify(world: &mut World) {
    let elapsed = world.resource::<Smoke>().started.elapsed();
    if elapsed > Duration::from_secs(35) {
        eprintln!("Native bug-report smoke timed out");
        world.write_message(AppExit::error());
        return;
    }
    if elapsed > Duration::from_secs(2) && !world.resource::<Smoke>().requested {
        world.resource_mut::<Smoke>().requested = true;
        request_report(world, false).expect("queue local smoke report");
    }
    let service = world.resource::<BugReportService>();
    if service.is_busy() {
        return;
    }
    if let Some(result) = &service.last_result {
        match result {
            ReportResult::Saved {
                directory, archive, ..
            } => {
                let image = directory.join("screenshot.png");
                let okay = image.is_file()
                    && archive.is_file()
                    && world.resource::<Time<Virtual>>().is_paused()
                    && world.resource::<ConsoleState>().open
                    && !world.resource::<ConsoleState>().capture_suppressed;
                println!("native_report_directory={}", directory.display());
                println!("native_report_archive={}", archive.display());
                println!("native_capture_and_pause_ok={okay}");
                world.write_message(if okay {
                    AppExit::Success
                } else {
                    AppExit::error()
                });
            }
            ReportResult::Failed { message, .. } => {
                eprintln!("Native bug-report smoke failed: {message}");
                world.write_message(AppExit::error());
            }
        }
    }
}
