//! CPU grass streaming observes committed cells after streaming has finished.
pub mod generation;

use crate::{
    config::EngineConfig,
    grass_render::GrassRenderPlugin,
    profiling::ProfilingState,
    streaming::RenderOrigin,
    world::{
        cache::TerrainSnapshot,
        components::{CELL_SIZE, CellRef, ExteriorCellGrid, StreamedCellRoot, StreamingCamera},
        grass_cache::GrassCache,
    },
};
use bevy::{
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, block_on, poll_once},
};
use generation::GeneratedGrass;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

/// Immutable surface actually committed by terrain streaming, after edge welding.
#[derive(Component, Clone)]
pub(crate) struct GrassTerrain(Arc<GrassSurface>);

struct GrassSurface {
    /// One 33x33 height plane, followed by an optional 33x33 VCLR luminance plane.
    /// The retained colour plane participates in surface equality and pool accounting.
    samples: Box<[f32]>,
    water_height: Option<f32>,
}

impl GrassTerrain {
    /// Retain the committed field once, without copying it during per-frame grass streaming.
    pub(crate) fn from_snapshot(terrain: &TerrainSnapshot) -> Self {
        // Streaming validates complete terrain before publishing this component.
        // Keep malformed programmatic fields unusable rather than interpreting a second
        // height plane as colour. Empty/malformed colours use the white fallback.
        if terrain.heights.len() != 33 * 33 {
            return Self::new(Vec::new(), terrain.water_height);
        }
        let mut samples = terrain.heights.clone();
        if terrain.vertex_colors.len() == 33 * 33 * 3 {
            samples.extend(terrain.vertex_colors.as_chunks::<3>().0.iter().map(|rgb| {
                0.31 * (f32::from(rgb[0]) / 255.0)
                    + 0.37 * (f32::from(rgb[1]) / 255.0)
                    + 0.32 * (f32::from(rgb[2]) / 255.0)
            }));
        }
        Self::new(samples, terrain.water_height)
    }

    /// Build a worker-owned speculative surface before terrain has committed.
    fn from_cache(cell: &shared::grass::ArchivedGrassCell) -> Self {
        Self::new(
            cell.heights
                .iter()
                .map(|height| f32::from(*height))
                .collect(),
            cell.water_height.as_ref().map(|height| f32::from(*height)),
        )
    }

    /// Own the height and optional luminance planes in one accounted allocation.
    fn new(samples: Vec<f32>, water_height: Option<f32>) -> Self {
        Self(Arc::new(GrassSurface {
            samples: samples.into_boxed_slice(),
            water_height,
        }))
    }

    /// Expose only the height plane to placement, including the original invalid-field check.
    fn heights(&self) -> &[f32] {
        if self.0.samples.len() == 2 * 33 * 33 {
            &self.0.samples[..33 * 33]
        } else {
            &self.0.samples
        }
    }

    /// Missing speculative or committed colours have the same white fallback as terrain.
    fn luminance(&self) -> Option<&[f32]> {
        (self.0.samples.len() == 2 * 33 * 33).then(|| &self.0.samples[33 * 33..])
    }

    /// Use pointer equality normally, and compare exact field bits once on a new root/source.
    fn same_surface(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.water_height.map(f32::to_bits) == other.0.water_height.map(f32::to_bits)
                && self.0.samples.len() == other.0.samples.len()
                && self
                    .0
                    .samples
                    .iter()
                    .zip(other.0.samples.iter())
                    .all(|(left, right)| left.to_bits() == right.to_bits()))
    }

    /// Conservatively charge the full retained source even when its root shares the allocation.
    fn bytes(&self) -> usize {
        std::mem::size_of_val(self.0.samples.as_ref())
            + std::mem::size_of::<GrassSurface>()
            + 2 * std::mem::size_of::<usize>()
    }
}

impl PartialEq for GrassTerrain {
    /// Surface content, independent of the lifetime of its committed root.
    fn eq(&self, other: &Self) -> bool {
        self.same_surface(other)
    }
}

impl Eq for GrassTerrain {}

/// Packed worker output; the renderer uploads these exact bytes once.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GrassInstance {
    /// Quantized Creation-local XYZ; height uses the batch's minimum/span.
    pub position: [u16; 3],
    pub rotation: u8,
    /// Scale uses the range 0..2.
    pub scale: u8,
    /// Darken-only brightness uses the unit range (byte / 255).
    pub color: u8,
    /// Creation-space authored signed direction bytes; the shader normalizes them.
    /// Missing-normal geometry fallback uses a unit normal multiplied by 127.
    pub normal: [i8; 3],
    /// Spatial fade rank retained in low 18 bits for deterministic prefix thinning.
    /// Tuning-only high bits retain 7-bit LAND luminance and signed range factor.
    pub slot: u32,
}

