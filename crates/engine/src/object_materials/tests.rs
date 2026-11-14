use super::*;
use crate::nif_material::skin_load_tests::{loader_app_with_formats, wait_for_dependencies};
use bevy::{
    gltf::GltfAssetLabel,
    image::CompressedImageFormats,
    world_serialization::{WorldAsset, WorldAssetRoot},
};
use converter::{
    esm::{
        exporter::{create_tables, export_to_db},
        records::RawRecord,
    },
    mesh::MeshConverter,
    texture::{TextureConverter, TextureEncoding},
};
use dummy_content::{dds, nif, rng::Rng};
use std::{fs, path::Path};

fn record(id: u32, tag: &[u8; 4], fields: &[(&[u8; 4], Vec<u8>)]) -> RawRecord {
    RawRecord {
        form_id: id,
        record_type: *tag,
        flags: 0,
        subrecords: fields
            .iter()
            .map(|(tag, bytes)| (tag.to_vec(), bytes.clone()))
            .collect(),
        cell_form_id: None,
        worldspace_form_id: None,
        load_order: 0,
    }
}

fn swap(name: &str, texture: u32, index: u32) -> Vec<u8> {
    [
        1u32.to_le_bytes().as_slice(),
        &(name.len() as u32).to_le_bytes(),
        name.as_bytes(),
        &texture.to_le_bytes(),
        &index.to_le_bytes(),
    ]
    .concat()
}

fn base(id: u32, texture: Option<u32>) -> RawRecord {
    let mut fields = vec![(b"MODL" as &[u8; 4], b"meshes/shared.nif\0".to_vec())];
    if let Some(texture) = texture {
        fields.push((b"MODS", swap("Rock", texture, 0)));
    }
    record(id, b"STAT", &fields)
}

fn texture(id: u32, name: &str) -> RawRecord {
    record(
        id,
        b"TXST",
        &[(b"TX00", format!("textures/{name}.dds\0").into_bytes())],
    )
}

fn database() -> (Connection, HashMap<u32, RawRecord>) {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    let records = [
        base(1, Some(101)),
        base(2, Some(102)),
        base(3, None),
        base(4, Some(0xDEAD)),
        texture(101, "red"),
        texture(102, "blue"),
        texture(103, "green"),
    ]
    .into_iter()
    .map(|record| (record.form_id, record))
    .collect();
    export_to_db(&conn, &records).unwrap();
    (conn, records)
}

fn fixture(directory: &Path) {
    fs::create_dir_all(directory.join("meshes")).unwrap();
    fs::create_dir_all(directory.join("textures")).unwrap();
    let bytes = nif::static_shape_with_material_flags(
        &nif::StaticShape {
            name: "Rock",
            positions: &[[0., 0., 0.], [100., 0., 0.], [0., 100., 0.]],
            normals: &[[0., 0., 1.]; 3],
            uvs: &[[0., 0.], [1., 0.], [0., 1.]],
            indices: &[[0, 1, 2]],
            diffuse: "textures/original.dds",
            normal_texture: "",
        },
        (1 << 31) | (1 << 9),
        1,
    ) // Depth test, cast shadows, depth write: an ordinary surface.
    .unwrap();
    let source = directory.join("shared.nif");
    fs::write(&source, bytes).unwrap();
    MeshConverter::convert_nif_to_glb(&source, &directory.join("meshes/shared.glb")).unwrap();
    for (name, seed) in [("red", 1), ("blue", 2), ("green", 3), ("original", 4)] {
        let bytes = dds::generate(
            &dds::Spec::new(dds::Format::Bc1Unorm, 4, 4),
            &mut Rng::new(seed),
        )
        .unwrap();
        let bytes =
            TextureConverter::convert_uncompressed(&bytes, TextureEncoding::ColorSrgb).unwrap();
        fs::write(directory.join(format!("textures/{name}.ktx2")), bytes).unwrap();
    }
    for (name, spec) in [
        (
            "cube",
            dds::Spec::new(dds::Format::Bc1Unorm, 4, 4).as_cubemap(),
        ),
        ("non_colour", dds::Spec::new(dds::Format::Bc5Unorm, 4, 4)),
    ] {
        let bytes = dds::generate(&spec, &mut Rng::new(5)).unwrap();
        let bytes =
            TextureConverter::convert_uncompressed(&bytes, TextureEncoding::DataLinear).unwrap();
        fs::write(directory.join(format!("textures/{name}.ktx2")), bytes).unwrap();
    }
    // Normal publication supplies byte-identical colour/wrap aliases. Honor the actual URIs
    // emitted above rather than leaving the headless scene with an unresolved dependency.
    let original = fs::read(directory.join("textures/original.ktx2")).unwrap();
    for uri in MeshConverter::glb_texture_uris(&directory.join("meshes/shared.glb")).unwrap() {
        assert!(
            uri.starts_with("../textures/original."),
            "unexpected fixture dependency {uri}"
        );
        let target = directory.join("meshes").join(uri);
        if !target.exists() {
            fs::write(target, &original).unwrap();
        }
    }
}

