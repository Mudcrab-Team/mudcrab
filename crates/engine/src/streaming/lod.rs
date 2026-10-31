use super::{
    ActiveSpace, LodHorizon, RenderOrigin, StreamingCommitBudget, StreamingMetrics, StreamingWorld,
    TerrainCoverage, TerrainSurfaceReady, requests,
};
use crate::{
    config::{EngineConfig, TerrainLodDistances},
    profiling::ProfilingState,
    world::{
        components::{CELL_SIZE, StreamingCamera},
        database::{LodChunkMetadata, LodChunkQuery, WorldDatabase},
    },
};
use bevy::{
    asset::{AssetLoadError, LoadState, RecursiveDependencyLoadState, io::AssetReaderError},
    gltf::GltfAssetLabel,
    mesh::{Indices, PrimitiveTopology, VertexAttributeValues},
    prelude::*,
    tasks::{IoTaskPool, Task, block_on},
    world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady},
};
use sha2::{Digest, Sha256};
use shared::lod::{
    ChunkKey, LodOrigin, LodTier, TERRAIN_QUADRANT_INDEX_COUNT, TERRAIN_QUADRANT_VERTEX_COUNT,
    nodes,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

mod visibility;
pub(super) use visibility::{
    TerrainLodIndex, terrain_lod_visibility_has_work, update_terrain_lod_visibility,
};

const LOD_UNLOAD_MARGIN_CELLS: i32 = 2;
const LOD_RESPONSE_SCAN_LIMIT: usize = 32;
const LOD_CHUNK_MAX_RETRIES: u8 = 3;
const LOD_CHUNK_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
const LOD_CHUNK_RETRY_MAX_DELAY: Duration = Duration::from_secs(4);

#[path = "terrain_batching.rs"]
pub(crate) mod terrain_batching;

pub(crate) use terrain_batching::LodTerrainBatch;

/// How far distant terrain reaches, in Creation units.
fn lod_horizon(distances: &TerrainLodDistances) -> f32 {
    CELL_SIZE * distances.max_reach_cells().max(0) as f32
}

/// A far plane that keeps every cell within `reach_cells` of the camera's cell.
fn lod_camera_far(reach_cells: i32) -> f32 {
    CELL_SIZE * (reach_cells as f32 + 0.5) * std::f32::consts::SQRT_2
}

/// The active exterior and its main streaming camera's cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LodDemand {
    pub(super) world: u32,
    pub(super) center: IVec2,
}

impl LodDemand {
    pub(super) fn contains_cell(self, world: u32, cell: IVec2, radius: i32) -> bool {
        radius >= 0
            && self.world == world
            && (i64::from(cell.x) - i64::from(self.center.x)).abs() <= i64::from(radius)
            && (i64::from(cell.y) - i64::from(self.center.y)).abs() <= i64::from(radius)
    }
}

#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct LodDemandViews<'w, 's> {
    space: Option<Res<'w, ActiveSpace>>,
    main: Query<'w, 's, &'static Transform, With<StreamingCamera>>,
}

impl LodDemandViews<'_, '_> {
    pub(super) fn current(&self, config: &EngineConfig, origin: IVec2) -> Option<LodDemand> {
        let world = lod_worldspace(
            config.stream_radius,
            self.space.as_deref(),
            config.worldspace_id,
        )?;
        let camera = self.main.single().ok()?;
        Some(LodDemand {
            world,
            center: streaming_center(config.acceptance_screenshot.is_some(), origin, camera),
        })
    }
}

#[derive(Resource, Default)]
pub(super) struct LodStreaming {
    generation: u64,
    center: Option<IVec2>,
    /// Exterior demand used by the current terrain source.
    demand: Option<LodDemand>,
    /// The worldspace `center` belongs to; a door into another worldspace moves the centre too.
    worldspace_id: Option<u32>,
    requested_queries: HashSet<LodTier>,
    pending_queries: HashSet<LodTier>,
    query_retries: HashMap<LodTier, (u8, Option<Instant>)>,
    chunks: HashMap<ChunkKey, LodChunkStatus>,
    pending_chunks: VecDeque<(u64, LodChunkMetadata, Option<u8>)>,
    queued_retry_chunks: HashSet<ChunkKey>,
    build_identity: Option<String>,
    pub(super) visibility_dirty: bool,
    submission_dirty: bool,
    // Exact generational source IDs replaced after the previous selector pass. Both removal
    // streams consume this set once on the next pass, even if one stream has no matching event.
    replaced_source_quadrants: HashSet<Entity>,
    visibility_revision: u64,
    terrain_batch_commit_used: bool,
    terrain_batch_prepared_chunks: u64,
    terrain_batch_activated_chunks: u64,
    terrain_batch_fallback_chunks: u64,
    terrain_batch_pending_cpu_chunks: usize,
    terrain_batch_completion_report: Option<(u64, u64, u64)>,
    terrain_batch_controls_warned: bool,
}

impl LodStreaming {
    fn query_ready(&self, tier: LodTier, now: Instant) -> bool {
        !self.requested_queries.contains(&tier)
            && self
                .query_retries
                .get(&tier)
                .is_none_or(|(_, retry_at)| retry_at.is_some_and(|deadline| now >= deadline))
    }

    fn query_submitted(&mut self, tier: LodTier) {
        self.requested_queries.insert(tier);
        self.pending_queries.insert(tier);
        if let Some((count, retry_at)) = self.query_retries.get_mut(&tier) {
            *count = count.saturating_add(1);
            *retry_at = None;
        }
    }

    fn query_failed(&mut self, tier: LodTier, class: LodChunkFailureClass, now: Instant) {
        self.requested_queries.remove(&tier);
        self.pending_queries.remove(&tier);
        let count = self.query_retries.get(&tier).map_or(0, |(count, _)| *count);
        let retry_at = lod_retry_deadline(class, count, now);
        self.query_retries.insert(tier, (count, retry_at));
    }

    fn has_queued_chunks(&self) -> bool {
        self.pending_chunks
            .iter()
            .any(|(generation, metadata, retry)| {
                *generation == self.generation
                    && (retry.is_some() || !self.chunks.contains_key(&metadata.key))
            })
    }

    /// The chunks waiting for their turn in the unload budget ([`super::despawn_cells`]), in
    /// [`ChunkKey`]'s own order (worldspace, tier, anchor): stable, so a replayed crossing
    /// unloads the same chunks on the same frames whatever order the map iterates in.
    pub(super) fn retiring_chunks(&self) -> Vec<(ChunkKey, Entity)> {
        let mut retiring: Vec<(ChunkKey, Entity)> = self
            .chunks
            .iter()
            .filter_map(|(key, status)| match status {
                LodChunkStatus::Retiring { root, .. } => Some((*key, *root)),
                _ => None,
            })
            .collect();
        retiring.sort_by_key(|(key, _)| *key);
        retiring
    }

    /// Forgets the retiring chunk whose root the unload pacer has just despawned.
    pub(super) fn chunk_despawned(&mut self, key: ChunkKey) {
        self.chunks.remove(&key);
    }

    fn move_center(&mut self, center: IVec2, worldspace_id: u32, distances: &TerrainLodDistances) {
        self.generation = self.generation.wrapping_add(1);
        self.requested_queries.clear();
        self.pending_queries.clear();
        self.query_retries.clear();
        self.center = Some(center);
        self.worldspace_id = Some(worldspace_id);
        self.pending_chunks.retain_mut(|(generation, metadata, _)| {
            let keep = metadata.key.worldspace_id == worldspace_id
                && chunk_within_radius(
                    metadata.key,
                    metadata.origin,
                    (i64::from(center.x), i64::from(center.y)),
                    query_unload_radius(distances.reach_cells(metadata.key.tier)),
                );
            if keep {
                *generation = self.generation;
            }
            keep
        });
        self.queued_retry_chunks = self
            .pending_chunks
            .iter()
            .filter_map(|(_, metadata, retry)| retry.map(|_| metadata.key))
            .collect();
    }
}

#[derive(Clone)]
enum LodChunkStatus {
    Loading {
        root: Entity,
        generation: u64,
        origin: LodOrigin,
    },
    Ready {
        root: Entity,
        origin: LodOrigin,
    },
    /// The chunk has left the keep-set and is waiting for its turn in the unload budget
    /// ([`super::despawn_cells`], which drains cells and LOD chunks under one budget).
    ///
    /// The map entry and the root stay, hidden the frame it retires ([`super::hide_retiring_root`],
    /// the same hide-and-mark a retiring cell gets), so the teardown is spread over frames instead
    /// of landing whole on the frame the camera walks through a door - and a chunk the camera
    /// comes back to is **revived** as it is ([`revive_left_chunk`]) instead of being despawned
    /// and streamed in again. Nothing draws it while it drains: the root is [`Visibility::Hidden`]
    /// the frame it retires, exactly as the cell retire is.
    Retiring {
        root: Entity,
        origin: LodOrigin,
    },
    Failed {
        origin: LodOrigin,
        metadata: LodChunkMetadata,
        retry_count: u8,
        retry_at: Option<Instant>,
    },
}

impl LodChunkStatus {
    fn origin(&self) -> LodOrigin {
        match self {
            Self::Loading { origin, .. }
            | Self::Ready { origin, .. }
            | Self::Retiring { origin, .. }
            | Self::Failed { origin, .. } => *origin,
        }
    }
}

/// What [`leave_keep_set`] did with a chunk that is no longer wanted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChunkLeave {
    /// The entry is gone: a failed chunk, which never drew a root to pace.
    Dropped,
    /// The chunk started leaving this frame and waits for the unload pacer.
    Retiring,
    /// The chunk was leaving already.
    AlreadyLeaving,
}

/// The unload half of the LOD plan: what a chunk that is no longer wanted does with its root.
///
/// It is the cell retire's rule, reused ([`super::hide_retiring_root`]): the root is hidden the
/// frame the chunk leaves - so a paced unload looks exactly like the unpaced one it replaced,
/// off every camera from that frame on - the map entry and the root stay, and the unload pacer
/// ([`super::despawn_cells`]) spreads the teardown over frames under the same budget the cells
/// drain by. A chunk that comes back before its turn is revived, not spawned again
/// ([`revive_left_chunk`]).
///
fn leave_keep_set(
    commands: &mut Commands,
    chunk_visibility: &Query<'_, '_, (&Visibility, Option<&super::RetireHidden>)>,
    status: &mut LodChunkStatus,
) -> ChunkLeave {
    match status {
        LodChunkStatus::Retiring { .. } => ChunkLeave::AlreadyLeaving,
        LodChunkStatus::Loading { root, origin, .. } | LodChunkStatus::Ready { root, origin } => {
            let (root, origin) = (*root, *origin);
            super::hide_retiring_root(commands, chunk_visibility, root);
            *status = LodChunkStatus::Retiring { root, origin };
            ChunkLeave::Retiring
        }
        // Nothing was spawned, so there is no subtree to pace.
        LodChunkStatus::Failed { .. } => ChunkLeave::Dropped,
    }
}

/// A chunk that comes back into the keep-set is revived as it is, the way a cell is: the root it
/// kept draws the chunk again - the same entity, so nothing is spawned a second time - and the
/// chunk is never requested again.
///
/// A chunk whose content was still loading when it left is revived *as loading*: it never showed
/// yet, so the revive leaves the hide in place and [`track_lod_readiness`] shows it when its
/// content is committed. Every other one is ready, and is given back exactly the visibility the
/// leave took ([`super::RetireHidden`]); a root without that marker - one something else hid, or
/// one never shown - is left alone.
///
/// Returns whether `status` was retiring and can come back ([`LodChunkStatus::Retiring`]).
fn revive_left_chunk(
    commands: &mut Commands,
    chunk_visibility: &Query<'_, '_, (&Visibility, Option<&super::RetireHidden>)>,
    chunk_roots: &Query<'_, '_, (&LodChunkRoot, Option<&PendingLodChunk>)>,
    status: &mut LodChunkStatus,
) -> bool {
    let (root, origin) = match *status {
        LodChunkStatus::Retiring { root, origin } => (root, origin),
        _ => return false,
    };
    let (generation, still_loading) =
        chunk_roots.get(root).map_or((0, false), |(root, pending)| {
            (root.generation, pending.is_some())
        });
    if still_loading {
        *status = LodChunkStatus::Loading {
            root,
            generation,
            origin,
        };
        return true;
    }
    *status = LodChunkStatus::Ready { root, origin };
    super::revive_retiring_root(commands, chunk_visibility, root);
    true
}

#[derive(Component)]
pub(crate) struct LodChunkRoot {
    key: ChunkKey,
    generation: u64,
    origin: LodOrigin,
    retry_count: u8,
}

#[derive(Component)]
pub(crate) struct LodChunkGridOrigin {
    pub(super) grid_x: i64,
    pub(super) grid_y: i64,
}

#[cfg(test)]
impl LodChunkGridOrigin {
    /// The origin of a root whose nodes already carry absolute Creation coordinates (an object
    /// LOD block): the render-origin shift places it, with no grid offset of its own.
    pub(super) const fn world_origin() -> Self {
        Self {
            grid_x: 0,
            grid_y: 0,
        }
    }
}

#[derive(Component)]
pub(super) struct PendingLodChunk {
    metadata: LodChunkMetadata,
    asset: Option<Handle<WorldAsset>>,
    scene_spawned: bool,
    hash_task: Option<Task<Result<(), LodChunkFailure>>>,
    hash_verified: bool,
    started: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LodChunkFailureClass {
    Transient,
    Terminal,
}

#[derive(Debug)]
struct LodChunkFailure {
    class: LodChunkFailureClass,
    reason: String,
}

impl LodChunkFailure {
    fn transient(reason: impl Into<String>) -> Self {
        Self {
            class: LodChunkFailureClass::Transient,
            reason: reason.into(),
        }
    }

