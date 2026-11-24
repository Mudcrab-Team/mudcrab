//! Exterior large-reference promotion and the width-4 object LOD deadline.
//! Retail facts G02–G04: https://github.com/Mudcrab-Team/skyrim_decomp/pull/821.

use super::{
    ActiveSpace, CellStatus, PendingAssetProfile, PendingModel, RenderOrigin, StreamingMetrics,
    StreamingWorld, cell_order_key, cell_translation,
    lod_objects::{LargeReferenceLod, LargeReferencePiece, LodObjectCell, LodObjects},
    requests,
};
use crate::{
    config::EngineConfig,
    profiling::ProfilingState,
    render::PLACED_OBJECT_RENDER_LAYERS,
    world::{
        actor_catalog::ActorCatalog,
        components::{CellRef, ExteriorCellGrid, FormId, MeshHandle, WorldTransform},
        database::{
            AssetCatalog, CellKey, DatabaseRequest, LargeReferenceRows, ReferenceRow,
            WorldDatabase, large_reference_in_bucket,
        },
    },
};
use bevy::{app::Propagate, gltf::GltfAssetLabel, prelude::*};
use crossbeam_channel::{Receiver, TryRecvError, bounded};
use shared::world_assets::{StreamedModelExclusion, classify_streamed_model_path};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

// The grid width is `EngineConfig::large_ref_grid_size`: `[General] uLargeRefLODGridSize`, 11 in
// `Ultra.ini` (#949), overridden by `--ini`. STAND-IN(#913): Mudcrab has no reviewed mapping from
// retail's Low, Medium, High and Ultra presets to the other widths, so every profile uses the
// configured width.
const DEMOTIONS_PER_FRAME: usize = 1;
const LOD_DELAY: Duration = Duration::from_millis(2_000);
// STAND-IN(#913): Bound admission by one large window, including roots waiting to retire.
fn max_buckets(grid_radius: i32) -> usize {
    let width = 2 * grid_radius.unsigned_abs() as usize + 1;
    width * width
}

type Response = Result<LargeReferenceRows, String>;

