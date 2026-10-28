//! Cluster LOD payload contract shared by the offline converter and the runtime (ADR-0013).
//!
//! Two payload kinds: mesh clusters (`.mcl`, one converted GLB primitive) and chunk instances
//! (`.mci`, one block of exterior cells). Both are flat little-endian files whose header carries
//! the builder identity (section 1 of the ADR). The readers validate everything a renderer would
//! otherwise trust (magic, version, section bounds, record ranges, monotonic errors) and return
//! borrowed sections; the writers produce byte-identical output for identical inputs.
//!
//! The database tables are optional and additive (ADR section 6): [`CLUSTER_TABLES_DDL`] is the
//! only place they are defined.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt;

pub const MCL_MAGIC: [u8; 4] = *b"MCL1";
pub const MCI_MAGIC: [u8; 4] = *b"MCI1";
/// Layout version of both payload kinds; also `payload_format` in the builder identity.
pub const PAYLOAD_FORMAT_VERSION: u32 = 3;
/// Vertex stream layout version (`vertex_layout` in the builder identity).
pub const VERTEX_LAYOUT_VERSION: u32 = 1;
/// Eligibility rules version (`eligibility_rules` in the builder identity).
pub const ELIGIBILITY_RULES_VERSION: u32 = 1;
/// Side of one instance block in cells (`block_side_cells`), `LodTier::Tier16`.
pub const BLOCK_SIDE_CELLS: u32 = 16;

pub const MCL_HEADER_BYTES: usize = 160;
pub const MCL_SECTION_COUNT: usize = 10;
pub const MCL_SECTION_TABLE_BYTES: usize = MCL_SECTION_COUNT * 16;
pub const SECTION_ALIGNMENT: usize = 256;
pub const CLUSTER_RECORD_BYTES: usize = 48;
pub const GROUP_RECORD_BYTES: usize = 32;
pub const BVH_NODE_BYTES: usize = 32;
pub const MCI_HEADER_BYTES: usize = 96;
pub const MCI_MESH_ENTRY_BYTES: usize = 48;
pub const INSTANCE_RECORD_BYTES: usize = 96;

/// `refined_group` of a cluster that is source geometry.
pub const NO_GROUP: u16 = 0xFFFF;
/// Error of the terminal group, which never simplifies further.
pub const TERMINAL_ERROR: f32 = f32::MAX;

pub const FLAG_VERTEX_COLOURS: u32 = 1 << 0;
pub const FLAG_TANGENTS: u32 = 1 << 1;
/// `InstanceRecord::flags` bit 0: the instance comes from a child worldspace of the block's
/// worldspace (`source_worldspace_id` names it).
pub const INSTANCE_FLAG_CHILD_WORLDSPACE: u32 = 1 << 0;

/// Sections of an `.mcl` file, in file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum MclSection {
    Positions = 0,
    Normals = 1,
    Uvs = 2,
    Tangents = 3,
    Colours = 4,
    Clusters = 5,
    ClusterVertices = 6,
    ClusterTriangles = 7,
    Groups = 8,
    Bvh = 9,
}

/// Why a payload was refused. [`ClusterFormatError::reason`] is the stable machine code for
/// `cluster_rejects`, logs and the engine's terminal failure class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterFormatError {
    Truncated(&'static str),
    BadMagic,
    BadVersion(u32),
    SectionOutOfBounds(usize),
    SectionMisaligned(usize),
    SectionLength(usize),
    BadRecord(String),
    NonMonotonicError(String),
    TooManyClusters(usize),
    Unsorted(&'static str),
    InvalidInput(String),
}

impl ClusterFormatError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Truncated(_) => "truncated",
            Self::BadMagic => "bad_magic",
            Self::BadVersion(_) => "bad_version",
            Self::SectionOutOfBounds(_) => "section_out_of_bounds",
            Self::SectionMisaligned(_) => "section_misaligned",
            Self::SectionLength(_) => "section_length",
            Self::BadRecord(_) => "bad_record",
            Self::NonMonotonicError(_) => "non_monotonic_error",
            Self::TooManyClusters(_) => "too_many_clusters",
            Self::Unsorted(_) => "unsorted",
            Self::InvalidInput(_) => "invalid_input",
        }
    }
}

impl fmt::Display for ClusterFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(what) => write!(f, "payload is truncated: {what}"),
            Self::BadMagic => write!(f, "payload magic does not match"),
            Self::BadVersion(version) => write!(f, "unsupported payload format version {version}"),
            Self::SectionOutOfBounds(index) => write!(f, "section {index} lies outside the file"),
            Self::SectionMisaligned(index) => {
                write!(f, "section {index} is not {SECTION_ALIGNMENT}-byte aligned")
            }
            Self::SectionLength(index) => {
                write!(f, "section {index} length does not match the header counts")
            }
            Self::BadRecord(why) => write!(f, "invalid record: {why}"),
            Self::NonMonotonicError(why) => {
                write!(f, "simplification error is not monotonic: {why}")
            }
            Self::TooManyClusters(count) => {
                write!(
                    f,
                    "{count} clusters or groups exceed the 65,535 record limit"
                )
            }
            Self::Unsorted(what) => write!(f, "{what} is not in canonical order"),
            Self::InvalidInput(why) => write!(f, "invalid payload input: {why}"),
        }
    }
}

impl std::error::Error for ClusterFormatError {}

// Records -----------------------------------------------------------------------------------

/// One cluster (48 bytes). See ADR-0013 section 2 for the field meanings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterRecord {
    pub bounds: [f32; 4],
    pub error: f32,
    pub parent_error: f32,
    pub vertex_offset: u32,
    pub triangle_offset: u32,
    pub vertex_count: u8,
    pub triangle_count: u8,
    pub group: u16,
    pub refined_group: u16,
}

impl ClusterRecord {
    pub fn to_bytes(&self) -> [u8; CLUSTER_RECORD_BYTES] {
        let mut out = [0u8; CLUSTER_RECORD_BYTES];
        put_f32s(&mut out, 0, &self.bounds);
        put_f32s(&mut out, 16, &[self.error, self.parent_error]);
        put_u32(&mut out, 24, self.vertex_offset);
        put_u32(&mut out, 28, self.triangle_offset);
        out[32] = self.vertex_count;
        out[33] = self.triangle_count;
        put_u16(&mut out, 34, self.group);
        put_u16(&mut out, 36, self.refined_group);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            bounds: get_f32s(bytes, 0),
            error: get_f32(bytes, 16),
            parent_error: get_f32(bytes, 20),
            vertex_offset: get_u32(bytes, 24),
            triangle_offset: get_u32(bytes, 28),
            vertex_count: bytes[32],
            triangle_count: bytes[33],
            group: get_u16(bytes, 34),
            refined_group: get_u16(bytes, 36),
        }
    }
}

/// One group (32 bytes): a merge-and-simplify step of the DAG.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroupRecord {
    pub bounds: [f32; 4],
    pub error: f32,
    pub depth: u16,
    pub first_cluster: u16,
    pub cluster_count: u16,
}

impl GroupRecord {
    pub fn to_bytes(&self) -> [u8; GROUP_RECORD_BYTES] {
        let mut out = [0u8; GROUP_RECORD_BYTES];
        put_f32s(&mut out, 0, &self.bounds);
        put_f32s(&mut out, 16, &[self.error]);
        put_u16(&mut out, 20, self.depth);
        put_u16(&mut out, 22, self.first_cluster);
        put_u16(&mut out, 24, self.cluster_count);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            bounds: get_f32s(bytes, 0),
            error: get_f32(bytes, 16),
            depth: get_u16(bytes, 20),
            first_cluster: get_u16(bytes, 22),
            cluster_count: get_u16(bytes, 24),
        }
    }
}

