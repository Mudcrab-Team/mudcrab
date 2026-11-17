//! Streaming of object LOD chunks: the retail `.bto` blocks the converter imports into the
//! additive `lod_object_chunks` table.
//!
//! A pack without that table has no object chunks, so nothing here runs for it. For a pack with
//! it the load fails closed: the rows are validated by [`crate::world::database::load_lod_object_chunks`]
//! and every listed file is hash-checked before any chunk is drawn. A damaged row, a missing file or
//! a wrong hash logs an error, draws no object LOD for that pack and is recorded in
//! [`LodObjects::failure`], never skipped chunk by chunk.
//!
//! Which chunks draw follows the retail object rule (RF-LOD-004): a chunk of tier `t` draws when
//! [`TerrainLodDistances::object_tier`] picks `t` for its cells. A tier-16 chunk is therefore drawn
//! between `fBlockLevel1Distance` and `fBlockMaximumDistance` from the camera. Object chunks do not
//! hand off to full detail yet: the inner tiers (8 and 4) are not published in the first slice.

use super::{
    ActiveSpace, RenderOrigin,
    lod::{LodChunkGridOrigin, chunk_min_grid, lod_worldspace, streaming_center},
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
    prelude::*,
    tasks::{IoTaskPool, Task, block_on},
    world_serialization::{WorldAsset, WorldAssetRoot},
};
use shared::lod::ChunkKey;
use std::{collections::HashMap, path::PathBuf};

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

/// Where the object chunks of the streamed worldspace stand.
#[derive(Resource, Default)]
pub struct LodObjects {
    worldspace_id: Option<u32>,
    generation: u64,
    requested: bool,
    /// Rows read from the database and waiting for their payload check.
    verifying: Option<Task<Result<Vec<LodChunkMetadata>, String>>>,
    /// Chunks that passed validation and the payload check.
    chunks: Vec<LodChunkMetadata>,
    roots: HashMap<ChunkKey, Entity>,
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

    fn reset(&mut self, commands: &mut Commands, worldspace_id: Option<u32>) {
        for (_, root) in self.roots.drain() {
            commands.entity(root).try_despawn();
        }
        self.worldspace_id = worldspace_id;
        self.generation = self.generation.wrapping_add(1);
        self.requested = false;
        self.verifying = None;
        self.chunks.clear();
        self.last_center = None;
        self.last_distances = None;
        self.failure = None;
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

/// The chunks that draw from the camera cell `camera`: those whose cells fall in the tier the
/// object rule picks for them. All cells of a chunk share its box, so the first cell decides.
pub fn wanted_object_chunks(
    chunks: &[LodChunkMetadata],
    camera: IVec2,
    distances: &TerrainLodDistances,
    nama: f32,
) -> Vec<ChunkKey> {
    chunks
        .iter()
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
pub fn update_lod_objects(
    mut commands: Commands,
    config: Res<EngineConfig>,
    space: Option<Res<ActiveSpace>>,
    database: Res<WorldDatabase>,
    origin: Res<RenderOrigin>,
    camera: Query<&Transform, With<StreamingCamera>>,
    asset_server: Res<AssetServer>,
    mut objects: ResMut<LodObjects>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    let worldspace = lod_worldspace(config.stream_radius, space.as_deref(), config.worldspace_id);
    if objects.worldspace_id != worldspace {
        objects.reset(&mut commands, worldspace);
    }
    let Some(worldspace_id) = worldspace else {
        return;
    };
    objects.submit_request(&database, worldspace_id);

    // Rows arrive from the database worker; a failure turns object LOD off for this pack.
    while let Some(response) = database.try_lod_object_response() {
        if response.generation != objects.generation || response.worldspace_id != worldspace_id {
            continue;
        }
        match response.result {
            Ok(chunks) if chunks.is_empty() => {}
            Ok(chunks) => {
                let assets: PathBuf = config.assets_dir.clone();
                objects.verifying = Some(IoTaskPool::get().spawn(async move {
                    verify_lod_object_payloads(&assets, &chunks)
                        .map(|()| chunks)
                        .map_err(|error| format!("{error:#}"))
                }));
            }
            Err(failure) => {
                let reason = format!("object LOD chunk rows are invalid: {}", failure.reason);
                error!("{reason}");
                objects.failure = Some(reason);
            }
        }
    }
    if objects.verifying.as_ref().is_some_and(Task::is_finished) {
        let task = objects.verifying.take().expect("finished task exists");
        match block_on(task) {
            Ok(chunks) => objects.chunks = chunks,
            Err(reason) => {
                let reason = format!("object LOD is off for this pack: {reason}");
                error!("{reason}");
                objects.failure = Some(reason);
            }
        }
    }
    if objects.failure.is_some() || objects.chunks.is_empty() {
        return;
    }

    let center = streaming_center(config.acceptance_screenshot.is_some(), origin.0, camera);
    if !objects.selection_changed(center, &config.terrain_lod) {
        return;
    }
    let wanted: std::collections::HashSet<ChunkKey> = wanted_object_chunks(
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
    for chunk in to_spawn {
        let handle: Handle<WorldAsset> =
            asset_server.load(GltfAssetLabel::Scene(0).from_asset(chunk.payload_path.clone()));
        let root = commands
            .spawn((object_root_bundle(&chunk, origin.0), WorldAssetRoot(handle)))
            .id();
        objects.roots.insert(chunk.key, root);
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

    #[test]
    fn the_default_nama_is_the_retail_one_for_tamriel() {
        assert_eq!(OBJECT_LOD_NAMA, 1.0);
    }
}
