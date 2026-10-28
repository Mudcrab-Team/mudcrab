//! Device-backed composition checks for the binding-free authored depth extension.
use bevy::{
    prelude::*,
    render::{
        render_resource::AsBindGroup,
        renderer::initialize_renderer_without_callbacks,
        settings::{Backends, RenderResources, WgpuSettings},
    },
};
use engine::nif_depth::{NifDepthExtension, NifDepthMaterial, NifDepthState};

#[test]
#[ignore = "needs a GPU; run with --run-ignored only"]
fn authored_depth_extension_preserves_legacy_non_bindless_layout() {
    // Probe compiled native backends before Bevy's infallible initializer. A
    // machine without a physical adapter cannot run this device-backed test;
    // device creation or validation failures on an available adapter must fail.
    let backends = wgpu::Instance::enabled_backend_features()
        & (Backends::VULKAN | Backends::DX12 | Backends::METAL | Backends::GL);
    assert!(
        !backends.is_empty(),
        "authored depth bindings require a compiled native GPU backend"
    );
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapters = bevy::tasks::block_on(instance.enumerate_adapters(backends));
    assert!(
        adapters
            .iter()
            .any(|adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu),
        "authored depth bindings require a physical GPU adapter for {backends:?}"
    );
    let RenderResources(device, _, adapter, ..) = bevy::tasks::block_on(
        initialize_renderer_without_callbacks(backends, None, &WgpuSettings::default()),
    );
    assert_ne!(adapter.device_type, wgpu::DeviceType::Cpu, "GPU required");
    eprintln!(
        "authored depth bindings adapter: {} ({:?})",
        adapter.name, adapter.backend
    );
    eprintln!(
        "base bindless supported: {}",
        StandardMaterial::bindless_supported(&device)
    );
    // Hardware capability and material opt-in are separate in Bevy 0.19.
    // ExtendedMaterial forces non-bindless when either slot count is None.
    assert!(NifDepthMaterial::bindless_slot_count().is_none());
    assert!(NifDepthMaterial::bindless_descriptor().is_none());
    let extension = NifDepthExtension {
        state: NifDepthState {
            depth_test: true,
            depth_write: false,
            decal: true,
            alpha_mask: true,
            cast_shadows: false,
        },
    };
    for requested_no_bindless in [false, true] {
        // Match the renderer's effective path: missing slot-count opt-in forces
        // the base material's non-bindless bindings.
        let force_no_bindless =
            requested_no_bindless || NifDepthMaterial::bindless_slot_count().is_none();
        assert!(force_no_bindless);
        let base = StandardMaterial::bind_group_layout_entries(&device, force_no_bindless);
        // Exercise the native wrapper's own opt-in check, including the default request.
        let combined = NifDepthMaterial::bind_group_layout_entries(&device, requested_no_bindless);
        assert_eq!(combined, base);
        // Ask wgpu to validate the actual combined non-bindless base layout.
        let _combined_layout = device.create_bind_group_layout("authored-depth-base", &combined);
        let entries = NifDepthExtension::bind_group_layout_entries(&device, force_no_bindless);
        assert!(entries.is_empty());
        let layout = device.create_bind_group_layout("authored-depth-extension", &entries);
        let prepared = extension
            .unprepared_bind_group(&layout, &device, &mut (), force_no_bindless)
            .expect("the state-only extension must prepare without GPU resources");
        assert!(prepared.bindings.0.is_empty());
        let _bind_group = device.create_bind_group("authored-depth-extension", &layout, &[]);
    }
}

/// Exercises the material allocator, native PBR pipeline and an actual color draw.
/// No cluster plugin is installed: this is the ordinary authored-depth path.
#[test]
#[ignore = "needs a physical GPU; run separately with --run-ignored only"]
fn authored_depth_material_draws_without_clusters() {
    use bevy::{
        app::PluginsState,
        camera::RenderTarget,
        core_pipeline::tonemapping::Tonemapping,
        pbr::MaterialPlugin,
        render::{
            gpu_readback::{Readback, ReadbackComplete},
            render_resource::{TextureFormat, TextureUsages},
            renderer::RenderAdapterInfo,
        },
        window::ExitCondition,
        winit::WinitPlugin,
    };
    use std::time::{Duration, Instant};

    #[derive(Resource, Default)]
    struct Pixels(Vec<u8>);

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            })
            .disable::<WinitPlugin>()
            .disable::<bevy::audio::AudioPlugin>(),
    )
    .add_plugins(MaterialPlugin::<NifDepthMaterial>::default())
    .init_resource::<Pixels>()
    .insert_resource(ClearColor(Color::BLACK));

    let deadline = Instant::now() + Duration::from_secs(120);
    while app.plugins_state() == PluginsState::Adding {
        assert!(
            Instant::now() < deadline,
            "renderer initialization timed out"
        );
        bevy::tasks::tick_global_task_pools_on_main_thread();
        std::thread::yield_now();
    }
    app.finish();
    app.cleanup();
    let adapter = app.world().resource::<RenderAdapterInfo>();
    assert_ne!(
        adapter.device_type,
        wgpu::DeviceType::Cpu,
        "physical GPU required"
    );
    eprintln!(
        "authored depth draw adapter: {} ({:?})",
        adapter.name, adapter.backend
    );

    // 64 RGBA pixels make one naturally aligned 256-byte readback row.
    let mut image = Image::new_target_texture(64, 64, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = app.world_mut().resource_mut::<Assets<Image>>().add(image);
    app.world_mut()
        .spawn(Readback::texture(target.clone()))
        .observe(|event: On<ReadbackComplete>, mut pixels: ResMut<Pixels>| {
            pixels.0.clone_from(&event.data);
        });
    app.world_mut().spawn((
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(Color::BLACK),
            ..default()
        },
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        Msaa::Off,
        Transform::from_xyz(0.0, 0.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    let mesh = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::default());
    let material = app
        .world_mut()
        .resource_mut::<Assets<NifDepthMaterial>>()
        .add(NifDepthMaterial {
            base: StandardMaterial {
                base_color: Color::srgb(1.0, 0.0, 0.0),
                unlit: true,
                ..default()
            },
            extension: NifDepthExtension {
                state: NifDepthState {
                    depth_test: true,
                    depth_write: false,
                    decal: false,
                    alpha_mask: false,
                    cast_shadows: false,
                },
            },
        });
    let cube = app
        .world_mut()
        .spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::default()))
        .id();
    let red_pixels = |bytes: &[u8]| {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 200 && p[1] < 20 && p[2] < 20 && p[3] > 200)
            .count()
    };
    loop {
        app.update();
        let pixels = &app.world().resource::<Pixels>().0;
        if red_pixels(pixels) > 100 {
            assert_eq!(pixels.len(), 64 * 64 * 4);
            if let Ok(path) = std::env::var("NIF_DEPTH_DRAW_OUT") {
                image::save_buffer(path, pixels, 64, 64, image::ColorType::Rgba8)
                    .expect("write GPU draw receipt image");
            }
            break;
        }
        assert!(
            Instant::now() < deadline,
            "authored-depth native PBR draw produced no red pixels"
        );
        std::thread::yield_now();
    }
    // Clear-only control: the observed red pixels must come from the material draw.
    app.world_mut().despawn(cube);
    app.world_mut().resource_mut::<Pixels>().0.clear();
    loop {
        app.update();
        let pixels = &app.world().resource::<Pixels>().0;
        if pixels.len() == 64 * 64 * 4
            && pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[0] < 5 && p[1] < 5 && p[2] < 5)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "clear-only control retained drawn pixels"
        );
        std::thread::yield_now();
    }
}
