//! Headless Metal validation of the actual cinematic fragment shader.
//!
//! Run with `--output <directory>`. This is a synthetic numerical probe, not a
//! Skyrim capture or a retail-parity test. Production uses an Rgba16Float main
//! target; this probe retains that input format but writes Rgba32Float so HDR
//! passthrough and shader error remain visible without output quantization.

use bevy::{math::Vec3, tasks::block_on};
use color_eyre::{
    Result,
    eyre::{Context, bail, ensure},
};
use engine::color_pipeline::SkyrimCinematicPost;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    num::NonZeroU64,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wgpu::util::DeviceExt;

const PRODUCTION_FRAGMENT: &str = include_str!("../src/shaders/cinematic_post.wgsl");
const VERTEX_IMPORT: &str =
    "#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput";
// Same interface and triangle as Bevy 0.19 fullscreen_vertex_shader/fullscreen.wgsl.
const VERTEX: &str = r#"
struct FullscreenVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}
@vertex
fn probe_vertex(@builtin(vertex_index) index: u32) -> FullscreenVertexOutput {
    let uv = vec2<f32>(f32(index >> 1u), f32(index & 1u)) * 2.0;
    let position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return FullscreenVertexOutput(position, uv);
}
"#;
const ABSOLUTE_TOLERANCE: f64 = 0.000_05;
const RELATIVE_TOLERANCE: f64 = 0.000_05;
const ROW_BYTES: u32 = 256;

struct Case {
    name: &'static str,
    input: [f32; 4],
    settings: SkyrimCinematicPost,
    manual_expected: Option<[f64; 4]>,
    preserve_chromatic_ratios: bool,
}

fn curve(operator: u32, coefficient: f32) -> SkyrimCinematicPost {
    SkyrimCinematicPost {
        tone_operator: operator,
        tone_coefficient: coefficient,
        sampled_luminance: 0.25,
        target_luminance: 0.25,
        ..Default::default()
    }
}

fn approved_grade(mut settings: SkyrimCinematicPost) -> SkyrimCinematicPost {
    // Fixed known fixture values. No database or IMGS resolution is exercised.
    settings.enabled = 1.0;
    settings.saturation = 1.66;
    settings.brightness = 1.38;
    settings.contrast = 1.5;
    settings.tint_amount = 0.56;
    settings.tint_rgb = Vec3::new(0.88, 0.81, 0.72);
    settings.pivot = 0.18;
    settings.linear_domain = 0;
    settings
}