    fn terminal(reason: impl Into<String>) -> Self {
        Self {
            class: LodChunkFailureClass::Terminal,
            reason: reason.into(),
        }
    }
}

pub(super) fn mark_lod_world_instance_ready(
    ready: On<WorldInstanceReady>,
    mut pending: Query<&mut PendingLodChunk>,
) {
    if let Ok(mut pending) = pending.get_mut(ready.entity) {
        pending.scene_spawned = true;
    }
}

/// The worldspace whose distant terrain should be streamed, or `None` for none at all: a negative
/// stream radius turns LOD off, and an interior has no distant terrain. Otherwise LOD follows the
/// exterior the player is in, which a door may have changed from the configured one.
fn lod_worldspace(stream_radius: i32, space: Option<&ActiveSpace>, configured: u32) -> Option<u32> {
    if stream_radius < 0 || space.is_some_and(|space| space.interior.is_some()) {
        return None;
    }
    Some(space.map_or(configured, |space| space.exterior_worldspace(configured)))
}

/// Whether a resident chunk stays: it must belong to the streamed worldspace and lie within its
/// tier's unload radius of the camera's cell.
fn keep_resident_chunk(
    key: ChunkKey,
    origin: LodOrigin,
    worldspace_id: u32,
    center: (i64, i64),
    distances: &TerrainLodDistances,
) -> bool {
    key.worldspace_id == worldspace_id
        && chunk_within_radius(
            key,
            origin,
            center,
            query_unload_radius(distances.reach_cells(key.tier)),
        )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn scene_pending(world: &World, entity: Entity) -> bool {
    world.get::<PendingLodChunk>(entity).is_some()
}

#[cfg(test)]
pub(super) fn pending_fixture(metadata: LodChunkMetadata) -> PendingLodChunk {
    PendingLodChunk {
        metadata,
        asset: None,
        scene_spawned: false,
        hash_task: None,
        hash_verified: false,
        started: Instant::now(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn plan_lod_chunks(
    commands: Commands,
    config: Res<EngineConfig>,
    database: Res<WorldDatabase>,
    origin: Res<RenderOrigin>,
    views: LodDemandViews,
    chunk_visibility: Query<(&Visibility, Option<&super::RetireHidden>)>,
    chunk_roots: Query<(&LodChunkRoot, Option<&PendingLodChunk>)>,
    mut streaming: ResMut<LodStreaming>,
    mut budget: ResMut<StreamingCommitBudget>,
    metrics: ResMut<StreamingMetrics>,
    profiler: ResMut<ProfilingState>,
    runtime: Option<Res<super::runtime::StreamingRuntime>>,
    mut previous_distances: Local<Option<crate::config::TerrainLodDistances>>,
    mut horizon: Option<ResMut<LodHorizon>>,
    terrain: Option<Res<crate::cluster::terrain::ClusterTerrain>>,
) {
    // Without the ordinary streaming camera, leave the plan and reservation untouched.
    if views.main.single().is_err() {
        return;
    }
    let demand = views.current(&config, origin.0);
    if config.clusters.is_some() {
        if streaming.demand != demand {
            streaming.demand = demand;
        }
        if let (Some(demand), Some(terrain), Some(horizon)) =
            (demand, terrain.as_deref(), horizon.as_mut())
        {
            let reach = lod_horizon(&config.terrain_lod);
            // Certified source availability is indexed; this does not scan the terrain blocks.
            if horizon.reach_in(demand.world) != Some(reach) && terrain.has_world(demand.world) {
                horizon.set(demand.world, reach);
            }
        }
        if budget.reserved_for_lod {
            budget.reserved_for_lod = false;
        }
        return;
    }
    plan_lod_demand_controlled(
        demand,
        commands,
        config,
        database,
        chunk_visibility,
        chunk_roots,
        streaming,
        budget,
        metrics,
        profiler,
        runtime,
        &mut previous_distances,
    );
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn plan_lod_demand(
    demand: Option<LodDemand>,
    commands: Commands,
    config: Res<EngineConfig>,
    database: Res<WorldDatabase>,
    chunk_visibility: Query<(&Visibility, Option<&super::RetireHidden>)>,
    chunk_roots: Query<(&LodChunkRoot, Option<&PendingLodChunk>)>,
    streaming: ResMut<LodStreaming>,
    budget: ResMut<StreamingCommitBudget>,
    metrics: ResMut<StreamingMetrics>,
    profiler: ResMut<ProfilingState>,
    previous_distances: &mut Option<crate::config::TerrainLodDistances>,
) {
    plan_lod_demand_controlled(
        demand,
        commands,
        config,
        database,
        chunk_visibility,
        chunk_roots,
        streaming,
        budget,
        metrics,
        profiler,
        None,
        previous_distances,
    );
}

#[allow(clippy::too_many_arguments)]
fn plan_lod_demand_controlled(
    demand: Option<LodDemand>,
    mut commands: Commands,
    config: Res<EngineConfig>,
    database: Res<WorldDatabase>,
    chunk_visibility: Query<(&Visibility, Option<&super::RetireHidden>)>,
    chunk_roots: Query<(&LodChunkRoot, Option<&PendingLodChunk>)>,
    mut streaming: ResMut<LodStreaming>,
    mut budget: ResMut<StreamingCommitBudget>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
    runtime: Option<Res<super::runtime::StreamingRuntime>>,
    previous_distances: &mut Option<crate::config::TerrainLodDistances>,
) {
    let started = Instant::now();
    let demand = demand.filter(|_| config.clusters.is_none());
    streaming.demand = demand;
    let Some(demand) = demand else {
        if streaming.center.take().is_some() {
            streaming.generation = streaming.generation.wrapping_add(1);
        }
        streaming.worldspace_id = None;
        streaming.requested_queries.clear();
        streaming.pending_queries.clear();
        streaming.query_retries.clear();
        streaming.pending_chunks.clear();
        streaming.queued_retry_chunks.clear();
        streaming.chunks.retain(|_, status| {
            leave_keep_set(&mut commands, &chunk_visibility, status) != ChunkLeave::Dropped
        });
        update_counts(&streaming, &mut metrics, &mut profiler);
        profiler.record_elapsed("lod/plan", started);
        return;
    };

    let worldspace_id = demand.world;
    let center = demand.center;
    let distances_changed = previous_distances.as_ref() != Some(&config.terrain_lod);
    *previous_distances = Some(config.terrain_lod);
    if distances_changed {
        // Resident patches keep their chunks when a distance changes, so no load or removal event
        // would otherwise re-select their tier while the camera stands still.
        streaming.visibility_dirty = true;
    }
    if streaming.center != Some(center)
        || streaming.worldspace_id != Some(worldspace_id)
        || distances_changed
    {
        streaming.move_center(center, worldspace_id, &config.terrain_lod);
    }
    let generation = streaming.generation;
    for tier in LodTier::ALL {
        if runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.cell_requests_allowed())
        {
            break;
        }
        if !streaming.query_ready(tier, Instant::now()) {
            continue;
        }
        let query = query_for(
            worldspace_id,
            tier,
            center,
            query_unload_radius(config.terrain_lod.reach_cells(tier)),
        );
        match database.request_lod_chunks(generation, query) {
            Ok(()) => {
                streaming.query_submitted(tier);
                metrics.lod_queries_submitted = metrics.lod_queries_submitted.saturating_add(1);
                profiler.increment("lod/queries_submitted", 1);
            }
            Err(error) => {
                metrics.lod_query_submission_failures =
                    metrics.lod_query_submission_failures.saturating_add(1);
                debug!(?tier, %error, "terrain LOD query will be retried");
            }
        }
    }
    metrics.pending_lod_queries = streaming.pending_queries.len();

    let center64 = (i64::from(center.x), i64::from(center.y));
    streaming.chunks.retain(|key, status| {
        let keep = keep_resident_chunk(
            *key,
            status.origin(),
            worldspace_id,
            center64,
            &config.terrain_lod,
        );
        if keep {
            revive_left_chunk(&mut commands, &chunk_visibility, &chunk_roots, status);
            return true;
        }
        leave_keep_set(&mut commands, &chunk_visibility, status) != ChunkLeave::Dropped
    });
    let generation = streaming.generation;
    streaming
        .pending_chunks
        .retain(|(queued_generation, _, _)| *queued_generation == generation);
    let resident_keys: HashSet<_> = streaming.chunks.keys().copied().collect();
    streaming
        .queued_retry_chunks
        .retain(|key| resident_keys.contains(key));
    enqueue_due_lod_retries(&mut streaming, Instant::now());
    budget.reserve_for_lod(
        streaming.has_queued_chunks(),
        runtime
            .as_ref()
            .and_then(|runtime| runtime.decision)
            .map_or(config.max_cell_commits_per_frame, |decision| {
                decision.budgets.max_cell_commits
            }),
    );
    update_counts(&streaming, &mut metrics, &mut profiler);
    profiler.record_elapsed("lod/plan", started);
}

fn widen_lod_cameras(
    demand: Option<LodDemand>,
    config: &EngineConfig,
    horizon: &LodHorizon,
    camera_projection: &mut Query<&mut Projection, With<StreamingCamera>>,
) {
    if let Some(demand) = demand
        && horizon.reach_in(demand.world).is_some()
        && let Ok(mut projection) = camera_projection.single_mut()
        && let Projection::Perspective(perspective) = &mut *projection
    {
        perspective.far = perspective
            .far
            .max(lod_camera_far(config.terrain_lod.max_reach_cells()));
    }
}

/// Collect LOD query results and request current chunks within the shared commit budget.
#[allow(clippy::too_many_arguments)]
pub(super) fn collect_lod_chunks(
    mut commands: Commands,
    config: Res<EngineConfig>,
    database: Res<WorldDatabase>,
    asset_server: Res<AssetServer>,
    origin: Res<RenderOrigin>,
    mut camera_projection: Query<&mut Projection, With<StreamingCamera>>,
    mut full_streaming: ResMut<StreamingWorld>,
    mut streaming: ResMut<LodStreaming>,
    mut budget: ResMut<StreamingCommitBudget>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
    runtime: Option<Res<super::runtime::StreamingRuntime>>,
    mut horizon: Option<ResMut<LodHorizon>>,
) {
    if config.clusters.is_some() {
        if let Some(horizon) = horizon.as_deref() {
            widen_lod_cameras(streaming.demand, &config, horizon, &mut camera_projection);
        }
        return;
    }
    let started = Instant::now();
    for _ in 0..LOD_RESPONSE_SCAN_LIMIT {
        let Some(response) = database.try_lod_response() else {
            break;
        };
        if response.generation == streaming.generation {
            streaming.pending_queries.remove(&response.query.tier);
        }
        metrics.lod_query_responses = metrics.lod_query_responses.saturating_add(1);
        metrics.total_lod_query_micros = metrics
            .total_lod_query_micros
            .saturating_add(response.query_micros);
        metrics.max_lod_query_micros = metrics.max_lod_query_micros.max(response.query_micros);
        profiler.record_completed_latency_micros("lod/db_queue_wait", response.queue_wait_micros);
        profiler.record_completed_latency_micros("lod/db_query", response.query_micros);
        profiler
            .record_completed_latency_micros("lod/db_request_total", response.total_request_micros);

        if !is_current_query(&streaming, response.generation) {
            metrics.stale_lod_query_responses = metrics.stale_lod_query_responses.saturating_add(1);
            profiler.increment("lod/stale_query_responses", 1);
            continue;
        }
        match response.result {
            Err(error) => {
                metrics.failed_lod_queries = metrics.failed_lod_queries.saturating_add(1);
                let class = if error.transient {
                    LodChunkFailureClass::Transient
                } else {
                    LodChunkFailureClass::Terminal
                };
                streaming.query_failed(response.query.tier, class, Instant::now());
                warn!(?response.query.tier, %error, "terrain LOD query failed");
            }
            Ok(chunks) => {
                streaming.query_retries.remove(&response.query.tier);
                if !chunks.is_empty()
                    && let Some(horizon) = horizon.as_mut()
                {
                    let reach = lod_horizon(&config.terrain_lod);
                    if horizon.reach_in(response.query.worldspace_id) != Some(reach) {
                        horizon.set(response.query.worldspace_id, reach);
                    }
                }
                if !chunks.is_empty()
                    && let Ok(mut projection) = camera_projection.single_mut()
                    && let Projection::Perspective(perspective) = &mut *projection
                {
                    perspective.far = perspective
                        .far
                        .max(lod_camera_far(config.terrain_lod.max_reach_cells()));
                }
                streaming.pending_chunks.extend(
                    chunks
                        .into_iter()
                        .map(|metadata| (response.generation, metadata, None)),
                );
            }
        }
    }

    if config.prioritize_streaming
        && let Some(center) = streaming.center
    {
        streaming
            .pending_chunks
            .make_contiguous()
            .sort_by_key(|(_, metadata, _)| {
                let min = chunk_min_grid(metadata.key, metadata.origin);
                let side = i64::from(metadata.key.tier.side_cells());
                let dx = min.0 + side / 2 - i64::from(center.x);
                let dy = min.1 + side / 2 - i64::from(center.y);
                (
                    i128::from(dx) * i128::from(dx) + i128::from(dy) * i128::from(dy),
                    metadata.key.tier,
                    metadata.key.anchor.y,
                    metadata.key.anchor.x,
                )
            });
    }
    while budget.remaining > 0 {
        if budget.elapsed_limit_reached(runtime.as_deref()) {
            break;
        }
        let Some((generation, metadata, retry_attempt)) = streaming.pending_chunks.pop_front()
        else {
            break;
        };
        if retry_attempt.is_some() {
            streaming.queued_retry_chunks.remove(&metadata.key);
        }
        if generation != streaming.generation || streaming.center.is_none() {
            metrics.stale_lod_query_responses = metrics.stale_lod_query_responses.saturating_add(1);
            continue;
        }
        let center = streaming.center.unwrap();
        if !chunk_within_radius(
            metadata.key,
            metadata.origin,
            (i64::from(center.x), i64::from(center.y)),
            query_unload_radius(config.terrain_lod.reach_cells(metadata.key.tier)),
        ) {
            continue;
        }
        let retry_count = match retry_attempt {
            Some(retry_attempt) => {
                let Some(LodChunkStatus::Failed {
                    retry_count,
                    retry_at: Some(retry_at),
                    ..
                }) = streaming.chunks.get(&metadata.key)
                else {
                    continue;
                };
                if retry_count.saturating_add(1) != retry_attempt || Instant::now() < *retry_at {
                    continue;
                }
                retry_attempt
            }
            None => {
                if streaming.chunks.contains_key(&metadata.key) {
                    continue;
                }
                0
            }
        };
        if let Some(identity) = &streaming.build_identity {
            if identity != &metadata.build_identity {
                metrics.failed_lod_chunks = metrics.failed_lod_chunks.saturating_add(1);
                error!(
                    ?metadata.key,
                    expected = identity,
                    actual = metadata.build_identity,
                    "terrain LOD chunk belongs to a different asset build"
                );
                streaming.chunks.insert(
                    metadata.key,
                    LodChunkStatus::Failed {
                        origin: metadata.origin,
                        metadata,
                        retry_count: 0,
                        retry_at: None,
                    },
                );
                continue;
            }
        } else {
            streaming.build_identity = Some(metadata.build_identity.clone());
        }

        let key = metadata.key;
        let chunk_origin = metadata.origin;
        let min_grid = chunk_min_grid(key, metadata.origin);
        let schedule_scenes = requests::enabled(&config);
        let asset = if schedule_scenes {
            None
        } else {
            let scene_path = GltfAssetLabel::Scene(0).from_asset(metadata.payload_path.clone());
            // Bevy's load request restarts an asset whose cached load state is Failed.
            let handle = asset_server.load(scene_path);
            profiler.observe_scene(handle.id(), &asset_server);
            Some(handle)
        };
        let hash_task = if schedule_scenes {
            None
        } else {
            let expected_hash = metadata.content_hash.clone();
            let hash_path = config.assets_dir.join(&metadata.payload_path);
            Some(
                IoTaskPool::get()
                    .spawn(async move { verify_payload_hash(hash_path, expected_hash) }),
            )
        };
        let request = schedule_scenes.then(|| {
            use bevy::math::DVec3;
            let sequence = full_streaming.next_model_sequence;
            full_streaming.next_model_sequence = sequence.saturating_add(1);
            let side = f64::from(metadata.key.tier.side_cells()) * f64::from(CELL_SIZE);
            requests::SceneRequest {
                path: metadata.payload_path.clone(),
                cell: None,
                coarse_terrain: true,
                retry_cleanup: None,
                content_identity: Some(metadata.content_hash.clone()),
                sequence,
                center: DVec3::new(
                    min_grid.0 as f64 * f64::from(CELL_SIZE) + side * 0.5,
                    0.0,
                    -(min_grid.1 as f64 * f64::from(CELL_SIZE) + side * 0.5),
                ),
                radius: Some(side * std::f64::consts::SQRT_2 * 0.5),
                collision_candidate: false,
                handle: None,
                started: Instant::now(),
                allow_retry: retry_count > 0,
            }
        });
        let local_grid_x = min_grid.0 - i64::from(origin.0.x);
        let local_grid_y = min_grid.1 - i64::from(origin.0.y);
        let root = commands
            .spawn((
                Name::new(format!(
                    "LOD {:?} chunk {},{}",
                    key.tier, key.anchor.x, key.anchor.y
                )),
                LodChunkRoot {
                    key,
                    generation,
                    origin: metadata.origin,
                    retry_count,
                },
                LodChunkGridOrigin {
                    grid_x: min_grid.0,
                    grid_y: min_grid.1,
                },
                Transform::from_xyz(
                    local_grid_x as f32 * CELL_SIZE,
                    0.0,
                    -(local_grid_y as f32) * CELL_SIZE,
                ),
                Visibility::Hidden,
                PendingLodChunk {
                    metadata,
                    asset: asset.clone(),
                    scene_spawned: false,
                    hash_task,
                    hash_verified: false,
                    started: Instant::now(),
                },
            ))
            .id();
        if let Some(handle) = asset {
            commands.entity(root).insert(WorldAssetRoot(handle));
        }
        if let Some(request) = request {
            commands.entity(root).insert(request);
        }
        streaming.chunks.insert(
            key,
            LodChunkStatus::Loading {
                root,
                generation,
                origin: chunk_origin,
            },
        );
        budget.remaining -= 1;
        budget.commits = budget.commits.saturating_add(1);
        metrics.lod_chunks_requested = metrics.lod_chunks_requested.saturating_add(1);
        profiler.increment("lod/chunks_requested", 1);
        profiler.event(format!("{key:?}"), "requested", None);
    }
    update_counts(&streaming, &mut metrics, &mut profiler);
    profiler.record_elapsed("lod/collect", started);
}

/// Start the existing integrity check only after this chunk's shared scene job is admitted.
pub(super) fn start_admitted_chunk(world: &mut World, entity: Entity, handle: Handle<WorldAsset>) {
    let assets = world.resource::<EngineConfig>().assets_dir.clone();
    let Some(mut pending) = world.get_mut::<PendingLodChunk>(entity) else {
        return;
    };
    if pending.asset.is_some() {
        return;
    }
    let path = assets.join(&pending.metadata.payload_path);
    let expected_hash = pending.metadata.content_hash.clone();
    pending.asset = Some(handle);
    pending.hash_task =
        Some(IoTaskPool::get().spawn(async move { verify_payload_hash(path, expected_hash) }));
}

/// Validate loaded LOD chunks and record completion latency or schedule retries for failures.
#[allow(clippy::too_many_arguments)]
pub(super) fn track_lod_readiness(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    world_assets: Res<Assets<WorldAsset>>,
    meshes: Res<Assets<Mesh>>,
    children: Query<&Children>,
    names: Query<&Name>,
    mesh_handles: Query<&Mesh3d>,
    mut pending_chunks: Query<(Entity, &LodChunkRoot, &mut PendingLodChunk)>,
    mut streaming: ResMut<LodStreaming>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    let started = Instant::now();
    for (entity, root, mut pending) in &mut pending_chunks {
        let Some(asset) = pending.asset.clone() else {
            continue;
        };
        let load_failure =
            asset_server
                .get_load_states(asset.id())
                .and_then(|(load, _, recursive)| match (load, recursive) {
                    (LoadState::Failed(error), _) => Some(classify_asset_load_failure(&error)),
                    (_, RecursiveDependencyLoadState::Failed(error)) => {
                        Some(classify_asset_load_failure(&error))
                    }
                    _ => None,
                });
        if let Some(failure) = load_failure {
            fail_chunk(
                (entity, root),
                &pending.metadata,
                failure,
                &mut commands,
                &mut streaming,
                &mut metrics,
                &mut profiler,
            );
            continue;
        }

        if !pending.hash_verified {
            let Some(task) = pending.hash_task.as_ref() else {
                continue;
            };
            if !task.is_finished() {
                continue;
            }
            let task = pending.hash_task.take().expect("finished hash task exists");
            match block_on(task) {
                Ok(()) => pending.hash_verified = true,
                Err(reason) => {
                    fail_chunk(
                        (entity, root),
                        &pending.metadata,
                        reason,
                        &mut commands,
                        &mut streaming,
                        &mut metrics,
                        &mut profiler,
                    );
                    continue;
                }
            }
        }

        if !pending.scene_spawned
            || !asset_server.is_loaded_with_dependencies(asset.id())
            || world_assets.get(&asset).is_none()
        {
            continue;
        }

        let patches = match validate_lod_scene(
            entity,
            &pending.metadata,
            &children,
            &names,
            &mesh_handles,
            &meshes,
        ) {
            Ok(patches) => patches,
            Err(reason) => {
                fail_chunk(
                    (entity, root),
                    &pending.metadata,
                    LodChunkFailure::terminal(reason),
                    &mut commands,
                    &mut streaming,
                    &mut metrics,
                    &mut profiler,
                );
                continue;
            }
        };
        for (patch, coverage) in &patches {
            commands
                .entity(*patch)
                .insert((*coverage, TerrainSurfaceReady, Visibility::Hidden));
        }
        finish_lod_chunk(&mut commands, &mut streaming, entity, root);
        metrics.lod_chunks_ready = metrics.lod_chunks_ready.saturating_add(1);
        metrics.ready_lod_terrain_patches = metrics
            .ready_lod_terrain_patches
            .saturating_add(patches.len() as u64);
        profiler.increment("lod/chunks_ready", 1);
        profiler.increment("lod/terrain_patches_ready", patches.len() as u64);
        profiler.record_completed_latency_elapsed("lod/chunk_ready", pending.started);
        profiler.event(format!("{:?}", root.key), "ready", None);
        commands
            .entity(entity)
            .insert(terrain_batching::PendingLodTerrainBatching {
                source: asset,
                patches,
            });
    }
    update_counts(&streaming, &mut metrics, &mut profiler);
    profiler.record_elapsed("lod/readiness", started);
}

/// Batching shares the streaming commit budget. Until its turn, a ready chunk keeps drawing its
/// original quadrant meshes. One chunk is the smallest atomic conversion and can exceed the time
/// allowance; its actual work is charged and reported before the frame's budget is finalized.
#[allow(clippy::too_many_arguments)]
pub(super) fn batch_ready_lod_chunks(
    mut commands: Commands,
    config: Res<EngineConfig>,
    mut budget: ResMut<StreamingCommitBudget>,
    mut meshes: ResMut<Assets<Mesh>>,
    preparation: terrain_batching::LodBatchPreparation,
    activation: terrain_batching::LodBatchActivation,
    children: Query<&Children>,
    names: Query<&Name>,
    mesh_handles: Query<&Mesh3d>,
    pending: Query<(
        Entity,
        &LodChunkRoot,
        &terrain_batching::PendingLodTerrainBatching,
    )>,
    mut streaming: ResMut<LodStreaming>,
    mut profiler: ResMut<ProfilingState>,
) {
    streaming.terrain_batch_commit_used = false;
    let pending_cpu = pending.iter().len();
    let pending_gpu = activation.pending_count();
    // Derived batches retain source scene assets and allocate additional meshes outside the
    // scene-admission ledger. While request controls are enabled, the original hierarchy stays
    // active and no batch is activated; default-mode batching is unchanged.
    if requests::enabled(&config) {
        if pending_cpu != 0 && !streaming.terrain_batch_controls_warned {
            warn!(
                "terrain LOD batching disabled with streaming admission/control limits; original meshes remain active until generated retained-byte reservations are supported"
            );
            streaming.terrain_batch_controls_warned = true;
        }
        for (entity, _, _) in &pending {
            commands
                .entity(entity)
                .remove::<terrain_batching::PendingLodTerrainBatching>();
        }
        record_terrain_batch_progress(&mut streaming, &mut profiler, 0, pending_gpu);
        return;
    }
    record_terrain_batch_progress(&mut streaming, &mut profiler, pending_cpu, pending_gpu);
    if !preparation.has_render_backend() {
        for (entity, _, _) in &pending {
            commands
                .entity(entity)
                .remove::<terrain_batching::PendingLodTerrainBatching>();
        }
        return;
    }
    if !terrain_batch_budget_available(&budget, config.max_commit_micros_per_frame) {
        return;
    }
    // The generation and key ordering gives a stable oldest-first cadence for arrived chunks.
    let Some((entity, root, queued)) = pending
        .iter()
        .min_by_key(|(_, root, _)| (root.generation, root.key))
    else {
        return;
    };
    let started = Instant::now();
    match preparation.prepare(
        entity,
        &queued.patches,
        &children,
        &names,
        &mesh_handles,
        &meshes,
    ) {
        Ok(batches) => {
            streaming.terrain_batch_prepared_chunks += 1;
            profiler.increment("lod/terrain_batch_chunks_prepared", 1);
            profiler.increment("lod/terrain_batches_created", batches.len() as u64);
            terrain_batching::install(
                entity,
                queued.source.clone(),
                batches,
                &children,
                &mut commands,
                &mut meshes,
                preparation.upload_readiness(),
            );
            streaming.submission_dirty = true;
        }
        Err(reason) => {
            streaming.terrain_batch_fallback_chunks += 1;
            debug!(?root.key, %reason, "terrain LOD keeps its original scene hierarchy");
            profiler.increment("lod/terrain_batch_fallback_chunks", 1);
        }
    }
    commands
        .entity(entity)
        .remove::<terrain_batching::PendingLodTerrainBatching>();
    budget.remaining -= 1;
    budget.commits = budget.commits.saturating_add(1);
    streaming.terrain_batch_commit_used = true;
    profiler.increment("lod/terrain_batch_chunks_committed", 1);
    profiler.record_elapsed("lod/terrain_batch_commit", started);
}

fn record_terrain_batch_progress(
    streaming: &mut LodStreaming,
    profiler: &mut ProfilingState,
    pending_cpu: usize,
    pending_gpu: usize,
) {
    streaming.terrain_batch_pending_cpu_chunks = pending_cpu;
    profiler.set_gauge("lod/pending_terrain_batch_chunks", pending_cpu as f64);
    profiler.set_gauge(
        "lod/pending_initial_terrain_upload_chunks",
        pending_gpu as f64,
    );
    let report = (
        streaming.terrain_batch_prepared_chunks,
        streaming.terrain_batch_activated_chunks,
        streaming.terrain_batch_fallback_chunks,
    );
    if pending_cpu == 0
        && pending_gpu == 0
        && report.0 != 0
        && streaming.terrain_batch_completion_report != Some(report)
    {
        streaming.terrain_batch_completion_report = Some(report);
        info!(
            prepared_chunks = report.0,
            activated_chunks = report.1,
            fallback_chunks = report.2,
            "terrain LOD batching queue drained; current initial replacements are GPU-ready"
        );
        profiler.event(
            "terrain LOD batching",
            "initial_uploads_drained",
            Some(report.1 as f64),
        );
    }
}

fn terrain_batch_budget_available(budget: &StreamingCommitBudget, max_micros: u64) -> bool {
    budget.remaining != 0 && budget.frame_started.elapsed().as_micros() < u128::from(max_micros)
}

fn terrain_handover_budget_available(budget: &StreamingCommitBudget, max_micros: u64) -> bool {
    budget.remaining != 0
        && !(budget.commits > 0
            && budget.frame_started.elapsed().as_micros() >= u128::from(max_micros))
}

fn finish_lod_chunk(
    commands: &mut Commands,
    streaming: &mut LodStreaming,
    entity: Entity,
    root: &LodChunkRoot,
) {
    let status = streaming.chunks.get(&root.key).cloned();
    match status {
        Some(LodChunkStatus::Loading {
            root: expected_root,
            generation,
            origin,
        }) if expected_root == entity && generation == root.generation => {
            streaming.chunks.insert(
                root.key,
                LodChunkStatus::Ready {
                    root: entity,
                    origin,
                },
            );
            commands.entity(entity).insert(Visibility::Inherited);
        }
        Some(LodChunkStatus::Retiring {
            root: expected_root,
            ..
        }) if expected_root == entity => {
            commands.entity(entity).try_insert((
                super::RetireHidden(Visibility::Inherited),
                Visibility::Hidden,
            ));
        }
        // Preserve the existing completion behavior for an untracked root.
        _ => {
            commands.entity(entity).insert(Visibility::Inherited);
        }
    }
    commands.entity(entity).remove::<PendingLodChunk>();
}

fn fail_chunk(
    chunk: (Entity, &LodChunkRoot),
    metadata: &LodChunkMetadata,
    failure: LodChunkFailure,
    commands: &mut Commands,
    streaming: &mut LodStreaming,
    metrics: &mut StreamingMetrics,
    profiler: &mut ProfilingState,
) {
    let (entity, root) = chunk;
    let retry_at = lod_retry_deadline(failure.class, root.retry_count, Instant::now());
    warn!(
        ?root.key,
        class = ?failure.class,
        reason = %failure.reason,
        retrying = retry_at.is_some(),
        "terrain LOD chunk failed validation or loading"
    );
    metrics.failed_lod_chunks = metrics.failed_lod_chunks.saturating_add(1);
    profiler.increment("lod/chunks_failed", 1);
    profiler.event(format!("{:?}", root.key), "failed", None);
    match streaming.chunks.get(&root.key).cloned() {
        // The chunk the plan still wants: the failure is recorded - a transient one is retried on
        // its own clock - and the root, which would never draw now, goes.
        Some(LodChunkStatus::Loading {
            root: expected_root,
            generation,
            ..
        }) if expected_root == entity && generation == root.generation => {
            streaming.chunks.insert(
                root.key,
                LodChunkStatus::Failed {
                    origin: root.origin,
                    metadata: metadata.clone(),
                    retry_count: root.retry_count,
                    retry_at,
                },
            );
            commands.entity(entity).try_despawn();
        }
        // A chunk that is already leaving: its root is the unload pacer's, not this function's -
        // despawning it here would put a single despawn back on the frame the retire exists to
        // keep off it.
        Some(LodChunkStatus::Retiring {
            root: expected_root,
            ..
        }) if expected_root == entity => {}
        _ => {
            commands.entity(entity).try_despawn();
        }
    }
}

fn validate_lod_scene(
    root: Entity,
    metadata: &LodChunkMetadata,
    children: &Query<&Children>,
    names: &Query<&Name>,
    mesh_handles: &Query<&Mesh3d>,
    meshes: &Assets<Mesh>,
) -> Result<Vec<(Entity, TerrainCoverage)>, String> {
    let key = metadata.key;
    let source_cells: HashSet<_> = metadata
        .source_cells
        .iter()
        .map(|[x, y]| IVec2::new(*x, *y))
        .collect();
    if source_cells.is_empty() || source_cells.len() != metadata.source_cells.len() {
        return Err(format!(
            "{key:?} has empty or duplicate source-cell metadata"
        ));
    }
    if source_cells
        .iter()
        .any(|cell| !chunk_contains_cell(key, metadata.origin, *cell))
    {
        return Err(format!(
            "{key:?} source-cell metadata lies outside its chunk"
        ));
    }

    let mut stack = vec![(root, None)];
    let mut seen = HashSet::new();
    let mut patches = Vec::with_capacity(source_cells.len() * 4);
    while let Some((entity, inherited_grid)) = stack.pop() {
        let name = names.get(entity).ok().map(Name::as_str);
        let source_grid = match name.filter(|name| name.starts_with("cell_")) {
            Some(name) => {
                let Some((x, y)) = nodes::parse_source_cell(name) else {
                    return Err(format!("{key:?} has malformed source-cell node {name:?}"));
                };
                let grid = IVec2::new(x, y);
                if !source_cells.contains(&grid) {
                    return Err(format!("{key:?} has unexpected source cell {grid:?}"));
                }
                Some(grid)
            }
            None => inherited_grid,
        };
        if let Some(name) = name.filter(|name| name.starts_with("terrain_quadrant_")) {
            let quadrant = quadrant_from_node_name(name)
                .ok_or_else(|| format!("{key:?} has unknown terrain quadrant node {name:?}"))?;
            let grid = source_grid
                .ok_or_else(|| format!("{key:?} quadrant node has no source-cell parent"))?;
            // Bevy's glTF loader spawns each mesh primitive as a child of the
            // glTF node entity, not on the node itself: the quadrant node
            // carries the name, its single primitive child carries the Mesh3d.
            let primitive_children: Vec<Entity> = children
                .get(entity)
                .map(|direct| {
                    direct
                        .iter()
                        .filter(|child| mesh_handles.contains(*child))
                        .collect()
                })
                .unwrap_or_default();
            let [primitive] = primitive_children.as_slice() else {
                return Err(format!(
                    "{key:?} quadrant node {name:?} has {} mesh primitives, expected 1",
                    primitive_children.len()
                ));
            };
            let mesh_handle = mesh_handles
                .get(*primitive)
                .map_err(|_| format!("{key:?} quadrant node {name:?} has no mesh"))?;
            let mesh = meshes
                .get(&mesh_handle.0)
                .ok_or_else(|| format!("{key:?} quadrant node {name:?} mesh is missing"))?;
            validate_lod_quadrant_mesh(mesh)
                .map_err(|reason| format!("{key:?} {grid:?} quadrant {quadrant}: {reason}"))?;
            if !seen.insert((grid, quadrant)) {
                return Err(format!("{key:?} duplicates {grid:?} quadrant {quadrant}"));
            }
            patches.push((
                entity,
                TerrainCoverage {
                    grid,
                    quadrant,
                    tier: Some(key.tier),
                },
            ));
        }
        if let Ok(direct_children) = children.get(entity) {
            for child in direct_children.iter() {
                stack.push((child, source_grid));
            }
        }
    }

    if seen.len() != source_cells.len() * 4 {
        return Err(format!(
            "{key:?} contains {} terrain quadrants for {} source cells; expected {}",
            seen.len(),
            source_cells.len(),
            source_cells.len() * 4
        ));
    }
    Ok(patches)
}

fn validate_lod_quadrant_mesh(mesh: &Mesh) -> Result<(), String> {
    if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
        return Err("mesh is not a triangle list".to_owned());
    }
    let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
        Some(VertexAttributeValues::Float32x3(values))
            if values.len() == TERRAIN_QUADRANT_VERTEX_COUNT =>
        {
            values
        }
        _ => return Err("mesh has an invalid Float32x3 position count".to_owned()),
    };
    if !positions.iter().flatten().all(|value| value.is_finite()) {
        return Err("mesh positions contain non-finite values".to_owned());
    }
    match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
        Some(VertexAttributeValues::Float32x3(values))
            if values.len() == positions.len()
                && values.iter().flatten().all(|value| value.is_finite()) => {}
        _ => return Err("mesh normals do not match its positions".to_owned()),
    }
    match mesh.attribute(Mesh::ATTRIBUTE_UV_0) {
        Some(VertexAttributeValues::Float32x2(values))
            if values.len() == positions.len()
                && values
                    .iter()
                    .flatten()
                    .all(|value| value.is_finite() && (0.0..=1.0).contains(value)) => {}
        _ => return Err("mesh UVs are missing, non-finite, or outside 0..1".to_owned()),
    }
    match mesh.attribute(Mesh::ATTRIBUTE_COLOR) {
        Some(VertexAttributeValues::Float32x4(values))
            if values.len() == positions.len()
                && values
                    .iter()
                    .flatten()
                    .all(|value| value.is_finite() && (0.0..=1.0).contains(value)) => {}
        _ => return Err("mesh colors are missing, non-finite, or outside 0..1".to_owned()),
    }
    let index_count = match mesh.indices() {
        Some(Indices::U16(indices)) if indices.len() == TERRAIN_QUADRANT_INDEX_COUNT => {
            if indices
                .iter()
                .any(|index| usize::from(*index) >= positions.len())
            {
                return Err("mesh has an out-of-range index".to_owned());
            }
            indices.len()
        }
        Some(Indices::U32(indices)) if indices.len() == TERRAIN_QUADRANT_INDEX_COUNT => {
            if indices
                .iter()
                .any(|index| *index as usize >= positions.len())
            {
                return Err("mesh has an out-of-range index".to_owned());
            }
            indices.len()
        }
        _ => return Err("mesh has an invalid triangle index count".to_owned()),
    };
    if index_count % 3 != 0 {
        return Err("mesh indices do not form complete triangles".to_owned());
    }
    Ok(())
}

/// The finest ready tier whose configured reach covers the cell, or `None` once
/// full detail is drawable or the cell is beyond every reach.
/// The finest ready tier whose configured reach covers the cell, or `None` once
/// full detail is drawable or the cell is beyond every reach.
fn select_ready_terrain_lod_tier(
    grid_distance: i32,
    full_detail_ready: bool,
    is_available: impl Fn(LodTier) -> bool,
    distances: &TerrainLodDistances,
) -> Option<LodTier> {
    if full_detail_ready {
        return None;
    }
    LodTier::ALL
        .into_iter()
        .find(|tier| grid_distance <= distances.reach_cells(*tier) && is_available(*tier))
}

/// [`select_ready_terrain_lod_tier`] over a set of ready tiers.
#[cfg(test)]
fn select_terrain_lod_tier(
    grid_distance: i32,
    full_detail_ready: bool,
    available: impl TestTerrainAvailability,
    distances: &TerrainLodDistances,
) -> Option<LodTier> {
    select_ready_terrain_lod_tier(
        grid_distance,
        full_detail_ready,
        |tier| available.has(tier),
        distances,
    )
}
#[cfg(test)]
trait TestTerrainAvailability {
    fn has(&self, tier: LodTier) -> bool;
}
#[cfg(test)]
impl TestTerrainAvailability for u8 {
    fn has(&self, tier: LodTier) -> bool {
        self & tier as u8 != 0
    }
}
#[cfg(test)]
impl TestTerrainAvailability for &HashSet<LodTier> {
    fn has(&self, tier: LodTier) -> bool {
        self.contains(&tier)
    }
}

fn chebyshev_grid_distance(left: IVec2, right: IVec2) -> i32 {
    left.x
        .saturating_sub(right.x)
        .saturating_abs()
        .max(left.y.saturating_sub(right.y).saturating_abs())
}

fn quadrant_from_node_name(name: &str) -> Option<u8> {
    let direction = name.strip_prefix("terrain_quadrant_")?;
    ["sw", "se", "nw", "ne"]
        .iter()
        .position(|candidate| *candidate == direction)
        .map(|index| index as u8)
}

fn streaming_center(screenshot: bool, origin: IVec2, camera: &Transform) -> IVec2 {
    if screenshot {
        return origin;
    }
    let global_x = camera.translation.x + origin.x as f32 * CELL_SIZE;
    let global_y = -camera.translation.z + origin.y as f32 * CELL_SIZE;
    IVec2::new(
        (global_x / CELL_SIZE).floor() as i32,
        (global_y / CELL_SIZE).floor() as i32,
    )
}

fn query_for(worldspace_id: u32, tier: LodTier, center: IVec2, radius: i32) -> LodChunkQuery {
    let min_x = (i64::from(center.x) - i64::from(radius)) as f64 * f64::from(CELL_SIZE);
    let min_y = (i64::from(center.y) - i64::from(radius)) as f64 * f64::from(CELL_SIZE);
    let max_x = (i64::from(center.x) + i64::from(radius) + 1) as f64 * f64::from(CELL_SIZE);
    let max_y = (i64::from(center.y) + i64::from(radius) + 1) as f64 * f64::from(CELL_SIZE);
    LodChunkQuery {
        worldspace_id,
        tier,
        bounds_min: [min_x, min_y],
        bounds_max: [max_x, max_y],
    }
}

fn is_current_query(streaming: &LodStreaming, generation: u64) -> bool {
    streaming.center.is_some() && generation == streaming.generation
}

fn query_unload_radius(reach_cells: i32) -> i32 {
    reach_cells.saturating_add(LOD_UNLOAD_MARGIN_CELLS)
}

fn chunk_min_grid(key: ChunkKey, origin: LodOrigin) -> (i64, i64) {
    let side = i64::from(key.tier.side_cells());
    (
        i64::from(origin.grid_x) + i64::from(key.anchor.x) * side,
        i64::from(origin.grid_y) + i64::from(key.anchor.y) * side,
    )
}

fn chunk_contains_cell(key: ChunkKey, origin: LodOrigin, cell: IVec2) -> bool {
    let side = i64::from(key.tier.side_cells());
    let min = chunk_min_grid(key, origin);
    let x = i64::from(cell.x);
    let y = i64::from(cell.y);
    x >= min.0 && x < min.0 + side && y >= min.1 && y < min.1 + side
}

fn chunk_within_radius(key: ChunkKey, origin: LodOrigin, center: (i64, i64), radius: i32) -> bool {
    let side = i64::from(key.tier.side_cells());
    let min = chunk_min_grid(key, origin);
    let max = (min.0 + side - 1, min.1 + side - 1);
    let distance = |value: i64, low: i64, high: i64| {
        if value < low {
            low - value
        } else if value > high {
            value - high
        } else {
            0
        }
    };
    distance(center.0, min.0, max.0).max(distance(center.1, min.1, max.1)) <= i64::from(radius)
}

fn enqueue_due_lod_retries(streaming: &mut LodStreaming, now: Instant) {
    let generation = streaming.generation;
    let due: Vec<_> = streaming
        .chunks
        .iter()
        .filter_map(|(&key, status)| match status {
            LodChunkStatus::Failed {
                metadata,
                retry_count,
                retry_at: Some(retry_at),
                ..
            } if *retry_at <= now && !streaming.queued_retry_chunks.contains(&key) => {
                Some((key, metadata.clone(), retry_count.saturating_add(1)))
            }
            _ => None,
        })
        .collect();
    for (key, metadata, retry_attempt) in due {
        if streaming.queued_retry_chunks.insert(key) {
            streaming
                .pending_chunks
                .push_back((generation, metadata, Some(retry_attempt)));
        }
    }
}

fn lod_retry_deadline(
    class: LodChunkFailureClass,
    retry_count: u8,
    now: Instant,
) -> Option<Instant> {
    if class != LodChunkFailureClass::Transient || retry_count >= LOD_CHUNK_MAX_RETRIES {
        return None;
    }
    let multiplier = 1_u32 << u32::from(retry_count.min(2));
    Some(
        now + LOD_CHUNK_RETRY_BASE_DELAY
            .saturating_mul(multiplier)
            .min(LOD_CHUNK_RETRY_MAX_DELAY),
    )
}

fn classify_asset_load_failure(error: &AssetLoadError) -> LodChunkFailure {
    let class = match error {
        AssetLoadError::AssetReaderError(AssetReaderError::Io(error))
            if error.kind() != std::io::ErrorKind::NotFound =>
        {
            LodChunkFailureClass::Transient
        }
        AssetLoadError::AssetReaderError(AssetReaderError::HttpError(408 | 429 | 500..=599)) => {
            LodChunkFailureClass::Transient
        }
        _ => LodChunkFailureClass::Terminal,
    };
    let reason = error.to_string();
    match class {
        LodChunkFailureClass::Transient => LodChunkFailure::transient(reason),
        LodChunkFailureClass::Terminal => LodChunkFailure::terminal(reason),
    }
}

fn verify_payload_hash(path: PathBuf, expected: String) -> Result<(), LodChunkFailure> {
    let bytes = std::fs::read(&path).map_err(|error| {
        let reason = format!("failed to read LOD payload {}: {error}", path.display());
        if error.kind() == std::io::ErrorKind::NotFound {
            LodChunkFailure::terminal(reason)
        } else {
            LodChunkFailure::transient(reason)
        }
    })?;
    let digest = Sha256::digest(bytes);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if actual == expected {
        Ok(())
    } else {
        Err(LodChunkFailure::terminal(format!(
            "LOD payload {} has SHA-256 {actual}, expected {expected}",
            path.display()
        )))
    }
}

fn update_counts(
    streaming: &LodStreaming,
    metrics: &mut StreamingMetrics,
    profiler: &mut ProfilingState,
) {
    metrics.pending_lod_queries = streaming.pending_queries.len()
        + if streaming.center.is_some() {
            LodTier::ALL
                .into_iter()
                .filter(|tier| {
                    !streaming.requested_queries.contains(tier)
                        && streaming
                            .query_retries
                            .get(tier)
                            .is_none_or(|(_, retry_at)| retry_at.is_some())
                })
                .count()
        } else {
            0
        };
    metrics.unrecovered_lod_queries = streaming
        .query_retries
        .iter()
        .filter(|(tier, (_, retry_at))| {
            retry_at.is_none() && !streaming.pending_queries.contains(tier)
        })
        .count();
    metrics.unrecovered_lod_chunks = streaming
        .chunks
        .values()
        .filter(|status| matches!(status, LodChunkStatus::Failed { retry_at: None, .. }))
        .count();
    metrics.resident_lod_chunks = streaming
        .chunks
        .values()
        .filter(|status| matches!(status, LodChunkStatus::Ready { .. }))
        .count();
    let mut pending_keys: HashSet<_> = streaming
        .chunks
        .iter()
        .filter_map(|(key, status)| {
            matches!(
                status,
                LodChunkStatus::Loading { .. }
                    | LodChunkStatus::Failed {
                        retry_at: Some(_),
                        ..
                    }
            )
            .then_some(*key)
        })
        .collect();
    pending_keys.extend(
        streaming
            .pending_chunks
            .iter()
            .filter(|(generation, metadata, _)| {
                *generation == streaming.generation && !streaming.chunks.contains_key(&metadata.key)
            })
            .map(|(_, metadata, _)| metadata.key),
    );
    metrics.pending_lod_chunks = pending_keys.len();
    profiler.set_gauge("lod/resident_chunks", metrics.resident_lod_chunks as f64);
    profiler.set_gauge("lod/pending_chunks", metrics.pending_lod_chunks as f64);
}

impl LodChunkRoot {
    /// Immutable placement identity; does not imply visibility or drawable assets.
    pub(super) fn terrain_key(&self) -> ChunkKey {
        self.key
    }
}

#[cfg(test)]
impl LodStreaming {
    /// Spawns a chunk root the way [`collect_lod_chunks`] does - its [`LodChunkRoot`] and the
    /// visibility a shown root carries - records it as ready, and returns it.
    pub(super) fn spawn_ready_chunk_for_test(
        world: &mut World,
        worldspace_id: u32,
        anchor: shared::lod::ChunkAnchor,
        origin: LodOrigin,
    ) -> Entity {
        Self::spawn_ready_chunk_at_tier_for_test(
            world,
            ChunkKey::new(worldspace_id, LodTier::Tier16, anchor),
            origin,
        )
    }
    /// Records a ready root with the exact world, tier and anchor owned by a visibility fixture.
    pub(super) fn spawn_ready_chunk_at_tier_for_test(
        world: &mut World,
        key: ChunkKey,
        origin: LodOrigin,
    ) -> Entity {
        let root = world
            .spawn((
                LodChunkRoot {
                    key,
                    generation: 0,
                    origin,
                    retry_count: 0,
                },
                Visibility::Inherited,
            ))
            .id();
        world
            .resource_mut::<LodStreaming>()
            .chunks
            .insert(key, LodChunkStatus::Ready { root, origin });
        root
    }

    /// The table's entries, whatever state the chunks are in.
    pub(super) fn resident_chunks_for_test(&self) -> usize {
        self.chunks.len()
    }

    /// Spawns a chunk root in the state [`plan_lod_chunks`] leaves a chunk that fell out of the
    /// keep-set - hidden, with the marker the revive gives back - and records it as retiring: for
    /// the unload pacer's tests, which cannot fill the private table themselves.
    pub(super) fn spawn_retiring_chunk_for_test(
        world: &mut World,
        worldspace_id: u32,
        anchor: shared::lod::ChunkAnchor,
        origin: LodOrigin,
    ) -> Entity {
        let key = ChunkKey::new(worldspace_id, LodTier::Tier16, anchor);
        let root = world
            .spawn((
                LodChunkRoot {
                    key,
                    generation: 0,
                    origin,
                    retry_count: 0,
                },
                Visibility::Inherited,
            ))
            .id();
        super::set_distance_retired_for_test(world, root, true);
        world
            .resource_mut::<LodStreaming>()
            .chunks
            .insert(key, LodChunkStatus::Retiring { root, origin });
        root
    }

    /// The number of chunks waiting for their turn in the unload budget.
    pub(super) fn retiring_chunks_for_test(&self) -> usize {
        self.chunks
            .values()
            .filter(|status| matches!(status, LodChunkStatus::Retiring { .. }))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("lod_admission_review_tests.rs");
    use crate::world::database::LodChunkBounds;
    use bevy::ecs::system::RunSystemOnce;

    /// A reach equal to each tier's chunk side (4/8/16 cells), the smallest
    /// nesting the residency tests below exercise.
    #[test]
    fn the_first_terrain_handover_of_a_frame_is_never_held_by_time() {
        let mut budget = crate::streaming::StreamingCommitBudget {
            remaining: 1,
            ..Default::default()
        };
        // A late frame (the hand-over runs in `Last`) with nothing committed yet still moves.
        budget.frame_started = std::time::Instant::now() - std::time::Duration::from_secs(1);
        assert!(terrain_handover_budget_available(&budget, 4_000));
        assert!(
            !terrain_batch_budget_available(&budget, 4_000),
            "a late frame still holds a CPU batching commit"
        );
        budget.commits = 1;
        assert!(
            !terrain_handover_budget_available(&budget, 4_000),
            "after a commit, the spent time budget holds the next step"
        );
        budget.frame_started = std::time::Instant::now();
        assert!(terrain_handover_budget_available(&budget, 4_000));
        budget.remaining = 0;
        assert!(
            !terrain_handover_budget_available(&budget, 4_000),
            "no slot left"
        );
    }

    fn fixture_chunk_owner(world: &mut World, tier: LodTier, grid: IVec2) -> Entity {
        let worldspace_id = world.resource::<EngineConfig>().worldspace_id;
        let origin = LodOrigin::new(0, 0);
        world
            .spawn((
                LodChunkRoot {
                    key: ChunkKey::new(
                        worldspace_id,
                        tier,
                        origin.chunk_for_cell(tier, grid.x, grid.y),
                    ),
                    generation: 0,
                    origin,
                    retry_count: 0,
                },
                Visibility::Inherited,
            ))
            .id()
    }

    fn identify_visibility_fixtures(world: &mut World) {
        world.init_resource::<TerrainLodIndex>();
        let worldspace_id = world.resource::<EngineConfig>().worldspace_id;
        let mut query = world.query::<(Entity, &TerrainCoverage)>();
        let patches: Vec<_> = query
            .iter(world)
            .map(|(entity, coverage)| (entity, *coverage))
            .collect();
        let mut roots = HashMap::new();
        for (entity, coverage) in patches {
            if let Some(tier) = coverage.tier {
                let origin = LodOrigin::new(0, 0);
                let key = ChunkKey::new(
                    worldspace_id,
                    tier,
                    origin.chunk_for_cell(tier, coverage.grid.x, coverage.grid.y),
                );
                let root = *roots.entry(key).or_insert_with(|| {
                    world
                        .spawn(LodChunkRoot {
                            key,
                            generation: 1,
                            origin,
                            retry_count: 0,
                        })
                        .id()
                });
                world.entity_mut(entity).insert(ChildOf(root));
            } else {
                world
                    .entity_mut(entity)
                    .insert(super::super::TerrainWorldspace(worldspace_id));
            }
        }
    }

    fn side_reach() -> TerrainLodDistances {
        TerrainLodDistances {
            block_level0_distance: 4.0 * CELL_SIZE,
            block_level1_distance: 8.0 * CELL_SIZE,
            block_maximum_distance: 16.0 * CELL_SIZE,
            split_distance_mult: 1.0,
        }
    }

    fn default_unload_radius(tier: LodTier) -> i32 {
        query_unload_radius(side_reach().reach_cells(tier))
    }

    fn tier_mask(tiers: impl IntoIterator<Item = LodTier>) -> u8 {
        tiers.into_iter().fold(0, |mask, tier| mask | tier as u8)
    }

    /// The pre-bitmask selection rule is the oracle for every legal ready set.
    fn reference_tier_selection(
        distance: i32,
        full_ready: bool,
        available: &HashSet<LodTier>,
        distances: &TerrainLodDistances,
    ) -> Option<LodTier> {
        if full_ready {
            None
        } else {
            LodTier::ALL
                .into_iter()
                .find(|tier| distance <= distances.reach_cells(*tier) && available.contains(tier))
        }
    }

    #[test]
    fn ready_tier_bitmask_matches_all_subsets_and_reach_boundaries() {
        let configurations = [
            side_reach(),
            TerrainLodDistances::default(),
            TerrainLodDistances {
                block_level0_distance: 20.0 * CELL_SIZE,
                block_level1_distance: 5.0 * CELL_SIZE,
                block_maximum_distance: 10.0 * CELL_SIZE,
                split_distance_mult: 1.0,
            },
        ];
        for subset in 0u8..8 {
            let reference: HashSet<_> = LodTier::ALL
                .into_iter()
                .enumerate()
                .filter_map(|(index, tier)| (subset & (1 << index) != 0).then_some(tier))
                .collect();
            let available = tier_mask(reference.iter().copied());
            assert_eq!(
                available,
                tier_mask(reference.iter().copied().chain(reference.iter().copied())),
                "duplicate source coverage must not change the ready set"
            );
            for distances in &configurations {
                let mut boundaries = vec![0, i32::MAX];
                for tier in LodTier::ALL {
                    let reach = distances.reach_cells(tier);
                    boundaries.extend([reach.saturating_sub(1), reach, reach.saturating_add(1)]);
                }
                for distance in boundaries {
                    for full_ready in [false, true] {
                        assert_eq!(
                            select_terrain_lod_tier(distance, full_ready, available, distances),
                            reference_tier_selection(distance, full_ready, &reference, distances),
                            "subset {subset:03b}, distance {distance}, full ready {full_ready}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ready_tier_bitmask_preserves_negative_rebased_quadrant_coverage() {
        let distances = side_reach();
        let center = IVec2::new(-32, -16);
        for subset in 0u8..8 {
            let reference: HashSet<_> = LodTier::ALL
                .into_iter()
                .enumerate()
                .filter_map(|(index, tier)| (subset & (1 << index) != 0).then_some(tier))
                .collect();
            let mut world = World::new();
            world.insert_resource(EngineConfig {
                terrain_lod: distances,
                ..default()
            });
            world.insert_resource(RenderOrigin(center));
            world.insert_resource(StreamingMetrics::default());
            world.insert_resource(ProfilingState::default());
            world.insert_resource(LodStreaming::default());
            world.spawn((
                StreamingCamera,
                Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
            ));
            let mut patches = Vec::new();
            let mut expected_visible = 0;
            for (index, distance) in [0, 4, 5, 8, 9, 16, 17].into_iter().enumerate() {
                let grid = center + IVec2::new(distance, 0);
                let quadrant = (index % 4) as u8;
                let full_ready = index % 2 == 0;
                if full_ready {
                    world.spawn((
                        super::super::TerrainPatch,
                        TerrainCoverage {
                            grid,
                            quadrant,
                            tier: None,
                        },
                        TerrainSurfaceReady,
                    ));
                }
                let expected =
                    reference_tier_selection(distance, full_ready, &reference, &distances);
                expected_visible += usize::from(expected.is_some());
                for tier in LodTier::ALL {
                    let mut entity = world.spawn((
                        TerrainCoverage {
                            grid,
                            quadrant,
                            tier: Some(tier),
                        },
                        Visibility::Inherited,
                    ));
                    let ready = reference.contains(&tier);
                    if ready {
                        entity.insert(TerrainSurfaceReady);
                    }
                    // The index owns ready patches only. Coverage arrives together with
                    // readiness in production and the chunk root hides everything until then,
                    // so a never-ready patch keeps the visibility it spawned with.
                    let expected_visibility = if ready && expected != Some(tier) {
                        Visibility::Hidden
                    } else {
                        Visibility::Inherited
                    };
                    patches.push((entity.id(), expected_visibility));
                }
            }
            identify_visibility_fixtures(&mut world);
            world
                .run_system_once(update_terrain_lod_visibility)
                .unwrap();
            for (entity, expected_visibility) in patches {
                assert_eq!(
                    *world.get::<Visibility>(entity).unwrap(),
                    expected_visibility,
                    "subset {subset:03b} changed rebased negative-grid coverage"
                );
            }
            assert_eq!(
                world
                    .resource::<StreamingMetrics>()
                    .visible_lod_terrain_patches,
                expected_visible
            );
        }
    }

    /// `SkyrimPrefs.ini` distances move each tier's reach, not the chunk sizes.
    /// The defaults are Skyrim's own, so level 4 reaches 12 cells, not 4.
    #[test]
    fn configured_skyrim_distances_set_each_tier_reach() {
        let skyrim = TerrainLodDistances::default();
        assert_eq!(
            LodTier::ALL.map(|tier| skyrim.reach_cells(tier)),
            [12, 25, 91]
        );
        let short = side_reach();
        assert_eq!(LodTier::ALL.map(|tier| short.reach_cells(tier)), [4, 8, 16]);
        let available = tier_mask(LodTier::ALL);
        assert_eq!(
            select_terrain_lod_tier(12, false, available, &skyrim),
            Some(LodTier::Tier4)
        );
        assert_eq!(
            select_terrain_lod_tier(12, false, available, &short),
            Some(LodTier::Tier16)
        );
        assert_eq!(
            select_terrain_lod_tier(91, false, available, &skyrim),
            Some(LodTier::Tier16)
        );
        assert_eq!(select_terrain_lod_tier(92, false, available, &skyrim), None);
        assert_eq!(query_unload_radius(skyrim.reach_cells(LodTier::Tier4)), 14);
        assert!(lod_camera_far(skyrim.max_reach_cells()) > lod_camera_far(16));
        let unbounded = TerrainLodDistances {
            block_maximum_distance: f32::MAX,
            split_distance_mult: f32::MAX,
            ..skyrim
        };
        assert_eq!(unbounded.reach_cells(LodTier::Tier16), i32::MAX);
        assert_eq!(query_unload_radius(i32::MAX), i32::MAX);
    }

    #[test]
    fn failed_queries_retry_with_bounded_backoff_in_the_same_generation() {
        let mut streaming = LodStreaming::default();
        let tier = LodTier::Tier4;
        let mut now = Instant::now();
        assert!(streaming.query_ready(tier, now));
        for delay in [1, 2, 4] {
            streaming.query_submitted(tier);
            assert!(!streaming.query_ready(tier, now));
            streaming.query_failed(tier, LodChunkFailureClass::Transient, now);
            assert!(!streaming.query_ready(tier, now));
            now += Duration::from_secs(delay);
            assert!(streaming.query_ready(tier, now));
        }
        streaming.query_submitted(tier);
        streaming.query_failed(tier, LodChunkFailureClass::Transient, now);
        assert!(!streaming.query_ready(tier, now + Duration::from_secs(100)));
        assert!(streaming.pending_queries.is_empty());
        streaming.move_center(IVec2::ZERO, 1, &TerrainLodDistances::default());
        assert!(streaming.query_ready(tier, now));
    }

    #[test]
    fn terminal_query_failure_never_schedules_a_retry() {
        let mut streaming = LodStreaming::default();
        let now = Instant::now();
        streaming.query_submitted(LodTier::Tier4);
        streaming.query_failed(LodTier::Tier4, LodChunkFailureClass::Terminal, now);
        assert!(!streaming.query_ready(LodTier::Tier4, now + Duration::from_secs(100)));
        assert!(streaming.pending_queries.is_empty());
        assert_eq!(streaming.query_retries[&LodTier::Tier4].1, None);
    }

    #[test]
    fn tier_residency_bounds_near_chunks_and_preserves_coarse_inner_fallback() {
        assert_eq!(LodTier::ALL.map(default_unload_radius), [6, 10, 18]);
        for tier in LodTier::ALL {
            let origin = LodOrigin::new(-4, -4);
            let inner = ChunkKey::new(1, tier, origin.chunk_for_cell(tier, -1, -1));
            assert!(chunk_within_radius(
                inner,
                origin,
                (-1, -1),
                default_unload_radius(tier)
            ));
            let outer = ChunkKey::new(1, tier, origin.chunk_for_cell(tier, 30, 30));
            assert!(!chunk_within_radius(
                outer,
                origin,
                (-1, -1),
                default_unload_radius(tier)
            ));
        }
    }

    #[test]
    fn tier_residency_covers_moving_positive_and_negative_boundaries_with_less_work() {
        let origin = LodOrigin::new(-4, 3);
        for x in [-33, -17, -16, -5, -4, -1, 0, 3, 4, 15, 16, 31, 32] {
            let center = IVec2::new(x, -x);
            for tier in LodTier::ALL {
                let radius = default_unload_radius(tier);
                let query = query_for(1, tier, center, radius);
                let mut bounded = 0;
                let mut previous = 0;
                for ax in -20..=20 {
                    for ay in -20..=20 {
                        let key = ChunkKey::new(1, tier, shared::lod::ChunkAnchor::new(ax, ay));
                        let within = chunk_within_radius(
                            key,
                            origin,
                            (i64::from(center.x), i64::from(center.y)),
                            radius,
                        );
                        bounded += usize::from(within);
                        previous += usize::from(chunk_within_radius(
                            key,
                            origin,
                            (i64::from(center.x), i64::from(center.y)),
                            18,
                        ));
                        if within {
                            let min = chunk_min_grid(key, origin);
                            let side = i64::from(tier.side_cells());
                            assert!((min.0 as f64 * f64::from(CELL_SIZE)) < query.bounds_max[0]);
                            assert!(
                                ((min.0 + side) as f64 * f64::from(CELL_SIZE))
                                    > query.bounds_min[0]
                            );
                            assert!((min.1 as f64 * f64::from(CELL_SIZE)) < query.bounds_max[1]);
                            assert!(
                                ((min.1 + side) as f64 * f64::from(CELL_SIZE))
                                    > query.bounds_min[1]
                            );
                        }
                    }
                }
                if tier != LodTier::Tier16 {
                    assert!(bounded < previous);
                }
                for dx in -tier.side_cells()..=tier.side_cells() {
                    for dy in -tier.side_cells()..=tier.side_cells() {
                        let cell = center + IVec2::new(dx, dy);
                        let key =
                            ChunkKey::new(1, tier, origin.chunk_for_cell(tier, cell.x, cell.y));
                        assert!(chunk_contains_cell(key, origin, cell));
                        assert!(chunk_within_radius(
                            key,
                            origin,
                            (i64::from(center.x), i64::from(center.y)),
                            radius
                        ));
                    }
                }
            }
        }
    }

    #[test]
    fn lod_follows_the_active_exterior_and_stops_in_an_interior() {
        assert_eq!(lod_worldspace(3, None, 60), Some(60));
        assert_eq!(lod_worldspace(-1, None, 60), None, "LOD turned off");
        let configured = ActiveSpace::default();
        assert_eq!(lod_worldspace(3, Some(&configured), 60), Some(60));
        let other = ActiveSpace {
            worldspace_id: Some(61),
            interior: None,
        };
        assert_eq!(lod_worldspace(3, Some(&other), 60), Some(61));
        let inside = ActiveSpace {
            worldspace_id: None,
            interior: Some(7),
        };
        assert_eq!(lod_worldspace(3, Some(&inside), 60), None);
    }

    #[test]
    fn resident_chunks_of_another_worldspace_are_dropped() {
        let metadata = retry_test_metadata();
        let distances = TerrainLodDistances::default();
        let at = |worldspace_id| {
            keep_resident_chunk(
                metadata.key,
                metadata.origin,
                worldspace_id,
                (0, 0),
                &distances,
            )
        };
        assert!(at(metadata.key.worldspace_id));
        assert!(!at(metadata.key.worldspace_id.wrapping_add(1)));
    }

    #[test]
    fn a_worldspace_change_moves_the_centre_even_on_the_same_grid() {
        let metadata = retry_test_metadata();
        let mut streaming = LodStreaming {
            generation: 3,
            center: Some(IVec2::ZERO),
            worldspace_id: Some(metadata.key.worldspace_id),
            pending_chunks: VecDeque::from([(3, metadata.clone(), None)]),
            ..default()
        };
        let other = metadata.key.worldspace_id.wrapping_add(1);
        streaming.move_center(IVec2::ZERO, other, &TerrainLodDistances::default());
        assert_eq!(streaming.worldspace_id, Some(other));
        assert!(
            streaming.pending_chunks.is_empty(),
            "queued chunks of the old worldspace are dropped"
        );
    }

    #[test]
    fn movement_retains_in_range_queued_metadata_but_discards_teleport_work() {
        let metadata = retry_test_metadata();
        let mut streaming = LodStreaming {
            generation: 7,
            center: Some(IVec2::ZERO),
            pending_chunks: VecDeque::from([(7, metadata.clone(), None)]),
            ..default()
        };
        streaming.move_center(
            IVec2::new(1, 0),
            metadata.key.worldspace_id,
            &TerrainLodDistances::default(),
        );
        assert_eq!(
            streaming.pending_chunks.front(),
            Some(&(8, metadata.clone(), None))
        );
        assert!(!is_current_query(&streaming, 7));
        streaming.move_center(
            IVec2::new(1000, 1000),
            metadata.key.worldspace_id,
            &TerrainLodDistances::default(),
        );
        assert!(streaming.pending_chunks.is_empty());
    }

    #[test]
    fn pending_counts_include_queued_and_retry_work_without_duplicates() {
        let metadata = retry_test_metadata();
        let key = metadata.key;
        let mut streaming = LodStreaming {
            generation: 7,
            center: Some(IVec2::ZERO),
            requested_queries: LodTier::ALL.into_iter().collect(),
            pending_chunks: VecDeque::from([
                (7, metadata.clone(), None),
                (7, metadata.clone(), None),
            ]),
            ..default()
        };
        let mut metrics = StreamingMetrics::default();
        let mut profiler = ProfilingState::default();
        update_counts(&streaming, &mut metrics, &mut profiler);
        assert_eq!(metrics.pending_lod_chunks, 1);
        streaming.chunks.insert(
            key,
            LodChunkStatus::Failed {
                origin: metadata.origin,
                metadata,
                retry_count: 0,
                retry_at: Some(Instant::now() + Duration::from_secs(1)),
            },
        );
        update_counts(&streaming, &mut metrics, &mut profiler);
        assert_eq!(metrics.pending_lod_chunks, 1);
        streaming.pending_chunks.clear();
        streaming.query_failed(
            LodTier::Tier4,
            LodChunkFailureClass::Transient,
            Instant::now(),
        );
        update_counts(&streaming, &mut metrics, &mut profiler);
        assert_eq!(metrics.pending_lod_queries, 1);
        assert_eq!(metrics.pending_lod_chunks, 1);
        if let Some(LodChunkStatus::Failed { retry_at, .. }) = streaming.chunks.get_mut(&key) {
            *retry_at = None;
        }
        update_counts(&streaming, &mut metrics, &mut profiler);
        assert_eq!(metrics.pending_lod_chunks, 0);
        assert_eq!(metrics.unrecovered_lod_chunks, 1);
        streaming.chunks.remove(&key);
        update_counts(&streaming, &mut metrics, &mut profiler);
        assert_eq!(metrics.unrecovered_lod_chunks, 0);
    }

    #[test]
    fn lod_payload_hash_checks_keep_bad_and_missing_content_terminal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("payload.glb");
        let bytes = b"fixture payload";
        std::fs::write(&path, bytes).unwrap();
        let digest = Sha256::digest(bytes);
        let expected: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(verify_payload_hash(path.clone(), expected.clone()).is_ok());
        std::fs::write(&path, b"changed payload").unwrap();
        assert_eq!(
            verify_payload_hash(path.clone(), expected.clone())
                .unwrap_err()
                .class,
            LodChunkFailureClass::Terminal
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            verify_payload_hash(path, expected).unwrap_err().class,
            LodChunkFailureClass::Terminal
        );
    }

    fn retry_test_metadata() -> LodChunkMetadata {
        LodChunkMetadata {
            key: ChunkKey::new(0x3c, LodTier::Tier16, shared::lod::ChunkAnchor::new(0, 0)),
            payload_path: "worlds/test/lod/tier16_0_0.glb".to_owned(),
            content_hash: "expected-hash".to_owned(),
            bounds: LodChunkBounds {
                min: [0.0; 3],
                max: [1.0; 3],
            },
            source_cells: vec![[0, 0]],
            build_identity: "test-build".to_owned(),
            origin: LodOrigin::new(0, 0),
        }
    }

    #[test]
    fn elapsed_shared_commit_budget_stops_lod_commits_but_drains_query_responses() {
        use crate::{
            streaming::{
                admission::SceneAdmission,
                runtime::{StreamingRuntime, update_streaming_control},
            },
            streaming_preparation::StreamingPreparationBridge,
        };
        use bevy::asset::{AssetApp, AssetPlugin};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lod-collector.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE schema_info(version INTEGER NOT NULL);
                 INSERT INTO schema_info VALUES({});",
                shared::WORLD_DATABASE_SCHEMA_VERSION,
            ))
            .unwrap();
        drop(connection);
        let first = retry_test_metadata();
        let mut second = first.clone();
        second.key.anchor.x = 1;
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin {
                file_path: directory.path().to_string_lossy().into_owned(),
                ..default()
            },
        ))
        .init_asset::<WorldAsset>()
        .insert_resource(EngineConfig {
            assets_dir: directory.path().into(),
            headless: true,
            max_streaming_backlog: 256,
            streaming_memory_mib: 64,
            max_commit_micros_per_frame: 1_000,
            ..default()
        })
        .insert_resource(WorldDatabase::open(&path).unwrap())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(LodStreaming {
            generation: 7,
            center: Some(IVec2::ZERO),
            pending_queries: HashSet::from([LodTier::Tier4]),
            pending_chunks: VecDeque::from([(7, first, None), (7, second, None)]),
            ..default()
        })
        .insert_resource(StreamingCommitBudget {
            frame_started: Instant::now() - Duration::from_secs(1),
            remaining: 3,
            commits: 1,
            ..default()
        })
        .init_resource::<StreamingWorld>()
        .init_resource::<StreamingMetrics>()
        .init_resource::<SceneAdmission>()
        .init_resource::<StreamingPreparationBridge>()
        .init_resource::<ProfilingState>()
        .init_resource::<StreamingRuntime>();
        update_streaming_control(app.world_mut());
        assert_eq!(
            app.world()
                .resource::<StreamingRuntime>()
                .decision
                .unwrap()
                .budgets
                .max_commit_micros,
            1_000
        );
        app.world()
            .resource::<WorldDatabase>()
            .request_lod_chunks(
                7,
                LodChunkQuery {
                    worldspace_id: 0x3c,
                    tier: LodTier::Tier4,
                    bounds_min: [0.0, 0.0],
                    bounds_max: [10.0, 10.0],
                },
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.world_mut().run_system_once(collect_lod_chunks).unwrap();
            if app
                .world()
                .resource::<StreamingMetrics>()
                .lod_query_responses
                == 1
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "LOD query response was not reconciled"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let streaming = app.world().resource::<LodStreaming>();
        assert!(!streaming.pending_queries.contains(&LodTier::Tier4));
        assert_eq!(streaming.pending_chunks.len(), 2);
        assert!(streaming.chunks.is_empty());
        assert_eq!(app.world().resource::<StreamingCommitBudget>().remaining, 3);
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .lod_chunks_requested,
            0
        );

        // An expired clock still allows the first total commit of a frame.
        app.world_mut()
            .resource_mut::<StreamingCommitBudget>()
            .commits = 0;
        app.world_mut().run_system_once(collect_lod_chunks).unwrap();
        assert_eq!(
            app.world().resource::<LodStreaming>().pending_chunks.len(),
            1
        );
        let budget = app.world().resource::<StreamingCommitBudget>();
        assert_eq!((budget.remaining, budget.commits), (2, 1));
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .lod_chunks_requested,
            1
        );
        assert_eq!(
            app.world_mut()
                .query::<&requests::SceneRequest>()
                .iter(app.world())
                .count(),
            1
        );
    }

    #[test]
    fn lod_asset_failures_separate_missing_payloads_from_transient_io() {
        let missing = AssetLoadError::AssetReaderError(AssetReaderError::NotFound(PathBuf::from(
            "missing.glb",
        )));
        assert_eq!(
            classify_asset_load_failure(&missing).class,
            LodChunkFailureClass::Terminal
        );

        let interrupted =
            AssetLoadError::AssetReaderError(AssetReaderError::Io(std::sync::Arc::new(
                std::io::Error::new(std::io::ErrorKind::TimedOut, "temporary read failure"),
            )));
        assert_eq!(
            classify_asset_load_failure(&interrupted).class,
            LodChunkFailureClass::Transient
        );
    }

    #[test]
    fn transient_lod_failures_back_off_and_requeue_saved_metadata() {
        let metadata = retry_test_metadata();
        let key = metadata.key;
        let failed_at = Instant::now();
        let first_retry_at =
            lod_retry_deadline(LodChunkFailureClass::Transient, 0, failed_at).unwrap();
        assert_eq!(first_retry_at, failed_at + Duration::from_secs(1));

        let mut streaming = LodStreaming {
            generation: 7,
            center: Some(IVec2::new(16, 0)),
            chunks: HashMap::from([(
                key,
                LodChunkStatus::Failed {
                    origin: metadata.origin,
                    metadata: metadata.clone(),
                    retry_count: 0,
                    retry_at: Some(first_retry_at),
                },
            )]),
            ..default()
        };

        enqueue_due_lod_retries(&mut streaming, first_retry_at - Duration::from_millis(1));
        assert!(streaming.pending_chunks.is_empty());
        enqueue_due_lod_retries(&mut streaming, first_retry_at);
        assert_eq!(
            streaming.pending_chunks.front(),
            Some(&(7, metadata.clone(), Some(1)))
        );
        assert!(streaming.queued_retry_chunks.contains(&key));
        enqueue_due_lod_retries(&mut streaming, first_retry_at);
        assert_eq!(streaming.pending_chunks.len(), 1, "a due retry queues once");

        let second_failed_at = first_retry_at + Duration::from_millis(200);
        let second_retry_at =
            lod_retry_deadline(LodChunkFailureClass::Transient, 1, second_failed_at).unwrap();
        assert_eq!(second_retry_at, second_failed_at + Duration::from_secs(2));
        streaming.pending_chunks.clear();
        streaming.queued_retry_chunks.clear();
        streaming.chunks.insert(
            key,
            LodChunkStatus::Failed {
                origin: metadata.origin,
                metadata: metadata.clone(),
                retry_count: 1,
                retry_at: Some(second_retry_at),
            },
        );
        enqueue_due_lod_retries(&mut streaming, second_retry_at - Duration::from_millis(1));
        assert!(streaming.pending_chunks.is_empty());
        enqueue_due_lod_retries(&mut streaming, second_retry_at);
        assert_eq!(
            streaming.pending_chunks.front(),
            Some(&(7, metadata.clone(), Some(2)))
        );

        assert_eq!(
            lod_retry_deadline(LodChunkFailureClass::Transient, 2, failed_at),
            Some(failed_at + Duration::from_secs(4))
        );
        assert_eq!(
            lod_retry_deadline(
                LodChunkFailureClass::Transient,
                LOD_CHUNK_MAX_RETRIES,
                failed_at
            ),
            None,
            "retry attempts are bounded"
        );
        assert_eq!(
            lod_retry_deadline(LodChunkFailureClass::Terminal, 0, failed_at),
            None,
            "terminal payload failures are not retried"
        );
    }

    #[test]
    fn tier_handoff_is_per_quadrant_and_falls_back_to_ready_coarser_data() {
        let defaults = side_reach();
        let available = tier_mask(LodTier::ALL);
        assert_eq!(
            select_terrain_lod_tier(2, false, available, &defaults),
            Some(LodTier::Tier4)
        );
        assert_eq!(
            select_terrain_lod_tier(6, false, available, &defaults),
            Some(LodTier::Tier8)
        );
        assert_eq!(
            select_terrain_lod_tier(12, false, available, &defaults),
            Some(LodTier::Tier16)
        );
        assert_eq!(select_terrain_lod_tier(2, true, available, &defaults), None);

        let coarse_only = tier_mask([LodTier::Tier16]);
        assert_eq!(
            select_terrain_lod_tier(2, false, coarse_only, &defaults),
            Some(LodTier::Tier16)
        );
        assert_eq!(
            select_terrain_lod_tier(17, false, coarse_only, &defaults),
            None
        );
        assert_eq!(select_terrain_lod_tier(2, false, 0, &defaults), None);
    }

    #[test]
    fn terrain_lod_visibility_skips_unchanged_frames() {
        let mut app = App::new();
        app.insert_resource(EngineConfig::default())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(StreamingMetrics::default())
            .insert_resource(ProfilingState::default())
            .insert_resource(LodStreaming::default())
            .init_resource::<TerrainLodIndex>()
            .add_systems(
                Update,
                update_terrain_lod_visibility.run_if(terrain_lod_visibility_has_work),
            );
        let world = app.world_mut();
        world.spawn((
            StreamingCamera,
            Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
        ));
        let patches: Vec<_> = (0..22_000)
            .map(|_| {
                world
                    .spawn((
                        TerrainCoverage {
                            grid: IVec2::new(3, 0),
                            quadrant: 0,
                            tier: Some(LodTier::Tier4),
                        },
                        TerrainSurfaceReady,
                        Visibility::Hidden,
                    ))
                    .id()
            })
            .collect();
        let patch = patches[0];

        identify_visibility_fixtures(app.world_mut());
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(patch).unwrap(),
            Visibility::Inherited
        );
        app.world_mut().entity_mut(patch).insert(Visibility::Hidden);
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(patch).unwrap(),
            Visibility::Hidden,
            "unchanged coverage leaves visibility untouched"
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            22_000,
            "cached handoff metrics remain unchanged on skipped frames"
        );

        let started = Instant::now();
        for _ in 0..32 {
            app.update();
        }
        eprintln!(
            "terrain_lod_visibility_noop_22k_mean_us={}",
            started.elapsed().as_micros() / 32
        );

        app.world_mut()
            .resource_mut::<EngineConfig>()
            .terrain_lod
            .block_level0_distance += 1.0;
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(patch).unwrap(),
            Visibility::Inherited,
            "terrain LOD distance changes trigger a visibility refresh"
        );
        app.world_mut().entity_mut(patch).despawn();
        app.update();
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            21_999,
            "removing a patch refreshes visibility metrics"
        );
    }

    #[test]
    fn stationary_camera_reselects_resident_tiers_when_lod_distances_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lod-distances.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE schema_info(version INTEGER NOT NULL);
                 INSERT INTO schema_info VALUES({});",
                shared::WORLD_DATABASE_SCHEMA_VERSION,
            ))
            .unwrap();
        drop(connection);
        let mut config = EngineConfig {
            headless: true,
            ..default()
        };
        // Cell reach 4 for the fine tier and 8 for the coarse one, so a patch six cells away
        // selects the coarse tier until the fine tier's distance is raised.
        config.terrain_lod = TerrainLodDistances {
            block_level0_distance: 4.0 * CELL_SIZE,
            block_level1_distance: 8.0 * CELL_SIZE,
            block_maximum_distance: 16.0 * CELL_SIZE,
            split_distance_mult: 1.0,
        };
        let mut app = App::new();
        app.insert_resource(config)
            .insert_resource(WorldDatabase::open(&path).unwrap())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(LodStreaming::default())
            .init_resource::<TerrainLodIndex>()
            .insert_resource(StreamingCommitBudget::default())
            .insert_resource(StreamingMetrics::default())
            .insert_resource(ProfilingState::default())
            .add_systems(
                Update,
                (
                    plan_lod_chunks,
                    update_terrain_lod_visibility.run_if(terrain_lod_visibility_has_work),
                )
                    .chain(),
            );
        let camera = Transform::from_xyz(CELL_SIZE * 0.5, 0.0, CELL_SIZE * 0.5);
        let center = streaming_center(false, IVec2::ZERO, &camera);
        let grid = center + IVec2::new(6, 0);
        let world = app.world_mut();
        world.spawn((StreamingCamera, camera));
        // Both tiers are resident for the same quadrant, and neither the camera nor any readiness
        // or removal event changes after the first frame.
        let fine_owner = fixture_chunk_owner(world, LodTier::Tier4, grid);
        let coarse_owner = fixture_chunk_owner(world, LodTier::Tier8, grid);
        let fine = world
            .spawn((
                ChildOf(fine_owner),
                TerrainCoverage {
                    grid,
                    quadrant: 0,
                    tier: Some(LodTier::Tier4),
                },
                TerrainSurfaceReady,
                Visibility::Hidden,
            ))
            .id();
        let coarse = world
            .spawn((
                ChildOf(coarse_owner),
                TerrainCoverage {
                    grid,
                    quadrant: 0,
                    tier: Some(LodTier::Tier8),
                },
                TerrainSurfaceReady,
                Visibility::Hidden,
            ))
            .id();
        let visibility =
            |app: &App, entity: Entity| *app.world().get::<Visibility>(entity).unwrap();

        app.update();
        assert_eq!(visibility(&app, fine), Visibility::Hidden);
        assert_eq!(
            visibility(&app, coarse),
            Visibility::Inherited,
            "six cells is past the fine tier's reach, so the coarse tier draws"
        );

        app.world_mut()
            .resource_mut::<EngineConfig>()
            .terrain_lod
            .block_level0_distance = 6.0 * CELL_SIZE;
        app.update();
        assert_eq!(
            visibility(&app, fine),
            Visibility::Inherited,
            "raising the fine tier's distance re-selects it on the next frame"
        );
        assert_eq!(visibility(&app, coarse), Visibility::Hidden);
    }

    #[test]
    fn terrain_lod_cell_crossing_only_writes_quadrants_whose_selection_changes() {
        let mut app = App::new();
        app.insert_resource(EngineConfig::default())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(StreamingMetrics::default())
            .insert_resource(ProfilingState::default())
            .insert_resource(LodStreaming::default())
            .init_resource::<TerrainLodIndex>()
            .init_resource::<visibility::TerrainLodObservation>()
            .add_systems(
                Update,
                update_terrain_lod_visibility.run_if(terrain_lod_visibility_has_work),
            );
        super::super::identity_changes::install_terrain(app.world_mut());
        let world = app.world_mut();
        let camera = world
            .spawn((
                StreamingCamera,
                Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
            ))
            .id();
        // Tier 16 reaches 91 cells with the default distances: cell 91 draws, cell 92 does not
        // until the camera crosses one cell east.
        let patches: HashMap<i32, Entity> = (-3..=95)
            .map(|x| {
                let owner = fixture_chunk_owner(world, LodTier::Tier16, IVec2::new(x, 0));
                let patch = world
                    .spawn((
                        ChildOf(owner),
                        TerrainCoverage {
                            grid: IVec2::new(x, 0),
                            quadrant: 0,
                            tier: Some(LodTier::Tier16),
                        },
                        TerrainSurfaceReady,
                        Visibility::Hidden,
                    ))
                    .id();
                (x, patch)
            })
            .collect();
        let visibility = |app: &App, x: i32| *app.world().get::<Visibility>(patches[&x]).unwrap();

        app.update();
        assert_eq!(visibility(&app, 91), Visibility::Inherited);
        assert_eq!(visibility(&app, 92), Visibility::Hidden);
        let visible_before = app
            .world()
            .resource::<StreamingMetrics>()
            .visible_lod_terrain_patches;

        // Observe writes directly: an outside visibility override is a separate refresh input
        // that the selector must repair on a crossing.
        let written_at = |app: &App| {
            app.world()
                .entity(patches[&5])
                .get_ref::<Visibility>()
                .unwrap()
                .last_changed()
        };
        let work = |app: &App| {
            app.world()
                .resource::<visibility::TerrainLodObservation>()
                .work()
        };
        assert_eq!(visibility(&app, 5), Visibility::Inherited);
        let written_before = written_at(&app);
        let work_before = work(&app);
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation
            .x += CELL_SIZE;
        app.update();
        assert_eq!(visibility(&app, 92), Visibility::Inherited);
        assert_eq!(visibility(&app, 5), Visibility::Inherited);
        assert_eq!(
            written_at(&app),
            written_before,
            "an unchanged selection is not rewritten on a crossing"
        );
        let work_after = work(&app);
        assert_eq!(
            &work_after[..3],
            &work_before[..3],
            "a crossing does not rebuild, walk ancestry or reread patches"
        );
        assert_eq!(
            work_after[3] - work_before[3],
            1.0,
            "only cell 92 changes its selected tier"
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            visible_before + 1,
            "cell 92 enters the reach"
        );
    }

    #[test]
    fn terrain_lod_index_follows_removals_and_returns_on_scheduled_updates() {
        let mut app = App::new();
        app.insert_resource(EngineConfig::default())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(StreamingMetrics::default())
            .insert_resource(ProfilingState::default())
            .insert_resource(LodStreaming::default())
            .init_resource::<TerrainLodIndex>()
            .add_systems(
                Update,
                update_terrain_lod_visibility.run_if(terrain_lod_visibility_has_work),
            );
        let world = app.world_mut();
        world.spawn((
            StreamingCamera,
            Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
        ));
        let spawn = |world: &mut World, tier| {
            let owner = fixture_chunk_owner(world, tier, IVec2::new(3, 0));
            world
                .spawn((
                    ChildOf(owner),
                    TerrainCoverage {
                        grid: IVec2::new(3, 0),
                        quadrant: 0,
                        tier: Some(tier),
                    },
                    TerrainSurfaceReady,
                    Visibility::Hidden,
                ))
                .id()
        };
        let fine = spawn(world, LodTier::Tier4);
        let coarse = spawn(world, LodTier::Tier8);
        let visible = |app: &App, entity| *app.world().get::<Visibility>(entity).unwrap();

        app.update();
        assert_eq!(visible(&app, fine), Visibility::Inherited);
        assert_eq!(visible(&app, coarse), Visibility::Hidden);

        // The finer tier goes away: the coarser one covers the quadrant.
        app.world_mut().entity_mut(fine).despawn();
        app.update();
        assert_eq!(visible(&app, coarse), Visibility::Inherited);

        // A live patch that loses readiness stops drawing and the quadrant has nothing left.
        app.world_mut()
            .entity_mut(coarse)
            .remove::<TerrainSurfaceReady>();
        app.update();
        assert_eq!(visible(&app, coarse), Visibility::Hidden);
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            0
        );

        // Readiness returns: the patch is indexed again and draws.
        app.world_mut()
            .entity_mut(coarse)
            .insert(TerrainSurfaceReady);
        app.update();
        assert_eq!(visible(&app, coarse), Visibility::Inherited);
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            1
        );
    }

    #[test]
    fn terrain_lod_index_drops_patches_when_their_chunk_root_is_despawned() {
        let mut app = App::new();
        app.insert_resource(EngineConfig::default())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(StreamingMetrics::default())
            .insert_resource(ProfilingState::default())
            .insert_resource(LodStreaming::default())
            .init_resource::<TerrainLodIndex>()
            .add_systems(
                Update,
                update_terrain_lod_visibility.run_if(terrain_lod_visibility_has_work),
            );
        let world = app.world_mut();
        world.spawn((
            StreamingCamera,
            Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
        ));
        let root = fixture_chunk_owner(world, LodTier::Tier4, IVec2::new(3, 0));
        for quadrant in 0..4 {
            world.spawn((
                ChildOf(root),
                TerrainCoverage {
                    grid: IVec2::new(3, 0),
                    quadrant,
                    tier: Some(LodTier::Tier4),
                },
                TerrainSurfaceReady,
                Visibility::Hidden,
            ));
        }
        app.update();
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            4
        );
        // Production retires a chunk by despawning its root; the patches go with it.
        app.world_mut().entity_mut(root).despawn();
        app.update();
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            0
        );
    }

    #[test]
    fn terrain_lod_index_quiet_removal_drops_an_emptied_quadrant() {
        visibility::assert_removed_sources_leave_no_ghost_keys();
    }

    #[test]
    fn terrain_readiness_removal_releases_lod_on_scheduled_update() {
        let mut app = App::new();
        app.insert_resource(EngineConfig::default())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(StreamingMetrics::default())
            .insert_resource(ProfilingState::default())
            .insert_resource(LodStreaming::default())
            .init_resource::<TerrainLodIndex>()
            .add_systems(
                Update,
                update_terrain_lod_visibility.run_if(terrain_lod_visibility_has_work),
            );
        let world = app.world_mut();
        world.spawn((
            StreamingCamera,
            Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
        ));
        let full_patch = world
            .spawn((
                super::super::TerrainPatch,
                TerrainCoverage {
                    grid: IVec2::ZERO,
                    quadrant: 0,
                    tier: None,
                },
                TerrainSurfaceReady,
                Visibility::Inherited,
            ))
            .id();
        let lod_patch = world
            .spawn((
                TerrainCoverage {
                    grid: IVec2::ZERO,
                    quadrant: 0,
                    tier: Some(LodTier::Tier4),
                },
                TerrainSurfaceReady,
                Visibility::Hidden,
            ))
            .id();

        identify_visibility_fixtures(app.world_mut());
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(lod_patch).unwrap(),
            Visibility::Hidden
        );
        app.world_mut()
            .entity_mut(full_patch)
            .remove::<TerrainSurfaceReady>();
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(lod_patch).unwrap(),
            Visibility::Inherited,
            "readiness removal event restores LOD coverage"
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            1
        );
    }

    #[test]
    fn lod_grid_rebase_and_unload_bounds_handle_negative_anchors() {
        let origin = LodOrigin::new(8, -8);
        let key = ChunkKey::new(0x3c, LodTier::Tier4, shared::lod::ChunkAnchor::new(-1, 2));
        assert_eq!(chunk_min_grid(key, origin), (4, 0));
        assert!(chunk_contains_cell(key, origin, IVec2::new(7, 3)));
        assert!(!chunk_contains_cell(key, origin, IVec2::new(8, 3)));
        assert!(chunk_within_radius(key, origin, (3, 1), 1));
        assert!(!chunk_within_radius(key, origin, (10, 1), 1));
    }

    #[test]
    fn two_cell_handoff_is_independent_and_falls_back_after_a_tier_failure() {
        let mut world = World::new();
        // The tier boundaries below are written for each tier reaching its chunk side.
        world.insert_resource(EngineConfig {
            terrain_lod: side_reach(),
            ..EngineConfig::default()
        });
        world.insert_resource(RenderOrigin(IVec2::ZERO));
        world.insert_resource(StreamingMetrics::default());
        world.insert_resource(ProfilingState::default());
        world.insert_resource(LodStreaming::default());
        let camera = world
            .spawn((
                StreamingCamera,
                Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
            ))
            .id();

        let full_patches: Vec<_> = (0..4)
            .map(|quadrant| {
                world
                    .spawn((
                        super::super::TerrainPatch,
                        TerrainCoverage {
                            grid: IVec2::ZERO,
                            quadrant,
                            tier: None,
                        },
                        TerrainSurfaceReady,
                        Visibility::Inherited,
                    ))
                    .id()
            })
            .collect();
        let mut lod_patches = HashMap::new();
        for quadrant in 0..4 {
            for tier in LodTier::ALL {
                let zero = world
                    .spawn((
                        TerrainCoverage {
                            grid: IVec2::ZERO,
                            quadrant,
                            tier: Some(tier),
                        },
                        TerrainSurfaceReady,
                        Visibility::Hidden,
                    ))
                    .id();
                let three = world
                    .spawn((
                        TerrainCoverage {
                            grid: IVec2::new(3, 0),
                            quadrant,
                            tier: Some(tier),
                        },
                        TerrainSurfaceReady,
                        Visibility::Hidden,
                    ))
                    .id();
                lod_patches.insert((IVec2::ZERO, tier, quadrant), zero);
                lod_patches.insert((IVec2::new(3, 0), tier, quadrant), three);
            }
        }

        identify_visibility_fixtures(&mut world);
        world
            .run_system_once(update_terrain_lod_visibility)
            .unwrap();
        assert!(
            full_patches.iter().all(|entity| {
                *world.get::<Visibility>(*entity).unwrap() == Visibility::Inherited
            })
        );
        for quadrant in 0..4 {
            assert_eq!(
                *world
                    .get::<Visibility>(lod_patches[&(IVec2::ZERO, LodTier::Tier4, quadrant)])
                    .unwrap(),
                Visibility::Hidden,
                "full-detail cell zero owns quadrant {quadrant}"
            );
            assert_eq!(
                *world
                    .get::<Visibility>(lod_patches[&(IVec2::new(3, 0), LodTier::Tier4, quadrant)])
                    .unwrap(),
                Visibility::Inherited,
                "cell three independently selects tier 4 for quadrant {quadrant}"
            );
        }

        world
            .entity_mut(full_patches[1])
            .remove::<TerrainSurfaceReady>();
        for quadrant in 0..4 {
            world
                .entity_mut(lod_patches[&(IVec2::new(3, 0), LodTier::Tier4, quadrant)])
                .remove::<TerrainSurfaceReady>();
        }
        world
            .run_system_once(update_terrain_lod_visibility)
            .unwrap();
        assert_eq!(
            *world
                .get::<Visibility>(lod_patches[&(IVec2::ZERO, LodTier::Tier4, 1)])
                .unwrap(),
            Visibility::Inherited,
            "a single unready full-detail quadrant transfers to LOD"
        );
        for quadrant in 0..4 {
            assert_eq!(
                *world
                    .get::<Visibility>(lod_patches[&(IVec2::new(3, 0), LodTier::Tier8, quadrant)])
                    .unwrap(),
                Visibility::Inherited,
                "ready tier 8 covers failed tier 4 in quadrant {quadrant}"
            );
        }

        world.get_mut::<Transform>(camera).unwrap().translation.x = CELL_SIZE * 10.5;
        world
            .run_system_once(update_terrain_lod_visibility)
            .unwrap();
        assert_eq!(
            *world
                .get::<Visibility>(lod_patches[&(IVec2::ZERO, LodTier::Tier16, 1)])
                .unwrap(),
            Visibility::Inherited,
            "teleporting outward selects tier 16 for cell zero"
        );
        assert_eq!(
            *world
                .get::<Visibility>(lod_patches[&(IVec2::new(3, 0), LodTier::Tier8, 0)])
                .unwrap(),
            Visibility::Inherited,
            "cell three retains its independent tier selection"
        );

        let visible_before_removal = world
            .resource::<StreamingMetrics>()
            .visible_lod_terrain_patches;
        world
            .entity_mut(lod_patches[&(IVec2::ZERO, LodTier::Tier16, 1)])
            .despawn();
        world
            .run_system_once(update_terrain_lod_visibility)
            .unwrap();
        assert_eq!(
            world
                .resource::<StreamingMetrics>()
                .visible_lod_terrain_patches,
            visible_before_removal - 1,
            "removing a selected patch updates visibility metrics"
        );

        let current = LodStreaming {
            generation: 2,
            center: Some(IVec2::new(10, 0)),
            ..default()
        };
        assert!(
            !is_current_query(&current, 1),
            "pre-teleport results are stale"
        );
        assert!(is_current_query(&current, 2));

        let fine_chunk = ChunkKey::new(0x3c, LodTier::Tier4, shared::lod::ChunkAnchor::new(0, 0));
        assert!(!chunk_within_radius(
            fine_chunk,
            LodOrigin::new(0, 0),
            (22, 0),
            side_reach().max_reach_cells() + LOD_UNLOAD_MARGIN_CELLS,
        ));
    }

    fn pending_retirement_fixture(world: &mut World) -> (LodChunkMetadata, Entity) {
        world.init_resource::<LodStreaming>();
        let metadata = retry_test_metadata();
        let root = world
            .spawn((
                LodChunkRoot {
                    key: metadata.key,
                    generation: 7,
                    origin: metadata.origin,
                    retry_count: 0,
                },
                Visibility::Hidden,
                PendingLodChunk {
                    metadata: metadata.clone(),
                    asset: None,
                    scene_spawned: false,
                    hash_task: None,
                    hash_verified: false,
                    started: Instant::now(),
                },
            ))
            .id();
        world.resource_mut::<LodStreaming>().chunks.insert(
            metadata.key,
            LodChunkStatus::Retiring {
                root,
                origin: metadata.origin,
            },
        );
        (metadata, root)
    }

    fn finish_fixture_chunk(world: &mut World) {
        use bevy::ecs::system::SystemState;
        let mut state = SystemState::<(
            Commands,
            ResMut<LodStreaming>,
            Query<(Entity, &LodChunkRoot)>,
        )>::new(world);
        {
            let (mut commands, mut streaming, roots) = state.get_mut(world).unwrap();
            let (entity, root) = roots.single().unwrap();
            finish_lod_chunk(&mut commands, &mut streaming, entity, root);
        }
        state.apply(world);
    }

    fn revive_fixture_chunk(world: &mut World, key: ChunkKey) -> bool {
        use bevy::ecs::system::SystemState;
        let mut state = SystemState::<(
            Commands,
            ResMut<LodStreaming>,
            Query<(&Visibility, Option<&super::super::RetireHidden>)>,
            Query<(&LodChunkRoot, Option<&PendingLodChunk>)>,
        )>::new(world);
        let revived;
        {
            let (mut commands, mut streaming, visibility, roots) = state.get_mut(world).unwrap();
            revived = revive_left_chunk(
                &mut commands,
                &visibility,
                &roots,
                streaming.chunks.get_mut(&key).unwrap(),
            );
        }
        state.apply(world);
        revived
    }

    #[test]
    fn completion_while_retiring_stays_hidden_until_exact_root_revival() {
        let mut world = World::new();
        let (metadata, root) = pending_retirement_fixture(&mut world);
        finish_fixture_chunk(&mut world);
        assert!(
            matches!(world.resource::<LodStreaming>().chunks.get(&metadata.key), Some(LodChunkStatus::Retiring { root: actual, .. }) if *actual == root)
        );
        assert!(world.get::<PendingLodChunk>(root).is_none());
        assert_eq!(world.get::<Visibility>(root), Some(&Visibility::Hidden));
        assert!(world.get::<super::super::RetireHidden>(root).is_some());
        assert!(revive_fixture_chunk(&mut world, metadata.key));
        assert!(
            matches!(world.resource::<LodStreaming>().chunks.get(&metadata.key), Some(LodChunkStatus::Ready { root: actual, .. }) if *actual == root)
        );
        assert_eq!(world.get::<Visibility>(root), Some(&Visibility::Inherited));
        assert!(world.get::<super::super::RetireHidden>(root).is_none());
        assert!(!revive_fixture_chunk(&mut world, metadata.key));
    }

    #[test]
    fn revival_before_completion_keeps_the_same_pending_root_hidden() {
        let mut world = World::new();
        let (metadata, root) = pending_retirement_fixture(&mut world);
        assert!(revive_fixture_chunk(&mut world, metadata.key));
        assert!(
            matches!(world.resource::<LodStreaming>().chunks.get(&metadata.key), Some(LodChunkStatus::Loading { root: actual, generation: 7, .. }) if *actual == root)
        );
        assert!(world.get::<PendingLodChunk>(root).is_some());
        assert_eq!(world.get::<Visibility>(root), Some(&Visibility::Hidden));
        finish_fixture_chunk(&mut world);
        assert!(
            matches!(world.resource::<LodStreaming>().chunks.get(&metadata.key), Some(LodChunkStatus::Ready { root: actual, .. }) if *actual == root)
        );
        assert!(world.get::<PendingLodChunk>(root).is_none());
        assert_eq!(world.get::<Visibility>(root), Some(&Visibility::Inherited));
    }

    #[test]
    fn retiring_failure_leaves_disposal_to_the_shared_budget() {
        use bevy::ecs::system::SystemState;
        let mut world = World::new();
        let (metadata, root) = pending_retirement_fixture(&mut world);
        let mut state = SystemState::<(
            Commands,
            ResMut<LodStreaming>,
            Query<(Entity, &LodChunkRoot)>,
        )>::new(&mut world);
        let mut metrics = StreamingMetrics::default();
        {
            let (mut commands, mut streaming, roots) = state.get_mut(&mut world).unwrap();
            let (entity, chunk) = roots.single().unwrap();
            fail_chunk(
                (entity, chunk),
                &metadata,
                LodChunkFailure::terminal("fixture failure"),
                &mut commands,
                &mut streaming,
                &mut metrics,
                &mut ProfilingState::default(),
            );
        }
        state.apply(&mut world);
        assert_eq!(metrics.failed_lod_chunks, 1);
        assert!(world.get_entity(root).is_ok());
        assert_eq!(world.get::<Visibility>(root), Some(&Visibility::Hidden));
        assert_eq!(
            world.resource::<LodStreaming>().retiring_chunks(),
            vec![(metadata.key, root)]
        );
        let streaming = world.resource::<LodStreaming>();
        assert_eq!(streaming.chunks.len(), 1);
        assert!(
            matches!(streaming.chunks.get(&metadata.key), Some(LodChunkStatus::Retiring { root: actual, .. }) if *actual == root)
        );
        assert!(streaming.pending_chunks.is_empty());
        let mut counts = StreamingMetrics::default();
        update_counts(streaming, &mut counts, &mut ProfilingState::default());
        assert_eq!(
            (
                streaming.retiring_chunks().len(),
                counts.resident_lod_chunks,
                counts.pending_lod_chunks
            ),
            (1, 0, 0)
        );
        world.insert_resource(EngineConfig {
            max_cell_unloads_per_frame: 1,
            ..default()
        });
        world.init_resource::<StreamingWorld>();
        world.init_resource::<super::super::TerrainContinuity>();
        world.init_resource::<StreamingMetrics>();
        world.init_resource::<ProfilingState>();
        super::super::despawn_cells(&mut world);
        assert!(world.get_entity(root).is_err());
        assert!(
            world
                .resource::<LodStreaming>()
                .retiring_chunks()
                .is_empty()
        );
        assert!(
            !world
                .resource::<LodStreaming>()
                .chunks
                .contains_key(&metadata.key)
        );
    }

    #[test]
    fn failed_chunks_without_roots_drop_from_the_keep_set() {
        use bevy::ecs::system::SystemState;
        let mut world = World::new();
        let metadata = retry_test_metadata();
        let mut status = LodChunkStatus::Failed {
            origin: metadata.origin,
            metadata,
            retry_count: 0,
            retry_at: None,
        };
        let mut state = SystemState::<(
            Commands,
            Query<(&Visibility, Option<&super::super::RetireHidden>)>,
        )>::new(&mut world);
        let initial_entities = world.entities().len();
        {
            let (mut commands, roots) = state.get_mut(&mut world).unwrap();
            assert_eq!(
                leave_keep_set(&mut commands, &roots, &mut status),
                ChunkLeave::Dropped
            );
        }
        state.apply(&mut world);
        // Bevy 0.19 stores resources on entities; the transition must create no entities.
        assert_eq!(world.entities().len(), initial_entities);
        assert_eq!(
            world
                .query_filtered::<Entity, With<LodChunkRoot>>()
                .iter(&world)
                .count(),
            0
        );
    }
}

