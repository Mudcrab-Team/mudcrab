//! Runs the retail HDR shaders on a real GPU through [`encode_chain`] and compares every stage
//! with a CPU reference of NEEDS 18 and NEEDS 27. The test opens a headless device (no window) and
//! needs an adapter, so it is ignored by default. Run it with
//! `cargo test -p engine retail_hdr_gpu -- --ignored`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "test sampling and half-float conversion over small, bounded texture coordinates"
)]

use crate::{
    retail_hdr::{HdrBlock, RetailHdr, RetailHdrConfig, adapt_rates, adapt_step},
    retail_hdr_chain::{ChainTarget, PassKind, bloom_size, bloom_taps, chain_plan, chain_sizes},
    retail_hdr_render::{ChainGpu, WIDE_HDR_FORMAT, encode_chain},
    shader_validation::compose,
};
use bevy::render::render_resource::encase::UniformBuffer;
use std::{borrow::Cow, collections::HashMap};

const SIZE: u32 = 64;
const RADIUS: usize = 3;

type Colour = [f32; 3];

/// One CPU texture, sampled as a clamped GPU sampler would.
#[derive(Clone)]
struct Plane {
    width: u32,
    height: u32,
    texels: Vec<Colour>,
}

impl Plane {
    fn from_fn((width, height): (u32, u32), value: impl Fn(f32, f32) -> Colour) -> Self {
        let mut texels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let u = (x as f32 + 0.5) / width as f32;
                let v = (y as f32 + 0.5) / height as f32;
                // The chain targets are half floats.
                texels.push(value(u, v).map(|c| half_to_f32(f32_to_half(c))));
            }
        }
        Self {
            width,
            height,
            texels,
        }
    }

    fn texel(&self, x: i64, y: i64) -> Colour {
        let x = x.clamp(0, i64::from(self.width) - 1) as usize;
        let y = y.clamp(0, i64::from(self.height) - 1) as usize;
        self.texels[y * self.width as usize + x]
    }

    fn nearest(&self, u: f32, v: f32) -> Colour {
        self.texel(
            (u * self.width as f32).floor() as i64,
            (v * self.height as f32).floor() as i64,
        )
    }

    fn linear(&self, u: f32, v: f32) -> Colour {
        let x = u * self.width as f32 - 0.5;
        let y = v * self.height as f32 - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b) = (self.texel(x0, y0), self.texel(x0 + 1, y0));
        let (c, d) = (self.texel(x0, y0 + 1), self.texel(x0 + 1, y0 + 1));
        std::array::from_fn(|i| {
            (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy
        })
    }
}

fn luminance(c: Colour) -> f32 {
    0.2125 * c[0] + 0.7154 * c[1] + 0.0721 * c[2]
}

/// The 4 taps at (+-1, +-1) source texels, weight 1/4, bilinear.
fn average4(source: &Plane, u: f32, v: f32) -> Colour {
    let (du, dv) = (1.0 / source.width as f32, 1.0 / source.height as f32);
    let mut sum = [0.0; 3];
    for ox in [-1.0_f32, 1.0] {
        for oy in [-1.0_f32, 1.0] {
            let sample = source.linear(u + ox * du, v + oy * dv);
            for i in 0..3 {
                sum[i] += sample[i] * 0.25;
            }
        }
    }
    sum
}

/// The 16 taps at -1.5, -0.5, 0.5 and 1.5 source texels in both axes, weight 1/16.
fn average16(source: &Plane, u: f32, v: f32, linear: bool) -> f32 {
    let (du, dv) = (1.0 / source.width as f32, 1.0 / source.height as f32);
    let mut sum = 0.0;
    for ox in [-1.5_f32, -0.5, 0.5, 1.5] {
        for oy in [-1.5_f32, -0.5, 0.5, 1.5] {
            let at = (u + ox * du, v + oy * dv);
            let sample = if linear {
                source.linear(at.0, at.1)
            } else {
                source.nearest(at.0, at.1)
            };
            sum += sample[0] / 16.0;
        }
    }
    sum
}

/// The CPU chain of one frame, with the sizes of the engine.
struct Frame {
    pair: Plane,
    bloom: Plane,
}

#[derive(Clone, Copy)]
enum ChainPrecision {
    Half,
    PackedNearest,
    PackedTruncated,
}

