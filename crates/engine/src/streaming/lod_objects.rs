//! Streaming of object LOD chunks: the retail `.bto` blocks the converter imports into the
//! additive `lod_object_chunks` table.
//!
//! A pack without that table has no object chunks, so nothing here runs for it. For a pack with
//! it the load fails closed: the rows are validated by [`crate::world::database::load_lod_object_chunks`]
//! and every tier-16 file (the only tier drawn in this slice) is hash-checked, one IO-pool task
//! per file, before any chunk is drawn. A damaged row, a missing file or a wrong hash logs one
//! error, draws no object LOD for that pack and is recorded in [`LodObjects::failure`], never
//! skipped chunk by chunk. The failure belongs to the pack: it stays through worldspace changes
//! and is cleared only when the pack changes. Rows of tiers that do not draw are not read.
//!
//! Which chunks draw follows the retail object rule (RF-LOD-004): a chunk of tier `t` draws when
//! [`TerrainLodDistances::object_tier`] picks `t` for its cells. **Only tier 16 draws in this
//! slice.** A tier-16 chunk draws between `fBlockLevel1Distance` and `fBlockMaximumDistance` from
//! the camera, where it never meets the loaded grid. Tier 8 and tier 4 rows are ignored, because
//! object chunks do not hand off to full detail yet and tier 4 would overlap the loaded cells.
//!
//! The system runs before `update_render_origin`, so a root spawned in the frame the origin shifts
//! is rebased in that frame. Spawns follow the streaming controls and the commit budget that
//! terrain chunks use. The verified rows are kept while the worldspace is unchanged, so leaving
//! and re-entering the exterior (a door) does not read or hash them again.

use super::{
    ActiveSpace, RenderOrigin, StreamingCommitBudget,
    lod::{LodChunkGridOrigin, chunk_min_grid, lod_worldspace, streaming_center},
    runtime::StreamingRuntime,
};
use crate::{
    config::{EngineConfig, TerrainLodDistances},
    world::{
        components::{CELL_SIZE, StreamingCamera},
        database::{LodChunkMetadata, WorldDatabase, verify_lod_object_payloads},
    },
};
use bevy::{
    gltf::GltfAssetLabel,
    light::{NotShadowCaster, NotShadowReceiver},
    prelude::*,
    tasks::{IoTaskPool, Task, block_on},
    world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady},
};
use shared::lod::{ChunkKey, LodTier};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

/// The worldspace multiplier for object LOD (RF-LOD-005): the WRLD `NAMA` value, 1.0 unless the
/// worldspace says otherwise. The pack does not carry `NAMA` yet, so every worldspace uses 1.0,
/// which is the retail value for Tamriel and most others. The four city worldspaces (0.25) need
/// the `NAMA` column, which waits for the converter-output freeze.
pub const OBJECT_LOD_NAMA: f32 = 1.0;

/// Marks the root of one object LOD chunk.
#[derive(Component)]
pub struct LodObjectRoot {
    pub key: ChunkKey,
}

/// One payload check in flight.
type Check = (LodChunkMetadata, Task<Result<(), String>>);

/// Where the object chunks of the streamed worldspace stand.
#[derive(Resource, Default)]
pub struct LodObjects {
    worldspace_id: Option<u32>,
    generation: u64,
    requested: bool,
    /// One payload check per listed chunk, on the IO pool. Dropping the tasks cancels them.
    checking: Vec<Check>,
    /// Chunks whose check has passed while the others are still running.
    checked: Vec<LodChunkMetadata>,
    /// Every listed chunk, after all checks passed. Object LOD draws only from this.
    chunks: Vec<LodChunkMetadata>,
    roots: HashMap<ChunkKey, Entity>,
    /// The pack (assets folder) the state above belongs to.
    pack: Option<PathBuf>,
    /// The camera cell and distances the current selection was made for.
    last_center: Option<IVec2>,
    last_distances: Option<TerrainLodDistances>,
    failure: Option<String>,
}

impl LodObjects {
    /// Why this pack's object LOD is off, when a row or payload failed its checks.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// The verified chunks of the streamed worldspace.
    pub fn chunks(&self) -> &[LodChunkMetadata] {
        &self.chunks
    }

    /// Chunks currently spawned.
    pub fn resident(&self) -> usize {
        self.roots.len()
    }

    /// A new worldspace: forget everything, including the verified rows.
    fn reset(&mut self, commands: &mut Commands, worldspace_id: Option<u32>) {
        self.suspend(commands);
        self.worldspace_id = worldspace_id;
        self.generation = self.generation.wrapping_add(1);
        self.requested = false;
        self.checking.clear();
        self.checked.clear();
        self.chunks.clear();
        // `failure` stays: it belongs to the pack, not to the worldspace.
    }

    /// A different pack: forget everything, including a failure.
    fn reset_pack(&mut self, commands: &mut Commands, pack: PathBuf) {
        self.reset(commands, None);
        self.failure = None;
        self.pack = Some(pack);
    }