#[cfg(test)]
mod terrain_primitive_identity_tests {
    use super::super::{TerrainIdentityAccess, TerrainPrimitiveIdentity, TerrainWorldspace};
    use super::*;
    use bevy::ecs::system::{RunSystemOnce, SystemState};

    fn resolve(world: &mut World, mesh: Entity) -> Option<TerrainPrimitiveIdentity> {
        let mut state = SystemState::<TerrainIdentityAccess>::new(world);
        state
            .get(world)
            .expect("terrain identity queries are valid")
            .identity(mesh)
    }

    fn mesh(world: &mut World) -> Entity {
        world.spawn(Mesh3d(Handle::default())).id()
    }

    fn attach(world: &mut World, parent: Entity, child: Entity) {
        world.entity_mut(parent).add_child(child);
    }

    fn chunk(world: &mut World, tier: LodTier, grid: IVec2, worldspace_id: u32) -> Entity {
        let origin = LodOrigin::new(13, -7);
        let key = ChunkKey::new(
            worldspace_id,
            tier,
            origin.chunk_for_cell(tier, grid.x, grid.y),
        );
        world
            .spawn(LodChunkRoot {
                key,
                generation: 1,
                origin,
                retry_count: 0,
            })
            .id()
    }

    #[test]
    fn near_mesh_owns_its_exact_coverage_and_worldspace() {
        let mut world = World::new();
        let grid = IVec2::new(-19, 37);
        let entity = world
            .spawn((
                Mesh3d(Handle::default()),
                TerrainCoverage {
                    grid,
                    quadrant: 3,
                    tier: None,
                },
                TerrainWorldspace(71),
            ))
            .id();
        assert_eq!(
            resolve(&mut world, entity),
            Some(TerrainPrimitiveIdentity {
                worldspace_id: 71,
                grid,
                quadrant: 3,
                tier: None,
            })
        );
        world
            .entity_mut(entity)
            .get_mut::<TerrainWorldspace>()
            .unwrap()
            .0 = 72;
        assert_eq!(resolve(&mut world, entity).unwrap().worldspace_id, 72);
    }

