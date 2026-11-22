//! Streaming of object LOD chunks: the retail `.bto` blocks the converter imports into the
//! additive `lod_object_chunks` table.
//!
//! A pack without that table has no object chunks, so nothing here runs for it. For a pack with
//! it the load fails closed: the rows are validated by [`crate::world::database::load_lod_object_chunks`]
//! and each chunk file is hash-checked, one IO-pool task
//! per file, before any chunk is drawn. A damaged row, a missing file or a wrong hash logs one
//! error, draws no object LOD for that pack and is recorded in [`LodObjects::failure`], never
//! skipped chunk by chunk. The failure belongs to the pack: it stays through worldspace changes
//! and is cleared only when the pack changes.
//!
//! Which chunks draw follows the retail object rule (RF-LOD-004): a chunk of tier `t` draws when
//! [`TerrainLodDistances::object_tier`] picks `t` for its cells. With the High defaults tier 4
//! draws out to `fBlockLevel0Distance` (35,000), tier 8 from there to `fBlockLevel1Distance`
//! (70,000) and tier 16 from there to `fBlockMaximumDistance` (250,000), by the distance from the
//! continuous camera position to the block rectangle (strict comparisons).
//!
//! **Hand-over.** Incoming scenes stay hidden until all replacements in an outgoing block have
//! attached. The visibility swap is atomic within the block. At the hand-over deadline, the
//! runtime releases the old block and shows available chunks.
//!
//! **Loaded cells.** A tier-4 chunk splits its geometry into cell-local pieces. Each piece stays
//! visible until the cell's full-detail models are ready, then stays hidden until the cell unloads.
//! Marked pieces also yield to ready large references beyond the ordinary grid.
//!
//! The system runs before `update_render_origin`, so a root spawned in the frame the origin shifts
//! is rebased in that frame. Spawns follow the streaming controls and the commit budget that
//! terrain chunks use. The verified rows are kept while the worldspace is unchanged, so leaving
//! and re-entering the exterior (a door) does not read or hash them again.

use super::{
    ActiveSpace, CellStatus, PendingAssetProfile, PendingModel, RenderOrigin,
    StreamingCommitBudget, StreamingWorld,
    large_references::LargeReferences,
    lod::{LodChunkGridOrigin, chunk_min_grid, lod_worldspace},
    runtime::StreamingRuntime,
};
use crate::{
    config::{EngineConfig, TerrainLodDistances},
    world::{
        components::{CELL_SIZE, StreamingCamera},
        database::{
            CellKey, LodChunkMetadata, LodObjectAtlas, WorldDatabase, verify_lod_object_atlas,
            verify_lod_object_payloads,
        },
    },
};
use bevy::{
    asset::{LoadState, RecursiveDependencyLoadState},
    gltf::GltfAssetLabel,
    math::DVec2,
    prelude::*,
    tasks::{IoTaskPool, Task, block_on},
    world_serialization::{WorldAsset, WorldAssetRoot, WorldInstanceReady},
};
use shared::lod::{ChunkKey, LodTier};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
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

#[derive(Component)]
pub(super) struct ObjectSceneReady;

pub(super) fn mark_object_scene_ready(
    event: On<WorldInstanceReady>,
    mut commands: Commands,
    roots: Query<(), With<LodObjectRoot>>,
) {
    if roots.contains(event.entity) {
        commands.entity(event.entity).insert(ObjectSceneReady);
    }
}

#[derive(Clone, Copy)]
struct ObjectRoot {
    entity: Entity,
    spawned: Instant,
    visible: bool,
}

struct RetiringRoot {
    root: ObjectRoot,
    since: Instant,
}

/// An engineering limit for a scene that never attaches or a stalled commit budget.
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(30);