    /// No exterior to draw in (an interior, or LOD off): drop the roots and the selection but keep
    /// the rows and their verified state, so coming back does not read or hash them again.
    fn suspend(&mut self, commands: &mut Commands) {
        for (_, root) in self.roots.drain() {
            commands.entity(root).try_despawn();
        }
        self.last_center = None;
        self.last_distances = None;
    }

    /// Submits the one-time row request. `requested` is set only after the worker accepted it, so
    /// a full queue (cell loads share it) is retried on a later frame instead of becoming a
    /// permanent failure, as terrain LOD retries a refused query.
    fn submit_request(&mut self, database: &WorldDatabase, worldspace_id: u32) {
        if self.requested {
            return;
        }
        match database.request_lod_object_chunks(self.generation, worldspace_id) {
            Ok(()) => self.requested = true,
            Err(error) => debug!(%error, "object LOD chunk request will be retried"),
        }
    }

    /// Whether the selection must be recomputed for `center` and `distances`, remembering both. A
    /// change of the camera cell or of any distance setting needs a new selection.
    fn selection_changed(&mut self, center: IVec2, distances: &TerrainLodDistances) -> bool {
        if self.last_center == Some(center) && self.last_distances.as_ref() == Some(distances) {
            return false;
        }
        self.last_center = Some(center);
        self.last_distances = Some(*distances);
        true
    }

    /// Starts one payload check per tier-16 row, the only rows that can draw in this slice.
    fn start_checks(&mut self, chunks: Vec<LodChunkMetadata>, assets: &std::path::Path) {
        self.checked.clear();
        self.checking = chunks
            .into_iter()
            .filter(|chunk| chunk.key.tier == LodTier::Tier16)
            .map(|chunk| {
                let assets = assets.to_owned();
                let listed = chunk.clone();
                let task = IoTaskPool::get().spawn(async move {
                    verify_lod_object_payloads(&assets, std::slice::from_ref(&listed))
                        .map_err(|error| format!("{error:#}"))
                });
                (chunk, task)
            })
            .collect();
    }

    /// Collects finished checks. The first failure turns object LOD off for the pack and cancels
    /// the rest; when every check passed, the chunks become drawable.
    fn poll_checks(&mut self) {
        if self.checking.is_empty() || self.failure.is_some() {
            return;
        }
        for (chunk, task) in std::mem::take(&mut self.checking) {
            if !task.is_finished() {
                self.checking.push((chunk, task));
                continue;
            }
            match block_on(task) {
                Ok(()) => self.checked.push(chunk),
                Err(reason) => {
                    let reason = format!("object LOD is off for this pack: {reason}");
                    error!("{reason}");
                    self.failure = Some(reason);
                    self.checking.clear();
                    self.checked.clear();
                    return;
                }
            }
        }
        if self.checking.is_empty() {
            let mut chunks = std::mem::take(&mut self.checked);
            chunks.sort_by_key(|chunk| chunk.key);
            self.chunks = chunks;
        }
    }
}

/// LOD objects neither receive nor cast the sun shadow (`RF-SHADER-013`, `RF-SHADER-014`): their
/// lighting shader keys have no sun-mask bits and the shipped LOD files have no cast flag. Every
/// mesh of an object chunk gets both markers when its scene is ready.
pub(super) fn unshadow_object_chunk(
    ready: On<WorldInstanceReady>,
    roots: Query<(), With<LodObjectRoot>>,
    children: Query<&Children>,
    meshes: Query<(), With<Mesh3d>>,
    mut commands: Commands,
) {
    if roots.contains(ready.entity) {
        unshadow_chunk_meshes(ready.entity, &children, &meshes, &mut commands);
    }
}

fn unshadow_chunk_meshes(
    root: Entity,
    children: &Query<&Children>,
    meshes: &Query<(), With<Mesh3d>>,
    commands: &mut Commands,
) {
    for descendant in children.iter_descendants(root) {
        if meshes.contains(descendant) {
            commands
                .entity(descendant)
                .insert((NotShadowCaster, NotShadowReceiver));
        }
    }
}

/// The components of an object chunk root, placed relative to the render origin. It carries
/// [`LodChunkGridOrigin`], which `update_render_origin` reads to move the root when the render
/// origin shifts, exactly like a terrain chunk root. The caller adds the scene handle.
fn object_root_bundle(
    chunk: &LodChunkMetadata,
    render_origin: IVec2,
) -> (
    Name,
    LodObjectRoot,
    LodChunkGridOrigin,
    Transform,
    Visibility,
) {
    let (grid_x, grid_y) = chunk_min_grid(chunk.key, chunk.origin);
    (
        Name::new(format!(
            "Object LOD {:?} chunk {},{}",
            chunk.key.tier, chunk.key.anchor.x, chunk.key.anchor.y
        )),
        LodObjectRoot { key: chunk.key },
        LodChunkGridOrigin { grid_x, grid_y },
        Transform::from_xyz(
            (grid_x - i64::from(render_origin.x)) as f32 * CELL_SIZE,
            0.0,
            -((grid_y - i64::from(render_origin.y)) as f32) * CELL_SIZE,
        ),
        Visibility::Inherited,
    )
}