fn spawn(
    app: &mut App,
    scene: &Handle<WorldAsset>,
    input: Option<ReferenceTextureSwaps>,
) -> Entity {
    let mut entity = app.world_mut().spawn((
        WorldAssetRoot(scene.clone()),
        Transform::default(),
        Visibility::default(),
    ));
    if let Some(input) = input {
        entity.insert(input);
    }
    entity.id()
}

fn surface(app: &App, root: Entity) -> (Handle<Mesh>, Handle<StandardMaterial>) {
    let mut pending = vec![root];
    while let Some(entity) = pending.pop() {
        if let Some(binding) = app.world().get::<MeshMaterial3d<StandardMaterial>>(entity) {
            return (
                app.world().get::<Mesh3d>(entity).unwrap().0.clone(),
                binding.0.clone(),
            );
        }
        if let Some(children) = app.world().get::<Children>(entity) {
            pending.extend(children.iter());
        }
    }
    panic!("fixture scene not instantiated");
}

fn settle(app: &mut App, roots: &[Entity]) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.update();
        if roots.iter().all(|root| {
            app.world()
                .get::<ObjectTextureSwapState>(*root)
                .is_some_and(|state| !matches!(state, ObjectTextureSwapState::Pending))
        }) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "material application timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn image_data(app: &App, material: &Handle<StandardMaterial>) -> Vec<u8> {
    let material = app
        .world()
        .resource::<Assets<StandardMaterial>>()
        .get(material)
        .unwrap();
    let image = app
        .world()
        .resource::<Assets<Image>>()
        .get(material.base_color_texture.as_ref().unwrap())
        .unwrap();
    assert!(image.texture_descriptor.format.is_srgb());
    image.data.clone().unwrap()
}

fn object_app(directory: &Path) -> App {
    let mut app = loader_app_with_formats(directory, CompressedImageFormats::BC);
    // Core scene components are normally registered by DefaultPlugins. This headless app loads
    // and instantiates the real glTF scene without installing a renderer or a native window.
    app.register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<Name>()
        .register_type::<Transform>()
        .register_type::<GlobalTransform>()
        .register_type::<Visibility>()
        .register_type::<InheritedVisibility>()
        .register_type::<ViewVisibility>()
        .register_type::<bevy::camera::primitives::Aabb>()
        .register_type::<Mesh3d>()
        .register_type::<MeshMaterial3d<StandardMaterial>>()
        .register_type::<bevy::gltf::GltfExtras>()
        .register_type::<bevy::gltf::GltfMeshExtras>()
        .register_type::<bevy::gltf::GltfMaterialExtras>()
        .register_type::<bevy::gltf::GltfMeshName>()
        .register_type::<bevy::gltf::GltfMaterialName>();
    // The loader helper has finished its plugins; invoke this resource/system-only build hook.
    ObjectMaterialPlugin.build(&mut app);
    app
}

