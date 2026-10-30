//! Producer-level readiness regressions. No renderer, synthetic certificate or asset-state mock.
use super::*;
use bevy::{asset::AssetPlugin, ecs::system::RunSystemOnce};

fn app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_resource::<crate::config::EngineConfig>()
        .init_resource::<crate::graphics_settings::GraphicsSettings>()
        .init_asset::<WorldAsset>()
        .init_asset::<StandardMaterial>()
        .init_resource::<ClusterBatchHandles>()
        .init_resource::<crate::streaming::StreamingMetrics>()
        .insert_resource(ClusterScene {
            ready_revision: default(),
            residency_revision: default(),
            certificate: None,
            inventory: None,
            terrain_blocks: Vec::new(),
            meshes: Vec::new(),
            instances: Vec::new(),
            origin_offset: Vec3::ZERO,
            blocks: Vec::new(),
        });
    app
}

fn batch() -> LazyBatch {
    LazyBatch {
        primitive: 0,
        key: BatchKey {
            model_path: "invented-loader-fixture.glb".into(),
            material_index: None,
            scale_inverted: false,
        },
        spheres: vec![LazySphere {
            center: Vec3::ZERO,
            radius: 1.0,
            world: LazyWorld::Fixture,
        }],
        bounds: (Vec3::ZERO, 1.0),
        scene: None,
        source_reload_from: None,
        loading: None,
        requested: false,
        requested_at: 0.0,
        reported: false,
    }
}

fn install_batch(app: &mut App, batch: LazyBatch, in_flight: bool) {
    app.insert_resource(LazyBatches {
        layout_scope: next_layout_scope(),
        batches: vec![batch],
        native_by_model: HashMap::new(),
        scenes: HashMap::new(),
        retried_sources: default(),
        prefetch_in_flight: Vec::new(),
        unrequested: if in_flight { Vec::new() } else { vec![0] },
        in_flight: if in_flight { vec![0] } else { Vec::new() },
        failed: Vec::new(),
        scenes_revision: 0,
        failed_revision: 0,
    });
}

fn load(app: &mut App) {
    app.world_mut()
        .run_system_once(load_batches_in_view)
        .unwrap();
}

fn pending(app: &App) -> usize {
    app.world()
        .resource::<crate::streaming::StreamingMetrics>()
        .pending_cluster_batches
}

#[test]
fn completed_source_load_stays_pending_until_actual_world_asset_is_available() {
    let mut app = app();
    let source = app
        .world()
        .resource::<AssetServer>()
        .add(WorldAsset::new(World::new()));
    app.update();
    assert!(
        app.world()
            .resource::<AssetServer>()
            .is_loaded_with_dependencies(source.id())
    );
    // Exercise the actual publication boundary without inventing LoadState facts.
    // Keep the strong source handle and do not run the asset schedule while absent.
    let saved = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .remove(source.id())
        .unwrap();
    let mut wanted = batch();
    wanted.requested = true;
    wanted.scene = Some(source.clone());
    install_batch(&mut app, wanted, true);

    load(&mut app);
    assert!(matches!(
        app.world()
            .resource::<AssetServer>()
            .load_state(source.id()),
        bevy::asset::LoadState::Loaded
    ));
    assert_eq!(pending(&app), 1);
    let lazy = app.world().resource::<LazyBatches>();
    assert_eq!(lazy.in_flight, [0]);
    assert_eq!(lazy.batches[0].scene.as_ref().unwrap().id(), source.id());
    assert!(lazy.batches[0].loading.is_none());

    app.world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .insert(source.id(), saved)
        .unwrap();
    load(&mut app);
    assert_eq!(
        pending(&app),
        1,
        "material handoff is still a producer step"
    );
    assert!(
        app.world().resource::<LazyBatches>().batches[0]
            .scene
            .is_none()
    );
    assert!(
        app.world().resource::<LazyBatches>().batches[0]
            .loading
            .is_some()
    );
    load(&mut app);
    assert_eq!(pending(&app), 0);
    assert!(app.world().resource::<LazyBatches>().in_flight.is_empty());
}

