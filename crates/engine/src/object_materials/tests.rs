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

/// A MODS list: (3D Name, TXST, 3D Index) per entry, as the retail record stores it.
fn mods(list: &[(&str, u32, i32)]) -> Vec<u8> {
    let mut data = (list.len() as u32).to_le_bytes().to_vec();
    for (name, texture, index) in list {
        data.extend_from_slice(&(name.len() as u32).to_le_bytes());
        data.extend_from_slice(name.as_bytes());
        data.extend_from_slice(&texture.to_le_bytes());
        data.extend_from_slice(&index.to_le_bytes());
    }
    data
}

fn stat(id: u32, list: &[(&str, u32, i32)]) -> RawRecord {
    record(
        id,
        b"STAT",
        &[
            (b"MODL", b"meshes/shared.nif\0".to_vec()),
            (b"MODS", mods(list)),
        ],
    )
}

/// A TXST with a diffuse and, when named, a normal map: the shape of every retail entry.
fn txst(id: u32, diffuse: &str, normal: Option<&str>) -> RawRecord {
    let mut fields = vec![(b"TX00" as &[u8; 4], format!("{diffuse}\0").into_bytes())];
    if let Some(normal) = normal {
        fields.push((b"TX01", format!("{normal}\0").into_bytes()));
    }
    record(id, b"TXST", &fields)
}

/// Retail STAT FormIDs and entries of the registered pack: 101534, 126810 and 126829 each carry
/// one entry `Name:0` at 3D Index 0 whose TXST fills TX00 and TX01 (RF-TEX-004).
const PINE: u32 = 101534;
const DIRT: u32 = 126810;
const ROCKS: u32 = 126829;
const NULL_TEXTURE_SET: u32 = 0x28;

fn database() -> (Connection, HashMap<u32, RawRecord>) {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    let records = [
        txst(101, "red.dds", Some("red_n.dds")),
        txst(102, "blue.dds", None),
        txst(103, "green.dds", Some("green_n.dds")),
        txst(
            201,
            r"Landscape\PineForest02.dds",
            Some(r"Landscape\PineForest02_N.dds"),
        ),
        txst(
            202,
            r"Landscape\Dirt02.dds",
            Some(r"Landscape\Dirt02_n.dds"),
        ),
        txst(
            203,
            r"Landscape\Rocks01.dds",
            Some(r"Landscape\Rocks01_n.dds"),
        ),
        stat(PINE, &[("RockPileS04:0", 201, 0)]),
        stat(DIRT, &[("RockCliff07:0", 202, 0)]),
        stat(ROCKS, &[("RockCliff05:0", 203, 0)]),
        stat(1, &[("Rock:0", 101, 0)]),
        stat(2, &[("Rock:0", 102, 0)]),
        record(3, b"STAT", &[(b"MODL", b"meshes/shared.nif\0".to_vec())]),
        stat(4, &[("Rock:0", 0xDEAD, 0)]),
    ]
    .into_iter()
    .map(|record| (record.form_id, record))
    .collect();
    export_to_db(&conn, &records).unwrap();
    (conn, records)
}

const TEXTURES: [(&str, u64); 7] = [
    ("red", 1),
    ("blue", 2),
    ("green", 3),
    ("original", 4),
    ("landscape/pineforest02", 6),
    ("landscape/dirt02", 7),
    ("landscape/rocks01", 8),
];

const ROCK: nif::StaticShape<'static> = nif::StaticShape {
    name: "Rock",
    positions: &[[0., 0., 0.], [100., 0., 0.], [0., 100., 0.]],
    normals: &[[0., 0., 1.]; 3],
    uvs: &[[0., 0.], [1., 0.], [0., 1.]],
    indices: &[[0, 1, 2]],
    diffuse: "textures/original.dds",
    normal_texture: "textures/original_n.dds",
};

