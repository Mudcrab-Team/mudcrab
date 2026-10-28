//! Types shared by the stages of the cluster converter (ADR-0013).
//!
//! `PrimitiveGeometry` is one glTF primitive of a converted GLB with its node transforms (and the
//! scene root's Creation-to-runtime rotation) baked in, so its positions are in runtime (Y-up)
//! axes and a reference's instance transform applies directly (ADR-0013 section 3, with the
//! convention fixed in `cluster/mod.rs`).

/// glTF alpha mode of a primitive's material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaMode {
    Opaque,
    Mask,
    Blend,
}

/// Vertex data of one primitive in mesh space.
#[derive(Debug, Clone, Default)]
pub struct PrimitiveGeometry {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// xyz tangent plus handedness in w, present only when the source has tangents.
    pub tangents: Option<Vec<[f32; 4]>>,
    /// Linear 0..1 RGBA, present only when the source has `COLOR_0`.
    pub colours: Option<Vec<[f32; 4]>>,
    /// Triangle list indices into the vertex arrays.
    pub indices: Vec<u32>,
}

impl PrimitiveGeometry {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

pub use shared::cluster_recipe::refusal::RejectReason;

/// A rejected primitive: the first failing rule and a short detail for the inspector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject {
    pub reason: RejectReason,
    pub detail: String,
}

impl Reject {
    pub fn new(reason: RejectReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

/// One drawable primitive of a converted model, loaded for classification and building.
#[derive(Debug, Clone)]
pub struct ModelPrimitive {
    /// Running index over the scene's (node, primitive) pairs in traversal order; the `<N>` of
    /// `clusters/meshes/<hh>/<mesh_hash>-p<N>.mcl`.
    pub index: u32,
    pub geometry: PrimitiveGeometry,
    pub alpha: AlphaMode,
    /// The material's `alphaCutoff` (glTF default 0.5) when `alpha` is `Mask`, else `None`.
    pub alpha_cutoff: Option<f32>,
    pub double_sided: bool,
    /// SHA-256 (lowercase hex) of the material as canonical JSON with image references replaced
    /// by converted texture keys (ADR-0013 section 3).
    pub material_key: String,
    /// The primitive's material slot in its model (the glTF `materials` index); `None` when the
    /// primitive names no material and takes the glTF default.
    pub material_index: Option<u32>,
    /// Axis-aligned bounds of the positions in mesh space: min, max.
    pub bounds: ([f32; 3], [f32; 3]),
    /// `None` when the primitive passes every cluster primitive rule except the triangle
    /// threshold (ADR-0013 section 5).
    pub reject: Option<Reject>,
}