impl GrassInstance {
    /// Decode the stable spatial fade rank independently of optional tuning metadata.
    pub const fn candidate_slot(&self) -> u32 {
        self.slot & 0x3ffff
    }
}

/// Production draw-prefix count, shared with shot evidence; shader root clipping happens later.
pub(crate) fn submitted_instance_count(
    instances: &[GrassInstance],
    slots: u32,
    eye_in_cell: Vec3,
    fade_start: f32,
    fade_end: f32,
    enabled: bool,
) -> usize {
    if !enabled {
        return instances.len();
    }
    let closest = Vec3::new(
        eye_in_cell.x.clamp(0.0, 4096.0),
        eye_in_cell.y,
        eye_in_cell.z.clamp(-4096.0, 0.0),
    );
    let distance = eye_in_cell.distance(closest);
    let fraction = ((fade_end - distance) / (fade_end - fade_start).max(1.0)).clamp(0.0, 1.0);
    let limit = (fraction * slots as f32).ceil() as u32;
    instances.partition_point(|instance| instance.candidate_slot() < limit)
}

/// One species in one cell. The renderer owns primitive children, never blade entities.
#[derive(Component, Clone)]
pub struct GrassBatch {
    pub cell_id: u32,
    pub grid: [i32; 2],
    pub grass_id: u32,
    pub model_path: String,
    pub instances: Arc<Vec<GrassInstance>>,
    pub height_min: f32,
    pub height_span: f32,
    pub slot_count: u32,
    pub wave_period: f32,
    pub flags: u8,
}

#[derive(Resource, Clone, Copy)]
pub struct GrassFade {
    pub enabled: bool,
    pub start: f32,
    pub end: f32,
}

/// Renderer lifecycle systems run after worker results have been admitted.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GrassStreaming;

pub struct GrassPlugin;

impl Plugin for GrassPlugin {
    /// Install bounded grass generation and streaming systems.
    fn build(&self, app: &mut App) {
        let config = app.world().resource::<EngineConfig>();
        if !config.grass {
            return;
        }
        let (start, end) = config.effective_grass_fade();
        let enabled = config.grass_enable_fade;
        app.insert_resource(GrassFade {
            enabled,
            start,
            end,
        })
        .init_resource::<GrassState>()
        .init_resource::<GrassMetrics>()
        .add_systems(PostUpdate, stream_grass.in_set(GrassStreaming))
        .add_plugins(GrassRenderPlugin);
    }
}

#[derive(Resource, Default, Debug)]
pub struct GrassMetrics {
    /// All desired cells have finished generation and admitted their species batches.
    pub ready: bool,
    /// Current desired cells rejected by the pool budget; retired/prefetch failures do not block capture.
    pub failures: u64,
    pub generated_cells: u64,
    pub generation_micros: u64,
    pub max_generation_micros: u64,
    pub cache_hits: u64,
    pub cache_bytes: usize,
    pub pending_cells: usize,
    pub uploads_this_frame: usize,
    pub rejected_for_budget: u64,
}

struct PoolEntry {
    data: GeneratedGrass,
    terrain: GrassTerrain,
    bytes: usize,
    touched: u64,
}

struct PendingGrass {
    /// A speculative job has no root; committed jobs may publish only to this exact lifetime.
    root: Option<Entity>,
    task: Task<(GeneratedGrass, GrassTerrain)>,
}
struct Attached {
    root: Entity,
    entities: Vec<Entity>,
    uploaded: usize,
}

#[derive(Resource, Default)]
struct GrassState {
    pending: HashMap<u32, PendingGrass>,
    pool: HashMap<u32, PoolEntry>,
    lru: BTreeSet<(u64, u32)>,
    attached: HashMap<u32, Attached>,
    /// A cell that exceeds the budget is not regenerated continuously.
    budget_rejected: HashSet<u32>,
    previous_desired: HashMap<u32, (Entity, GrassTerrain)>,
    bytes: usize,
    tick: u64,
    previous_camera: Option<Vec2>,
}

