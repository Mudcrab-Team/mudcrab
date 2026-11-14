//! Fixtures and waits shared by the engine's unit tests.

use bevy::{
    asset::{Asset, LoadState},
    prelude::*,
};
use converter::texture::{TextureConverter, TextureEncoding};
use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};

/// A KTX2 colour texture encoded from a generated DDS of `format`.
///
/// The converter copies native block data and uncompressed RGBA8 verbatim, so the fixture's bytes
/// are the source texture's bytes; only the container is new.
pub(crate) fn colour_ktx2(format: DxgiFormat, mip_levels: u32) -> Vec<u8> {
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
    TextureConverter::convert_uncompressed(&bytes, TextureEncoding::ColorSrgb).unwrap()
}

/// Runs the app until `handle` and its dependencies are loaded, failing the test on a load error
/// or after 30 seconds.
pub(crate) fn wait_for_asset<A: Asset>(app: &mut App, handle: &Handle<A>) {
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