fn cases() -> Vec<Case> {
    let mut scaled = curve(1, 0.25);
    scaled.target_luminance = 0.375;
    vec![
        Case {
            name: "disabled_curve_and_grade_preserve_hdr",
            input: [4.0, 2.0, 0.5, 0.375],
            settings: SkyrimCinematicPost::default(),
            manual_expected: Some([4.0, 2.0, 0.5, 0.375]),
            preserve_chromatic_ratios: true,
        },
        Case {
            name: "reinhard_unit_neutral_maps_to_half",
            input: [1.0, 1.0, 1.0, 0.625],
            settings: curve(1, 0.0),
            manual_expected: Some([0.5, 0.5, 0.5, 0.625]),
            preserve_chromatic_ratios: true,
        },
        Case {
            name: "reinhard_chromatic_scalar_and_exposure_ratio",
            input: [0.25, 0.5, 2.0, 0.375],
            settings: scaled,
            manual_expected: None,
            preserve_chromatic_ratios: true,
        },
        Case {
            name: "reinhard_black_is_finite",
            input: [0.0, 0.0, 0.0, 0.75],
            settings: curve(1, 0.749_611_14),
            manual_expected: Some([0.0, 0.0, 0.0, 0.75]),
            preserve_chromatic_ratios: false,
        },
        Case {
            name: "filmic_below_black_offset_is_zero",
            input: [0.000_976_562_5, 0.000_976_562_5, 0.000_976_562_5, 0.5],
            settings: curve(2, 0.749_611_14),
            manual_expected: Some([0.0, 0.0, 0.0, 0.5]),
            preserve_chromatic_ratios: false,
        },
        Case {
            name: "filmic_black_is_finite",
            input: [0.0, 0.0, 0.0, 0.875],
            settings: curve(2, 1.306_991_1),
            manual_expected: Some([0.0, 0.0, 0.0, 0.875]),
            preserve_chromatic_ratios: false,
        },
        Case {
            name: "reinhard_white_derived_coefficient",
            input: [0.25, 0.5, 2.0, 0.25],
            settings: curve(1, 0.749_611_14),
            manual_expected: None,
            preserve_chromatic_ratios: true,
        },
        Case {
            name: "filmic_same_coefficient_checks_other_shader_branch",
            input: [0.25, 0.5, 2.0, 0.375],
            settings: curve(2, 0.749_611_14),
            manual_expected: None,
            preserve_chromatic_ratios: true,
        },
        Case {
            name: "filmic_white_derived_coefficient",
            input: [0.25, 0.5, 2.0, 0.625],
            settings: curve(2, 1.306_991_1),
            manual_expected: None,
            preserve_chromatic_ratios: true,
        },
        Case {
            name: "reinhard_then_approved_srgb_grade",
            input: [0.0625, 0.25, 0.5, 0.375],
            settings: approved_grade(curve(1, 0.749_611_14)),
            manual_expected: None,
            preserve_chromatic_ratios: false,
        },
        Case {
            name: "filmic_then_approved_srgb_grade",
            input: [0.0625, 0.25, 0.5, 0.625],
            settings: approved_grade(curve(2, 1.306_991_1)),
            manual_expected: None,
            preserve_chromatic_ratios: false,
        },
        Case {
            name: "approved_srgb_grade_without_scalar_curve",
            input: [0.0625, 0.25, 0.5, 0.25],
            settings: approved_grade(SkyrimCinematicPost::default()),
            manual_expected: None,
            preserve_chromatic_ratios: false,
        },
        Case {
            name: "srgb_grade_lower_transfer_branch",
            input: [0.001_953_125, 0.000_976_562_5, 0.0, 0.75],
            settings: SkyrimCinematicPost {
                enabled: 1.0,
                ..Default::default()
            },
            manual_expected: Some([0.001_953_125, 0.000_976_562_5, 0.0, 0.75]),
            preserve_chromatic_ratios: false,
        },
    ]
}

/// Independent f64 evaluation of the documented conceptual transform. This does
/// not reuse production CPU helpers, coefficient resolution, or shader parsing.
fn reference(case: &Case) -> [f64; 4] {
    let settings = &case.settings;
    let mut rgb = [case.input[0], case.input[1], case.input[2]].map(f64::from);
    let luminance = |rgb: [f64; 3]| rgb[0] * 0.2125 + rgb[1] * 0.7154 + rgb[2] * 0.0721;
    if settings.tone_operator != 0 {
        let y = luminance(rgb).max(0.00001);
        let z = y * f64::from(settings.target_luminance) / f64::from(settings.sampled_luminance);
        let coefficient = f64::from(settings.tone_coefficient);
        let q = if settings.tone_operator == 2 {
            let x = (z - 0.004).max(0.0);
            let h = (6.2 * x * x + 0.5 * x) / (6.2 * x * x + 1.7 * x + 0.06);
            coefficient * h.powf(2.2)
        } else {
            (z + coefficient * z * z) / (1.0 + z)
        };
        rgb = rgb.map(|component| component * q / y);
    }
    if settings.enabled >= 0.5 {
        if settings.linear_domain == 0 {
            rgb = rgb.map(|component| {
                let component = component.max(0.0);
                if component <= 0.003_130_8 {
                    12.92 * component
                } else {
                    1.055 * component.powf(1.0 / 2.4) - 0.055
                }
            });
        }
        let original_luminance = luminance(rgb);
        let saturation = f64::from(settings.saturation);
        let tint = f64::from(settings.tint_amount);
        let tint_rgb = settings.tint_rgb.to_array();
        for (component, tint_component) in rgb.iter_mut().zip(tint_rgb) {
            let saturated = original_luminance + saturation * (*component - original_luminance);
            let tinted =
                saturated * (1.0 - tint) + original_luminance * f64::from(tint_component) * tint;
            *component = (f64::from(settings.pivot)
                + f64::from(settings.contrast)
                    * (f64::from(settings.brightness) * tinted - f64::from(settings.pivot)))
            .clamp(0.0, 1.0);
        }
        if settings.linear_domain == 0 {
            rgb = rgb.map(|component| {
                if component <= 0.04045 {
                    component / 12.92
                } else {
                    ((component + 0.055) / 1.055).powf(2.4)
                }
            });
        }
    }
    [rgb[0], rgb[1], rgb[2], f64::from(case.input[3])]
}

