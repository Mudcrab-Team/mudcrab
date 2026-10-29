//! `--route <file>`: a camera that follows a list of waypoints at a fixed speed, so a benchmark can
//! measure travelling instead of standing still or flying in a straight line.
//!
//! A route is a data file (see `scripts/routes/`). The engine knows nothing about any particular
//! route: no coordinates, no place names, no expectations about what lies along the way. The file
//! gives the worldspace, the speed, the eye height and the waypoints in Skyrim world units (`x`
//! east, `y` north).
//!
//! * The camera moves along straight legs between consecutive waypoints at the file's speed, facing
//!   the direction of travel and looking level. A looped route is a closed circuit: its last leg
//!   returns to the first waypoint, so the camera never jumps.
//! * The eye sits a fixed height above the terrain. The terrain height comes from the converted
//!   height field the terrain mesh is built from (`CellCache`), read for the cells along the route
//!   before the run starts, so following the ground costs nothing while measuring. A frame whose
//!   position has no terrain (a cell past the edge of the map) keeps the last height and is counted
//!   in the report as `route_frames_without_ground`.
//! * The route starts when the world is first ready (the pacing predicate, see `pacing`), so the
//!   measured walk is not the cold load of the start cell; until then the camera waits at the first
//!   waypoint. That wait is the only one: once the route has started the runner never waits for
//!   loading, never slows down and hides nothing. A route with `"start": "immediately"` does not
//!   wait at all. Frame times are scored from the first full frame on the route.
//! * The route ends when the last waypoint is reached, and the benchmark window with it, so the
//!   report covers the route and nothing after it. A looped route never ends; the benchmark's own
//!   duration does, counted in real seconds from the start of the process, and so does a route that
//!   never becomes ready or never reaches its end.
//!
//! The distance travelled follows the real clock (`Time<Real>`), not Bevy's virtual clock, which is
//! clamped at 250 ms: a long hitch must cost the camera its distance, as it would a player's.

use crate::{
    config::EngineConfig,
    pacing::PacingTracker,
    streaming::RenderOrigin,
    world::{
        cache::CellCache,
        components::{CELL_SIZE, StreamingCamera},
    },
};
use bevy::prelude::*;
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

/// The default height of the eye above the ground, in units.
pub const DEFAULT_EYE_HEIGHT: f32 = 120.0;
/// The fastest route speed accepted, in units per second: far past anything that streams.
const MAX_SPEED: f32 = 20_000.0;
const MAX_EYE_HEIGHT: f32 = 5_000.0;
/// The largest coordinate accepted. The world is about 250,000 units across.
const MAX_COORDINATE: f32 = 1_000_000.0;
const MAX_WAYPOINTS: usize = 10_000;
/// The shortest leg accepted: two waypoints closer than this are one waypoint.
const MIN_LEG: f32 = 1.0;
/// Samples in a cell's height field along one side.
const TERRAIN_SAMPLES: usize = 33;
/// How far apart the points are that decide which cells' heights are read before the run.
const GROUND_PROBE_SPACING: f32 = 256.0;

/// When the route starts moving.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStart {
    /// On the frame the world is first ready; the camera waits at the first waypoint until then,
    /// and stands there on that frame too, walking from the next one. Without a measuring run (no
    /// pacing readiness to wait for) the route starts at once.
    #[default]
    WorldReady,
    /// On the first frame, which stands still too; the walk begins on the next one.
    Immediately,
}

/// A worldspace as written in a route file: a number, or text in decimal or `0x` hex.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum WorldspaceValue {
    Number(u32),
    Text(String),
}

/// The JSON of a route file. Unknown keys are refused, so a mistyped option is an error and not a
/// default.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteFile {
    #[serde(default)]
    name: String,
    /// What the route is and why its waypoints are where they are. Not read by the engine.
    #[serde(default)]
    #[allow(dead_code)]
    description: String,
    /// Notes on individual waypoints or the route's purpose. Not read by the engine.
    #[serde(default)]
    #[allow(dead_code)]
    notes: Vec<String>,
    worldspace: WorldspaceValue,
    /// Units per second.
    speed: f32,
    #[serde(default = "default_eye_height")]
    eye_height: f32,
    #[serde(rename = "loop", default)]
    looped: bool,
    #[serde(default)]
    start: RouteStart,
    /// `[x, y]` in Skyrim world units, `x` east and `y` north.
    waypoints: Vec<[f32; 2]>,
}

fn default_eye_height() -> f32 {
    DEFAULT_EYE_HEIGHT
}

fn parse_worldspace(value: &WorldspaceValue) -> Result<u32> {
    match value {
        WorldspaceValue::Number(number) => Ok(*number),
        WorldspaceValue::Text(text) => {
            let text = text.trim();
            let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => text.parse().ok(),
            };
            parsed.ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "worldspace {text:?} is not a form id: write a number or \"0x\" and hex digits"
                )
            })
        }
    }
}