    #[test]
    fn lod_mesh_inherits_one_exact_quadrant_regardless_of_tier_or_chunk_anchor() {
        let mut world = World::new();
        for tier in LodTier::ALL {
            for grid in [IVec2::new(-19, 37), IVec2::new(108, -209)] {
                let root = chunk(&mut world, tier, grid, 91);
                let wrapper = world.spawn_empty().id();
                let quadrant = world
                    .spawn(TerrainCoverage {
                        grid,
                        quadrant: 2,
                        tier: Some(tier),
                    })
                    .id();
                let primitive = mesh(&mut world);
                attach(&mut world, root, wrapper);
                attach(&mut world, wrapper, quadrant);
                attach(&mut world, quadrant, primitive);
                assert_eq!(
                    resolve(&mut world, primitive),
                    Some(TerrainPrimitiveIdentity {
                        worldspace_id: 91,
                        grid,
                        quadrant: 2,
                        tier: Some(tier),
                    })
                );
                // A coverage node itself is not the draw primitive.
                assert_eq!(resolve(&mut world, quadrant), None);
            }
        }
    }

    #[test]
    fn incomplete_or_out_of_range_identity_never_guesses_from_active_world() {
        let mut world = World::new();
        let bare = mesh(&mut world);
        assert_eq!(resolve(&mut world, bare), None);
        world.entity_mut(bare).insert(TerrainCoverage {
            grid: IVec2::ZERO,
            quadrant: 0,
            tier: None,
        });
        assert_eq!(resolve(&mut world, bare), None);
        world.entity_mut(bare).insert(TerrainWorldspace(7));
        assert!(resolve(&mut world, bare).is_some());
        world
            .entity_mut(bare)
            .get_mut::<TerrainCoverage>()
            .unwrap()
            .quadrant = 4;
        assert_eq!(resolve(&mut world, bare), None);

        let orphan = world
            .spawn(TerrainCoverage {
                grid: IVec2::new(400, -500),
                quadrant: 1,
                tier: Some(LodTier::Tier16),
            })
            .id();
        let primitive = mesh(&mut world);
        attach(&mut world, orphan, primitive);
        assert_eq!(resolve(&mut world, primitive), None);
    }

