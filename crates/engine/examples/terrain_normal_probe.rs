//! Headless Metal check of the production terrain normal convention (#963).
//!
//! Run with `--output <directory>`. The probe opens no window. It renders a synthetic
//! DirectX-convention dome normal map through the production `terrain_normal.wgsl` (the shared
//! sign and tangent frame of both terrain shaders) on a flat top-down terrain tile, lights it with
//! a sun due north and due east at 30 degrees, and compares each pixel with an independent f64
//! Lambert reference. It runs the shipped sign and the opposite sign. The opposite sign is the
//! negative control: it must fail the north sun and pass the east sun.
//!
//! The reference does not use a tangent frame or a sign. It displaces the plane by the dome relief
//! and takes the cross product of the surface partials, with the plane given the production mesh
//! mapping (U along world +X, V along world -Z). `terrain_normal::the_frame_follows_the_mesh_texture_axes`
//! checks that mapping against `build_terrain_quadrant_mesh`.
//!
//! This is a synthetic probe of the shader's convention, not a Skyrim capture or retail parity.

use bevy::tasks::block_on;
use color_eyre::{
    Result,
    eyre::{Context, bail, ensure},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::mpsc,
    time::Duration,
};
use wgpu::util::DeviceExt;

const PRODUCTION_MODULE: &str = include_str!("../src/shaders/terrain_normal.wgsl");
const SIZE: u32 = 64;
const SCALE: u32 = 4;
const TOLERANCE: f64 = 0.02;
const FAIL_FLOOR: f64 = 0.1;
/// World length of one texture unit. Any positive value gives the same normals.
const LENGTH: f64 = 1.0;

const PROBE: &str = r#"
struct Probe { light: vec4<f32> }
@group(0) @binding(0) var dome: texture_2d<f32>;
@group(0) @binding(1) var<uniform> probe: Probe;

struct TileOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// One full-screen triangle over a flat terrain tile seen from above with north up. Texture
// coordinate U grows along world +X (right) and V along world -Z (north, up the screen), as the
// production terrain mesh lays them out. V = 0 is the south edge, so image row 0 is at the bottom.
@vertex
fn probe_vertex(@builtin(vertex_index) index: u32) -> TileOut {
    let ndc = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u)) * 2.0 - 1.0;
    return TileOut(vec4<f32>(ndc, 0.0, 1.0), ndc * 0.5 + 0.5);
}

@fragment
fn probe_fragment(in: TileOut) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(dome));
    let texel = vec2<i32>(clamp(floor(in.uv * size), vec2<f32>(0.0), size - 1.0));
    let stored = textureLoad(dome, texel, 0);
    let normal = normalize(terrain_tbn(vec3<f32>(0.0, 1.0, 0.0)) * terrain_tangent_normal(stored.xy));
    return vec4<f32>(max(dot(normal, probe.light.xyz), 0.0), normal);
}
"#;

type Vec3 = [f64; 3];

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(a: Vec3) -> Vec3 {
    let length = dot(a, a).sqrt();
    a.map(|component| component / length)
}

/// Sun direction (toward the sun) at 30 degrees elevation. `toward` is a horizontal world vector:
/// north is runtime -Z (Creation +Y), east is +X.
fn sun(toward: Vec3) -> Vec3 {
    let elevation = 30.0_f64.to_radians();
    let flat = normalize([toward[0], 0.0, toward[2]]);
    [
        flat[0] * elevation.cos(),
        elevation.sin(),
        flat[2] * elevation.cos(),
    ]
}

/// A dome of radius 0.4 texture units about the middle, 0.15 texture lengths high.
fn relief_gradient(u: f64, v: f64) -> (f64, f64) {
    let radius2 = 0.4_f64.powi(2);
    if ((u - 0.5).powi(2) + (v - 0.5).powi(2)) >= radius2 {
        return (0.0, 0.0);
    }
    let slope = -0.15 * LENGTH * 2.0 / radius2;
    (slope * (u - 0.5), slope * (v - 0.5))
}

