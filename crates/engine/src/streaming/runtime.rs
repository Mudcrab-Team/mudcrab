//! Admission, downstream pacing and retained byte estimates for one immutable pack.
//! Reservations are estimates; process pressure is an independent backstop.

use super::{
    CellStatus, PendingAssetProfile, PendingModel, StreamingMetrics, StreamingWorld,
    admission::{SceneAdmission, SceneKey},
    control::{
        ControllerDecision, ControllerInput, ControllerSettings, ControllerState,
        DownstreamBacklog, StageBudgets,
    },
    requests::{self, SceneRequest, SceneRequestKey},
    reservations::{MemoryLedger, ResourceReservation},
};
use crate::nif_depth::NifDepthMaterial;
use crate::skin_material::{SkyrimSkinDepthMaterial, SkyrimSkinMaterial};
use crate::{
    config::EngineConfig,
    profiling::ProfilingState,
    render::{TerrainMaterial, WaterMaterial},
    streaming_preparation::{
        PreparationDemand, PreparationKey, StreamingPreparationBridge, scene_demand,
        scene_dependencies_available,
    },
    world::{
        components::CELL_SIZE,
        database::{CellKey, DatabaseResponse, WorldDatabase},
    },
};
use bevy::{
    asset::{AssetId, LoadState},
    diagnostic::{DiagnosticsStore, SystemInfo, SystemInformationDiagnosticsPlugin},
    math::DVec3,
    prelude::*,
    render::render_asset::RenderAssetBytesPerFrame,
    world_serialization::WorldAsset,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use shared::streaming_costs::{
    ByteEstimate, ResolvedSceneCost, ResourceKind, STREAMING_COST_FILE_NAME, StreamingCostCatalog,
    canonical_resource_key,
};
use std::{
    any::TypeId,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
};

const MIB: u64 = 1024 * 1024;
const UNKNOWN_BYTES: u64 = 64 * MIB;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RuntimeSnapshot {
    pub frame: u64,
    pub control: ControllerDecision,
    pub backlog: DownstreamBacklog,
    pub resident_reserved_bytes: u64,
    pub transient_reserved_bytes: u64,
    pub orphan_reserved_bytes: u64,
    pub memory_limit_bytes: Option<u64>,
    pub memory_denied_total: u64,
    /// Admission denials since the preceding control update, not queued scenes.
    pub memory_denied_since_last_update: u64,
    /// Physical pressure or a reservation denial in the preceding interval.
    pub memory_blocked: bool,
    pub physical_memory_blocked: bool,
    pub unobserved_scene_dependencies: usize,
    pub unknown_scene_estimates: usize,
    pub catalog_loaded: bool,
    pub process_rss_bytes: Option<u64>,
    pub system_free_estimate_bytes: Option<u64>,
    pub pressure_sample_fresh: bool,
    pub gpu_observation_available: bool,
}

struct SceneReservation {
    owner: String,
    resources: Vec<String>,
    cost: ResolvedSceneCost,
    id: Option<AssetId<WorldAsset>>,
    dependencies_cached: bool,
    prepared: bool,
}

/// Logical demand can have several simultaneous allocation lifetimes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CellAttempt {
    key: CellKey,
    generation: u64,
}

struct CellReservation {
    owner: String,
    resource: String,
    root: Option<Entity>,
    dependencies_cached: bool,
}

struct ResourceWatch {
    demand: PreparationDemand,
    /// Generated dependencies prepare together; disposal keeps only images not covered by
    /// separate catalog texture claims, so another live cell cannot pin dead geometry.
    allocation_images: Option<Vec<AssetId<Image>>>,
    root_scene: Option<AssetId<WorldAsset>>,
    root_entity: Option<Entity>,
    texture_path: Option<String>,
    orphaned_frame: Option<u64>,
    /// A loaded source scene had dependencies that we could not inspect.
    /// Keep its charge even after the root disappears; empty IDs prove nothing.
    unobserved_scenes: HashSet<AssetId<WorldAsset>>,
}

#[derive(Resource)]
pub(crate) struct StreamingRuntime {
    enabled: bool,
    pub decision: Option<ControllerDecision>,
    pub snapshot: Option<RuntimeSnapshot>,
    controller: ControllerState,
    settings: ControllerSettings,
    catalog: StreamingCostCatalog,
    catalog_loaded: bool,
    pack_identity: String,
    ledger: MemoryLedger,
    scenes: BTreeMap<SceneKey, SceneReservation>,
    pending_cells: HashMap<CellAttempt, CellReservation>,
    cells: HashMap<Entity, CellReservation>,
    placements: HashMap<Entity, String>,
    watches: BTreeMap<String, ResourceWatch>,
    prepared_resources: HashSet<String>,
    next_resource: u64,
    frame: u64,
    previous_camera: Option<(DVec3, DVec3, u32)>,
    /// Commit window from `collect_cells` through `finish_streaming_commit_budget`. It already
    /// contains the readiness scan, so it must not be added to `previous_validation_ms`.
    pub previous_commit_ms: f64,
    /// Scene spawn batch, measured outside the commit window.
    pub previous_spawn_ms: f64,
    /// Readiness scan diagnostic, a subset of `previous_commit_ms`. Not an additive input.
    pub previous_validation_ms: f64,
    pressure_blocked: bool,
    previous_denied_total: u64,
    memory_limit: Option<u64>,
    pub deferred_responses: VecDeque<DatabaseResponse>,
}

