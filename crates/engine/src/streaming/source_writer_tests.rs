//! Ordinary producer regressions: all identity tags come from production spawning.
use super::*;
use crate::world::{
    components::{StreamedCellKey, WorldPosition},
    database::ReferenceRow,
};
use bevy::{
    asset::{AssetApp, AssetPlugin},
    ecs::{system::RunSystemOnce, world::CommandQueue},
};

fn exterior(worldspace_id: u32, grid_x: i32, grid_y: i32) -> CellKey {
    CellKey::Exterior {
        worldspace_id,
        grid_x,
        grid_y,
    }
}

/// Reuse the LAND fixtures, but exercise the actual ordinary spawn entry point.
/// `merged`: None = no LAND, Some(false) = quadrants, Some(true) = merged.
fn ordinary(key: CellKey, merged: Option<bool>, references: Vec<ReferenceRow>) -> (App, Entity) {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<TerrainMaterial>()
        .init_asset::<WaterMaterial>();
    let asset_server = app.world().resource::<AssetServer>().clone();
    let record = land(
        CELL,
        rolling,
        [0x100, 0x101, 0x102, 0x103],
        &atxt_layers(),
        true,
    );
    let terrain = merged.map(|_| {
        let mut terrain = cached_snapshot(&record);
        terrain.layers.clear();
        terrain.water_height = None;
        terrain
    });
    let merged = merged.filter(|merged| *merged).map(|_| MergedSpawn {
        cell: Box::new(merged_cell(&record)),
        classes: Arc::new(TerrainClasses::from_table(vanilla_table(9)).unwrap()),
        material: Handle::default(),
        images: Arc::from([]),
    });
    let mut meshes = Assets::<Mesh>::default();
    let mut terrain_materials = Assets::<TerrainMaterial>::default();
    let mut water_materials = Assets::<WaterMaterial>::default();
    let mut queue = CommandQueue::default();
    let mut commands = Commands::new(&mut queue, app.world());
    let root = spawn_cell(
        &mut commands,
        &asset_server,
        &AssetCatalog::default(),
        &ActorCatalog::default(),
        &WaterReflectionTexture(Handle::default()),
        &mut meshes,
        &mut terrain_materials,
        &mut water_materials,
        IVec2::new(3, -4),
        false,
        false,
        false,
        CellPayload {
            generation: 1,
            key,
            cell_id: CELL,
            references,
        },
        terrain,
        merged,
        &mut 0,
        &mut ProfilingState::default(),
    )
    .expect("ordinary spawn succeeds");
    queue.apply(app.world_mut());
    app.insert_resource(meshes)
        .insert_resource(terrain_materials)
        .insert_resource(water_materials);
    (app, root)
}

#[test]
fn ordinary_interior_writes_key_without_exterior_identity() {
    for merged in [false, true] {
        let key = CellKey::Interior(CELL);
        let (mut app, root) = ordinary(key, Some(merged), Vec::new());
        let world = app.world_mut();
        // Inspect the negative terrain branch too, rather than an empty world.
        assert_eq!(
            world
                .query_filtered::<Entity, With<TerrainPatch>>()
                .iter(world)
                .count(),
            if merged { 1 } else { 4 },
        );
        assert!(world.get::<ExteriorCellGrid>(root).is_none());
        assert_eq!(world.query::<&TerrainWorldspace>().iter(world).count(), 0);
        assert_eq!(world.query::<&TerrainCoverage>().iter(world).count(), 0);
        assert_eq!(
            world.query::<&MergedTerrainCoverage>().iter(world).count(),
            0
        );
        assert_eq!(
            world.get::<StreamedCellKey>(root),
            Some(&StreamedCellKey(key))
        );
    }
}

#[test]
fn ordinary_quadrants_write_the_payload_world_and_reach_identity_consumer() {
    for worldspace_id in [0x1234_5678, 0x8765_4321] {
        let grid = IVec2::new(-7, 9);
        let key = exterior(worldspace_id, grid.x, grid.y);
        let (mut app, root) = ordinary(key, Some(false), Vec::new());
        let patches: Vec<_> = app
            .world_mut()
            .query_filtered::<Entity, With<TerrainPatch>>()
            .iter(app.world())
            .collect();
        assert_eq!(patches.len(), 4);
        let mut quadrants = HashSet::new();
        for patch in patches {
            let world = app.world();
            let coverage = world.get::<TerrainCoverage>(patch).unwrap();
            assert!(quadrants.insert(coverage.quadrant));
            assert_eq!(coverage.grid, grid);
            assert_eq!(coverage.tier, None);
            assert_eq!(
                world.get::<TerrainWorldspace>(patch).map(|tag| tag.0),
                Some(worldspace_id)
            );
            assert_eq!(world.get::<ChildOf>(patch).unwrap().parent(), root);
            assert!(world.get::<MergedTerrainCoverage>(patch).is_none());
            let want = TerrainPrimitiveIdentity {
                worldspace_id,
                grid,
                quadrant: coverage.quadrant,
                tier: None,
            };
            let identity = app
                .world_mut()
                .run_system_once(move |access: TerrainIdentityAccess| access.identity(patch))
                .unwrap();
            assert_eq!(identity, Some(want));
        }
        assert_eq!(
            app.world().get::<StreamedCellKey>(root),
            Some(&StreamedCellKey(key))
        );
    }
}

