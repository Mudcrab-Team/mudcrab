// Cluster selection and compaction (ADR-0013 section 2, clusterlod rule), in two passes per view
// that cover every batch at once, so the pass count does not grow with the number of materials.
//
// Pass 1, `cull_instances`: one thread per instance. An instance whose batch has no material yet,
// or whose bounding sphere is outside the view, is dropped before any of its clusters is looked
// at; the others are appended to `instance_list` with the draw group and slice of `args` their
// batch maps to in this view, and counted in the x of the indirect dispatch arguments.
//
// Pass 2, `cull`: one workgroup per surviving instance (an indirect dispatch), its threads
// striding over the instance's clusters. A cluster that is selected and inside the frustum appends
// one non-indexed indirect draw argument to its batch's slice of `args`, records which instance,
// mesh and cluster it is in `visible`, and bumps the batch's draw count; every other cluster
// writes nothing. The caller clears the arguments and the counts before the passes, so the unused
// tail of a group's slice is zero vertices for the fixed-count fallback and the count buffer
// drives `multi_draw_indirect_count` where the adapter has it. A draw group (the batches of one
// view that share a pipeline and a material bind group) is drawn with one indirect call over its
// own slice.
//
// A cluster is drawn when
//   1. the error of the group it belongs to is over the threshold, and
//   2. it came from source geometry, or the group that produced it is at or under the threshold.
// Both errors are projected through the bounds of their own group, so every cluster of a group
// makes the same decision and neighbouring groups never open a crack.

struct DrawArgs {
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
}

struct CullParams {
    planes: array<vec4<f32>, 5>,
    // xyz: camera position, w: pixels per unit of error (per unit of error at distance one for a
    // perspective view, per unit of error for an orthographic one).
    camera: vec4<f32>,
    error_pixels: f32,
    near: f32,
    // An instance whose bounding sphere spans fewer pixels than this is dropped (perspective only).
    min_instance_pixels: f32,
    instance_count: u32,
    // Non-zero for an orthographic view (a directional light's shadow cascade).
    orthographic: u32,
    // The threshold for meshes only terrain draws (MeshInfo.terrain), in place of error_pixels.
    terrain_error_pixels: f32,
    instance_mask_base: u32,
    // Names this run of the cull: the stamp pass 1 gives its entries and pass 2 requires.
    epoch: u32,
}

// Where a pooled mesh starts in each shared buffer, its UV range and its bounding sphere.
struct MeshInfo {
    vertex_base: u32,
    cluster_base: u32,
    vertex_ref_base: u32,
    triangle_base: u32,
    group_base: u32,
    cluster_count: u32,
    // 1 when only terrain instances draw the mesh: it is selected against terrain_error_pixels.
    terrain: u32,
    pad1: u32,
    // uv_min (xy), uv_scale (zw)
    uv: vec4<f32>,
    // Whole-mesh bounding sphere: centre xyz, radius.
    bounds: vec4<f32>,
}

struct Instance {
    world: mat4x4<f32>,
    mesh: u32,
    // The batch (material) the instance draws with.
    batch: u32,
    region_start: u32,
    // The child worldspace the object is placed in; zero when it is in the block's own.
    source_worldspace: u32,
}

const NO_GROUP: u32 = 0xffffu;

@group(0) @binding(0) var<uniform> params: CullParams;
// 48-byte cluster records as 12 words: bounds (4 f32), error, parent_error, vertex_offset,
// triangle_offset, vertex_count | triangle_count << 8 | group << 16, refined_group, 2 reserved.
@group(0) @binding(1) var<storage, read> clusters: array<u32>;
// 32-byte group records as 8 words: bounds (4 f32), error, depth | first_cluster << 16, ...
@group(0) @binding(2) var<storage, read> groups: array<u32>;
@group(0) @binding(3) var<storage, read> instances: array<Instance>;
@group(0) @binding(4) var<storage, read_write> args: array<DrawArgs>;
// One draw count per draw group.
@group(0) @binding(5) var<storage, read_write> counts: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read> meshes: array<MeshInfo>;
// Per draw: instance, mesh, cluster within the mesh.
@group(0) @binding(7) var<storage, read_write> visible: array<vec4<u32>>;
// The instances that are in view, written by pass 1 and read by pass 2.
// Per surviving instance: x the instance, y its draw group, z where the group's slice of `args`
// and `visible` starts, w the epoch of the run that wrote it.
@group(0) @binding(8) var<storage, read_write> instance_list: array<vec4<u32>>;
// The indirect dispatch of pass 2, with the survivor count in the fourth word. A dispatch
// dimension is at most 65,535 workgroups, so a view with more survivors than that spreads them over
// rows of 65,535: x grows to at most 65,535, y to the rows needed, and pass 2 skips the unused tail.
@group(0) @binding(9) var<storage, read_write> dispatch: array<atomic<u32>, 4>;

const DISPATCH_ROW: u32 = 65535u;
// Per batch, for this view: x is non-zero once the batch may draw, y its draw group, z where the
// group's slice of `args` starts.
@group(0) @binding(10) var<storage, read> batch_table: array<vec4<u32>>;