impl GrassState {
    /// Admit generated instances within budget, evicting only cells outside the desired window.
    fn admit(
        &mut self,
        id: u32,
        data: GeneratedGrass,
        terrain: GrassTerrain,
        budget: usize,
        desired: &HashMap<u32, (Entity, GrassTerrain)>,
    ) -> bool {
        let bytes = data.bytes.saturating_add(terrain.bytes());
        if bytes > budget {
            return false;
        }
        while self.bytes.saturating_add(bytes) > budget {
            let victim = self
                .lru
                .iter()
                .find(|(_, id)| !desired.contains_key(id))
                .copied();
            let Some((touched, victim)) = victim else {
                return false;
            };
            self.lru.remove(&(touched, victim));
            self.remove_pool(victim);
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.lru.insert((self.tick, id));
        self.pool.insert(
            id,
            PoolEntry {
                data,
                terrain,
                bytes,
                touched: self.tick,
            },
        );
        true
    }

    /// Retire both the retained surface and generated arrays when their source stops matching.
    fn remove_pool(&mut self, id: u32) {
        if let Some(entry) = self.pool.remove(&id) {
            self.lru.remove(&(entry.touched, id));
            self.bytes = self.bytes.saturating_sub(entry.bytes);
        }
    }
}

/// Keep the scheduling/main-thread work bounded; masks and blade generation stay on the pool.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn stream_grass(
    mut commands: Commands,
    config: Res<EngineConfig>,
    cache: Res<GrassCache>,
    origin: Res<RenderOrigin>,
    camera: Query<&Transform, With<StreamingCamera>>,
    cells: Query<(Entity, &CellRef, &ExteriorCellGrid, &GrassTerrain), With<StreamedCellRoot>>,
    mut state: ResMut<GrassState>,
    mut metrics: ResMut<GrassMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    let started = Instant::now();
    let Ok(camera) = camera.single() else {
        return;
    };
    let radius = config.effective_grass_radius();
    let absolute = Vec2::new(
        camera.translation.x + origin.0.x as f32 * CELL_SIZE,
        -camera.translation.z + origin.0.y as f32 * CELL_SIZE,
    );
    let center = IVec2::new(
        (absolute.x / CELL_SIZE).floor() as i32,
        (absolute.y / CELL_SIZE).floor() as i32,
    );
    let movement = state
        .previous_camera
        .map_or(Vec2::ZERO, |old| absolute - old);
    state.previous_camera = Some(absolute);
    state.tick = state.tick.wrapping_add(1);
    let mut wanted: Vec<_> = cells
        .iter()
        .filter(|(_, _, grid, _)| {
            grid.0.x.abs_diff(center.x).max(grid.0.y.abs_diff(center.y)) <= radius as u32
        })
        .map(|(root, id, grid, terrain)| (id.0, root, grid.0, terrain.clone()))
        .collect();
    wanted.sort_by_key(|(id, _, grid, _)| {
        let x = u128::from(grid.x.abs_diff(center.x));
        let y = u128::from(grid.y.abs_diff(center.y));
        (x * x + y * y, *id)
    });
    let desired: HashMap<_, _> = wanted
        .iter()
        .map(|(id, root, _, terrain)| (*id, (*root, terrain.clone())))
        .collect();
    // A cell's welded field can differ when it commits again. Never upload a cached
    // result against a new surface, even before a replacement worker completes.
    let changed: Vec<_> = desired
        .iter()
        .filter(|(id, (_, terrain))| {
            state
                .pool
                .get(id)
                .is_some_and(|entry| !entry.terrain.same_surface(terrain))
        })
        .map(|(&id, _)| id)
        .collect();
    for id in changed {
        state.remove_pool(id);
    }
    let retiring: Vec<_> = state
        .attached
        .iter()
        .filter(|(id, attached)| {
            desired.get(id).map(|(root, _)| *root) != Some(attached.root)
                || !state.pool.contains_key(id)
        })
        .map(|(&id, _)| id)
        .collect();
    for id in retiring {
        if let Some(attached) = state.attached.remove(&id) {
            for entity in attached.entities {
                commands.entity(entity).try_despawn();
            }
        }
    }
    // A rejected resident can be tried again after the committed window changes.
    // This prevents transient pinning pressure from becoming a permanent missing cell.
    if state.previous_desired != desired {
        state.budget_rejected.clear();
    }
    // Replace equal-content old Arcs too, making subsequent checks pointer-fast.
    state.previous_desired.clone_from(&desired);
    // Keep obsolete task handles until completion. Dropping a Task cannot preempt
    // synchronous CPU work inside an already-polling generation future; retaining
    // handles ensures fast travel cannot start more than four workers in total.
    let direction = movement.normalize_or_zero();
    let ahead = IVec2::new(
        center
            .x
            .saturating_add(direction.x.round() as i32 * (radius + 1)),
        center
            .y
            .saturating_add(direction.y.round() as i32 * (radius + 1)),
    );
    let prefetch = (movement.length_squared() > 0.01)
        .then(|| cache.cell_at(config.worldspace_id, ahead.x, ahead.y))
        .flatten();
    let ready: Vec<_> = state
        .pending
        .iter_mut()
        .filter_map(|(&id, pending)| {
            block_on(poll_once(&mut pending.task))
                .map(|(data, terrain)| (id, pending.root, data, terrain))
        })
        .collect();
    let budget = if config.grass_cache_mib == 0 {
        usize::MAX
    } else {
        config.grass_cache_mib.saturating_mul(1024 * 1024)
    };
    for (id, root, data, terrain) in ready {
        state.pending.remove(&id);
        metrics.generated_cells += 1;
        metrics.generation_micros = metrics
            .generation_micros
            .saturating_add(data.generation_micros);
        metrics.max_generation_micros = metrics.max_generation_micros.max(data.generation_micros);
        profiler.record_micros("grass/generate_cell", data.generation_micros);
        let terrain = if let Some((current_root, current_terrain)) = desired.get(&id) {
            if root.is_some_and(|root| root != *current_root)
                || !terrain.same_surface(current_terrain)
            {
                continue;
            }
            current_terrain.clone()
        } else if root.is_none() && Some(id) == prefetch {
            terrain
        } else {
            continue;
        };
        if !state.admit(id, data, terrain, budget, &desired) {
            state.budget_rejected.insert(id);
            metrics.rejected_for_budget += 1;
            warn!(
                "grass cell {id:08X} exceeds the available CPU pool budget; increase --grass-cache-mib"
            );
        }
    }
    metrics.uploads_this_frame = 0;
    let upload_limit = if config.grass_uploads_per_frame == 0 {
        usize::MAX
    } else {
        config.grass_uploads_per_frame
    };
    for (id, root, _, terrain) in &wanted {
        let (id, root) = (*id, *root);
        if state.pool.contains_key(&id) {
            let tick = state.tick;
            let old_tick = state.pool[&id].touched;
            state.lru.remove(&(old_tick, id));
            state.lru.insert((tick, id));
            let entry = state.pool.get_mut(&id).unwrap();
            entry.touched = tick;
            // Identical prefetch/reloaded surfaces now share the committed root's Arc.
            entry.terrain = terrain.clone();
            if !state.attached.contains_key(&id) {
                metrics.cache_hits += 1;
            }
            let uploaded = state.attached.get(&id).map_or(0, |entry| entry.uploaded);
            let batches: Vec<_> = state.pool[&id]
                .data
                .batches
                .iter()
                .skip(uploaded)
                .take(upload_limit.saturating_sub(metrics.uploads_this_frame))
                .cloned()
                .collect();
            let attached = state.attached.entry(id).or_insert(Attached {
                root,
                entities: vec![],
                uploaded: 0,
            });
            for batch in batches {
                let entity = commands
                    .spawn((
                        batch,
                        Transform::default(),
                        Visibility::default(),
                        ChildOf(root),
                    ))
                    .id();
                attached.entities.push(entity);
                attached.uploaded += 1;
                metrics.uploads_this_frame += 1;
            }
        }
    }
    for id in wanted.iter().map(|(id, _, _, _)| *id).chain(prefetch) {
        if state.pending.len() >= 4 {
            break;
        }
        if state.pool.contains_key(&id)
            || state.pending.contains_key(&id)
            || state.budget_rejected.contains(&id)
            || cache.cell(id).is_none()
        {
            continue;
        }
        let mapped = cache.clone();
        let generation_config = config.clone();
        let source = desired.get(&id).cloned();
        let root = source.as_ref().map(|(root, _)| *root);
        state.pending.insert(
            id,
            PendingGrass {
                root,
                task: AsyncComputeTaskPool::get().spawn(async move {
                    let terrain = source.map(|(_, terrain)| terrain).unwrap_or_else(|| {
                        GrassTerrain::from_cache(
                            mapped.cell(id).expect("selected cached cell exists"),
                        )
                    });
                    let data =
                        generation::generate_on_terrain(&mapped, id, &generation_config, &terrain);
                    (data, terrain)
                }),
            },
        );
    }
    metrics.cache_bytes = state.bytes;
    metrics.pending_cells = state.pending.len();
    metrics.failures = desired
        .keys()
        .filter(|id| state.budget_rejected.contains(id))
        .count() as u64;
    metrics.ready = state.pending.is_empty()
        && wanted.iter().all(|(id, _, _, _)| {
            let id = *id;
            cache.cell(id).is_none()
                || state.pool.get(&id).is_some_and(|entry| {
                    state
                        .attached
                        .get(&id)
                        .is_some_and(|attached| attached.uploaded == entry.data.batches.len())
                })
        });
    profiler.set_gauge("grass/cache_bytes", state.bytes as f64);
    profiler.set_gauge("grass/pending_cells", state.pending.len() as f64);
    profiler.increment("grass/uploads", metrics.uploads_this_frame as u64);
    profiler.record_elapsed("grass/stream_frame", started);
}