/// A validated route.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutePlan {
    /// The file it was read from, as given on the command line (empty for a parsed string).
    pub source: String,
    pub name: String,
    /// The worldspace the file names; the run starts there.
    pub worldspace: u32,
    /// Units per second.
    pub speed: f32,
    pub eye_height: f32,
    pub looped: bool,
    pub start: RouteStart,
    /// The waypoints as written, in Skyrim world units (`x` east, `y` north).
    pub waypoints: Vec<Vec2>,
    /// The polyline the camera follows: the waypoints, plus the first one again at the end of a
    /// looped route.
    points: Vec<Vec2>,
    /// The distance along the route at each point; `cumulative[0]` is zero.
    cumulative: Vec<f32>,
}

/// Where the camera is on a route, and which way it faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoutePose {
    /// Skyrim world units, `x` east and `y` north.
    pub position: Vec2,
    /// The unit direction of the leg the position is on.
    pub heading: Vec2,
    /// The end of a route that does not loop has been reached.
    pub finished: bool,
}

impl RoutePlan {
    /// Reads and validates a route file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("cannot read the route file {}", path.display()))?;
        let mut plan = Self::parse(&text)
            .wrap_err_with(|| format!("the route file {} is not usable", path.display()))?;
        plan.source = path.display().to_string();
        Ok(plan)
    }

    /// Validates the text of a route file.
    pub fn parse(json: &str) -> Result<Self> {
        let file: RouteFile = serde_json::from_str(json).wrap_err("the route is not valid JSON")?;
        Self::from_file(file)
    }

    fn from_file(file: RouteFile) -> Result<Self> {
        let worldspace = parse_worldspace(&file.worldspace)?;
        ensure!(
            file.speed.is_finite() && file.speed > 0.0 && file.speed <= MAX_SPEED,
            "speed must be a number above 0 and up to {MAX_SPEED} units per second, not {}",
            file.speed
        );
        ensure!(
            file.eye_height.is_finite() && (0.0..=MAX_EYE_HEIGHT).contains(&file.eye_height),
            "eye_height must be from 0 to {MAX_EYE_HEIGHT} units, not {}",
            file.eye_height
        );
        ensure!(
            file.waypoints.len() >= 2,
            "a route needs at least two waypoints, this one has {}",
            file.waypoints.len()
        );
        ensure!(
            file.waypoints.len() <= MAX_WAYPOINTS,
            "a route has at most {MAX_WAYPOINTS} waypoints, this one has {}",
            file.waypoints.len()
        );
        let mut waypoints = Vec::with_capacity(file.waypoints.len());
        for (index, [x, y]) in file.waypoints.iter().copied().enumerate() {
            ensure!(
                x.is_finite()
                    && y.is_finite()
                    && x.abs() <= MAX_COORDINATE
                    && y.abs() <= MAX_COORDINATE,
                "waypoint {index} ({x}, {y}) is not within {MAX_COORDINATE} units of the origin"
            );
            waypoints.push(Vec2::new(x, y));
        }
        let mut points = waypoints.clone();
        if file.looped {
            points.push(waypoints[0]);
        }
        let mut cumulative = Vec::with_capacity(points.len());
        cumulative.push(0.0);
        for (index, pair) in points.windows(2).enumerate() {
            let length = pair[0].distance(pair[1]);
            if length < MIN_LEG {
                bail!(
                    "legs must be at least {MIN_LEG} unit long, but the leg from waypoint {index} to the next is {length}"
                );
            }
            cumulative.push(cumulative[index] + length);
        }
        Ok(Self {
            source: String::new(),
            name: file.name,
            worldspace,
            speed: file.speed,
            eye_height: file.eye_height,
            looped: file.looped,
            start: file.start,
            waypoints,
            points,
            cumulative,
        })
    }

    /// The length of one lap (or, for a route that does not loop, of the route), in units.
    pub fn length(&self) -> f32 {
        self.cumulative.last().copied().unwrap_or(0.0)
    }

    /// The grid square of the first waypoint, where the run's streaming starts.
    pub fn start_grid(&self) -> (i32, i32) {
        let start = self.waypoints[0];
        (
            (start.x / CELL_SIZE).floor() as i32,
            (start.y / CELL_SIZE).floor() as i32,
        )
    }

    /// The pose `distance` units along the route. Beyond the end of a route that does not loop it
    /// is the end, `finished`; a looped route wraps around.
    pub fn pose_at(&self, distance: f32) -> RoutePose {
        let total = self.length();
        let (distance, finished) = if self.looped {
            (distance.rem_euclid(total), false)
        } else {
            (distance.clamp(0.0, total), distance >= total)
        };
        let legs = self.points.len() - 1;
        let leg = self
            .cumulative
            .partition_point(|start| *start <= distance)
            .saturating_sub(1)
            .min(legs - 1);
        let from = self.points[leg];
        let to = self.points[leg + 1];
        let along =
            (distance - self.cumulative[leg]) / (self.cumulative[leg + 1] - self.cumulative[leg]);
        RoutePose {
            position: from.lerp(to, along.clamp(0.0, 1.0)),
            heading: (to - from).normalize(),
            finished,
        }
    }
}