fn uniform_bytes(settings: &SkyrimCinematicPost) -> [u8; 64] {
    let mut bytes = [0; 64];
    for (offset, value) in [
        (0, settings.brightness),
        (4, settings.contrast),
        (8, settings.tint_amount),
        (12, settings.pivot),
        (16, settings.tint_rgb.x),
        (20, settings.tint_rgb.y),
        (24, settings.tint_rgb.z),
        (28, settings.enabled),
        (32, settings.saturation),
        (44, settings.tone_coefficient),
        (48, settings.sampled_luminance),
        (52, settings.target_luminance),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[36..40].copy_from_slice(&settings.linear_domain.to_le_bytes());
    bytes[40..44].copy_from_slice(&settings.tone_operator.to_le_bytes());
    bytes
}

fn input_bytes(input: [f32; 4]) -> Result<[u8; 8]> {
    let mut bytes = [0; 8];
    for (channel, component) in input.into_iter().enumerate() {
        ensure!(
            component.is_finite() && component >= 0.0,
            "invalid fixture component"
        );
        let half = if component == 0.0 {
            0u16
        } else {
            let bits = component.to_bits();
            let exponent = ((bits >> 23) & 0xff) as i32 - 127;
            ensure!(
                (-14..=15).contains(&exponent) && bits & 0x1fff == 0,
                "fixture component {component} must be exactly representable as normal binary16"
            );
            (((exponent + 15) as u16) << 10) | ((bits & 0x7f_ffff) >> 13) as u16
        };
        bytes[channel * 2..channel * 2 + 2].copy_from_slice(&half.to_le_bytes());
    }
    Ok(bytes)
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

impl Gpu {
    async fn new(source: &str) -> Result<(Self, wgpu::AdapterInfo)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .wrap_err("request Metal adapter")?;
        let info = adapter.get_info();
        ensure!(
            info.backend == wgpu::Backend::Metal,
            "probe requires backend=Metal"
        );
        ensure!(
            adapter
                .get_texture_format_features(wgpu::TextureFormat::Rgba32Float)
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT),
            "Metal adapter cannot render to Rgba32Float"
        );
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("retail_curve_numerical_probe"),
                ..Default::default()
            })
            .await
            .wrap_err("request probe device")?;
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe_fragment_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(64),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("probe_pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("production_cinematic_fragment_with_probe_vertex"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(source)),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("retail_curve_float_output"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("probe_vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        if let Some(error) = error_scope.pop().await {
            bail!("probe pipeline validation: {error}");
        }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        Ok((
            Self {
                device,
                queue,
                layout,
                pipeline,
                sampler,
            },
            info,
        ))
    }

    fn render(&self, case: &Case) -> Result<[f32; 4]> {
        let size = wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        };
        let input = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe_half_float_input"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            input.as_image_copy(),
            &input_bytes(case.input)?,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: Some(1),
            },
            size,
        );
        let output = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe_full_float_output"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let input_view = input.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("probe_settings"),
                contents: &uniform_bytes(&case.settings),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("probe_bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe_readback"),
            size: u64::from(ROW_BYTES),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("production_fragment_numerical_probe"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ROW_BYTES),
                    rows_per_image: Some(1),
                },
            },
            size,
        );
        let submission = self.queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::sync_channel(1);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(10)),
            })
            .wrap_err("wait for probe GPU submission")?;
        receiver
            .recv_timeout(Duration::from_secs(1))
            .wrap_err("wait for probe readback callback")?
            .wrap_err("map probe readback")?;
        let mapped = readback.slice(..).get_mapped_range();
        let output = std::array::from_fn(|channel| {
            f32::from_le_bytes(mapped[channel * 4..channel * 4 + 4].try_into().unwrap())
        });
        drop(mapped);
        readback.unmap();
        Ok(output)
    }
}