fn fixture(directory: &Path) {
    fs::create_dir_all(directory.join("meshes")).unwrap();
    fs::create_dir_all(directory.join("textures/landscape")).unwrap();
    // Depth test, cast shadows, depth write: an ordinary surface.
    let bytes = nif::static_shape_with_material_flags(&ROCK, (1 << 31) | (1 << 9), 1).unwrap();
    let source = directory.join("shared.nif");
    fs::write(&source, bytes).unwrap();
    MeshConverter::convert_nif_to_glb(&source, &directory.join("meshes/shared.glb")).unwrap();
    for (name, seed) in TEXTURES {
        let bytes = dds::generate(
            &dds::Spec::new(dds::Format::Bc1Unorm, 4, 4),
            &mut Rng::new(seed),
        )
        .unwrap();
        let bytes =
            TextureConverter::convert_uncompressed(&bytes, TextureEncoding::ColorSrgb).unwrap();
        fs::write(directory.join(format!("textures/{name}.ktx2")), bytes).unwrap();
        let normal = dds::generate(
            &dds::Spec::new(dds::Format::Bc5Unorm, 4, 4),
            &mut Rng::new(seed + 100),
        )
        .unwrap();
        let normal =
            TextureConverter::convert_uncompressed(&normal, TextureEncoding::NormalLinear).unwrap();
        fs::write(directory.join(format!("textures/{name}_n.ktx2")), normal).unwrap();
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
    alias_texture_dependencies(directory, &directory.join("meshes/shared.glb"));
    multi_geometry_glb(directory);
}

/// Normal publication supplies byte-identical colour/wrap aliases. Honor the actual URIs a
/// converted fixture GLB emits rather than leaving the headless scene with an unresolved
/// dependency.
fn alias_texture_dependencies(directory: &Path, glb: &Path) {
    for uri in MeshConverter::glb_texture_uris(glb).unwrap() {
        assert!(
            uri.starts_with("../textures/original"),
            "unexpected fixture dependency {uri}"
        );
        let target = directory.join("meshes").join(&uri);
        if !target.exists() {
            let stem = uri
                .rsplit('/')
                .next()
                .and_then(|file| file.split('.').next())
                .unwrap();
            let canonical = fs::read(directory.join(format!("textures/{stem}.ktx2"))).unwrap();
            fs::write(target, canonical).unwrap();
        }
    }
}

/// `meshes/multi.glb`: four primitives that stand for source geometry 0 to 3. Geometry 2 stands
/// for a shape the converter does not tag (RF-TEX-003): it counts in the index but has no tag.
fn multi_geometry_glb(directory: &Path) {
    let bytes = fs::read(directory.join("meshes/shared.glb")).unwrap();
    let mut container = gltf::binary::Glb::from_slice(&bytes).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&container.json).unwrap();
    let meshes = document["meshes"].as_array_mut().unwrap();
    for index in 1..4 {
        let copy = meshes[0].clone();
        meshes.push(copy);
        let last = meshes.len() - 1;
        meshes[last]["primitives"][0]["extras"]["mudcrabGeometryIndex"] = index.into();
    }
    meshes[2]["primitives"][0]["extras"]
        .as_object_mut()
        .unwrap()
        .remove("mudcrabGeometryIndex");
    let nodes = document["nodes"].as_array_mut().unwrap();
    let mut added = Vec::new();
    for mesh in 1..4 {
        added.push(nodes.len());
        nodes.push(serde_json::json!({"mesh": mesh}));
    }
    let scene_nodes = document["scenes"][0]["nodes"].as_array_mut().unwrap();
    scene_nodes.extend(added.into_iter().map(serde_json::Value::from));
    container.json = std::borrow::Cow::Owned(serde_json::to_vec(&document).unwrap());
    fs::write(
        directory.join("meshes/multi.glb"),
        container.to_vec().unwrap(),
    )
    .unwrap();
}

fn load_scene(app: &mut App, glb: &str) -> Handle<WorldAsset> {
    let scene: Handle<WorldAsset> = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset(format!("meshes/{glb}")));
    wait_for_dependencies(app, &scene);
    scene
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

/// (geometry index tag, mesh, material, visibility) of one drawn primitive.
type DrawnSurface = (
    Option<u32>,
    Handle<Mesh>,
    Handle<StandardMaterial>,
    Visibility,
);