/// The terrain heights along a route, read before the run so the route follows the ground without
/// touching the database or the cache while it is measured.
#[derive(Debug, Clone, Default)]
pub struct RouteGround {
    /// Each exterior cell's 33 by 33 height field, by grid square. Row `y` runs south to north.
    cells: HashMap<(i32, i32), Vec<f32>>,
}

impl RouteGround {
    /// A ground made of the given height fields (`TERRAIN_SAMPLES` squared values each).
    pub fn from_cells(cells: impl IntoIterator<Item = ((i32, i32), Vec<f32>)>) -> Self {
        Self {
            cells: cells
                .into_iter()
                .filter(|(_, heights)| heights.len() == TERRAIN_SAMPLES * TERRAIN_SAMPLES)
                .collect(),
        }
    }

    /// How many cells' heights are held.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// The terrain height at a world position, interpolated between the height field's samples, or
    /// `None` where no cell is held: there is no ground to report, not a ground at zero.
    pub fn height_at(&self, position: Vec2) -> Option<f32> {
        let grid = (
            (position.x / CELL_SIZE).floor() as i32,
            (position.y / CELL_SIZE).floor() as i32,
        );
        let heights = self.cells.get(&grid)?;
        let step = CELL_SIZE / (TERRAIN_SAMPLES - 1) as f32;
        let last = (TERRAIN_SAMPLES - 1) as f32;
        let fx = ((position.x - grid.0 as f32 * CELL_SIZE) / step).clamp(0.0, last);
        let fy = ((position.y - grid.1 as f32 * CELL_SIZE) / step).clamp(0.0, last);
        let x0 = (fx.floor() as usize).min(TERRAIN_SAMPLES - 2);
        let y0 = (fy.floor() as usize).min(TERRAIN_SAMPLES - 2);
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let at = |x: usize, y: usize| heights[y * TERRAIN_SAMPLES + x];
        let south = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * tx;
        let north = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * tx;
        Some(south + (north - south) * ty)
    }

    /// Reads the height fields of every cell the route passes through, with the cells around them,
    /// from the converted world. Fails when none of them has terrain, which means the route is not
    /// in the converted data.
    pub fn load(
        plan: &RoutePlan,
        worldspace_id: u32,
        database_path: &Path,
        cache: &CellCache,
    ) -> Result<Self> {
        let connection =
            Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .wrap_err_with(|| format!("failed to open {}", database_path.display()))?;
        let mut cells = HashMap::new();
        for grid in route_cells(plan) {
            let cell_id = connection
                .query_row(
                    crate::world::database::EXTERIOR_CELL_ID_SQL,
                    params![worldspace_id, grid.0, grid.1],
                    |row| row.get::<_, u32>(0),
                )
                .optional()?;
            if let Some(terrain) = cell_id.and_then(|cell_id| cache.terrain(cell_id))
                && usize::from(terrain.width) == TERRAIN_SAMPLES
                && usize::from(terrain.height) == TERRAIN_SAMPLES
            {
                cells.insert(grid, terrain.heights);
            }
        }
        let ground = Self::from_cells(cells);
        ensure!(
            ground.cell_count() > 0,
            "none of the cells along the route has terrain in {}: the route is not in this converted world",
            database_path.display()
        );
        Ok(ground)
    }
}

/// The grid squares whose height fields a route needs: every square a leg passes through, and the
/// eight around each, so a leg that clips a corner is still covered.
fn route_cells(plan: &RoutePlan) -> Vec<(i32, i32)> {
    let mut cells = HashSet::new();
    for pair in plan.points.windows(2) {
        let length = pair[0].distance(pair[1]);
        let steps = (length / GROUND_PROBE_SPACING).ceil().max(1.0) as usize;
        for step in 0..=steps {
            let point = pair[0].lerp(pair[1], step as f32 / steps as f32);
            let grid = (
                (point.x / CELL_SIZE).floor() as i32,
                (point.y / CELL_SIZE).floor() as i32,
            );
            for dx in -1..=1 {
                for dy in -1..=1 {
                    cells.insert((grid.0 + dx, grid.1 + dy));
                }
            }
        }
    }
    let mut cells: Vec<_> = cells.into_iter().collect();
    cells.sort_unstable();
    cells
}

/// What the camera does on one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteFrame {
    pub position: Vec2,
    pub heading: Vec2,
    /// The height of the eye, or `None` while no ground has been found yet.
    pub eye_height: Option<f32>,
}

/// The route's progress, and the system resource that moves the camera along it.
#[derive(Resource)]
pub struct RouteRunner {
    plan: RoutePlan,
    ground: RouteGround,
    /// How far the camera has travelled, counted in `f64` so a long run or many laps do not lose
    /// the distance to `f32` rounding; it becomes `f32` only where a position is computed.
    distance: f64,
    started: bool,
    started_at_ms: Option<f64>,
    finished: bool,
    frames_on_route: u64,
    frames_without_ground: u64,
    /// The last terrain height found, which a frame with no ground keeps.
    last_ground: Option<f32>,
}

