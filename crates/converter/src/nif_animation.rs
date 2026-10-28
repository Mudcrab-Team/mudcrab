//! NIF controller sequences (`Open`, `Close`, ...) exported as glTF animation clips.
//!
//! A Skyrim NIF that animates - a load door, a gate, a secret door - carries a
//! `NiControllerManager` whose `NiControllerSequence` list names the model's clips (`Open`,
//! `Close`, `playanim01`, ...). Every sequence lists `ControlledBlock`s that pair an animated
//! node's *name* with a `NiTransformInterpolator`, and the interpolator points at the
//! `NiTransformData` that holds the keys.
//!
//! The vendored NIF parser has no arms for those three types (`NiTransformController`,
//! `NiTransformInterpolator` and `NiTransformData` are empty stubs), so this module decodes the
//! blocks from their raw bytes. The format reference is niftools/nifxml's `nif.xml`:
//! <https://github.com/niftools/nifxml/blob/292bb9403cbf4052c58d66e80906b6bde1700779/nif.xml> (`NiKeyframeData`,
//! `NiTransformData`, `KeyGroup`, `NiTransformInterpolator`, `NiControllerSequence`,
//! `NiControllerManager`, `NiDefaultAVObjectPalette`, `AVObject`).
//! The decoder targets Skyrim's 20.2.0.7 / Bethesda 100 layout. Unknown key types,
//! multiple XYZ rotation key sets and trailing bytes are rejected with warnings;
//! the model keeps its authored static pose for unreadable tracks.
//!
//! ```text
//! NiTransformData
//!   u32 Num Rotation Keys
//!   [u32 Rotation Type]                      # written only when the count is nonzero
//!   rotation keys                            # 4 = XYZ_ROTATION_KEY: three euler KeyGroups
//!   u32 Num Translation Keys
//!   [u32 Translation Type]                   # written only when the count is nonzero
//!   translation keys                         # f32 time + three floats
//!   u32 Num Scale Keys
//!   [u32 Scale Type]                         # written only when the count is nonzero
//!   scale keys                               # f32 time + one float
//! ```
//!
//! **A group writes its interpolation type only when it has keys.** That one rule is what makes
//! the blocks line up end to end: a rotation-only door's transform data ends in a bare `0, 0`
//! (no translation keys, no scale keys), a sliding door that does not rotate starts
//! `0, 95, 1, ...` (no rotation keys, 95 linear translation keys), and the word after a
//! zero-count group is always the next group's count. Reading the rotation type unconditionally
//! misreads 53 transform-data blocks across 13 door models in the converted tree.
//!
//! `XYZ_ROTATION_KEY` stores one `KeyGroup` per euler axis (X, then Y, then Z) instead of
//! quaternions; every other rotation type stores quaternion keys. A `KeyGroup` is
//! `u32 Num Keys, u32 Key Type` followed by its keys, and a key is `f32 Time, <value>` plus
//! `f32 Forward, f32 Backward` for `QUADRATIC_KEY` or three TBC floats for `TBC_KEY`. Times are
//! seconds from the sequence's start; euler values are radians about the node's own axes.
//!
//! One more shape is worth naming: a `NiTransformInterpolator`'s own translation/rotation/scale
//! are the "invalid" sentinel `-FLT_MAX` on animated doors, so the node's rest transform in the
//! exported scene *is* the pose and the curves are absolute - the curve at t = 0 equals the
//! node's rest transform.
//!
//! Translation and scale retain their authored interpolation: CONST_KEY emits STEP, linear keys
//! emit LINEAR, and quadratic/TBC keys emit CUBICSPLINE with time-normalized derivatives.
//! Scalar Euler curves use the same Hermite evaluation before the existing 30 Hz quaternion
//! bake. Pure constant rotation channels emit STEP. Node transforms remain local; the
//! Creation-to-runtime basis change lives on the scene root.

use crate::glb::{rebuild_glb, split_glb};
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use project_wormhole_nif::nif_header::NifHeader;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
};

/// Per-kind mesh cache identity: source quaternion order, held endpoints and bounded rotation grids.
pub(crate) const PRODUCER_VERSION: u32 = 6;

/// Sampling rate of a baked rotation channel, in samples per second.
const ROTATION_SAMPLES_PER_SECOND: f32 = 30.0;

/// Decoder resource validation, shared by every rotation track in one source NIF.
/// One million candidate slots bound the sampled time/pose storage to about 20 MB,
/// excluding source keys. Authored times, grid points and endpoints all consume slots.
const ROTATION_SAMPLE_BUDGET: usize = 1_000_000;

/// The sampled form of one animated node inside one clip.
#[derive(Debug, Clone, PartialEq)]
struct ClipTrack {
    node: String,
    target: TrackTarget,
    rotation: Option<SampledChannel<[f32; 4]>>,
    translation: Option<SampledChannel<[f32; 3]>>,
    scale: Option<SampledChannel<f32>>,
}

/// A palette pointer is authoritative even when its object was not exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackTarget {
    Name,
    SourceBlock(u32),
    AmbiguousPalette,
}

impl ClipTrack {
    fn is_empty(&self) -> bool {
        self.rotation.is_none() && self.translation.is_none() && self.scale.is_none()
    }
}

/// A channel as it is written to glTF: parallel time and value arrays.
#[derive(Debug, Clone, PartialEq)]
struct SampledChannel<T> {
    times: Vec<f32>,
    /// CUBICSPLINE stores incoming derivative, value and outgoing derivative per time.
    values: Vec<T>,
    interpolation: &'static str,
}

/// One `NiControllerSequence`, ready to be written as a glTF animation.
#[derive(Debug, Clone, PartialEq)]
struct Clip {
    name: String,
    tracks: Vec<ClipTrack>,
}

/// Key interpolation, as stored in a group's `Key Type` word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyType {
    Linear = 1,
    Quadratic = 2,
    Tbc = 3,
    XyzRotation = 4,
    Const = 5,
}

impl KeyType {
    fn from_bits(bits: u32) -> Result<Self> {
        match bits {
            1 => Ok(KeyType::Linear),
            2 => Ok(KeyType::Quadratic),
            3 => Ok(KeyType::Tbc),
            4 => Ok(KeyType::XyzRotation),
            5 => Ok(KeyType::Const),
            other => Err(color_eyre::eyre::eyre!("unknown NIF key type {other}")),
        }
    }

    /// Whether the curve steps between keys instead of interpolating.
    fn is_step(self) -> bool {
        matches!(self, KeyType::Const)
    }

    fn is_cubic(self) -> bool {
        matches!(self, KeyType::Quadratic | KeyType::Tbc)
    }

    /// Bytes one key of this type occupies for a four-float quaternion value.
    fn quaternion_stride(self) -> usize {
        match self {
            KeyType::Quadratic => 52,
            KeyType::Tbc => 32,
            _ => 20,
        }
    }
}

/// A single-float curve.
#[derive(Debug, Clone, PartialEq)]
struct ScalarCurve {
    key_type: KeyType,
    keys: Vec<(f32, f32)>,
    tangents: Vec<(f32, f32)>,
}

/// A three-float curve.
#[derive(Debug, Clone, PartialEq)]
struct Vec3Curve {
    key_type: KeyType,
    keys: Vec<(f32, [f32; 3])>,
    tangents: Vec<Tangents<3>>,
}

/// A quaternion curve, components ordered `x, y, z, w`.
#[derive(Debug, Clone, PartialEq)]
struct QuatCurve {
    key_type: KeyType,
    keys: Vec<(f32, [f32; 4])>,
}

/// The rotation half of `NiTransformData`: quaternion keys or three euler curves.
#[derive(Debug, Clone, PartialEq)]
enum RotationKeys {
    Quaternion(QuatCurve),
    Euler {
        x: ScalarCurve,
        y: ScalarCurve,
        z: ScalarCurve,
    },
}

/// One `NiTransformData` block, decoded.
#[derive(Debug, Clone, PartialEq)]
struct TransformKeys {
    rotation: RotationKeys,
    translation: Vec3Curve,
    scale: ScalarCurve,
}

/// The index of the right key of the pair bracketing `time`, and the blend towards it.
///
/// Only called for a `time` strictly inside the keyed range, so a bracketing pair exists.
fn bracket<V>(keys: &[(f32, V)], time: f32) -> (usize, f32) {
    let index = keys.partition_point(|key| key.0 <= time);
    let (left_time, _) = keys[index - 1];
    let (right_time, _) = keys[index];
    if right_time <= left_time {
        return (index, 0.0);
    }
    (index, (time - left_time) / (right_time - left_time))
}

impl ScalarCurve {
    /// The curve's value at `time`, held flat outside the keyed range.
    fn value_at(&self, time: f32) -> f32 {
        let Some((first_time, first_value)) = self.keys.first().copied() else {
            return 0.0;
        };
        if time <= first_time {
            return first_value;
        }
        let Some(&(last_time, last_value)) = self.keys.last() else {
            return first_value;
        };
        if time >= last_time {
            return last_value;
        }
        let (index, blend) = bracket(&self.keys, time);
        let (_, left) = self.keys[index - 1];
        let (_, right) = self.keys[index];
        if self.key_type.is_step() {
            return left;
        }
        if self.key_type.is_cubic() {
            let span = self.keys[index].0 - self.keys[index - 1].0;
            return hermite(
                left,
                right,
                self.tangents[index - 1].1,
                self.tangents[index].0,
                span,
                blend,
            );
        }
        left + (right - left) * blend
    }

    fn times(&self) -> impl Iterator<Item = f32> + '_ {
        self.keys.iter().map(|key| key.0)
    }
}

impl Vec3Curve {
    /// The curve's value at `time`, held flat outside the keyed range.
    fn value_at(&self, time: f32) -> [f32; 3] {
        let Some((first_time, first_value)) = self.keys.first().copied() else {
            return [0.0; 3];
        };
        if time <= first_time {
            return first_value;
        }
        let Some(&(last_time, last_value)) = self.keys.last() else {
            return first_value;
        };
        if time >= last_time {
            return last_value;
        }
        let (index, blend) = bracket(&self.keys, time);
        let (_, left) = self.keys[index - 1];
        let (_, right) = self.keys[index];
        if self.key_type.is_step() {
            return left;
        }
        std::array::from_fn(|axis| {
            if self.key_type.is_cubic() {
                let span = self.keys[index].0 - self.keys[index - 1].0;
                hermite(
                    left[axis],
                    right[axis],
                    self.tangents[index - 1].1[axis],
                    self.tangents[index].0[axis],
                    span,
                    blend,
                )
            } else {
                left[axis] + (right[axis] - left[axis]) * blend
            }
        })
    }
}

impl QuatCurve {
    /// The curve's value at `time`, held flat outside the keyed range.
    fn value_at(&self, time: f32) -> [f32; 4] {
        let Some((first_time, first_value)) = self.keys.first().copied() else {
            return [0.0, 0.0, 0.0, 1.0];
        };
        if time <= first_time {
            return first_value;
        }
        let Some(&(last_time, last_value)) = self.keys.last() else {
            return first_value;
        };
        if time >= last_time {
            return last_value;
        }
        let (index, blend) = bracket(&self.keys, time);
        let (_, left) = self.keys[index - 1];
        let (_, right) = self.keys[index];
        if self.key_type.is_step() {
            return left;
        }
        // Shortest-path linear blend, normalised: the source keys of a quaternion curve are
        // already the authored poses, so this only fills in the resampled times between them.
        let mut right = right;
        if dot4(left, right) < 0.0 {
            right = [-right[0], -right[1], -right[2], -right[3]];
        }
        normalize4([
            left[0] + (right[0] - left[0]) * blend,
            left[1] + (right[1] - left[1]) * blend,
            left[2] + (right[2] - left[2]) * blend,
            left[3] + (right[3] - left[3]) * blend,
        ])
    }
}

fn dot4(left: [f32; 4], right: [f32; 4]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2] + left[3] * right[3]
}

fn normalize4(quaternion: [f32; 4]) -> [f32; 4] {
    let length = dot4(quaternion, quaternion).sqrt();
    if length <= f32::EPSILON || !length.is_finite() {
        return [0.0, 0.0, 0.0, 1.0];
    }
    quaternion.map(|component| component / length)
}

/// The euler triple of an `XYZ_ROTATION_KEY` group as a quaternion, `x, y, z, w`.
///
/// The three axis angles compose as `Rz(z) * Ry(y) * Rx(x)` in the node's own frame, which is
/// what the authored rest transforms in the exported scenes show: a node whose Z curve is
/// `-0.15` has the rest quaternion of a `-0.15` rad turn about Z, and a node whose X curve is
/// the constant `3.14159` has the rest quaternion `[1, 0, 0, 0]`.
fn euler_xyz_to_quaternion([x, y, z]: [f32; 3]) -> [f32; 4] {
    let (sx, cx) = (x * 0.5).sin_cos();
    let (sy, cy) = (y * 0.5).sin_cos();
    let (sz, cz) = (z * 0.5).sin_cos();
    // qz * qy * qx
    let qx = [sx, 0.0, 0.0, cx];
    let qy = [0.0, sy, 0.0, cy];
    let qz = [0.0, 0.0, sz, cz];
    let qzqy = [
        qz[3] * qy[0] + qz[0] * qy[3] + qz[1] * qy[2] - qz[2] * qy[1],
        qz[3] * qy[1] - qz[0] * qy[2] + qz[1] * qy[3] + qz[2] * qy[0],
        qz[3] * qy[2] + qz[0] * qy[1] - qz[1] * qy[0] + qz[2] * qy[3],
        qz[3] * qy[3] - qz[0] * qy[0] - qz[1] * qy[1] - qz[2] * qy[2],
    ];
    normalize4([
        qzqy[3] * qx[0] + qzqy[0] * qx[3] + qzqy[1] * qx[2] - qzqy[2] * qx[1],
        qzqy[3] * qx[1] - qzqy[0] * qx[2] + qzqy[1] * qx[3] + qzqy[2] * qx[0],
        qzqy[3] * qx[2] + qzqy[0] * qx[1] - qzqy[1] * qx[0] + qzqy[2] * qx[3],
        qzqy[3] * qx[3] - qzqy[0] * qx[0] - qzqy[1] * qx[1] - qzqy[2] * qx[2],
    ])
}

// ---------------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------------

/// A bounds-checked cursor over one NIF block's bytes.
struct BlockCursor<'a> {
    bytes: &'a [u8],
    position: usize,
    what: &'static str,
}

