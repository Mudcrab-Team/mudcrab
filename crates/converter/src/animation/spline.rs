//! `hkaSplineCompressedAnimation::m_data` decoding.
//!
//! Per block, every alignment relative to the start of `m_data`: one 4-byte mask per transform
//! track, one mask byte per float track, align 4; then per track translation, align 4, rotation,
//! align 4, scale, align 4. Float-track data lives elsewhere in the block; only its mask bytes
//! sit here, and the transform tracks stay aligned only if those bytes are skipped.
//!
//! Rotation compression is limited to THREECOMP40, the only type any retail Skyrim SE clip uses.
//! The other quantizations are parsed far enough to name them and then rejected.

use super::TransformSample;
use super::classes::SplineAnimation;
use color_eyre::{
    Result,
    eyre::{bail, ensure, eyre},
};

const STATIC: [u8; 4] = [0x01, 0x02, 0x04, 0x08];
const SPLINE: [u8; 4] = [0x10, 0x20, 0x40, 0x80];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationQuantization {
    Polar32,
    ThreeComp40,
    ThreeComp48,
    ThreeComp24,
    Straight16,
    Uncompressed,
}

impl RotationQuantization {
    fn from_bits(bits: u8) -> Result<Self> {
        Ok(match bits {
            0 => Self::Polar32,
            1 => Self::ThreeComp40,
            2 => Self::ThreeComp48,
            3 => Self::ThreeComp24,
            4 => Self::Straight16,
            5 => Self::Uncompressed,
            _ => bail!("rotation quantization {bits} is not defined"),
        })
    }