/// All tiers share a worldspace origin and tile each coarser chunk exactly.
fn overlaps(first: ChunkKey, second: ChunkKey) -> bool {
    let (fine, coarse) = if first.tier <= second.tier {
        (first, second)
    } else {
        (second, first)
    };
    let ratio = coarse.tier.side_cells() / fine.tier.side_cells();
    fine.worldspace_id == coarse.worldspace_id
        && fine.anchor.x.div_euclid(ratio) == coarse.anchor.x
        && fine.anchor.y.div_euclid(ratio) == coarse.anchor.y
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
    wanted: HashSet<ChunkKey>,
    roots: HashMap<ChunkKey, ObjectRoot>,
    retiring: HashMap<ChunkKey, RetiringRoot>,
    failed_scenes: HashSet<ChunkKey>,
    /// The worldspace's shared atlas file, when its chunks reference one, with its integrity check
    /// in flight until `atlas_ready`. Chunks draw only after the atlas check has passed too.
    atlas: Option<LodObjectAtlas>,
    atlas_check: Option<Task<Result<(), String>>>,
    atlas_ready: bool,
    /// Keeps the shared atlas loaded once for every chunk that references it.
    atlas_handle: Option<Handle<Image>>,
    /// The pack (assets folder) the state above belongs to.
    pack: Option<PathBuf>,
    /// The camera position and distances used for the current selection.
    last_camera: Option<DVec2>,
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

    /// Chunks currently spawned and wanted.
    pub fn resident(&self) -> usize {
        self.roots.len()
    }

    /// Chunks that are no longer wanted and wait for the wanted ones to attach.
    pub fn retiring(&self) -> usize {
        self.retiring.len()
    }

    fn select(&mut self, commands: &mut Commands, wanted: HashSet<ChunkKey>, now: Instant) {
        self.roots.retain(|key, root| {
            if wanted.contains(key) {
                return true;
            }
            if root.visible {
                self.retiring.insert(
                    *key,
                    RetiringRoot {
                        root: *root,
                        since: now,
                    },
                );
            } else {
                commands.entity(root.entity).try_despawn();
            }
            false
        });
        self.retiring.retain(|key, retiring| {
            if wanted.contains(key) {
                self.roots.insert(*key, retiring.root);
                return false;
            }
            true
        });
        self.wanted = wanted;
    }

    /// Remove outgoing roots and show their replacements in the same visibility pass. A wanted
    /// chunk that has no root yet (for example, it is waiting for budget) still blocks retirement.
    fn switch_ready_blocks(
        &mut self,
        commands: &mut Commands,
        ready: &HashSet<ChunkKey>,
        now: Instant,
    ) {
        self.retiring.retain(|&outgoing, retiring| {
            let waiting = self.wanted.iter().any(|&incoming| {
                overlaps(outgoing, incoming)
                    && !ready.contains(&incoming)
                    && !self.roots.get(&incoming).is_some_and(|root| root.visible)
            });
            if waiting && now.duration_since(retiring.since) < HANDOVER_TIMEOUT {
                return true;
            }
            if waiting {
                warn!(?outgoing, "object LOD hand-over timed out");
            }
            commands.entity(retiring.root.entity).try_despawn();
            false
        });
        for (&incoming, root) in &mut self.roots {
            if !root.visible
                && ready.contains(&incoming)
                && !self
                    .retiring
                    .keys()
                    .any(|&outgoing| overlaps(outgoing, incoming))
            {
                commands.entity(root.entity).insert(Visibility::Inherited);
                root.visible = true;
            }
        }
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
        self.atlas = None;
        self.atlas_check = None;
        self.atlas_ready = false;
        self.atlas_handle = None;
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
            commands.entity(root.entity).try_despawn();
        }
        for (_, retiring) in self.retiring.drain() {
            commands.entity(retiring.root.entity).try_despawn();
        }
        self.wanted.clear();
        self.failed_scenes.clear();
        self.last_camera = None;
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

    /// Recompute when the camera position or distance settings change.
    fn selection_changed(&mut self, camera: DVec2, distances: &TerrainLodDistances) -> bool {
        if self.last_camera == Some(camera) && self.last_distances.as_ref() == Some(distances) {
            return false;
        }
        self.last_camera = Some(camera);
        self.last_distances = Some(*distances);
        true
    }

    /// Starts one payload check per row of every tier, and one for the shared atlas file.
    fn start_checks(
        &mut self,
        chunks: Vec<LodChunkMetadata>,
        atlas: Option<LodObjectAtlas>,
        assets: &std::path::Path,
    ) {
        self.checked.clear();
        self.atlas_ready = atlas.is_none();
        self.atlas_check = atlas.clone().map(|atlas| {
            let assets = assets.to_owned();
            IoTaskPool::get().spawn(async move {
                verify_lod_object_atlas(&assets, &atlas).map_err(|error| format!("{error:#}"))
            })
        });
        self.atlas = atlas;
        self.checking = chunks
            .into_iter()
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
        if self.failure.is_some() {
            return;
        }
        if let Some(task) = self.atlas_check.take() {
            if task.is_finished() {
                match block_on(task) {
                    Ok(()) => self.atlas_ready = true,
                    Err(reason) => {
                        self.fail(format!("object LOD is off for this pack: {reason}"));
                        return;
                    }
                }
            } else {
                self.atlas_check = Some(task);
            }
        }
        for (chunk, task) in std::mem::take(&mut self.checking) {
            if !task.is_finished() {
                self.checking.push((chunk, task));
                continue;
            }
            match block_on(task) {
                Ok(()) => self.checked.push(chunk),
                Err(reason) => {
                    self.fail(format!("object LOD is off for this pack: {reason}"));
                    return;
                }
            }
        }
        if self.checking.is_empty() && self.atlas_ready && !self.checked.is_empty() {
            let mut chunks = std::mem::take(&mut self.checked);
            chunks.sort_by_key(|chunk| chunk.key);
            self.chunks = chunks;
        }
    }

    /// Turns object LOD off for the pack, once, with the reason logged as an error.
    fn fail(&mut self, reason: String) {
        error!("{reason}");
        self.failure = Some(reason);
        self.checking.clear();
        self.checked.clear();
        self.atlas_check = None;
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
        Visibility::Hidden,
    )
}