#[test]
fn object_diffuse_published_stat_swaps_reach_drawn_materials_without_changing_the_shared_mesh() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let original_glb = fs::read(directory.path().join("meshes/shared.glb")).unwrap();
    let (conn, mut records) = database();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    let mut app = object_app(directory.path());
    let awaiting_scene = app.world_mut().spawn(catalog.get(1).unwrap()).id();
    for _ in 0..3 {
        app.update();
    }
    assert!(
        app.world()
            .get::<PendingTextureSwap>(awaiting_scene)
            .is_none(),
        "scene loading/failure must not create an endless appearance poll"
    );
    assert!(
        app.world()
            .get::<ObjectTextureSwapState>(awaiting_scene)
            .is_none()
    );
    let scene: Handle<WorldAsset> = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset("meshes/shared.glb"));
    wait_for_dependencies(&mut app, &scene);
    let red = spawn(&mut app, &scene, catalog.get(1));
    let blue = spawn(&mut app, &scene, catalog.get(2));
    let control = spawn(&mut app, &scene, catalog.get(3));
    let missing = spawn(&mut app, &scene, catalog.get(4));
    let red_again = spawn(&mut app, &scene, catalog.get(1));
    settle(&mut app, &[red, blue, missing, red_again]);
    let (mesh, red_material) = surface(&app, red);
    let (blue_mesh, blue_material) = surface(&app, blue);
    let (control_mesh, control_material) = surface(&app, control);
    assert_eq!(mesh, blue_mesh);
    assert_eq!(mesh, control_mesh);
    assert_ne!(red_material, blue_material);
    assert_eq!(
        surface(&app, red_again).1,
        red_material,
        "identical inputs share the derived material"
    );
    assert_eq!(surface(&app, missing).1, control_material);
    assert_ne!(
        image_data(&app, &red_material),
        image_data(&app, &blue_material)
    );
    assert!(
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .get(&control_material)
            .unwrap()
            .base_color_texture
            .is_some()
    );
    let material_assets = app.world().resource::<Assets<StandardMaterial>>();
    let source = material_assets.get(&control_material).unwrap();
    let derived = material_assets.get(&red_material).unwrap();
    assert_eq!(source.uv_transform, derived.uv_transform);
    assert_eq!(source.alpha_mode, derived.alpha_mode);
    assert_eq!(source.normal_map_texture, derived.normal_map_texture);
    assert_eq!(source.perceptual_roughness, derived.perceptual_roughness);
    let images = app.world().resource::<Assets<Image>>();
    assert_eq!(
        images
            .get(source.base_color_texture.as_ref().unwrap())
            .unwrap()
            .sampler,
        images
            .get(derived.base_color_texture.as_ref().unwrap())
            .unwrap()
            .sampler
    );
    assert_eq!(
        app.world().get::<ObjectTextureSwapState>(red),
        Some(&ObjectTextureSwapState::Applied { texture_set: 101 })
    );
    assert!(matches!(
        app.world().get::<ObjectTextureSwapState>(missing),
        Some(ObjectTextureSwapState::Unsupported(_))
    ));

    // Intervention: only base 1's authored TXST link changes. Re-export, then stream a fresh
    // placement; base 2 and the model's own material remain controls.
    records.insert(1, base(1, Some(103)));
    export_to_db(&conn, &records).unwrap();
    let updated = ObjectTextureCatalog::read(&conn).unwrap();
    let green = spawn(&mut app, &scene, updated.get(1));
    settle(&mut app, &[green]);
    let (green_mesh, green_material) = surface(&app, green);
    assert_eq!(mesh, green_mesh);
    assert_ne!(
        image_data(&app, &green_material),
        image_data(&app, &red_material)
    );
    assert_eq!(surface(&app, blue).1, blue_material);
    assert_eq!(surface(&app, control).1, control_material);
    assert_eq!(
        fs::read(directory.path().join("meshes/shared.glb")).unwrap(),
        original_glb
    );

    app.world_mut().despawn(red);
    app.world_mut().despawn(red_again);
    for _ in 0..4 {
        app.update();
    }
    let reloaded = spawn(&mut app, &scene, updated.get(1));
    settle(&mut app, &[reloaded]);
    assert_eq!(surface(&app, reloaded).1, green_material);
}

#[test]
fn object_diffuse_partial_unload_preserves_live_shared_material_and_reuse() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (connection, _) = database();
    let catalog = ObjectTextureCatalog::read(&connection).unwrap();
    let mut app = object_app(directory.path());
    let scene: Handle<WorldAsset> = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset("meshes/shared.glb"));
    wait_for_dependencies(&mut app, &scene);
    let first = spawn(&mut app, &scene, catalog.get(1));
    let survivor = spawn(&mut app, &scene, catalog.get(1));
    settle(&mut app, &[first, survivor]);
    // IDs cannot accidentally keep the shared assets alive after the first root unloads.
    let material_id = surface(&app, first).1.id();
    assert_eq!(surface(&app, survivor).1.id(), material_id);
    let image_id = app
        .world()
        .resource::<Assets<StandardMaterial>>()
        .get(material_id)
        .unwrap()
        .base_color_texture
        .as_ref()
        .unwrap()
        .id();
    let request_id = app.world().get::<RequestedDiffuse>(first).unwrap().0.id();
    app.world_mut().despawn(first);
    for _ in 0..8 {
        app.update();
    }
    assert_eq!(surface(&app, survivor).1.id(), material_id);
    assert!(
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .contains(material_id)
    );
    let images = app.world().resource::<Assets<Image>>();
    assert!(images.contains(image_id));
    assert!(images.contains(request_id));
    assert_eq!(app.world().resource::<DiffuseMaterials>().0.len(), 1);
    assert_eq!(
        app.world().get::<ObjectTextureSwapState>(survivor),
        Some(&ObjectTextureSwapState::Applied { texture_set: 101 })
    );

    let next = spawn(&mut app, &scene, catalog.get(1));
    settle(&mut app, &[next]);
    assert_eq!(surface(&app, next).1.id(), material_id);
    assert_eq!(surface(&app, survivor).1.id(), material_id);
    assert_eq!(app.world().resource::<DiffuseMaterials>().0.len(), 1);
}

