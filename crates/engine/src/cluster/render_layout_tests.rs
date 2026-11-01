//! Count the actual specialized layouts with Metal's buffer-slot rules on every backend.
use super::*;
use bevy::{
    asset::{AssetApp, AssetPlugin},
    ecs::system::RunSystemOnce,
    material::MaterialProperties,
    mesh::MeshVertexBufferLayouts,
    pbr::{
        Material, MeshLayouts, MeshPipeline, MeshPipelineViewLayouts,
        init_mesh_pipeline_view_layouts, init_prepass_pipeline,
    },
    render::{
        render_resource::BindingType,
        renderer::{RenderAdapter, WgpuWrapper},
    },
};

/// wgpu-hal 29.0.4 `src/lib.rs:326`: MAX_VERTEX_BUFFERS = 16. Metal caps its
/// adapter at that value (`src/metal/adapter.rs:784`) and checks layout buffers
/// plus vertex buffers against it (`src/metal/device.rs:1354`).
const METAL_VERTEX_STAGE_BUFFER_LIMIT: u32 = 16;

/// Mirror `create_pipeline_layout` in wgpu-hal 29.0.4 `src/metal/device.rs`:
/// immediates, buffer bindings, one argument buffer per binding array (including
/// texture/sampler arrays), and a sizes buffer reserved for every vertex stage.
fn vertex_stage_buffer_count(descriptor: &RenderPipelineDescriptor) -> u32 {
    let bindings = descriptor
        .layout
        .iter()
        .flat_map(|group| &group.entries)
        .filter(|entry| entry.visibility.contains(ShaderStages::VERTEX))
        .filter(|entry| {
            entry.count.is_some()
                || matches!(
                    entry.ty,
                    BindingType::Buffer { .. }
                        | BindingType::AccelerationStructure { .. }
                        | BindingType::ExternalTexture
                )
        })
        .count() as u32;
    u32::from(descriptor.immediate_size != 0)
        + bindings
        + 1
        + descriptor.vertex.buffers.len() as u32
}

fn pipeline_fixture() -> App {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = bevy::tasks::block_on(instance.request_adapter(&default()))
        .expect("pipeline layout regression requires an adapter");
    let features = adapter.features()
        & (wgpu::Features::TEXTURE_BINDING_ARRAY
            | wgpu::Features::BUFFER_BINDING_ARRAY
            | wgpu::Features::STORAGE_RESOURCE_BINDING_ARRAY
            | wgpu::Features::SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING
            | wgpu::Features::PARTIALLY_BOUND_BINDING_ARRAY);
    let (device, _) = bevy::tasks::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: adapter.limits(),
        ..default()
    }))
    .unwrap();
    let device = RenderDevice::from(device);
    let adapter = RenderAdapter(Arc::new(WgpuWrapper::new(adapter)));
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Shader>()
        .insert_resource(device.clone())
        .insert_resource(adapter.clone());
    app.world_mut()
        .run_system_once(init_mesh_pipeline_view_layouts)
        .unwrap();
    // MeshPipeline's initializer is private. Its layout owners are public and
    // initialized above by Bevy; specialize the real pipeline with those owners.
    let mesh_pipeline = MeshPipeline {
        view_layouts: app.world().resource::<MeshPipelineViewLayouts>().clone(),
        clustered_forward_buffer_binding_type: device.get_supported_read_only_binding_type(1),
        mesh_layouts: MeshLayouts::new(&device, &adapter),
        shader: Handle::default(),
        per_object_buffer_batch_size: None,
        binding_arrays_are_usable: true,
        clustered_decals_are_usable: false,
        skins_use_uniform_buffers: false,
    };
    app.insert_resource(MaterialPipeline {
        mesh_pipeline: mesh_pipeline.clone(),
    })
    .insert_resource(mesh_pipeline);
    app.world_mut()
        .run_system_once(init_prepass_pipeline)
        .unwrap();
    app
}

