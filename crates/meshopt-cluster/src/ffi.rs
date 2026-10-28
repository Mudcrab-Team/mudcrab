//! Raw bindings to `cpp/mlc_wrapper.h`. Only `lib.rs` calls these, and only through its checked wrappers.

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawBounds {
    pub center: [f32; 3],
    pub radius: f32,
    pub error: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawGroup {
    pub depth: i32,
    pub bounds: RawBounds,
    pub cluster_begin: usize,
    pub cluster_count: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawCluster {
    pub refined: i32,
    pub bounds: RawBounds,
    pub index_begin: usize,
    pub index_count: usize,
    pub vertex_count: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawNode {
    pub bounds: RawBounds,
    pub group: i32,
    pub child_offset: u32,
    pub child_count: u32,
}

#[repr(C)]
pub struct RawMesh {
    pub indices: *const u32,
    pub index_count: usize,
    pub positions: *const f32,
    pub vertex_count: usize,
    pub attributes: *const f32,
    pub attribute_stride: usize,
    pub attribute_weights: *const f32,
    pub attribute_count: usize,
    pub attribute_protect_mask: u32,
    pub vertex_lock: *const u8,
}

/// Opaque handle returned by `mlc_build`; freed with `mlc_free`.
#[repr(C)]
pub struct RawResult {
    _private: [u8; 0],
}

pub const PRESET_CLASSIC: i32 = 0;
pub const PRESET_RT: i32 = 1;

unsafe extern "C" {
    pub fn mlc_build(
        mesh: *const RawMesh,
        preset: i32,
        max_triangles: usize,
        tail_min_triangles: usize,
    ) -> *mut RawResult;
    pub fn mlc_free(result: *mut RawResult);

    pub fn mlc_group_count(result: *const RawResult) -> usize;
    pub fn mlc_groups(result: *const RawResult) -> *const RawGroup;
    pub fn mlc_cluster_count(result: *const RawResult) -> usize;
    pub fn mlc_clusters(result: *const RawResult) -> *const RawCluster;
    pub fn mlc_index_count(result: *const RawResult) -> usize;
    pub fn mlc_indices(result: *const RawResult) -> *const u32;

    pub fn mlc_hierarchy_bound(group_count: usize, node_width: usize, level_count: usize) -> usize;
    pub fn mlc_build_hierarchy(
        nodes: *mut RawNode,
        node_capacity: usize,
        groups: *const RawGroup,
        group_count: usize,
        node_width: usize,
        level_count: usize,
    ) -> usize;

    pub fn mlc_local_indices(
        vertices: *mut u32,
        triangles: *mut u8,
        indices: *const u32,
        index_count: usize,
    ) -> usize;
}
