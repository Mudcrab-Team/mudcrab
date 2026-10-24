//! Local, bounded diagnostics with an optional handoff to GitHub's reviewed issue form.
//!
//! Capture never waits for asset readiness. Blocking encoding, storage and browser work stays
//! off the game thread, and a timed-out worker retains its slot until it actually exits.

pub mod export;
pub mod logging;
pub mod privacy;
pub mod snapshot;

use bevy::{
    diagnostic::FrameCount,
    prelude::*,
    render::{
        RenderApp,
        view::screenshot::{Capturing, Screenshot, ScreenshotCaptured},
    },
    window::{PrimaryWindow, WindowOccluded},
};
use serde_json::{Value, json};
use std::{
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    config::EngineConfig,
    console::{AppConsoleExt, ConsoleCommand, ConsoleState, ConsoleSystems, push_scrollback},
};
use export::{BundleError, BundleInput, BundleOutcome};
use privacy::Sanitizer;

const READBACK_TIMEOUT: Duration = Duration::from_secs(10);
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(30);
const BROWSER_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CAPTURE_PIXELS: u64 = 16_000_000;
const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;

pub struct BugReportPlugin;

impl Plugin for BugReportPlugin {
    fn build(&self, app: &mut App) {
        let renderer_available = app.get_sub_app(RenderApp).is_some();
        app.init_resource::<BugReportService>()
            .init_resource::<logging::RecentLogs>()
            .init_resource::<CaptureSlot>()
            .init_resource::<WindowOcclusion>()
            .add_message::<WindowOccluded>()
            .init_resource::<snapshot::FrameHistory>()
            .insert_resource(ReportRendererAvailable(renderer_available))
            .add_console_command(ConsoleCommand {
                name: "BugReport",
                short: None,
                usage: "BugReport [local]",
                help: "save screenshot and diagnostics, then open GitHub for review; local skips the browser",
                handler: Box::new(|world, args| {
                    let open_browser = match args {
                        [] => true,
                        [arg] if arg.eq_ignore_ascii_case("local") => false,
                        _ => return Err("usage: BugReport [local]".into()),
                    };
                    request_report(world, open_browser)?;
                    Ok("Collecting bug report…".into())
                }),
            })
            .add_systems(PreUpdate, track_window_occlusion)
            .add_systems(Update, drive_reports.before(ConsoleSystems::Ui))
            .add_systems(Update, snapshot::sample_frame.run_if(interactive_sampling))
            .add_systems(Last, capture_frame_metadata);
    }
}

#[derive(Resource)]
struct ReportRendererAvailable(bool);

/// The renderer owns an in-flight readback until its callback. Keep its entity alive after a
/// report timeout so Bevy's late callback can safely mark it captured, and permit no second readback.
#[derive(Resource, Default)]
struct CaptureSlot(Option<Entity>);

#[derive(Resource, Default)]
struct WindowOcclusion(Option<(Entity, bool)>);

fn track_window_occlusion(
    mut events: MessageReader<WindowOccluded>,
    primary: Query<Entity, With<PrimaryWindow>>,
    mut state: ResMut<WindowOcclusion>,
) {
    let window = primary.single().ok();
    for event in events.read() {
        if Some(event.window) == window {
            state.0 = Some((event.window, event.occluded));
        }
    }
}

/// Last local result. Browser launch is reported separately and never changes a saved bundle.
#[derive(Debug, Clone)]
pub enum ReportResult {
    Saved {
        directory: PathBuf,
        archive: PathBuf,
        partial: bool,
        missing: Vec<String>,
    },
    Failed {
        message: String,
        recovered_directory: Option<PathBuf>,
    },
}

#[derive(Resource, Default)]
pub struct BugReportService {
    queued: Option<bool>,
    active: Option<ActiveReport>,
    pub last_result: Option<ReportResult>,
}