impl ChainPrecision {
    fn store(self, size: (u32, u32), value: impl Fn(f32, f32) -> Colour) -> Plane {
        Plane::from_fn(size, |u, v| {
            let colour = value(u, v);
            match self {
                Self::Half => colour,
                Self::PackedNearest | Self::PackedTruncated => {
                    // These fixtures use finite nonnegative values within the packed range.
                    std::array::from_fn(|channel| {
                        let value = colour[channel];
                        let exponent = ((value.to_bits() >> 23) & 0xff) as i32 - 127;
                        let mantissa_bits = if channel == 2 { 5 } else { 6 };
                        let step = 2.0_f32.powi(exponent.max(-14) - mantissa_bits);
                        let units = value / step;
                        step * match self {
                            Self::PackedNearest => units.round_ties_even(),
                            _ => units.floor(),
                        }
                    })
                }
            }
        })
    }
}

fn cpu_frame(
    scene: &Plane,
    previous: Option<&Plane>,
    hdr: &RetailHdr,
    precision: ChainPrecision,
) -> Frame {
    let sizes = chain_sizes(scene.width, scene.height, true);
    let first = precision.store(sizes[0], |u, v| average4(scene, u, v));
    let second = precision.store(sizes[1], |u, v| {
        let l = luminance(average4(&first, u, v));
        [l, l, l]
    });
    assert_eq!(sizes.len(), 3, "the test scene has one 16-tap step");
    let pair = precision.store(sizes[2], |u, v| match previous {
        None => {
            let l = average16(&second, u, v, false);
            [l, l, l]
        }
        Some(old) => {
            let n = average16(&second, u, v, true);
            let old = old.nearest(u, v);
            [
                adapt_step(old[0], n, hdr.adapt.x),
                adapt_step(old[1], n, hdr.adapt.y),
                0.0,
            ]
        }
    });
    let size = bloom_size(scene.width, scene.height);
    let weights = bloom_taps(RADIUS);
    let (threshold, scale, k_r) = (hdr.bloom.x, hdr.bloom.y, hdr.bloom.w);
    let k = RADIUS as i32;
    let swap = precision.store(size, |u, v| {
        let mut sum = [0.0; 3];
        for tap in -k..=k {
            let sample = first.nearest(u, v + tap as f32 / size.1 as f32);
            let weight = weights[(tap + 7) as usize];
            for i in 0..3 {
                sum[i] += weight * (sample[i] / k_r - threshold).max(0.0);
            }
        }
        sum.map(|c| k_r * scale * c)
    });
    let bloom = precision.store(size, |u, v| {
        let mut sum = [0.0; 3];
        for tap in -k..=k {
            let sample = swap.nearest(u + tap as f32 / swap.width as f32, v);
            let weight = weights[(tap + 7) as usize];
            for i in 0..3 {
                sum[i] += weight * sample[i];
            }
        }
        sum
    });
    Frame { pair, bloom }
}

/// IEEE binary32 to binary16 with rounding to nearest, for the normal range of the test values.
fn f32_to_half(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x007f_ffff;
    if exponent <= 0 {
        return sign;
    }
    let mut half = ((exponent as u32) << 10) | (mantissa >> 13);
    let rest = mantissa & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && half & 1 == 1) {
        half += 1;
    }
    sign | half as u16
}

fn half_to_f32(half: u16) -> f32 {
    let sign = if half & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = i32::from((half >> 10) & 0x1f);
    let mantissa = f32::from(half & 0x03ff);
    if exponent == 31 {
        if mantissa == 0.0 {
            sign * f32::INFINITY
        } else {
            f32::NAN
        }
    } else if exponent == 0 {
        sign * mantissa * 2.0_f32.powi(-24)
    } else {
        sign * (1.0 + mantissa / 1024.0) * 2.0_f32.powi(exponent - 15)
    }
}

fn module(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    definitions: HashMap<String, naga_oil::compose::ShaderDefValue>,
) -> wgpu::ShaderModule {
    let naga = compose(source, label, definitions).unwrap_or_else(|error| panic!("{error}"));
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Naga(Cow::Owned(naga)),
    })
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry() -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: None,
        },
        count: None,
    }
}