#[test]
fn ordinary_merged_terrain_writes_world_without_inventing_single_quadrant_identity() {
    for worldspace_id in [0x1234_5678, 0x8765_4321] {
        let grid = IVec2::new(7, -9);
        let key = exterior(worldspace_id, grid.x, grid.y);
        let (mut app, root) = ordinary(key, Some(true), Vec::new());
        let patches: Vec<_> = app
            .world_mut()
            .query_filtered::<Entity, With<TerrainPatch>>()
            .iter(app.world())
            .collect();
        assert_eq!(patches.len(), 1);
        let patch = patches[0];
        let world = app.world();
        assert_eq!(
            world.get::<MergedTerrainCoverage>(patch),
            Some(&MergedTerrainCoverage { grid })
        );
        assert_eq!(
            world.get::<TerrainWorldspace>(patch).map(|tag| tag.0),
            Some(worldspace_id)
        );
        assert_eq!(world.get::<ChildOf>(patch).unwrap().parent(), root);
        assert!(world.get::<TerrainCoverage>(patch).is_none());
        let identity = app
            .world_mut()
            .run_system_once(move |access: TerrainIdentityAccess| access.identity(patch))
            .unwrap();
        assert_eq!(
            identity, None,
            "a merged cell covers four quadrants, not one"
        );
        assert_eq!(
            app.world().get::<StreamedCellKey>(root),
            Some(&StreamedCellKey(key))
        );
    }
}

fn authored_reference(position: [f32; 3]) -> ReferenceRow {
    ReferenceRow {
        form_id: 0x77,
        initially_enabled: true,
        cell_id: CELL,
        base_form_id: 0x88,
        base_record_type: None,
        base_is_marker: false,
        model_path: None,
        position,
        rotation: [0.31, -0.47, 0.83],
        scale: 1.75,
        bounds_min: [-2.0, -3.0, -4.0],
        bounds_max: [5.0, 7.0, 11.0],
        bounds_valid: true,
        light: None,
        light_radius_override: None,
        door: None,
        emittance: None,
    }
}

#[test]
fn ordinary_authored_placement_and_bounds_survive_shared_math() {
    // Independent values from C * Rx(-x) * Ry(-y) * Rz(-z) * transpose(C),
    // C maps (x,y,z) to (x,z,-y). Bounds enumerate the eight authored corners.
    // No expected value below is computed by reference_placement or its helpers.
    let want_rotation = Quat::from_xyzw(-0.23014716, -0.4203023, -0.14999026, 0.8648012);
    for (key, position, want_grid, want_root) in [
        (
            exterior(0x1234_5678, 7, 9),
            [28800.0, 37120.0, 96.0],
            IVec2::new(7, 9),
            Vec3::new(16384.0, 0.0, -53248.0),
        ),
        (
            exterior(0x8765_4321, -7, -9),
            [-28544.0, -36608.0, 96.0],
            IVec2::new(-7, -9),
            Vec3::new(-40960.0, 0.0, 20480.0),
        ),
        (
            exterior(0x1234_5678, 100000, -100000),
            [409600128.0, -409599744.0, 96.0],
            IVec2::new(100000, -100000),
            Vec3::new(409587712.0, 0.0, 409583616.0),
        ),
        (
            CellKey::Interior(CELL),
            [128.0, 256.0, 96.0],
            IVec2::ZERO,
            Vec3::ZERO,
        ),
    ] {
        let reference = authored_reference(position);
        let (mut app, root) = ordinary(key, None, vec![reference.clone()]);
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &FormId)>();
        let entity = query
            .iter(world)
            .find(|(_, id)| id.0 == reference.form_id)
            .unwrap()
            .0;
        let placement = world.get::<Transform>(entity).unwrap();
        let world_position = world.get::<WorldPosition>(entity).unwrap();
        assert_eq!(placement.translation, Vec3::new(128.0, 96.0, -256.0));
        assert!(placement.rotation.abs_diff_eq(want_rotation, 1.0e-6));
        assert_eq!(placement.scale, Vec3::splat(1.75));
        assert_eq!(world_position.grid, want_grid);
        assert_eq!(world_position.local, Vec3::new(128.0, 256.0, 96.0));
        assert_eq!(world.get::<Transform>(root).unwrap().translation, want_root);
        assert_eq!(world.get::<ChildOf>(entity).unwrap().parent(), root);
        assert_eq!(
            world.get::<WorldTransform>(entity).unwrap().0,
            placement.to_matrix()
        );
        let expected = world.get::<ExpectedModelBounds>(entity).unwrap();
        assert_eq!(expected.min, Vec3::new(-2.0, -3.0, -4.0));
        assert_eq!(expected.max, Vec3::new(5.0, 7.0, 11.0));
        let bounds = world.get::<InstanceBounds>(entity).unwrap();
        assert!(
            bounds
                .min
                .abs_diff_eq(Vec3::new(110.85152, 87.2962, -265.90305), 1.0e-4)
        );
        assert!(
            bounds
                .max
                .abs_diff_eq(Vec3::new(143.41812, 116.72178, -237.19757), 1.0e-4)
        );
        // The qualification path uses this helper. Compare its result with real
        // loader output only after the independent authored expectations above.
        let (qualified_position, qualified_pose) =
            reference_placement(reference.position, reference.rotation, reference.scale, key);
        assert_eq!(qualified_position.grid, world_position.grid);
        assert_eq!(qualified_position.local, world_position.local);
        assert_eq!(qualified_pose, *placement);
    }
}