    #[test]
    fn duplicate_coverage_along_one_ancestry_is_ambiguous_even_if_equal() {
        let mut world = World::new();
        let coverage = TerrainCoverage {
            grid: IVec2::ONE,
            quadrant: 0,
            tier: None,
        };
        let parent = world.spawn((coverage, TerrainWorldspace(71))).id();
        let primitive = world.spawn((Mesh3d(Handle::default()), coverage)).id();
        attach(&mut world, parent, primitive);
        assert_eq!(resolve(&mut world, primitive), None);
    }

    #[test]
    fn conflicting_world_or_chunk_ownership_refuses_replacement() {
        let mut world = World::new();
        let grid = IVec2::new(25, 3);
        let parent = world.spawn(TerrainWorldspace(72)).id();
        let primitive = world
            .spawn((
                Mesh3d(Handle::default()),
                TerrainWorldspace(71),
                TerrainCoverage {
                    grid,
                    quadrant: 0,
                    tier: None,
                },
            ))
            .id();
        attach(&mut world, parent, primitive);
        assert_eq!(resolve(&mut world, primitive), None);

        let outer = chunk(&mut world, LodTier::Tier8, grid, 71);
        let inner = chunk(&mut world, LodTier::Tier8, grid, 72);
        let quadrant = world
            .spawn(TerrainCoverage {
                grid,
                quadrant: 0,
                tier: Some(LodTier::Tier8),
            })
            .id();
        let primitive = mesh(&mut world);
        attach(&mut world, outer, inner);
        attach(&mut world, inner, quadrant);
        attach(&mut world, quadrant, primitive);
        assert_eq!(resolve(&mut world, primitive), None);
    }

