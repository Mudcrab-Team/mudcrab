//! Stage 3 of the visual-profile stage receipts: the scene color after fog and before tone mapping.
//!
//! A probe names a world position. In the frame a capture records, each probe is projected onto the
//! main view, a small patch of the HDR main texture around it is copied out just before Bevy's
//! tone-mapping pass, and the patch is averaged once the copy maps. The copy is taken in the same
//! frame as the screenshot and the capture receipt, so stage 3 and the final image describe one
//! render. See `docs/specs/engine/visual-profiles.md`.

use bevy::{
    camera::Camera,
    core_pipeline::{Core3d, Core3dSystems, tonemapping::tonemapping},
    prelude::*,
    render::{
        Extract, Render, RenderApp, RenderSystems,
        render_resource::{
            Buffer, BufferDescriptor, BufferUsages, Extent3d, MapMode, Origin3d,
            TexelCopyBufferInfo, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
            TextureFormat,
        },
        renderer::{RenderContext, RenderDevice, ViewQuery},
        sync_world::RenderEntity,
        view::{ViewTarget, screenshot::Screenshot},
    },
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use crate::{
    capture_frame::CaptureTicket, streaming::RenderOrigin, world::components::StreamingCamera,
};

/// Side of the averaged square, in pixels, when a probe does not choose one. Large enough that a
/// sub-pixel offset between two captures moves the mean by a fraction of one pixel's weight.
pub const DEFAULT_PATCH_PX: u32 = 5;
/// Largest patch a probe may ask for; one patch row then still fits one aligned copy row.
pub const MAX_PATCH_PX: u32 = 31;
const BYTES_PER_TEXEL: u32 = 8;
const ROW_PITCH: u32 = wgpu_row_alignment(MAX_PATCH_PX * BYTES_PER_TEXEL);

const fn wgpu_row_alignment(bytes: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    bytes.div_ceil(align) * align
}

/// One named point to sample, in absolute Creation units.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ProbePoint {
    pub name: String,
    pub position: [f32; 3],
    #[serde(default = "default_patch")]
    pub patch_px: u32,
}

fn default_patch() -> u32 {
    DEFAULT_PATCH_PX
}

impl ProbePoint {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("a probe has no name".into());
        }
        if !self.position.iter().all(|v| v.is_finite()) {
            return Err(format!("probe \"{}\" has a non-finite position", self.name));
        }
        if self.patch_px == 0 || self.patch_px > MAX_PATCH_PX || self.patch_px.is_multiple_of(2) {
            return Err(format!(
                "probe \"{}\": patch_px must be odd and between 1 and {MAX_PATCH_PX}",
                self.name
            ));
        }
        Ok(())
    }
}

/// Top-left texel and size of an averaged square.
pub type Patch = ([u32; 2], [u32; 2]);

/// Where a probe landed and what the HDR texture held there.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProbeSample {
    pub name: String,
    /// The averaged square, or `None` when the probe was off screen.
    pub patch: Option<Patch>,
    /// Mean linear RGBA over the patch, exactly as the main texture stored it.
    pub hdr_pre_tonemap: Option<[f32; 4]>,
    pub unavailable: Option<String>,
}

#[derive(Debug, Default)]
struct ProbeState {
    points: Vec<ProbePoint>,
    /// Filled when the capture frame is extracted.
    samples: Option<Vec<ProbeSample>>,
    /// Set once every on-screen patch has been read back, or the read failed.
    result: Option<Result<(), String>>,
}

/// Shared between the capture request and the render world; cloned into both.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProbeTicket(Arc<Mutex<ProbeState>>);

impl ProbeTicket {
    pub(crate) fn new(points: Vec<ProbePoint>) -> Self {
        Self(Arc::new(Mutex::new(ProbeState {
            points,
            ..default()
        })))
    }

    /// The patches the capture frame chose, once it has been extracted.
    pub(crate) fn patches(&self) -> Option<Vec<ProbeSample>> {
        self.0.lock().unwrap().samples.clone()
    }

    /// `None` while the read is in flight; the samples once it is done.
    pub(crate) fn finished(&self) -> Option<Result<Vec<ProbeSample>, String>> {
        let state = self.0.lock().unwrap();
        let result = state.result.clone()?;
        Some(result.map(|()| state.samples.clone().unwrap_or_default()))
    }

    fn fail(&self, reason: String) {
        let mut state = self.0.lock().unwrap();
        if state.result.is_none() {
            state.result = Some(Err(reason));
        }
    }
}

/// The patch a probe averages: centred on its pixel and moved, not shrunk, to stay on screen, so
/// every patch has the same number of texels.
fn patch_around(pixel: Vec2, patch: u32, size: UVec2) -> Option<Patch> {
    if patch > size.x || patch > size.y {
        return None;
    }
    if !(pixel.x >= 0.0 && pixel.y >= 0.0 && pixel.x < size.x as f32 && pixel.y < size.y as f32) {
        return None;
    }
    let half = patch / 2;
    let clamp = |centre: f32, side: u32| {
        (centre.floor() as u32)
            .saturating_sub(half)
            .min(side - patch)
    };
    Some((
        [clamp(pixel.x, size.x), clamp(pixel.y, size.y)],
        [patch, patch],
    ))
}

