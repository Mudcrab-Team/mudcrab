use super::super::select_terrain_lod_tier;
use super::*;
use crate::{
    cluster::terrain::{ClusterTerrain, TerrainBlock, TerrainDrawReadiness},
    streaming::{ActiveSpace, identity_changes},
    world::components::{CELL_SIZE, StreamingCamera},
};
use bevy::ecs::{
    entity_disabling::Disabled, relationship::RelationshipHookMode, system::SystemState,
};
use shared::lod::LodOrigin;

const WORLD: u32 = 0x3c;

/// A component registered as a default query disabler: a query only sees entities that carry
/// it when the query names the component, so each choice changes which patches a pass reads.
#[derive(Clone, Copy, Debug)]
enum Disabler {
    Ready,
    Visibility,
    Patch,
    /// A component the journal's observers were never told about: registered as a default
    /// disabler after the journal was installed and before the pass's queries exist.
    Custom,
}

/// A default disabler no part of the engine knows.
#[derive(Component)]
struct CustomDisabled;

fn app(journal: bool) -> App {
    app_with(journal, None)
}

fn register_disabler<T: Component>(world: &mut World) {
    let id = world.register_component::<T>();
    world
        .resource_mut::<DefaultQueryFilters>()
        .register_disabling_component(id);
}

fn app_with(journal: bool, disabler: Option<Disabler>) -> App {
    let mut app = App::new();
    app.insert_resource(EngineConfig {
        worldspace_id: WORLD,
        ..default()
    })
    .insert_resource(RenderOrigin(IVec2::ZERO))
    .insert_resource(StreamingMetrics::default())
    .insert_resource(LodStreaming::default())
    .init_resource::<crate::profiling::ProfilingState>()
    .init_resource::<TerrainLodObservation>()
    .init_resource::<TerrainLodIndex>()
    .add_systems(
        Update,
        update_terrain_lod_visibility.run_if(terrain_lod_visibility_has_work),
    );
    // Followed by the journal's observers, because they are installed after this.
    match disabler {
        Some(Disabler::Ready) => register_disabler::<TerrainSurfaceReady>(app.world_mut()),
        Some(Disabler::Visibility) => register_disabler::<Visibility>(app.world_mut()),
        Some(Disabler::Patch) => register_disabler::<TerrainPatch>(app.world_mut()),
        Some(Disabler::Custom) | None => {}
    }
    if journal {
        identity_changes::install_terrain(app.world_mut());
    }
    if let Some(Disabler::Custom) = disabler {
        register_disabler::<CustomDisabled>(app.world_mut());
    }
    app.world_mut().spawn((
        StreamingCamera,
        Transform::from_xyz(CELL_SIZE * 0.5, 0.0, -CELL_SIZE * 0.5),
    ));
    app
}

fn cluster_terrain(blocks: &[(u32, &[IVec2], Option<u32>)]) -> ClusterTerrain {
    ClusterTerrain::for_blocks_test(
        blocks
            .iter()
            .map(|&(worldspace_id, cells, batch)| TerrainBlock {
                mesh_hash: String::new(),
                primitive_index: 0,
                worldspace_id,
                anchor: cells.first().copied().unwrap_or_default(),
                side_cells: 1,
                instance_path: String::new(),
                cells: cells.to_vec(),
                material: Handle::default(),
                batch,
            })
            .collect(),
    )
}

fn pending_blocks(app: &App) -> usize {
    app.world()
        .resource::<StreamingMetrics>()
        .pending_cluster_terrain_blocks
}

fn enable_cluster_terrain(app: &mut App) {
    let mut config = app.world_mut().resource_mut::<EngineConfig>();
    config.clusters = Some(String::new());
    config.cluster_terrain = true;
}

#[test]
fn coverage_change_filters_preserve_patch_domains_with_disablers() {
    for disabler in [Disabler::Ready, Disabler::Visibility, Disabler::Patch] {
        let mut app = app_with(true, Some(disabler));
        let near = near_patch(app.world_mut(), Some(WORLD), IVec2::ZERO, 0);
        let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, IVec2::ZERO);
        let far = lod_patch(
            app.world_mut(),
            Some(root),
            IVec2::ZERO,
            0,
            Some(LodTier::Tier4),
        );
        app.update();
        // Even refused patches retain exact coverage facts in the maintained index. The
        // debug/test audit also runs on otherwise idle frames and compares those facts.
        app.world_mut()
            .get_mut::<TerrainCoverage>(near)
            .unwrap()
            .grid
            .x = 3;
        app.world_mut()
            .get_mut::<TerrainCoverage>(far)
            .unwrap()
            .quadrant = 1;
        app.update();
        assert_matches_full_pass(&mut app, &format!("coverage mutation with {disabler:?}"));
    }
}

fn move_camera(world: &mut World, grid_x: i32) {
    let mut query = world.query_filtered::<&mut Transform, With<StreamingCamera>>();
    let mut transform = query.single_mut(world).unwrap();
    transform.translation.x = CELL_SIZE * (grid_x as f32 + 0.5);
}

fn root_component(worldspace: u32, tier: LodTier, grid: IVec2) -> LodChunkRoot {
    let origin = LodOrigin::new(0, 0);
    LodChunkRoot {
        key: ChunkKey::new(
            worldspace,
            tier,
            origin.chunk_for_cell(tier, grid.x, grid.y),
        ),
        generation: 1,
        origin,
        retry_count: 0,
    }
}

fn chunk_root(world: &mut World, worldspace: u32, tier: LodTier, grid: IVec2) -> Entity {
    world.spawn(root_component(worldspace, tier, grid)).id()
}

fn lod_patch(
    world: &mut World,
    parent: Option<Entity>,
    grid: IVec2,
    quadrant: u8,
    tier: Option<LodTier>,
) -> Entity {
    let patch = world
        .spawn((
            TerrainCoverage {
                grid,
                quadrant,
                tier,
            },
            TerrainSurfaceReady,
            Visibility::Hidden,
        ))
        .id();
    if let Some(parent) = parent {
        world.entity_mut(patch).insert(ChildOf(parent));
    }
    patch
}

fn near_patch(world: &mut World, worldspace: Option<u32>, grid: IVec2, quadrant: u8) -> Entity {
    let patch = world
        .spawn((
            TerrainPatch,
            TerrainCoverage {
                grid,
                quadrant,
                tier: None,
            },
            TerrainSurfaceReady,
            Visibility::Inherited,
        ))
        .id();
    if let Some(worldspace) = worldspace {
        world
            .entity_mut(patch)
            .insert(TerrainWorldspace(worldspace));
    }
    patch
}

fn work(app: &App) -> [f64; 4] {
    app.world().resource::<TerrainLodObservation>().work()
}

fn visibility(app: &App, entity: Entity) -> Visibility {
    *app.world().get::<Visibility>(entity).unwrap()
}

/// What a full pass over every patch gives.
struct Expected {
    visibility: HashMap<Entity, Visibility>,
    visible: usize,
    pending: usize,
}

/// The independent exhaustive ownership walk used by the oracle.
fn reference_world(
    owners: &Query<LodTerrainOwner>,
    entity: Entity,
    tier: Option<LodTier>,
) -> Option<u32> {
    let mut at = entity;
    let mut seen = HashSet::new();
    let mut near_world = None;
    let mut chunk = None;
    loop {
        if !seen.insert(at) {
            return None;
        }
        let (world, root, parent) = owners.get(at).ok()?;
        if let Some(world) = world
            && near_world
                .replace(world.0)
                .is_some_and(|old| old != world.0)
        {
            return None;
        }
        if let Some(root) = root
            && chunk.replace(root.terrain_key()).is_some()
        {
            return None;
        }
        match parent {
            Some(parent) => at = parent.parent(),
            None => break,
        }
    }
    match (tier, near_world, chunk) {
        (None, Some(world), None) => Some(world),
        (Some(tier), None, Some(chunk)) if chunk.tier == tier => Some(chunk.worldspace_id),
        _ => None,
    }
}

/// An independent exhaustive oracle over the original two query domains, the
/// original ancestry walk, the public main-view demand, and the original
/// stock inputs, sharing nothing with the index: the
/// independent oracle.
fn reference(world: &mut World) -> Expected {
    let mut state = SystemState::<(
        LodDemandViews,
        Res<EngineConfig>,
        Res<RenderOrigin>,
        Option<Res<ClusterTerrain>>,
        Option<Res<TerrainDrawReadiness>>,
        Query<LodTerrainOwner>,
        Query<
            (Entity, &TerrainCoverage, Option<&TerrainWorldspace>),
            (With<TerrainPatch>, With<TerrainSurfaceReady>),
        >,
        Query<
            (
                Entity,
                &TerrainCoverage,
                Option<&TerrainSurfaceReady>,
                &Visibility,
            ),
            Without<TerrainPatch>,
        >,
        Query<(&MergedTerrainCoverage, &TerrainWorldspace), DrawnMergedTerrain>,
    )>::new(world);
    let (views, config, origin, cluster_terrain, terrain_readiness, owners, full, lod, merged) =
        state.get(world).unwrap();
    let demand = views.current(&config, origin.0);
    let camera_grid = demand.map_or(IVec2::ZERO, |demand| demand.center);
    let terrain_world = demand.map(|demand| demand.world);
    let pending = if config.clusters.is_some() && config.cluster_terrain {
        cluster_terrain.as_ref().map_or(0, |terrain| {
            terrain
                .blocks
                .iter()
                .filter(|block| Some(block.worldspace_id) == terrain_world)
                .filter(|block| {
                    !block.batch.is_some_and(|batch| {
                        terrain_readiness
                            .as_ref()
                            .is_some_and(|ready| ready.contains(batch))
                    })
                })
                .count()
        })
    } else {
        0
    };
    let mut full_ready: HashSet<_> = full
        .iter()
        .filter_map(|(entity, coverage, _)| {
            reference_world(&owners, entity, None)
                .is_some_and(|world| Some(world) == terrain_world)
                .then_some((coverage.grid, coverage.quadrant))
        })
        .collect();
    full_ready.extend(
        merged
            .iter()
            .filter(|(_, owner)| Some(owner.0) == terrain_world)
            .flat_map(|(coverage, _)| (0..4).map(move |quadrant| (coverage.grid, quadrant))),
    );
    let mut available = HashMap::<(IVec2, u8), HashSet<LodTier>>::new();
    for (entity, coverage, surface, _) in &lod {
        if let (Some(tier), Some(_)) = (coverage.tier, surface)
            && reference_world(&owners, entity, Some(tier))
                .is_some_and(|world| Some(world) == terrain_world)
        {
            available
                .entry((coverage.grid, coverage.quadrant))
                .or_default()
                .insert(tier);
        }
    }
    let selected: HashMap<_, _> = available
        .iter()
        .map(|(&(grid, quadrant), candidates)| {
            (
                (grid, quadrant),
                select_terrain_lod_tier(
                    chebyshev_grid_distance(grid, camera_grid),
                    full_ready.contains(&(grid, quadrant)),
                    candidates,
                    &config.terrain_lod,
                ),
            )
        })
        .collect();
    let mut expected = Expected {
        visibility: HashMap::new(),
        visible: 0,
        pending,
    };
    for (entity, coverage, surface, _) in &lod {
        if surface.is_none() {
            continue;
        }
        let drawn = coverage.tier.is_some_and(|tier| {
            surface.is_some()
                && reference_world(&owners, entity, Some(tier))
                    .is_some_and(|world| Some(world) == terrain_world)
                && selected.get(&(coverage.grid, coverage.quadrant)) == Some(&Some(tier))
        });
        expected.visible += usize::from(drawn);
        expected.visibility.insert(
            entity,
            if drawn {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            },
        );
    }
    expected
}

/// The drawn set and visible metrics equal an exhaustive pass. Production ready
/// counts are checked by the scheduled audit.
fn assert_matches_full_pass(app: &mut App, context: &str) {
    assert_matches_full_pass_except(app, context, &HashSet::new());
}

