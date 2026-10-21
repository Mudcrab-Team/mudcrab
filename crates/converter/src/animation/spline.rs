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
    Ok((count_minus_one + 1, Knots { degree, knots }))
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
    Ok(minimum + (maximum - minimum) * fraction)
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
                *component = cursor.f32()?;
            }
        }
        return Ok(Vec3Track::Static(value));
    }

    let (count, knots) = read_knots(cursor)?;
    cursor.align(4);
    let mut bounds = [(0.0, 0.0); 3];
    let mut components: [Vec<f32>; 3] = Default::default();
    for index in 0..3 {
        if flags & SPLINE[index] != 0 {
            bounds[index] = (cursor.f32()?, cursor.f32()?);
        } else if flags & STATIC[index] != 0 {
            components[index].push(cursor.f32()?);
        } else {
            components[index].push(identity);
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
        let points = (0..count)
            .map(|_| read_quat(cursor, quantization))
            .collect::<Result<Vec<_>>>()?;
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
        decoded.push(Track {
            translation,
            rotation,
            scale,
        });
    }
    Ok(decoded)
}

/// NURBS book A2.1: span index for `u` with `n` control points.
fn find_span(degree: usize, u: f32, n: usize, knots: &[u8]) -> usize {
    if n <= 1 {
        return 0;
    }
    if u >= knots[n] as f32 {
        return n - 1;
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

    pub fn sample(&self, track: usize, frame: usize) -> TransformSample {
        let (block, u) = self.locate(frame);
        let track = &self.blocks[block][track];
        TransformSample {
            translation: eval_vec3(&track.translation, u),
            rotation: eval_quat(&track.rotation, u),
            scale: eval_vec3(&track.scale, u),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(data: &[u8]) -> Cursor<'_> {
        Cursor { data, position: 0 }
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
