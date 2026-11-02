//! Real loader/native-handler/scene-spawner coverage of the materials actually drawn.
use super::*;
use crate::{
    extensions::{
        DataSetExtensions,
        fixtures::{data_set, manifest},
        read_data_extensions,
    },
    nif_depth::NifDepthMaterial,
    nif_material::{
        EffectPaletteDepthMaterial, EffectPaletteMaterial, NifMaterialPlugin, NifSourceMaterial,
    },
    skin_material::{SkyrimSkinDepthMaterial, SkyrimSkinMaterial},
};
use bevy::{
    asset::{
        LoadContext, LoadState,
        io::{
            AssetSourceBuilder, AssetSourceId,
            memory::{Dir, MemoryAssetReader},
        },
    },
    gltf::{
        GltfMaterial,
        extensions::{ErasedGltfExtensionHandler, GltfExtensionHandler, GltfExtensionHandlers},
        gltf,
    },
    time::TimeUpdateStrategy,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

// PbrPlugin's private handler needs a renderer. Supply only its stock /std contract;
// all native material construction, draw replacement, spawning and playback are production code.
#[derive(Clone)]
struct StockHandler;
impl GltfExtensionHandler for StockHandler {
    fn dyn_clone(&self) -> Box<dyn ErasedGltfExtensionHandler> {
        Box::new(self.clone())
    }
    fn on_material(
        &mut self,
        context: &mut LoadContext<'_>,
        _: &gltf::Material<'_>,
        _: Handle<GltfMaterial>,
        loaded: &GltfMaterial,
        label: &str,
    ) {
        context.add_labeled_asset(
            format!("{label}/std"),
            StandardMaterial {
                uv_transform: loaded.uv_transform,
                base_color_texture: loaded.base_color_texture.clone(),
                alpha_mode: loaded.alpha_mode,
                ..default()
            },
        );
    }
    fn on_spawn_mesh_and_material(
        &mut self,
        context: &mut LoadContext<'_>,
        _: &gltf::Primitive<'_>,
        _: &gltf::Mesh<'_>,
        _: &gltf::Material<'_>,
        entity: &mut EntityWorldMut,
        label: &str,
    ) {
        entity.insert(MeshMaterial3d(
            context.get_label_handle::<StandardMaterial>(format!("{label}/std")),
        ));
    }
}

fn document(variant: &str) -> serde_json::Value {
    let palette = variant.starts_with("nif_palette");
    let skin = variant.starts_with("skin");
    let depth = variant.contains("depth");
    let mut material = serde_json::json!({
        "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}},
        "extras": {"openSkyrim": {"uvTransform": {"offset": [0.25, 0.5], "scale": [2.0, 3.0]}}},
        "extensions": {"OPEN_SKYRIM_material_animation": {"channels": [{
            "variable": "vOffset", "interpolation": "QUADRATIC", "times": [0.0, 2.0],
            "values": [0.0, 1.0], "tangents": [[0.0, 0.0], [0.0, 0.0]],
            "loop": "cycle", "frequency": 1.0, "phase": 0.0, "start": 0.0, "stop": 2.0
        }]}}
    });
    if variant == "std" {
        material.as_object_mut().unwrap().remove("extras");
    }
    if palette || skin {
        material["extensions"]["OPEN_SKYRIM_material"] = if palette {
            serde_json::json!({"shaderFamily": "effect", "shaderFlags1": 0x30,
                "textureSlots": [{"semantic": "greyscale", "texture": 0}]})
        } else {
            serde_json::json!({"lightingShaderType": "hair_tint", "tintColor": [0.5, 0.25, 1.0]})
        };
    }
    if depth {
        material["extras"]["openSkyrim"]["shaderFlags1"] =
            serde_json::json!(crate::nif_depth::DEPTH_TEST | crate::nif_depth::DECAL);
        material["extras"]["openSkyrim"]["shaderFlags2"] = serde_json::json!(0);
    }
    serde_json::json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["OPEN_SKYRIM_material_animation", "OPEN_SKYRIM_material"],
        "buffers": [{"uri": "triangle.bin", "byteLength": 96}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 24}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"}
        ],
        "images": [{"uri": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGP4DwQACfsD/fteaysAAAAASUVORK5CYII="}],
        "samplers": [{"wrapS": 33648, "wrapT": 10497}],
        "textures": [{"source": 0, "sampler": 0}],
        "materials": [material],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "material": 0}]}],
        "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0
    })
}