    #[test]
    fn lod_tier_must_match_owner_and_near_world_marker_cannot_override_it() {
        let mut world = World::new();
        let grid = IVec2::new(20, 30);
        let root = chunk(&mut world, LodTier::Tier8, grid, 71);
        let quadrant = world
            .spawn(TerrainCoverage {
                grid,
                quadrant: 0,
                tier: Some(LodTier::Tier16),
            })
            .id();
        let primitive = mesh(&mut world);
        attach(&mut world, root, quadrant);
        attach(&mut world, quadrant, primitive);
        assert_eq!(resolve(&mut world, primitive), None);
        world
            .entity_mut(quadrant)
            .get_mut::<TerrainCoverage>()
            .unwrap()
            .tier = Some(LodTier::Tier8);
        assert!(resolve(&mut world, primitive).is_some());
        world.entity_mut(primitive).insert(TerrainWorldspace(72));
        assert_eq!(resolve(&mut world, primitive), None);
    }
    #[test]
    fn terrain_tiers_do_not_share_coverage_across_worlds_or_unknown_ancestry() {
        let mut world = World::new();
        world.insert_resource(EngineConfig {
            worldspace_id: 77,
            ..default()
        });
        world.insert_resource(RenderOrigin(IVec2::ZERO));
        world.insert_resource(StreamingMetrics::default());
        world.insert_resource(ProfilingState::default());
        world.insert_resource(LodStreaming::default());
        world.init_resource::<TerrainLodIndex>();
        world.spawn((
            StreamingCamera,
            Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
        ));
        let coverage = TerrainCoverage {
            grid: IVec2::ZERO,
            quadrant: 0,
            tier: Some(LodTier::Tier4),
        };
        let origin = LodOrigin::new(0, 0);
        let root = |world: &mut World, worldspace_id| {
            world
                .spawn(LodChunkRoot {
                    key: ChunkKey::new(
                        worldspace_id,
                        LodTier::Tier4,
                        shared::lod::ChunkAnchor::new(0, 0),
                    ),
                    generation: 1,
                    origin,
                    retry_count: 0,
                })
                .id()
        };
        let current_root = root(&mut world, 77);
        let wrong_root = root(&mut world, 88);
        let current = world
            .spawn((
                coverage,
                TerrainSurfaceReady,
                Visibility::Hidden,
                ChildOf(current_root),
            ))
            .id();
        world.spawn((Mesh3d(Handle::default()), ChildOf(current)));
        world.spawn((Mesh3d(Handle::default()), ChildOf(current)));
        let other = world
            .spawn((
                coverage,
                TerrainSurfaceReady,
                Visibility::Inherited,
                ChildOf(wrong_root),
            ))
            .id();
        let unknown = world
            .spawn((coverage, TerrainSurfaceReady, Visibility::Inherited))
            .id();
        let near = world
            .spawn((
                Mesh3d(Handle::default()),
                super::super::TerrainPatch,
                super::super::TerrainWorldspace(88),
                TerrainCoverage {
                    tier: None,
                    ..coverage
                },
                TerrainSurfaceReady,
                Visibility::Inherited,
            ))
            .id();
        world
            .run_system_once(update_terrain_lod_visibility)
            .unwrap();
        assert_eq!(
            *world.get::<Visibility>(current).unwrap(),
            Visibility::Inherited,
            "a different world's near terrain cannot suppress this world's far tier"
        );
        assert_eq!(*world.get::<Visibility>(other).unwrap(), Visibility::Hidden);
        assert_eq!(
            *world.get::<Visibility>(unknown).unwrap(),
            Visibility::Hidden
        );
        world
            .entity_mut(near)
            .insert(super::super::TerrainWorldspace(77));
        world.resource_mut::<LodStreaming>().visibility_dirty = true;
        world
            .run_system_once(update_terrain_lod_visibility)
            .unwrap();
        assert_eq!(
            *world.get::<Visibility>(current).unwrap(),
            Visibility::Hidden,
            "same-world near terrain still replaces its far tier"
        );
    }
}