#[test]
fn default_material_batch_requests_and_retains_the_shared_source_scene() {
    let mut app = app();
    let source = app
        .world()
        .resource::<AssetServer>()
        .add(WorldAsset::new(World::new()));
    app.update();
    let wanted = batch();
    let path = wanted.key.model_path.clone();
    install_batch(&mut app, wanted, false);
    // Reuse a real server-owned source, as when another stock placement requested
    // the model already. This fixture performs no file IO or GLB-loader emulation.
    app.world_mut()
        .resource_mut::<LazyBatches>()
        .scenes
        .insert(path.clone(), source.clone());
    app.world_mut().spawn((
        Camera3d::default(),
        Camera::default(),
        bevy::camera::primitives::Frustum::default(),
        GlobalTransform::IDENTITY,
    ));

    load(&mut app);
    assert_eq!(pending(&app), 1);
    let lazy = app.world().resource::<LazyBatches>();
    assert!(lazy.batches[0].requested);
    assert_eq!(lazy.in_flight, [0]);
    assert!(lazy.unrequested.is_empty());
    assert_eq!(lazy.batches[0].scene.as_ref().unwrap().id(), source.id());
    assert!(
        lazy.batches[0].loading.is_none(),
        "default material must not bypass source phase"
    );

    load(&mut app);
    load(&mut app);
    assert_eq!(pending(&app), 0);
    let lazy = app.world().resource::<LazyBatches>();
    assert!(lazy.batches[0].scene.is_none());
    assert_eq!(lazy.source_scene(0).unwrap().id(), source.id());
    assert_eq!(lazy.scenes[&path].id(), source.id());
}

#[test]
fn loaded_material_with_a_failed_texture_dependency_is_terminal_not_pending_or_ready() {
    let mut app = app();
    app.init_asset::<Image>();
    let server = app.world().resource::<AssetServer>();
    let texture = server.load::<Image>("cluster-readiness-missing-texture.png");
    let base = server.add(StandardMaterial {
        base_color_texture: Some(texture),
        ..default()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        app.update();
        let server = app.world().resource::<AssetServer>();
        if matches!(server.load_state(base.id()), bevy::asset::LoadState::Loaded)
            && asset_failed(server, base.id())
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "real failed texture dependency did not resolve"
        );
        std::thread::yield_now();
    }
    assert!(
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .contains(base.id())
    );
    let mut wanted = batch();
    wanted.requested = true;
    wanted.loading = Some(base);
    wanted.key.material_index = Some(0);
    install_batch(&mut app, wanted, true);
    app.world_mut().spawn((
        Camera3d::default(),
        Camera::default(),
        bevy::camera::primitives::Frustum::default(),
        GlobalTransform::IDENTITY,
    ));
    load(&mut app);
    assert_eq!(
        pending(&app),
        0,
        "a terminal dependency is not pending work"
    );
    assert_eq!(app.world().resource::<LazyBatches>().failed, [0]);
    assert_eq!(
        app.world()
            .resource::<crate::streaming::StreamingMetrics>()
            .failed_cluster_batches,
        1
    );
}

#[test]
fn speculative_source_capacity_survives_handoff_without_becoming_view_demand() {
    let mut app = app();
    install_batch(&mut app, batch(), false);
    // Real asset handles with no completed source publication represent in-flight sources.
    // Neither asset presence nor a ready flag is fabricated.
    let handles: Vec<_> = (0..MAX_LOADING)
        .map(|_| {
            app.world()
                .resource::<Assets<WorldAsset>>()
                .reserve_handle()
        })
        .collect();
    {
        let mut lazy = app.world_mut().resource_mut::<LazyBatches>();
        for (index, handle) in handles.iter().enumerate() {
            lazy.scenes
                .insert(format!("far-{index}.glb"), handle.clone());
        }
        lazy.prefetch_in_flight = handles;
    }
    app.add_systems(Update, load_batches_in_view);
    app.update();
    assert_eq!(
        pending(&app),
        0,
        "far speculation is not view readiness demand"
    );
    let camera = app
        .world_mut()
        .spawn((
            Camera3d::default(),
            Camera::default(),
            bevy::camera::primitives::Frustum::default(),
            GlobalTransform::IDENTITY,
        ))
        .id();
    for _ in 0..3 {
        app.update();
        assert_eq!(
            pending(&app),
            1,
            "unchanged cached views retain blocked wanted demand"
        );
    }
    assert_eq!(pending(&app), 1, "only the requested batch holds readiness");
    assert!(app.world().resource::<LazyBatches>().in_flight.is_empty());
    assert_eq!(
        app.world()
            .resource::<LazyBatches>()
            .prefetch_only_in_flight(),
        MAX_LOADING
    );
    // The wanted batch can reuse an outstanding source without counting it twice.
    app.world_mut().resource_mut::<LazyBatches>().batches[0]
        .key
        .model_path = "far-0.glb".into();
    app.world_mut()
        .entity_mut(camera)
        .insert(GlobalTransform::from_translation(Vec3::X));
    app.update();
    let lazy = app.world().resource::<LazyBatches>();
    assert_eq!(lazy.in_flight.len(), 1);
    assert_eq!(lazy.prefetch_only_in_flight(), MAX_LOADING - 1);
    assert_eq!(lazy.scenes.len(), MAX_LOADING);
    assert_eq!(pending(&app), 1);
}

