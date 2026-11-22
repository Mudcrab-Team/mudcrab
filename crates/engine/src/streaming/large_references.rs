//! Exterior large-reference promotion and the width-4 object LOD deadline.
//! Retail facts G02–G04: https://github.com/Mudcrab-Team/skyrim_decomp/pull/821.

use super::{
    ActiveSpace, CellStatus, PendingAssetProfile, PendingModel, RenderOrigin, StreamingMetrics,
    StreamingWorld, cell_order_key, cell_translation, requests, spawned_model_path,
};
use crate::{
    config::EngineConfig,
    profiling::ProfilingState,
    render::PLACED_OBJECT_RENDER_LAYERS,
    world::{
        components::{CellRef, ExteriorCellGrid, FormId, MeshHandle, WorldTransform},
        database::{AssetCatalog, CellKey, DatabaseRequest, LargeReferenceRows, WorldDatabase},
    },
};
use bevy::{app::Propagate, gltf::GltfAssetLabel, prelude::*};
use crossbeam_channel::{Receiver, TryRecvError, bounded};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const LARGE_GRID_RADIUS: i32 = 9 / 2;
const DEMOTIONS_PER_FRAME: usize = 1;
const LOD_DELAY: Duration = Duration::from_millis(2_000);

type Response = Result<LargeReferenceRows, String>;