fn check_material<M: Material>(app: &App, name: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let server = app.world().resource::<AssetServer>();
    let device = app.world().resource::<RenderDevice>();
    let forward = app.world().resource::<MaterialPipeline>();
    let prepass = app.world().resource::<PrepassPipeline>();
    let (draw, shadow) = data_layouts();
    let mut layouts = MeshVertexBufferLayouts::default();
    for tangents in [false, true] {
        let mesh = dummy_mesh_with_tangents(tangents);
        let layout = mesh.get_mesh_vertex_buffer_layout(&mut layouts);
        for force_no_bindless in [false, true] {
            let material = PreparedMaterial {
                binding: default(),
                shadow_cache_untracked_texture: false,
                properties: Arc::new(MaterialProperties {
                    material_layout: Some(BindGroupLayoutDescriptor {
                        label: M::label().into(),
                        entries: M::bind_group_layout_entries(device, force_no_bindless),
                    }),
                    bindless: !force_no_bindless && M::bindless_supported(device),
                    ..default()
                }),
            };
            let key = |bits| ClusterMaterialPipelineKey {
                material: ErasedMaterialPipelineKey {
                    type_id: std::any::TypeId::of::<M>(),
                    mesh_key: ErasedMeshPipelineKey::new(bits),
                    material_key: material.properties.material_key.clone(),
                },
                tangents,
            };
            let camera = MeshPipelineKey::from_msaa_samples(1)
                | MeshPipelineKey::VIEW_PROJECTION_PERSPECTIVE;
            let mut descriptors = vec![
                build_descriptor(forward, key(camera), &layout, &material, &draw, server).unwrap(),
            ];
            // Include the largest camera prepass view layout, depth-only, and
            // material-backed cutout variants. No hand-maintained stock counts.
            for bits in [
                MeshPipelineKey::DEPTH_PREPASS,
                MeshPipelineKey::DEPTH_PREPASS | MeshPipelineKey::MAY_DISCARD,
                MeshPipelineKey::DEPTH_PREPASS
                    | MeshPipelineKey::NORMAL_PREPASS
                    | MeshPipelineKey::MOTION_VECTOR_PREPASS
                    | MeshPipelineKey::MAY_DISCARD,
            ] {
                descriptors.push(
                    build_prepass_descriptor(
                        prepass,
                        key(camera | bits),
                        &layout,
                        &material,
                        &draw,
                        server,
                    )
                    .unwrap()
                    .0,
                );
            }
            for unclipped in [false, true] {
                descriptors.push(build_shadow_descriptor(prepass, &shadow, unclipped, server));
                descriptors.push(
                    build_shadow_mask_descriptor(
                        prepass,
                        key(shadow_mesh_key(unclipped, MeshPipelineKey::empty())),
                        &layout,
                        &material,
                        &draw,
                        server,
                    )
                    .unwrap(),
                );
            }
            for descriptor in descriptors {
                let count = vertex_stage_buffer_count(&descriptor);
                let label = descriptor.label.as_deref().unwrap();
                let groups: Vec<_> = descriptor
                    .layout
                    .iter()
                    .map(|group| {
                        vertex_stage_buffer_count(&RenderPipelineDescriptor {
                            layout: vec![group.clone()],
                            ..default()
                        }) - 1
                    })
                    .collect();
                println!(
                    "{name}, tangents={tangents}, bindless={}: {label}: {count} (groups {groups:?}, sizes 1, vertex {})",
                    material.properties.bindless,
                    descriptor.vertex.buffers.len()
                );
                if count > METAL_VERTEX_STAGE_BUFFER_LIMIT {
                    violations.push(format!(
                        "{name} {label} needs {count} vertex-stage buffers (groups {groups:?})"
                    ));
                }
            }
        }
    }
    violations
}

#[test]
fn cluster_pipeline_layouts_fit_the_metal_vertex_stage_buffer_limit() {
    let app = pipeline_fixture();
    let violations = [
        check_material::<StandardMaterial>(&app, "standard"),
        check_material::<super::super::terrain::ClusterTerrainMaterial>(&app, "terrain"),
    ]
    .concat();
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}