fn settings_evidence(settings: &SkyrimCinematicPost) -> Value {
    json!({
        "brightness": settings.brightness,
        "contrast": settings.contrast,
        "tint_amount": settings.tint_amount,
        "pivot": settings.pivot,
        "tint_rgb": settings.tint_rgb.to_array(),
        "enabled": settings.enabled,
        "saturation": settings.saturation,
        "linear_domain": settings.linear_domain,
        "tone_operator": settings.tone_operator,
        "tone_coefficient": settings.tone_coefficient,
        "sampled_luminance": settings.sampled_luminance,
        "target_luminance": settings.target_luminance,
    })
}

async fn run(output: &Path) -> Result<Value> {
    ensure!(
        PRODUCTION_FRAGMENT.matches(VERTEX_IMPORT).count() == 1,
        "expected exactly one production vertex import"
    );
    let source = PRODUCTION_FRAGMENT.replace(VERTEX_IMPORT, VERTEX);
    let parsed = wgpu::naga::front::wgsl::parse_str(&source)
        .map_err(|error| color_eyre::eyre::eyre!(error.emit_to_string(&source)))?;
    let uniform = parsed.types.iter().find_map(|(_, ty)| {
        (ty.name.as_deref() == Some("SkyrimCinematicPost")).then_some(&ty.inner)
    });
    let Some(wgpu::naga::TypeInner::Struct { members, span }) = uniform else {
        bail!("production cinematic uniform is missing");
    };
    ensure!(*span == 64, "production uniform size changed");
    ensure!(
        members
            .iter()
            .map(|member| member.offset)
            .collect::<Vec<_>>()
            == [0, 4, 8, 12, 16, 28, 32, 36, 40, 44, 48, 52],
        "production uniform offsets changed"
    );
    std::fs::write(output.join("compiled-probe.wgsl"), &source)?;
    let (gpu, adapter) = Gpu::new(&source).await?;
    let mut results = Vec::new();
    for case in cases() {
        let expected = reference(&case);
        if let Some(manual) = case.manual_expected {
            ensure!(
                expected
                    .iter()
                    .zip(manual)
                    .all(|(actual, expected)| (actual - expected).abs() < 0.000_001),
                "independent CPU reference disagrees with manual point {}",
                case.name
            );
        }
        let actual = gpu
            .render(&case)
            .wrap_err_with(|| format!("render case {}", case.name))?;
        let finite = actual.iter().all(|component| component.is_finite());
        let errors: [f64; 4] =
            std::array::from_fn(|channel| (f64::from(actual[channel]) - expected[channel]).abs());
        let numeric_match = finite
            && errors.iter().enumerate().all(|(channel, error)| {
                *error <= ABSOLUTE_TOLERANCE + RELATIVE_TOLERANCE * expected[channel].abs()
            });
        let alpha_unchanged = actual[3].to_bits() == case.input[3].to_bits();
        let ratio_spread = if case.preserve_chromatic_ratios {
            let gains: Vec<f64> = (0..3)
                .filter(|channel| case.input[*channel] > 0.0)
                .map(|channel| f64::from(actual[channel]) / f64::from(case.input[channel]))
                .collect();
            let min = gains.iter().copied().fold(f64::INFINITY, f64::min);
            let max = gains.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            Some((max - min).abs() / max.abs().max(0.000_001))
        } else {
            None
        };
        let ratios_preserved =
            ratio_spread.is_none_or(|spread| spread.is_finite() && spread <= RELATIVE_TOLERANCE);
        results.push(json!({
            "name": case.name,
            "input_rgba": case.input,
            "input_exactly_representable_binary16": true,
            "settings": settings_evidence(&case.settings),
            "uniform_le_bytes": uniform_bytes(&case.settings).as_slice(),
            "expected_f64": expected,
            "manual_expected": case.manual_expected,
            "gpu_rgba": actual,
            "gpu_rgba_f32_bits": actual.map(f32::to_bits),
            "absolute_errors": errors,
            "finite": finite,
            "alpha_bitwise_unchanged": alpha_unchanged,
            "relative_chromatic_gain_spread": ratio_spread,
            "chromatic_ratios_preserved": ratios_preserved,
            "passed": numeric_match && alpha_unchanged && ratios_preserved,
        }));
    }
    let passed = results.iter().all(|case| case["passed"] == true);
    Ok(json!({
        "kind": "synthetic-retail-curve-gpu-numerical",
        "retail_parity": false,
        "passed": passed,
        "adapter": { "backend": format!("{:?}", adapter.backend), "name": adapter.name, "device_type": format!("{:?}", adapter.device_type), "driver": adapter.driver, "driver_info": adapter.driver_info },
        "production_fragment": "crates/engine/src/shaders/cinematic_post.wgsl",
        "compiled_production_fragment_sha256": format!("{:x}", Sha256::digest(PRODUCTION_FRAGMENT.as_bytes())),
        "assembled_shader_sha256": format!("{:x}", Sha256::digest(source.as_bytes())),
        "vertex_change": "only Bevy fullscreen vertex import replaced with equivalent interface and triangle",
        "input_format": "Rgba16Float",
        "output_format": "Rgba32Float",
        "production_main_target_format": "Rgba16Float",
        "production_output_quantization_exercised": false,
        "target_size": [1, 1],
        "reference": "independent f64 conceptual equations; manual HDR passthrough, neutral half, black and transfer-branch points",
        "tolerance": { "absolute": ABSOLUTE_TOLERANCE, "relative": RELATIVE_TOLERANCE, "rule": "abs(gpu-reference) <= absolute + relative*abs(reference); alpha must match bits exactly", "reason": "allows f32 arithmetic and Metal pow differences against f64; excludes input/output u8 quantization" },
        "coefficient_fixtures": "0.74961114 Reinhard and 1.3069911 filmic independently computed from White=1.05 and static white scales=1.1/1.4; this probe bypasses record loading",
        "limits": "synthetic constant-texture inputs only; does not validate Bevy scheduling, scene composition, camera exposure, record loading, native effective branch, GPU parameter upload, adaptation, bloom, native input domain or final native exponent; real screenshots remain required",
        "cases": results,
    }))
}