impl FromWorld for StreamingRuntime {
    fn from_world(world: &mut World) -> Self {
        let config = world.resource::<EngineConfig>();
        let enabled = config.streaming_controls_enabled();
        let manifest = enabled
            .then(|| std::fs::read(config.assets_dir.join("conversion-manifest.json")).ok())
            .flatten();
        let fingerprint = manifest
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .unwrap_or_default();
        let path = config
            .streaming_costs
            .clone()
            .unwrap_or_else(|| config.assets_dir.join(STREAMING_COST_FILE_NAME));
        let loaded = enabled.then(|| StreamingCostCatalog::load(&path, &fingerprint));
        let catalog_loaded = loaded.as_ref().is_some_and(|result| result.is_ok());
        let catalog = match loaded {
            Some(Ok(catalog)) => catalog,
            Some(Err(error)) => {
                warn!(path = %path.display(), %error, unknown_resident_bytes = UNKNOWN_BYTES,
                    unknown_transient_bytes = UNKNOWN_BYTES,
                    "Streaming cost metadata unavailable; conservative reservations can leave required scenery queued indefinitely. Reconvert the pack or generate a sidecar with streaming-costs --assets <pack> --output <new-file-outside-pack> and pass --streaming-costs <new-file-outside-pack>");
                StreamingCostCatalog::empty(fingerprint.clone())
            }
            None => StreamingCostCatalog::empty(fingerprint.clone()),
        };
        let hard = configured_budgets(config);
        let limit = config.streaming_backlog_limit();
        let mut settings = ControllerSettings {
            adaptive: config.adaptive_streaming,
            fixed: hard,
            target_frame_ms: config.streaming_frame_ms,
            ..default()
        };
        if limit != 0 {
            settings.high_watermarks.ready_placements = limit;
            settings.high_watermarks.spawned_instances = limit;
            settings.high_watermarks.collider_jobs = limit;
            settings.high_watermarks.gpu_assets = limit.saturating_mul(4);
            settings.low_watermarks.ready_placements = limit / 4;
            settings.low_watermarks.spawned_instances = limit / 4;
            settings.low_watermarks.collider_jobs = limit / 4;
            settings.low_watermarks.gpu_assets = limit;
        } else {
            settings.high_watermarks = default();
            settings.low_watermarks = default();
        }
        Self {
            enabled,
            decision: None,
            snapshot: None,
            controller: default(),
            settings,
            catalog,
            catalog_loaded,
            pack_identity: format!("{}#{fingerprint}", config.assets_dir.display()),
            ledger: default(),
            scenes: default(),
            pending_cells: default(),
            cells: default(),
            placements: default(),
            watches: default(),
            prepared_resources: default(),
            next_resource: 0,
            frame: 0,
            previous_camera: None,
            previous_commit_ms: 0.0,
            previous_spawn_ms: 0.0,
            previous_validation_ms: 0.0,
            pressure_blocked: false,
            previous_denied_total: 0,
            memory_limit: automatic_memory_limit(config, world),
            deferred_responses: default(),
        }
    }
}

fn configured_budgets(config: &EngineConfig) -> StageBudgets {
    // Adaptive ceilings are finite even where the old fixed option meant unlimited.
    StageBudgets {
        max_scene_jobs: if config.max_scene_loads == 0 {
            128
        } else {
            config.max_scene_loads
        },
        max_model_activations: if config.max_model_spawns_per_frame == 0 {
            128
        } else {
            config.max_model_spawns_per_frame
        },
        max_cell_commits: config.max_cell_commits_per_frame.max(1),
        max_upload_bytes_per_frame: config
            .max_upload_bytes_per_frame()
            .map_or(32 * MIB, |bytes| bytes as u64),
        max_commit_micros: config.max_commit_micros_per_frame,
        ..default()
    }
}

fn physical_memory_bytes(world: &World) -> Option<u64> {
    world
        .get_resource::<SystemInfo>()
        .and_then(|info| info.memory.strip_suffix(" GiB")?.parse::<f64>().ok())
        .filter(|gib| gib.is_finite() && *gib > 0.0)
        .map(|gib| (gib * 1024.0 * 1024.0 * 1024.0) as u64)
}

fn automatic_memory_limit(config: &EngineConfig, world: &World) -> Option<u64> {
    if !config.adaptive_streaming || config.streaming_memory_mib != 0 {
        return config.memory_budget_bytes();
    }
    physical_memory_bytes(world)
        .map(|physical| {
            (physical / 2)
                .min(16 * 1024 * MIB)
                .min(physical.saturating_sub(config.streaming_headroom_bytes()))
                .max(1)
        })
        .or_else(|| config.memory_budget_bytes())
}

impl StreamingRuntime {
    fn add_watch(
        &mut self,
        resource: String,
        root_scene: Option<AssetId<WorldAsset>>,
        root_entity: Option<Entity>,
        texture_path: Option<String>,
    ) {
        if let Some(watch) = self.watches.get_mut(&resource) {
            watch.orphaned_frame = None;
            if root_scene.is_some() {
                watch.root_scene = root_scene;
            }
            return;
        }
        self.next_resource = self.next_resource.saturating_add(1);
        self.watches.insert(
            resource,
            ResourceWatch {
                demand: PreparationDemand {
                    key: PreparationKey::Resource(self.next_resource),
                    meshes: vec![],
                    images: vec![],
                    materials: vec![],
                },
                allocation_images: None,
                root_scene,
                root_entity,
                texture_path,
                orphaned_frame: None,
                unobserved_scenes: HashSet::new(),
            },
        );
    }

    fn resource_key(&self, path: &str, content: Option<&str>) -> String {
        format!("{}|{path}|{}", self.pack_identity, content.unwrap_or(""))
    }

    pub fn reserve_scene(&mut self, key: &SceneKey) -> bool {
        if self.scenes.contains_key(key) {
            return true;
        }
        self.reserve_scene_with_placements(key, &[])
    }

