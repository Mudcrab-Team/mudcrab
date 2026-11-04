mod loading_compatibility {
    use super::*;
    use bevy::ecs::system::{RunSystemOnce, SystemState};
    use std::time::Duration;

    #[test]
    fn window_residency_counts_only_the_active_interior_inside() {
        let mut streaming = StreamingWorld::default();
        let inside = ActiveSpace {
            worldspace_id: None,
            interior: Some(77),
        };
        // Entering an interior unloads every exterior cell; the window is the one interior cell.
        assert_eq!(
            streaming.active_residency(&inside, 0x3c, IVec2::ZERO, 2),
            (1, 0, 0, 0)
        );
        // An exterior cell left in the map does not count towards the interior's window.
        streaming.set_resident_for_test(
            CellKey::Exterior {
                worldspace_id: 0x3c,
                grid_x: 0,
                grid_y: 0,
            },
            Entity::PLACEHOLDER,
        );
        assert_eq!(
            streaming.active_residency(&inside, 0x3c, IVec2::ZERO, 2),
            (1, 0, 0, 0)
        );
        streaming.set_resident_for_test(CellKey::Interior(77), Entity::PLACEHOLDER);
        assert_eq!(
            streaming.active_residency(&inside, 0x3c, IVec2::ZERO, 2),
            (1, 1, 0, 0)
        );
        streaming.set_failed_for_test(CellKey::Interior(77));
        assert_eq!(
            streaming.active_residency(&inside, 0x3c, IVec2::ZERO, 2),
            (1, 0, 1, 0)
        );
        streaming
            .cells
            .insert(CellKey::Interior(77), CellStatus::Absent);
        assert_eq!(
            streaming.active_residency(&inside, 0x3c, IVec2::ZERO, 2),
            (1, 0, 0, 1)
        );
    }

    #[test]
    fn window_residency_reads_the_active_exterior_worldspace() {
        let mut streaming = StreamingWorld::default();
        let configured = 0x3c;
        let other = ActiveSpace {
            worldspace_id: Some(0x1a26f),
            interior: None,
        };
        for (worldspace_id, grid_x) in [(configured, 0), (0x1a26f, 0), (0x1a26f, 1)] {
            streaming.set_resident_for_test(
                CellKey::Exterior {
                    worldspace_id,
                    grid_x,
                    grid_y: 0,
                },
                Entity::PLACEHOLDER,
            );
        }
        // The default space is the configured worldspace: one of its 9 window cells is resident.
        assert_eq!(
            streaming.active_residency(&ActiveSpace::default(), configured, IVec2::ZERO, 1),
            (9, 1, 0, 0)
        );
        // After a crossing into another worldspace, its own cells count, not the configured one's.
        assert_eq!(
            streaming.active_residency(&other, configured, IVec2::ZERO, 1),
            (9, 2, 0, 0)
        );
        streaming.cells.insert(
            CellKey::Exterior {
                worldspace_id: 0x1a26f,
                grid_x: -1,
                grid_y: 0,
            },
            CellStatus::Absent,
        );
        assert_eq!(
            streaming.active_residency(&other, configured, IVec2::ZERO, 1),
            (9, 2, 0, 1)
        );
    }

    #[test]
    fn wanted_cells_are_the_active_interior_or_the_active_worldspace_grid() {
        let center = IVec2::new(4, -2);
        let inside = ActiveSpace {
            worldspace_id: Some(60),
            interior: Some(99),
        };
        assert_eq!(
            wanted_cells(&inside, 60, center, 1),
            HashSet::from([CellKey::Interior(99)]),
            "the active interior excludes all exteriors"
        );
        let outside = ActiveSpace {
            worldspace_id: Some(614),
            interior: None,
        };
        let wanted = wanted_cells(&outside, 60, center, 1);
        assert_eq!(wanted.len(), 9);
        assert!(wanted.iter().all(|key| matches!(
            key,
            CellKey::Exterior {
                worldspace_id: 614,
                ..
            }
        )));
        assert!(wanted.contains(&CellKey::Exterior {
            worldspace_id: 614,
            grid_x: 5,
            grid_y: -3
        }));
    }

    #[test]
    fn wanted_cells_respect_grid_bounds_and_world_identity() {
        let center = IVec2::new(4, -2);
        let outside = ActiveSpace {
            worldspace_id: Some(60),
            interior: None,
        };
        let exterior = |grid_x, grid_y| CellKey::Exterior {
            worldspace_id: 60,
            grid_x,
            grid_y,
        };
        let wanted = wanted_cells(&outside, 60, center, 2);
        assert_eq!(wanted.len(), 25);
        // The native wanted set is unordered; check nearest-first at its real planner owner.
        let (mut app, _directory) = streaming_test_app(EngineConfig {
            stream_radius: 2,
            unload_radius: 2,
            prioritize_streaming: true,
            max_cell_commits_per_frame: 1,
            max_streaming_backlog: 1,
            ..default()
        });
        let (database, requests, _responses) = WorldDatabase::channel_fixture(1);
        app.insert_resource(database);
        spawn_camera(&mut app, center);
        app.update();
        let DatabaseRequest::Load { key: first, .. } = requests.try_recv().unwrap() else {
            panic!("planner must request a full cell");
        };
        assert_eq!(first, exterior(4, -2), "nearest first");
        for key in [exterior(4, -2), exterior(2, -4), exterior(6, 0)] {
            assert!(
                wanted.contains(&key),
                "own cell and both corners are inside"
            );
        }
        for key in [
            exterior(7, -2),
            exterior(4, 3),
            exterior(9, -7),
            exterior(10, -2),
            exterior(4, 4),
            exterior(4, 8),
            CellKey::Interior(99),
            CellKey::Exterior {
                worldspace_id: 614,
                grid_x: 4,
                grid_y: -2,
            },
        ] {
            assert!(
                !wanted.contains(&key),
                "outside grid or wrong space: {key:?}"
            );
        }
        let wider = wanted_cells(&outside, 60, center, 3);
        assert!(wider.contains(&exterior(7, -2)));
        assert!(!wider.contains(&exterior(8, -2)));
        assert_eq!(
            wanted_cells(&outside, 60, center, 0),
            HashSet::from([exterior(4, -2)])
        );
    }

    #[test]
    fn the_lod_horizon_is_known_per_worldspace() {
        let mut horizon = LodHorizon::default();
        assert_eq!(horizon.reach_in(60), None, "nothing known before an answer");
        horizon.set(60, 372_000.0);
        assert_eq!(horizon.reach_in(60), Some(372_000.0));
        assert_eq!(
            horizon.reach_in(61),
            None,
            "another worldspace has not answered"
        );
    }

    #[test]
    fn a_crossing_into_an_interior_unloads_the_space_it_left_in_one_frame() {
        let (mut app, _directory) = streaming_test_app(crossing_config(2));
        spawn_camera(&mut app, IVec2::ZERO);
        let left_behind: Vec<Entity> = (0..3)
            .map(|grid_x| spawn_mock_cell(&mut app, grid_x, 0, true))
            .collect();
        app.update();
        assert_eq!(resident_root_count(&mut app), 3);

        // The real door-crossing switch_space driver calls this native operation before
        // publishing ActiveSpace. Mutating ActiveSpace alone does not drive a crossing.
        unload_all_cells_now(app.world_mut());
        *app.world_mut().resource_mut::<ActiveSpace>() = ActiveSpace {
            worldspace_id: Some(60),
            interior: Some(99),
        };
        let interior = app
            .world_mut()
            .spawn((
                CellRef(99),
                StreamedCellRoot,
                Transform::default(),
                Visibility::default(),
            ))
            .id();
        app.world_mut()
            .resource_mut::<StreamingWorld>()
            .cells
            .insert(
                CellKey::Interior(99),
                CellStatus::Resident { root: interior },
            );
        app.update();

        let metrics = streaming_metrics(&app);
        assert_eq!(
            metrics.despawns_this_frame, 0,
            "a space change is not paced: nothing waited for the budget"
        );
        assert_eq!(metrics.retiring_cells, 0);
        for root in &left_behind {
            assert!(
                app.world().get_entity(*root).is_err(),
                "an exterior of the space the camera left must go the frame it falls out"
            );
        }
        assert!(app.world().get_entity(interior).is_ok());
        assert!(
            app.world()
                .resource::<StreamingWorld>()
                .cells
                .contains_key(&CellKey::Interior(99)),
            "the interior the camera is in is the whole plan"
        );
        assert_eq!(metrics.streaming_invariant_failures, 0);
    }

    #[test]
    fn cluster_ownership_defers_instantiation_until_the_certificate_can_decide() {
        use crate::cluster::{
            ownership::{OwnershipFacts, OwnershipReference, install},
            reader::producer_fixture_tests::published_for_loader,
        };
        let directory = published_for_loader();
        let package = std::sync::Arc::new(
            crate::cluster::overlay_fixture::open_fixture(directory.path(), false).unwrap(),
        );
        let mut app = model_app();
        install(&mut app);
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(2.0, 4.0, 6.0));
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        let model = add_converted_model(&mut app, converted_scene_with_material(mesh, material));
        let root = spawn_pending_reference(&mut app, model, 0);
        app.world_mut()
            .entity_mut(root)
            .insert(OwnershipReference(0x00F9907));
        app.update();
        assert!(app.world().get::<PendingModel>(root).is_some());
        assert!(app.world().get::<WorldAssetRoot>(root).is_none());
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .pending_asset_instances,
            1
        );
        // The certificate proves this reference is not owned; unrelated unresolved
        // certified references must not prevent its ordinary model from spawning.
        app.world_mut()
            .resource_mut::<OwnershipFacts>()
            .publish(&package, Default::default());
        app.update();
        assert!(app.world().get::<PendingModel>(root).is_none());
        assert!(app.world().get::<WorldAssetRoot>(root).is_some());
    }

    #[test]
    fn the_ordinary_arming_budget_preserves_its_fixed_floor_and_unlimited_mode() {
        // Keep the unambiguous ordinary floor/unlimited samples at the actual arming system.
        for (floor, ready, expected, reason) in [
            (4, 3, 3, "never more than is ready"),
            (4, 100, 4, "a small backlog keeps the floor"),
            (0, 2_744, 2_744, "0 arms every ready model"),
        ] {
            let mut app = model_app();
            app.insert_resource(paced_config(floor));
            let model = add_converted_model(&mut app, empty_converted_scene());
            for sequence in 0..ready {
                spawn_pending_reference(&mut app, model.clone(), sequence as u64);
            }
            app.update();
            assert_eq!(
                armed_and_pending(&mut app),
                (expected, ready - expected),
                "{reason}"
            );
        }
    }

    #[test]
    fn the_arming_budget_arms_the_model_nearest_the_camera_first() {
        let mut app = model_app();
        let mut config = paced_config(1);
        config.prioritize_streaming = true;
        app.insert_resource(config);
        app.world_mut().spawn((
            StreamingCamera,
            Transform::default(),
            GlobalTransform::IDENTITY,
        ));
        let far = add_converted_model(&mut app, empty_converted_scene());
        let near = add_converted_model(&mut app, empty_converted_scene());
        // The far model is the older one, so commit order alone would arm it first.
        let far = spawn_pending_reference(&mut app, far, 0);
        let near = spawn_pending_reference(&mut app, near, 1);
        let place = |app: &mut App, entity: Entity, at: Vec3| {
            let transform = Transform::from_translation(at);
            let model = app.world().get::<PendingModel>(entity).unwrap();
            let request = requests::SceneRequest {
                path: app
                    .world()
                    .get::<PendingAssetProfile>(entity)
                    .unwrap()
                    .path
                    .clone(),
                cell: None,
                coarse_terrain: false,
                retry_cleanup: None,
                content_identity: None,
                sequence: model.sequence,
                center: at.as_dvec3(),
                radius: Some(1.0),
                collision_candidate: false,
                handle: Some(model.handle.clone()),
                started: Instant::now(),
                allow_retry: false,
            };
            app.world_mut().entity_mut(entity).insert((
                transform,
                GlobalTransform::from(transform),
                request,
            ));
        };
        place(&mut app, far, Vec3::new(5_000.0, 0.0, 0.0));
        place(&mut app, near, Vec3::new(10.0, 0.0, 0.0));

        app.update();
        assert!(app.world().entity(near).contains::<WorldAssetRoot>());
        assert!(!app.world().entity(far).contains::<WorldAssetRoot>());
    }

    fn collector_app(
        config: EngineConfig,
    ) -> (
        App,
        tempfile::TempDir,
        crossbeam_channel::Sender<crate::world::database::DatabaseResponse>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let catalog_path = directory.path().join("catalog.db");
        write_empty_catalogue(&catalog_path);
        let cache_path = directory.path().join("cells.rkyv");
        std::fs::write(
            &cache_path,
            rkyv::to_bytes::<rkyv::rancor::Error>(&shared::CellCache {
                version: shared::CELL_CACHE_VERSION,
                cells: vec![],
            })
            .unwrap(),
        )
        .unwrap();
        let mut app = model_app();
        app.insert_resource(config)
            .insert_resource(AssetCatalog::open(&catalog_path).unwrap())
            .init_resource::<ActorCatalog>()
            .insert_resource(CellCache::open(&cache_path).unwrap())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(WaterReflectionTexture(Handle::default()))
            .init_asset::<TerrainMaterial>()
            .init_asset::<WaterMaterial>()
            .init_resource::<StreamingCommitBudget>();
        let (database, _requests, responses) = WorldDatabase::channel_fixture(8);
        app.insert_resource(database);
        (app, directory, responses)
    }

    // Native controller fixture: a decision with the requested time budget, while the
    // runtime's placement reservation accounting remains disabled as in an ordinary unselected run.
    fn install_commit_decision(app: &mut App, max_commit_micros: u64) {
        use crate::streaming::control::{
            ControllerInput, ControllerSettings, ControllerState, StageBudgets,
        };
        let limits = StageBudgets {
            max_commit_micros,
            ..default()
        };
        let decision = ControllerState::default().tick(
            &ControllerSettings {
                fixed: limits,
                ..default()
            },
            ControllerInput {
                delta_seconds: 1.0 / 60.0,
                frame_ms: None,
                gpu_frame_ms: None,
                streaming_work_ms: 0.0,
                camera_moving: false,
                camera_turning: false,
                camera_discontinuity: false,
                useful_nearby_ready: false,
                demand_settled: false,
                backlog: default(),
                active_scene_jobs: 0,
                hard_limits: limits,
                memory_blocked: false,
                mandatory_collision_pending: false,
            },
        );
        app.init_resource::<runtime::StreamingRuntime>();
        app.world_mut()
            .resource_mut::<runtime::StreamingRuntime>()
            .decision = Some(decision);
    }

    fn enqueue_cell_response(
        app: &mut App,
        responses: &crossbeam_channel::Sender<crate::world::database::DatabaseResponse>,
        key: CellKey,
        failed: bool,
    ) {
        let generation = 1;
        let mut streaming = app.world_mut().resource_mut::<StreamingWorld>();
        streaming
            .cells
            .insert(key, CellStatus::Loading { generation });
        streaming.outstanding_cells.insert(key, generation);
        responses
            .send(crate::world::database::DatabaseResponse {
                generation,
                key,
                result: if failed {
                    Err("fixture full commit failure".to_owned())
                } else {
                    Ok(Some(crate::world::database::CellPayload {
                        generation,
                        key,
                        cell_id: 1,
                        references: vec![],
                    }))
                },
                query_micros: 0,
                queue_wait_micros: 0,
                total_request_micros: 0,
                row_count: 0,
            })
            .unwrap();
    }

    #[test]
    fn a_full_commit_waits_when_the_frame_budget_is_spent_before_it_starts() {
        // Native collector resets its clock on entry and offers no injected elapsed-time seam.
        // Successful and failed responses both spend the native commit budget.
        let config = EngineConfig {
            max_cell_commits_per_frame: 2,
            headless: true,
            ..default()
        };
        for failed_first in [false, true] {
            let (mut app, _directory, responses) = collector_app(config.clone());
            for cell in 1..=3 {
                enqueue_cell_response(
                    &mut app,
                    &responses,
                    CellKey::Interior(cell),
                    failed_first && cell == 1,
                );
            }
            {
                let mut state = SystemState::<ResMut<StreamingCommitBudget>>::new(app.world_mut());
                let mut budget = state.get_mut(app.world_mut()).unwrap();
                budget.remaining = 2;
                budget.commits = 0;
            }
            app.world_mut().run_system_once(collect_cells).unwrap();
            assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 2);
            assert!(matches!(
                app.world().resource::<StreamingWorld>().cells[&CellKey::Interior(3)],
                CellStatus::Loading { .. }
            ));
            if failed_first {
                assert_eq!(
                    app.world().resource::<StreamingMetrics>().failed_cells,
                    1,
                    "a failed commit also spends frame time/count"
                );
            }
        }

        // Time spending is checked through the real collector, including failed commits.
        // Zero makes the expired boundary deterministic without emulating its predicate.
        // The collector owns the elapsed clock; zero gives a deterministic expired boundary.
        for failed_first in [false, true] {
            let (mut app, _directory, responses) = collector_app(config.clone());
            install_commit_decision(&mut app, 0);
            enqueue_cell_response(&mut app, &responses, CellKey::Interior(1), failed_first);
            enqueue_cell_response(&mut app, &responses, CellKey::Interior(2), false);
            app.world_mut()
                .resource_mut::<StreamingCommitBudget>()
                .remaining = 2;
            app.world_mut().run_system_once(collect_cells).unwrap();
            assert_eq!(app.world().resource::<StreamingCommitBudget>().commits, 1);
            assert!(
                matches!(
                    app.world().resource::<StreamingWorld>().cells[&CellKey::Interior(2)],
                    CellStatus::Loading { .. }
                ),
                "a prior commit, including failure, spends the time budget"
            );
        }
    }

    #[test]
    fn the_first_commit_of_a_frame_is_never_held_by_the_time_budget() {
        let config = EngineConfig {
            max_commit_micros_per_frame: 0,
            headless: true,
            ..default()
        };
        let (mut app, _directory, responses) = collector_app(config);
        install_commit_decision(&mut app, 0);
        enqueue_cell_response(&mut app, &responses, CellKey::Interior(1), false);
        {
            let mut state = SystemState::<ResMut<StreamingCommitBudget>>::new(app.world_mut());
            let mut budget = state.get_mut(app.world_mut()).unwrap();
            budget.remaining = 1;
            budget.commits = 0;
            budget.frame_started = Instant::now() - Duration::from_micros(50_000);
        }
        assert!(
            !app.world()
                .resource::<StreamingCommitBudget>()
                .elapsed_limit_reached(Some(app.world().resource::<runtime::StreamingRuntime>())),
            "the first commit goes ahead despite elapsed time"
        );
        app.world_mut().run_system_once(collect_cells).unwrap();
        assert!(
            app.world()
                .resource::<StreamingWorld>()
                .is_resident(&CellKey::Interior(1)),
            "the first commit goes ahead despite elapsed time"
        );
        app.world_mut()
            .resource_mut::<EngineConfig>()
            .max_cell_commits_per_frame = 0;
        enqueue_cell_response(&mut app, &responses, CellKey::Interior(2), false);
        app.world_mut()
            .resource_mut::<StreamingCommitBudget>()
            .remaining = 0;
        app.world_mut().run_system_once(collect_cells).unwrap();
        assert!(matches!(
            app.world().resource::<StreamingWorld>().cells[&CellKey::Interior(2)],
            CellStatus::Loading { .. }
        ));
    }
    #[test]
    fn canonical_dimension_mismatch_is_refused_before_normal_recomputation() {
        let (mut app, directory, responses) = collector_app(EngineConfig {
            headless: true,
            max_cell_commits_per_frame: 1,
            ..default()
        });
        let path = directory.path().join("different-sized-land.rkyv");
        let cache = shared::CellCache {
            version: shared::CELL_CACHE_VERSION,
            cells: vec![shared::CachedLand {
                cell_id: 1,
                width: 65,
                height: 65,
                heights: vec![100.0; 65 * 65],
                normals: (0..65 * 65).flat_map(|_| [0, 0, 127]).collect(),
                vertex_colors: vec![],
                layers: vec![],
                water_height: None,
                water_type_form_id: None,
            }],
        };
        std::fs::write(
            &path,
            rkyv::to_bytes::<rkyv::rancor::Error>(&cache).unwrap(),
        )
        .unwrap();
        app.insert_resource(CellCache::open(&path).unwrap());
        app.insert_resource(crate::cluster::terrain::ClusterTerrain::for_heights_test(
            60,
            IVec2::ZERO,
            vec![0.0; 33 * 33],
        ));
        let key = CellKey::Exterior {
            worldspace_id: 60,
            grid_x: 0,
            grid_y: 0,
        };
        enqueue_cell_response(&mut app, &responses, key, false);
        app.world_mut()
            .resource_mut::<StreamingCommitBudget>()
            .remaining = 1;
        app.world_mut().run_system_once(collect_cells).unwrap();
        let streaming = app.world().resource::<StreamingWorld>();
        let Some(CellStatus::Failed { reason, .. }) = streaming.cells.get(&key) else {
            panic!("mismatched canonical grid must remain an explicit failed full-cell attempt");
        };
        assert!(
            reason.contains("canonical") && reason.contains("dimension"),
            "{reason}"
        );
        assert_eq!(
            app.world()
                .resource::<StreamingMetrics>()
                .terrain_validation_failures,
            1
        );
        assert!(streaming.outstanding_cells.is_empty());
        assert_eq!(
            app.world_mut()
                .query::<&StreamedCellRoot>()
                .iter(app.world())
                .count(),
            0
        );
    }
}