/// Exercise CPU grass streaming from terrain commit regressions without installing a renderer.
#[cfg(test)]
pub(crate) fn add_cpu_streaming_test_system(app: &mut App) {
    app.init_resource::<GrassState>()
        .init_resource::<GrassMetrics>()
        .add_systems(PostUpdate, stream_grass);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Colour-only changes invalidate pooled/prefetched surfaces and charge the extra plane.
    fn vclr_surface_identity_and_budget_include_the_colour_plane() {
        let mut snapshot = TerrainSnapshot {
            cell_id: 42,
            width: 33,
            height: 33,
            heights: vec![0.0; 1089],
            normals: vec![0; 3267],
            vertex_colors: vec![255; 3267],
            layers: vec![],
            water_height: None,
            water_type_form_id: None,
        };
        let plain = GrassTerrain::new(snapshot.heights.clone(), snapshot.water_height);
        assert!(plain.same_surface(&GrassTerrain::new(vec![0.0; 1089], None)));
        assert_eq!(
            plain.bytes(),
            1089 * 4 + std::mem::size_of::<GrassSurface>() + 2 * std::mem::size_of::<usize>()
        );
        let white = GrassTerrain::from_snapshot(&snapshot);
        assert!(
            !plain.same_surface(&white),
            "white speculative data cannot promote as a coloured field"
        );
        assert_eq!(white.bytes() - plain.bytes(), 4356);
        snapshot.vertex_colors[0] = 0;
        let changed = GrassTerrain::from_snapshot(&snapshot);
        assert!(!white.same_surface(&changed));
        snapshot.vertex_colors.clear();
        let absent = GrassTerrain::from_snapshot(&snapshot);
        assert!(plain.same_surface(&absent));
        assert!(absent.luminance().is_none());
    }

    #[test]
    /// Streaming regenerates colour-only replacements before marking grass ready.
    fn tint_colour_only_recommit_replaces_cached_brightness() {
        let (mut app, _directory) = surface_streaming_app();
        let root = surface_root(&mut app, 0.0, None);
        settle_surface_streaming(&mut app);
        let original = app
            .world_mut()
            .query::<&GrassBatch>()
            .single(app.world())
            .unwrap()
            .instances
            .clone();
        app.world_mut().despawn(root);
        app.update();
        let root = surface_root(&mut app, 0.0, None);
        let mut samples = vec![0.0; 1089];
        samples.extend(vec![0.0; 1089]);
        app.world_mut()
            .entity_mut(root)
            .insert(GrassTerrain::new(samples, None));
        settle_surface_streaming(&mut app);
        let batch = app
            .world_mut()
            .query::<&GrassBatch>()
            .single(app.world())
            .unwrap();
        assert!(!Arc::ptr_eq(&original, &batch.instances));
        assert!(original.iter().any(|instance| instance.color != 0));
        assert!(batch.instances.iter().all(|instance| instance.color == 0));
        assert_eq!(app.world().resource::<GrassMetrics>().generated_cells, 2);
    }

    /// Build an empty generated result with a chosen allocation budget for pool tests.
    fn result(bytes: usize) -> GeneratedGrass {
        GeneratedGrass {
            batches: vec![],
            generation_micros: 17,
            bytes,
        }
    }

    #[test]
    /// Verify lru evicts old recent cells but keeps committed cells within budget.
    fn lru_evicts_old_recent_cells_but_keeps_committed_cells_within_budget() {
        let mut state = GrassState::default();
        let mut desired = HashMap::new();
        let terrain = GrassTerrain::new(vec![0.0; 33 * 33], None);
        let budget = 100 + 3 * terrain.bytes();
        assert!(state.admit(7, result(30), terrain.clone(), budget, &desired));
        state.tick = 1;
        assert!(state.admit(8, result(30), terrain.clone(), budget, &desired));
        state.tick = 2;
        assert!(state.admit(9, result(30), terrain.clone(), budget, &desired));
        desired.insert(7, (Entity::PLACEHOLDER, terrain.clone()));
        assert!(state.admit(10, result(40), terrain.clone(), budget, &desired));
        assert!(state.pool.contains_key(&7));
        assert!(!state.pool.contains_key(&8));
        assert_eq!(state.bytes, budget);
        assert!(!state.admit(11, result(budget + 1), terrain, budget, &desired));
        assert!(state.bytes <= budget);
    }

    #[test]
    /// Verify disabled plugin does not install grass resources or systems.
    fn disabled_plugin_does_not_install_grass_resources_or_systems() {
        let mut app = App::new();
        app.insert_resource(EngineConfig {
            grass: false,
            ..Default::default()
        })
        .add_plugins(GrassPlugin);
        assert!(!app.world().contains_resource::<GrassState>());
        assert!(!app.world().contains_resource::<GrassFade>());
        app.update();
    }

    /// Create a one-species mapped cache and CPU-only app with one candidate per spot.
    fn surface_streaming_app() -> (App, tempfile::TempDir) {
        use shared::grass::{GrassCell, GrassLayer, GrassQuadrant, GrassType};
        AsyncComputeTaskPool::get_or_init(|| {
            bevy::tasks::TaskPoolBuilder::new().num_threads(1).build()
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grass_cache.rkyv");
        std::fs::write(
            &path,
            rkyv::to_bytes::<rkyv::rancor::Error>(&shared::grass::GrassCache {
                version: shared::grass::GRASS_CACHE_VERSION,
                types: vec![GrassType {
                    form_id: 7,
                    model_path: "meshes/grass.glb".into(),
                    density: 100,
                    min_slope: 0,
                    max_slope: 90,
                    units_from_water: 0,
                    water_comparison: 0,
                    position_range: 0.0,
                    height_range: 0.0,
                    color_range: 0.0,
                    wave_period: 0.0,
                    flags: 4,
                }],
                cells: vec![GrassCell {
                    cell_id: 42,
                    worldspace_id: 3,
                    grid_x: 0,
                    grid_y: 0,
                    input_hash: [0; 32],
                    obstruction_failures: vec![],
                    expanded_sampling: 0,
                    heights: vec![0.0; 33 * 33],
                    normals: vec![],
                    vertex_colors: vec![],
                    water_height: None,
                    quadrants: (0..4)
                        .map(|quadrant| GrassQuadrant {
                            quadrant,
                            layers: vec![GrassLayer {
                                texture_form_id: 9,
                                is_base: false,
                                grass_ids: vec![7],
                                weights: vec![1.0; 17 * 17],
                            }],
                            exclusion: vec![0; 512],
                        })
                        .collect(),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        let mut app = App::new();
        app.insert_resource(EngineConfig {
            grass: true,
            grass_min_spacing: 256.0,
            grass_uploads_per_frame: 0,
            ..EngineConfig::default()
        })
        .insert_resource(GrassCache::open(&path).unwrap())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .init_resource::<ProfilingState>();
        add_cpu_streaming_test_system(&mut app);
        app.world_mut()
            .spawn((StreamingCamera, Transform::default()));
        (app, directory)
    }

    /// Commit a chosen flat surface without changing the immutable archived source.
    fn surface_root(app: &mut App, height: f32, water: Option<f32>) -> Entity {
        app.world_mut()
            .spawn((
                StreamedCellRoot,
                CellRef(42),
                ExteriorCellGrid(IVec2::ZERO),
                Transform::default(),
                GrassTerrain::new(vec![height; 33 * 33], water),
            ))
            .id()
    }

    /// Wait only for CPU generation/admission; every fixture worker has one candidate.
    fn settle_surface_streaming(app: &mut App) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.update();
            if app.world().resource::<GrassMetrics>().ready {
                break;
            }
            assert!(Instant::now() < deadline, "surface grass did not settle");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Identical reloads reuse arrays; new height or water contents invalidate cached placement.
    #[test]
    fn cached_surface_reuses_equal_reload_and_invalidates_height_and_water_changes() {
        let (mut app, _directory) = surface_streaming_app();
        let root = surface_root(&mut app, 0.0, None);
        settle_surface_streaming(&mut app);
        let original = app
            .world_mut()
            .query::<&GrassBatch>()
            .single(app.world())
            .unwrap()
            .instances
            .clone();
        assert_eq!(app.world().resource::<GrassMetrics>().generated_cells, 1);
        app.world_mut().despawn(root);
        app.update();
        let root = surface_root(&mut app, 0.0, None);
        settle_surface_streaming(&mut app);
        let reused = app
            .world_mut()
            .query::<&GrassBatch>()
            .single(app.world())
            .unwrap()
            .instances
            .clone();
        assert!(
            Arc::ptr_eq(&original, &reused),
            "same surface must reuse its instance allocation"
        );
        assert_eq!(app.world().resource::<GrassMetrics>().generated_cells, 1);
        app.world_mut().despawn(root);
        app.update();
        let root = surface_root(&mut app, 10.0, None);
        settle_surface_streaming(&mut app);
        let batch = app
            .world_mut()
            .query::<&GrassBatch>()
            .single(app.world())
            .unwrap();
        assert_eq!(
            batch.height_min, 10.0,
            "cached roots must follow the new committed surface"
        );
        assert!(!Arc::ptr_eq(&original, &batch.instances));
        assert_eq!(app.world().resource::<GrassMetrics>().generated_cells, 2);
        app.world_mut().despawn(root);
        app.update();
        surface_root(&mut app, 10.0, Some(20.0));
        settle_surface_streaming(&mut app);
        assert_eq!(
            app.world_mut()
                .query::<&GrassBatch>()
                .iter(app.world())
                .count(),
            0,
            "new water must reject the previously cached above-water species"
        );
        assert_eq!(app.world().resource::<GrassMetrics>().generated_cells, 3);
        let state = app.world().resource::<GrassState>();
        assert!(
            state.bytes >= state.pool[&42].terrain.bytes(),
            "source storage counts against the pool"
        );
    }

    /// An obsolete job cannot publish into a new root lifetime, even with identical heights.
    #[test]
    fn pending_surface_job_cannot_publish_after_equal_surface_root_replacement() {
        let (mut app, _directory) = surface_streaming_app();
        let root = surface_root(&mut app, 0.0, None);
        app.update();
        assert_eq!(
            app.world().resource::<GrassState>().pending[&42].root,
            Some(root)
        );
        app.world_mut().despawn(root);
        let replacement = surface_root(&mut app, 0.0, None);
        settle_surface_streaming(&mut app);
        assert_eq!(
            app.world().resource::<GrassMetrics>().generated_cells,
            2,
            "the old-root result must be discarded and a replacement job completed"
        );
        assert_eq!(
            app.world().resource::<GrassState>().attached[&42].root,
            replacement
        );
        assert_eq!(
            app.world_mut()
                .query::<&GrassBatch>()
                .iter(app.world())
                .count(),
            1
        );
    }

    /// Speculative jobs may be promoted only when the later committed field matches exactly.
    #[test]
    fn prefetched_surface_is_promoted_only_when_committed_heights_match() {
        for height in [0.0, 10.0] {
            let (mut app, _directory) = surface_streaming_app();
            let mapped = app.world().resource::<GrassCache>().clone();
            let config = app.world().resource::<EngineConfig>().clone();
            let task = AsyncComputeTaskPool::get().spawn(async move {
                let terrain = GrassTerrain::from_cache(mapped.cell(42).unwrap());
                let data = generation::generate(&mapped, 42, &config);
                (data, terrain)
            });
            app.world_mut()
                .resource_mut::<GrassState>()
                .pending
                .insert(42, PendingGrass { root: None, task });
            let root = surface_root(&mut app, height, None);
            settle_surface_streaming(&mut app);
            let batch = app
                .world_mut()
                .query::<&GrassBatch>()
                .single(app.world())
                .unwrap();
            assert_eq!(batch.height_min, height);
            assert_eq!(
                app.world().resource::<GrassMetrics>().generated_cells,
                if height == 0.0 { 1 } else { 2 }
            );
            let published = app.world().get::<GrassTerrain>(root).unwrap();
            let retained = &app.world().resource::<GrassState>().pool[&42].terrain;
            assert!(
                Arc::ptr_eq(&published.0, &retained.0),
                "future frames must use the committed source identity"
            );
        }
    }

    #[test]
    /// Verify committed cells upload in budget and reuse lru after unload.
    fn committed_cells_upload_in_budget_and_reuse_lru_after_unload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grass_cache.rkyv");
        std::fs::write(
            &path,
            rkyv::to_bytes::<rkyv::rancor::Error>(&shared::grass::GrassCache {
                version: shared::grass::GRASS_CACHE_VERSION,
                types: vec![],
                cells: vec![shared::grass::GrassCell {
                    cell_id: 42,
                    worldspace_id: 3,
                    grid_x: 0,
                    grid_y: 0,
                    input_hash: [0; 32],
                    obstruction_failures: vec![],
                    expanded_sampling: 0,
                    heights: vec![0.0; 33 * 33],
                    normals: vec![],
                    vertex_colors: vec![],
                    water_height: None,
                    quadrants: (0..4)
                        .map(|quadrant| shared::grass::GrassQuadrant {
                            quadrant,
                            layers: vec![],
                            exclusion: vec![0; 512],
                        })
                        .collect(),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        let mut app = App::new();
        app.insert_resource(EngineConfig {
            grass: true,
            grass_uploads_per_frame: 1,
            ..EngineConfig::default()
        })
        .insert_resource(GrassCache::open(&path).unwrap())
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .init_resource::<GrassMetrics>()
        .init_resource::<ProfilingState>()
        .add_systems(PostUpdate, stream_grass);
        let batches = [7, 11]
            .into_iter()
            .map(|grass_id| GrassBatch {
                cell_id: 42,
                grid: [0, 0],
                grass_id,
                model_path: format!("meshes/{grass_id}.glb"),
                instances: Arc::new(vec![]),
                height_min: 100.0,
                height_span: 20.0,
                slot_count: 128,
                wave_period: 2.0,
                flags: 0,
            })
            .collect();
        let mut state = GrassState::default();
        let terrain = GrassTerrain::new(vec![0.0; 33 * 33], None);
        assert!(state.admit(
            42,
            GeneratedGrass {
                batches,
                generation_micros: 1,
                bytes: 100
            },
            terrain.clone(),
            10_000,
            &HashMap::new()
        ));
        app.insert_resource(state);
        app.world_mut()
            .spawn((StreamingCamera, Transform::default()));
        let root = app
            .world_mut()
            .spawn((
                StreamedCellRoot,
                CellRef(42),
                ExteriorCellGrid(IVec2::ZERO),
                terrain.clone(),
                Transform::default(),
            ))
            .id();
        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&GrassBatch>()
                .iter(app.world())
                .count(),
            1
        );
        assert_eq!(app.world().resource::<GrassMetrics>().uploads_this_frame, 1);
        assert!(!app.world().resource::<GrassMetrics>().ready);
        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&GrassBatch>()
                .iter(app.world())
                .count(),
            2
        );
        assert_eq!(app.world().resource::<GrassMetrics>().uploads_this_frame, 1);
        assert!(app.world().resource::<GrassMetrics>().ready);
        app.world_mut().despawn(root);
        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&GrassBatch>()
                .iter(app.world())
                .count(),
            0
        );
        assert!(app.world().resource::<GrassState>().pool.contains_key(&42));
        app.world_mut().spawn((
            StreamedCellRoot,
            CellRef(42),
            ExteriorCellGrid(IVec2::ZERO),
            GrassTerrain::new(vec![0.0; 33 * 33], None),
            Transform::default(),
        ));
        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&GrassBatch>()
                .iter(app.world())
                .count(),
            1
        );
        assert_eq!(app.world().resource::<GrassMetrics>().generated_cells, 0);
        assert_eq!(
            app.world().resource::<GrassMetrics>().cache_bytes,
            100 + terrain.bytes()
        );
        let pool = AsyncComputeTaskPool::get_or_init(|| {
            bevy::tasks::TaskPoolBuilder::new().num_threads(1).build()
        });
        for id in 100..104 {
            app.world_mut().resource_mut::<GrassState>().pending.insert(
                id,
                PendingGrass {
                    root: None,
                    task: pool.spawn(async {
                        std::future::pending::<(GeneratedGrass, GrassTerrain)>().await
                    }),
                },
            );
        }
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<StreamingCamera>>()
            .single(app.world())
            .unwrap();
        app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap()
            .translation
            .x = CELL_SIZE * 20.0;
        app.update();
        assert_eq!(
            app.world().resource::<GrassState>().pending.len(),
            4,
            "obsolete workers must retain their slots until completion, even after a camera jump"
        );
        // A distant mod cell and a rebase at the coordinate boundary must not overflow
        // cell selection or directional prefetch while obsolete workers are still alive.
        app.world_mut().resource_mut::<RenderOrigin>().0 = IVec2::splat(i32::MIN);
        app.world_mut()
            .entity_mut(camera)
            .get_mut::<Transform>()
            .unwrap()
            .translation = Vec3::ZERO;
        app.world_mut().spawn((
            StreamedCellRoot,
            CellRef(99),
            ExteriorCellGrid(IVec2::splat(i32::MAX)),
            terrain,
            Transform::default(),
        ));
        app.update();
        assert_eq!(app.world().resource::<GrassState>().pending.len(), 4);
        assert_eq!(
            app.world_mut()
                .query::<&GrassBatch>()
                .iter(app.world())
                .count(),
            0
        );
    }
}

#[cfg(test)]
mod submission_count_tests {
    use super::*;
    use bytemuck::Zeroable;

    /// Equal ranks survive together, and shot evidence shares the renderer's conservative prefix.
    #[test]
    fn conservative_prefix_preserves_rank_collisions() {
        let instances: Vec<_> = [1, 1, 3]
            .into_iter()
            .map(|slot| GrassInstance {
                slot,
                ..GrassInstance::zeroed()
            })
            .collect();
        assert_eq!(
            submitted_instance_count(
                &instances,
                4,
                Vec3::new(-7500.0, 0.0, 0.0),
                7000.0,
                8000.0,
                true
            ),
            2
        );
        assert_eq!(
            submitted_instance_count(
                &instances,
                4,
                Vec3::new(-9000.0, 0.0, 0.0),
                7000.0,
                8000.0,
                true
            ),
            0
        );
        assert_eq!(
            submitted_instance_count(
                &instances,
                4,
                Vec3::new(-9000.0, 0.0, 0.0),
                7000.0,
                8000.0,
                false
            ),
            3
        );
    }
}