/// The true world normal of the displaced tile at texture coordinate (u, v). The tile is
/// P(u, v) = (L u, 0, -L v) plus the relief along +Y; the normal is the cross product of its
/// partials. No tangent frame and no sign are involved.
fn true_normal(u: f64, v: f64) -> Vec3 {
    let (hu, hv) = relief_gradient(u, v);
    normalize(cross([LENGTH, hu, 0.0], [0.0, hv, -LENGTH]))
}

/// The texel a DirectX-convention map stores for that relief: red along +U, green along +V (down
/// the image), blue the rest. These are the surface tilts in the frame (dP/du, dP/dv, n).
fn directx_texel(u: f64, v: f64) -> [u8; 4] {
    let (hu, hv) = relief_gradient(u, v);
    let tangent = normalize([-hu, -hv, LENGTH]);
    let encode = |value: f64| ((0.5 + 0.5 * value) * 255.0).round() as u8;
    [
        encode(tangent[0]),
        encode(tangent[1]),
        encode(tangent[2]),
        255,
    ]
}

/// Texture-space coordinate of the centre of texel (column, row). Row 0 is V = 0.
fn centre(column: u32, row: u32) -> (f64, f64) {
    (
        (f64::from(column) + 0.5) / f64::from(SIZE),
        (f64::from(row) + 0.5) / f64::from(SIZE),
    )
}

fn dome_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for row in 0..SIZE {
        for column in 0..SIZE {
            let (u, v) = centre(column, row);
            bytes.extend(directx_texel(u, v));
        }
    }
    bytes
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    dome: wgpu::TextureView,
}

impl Gpu {
    async fn new() -> Result<(Self, wgpu::AdapterInfo)> {
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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("terrain_normal_probe"),
                ..Default::default()
            })
            .await
            .wrap_err("request probe device")?;
        let size = wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        };
        // Linear, as the engine loads terrain normal maps.
        let dome = device.create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: Some("probe_dome_normal_map"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &dome_bytes(),
        );
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("probe_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let dome = dome.create_view(&Default::default());
        Ok((
            Self {
                device,
                queue,
                layout,
                dome,
            },
            info,
        ))
    }

    /// Render the dome with `module` (the production source, possibly with the sign replaced) and
    /// read back (lambert, normal) per pixel, rows top to bottom.
    async fn render(&self, module: &str, light: Vec3) -> Result<Vec<[f32; 4]>> {
        let source = format!(
            "{}\n{PROBE}",
            module.replace("#define_import_path engine::terrain_normal", "")
        );
        let error_scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("production_terrain_normal_with_probe"),
                source: wgpu::ShaderSource::Wgsl(Cow::Owned(source)),
            });
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("probe_pipeline_layout"),
                bind_group_layouts: &[Some(&self.layout)],
                immediate_size: 0,
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("terrain_normal_probe"),
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
                    entry_point: Some("probe_fragment"),
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
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("probe_light"),
                contents: bytemuck::cast_slice(&[
                    light[0] as f32,
                    light[1] as f32,
                    light[2] as f32,
                    0.0,
                ]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("probe_bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.dome),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let size = wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        };
        let output = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe_output"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output.create_view(&Default::default());
        let row_bytes = SIZE * 16;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe_readback"),
            size: u64::from(row_bytes * SIZE),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terrain_normal_probe_pass"),
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
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(SIZE),
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
                timeout: Some(Duration::from_secs(30)),
            })
            .wrap_err("wait for probe GPU submission")?;
        receiver
            .recv_timeout(Duration::from_secs(1))
            .wrap_err("wait for probe readback callback")?
            .wrap_err("map probe readback")?;
        let mapped = readback.slice(..).get_mapped_range();
        let pixels = mapped
            .as_chunks::<16>()
            .0
            .iter()
            .map(|texel| {
                std::array::from_fn(|channel| {
                    f32::from_le_bytes(texel[channel * 4..channel * 4 + 4].try_into().unwrap())
                })
            })
            .collect();
        drop(mapped);
        readback.unmap();
        Ok(pixels)
    }
}