// Projected error in pixels of a sphere with simplification error `error` (mesh units).
fn projected_error(bounds: vec4<f32>, error: f32, world_from_local: mat4x4<f32>, scale: f32) -> f32 {
    if params.orthographic != 0u {
        return error * scale * params.camera.w;
    }
    let center = (world_from_local * vec4<f32>(bounds.xyz, 1.0)).xyz;
    let distance = max(length(center - params.camera.xyz) - bounds.w * scale, params.near);
    return error * scale / distance * params.camera.w;
}

fn group_bounds(group: u32) -> vec4<f32> {
    let base = group * 8u;
    return vec4<f32>(
        bitcast<f32>(groups[base]),
        bitcast<f32>(groups[base + 1u]),
        bitcast<f32>(groups[base + 2u]),
        bitcast<f32>(groups[base + 3u]),
    );
}

fn group_error(group: u32) -> f32 {
    return bitcast<f32>(groups[group * 8u + 4u]);
}

fn largest_scale(world_from_local: mat4x4<f32>) -> f32 {
    return max(
        length(world_from_local[0].xyz),
        max(length(world_from_local[1].xyz), length(world_from_local[2].xyz)),
    );
}

// Whether a world-space sphere is at least partly inside the view's side planes.
fn sphere_in_view(center: vec3<f32>, radius: f32) -> bool {
    for (var plane = 0u; plane < 5u; plane = plane + 1u) {
        let p = params.planes[plane];
        if dot(p.xyz, center) + p.w < -radius {
            return false;
        }
    }
    return true;
}

@compute @workgroup_size(64)
fn cull_instances(@builtin(global_invocation_id) id: vec3<u32>) {
    // A dispatch dimension is at most 65,535 workgroups, so a large scene spreads over rows.
    let instance_id = id.y * (DISPATCH_ROW * 64u) + id.x;
    if instance_id >= params.instance_count {
        return;
    }
    let instance = instances[instance_id];
    let entry = batch_table[instance.batch];
    if entry.x == 0u {
        return;
    }
    // The tail follows the material batch table. Every bit starts disabled;
    // CPU selection authorizes this exact GPU instance independently per view.
    let bits = batch_table[params.instance_mask_base + (instance_id >> 7u)];
    if (bits[(instance_id >> 5u) & 3u] & (1u << (instance_id & 31u))) == 0u {
        return;
    }
    let mesh = meshes[instance.mesh];
    let center = (instance.world * vec4<f32>(mesh.bounds.xyz, 1.0)).xyz;
    let radius = mesh.bounds.w * largest_scale(instance.world);
    if !sphere_in_view(center, radius) {
        return;
    }
    if params.orthographic == 0u && params.min_instance_pixels > 0.0 {
        let distance = max(length(center - params.camera.xyz), params.near);
        if 2.0 * radius / distance * params.camera.w < params.min_instance_pixels {
            return;
        }
    }
    let index = atomicAdd(&dispatch[3], 1u);
    atomicMax(&dispatch[0], min(index + 1u, DISPATCH_ROW));
    atomicMax(&dispatch[1], index / DISPATCH_ROW + 1u);
    instance_list[index] = vec4<u32>(instance_id, entry.y, entry.z, params.epoch);
}

@compute @workgroup_size(64)
fn cull(
    @builtin(workgroup_id) group_id: vec3<u32>,
    @builtin(local_invocation_id) local_id: vec3<u32>,
) {
    let entry = instance_list[group_id.y * DISPATCH_ROW + group_id.x];
    if entry.w != params.epoch {
        return;
    }
    let instance_id = entry.x;
    let instance = instances[instance_id];
    let mesh = meshes[instance.mesh];
    let world_from_local = instance.world;
    let scale = largest_scale(world_from_local);
    let threshold = select(params.error_pixels, params.terrain_error_pixels, mesh.terrain == 1u);
    for (var cluster_id = local_id.x; cluster_id < mesh.cluster_count; cluster_id = cluster_id + 64u) {
        let base = (mesh.cluster_base + cluster_id) * 12u;
        let packed = clusters[base + 8u];
        let triangle_count = (packed >> 8u) & 0xffu;
        let group = mesh.group_base + (packed >> 16u);
        let refined = clusters[base + 9u] & 0xffffu;

        var selected = projected_error(group_bounds(group), group_error(group), world_from_local, scale)
            > threshold;
        if selected && refined != NO_GROUP {
            let refined_group = mesh.group_base + refined;
            selected = projected_error(
                group_bounds(refined_group),
                group_error(refined_group),
                world_from_local,
                scale,
            ) <= threshold;
        }
        if !selected {
            continue;
        }

        let bounds = vec4<f32>(
            bitcast<f32>(clusters[base]),
            bitcast<f32>(clusters[base + 1u]),
            bitcast<f32>(clusters[base + 2u]),
            bitcast<f32>(clusters[base + 3u]),
        );
        let center = (world_from_local * vec4<f32>(bounds.xyz, 1.0)).xyz;
        if !sphere_in_view(center, bounds.w * scale) {
            continue;
        }
        let slot = entry.z + atomicAdd(&counts[entry.y], 1u);
        args[slot] = DrawArgs(triangle_count * 3u, 1u, 0u, slot);
        visible[slot] = vec4<u32>(instance_id, instance.mesh, cluster_id, 0u);
    }
}