fn extent((width, height): (u32, u32)) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

fn texture(
    device: &wgpu::Device,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

/// The texels of a texture as 32-bit floats (four per texel).
fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::Texture,
) -> Vec<[f32; 4]> {
    let (width, height) = (source.width(), source.height());
    let format = source.format();
    let bytes_per_texel = match format {
        wgpu::TextureFormat::Rgba16Float => 8,
        wgpu::TextureFormat::Rg11b10Ufloat
        | wgpu::TextureFormat::Rgba8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb => 4,
        _ => panic!("unsupported HDR readback format: {format:?}"),
    };
    let row = (width * bytes_per_texel).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        source.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(height),
            },
        },
        extent((width, height)),
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range();
    let mut texels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let start = (y * row + x * bytes_per_texel) as usize;
            if format == wgpu::TextureFormat::Rg11b10Ufloat {
                let packed = u32::from_le_bytes(data[start..start + 4].try_into().unwrap());
                // The packed channels share the half-float exponent. Restore the mantissa bits.
                texels.push([
                    half_to_f32(((packed & 0x7ff) << 4) as u16),
                    half_to_f32((((packed >> 11) & 0x7ff) << 4) as u16),
                    half_to_f32(((packed >> 22) << 5) as u16),
                    1.0,
                ]);
                continue;
            }
            texels.push(std::array::from_fn(|i| {
                if bytes_per_texel == 8 {
                    let at = start + i * 2;
                    half_to_f32(u16::from_le_bytes([data[at], data[at + 1]]))
                } else {
                    f32::from(data[start + i]) / 255.0
                }
            }));
        }
    }
    texels
}

/// A scene with a different colour in each quadrant and a bright square, times `gain`.
fn scene_plane(gain: f32) -> Plane {
    Plane::from_fn((SIZE, SIZE), |u, v| {
        let base: Colour = match (u > 0.5, v > 0.5) {
            (false, false) => [0.2, 0.2, 0.2],
            (true, false) => [0.8, 0.4, 0.1],
            (false, true) => [2.0, 1.0, 0.25],
            (true, true) => [0.05, 0.5, 1.5],
        };
        let spot = if (u - 0.3).abs() < 0.07 && (v - 0.3).abs() < 0.07 {
            4.0
        } else {
            0.0
        };
        base.map(|c| (c + spot) * gain)
    })
}

#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn retail_hdr_gpu_matches_the_cpu_reference() {
    verify_chain(WIDE_HDR_FORMAT);
}

#[test]
#[ignore = "requires a GPU with renderable packed HDR"]
fn retail_hdr_packed_gpu_matches_the_cpu_reference() {
    verify_chain(wgpu::TextureFormat::Rg11b10Ufloat);
}