fn output_directory() -> Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    ensure!(
        args.next().as_deref() == Some("--output"),
        "usage: retail_curve_probe --output <directory>"
    );
    let output = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| color_eyre::eyre::eyre!("--output requires a directory"))?;
    ensure!(
        args.next().is_none(),
        "usage: retail_curve_probe --output <directory>"
    );
    Ok(output)
}

fn main() -> ExitCode {
    let output = match output_directory() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("{error:#}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = std::fs::create_dir_all(&output) {
        eprintln!("create {}: {error}", output.display());
        return ExitCode::FAILURE;
    }
    let receipt_path = output.join("probe.json");
    if receipt_path.exists()
        && let Err(error) = std::fs::remove_file(&receipt_path)
    {
        eprintln!("remove stale {}: {error}", receipt_path.display());
        return ExitCode::FAILURE;
    }
    let mut receipt = match block_on(run(&output)) {
        Ok(receipt) => receipt,
        Err(error) => {
            json!({ "kind": "synthetic-retail-curve-gpu-numerical", "retail_parity": false, "passed": false, "error": format!("{error:#}"), "requested_backend": "Metal", "compiled_production_fragment_sha256": format!("{:x}", Sha256::digest(PRODUCTION_FRAGMENT.as_bytes())) })
        }
    };
    receipt["unix_timestamp"] = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
        .into();
    receipt["command"] = json!(std::env::args().collect::<Vec<_>>());
    let passed = receipt["passed"] == true;
    let write = serde_json::to_vec_pretty(&receipt)
        .map_err(color_eyre::eyre::Error::from)
        .and_then(|bytes| {
            std::fs::write(&receipt_path, bytes).map_err(color_eyre::eyre::Error::from)
        });
    if let Err(error) = write {
        eprintln!("write {}: {error:#}", receipt_path.display());
        return ExitCode::FAILURE;
    }
    println!(
        "{}",
        json!({ "passed": passed, "receipt": receipt_path, "cases": receipt["cases"].as_array().map(Vec::len), "error": receipt["error"] })
    );
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
