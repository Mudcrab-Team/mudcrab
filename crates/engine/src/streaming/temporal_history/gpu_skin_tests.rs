//! Native GPU regression coverage for retained skin extraction, allocation and upload.

use super::*;
use crate::world::components::ExteriorCellGrid;
use bevy::{
    app::{Plugin, SubApp, TaskPoolPlugin},
    asset::{AssetApp, AssetPlugin},
    ecs::schedule::ScheduleCleanupPolicy,
    mesh::skinning::SkinnedMeshInverseBindposes,
    pbr::{MAX_JOINTS, MeshPipelineSystems, MeshRenderPlugin, shadow_cache::ShadowStockInputs},
    render::{
        MainWorld, RenderStartup,
        render_resource::Buffer,
        renderer::{RenderDevice, initialize_renderer_without_callbacks},
    },
    shader::{Shader, ShaderLoader},
    tasks::block_on,
};
use std::{borrow::Cow, sync::mpsc, time::Duration};

fn native_skin_world() -> World {
    // Vulkan follows the engine's public native initialization path on Windows
    // too, without needing the private DX12 adapter-policy initializer.
    let mut settings = bevy::render::settings::WgpuSettings {
        backends: Some(wgpu::Backends::VULKAN),
        ..default()
    };
    if !cfg!(debug_assertions) {
        settings
            .instance_flags
            .remove(bevy::render::settings::InstanceFlags::VALIDATION_INDIRECT_CALL);
        settings.instance_flags = settings.instance_flags.with_env();
    }
    let resources = block_on(initialize_renderer_without_callbacks(
        wgpu::Backends::VULKAN,
        None,
        &settings,
    ));
    assert_ne!(resources.2.device_type, wgpu::DeviceType::Cpu);
    assert_eq!(resources.2.backend, wgpu::Backend::Vulkan);
    eprintln!("previous-skin upload adapter={}", resources.2.name);
    assert!(!bevy::pbr::skins_use_uniform_buffers(&resources.0.limits()));

    let mut app = App::new();
    app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default()))
        .init_asset::<Shader>()
        .init_asset_loader::<ShaderLoader>();
    let mut render = SubApp::new();
    render.insert_resource(resources.0);
    render.insert_resource(resources.1);
    render.insert_resource(resources.3);
    app.insert_sub_app(RenderApp, render);

    // SkinUniforms has private allocator fields and its initializer is not
    // re-exported. The public plugin finish hook registers that exact native
    // initializer. Keep its lightweight buffer setup, omit unrelated pipelines.
    MeshRenderPlugin::new(default()).finish(&mut app);
    let render = app.get_sub_app_mut(RenderApp).unwrap().world_mut();
    render.schedule_scope(RenderStartup, |world, schedule| {
        assert_eq!(
            schedule
                .remove_systems_in_set(
                    MeshPipelineSystems,
                    world,
                    ScheduleCleanupPolicy::RemoveSystemsOnly,
                )
                .expect("remove unrelated native mesh pipeline initialization"),
            2,
        );
    });
    render.run_schedule(RenderStartup);
    assert!(render.contains_resource::<SkinUniforms>());
    render.init_resource::<PreviousSkins>();
    render.init_resource::<ShadowStockInputs>();
    // Move the initialized world, preserving its identity and native resources.
    std::mem::take(app.remove_sub_app(RenderApp).unwrap().world_mut())
}

fn spawn_skin(main: &mut MainWorld, joints: Vec<Entity>) -> Entity {
    let inverse_bindposes = main
        .resource_mut::<Assets<SkinnedMeshInverseBindposes>>()
        .add(SkinnedMeshInverseBindposes::from(vec![
            Mat4::IDENTITY;
            joints.len()
        ]));
    main.spawn((
        SkinnedMesh {
            inverse_bindposes,
            joints,
        },
        ViewVisibility::VISIBLE,
    ))
    .id()
}

fn skin_rows(render: &World, entity: Entity, count: usize) -> Vec<Mat4> {
    let uniforms = render.resource::<SkinUniforms>();
    let start = uniforms.skin_index(entity.into()).unwrap() as usize;
    uniforms.current_staging_buffer[start..start + count].to_vec()
}

fn extract_native(extract: &mut Schedule, render: &mut World) {
    extract.run(render);
    render.resource_mut::<MainWorld>().clear_trackers();
}

