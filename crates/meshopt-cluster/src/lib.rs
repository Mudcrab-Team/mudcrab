//! Cluster LOD hierarchy built with meshoptimizer's `demo/clusterlod.h` (MIT, v1.3).
//!
//! The C++ code is compiled by `build.rs` from `vendor/meshoptimizer` and called through
//! `cpp/mlc_wrapper.cpp`. That wrapper copies groups, clusters and indices into plain arrays,
//! so no callback crosses the FFI boundary. Every input is checked here before it reaches C++,
//! because meshoptimizer's own asserts are compiled out in release builds.

mod ffi;

use std::fmt;
use std::ops::Range;
use std::ptr::NonNull;

pub const MIN_MAX_TRIANGLES: usize = 4;
pub const MAX_MAX_TRIANGLES: usize = 256;

// meshopt_simplifyEdge limits, in vendor/meshoptimizer/src/simplifier.cpp.
const MAX_ATTRIBUTE_COUNT: usize = 32;
const MAX_ATTRIBUTE_STRIDE_BYTES: usize = 256;

/// Which clusterlod default configuration to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// `clodDefaultConfig`: tuned for rasterization.
    Classic,
    /// `clodDefaultConfigRT`: spatial clustering and larger vertex budget, tuned for ray tracing.
    Rt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub preset: Preset,
    /// Triangles per cluster, 4 to 256.
    pub max_triangles: usize,
    /// Coarser levels above the root: while the single root cluster of a mesh has more than this
    /// many triangles, the build adds a group that simplifies it to about half (each with its own
    /// error and a one-cluster group above it), stopping at this size or when simplification gets
    /// stuck. 0 adds none. Without them every instance of a mesh costs a whole root cluster
    /// (up to `max_triangles`) however small it is on screen.
    pub tail_min_triangles: usize,
}

impl Config {
    pub const fn classic(max_triangles: usize) -> Self {
        Self {
            preset: Preset::Classic,
            max_triangles,
            tail_min_triangles: 0,
        }
    }

    pub const fn rt(max_triangles: usize) -> Self {
        Self {
            preset: Preset::Rt,
            max_triangles,
            tail_min_triangles: 0,
        }
    }

    /// The same config with coarser root levels down to `min_triangles` (see
    /// [`Config::tail_min_triangles`]).
    pub const fn with_tail(mut self, min_triangles: usize) -> Self {
        self.tail_min_triangles = min_triangles;
        self
    }
}

/// Per-vertex attributes for attribute-aware simplification.
#[derive(Clone, Copy, Debug)]
pub struct Attributes<'a> {
    /// `stride` floats per vertex, `positions.len() * stride` values in total.
    pub data: &'a [f32],
    /// Floats per vertex, at most 64 (256 bytes).
    pub stride: usize,
    /// At most 32 finite, nonnegative weights, with `weights.len() <= stride`.
    pub weights: &'a [f32],
    /// Bit `k` marks attribute `k` as a seam: vertices that differ in it are never merged.
    /// Only the first 32 floats can be protected, including when the stride is larger.
    pub protect_mask: u32,
}

/// Input triangle mesh. Vertices are referenced by index into `positions`.
#[derive(Clone, Copy, Debug)]
pub struct Mesh<'a> {
    pub positions: &'a [[f32; 3]],
    /// Triangle list, a multiple of three.
    pub indices: &'a [u32],
    pub attributes: Option<Attributes<'a>>,
    /// One byte per vertex (same length as `positions`); a non-zero byte keeps the vertex where it is
    /// at every level, so a border shared with another mesh stays identical there. `None` locks nothing.
    pub vertex_lock: Option<&'a [u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub center: [f32; 3],
    pub radius: f32,
    /// Combined simplification error. `f32::MAX` for terminal groups, which never simplify further.
    pub error: f32,
}

/// A group is one merge-and-simplify step of the DAG. Its clusters are the inputs it merged.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// DAG level, 0 for the source geometry.
    pub depth: u32,
    /// Simplified bounds of the group.
    pub bounds: Bounds,
    /// Range of this group's clusters in `Hierarchy::clusters()`.
    pub clusters: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    /// Index of the group that simplified this cluster into existence, or `None` for source geometry.
    pub refined: Option<usize>,
    /// Culling bounds. `error` is not monotonic across the DAG; use it only for culling.
    pub bounds: Bounds,
    /// Unique vertices referenced by `indices()`.
    pub vertex_count: usize,
    indices: Box<[u32]>,
}

