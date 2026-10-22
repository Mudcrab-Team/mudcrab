//! Loads a model-space glTF through the real glTF loader and checks what the native handler
//! binds: the skin material, its linear `_s` twin and the dependency holder.

use super::*;
use crate::skin_material::SkyrimSkinFlags;
use bevy::{
    asset::{LoadState, RenderAssetUsages},
    image::{CompressedImageFormats, ImageAddressMode, ImageLoader, ImagePlugin, ImageSampler},
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    world_serialization::{WorldAsset, WorldSerializationPlugin},
};
use std::{fs, path::Path};

/// Stands in for `bevy_pbr`'s private glTF handler, which only a rendering app registers: it
/// publishes `"{label}/std"` and binds it, which is all [`NativeMaterialHandler`] reads.
#[derive(Clone)]
struct StockStandardHandler;

impl GltfExtensionHandler for StockStandardHandler {
    fn dyn_clone(&self) -> Box<dyn ErasedGltfExtensionHandler> {
        Box::new(self.clone())
    }

    fn on_material(
        &mut self,
        context: &mut LoadContext<'_>,
        _source: &gltf::Material<'_>,
        _handle: Handle<GltfMaterial>,
        loaded: &GltfMaterial,
        label: &str,
    ) {
        context.add_labeled_asset(
            format!("{label}/std"),
            StandardMaterial {
                base_color_texture: loaded.base_color_texture.clone(),
                normal_map_texture: loaded.normal_map_texture.clone(),
                specular_tint_texture: loaded.specular_tint_texture.clone(),
                reflectance: loaded.reflectance,
                ..default()
            },
        );
    }

    fn on_spawn_mesh_and_material(
        &mut self,
        context: &mut LoadContext<'_>,
        _primitive: &gltf::Primitive<'_>,
        _mesh: &gltf::Mesh<'_>,
        _material: &gltf::Material<'_>,
        entity: &mut EntityWorldMut,
        label: &str,
    ) {
        entity.insert(MeshMaterial3d(
            context.get_label_handle::<StandardMaterial>(format!("{label}/std")),
        ));
    }
}

fn write_png(directory: &Path, name: &str, pixel: [u8; 4]) {
    Image::new_fill(
        Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &pixel,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD,
    )
    .try_into_dynamic()
    .unwrap()
    .save(directory.join(name))
    .unwrap();
}

/// One triangle with positions, normals and UVs, its material tagged `model_space` with the given
/// `shaderFlags1`, the `_msn` as normal texture and the `_s` published as the converter does: an
/// sRGB `.opensky-srgb` alias in `KHR_materials_specular.specularColorTexture`, clamped.
fn write_model_space_gltf(directory: &Path, shader_flags_1: u64) {
    write_png(directory, "body_msn.png", [128, 128, 255, 255]);
    write_png(directory, "body_s.opensky-srgb.png", [64, 64, 64, 255]);
    write_png(directory, "body_s.png", [64, 64, 64, 255]);
    let floats: [f32; 24] = [
        0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, // positions
        0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, // normals
        0.0, 0.0, 1.0, 0.0, 0.0, 1.0, // uvs
    ];
    let bytes: Vec<u8> = floats
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    fs::write(directory.join("body.bin"), &bytes).unwrap();
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["KHR_materials_specular"],
        "buffers": [{"uri": "body.bin", "byteLength": bytes.len()}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 24}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"}
        ],
        "images": [{"uri": "body_msn.png"}, {"uri": "body_s.opensky-srgb.png"}],
        "samplers": [{"wrapS": 33071, "wrapT": 33071}],
        "textures": [{"source": 0}, {"source": 1, "sampler": 0}],
        "materials": [{
            "normalTexture": {"index": 0},
            "extensions": {"KHR_materials_specular": {
                "specularFactor": 1.0,
                "specularColorTexture": {"index": 1}
            }},
            "extras": {"openSkyrim": {
                "normalConvention": "model_space",
                "shaderFlags1": shader_flags_1,
                "shaderFlags2": 1
            }}
        }],
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
            "material": 0
        }]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}],
        "scene": 0
    });
    fs::write(
        directory.join("body.gltf"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
}

fn loader_app(directory: &Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: directory.to_string_lossy().into_owned(),
            ..default()
        },
        ImagePlugin::default(),
        WorldSerializationPlugin,
        bevy::gltf::GltfPlugin::default(),
    ))
    .init_asset::<Mesh>()
    .init_asset::<StandardMaterial>()
    .register_asset_loader(ImageLoader::new(CompressedImageFormats::NONE));
    app.world_mut()
        .resource_mut::<GltfExtensionHandlers>()
        .0
        .write_blocking()
        .push(Box::new(StockStandardHandler));
    app.add_plugins(NifMaterialPlugin);
    app.finish();
    app.cleanup();
    app
}

