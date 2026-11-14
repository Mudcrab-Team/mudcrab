//! A headless proof of what the vertex-pulled draw relies on in wgpu.
//!
//! It issues the production selector's `draw_indirect` over `group_args`-shaped records on a real
//! wgpu device and reads the pixels back. It shows that `first_instance` becomes the shader's
//! `instance_index`, that the instance count bounds the group, that a zero count draws nothing,
//! that slots outside every group never draw, and that padding triangles leave no pixel.
//!
//! This is a mechanism test, not hardware proof: it uses a small stand-in shader, not the cluster
//! shaders, and any adapter may run it, software Vulkan included. CI runs it on software Vulkan.
//! Run it on a Mac with `WGPU_BACKEND=metal`:
//! `cargo test -p engine --lib pulled_draw -- --ignored`.
use super::*;
use std::sync::mpsc;
use wgpu::util::DeviceExt;

/// The stand-in vertex stage mirrors the production pulled stage: the slot is `instance_index`, the
/// record comes from `visible[slot]` with the triangle count in the top byte of word 2, the corner
/// comes from `vertex_index`, and a padding vertex is one finite position (zero area).
const PULLED_SHADER: &str = r#"
@group(0) @binding(0) var<storage, read> visible: array<vec4<u32>>;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) tag: u32,
}

@vertex
fn vertex(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) slot: u32,
) -> Out {
    let draw = visible[slot];
    let tri = vertex_index / 3u;
    let corner = vertex_index % 3u;
    var out: Out;
    out.tag = 0u;
    if tri >= (draw.z >> 24u) {
        out.position = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        return out;
    }
    // Triangle `tri` of the cluster owns pixel `draw.x * 4 + tri` of an 8 by 8 target. A pixel is
    // 0.25 clip units. The triangle is 0.3 pixel around the pixel centre, so it covers that centre.
    let cell = draw.x * 4u + tri;
    let centre = vec2<f32>(
        (f32(cell % 8u) + 0.5) * 0.25 - 1.0,
        1.0 - (f32(cell / 8u) + 0.5) * 0.25,
    );
    var offset = vec2<f32>(-0.3, -0.3);
    if corner == 1u {
        offset = vec2<f32>(0.3, -0.3);
    } else if corner == 2u {
        offset = vec2<f32>(0.0, 0.3);
    }
    out.position = vec4<f32>(centre + offset * 0.25, 0.0, 1.0);
    out.tag = ((draw.z & 0xffffffu) + 1u) | ((tri + 1u) << 16u);
    return out;
}

@fragment
fn fragment(in: Out) -> @location(0) u32 {
    return in.tag;
}
"#;

const TARGET: u32 = 8;
const SLOT_COUNT: u32 = 16;
const TRIANGLE_LIMIT: u32 = 4;
/// Triangles of the cluster in slot `slot`: 1 to 4, so padding vertices are drawn too.
fn cluster_triangles(slot: u32) -> u32 {
    1 + slot % 4
}

fn pulled_gpu() -> (wgpu::Device, wgpu::Queue, wgpu::AdapterInfo) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = bevy::tasks::block_on(instance.request_adapter(&default()))
        .expect("the pulled-draw proof needs a Vulkan, Metal or DX12 adapter");
    let info = adapter.get_info();
    crate::cluster::gpu_support(adapter.features(), &adapter.limits())
        .unwrap_or_else(|reason| panic!("adapter {info:?} cannot run the cluster path: {reason}"));
    let (device, queue) = bevy::tasks::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::INDIRECT_FIRST_INSTANCE,
        required_limits: adapter.limits(),
        ..default()
    }))
    .unwrap_or_else(|error| panic!("adapter {info:?} refused the device: {error}"));
    (device, queue, info)
}

fn words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

#[test]
#[ignore = "needs a GPU adapter, software Vulkan included; CI runs it by name"]
fn pulled_draw_indirect_covers_exactly_each_groups_slots() {
    let (device, queue, info) = pulled_gpu();
    eprintln!("pulled draw adapter: {info:?}");

    // Three groups with millions of slots of room each, as on the real pack. Each must still
    // encode as one command. The selected ranges below are what the cull would have written.
    let mut groups = DrawGroups::new(3);
    for batch in 0..3 {
        groups.add(batch, 4_000_000, None);
    }
    let draws = groups.plan().0;
    assert_eq!(draws.len(), 3);
    // (first slot, selected count) per group: a plain range, an empty group, and a range at the end.
    let selected = [(2u32, 3u32), (5, 0), (9, 2)];

    let mut args = Vec::new();
    for (draw, &(first, count)) in draws.iter().zip(&selected) {
        assert_eq!(draw.group_args_offset as usize, args.len());
        args.extend(words(&[TRIANGLE_LIMIT * 3, count, 0, first]));
    }
    let group_args = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("pulled draw group args"),
        contents: &args,
        usage: BufferUsages::STORAGE | BufferUsages::INDIRECT | BufferUsages::COPY_DST,
    });
    // Every slot holds a valid record, so a draw outside its group's range would show up.
    let mut records = Vec::new();
    for slot in 0..SLOT_COUNT {
        records.extend([slot, 0, (cluster_triangles(slot) << 24) | slot, 0]);
    }
    let visible = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("pulled draw visible"),
        contents: &words(&records),
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
    });

    let layout_entry = wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[layout_entry],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &bind_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: visible.as_entire_binding(),
        }],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pulled draw stand-in"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(PULLED_SHADER)),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&bind_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vertex"),
            compilation_options: default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fragment"),
            compilation_options: default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::R32Uint,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: TARGET,
            height: TARGET,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Uint,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&default());
    let row_bytes = 256u32; // the copy row alignment; 8 pixels use 32 of it
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row_bytes * TARGET),
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&default());
    let mut commands = 0;
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        for draw in &draws {
            // The production selector decides the call, as `DrawClusterIndirect` does.
            match group_draw_call(false, draw) {
                GroupDrawCall::Indirect { offset } => pass.draw_indirect(&group_args, offset),
                other => panic!("no GPU draw count must give a pulled draw, got {other:?}"),
            }
            commands += 1;
        }
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: Some(TARGET),
            },
        },
        wgpu::Extent3d {
            width: TARGET,
            height: TARGET,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let (send, receive) = mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            send.send(result).unwrap();
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(60)),
        })
        .unwrap();
    receive
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap()
        .unwrap();
    let mapped = readback.slice(..).get_mapped_range();
    let pixels: &[u32] = bytemuck::cast_slice(&mapped);

    let mut expected = [0u32; (TARGET * TARGET) as usize];
    for &(first, count) in &selected {
        for slot in first..first + count {
            for tri in 0..cluster_triangles(slot) {
                expected[(slot * 4 + tri) as usize] = (slot + 1) | ((tri + 1) << 16);
            }
        }
    }
    let mut actual = [0u32; (TARGET * TARGET) as usize];
    for row in 0..TARGET {
        for column in 0..TARGET {
            actual[(row * TARGET + column) as usize] =
                pixels[(row * row_bytes / 4 + column) as usize];
        }
    }
    assert_eq!(
        actual, expected,
        "adapter {info:?}: the drawn pixels must be exactly the selected slots' triangles \
         (a wrong first_instance, instance count or padding triangle changes them)"
    );
    assert_eq!(commands, draws.len(), "one command per group");
}
