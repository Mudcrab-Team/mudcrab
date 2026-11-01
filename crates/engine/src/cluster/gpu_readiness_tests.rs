// Included inside render::tests. No window or GPU adapter is needed.
fn headless_cluster_app() -> App {
    use bevy::asset::{AssetApp, AssetPlugin};
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>();
    app
}

#[test]
fn renderer_initializes_missing_main_settings_before_headless_startup() {
    let mut app = headless_cluster_app();
    app.add_plugins(ClusterRenderPlugin);
    assert!(app.world().contains_resource::<ClusterSettings>());
    assert!(
        app.world()
            .contains_resource::<crate::graphics_settings::GraphicsSettings>()
    );
    app.update();
    assert!(app.world().contains_resource::<ClusterDummyMesh>());
}

#[test]
fn renderer_preserves_main_settings_and_propagates_quality_to_gpu_uniforms() {
    use bevy::render::{MainWorld, extract_resource::extract_resource};
    let mut app = headless_cluster_app();
    app.insert_resource(crate::graphics_settings::GraphicsSettings {
        cluster_error_pixels: 2.5,
        cluster_terrain_error_pixels: 3.5,
    })
    .insert_resource(ClusterSettings {
        fixed_count_draw: true,
        ..default()
    })
    .add_plugins(ClusterRenderPlugin);
    app.update();
    assert!(app.world().resource::<ClusterSettings>().fixed_count_draw);

    let mut render = World::new();
    let mut main = MainWorld::default();
    *main = std::mem::take(app.world_mut());
    render.insert_resource(main);
    let mut extract = Schedule::new(ExtractSchedule);
    extract.add_systems(extract_resource::<ClusterSettings, ()>);
    extract.run(&mut render);
    let view = ExtractedView {
        retained_view_entity: RetainedViewEntity::new(Entity::PLACEHOLDER.into(), None, 0),
        clip_from_view: Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.1),
        world_from_view: GlobalTransform::IDENTITY,
        clip_from_world: None,
        target_format: bevy::render::render_resource::TextureFormat::Rgba16Float,
        viewport: UVec4::new(0, 0, 1280, 720),
        color_grading: default(),
        invert_culling: false,
    };
    for (object, terrain) in [(2.5, 3.5), (4.5, 5.5)] {
        {
            let mut main = render.resource_mut::<MainWorld>();
            {
                let mut graphics =
                    main.resource_mut::<crate::graphics_settings::GraphicsSettings>();
                graphics.cluster_error_pixels = object;
                graphics.cluster_terrain_error_pixels = terrain;
            }
            use bevy::ecs::system::RunSystemOnce;
            main.run_system_once(follow_graphics_quality).unwrap();
        }
        extract.run(&mut render);
        let settings = render.resource::<ClusterSettings>();
        let params = cull_params(&view, settings, 1, 0, 1);
        assert_eq!(params.error_pixels, object);
        assert_eq!(params.terrain_error_pixels, terrain);
        let bytes = bytemuck::bytes_of(&params);
        let offset = std::mem::offset_of!(CullParams, error_pixels);
        assert_eq!(&bytes[offset..offset + 4], &object.to_ne_bytes());
        assert!(settings.fixed_count_draw);
    }
}