#[cfg(test)]
#[path = "lod_source_writer_tests.rs"]
mod source_writer_tests;

#[cfg(test)]
mod cluster_lod_source_tests {
    use super::*;
    use bevy::ecs::system::SystemState;

    #[derive(Resource, Default)]
    struct TestLodDistances(Option<crate::config::TerrainLodDistances>);

    fn plan(world: &mut World, demand: Option<LodDemand>) {
        world.init_resource::<TestLodDistances>();
        let mut state = SystemState::<(
            Commands,
            Res<EngineConfig>,
            Res<WorldDatabase>,
            Query<(&Visibility, Option<&super::super::RetireHidden>)>,
            Query<(&LodChunkRoot, Option<&PendingLodChunk>)>,
            ResMut<LodStreaming>,
            ResMut<StreamingCommitBudget>,
            ResMut<StreamingMetrics>,
            ResMut<ProfilingState>,
            ResMut<TestLodDistances>,
        )>::new(world);
        let (
            commands,
            config,
            database,
            visibility,
            roots,
            streaming,
            budget,
            metrics,
            profiler,
            mut previous,
        ) = state.get_mut(world).unwrap();
        plan_lod_demand(
            demand,
            commands,
            config,
            database,
            visibility,
            roots,
            streaming,
            budget,
            metrics,
            profiler,
            &mut previous.0,
        );
        state.apply(world);
    }