/// [`assert_matches_full_pass`], leaving out patches whose `Visibility` was changed from
/// outside since the last frame the old pass would have refreshed on.
fn assert_matches_full_pass_except(app: &mut App, context: &str, held: &HashSet<Entity>) {
    let expected = reference(app.world_mut());
    for (entity, want) in &expected.visibility {
        if held.contains(entity) {
            continue;
        }
        assert_eq!(
            visibility(app, *entity),
            *want,
            "{context}: {entity} differs from a full pass"
        );
    }
    assert_eq!(
        pending_blocks(app),
        expected.pending,
        "{context}: pending cluster terrain blocks"
    );
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .visible_lod_terrain_patches,
        expected.visible,
        "{context}: visible count"
    );
    // Ready counts are asserted against the live production index in the
    // scheduled audit, rather than maintained in a cfg(test)-only counter.
}

#[test]
fn unrelated_object_streaming_does_no_terrain_work() {
    let mut app = app(true);
    let world = app.world_mut();
    let root = chunk_root(world, WORLD, LodTier::Tier4, IVec2::new(3, 0));
    let node = world.spawn(ChildOf(root)).id();
    for quadrant in 0..4 {
        lod_patch(
            world,
            Some(node),
            IVec2::new(3, 0),
            quadrant,
            Some(LodTier::Tier4),
        );
        near_patch(world, Some(WORLD), IVec2::ZERO, quadrant);
    }
    app.update();
    assert_matches_full_pass(&mut app, "after the first pass");
    let before = work(&app);
    assert_eq!(before[0], 1.0, "the journal sync is the only rebuild");
    assert_eq!(before[1], 8.0, "one walk per patch");

    // Object streaming: scenes of nodes and meshes arrive, move and leave.
    let mut objects = Vec::new();
    for _ in 0..200 {
        let object = app
            .world_mut()
            .spawn((Transform::default(), Visibility::Inherited))
            .id();
        for _ in 0..4 {
            app.world_mut().spawn((
                Mesh3d(Handle::default()),
                Visibility::Inherited,
                ChildOf(object),
            ));
        }
        objects.push(object);
    }
    app.update();
    let parent = app.world_mut().spawn_empty().id();
    app.world_mut()
        .entity_mut(objects[0])
        .insert(ChildOf(parent));
    app.update();
    for object in objects {
        app.world_mut().despawn(object);
    }
    app.update();

    assert_eq!(
        work(&app),
        before,
        "object streaming must not rebuild, walk or evaluate terrain"
    );
    assert_matches_full_pass(&mut app, "after object streaming");
}

#[test]
fn camera_config_and_demand_changes_reselect_without_ancestry_walks() {
    let mut app = app(true);
    let world = app.world_mut();
    for tier in LodTier::ALL {
        let root = chunk_root(world, WORLD, tier, IVec2::new(3, 0));
        let node = world.spawn(ChildOf(root)).id();
        for quadrant in 0..4 {
            lod_patch(world, Some(node), IVec2::new(3, 0), quadrant, Some(tier));
        }
    }
    near_patch(world, Some(WORLD), IVec2::ZERO, 0);
    app.update();
    let before = work(&app);
    for grid_x in [1, 5, 9, 30, 0] {
        move_camera(app.world_mut(), grid_x);
        app.update();
        assert_matches_full_pass(&mut app, "camera moved");
    }
    app.world_mut().resource_mut::<RenderOrigin>().0 = IVec2::new(2, 0);
    app.update();
    assert_matches_full_pass(&mut app, "origin moved");
    app.world_mut()
        .resource_mut::<EngineConfig>()
        .terrain_lod
        .block_level0_distance *= 0.1;
    app.update();
    assert_matches_full_pass(&mut app, "reach changed");
    app.world_mut().resource_mut::<EngineConfig>().worldspace_id = WORLD + 1;
    app.update();
    assert_matches_full_pass(&mut app, "demanded world changed");
    app.world_mut().resource_mut::<EngineConfig>().worldspace_id = WORLD;
    app.update();
    assert_matches_full_pass(&mut app, "demanded world restored");
    let after = work(&app);
    assert_eq!(after[0], before[0], "no rebuild");
    assert_eq!(after[1], before[1], "no ancestry walk");
    assert_eq!(after[2], before[2], "no patch read again");
    assert!(after[3] > before[3], "keys were applied");
}

#[test]
fn a_config_change_writes_every_patch_again_even_with_the_journal() {
    for journal in [true, false] {
        let mut app = app(journal);
        let world = app.world_mut();
        let grid = IVec2::new(3, 0);
        let root = chunk_root(world, WORLD, LodTier::Tier4, grid);
        let patch = lod_patch(world, Some(root), grid, 0, Some(LodTier::Tier4));
        app.update();
        assert_eq!(visibility(&app, patch), Visibility::Inherited);
        // In place: no insert for the journal to see, so only the config change can fix it.
        *app.world_mut().get_mut::<Visibility>(patch).unwrap() = Visibility::Hidden;
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .terrain_lod
            .block_level0_distance += 1.0;
        app.update();
        assert_eq!(
            visibility(&app, patch),
            Visibility::Inherited,
            "journal {journal}: the terrain LOD distances changed"
        );
    }
}

#[test]
fn an_ancestor_change_walks_only_the_patches_below_it() {
    let mut app = app(true);
    let world = app.world_mut();
    let grid = IVec2::new(3, 0);
    let root = chunk_root(world, WORLD, LodTier::Tier4, grid);
    let node = world.spawn(ChildOf(root)).id();
    let below: Vec<_> = (0..4)
        .map(|quadrant| lod_patch(world, Some(node), grid, quadrant, Some(LodTier::Tier4)))
        .collect();
    let other_root = chunk_root(world, WORLD, LodTier::Tier8, grid);
    for quadrant in 0..4 {
        lod_patch(
            world,
            Some(other_root),
            grid,
            quadrant,
            Some(LodTier::Tier8),
        );
    }
    // Refused at first: a full-detail patch whose ancestry names no worldspace.
    let unowned = world.spawn_empty().id();
    let refused = near_patch(world, None, IVec2::ZERO, 0);
    world.entity_mut(refused).insert(ChildOf(unowned));
    app.update();
    assert_matches_full_pass(&mut app, "initial");
    assert_eq!(visibility(&app, below[0]), Visibility::Inherited);

    // The chunk root becomes another world's: only the four patches below it walk again.
    let walked = work(&app)[1];
    app.world_mut()
        .entity_mut(root)
        .insert(root_component(WORLD + 1, LodTier::Tier4, grid));
    app.update();
    assert_eq!(work(&app)[1] - walked, 4.0, "only the dependants walk");
    assert_eq!(visibility(&app, below[0]), Visibility::Hidden);
    assert_matches_full_pass(&mut app, "root of another world");

    // A previously refused ancestry is repaired by the marker appearing on its ancestor.
    app.world_mut()
        .entity_mut(unowned)
        .insert(TerrainWorldspace(WORLD));
    app.update();
    assert_matches_full_pass(&mut app, "world marker added to a refused ancestry");

    // Disabled ancestors refuse, and enabling them restores.
    app.world_mut().entity_mut(other_root).insert(Disabled);
    app.update();
    assert_matches_full_pass(&mut app, "ancestor disabled");
    app.world_mut().entity_mut(other_root).remove::<Disabled>();
    app.update();
    assert_matches_full_pass(&mut app, "ancestor enabled");

    // Reparent the node under the second root: two chunk roots refuse the ancestry.
    app.world_mut().entity_mut(node).insert(ChildOf(other_root));
    app.update();
    assert_matches_full_pass(&mut app, "reparented under a second chunk root");
}

#[test]
fn despawning_an_unobserved_root_refuses_the_patches_below_it() {
    let mut app = app(true);
    let world = app.world_mut();
    let grid = IVec2::new(3, 0);
    // No observed component on the root, and a link that skipped the relationship hooks:
    // despawning the root announces nothing and does not despawn the chunk.
    let root = world.spawn_empty().id();
    let chunk = chunk_root(world, WORLD, LodTier::Tier4, grid);
    world
        .entity_mut(chunk)
        .insert_with_relationship_hook_mode(ChildOf(root), RelationshipHookMode::Skip);
    let patch = lod_patch(world, Some(chunk), grid, 0, Some(LodTier::Tier4));
    app.update();
    assert_eq!(visibility(&app, patch), Visibility::Inherited);
    assert_matches_full_pass(&mut app, "before the root left");
    let _ = app.world_mut().despawn(root);
    app.update();
    assert_eq!(visibility(&app, patch), Visibility::Hidden);
    assert_matches_full_pass(&mut app, "after the root left");
}

#[test]
fn a_cyclic_ancestry_refuses_its_patches() {
    let mut app = app(true);
    let world = app.world_mut();
    let grid = IVec2::new(3, 0);
    let first = world.spawn_empty().id();
    let second = world.spawn(ChildOf(first)).id();
    // The link that closes the loop skips the relationship hooks, which would refuse it.
    world
        .entity_mut(first)
        .insert_with_relationship_hook_mode(ChildOf(second), RelationshipHookMode::Skip);
    let chunk = chunk_root(world, WORLD, LodTier::Tier4, grid);
    world.entity_mut(chunk).insert(ChildOf(first));
    let patch = lod_patch(world, Some(chunk), grid, 0, Some(LodTier::Tier4));
    app.update();
    assert_eq!(visibility(&app, patch), Visibility::Hidden);
    assert_matches_full_pass(&mut app, "cyclic ancestry");
}

#[test]
fn duplicate_patches_count_exactly() {
    let mut app = app(true);
    let world = app.world_mut();
    let grid = IVec2::new(3, 0);
    let root = chunk_root(world, WORLD, LodTier::Tier4, grid);
    let first = lod_patch(world, Some(root), grid, 0, Some(LodTier::Tier4));
    let second = lod_patch(world, Some(root), grid, 0, Some(LodTier::Tier4));
    app.update();
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .visible_lod_terrain_patches,
        2
    );
    app.world_mut()
        .entity_mut(first)
        .remove::<TerrainSurfaceReady>();
    app.update();
    assert_eq!(visibility(&app, second), Visibility::Inherited);
    assert_eq!(visibility(&app, first), Visibility::Hidden);
    assert_matches_full_pass(&mut app, "one duplicate lost readiness");
    let _ = app.world_mut().despawn(second);
    app.update();
    assert_eq!(visibility(&app, first), Visibility::Hidden);
    assert_matches_full_pass(&mut app, "the other duplicate despawned");
    app.world_mut()
        .entity_mut(first)
        .insert(TerrainSurfaceReady);
    app.update();
    assert_eq!(visibility(&app, first), Visibility::Inherited);
    assert_matches_full_pass(&mut app, "readiness restored");
}

/// Ready full-detail and LOD patches that carry no world, no chunk root and no parent. None
/// of their ancestry supplies a world, so the ancestry walk refuses both. Their coverage
/// and readiness are journal-observed. Their keys are outside the random runs' keys.
fn orphan_patches(world: &mut World) -> [Entity; 2] {
    [
        near_patch(world, None, IVec2::new(7, 0), 0),
        lod_patch(world, None, IVec2::new(8, 0), 0, Some(LodTier::Tier4)),
    ]
}

/// One round of unrelated object streaming: scenes of nodes and meshes arrive, are
/// reparented, change or lose their `Visibility`, and leave. No terrain input changes.
fn churn_objects(world: &mut World, objects: &mut Vec<Entity>, round: u32) {
    match round % 5 {
        0 => {
            for _ in 0..20 {
                let object = world
                    .spawn((Transform::default(), Visibility::Inherited))
                    .id();
                for _ in 0..3 {
                    world.spawn((
                        Mesh3d(Handle::default()),
                        Visibility::Inherited,
                        ChildOf(object),
                    ));
                }
                objects.push(object);
            }
        }
        1 => {
            let parent = world.spawn_empty().id();
            if let Some(&object) = objects.first()
                && let Ok(mut object) = world.get_entity_mut(object)
            {
                object.insert(ChildOf(parent));
            }
        }
        2 => {
            for &object in objects.iter() {
                if let Some(mut visibility) = world.get_mut::<Visibility>(object) {
                    *visibility = Visibility::Hidden;
                }
            }
        }
        3 => {
            for (index, &object) in objects.iter().enumerate() {
                if let Ok(mut object) = world.get_entity_mut(object) {
                    if index % 2 == 0 {
                        object.remove::<Visibility>();
                    } else {
                        object.insert(Visibility::Visible);
                    }
                }
            }
        }
        _ => {
            for object in objects.split_off(objects.len() / 2) {
                let _ = world.despawn(object);
            }
            objects.retain(|&object| world.get_entity(object).is_ok());
        }
    }
}