// This headless glTF fixture has no PBR plugin. Declare the same external image as a
// scene dependency through the real LoadContext, retaining its handle in the template World.
#[derive(Resource)]
struct FixtureImageDependency {
    _image: Handle<Image>,
}
#[derive(Clone)]
struct FixtureSceneImageHandler;
impl bevy::gltf::extensions::GltfExtensionHandler for FixtureSceneImageHandler {
    fn dyn_clone(&self) -> Box<dyn bevy::gltf::extensions::ErasedGltfExtensionHandler> {
        Box::new(self.clone())
    }
    fn on_scene_completed(
        &mut self,
        context: &mut bevy::asset::LoadContext<'_>,
        _scene: &bevy::gltf::gltf::Scene<'_>,
        _root: Entity,
        world: &mut World,
    ) {
        world.insert_resource(FixtureImageDependency {
            _image: context.load("meshes/transient.png"),
        });
    }
}

#[test]
fn cached_scene_dependency_failure_retries_once_and_keeps_terminal_failure_explicit() {
    use serde_json::json;
    for repair in [false, true] {
        let directory = crate::cluster::reader::producer_fixture_tests::published_for_loader();
        let rows = read_rows(&crate::cluster::overlay_fixture::read_db(directory.path())).unwrap();
        let original = std::fs::read(
            directory
                .path()
                .join(&rows.values().next().unwrap().model_path),
        )
        .unwrap();
        let json_len = u32::from_le_bytes(original[12..16].try_into().unwrap()) as usize;
        let mut document: serde_json::Value =
            serde_json::from_slice(&original[20..20 + json_len]).unwrap();
        document["images"] = json!([{"uri": "transient.png"}]);
        document["textures"] = json!([{"source": 0}]);
        document["materials"] =
            json!([{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}]);
        let mut json_bytes = serde_json::to_vec(&document).unwrap();
        while !json_bytes.len().is_multiple_of(4) {
            json_bytes.push(b' ');
        }
        let binary_chunk = &original[20 + json_len..];
        let total = 20 + json_bytes.len() + binary_chunk.len();
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4e4f534au32.to_le_bytes());
        glb.extend_from_slice(&json_bytes);
        glb.extend_from_slice(binary_chunk);
        let path = "meshes/dependency-retry.glb";
        std::fs::write(directory.path().join(path), glb).unwrap();
        let mut app = crate::cluster::source_identity::actual_loader_tests::loader_app_with(
            directory.path(),
            |app| {
                app.world_mut()
                    .resource_mut::<bevy::gltf::extensions::GltfExtensionHandlers>()
                    .0
                    .write_blocking()
                    .push(Box::new(FixtureSceneImageHandler));
            },
        );
        app.init_asset::<StandardMaterial>()
            .register_asset_loader(bevy::image::ImageLoader::new(
                bevy::image::CompressedImageFormats::NONE,
            ))
            .init_resource::<ClusterBatchHandles>()
            .init_resource::<crate::streaming::StreamingMetrics>();
        let assets = app.world().resource::<AssetServer>().clone();
        // Retain this original consumer, so dropping the prefetch map cannot hide the bug via GC.
        let source: Handle<WorldAsset> = assets.load(GltfAssetLabel::Scene(0).from_asset(path));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !(matches!(
            assets.load_state(source.id()),
            bevy::asset::LoadState::Loaded
        ) && asset_failed(&assets, source.id())
            && app
                .world()
                .resource::<Assets<WorldAsset>>()
                .contains(source.id()))
        {
            app.update();
            assert!(
                std::time::Instant::now() < deadline,
                "expected Loaded scene + failed image: root={:?}, recursive={:?}, present={}",
                assets.load_state(source.id()),
                assets.recursive_dependency_load_state(source.id()),
                app.world()
                    .resource::<Assets<WorldAsset>>()
                    .contains(source.id())
            );
            std::thread::yield_now();
        }
        let previous = app
            .world()
            .resource::<Assets<WorldAsset>>()
            .get(source.id())
            .unwrap()
            .world
            .id();
        let fresh: Handle<WorldAsset> = assets.load(GltfAssetLabel::Scene(0).from_asset(path));
        assert_eq!(fresh.id(), source.id());
        assert!(
            asset_failed(&assets, fresh.id()),
            "plain load must reproduce Bevy's cached dependency failure"
        );
        if repair {
            // Generated 1x1 RGBA PNG: no external fixture or game data.
            std::fs::write(
                directory.path().join("meshes/transient.png"),
                [
                    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0,
                    0, 1, 8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156,
                    99, 248, 15, 4, 0, 9, 251, 3, 253, 251, 94, 107, 43, 0, 0, 0, 0, 73, 69, 78,
                    68, 174, 66, 96, 130,
                ],
            )
            .unwrap();
        }
        let mut wanted = batch();
        wanted.key.model_path = path.into();
        install_batch(&mut app, wanted, false);
        app.world_mut()
            .resource_mut::<LazyBatches>()
            .scenes
            .insert(path.into(), source.clone());
        app.insert_resource(ClusterScene {
            ready_revision: default(),
            residency_revision: default(),
            certificate: None,
            inventory: None,
            terrain_blocks: Vec::new(),
            meshes: Vec::new(),
            instances: Vec::new(),
            origin_offset: Vec3::ZERO,
            blocks: Vec::new(),
        })
        .add_systems(Update, load_batches_in_view);
        app.world_mut().spawn((
            Camera3d::default(),
            Camera::default(),
            bevy::camera::primitives::Frustum::default(),
            GlobalTransform::IDENTITY,
        ));
        // Loaded precedes publication. An unchanged lazy view must retry after publication,
        // rather than preserving its blocked candidate forever in ScanCache.
        let saved = app
            .world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .remove(source.id())
            .unwrap();
        app.update();
        assert!(!app.world().resource::<LazyBatches>().batches[0].requested);
        assert_eq!(pending(&app), 1);
        app.world_mut()
            .resource_mut::<Assets<WorldAsset>>()
            .insert(source.id(), saved)
            .unwrap();
        app.update();
        assert_eq!(
            app.world().resource::<LazyBatches>().batches[0].source_reload_from,
            Some(previous)
        );
        // The old failed result cannot terminate the new attempt before a new template publishes.
        load(&mut app);
        assert!(app.world().resource::<LazyBatches>().failed.is_empty());
        assert_eq!(pending(&app), 1);
        loop {
            app.update();
            let lazy = app.world().resource::<LazyBatches>();
            if (repair
                && assets.is_loaded_with_dependencies(source.id())
                && lazy.batches[0].scene.is_none())
                || (!repair && !lazy.failed.is_empty())
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "retry did not resolve, repair={repair}"
            );
            std::thread::yield_now();
        }
        let replacement = app
            .world()
            .resource::<Assets<WorldAsset>>()
            .get(source.id())
            .unwrap()
            .world
            .id();
        assert_ne!(replacement, previous);
        assert_eq!(
            app.world().resource::<LazyBatches>().failed.is_empty(),
            repair
        );
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(
            app.world()
                .resource::<Assets<WorldAsset>>()
                .get(source.id())
                .unwrap()
                .world
                .id(),
            replacement,
            "a terminal failure must not start an automatic reload loop"
        );
    }
}

#[test]
fn settled_loader_does_no_batch_or_failure_scan_work() {
    let mut app = app();
    app.add_message::<AppExit>();
    let mut done = batch();
    done.requested = true;
    install_batch(&mut app, done, false);
    app.world_mut()
        .resource_mut::<LazyBatches>()
        .unrequested
        .clear();
    app.add_systems(Update, load_batches_in_view);
    app.init_resource::<LazyScanWork>();
    app.update();
    assert_eq!(app.world().resource::<LazyScanWork>().0, 1);
    for _ in 0..10 {
        app.update();
    }
    assert_eq!(app.world().resource::<LazyScanWork>().0, 1);
    assert_eq!(pending(&app), 0);
    assert_eq!(
        app.world()
            .resource::<crate::streaming::StreamingMetrics>()
            .failed_cluster_batches,
        0
    );
}