impl BugReportService {
    pub fn is_busy(&self) -> bool {
        self.queued.is_some() || self.active.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Capturing,
    Packaging,
    Browser,
    Retiring,
}

enum WorkerResult {
    Bundle(Result<BundleOutcome, BundleError>),
    Browser(Result<(), String>),
}

struct ActiveReport {
    id: String,
    created_unix_ms: u128,
    started: Instant,
    deadline: Instant,
    phase_started: Instant,
    phase: Phase,
    cancelled: Arc<AtomicBool>,
    open_browser: bool,
    snapshot: Value,
    logs: String,
    image: Option<Result<Image, String>>,
    screenshot_entity: Option<Entity>,
    capture_sample_pending: bool,
    worker: Option<JoinHandle<WorkerResult>>,
    issue_url: Option<String>,
    saved_directory: Option<PathBuf>,
}

/// Queue collection without filesystem work. Automated and headless sessions always stay local.
pub fn request_report(world: &mut World, open_browser: bool) -> Result<(), String> {
    let automated = world
        .get_resource::<EngineConfig>()
        .is_some_and(automated_run);
    let Some(mut service) = world.get_resource_mut::<BugReportService>() else {
        return Err("Bug reporting is unavailable in this application".into());
    };
    if service
        .active
        .as_ref()
        .is_some_and(|active| active.phase == Phase::Retiring)
    {
        return Err("The previous bug report is still finishing".into());
    }
    if service.is_busy() {
        return Err("A bug report is already being collected".into());
    }
    service.last_result = None;
    service.queued = Some(open_browser && !automated);
    Ok(())
}

fn automated_run(config: &EngineConfig) -> bool {
    config.headless
        || config.is_benchmark_run()
        || config.benchmark_only
        || config.shots.is_some()
        || config.material_fixture
        || config.terrain_water_fixture
        || config.renderer_fixture
        || config.transform_bounds_fixture
        || config.streaming_fixture
        || config.physics_fixture
}

fn interactive_sampling(config: Option<Res<EngineConfig>>) -> bool {
    config.is_none_or(|config| !automated_run(&config))
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn frame(world: &World) -> Option<u32> {
    world.get_resource::<FrameCount>().map(|f| f.0)
}

fn console_output(world: &mut World, text: &str) {
    if let Some(mut console) = world.get_resource_mut::<ConsoleState>() {
        push_scrollback(&mut console.scrollback, text);
    }
}

fn suppress_console(world: &mut World, suppressed: bool) {
    if let Some(mut console) = world.get_resource_mut::<ConsoleState>() {
        console.capture_suppressed = suppressed;
    }
}

fn capture_unavailable(world: &mut World) -> Option<String> {
    if world
        .get_resource::<CaptureSlot>()
        .is_some_and(|slot| slot.0.is_some())
    {
        return Some("previous screenshot is still awaiting GPU readback".into());
    }
    if !world
        .get_resource::<ReportRendererAvailable>()
        .is_some_and(|renderer| renderer.0)
    {
        return Some("renderer is unavailable".into());
    }
    let mut query = world.query_filtered::<(Entity, &Window), With<PrimaryWindow>>();
    let Ok((entity, window)) = query.single(world) else {
        return Some("primary window is unavailable".into());
    };
    if !window.visible {
        return Some("primary window is hidden".into());
    }
    if world.resource::<WindowOcclusion>().0 == Some((entity, true)) {
        return Some("primary window is occluded or minimized".into());
    }
    let width = u64::from(window.physical_width());
    let height = u64::from(window.physical_height());
    if width == 0 || height == 0 {
        return Some("window has no renderable pixels".into());
    }
    if width * height > MAX_CAPTURE_PIXELS {
        return Some("window exceeds the 16-million-pixel readback limit".into());
    }
    let mut cameras = world.query_filtered::<&Camera, With<Camera3d>>();
    if !cameras.iter(world).any(|camera| camera.is_active) {
        return Some("active game camera is unavailable".into());
    }
    None
}

fn begin_report(world: &mut World, open_browser: bool) -> ActiveReport {
    let started = Instant::now();
    let stamp = time::OffsetDateTime::now_utc()
        .format(
            &time::format_description::parse_borrowed::<2>(
                "[year][month][day]T[hour][minute][second]Z",
            )
            .expect("fixed timestamp format"),
        )
        .unwrap_or_else(|_| epoch_ms().to_string());
    let id = format!("{stamp}-{}", uuid::Uuid::new_v4().simple());
    let mut snapshot = snapshot::capture(world);
    snapshot["report_id"] = json!(id);
    let logs = world.resource::<logging::RecentLogs>().snapshot();
    let mut active = ActiveReport {
        id: id.clone(),
        created_unix_ms: epoch_ms(),
        started,
        deadline: started + COLLECTION_TIMEOUT,
        phase_started: started,
        phase: Phase::Capturing,
        cancelled: Arc::new(AtomicBool::new(false)),
        open_browser,
        snapshot,
        logs,
        image: None,
        screenshot_entity: None,
        capture_sample_pending: false,
        worker: None,
        issue_url: None,
        saved_directory: None,
    };
    if let Some(reason) = capture_unavailable(world) {
        active.image = Some(Err(reason));
    } else {
        suppress_console(world, true);
        active.capture_sample_pending = true;
        active.snapshot["capture_request"] =
            json!({"unix_ms":epoch_ms(), "main_world_frame":frame(world), "rendered_frame":null});
        let entity = world.spawn(Screenshot::primary_window()).observe(
            move |event: On<ScreenshotCaptured>, mut service: ResMut<BugReportService>, mut slot: ResMut<CaptureSlot>, mut console: Option<ResMut<ConsoleState>>, occlusion: Res<WindowOcclusion>, windows: Query<(Entity, &Window), With<PrimaryWindow>>| {
                if slot.0 == Some(event.entity) { slot.0 = None; }
                let Some(active) = service.active.as_mut() else { return; };
                if active.id != id || active.phase != Phase::Capturing || active.image.is_some() { return; }
                if active.phase_started.elapsed() >= READBACK_TIMEOUT || Instant::now() >= active.deadline {
                    active.image = Some(Err("GPU readback timed out".into()));
                    if let Some(console) = console.as_mut() { console.capture_suppressed = false; }
                    return;
                }
                if windows.single().is_ok_and(|(entity, window)| !window.visible || occlusion.0 == Some((entity, true))) {
                    active.image = Some(Err("primary window became hidden, occluded, or minimized".into()));
                    if let Some(console) = console.as_mut() { console.capture_suppressed = false; }
                    return;
                }
                let pixels = u64::from(event.image.width()) * u64::from(event.image.height());
                let bytes = event.image.data.as_ref().map_or(0, Vec::len);
                active.image = Some(if pixels > MAX_CAPTURE_PIXELS || bytes > MAX_CAPTURE_BYTES {
                    Err("captured image exceeds readback limits".into())
                } else { Ok(event.image.clone()) });
                active.snapshot["capture_callback"] = json!({"unix_ms":epoch_ms(), "rendered_frame":null,
                    "note":"callback time is not the rendered frame time"});
                if let Some(console) = console.as_mut() { console.capture_suppressed = false; }
            },
        ).id();
        world.resource_mut::<CaptureSlot>().0 = Some(entity);
        active.screenshot_entity = Some(entity);
    }
    active
}

fn remove_capture(world: &mut World, active: &mut ActiveReport) {
    if let Some(entity) = active.screenshot_entity.take()
        && !world
            .get_entity(entity)
            .is_ok_and(|entity| entity.contains::<Capturing>())
    {
        if world.get_entity(entity).is_ok() {
            world.despawn(entity);
        }
        if let Some(mut slot) = world.get_resource_mut::<CaptureSlot>()
            && slot.0 == Some(entity)
        {
            slot.0 = None;
        }
    }
    active.capture_sample_pending = false;
    suppress_console(world, false);
}

fn start_packaging(world: &mut World, service: &mut BugReportService, active: &mut ActiveReport) {
    // Only the two local paths are needed by the worker; never clone private profiling/CLI text.
    let (output_override, assets_dir) = world
        .get_resource::<EngineConfig>()
        .map(|config| (config.bug_report_dir.clone(), config.assets_dir.clone()))
        .unwrap_or_else(|| (None, PathBuf::from("modern_assets")));
    let (screenshot, screenshot_error) = match active.image.take() {
        Some(Ok(image)) => (Some(image), None),
        Some(Err(reason)) => (None, Some(reason)),
        None => (None, Some("GPU readback timed out".into())),
    };
    let id = active.id.clone();
    let created_unix_ms = active.created_unix_ms;
    let started = active.started;
    let deadline = active.deadline;
    let cancelled = active.cancelled.clone();
    let mut snapshot = std::mem::take(&mut active.snapshot);
    let logs = std::mem::take(&mut active.logs);
    active.phase = Phase::Packaging;
    active.phase_started = Instant::now();
    let worker = thread::Builder::new()
        .name("mudcrab-bug-report-packaging".into())
        .spawn(move || {
            let result = (|| {
                let check = || {
                    if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                        Err("Bug report collection canceled or timed out".to_owned())
                    } else {
                        Ok(())
                    }
                };
                check()?;
                let output_dir = match output_override {
                    Some(path) => std::path::absolute(path).map_err(|e| e.to_string())?,
                    None => export::default_report_dir()?,
                };
                let assets = std::path::absolute(&assets_dir).map_err(|e| e.to_string())?;
                let mut roots = vec![
                    (assets.clone(), "<assets>".into()),
                    (output_dir.clone(), "<reports>".into()),
                ];
                for variable in ["HOME", "USERPROFILE"] {
                    if let Some(home) = std::env::var_os(variable) {
                        roots.push((PathBuf::from(home), "~".into()));
                    }
                }
                check()?;
                if let Ok(canonical) = std::fs::canonicalize(&assets) {
                    roots.push((canonical, "<assets>".into()));
                }
                check()?;
                snapshot["content"] = snapshot::content_metadata(&assets);
                check()?;
                let mut warnings = Vec::new();
                for (pointer, warning) in [
                    ("/build/git_commit", "Build commit is unavailable"),
                    ("/platform/gpu", "GPU adapter diagnostics are unavailable"),
                    (
                        "/session/active_space",
                        "Current active-space identity is unavailable",
                    ),
                    ("/pose/camera", "Camera pose is unavailable"),
                ] {
                    if snapshot.pointer(pointer).is_none_or(Value::is_null) {
                        warnings.push(warning);
                    }
                }
                if snapshot
                    .pointer("/performance/sample_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    == 0
                {
                    warnings.push("Recent frame-time diagnostics are unavailable");
                }
                snapshot["collection_warnings"] = json!(warnings);
                Ok(BundleInput {
                    id,
                    created_unix_ms,
                    started,
                    deadline,
                    cancelled,
                    snapshot,
                    logs,
                    screenshot,
                    screenshot_error,
                    output_dir,
                    sanitizer: Sanitizer::new(roots),
                })
            })();
            WorkerResult::Bundle(match result {
                Ok(input) => export::publish(input),
                Err(message) => Err(BundleError {
                    message,
                    recovered_directory: None,
                }),
            })
        });
    record_packaging_spawn_result(world, service, active, worker);
}

fn record_packaging_spawn_result(
    world: &mut World,
    service: &mut BugReportService,
    active: &mut ActiveReport,
    result: io::Result<JoinHandle<WorkerResult>>,
) {
    match result {
        Ok(worker) => active.worker = Some(worker),
        Err(error) => {
            let message = format!("Could not start bug-report packaging: {error}");
            console_output(world, &message);
            suppress_console(world, false);
            service.last_result = Some(ReportResult::Failed {
                message,
                recovered_directory: None,
            });
            active.phase = Phase::Retiring;
            active.worker = None;
        }
    }
}

fn report_browser_handoff_failure(world: &mut World, active: &ActiveReport, reason: &str) {
    console_output(
        world,
        &format!("Report saved; browser could not be opened: {reason}"),
    );
    if let Some(url) = &active.issue_url {
        console_output(world, url);
    }
    if let Some(directory) = &active.saved_directory {
        console_output(
            world,
            &format!("Issue draft: {}", directory.join("report.md").display()),
        );
    }
}

fn record_browser_spawn_result(
    world: &mut World,
    active: &mut ActiveReport,
    result: io::Result<JoinHandle<WorkerResult>>,
) -> bool {
    match result {
        Ok(worker) => {
            active.worker = Some(worker);
            false
        }
        Err(error) => {
            report_browser_handoff_failure(
                world,
                active,
                &format!("Could not start browser handoff: {error}"),
            );
            active.phase = Phase::Retiring;
            active.worker = None;
            true
        }
    }
}

fn timeout(world: &mut World, service: &mut BugReportService, active: &mut ActiveReport) {
    active.cancelled.store(true, Ordering::Release);
    remove_capture(world, active);
    let browser = active.phase == Phase::Browser;
    let message = if browser {
        "Report saved; browser launch timed out"
    } else {
        "Bug report collection timed out"
    };
    console_output(world, message);
    if let Some(url) = &active.issue_url {
        console_output(world, url);
    }
    if !browser {
        service.last_result = Some(ReportResult::Failed {
            message: message.into(),
            recovered_directory: None,
        });
    }
    active.phase = Phase::Retiring;
}

fn saved(
    world: &mut World,
    service: &mut BugReportService,
    active: &mut ActiveReport,
    outcome: BundleOutcome,
) -> bool {
    let text = if outcome.partial {
        "Report saved with missing diagnostics"
    } else {
        "Report saved"
    };
    console_output(world, &format!("{text}: {}", outcome.archive.display()));
    for missing in &outcome.missing {
        console_output(world, missing);
    }
    service.last_result = Some(ReportResult::Saved {
        directory: outcome.directory.clone(),
        archive: outcome.archive,
        partial: outcome.partial,
        missing: outcome.missing,
    });
    active.issue_url = Some(outcome.issue_url.clone());
    active.saved_directory = Some(outcome.directory);
    if !active.open_browser {
        return true;
    }
    let url = outcome.issue_url;
    active.cancelled = Arc::new(AtomicBool::new(false));
    let cancelled = active.cancelled.clone();
    active.phase = Phase::Browser;
    active.phase_started = Instant::now();
    let worker = thread::Builder::new()
        .name("mudcrab-bug-report-browser".into())
        .spawn(move || WorkerResult::Browser(open_browser(&url, &cancelled)));
    record_browser_spawn_result(world, active, worker)
}

fn drive_reports(world: &mut World) {
    let work_pending = world
        .get_resource::<BugReportService>()
        .is_some_and(|service| service.queued.is_some() || service.active.is_some());
    if !work_pending {
        return;
    }
    let Some(mut service) = world.remove_resource::<BugReportService>() else {
        return;
    };
    if let Some(open_browser) = service.queued.take() {
        service.active = Some(begin_report(world, open_browser));
    }
    if let Some(mut active) = service.active.take() {
        if active.phase == Phase::Capturing
            && Instant::now() < active.deadline
            && (active.image.is_some() || active.phase_started.elapsed() >= READBACK_TIMEOUT)
        {
            remove_capture(world, &mut active);
            start_packaging(world, &mut service, &mut active);
        }
        let exceeded = match active.phase {
            Phase::Capturing | Phase::Packaging => Instant::now() >= active.deadline,
            Phase::Browser => active.phase_started.elapsed() >= BROWSER_TIMEOUT,
            Phase::Retiring => false,
        };
        if exceeded {
            timeout(world, &mut service, &mut active);
        }
        let mut finished = active.phase == Phase::Retiring && active.worker.is_none();
        if active.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let result = active.worker.take().expect("finished worker").join();
            if active.phase == Phase::Retiring {
                if let Ok(WorkerResult::Bundle(Ok(outcome))) = result {
                    console_output(
                        world,
                        &format!(
                            "Timed-out report files preserved: {}",
                            outcome.directory.display()
                        ),
                    );
                } else if let Ok(WorkerResult::Bundle(Err(error))) = result
                    && let Some(directory) = error.recovered_directory
                {
                    console_output(
                        world,
                        &format!("Report files preserved: {}", directory.display()),
                    );
                }
                finished = true;
            } else {
                match result {
                    Ok(WorkerResult::Bundle(Ok(outcome))) => {
                        finished = saved(world, &mut service, &mut active, outcome);
                    }
                    Ok(WorkerResult::Bundle(Err(error))) => {
                        console_output(world, &format!("Bug report failed: {}", error.message));
                        if let Some(directory) = &error.recovered_directory {
                            console_output(
                                world,
                                &format!(
                                    "Report files preserved; ZIP could not be saved: {}",
                                    directory.display()
                                ),
                            );
                        }
                        service.last_result = Some(ReportResult::Failed {
                            message: error.message,
                            recovered_directory: error.recovered_directory,
                        });
                        finished = true;
                    }
                    Ok(WorkerResult::Browser(result)) => {
                        if let Err(reason) = result {
                            report_browser_handoff_failure(world, &active, &reason);
                        } else {
                            console_output(
                                world,
                                "GitHub browser handoff requested; review the form and attach the ZIP",
                            );
                        }
                        finished = true;
                    }
                    Err(_) => {
                        let message = "Bug report worker failed".to_owned();
                        console_output(world, &message);
                        if active.phase != Phase::Browser {
                            service.last_result = Some(ReportResult::Failed {
                                message,
                                recovered_directory: None,
                            });
                        }
                        finished = true;
                    }
                }
            }
        }
        if !finished {
            service.active = Some(active);
        }
    }
    world.insert_resource(service);
}

// Last runs before render extraction, after the scene's Update/PostUpdate systems. Re-check size
// here so a resize between command dispatch and rendering cannot bypass the allocation bound.
fn capture_frame_metadata(world: &mut World) {
    let capture_sample_pending = world
        .get_resource::<BugReportService>()
        .and_then(|service| service.active.as_ref())
        .is_some_and(|active| active.phase == Phase::Capturing && active.capture_sample_pending);
    if !capture_sample_pending {
        return;
    }
    let Some(mut service) = world.remove_resource::<BugReportService>() else {
        return;
    };
    if let Some(active) = service.active.as_mut()
        && active.phase == Phase::Capturing
        && active.capture_sample_pending
    {
        // This report owns the existing slot; temporarily exclude it from the size recheck.
        let slot = world.resource_mut::<CaptureSlot>().0.take();
        let unavailable = capture_unavailable(world);
        world.resource_mut::<CaptureSlot>().0 = slot;
        if let Some(reason) = unavailable {
            remove_capture(world, active);
            active.image = Some(Err(reason));
        } else {
            // Keep scene fields for the submitted frame, without duplicating bounded failure
            // histories and causing the entire runtime snapshot to exceed the export cap.
            let mut capture = snapshot::capture(world);
            let mut scene = serde_json::Map::new();
            for key in ["context", "session", "pose", "rendering"] {
                if let Some(value) = capture.get_mut(key) {
                    scene.insert(key.into(), value.take());
                }
            }
            active.snapshot["capture_context"] = Value::Object(scene);
            active.snapshot["capture_submission"] = json!({"unix_ms":epoch_ms(),"main_world_frame":frame(world),
                    "rendered_frame":null,"note":"main-world snapshot before render extraction"});
            active.capture_sample_pending = false;
        }
    }
    world.insert_resource(service);
}

fn open_browser(url: &str, cancelled: &AtomicBool) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| "invalid issue URL".to_string())?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || parsed.path() != "/Mudcrab-Team/mudcrab/issues/new"
        || url.len() > 4096
    {
        return Err("invalid issue URL destination".into());
    }
    if cancelled.load(Ordering::Acquire) {
        return Err("browser handoff canceled".into());
    }
    open_system_browser(url, cancelled)
}