    fn align(self) -> usize {
        match self {
            Self::Polar32 | Self::Uncompressed => 4,
            Self::ThreeComp48 | Self::Straight16 => 2,
            Self::ThreeComp40 | Self::ThreeComp24 => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarQuantization {
    Bits8,
    Bits16,
}

impl ScalarQuantization {
    fn from_bits(bits: u8) -> Result<Self> {
        Ok(match bits {
            0 => Self::Bits8,
            1 => Self::Bits16,
            _ => bail!("scalar quantization {bits} is not defined"),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct TrackMask {
    translation_q: ScalarQuantization,
    rotation_q: RotationQuantization,
    scale_q: ScalarQuantization,
    translation: u8,
    rotation: u8,
    scale: u8,
}

#[derive(Debug, Clone)]
pub struct Knots {
    pub degree: usize,
    pub knots: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Vec3Track {
    Static([f32; 3]),
    /// Per component: control points (a single value for a static component).
    Spline {
        knots: Knots,
        components: [Vec<f32>; 3],
    },
}

#[derive(Debug, Clone)]
pub enum QuatTrack {
    Static([f32; 4]),
    Spline { knots: Knots, points: Vec<[f32; 4]> },
}

#[derive(Debug, Clone)]
pub struct Track {
    pub translation: Vec3Track,
    pub rotation: QuatTrack,
    pub scale: Vec3Track,
}

struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(len)
            .ok_or_else(|| eyre!("spline data read at {:#x} overflows", self.position))?;
        let bytes = self
            .data
            .get(self.position..end)
            .ok_or_else(|| eyre!("spline data read of {len} at {:#x} past end", self.position))?;
        self.position = end;
        Ok(bytes)
    }

    fn ensure_available(&self, len: usize) -> Result<()> {
        let end = self
            .position
            .checked_add(len)
            .ok_or_else(|| eyre!("spline data read at {:#x} overflows", self.position))?;
        ensure!(
            end <= self.data.len(),
            "spline data read of {len} at {:#x} past end",
            self.position
        );
        Ok(())
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn align(&mut self, alignment: usize) {
        self.position = self.position.div_ceil(alignment) * alignment;
    }
}

fn read_knots(cursor: &mut Cursor<'_>) -> Result<(usize, Knots)> {
    let count_minus_one = cursor.u16()? as usize;
    let degree = cursor.u8()? as usize;
    ensure!(
        (1..=4).contains(&degree),
        "spline degree {degree} out of range at {:#x}",
        cursor.position
    );
    let knots = cursor.take(count_minus_one + degree + 2)?.to_vec();
    ensure!(
        knots.windows(2).all(|pair| pair[0] <= pair[1]),
        "knot vector is not non-decreasing at {:#x}: {knots:?}",
        cursor.position
    );
    let count = count_minus_one + 1;
    ensure!(
        count == 1 || count > degree,
        "spline has {count} control points for degree {degree} at {:#x}",
        cursor.position
    );
    ensure!(
        count == 1 || (knots[degree] == 0 && knots[count] > 0),
        "spline knot domain does not contain frame 0 at {:#x}: {knots:?}",
        cursor.position
    );
    Ok((count, Knots { degree, knots }))
}

fn quantized(
    cursor: &mut Cursor<'_>,
    quantization: ScalarQuantization,
    minimum: f32,
    maximum: f32,
) -> Result<f32> {
    let fraction = match quantization {
        ScalarQuantization::Bits8 => cursor.u8()? as f32 / 255.0,
        ScalarQuantization::Bits16 => cursor.u16()? as f32 / 65535.0,
    };
    let value = minimum + (maximum - minimum) * fraction;
    ensure!(
        value.is_finite(),
        "spline value {value} from bounds [{minimum}, {maximum}] is not finite at {:#x}",
        cursor.position
    );
    Ok(value)
}

fn read_vec3(
    cursor: &mut Cursor<'_>,
    flags: u8,
    quantization: ScalarQuantization,
    identity: f32,
) -> Result<Vec3Track> {
    if flags & 0x70 == 0 {
        let mut value = [identity; 3];
        for (index, component) in value.iter_mut().enumerate() {
            if flags & STATIC[index] != 0 {
                let decoded = cursor.f32()?;
                ensure!(
                    decoded.is_finite(),
                    "spline static value {decoded} is not finite at {:#x}",
                    cursor.position
                );
                *component = decoded;
            }
        }
        return Ok(Vec3Track::Static(value));
    }

    let (count, knots) = read_knots(cursor)?;
    cursor.align(4);
    let mut bounds = [(0.0, 0.0); 3];
    let spline_components = (0..3).filter(|&index| flags & SPLINE[index] != 0).count();
    let scalar_bytes = match quantization {
        ScalarQuantization::Bits8 => 1,
        ScalarQuantization::Bits16 => 2,
    };
    let encoded_points = count
        .checked_mul(spline_components)
        .and_then(|points| points.checked_mul(scalar_bytes))
        .ok_or_else(|| eyre!("spline point data size overflow"))?;
    let static_values = (0..3)
        .filter(|&index| flags & SPLINE[index] == 0 && flags & STATIC[index] != 0)
        .count()
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| eyre!("spline static data size overflow"))?;
    let bound_bytes = spline_components
        .checked_mul(2 * std::mem::size_of::<f32>())
        .ok_or_else(|| eyre!("spline bound data size overflow"))?;
    cursor.ensure_available(
        static_values
            .checked_add(bound_bytes)
            .and_then(|size| size.checked_add(encoded_points))
            .ok_or_else(|| eyre!("spline point data size overflow"))?,
    )?;
    // Metadata is interleaved by component index in the wire format.
    let mut static_components = [identity; 3];
    for index in 0..3 {
        if flags & SPLINE[index] != 0 {
            let minimum = cursor.f32()?;
            let maximum = cursor.f32()?;
            ensure!(
                minimum.is_finite() && maximum.is_finite(),
                "spline bounds [{minimum}, {maximum}] are not finite at {:#x}",
                cursor.position
            );
            bounds[index] = (minimum, maximum);
        } else if flags & STATIC[index] != 0 {
            let decoded = cursor.f32()?;
            ensure!(
                decoded.is_finite(),
                "spline static value {decoded} is not finite at {:#x}",
                cursor.position
            );
            static_components[index] = decoded;
        }
    }
    let mut components: [Vec<f32>; 3] = std::array::from_fn(|index| {
        Vec::with_capacity(if flags & SPLINE[index] != 0 { count } else { 1 })
    });
    for index in 0..3 {
        if flags & SPLINE[index] == 0 {
            components[index].push(static_components[index]);
        }
    }
    for _ in 0..count {
        for index in 0..3 {
            if flags & SPLINE[index] != 0 {
                components[index].push(quantized(
                    cursor,
                    quantization,
                    bounds[index].0,
                    bounds[index].1,
                )?);
            }
        }
    }
    Ok(Vec3Track::Spline { knots, components })
}

/// Rebuilds the dropped (largest) component from unit length, then applies its sign.
fn rebuild_largest(small: [f32; 3], dropped: usize, negative: bool) -> [f32; 4] {
    let mut quaternion = [0.0; 4];
    let mut source = small.iter();
    for (index, value) in quaternion.iter_mut().enumerate() {
        if index != dropped {
            *value = *source.next().expect("three stored components");
        }
    }
    let rest = 1.0 - small.iter().map(|value| value * value).sum::<f32>();
    quaternion[dropped] = if rest > 0.0 { rest.sqrt() } else { 0.0 };
    if negative {
        quaternion[dropped] = -quaternion[dropped];
    }
    quaternion
}

fn read_quat(cursor: &mut Cursor<'_>, quantization: RotationQuantization) -> Result<[f32; 4]> {
    match quantization {
        RotationQuantization::ThreeComp40 => {
            let bytes = cursor.take(5)?;
            let value =
                u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], 0, 0, 0]);
            // 12 bits per component, range ±1/√2 around 2047.
            let scale = std::f32::consts::FRAC_1_SQRT_2 / 2047.0;
            let component = |shift: u32| ((value >> shift) & 0xFFF) as i32 - 2047;
            let small = [
                component(0) as f32 * scale,
                component(12) as f32 * scale,
                component(24) as f32 * scale,
            ];
            Ok(rebuild_largest(
                small,
                ((value >> 36) & 3) as usize,
                (value >> 38) & 1 != 0,
            ))
        }
        other => bail!("unsupported rotation compression {other:?}: no retail Skyrim clip uses it"),
    }
}

fn read_rotation(
    cursor: &mut Cursor<'_>,
    flags: u8,
    quantization: RotationQuantization,
) -> Result<QuatTrack> {
    if flags & 0xF0 != 0 {
        let (count, knots) = read_knots(cursor)?;
        cursor.align(quantization.align());
        if quantization != RotationQuantization::ThreeComp40 {
            read_quat(cursor, quantization)?;
            unreachable!("unsupported rotation compression returned a value");
        }
        let encoded_points = count
            .checked_mul(5)
            .ok_or_else(|| eyre!("rotation point data size overflow"))?;
        cursor.ensure_available(encoded_points)?;
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            points.push(read_quat(cursor, quantization)?);
        }
        Ok(QuatTrack::Spline { knots, points })
    } else if flags & 0x0F != 0 {
        cursor.align(quantization.align());
        Ok(QuatTrack::Static(read_quat(cursor, quantization)?))
    } else {
        Ok(QuatTrack::Static([0.0, 0.0, 0.0, 1.0]))
    }
}

/// Decodes one block starting at `offset` in `m_data`.
pub fn read_block(
    data: &[u8],
    offset: usize,
    tracks: usize,
    float_tracks: usize,
) -> Result<Vec<Track>> {
    let mut cursor = Cursor {
        data,
        position: offset,
    };
    let mut masks = Vec::with_capacity(tracks);
    for _ in 0..tracks {
        let bytes = cursor.take(4)?;
        masks.push(TrackMask {
            translation_q: ScalarQuantization::from_bits(bytes[0] & 0x03)?,
            rotation_q: RotationQuantization::from_bits((bytes[0] >> 2) & 0x0F)?,
            scale_q: ScalarQuantization::from_bits(bytes[0] >> 6)?,
            translation: bytes[1],
            rotation: bytes[2],
            scale: bytes[3],
        });
    }
    // Float tracks keep their data elsewhere; only the mask bytes sit before the tracks.
    cursor.take(float_tracks)?;
    cursor.align(4);

    let mut decoded = Vec::with_capacity(tracks);
    for mask in masks {
        ensure!(
            mask.translation & 0x88 == 0 && mask.scale & 0x88 == 0,
            "translation/scale flags use the w bit: {mask:?}"
        );
        let translation = read_vec3(&mut cursor, mask.translation, mask.translation_q, 0.0)?;
        cursor.align(4);
        let rotation = read_rotation(&mut cursor, mask.rotation, mask.rotation_q)?;
        cursor.align(4);
        let scale = read_vec3(&mut cursor, mask.scale, mask.scale_q, 1.0)?;
        cursor.align(4);
        let track = Track {
            translation,
            rotation,
            scale,
        };
        // Belt and braces before the clip GLB is written: every value a sample
        // can interpolate from must be finite, including values a future
        // quantization arm might produce.
        ensure!(
            track_values_are_finite(&track),
            "decoded track {} at block offset {offset:#x} has a non-finite value",
            decoded.len()
        );
        decoded.push(track);
    }
    Ok(decoded)
}

/// Whether every static value and control point a track can sample is finite.
/// `find_span` only returns spans with a width, so a finite track
/// yields finite samples across its knot domain.
fn track_values_are_finite(track: &Track) -> bool {
    fn vec3(track: &Vec3Track) -> bool {
        match track {
            Vec3Track::Static(value) => value.iter().all(|value| value.is_finite()),
            Vec3Track::Spline { components, .. } => {
                components.iter().flatten().all(|value| value.is_finite())
            }
        }
    }
    fn quat(track: &QuatTrack) -> bool {
        match track {
            QuatTrack::Static(value) => value.iter().all(|value| value.is_finite()),
            QuatTrack::Spline { points, .. } => {
                points.iter().flatten().all(|value| value.is_finite())
            }
        }
    }
    vec3(&track.translation) && quat(&track.rotation) && vec3(&track.scale)
}

/// NURBS book A2.1: span index for `u` with `n` control points.
fn find_span(degree: usize, u: f32, n: usize, knots: &[u8]) -> usize {
    if n <= 1 {
        return 0;
    }
    // A frame at either end of the domain takes the nearest span with a width: a repeated end
    // knot leaves the outermost span empty, and `basis` divides by the width of the span it is
    // given. `read_knots` guarantees the domain itself has a width, so such a span exists.
    // Searching below the domain would not terminate.
    if u <= knots[degree] as f32 {
        return (degree..n)
            .find(|&span| knots[span] < knots[span + 1])
            .unwrap_or(degree);
    }
    if u >= knots[n] as f32 {
        return (degree..n)
            .rev()
            .find(|&span| knots[span] < knots[span + 1])
            .unwrap_or(n - 1);
    }
    let (mut low, mut high) = (degree, n);
    let mut mid = (low + high) / 2;
    while u < knots[mid] as f32 || u >= knots[mid + 1] as f32 {
        if u < knots[mid] as f32 {
            high = mid;
        } else {
            low = mid;
        }
        mid = (low + high) / 2;
    }
    mid
}

/// B-spline basis weights for `span`, ordered so that `weights[i]` multiplies point `span - i`.
fn basis(span: usize, degree: usize, u: f32, knots: &[u8]) -> [f32; 5] {
    let mut weights = [1.0, 0.0, 0.0, 0.0, 0.0];
    for order in 1..=degree {
        for index in (0..order).rev() {
            let low = knots[span - index] as f32;
            let high = knots[span + order - index] as f32;
            let alpha = (u - low) / (high - low);
            let scaled = weights[index] * alpha;
            weights[index + 1] += weights[index] - scaled;
            weights[index] = scaled;
        }
    }
    weights
}

fn eval_scalar(knots: &Knots, points: &[f32], u: f32) -> f32 {
    if points.len() == 1 {
        return points[0];
    }
    let span = find_span(knots.degree, u, points.len(), &knots.knots);
    let weights = basis(span, knots.degree, u, &knots.knots);
    (0..=knots.degree)
        .map(|index| points[span - index] * weights[index])
        .sum()
}

pub fn eval_vec3(track: &Vec3Track, u: f32) -> [f32; 3] {
    match track {
        Vec3Track::Static(value) => *value,
        Vec3Track::Spline { knots, components } => {
            [0, 1, 2].map(|index| eval_scalar(knots, &components[index], u))
        }
    }
}

pub fn eval_quat(track: &QuatTrack, u: f32) -> [f32; 4] {
    let quaternion = match track {
        QuatTrack::Static(quaternion) => *quaternion,
        QuatTrack::Spline { knots, points } => {
            if points.len() == 1 {
                points[0]
            } else {
                let span = find_span(knots.degree, u, points.len(), &knots.knots);
                let weights = basis(span, knots.degree, u, &knots.knots);
                let mut quaternion = [0.0; 4];
                for index in 0..=knots.degree {
                    for component in 0..4 {
                        quaternion[component] += points[span - index][component] * weights[index];
                    }
                }
                quaternion
            }
        }
    };
    let norm = quaternion
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if norm > 0.0 {
        quaternion.map(|value| value / norm)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

pub struct Decoded {
    blocks: Vec<Vec<Track>>,
    frames_per_block: usize,
}

/// Supported format limit for block knots, whose frame indices use a byte.
const SUPPORTED_MAX_FRAMES_PER_BLOCK: i32 = 256;
/// Bound retained decoded animation memory before any mask, block, or track allocation.
const MAX_DECODED_ANIMATION_BYTES: usize = 256 * 1024 * 1024;
/// Conservative allowance for decoded spline payload per declared block. Repeated offsets can
/// retain duplicate tracks. Point vectors reserve exact lengths: all three vec3 channels retain
/// 12 bytes per three encoded 8-bit scalars (4×), and rotation points retain 16 bytes per five
/// encoded bytes (3.2×). Knot storage is copied byte-for-byte. Encoded bounds, knot headers, and
/// static values provide credit for the remaining one-value static/identity vec3 channels. Thus
/// four times encoded block data covers retained spline payload; this is not an RSS prediction.
const DECODED_BLOCK_PAYLOAD_MULTIPLIER: usize = 4;

fn decoded_memory_estimate(
    frames: usize,
    tracks: usize,
    blocks: usize,
    encoded_data_bytes: usize,
) -> Result<usize> {
    let sample_bytes = frames
        .checked_mul(tracks)
        .and_then(|samples| samples.checked_mul(std::mem::size_of::<super::TransformSample>()))
        .ok_or_else(|| eyre!("decoded-sample size overflow"))?;
    let vector_metadata = std::mem::size_of::<Vec<Track>>()
        .checked_mul(3)
        .ok_or_else(|| eyre!("decoded-block metadata size overflow"))?;
    let track_bytes = tracks
        .checked_mul(std::mem::size_of::<Track>() + std::mem::size_of::<TrackMask>())
        .and_then(|bytes| bytes.checked_add(vector_metadata))
        .ok_or_else(|| eyre!("decoded-block metadata size overflow"))?;
    let block_payload_bytes = encoded_data_bytes
        .checked_mul(DECODED_BLOCK_PAYLOAD_MULTIPLIER)
        .ok_or_else(|| eyre!("decoded-block payload size overflow"))?;
    let retained_block_bytes = blocks
        .checked_mul(
            track_bytes
                .checked_add(block_payload_bytes)
                .ok_or_else(|| eyre!("decoded-block size overflow"))?,
        )
        .ok_or_else(|| eyre!("decoded-block size overflow"))?;
    sample_bytes
        .checked_add(retained_block_bytes)
        .ok_or_else(|| eyre!("decoded-animation size overflow"))
}

impl Decoded {
    pub fn new(animation: &SplineAnimation) -> Result<Self> {
        ensure!(
            animation.endian == 0,
            "m_endian = {}, only little-endian data is supported",
            animation.endian
        );
        ensure!(
            animation.num_transform_tracks >= 0,
            "negative transform track count {}",
            animation.num_transform_tracks
        );
        ensure!(
            animation.num_float_tracks >= 0,
            "negative float track count {}",
            animation.num_float_tracks
        );
        ensure!(
            animation.num_frames >= 1,
            "clip has {} frames",
            animation.num_frames
        );
        ensure!(
            animation.num_blocks >= 1,
            "clip has {} blocks",
            animation.num_blocks
        );
        ensure!(
            animation.max_frames_per_block > 1,
            "max frames per block {} leaves no frame step",
            animation.max_frames_per_block
        );
        ensure!(
            animation.max_frames_per_block <= SUPPORTED_MAX_FRAMES_PER_BLOCK,
            "max frames per block {} exceeds supported limit of {SUPPORTED_MAX_FRAMES_PER_BLOCK}",
            animation.max_frames_per_block
        );
        // Blocks share their boundary frame, so the layout holds at most this many frames;
        // a larger count would sample past the last block's data.
        let layout_frames =
            i64::from(animation.num_blocks) * (i64::from(animation.max_frames_per_block) - 1) + 1;
        ensure!(
            i64::from(animation.num_frames) <= layout_frames,
            "clip has {} frames but {} blocks of {} frames hold at most {layout_frames}",
            animation.num_frames,
            animation.num_blocks,
            animation.max_frames_per_block
        );
        ensure!(
            animation.num_transform_tracks > 0,
            "clips without transform tracks are unsupported"
        );
        let estimate = usize::try_from(animation.num_frames)
            .ok()
            .and_then(|frames| {
                usize::try_from(animation.num_transform_tracks)
                    .ok()
                    .map(|tracks| (frames, tracks))
            })
            .and_then(|(frames, tracks)| {
                usize::try_from(animation.num_blocks)
                    .ok()
                    .map(|blocks| (frames, tracks, blocks))
            })
            .ok_or_else(|| eyre!("decoded-animation size overflow"))?;
        let estimate =
            decoded_memory_estimate(estimate.0, estimate.1, estimate.2, animation.data.len())?;
        ensure!(
            estimate <= MAX_DECODED_ANIMATION_BYTES,
            "clip estimated decoded animation memory requires {estimate} bytes, exceeding the 256 MiB decoded-animation budget"
        );
        ensure!(
            animation.frame_duration.is_finite() && animation.frame_duration > 0.0,
            "invalid frame duration {}",
            animation.frame_duration
        );
        ensure!(
            animation.block_offsets.len() == animation.num_blocks as usize,
            "{} block offsets for {} blocks",
            animation.block_offsets.len(),
            animation.num_blocks
        );
        let float_tracks = animation.num_float_tracks as usize;
        ensure!(
            float_tracks == 0
                || animation.float_block_offsets.len() == animation.num_blocks as usize,
            "{} float block offsets for {} blocks and {float_tracks} float tracks",
            animation.float_block_offsets.len(),
            animation.num_blocks
        );

        let tracks = animation.num_transform_tracks as usize;
        let mut blocks = Vec::with_capacity(animation.block_offsets.len());
        for &offset in &animation.block_offsets {
            ensure!(
                offset % 16 == 0,
                "block offset {offset:#x} is not 16-aligned"
            );
            blocks.push(read_block(
                &animation.data,
                offset as usize,
                tracks,
                float_tracks,
            )?);
        }
        Ok(Self {
            blocks,
            frames_per_block: animation.max_frames_per_block as usize - 1,
        })
    }

    /// `(block, frame within block)` for a global frame. Adjacent blocks share their boundary
    /// frame, so each block advances `maxFramesPerBlock - 1` frames.
    pub fn locate(&self, frame: usize) -> (usize, f32) {
        let block = (frame / self.frames_per_block).min(self.blocks.len() - 1);
        (block, (frame - block * self.frames_per_block) as f32)
    }

    /// Samples `track` at a global frame, refusing a non-finite result.
    ///
    /// Finite control points do not guarantee a finite sample: a frame past the knot domain
    /// extrapolates, with basis weights above one, and can overflow `f32`.
    pub fn sample(&self, track_index: usize, frame: usize) -> Result<TransformSample> {
        let (block, u) = self.locate(frame);
        let track = &self.blocks[block][track_index];
        let sample = TransformSample {
            translation: eval_vec3(&track.translation, u),
            rotation: eval_quat(&track.rotation, u),
            scale: eval_vec3(&track.scale, u),
        };
        ensure!(
            sample
                .translation
                .iter()
                .chain(&sample.rotation)
                .chain(&sample.scale)
                .all(|value| value.is_finite()),
            "track {track_index} samples to a non-finite transform at frame {frame}: {sample:?}"
        );
        Ok(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(data: &[u8]) -> Cursor<'_> {
        Cursor { data, position: 0 }
    }

    fn spline_fixture(count: usize, degree: usize) -> Vec<u8> {
        let mut data = vec![0; 4];
        data.extend_from_slice(&((count - 1) as u16).to_le_bytes());
        data.push(degree as u8);
        let mut knots = vec![0; degree + 1];
        knots.extend((1..count - degree).map(|value| value as u8));
        knots.extend(std::iter::repeat_n(255, degree + 1));
        assert_eq!(knots.len(), count + degree + 1);
        data.extend(knots);
        data.resize(data.len().div_ceil(4) * 4, 0);
        data
    }

    fn measured_vec3_payload(
        count: usize,
        flags: u8,
        quantization: ScalarQuantization,
    ) -> (usize, usize) {
        let mut data = spline_fixture(count, 3);
        let spline_channels = (0..3).filter(|&index| flags & SPLINE[index] != 0).count();
        for _ in 0..spline_channels {
            data.extend_from_slice(&0.0_f32.to_le_bytes());
            data.extend_from_slice(&1.0_f32.to_le_bytes());
        }
        for index in 0..3 {
            if flags & SPLINE[index] == 0 && flags & STATIC[index] != 0 {
                data.extend_from_slice(&2.0_f32.to_le_bytes());
            }
        }
        let scalar_bytes = match quantization {
            ScalarQuantization::Bits8 => 1,
            ScalarQuantization::Bits16 => 2,
        };
        for _ in 0..count * spline_channels {
            data.extend(std::iter::repeat_n(0, scalar_bytes));
        }
        let mut cursor = Cursor {
            data: &data,
            position: 4,
        };
        let track = read_vec3(&mut cursor, flags, quantization, 0.0).unwrap();
        let retained_capacity_bytes = match track {
            Vec3Track::Spline { knots, components } => {
                knots.knots.capacity()
                    + components
                        .iter()
                        .map(|component| component.capacity() * std::mem::size_of::<f32>())
                        .sum::<usize>()
            }
            Vec3Track::Static(_) => unreachable!(),
        };
        (retained_capacity_bytes, cursor.position - 4)
    }

    fn measured_rotation_payload(count: usize) -> (usize, usize) {
        let mut data = spline_fixture(count, 3);
        data.extend(std::iter::repeat_n(0, count * 5));
        let mut cursor = Cursor {
            data: &data,
            position: 4,
        };
        let track = read_rotation(&mut cursor, 0x10, RotationQuantization::ThreeComp40).unwrap();
        let retained_capacity_bytes = match track {
            QuatTrack::Spline { knots, points } => {
                knots.knots.capacity() + points.capacity() * std::mem::size_of::<[f32; 4]>()
            }
            QuatTrack::Static(_) => unreachable!(),
        };
        (retained_capacity_bytes, cursor.position - 4)
    }

    fn round_trip(value: f32, minimum: f32, maximum: f32, quantization: ScalarQuantization) -> f32 {
        let fraction = (value - minimum) / (maximum - minimum);
        let bytes = match quantization {
            ScalarQuantization::Bits8 => vec![(fraction * 255.0).round() as u8],
            ScalarQuantization::Bits16 => {
                ((fraction * 65535.0).round() as u16).to_le_bytes().to_vec()
            }
        };
        let mut cursor = cursor(&bytes);
        quantized(&mut cursor, quantization, minimum, maximum).expect("bytes are long enough")
    }

    #[test]
    fn quantized_round_trips_8_and_16_bit_values() {
        for (quantization, tolerance) in [
            (ScalarQuantization::Bits8, 1.0 / 255.0),
            (ScalarQuantization::Bits16, 1.0 / 65535.0),
        ] {
            for value in [-2.0, 0.0, 0.25, 1.5, 2.0] {
                let decoded = round_trip(value, -2.0, 2.0, quantization);
                assert!(
                    (decoded - value).abs() <= 4.0 * tolerance,
                    "{quantization:?} round-trip of {value} gave {decoded}"
                );
            }
        }
    }

    #[test]
    fn retained_spline_capacities_fit_four_times_encoded_payload() {
        for count in [17, 33, 129] {
            for quantization in [ScalarQuantization::Bits8, ScalarQuantization::Bits16] {
                let (retained, encoded) =
                    measured_vec3_payload(count, SPLINE[0] | SPLINE[1] | SPLINE[2], quantization);
                assert!(
                    retained <= encoded * DECODED_BLOCK_PAYLOAD_MULTIPLIER,
                    "vec3 {count} points at {quantization:?}: retained {retained} B, encoded {encoded} B"
                );
            }
            let (retained, encoded) =
                measured_vec3_payload(count, SPLINE[0] | STATIC[1], ScalarQuantization::Bits8);
            assert!(
                retained <= encoded * DECODED_BLOCK_PAYLOAD_MULTIPLIER,
                "mixed vec3 {count} points: retained {retained} B, encoded {encoded} B"
            );
            let (retained, encoded) = measured_rotation_payload(count);
            assert!(
                retained <= encoded * DECODED_BLOCK_PAYLOAD_MULTIPLIER,
                "rotation {count} points: retained {retained} B, encoded {encoded} B"
            );
        }
    }

    #[test]
    fn mixed_static_x_spline_y_uses_original_component_metadata_order() {
        // Original wire order follows component index: static x, then spline y bounds.
        let mut data = spline_fixture(2, 1);
        data.extend_from_slice(&7.0_f32.to_le_bytes());
        data.extend_from_slice(&10.0_f32.to_le_bytes());
        data.extend_from_slice(&20.0_f32.to_le_bytes());
        data.extend_from_slice(&[0, 255]);
        let mut cursor = Cursor {
            data: &data,
            position: 4,
        };

        let track = read_vec3(
            &mut cursor,
            STATIC[0] | SPLINE[1],
            ScalarQuantization::Bits8,
            0.0,
        )
        .unwrap();
        assert_eq!(eval_vec3(&track, 0.0), [7.0, 10.0, 0.0]);
        assert_eq!(eval_vec3(&track, 255.0), [7.0, 20.0, 0.0]);
        let Vec3Track::Spline { components, .. } = track else {
            panic!("expected spline track");
        };
        assert_eq!(components[0], [7.0]);
        assert_eq!(components[1], [10.0, 20.0]);
        assert_eq!(components[2], [0.0]);
    }

    #[test]
    fn truncated_spline_points_fail_before_point_capacity_reservation() {
        let count = 129;
        let mut vec3_data = spline_fixture(count, 3);
        vec3_data.extend(std::iter::repeat_n(0, 3 * 8));
        let mut vec3_cursor = Cursor {
            data: &vec3_data,
            position: 4,
        };
        let error = read_vec3(
            &mut vec3_cursor,
            SPLINE[0] | SPLINE[1] | SPLINE[2],
            ScalarQuantization::Bits8,
            0.0,
        )
        .expect_err("missing point payload is rejected before point vectors reserve")
        .to_string();
        assert!(error.contains("past end"), "{error}");

        let rotation_data = spline_fixture(count, 3);
        let mut rotation_cursor = Cursor {
            data: &rotation_data,
            position: 4,
        };
        let error = read_rotation(
            &mut rotation_cursor,
            0x10,
            RotationQuantization::ThreeComp40,
        )
        .expect_err("missing rotation payload is rejected before point vector reserves")
        .to_string();
        assert!(error.contains("past end"), "{error}");
    }

    #[test]
    fn b_spline_linear_interpolates_hand_computed_points() {
        let track = Vec3Track::Spline {
            knots: Knots {
                degree: 1,
                knots: vec![0, 0, 1, 1],
            },
            components: [vec![0.0, 10.0], vec![0.0, 10.0], vec![0.0, 10.0]],
        };
        assert_eq!(eval_vec3(&track, 0.0), [0.0, 0.0, 0.0]);
        assert_eq!(eval_vec3(&track, 0.5), [5.0, 5.0, 5.0]);
        assert_eq!(eval_vec3(&track, 1.0), [10.0, 10.0, 10.0]);
    }

    #[test]
    fn b_spline_quadratic_matches_hand_computed_weights() {
        let track = Vec3Track::Spline {
            knots: Knots {
                degree: 2,
                knots: vec![0, 0, 0, 1, 1, 1],
            },
            components: [
                vec![0.0, 10.0, 20.0],
                vec![0.0, 10.0, 20.0],
                vec![0.0, 10.0, 20.0],
            ],
        };
        let midpoint = eval_vec3(&track, 0.5);
        for component in midpoint {
            assert!((component - 10.0).abs() < 1e-6, "got {midpoint:?}");
        }
        assert_eq!(eval_vec3(&track, 0.0), [0.0, 0.0, 0.0]);
        assert_eq!(eval_vec3(&track, 1.0), [20.0, 20.0, 20.0]);
    }

    #[test]
    fn b_spline_single_point_is_constant() {
        let track = Vec3Track::Spline {
            knots: Knots {
                degree: 1,
                knots: vec![0, 0, 1],
            },
            components: [vec![5.0], vec![6.0], vec![7.0]],
        };
        for u in [0.0, 0.25, 0.75] {
            assert_eq!(eval_vec3(&track, u), [5.0, 6.0, 7.0]);
        }

        let rotation = QuatTrack::Spline {
            knots: Knots {
                degree: 1,
                knots: vec![0, 0, 1],
            },
            points: vec![[0.0, 0.0, 0.0, 1.0]],
        };
        assert_eq!(eval_quat(&rotation, 0.5), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn eval_quat_normalizes_blended_control_points() {
        let track = QuatTrack::Static([0.0, 0.0, 0.0, 2.0]);
        let quaternion = eval_quat(&track, 0.0);
        let norm = quaternion
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        assert!((norm - 1.0).abs() <= 1e-6, "norm {norm}");
    }

    #[test]
    fn read_block_skips_float_track_masks() {
        // Translation x is static; rotation and scale are identity. The four float-track mask
        // bytes sit between the transform mask and the static value.
        let mut data = vec![0x04, 0x01, 0x00, 0x00, 0xAA, 0xBB, 0xCC, 0xDD];
        data.extend_from_slice(&3.0_f32.to_le_bytes());
        let tracks = read_block(&data, 0, 1, 4).expect("block parses");

        assert_eq!(tracks.len(), 1);
        match &tracks[0].translation {
            Vec3Track::Static(value) => assert_eq!(*value, [3.0, 0.0, 0.0]),
            Vec3Track::Spline { .. } => panic!("expected a static translation"),
        }
        match &tracks[0].scale {
            Vec3Track::Static(value) => assert_eq!(*value, [1.0, 1.0, 1.0]),
            Vec3Track::Spline { .. } => panic!("expected a static scale"),
        }
        assert_eq!(eval_quat(&tracks[0].rotation, 0.0), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn read_knots_rejects_too_few_control_points_for_the_degree() {
        let data = [1, 0, 3, 0, 0, 0, 0, 0, 0];
        let error = read_knots(&mut cursor(&data))
            .expect_err("two control points cannot support degree three")
            .to_string();
        assert!(error.contains("2 control points"), "got {error:?}");
    }

    #[test]
    fn read_knots_rejects_a_knot_domain_that_excludes_frame_zero() {
        let data = [1, 0, 1, 5, 5, 6, 6];
        let error = read_knots(&mut cursor(&data))
            .expect_err("a domain starting above frame 0 is rejected")
            .to_string();
        assert!(error.contains("frame 0"), "got {error:?}");
    }

    #[test]
    fn read_knots_rejects_an_empty_knot_domain() {
        let data = [1, 0, 1, 0, 0, 0, 0];
        let error = read_knots(&mut cursor(&data))
            .expect_err("an empty knot domain is rejected")
            .to_string();
        assert!(error.contains("frame 0"), "got {error:?}");
    }

    fn static_track_animation(num_frames: i32) -> SplineAnimation {
        SplineAnimation {
            duration: 1.0,
            num_transform_tracks: 1,
            num_float_tracks: 0,
            extracted_motion: None,
            num_frames,
            num_blocks: 2,
            max_frames_per_block: 256,
            frame_duration: 1.0 / 30.0,
            block_offsets: vec![0, 0],
            float_block_offsets: Vec::new(),
            // One valid static THREECOMP40 track, shared by the duplicate test offsets.
            data: vec![0x04, 0, 0x01, 0, 0, 0, 0, 0, 0],
            endian: 0,
        }
    }

    #[test]
    fn decoded_rejects_more_frames_than_the_block_layout_holds() {
        let error = |num_frames| {
            Decoded::new(&static_track_animation(num_frames))
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_default()
        };
        assert!(!error(511).contains("hold at most"), "{}", error(511));
        for num_frames in [512, i32::MAX] {
            assert!(
                error(num_frames).contains("hold at most 511"),
                "{}",
                error(num_frames)
            );
        }
    }

    #[test]
    fn decoded_accepts_supported_block_boundary_and_rejects_257() {
        let mut animation = static_track_animation(1);
        animation.num_blocks = 1;
        animation.block_offsets = vec![0];
        animation.max_frames_per_block = 256;
        assert!(Decoded::new(&animation).is_ok());

        animation.max_frames_per_block = 257;
        let error = Decoded::new(&animation)
            .err()
            .expect("block bound rejected")
            .to_string();
        assert!(error.contains("supported limit of 256"), "{error}");
    }

    #[test]
    fn decoded_rejects_memory_budget_and_size_overflow_before_block_allocation() {
        let mut animation = static_track_animation(100_000_000);
        animation.num_blocks = 400_000;
        animation.block_offsets.clear();
        animation.num_transform_tracks = 1;
        animation.num_frames = 100_000_000;
        let error = Decoded::new(&animation)
            .err()
            .expect("decoded-animation budget rejected")
            .to_string();
        assert!(
            error.contains("256 MiB decoded-animation budget"),
            "{error}"
        );

        animation.num_frames = i32::MAX;
        animation.num_blocks = i32::MAX;
        animation.num_transform_tracks = i32::MAX;
        let error = Decoded::new(&animation)
            .err()
            .expect("sample size overflow rejected")
            .to_string();
        assert!(error.contains("decoded-sample size overflow"), "{error}");
    }

    #[test]
    fn decoded_memory_estimate_counts_per_block_tracks_and_payload() {
        let one = decoded_memory_estimate(1, 1, 1, 8).unwrap();
        let two_blocks = decoded_memory_estimate(1, 1, 2, 8).unwrap();
        assert_eq!(
            two_blocks - one,
            std::mem::size_of::<Track>()
                + std::mem::size_of::<TrackMask>()
                + 3 * std::mem::size_of::<Vec<Track>>()
                + 32
        );

        let repeated_block_estimate = decoded_memory_estimate(1, 32_768, 10_000, 128 * 1024)
            .expect("the conservative estimate is representable");
        assert!(repeated_block_estimate > MAX_DECODED_ANIMATION_BYTES);
    }

    #[test]
    fn decoded_rejects_zero_transform_tracks() {
        let mut animation = static_track_animation(1);
        animation.num_transform_tracks = 0;
        animation.data.clear();
        let error = Decoded::new(&animation)
            .err()
            .expect("float-only clips are unsupported")
            .to_string();
        assert!(
            error.contains("without transform tracks are unsupported"),
            "{error}"
        );
    }

    #[test]
    fn decoded_rejects_one_block_i32_max_frame_declaration() {
        let mut animation = static_track_animation(i32::MAX);
        animation.num_blocks = 1;
        animation.block_offsets = vec![0];
        animation.max_frames_per_block = i32::MAX;
        let error = Decoded::new(&animation)
            .err()
            .expect("large declared block rejected")
            .to_string();
        assert!(error.contains("supported limit of 256"), "{error}");
    }

    /// A bound or static value that is not finite quantises every sample to
    /// NaN, so it is rejected where it enters rather than written to a GLB.
    #[test]
    fn read_vec3_rejects_non_finite_bounds_and_static_values() {
        let mut spline = spline_fixture(2, 1);
        spline.extend_from_slice(&f32::NAN.to_le_bytes());
        spline.extend_from_slice(&1.0_f32.to_le_bytes());
        spline.extend_from_slice(&[0, 255]);
        let error = read_vec3(
            &mut Cursor {
                data: &spline,
                position: 4,
            },
            SPLINE[0],
            ScalarQuantization::Bits8,
            0.0,
        )
        .expect_err("a NaN spline bound is rejected")
        .to_string();
        assert!(error.contains("not finite"), "got {error:?}");

        let static_value = f32::INFINITY.to_le_bytes();
        let error = read_vec3(
            &mut cursor(&static_value),
            STATIC[0],
            ScalarQuantization::Bits8,
            0.0,
        )
        .expect_err("an infinite static value is rejected")
        .to_string();
        assert!(error.contains("not finite"), "got {error:?}");
    }

    /// A repeated end knot leaves the outermost span zero-width. A frame at either end of the
    /// domain must use the nearest span with a width, so its sample stays finite and lands on
    /// the clamped end point instead of dividing by zero.
    #[test]
    fn frames_at_the_domain_ends_skip_an_empty_outermost_span() {
        // Five control points at degree three; the last span (knots[4]..knots[5]) is empty.
        let end = Knots {
            degree: 3,
            knots: vec![0, 0, 0, 0, 4, 4, 4, 4, 4],
        };
        assert!(read_knots(&mut cursor(&[4, 0, 3, 0, 0, 0, 0, 4, 4, 4, 4, 4])).is_ok());
        let points = [1.0, 2.0, 3.0, 4.0, 5.0];
        let sample = eval_scalar(&end, &points, 4.0);
        assert!(sample.is_finite(), "end sample {sample}");
        assert!((sample - 4.0).abs() < 1e-5, "end sample {sample}");

        // The mirror case: frame 0 sits where the first span (knots[3]..knots[4]) is empty.
        let start = Knots {
            degree: 3,
            knots: vec![0, 0, 0, 0, 0, 1, 1, 1, 1],
        };
        let sample = eval_scalar(&start, &points, 0.0);
        assert!(sample.is_finite(), "start sample {sample}");
        assert!((sample - 2.0).abs() < 1e-5, "start sample {sample}");
    }

    #[test]
    fn quantized_rejects_bounds_whose_range_overflows() {
        let error = quantized(
            &mut cursor(&[0]),
            ScalarQuantization::Bits8,
            -f32::MAX,
            f32::MAX,
        )
        .expect_err("a range that overflows f32 is rejected")
        .to_string();
        assert!(error.contains("not finite"), "got {error:?}");
    }

    /// Belt and braces at the decode choke point: a track holding a non-finite
    /// value is refused before any clip samples it.
    #[test]
    fn track_finiteness_check_rejects_a_non_finite_control_point() {
        let finite = Track {
            translation: Vec3Track::Spline {
                knots: Knots {
                    degree: 1,
                    knots: vec![0, 0, 1, 1],
                },
                components: [vec![0.0, 1.0], vec![0.0], vec![0.0]],
            },
            rotation: QuatTrack::Static([0.0, 0.0, 0.0, 1.0]),
            scale: Vec3Track::Static([1.0; 3]),
        };
        assert!(track_values_are_finite(&finite));

        let non_finite = Track {
            translation: Vec3Track::Static([f32::NAN, 0.0, 0.0]),
            ..finite
        };
        assert!(!track_values_are_finite(&non_finite));
    }

    /// Finite control points can still extrapolate past `f32::MAX` at a frame beyond the knot
    /// domain; that sample is refused rather than written into the clip.
    #[test]
    fn sample_past_the_knot_domain_that_overflows_is_refused() {
        let track = Track {
            translation: Vec3Track::Spline {
                knots: Knots {
                    degree: 1,
                    knots: vec![0, 0, 1, 1],
                },
                components: [vec![0.0, 3.0e38], vec![0.0], vec![0.0]],
            },
            rotation: QuatTrack::Static([0.0, 0.0, 0.0, 1.0]),
            scale: Vec3Track::Static([1.0; 3]),
        };
        let decoded = Decoded {
            blocks: vec![vec![track]],
            frames_per_block: 2,
        };
        assert_eq!(
            decoded.sample(0, 1).unwrap().translation,
            [3.0e38, 0.0, 0.0]
        );
        let error = decoded.sample(0, 2).unwrap_err().to_string();
        assert!(
            error.contains("track 0") && error.contains("frame 2"),
            "{error}"
        );
    }

    #[test]
    fn read_block_rejects_unsupported_rotation_compression() {
        for (bits, name) in [
            (0_u8, "Polar32"),
            (2, "ThreeComp48"),
            (3, "ThreeComp24"),
            (4, "Straight16"),
            (5, "Uncompressed"),
        ] {
            let data = [bits << 2, 0x00, 0x0F, 0x00, 0, 0, 0, 0];
            let error = read_block(&data, 0, 1, 0)
                .expect_err("unsupported compression must be rejected")
                .to_string();
            assert!(
                error.contains("unsupported rotation compression") && error.contains(name),
                "expected {name} to be rejected by name, got {error:?}"
            );
        }
    }
}
