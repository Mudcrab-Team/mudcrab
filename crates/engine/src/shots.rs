//! `--shots`: render a list of exact camera poses, one PNG each, then exit.
//!
//! A shots file is a small JSON document (`docs/specs/engine/reference-shots.md`): a frame size,
//! `{ "width", "height" }`, and a list of poses in Creation units - `position`, `yaw` (a heading in
//! degrees clockwise from north), `pitch` (degrees, positive looking down) and `hfov` (degrees).
//! Each pose is rendered to `<out>/<name>.png` at exactly the frame size the file asks for, so the
//! image can be put beside a reference screenshot of the same view.
//!
//! Nothing here is interactive: the run poses the camera, waits for the streamer to finish loading
//! around the pose, takes the screenshot, and exits when the file runs out of shots. The run streams
//! one worldspace, the one its first exterior shot names; a shot in an interior or in another
//! worldspace is skipped, and the log says so.
//!
//! `shots.log`, next to the images, has one line per shot: its name, how many frames it waited to
//! settle, whether it settled or the timeout took it, and the image's path - appended as each shot
//! finishes, so a run that is killed keeps what it had already done.

use crate::{
    render::RendererMetrics,
    streaming::{RenderOrigin, StreamingMetrics},
    world::components::{CELL_SIZE, StreamingCamera},
};
use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, ScreenshotCaptured},
    window::{PrimaryWindow, WindowResolution},
};
use serde::Deserialize;
use std::{
    collections::{BTreeSet, HashSet},
    fmt, fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// Frames a view has to report nothing pending for, in a row, before it is photographed.
///
/// A single quiet frame is not enough: the counts fall to zero when the last response is committed,
/// and the frames right after that are the ones in which the terrain's textures, the sun's shadow
/// cascades and the renderer's indirect draw buffers catch up with the cells that just arrived.
pub const SETTLE_QUIET_FRAMES: u32 = 10;

/// Seconds after start-up before any shot is taken, as for the acceptance screenshot: the first
/// frames compile pipelines, and a mesh whose pipeline is not ready yet is simply not drawn.
pub const WARM_UP_SECONDS: f32 = 2.0;

/// Seconds a shot's view may take to settle before the shot is taken anyway.
///
/// A pose the streamer cannot finish - an asset that never loads, a cell the database refuses -
/// must not hang a file of forty shots: the shot is taken with whatever is resident and its log
/// line says `timed_out` with what was still pending.
pub const SETTLE_TIMEOUT_SECONDS: f32 = 30.0;

/// Seconds a requested screenshot may take to reach the disk before the shot is given up on.
pub const CAPTURE_TIMEOUT_SECONDS: f32 = 60.0;

/// Frames a run waits for the streaming camera to exist before it gives up: without one no shot
/// can be posed, and the run would otherwise never end.
pub const MISSING_CAMERA_FRAMES: u32 = 600;

/// The longest side of a shot's frame, in pixels.
///
/// The window is opened at the file's frame, and a swapchain wider or taller than the limits of
/// common hardware cannot be created: the run would fail after the window opened, with an error
/// from the renderer rather than from the file. The check is on each side, not the pixel count, so
/// a very wide frame is refused like a very tall one - and a frame at the cap on both sides is a
/// 8192x8192 (256 MB) framebuffer encoded on the main thread.
pub const MAX_FRAME_SIDE_PIXELS: u32 = 8192;

// ---------------------------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------------------------

/// A shots file: the frame every image is rendered at, and the poses to render.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ShotsFile {
    /// Window width in pixels, and the width of every PNG.
    pub width: u32,
    /// Window height in pixels, and the height of every PNG.
    pub height: u32,
    pub shots: Vec<Shot>,
}

impl ShotsFile {
    /// Reads and checks a shots file. The failure carries the path, because a run that cannot read
    /// its poses has nothing to fall back on.
    pub fn load(path: &Path) -> Result<Self, ShotsError> {
        let text = fs::read_to_string(path)
            .map_err(|error| ShotsError(format!("could not read {}: {error}", path.display())))?;
        Self::parse(&text).map_err(|error| ShotsError(format!("in {}: {error}", path.display())))
    }

    /// Parses a shots file's text. Fields the engine does not know (a comparison tool's own
    /// annotations) are ignored, as `reference` and `note` are.
    pub fn parse(text: &str) -> Result<Self, ShotsError> {
        let file: Self = serde_json::from_str(text)
            .map_err(|error| ShotsError(format!("not a shots file: {error}")))?;
        file.validate()?;
        Ok(file)
    }

    pub(crate) fn validate(&self) -> Result<(), ShotsError> {
        if self.width == 0 || self.height == 0 {
            let frame = format!("{}x{}", self.width, self.height);
            let message = format!("the frame is {frame}: a screenshot of no pixels is not a shot");
            return Err(ShotsError(message));
        }
        if self.width > MAX_FRAME_SIDE_PIXELS || self.height > MAX_FRAME_SIDE_PIXELS {
            let frame = format!("{}x{}", self.width, self.height);
            let message = format!(
                "the frame is {frame}: a side longer than {MAX_FRAME_SIDE_PIXELS} pixels is more \
                 than the window can be"
            );
            return Err(ShotsError(message));
        }
        if self.shots.is_empty() {
            return Err(ShotsError("the file has no shots".to_owned()));
        }
        // Names are compared ignoring case: on Windows "Tower.png" and "tower.png" are one file.
        let mut names = HashSet::new();
        for shot in &self.shots {
            shot.validate()?;
            if !names.insert(shot.name.to_lowercase()) {
                let message = format!(
                    "two shots are named \"{}\" (ignoring case): the second image would replace \
                     the first",
                    shot.name
                );
                return Err(ShotsError(message));
            }
        }
        Ok(())
    }

    /// The window size the file asks for, in physical pixels: the scale factor is pinned to 1, so
    /// the window - and so the PNG - is exactly this many pixels whatever the display is set to.
    pub fn window_resolution(&self) -> WindowResolution {
        WindowResolution::new(self.width, self.height).with_scale_factor_override(1.0)
    }

    /// Width over height: the shape of every frame in the file.
    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    /// Where the run starts streaming: the worldspace the first exterior shot names (`None` leaves
    /// it to `--worldspace`) and the grid square its camera stands over. `None` when the file has
    /// no exterior shot.
    pub fn start(&self) -> Option<(Option<u32>, (i32, i32))> {
        let shot = self.shots.iter().find(|shot| shot.is_exterior())?;
        Some((shot.worldspace_id, shot.grid()))
    }
}

/// One camera pose, rendered to `<out>/<name>.png`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Shot {
    /// Output file stem: the engine writes `<out>/<name>.png`.
    pub name: String,
    /// The worldspace of an exterior shot, or `None` when the file leaves it to `--worldspace`.
    #[serde(default)]
    pub worldspace_id: Option<u32>,
    /// The interior cell of an interior shot, or `None` for an exterior one.
    #[serde(default)]
    pub interior_cell_id: Option<u32>,
    /// The camera's eye, Creation units, absolute (not relative to a cell or the render origin).
    pub position: [f32; 3],
    /// Skyrim heading in degrees: 0 looks north (Creation `+Y`), 90 looks east (`+X`), clockwise
    /// seen from above.
    pub yaw: f32,
    /// Skyrim player X angle in degrees: positive looks down, negative up.
    pub pitch: f32,
    /// Horizontal field of view in degrees, in this file's aspect.
    pub hfov: f32,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl Shot {
    /// Whether this is an exterior pose, which is what the streamer holds cells for.
    pub fn is_exterior(&self) -> bool {
        self.interior_cell_id.is_none()
    }

    /// The exterior grid square the camera stands over.
    pub fn grid(&self) -> (i32, i32) {
        let [x, y, _] = self.position;
        (
            (x / CELL_SIZE).floor() as i32,
            (y / CELL_SIZE).floor() as i32,
        )
    }

    fn validate(&self) -> Result<(), ShotsError> {
        let name = &self.name;
        if name.is_empty() {
            return Err(ShotsError(
                "a shot has no name to name its image".to_owned(),
            ));
        }
        // The name is a file stem inside the output folder, never a path out of it: the separators,
        // and the characters Windows refuses in a file name, are all malformed here, so that the
        // run stops before the window opens rather than at the shot's write.
        let plain = name.chars().all(|character| {
            !matches!(
                character,
                '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
            ) && !character.is_control()
        }) && name != "."
            && name != "..";
        if !plain {
            let message = format!("shot \"{name}\" has a name that is not a plain file name");
            return Err(ShotsError(message));
        }
        // Windows cannot create a name that ends in a dot or a space: it strips the trailing
        // characters, so the image would be written under a name the file never asked for.
        if name.ends_with('.') || name.ends_with(' ') {
            let message = format!(
                "shot \"{name}\" has a name that ends in a dot or a space, which Windows cannot \
                 create as a file"
            );
            return Err(ShotsError(message));
        }
        if is_reserved_device_name(name) {
            let message = format!(
                "shot \"{name}\" is a reserved device name on Windows, which cannot name a file"
            );
            return Err(ShotsError(message));
        }
        let finite = self.position.iter().all(|value| value.is_finite())
            && self.yaw.is_finite()
            && self.pitch.is_finite()
            && self.hfov.is_finite();
        if !finite {
            let message = format!("shot \"{name}\" has a pose that is not a number");
            return Err(ShotsError(message));
        }
        if self.hfov <= 0.0 || self.hfov >= 180.0 {
            let hfov = self.hfov;
            let message = format!(
                "shot \"{name}\" asks for a horizontal field of view of {hfov} degrees, \
                 which is not one between 0 and 180"
            );
            return Err(ShotsError(message));
        }
        // The pitch is a Skyrim player X angle: straight down is 90, straight up -90, and a value
        // outside that range is not a pose the game can hold.
        if self.pitch < -90.0 || self.pitch > 90.0 {
            let pitch = self.pitch;
            let message = format!(
                "shot \"{name}\" asks for a pitch of {pitch} degrees, which is not one between \
                 -90 and 90"
            );
            return Err(ShotsError(message));
        }
        Ok(())
    }
}

