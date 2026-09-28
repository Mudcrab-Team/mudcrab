use crate::{
    config::EngineConfig,
    profiling::ProfilingState,
    render::{
        TerrainExtension, TerrainMaterial, WaterExtension, WaterMaterial, WaterReflectionTexture,
    },
    world::{
        cache::{CellCache, TerrainLayerSnapshot, TerrainSnapshot},
        components::{
            CELL_SIZE, CellRef, ExpectedModelBounds, ExteriorCellGrid, FormId, InstanceBounds,
            MeshHandle, StreamedCellRoot, StreamingCamera, TerrainPatch, WaterSurface,
            WorldPosition, WorldTransform,
        },
        database::{AssetCatalog, CellKey, CellPayload, DatabaseRequest, WorldDatabase},
    },
};
use bevy::{
    app::SceneSpawnerSystems,
    asset::{LoadState, RecursiveDependencyLoadState, RenderAssetUsages},
    camera::primitives::MeshAabb,
    gltf::GltfExtras,
    image::{ImageFilterMode, ImageLoaderSettings, ImageSampler},
    math::Affine3A,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    world_serialization::{WorldInstance, WorldInstanceReady},
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::error::Error as StdError;
use std::time::Instant;

// Wall-clock spans can include a short OS scheduler preemption. Keep the raw maximum in metrics,
// but require a material overrun before classifying the frame as a commit-budget violation.
const COMMIT_BUDGET_SCHEDULER_TOLERANCE_MICROS: u64 = 1_000;

fn commit_budget_exceeded(elapsed_micros: u64, budget_micros: u64) -> bool {
    elapsed_micros > budget_micros.saturating_add(COMMIT_BUDGET_SCHEDULER_TOLERANCE_MICROS)
}

#[cfg(test)]
use bevy::mesh::VertexAttributeValues;

pub struct StreamingPlugin;

impl Plugin for StreamingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StreamingWorld>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<DiagnosticFallbackAssets>()
            .init_resource::<TerrainContinuity>()
            .init_resource::<SceneSpawnBatch>()
            .add_observer(mark_world_instance_ready)
            .add_systems(
                Update,
                (
                    plan_cells,
                    despawn_cells,
                    collect_cells,
                    arm_pending_models,
                    track_asset_readiness,
                    track_surface_readiness,
                    update_render_origin,
                    validate_streaming_lifecycle,
                )
                    .chain(),
            )
            // Bevy instantiates every ready converted model in one unbudgeted pass inside
            // `SceneSpawnerSystems::WorldInstanceSpawn`; these two systems bracket that pass so the
            // profile can tell a frame that spawned a batch from one that spawned nothing.
            .add_systems(
                SpawnScene,
                (
                    begin_scene_spawn_batch.before(SceneSpawnerSystems::WorldInstanceSpawn),
                    end_scene_spawn_batch.after(SceneSpawnerSystems::WorldInstanceSpawn),
                ),
            );
    }
}

#[derive(Resource, Default)]
pub struct StreamingWorld {
    generation: u64,
    /// Spawn order for the models [`arm_pending_models`] has yet to arm, so a backlog drains oldest
    /// first whatever order the queries iterate in.
    next_model_sequence: u64,
    cells: HashMap<CellKey, CellStatus>,
}