#[test]
fn orphan_patches_cost_no_work_while_only_objects_and_refreshes_change() {
    for disabler in [
        None,
        Some(Disabler::Ready),
        Some(Disabler::Visibility),
        Some(Disabler::Patch),
    ] {
        let mut app = app_with(true, disabler);
        let world = app.world_mut();
        let grid = IVec2::new(3, 0);
        let root = chunk_root(world, WORLD, LodTier::Tier4, grid);
        let far = lod_patch(world, Some(root), grid, 0, Some(LodTier::Tier4));
        near_patch(world, Some(WORLD), IVec2::ZERO, 0);
        let [_, far_orphan] = orphan_patches(world);
        app.update();
        let context = format!("orphans with {disabler:?}");
        assert_matches_full_pass(&mut app, &format!("{context}, first pass"));
        if disabler.is_none() {
            assert_eq!(visibility(&app, far), Visibility::Inherited);
            assert_eq!(
                visibility(&app, far_orphan),
                Visibility::Hidden,
                "an orphan has no world, so it is never drawn"
            );
            assert_eq!(work(&app)[1], 4.0, "one walk per patch, orphans included");
        }
        let before = work(&app);

        let mut objects = Vec::new();
        for round in 0..5 {
            churn_objects(app.world_mut(), &mut objects, round);
            app.update();
            assert_matches_full_pass(&mut app, &format!("{context}, object round {round}"));
        }
        for idle in 0..3 {
            app.update();
            assert_matches_full_pass(&mut app, &format!("{context}, idle frame {idle}"));
        }
        app.world_mut()
            .resource_mut::<LodStreaming>()
            .visibility_dirty = true;
        app.update();
        assert_matches_full_pass(&mut app, &format!("{context}, refresh"));

        let after = work(&app);
        assert_eq!(
            &after[..3],
            &before[..3],
            "{context}: object streaming, idle frames and refreshes must not rebuild, walk \
             or read a patch again"
        );
    }
}

/// Frames in which only unrelated objects and one refresh change, after a settling frame.
/// The index is exact, so no rebuild, ancestry walk or patch read has a reason to happen,
/// whatever the random run left in the world, the orphan patches included. A `Visibility`
/// changed from outside stays held until a frame the old pass refreshed on.
fn object_only_phase(
    app: &mut App,
    objects: &mut Vec<Entity>,
    held: &mut HashSet<Entity>,
    context: &str,
) {
    app.update();
    assert_matches_full_pass_except(app, &format!("{context} settled"), held);
    still_overridden(app, held);
    let before = work(app);
    for round in 0..5 {
        churn_objects(app.world_mut(), objects, round);
        app.update();
        assert_matches_full_pass_except(app, &format!("{context} objects {round}"), held);
        still_overridden(app, held);
    }
    app.world_mut()
        .resource_mut::<LodStreaming>()
        .visibility_dirty = true;
    app.update();
    // A refresh repairs outside changes, as it does in the main loop.
    held.clear();
    assert_matches_full_pass_except(app, &format!("{context} refreshed"), held);
    let after = work(app);
    assert_eq!(
        &after[..3],
        &before[..3],
        "{context}: object-only frames must not rebuild, walk or read a patch again"
    );
}

fn desync_key() -> PatchKey {
    (IVec2::new(3, 0), 0)
}

fn desync_entry(near: bool, counted: bool, visible: bool) -> PatchEntry {
    let (grid, quadrant) = desync_key();
    PatchEntry {
        coverage: TerrainCoverage {
            grid,
            quadrant,
            tier: (!near).then_some(LodTier::Tier4),
        },
        near,
        ready: true,
        world: Some(WORLD),
        path: Vec::new(),
        counted,
        visible,
    }
}

fn tier4_counts(count: u32) -> [u32; LodTier::ALL.len()] {
    let mut counts = [0; LodTier::ALL.len()];
    counts[tier_slot(LodTier::Tier4)] = count;
    counts
}

/// An index whose one key holds `state` and whose aggregate counts are as given.
fn desync_index(state: KeyState, ready: usize, visible: usize) -> TerrainLodIndex {
    let mut index = TerrainLodIndex {
        world: Some(WORLD),
        ready_patches: ready,
        visible_patches: visible,
        ..default()
    };
    index.keys.insert(desync_key(), state);
    index
}

/// Takes a counted LOD patch out of counts that all agree with it.
fn desync_none() -> TerrainLodIndex {
    let entity = Entity::PLACEHOLDER;
    let state = KeyState {
        tier_ready: tier4_counts(1),
        members: HashSet::from([entity]),
        ..default()
    };
    let mut index = desync_index(state, 1, 1);
    index.uncount(entity, &desync_entry(false, true, true));
    index
}

/// Takes a counted full-detail patch out of a key that counts none.
fn desync_near_ready() -> TerrainLodIndex {
    let mut index = desync_index(KeyState::default(), 0, 0);
    index.uncount(Entity::PLACEHOLDER, &desync_entry(true, true, false));
    index
}

/// Takes a counted LOD patch out of a tier that counts none.
fn desync_tier_ready() -> TerrainLodIndex {
    let entity = Entity::PLACEHOLDER;
    let state = KeyState {
        members: HashSet::from([entity]),
        ..default()
    };
    let mut index = desync_index(state, 1, 0);
    index.uncount(entity, &desync_entry(false, true, false));
    index
}

/// Takes a counted LOD patch out of an aggregate ready count that is already zero.
fn desync_ready_patches() -> TerrainLodIndex {
    let entity = Entity::PLACEHOLDER;
    let state = KeyState {
        tier_ready: tier4_counts(1),
        members: HashSet::from([entity]),
        ..default()
    };
    let mut index = desync_index(state, 0, 0);
    index.uncount(entity, &desync_entry(false, true, false));
    index
}

/// Takes a visible patch out of an aggregate visible count that is already zero.
fn desync_visible_on_removal() -> TerrainLodIndex {
    let entity = Entity::PLACEHOLDER;
    let state = KeyState {
        members: HashSet::from([entity]),
        ..default()
    };
    let mut index = desync_index(state, 0, 0);
    index.uncount(entity, &desync_entry(false, false, true));
    index
}

/// Hides a patch the index believes visible while the aggregate visible count is zero.
fn desync_visible_on_reselect() -> TerrainLodIndex {
    let mut world = World::new();
    let (grid, quadrant) = desync_key();
    let entity = world
        .spawn((
            TerrainCoverage {
                grid,
                quadrant,
                tier: Some(LodTier::Tier4),
            },
            Visibility::Inherited,
        ))
        .id();
    let state = KeyState {
        members: HashSet::from([entity]),
        ..default()
    };
    let mut index = desync_index(state, 0, 0);
    index
        .patches
        .insert(entity, desync_entry(false, false, true));
    index.dirty.insert(desync_key());
    let selection = Selection {
        world: Some(WORLD),
        origin: IVec2::ZERO,
        camera_grid: IVec2::ZERO,
        distances: EngineConfig::default().terrain_lod,
        own_statics: false,
    };
    let mut state = SystemState::<Query<LodPatches, LodPatchFilter>>::new(&mut world);
    let mut lod = state.get_mut(&mut world).unwrap();
    index.reselect(&mut lod, &selection);
    index
}

/// Every count of the index, which none of the cases starts above one.
fn desync_counts(index: &TerrainLodIndex) -> Vec<u64> {
    let mut counts = vec![index.ready_patches as u64, index.visible_patches as u64];
    for state in index.keys.values() {
        counts.push(u64::from(state.near_ready));
        counts.extend(state.tier_ready.iter().copied().map(u64::from));
    }
    counts
}

#[test]
fn corrupt_counts_are_reported_as_desyncs_without_panicking_or_wrapping() {
    // Each case corrupts exactly one count and takes a patch out of it, so each must report
    // exactly one desync; the first case is consistent and reports none.
    type TestCase = (&'static str, f64, fn() -> TerrainLodIndex);
    let cases: [TestCase; 6] = [
        ("consistent counts", 0.0, desync_none),
        ("near_ready", 1.0, desync_near_ready),
        ("tier_ready", 1.0, desync_tier_ready),
        ("ready_patches", 1.0, desync_ready_patches),
        ("visible_patches on removal", 1.0, desync_visible_on_removal),
        (
            "visible_patches on reselect",
            1.0,
            desync_visible_on_reselect,
        ),
    ];
    let mut failures = Vec::new();
    for (name, desyncs, run) in cases {
        let Ok(index) = std::panic::catch_unwind(run) else {
            failures.push(format!("{name}: panicked"));
            continue;
        };
        let wrapped: Vec<_> = desync_counts(&index)
            .into_iter()
            .filter(|&count| count > 1)
            .collect();
        if !wrapped.is_empty() {
            failures.push(format!("{name}: a count wrapped to {wrapped:?}"));
        }
        let reported = index.stats.desyncs as f64;
        if reported != desyncs {
            failures.push(format!(
                "{name}: desync count is {reported}, expected {desyncs}"
            ));
        }
    }
    assert!(failures.is_empty(), "corrupt counts: {failures:#?}");
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u32) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 32) as u32) % bound
    }

    fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent
    }
}

struct Pool {
    entities: Vec<Entity>,
    cluster: bool,
}

impl Pool {
    /// A live entity, or `None`.
    fn pick(&self, world: &World, rng: &mut Rng) -> Option<(usize, Entity)> {
        if self.entities.is_empty() {
            return None;
        }
        let index = rng.below(self.entities.len() as u32) as usize;
        let entity = self.entities[index];
        world.get_entity(entity).is_ok().then_some((index, entity))
    }

    /// A live entity created before `index`: parenting only downwards keeps the hierarchy
    /// acyclic.
    fn parent_for(&self, world: &World, rng: &mut Rng, index: usize) -> Option<Entity> {
        if index == 0 {
            return None;
        }
        let candidate = self.entities[rng.below(index as u32) as usize];
        world.get_entity(candidate).is_ok().then_some(candidate)
    }
}

fn random_coverage(rng: &mut Rng, near: bool) -> TerrainCoverage {
    TerrainCoverage {
        grid: IVec2::new(rng.below(6) as i32, rng.below(2) as i32),
        quadrant: rng.below(3) as u8,
        tier: if near || rng.chance(10) {
            None
        } else {
            Some(LodTier::ALL[rng.below(3) as usize])
        },
    }
}

fn random_worldspace(rng: &mut Rng) -> u32 {
    WORLD + rng.below(2)
}

fn random_root(rng: &mut Rng) -> LodChunkRoot {
    root_component(
        random_worldspace(rng),
        LodTier::ALL[rng.below(3) as usize],
        IVec2::new(rng.below(6) as i32, rng.below(2) as i32),
    )
}

fn random_cluster_terrain(rng: &mut Rng) -> ClusterTerrain {
    let blocks: Vec<_> = (0..1 + rng.below(3))
        .map(|_| {
            let cells: Vec<_> = (0..1 + rng.below(3))
                .map(|_| IVec2::new(rng.below(6) as i32, rng.below(2) as i32))
                .collect();
            let batch = (!rng.chance(20)).then(|| rng.below(3));
            (random_worldspace(rng), cells, batch)
        })
        .collect();
    let blocks: Vec<_> = blocks
        .iter()
        .map(|(world, cells, batch)| (*world, cells.as_slice(), *batch))
        .collect();
    cluster_terrain(&blocks)
}

fn mutate_cluster(world: &mut World, rng: &mut Rng) {
    match rng.below(8) {
        0 => {
            if let Some(ready) = world.get_resource::<TerrainDrawReadiness>() {
                ready.mark(rng.below(3));
            }
        }
        1 => {
            if let Some(ready) = world.get_resource::<TerrainDrawReadiness>() {
                ready.clear();
            }
        }
        2 => {
            world.remove_resource::<ClusterTerrain>();
        }
        3 => {
            world.remove_resource::<TerrainDrawReadiness>();
        }
        4 => {
            let terrain = random_cluster_terrain(rng);
            world.insert_resource(terrain);
        }
        5 => {
            // A replacement that starts over: fresh state, perhaps the same revision as the
            // resource it replaces.
            let ready = TerrainDrawReadiness::default();
            if rng.chance(70) {
                ready.mark(rng.below(3));
            }
            world.insert_resource(ready);
        }
        6 => {
            if let Some(mut terrain) = world.get_resource_mut::<ClusterTerrain>()
                && !terrain.blocks.is_empty()
            {
                let index = rng.below(terrain.blocks.len() as u32) as usize;
                terrain.blocks[index].batch = (!rng.chance(25)).then(|| rng.below(3));
            }
        }
        _ => {
            let mut config = world.resource_mut::<EngineConfig>();
            config.cluster_own_statics = !config.cluster_own_statics;
        }
    }
}