fn read_skin_buffer(render: &World, buffer: &Buffer, rows: usize) -> Vec<Mat4> {
    // Native buffers deliberately have no COPY_SRC usage. Read the actual
    // storage binding through a compute copy, never a CPU staging upload clone.
    let device = render.resource::<RenderDevice>().wgpu_device();
    let queue = render.resource::<RenderQueue>();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("read native skin buffer"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(
            r#"
@group(0) @binding(0) var<storage, read> source: array<mat4x4<f32>>;
@group(0) @binding(1) var<storage, read_write> observed: array<mat4x4<f32>>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x < arrayLength(&observed) {
        observed[id.x] = source[id.x];
    }
}
"#,
        )),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("copy native skin matrices for readback"),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: default(),
        cache: None,
    });
    let size = (rows * size_of::<Mat4>()) as u64;
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("observed native skin matrices"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("native skin readback"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("actual skin buffer and observation"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("native skin readback"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("observe actual skin storage"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups((rows as u32).div_ceil(64), 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
    let submission = queue.submit([encoder.finish()]);
    let (send, receive) = mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            send.send(result).expect("readback receiver remains alive");
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(30)),
        })
        .expect("native skin readback completes within 30 seconds");
    receive
        .recv_timeout(Duration::from_secs(1))
        .expect("native skin map callback ran")
        .expect("native skin readback maps");
    let mapped = readback.slice(..).get_mapped_range();
    let matrices = mapped
        .as_chunks::<{ size_of::<Mat4>() }>()
        .0
        .iter()
        .map(|chunk| bytemuck::pod_read_unaligned::<Mat4>(chunk))
        .collect();
    drop(mapped);
    readback.unmap();
    let validation = block_on(scope.pop());
    assert!(
        validation.is_none(),
        "GPU readback validation: {validation:?}"
    );
    matrices
}

#[test]
#[ignore = "Requires reserved GPU ownership and a hardware Vulkan adapter; fails without an adapter"]
fn previous_skin_upload_preserves_animation_after_native_growth_and_compatible_relocation() {
    let mut render = native_skin_world();
    let mut main = MainWorld::default();
    main.insert_resource(RenderOrigin(IVec2::ZERO));
    main.insert_resource(CapturedOrigin::default());
    main.insert_resource(ParticipantRebase::default());
    main.spawn((
        Camera3d::default(),
        Camera::default(),
        bevy::core_pipeline::prepass::MotionVectorPrepass,
    ));
    main.init_resource::<Assets<SkinnedMeshInverseBindposes>>();
    let root = main.spawn(ExteriorCellGrid(IVec2::ZERO)).id();
    let old_pose = Mat4::from_scale_rotation_translation(
        Vec3::new(2.0, 3.0, 4.0),
        Quat::from_rotation_y(0.6),
        Vec3::new(30.0, 40.0, -90.0),
    );
    let exterior = main
        .spawn((ChildOf(root), GlobalTransform::from(old_pose)))
        .id();
    let interior_pose = Mat4::from_translation(Vec3::new(2.0, 3.0, 4.0));
    let interior = main.spawn(GlobalTransform::from(interior_pose)).id();
    let retained = spawn_skin(&mut main, vec![exterior, interior]);
    let replaced = spawn_skin(&mut main, vec![exterior]);
    render.insert_resource(main);

    // Keep these Schedule instances: recreating them would make every
    // SkinnedMesh appear changed and bypass unchanged-skin capture entirely.
    let mut capture = Schedule::new(ExtractSchedule);
    capture.add_systems(capture_previous_skins);
    let mut extract = Schedule::new(ExtractSchedule);
    extract.add_systems(extract_skins);
    let mut prepare = Schedule::new(Render);
    prepare.add_systems(prepare_skins);
    let mut upload = Schedule::new(Render);
    upload.add_systems(upload_previous_skins);
    for _ in 0..2 {
        capture.run(&mut render);
        extract_native(&mut extract, &mut render);
        prepare.run(&mut render);
        upload.run(&mut render);
    }
    let prior = skin_rows(&render, retained, 2);
    assert_eq!(prior, vec![old_pose, interior_pose]);
    let old_index = render
        .resource::<SkinUniforms>()
        .skin_index(retained.into())
        .unwrap();
    let old_capacity = render.resource::<SkinUniforms>().current_buffer.size();
    let old_current = render.resource::<SkinUniforms>().current_buffer.id();
    let old_previous = render.resource::<SkinUniforms>().prev_buffer.id();
    let displacement = grid_translation(IVec2::X, IVec2::ZERO);
    let rebase = Mat4::from_translation(-displacement);
    let animated =
        Mat4::from_translation(Vec3::new(3.0, 1.0, 2.0)) * Mat4::from_rotation_y(0.2) * old_pose;
    let replacement_pose = rebase * Mat4::from_translation(Vec3::new(90.0, 80.0, 70.0));
    {
        let mut main = render.resource_mut::<MainWorld>();
        main.resource_mut::<RenderOrigin>().0 = IVec2::X;
        *main.get_mut::<GlobalTransform>(exterior).unwrap() =
            GlobalTransform::from(rebase * animated);
        let replacement = main
            .spawn((ChildOf(root), GlobalTransform::from(replacement_pose)))
            .id();
        main.get_mut::<SkinnedMesh>(replaced).unwrap().joints = vec![replacement];
    }
    capture.run(&mut render);
    let captured = &render.resource::<PreviousSkins>().0;
    assert_eq!(
        captured.len(),
        1,
        "changed topology must retain native policy"
    );
    assert_eq!(captured[0].entity, MainEntity::from(retained));
    assert_eq!(captured[0].joints, vec![rebase * prior[0], prior[1]]);

    // Visibility can relocate an unchanged skin without changing its topology.
    // Free its allocation, occupy the hole, then let native extraction re-add
    // it. A full-size additional skin forces native prepare_skins to regrow
    // both GPU buffers after capture, invalidating their previous contents.
    *render
        .resource_mut::<MainWorld>()
        .get_mut::<ViewVisibility>(retained)
        .unwrap() = ViewVisibility::HIDDEN;
    extract_native(&mut extract, &mut render);
    assert!(
        render
            .resource::<SkinUniforms>()
            .skin_index(retained.into())
            .is_none()
    );
    let blocker = {
        let mut main = render.resource_mut::<MainWorld>();
        let blocker = spawn_skin(&mut main, vec![interior, interior]);
        spawn_skin(&mut main, vec![interior; MAX_JOINTS]);
        blocker
    };
    extract_native(&mut extract, &mut render);
    // Native changed-skin iteration may let the replacement claim the freed
    // slot first. Either occupant must remain untouched by the history upload.
    assert!(
        [blocker, replaced].into_iter().any(|entity| {
            render.resource::<SkinUniforms>().skin_index(entity.into()) == Some(old_index)
        }),
        "another native skin must occupy the retained skin's old slot",
    );
    *render
        .resource_mut::<MainWorld>()
        .get_mut::<ViewVisibility>(retained)
        .unwrap() = ViewVisibility::VISIBLE;
    extract_native(&mut extract, &mut render);
    let final_index = render
        .resource::<SkinUniforms>()
        .skin_index(retained.into())
        .unwrap() as usize;
    assert_ne!(
        final_index, old_index as usize,
        "exercise final allocation lookup"
    );
    let current = skin_rows(&render, retained, 2);
    assert_ne!(
        current[0],
        rebase * prior[0],
        "animation must survive the rebase"
    );
    assert_eq!(
        current[1], prior[1],
        "absolute interior joint must not shift"
    );
    assert_eq!(skin_rows(&render, replaced, 1), vec![replacement_pose]);

    prepare.run(&mut render);
    let uniforms = render.resource::<SkinUniforms>();
    assert!(uniforms.current_buffer.size() > old_capacity);
    assert!(uniforms.prev_buffer.size() > old_capacity);
    assert!(![old_current, old_previous].contains(&uniforms.current_buffer.id()));
    assert!(![old_current, old_previous].contains(&uniforms.prev_buffer.id()));
    let staging = uniforms.current_staging_buffer.clone();
    let current_before = read_skin_buffer(&render, &uniforms.current_buffer, staging.len());
    let mut expected_previous = read_skin_buffer(&render, &uniforms.prev_buffer, staging.len());
    assert_eq!(current_before, staging);
    assert_eq!(
        expected_previous, staging,
        "native growth initializes history with current poses"
    );
    expected_previous[final_index] = rebase * prior[0];
    expected_previous[final_index + 1] = prior[1];

    upload.run(&mut render);
    assert!(render.resource::<PreviousSkins>().0.is_empty());
    let uniforms = render.resource::<SkinUniforms>();
    let previous = read_skin_buffer(&render, &uniforms.prev_buffer, staging.len());
    assert_eq!(
        previous, expected_previous,
        "patch only the relocated prior joint rows"
    );
    assert_eq!(
        read_skin_buffer(&render, &uniforms.current_buffer, staging.len()),
        current_before
    );
    assert_eq!(uniforms.current_staging_buffer, staging);
    let vertex = Vec4::new(1.0, 2.0, 3.0, 1.0);
    assert!(
        (current[0] * vertex - previous[final_index] * vertex)
            .abs_diff_eq(animated * vertex - prior[0] * vertex, 0.001),
        "GPU history must preserve animated motion in the new coordinate frame",
    );

    // The next ordinary frame swaps native buffers. No captured work may replay
    // into that frame's previous buffer or translate it a second time.
    render
        .resource_mut::<MainWorld>()
        .resource_mut::<CapturedOrigin>()
        .grid = IVec2::X;
    capture.run(&mut render);
    assert!(render.resource::<PreviousSkins>().0.is_empty());
    extract_native(&mut extract, &mut render);
    prepare.run(&mut render);
    upload.run(&mut render);
    let uniforms = render.resource::<SkinUniforms>();
    assert_eq!(
        read_skin_buffer(&render, &uniforms.prev_buffer, staging.len()),
        current_before
    );
    assert_eq!(
        read_skin_buffer(&render, &uniforms.current_buffer, staging.len()),
        current_before
    );
}
