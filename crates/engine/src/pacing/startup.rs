//! Startup admission behind an explicit loading cover.
//!
//! Ordinary uncovered runs retain configured pacing. A covered load may lift
//! budgets until readiness is proved. Permanent dependency failures refuse the
//! run instead of exposing unproved geometry or holding a black screen forever.

use super::{LoadingCover, READY_FRAMES_REQUIRED, world_ready_inputs_at_frame_end};
use crate::{
    config::EngineConfig,
    streaming::{ActiveSpace, RenderOrigin, StreamingMetrics, StreamingWorld},
    swap_bridge::{WaitingVisibility, pending_drawn},
    world::components::StreamingCamera,
};
use bevy::{prelude::*, render::render_asset::RenderAssetBytesPerFrame};

/// Duration of the public startup cover fade, in seconds.
pub const FADE_SECONDS: f32 = 0.25;

fn set_upload_budget(
    budget: &mut Option<ResMut<RenderAssetBytesPerFrame>>,
    max_bytes: Option<usize>,
) {
    if let Some(budget) = budget {
        budget.max_bytes = max_bytes;
    }
}

fn set_alpha<C: Component>(overlay: &mut Query<&mut BackgroundColor, With<C>>, alpha: f32) {
    for mut colour in overlay.iter_mut() {
        colour.0 = Color::srgba(0.0, 0.0, 0.0, alpha.clamp(0.0, 1.0));
    }
}

/// Maximum time without progress during initial loading, in seconds. Progress
/// means a change in readiness inputs or an in-progress package certification.
/// Expiry restores configured pacing. A blocked cluster run reports an error
/// and exits while covered; it never reveals a frame without readiness proof.
pub const STARTUP_LIFT_TIMEOUT_SECONDS: f32 = 60.0;

/// Blocked admission is reported on the real clock even while loading makes progress.
const STARTUP_BLOCKED_REPORT_INTERVAL_SECONDS: f64 = 60.0;

/// The full-screen black cover the first load happens behind: spawned full black on the first frame
/// and faded out (over [`FADE_SECONDS`], the crossing's own fade) once admission succeeds.
///
/// Its own node, not the crossing's [`FadeOverlay`]: that overlay is the crossing's (and this build
/// does not register the crossing), and two writers on one node would fight over its alpha.
#[derive(Component)]
pub struct StartupCover;

/// The configured pacing values the startup lift puts aside, restored as one when the world is
/// first ready.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ConfiguredPacing {
    spawns_per_frame: usize,
    unloads_per_frame: usize,
    upload_bytes_per_frame: Option<usize>,
}

/// The startup lift's own state, advanced once a frame by [`drive_startup_load`].
/// The first load made no progress in time. Triggered once, when the stall is reported, so
/// observers can say what they are waiting for at no cost on any other frame.
#[derive(Event, Debug, Clone, Copy)]
pub struct StartupLoadStalled;

/// Diagnostic-only notification for each periodic blocked-admission report.
/// Observers can explain their cached blockers without changing loading recovery.
#[derive(Event, Debug, Clone, Copy)]
pub struct StartupLoadBlocked;

#[derive(Resource, Default)]
pub(crate) struct StartupLoad {
    /// `Some` while the lift is in force, holding the configured values to restore.
    configured: Option<ConfiguredPacing>,
    /// Consecutive frames the world-ready predicate has held;
    /// [`crate::pacing::READY_FRAMES_REQUIRED`] of them latch it, the rule the pacing report's own
    /// first-ready latch uses.
    ready_frames: u32,
    /// Seconds since the first frame on Bevy's virtual clock (fade/stall behavior).
    elapsed: f32,
    /// Monotonic first-load measurement; virtual time clamps long loading frames.
    wall_started: Option<std::time::Instant>,
    /// Unclamped real seconds since startup, independent of loading progress.
    real_elapsed: f64,
    /// Real elapsed time of the last periodic blocked-admission report.
    last_blocked_report: f64,
    /// What the world-ready predicate read last frame, to tell a moving load from a stopped one.
    last_inputs: Option<crate::pacing::WorldReadyInputs>,
    /// Seconds since the load last made progress, for [`STARTUP_LIFT_TIMEOUT_SECONDS`].
    stalled: f32,
    /// `Some(elapsed)` once the lift is over and the cover is fading out; `None` before.
    fade: Option<f32>,
    /// The cover node, despawned when the fade ends.
    cover: Option<Entity>,
    /// The cover has faded out and gone: the lift is over for the rest of the run.
    finished: bool,
    failure_reported: bool,
}

impl StartupLoad {
    pub(crate) fn cover_pending(&self) -> bool {
        !self.finished
    }
}

/// Covers the selected cluster package's initial load and lifts streaming pacing only
/// behind that cover. Ordinary and capture-only runs keep their configured budgets.
/// Spawn, root unload and upload budgets are restored together on admission
/// or inactivity timeout. Timeout refuses the run while covered; it never grants readiness.
/// A later loss of readiness never reactivates the startup lift.
pub struct StartupLoadPlugin;