/// Changes the `Visibility` of a LOD patch in place, as an outside system would. Returns it.
fn override_visibility(world: &mut World, pool: &Pool, rng: &mut Rng) -> Option<Entity> {
    let (_, entity) = pool.pick(world, rng)?;
    let value = [
        Visibility::Hidden,
        Visibility::Inherited,
        Visibility::Visible,
    ][rng.below(3) as usize];
    let mut visibility = world.get_mut::<Visibility>(entity)?;
    // A full-detail patch is never written by the pass; the comparison leaves it out.
    visibility.set_if_neq(value);
    Some(entity)
}

/// One random mutation of the terrain world, and the patch whose `Visibility` it changed
/// from outside, if it did.
fn mutate(world: &mut World, pool: &mut Pool, rng: &mut Rng) -> Option<Entity> {
    match rng.below(12) {
        0 => override_visibility(world, pool, rng),
        1 if pool.cluster => {
            mutate_cluster(world, rng);
            None
        }
        _ => {
            mutate_terrain(world, pool, rng);
            None
        }
    }
}

/// One random mutation of the terrain world.
fn mutate_terrain(world: &mut World, pool: &mut Pool, rng: &mut Rng) {
    match rng.below(20) {
        0 | 1 => {
            let coverage = random_coverage(rng, false);
            let mut patch = world.spawn((coverage, Visibility::Inherited));
            if rng.chance(85) {
                patch.insert(TerrainSurfaceReady);
            }
            if rng.chance(8) {
                patch.remove::<Visibility>();
            }
            let patch = patch.id();
            pool.entities.push(patch);
            if let Some(parent) = pool.parent_for(world, rng, pool.entities.len() - 1) {
                world.entity_mut(patch).insert(ChildOf(parent));
            }
        }
        2 => {
            let coverage = random_coverage(rng, true);
            let mut patch = world.spawn((TerrainPatch, coverage));
            if rng.chance(85) {
                patch.insert(TerrainSurfaceReady);
            }
            if rng.chance(60) {
                patch.insert(TerrainWorldspace(random_worldspace(rng)));
            }
            let patch = patch.id();
            pool.entities.push(patch);
            if let Some(parent) = pool.parent_for(world, rng, pool.entities.len() - 1) {
                world.entity_mut(patch).insert(ChildOf(parent));
            }
        }
        3 => {
            let node = world.spawn_empty().id();
            if rng.chance(30) {
                world
                    .entity_mut(node)
                    .insert(TerrainWorldspace(random_worldspace(rng)));
            }
            pool.entities.push(node);
            if let Some(parent) = pool.parent_for(world, rng, pool.entities.len() - 1) {
                // Some links skip the relationship hooks, as an unobserved insert does.
                let mode = if rng.chance(15) {
                    RelationshipHookMode::Skip
                } else {
                    RelationshipHookMode::Run
                };
                world
                    .entity_mut(node)
                    .insert_with_relationship_hook_mode(ChildOf(parent), mode);
            }
        }
        4 | 5 => {
            let root = world.spawn(random_root(rng)).id();
            pool.entities.push(root);
            if rng.chance(20)
                && let Some(parent) = pool.parent_for(world, rng, pool.entities.len() - 1)
            {
                world.entity_mut(root).insert(ChildOf(parent));
            }
        }
        6 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                let _ = world.despawn(entity);
            }
        }
        7 | 8 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                let mut entity = world.entity_mut(entity);
                if entity.contains::<TerrainSurfaceReady>() {
                    entity.remove::<TerrainSurfaceReady>();
                } else {
                    entity.insert(TerrainSurfaceReady);
                }
            }
        }
        9 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                let coverage = random_coverage(rng, false);
                if let Some(mut current) = world.get_mut::<TerrainCoverage>(entity) {
                    *current = coverage;
                }
            }
        }
        10 => {
            if let Some((index, entity)) = pool.pick(world, rng)
                && let Some(parent) = pool.parent_for(world, rng, index)
            {
                world.entity_mut(entity).insert(ChildOf(parent));
            }
        }
        11 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                world.entity_mut(entity).remove::<ChildOf>();
            }
        }
        12 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                if rng.chance(70) {
                    world
                        .entity_mut(entity)
                        .insert(TerrainWorldspace(random_worldspace(rng)));
                } else {
                    world.entity_mut(entity).remove::<TerrainWorldspace>();
                }
            }
        }
        13 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                let value = random_worldspace(rng);
                if let Some(mut current) = world.get_mut::<TerrainWorldspace>(entity) {
                    current.0 = value;
                }
            }
        }
        14 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                if rng.chance(60) {
                    let root = random_root(rng);
                    world.entity_mut(entity).insert(root);
                } else {
                    world.entity_mut(entity).remove::<LodChunkRoot>();
                }
            }
        }
        15 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                let mut entity = world.entity_mut(entity);
                if entity.contains::<Disabled>() {
                    entity.remove::<Disabled>();
                } else if rng.chance(50) {
                    entity.insert(Disabled);
                } else if entity.contains::<CustomDisabled>() {
                    entity.remove::<CustomDisabled>();
                } else {
                    entity.insert(CustomDisabled);
                }
            }
        }
        16 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                let mut entity = world.entity_mut(entity);
                if entity.contains::<TerrainPatch>() {
                    entity.remove::<TerrainPatch>();
                } else {
                    entity.insert(TerrainPatch);
                }
            }
        }
        17 => {
            if let Some((_, entity)) = pool.pick(world, rng) {
                let mut entity = world.entity_mut(entity);
                if entity.contains::<Visibility>() {
                    entity.remove::<Visibility>();
                } else {
                    entity.insert(Visibility::Inherited);
                }
            }
        }
        18 => move_camera(world, rng.below(12) as i32),
        _ => match rng.below(3) {
            0 => {
                let value = random_worldspace(rng);
                world.resource_mut::<EngineConfig>().worldspace_id = value;
            }
            1 => {
                let factor = if rng.chance(50) { 0.5 } else { 2.0 };
                world
                    .resource_mut::<EngineConfig>()
                    .terrain_lod
                    .block_level0_distance *= factor;
            }
            _ => {
                let radius = 8 + rng.below(4) as i32;
                world.resource_mut::<EngineConfig>().stream_radius = radius;
            }
        },
    }
}

/// What a random run is built from.
#[derive(Clone, Copy, Debug)]
struct Scenario {
    journal: bool,
    disabler: Option<Disabler>,
    cluster: bool,
}

impl Scenario {
    const fn new(journal: bool) -> Self {
        Self {
            journal,
            disabler: None,
            cluster: false,
        }
    }
}

/// Patches in `held` whose `Visibility` is still not what a full pass gives.
fn still_overridden(app: &mut App, held: &mut HashSet<Entity>) {
    let expected = reference(app.world_mut());
    held.retain(|entity| {
        expected
            .visibility
            .get(entity)
            .is_some_and(|want| visibility(app, *entity) != *want)
    });
}

/// The incremental pass (and, without a journal, the rebuild pass) must equal a full pass
/// after every step of a random sequence of terrain mutations. The pass itself also audits
/// its index against a rebuilt one in this build.
///
/// A `Visibility` changed from outside is only written again on a frame the old pass would
/// have refreshed on, so such a patch is left out of the comparison until a step that forces
/// a refresh (the dirty flag) or until it equals a full pass again.
///
/// With a journal that covers the pass's query domains, two orphan patches (ready, no world,
/// no chunk root, no parent) join the world, and every twentieth step is followed by
/// object-only frames that must not cost a rebuild, an ancestry walk or a patch read.
fn run_oracle(scenario: Scenario, seed: u64, steps: u32) {
    let Scenario {
        journal,
        disabler,
        cluster,
    } = scenario;
    let mut app = app_with(journal, disabler);
    if cluster {
        enable_cluster_terrain(&mut app);
        let mut rng = Rng(seed ^ 0xc1u64);
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = true;
        app.insert_resource(random_cluster_terrain(&mut rng));
        let ready = TerrainDrawReadiness::default();
        ready.mark(0);
        app.insert_resource(ready);
    }
    let mut pool = Pool {
        entities: Vec::new(),
        cluster,
    };
    let mut rng = Rng(seed);
    let mut held = HashSet::new();
    // Supplemental disabler observers recover once, then share the same settled contract.
    let object_phases = journal;
    let mut objects = Vec::new();
    if object_phases {
        orphan_patches(app.world_mut());
    }
    app.update();
    app.update();
    for step in 0..steps {
        for _ in 0..1 + rng.below(3) {
            held.extend(mutate(app.world_mut(), &mut pool, &mut rng));
        }
        let forced = !journal || rng.chance(25);
        if forced {
            app.world_mut()
                .resource_mut::<LodStreaming>()
                .visibility_dirty = true;
        }
        let context = format!("{scenario:?} seed {seed:#x} step {step}");
        app.update();
        if forced {
            held.clear();
        }
        assert_matches_full_pass_except(&mut app, &context, &held);
        still_overridden(&mut app, &mut held);
        if rng.chance(15) {
            app.update();
            assert_matches_full_pass_except(&mut app, &format!("{context} idle"), &held);
            still_overridden(&mut app, &mut held);
        }
        if object_phases && step % 20 == 10 {
            object_only_phase(&mut app, &mut objects, &mut held, &context);
        }
    }
}

#[test]
fn incremental_visibility_equals_a_full_pass_under_random_mutation() {
    for seed in [
        0x9e37_79b9_7f4a_7c15,
        0x1234_5678_9abc_def1,
        42,
        0xdead_beef,
    ] {
        run_oracle(Scenario::new(true), seed, 300);
    }
}

#[test]
fn incremental_visibility_keeps_the_original_query_domains_for_disablers() {
    for disabler in [
        Disabler::Ready,
        Disabler::Visibility,
        Disabler::Patch,
        Disabler::Custom,
    ] {
        for seed in [0x51ed_270b_1234_5678, 99] {
            run_oracle(
                Scenario {
                    disabler: Some(disabler),
                    ..Scenario::new(true)
                },
                seed,
                150,
            );
        }
    }
}

#[test]
fn rebuilt_visibility_equals_a_full_pass_under_random_mutation() {
    for seed in [0xfeed_face_cafe_beef, 7] {
        run_oracle(Scenario::new(false), seed, 200);
    }
}

#[test]
fn without_the_journal_the_pass_still_refreshes_on_its_old_triggers() {
    let mut app = app(false);
    let world = app.world_mut();
    let grid = IVec2::new(3, 0);
    let root = chunk_root(world, WORLD, LodTier::Tier4, grid);
    let patch = lod_patch(world, Some(root), grid, 0, Some(LodTier::Tier4));
    app.update();
    assert_eq!(visibility(&app, patch), Visibility::Inherited);
    *app.world_mut().get_mut::<Visibility>(patch).unwrap() = Visibility::Hidden;
    app.update();
    assert_eq!(
        visibility(&app, patch),
        Visibility::Hidden,
        "nothing changed, so nothing is rewritten"
    );
    let _ = app.world_mut().despawn(patch);
    app.update();
    assert_eq!(work(&app)[0], 2.0);
}

