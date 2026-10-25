//! Raw-texel image loading for the `skyrim` visual profile.
//!
//! Bevy picks an image's texture format from [`ImageLoaderSettings::is_srgb`], not from the KTX2
//! header, so a colour texture's `.opensky-srgb` alias decodes as `…UnormSrgb` under the default
//! loader. `skyrim` evaluates the authored bytes, so those aliases have to decode as their UNORM
//! twin. [`RawTexelImageLoader`] claims only the alias extensions and forces `is_srgb = false`;
//! every other image keeps Bevy's own loader.

use bevy::{
    asset::{AssetApp, AssetLoader, LoadContext, io::Reader},
    image::{
        CompressedImageFormatSupport, CompressedImageFormats, Image, ImageLoader, ImageLoaderError,
        ImageLoaderSettings,
    },
    prelude::*,
    reflect::TypePath,
    render::render_resource::TextureFormat,
};

use crate::visual_profile::VisualProfile;

/// The alias extensions this loader claims. The literal list exists because `AssetLoader` hands
/// back a `&'static [&str]`; `alias_extensions_match_the_shared_infixes` keeps it honest.
const ALIAS_EXTENSIONS: [&str; 4] = [
    "opensky-srgb.ktx2",
    "opensky-srgb.opensky-wrap0.ktx2",
    "opensky-srgb.opensky-wrap1.ktx2",
    "opensky-srgb.opensky-wrap2.ktx2",
];

#[derive(Debug)]
pub enum RawTexelLoaderError {
    Image(ImageLoaderError),
    Srgb { path: String, format: TextureFormat },
}

impl std::fmt::Display for RawTexelLoaderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Image(error) => write!(formatter, "{error}"),
            Self::Srgb { path, format } => write!(
                formatter,
                "{path} decoded as sRGB ({format:?}); the skyrim profile requires raw texels"
            ),
        }
    }
}

impl std::error::Error for RawTexelLoaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Image(error) => Some(error),
            Self::Srgb { .. } => None,
        }
    }
}

impl From<ImageLoaderError> for RawTexelLoaderError {
    fn from(error: ImageLoaderError) -> Self {
        Self::Image(error)
    }
}

/// Loads colour-texture aliases as the UNORM twin of the format Bevy would pick.
#[derive(Clone, TypePath)]
pub struct RawTexelImageLoader {
    inner: ImageLoader,
}

impl RawTexelImageLoader {
    pub fn new(supported_compressed_formats: CompressedImageFormats) -> Self {
        Self {
            inner: ImageLoader::new(supported_compressed_formats),
        }
    }
}

impl AssetLoader for RawTexelImageLoader {
    type Asset = Image;
    type Settings = ImageLoaderSettings;
    type Error = RawTexelLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        settings: &ImageLoaderSettings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<Image, Self::Error> {
        let raw = ImageLoaderSettings {
            is_srgb: false,
            ..settings.clone()
        };
        let image = self.inner.load(reader, &raw, load_context).await?;
        let format = image.texture_descriptor.format;
        if format.is_srgb() {
            return Err(RawTexelLoaderError::Srgb {
                path: load_context.path().path().display().to_string(),
                format,
            });
        }
        Ok(image)
    }

    fn extensions(&self) -> &[&str] {
        &ALIAS_EXTENSIONS
    }
}

/// Registers [`RawTexelImageLoader`] under `skyrim` and nothing under `mudcrab`, so the default
/// profile keeps exactly the loader graph it had.
pub struct RawTexelPlugin {
    pub profile: VisualProfile,
}

impl Plugin for RawTexelPlugin {
    fn build(&self, app: &mut App) {
        if self.profile.decodes_colour_srgb() {
            return;
        }
        app.preregister_asset_loader::<RawTexelImageLoader>(&ALIAS_EXTENSIONS);
    }