/// One BVH node (32 bytes). `group` is [`NO_GROUP`] for an internal node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BvhNode {
    pub bounds: [f32; 4],
    pub error: f32,
    pub first_child: u32,
    pub child_count: u16,
    pub group: u16,
}

impl BvhNode {
    pub fn to_bytes(&self) -> [u8; BVH_NODE_BYTES] {
        let mut out = [0u8; BVH_NODE_BYTES];
        put_f32s(&mut out, 0, &self.bounds);
        put_f32s(&mut out, 16, &[self.error]);
        put_u32(&mut out, 20, self.first_child);
        put_u16(&mut out, 24, self.child_count);
        put_u16(&mut out, 26, self.group);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            bounds: get_f32s(bytes, 0),
            error: get_f32(bytes, 16),
            first_child: get_u32(bytes, 20),
            child_count: get_u16(bytes, 24),
            group: get_u16(bytes, 26),
        }
    }
}

/// One placed object (96 bytes).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InstanceRecord {
    /// 3x4 row-major, mesh space to world space.
    pub transform: [f32; 12],
    /// Index into the mesh table.
    pub mesh: u32,
    pub cell_x: i32,
    pub cell_y: i32,
    pub stable_id: [u8; 16],
    pub flags: u32,
    /// The base object's form id in the world database (`statics.id`), so the engine can look up
    /// per-object data such as the snow material; 0 when unknown.
    pub base_form_id: u32,
    /// The base object's snow material (`MATO` form id, `statics.material_object`); 0 = none.
    pub snow_material: u32,
    /// The child worldspace the object is placed in, 0 when it is placed in the block's own
    /// worldspace. Set iff `flags` has [`INSTANCE_FLAG_CHILD_WORLDSPACE`].
    pub source_worldspace_id: u32,
}

impl InstanceRecord {
    pub fn to_bytes(&self) -> [u8; INSTANCE_RECORD_BYTES] {
        let mut out = [0u8; INSTANCE_RECORD_BYTES];
        put_f32s(&mut out, 0, &self.transform);
        put_u32(&mut out, 48, self.mesh);
        put_u32(&mut out, 52, self.cell_x as u32);
        put_u32(&mut out, 56, self.cell_y as u32);
        out[60..76].copy_from_slice(&self.stable_id);
        put_u32(&mut out, 76, self.flags);
        put_u32(&mut out, 80, self.base_form_id);
        put_u32(&mut out, 84, self.snow_material);
        put_u32(&mut out, 88, self.source_worldspace_id);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut transform = [0f32; 12];
        for (index, value) in transform.iter_mut().enumerate() {
            *value = get_f32(bytes, index * 4);
        }
        let mut stable_id = [0u8; 16];
        stable_id.copy_from_slice(&bytes[60..76]);
        Self {
            transform,
            mesh: get_u32(bytes, 48),
            cell_x: get_u32(bytes, 52) as i32,
            cell_y: get_u32(bytes, 56) as i32,
            stable_id,
            flags: get_u32(bytes, 76),
            base_form_id: get_u32(bytes, 80),
            snow_material: get_u32(bytes, 84),
            source_worldspace_id: get_u32(bytes, 88),
        }
    }
}

// Vertex encodings ----------------------------------------------------------------------------

/// Octahedral encoding of a unit vector to two `snorm16` values (Cigolle et al. 2014).
pub fn encode_oct_snorm16(v: [f32; 3]) -> [i16; 2] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let (x, y, z) = if length > 0.0 && length.is_finite() {
        (v[0] / length, v[1] / length, v[2] / length)
    } else {
        (0.0, 0.0, 1.0)
    };
    let l1 = x.abs() + y.abs() + z.abs();
    let (mut u, mut w) = (x / l1, y / l1);
    if z < 0.0 {
        let (fu, fw) = (
            (1.0 - w.abs()) * sign_not_zero(u),
            (1.0 - u.abs()) * sign_not_zero(w),
        );
        u = fu;
        w = fw;
    }
    [snorm16(u), snorm16(w)]
}

/// Inverse of [`encode_oct_snorm16`], normalised.
pub fn decode_oct_snorm16(e: [i16; 2]) -> [f32; 3] {
    let u = f32::from(e[0]) / 32767.0;
    let w = f32::from(e[1]) / 32767.0;
    let mut z = 1.0 - u.abs() - w.abs();
    let (mut x, mut y) = (u, w);
    if z < 0.0 {
        x = (1.0 - w.abs()) * sign_not_zero(u);
        y = (1.0 - u.abs()) * sign_not_zero(w);
    }
    let length = (x * x + y * y + z * z).sqrt();
    if length == 0.0 {
        z = 1.0;
        return [0.0, 0.0, z];
    }
    [x / length, y / length, z / length]
}

fn sign_not_zero(v: f32) -> f32 {
    if v >= 0.0 { 1.0 } else { -1.0 }
}

fn snorm16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// Quantises a UV to `unorm16` against the primitive's `uv_min` and `uv_scale`.
pub fn encode_uv_unorm16(uv: [f32; 2], uv_min: [f32; 2], uv_scale: [f32; 2]) -> [u16; 2] {
    let q = |value: f32, min: f32, scale: f32| {
        if scale <= 0.0 {
            0
        } else {
            (((value - min) / scale).clamp(0.0, 1.0) * 65535.0).round() as u16
        }
    };
    [
        q(uv[0], uv_min[0], uv_scale[0]),
        q(uv[1], uv_min[1], uv_scale[1]),
    ]
}

pub fn decode_uv_unorm16(q: [u16; 2], uv_min: [f32; 2], uv_scale: [f32; 2]) -> [f32; 2] {
    [
        uv_min[0] + f32::from(q[0]) / 65535.0 * uv_scale[0],
        uv_min[1] + f32::from(q[1]) / 65535.0 * uv_scale[1],
    ]
}

// Builder identity ----------------------------------------------------------------------------

/// Every input of the builder identity (ADR-0013 section 1). `builder_config` is the canonical
/// JSON object of the builder's settings, built by the converter so it names exactly what its
/// wrapper sets.
#[derive(Debug, Clone)]
pub struct BuilderIdentityInputs {
    pub clusterlod_source: String,
    pub meshoptimizer: String,
    pub builder_config: Value,
    pub triangle_threshold: u32,
    pub converter_schema: u32,
}

impl BuilderIdentityInputs {
    /// Canonical JSON: sorted keys, no whitespace.
    pub fn canonical_json(&self) -> Vec<u8> {
        let value = json!({
            "payload_format": PAYLOAD_FORMAT_VERSION,
            "vertex_layout": VERTEX_LAYOUT_VERSION,
            "eligibility_rules": ELIGIBILITY_RULES_VERSION,
            "clusterlod_source": self.clusterlod_source,
            "meshoptimizer": self.meshoptimizer,
            "builder_config": self.builder_config,
            "triangle_threshold": self.triangle_threshold,
            "block_side_cells": BLOCK_SIDE_CELLS,
            "converter_schema": self.converter_schema,
        });
        canonical_json(&value)
    }

    pub fn identity(&self) -> [u8; 32] {
        Sha256::digest(self.canonical_json()).into()
    }

    pub fn identity_hex(&self) -> String {
        hex(&self.identity())
    }
}

/// Canonical JSON: object keys sorted by byte order at every level, no whitespace. Written out
/// explicitly so it never depends on whether another crate enabled `serde_json`'s
/// `preserve_order` feature.
pub fn canonical_json(value: &Value) -> Vec<u8> {
    fn write(value: &Value, out: &mut Vec<u8>) {
        match value {
            Value::Object(map) => {
                out.push(b'{');
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    out.extend_from_slice(
                        serde_json::to_string(key)
                            .expect("a string serialises")
                            .as_bytes(),
                    );
                    out.push(b':');
                    write(&map[key], out);
                }
                out.push(b'}');
            }
            Value::Array(items) => {
                out.push(b'[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    write(item, out);
                }
                out.push(b']');
            }
            other => out.extend_from_slice(
                serde_json::to_string(other)
                    .expect("a scalar serialises")
                    .as_bytes(),
            ),
        }
    }
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