    pub fn reserve_scene_with_placements(
        &mut self,
        key: &SceneKey,
        placements: &[(Entity, bool)],
    ) -> bool {
        if !self.enabled {
            return true;
        }
        if let Some(scene) = self.scenes.get(key) {
            // A queued retry must first retire the previous loader generation.
            // Reusing its bytes for a new ID would undercount overlapping GPU data.
            return scene.id.is_none();
        }
        if self.pressure_blocked {
            return false;
        }
        let Ok(cost) = self
            .catalog
            .resolve_scene(&key.canonical_path, UNKNOWN_BYTES)
        else {
            return false;
        };
        let owner = format!("scene:{key:?}");
        let mut bundle = vec![];
        let mut keys = vec![];
        for (path, cost) in &cost.resources {
            let content = (cost.kind == ResourceKind::SceneGeometry)
                .then_some(key.content_identity.as_deref())
                .flatten();
            let resource = self.resource_key(path, content);
            // A disposed loader may create a new GPU generation while the old
            // one is still retiring. Wait rather than charging both copies once.
            if self.ledger.contains_resource(&resource)
                && !self.ledger.resource_has_owners(&resource)
            {
                return false;
            }
            bundle.push(ResourceReservation {
                key: resource.clone(),
                resident_bytes: cost.resident_bytes,
                transient_bytes: cost.peak_transient_bytes,
            });
            keys.push(resource);
        }
        let mut planned = vec![];
        for &(entity, collision) in placements {
            if self.placements.contains_key(&entity) {
                continue;
            }
            let resource = format!("placement:{}", entity.to_bits());
            let collision = if collision {
                cost.per_placement_collision.clone()
            } else {
                ByteEstimate::conservative(0, 0, "Collision disabled")
            };
            let Some(resident_bytes) = cost
                .per_placement_ecs
                .resident_bytes
                .checked_add(collision.resident_bytes)
            else {
                return false;
            };
            let Some(transient_bytes) = cost
                .per_placement_ecs
                .peak_transient_bytes
                .checked_add(collision.peak_transient_bytes)
            else {
                return false;
            };
            let reservation = ResourceReservation {
                key: resource.clone(),
                resident_bytes,
                transient_bytes,
            };
            bundle.push(reservation.clone());
            planned.push((entity, resource, reservation));
        }
        // Reserve the whole pipeline before loading. Otherwise decode could fill
        // the limit and leave no space to instantiate the work needed to drain it.
        if !self
            .ledger
            .reserve(owner.clone(), bundle, self.memory_limit.unwrap_or(0))
        {
            return false;
        }
        for ((path, cost), resource) in cost.resources.iter().zip(&keys) {
            self.add_watch(
                resource.clone(),
                None,
                None,
                (cost.kind == ResourceKind::Texture).then(|| path.clone()),
            );
        }
        for (entity, placement_owner, reservation) in planned {
            // The first transaction already fitted these exact resources. Move
            // each claim to its actual placement without changing charged bytes.
            let accepted = self
                .ledger
                .reserve(placement_owner.clone(), vec![reservation], 0);
            debug_assert!(accepted);
            self.ledger.detach_resource(&owner, &placement_owner);
            self.add_watch(placement_owner.clone(), None, Some(entity), None);
            self.placements.insert(entity, placement_owner);
        }
        self.scenes.insert(
            key.clone(),
            SceneReservation {
                owner,
                resources: keys,
                cost,
                id: None,
                dependencies_cached: false,
                prepared: false,
            },
        );
        true
    }

    pub fn bind_scene(&mut self, key: &SceneKey, id: AssetId<WorldAsset>) {
        if let Some(scene) = self.scenes.get_mut(key) {
            if scene.id.is_some_and(|previous| previous != id) {
                scene.dependencies_cached = false;
                scene.prepared = false;
                for resource in &scene.resources {
                    if let Some(watch) = self.watches.get_mut(resource)
                        && watch.texture_path.is_none()
                    {
                        watch.demand.meshes.clear();
                        watch.demand.images.clear();
                        watch.demand.materials.clear();
                    }
                    self.prepared_resources.remove(resource);
                }
            }
            scene.id = Some(id);
            for resource in &scene.resources {
                if let Some(watch) = self.watches.get_mut(resource)
                    && watch.texture_path.is_none()
                {
                    watch.root_scene = Some(id);
                }
            }
        }
    }

    /// Already-live work must be counted even if it exceeds the admission limit.
    pub fn adopt_scene(&mut self, key: &SceneKey, id: AssetId<WorldAsset>) {
        if !self.scenes.contains_key(key) {
            let limit = self.memory_limit.take();
            let blocked = self.pressure_blocked;
            self.pressure_blocked = false;
            self.reserve_scene(key);
            self.memory_limit = limit;
            self.pressure_blocked = blocked;
        }
        self.bind_scene(key, id);
    }

    pub(super) fn reserve_placement(
        &mut self,
        entity: Entity,
        request: Option<&SceneRequest>,
        collision: bool,
    ) -> bool {
        if !self.enabled || self.placements.contains_key(&entity) {
            return true;
        }
        if self.pressure_blocked {
            return false;
        }
        let scene = request.and_then(|request| {
            self.scenes
                .values()
                .find(|scene| scene.id == request.handle.as_ref().map(Handle::id))
        });
        let ecs = scene.map_or_else(
            || self.catalog.generated.model_placement.clone(),
            |scene| scene.cost.per_placement_ecs.clone(),
        );
        let collision = if collision {
            scene.map_or_else(
                || {
                    ByteEstimate::unknown("Missing placement collision estimate")
                        .with_fallback(UNKNOWN_BYTES)
                },
                |scene| scene.cost.per_placement_collision.clone(),
            )
        } else {
            ByteEstimate::conservative(0, 0, "Collision disabled")
        };
        let owner = format!("placement:{}", entity.to_bits());
        let resource = owner.clone();
        let Some(resident_bytes) = ecs.resident_bytes.checked_add(collision.resident_bytes) else {
            return false;
        };
        let Some(transient_bytes) = ecs
            .peak_transient_bytes
            .checked_add(collision.peak_transient_bytes)
        else {
            return false;
        };
        if !self.ledger.reserve(
            owner.clone(),
            vec![ResourceReservation {
                key: resource.clone(),
                resident_bytes,
                transient_bytes,
            }],
            self.memory_limit.unwrap_or(0),
        ) {
            return false;
        }
        self.add_watch(resource, None, Some(entity), None);
        self.placements.insert(entity, owner);
        true
    }

    pub fn reserve_scene_placement(
        &mut self,
        key: &SceneKey,
        entity: Entity,
        collision: bool,
    ) -> bool {
        if !self.enabled || self.placements.contains_key(&entity) {
            return true;
        }
        if self.pressure_blocked {
            return false;
        }
        let Some(scene) = self.scenes.get(key) else {
            return false;
        };
        let ecs = &scene.cost.per_placement_ecs;
        let collision = if collision {
            scene.cost.per_placement_collision.clone()
        } else {
            ByteEstimate::conservative(0, 0, "Collision disabled")
        };
        let Some(resident_bytes) = ecs.resident_bytes.checked_add(collision.resident_bytes) else {
            return false;
        };
        let Some(transient_bytes) = ecs
            .peak_transient_bytes
            .checked_add(collision.peak_transient_bytes)
        else {
            return false;
        };
        let owner = format!("placement:{}", entity.to_bits());
        if !self.ledger.reserve(
            owner.clone(),
            vec![ResourceReservation {
                key: owner.clone(),
                resident_bytes,
                transient_bytes,
            }],
            self.memory_limit.unwrap_or(0),
        ) {
            return false;
        }
        self.add_watch(owner.clone(), None, Some(entity), None);
        self.placements.insert(entity, owner);
        true
    }