/// Every drawn primitive under `root`: (geometry index tag, mesh, material, visibility), ordered
/// by tag with the untagged primitive last.
fn surfaces_of(app: &App, root: Entity) -> Vec<DrawnSurface> {
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(entity) = pending.pop() {
        if let Some(binding) = app.world().get::<MeshMaterial3d<StandardMaterial>>(entity) {
            found.push((
                app.world()
                    .get::<ObjectGeometryIndex>(entity)
                    .map(|tag| tag.0),
                app.world().get::<Mesh3d>(entity).unwrap().0.clone(),
                binding.0.clone(),
                app.world()
                    .get::<Visibility>(entity)
                    .copied()
                    .unwrap_or_default(),
            ));
        }
        if let Some(children) = app.world().get::<Children>(entity) {
            pending.extend(children.iter());
        }
    }
    found.sort_by_key(|(tag, ..)| tag.unwrap_or(u32::MAX));
    found
}

fn surface(app: &App, root: Entity) -> (Handle<Mesh>, Handle<StandardMaterial>) {
    let surfaces = surfaces_of(app, root);
    assert!(!surfaces.is_empty(), "fixture scene not instantiated");
    (surfaces[0].1.clone(), surfaces[0].2.clone())
}

fn material_at(app: &App, root: Entity, tag: Option<u32>) -> Handle<StandardMaterial> {
    surfaces_of(app, root)
        .into_iter()
        .find(|surface| surface.0 == tag)
        .unwrap()
        .2
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

fn material_of<'a>(app: &'a App, handle: &Handle<StandardMaterial>) -> &'a StandardMaterial {
    app.world()
        .resource::<Assets<StandardMaterial>>()
        .get(handle)
        .unwrap()
}

fn image_data(app: &App, material: &Handle<StandardMaterial>) -> Vec<u8> {
    let image = app
        .world()
        .resource::<Assets<Image>>()
        .get(
            material_of(app, material)
                .base_color_texture
                .as_ref()
                .unwrap(),
        )
        .unwrap();
    assert!(image.texture_descriptor.format.is_srgb());
    image.data.clone().unwrap()
}

fn normal_data(app: &App, material: &Handle<StandardMaterial>) -> Vec<u8> {
    let image = app
        .world()
        .resource::<Assets<Image>>()
        .get(
            material_of(app, material)
                .normal_map_texture
                .as_ref()
                .unwrap(),
        )
        .unwrap();
    assert!(!image.texture_descriptor.format.is_srgb());
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
fn retail_stat_examples_replace_the_whole_texture_set_per_instance() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let original_glb = fs::read(directory.path().join("meshes/shared.glb")).unwrap();
    let (conn, _) = database();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    let mut app = object_app(directory.path());
    let awaiting_scene = app.world_mut().spawn(catalog.get(PINE).unwrap()).id();
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
    let scene = load_scene(&mut app, "shared.glb");
    let pine = spawn(&mut app, &scene, catalog.get(PINE));
    let dirt = spawn(&mut app, &scene, catalog.get(DIRT));
    let rocks = spawn(&mut app, &scene, catalog.get(ROCKS));
    let pine_again = spawn(&mut app, &scene, catalog.get(PINE));
    let control = spawn(&mut app, &scene, catalog.get(3));
    settle(&mut app, &[pine, dirt, rocks, pine_again]);
    let (mesh, pine_material) = surface(&app, pine);
    let (_, dirt_material) = surface(&app, dirt);
    let (_, rocks_material) = surface(&app, rocks);
    let (control_mesh, control_material) = surface(&app, control);
    assert_eq!(mesh, control_mesh, "the shared mesh is never specialized");
    assert_eq!(surface(&app, pine_again).1, pine_material);
    // RF-TEX-004: diffuse and normal both come from the TXST.
    for (a, b) in [
        (&pine_material, &dirt_material),
        (&pine_material, &rocks_material),
        (&dirt_material, &rocks_material),
    ] {
        assert_ne!(a, b);
        assert_ne!(image_data(&app, a), image_data(&app, b));
        assert_ne!(normal_data(&app, a), normal_data(&app, b));
    }
    assert_ne!(
        normal_data(&app, &pine_material),
        normal_data(&app, &control_material),
        "the model's own normal map is replaced"
    );
    let source = material_of(&app, &control_material);
    let derived = material_of(&app, &pine_material);
    assert_eq!(source.uv_transform, derived.uv_transform);
    assert_eq!(source.alpha_mode, derived.alpha_mode);
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
        app.world().get::<ObjectTextureSwapState>(pine),
        Some(&ObjectTextureSwapState::Applied {
            textured: 1,
            hidden: 0
        })
    );
    assert!(
        app.world().get::<ObjectTextureSwapState>(control).is_none(),
        "a base without MODS has no swap state"
    );
    assert_eq!(
        fs::read(directory.path().join("meshes/shared.glb")).unwrap(),
        original_glb
    );
}