pub fn builder_identity(inputs: &BuilderIdentityInputs) -> [u8; 32] {
    inputs.identity()
}

pub(crate) use crate::extensions::hex;

/// Parses a 64-digit lowercase or uppercase hex SHA-256.
pub fn parse_hash(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

// Paths ---------------------------------------------------------------------------------------

/// `clusters/meshes/<hh>/<mesh_hash>-p<N>.mcl`
pub fn mesh_payload_path(mesh_hash_hex: &str, primitive_index: u32) -> String {
    let prefix = mesh_hash_hex.get(..2).unwrap_or("00");
    format!("clusters/meshes/{prefix}/{mesh_hash_hex}-p{primitive_index}.mcl")
}

/// `clusters/instances/<worldspace hex>/<side>/cell_<ax>_<ay>.mci`
pub fn instance_payload_path(
    worldspace_id: u32,
    side_cells: i32,
    anchor_x: i32,
    anchor_y: i32,
) -> String {
    format!("clusters/instances/{worldspace_id:08x}/{side_cells}/cell_{anchor_x}_{anchor_y}.mci")
}

// Mesh cluster payload ------------------------------------------------------------------------

/// Everything an `.mcl` holds, borrowed. The counts and flags in the header derive from it.
#[derive(Debug, Clone)]
pub struct MclContents<'a> {
    pub builder_identity: [u8; 32],
    pub mesh_hash: [u8; 32],
    pub primitive_index: u32,
    pub root_error: f32,
    /// Whole-mesh bounding sphere: centre xyz, radius.
    pub bounds: [f32; 4],
    pub uv_min: [f32; 2],
    pub uv_scale: [f32; 2],
    pub positions: &'a [[f32; 3]],
    pub normals: &'a [[i16; 2]],
    pub uvs: &'a [[u16; 2]],
    pub tangents: Option<&'a [[i16; 2]]>,
    pub colours: Option<&'a [[u8; 4]]>,
    pub clusters: &'a [ClusterRecord],
    pub cluster_vertices: &'a [u32],
    pub cluster_triangles: &'a [[u8; 3]],
    pub groups: &'a [GroupRecord],
    pub bvh: &'a [BvhNode],
}

pub fn write_mcl(contents: &MclContents<'_>) -> Result<Vec<u8>, ClusterFormatError> {
    let vertex_count = contents.positions.len();
    let lengths_match = contents.normals.len() == vertex_count
        && contents.uvs.len() == vertex_count
        && contents.tangents.is_none_or(|t| t.len() == vertex_count)
        && contents.colours.is_none_or(|c| c.len() == vertex_count);
    if !lengths_match {
        return Err(ClusterFormatError::InvalidInput(
            "vertex streams differ in length".into(),
        ));
    }
    if contents.clusters.len() > usize::from(u16::MAX)
        || contents.groups.len() > usize::from(u16::MAX)
    {
        return Err(ClusterFormatError::TooManyClusters(
            contents.clusters.len().max(contents.groups.len()),
        ));
    }
    let mut flags = 0;
    if contents.colours.is_some() {
        flags |= FLAG_VERTEX_COLOURS;
    }
    if contents.tangents.is_some() {
        flags |= FLAG_TANGENTS;
    }

    let mut sections: [Vec<u8>; MCL_SECTION_COUNT] = Default::default();
    sections[0] = contents
        .positions
        .iter()
        .flat_map(|p| p.iter().flat_map(|c| c.to_le_bytes()))
        .collect();
    sections[1] = contents
        .normals
        .iter()
        .flat_map(|n| n.iter().flat_map(|c| c.to_le_bytes()))
        .collect();
    sections[2] = contents
        .uvs
        .iter()
        .flat_map(|u| u.iter().flat_map(|c| c.to_le_bytes()))
        .collect();
    if let Some(tangents) = contents.tangents {
        sections[3] = tangents
            .iter()
            .flat_map(|n| n.iter().flat_map(|c| c.to_le_bytes()))
            .collect();
    }
    if let Some(colours) = contents.colours {
        sections[4] = colours.iter().flatten().copied().collect();
    }
    sections[5] = contents
        .clusters
        .iter()
        .flat_map(|c| c.to_bytes())
        .collect();
    sections[6] = contents
        .cluster_vertices
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    sections[7] = contents
        .cluster_triangles
        .iter()
        .flatten()
        .copied()
        .collect();
    sections[8] = contents.groups.iter().flat_map(|g| g.to_bytes()).collect();
    sections[9] = contents.bvh.iter().flat_map(|n| n.to_bytes()).collect();

    let mut file = vec![0u8; MCL_HEADER_BYTES + MCL_SECTION_TABLE_BYTES];
    file[0..4].copy_from_slice(&MCL_MAGIC);
    put_u32(&mut file, 4, PAYLOAD_FORMAT_VERSION);
    file[8..40].copy_from_slice(&contents.builder_identity);
    file[40..72].copy_from_slice(&contents.mesh_hash);
    put_u32(&mut file, 72, contents.primitive_index);
    put_u32(&mut file, 76, flags);
    put_u32(&mut file, 80, vertex_count as u32);
    put_u32(&mut file, 84, contents.clusters.len() as u32);
    put_u32(&mut file, 88, contents.groups.len() as u32);
    put_u32(&mut file, 92, contents.bvh.len() as u32);
    put_f32s(&mut file, 96, &[contents.root_error]);
    put_f32s(&mut file, 100, &contents.bounds);
    put_f32s(&mut file, 116, &contents.uv_min);
    put_f32s(&mut file, 124, &contents.uv_scale);

    for (index, section) in sections.iter().enumerate() {
        if section.is_empty() {
            continue;
        }
        pad_to_alignment(&mut file);
        let entry = MCL_HEADER_BYTES + index * 16;
        let offset = file.len() as u64;
        put_u64(&mut file, entry, offset);
        put_u64(&mut file, entry + 8, section.len() as u64);
        file.extend_from_slice(section);
    }
    pad_to_alignment(&mut file);
    Ok(file)
}

/// A validated, borrowed `.mcl`.
#[derive(Debug, Clone)]
pub struct MclView<'a> {
    pub builder_identity: [u8; 32],
    pub mesh_hash: [u8; 32],
    pub primitive_index: u32,
    pub flags: u32,
    pub vertex_count: u32,
    pub cluster_count: u32,
    pub group_count: u32,
    pub bvh_node_count: u32,
    pub root_error: f32,
    pub bounds: [f32; 4],
    pub uv_min: [f32; 2],
    pub uv_scale: [f32; 2],
    sections: [&'a [u8]; MCL_SECTION_COUNT],
}

