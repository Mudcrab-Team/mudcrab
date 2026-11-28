//! SPEC-2 render regressions. Read back the offscreen camera after the production passes.
use super::*;
use crate::{
    nif_depth::{DEPTH_TEST, NifDepthExtension, NifDepthMaterial, NifDepthState},
    terrain_merge::{
        TerrainMergePlugin,
        material::{MergedTerrainExtension, MergedTerrainMaterial},
        mesh::{TERRAIN_SLOT_IDS, TERRAIN_SLOT_WEIGHTS},
    },
};
use bevy::{
    core_pipeline::prepass::DepthPrepass,
    mesh::VertexAttributeValues,
    pbr::{MaterialExtension, MaterialPlugin},
    render::storage::ShaderBuffer,
};

#[test]
#[ignore = "needs a GPU adapter; run with MUDCRAB_RETAIL_PIXEL=1 and --ignored"]
fn retail_pixel_spec2_merged_terrain_fog_uses_the_visible_depth() {
    let mut app = app_with(|app| {
        app.add_plugins(TerrainMergePlugin::new(None));
    });
    assert!(!MergedTerrainExtension::enable_prepass());
    let terrain = spawn_surface(&mut app, false, AlphaMode::Opaque);
    let mesh = app.world().get::<Mesh3d>(terrain).unwrap().0.clone();
    {
        let mut meshes = app.world_mut().resource_mut::<Assets<Mesh>>();
        let mut mesh = meshes.get_mut(&mesh).unwrap();
        for group in 0..2 {
            mesh.insert_attribute(
                TERRAIN_SLOT_IDS[group],
                VertexAttributeValues::Uint16x4(vec![[0; 4]; 4]),
            );
            mesh.insert_attribute(
                TERRAIN_SLOT_WEIGHTS[group],
                VertexAttributeValues::Unorm8x4(vec![[0; 4]; 4]),
            );
        }
    }
    let mut texture = Image::new_fill(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[255; 4],
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    texture.texture_view_descriptor = Some(bevy::render::render_resource::TextureViewDescriptor {
        dimension: Some(bevy::render::render_resource::TextureViewDimension::D2Array),
        ..default()
    });
    let texture = app.world_mut().resource_mut::<Assets<Image>>().add(texture);
    let entries = app
        .world_mut()
        .resource_mut::<Assets<ShaderBuffer>>()
        .add(ShaderBuffer::new(
            &[0, 0, 0, 0, 255, 255, 255, 255],
            RenderAssetUsages::RENDER_WORLD,
        ));
    let material = app
        .world_mut()
        .resource_mut::<Assets<MergedTerrainMaterial>>()
        .add(MergedTerrainMaterial {
            base: StandardMaterial {
                unlit: true,
                cull_mode: None,
                ..default()
            },
            extension: MergedTerrainExtension::new(&[texture], &[], entries).unwrap(),
        });
    app.world_mut()
        .entity_mut(terrain)
        .remove::<MeshMaterial3d<StandardMaterial>>()
        .insert(MeshMaterial3d(material));

    let target = target(&mut app);
    let camera = spawn_camera(&mut app, &target, overhead(500.0));
    app.world_mut().entity_mut(camera).insert((
        DepthPrepass,
        RetailFogPass::new(RetailFogParams {
            near: 250.0,
            far: 1000.0,
            power: 1.0,
            max: 1.0,
            near_rgb: [1.0, 0.0, 0.0],
            far_rgb: [1.0, 0.0, 0.0],
        }),
    ));
    let hill = read_pixel(&mut app, &target, 12);
    assert!(
        hill.y > 0.25 && hill.x - hill.y > 0.15,
        "fogged terrain: {hill}"
    );
    let behind = spawn_surface(&mut app, false, AlphaMode::Opaque);
    app.world_mut()
        .entity_mut(behind)
        .insert(Transform::from_xyz(0.0, -600.0, 0.0));
    close(
        read_pixel(&mut app, &target, 12),
        hill.to_array(),
        "hill over prepass object",
    );
}

#[test]
#[ignore = "needs a GPU adapter; run with MUDCRAB_RETAIL_PIXEL=1 and --ignored"]
fn retail_pixel_spec2_sorted_cutout_keeps_alpha_test_and_fogs_like_blend() {
    let mut app = app_with(|app| {
        app.add_plugins(MaterialPlugin::<NifDepthMaterial>::default());
    });
    let surface = spawn_surface(&mut app, false, AlphaMode::Blend);
    spawn_sun(&mut app, [255, 255, 255], 1.0, Vec3::Z);
    let stock = app
        .world()
        .get::<MeshMaterial3d<StandardMaterial>>(surface)
        .unwrap()
        .0
        .clone();
    let mut cutout = {
        let mut materials = app.world_mut().resource_mut::<Assets<StandardMaterial>>();
        let mut blend = materials.get_mut(&stock).unwrap();
        blend.base_color = Color::linear_rgba(1.0, 1.0, 1.0, 0.5);
        blend.clone()
    };

    let target = target(&mut app);
    let camera = spawn_camera(&mut app, &target, overhead(500.0));
    app.world_mut().entity_mut(camera).insert(DistanceFog {
        color: Color::linear_rgba(1.0, 0.0, 0.0, 1.0),
        falloff: FogFalloff::PowerRampBlended {
            start: 100.0,
            end: 400.0,
            power: 1.0,
            near_color: Color::linear_rgb(1.0, 0.0, 0.0),
        },
        ..default()
    });
    let fogged_blend = read_pixel(&mut app, &target, 12);
    assert!(
        fogged_blend.x > 0.25 && fogged_blend.y < 0.01,
        "fogged blend: {fogged_blend}"
    );

    cutout.alpha_mode = AlphaMode::Mask(0.25);
    let state =
        NifDepthState::retail_from_material(DEPTH_TEST, 0, cutout.alpha_mode, false, Some([6, 7]))
            .unwrap()
            .retail_first_stage(cutout.alpha_mode);
    assert!(!state.first_stage);
    let material = app
        .world_mut()
        .resource_mut::<Assets<NifDepthMaterial>>()
        .add(NifDepthMaterial {
            base: cutout,
            extension: NifDepthExtension { state },
        });
    app.world_mut()
        .entity_mut(surface)
        .remove::<MeshMaterial3d<StandardMaterial>>()
        .insert(MeshMaterial3d(material.clone()));
    close(
        read_pixel(&mut app, &target, 12),
        fogged_blend.to_array(),
        "sorted cutout passes",
    );

    app.world_mut()
        .resource_mut::<Assets<NifDepthMaterial>>()
        .get_mut(&material)
        .unwrap()
        .base
        .alpha_mode = AlphaMode::Mask(0.75);
    close(
        read_pixel(&mut app, &target, 12),
        [0.0; 3],
        "sorted cutout discards",
    );
}