    /// Reserve before queueing; repeating the same attempt is idempotent.
    pub fn reserve_cell_attempt(&mut self, key: CellKey, generation: u64, physics: bool) -> bool {
        self.reserve_cell_attempt_with_textures(key, generation, physics, Vec::new())
    }

    /// Atomically admit any new generated allocation and its discovered textures.
    /// A deferred existing attempt keeps its funded claims; denial creates no new owner.
    pub fn reserve_cell_attempt_with_textures(
        &mut self,
        key: CellKey,
        generation: u64,
        physics: bool,
        paths: Vec<String>,
    ) -> bool {
        if !self.enabled {
            return true;
        }
        let attempt = CellAttempt { key, generation };
        let mut bundle = Vec::new();
        let new_cell = if self.pending_cells.contains_key(&attempt) {
            None
        } else {
            if self.pressure_blocked {
                return false;
            }
            self.next_resource = self.next_resource.saturating_add(1);
            let owner = format!("cell:{key:?}:{}", self.next_resource);
            let mut estimates = vec![
                self.catalog
                    .generated
                    .full_cell_terrain
                    .with_fallback(UNKNOWN_BYTES),
                self.catalog
                    .generated
                    .full_cell_water
                    .with_fallback(UNKNOWN_BYTES),
            ];
            if physics {
                estimates.push(
                    self.catalog
                        .generated
                        .full_cell_collision
                        .with_fallback(UNKNOWN_BYTES),
                );
            }
            let Some(resident_bytes) = estimates
                .iter()
                .try_fold(MIB, |sum, cost| sum.checked_add(cost.resident_bytes))
            else {
                return false;
            };
            let Some(transient_bytes) = estimates
                .iter()
                .try_fold(MIB, |sum, cost| sum.checked_add(cost.peak_transient_bytes))
            else {
                return false;
            };
            bundle.push(ResourceReservation {
                key: owner.clone(),
                resident_bytes,
                transient_bytes,
            });
            Some(CellReservation {
                resource: owner.clone(),
                owner,
                root: None,
                dependencies_cached: false,
            })
        };
        let cell = new_cell
            .as_ref()
            .unwrap_or_else(|| &self.pending_cells[&attempt]);
        let owner = cell.owner.clone();
        for path in &paths {
            let cost = self
                .catalog
                .resources
                .get(path)
                .map(|cost| cost.estimate())
                .unwrap_or_else(|| ByteEstimate::unknown("Generated texture metadata absent"))
                .with_fallback(UNKNOWN_BYTES);
            let resource = self.resource_key(path, None);
            if self.pressure_blocked
                && !self
                    .ledger
                    .owner_resources(&owner)
                    .is_some_and(|resources| resources.contains(&resource))
            {
                return false;
            }
            if self.ledger.contains_resource(&resource)
                && !self.ledger.resource_has_owners(&resource)
            {
                return false;
            }
            bundle.push(ResourceReservation {
                key: resource,
                resident_bytes: cost.resident_bytes,
                transient_bytes: cost.peak_transient_bytes,
            });
        }
        // One ledger transaction prevents partially admitted fresh collect attempts.
        if !self
            .ledger
            .reserve(owner, bundle, self.memory_limit.unwrap_or(0))
        {
            return false;
        }
        if let Some(cell) = new_cell {
            self.add_watch(cell.resource.clone(), None, None, None);
            self.pending_cells.insert(attempt, cell);
        }
        for path in paths {
            self.add_watch(self.resource_key(&path, None), None, None, Some(path));
        }
        true
    }

    /// Move only this attempt into root ownership. Retained roots keep their own watches.
    pub fn bind_cell_attempt(&mut self, key: CellKey, generation: u64, root: Entity) {
        if let Some(mut cell) = self.pending_cells.remove(&CellAttempt { key, generation }) {
            cell.root = Some(root);
            if let Some(watch) = self.watches.get_mut(&cell.resource) {
                watch.root_entity = Some(root);
            }
            // A bind consumes one pending allocation; revival never calls this API.
            debug_assert!(!self.cells.contains_key(&root));
            self.cells.insert(root, cell);
        }
    }

    /// A consumed/refused attempt owns no root. Never abandon a retained root here.
    pub fn retire_cell_attempt(&mut self, key: CellKey, generation: u64) {
        if let Some(cell) = self.pending_cells.remove(&CellAttempt { key, generation }) {
            self.abandon(&cell.owner);
        }
    }

    // Compatibility helpers for the single-attempt ledger fixtures only.
    #[cfg(test)]
    fn reserve_cell(&mut self, key: CellKey, physics: bool) -> bool {
        self.reserve_cell_attempt(key, 0, physics)
    }

    #[cfg(test)]
    fn reserve_cell_textures(&mut self, key: CellKey, paths: Vec<String>) -> bool {
        self.reserve_cell_attempt_with_textures(key, 0, false, paths)
    }

    #[cfg(test)]
    fn bind_cell(&mut self, key: CellKey, root: Entity) {
        self.bind_cell_attempt(key, 0, root);
    }

    fn abandon(&mut self, owner: &str) {
        for resource in self.ledger.abandon(owner) {
            if let Some(watch) = self.watches.get_mut(&resource) {
                watch.orphaned_frame = Some(self.frame);
                if let Some(images) = &watch.allocation_images {
                    watch.demand.images.clone_from(images);
                }
            }
            self.prepared_resources.remove(&resource);
        }
    }

    pub fn cell_requests_allowed(&self) -> bool {
        self.decision
            .is_none_or(|decision| decision.allow_cell_requests)
    }
}

fn diagnostic_bytes(
    world: &World,
    path: &bevy::diagnostic::DiagnosticPath,
    multiplier: f64,
) -> Option<u64> {
    let measurement = world
        .get_resource::<DiagnosticsStore>()?
        .get(path)?
        .measurement()?;
    (measurement.time.elapsed().as_secs_f64() <= 2.0
        && measurement.value.is_finite()
        && measurement.value >= 0.0)
        .then_some((measurement.value * multiplier) as u64)
}