impl<'a> MclView<'a> {
    pub fn section(&self, section: MclSection) -> &'a [u8] {
        self.sections[section as usize]
    }

    pub fn position(&self, vertex: usize) -> [f32; 3] {
        let bytes = &self.section(MclSection::Positions)[vertex * 12..];
        get_f32s(bytes, 0)
    }

    pub fn normal(&self, vertex: usize) -> [i16; 2] {
        let bytes = &self.section(MclSection::Normals)[vertex * 4..];
        [get_u16(bytes, 0) as i16, get_u16(bytes, 2) as i16]
    }

    pub fn uv(&self, vertex: usize) -> [u16; 2] {
        let bytes = &self.section(MclSection::Uvs)[vertex * 4..];
        [get_u16(bytes, 0), get_u16(bytes, 2)]
    }

    pub fn cluster(&self, index: usize) -> ClusterRecord {
        ClusterRecord::from_bytes(
            &self.section(MclSection::Clusters)[index * CLUSTER_RECORD_BYTES..],
        )
    }

    pub fn group(&self, index: usize) -> GroupRecord {
        GroupRecord::from_bytes(&self.section(MclSection::Groups)[index * GROUP_RECORD_BYTES..])
    }

    pub fn bvh_node(&self, index: usize) -> BvhNode {
        BvhNode::from_bytes(&self.section(MclSection::Bvh)[index * BVH_NODE_BYTES..])
    }

    pub fn cluster_vertex(&self, index: usize) -> u32 {
        get_u32(self.section(MclSection::ClusterVertices), index * 4)
    }

    /// The local triangles of `cluster`: three indices into its own vertex list per triangle.
    pub fn cluster_triangles(&self, cluster: &ClusterRecord) -> &'a [u8] {
        let start = cluster.triangle_offset as usize * 3;
        &self.section(MclSection::ClusterTriangles)
            [start..start + cluster.triangle_count as usize * 3]
    }
}

pub fn read_mcl(bytes: &[u8]) -> Result<MclView<'_>, ClusterFormatError> {
    parse_mcl(bytes, true)
}

/// Reborrows immutable bytes already accepted by [`read_mcl`], checking the header and section
/// bounds again without repeating the per-record walk. External bytes must use [`read_mcl`].
pub fn read_mcl_validated(bytes: &[u8]) -> Result<MclView<'_>, ClusterFormatError> {
    parse_mcl(bytes, false)
}

fn parse_mcl(bytes: &[u8], check_records: bool) -> Result<MclView<'_>, ClusterFormatError> {
    if bytes.len() < MCL_HEADER_BYTES + MCL_SECTION_TABLE_BYTES {
        return Err(ClusterFormatError::Truncated("header"));
    }
    if bytes[0..4] != MCL_MAGIC {
        return Err(ClusterFormatError::BadMagic);
    }
    let version = get_u32(bytes, 4);
    if version != PAYLOAD_FORMAT_VERSION {
        return Err(ClusterFormatError::BadVersion(version));
    }
    let mut builder_identity = [0u8; 32];
    builder_identity.copy_from_slice(&bytes[8..40]);
    let mut mesh_hash = [0u8; 32];
    mesh_hash.copy_from_slice(&bytes[40..72]);
    let flags = get_u32(bytes, 76);
    let vertex_count = get_u32(bytes, 80);
    let cluster_count = get_u32(bytes, 84);
    let group_count = get_u32(bytes, 88);
    let bvh_node_count = get_u32(bytes, 92);
    if cluster_count > u32::from(u16::MAX) || group_count > u32::from(u16::MAX) {
        return Err(ClusterFormatError::TooManyClusters(
            cluster_count.max(group_count) as usize,
        ));
    }
    if flags & !(FLAG_VERTEX_COLOURS | FLAG_TANGENTS) != 0 {
        return Err(ClusterFormatError::BadRecord("unknown header flags".into()));
    }

    let vertices = vertex_count as usize;
    let expected: [usize; MCL_SECTION_COUNT] = [
        vertices * 12,
        vertices * 4,
        vertices * 4,
        if flags & FLAG_TANGENTS != 0 {
            vertices * 4
        } else {
            0
        },
        if flags & FLAG_VERTEX_COLOURS != 0 {
            vertices * 4
        } else {
            0
        },
        cluster_count as usize * CLUSTER_RECORD_BYTES,
        usize::MAX, // multiple of 4, checked below
        usize::MAX, // multiple of 3, checked below
        group_count as usize * GROUP_RECORD_BYTES,
        bvh_node_count as usize * BVH_NODE_BYTES,
    ];
    let mut sections: [&[u8]; MCL_SECTION_COUNT] = [&[]; MCL_SECTION_COUNT];
    for (index, slot) in sections.iter_mut().enumerate() {
        let entry = MCL_HEADER_BYTES + index * 16;
        let offset = get_u64(bytes, entry);
        let length = get_u64(bytes, entry + 8);
        if length == 0 {
            if expected[index] != 0 && expected[index] != usize::MAX {
                return Err(ClusterFormatError::SectionLength(index));
            }
            continue;
        }
        if offset < (MCL_HEADER_BYTES + MCL_SECTION_TABLE_BYTES) as u64 {
            return Err(ClusterFormatError::SectionOutOfBounds(index));
        }
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= bytes.len() as u64)
            .ok_or(ClusterFormatError::SectionOutOfBounds(index))?;
        if !offset.is_multiple_of(SECTION_ALIGNMENT as u64) {
            return Err(ClusterFormatError::SectionMisaligned(index));
        }
        let length = length as usize;
        let length_ok = match index {
            6 => length.is_multiple_of(4),
            7 => length.is_multiple_of(3),
            _ => length == expected[index],
        };
        if !length_ok {
            return Err(ClusterFormatError::SectionLength(index));
        }
        *slot = &bytes[offset as usize..end as usize];
    }
    if vertices > 0 && sections[0].is_empty() {
        return Err(ClusterFormatError::SectionLength(0));
    }

    let header_floats: [f32; 9] = [
        get_f32(bytes, 96),
        get_f32(bytes, 100),
        get_f32(bytes, 104),
        get_f32(bytes, 108),
        get_f32(bytes, 112),
        get_f32(bytes, 116),
        get_f32(bytes, 120),
        get_f32(bytes, 124),
        get_f32(bytes, 128),
    ];
    if header_floats.iter().any(|value| !value.is_finite())
        || get_f32(bytes, 112) < 0.0
        || get_f32(bytes, 124) < 0.0
        || get_f32(bytes, 128) < 0.0
    {
        return Err(ClusterFormatError::BadRecord(
            "header bounds, error or uv range is not finite and non-negative".into(),
        ));
    }

    let view = MclView {
        builder_identity,
        mesh_hash,
        primitive_index: get_u32(bytes, 72),
        flags,
        vertex_count,
        cluster_count,
        group_count,
        bvh_node_count,
        root_error: get_f32(bytes, 96),
        bounds: get_f32s(bytes, 100),
        uv_min: get_f32s(bytes, 116),
        uv_scale: get_f32s(bytes, 124),
        sections,
    };
    if check_records {
        validate_records(&view)?;
    }
    Ok(view)
}