/// Whether a name is one of the DOS device names Windows refuses to create, whatever extension it
/// carries: `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9` and `LPT1`-`LPT9`, ignoring case. Windows
/// drops the extension before the check, so `con.png` and `Nul.x` are reserved like `NUL` is.
fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let digit = stem.as_bytes().get(3).copied();
    stem.len() == 4
        && matches!(stem.get(..3), Some("COM" | "LPT"))
        && matches!(digit, Some(b'1'..=b'9'))
}

/// A shots file that could not be read or is not a shots file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShotsError(pub(crate) String);

impl fmt::Display for ShotsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ShotsError {}

/// Where the images and `shots.log` go when `--shots-out` is not given: a folder named after the
/// shots file, next to it (`reference/riverwood_shots.json` -> `reference/riverwood_shots-shots/`).
pub fn default_output_dir(shots_path: &Path) -> PathBuf {
    let stem = shots_path.file_stem().map_or_else(
        || "shots".into(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let directory = format!("{stem}-shots");
    match shots_path.parent() {
        Some(parent) => parent.join(directory),
        None => PathBuf::from(directory),
    }
}

// ---------------------------------------------------------------------------------------------
// The pose
// ---------------------------------------------------------------------------------------------

/// The camera rotation for a Skyrim pose.
///
/// `yaw` is a Creation heading in degrees, clockwise from north (`+Y`) seen from above, where
/// Creation `+Y` is runtime `-Z`. `pitch` is Skyrim's player X angle, positive looking **down**.
pub fn shot_camera_rotation(yaw_degrees: f32, pitch_degrees: f32) -> Quat {
    // A camera looks down its own -Z, and runtime -Z is Creation north: turning -yaw about up puts
    // that along Creation `(sin yaw, cos yaw, 0)`, and the pitch turns the same forward down.
    let yaw = Quat::from_rotation_y(-yaw_degrees.to_radians());
    let pitch = Quat::from_rotation_x(-pitch_degrees.to_radians());
    yaw * pitch
}

/// The vertical field of view for a horizontal one at `aspect` (width / height), in degrees:
/// `tan(h / 2) / tan(v / 2) = aspect` for a rectilinear projection.
pub fn vertical_fov_degrees(hfov_degrees: f32, aspect: f32) -> f32 {
    let half_horizontal = (hfov_degrees.to_radians() * 0.5).tan();
    (2.0 * (half_horizontal / aspect).atan()).to_degrees()
}

/// Where a shot's camera stands in render space: the pose in Creation units converted to runtime
/// axes, measured from the floating origin, which is how the streamer places every cell.
pub fn shot_camera_translation(position: [f32; 3], origin: IVec2) -> Vec3 {
    let axes = shared::coordinates::creation_to_runtime_vector(position);
    let offset_x = origin.x as f32 * CELL_SIZE;
    let offset_z = -(origin.y as f32 * CELL_SIZE);
    Vec3::from_array(axes) - Vec3::new(offset_x, 0.0, offset_z)
}

/// Why a shot is passed over rather than rendered, or `None` when it can be rendered in a run that
/// streams `worldspace_id`. The streamer holds the exterior cells of one worldspace per run.
pub fn skip_reason(shot: &Shot, worldspace_id: u32) -> Option<String> {
    if !shot.is_exterior() {
        return Some("interior".to_owned());
    }
    match shot.worldspace_id {
        Some(named) if named != worldspace_id => Some(format!(
            "worldspace {named:08X}, not the {worldspace_id:08X} this run streams"
        )),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The settle rule
// ---------------------------------------------------------------------------------------------

/// What the settle rule reads, so that the rule is a pure function of the counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct SettleCounts {
    /// Cells submitted to the database but not yet resident.
    pub loading_cells: usize,
    /// Database requests in flight.
    pub active_requests: usize,
    /// Spawned model scenes whose assets have not finished loading.
    pub pending_asset_instances: usize,
    /// Terrain and water surfaces whose textures have not finished loading.
    pub pending_surface_instances: usize,
    /// Loaded models still waiting for their turn in the arming budget.
    pub arming_queue_depth: usize,
    /// Cells out of range still waiting for their turn in the unload budget: still drawn.
    pub retiring_cells: usize,
    /// Cells the database refused, or whose terrain or water failed validation, since this shot
    /// started settling (the run's counter is cumulative; see [`Failures`]).
    pub failed_cells: u64,
    /// Assets that failed to load at all, since this shot started settling.
    pub asset_load_failures: u64,
    /// Material, terrain, water, transform-bounds and renderer validation failures together: a
    /// view with one of these was drawn from something the engine refused, and a shot of it is
    /// not the reference the run is trying to reproduce. Counted since this shot started settling.
    pub validation_failures: u64,
    /// The renderer's final path is running and the warm-up has passed.
    pub renderer_ready: bool,
    /// Frames the counts have been quiet for, before this frame.
    pub quiet_frames: u32,
    pub pending_lod_queries: usize,
    pub pending_lod_chunks: usize,
    pub failed_lod_work: u64,
    pub outstanding_terrain_uploads: usize,
    pub pending_batch_cpu: usize,
    pub pending_batch_initial: usize,
    pub pending_batch_selection: usize,
    pub pending_specializations: Option<usize>,
    pub pending_render_transfers: Option<usize>,
}

/// The run-wide failure counters at one moment. They only ever grow, so a shot compares them with
/// the snapshot taken when it started settling: a failure from an earlier shot's view must not
/// hold every later shot back until the timeout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Failures {
    pub failed_cells: u64,
    pub asset_load_failures: u64,
    pub validation_failures: u64,
}

impl Failures {
    /// The cumulative counters as the streamer and the renderer report them now.
    pub fn read(streaming: &StreamingMetrics, renderer: &RendererMetrics) -> Self {
        Self {
            failed_cells: streaming.failed_cells,
            asset_load_failures: streaming.asset_load_failures,
            validation_failures: streaming
                .material_validation_failures
                .saturating_add(streaming.terrain_validation_failures)
                .saturating_add(streaming.water_validation_failures)
                .saturating_add(streaming.transform_bounds_validation_failures)
                .saturating_add(renderer.renderer_validation_failures),
        }
    }
}

impl SettleCounts {
    /// What the streamer and the renderer say is still pending, with the quiet window so far. The
    /// failure fields hold only the failures that happened after `baseline`.
    pub fn read(
        streaming: &StreamingMetrics,
        renderer: &RendererMetrics,
        baseline: Failures,
        renderer_ready: bool,
        quiet_frames: u32,
    ) -> Self {
        let now = Failures::read(streaming, renderer);
        Self {
            loading_cells: streaming.loading_cells,
            active_requests: streaming.active_requests,
            pending_asset_instances: streaming.pending_asset_instances,
            pending_surface_instances: streaming.pending_surface_instances,
            arming_queue_depth: streaming.arming_queue_depth,
            retiring_cells: streaming.retiring_cells,
            failed_cells: now.failed_cells.saturating_sub(baseline.failed_cells),
            asset_load_failures: now
                .asset_load_failures
                .saturating_sub(baseline.asset_load_failures),
            validation_failures: now
                .validation_failures
                .saturating_sub(baseline.validation_failures),
            renderer_ready,
            quiet_frames,
            pending_lod_queries: streaming.pending_lod_queries,
            pending_lod_chunks: streaming.pending_lod_chunks,
            failed_lod_work: streaming
                .failed_lod_queries
                .saturating_add(streaming.failed_lod_chunks),
            ..default()
        }
    }

    /// Nothing is pending and nothing failed: every load the current view asked for has landed and
    /// is drawn. A failure is not "still pending", it is a view that will never be right, so the
    /// shot is held back and given up on by the timeout rather than photographed as settled.
    pub fn is_quiet(&self) -> bool {
        self.pending_lod_queries == 0
            && self.pending_lod_chunks == 0
            && self.failed_lod_work == 0
            && self.outstanding_terrain_uploads == 0
            && self.pending_batch_cpu == 0
            && self.pending_batch_initial == 0
            && self.pending_batch_selection == 0
            && self.pending_render_transfers == Some(0)
            && self.pending_specializations == Some(0)
            && self.loading_cells == 0
            && self.active_requests == 0
            && self.pending_asset_instances == 0
            && self.pending_surface_instances == 0
            && self.arming_queue_depth == 0
            && self.retiring_cells == 0
            && self.failed_cells == 0
            && self.asset_load_failures == 0
            && self.validation_failures == 0
            && self.renderer_ready
    }

    /// The pending work and the failures, for the log line of a shot that never settled.
    pub fn describe(&self) -> String {
        format!(
            "loading_cells={} active_requests={} pending_assets={} pending_surfaces={} \
             arming={} retiring={} failed_cells={} asset_load_failures={} validation_failures={} \
             renderer_ready={}",
            self.loading_cells,
            self.active_requests,
            self.pending_asset_instances,
            self.pending_surface_instances,
            self.arming_queue_depth,
            self.retiring_cells,
            self.failed_cells,
            self.asset_load_failures,
            self.validation_failures,
            self.renderer_ready
        ) + &format!(
            " lod_queries={} lod_chunks={} lod_failures={} terrain_transfers={} batch_cpu={} initial_uploads={} selection_uploads={} specializations={:?} render_transfers={:?}",
            self.pending_lod_queries,
            self.pending_lod_chunks,
            self.failed_lod_work,
            self.outstanding_terrain_uploads,
            self.pending_batch_cpu,
            self.pending_batch_initial,
            self.pending_batch_selection,
            self.pending_specializations,
            self.pending_render_transfers
        )
    }
}

/// The settle state one frame on: the frame counted into the quiet window when nothing was
/// pending, the window started again when something was.
pub fn advance_settle(counts: SettleCounts) -> SettleCounts {
    let quiet_frames = if counts.is_quiet() {
        counts.quiet_frames.saturating_add(1)
    } else {
        0
    };
    SettleCounts {
        quiet_frames,
        ..counts
    }
}

/// Whether a view may be photographed: nothing pending for [`SETTLE_QUIET_FRAMES`] frames running.
pub fn shots_settled(counts: &SettleCounts) -> bool {
    counts.is_quiet() && counts.quiet_frames >= SETTLE_QUIET_FRAMES
}

// ---------------------------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------------------------

/// Runs a shots file. Added by the app when `--shots` is given, beside `StreamingPlugin`: every
/// shot is a camera pose the streamer loads around, and the settle rule waits on it.
pub struct ShotsPlugin {
    pub run: ShotsRun,
}

impl Plugin for ShotsPlugin {
    fn build(&self, app: &mut App) {
        // In `Update`, so the pose is in the camera's `Transform` before `PostUpdate` reads it
        // (the sky dome follows the camera there, and transforms propagate).
        app.insert_resource(self.run.clone())
            .add_systems(Update, run_shots);
    }
}

/// The state of a `--shots` run: what to render, where it goes, and how far it has got.
#[derive(Resource, Debug, Clone)]
pub struct ShotsRun {
    pub file: ShotsFile,
    pub output_dir: PathBuf,
    /// The worldspace the run streams; shots in another one are skipped.
    pub worldspace_id: u32,
    pub(crate) route_checkpoints: Option<BTreeSet<usize>>,
    transition_capture: bool,
    shot: usize,
    phase: Phase,
    /// Seconds in the current phase; the settle and capture timeouts read it.
    timer: f32,
    /// Frames the current shot has waited for its view to settle, for the log.
    frames: u32,
    /// What was last pending, for the log of a shot that timed out.
    counts: SettleCounts,
    /// The run's failure counters when the current shot started settling.
    failure_baseline: Failures,
    timed_out: bool,
    /// The current shot's PNG write, set by its capture observer once it has run: `None` while the
    /// screenshot is still on its way back from the renderer.
    capture: Option<Result<(), String>>,
    directory_made: bool,
    window_checked: bool,
    /// The window's size when the last screenshot was asked for, when a window was found.
    window_size: Option<(u32, u32)>,
    /// This run has created `shots.log`: the first line truncates what an earlier run left there.
    log_started: bool,
    /// A write to `shots.log` has failed; the error is reported once, not on every line.
    log_failed: bool,
    written: bool,
    failed: bool,
    /// Frames in a row without exactly one streaming camera.
    frames_without_camera: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Pose the camera on the shot and start settling.
    Move,
    /// Hold a non-checkpoint route step through an extraction before advancing.
    Traverse,
    /// Wait for the view to settle (or time out), then ask for the screenshot.
    Settle,
    /// The screenshot has been asked for; wait for it on disk.
    Capture,
    /// Write the log and exit.
    Done,
}

impl ShotsRun {
    pub fn new(file: ShotsFile, output_dir: PathBuf, worldspace_id: u32) -> Self {
        Self {
            file,
            output_dir,
            worldspace_id,
            route_checkpoints: None,
            transition_capture: false,
            shot: 0,
            phase: Phase::Move,
            timer: 0.0,
            frames: 0,
            counts: SettleCounts::default(),
            failure_baseline: Failures::default(),
            timed_out: false,
            capture: None,
            directory_made: false,
            window_checked: false,
            window_size: None,
            log_started: false,
            log_failed: false,
            written: false,
            failed: false,
            frames_without_camera: 0,
        }
    }

    pub fn shot_path(&self, shot: &Shot) -> PathBuf {
        let suffix = if self.transition_capture {
            "-transition"
        } else {
            ""
        };
        self.output_dir.join(format!("{}{suffix}.png", shot.name))
    }

    pub fn log_path(&self) -> PathBuf {
        self.output_dir.join("shots.log")
    }

    /// One line of `shots.log`: the shot's name, how many frames it waited to settle, whether it
    /// settled or the timeout took it - and with what still pending, when it did - and its image.
    /// A window that is not the file's frame is named too: the image is then not the shape the
    /// reference is, which a comparison tool reading the log has to know.
    pub fn log_line(&self, shot: &Shot, path: &Path) -> String {
        let outcome = if self.transition_capture {
            "transition".to_owned()
        } else if self.timed_out {
            format!("timed_out ({})", self.counts.describe())
        } else {
            "settled".to_owned()
        };
        let mut line = format!(
            "{} frames={} {outcome} path={}",
            shot.name,
            self.frames,
            path.display()
        );
        if let Some((width, height)) = self.window_size
            && (width, height) != (self.file.width, self.file.height)
        {
            line.push_str(&format!(" window={width}x{height}"));
        }
        line
    }

    /// Records a line in the engine log and appends it to `shots.log` as the shot finishes.
    fn note(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref();
        info!(target: "shots", "{line}");
        self.append_log(line);
    }

    /// Appends one line to `shots.log`, creating the file on the first line (and truncating what an
    /// earlier run left there). Writing each line as it happens is what keeps a run that is killed
    /// from losing the record of the shots it had already taken.
    fn append_log(&mut self, line: &str) {
        if self.log_failed {
            return;
        }
        if let Err(error) = self.write_log_line(line) {
            self.log_failed = true;
            self.failed = true;
            error!(
                target: "shots",
                "could not append to {}: {error}",
                self.log_path().display()
            );
        }
    }

    fn write_log_line(&mut self, line: &str) -> std::io::Result<()> {
        fs::create_dir_all(&self.output_dir)?;
        let path = self.log_path();
        let mut file = if self.log_started {
            fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)?
        } else {
            fs::File::create(&path)?
        };
        writeln!(file, "{line}")?;
        self.log_started = true;
        Ok(())
    }

    fn fail(&mut self, reason: impl AsRef<str>) {
        self.failed = true;
        self.note(format!("FAILED: {}", reason.as_ref()));
    }

    fn enter(&mut self, phase: Phase) {
        self.phase = phase;
        self.timer = 0.0;
    }

    /// Starts waiting for the shot's view to settle. The quiet window counts from the frame after
    /// the pose, because the streamer's plan may have read the camera before this frame's pose
    /// reached it; counting from the pose frame would let a view that has not asked for a single
    /// cell yet read as quiet.
    fn start_settle(&mut self) {
        self.frames = 0;
        self.counts = SettleCounts::default();
        self.timed_out = false;
        self.enter(Phase::Settle);
    }

    /// Moves on to the next shot in the file, or to the end when this was the last one.
    fn advance(&mut self) {
        self.shot += 1;
        if self.shot < self.file.shots.len() {
            self.enter(Phase::Move);
        } else {
            self.enter(Phase::Done);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_shots(
    mut commands: Commands,
    time: Res<Time>,
    mut run: ResMut<ShotsRun>,
    origin: Res<RenderOrigin>,
    mut camera: Query<(&mut Transform, &mut Projection), With<StreamingCamera>>,
    streaming: Res<StreamingMetrics>,
    renderer: Res<RendererMetrics>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut exit: MessageWriter<AppExit>,
    profiler: Res<crate::profiling::ProfilingState>,
    uploads: Res<crate::terrain_upload::TerrainMeshUploadReadiness>,
    retries: Res<crate::capture_frame::RetryReadiness>,
    main_frame: Res<bevy::diagnostic::FrameCount>,
    transfers: Res<crate::capture_frame::TransferReadiness>,
) {
    // Borrowed once: the shot is taken from the file without cloning it, and the arms below still
    // write the run's other fields, which the borrow checker allows field by field.
    let run = &mut *run;
    if run.phase == Phase::Done {
        if !run.written {
            run.written = true;
            finish_log(run);
        }
        exit.write(if run.failed {
            AppExit::error()
        } else {
            AppExit::Success
        });
        return;
    }
    let Ok((mut transform, mut projection)) = camera.single_mut() else {
        run.frames_without_camera = run.frames_without_camera.saturating_add(1);
        if run.frames_without_camera >= MISSING_CAMERA_FRAMES {
            run.fail(format!(
                "no single streaming camera to pose within {MISSING_CAMERA_FRAMES} frames"
            ));
            run.enter(Phase::Done);
        }
        return;
    };
    run.frames_without_camera = 0;
    if run.shot >= run.file.shots.len() {
        run.fail("there is no shot to render");
        run.enter(Phase::Done);
        return;
    }
    // Borrowed from the file rather than cloned: the arms below only write other fields of the
    // run while this borrow is live, which the borrow checker allows field by field.
    let shot = &run.file.shots[run.shot];
    match run.phase {
        Phase::Move => {
            if let Some(reason) = skip_reason(shot, run.worldspace_id) {
                let line = format!("{} skipped: {reason}", shot.name);
                run.note(line);
                run.advance();
                return;
            }
            if !run.directory_made {
                run.directory_made = true;
                if let Err(error) = fs::create_dir_all(&run.output_dir) {
                    let message = format!(
                        "could not make the output directory {}: {error}",
                        run.output_dir.display()
                    );
                    run.fail(message);
                    run.enter(Phase::Done);
                    return;
                }
            }
            if !run.window_checked {
                run.window_checked = true;
                warn_window_size(run, &windows);
            }
            place_camera(
                shot,
                run.file.aspect(),
                origin.0,
                &mut transform,
                &mut projection,
            );
            run.failure_baseline = Failures::read(&streaming, &renderer);
            run.start_settle();
            if let Some(checkpoints) = &run.route_checkpoints {
                if checkpoints.contains(&run.shot) {
                    run.transition_capture = true;
                    request_capture(&mut commands, run, main_frame.0);
                } else {
                    run.enter(Phase::Traverse);
                }
            }
        }
        Phase::Traverse => {
            place_camera(
                shot,
                run.file.aspect(),
                origin.0,
                &mut transform,
                &mut projection,
            );
            run.note(format!(
                "route_step={} main_frame={} position={:?} elapsed_seconds={}",
                run.shot,
                main_frame.0,
                shot.position,
                time.elapsed_secs_f64()
            ));
            run.advance();
        }
        Phase::Settle => {
            // Re-assert the pose every frame: a render-origin rebase moves the camera in render
            // space, and the Creation position is what has to be photographed.
            place_camera(
                shot,
                run.file.aspect(),
                origin.0,
                &mut transform,
                &mut projection,
            );
            run.timer += time.delta_secs();
            run.frames = run.frames.saturating_add(1);
            let renderer_ready =
                renderer.final_path_active() && time.elapsed_secs() >= WARM_UP_SECONDS;
            let mut counts = SettleCounts::read(
                &streaming,
                &renderer,
                run.failure_baseline,
                renderer_ready,
                run.counts.quiet_frames,
            );
            counts.pending_lod_queries = streaming.pending_lod_queries;
            counts.pending_lod_chunks = streaming.pending_lod_chunks;
            counts.failed_lod_work = streaming
                .failed_lod_queries
                .saturating_add(streaming.failed_lod_chunks);
            counts.outstanding_terrain_uploads = uploads.outstanding();
            counts.pending_batch_cpu = profiler
                .gauge("lod/pending_terrain_batch_chunks")
                .unwrap_or(0.0) as usize;
            counts.pending_batch_initial = profiler
                .gauge("lod/pending_initial_terrain_upload_chunks")
                .unwrap_or(0.0) as usize;
            counts.pending_batch_selection = profiler
                .gauge("lod/pending_terrain_selection_uploads")
                .unwrap_or(0.0) as usize;
            counts.pending_specializations = *retries.0.lock().unwrap();
            counts.pending_render_transfers = *transfers.0.lock().unwrap();
            // The window is counted in before it is read, so a view that has been quiet for
            // `SETTLE_QUIET_FRAMES` frames running is photographed on that very frame: the log's
            // `frames=` is then the constant, not one more (the count read here is the previous
            // frame's).
            run.counts = advance_settle(counts);
            let settled = shots_settled(&run.counts);
            if settled || run.timer >= SETTLE_TIMEOUT_SECONDS {
                run.timed_out = !settled;
                if !settled {
                    // The shot is taken with what is resident, but a shot the run gave up on must
                    // not leave the run reporting success: a settled-only exit code would say the
                    // images can be compared when this one cannot.
                    run.failed = true;
                    warn!(
                        target: "shots",
                        "shot \"{}\" did not settle within {SETTLE_TIMEOUT_SECONDS:.0} s ({}); \
                         shooting anyway and failing the run",
                        shot.name,
                        run.counts.describe()
                    );
                }
                run.window_size = window_physical_size(&windows);
                let path = run.shot_path(shot);
                if let Err(message) = discard_previous_shot(&path) {
                    // The old image would be taken for this shot's, so the shot fails instead.
                    let message = format!("shot \"{}\": {message}", shot.name);
                    run.fail(message);
                    run.advance();
                    return;
                }
                run.transition_capture = false;
                request_capture(&mut commands, run, main_frame.0);
            }
        }
        Phase::Capture => {
            place_camera(
                shot,
                run.file.aspect(),
                origin.0,
                &mut transform,
                &mut projection,
            );
            run.timer += time.delta_secs();
            // The capture observer records the write's own result, so a save that failed - even
            // one that left a partial or empty file behind - is this shot's failure, not a
            // settled shot. `None` means the screenshot has not come back yet.
            if let Some(Err(reason)) = &run.capture {
                let message = format!("shot \"{}\": {reason}", shot.name);
                run.fail(message);
                run.advance();
            } else if matches!(run.capture, Some(Ok(()))) {
                let path = run.shot_path(shot);
                let line = run.log_line(shot, &path);
                run.note(line);
                if run.transition_capture {
                    run.transition_capture = false;
                    run.start_settle();
                } else {
                    run.advance();
                }
            } else if run.timer >= CAPTURE_TIMEOUT_SECONDS {
                let message = format!(
                    "shot \"{}\": no screenshot reached {} within {CAPTURE_TIMEOUT_SECONDS:.0} s",
                    shot.name,
                    run.shot_path(shot).display()
                );
                run.fail(message);
                run.advance();
            }
        }
        Phase::Done => {}
    }
}

/// A serialized runner has one primary-window request in flight. Keep the ticket on that
/// entity until its readback arrives; callback arrival time is deliberately not its frame ID.
fn request_capture(commands: &mut Commands, run: &mut ShotsRun, request_frame: u32) {
    let shot = &run.file.shots[run.shot];
    let path = run.shot_path(shot);
    if let Err(reason) = discard_previous_shot(&path) {
        run.fail(reason);
        run.advance();
        return;
    }
    let ticket = crate::capture_frame::CaptureTicket::default();
    let observed = ticket.clone();
    let position = shot.position;
    let rotation = shot_camera_rotation(shot.yaw, shot.pitch).to_array();
    let fov = vertical_fov_degrees(shot.hfov, run.file.aspect()).to_radians();
    let index = run.shot;
    let transition = run.transition_capture;
    let dimensions = (run.file.width, run.file.height);
    let counts = run.counts;
    run.capture = None;
    commands
        .spawn((Screenshot::primary_window(), ticket))
        .observe(move |trigger: On<ScreenshotCaptured>, mut run: ResMut<ShotsRun>| {
            if run.shot != index
                || run.transition_capture != transition
                || run.phase != Phase::Capture
            {
                return;
            }
            let result = crate::capture_frame::validate_receipt(&observed, position)
                .and_then(|receipt| {
                    if receipt.main_frame < request_frame
                        || receipt.rotation_runtime_xyzw != rotation
                        || receipt.vertical_fov_radians != Some(fov)
                    {
                        return Err(
                            "captured frame precedes request or differs in camera orientation/projection"
                                .into(),
                        );
                    }
                    let size = trigger.image.texture_descriptor.size;
                    if (size.width, size.height) != dimensions {
                        return Err("captured image dimensions differ from requested dimensions".into());
                    }
                    if !transition && !run.timed_out && !receipt.work.is_quiet() {
                        return Err("captured settled frame has pending or unavailable work".into());
                    }
                    write_shot(trigger.image.clone(), &path)?;
                    let line = serde_json::json!({
                        "capture": index,
                        "kind": if transition { "transition" } else { "settled" },
                        "requested_main_frame": request_frame,
                        "requested_camera_creation": position,
                        "requested_rotation_runtime_xyzw": rotation,
                        "requested_vertical_fov_radians": fov,
                        "captured_frame": receipt,
                        "settle_counts": counts,
                        "timed_out": run.timed_out,
                        "image": path,
                        "dimensions": dimensions
                    });
                    run.note(line.to_string());
                    if run.log_failed {
                        return Err("capture receipt could not be saved".into());
                    }
                    Ok(())
                });
            run.capture = Some(result);
        });
    run.enter(Phase::Capture);
}

/// Writes one captured image to `path`, or says why the image is not there to be compared.
///
/// The run writes its screenshots itself rather than through Bevy's `save_to_disk`, which logs its
/// error and drops it: a save that dies after creating the file - a full disk, an encoding error -
/// would otherwise be logged as a settled shot and the run would exit 0. The write is done here so
/// the shot is judged on the `Result` itself. The file is RGB, not RGBA, exactly as Bevy 0.19.0's
/// `save_to_disk` writes it (`bevy_render` `view/window/screenshot.rs:143`: `dyn_img.to_rgb8()`,
/// "discard the alpha channel which stores brightness values when HDR is enabled"), so the PNGs
/// match what the screenshot helper would have produced. The file is then checked to be a
/// complete, non-empty PNG.
///
/// `image` is taken by value because `Image::try_into_dynamic` consumes it (bevy_image 0.19.0,
/// `image_texture_conversion.rs:168`); the observer only has a borrow of the event, so it clones,
/// as Bevy's own `save_to_disk` does (`screenshot.rs:137`).
fn write_shot(image: Image, path: &Path) -> Result<(), String> {
    let dynamic = image
        .try_into_dynamic()
        .map_err(|error| format!("the captured image could not be understood: {error}"))?;
    // Written to a temporary file next to the target and renamed only once it is a complete PNG,
    // so a failed or partial save never leaves a file at the shot's path.
    let partial = partial_path(path);
    let written = dynamic
        .to_rgb8()
        .save(&partial)
        .map_err(|error| format!("the image could not be written: {error}"))
        .and_then(|()| {
            if png_is_complete(&partial) {
                Ok(())
            } else {
                Err(format!("{} is not a complete PNG", partial.display()))
            }
        })
        .and_then(|()| {
            fs::rename(&partial, path).map_err(|error| {
                format!(
                    "the image could not be moved to {}: {error}",
                    path.display()
                )
            })
        });
    if written.is_err() {
        let _ = fs::remove_file(&partial);
    }
    written
}

/// The temporary file a shot's PNG is written to before it is renamed into place: `<name>.partial..png`.
/// It keeps the `.png` extension, which is how the encoder picks the format, and its stem
/// (`<name>.partial.`) ends in a dot, which [`Shot::validate`] refuses in a shot's name. So no other
/// shot's image can have this path, and writing one shot never touches another shot's file.
fn partial_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}.partial..png"))
}

/// Whether a file is a whole PNG stream: the signature at its head and the end-of-image chunk at
/// its tail. A save that died part way leaves one or the other out (or an empty file), which is
/// the failure the file's existence alone cannot see.
fn png_is_complete(path: &Path) -> bool {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    const END: [u8; 12] = [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82];
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 8];
    if file.read_exact(&mut head).is_err() || head != SIGNATURE {
        return false;
    }
    if file.seek(SeekFrom::End(-(END.len() as i64))).is_err() {
        return false;
    }
    let mut tail = [0u8; 12];
    file.read_exact(&mut tail).is_ok() && tail == END
}

/// Poses the camera on the shot: its position in Creation coordinates, its heading and pitch, and
/// the field of view the shot's horizontal angle is at this frame's aspect.
fn place_camera(
    shot: &Shot,
    aspect: f32,
    origin: IVec2,
    transform: &mut Transform,
    projection: &mut Projection,
) {
    transform.translation = shot_camera_translation(shot.position, origin);
    transform.rotation = shot_camera_rotation(shot.yaw, shot.pitch);
    if let Projection::Perspective(perspective) = projection {
        perspective.fov = vertical_fov_degrees(shot.hfov, aspect).to_radians();
        perspective.aspect_ratio = aspect;
    }
}

/// The primary window's size in physical pixels, when there is a window: the size of every
/// screenshot taken of it, which is what the file's frame is supposed to be.
fn window_physical_size(windows: &Query<&Window, With<PrimaryWindow>>) -> Option<(u32, u32)> {
    let window = windows.single().ok()?;
    Some((
        window.resolution.physical_width(),
        window.resolution.physical_height(),
    ))
}

/// Says once whether the window is the size the file's images are supposed to be.
fn warn_window_size(run: &ShotsRun, windows: &Query<&Window, With<PrimaryWindow>>) {
    let Some((width, height)) = window_physical_size(windows) else {
        return;
    };
    if (width, height) == (run.file.width, run.file.height) {
        return;
    }
    warn!(
        target: "shots",
        "the window is {width}x{height}, not the {}x{} the file asks for: the image will not match \
         the reference's frame",
        run.file.width,
        run.file.height
    );
}

/// Removes a PNG an earlier run left at this shot's path, so that the file appearing is proof that
/// this screenshot reached the disk. A file that cannot be removed (on Windows, one open in an
/// image viewer) is an error: it would otherwise be logged as this shot.
fn discard_previous_shot(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not remove the previous {}: {error}",
            path.display()
        )),
    }
}