pub(super) fn update_streaming_control(world: &mut World) {
    let Some(mut runtime) = world.remove_resource::<StreamingRuntime>() else {
        return;
    };
    if !runtime.enabled {
        world.insert_resource(runtime);
        return;
    }
    runtime.frame = runtime.frame.saturating_add(1);
    let config = world.resource::<EngineConfig>().clone();
    let view = requests::priority_view(world);
    let (moving, turning, discontinuity) = match (view, runtime.previous_camera) {
        (Some(view), Some((position, forward, space))) => {
            let distance = view.position.distance(position);
            (
                distance > 0.05,
                view.forward.dot(forward) < 0.99999,
                space != config.worldspace_id || distance > f64::from(CELL_SIZE) * 2.0,
            )
        }
        _ => (false, false, false),
    };
    runtime.previous_camera = view.map(|view| (view.position, view.forward, config.worldspace_id));
    let process = diagnostic_bytes(
        world,
        &SystemInformationDiagnosticsPlugin::PROCESS_MEM_USAGE,
        1024.0 * 1024.0 * 1024.0,
    )
    // sysinfo can report zero when the process observation is unavailable.
    // A running game cannot have zero RSS; do not present it as a fresh sample.
    .filter(|rss| *rss != 0);
    let used_percent = diagnostic_bytes(
        world,
        &SystemInformationDiagnosticsPlugin::SYSTEM_MEM_USAGE,
        1000.0,
    )
    .map(|value| value as f64 / 1000.0);
    let physical = physical_memory_bytes(world);
    let free = physical
        .zip(used_percent)
        .map(|(total, used)| (total as f64 * (1.0 - used / 100.0).clamp(0.0, 1.0)) as u64);
    runtime.pressure_blocked = runtime.memory_limit.is_some()
        && (physical.is_some_and(|total| total <= config.streaming_headroom_bytes())
            || free.is_some_and(|free| free < config.streaming_headroom_bytes())
            || process.zip(physical).is_some_and(|(rss, total)| {
                rss > total.saturating_sub(config.streaming_headroom_bytes())
            }));
    let mut models = world.query::<(&PendingModel, Option<&SceneRequest>)>();
    let server = world.resource::<AssetServer>();
    let ready = models
        .iter(world)
        .filter(|(model, _)| server.is_loaded_with_dependencies(model.handle.id()))
        .count();
    let mandatory = models
        .iter(world)
        .any(|(_, request)| request.is_some_and(|request| request.collision_candidate));
    let mut queued_requests = world.query::<&SceneRequest>();
    let mandatory = mandatory
        || queued_requests.iter(world).any(|request| {
            request.handle.is_none()
                && request.collision_candidate
                && requests::relevant(request, world.resource::<StreamingWorld>())
        });
    let mut pending =
        world.query_filtered::<(), (With<PendingAssetProfile>, With<WorldAssetRoot>)>();
    let spawned = pending.iter(world).count();
    let bridge = world.resource::<StreamingPreparationBridge>();
    let gpu = bridge
        .latest()
        .filter(|snapshot| runtime.frame.saturating_sub(snapshot.source_main_frame) <= 8);
    let backlog = DownstreamBacklog {
        ready_placements: ready,
        spawned_instances: spawned,
        collider_jobs: if config.interactive_world_physics() {
            spawned
        } else {
            0
        },
        gpu_assets: gpu
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .pending_meshes
                    .zip(snapshot.pending_images)
                    .zip(snapshot.pending_materials)
                    .map(|((meshes, images), materials)| meshes + images + materials)
            })
            .unwrap_or(0),
        // Reserved decode bytes are not GPU backlog bytes; only IDs are observed here.
        gpu_bytes: 0,
        cell_responses: runtime.deferred_responses.len().saturating_add(
            world
                .get_resource::<WorldDatabase>()
                .map_or(0, WorldDatabase::pending_cell_responses),
        ),
    };
    let admission = world.resource::<SceneAdmission>();
    let metrics = world.resource::<StreamingMetrics>();
    let settled = admission.active_jobs() == 0
        && admission.queued_jobs() == 0
        && metrics.pending_asset_instances == 0
        && metrics.pending_surface_instances == 0
        && metrics.active_requests == 0
        && metrics.pending_lod_queries == 0
        && metrics.pending_lod_chunks == 0
        && runtime.deferred_responses.is_empty()
        && world
            .resource::<StreamingWorld>()
            .outstanding_cells
            .is_empty()
        && backlog.cell_responses == 0;
    let time = world
        .get_resource::<Time<Real>>()
        .map_or(0.0, Time::delta_secs_f64);
    let hard_limits = configured_budgets(&config);
    let decision = runtime.controller.tick(
        &runtime.settings,
        ControllerInput {
            delta_seconds: time,
            frame_ms: (time > 0.0).then_some(time * 1000.0),
            gpu_frame_ms: None,
            // Validation is already inside the commit window; adding it again would make a
            // healthy readiness scan look like a streaming spike.
            streaming_work_ms: runtime.previous_commit_ms + runtime.previous_spawn_ms,
            camera_moving: moving,
            camera_turning: turning,
            camera_discontinuity: discontinuity,
            useful_nearby_ready: settled
                || (metrics.terrain_patches_validated >= 4
                    && ready < 64
                    && !runtime.scenes.is_empty()
                    && runtime
                        .scenes
                        .values()
                        .filter(|scene| scene.prepared)
                        .count()
                        * 4
                        >= runtime
                            .scenes
                            .len()
                            .saturating_add(admission.queued_jobs())
                            .saturating_mul(3)),
            demand_settled: settled,
            backlog,
            active_scene_jobs: admission.active_jobs(),
            hard_limits,
            memory_blocked: runtime.pressure_blocked,
            mandatory_collision_pending: mandatory,
        },
    );
    if let Some(mut upload) = world.get_resource_mut::<RenderAssetBytesPerFrame>() {
        upload.max_bytes = Some(decision.budgets.max_upload_bytes_per_frame as usize);
    }
    let totals = runtime.ledger.totals();
    let denied = totals
        .denied_total
        .saturating_sub(runtime.previous_denied_total);
    runtime.previous_denied_total = totals.denied_total;
    runtime.snapshot = Some(RuntimeSnapshot {
        frame: runtime.frame,
        control: decision,
        backlog,
        resident_reserved_bytes: totals.resident_bytes,
        transient_reserved_bytes: totals.transient_bytes,
        orphan_reserved_bytes: totals.orphan_bytes().unwrap_or(u64::MAX),
        memory_limit_bytes: runtime.memory_limit,
        memory_denied_total: totals.denied_total,
        memory_denied_since_last_update: denied,
        memory_blocked: runtime.pressure_blocked || denied != 0,
        physical_memory_blocked: runtime.pressure_blocked,
        unobserved_scene_dependencies: runtime
            .watches
            .values()
            .flat_map(|watch| watch.unobserved_scenes.iter().copied())
            .collect::<HashSet<_>>()
            .len(),
        unknown_scene_estimates: runtime
            .scenes
            .values()
            .filter(|scene| scene.cost.used_unknown_fallback)
            .count(),
        catalog_loaded: runtime.catalog_loaded,
        process_rss_bytes: process,
        system_free_estimate_bytes: free,
        pressure_sample_fresh: process.is_some() && free.is_some(),
        gpu_observation_available: gpu
            .as_ref()
            .is_some_and(|snapshot| snapshot.observed_available),
    });
    runtime.decision = Some(decision);
    runtime.previous_commit_ms = 0.0;
    runtime.previous_validation_ms = 0.0;
    let mut profiler = world.resource_mut::<ProfilingState>();
    profiler.set_gauge(
        "streaming/resident_reserved_bytes",
        totals.resident_bytes as f64,
    );
    profiler.set_gauge(
        "streaming/transient_reserved_bytes",
        totals.transient_bytes as f64,
    );
    profiler.set_gauge("streaming/memory_denied_total", totals.denied_total as f64);
    world.insert_resource(runtime);
}

