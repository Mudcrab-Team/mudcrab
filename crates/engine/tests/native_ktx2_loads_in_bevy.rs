//! Native-block KTX2 from the converter must load through Bevy's KTX2 image
//! path with the expected GPU format, not just parse in the `ktx2` crate.

use bevy::image::{CompressedImageFormats, Image};
use bevy::render::render_resource::TextureFormat;
use converter::texture::{TextureConverter, TextureEncoding};
use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};

fn bc3_fixture() -> Vec<u8> {
    let mut dds = Dds::new_dxgi(NewDxgiParams {
        height: 8,
        width: 8,
        depth: None,
        format: DxgiFormat::BC3_UNorm,
        mipmap_levels: Some(2),
        array_layers: None,
        caps2: None,
        is_cubemap: false,
        resource_dimension: D3D10ResourceDimension::Texture2D,
        alpha_mode: AlphaMode::Straight,
    })
    .unwrap();
    for (index, byte) in dds.data.iter_mut().enumerate() {
        *byte = (index * 13 + 1) as u8;
    }
    let mut bytes = Vec::new();
    dds.write(&mut bytes).unwrap();
    bytes
}

#[test]
fn native_bc3_loads_as_bc3_without_transcoding() {
    let dds = bc3_fixture();
    let ktx = TextureConverter::convert_uncompressed(&dds, TextureEncoding::ColorSrgb).unwrap();
    let image: Image = bevy::image::ktx2_buffer_to_image(&ktx, CompressedImageFormats::all(), true)
        .expect("Bevy must load native-block KTX2");
    assert_eq!(
        image.texture_descriptor.format,
        TextureFormat::Bc3RgbaUnormSrgb
    );
    assert_eq!(image.texture_descriptor.size.width, 8);
    assert_eq!(image.texture_descriptor.size.height, 8);
    assert_eq!(image.texture_descriptor.mip_level_count, 2);
}

#[test]
fn zstd_supercompressed_bc3_loads_as_bc3() {
    let dds = bc3_fixture();
    // Default conversion supercompresses each mip level; Bevy's zstd_rust
    // decode path must restore the native blocks behind the same format.
    let ktx = TextureConverter::convert(&dds, TextureEncoding::ColorSrgb).unwrap();
    let reader = ktx2::Reader::new(&ktx).unwrap();
    assert_eq!(
        reader.header().supercompression_scheme,
        Some(ktx2::SupercompressionScheme::Zstandard)
    );
    let image: Image = bevy::image::ktx2_buffer_to_image(&ktx, CompressedImageFormats::all(), true)
        .expect("Bevy must decode Zstandard supercompression");
    assert_eq!(
        image.texture_descriptor.format,
        TextureFormat::Bc3RgbaUnormSrgb
    );
    assert_eq!(image.texture_descriptor.mip_level_count, 2);
    let plain = TextureConverter::convert_uncompressed(&dds, TextureEncoding::ColorSrgb).unwrap();
    let plain_image: Image =
        bevy::image::ktx2_buffer_to_image(&plain, CompressedImageFormats::all(), true).unwrap();
    assert_eq!(image.data, plain_image.data);
}