impl Plugin for StartupLoadPlugin {
    fn build(&self, app: &mut App) {
        if !app
            .world()
            .get_resource::<EngineConfig>()
            .is_some_and(|config| config.clusters.is_some())
        {
            return;
        }
        app.init_resource::<LoadingCover>()
            .init_resource::<StartupLoad>()
            .add_systems(Startup, begin_startup_load)
            .add_systems(PostUpdate, drive_startup_load);
    }
}

/// Spawns the cover and lifts spawn, root unload and upload pacing.
fn begin_startup_load(
    mut commands: Commands,
    mut config: ResMut<EngineConfig>,
    mut budget: Option<ResMut<RenderAssetBytesPerFrame>>,
    mut load: ResMut<StartupLoad>,
    mut loading_cover: Option<ResMut<LoadingCover>>,
) {
    if let Some(cover) = loading_cover.as_deref_mut() {
        cover.set_startup_pending(true);
    }
    load.wall_started = Some(std::time::Instant::now());
    let configured = ConfiguredPacing {
        spawns_per_frame: config.max_model_spawns_per_frame,
        unloads_per_frame: config.max_cell_unloads_per_frame,
        upload_bytes_per_frame: config.max_upload_bytes_per_frame(),
    };
    config.max_model_spawns_per_frame = 0;
    config.max_cell_unloads_per_frame = 0;
    set_upload_budget(&mut budget, None);
    load.configured = Some(configured);
    load.cover = Some(
        commands
            .spawn((
                Name::new("Startup cover"),
                StartupCover,
                Node {
                    position_type: PositionType::Absolute,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 1.0)),
                // Under the crossing's own fade (`i32::MAX - 1`): the more specific cover wins.
                GlobalZIndex(i32::MAX - 2),
            ))
            .id(),
    );
    info!(
    spawns_per_frame = configured.spawns_per_frame,
    unloads_per_frame = configured.unloads_per_frame,
    upload_bytes_per_frame = ?configured.upload_bytes_per_frame,
    "startup load: streaming pacing lifted until the world is first ready"
    );
}

/// Restores spawn, root unload and upload pacing together.
fn restore_startup_pacing(
    config: &mut EngineConfig,
    budget: &mut Option<ResMut<RenderAssetBytesPerFrame>>,
    configured: ConfiguredPacing,
) {
    config.max_model_spawns_per_frame = configured.spawns_per_frame;
    config.max_cell_unloads_per_frame = configured.unloads_per_frame;
    set_upload_budget(budget, configured.upload_bytes_per_frame);
}

/// Publish the visible-world boundary using the same run clock as scripted routes.
fn record_visible_ready(load: &StartupLoad, real_time: &Time<Real>) {
    info!(
        seconds = load.elapsed,
        wall_seconds = load.wall_started.map(|start| start.elapsed().as_secs_f64()),
        run_ms = real_time.elapsed_secs_f64() * 1000.0,
        "startup load: the world is ready; the configured streaming pacing is back"
    );
}