/// The chunks that draw from the camera cell `camera`: tier-16 chunks whose cells fall in the tier
/// the object rule picks for them. All cells of a chunk share its box, so the first cell decides.
/// Tier 8 and tier 4 chunks are never selected in this slice: they would draw over the loaded
/// grid with no hand-off to full detail.
pub fn wanted_object_chunks(
    chunks: &[LodChunkMetadata],
    camera: IVec2,
    distances: &TerrainLodDistances,
    nama: f32,
) -> Vec<ChunkKey> {
    chunks
        .iter()
        .filter(|chunk| chunk.key.tier == LodTier::Tier16)
        .filter(|chunk| {
            let (x, y) = chunk_min_grid(chunk.key, chunk.origin);
            let cell = IVec2::new(x as i32, y as i32);
            let origin = IVec2::new(chunk.origin.grid_x, chunk.origin.grid_y);
            distances.object_tier(camera, cell, origin, nama) == Some(chunk.key.tier)
        })
        .map(|chunk| chunk.key)
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn update_lod_objects(
    mut commands: Commands,
    config: Res<EngineConfig>,
    space: Option<Res<ActiveSpace>>,
    database: Res<WorldDatabase>,
    origin: Res<RenderOrigin>,
    camera: Query<&Transform, With<StreamingCamera>>,
    asset_server: Res<AssetServer>,
    runtime: Option<Res<StreamingRuntime>>,
    mut budget: ResMut<StreamingCommitBudget>,
    mut objects: ResMut<LodObjects>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    if objects.pack.as_deref() != Some(config.assets_dir.as_path()) {
        objects.reset_pack(&mut commands, config.assets_dir.clone());
    }
    let worldspace = lod_worldspace(config.stream_radius, space.as_deref(), config.worldspace_id);
    let Some(worldspace_id) = worldspace else {
        // An interior, or LOD off: keep the verified rows, drop what is drawn.
        objects.suspend(&mut commands);
        return;
    };
    if objects.worldspace_id != Some(worldspace_id) {
        objects.reset(&mut commands, Some(worldspace_id));
    }
    // A failure keeps object LOD off for the whole pack, whatever the worldspace.
    if objects.failure.is_some() {
        return;
    }
    objects.submit_request(&database, worldspace_id);

    // Rows arrive from the database worker; a failure turns object LOD off for this pack. A
    // response of an older generation belongs to a worldspace that is no longer streamed.
    while let Some(response) = database.try_lod_object_response() {
        if response.generation != objects.generation || response.worldspace_id != worldspace_id {
            continue;
        }
        match response.result {
            Ok(chunks) if chunks.is_empty() => {}
            Ok(chunks) => objects.start_checks(chunks, &config.assets_dir),
            Err(failure) => {
                let reason = format!("object LOD chunk rows are invalid: {}", failure.reason);
                error!("{reason}");
                objects.failure = Some(reason);
            }
        }
    }
    objects.poll_checks();
    if objects.failure.is_some() || objects.chunks.is_empty() {
        return;
    }

    let center = streaming_center(config.acceptance_screenshot.is_some(), origin.0, camera);
    if !objects.selection_changed(center, &config.terrain_lod) {
        return;
    }
    let wanted: HashSet<ChunkKey> = wanted_object_chunks(
        &objects.chunks,
        center,
        &config.terrain_lod,
        OBJECT_LOD_NAMA,
    )
    .into_iter()
    .collect();
    let stale: Vec<ChunkKey> = objects
        .roots
        .keys()
        .filter(|key| !wanted.contains(key))
        .copied()
        .collect();
    for key in stale {
        if let Some(root) = objects.roots.remove(&key) {
            commands.entity(root).try_despawn();
        }
    }
    let to_spawn: Vec<LodChunkMetadata> = objects
        .chunks
        .iter()
        .filter(|chunk| wanted.contains(&chunk.key) && !objects.roots.contains_key(&chunk.key))
        .cloned()
        .collect();
    // Spawns follow the streaming controls and the commit budget terrain chunks use. Chunks left
    // over make the next frame select again.
    let mut deferred = false;
    for chunk in to_spawn {
        if runtime
            .as_deref()
            .is_some_and(|runtime| !runtime.cell_requests_allowed())
            || budget.remaining == 0
        {
            deferred = true;
            break;
        }
        budget.remaining -= 1;
        budget.commits = budget.commits.saturating_add(1);
        let handle: Handle<WorldAsset> =
            asset_server.load(GltfAssetLabel::Scene(0).from_asset(chunk.payload_path.clone()));
        let root = commands
            .spawn((object_root_bundle(&chunk, origin.0), WorldAssetRoot(handle)))
            .id();
        objects.roots.insert(chunk.key, root);
    }
    if deferred {
        objects.last_center = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::database::LodChunkBounds;
    use shared::lod::{ChunkAnchor, LodOrigin, LodTier};

    /// A level-16 object chunk of Tamriel (LOD origin -96) at `anchor`.
    fn chunk(anchor_x: i32, anchor_y: i32) -> LodChunkMetadata {
        let key = ChunkKey::new(60, LodTier::Tier16, ChunkAnchor::new(anchor_x, anchor_y));
        LodChunkMetadata {
            key,
            payload_path: shared::lod::object_chunk_payload_path(key),
            content_hash: "a".repeat(64),
            bounds: LodChunkBounds {
                min: [0.0; 3],
                max: [1.0; 3],
            },
            source_cells: Vec::new(),
            build_identity: "b".repeat(64),
            origin: LodOrigin::new(-96, -96),
        }
    }

    #[test]
    fn a_level_16_chunk_draws_between_the_level_1_distance_and_the_maximum() {
        let distances = TerrainLodDistances::default();
        // Camera cell (5, -12), Riverwood. Anchors are 16-cell blocks from origin -96.
        let camera = IVec2::new(5, -12);
        let chunks: Vec<_> = [(6, 5), (7, 5), (6, 6), (12, 12), (-2, 5), (4, 4)]
            .into_iter()
            .map(|(x, y)| chunk(x, y))
            .collect();
        let wanted = wanted_object_chunks(&chunks, camera, &distances, OBJECT_LOD_NAMA);
        let at = |x: i32, y: i32| {
            wanted
                .iter()
                .any(|key| key.anchor == ChunkAnchor::new(x, y))
        };
        // Anchor (6, 5) is the block [0, 16) x [-16, 0) that holds the camera: too near for tier 16.
        assert!(!at(6, 5));
        // Anchor (7, 5) is [16, 32) x [-16, 0): 43,008 units east of the camera cell's centre, under
        // 70,000.
        assert!(!at(7, 5));
        // Anchor (6, 6) is [0, 16) x [0, 16): 47,104 units away, under 70,000.
        assert!(!at(6, 6));
        // Anchor (12, 12) is [96, 112) x [96, 112): about 575,600 units away, past the maximum.
        assert!(!at(12, 12));
        // Anchor (4, 4) is [-32, -16) x [-32, -16): 88,064 units west and 18,432 south, about 89,975
        // away: between 70,000 and 250,000, so tier 16 draws.
        assert!(at(4, 4));
        // Anchor (-2, 5) is [-128, -112) x [-16, 0): 481,280 units west, past the maximum.
        assert!(!at(-2, 5));
    }

    #[test]
    fn a_full_request_queue_is_retried_and_never_a_permanent_failure() {
        use crate::world::database::DatabaseRequest;
        // A queue of one, already holding another request, as when cell loads fill the shared
        // queue.
        let (database, requests, _responses) = WorldDatabase::channel_fixture(1);
        database.request_lod_object_chunks(99, 60).unwrap();
        let mut objects = LodObjects::default();

        objects.submit_request(&database, 60);
        assert!(
            !objects.requested,
            "a refused request is not marked as sent"
        );
        assert!(objects.failure().is_none(), "a full queue is not a failure");
        // Still full on the next frame: still retried, still not a failure.
        objects.submit_request(&database, 60);
        assert!(!objects.requested && objects.failure().is_none());

        // The queue drains and the next frame submits the request once.
        assert!(matches!(
            requests.try_recv(),
            Ok(DatabaseRequest::LoadLodObjectChunks { generation: 99, .. })
        ));
        objects.submit_request(&database, 60);
        assert!(objects.requested);
        assert!(matches!(
            requests.try_recv(),
            Ok(DatabaseRequest::LoadLodObjectChunks {
                generation: 0,
                worldspace_id: 60,
                ..
            })
        ));
        // A sent request is not sent again.
        objects.submit_request(&database, 60);
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn the_selection_follows_the_distance_settings_and_not_only_the_camera_cell() {
        let mut objects = LodObjects::default();
        let distances = TerrainLodDistances::default();
        let cell = IVec2::new(5, -12);
        assert!(
            objects.selection_changed(cell, &distances),
            "first selection"
        );
        assert!(
            !objects.selection_changed(cell, &distances),
            "same cell and distances keep the selection"
        );
        // A distance change re-selects although the camera stands still.
        let nearer = TerrainLodDistances {
            block_level1_distance: 30_000.0,
            ..distances
        };
        assert!(objects.selection_changed(cell, &nearer));
        assert!(!objects.selection_changed(cell, &nearer));
        // So does a move to another cell.
        assert!(objects.selection_changed(IVec2::new(6, -12), &nearer));
    }

    #[test]
    fn object_chunk_roots_move_with_the_terrain_when_the_render_origin_shifts() {
        use crate::{
            profiling::ProfilingState,
            streaming::{ActiveSpace, StreamingMetrics},
        };
        let mut app = App::new();
        app.insert_resource(EngineConfig::default())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .init_resource::<ActiveSpace>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, super::super::update_render_origin);
        let camera = app
            .world_mut()
            .spawn((Transform::default(), StreamingCamera))
            .id();
        // Anchor (6, 5) from LOD origin -96: its south-west cell is (0, -16).
        let root = app
            .world_mut()
            .spawn(object_root_bundle(&chunk(6, 5), IVec2::ZERO))
            .id();
        let before = app
            .world()
            .entity(root)
            .get::<Transform>()
            .unwrap()
            .translation;
        assert_eq!(before, Vec3::new(0.0, 0.0, 16.0 * CELL_SIZE));

        // The camera crosses 3 cells east and 2 north: the origin shifts by (3, 2).
        {
            let mut entity = app.world_mut().entity_mut(camera);
            let mut transform = entity.get_mut::<Transform>().unwrap();
            transform.translation.x = 3.0 * CELL_SIZE + 12.0;
            transform.translation.z = -(2.0 * CELL_SIZE) - 20.0;
        }
        app.update();
        assert_eq!(app.world().resource::<RenderOrigin>().0, IVec2::new(3, 2));
        let after = app
            .world()
            .entity(root)
            .get::<Transform>()
            .unwrap()
            .translation;
        assert_ne!(after, before, "the root moved");
        assert_eq!(
            after,
            Vec3::new(-3.0 * CELL_SIZE, 0.0, 18.0 * CELL_SIZE),
            "the root is where a terrain chunk of the same cell would be"
        );
    }

    #[test]
    fn nothing_is_wanted_without_chunks() {
        assert!(
            wanted_object_chunks(&[], IVec2::ZERO, &TerrainLodDistances::default(), 1.0).is_empty()
        );
    }

    /// A chunk of `tier` at `anchor` in Tamriel (LOD origin -96).
    fn chunk_of(tier: LodTier, anchor_x: i32, anchor_y: i32) -> LodChunkMetadata {
        let mut chunk = chunk(anchor_x, anchor_y);
        chunk.key = ChunkKey::new(60, tier, ChunkAnchor::new(anchor_x, anchor_y));
        chunk.payload_path = shared::lod::object_chunk_payload_path(chunk.key);
        chunk
    }

    #[test]
    fn only_tier_16_rows_are_selected_in_this_slice() {
        let distances = TerrainLodDistances::default();
        let camera = IVec2::new(5, -12);
        // The object rule picks tier 8 for cell (24, -16) and tier 4 for cell (8, -12) from this
        // camera, so these rows would draw if tiers were not filtered.
        let tier8 = chunk_of(LodTier::Tier8, 15, 10);
        let tier4 = chunk_of(LodTier::Tier4, 26, 21);
        for (row, tier, cell) in [
            (&tier8, LodTier::Tier8, IVec2::new(24, -16)),
            (&tier4, LodTier::Tier4, IVec2::new(8, -12)),
        ] {
            assert_eq!(
                distances.object_tier(camera, cell, IVec2::new(-96, -96), 1.0),
                Some(tier),
                "the rule alone would draw this row"
            );
            assert_eq!(
                chunk_min_grid(row.key, row.origin),
                (i64::from(cell.x), i64::from(cell.y))
            );
        }
        let rows = [tier8, tier4, chunk(4, 4)];
        let wanted = wanted_object_chunks(&rows, camera, &distances, OBJECT_LOD_NAMA);
        assert_eq!(
            wanted,
            vec![rows[2].key],
            "only the tier-16 row is selected"
        );
    }

    // ---- The real `update_lod_objects` system -------------------------------------------------

    use crate::{
        profiling::ProfilingState,
        streaming::{StreamingMetrics, runtime::StreamingRuntime},
        world::database::{DatabaseRequest, LodObjectResponse},
    };
    use bevy::asset::{AssetApp, AssetPlugin};
    use crossbeam_channel::{Receiver, Sender};
    use sha2::{Digest, Sha256};
    use std::time::{Duration, Instant};

    /// The real system on a real `AssetServer` and a temporary assets folder, with a database
    /// whose object channel the test answers itself.
    struct Rig {
        app: App,
        requests: Receiver<DatabaseRequest>,
        responses: Sender<LodObjectResponse>,
        assets: tempfile::TempDir,
        camera: Entity,
    }

    /// Camera at the centre of cell (5, -12), Riverwood: chunk (4, 4) is in the tier-16 ring.
    const RIVERWOOD: Vec3 = Vec3::new(5.5 * CELL_SIZE, 0.0, 11.5 * CELL_SIZE);
    /// Camera inside the block of chunk (4, 4), where tier 16 does not draw.
    const INSIDE_CHUNK_4_4: Vec3 = Vec3::new(-20.5 * CELL_SIZE, 0.0, 20.5 * CELL_SIZE);

    fn rig(with_rebase: bool) -> Rig {
        let assets = tempfile::tempdir().unwrap();
        let (database, requests, responses) = WorldDatabase::channel_fixture_with_lod_objects(4);
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin {
                file_path: assets.path().to_string_lossy().into_owned(),
                ..default()
            },
        ))
        .init_asset::<WorldAsset>()
        .insert_resource(EngineConfig {
            assets_dir: assets.path().into(),
            worldspace_id: 60,
            headless: true,
            ..default()
        })
        .insert_resource(database)
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(StreamingCommitBudget {
            remaining: 100,
            ..default()
        })
        .init_resource::<LodObjects>();
        if with_rebase {
            app.init_resource::<ActiveSpace>()
                .init_resource::<StreamingMetrics>()
                .init_resource::<ProfilingState>()
                .add_systems(
                    Update,
                    (
                        update_lod_objects.before(super::super::update_render_origin),
                        super::super::update_render_origin,
                    ),
                );
        } else {
            app.add_systems(Update, update_lod_objects);
        }
        let camera = app
            .world_mut()
            .spawn((StreamingCamera, Transform::from_translation(RIVERWOOD)))
            .id();
        Rig {
            app,
            requests,
            responses,
            assets,
            camera,
        }
    }

    impl Rig {
        fn objects(&self) -> &LodObjects {
            self.app.world().resource::<LodObjects>()
        }

        fn move_camera(&mut self, to: Vec3) {
            self.app
                .world_mut()
                .entity_mut(self.camera)
                .get_mut::<Transform>()
                .unwrap()
                .translation = to;
        }

        /// Writes `bytes` as the chunk's payload and records their hash in the row.
        fn write_payload(&self, chunk: &mut LodChunkMetadata, bytes: &[u8]) {
            chunk.content_hash = Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let path = self.assets.path().join(&chunk.payload_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        /// Runs one frame, takes the system's row request and answers it with `chunks`. The
        /// answer carries the request's generation plus `generation_offset`.
        fn answer(&mut self, chunks: Vec<LodChunkMetadata>, generation_offset: u64) {
            self.app.update();
            let Ok(DatabaseRequest::LoadLodObjectChunks {
                generation,
                worldspace_id,
                ..
            }) = self.requests.try_recv()
            else {
                panic!("the system did not request the object chunks");
            };
            self.responses
                .send(LodObjectResponse {
                    generation: generation + generation_offset,
                    worldspace_id,
                    result: Ok(chunks),
                })
                .unwrap();
        }

        /// Runs frames until `done` holds, or fails after a deadline.
        fn run_until(&mut self, what: &str, done: impl Fn(&Rig) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done(self) {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(5));
                self.app.update();
            }
        }

        fn root_count(&mut self) -> usize {
            self.app
                .world_mut()
                .query::<&LodObjectRoot>()
                .iter(self.app.world())
                .count()
        }
    }

    #[test]
    fn a_missing_payload_turns_object_lod_off_once_and_spawns_nothing() {
        let mut rig = rig(false);
        let mut good = chunk(4, 4);
        rig.write_payload(&mut good, b"good");
        let missing = chunk(8, 8); // listed, never written
        rig.answer(vec![good, missing], 0);
        rig.run_until("the failure", |rig| rig.objects().failure().is_some());
        let reason = rig.objects().failure().unwrap().to_owned();
        assert!(
            reason.contains("x: 8"),
            "the error names the chunk: {reason}"
        );
        // The good chunk is in range but must not draw: the pack fails closed as a whole.
        for _ in 0..5 {
            rig.app.update();
        }
        assert_eq!(rig.objects().resident(), 0);
        assert_eq!(rig.root_count(), 0);
        assert_eq!(
            rig.objects().failure(),
            Some(reason.as_str()),
            "one error, kept"
        );
        assert!(rig.objects().chunks().is_empty());
    }

    #[test]
    fn a_failure_belongs_to_the_pack_and_survives_a_worldspace_change() {
        let mut rig = rig(false);
        let mut good = chunk(4, 4);
        rig.write_payload(&mut good, b"good");
        rig.answer(vec![good, chunk(8, 8)], 0); // (8, 8) is listed but never written
        rig.run_until("the failure", |rig| rig.objects().failure().is_some());

        // Entering another worldspace does not clear it: no request, no roots.
        rig.app
            .world_mut()
            .resource_mut::<EngineConfig>()
            .worldspace_id = 61;
        for _ in 0..3 {
            rig.app.update();
        }
        assert!(rig.objects().failure().is_some(), "still off for the pack");
        assert!(rig.requests.try_recv().is_err(), "no new row request");
        assert_eq!(rig.root_count(), 0);

        // A different pack starts clean and asks for its own rows.
        let other = tempfile::tempdir().unwrap();
        rig.app
            .world_mut()
            .resource_mut::<EngineConfig>()
            .assets_dir = other.path().into();
        rig.app.update();
        assert!(
            rig.objects().failure().is_none(),
            "a new pack clears the failure"
        );
        assert!(matches!(
            rig.requests.try_recv(),
            Ok(DatabaseRequest::LoadLodObjectChunks {
                worldspace_id: 61,
                ..
            })
        ));
    }

    #[test]
    fn rows_of_tiers_that_do_not_draw_are_not_read() {
        let mut rig = rig(false);
        let mut drawable = chunk(4, 4);
        rig.write_payload(&mut drawable, b"glb");
        // A tier-8 row whose file is missing: it can never draw in this slice, so it is not
        // checked and does not turn object LOD off.
        let ignored = chunk_of(LodTier::Tier8, 15, 10);
        rig.answer(vec![drawable, ignored], 0);
        rig.run_until("the root", |rig| rig.objects().resident() == 1);
        assert!(rig.objects().failure().is_none());
        assert_eq!(
            rig.objects().chunks().len(),
            1,
            "only the tier-16 row is kept"
        );
    }

    #[test]
    fn a_hash_mismatch_fails_closed_too() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"as converted");
        std::fs::write(rig.assets.path().join(&chunk.payload_path), b"damaged").unwrap();
        rig.answer(vec![chunk], 0);
        rig.run_until("the failure", |rig| rig.objects().failure().is_some());
        assert!(rig.objects().failure().unwrap().contains("SHA-256"));
        assert_eq!(rig.objects().resident(), 0);
    }

    #[test]
    fn verified_chunks_in_range_spawn_through_the_system() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        let expected = object_root_bundle(&chunk, IVec2::ZERO).3.translation;
        rig.answer(vec![chunk], 0);
        rig.run_until("the root", |rig| rig.objects().resident() == 1);
        assert!(rig.objects().failure().is_none());
        let mut roots = rig.app.world_mut().query::<(&LodObjectRoot, &Transform)>();
        let found: Vec<_> = roots.iter(rig.app.world()).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.key.anchor, ChunkAnchor::new(4, 4));
        assert_eq!(found[0].1.translation, expected);
    }

    #[test]
    fn a_response_of_an_older_generation_is_dropped() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        // The generation of a worldspace that is no longer streamed (offset 5 is never current).
        rig.answer(vec![chunk], 5);
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(5));
            rig.app.update();
        }
        assert!(rig.objects().chunks().is_empty());
        assert!(rig.objects().checking.is_empty());
        assert!(rig.objects().failure().is_none());
        assert_eq!(rig.root_count(), 0);
    }

    #[test]
    fn leaving_the_ring_despawns_the_root_and_coming_back_spawns_it() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        rig.answer(vec![chunk], 0);
        rig.run_until("the root", |rig| rig.objects().resident() == 1);
        rig.move_camera(INSIDE_CHUNK_4_4);
        rig.app.update();
        assert_eq!(rig.objects().resident(), 0);
        assert_eq!(rig.root_count(), 0, "the entity is despawned");
        rig.move_camera(RIVERWOOD);
        rig.app.update();
        assert_eq!(rig.root_count(), 1);
    }

    #[test]
    fn an_interior_visit_keeps_the_verified_rows() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        rig.answer(vec![chunk], 0);
        rig.run_until("the root", |rig| rig.objects().resident() == 1);
        // A negative stream radius stands for "no exterior" (as an interior does).
        rig.app
            .world_mut()
            .resource_mut::<EngineConfig>()
            .stream_radius = -1;
        rig.app.update();
        assert_eq!(rig.root_count(), 0, "nothing draws indoors");
        assert_eq!(
            rig.objects().chunks().len(),
            1,
            "the verified rows are kept"
        );
        rig.app
            .world_mut()
            .resource_mut::<EngineConfig>()
            .stream_radius = 2;
        rig.app.update();
        assert_eq!(rig.root_count(), 1, "the root returns");
        assert!(
            rig.requests.try_recv().is_err(),
            "no second row request and no second hash pass"
        );
    }

    #[test]
    fn spawns_wait_for_the_commit_budget() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        rig.app
            .world_mut()
            .resource_mut::<StreamingCommitBudget>()
            .remaining = 0;
        rig.answer(vec![chunk], 0);
        rig.run_until("the verified rows", |rig| {
            !rig.objects().chunks().is_empty()
        });
        rig.app.update();
        assert_eq!(rig.root_count(), 0, "no commit budget, no spawn");
        rig.app
            .world_mut()
            .resource_mut::<StreamingCommitBudget>()
            .remaining = 1;
        rig.app.update();
        assert_eq!(
            rig.root_count(),
            1,
            "the deferred spawn follows when budget returns"
        );
        assert_eq!(
            rig.app.world().resource::<StreamingCommitBudget>().commits,
            1,
            "the spawn counts against the budget"
        );
    }

    #[test]
    fn a_root_spawned_in_the_frame_the_render_origin_shifts_is_rebased_that_frame() {
        let mut rig = rig(true);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        // Start where tier 16 does not draw, so the rows are verified and nothing is spawned.
        rig.move_camera(INSIDE_CHUNK_4_4);
        rig.answer(vec![chunk], 0);
        rig.run_until("the verified rows", |rig| {
            !rig.objects().chunks().is_empty()
        });
        rig.app.update();
        assert_eq!(rig.root_count(), 0);
        let origin_before = rig.app.world().resource::<RenderOrigin>().0;

        // One frame: the camera enters the ring (the system spawns with the old origin) and is
        // far enough from the origin for `update_render_origin` to shift it.
        // The first frames already rebased the origin, so the target is given relative to it: the
        // camera stands in global cell (5, -12), which is `translation = global - origin`.
        rig.move_camera(Vec3::new(
            (5.5 - origin_before.x as f32) * CELL_SIZE,
            0.0,
            (11.5 + origin_before.y as f32) * CELL_SIZE,
        ));
        rig.app.update();
        let origin = rig.app.world().resource::<RenderOrigin>().0;
        assert_ne!(origin, origin_before, "the origin shifted in this frame");
        let mut roots = rig.app.world_mut().query::<(&LodObjectRoot, &Transform)>();
        let found: Vec<_> = roots.iter(rig.app.world()).collect();
        assert_eq!(found.len(), 1, "the root spawned in the same frame");
        // Chunk (4, 4) starts at cell (-32, -32): it must sit where a terrain chunk of that cell
        // sits for the new origin, not for the old one.
        assert_eq!(
            found[0].1.translation,
            Vec3::new(
                (-32 - origin.x) as f32 * CELL_SIZE,
                0.0,
                -((-32 - origin.y) as f32) * CELL_SIZE,
            )
        );
        // The runtime controls are optional: the rig has none, as many headless tests do.
        assert!(rig.app.world().get_resource::<StreamingRuntime>().is_none());
    }
}