/// #723: the converter tags a model whose NIF carries Havok collision, so its MODS entry applies
/// to the shape at that 3D Index like any other model's.
#[test]
fn a_model_with_havok_collision_receives_its_swap() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let source = directory.path().join("collidable.nif");
    let bytes = nif::static_shape_with_bodies(&ROCK, &[nif::fixed_box_body("RockCollision")]);
    fs::write(&source, bytes.unwrap()).unwrap();
    let glb = directory.path().join("meshes/collidable.glb");
    MeshConverter::convert_nif_to_glb(&source, &glb).unwrap();
    alias_texture_dependencies(directory.path(), &glb);
    let (conn, _) = database();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    let mut app = object_app(directory.path());
    let scene = load_scene(&mut app, "collidable.glb");
    let swapped = spawn(&mut app, &scene, catalog.get(1));
    let control = spawn(&mut app, &scene, None);
    settle(&mut app, &[swapped]);
    assert_eq!(
        app.world().get::<ObjectTextureSwapState>(swapped),
        Some(&ObjectTextureSwapState::Applied {
            textured: 1,
            hidden: 0
        })
    );
    assert_eq!(
        surfaces_of(&app, swapped)[0].0,
        Some(0),
        "3D Index 0 is tagged"
    );
    let (_, swapped_material) = surface(&app, swapped);
    let (_, control_material) = surface(&app, control);
    assert_ne!(swapped_material, control_material);
    assert_ne!(
        image_data(&app, &swapped_material),
        image_data(&app, &control_material),
        "the TXST diffuse replaces the model's own"
    );
}

/// RF-TEX-004: a slot the TXST leaves empty stays empty; the model's texture is not kept.
#[test]
fn an_empty_txst_slot_stays_empty_instead_of_keeping_the_model_texture() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (conn, _) = database();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    let mut app = object_app(directory.path());
    let scene = load_scene(&mut app, "shared.glb");
    let blue = spawn(&mut app, &scene, catalog.get(2));
    let red = spawn(&mut app, &scene, catalog.get(1));
    let control = spawn(&mut app, &scene, None);
    settle(&mut app, &[blue, red]);
    let control_material = surface(&app, control).1;
    assert!(
        material_of(&app, &control_material)
            .normal_map_texture
            .is_some(),
        "the model has a normal map"
    );
    let blue_material = surface(&app, blue).1;
    assert!(
        material_of(&app, &blue_material)
            .base_color_texture
            .is_some()
    );
    assert!(
        material_of(&app, &blue_material)
            .normal_map_texture
            .is_none(),
        "TXST 102 has no TX01, so the shape has no normal map"
    );
    assert!(
        material_of(&app, &surface(&app, red).1)
            .normal_map_texture
            .is_some()
    );
}

