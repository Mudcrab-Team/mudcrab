//! Actual GLB loader/WorldAsset-spawner evidence for public animation source ownership.
//!
//! Historical test names retain traceability. No cluster geometry tag, path cache, elapsed frame
//! count or collected Gltf root certifies animation readiness. The completed scene inventory does.
use super::*;
use crate::cluster::source_identity::{
    SourceStaticModel,
    actual_loader_tests::{loader_app_with, wait_for_loaded},
};
use bevy::asset::AssetId;
use std::{
    fs,
    time::{Duration, Instant},
};
const PATH: &str = "invented-static.glb";
fn static_glb() -> Vec<u8> {
    let mut binary = Vec::new();
    for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        binary.extend(value.to_le_bytes());
    }
    let document = serde_json::json!({
        "asset":{"version":"2.0"},
        "buffers":[{"byteLength":binary.len()}],
        "bufferViews":[{"buffer":0,"byteLength":binary.len()}],
        "accessors":[{
            "bufferView":0,"componentType":5126,"count":3,"type":"VEC3",
            "min":[0,0,0],"max":[1,1,0]
        }],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "nodes":[{"mesh":0}],
        "scenes":[{"nodes":[0]}],"scene":0
    });
    let mut json = serde_json::to_vec(&document).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bytes = Vec::new();
    bytes.extend(b"glTF");
    bytes.extend(2_u32.to_le_bytes());
    bytes.extend((12_u32 + 8 + json.len() as u32 + 8 + binary.len() as u32).to_le_bytes());
    bytes.extend((json.len() as u32).to_le_bytes());
    bytes.extend(b"JSON");
    bytes.extend(json);
    bytes.extend((binary.len() as u32).to_le_bytes());
    bytes.extend(b"BIN\0");
    bytes.extend(binary);
    bytes
}

fn empty_glb() -> Vec<u8> {
    let mut json = serde_json::to_vec(&serde_json::json!({
        "asset": {"version": "2.0"},
        "nodes": [],
        "scenes": [{"nodes": []}],
        "scene": 0
    }))
    .unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bytes = Vec::new();
    bytes.extend(b"glTF");
    bytes.extend(2_u32.to_le_bytes());
    bytes.extend((12_u32 + 8 + json.len() as u32).to_le_bytes());
    bytes.extend((json.len() as u32).to_le_bytes());
    bytes.extend(b"JSON");
    bytes.extend(json);
    bytes
}

struct Loaded {
    app: App,
    // App/assets drop before tempfile cleanup, including on assertion failure.
    _directory: tempfile::TempDir,
    source: Handle<WorldAsset>,
    path: &'static str,
    meshes: Vec<AssetId<Mesh>>,
}

