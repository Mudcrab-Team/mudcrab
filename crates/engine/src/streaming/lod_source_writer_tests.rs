//! The baked collector owns its chunk identity; ordinary near tags never belong here.
use super::*;
use crate::streaming::TerrainWorldspace;
use crate::world::components::StreamedCellKey;
use bevy::{
    asset::{AssetApp, AssetPlugin},
    ecs::system::RunSystemOnce,
};

#[test]
fn baked_collector_writes_chunk_identity_without_near_world_tag() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lod.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE schema_info(version INTEGER NOT NULL);
         INSERT INTO schema_info VALUES({});",
            shared::WORLD_DATABASE_SCHEMA_VERSION,
        ))
        .unwrap();
    drop(connection);
    let keys = [0x1234_5678, 0x8765_4321].map(|worldspace_id| {
        ChunkKey::new(
            worldspace_id,
            LodTier::Tier4,
            shared::lod::ChunkAnchor::new(0, 0),
        )
    });
    for key in keys {
        let metadata = LodChunkMetadata {
            key,
            payload_path: "worlds/test/lod/tier4_0_0.glb".to_owned(),
            content_hash: "not-read-until-the-scene-is-dispatched".to_owned(),
            bounds: crate::world::database::LodChunkBounds {
                min: [0.0; 3],
                max: [1.0; 3],
            },
            source_cells: vec![[0, 0]],
            build_identity: "source-writer-test".to_owned(),
            origin: LodOrigin::new(0, 0),
        };
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<WorldAsset>()
            .insert_resource(EngineConfig {
                // Queue the request without dispatching IO; no admission API or
                // sparse owner is installed in this ordinary collector test.
                max_scene_loads: 1,
                ..default()
            })
            .insert_resource(WorldDatabase::open(&path).unwrap())
            .insert_resource(RenderOrigin(IVec2::ZERO))
            .insert_resource(LodStreaming {
                generation: 7,
                center: Some(IVec2::ZERO),
                worldspace_id: Some(key.worldspace_id),
                pending_chunks: VecDeque::from([(7, metadata, None)]),
                ..default()
            })
            .insert_resource(StreamingCommitBudget {
                remaining: 1,
                ..default()
            })
            .init_resource::<StreamingWorld>()
            .init_resource::<StreamingMetrics>()
            .init_resource::<ProfilingState>();
        app.world_mut().run_system_once(collect_lod_chunks).unwrap();
        let world = app.world_mut();
        let roots: Vec<_> = world
            .query::<(Entity, &LodChunkRoot)>()
            .iter(world)
            .map(|(entity, root)| (entity, root.key))
            .collect();
        assert_eq!(
            roots.len(),
            1,
            "inspect real collector output, never a tagged fixture"
        );
        let (root, actual_key) = roots[0];
        assert_eq!(actual_key, key);
        assert!(world.get::<PendingLodChunk>(root).is_some());
        assert_eq!(world.resource::<StreamingMetrics>().lod_chunks_requested, 1);
        assert!(world.get::<StreamedCellKey>(root).is_none());
        assert_eq!(world.query::<&TerrainWorldspace>().iter(world).count(), 0);
        assert_eq!(
            world.query::<&TerrainCoverage>().iter(world).count(),
            0,
            "tier coverage is attached by readiness, not by the ordinary near loader"
        );
    }
}