    #[test]
    fn cluster_mode_uses_certified_terrain_for_horizon_and_camera_reach() {
        use crate::cluster::terrain::{ClusterTerrain, TerrainBlock};

        let config = EngineConfig {
            clusters: Some("fixture".into()),
            cluster_terrain: true,
            ..default()
        };
        let worldspace = config.worldspace_id;
        let required_far = lod_camera_far(config.terrain_lod.max_reach_cells());
        let expected_reach = lod_horizon(&config.terrain_lod);
        let (database, _requests, _responses) = WorldDatabase::channel_fixture(0);
        let mut app = App::new();
        app.insert_resource(config)
            .insert_resource(database)
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(ClusterTerrain::for_blocks_test(vec![TerrainBlock {
                mesh_hash: "fixture".into(),
                primitive_index: 0,
                worldspace_id: worldspace,
                anchor: IVec2::ZERO,
                side_cells: 1,
                instance_path: "terrain/fixture.mci".into(),
                cells: vec![IVec2::ZERO],
                material: Handle::default(),
                batch: None,
            }]))
            .init_resource::<LodStreaming>()
            .init_resource::<LodHorizon>()
            .init_resource::<StreamingCommitBudget>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, plan_lod_chunks);
        let camera = app
            .world_mut()
            .spawn((
                Transform::default(),
                StreamingCamera,
                Projection::Perspective(PerspectiveProjection {
                    far: 10.0,
                    ..default()
                }),
            ))
            .id();
        app.update();
        assert_eq!(
            app.world().resource::<LodHorizon>().reach_in(worldspace),
            Some(expected_reach)
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .lod_queries_submitted,
            0
        );
        let demand = app.world().resource::<LodStreaming>().demand;
        assert_eq!(demand.unwrap().world, worldspace);
        let config = app.world().resource::<EngineConfig>().clone();
        let horizon = *app.world().resource::<LodHorizon>();
        let mut state =
            SystemState::<Query<&mut Projection, With<StreamingCamera>>>::new(app.world_mut());
        widen_lod_cameras(
            demand,
            &config,
            &horizon,
            &mut state.get_mut(app.world_mut()).unwrap(),
        );
        let Projection::Perspective(projection) = app.world().get::<Projection>(camera).unwrap()
        else {
            panic!("fixture camera must remain perspective");
        };
        assert_eq!(projection.far, required_far);
    }

    #[test]
    fn cluster_mode_submits_no_baked_terrain_queries_without_baked_tables() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("skyrim_world.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE schema_info(version INTEGER); INSERT INTO schema_info VALUES({});",
                shared::WORLD_DATABASE_SCHEMA_VERSION,
            ))
            .unwrap();
        drop(connection);
        let mut world = World::new();
        world.insert_resource(EngineConfig {
            clusters: Some("fixture".into()),
            cluster_terrain: true,
            ..default()
        });
        world.insert_resource(WorldDatabase::open_with(&path, Default::default()).unwrap());
        world.init_resource::<LodStreaming>();
        world.init_resource::<StreamingCommitBudget>();
        world.init_resource::<StreamingMetrics>();
        world.init_resource::<ProfilingState>();
        plan(
            &mut world,
            Some(LodDemand {
                world: 77,
                center: IVec2::ZERO,
            }),
        );
        assert_eq!(
            world.resource::<StreamingMetrics>().lod_queries_submitted,
            0
        );
        assert!(world.resource::<LodStreaming>().pending_queries.is_empty());
    }
}

#[cfg(test)]
mod admission_demand_tests {
    use super::*;

    #[test]
    fn demand_membership_checks_world_radius_and_wide_coordinate_differences() {
        let demand = LodDemand {
            world: 7,
            center: IVec2::new(i32::MIN, i32::MAX),
        };
        assert!(demand.contains_cell(7, demand.center, 0));
        assert!(!demand.contains_cell(8, demand.center, 1));
        assert!(!demand.contains_cell(7, demand.center, -1));
        assert!(!demand.contains_cell(7, IVec2::new(i32::MAX, i32::MIN), i32::MAX));
        assert!(demand.contains_cell(7, IVec2::new(i32::MIN + 1, i32::MAX - 1), 1));
    }
}

#[cfg(test)]
mod source_choice_tests {
    use super::*;

    fn app(
        selected: bool,
    ) -> (
        App,
        crossbeam_channel::Receiver<crate::world::database::DatabaseRequest>,
    ) {
        let (database, requests, _) = WorldDatabase::channel_fixture(16);
        let mut app = App::new();
        app.insert_resource(EngineConfig {
            clusters: selected.then(|| "selected".to_owned()),
            ..default()
        })
        .insert_resource(database)
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .init_resource::<LodStreaming>()
        .init_resource::<StreamingCommitBudget>()
        .init_resource::<StreamingMetrics>()
        .init_resource::<ProfilingState>()
        .add_systems(Update, plan_lod_chunks);
        app.world_mut()
            .spawn((StreamingCamera, Transform::default()));
        (app, requests)
    }

    #[test]
    fn selected_package_does_not_request_baked_lod_even_when_database_can_accept_it() {
        let (mut app, requests) = app(true);
        app.update();
        assert!(
            requests.is_empty(),
            "selected package must not start a native source fallback while its package is loading"
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .lod_queries_submitted,
            0
        );
        assert!(
            app.world()
                .resource::<LodStreaming>()
                .pending_queries
                .is_empty()
        );
    }

    #[test]
    fn ordinary_source_still_submits_native_lod_to_the_database() {
        let (mut app, requests) = app(false);
        app.update();
        assert!(
            !requests.is_empty(),
            "ordinary source choice must retain the real baked request path"
        );
        assert!(
            app.world()
                .resource::<StreamingMetrics>()
                .lod_queries_submitted
                > 0
        );
    }
}
