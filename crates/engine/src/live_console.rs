//! Opt-in process-local control for a visible developer tuning session.

use crate::{
    color_pipeline::{SceneGradeMode, SceneVisualPreset},
    console::{AppConsoleExt, ConsoleCommand, ConsoleState, execute_line},
    streaming::StreamingMetrics,
    terrain_upload::TerrainMeshUploadReadiness,
};
use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, ScreenshotCaptured},
};
use std::{
    io::{self, BufRead},
    path::PathBuf,
    sync::{Mutex, mpsc},
};

#[derive(Resource)]
struct IncomingCommands(Mutex<mpsc::Receiver<String>>);

pub(crate) struct StdinConsolePlugin;

impl Plugin for StdinConsolePlugin {
    fn build(&self, app: &mut App) {
        let (sender, receiver) = mpsc::sync_channel(32);
        std::thread::Builder::new()
            .name("mudcrab-stdin-console".into())
            .spawn(move || {
                for line in io::stdin().lock().lines() {
                    let Ok(line) = line else { break };
                    if line.chars().count() <= crate::console::INPUT_CAP
                        && sender.send(line).is_err()
                    {
                        break;
                    }
                }
            })
            .expect("start developer console reader");
        app.insert_resource(IncomingCommands(Mutex::new(receiver)))
            .init_resource::<ConsoleState>()
            .add_console_command(ConsoleCommand {
                name: "SceneState", short: None, usage: "SceneState",
                help: "report live grading and streaming readiness",
                handler: Box::new(|world, args| {
                    if !args.is_empty() { return Err("usage: SceneState".into()) }
                    Ok(scene_state(world).to_string())
                }),
            })
            .add_console_command(ConsoleCommand {
                name: "SceneShot", short: None, usage: "SceneShot <png-path>",
                help: "capture the visible world after streaming and uploads settle",
                handler: Box::new(|world, args| {
                    let [path] = args else { return Err("usage: SceneShot <png-path>".into()) };
                    let path = PathBuf::from(path);
                    if path.extension().is_none_or(|ext| ext != "png") {
                        return Err("SceneShot requires a .png path".into());
                    }
                    let state = scene_state(world);
                    if state["ready"] != true { return Err(format!("scene is still loading: {state}")) }
                    if path.exists() || path.with_extension("json").exists() {
                        return Err("SceneShot refuses to replace existing evidence".into());
                    }
                    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
                    let destination = path.clone();
                    let ticket = crate::capture_frame::CaptureTicket::default();
                    world.spawn((Screenshot::primary_window(), ticket.clone())).observe(move |event: On<ScreenshotCaptured>| {
                        let result = (|| -> Result<(), String> {
                            let frame = ticket.receipt().ok_or("missing live capture-frame receipt")?;
                            let image = event.image.clone().try_into_dynamic().map_err(|e| e.to_string())?;
                            let mut receipt = state.clone();
                            receipt["dimensions"] = serde_json::json!([image.width(), image.height()]);
                            receipt["capture_frame"] = serde_json::to_value(frame).map_err(|e| e.to_string())?;
                            receipt["classification"] = "live screenshot; grading state recorded at request, camera and readiness bound to captured render frame".into();
                            image.save(&destination).map_err(|e| e.to_string())?;
                            std::fs::write(destination.with_extension("json"), serde_json::to_string_pretty(&receipt).unwrap()).map_err(|e| e.to_string())?;
                            Ok(())
                        })();
                        match result {
                            Ok(()) => info!(target: "live_console", "saved live screenshot {}", destination.display()),
                            Err(error) => error!(target: "live_console", "live screenshot failed: {error}"),
                        }
                    });
                    Ok(format!("live screenshot requested: {}", path.display()))
                }),
            })
            .add_systems(Update, dispatch_stdin);
    }
}

fn dispatch_stdin(world: &mut World) {
    let lines = {
        let receiver = world.resource::<IncomingCommands>().0.lock().unwrap();
        receiver.try_iter().take(16).collect::<Vec<_>>()
    };
    for line in lines {
        let output = execute_line(world, &line);
        for result in output.lines() {
            info!(target: "live_console", "{result}");
        }
    }
}