fn loaded_bytes(path: &'static str, bytes: Vec<u8>) -> Loaded {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join(path), bytes).unwrap();
    // Register the real extension BEFORE the real loader starts; injecting an inventory after
    // loading would erase precisely the evidence these regressions must retain.
    let mut app = loader_app_with(directory.path(), |app| {
        app.init_asset::<AnimationClip>()
            .init_asset::<AnimationGraph>()
            .add_plugins(ModelAnimationPlugin);
    });
    let root: Handle<Gltf> = app.world().resource::<AssetServer>().load(path);
    wait_for_loaded(&mut app, &root);
    let source = app
        .world()
        .resource::<Assets<Gltf>>()
        .get(&root)
        .unwrap()
        .scenes[0]
        .clone();
    let meshes = {
        let scenes = app.world().resource::<Assets<WorldAsset>>();
        let template = scenes.get(&source).unwrap();
        let mut inventories = template.world.try_query::<&SourceClips>().unwrap();
        assert_eq!(
            inventories.iter(&template.world).count(),
            1,
            "actual loader completed one inventory"
        );
        assert!(
            inventories
                .iter(&template.world)
                .next()
                .unwrap()
                .0
                .is_empty()
        );
        // A scene without meshes never registers `Mesh3d` in its world.
        template
            .world
            .try_query::<(Entity, &Mesh3d)>()
            .map(|mut query| {
                query
                    .iter(&template.world)
                    .map(|(entity, mesh)| {
                        assert_eq!(
                            template.world.get::<SourceStaticModel>(entity),
                            Some(&SourceStaticModel(true))
                        );
                        mesh.0.id()
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    drop(root);
    until(&mut app, |app| {
        app.world()
            .resource::<AssetServer>()
            .get_handle::<Gltf>(path)
            .is_none()
    });
    Loaded {
        app,
        _directory: directory,
        source,
        path,
        meshes,
    }
}
fn loaded() -> Loaded {
    loaded_bytes(PATH, static_glb())
}
fn until(app: &mut App, condition: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        app.update();
        if condition(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "actual loader/spawner observation timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn reference(loaded: &mut Loaded, id: u32) -> Entity {
    let entity = loaded
        .app
        .world_mut()
        .spawn((
            MeshHandle(loaded.path.into()),
            FormId(id),
            WorldAssetRoot(loaded.source.clone()),
        ))
        .id();
    until(&mut loaded.app, |app| {
        app.world().get::<SourceClipBinding>(entity).is_some()
    });
    entity
}
fn static_answer(app: &App, reference: Entity) -> Entity {
    let binding = app
        .world()
        .get::<SourceClipBinding>(reference)
        .expect("completed live scene inventory");
    assert!(binding.graph.is_none() && binding.idle.is_none() && binding.open.is_none());
    assert!(binding.players.is_empty());
    assert!(
        app.world()
            .get::<SourceClips>(binding.source)
            .unwrap()
            .0
            .is_empty()
    );
    binding.source
}
fn live_meshes(app: &mut App, reference: Entity) -> Vec<(Entity, AssetId<Mesh>)> {
    let world = app.world();
    let mut pending = vec![reference];
    let mut result = Vec::new();
    while let Some(entity) = pending.pop() {
        if let Some(mesh) = world.get::<Mesh3d>(entity) {
            result.push((entity, mesh.0.id()));
        }
        if let Some(children) = world.get::<Children>(entity) {
            pending.extend(children.iter());
        }
    }
    result
}
fn no_root(loaded: &Loaded) {
    assert!(
        loaded
            .app
            .world()
            .resource::<AssetServer>()
            .get_handle::<Gltf>(loaded.path)
            .is_none()
    );
}

#[test]
fn cached_no_clip_for_a_tagged_source_does_not_acquire_source_proof_ownership() {
    let mut loaded = loaded();
    let first = reference(&mut loaded, 1);
    let second = reference(&mut loaded, 2);
    let first_source = static_answer(&loaded.app, first);
    let second_source = static_answer(&loaded.app, second);
    assert_ne!(
        first_source, second_source,
        "each reference records its own actual instance, never a path-cache proof"
    );
    loaded
        .app
        .world_mut()
        .entity_mut(first_source)
        .remove::<SourceClips>();
    loaded.app.update();
    assert!(loaded.app.world().get::<SourceClipBinding>(first).is_none());
    assert_eq!(static_answer(&loaded.app, second), second_source);
    no_root(&loaded);
}

#[test]
fn actual_static_scene_resolves_immediately_without_root_reload_or_scene_mesh_replacement() {
    let mut loaded = loaded();
    assert!(
        !loaded.meshes.is_empty(),
        "fixture contains real loader meshes"
    );
    let first = reference(&mut loaded, 1);
    let second = reference(&mut loaded, 2);
    let sources = [
        static_answer(&loaded.app, first),
        static_answer(&loaded.app, second),
    ];
    let meshes = live_meshes(&mut loaded.app, first);
    assert!(!meshes.is_empty());
    loaded
        .app
        .world_mut()
        .resource_mut::<Messages<AssetEvent<WorldAsset>>>()
        .drain()
        .for_each(drop);
    loaded
        .app
        .world_mut()
        .resource_mut::<Messages<AssetEvent<Mesh>>>()
        .drain()
        .for_each(drop);
    for _ in 0..244 {
        loaded.app.update();
        no_root(&loaded);
        assert_eq!(static_answer(&loaded.app, first), sources[0]);
        assert_eq!(static_answer(&loaded.app, second), sources[1]);
        assert_eq!(
            live_meshes(&mut loaded.app, first),
            meshes,
            "same live mesh entities and handles"
        );
        for mesh in &loaded.meshes {
            assert!(
                loaded
                    .app
                    .world()
                    .resource::<Assets<Mesh>>()
                    .contains(*mesh)
            );
        }
        assert!(
            loaded
                .app
                .world_mut()
                .resource_mut::<Messages<AssetEvent<WorldAsset>>>()
                .drain()
                .all(|e| !matches!(
                    e,
                    AssetEvent::Added { .. }
                        | AssetEvent::Modified { .. }
                        | AssetEvent::Removed { .. }
                ))
        );
        assert!(
            loaded
                .app
                .world_mut()
                .resource_mut::<Messages<AssetEvent<Mesh>>>()
                .drain()
                .all(|e| !matches!(
                    e,
                    AssetEvent::Added { .. }
                        | AssetEvent::Modified { .. }
                        | AssetEvent::Removed { .. }
                ))
        );
    }
}

#[test]
fn empty_untagged_and_explicitly_nonstatic_source_scenes_never_certify_no_animation() {
    // Geometry eligibility is deliberately no longer animation proof. Keep the three historical
    // counterexamples, but remove the completed inventory: none may manufacture its replacement.
    for variant in 0..3 {
        let mut loaded = loaded();
        let reference = reference(&mut loaded, 1);
        let source = static_answer(&loaded.app, reference);
        loaded
            .app
            .world_mut()
            .entity_mut(source)
            .remove::<SourceClips>();
        let meshes = live_meshes(&mut loaded.app, reference);
        for (entity, _) in meshes {
            match variant {
                0 => {
                    loaded.app.world_mut().entity_mut(entity).remove::<Mesh3d>();
                }
                1 => {
                    loaded
                        .app
                        .world_mut()
                        .entity_mut(entity)
                        .remove::<SourceStaticModel>();
                }
                _ => {
                    loaded
                        .app
                        .world_mut()
                        .entity_mut(entity)
                        .insert(SourceStaticModel(false));
                }
            }
        }
        for _ in 0..244 {
            loaded.app.update();
        }
        assert!(
            loaded
                .app
                .world()
                .get::<SourceClipBinding>(reference)
                .is_none()
        );
        no_root(&loaded);
    }
}

#[test]
fn actual_source_modification_evicts_no_clip_and_reclaims_its_existing_reference() {
    let mut loaded = loaded();
    let entity = reference(&mut loaded, 1);
    let old_source = static_answer(&loaded.app, entity);
    let scene_root: Entity;
    {
        let mut scenes = loaded.app.world_mut().resource_mut::<Assets<WorldAsset>>();
        let mut template = scenes.get_mut(&loaded.source).unwrap();
        let ids: Vec<_> = template
            .world
            .query_filtered::<Entity, With<SourceClips>>()
            .iter(&template.world)
            .collect();
        assert_eq!(ids.len(), 1, "the loader marks exactly one scene root");
        for id in &ids {
            template.world.entity_mut(*id).remove::<SourceClips>();
        }
        scene_root = ids[0];
    } // Real Assets mutation emits Modified; the real spawner replaces the live instance.
    until(&mut loaded.app, |app| {
        app.world().get_entity(old_source).is_err()
            && app.world().get::<SourceClipBinding>(entity).is_none()
    });
    // The world-asset spawner debounces `Modified` events that arrive within a few frames of the
    // previous one (it assumes they are not a real change), so the second edit waits them out.
    for _ in 0..4 {
        loaded.app.update();
    }
    {
        let mut scenes = loaded.app.world_mut().resource_mut::<Assets<WorldAsset>>();
        let mut template = scenes.get_mut(&loaded.source).unwrap();
        template
            .world
            .entity_mut(scene_root)
            .insert(SourceClips::default());
    }
    until(&mut loaded.app, |app| {
        app.world().get::<SourceClipBinding>(entity).is_some()
    });
    assert_ne!(static_answer(&loaded.app, entity), old_source);
    no_root(&loaded);
}

#[test]
fn actual_source_collection_evicts_no_clip_even_after_asset_server_path_disappears() {
    let mut loaded = loaded();
    let entity = reference(&mut loaded, 1);
    let source_instance = static_answer(&loaded.app, entity);
    let source_id = loaded.source.id();
    // A live instance owns its inventory even after its template/root is collected. End its
    // lifetime explicitly; the resolver must release only this reference's old binding.
    loaded
        .app
        .world_mut()
        .entity_mut(entity)
        .remove::<WorldAssetRoot>();
    loaded.app.world_mut().entity_mut(source_instance).despawn();
    let source = std::mem::take(&mut loaded.source);
    drop(source);
    until(&mut loaded.app, |app| {
        app.world()
            .resource::<AssetServer>()
            .get_path(source_id)
            .is_none()
            && !app
                .world()
                .resource::<Assets<WorldAsset>>()
                .contains(source_id)
    });
    assert!(
        loaded
            .app
            .world()
            .get::<SourceClipBinding>(entity)
            .is_none()
    );
    no_root(&loaded);
}

#[test]
fn empty_scene_root_derived_no_clip_survives_source_edits_without_repeated_root_loads() {
    let mut loaded = loaded_bytes("invented-empty-trigger.glb", empty_glb());
    assert!(loaded.meshes.is_empty());
    let references = [reference(&mut loaded, 1), reference(&mut loaded, 2)];
    let old = references.map(|e| static_answer(&loaded.app, e));
    {
        let mut scenes = loaded.app.world_mut().resource_mut::<Assets<WorldAsset>>();
        let mut template = scenes.get_mut(&loaded.source).unwrap();
        let temporary = template.world.spawn_empty().id();
        template.world.despawn(temporary);
    }
    until(&mut loaded.app, |app| {
        references.iter().zip(old).all(|(reference, old)| {
            app.world()
                .get::<SourceClipBinding>(*reference)
                .is_some_and(|b| b.source != old)
        })
    });
    for _ in 0..244 {
        loaded.app.update();
        for reference in references {
            static_answer(&loaded.app, reference);
        }
        no_root(&loaded);
    }
}

// These fixtures exercise the extension hooks through GltfPlugin, not a synthetic inventory.
mod named_actual_loader {
    use super::*;
    use bevy::{
        animation::{AnimatedBy, AnimationPlugin, AnimationTargetId, graph::AnimationNodeType},
        time::TimeUpdateStrategy,
        world_serialization::{WorldInstance, WorldInstanceSpawner},
    };

    fn animated_glb(scale: f32) -> Vec<u8> {
        let mut binary = Vec::new();
        let mut views = Vec::new();
        let mut accessors = Vec::new();
        let mut animations = Vec::new();
        // Different durations and directions make swapped names or stale replacement clips visible.
        for (name, duration, start, end) in [
            ("Open", 2.0_f32, 0.0, 4.0),
            ("Close", 4.0, 4.0, 0.0),
            ("Idle", 6.0, 10.0, 22.0),
        ] {
            let time_view = views.len();
            views.push(serde_json::json!({
                "buffer":0,"byteOffset":binary.len(),"byteLength":8
            }));
            for value in [0.0_f32, duration] {
                binary.extend(value.to_le_bytes());
            }
            let output_view = views.len();
            views.push(serde_json::json!({
                "buffer":0,"byteOffset":binary.len(),"byteLength":24
            }));
            for value in [start * scale, 0.0, 0.0, end * scale, 0.0, 0.0] {
                binary.extend(value.to_le_bytes());
            }
            let input = accessors.len();
            accessors.push(serde_json::json!({
                "bufferView":time_view,"componentType":5126,"count":2,"type":"SCALAR",
                "min":[0.0],"max":[duration]
            }));
            let output = accessors.len();
            accessors.push(serde_json::json!({
                "bufferView":output_view,"componentType":5126,"count":2,"type":"VEC3"
            }));
            animations.push(serde_json::json!({
                "name":name,
                "samplers":[{"input":input,"output":output,"interpolation":"LINEAR"}],
                "channels":[{"sampler":0,"target":{"node":1,"path":"translation"}}]
            }));
        }
        let mut json = serde_json::to_vec(&serde_json::json!({
            "asset":{"version":"2.0"},
            "buffers":[{"byteLength":binary.len()}],
            "bufferViews":views,"accessors":accessors,"animations":animations,
            "nodes":[{"name":"Hinge","children":[1]},{"name":"Leaf"}],
            "scenes":[{"name":"Invented animated scene","nodes":[0]}],"scene":0
        }))
        .unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bytes = Vec::new();
        bytes.extend(b"glTF");
        bytes.extend(2_u32.to_le_bytes());
        bytes.extend((12_u32 + 8 + json.len() as u32 + 8 + binary.len() as u32).to_le_bytes());
        bytes.extend((json.len() as u32).to_le_bytes());
        bytes.extend(b"JSON");
        bytes.extend(json);
        bytes.extend((binary.len() as u32).to_le_bytes());
        bytes.extend(b"BIN\0");
        bytes.extend(binary);
        bytes
    }

    fn app(directory: &std::path::Path) -> App {
        loader_app_with(directory, |app| {
            app.add_plugins(AnimationPlugin)
                .register_type::<AnimationPlayer>()
                .register_type::<AnimationTargetId>()
                .register_type::<AnimatedBy>()
                // Async loading cannot advance the authored playback clock.
                .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO))
                .add_plugins(ModelAnimationPlugin);
        })
    }

    fn scene(app: &mut App, path: &'static str, animations: bool) -> Handle<WorldAsset> {
        let root: Handle<Gltf> = app
            .world()
            .resource::<AssetServer>()
            .load_builder()
            .with_settings(move |settings: &mut GltfLoaderSettings| {
                settings.load_animations = animations
            })
            .load(path);
        wait_for_loaded(app, &root);
        let gltfs = app.world().resource::<Assets<Gltf>>();
        let gltf = gltfs.get(&root).unwrap();
        let source = gltf.scenes[0].clone();
        let scenes = app.world().resource::<Assets<WorldAsset>>();
        let template = scenes.get(&source).unwrap();
        if animations {
            let mut inventories = template.world.try_query::<&SourceClips>().unwrap();
            assert_eq!(gltf.animations.len(), 3);
            assert_eq!(gltf.named_animations.len(), 3);
            let inventory = inventories.single(&template.world).unwrap();
            assert_eq!(
                inventory.0.len(),
                3,
                "on_scene_completed retained the named inventory"
            );
            for (name, duration) in [("Open", 2.0), ("Close", 4.0), ("Idle", 6.0)] {
                let clip = &inventory.0.iter().find(|(n, _)| n == name).unwrap().1;
                assert_eq!(
                    clip,
                    gltf.named_animations.get(name).unwrap(),
                    "on_animations_collected retained the actual labeled clip"
                );
                assert_eq!(
                    app.world()
                        .resource::<Assets<AnimationClip>>()
                        .get(clip)
                        .unwrap()
                        .duration(),
                    duration
                );
            }
        } else {
            assert!(gltf.animations.is_empty() && gltf.named_animations.is_empty());
            let count = template
                .world
                .try_query::<&SourceClips>()
                .map_or(0, |mut q| q.iter(&template.world).count());
            assert_eq!(
                count, 0,
                "disabled loading of actual animation content must not certify static"
            );
        }
        drop(root);
        until(app, |app| {
            app.world()
                .resource::<AssetServer>()
                .get_handle::<Gltf>(path)
                .is_none()
        });
        source
    }

    fn spawn(app: &mut App, source: &Handle<WorldAsset>, form: u32, door: bool) -> Entity {
        let reference = app
            .world_mut()
            .spawn((
                MeshHandle("invented-animated.glb".into()),
                FormId(form),
                WorldAssetRoot(source.clone()),
            ))
            .id();
        if door {
            app.world_mut().entity_mut(reference).insert(LoadDoor {
                ref_id: form,
                destination: crate::doors::DoorDestination {
                    destination_ref_id: form + 1,
                    interior_cell_id: Some(19),
                    worldspace_id: None,
                    arrival_position: [0.0; 3],
                    arrival_rotation: [0.0; 3],
                },
            });
        }
        reference
    }

    fn ready(app: &App, reference: Entity) -> bool {
        app.world()
            .get::<WorldInstance>(reference)
            .is_some_and(|instance| {
                app.world()
                    .resource::<WorldInstanceSpawner>()
                    .instance_is_ready(**instance)
            })
    }

    fn leaf(app: &App, reference: Entity) -> Entity {
        let mut pending = vec![reference];
        let mut leaves = Vec::new();
        while let Some(entity) = pending.pop() {
            if app
                .world()
                .get::<Name>(entity)
                .is_some_and(|n| n.as_str() == "Leaf")
            {
                leaves.push(entity);
            }
            if let Some(children) = app.world().get::<Children>(entity) {
                pending.extend(children.iter());
            }
        }
        assert_eq!(leaves.len(), 1);
        leaves[0]
    }

    fn binding(app: &App, reference: Entity) -> SourceClipBinding {
        let binding = app
            .world()
            .get::<SourceClipBinding>(reference)
            .unwrap()
            .clone();
        let inventory = app.world().get::<SourceClips>(binding.source).unwrap();
        assert_eq!(inventory.0.len(), 3);
        assert_eq!(binding.players.len(), 1);
        let player = binding.players[0];
        assert_eq!(
            app.world()
                .get::<AnimatedBy>(leaf(app, reference))
                .unwrap()
                .0,
            player,
            "scene instancing remapped the target to this instance's player"
        );
        assert_eq!(
            app.world().get::<AnimationGraphHandle>(player).unwrap().0,
            *binding.graph.as_ref().unwrap()
        );
        let graphs = app.world().resource::<Assets<AnimationGraph>>();
        let graph = graphs.get(binding.graph.as_ref().unwrap()).unwrap();
        for (name, node) in [("Open", binding.open), ("Idle", binding.idle.map(|i| i.0))] {
            if let Some(node) = node {
                let AnimationNodeType::Clip(handle) = &graph.graph[node].node_type else {
                    panic!("binding must select an authored clip");
                };
                assert_eq!(
                    handle,
                    &inventory.0.iter().find(|(n, _)| n == name).unwrap().1
                );
            }
        }
        binding
    }

    fn sample(
        app: &mut App,
        reference: Entity,
        node: AnimationNodeIndex,
        time: f32,
        expected_x: f32,
    ) {
        let player = app
            .world()
            .get::<SourceClipBinding>(reference)
            .unwrap()
            .players[0];
        {
            let mut playback = app.world_mut().get_mut::<AnimationPlayer>(player).unwrap();
            playback.stop_all();
            playback.play(node).seek_to(time).pause();
        }
        // Graph threading consumes asset events asynchronously with respect to binding insertion.
        // Wait for the actual curve outcome; never replace it with an elapsed-frame answer.
        until(app, |app| {
            (app.world()
                .get::<Transform>(leaf(app, reference))
                .unwrap()
                .translation
                - Vec3::new(expected_x, 0.0, 0.0))
            .length()
                < 0.0001
        });
        let actual = app
            .world()
            .get::<Transform>(leaf(app, reference))
            .unwrap()
            .translation;
        assert!(
            (actual - Vec3::new(expected_x, 0.0, 0.0)).length() < 0.0001,
            "named clip at {time}: expected x={expected_x}, got {actual:?}"
        );
    }

    fn directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("OWNER.txt"),
            "Real named animation loader test fixture.\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("invented-animated.glb"),
            animated_glb(1.0),
        )
        .unwrap();
        fs::write(directory.path().join("replacement.glb"), animated_glb(2.0)).unwrap();
        directory
    }

    #[test]
    fn actual_named_glb_binds_door_clips_and_plays_each_instances_authored_idle() {
        let directory = directory();
        let mut app = app(directory.path());
        let source = scene(&mut app, "invented-animated.glb", true);
        let first = spawn(&mut app, &source, 1, false);
        let second = spawn(&mut app, &source, 2, false);
        let door = spawn(&mut app, &source, 3, true);
        until(&mut app, |app| {
            [first, second, door]
                .iter()
                .all(|r| app.world().get::<SourceClipBinding>(*r).is_some())
        });
        until(&mut app, |app| {
            app.world()
                .get::<Transform>(leaf(app, first))
                .unwrap()
                .translation
                .x
                == 12.0
                && app
                    .world()
                    .get::<Transform>(leaf(app, second))
                    .unwrap()
                    .translation
                    .x
                    == 14.0
        });
        let first_binding = binding(&app, first);
        let second_binding = binding(&app, second);
        let door_binding = binding(&app, door);
        assert_ne!(first_binding.source, second_binding.source);
        assert_ne!(first_binding.players, second_binding.players);
        for (reference, binding, phase, x) in [
            (first, &first_binding, 1.0, 12.0),
            (second, &second_binding, 2.0, 14.0),
        ] {
            assert!(binding.open.is_none());
            let (node, duration) = binding.idle.unwrap();
            assert_eq!(duration, 6.0);
            let active = app
                .world()
                .get::<AnimationPlayer>(binding.players[0])
                .unwrap()
                .animation(node)
                .unwrap();
            assert_eq!(active.seek_time(), phase);
            assert_eq!(
                app.world()
                    .get::<Transform>(leaf(&app, reference))
                    .unwrap()
                    .translation,
                Vec3::new(x, 0.0, 0.0)
            );
        }
        assert!(door_binding.idle.is_none());
        assert!(
            app.world()
                .get::<AnimationPlayer>(door_binding.players[0])
                .unwrap()
                .playing_animations()
                .next()
                .is_none()
        );
        sample(&mut app, door, door_binding.open.unwrap(), 0.5, 1.0);
        // Close has no runtime playback consumer. Exercise its decoded source data
        // through this test-owned node without adding an unused production binding.
        let close = app
            .world()
            .get::<SourceClips>(door_binding.source)
            .unwrap()
            .0
            .iter()
            .find(|(name, _)| name == "Close")
            .unwrap()
            .1
            .clone();
        let close_node = {
            let mut graphs = app.world_mut().resource_mut::<Assets<AnimationGraph>>();
            let mut graph = graphs
                .get_mut(door_binding.graph.as_ref().unwrap())
                .unwrap();
            let root = graph.root;
            graph.add_clip(close, 1.0, root)
        };
        sample(&mut app, door, close_node, 1.0, 3.0);
        assert_eq!(
            app.world()
                .get::<Transform>(leaf(&app, first))
                .unwrap()
                .translation
                .x,
            12.0,
            "door playback cannot drive the other instance's target"
        );
    }

    #[test]
    fn actual_animated_scene_replacement_rebinds_inventory_and_retires_only_outgoing_instance() {
        let directory = directory();
        let mut app = app(directory.path());
        let source = scene(&mut app, "invented-animated.glb", true);
        let replacement = scene(&mut app, "replacement.glb", true);
        let reference = spawn(&mut app, &source, 1, false);
        let survivor = spawn(&mut app, &source, 2, false);
        until(&mut app, |app| {
            [reference, survivor]
                .iter()
                .all(|r| app.world().get::<SourceClipBinding>(*r).is_some())
        });
        let old = binding(&app, reference);
        let other = binding(&app, survivor);
        let old_leaf = leaf(&app, reference);
        let old_clip = app
            .world()
            .get::<SourceClips>(old.source)
            .unwrap()
            .0
            .iter()
            .find(|(n, _)| n == "Idle")
            .unwrap()
            .1
            .id();
        app.world_mut()
            .entity_mut(reference)
            .insert(WorldAssetRoot(replacement.clone()));
        until(&mut app, |app| {
            app.world().get_entity(old.source).is_err()
                && app
                    .world()
                    .get::<SourceClipBinding>(reference)
                    .is_some_and(|b| b.source != old.source)
        });
        until(&mut app, |app| {
            app.world()
                .get::<Transform>(leaf(app, reference))
                .unwrap()
                .translation
                .x
                == 24.0
        });
        let new = binding(&app, reference);
        assert!(app.world().get_entity(old_leaf).is_err());
        assert!(
            old.players
                .iter()
                .all(|p| app.world().get_entity(*p).is_err())
        );
        assert_ne!(new.graph, old.graph);
        let new_clip = app
            .world()
            .get::<SourceClips>(new.source)
            .unwrap()
            .0
            .iter()
            .find(|(n, _)| n == "Idle")
            .unwrap()
            .1
            .id();
        assert_ne!(
            new_clip, old_clip,
            "replacement came from a second actual loader inventory"
        );
        assert_eq!(binding(&app, survivor).source, other.source);
        assert_eq!(
            app.world()
                .get::<Transform>(leaf(&app, reference))
                .unwrap()
                .translation
                .x,
            24.0
        );
        sample(&mut app, reference, new.idle.unwrap().0, 3.0, 32.0);
        assert_eq!(
            app.world()
                .get::<Transform>(leaf(&app, survivor))
                .unwrap()
                .translation
                .x,
            14.0
        );
        // Dropping the root Gltf did not collect the clips retained by live scene inventories.
        assert!(
            app.world()
                .resource::<Assets<AnimationClip>>()
                .contains(old_clip)
        );
        assert!(
            app.world()
                .resource::<Assets<AnimationClip>>()
                .contains(new_clip)
        );
        let new_source = new.source;
        let new_players = new.players.clone();
        app.world_mut().entity_mut(reference).despawn();
        until(&mut app, |app| app.world().get_entity(new_source).is_err());
        assert!(
            new_players
                .iter()
                .all(|p| app.world().get_entity(*p).is_err())
        );
        assert_eq!(binding(&app, survivor).source, other.source);
        for path in ["invented-animated.glb", "replacement.glb"] {
            assert!(
                app.world()
                    .resource::<AssetServer>()
                    .get_handle::<Gltf>(path)
                    .is_none()
            );
        }
        let old_scene_id = source.id();
        let old_graph_id = old.graph.as_ref().unwrap().id();
        app.world_mut().entity_mut(survivor).despawn();
        drop(old);
        drop(other);
        drop(source);
        until(&mut app, |app| {
            !app.world()
                .resource::<Assets<WorldAsset>>()
                .contains(old_scene_id)
                && !app
                    .world()
                    .resource::<Assets<AnimationClip>>()
                    .contains(old_clip)
                && !app
                    .world()
                    .resource::<Assets<AnimationGraph>>()
                    .contains(old_graph_id)
        });
        assert!(
            app.world()
                .resource::<Assets<WorldAsset>>()
                .contains(replacement.id()),
            "collecting the old source cannot collect the retained replacement"
        );
    }

    #[test]
    fn actual_animated_glb_with_animation_loading_disabled_stays_unresolved_after_scene_completion()
    {
        let directory = directory();
        let bytes = fs::read(directory.path().join("invented-animated.glb")).unwrap();
        let authored = bevy::gltf::gltf::Gltf::from_slice(&bytes).unwrap();
        assert_eq!(
            authored
                .animations()
                .map(|a| a.name().unwrap())
                .collect::<Vec<_>>(),
            ["Open", "Close", "Idle"]
        );
        let mut app = app(directory.path());
        let source = scene(&mut app, "invented-animated.glb", false);
        let reference = spawn(&mut app, &source, 1, false);
        until(&mut app, |app| ready(app, reference));
        let target = leaf(&app, reference);
        assert!(app.world().get::<AnimatedBy>(target).is_none());
        // Repeated resolver calls after proven scene completion cannot turn absence into static proof.
        for _ in 0..8 {
            app.update();
            assert!(ready(&app, reference));
            assert!(app.world().get::<SourceClipBinding>(reference).is_none());
            assert!(app.world().get::<IdleStarted>(reference).is_none());
            let mut pending = vec![reference];
            while let Some(entity) = pending.pop() {
                assert!(app.world().get::<SourceClips>(entity).is_none());
                assert!(app.world().get::<AnimationPlayer>(entity).is_none());
                if let Some(children) = app.world().get::<Children>(entity) {
                    pending.extend(children.iter());
                }
            }
        }
        assert!(
            app.world()
                .resource::<AssetServer>()
                .get_handle::<Gltf>("invented-animated.glb")
                .is_none()
        );
    }
}