/// The chunks that draw from `camera`: chunks whose cells fall in the tier the
/// object rule picks for them. All cells of a chunk share its box, so the first cell decides.
pub fn wanted_object_chunks(
    chunks: &[LodChunkMetadata],
    camera: DVec2,
    distances: &TerrainLodDistances,
    nama: f32,
) -> Vec<ChunkKey> {
    chunks
        .iter()
        .filter(|chunk| {
            let (x, y) = chunk_min_grid(chunk.key, chunk.origin);
            let cell = DVec2::new(x as f64, y as f64);
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
    // `--no-object-lod`: nothing is requested, hashed, loaded or drawn for object LOD.
    if config.no_object_lod {
        objects.suspend(&mut commands);
        return;
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
        match (response.result, response.atlas) {
            (Ok(chunks), _) if chunks.is_empty() => {}
            (Ok(_), Err(failure)) => {
                let reason = format!(
                    "object LOD is off for this pack: the shared atlas row is invalid: {}",
                    failure.reason
                );
                error!("{reason}");
                objects.failure = Some(reason);
            }
            (Ok(chunks), Ok(atlas)) => objects.start_checks(chunks, atlas, &config.assets_dir),
            (Err(failure), _) => {
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

    // The shared atlas is loaded once and kept, so every chunk that references it shares the image.
    if objects.atlas_handle.is_none()
        && let Some(atlas) = &objects.atlas
    {
        objects.atlas_handle = Some(asset_server.load(atlas.payload_path.clone()));
    }
    let camera = origin.0.as_dvec2() * f64::from(CELL_SIZE)
        + DVec2::new(
            f64::from(camera.translation.x),
            -f64::from(camera.translation.z),
        );
    let now = Instant::now();
    if objects.selection_changed(camera, &config.terrain_lod) {
        let wanted = wanted_object_chunks(
            &objects.chunks,
            camera,
            &config.terrain_lod,
            OBJECT_LOD_NAMA,
        )
        .into_iter()
        .collect();
        objects.select(&mut commands, wanted, now);
    }
    if budget.remaining == 0
        || runtime
            .as_deref()
            .is_some_and(|runtime| !runtime.cell_requests_allowed())
    {
        return;
    }
    let to_spawn: Vec<_> = objects
        .chunks
        .iter()
        .filter(|chunk| {
            objects.wanted.contains(&chunk.key)
                && !objects.roots.contains_key(&chunk.key)
                && !objects.failed_scenes.contains(&chunk.key)
        })
        .take(budget.remaining)
        .cloned()
        .collect();
    for chunk in to_spawn {
        budget.remaining -= 1;
        budget.commits = budget.commits.saturating_add(1);
        let handle: Handle<WorldAsset> =
            asset_server.load(GltfAssetLabel::Scene(0).from_asset(chunk.payload_path.clone()));
        let root = commands
            .spawn((object_root_bundle(&chunk, origin.0), WorldAssetRoot(handle)))
            .id();
        objects.roots.insert(
            chunk.key,
            ObjectRoot {
                entity: root,
                spawned: now,
                visible: false,
            },
        );
    }
}

/// Reconcile after Bevy spawns the scenes and before inherited visibility is propagated.
pub(super) fn update_object_visibility(
    mut commands: Commands,
    assets: Res<AssetServer>,
    scenes: Query<(&WorldAssetRoot, Has<ObjectSceneReady>)>,
    mut objects: ResMut<LodObjects>,
) {
    let now = Instant::now();
    let mut ready = HashSet::new();
    let mut failed = Vec::new();
    for (&key, root) in &objects.roots {
        if root.visible {
            continue;
        }
        let reason = match scenes.get(root.entity) {
            Ok((asset, spawned)) => match assets.get_load_states(asset.0.id()) {
                Some((LoadState::Failed(error), _, _))
                | Some((_, _, RecursiveDependencyLoadState::Failed(error))) => {
                    Some(error.to_string())
                }
                Some((LoadState::Loaded, _, RecursiveDependencyLoadState::Loaded)) if spawned => {
                    ready.insert(key);
                    continue;
                }
                _ => None,
            },
            Err(_) => Some("scene root is missing".into()),
        };
        let reason = reason.or_else(|| {
            (now.duration_since(root.spawned) >= HANDOVER_TIMEOUT)
                .then(|| "scene did not become ready before the timeout".into())
        });
        if let Some(reason) = reason {
            warn!(?key, %reason, "object LOD scene failed");
            commands.entity(root.entity).try_despawn();
            failed.push(key);
        }
    }
    for key in failed {
        objects.roots.remove(&key);
        objects.failed_scenes.insert(key);
    }
    objects.switch_ready_blocks(&mut commands, &ready, now);
}

/// A number in a node name, or `None` for a name that is not an object cell piece.
fn parse_number<T: std::str::FromStr>(text: &str) -> Option<T> {
    // silent-ok: a part that is not a number means the name is not an object cell piece.
    text.parse().ok()
}

/// The cell of an `object_cell_<x>_<y>_<n>` node name of a level-4 chunk.
pub fn object_cell_of_name(name: &str) -> Option<IVec2> {
    let rest = name.strip_prefix("object_cell_")?;
    let mut parts = rest.split('_');
    let x = parse_number::<i32>(parts.next()?)?;
    let y = parse_number::<i32>(parts.next()?)?;
    parse_number::<u32>(parts.next()?)?;
    match (parts.next(), parts.next()) {
        (None | Some("large"), None) => Some(IVec2::new(x, y)),
        _ => None,
    }
}

/// The cell a level-4 piece covers, on the node of a spawned object LOD scene.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LodObjectCell(pub(super) CellKey);

#[derive(Component)]
pub(super) struct LargeReferencePiece;

/// Tags only pieces inside their owning tier-4 chunk. Ordinary model scenes may use any name.
pub(super) fn tag_object_cells(
    mut commands: Commands,
    named: Query<(Entity, &Name), Added<Name>>,
    parents: Query<&ChildOf>,
    roots: Query<(&LodObjectRoot, &LodChunkGridOrigin)>,
) {
    for (entity, name) in &named {
        let Some(cell) = object_cell_of_name(name.as_str()) else {
            continue;
        };
        let Some((root, grid)) = parents.iter_ancestors(entity).find_map(|parent| {
            // silent-ok: ordinary scene ancestors do not carry these components.
            roots.get(parent).ok()
        }) else {
            continue;
        };
        let side = i64::from(root.key.tier.side_cells());
        let x = i64::from(cell.x) - grid.grid_x;
        let y = i64::from(cell.y) - grid.grid_y;
        if root.key.tier == LodTier::Tier4 && (0..side).contains(&x) && (0..side).contains(&y) {
            let mut piece = commands.entity(entity);
            piece.insert(LodObjectCell(CellKey::Exterior {
                worldspace_id: root.key.worldspace_id,
                grid_x: cell.x,
                grid_y: cell.y,
            }));
            if name.ends_with("_large") {
                piece.insert(LargeReferencePiece);
            }
        }
    }
}

/// Use actual cell roots and their model readiness. Retiring roots still draw until despawn.
#[allow(clippy::type_complexity)]
pub(super) fn hide_loaded_object_cells(
    streaming: Res<StreamingWorld>,
    children: Query<&Children>,
    pending: Query<(), Or<(With<PendingModel>, With<PendingAssetProfile>)>>,
    large: Option<Res<LargeReferences>>,
    mut pieces: Query<(&LodObjectCell, Has<LargeReferencePiece>, &mut Visibility)>,
    mut ready: Local<HashMap<CellKey, bool>>,
) {
    ready.clear();
    for (cell, marked, mut visibility) in &mut pieces {
        let loaded = *ready.entry(cell.0).or_insert_with(|| {
            let Some(CellStatus::Resident { root } | CellStatus::Retiring { root }) =
                streaming.cells.get(&cell.0)
            else {
                return false;
            };
            !std::iter::once(*root)
                .chain(children.iter_descendants(*root))
                .any(|entity| pending.contains(entity))
        });
        // The tier-4 tag proves node overlap when its cell is inside the active square.
        let wanted =
            if loaded || (marked && large.as_ref().is_some_and(|large| large.suppresses(cell.0))) {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            };
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::database::LodChunkBounds;
    use shared::lod::{ChunkAnchor, LodOrigin};

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
        let camera = DVec2::new(5.5, -11.5) * f64::from(CELL_SIZE);
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
    fn the_selection_follows_distance_changes_and_movement_within_a_cell() {
        let mut objects = LodObjects::default();
        let distances = TerrainLodDistances::default();
        let camera = DVec2::new(5.5, -11.5) * f64::from(CELL_SIZE);
        assert!(
            objects.selection_changed(camera, &distances),
            "first selection"
        );
        assert!(
            !objects.selection_changed(camera, &distances),
            "same position and distances keep the selection"
        );
        // A distance change re-selects although the camera stands still.
        let nearer = TerrainLodDistances {
            block_level1_distance: 30_000.0,
            ..distances
        };
        assert!(objects.selection_changed(camera, &nearer));
        assert!(!objects.selection_changed(camera, &nearer));
        assert!(objects.selection_changed(camera + DVec2::X, &nearer));
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
            wanted_object_chunks(&[], DVec2::ZERO, &TerrainLodDistances::default(), 1.0).is_empty()
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
    fn every_tier_is_selected_by_the_object_rule() {
        let distances = TerrainLodDistances::default();
        let camera = DVec2::new(5.5, -11.5) * f64::from(CELL_SIZE);
        // The object rule picks tier 8 for cell (24, -16) and tier 4 for cell (8, -12) from this
        // camera, and tier 16 for the block of chunk (4, 4).
        let tier8 = chunk_of(LodTier::Tier8, 15, 10);
        let tier4 = chunk_of(LodTier::Tier4, 26, 21);
        for (row, tier, cell) in [
            (&tier8, LodTier::Tier8, IVec2::new(24, -16)),
            (&tier4, LodTier::Tier4, IVec2::new(8, -12)),
        ] {
            assert_eq!(
                distances.object_tier(camera, cell.as_dvec2(), IVec2::new(-96, -96), 1.0),
                Some(tier)
            );
            assert_eq!(
                chunk_min_grid(row.key, row.origin),
                (i64::from(cell.x), i64::from(cell.y))
            );
        }
        let rows = [tier8, tier4, chunk(4, 4)];
        let wanted = wanted_object_chunks(&rows, camera, &distances, OBJECT_LOD_NAMA);
        assert_eq!(wanted, rows.iter().map(|row| row.key).collect::<Vec<_>>());
        // A tier-8 chunk beside the camera is not wanted: the rule picks tier 4 there.
        let near8 = chunk_of(LodTier::Tier8, 13, 10);
        assert!(wanted_object_chunks(&[near8], camera, &distances, OBJECT_LOD_NAMA).is_empty());
    }

    #[test]
    fn object_cell_tags_require_a_tier_four_owner_and_valid_widened_coordinates() {
        let mut app = App::new();
        app.add_systems(Update, tag_object_cells);
        let root = app
            .world_mut()
            .spawn(object_root_bundle(
                &chunk_of(LodTier::Tier4, 25, 21),
                IVec2::ZERO,
            ))
            .id();
        let layer = app.world_mut().spawn(ChildOf(root)).id();
        let tagged = app
            .world_mut()
            .spawn((Name::new("object_cell_5_-12_0"), ChildOf(layer)))
            .id();
        let unrelated = app.world_mut().spawn(Name::new("object_cell_5_-12_0")).id();
        let extreme = app
            .world_mut()
            .spawn((Name::new("object_cell_-2147483648_0_0"), ChildOf(root)))
            .id();
        let outside = app
            .world_mut()
            .spawn((Name::new("object_cell_8_-12_0"), ChildOf(root)))
            .id();
        let malformed = app
            .world_mut()
            .spawn((Name::new("object_cell_5_x_3"), ChildOf(root)))
            .id();
        app.update();
        assert_eq!(
            app.world().get(tagged),
            Some(&LodObjectCell(CellKey::Exterior {
                worldspace_id: 60,
                grid_x: 5,
                grid_y: -12,
            }))
        );
        for invalid in [unrelated, extreme, outside, malformed] {
            assert!(app.world().get::<LodObjectCell>(invalid).is_none());
        }
    }

    #[test]
    fn object_cells_follow_model_readiness_and_actual_unload() {
        let mut app = App::new();
        app.init_resource::<StreamingWorld>()
            .add_systems(Update, hide_loaded_object_cells);
        let piece = app
            .world_mut()
            .spawn((
                LodObjectCell(CellKey::Exterior {
                    worldspace_id: 60,
                    grid_x: 5,
                    grid_y: -12,
                }),
                Visibility::Inherited,
            ))
            .id();
        let root = app.world_mut().spawn_empty().id();
        let model = app
            .world_mut()
            .spawn((
                ChildOf(root),
                PendingModel {
                    handle: Handle::default(),
                    sequence: 0,
                },
            ))
            .id();
        let key = app.world().get::<LodObjectCell>(piece).unwrap().0;
        let set_status = |app: &mut App, status| {
            app.world_mut()
                .resource_mut::<StreamingWorld>()
                .cells
                .insert(key, status);
        };
        let check = |app: &mut App, visibility| {
            app.update();
            assert_eq!(app.world().get(piece), Some(&visibility));
        };
        set_status(&mut app, CellStatus::Loading { generation: 0 });
        check(&mut app, Visibility::Inherited);
        set_status(&mut app, CellStatus::Resident { root });
        check(&mut app, Visibility::Inherited);
        app.world_mut()
            .entity_mut(model)
            .remove::<PendingModel>()
            .insert(PendingAssetProfile::test_actor());
        check(&mut app, Visibility::Inherited);
        app.world_mut()
            .entity_mut(model)
            .remove::<PendingAssetProfile>();
        check(&mut app, Visibility::Hidden);
        set_status(&mut app, CellStatus::Retiring { root });
        check(&mut app, Visibility::Hidden);
        app.world_mut().despawn(root);
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .remove(&key);
        check(&mut app, Visibility::Inherited);
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
        .init_asset::<Image>()
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
            self.answer_with_atlas(chunks, Ok(None), generation_offset);
        }

        fn answer_with_atlas(
            &mut self,
            chunks: Vec<LodChunkMetadata>,
            atlas: std::result::Result<
                Option<LodObjectAtlas>,
                crate::world::database::LodQueryFailure,
            >,
            generation_offset: u64,
        ) {
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
                    atlas,
                })
                .unwrap();
        }

        /// Writes `bytes` as the worldspace's shared atlas and returns its row.
        fn write_atlas(&self, bytes: &[u8]) -> LodObjectAtlas {
            let payload_path = shared::lod::object_atlas_path(60);
            let path = self.assets.path().join(&payload_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
            LodObjectAtlas {
                worldspace_id: 60,
                payload_path,
                content_hash: Sha256::digest(bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            }
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
    fn rows_of_every_tier_are_checked_and_a_damaged_one_fails_the_pack() {
        let mut rig = rig(false);
        let mut drawable = chunk(4, 4);
        rig.write_payload(&mut drawable, b"glb");
        // A tier-8 row whose file is missing: every tier is checked, so object LOD goes off.
        let missing = chunk_of(LodTier::Tier8, 15, 10);
        rig.answer(vec![drawable, missing], 0);
        rig.run_until("the failure", |rig| rig.objects().failure().is_some());
        assert_eq!(rig.root_count(), 0);
    }

    #[test]
    fn a_verified_shared_atlas_is_loaded_once_and_chunks_draw() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        let atlas = rig.write_atlas(b"ktx2 bytes");
        rig.answer_with_atlas(vec![chunk], Ok(Some(atlas)), 0);
        rig.run_until("the root", |rig| rig.objects().resident() == 1);
        assert!(rig.objects().failure().is_none());
        assert!(
            rig.objects().atlas_handle.is_some(),
            "the atlas is held, loaded once for every chunk"
        );
    }

    #[test]
    fn a_missing_shared_atlas_turns_object_lod_off_once_with_a_clear_message() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        let atlas = rig.write_atlas(b"ktx2 bytes");
        std::fs::remove_file(rig.assets.path().join(&atlas.payload_path)).unwrap();
        rig.answer_with_atlas(vec![chunk], Ok(Some(atlas)), 0);
        rig.run_until("the failure", |rig| rig.objects().failure().is_some());
        let reason = rig.objects().failure().unwrap().to_owned();
        assert!(
            reason.contains("shared object LOD atlas") && reason.contains("integrity"),
            "{reason}"
        );
        assert_eq!(
            rig.root_count(),
            0,
            "the chunks do not draw without their atlas"
        );
        assert!(rig.objects().atlas_handle.is_none());
    }

    #[test]
    fn a_damaged_shared_atlas_fails_closed_with_its_hash() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        let atlas = rig.write_atlas(b"ktx2 bytes");
        std::fs::write(rig.assets.path().join(&atlas.payload_path), b"damaged").unwrap();
        rig.answer_with_atlas(vec![chunk], Ok(Some(atlas)), 0);
        rig.run_until("the failure", |rig| rig.objects().failure().is_some());
        assert!(rig.objects().failure().unwrap().contains("SHA-256"));
        assert_eq!(rig.root_count(), 0);
    }

    #[test]
    fn an_invalid_atlas_row_turns_object_lod_off_too() {
        let mut rig = rig(false);
        let mut chunk = chunk(4, 4);
        rig.write_payload(&mut chunk, b"glb");
        rig.answer_with_atlas(
            vec![chunk],
            Err(crate::world::database::LodQueryFailure {
                transient: false,
                reason: "non-canonical path".into(),
            }),
            0,
        );
        rig.run_until("the failure", |rig| rig.objects().failure().is_some());
        assert!(
            rig.objects()
                .failure()
                .unwrap()
                .contains("atlas row is invalid")
        );
    }

    #[test]
    fn the_no_object_lod_flag_reads_hashes_and_draws_nothing() {
        let mut rig = rig(false);
        rig.app
            .world_mut()
            .resource_mut::<EngineConfig>()
            .no_object_lod = true;
        for _ in 0..5 {
            rig.app.update();
        }
        assert!(
            rig.requests.try_recv().is_err(),
            "the rows are not even requested"
        );
        assert_eq!(rig.root_count(), 0);
        assert!(rig.objects().failure().is_none());
        // Turning it off again starts the normal request.
        rig.app
            .world_mut()
            .resource_mut::<EngineConfig>()
            .no_object_lod = false;
        rig.app.update();
        assert!(matches!(
            rig.requests.try_recv(),
            Ok(DatabaseRequest::LoadLodObjectChunks { .. })
        ));
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
        // The uninstantiated root was never visible and is dropped with its request.
        assert_eq!(rig.root_count(), 0, "the entity is despawned");
        rig.move_camera(RIVERWOOD);
        rig.app.update();
        assert_eq!(rig.root_count(), 1);
    }

    fn transition_root(world: &mut World, now: Instant, visible: bool) -> ObjectRoot {
        let visibility = if visible {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        ObjectRoot {
            entity: world.spawn(visibility).id(),
            spawned: now,
            visible,
        }
    }

    #[test]
    fn split_waits_for_every_sibling_including_chunks_deferred_by_budget() {
        let mut world = World::new();
        let mut objects = LodObjects::default();
        let now = Instant::now();
        let parent = chunk_of(LodTier::Tier8, -1, 2).key;
        let children = [
            chunk_of(LodTier::Tier4, -2, 4).key,
            chunk_of(LodTier::Tier4, -1, 4).key,
        ];
        let old = transition_root(&mut world, now, true);
        objects.roots.insert(parent, old);
        objects.select(&mut world.commands(), children.into_iter().collect(), now);
        let first = transition_root(&mut world, now, false);
        objects.roots.insert(children[0], first);
        let mut ready = HashSet::from([children[0]]);
        objects.switch_ready_blocks(&mut world.commands(), &ready, now);
        world.flush();
        assert_eq!(world.get(first.entity), Some(&Visibility::Hidden));
        assert_eq!(world.get(old.entity), Some(&Visibility::Inherited));

        let second = transition_root(&mut world, now, false);
        objects.roots.insert(children[1], second);
        ready.insert(children[1]);
        objects.switch_ready_blocks(&mut world.commands(), &ready, now);
        world.flush();
        assert!(world.get_entity(old.entity).is_err());
        assert_eq!(world.get(first.entity), Some(&Visibility::Inherited));
        assert_eq!(world.get(second.entity), Some(&Visibility::Inherited));
    }

    #[test]
    fn merge_reveals_the_parent_in_the_same_pass_that_removes_the_children() {
        let mut world = World::new();
        let mut objects = LodObjects::default();
        let now = Instant::now();
        let parent = chunk_of(LodTier::Tier8, 1, 2).key;
        let children = [
            chunk_of(LodTier::Tier4, 2, 4).key,
            chunk_of(LodTier::Tier4, 3, 4).key,
        ];
        let outgoing = children.map(|key| {
            let root = transition_root(&mut world, now, true);
            objects.roots.insert(key, root);
            root.entity
        });
        objects.select(&mut world.commands(), HashSet::from([parent]), now);
        let replacement = transition_root(&mut world, now, false);
        objects.roots.insert(parent, replacement);
        objects.switch_ready_blocks(&mut world.commands(), &HashSet::new(), now);
        world.flush();
        assert_eq!(world.get(replacement.entity), Some(&Visibility::Hidden));
        for old in outgoing {
            assert_eq!(world.get(old), Some(&Visibility::Inherited));
        }
        objects.switch_ready_blocks(&mut world.commands(), &HashSet::from([parent]), now);
        world.flush();
        assert_eq!(world.get(replacement.entity), Some(&Visibility::Inherited));
        assert!(outgoing.iter().all(|&old| world.get_entity(old).is_err()));
    }

    #[test]
    fn failed_and_stalled_replacements_cannot_hold_the_outgoing_block() {
        for failed in [true, false] {
            let mut world = World::new();
            let mut objects = LodObjects::default();
            let now = Instant::now();
            let parent = chunk_of(LodTier::Tier8, 0, 0).key;
            let replacement = chunk_of(LodTier::Tier4, 0, 0).key;
            let old = transition_root(&mut world, now, true);
            objects.roots.insert(parent, old);
            objects.select(&mut world.commands(), HashSet::from([replacement]), now);
            objects.switch_ready_blocks(&mut world.commands(), &HashSet::new(), now);
            world.flush();
            assert!(world.get_entity(old.entity).is_ok());

            if failed {
                objects.failed_scenes.insert(replacement);
            }
            objects.switch_ready_blocks(&mut world.commands(), &HashSet::new(), now);
            world.flush();
            assert!(world.get_entity(old.entity).is_ok());

            objects.switch_ready_blocks(
                &mut world.commands(),
                &HashSet::new(),
                now + HANDOVER_TIMEOUT,
            );
            world.flush();
            assert!(world.get_entity(old.entity).is_err());
        }
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