fn cpu_absent(world: &World, watch: &ResourceWatch) -> bool {
    if !watch.unobserved_scenes.is_empty() {
        return false;
    }
    let server = world.resource::<AssetServer>();
    if watch
        .root_entity
        .is_some_and(|entity| world.get_entity(entity).is_ok())
    {
        return false;
    }
    if watch.root_scene.is_some_and(|id| {
        world.resource::<Assets<WorldAsset>>().contains(id)
            || server
                .get_load_state(id)
                .is_some_and(|state| !matches!(state, LoadState::NotLoaded))
    }) {
        return false;
    }
    if watch
        .demand
        .meshes
        .iter()
        .any(|id| world.resource::<Assets<Mesh>>().contains(*id))
        || watch.demand.images.iter().any(|id| {
            world.resource::<Assets<Image>>().contains(*id)
                || server
                    .get_load_state(*id)
                    .is_some_and(|state| matches!(state, LoadState::Loading))
        })
    {
        return false;
    }
    !watch.demand.materials.iter().any(|id| {
        if id.type_id() == TypeId::of::<StandardMaterial>() {
            world
                .resource::<Assets<StandardMaterial>>()
                .contains(id.typed_debug_checked::<StandardMaterial>())
        } else if id.type_id() == TypeId::of::<TerrainMaterial>() {
            world
                .resource::<Assets<TerrainMaterial>>()
                .contains(id.typed_debug_checked::<TerrainMaterial>())
        } else if id.type_id() == TypeId::of::<WaterMaterial>() {
            world
                .resource::<Assets<WaterMaterial>>()
                .contains(id.typed_debug_checked::<WaterMaterial>())
        } else if id.type_id() == TypeId::of::<NifDepthMaterial>() {
            world
                .get_resource::<Assets<NifDepthMaterial>>()
                .is_none_or(|assets| assets.contains(id.typed_debug_checked::<NifDepthMaterial>()))
        } else if id.type_id() == TypeId::of::<SkyrimSkinMaterial>() {
            world
                .get_resource::<Assets<SkyrimSkinMaterial>>()
                .is_none_or(|assets| {
                    assets.contains(id.typed_debug_checked::<SkyrimSkinMaterial>())
                })
        } else if id.type_id() == TypeId::of::<SkyrimSkinDepthMaterial>() {
            world
                .get_resource::<Assets<SkyrimSkinDepthMaterial>>()
                .is_none_or(|assets| {
                    assets.contains(id.typed_debug_checked::<SkyrimSkinDepthMaterial>())
                })
        } else {
            true
        }
    })
}

fn watch_prepared(
    world: &World,
    bridge: &StreamingPreparationBridge,
    snapshot: Option<&crate::streaming_preparation::PreparationSnapshot>,
    watch: &ResourceWatch,
) -> bool {
    // An undiscovered image list is unknown, even if the empty GPU list was ready.
    if watch.texture_path.is_some() && watch.demand.images.is_empty() {
        return false;
    }
    if !bridge.has_render_world() {
        return true;
    }
    let Some(snapshot) = snapshot else {
        return false;
    };
    if snapshot.readiness_for(&watch.demand) == Some(true) {
        return true;
    }
    // A failed optional normal has completed its loader work and owns no GPU
    // image. It stays resident-charged until its live owner releases the handle.
    watch.texture_path.is_some()
        && watch.demand.images.iter().all(|id| {
            matches!(
                world.resource::<AssetServer>().get_load_state(*id),
                Some(LoadState::Failed(_))
            )
        })
        && snapshot
            .presence_for(&watch.demand)
            .is_some_and(|presence| presence.all_absent())
}

fn surface_validated(world: &World, root: Entity) -> bool {
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        let Ok(entity) = world.get_entity(entity) else {
            return false;
        };
        if entity.contains::<super::PendingTerrainProfile>()
            || entity.contains::<super::PendingWaterProfile>()
        {
            return false;
        }
        if let Some(children) = entity.get::<Children>() {
            stack.extend(children.iter());
        }
    }
    true
}

fn colliders_installed(world: &World, root: Entity) -> bool {
    use bevy_rapier3d::prelude::{Collider, ColliderDisabled, RapierColliderHandle};
    if !world.resource::<EngineConfig>().interactive_world_physics() {
        return true;
    }
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        let Ok(entity) = world.get_entity(entity) else {
            return false;
        };
        if entity.contains::<Collider>()
            && !entity.contains::<ColliderDisabled>()
            && !entity.contains::<RapierColliderHandle>()
        {
            return false;
        }
        if let Some(children) = entity.get::<Children>() {
            stack.extend(children.iter());
        }
    }
    true
}