#[cfg(test)]
mod shadow_policy_tests {
    use super::*;
    use bevy::{
        asset::{AssetApp, AssetPlugin},
        ecs::system::RunSystemOnce,
        world_serialization::WorldSerializationPlugin,
    };
    use shared::lod::ChunkAnchor;

    /// An object chunk's meshes, however deep, neither receive nor cast; a mesh elsewhere does both.
    #[test]
    fn object_lod_meshes_neither_receive_nor_cast() {
        let mut world = World::new();
        let root = world.spawn_empty().id();
        let node = world.spawn(ChildOf(root)).id();
        let deep = world.spawn((Mesh3d(Handle::default()), ChildOf(node))).id();
        let shallow = world.spawn((Mesh3d(Handle::default()), ChildOf(root))).id();
        let other_root = world.spawn_empty().id();
        let elsewhere = world
            .spawn((Mesh3d(Handle::default()), ChildOf(other_root)))
            .id();
        world
            .run_system_once(
                move |children: Query<&Children>,
                      meshes: Query<(), With<Mesh3d>>,
                      mut commands: Commands| {
                    unshadow_chunk_meshes(root, &children, &meshes, &mut commands);
                },
            )
            .unwrap();
        for mesh in [deep, shallow] {
            let mesh = world.entity(mesh);
            assert!(mesh.contains::<NotShadowCaster>() && mesh.contains::<NotShadowReceiver>());
        }
        let elsewhere = world.entity(elsewhere);
        assert!(!elsewhere.contains::<NotShadowCaster>());
        assert!(!elsewhere.contains::<NotShadowReceiver>());
    }
    /// A converted scene as the loader builds one: a root, a node and a mesh primitive below it.
    fn scene_with_mesh() -> World {
        let mut world = World::new();
        let root = world
            .spawn((Name::new("scene_root"), Transform::default()))
            .id();
        let node = world
            .spawn((Name::new("node"), Transform::default(), ChildOf(root)))
            .id();
        world.spawn((
            Mesh3d(Handle::default()),
            Transform::default(),
            ChildOf(node),
        ));
        world
    }