    fn finish(&self, app: &mut App) {
        if self.profile.decodes_colour_srgb() {
            return;
        }
        let supported = match app.world().get_resource::<CompressedImageFormatSupport>() {
            Some(support) => support.0,
            None => {
                warn!(
                    "CompressedImageFormatSupport is absent; raw texel aliases load without \
                     compressed format support"
                );
                CompressedImageFormats::NONE
            }
        };
        app.register_asset_loader(RawTexelImageLoader::new(supported));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        asset::RenderAssetUsages,
        asset::{AssetApp, AssetPlugin, LoadState},
        gltf::{Gltf, GltfMaterial, GltfPlugin},
        image::{ImageLoader, ImagePlugin, ImageSampler},
    };
    use converter::texture::{TextureConverter, TextureEncoding};
    use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};
    use std::path::Path;

    fn dds_bytes(format: DxgiFormat, mip_levels: u32) -> Vec<u8> {
        let mut dds = Dds::new_dxgi(NewDxgiParams {
            height: 8,
            width: 8,
            depth: None,
            format,
            mipmap_levels: Some(mip_levels),
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: D3D10ResourceDimension::Texture2D,
            alpha_mode: AlphaMode::Straight,
        })
        .unwrap();
        for (index, byte) in dds.data.iter_mut().enumerate() {
            *byte = (index * 7 + 3) as u8;
        }
        let mut bytes = Vec::new();
        dds.write(&mut bytes).unwrap();
        bytes
    }

    fn ktx2(format: DxgiFormat) -> Vec<u8> {
        TextureConverter::convert_uncompressed(&dds_bytes(format, 2), TextureEncoding::ColorSrgb)
            .unwrap()
    }

    fn write_ktx2(directory: &Path, name: &str, format: DxgiFormat) {
        std::fs::write(directory.join(name), ktx2(format)).unwrap();
    }

    fn app(directory: &Path, profile: VisualProfile) -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin {
                file_path: directory.to_string_lossy().into_owned(),
                ..default()
            },
            ImagePlugin::default(),
            GltfPlugin::default(),
        ))
        .init_asset::<Mesh>()
        .insert_resource(CompressedImageFormatSupport(CompressedImageFormats::all()))
        .add_plugins(RawTexelPlugin { profile })
        // Stands in for the loader `RenderPlugin::finish` registers from the device's support.
        .register_asset_loader(ImageLoader::new(CompressedImageFormats::all()));
        app.finish();
        app.cleanup();
        app
    }

    fn wait<A: Asset>(app: &mut App, handle: &Handle<A>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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

    fn loaded_format(app: &App, handle: &Handle<Image>) -> TextureFormat {
        app.world()
            .resource::<Assets<Image>>()
            .get(handle)
            .expect("image is loaded")
            .texture_descriptor
            .format
    }

    #[test]
    fn alias_extensions_match_the_shared_infixes() {
        let transfer = shared::texture_aliases::SRGB_TEXTURE_SUFFIX.trim_start_matches('.');
        let expected: Vec<String> = std::iter::once(format!("{transfer}.ktx2"))
            .chain(
                shared::texture_aliases::WRAP_TEXTURE_SUFFIXES
                    .into_iter()
                    .map(|wrap| format!("{transfer}{wrap}.ktx2")),
            )
            .collect();
        assert_eq!(ALIAS_EXTENSIONS.to_vec(), expected);
    }

    #[test]
    fn skyrim_loads_every_alias_as_the_unorm_twin() {
        let directory = tempfile::tempdir().unwrap();
        let cases = [
            (
                "bc1.opensky-srgb.ktx2",
                DxgiFormat::BC1_UNorm,
                TextureFormat::Bc1RgbaUnorm,
            ),
            (
                "bc2.opensky-srgb.ktx2",
                DxgiFormat::BC2_UNorm,
                TextureFormat::Bc2RgbaUnorm,
            ),
            (
                "bc3.opensky-srgb.ktx2",
                DxgiFormat::BC3_UNorm,
                TextureFormat::Bc3RgbaUnorm,
            ),
            (
                "bc7.opensky-srgb.ktx2",
                DxgiFormat::BC7_UNorm,
                TextureFormat::Bc7RgbaUnorm,
            ),
            (
                "rgba8.opensky-srgb.ktx2",
                DxgiFormat::R8G8B8A8_UNorm,
                TextureFormat::Rgba8Unorm,
            ),
            (
                "wrap0.opensky-srgb.opensky-wrap0.ktx2",
                DxgiFormat::BC1_UNorm,
                TextureFormat::Bc1RgbaUnorm,
            ),
            (
                "wrap2.opensky-srgb.opensky-wrap2.ktx2",
                DxgiFormat::BC3_UNorm,
                TextureFormat::Bc3RgbaUnorm,
            ),
            (
                "a.b.opensky-srgb.ktx2",
                DxgiFormat::BC1_UNorm,
                TextureFormat::Bc1RgbaUnorm,
            ),
        ];
        for (name, format, _) in cases {
            write_ktx2(directory.path(), name, format);
        }
        let mut app = app(directory.path(), VisualProfile::Skyrim);
        for (name, _, expected) in cases {
            let handle = app.world().resource::<AssetServer>().load::<Image>(name);
            wait(&mut app, &handle);
            assert_eq!(loaded_format(&app, &handle), expected, "{name}");
        }
    }

    #[test]
    fn mudcrab_keeps_the_srgb_alias_identity() {
        let directory = tempfile::tempdir().unwrap();
        write_ktx2(
            directory.path(),
            "bc1.opensky-srgb.ktx2",
            DxgiFormat::BC1_UNorm,
        );
        let mut app = app(directory.path(), VisualProfile::Mudcrab);
        let handle = app
            .world()
            .resource::<AssetServer>()
            .load::<Image>("bc1.opensky-srgb.ktx2");
        wait(&mut app, &handle);
        assert_eq!(
            loaded_format(&app, &handle),
            TextureFormat::Bc1RgbaUnormSrgb
        );
    }

    #[test]
    fn plain_extensions_stay_with_bevys_image_loader() {
        let directory = tempfile::tempdir().unwrap();
        write_ktx2(directory.path(), "plain.ktx2", DxgiFormat::BC1_UNorm);
        Image::new_fill(
            bevy::render::render_resource::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            &[10, 20, 30, 255],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::MAIN_WORLD,
        )
        .try_into_dynamic()
        .unwrap()
        .save(directory.path().join("plain.png"))
        .unwrap();
        let mut app = app(directory.path(), VisualProfile::Skyrim);
        for (name, expected) in [
            ("plain.ktx2", TextureFormat::Bc1RgbaUnormSrgb),
            ("plain.png", TextureFormat::Rgba8UnormSrgb),
        ] {
            let handle = app.world().resource::<AssetServer>().load::<Image>(name);
            wait(&mut app, &handle);
            assert_eq!(loaded_format(&app, &handle), expected, "{name}");
        }
    }

    fn write_material_gltf(directory: &Path, images: serde_json::Value) {
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "images": images,
            "samplers": [{"wrapS": 10497, "wrapT": 10497, "magFilter": 9728, "minFilter": 9984}],
            "textures": [{"source": 0, "sampler": 0}, {"source": 1, "sampler": 0}],
            "materials": [{
                "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}},
                "normalTexture": {"index": 1}
            }]
        });
        std::fs::write(
            directory.join("body.gltf"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn gltf_aliases_load_raw_and_keep_the_gltf_sampler() {
        let directory = tempfile::tempdir().unwrap();
        write_ktx2(
            directory.path(),
            "body.opensky-srgb.ktx2",
            DxgiFormat::BC1_UNorm,
        );
        write_ktx2(directory.path(), "body.ktx2", DxgiFormat::BC1_UNorm);
        write_material_gltf(
            directory.path(),
            serde_json::json!([{"uri": "body.opensky-srgb.ktx2"}, {"uri": "body.ktx2"}]),
        );
        let mut app = app(directory.path(), VisualProfile::Skyrim);
        let gltf_handle = app
            .world()
            .resource::<AssetServer>()
            .load::<Gltf>("body.gltf");
        wait(&mut app, &gltf_handle);

        let world = app.world();
        let gltf = world.resource::<Assets<Gltf>>().get(&gltf_handle).unwrap();
        let material = world
            .resource::<Assets<GltfMaterial>>()
            .get(&gltf.materials[0])
            .unwrap();
        let images = world.resource::<Assets<Image>>();
        let base = images
            .get(material.base_color_texture.as_ref().unwrap())
            .unwrap();
        assert_eq!(base.texture_descriptor.format, TextureFormat::Bc1RgbaUnorm);
        let ImageSampler::Descriptor(sampler) = &base.sampler else {
            panic!("the glTF sampler reaches the alias load");
        };
        assert_eq!(
            sampler.address_mode_u,
            bevy::image::ImageAddressMode::Repeat
        );
        assert_eq!(
            sampler.address_mode_v,
            bevy::image::ImageAddressMode::Repeat
        );
        let normal = images
            .get(material.normal_map_texture.as_ref().unwrap())
            .unwrap();
        assert_eq!(
            normal.texture_descriptor.format,
            TextureFormat::Bc1RgbaUnorm
        );
    }

    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut encoded = String::new();
        for chunk in bytes.chunks(3) {
            let bytes = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let packed =
                (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
            encoded.push(ALPHABET[(packed >> 18) as usize & 63] as char);
            encoded.push(ALPHABET[(packed >> 12) as usize & 63] as char);
            encoded.push(if chunk.len() > 1 {
                ALPHABET[(packed >> 6) as usize & 63] as char
            } else {
                '='
            });
            encoded.push(if chunk.len() > 2 {
                ALPHABET[packed as usize & 63] as char
            } else {
                '='
            });
        }
        encoded
    }

    #[test]
    fn embedded_gltf_images_bypass_the_wrapper_and_fail_validation_under_skyrim() {
        let directory = tempfile::tempdir().unwrap();
        let mut png = Vec::new();
        Image::new_fill(
            bevy::render::render_resource::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            &[10, 20, 30, 255],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::MAIN_WORLD,
        )
        .try_into_dynamic()
        .unwrap()
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        write_material_gltf(
            directory.path(),
            serde_json::json!([
                {"uri": format!("data:image/png;base64,{}", base64(&png))},
                {"uri": format!("data:image/png;base64,{}", base64(&png))}
            ]),
        );
        let mut app = app(directory.path(), VisualProfile::Skyrim);
        let gltf_handle = app
            .world()
            .resource::<AssetServer>()
            .load::<Gltf>("body.gltf");
        wait(&mut app, &gltf_handle);

        let world = app.world();
        let gltf = world.resource::<Assets<Gltf>>().get(&gltf_handle).unwrap();
        let material = world
            .resource::<Assets<GltfMaterial>>()
            .get(&gltf.materials[0])
            .unwrap();
        let embedded = material.base_color_texture.clone().unwrap();
        let images = world.resource::<Assets<Image>>();
        assert_eq!(
            images.get(&embedded).unwrap().texture_descriptor.format,
            TextureFormat::Rgba8UnormSrgb
        );
        let material = StandardMaterial {
            base_color_texture: Some(embedded),
            ..default()
        };
        assert!(
            crate::streaming::validate_standard_material(&material, images, false)
                .unwrap_err()
                .contains("color space mismatch")
        );
        assert_eq!(
            crate::streaming::validate_standard_material(&material, images, true),
            Ok(1)
        );
    }
}