/// Holds the cover, watches the world-ready predicate, restores the configured pacing on the frame
/// it latches (or at the timeout), then fades the cover out.
///
/// The predicate is read at frame end, through [`world_ready_at_frame_end`] - the same function
/// [`crate::pacing::track_world_ready`] reads for measurement. The cover's own
/// admission remains independent; measurement additionally waits for its removal.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn drive_startup_load(
    time: Res<Time>,
    real_time: Res<Time<Real>>,
    mut config: ResMut<EngineConfig>,
    origin: Res<RenderOrigin>,
    streaming: Res<StreamingWorld>,
    space: Res<ActiveSpace>,
    metrics: Res<StreamingMetrics>,
    camera: Query<&Transform, With<StreamingCamera>>,
    waiting: WaitingVisibility,
    mut overlay: Query<&mut BackgroundColor, With<StartupCover>>,
    mut budget: Option<ResMut<RenderAssetBytesPerFrame>>,
    mut commands: Commands,
    mut load: ResMut<StartupLoad>,
    (validation, scene, mut gate, mode, hold, mut loading_cover): (
        Option<Res<crate::cluster::package::PackageValidationStatus>>,
        Option<Res<crate::cluster::render::ClusterScene>>,
        Option<ResMut<crate::cluster::loading::ClusterLoadingGate>>,
        Option<Res<crate::physics::MoveMode>>,
        Option<Res<crate::physics::WalkSpawnHold>>,
        Option<ResMut<LoadingCover>>,
    ),
) {
    if load.finished || load.failure_reported {
        return;
    }
    if config.clusters.is_some()
        && (metrics.failed_far_stock_sources > 0 || metrics.failed_cluster_batches > 0)
    {
        if let Some(configured) = load.configured.take() {
            restore_startup_pacing(&mut config, &mut budget, configured);
        }
        load.failure_reported = true;
        let reason = format!(
            "cluster startup refused: {} required stock source(s) and {} required cluster batch(es) failed; repair or re-convert the selected dataset",
            metrics.failed_far_stock_sources, metrics.failed_cluster_batches
        );
        commands.queue(move |world: &mut World| {
            crate::streaming::failure::refuse_loading(world, reason)
        });
        return;
    }
    let delta = time.delta_secs();
    load.elapsed += delta;
    load.real_elapsed += real_time.delta_secs_f64();
    if config.clusters.is_some()
        && load.cover_pending()
        && let (Ok(camera), Some(gate)) = (camera.single(), gate.as_deref_mut())
    {
        gate.observe_loading_pose(camera, origin.0, *space);
    }
    let physics_ready = config.clusters.is_none()
        || crate::cluster::loading::physics_settled(mode.as_deref(), hold.as_deref());
    if !physics_ready
        || !crate::cluster::loading::ready_for_active(&config, &space, gate.as_deref())
    {
        // A dependency withdrawn during the fade cannot expose an unproved view.
        load.fade = None;
        set_alpha(&mut overlay, 1.0);
    }

    // The lift is over; the cover is on its way out.
    if let Some(fade) = load.fade {
        let fade = fade + delta;
        if fade >= FADE_SECONDS {
            if let Some(cover) = load.cover.take() {
                commands.entity(cover).despawn();
            }
            load.finished = true;
            if let Some(cover) = loading_cover.as_deref_mut() {
                cover.set_startup_pending(false);
            }
            info!(
                run_ms = real_time.elapsed_secs_f64() * 1000.0,
                "startup load: cover fade finished"
            );
            if load.ready_frames >= READY_FRAMES_REQUIRED {
                record_visible_ready(&load, &real_time);
            }
        } else {
            set_alpha(&mut overlay, 1.0 - fade / FADE_SECONDS);
            load.fade = Some(fade);
        }
        return;
    }

    // Loading: full black over the burst frames the lift is paying for.
    set_alpha(&mut overlay, 1.0);
    let inputs = camera.single().ok().map(|camera| {
        world_ready_inputs_at_frame_end(
            &config,
            origin.0,
            &streaming,
            &space,
            &metrics,
            camera.translation,
            pending_drawn(&waiting),
            gate.as_deref(),
        )
    });
    // A requested cluster package is part of the first load: the world is not ready while it is
    // certifying or its blocks and batches are still arriving (the rule the demo tour and the shot
    // runner wait on, `initial_cluster_load_ready`).
    let certifying = validation
        .as_deref()
        .is_some_and(|status| *status == crate::cluster::package::PackageValidationStatus::Pending);
    let clusters_ready = match validation.as_deref() {
        None => config.clusters.is_none(),
        Some(crate::cluster::package::PackageValidationStatus::Failed(_)) => false,
        Some(_) => {
            crate::cluster::loading::initial_cluster_load_ready(scene.as_deref(), Some(&metrics))
        }
    };
    let ready = physics_ready
        && clusters_ready
        && !certifying
        && inputs.is_some_and(|inputs| inputs.is_ready());
    load.ready_frames = if ready { load.ready_frames + 1 } else { 0 };
    let latched = load.ready_frames >= READY_FRAMES_REQUIRED;
    let moved = certifying || inputs != load.last_inputs;
    load.last_inputs = inputs;
    load.stalled = if moved { 0.0 } else { load.stalled + delta };
    if !latched
        && load.real_elapsed - load.last_blocked_report >= STARTUP_BLOCKED_REPORT_INTERVAL_SECONDS
    {
        load.last_blocked_report = load.real_elapsed;
        commands.trigger(StartupLoadBlocked);
        warn!(
            wall_seconds = load.real_elapsed,
            stalled_seconds = load.stalled,
            physics_ready,
            clusters_ready,
            certification = ?validation.as_deref(),
            camera_available = inputs.is_some(),
            cover_pending = load.cover_pending(),
            active_space = ?*space,
            render_origin = ?origin.0,
            inputs = ?inputs,
            pending_lod_chunk_parts = ?metrics.pending_lod_chunk_parts,
            cluster_gate = ?gate.as_deref().map(|gate| {
                gate.diagnostics(space.exterior_worldspace(config.worldspace_id))
            }),
            "startup load: admission remains blocked"
        );
    }
    if !latched && load.stalled < STARTUP_LIFT_TIMEOUT_SECONDS {
        return;
    }
    if let Some(configured) = load.configured.take() {
        restore_startup_pacing(&mut config, &mut budget, configured);
    }
    // A timeout restores pacing and refuses admission while the cover stays raised.
    if !latched {
        load.failure_reported = true;
        commands.trigger(StartupLoadStalled);
        let reason = format!(
            "cluster startup refused: admission made no progress before the loading deadline; required inputs: {:?}",
            load.last_inputs
        );
        commands.queue(move |world: &mut World| {
            crate::streaming::failure::refuse_loading(world, reason)
        });
        return;
    }
    load.fade = Some(0.0);
    info!(
        seconds = load.elapsed,
        // `seconds` stays for existing parsers; the virtual clock clamps long loading frames.
        wall_seconds = load.wall_started.map(|start| start.elapsed().as_secs_f64()),
        run_ms = real_time.elapsed_secs_f64() * 1000.0,
        "startup load: dependencies ready; the configured streaming pacing is back"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::database::CellKey;
    use bevy::{
        ecs::schedule::SingleThreadedExecutor,
        log::tracing_subscriber::{Layer, Registry, layer::Context, prelude::*},
        time::TimeUpdateStrategy,
    };
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tracing::{
        Event as TracingEvent, Subscriber,
        field::{Field, Visit},
    };

    const DT: f32 = 0.05;

    /// The configured pacing the lift must put back: all three values differ from the defaults, so
    /// a restore to the wrong ones cannot pass.
    fn configured_pacing() -> EngineConfig {
        EngineConfig {
            max_model_spawns_per_frame: 7,
            max_cell_unloads_per_frame: 3,
            clusters: Some("selected".into()),
            ..default()
        }
    }

    /// A headless app with the startup lift registered, one camera, and the default interior
    /// window (the camera's own cell, so residency alone decides readiness).
    fn startup_app(config: EngineConfig) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, TransformPlugin));
        app.init_resource::<StreamingMetrics>()
            .init_resource::<StreamingWorld>()
            .insert_resource(ActiveSpace::default())
            .insert_resource(RenderAssetBytesPerFrame {
                max_bytes: config.max_upload_bytes_per_frame(),
            })
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                DT,
            )))
            .insert_resource(config);
        if app.world().resource::<EngineConfig>().clusters.is_some() {
            app.insert_resource(crate::cluster::package::PackageValidationStatus::Ready)
                .insert_resource(crate::cluster::render::ClusterScene {
                    ready_revision: default(),
                    residency_revision: default(),
                    certificate: None,
                    inventory: None,
                    terrain_blocks: Vec::new(),
                    meshes: Vec::new(),
                    instances: Vec::new(),
                    origin_offset: Vec3::ZERO,
                    blocks: Vec::new(),
                });
        }
        app.add_plugins(StartupLoadPlugin);
        app.world_mut().spawn((
            StreamingCamera,
            Transform::default(),
            GlobalTransform::default(),
        ));
        app.finish();
        app
    }

    /// Puts the camera inside interior `cell`, whose cell is resident (the world reads ready) or
    /// still loading (it does not).
    fn set_interior(app: &mut App, cell: u32, resident: bool) {
        app.world_mut().resource_mut::<ActiveSpace>().interior = Some(cell);
        if resident {
            let root = app.world_mut().spawn_empty().id();
            app.world_mut()
                .resource_mut::<StreamingWorld>()
                .set_resident_for_test(CellKey::Interior(cell), root);
        }
    }

    fn cover_alpha(app: &mut App) -> Option<f32> {
        let mut query = app
            .world_mut()
            .query_filtered::<&BackgroundColor, With<StartupCover>>();
        query
            .iter(app.world())
            .next()
            .map(|colour| colour.0.to_srgba().alpha)
    }

    fn frames_until_faded() -> u32 {
        (FADE_SECONDS / DT).ceil() as u32 + 1
    }

    #[test]
    fn permanently_failed_owned_sources_refuse_startup_instead_of_covering_forever() {
        for cluster_failure in [false, true] {
            let mut config = configured_pacing();
            config.clusters = Some("selected".into());
            let mut app = startup_app(config);
            set_interior(&mut app, 77, true);
            let mut metrics = app.world_mut().resource_mut::<StreamingMetrics>();
            if cluster_failure {
                metrics.failed_cluster_batches = 1;
            } else {
                metrics.failed_far_stock_sources = 1;
            }
            app.update();
            assert!(
                app.world()
                    .resource::<Messages<AppExit>>()
                    .iter_current_update_messages()
                    .any(AppExit::is_error),
                "a permanent source failure must produce a failing process exit"
            );
            assert_eq!(
                app.world()
                    .resource::<EngineConfig>()
                    .max_model_spawns_per_frame,
                7
            );
            assert_eq!(
                cover_alpha(&mut app),
                Some(1.0),
                "never expose an unproved view before exit"
            );
        }
    }

    #[test]
    fn the_first_frame_lifts_the_pacing_and_raises_the_cover() {
        let mut app = startup_app(configured_pacing());
        set_interior(&mut app, 77, false);
        app.update();
        let config = app.world().resource::<EngineConfig>();
        assert_eq!(config.max_model_spawns_per_frame, 0);
        assert_eq!(config.max_cell_unloads_per_frame, 0);
        assert_eq!(
            app.world().resource::<RenderAssetBytesPerFrame>().max_bytes,
            None,
            "the upload budget is the crossing's own lift"
        );
        assert_eq!(
            cover_alpha(&mut app),
            Some(1.0),
            "the cover is up from the first frame"
        );
        assert!(app.world().resource::<StartupLoad>().configured.is_some());
    }

    #[test]
    fn the_configured_pacing_is_back_on_the_ready_frame() {
        let mut app = startup_app(configured_pacing());
        set_interior(&mut app, 77, true);
        app.update();
        // One ready frame is not the latch: the report's own rule wants two consecutive ones.
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            0
        );
        app.update();
        let config = app.world().resource::<EngineConfig>();
        assert_eq!(config.max_model_spawns_per_frame, 7);
        assert_eq!(config.max_cell_unloads_per_frame, 3);
        assert_eq!(
            app.world().resource::<RenderAssetBytesPerFrame>().max_bytes,
            Some(16 * 1024 * 1024)
        );
        assert_eq!(
            cover_alpha(&mut app),
            Some(1.0),
            "still black on the restore frame"
        );
        // The fade runs out over FADE_SECONDS and takes the cover with it.
        for _ in 0..frames_until_faded() {
            app.update();
        }
        assert_eq!(cover_alpha(&mut app), None, "the cover is gone");
        assert!(app.world().resource::<StartupLoad>().finished);
    }

    #[test]
    fn pacing_measurement_waits_for_mandatory_cluster_cover_release() {
        let mut config = configured_pacing();
        config.clusters = Some("invented-selected-package".into());
        config.cluster_own_statics = true;
        config.benchmark_duration_secs = Some(10.0);
        let mut app = startup_app(config);
        set_interior(&mut app, 77, true);
        let (_directory, cache) = crate::pacing::tests::empty_cell_cache();
        app.insert_resource(cache)
            .insert_resource(crate::cluster::package::PackageValidationStatus::Ready)
            .insert_resource(crate::cluster::render::ClusterScene {
                ready_revision: default(),
                residency_revision: default(),
                certificate: None,
                inventory: None,
                terrain_blocks: Vec::new(),
                meshes: Vec::new(),
                instances: Vec::new(),
                origin_offset: Vec3::ZERO,
                blocks: Vec::new(),
            })
            .init_resource::<crate::pacing::PacingTracker>()
            .init_resource::<crate::profiling::ProfilingState>()
            .add_systems(Update, crate::pacing::track_world_ready);
        let mut release_frame = None;
        for _ in 0..frames_until_faded() + 6 {
            app.update();
            let tracker = app.world().resource::<crate::pacing::PacingTracker>();
            if app.world().resource::<StartupLoad>().cover_pending() {
                assert!(
                    !tracker.world_ready_reached(),
                    "benchmark admission must not latch while the mandatory cluster cover is visible"
                );
                assert!(
                    tracker
                        .report(app.world().resource::<EngineConfig>(), 0)
                        .frames_after_ready
                        .is_none()
                );
            } else {
                release_frame = Some(tracker.frames);
                break;
            }
        }
        let release_frame =
            release_frame.expect("ready dependencies must release the startup cover");
        assert_eq!(cover_alpha(&mut app), None);
        for _ in 0..3 {
            app.update();
        }
        let report = app
            .world()
            .resource::<crate::pacing::PacingTracker>()
            .report(app.world().resource::<EngineConfig>(), 0);
        assert!(report.frames_to_world_ready.unwrap() > release_frame);
        assert!(report.frames_after_ready.unwrap().frames > 0);
    }

    #[test]
    fn world_ready_route_waits_for_startup_cover_to_finish_before_moving() {
        use crate::route::{RouteGround, RoutePlan, RouteRunner, drive_route};

        let mut config = configured_pacing();
        config.benchmark_duration_secs = Some(10.0);
        let mut app = startup_app(config);
        set_interior(&mut app, 77, true);
        let plan = RoutePlan::parse(
            r#"{"name":"fade","worldspace":60,"speed":400,"waypoints":[[0,0],[1000,0]]}"#,
        )
        .unwrap();
        let mut tracker = crate::pacing::PacingTracker::default();
        tracker.observe(0.0, true, false, 16.0);
        tracker.observe(16.0, true, false, 16.0);
        assert!(tracker.world_ready_reached());
        app.insert_resource(tracker)
            .insert_resource(RouteRunner::new(plan, RouteGround::from_cells([])))
            .add_systems(PreUpdate, drive_route);
        app.update();
        for _ in 0..frames_until_faded() + 2 {
            if !app.world().resource::<StartupLoad>().cover_pending() {
                break;
            }
            assert!(
                !app.world().resource::<RouteRunner>().report().started,
                "a ready latch must not start traversal during the startup fade"
            );
            app.update();
        }
        assert!(!app.world().resource::<StartupLoad>().cover_pending());
        app.update();
        app.update();
        let route = app.world().resource::<RouteRunner>().report();
        assert!(route.started);
        assert!(route.distance_travelled_units > 0.0);
    }

    #[test]
    fn an_explicit_immediate_route_keeps_its_startup_stress_behavior() {
        use crate::route::{RouteGround, RoutePlan, RouteRunner, drive_route};

        let mut config = configured_pacing();
        config.benchmark_duration_secs = Some(10.0);
        let mut app = startup_app(config);
        set_interior(&mut app, 77, true);
        let plan = RoutePlan::parse(
            r#"{"name":"immediate","worldspace":60,"speed":400,"start":"immediately","waypoints":[[0,0],[1000,0]]}"#,
        )
        .unwrap();
        app.insert_resource(RouteRunner::new(plan, RouteGround::from_cells([])))
            .add_systems(PreUpdate, drive_route);
        app.update();
        assert!(app.world().resource::<StartupLoad>().cover_pending());
        assert!(app.world().resource::<RouteRunner>().report().started);
    }

    #[test]
    fn a_later_loss_of_ready_never_lifts_again() {
        let mut app = startup_app(configured_pacing());
        set_interior(&mut app, 77, true);
        for _ in 0..frames_until_faded() + 2 {
            app.update();
        }
        assert_eq!(app.world().resource::<StartupLoad>().configured, None);
        // The world falls out of ready and comes back: the latch has happened, so nothing lifts.
        set_interior(&mut app, 78, false);
        app.update();
        set_interior(&mut app, 77, true);
        for _ in 0..3 {
            app.update();
        }
        let config = app.world().resource::<EngineConfig>();
        assert_eq!(config.max_model_spawns_per_frame, 7);
        assert_eq!(config.max_cell_unloads_per_frame, 3);
        assert_eq!(
            app.world().resource::<RenderAssetBytesPerFrame>().max_bytes,
            Some(16 * 1024 * 1024)
        );
        assert_eq!(cover_alpha(&mut app), None);
    }

    #[test]
    fn ordinary_and_capture_startup_keep_their_configured_budgets() {
        for shots in [None, Some("poses.json".into())] {
            let mut app = startup_app(EngineConfig {
                clusters: None,
                shots,
                ..configured_pacing()
            });
            set_interior(&mut app, 77, false);
            app.update();
            assert_eq!(
                app.world()
                    .resource::<EngineConfig>()
                    .max_model_spawns_per_frame,
                7
            );
            assert_eq!(
                app.world()
                    .resource::<EngineConfig>()
                    .max_cell_unloads_per_frame,
                3
            );
            assert!(app.world().get_resource::<StartupLoad>().is_none());
            assert_eq!(cover_alpha(&mut app), None);
        }
    }

    #[test]
    fn a_load_that_keeps_moving_stays_lifted_past_the_timeout_and_stops_when_it_stalls() {
        let mut app = startup_app(configured_pacing());
        const STEP: f32 = 0.25;
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            STEP,
        )));
        set_interior(&mut app, 77, false);
        let frames_to_timeout = (STARTUP_LIFT_TIMEOUT_SECONDS / STEP).ceil() as usize;
        for frame in 0..frames_to_timeout * 2 {
            app.world_mut()
                .resource_mut::<StreamingMetrics>()
                .loading_cells = frame % 3 + 1;
            app.update();
        }
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            0,
            "a load still moving after twice the timeout keeps the lift"
        );
        for _ in 0..=frames_to_timeout {
            app.update();
        }
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            7,
            "a load that stops moving for the timeout gets its pacing back"
        );
    }

    #[test]
    fn selected_clusters_cover_and_lift_startup_without_optional_controls() {
        let config = EngineConfig {
            clusters: Some("selected".into()),
            cluster_own_statics: true,
            ..configured_pacing()
        };
        let mut app = startup_app(config);
        set_interior(&mut app, 77, false);
        app.update();
        assert_eq!(cover_alpha(&mut app), Some(1.0));
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            0
        );
        assert_eq!(
            app.world().resource::<RenderAssetBytesPerFrame>().max_bytes,
            None
        );
    }

    #[test]
    fn a_failed_cluster_load_never_uncovers_an_otherwise_resident_world() {
        let mut config = configured_pacing();
        config.clusters = Some("selected".into());
        config.cluster_own_statics = true;
        let mut app = startup_app(config);
        set_interior(&mut app, 77, true);
        app.insert_resource(crate::cluster::package::PackageValidationStatus::Failed(
            "requested package failed".into(),
        ));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            0.25,
        )));
        for _ in 0..(STARTUP_LIFT_TIMEOUT_SECONDS / 0.25).ceil() as usize + 10 {
            app.update();
        }
        assert_eq!(
            cover_alpha(&mut app),
            Some(1.0),
            "a failed requested cluster package cannot be replaced by a timeout"
        );
    }

    #[test]
    fn a_certifying_cluster_package_holds_an_otherwise_ready_world_lifted() {
        let mut app = startup_app(configured_pacing());
        set_interior(&mut app, 77, true);
        app.insert_resource(crate::cluster::package::PackageValidationStatus::Pending);
        for _ in 0..(READY_FRAMES_REQUIRED as usize + 20) {
            app.update();
        }
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            0,
            "the world is not ready while its requested cluster package certifies"
        );
        app.world_mut()
            .remove_resource::<crate::cluster::package::PackageValidationStatus>();
        for _ in 0..=(READY_FRAMES_REQUIRED as usize) {
            app.update();
        }
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            0,
            "a missing status cannot certify the selected package"
        );
        assert_eq!(cover_alpha(&mut app), Some(1.0));
        app.insert_resource(crate::cluster::package::PackageValidationStatus::Ready);
        for _ in 0..=(READY_FRAMES_REQUIRED as usize) {
            app.update();
        }
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            7
        );
    }

    #[derive(Clone, Default)]
    struct BlockedLogs(Arc<Mutex<Vec<BTreeMap<String, String>>>>);

    #[derive(Default)]
    struct LogFields(BTreeMap<String, String>);

    impl Visit for LogFields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().into(), format!("{value:?}"));
        }
    }

    impl<S: Subscriber> Layer<S> for BlockedLogs {
        fn on_event(&self, event: &TracingEvent<'_>, _: Context<'_, S>) {
            if event.metadata().target() != "engine::pacing::startup" {
                return;
            }
            let mut fields = LogFields::default();
            event.record(&mut fields);
            if fields
                .0
                .get("message")
                .is_some_and(|message| message.contains("admission remains blocked"))
            {
                self.0.lock().unwrap().push(fields.0);
            }
        }
    }

    impl BlockedLogs {
        fn capture(&self, run: impl FnOnce()) {
            tracing::subscriber::with_default(Registry::default().with(self.clone()), run);
        }

        fn snapshot(&self) -> Vec<BTreeMap<String, String>> {
            self.0.lock().unwrap().clone()
        }
    }

    #[derive(Resource, Default)]
    struct StallReports(usize);

    #[derive(Resource, Default)]
    struct BlockedReports(usize);

    fn blocked_startup_app() -> App {
        let mut config = configured_pacing();
        config.clusters = Some("selected".into());
        config.worldspace_id = 60;
        let mut app = startup_app(config);
        app.world_mut()
            .remove_resource::<crate::cluster::package::PackageValidationStatus>();
        app.world_mut()
            .remove_resource::<crate::cluster::render::ClusterScene>();
        // Keep the real PostUpdate system on the scoped subscriber's thread.
        app.edit_schedule(PostUpdate, |schedule| {
            schedule.set_executor(SingleThreadedExecutor::new());
        });
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)))
            .init_resource::<crate::cluster::loading::ClusterLoadingGate>()
            .insert_resource(crate::physics::MoveMode::Walk)
            .init_resource::<StallReports>()
            .init_resource::<BlockedReports>()
            .add_observer(
                |_: On<StartupLoadBlocked>, mut reports: ResMut<BlockedReports>| {
                    reports.0 += 1;
                },
            )
            .add_observer(
                |_: On<StartupLoadStalled>, mut reports: ResMut<StallReports>| {
                    reports.0 += 1;
                },
            );
        app
    }

    #[test]
    fn blocked_report_uses_real_seconds_even_while_loading_inputs_change() {
        let mut app = blocked_startup_app();
        {
            let mut metrics = app.world_mut().resource_mut::<StreamingMetrics>();
            metrics.pending_asset_instances = 7;
            metrics.pending_lod_chunks = 163;
            metrics.pending_lod_chunk_parts = [0, 17, 146];
            metrics.pending_far_stock_queries = 1;
        }
        let logs = BlockedLogs::default();
        logs.capture(|| {
            app.update(); // The first real-time update has zero delta.
            for second in 1..=120 {
                app.world_mut()
                    .resource_mut::<StreamingMetrics>()
                    .queued_far_stock_queries = 2451 + second % 2;
                app.update();
                assert_eq!(
                    logs.snapshot().len(),
                    second / 60,
                    "a blocked admission reports every 60 real seconds despite progress; second={second}"
                );
                assert_eq!(app.world().resource::<BlockedReports>().0, second / 60);
            }
        });
        let reports = logs.snapshot();
        assert_eq!(reports[0]["physics_ready"], "false");
        assert_eq!(reports[0]["clusters_ready"], "false");
        assert_eq!(reports[0]["certification"], "None");
        assert_eq!(reports[0]["pending_lod_chunk_parts"], "[0, 17, 146]");
        assert!(reports[0]["inputs"].contains("pending_asset_instances: 7"));
        assert!(reports[0]["inputs"].contains("pending_lod_chunks: 163"));
        assert!(reports[0]["inputs"].contains("queued_far_stock_queries: 2451"));
        assert!(reports[0]["inputs"].contains("pending_far_stock_queries: 1"));
        assert!(reports[0].contains_key("active_space"));
        assert!(reports[0].contains_key("render_origin"));
        assert!(reports[0].contains_key("wall_seconds"));
        assert_eq!(cover_alpha(&mut app), Some(1.0));
        assert_eq!(app.world().resource::<StallReports>().0, 0);
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            0,
            "periodic diagnostics must not restore pacing"
        );
    }

    #[test]
    fn blocked_report_continues_during_pending_certification() {
        let mut app = blocked_startup_app();
        app.insert_resource(crate::cluster::package::PackageValidationStatus::Pending);
        let logs = BlockedLogs::default();
        logs.capture(|| {
            app.update();
            for _ in 0..120 {
                app.update();
            }
        });
        let reports = logs.snapshot();
        assert_eq!(
            reports.len(),
            2,
            "Pending certification must not reset the blocked-report clock"
        );
        assert_eq!(reports[0]["certification"], "Some(Pending)");
        assert_eq!(reports[1]["certification"], "Some(Pending)");
        assert_eq!(app.world().resource::<BlockedReports>().0, 2);
        assert_eq!(app.world().resource::<StallReports>().0, 0);
        assert_eq!(cover_alpha(&mut app), Some(1.0));
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            0
        );
    }

    #[test]
    fn blocked_report_exposes_cached_gate_scope_and_withdrawal_without_releasing_cover() {
        use crate::cluster::loading::{ClusterLoadingGate, ClusterWorldReadiness};
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let package = Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let mut gate = ClusterLoadingGate::default();
        for _ in 0..6 {
            gate.invalidate();
        }
        assert!(gate.publish(
            &package,
            9,
            7,
            31,
            std::collections::HashMap::from([(
                60,
                ClusterWorldReadiness {
                    applicable_cameras: 2,
                    missing_views: 3,
                    needed_batches: 11,
                    pending_materials: 5,
                    pending_templates: 7,
                    pending_compute: 13,
                    pending_pipelines: 17,
                    failed_batches: 19,
                },
            )]),
        ));
        let mut app = blocked_startup_app();
        app.insert_resource(gate);
        let logs = BlockedLogs::default();
        logs.capture(|| {
            app.update();
            for _ in 0..60 {
                app.update();
            }
            let reports = logs.snapshot();
            assert_eq!(reports.len(), 1, "the current cached gate must be reported");
            let current = &reports[0]["cluster_gate"];
            for expected in [
                "world: 60",
                "package_scope: Some(",
                "layout: 9",
                "generation: 7",
                "snapshot_generation: Some(7)",
                "render_frame: Some(31)",
                "applicable_cameras: 2",
                "missing_views: 3",
                "needed_batches: 11",
                "pending_materials: 5",
                "pending_templates: 7",
                "pending_compute: 13",
                "pending_pipelines: 17",
                "failed_batches: 19",
            ] {
                assert!(current.contains(expected), "missing {expected}: {current}");
            }
            app.world_mut()
                .resource_mut::<ClusterLoadingGate>()
                .invalidate();
            for _ in 0..180 {
                app.update();
            }
        });
        let reports = logs.snapshot();
        assert_eq!(
            reports.len(),
            4,
            "blocked reports continue after the stall timeout"
        );
        let withdrawn = &reports[1]["cluster_gate"];
        assert!(withdrawn.contains("generation: 8"));
        assert!(withdrawn.contains("snapshot_generation: None"));
        assert!(withdrawn.contains("counts: None"));
        assert!(!withdrawn.contains("pending_pipelines: 17"));
        assert_eq!(app.world().resource::<BlockedReports>().0, 4);
        assert_eq!(app.world().resource::<StallReports>().0, 1);
        assert!(app.world().resource::<StartupLoad>().failure_reported);
        assert!(
            app.world()
                .resource::<crate::streaming::failure::LoadingRefusal>()
                .0
                .contains("no progress")
        );
        assert_eq!(cover_alpha(&mut app), Some(1.0));
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            7,
            "the existing 60-virtual-second stall still restores pacing"
        );
    }

    #[test]
    fn blocked_report_stops_after_startup_admission_finishes() {
        let mut app = startup_app(configured_pacing());
        app.edit_schedule(PostUpdate, |schedule| {
            schedule.set_executor(SingleThreadedExecutor::new());
        });
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)))
            .init_resource::<BlockedReports>()
            .add_observer(
                |_: On<StartupLoadBlocked>, mut reports: ResMut<BlockedReports>| {
                    reports.0 += 1;
                },
            );
        set_interior(&mut app, 77, false);
        let logs = BlockedLogs::default();
        logs.capture(|| {
            app.update();
            for _ in 0..60 {
                app.update();
            }
            assert_eq!(logs.snapshot().len(), 1);
            set_interior(&mut app, 77, true);
            for _ in 0..120 {
                app.update();
            }
        });
        assert!(app.world().resource::<StartupLoad>().finished);
        assert_eq!(app.world().resource::<BlockedReports>().0, 1);
        assert_eq!(
            logs.snapshot().len(),
            1,
            "a finished startup must stop reporting"
        );
    }

    #[test]
    fn a_world_that_never_becomes_ready_gets_its_pacing_back_at_the_timeout() {
        let mut app = startup_app(configured_pacing());
        // Virtual time clamps each frame's delta to its 0.25 s maximum, so the timeout is reached
        // in frames of that length; Bevy's first update advances time by zero.
        const STEP: f32 = 0.25;
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            STEP,
        )));
        set_interior(&mut app, 77, false);
        #[derive(Resource, Default)]
        struct Stalls(u32);
        app.init_resource::<Stalls>()
            .add_observer(|_: On<StartupLoadStalled>, mut stalls: ResMut<Stalls>| stalls.0 += 1);
        let frames_to_timeout = (STARTUP_LIFT_TIMEOUT_SECONDS / STEP).ceil() as usize;
        for _ in 0..frames_to_timeout {
            app.update();
        }
        assert_eq!(
            app.world()
                .resource::<EngineConfig>()
                .max_model_spawns_per_frame,
            0,
            "still lifted before the timeout"
        );
        assert_eq!(app.world().resource::<Stalls>().0, 0);
        app.update();
        assert_eq!(
            app.world().resource::<Stalls>().0,
            1,
            "the stall is announced once, so detailed reports run only then"
        );
        let config = app.world().resource::<EngineConfig>();
        assert_eq!(config.max_model_spawns_per_frame, 7);
        assert_eq!(config.max_cell_unloads_per_frame, 3);
        assert_eq!(
            app.world().resource::<RenderAssetBytesPerFrame>().max_bytes,
            Some(16 * 1024 * 1024),
            "the timeout path restores every budget too"
        );
        assert!(
            app.world()
                .resource::<crate::streaming::failure::LoadingRefusal>()
                .0
                .contains("no progress")
        );
        assert!(
            app.world()
                .resource::<Messages<AppExit>>()
                .iter_current_update_messages()
                .any(AppExit::is_error)
        );
        app.update();
        assert_eq!(
            cover_alpha(&mut app),
            Some(1.0),
            "inactivity refuses admission without uncovering the world"
        );
        assert_eq!(app.world().resource::<Stalls>().0, 1, "and never again");
    }
}