/// Lambert per pixel and true normal per pixel, rows top to bottom. Pixel row `py` (down the
/// screen) is V = 1 - (py + 0.5) / SIZE, because V grows up the screen (north).
fn reference(light: Vec3) -> Vec<[f64; 4]> {
    let mut pixels = Vec::new();
    for py in 0..SIZE {
        for px in 0..SIZE {
            let (u, v) = centre(px, SIZE - 1 - py);
            let normal = true_normal(u, v);
            pixels.push([dot(normal, light).max(0.0), normal[0], normal[1], normal[2]]);
        }
    }
    pixels
}

fn compare(gpu: &[[f32; 4]], reference: &[[f64; 4]]) -> (f64, f64) {
    let mut worst_light = 0.0_f64;
    let mut worst_normal = 0.0_f64;
    for (gpu, reference) in gpu.iter().zip(reference) {
        worst_light = worst_light.max((f64::from(gpu[0]) - reference[0]).abs());
        for axis in 1..4 {
            worst_normal = worst_normal.max((f64::from(gpu[axis]) - reference[axis]).abs());
        }
    }
    (worst_light, worst_normal)
}

fn grey(pixels: impl Iterator<Item = f64>) -> Vec<u8> {
    pixels
        .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect()
}

/// A strip of 4x-upscaled lit tiles: before (opposite sign), after (shipped sign), reference.
fn write_strip(path: &Path, tiles: [&[f64]; 3]) -> Result<()> {
    let width = SIZE * SCALE;
    let mut image = image::GrayImage::new(width * 3 + 8, width);
    image.fill(128);
    for (index, tile) in tiles.into_iter().enumerate() {
        let values = grey(tile.iter().copied());
        for y in 0..width {
            for x in 0..width {
                let source = ((y / SCALE) * SIZE + x / SCALE) as usize;
                image.put_pixel(
                    index as u32 * (width + 4) + x,
                    y,
                    image::Luma([values[source]]),
                );
            }
        }
    }
    image
        .save(path)
        .wrap_err_with(|| format!("write {}", path.display()))
}