fn scene_state(world: &World) -> serde_json::Value {
    let mode = *world.resource::<SceneGradeMode>();
    let preset = *world.resource::<SceneVisualPreset>();
    let streaming = world.get_resource::<StreamingMetrics>();
    let pending_terrain = world
        .get_resource::<TerrainMeshUploadReadiness>()
        .map_or(0, |uploads| uploads.outstanding());
    let renderer_ready = world
        .get_resource::<crate::render::RendererMetrics>()
        .is_some_and(|r| r.final_path_active());
    let pending_specializations = world
        .get_resource::<crate::capture_frame::RetryReadiness>()
        .and_then(|r| *r.0.lock().unwrap());
    let pending_transfers = world
        .get_resource::<crate::capture_frame::TransferReadiness>()
        .and_then(|r| *r.0.lock().unwrap());
    let profiler = world.get_resource::<crate::profiling::ProfilingState>();
    let pending_batches = [
        "lod/pending_terrain_batch_chunks",
        "lod/pending_initial_terrain_upload_chunks",
        "lod/pending_terrain_selection_uploads",
    ]
    .iter()
    .map(|key| profiler.and_then(|p| p.gauge(key)).unwrap_or(0.0))
    .sum::<f64>();
    let ready = renderer_ready
        && streaming.is_some_and(|s| {
            s.pending_asset_instances == 0
                && s.pending_surface_instances == 0
                && s.loading_cells == 0
                && s.pending_lod_queries == 0
                && s.pending_lod_chunks == 0
                && s.failed_cells == 0
                && s.asset_load_failures == 0
                && s.failed_lod_queries == 0
                && s.failed_lod_chunks == 0
        })
        && pending_terrain == 0
        && pending_specializations == Some(0)
        && pending_transfers == Some(0)
        && pending_batches == 0.0;
    let grade = mode.grade(preset);
    serde_json::json!({
        "ready": ready, "grade": mode.name(), "preset": preset.name(),
        "saturation": grade.saturation, "brightness": grade.brightness, "contrast": grade.contrast,
        "tint_amount": grade.tint_amount, "tint_rgb": grade.tint_rgb,
        "pending_terrain_uploads": pending_terrain,
        "pending_specializations": pending_specializations,
        "pending_render_transfers": pending_transfers,
        "pending_batches": pending_batches,
        "renderer_ready": renderer_ready,
        "loading_cells": streaming.map(|s| s.loading_cells),
        "pending_assets": streaming.map(|s| s.pending_asset_instances),
        "pending_surfaces": streaming.map(|s| s.pending_surface_instances),
        "pending_lod_queries": streaming.map(|s| s.pending_lod_queries),
        "pending_lod_chunks": streaming.map(|s| s.pending_lod_chunks),
        "failed_cells": streaming.map(|s| s.failed_cells),
        "asset_load_failures": streaming.map(|s| s.asset_load_failures),
        "failed_lod_queries": streaming.map(|s| s.failed_lod_queries),
        "failed_lod_chunks": streaming.map(|s| s.failed_lod_chunks),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_commands_use_the_registered_console_and_bound_work_per_update() {
        let mut app = App::new();
        let (sender, receiver) = mpsc::sync_channel(32);
        app.insert_resource(IncomingCommands(Mutex::new(receiver)))
            .init_resource::<ConsoleState>()
            .insert_resource(SceneGradeMode::Full)
            .add_console_command(ConsoleCommand {
                name: "TestMode",
                short: None,
                usage: "TestMode",
                help: "test",
                handler: Box::new(|world, _| {
                    *world.resource_mut::<SceneGradeMode>() = SceneGradeMode::SnapshotLinear;
                    Ok("selected snapshot-linear".into())
                }),
            });
        for _ in 0..17 {
            sender.send("TestMode".into()).unwrap();
        }
        dispatch_stdin(app.world_mut());
        assert_eq!(
            *app.world().resource::<SceneGradeMode>(),
            SceneGradeMode::SnapshotLinear
        );
        assert_eq!(app.world().resource::<ConsoleState>().scrollback.len(), 32);
        dispatch_stdin(app.world_mut());
        assert_eq!(app.world().resource::<ConsoleState>().scrollback.len(), 34);
    }
}