impl<'a> BlockCursor<'a> {
    fn new(bytes: &'a [u8], what: &'static str) -> Self {
        Self {
            bytes,
            position: 0,
            what,
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        ensure!(
            self.remaining() >= count,
            "{} block ended {} bytes early",
            self.what,
            count - self.remaining()
        );
        let slice = &self.bytes[self.position..self.position + count];
        self.position += count;
        Ok(slice)
    }

    fn skip(&mut self, count: usize) -> Result<()> {
        self.take(count).map(|_| ())
    }

    fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn quaternion(&mut self) -> Result<[f32; 4]> {
        // nif.xml's Quaternion stores w,x,y,z; curves and glTF use x,y,z,w.
        let [w, x, y, z] = [self.f32()?, self.f32()?, self.f32()?, self.f32()?];
        Ok([x, y, z, w])
    }

    /// Requires that the whole block was consumed: a layout that leaves bytes behind is wrong.
    fn finish(&self) -> Result<()> {
        ensure!(
            self.remaining() == 0,
            "{} block has {} unread trailing bytes",
            self.what,
            self.remaining()
        );
        Ok(())
    }
}

fn read_key_header(cursor: &mut BlockCursor<'_>, what: &'static str) -> Result<(usize, KeyType)> {
    let count = cursor.u32()? as usize;
    let key_type = KeyType::from_bits(cursor.u32()?)?;
    ensure!(
        key_type != KeyType::XyzRotation || count == 0,
        "{what} uses XYZ_ROTATION_KEY, which is a rotation key type only"
    );
    Ok((count, key_type))
}

/// Decoded numeric keys keep the two one-sided derivatives at each authored key.
/// NIF's quadratic Forward field is incoming and Backward is outgoing; both are
/// normalized segment tangents. glTF requires derivatives per second instead.
type Tangents<const N: usize> = ([f32; N], [f32; N]);

struct NumericKey<const N: usize> {
    time: f32,
    value: [f32; N],
    incoming: [f32; N],
    outgoing: [f32; N],
    tbc: [f32; 3],
}

struct NumericCurve<const N: usize> {
    keys: Vec<(f32, [f32; N])>,
    tangents: Vec<Tangents<N>>,
}

fn read_numeric_keys<const N: usize>(
    cursor: &mut BlockCursor<'_>,
    count: usize,
    key_type: KeyType,
    what: &str,
) -> Result<NumericCurve<N>> {
    let stride = 4
        * (1 + N
            + match key_type {
                KeyType::Quadratic => 2 * N,
                KeyType::Tbc => 3,
                _ => 0,
            });
    ensure!(
        count.saturating_mul(stride) <= cursor.remaining(),
        "{what} declares {count} keys, more than the block holds"
    );
    let mut raw = Vec::with_capacity(count);
    for _ in 0..count {
        let mut key = NumericKey {
            time: cursor.f32()?,
            value: [0.0; N],
            incoming: [0.0; N],
            outgoing: [0.0; N],
            tbc: [0.0; 3],
        };
        for value in &mut key.value {
            *value = cursor.f32()?;
        }
        if key_type == KeyType::Quadratic {
            for value in &mut key.incoming {
                *value = cursor.f32()?;
            }
            for value in &mut key.outgoing {
                *value = cursor.f32()?;
            }
        } else if key_type == KeyType::Tbc {
            for value in &mut key.tbc {
                *value = cursor.f32()?;
            }
        }
        ensure!(
            key.time.is_finite()
                && key
                    .value
                    .iter()
                    .chain(&key.incoming)
                    .chain(&key.outgoing)
                    .chain(&key.tbc)
                    .all(|v| v.is_finite()),
            "{what} has a non-finite key, tangent or TBC parameter"
        );
        raw.push(key);
    }
    // Sort the entire record: values and tangent payloads must never be separated.
    raw.sort_by(|left, right| left.time.total_cmp(&right.time));
    let mut tangents = Vec::new();
    if key_type.is_cubic() {
        ensure!(
            raw.windows(2).all(|pair| pair[1].time > pair[0].time),
            "{what} has duplicate cubic key times"
        );
        tangents.reserve(count);
        for (index, key) in raw.iter().enumerate() {
            let prev = &raw[index.saturating_sub(1)];
            let next = &raw[(index + 1).min(raw.len() - 1)];
            let mut incoming = key.incoming;
            let mut outgoing = key.outgoing;
            if key_type == KeyType::Tbc && count > 1 {
                let [tension, continuity, bias] = key.tbc;
                let common = 1.0 - tension;
                let a = common * (1.0 - continuity) * (1.0 + bias);
                let b = common * (1.0 + continuity) * (1.0 - bias);
                let c = common * (1.0 + continuity) * (1.0 + bias);
                let d = common * (1.0 - continuity) * (1.0 - bias);
                for axis in 0..N {
                    if index == 0 || index + 1 == count {
                        let delta = next.value[axis] - prev.value[axis];
                        incoming[axis] = delta * (a + b) * 0.5;
                        outgoing[axis] = delta * (c + d) * 0.5;
                    } else {
                        let before = key.value[axis] - prev.value[axis];
                        let after = next.value[axis] - key.value[axis];
                        let span = next.time - prev.time;
                        incoming[axis] = (a * before + b * after) * ((key.time - prev.time) / span);
                        outgoing[axis] = (c * before + d * after) * ((next.time - key.time) / span);
                    }
                }
            }
            // Endpoints clamp outside the keyed range; the unused derivative is zero.
            for axis in 0..N {
                incoming[axis] = if index == 0 {
                    0.0
                } else {
                    incoming[axis] / (key.time - prev.time)
                };
                outgoing[axis] = if index + 1 == count {
                    0.0
                } else {
                    outgoing[axis] / (next.time - key.time)
                };
            }
            ensure!(
                incoming.iter().chain(&outgoing).all(|v| v.is_finite()),
                "{what} tangent overflows during time conversion"
            );
            tangents.push((incoming, outgoing));
        }
    }
    Ok(NumericCurve {
        keys: raw.into_iter().map(|key| (key.time, key.value)).collect(),
        tangents,
    })
}

/// Reads one KeyGroup, including the payload shaping its interpolation.
fn read_scalar_group(cursor: &mut BlockCursor<'_>, what: &'static str) -> Result<ScalarCurve> {
    let (count, key_type) = read_key_header(cursor, what)?;
    read_scalar_keys(cursor, count, key_type, what)
}

fn read_scalar_keys(
    cursor: &mut BlockCursor<'_>,
    count: usize,
    key_type: KeyType,
    what: &str,
) -> Result<ScalarCurve> {
    let curve = read_numeric_keys::<1>(cursor, count, key_type, what)?;
    Ok(ScalarCurve {
        key_type,
        keys: curve
            .keys
            .into_iter()
            .map(|(time, [value])| (time, value))
            .collect(),
        tangents: curve
            .tangents
            .into_iter()
            .map(|([incoming], [outgoing])| (incoming, outgoing))
            .collect(),
    })
}

/// Reads a run of quaternion keys: the count and type come from the `NiTransformData` rotation
/// header rather than from a `KeyGroup` of their own.
fn read_quaternion_keys(
    cursor: &mut BlockCursor<'_>,
    count: usize,
    key_type: KeyType,
    what: &'static str,
) -> Result<QuatCurve> {
    let stride = key_type.quaternion_stride();
    ensure!(
        count.saturating_mul(stride) <= cursor.remaining(),
        "{what} declares {count} keys, more than the block holds"
    );
    let mut keys = Vec::with_capacity(count);
    for _ in 0..count {
        let time = cursor.f32()?;
        let value = cursor.quaternion()?;
        cursor.skip(stride - 20)?;
        keys.push((time, value));
    }
    sort_keys(&mut keys);
    Ok(QuatCurve { key_type, keys })
}

/// Reads an empty-or-keyed group of the shape `u32 count, [u32 type], keys`.
///
/// The interpolation type word is written only when the group has keys - every rotation-only door
/// model ends its transform data with a bare `0, 0` - so an empty group is a single count word.
fn read_optional_group<T>(
    cursor: &mut BlockCursor<'_>,
    what: &'static str,
    empty: T,
    read: impl FnOnce(&mut BlockCursor<'_>, usize, KeyType) -> Result<T>,
) -> Result<T> {
    let count = cursor.u32()? as usize;
    if count == 0 {
        return Ok(empty);
    }
    let key_type = KeyType::from_bits(cursor.u32()?)?;
    ensure!(
        key_type != KeyType::XyzRotation,
        "{what} uses XYZ_ROTATION_KEY, which is a rotation key type only"
    );
    read(cursor, count, key_type)
}

/// Decodes a whole `NiTransformData` block, consuming every byte of it.
///
/// A block that does not fit the fitted layout exactly is an error: the caller drops that node's
/// track and warns, rather than exporting a guess.
fn decode_transform_data(bytes: &[u8]) -> Result<TransformKeys> {
    let mut cursor = BlockCursor::new(bytes, "NiTransformData");

    let rotation_count = cursor.u32()? as usize;
    let rotation = if rotation_count == 0 {
        // A group with no keys writes no interpolation type at all, so the next word is already
        // the translation count. This is the shape of every model that animates without rotating:
        // a sliding secret door, an amulet hanging off a door.
        RotationKeys::Quaternion(QuatCurve {
            key_type: KeyType::Linear,
            keys: Vec::new(),
        })
    } else {
        let rotation_type = KeyType::from_bits(cursor.u32()?)?;
        if rotation_type == KeyType::XyzRotation {
            ensure!(
                rotation_count == 1,
                "XYZ_ROTATION_KEY transform data with {rotation_count} rotation key sets is not \
                 supported by the Skyrim transform decoder"
            );
            let x = read_scalar_group(&mut cursor, "NiTransformData euler X")?;
            let y = read_scalar_group(&mut cursor, "NiTransformData euler Y")?;
            let z = read_scalar_group(&mut cursor, "NiTransformData euler Z")?;
            RotationKeys::Euler { x, y, z }
        } else {
            RotationKeys::Quaternion(read_quaternion_keys(
                &mut cursor,
                rotation_count,
                rotation_type,
                "NiTransformData rotation",
            )?)
        }
    };

    let translation = read_optional_group(
        &mut cursor,
        "translation keys",
        Vec3Curve {
            key_type: KeyType::Linear,
            tangents: Vec::new(),
            keys: Vec::new(),
        },
        |cursor, count, key_type| {
            let curve = read_numeric_keys::<3>(cursor, count, key_type, "translation")?;
            Ok(Vec3Curve {
                key_type,
                keys: curve.keys,
                tangents: curve.tangents,
            })
        },
    )?;

    let scale = read_optional_group(
        &mut cursor,
        "scale keys",
        ScalarCurve {
            key_type: KeyType::Linear,
            tangents: Vec::new(),
            keys: Vec::new(),
        },
        |cursor, count, key_type| read_scalar_keys(cursor, count, key_type, "scale"),
    )?;

    cursor.finish()?;
    let keys = TransformKeys {
        rotation,
        translation,
        scale,
    };
    // A non-finite key would be written straight into the glTF as a NaN and poison the reader;
    // the block is rejected instead. Sorting afterwards is safe because times are finite, and it
    // makes the sampled curve independent of the order the exporter wrote the keys in.
    validate(&keys)?;
    Ok(keys)
}

fn sort_keys<V>(keys: &mut [(f32, V)]) {
    keys.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn validate(keys: &TransformKeys) -> Result<()> {
    let check = |what: &str, time: f32, values: &[f32]| -> Result<()> {
        ensure!(time.is_finite(), "{what} key time {time} is not finite");
        for value in values {
            ensure!(value.is_finite(), "{what} key value {value} is not finite");
        }
        Ok(())
    };
    match &keys.rotation {
        RotationKeys::Quaternion(curve) => {
            for (time, value) in &curve.keys {
                check("rotation", *time, value)?;
            }
        }
        RotationKeys::Euler { x, y, z } => {
            for (axis, curve) in [("X", x), ("Y", y), ("Z", z)] {
                for (time, value) in &curve.keys {
                    check(&format!("rotation {axis}"), *time, &[*value])?;
                }
            }
        }
    }
    for (time, value) in &keys.translation.keys {
        check("translation", *time, value)?;
    }
    for (time, value) in &keys.scale.keys {
        check("scale", *time, &[*value])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// NIF block walking
// ---------------------------------------------------------------------------------------------

/// One `NiControllerSequence`, reduced to what a clip needs.
struct SequenceSpec {
    name: String,
    stop_time: f32,
    targets: Vec<SequenceTarget>,
}

struct SequenceTarget {
    node: String,
    interpolator: usize,
    target: TrackTarget,
}

fn blocks_of<'a>(block_bytes: &'a [u8], header: &NifHeader) -> Result<Vec<&'a [u8]>> {
    let mut slices = Vec::with_capacity(header.block_size_index.len());
    let mut rest = block_bytes;
    for (index, size) in header.block_size_index.iter().enumerate() {
        let size = *size as usize;
        ensure!(
            rest.len() >= size,
            "NIF block {index} is truncated ({} bytes left for {size})",
            rest.len()
        );
        let (block, remaining) = rest.split_at(size);
        slices.push(block);
        rest = remaining;
    }
    Ok(slices)
}

fn type_of(header: &NifHeader, index: usize) -> &str {
    header.get_block_type(index).unwrap_or("<invalid>")
}

fn string_of(header: &NifHeader, index: u32) -> Option<String> {
    let index = usize::try_from(index).ok()?;
    header.strings.get(index).map(|name| name.0.clone())
}

/// Reads one `NiControllerSequence` block.
fn read_sequence(block: &[u8], header: &NifHeader) -> Result<SequenceSpec> {
    let mut cursor = BlockCursor::new(block, "NiControllerSequence");
    let name_index = cursor.u32()?;
    let controlled_count = cursor.u32()? as usize;
    cursor.skip(4)?; // array grow by
    ensure!(
        controlled_count.saturating_mul(29) <= cursor.remaining(),
        "NiControllerSequence declares {controlled_count} controlled blocks, more than the block \
         holds"
    );
    let mut targets = Vec::with_capacity(controlled_count);
    for _ in 0..controlled_count {
        let interpolator = cursor.u32()?;
        cursor.skip(4)?; // controller
        cursor.skip(1)?; // priority
        let node_name = cursor.u32()?;
        cursor.skip(16)?; // property type, controller type, controller id, interpolator id
        if let Some(node) = string_of(header, node_name) {
            targets.push(SequenceTarget {
                node,
                interpolator: interpolator as usize,
                target: TrackTarget::Name,
            });
        }
    }
    cursor.skip(4)?; // weight
    cursor.skip(4)?; // text keys
    cursor.skip(4)?; // cycle type
    cursor.skip(4)?; // frequency
    cursor.skip(4)?; // start time
    let stop_time = cursor.f32()?;
    cursor.skip(4)?; // manager back-reference; the traversed manager list owns palette resolution
    cursor.skip(4)?; // accumulation root name
    let note_count = cursor.u16()? as usize;
    cursor.skip(note_count * 4)?;
    cursor.finish()?;
    Ok(SequenceSpec {
        name: string_of(header, name_index).unwrap_or_else(|| format!("sequence_{name_index}")),
        stop_time,
        targets,
    })
}

/// Reads the sequence lists of every `NiControllerManager` in a NIF.
fn read_sequences(blocks: &[&[u8]], header: &NifHeader) -> (Vec<SequenceSpec>, Vec<String>) {
    let mut sequences = Vec::new();
    let mut warnings = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        if type_of(header, index) != "NiControllerManager" {
            continue;
        }
        let manager = match read_manager(block, blocks.len()) {
            Ok(manager) => manager,
            Err(error) => {
                warnings.push(format!("NiControllerManager block {index}: {error:#}"));
                continue;
            }
        };
        let palette = match manager.object_palette {
            Some(palette_index) => {
                if type_of(header, palette_index) != "NiDefaultAVObjectPalette" {
                    warnings.push(format!(
                        "NiControllerManager block {index} references non-palette block {palette_index}; transform animation omitted"
                    ));
                    continue;
                }
                match read_object_palette(blocks[palette_index], blocks.len()) {
                    Ok(palette) => Some(palette),
                    Err(error) => {
                        warnings.push(format!(
                            "NiControllerManager block {index}, NiDefaultAVObjectPalette block {palette_index}: {error:#}; transform animation omitted"
                        ));
                        continue;
                    }
                }
            }
            None => None,
        };
        for sequence_index in manager.sequences {
            let Some(sequence) = blocks.get(sequence_index) else {
                warnings.push(format!(
                    "NiControllerManager block {index} references missing sequence {sequence_index}"
                ));
                continue;
            };
            if type_of(header, sequence_index) != "NiControllerSequence" {
                warnings.push(format!("NiControllerManager block {index} references non-sequence block {sequence_index}"));
                continue;
            }
            match read_sequence(sequence, header) {
                Ok(mut sequence) => {
                    for target in &mut sequence.targets {
                        target.target = match palette.as_ref().and_then(|p| p.get(&target.node)) {
                            Some(Some(source)) => TrackTarget::SourceBlock(*source),
                            Some(None) => TrackTarget::AmbiguousPalette,
                            None => TrackTarget::Name,
                        };
                    }
                    sequences.push(sequence);
                }
                Err(error) => warnings.push(format!(
                    "NiControllerSequence block {sequence_index}: {error:#}"
                )),
            }
        }
    }
    (sequences, warnings)
}

struct ManagerSpec {
    sequences: Vec<usize>,
    object_palette: Option<usize>,
}

fn check_reference(reference: u32, block_count: usize, what: &str) -> Result<()> {
    ensure!(
        reference == u32::MAX || (reference as usize) < block_count,
        "{what} references missing block {reference}"
    );
    Ok(())
}

/// nif.xml: NiControllerManager appends sequence refs and Object Palette after NiTimeController.
fn read_manager(block: &[u8], block_count: usize) -> Result<ManagerSpec> {
    let mut cursor = BlockCursor::new(block, "NiControllerManager");
    check_reference(cursor.u32()?, block_count, "next controller")?;
    cursor.skip(18)?; // flags, frequency, phase, start and stop times
    check_reference(cursor.u32()?, block_count, "manager target")?;
    ensure!(cursor.take(1)?[0] <= 1, "invalid manager cumulative flag");
    let count = cursor.u32()? as usize;
    ensure!(
        count <= cursor.remaining().saturating_sub(4) / 4,
        "NiControllerManager declares {count} sequences, more than the block holds"
    );
    let mut sequences = Vec::with_capacity(count);
    for _ in 0..count {
        // A missing list entry must not discard independently readable siblings.
        // read_sequences diagnoses each reference before dereferencing it.
        sequences.push(cursor.u32()? as usize);
    }
    let palette = cursor.u32()?;
    check_reference(palette, block_count, "manager object palette")?;
    cursor.finish()?;
    Ok(ManagerSpec {
        sequences,
        object_palette: (palette != u32::MAX).then_some(palette as usize),
    })
}

/// nif.xml: NiDefaultAVObjectPalette stores Scene, Num Objs, then AVObject records.
/// Each AVObject contains a SizedString name and a NiAVObject pointer, not a string-table index.
fn read_object_palette(block: &[u8], block_count: usize) -> Result<BTreeMap<String, Option<u32>>> {
    let mut cursor = BlockCursor::new(block, "NiDefaultAVObjectPalette");
    check_reference(cursor.u32()?, block_count, "palette scene")?;
    let count = cursor.u32()? as usize;
    ensure!(
        count <= cursor.remaining() / 8,
        "NiDefaultAVObjectPalette declares {count} objects, more than the block holds"
    );
    let mut objects = BTreeMap::new();
    for _ in 0..count {
        let length = cursor.u32()? as usize;
        let name = std::str::from_utf8(cursor.take(length)?)
            .wrap_err("AVObject name is not UTF-8")?
            .to_owned();
        let reference = cursor.u32()?;
        check_reference(reference, block_count, "palette AVObject")?;
        objects
            .entry(name)
            .and_modify(|existing| {
                if *existing != Some(reference) {
                    *existing = None;
                }
            })
            .or_insert(Some(reference));
    }
    cursor.finish()?;
    Ok(objects)
}

/// Decodes a `NiTransformData` through the `NiTransformInterpolator` that references it.
///
/// `Ok(None)` says the block controls something other than a node's transform - a `NiVisController`
/// through a `NiBoolInterpolator`, a particle emitter's rate through a `NiFloatInterpolator`, or an
/// interpolator with no data at all. Those are not transform tracks and are not a problem.
fn transform_keys_of(
    blocks: &[&[u8]],
    header: &NifHeader,
    interpolator: usize,
) -> Result<Option<TransformKeys>> {
    let Some(block) = blocks.get(interpolator) else {
        return Err(color_eyre::eyre::eyre!(
            "controlled block references interpolator {interpolator}, which does not exist"
        ));
    };
    if type_of(header, interpolator) != "NiTransformInterpolator" {
        return Ok(None);
    }
    ensure!(
        block.len() >= 36,
        "NiTransformInterpolator block is {} bytes, shorter than its 36 byte layout",
        block.len()
    );
    let data_index = u32::from_le_bytes([block[32], block[33], block[34], block[35]]) as usize;
    if data_index == u32::MAX as usize {
        return Ok(None);
    }
    let Some(data) = blocks.get(data_index) else {
        return Err(color_eyre::eyre::eyre!(
            "transform interpolator references transform data {data_index}, which does not exist"
        ));
    };
    ensure!(
        type_of(header, data_index) == "NiTransformData",
        "transform interpolator references block {data_index} of type {}, not transform data",
        type_of(header, data_index)
    );
    decode_transform_data(data)
        .map(Some)
        .wrap_err_with(|| format!("NiTransformData block {data_index}"))
}

/// Every animation clip a NIF declares, in the order its sequences are listed.
///
/// The second return value holds one warning per sequence or node track that could not be read.
/// A model with no animation returns nothing, and nothing here is ever fatal to a conversion.
fn clips_from_blocks(block_bytes: &[u8], header: &NifHeader) -> Result<(Vec<Clip>, Vec<String>)> {
    let blocks = blocks_of(block_bytes, header)?;
    let (sequences, mut warnings) = read_sequences(&blocks, header);
    let mut clips = Vec::new();
    let mut rotation_budget = ROTATION_SAMPLE_BUDGET;
    for sequence in sequences {
        let mut tracks: Vec<ClipTrack> = Vec::new();
        let mut has_transform_block = false;
        for target in &sequence.targets {
            let node = &target.node;
            let keys = match transform_keys_of(&blocks, header, target.interpolator) {
                Ok(Some(keys)) => keys,
                // A block that controls something other than a node's transform - a visibility
                // flag, a particle emitter's rate - is not a transform track to drop.
                Ok(None) => continue,
                Err(error) => {
                    has_transform_block = true;
                    // `{:#}` keeps the cause on one line: "NiTransformData block 6: <reason>".
                    warnings.push(format!("sequence '{}': {error:#}", sequence.name));
                    continue;
                }
            };
            has_transform_block = true;
            let mut track = match build_track(node, &keys, sequence.stop_time, &mut rotation_budget)
            {
                Ok(track) => track,
                Err(error) => {
                    warnings.push(format!(
                        "sequence '{}', node '{node}': {error:#}",
                        sequence.name
                    ));
                    continue;
                }
            };
            track.target = target.target;
            if track.is_empty() {
                continue;
            }
            match tracks.iter_mut().find(|existing| &existing.node == node) {
                // A node controlled twice: the later block wins, which is what the game's
                // controller priority would do for the only case the install shows.
                Some(existing) => *existing = track,
                None => tracks.push(track),
            }
        }
        if tracks.is_empty() {
            if has_transform_block {
                warnings.push(format!(
                    "sequence '{}' has no readable transform track",
                    sequence.name
                ));
            }
            continue;
        }
        let duration = sequence.stop_time.max(track_end(&tracks)).max(0.0);
        if duration <= 0.0 {
            continue;
        }
        clips.push(Clip {
            name: sequence.name,
            tracks,
        });
    }
    Ok((clips, warnings))
}

fn track_end(tracks: &[ClipTrack]) -> f32 {
    let mut end: f32 = 0.0;
    for track in tracks {
        if let Some(channel) = &track.rotation {
            end = end.max(channel.times.last().copied().unwrap_or(0.0));
        }
        if let Some(channel) = &track.translation {
            end = end.max(channel.times.last().copied().unwrap_or(0.0));
        }
        if let Some(channel) = &track.scale {
            end = end.max(channel.times.last().copied().unwrap_or(0.0));
        }
    }
    end
}

/// Samples one `NiTransformData` into the channels of a clip track.
fn build_track(
    node: &str,
    keys: &TransformKeys,
    duration: f32,
    rotation_budget: &mut usize,
) -> Result<ClipTrack> {
    let duration = duration.max(keys_end(keys));
    Ok(ClipTrack {
        node: node.to_owned(),
        target: TrackTarget::Name,
        rotation: sample_rotation(&keys.rotation, duration, rotation_budget)?,
        translation: sample_translation(&keys.translation, duration),
        scale: sample_scale(&keys.scale, duration),
    })
}

/// The time of a block's last key, whichever curve carries it.
fn keys_end(keys: &TransformKeys) -> f32 {
    let mut end: f32 = 0.0;
    match &keys.rotation {
        RotationKeys::Quaternion(curve) => {
            for (time, _) in &curve.keys {
                end = end.max(*time);
            }
        }
        RotationKeys::Euler { x, y, z } => {
            for curve in [x, y, z] {
                for (time, _) in &curve.keys {
                    end = end.max(*time);
                }
            }
        }
    }
    for (time, _) in &keys.translation.keys {
        end = end.max(*time);
    }
    for (time, _) in &keys.scale.keys {
        end = end.max(*time);
    }
    end
}

/// The rotation of a clip at `time`: an euler triple converted, or the quaternion key itself.
fn rotation_value_at(rotation: &RotationKeys, time: f32) -> [f32; 4] {
    match rotation {
        RotationKeys::Quaternion(curve) => curve.value_at(time),
        RotationKeys::Euler { x, y, z } => {
            euler_xyz_to_quaternion([x.value_at(time), y.value_at(time), z.value_at(time)])
        }
    }
}

/// Bakes a rotation to quaternion samples at 30 Hz plus every authored key time.
fn sample_rotation(
    rotation: &RotationKeys,
    duration: f32,
    budget: &mut usize,
) -> Result<Option<SampledChannel<[f32; 4]>>> {
    let key_count = match rotation {
        RotationKeys::Quaternion(curve) => curve.keys.len(),
        RotationKeys::Euler { x, y, z } => x
            .keys
            .len()
            .saturating_add(y.keys.len())
            .saturating_add(z.keys.len()),
    };
    if key_count == 0 {
        // No rotation keys: the node keeps its rest rotation, and a channel of identity poses
        // would only fight the rest pose the scene already carries.
        return Ok(None);
    }
    let step = match rotation {
        RotationKeys::Quaternion(curve) => curve.key_type.is_step(),
        RotationKeys::Euler { x, y, z } => [x, y, z]
            .into_iter()
            .all(|curve| curve.keys.len() <= 1 || curve.key_type.is_step()),
    };
    // Check the floating-point count before any integer cast, allocation or sampling.
    // STEP uses only authored times and endpoints, regardless of sequence duration.
    let grid_count = if step {
        0.0
    } else {
        (f64::from(duration.max(0.0)) * f64::from(ROTATION_SAMPLES_PER_SECOND)).ceil()
    };
    let slots = grid_count + key_count as f64 + 2.0;
    ensure!(
        slots.is_finite() && slots <= *budget as f64,
        "rotation sample budget exceeded (limit {ROTATION_SAMPLE_BUDGET} candidate slots per NIF; {budget} remaining)"
    );
    let grid_count = grid_count as usize;
    *budget -= slots as usize;
    let key_times: Vec<f32> = match rotation {
        RotationKeys::Quaternion(curve) => curve.keys.iter().map(|key| key.0).collect(),
        RotationKeys::Euler { x, y, z } => x.times().chain(y.times()).chain(z.times()).collect(),
    };
    let times = if step {
        let mut times = key_times;
        times.retain(|time| *time >= 0.0);
        times.sort_by(f32::total_cmp);
        times.dedup();
        if times.first().is_none_or(|time| *time > 0.0) {
            times.insert(0, 0.0);
        }
        // STEP still needs the sequence's trailing hold to retain its playback duration.
        if duration.is_finite() && duration > times.last().copied().unwrap_or(0.0) {
            times.push(duration);
        }
        times
    } else {
        sample_times(key_times, duration, grid_count)
    };
    if times.is_empty() {
        return Ok(None);
    }
    let values = times
        .iter()
        .map(|time| rotation_value_at(rotation, *time))
        .collect();
    Ok(Some(SampledChannel {
        times,
        values,
        interpolation: if step { "STEP" } else { "LINEAR" },
    }))
}

/// Hermite segment; tangents are derivatives per second, not normalized deltas.
fn hermite(left: f32, right: f32, outgoing: f32, incoming: f32, span: f32, u: f32) -> f32 {
    let u2 = u * u;
    let u3 = u2 * u;
    (2.0 * u3 - 3.0 * u2 + 1.0) * left
        + (-2.0 * u3 + 3.0 * u2) * right
        + (u3 - 2.0 * u2 + u) * span * outgoing
        + (u3 - u2) * span * incoming
}

fn hermite_derivative(
    left: f32,
    right: f32,
    outgoing: f32,
    incoming: f32,
    span: f32,
    u: f32,
) -> f32 {
    let u2 = u * u;
    ((6.0 * u2 - 6.0 * u) * left + (-6.0 * u2 + 6.0 * u) * right) / span
        + (3.0 * u2 - 4.0 * u + 1.0) * outgoing
        + (3.0 * u2 - 2.0 * u) * incoming
}

/// Publish numeric curves directly: STEP and LINEAR need one value per time;
/// CUBICSPLINE needs incoming derivative, value, outgoing derivative per time.
fn sample_numeric<const N: usize>(
    key_type: KeyType,
    keys: &[(f32, [f32; N])],
    tangents: &[Tangents<N>],
    duration: f32,
    value_at: impl Fn(f32) -> [f32; N],
) -> Option<SampledChannel<[f32; N]>> {
    if keys.is_empty() {
        return None;
    }
    // Keep every distinct authored time, including jumps closer than the rotation bake epsilon.
    let mut times: Vec<f32> = keys
        .iter()
        .map(|key| key.0)
        .filter(|time| *time >= 0.0)
        .collect();
    times.dedup();
    if times.first().is_none_or(|time| *time > 0.0) {
        times.insert(0, 0.0);
    }
    // A singleton cubic curve is a held pose, including its sequence endpoint.
    let cubic = key_type.is_cubic() && times.len() > 1;
    let trailing_hold = duration.is_finite() && duration > times.last().copied().unwrap_or(0.0);
    if trailing_hold {
        times.push(duration);
    }
    let mut values = Vec::with_capacity(times.len() * if cubic { 3 } else { 1 });
    for time in &times {
        let right = keys.partition_point(|key| key.0 <= *time);
        let mut incoming = [0.0; N];
        let mut outgoing = [0.0; N];
        if cubic && right > 0 {
            let left = right - 1;
            if keys[left].0 == *time {
                (incoming, outgoing) = tangents[left];
            } else if right < keys.len() {
                let span = keys[right].0 - keys[left].0;
                let u = (*time - keys[left].0) / span;
                for axis in 0..N {
                    let a = keys[left].1[axis];
                    let b = keys[right].1[axis];
                    let out = tangents[left].1[axis];
                    let input = tangents[right].0[axis];
                    incoming[axis] = hermite_derivative(a, b, out, input, span, u);
                    outgoing[axis] = incoming[axis];
                }
            }
        }
        if trailing_hold && *time >= keys[keys.len() - 1].0 {
            // Preserve the incoming derivative of the last authored segment, but
            // make the new trailing segment constant rather than a tangent overshoot.
            outgoing = [0.0; N];
        }
        if cubic {
            values.push(incoming);
        }
        values.push(value_at(*time));
        if cubic {
            values.push(outgoing);
        }
    }
    Some(SampledChannel {
        times,
        values,
        interpolation: if cubic {
            "CUBICSPLINE"
        } else if key_type.is_step() || key_type.is_cubic() {
            "STEP"
        } else {
            "LINEAR"
        },
    })
}

fn sample_translation(curve: &Vec3Curve, duration: f32) -> Option<SampledChannel<[f32; 3]>> {
    sample_numeric(
        curve.key_type,
        &curve.keys,
        &curve.tangents,
        duration,
        |time| curve.value_at(time),
    )
}

fn sample_scale(curve: &ScalarCurve, duration: f32) -> Option<SampledChannel<f32>> {
    let keys: Vec<_> = curve
        .keys
        .iter()
        .map(|(time, value)| (*time, [*value]))
        .collect();
    let tangents: Vec<_> = curve
        .tangents
        .iter()
        .map(|(incoming, outgoing)| ([*incoming], [*outgoing]))
        .collect();
    sample_numeric(curve.key_type, &keys, &tangents, duration, |time| {
        [curve.value_at(time)]
    })
    .map(|channel| SampledChannel {
        times: channel.times,
        values: channel.values.into_iter().map(|[value]| value).collect(),
        interpolation: channel.interpolation,
    })
}

/// Rotation sample times on a previously budget-checked integer-indexed 30 Hz grid.
fn sample_times(key_times: Vec<f32>, duration: f32, grid_count: usize) -> Vec<f32> {
    let mut times: Vec<f32> = key_times
        .into_iter()
        .filter(|time| time.is_finite() && *time >= 0.0 && *time <= duration.max(0.0) + 1.0e-6)
        .collect();
    if duration.is_finite() && duration > 0.0 {
        for index in 0..grid_count {
            let time = (index as f64 / f64::from(ROTATION_SAMPLES_PER_SECOND)) as f32;
            if time < duration - 1.0e-6 {
                times.push(time);
            }
        }
        times.push(duration);
    }
    times.retain(|time| time.is_finite());
    times.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    times.dedup_by(|left, right| (*left - *right).abs() <= 1.0e-6);
    times
}

// ---------------------------------------------------------------------------------------------
// glTF output
// ---------------------------------------------------------------------------------------------

/// Reads a NIF's animation clips from disk.
fn clips_from_nif(path: &Path) -> Result<(Vec<Clip>, Vec<String>)> {
    let bytes = fs::read(path).wrap_err("failed to read animation source")?;
    let (block_bytes, header) =
        crate::mesh::parse_skyrim_header(&bytes, path).wrap_err("failed to read the NIF header")?;
    clips_from_blocks(block_bytes, &header).wrap_err("failed to decode animation source")
}

/// Appends a NIF's clips to a converted GLB.
///
/// Manager palettes bind tracks through the exporter's exact source-block map. A missing
/// palette entry retains unique-name lookup; a resolved pointer never guesses another node.
/// Returns the rewritten GLB and non-gating notices; models without clips remain unchanged.
pub(crate) fn append_nif_animations(
    glb: &[u8],
    path: &Path,
    source_nodes: &BTreeMap<u32, usize>,
) -> Result<(Vec<u8>, Vec<String>)> {
    let (clips, mut warnings) = match clips_from_nif(path) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Ok((
                glb.to_vec(),
                vec![format!("transform animation decode failed: {error:#}")],
            ));
        }
    };
    if clips.is_empty() {
        return Ok((glb.to_vec(), warnings));
    }
    let (glb, more) = append_animations(glb, &clips, source_nodes)?;
    warnings.extend(more);
    Ok((glb, warnings))
}