#[test]
fn without_the_journal_coverage_changes_refresh_immediately() {
    let mut app = app(false);
    let world = app.world_mut();
    near_patch(world, Some(WORLD), IVec2::ZERO, 0);
    let grid = IVec2::new(3, 0);
    let root = chunk_root(world, WORLD, LodTier::Tier4, grid);
    let patch = lod_patch(world, Some(root), grid, 0, Some(LodTier::Tier4));
    app.update();
    assert_eq!(visibility(&app, patch), Visibility::Inherited);
    assert_matches_full_pass(&mut app, "before the idle coverage change");
    let before = work(&app);

    // An in-place coverage change must refresh even without the identity journal.
    // Moving this patch under ready full detail changes the full-pass answer.
    app.world_mut()
        .get_mut::<TerrainCoverage>(patch)
        .unwrap()
        .grid = IVec2::ZERO;
    assert_eq!(
        reference(app.world_mut()).visibility[&patch],
        Visibility::Hidden,
        "the coverage mutation changes the full-pass answer"
    );
    app.update();
    assert_eq!(visibility(&app, patch), Visibility::Hidden);
    assert_eq!(work(&app)[0], before[0] + 1.0, "coverage refreshes once");
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .visible_lod_terrain_patches,
        0
    );
    assert_matches_full_pass(&mut app, "after the immediate coverage refresh");
    let refreshed = work(&app);
    for _ in 0..2 {
        app.update();
        assert_eq!(visibility(&app, patch), Visibility::Hidden);
        assert_eq!(work(&app), refreshed, "idle frames do no additional work");
    }

    app.world_mut()
        .resource_mut::<LodStreaming>()
        .visibility_dirty = true;
    app.update();
    assert_eq!(visibility(&app, patch), Visibility::Hidden);
    assert_eq!(work(&app)[0], refreshed[0] + 1.0, "the refresh rebuilds");
    assert_matches_full_pass(&mut app, "after the forced refresh");
}

/// The frames the old pass refreshed on, each one that leaves the selected tier of the
/// patch under test as it was.
#[derive(Clone, Copy, Debug)]
enum Refresh {
    Camera,
    Origin,
    TerrainDistances,
    Flag,
    UnrelatedRemoval,
}

/// Four patches of one grid under a Tier4 root, four of the next grid, and one alone at a
/// third grid. Returns the patch the
/// tests change from outside and the lone one.
fn override_scene(journal: bool) -> (App, Entity, Entity) {
    let mut app = app(journal);
    let world = app.world_mut();
    let root = chunk_root(world, WORLD, LodTier::Tier4, IVec2::new(3, 0));
    let mut target = None;
    for grid_x in [3, 4] {
        for quadrant in 0..4 {
            let patch = lod_patch(
                world,
                Some(root),
                IVec2::new(grid_x, 0),
                quadrant,
                Some(LodTier::Tier4),
            );
            target.get_or_insert(patch);
        }
    }
    let alone = lod_patch(world, Some(root), IVec2::new(5, 0), 0, Some(LodTier::Tier4));
    (app, target.unwrap(), alone)
}

#[test]
fn an_outside_visibility_change_is_repaired_on_every_frame_the_old_pass_refreshed() {
    for journal in [true, false] {
        for refresh in [
            Refresh::Camera,
            Refresh::Origin,
            Refresh::TerrainDistances,
            Refresh::Flag,
            Refresh::UnrelatedRemoval,
        ] {
            let context = format!("journal {journal}, {refresh:?}");
            let (mut app, target, alone) = override_scene(journal);
            app.update();
            assert_eq!(visibility(&app, target), Visibility::Inherited, "{context}");
            // In place: nothing for an observer to see, and the tier stays selected.
            *app.world_mut().get_mut::<Visibility>(target).unwrap() = Visibility::Hidden;
            for _ in 0..3 {
                app.update();
                assert_eq!(
                    visibility(&app, target),
                    Visibility::Hidden,
                    "{context}: frames with no refresh leave an outside change alone"
                );
            }
            let before = work(&app);
            match refresh {
                Refresh::Camera => move_camera(app.world_mut(), 1),
                Refresh::Origin => app.world_mut().resource_mut::<RenderOrigin>().0.x = 1,
                Refresh::TerrainDistances => {
                    app.world_mut()
                        .resource_mut::<EngineConfig>()
                        .terrain_lod
                        .block_level0_distance += 1.0;
                }
                Refresh::Flag => {
                    app.world_mut()
                        .resource_mut::<LodStreaming>()
                        .visibility_dirty = true;
                }
                Refresh::UnrelatedRemoval => {
                    let _ = app.world_mut().despawn(alone);
                }
            }
            app.update();
            assert_eq!(
                visibility(&app, target),
                Visibility::Inherited,
                "{context}: the refresh writes the patch again"
            );
            assert_matches_full_pass(&mut app, &context);
            if journal && !matches!(refresh, Refresh::TerrainDistances) {
                // The repair is the key of the changed patch and nothing else: the camera
                // reselects every key but applies only those whose answer changed.
                assert_eq!(
                    work(&app)[3] - before[3],
                    1.0,
                    "{context}: only the overridden key is applied"
                );
            }
        }
    }
}

#[test]
fn an_outside_visibility_change_is_left_alone_while_other_keys_change() {
    let (mut app, target, alone) = override_scene(true);
    app.update();
    *app.world_mut().get_mut::<Visibility>(target).unwrap() = Visibility::Hidden;
    let before = work(&app);
    // Two other keys gain patches: normal incremental work, and no frame the old pass
    // refreshed on (a lost readiness or coverage would be one).
    let world = app.world_mut();
    let root = chunk_root(world, WORLD, LodTier::Tier4, IVec2::new(6, 0));
    let first = lod_patch(world, Some(root), IVec2::new(6, 0), 0, Some(LodTier::Tier4));
    let second = lod_patch(world, Some(root), IVec2::new(7, 0), 0, Some(LodTier::Tier4));
    app.update();
    assert_eq!(visibility(&app, first), Visibility::Inherited);
    assert_eq!(visibility(&app, second), Visibility::Inherited);
    assert_eq!(visibility(&app, alone), Visibility::Inherited);
    assert_eq!(
        visibility(&app, target),
        Visibility::Hidden,
        "no refresh trigger, so the outside change stays"
    );
    assert_eq!(work(&app)[3] - before[3], 2.0, "two keys, both changed");
    assert_eq!(work(&app)[0], before[0], "no rebuild");
}

#[test]
fn a_default_disabler_the_journal_does_not_follow_recovers_then_uses_events() {
    let mut app = app_with(true, Some(Disabler::Custom));
    let world = app.world_mut();
    let grid = IVec2::new(3, 0);
    let root = chunk_root(world, WORLD, LodTier::Tier4, grid);
    let far = lod_patch(world, Some(root), grid, 0, Some(LodTier::Tier4));
    let near = near_patch(world, Some(WORLD), grid, 0);
    app.update();
    assert_eq!(
        visibility(&app, far),
        Visibility::Hidden,
        "ready full detail covers the key"
    );
    assert_matches_full_pass(&mut app, "before the disabler");
    let rebuilds = work(&app)[0];
    app.world_mut().entity_mut(near).insert(CustomDisabled);
    app.update();
    assert_eq!(visibility(&app, far), Visibility::Inherited);
    assert_matches_full_pass(&mut app, "near patch disabled");
    app.update();
    app.update();
    assert_eq!(
        work(&app)[0] - rebuilds,
        1.0,
        "one recovery after deferred observer installation"
    );
    let settled = work(&app);
    app.update();
    assert_eq!(
        work(&app),
        settled,
        "settled frames perform no additional work"
    );
    app.world_mut().entity_mut(near).remove::<CustomDisabled>();
    app.update();
    assert_eq!(visibility(&app, far), Visibility::Hidden);
    assert_eq!(
        work(&app)[0],
        settled[0],
        "enabling uses the supplemental journal"
    );
    assert_matches_full_pass(&mut app, "near patch enabled");
}

#[test]
fn journal_loss_warns_once_until_a_complete_read_ends_the_episode() {
    let mut index = TerrainLodIndex::default();
    assert!(!index.note_journal_read(true));
    assert!(index.note_journal_read(false));
    assert!(!index.note_journal_read(false));
    assert!(!index.note_journal_read(false));
    assert!(!index.note_journal_read(true));
    assert!(index.note_journal_read(false));
    assert!(!index.note_journal_read(false));
}

#[test]
fn audit_returns_descriptive_failures_for_corrupt_counts_and_selection() {
    let mut world = World::new();
    let grid = IVec2::new(3, 0);
    let root = chunk_root(&mut world, WORLD, LodTier::Tier4, grid);
    let patch = lod_patch(&mut world, Some(root), grid, 0, Some(LodTier::Tier4));
    let mut state = SystemState::<TerrainLodInputs>::new(&mut world);
    let mut inputs = state.get_mut(&mut world).unwrap();
    let selection = Selection {
        world: Some(WORLD),
        origin: IVec2::ZERO,
        camera_grid: IVec2::ZERO,
        distances: EngineConfig::default().terrain_lod,
        own_statics: false,
    };
    let mut index = TerrainLodIndex::default();
    index.sync_selection(selection);
    index.rebuild(&inputs);
    index.reselect(&mut inputs.lod_visibility.p0(), &selection);
    assert!(index.audit(&mut inputs).is_ok());

    let key = (grid, 0);
    index.keys.get_mut(&key).unwrap().tier_ready[tier_slot(LodTier::Tier4)] = 0;
    let error = index.audit(&mut inputs).unwrap_err();
    assert!(error.contains("key"), "{error}");
    assert!(error.contains("differs from full pass"), "{error}");
    index.keys.get_mut(&key).unwrap().tier_ready[tier_slot(LodTier::Tier4)] = 1;
    index.ready_patches = 0;
    assert!(
        index
            .audit(&mut inputs)
            .unwrap_err()
            .contains("ready_patches")
    );
    index.ready_patches = 1;
    index.keys.get_mut(&key).unwrap().selected = None;
    assert!(index.audit(&mut inputs).unwrap_err().contains("selects"));
    index.keys.get_mut(&key).unwrap().selected = Some(LodTier::Tier4);
    index.patches.get_mut(&patch).unwrap().visible = false;
    assert!(index.audit(&mut inputs).unwrap_err().contains("visibility"));
}

#[test]
fn removing_the_last_observed_marker_restores_ancestor_liveness_polling() {
    let mut app = app(true);
    let world = app.world_mut();
    let grid = IVec2::new(3, 0);
    let root = world.spawn(Visibility::Inherited).id();
    let chunk = chunk_root(world, WORLD, LodTier::Tier4, grid);
    world
        .entity_mut(chunk)
        .insert_with_relationship_hook_mode(ChildOf(root), RelationshipHookMode::Skip);
    let patch = lod_patch(world, Some(chunk), grid, 0, Some(LodTier::Tier4));
    app.update();
    assert_eq!(visibility(&app, patch), Visibility::Inherited);
    app.world_mut().entity_mut(root).remove::<Visibility>();
    app.update();
    assert_matches_full_pass(&mut app, "last observed marker removed");
    // The now-empty root has no observed removal and its ChildOf backlink was skipped.
    app.world_mut().despawn(root);
    app.update();
    assert_eq!(visibility(&app, patch), Visibility::Hidden);
    assert_matches_full_pass(&mut app, "empty root despawned");
}

#[test]
fn audit_checks_ecs_visibility_but_allows_held_overrides() {
    let mut world = World::new();
    let grid = IVec2::new(3, 0);
    let root = chunk_root(&mut world, WORLD, LodTier::Tier4, grid);
    let patch = lod_patch(&mut world, Some(root), grid, 0, Some(LodTier::Tier4));
    let mut state = SystemState::<TerrainLodInputs>::new(&mut world);
    let mut inputs = state.get_mut(&mut world).unwrap();
    let selection = Selection {
        world: Some(WORLD),
        origin: IVec2::ZERO,
        camera_grid: IVec2::ZERO,
        distances: EngineConfig::default().terrain_lod,
        own_statics: false,
    };
    let mut index = TerrainLodIndex::default();
    index.sync_selection(selection);
    index.rebuild(&inputs);
    index.reselect(&mut inputs.lod_visibility.p0(), &selection);
    assert!(index.audit(&mut inputs).is_ok());
    {
        let mut patches = inputs.lod_visibility.p0();
        let (_, _, _, mut actual) = patches.get_mut(patch).unwrap();
        assert_eq!(*actual, Visibility::Inherited);
        *actual = Visibility::Hidden;
    }
    // External visibility edits are deliberately held until an original refresh trigger. The
    // audit must exempt exactly the edits recorded in the override set, without changing them.
    index.overrides.insert(patch);
    assert!(index.audit(&mut inputs).is_ok());
    index.overrides.remove(&patch);
    let error = index.audit(&mut inputs).unwrap_err();
    assert!(error.contains("Visibility"), "{error}");
    let patches = inputs.lod_visibility.p0();
    assert_eq!(*patches.get(patch).unwrap().3, Visibility::Hidden);
}