impl RouteRunner {
    pub fn new(plan: RoutePlan, ground: RouteGround) -> Self {
        Self {
            plan,
            ground,
            distance: 0.0,
            started: false,
            started_at_ms: None,
            finished: false,
            frames_on_route: 0,
            frames_without_ground: 0,
            last_ground: None,
        }
    }

    pub fn plan(&self) -> &RoutePlan {
        &self.plan
    }

    /// Whether the end of the route has been reached. The benchmark window ends with it.
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Whether the frame times of this frame belong to the route: the route has been on for a
    /// frame already, so the time since the previous frame was spent moving along it.
    pub fn scoring(&self) -> bool {
        self.frames_on_route >= 2
    }

    /// One frame. `delta_secs` is the real time since the previous frame, `ready` whether the route
    /// may start (the world is ready, or the route does not wait), and `now_ms` the real time since
    /// the app started. Before the start the camera waits at the first waypoint. The start frame
    /// stands there too: its delta is the wait itself, which the route does not walk. From the next
    /// frame on the route advances by `speed * delta_secs` whatever the world is doing.
    pub fn step(&mut self, delta_secs: f32, ready: bool, now_ms: f64) -> RouteFrame {
        let starting = !self.started && ready;
        if starting {
            self.started = true;
            self.started_at_ms = Some(now_ms);
        }
        let moving = self.started && !self.finished && !starting;
        if moving {
            self.distance += f64::from(self.plan.speed) * f64::from(delta_secs.max(0.0));
        }
        let pose = self.plan.pose_at(self.distance as f32);
        let ground = self.ground.height_at(pose.position);
        if moving {
            self.frames_on_route += 1;
            if ground.is_none() {
                self.frames_without_ground += 1;
            }
            if pose.finished {
                self.finished = true;
            }
        }
        if ground.is_some() {
            self.last_ground = ground;
        }
        RouteFrame {
            position: pose.position,
            heading: pose.heading,
            eye_height: self.last_ground.map(|ground| ground + self.plan.eye_height),
        }
    }

    /// The report block.
    pub fn report(&self) -> RouteReport {
        let laps = if self.plan.looped {
            (self.distance / f64::from(self.plan.length())).floor() as u64
        } else {
            u64::from(self.finished)
        };
        RouteReport {
            file: self.plan.source.clone(),
            name: self.plan.name.clone(),
            worldspace: self.plan.worldspace,
            waypoints: self.plan.waypoints.len(),
            length_units: self.plan.length(),
            speed_units_per_second: self.plan.speed,
            eye_height: self.plan.eye_height,
            looped: self.plan.looped,
            start: self.plan.start,
            started: self.started,
            started_at_run_ms: self.started_at_ms,
            completed: self.finished,
            laps,
            distance_travelled_units: if self.plan.looped {
                self.distance
            } else {
                self.distance.min(f64::from(self.plan.length()))
            },
            frames_on_route: self.frames_on_route,
            route_frames_without_ground: self.frames_without_ground,
            ground_cells_read: self.ground.cell_count(),
        }
    }
}

/// The report's `route` block: what the route was and how far the run got along it. `null` in a
/// run that followed no route.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RouteReport {
    /// The route file as given on the command line.
    pub file: String,
    pub name: String,
    pub worldspace: u32,
    pub waypoints: usize,
    /// One lap, or the whole route when it does not loop.
    pub length_units: f32,
    pub speed_units_per_second: f32,
    pub eye_height: f32,
    pub looped: bool,
    pub start: RouteStart,
    /// Whether the route started moving (the world became ready).
    pub started: bool,
    /// Milliseconds since the app started, on the real clock, when the route started.
    pub started_at_run_ms: Option<f64>,
    /// Whether the last waypoint was reached before the benchmark window ended.
    pub completed: bool,
    pub laps: u64,
    /// Total distance along the route, counted in `f64` (see `RouteRunner::distance`), so a long
    /// looped run keeps its last units.
    pub distance_travelled_units: f64,
    /// Frames the camera moved along the route.
    pub frames_on_route: u64,
    /// Route frames with no terrain under the camera, which kept the previous height.
    pub route_frames_without_ground: u64,
    /// Cells whose terrain heights were read before the run.
    pub ground_cells_read: usize,
}