struct PendingBucket {
    response: Receiver<Response>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for PendingBucket {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

enum Cell {
    Loading(PendingBucket),
    Promoted {
        root: Entity,
        models: usize,
        deadline: Option<Duration>,
    },
    Failed,
}

impl Cell {
    fn despawn(self, commands: &mut Commands) {
        if let Self::Promoted { root, .. } = self {
            commands.entity(root).try_despawn();
        }
    }
}

/// Set after the existing model loader instantiates and validates a nonempty full model.
#[derive(Component)]
pub(super) struct LargeReferenceModel {
    pub ready: bool,
}

#[derive(Component)]
pub(super) struct OrdinaryLargeReferences {
    expected: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
enum MemberModel {
    Draw(String),
    Disabled,
    EditorMarker,
    Effects,
    Sky,
    Unknown,
}

fn member_model(row: &ReferenceRow) -> MemberModel {
    if row.enable_state_unresolved {
        return MemberModel::Unknown;
    }
    if !row.initially_enabled {
        return MemberModel::Disabled;
    }
    if row.base_is_marker {
        return MemberModel::EditorMarker;
    }
    match row.model_path.as_deref() {
        Some(path) => match classify_streamed_model_path(path) {
            Ok(model) => MemberModel::Draw(model),
            Err(StreamedModelExclusion::Effects) => MemberModel::Effects,
            Err(StreamedModelExclusion::Sky) => MemberModel::Sky,
            Err(StreamedModelExclusion::Other) => MemberModel::Unknown,
        },
        None => MemberModel::Unknown,
    }
}

#[derive(Default)]
struct KnownSkips {
    editor_markers: usize,
    effects: usize,
    sky: usize,
}

impl KnownSkips {
    fn record(&mut self, model: &MemberModel) {
        match model {
            MemberModel::EditorMarker => self.editor_markers += 1,
            MemberModel::Effects => self.effects += 1,
            MemberModel::Sky => self.sky += 1,
            _ => {}
        }
    }

    fn log(&self, key: CellKey, representation: &str) {
        let Self {
            editor_markers,
            effects,
            sky,
        } = *self;
        if editor_markers + effects + sky > 0 {
            info!(
                ?key,
                representation,
                editor_markers,
                effects,
                sky,
                "large-reference known non-drawing placements"
            );
        }
    }
}

pub(super) fn ordinary_members(
    key: CellKey,
    members: &HashSet<u32>,
    rows: &[ReferenceRow],
    actors: &ActorCatalog,
) -> OrdinaryLargeReferences {
    let CellKey::Exterior { grid_x, grid_y, .. } = key else {
        warn!(
            ?key,
            "ordinary large-reference bucket has no exterior coordinates"
        );
        return OrdinaryLargeReferences { expected: None };
    };
    let (mut resolved, mut enabled) = (0, 0);
    let mut skips = KnownSkips::default();
    for row in rows.iter().filter(|row| members.contains(&row.form_id)) {
        if !row.enable_state_unresolved && large_reference_in_bucket(row, grid_x, grid_y) {
            resolved += 1;
            if !row.initially_enabled {
                continue;
            }
            // The ordinary actor path can draw a composed body without the reference's mesh.
            if actors.get(row.base_form_id).is_some() {
                enabled += 1;
                continue;
            }
            let model = member_model(row);
            skips.record(&model);
            enabled += usize::from(matches!(model, MemberModel::Draw(_) | MemberModel::Unknown));
        }
    }
    skips.log(key, "ordinary");
    let missing = members.len().saturating_sub(resolved);
    if missing != 0 {
        warn!(?key, missing, "ordinary large-reference placements omitted");
    }
    OrdinaryLargeReferences {
        expected: (missing == 0).then_some(enabled),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Lod,
    Ordinary,
    Promoted,
}

#[derive(Resource)]
pub(super) struct LargeReferences {
    cells: HashMap<CellKey, Cell>,
    window: Option<(u32, IVec2)>,
    now: Duration,
    owners: HashMap<CellKey, Owner>,
    held_ordinary: HashSet<CellKey>,
    ordinary_radius: i32,
    /// Cells from the camera's cell to the edge of the large-reference grid, from
    /// `EngineConfig::large_ref_grid_radius`. `update` refreshes it every frame.
    grid_radius: i32,
}

impl Default for LargeReferences {
    fn default() -> Self {
        Self {
            cells: HashMap::new(),
            window: None,
            now: Duration::ZERO,
            owners: HashMap::new(),
            held_ordinary: HashSet::new(),
            ordinary_radius: 0,
            grid_radius: crate::config::ULTRA_LARGE_REF_LOD_GRID_SIZE / 2,
        }
    }
}

fn in_window(key: CellKey, window: Option<(u32, IVec2)>, grid_radius: i32) -> bool {
    let (
        CellKey::Exterior {
            worldspace_id,
            grid_x,
            grid_y,
        },
        Some((world, center)),
    ) = (key, window)
    else {
        return false;
    };
    worldspace_id == world
        && grid_x.abs_diff(center.x) <= grid_radius.unsigned_abs()
        && grid_y.abs_diff(center.y) <= grid_radius.unsigned_abs()
}

pub(super) fn window_enabled(ordinary_radius: i32, grid_radius: i32) -> bool {
    ordinary_radius < grid_radius
}

impl LargeReferences {
    fn demotion(&self) -> Option<CellKey> {
        // STAND-IN(#913): The capped retail path is known. Its selection order is unverified.
        self.cells
            .keys()
            .copied()
            .filter(|&key| {
                !in_window(key, self.window, self.grid_radius) && self.owner(key) != Owner::Promoted
            })
            .min_by_key(|&key| cell_order_key(key))
    }

    fn owner(&self, key: CellKey) -> Owner {
        self.owners.get(&key).copied().unwrap_or(Owner::Lod)
    }

    pub(super) fn marked_visibility(&self, key: CellKey) -> Option<Visibility> {
        self.owners.get(&key).map(|owner| match owner {
            Owner::Lod => Visibility::Inherited,
            Owner::Ordinary | Owner::Promoted => Visibility::Hidden,
        })
    }

    pub(super) fn holds_ordinary(&self, key: CellKey) -> bool {
        self.held_ordinary.contains(&key)
    }

    fn cancel_outside(&mut self, commands: &mut Commands) {
        let (window, grid_radius) = (self.window, self.grid_radius);
        self.cells.retain(|&key, cell| {
            if in_window(key, window, grid_radius)
                || matches!(
                    cell,
                    Cell::Promoted {
                        deadline: Some(_),
                        ..
                    }
                )
            {
                return true;
            }
            // A root that never drew is still loading. Drop its model requests with the root.
            if let Cell::Promoted { root, .. } = cell {
                commands.entity(*root).try_despawn();
            }
            false
        });
    }

    fn can_admit(&self) -> bool {
        self.cells.len() < max_buckets(self.grid_radius)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn update(
    mut commands: Commands,
    config: Res<EngineConfig>,
    catalog: Res<AssetCatalog>,
    database: Res<WorldDatabase>,
    space: Res<ActiveSpace>,
    origin: Res<RenderOrigin>,
    asset_server: Res<AssetServer>,
    mut streaming: ResMut<StreamingWorld>,
    mut large: ResMut<LargeReferences>,
    mut metrics: ResMut<StreamingMetrics>,
    mut profiler: ResMut<ProfilingState>,
) {
    let world = space.exterior_worldspace(config.worldspace_id);
    let grid_radius = config.large_ref_grid_radius();
    large.grid_radius = grid_radius;
    // STAND-IN(#913): Mudcrab uses its camera-planned grid. Retail's normal caller uses the player.
    let window = (space.interior.is_none() && window_enabled(config.stream_radius, grid_radius))
        .then_some(streaming.planned_center)
        .flatten()
        .map(|center| (world, center));
    if large.window.map(|(world, _)| world) != window.map(|(world, _)| world) {
        for (_, cell) in large.cells.drain() {
            cell.despawn(&mut commands);
        }
        large.owners.clear();
        large.held_ordinary.clear();
    }
    large.window = window;
    large.ordinary_radius = config.stream_radius;
    let Some((world, center)) = window else {
        metrics.loading_large_reference_cells = 0;
        return;
    };

    large.cancel_outside(&mut commands);
    // STAND-IN(#913): Mudcrab applies the known cap to all same-worldspace grid moves.
    for _ in 0..DEMOTIONS_PER_FRAME {
        if let Some(key) = large.demotion()
            && let Some(cell) = large.cells.remove(&key)
        {
            cell.despawn(&mut commands);
        }
    }
    let mut outstanding = 0;
    for dx in -grid_radius..=grid_radius {
        for dy in -grid_radius..=grid_radius {
            let (Some(grid_x), Some(grid_y)) = (center.x.checked_add(dx), center.y.checked_add(dy))
            else {
                continue;
            };
            let key = CellKey::Exterior {
                worldspace_id: world,
                grid_x,
                grid_y,
            };
            if !catalog.large_references.members.contains_key(&key) {
                continue;
            }
            let ordinary = dx.abs() <= config.stream_radius && dy.abs() <= config.stream_radius;
            // STAND-IN(#913): Ordinary cells source their own models. Start the separate sweep
            // in the outer ring and keep a visible root until its ordinary replacement can draw.
            if ordinary {
                if large.owner(key) != Owner::Promoted
                    && let Some(cell) = large.cells.remove(&key)
                {
                    cell.despawn(&mut commands);
                }
                continue;
            }
            let can_admit = large.can_admit();
            match large.cells.entry(key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    if !can_admit {
                        outstanding += 1;
                        continue;
                    }
                    let (reply, response) = bounded(1);
                    let cancelled = Arc::new(AtomicBool::new(false));
                    if let Err(reason) =
                        database.try_request(DatabaseRequest::LoadLargeReferences {
                            key,
                            reply,
                            omissions: catalog.large_references.omissions(key),
                            cancelled: cancelled.clone(),
                        })
                    {
                        // The ordinary worker queue can fill. Keep the cell eligible for retry.
                        debug!(?key, %reason, "large-reference request deferred");
                    } else {
                        entry.insert(Cell::Loading(PendingBucket {
                            response,
                            cancelled,
                        }));
                    }
                    outstanding += 1;
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let Cell::Loading(reply) = entry.get() else {
                        continue;
                    };
                    let response = match reply.response.try_recv() {
                        Ok(response) => response,
                        Err(TryRecvError::Empty) => {
                            outstanding += 1;
                            continue;
                        }
                        Err(TryRecvError::Disconnected) => {
                            Err("database worker closed the response".into())
                        }
                    };
                    let cell = match response {
                        Ok(rows) => spawn_cell(
                            &mut commands,
                            &config,
                            &asset_server,
                            &catalog,
                            origin.0,
                            key,
                            rows,
                            &mut streaming,
                            &mut profiler,
                        ),
                        Err(reason) => {
                            metrics.failed_cells = metrics.failed_cells.saturating_add(1);
                            warn!(?key, %reason, "large-reference cell omitted");
                            Cell::Failed
                        }
                    };
                    entry.insert(cell);
                }
            }
        }
    }
    metrics.loading_large_reference_cells = outstanding;
}

#[allow(clippy::too_many_arguments)]
fn spawn_cell(
    commands: &mut Commands,
    config: &EngineConfig,
    asset_server: &AssetServer,
    catalog: &AssetCatalog,
    origin: IVec2,
    key: CellKey,
    rows: LargeReferenceRows,
    streaming: &mut StreamingWorld,
    profiler: &mut ProfilingState,
) -> Cell {
    let CellKey::Exterior { grid_x, grid_y, .. } = key else {
        warn!(?key, "large-reference cell has no exterior coordinates");
        return Cell::Failed;
    };
    let mut missing = rows.missing;
    let mut placements = Vec::new();
    let mut skips = KnownSkips::default();
    for reference in rows.references {
        match member_model(&reference) {
            MemberModel::Draw(path) => placements.push((reference, path)),
            MemberModel::Unknown => missing += 1,
            model => skips.record(&model),
        }
    }
    skips.log(key, "promoted");
    if missing != 0 {
        warn!(
            ?key,
            missing, "large-reference placements omitted; keeping object LOD for the bucket"
        );
        return Cell::Failed;
    }
    let models = placements.len();
    let root = commands
        .spawn((
            Name::new("Large-reference cell"),
            ExteriorCellGrid(IVec2::new(grid_x, grid_y)),
            Transform::from_translation(cell_translation(key, origin)),
            Visibility::Hidden,
        ))
        .id();
    commands.entity(root).with_children(|parent| {
        for (reference, path) in placements {
            let (world_position, transform, model_bounds, bounds) =
                super::reference_placement(key, &reference);
            let mut entity = parent.spawn((
                Name::new(format!("Large reference {:08X}", reference.form_id)),
                FormId(reference.form_id),
                CellRef(reference.cell_id),
                world_position,
                WorldTransform(transform.to_matrix()),
                transform,
                Propagate(PLACED_OBJECT_RENDER_LAYERS),
                LargeReferenceModel { ready: false },
            ));
            if let Some(bounds) = bounds.zip(model_bounds) {
                entity.insert(bounds);
            }
            if let Some(rgb) = reference.emittance {
                entity.insert(crate::emittance::ReferenceEmittance::from_srgb(rgb));
            }
            if let Some(swaps) = catalog.object_texture_swaps(reference.base_form_id) {
                entity.insert(swaps);
            }
            let sequence = streaming.next_model_sequence;
            streaming.next_model_sequence = sequence.saturating_add(1);
            if requests::enabled(config) {
                let (center, radius) =
                    requests::reference_geometry(world_position, &transform, bounds);
                entity.insert(requests::SceneRequest {
                    path: path.clone(),
                    // The promoted root owns this request outside the ordinary resident map.
                    cell: None,
                    coarse_terrain: false,
                    retry_cleanup: None,
                    content_identity: None,
                    sequence,
                    center,
                    radius,
                    collision_candidate: false,
                    handle: None,
                    started: Instant::now(),
                    allow_retry: false,
                });
            } else {
                let handle = asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.clone()));
                profiler.observe_scene(handle.id(), asset_server);
                entity.insert(PendingModel { handle, sequence });
            }
            entity.insert((
                MeshHandle(path.clone()),
                PendingAssetProfile {
                    started: Instant::now(),
                    scene_spawned: false,
                    path,
                    form_id: reference.form_id,
                    base_form_id: reference.base_form_id,
                    base_record_type: reference.base_record_type,
                    static_physics: false,
                    cell_id: reference.cell_id,
                    actor_scene: false,
                },
            ));
        }
    });
    debug!(?key, models, "large-reference models queued");
    Cell::Promoted {
        root,
        models,
        deadline: None,
    }
}

#[derive(Clone, Copy)]
struct Handoff {
    near: bool,
    in_large_grid: bool,
    ordinary_ready: bool,
    ordinary_drawable: bool,
    ordinary_resident: bool,
    promoted_ready: bool,
    promotion_pending: bool,
    incomplete: bool,
    lod: LargeReferenceLod,
}

impl Handoff {
    // STAND-IN(#913): Ownership waits for a drawable replacement. Retail attachment, refresh
    // and release timing are unverified.
    fn owner(self, previous: Owner) -> Owner {
        if self.lod == LargeReferenceLod::Coarse
            || (self.incomplete && self.lod == LargeReferenceLod::Segmented)
        {
            return Owner::Lod;
        }
        if self.near && self.ordinary_ready {
            return Owner::Ordinary;
        }
        if self.in_large_grid
            && !self.near
            && !self.incomplete
            && (previous == Owner::Promoted
                || (self.promoted_ready && self.lod != LargeReferenceLod::Pending))
        {
            return Owner::Promoted;
        }
        if self.ordinary_ready
            && (self.ordinary_resident
                || (previous == Owner::Ordinary
                    && (self.lod == LargeReferenceLod::Pending
                        || (self.promotion_pending && self.lod == LargeReferenceLod::Absent))))
        {
            return Owner::Ordinary;
        }
        if previous == Owner::Promoted && (self.near || self.lod == LargeReferenceLod::Pending) {
            return Owner::Promoted;
        }
        if self.ordinary_drawable
            && self.ordinary_resident
            && (self.lod == LargeReferenceLod::Absent
                || (previous == Owner::Ordinary && self.lod == LargeReferenceLod::Pending))
        {
            return Owner::Ordinary;
        }
        Owner::Lod
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn resolve_visibility(
    mut large: ResMut<LargeReferences>,
    streaming: Res<StreamingWorld>,
    lod: Res<LodObjects>,
    catalog: Res<AssetCatalog>,
    children: Query<&Children>,
    ordinary_buckets: Query<&OrdinaryLargeReferences>,
    pending: Query<(), Or<(With<PendingModel>, With<PendingAssetProfile>)>>,
    marked_pieces: Query<(&LodObjectCell, &LargeReferencePiece)>,
    mut models: Query<(&LargeReferenceModel, &mut Visibility), With<LargeReferenceModel>>,
    mut roots: Query<&mut Visibility, Without<LargeReferenceModel>>,
    mut scratch: Local<(HashSet<CellKey>, HashSet<CellKey>)>,
) {
    if large.window.is_none() {
        return;
    }
    let (keys, drawable_lod) = &mut *scratch;
    keys.clear();
    keys.extend(large.cells.keys().copied());
    keys.extend(
        streaming
            .cells
            .keys()
            .filter(|key| {
                catalog.large_references.members.contains_key(key)
                    || catalog.large_references.omissions(**key) != 0
            })
            .copied(),
    );
    large.owners.retain(|key, _| keys.contains(key));
    drawable_lod.clear();
    drawable_lod.extend(marked_pieces.iter().filter_map(|(cell, piece)| {
        (keys.contains(&cell.0) && lod.marked_piece_ready(piece)).then_some(cell.0)
    }));
    large.held_ordinary.clear();
    for &key in &*keys {
        let (ordinary_root, ordinary_resident) = match streaming.cells.get(&key) {
            Some(CellStatus::Resident { root }) => (Some(*root), true),
            Some(CellStatus::Retiring { root }) => (Some(*root), false),
            _ => (None, false),
        };
        let children_of = |root| {
            children
                .get(root)
                .into_iter()
                .flat_map(|children| children.iter())
        };
        let (promoted_root, deadline, promotion_pending) = match large.cells.get(&key) {
            Some(Cell::Promoted { root, deadline, .. }) => (
                Some(*root),
                *deadline,
                deadline.is_some() || children_of(*root).any(|entity| pending.contains(entity)),
            ),
            Some(Cell::Loading(_)) | None => (None, None, true),
            Some(Cell::Failed) => (None, None, false),
        };
        let (ready_count, ordinary_pending) = ordinary_root
            .into_iter()
            .flat_map(children_of)
            .filter_map(|entity| {
                // silent-ok: terrain, water and ordinary models have no RNAM marker.
                let (model, _) = models.get(entity).ok()?;
                Some((model.ready, pending.contains(entity)))
            })
            .fold((0, false), |(count, pending), (ready, loading)| {
                (count + usize::from(ready), pending || loading)
            });
        // silent-ok: an ordinary root without known membership has no bucket component.
        let ordinary_bucket = ordinary_root.and_then(|root| ordinary_buckets.get(root).ok());
        let expected_ordinary = ordinary_bucket.and_then(|bucket| bucket.expected);
        let ordinary_ready = ready_count > 0 && expected_ordinary == Some(ready_count);
        let near = match (key, large.window) {
            (
                CellKey::Exterior {
                    worldspace_id,
                    grid_x,
                    grid_y,
                },
                Some((world, center)),
            ) => {
                worldspace_id == world
                    && grid_x.abs_diff(center.x) <= large.ordinary_radius.unsigned_abs()
                    && grid_y.abs_diff(center.y) <= large.ordinary_radius.unsigned_abs()
            }
            _ => false,
        };
        let in_large_grid = in_window(key, large.window, large.grid_radius);
        let ordinary_incomplete = near
            && ordinary_bucket.is_some()
            && (expected_ordinary.is_none()
                || (expected_ordinary.is_some_and(|expected| ready_count < expected)
                    && !ordinary_pending));
        let handoff = Handoff {
            near,
            in_large_grid,
            ordinary_ready,
            ordinary_drawable: ready_count > 0,
            ordinary_resident,
            promoted_ready: deadline.is_some_and(|deadline| deadline < large.now),
            promotion_pending: promotion_pending && in_large_grid,
            incomplete: ordinary_incomplete
                || catalog.large_references.omissions(key) != 0
                || matches!(large.cells.get(&key), Some(Cell::Failed)),
            lod: lod.large_reference_lod(key, drawable_lod.contains(&key)),
        };
        let owner = handoff.owner(large.owner(key));
        if promoted_root.is_some()
            || ordinary_root
                .is_some_and(|root| children_of(root).any(|entity| models.contains(entity)))
            || large.cells.contains_key(&key)
            || handoff.incomplete
        {
            large.owners.insert(key, owner);
        } else {
            large.owners.remove(&key);
        }
        if owner == Owner::Ordinary {
            large.held_ordinary.insert(key);
        }
        for entity in ordinary_root.into_iter().flat_map(children_of) {
            if let Ok((_, mut visibility)) = models.get_mut(entity) {
                set_visibility(&mut visibility, owner == Owner::Ordinary);
            }
        }
        if let Some(root) = promoted_root
            && let Ok(mut visibility) = roots.get_mut(root)
        {
            set_visibility(&mut visibility, owner == Owner::Promoted);
        }
    }
}

fn set_visibility(visibility: &mut Mut<Visibility>, visible: bool) {
    let wanted = if visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if **visibility != wanted {
        **visibility = wanted;
    }
}

pub(super) fn set_deadlines(
    time: Res<Time>,
    mut large: ResMut<LargeReferences>,
    mut metrics: ResMut<StreamingMetrics>,
    children: Query<&Children>,
    models_ready: Query<&LargeReferenceModel>,
) {
    // STAND-IN(#913): Bevy virtual time supplies the reviewed engine-clock delay.
    let now = time.elapsed();
    large.now = now;
    let (window, grid_radius) = (large.window, large.grid_radius);
    for (&key, cell) in &mut large.cells {
        if !in_window(key, window, grid_radius) {
            continue;
        }
        let Cell::Promoted {
            root,
            models,
            deadline,
        } = cell
        else {
            continue;
        };
        // STAND-IN(#913): Exact retail attachment and the no-model path are unverified.
        // Use Mudcrab's validated full-model completion and keep LOD for incomplete cells.
        if deadline.is_none()
            && *models > 0
            && children.get(*root).is_ok_and(|models| {
                models
                    .iter()
                    .all(|entity| models_ready.get(entity).is_ok_and(|model| model.ready))
            })
        {
            *deadline = Some(now + LOD_DELAY);
        }
        // Capture settlement includes the wait before the LOD visibility can change.
        if deadline.is_some_and(|deadline| deadline >= now) {
            metrics.loading_large_reference_cells += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(x: i32, y: i32) -> CellKey {
        CellKey::Exterior {
            worldspace_id: 60,
            grid_x: x,
            grid_y: y,
        }
    }

    #[test]
    fn width_nine_includes_both_edges_and_demotes_one_outside_cell() {
        let origin = Some((60, IVec2::new(-2, 3)));
        assert!(window_enabled(2, 4));
        assert!(!window_enabled(4, 4));
        assert!(in_window(key(-6, -1), origin, 4));
        assert!(in_window(key(2, 7), origin, 4));
        assert!(!in_window(key(3, 7), origin, 4));
        assert!(!in_window(key(2, 8), origin, 4));

        let mut large = LargeReferences {
            window: Some((60, IVec2::new(-1, 3))),
            cells: [
                (key(-6, -1), Cell::Failed),
                (key(-6, 0), Cell::Failed),
                (key(2, 7), Cell::Failed),
            ]
            .into(),
            grid_radius: 4,
            ..default()
        };
        assert_eq!(large.demotion(), Some(key(-6, -1)));
        large.cells.remove(&key(-6, -1));
        assert_eq!(large.demotion(), Some(key(-6, 0)));
    }

    #[test]
    fn the_default_grid_is_the_ultra_width_and_includes_both_edges() {
        let config = EngineConfig::default();
        assert_eq!(config.large_ref_grid_size, 11);
        let radius = config.large_ref_grid_radius();
        assert_eq!(radius, 5);
        assert_eq!(LargeReferences::default().grid_radius, radius);
        assert_eq!(max_buckets(radius), 11 * 11);
        let origin = Some((60, IVec2::new(-2, 3)));
        assert!(in_window(key(-7, -2), origin, radius));
        assert!(in_window(key(3, 8), origin, radius));
        assert!(!in_window(key(4, 8), origin, radius));
        assert!(!in_window(key(3, 9), origin, radius));
        assert!(window_enabled(4, radius));
        assert!(!window_enabled(5, radius));
    }

    /// The width the fixtures below were written for: the compiled-default width 9, so a cell
    /// at the edge of a width-11 grid stays outside it.
    const FIXTURE_GRID_SIZE: i32 = 9;

    fn streaming_fixture(catalog: AssetCatalog) -> (App, Receiver<DatabaseRequest>) {
        streaming_fixture_with_width(catalog, FIXTURE_GRID_SIZE)
    }

    fn streaming_fixture_with_width(
        catalog: AssetCatalog,
        grid_size: i32,
    ) -> (App, Receiver<DatabaseRequest>) {
        let (database, inbox, _responses) =
            WorldDatabase::channel_fixture(max_buckets(grid_size / 2));
        let mut app = App::new();
        app.add_plugins(bevy::asset::AssetPlugin::default())
            .insert_resource(database)
            .insert_resource(catalog)
            .insert_resource(EngineConfig {
                worldspace_id: 60,
                stream_radius: 2,
                max_scene_loads: 1,
                large_ref_grid_size: grid_size,
                ..default()
            })
            .insert_resource(StreamingWorld {
                planned_center: Some(IVec2::ZERO),
                ..default()
            })
            .init_resource::<ActiveSpace>()
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .init_resource::<LargeReferences>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, update);
        (app, inbox)
    }

    fn placement(id: u32, model: Option<&str>, marker: bool) -> ReferenceRow {
        ReferenceRow {
            form_id: id,
            initially_enabled: true,
            enable_state_unresolved: false,
            has_multibound_override: false,
            named_room_target: false,
            starts_dead: false,
            cell_id: 1,
            base_form_id: 2,
            base_record_type: Some("STAT".into()),
            base_is_marker: marker,
            model_path: model.map(str::to_owned),
            position: [3.0 * crate::world::components::CELL_SIZE, 0.0, 0.0],
            rotation: [0.0; 3],
            scale: 1.0,
            bounds_min: [0.0; 3],
            bounds_max: [0.0; 3],
            bounds_valid: false,
            light: None,
            light_radius_override: None,
            emittance: None,
            door: None,
        }
    }

    #[test]
    fn known_filtered_members_allow_both_representations_to_plan_drawable_models() {
        let cell = key(3, 0);
        let rows = vec![
            placement(7, Some("Landscape/Rock01.nif"), false),
            placement(8, None, true),
            placement(9, Some("Meshes\\Effects\\FXRapids01.nif"), false),
            placement(10, Some("SKY/Clouds.nif"), false),
            placement(11, Some("Effects/FXCobwebRoundWall01.nif"), false),
            placement(12, Some("Markers/Marker.nif"), true),
        ];
        assert_eq!(member_model(&rows[1]), MemberModel::EditorMarker);
        assert_eq!(member_model(&rows[2]), MemberModel::Effects);
        assert_eq!(member_model(&rows[3]), MemberModel::Sky);
        assert_eq!(member_model(&rows[5]), MemberModel::EditorMarker);
        for row in &rows {
            let actual =
                super::super::spawned_model_path(row.model_path.clone(), row.base_is_marker);
            let expected = match member_model(row) {
                MemberModel::Draw(path) => Some(path),
                _ => None,
            };
            assert_eq!(actual, expected);
        }
        let members = rows.iter().map(|row| row.form_id).collect();
        assert_eq!(
            ordinary_members(cell, &members, &rows, &ActorCatalog::default()).expected,
            Some(2)
        );
        let mut catalog = AssetCatalog::default();
        catalog.large_references.members.insert(cell, members);
        let (mut app, inbox) = streaming_fixture(catalog);
        app.update();
        let DatabaseRequest::LoadLargeReferences { reply, .. } = inbox.try_recv().unwrap() else {
            panic!("expected a large-reference request");
        };
        reply
            .send(Ok(LargeReferenceRows {
                references: rows,
                missing: 0,
            }))
            .unwrap();
        app.update();
        assert!(matches!(
            app.world().resource::<LargeReferences>().cells[&cell],
            Cell::Promoted { models: 2, .. }
        ));
    }

    #[test]
    fn unknown_filtered_members_and_enable_state_still_require_lod() {
        let cell = key(3, 0);
        let mut disabled_marker = placement(8, Some("Markers/Marker.nif"), true);
        disabled_marker.initially_enabled = false;
        assert_eq!(member_model(&disabled_marker), MemberModel::Disabled);
        let mut unresolved_marker = disabled_marker;
        unresolved_marker.enable_state_unresolved = true;
        for unknown in [
            placement(8, None, false),
            placement(8, Some("Markers/Marker.nif"), false),
            placement(8, Some("Effects/../Unverified.nif"), false),
            unresolved_marker,
        ] {
            assert_eq!(member_model(&unknown), MemberModel::Unknown);
            let mut catalog = AssetCatalog::default();
            catalog
                .large_references
                .members
                .insert(cell, HashSet::from([7, 8]));
            let (mut app, inbox) = streaming_fixture(catalog);
            app.update();
            let DatabaseRequest::LoadLargeReferences { reply, .. } = inbox.try_recv().unwrap()
            else {
                panic!("expected a large-reference request");
            };
            reply
                .send(Ok(LargeReferenceRows {
                    references: vec![placement(7, Some("Landscape/Rock01.nif"), false), unknown],
                    missing: 0,
                }))
                .unwrap();
            app.update();
            assert!(matches!(
                app.world().resource::<LargeReferences>().cells[&cell],
                Cell::Failed
            ));
        }
    }

    #[test]
    fn loads_only_the_outer_ring_and_retires_one_root_per_frame() {
        let requested = [key(3, 0), key(4, 0)];
        let mut catalog = AssetCatalog::default();
        for member_key in [key(2, 0), key(5, 0)].into_iter().chain(requested) {
            catalog
                .large_references
                .members
                .insert(member_key, HashSet::from([7]));
        }
        let (mut app, inbox) = streaming_fixture(catalog);

        app.update();
        for expected in requested {
            let DatabaseRequest::LoadLargeReferences { key, reply, .. } = inbox.try_recv().unwrap()
            else {
                panic!("unexpected database request");
            };
            assert_eq!(key, expected);
            reply
                .send(Ok(LargeReferenceRows {
                    references: Vec::new(),
                    missing: 0,
                }))
                .unwrap();
        }
        assert!(inbox.is_empty());
        app.update();
        let roots = requested.map(|key| {
            let mut large = app.world_mut().resource_mut::<LargeReferences>();
            let Cell::Promoted { root, deadline, .. } = large.cells.get_mut(&key).unwrap() else {
                panic!("cell {key:?} failed to promote");
            };
            *deadline = Some(Duration::ZERO);
            *root
        });
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .planned_center = Some(IVec2::new(-2, 0));
        app.update();
        assert!(app.world().get_entity(roots[0]).is_err());
        assert!(app.world().get_entity(roots[1]).is_ok());
        app.update();
        assert!(app.world().get_entity(roots[1]).is_err());
    }

    struct HandoffRig {
        app: App,
        cell: CellKey,
        marked: Entity,
        unmarked: Entity,
        promoted: Entity,
        ordinary: Entity,
        model: Entity,
    }

    impl HandoffRig {
        fn new(missing: bool) -> Self {
            use super::super::lod_objects::{LodObjectCell, hide_loaded_object_cells};
            let cell = key(4, 0);
            let mut catalog = AssetCatalog::default();
            catalog
                .large_references
                .members
                .insert(cell, HashSet::from([7]));
            let mut app = App::new();
            app.insert_resource(catalog)
                .init_resource::<StreamingWorld>()
                .init_resource::<LodObjects>()
                .insert_resource(EngineConfig {
                    large_ref_grid_size: FIXTURE_GRID_SIZE,
                    ..default()
                })
                .init_resource::<StreamingMetrics>()
                .init_resource::<ProfilingState>()
                .init_resource::<super::super::TerrainContinuity>()
                .insert_resource(LargeReferences {
                    window: Some((60, IVec2::ZERO)),
                    ordinary_radius: 2,
                    grid_radius: FIXTURE_GRID_SIZE / 2,
                    now: LOD_DELAY,
                    ..default()
                })
                .add_systems(
                    Update,
                    (
                        super::super::despawn_cells,
                        resolve_visibility,
                        hide_loaded_object_cells,
                    )
                        .chain(),
                );
            let world = app.world_mut();
            let piece = world
                .resource_mut::<LodObjects>()
                .fixture_large_reference_lod(cell, LargeReferenceLod::Segmented);
            let marked = world
                .spawn((LodObjectCell(cell), piece, Visibility::Inherited))
                .id();
            let unmarked = world
                .spawn((LodObjectCell(cell), Visibility::Inherited))
                .id();
            let promoted = world.spawn(Visibility::Hidden).id();
            world.resource_mut::<LargeReferences>().cells.insert(
                cell,
                if missing {
                    Cell::Failed
                } else {
                    Cell::Promoted {
                        root: promoted,
                        models: 1,
                        deadline: Some(LOD_DELAY),
                    }
                },
            );
            let ordinary = world
                .spawn((
                    Visibility::Inherited,
                    OrdinaryLargeReferences { expected: Some(1) },
                ))
                .id();
            let model = world
                .spawn((
                    ChildOf(ordinary),
                    LargeReferenceModel { ready: false },
                    Visibility::Hidden,
                ))
                .id();
            Self {
                app,
                cell,
                marked,
                unmarked,
                promoted,
                ordinary,
                model,
            }
        }

        fn move_to(&mut self, x: i32) {
            self.app
                .world_mut()
                .resource_mut::<LargeReferences>()
                .window = Some((60, IVec2::new(x, 0)));
        }

        fn lod(&mut self, lod: LargeReferenceLod) {
            let world = self.app.world_mut();
            let piece = world
                .resource_mut::<LodObjects>()
                .fixture_large_reference_lod(self.cell, lod);
            world.entity_mut(self.marked).insert(piece);
        }

        fn expect_owner(&mut self, owner: Owner) {
            self.app.update();
            let world = self.app.world();
            let shown = |entity| *world.get::<Visibility>(entity).unwrap() == Visibility::Inherited;
            assert_eq!(world.resource::<LargeReferences>().owner(self.cell), owner);
            assert_eq!(shown(self.marked), owner == Owner::Lod);
            assert_eq!(shown(self.promoted), owner == Owner::Promoted);
            assert_eq!(shown(self.model), owner == Owner::Ordinary);
        }

        fn pass_deadline(&mut self) {
            self.app.world_mut().resource_mut::<LargeReferences>().now += Duration::from_millis(1);
        }
    }

    #[test]
    fn marked_lod_handoff_waits_in_both_directions_at_large_grid_edge() {
        let mut rig = HandoffRig::new(false);
        rig.move_to(-1);
        rig.expect_owner(Owner::Lod);
        rig.move_to(0);
        rig.expect_owner(Owner::Lod);
        rig.pass_deadline();
        rig.expect_owner(Owner::Promoted);
        assert_eq!(
            rig.app.world().get(rig.unmarked),
            Some(&Visibility::Inherited)
        );
        rig.move_to(-1);
        rig.lod(LargeReferenceLod::Pending);
        rig.expect_owner(Owner::Promoted);
        assert_eq!(
            rig.app.world().resource::<LargeReferences>().demotion(),
            None
        );
        rig.lod(LargeReferenceLod::Segmented);
        rig.expect_owner(Owner::Lod);
        assert_eq!(
            rig.app.world().resource::<LargeReferences>().demotion(),
            Some(rig.cell)
        );
    }

    #[test]
    fn ordinary_full_model_handoff_waits_in_both_directions_at_near_grid_edge() {
        let mut rig = HandoffRig::new(false);
        rig.pass_deadline();
        rig.expect_owner(Owner::Promoted);
        rig.move_to(2);
        rig.app
            .world_mut()
            .entity_mut(rig.model)
            .insert(PendingAssetProfile {
                started: Instant::now(),
                scene_spawned: false,
                path: String::new(),
                form_id: 7,
                base_form_id: 7,
                base_record_type: None,
                static_physics: false,
                cell_id: 1,
                actor_scene: false,
            });
        rig.app
            .world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(rig.cell, CellStatus::Resident { root: rig.ordinary });
        rig.expect_owner(Owner::Promoted);
        rig.app
            .world_mut()
            .get_mut::<LargeReferenceModel>(rig.model)
            .unwrap()
            .ready = true;
        rig.expect_owner(Owner::Ordinary);
        assert!(
            rig.app
                .world()
                .resource::<LargeReferences>()
                .holds_ordinary(rig.cell)
        );
        rig.move_to(1);
        rig.app.world_mut().resource_mut::<LargeReferences>().now = LOD_DELAY;
        rig.expect_owner(Owner::Ordinary);
        rig.pass_deadline();
        rig.expect_owner(Owner::Promoted);
        assert!(
            !rig.app
                .world()
                .resource::<LargeReferences>()
                .holds_ordinary(rig.cell)
        );
    }

    #[test]
    fn retiring_ordinary_root_waits_for_admission_and_drawable_replacement() {
        let mut rig = HandoffRig::new(false);
        rig.move_to(2);
        rig.lod(LargeReferenceLod::Absent);
        let world = rig.app.world_mut();
        let mut large = world.resource_mut::<LargeReferences>();
        let promotion = large.cells.remove(&rig.cell).unwrap();
        world
            .get_mut::<LargeReferenceModel>(rig.model)
            .unwrap()
            .ready = true;
        world
            .resource_mut::<StreamingWorld>()
            .set_resident_for_test(rig.cell, rig.ordinary);
        rig.expect_owner(Owner::Ordinary);
        rig.move_to(1);
        rig.app
            .world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(rig.cell, CellStatus::Retiring { root: rig.ordinary });
        rig.expect_owner(Owner::Ordinary);
        assert!(rig.app.world().get_entity(rig.ordinary).is_ok());
        rig.app
            .world_mut()
            .resource_mut::<LargeReferences>()
            .cells
            .insert(rig.cell, promotion);
        rig.expect_owner(Owner::Ordinary);
        rig.pass_deadline();
        rig.expect_owner(Owner::Promoted);
        rig.app.update();
        assert!(rig.app.world().get_entity(rig.ordinary).is_err());
    }

    #[test]
    fn incomplete_bucket_keeps_marked_lod_and_never_shows_partial_promotion() {
        let mut rig = HandoffRig::new(true);
        rig.pass_deadline();
        rig.expect_owner(Owner::Lod);
    }

    #[test]
    fn incomplete_ordinary_bucket_keeps_marked_lod_with_a_ready_model() {
        let mut rig = HandoffRig::new(false);
        rig.pass_deadline();
        rig.expect_owner(Owner::Promoted);
        rig.move_to(2);
        let world = rig.app.world_mut();
        world
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(rig.cell, CellStatus::Resident { root: rig.ordinary });
        world
            .entity_mut(rig.ordinary)
            .insert(OrdinaryLargeReferences { expected: None });
        world
            .get_mut::<LargeReferenceModel>(rig.model)
            .unwrap()
            .ready = true;
        rig.expect_owner(Owner::Lod);
    }

    #[test]
    fn repeated_teleports_cancel_stale_work_and_bound_retained_buckets() {
        // Width 9: 81 cells, 25 of them ordinary, so 56 requests; then the retained roots use up
        // the cap.
        repeated_teleports_bound_retained_buckets(9, [56, 26, 1, 1]);
    }

    /// The Ultra default width (#949): 121 cells, 25 ordinary, so 96 requests, and the same
    /// 121-cell cap bounds the retained roots.
    #[test]
    fn repeated_teleports_bound_retained_buckets_at_the_default_width() {
        assert_eq!(EngineConfig::default().large_ref_grid_size, 11);
        repeated_teleports_bound_retained_buckets(11, [96, 26, 1, 1]);
    }

    fn repeated_teleports_bound_retained_buckets(grid_size: i32, expected: [usize; 4]) {
        let radius = grid_size / 2;
        let centers = [0, 20, 40, 60];
        let mut catalog = AssetCatalog::default();
        for x in centers {
            for dx in -radius..=radius {
                for dy in -radius..=radius {
                    catalog
                        .large_references
                        .members
                        .insert(key(x + dx, dy), HashSet::from([7]));
                }
            }
        }
        let (mut app, inbox) = streaming_fixture_with_width(catalog, grid_size);
        let mut stale: Option<Arc<AtomicBool>> = None;
        for (x, admitted) in centers.into_iter().zip(expected) {
            app.world_mut()
                .resource_mut::<StreamingWorld>()
                .planned_center = Some(IVec2::new(x, 0));
            app.update();
            if let Some(stale) = stale.take() {
                assert!(stale.load(Ordering::Relaxed));
            }
            let mut requests: Vec<_> = inbox.try_iter().collect();
            assert_eq!(requests.len(), admitted);
            let DatabaseRequest::LoadLargeReferences { cancelled, .. } = requests.pop().unwrap()
            else {
                panic!("expected a large-reference request");
            };
            assert!(!cancelled.load(Ordering::Relaxed));
            stale = Some(cancelled);
            for request in requests {
                let DatabaseRequest::LoadLargeReferences { key, .. } = request else {
                    panic!("expected a large-reference request");
                };
                let world = app.world_mut();
                let root = world.spawn(Visibility::Inherited).id();
                let mut large = world.resource_mut::<LargeReferences>();
                large.cells.insert(
                    key,
                    Cell::Promoted {
                        root,
                        models: 1,
                        deadline: Some(LOD_DELAY),
                    },
                );
                // These visible roots are waiting for an outgoing LOD replacement.
                large.owners.insert(key, Owner::Promoted);
            }
            assert!(app.world().resource::<LargeReferences>().cells.len() <= max_buckets(radius));
        }
    }
}