impl StreamingWorld {
    /// Submits one load for `key` unless it is already loading or resident, and records the
    /// request on the streaming metrics. This is the loader path every cell goes through, however
    /// the request is driven: the camera planner streams exteriors from it, and the streaming
    /// fixture loads an interior from it by id, because this tree has no runtime path that
    /// switches the active space to an interior on its own.
    pub(crate) fn request_cell(
        &mut self,
        database: &WorldDatabase,
        key: CellKey,
        metrics: &mut StreamingMetrics,
        profiler: &mut ProfilingState,
    ) {
        if self.cells.contains_key(&key) {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        if database
            .request(DatabaseRequest::Load {
                generation,
                key,
                queued_at: Instant::now(),
            })
            .is_ok()
        {
            metrics.requests_submitted += 1;
            profiler.increment("streaming/requests", 1);
            profiler.event(format!("{key:?}"), "requested", None);
            self.cells.insert(key, CellStatus::Loading { generation });
        }
    }
}

#[derive(Resource, Debug, Clone, Default, Serialize)]
pub struct StreamingMetrics {
    pub requests_submitted: u64,
    pub responses_received: u64,
    pub stale_responses: u64,
    pub failed_cells: u64,
    pub unloaded_cells: u64,
    /// Cell roots despawned in the most recent frame, and the largest value that counter reached.
    pub despawns_this_frame: u64,
    pub max_despawns_per_frame: u64,
    /// Entities removed by those despawns, counted over each root's whole subtree.
    pub despawned_entities: u64,
    /// Converted models Bevy instantiated in the most recent frame, and the largest value that
    /// counter reached. An instance is counted on the frame its entities are written into the
    /// world, which is the frame the spawn batch's cost lands on.
    pub instances_spawned_this_frame: u64,
    pub max_instances_spawned_per_frame: u64,
    /// Models whose converted scene is loaded but which have not been handed to the spawner yet,
    /// waiting for their turn in the arming budget, and the largest backlog seen.
    pub arming_queue_depth: usize,
    pub peak_arming_queue_depth: usize,
    /// Models handed to Bevy's spawner in the most recent frame, and the largest value that counter
    /// reached. While the arming pacer is unlimited this is the frame a cell commits; once models
    /// are armed only when their asset is loaded, it is the frame the instance spawns.
    pub instances_armed_this_frame: u64,
    pub max_instances_armed_per_frame: u64,
    /// Model instances the readiness scan validated to completion in its most recent run, and the
    /// largest value that counter reached.
    pub instances_completed_this_scan: u64,
    pub max_instances_completed_per_scan: u64,
    /// Cells outside the unload radius that are waiting for their turn in the unload budget, and
    /// the largest backlog seen.
    pub retiring_cells: usize,
    pub peak_retiring_cells: usize,
    /// Retiring cells that came back into range before their turn and kept the root they had.
    pub revived_cells: u64,
    /// Frames where the backlog passed [`retire_backlog_bound`] and every retiring cell was
    /// unloaded at once.
    pub retire_backlog_overflows: u64,
    pub resident_cells: usize,
    pub loading_cells: usize,
    pub peak_resident_cells: usize,
    pub peak_loading_cells: usize,
    pub total_query_micros: u64,
    pub max_query_micros: u64,
    pub max_commit_micros: u64,
    pub total_frame_commit_micros: u64,
    pub max_frame_commit_micros: u64,
    pub commit_frames: u64,
    pub commit_budget_micros: u64,
    pub commit_budget_violations: u64,
    pub total_queue_wait_micros: u64,
    pub max_queue_wait_micros: u64,
    pub total_request_micros: u64,
    pub max_request_micros: u64,
    pub total_rows_loaded: u64,
    pub assets_ready: u64,
    pub asset_load_failures: u64,
    pub max_asset_ready_micros: u64,
    pub pending_asset_instances: usize,
    pub pending_surface_instances: usize,
    pub meshes_validated: u64,
    pub materials_validated: u64,
    pub images_validated: u64,
    pub material_validation_failures: u64,
    pub diagnostic_fallbacks: u64,
    pub canonical_fixture_validated: bool,
    pub terrain_patches_validated: u64,
    pub terrain_seams_validated: u64,
    pub terrain_validation_failures: u64,
    pub water_surfaces_validated: u64,
    pub water_validation_failures: u64,
    pub terrain_water_fixture_validated: bool,
    pub transform_instances_validated: u64,
    pub transform_nodes_validated: u64,
    pub bounds_validated: u64,
    /// References whose converted model is an empty scene: a glTF scene with no node and no mesh,
    /// which is what the converter writes for a model whose NIF has no renderable geometry (an
    /// editor-marker-only model, for example). There is nothing to place, draw or bound, so such
    /// a reference is counted here rather than in [`Self::transform_bounds_validation_failures`],
    /// which is a hard gate and must count only real conversion defects. The tolerance stops
    /// there: a scene an exporter emptied by mistake looks exactly like one with nothing to
    /// export, so every empty scene is counted here, where the profiling reports can see it.
    pub empty_model_references: u64,
    pub transform_bounds_validation_failures: u64,
    pub transform_bounds_fixture_validated: bool,
    pub active_requests: usize,
    pub peak_active_requests: usize,
    pub resident_roots: usize,
    pub duplicate_cell_roots: u64,
    pub orphaned_cell_roots: u64,
    pub missing_cell_roots: u64,
    pub out_of_range_cell_roots: u64,
    pub streaming_invariant_failures: u64,
    pub origin_rebases: u64,
    pub streaming_fixture_validated: bool,
    pub streaming_fixture_failures: u64,
    pub asset_failures: Vec<AssetFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetFailure {
    pub model_path: String,
    pub reference_form_id: u32,
    pub base_form_id: u32,
    pub cell_id: u32,
    pub dependency_chain: Vec<String>,
}

#[derive(Resource, Default)]
struct DiagnosticFallbackAssets {
    mesh: Option<Handle<Mesh>>,
    material: Option<Handle<StandardMaterial>>,
}

#[derive(Resource, Default)]
struct TerrainContinuity {
    edges: HashMap<CellKey, TerrainEdges>,
}

#[derive(Clone)]
struct TerrainEdges {
    west: Vec<f32>,
    east: Vec<f32>,
    south: Vec<f32>,
    north: Vec<f32>,
}

enum CellStatus {
    Loading {
        generation: u64,
    },
    Resident {
        root: Entity,
    },
    /// Outside the unload radius, waiting for its turn in the unload budget. The map entry and the
    /// root stay, so a cell that comes back into range before its turn is revived as it is instead
    /// of being despawned and requested again. A retiring cell is still drawn until its turn; that
    /// is harmless while the camera stays in one worldspace (retiring cells are the far ones), but a
    /// future runtime worldspace change must unload them at once rather than pace them.
    Retiring {
        root: Entity,
    },
    Failed,
}

#[derive(Resource, Debug, Clone, Copy)]
pub struct RenderOrigin(pub IVec2);

#[allow(clippy::too_many_arguments)]
fn plan_cells(
    config: Res<EngineConfig>,
    database: Res<WorldDatabase>,
    origin: Res<RenderOrigin>,
    camera: Query<&Transform, With<StreamingCamera>>,
    mut streaming: ResMut<StreamingWorld>,
    mut continuity: ResMut<TerrainContinuity>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    let plan_started = Instant::now();
    let Ok(camera) = camera.single() else {
        return;
    };
    let center = streaming_center(camera.translation, origin.0);
    let mut wanted = HashSet::new();
    for y in -config.stream_radius..=config.stream_radius {
        for x in -config.stream_radius..=config.stream_radius {
            wanted.insert(CellKey::Exterior {
                worldspace_id: config.worldspace_id,
                grid_x: center.x + x,
                grid_y: center.y + y,
            });
        }
    }
    for key in &wanted {
        // A retiring cell is still in the map, so `request_cell` never requests it a second time:
        // the retain pass below revives it with the root it kept.
        streaming.request_cell(&database, *key, &mut metrics, &mut profiler);
    }
    let mut revived = 0u64;
    streaming.cells.retain(|key, status| {
        if cell_within_unload_radius(*key, center, config.unload_radius) {
            if let CellStatus::Retiring { root } = status {
                *status = CellStatus::Resident { root: *root };
                revived += 1;
            }
            return true;
        }
        match status {
            CellStatus::Resident { root } => {
                *status = CellStatus::Retiring { root: *root };
                true
            }
            // Already waiting for its turn in the budget.
            CellStatus::Retiring { .. } => true,
            // Nothing was spawned, so there is no subtree to pace: drop the entry now and let the
            // in-flight response turn into a stale one.
            CellStatus::Loading { .. } | CellStatus::Failed => {
                continuity.edges.remove(key);
                profiler.event(format!("{key:?}"), "unload_dropped", None);
                false
            }
        }
    });
    metrics.resident_cells = streaming
        .cells
        .values()
        .filter(|status| matches!(status, CellStatus::Resident { .. }))
        .count();
    metrics.loading_cells = streaming
        .cells
        .values()
        .filter(|status| matches!(status, CellStatus::Loading { .. }))
        .count();
    metrics.retiring_cells = streaming
        .cells
        .values()
        .filter(|status| matches!(status, CellStatus::Retiring { .. }))
        .count();
    metrics.peak_resident_cells = metrics.peak_resident_cells.max(metrics.resident_cells);
    metrics.peak_loading_cells = metrics.peak_loading_cells.max(metrics.loading_cells);
    metrics.peak_retiring_cells = metrics.peak_retiring_cells.max(metrics.retiring_cells);
    if revived > 0 {
        metrics.revived_cells = metrics.revived_cells.saturating_add(revived);
        profiler.increment("streaming/revived_cells", revived);
    }
    profiler.set_gauge("streaming/resident_cells", metrics.resident_cells as f64);
    profiler.set_gauge("streaming/loading_cells", metrics.loading_cells as f64);
    profiler.set_gauge("streaming/retiring_cells", metrics.retiring_cells as f64);
    profiler.record_elapsed("streaming/plan_cells", plan_started);
}

/// How many cells may wait for their turn in the unload budget before the pacer gives up on pacing
/// and unloads every one of them in a single frame.
///
/// One whole window at the unload radius, `(2 * r + 1)^2` cells: the most a single crossing or
/// teleport can retire at once. Both are paced, so the overflow path only fires when sustained
/// flight keeps retiring cells faster than the budget unloads them, the case where holding on to
/// the backlog costs more than one larger frame.
fn retire_backlog_bound(unload_radius: i32) -> usize {
    let side = 2 * unload_radius.max(0) as usize + 1;
    side * side
}

/// Stable order for the unload budget, so a replayed crossing unloads the same cells on the same
/// frames whatever order the status map iterates in.
fn cell_order_key(key: CellKey) -> (u32, i32, i32, u32) {
    match key {
        CellKey::Exterior {
            worldspace_id,
            grid_x,
            grid_y,
        } => (0, grid_y, grid_x, worldspace_id),
        CellKey::Interior(cell_id) => (1, 0, 0, cell_id),
    }
}

/// Unloads retiring cells, at most `max_cell_unloads_per_frame` of them per frame.
///
/// This is an exclusive system on purpose: a command-buffered `despawn` only queues the removal and
/// the real work happens later at a schedule sync point, where no span can see it — and the whole
/// point of the budget is to keep that work off one frame.
fn despawn_cells(world: &mut World) {
    let (budget, backlog_bound) = {
        let config = world.resource::<EngineConfig>();
        (
            config.max_cell_unloads_per_frame.max(1),
            retire_backlog_bound(config.unload_radius),
        )
    };
    let mut retiring: Vec<(CellKey, Entity)> = world
        .resource::<StreamingWorld>()
        .cells
        .iter()
        .filter_map(|(key, status)| match status {
            CellStatus::Retiring { root } => Some((*key, *root)),
            _ => None,
        })
        .collect();
    if retiring.is_empty() {
        world.resource_mut::<StreamingMetrics>().despawns_this_frame = 0;
        world
            .resource_mut::<ProfilingState>()
            .set_gauge("streaming/despawns_this_frame", 0.0);
        return;
    }
    retiring.sort_by_key(|(key, _)| cell_order_key(*key));
    let backlog = retiring.len();
    let overflow = backlog > backlog_bound;
    let budget = if overflow {
        backlog
    } else {
        budget.min(backlog)
    };
    let started = Instant::now();
    let mut entities = 0usize;
    for (_, root) in retiring.iter().take(budget) {
        entities = entities.saturating_add(despawn_subtree(world, *root));
    }
    {
        let mut streaming = world.resource_mut::<StreamingWorld>();
        for (key, _) in retiring.iter().take(budget) {
            streaming.cells.remove(key);
        }
    }
    for (key, _) in retiring.iter().take(budget) {
        world.resource_mut::<TerrainContinuity>().edges.remove(key);
        world
            .resource_mut::<ProfilingState>()
            .event(format!("{key:?}"), "unloaded", None);
    }
    let cells = budget as u64;
    let remaining = backlog - budget;
    let max_despawns_per_frame = {
        let mut metrics = world.resource_mut::<StreamingMetrics>();
        metrics.retiring_cells = remaining;
        metrics.peak_retiring_cells = metrics.peak_retiring_cells.max(backlog);
        metrics.unloaded_cells = metrics.unloaded_cells.saturating_add(cells);
        metrics.despawns_this_frame = cells;
        metrics.despawned_entities = metrics.despawned_entities.saturating_add(entities as u64);
        metrics.max_despawns_per_frame = metrics.max_despawns_per_frame.max(cells);
        if overflow {
            metrics.retire_backlog_overflows = metrics.retire_backlog_overflows.saturating_add(1);
        }
        metrics.max_despawns_per_frame
    };
    let mut profiler = world.resource_mut::<ProfilingState>();
    profiler.record_elapsed("streaming/cell_despawn", started);
    profiler.increment("streaming/despawned_cells", cells);
    profiler.increment("streaming/despawned_entities", entities as u64);
    profiler.set_gauge("streaming/despawns_this_frame", cells as f64);
    profiler.set_gauge(
        "streaming/max_despawns_per_frame",
        max_despawns_per_frame as f64,
    );
    profiler.set_gauge("streaming/retiring_cells", remaining as f64);
    if overflow {
        profiler.increment("streaming/retire_backlog_overflows", 1);
        profiler.event("streaming", "retire_backlog_overflow", Some(backlog as f64));
        warn!(
            backlog,
            bound = backlog_bound,
            "retire backlog exceeded its bound; unloading every retiring cell at once"
        );
    }
}

/// Despawns a cell root and everything under it, returning the number of entities that removes.
///
/// The count walks `Children` first, so the batch span covers the accounting as well as the
/// recursive removal. The walk is only paid on frames that unload cells.
fn despawn_subtree(world: &mut World, root: Entity) -> usize {
    let mut entities = 0usize;
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        entities += 1;
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
    }
    if let Ok(entity) = world.get_entity_mut(root) {
        entity.despawn();
    }
    entities
}

#[allow(clippy::too_many_arguments)]
fn collect_cells(
    mut commands: Commands,
    config: Res<EngineConfig>,
    database: Res<WorldDatabase>,
    cache: Res<CellCache>,
    origin: Res<RenderOrigin>,
    asset_server: Res<AssetServer>,
    catalog: Res<AssetCatalog>,
    reflection: Res<WaterReflectionTexture>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut terrain_materials: ResMut<Assets<TerrainMaterial>>,
    mut water_materials: ResMut<Assets<WaterMaterial>>,
    mut streaming: ResMut<StreamingWorld>,
    mut continuity: ResMut<TerrainContinuity>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    let frame_commit_started = Instant::now();
    let mut commits_this_frame = 0u64;
    for _ in 0..config.max_cell_commits_per_frame {
        let Some(response) = database.try_response() else {
            break;
        };
        metrics.responses_received += 1;
        metrics.total_query_micros = metrics
            .total_query_micros
            .saturating_add(response.query_micros);
        metrics.max_query_micros = metrics.max_query_micros.max(response.query_micros);
        metrics.total_queue_wait_micros = metrics
            .total_queue_wait_micros
            .saturating_add(response.queue_wait_micros);
        metrics.max_queue_wait_micros = metrics
            .max_queue_wait_micros
            .max(response.queue_wait_micros);
        metrics.total_request_micros = metrics
            .total_request_micros
            .saturating_add(response.total_request_micros);
        metrics.max_request_micros = metrics
            .max_request_micros
            .max(response.total_request_micros);
        metrics.total_rows_loaded = metrics
            .total_rows_loaded
            .saturating_add(response.row_count as u64);
        profiler.record_micros("streaming/db_queue_wait", response.queue_wait_micros);
        profiler.record_micros("streaming/db_query", response.query_micros);
        profiler.record_micros("streaming/db_request_total", response.total_request_micros);
        let Some(CellStatus::Loading { generation }) = streaming.cells.get(&response.key) else {
            metrics.stale_responses += 1;
            profiler.event(format!("{:?}", response.key), "stale_discarded", None);
            continue;
        };
        if *generation != response.generation {
            metrics.stale_responses += 1;
            profiler.event(format!("{:?}", response.key), "stale_generation", None);
            continue;
        }
        let commit_started = std::time::Instant::now();
        match response.result {
            Ok(payload) => {
                let terrain = cache.terrain(payload.cell_id);
                if let Some(terrain) = &terrain {
                    let validation = validate_terrain_snapshot(terrain, &catalog).and_then(|()| {
                        validate_and_register_terrain_edges(
                            payload.key,
                            terrain,
                            &mut continuity,
                            &mut metrics,
                        )
                    });
                    if let Err(reason) = validation {
                        error!(cell = format_args!("{:08X}", payload.cell_id), %reason, "LAND failed strict validation");
                        metrics.failed_cells = metrics.failed_cells.saturating_add(1);
                        metrics.terrain_validation_failures =
                            metrics.terrain_validation_failures.saturating_add(1);
                        metrics.asset_failures.push(AssetFailure {
                            model_path: format!("terrain/{:08X}", payload.cell_id),
                            reference_form_id: 0,
                            base_form_id: 0,
                            cell_id: payload.cell_id,
                            dependency_chain: vec![reason],
                        });
                        profiler.increment("terrain/validation_failures", 1);
                        streaming.cells.insert(response.key, CellStatus::Failed);
                        continue;
                    }
                }
                let root = spawn_cell(
                    &mut commands,
                    &asset_server,
                    &catalog,
                    &reflection,
                    &mut meshes,
                    &mut terrain_materials,
                    &mut water_materials,
                    origin.0,
                    payload,
                    terrain,
                    &mut streaming.next_model_sequence,
                    &mut profiler,
                );
                streaming
                    .cells
                    .insert(response.key, CellStatus::Resident { root });
            }
            Err(error) => {
                debug!(?response.key, %error, "cell could not be streamed");
                streaming.cells.insert(response.key, CellStatus::Failed);
                metrics.failed_cells += 1;
                profiler.increment("streaming/failed_cells", 1);
            }
        }
        let commit_micros = commit_started
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        metrics.max_commit_micros = metrics.max_commit_micros.max(commit_micros);
        commits_this_frame = commits_this_frame.saturating_add(1);
        profiler.record_micros("streaming/cell_commit", commit_micros);
        profiler.event(
            format!("{:?}", response.key),
            "committed",
            Some(commit_micros as f64 / 1000.0),
        );
    }
    if commits_this_frame > 0 {
        let frame_micros = frame_commit_started
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        metrics.commit_frames = metrics.commit_frames.saturating_add(1);
        metrics.total_frame_commit_micros = metrics
            .total_frame_commit_micros
            .saturating_add(frame_micros);
        metrics.max_frame_commit_micros = metrics.max_frame_commit_micros.max(frame_micros);
        metrics.commit_budget_micros = config.max_commit_micros_per_frame;
        if commit_budget_exceeded(frame_micros, config.max_commit_micros_per_frame) {
            metrics.commit_budget_violations = metrics.commit_budget_violations.saturating_add(1);
            profiler.event(
                "streaming",
                "commit_budget_exceeded",
                Some(frame_micros as f64 / 1_000.0),
            );
        }
        profiler.set_gauge("streaming/commits_this_frame", commits_this_frame as f64);
        profiler.record_micros("streaming/frame_commit", frame_micros);
    }
}

/// The exterior grid square the camera is over: its rebased translation, put back through the
/// render origin. The planner streams from this square and the lifecycle checks measure against
/// it, so both read the same one.
pub(crate) fn streaming_center(translation: Vec3, origin: IVec2) -> IVec2 {
    let global_x = translation.x + origin.x as f32 * CELL_SIZE;
    let global_y = -translation.z + origin.y as f32 * CELL_SIZE;
    IVec2::new(
        (global_x / CELL_SIZE).floor() as i32,
        (global_y / CELL_SIZE).floor() as i32,
    )
}

/// Whether a loaded cell stays loaded. Exteriors fall out of the radius the camera carries; an
/// interior has no grid square to fall out of, so it stays until something unloads it, and no
/// runtime path unloads one yet.
fn cell_within_unload_radius(key: CellKey, center: IVec2, radius: i32) -> bool {
    match key {
        CellKey::Exterior { grid_x, grid_y, .. } => {
            (grid_x - center.x).abs() <= radius && (grid_y - center.y).abs() <= radius
        }
        CellKey::Interior(_) => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_cell(
    commands: &mut Commands,
    asset_server: &AssetServer,
    catalog: &AssetCatalog,
    reflection: &WaterReflectionTexture,
    meshes: &mut Assets<Mesh>,
    terrain_materials: &mut Assets<TerrainMaterial>,
    water_materials: &mut Assets<WaterMaterial>,
    origin: IVec2,
    payload: CellPayload,
    terrain: Option<TerrainSnapshot>,
    model_sequence: &mut u64,
    profiler: &mut ProfilingState,
) -> Entity {
    let spawn_started = Instant::now();
    let reference_count = payload.references.len();
    let root_translation = cell_translation(payload.key, origin);
    let mut root_commands = commands.spawn((
        Name::new(format!("Cell {:08X}", payload.cell_id)),
        CellRef(payload.cell_id),
        StreamedCellRoot,
        Transform::from_translation(root_translation),
        Visibility::default(),
    ));
    if let CellKey::Exterior { grid_x, grid_y, .. } = payload.key {
        root_commands.insert(ExteriorCellGrid(IVec2::new(grid_x, grid_y)));
    }
    let root = root_commands.id();
    commands.entity(root).with_children(|parent| {
        if let Some(terrain) = terrain {
            for quadrant in 0..4 {
                let started = Instant::now();
                let mesh = build_terrain_quadrant_mesh(&terrain, quadrant)
                    .expect("validated terrain must build");
                profiler.record_elapsed("streaming/terrain_mesh", started);
                let (extension, images) =
                    TerrainExtension::from_quadrant(&terrain, quadrant, catalog, asset_server)
                        .expect("validated terrain material must build");
                let material = terrain_materials.add(TerrainMaterial {
                    base: StandardMaterial {
                        base_color: Color::WHITE,
                        perceptual_roughness: 0.92,
                        cull_mode: None,
                        double_sided: true,
                        ..default()
                    },
                    extension,
                });
                parent.spawn((
                    Name::new(format!("Terrain quadrant {quadrant}")),
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(material),
                    Transform::default(),
                    TerrainPatch,
                    Visibility::Hidden,
                    PendingTerrainProfile {
                        cell_id: terrain.cell_id,
                        quadrant,
                        images,
                    },
                ));
            }
            if let Some(height) = terrain
                .water_height
                .filter(|height| height.is_finite() && height.abs() < 1.0e7)
            {
                let water_mesh = meshes.add(Plane3d::default().mesh().size(CELL_SIZE, CELL_SIZE));
                let flow_normal = terrain
                    .water_type_form_id
                    .and_then(|form_id| catalog.water_flow(form_id))
                    .map(|path| {
                        asset_server
                            .load_builder()
                            .with_settings(|settings: &mut ImageLoaderSettings| {
                                settings.is_srgb = false;
                            })
                            .load(path.to_owned())
                    });
                let water_material = water_materials.add(WaterMaterial {
                    base: StandardMaterial {
                        base_color: Color::srgba(0.05, 0.2, 0.32, 0.68),
                        metallic: 0.15,
                        perceptual_roughness: 0.06,
                        reflectance: 0.9,
                        alpha_mode: AlphaMode::Blend,
                        ..default()
                    },
                    extension: WaterExtension::with_reflection(
                        reflection.0.clone(),
                        flow_normal.clone(),
                    ),
                });
                parent.spawn((
                    Name::new("Water"),
                    Mesh3d(water_mesh),
                    MeshMaterial3d(water_material),
                    Transform::from_translation(Vec3::new(
                        CELL_SIZE * 0.5,
                        height,
                        -CELL_SIZE * 0.5,
                    )),
                    WaterSurface,
                    Visibility::Hidden,
                    PendingWaterProfile {
                        cell_id: terrain.cell_id,
                        flow_normal,
                    },
                    bevy::camera::visibility::RenderLayers::layer(1),
                ));
            }
        }
        for reference in payload.references {
            let creation_position = Vec3::from_array(reference.position);
            let world_position = WorldPosition::from_creation_units(creation_position);
            let translation = match payload.key {
                CellKey::Exterior { grid_x, grid_y, .. } => {
                    let cell_origin = IVec2::new(grid_x, grid_y);
                    creation_to_bevy(world_position.relative_to(cell_origin))
                }
                CellKey::Interior(_) => creation_to_bevy(creation_position),
            };
            let rotation = creation_rotation_to_bevy(reference.rotation);
            let transform = Transform::from_translation(translation)
                .with_rotation(rotation)
                .with_scale(Vec3::splat(reference.scale));
            let model_bounds = reference.bounds_valid.then(|| {
                ExpectedModelBounds::new(
                    Vec3::from_array(reference.bounds_min),
                    Vec3::from_array(reference.bounds_max),
                )
            });
            let model_bounds = model_bounds.flatten();
            let bounds = model_bounds.map(|bounds| {
                InstanceBounds::transformed(bounds.min, bounds.max, transform.to_matrix())
            });
            let mut entity = parent.spawn((
                Name::new(format!("Reference {:08X}", reference.form_id)),
                FormId(reference.form_id),
                CellRef(reference.cell_id),
                world_position,
                WorldTransform(transform.to_matrix()),
                transform,
            ));
            if let Some(bounds) = bounds.zip(model_bounds) {
                entity.insert(bounds);
            }
            if let Some(path) = reference.model_path.and_then(converted_model_path) {
                let sequence = *model_sequence;
                *model_sequence = model_sequence.saturating_add(1);
                entity.insert((
                    MeshHandle(path.clone()),
                    PendingModel {
                        handle: asset_server
                            .load(GltfAssetLabel::Scene(0).from_asset(path.clone())),
                        sequence,
                    },
                    PendingAssetProfile {
                        started: Instant::now(),
                        scene_spawned: false,
                        path,
                        form_id: reference.form_id,
                        base_form_id: reference.base_form_id,
                        cell_id: reference.cell_id,
                    },
                ));
            }
        }
    });
    profiler.increment("streaming/references_spawned", reference_count as u64);
    profiler.record_elapsed("streaming/spawn_cell", spawn_started);
    root
}

/// A converted model [`spawn_cell`] found for a reference, waiting for its turn in the arming
/// budget.
///
/// Bevy instantiates every model it is handed in one unbudgeted pass, so handing it a whole cell's
/// models at once is what makes an arrival frame expensive. [`arm_pending_models`] replaces this
/// component with [`WorldAssetRoot`] once the converted scene is loaded and the budget allows it.
/// A model whose cell is unloaded before its turn needs no cleanup: this is a component on that
/// cell's subtree, not an entry in a queue of its own, so it disappears with the subtree.
#[derive(Component)]
struct PendingModel {
    handle: Handle<WorldAsset>,
    /// Where the reference was in spawn order, so a backlog is armed oldest first.
    sequence: u64,
}

#[derive(Component)]
struct PendingAssetProfile {
    started: Instant,
    scene_spawned: bool,
    path: String,
    form_id: u32,
    base_form_id: u32,
    cell_id: u32,
}

#[derive(Component)]
struct PendingTerrainProfile {
    cell_id: u32,
    quadrant: u8,
    images: Vec<Handle<Image>>,
}

#[derive(Component)]
struct PendingWaterProfile {
    cell_id: u32,
    flow_normal: Option<Handle<Image>>,
}

type RenderPrimitiveQuery<'world, 'state> = Query<
    'world,
    'state,
    (
        &'static Mesh3d,
        Option<&'static MeshMaterial3d<StandardMaterial>>,
        Option<&'static GltfExtras>,
    ),
>;

type PendingAssetQuery<'world, 'state> = Query<
    'world,
    'state,
    (
        Entity,
        &'static WorldAssetRoot,
        &'static PendingAssetProfile,
        &'static Transform,
        &'static GlobalTransform,
        &'static WorldTransform,
        Option<&'static ExpectedModelBounds>,
    ),
>;

/// Wall-clock timing for the pass Bevy spends instantiating converted models.
#[derive(Resource, Default)]
struct SceneSpawnBatch {
    started: Option<Instant>,
}

fn begin_scene_spawn_batch(mut batch: ResMut<SceneSpawnBatch>) {
    batch.started = Some(Instant::now());
}

/// Records the cost of Bevy's world-instance spawn pass and counts what the pass did.
///
/// The counts read the world rather than Bevy's instance bookkeeping: a reference gains a child the
/// moment its converted scene is written below it, so `Changed<Children>` on a reference that owns
/// an instance is exactly "this model was instantiated this frame". `WorldInstance` itself is
/// inserted when the reference joins the spawner's queue
/// (`bevy_world_serialization::world_asset_spawner`, `world_instance_spawner`) - which happens
/// whether or not the asset has landed - so `Added<WorldInstance>` counts the models *handed to*
/// the spawner this frame, the same thing as spawned only once arming waits for the asset.
fn end_scene_spawn_batch(
    mut batch: ResMut<SceneSpawnBatch>,
    spawned: Query<(), (With<WorldInstance>, Changed<Children>)>,
    armed: Query<(), Added<WorldInstance>>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    if let Some(started) = batch.started.take() {
        profiler.record_elapsed("scene/spawn_batch", started);
    }
    let spawned = spawned.iter().count() as u64;
    let armed = armed.iter().count() as u64;
    metrics.instances_spawned_this_frame = spawned;
    metrics.max_instances_spawned_per_frame = metrics.max_instances_spawned_per_frame.max(spawned);
    metrics.instances_armed_this_frame = armed;
    metrics.max_instances_armed_per_frame = metrics.max_instances_armed_per_frame.max(armed);
    profiler.set_gauge("scene/instances_spawned_this_frame", spawned as f64);
    profiler.set_gauge("scene/instances_armed_this_frame", armed as f64);
}

/// Hands loaded converted models to Bevy's scene spawner, at most `max_model_spawns_per_frame` of
/// them per frame.
///
/// Bevy instantiates every model whose converted scene is ready in the frame it is handed over, in
/// `SceneSpawnerSystems::WorldInstanceSpawn`, so the batch a frame pays to instantiate is the batch
/// this system lets through. The budget is spent on the models that are ready and skipped over the
/// ones that are still loading, so a slow load cannot hold the models behind it back; among the
/// ready ones the oldest reference is armed first. `0` arms every ready model, the unbudgeted
/// behaviour the engine had before this budget existed.
fn arm_pending_models(
    config: Res<EngineConfig>,
    asset_server: Res<AssetServer>,
    world_assets: Res<Assets<WorldAsset>>,
    pending: Query<(Entity, &PendingModel)>,
    mut commands: Commands,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    let started = Instant::now();
    let backlog = pending.iter().count();
    let mut ready: Vec<(u64, Entity, Handle<WorldAsset>)> = pending
        .iter()
        .filter(|(_, model)| model_can_spawn(&world_assets, &asset_server, &model.handle))
        .map(|(entity, model)| (model.sequence, entity, model.handle.clone()))
        .collect();
    ready.sort_by_key(|(sequence, _, _)| *sequence);
    let budget = config.max_model_spawns_per_frame;
    let armed = if budget == 0 {
        ready.len()
    } else {
        budget.min(ready.len())
    };
    for (_, entity, handle) in ready.drain(..armed) {
        commands
            .entity(entity)
            .insert(WorldAssetRoot(handle))
            .remove::<PendingModel>();
    }
    // The depth is what is left waiting once the budget has been spent; the peak is the backlog the
    // pacer was handed, which is the number that says how far behind on a burst it is.
    let depth = backlog.saturating_sub(armed);
    metrics.arming_queue_depth = depth;
    metrics.peak_arming_queue_depth = metrics.peak_arming_queue_depth.max(backlog);
    profiler.increment("streaming/models_armed", armed as u64);
    profiler.set_gauge("streaming/arming_queue_depth", depth as f64);
    profiler.set_gauge(
        "streaming/peak_arming_queue_depth",
        metrics.peak_arming_queue_depth as f64,
    );
    profiler.record_elapsed("streaming/arm_models", started);
}

/// Whether Bevy can instantiate this model now.
///
/// The converted scene being in `Assets<WorldAsset>` is exactly the condition the spawner retries
/// on, so arming on it hands a model over on the first frame it can actually spawn. A model whose
/// load failed will never arrive: it is armed too, so the readiness scan reports the failure
/// exactly as it did when every model was armed at commit, instead of leaving the reference
/// pending for as long as its cell lives.
fn model_can_spawn(
    world_assets: &Assets<WorldAsset>,
    asset_server: &AssetServer,
    handle: &Handle<WorldAsset>,
) -> bool {
    world_assets.contains(handle.id())
        || matches!(
            asset_server
                .get_load_states(handle.id())
                .map(|(load, _, _)| load),
            Some(LoadState::Failed(_))
        )
}

fn mark_world_instance_ready(
    ready: On<WorldInstanceReady>,
    mut pending: Query<&mut PendingAssetProfile>,
) {
    if let Ok(mut pending) = pending.get_mut(ready.entity) {
        pending.scene_spawned = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn track_asset_readiness(
    mut commands: Commands,
    config: Res<EngineConfig>,
    asset_server: Res<AssetServer>,
    pending: PendingAssetQuery,
    unarmed: Query<(), (With<PendingAssetProfile>, With<PendingModel>)>,
    children: Query<&Children>,
    primitives: RenderPrimitiveQuery,
    transforms: Query<(&Transform, &GlobalTransform)>,
    images: Res<Assets<Image>>,
    world_assets: Res<Assets<WorldAsset>>,
    mut fallback_assets: ResMut<DiagnosticFallbackAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    let started = Instant::now();
    // A model still waiting in the arming queue has no scene yet, so the scan below cannot see it;
    // count it here so the readiness gates keep waiting for every queued model.
    metrics.pending_asset_instances = pending.iter().count() + unarmed.iter().count();
    let mut completed_this_scan = 0usize;
    for (entity, root, pending, local, global, world_transform, expected_bounds) in &pending {
        let load_failure =
            asset_server
                .get_load_states(root.0.id())
                .and_then(|(load, _, recursive)| match (load, recursive) {
                    (LoadState::Failed(error), _) => Some(error),
                    (_, RecursiveDependencyLoadState::Failed(error)) => Some(error),
                    _ => None,
                });
        if let Some(error) = load_failure {
            let chain = error_chain(error.as_ref());
            record_asset_failure(&mut metrics, &mut profiler, pending, chain, false);
            hide_partial_scene(&mut commands, entity, &children);
            if config.diagnostic_asset_fallbacks {
                spawn_diagnostic_fallback(
                    &mut commands,
                    entity,
                    &mut fallback_assets,
                    &mut meshes,
                    &mut materials,
                );
                metrics.diagnostic_fallbacks = metrics.diagnostic_fallbacks.saturating_add(1);
            }
            commands.entity(entity).remove::<PendingAssetProfile>();
            completed_this_scan += 1;
        } else if pending.scene_spawned && asset_server.is_loaded_with_dependencies(root.0.id()) {
            let transform_validation = validate_spawned_transforms_and_bounds(
                entity,
                local,
                global,
                world_transform,
                expected_bounds,
                world_assets.get(&root.0),
                &children,
                &transforms,
                &primitives,
                &meshes,
            );
            let transform_summary = match transform_validation {
                Ok(summary) => summary,
                Err(reason) => {
                    metrics.transform_bounds_validation_failures = metrics
                        .transform_bounds_validation_failures
                        .saturating_add(1);
                    profiler.increment("transforms/validation_failures", 1);
                    record_asset_failure(&mut metrics, &mut profiler, pending, vec![reason], false);
                    hide_partial_scene(&mut commands, entity, &children);
                    if config.diagnostic_asset_fallbacks {
                        spawn_diagnostic_fallback(
                            &mut commands,
                            entity,
                            &mut fallback_assets,
                            &mut meshes,
                            &mut materials,
                        );
                        metrics.diagnostic_fallbacks =
                            metrics.diagnostic_fallbacks.saturating_add(1);
                    }
                    commands.entity(entity).remove::<PendingAssetProfile>();
                    completed_this_scan += 1;
                    continue;
                }
            };
            // An empty converted model has nothing to place, draw or validate, so the reference is
            // skipped and counted on its own instead of failing the run's bounds gate. Everything
            // else about the reference stays: it keeps its transform, and its scene is left alone
            // (it is empty; there is nothing in it to hide).
            if transform_summary.empty_model {
                metrics.empty_model_references = metrics.empty_model_references.saturating_add(1);
                profiler.increment("assets/empty_model_references", 1);
                profiler.event(&pending.path, "asset_empty", None);
                commands.entity(entity).remove::<PendingAssetProfile>();
                completed_this_scan += 1;
                continue;
            }
            let validation = validate_spawned_asset(
                entity,
                &children,
                &primitives,
                &meshes,
                &materials,
                &images,
            );
            let summary = match validation {
                Ok(summary) => summary,
                Err(reason) => {
                    record_asset_failure(&mut metrics, &mut profiler, pending, vec![reason], true);
                    hide_partial_scene(&mut commands, entity, &children);
                    if config.diagnostic_asset_fallbacks {
                        spawn_diagnostic_fallback(
                            &mut commands,
                            entity,
                            &mut fallback_assets,
                            &mut meshes,
                            &mut materials,
                        );
                        metrics.diagnostic_fallbacks =
                            metrics.diagnostic_fallbacks.saturating_add(1);
                    }
                    commands.entity(entity).remove::<PendingAssetProfile>();
                    completed_this_scan += 1;
                    continue;
                }
            };
            let micros = pending
                .started
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;
            metrics.assets_ready = metrics.assets_ready.saturating_add(1);
            metrics.meshes_validated = metrics
                .meshes_validated
                .saturating_add(summary.meshes as u64);
            metrics.materials_validated = metrics
                .materials_validated
                .saturating_add(summary.materials as u64);
            metrics.images_validated = metrics
                .images_validated
                .saturating_add(summary.images as u64);
            metrics.transform_instances_validated =
                metrics.transform_instances_validated.saturating_add(1);
            metrics.transform_nodes_validated = metrics
                .transform_nodes_validated
                .saturating_add(transform_summary.nodes as u64);
            metrics.bounds_validated = metrics.bounds_validated.saturating_add(1);
            metrics.max_asset_ready_micros = metrics.max_asset_ready_micros.max(micros);
            profiler.record_micros("assets/model_ready", micros);
            profiler.event(&pending.path, "asset_ready", Some(micros as f64 / 1000.0));
            commands.entity(entity).remove::<PendingAssetProfile>();
            completed_this_scan += 1;
        }
    }
    metrics.pending_asset_instances = metrics
        .pending_asset_instances
        .saturating_sub(completed_this_scan);
    metrics.instances_completed_this_scan = completed_this_scan as u64;
    metrics.max_instances_completed_per_scan = metrics
        .max_instances_completed_per_scan
        .max(metrics.instances_completed_this_scan);
    profiler.set_gauge(
        "assets/pending_instances",
        metrics.pending_asset_instances as f64,
    );
    profiler.set_gauge(
        "assets/instances_completed_this_scan",
        metrics.instances_completed_this_scan as f64,
    );
    profiler.record_elapsed("assets/readiness_scan", started);
}

fn track_surface_readiness(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    images: Res<Assets<Image>>,
    terrain: Query<(Entity, &PendingTerrainProfile)>,
    water: Query<(Entity, &PendingWaterProfile)>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    metrics.pending_surface_instances = terrain.iter().count() + water.iter().count();
    let mut completed = 0usize;
    for (entity, pending) in &terrain {
        match validate_surface_dependencies(&asset_server, &images, &pending.images, true) {
            SurfaceDependencyState::Pending => {}
            SurfaceDependencyState::Ready => {
                metrics.terrain_patches_validated =
                    metrics.terrain_patches_validated.saturating_add(1);
                metrics.materials_validated = metrics.materials_validated.saturating_add(1);
                metrics.images_validated = metrics
                    .images_validated
                    .saturating_add(pending.images.len() as u64);
                profiler.increment("terrain/patches_validated", 1);
                commands.entity(entity).insert(Visibility::Inherited);
                commands.entity(entity).remove::<PendingTerrainProfile>();
                completed += 1;
            }
            SurfaceDependencyState::Failed(reason) => {
                metrics.asset_load_failures = metrics.asset_load_failures.saturating_add(1);
                metrics.terrain_validation_failures =
                    metrics.terrain_validation_failures.saturating_add(1);
                metrics.asset_failures.push(AssetFailure {
                    model_path: format!(
                        "terrain/{:08X}/quadrant-{}",
                        pending.cell_id, pending.quadrant
                    ),
                    reference_form_id: 0,
                    base_form_id: 0,
                    cell_id: pending.cell_id,
                    dependency_chain: vec![reason],
                });
                profiler.increment("terrain/validation_failures", 1);
                commands.entity(entity).insert(Visibility::Hidden);
                commands.entity(entity).remove::<PendingTerrainProfile>();
                completed += 1;
            }
        }
    }
    for (entity, pending) in &water {
        let handles: Vec<_> = pending.flow_normal.iter().cloned().collect();
        match validate_surface_dependencies(&asset_server, &images, &handles, false) {
            SurfaceDependencyState::Pending => {}
            SurfaceDependencyState::Ready => {
                metrics.water_surfaces_validated =
                    metrics.water_surfaces_validated.saturating_add(1);
                metrics.materials_validated = metrics.materials_validated.saturating_add(1);
                metrics.images_validated = metrics
                    .images_validated
                    .saturating_add(handles.len() as u64);
                profiler.increment("water/surfaces_validated", 1);
                commands.entity(entity).insert(Visibility::Inherited);
                commands.entity(entity).remove::<PendingWaterProfile>();
                completed += 1;
            }
            SurfaceDependencyState::Failed(reason) => {
                metrics.asset_load_failures = metrics.asset_load_failures.saturating_add(1);
                metrics.water_validation_failures =
                    metrics.water_validation_failures.saturating_add(1);
                metrics.asset_failures.push(AssetFailure {
                    model_path: format!("water/{:08X}", pending.cell_id),
                    reference_form_id: 0,
                    base_form_id: 0,
                    cell_id: pending.cell_id,
                    dependency_chain: vec![reason],
                });
                profiler.increment("water/validation_failures", 1);
                commands.entity(entity).insert(Visibility::Hidden);
                commands.entity(entity).remove::<PendingWaterProfile>();
                completed += 1;
            }
        }
    }
    metrics.pending_surface_instances = metrics.pending_surface_instances.saturating_sub(completed);
    profiler.set_gauge(
        "assets/pending_surface_instances",
        metrics.pending_surface_instances as f64,
    );
}

enum SurfaceDependencyState {
    Pending,
    Ready,
    Failed(String),
}

fn validate_surface_dependencies(
    asset_server: &AssetServer,
    images: &Assets<Image>,
    handles: &[Handle<Image>],
    expects_srgb: bool,
) -> SurfaceDependencyState {
    for handle in handles {
        if let Some((load, _, recursive)) = asset_server.get_load_states(handle.id()) {
            let failed = match (load, recursive) {
                (LoadState::Failed(error), _) => Some(error),
                (_, RecursiveDependencyLoadState::Failed(error)) => Some(error),
                _ => None,
            };
            if let Some(error) = failed {
                return SurfaceDependencyState::Failed(error_chain(error.as_ref()).join(" -> "));
            }
        }
        if !asset_server.is_loaded_with_dependencies(handle.id()) {
            return SurfaceDependencyState::Pending;
        }
        let Some(image) = images.get(handle) else {
            return SurfaceDependencyState::Pending;
        };
        if image.texture_descriptor.format.is_srgb() != expects_srgb {
            return SurfaceDependencyState::Failed(format!(
                "image {:?} has wrong color space {:?}",
                handle.id(),
                image.texture_descriptor.format
            ));
        }
        if let Err(reason) = validate_image_sampler("surface", &image.sampler) {
            return SurfaceDependencyState::Failed(reason);
        }
    }
    SurfaceDependencyState::Ready
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AssetValidationSummary {
    meshes: usize,
    materials: usize,
    images: usize,
    excluded_materials: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct TransformValidationSummary {
    nodes: usize,
    /// The reference's converted model is an empty scene, so it was counted in
    /// [`StreamingMetrics::empty_model_references`] rather than as a validated instance: there
    /// were no converted bounds to compare against and nothing to draw.
    empty_model: bool,
}

#[allow(clippy::too_many_arguments)]
fn validate_spawned_transforms_and_bounds(
    root: Entity,
    root_local: &Transform,
    root_global: &GlobalTransform,
    world_transform: &WorldTransform,
    expected: Option<&ExpectedModelBounds>,
    converted_model: Option<&WorldAsset>,
    children: &Query<&Children>,
    transforms: &Query<(&Transform, &GlobalTransform)>,
    primitives: &RenderPrimitiveQuery,
    meshes: &Assets<Mesh>,
) -> Result<TransformValidationSummary, String> {
    validate_transform("reference", root_local, root_global)?;
    let local_matrix = root_local.to_matrix();
    if matrix_max_difference(local_matrix, world_transform.0) > 1.0e-4 {
        return Err("WorldTransform differs from the spawned reference Transform".to_owned());
    }
    let Some(expected) = expected else {
        // A model the converter wrote no aggregate bounds for is an empty scene: a model with no
        // renderable geometry, so there is nothing to place, draw or bound. What makes it empty is
        // read from the converted model itself - the scene the converter wrote, which declares no
        // node and no mesh - rather than from the spawned instance, so a hierarchy that has not
        // spawned yet cannot pass as an empty model. A scene that does declare a node or a mesh
        // and still arrived without aggregate bounds is a conversion defect, and stays as fatal as
        // any other.
        let scene = converted_model.ok_or_else(|| {
            "the converted model is not loaded while validating its bounds; reconvert the asset"
                .to_owned()
        })?;
        let contents = converted_scene_contents(scene);
        return if contents.is_empty() {
            Ok(TransformValidationSummary {
                nodes: 0,
                empty_model: true,
            })
        } else {
            Err(format!(
                "converted model has no validated aggregate bounds; reconvert the asset (its converted scene is not empty: {} mesh primitives, {} nodes)",
                contents.meshes, contents.nodes
            ))
        };
    };
    ExpectedModelBounds::new(expected.min, expected.max)
        .ok_or_else(|| "converted model bounds are non-finite, empty, or inverted".to_owned())?;

    let mut actual_min = Vec3::splat(f32::INFINITY);
    let mut actual_max = Vec3::splat(f32::NEG_INFINITY);
    let mut nodes = 0usize;
    let mut bounded_meshes = 0usize;
    if let Ok(direct_children) = children.get(root) {
        for child in direct_children.iter() {
            accumulate_relative_bounds(
                child,
                Affine3A::IDENTITY,
                children,
                transforms,
                primitives,
                meshes,
                &mut nodes,
                &mut bounded_meshes,
                &mut actual_min,
                &mut actual_max,
            )?;
        }
    }
    if bounded_meshes == 0 {
        return Err("spawned hierarchy contains no bounded mesh".to_owned());
    }
    let extent = (expected.max - expected.min).abs().max_element().max(1.0);
    let tolerance = (extent * 1.0e-4).max(1.0e-3);
    let error = (actual_min - expected.min)
        .abs()
        .max((actual_max - expected.max).abs())
        .max_element();
    if !error.is_finite() || error > tolerance {
        return Err(format!(
            "spawned hierarchy bounds diverge from conversion: expected {:?}..{:?}, actual {:?}..{:?}, tolerance {tolerance}",
            expected.min, expected.max, actual_min, actual_max
        ));
    }
    Ok(TransformValidationSummary {
        nodes,
        empty_model: false,
    })
}

/// What a converted model holds: the scene the converter wrote for it, as the asset loader built
/// it, with the model's own node and mesh count. A model with no renderable geometry converts to
/// an empty scene, and the loader still gives that scene its own root entity, so emptiness is "no
/// mesh and no node", not "no entity".
#[derive(Debug, Default, PartialEq, Eq)]
struct ConvertedSceneContents {
    /// One per glTF primitive the converter exported: an entity carrying a [`Mesh3d`].
    meshes: usize,
    /// Every entity the loader attached below another one, which in a glTF scene is every node;
    /// the scene's own root is the only entity without a parent.
    nodes: usize,
}

impl ConvertedSceneContents {
    /// The converter's empty scene, which is what a model with no renderable geometry converts
    /// to: no node and no mesh to place, draw or bound.
    fn is_empty(&self) -> bool {
        self.meshes == 0 && self.nodes == 0
    }
}

/// Counts what a converted model holds. Taken from the loaded asset rather than from its spawned
/// instance, so what is read is the whole converted file: a scene whose entities have not spawned
/// yet, or one whose geometry an exporter dropped, cannot pass as an empty model.
fn converted_scene_contents(scene: &WorldAsset) -> ConvertedSceneContents {
    let mut contents = ConvertedSceneContents::default();
    for entity in scene.world.iter_entities() {
        contents.meshes += usize::from(entity.contains::<Mesh3d>());
        contents.nodes += usize::from(entity.contains::<ChildOf>());
    }
    contents
}

/// Walks the spawned hierarchy under a root, accumulating each descendant's transform
/// relative to the root by composing local `Transform`s along the path from the root.
///
/// This deliberately never forms the root's or a descendant's absolute `GlobalTransform`
/// matrix: at real-world placements (tens of thousands of units from the origin) building
/// that large-magnitude matrix and then multiplying by its inverse cancels lossily in f32,
/// losing more precision than the bounds-check tolerance allows for small models. Composing
/// only the local, mesh-scale transforms keeps every intermediate value small and exact
/// enough for the tolerance.
#[allow(clippy::too_many_arguments)]
fn accumulate_relative_bounds(
    entity: Entity,
    relative_to_root: Affine3A,
    children: &Query<&Children>,
    transforms: &Query<(&Transform, &GlobalTransform)>,
    primitives: &RenderPrimitiveQuery,
    meshes: &Assets<Mesh>,
    nodes: &mut usize,
    bounded_meshes: &mut usize,
    actual_min: &mut Vec3,
    actual_max: &mut Vec3,
) -> Result<(), String> {
    // An explicit stack, not recursion: a deeply nested model must not overflow the thread's
    // stack. Children are pushed in reverse so they are visited in order, as before.
    let mut stack = vec![(entity, relative_to_root)];
    while let Some((entity, parent_to_root)) = stack.pop() {
        let (local, global) = transforms
            .get(entity)
            .map_err(|_| format!("hierarchy node {entity:?} has no local/global transform"))?;
        validate_transform(&format!("hierarchy node {entity:?}"), local, global)?;
        *nodes += 1;
        let relative_to_root = parent_to_root * local.compute_affine();
        if let Ok((mesh_handle, _, _)) = primitives.get(entity) {
            let mesh = meshes.get(mesh_handle).ok_or_else(|| {
                format!(
                    "mesh {:?} is absent while validating bounds",
                    mesh_handle.id()
                )
            })?;
            let aabb = mesh.compute_aabb().ok_or_else(|| {
                format!("mesh {:?} has no finite POSITION bounds", mesh_handle.id())
            })?;
            let center = Vec3::from(aabb.center);
            let half_extents = Vec3::from(aabb.half_extents);
            let transformed = InstanceBounds::transformed(
                center - half_extents,
                center + half_extents,
                Mat4::from(relative_to_root),
            );
            *actual_min = actual_min.min(transformed.min);
            *actual_max = actual_max.max(transformed.max);
            *bounded_meshes += 1;
        }
        if let Ok(kids) = children.get(entity) {
            stack.extend(kids.iter().rev().map(|child| (child, relative_to_root)));
        }
    }
    Ok(())
}

fn validate_transform(
    label: &str,
    local: &Transform,
    global: &GlobalTransform,
) -> Result<(), String> {
    let local_matrix = local.to_matrix();
    let global_matrix = global.to_matrix();
    if !local_matrix.is_finite() || !global_matrix.is_finite() {
        return Err(format!("{label} contains a non-finite transform"));
    }
    if local.scale.abs().min_element() <= 1.0e-6
        || local_matrix.determinant().abs() <= 1.0e-8
        || global_matrix.determinant().abs() <= 1.0e-8
    {
        return Err(format!(
            "{label} contains a singular scale or hierarchy transform"
        ));
    }
    let rotation_length = local.rotation.length();
    if !rotation_length.is_finite() || (rotation_length - 1.0).abs() > 1.0e-3 {
        return Err(format!("{label} contains a non-normalized rotation"));
    }
    Ok(())
}

fn matrix_max_difference(left: Mat4, right: Mat4) -> f32 {
    left.to_cols_array()
        .into_iter()
        .zip(right.to_cols_array())
        .map(|(left, right)| (left - right).abs())
        .fold(0.0, f32::max)
}

fn validate_spawned_asset(
    root: Entity,
    children: &Query<&Children>,
    primitives: &RenderPrimitiveQuery,
    meshes: &Assets<Mesh>,
    materials: &Assets<StandardMaterial>,
    images: &Assets<Image>,
) -> Result<AssetValidationSummary, String> {
    let mut summary = AssetValidationSummary::default();
    for descendant in children.iter_descendants(root) {
        let Ok((mesh, material_handle, extras)) = primitives.get(descendant) else {
            continue;
        };
        if meshes.get(mesh).is_none() {
            return Err(format!(
                "mesh {:?} is absent after scene readiness",
                mesh.id()
            ));
        }
        summary.meshes += 1;
        let Some(material_handle) = material_handle else {
            if extras.is_some_and(has_explicit_material_exclusion) {
                summary.excluded_materials += 1;
                continue;
            }
            return Err(format!(
                "mesh entity {descendant:?} has no loaded material or explicit exclusion"
            ));
        };
        let material = materials.get(material_handle).ok_or_else(|| {
            format!(
                "material {:?} is absent after scene readiness",
                material_handle.id()
            )
        })?;
        summary.images += validate_standard_material(material, images)?;
        summary.materials += 1;
    }
    Ok(summary)
}

fn has_explicit_material_exclusion(extras: &GltfExtras) -> bool {
    serde_json::from_str::<serde_json::Value>(&extras.value)
        .ok()
        .and_then(|value| {
            value
                .pointer("/openSkyrim/materialExclusion")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some()
}

pub(crate) fn validate_standard_material(
    material: &StandardMaterial,
    images: &Assets<Image>,
) -> Result<usize, String> {
    match material.alpha_mode {
        AlphaMode::Opaque | AlphaMode::Blend => {}
        AlphaMode::Mask(cutoff) if cutoff.is_finite() && (0.0..=1.0).contains(&cutoff) => {}
        AlphaMode::Mask(cutoff) => return Err(format!("invalid alpha cutoff {cutoff}")),
        mode => return Err(format!("unsupported Skyrim material alpha mode {mode:?}")),
    }
    if material.double_sided != material.cull_mode.is_none() {
        return Err(format!(
            "inconsistent culling: double_sided={} cull_mode={:?}",
            material.double_sided, material.cull_mode
        ));
    }
    let emissive = material.emissive;
    if ![emissive.red, emissive.green, emissive.blue, emissive.alpha]
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0)
    {
        return Err("emissive contains a non-finite or negative channel".to_owned());
    }

    let slots = [
        ("base_color", material.base_color_texture.as_ref(), true),
        ("emissive", material.emissive_texture.as_ref(), true),
        (
            "metallic_roughness",
            material.metallic_roughness_texture.as_ref(),
            false,
        ),
        ("normal", material.normal_map_texture.as_ref(), false),
        ("occlusion", material.occlusion_texture.as_ref(), false),
        ("specular", material.specular_texture.as_ref(), false),
        (
            "specular_tint",
            material.specular_tint_texture.as_ref(),
            true,
        ),
    ];
    let mut validated = 0usize;
    for (slot, handle, expects_srgb) in slots {
        let Some(handle) = handle else {
            continue;
        };
        let image = images
            .get(handle)
            .ok_or_else(|| format!("{slot} image {:?} is not loaded", handle.id()))?;
        let descriptor = &image.texture_descriptor;
        if descriptor.size.width == 0
            || descriptor.size.height == 0
            || descriptor.mip_level_count == 0
        {
            return Err(format!("{slot} image has invalid dimensions or mip levels"));
        }
        if descriptor.format.is_srgb() != expects_srgb {
            return Err(format!(
                "{slot} image color space mismatch: {:?}",
                descriptor.format
            ));
        }
        validate_image_sampler(slot, &image.sampler)?;
        validated += 1;
    }
    Ok(validated)
}

fn validate_image_sampler(slot: &str, sampler: &ImageSampler) -> Result<(), String> {
    let ImageSampler::Descriptor(descriptor) = sampler else {
        return Ok(());
    };
    if descriptor.anisotropy_clamp == 0
        || !descriptor.lod_min_clamp.is_finite()
        || !descriptor.lod_max_clamp.is_finite()
        || descriptor.lod_min_clamp > descriptor.lod_max_clamp
    {
        return Err(format!("{slot} image has an invalid sampler descriptor"));
    }
    if descriptor.anisotropy_clamp > 1
        && (descriptor.mag_filter != ImageFilterMode::Linear
            || descriptor.min_filter != ImageFilterMode::Linear
            || descriptor.mipmap_filter != ImageFilterMode::Linear)
    {
        return Err(format!(
            "{slot} image requests anisotropy without linear filtering"
        ));
    }
    Ok(())
}

fn error_chain(error: &(dyn StdError + 'static)) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = Some(error);
    while let Some(error) = current {
        chain.push(error.to_string());
        current = error.source();
    }
    chain
}

fn record_asset_failure(
    metrics: &mut StreamingMetrics,
    profiler: &mut ProfilingState,
    pending: &PendingAssetProfile,
    dependency_chain: Vec<String>,
    material_validation: bool,
) {
    metrics.asset_load_failures = metrics.asset_load_failures.saturating_add(1);
    if material_validation {
        metrics.material_validation_failures =
            metrics.material_validation_failures.saturating_add(1);
    }
    profiler.increment("assets/load_failures", 1);
    profiler.event(&pending.path, "asset_failed", None);
    let mut full_chain = vec![
        format!("REFR {:08X}", pending.form_id),
        format!("base record {:08X}", pending.base_form_id),
        pending.path.clone(),
    ];
    full_chain.extend(dependency_chain);
    error!(
        reference = format_args!("{:08X}", pending.form_id),
        base = format_args!("{:08X}", pending.base_form_id),
        cell = format_args!("{:08X}", pending.cell_id),
        path = %pending.path,
        chain = ?full_chain,
        "model, material, or image dependency failed strict validation"
    );
    metrics.asset_failures.push(AssetFailure {
        model_path: pending.path.clone(),
        reference_form_id: pending.form_id,
        base_form_id: pending.base_form_id,
        cell_id: pending.cell_id,
        dependency_chain: full_chain,
    });
}

fn hide_partial_scene(commands: &mut Commands, root: Entity, children: &Query<&Children>) {
    for descendant in children.iter_descendants(root) {
        commands.entity(descendant).insert(Visibility::Hidden);
    }
}

fn spawn_diagnostic_fallback(
    commands: &mut Commands,
    root: Entity,
    fallback: &mut DiagnosticFallbackAssets,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let mesh = fallback
        .mesh
        .get_or_insert_with(|| meshes.add(Cuboid::new(96.0, 96.0, 96.0)))
        .clone();
    let material = fallback
        .material
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::srgb(1.0, 0.0, 0.8),
                emissive: LinearRgba::new(8.0, 0.0, 5.0, 1.0),
                unlit: true,
                ..default()
            })
        })
        .clone();
    commands.entity(root).with_child((
        Name::new("DIAGNOSTIC ASSET FAILURE"),
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
    ));
}

fn cell_translation(key: CellKey, origin: IVec2) -> Vec3 {
    match key {
        CellKey::Exterior { grid_x, grid_y, .. } => Vec3::new(
            (grid_x - origin.x) as f32 * CELL_SIZE,
            0.0,
            -(grid_y - origin.y) as f32 * CELL_SIZE,
        ),
        CellKey::Interior(_) => Vec3::ZERO,
    }
}

fn creation_to_bevy(position: Vec3) -> Vec3 {
    Vec3::from_array(shared::coordinates::creation_to_runtime_vector(
        position.to_array(),
    ))
}

fn creation_rotation_to_bevy(rotation: [f32; 3]) -> Quat {
    Quat::from_array(shared::coordinates::creation_euler_to_runtime_quaternion(
        rotation,
    ))
}

fn converted_model_path(path: String) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let lowercase = normalized.to_ascii_lowercase();
    let filename = lowercase.rsplit('/').next().unwrap_or_default();
    if lowercase.starts_with("meshes/sky/")
        || lowercase.starts_with("sky/")
        || lowercase.starts_with("meshes/markers/")
        || lowercase.starts_with("markers/")
        || lowercase.starts_with("meshes/effects/")
        || lowercase.starts_with("effects/")
        || filename.contains("marker")
    {
        return None;
    }
    let without_prefix = normalized
        .strip_prefix("meshes/")
        .or_else(|| normalized.strip_prefix("Meshes/"))
        .unwrap_or(&normalized);
    if without_prefix.is_empty() {
        return None;
    }
    let mut converted = std::path::PathBuf::from("meshes").join(without_prefix);
    converted.set_extension("glb");
    Some(converted.to_string_lossy().replace('\\', "/"))
}

pub(crate) fn quadrant_layers(
    terrain: &TerrainSnapshot,
    quadrant: u8,
) -> Result<Vec<&TerrainLayerSnapshot>, String> {
    let mut layers: Vec<_> = terrain
        .layers
        .iter()
        .filter(|layer| layer.quadrant == quadrant)
        .collect();
    layers.sort_by_key(|layer| (!layer.is_base, layer.layer, layer.texture_form_id));
    if layers.is_empty() {
        return Ok(layers);
    }
    let base_count = layers.iter().filter(|layer| layer.is_base).count();
    if base_count != 1 {
        return Err(format!(
            "LAND {:08X} quadrant {quadrant} has {base_count} base layers; expected one",
            terrain.cell_id
        ));
    }
    if layers.len() > 6 {
        return Err(format!(
            "LAND {:08X} quadrant {quadrant} has {} layers; runtime supports six",
            terrain.cell_id,
            layers.len()
        ));
    }
    let mut layer_ids = HashSet::new();
    for layer in layers.iter().filter(|layer| !layer.is_base) {
        if !layer_ids.insert(layer.layer) {
            return Err(format!(
                "LAND {:08X} quadrant {quadrant} repeats ATXT layer {}",
                terrain.cell_id, layer.layer
            ));
        }
        let mut vertices = HashSet::new();
        for &(vertex, opacity) in &layer.weights {
            if usize::from(vertex) >= 17 * 17
                || !opacity.is_finite()
                || !(0.0..=1.0).contains(&opacity)
                || !vertices.insert(vertex)
            {
                return Err(format!(
                    "LAND {:08X} quadrant {quadrant} has invalid or duplicate VTXT data",
                    terrain.cell_id
                ));
            }
        }
    }
    Ok(layers)
}

fn validate_terrain_snapshot(
    terrain: &TerrainSnapshot,
    catalog: &AssetCatalog,
) -> Result<(), String> {
    let width = usize::from(terrain.width);
    let height = usize::from(terrain.height);
    if width != 33 || height != 33 || terrain.heights.len() != width * height {
        return Err(format!(
            "terrain dimensions/data mismatch: {width}x{height} with {} heights",
            terrain.heights.len()
        ));
    }
    if terrain.heights.iter().any(|height| !height.is_finite()) {
        return Err("terrain contains a non-finite height".to_owned());
    }
    if terrain.normals.len() != width * height * 3 {
        return Err(format!(
            "terrain has {} packed normal bytes",
            terrain.normals.len()
        ));
    }
    if terrain
        .normals
        .as_chunks::<3>()
        .0
        .iter()
        .any(|normal| normal == &[0, 0, 0])
    {
        return Err("terrain contains a zero-length packed normal".to_owned());
    }
    if !terrain.vertex_colors.is_empty() && terrain.vertex_colors.len() != width * height * 3 {
        return Err(format!(
            "terrain has {} packed vertex-color bytes",
            terrain.vertex_colors.len()
        ));
    }
    for quadrant in 0..4 {
        for layer in quadrant_layers(terrain, quadrant)? {
            if layer.is_base && layer.texture_form_id == 0 {
                continue;
            }
            if catalog.landscape_diffuse(layer.texture_form_id).is_none() {
                return Err(format!(
                    "quadrant {quadrant} texture {:08X} has no converted diffuse image",
                    layer.texture_form_id
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn build_terrain_quadrant_mesh(
    terrain: &TerrainSnapshot,
    quadrant: u8,
) -> Result<Mesh, String> {
    let layers = quadrant_layers(terrain, quadrant)?;
    let width = usize::from(terrain.width);
    let height = usize::from(terrain.height);
    if width != 33 || height != 33 || terrain.heights.len() != width * height {
        return Err("terrain must contain a complete 33x33 height field".to_owned());
    }
    let step_x = CELL_SIZE / (width - 1) as f32;
    let step_z = CELL_SIZE / (height - 1) as f32;
    let origin_x = usize::from(quadrant % 2) * 16;
    let origin_y = usize::from(quadrant / 2) * 16;
    let mut overlay_weights = vec![vec![0.0f32; 17 * 17]; layers.len().saturating_sub(1)];
    for (slot, layer) in layers.iter().skip(1).enumerate() {
        for &(vertex, opacity) in &layer.weights {
            overlay_weights[slot][usize::from(vertex)] = opacity;
        }
    }
    let mut positions = Vec::with_capacity(17 * 17);
    let mut normals = Vec::with_capacity(17 * 17);
    let mut uvs = Vec::with_capacity(17 * 17);
    let mut extra_weights = Vec::with_capacity(17 * 17);
    let mut packed_weights = Vec::with_capacity(17 * 17);
    let mut colors = Vec::with_capacity(17 * 17);
    for local_y in 0..17 {
        for local_x in 0..17 {
            let x = origin_x + local_x;
            let y = origin_y + local_y;
            let index = y * width + x;
            let local = local_y * 17 + local_x;
            positions.push([
                x as f32 * step_x,
                terrain.heights[index],
                -(y as f32 * step_z),
            ]);
            normals.push(
                Vec3::new(
                    terrain.normals[index * 3] as f32,
                    terrain.normals[index * 3 + 2] as f32,
                    -(terrain.normals[index * 3 + 1] as f32),
                )
                .normalize_or(Vec3::Y)
                .to_array(),
            );
            uvs.push([
                x as f32 / (width - 1) as f32,
                y as f32 / (height - 1) as f32,
            ]);
            let weight = |slot: usize| {
                overlay_weights
                    .get(slot)
                    .map_or(0.0, |values| values[local])
            };
            let first = Vec3::new(weight(0), weight(1), weight(2));
            let length = first.length();
            packed_weights.push(if length > 0.0 {
                let normalized = first / length;
                [normalized.x, normalized.y, normalized.z, length]
            } else {
                [0.0; 4]
            });
            extra_weights.push([weight(3), weight(4)]);
            colors.push(if terrain.vertex_colors.is_empty() {
                [1.0; 4]
            } else {
                [
                    terrain.vertex_colors[index * 3] as f32 / 255.0,
                    terrain.vertex_colors[index * 3 + 1] as f32 / 255.0,
                    terrain.vertex_colors[index * 3 + 2] as f32 / 255.0,
                    1.0,
                ]
            });
        }
    }
    let mut indices = Vec::with_capacity(16 * 16 * 6);
    for y in 0..16 {
        for x in 0..16 {
            let a = (y * 17 + x) as u32;
            let b = a + 1;
            let c = a + 17;
            let d = c + 1;
            indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, extra_weights);
    mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, packed_weights);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    Ok(mesh)
}

fn validate_and_register_terrain_edges(
    key: CellKey,
    terrain: &TerrainSnapshot,
    continuity: &mut TerrainContinuity,
    metrics: &mut StreamingMetrics,
) -> Result<(), String> {
    let CellKey::Exterior {
        worldspace_id,
        grid_x,
        grid_y,
    } = key
    else {
        return Ok(());
    };
    let width = usize::from(terrain.width);
    let height = usize::from(terrain.height);
    let edges = TerrainEdges {
        west: (0..height)
            .map(|row| terrain.heights[row * width])
            .collect(),
        east: (0..height)
            .map(|row| terrain.heights[row * width + width - 1])
            .collect(),
        south: terrain.heights[..width].to_vec(),
        north: terrain.heights[(height - 1) * width..].to_vec(),
    };
    let neighbors = [
        (
            CellKey::Exterior {
                worldspace_id,
                grid_x: grid_x - 1,
                grid_y,
            },
            &edges.west,
            true,
        ),
        (
            CellKey::Exterior {
                worldspace_id,
                grid_x: grid_x + 1,
                grid_y,
            },
            &edges.east,
            true,
        ),
        (
            CellKey::Exterior {
                worldspace_id,
                grid_x,
                grid_y: grid_y - 1,
            },
            &edges.south,
            false,
        ),
        (
            CellKey::Exterior {
                worldspace_id,
                grid_x,
                grid_y: grid_y + 1,
            },
            &edges.north,
            false,
        ),
    ];
    for (neighbor_key, edge, horizontal) in neighbors {
        let Some(neighbor) = continuity.edges.get(&neighbor_key) else {
            continue;
        };
        let other = if horizontal {
            if matches!(neighbor_key, CellKey::Exterior { grid_x: neighbor_x, .. } if neighbor_x < grid_x)
            {
                &neighbor.east
            } else {
                &neighbor.west
            }
        } else if matches!(neighbor_key, CellKey::Exterior { grid_y: neighbor_y, .. } if neighbor_y < grid_y)
        {
            &neighbor.north
        } else {
            &neighbor.south
        };
        if edge.len() != other.len()
            || edge
                .iter()
                .zip(other)
                .any(|(left, right)| (left - right).abs() > 0.01)
        {
            return Err(format!(
                "terrain edge does not match neighbor {neighbor_key:?}"
            ));
        }
        metrics.terrain_seams_validated = metrics.terrain_seams_validated.saturating_add(1);
    }
    continuity.edges.insert(key, edges);
    Ok(())
}

fn update_render_origin(
    mut origin: ResMut<RenderOrigin>,
    mut camera: Query<&mut Transform, With<StreamingCamera>>,
    mut roots: Query<(&ExteriorCellGrid, &mut Transform), Without<StreamingCamera>>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    let started = Instant::now();
    let Ok(mut camera) = camera.single_mut() else {
        return;
    };
    let shift = IVec2::new(
        (camera.translation.x / CELL_SIZE).trunc() as i32,
        (-camera.translation.z / CELL_SIZE).trunc() as i32,
    );
    if shift == IVec2::ZERO {
        return;
    }
    origin.0 += shift;
    camera.translation.x -= shift.x as f32 * CELL_SIZE;
    camera.translation.z += shift.y as f32 * CELL_SIZE;
    for (grid, mut transform) in &mut roots {
        transform.translation = Vec3::new(
            (grid.0.x - origin.0.x) as f32 * CELL_SIZE,
            0.0,
            -(grid.0.y - origin.0.y) as f32 * CELL_SIZE,
        );
    }
    profiler.increment("streaming/origin_rebases", 1);
    metrics.origin_rebases = metrics.origin_rebases.saturating_add(1);
    profiler.event(
        format!("{},{}", origin.0.x, origin.0.y),
        "origin_rebased",
        None,
    );
    profiler.record_elapsed("streaming/render_origin_rebase", started);
}

fn validate_streaming_lifecycle(
    config: Res<EngineConfig>,
    origin: Res<RenderOrigin>,
    streaming: Res<StreamingWorld>,
    camera: Query<&Transform, With<StreamingCamera>>,
    roots: Query<(Entity, &CellRef, Option<&ExteriorCellGrid>), With<StreamedCellRoot>>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    let active_requests = streaming
        .cells
        .values()
        .filter(|status| matches!(status, CellStatus::Loading { .. }))
        .count();
    // A retiring cell still owns its root until the unload budget reaches it, so its root is
    // accounted for here rather than reading as orphaned.
    let resident_entities: HashSet<_> = streaming
        .cells
        .values()
        .filter_map(|status| match status {
            CellStatus::Resident { root } | CellStatus::Retiring { root } => Some(*root),
            _ => None,
        })
        .collect();
    let retiring_entities: HashSet<_> = streaming
        .cells
        .values()
        .filter_map(|status| match status {
            CellStatus::Retiring { root } => Some(*root),
            _ => None,
        })
        .collect();
    let root_entries: Vec<_> = roots.iter().collect();
    let root_entities: HashSet<_> = root_entries.iter().map(|(entity, _, _)| *entity).collect();
    let mut roots_by_cell = HashMap::<u32, usize>::new();
    for (_, cell, _) in &root_entries {
        *roots_by_cell.entry(cell.0).or_default() += 1;
    }
    let duplicate_roots = roots_by_cell.values().filter(|count| **count > 1).count() as u64;
    let orphaned_roots = root_entities.difference(&resident_entities).count() as u64;
    let missing_roots = resident_entities.difference(&root_entities).count() as u64;
    let out_of_range_roots = camera.single().map_or(0, |camera| {
        let center = streaming_center(camera.translation, origin.0);
        root_entries
            .iter()
            // A retiring root is outside the radius by construction: it is waiting for its turn in
            // the unload budget, which [`despawn_cells`] bounds, so its lag is not a violation.
            .filter(|(entity, _, _)| !retiring_entities.contains(entity))
            .filter_map(|(_, _, grid)| *grid)
            .filter(|grid| {
                (grid.0.x - center.x).abs() > config.unload_radius
                    || (grid.0.y - center.y).abs() > config.unload_radius
            })
            .count() as u64
    });
    let violations = duplicate_roots + orphaned_roots + missing_roots + out_of_range_roots;

    metrics.active_requests = active_requests;
    metrics.peak_active_requests = metrics.peak_active_requests.max(active_requests);
    metrics.resident_roots = root_entries.len();
    metrics.duplicate_cell_roots = metrics.duplicate_cell_roots.max(duplicate_roots);
    metrics.orphaned_cell_roots = metrics.orphaned_cell_roots.max(orphaned_roots);
    metrics.missing_cell_roots = metrics.missing_cell_roots.max(missing_roots);
    metrics.out_of_range_cell_roots = metrics.out_of_range_cell_roots.max(out_of_range_roots);
    if violations > metrics.streaming_invariant_failures {
        error!(
            duplicate_roots,
            orphaned_roots,
            missing_roots,
            out_of_range_roots,
            "streaming lifecycle invariant failed"
        );
        profiler.event("streaming", "invariant_failed", None);
    }
    metrics.streaming_invariant_failures = metrics.streaming_invariant_failures.max(violations);
    profiler.set_gauge("streaming/active_requests", active_requests as f64);
    profiler.set_gauge("streaming/resident_roots", root_entries.len() as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_budget_ignores_only_the_documented_scheduler_tolerance() {
        assert!(!commit_budget_exceeded(16_670, 16_670));
        assert!(!commit_budget_exceeded(17_670, 16_670));
        assert!(commit_budget_exceeded(17_671, 16_670));
    }

    fn terrain_fixture(cell_id: u32, height: f32) -> TerrainSnapshot {
        TerrainSnapshot {
            cell_id,
            width: 33,
            height: 33,
            heights: vec![height; 33 * 33],
            normals: (0..33 * 33).flat_map(|_| [0, 0, 127]).collect(),
            vertex_colors: vec![255; 33 * 33 * 3],
            layers: (0..4)
                .map(|quadrant| TerrainLayerSnapshot {
                    texture_form_id: u32::from(quadrant) + 1,
                    quadrant,
                    layer: 0,
                    is_base: true,
                    weights: Vec::new(),
                })
                .collect(),
            water_height: None,
            water_type_form_id: None,
        }
    }

    #[test]
    fn maps_nif_paths_to_converted_glb_paths() {
        assert_eq!(
            converted_model_path("meshes\\architecture\\wall.nif".into()).as_deref(),
            Some("meshes/architecture/wall.glb")
        );
        assert_eq!(
            converted_model_path("meshes/Sky/CloudShape01.nif".into()),
            None
        );
        assert_eq!(converted_model_path("meshes/Marker_Map.nif".into()), None);
        assert_eq!(
            converted_model_path("Markers/CivilWarMarkers/CWAttSpawn02.nif".into()),
            None
        );
        assert_eq!(converted_model_path("Effects/FXRapids.nif".into()), None);
        assert_eq!(
            converted_model_path("meshes/Furniture/SitLedgeMarker.nif".into()),
            None
        );
    }

    #[test]
    fn unload_radius_removes_distant_exteriors_but_keeps_interiors() {
        let center = IVec2::new(4, -2);
        assert!(cell_within_unload_radius(
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 7,
                grid_y: -5,
            },
            center,
            3,
        ));
        assert!(!cell_within_unload_radius(
            CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 8,
                grid_y: -2,
            },
            center,
            3,
        ));
        assert!(cell_within_unload_radius(CellKey::Interior(99), center, 0));
    }

    /// An app with the cell plan, the unload pacer and the lifecycle validator chained as they run
    /// in `StreamingPlugin`. `collect_cells` is left out, so a requested cell stays `Loading` and
    /// the test drives the resident cells itself through [`spawn_resident_cell`].
    fn streaming_test_app(config: EngineConfig) -> (App, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE schema_info(version INTEGER NOT NULL);
                 INSERT INTO schema_info VALUES({});",
                shared::WORLD_DATABASE_SCHEMA_VERSION
            ))
            .unwrap();
        drop(connection);
        let mut app = App::new();
        app.insert_resource(config)
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(WorldDatabase::open(&path).unwrap())
            .init_resource::<StreamingWorld>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<TerrainContinuity>()
            .init_resource::<ProfilingState>()
            .add_systems(
                Update,
                (plan_cells, despawn_cells, validate_streaming_lifecycle).chain(),
            );
        (app, directory)
    }

    fn spawn_camera(app: &mut App, center: IVec2) {
        app.world_mut().spawn((
            Transform::from_xyz(
                center.x as f32 * CELL_SIZE,
                0.0,
                -(center.y as f32) * CELL_SIZE,
            ),
            StreamingCamera,
        ));
    }

    fn move_camera(app: &mut App, shift: IVec2) {
        let mut camera = app
            .world_mut()
            .query_filtered::<&mut Transform, With<StreamingCamera>>();
        let world = app.world_mut();
        let mut transforms = camera.query_mut(world);
        let mut transform = transforms.single_mut().unwrap();
        transform.translation.x += shift.x as f32 * CELL_SIZE;
        transform.translation.z -= shift.y as f32 * CELL_SIZE;
    }

    fn exterior_key(grid_x: i32, grid_y: i32) -> CellKey {
        CellKey::Exterior {
            worldspace_id: 0x3c,
            grid_x,
            grid_y,
        }
    }

    /// Spawns a cell root with a two-entity subtree under it and records the cell as resident or
    /// retiring, the state a committed cell is in.
    fn spawn_cell(app: &mut App, grid_x: i32, grid_y: i32, resident: bool) -> Entity {
        let root = app
            .world_mut()
            .spawn((
                CellRef(cell_id_of(grid_x, grid_y)),
                StreamedCellRoot,
                ExteriorCellGrid(IVec2::new(grid_x, grid_y)),
                Transform::default(),
                Visibility::default(),
            ))
            .id();
        let child = app
            .world_mut()
            .spawn((Transform::default(), ChildOf(root)))
            .id();
        app.world_mut()
            .spawn((Transform::default(), ChildOf(child)));
        let status = if resident {
            CellStatus::Resident { root }
        } else {
            CellStatus::Retiring { root }
        };
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(exterior_key(grid_x, grid_y), status);
        root
    }

    /// A steady window around `center`: the 7x7 cells the camera wants plus the trailing column a
    /// crossing drops, which is the shape the resident set settles into while flying.
    fn spawn_steady_window(app: &mut App, center: IVec2) {
        for grid_y in (center.y - 3)..=(center.y + 3) {
            for grid_x in (center.x - 4)..=(center.x + 3) {
                spawn_cell(app, grid_x, grid_y, true);
            }
        }
    }

    fn cell_id_of(grid_x: i32, grid_y: i32) -> u32 {
        ((grid_x + 64) as u32) * 256 + (grid_y + 64) as u32
    }

    fn resident_root_count(app: &mut App) -> usize {
        let mut query = app
            .world_mut()
            .query_filtered::<Entity, With<StreamedCellRoot>>();
        let world = app.world();
        query.iter(world).count()
    }

    fn streaming_metrics(app: &App) -> StreamingMetrics {
        app.world().resource::<StreamingMetrics>().clone()
    }

    #[test]
    fn a_crossing_unloads_cells_within_the_budget_and_finishes_within_ceil_frames() {
        let (mut app, _directory) = streaming_test_app(EngineConfig {
            stream_radius: 3,
            unload_radius: 4,
            max_cell_unloads_per_frame: 2,
            ..default()
        });
        spawn_camera(&mut app, IVec2::ZERO);
        spawn_steady_window(&mut app, IVec2::ZERO);
        let roots = resident_root_count(&mut app);
        app.update();
        let settled = streaming_metrics(&app);
        assert_eq!(
            settled.unloaded_cells, 0,
            "the window fits inside the radius"
        );
        assert_eq!(settled.retiring_cells, 0);
        assert_eq!(settled.streaming_invariant_failures, 0);

        // Cross into the next cell: the whole trailing column leaves the unload radius at once.
        move_camera(&mut app, IVec2::X);
        let mut unloads_per_frame = Vec::new();
        let mut roots_left = Vec::new();
        for frame in 1..=4 {
            app.update();
            let metrics = streaming_metrics(&app);
            unloads_per_frame.push(metrics.despawns_this_frame);
            assert!(
                metrics.despawns_this_frame <= 2,
                "frame {frame} unloaded {} cells against a budget of 2",
                metrics.despawns_this_frame
            );
            assert_eq!(metrics.streaming_invariant_failures, 0, "frame {frame}");
            roots_left.push(resident_root_count(&mut app));
        }
        assert_eq!(
            unloads_per_frame,
            vec![2, 2, 2, 1],
            "the column's 7 cells must spread over ceil(7 / 2) frames"
        );
        assert_eq!(roots_left, vec![roots - 2, roots - 4, roots - 6, roots - 7]);
        let metrics = streaming_metrics(&app);
        assert_eq!(metrics.unloaded_cells, 7);
        assert_eq!(metrics.retiring_cells, 0);
        assert_eq!(metrics.max_despawns_per_frame, 2);
        assert_eq!(metrics.streaming_invariant_failures, 0);
    }

    #[test]
    fn a_reversal_revives_retiring_cells_without_another_request() {
        let (mut app, _directory) = streaming_test_app(EngineConfig {
            stream_radius: 3,
            unload_radius: 4,
            max_cell_unloads_per_frame: 2,
            ..default()
        });
        spawn_camera(&mut app, IVec2::ZERO);
        spawn_steady_window(&mut app, IVec2::ZERO);
        app.update();
        move_camera(&mut app, IVec2::X);
        app.update();
        let after_crossing = streaming_metrics(&app);
        assert_eq!(
            after_crossing.retiring_cells, 5,
            "the crossing retires a column of 7 and the budget takes 2 in the same frame"
        );
        assert_eq!(after_crossing.unloaded_cells, 2);
        let roots_after_crossing = resident_root_count(&mut app);

        // Turn around before the budget has worked through the column.
        move_camera(&mut app, IVec2::NEG_X);
        app.update();
        let after_reversal = streaming_metrics(&app);
        assert_eq!(after_reversal.revived_cells, 5);
        assert_eq!(after_reversal.retiring_cells, 0);
        assert_eq!(after_reversal.despawns_this_frame, 0);
        assert_eq!(after_reversal.unloaded_cells, 2);
        assert_eq!(
            after_reversal.requests_submitted, after_crossing.requests_submitted,
            "a revived cell keeps its root, so it must not be requested again"
        );
        assert_eq!(resident_root_count(&mut app), roots_after_crossing);
        assert_eq!(after_reversal.streaming_invariant_failures, 0);
    }

    #[test]
    fn retire_backlog_bound_is_one_window_at_the_unload_radius() {
        assert_eq!(retire_backlog_bound(4), 81);
        assert_eq!(retire_backlog_bound(3), 49);
        assert_eq!(retire_backlog_bound(1), 9);
        assert_eq!(retire_backlog_bound(0), 1);
    }

    #[test]
    fn a_backlog_past_the_bound_unloads_every_retiring_cell_at_once() {
        let (mut app, _directory) = streaming_test_app(EngineConfig {
            stream_radius: 1,
            unload_radius: 1,
            max_cell_unloads_per_frame: 2,
            ..default()
        });
        spawn_camera(&mut app, IVec2::ZERO);
        // Under the bound the pacer keeps to the budget: 8 cells wait, 2 go per frame.
        for grid_x in 10..18 {
            spawn_cell(&mut app, grid_x, 0, false);
        }
        app.update();
        let paced = streaming_metrics(&app);
        assert_eq!(paced.retire_backlog_overflows, 0);
        assert_eq!(paced.despawns_this_frame, 2);
        assert_eq!(paced.retiring_cells, 6);
        assert_eq!(
            resident_root_count(&mut app),
            6,
            "the waiters keep their roots"
        );

        // Past the bound the pacer gives up pacing and takes the whole backlog in one frame.
        for grid_x in 20..40 {
            spawn_cell(&mut app, grid_x, 0, false);
        }
        app.update();
        let overflowed = streaming_metrics(&app);
        assert_eq!(overflowed.retire_backlog_overflows, 1);
        assert_eq!(overflowed.despawns_this_frame, 26);
        assert_eq!(overflowed.retiring_cells, 0);
        assert_eq!(overflowed.unloaded_cells, 28);
        assert_eq!(resident_root_count(&mut app), 0);
        assert_eq!(overflowed.streaming_invariant_failures, 0);
    }

    #[test]
    fn repeated_rebasing_preserves_camera_and_cell_root_locality() {
        let mut app = App::new();
        app.insert_resource(RenderOrigin(IVec2::ZERO))
            .init_resource::<StreamingMetrics>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, update_render_origin);
        let camera = app
            .world_mut()
            .spawn((Transform::default(), StreamingCamera))
            .id();
        let root = app
            .world_mut()
            .spawn((ExteriorCellGrid(IVec2::new(8, -3)), Transform::default()))
            .id();
        for shift in [IVec2::new(2, 1), IVec2::new(-3, 4), IVec2::new(7, -2)] {
            {
                let mut entity = app.world_mut().entity_mut(camera);
                let mut transform = entity.get_mut::<Transform>().unwrap();
                transform.translation.x = shift.x as f32 * CELL_SIZE + 12.0;
                transform.translation.z = -(shift.y as f32 * CELL_SIZE) - 20.0;
            }
            app.update();
            let camera_transform = app.world().entity(camera).get::<Transform>().unwrap();
            assert!(camera_transform.translation.x.abs() < CELL_SIZE);
            assert!(camera_transform.translation.z.abs() < CELL_SIZE);
        }
        assert_eq!(app.world().resource::<StreamingMetrics>().origin_rebases, 3);
        let origin = app.world().resource::<RenderOrigin>().0;
        let root_transform = app.world().entity(root).get::<Transform>().unwrap();
        assert_eq!(
            root_transform.translation,
            Vec3::new(
                (8 - origin.x) as f32 * CELL_SIZE,
                0.0,
                -(-3 - origin.y) as f32 * CELL_SIZE,
            )
        );
    }

    #[test]
    fn lifecycle_validator_detects_duplicate_and_orphaned_roots() {
        let mut app = App::new();
        app.insert_resource(EngineConfig::default())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .init_resource::<StreamingWorld>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, validate_streaming_lifecycle);
        app.world_mut()
            .spawn((Transform::default(), StreamingCamera));
        let resident = app.world_mut().spawn((CellRef(7), StreamedCellRoot)).id();
        app.world_mut().spawn((CellRef(7), StreamedCellRoot));
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(
                CellKey::Interior(7),
                CellStatus::Resident { root: resident },
            );
        app.update();
        let metrics = app.world().resource::<StreamingMetrics>();
        assert_eq!(metrics.duplicate_cell_roots, 1);
        assert_eq!(metrics.orphaned_cell_roots, 1);
        assert_eq!(metrics.missing_cell_roots, 0);
        assert_eq!(metrics.streaming_invariant_failures, 2);
    }

    #[test]
    fn maps_creation_position_and_rotation_through_the_same_basis() {
        assert_eq!(creation_to_bevy(Vec3::Y), Vec3::NEG_Z);
        assert_eq!(creation_to_bevy(Vec3::Z), Vec3::Y);

        let rotation = creation_rotation_to_bevy([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
        let rotated = rotation * Vec3::X;
        // Creation yaw turns clockwise: east (+X) turns to south (-Y), runtime +Z.
        assert!(rotated.abs_diff_eq(Vec3::Z, 1.0e-5));
    }

    #[test]
    fn creates_upward_wound_quadrants_with_continuous_uvs() {
        let terrain = terrain_fixture(1, 0.0);
        let mesh = build_terrain_quadrant_mesh(&terrain, 3).unwrap();
        assert_eq!(mesh.count_vertices(), 17 * 17);
        assert_eq!(mesh.indices().unwrap().len(), 16 * 16 * 6);
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .unwrap()
            .as_float3()
            .unwrap();
        let [a, b, c] = [positions[0], positions[1], positions[17]];
        let normal = (Vec3::from(b) - Vec3::from(a)).cross(Vec3::from(c) - Vec3::from(a));
        assert!(normal.y > 0.0);
        let VertexAttributeValues::Float32x2(uvs) = mesh.attribute(Mesh::ATTRIBUTE_UV_0).unwrap()
        else {
            panic!("terrain UVs must be Float32x2");
        };
        assert_eq!(uvs[0], [0.5, 0.5]);
        assert_eq!(uvs[16 * 17 + 16], [1.0, 1.0]);
    }

    #[test]
    fn accepts_matching_neighbor_edges_and_rejects_cracks() {
        let mut continuity = TerrainContinuity::default();
        let mut metrics = StreamingMetrics::default();
        let west = CellKey::Exterior {
            worldspace_id: 60,
            grid_x: 0,
            grid_y: 0,
        };
        let east = CellKey::Exterior {
            worldspace_id: 60,
            grid_x: 1,
            grid_y: 0,
        };
        validate_and_register_terrain_edges(
            west,
            &terrain_fixture(1, 10.0),
            &mut continuity,
            &mut metrics,
        )
        .unwrap();
        validate_and_register_terrain_edges(
            east,
            &terrain_fixture(2, 10.0),
            &mut continuity,
            &mut metrics,
        )
        .unwrap();
        assert_eq!(metrics.terrain_seams_validated, 1);

        let farther_east = CellKey::Exterior {
            worldspace_id: 60,
            grid_x: 2,
            grid_y: 0,
        };
        assert!(
            validate_and_register_terrain_edges(
                farther_east,
                &terrain_fixture(3, 11.0),
                &mut continuity,
                &mut metrics
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_more_than_six_layers_per_quadrant() {
        let mut terrain = terrain_fixture(1, 0.0);
        terrain
            .layers
            .extend((1..=6).map(|layer| TerrainLayerSnapshot {
                texture_form_id: u32::from(layer) + 10,
                quadrant: 0,
                layer,
                is_base: false,
                weights: Vec::new(),
            }));
        assert!(quadrant_layers(&terrain, 0).is_err());
    }

    #[test]
    fn accepts_textureless_official_land_quadrant() {
        let mut terrain = terrain_fixture(1, 0.0);
        terrain.layers.clear();
        assert!(quadrant_layers(&terrain, 0).unwrap().is_empty());
    }

    #[test]
    fn validates_loaded_material_images_and_rejects_missing_required_texture() {
        let mut images = Assets::<Image>::default();
        let base_color = images.add(Image::new_fill(
            bevy::render::render_resource::Extent3d {
                width: 2,
                height: 2,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            &[255, 255, 255, 255],
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        ));
        let material = StandardMaterial {
            base_color_texture: Some(base_color),
            ..default()
        };
        assert_eq!(validate_standard_material(&material, &images), Ok(1));

        let missing = StandardMaterial {
            normal_map_texture: Some(Handle::default()),
            ..default()
        };
        assert!(
            validate_standard_material(&missing, &images)
                .unwrap_err()
                .contains("normal image")
        );
    }

    #[test]
    fn rejects_invalid_alpha_and_culling_semantics() {
        let images = Assets::<Image>::default();
        assert!(
            validate_standard_material(
                &StandardMaterial {
                    alpha_mode: AlphaMode::Mask(f32::NAN),
                    ..default()
                },
                &images
            )
            .is_err()
        );
        assert!(
            validate_standard_material(
                &StandardMaterial {
                    double_sided: true,
                    ..default()
                },
                &images
            )
            .is_err()
        );
    }

    use bevy::asset::{AssetApp, AssetPlugin};
    use bevy::world_serialization::WorldSerializationPlugin;

    /// The app the model tests run in: the unload pacer, the arming pacer and the real readiness
    /// scan ([`track_asset_readiness`]) over an asset server and the world serialization spawner
    /// the engine uses, so a converted model is spawned and its reference becomes ready by the same
    /// route a converted glb takes. The three run chained, in the order `StreamingPlugin` runs
    /// them, so a test can tell which frame of that chain a model answered on.
    fn model_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            WorldSerializationPlugin,
        ))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .insert_resource(EngineConfig::default())
        .init_resource::<StreamingMetrics>()
        .init_resource::<StreamingWorld>()
        .init_resource::<TerrainContinuity>()
        .init_resource::<ProfilingState>()
        .init_resource::<DiagnosticFallbackAssets>()
        .init_resource::<SceneSpawnBatch>()
        // The converted scene holds entities, and the spawner reads each of their components out
        // of the type registry.
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<GlobalTransform>()
        .register_type::<Mesh3d>()
        .register_type::<MeshMaterial3d<StandardMaterial>>()
        .register_type::<Name>()
        .register_type::<Transform>()
        .add_observer(mark_world_instance_ready)
        .add_systems(
            Update,
            (despawn_cells, arm_pending_models, track_asset_readiness).chain(),
        )
        .add_systems(
            SpawnScene,
            (
                begin_scene_spawn_batch.before(SceneSpawnerSystems::WorldInstanceSpawn),
                end_scene_spawn_batch.after(SceneSpawnerSystems::WorldInstanceSpawn),
            ),
        );
        app
    }

    /// Adds `scene` to the asset server as the converted model a reference points at, and runs the
    /// frame the asset system needs to publish it, so `is_loaded_with_dependencies` is true for it
    /// exactly as it is for a loaded glb.
    fn add_converted_model(app: &mut App, scene: World) -> Handle<WorldAsset> {
        let handle = app
            .world()
            .resource::<AssetServer>()
            .add(WorldAsset::new(scene));
        // The `Loaded` event is applied in the asset schedule, before the spawner reads it.
        app.update();
        handle
    }

    /// A converted model as the loader builds one for the converter's empty scene: the scene's own
    /// root entity and nothing below it, so the model has no node and no mesh.
    fn empty_converted_scene() -> World {
        let mut world = World::new();
        world.spawn((Name::new("wispambush"), Transform::default()));
        world
    }

    /// A converted model with real geometry, as the loader builds one: the scene's root and the
    /// mesh primitive below it.
    fn converted_scene_with_mesh(mesh: Handle<Mesh>) -> World {
        let mut world = World::new();
        let root = world.spawn(Name::new("wispambush")).id();
        world.spawn((Mesh3d(mesh), Transform::default(), ChildOf(root)));
        world
    }

    /// A converted model whose primitive passes readiness: the mesh the converter exported and the
    /// material it points at, so the scan has something to validate rather than an empty model.
    fn converted_scene_with_material(
        mesh: Handle<Mesh>,
        material: Handle<StandardMaterial>,
    ) -> World {
        let mut world = World::new();
        // The loader gives every node a transform, the scene's own root included: the bounds walk
        // reads one from every node below the reference.
        let root = world
            .spawn((Name::new("wispambush"), Transform::default()))
            .id();
        world.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::default(),
            ChildOf(root),
        ));
        world
    }

    /// The aggregate bounds the converter writes for a `Cuboid::new(2.0, 4.0, 6.0)` primitive in a
    /// reference's own frame, which is the frame the bounds check compares them in.
    fn cuboid_bounds() -> ExpectedModelBounds {
        ExpectedModelBounds::new(Vec3::new(-1.0, -2.0, -3.0), Vec3::new(1.0, 2.0, 3.0))
            .expect("the fixture bounds are finite and not degenerate")
    }

    /// A config whose arming budget is `budget` models per frame.
    fn paced_config(budget: usize) -> EngineConfig {
        EngineConfig {
            max_model_spawns_per_frame: budget,
            ..default()
        }
    }

    /// The reference root components every model path shares, as [`spawn_cell`] spawns them: the
    /// placement, the ids, and the pending profile the readiness scan waits on.
    fn reference_root() -> impl Bundle {
        let transform = Transform::from_translation(Vec3::new(3.0, -4.0, 5.0));
        (
            Name::new("Reference 000F9907"),
            FormId(0x00F9907),
            CellRef(0x02D4E0),
            transform,
            GlobalTransform::from(transform),
            WorldTransform(transform.to_matrix()),
            PendingAssetProfile {
                started: Instant::now(),
                scene_spawned: false,
                path: "meshes/furniture/creatureexit/wispambush.glb".to_owned(),
                form_id: 0x00F9907,
                base_form_id: 0x00EF957,
                cell_id: 0x02D4E0,
            },
        )
    }

    /// A reference as [`spawn_cell`] spawns one for a model whose asset is already loaded: the root
    /// components, the asset root pointing at the loaded scene, and the pending profile the
    /// readiness scan waits on. No `ExpectedModelBounds` is inserted, which is what
    /// `statics.bounds_valid = 0` produces - the component's absence is the whole signal.
    fn spawn_model_reference(
        app: &mut App,
        handle: Handle<WorldAsset>,
        expected_bounds: Option<ExpectedModelBounds>,
    ) -> Entity {
        let mut entity = app
            .world_mut()
            .spawn((reference_root(), WorldAssetRoot(handle)));
        if let Some(bounds) = expected_bounds {
            entity.insert(bounds);
        }
        entity.id()
    }

    /// A reference as `spawn_cell` leaves one for [`arm_pending_models`]: the same root components
    /// and the model it found, but still pending, with `sequence` its place in spawn order. The
    /// cell path assigns that sequence from [`StreamingWorld::next_model_sequence`] as it walks the
    /// cell's references, so handing the sequence in is what a test does instead of committing a
    /// cell.
    fn spawn_pending_reference(app: &mut App, handle: Handle<WorldAsset>, sequence: u64) -> Entity {
        app.world_mut()
            .spawn((reference_root(), PendingModel { handle, sequence }))
            .id()
    }

    /// The references the arming pacer has handed to the spawner and the ones still waiting for
    /// their turn, which is the whole queue: arming removes the component that put a model in it.
    fn armed_and_pending(app: &mut App) -> (usize, usize) {
        let mut armed = app
            .world_mut()
            .query_filtered::<Entity, With<WorldAssetRoot>>();
        let armed = armed.iter(app.world()).count();
        let mut pending = app
            .world_mut()
            .query_filtered::<Entity, With<PendingModel>>();
        let pending = pending.iter(app.world()).count();
        (armed, pending)
    }

    /// Runs the readiness scan until the reference leaves the pending set, and fails the test
    /// rather than reading an unsettled metric. The world instance is spawned in `SpawnScene` and
    /// the scan runs in `Update`, so a reference settles over more than one frame. A model still
    /// waiting in the arming queue is not pending yet, so the queue has to drain too.
    fn settle_readiness(app: &mut App) -> StreamingMetrics {
        for _ in 0..16 {
            app.update();
            let streaming = app.world().resource::<StreamingMetrics>();
            if streaming.pending_asset_instances == 0 && streaming.arming_queue_depth == 0 {
                break;
            }
        }
        let metrics = app.world().resource::<StreamingMetrics>().clone();
        assert_eq!(
            metrics.pending_asset_instances, 0,
            "the reference never left the pending set"
        );
        assert_eq!(
            metrics.arming_queue_depth, 0,
            "the arming queue never drained"
        );
        metrics
    }

    /// The spawn-batch measurement reads the world rather than the spawner's bookkeeping: three
    /// models whose assets are already loaded are reported as three instances on the frame Bevy
    /// writes them into the world, and as none on the frame after, while the readiness scan
    /// completes the three of them on its own, later, scan.
    #[test]
    fn the_spawn_batch_counts_the_instances_bevy_instantiated_this_frame() {
        let mut app = model_app();
        // Every model is published before any reference points at it, so all three references are
        // handed to the spawner on the same frame.
        let handles: Vec<_> = (0..3)
            .map(|_| add_converted_model(&mut app, empty_converted_scene()))
            .collect();
        for handle in handles {
            spawn_model_reference(&mut app, handle, None);
        }

        app.update();
        let arrival = streaming_metrics(&app);
        assert_eq!(
            arrival.instances_spawned_this_frame, 3,
            "three references were handed to the spawner with their assets loaded"
        );
        assert_eq!(
            arrival.instances_armed_this_frame, 3,
            "the references joining the spawner's queue are the models armed this frame"
        );
        assert_eq!(
            arrival.instances_completed_this_scan, 0,
            "the scan validates an instance on a later frame than the one it spawns on"
        );

        app.update();
        let quiet = streaming_metrics(&app);
        assert_eq!(
            quiet.instances_spawned_this_frame, 0,
            "an instance spawns once, so the frame after reports none"
        );
        assert_eq!(
            quiet.instances_armed_this_frame, 0,
            "a model arms once, so the frame after reports none"
        );
        assert_eq!(quiet.max_instances_spawned_per_frame, 3);
        assert_eq!(
            quiet.instances_completed_this_scan, 3,
            "the scan completes the three instances it accepts"
        );
        assert_eq!(quiet.max_instances_completed_per_scan, 3);
        assert_eq!(
            quiet.empty_model_references, 3,
            "the empty converted scenes are counted, not failed"
        );
    }

    /// Pacing is what a frame instantiates: a cell's models wait in the arming queue and are handed
    /// over a budget at a time, so the spawn batch is the armed batch and a drained queue stays
    /// drained.
    #[test]
    fn the_arming_budget_spreads_a_cells_models_over_frames() {
        let mut app = model_app();
        app.insert_resource(paced_config(2));
        let handles: Vec<_> = (0..5)
            .map(|_| add_converted_model(&mut app, empty_converted_scene()))
            .collect();
        for (sequence, handle) in handles.into_iter().enumerate() {
            spawn_pending_reference(&mut app, handle, sequence as u64);
        }

        app.update();
        let first = streaming_metrics(&app);
        assert_eq!(
            armed_and_pending(&mut app),
            (2, 3),
            "a budget of two hands two models over and leaves the rest queued"
        );
        assert_eq!(first.arming_queue_depth, 3);
        assert_eq!(
            first.peak_arming_queue_depth, 5,
            "the peak is the cell's whole backlog"
        );
        assert_eq!(
            first.instances_spawned_this_frame, 2,
            "the frame instantiates the models the budget armed, not the whole cell"
        );

        app.update();
        assert_eq!(armed_and_pending(&mut app), (4, 1));
        app.update();
        assert_eq!(armed_and_pending(&mut app), (5, 0));
        assert_eq!(streaming_metrics(&app).arming_queue_depth, 0);

        app.update();
        assert_eq!(
            armed_and_pending(&mut app),
            (5, 0),
            "a model left the queue when it was armed, so none is armed twice"
        );
        assert_eq!(streaming_metrics(&app).instances_spawned_this_frame, 0);
    }

    /// A backlog drains oldest first, whatever order the references were spawned in: the entities
    /// here are created newest-first, so arming in entity order would arm the wrong model.
    #[test]
    fn the_arming_budget_arms_the_oldest_reference_first() {
        let mut app = model_app();
        app.insert_resource(paced_config(1));
        let handles: Vec<_> = (0..4)
            .map(|_| add_converted_model(&mut app, empty_converted_scene()))
            .collect();
        let mut oldest_first = Vec::new();
        for (index, handle) in handles.into_iter().enumerate() {
            // The reference spawned first is the newest model: the sequences run the other way.
            oldest_first.push(spawn_pending_reference(&mut app, handle, 3 - index as u64));
        }
        oldest_first.reverse();

        for (frame, reference) in oldest_first.iter().enumerate() {
            app.update();
            assert!(
                app.world().entity(*reference).contains::<WorldAssetRoot>(),
                "frame {frame} armed a newer model while the oldest one was still waiting"
            );
            assert_eq!(armed_and_pending(&mut app), (frame + 1, 3 - frame));
        }
    }

    /// A budget is spent on the models that can spawn now: a model whose converted scene has not
    /// arrived keeps its place in the queue without holding up the models behind it, and takes its
    /// turn as soon as it can.
    #[test]
    fn a_model_still_loading_does_not_use_a_budget_slot() {
        let mut app = model_app();
        app.insert_resource(paced_config(2));
        // A handle `Assets<WorldAsset>` has no entry for is the state a loading model is in: the
        // loader has not published the scene yet. The default handle is exactly that - identity
        // without storage - and this one gets its scene before the test ends.
        let loading = Handle::<WorldAsset>::default();
        let oldest = spawn_pending_reference(&mut app, loading.clone(), 0);
        let handles: Vec<_> = (0..4)
            .map(|_| add_converted_model(&mut app, empty_converted_scene()))
            .collect();
        for (sequence, handle) in handles.into_iter().enumerate() {
            spawn_pending_reference(&mut app, handle, sequence as u64 + 1);
        }

        app.update();
        assert!(
            !app.world().entity(oldest).contains::<WorldAssetRoot>(),
            "a model whose scene has not arrived is not armed"
        );
        assert_eq!(
            armed_and_pending(&mut app),
            (2, 3),
            "the budget went to the two models that are ready, not to the loading one"
        );
        assert_eq!(streaming_metrics(&app).arming_queue_depth, 3);

        app.world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .insert(&loading, WorldAsset::new(empty_converted_scene()))
            .expect("publishing the model's scene cannot fail");
        app.update();
        assert!(
            app.world().entity(oldest).contains::<WorldAssetRoot>(),
            "the loading model is armed once its scene arrives"
        );
        assert_eq!(armed_and_pending(&mut app), (4, 1));
    }

    /// A model whose cell is unloaded before its turn needs no cleanup, and the pacer cannot trip
    /// over it: the queue is the component set, and the despawn takes it with the subtree.
    #[test]
    fn a_pending_model_whose_cell_is_despawned_leaves_nothing_queued() {
        let mut app = model_app();
        app.insert_resource(paced_config(1));
        let handle = add_converted_model(&mut app, empty_converted_scene());
        let cell = app
            .world_mut()
            .spawn((
                Name::new("Cell"),
                StreamedCellRoot,
                ExteriorCellGrid(IVec2::ZERO),
                Transform::default(),
                Visibility::default(),
            ))
            .id();
        for sequence in 0..2 {
            let reference = spawn_pending_reference(&mut app, handle.clone(), sequence);
            app.world_mut().entity_mut(reference).insert(ChildOf(cell));
        }
        // The cell is outside the unload radius and its turn in the unload budget has come.
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(exterior_key(4, 4), CellStatus::Retiring { root: cell });

        app.update();

        assert_eq!(
            armed_and_pending(&mut app),
            (0, 0),
            "the cell took its pending models with it; none was armed and none is queued"
        );
        let metrics = streaming_metrics(&app);
        assert_eq!(metrics.arming_queue_depth, 0);
        assert_eq!(metrics.unloaded_cells, 1, "the cell itself was unloaded");
        assert_eq!(metrics.instances_spawned_this_frame, 0);
    }

    /// `0` is the unbudgeted behaviour: every model whose scene is ready is handed over at once.
    #[test]
    fn an_arming_budget_of_zero_arms_every_ready_model() {
        let mut app = model_app();
        app.insert_resource(paced_config(0));
        let handles: Vec<_> = (0..5)
            .map(|_| add_converted_model(&mut app, empty_converted_scene()))
            .collect();
        for (sequence, handle) in handles.into_iter().enumerate() {
            spawn_pending_reference(&mut app, handle, sequence as u64);
        }

        app.update();
        assert_eq!(armed_and_pending(&mut app), (5, 0));
        assert_eq!(
            streaming_metrics(&app).instances_spawned_this_frame,
            5,
            "without a budget the whole batch lands on one frame, as it did before the pacer"
        );
    }

    /// Pacing moves the frame a model is spawned on, not the totals: every queued model is armed
    /// once, spawned once, and validated exactly once, and no frame instantiates more than the
    /// budget.
    #[test]
    fn every_paced_model_is_spawned_and_validated_exactly_once() {
        let mut app = model_app();
        app.insert_resource(paced_config(2));
        let mut handles = Vec::new();
        for _ in 0..5 {
            let mesh = app
                .world_mut()
                .resource_mut::<Assets<Mesh>>()
                .add(Cuboid::new(2.0, 4.0, 6.0));
            let material = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial::default());
            handles.push(add_converted_model(
                &mut app,
                converted_scene_with_material(mesh, material),
            ));
        }
        for (sequence, handle) in handles.into_iter().enumerate() {
            let reference = spawn_pending_reference(&mut app, handle, sequence as u64);
            app.world_mut()
                .entity_mut(reference)
                .insert(cuboid_bounds());
        }

        let metrics = settle_readiness(&mut app);

        assert_eq!(
            metrics.assets_ready, 5,
            "every instance is validated once: {:?}",
            metrics.asset_failures
        );
        assert_eq!(metrics.meshes_validated, 5);
        assert_eq!(metrics.materials_validated, 5);
        assert_eq!(metrics.transform_instances_validated, 5);
        assert_eq!(metrics.empty_model_references, 0);
        assert_eq!(metrics.peak_arming_queue_depth, 5);
        assert_eq!(
            metrics.max_instances_spawned_per_frame, 2,
            "no frame instantiated more than the budget allowed"
        );
        assert_eq!(armed_and_pending(&mut app), (5, 0));
        let mut instances = app
            .world_mut()
            .query_filtered::<Entity, With<WorldInstance>>();
        assert_eq!(
            instances.iter(app.world()).count(),
            5,
            "one instance per reference, paced or not"
        );
    }

    /// What the emptiness rule reads: the converted model's own node and mesh count, taken from
    /// the asset rather than from anything the engine spawned.
    #[test]
    fn reads_a_converted_models_own_node_and_mesh_count() {
        let empty = WorldAsset::new(empty_converted_scene());
        assert!(
            converted_scene_contents(&empty).is_empty(),
            "the converter's empty scene is what an empty model looks like"
        );

        let mesh_model = WorldAsset::new(converted_scene_with_mesh(Handle::default()));
        let contents = converted_scene_contents(&mesh_model);
        assert_eq!(contents.meshes, 1, "one primitive: {contents:?}");
        assert_eq!(contents.nodes, 1, "one node: {contents:?}");
    }

    /// A model with no renderable geometry converts to an **empty scene**, so it has no converted
    /// bounds and nothing to draw. A streamed cell full of such models used to fail the bounds
    /// gate on a model with nothing to draw. Such a reference is skipped and counted on its own,
    /// and nothing fails.
    #[test]
    fn an_empty_scene_model_is_skipped_and_counted_instead_of_failing() {
        let mut app = model_app();
        let handle = add_converted_model(&mut app, empty_converted_scene());
        let reference = spawn_model_reference(&mut app, handle, None);

        let metrics = settle_readiness(&mut app);

        assert_eq!(
            metrics.transform_bounds_validation_failures, 0,
            "an invisible marker is not a conversion failure: {:?}",
            metrics.asset_failures
        );
        assert_eq!(
            metrics.empty_model_references, 1,
            "the empty model is counted on its own"
        );
        assert!(
            metrics.asset_failures.is_empty(),
            "nothing is recorded as a failed asset: {:?}",
            metrics.asset_failures
        );
        assert_eq!(
            metrics.bounds_validated, 0,
            "there were no converted bounds to validate"
        );
        assert_eq!(
            metrics.assets_ready, 0,
            "an empty model is not counted as a ready asset either"
        );
        assert!(
            !app.world()
                .entity(reference)
                .contains::<PendingAssetProfile>(),
            "the reference is no longer pending"
        );
        assert!(
            app.world().entity(reference).contains::<Transform>(),
            "the empty reference keeps its own transform"
        );
        assert!(
            app.world()
                .entity(reference)
                .get::<Children>()
                .is_some_and(|children| !children.is_empty()),
            "the converted scene is spawned below the reference"
        );
    }

    /// The tolerated class is narrow: the model itself must be empty. A converted scene that
    /// declares a node but no mesh - a hierarchy whose geometry an exporter dropped - is not an
    /// empty model, so it keeps failing exactly as it did before the rule existed.
    #[test]
    fn a_model_whose_converted_scene_declares_only_a_node_still_fails() {
        let mut app = model_app();
        let mut scene = World::new();
        let root = scene.spawn(Transform::default()).id();
        scene.spawn((Transform::default(), ChildOf(root)));
        let handle = add_converted_model(&mut app, scene);
        spawn_model_reference(&mut app, handle, None);

        let metrics = settle_readiness(&mut app);

        assert_eq!(
            metrics.empty_model_references, 0,
            "a scene with a node in it is not an empty model"
        );
        assert_eq!(
            metrics.transform_bounds_validation_failures, 1,
            "a model that declares a node and no mesh must still fail: {:?}",
            metrics.asset_failures
        );
        let failure = metrics
            .asset_failures
            .first()
            .expect("the failure is recorded as an asset failure");
        assert!(
            failure
                .dependency_chain
                .iter()
                .any(|reason| reason.contains("is not empty: 0 mesh primitives, 1 nodes")),
            "unexpected failure reason: {:?}",
            failure.dependency_chain
        );
    }

    /// The other side of the same evidence: a model whose converted scene really holds a mesh
    /// primitive, spawned from the asset by the engine's own spawner rather than hand-built, and
    /// which still arrives without converted bounds, is a conversion defect and stays fatal.
    #[test]
    fn a_model_whose_converted_scene_holds_a_mesh_still_fails_without_bounds() {
        let mut app = model_app();
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(2.0, 4.0, 6.0));
        let handle = add_converted_model(&mut app, converted_scene_with_mesh(mesh));
        let reference = spawn_model_reference(&mut app, handle, None);

        let metrics = settle_readiness(&mut app);

        assert_eq!(
            metrics.empty_model_references, 0,
            "a model with geometry is never counted as empty"
        );
        assert_eq!(
            metrics.transform_bounds_validation_failures, 1,
            "a model with geometry and no converted bounds must still fail: {:?}",
            metrics.asset_failures
        );
        let failure = metrics
            .asset_failures
            .first()
            .expect("the failure is recorded as an asset failure");
        assert!(
            failure
                .dependency_chain
                .iter()
                .any(|reason| reason.contains("is not empty: 1 mesh primitives")),
            "unexpected failure reason: {:?}",
            failure.dependency_chain
        );
        // The model's mesh is a descendant of the reference: the engine's own spawner put it
        // there, which is the shape every bounds check in this module reads.
        let mut primitives = app.world_mut().query::<(Entity, &Mesh3d)>();
        let (primitive, _) = primitives
            .iter(app.world())
            .next()
            .expect("the converted model's mesh primitive is spawned");
        let scene_root = app
            .world()
            .entity(reference)
            .get::<Children>()
            .and_then(|children| children.first().copied())
            .expect("the converted scene is spawned below the reference");
        assert_eq!(
            app.world()
                .entity(primitive)
                .get::<ChildOf>()
                .map(ChildOf::parent),
            Some(scene_root),
            "the mesh primitive hangs below the scene root the spawner attached"
        );
    }

    /// The other direction of the same rule: a model the converter *did* bound, whose spawned
    /// scene turns out to be empty, is not an empty model to wave through - the two disagree and
    /// the disagreement is fatal.
    #[test]
    fn a_bounded_model_whose_scene_is_empty_still_fails() {
        let mut app = model_app();
        let handle = add_converted_model(&mut app, empty_converted_scene());
        spawn_model_reference(
            &mut app,
            handle,
            ExpectedModelBounds::new(Vec3::splat(-1.0), Vec3::splat(1.0)),
        );

        let metrics = settle_readiness(&mut app);

        assert_eq!(
            metrics.empty_model_references, 0,
            "only a model without converted bounds may be an empty model"
        );
        assert_eq!(
            metrics.transform_bounds_validation_failures, 1,
            "a bound model whose hierarchy holds no mesh must still fail: {:?}",
            metrics.asset_failures
        );
        let failure = metrics
            .asset_failures
            .first()
            .expect("the failure is recorded as an asset failure");
        assert!(
            failure
                .dependency_chain
                .iter()
                .any(|reason| reason.contains("no bounded mesh")),
            "unexpected failure reason: {:?}",
            failure.dependency_chain
        );
    }

    /// Composes the same basis-rotation / mesh-translation chain as `HumanSkull.glb`'s node
    /// hierarchy in f64, giving a ground-truth model-space (relative-to-root) bounding box
    /// that never touches the root's large world-space position. This stands in for the
    /// converter's own independent, exact recomputation of the model's bounds.
    fn f64_relative_bounds(
        local_min: Vec3,
        local_max: Vec3,
        basis_rotation: Quat,
        mesh_translation: Vec3,
        mesh_scale: Vec3,
    ) -> (Vec3, Vec3) {
        use bevy::math::{DAffine3, DQuat, DVec3};

        let basis = DAffine3::from_quat(DQuat::from_xyzw(
            basis_rotation.x as f64,
            basis_rotation.y as f64,
            basis_rotation.z as f64,
            basis_rotation.w as f64,
        ));
        let mesh = DAffine3::from_scale_rotation_translation(
            DVec3::new(
                mesh_scale.x as f64,
                mesh_scale.y as f64,
                mesh_scale.z as f64,
            ),
            DQuat::IDENTITY,
            DVec3::new(
                mesh_translation.x as f64,
                mesh_translation.y as f64,
                mesh_translation.z as f64,
            ),
        );
        let relative = basis * mesh;
        let mut min = DVec3::splat(f64::INFINITY);
        let mut max = DVec3::splat(f64::NEG_INFINITY);
        for x in [local_min.x, local_max.x] {
            for y in [local_min.y, local_max.y] {
                for z in [local_min.z, local_max.z] {
                    let point = relative.transform_point3(DVec3::new(x as f64, y as f64, z as f64));
                    min = min.min(point);
                    max = max.max(point);
                }
            }
        }
        (
            Vec3::new(min.x as f32, min.y as f32, min.z as f32),
            Vec3::new(max.x as f32, max.y as f32, max.z as f32),
        )
    }

    // Composing each mesh's world transform and then multiplying by the root's
    // inverse world transform (the old algorithm) loses precision at real-data world
    // placements. This fixture mirrors the failing acceptance run: reference 000F6031's
    // world position/rotation, and HumanSkull.glb's node hierarchy (a -90-degree X basis
    // node with a mesh child of scale ~1.14 and translation (0, -7.7, -150.7)).
    #[test]
    fn spawned_bounds_validate_within_tolerance_despite_large_world_placement() {
        use bevy::ecs::system::SystemState;

        let root_translation = Vec3::new(20790.89, -69970.35, 10984.74);
        let root_rotation = Quat::from_euler(EulerRot::XYZ, 2.0587, 0.6207, 1.3418);
        let basis_rotation = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let mesh_translation = Vec3::new(0.0, -7.7, -150.7);
        let mesh_scale = Vec3::splat(1.14);
        let local_min = Vec3::splat(-6.0);
        let local_max = Vec3::splat(6.0);

        let (expected_min, expected_max) = f64_relative_bounds(
            local_min,
            local_max,
            basis_rotation,
            mesh_translation,
            mesh_scale,
        );
        let expected_bounds = ExpectedModelBounds::new(expected_min, expected_max)
            .expect("fixture bounds must be finite and non-degenerate");
        let extent = (expected_max - expected_min).abs().max_element().max(1.0);
        let tolerance = (extent * 1.0e-4).max(1.0e-3);

        let mut world = World::new();
        let mut meshes = Assets::<Mesh>::default();
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        let corners: Vec<[f32; 3]> = [local_min.x, local_max.x]
            .into_iter()
            .flat_map(|x| {
                [local_min.y, local_max.y]
                    .into_iter()
                    .flat_map(move |y| [local_min.z, local_max.z].map(move |z| [x, y, z]))
            })
            .collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, corners);
        let mesh_handle = meshes.add(mesh);
        world.insert_resource(meshes);

        let root_local = Transform {
            translation: root_translation,
            rotation: root_rotation,
            ..Default::default()
        };
        let root_global = GlobalTransform::from(root_local);
        let root = world.spawn((root_local, root_global)).id();

        let basis_local = Transform {
            rotation: basis_rotation,
            ..Default::default()
        };
        let basis_global = root_global.mul_transform(basis_local);
        let basis = world.spawn((basis_local, basis_global, ChildOf(root))).id();

        let mesh_local = Transform {
            translation: mesh_translation,
            scale: mesh_scale,
            ..Default::default()
        };
        let mesh_global = basis_global.mul_transform(mesh_local);
        world.spawn((mesh_local, mesh_global, ChildOf(basis), Mesh3d(mesh_handle)));

        #[allow(clippy::type_complexity)]
        let mut system_state: SystemState<(
            Query<&Children>,
            Query<(&Transform, &GlobalTransform)>,
            RenderPrimitiveQuery,
        )> = SystemState::new(&mut world);
        let (children, transforms, primitives) = system_state.get(&world).unwrap();
        let meshes = world.resource::<Assets<Mesh>>();

        // The old algorithm: compose each descendant's absolute GlobalTransform (which
        // bakes in the root's huge world position), then cancel that position back out by
        // multiplying by the root's inverted absolute GlobalTransform.
        let root_inverse = root_global.affine().inverse();
        let mut old_min = Vec3::splat(f32::INFINITY);
        let mut old_max = Vec3::splat(f32::NEG_INFINITY);
        for descendant in children.iter_descendants(root) {
            let Ok((mesh_handle, _, _)) = primitives.get(descendant) else {
                continue;
            };
            let mesh = meshes.get(mesh_handle).unwrap();
            let aabb = mesh.compute_aabb().unwrap();
            let center = Vec3::from(aabb.center);
            let half_extents = Vec3::from(aabb.half_extents);
            let (_, global) = transforms.get(descendant).unwrap();
            let relative = Mat4::from(root_inverse * global.affine());
            let transformed =
                InstanceBounds::transformed(center - half_extents, center + half_extents, relative);
            old_min = old_min.min(transformed.min);
            old_max = old_max.max(transformed.max);
        }
        let old_error = (old_min - expected_min)
            .abs()
            .max((old_max - expected_max).abs())
            .max_element();
        assert!(
            old_error > tolerance,
            "expected the old world-transform-and-invert composition to exceed tolerance \
             {tolerance} (it should reproduce the acceptance failure), got error \
             {old_error}"
        );

        // The fix, exercised through the real validation function: composing local
        // transforms along the path from the root never forms the large-magnitude matrix,
        // so it validates within tolerance.
        let world_transform = WorldTransform(root_local.to_matrix());
        let summary = validate_spawned_transforms_and_bounds(
            root,
            &root_local,
            &root_global,
            &world_transform,
            Some(&expected_bounds),
            None,
            &children,
            &transforms,
            &primitives,
            meshes,
        )
        .unwrap_or_else(|error| {
            panic!(
                "expected local-transform composition to validate within tolerance {tolerance}: \
                 {error}"
            )
        });
        assert_eq!(summary.nodes, 2);

        // Measure the new method's own error directly (mirroring what
        // `validate_spawned_transforms_and_bounds` computes internally) to report it
        // alongside the old method's.
        let mut new_min = Vec3::splat(f32::INFINITY);
        let mut new_max = Vec3::splat(f32::NEG_INFINITY);
        let mut new_nodes = 0usize;
        let mut new_bounded_meshes = 0usize;
        if let Ok(direct_children) = children.get(root) {
            for child in direct_children.iter() {
                accumulate_relative_bounds(
                    child,
                    Affine3A::IDENTITY,
                    &children,
                    &transforms,
                    &primitives,
                    meshes,
                    &mut new_nodes,
                    &mut new_bounded_meshes,
                    &mut new_min,
                    &mut new_max,
                )
                .unwrap();
            }
        }
        let new_error = (new_min - expected_min)
            .abs()
            .max((new_max - expected_max).abs())
            .max_element();
        assert!(
            new_error <= tolerance,
            "new method exceeded tolerance {tolerance}: error {new_error}"
        );
        eprintln!(
            "bounds precision fixture: tolerance={tolerance}, old_error={old_error}, new_error={new_error}"
        );
    }

    #[test]
    fn a_deeply_nested_model_is_bounded_without_overflowing_the_stack() {
        use bevy::ecs::system::SystemState;

        // 50,000 nested nodes with one unit cube at the leaf: a recursive walk would
        // overflow a test thread's stack long before the leaf.
        const DEPTH: usize = 50_000;
        let mut world = World::new();
        let mut meshes = Assets::<Mesh>::default();
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![[-0.5, -0.5, -0.5], [0.5, 0.5, 0.5], [0.5, -0.5, 0.5]],
        );
        let mesh_handle = meshes.add(mesh);
        world.insert_resource(meshes);

        let root = world
            .spawn((Transform::default(), GlobalTransform::default()))
            .id();
        let mut parent = root;
        for _ in 0..DEPTH {
            parent = world
                .spawn((
                    Transform::default(),
                    GlobalTransform::default(),
                    ChildOf(parent),
                ))
                .id();
        }
        world.spawn((
            Transform::default(),
            GlobalTransform::default(),
            ChildOf(parent),
            Mesh3d(mesh_handle),
        ));

        #[allow(clippy::type_complexity)]
        let mut system_state: SystemState<(
            Query<&Children>,
            Query<(&Transform, &GlobalTransform)>,
            RenderPrimitiveQuery,
        )> = SystemState::new(&mut world);
        let (children, transforms, primitives) = system_state.get(&world).unwrap();
        let meshes = world.resource::<Assets<Mesh>>();

        let mut nodes = 0usize;
        let mut bounded_meshes = 0usize;
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        accumulate_relative_bounds(
            root,
            Affine3A::IDENTITY,
            &children,
            &transforms,
            &primitives,
            meshes,
            &mut nodes,
            &mut bounded_meshes,
            &mut min,
            &mut max,
        )
        .unwrap();
        assert_eq!(nodes, DEPTH + 2);
        assert_eq!(bounded_meshes, 1);
        assert_eq!((min, max), (Vec3::splat(-0.5), Vec3::splat(0.5)));
    }
}