#[test]
fn object_diffuse_last_unload_releases_requests_materials_images_and_cache_entries() {
    use bevy::ecs::message::MessageCursor;
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (connection, _) = database();
    let catalog = ObjectTextureCatalog::read(&connection).unwrap();
    let mut app = object_app(directory.path());
    let scene: Handle<WorldAsset> = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset("meshes/shared.glb"));
    wait_for_dependencies(&mut app, &scene);
    let roots = [
        spawn(&mut app, &scene, catalog.get(1)),
        spawn(&mut app, &scene, catalog.get(1)),
    ];
    settle(&mut app, &roots);
    // Retain IDs only: a strong handle in this test would keep the asset alive itself.
    let material_id = surface(&app, roots[0]).1.id();
    assert_eq!(surface(&app, roots[1]).1.id(), material_id);
    let image_id = app
        .world()
        .resource::<Assets<StandardMaterial>>()
        .get(material_id)
        .unwrap()
        .base_color_texture
        .as_ref()
        .unwrap()
        .id();
    let request_id = app
        .world()
        .get::<RequestedDiffuse>(roots[0])
        .unwrap()
        .0
        .id();
    assert_eq!(app.world().resource::<DiffuseMaterials>().0.len(), 1);
    for root in roots {
        app.world_mut().despawn(root);
    }
    let mut material_events = MessageCursor::<AssetEvent<StandardMaterial>>::default();
    let mut image_events = MessageCursor::<AssetEvent<Image>>::default();
    let (mut material_removed, mut image_removed, mut request_removed) = (false, false, false);
    for _ in 0..128 {
        app.update();
        material_removed |= material_events
            .read(
                app.world()
                    .resource::<Messages<AssetEvent<StandardMaterial>>>(),
            )
            .any(|event| matches!(event, AssetEvent::Removed { id } if *id == material_id));
        for event in image_events.read(app.world().resource::<Messages<AssetEvent<Image>>>()) {
            if let AssetEvent::Removed { id } = event {
                image_removed |= *id == image_id;
                request_removed |= *id == request_id;
            }
        }
        if material_removed
            && image_removed
            && request_removed
            && app.world().resource::<DiffuseMaterials>().0.is_empty()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        material_removed,
        "derived material did not emit Removed after last unload"
    );
    assert!(
        image_removed,
        "derived image did not emit Removed after last unload"
    );
    assert!(
        request_removed,
        "root-owned source image request survived last unload"
    );
    assert!(
        !app.world()
            .resource::<Assets<StandardMaterial>>()
            .contains(material_id)
    );
    let images = app.world().resource::<Assets<Image>>();
    assert!(!images.contains(image_id));
    assert!(!images.contains(request_id));
    assert!(app.world().resource::<DiffuseMaterials>().0.is_empty());

    let reloaded = spawn(&mut app, &scene, catalog.get(1));
    settle(&mut app, &[reloaded]);
    assert_eq!(
        app.world().get::<ObjectTextureSwapState>(reloaded),
        Some(&ObjectTextureSwapState::Applied { texture_set: 101 })
    );
    let reloaded_material = surface(&app, reloaded).1;
    assert_ne!(reloaded_material.id(), material_id);
    assert!(
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .contains(reloaded_material.id())
    );
}