#[test]
fn every_entry_applies_by_geometry_index_with_minus_one_skipped_and_the_later_duplicate_winning() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (conn, mut records) = database();
    for record in [
        // Multi-entry list (655 retail STATs carry several): geometry 0, 1 and 3 each get theirs.
        stat(20, &[("A:0", 101, 0), ("B:1", 102, 1), ("C:2", 103, 3)]),
        // Index -1 is skipped; the entry that follows still applies.
        stat(22, &[("X:0", 101, -1), ("Y:1", 102, 0)]),
        // A later entry with the same index replaces the earlier one.
        stat(23, &[("A:0", 101, 0), ("B:1", 102, 0)]),
        // Geometry 2 is an untagged (non-lighting) shape: it counts but is not swapped.
        stat(21, &[("S:0", 101, 2)]),
    ] {
        records.insert(record.form_id, record);
    }
    export_to_db(&conn, &records).unwrap();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    let mut app = object_app(directory.path());
    let scene = load_scene(&mut app, "multi.glb");
    let control = spawn(&mut app, &scene, None);
    let all = spawn(&mut app, &scene, catalog.get(20));
    let minus = spawn(&mut app, &scene, catalog.get(22));
    let duplicate = spawn(&mut app, &scene, catalog.get(23));
    let untagged = spawn(&mut app, &scene, catalog.get(21));
    settle(&mut app, &[all, minus, duplicate, untagged]);
    let base = surfaces_of(&app, control);
    assert_eq!(
        base.iter().map(|surface| surface.0).collect::<Vec<_>>(),
        [Some(0), Some(1), Some(3), None],
        "the fixture has geometry 0, 1 and 3 tagged and geometry 2 untagged"
    );
    let model = |tag| material_at(&app, control, tag);
    let diffuse = |root, tag| image_data(&app, &material_at(&app, root, tag));
    // All three entries of the list apply, each to its own shape.
    let red = diffuse(all, Some(0));
    let blue = diffuse(all, Some(1));
    let green = diffuse(all, Some(3));
    assert!(red != blue && blue != green && red != green);
    assert_eq!(material_at(&app, all, None), model(None));
    assert_eq!(
        app.world().get::<ObjectTextureSwapState>(all),
        Some(&ObjectTextureSwapState::Applied {
            textured: 3,
            hidden: 0
        })
    );
    // -1 is skipped: only the entry at index 0 applies (blue), the rest keep the model material.
    assert_eq!(diffuse(minus, Some(0)), blue);
    for tag in [Some(1), Some(3), None] {
        assert_eq!(material_at(&app, minus, tag), model(tag));
    }
    // The later duplicate wins: blue, not red.
    assert_eq!(diffuse(duplicate, Some(0)), blue);
    // An entry that names the untagged shape applies to nothing.
    for tag in [Some(0), Some(1), Some(3), None] {
        assert_eq!(material_at(&app, untagged, tag), model(tag));
    }
    assert_eq!(
        app.world().get::<ObjectTextureSwapState>(untagged),
        Some(&ObjectTextureSwapState::Applied {
            textured: 0,
            hidden: 0
        })
    );
}

/// RF-TEX-005: an entry that names the NullTextureSet hides the shape.
#[test]
fn a_null_texture_set_entry_hides_the_shape() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (conn, mut records) = database();
    let hide = stat(30, &[("H:1", NULL_TEXTURE_SET, 1), ("A:0", 101, 0)]);
    records.insert(hide.form_id, hide);
    export_to_db(&conn, &records).unwrap();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    let mut app = object_app(directory.path());
    let scene = load_scene(&mut app, "multi.glb");
    let root = spawn(&mut app, &scene, catalog.get(30));
    settle(&mut app, &[root]);
    let visibility: Vec<_> = surfaces_of(&app, root)
        .into_iter()
        .map(|surface| (surface.0, surface.3))
        .collect();
    assert_eq!(
        visibility,
        [
            (Some(0), Visibility::Inherited),
            (Some(1), Visibility::Hidden),
            (Some(3), Visibility::Inherited),
            (None, Visibility::Inherited),
        ]
    );
    assert_eq!(
        app.world().get::<ObjectTextureSwapState>(root),
        Some(&ObjectTextureSwapState::Applied {
            textured: 1,
            hidden: 1
        })
    );
}