fn pump(app: &mut App, mut ready: impl FnMut(&mut App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        app.update();
        if ready(app) {
            return;
        }
        assert!(Instant::now() < deadline, "load/spawn/lifetime timed out");
        std::thread::yield_now();
    }
}
fn uv<M: AnimatedMaterial>(app: &App, handle: &Handle<M>) -> bevy::math::Affine2 {
    app.world()
        .resource::<Assets<M>>()
        .get(handle)
        .unwrap()
        .standard()
        .uv_transform
}
fn advance(app: &mut App, millis: u64) {
    *app.world_mut().resource_mut::<TimeUpdateStrategy>() =
        TimeUpdateStrategy::ManualDuration(Duration::from_millis(millis));
    app.update();
}

fn check_drawn<M: AnimatedMaterial>(variant: &str, enabled: bool) {
    let entries = if enabled {
        vec![(shared::extensions::MATERIAL, 1, "core")]
    } else {
        vec![]
    };
    let directory = data_set(&manifest(&entries), Some("core"), &[]);
    let files = Dir::default();
    let floats: [f32; 24] = [
        0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1., 0., 0., 1., 0., 0.,
        1.,
    ];
    files.insert_asset(
        Path::new("triangle.bin"),
        floats
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut source = document(variant);
    files.insert_asset_text(
        Path::new("animated.gltf"),
        &serde_json::to_string(&source).unwrap(),
    );
    // A distinct model has a distinct authored period and must not inherit the first payload.
    let mut other = source.clone();
    other["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"]["channels"][0]["times"] =
        serde_json::json!([0.0, 4.0]);
    other["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"]["channels"][0]["stop"] =
        serde_json::json!(4.0);
    files.insert_asset_text(
        Path::new("other.gltf"),
        &serde_json::to_string(&other).unwrap(),
    );
    let reader = MemoryAssetReader {
        root: files.clone(),
    };
    let mut app = App::new();
    app.register_asset_source(
        AssetSourceId::Default,
        AssetSourceBuilder::new(move || Box::new(reader.clone())),
    )
    .add_plugins((
        MinimalPlugins,
        AssetPlugin::default(),
        bevy::world_serialization::WorldSerializationPlugin,
        bevy::mesh::MeshPlugin,
        bevy::image::ImagePlugin::default(),
        bevy::gltf::GltfPlugin::default(),
    ))
    .init_asset::<StandardMaterial>()
    .register_asset_loader(bevy::image::ImageLoader::new(
        bevy::image::CompressedImageFormats::NONE,
    ))
    .register_type::<MeshMaterial3d<StandardMaterial>>()
    .register_type::<MeshMaterial3d<M>>()
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO))
    .insert_resource(DataSetExtensions::new(
        read_data_extensions(directory.path()).unwrap(),
    ));
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(Duration::from_secs(1));
    app.world_mut()
        .resource_mut::<GltfExtensionHandlers>()
        .0
        .write_blocking()
        .push(Box::new(StockHandler));
    crate::render::install_material_animation(&mut app);
    crate::render::register_skyrim_material_handler(&mut app);
    app.add_plugins(NifMaterialPlugin);
    app.finish();
    app.cleanup();
    let server = app.world().resource::<AssetServer>().clone();
    let root = server.load::<bevy::gltf::Gltf>("animated.gltf");
    let scene = server.load::<WorldAsset>("animated.gltf#Scene0");
    let other_scene = server.load::<WorldAsset>("other.gltf#Scene0");
    let mut parents: Vec<_> = [scene.clone(), scene.clone(), other_scene.clone()]
        .into_iter()
        .map(|scene| app.world_mut().spawn(WorldAssetRoot(scene)).id())
        .collect();
    pump(&mut app, |app| {
        for id in [scene.id(), other_scene.id()] {
            if let Some(LoadState::Failed(error)) = server.get_load_state(id) {
                panic!("{error}");
            }
        }
        server.is_loaded_with_dependencies(root.id())
            && server.is_loaded_with_dependencies(other_scene.id())
            && app
                .world_mut()
                .query::<&MeshMaterial3d<M>>()
                .iter(app.world())
                .count()
                == 3
    });
    let draws: Vec<_> = app
        .world_mut()
        .query::<(Entity, &MeshMaterial3d<M>)>()
        .iter(app.world())
        .map(|(entity, material)| (entity, material.0.clone()))
        .collect();
    let first = draws
        .iter()
        .find(|(_, h)| {
            server.get_path(h.id()).unwrap().to_string()
                == format!("animated.gltf#Material0/{variant}")
        })
        .unwrap()
        .1
        .clone();
    let second = draws
        .iter()
        .find(|(_, h)| {
            server
                .get_path(h.id())
                .unwrap()
                .to_string()
                .starts_with("other.gltf#")
        })
        .unwrap()
        .1
        .clone();
    assert_eq!(
        draws.iter().filter(|(_, h)| *h == first).count(),
        2,
        "same model instances share authored clock"
    );
    assert_ne!(first, second);
    let stock = app
        .world()
        .get::<NifSourceMaterial>(draws[0].0)
        .map(|s| s.0.clone());
    if variant != "std" && variant != "nif" {
        for (entity, _) in &draws {
            assert!(
                app.world()
                    .get::<MeshMaterial3d<StandardMaterial>>(*entity)
                    .is_none()
            );
        }
    }
    let base = uv(&app, &first);
    let image = app
        .world()
        .resource::<Assets<M>>()
        .get(&first)
        .unwrap()
        .standard()
        .base_color_texture
        .clone()
        .unwrap();
    let sampler = app
        .world()
        .resource::<Assets<Image>>()
        .get(&image)
        .unwrap()
        .sampler
        .clone();
    if enabled {
        assert!(
            app.world()
                .resource::<AnimatedMaterials<M>>()
                .animated
                .contains_key(&first.id())
        );
        assert!(
            app.world()
                .resource::<AnimatedMaterials<M>>()
                .animated
                .contains_key(&second.id())
        );
    }
    // Invisible assets hold their previous value, even with a running clock.
    advance(&mut app, 500);
    assert_eq!(uv(&app, &first), base);
    for (entity, _) in &draws {
        app.world_mut()
            .entity_mut(*entity)
            .insert(ViewVisibility::VISIBLE);
    }
    advance(&mut app, 500);
    let expected = MaterialAnimation::from_extensions(
        &source["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"],
        base,
    )
    .unwrap();
    let other_expected = MaterialAnimation::from_extensions(
        &other["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"],
        base,
    )
    .unwrap();
    assert_eq!(
        uv(&app, &first),
        if enabled {
            expected.transform_at(1.0)
        } else {
            base
        }
    );
    assert_eq!(
        uv(&app, &second),
        if enabled {
            other_expected.transform_at(1.0)
        } else {
            base
        }
    );
    if enabled {
        assert_ne!(uv(&app, &first), uv(&app, &second));
    }
    if let Some(stock) = stock {
        assert_eq!(
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .get(&stock)
                .unwrap()
                .uv_transform,
            bevy::math::Affine2::IDENTITY
        );
    }
    assert_eq!(
        app.world()
            .resource::<Assets<Image>>()
            .get(&image)
            .unwrap()
            .sampler,
        sampler
    );
    if !enabled {
        assert!(!app.world().contains_resource::<MaterialAnimationRegistry>());
        return;
    }
    // Real asset event feedback must settle: neither writes nor entity-index visits repeat.
    advance(&mut app, 0);
    advance(&mut app, 0);
    let visits = app.world().resource::<MaterialUsers<M>>().entity_visits;
    let mut cursor = app
        .world()
        .resource::<Messages<AssetEvent<M>>>()
        .get_cursor_current();
    for _ in 0..4 {
        advance(&mut app, 0);
        assert_eq!(
            cursor
                .read(app.world().resource::<Messages<AssetEvent<M>>>())
                .filter(|e| matches!(e, AssetEvent::Modified { .. }))
                .count(),
            0
        );
    }
    assert_eq!(
        app.world().resource::<MaterialUsers<M>>().entity_visits,
        visits
    );
    // One hidden instance does not pause its visible sibling; all hidden instances catch up.
    for (entity, h) in &draws {
        if *h == first {
            app.world_mut()
                .entity_mut(*entity)
                .insert(ViewVisibility::HIDDEN);
        }
    }
    advance(&mut app, 500);
    assert_eq!(uv(&app, &first), expected.transform_at(1.0));
    let visible = draws.iter().find(|(_, h)| *h == first).unwrap().0;
    app.world_mut()
        .entity_mut(visible)
        .insert(ViewVisibility::VISIBLE);
    advance(&mut app, 250);
    assert_eq!(uv(&app, &first), expected.transform_at(1.75));
    // Reload withdraws only this source, including labels that disappear while held alive.
    let registry = app.world().resource::<MaterialAnimationRegistry>().clone();
    source["materials"][0]["extensions"]
        .as_object_mut()
        .unwrap()
        .remove("OPEN_SKYRIM_material_animation");
    // A changed shader family also removes the old native draw label while its handle survives.
    if variant != "std" {
        source["materials"][0]
            .as_object_mut()
            .unwrap()
            .remove("extras");
        source["materials"][0]["extensions"]
            .as_object_mut()
            .unwrap()
            .remove("OPEN_SKYRIM_material");
    }
    files.insert_asset_text(
        Path::new("animated.gltf"),
        &serde_json::to_string(&source).unwrap(),
    );
    server.reload("animated.gltf");
    pump(&mut app, |app| {
        registry
            .get(&format!("animated.gltf#Material0/{variant}"))
            .is_none()
            && !app
                .world()
                .resource::<AnimatedMaterials<M>>()
                .animated
                .contains_key(&first.id())
    });
    assert!(
        app.world()
            .resource::<AnimatedMaterials<M>>()
            .animated
            .contains_key(&second.id())
    );
    // Reload back to the animated variant and spawn a fresh instance at the current clock.
    files.insert_asset_text(
        Path::new("animated.gltf"),
        &serde_json::to_string(&document(variant)).unwrap(),
    );
    server.reload("animated.gltf");
    pump(&mut app, |app| {
        registry
            .get(&format!("animated.gltf#Material0/{variant}"))
            .is_some()
            && app
                .world()
                .resource::<AnimatedMaterials<M>>()
                .animated
                .contains_key(&first.id())
    });
    parents.push(app.world_mut().spawn(WorldAssetRoot(scene.clone())).id());
    pump(&mut app, |app| {
        app.world_mut()
            .query::<&MeshMaterial3d<M>>()
            .iter(app.world())
            .any(|m| m.id() == first.id())
    });
    let reloaded: Vec<_> = app
        .world_mut()
        .query::<(Entity, &MeshMaterial3d<M>)>()
        .iter(app.world())
        .filter(|(_, m)| m.id() == first.id())
        .map(|(entity, _)| entity)
        .collect();
    for entity in reloaded {
        app.world_mut()
            .entity_mut(entity)
            .insert(ViewVisibility::VISIBLE);
    }
    advance(&mut app, 125);
    assert_eq!(
        uv(&app, &first),
        expected.transform_at(app.world().resource::<Time>().elapsed_secs())
    );
    // Last instance and handle release must clear typed animation, inverse users and clone edges.
    let ids = [first.id(), second.id()];
    for parent in parents {
        if let Ok(entity) = app.world_mut().get_entity_mut(parent) {
            entity.despawn();
        }
    }
    drop(draws);
    drop(first);
    drop(second);
    drop(scene);
    drop(other_scene);
    drop(root);
    pump(&mut app, |app| {
        ids.iter()
            .all(|id| !app.world().resource::<Assets<M>>().contains(*id))
    });
    let animated = app.world().resource::<AnimatedMaterials<M>>();
    for id in ids {
        assert!(!animated.animated.contains_key(&id) && !animated.resolved.contains(&id));
    }
    assert!(
        app.world()
            .resource::<MaterialUsers<M>>()
            .entities
            .is_empty()
    );
}