enum Cell {
    Loading(Receiver<Response>),
    Promoted {
        root: Entity,
        models: usize,
        complete_input: bool,
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

#[derive(Resource, Default)]
pub(super) struct LargeReferences {
    cells: HashMap<CellKey, Cell>,
    window: Option<(u32, IVec2)>,
    now: Duration,
}

fn in_window(key: CellKey, window: Option<(u32, IVec2)>) -> bool {
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
        && grid_x.abs_diff(center.x) <= LARGE_GRID_RADIUS.unsigned_abs()
        && grid_y.abs_diff(center.y) <= LARGE_GRID_RADIUS.unsigned_abs()
}

fn window_enabled(ordinary_radius: i32) -> bool {
    ordinary_radius < LARGE_GRID_RADIUS
}

impl LargeReferences {
    fn demotion(&self) -> Option<CellKey> {
        self.cells
            .keys()
            .copied()
            .filter(|&key| !in_window(key, self.window))
            .min_by_key(|&key| cell_order_key(key))
    }

    pub(super) fn suppresses(&self, key: CellKey) -> bool {
        in_window(key, self.window)
            && matches!(
                self.cells.get(&key),
                Some(Cell::Promoted { deadline: Some(deadline), .. }) if *deadline < self.now
            )
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
    let window = (space.interior.is_none() && window_enabled(config.stream_radius))
        .then_some(streaming.planned_center)
        .flatten()
        .map(|center| (world, center));
    if large.window.map(|(world, _)| world) != window.map(|(world, _)| world) {
        for (_, cell) in large.cells.drain() {
            cell.despawn(&mut commands);
        }
    }
    large.window = window;
    let Some((world, center)) = window else {
        metrics.loading_large_reference_cells = 0;
        return;
    };

    // The regular full-cell roots own all their references, including RNAM members.
    large.cells.retain(|key, cell| {
        if matches!(
            streaming.cells.get(key),
            Some(CellStatus::Resident { .. } | CellStatus::Retiring { .. })
        ) {
            if let Cell::Promoted { root, .. } = cell {
                commands.entity(*root).try_despawn();
            }
            return false;
        }
        true
    });
    for _ in 0..DEMOTIONS_PER_FRAME {
        if let Some(key) = large.demotion()
            && let Some(cell) = large.cells.remove(&key)
        {
            cell.despawn(&mut commands);
        }
    }
    let mut outstanding = 0;
    for dx in -LARGE_GRID_RADIUS..=LARGE_GRID_RADIUS {
        for dy in -LARGE_GRID_RADIUS..=LARGE_GRID_RADIUS {
            let (Some(grid_x), Some(grid_y)) = (center.x.checked_add(dx), center.y.checked_add(dy))
            else {
                continue;
            };
            let key = CellKey::Exterior {
                worldspace_id: world,
                grid_x,
                grid_y,
            };
            if !catalog.large_reference_cells.contains(&key) {
                continue;
            }
            let ordinary = dx.abs() <= config.stream_radius && dy.abs() <= config.stream_radius;
            if ordinary
                || matches!(
                    streaming.cells.get(&key),
                    Some(CellStatus::Resident { .. } | CellStatus::Retiring { .. })
                )
            {
                continue;
            }
            match large.cells.entry(key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let (reply, response) = bounded(1);
                    if let Err(reason) =
                        database.try_request(DatabaseRequest::LoadLargeReferences { key, reply })
                    {
                        // The ordinary worker queue can fill. Keep the cell eligible for retry.
                        debug!(?key, %reason, "large-reference request deferred");
                    } else {
                        entry.insert(Cell::Loading(response));
                    }
                    outstanding += 1;
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let Cell::Loading(reply) = entry.get() else {
                        continue;
                    };
                    let response = match reply.try_recv() {
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
    let mut models = 0;
    let mut missing = rows.missing;
    let root = commands
        .spawn((
            Name::new("Large-reference cell"),
            ExteriorCellGrid(IVec2::new(grid_x, grid_y)),
            Transform::from_translation(cell_translation(key, origin)),
            Visibility::default(),
        ))
        .id();
    commands.entity(root).with_children(|parent| {
        for reference in rows.references {
            if !reference.initially_enabled {
                continue;
            }
            let Some(path) =
                spawned_model_path(reference.model_path.clone(), reference.base_is_marker)
            else {
                missing += 1;
                continue;
            };
            let (world_position, transform, model_bounds, bounds) =
                super::reference_placement(key, &reference);
            // STAND-IN(#913): Omit moved overrides until the retail placement policy is known.
            // The ordinary cell at the winning position could otherwise draw the same reference.
            if world_position.grid != IVec2::new(grid_x, grid_y) {
                missing += 1;
                continue;
            }
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
            models += 1;
        }
    });
    if missing != 0 {
        warn!(
            ?key,
            missing, "large-reference placements without resolved full models omitted"
        );
    }
    debug!(?key, models, missing, "large-reference cell promoted");
    Cell::Promoted {
        root,
        models,
        complete_input: missing == 0,
        deadline: None,
    }
}

pub(super) fn set_deadlines(
    time: Res<Time>,
    mut large: ResMut<LargeReferences>,
    mut metrics: ResMut<StreamingMetrics>,
    children: Query<&Children>,
    models_ready: Query<&LargeReferenceModel>,
) {
    let now = time.elapsed();
    large.now = now;
    let window = large.window;
    for (&key, cell) in &mut large.cells {
        if !in_window(key, window) {
            continue;
        }
        let Cell::Promoted {
            root,
            models,
            complete_input: true,
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
        assert!(window_enabled(2));
        assert!(!window_enabled(4));
        assert!(in_window(key(-6, -1), origin));
        assert!(in_window(key(2, 7), origin));
        assert!(!in_window(key(3, 7), origin));
        assert!(!in_window(key(2, 8), origin));

        let mut large = LargeReferences {
            window: Some((60, IVec2::new(-1, 3))),
            cells: [
                (key(-6, -1), Cell::Failed),
                (key(-6, 0), Cell::Failed),
                (key(2, 7), Cell::Failed),
            ]
            .into(),
            ..default()
        };
        assert_eq!(large.demotion(), Some(key(-6, -1)));
        large.cells.remove(&key(-6, -1));
        assert_eq!(large.demotion(), Some(key(-6, 0)));
    }

    #[test]
    fn loads_only_the_outer_ring_and_retires_one_root_per_frame() {
        let (database, inbox, _responses) = WorldDatabase::channel_fixture(8);
        let requested = [key(3, 0), key(4, 0)];
        let mut catalog = AssetCatalog::default();
        catalog.large_reference_cells.extend([key(2, 0), key(5, 0)]);
        catalog.large_reference_cells.extend(requested);
        let mut app = App::new();
        app.add_plugins(bevy::asset::AssetPlugin::default())
            .insert_resource(database)
            .insert_resource(catalog)
            .insert_resource(EngineConfig {
                worldspace_id: 60,
                stream_radius: 2,
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

        app.update();
        for expected in requested {
            let DatabaseRequest::LoadLargeReferences { key, reply } = inbox.try_recv().unwrap()
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
            let Cell::Promoted { root, .. } = app.world().resource::<LargeReferences>().cells[&key]
            else {
                panic!("cell {key:?} failed to promote");
            };
            root
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

    #[test]
    fn fallback_hides_only_inside_the_square_after_the_deadline() {
        use super::super::lod_objects::{
            LargeReferencePiece, LodObjectCell, hide_loaded_object_cells,
        };

        let cell = key(4, 4);
        let deadline = Duration::from_secs(4);
        let mut app = App::new();
        app.init_resource::<StreamingWorld>()
            .insert_resource(LargeReferences {
                window: Some((60, IVec2::ZERO)),
                now: deadline,
                ..default()
            })
            .add_systems(Update, hide_loaded_object_cells);
        let marked = app
            .world_mut()
            .spawn((
                LodObjectCell(cell),
                LargeReferencePiece,
                Visibility::Inherited,
            ))
            .id();
        let ordinary = app
            .world_mut()
            .spawn((LodObjectCell(cell), Visibility::Inherited))
            .id();
        let visibility = |app: &App, entity| *app.world().get::<Visibility>(entity).unwrap();
        app.update();
        assert_eq!(visibility(&app, marked), Visibility::Inherited);
        app.world_mut()
            .resource_mut::<LargeReferences>()
            .cells
            .insert(
                cell,
                Cell::Promoted {
                    root: Entity::PLACEHOLDER,
                    models: 2,
                    complete_input: true,
                    deadline: Some(deadline),
                },
            );
        app.update();
        assert_eq!(visibility(&app, marked), Visibility::Inherited);
        app.world_mut().resource_mut::<LargeReferences>().now += Duration::from_millis(1);
        app.update();
        assert_eq!(visibility(&app, marked), Visibility::Hidden);
        assert_eq!(visibility(&app, ordinary), Visibility::Inherited);
        app.world_mut().resource_mut::<LargeReferences>().window = Some((60, IVec2::new(-1, 0)));
        app.update();
        assert_eq!(visibility(&app, marked), Visibility::Inherited);
    }
}
