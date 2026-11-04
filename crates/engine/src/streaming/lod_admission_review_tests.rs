#[test]
fn repeated_nonempty_lod_response_preserves_unchanged_horizon_tick() {
    use crate::world::database::LodChunkResponse;
    use bevy::asset::{AssetApp, AssetPlugin};
    let (database, _, _) = WorldDatabase::channel_fixture(1);
    let config = EngineConfig::default();
    let mut horizon = LodHorizon::default();
    let reach = lod_horizon(&config.terrain_lod);
    horizon.set(60, reach);
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<WorldAsset>()
        .insert_resource(config)
        .insert_resource(database)
        .insert_resource(horizon)
        .insert_resource(RenderOrigin(IVec2::ZERO))
        .insert_resource(LodStreaming {
            center: Some(IVec2::ZERO),
            ..default()
        })
        .init_resource::<StreamingWorld>()
        .init_resource::<StreamingCommitBudget>()
        .init_resource::<StreamingMetrics>()
        .init_resource::<ProfilingState>()
        .add_systems(Update, collect_lod_chunks);
    app.update();
    let before = app
        .world()
        .get_resource_ref::<LodHorizon>()
        .unwrap()
        .last_changed();
    let mut metadata = retry_test_metadata();
    metadata.key.worldspace_id = 60;
    app.world_mut()
        .resource_mut::<WorldDatabase>()
        .enqueue_lod_response_for_test(LodChunkResponse {
            generation: 0,
            query: query_for(60, metadata.key.tier, IVec2::ZERO, 1),
            result: Ok(vec![metadata.clone()]),
            query_micros: 0,
            queue_wait_micros: 0,
            total_request_micros: 0,
            chunk_count: 1,
        });
    app.update();
    assert_eq!(
        app.world()
            .resource::<StreamingMetrics>()
            .lod_query_responses,
        1
    );
    assert_eq!(
        app.world().resource::<LodHorizon>().reach_in(60),
        Some(reach)
    );
    assert_eq!(
        app.world()
            .get_resource_ref::<LodHorizon>()
            .unwrap()
            .last_changed(),
        before,
        "equal reach is not a horizon change"
    );
    metadata.key.worldspace_id = 61;
    app.world_mut()
        .resource_mut::<WorldDatabase>()
        .enqueue_lod_response_for_test(LodChunkResponse {
            generation: 0,
            query: query_for(61, metadata.key.tier, IVec2::ZERO, 1),
            result: Ok(vec![metadata]),
            query_micros: 0,
            queue_wait_micros: 0,
            total_request_micros: 0,
            chunk_count: 1,
        });
    app.update();
    assert_eq!(
        app.world().resource::<LodHorizon>().reach_in(61),
        Some(reach)
    );
    assert_ne!(
        app.world()
            .get_resource_ref::<LodHorizon>()
            .unwrap()
            .last_changed(),
        before
    );
}