#[test]
fn active_main_world_and_no_demand_reselect_from_cached_worlds() {
    for journal in [true, false] {
        let mut app = app(journal);
        let grid = IVec2::new(3, 0);
        let first_root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
        let first = lod_patch(
            app.world_mut(),
            Some(first_root),
            grid,
            0,
            Some(LodTier::Tier4),
        );
        let second_root = chunk_root(app.world_mut(), WORLD + 1, LodTier::Tier4, grid);
        let second = lod_patch(
            app.world_mut(),
            Some(second_root),
            grid,
            0,
            Some(LodTier::Tier4),
        );
        let check = |app: &mut App, first_visible: bool, second_visible: bool, context: &str| {
            app.update();
            for (patch, drawn) in [(first, first_visible), (second, second_visible)] {
                assert_eq!(
                    visibility(app, patch),
                    if drawn {
                        Visibility::Inherited
                    } else {
                        Visibility::Hidden
                    },
                    "{context}: {patch}"
                );
            }
            assert_matches_full_pass(app, context);
        };
        check(&mut app, true, false, "configured main world");
        let before = work(&app);
        app.insert_resource(ActiveSpace {
            worldspace_id: Some(WORLD + 1),
            interior: None,
        });
        check(
            &mut app,
            false,
            true,
            "active exterior overrides configured world",
        );
        app.world_mut().resource_mut::<ActiveSpace>().interior = Some(10);
        check(&mut app, false, false, "interior withdraws exterior demand");
        app.world_mut().resource_mut::<ActiveSpace>().interior = None;
        check(&mut app, false, true, "active exterior restored");
        app.world_mut().resource_mut::<EngineConfig>().stream_radius = -1;
        check(
            &mut app,
            false,
            false,
            "disabled streaming withdraws demand",
        );
        app.world_mut().resource_mut::<EngineConfig>().stream_radius = 8;
        check(&mut app, false, true, "streaming restored");
        app.world_mut().remove_resource::<ActiveSpace>();
        check(&mut app, true, false, "configured main world restored");
        if journal {
            let after = work(&app);
            assert_eq!(
                &after[..3],
                &before[..3],
                "public demand changes use cached worlds"
            );
            assert!(
                after[3] > before[3],
                "public demand changes apply selected keys"
            );
        }
    }
}

#[test]
fn merged_coverage_world_mutation_and_identity_loss_release_stock_lod() {
    for journal in [false, true] {
        let mut app = app(journal);
        let grid = IVec2::new(3, 0);
        let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
        let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
        let merged = app
            .world_mut()
            .spawn((
                TerrainPatch,
                TerrainSurfaceReady,
                TerrainWorldspace(WORLD),
                MergedTerrainCoverage { grid },
            ))
            .id();
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Hidden);
        app.world_mut()
            .get_mut::<TerrainWorldspace>(merged)
            .unwrap()
            .0 = WORLD + 1;
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        app.world_mut()
            .get_mut::<TerrainWorldspace>(merged)
            .unwrap()
            .0 = WORLD;
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Hidden);
        app.world_mut()
            .entity_mut(merged)
            .remove::<TerrainWorldspace>();
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
    }
}

fn assert_merged_membership_refresh(journal: bool, clear_journal: bool, disabled: bool) {
    let mut app = if disabled {
        app_with(true, Some(Disabler::Custom))
    } else {
        app(journal)
    };
    let grid = IVec2::new(3, 0);
    let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
    let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
    let merged = app
        .world_mut()
        .spawn((
            TerrainPatch,
            TerrainSurfaceReady,
            TerrainWorldspace(WORLD),
            MergedTerrainCoverage { grid },
        ))
        .id();
    app.update();
    app.update();
    assert_eq!(visibility(&app, far), Visibility::Hidden);
    if disabled {
        app.world_mut().entity_mut(merged).insert(CustomDisabled);
    } else {
        app.world_mut().entity_mut(merged).remove::<TerrainPatch>();
    }
    if clear_journal {
        app.world_mut()
            .resource_mut::<Messages<TerrainNodeChanged>>()
            .clear();
    }
    app.update();
    assert_eq!(
        visibility(&app, far),
        Visibility::Inherited,
        "merged membership was removed but cached full-detail proof still hides stock"
    );
    if disabled {
        app.world_mut()
            .entity_mut(merged)
            .remove::<CustomDisabled>();
    } else {
        app.world_mut().entity_mut(merged).insert(TerrainPatch);
    }
    app.update();
    assert_eq!(visibility(&app, far), Visibility::Hidden);
    assert_matches_full_pass(&mut app, "merged membership restored");
}

#[test]
fn merged_membership_removal_releases_stock_lod() {
    for journal in [false, true] {
        assert_merged_membership_refresh(journal, false, false);
    }
}
#[test]
fn lost_journal_merged_membership_releases_stock_lod() {
    assert_merged_membership_refresh(true, true, false);
}
#[test]
fn default_disabler_merged_membership_releases_stock_lod() {
    assert_merged_membership_refresh(true, false, true);
}

#[test]
fn never_ready_lod_keeps_authored_visibility_but_withdrawn_readiness_hides_it() {
    for journal in [false, true] {
        for lost in [false, true] {
            let mut app = app(journal);
            let grid = IVec2::new(3, 0);
            let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
            let patch = app
                .world_mut()
                .spawn((
                    ChildOf(root),
                    TerrainCoverage {
                        grid,
                        quadrant: 0,
                        tier: Some(LodTier::Tier4),
                    },
                    Visibility::Inherited,
                ))
                .id();
            app.update();
            assert_eq!(
                visibility(&app, patch),
                Visibility::Inherited,
                "stock selection does not own a source that has never become ready"
            );
            assert_eq!(
                app.world()
                    .resource::<StreamingMetrics>()
                    .visible_lod_terrain_patches,
                0
            );
            app.world_mut()
                .entity_mut(patch)
                .insert(TerrainSurfaceReady);
            app.update();
            assert_eq!(visibility(&app, patch), Visibility::Inherited);
            assert_eq!(
                app.world()
                    .resource::<StreamingMetrics>()
                    .visible_lod_terrain_patches,
                1
            );
            app.world_mut()
                .entity_mut(patch)
                .remove::<TerrainSurfaceReady>();
            if journal && lost {
                app.world_mut()
                    .resource_mut::<Messages<TerrainNodeChanged>>()
                    .clear();
            }
            app.update();
            assert_eq!(
                visibility(&app, patch),
                Visibility::Hidden,
                "withdrawing previously selected readiness must hide the old draw"
            );
            assert_eq!(
                app.world()
                    .resource::<StreamingMetrics>()
                    .visible_lod_terrain_patches,
                0
            );
            app.world_mut()
                .entity_mut(patch)
                .insert(TerrainSurfaceReady);
            app.update();
            assert_eq!(visibility(&app, patch), Visibility::Inherited);
            assert_matches_full_pass(&mut app, "readiness restored");
        }
    }
}

#[test]
fn visibility_inputs_exclude_unrelated_visibility_archetypes() {
    let mut world = World::new();
    let grid = IVec2::new(3, 0);
    let root = chunk_root(&mut world, WORLD, LodTier::Tier4, grid);
    let patch = lod_patch(&mut world, Some(root), grid, 0, Some(LodTier::Tier4));
    let near = near_patch(&mut world, Some(WORLD), grid, 0);
    let objects: Vec<_> = (0..256)
        .map(|_| {
            world
                .spawn((Transform::default(), Visibility::Inherited))
                .id()
        })
        .collect();
    // Initialization rejects any Visibility access in TerrainLodInputs that overlaps
    // these unrelated rows. This catches the former unrestricted p2/p3 queries,
    // even when their values would be discarded after iteration.
    let mut state = SystemState::<(
        TerrainLodInputs,
        Query<&mut Visibility, Without<TerrainCoverage>>,
    )>::new(&mut world);
    let (mut inputs, _) = state.get_mut(&mut world).unwrap();
    let mut changed = Vec::new();
    inputs.changed_visibility(&mut changed);
    assert_eq!(changed, vec![(patch, Visibility::Hidden)]);
    let changed_query = inputs.lod_visibility.p1();
    assert!(changed_query.get(near).is_err());
    for object in objects {
        assert!(changed_query.get(object).is_err());
    }
}

#[test]
fn actual_terrain_dependencies_still_invalidate_selection() {
    for journal in [false, true] {
        let mut app = app(journal);
        let grid = IVec2::new(3, 0);
        let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
        let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
        let near = near_patch(app.world_mut(), Some(WORLD), grid, 0);
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Hidden);
        app.world_mut()
            .entity_mut(near)
            .remove::<TerrainSurfaceReady>();
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        app.world_mut()
            .get_mut::<LodChunkRoot>(root)
            .unwrap()
            .key
            .worldspace_id = WORLD + 1;
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Hidden);
        app.world_mut()
            .get_mut::<LodChunkRoot>(root)
            .unwrap()
            .key
            .worldspace_id = WORLD;
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        *app.world_mut().get_mut::<Visibility>(far).unwrap() = Visibility::Hidden;
        app.update();
        assert_eq!(
            visibility(&app, far),
            Visibility::Hidden,
            "override waits for a trigger"
        );
        move_camera(app.world_mut(), 1);
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_matches_full_pass(&mut app, "real dependencies after unrelated-scan removal");
    }
}

#[test]
fn supplemental_disabler_journal_recovers_before_settling() {
    let mut app = app_with(true, Some(Disabler::Custom));
    let grid = IVec2::new(3, 0);
    let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
    let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
    let near = near_patch(app.world_mut(), Some(WORLD), grid, 0);
    app.update();
    assert_eq!(visibility(&app, far), Visibility::Hidden);
    app.world_mut().entity_mut(near).insert(CustomDisabled);
    // Recovery must cover the installation interval even with no usable event.
    app.world_mut()
        .resource_mut::<Messages<TerrainNodeChanged>>()
        .clear();
    app.update();
    assert_eq!(visibility(&app, far), Visibility::Inherited);
    let settled = work(&app);
    for _ in 0..4 {
        app.update();
        assert_eq!(
            work(&app),
            settled,
            "an unsupported legacy path cannot rebuild settled frames"
        );
    }
    app.world_mut().entity_mut(near).remove::<CustomDisabled>();
    app.update();
    assert_eq!(visibility(&app, far), Visibility::Hidden);
    let recovered = work(&app);
    assert_eq!(
        recovered[0], settled[0],
        "membership changes use the journal"
    );
    app.world_mut().entity_mut(near).insert(CustomDisabled);
    app.world_mut()
        .resource_mut::<Messages<TerrainNodeChanged>>()
        .clear();
    app.update();
    assert_eq!(visibility(&app, far), Visibility::Inherited);
    assert_eq!(
        work(&app)[0],
        recovered[0] + 1.0,
        "lost supplemental events recover exactly"
    );
    let recovered = work(&app);
    app.update();
    assert_eq!(work(&app), recovered);
}

#[test]
fn runtime_disabler_registration_requests_error_exit_once() {
    let mut app = app(true);
    let grid = IVec2::new(3, 0);
    let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
    let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
    app.update();
    app.update();
    let settled = work(&app);
    register_disabler::<CustomDisabled>(app.world_mut());
    app.update();
    assert_eq!(app.should_exit(), Some(bevy::app::AppExit::error()));
    assert_eq!(
        work(&app),
        settled,
        "invalid query domains are refused before index work"
    );
    let mut cursor = MessageCursor::<bevy::app::AppExit>::default();
    assert_eq!(
        cursor
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        1
    );
    app.update();
    assert_eq!(work(&app), settled);
    assert_eq!(
        cursor
            .read(app.world().resource::<Messages<bevy::app::AppExit>>())
            .count(),
        0
    );
    assert_eq!(visibility(&app, far), Visibility::Inherited);
}

/// Compare the live production count, not a test-maintained unload/readiness counter.
pub(super) fn assert_production_ready_count(index: &TerrainLodIndex, inputs: &TerrainLodInputs) {
    let expected = inputs
        .lod
        .iter()
        .filter(|(entity, coverage, ready)| {
            ready.is_some()
                && coverage.tier.is_some()
                && reference_world(&inputs.owners, *entity, coverage.tier)
                    .is_some_and(|world| Some(world) == index.world)
        })
        .count();
    assert_eq!(
        index.ready_patches, expected,
        "production ready count after lifecycle updates"
    );
}