#[cfg(windows)]
fn open_system_browser(url: &str, _cancelled: &AtomicBool) -> Result<(), String> {
    use windows_sys::Win32::{
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    };
    // ShellExecuteW may load COM-based shell extensions that need an STA on this worker.
    // RPC_E_CHANGED_MODE leaves the existing apartment in place and must not be balanced.
    let com = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let operation: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    // Both UTF-16 strings are terminated and remain live through this native shell call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if com >= 0 {
        // S_OK and S_FALSE both require a matching uninitialize on this thread.
        unsafe { CoUninitialize() };
    }
    if result > 32 {
        Ok(())
    } else {
        Err(format!("system browser error {result}"))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_system_browser(url: &str, cancelled: &AtomicBool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let program = "/usr/bin/open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    let mut child = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("system browser exit {status}"))
                };
            }
            None if cancelled.load(Ordering::Acquire) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("browser launch timed out".into());
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn open_system_browser(_url: &str, _cancelled: &AtomicBool) -> Result<(), String> {
    Err("system browser handoff is unsupported on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::{ConsolePlugin, console_closed, execute_line};
    use bevy::ecs::system::RunSystemOnce;

    fn test_app(directory: &std::path::Path) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::input::InputPlugin))
            .insert_resource(EngineConfig {
                bug_report_dir: Some(directory.into()),
                headless: true,
                ..default()
            })
            .add_plugins((ConsolePlugin, BugReportPlugin));
        app
    }

    #[test]
    fn command_validates_arguments_and_queues_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());
        execute_line(app.world_mut(), "bugreport extra bad");
        assert!(!app.world().resource::<BugReportService>().is_busy());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        execute_line(app.world_mut(), "BuGrEpOrT LOCAL");
        assert!(app.world().resource::<BugReportService>().is_busy());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        assert!(
            request_report(app.world_mut(), false)
                .unwrap_err()
                .contains("already")
        );
    }

    #[test]
    fn idle_report_systems_do_not_mark_service_changed() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());

        app.world_mut().clear_trackers();
        drive_reports(app.world_mut());
        assert!(!app.world().resource_ref::<BugReportService>().is_changed());

        app.world_mut().clear_trackers();
        capture_frame_metadata(app.world_mut());
        assert!(!app.world().resource_ref::<BugReportService>().is_changed());
    }

    #[test]
    fn packaging_spawn_error_finishes_report_and_restores_console() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());
        let mut service = BugReportService::default();
        let mut active = begin_report(app.world_mut(), false);
        suppress_console(app.world_mut(), true);

        record_packaging_spawn_result(
            app.world_mut(),
            &mut service,
            &mut active,
            Err(io::Error::other("simulated spawn failure")),
        );

        assert!(active.worker.is_none());
        assert_eq!(active.phase, Phase::Retiring);
        assert!(!app.world().resource::<ConsoleState>().capture_suppressed);
        let Some(ReportResult::Failed {
            message,
            recovered_directory: None,
        }) = &service.last_result
        else {
            panic!("packaging spawn failure must become a completed failed report");
        };
        assert!(message.contains("simulated spawn failure"));
        assert!(
            app.world()
                .resource::<ConsoleState>()
                .scrollback
                .iter()
                .any(|line| line.contains("Could not start bug-report packaging"))
        );
    }

    #[test]
    fn browser_spawn_error_keeps_saved_bundle_and_prints_recovery_paths() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());
        let saved_directory = directory.path().join("saved-report");
        let issue_url =
            "https://github.com/Mudcrab-Team/mudcrab/issues/new?template=bug_report.yml";
        let service = BugReportService {
            queued: None,
            active: None,
            last_result: Some(ReportResult::Saved {
                directory: saved_directory.clone(),
                archive: saved_directory.join("report.zip"),
                partial: false,
                missing: Vec::new(),
            }),
        };
        let mut active = begin_report(app.world_mut(), true);
        active.issue_url = Some(issue_url.into());
        active.saved_directory = Some(saved_directory.clone());
        active.phase = Phase::Browser;

        let finished = record_browser_spawn_result(
            app.world_mut(),
            &mut active,
            Err(io::Error::other("simulated spawn failure")),
        );

        assert!(finished);
        assert!(active.worker.is_none());
        assert_eq!(active.phase, Phase::Retiring);
        assert!(matches!(
            service.last_result,
            Some(ReportResult::Saved { ref directory, .. }) if directory == &saved_directory
        ));
        let output = &app.world().resource::<ConsoleState>().scrollback;
        assert!(
            output
                .iter()
                .any(|line| line.contains("Could not start browser handoff"))
        );
        assert!(output.iter().any(|line| line == issue_url));
        assert!(output
            .iter()
            .any(|line| line.contains(&saved_directory.join("report.md").display().to_string())));
    }

    #[test]
    fn paused_headless_report_publishes_partial_archive_and_preserves_pause() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());
        app.world_mut().resource_mut::<Time<Virtual>>().pause();
        request_report(app.world_mut(), true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.update();
            if !app.world().resource::<BugReportService>().is_busy() {
                break;
            }
            assert!(Instant::now() < deadline, "paused reporting did not finish");
            thread::sleep(Duration::from_millis(5));
        }
        let result = app
            .world()
            .resource::<BugReportService>()
            .last_result
            .as_ref()
            .unwrap();
        assert!(
            matches!(result, ReportResult::Saved { partial:true, archive, .. } if archive.is_file())
        );
        assert!(app.world().resource::<Time<Virtual>>().is_paused());
        assert!(!app.world().resource::<ConsoleState>().capture_suppressed);
    }

    #[test]
    fn suppressed_closed_console_blocks_gameplay_without_reopening_on_restore() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());
        suppress_console(app.world_mut(), true);
        assert!(!app.world_mut().run_system_once(console_closed).unwrap());
        suppress_console(app.world_mut(), false);
        assert!(!app.world().resource::<ConsoleState>().open);
        assert!(app.world_mut().run_system_once(console_closed).unwrap());
    }

    #[test]
    fn expired_capture_does_not_spawn_packaging_work() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());
        let mut active = begin_report(app.world_mut(), false);
        active.deadline = Instant::now() - Duration::from_millis(1);
        app.world_mut().resource_mut::<BugReportService>().active = Some(active);
        drive_reports(app.world_mut());
        let service = app.world().resource::<BugReportService>();
        assert!(!service.is_busy());
        assert!(matches!(
            service.last_result,
            Some(ReportResult::Failed { .. })
        ));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn timed_out_worker_retains_its_slot_until_it_exits() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());
        let mut active = begin_report(app.world_mut(), false);
        let (release, blocked) = std::sync::mpsc::channel();
        active.phase = Phase::Packaging;
        active.deadline = Instant::now() - Duration::from_millis(1);
        active.worker = Some(thread::spawn(move || {
            blocked.recv().unwrap();
            WorkerResult::Bundle(Err(BundleError {
                message: "canceled".into(),
                recovered_directory: None,
            }))
        }));
        app.world_mut().resource_mut::<BugReportService>().active = Some(active);
        suppress_console(app.world_mut(), true);
        drive_reports(app.world_mut());
        assert!(!app.world().resource::<ConsoleState>().capture_suppressed);
        assert!(
            request_report(app.world_mut(), false)
                .unwrap_err()
                .contains("still finishing")
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.world().resource::<BugReportService>().is_busy() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
            drive_reports(app.world_mut());
        }
        assert!(matches!(
            app.world().resource::<BugReportService>().last_result,
            Some(ReportResult::Failed { .. })
        ));
    }

    #[test]
    fn hidden_and_occluded_windows_skip_readback() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());
        app.insert_resource(ReportRendererAvailable(true));
        let entity = app
            .world_mut()
            .spawn((
                Window {
                    visible: false,
                    ..default()
                },
                PrimaryWindow,
            ))
            .id();
        app.world_mut().spawn(Camera3d::default());
        assert!(
            capture_unavailable(app.world_mut())
                .unwrap()
                .contains("hidden")
        );
        app.world_mut().get_mut::<Window>(entity).unwrap().visible = true;
        app.world_mut().resource_mut::<WindowOcclusion>().0 = Some((entity, true));
        assert!(
            capture_unavailable(app.world_mut())
                .unwrap()
                .contains("occluded")
        );
        app.world_mut().resource_mut::<WindowOcclusion>().0 = Some((entity, false));
        assert!(capture_unavailable(app.world_mut()).is_none());
    }

    #[test]
    fn pending_gpu_slot_prevents_additional_readbacks() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app(directory.path());
        let pending = app.world_mut().spawn(Capturing).id();
        app.world_mut().resource_mut::<CaptureSlot>().0 = Some(pending);
        let mut active = begin_report(app.world_mut(), false);
        assert!(
            matches!(&active.image, Some(Err(reason)) if reason.contains("previous screenshot"))
        );
        active.screenshot_entity = Some(pending);
        remove_capture(app.world_mut(), &mut active);
        assert!(app.world().get_entity(pending).is_ok());
        assert_eq!(app.world().resource::<CaptureSlot>().0, Some(pending));
    }

    #[test]
    fn browser_rejects_untrusted_destinations_before_opening_anything() {
        let cancelled = AtomicBool::new(true);
        for url in [
            "file:///tmp/report",
            "https://example.com/",
            "https://github.com/other/issues/new",
        ] {
            assert!(open_browser(url, &cancelled).is_err());
        }
        assert!(
            open_browser(
                "https://github.com/Mudcrab-Team/mudcrab/issues/new",
                &cancelled
            )
            .unwrap_err()
            .contains("canceled")
        );
    }
}