fn verify_chain(format: wgpu::TextureFormat) {
    let wide = format == WIDE_HDR_FORMAT;
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = bevy::tasks::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("a GPU adapter");
    let (device, queue) = bevy::tasks::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("retail_hdr_test"),
        required_features: if wide {
            wgpu::Features::empty()
        } else {
            wgpu::Features::RG11B10UFLOAT_RENDERABLE
        },
        required_limits: adapter.limits(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .expect("a GPU device");
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);

    // The settings: every stage non-neutral, with bloom on.
    let block = HdrBlock {
        white: 1.0,
        saturation: 1.4,
        brightness: 1.2,
        contrast: 1.3,
        tint_amount: 0.5,
        tint_rgb: [1.0, 0.5, 0.25],
        eye_adapt_speed: 3.0,
        eye_adapt_strength: 3.0,
        bloom_threshold: 0.6,
        bloom_scale: 1.5,
        bloom_blur_radius: RADIUS as f32,
        receive_bloom_threshold: 0.9,
        ..HdrBlock::IMGS_CONSTRUCTOR_DEFAULT
    };
    let config = RetailHdrConfig {
        gamma: 2.0,
        // A packed request must not change wide stores on a device without packed support.
        use_64_bit_hdr_targets: false,
        ..RetailHdrConfig::default()
    };
    let t = 1.0 / 30.0;
    let mut hdr = RetailHdr::from_block(&block, &config, t);
    hdr.fade = bevy::math::Vec4::new(0.0, 0.0, 1.0, 0.0);
    hdr.curve.w = 0.25;
    assert_eq!((hdr.adapt.x, hdr.adapt.y), adapt_rates(&block, &config, t));
    let mut uniform = UniformBuffer::new(Vec::<u8>::new());
    uniform.write(&hdr).unwrap();
    let uniform_bytes = uniform.into_inner();
    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: uniform_bytes.len() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&uniform_buffer, 0, &uniform_bytes);

    use wgpu::TextureUsages as Usage;
    let chain_usage = Usage::RENDER_ATTACHMENT | Usage::TEXTURE_BINDING | Usage::COPY_SRC;
    let scene = texture(
        &device,
        (SIZE, SIZE),
        WIDE_HDR_FORMAT,
        Usage::TEXTURE_BINDING | Usage::COPY_DST,
    );
    let sizes = chain_sizes(SIZE, SIZE, true);
    let levels: Vec<_> = sizes[..sizes.len() - 1]
        .iter()
        .map(|&size| texture(&device, size, format, chain_usage | Usage::COPY_DST))
        .collect();
    let pair_size = sizes[sizes.len() - 1];
    let adapted = [
        texture(&device, pair_size, format, chain_usage),
        texture(&device, pair_size, format, chain_usage),
    ];
    let swap = texture(&device, bloom_size(SIZE, SIZE), format, chain_usage);
    let bloom = texture(&device, bloom_size(SIZE, SIZE), format, chain_usage);
    let display_target = texture(
        &device,
        (SIZE, SIZE),
        wgpu::TextureFormat::Rgba8Unorm,
        chain_usage,
    );
    let target = texture(
        &device,
        (SIZE, SIZE),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        Usage::RENDER_ATTACHMENT | Usage::COPY_SRC,
    );
    let presentation = texture(&device, (SIZE, SIZE), WIDE_HDR_FORMAT, chain_usage);
    let linear = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let nearest = device.create_sampler(&Default::default());

    let chain_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("chain"),
        entries: &[
            uniform_entry(),
            texture_entry(1),
            texture_entry(2),
            sampler_entry(3),
            sampler_entry(4),
        ],
    });
    let tonemap_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tonemap"),
        entries: &[
            uniform_entry(),
            texture_entry(1),
            texture_entry(2),
            texture_entry(3),
            sampler_entry(4),
        ],
    });
    let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fullscreen"),
        source: wgpu::ShaderSource::Wgsl(
            "struct Out { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
             @vertex fn vs(@builtin(vertex_index) i: u32) -> Out {
                 let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
                 var out: Out;
                 out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
                 out.uv = uv;
                 return out;
             }"
            .into(),
        ),
    });
    let pipeline = |fragment: &wgpu::ShaderModule,
                    entry: &str,
                    layout: &wgpu::BindGroupLayout,
                    format: wgpu::TextureFormat| {
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &vertex,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: fragment,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    };
    let chain_module = module(
        &device,
        "retail_hdr_chain.wgsl",
        include_str!("shaders/retail_hdr_chain.wgsl"),
        if wide {
            HashMap::new()
        } else {
            HashMap::from([(
                "RETAIL_HDR_PACKED".into(),
                naga_oil::compose::ShaderDefValue::Bool(true),
            )])
        },
    );
    let tonemap_module = module(
        &device,
        "retail_hdr_tonemap.wgsl",
        include_str!("shaders/retail_hdr_tonemap.wgsl"),
        HashMap::new(),
    );
    let chain_pipelines = PassKind::ALL
        .map(|kind| pipeline(&chain_module, kind.entry_point(), &chain_layout, format));
    let tonemap = pipeline(
        &tonemap_module,
        "display_fragment",
        &tonemap_layout,
        display_target.0.format(),
    );
    let hazard_target = texture(&device, (SIZE, SIZE), WIDE_HDR_FORMAT, chain_usage);
    let hazard_tonemap = pipeline(
        &tonemap_module,
        "display_fragment",
        &tonemap_layout,
        hazard_target.0.format(),
    );
    let present = pipeline(
        &tonemap_module,
        "present_fragment",
        &tonemap_layout,
        presentation.0.format(),
    );
    let blit = pipeline(
        &tonemap_module,
        "copy_display_fragment",
        &tonemap_layout,
        target.0.format(),
    );
    let copy = pipeline(
        &tonemap_module,
        "copy_display_fragment",
        &tonemap_layout,
        display_target.0.format(),
    );
    let chain_gpu = ChainGpu {
        layout: &chain_layout,
        linear: &linear,
        nearest: &nearest,
        pipelines: std::array::from_fn(|i| &chain_pipelines[i]),
        uniform: uniform_buffer.as_entire_binding(),
        offsets: &[0],
    };
    let view = |id: ChainTarget| match id {
        ChainTarget::Scene => &scene.1,
        ChainTarget::Level(level) => &levels[level].1,
        ChainTarget::Adapted(pair) => &adapted[pair].1,
        ChainTarget::BloomSwap => &swap.1,
        ChainTarget::Bloom => &bloom.1,
    };
    let upload = |plane: &Plane| {
        let bytes: Vec<u8> = plane
            .texels
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 1.0])
            .flat_map(|c| f32_to_half(c).to_le_bytes())
            .collect();
        queue.write_texture(
            scene.0.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * 8),
                rows_per_image: Some(SIZE),
            },
            extent((SIZE, SIZE)),
        );
    };
    let run = |valid: bool, current: usize| {
        let mut encoder = device.create_command_encoder(&Default::default());
        let plan = chain_plan(sizes.len(), valid, current);
        encode_chain(&device, &mut encoder, &chain_gpu, &plan, view);
        queue.submit([encoder.finish()]);
    };
    let compare =
        |what: &str, actual: &[[f32; 4]], lower: &Plane, upper: &Plane, channels: usize| {
            assert_eq!(actual.len(), lower.texels.len(), "{what} size");
            assert_eq!(actual.len(), upper.texels.len(), "{what} size");
            for (index, gpu) in actual.iter().enumerate() {
                let low = lower.texels[index];
                let high = upper.texels[index];
                for channel in 0..channels {
                    // Allow shader arithmetic error in addition to the per-store precision.
                    let tolerance = 3.0e-3 * high[channel].abs().max(1.0);
                    assert!(
                        (low[channel] - tolerance..=high[channel] + tolerance)
                            .contains(&gpu[channel]),
                        "{what} texel {index} channel {channel}: GPU {} vs CPU {}..={}",
                        gpu[channel],
                        low[channel],
                        high[channel]
                    );
                }
            }
        };
    let (lower, upper) = if wide {
        (ChainPrecision::Half, ChainPrecision::Half)
    } else {
        // Packed stores may truncate or round. Propagate both through the positive-weight passes.
        (
            ChainPrecision::PackedTruncated,
            ChainPrecision::PackedNearest,
        )
    };

    let store_probe = Plane::from_fn((SIZE, SIZE), |_, _| [-1.0, 0.0, 64992.0]);
    upload(&store_probe);
    run(false, 0);
    let stored = read_texture(&device, &queue, &levels[0].0);
    let expected = if wide {
        [-1.0, 0.0, 64992.0]
    } else {
        [0.0, 0.0, 64512.0]
    };
    assert!(stored.iter().all(|pixel| pixel[..3] == expected));

    // Frame 1: no adapted pair, so the last step becomes the pair (A.x = A.y = n).
    let first_scene = scene_plane(1.0);
    upload(&first_scene);
    run(false, 0);
    let first_low = cpu_frame(&first_scene, None, &hdr, lower);
    let first_high = cpu_frame(&first_scene, None, &hdr, upper);
    let pair = read_texture(&device, &queue, &adapted[1].0);
    compare("first pair", &pair, &first_low.pair, &first_high.pair, 3);
    for texel in &pair {
        assert_eq!(texel[0], texel[1], "the first-frame ratio is exactly 1");
    }
    compare(
        "first bloom",
        &read_texture(&device, &queue, &bloom.0),
        &first_low.bloom,
        &first_high.bloom,
        3,
    );

    // Frame 2: a darker scene. The adapt draw moves each channel with its own rate.
    let second_scene = scene_plane(0.4);
    upload(&second_scene);
    run(true, 1);
    let second_low = cpu_frame(&second_scene, Some(&first_low.pair), &hdr, lower);
    let second_high = cpu_frame(&second_scene, Some(&first_high.pair), &hdr, upper);
    let pair = read_texture(&device, &queue, &adapted[0].0);
    compare(
        "adapted pair",
        &pair,
        &second_low.pair,
        &second_high.pair,
        2,
    );
    assert!(
        pair.iter()
            .any(|texel| (texel[0] - texel[1]).abs() > 1.0e-3),
        "the two channels adapt at different rates"
    );
    assert!(
        second_low.bloom.texels.iter().any(|c| c[0] > 0.01),
        "the test scene blooms"
    );

    compare(
        "frame-two bloom",
        &read_texture(&device, &queue, &bloom.0),
        &second_low.bloom,
        &second_high.bloom,
        3,
    );

    // Inject an invalid measurement after the upstream finite-input guards.
    let plan = chain_plan(sizes.len(), true, 0);
    let adapt = plan
        .iter()
        .find(|step| step.kind == PassKind::Adapt)
        .expect("adapt draw");
    let ChainTarget::Level(source) = adapt.source else {
        panic!("the adapt draw reads the preceding luminance level");
    };
    let source = &levels[source].0;
    let invalid_texel = if wide {
        [0x00, 0x7e, 0, 0, 0, 0, 0, 0].as_slice()
    } else {
        [0xc1, 0x07, 0, 0].as_slice()
    };
    let pattern = invalid_texel.repeat((source.width() * source.height()) as usize);
    queue.write_texture(
        source.as_image_copy(),
        &pattern,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(source.width() * if wide { 8 } else { 4 }),
            rows_per_image: Some(source.height()),
        },
        extent((source.width(), source.height())),
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    encode_chain(
        &device,
        &mut encoder,
        &chain_gpu,
        std::slice::from_ref(adapt),
        view,
    );
    queue.submit([encoder.finish()]);
    for (held, old) in read_texture(&device, &queue, &adapted[1].0)
        .iter()
        .zip(&pair)
    {
        assert_eq!(
            held[..2],
            old[..2],
            "an invalid measurement holds both channels"
        );
    }

    // Use the same display store and transfer as the render-world shaders.
    let output = |pair: usize, first_pass: &wgpu::RenderPipeline, float: bool| {
        let mut encoder = device.create_command_encoder(&Default::default());
        let passes = [
            (first_pass, &scene.1, &display_target.1),
            (&present, &display_target.1, &presentation.1),
            (&blit, &presentation.1, &target.1),
        ];
        let hazard_pass = [(first_pass, &scene.1, &hazard_target.1)];
        let passes: &[_] = if float { &hazard_pass } else { &passes };
        for &(pipeline, source, destination) in passes {
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("retail_hdr_test_output"),
                layout: &tonemap_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&bloom.1),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&adapted[pair].1),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&linear),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("retail_hdr_test_output"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: destination,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[0]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
        read_texture(
            &device,
            &queue,
            if float { &hazard_target.0 } else { &target.0 },
        )
    };
    let pixels = output(0, &tonemap, false);
    assert_eq!(pixels, read_texture(&device, &queue, &display_target.0));
    let copied = output(0, &copy, false);
    for (pixel, scene) in copied.iter().zip(&second_scene.texels) {
        for channel in 0..3 {
            let expected = (scene[channel].clamp(0.0, 1.0) * 255.0).round() / 255.0;
            assert!((pixel[channel] - expected).abs() <= f32::EPSILON);
        }
    }
    // The GPU pair, which the tonemap samples.
    let gpu_bloom = Plane {
        width: bloom.0.width(),
        height: bloom.0.height(),
        texels: read_texture(&device, &queue, &bloom.0)
            .iter()
            .map(|t| [t[0], t[1], t[2]])
            .collect(),
    };
    let gpu_pair = Plane {
        width: pair_size.0,
        height: pair_size.1,
        texels: pair.iter().map(|t| [t[0], t[1], t[2]]).collect(),
    };
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (u, v) = (
                (x as f32 + 0.5) / SIZE as f32,
                (y as f32 + 0.5) / SIZE as f32,
            );
            let adapted = gpu_pair.linear(u, v);
            let display = hdr.display_value(
                second_scene.texel(i64::from(x), i64::from(y)),
                [adapted[0], adapted[1]],
                gpu_bloom.linear(u, v),
            );
            let gpu = pixels[(y * SIZE + x) as usize];
            for channel in 0..3 {
                let expected = (display[channel].clamp(0.0, 1.0) * 255.0).round();
                let actual = (gpu[channel] * 255.0).round();
                // Shader arithmetic and texture filtering can move the result across a byte boundary.
                assert!(
                    (actual - expected).abs() <= 1.0,
                    "pixel ({x}, {y}) channel {channel}: GPU byte {actual} vs CPU byte {expected}",
                );
            }
        }
    }

    // Many frames of inputs a broken shader upstream could produce: nothing that is not finite
    // may reach a target of the chain or the screen, and a normal scene recovers afterwards.
    const ZERO: u16 = 0x0000;
    const LARGEST: u16 = 0x7bff;
    const INFINITY: u16 = 0x7c00;
    const NOT_A_NUMBER: u16 = 0x7e00;
    let upload_halves = |value: &dyn Fn(u32, u32) -> u16| {
        let mut bytes = Vec::new();
        for y in 0..SIZE {
            for x in 0..SIZE {
                let colour = value(x, y);
                for half in [colour, colour, colour, f32_to_half(1.0)] {
                    bytes.extend_from_slice(&half.to_le_bytes());
                }
            }
        }
        queue.write_texture(
            scene.0.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * 8),
                rows_per_image: Some(SIZE),
            },
            extent((SIZE, SIZE)),
        );
    };
    let tonemapped = |pair| output(pair, &tonemap, false);
    // The first pair of the run is made from a NaN frame, then the pair adapts through the rest.
    let hazards = [
        NOT_A_NUMBER,
        ZERO,
        INFINITY,
        LARGEST,
        0xfc00, // negative infinity
        f32_to_half(0.5),
    ];
    let mut current = 0;
    for frame in 0..36_usize {
        let hazard = hazards[frame % hazards.len()];
        // Whole hazard frames, then frames with the hazard in one texel of eight.
        let sparse = frame >= hazards.len() * 3;
        upload_halves(&|x, y| {
            if sparse && (x + 3 * y) % 8 != 0 {
                f32_to_half(0.25)
            } else {
                hazard
            }
        });
        run(frame != 0, current);
        let next = 1 - current;
        for (what, texture) in [("adapted pair", &adapted[next].0), ("bloom", &bloom.0)] {
            for texel in read_texture(&device, &queue, texture) {
                assert!(
                    texel.iter().all(|value| value.is_finite()),
                    "frame {frame}, input {hazard:#06x}: {what} holds {texel:?}"
                );
            }
        }
        for pixel in output(next, &hazard_tonemap, true) {
            assert!(
                pixel[..3].iter().all(|value| (0.0..=1.0).contains(value)),
                "frame {frame}, input {hazard:#06x}: the screen holds {pixel:?}"
            );
        }
        current = next;
    }
    // A steady scene after the hazards. The pair comes back from the largest values by its
    // adaptation rates, so the screen may stay washed for a while; no single frame may flash,
    // and the scene must be there at the end.
    upload(&first_scene);
    let mean = |pixels: &[[f32; 4]]| {
        pixels.iter().map(|pixel| pixel[1]).sum::<f32>() / pixels.len() as f32
    };
    let mut means = Vec::new();
    let mut last = Vec::new();
    for _ in 0..600 {
        run(true, current);
        current = 1 - current;
        last = tonemapped(current);
        means.push(mean(&last));
    }
    for (frame, three) in means.windows(3).enumerate() {
        assert!(
            (three[1] - three[0]).abs() < 0.05 || (three[2] - three[0]).abs() > 0.01,
            "a flash at steady frame {frame}: {three:?}"
        );
    }
    for pair in means[means.len() - 60..].windows(2) {
        assert!((pair[1] - pair[0]).abs() < 0.01, "not settled: {pair:?}");
    }
    let settled = mean(&last);
    assert!(
        last.iter().any(|pixel| (pixel[1] - settled).abs() > 0.05),
        "the recovered frame is solid, mean {settled}; pair {:?}",
        read_texture(&device, &queue, &adapted[current].0)
    );

    let error = bevy::tasks::block_on(validation.pop());
    assert!(error.is_none(), "GPU validation error: {error:?}");
}