pub(super) fn observe_streaming_ownership(world: &mut World) {
    let Some(mut runtime) = world.remove_resource::<StreamingRuntime>() else {
        return;
    };
    if !runtime.enabled {
        world.insert_resource(runtime);
        return;
    }
    let jobs: BTreeMap<_, _> = world
        .resource::<SceneAdmission>()
        .tracked_jobs()
        .into_iter()
        .collect();
    let mut profiles = world.query::<(Entity, &SceneRequestKey, &SceneRequest)>();
    let pending_keys: HashSet<_> = profiles
        .iter(world)
        .filter(|(entity, _, request)| {
            request.handle.is_some()
                && (world.get::<PendingAssetProfile>(*entity).is_some()
                    || super::lod::scene_pending(world, *entity))
        })
        .map(|(_, key, _)| key.0.clone())
        .collect();
    let bridge = world.resource::<StreamingPreparationBridge>().clone();
    let gpu = bridge
        .latest()
        .filter(|snapshot| runtime.frame.saturating_sub(snapshot.source_main_frame) <= 8);
    let mut orphan_owners = vec![];
    for (key, scene) in &mut runtime.scenes {
        if !jobs.contains_key(key) {
            orphan_owners.push(scene.owner.clone());
            continue;
        }
        if let Some(id) = scene.id
            && !scene.dependencies_cached
            && let Some(asset) = world.resource::<Assets<WorldAsset>>().get(id)
        {
            let unavailable = !scene_dependencies_available(
                asset,
                world.resource::<Assets<StandardMaterial>>(),
                world.get_resource::<Assets<NifDepthMaterial>>(),
                world.get_resource::<Assets<SkyrimSkinMaterial>>(),
                world.get_resource::<Assets<SkyrimSkinDepthMaterial>>(),
            );
            for resource in &scene.resources {
                // Shared textures can have several source scenes. Discovering
                // one scene must not clear another scene's unknown ownership.
                let watch = runtime.watches.get_mut(resource).unwrap();
                if unavailable {
                    watch.unobserved_scenes.insert(id);
                } else {
                    watch.unobserved_scenes.remove(&id);
                }
            }
        }
        if let Some(id) = scene.id
            && !scene.dependencies_cached
            && let Some(asset) = world.resource::<Assets<WorldAsset>>().get(id)
            && world
                .resource::<AssetServer>()
                .is_loaded_with_dependencies(id)
            && scene_dependencies_available(
                asset,
                world.resource::<Assets<StandardMaterial>>(),
                world.get_resource::<Assets<NifDepthMaterial>>(),
                world.get_resource::<Assets<SkyrimSkinMaterial>>(),
                world.get_resource::<Assets<SkyrimSkinDepthMaterial>>(),
            )
        {
            let demand = scene_demand(
                PreparationKey::Scene(id),
                asset,
                world.resource::<Assets<StandardMaterial>>(),
                world.get_resource::<Assets<NifDepthMaterial>>(),
                world.get_resource::<Assets<SkyrimSkinMaterial>>(),
                world.get_resource::<Assets<SkyrimSkinDepthMaterial>>(),
            );
            // Every material dependency must have an exact loader identity
            // before catalog entries can be proved unused. Embedded images
            // stay with scene geometry; unknown identities retain all claims.
            let dependency_paths = (|| {
                let mut paths = HashMap::<String, Vec<AssetId<Image>>>::new();
                for image in &demand.images {
                    let path = world.resource::<AssetServer>().get_path(*image)?;
                    if path.label().is_some() {
                        continue;
                    }
                    let path = canonical_resource_key(&path.path().to_string_lossy()).ok()?;
                    paths.entry(path).or_default().push(*image);
                }
                Some(paths)
            })();
            scene.resources.retain(|resource| {
                let watch = runtime.watches.get_mut(resource).unwrap();
                if let Some(path) = &watch.texture_path {
                    if let Some(paths) = &dependency_paths {
                        let Some(images) = paths.get(path) else {
                            runtime.ledger.detach_resource(&scene.owner, resource);
                            if !runtime.ledger.resource_has_owners(resource) {
                                watch.orphaned_frame = Some(runtime.frame);
                                runtime.prepared_resources.remove(resource);
                            }
                            // Detaching a claim leaves last-owner bytes charged
                            // until CPU and render disposal are both observed.
                            return false;
                        };
                        watch.demand.images.extend(images);
                        return true;
                    }
                    watch.demand.images.extend(
                        demand
                            .images
                            .iter()
                            .filter(|image| {
                                world
                                    .resource::<AssetServer>()
                                    .get_path(**image)
                                    .is_some_and(|asset_path| {
                                        asset_path.path().to_string_lossy() == *path
                                    })
                            })
                            .copied(),
                    );
                } else {
                    watch.demand.meshes.extend(&demand.meshes);
                    watch.demand.materials.extend(&demand.materials);
                    // Embedded/unknown image allocations belong to scene geometry.
                    watch.demand.images.extend(
                        demand
                            .images
                            .iter()
                            .filter(|image| {
                                scene.cost.used_unknown_fallback
                                    || world
                                        .resource::<AssetServer>()
                                        .get_path(**image)
                                        .is_none_or(|path| path.label().is_some())
                            })
                            .copied(),
                    );
                }
                true
            });
            scene.dependencies_cached = true;
        }
        if !scene.prepared && scene.dependencies_cached && !pending_keys.contains(key) {
            let prepared = scene.resources.iter().all(|resource| {
                runtime.prepared_resources.contains(resource)
                    || watch_prepared(world, &bridge, gpu.as_ref(), &runtime.watches[resource])
            });
            if prepared {
                runtime.ledger.mark_prepared(&scene.owner);
                runtime
                    .prepared_resources
                    .extend(scene.resources.iter().cloned());
                scene.prepared = true;
            }
        }
    }
    if !orphan_owners.is_empty() {
        let mut meshes = world.query::<&crate::world::components::MeshHandle>();
        let live_paths: HashSet<_> = meshes.iter(world).map(|mesh| mesh.0.clone()).collect();
        world
            .resource_mut::<super::StaticCollisionCache>()
            .0
            .retain(|(_, path), _| live_paths.contains(path));
    }
    runtime.scenes.retain(|key, _| jobs.contains_key(key));
    for owner in orphan_owners {
        runtime.abandon(&owner);
    }
    // Pending payloads retain their own admission until consumed, including stale responses.
    let finished_attempts: Vec<_> = runtime
        .pending_cells
        .keys()
        .filter(|attempt| {
            let streaming = world.resource::<StreamingWorld>();
            streaming.outstanding_cells.get(&attempt.key) != Some(&attempt.generation)
                && !matches!(streaming.cells.get(&attempt.key),
                Some(CellStatus::Loading { generation, .. })
                if *generation == attempt.generation)
        })
        .copied()
        .collect();
    for attempt in finished_attempts {
        runtime.retire_cell_attempt(attempt.key, attempt.generation);
    }
    // Logical removal or failure is not physical disposal. Keep each allocation owned until
    // its exact generational root is absent, independently of the current CellKey.
    let absent_roots: Vec<_> = runtime
        .cells
        .keys()
        .filter(|root| world.get_entity(**root).is_err())
        .copied()
        .collect();
    for root in absent_roots {
        if let Some(cell) = runtime.cells.remove(&root) {
            runtime.abandon(&cell.owner);
        }
    }
    for cell in runtime.cells.values_mut() {
        if runtime.prepared_resources.contains(&cell.resource) {
            continue;
        }
        if let Some(root) = cell.root {
            let validated = surface_validated(world, root);
            if !cell.dependencies_cached && world.get_entity(root).is_ok() {
                let demand = super::surface_costs::collect_surface_demand(world, root);
                // Match the actual owner's ledger claims, not just the catalog
                // or another live cell's watch. Canonical paths can have several
                // loader IDs (e.g. case aliases); retain each exact dependency ID
                // on the separate watch that owns its shared texture charge.
                let texture_claims: HashMap<_, _> = runtime
                    .ledger
                    .owner_resources(&cell.owner)
                    .into_iter()
                    .flatten()
                    .filter_map(|resource| {
                        let watch = runtime.watches.get(resource)?;
                        let path = canonical_resource_key(watch.texture_path.as_ref()?).ok()?;
                        Some((path, resource.clone()))
                    })
                    .collect();
                let mut allocation_images = Vec::new();
                let server = world.resource::<AssetServer>();
                for image in &demand.images {
                    let claimed_resource = server.get_path(*image).and_then(|path| {
                        // A subasset or another asset source is not the pack's
                        // standalone catalog image, even if its path matches.
                        if path.label().is_some()
                            || !matches!(path.source(), bevy::asset::io::AssetSourceId::Default)
                        {
                            return None;
                        }
                        let path = canonical_resource_key(&path.path().to_string_lossy()).ok()?;
                        texture_claims.get(&path)
                    });
                    if let Some(resource) = claimed_resource {
                        let watch = runtime.watches.get_mut(resource).unwrap();
                        if !watch.demand.images.contains(image) {
                            watch.demand.images.push(*image);
                            runtime.prepared_resources.remove(resource);
                        }
                    } else {
                        allocation_images.push(*image);
                    }
                }
                let watch = runtime.watches.get_mut(&cell.resource).unwrap();
                watch.demand.meshes = demand.meshes;
                watch.demand.images = demand.images;
                watch.demand.materials = demand.materials;
                watch.allocation_images = Some(allocation_images);
                // Pending validation may still change the actual material bindings (for
                // example the supported optional-normal failure writer). Freeze only
                // after those owners have resolved; keep exact live and disposal IDs.
                cell.dependencies_cached = validated;
            }
            let resources: Vec<_> = runtime
                .ledger
                .owner_resources(&cell.owner)
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            let prepared = resources.iter().all(|resource| {
                runtime.prepared_resources.contains(resource)
                    || watch_prepared(world, &bridge, gpu.as_ref(), &runtime.watches[resource])
            });
            if cell.dependencies_cached
                && !runtime.prepared_resources.contains(&cell.resource)
                && validated
                && colliders_installed(world, root)
                && prepared
            {
                runtime.ledger.mark_prepared(&cell.owner);
                runtime.prepared_resources.extend(resources);
            }
        }
    }
    let absent_placements: Vec<_> = runtime
        .placements
        .keys()
        .filter(|entity| world.get_entity(**entity).is_err())
        .copied()
        .collect();
    for entity in absent_placements {
        let owner = runtime.placements.remove(&entity).unwrap();
        runtime.abandon(&owner);
    }
    for (entity, owner) in &runtime.placements {
        if !runtime.prepared_resources.contains(owner)
            && world.get::<PendingAssetProfile>(*entity).is_none()
            && !super::lod::scene_pending(world, *entity)
            && colliders_installed(world, *entity)
        {
            runtime.ledger.mark_prepared(owner);
            runtime.prepared_resources.insert(owner.clone());
        }
    }
    // Discover image loader IDs without taking a strong handle or triggering I/O.
    for (resource, watch) in &mut runtime.watches {
        if runtime.prepared_resources.contains(resource) && watch.orphaned_frame.is_none() {
            continue;
        }
        if let Some(path) = &watch.texture_path {
            let mut current: Vec<_> = world
                .resource::<AssetServer>()
                .get_path_ids(path.clone())
                .into_iter()
                .filter(|id| id.type_id() == TypeId::of::<Image>())
                .map(|id| id.typed_debug_checked::<Image>())
                .collect();
            current.sort_unstable();
            current.dedup();
            if !current.is_empty() {
                if watch.orphaned_frame.is_some() {
                    // A detached catalog image may still finish loading. Keep
                    // every generation until its exact GPU IDs disappear.
                    watch.demand.images.extend(current);
                } else if current != watch.demand.images {
                    watch.demand.images = current;
                    runtime.prepared_resources.remove(resource);
                }
            }
        }
        watch.demand.meshes.sort_unstable();
        watch.demand.meshes.dedup();
        watch.demand.images.sort_unstable();
        watch.demand.images.dedup();
        watch.demand.materials.sort_unstable();
        watch.demand.materials.dedup();
    }
    let reclaim: Vec<_> = runtime
        .ledger
        .orphan_keys()
        .filter_map(|resource| {
            let watch = runtime.watches.get(resource)?;
            let frame = watch.orphaned_frame?;
            if runtime.frame.saturating_sub(frame) < 2 || !cpu_absent(world, watch) {
                return None;
            }
            let absent = !bridge.has_render_world()
                || gpu
                    .as_ref()
                    .and_then(|snapshot| snapshot.presence_for(&watch.demand))
                    .is_some_and(|presence| presence.all_absent());
            absent.then(|| resource.to_owned())
        })
        .collect();
    for resource in reclaim {
        if runtime.ledger.confirm_absent(&resource) {
            runtime.watches.remove(&resource);
            runtime.prepared_resources.remove(&resource);
        }
    }
    let demands = runtime
        .watches
        .iter()
        .filter(|(resource, watch)| {
            watch.orphaned_frame.is_some() || !runtime.prepared_resources.contains(*resource)
        })
        .map(|(_, watch)| watch.demand.clone())
        .collect();
    bridge.replace_demands(runtime.frame, demands);
    world.insert_resource(runtime);
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