#[test]
fn settled_selector_preserves_selected_tiers_and_does_not_change_streaming_ticks() {
    // The isolated fixture schedules only the real selector. Earlier planning/batching
    // writers therefore cannot conceal the selector's own empty take/false reset.
    for journal in [false, true] {
        for with_budget in [false, true] {
            let mut app = app(journal);
            if with_budget {
                app.insert_resource(StreamingCommitBudget {
                    remaining: 4,
                    ..default()
                });
            }
            let grid = IVec2::new(3, 0);
            let patches: Vec<_> = LodTier::ALL
                .into_iter()
                .map(|tier| {
                    let root = chunk_root(app.world_mut(), WORLD, tier, grid);
                    lod_patch(app.world_mut(), Some(root), grid, 0, Some(tier))
                })
                .collect();
            let near = near_patch(app.world_mut(), Some(WORLD), IVec2::ZERO, 0);
            app.update();
            app.update();
            let selected_tiers = |app: &App| {
                patches
                    .iter()
                    .filter(|&&entity| visibility(app, entity) == Visibility::Inherited)
                    .map(|&entity| app.world().get::<TerrainCoverage>(entity).unwrap().tier)
                    .collect::<Vec<_>>()
            };
            assert_eq!(selected_tiers(&app), vec![Some(LodTier::Tier4)]);
            assert_eq!(visibility(&app, near), Visibility::Inherited);
            let actual_visibility: Vec<_> = patches
                .iter()
                .map(|&entity| visibility(&app, entity))
                .collect();
            let revision = app.world().resource::<LodStreaming>().visibility_revision;
            assert!(revision > 0);
            assert!(
                app.world()
                    .resource::<LodStreaming>()
                    .replaced_source_quadrants
                    .is_empty()
            );
            assert!(!app.world().resource::<LodStreaming>().submission_dirty);
            assert!(!app.world().resource::<LodStreaming>().visibility_dirty);
            let budget = app
                .world()
                .get_resource::<StreamingCommitBudget>()
                .map(|budget| (budget.remaining, budget.commits));
            assert_eq!(budget, with_budget.then_some((4, 0)));
            let tick = app
                .world()
                .get_resource_ref::<LodStreaming>()
                .unwrap()
                .last_changed();
            for _ in 0..3 {
                app.update();
                assert_eq!(selected_tiers(&app), vec![Some(LodTier::Tier4)]);
                assert_eq!(
                    patches
                        .iter()
                        .map(|&entity| visibility(&app, entity))
                        .collect::<Vec<_>>(),
                    actual_visibility
                );
                assert_eq!(visibility(&app, near), Visibility::Inherited);
                assert_eq!(
                    app.world().resource::<LodStreaming>().visibility_revision,
                    revision
                );
                assert_eq!(
                    app.world()
                        .get_resource::<StreamingCommitBudget>()
                        .map(|budget| (budget.remaining, budget.commits)),
                    budget
                );
                assert_eq!(
                    app.world()
                        .get_resource_ref::<LodStreaming>()
                        .unwrap()
                        .last_changed(),
                    tick,
                    "isolated settled selector touched LodStreaming: journal={journal}, budget={with_budget}"
                );
            }
            // A real readiness withdrawal still changes the selected tier and ECS output.
            app.world_mut()
                .entity_mut(patches[0])
                .remove::<TerrainSurfaceReady>();
            app.update();
            assert_eq!(selected_tiers(&app), vec![Some(LodTier::Tier8)]);
            assert_eq!(visibility(&app, patches[0]), Visibility::Hidden);
            assert_ne!(
                app.world().resource::<LodStreaming>().visibility_revision,
                revision
            );
            assert_ne!(
                app.world()
                    .get_resource_ref::<LodStreaming>()
                    .unwrap()
                    .last_changed(),
                tick
            );
            assert_matches_full_pass(
                &mut app,
                "readiness change after settled native-tick checks",
            );
        }
    }
}

// Native system metadata is an observer: neither this tick nor any test counter
// is a selector decision input. The real ECS state remains the behavior oracle.
fn selector_last_run(app: &App) -> bevy::ecs::change_detection::Tick {
    let selector = bevy::ecs::system::IntoSystem::into_system(update_terrain_lod_visibility);
    let selector_type = bevy::ecs::system::System::system_type(&selector);
    app.world()
        .resource::<bevy::ecs::schedule::Schedules>()
        .get(Update)
        .unwrap()
        .systems()
        .unwrap()
        .find(|(_, system)| system.system_type() == selector_type)
        .unwrap()
        .1
        .get_last_run()
}

#[test]
fn outer_selector_gate_skips_settled_frames_despite_unrelated_streaming_writes() {
    for journal in [false, true] {
        let mut app = app(journal);
        app.add_systems(
            Update,
            (|mut state: ResMut<LodStreaming>| {
                // Model the existing batching/planning writer's unchanged false write.
                state.terrain_batch_commit_used = false;
            })
            .before(update_terrain_lod_visibility),
        );
        let grid = IVec2::new(3, 0);
        let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
        let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_matches_full_pass(&mut app, "before settled outer gate");
        let last_run = selector_last_run(&app);
        let revision = app.world().resource::<LodStreaming>().visibility_revision;
        let work_before = work(&app);
        for _ in 0..3 {
            let previous_streaming_tick = app
                .world()
                .get_resource_ref::<LodStreaming>()
                .unwrap()
                .last_changed();
            app.update();
            assert_ne!(
                app.world()
                    .get_resource_ref::<LodStreaming>()
                    .unwrap()
                    .last_changed(),
                previous_streaming_tick
            );
            assert_eq!(visibility(&app, far), Visibility::Inherited);
            assert_eq!(
                app.world().resource::<LodStreaming>().visibility_revision,
                revision
            );
            assert_eq!(work(&app), work_before);
            assert_eq!(
                app.world()
                    .resource::<ProfilingState>()
                    .gauge("lod/pending_terrain_selection_uploads"),
                Some(0.0)
            );
            assert_eq!(
                selector_last_run(&app),
                last_run,
                "outer gate must skip selector invocation; journal={journal}"
            );
        }
        let near = near_patch(app.world_mut(), Some(WORLD), grid, 0);
        app.update();
        assert_ne!(selector_last_run(&app), last_run);
        assert_eq!(visibility(&app, far), Visibility::Hidden);
        assert_matches_full_pass(&mut app, "new near coverage wakes outer gate");
        app.world_mut()
            .entity_mut(near)
            .remove::<TerrainSurfaceReady>();
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_matches_full_pass(&mut app, "native removal wakes outer gate");
        app.world_mut()
            .get_mut::<TerrainCoverage>(far)
            .unwrap()
            .quadrant = 1;
        app.update();
        assert_matches_full_pass(&mut app, "in-place coverage wakes outer gate");
    }
}

#[test]
fn outer_selector_gate_retains_unobserved_ancestor_liveness_after_idle_frames() {
    let mut app = app(true);
    let ancestor = app.world_mut().spawn_empty().id();
    let grid = IVec2::new(3, 0);
    let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
    app.world_mut()
        .entity_mut(root)
        .insert_with_relationship_hook_mode(ChildOf(ancestor), RelationshipHookMode::Skip);
    let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(visibility(&app, far), Visibility::Inherited);
    let last_run = selector_last_run(&app);
    app.update();
    assert_eq!(selector_last_run(&app), last_run);
    app.world_mut().despawn(ancestor);
    app.update();
    assert_ne!(selector_last_run(&app), last_run);
    assert_eq!(visibility(&app, far), Visibility::Hidden);
    assert_matches_full_pass(&mut app, "unobserved ancestor removed after gate slept");
}

#[test]
fn outer_selector_gate_completes_a_journal_loss_episode_without_another_mutation() {
    let mut app = app(true);
    for _ in 0..4 {
        app.update();
    }
    let entity = app.world_mut().spawn_empty().id();
    let before = work(&app);
    {
        let mut journal = app
            .world_mut()
            .resource_mut::<Messages<TerrainNodeChanged>>();
        journal.write(TerrainNodeChanged(entity));
        journal.clear();
    }
    app.update();
    assert!(app.world().resource::<TerrainLodObservation>().journal_lost);
    assert_eq!(work(&app)[0], before[0] + 1.0);
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .visible_lod_terrain_patches,
        0
    );
    assert_eq!(
        app.world()
            .resource::<ProfilingState>()
            .gauge("lod/pending_terrain_selection_uploads"),
        Some(0.0)
    );
    let last_run = selector_last_run(&app);
    let rebuilt = work(&app);
    app.update();
    assert_ne!(selector_last_run(&app), last_run);
    assert!(!app.world().resource::<TerrainLodObservation>().journal_lost);
    assert_eq!(work(&app), rebuilt);
    assert_matches_full_pass(&mut app, "complete read ends lost-journal episode");
}

#[test]
fn visibility_equals_a_full_pass_with_cluster_terrain() {
    for journal in [true, false] {
        for seed in [0xc0ff_ee00_dead_beef, 31, 0x1357_9bdf_2468_ace0] {
            run_oracle(
                Scenario {
                    cluster: true,
                    ..Scenario::new(journal)
                },
                seed,
                250,
            );
        }
    }
}

#[test]
fn cluster_resources_removed_replaced_and_restored_change_what_is_owned() {
    for journal in [true, false] {
        let mut app = app(journal);
        enable_cluster_terrain(&mut app);
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = true;
        let world = app.world_mut();
        let grid = IVec2::new(3, 0);
        let root = chunk_root(world, WORLD, LodTier::Tier4, grid);
        let far = lod_patch(world, Some(root), grid, 0, Some(LodTier::Tier4));
        // Batch 0 is drawable; batch 1 is pending elsewhere. Stock stays available to extraction.
        let owned_cells = [grid];
        let other_cells = [IVec2::new(9, 9)];
        let blocks = [
            (WORLD, &owned_cells[..], Some(0)),
            (WORLD, &other_cells[..], Some(1)),
        ];
        let ready = TerrainDrawReadiness::default();
        ready.mark(0);
        app.insert_resource(cluster_terrain(&blocks));
        app.insert_resource(ready);

        let step = |app: &mut App, context: &str, pending: usize| {
            app.update();
            let context = format!("journal {journal}: {context}");
            assert_eq!(visibility(app, far), Visibility::Inherited, "{context}");
            assert_eq!(pending_blocks(app), pending, "{context}");
            assert_matches_full_pass(app, &context);
        };
        step(&mut app, "owned by a drawable block", 1);

        // Same revision, same world, same flag: only the resource itself went away.
        app.world_mut().remove_resource::<ClusterTerrain>();
        step(&mut app, "terrain removed", 0);
        app.insert_resource(cluster_terrain(&blocks));
        step(&mut app, "terrain restored", 1);

        app.world_mut().remove_resource::<TerrainDrawReadiness>();
        step(&mut app, "readiness removed", 2);
        let ready = TerrainDrawReadiness::default();
        ready.mark(0);
        app.insert_resource(ready);
        step(&mut app, "readiness restored", 1);

        // A replacement that starts over reaches the same revision with a different set:
        // batch 1 draws now and batch 0 no longer does; stock selection remains unchanged.
        let replacement = TerrainDrawReadiness::default();
        replacement.mark(1);
        assert_eq!(
            replacement.revision(),
            app.world().resource::<TerrainDrawReadiness>().revision()
        );
        app.insert_resource(replacement);
        step(&mut app, "readiness replaced at the same revision", 1);

        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = false;
        step(&mut app, "owned statics off", 1);
    }
}

