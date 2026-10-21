//! Havok 2010.2 class layouts for the 64-bit packfile used by Skyrim SE.
//!
//! The packfile carries no member table, so every offset below is fixed by the format. Only
//! the fields the decoder reads are modelled.

use super::packfile::{Packfile, Reader};
use color_eyre::{
    Result,
    eyre::{ensure, eyre},
};

#[derive(Debug, Clone)]
pub struct NamedVariant {
    pub class_name: String,
    pub variant: Option<u32>,
}

/// `hkRootLevelContainer`: `namedVariants` hkArray at 0; each variant is 24 bytes
/// (name pointer @0, className pointer @8, variant pointer @16).
pub fn root_level_container(packfile: &Packfile) -> Result<Vec<NamedVariant>> {
    let root = packfile.object_at(packfile.contents_offset)?;
    ensure!(
        root.class == "hkRootLevelContainer",
        "contents object is {}, expected hkRootLevelContainer",
        root.class
    );
    let (at, count) = packfile.reader(root.offset).array(0)?;
    let mut variants = Vec::with_capacity(count as usize);
    for index in 0..count {
        let at = element_offset(at, index, 24, "root-level variant")?;
        let variant = packfile.reader(at);
        variants.push(NamedVariant {
            class_name: variant.string(8)?.unwrap_or_default(),
            variant: variant.ptr(16)?,
        });
    }
    Ok(variants)
}

pub fn pointer_array(packfile: &Packfile, base: u32, offset: u32) -> Result<Vec<u32>> {
    let (at, count) = packfile.reader(base).array(offset)?;
    let mut pointers = Vec::with_capacity(count as usize);
    for index in 0..count {
        let slot = element_offset(at, index, 8, "pointer array")?;
        let pointer = packfile
            .reader(slot)
            .ptr(0)?
            .ok_or_else(|| eyre!("null pointer in array at {base:#x}[{index}]"))?;
        pointers.push(pointer);
    }
    Ok(pointers)
}

/// `hkaAnimationContainer` (`hkReferencedObject` base is 16 bytes).
pub struct AnimationContainer {
    pub skeletons: Vec<u32>,
    pub animations: Vec<u32>,
    pub bindings: Vec<u32>,
}

pub fn animation_container(packfile: &Packfile, at: u32) -> Result<AnimationContainer> {
    Ok(AnimationContainer {
        skeletons: pointer_array(packfile, at, 16)?,
        animations: pointer_array(packfile, at, 32)?,
        bindings: pointer_array(packfile, at, 48)?,
    })
}

#[derive(Debug, Clone)]
pub struct Skeleton {
    pub name: String,
    pub parent_indices: Vec<i16>,
    pub bone_names: Vec<String>,
}

/// `hkaSkeleton`: name @16, parentIndices @24, bones @40 (hkaBone = 16 bytes:
/// name pointer @0, lockTranslation @8).
pub fn skeleton(packfile: &Packfile, at: u32) -> Result<Skeleton> {
    let reader = packfile.reader(at);
    let parents = reader.array_bytes(24, 2)?;
    let (bones_at, bone_count) = reader.array(40)?;
    let parent_indices: Vec<i16> = parents
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect();
    let mut bone_names = Vec::with_capacity(bone_count as usize);
    for index in 0..bone_count {
        let at = element_offset(bones_at, index, 16, "skeleton bone")?;
        bone_names.push(packfile.reader(at).string(0)?.unwrap_or_default());
    }
    ensure!(
        parent_indices.len() == bone_names.len(),
        "skeleton arrays disagree: {} parents for {} bones",
        parent_indices.len(),
        bone_names.len()
    );
    Ok(Skeleton {
        name: reader.string(16)?.unwrap_or_default(),
        parent_indices,
        bone_names,
    })
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub original_skeleton_name: String,
    pub animation: Option<u32>,
    pub transform_track_to_bone: Vec<i16>,
}

/// `hkaAnimationBinding`: originalSkeletonName @16, animation pointer @24,
/// transformTrackToBoneIndices @32.
pub fn binding(packfile: &Packfile, at: u32) -> Result<Binding> {
    let reader = packfile.reader(at);
    let map = reader.array_bytes(32, 2)?;
    Ok(Binding {
        original_skeleton_name: reader.string(16)?.unwrap_or_default(),
        animation: reader.ptr(24)?,
        transform_track_to_bone: map
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect(),
    })
}

#[derive(Debug, Clone)]
pub struct SplineAnimation {
    pub duration: f32,
    pub num_transform_tracks: i32,
    pub num_float_tracks: i32,
    pub extracted_motion: Option<u32>,
    pub num_frames: i32,
    pub num_blocks: i32,
    pub max_frames_per_block: i32,
    pub frame_duration: f32,
    pub block_offsets: Vec<u32>,
    pub float_block_offsets: Vec<u32>,
    pub data: Vec<u8>,
    pub endian: i32,
}

/// `hkaAnimation` @16..56 and `hkaSplineCompressedAnimation` @56..176.
pub fn spline_animation(packfile: &Packfile, at: u32) -> Result<SplineAnimation> {
    let reader = packfile.reader(at);
    Ok(SplineAnimation {
        duration: reader.f32(20)?,
        num_transform_tracks: reader.i32(24)?,
        num_float_tracks: reader.i32(28)?,
        extracted_motion: reader.ptr(32)?,
        num_frames: reader.i32(56)?,
        num_blocks: reader.i32(60)?,
        max_frames_per_block: reader.i32(64)?,
        frame_duration: reader.f32(80)?,
        block_offsets: u32_array(reader, 88)?,
        float_block_offsets: u32_array(reader, 104)?,
        data: reader.array_bytes(152, 1)?.to_vec(),
        endian: reader.i32(168)?,
    })
}

fn u32_array(reader: Reader<'_>, offset: u32) -> Result<Vec<u32>> {
    Ok(reader
        .array_bytes(offset, 4)?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .collect())
}

fn element_offset(base: u32, index: u32, stride: u32, what: &str) -> Result<u32> {
    index
        .checked_mul(stride)
        .and_then(|offset| base.checked_add(offset))
        .ok_or_else(|| eyre!("{what} table overflows at index {index}"))
}