#[test]
fn object_diffuse_unsupported_identity_missing_image_and_profile_keep_the_model_material() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let bytes = fs::read(directory.path().join("meshes/shared.glb")).unwrap();
    for (name, duplicate) in [("old", false), ("duplicate", true)] {
        let mut container = gltf::binary::Glb::from_slice(&bytes).unwrap();
        let mut document: serde_json::Value = serde_json::from_slice(&container.json).unwrap();
        if duplicate {
            let nodes = document["nodes"].as_array_mut().unwrap();
            let index = nodes.len();
            nodes.push(serde_json::json!({"mesh":0}));
            document["scenes"][0]["nodes"]
                .as_array_mut()
                .unwrap()
                .push(index.into());
        } else {
            document["meshes"][0]["primitives"][0]["extras"]
                .as_object_mut()
                .unwrap()
                .remove("mudcrabObjectDiffuseV1");
        }
        container.json = std::borrow::Cow::Owned(serde_json::to_vec(&document).unwrap());
        fs::write(
            directory.path().join(format!("meshes/{name}.glb")),
            container.to_vec().unwrap(),
        )
        .unwrap();
    }
    let (conn, mut records) = database();
    records.insert(
        5,
        record(5, b"STAT", &[(b"MODS", swap("WrongName", 101, 0))]),
    );
    records.insert(6, record(6, b"STAT", &[(b"MODS", swap("Rock", 101, 1))]));
    records.insert(7, base(7, Some(104)));
    records.insert(104, texture(104, "missing_file"));
    records.insert(8, base(8, Some(105)));
    records.insert(105, texture(105, "cube"));
    records.insert(9, base(9, Some(106)));
    records.insert(106, texture(106, "non_colour"));
    export_to_db(&conn, &records).unwrap();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    let mut app = object_app(directory.path());
    let scene: Handle<WorldAsset> = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset("meshes/shared.glb"));
    wait_for_dependencies(&mut app, &scene);
    let control = spawn(&mut app, &scene, None);
    let name = spawn(&mut app, &scene, catalog.get(5));
    let index = spawn(&mut app, &scene, catalog.get(6));
    let missing = spawn(&mut app, &scene, catalog.get(7));
    let cube = spawn(&mut app, &scene, catalog.get(8));
    let non_colour = spawn(&mut app, &scene, catalog.get(9));
    settle(&mut app, &[name, index, missing, cube, non_colour]);
    for root in [name, index, missing, cube, non_colour] {
        assert_eq!(surface(&app, root).1, surface(&app, control).1);
    }
    assert!(matches!(
        app.world().get::<ObjectTextureSwapState>(name),
        Some(ObjectTextureSwapState::Unsupported(_))
    ));
    assert!(matches!(
        app.world().get::<ObjectTextureSwapState>(index),
        Some(ObjectTextureSwapState::Unsupported(_))
    ));
    assert!(matches!(
        app.world().get::<ObjectTextureSwapState>(missing),
        Some(ObjectTextureSwapState::Failed(_))
    ));
    for root in [cube, non_colour] {
        assert!(matches!(
            app.world().get::<ObjectTextureSwapState>(root),
            Some(ObjectTextureSwapState::Unsupported(_))
        ));
    }
    app.insert_resource(crate::visual_profile::VisualProfile::Skyrim);
    let raw = spawn(&mut app, &scene, catalog.get(1));
    settle(&mut app, &[raw]);
    assert_eq!(surface(&app, raw).1, surface(&app, control).1);
    assert!(matches!(
        app.world().get::<ObjectTextureSwapState>(raw),
        Some(ObjectTextureSwapState::Unsupported(_))
    ));
    app.insert_resource(crate::visual_profile::VisualProfile::Mudcrab);
    for name in ["old", "duplicate"] {
        let alternate_scene: Handle<WorldAsset> = app
            .world()
            .resource::<AssetServer>()
            .load(GltfAssetLabel::Scene(0).from_asset(format!("meshes/{name}.glb")));
        wait_for_dependencies(&mut app, &alternate_scene);
        let root = spawn(&mut app, &alternate_scene, catalog.get(1));
        settle(&mut app, &[root]);
        assert!(
            matches!(
                app.world().get::<ObjectTextureSwapState>(root),
                Some(ObjectTextureSwapState::Unsupported(_))
            ),
            "{name}"
        );
    }
}

#[test]
fn object_diffuse_optional_projection_rejects_missing_links_and_preserves_other_txst_consumers() {
    let (conn, mut records) = database();
    records.insert(
        101,
        record(
            101,
            b"TXST",
            &[
                (b"TX00", b"red.dds\0".to_vec()),
                (b"TX07", b"specular.dds\0".to_vec()),
            ],
        ),
    );
    records.insert(102, record(102, b"MISC", &[]));
    export_to_db(&conn, &records).unwrap();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    assert_eq!(
        catalog.get(1).unwrap().swaps[0].disposition,
        "unsupported_texture_slots"
    );
    assert_eq!(
        catalog.get(2).unwrap().swaps[0].disposition,
        "missing_texture_set"
    );
    let path: String = conn
        .query_row(
            "SELECT specular_path FROM texture_sets WHERE id=101",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(path, "specular.dds");
    records.insert(1, base(1, None));
    export_to_db(&conn, &records).unwrap();
    assert!(ObjectTextureCatalog::read(&conn).unwrap().get(1).is_none());
    conn.execute_batch("DROP TABLE base_texture_swaps").unwrap();
    assert!(
        ObjectTextureCatalog::read(&conn).unwrap().get(2).is_none(),
        "old packs remain compatible"
    );
}