#[test]
fn partial_unload_preserves_live_shared_material_and_reuse() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (connection, _) = database();
    let catalog = ObjectTextureCatalog::read(&connection).unwrap();
    let mut app = object_app(directory.path());
    let scene = load_scene(&mut app, "shared.glb");
    let first = spawn(&mut app, &scene, catalog.get(1));
    let survivor = spawn(&mut app, &scene, catalog.get(1));
    settle(&mut app, &[first, survivor]);
    // IDs cannot accidentally keep the shared assets alive after the first root unloads.
    let material_id = surface(&app, first).1.id();
    assert_eq!(surface(&app, survivor).1.id(), material_id);
    let (image_id, normal_id) = {
        let material = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(material_id)
            .unwrap();
        (
            material.base_color_texture.as_ref().unwrap().id(),
            material.normal_map_texture.as_ref().unwrap().id(),
        )
    };
    let request_ids: Vec<_> = app
        .world()
        .get::<RequestedImages>(first)
        .unwrap()
        .0
        .values()
        .map(|handle| handle.id())
        .collect();
    assert_eq!(request_ids.len(), 2, "one request per applied slot");
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
    assert!(images.contains(image_id) && images.contains(normal_id));
    for request in &request_ids {
        assert!(images.contains(*request));
    }
    assert_eq!(app.world().resource::<DiffuseMaterials>().0.len(), 1);
    assert_eq!(
        app.world().get::<ObjectTextureSwapState>(survivor),
        Some(&ObjectTextureSwapState::Applied {
            textured: 1,
            hidden: 0
        })
    );
    let next = spawn(&mut app, &scene, catalog.get(1));
    settle(&mut app, &[next]);
    assert_eq!(surface(&app, next).1.id(), material_id);
    assert_eq!(surface(&app, survivor).1.id(), material_id);
    assert_eq!(app.world().resource::<DiffuseMaterials>().0.len(), 1);
}

#[test]
fn last_unload_releases_requests_materials_images_and_cache_entries() {
    use bevy::ecs::message::MessageCursor;
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (connection, _) = database();
    let catalog = ObjectTextureCatalog::read(&connection).unwrap();
    let mut app = object_app(directory.path());
    let scene = load_scene(&mut app, "shared.glb");
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
        .get::<RequestedImages>(roots[0])
        .unwrap()
        .0
        .get("textures/red.ktx2")
        .unwrap()
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
        Some(&ObjectTextureSwapState::Applied {
            textured: 1,
            hidden: 0
        })
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
fn unsupported_inputs_keep_the_model_material_and_say_why() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    // An older GLB has no geometry-index tags at all.
    let bytes = fs::read(directory.path().join("meshes/shared.glb")).unwrap();
    let mut container = gltf::binary::Glb::from_slice(&bytes).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&container.json).unwrap();
    document["meshes"][0]["primitives"][0]["extras"]
        .as_object_mut()
        .unwrap()
        .remove("mudcrabGeometryIndex");
    container.json = std::borrow::Cow::Owned(serde_json::to_vec(&document).unwrap());
    fs::write(
        directory.path().join("meshes/old.glb"),
        container.to_vec().unwrap(),
    )
    .unwrap();
    let (conn, mut records) = database();
    for record in [
        stat(7, &[("Rock:0", 104, 0)]),
        txst(104, "missing_file.dds", None),
        stat(8, &[("Rock:0", 105, 0)]),
        txst(105, "cube.dds", None),
        stat(9, &[("Rock:0", 106, 0)]),
        txst(106, "non_colour.dds", None),
        // A TXST that fills a slot this slice cannot apply keeps the model material.
        stat(10, &[("Rock:0", 107, 0)]),
        record(
            107,
            b"TXST",
            &[
                (b"TX00", b"red.dds\0".to_vec()),
                (b"TX07", b"specular.dds\0".to_vec()),
            ],
        ),
    ] {
        records.insert(record.form_id, record);
    }
    export_to_db(&conn, &records).unwrap();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    let mut app = object_app(directory.path());
    let scene = load_scene(&mut app, "shared.glb");
    let control = spawn(&mut app, &scene, None);
    let unresolved = spawn(&mut app, &scene, catalog.get(4));
    let missing = spawn(&mut app, &scene, catalog.get(7));
    let cube = spawn(&mut app, &scene, catalog.get(8));
    let non_colour = spawn(&mut app, &scene, catalog.get(9));
    let slots = spawn(&mut app, &scene, catalog.get(10));
    settle(&mut app, &[unresolved, missing, cube, non_colour, slots]);
    for root in [unresolved, missing, cube, non_colour, slots] {
        assert_eq!(surface(&app, root).1, surface(&app, control).1);
    }
    // An unresolved TXST and unsupported slots are reported and skipped for that entry only.
    for root in [unresolved, slots] {
        assert_eq!(
            app.world().get::<ObjectTextureSwapState>(root),
            Some(&ObjectTextureSwapState::Applied {
                textured: 0,
                hidden: 0
            })
        );
    }
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
    let reported = &app.world().resource::<ReportedSwapRefusals>().0;
    assert!(
        reported
            .iter()
            .any(|(base, reason)| *base == 10 && reason.contains("unsupported slots")),
        "{reported:?}"
    );
    assert!(reported.iter().any(|(base, _)| *base == 4));
    app.insert_resource(crate::visual_profile::VisualProfile::Skyrim);
    let raw = spawn(&mut app, &scene, catalog.get(1));
    settle(&mut app, &[raw]);
    assert_eq!(surface(&app, raw).1, surface(&app, control).1);
    assert!(matches!(
        app.world().get::<ObjectTextureSwapState>(raw),
        Some(ObjectTextureSwapState::Unsupported(_))
    ));
    app.insert_resource(crate::visual_profile::VisualProfile::Mudcrab);
    let old_scene = load_scene(&mut app, "old.glb");
    let old = spawn(&mut app, &old_scene, catalog.get(1));
    settle(&mut app, &[old]);
    assert!(matches!(
        app.world().get::<ObjectTextureSwapState>(old),
        Some(ObjectTextureSwapState::Unsupported(_))
    ));
}