fn validate_records(view: &MclView<'_>) -> Result<(), ClusterFormatError> {
    let bad = |why: String| Err(ClusterFormatError::BadRecord(why));
    let cluster_vertex_total = view.section(MclSection::ClusterVertices).len() / 4;
    let triangle_total = view.section(MclSection::ClusterTriangles).len() / 3;

    for index in 0..view.group_count as usize {
        let group = view.group(index);
        let end = usize::from(group.first_cluster) + usize::from(group.cluster_count);
        if end > view.cluster_count as usize {
            return bad(format!(
                "group {index} cluster range exceeds {}",
                view.cluster_count
            ));
        }
        if group.error.is_nan() || group.bounds.iter().any(|v| !v.is_finite()) {
            return bad(format!(
                "group {index} has a NaN error or non-finite bounds"
            ));
        }
    }
    let mut max_child_error = vec![0.0f32; view.group_count as usize];
    for index in 0..view.cluster_count as usize {
        let cluster = view.cluster(index);
        let (offset, count) = (
            cluster.vertex_offset as usize,
            usize::from(cluster.vertex_count),
        );
        if offset + count > cluster_vertex_total {
            return bad(format!("cluster {index} vertex range exceeds the section"));
        }
        let (toffset, tcount) = (
            cluster.triangle_offset as usize,
            usize::from(cluster.triangle_count),
        );
        if toffset + tcount > triangle_total {
            return bad(format!(
                "cluster {index} triangle range exceeds the section"
            ));
        }
        for slot in offset..offset + count {
            if view.cluster_vertex(slot) >= view.vertex_count {
                return bad(format!(
                    "cluster {index} references vertex {} of {}",
                    view.cluster_vertex(slot),
                    view.vertex_count
                ));
            }
        }
        if view
            .cluster_triangles(&cluster)
            .iter()
            .any(|&local| usize::from(local) >= count)
        {
            return bad(format!(
                "cluster {index} has a local triangle index beyond its {count} vertices"
            ));
        }
        if u32::from(cluster.group) >= view.group_count {
            return bad(format!(
                "cluster {index} group {} of {}",
                cluster.group, view.group_count
            ));
        }
        if cluster.refined_group != NO_GROUP && u32::from(cluster.refined_group) >= view.group_count
        {
            return bad(format!(
                "cluster {index} refined group {} of {}",
                cluster.refined_group, view.group_count
            ));
        }
        if cluster.error.is_nan()
            || cluster.parent_error.is_nan()
            || cluster.bounds.iter().any(|v| !v.is_finite())
        {
            return bad(format!("cluster {index} has non-finite bounds or errors"));
        }
        if cluster.error > cluster.parent_error {
            return Err(ClusterFormatError::NonMonotonicError(format!(
                "cluster {index} error {} exceeds its parent error {}",
                cluster.error, cluster.parent_error
            )));
        }
        let parent = view.group(usize::from(cluster.group));
        let first = usize::from(parent.first_cluster);
        if index < first || index >= first + usize::from(parent.cluster_count) {
            return bad(format!(
                "cluster {index} lies outside the cluster range of its group {}",
                cluster.group
            ));
        }
        if parent.error != cluster.parent_error {
            return Err(ClusterFormatError::NonMonotonicError(format!(
                "cluster {index} parent error differs from group {}",
                cluster.group
            )));
        }
        let slot = &mut max_child_error[usize::from(cluster.group)];
        *slot = slot.max(cluster.error);
        if cluster.refined_group != NO_GROUP {
            let producer = view.group(usize::from(cluster.refined_group));
            if producer.error != cluster.error {
                return Err(ClusterFormatError::NonMonotonicError(format!(
                    "cluster {index} error differs from producing group {}",
                    cluster.refined_group
                )));
            }
        } else if cluster.error != 0.0 {
            return Err(ClusterFormatError::NonMonotonicError(format!(
                "source cluster {index} has error {}",
                cluster.error
            )));
        }
    }
    for (index, child_error) in max_child_error.iter().enumerate() {
        if view.group(index).error < *child_error {
            return Err(ClusterFormatError::NonMonotonicError(format!(
                "group {index} error is below an input cluster error {child_error}"
            )));
        }
    }
    for index in 0..view.bvh_node_count as usize {
        let node = view.bvh_node(index);
        if node.error.is_nan() || node.bounds.iter().any(|v| !v.is_finite()) {
            return bad(format!(
                "bvh node {index} has a NaN error or non-finite bounds"
            ));
        }
        if node.group != NO_GROUP {
            if u32::from(node.group) >= view.group_count || node.child_count != 0 {
                return bad(format!("bvh leaf {index} has a bad group or children"));
            }
        } else if node.first_child as usize + usize::from(node.child_count)
            > view.bvh_node_count as usize
        {
            return bad(format!("bvh node {index} children exceed the node count"));
        }
    }
    // A walk from any node must end: no node may be reached again below itself.
    let count = view.bvh_node_count as usize;
    let mut state = vec![0u8; count];
    for start in 0..count {
        if state[start] != 0 {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        state[start] = 1;
        while let Some((node_index, next)) = stack.last_mut() {
            let node_index = *node_index;
            let node = view.bvh_node(node_index);
            let children = if node.group == NO_GROUP {
                node.first_child as usize..node.first_child as usize + usize::from(node.child_count)
            } else {
                0..0
            };
            let child = children.start + *next;
            if child >= children.end {
                state[node_index] = 2;
                stack.pop();
                continue;
            }
            *next += 1;
            match state[child] {
                1 => return bad(format!("bvh node {child} is its own descendant")),
                0 => {
                    state[child] = 1;
                    stack.push((child, 0));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

// Chunk instance payload ----------------------------------------------------------------------

/// One mesh table entry of an `.mci`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MeshRef {
    pub mesh_hash: [u8; 32],
    pub primitive_index: u32,
}

/// One instance before the writer sorts and indexes it.
#[derive(Debug, Clone, Copy)]
pub struct InstanceInput {
    pub mesh: MeshRef,
    pub transform: [f32; 12],
    pub cell_x: i32,
    pub cell_y: i32,
    pub stable_id: [u8; 16],
    /// The base object's form id and snow material, copied into the record (see
    /// [`InstanceRecord`]).
    pub base_form_id: u32,
    pub snow_material: u32,
    /// The child worldspace the object is placed in; 0 for the block's own worldspace.
    pub source_worldspace_id: u32,
    /// World-space bounds of this instance, folded into the block bounds.
    pub world_min: [f32; 3],
    pub world_max: [f32; 3],
}

#[derive(Debug, Clone)]
pub struct MciInputs<'a> {
    pub builder_identity: [u8; 32],
    pub worldspace_id: u32,
    pub tier: u32,
    pub anchor_x: i32,
    pub anchor_y: i32,
    pub instances: &'a [InstanceInput],
}

/// Sorts the inputs canonically (mesh table by (hash, primitive), instances by
/// (cell_y, cell_x, mesh, stable_id, transform)) and writes the block.
pub fn write_mci(inputs: &MciInputs<'_>) -> Result<Vec<u8>, ClusterFormatError> {
    write_mci_ordered(inputs).map(|(bytes, _)| bytes)
}

/// [`write_mci`], also returning the writer's real sort order: element `n` is the index into
/// `inputs.instances` of the record written at ordinal `n`. Instances equal on the whole sort key
/// keep input order (the sort is stable), so only the writer can say which is which; a mapping
/// from records to the things they came from is built from this, never guessed afterwards.
pub fn write_mci_ordered(
    inputs: &MciInputs<'_>,
) -> Result<(Vec<u8>, Vec<usize>), ClusterFormatError> {
    let mut meshes: Vec<MeshRef> = inputs.instances.iter().map(|i| i.mesh).collect();
    meshes.sort();
    meshes.dedup();
    let mut order: Vec<usize> = (0..inputs.instances.len()).collect();
    let mesh_index = |mesh: &MeshRef| {
        meshes
            .binary_search(mesh)
            .expect("mesh table holds every mesh") as u32
    };
    order.sort_by(|&a, &b| {
        let (a, b) = (&inputs.instances[a], &inputs.instances[b]);
        let key = |i: &InstanceInput| (i.cell_y, i.cell_x, mesh_index(&i.mesh), i.stable_id);
        key(a).cmp(&key(b)).then_with(|| {
            a.transform
                .map(f32::to_bits)
                .cmp(&b.transform.map(f32::to_bits))
        })
    });

    let (mut bounds_min, mut bounds_max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for instance in inputs.instances {
        for axis in 0..3 {
            bounds_min[axis] = bounds_min[axis].min(instance.world_min[axis]);
            bounds_max[axis] = bounds_max[axis].max(instance.world_max[axis]);
        }
    }
    if inputs.instances.is_empty() {
        bounds_min = [0.0; 3];
        bounds_max = [0.0; 3];
    }

    let mut file = vec![0u8; MCI_HEADER_BYTES];
    file[0..4].copy_from_slice(&MCI_MAGIC);
    put_u32(&mut file, 4, PAYLOAD_FORMAT_VERSION);
    file[8..40].copy_from_slice(&inputs.builder_identity);
    put_u32(&mut file, 40, inputs.worldspace_id);
    put_u32(&mut file, 44, inputs.tier);
    put_u32(&mut file, 48, inputs.anchor_x as u32);
    put_u32(&mut file, 52, inputs.anchor_y as u32);
    put_u32(&mut file, 56, inputs.instances.len() as u32);
    put_u32(&mut file, 60, meshes.len() as u32);
    put_f32s(&mut file, 64, &bounds_min);
    put_f32s(&mut file, 76, &bounds_max);
    for mesh in &meshes {
        let mut entry = [0u8; MCI_MESH_ENTRY_BYTES];
        entry[0..32].copy_from_slice(&mesh.mesh_hash);
        put_u32(&mut entry, 32, mesh.primitive_index);
        file.extend_from_slice(&entry);
    }
    pad_to_alignment(&mut file);
    for &position in &order {
        let instance = &inputs.instances[position];
        file.extend_from_slice(
            &InstanceRecord {
                transform: instance.transform,
                mesh: mesh_index(&instance.mesh),
                cell_x: instance.cell_x,
                cell_y: instance.cell_y,
                stable_id: instance.stable_id,
                flags: if instance.source_worldspace_id != 0 {
                    INSTANCE_FLAG_CHILD_WORLDSPACE
                } else {
                    0
                },
                base_form_id: instance.base_form_id,
                snow_material: instance.snow_material,
                source_worldspace_id: instance.source_worldspace_id,
            }
            .to_bytes(),
        );
    }
    pad_to_alignment(&mut file);
    Ok((file, order))
}

/// A validated, borrowed `.mci`.
#[derive(Debug, Clone)]
pub struct MciView<'a> {
    pub builder_identity: [u8; 32],
    pub worldspace_id: u32,
    pub tier: u32,
    pub anchor_x: i32,
    pub anchor_y: i32,
    pub instance_count: u32,
    pub mesh_count: u32,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    mesh_table: &'a [u8],
    instances: &'a [u8],
}

impl MciView<'_> {
    pub fn mesh(&self, index: usize) -> MeshRef {
        let entry = &self.mesh_table[index * MCI_MESH_ENTRY_BYTES..];
        let mut mesh_hash = [0u8; 32];
        mesh_hash.copy_from_slice(&entry[0..32]);
        MeshRef {
            mesh_hash,
            primitive_index: get_u32(entry, 32),
        }
    }

    pub fn instance(&self, index: usize) -> InstanceRecord {
        InstanceRecord::from_bytes(&self.instances[index * INSTANCE_RECORD_BYTES..])
    }
}

pub fn read_mci(bytes: &[u8]) -> Result<MciView<'_>, ClusterFormatError> {
    if bytes.len() < MCI_HEADER_BYTES {
        return Err(ClusterFormatError::Truncated("header"));
    }
    if bytes[0..4] != MCI_MAGIC {
        return Err(ClusterFormatError::BadMagic);
    }
    let version = get_u32(bytes, 4);
    if version != PAYLOAD_FORMAT_VERSION {
        return Err(ClusterFormatError::BadVersion(version));
    }
    let mut builder_identity = [0u8; 32];
    builder_identity.copy_from_slice(&bytes[8..40]);
    let instance_count = get_u32(bytes, 56);
    let mesh_count = get_u32(bytes, 60);

    let table_end = MCI_HEADER_BYTES
        .checked_add(mesh_count as usize * MCI_MESH_ENTRY_BYTES)
        .ok_or(ClusterFormatError::SectionOutOfBounds(0))?;
    let instances_start = table_end.next_multiple_of(SECTION_ALIGNMENT);
    let instances_end = instances_start
        .checked_add(instance_count as usize * INSTANCE_RECORD_BYTES)
        .ok_or(ClusterFormatError::SectionOutOfBounds(1))?;
    if table_end > bytes.len() {
        return Err(ClusterFormatError::SectionOutOfBounds(0));
    }
    if instances_end > bytes.len() && instance_count > 0 {
        return Err(ClusterFormatError::SectionOutOfBounds(1));
    }
    let view = MciView {
        builder_identity,
        worldspace_id: get_u32(bytes, 40),
        tier: get_u32(bytes, 44),
        anchor_x: get_u32(bytes, 48) as i32,
        anchor_y: get_u32(bytes, 52) as i32,
        instance_count,
        mesh_count,
        bounds_min: get_f32s(bytes, 64),
        bounds_max: get_f32s(bytes, 76),
        mesh_table: &bytes[MCI_HEADER_BYTES..table_end],
        instances: if instance_count > 0 {
            &bytes[instances_start..instances_end]
        } else {
            &[]
        },
    };
    if view
        .bounds_min
        .iter()
        .chain(&view.bounds_max)
        .any(|v| !v.is_finite())
        || view
            .bounds_min
            .iter()
            .zip(&view.bounds_max)
            .any(|(min, max)| min > max)
    {
        return Err(ClusterFormatError::BadRecord(
            "block bounds are not finite and ordered".into(),
        ));
    }
    for index in 1..mesh_count as usize {
        if view.mesh(index - 1) >= view.mesh(index) {
            return Err(ClusterFormatError::Unsorted("mesh table"));
        }
    }
    let mut previous: Option<(i32, i32, u32, [u8; 16])> = None;
    for index in 0..instance_count as usize {
        let instance = view.instance(index);
        if view.instances[index * INSTANCE_RECORD_BYTES + 92..(index + 1) * INSTANCE_RECORD_BYTES]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(ClusterFormatError::BadRecord(format!(
                "instance {index} has a non-zero reserved field"
            )));
        }
        if instance.flags & !INSTANCE_FLAG_CHILD_WORLDSPACE != 0
            || (instance.flags & INSTANCE_FLAG_CHILD_WORLDSPACE != 0)
                != (instance.source_worldspace_id != 0)
        {
            return Err(ClusterFormatError::BadRecord(format!(
                "instance {index} flags disagree with its source worldspace"
            )));
        }
        if instance.mesh >= mesh_count {
            return Err(ClusterFormatError::BadRecord(format!(
                "instance {index} mesh {} of {mesh_count}",
                instance.mesh
            )));
        }
        if instance.transform.iter().any(|v| !v.is_finite()) {
            return Err(ClusterFormatError::BadRecord(format!(
                "instance {index} has a non-finite transform"
            )));
        }
        let key = (
            instance.cell_y,
            instance.cell_x,
            instance.mesh,
            instance.stable_id,
        );
        if previous.is_some_and(|before| before > key) {
            return Err(ClusterFormatError::Unsorted("instances"));
        }
        previous = Some(key);
    }
    Ok(view)
}

// Database tables -----------------------------------------------------------------------------

/// The five optional cluster tables (ADR-0013 section 3). Additive: a package without them is a
/// complete package, and the world database schema version does not change.
pub const CLUSTER_TABLES_DDL: &str = "
CREATE TABLE IF NOT EXISTS cluster_build (
    id INTEGER PRIMARY KEY CHECK (id = 1), builder_identity TEXT NOT NULL,
    triangle_threshold INTEGER NOT NULL, block_side_cells INTEGER NOT NULL,
    lod_mode TEXT NOT NULL CHECK (lod_mode = 'cluster')
);
CREATE TABLE IF NOT EXISTS cluster_meshes (
    mesh_hash TEXT NOT NULL, primitive_index INTEGER NOT NULL,
    payload_path TEXT NOT NULL, content_hash TEXT NOT NULL,
    source_triangles INTEGER NOT NULL, cluster_count INTEGER NOT NULL,
    vertex_count INTEGER NOT NULL, payload_bytes INTEGER NOT NULL,
    material_key TEXT NOT NULL,
    model_path TEXT NOT NULL DEFAULT '', material_index INTEGER,
    alpha_mode TEXT NOT NULL DEFAULT 'opaque', alpha_cutoff REAL,
    double_sided INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (mesh_hash, primitive_index)
);
CREATE TABLE IF NOT EXISTS cluster_chunks (
    worldspace_id INTEGER NOT NULL, tier INTEGER NOT NULL,
    anchor_x INTEGER NOT NULL, anchor_y INTEGER NOT NULL,
    payload_path TEXT NOT NULL, content_hash TEXT NOT NULL, input_key TEXT NOT NULL,
    bounds_min_x REAL NOT NULL, bounds_min_y REAL NOT NULL, bounds_min_z REAL NOT NULL,
    bounds_max_x REAL NOT NULL, bounds_max_y REAL NOT NULL, bounds_max_z REAL NOT NULL,
    instance_count INTEGER NOT NULL, source_cells TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (worldspace_id, tier, anchor_x, anchor_y)
);
CREATE VIRTUAL TABLE IF NOT EXISTS cluster_chunks_spatial USING rtree(
    id, minX, maxX, minY, maxY, +worldspace_id, +tier, +anchor_x, +anchor_y
);
CREATE TABLE IF NOT EXISTS cluster_rejects (
    mesh_hash TEXT NOT NULL, primitive_index INTEGER NOT NULL,
    reason TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (mesh_hash, primitive_index)
);
";

// Byte helpers --------------------------------------------------------------------------------

fn pad_to_alignment(file: &mut Vec<u8>) {
    file.resize(file.len().next_multiple_of(SECTION_ALIGNMENT), 0);
}

fn put_u16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut [u8], at: usize, value: u64) {
    out[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_f32s(out: &mut [u8], at: usize, values: &[f32]) {
    for (index, value) in values.iter().enumerate() {
        out[at + index * 4..at + index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
}

fn get_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().expect("two bytes"))
}

fn get_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn get_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

fn get_f32(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn get_f32s<const N: usize>(bytes: &[u8], at: usize) -> [f32; N] {
    std::array::from_fn(|index| get_f32(bytes, at + index * 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_hex_matches_original_formatting_for_every_byte() {
        let bytes: Vec<u8> = (0..=255).collect();
        for bytes in [&[][..], &bytes[..1], bytes.as_slice()] {
            let original: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(hex(bytes), original);
        }
    }

    fn identity_inputs() -> BuilderIdentityInputs {
        BuilderIdentityInputs {
            clusterlod_source: "a".repeat(64),
            meshoptimizer: "b".repeat(64),
            builder_config: json!({"preset": "classic", "max_triangles": 128}),
            triangle_threshold: 1024,
            converter_schema: 24,
        }
    }

    /// A two-level DAG: two source clusters, one group that simplifies them into one terminal cluster.
    fn sample() -> (Vec<ClusterRecord>, Vec<GroupRecord>, Vec<u32>, Vec<[u8; 3]>) {
        let clusters = vec![
            ClusterRecord {
                bounds: [0.0, 0.0, 0.0, 1.0],
                error: 0.0,
                parent_error: 0.5,
                vertex_offset: 0,
                triangle_offset: 0,
                vertex_count: 3,
                triangle_count: 1,
                group: 0,
                refined_group: NO_GROUP,
            },
            ClusterRecord {
                bounds: [1.0, 0.0, 0.0, 1.0],
                error: 0.0,
                parent_error: 0.5,
                vertex_offset: 3,
                triangle_offset: 1,
                vertex_count: 3,
                triangle_count: 1,
                group: 0,
                refined_group: NO_GROUP,
            },
            ClusterRecord {
                bounds: [0.5, 0.0, 0.0, 2.0],
                error: 0.5,
                parent_error: TERMINAL_ERROR,
                vertex_offset: 6,
                triangle_offset: 2,
                vertex_count: 3,
                triangle_count: 1,
                group: 1,
                refined_group: 0,
            },
        ];
        let groups = vec![
            GroupRecord {
                bounds: [0.5, 0.0, 0.0, 2.0],
                error: 0.5,
                depth: 0,
                first_cluster: 0,
                cluster_count: 2,
            },
            GroupRecord {
                bounds: [0.5, 0.0, 0.0, 2.0],
                error: TERMINAL_ERROR,
                depth: 1,
                first_cluster: 2,
                cluster_count: 1,
            },
        ];
        (
            clusters,
            groups,
            vec![0, 1, 2, 1, 2, 3, 0, 1, 3],
            vec![[0, 1, 2]; 3],
        )
    }

    fn write_sample(mutate: impl FnOnce(&mut Vec<ClusterRecord>)) -> Vec<u8> {
        let (mut clusters, groups, cluster_vertices, triangles) = sample();
        mutate(&mut clusters);
        let positions = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ];
        let normals = [encode_oct_snorm16([0.0, 0.0, 1.0]); 4];
        let uvs = [[0u16, 0], [65535, 0], [0, 65535], [65535, 65535]];
        write_mcl(&MclContents {
            builder_identity: [1; 32],
            mesh_hash: [2; 32],
            primitive_index: 3,
            root_error: 0.5,
            bounds: [0.5, 0.5, 0.0, 1.0],
            uv_min: [0.0; 2],
            uv_scale: [1.0; 2],
            positions: &positions,
            normals: &normals,
            uvs: &uvs,
            tangents: None,
            colours: None,
            clusters: &clusters,
            cluster_vertices: &cluster_vertices,
            cluster_triangles: &triangles,
            groups: &groups,
            bvh: &[],
        })
        .unwrap()
    }

    #[test]
    fn record_sizes_match_the_adr() {
        assert_eq!(
            ClusterRecord::from_bytes(&sample().0[0].to_bytes()),
            sample().0[0]
        );
        assert_eq!(
            GroupRecord::from_bytes(&sample().1[0].to_bytes()),
            sample().1[0]
        );
        let node = BvhNode {
            bounds: [1.0, 2.0, 3.0, 4.0],
            error: 5.0,
            first_child: 6,
            child_count: 7,
            group: 8,
        };
        assert_eq!(BvhNode::from_bytes(&node.to_bytes()), node);
        let instance = InstanceRecord {
            transform: [1.0; 12],
            mesh: 2,
            cell_x: -3,
            cell_y: 4,
            stable_id: [9; 16],
            flags: 0,
            base_form_id: 0x1A2B3,
            snow_material: 0x25129,
            source_worldspace_id: 0x1A22F,
        };
        assert_eq!(InstanceRecord::from_bytes(&instance.to_bytes()), instance);
        assert_eq!(
            CLUSTER_RECORD_BYTES + GROUP_RECORD_BYTES + BVH_NODE_BYTES,
            112
        );
        assert_eq!(INSTANCE_RECORD_BYTES, 96);
    }

    #[test]
    fn mcl_round_trips_and_is_deterministic() {
        let bytes = write_sample(|_| {});
        assert_eq!(bytes, write_sample(|_| {}));
        assert_eq!(bytes.len() % SECTION_ALIGNMENT, 0);
        let view = read_mcl(&bytes).unwrap();
        assert_eq!(
            (view.vertex_count, view.cluster_count, view.group_count),
            (4, 3, 2)
        );
        assert_eq!(view.primitive_index, 3);
        assert_eq!(view.cluster(2), sample().0[2]);
        assert_eq!(view.position(3), [1.0, 1.0, 0.0]);
        assert_eq!(view.cluster_triangles(&view.cluster(1)), &[0, 1, 2]);
        assert_eq!(view.cluster_vertex(8), 3);
    }

    #[test]
    fn mcl_rejects_damage() {
        let good = write_sample(|_| {});
        assert_eq!(read_mcl(&good[..100]).unwrap_err().reason(), "truncated");
        let mut bad = good.clone();
        bad[0] = b'X';
        assert_eq!(read_mcl(&bad).unwrap_err().reason(), "bad_magic");
        let mut bad = good.clone();
        bad[4] = 9;
        assert_eq!(read_mcl(&bad).unwrap_err().reason(), "bad_version");
        assert_eq!(
            read_mcl(&good[..good.len() - 256 * 2])
                .unwrap_err()
                .reason(),
            "section_out_of_bounds"
        );
        let mut bad = good.clone();
        put_u64(&mut bad, MCL_HEADER_BYTES + 5, 12345);
        assert!(read_mcl(&bad).is_err());
        let error = read_mcl(&write_sample(|clusters| clusters[2].vertex_offset = 8)).unwrap_err();
        assert_eq!(error.reason(), "bad_record");
        let error = read_mcl(&write_sample(|clusters| clusters[0].error = 0.9)).unwrap_err();
        assert_eq!(error.reason(), "non_monotonic_error");
        let error = read_mcl(&write_sample(|clusters| clusters[1].group = 7)).unwrap_err();
        assert_eq!(error.reason(), "bad_record");
    }

    #[test]
    fn mcl_bvh_children_may_precede_parents_but_not_loop() {
        let (clusters, groups, cluster_vertices, triangles) = sample();
        let positions = [[0.0; 3]; 4];
        let normals = [[0i16; 2]; 4];
        let uvs = [[0u16; 2]; 4];
        let leaf = |group| BvhNode {
            bounds: [0.0; 4],
            error: 0.5,
            first_child: 0,
            child_count: 0,
            group,
        };
        let inner = |first_child, child_count| BvhNode {
            bounds: [0.0; 4],
            error: 0.5,
            first_child,
            child_count,
            group: NO_GROUP,
        };
        let write = |bvh: &[BvhNode]| {
            write_mcl(&MclContents {
                builder_identity: [1; 32],
                mesh_hash: [2; 32],
                primitive_index: 0,
                root_error: 0.5,
                bounds: [0.0; 4],
                uv_min: [0.0; 2],
                uv_scale: [1.0; 2],
                positions: &positions,
                normals: &normals,
                uvs: &uvs,
                tangents: None,
                colours: None,
                clusters: &clusters,
                cluster_vertices: &cluster_vertices,
                cluster_triangles: &triangles,
                groups: &groups,
                bvh,
            })
            .unwrap()
        };
        // Root last, leaves first: allowed.
        assert!(read_mcl(&write(&[leaf(0), leaf(1), inner(0, 2)])).is_ok());
        // A node under itself is not.
        let looped = write(&[inner(0, 1)]);
        assert_eq!(read_mcl(&looped).unwrap_err().reason(), "bad_record");
        let cycle = write(&[inner(1, 1), inner(0, 1)]);
        assert_eq!(read_mcl(&cycle).unwrap_err().reason(), "bad_record");
    }

    #[test]
    fn mci_sorts_and_round_trips() {
        let mesh = |byte: u8| MeshRef {
            mesh_hash: [byte; 32],
            primitive_index: 0,
        };
        let instance = |mesh_ref, cell_x, cell_y, x: f32| InstanceInput {
            mesh: mesh_ref,
            transform: [1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            cell_x,
            cell_y,
            stable_id: [0; 16],
            base_form_id: 0,
            snow_material: 0,
            source_worldspace_id: 0,
            world_min: [x - 1.0, -1.0, -1.0],
            world_max: [x + 1.0, 1.0, 1.0],
        };
        let forward = [
            instance(mesh(9), 1, 0, 5.0),
            instance(mesh(3), 0, 0, 1.0),
            instance(mesh(9), 0, 1, 7.0),
        ];
        let mut reversed = forward;
        reversed.reverse();
        let make = |instances: &[InstanceInput]| {
            write_mci(&MciInputs {
                builder_identity: [4; 32],
                worldspace_id: 0x3c,
                tier: 16,
                anchor_x: -16,
                anchor_y: 32,
                instances,
            })
            .unwrap()
        };
        let bytes = make(&forward);
        assert_eq!(bytes, make(&reversed));
        let view = read_mci(&bytes).unwrap();
        assert_eq!(
            (view.instance_count, view.mesh_count, view.anchor_x),
            (3, 2, -16)
        );
        assert_eq!(view.mesh(0), mesh(3));
        assert_eq!(view.instance(0).mesh, 0);
        assert_eq!((view.instance(2).cell_x, view.instance(2).cell_y), (0, 1));
        assert_eq!(view.bounds_min, [0.0, -1.0, -1.0]);
        assert_eq!(view.bounds_max, [8.0, 1.0, 1.0]);
        assert!(read_mci(&bytes[..MCI_HEADER_BYTES + 10]).is_err());
        let empty = make(&[]);
        assert_eq!(read_mci(&empty).unwrap().instance_count, 0);
    }

    #[test]
    fn builder_identity_follows_every_input() {
        let base = identity_inputs();
        assert_eq!(base.identity(), identity_inputs().identity());
        let mut changed = identity_inputs();
        changed.clusterlod_source = "c".repeat(64);
        assert_ne!(base.identity(), changed.identity());
        let mut changed = identity_inputs();
        changed.triangle_threshold = 512;
        assert_ne!(base.identity(), changed.identity());
        let mut changed = identity_inputs();
        changed.builder_config = json!({"preset": "rt", "max_triangles": 128});
        assert_ne!(base.identity(), changed.identity());
        let text = String::from_utf8(base.canonical_json()).unwrap();
        assert!(text.starts_with("{\"block_side_cells\":16,\"builder_config\""));
        assert_eq!(parse_hash(&base.identity_hex()), Some(base.identity()));
    }

    #[test]
    fn oct_and_uv_encodings_round_trip() {
        let samples: [[f32; 3]; 5] = [
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
            [1.0, 0.0, 0.0],
            [0.3, -0.5, -0.8],
            [-0.6, 0.6, 0.1],
        ];
        for v in samples {
            let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let unit = [v[0] / length, v[1] / length, v[2] / length];
            let back = decode_oct_snorm16(encode_oct_snorm16(unit));
            let dot = unit[0] * back[0] + unit[1] * back[1] + unit[2] * back[2];
            assert!(dot > 0.9999, "{unit:?} came back as {back:?}");
        }
        let q = encode_uv_unorm16([5.25, -3.0], [4.0, -4.0], [4.0, 2.0]);
        let back = decode_uv_unorm16(q, [4.0, -4.0], [4.0, 2.0]);
        assert!((back[0] - 5.25).abs() < 1e-3 && (back[1] + 3.0).abs() < 1e-3);
    }

    #[test]
    fn paths_follow_the_adr() {
        assert_eq!(
            mesh_payload_path("ab12", 3),
            "clusters/meshes/ab/ab12-p3.mcl"
        );
        assert_eq!(
            instance_payload_path(0x3c, 16, -16, 32),
            "clusters/instances/0000003c/16/cell_-16_32.mci"
        );
    }
}

#[cfg(test)]
mod committed_fixture_pins {
    use super::*;

    #[test]
    fn committed_payloads_validate_and_match_the_recorded_sizes_and_hashes() {
        let expected: Value =
            serde_json::from_str(include_str!("../tests/data/cluster/expected.json")).unwrap();
        let mesh = include_bytes!("../tests/data/cluster/fixture.mcl");
        let instances = include_bytes!("../tests/data/cluster/fixture.mci");
        read_mcl(mesh).unwrap();
        read_mci(instances).unwrap();
        for (kind, bytes) in [("mcl", mesh.as_slice()), ("mci", instances.as_slice())] {
            assert_eq!(
                bytes.len() as u64,
                expected[format!("{kind}_bytes")].as_u64().unwrap()
            );
            assert_eq!(
                hex(&Sha256::digest(bytes)),
                expected[format!("{kind}_sha256")].as_str().unwrap()
            );
        }
    }
}