/// Moves the camera along the route. Runs before `Update`, so the streaming planner of the same
/// frame sees where the camera is.
pub(crate) fn drive_route(
    config: Res<EngineConfig>,
    origin: Res<RenderOrigin>,
    real_time: Res<Time<Real>>,
    tracker: Option<Res<PacingTracker>>,
    cover: Option<Res<crate::pacing::LoadingCover>>,
    mut runner: ResMut<RouteRunner>,
    mut camera: Query<&mut Transform, With<StreamingCamera>>,
) {
    let Ok(mut camera) = camera.single_mut() else {
        return;
    };
    // Only a measuring run keeps the readiness the start waits for.
    let waits_for_world = runner.plan.start == RouteStart::WorldReady && config.measures_pacing();
    let ready = !waits_for_world
        || (cover.is_none_or(|cover| !cover.controls_blocked())
            && tracker
                .as_deref()
                .is_some_and(PacingTracker::world_ready_reached));
    let frame = runner.step(
        real_time.delta_secs(),
        ready,
        real_time.elapsed_secs_f64() * 1000.0,
    );
    let eye = frame.eye_height.unwrap_or(camera.translation.y);
    camera.translation = Vec3::new(
        frame.position.x - origin.0.x as f32 * CELL_SIZE,
        eye,
        -(frame.position.y - origin.0.y as f32 * CELL_SIZE),
    );
    // World +y is render -z.
    if let Ok(direction) = Dir3::new(Vec3::new(frame.heading.x, 0.0, -frame.heading.y)) {
        camera.look_to(direction, Dir3::Y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{
        "name": "test",
        "worldspace": "0x3c",
        "speed": 400,
        "waypoints": [[0, 0], [1000, 0], [1000, 500]]
    }"#;

    fn plan(json: &str) -> RoutePlan {
        RoutePlan::parse(json).unwrap()
    }

    fn error_of(json: &str) -> String {
        format!("{:#}", RoutePlan::parse(json).unwrap_err())
    }

    #[test]
    fn a_good_route_file_parses_with_its_defaults() {
        let plan = plan(GOOD);
        assert_eq!(plan.name, "test");
        assert_eq!(plan.worldspace, 0x3c);
        assert_eq!(plan.speed, 400.0);
        assert_eq!(plan.eye_height, DEFAULT_EYE_HEIGHT);
        assert!(!plan.looped);
        assert_eq!(plan.start, RouteStart::WorldReady);
        assert_eq!(plan.waypoints.len(), 3);
        assert_eq!(plan.length(), 1500.0);
        assert_eq!(plan.start_grid(), (0, 0));
    }

    #[test]
    fn every_option_can_be_given() {
        let plan = plan(
            r#"{
                "name": "n", "description": "d", "notes": ["a", "b"],
                "worldspace": 60, "speed": 123.5, "eye_height": 90,
                "loop": true, "start": "immediately",
                "waypoints": [[-5000, 9000], [5000, 9000], [5000, -9000]]
            }"#,
        );
        assert_eq!(plan.worldspace, 60);
        assert_eq!(plan.speed, 123.5);
        assert_eq!(plan.eye_height, 90.0);
        assert!(plan.looped);
        assert_eq!(plan.start, RouteStart::Immediately);
        // Cell of the first waypoint: floor(-5000 / 4096) = -2, floor(9000 / 4096) = 2.
        assert_eq!(plan.start_grid(), (-2, 2));
        // A loop closes: 10000 + 18000 + the way back, sqrt(10000^2 + 18000^2).
        let closing = (10_000.0f32 * 10_000.0 + 18_000.0 * 18_000.0).sqrt();
        assert!((plan.length() - (28_000.0 + closing)).abs() < 1.0);
    }

    #[test]
    fn worldspaces_are_numbers_or_decimal_or_hex_text() {
        for (worldspace, expected) in [
            ("60", 60),
            ("\"60\"", 60),
            ("\"0x3C\"", 0x3c),
            ("\"0X3c\"", 0x3c),
            ("\"0x0000003c\"", 0x3c),
        ] {
            let json = GOOD.replace("\"0x3c\"", worldspace);
            assert_eq!(plan(&json).worldspace, expected, "{worldspace}");
        }
        for bad in ["\"skyrim\"", "\"0xzz\"", "-1", "1.5", "true", "null"] {
            let json = GOOD.replace("\"0x3c\"", bad);
            assert!(RoutePlan::parse(&json).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn malformed_routes_are_refused_with_the_reason() {
        for (json, reason) in [
            ("not json", "not valid JSON"),
            ("{}", "not valid JSON"),
            ("[]", "not valid JSON"),
            // A mistyped key is an error and not a silent default.
            (
                r#"{"worldspace": 60, "speed": 400, "speeed": 5, "waypoints": [[0,0],[1000,0]]}"#,
                "not valid JSON",
            ),
            (
                r#"{"worldspace": 60, "speed": 400, "waypoints": [[0,0]]}"#,
                "at least two waypoints",
            ),
            (
                r#"{"worldspace": 60, "speed": 400, "waypoints": []}"#,
                "at least two waypoints",
            ),
            (
                r#"{"worldspace": 60, "speed": 0, "waypoints": [[0,0],[1000,0]]}"#,
                "speed must be",
            ),
            (
                r#"{"worldspace": 60, "speed": -5, "waypoints": [[0,0],[1000,0]]}"#,
                "speed must be",
            ),
            (
                r#"{"worldspace": 60, "speed": 99999, "waypoints": [[0,0],[1000,0]]}"#,
                "speed must be",
            ),
            (
                r#"{"worldspace": 60, "speed": 400, "eye_height": -1, "waypoints": [[0,0],[1000,0]]}"#,
                "eye_height must be",
            ),
            (
                r#"{"worldspace": 60, "speed": 400, "waypoints": [[0,0],[1e9,0]]}"#,
                "waypoint 1",
            ),
            (
                r#"{"worldspace": 60, "speed": 400, "waypoints": [[0,0],[0.5,0]]}"#,
                "legs must be at least",
            ),
            (
                r#"{"worldspace": 60, "speed": 400, "waypoints": [[0,0],[1000,0],[1000,0]]}"#,
                "legs must be at least",
            ),
            (
                r#"{"worldspace": 60, "speed": 400, "waypoints": [[0,0],[1000]]}"#,
                "not valid JSON",
            ),
            (
                r#"{"worldspace": 60, "speed": 400, "waypoints": [[0,0],[1000,0]], "start": "later"}"#,
                "not valid JSON",
            ),
            (
                r#"{"speed": 400, "waypoints": [[0,0],[1000,0]]}"#,
                "not valid JSON",
            ),
        ] {
            let message = error_of(json);
            assert!(
                message.contains(reason),
                "{json}\n  expected {reason:?} in: {message}"
            );
        }
        // A loop whose last waypoint is its first has a zero-length closing leg.
        let message = error_of(
            r#"{"worldspace": 60, "speed": 400, "loop": true, "waypoints": [[0,0],[1000,0],[0,0]]}"#,
        );
        assert!(message.contains("legs must be at least"), "{message}");
    }

    #[test]
    fn loading_names_the_file_in_its_errors() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.json");
        let message = format!("{:#}", RoutePlan::load(&missing).unwrap_err());
        assert!(message.contains("missing.json"), "{message}");
        let broken = directory.path().join("broken.json");
        std::fs::write(&broken, "{").unwrap();
        let message = format!("{:#}", RoutePlan::load(&broken).unwrap_err());
        assert!(message.contains("broken.json"), "{message}");
        let good = directory.path().join("good.json");
        std::fs::write(&good, GOOD).unwrap();
        let plan = RoutePlan::load(&good).unwrap();
        assert!(plan.source.ends_with("good.json"));
    }

    #[test]
    fn legs_interpolate_and_face_the_direction_of_travel() {
        let plan = plan(GOOD);
        let start = plan.pose_at(0.0);
        assert_eq!(start.position, Vec2::new(0.0, 0.0));
        assert_eq!(start.heading, Vec2::X);
        assert!(!start.finished);
        let middle = plan.pose_at(400.0);
        assert_eq!(middle.position, Vec2::new(400.0, 0.0));
        // The corner at 1000: the second leg begins there, and faces north.
        let corner = plan.pose_at(1000.0);
        assert_eq!(corner.position, Vec2::new(1000.0, 0.0));
        assert_eq!(corner.heading, Vec2::Y);
        let second = plan.pose_at(1250.0);
        assert_eq!(second.position, Vec2::new(1000.0, 250.0));
        assert_eq!(second.heading, Vec2::Y);
        // Just before the corner is still on the first leg.
        assert_eq!(plan.pose_at(999.0).heading, Vec2::X);
    }

    #[test]
    fn arrival_ends_a_route_and_a_loop_wraps() {
        let plan = plan(GOOD);
        let arrived = plan.pose_at(1500.0);
        assert!(arrived.finished);
        assert_eq!(arrived.position, Vec2::new(1000.0, 500.0));
        // Past the end it stays at the end, facing the last leg.
        let beyond = plan.pose_at(99_999.0);
        assert!(beyond.finished);
        assert_eq!(beyond.position, Vec2::new(1000.0, 500.0));
        assert_eq!(beyond.heading, Vec2::Y);
        assert!(!plan.pose_at(1499.0).finished);
        // Before the start is the start.
        assert_eq!(plan.pose_at(-5.0).position, Vec2::ZERO);

        let looped = self::plan(
            r#"{"worldspace": 60, "speed": 100, "loop": true,
                "waypoints": [[0,0],[1000,0],[1000,1000],[0,1000]]}"#,
        );
        assert_eq!(looped.length(), 4000.0);
        let back = looped.pose_at(3500.0);
        assert_eq!(back.position, Vec2::new(0.0, 500.0));
        assert_eq!(back.heading, Vec2::NEG_Y, "the closing leg returns south");
        let wrapped = looped.pose_at(4500.0);
        assert_eq!(wrapped.position, Vec2::new(500.0, 0.0));
        assert!(!wrapped.finished, "a loop never finishes");
        assert_eq!(looped.pose_at(4000.0).position, Vec2::ZERO);
    }

    /// A height field that rises one unit per sample to the east and two to the north.
    fn slope() -> Vec<f32> {
        let mut heights = Vec::new();
        for y in 0..TERRAIN_SAMPLES {
            for x in 0..TERRAIN_SAMPLES {
                heights.push(x as f32 + 2.0 * y as f32);
            }
        }
        heights
    }

    #[test]
    fn the_ground_is_interpolated_and_absent_outside_the_cells_read() {
        let ground = RouteGround::from_cells([((0, 0), slope())]);
        let step = CELL_SIZE / 32.0;
        assert_eq!(ground.height_at(Vec2::new(0.0, 0.0)), Some(0.0));
        assert_eq!(ground.height_at(Vec2::new(step, 0.0)), Some(1.0));
        assert_eq!(ground.height_at(Vec2::new(0.0, step)), Some(2.0));
        // Half a step east of the sample at (4, 4): the mean of 12 and 13.
        let height = ground.height_at(Vec2::new(4.5 * step, 4.0 * step)).unwrap();
        assert!((height - 12.5).abs() < 1e-3, "{height}");
        // The far edges of the cell are inside it.
        let corner = ground.height_at(Vec2::new(CELL_SIZE - 0.01, CELL_SIZE - 0.01));
        assert!((corner.unwrap() - 96.0).abs() < 0.1);
        // Neighbouring cells were not read.
        assert_eq!(ground.height_at(Vec2::new(CELL_SIZE + 1.0, 10.0)), None);
        assert_eq!(ground.height_at(Vec2::new(-1.0, 10.0)), None);
        assert_eq!(ground.height_at(Vec2::new(10.0, -1.0)), None);
        // A field of the wrong size is not kept.
        assert_eq!(
            RouteGround::from_cells([((0, 0), vec![0.0; 10])]).cell_count(),
            0
        );
    }

    #[test]
    fn the_cells_a_route_needs_cover_its_legs_and_their_neighbours() {
        let plan =
            plan(r#"{"worldspace": 60, "speed": 400, "waypoints": [[100, 100], [9000, 100]]}"#);
        let cells = route_cells(&plan);
        for x in -1..=3 {
            for y in -1..=1 {
                assert!(cells.contains(&(x, y)), "({x}, {y}) is missing");
            }
        }
        assert!(!cells.contains(&(4, 0)));
        assert_eq!(cells.len(), 15);
    }

    fn runner(json: &str, ground: RouteGround) -> RouteRunner {
        RouteRunner::new(plan(json), ground)
    }

    fn flat(height: f32) -> Vec<f32> {
        vec![height; TERRAIN_SAMPLES * TERRAIN_SAMPLES]
    }

    #[test]
    fn the_camera_waits_for_the_start_then_moves_at_the_files_speed() {
        let ground = RouteGround::from_cells([((0, 0), flat(100.0))]);
        let mut runner = runner(GOOD, ground);
        // Not ready: it waits at the first waypoint, at the eye height above the ground.
        let waiting = runner.step(1.0, false, 0.0);
        assert_eq!(waiting.position, Vec2::ZERO);
        assert_eq!(waiting.eye_height, Some(220.0));
        assert!(!runner.report().started);
        assert!(!runner.scoring());
        // Ready: the start frame stands at the first waypoint, whatever its delta was.
        let first = runner.step(0.5, true, 1234.0);
        assert_eq!(first.position, Vec2::ZERO);
        let report = runner.report();
        assert!(report.started);
        assert_eq!(report.started_at_run_ms, Some(1234.0));
        assert_eq!(report.frames_on_route, 0);
        // The next frame walks its own interval: 400 units/s for half a second.
        let second = runner.step(0.5, true, 1300.0);
        assert_eq!(second.position, Vec2::new(200.0, 0.0));
        assert_eq!(runner.report().frames_on_route, 1);
        assert!(
            !runner.scoring(),
            "the first walking frame is not scored yet"
        );
        runner.step(0.5, true, 1350.0);
        assert!(runner.scoring());
        // Readiness is read only at the start: a world that stops being ready does not stop it.
        let later = runner.step(0.5, false, 1400.0);
        assert_eq!(later.position, Vec2::new(600.0, 0.0));
        assert_eq!(runner.report().started_at_run_ms, Some(1234.0));
    }

    #[test]
    fn a_ready_route_starts_still_and_walks_from_its_next_frame() {
        let ground = RouteGround::from_cells([((0, 0), flat(0.0))]);
        let mut runner = runner(GOOD, ground);
        let start = runner.step(0.25, true, 0.0);
        assert_eq!(start.position, Vec2::ZERO);
        let frame = runner.step(0.25, true, 250.0);
        assert_eq!(frame.position, Vec2::new(100.0, 0.0));
    }

    /// The start frame carries the whole readiness wait as its delta. A short route must not run
    /// through its length in that one frame, nor finish on it.
    #[test]
    fn a_long_readiness_wait_does_not_walk_a_short_route_on_its_start_frame() {
        const SHORT: &str = r#"{
            "name": "short",
            "worldspace": "0x3c",
            "speed": 400,
            "waypoints": [[0, 0], [100, 0]]
        }"#;
        let ground = RouteGround::from_cells([((0, 0), flat(0.0))]);
        let mut runner = runner(SHORT, ground);
        let start = runner.step(10.0, true, 10_000.0);
        assert_eq!(start.position, Vec2::ZERO);
        assert!(!runner.finished());
        assert_eq!(runner.report().started_at_run_ms, Some(10_000.0));
        let walking = runner.step(0.1, true, 10_100.0);
        assert_eq!(walking.position, Vec2::new(40.0, 0.0));
        assert!(!runner.finished());
        let arrived = runner.step(0.25, true, 10_350.0);
        assert_eq!(arrived.position, Vec2::new(100.0, 0.0));
        assert!(runner.finished());
    }

    #[test]
    fn arrival_finishes_the_run_once_and_the_camera_stays_at_the_end() {
        let ground = RouteGround::from_cells([((0, 0), flat(0.0))]);
        let mut runner = runner(GOOD, ground);
        runner.step(0.0, true, 0.0);
        assert!(!runner.finished());
        // 1500 units at 400/s is 3.75 s.
        for _ in 0..3 {
            runner.step(1.0, true, 0.0);
        }
        assert!(!runner.finished());
        let end = runner.step(1.0, true, 0.0);
        assert!(runner.finished());
        assert_eq!(end.position, Vec2::new(1000.0, 500.0));
        let frames = runner.report().frames_on_route;
        let after = runner.step(1.0, true, 0.0);
        assert_eq!(after.position, end.position, "it does not move on");
        assert_eq!(
            runner.report().frames_on_route,
            frames,
            "and counts no more"
        );
        let report = runner.report();
        assert!(report.completed);
        assert_eq!(report.laps, 1);
        assert_eq!(report.distance_travelled_units, 1500.0);
    }

    #[test]
    fn a_loop_counts_laps_and_never_finishes() {
        let ground = RouteGround::from_cells([((0, 0), flat(0.0))]);
        let mut runner = runner(
            r#"{"worldspace": 60, "speed": 1000, "loop": true,
                "waypoints": [[0,0],[1000,0],[1000,1000],[0,1000]]}"#,
            ground,
        );
        for _ in 0..9 {
            runner.step(1.0, true, 0.0);
        }
        assert!(!runner.finished());
        let report = runner.report();
        assert_eq!(report.laps, 2, "9000 units over a 4000 unit lap");
        assert!(!report.completed);
    }

    /// A frame with no terrain under the camera keeps the last height and is counted; the count
    /// is of frames on the route, so the wait at the start is never one of them.
    #[test]
    fn frames_without_ground_keep_the_last_height_and_are_counted() {
        // Only the cell the route starts in has terrain; the route leaves it at x = 4096.
        let ground = RouteGround::from_cells([((0, 0), flat(500.0))]);
        let mut runner = runner(
            r#"{"worldspace": 60, "speed": 1000, "eye_height": 100,
                "waypoints": [[0, 100], [8000, 100]]}"#,
            ground,
        );
        // Waiting before the start is not a route frame even when there is no ground.
        assert_eq!(runner.step(1.0, false, 0.0).eye_height, Some(600.0));
        assert_eq!(runner.report().route_frames_without_ground, 0);
        // The start frame stands at the first waypoint and is not a route frame either.
        assert_eq!(runner.step(1.0, true, 0.0).position.x, 0.0);
        assert_eq!(runner.report().route_frames_without_ground, 0);
        for second in 1..=4 {
            let frame = runner.step(1.0, true, 0.0);
            assert_eq!(frame.position.x, 1000.0 * second as f32);
            assert_eq!(frame.eye_height, Some(600.0));
        }
        assert_eq!(runner.report().route_frames_without_ground, 0);
        // Now past x = 4096: no cell, the height is kept.
        let past = runner.step(1.0, true, 0.0);
        assert_eq!(past.position.x, 5000.0);
        assert_eq!(past.eye_height, Some(600.0), "the last height is kept");
        runner.step(1.0, true, 0.0);
        let report = runner.report();
        assert_eq!(report.route_frames_without_ground, 2);
        assert_eq!(report.frames_on_route, 6);
    }

    #[test]
    fn a_route_that_never_finds_ground_has_no_eye_height_to_offer() {
        let mut runner = runner(GOOD, RouteGround::default());
        // The start frame walks nothing, so it has no ground to count either.
        runner.step(1.0, true, 0.0);
        let frame = runner.step(1.0, true, 0.0);
        assert_eq!(
            frame.eye_height, None,
            "the caller keeps the camera's own height"
        );
        assert_eq!(runner.report().route_frames_without_ground, 1);
    }

    /// The route files the baseline ships parse and validate. What they are (length, speed,
    /// places) is documented in the files themselves and is no business of an engine test.
    /// Skipped when the repository's `scripts/` is not next to this crate.
    #[test]
    fn the_shipped_routes_parse_and_validate() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/routes");
        if !directory.is_dir() {
            return;
        }
        for name in ["travel.json", "check.json"] {
            RoutePlan::load(&directory.join(name)).unwrap();
        }
    }

    #[test]
    fn the_report_serialises_with_the_documented_field_names() {
        let ground = RouteGround::from_cells([((0, 0), flat(0.0))]);
        let mut runner = runner(GOOD, ground);
        runner.step(0.5, true, 10.0);
        runner.step(0.5, true, 510.0);
        let json = serde_json::to_value(runner.report()).unwrap();
        assert_eq!(json["name"], "test");
        assert_eq!(json["start"], "world_ready");
        assert_eq!(json["route_frames_without_ground"], 0);
        assert_eq!(json["frames_on_route"], 1);
        assert_eq!(json["completed"], false);
        assert_eq!(json["ground_cells_read"], 1);
    }
}