impl Cluster {
    /// Global vertex indices, three per triangle.
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Meshlet-local form: `vertices[triangles[i]] == indices()[i]`.
    pub fn local_geometry(&self) -> LocalGeometry {
        let count = self.indices.len();
        if count == 0 {
            return LocalGeometry::default();
        }

        let mut vertices = vec![0u32; count];
        let mut triangles = vec![0u8; count];
        // SAFETY: both output buffers hold `count` entries and the input is `count` indices.
        let unique = unsafe {
            ffi::mlc_local_indices(
                vertices.as_mut_ptr(),
                triangles.as_mut_ptr(),
                self.indices.as_ptr(),
                count,
            )
        };
        vertices.truncate(unique);
        LocalGeometry {
            vertices,
            triangles,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalGeometry {
    pub vertices: Vec<u32>,
    pub triangles: Vec<u8>,
}

/// The cluster DAG for one mesh.
#[derive(Clone, Debug, PartialEq)]
pub struct Hierarchy {
    groups: Vec<Group>,
    clusters: Vec<Cluster>,
}

impl Hierarchy {
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn clusters(&self) -> &[Cluster] {
        &self.clusters
    }

    /// The clusters that `group` merged and simplified.
    pub fn group_clusters(&self, group: &Group) -> &[Cluster] {
        &self.clusters[group.clusters.clone()]
    }

    /// Number of DAG levels: the largest depth plus one, or 0 for an empty mesh.
    pub fn level_count(&self) -> usize {
        self.groups
            .iter()
            .map(|g| g.depth as usize + 1)
            .max()
            .unwrap_or(0)
    }

    /// Spatial tree over the groups, for cluster visibility tests.
    pub fn build_bvh(&self, node_width: usize) -> Result<Bvh, Error> {
        if node_width < 2 {
            return Err(Error::NodeWidth(node_width));
        }
        if self.groups.is_empty() {
            return Ok(Bvh::default());
        }

        let level_count = self.level_count();
        let raw_groups: Vec<ffi::RawGroup> = self
            .groups
            .iter()
            .map(|g| ffi::RawGroup {
                depth: g.depth as i32,
                bounds: g.bounds.into(),
                cluster_begin: 0,
                cluster_count: 0,
            })
            .collect();

        // SAFETY: plain arithmetic on counts.
        let bound = unsafe { ffi::mlc_hierarchy_bound(raw_groups.len(), node_width, level_count) };
        let mut raw = vec![ffi::RawNode::default(); bound];
        // SAFETY: `raw` has `bound` nodes, which the wrapper checks against its required capacity;
        // `raw_groups` holds `raw_groups.len()` valid groups.
        let used = unsafe {
            ffi::mlc_build_hierarchy(
                raw.as_mut_ptr(),
                raw.len(),
                raw_groups.as_ptr(),
                raw_groups.len(),
                node_width,
                level_count,
            )
        };
        if used == 0 {
            return Err(Error::Hierarchy);
        }
        raw.truncate(used);

        let nodes = raw
            .iter()
            .map(|n| Node {
                bounds: n.bounds.into(),
                group: usize::try_from(n.group).ok(),
                children: n.child_offset as usize..(n.child_offset + n.child_count) as usize,
            })
            .collect();

        Ok(Bvh { nodes, level_count })
    }
}

/// One node of the spatial tree. A leaf refers to one group; an internal node to its children.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub bounds: Bounds,
    /// Leaf: the group index in `Hierarchy::groups()`.
    pub group: Option<usize>,
    /// Internal: range of child indices in `Bvh::nodes()`. Empty for leaves.
    pub children: Range<usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bvh {
    nodes: Vec<Node>,
    level_count: usize,
}

impl Bvh {
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// One root per DAG level, in level order.
    pub fn roots(&self) -> &[Node] {
        &self.nodes[..self.level_count]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    MaxTriangles(usize),
    IndexCount(usize),
    IndexOutOfRange {
        index: u32,
        vertex_count: usize,
    },
    NonFinitePosition {
        vertex: usize,
    },
    AttributeDataLength {
        expected: usize,
        actual: usize,
    },
    AttributeWeights {
        stride: usize,
        count: usize,
    },
    /// Attribute stride exceeds the native 256-byte limit or overflows in bytes.
    AttributeStride(usize),
    /// The total attribute float count overflows `usize`.
    AttributeSizeOverflow {
        vertex_count: usize,
        stride: usize,
    },
    /// An attribute weight is negative or non-finite.
    AttributeWeight {
        attribute: usize,
    },
    AttributeProtectMask {
        stride: usize,
        mask: u32,
    },
    /// `vertex_lock` must hold one byte per vertex.
    VertexLockLength {
        expected: usize,
        actual: usize,
    },
    NodeWidth(usize),
    /// The C++ builder returned no result (allocation failure or an internal exception).
    Native,
    /// The C++ hierarchy builder refused the input (capacity, missing level, or internal exception).
    Hierarchy,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::MaxTriangles(n) => write!(
                f,
                "max_triangles {n} is outside {MIN_MAX_TRIANGLES}..={MAX_MAX_TRIANGLES}"
            ),
            Error::IndexCount(n) => write!(f, "index count {n} is not a multiple of 3"),
            Error::IndexOutOfRange {
                index,
                vertex_count,
            } => write!(f, "index {index} is outside {vertex_count} vertices"),
            Error::NonFinitePosition { vertex } => {
                write!(f, "vertex {vertex} has a non-finite position")
            }
            Error::AttributeDataLength { expected, actual } => {
                write!(f, "attribute data has {actual} floats, expected {expected}")
            }
            Error::AttributeWeights { stride, count } => write!(
                f,
                "{count} attribute weights exceed stride {stride} or the native limit of {MAX_ATTRIBUTE_COUNT}"
            ),
            Error::AttributeStride(stride) => write!(
                f,
                "attribute stride {stride} floats exceeds the native limit of {MAX_ATTRIBUTE_STRIDE_BYTES} bytes"
            ),
            Error::AttributeSizeOverflow {
                vertex_count,
                stride,
            } => write!(
                f,
                "attribute data size overflows for {vertex_count} vertices with stride {stride}"
            ),
            Error::AttributeWeight { attribute } => {
                write!(f, "attribute weight {attribute} is negative or non-finite")
            }
            Error::AttributeProtectMask { stride, mask } => write!(
                f,
                "protect mask {mask:#x} has bits beyond the {stride} attribute floats"
            ),
            Error::VertexLockLength { expected, actual } => write!(
                f,
                "vertex lock has {actual} bytes, expected one per vertex ({expected})"
            ),
            Error::NodeWidth(n) => write!(f, "node width {n} is below 2"),
            Error::Native => write!(f, "clusterlod build failed inside the C++ wrapper"),
            Error::Hierarchy => write!(
                f,
                "clusterlod hierarchy build failed inside the C++ wrapper"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Builds the cluster DAG for `mesh`. An empty index list gives an empty hierarchy.
pub fn build(mesh: &Mesh<'_>, config: Config) -> Result<Hierarchy, Error> {
    if !(MIN_MAX_TRIANGLES..=MAX_MAX_TRIANGLES).contains(&config.max_triangles) {
        return Err(Error::MaxTriangles(config.max_triangles));
    }

    let vertex_count = mesh.positions.len();
    if !mesh.indices.len().is_multiple_of(3) {
        return Err(Error::IndexCount(mesh.indices.len()));
    }
    if let Some(&index) = mesh.indices.iter().find(|&&i| i as usize >= vertex_count) {
        return Err(Error::IndexOutOfRange {
            index,
            vertex_count,
        });
    }
    if let Some(vertex) = mesh
        .positions
        .iter()
        .position(|p| !p.iter().all(|c| c.is_finite()))
    {
        return Err(Error::NonFinitePosition { vertex });
    }

    let lock = mesh.vertex_lock;
    if let Some(lock) = lock
        && lock.len() != vertex_count
    {
        return Err(Error::VertexLockLength {
            expected: vertex_count,
            actual: lock.len(),
        });
    }

    let (attributes, attribute_stride, weights, attribute_count, protect_mask) =
        match mesh.attributes {
            None => (std::ptr::null(), 0, std::ptr::null(), 0, 0),
            Some(a) => {
                let expected =
                    vertex_count
                        .checked_mul(a.stride)
                        .ok_or(Error::AttributeSizeOverflow {
                            vertex_count,
                            stride: a.stride,
                        })?;
                let attribute_stride = a
                    .stride
                    .checked_mul(size_of::<f32>())
                    .filter(|&bytes| bytes <= MAX_ATTRIBUTE_STRIDE_BYTES)
                    .ok_or(Error::AttributeStride(a.stride))?;
                if a.data.len() != expected {
                    return Err(Error::AttributeDataLength {
                        expected,
                        actual: a.data.len(),
                    });
                }
                if a.weights.len() > a.stride || a.weights.len() > MAX_ATTRIBUTE_COUNT {
                    return Err(Error::AttributeWeights {
                        stride: a.stride,
                        count: a.weights.len(),
                    });
                }
                if let Some(attribute) = a
                    .weights
                    .iter()
                    .position(|&weight| !weight.is_finite() || weight < 0.0)
                {
                    return Err(Error::AttributeWeight { attribute });
                }
                if a.stride < u32::BITS as usize && u64::from(a.protect_mask) >= 1u64 << a.stride {
                    return Err(Error::AttributeProtectMask {
                        stride: a.stride,
                        mask: a.protect_mask,
                    });
                }
                (
                    a.data.as_ptr(),
                    attribute_stride,
                    a.weights.as_ptr(),
                    a.weights.len(),
                    a.protect_mask,
                )
            }
        };

    // clodBuild takes &remap[0], which is invalid when there are no vertices.
    // No triangles means there is no native work, after validating every input.
    if mesh.indices.is_empty() {
        return Ok(Hierarchy {
            groups: Vec::new(),
            clusters: Vec::new(),
        });
    }

    let raw = ffi::RawMesh {
        indices: mesh.indices.as_ptr(),
        index_count: mesh.indices.len(),
        positions: mesh.positions.as_ptr().cast(),
        vertex_count,
        attributes,
        attribute_stride,
        attribute_weights: weights,
        attribute_count,
        attribute_protect_mask: protect_mask,
        vertex_lock: lock.map_or(std::ptr::null(), <[u8]>::as_ptr),
    };
    let preset = match config.preset {
        Preset::Classic => ffi::PRESET_CLASSIC,
        Preset::Rt => ffi::PRESET_RT,
    };

    // SAFETY: `raw` borrows slices that outlive this call; the wrapper copies everything it keeps.
    // Attribute sizes, count, stride, weights, and mask satisfy the native preconditions above.
    let handle = NonNull::new(unsafe {
        ffi::mlc_build(
            &raw,
            preset,
            config.max_triangles,
            config.tail_min_triangles,
        )
    })
    .ok_or(Error::Native)?;
    let result = NativeResult(handle);
    Ok(result.to_hierarchy())
}

/// Owns a `MlcResult` and frees it on drop.
struct NativeResult(NonNull<ffi::RawResult>);

impl Drop for NativeResult {
    fn drop(&mut self) {
        // SAFETY: the handle came from `mlc_build` and is freed exactly once.
        unsafe { ffi::mlc_free(self.0.as_ptr()) }
    }
}

impl NativeResult {
    fn to_hierarchy(&self) -> Hierarchy {
        let p = self.0.as_ptr();
        // SAFETY: each slice is valid until `self` drops, and `slice` covers the empty case.
        let (raw_groups, raw_clusters, raw_indices) = unsafe {
            (
                slice(ffi::mlc_groups(p), ffi::mlc_group_count(p)),
                slice(ffi::mlc_clusters(p), ffi::mlc_cluster_count(p)),
                slice(ffi::mlc_indices(p), ffi::mlc_index_count(p)),
            )
        };

        let groups = raw_groups
            .iter()
            .map(|g| Group {
                depth: g.depth as u32,
                bounds: g.bounds.into(),
                clusters: g.cluster_begin..g.cluster_begin + g.cluster_count,
            })
            .collect();

        let clusters = raw_clusters
            .iter()
            .map(|c| Cluster {
                refined: usize::try_from(c.refined).ok(),
                bounds: c.bounds.into(),
                vertex_count: c.vertex_count,
                indices: raw_indices[c.index_begin..c.index_begin + c.index_count].into(),
            })
            .collect();

        Hierarchy { groups, clusters }
    }
}

/// # Safety
/// `ptr` must point to `len` valid values that outlive the returned slice. Never called with a null
/// pointer for `len > 0` in this crate, and `len == 0` returns an empty slice without reading `ptr`.
unsafe fn slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: upheld by the caller, see above.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

impl From<ffi::RawBounds> for Bounds {
    fn from(b: ffi::RawBounds) -> Self {
        Bounds {
            center: b.center,
            radius: b.radius,
            error: b.error,
        }
    }
}

impl From<Bounds> for ffi::RawBounds {
    fn from(b: Bounds) -> Self {
        ffi::RawBounds {
            center: b.center,
            radius: b.radius,
            error: b.error,
        }
    }
}

#[cfg(test)]
mod tests;