#[derive(Resource, Default)]
struct ProbeRequests {
    /// The main view's render entity; only that view is sampled.
    view: Option<Entity>,
    pending: Vec<ProbeTicket>,
    in_flight: Vec<(ProbeTicket, Buffer, Vec<usize>)>,
}

#[allow(clippy::type_complexity)]
fn extract_probe_requests(
    mut requests: ResMut<ProbeRequests>,
    tickets: Extract<Query<&CaptureTicket, With<Screenshot>>>,
    camera: Extract<Query<(&Camera, &GlobalTransform, &RenderEntity), With<StreamingCamera>>>,
    origin: Extract<Option<Res<RenderOrigin>>>,
) {
    requests.pending.clear();
    let Ok((camera, transform, render_entity)) = camera.single() else {
        requests.view = None;
        return;
    };
    requests.view = Some(render_entity.id());
    let size = camera.physical_viewport_size();
    let scale = camera.target_scaling_factor().unwrap_or(1.0);
    let offset = origin
        .as_deref()
        .map(|origin| crate::shots::shot_camera_translation([0.0; 3], origin.0))
        .unwrap_or(Vec3::ZERO);
    for ticket in &tickets {
        let Some(probes) = ticket.probes() else {
            continue;
        };
        let mut state = probes.0.lock().unwrap();
        if state.samples.is_some() {
            continue;
        }
        let samples = state
            .points
            .iter()
            .map(|point| {
                let runtime = Vec3::from(shared::coordinates::creation_to_runtime_vector(
                    point.position,
                )) + offset;
                let pixel = camera
                    .world_to_viewport(transform, runtime)
                    .ok()
                    .map(|logical| logical * scale);
                let patch = size
                    .zip(pixel)
                    .and_then(|(size, pixel)| patch_around(pixel, point.patch_px, size));
                ProbeSample {
                    name: point.name.clone(),
                    patch,
                    hdr_pre_tonemap: None,
                    unavailable: patch.is_none().then(|| "off screen".into()),
                }
            })
            .collect();
        state.samples = Some(samples);
        drop(state);
        requests.pending.push(probes.clone());
    }
}

/// Copies each requested patch out of the main texture before tone mapping overwrites it.
fn copy_probe_patches(
    view: ViewQuery<&ViewTarget>,
    mut requests: ResMut<ProbeRequests>,
    device: Res<RenderDevice>,
    mut ctx: RenderContext,
) {
    if requests.view != Some(view.entity()) || requests.pending.is_empty() {
        return;
    }
    let target = view.into_inner();
    let texture = target.main_texture();
    let pending = std::mem::take(&mut requests.pending);
    for ticket in pending {
        if target.main_texture_format() != TextureFormat::Rgba16Float {
            ticket.fail(format!(
                "main texture is {:?}, not the HDR Rgba16Float stage 3 reads",
                target.main_texture_format()
            ));
            continue;
        }
        let patches: Vec<(usize, Patch)> = ticket
            .patches()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter_map(|(index, sample)| sample.patch.map(|patch| (index, patch)))
            .collect();
        if patches.is_empty() {
            ticket.0.lock().unwrap().result = Some(Ok(()));
            continue;
        }
        let slot = u64::from(ROW_PITCH) * u64::from(MAX_PATCH_PX);
        let buffer = device.create_buffer(&BufferDescriptor {
            label: Some("stage 3 probe readback"),
            size: slot * patches.len() as u64,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let encoder = ctx.command_encoder();
        for (slot_index, (_, (origin, size))) in patches.iter().enumerate() {
            encoder.copy_texture_to_buffer(
                TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: Origin3d {
                        x: origin[0],
                        y: origin[1],
                        z: 0,
                    },
                    aspect: TextureAspect::All,
                },
                TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: TexelCopyBufferLayout {
                        offset: slot * slot_index as u64,
                        bytes_per_row: Some(ROW_PITCH),
                        rows_per_image: Some(size[1]),
                    },
                },
                Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
            );
        }
        let indices = patches.iter().map(|(index, _)| *index).collect();
        requests.in_flight.push((ticket, buffer, indices));
    }
}

/// Maps the copies after the frame's commands are submitted and averages each patch.
fn map_probe_patches(mut requests: ResMut<ProbeRequests>) {
    for (ticket, buffer, indices) in requests.in_flight.drain(..) {
        let mapped = buffer.clone();
        buffer.slice(..).map_async(MapMode::Read, move |result| {
            if let Err(error) = result {
                ticket.fail(format!("stage 3 readback did not map: {error}"));
                return;
            }
            let means = {
                let bytes = mapped.slice(..).get_mapped_range();
                let state = ticket.0.lock().unwrap();
                let samples = state.samples.as_deref().unwrap_or_default();
                indices
                    .iter()
                    .enumerate()
                    .map(|(slot, &index)| {
                        let (_, size) = samples[index].patch.expect("copied patches are on screen");
                        let start = slot * (ROW_PITCH * MAX_PATCH_PX) as usize;
                        (index, mean_rgba16f(&bytes[start..], size))
                    })
                    .collect::<Vec<_>>()
            };
            mapped.unmap();
            let mut state = ticket.0.lock().unwrap();
            if let Some(samples) = state.samples.as_mut() {
                for (index, mean) in means {
                    samples[index].hdr_pre_tonemap = Some(mean);
                }
            }
            state.result.get_or_insert(Ok(()));
        });
    }
}