async fn run(output: &Path) -> Result<Value> {
    let shipped = PRODUCTION_MODULE;
    let declaration = "const NORMAL_GREEN_SIGN: f32 = ";
    ensure!(
        shipped.matches(declaration).count() == 1,
        "expected one NORMAL_GREEN_SIGN declaration"
    );
    let shipped_sign: f64 = shipped
        .split_once(declaration)
        .and_then(|(_, rest)| rest.split_once(';'))
        .map(|(value, _)| value.trim())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| color_eyre::eyre::eyre!("NORMAL_GREEN_SIGN is not a number"))?;
    let opposite = shipped.replace(
        &format!("{declaration}{shipped_sign:?};"),
        &format!("{declaration}{:?};", -shipped_sign),
    );
    ensure!(opposite != shipped, "the opposite-sign module must differ");
    let (gpu, adapter) = Gpu::new().await?;
    let mut cases = Vec::new();
    let mut passed = true;
    for (sun_name, toward) in [("north", [0.0, 0.0, -1.0]), ("east", [1.0, 0.0, 0.0])] {
        let light = sun(toward);
        let expected = reference(light);
        let shipped_pixels = gpu.render(shipped, light).await?;
        let opposite_pixels = gpu.render(&opposite, light).await?;
        let (shipped_light, shipped_normal) = compare(&shipped_pixels, &expected);
        let (opposite_light, opposite_normal) = compare(&opposite_pixels, &expected);
        let shipped_ok = shipped_light < TOLERANCE && shipped_normal < 2.0 * TOLERANCE;
        // The control: the opposite sign must fail the north sun and pass the east sun.
        let opposite_ok = if sun_name == "north" {
            opposite_light > FAIL_FLOOR
        } else {
            opposite_light < TOLERANCE
        };
        passed &= shipped_ok && opposite_ok;
        // Mean lit value of the dome's north half (larger V, the top of the screen) and south half.
        let half = |lit: &dyn Fn(usize) -> f64, north: bool| {
            let mut sum = 0.0;
            let mut count = 0.0;
            for py in 0..SIZE {
                for px in 0..SIZE {
                    let (u, v) = centre(px, SIZE - 1 - py);
                    if (u - 0.5).hypot(v - 0.5) < 0.35 && (v > 0.5) == north {
                        sum += lit((py * SIZE + px) as usize);
                        count += 1.0;
                    }
                }
            }
            sum / count
        };
        let reference_lambert: Vec<f64> = expected.iter().map(|pixel| pixel[0]).collect();
        write_strip(
            &output.join(format!("dome-sun-{sun_name}.png")),
            [
                &opposite_pixels
                    .iter()
                    .map(|pixel| f64::from(pixel[0]))
                    .collect::<Vec<_>>(),
                &shipped_pixels
                    .iter()
                    .map(|pixel| f64::from(pixel[0]))
                    .collect::<Vec<_>>(),
                &reference_lambert,
            ],
        )?;
        cases.push(json!({
            "sun": sun_name,
            "sun_direction": light,
            "shipped_sign": shipped_sign,
            "shipped_worst_lambert_error": shipped_light,
            "shipped_worst_normal_error": shipped_normal,
            "shipped_passed": shipped_ok,
            "shipped_north_half_mean": half(&|i| f64::from(shipped_pixels[i][0]), true),
            "shipped_south_half_mean": half(&|i| f64::from(shipped_pixels[i][0]), false),
            "reference_north_half_mean": half(&|i| expected[i][0], true),
            "reference_south_half_mean": half(&|i| expected[i][0], false),
            "opposite_sign": -shipped_sign,
            "opposite_worst_lambert_error": opposite_light,
            "opposite_worst_normal_error": opposite_normal,
            "opposite_north_half_mean": half(&|i| f64::from(opposite_pixels[i][0]), true),
            "opposite_south_half_mean": half(&|i| f64::from(opposite_pixels[i][0]), false),
            "opposite_behaved_as_the_control_expects": opposite_ok,
            "image": format!("dome-sun-{sun_name}.png (left: opposite sign, middle: shipped sign, right: CPU reference)"),
        }));
    }
    Ok(json!({
        "kind": "synthetic-terrain-normal-sign-gpu",
        "retail_parity": false,
        "passed": passed,
        "adapter": { "backend": format!("{:?}", adapter.backend), "name": adapter.name, "device_type": format!("{:?}", adapter.device_type), "driver": adapter.driver, "driver_info": adapter.driver_info },
        "production_module": "crates/engine/src/shaders/terrain_normal.wgsl",
        "production_module_sha256": format!("{:x}", Sha256::digest(PRODUCTION_MODULE.as_bytes())),
        "tile": { "texels": SIZE, "view": "top-down, north up, U right, V up the screen", "texture_format": "Rgba8Unorm (linear)", "convention": "DirectX: green follows +V (down the image)" },
        "tolerance": { "lambert": TOLERANCE, "control_fail_floor": FAIL_FLOOR },
        "limits": "synthetic dome and flat tile; not a Skyrim capture, not retail parity, and the retail rule it encodes is skyrim-research NEEDS 21 (reviewed, https://github.com/Mudcrab-Team/skyrim_decomp/pull/1082)",
        "cases": cases,
    }))
}

fn output_directory() -> Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    ensure!(
        args.next().as_deref() == Some("--output"),
        "usage: terrain_normal_probe --output <directory>"
    );
    let output = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| color_eyre::eyre::eyre!("--output requires a directory"))?;
    ensure!(
        args.next().is_none(),
        "usage: terrain_normal_probe --output <directory>"
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
    let receipt = match block_on(run(&output)) {
        Ok(receipt) => receipt,
        Err(error) => json!({
            "kind": "synthetic-terrain-normal-sign-gpu",
            "retail_parity": false,
            "passed": false,
            "error": format!("{error:#}"),
        }),
    };
    let passed = receipt["passed"] == true;
    if let Err(error) = std::fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&receipt).expect("receipt serialises"),
    ) {
        eprintln!("write {}: {error}", receipt_path.display());
        return ExitCode::FAILURE;
    }
    println!(
        "{}",
        json!({ "passed": passed, "receipt": receipt_path, "error": receipt["error"] })
    );
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