/// Appends animation clips to a GLB's JSON and binary chunk.
fn append_animations(
    glb: &[u8],
    clips: &[Clip],
    source_nodes: &BTreeMap<u32, usize>,
) -> Result<(Vec<u8>, Vec<String>)> {
    let (mut document, mut bin) = split_glb(glb)?;
    // The new accessors point into buffer 0, which the animation data below extends; a model with
    // geometry always carries one.
    if !document
        .get("buffers")
        .and_then(Value::as_array)
        .is_some_and(|buffers| !buffers.is_empty())
    {
        return Ok((
            glb.to_vec(),
            vec!["transform animation omitted: the GLB has no buffer for animation data".into()],
        ));
    }
    let node_names = gltf_node_names(&document);
    let mut warnings = Vec::new();
    let mut animations = document
        .get("animations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut accessors = document
        .get("accessors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut buffer_views = document
        .get("bufferViews")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for clip in clips {
        let mut samplers = Vec::new();
        let mut channels = Vec::new();
        for track in &clip.tracks {
            let node = match track.target {
                TrackTarget::SourceBlock(source) => {
                    let Some(&node) = source_nodes.get(&source) else {
                        warnings.push(format!(
                            "clip '{}' animates '{}' at palette source block {source}, which is not a node of the exported scene; track omitted",
                            clip.name, track.node
                        ));
                        continue;
                    };
                    if !document
                        .get("nodes")
                        .and_then(Value::as_array)
                        .is_some_and(|nodes| node < nodes.len())
                    {
                        warnings.push(format!(
                            "clip '{}' palette source block {source} maps to missing exported node {node}; track omitted",
                            clip.name
                        ));
                        continue;
                    }
                    node
                }
                TrackTarget::AmbiguousPalette => {
                    warnings.push(format!(
                        "clip '{}' animates ambiguous palette name '{}'; track omitted",
                        clip.name, track.node
                    ));
                    continue;
                }
                TrackTarget::Name => {
                    let Some(&target) = node_names.get(&track.node) else {
                        warnings.push(format!(
                            "clip '{}' animates '{}', which is not a node of the exported scene",
                            clip.name, track.node
                        ));
                        continue;
                    };
                    let Some(node) = target else {
                        warnings.push(format!(
                            "clip '{}' animates ambiguous node name '{}'; track omitted",
                            clip.name, track.node
                        ));
                        continue;
                    };
                    node
                }
            };
            if let Some(rotation) = &track.rotation {
                let mut values = rotation.values.clone();
                orient_rotation_samples(&mut values, gltf_node_rotation(&document, node));
                let input = push_accessor(
                    &mut accessors,
                    &mut buffer_views,
                    &mut bin,
                    Cast::F32(&rotation.times),
                    AccessorType::Scalar,
                    true,
                )?;
                let output = push_accessor(
                    &mut accessors,
                    &mut buffer_views,
                    &mut bin,
                    Cast::Vec4(&values),
                    AccessorType::Vec4,
                    false,
                )?;
                samplers.push(json!({
                    "input": input,
                    "interpolation": rotation.interpolation,
                    "output": output,
                }));
                channels.push(json!({
                    "sampler": samplers.len() - 1,
                    "target": { "node": node, "path": "rotation" },
                }));
            }
            if let Some(translation) = &track.translation {
                let input = push_accessor(
                    &mut accessors,
                    &mut buffer_views,
                    &mut bin,
                    Cast::F32(&translation.times),
                    AccessorType::Scalar,
                    true,
                )?;
                let output = push_accessor(
                    &mut accessors,
                    &mut buffer_views,
                    &mut bin,
                    Cast::Vec3(&translation.values),
                    AccessorType::Vec3,
                    false,
                )?;
                samplers.push(json!({
                    "input": input,
                    "interpolation": translation.interpolation,
                    "output": output,
                }));
                channels.push(json!({
                    "sampler": samplers.len() - 1,
                    "target": { "node": node, "path": "translation" },
                }));
            }
            if let Some(scale) = &track.scale {
                let input = push_accessor(
                    &mut accessors,
                    &mut buffer_views,
                    &mut bin,
                    Cast::F32(&scale.times),
                    AccessorType::Scalar,
                    true,
                )?;
                // glTF's `scale` channel is a VEC3 per key, while a `NiTransformData` scale key is
                // one uniform float: write each key as (s, s, s). A SCALAR output here is invalid
                // glTF, and a reader that takes the specification at its word rejects the whole
                // file rather than the channel (Bevy 0.19: "Animations without a sampler output
                // are not supported").
                let uniform: Vec<[f32; 3]> = scale
                    .values
                    .iter()
                    .map(|value| [*value, *value, *value])
                    .collect();
                let output = push_accessor(
                    &mut accessors,
                    &mut buffer_views,
                    &mut bin,
                    Cast::Vec3(&uniform),
                    AccessorType::Vec3,
                    false,
                )?;
                samplers.push(json!({
                    "input": input,
                    "interpolation": scale.interpolation,
                    "output": output,
                }));
                channels.push(json!({
                    "sampler": samplers.len() - 1,
                    "target": { "node": node, "path": "scale" },
                }));
            }
        }
        if channels.is_empty() {
            warnings.push(format!("clip '{}' has no usable channels", clip.name));
            continue;
        }
        animations.push(json!({
            "name": clip.name,
            "channels": channels,
            "samplers": samplers,
        }));
    }

    document["accessors"] = Value::Array(accessors);
    document["bufferViews"] = Value::Array(buffer_views);
    document["animations"] = Value::Array(animations);
    if let Some(buffers) = document.get_mut("buffers").and_then(Value::as_array_mut)
        && let Some(buffer) = buffers.first_mut()
        && let Some(object) = buffer.as_object_mut()
    {
        object.insert("byteLength".to_owned(), json!(u64_from_usize(bin.len())?));
    }
    rebuild_glb(&document, &bin).map(|glb| (glb, warnings))
}

/// Writes the samples of a channel into the binary chunk, returning its accessor index.
#[allow(clippy::too_many_arguments)]
fn push_accessor(
    accessors: &mut Vec<Value>,
    buffer_views: &mut Vec<Value>,
    bin: &mut Vec<u8>,
    data: Cast<'_>,
    kind: AccessorType,
    with_bounds: bool,
) -> Result<usize> {
    if !bin.len().is_multiple_of(4) {
        bin.resize(bin.len() + (4 - bin.len() % 4), 0);
    }
    let offset = bin.len();
    let count = data.count();
    ensure!(
        data.is_finite(),
        "animation accessor contains a non-finite value"
    );
    data.write_to(bin);
    let bounds = if with_bounds { data.bounds()? } else { None };
    buffer_views.push(json!({
        "buffer": 0,
        "byteOffset": u64_from_usize(offset)?,
        "byteLength": u64_from_usize(bin.len() - offset)?,
    }));
    let mut accessor = json!({
        "bufferView": buffer_views.len() - 1,
        "componentType": 5126,
        "count": count,
        "type": kind.component_type(),
    });
    if let Some((min, max)) = bounds {
        accessor["min"] = json!(min);
        accessor["max"] = json!(max);
    }
    accessors.push(accessor);
    Ok(accessors.len() - 1)
}

fn u64_from_usize(value: usize) -> Result<u64> {
    u64::try_from(value).wrap_err("GLB buffer exceeds 2^64 bytes")
}

/// Accessor data, borrowed from a sampled channel.
enum Cast<'a> {
    F32(&'a [f32]),
    Vec3(&'a [[f32; 3]]),
    Vec4(&'a [[f32; 4]]),
}

impl Cast<'_> {
    fn is_finite(&self) -> bool {
        match self {
            Cast::F32(values) => values.iter().all(|value| value.is_finite()),
            Cast::Vec3(values) => values.iter().flatten().all(|value| value.is_finite()),
            Cast::Vec4(values) => values.iter().flatten().all(|value| value.is_finite()),
        }
    }

    fn count(&self) -> usize {
        match self {
            Cast::F32(values) => values.len(),
            Cast::Vec3(values) => values.len(),
            Cast::Vec4(values) => values.len(),
        }
    }

    fn write_to(&self, bin: &mut Vec<u8>) {
        match self {
            Cast::F32(values) => {
                for value in *values {
                    bin.extend_from_slice(&value.to_le_bytes());
                }
            }
            Cast::Vec3(values) => {
                for value in *values {
                    for component in value {
                        bin.extend_from_slice(&component.to_le_bytes());
                    }
                }
            }
            Cast::Vec4(values) => {
                for value in *values {
                    for component in value {
                        bin.extend_from_slice(&component.to_le_bytes());
                    }
                }
            }
        }
    }

    fn bounds(&self) -> Result<Option<(Value, Value)>> {
        match self {
            Cast::F32(values) => {
                let mut min = f32::INFINITY;
                let mut max = f32::NEG_INFINITY;
                for value in *values {
                    min = min.min(*value);
                    max = max.max(*value);
                }
                ensure!(
                    min.is_finite() && max.is_finite(),
                    "animation accessor has no finite bounds"
                );
                // glTF requires min/max to be component arrays, not bare numbers.
                Ok(Some((json!([min]), json!([max]))))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccessorType {
    Scalar,
    Vec3,
    Vec4,
}

impl AccessorType {
    fn component_type(self) -> &'static str {
        match self {
            AccessorType::Scalar => "SCALAR",
            AccessorType::Vec3 => "VEC3",
            AccessorType::Vec4 => "VEC4",
        }
    }
}

fn gltf_node_names(document: &Value) -> HashMap<String, Option<usize>> {
    let mut names = HashMap::new();
    if let Some(nodes) = document.get("nodes").and_then(Value::as_array) {
        for (index, node) in nodes.iter().enumerate() {
            if let Some(name) = node.get("name").and_then(Value::as_str) {
                names
                    .entry(name.to_owned())
                    .and_modify(|target| *target = None)
                    .or_insert(Some(index));
            }
        }
    }
    names
}

fn gltf_node_rotation(document: &Value, node: usize) -> Option<[f32; 4]> {
    let rotation = document
        .get("nodes")?
        .get(node)?
        .get("rotation")?
        .as_array()?;
    let mut values = [0.0f32, 0.0, 0.0, 1.0];
    if rotation.len() != 4 {
        return None;
    }
    for (index, component) in rotation.iter().enumerate() {
        values[index] = component.as_f64()? as f32;
    }
    Some(values)
}

/// Puts every sample on the same hemisphere as the previous one, and the first one on the same
/// hemisphere as the node's rest rotation: `q` and `-q` are the same rotation, and a channel that
/// flips sign between two frames would interpolate the long way round.
fn orient_rotation_samples(values: &mut [[f32; 4]], rest: Option<[f32; 4]>) {
    if let (Some(rest), Some(first)) = (rest, values.first_mut())
        && dot4(*first, rest) < 0.0
    {
        for value in values.iter_mut() {
            *value = value.map(|component| -component);
        }
    }
    for index in 1..values.len() {
        let previous = values[index - 1];
        if dot4(previous, values[index]) < 0.0 {
            values[index] = values[index].map(|component| -component);
        }
    }
}

/// Unit tests for the door-clip decoder, plus one opt-in test that walks a real
/// converted asset tree.
///
/// That test reads the NIFs extracted under `MUDCRAB_CONVERTED_DIR`'s
/// `vfs/meshes`; it is `#[ignore]`d and skips - printing why - when the variable
/// is unset or the tree is not there, so CI never needs proprietary data
/// (ADR-0002).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_order_quaternion_bytes_become_gltf_xyzw() {
        let half = std::f32::consts::FRAC_1_SQRT_2;
        for (source, expected) in [
            ([1.0_f32, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]),
            ([half, 0.0, half, 0.0], [0.0, half, 0.0, half]),
        ] {
            let bytes: Vec<u8> = source.into_iter().flat_map(f32::to_le_bytes).collect();
            let mut cursor = BlockCursor::new(&bytes, "Quaternion");
            assert_eq!(cursor.quaternion().unwrap(), expected);
            cursor.finish().unwrap();
        }
    }

    #[test]
    fn rotation_budget_checks_before_sampling_and_does_not_charge_rejected_tracks() {
        let bytes = DataWriter::rotation_only(&[(0.0, 0.0)], &[(0.0, 0.0), (1.0, 1.0)]);
        let keys = decode_transform_data(&bytes).unwrap();
        let required = 30 + 4 + 2; // Grid, authored axis times, and endpoints.
        let mut budget = required - 1;
        assert!(sample_rotation(&keys.rotation, 1.0, &mut budget).is_err());
        assert_eq!(budget, required - 1);
        budget = required;
        let channel = sample_rotation(&keys.rotation, 1.0, &mut budget)
            .unwrap()
            .unwrap();
        assert_eq!(budget, 0);
        assert_eq!(channel.times.first(), Some(&0.0));
        assert_eq!(channel.times.last(), Some(&1.0));
        assert_eq!(channel.times.len(), 31);
        assert!(channel.times.windows(2).all(|pair| pair[0] < pair[1]));

        // Sparse constant rotations retain huge finite holds without generating a grid.
        let mut writer = DataWriter::default();
        writer.u32(1).u32(5).linear_quat(0.0, [0.0, 0.0, 0.0, 1.0]);
        writer.u32(0).u32(0);
        let keys = decode_transform_data(&writer.finish()).unwrap();
        let mut budget = 3;
        let channel = sample_rotation(&keys.rotation, f32::MAX, &mut budget)
            .unwrap()
            .unwrap();
        assert_eq!(budget, 0);
        assert_eq!(channel.interpolation, "STEP");
        assert_eq!(channel.times, [0.0, f32::MAX]);
        assert_eq!(channel.values, [[0.0, 0.0, 0.0, 1.0]; 2]);
    }

    #[test]
    fn numeric_channels_keep_sequence_duration_and_a_true_trailing_hold() {
        for kind in [1, 5, 2, 3] {
            let mut writer = DataWriter::default();
            writer.u32(0).u32(2).u32(kind);
            for (time, value) in [(0.0, [0.0, 2.0, 0.0]), (1.0, [10.0, 2.0, -1.0])] {
                match kind {
                    2 => {
                        writer.quadratic_vec3(time, value, [0.0; 3], [8.0; 3]);
                    }
                    3 => {
                        writer.linear_vec3(time, value).f32(0.0).f32(0.0).f32(0.0);
                    }
                    _ => {
                        writer.linear_vec3(time, value);
                    }
                }
            }
            writer.u32(2).u32(kind);
            for (time, value) in [(0.0, 1.0), (1.0, 2.0)] {
                match kind {
                    2 => {
                        writer.quadratic_scalar(time, value, 0.0, 8.0);
                    }
                    3 => {
                        writer.tbc_scalar(time, value, [0.0; 3]);
                    }
                    _ => {
                        writer.linear_scalar(time, value);
                    }
                }
            }
            let (_, glb) = publish_data(&writer); // Sequence stop is 4; the last authored key is 1.
            let (document, _) = split_glb(&glb).unwrap();
            let animation = &document["animations"][0];
            for channel in animation["channels"].as_array().unwrap() {
                let sampler = &animation["samplers"][channel["sampler"].as_u64().unwrap() as usize];
                let input = &document["accessors"][sampler["input"].as_u64().unwrap() as usize];
                assert_eq!(
                    input["max"],
                    json!([4.0]),
                    "kind {kind}: sequence trailing hold was lost"
                );
                assert_eq!(
                    input["count"], 3,
                    "two authored keys plus the sequence endpoint"
                );
            }
            for time in [2.0, 3.0, 4.0] {
                assert_published(&glb, "translation", time, [10.0, 2.0, -1.0]);
                assert_published(&glb, "scale", time, [2.0; 3]);
            }
        }
    }

    /// Builds a `NiTransformData` block byte by byte.
    #[derive(Default)]
    struct DataWriter {
        bytes: Vec<u8>,
    }

    impl DataWriter {
        fn u32(&mut self, value: u32) -> &mut Self {
            self.bytes.extend_from_slice(&value.to_le_bytes());
            self
        }

        fn f32(&mut self, value: f32) -> &mut Self {
            self.bytes.extend_from_slice(&value.to_le_bytes());
            self
        }

        fn linear_scalar(&mut self, time: f32, value: f32) -> &mut Self {
            self.f32(time).f32(value)
        }

        fn quadratic_scalar(
            &mut self,
            time: f32,
            value: f32,
            forward: f32,
            backward: f32,
        ) -> &mut Self {
            self.f32(time).f32(value).f32(forward).f32(backward)
        }

        fn tbc_scalar(&mut self, time: f32, value: f32, tbc: [f32; 3]) -> &mut Self {
            self.f32(time)
                .f32(value)
                .f32(tbc[0])
                .f32(tbc[1])
                .f32(tbc[2])
        }

        fn linear_vec3(&mut self, time: f32, value: [f32; 3]) -> &mut Self {
            self.f32(time).f32(value[0]).f32(value[1]).f32(value[2])
        }

        fn quadratic_vec3(
            &mut self,
            time: f32,
            value: [f32; 3],
            forward: [f32; 3],
            backward: [f32; 3],
        ) -> &mut Self {
            self.linear_vec3(time, value);
            self.f32(forward[0]).f32(forward[1]).f32(forward[2]);
            self.f32(backward[0]).f32(backward[1]).f32(backward[2])
        }

        /// Accept a logical xyzw pose and encode nif.xml Quaternion's wxyz bytes.
        fn linear_quat(&mut self, time: f32, value: [f32; 4]) -> &mut Self {
            self.f32(time)
                .f32(value[3])
                .f32(value[0])
                .f32(value[1])
                .f32(value[2])
        }

        /// A rotation-only door: one XYZ key set, then empty translation and scale groups.
        fn rotation_only(x_keys: &[(f32, f32)], z_keys: &[(f32, f32)]) -> Vec<u8> {
            let mut writer = DataWriter::default();
            writer.u32(1).u32(4);
            writer.u32(x_keys.len() as u32).u32(1);
            for (time, value) in x_keys {
                writer.linear_scalar(*time, *value);
            }
            writer.u32(1).u32(1);
            writer.linear_scalar(0.0, 0.0);
            writer.u32(z_keys.len() as u32).u32(2);
            for (time, value) in z_keys {
                writer.quadratic_scalar(*time, *value, 0.0, 0.0);
            }
            writer.u32(0).u32(0);
            writer.bytes
        }

        fn finish(&self) -> Vec<u8> {
            self.bytes.clone()
        }
    }

    fn close3(left: [f32; 3], right: [f32; 3], tolerance: f32) -> bool {
        (0..3).all(|axis| (left[axis] - right[axis]).abs() <= tolerance)
    }

    fn euler(keys: &TransformKeys) -> ([f32; 3], KeyType) {
        match &keys.rotation {
            RotationKeys::Euler { x, y, z } => {
                ([x.keys[0].1, y.keys[0].1, z.keys[0].1], z.key_type)
            }
            RotationKeys::Quaternion(curve) => panic!("expected euler keys, got {:?}", curve),
        }
    }

    #[test]
    fn decodes_xyz_euler_rotation_with_linear_and_quadratic_axes() {
        // The shape of a dwemer load door: an X axis holding a constant 180 degree turn, a flat Y
        // axis and a quadratic Z curve - the swing itself.
        let bytes = DataWriter::rotation_only(
            &[(0.0, std::f32::consts::PI)],
            &[(0.0, 0.0), (0.6, 0.0934676)],
        );
        let keys = decode_transform_data(&bytes).expect("the fitted layout consumes the block");
        let (values, key_type) = euler(&keys);
        assert!(close3(values, [std::f32::consts::PI, 0.0, 0.0], 1.0e-6));
        assert_eq!(key_type, KeyType::Quadratic, "the Z axis is quadratic");
        assert_eq!(keys.translation.keys.len(), 0);
        assert_eq!(keys.scale.keys.len(), 0);
        let RotationKeys::Euler { z, .. } = &keys.rotation else {
            unreachable!()
        };
        assert_eq!(z.keys[1].0, 0.6);
        assert!((z.keys[1].1 - 0.0934676).abs() < 1.0e-6);
    }

    #[test]
    fn decodes_linear_translation_and_quadratic_scale_keys() {
        let mut writer = DataWriter::default();
        writer.u32(0); // no rotation keys, so no rotation type word either
        writer.u32(2).u32(1); // two linear translation keys
        writer.linear_vec3(0.0, [0.0, 0.0, 0.0]);
        writer.linear_vec3(0.0333, [12.5, 0.0, -3.0]);
        writer.u32(2).u32(2); // two quadratic scale keys
        writer.quadratic_scalar(0.0, 1.0, 0.0, 0.0);
        writer.quadratic_scalar(0.3, 1.5, 0.25, -0.25);
        let keys = decode_transform_data(&writer.finish()).unwrap();
        assert!(matches!(
            keys.rotation,
            RotationKeys::Quaternion(ref curve) if curve.keys.is_empty()
        ));
        assert_eq!(keys.translation.keys.len(), 2);
        assert_eq!(keys.translation.key_type, KeyType::Linear);
        assert!(close3(
            keys.translation.keys[1].1,
            [12.5, 0.0, -3.0],
            1.0e-6
        ));
        assert_eq!(keys.scale.keys.len(), 2);
        assert_eq!(keys.scale.key_type, KeyType::Quadratic);
        assert!((keys.scale.keys[1].1 - 1.5).abs() < 1.0e-6);
        // The incoming 0.25 tangent contributes -0.03125 at the midpoint.
        // Keep the exact assertion, now against the authored curve rather than its linear chord.
        assert_eq!(keys.scale.value_at(0.15), 1.21875);
    }

    #[test]
    fn decodes_a_sliding_door_with_no_rotation_keys_at_all() {
        // `riftenrwthievesguilddoor01.nif`'s own shape: the group with no keys writes no
        // interpolation type, so the block reads `0, 95, 1, <95 keys>, 0` - a door that slides
        // along its local X without ever turning.
        let mut writer = DataWriter::default();
        writer.u32(0); // no rotation keys, and therefore no rotation type word
        writer.u32(2).u32(1); // two linear translation keys
        writer.linear_vec3(0.0, [0.0, 0.0, 0.0]);
        writer.linear_vec3(0.0333, [-0.1433, 0.0378, 0.0]);
        writer.u32(0); // no scale keys
        let keys = decode_transform_data(&writer.finish()).expect("the block consumes exactly");
        assert!(
            matches!(keys.rotation, RotationKeys::Quaternion(ref curve) if curve.keys.is_empty()),
            "no rotation keys: {:?}",
            keys.rotation
        );
        assert_eq!(keys.translation.keys.len(), 2);
        assert!((keys.translation.keys[1].1[0] + 0.1433).abs() < 1.0e-4);
        assert!(keys.scale.keys.is_empty());

        // A block with a rotation *and* a translation keeps both type words.
        let mut both = DataWriter::default();
        both.u32(1).u32(1);
        both.linear_quat(0.0, [0.0, 0.0, 0.0, 1.0]);
        both.u32(1).u32(2);
        both.quadratic_vec3(0.0, [1.0, 2.0, 3.0], [0.0; 3], [0.0; 3]);
        both.u32(0);
        let keys = decode_transform_data(&both.finish()).unwrap();
        let RotationKeys::Quaternion(curve) = &keys.rotation else {
            panic!("expected quaternion keys")
        };
        assert_eq!(curve.keys.len(), 1);
        assert_eq!(keys.translation.keys.len(), 1);
        assert_eq!(keys.translation.key_type, KeyType::Quadratic);
    }

    #[test]
    fn decodes_linear_and_constant_quaternion_rotation_keys() {
        let mut writer = DataWriter::default();
        writer.u32(2).u32(1);
        writer.linear_quat(0.0, [0.0, 0.0, 0.0, 1.0]);
        let quarter = std::f32::consts::FRAC_1_SQRT_2;
        writer.linear_quat(1.0, [0.0, quarter, 0.0, quarter]);
        writer.u32(0).u32(0);
        let keys = decode_transform_data(&writer.finish()).unwrap();
        let RotationKeys::Quaternion(curve) = &keys.rotation else {
            panic!("expected quaternion keys")
        };
        assert_eq!(curve.keys.len(), 2);
        assert_eq!(curve.value_at(0.0), [0.0, 0.0, 0.0, 1.0]);
        assert!(dot4(curve.value_at(1.0), [0.0, quarter, 0.0, quarter]) > 0.999);

        let mut constant = DataWriter::default();
        constant.u32(1).u32(5);
        constant.linear_quat(0.25, [1.0, 0.0, 0.0, 0.0]);
        constant.u32(0).u32(0);
        let keys = decode_transform_data(&constant.finish()).unwrap();
        match keys.rotation {
            RotationKeys::Quaternion(curve) => {
                assert_eq!(curve.key_type, KeyType::Const);
                assert_eq!(curve.value_at(0.0), [1.0, 0.0, 0.0, 0.0]);
                assert_eq!(curve.value_at(9.0), [1.0, 0.0, 0.0, 0.0]);
            }
            other => panic!("expected quaternion keys, got {other:?}"),
        }
    }

    #[test]
    fn decodes_tbc_keys_with_their_tangents() {
        let mut writer = DataWriter::default();
        writer.u32(0); // no rotation keys
        writer.u32(1).u32(3); // one TBC translation key: time, vec3, tension/bias/continuity
        writer.f32(0.0);
        writer.f32(1.0).f32(2.0).f32(3.0);
        writer.f32(0.25).f32(0.5).f32(0.75);
        writer.u32(1).u32(3);
        writer.tbc_scalar(0.0, 2.0, [0.1, 0.2, 0.3]);
        let keys = decode_transform_data(&writer.finish()).unwrap();
        assert_eq!(keys.translation.key_type, KeyType::Tbc);
        assert!(close3(keys.translation.keys[0].1, [1.0, 2.0, 3.0], 1.0e-6));
        assert_eq!(keys.scale.key_type, KeyType::Tbc);
        assert_eq!(keys.scale.keys[0].1, 2.0);
    }

    #[test]
    fn rejects_a_block_that_leaves_bytes_unread() {
        let mut bytes = DataWriter::rotation_only(&[(0.0, 0.0)], &[(0.0, 0.0)]);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let error = decode_transform_data(&bytes).expect_err("trailing bytes are an error");
        assert!(
            error.to_string().contains("unread trailing bytes"),
            "{error}"
        );
    }

    #[test]
    fn rejects_an_unknown_key_type() {
        let mut writer = DataWriter::default();
        writer.u32(1).u32(9);
        let error = decode_transform_data(&writer.finish()).expect_err("key type 9 does not exist");
        assert!(
            error.to_string().contains("unknown NIF key type 9"),
            "{error}"
        );
    }

    #[test]
    fn rejects_truncated_key_data() {
        let bytes = DataWriter::rotation_only(&[(0.0, 0.0)], &[(0.0, 0.0)]);
        let error = decode_transform_data(&bytes[..bytes.len() - 3])
            .expect_err("a truncated block is an error");
        assert!(error.to_string().contains("bytes early"), "{error}");

        let mut writer = DataWriter::default();
        writer.u32(0); // no rotation keys
        writer.u32(4).u32(1); // four linear keys declared, none stored
        let error = decode_transform_data(&writer.finish()).expect_err("missing keys are an error");
        assert!(
            error.to_string().contains("more than the block holds"),
            "{error}"
        );
    }

    #[test]
    fn euler_xyz_composes_as_z_then_y_then_x() {
        // A single axis is a half-angle turn about that axis.
        let z = euler_xyz_to_quaternion([0.0, 0.0, -0.15]);
        assert!((z[2] - (-0.15f32 * 0.5).sin()).abs() < 1.0e-6);
        assert!((z[3] - (-0.15f32 * 0.5).cos()).abs() < 1.0e-6);
        let x = euler_xyz_to_quaternion([std::f32::consts::PI, 0.0, 0.0]);
        assert!(close3([x[0], x[1], x[2]], [1.0, 0.0, 0.0], 1.0e-6));

        // A mixed triple is Rz * Ry * Rx applied to a vector.
        let source = [0.3f32, -0.7, 1.1];
        let expected = rotate_xyz(source);
        let actual = rotate_quaternion(euler_xyz_to_quaternion(source), [1.0, 0.0, 0.0]);
        assert!(
            close3(expected, actual, 1.0e-5),
            "{expected:?} != {actual:?}"
        );
    }

    /// `Rz(z) * Ry(y) * Rx(x)` applied to the X axis, built from matrices so the expectation does
    /// not reuse the implementation's quaternion helpers.
    fn rotate_xyz([x, y, z]: [f32; 3]) -> [f32; 3] {
        let (sx, cx) = x.sin_cos();
        let (sy, cy) = y.sin_cos();
        let (sz, cz) = z.sin_cos();
        let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
        let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
        let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
        let multiply = |left: [[f32; 3]; 3], right: [[f32; 3]; 3]| {
            let mut product = [[0.0f32; 3]; 3];
            for row in 0..3 {
                for column in 0..3 {
                    product[row][column] = (0..3)
                        .map(|inner| left[row][inner] * right[inner][column])
                        .sum();
                }
            }
            product
        };
        let matrix = multiply(rz, multiply(ry, rx));
        [matrix[0][0], matrix[1][0], matrix[2][0]]
    }

    fn rotate_quaternion(quaternion: [f32; 4], vector: [f32; 3]) -> [f32; 3] {
        let [x, y, z, w] = quaternion;
        let rotate = |v: [f32; 3]| {
            let uv = [
                y * v[2] - z * v[1],
                z * v[0] - x * v[2],
                x * v[1] - y * v[0],
            ];
            let uuv = [
                y * uv[2] - z * uv[1],
                z * uv[0] - x * uv[2],
                x * uv[1] - y * uv[0],
            ];
            [
                v[0] + 2.0 * (w * uv[0] + uuv[0]),
                v[1] + 2.0 * (w * uv[1] + uuv[1]),
                v[2] + 2.0 * (w * uv[2] + uuv[2]),
            ]
        };
        rotate(vector)
    }

    #[test]
    fn rotation_channel_bakes_at_thirty_hertz_and_keeps_key_times() {
        let bytes = DataWriter::rotation_only(&[(0.0, 0.0)], &[(0.0, 0.0), (0.6, -0.15)]);
        let keys = decode_transform_data(&bytes).unwrap();
        let mut budget = ROTATION_SAMPLE_BUDGET;
        let channel = sample_rotation(&keys.rotation, 0.6, &mut budget)
            .unwrap()
            .unwrap();
        assert_eq!(channel.values.len(), channel.times.len());
        assert_eq!(channel.times[0], 0.0);
        assert!((channel.times[channel.times.len() - 1] - 0.6).abs() < 1.0e-6);
        assert!(
            channel
                .times
                .iter()
                .any(|time| (*time - 0.3).abs() < 1.0e-6),
            "the 30 Hz grid covers the clip: {:?}",
            channel.times
        );
        assert!(
            channel
                .times
                .iter()
                .any(|time| (*time - 0.6).abs() < 1.0e-6),
            "the authored key times survive: {:?}",
            channel.times
        );
        // t = 0 is the rest pose, and the end of the clip is the authored key.
        assert!(dot4(channel.values[0], [0.0, 0.0, 0.0, 1.0]) > 0.999);
        let last = channel.values[channel.values.len() - 1];
        assert!((last[2] - (-0.075f32).sin()).abs() < 1.0e-4, "{last:?}");
    }

    #[test]
    fn translation_channel_starts_at_zero_when_its_first_key_is_later() {
        let curve = Vec3Curve {
            key_type: KeyType::Linear,
            tangents: Vec::new(),
            keys: vec![(0.5, [1.0, 2.0, 3.0]), (1.0, [4.0, 5.0, 6.0])],
        };
        let channel = sample_translation(&curve, 1.0).unwrap();
        assert_eq!(channel.times, vec![0.0, 0.5, 1.0]);
        assert!(close3(channel.values[0], [1.0, 2.0, 3.0], 1.0e-6));
        assert!(close3(channel.values[2], [4.0, 5.0, 6.0], 1.0e-6));
    }

    /// A minimal GLB with the two nodes a door model exports.
    pub(super) fn tiny_glb() -> Vec<u8> {
        // The root carries the Z-up to Y-up basis change, as a converted model's does.
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let document = json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [
                { "name": "DweSmallDoorLoad01", "children": [1], "rotation": [-half, 0.0, 0.0, half] },
                { "name": "Object02", "rotation": [1.0, 0.0, 0.0, 0.0] },
            ],
            "buffers": [{ "byteLength": 0 }],
        });
        let mut json = serde_json::to_vec(&document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total = 20 + json.len();
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb
    }

    pub(super) fn glb_json(glb: &[u8]) -> Value {
        let (document, _) = split_glb(glb).unwrap();
        document
    }

    fn glb_bin(glb: &[u8]) -> Vec<u8> {
        let (_, bin) = split_glb(glb).unwrap();
        bin
    }

    #[test]
    fn appends_a_clip_to_a_glb_and_targets_the_named_node() {
        let clip = Clip {
            name: "Open".to_owned(),
            tracks: vec![ClipTrack {
                node: "Object02".to_owned(),
                target: TrackTarget::Name,
                rotation: Some(SampledChannel {
                    interpolation: "LINEAR",
                    times: vec![0.0, 0.3, 0.6],
                    values: vec![
                        [1.0, 0.0, 0.0, 0.0],
                        [0.9996, 0.0, 0.0, 0.0288],
                        [0.9986, 0.0, 0.0, 0.0523],
                    ],
                }),
                translation: Some(SampledChannel {
                    interpolation: "LINEAR",
                    times: vec![0.0, 0.6],
                    values: vec![[0.0, 0.0, 0.0], [0.0, 0.0, 12.0]],
                }),
                scale: None,
            }],
        };
        let (glb, warnings) = append_animations(&tiny_glb(), &[clip], &BTreeMap::new()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let document = glb_json(&glb);
        assert_eq!(document["animations"][0]["name"], "Open");
        let channels = document["animations"][0]["channels"].as_array().unwrap();
        assert_eq!(channels.len(), 2, "rotation and translation");
        for channel in channels {
            assert_eq!(channel["target"]["node"], 1, "Object02 is node 1");
        }
        assert_eq!(channels[0]["target"]["path"], "rotation");
        assert_eq!(channels[1]["target"]["path"], "translation");
        let samplers = document["animations"][0]["samplers"].as_array().unwrap();
        assert_eq!(samplers[0]["interpolation"], "LINEAR");
        let inputs = samplers[0]["input"].as_u64().unwrap() as usize;
        let accessor = &document["accessors"][inputs];
        assert_eq!(accessor["count"], 3);
        assert_eq!(accessor["componentType"], 5126);
        assert_eq!(accessor["type"], "SCALAR");
        assert!(accessor["min"][0].as_f64().unwrap().abs() < 1e-6);
        assert!((accessor["max"][0].as_f64().unwrap() - 0.6).abs() < 1e-6);

        // The accessor's bytes are inside the binary chunk, and the buffer's length covers them.
        let bin = glb_bin(&glb);
        assert!(document["buffers"][0]["byteLength"].as_u64().unwrap() <= bin.len() as u64);
        let view = document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize]
            .as_object()
            .unwrap();
        let offset = view["byteOffset"].as_u64().unwrap() as usize;
        let times = (0..3)
            .map(|index| {
                let start = offset + index * 4;
                f32::from_le_bytes(bin[start..start + 4].try_into().unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(times, vec![0.0, 0.3, 0.6]);
        // The GLB is still a valid container.
        assert_eq!(
            u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize,
            glb.len()
        );
    }

    /// Every channel's output accessor has to be the type glTF fixes for its target path:
    /// rotation VEC4, translation and scale VEC3, weights SCALAR. A `NiTransformData` scale key is
    /// one uniform float, and writing it as a SCALAR output produced files that a conforming
    /// reader rejects whole ("Animations without a sampler output are not supported"), which took
    /// out every animated model in a converted install.
    #[test]
    fn every_channel_output_matches_the_type_its_path_requires() {
        let clip = Clip {
            name: "Open".to_owned(),
            tracks: vec![ClipTrack {
                node: "Object02".to_owned(),
                target: TrackTarget::Name,
                rotation: Some(SampledChannel {
                    interpolation: "LINEAR",
                    times: vec![0.0, 0.6],
                    values: vec![[0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.3, 0.95]],
                }),
                translation: Some(SampledChannel {
                    interpolation: "LINEAR",
                    times: vec![0.0, 0.6],
                    values: vec![[0.0, 0.0, 0.0], [0.0, 0.0, 12.0]],
                }),
                scale: Some(SampledChannel {
                    interpolation: "LINEAR",
                    times: vec![0.0, 0.6],
                    values: vec![1.0, 2.0],
                }),
            }],
        };
        let (glb, warnings) = append_animations(&tiny_glb(), &[clip], &BTreeMap::new()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let document = glb_json(&glb);
        let animation = &document["animations"][0];
        let samplers = animation["samplers"].as_array().unwrap();
        let mut seen = 0;
        for channel in animation["channels"].as_array().unwrap() {
            let path = channel["target"]["path"].as_str().unwrap();
            let sampler = &samplers[channel["sampler"].as_u64().unwrap() as usize];
            let output = &document["accessors"][sampler["output"].as_u64().unwrap() as usize];
            let expected = match path {
                "rotation" => "VEC4",
                "translation" | "scale" => "VEC3",
                "weights" => "SCALAR",
                other => panic!("unexpected channel path {other}"),
            };
            assert_eq!(output["type"], expected, "{path} output");
            assert_eq!(
                document["accessors"][sampler["input"].as_u64().unwrap() as usize]["count"],
                output["count"],
                "{path}: one output per key time"
            );
            seen += 1;
        }
        assert_eq!(seen, 3, "rotation, translation and scale");
    }

    #[test]
    fn skips_a_track_whose_node_is_missing_and_keeps_the_container_valid() {
        let clip = Clip {
            name: "Open".to_owned(),
            tracks: vec![
                ClipTrack {
                    node: "NotInTheScene".to_owned(),
                    target: TrackTarget::Name,
                    rotation: Some(SampledChannel {
                        interpolation: "LINEAR",
                        times: vec![0.0],
                        values: vec![[0.0, 0.0, 0.0, 1.0]],
                    }),
                    translation: None,
                    scale: None,
                },
                ClipTrack {
                    node: "Object02".to_owned(),
                    target: TrackTarget::Name,
                    rotation: Some(SampledChannel {
                        interpolation: "LINEAR",
                        times: vec![0.0],
                        values: vec![[1.0, 0.0, 0.0, 0.0]],
                    }),
                    translation: None,
                    scale: None,
                },
            ],
        };
        let (glb, warnings) = append_animations(&tiny_glb(), &[clip], &BTreeMap::new()).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("NotInTheScene"), "{warnings:?}");
        let document = glb_json(&glb);
        assert_eq!(
            document["animations"][0]["channels"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize,
            glb.len()
        );
    }

    /// The fitted layout decodes *every* animated door model in the converted tree.
    ///
    /// This is the instrument for the layout itself: the rule that a group writes its
    /// interpolation type only when it has keys is what makes every block in the install consume
    /// exactly, and one model that stops fitting here means the rule is wrong again. Walking the
    /// tree takes about twenty seconds, so it stays ignored unless the assets are being checked.
    #[test]
    #[ignore = "walks the extracted Skyrim NIFs (MUDCRAB_CONVERTED_DIR)"]
    fn real_every_animated_door_model_decodes() {
        let Some(converted) = std::env::var_os("MUDCRAB_CONVERTED_DIR")
            .or_else(|| std::env::var_os("OPENSKYRIM_CONVERTED_DIR"))
            .map(std::path::PathBuf::from)
        else {
            eprintln!("skipping: set MUDCRAB_CONVERTED_DIR to a converted asset tree");
            return;
        };
        let root = converted.join("vfs").join("meshes");
        if !root.is_dir() {
            eprintln!("skipping: {} is not present", root.display());
            return;
        }
        let mut files = 0usize;
        let mut animated = 0usize;
        let mut clips = 0usize;
        let mut tracks = 0usize;
        let mut warnings: Vec<String> = Vec::new();
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("nif") {
                continue;
            }
            if !path.to_string_lossy().to_lowercase().contains("door") {
                continue;
            }
            files += 1;
            let (clips_of_file, file_warnings) = match clips_from_nif(path) {
                Ok(result) => result,
                Err(error) => {
                    warnings.push(format!("{}: {error:#}", path.display()));
                    continue;
                }
            };
            if !clips_of_file.is_empty() {
                animated += 1;
            }
            for warning in file_warnings {
                warnings.push(format!("{}: {warning}", path.display()));
            }
            for clip in &clips_of_file {
                clips += 1;
                tracks += clip.tracks.len();
            }
        }
        eprintln!(
            "{files} door NIFs, {animated} animated, {clips} clips, {tracks} tracks, {} warnings",
            warnings.len()
        );
        for warning in warnings.iter().take(10) {
            eprintln!("  {warning}");
        }
        assert!(
            warnings.is_empty(),
            "{} animated door models no longer decode; the first is {}",
            warnings.len(),
            warnings.first().cloned().unwrap_or_default()
        );
        // The install's door models carry roughly 350 clips and 640 tracks; a floor well below
        // that still catches a systematic regression without pinning the asset tree's contents.
        assert!(
            clips >= 300 && tracks >= 550,
            "only {clips} clips and {tracks} tracks were read from {files} door NIFs"
        );
    }

    #[test]
    fn a_clip_with_no_nodes_in_the_scene_is_dropped_with_a_warning() {
        let clip = Clip {
            name: "Close".to_owned(),
            tracks: vec![ClipTrack {
                node: "Missing".to_owned(),
                target: TrackTarget::Name,
                rotation: Some(SampledChannel {
                    interpolation: "LINEAR",
                    times: vec![0.0],
                    values: vec![[0.0, 0.0, 0.0, 1.0]],
                }),
                translation: None,
                scale: None,
            }],
        };
        let (glb, warnings) = append_animations(&tiny_glb(), &[clip], &BTreeMap::new()).unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        let document = glb_json(&glb);
        assert!(
            document["animations"].as_array().unwrap().is_empty(),
            "no clip survives"
        );
    }
    /// Read the published channel, independently of the converter's curve evaluator.
    /// Cubic values are evaluated in Bezier form to avoid reusing its Hermite formula.
    fn published_value(glb: &[u8], path: &str, time: f32) -> Vec<f32> {
        let (document, bin) = split_glb(glb).unwrap();
        let animation = &document["animations"][0];
        let channel = animation["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|channel| channel["target"]["path"] == path)
            .unwrap();
        let sampler = &animation["samplers"][channel["sampler"].as_u64().unwrap() as usize];
        let read = |index: &Value| {
            let accessor = &document["accessors"][index.as_u64().unwrap() as usize];
            let view = &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
            let width = match accessor["type"].as_str().unwrap() {
                "SCALAR" => 1,
                "VEC3" => 3,
                "VEC4" => 4,
                other => panic!("{other}"),
            };
            let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize
                + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
            let count = accessor["count"].as_u64().unwrap() as usize * width;
            bin[offset..offset + 4 * count]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect::<Vec<_>>()
        };
        let times = read(&sampler["input"]);
        let values = read(&sampler["output"]);
        let width = if path == "rotation" { 4 } else { 3 };
        let cubic = sampler["interpolation"] == "CUBICSPLINE";
        let stride = if cubic { 3 * width } else { width };
        assert_eq!(values.len(), times.len() * stride);
        let pose = |index| {
            values[index * stride + if cubic { width } else { 0 }..]
                .iter()
                .take(width)
                .copied()
                .collect::<Vec<_>>()
        };
        if time <= times[0] {
            return pose(0);
        }
        if time >= *times.last().unwrap() {
            return pose(times.len() - 1);
        }
        let right = times.partition_point(|key_time| *key_time <= time);
        let left = right - 1;
        if sampler["interpolation"] == "STEP" {
            return pose(left);
        }
        let span = times[right] - times[left];
        let u = (time - times[left]) / span;
        let a = pose(left);
        let d = pose(right);
        (0..width)
            .map(|axis| {
                if cubic {
                    let b = a[axis] + span * values[left * stride + 2 * width + axis] / 3.0;
                    let c = d[axis] - span * values[right * stride + axis] / 3.0;
                    (1.0 - u).powi(3) * a[axis]
                        + 3.0 * (1.0 - u).powi(2) * u * b
                        + 3.0 * (1.0 - u) * u * u * c
                        + u.powi(3) * d[axis]
                } else {
                    a[axis] + u * (d[axis] - a[axis])
                }
            })
            .collect()
    }

    fn publish_data(writer: &DataWriter) -> (TransformKeys, Vec<u8>) {
        let keys = decode_transform_data(&writer.finish()).unwrap();
        let mut budget = ROTATION_SAMPLE_BUDGET;
        let clip = Clip {
            name: "Authored".to_owned(),
            tracks: vec![build_track("Object02", &keys, 4.0, &mut budget).unwrap()],
        };
        let (glb, warnings) = append_animations(&tiny_glb(), &[clip], &BTreeMap::new()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        (keys, glb)
    }

    fn assert_published(glb: &[u8], path: &str, time: f32, expected: [f32; 3]) {
        let actual = published_value(glb, path, time);
        assert!(
            close3(actual.try_into().unwrap(), expected, 1.0e-5),
            "{path} at {time}: expected {expected:?}"
        );
    }

    #[test]
    fn const_translation_and_scale_publish_step_at_each_discontinuity() {
        let mut writer = DataWriter::default();
        writer.u32(0).u32(3).u32(5);
        writer.linear_vec3(0.25, [0.0, 2.0, 3.0]);
        writer.linear_vec3(1.0, [10.0, 4.0, 5.0]);
        writer.linear_vec3(2.0, [20.0, 6.0, 7.0]);
        writer.u32(3).u32(5);
        writer
            .linear_scalar(0.25, 1.0)
            .linear_scalar(1.0, 2.0)
            .linear_scalar(2.0, 3.0);
        let (keys, glb) = publish_data(&writer);
        for (time, expected) in [
            (0.0, 0.0),
            (0.5, 0.0),
            (0.9999, 0.0),
            (1.0, 10.0),
            (1.5, 10.0),
            (2.0, 20.0),
        ] {
            assert_eq!(keys.translation.value_at(time)[0], expected);
            let y = if expected == 0.0 {
                2.0
            } else if expected == 10.0 {
                4.0
            } else {
                6.0
            };
            assert_published(&glb, "translation", time, [expected, y, y + 1.0]);
            assert_published(&glb, "scale", time, [expected / 10.0 + 1.0; 3]);
        }
        for sampler in glb_json(&glb)["animations"][0]["samplers"]
            .as_array()
            .unwrap()
        {
            assert_eq!(sampler["interpolation"], "STEP");
        }
    }

    #[test]
    fn quadratic_translation_and_scale_keep_tangent_direction_and_time_units() {
        let mut writer = DataWriter::default();
        writer.u32(0).u32(2).u32(2);
        // NIF stores incoming (Forward) then outgoing (Backward) normalized tangents.
        // X is 8u^3-20u^2+16u; Y and Z are flat. Span is two seconds, not one.
        writer.quadratic_vec3(0.5, [0.0, 2.0, 3.0], [91.0; 3], [16.0, 0.0, 0.0]);
        writer.quadratic_vec3(2.5, [4.0, 2.0, 3.0], [0.0; 3], [-87.0; 3]);
        writer.u32(2).u32(2);
        writer.quadratic_scalar(0.5, 1.0, 91.0, 4.0);
        writer.quadratic_scalar(2.5, 3.0, 0.0, -87.0);
        let (keys, glb) = publish_data(&writer);
        for (time, x, scale) in [
            (0.0, 0.0, 1.0),
            (0.25, 0.0, 1.0),
            (0.5, 0.0, 1.0),
            (1.0, 2.875, 1.875),
            (1.5, 4.0, 2.5),
            (2.0, 4.125, 2.875),
            (2.5, 4.0, 3.0),
        ] {
            assert!((keys.translation.value_at(time)[0] - x).abs() < 1.0e-6);
            assert!((keys.scale.value_at(time) - scale).abs() < 1.0e-6);
            assert_published(&glb, "translation", time, [x, 2.0, 3.0]);
            assert_published(&glb, "scale", time, [scale; 3]);
        }
        for sampler in glb_json(&glb)["animations"][0]["samplers"]
            .as_array()
            .unwrap()
        {
            assert_eq!(sampler["interpolation"], "CUBICSPLINE");
        }
    }

    #[test]
    fn tbc_translation_and_scale_use_neighbors_and_unequal_key_intervals() {
        let mut writer = DataWriter::default();
        writer.u32(0).u32(3).u32(3);
        for (time, x, tbc) in [
            (0.0, 0.0, [0.0; 3]),
            (1.0, 4.0, [0.25, 0.5, -0.5]),
            (4.0, 8.0, [0.0; 3]),
        ] {
            writer.linear_vec3(time, [x, 2.0, 3.0]);
            for value in tbc {
                writer.f32(value);
            }
        }
        writer.u32(3).u32(3);
        writer.tbc_scalar(0.0, 1.0, [0.0; 3]);
        writer.tbc_scalar(1.0, 5.0, [0.25, 0.5, -0.5]);
        writer.tbc_scalar(4.0, 9.0, [0.0; 3]);
        let (keys, glb) = publish_data(&writer);
        // Middle incoming/outgoing normalized tangents: 1.875 and 3.375.
        for (time, x) in [
            (0.25, 1.0996094),
            (0.5, 2.265625),
            (2.5, 5.921875),
            (4.0, 8.0),
        ] {
            assert!((keys.translation.value_at(time)[0] - x).abs() < 1.0e-6);
            assert!((keys.scale.value_at(time) - (x + 1.0)).abs() < 1.0e-6);
            assert_published(&glb, "translation", time, [x, 2.0, 3.0]);
            assert_published(&glb, "scale", time, [x + 1.0; 3]);
        }
    }

    #[test]
    fn quadratic_euler_is_evaluated_before_rotation_baking() {
        let mut writer = DataWriter::default();
        writer.u32(1).u32(4);
        writer.u32(1).u32(1).linear_scalar(0.0, 0.0);
        writer.u32(1).u32(1).linear_scalar(0.0, 0.0);
        writer.u32(2).u32(2);
        writer.quadratic_scalar(0.0, 0.0, 0.0, 2.0);
        writer.quadratic_scalar(2.0, 1.0, 0.0, 0.0);
        writer.u32(0).u32(0);
        let (keys, glb) = publish_data(&writer);
        let expected = euler_xyz_to_quaternion([0.0, 0.0, 0.75]);
        assert!(dot4(rotation_value_at(&keys.rotation, 1.0), expected) > 0.999999);
        let actual: [f32; 4] = published_value(&glb, "rotation", 1.0).try_into().unwrap();
        assert!(dot4(actual, expected).abs() > 0.99999);
    }

    #[test]
    fn nonfinite_curve_tangents_and_parameters_are_rejected() {
        for kind in [2, 3] {
            let mut writer = DataWriter::default();
            writer.u32(0).u32(0).u32(1).u32(kind);
            if kind == 2 {
                writer.quadratic_scalar(0.0, 1.0, f32::NAN, 0.0);
            } else {
                writer.tbc_scalar(0.0, 1.0, [0.0, f32::INFINITY, 0.0]);
            }
            assert!(decode_transform_data(&writer.finish()).is_err());
        }
    }

    #[test]
    fn cubic_payload_stays_with_unsorted_keys_and_clips_a_negative_start() {
        let mut writer = DataWriter::default();
        writer.u32(0).u32(0).u32(2).u32(2);
        // Reverse order on disk. On [-1, 1], the authored polynomial is 1+4u-2u^2.
        writer.quadratic_scalar(1.0, 3.0, 0.0, 99.0);
        writer.quadratic_scalar(-1.0, 1.0, -99.0, 4.0);
        let (keys, glb) = publish_data(&writer);
        for (time, expected) in [(0.0, 2.5), (0.25, 2.71875), (0.5, 2.875), (1.0, 3.0)] {
            assert_eq!(keys.scale.value_at(time), expected);
            assert_published(&glb, "scale", time, [expected; 3]);
        }
    }

    #[test]
    fn constant_rotation_and_submicrosecond_discontinuities_publish_step() {
        let mut writer = DataWriter::default();
        writer.u32(3).u32(5);
        writer.linear_quat(0.0, [0.0, 0.0, 0.0, 1.0]);
        writer.linear_quat(1.0, [0.0, 0.0, 1.0, 0.0]);
        writer.linear_quat(2.0, [0.0, 1.0, 0.0, 0.0]);
        writer.u32(0).u32(3).u32(5);
        writer
            .linear_scalar(0.0, 1.0)
            .linear_scalar(0.0000002, 2.0)
            .linear_scalar(1.0, 3.0);
        let (_, glb) = publish_data(&writer);
        for (time, expected) in [
            (0.5, [0.0, 0.0, 0.0, 1.0]),
            (1.0, [0.0, 0.0, 1.0, 0.0]),
            (1.5, [0.0, 0.0, 1.0, 0.0]),
        ] {
            let actual: [f32; 4] = published_value(&glb, "rotation", time).try_into().unwrap();
            assert!(dot4(actual, expected).abs() > 0.999999);
        }
        assert_published(&glb, "scale", 0.0000001, [1.0; 3]);
        assert_published(&glb, "scale", 0.0000002, [2.0; 3]);
    }

    #[test]
    fn duplicate_cubic_times_are_reported_instead_of_dividing_by_zero() {
        let mut writer = DataWriter::default();
        writer.u32(0).u32(0).u32(2).u32(2);
        writer.quadratic_scalar(0.0, 1.0, 0.0, 0.0);
        writer.quadratic_scalar(0.0, 2.0, 0.0, 0.0);
        let error = decode_transform_data(&writer.finish()).unwrap_err();
        assert!(error.to_string().contains("duplicate cubic key times"));
    }

    /// Inspect STEP metadata and raw accessor payload, including the first output a
    /// singleton loader consumes. An evaluator alone could hide an invalid cubic.
    fn published_step_samples(glb: &[u8], path: &str) -> (Vec<f32>, Vec<f32>) {
        let (document, bin) = split_glb(glb).unwrap();
        let animation = &document["animations"][0];
        let channel = animation["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|channel| channel["target"]["path"] == path)
            .unwrap();
        let sampler = &animation["samplers"][channel["sampler"].as_u64().unwrap() as usize];
        assert_eq!(sampler["interpolation"], "STEP", "{path}");
        let input = &document["accessors"][sampler["input"].as_u64().unwrap() as usize];
        let output = &document["accessors"][sampler["output"].as_u64().unwrap() as usize];
        assert_eq!(input["type"], "SCALAR");
        assert_eq!(
            output["type"],
            if path == "rotation" { "VEC4" } else { "VEC3" }
        );
        assert_eq!(input["count"], output["count"], "{path}: one pose per time");
        let read = |accessor: &Value, width: usize| {
            assert_eq!(accessor["componentType"], 5126);
            let view = &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
            let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize
                + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
            let count = accessor["count"].as_u64().unwrap() as usize * width;
            bin[offset..offset + 4 * count]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect::<Vec<_>>()
        };
        let times = read(input, 1);
        assert_eq!(input["min"], json!([times[0]]));
        assert_eq!(input["max"], json!([times[times.len() - 1]]));
        (times, read(output, if path == "rotation" { 4 } else { 3 }))
    }

    #[test]
    fn singleton_and_nonpositive_cubic_numeric_channels_publish_the_held_pose_as_step() {
        for kind in [2, 3] {
            for times in [&[0.0][..], &[-1.0][..], &[-2.0, -1.0][..], &[-2.0, 0.0][..]] {
                let mut writer = DataWriter::default();
                writer.u32(0).u32(times.len() as u32).u32(kind);
                for (index, time) in times.iter().enumerate() {
                    let value = [index as f32 + 2.0, -3.0, 5.0];
                    if kind == 2 {
                        writer.quadratic_vec3(*time, value, [7.0; 3], [-11.0; 3]);
                    } else {
                        writer
                            .linear_vec3(*time, value)
                            .f32(0.25)
                            .f32(0.5)
                            .f32(-0.5);
                    }
                }
                writer.u32(times.len() as u32).u32(kind);
                for (index, time) in times.iter().enumerate() {
                    let value = index as f32 + 2.0;
                    if kind == 2 {
                        writer.quadratic_scalar(*time, value, 7.0, -11.0);
                    } else {
                        writer.tbc_scalar(*time, value, [0.25, 0.5, -0.5]);
                    }
                }
                let (_, glb) = publish_data(&writer);
                let held = times.len() as f32 + 1.0;
                let (input, output) = published_step_samples(&glb, "translation");
                assert_eq!(input, [0.0, 4.0], "kind {kind}, authored {times:?}");
                assert_eq!(
                    output,
                    [held, -3.0, 5.0, held, -3.0, 5.0],
                    "actual singleton pose and trailing hold"
                );
                let (input, output) = published_step_samples(&glb, "scale");
                assert_eq!(input, [0.0, 4.0], "kind {kind}, authored {times:?}");
                assert_eq!(
                    output, [held; 6],
                    "actual singleton scale and trailing hold, not a tangent"
                );
                assert_published(&glb, "translation", 4.0, [held, -3.0, 5.0]);
                assert_published(&glb, "scale", 4.0, [held; 3]);
            }
        }
    }

    #[test]
    fn step_quaternion_rotation_keeps_stop_time_and_every_authored_jump() {
        for times in [&[0.0][..], &[0.0, 0.0000002, 1.0][..]] {
            let mut writer = DataWriter::default();
            writer.u32(times.len() as u32).u32(5);
            let poses = [
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
            ];
            for (index, time) in times.iter().enumerate() {
                writer.linear_quat(*time, poses[index]);
            }
            writer.u32(0).u32(0);
            // publish_data supplies the sequence's stop time (4), with no other channels.
            let (keys, glb) = publish_data(&writer);
            assert!(keys.translation.keys.is_empty() && keys.scale.keys.is_empty());
            let (input, output) = published_step_samples(&glb, "rotation");
            let mut expected_times = times.to_vec();
            expected_times.push(4.0);
            assert_eq!(input, expected_times);
            let expected = poses[times.len() - 1];
            for (index, &actual) in output.as_chunks::<4>().0.iter().enumerate() {
                assert!(dot4(actual, poses[index.min(times.len() - 1)]).abs() > 0.999999);
            }
            for time in [3.0, 4.0] {
                let actual = published_value(&glb, "rotation", time).try_into().unwrap();
                assert!(dot4(actual, expected).abs() > 0.999999);
            }
            if times.len() > 1 {
                for (time, expected) in [
                    (0.0000001, poses[0]),
                    (0.0000002, poses[1]),
                    (1.0, poses[2]),
                ] {
                    let actual = published_value(&glb, "rotation", time).try_into().unwrap();
                    assert!(dot4(actual, expected).abs() > 0.999999);
                }
            }
        }
    }

    #[test]
    fn step_euler_rotation_keeps_stop_time_for_singleton_and_staggered_axis_jumps() {
        for singleton in [true, false] {
            let mut writer = DataWriter::default();
            writer.u32(1).u32(4);
            writer.u32(if singleton { 1 } else { 2 }).u32(5);
            writer.linear_scalar(0.0, 0.0);
            if !singleton {
                writer.linear_scalar(0.0000002, std::f32::consts::PI);
            }
            writer.u32(1).u32(5).linear_scalar(0.0, 0.0);
            writer.u32(if singleton { 1 } else { 2 }).u32(5);
            writer.linear_scalar(0.0, std::f32::consts::PI);
            if !singleton {
                writer.linear_scalar(1.0, 0.0);
            }
            writer.u32(0).u32(0);
            let (keys, glb) = publish_data(&writer);
            assert!(keys.translation.keys.is_empty() && keys.scale.keys.is_empty());
            let (input, output) = published_step_samples(&glb, "rotation");
            let expected_times = if singleton {
                vec![0.0, 4.0]
            } else {
                vec![0.0, 0.0000002, 1.0, 4.0]
            };
            assert_eq!(input, expected_times);
            // Half turns have simple independent quaternion expectations (up to sign).
            let first = [0.0, 0.0, 1.0, 0.0];
            let middle = [0.0, 1.0, 0.0, 0.0];
            let last = if singleton {
                first
            } else {
                [1.0, 0.0, 0.0, 0.0]
            };
            let expected_poses = if singleton {
                vec![first, last]
            } else {
                vec![first, middle, last, last]
            };
            for (&actual, expected) in output.as_chunks::<4>().0.iter().zip(expected_poses) {
                assert!(dot4(actual, expected).abs() > 0.999999);
            }
            for time in [3.0, 4.0] {
                let actual = published_value(&glb, "rotation", time).try_into().unwrap();
                assert!(dot4(actual, last).abs() > 0.999999);
            }
            if !singleton {
                for (time, expected) in [(0.0000001, first), (0.0000002, middle), (1.0, last)] {
                    let actual = published_value(&glb, "rotation", time).try_into().unwrap();
                    assert!(dot4(actual, expected).abs() > 0.999999);
                }
            }
        }
    }
}

#[cfg(test)]
mod decode_regressions {
    use super::tests::{glb_json, tiny_glb};
    use super::*;
    use project_wormhole_esm::structs::strings::{SizedString32, StringN};
    use project_wormhole_nif::nif_header::{Endianess, NifFileVersion};

    fn two_rotation_sequences(first_duration: f32, second_duration: f32) -> (Vec<u8>, NifHeader) {
        let data: Vec<u8> = [
            2u32,
            1,
            0f32.to_bits(),
            1f32.to_bits(),
            0,
            0,
            0,
            1f32.to_bits(),
            1f32.to_bits(),
            0,
            0,
            0,
            0,
            0,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        let mut first = sequence();
        let stop_offset = first.len() - 14; // Stop Time precedes manager/root/empty notes.
        first[stop_offset..stop_offset + 4].copy_from_slice(&first_duration.to_le_bytes());
        let mut second = sequence();
        second[0..4].copy_from_slice(&2u32.to_le_bytes()); // Healthy clip name.
        second[12..16].copy_from_slice(&5u32.to_le_bytes()); // Its interpolator.
        second[21..25].copy_from_slice(&3u32.to_le_bytes()); // Healthy node name.
        second[stop_offset..stop_offset + 4].copy_from_slice(&second_duration.to_le_bytes());
        let mut manager = vec![0; 27];
        for value in [2u32, 1, 4, u32::MAX] {
            manager.extend_from_slice(&value.to_le_bytes());
        }
        let mut first_interpolator = vec![0; 32];
        first_interpolator.extend_from_slice(&3u32.to_le_bytes());
        let mut second_interpolator = vec![0; 32];
        second_interpolator.extend_from_slice(&6u32.to_le_bytes());
        let (_, mut header) = fixture(first.clone(), data.clone());
        let blocks = [
            manager,
            first,
            first_interpolator,
            data.clone(),
            second,
            second_interpolator,
            data,
        ];
        header.block_count = 7;
        header.block_type_index = vec![0, 1, 2, 3, 1, 2, 3];
        header.block_size_index = blocks.iter().map(|block| block.len() as u32).collect();
        header.strings.extend([
            SizedString32("Healthy".into()),
            SizedString32("HealthyNode".into()),
        ]);
        header.string_count = 4;
        header.string_max_size = 11;
        (blocks.concat(), header)
    }

    #[test]
    fn oversized_rotation_sampling_is_a_notice_and_keeps_healthy_clips() {
        // RED stops at the safely sized first case; it cannot enter the huge old f32 loop.
        for duration in [60_000.0_f32, f32::MAX] {
            let (bytes, header) = two_rotation_sequences(duration, 1.0);
            let (clips, notices) = clips_from_blocks(&bytes, &header).unwrap();
            assert_eq!(
                clips.len(),
                1,
                "oversized sampling must omit only its track"
            );
            assert_eq!(clips[0].name, "Healthy");
            assert!(
                notices
                    .iter()
                    .any(|notice| notice.contains("rotation sample budget")),
                "{notices:?}"
            );
            assert_eq!(
                clips[0].tracks[0].rotation.as_ref().unwrap().times.last(),
                Some(&1.0)
            );
        }
    }

    #[test]
    fn rotation_sampling_budget_is_shared_by_all_clips_in_one_nif() {
        let (bytes, header) = two_rotation_sequences(20_000.0, 20_000.0);
        let (clips, notices) = clips_from_blocks(&bytes, &header).unwrap();
        assert_eq!(
            clips.len(),
            1,
            "two individually bounded grids must not exceed the file quota"
        );
        assert_eq!(clips[0].name, "Open");
        assert!(
            notices
                .iter()
                .any(|notice| notice.contains("rotation sample budget")),
            "{notices:?}"
        );
    }

    fn fixture(sequence: Vec<u8>, data: Vec<u8>) -> (Vec<u8>, NifHeader) {
        let mut manager = vec![0; 27];
        manager.extend_from_slice(&1u32.to_le_bytes());
        manager.extend_from_slice(&1u32.to_le_bytes());
        manager.extend_from_slice(&u32::MAX.to_le_bytes()); // no object palette
        let mut interpolator = vec![0; 32];
        interpolator.extend_from_slice(&3u32.to_le_bytes());
        let blocks = [manager, sequence, interpolator, data];
        let header = NifHeader {
            file_desc: StringN {
                value: "Gamebryo File Format, Version 20.2.0.7".into(),
            },
            nif_version: NifFileVersion(0x14020007),
            endian_type: Endianess::Little,
            user_version: 12,
            block_count: 4,
            bethesda_version: 100,
            author: None,
            process_script: None,
            export_script: None,
            max_filepath: None,
            block_types: [
                "NiControllerManager",
                "NiControllerSequence",
                "NiTransformInterpolator",
                "NiTransformData",
            ]
            .map(|name| SizedString32(name.into()))
            .into(),
            block_type_index: vec![0, 1, 2, 3],
            block_size_index: blocks.iter().map(|block| block.len() as u32).collect(),
            string_count: 2,
            string_max_size: 4,
            strings: vec![SizedString32("Open".into()), SizedString32("Node".into())],
            groups: vec![],
        };
        (blocks.concat(), header)
    }

    fn sequence() -> Vec<u8> {
        let mut bytes = Vec::new();
        for word in [0u32, 1, 0, 2, u32::MAX] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.push(0); // priority
        bytes.extend_from_slice(&1u32.to_le_bytes()); // Node
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&[0; 20]); // weight through start time
        bytes.extend_from_slice(&1f32.to_le_bytes()); // stop time
        bytes.extend_from_slice(&[0; 10]); // manager, root name, empty notes
        bytes
    }

    fn palette(entries: &[(&str, u32)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // no scene pointer
        bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (name, pointer) in entries {
            bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(&pointer.to_le_bytes());
        }
        bytes
    }

    fn palette_fixture(palette: Vec<u8>) -> (Vec<u8>, NifHeader) {
        let data = [
            0u32,
            2,
            1,
            0,
            0,
            0,
            0,
            1f32.to_bits(),
            1f32.to_bits(),
            0,
            0,
            0,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        let (mut bytes, mut header) = fixture(sequence(), data);
        bytes[35..39].copy_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&palette);
        header.block_count = 7;
        header.block_types.extend([
            SizedString32("NiDefaultAVObjectPalette".into()),
            SizedString32("NiNode".into()),
        ]);
        // The source objects are empty stubs: only block identity matters to this raw decoder.
        header.block_type_index.extend([4, 5, 5]);
        header.block_size_index.extend([palette.len() as u32, 0, 0]);
        (bytes, header)
    }

    fn palette_scene(duplicate: bool) -> Vec<u8> {
        let (mut document, bin) = split_glb(&tiny_glb()).unwrap();
        document["nodes"][1]["name"] = json!("Node");
        if duplicate {
            document["nodes"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"Node"}));
            document["nodes"][0]["children"]
                .as_array_mut()
                .unwrap()
                .push(json!(2));
        }
        rebuild_glb(&document, &bin).unwrap()
    }

    #[test]
    fn palette_selects_exact_second_node_and_accepts_repeated_identical_pointer() {
        for entries in [vec![("Node", 6)], vec![("Node", 6), ("Node", 6)]] {
            let (bytes, header) = palette_fixture(palette(&entries));
            let (clips, notices) = clips_from_blocks(&bytes, &header).unwrap();
            assert!(notices.is_empty(), "{notices:?}");
            let (glb, notices) = append_animations(
                &palette_scene(true),
                &clips,
                &BTreeMap::from([(5, 1), (6, 2)]),
            )
            .unwrap();
            assert!(notices.is_empty(), "{notices:?}");
            let document = glb_json(&glb);
            assert_eq!(
                document["animations"][0]["channels"][0]["target"]["node"],
                2
            );
        }
    }

    #[test]
    fn palette_pointer_never_guesses_a_unique_name_when_source_is_not_exported() {
        for pointer in [6, u32::MAX] {
            let (bytes, header) = palette_fixture(palette(&[("Node", pointer)]));
            let (clips, notices) = clips_from_blocks(&bytes, &header).unwrap();
            assert!(notices.is_empty(), "{notices:?}");
            for source_nodes in [
                BTreeMap::new(),
                BTreeMap::from([(5, 1)]),
                BTreeMap::from([(pointer, 99)]),
            ] {
                let input = palette_scene(false); // a tempting unique name must not win
                let (output, notices) = append_animations(&input, &clips, &source_nodes).unwrap();
                assert!(
                    glb_json(&output)["animations"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    notices
                        .iter()
                        .any(|notice| notice.contains("palette source block")),
                    "{notices:?}"
                );
                assert_eq!(glb_json(&output)["nodes"], glb_json(&input)["nodes"]);
            }
        }
    }

    #[test]
    fn missing_palette_name_retains_unique_lookup_and_duplicate_notice() {
        for entries in [vec![], vec![("Other", 6)]] {
            let (bytes, header) = palette_fixture(palette(&entries));
            let (clips, notices) = clips_from_blocks(&bytes, &header).unwrap();
            assert!(notices.is_empty(), "{notices:?}");
            for duplicate in [false, true] {
                let (output, notices) =
                    append_animations(&palette_scene(duplicate), &clips, &BTreeMap::new()).unwrap();
                let document = glb_json(&output);
                if duplicate {
                    assert!(document["animations"].as_array().unwrap().is_empty());
                    assert!(
                        notices
                            .iter()
                            .any(|notice| notice.contains("ambiguous node name")),
                        "{notices:?}"
                    );
                } else {
                    assert_eq!(
                        document["animations"][0]["channels"][0]["target"]["node"],
                        1
                    );
                    assert!(notices.is_empty(), "{notices:?}");
                }
            }
        }
    }

    #[test]
    fn ambiguous_palette_omits_only_its_track_even_with_unique_exported_name() {
        let (bytes, header) = palette_fixture(palette(&[("Node", 5), ("Node", 6), ("Node", 5)]));
        let (mut clips, notices) = clips_from_blocks(&bytes, &header).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        let mut sibling = clips[0].tracks[0].clone();
        sibling.node = "DweSmallDoorLoad01".into();
        sibling.target = TrackTarget::Name;
        clips[0].tracks.push(sibling);
        let (output, notices) = append_animations(
            &palette_scene(false),
            &clips,
            &BTreeMap::from([(5, 1), (6, 1)]),
        )
        .unwrap();
        let document = glb_json(&output);
        let channels = document["animations"][0]["channels"].as_array().unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0]["target"]["node"], 0);
        assert!(
            notices
                .iter()
                .any(|notice| notice.contains("ambiguous palette name 'Node'")),
            "{notices:?}"
        );
    }

    #[test]
    fn rejects_truncated_oversized_or_trailing_manager_layouts_and_invalid_pointers() {
        let (bytes, header) = fixture(sequence(), vec![0; 12]);
        let manager = bytes[..header.block_size_index[0] as usize].to_vec();
        for length in 0..manager.len() {
            assert!(
                read_manager(&manager[..length], 4).is_err(),
                "truncation at {length}"
            );
        }
        let mut bad = manager.clone();
        bad.push(0);
        assert!(read_manager(&bad, 4).is_err());
        for (offset, word) in [(27, u32::MAX), (0, 4), (22, 4), (35, 4)] {
            let mut bad = manager.clone();
            bad[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
            assert!(read_manager(&bad, 4).is_err(), "offset {offset}");
        }
        let mut bad = manager.clone();
        bad[26] = 2;
        assert!(read_manager(&bad, 4).is_err());
        assert!(read_manager(&manager, 4).is_ok());
    }

    #[test]
    fn malformed_palettes_warn_and_leave_transform_clips_static() {
        let valid = palette(&[("Node", 6)]);
        let mut malformed: Vec<Vec<u8>> = (0..valid.len())
            .map(|length| valid[..length].to_vec())
            .collect();
        let mut trailing = valid.clone();
        trailing.push(0);
        malformed.push(trailing);
        for (offset, word) in [(0, 7), (4, u32::MAX), (8, u32::MAX), (16, 7)] {
            let mut bad = valid.clone();
            bad[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
            malformed.push(bad);
        }
        let mut invalid_name = valid.clone();
        invalid_name[12] = 0xff;
        malformed.push(invalid_name);
        for bad in malformed {
            let (bytes, header) = palette_fixture(bad);
            let (clips, notices) = clips_from_blocks(&bytes, &header).unwrap();
            assert!(clips.is_empty());
            assert!(
                notices
                    .iter()
                    .any(|notice| notice.contains("NiDefaultAVObjectPalette block 4")),
                "{notices:?}"
            );
        }
    }

    #[test]
    fn wrong_palette_type_warns_without_name_fallback() {
        let (bytes, mut header) = palette_fixture(palette(&[("Node", 6)]));
        header.block_type_index[4] = 5; // NiNode, not a palette
        let (clips, notices) = clips_from_blocks(&bytes, &header).unwrap();
        assert!(clips.is_empty());
        assert!(
            notices.iter().any(|notice| notice.contains("non-palette")),
            "{notices:?}"
        );
    }

    #[test]
    fn listed_sequence_back_reference_does_not_reject_readable_tracks() {
        for owner in [u32::MAX, 6] {
            for has_palette in [false, true] {
                let (bytes, mut header) = palette_fixture(palette(&[("Node", 6)]));
                let mut blocks: Vec<Vec<u8>> = blocks_of(&bytes, &header)
                    .unwrap()
                    .into_iter()
                    .map(<[u8]>::to_vec)
                    .collect();
                blocks[1][65..69].copy_from_slice(&owner.to_le_bytes());
                if !has_palette {
                    blocks[0][35..39].copy_from_slice(&u32::MAX.to_le_bytes());
                }
                header.block_size_index = blocks.iter().map(|block| block.len() as u32).collect();
                let (clips, notices) = clips_from_blocks(&blocks.concat(), &header).unwrap();
                assert!(notices.is_empty(), "{notices:?}");
                assert_eq!(clips.len(), 1);
                let (output, notices) = append_animations(
                    &palette_scene(has_palette),
                    &clips,
                    &BTreeMap::from([(6, 2)]),
                )
                .unwrap();
                assert!(notices.is_empty(), "{notices:?}");
                assert_eq!(
                    glb_json(&output)["animations"][0]["channels"][0]["target"]["node"],
                    if has_palette { 2 } else { 1 }
                );
            }
        }
    }

    #[test]
    fn bad_sequence_reference_preserves_healthy_siblings() {
        for bad_reference in [u32::MAX, 99, 2] {
            let (bytes, mut header) = palette_fixture(palette(&[("Node", 6)]));
            let mut blocks: Vec<Vec<u8>> = blocks_of(&bytes, &header)
                .unwrap()
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect();
            blocks[0][27..31].copy_from_slice(&2u32.to_le_bytes());
            blocks[0].splice(35..35, bad_reference.to_le_bytes());
            header.block_size_index = blocks.iter().map(|block| block.len() as u32).collect();
            let (clips, notices) = clips_from_blocks(&blocks.concat(), &header).unwrap();
            assert_eq!(clips.len(), 1, "{notices:?}");
            assert_eq!(notices.len(), 1, "{notices:?}");
            assert!(
                notices[0].contains(if bad_reference == 2 {
                    "non-sequence"
                } else {
                    "missing sequence"
                }),
                "{notices:?}"
            );
            let (output, notices) =
                append_animations(&palette_scene(true), &clips, &BTreeMap::from([(6, 2)])).unwrap();
            assert!(notices.is_empty(), "{notices:?}");
            assert_eq!(
                glb_json(&output)["animations"][0]["channels"][0]["target"]["node"],
                2
            );
        }
    }

    #[test]
    fn each_manager_binds_its_own_sequence_through_its_own_palette() {
        let (bytes, mut header) = palette_fixture(palette(&[("Node", 5)]));
        let mut blocks: Vec<Vec<u8>> = blocks_of(&bytes, &header)
            .unwrap()
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        let mut other_manager = blocks[0].clone();
        other_manager[31..35].copy_from_slice(&8u32.to_le_bytes());
        other_manager[35..39].copy_from_slice(&9u32.to_le_bytes());
        let mut other_sequence = sequence();
        other_sequence[65..69].copy_from_slice(&7u32.to_le_bytes());
        blocks.extend([other_manager, other_sequence, palette(&[("Node", 6)])]);
        header.block_count = 10;
        header.block_type_index.extend([0, 1, 4]);
        header.block_size_index = blocks.iter().map(|block| block.len() as u32).collect();
        let (clips, notices) = clips_from_blocks(&blocks.concat(), &header).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(clips.len(), 2);
        let (output, notices) = append_animations(
            &palette_scene(true),
            &clips,
            &BTreeMap::from([(5, 1), (6, 2)]),
        )
        .unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        let document = glb_json(&output);
        assert_eq!(
            document["animations"][0]["channels"][0]["target"]["node"],
            1
        );
        assert_eq!(
            document["animations"][1]["channels"][0]["target"]["node"],
            2
        );
    }

    #[test]
    fn unreadable_sequence_warns_instead_of_discarding_the_model() {
        let (bytes, header) = fixture(vec![0], vec![0; 12]);
        let (clips, warnings) = clips_from_blocks(&bytes, &header).unwrap();
        assert!(clips.is_empty());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("NiControllerSequence block 1")),
            "{warnings:?}"
        );
    }

    #[test]
    fn unreadable_sequence_does_not_remove_a_valid_sibling_clip() {
        let data = [
            0u32,
            1,
            1,
            0,
            1f32.to_bits(),
            2f32.to_bits(),
            3f32.to_bits(),
            0,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        let (mut bytes, mut header) = fixture(sequence(), data);
        // Manager references the existing valid sequence and an additional unreadable one.
        bytes[27..31].copy_from_slice(&2u32.to_le_bytes());
        bytes.splice(35..35, 4u32.to_le_bytes());
        bytes.push(0);
        header.block_count += 1;
        header.block_size_index[0] += 4;
        header.block_size_index.push(1);
        header.block_type_index.push(1);
        let (clips, warnings) = clips_from_blocks(&bytes, &header).unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].name, "Open");
        assert_eq!(clips[0].tracks[0].node, "Node");
        assert!(clips[0].tracks[0].translation.is_some());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("NiControllerSequence block 4")),
            "{warnings:?}"
        );
    }

    #[test]
    fn unsupported_transform_data_warns_and_keeps_the_static_pose() {
        // One rotation key with an unknown interpolation type; this must stay rejected.
        let (bytes, header) = fixture(
            sequence(),
            [1u32.to_le_bytes(), 99u32.to_le_bytes()].concat(),
        );
        let (clips, warnings) = clips_from_blocks(&bytes, &header).unwrap();
        assert!(clips.is_empty());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("NiTransformData block 3")
                    && warning.contains("unknown NIF key type 99")),
            "{warnings:?}"
        );
    }

    #[test]
    fn unreadable_animation_source_returns_original_glb_with_a_warning() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unsupported.nif");
        fs::write(&path, b"unreadable animation source").unwrap();
        let original = b"already validated model";
        let (output, warnings) = append_nif_animations(original, &path, &BTreeMap::new()).unwrap();
        assert_eq!(output, original);
        assert_eq!(warnings.len(), 1);
    }

    fn single_node_clip() -> Clip {
        Clip {
            name: "Open".into(),
            tracks: vec![ClipTrack {
                node: "Object02".into(),
                target: TrackTarget::Name,
                rotation: None,
                scale: None,
                translation: Some(SampledChannel {
                    interpolation: "LINEAR",
                    times: vec![0.0, 1.0],
                    values: vec![[0.0; 3], [1.0, 0.0, 0.0]],
                }),
            }],
        }
    }

    #[test]
    fn duplicate_node_names_skip_ambiguous_track_and_keep_unique_sibling() {
        let (mut document, bin) = split_glb(&tiny_glb()).unwrap();
        document["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"Object02"}));
        let input = rebuild_glb(&document, &bin).unwrap();
        let mut clip = single_node_clip();
        let mut sibling = clip.tracks[0].clone();
        sibling.node = "DweSmallDoorLoad01".into();
        clip.tracks.push(sibling);
        let (output, notices) = append_animations(&input, &[clip], &BTreeMap::new()).unwrap();
        assert!(
            notices
                .iter()
                .any(|notice| notice.contains("ambiguous") && notice.contains("Object02")),
            "{notices:?}"
        );
        let document = glb_json(&output);
        assert_eq!(
            document["animations"][0]["channels"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            document["animations"][0]["channels"][0]["target"]["node"],
            0
        );
    }

    #[test]
    fn missing_animation_buffer_preserves_static_glb_with_notice() {
        let (mut document, _) = split_glb(&tiny_glb()).unwrap();
        document.as_object_mut().unwrap().remove("buffers");
        let input = rebuild_glb(&document, &[]).unwrap();
        let (output, notices) =
            append_animations(&input, &[single_node_clip()], &BTreeMap::new()).unwrap();
        assert_eq!(output, input);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("buffer"), "{notices:?}");
    }

    #[test]
    fn unreadable_animation_diagnostics_and_embedded_glb_are_location_independent() {
        let directory = tempfile::tempdir().unwrap();
        for contents in [None, Some(b"malformed NIF".as_slice())] {
            let mut outputs = Vec::new();
            let mut all_notices = Vec::new();
            for root in ["private-a", "private-b"] {
                let parent = directory.path().join(root);
                fs::create_dir_all(&parent).unwrap();
                let path = parent.join("model.nif");
                if let Some(contents) = contents {
                    fs::write(&path, contents).unwrap();
                }
                let (glb, notices) =
                    append_nif_animations(&tiny_glb(), &path, &BTreeMap::new()).unwrap();
                assert_eq!(notices.len(), 1);
                for notice in &notices {
                    assert!(!notice.contains(root), "{notice}");
                    assert!(
                        !notice.contains(&directory.path().to_string_lossy().to_string()),
                        "{notice}"
                    );
                }
                let (mut document, bin) = split_glb(&glb).unwrap();
                document["asset"]["extras"]["openSkyrim"]["animationWarnings"] = json!(notices);
                outputs.push(rebuild_glb(&document, &bin).unwrap());
                all_notices.push(notices);
            }
            assert_eq!(all_notices[0], all_notices[1]);
            assert_eq!(outputs[0], outputs[1]);
        }
    }
}