fn mean_rgba16f(bytes: &[u8], [width, height]: [u32; 2]) -> [f32; 4] {
    let mut sum = [0.0f64; 4];
    for row in 0..height as usize {
        let row_bytes = &bytes[row * ROW_PITCH as usize..];
        for texel in 0..width as usize {
            for (channel, total) in sum.iter_mut().enumerate() {
                let at = texel * BYTES_PER_TEXEL as usize + channel * 2;
                let bits = u16::from_le_bytes([row_bytes[at], row_bytes[at + 1]]);
                *total += f64::from(f16_to_f32(bits));
            }
        }
    }
    let count = f64::from(width * height);
    sum.map(|total| (total / count) as f32)
}

/// IEEE 754 binary16 to binary32, including subnormals, infinities and NaN.
fn f16_to_f32(bits: u16) -> f32 {
    let exponent = (bits >> 10) & 0x1f;
    let mantissa = f32::from(bits & 0x3ff);
    let magnitude = match exponent {
        0 => mantissa * 2f32.powi(-24),
        0x1f if mantissa == 0.0 => f32::INFINITY,
        0x1f => f32::NAN,
        _ => (1.0 + mantissa / 1024.0) * 2f32.powi(i32::from(exponent) - 15),
    };
    if bits & 0x8000 != 0 {
        -magnitude
    } else {
        magnitude
    }
}

pub(crate) struct HdrProbePlugin;

impl Plugin for HdrProbePlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<ProbeRequests>()
            .add_systems(ExtractSchedule, extract_probe_requests)
            .add_systems(
                Core3d,
                copy_probe_patches
                    .in_set(Core3dSystems::PostProcess)
                    .after(bevy::post_process::bloom::bloom)
                    .before(tonemapping),
            )
            .add_systems(Render, map_probe_patches.in_set(RenderSystems::Cleanup));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_decode_exactly() {
        for (bits, value) in [
            (0x0000, 0.0),
            (0x3c00, 1.0),
            (0xc000, -2.0),
            (0x3800, 0.5),
            (0x7bff, 65504.0),
            (0x0001, 2f32.powi(-24)),
            (0x03ff, 1023.0 * 2f32.powi(-24)),
            (0x3555, 0.333_251_95),
        ] {
            assert_eq!(f16_to_f32(bits), value, "{bits:#06x}");
        }
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
        assert_eq!(f16_to_f32(0x8001), -(2f32.powi(-24)));
    }

    #[test]
    fn patches_stay_whole_and_on_screen() {
        let size = UVec2::new(1280, 720);
        assert_eq!(
            patch_around(Vec2::new(640.5, 360.5), 5, size),
            Some(([638, 358], [5, 5]))
        );
        assert_eq!(
            patch_around(Vec2::new(0.2, 0.2), 5, size),
            Some(([0, 0], [5, 5]))
        );
        assert_eq!(
            patch_around(Vec2::new(1279.9, 719.9), 5, size),
            Some(([1275, 715], [5, 5]))
        );
        assert_eq!(patch_around(Vec2::new(-1.0, 10.0), 5, size), None);
        assert_eq!(patch_around(Vec2::new(10.0, 720.0), 5, size), None);
        assert_eq!(patch_around(Vec2::new(f32::NAN, 10.0), 5, size), None);
    }

    #[test]
    fn patch_mean_reads_padded_rows() {
        let one = 0x3c00u16.to_le_bytes();
        let two = 0x4000u16.to_le_bytes();
        let mut bytes = vec![0u8; (ROW_PITCH * 2) as usize];
        for (row, value) in [(0usize, one), (1, two)] {
            for texel in 0..2usize {
                for channel in 0..4usize {
                    let at = row * ROW_PITCH as usize + texel * 8 + channel * 2;
                    bytes[at..at + 2].copy_from_slice(&value);
                }
            }
        }
        assert_eq!(mean_rgba16f(&bytes, [2, 2]), [1.5; 4]);
    }

    #[test]
    fn probe_points_validate_patch_sizes() {
        let point = |patch_px| ProbePoint {
            name: "thatch".into(),
            position: [0.0; 3],
            patch_px,
        };
        assert!(point(5).validate().is_ok());
        assert!(point(1).validate().is_ok());
        for bad in [0, 4, MAX_PATCH_PX + 2] {
            assert!(point(bad).validate().is_err(), "{bad}");
        }
        let mut unnamed = point(5);
        unnamed.name.clear();
        assert!(unnamed.validate().is_err());
    }
}