#[test]
fn fallback_terrain_survives_rebuilds_and_lost_journal_messages() {
    for journal in [true, false] {
        let mut app = app(journal);
        enable_cluster_terrain(&mut app);
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = true;
        let grid = IVec2::new(3, 0);
        let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
        let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
        let ready = TerrainDrawReadiness::default();
        ready.mark(0);
        app.insert_resource(ready);
        app.insert_resource(cluster_terrain(&[(WORLD, &[grid], Some(0))]));
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_matches_full_pass(&mut app, "owned initial snapshot");
        let before = work(&app);
        // Newly arriving stock stays extracted despite historical readiness.
        // Only the render view can authorize current complementary suppression.
        let duplicate = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
        if journal {
            // Discard the unread insertion messages, forcing recovery from a snapshot.
            app.world_mut()
                .resource_mut::<Messages<TerrainNodeChanged>>()
                .clear();
        } else {
            app.world_mut()
                .resource_mut::<LodStreaming>()
                .visibility_dirty = true;
        }
        app.update();
        assert_eq!(work(&app)[0], before[0] + 1.0, "one recovery rebuild");
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_eq!(visibility(&app, duplicate), Visibility::Inherited);
        assert_eq!(pending_blocks(&app), 0);
        assert_matches_full_pass(&mut app, "owned cells after snapshot recovery");
        let after = work(&app);
        app.update();
        assert_eq!(work(&app), after, "an idle frame performs no terrain work");
        // Shared readiness changes through an immutable handle: Res::is_changed alone
        // cannot detect this transition. Clearing it changes pending diagnostics only.
        assert!(app.world().resource::<TerrainDrawReadiness>().clear());
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_eq!(visibility(&app, duplicate), Visibility::Inherited);
        assert_eq!(pending_blocks(&app), 1);
        assert_matches_full_pass(&mut app, "readiness withdrawal after recovery");
        if journal {
            assert_eq!(
                &work(&app)[..3],
                &after[..3],
                "readiness uses cached patch facts"
            );
        }
    }
}

#[test]
fn fallback_terrain_and_pending_blocks_follow_public_world_demand() {
    for journal in [true, false] {
        let mut app = app(journal);
        enable_cluster_terrain(&mut app);
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = true;
        let grid = IVec2::new(3, 0);
        let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
        let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
        let other_root = chunk_root(app.world_mut(), WORLD + 1, LodTier::Tier4, grid);
        let other = lod_patch(
            app.world_mut(),
            Some(other_root),
            grid,
            0,
            Some(LodTier::Tier4),
        );
        app.insert_resource(cluster_terrain(&[
            (WORLD, &[grid], Some(0)),
            (WORLD, &[IVec2::new(8, 0)], None),
            (WORLD + 1, &[grid], Some(1)),
        ]));
        let ready = TerrainDrawReadiness::default();
        ready.mark(0);
        app.insert_resource(ready);
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_eq!(visibility(&app, other), Visibility::Hidden);
        assert_eq!(pending_blocks(&app), 1, "unassigned batch stays pending");
        assert_matches_full_pass(&mut app, "configured world owns ready terrain only");
        let before = work(&app);
        app.insert_resource(ActiveSpace {
            worldspace_id: Some(WORLD + 1),
            interior: None,
        });
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Hidden);
        assert_eq!(visibility(&app, other), Visibility::Inherited);
        assert_eq!(
            pending_blocks(&app),
            1,
            "only the demanded world's block counts"
        );
        assert_matches_full_pass(&mut app, "unready other-world terrain retains stock");
        app.world_mut().resource_mut::<ActiveSpace>().interior = Some(10);
        app.update();
        assert_eq!(visibility(&app, other), Visibility::Hidden);
        assert_eq!(
            pending_blocks(&app),
            0,
            "no exterior demand has no pending blocks"
        );
        assert_matches_full_pass(&mut app, "interior withdraws exterior terrain demand");
        app.world_mut().remove_resource::<ActiveSpace>();
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_eq!(pending_blocks(&app), 1);
        assert_matches_full_pass(&mut app, "configured owned world restored");
        if journal {
            assert_eq!(
                &work(&app)[..3],
                &before[..3],
                "world changes reuse patch ancestry"
            );
        }
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = false;
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_eq!(
            pending_blocks(&app),
            1,
            "pending readiness is independent of ownership flag"
        );
        assert_matches_full_pass(
            &mut app,
            "ownership off restores stock without hiding pending work",
        );
    }
}

#[test]
fn disabled_cluster_terrain_preserves_ordinary_selector_behavior() {
    for journal in [false, true] {
        // Neither half of upper activation alone admits the terrain policy.
        for (package, terrain) in [(false, false), (true, false), (false, true)] {
            let mut ordinary = app(journal);
            let mut disabled = app(journal);
            let grid = IVec2::new(3, 0);
            let mut patches = Vec::new();
            for app in [&mut ordinary, &mut disabled] {
                let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
                patches.push(lod_patch(
                    app.world_mut(),
                    Some(root),
                    grid,
                    0,
                    Some(LodTier::Tier4),
                ));
            }
            {
                let mut config = disabled.world_mut().resource_mut::<EngineConfig>();
                config.clusters = package.then(String::new);
                config.cluster_terrain = terrain;
                config.cluster_own_statics = true;
            }
            disabled.insert_resource(cluster_terrain(&[(WORLD, &[grid], Some(0))]));
            disabled.insert_resource(TerrainDrawReadiness::default());
            for _ in 0..4 {
                ordinary.update();
                disabled.update();
            }
            let ordinary_run = selector_last_run(&ordinary);
            let disabled_run = selector_last_run(&disabled);
            for step in 0..6 {
                match step {
                    0 => {
                        disabled.world().resource::<TerrainDrawReadiness>().mark(0);
                    }
                    1 => {
                        disabled.insert_resource(TerrainDrawReadiness::default());
                    }
                    2 => {
                        disabled.world_mut().resource_mut::<ClusterTerrain>().blocks[0].batch =
                            None;
                    }
                    3 => {
                        disabled.world_mut().remove_resource::<ClusterTerrain>();
                    }
                    4 => {
                        disabled
                            .world_mut()
                            .remove_resource::<TerrainDrawReadiness>();
                    }
                    _ => {
                        disabled.insert_resource(cluster_terrain(&[(WORLD, &[grid], None)]));
                    }
                }
                ordinary.update();
                disabled.update();
                assert_eq!(selector_last_run(&ordinary), ordinary_run);
                assert_eq!(selector_last_run(&disabled), disabled_run);
                assert_eq!(
                    visibility(&ordinary, patches[0]),
                    visibility(&disabled, patches[1])
                );
                assert_eq!(work(&ordinary), work(&disabled));
                assert_eq!(
                    ordinary
                        .world()
                        .resource::<LodStreaming>()
                        .visibility_revision,
                    disabled
                        .world()
                        .resource::<LodStreaming>()
                        .visibility_revision
                );
                assert_eq!(pending_blocks(&disabled), 0);
                assert_eq!(
                    ordinary
                        .world()
                        .resource::<StreamingMetrics>()
                        .visible_lod_terrain_patches,
                    disabled
                        .world()
                        .resource::<StreamingMetrics>()
                        .visible_lod_terrain_patches
                );
                assert_eq!(
                    ordinary
                        .world()
                        .resource::<ProfilingState>()
                        .gauge("lod/pending_terrain_selection_uploads"),
                    disabled
                        .world()
                        .resource::<ProfilingState>()
                        .gauge("lod/pending_terrain_selection_uploads")
                );
            }
            // Native selection dependencies still work with the inactive upper resources present.
            move_camera(ordinary.world_mut(), 30);
            move_camera(disabled.world_mut(), 30);
            ordinary.update();
            disabled.update();
            assert_eq!(
                visibility(&ordinary, patches[0]),
                visibility(&disabled, patches[1])
            );
            assert_eq!(work(&ordinary), work(&disabled));
            assert_matches_full_pass(&mut ordinary, "ordinary control camera move");
            assert_matches_full_pass(&mut disabled, "disabled upper camera move");
        }
    }
}

#[test]
fn cluster_inputs_wake_the_outer_selector_and_settle_again() {
    for journal in [false, true] {
        let mut app = app(journal);
        let grid = IVec2::new(3, 0);
        let root = chunk_root(app.world_mut(), WORLD, LodTier::Tier4, grid);
        let far = lod_patch(app.world_mut(), Some(root), grid, 0, Some(LodTier::Tier4));
        app.insert_resource(cluster_terrain(&[(WORLD, &[grid], Some(0))]));
        app.insert_resource(TerrainDrawReadiness::default());
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(pending_blocks(&app), 0);
        let disabled_run = selector_last_run(&app);
        enable_cluster_terrain(&mut app);
        app.update();
        assert_ne!(
            selector_last_run(&app),
            disabled_run,
            "config-only activation wakes the outer gate"
        );
        assert_eq!(pending_blocks(&app), 1);
        app.update();
        let ready = app.world().resource::<TerrainDrawReadiness>().clone();
        let mut last_run = selector_last_run(&app);
        let before = work(&app);
        for _ in 0..3 {
            app.update();
            assert_eq!(selector_last_run(&app), last_run);
            assert_eq!(work(&app), before);
        }
        assert!(ready.mark(0));
        app.update();
        assert_ne!(
            selector_last_run(&app),
            last_run,
            "immutable shared readiness revision wakes the gate"
        );
        assert_eq!(pending_blocks(&app), 0);
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        if journal {
            assert_eq!(&work(&app)[..3], &before[..3]);
        }
        last_run = selector_last_run(&app);
        app.update();
        assert_eq!(selector_last_run(&app), last_run);
        let replacement = TerrainDrawReadiness::default();
        replacement.mark(1);
        assert_eq!(replacement.revision(), ready.revision());
        app.insert_resource(replacement);
        app.update();
        assert_ne!(
            selector_last_run(&app),
            last_run,
            "same-revision resource replacement wakes the gate"
        );
        assert_eq!(pending_blocks(&app), 1);
        // The policy is independent of static ownership; toggling that flag still refreshes.
        *app.world_mut().get_mut::<Visibility>(far).unwrap() = Visibility::Hidden;
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_own_statics = true;
        app.update();
        assert_eq!(visibility(&app, far), Visibility::Inherited);
        assert_eq!(pending_blocks(&app), 1);
        last_run = selector_last_run(&app);
        app.world_mut().resource_mut::<ClusterTerrain>().blocks[0].batch = Some(1);
        app.update();
        assert_ne!(
            selector_last_run(&app),
            last_run,
            "in-place terrain edits wake the gate"
        );
        assert_eq!(pending_blocks(&app), 0);
        last_run = selector_last_run(&app);
        app.world_mut().remove_resource::<TerrainDrawReadiness>();
        app.update();
        assert_ne!(
            selector_last_run(&app),
            last_run,
            "optional resource removal wakes the gate"
        );
        assert_eq!(pending_blocks(&app), 1);
        last_run = selector_last_run(&app);
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .cluster_terrain = false;
        app.update();
        assert_ne!(
            selector_last_run(&app),
            last_run,
            "disabling policy clears the pending count"
        );
        assert_eq!(pending_blocks(&app), 0);
        app.update();
        last_run = selector_last_run(&app);
        app.insert_resource(TerrainDrawReadiness::default());
        app.update();
        assert_eq!(
            selector_last_run(&app),
            last_run,
            "disabled policy ignores resource writes"
        );
        assert_matches_full_pass(&mut app, "cluster gate lifecycle");
    }
}

#[test]
fn cluster_resource_refreshes_repair_only_the_overridden_key() {
    for journal in [false, true] {
        for refresh in 0..3 {
            let (mut app, target, _) = override_scene(journal);
            enable_cluster_terrain(&mut app);
            app.insert_resource(cluster_terrain(&[(WORLD, &[], None)]));
            app.insert_resource(TerrainDrawReadiness::default());
            for _ in 0..4 {
                app.update();
            }
            *app.world_mut().get_mut::<Visibility>(target).unwrap() = Visibility::Hidden;
            for _ in 0..3 {
                app.update();
            }
            assert_eq!(visibility(&app, target), Visibility::Hidden);
            let before = work(&app);
            match refresh {
                0 => {
                    app.world().resource::<TerrainDrawReadiness>().mark(5);
                }
                1 => {
                    app.insert_resource(TerrainDrawReadiness::default());
                }
                _ => {
                    app.world_mut()
                        .resource_mut::<ClusterTerrain>()
                        .set_changed();
                }
            }
            app.update();
            assert_eq!(visibility(&app, target), Visibility::Inherited);
            assert_matches_full_pass(&mut app, "cluster resource override repair");
            if journal {
                assert_eq!(work(&app)[3] - before[3], 1.0);
            }
        }
    }
}