/// Ends `shots.log`: the lines were appended as the shots ended, so this only makes sure a file of
/// shots leaves a log at all (a run whose every line failed to write reports the error) and says
/// where it is. A run killed part way through never reaches this, and keeps the lines written.
fn finish_log(run: &mut ShotsRun) {
    let path = run.log_path();
    if run.log_failed {
        return;
    }
    let opened = fs::create_dir_all(&run.output_dir).and_then(|()| {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map(|_| ())
    });
    if let Err(error) = opened {
        error!(target: "shots", "could not write {}: {error}", path.display());
        run.failed = true;
    } else {
        run.note(serde_json::json!({"run_complete": !run.failed, "poses_finished": run.shot, "expected_poses": run.file.shots.len(), "expected_captures": run.route_checkpoints.as_ref().map_or(run.file.shots.len(), |checkpoints| checkpoints.len() * 2)}).to_string());
        info!(target: "shots", "shots log written to {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        asset::RenderAssetUsages,
        render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    };

    /// A shots file with fields this engine ignores - a long `note` and a comparison tool's own
    /// annotation - and both kinds of shot in one file.
    const REFERENCE_FILE: &str = r#"{
     "width": 1400,
     "height": 1050,
     "shots": [
      {
       "name": "tower-east-0",
       "worldspace_id": 60,
       "interior_cell_id": null,
       "position": [74950, 78431, -5050],
       "yaw": 270.0,
       "pitch": 2.0,
       "hfov": 75.0,
       "reference": "reference/ruined-tower.jpg",
       "note": "Tamriel, the small Dwemer tower east of the ravine",
       "confidence": "matched"
      },
      {
       "name": "y-05f",
       "worldspace_id": null,
       "interior_cell_id": 355355,
       "position": [7900.0, -4450.0, -4500.0],
       "yaw": 270.0,
       "pitch": 80.0,
       "hfov": 75.0
      }
     ]
    }"#;

    fn reference_file() -> ShotsFile {
        ShotsFile::parse(REFERENCE_FILE).expect("the reference shape parses")
    }

    /// A one-shot file with `shot` spliced in as the shot's fields.
    fn one_shot(shot: &str) -> String {
        format!(r#"{{"width": 100, "height": 100, "shots": [{{{shot}}}]}}"#)
    }

    /// Where a camera posed this way looks: a camera faces its own -Z.
    fn forward(rotation: Quat) -> Vec3 {
        rotation * Vec3::NEG_Z
    }

    fn assert_close(actual: Vec3, expected: Vec3) {
        let difference = (actual - expected).abs().max_element();
        assert!(difference < 1.0e-5, "{actual:?} != {expected:?}");
    }

    #[test]
    fn a_yaw_turns_clockwise_from_north() {
        // Creation axes as the runtime sees them: north is -Z, east is +X (shared::coordinates).
        let cases = [
            (0.0, Vec3::new(0.0, 0.0, -1.0)),
            (90.0, Vec3::new(1.0, 0.0, 0.0)),
            (180.0, Vec3::new(0.0, 0.0, 1.0)),
            (270.0, Vec3::new(-1.0, 0.0, 0.0)),
        ];
        for (yaw, expected) in cases {
            assert_close(forward(shot_camera_rotation(yaw, 0.0)), expected);
        }
        // The heading is the converted Creation direction it names, `(sin yaw, cos yaw, 0)`.
        for yaw in [12.0_f32, 137.0, 250.0, 355.0] {
            let radians = yaw.to_radians();
            let axes = [radians.sin(), radians.cos(), 0.0];
            let direction = shared::coordinates::creation_to_runtime_vector(axes);
            let rotation = shot_camera_rotation(yaw, 0.0);
            assert_close(forward(rotation), Vec3::from_array(direction).normalize());
        }
    }

    #[test]
    fn a_positive_pitch_looks_down() {
        for pitch in [5.0_f32, 30.0, 80.0] {
            let radians = pitch.to_radians();
            let expected = Vec3::new(0.0, -radians.sin(), -radians.cos());
            assert_close(forward(shot_camera_rotation(0.0, pitch)), expected);
        }
        assert!(forward(shot_camera_rotation(0.0, -30.0)).y > 0.0);
        // Yaw and pitch together: a heading of 90 (east) pitched 30 down looks east and down.
        let pitched = 30.0_f32.to_radians();
        let expected = Vec3::new(pitched.cos(), -pitched.sin(), 0.0);
        assert_close(forward(shot_camera_rotation(90.0, 30.0)), expected);
    }

    #[test]
    fn the_vertical_field_of_view_follows_the_aspect() {
        assert!((vertical_fov_degrees(75.0, 1.0) - 75.0).abs() < 1.0e-4);
        let vertical = vertical_fov_degrees(75.0, 4.0 / 3.0);
        assert!(
            (vertical - 59.83).abs() < 0.05,
            "75 degrees at 4:3 is {vertical}"
        );
        assert!(vertical_fov_degrees(75.0, 2.0) < vertical);
        assert!(vertical_fov_degrees(75.0, 0.75) > vertical);
        assert!((vertical_fov_degrees(90.0, 2.0) - 53.130_1).abs() < 1.0e-3);
    }

    #[test]
    fn a_pose_lands_where_the_streamer_measures_it() {
        // Creation (x, y, z) is runtime (x, z, -y), and the streamer reads a camera back as
        // `translation + origin * CELL`.
        let position = [74_950.0, 78_431.0, -5_050.0];
        for (x, y) in [(0, 0), (18, 19), (-4, 7)] {
            let translation = shot_camera_translation(position, IVec2::new(x, y));
            let world_x = translation.x + x as f32 * CELL_SIZE;
            let world_y = -translation.z + y as f32 * CELL_SIZE;
            assert!((world_x - position[0]).abs() < 1.0e-2);
            assert!((world_y - position[1]).abs() < 1.0e-2);
            assert!((translation.y - position[2]).abs() < 1.0e-2);
            // The streamer's own reading of that camera is the shot's grid square.
            let center = crate::streaming::streaming_center(translation, IVec2::new(x, y));
            let grid = Shot {
                position,
                ..reference_file().shots[0].clone()
            }
            .grid();
            assert_eq!((center.x, center.y), grid);
        }
    }

    #[test]
    fn the_reference_shape_parses_and_names_where_the_run_starts() {
        let file = reference_file();
        assert_eq!((file.width, file.height), (1400, 1050));
        assert_eq!(file.shots.len(), 2);

        let exterior = &file.shots[0];
        assert_eq!(exterior.worldspace_id, Some(60));
        assert!(exterior.is_exterior());
        assert_eq!(exterior.grid(), (18, 19));
        let expected = "reference/ruined-tower.jpg";
        assert_eq!(exterior.reference.as_deref(), Some(expected));
        assert!(!file.shots[1].is_exterior());

        // The run starts on the first exterior shot, whatever order the kinds come in.
        assert_eq!(file.start(), Some((Some(60), (18, 19))));
        let interiors_only = ShotsFile {
            shots: vec![file.shots[1].clone()],
            ..file.clone()
        };
        assert_eq!(interiors_only.start(), None);

        // A negative position floors into the square below zero.
        let west = Shot {
            position: [-1.0, -4097.0, 0.0],
            ..exterior.clone()
        };
        assert_eq!(west.grid(), (-1, -2));

        // The window is exactly the file's frame, whatever the display's scale factor.
        let resolution = file.window_resolution();
        let physical = (resolution.physical_width(), resolution.physical_height());
        assert_eq!(physical, (1400, 1050));
    }

    #[test]
    fn a_malformed_file_is_an_error_that_says_what_is_wrong() {
        let good = r#""name": "x", "position": [0, 0, 0], "yaw": 0, "pitch": 0, "hfov": 75"#;
        let broken = [
            (String::new(), "not a shots file"),
            ("not json".to_owned(), "not a shots file"),
            ("[]".to_owned(), "not a shots file"),
            (r#"{"width": 100, "height": 100}"#.to_owned(), "shots"),
            (
                r#"{"width": -1, "height": 100, "shots": []}"#.to_owned(),
                "not a shots file",
            ),
            (
                r#"{"width": 0, "height": 10, "shots": []}"#.to_owned(),
                "frame",
            ),
            (
                r#"{"width": 8193, "height": 100, "shots": []}"#.to_owned(),
                "8192",
            ),
            (
                r#"{"width": 100, "height": 9000, "shots": []}"#.to_owned(),
                "8192",
            ),
            (
                r#"{"width": 10, "height": 10, "shots": []}"#.to_owned(),
                "no shots",
            ),
            (
                one_shot(r#""name": "x", "position": [0, 0], "yaw": 0, "pitch": 0, "hfov": 75"#),
                "not a shots file",
            ),
            (
                one_shot(r#""name": "x", "position": [0, 0, 0], "pitch": 0, "hfov": 75"#),
                "yaw",
            ),
            (
                one_shot(
                    r#""name": "x", "position": [0, 0, 0], "yaw": "north", "pitch": 0, "hfov": 75"#,
                ),
                "not a shots file",
            ),
            (
                one_shot(
                    r#""name": "x", "position": [0, 0, 0], "yaw": 0, "pitch": 0, "hfov": 180"#,
                ),
                "field of view",
            ),
            (
                one_shot(
                    r#""name": "x", "position": [0, 0, 0], "yaw": 0, "pitch": 0, "hfov": 1e39"#,
                ),
                "not a number",
            ),
            (
                one_shot(
                    r#""name": "x", "position": [0, 0, 0], "yaw": 0, "pitch": 91, "hfov": 75"#,
                ),
                "pitch",
            ),
            (
                one_shot(
                    r#""name": "x", "position": [0, 0, 0], "yaw": 0, "pitch": -90.5, "hfov": 75"#,
                ),
                "pitch",
            ),
            (
                one_shot(
                    r#""name": "x", "position": [0, 0, 0], "yaw": 0, "pitch": 1e9, "hfov": 75"#,
                ),
                "pitch",
            ),
            (
                one_shot(r#""name": "", "position": [0, 0, 0], "yaw": 0, "pitch": 0, "hfov": 75"#),
                "no name",
            ),
            (
                one_shot(
                    r#""name": "../escape", "position": [0, 0, 0], "yaw": 0, "pitch": 0, "hfov": 75"#,
                ),
                "plain file name",
            ),
            (
                format!(r#"{{"width": 10, "height": 10, "shots": [{{{good}}}, {{{good}}}]}}"#),
                "two shots are named",
            ),
            (
                format!(
                    r#"{{"width": 10, "height": 10, "shots": [{{{good}}}, {{{}}}]}}"#,
                    good.replace(r#""name": "x""#, r#""name": "X""#)
                ),
                "ignoring case",
            ),
        ];
        for (text, expected) in broken {
            let error = ShotsFile::parse(&text).expect_err("this is not a shots file");
            let message = error.to_string();
            assert!(
                message.contains(expected),
                "{expected:?} is missing from {message:?}"
            );
        }
        assert!(ShotsFile::parse(&one_shot(good)).is_ok());
    }

    #[test]
    fn names_windows_cannot_create_are_refused_before_the_window_opens() {
        let pose = r#""position": [0, 0, 0], "yaw": 0, "pitch": 0, "hfov": 75"#;
        let refused = [
            ("a<b", "plain file name"),
            ("a>b", "plain file name"),
            ("a\"b", "plain file name"),
            ("a|b", "plain file name"),
            ("a?b", "plain file name"),
            ("a*b", "plain file name"),
            ("trailing.", "ends in a dot"),
            ("trailing ", "ends in a dot"),
            ("CON", "reserved device name"),
            ("con", "reserved device name"),
            ("con.png", "reserved device name"),
            ("Nul.x", "reserved device name"),
            ("COM1", "reserved device name"),
            ("lpt9.png", "reserved device name"),
        ];
        for (name, expected) in refused {
            let quoted = serde_json::to_string(name).unwrap();
            let text = one_shot(&format!("\"name\": {quoted}, {pose}"));
            let error = ShotsFile::parse(&text).expect_err("Windows cannot create this name");
            let message = error.to_string();
            assert!(message.contains(expected), "{name:?} is missing: {message}");
        }
        // Names that only look like a device name, or carry an extension, are ordinary files.
        for name in ["console", "com0", "lpt10", "auxiliary", "tower.east", "a b"] {
            let quoted = serde_json::to_string(name).unwrap();
            let text = one_shot(&format!("\"name\": {quoted}, {pose}"));
            assert!(ShotsFile::parse(&text).is_ok(), "{name:?} is a file name");
        }
    }

    #[test]
    fn a_frame_at_the_size_cap_is_accepted() {
        let shot = r#""name": "cap", "position": [0, 0, 0], "yaw": 0, "pitch": 0, "hfov": 75"#;
        let text = format!(
            r#"{{"width": {MAX_FRAME_SIDE_PIXELS}, "height": {MAX_FRAME_SIDE_PIXELS}, "shots": [{{{shot}}}]}}"#
        );
        let file = ShotsFile::parse(&text).expect("the cap itself is a frame the window can be");
        assert_eq!(
            (file.width, file.height),
            (MAX_FRAME_SIDE_PIXELS, MAX_FRAME_SIDE_PIXELS)
        );
    }

    #[test]
    fn each_log_line_reaches_the_file_as_the_shot_ends() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("out");
        fs::create_dir_all(&output).unwrap();
        let path = output.join("shots.log");
        // What an earlier run left there is replaced by this run's first line, not appended to.
        fs::write(&path, "stale from an earlier run\n").unwrap();
        let mut run = ShotsRun::new(reference_file(), output, 60);

        run.note("tower-east-0 frames=10 settled path=out/tower-east-0.png");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "tower-east-0 frames=10 settled path=out/tower-east-0.png\n"
        );
        // A failure's line is in the file the moment it happens: a killed run keeps it.
        run.fail("shot \"y-05f\": no screenshot reached the disk");
        assert!(run.failed);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "tower-east-0 frames=10 settled path=out/tower-east-0.png\n\
             FAILED: shot \"y-05f\": no screenshot reached the disk\n"
        );
    }

    #[test]
    fn a_previous_shot_that_cannot_be_removed_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            discard_previous_shot(&directory.path().join("missing.png")),
            Ok(())
        );
        let stale = directory.path().join("stale.png");
        fs::write(&stale, b"old").unwrap();
        assert_eq!(discard_previous_shot(&stale), Ok(()));
        assert!(!stale.exists());
        // A folder where the image goes cannot be removed as a file, like a PNG a viewer holds.
        let blocked = directory.path().join("blocked.png");
        fs::create_dir(&blocked).unwrap();
        let message = discard_previous_shot(&blocked).expect_err("a folder is not removed");
        assert!(message.contains("blocked.png"), "{message}");
    }

    #[test]
    fn a_file_that_cannot_be_read_names_its_path() {
        let path = Path::new("no-such-folder/no-such-shots.json");
        let message = ShotsFile::load(path).expect_err("missing").to_string();
        assert!(message.contains("no-such-shots.json"), "{message}");
    }

    #[test]
    fn the_output_directory_defaults_to_the_file_stem_shots() {
        let cases = [
            (
                "reference/riverwood_shots.json",
                "reference/riverwood_shots-shots",
            ),
            ("shots.json", "shots-shots"),
            ("poses/east/tower_east.json", "poses/east/tower_east-shots"),
        ];
        for (path, expected) in cases {
            assert_eq!(default_output_dir(Path::new(path)), PathBuf::from(expected));
        }
    }

    #[test]
    fn interiors_and_other_worldspaces_are_skipped_with_a_reason() {
        let file = reference_file();
        let exterior = &file.shots[0];
        assert_eq!(skip_reason(exterior, 60), None);
        assert_eq!(skip_reason(exterior, 0x3c), None, "0x3c is 60");
        let other = skip_reason(exterior, 0x0001_EE62).expect("another worldspace is skipped");
        assert!(other.contains("0000003C"), "{other}");
        assert_eq!(skip_reason(&file.shots[1], 60).as_deref(), Some("interior"));
        // A shot that names no worldspace takes the one being streamed.
        let unnamed = Shot {
            worldspace_id: None,
            ..exterior.clone()
        };
        assert_eq!(skip_reason(&unnamed, 0x0001_EE62), None);
    }

    #[test]
    fn the_settle_rule_needs_a_quiet_window_and_every_count_at_zero() {
        let ready = SettleCounts {
            renderer_ready: true,
            pending_specializations: Some(0),
            pending_render_transfers: Some(0),
            ..SettleCounts::default()
        };
        assert!(
            !shots_settled(&ready),
            "a window of no frames is not quiet yet"
        );
        let quiet = SettleCounts {
            quiet_frames: SETTLE_QUIET_FRAMES,
            ..ready
        };
        assert!(shots_settled(&quiet));
        let short = SettleCounts {
            quiet_frames: SETTLE_QUIET_FRAMES - 1,
            ..ready
        };
        assert!(!shots_settled(&short));

        // Each kind of pending work on its own holds the shot back and starts the window again.
        let pending = [
            SettleCounts {
                loading_cells: 1,
                ..ready
            },
            SettleCounts {
                active_requests: 1,
                ..ready
            },
            SettleCounts {
                pending_asset_instances: 3,
                ..ready
            },
            SettleCounts {
                pending_surface_instances: 1,
                ..ready
            },
            SettleCounts {
                arming_queue_depth: 2,
                ..ready
            },
            SettleCounts {
                retiring_cells: 1,
                ..ready
            },
            SettleCounts {
                renderer_ready: false,
                ..ready
            },
            // A failure is not pending work that will clear: the view is held back and the
            // timeout gives up on the shot, rather than the shot being logged as settled.
            SettleCounts {
                failed_cells: 1,
                ..ready
            },
            SettleCounts {
                asset_load_failures: 1,
                ..ready
            },
            SettleCounts {
                validation_failures: 1,
                ..ready
            },
        ];
        for counts in pending {
            assert!(!counts.is_quiet(), "{counts:?}");
            let counted = advance_settle(SettleCounts {
                quiet_frames: SETTLE_QUIET_FRAMES,
                ..counts
            });
            assert_eq!(counted.quiet_frames, 0, "the window starts again");
            assert!(!shots_settled(&counted));
        }

        // The window counts up while quiet, and reaches a settled view after ten frames.
        let mut counts = ready;
        for frame in 1..=SETTLE_QUIET_FRAMES {
            counts = advance_settle(counts);
            assert_eq!(counts.quiet_frames, frame);
            assert_eq!(shots_settled(&counts), frame >= SETTLE_QUIET_FRAMES);
        }

        // The streamer's and renderer's metrics are read field by field, failures included.
        let metrics = StreamingMetrics {
            loading_cells: 1,
            active_requests: 2,
            pending_asset_instances: 3,
            pending_surface_instances: 4,
            arming_queue_depth: 5,
            retiring_cells: 6,
            failed_cells: 1,
            asset_load_failures: 2,
            material_validation_failures: 1,
            terrain_validation_failures: 1,
            water_validation_failures: 1,
            transform_bounds_validation_failures: 1,
            ..StreamingMetrics::default()
        };
        let renderer = RendererMetrics {
            renderer_validation_failures: 1,
            ..RendererMetrics::default()
        };
        let read = SettleCounts::read(&metrics, &renderer, Failures::default(), true, 7);
        let expected = SettleCounts {
            loading_cells: 1,
            active_requests: 2,
            pending_asset_instances: 3,
            pending_surface_instances: 4,
            arming_queue_depth: 5,
            retiring_cells: 6,
            failed_cells: 1,
            asset_load_failures: 2,
            validation_failures: 5,
            renderer_ready: true,
            quiet_frames: 7,
            ..default()
        };
        assert_eq!(read, expected);
        assert!(!read.is_quiet(), "a failed cell is not a settled view");

        // A failure that was already counted when the shot started settling is history, not a
        // reason to hold the shot back; one that happens during the settle is.
        let baseline = Failures::read(&metrics, &renderer);
        let quiet = SettleCounts::read(&metrics, &renderer, baseline, true, 0);
        assert_eq!(
            (
                quiet.failed_cells,
                quiet.asset_load_failures,
                quiet.validation_failures
            ),
            (0, 0, 0)
        );
        let idle = StreamingMetrics {
            failed_cells: metrics.failed_cells,
            asset_load_failures: metrics.asset_load_failures,
            material_validation_failures: metrics.material_validation_failures,
            terrain_validation_failures: metrics.terrain_validation_failures,
            water_validation_failures: metrics.water_validation_failures,
            transform_bounds_validation_failures: metrics.transform_bounds_validation_failures,
            ..StreamingMetrics::default()
        };
        let mut idle_counts = SettleCounts::read(&idle, &renderer, baseline, true, 0);
        idle_counts.pending_specializations = Some(0);
        idle_counts.pending_render_transfers = Some(0);
        assert!(idle_counts.is_quiet());
        let later = StreamingMetrics {
            failed_cells: metrics.failed_cells + 1,
            ..idle
        };
        let fresh = SettleCounts::read(&later, &renderer, baseline, true, 0);
        assert_eq!(fresh.failed_cells, 1);
        assert!(!fresh.is_quiet());
    }

    #[test]
    fn settle_requires_lod_upload_and_render_retry_acknowledgments() {
        let ready = SettleCounts {
            renderer_ready: true,
            pending_specializations: Some(0),
            pending_render_transfers: Some(0),
            quiet_frames: 10,
            ..default()
        };
        assert!(shots_settled(&ready));
        for counts in [
            SettleCounts {
                pending_lod_queries: 1,
                ..ready
            },
            SettleCounts {
                pending_lod_chunks: 1,
                ..ready
            },
            SettleCounts {
                outstanding_terrain_uploads: 1,
                ..ready
            },
            SettleCounts {
                pending_batch_cpu: 1,
                ..ready
            },
            SettleCounts {
                pending_batch_initial: 1,
                ..ready
            },
            SettleCounts {
                pending_batch_selection: 1,
                ..ready
            },
            SettleCounts {
                pending_specializations: Some(1),
                ..ready
            },
            SettleCounts {
                pending_specializations: None,
                ..ready
            },
            SettleCounts {
                pending_render_transfers: Some(1),
                ..ready
            },
            SettleCounts {
                pending_render_transfers: None,
                ..ready
            },
        ] {
            assert!(!shots_settled(&counts));
            assert_eq!(advance_settle(counts).quiet_frames, 0);
        }
    }

    #[test]
    fn route_freezes_capture_pose_and_executes_every_step_after_rebase() {
        let (mut file, _) = crate::matched_route::MatchedRoute::parse_for_test(include_str!(
            "../../../scripts/profiling/fixtures/matched-route.json"
        ));
        file.shots.truncate(3);
        let output = tempfile::tempdir().unwrap();
        let mut run = ShotsRun::new(file, output.path().into(), 60);
        run.route_checkpoints = Some(BTreeSet::from([0, 2]));
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .init_resource::<StreamingMetrics>()
            .init_resource::<RendererMetrics>()
            .init_resource::<crate::profiling::ProfilingState>()
            .init_resource::<crate::terrain_upload::TerrainMeshUploadReadiness>()
            .init_resource::<crate::capture_frame::RetryReadiness>()
            .init_resource::<crate::capture_frame::TransferReadiness>()
            .add_plugins(ShotsPlugin { run });
        let camera = app
            .world_mut()
            .spawn((
                Transform::default(),
                Projection::Perspective(default()),
                StreamingCamera,
            ))
            .id();
        app.update();
        assert_eq!(app.world().resource::<ShotsRun>().phase, Phase::Capture);
        assert!(app.world().resource::<ShotsRun>().transition_capture);
        // Simulate a rebase and a delayed image while the capture is in flight.
        app.world_mut().resource_mut::<RenderOrigin>().0 = IVec2::new(5, -11);
        app.update();
        assert_eq!(
            app.world().get::<Transform>(camera).unwrap().translation,
            Vec3::new(2048.0, 6000.0, 2048.0)
        );
        assert_eq!(app.world().resource::<ShotsRun>().shot, 0);
        app.world_mut().resource_mut::<ShotsRun>().capture = Some(Ok(()));
        app.update();
        assert_eq!(app.world().resource::<ShotsRun>().phase, Phase::Settle);
        assert_eq!(app.world().resource::<ShotsRun>().shot, 0);
        // A completed settled capture is the only path that releases this checkpoint.
        {
            let mut run = app.world_mut().resource_mut::<ShotsRun>();
            run.enter(Phase::Capture);
            run.capture = Some(Ok(()));
        }
        app.update();
        app.update();
        assert_eq!(app.world().resource::<ShotsRun>().phase, Phase::Traverse);
        assert_eq!(
            app.world().get::<Transform>(camera).unwrap().translation.x,
            2112.0
        );
        app.update();
        app.update();
        assert_eq!(app.world().resource::<ShotsRun>().shot, 2);
        assert_eq!(
            app.world().get::<Transform>(camera).unwrap().translation.x,
            2176.0
        );
        assert!(app.world().resource::<ShotsRun>().transition_capture);
        app.world_mut().resource_mut::<ShotsRun>().timer = CAPTURE_TIMEOUT_SECONDS;
        app.update();
        assert!(app.world().resource::<ShotsRun>().failed);
        assert_eq!(app.world().resource::<ShotsRun>().phase, Phase::Done);
        assert!(!output.path().join("route-000002-transition.png").exists());
    }

    /// An image the size a shot's capture arrives at, so the writer can be exercised without a
    /// window or a GPU.
    fn test_image() -> Image {
        Image::new(
            Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            vec![0x40; 2 * 2 * 4],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::MAIN_WORLD,
        )
    }

    #[test]
    fn a_captured_image_is_written_and_a_write_that_fails_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shot.png");
        write_shot(test_image(), &path).expect("the shot's PNG is written");
        assert!(path.is_file(), "the PNG is on disk");
        assert!(png_is_complete(&path), "the PNG is a whole stream");
        assert!(
            !partial_path(&path).exists(),
            "the temporary file was renamed into place"
        );

        // A shot named `a.partial` beside one named `a`: writing `a` must not touch the other
        // shot's image, so the temporary name can never be a shot's own file name.
        let other_shot = directory.path().join("a.partial.png");
        write_shot(test_image(), &other_shot).expect("the a.partial shot is written");
        let before = fs::read(&other_shot).unwrap();
        write_shot(test_image(), &directory.path().join("a.png")).expect("the a shot is written");
        assert_eq!(
            fs::read(&other_shot).expect("a.partial's image is still there"),
            before,
            "writing shot a left shot a.partial's image alone"
        );
        let stem = partial_path(&directory.path().join("a.png"))
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let pose = r#""position": [0, 0, 0], "yaw": 0, "pitch": 0, "hfov": 75"#;
        let quoted = serde_json::to_string(&stem).unwrap();
        assert!(
            ShotsFile::parse(&one_shot(&format!("\"name\": {quoted}, {pose}"))).is_err(),
            "the temporary stem {stem:?} must be a name the shots file refuses"
        );

        // A directory that is not there: the save's own error is the shot's failure - Bevy's
        // `save_to_disk` would have logged it and left the shot looking settled.
        let missing = directory.path().join("no-such-folder").join("shot.png");
        let message = write_shot(test_image(), &missing).expect_err("nothing was written");
        assert!(message.contains("could not be written"), "{message}");
        assert!(
            !missing.exists(),
            "a failed save leaves nothing at the shot's path"
        );

        // A file that is there but is not a whole PNG - the 0-byte or truncated file a failed
        // save leaves - is not a shot.
        let empty = directory.path().join("empty.png");
        fs::write(&empty, b"").unwrap();
        assert!(!png_is_complete(&empty));
        let truncated = directory.path().join("truncated.png");
        fs::write(&truncated, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(!png_is_complete(&truncated));
        let other = directory.path().join("other.png");
        fs::write(
            &other,
            b"not a png at all, but long enough to have a head and a tail",
        )
        .unwrap();
        assert!(!png_is_complete(&other));
    }

    #[test]
    fn a_log_line_says_what_the_shot_did() {
        let file = reference_file();
        let shot = file.shots[0].clone();
        let output = PathBuf::from("reference/riverwood_shots-shots");
        let mut run = ShotsRun::new(file, output, 60);
        let path = run.shot_path(&shot);
        let display = path.display();

        run.frames = 42;
        let expected = format!("tower-east-0 frames=42 settled path={display}");
        assert_eq!(run.log_line(&shot, &path), expected);

        run.frames = 1_812;
        run.timed_out = true;
        run.counts = SettleCounts {
            loading_cells: 2,
            pending_asset_instances: 7,
            ..SettleCounts::default()
        };
        let timed_out = run.log_line(&shot, &path);
        assert!(timed_out.starts_with("tower-east-0 frames=1812 timed_out ("));
        assert!(timed_out.contains("loading_cells=2"));
        assert!(timed_out.contains("pending_assets=7"));
        assert!(timed_out.ends_with(&format!("path={display}")));
        assert_eq!(
            timed_out.lines().count(),
            1,
            "one line per shot: {timed_out}"
        );

        // The window's size is on the line when it is not the file's frame - the image is then not
        // the shape the reference is - and absent when it is.
        run.timed_out = false;
        run.window_size = Some((1400, 1050));
        let same = run.log_line(&shot, &path);
        assert!(!same.contains("window="), "the file's frame needs no note");
        run.window_size = Some((1920, 1080));
        let resized = run.log_line(&shot, &path);
        assert!(resized.ends_with(" window=1920x1080"), "{resized}");
        assert_eq!(resized.lines().count(), 1, "one line per shot: {resized}");
    }
}