    /// The root of the hierarchy that `entity` belongs to.
    fn top_of(world: &World, mut entity: Entity) -> Entity {
        while let Some(parent) = world.get::<ChildOf>(entity) {
            entity = parent.parent();
        }
        entity
    }

    /// The registration the plugin makes, driven through the real world-instance spawner: a ready
    /// object chunk scene (a `LodObjectRoot`) has every mesh unshadowed, and a scene that is not
    /// an object chunk keeps casting and receiving.
    #[test]
    fn the_registered_observer_unshadows_object_chunks_and_nothing_else() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            WorldSerializationPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<GlobalTransform>()
        .register_type::<Mesh3d>()
        .register_type::<Name>()
        .register_type::<Transform>();
        crate::streaming::register_lod_shadow_policy(&mut app);
        let assets = app.world().resource::<AssetServer>().clone();
        let chunk_scene = assets.add(WorldAsset::new(scene_with_mesh()));
        let other_scene = assets.add(WorldAsset::new(scene_with_mesh()));
        // The `Loaded` events are applied in the asset schedule, before the spawner reads them.
        app.update();
        let key = ChunkKey::new(60, LodTier::Tier16, ChunkAnchor::new(6, 5));
        let chunk = app
            .world_mut()
            .spawn((LodObjectRoot { key }, WorldAssetRoot(chunk_scene)))
            .id();
        let other = app.world_mut().spawn(WorldAssetRoot(other_scene)).id();
        for _ in 0..4 {
            app.update();
        }
        let world = app.world_mut();
        let meshes: Vec<Entity> = world
            .query_filtered::<Entity, With<Mesh3d>>()
            .iter(world)
            .collect();
        let (mut in_chunk, mut elsewhere) = (0, 0);
        for mesh in meshes {
            let entity = world.entity(mesh);
            let casts = !entity.contains::<NotShadowCaster>();
            let receives = !entity.contains::<NotShadowReceiver>();
            let top = top_of(world, mesh);
            if top == chunk {
                in_chunk += 1;
                // Both markers, not either one: the mesh neither casts nor receives.
                assert!(!casts, "an object chunk mesh still casts");
                assert!(!receives, "an object chunk mesh still receives");
            } else if top == other {
                elsewhere += 1;
                assert!(casts, "a mesh outside an object chunk lost its caster");
                assert!(receives, "a mesh outside an object chunk lost its receiver");
            }
        }
        assert_eq!((in_chunk, elsewhere), (1, 1), "both scenes spawned a mesh");
    }
}
