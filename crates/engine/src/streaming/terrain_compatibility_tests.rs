mod terrain_compatibility {
    use super::*;
    use crate::swap_bridge::SwapCameraObserved;
    use crate::swap_bridge::terrain_compatibility_fixture::{
        image_ids, publish_completed_camera_observations,
    };
    use crate::world::components::WorldPosition;
    use bevy::ecs::system::SystemState;

    #[derive(Resource, Default)]
    struct QueuedCells(Vec<(CellKey, TerrainSnapshot)>);

    // Fixture initialization through TerrainExtension's existing Reflect interface.
    // No validation/drop behavior is implemented here.
    fn authored_normals(normal: Handle<Image>, valid: Handle<Image>) -> TerrainExtension {
        let mut extension = TerrainExtension::default();
        let bevy::reflect::ReflectMut::Struct(fields) = extension.reflect_mut() else {
            panic!("TerrainExtension must remain a reflected struct");
        };
        fields
            .field_mut("normal_0")
            .unwrap()
            .try_apply(&Some(normal))
            .unwrap();
        fields
            .field_mut("normal_1")
            .unwrap()
            .try_apply(&Some(valid))
            .unwrap();
        let settings = fields.field_mut("settings").unwrap();
        let bevy::reflect::ReflectMut::Struct(settings) = settings.reflect_mut() else {
            panic!("terrain settings must remain a reflected struct");
        };
        settings
            .field_mut("normal_layers_0")
            .unwrap()
            .try_apply(&Vec4::new(1.0, 1.0, 0.0, 0.0))
            .unwrap();
        extension
    }

    fn surface_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<TerrainMaterial>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<ProfilingState>()
            .add_systems(Update, track_surface_readiness);
        app
    }

    // Capture the real native optional-normal warning on the current thread.
    // This observes the diagnostic; it does not synthesize failure accounting.
    #[derive(Clone, Default)]
    struct NormalWarnings(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl NormalWarnings {
        fn count(&self) -> usize {
            self.0.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl tracing::Subscriber for NormalWarnings {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(bool);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message"
                        && format!("{value:?}").contains(
                            "terrain normal map failed; drawing the quadrant without normal maps",
                        )
                    {
                        self.0 = true;
                    }
                }
            }
            let mut message = Message(false);
            event.record(&mut message);
            if message.0 {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[derive(Debug, Clone, Copy)]
    struct SpawnedBounds {
        min: Vec3,
        max: Vec3,
        nodes: usize,
    }

    #[derive(Resource, Default)]
    struct BoundsOutcome {
        expected: Option<ExpectedModelBounds>,
        bounds: Option<Result<SpawnedBounds, String>>,
        check: Option<Result<TransformValidationSummary, String>>,
    }

    // Capture actual native measurements; the independent expected bounds are
    // composed once from the authored, unshifted fixture in spawn_bounds_fixture.
    fn run_bounds_check(
        mut outcome: ResMut<BoundsOutcome>,
        roots: Query<(
            Entity,
            &Transform,
            &GlobalTransform,
            &WorldTransform,
            Option<&ExpectedModelBounds>,
        )>,
        transforms: SpawnedNodeQuery,
        children: Query<&Children>,
        primitives: RenderPrimitiveQuery,
        meshes: Res<Assets<Mesh>>,
        _converted_models: Res<Assets<WorldAsset>>,
    ) {
        let (root, local, global, world_transform, expected) = roots.single().unwrap();
        outcome.expected = expected.copied();
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        let mut nodes = 0;
        let mut bounded_meshes = 0;
        let result = children
            .get(root)
            .map_err(|error| error.to_string())
            .and_then(|direct| {
                for child in direct.iter() {
                    accumulate_relative_bounds(
                        child,
                        Affine3A::IDENTITY,
                        &children,
                        &transforms,
                        &primitives,
                        &meshes,
                        &mut nodes,
                        &mut bounded_meshes,
                        &mut min,
                        &mut max,
                    )?;
                }
                Ok(SpawnedBounds { min, max, nodes })
            });
        outcome.bounds = Some(result);
        outcome.check = Some(validate_spawned_transforms_and_bounds(
            root,
            local,
            global,
            world_transform,
            expected,
            false,
            None,
            &children,
            &transforms,
            &primitives,
            &meshes,
        ));
    }
    fn untextured_fixture(cell_id: u32, height: f32) -> TerrainSnapshot {
        let mut terrain = terrain_fixture(cell_id, height);
        terrain.layers.clear();
        terrain
    }

    fn write_terrain_database(path: &std::path::Path) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
        .execute_batch(&format!(
            r#"CREATE TABLE schema_info(version INTEGER NOT NULL);
            INSERT INTO schema_info VALUES({version});
            CREATE TABLE cells(id INTEGER PRIMARY KEY,worldspace_id INTEGER,grid_x INTEGER,grid_y INTEGER);
            CREATE TABLE land(cell_id INTEGER PRIMARY KEY);
            CREATE TABLE statics(id INTEGER PRIMARY KEY,model_path TEXT,flags INTEGER NOT NULL,
                bounds_min_x REAL NOT NULL DEFAULT -64,bounds_min_y REAL NOT NULL DEFAULT -64,bounds_min_z REAL NOT NULL DEFAULT -64,
                bounds_max_x REAL NOT NULL DEFAULT 64,bounds_max_y REAL NOT NULL DEFAULT 64,bounds_max_z REAL NOT NULL DEFAULT 64,
                bounds_valid INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE "references"(id INTEGER PRIMARY KEY,cell_id INTEGER NOT NULL,worldspace_id INTEGER,base_form_id INTEGER NOT NULL,
                is_exterior INTEGER NOT NULL,pos_x REAL NOT NULL,pos_y REAL NOT NULL,pos_z REAL NOT NULL,local_x REAL,local_y REAL,
                rot_x REAL NOT NULL,rot_y REAL NOT NULL,rot_z REAL NOT NULL,scale REAL NOT NULL DEFAULT 1.0);
            CREATE VIRTUAL TABLE exterior_spatial USING rtree(id,minX,maxX,minY,maxY,minZ,maxZ,+cell_id,+worldspace_id);
            INSERT INTO cells VALUES(1,60,2,-3);
            INSERT INTO land VALUES(1);
            -- The cell one grid west, with no landscape in the cache: a full request for it
            -- still resolves to a cell, so a run that starts there has no failed cell in it.
            INSERT INTO cells VALUES(2,60,1,-3);"#,
            version = shared::WORLD_DATABASE_SCHEMA_VERSION
        ))
        .unwrap();
    }

    fn write_terrain_cache(path: &std::path::Path, cell_id: u32) {
        let cache = shared::CellCache {
            version: shared::CELL_CACHE_VERSION,
            cells: vec![shared::CachedLand {
                cell_id,
                width: 33,
                height: 33,
                heights: vec![0.0; 33 * 33],
                normals: (0..33 * 33).flat_map(|_| [0, 0, 127]).collect(),
                vertex_colors: Vec::new(),
                layers: Vec::new(),
                water_height: None,
                water_type_form_id: None,
            }],
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn terrain_streaming_app_with(
        database_path: &std::path::Path,
        cache_path: &std::path::Path,
        walk: bool,
    ) -> App {
        let catalogue_path = database_path.with_extension("catalogue");
        write_empty_catalogue(&catalogue_path);
        let mut app = App::new();
        let mut config = EngineConfig {
            // Plan only the camera's own full cell.
            stream_radius: 0,
            unload_radius: 1,
            ..EngineConfig::default()
        };
        config.headless = !walk;
        app.add_plugins(MinimalPlugins)
            .add_plugins(AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .init_asset::<TerrainMaterial>()
            .init_asset::<WaterMaterial>()
            .insert_resource(config)
            .insert_resource(RenderOrigin(IVec2::new(2, -3)))
            .insert_resource(ActiveSpace {
                worldspace_id: Some(60),
                interior: None,
            })
            .insert_resource(CellCache::open(cache_path).unwrap())
            .insert_resource(AssetCatalog::open(&catalogue_path).unwrap())
            .init_resource::<ActorCatalog>()
            .insert_resource(WaterReflectionTexture(Handle::default()))
            .insert_resource(WorldDatabase::open(database_path).unwrap())
            .init_resource::<StreamingWorld>()
            .init_resource::<TerrainContinuity>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<StreamingCommitBudget>()
            .init_resource::<ProfilingState>()
            .add_systems(
                Update,
                (plan_cells, collect_cells, track_surface_readiness).chain(),
            );
        app
    }

    fn run_until<F: Fn(&App) -> bool>(app: &mut App, what: &str, done: F) {
        for _ in 0..600 {
            app.update();
            if done(app) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let streaming = app.world().resource::<StreamingWorld>();
        panic!(
            "timed out waiting for {what}; cells={:#?}; \
         outstanding={:?}; held_responses={}; queued_responses={}; metrics={:#?}",
            streaming.cells,
            streaming.outstanding_cells,
            streaming.held_responses.len(),
            app.world()
                .resource::<WorldDatabase>()
                .pending_cell_responses(),
            app.world().resource::<StreamingMetrics>(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_queued_cells(
        mut commands: Commands,
        queued: Res<QueuedCells>,
        asset_server: Res<AssetServer>,
        catalog: Res<AssetCatalog>,
        reflection: Res<WaterReflectionTexture>,
        mut assets: CellAssets,
        mut streaming: ResMut<StreamingWorld>,
        mut profiler: ResMut<ProfilingState>,
    ) {
        for (key, terrain) in &queued.0 {
            let payload = CellPayload {
                generation: 1,
                key: *key,
                cell_id: terrain.cell_id,
                references: Vec::new(),
            };
            let root = spawn_cell(
                &mut commands,
                &asset_server,
                &catalog,
                &ActorCatalog::default(),
                &reflection,
                &mut assets.meshes,
                &mut assets.terrain_materials,
                &mut assets.water_materials,
                IVec2::ZERO,
                false,
                false,
                false,
                payload,
                Some(terrain.clone()),
                None,
                &mut 0,
                &mut profiler,
            )
            .expect("spawn the prior full terrain root through the native cell spawner");
            streaming.cells.insert(*key, CellStatus::Resident { root });
        }
    }

    fn spawn_cells_app(cells: Vec<(CellKey, TerrainSnapshot)>, metrics: StreamingMetrics) -> App {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalogue.db");
        write_empty_catalogue(&path);
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<StandardMaterial>()
            .init_asset::<TerrainMaterial>()
            .init_asset::<WaterMaterial>()
            .insert_resource(EngineConfig::default())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(ActiveSpace {
                worldspace_id: Some(60),
                interior: None,
            })
            .insert_resource(AssetCatalog::open(&path).unwrap())
            .insert_resource(WaterReflectionTexture(Handle::default()))
            .insert_resource(QueuedCells(cells))
            .insert_resource(metrics)
            .init_resource::<StreamingWorld>()
            .init_resource::<TerrainContinuity>()
            .init_resource::<ProfilingState>()
            .add_systems(
                Update,
                (spawn_queued_cells, validate_streaming_lifecycle).chain(),
            );
        app.world_mut()
            .spawn((Transform::default(), StreamingCamera));
        app.update();
        app
    }

    fn terrain_collider_state(app: &mut App) -> (usize, usize) {
        let mut query = app
            .world_mut()
            .query_filtered::<(Entity, Option<&ColliderDisabled>), With<TerrainCollider>>();
        let mut total = 0;
        let mut disabled = 0;
        for (_, flag) in query.iter(app.world()) {
            total += 1;
            disabled += usize::from(flag.is_some());
        }
        (total, disabled)
    }

    fn spawn_mock_interior(app: &mut App, cell_id: u32) -> Entity {
        let root = app
            .world_mut()
            .spawn((
                CellRef(cell_id),
                StreamedCellRoot,
                Transform::default(),
                Visibility::default(),
            ))
            .id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(CellKey::Interior(cell_id), CellStatus::Resident { root });
        root
    }

    fn far_reference() -> Transform {
        Transform::from_rotation(Quat::from_rotation_z(0.41)).with_translation(
            Vec3::new(120_000.0, -95_000.0, 8_000.0) + Vec3::new(1.37, -2.19, 0.43),
        )
    }

    fn spawn_bounds_fixture(
        world: &mut World,
        root_local: Transform,
        node_local: Transform,
        mesh_local: Transform,
        mesh_shift: Vec3,
        mesh: Handle<Mesh>,
    ) -> (Entity, ExpectedModelBounds) {
        let (min, max) = {
            let aabb = world
                .resource::<Assets<Mesh>>()
                .get(&mesh)
                .and_then(|mesh| mesh.compute_aabb())
                .expect("the fixture mesh has finite POSITION bounds");
            let center = Vec3::from(aabb.center);
            let half_extents = Vec3::from(aabb.half_extents);
            let converted = InstanceBounds::transformed(
                center - half_extents,
                center + half_extents,
                node_local.to_matrix() * mesh_local.to_matrix(),
            );
            (converted.min, converted.max)
        };
        let root_global = GlobalTransform::from(root_local.to_matrix());
        let root = world
            .spawn((
                root_local,
                root_global,
                WorldTransform(root_local.to_matrix()),
                ExpectedModelBounds { min, max },
            ))
            .id();
        let node_global = root_global.mul_transform(node_local);
        let node = world.spawn((node_local, node_global, ChildOf(root))).id();
        let mut mesh_local = mesh_local;
        mesh_local.translation += mesh_shift;
        world.spawn((
            mesh_local,
            node_global.mul_transform(mesh_local),
            Mesh3d(mesh),
            ChildOf(node),
        ));
        (root, ExpectedModelBounds { min, max })
    }

    fn bounds_case(root_local: Transform, mesh_shift: Vec3) -> BoundsOutcome {
        let mut app = App::new();
        let mut meshes = Assets::<Mesh>::default();
        let mesh = meshes.add(Cuboid::new(2.0, 4.0, 6.0));
        app.insert_resource(meshes)
            .init_resource::<Assets<WorldAsset>>();
        spawn_bounds_fixture(
            app.world_mut(),
            root_local,
            scene_node(),
            rotated_mesh_node(),
            mesh_shift,
            mesh,
        );
        app.init_resource::<BoundsOutcome>()
            .add_systems(Update, run_bounds_check);
        app.update();
        app.world_mut()
            .remove_resource::<BoundsOutcome>()
            .expect("the bounds check system ran")
    }

    fn scene_node() -> Transform {
        Transform::from_rotation(Quat::from_rotation_x(-0.55) * Quat::from_rotation_z(0.3))
            .with_translation(Vec3::new(-1.23, 4.07, 0.61))
    }

    fn rotated_mesh_node() -> Transform {
        Transform::from_rotation(
            Quat::from_rotation_y(0.7) * Quat::from_rotation_x(0.35) * Quat::from_rotation_z(-0.2),
        )
        .with_translation(Vec3::new(3.37, 0.51, -2.19))
        .with_scale(Vec3::new(1.5, 0.75, 2.0))
    }

    fn turned_node_case(billboard: bool) -> Result<TransformValidationSummary, String> {
        let mut app = App::new();
        let mut meshes = Assets::<Mesh>::default();
        let mesh = meshes.add(Cuboid::new(2.0, 4.0, 6.0));
        app.insert_resource(meshes)
            .init_resource::<Assets<WorldAsset>>();
        spawn_bounds_fixture(
            app.world_mut(),
            Transform::default(),
            scene_node(),
            rotated_mesh_node(),
            Vec3::ZERO,
            mesh,
        );
        let world = app.world_mut();
        let node = world
            .query_filtered::<Entity, With<Mesh3d>>()
            .single(world)
            .expect("the fixture has one mesh node");
        let authored = rotated_mesh_node().rotation;
        let mut entity = world.entity_mut(node);
        entity.get_mut::<Transform>().unwrap().rotation = Quat::from_rotation_y(1.1) * authored;
        if billboard {
            entity.insert(crate::billboard::Billboard {
                mode: crate::billboard::BillboardMode::TurnAboutUp,
                authored,
            });
        }
        app.init_resource::<BoundsOutcome>()
            .add_systems(Update, run_bounds_check);
        app.update();
        app.world_mut()
            .remove_resource::<BoundsOutcome>()
            .expect("the bounds check system ran")
            .check
            .expect("the bounds check ran")
    }
    // Native model fixture with authored per-reference collision inputs.
    fn spawn_collision_reference(
        app: &mut App,
        model: Handle<WorldAsset>,
        sequence: u64,
        path: &str,
        record_type: &str,
        physics: bool,
    ) -> Entity {
        let entity = spawn_pending_reference(app, model, sequence);
        let mut reference = app.world_mut().entity_mut(entity);
        let mut pending = reference.get_mut::<PendingAssetProfile>().unwrap();
        pending.path = path.to_owned();
        pending.base_record_type = Some(record_type.to_owned());
        pending.static_physics = physics;
        entity
    }

    mod tests {
        use super::*;

        #[test]
        fn invalid_authored_normal_preserves_collision_and_reveals_the_surface() {
            use bevy::asset::RenderAssetUsages;
            use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, AssetPlugin::default()))
                .init_asset::<Image>()
                .init_asset::<TerrainMaterial>()
                .init_resource::<EngineConfig>()
                .init_resource::<lod::LodStreaming>()
                .init_resource::<StreamingMetrics>()
                .init_resource::<ProfilingState>()
                .add_systems(Update, track_surface_readiness);
            app.edit_schedule(Update, |schedule| {
                schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
            });
            // sRGB is invalid for a declared linear terrain normal, even when fully loaded.
            let image = Image::new_fill(
                Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                &[128, 128, 255, 255],
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
            let mut valid_image = image.clone();
            valid_image.texture_descriptor.format = TextureFormat::Rgba8Unorm;
            valid_image.sampler = ImageSampler::Descriptor(crate::render::terrain_layer_sampler());
            let valid = app.world().resource::<AssetServer>().add(valid_image);
            let normal = app.world().resource::<AssetServer>().add(image);
            app.update();
            assert!(
                app.world()
                    .resource::<AssetServer>()
                    .is_loaded_with_dependencies(normal.id())
            );
            let material = app
                .world_mut()
                .resource_mut::<Assets<TerrainMaterial>>()
                .add(TerrainMaterial {
                    base: StandardMaterial::default(),
                    extension: authored_normals(normal.clone(), valid.clone()),
                });
            let entity = app
                .world_mut()
                .spawn((
                    TerrainCollider,
                    ColliderDisabled,
                    MeshMaterial3d(material.clone()),
                    PendingTerrainProfile {
                        cell_id: 7,
                        quadrant: 2,
                        images: vec![],
                        normals: vec![normal.clone(), valid.clone()],
                    },
                    Visibility::Hidden,
                    SwapSurface::new(material.id().untyped(), [normal.id(), valid.id()]),
                    SwapWaiting,
                ))
                .id();
            let warnings = NormalWarnings::default();
            tracing::subscriber::with_default(warnings.clone(), || app.update());
            assert!(app.world().get::<AssetFailed>(entity).is_none());
            assert!(app.world().get::<ColliderDisabled>(entity).is_none());
            assert_eq!(
                app.world().get::<Visibility>(entity),
                Some(&Visibility::Inherited)
            );
            assert!(app.world().get::<TerrainSurfaceReady>(entity).is_some());
            assert!(app.world().get::<PendingTerrainProfile>(entity).is_none());
            assert!(app.world().get::<SwapWaiting>(entity).is_some());
            let bindings: Vec<_> = app
                .world()
                .resource::<Assets<TerrainMaterial>>()
                .get(&material)
                .unwrap()
                .extension
                .image_ids()
                .collect();
            assert!(!bindings.contains(&normal.id()));
            assert!(!bindings.contains(&valid.id()));
            let declaration = image_ids(app.world().get::<SwapSurface>(entity).unwrap());
            assert!(!declaration.contains(&normal.id()));
            assert!(!declaration.contains(&valid.id()));
            let metrics = app.world().resource::<StreamingMetrics>();
            assert_eq!(metrics.terrain_validation_failures, 0);
            // Current owner logs the optional-normal fallback; no removed metric is restored.
            assert_eq!(warnings.count(), 1);
            assert_eq!(metrics.terrain_patches_validated, 1);
            assert_eq!(metrics.asset_load_failures, 0);
            tracing::subscriber::with_default(warnings.clone(), || app.update());
            assert_eq!(
                warnings.count(),
                1,
                "normal fallback is diagnosed only once"
            );
        }

        #[test]
        fn unresolved_terrain_dependencies_wait_or_report_their_reason() {
            use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
            let mut app = surface_app();
            let image = Image::new_fill(
                Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                &[128, 128, 255, 255],
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
            let mut valid_image = image.clone();
            valid_image.texture_descriptor.format = TextureFormat::Rgba8Unorm;
            valid_image.sampler = ImageSampler::Descriptor(crate::render::terrain_layer_sampler());
            let valid_normal = app.world().resource::<AssetServer>().add(valid_image);
            let invalid_normal = app.world().resource::<AssetServer>().add(image);
            app.update();
            let server = app.world().resource::<AssetServer>();
            let images = app.world().resource::<Assets<Image>>();
            let state = validate_surface_dependencies(
                server,
                images,
                &[invalid_normal],
                SurfaceImageKind::TerrainNormal,
            );
            assert!(matches!(state, SurfaceDependencyState::Failed(reason)
        if reason.contains("wrong color space")));
            let pending = app.world().resource::<Assets<Image>>().reserve_handle();
            assert!(matches!(
                validate_surface_dependencies(
                    server,
                    images,
                    &[pending],
                    SurfaceImageKind::TerrainNormal
                ),
                SurfaceDependencyState::Pending
            ));
            assert!(matches!(
                validate_surface_dependencies(
                    server,
                    images,
                    &[valid_normal],
                    SurfaceImageKind::TerrainNormal
                ),
                SurfaceDependencyState::Ready
            ));
        }

        #[test]
        fn the_grid_offset_and_its_inverse_agree_at_a_non_zero_origin() {
            let origin = IVec2::new(5, -3);
            let creation = Vec3::new(7.0 * CELL_SIZE + 1234.5, -2.0 * CELL_SIZE + 301.25, 812.0);
            let grid = IVec2::new(7, -2);
            let key = exterior_cell(grid.x, grid.y);

            let position = WorldPosition::from_creation_units(creation);
            let render = creation_to_bevy(position.relative_to(origin));
            assert_eq!(
                render + grid_translation(origin, IVec2::ZERO),
                creation_to_bevy(creation)
            );
            assert_eq!(streaming_center(render, origin), grid);
            assert_eq!(
                render - cell_translation(key, origin),
                creation_to_bevy(position.relative_to(IVec2::ZERO))
                    - cell_translation(key, IVec2::ZERO),
                "a point keeps its place inside its cell root whatever the origin"
            );
            // Two cells east and one north of the origin, in render space.
            assert_eq!(
                grid_translation(origin + IVec2::new(2, 1), origin),
                Vec3::new(2.0 * CELL_SIZE, 0.0, -CELL_SIZE)
            );
            // The origin's own cell sits at the render-space zero.
            assert_eq!(
                cell_translation(exterior_cell(origin.x, origin.y), origin),
                Vec3::ZERO
            );
        }

        #[test]
        fn a_full_cell_builds_terrain_colliders_only_in_a_walked_run() {
            for &(walk, expected) in &[(true, (4, 0)), (false, (0, 0))] {
                let directory = tempfile::tempdir().unwrap();
                let database_path = directory.path().join("world.db");
                write_terrain_database(&database_path);
                let cache_path = directory.path().join("cell_cache.rkyv");
                write_terrain_cache(&cache_path, 1);
                let mut app = terrain_streaming_app_with(&database_path, &cache_path, walk);
                let key = exterior_cell(2, -3);
                // The camera stands in the cell, which is the full window's centre.
                app.world_mut()
                    .spawn((Transform::default(), StreamingCamera));
                run_until(&mut app, "the camera's own full cell", |app| {
                    matches!(
                        app.world().resource::<StreamingWorld>().cells.get(&key),
                        Some(CellStatus::Resident { .. })
                    )
                });

                assert_eq!(
                    terrain_collider_state(&mut app),
                    expected,
                    "a walked run ({walk}) builds one enabled collider per quadrant"
                );
            }
        }

        #[test]
        fn a_quadrant_without_a_usable_collider_does_not_fail_the_cell() {
            let mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
            )
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![Vec3::ZERO, Vec3::X, Vec3::Y]);
            assert!(
                terrain_collider_from_mesh(&mesh).is_err(),
                "a mesh with no U32 triangle indices cannot become a terrain collider"
            );

            let directory = tempfile::tempdir().unwrap();
            let database_path = directory.path().join("world.db");
            write_terrain_database(&database_path);
            let cache_path = directory.path().join("cell_cache.rkyv");
            write_terrain_cache(&cache_path, 1);
            let mut app = terrain_streaming_app_with(&database_path, &cache_path, false);
            let key = exterior_cell(2, -3);
            app.world_mut()
                .spawn((Transform::default(), StreamingCamera));
            run_until(&mut app, "the cell drawn without a collider", |app| {
                matches!(
                    app.world().resource::<StreamingWorld>().cells.get(&key),
                    Some(CellStatus::Resident { .. })
                )
            });
            assert_eq!(
                terrain_collider_state(&mut app),
                (0, 0),
                "the cell commits and draws its quadrants with no collider built for them"
            );
            assert!(
                !app.world().resource::<StreamingWorld>().has_failed(&key),
                "and its commit is not a cell failure"
            );
            let roots = app
                .world_mut()
                .query_filtered::<Entity, With<TerrainPatch>>()
                .iter(app.world())
                .count();
            assert_eq!(roots, 4, "four quadrants are drawn for it");
        }

        #[test]
        fn a_retiring_cells_colliders_go_with_its_subtree() {
            let (mut app, _directory) = streaming_test_app(crossing_config(1));
            spawn_camera(&mut app, IVec2::ZERO);
            app.world_mut().resource_mut::<ActiveSpace>().interior = Some(98);
            let root = spawn_mock_interior(&mut app, 98);
            let collider = app
                .world_mut()
                .spawn((
                    TerrainCollider,
                    Collider::cuboid(1.0, 1.0, 1.0),
                    RigidBody::Fixed,
                    ChildOf(root),
                ))
                .id();
            // The crossing's immediate unload owns departing interior disposal.
            // A synthetic retiring interior alone is kept by the ordinary exterior planner.
            app.world_mut()
                .resource_mut::<StreamingWorld>()
                .cells
                .insert(CellKey::Interior(98), CellStatus::Retiring { root });

            unload_all_cells_now(app.world_mut());
            *app.world_mut().resource_mut::<ActiveSpace>() = ActiveSpace::default();
            app.update();

            assert!(
                app.world().get_entity(collider).is_err(),
                "the collider was despawned with the subtree its cell root owns"
            );
            assert!(app.world().get_entity(root).is_err(), "the root went too");
        }

        #[test]
        fn a_rebase_moves_the_camera_and_the_roots_with_colliders_present() {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(EngineConfig::default())
                .insert_resource(RenderOrigin(IVec2::ZERO))
                .init_resource::<ActiveSpace>()
                .init_resource::<StreamingMetrics>()
                .init_resource::<ProfilingState>()
                .init_resource::<temporal_history::ParticipantRebase>()
                .add_systems(Update, update_render_origin);
            let camera = app
                .world_mut()
                .spawn((
                    Transform::from_xyz(CELL_SIZE * 2.0, 0.0, 0.0),
                    StreamingCamera,
                ))
                .id();
            let root = app
                .world_mut()
                .spawn((ExteriorCellGrid(IVec2::new(3, 0)), Transform::default()))
                .id();
            let collider = app
                .world_mut()
                .spawn((
                    TerrainCollider,
                    Collider::cuboid(1.0, 1.0, 1.0),
                    RigidBody::Fixed,
                    Transform::default(),
                    GlobalTransform::default(),
                    ChildOf(root),
                ))
                .id();

            app.update();

            assert_eq!(
                app.world().resource::<RenderOrigin>().0,
                IVec2::new(2, 0),
                "the origin advances by the cells the camera moved"
            );
            assert_eq!(
                app.world()
                    .entity(camera)
                    .get::<Transform>()
                    .unwrap()
                    .translation,
                Vec3::ZERO,
                "and the camera is put back at the space it stands in"
            );
            assert_eq!(
                app.world()
                    .entity(root)
                    .get::<Transform>()
                    .unwrap()
                    .translation,
                Vec3::new(CELL_SIZE, 0.0, 0.0),
                "the cell root moves with the origin, so it keeps its place in the world"
            );
            assert!(
                app.world().entity(collider).contains::<Collider>(),
                "the collider is still there: without a rapier context there is no body to shift"
            );
            assert_eq!(app.world().resource::<StreamingMetrics>().origin_rebases, 1);
        }

        #[test]
        fn a_rebase_moves_live_rapier_bodies_with_their_roots() {
            use bevy::ecs::system::RunSystemOnce;
            use bevy_rapier3d::prelude::{RapierRigidBodyHandle, ReadRapierContext};

            let mut app = App::new();
            app.add_plugins((MinimalPlugins, TransformPlugin))
                .add_plugins(crate::physics::PhysicsCorePlugin)
                .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                    std::time::Duration::from_secs_f32(1.0 / 60.0),
                ))
                .insert_resource(EngineConfig::default())
                .insert_resource(RenderOrigin(IVec2::ZERO))
                .init_resource::<ActiveSpace>()
                .init_resource::<StreamingMetrics>()
                .init_resource::<ProfilingState>()
                .init_resource::<temporal_history::ParticipantRebase>()
                .add_systems(Update, update_render_origin);
            app.finish();
            let camera = app
                .world_mut()
                .spawn((Transform::default(), StreamingCamera))
                .id();
            let root = app
                .world_mut()
                .spawn((
                    ExteriorCellGrid(IVec2::new(3, 0)),
                    Transform::from_xyz(CELL_SIZE * 3.0, 0.0, 0.0),
                ))
                .id();
            let body = app
                .world_mut()
                .spawn((
                    TerrainCollider,
                    Collider::cuboid(10.0, 10.0, 10.0),
                    RigidBody::Fixed,
                    Transform::from_xyz(5.0, 0.0, 0.0),
                    ChildOf(root),
                ))
                .id();
            let body_x = |app: &mut App| {
                let handle = app
                    .world()
                    .entity(body)
                    .get::<RapierRigidBodyHandle>()
                    .expect("rapier has the body")
                    .0;
                app.world_mut()
                    .run_system_once(move |context: ReadRapierContext| {
                        context.single().unwrap().rigidbody_set.bodies[handle]
                            .translation()
                            .x
                    })
                    .unwrap()
            };
            for _ in 0..5 {
                app.update();
            }
            assert!((body_x(&mut app) - (CELL_SIZE * 3.0 + 5.0)).abs() < 1.0e-2);

            // Two cells east: the origin moves by two cells and everything drawn shifts back by them.
            app.world_mut()
                .entity_mut(camera)
                .get_mut::<Transform>()
                .unwrap()
                .translation
                .x = CELL_SIZE * 2.0;
            app.update();
            let root_x = app
                .world()
                .entity(root)
                .get::<Transform>()
                .unwrap()
                .translation
                .x;
            assert!(
                (root_x - CELL_SIZE).abs() < 1.0e-2,
                "the root moved: {root_x}"
            );
            let moved = body_x(&mut app);
            assert!(
                (moved - (root_x + 5.0)).abs() < 1.0e-2,
                "the body sits where its root now is: body {moved}, root {root_x}"
            );
        }

        #[test]
        fn pending_under_counts_a_visible_waiting_surface_and_ignores_a_hidden_one() {
            let mut world = World::new();
            let root = world.spawn_empty().id();
            let material = Handle::<StandardMaterial>::default().id().untyped();
            let visible = world
                .spawn((
                    Mesh3d(Handle::default()),
                    SwapSurface::new(material, []),
                    SwapWaiting,
                    ChildOf(root),
                ))
                .id();
            let hidden = world
                .spawn((
                    Mesh3d(Handle::default()),
                    SwapSurface::new(material, []),
                    SwapWaiting,
                    ChildOf(root),
                ))
                .id();
            publish_completed_camera_observations(
                &mut world,
                &[(visible, true, false), (hidden, false, false)],
            );
            let mut state = SystemState::<PendingUnder>::new(&mut world);

            let count = state.get(&world).unwrap().count(root);
            assert_eq!(
                count, 1,
                "only the surface a view can see holds the crossing"
            );

            // The old pending-work markers still count whatever their visibility.
            world.spawn((pending_marker_for_test(), ChildOf(root)));
            assert_eq!(state.get(&world).unwrap().count(root), 2);

            // Once the visible surface is drawn (its marker is gone) and the hidden one is not needed,
            // only the model marker is left.
            // Redeclaration withdraws old evidence before the new completed Render observation.
            crate::swap_bridge::rearm_swap(&mut world, visible);
            assert!(world.get::<SwapCameraObserved>(visible).is_none());
            assert_eq!(
                state.get(&world).unwrap().count(root),
                2,
                "the rearmed visible declaration is unknown until a fresh completion"
            );
            publish_completed_camera_observations(&mut world, &[(visible, true, true)]);
            assert!(
                world
                    .get::<crate::swap_bridge::SwapDrawn>(visible)
                    .is_some()
            );
            assert!(world.get::<SwapWaiting>(visible).is_none());
            assert!(world.get::<SwapCameraObserved>(hidden).is_some());
            assert_eq!(state.get(&world).unwrap().count(root), 1);
        }

        #[test]
        fn pending_under_reports_a_failed_asset_after_its_pending_markers_are_gone() {
            let mut world = World::new();
            let root = world.spawn_empty().id();
            let model = world.spawn((pending_marker_for_test(), ChildOf(root))).id();
            world.spawn((Visibility::Hidden, ChildOf(model)));
            let mut state = SystemState::<PendingUnder>::new(&mut world);
            assert_eq!(state.get(&world).unwrap().failed(root), 0);

            // The failure branch: pending marker removed, failure recorded.
            world
                .entity_mut(model)
                .remove::<PendingTerrainProfile>()
                .insert(AssetFailed);
            let under = state.get(&world).unwrap();
            assert_eq!(under.count(root), 0, "nothing is pending any more");
            assert_eq!(under.failed(root), 1, "the failure is still on record");
        }

        #[test]
        fn accepts_matching_neighbor_edges_and_welds_small_cracks() {
            let mut continuity = TerrainContinuity::default();
            let mut metrics = StreamingMetrics::default();
            let west = exterior_cell(0, 0);
            let east = exterior_cell(1, 0);
            let mut west_terrain = terrain_fixture(1, 10.0);
            validate_and_register_terrain_edges(
                west,
                &mut west_terrain,
                &mut continuity,
                &mut metrics,
            )
            .unwrap();
            let mut east_terrain = terrain_fixture(2, 10.0);
            validate_and_register_terrain_edges(
                east,
                &mut east_terrain,
                &mut continuity,
                &mut metrics,
            )
            .unwrap();
            assert_eq!(metrics.terrain_seams_validated, 1);
            assert_eq!(
                metrics.terrain_seam_points_welded, 0,
                "a matching edge moves nothing"
            );

            // A one-unit crack is a seam, not a failure: the arriving cell takes the resident heights
            // along the shared edge and keeps its own everywhere else.
            let mut farther_east_terrain = terrain_fixture(3, 11.0);
            validate_and_register_terrain_edges(
                exterior_cell(2, 0),
                &mut farther_east_terrain,
                &mut continuity,
                &mut metrics,
            )
            .unwrap();
            assert!(
                farther_east_terrain
                    .heights
                    .iter()
                    .enumerate()
                    .all(|(index, height)| *height == if index % 33 == 0 { 10.0 } else { 11.0 })
            );
            assert_eq!(metrics.terrain_seams_validated, 2);
            assert_eq!(metrics.terrain_seam_points_welded, 33);

            // Past the bound it is not a seam but two landscapes authored apart, as where a city's
            // sculpted land meets the unsculpted copy beside it: the cell is kept as authored, not
            // welded, and registered so the cells after it weld onto what is drawn.
            let authored = exterior_cell(3, 0);
            let authored_height = 11.0 + MAX_WELDABLE_EDGE_DELTA + 1.0;
            let mut authored_terrain = terrain_fixture(4, authored_height);
            validate_and_register_terrain_edges(
                authored,
                &mut authored_terrain,
                &mut continuity,
                &mut metrics,
            )
            .expect(
                "an edge past the weld bound is drawn as authored, not a reason to drop the cell",
            );
            assert!(
                authored_terrain
                    .heights
                    .iter()
                    .all(|height| *height == authored_height),
                "the edge past the bound keeps its authored heights"
            );
            assert!(continuity.edges.contains_key(&authored));
            assert_eq!(metrics.terrain_edges_left_as_authored, 1);
            assert_eq!(
                metrics.terrain_seam_points_welded, 33,
                "nothing was welded onto it"
            );
        }

        #[test]
        fn welds_one_point_of_an_edge_and_spawns_the_arriving_cell() {
            let mut continuity = TerrainContinuity::default();
            let mut metrics = StreamingMetrics::default();
            let resident_key = exterior_cell(0, 0);
            let arriving_key = exterior_cell(0, 1);
            let mut resident = untextured_fixture(0x0000_8F64, 100.0);
            validate_and_register_terrain_edges(
                resident_key,
                &mut resident,
                &mut continuity,
                &mut metrics,
            )
            .unwrap();

            // The arriving cell's south edge - height-field row 0 - is the edge it shares with the
            // resident cell's north edge, and it differs at one of its 33 points by 24 units.
            let mut arriving = untextured_fixture(0x0000_8F65, 100.0);
            arriving.heights[9] = 124.0;
            validate_and_register_terrain_edges(
                arriving_key,
                &mut arriving,
                &mut continuity,
                &mut metrics,
            )
            .expect("a seam inside the weldable range must not reject the cell");

            for column in 0..33 {
                assert_eq!(
                    arriving.heights[column],
                    resident.heights[32 * 33 + column],
                    "point {column} of the shared edge"
                );
            }
            assert_eq!(arriving.heights[9], 100.0, "the arriving cell moved");
            assert_eq!(arriving.heights[33 + 9], 100.0, "only the edge moved");
            assert_eq!(metrics.terrain_seams_validated, 1);
            assert_eq!(metrics.terrain_seam_points_welded, 1);

            // Both cells spawn, the arriving one with the welded height in its mesh, and the
            // lifecycle validator has nothing to report about either.
            let mut app = spawn_cells_app(
                vec![(resident_key, resident), (arriving_key, arriving)],
                metrics,
            );
            let metrics = app.world().resource::<StreamingMetrics>();
            assert_eq!(metrics.terrain_seam_points_welded, 1);
            assert_eq!(metrics.streaming_invariant_failures, 0);
            let handles: Vec<Handle<Mesh>> = {
                let mut patches = app
                    .world_mut()
                    .query_filtered::<&Mesh3d, With<TerrainPatch>>();
                patches
                    .iter(app.world())
                    .map(|mesh| mesh.0.clone())
                    .collect()
            };
            assert_eq!(handles.len(), 8, "two cells of four quadrants each");
            let meshes = app.world().resource::<Assets<Mesh>>();
            let mut vertices = 0usize;
            for handle in &handles {
                let mesh = meshes.get(handle).expect("a spawned quadrant has its mesh");
                let positions = mesh
                    .attribute(Mesh::ATTRIBUTE_POSITION)
                    .unwrap()
                    .as_float3()
                    .unwrap();
                assert!(
                    positions.iter().all(|position| position[1] == 100.0),
                    "the seam must not survive into the geometry"
                );
                vertices += positions.len();
            }
            assert_eq!(vertices, 8 * 17 * 17);
        }

        #[test]
        fn welded_normals_refresh_the_symmetric_neighbor_stencil() {
            let mut continuity = TerrainContinuity::default();
            let mut metrics = StreamingMetrics::default();
            let mut resident = untextured_fixture(1, 100.0);
            validate_and_register_terrain_edges(
                exterior_cell(0, 0),
                &mut resident,
                &mut continuity,
                &mut metrics,
            )
            .unwrap();

            // Rows 0 and 1 of the arriving cell stand 24 units above the resident cell, so row 0 is
            // welded down onto it and row 1 stays where it is: the welded point's normal must lean
            // towards the higher row instead of keeping the flat normal it was loaded with.
            let mut arriving = untextured_fixture(2, 100.0);
            for column in 0..33 {
                arriving.heights[column] = 124.0;
                arriving.heights[33 + column] = 124.0;
            }
            validate_and_register_terrain_edges(
                exterior_cell(0, 1),
                &mut arriving,
                &mut continuity,
                &mut metrics,
            )
            .unwrap();
            assert_eq!(metrics.terrain_seam_points_welded, 33);

            // At (x, y) = (9, 0), on the south edge, the y stencil spans one 128-unit interval (main's
            // span-divided stencil): (0, -24 / 128, 1) normalized and scaled by 127 is (0, -23, 125).
            assert_eq!(arriving.normals[9 * 3..9 * 3 + 3], [0, -23, 125]);
            assert_eq!(
                arriving.normals[(33 + 9) * 3..(33 + 9) * 3 + 3],
                [0, 0, 127],
                "the refreshed symmetric neighbor stencil remains flat"
            );
        }

        #[test]
        fn still_rejects_wrong_edge_lengths_and_non_finite_heights() {
            let mut continuity = TerrainContinuity::default();
            let mut metrics = StreamingMetrics::default();
            let mut short_neighbor = untextured_fixture(1, 10.0);
            short_neighbor.width = 17;
            short_neighbor.height = 17;
            short_neighbor.heights = vec![10.0; 17 * 17];
            validate_and_register_terrain_edges(
                exterior_cell(0, 0),
                &mut short_neighbor,
                &mut continuity,
                &mut metrics,
            )
            .unwrap();

            let mut full = untextured_fixture(2, 10.0);
            let error = validate_and_register_terrain_edges(
                exterior_cell(1, 0),
                &mut full,
                &mut continuity,
                &mut metrics,
            )
            .unwrap_err();
            assert!(error.contains("points"), "{error}");

            let mut continuity = TerrainContinuity::default();
            let mut metrics = StreamingMetrics::default();
            let mut resident = untextured_fixture(3, 10.0);
            validate_and_register_terrain_edges(
                exterior_cell(2, 0),
                &mut resident,
                &mut continuity,
                &mut metrics,
            )
            .unwrap();
            let mut broken = untextured_fixture(4, 10.0);
            broken.heights[4 * 33] = f32::NAN;
            let error = validate_and_register_terrain_edges(
                exterior_cell(3, 0),
                &mut broken,
                &mut continuity,
                &mut metrics,
            )
            .unwrap_err();
            assert!(error.contains("non-finite"), "{error}");

            // A value `f32::max` would quietly fold away is a rejection, not a weld.
            continuity.edges.insert(
                exterior_cell(3, 0),
                TerrainEdges {
                    west: vec![f32::NAN; 33],
                    east: vec![f32::NAN; 33],
                    south: vec![10.0; 33],
                    north: vec![10.0; 33],
                },
            );
            let mut arriving = untextured_fixture(5, 10.0);
            assert!(
                validate_and_register_terrain_edges(
                    exterior_cell(4, 0),
                    &mut arriving,
                    &mut continuity,
                    &mut metrics
                )
                .is_err()
            );
        }

        #[test]
        fn finite_authored_water_planes_are_emitted_even_when_terrain_occludes_them() {
            let mut dry = untextured_fixture(1, 200.0);
            dry.water_height = Some(100.0);
            let mut wet = untextured_fixture(2, 200.0);
            wet.water_height = Some(100.0);
            wet.heights[16 * 33 + 16] = 99.0;
            let mut app = spawn_cells_app(
                vec![(exterior_cell(0, 0), dry), (exterior_cell(1, 0), wet)],
                StreamingMetrics::default(),
            );

            let mut roots = app.world_mut().query::<(&ExteriorCellGrid, &Children)>();
            let mut planes_per_cell = Vec::new();
            for (grid, children) in roots.iter(app.world()) {
                let planes = children
                    .iter()
                    .filter(|child| app.world().get::<WaterSurface>(*child).is_some())
                    .count();
                planes_per_cell.push(((grid.0.x, grid.0.y), planes));
            }
            planes_per_cell.sort();
            assert_eq!(
                planes_per_cell,
                vec![((0, 0), 1), ((1, 0), 1)],
                "finite authored cell water is emitted even when terrain occludes the plane"
            );

            let mut surfaces = app.world_mut().query::<(&WaterSurface, &Transform)>();
            let heights: Vec<f32> = surfaces
                .iter(app.world())
                .map(|(_, transform)| transform.translation.y)
                .collect();
            assert_eq!(heights, vec![100.0, 100.0]);
        }

        #[test]
        fn bounds_check_ignores_the_reference_distance_from_the_origin() {
            let mut at_origin = bounds_case(Transform::default(), Vec3::ZERO);
            let mut far = bounds_case(far_reference(), Vec3::ZERO);

            let (at_origin_check, far_check) = (
                at_origin.check.take().expect("the bounds check ran"),
                far.check.take().expect("the bounds check ran"),
            );
            assert!(at_origin_check.is_ok(), "{at_origin_check:?}");
            assert!(far_check.is_ok(), "{far_check:?}");
            let expected = far.expected.expect("the fixture carries converted bounds");
            assert_eq!(expected, at_origin.expected.unwrap());
            let at_origin = at_origin
                .bounds
                .expect("bounds were measured")
                .expect("the hierarchy has bounded meshes");
            let far = far
                .bounds
                .expect("bounds were measured")
                .expect("the hierarchy has bounded meshes");
            assert_eq!(
                at_origin.nodes, 2,
                "the fixture has two descendants under the reference"
            );
            assert_eq!(
                far.nodes, 2,
                "the fixture has two descendants under the reference"
            );

            for (label, bounds) in [("origin", at_origin), ("far from the origin", far)] {
                assert!(
                    bounds.min.abs_diff_eq(expected.min, 1.0e-5)
                        && bounds.max.abs_diff_eq(expected.max, 1.0e-5),
                    "measured bounds at the {label} ({:?}..{:?}) differ from the converted bounds {:?}..{:?}",
                    bounds.min,
                    bounds.max,
                    expected.min,
                    expected.max
                );
            }
            assert!(
                far.min.abs_diff_eq(at_origin.min, 1.0e-5)
                    && far.max.abs_diff_eq(at_origin.max, 1.0e-5),
                "measured bounds moved with the reference: {:?}..{:?} at the origin, {:?}..{:?} far from it",
                at_origin.min,
                at_origin.max,
                far.min,
                far.max
            );
        }

        #[test]
        fn bounds_check_rejects_a_moved_mesh_at_any_distance_from_the_origin() {
            for root_local in [Transform::default(), far_reference()] {
                let outcome = bounds_case(root_local, Vec3::new(0.5, 0.0, 0.0));
                let reason = outcome.check.expect("the bounds check ran").expect_err(
                    "a mesh moved 0.5 units from the converted bounds must be rejected",
                );
                assert!(
                    reason.contains("spawned hierarchy bounds diverge from conversion"),
                    "unexpected rejection reason at {root_local:?}: {reason}"
                );
            }
        }

        #[test]
        fn bounds_check_measures_a_billboard_at_its_authored_rotation() {
            let turned_billboard = turned_node_case(true);
            assert!(
                turned_billboard.is_ok(),
                "a billboard turned toward the camera must pass: {turned_billboard:?}"
            );
            let reason = turned_node_case(false)
                .expect_err("the same turn on an ordinary node is a real divergence");
            assert!(
                reason.contains("spawned hierarchy bounds diverge from conversion"),
                "{reason}"
            );
        }

        #[test]
        fn a_fixed_static_gets_a_proxy_collider_and_a_door_gets_none() {
            let mut app = model_app();
            let mesh = app
                .world_mut()
                .resource_mut::<Assets<Mesh>>()
                .add(Cuboid::new(2.0, 4.0, 6.0));
            let material = app
                .world_mut()
                .resource_mut::<Assets<StandardMaterial>>()
                .add(StandardMaterial::default());
            let model =
                add_converted_model(&mut app, converted_scene_with_material(mesh, material));
            // The converted (lowercase) path `converted_model_path` returns, which the eligibility
            // rules are written in.
            let stat = spawn_collision_reference(
                &mut app,
                model.clone(),
                0,
                "meshes/architecture/farmhouse/farmhouse01.glb",
                "STAT",
                true,
            );
            app.world_mut().entity_mut(stat).insert(cuboid_bounds());
            let door = spawn_collision_reference(
                &mut app,
                model,
                1,
                "meshes/architecture/farmhouse/farmhouseldoor01.glb",
                "DOOR",
                true,
            );
            app.world_mut().entity_mut(door).insert(cuboid_bounds());

            let metrics = settle_readiness(&mut app);

            assert_eq!(
                metrics.static_colliders_proxy, 1,
                "one of the two placements is known collision: {:?}",
                metrics.asset_failures
            );
            assert_eq!(
                metrics.static_colliders_skipped, 0,
                "a DOOR is not eligible for collision, which is not the same as a proxy that failed"
            );
            let world = app.world();
            let stat_entity = world.entity(stat);
            assert_eq!(
                stat_entity.get::<StaticColliderSource>(),
                Some(&StaticColliderSource::RenderProxy)
            );
            assert!(stat_entity.contains::<Collider>());
            assert!(stat_entity.contains::<RigidBody>());
            assert!(
                !stat_entity.contains::<ColliderDisabled>(),
                "a vanilla static proxy is enabled without a private space gate"
            );
            let door_entity = world.entity(door);
            assert!(
                door_entity.get::<StaticColliderSource>().is_none(),
                "a load door gets no collider"
            );
            assert!(!door_entity.contains::<Collider>());
        }

        #[test]
        fn a_model_with_primitives_but_no_converted_bounds_still_fails() {
            let mut app = model_app();
            let mesh = app
                .world_mut()
                .resource_mut::<Assets<Mesh>>()
                .add(Cuboid::new(2.0, 4.0, 6.0));
            let mut converted = World::new();
            let scene_root = converted.spawn(Transform::default()).id();
            converted.spawn((Mesh3d(mesh), Transform::default(), ChildOf(scene_root)));
            let handle = add_converted_model(&mut app, converted);
            spawn_model_reference(&mut app, handle, None);

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
                    .any(|reason| reason.contains("no validated aggregate bounds")),
                "unexpected failure reason: {:?}",
                failure.dependency_chain
            );
        }
    }
    mod upstream_tests {
        use super::*;

        #[test]
        fn bounds_check_measures_a_billboard_at_its_authored_rotation() {
            let turned_billboard = turned_node_case(true);
            assert!(
                turned_billboard.is_ok(),
                "a billboard turned toward the camera must pass: {turned_billboard:?}"
            );
            let reason = turned_node_case(false)
                .expect_err("the same turn on an ordinary node is a real divergence");
            assert!(
                reason.contains("spawned hierarchy bounds diverge from conversion"),
                "{reason}"
            );
        }
    }
}