fn wait_for_dependencies<A: Asset>(app: &mut App, handle: &Handle<A>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.update();
        let server = app.world().resource::<AssetServer>();
        if let LoadState::Failed(error) = server.load_state(handle.id()) {
            panic!("fixture load failed: {error}");
        }
        if server.is_loaded_with_dependencies(handle.id()) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture load timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn model_space_material_loads_as_a_skin_material_with_a_linear_gloss_mask() {
    let directory = tempfile::tempdir().unwrap();
    // Depth test and write (no depth state), SLSF1 Specular and Model_Space_Normals.
    write_model_space_gltf(directory.path(), (1 << 31) | (1 << 12) | 1);
    let mut app = loader_app(directory.path());
    let scene: Handle<WorldAsset> = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset("body.gltf"));
    wait_for_dependencies(&mut app, &scene);

    let mut scene_world = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .remove(&scene)
        .unwrap()
        .world;
    let mut query = scene_world.query::<(
        &MeshMaterial3d<SkyrimSkinMaterial>,
        &NifNativeMaterialSource,
        &NifSourceMaterial,
        Option<&MeshMaterial3d<StandardMaterial>>,
    )>();
    let (skin, base, source, standard) = query.single(&scene_world).unwrap();
    assert!(standard.is_none(), "a mesh draws once per material binding");

    let world = app.world();
    let server = world.resource::<AssetServer>();
    assert!(server.is_loaded_with_dependencies(skin.0.id()));
    let skin = world
        .resource::<Assets<SkyrimSkinMaterial>>()
        .get(&skin.0)
        .unwrap();
    let materials = world.resource::<Assets<StandardMaterial>>();
    let stock = materials.get(&source.0).unwrap();
    let base = materials.get(&base.0).unwrap();
    assert!(base.normal_map_texture.is_none());
    assert_eq!(
        skin.extension.model_space_normal,
        stock.normal_map_texture.clone()
    );
    assert!(skin.base.normal_map_texture.is_none());
    assert!(skin.base.specular_tint_texture.is_none());
    assert_eq!(
        skin.extension.settings.flags,
        SkyrimSkinFlags::MODEL_SPACE_NORMALS
            | SkyrimSkinFlags::SPECULAR_MAP
            | SkyrimSkinFlags::SPECULAR
    );

    let srgb = stock.specular_tint_texture.as_ref().unwrap();
    let linear = skin.extension.specular.as_ref().unwrap();
    assert_eq!(
        server.get_path(srgb.id()).unwrap(),
        "body_s.opensky-srgb.png".into()
    );
    assert_eq!(server.get_path(linear.id()).unwrap(), "body_s.png".into());
    let images = world.resource::<Assets<Image>>();
    assert_eq!(
        images.get(srgb).unwrap().texture_descriptor.format,
        TextureFormat::Rgba8UnormSrgb
    );
    let linear = images.get(linear).unwrap();
    assert_eq!(linear.texture_descriptor.format, TextureFormat::Rgba8Unorm);
    let ImageSampler::Descriptor(sampler) = &linear.sampler else {
        panic!("the glTF sampler is passed to the linear load");
    };
    assert_eq!(sampler.address_mode_u, ImageAddressMode::ClampToEdge);
    assert_eq!(sampler.address_mode_v, ImageAddressMode::ClampToEdge);
}

#[test]
fn model_space_material_with_a_depth_state_keeps_the_depth_material() {
    let directory = tempfile::tempdir().unwrap();
    // No depth write: an authored depth state, which keeps its own extension.
    write_model_space_gltf(directory.path(), (1 << 12) | 1);
    let mut app = loader_app(directory.path());
    let scene: Handle<WorldAsset> = app
        .world()
        .resource::<AssetServer>()
        .load(GltfAssetLabel::Scene(0).from_asset("body.gltf"));
    wait_for_dependencies(&mut app, &scene);
    let mut scene_world = app
        .world_mut()
        .resource_mut::<Assets<WorldAsset>>()
        .remove(&scene)
        .unwrap()
        .world;
    let mut skins = scene_world.query::<&MeshMaterial3d<SkyrimSkinMaterial>>();
    assert_eq!(skins.iter(&scene_world).count(), 0);
    let mut depth = scene_world.query::<&MeshMaterial3d<NifDepthMaterial>>();
    assert_eq!(depth.iter(&scene_world).count(), 1);
}