macro_rules! drawn_test {
    ($name:ident, $material:ty, $label:literal) => {
        #[test]
        fn $name() {
            for enabled in [false, true] {
                check_drawn::<$material>($label, enabled);
            }
        }
    };
}
drawn_test!(drawn_stock_lifecycle, StandardMaterial, "std");
drawn_test!(drawn_native_lifecycle, StandardMaterial, "nif");
drawn_test!(drawn_depth_lifecycle, NifDepthMaterial, "nif-depth");
drawn_test!(
    drawn_palette_lifecycle,
    EffectPaletteMaterial,
    "nif_palette"
);
drawn_test!(
    drawn_palette_depth_lifecycle,
    EffectPaletteDepthMaterial,
    "nif_palette_depth"
);
drawn_test!(drawn_skin_lifecycle, SkyrimSkinMaterial, "skin");
drawn_test!(
    drawn_skin_depth_lifecycle,
    SkyrimSkinDepthMaterial,
    "skin-depth"
);

#[test]
fn skin_tone_clones_keep_uv_base_and_release_ownership() {
    use crate::{
        actor_skin_tone::{ActorSkinTone, ActorSkinTonePlugin},
        skin_material::SkinTintSurface,
    };
    for late in [false, true] {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .add_plugins((MaterialAnimationPlugin, ActorSkinTonePlugin))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
        app.world_mut()
            .resource_mut::<Time<Virtual>>()
            .set_max_delta(Duration::from_secs(1));
        let base = bevy::math::Affine2::from_scale_angle_translation(
            Vec2::new(2.0, 3.0),
            0.3,
            Vec2::new(0.25, 0.5),
        );
        let source = app
            .world_mut()
            .resource_mut::<Assets<SkyrimSkinMaterial>>()
            .add(SkyrimSkinMaterial {
                base: StandardMaterial {
                    uv_transform: base,
                    ..default()
                },
                ..default()
            });
        app.update();
        let animation = MaterialAnimation::from_extensions(
            &document("skin")["materials"][0]["extensions"]["OPEN_SKYRIM_material_animation"],
            base,
        )
        .unwrap();
        let registry = MaterialAnimationRegistry::default();
        registry.insert("skin-fixture".into(), animation.clone());
        let register = |app: &mut App| {
            let material = app
                .world()
                .resource::<Assets<SkyrimSkinMaterial>>()
                .get(&source)
                .unwrap()
                .clone();
            app.world_mut()
                .resource_mut::<AnimatedMaterials<SkyrimSkinMaterial>>()
                .register(
                    source.id(),
                    Some("skin-fixture"),
                    Some(&material),
                    Some(&registry),
                );
        };
        if !late {
            register(&mut app);
        }
        let mut actors = Vec::new();
        let mut surfaces = Vec::new();
        for tone in [Vec3::X, Vec3::Y, Vec3::X] {
            let surface = app
                .world_mut()
                .spawn((
                    MeshMaterial3d(source.clone()),
                    SkinTintSurface,
                    ViewVisibility::VISIBLE,
                ))
                .id();
            actors.push(
                app.world_mut()
                    .spawn(ActorSkinTone(tone))
                    .add_child(surface)
                    .id(),
            );
            surfaces.push(surface);
        }
        app.update();
        if late {
            register(&mut app);
        }
        let variants: Vec<_> = surfaces
            .iter()
            .map(|&entity| {
                app.world()
                    .get::<MeshMaterial3d<SkyrimSkinMaterial>>(entity)
                    .unwrap()
                    .0
                    .clone()
            })
            .collect();
        assert_eq!(variants[0], variants[2]);
        assert_ne!(variants[0], variants[1]);
        assert_ne!(variants[0], source);
        for _ in 0..3 {
            advance(&mut app, 500);
            let seconds = app.world().resource::<Time>().elapsed_secs();
            for variant in &variants {
                assert_eq!(uv(&app, variant), animation.transform_at(seconds));
            }
            let materials = app.world().resource::<Assets<SkyrimSkinMaterial>>();
            assert_eq!(
                materials
                    .get(&variants[0])
                    .unwrap()
                    .extension
                    .settings
                    .tint_color,
                Vec3::X.extend(1.0)
            );
            assert_eq!(
                materials
                    .get(&variants[1])
                    .unwrap()
                    .extension
                    .settings
                    .tint_color,
                Vec3::Y.extend(1.0)
            );
        }
        registry.replace("skin-fixture", None);
        register(&mut app);
        let stopped = uv(&app, &variants[0]);
        advance(&mut app, 250);
        assert_eq!(uv(&app, &variants[0]), stopped);
        let ids: Vec<_> = variants.iter().map(Handle::id).collect();
        for actor in actors {
            app.world_mut().entity_mut(actor).despawn();
        }
        drop(variants);
        pump(&mut app, |app| {
            ids.iter().all(|id| {
                !app.world()
                    .resource::<Assets<SkyrimSkinMaterial>>()
                    .contains(*id)
            })
        });
        let index = app
            .world()
            .resource::<AnimatedMaterials<SkyrimSkinMaterial>>();
        for id in ids {
            assert!(!index.derived_sources.contains_key(&id));
        }
        assert!(index.derived_by_source.is_empty());
        assert!(
            app.world()
                .resource::<MaterialUsers<SkyrimSkinMaterial>>()
                .entities
                .is_empty()
        );
    }
}
