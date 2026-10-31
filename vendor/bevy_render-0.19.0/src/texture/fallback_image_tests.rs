use super::*;

#[test]
fn initialized_color_fallbacks_preserve_bytes_without_gpu_write_capabilities() {
    for dimension in [
        TextureViewDimension::D1,
        TextureViewDimension::D2,
        TextureViewDimension::D2Array,
        TextureViewDimension::Cube,
        TextureViewDimension::CubeArray,
        TextureViewDimension::D3,
    ] {
        for value in [0, 255] {
            let image = fallback_image_source(TextureFormat::Rgba8UnormSrgb, dimension, 1, value);
            let layers = match dimension {
                TextureViewDimension::Cube | TextureViewDimension::CubeArray => 6,
                _ => 1,
            };
            assert_eq!(image.data, Some(vec![value; layers as usize * 4]));
            assert_eq!(
                image.texture_descriptor.size,
                Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: layers,
                }
            );
            assert_eq!(image.texture_descriptor.sample_count, 1);
            assert!(image.texture_descriptor.usage.contains(
                TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC
            ));
            assert!(
                !image
                    .texture_descriptor
                    .usage
                    .intersects(TextureUsages::RENDER_ATTACHMENT | TextureUsages::STORAGE_BINDING),
                "{dimension:?}"
            );
        }
    }
}

#[test]
fn depth_and_multisample_fallbacks_keep_render_attachment_initialization() {
    for (format, samples) in [
        (TextureFormat::Depth32Float, 1),
        (TextureFormat::Rgba8UnormSrgb, 4),
        (TextureFormat::Depth32Float, 4),
    ] {
        let image = fallback_image_source(format, TextureViewDimension::D2, samples, 255);
        assert!(image.data.is_none());
        assert!(image
            .texture_descriptor
            .usage
            .contains(TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING));
        assert_eq!(image.texture_descriptor.sample_count, samples);
        assert_eq!(image.texture_descriptor.format, format);
    }
}