#[test]
fn projection_rows_resolve_texture_sets_and_keep_other_txst_consumers() {
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
    assert!(matches!(
        &catalog.get(1).unwrap().swaps[0].kind,
        SwapKind::Textures { unapplied, .. } if unapplied == &["specular"]
    ));
    assert!(matches!(
        catalog.get(2).unwrap().swaps[0].kind,
        SwapKind::Missing(_)
    ));
    let path: String = conn
        .query_row(
            "SELECT specular_path FROM texture_sets WHERE id=101",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(path, "specular.dds");
    // Retail rows: both landscape paths convert, and the pack's `:N` names are not read.
    let pine = catalog.get(PINE).unwrap();
    assert!(matches!(
        &pine.swaps[0].kind,
        SwapKind::Textures { diffuse: Some(diffuse), normal: Some(normal), unapplied }
            if diffuse == "textures/landscape/pineforest02.ktx2"
                && normal == "textures/landscape/pineforest02_n.ktx2"
                && unapplied.is_empty()
    ));
    records.insert(1, record(1, b"STAT", &[]));
    export_to_db(&conn, &records).unwrap();
    assert!(ObjectTextureCatalog::read(&conn).unwrap().get(1).is_none());
    conn.execute_batch("DROP TABLE base_texture_swaps").unwrap();
    assert!(
        ObjectTextureCatalog::read(&conn).unwrap().get(2).is_none(),
        "old packs remain compatible"
    );
}

#[test]
fn refusals_warn_once_per_base_record_and_reason() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let (conn, mut records) = database();
    let other = stat(6, &[("Rock:0", 0xBEEF, 0)]);
    records.insert(other.form_id, other);
    export_to_db(&conn, &records).unwrap();
    let catalog = ObjectTextureCatalog::read(&conn).unwrap();
    let mut app = object_app(directory.path());
    let scene = load_scene(&mut app, "shared.glb");
    // Many placements of one refused base record, one placement of another, one applied.
    let refused_a: Vec<_> = (0..5)
        .map(|_| spawn(&mut app, &scene, catalog.get(4)))
        .collect();
    let refused_b = spawn(&mut app, &scene, catalog.get(6));
    let applied = spawn(&mut app, &scene, catalog.get(1));
    let roots: Vec<_> = refused_a
        .iter()
        .copied()
        .chain([refused_b, applied])
        .collect();
    settle(&mut app, &roots);
    // Later passes (a cell reload, a changed child list) must not add entries.
    for _ in 0..3 {
        app.update();
    }
    let reported = &app.world().resource::<ReportedSwapRefusals>().0;
    let bases: std::collections::BTreeSet<_> = reported.iter().map(|(base, _)| *base).collect();
    assert_eq!(reported.len(), 2, "{reported:?}");
    assert_eq!(bases, [4, 6].into());
}
