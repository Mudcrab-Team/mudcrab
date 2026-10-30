// Stable per-view stream compaction. Eight stages: instance visibility/local scan,
// instance tile prefix, stable instance scatter, cluster selection, draw tile scan,
// draw-group tile prefix, and small/large stable cluster scatter. No CPU readback or global append.
// Prefixing source-ordered instances then clusters fixes equal-depth alpha-mask ties.
// Camera forward/prepass, every shadow view and terrain consume the same final lists.

struct DrawArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

struct CullParams {
    planes: array<vec4<f32>, 5>,
    // xyz: camera position, w: pixels per unit of error (per unit of error at distance one for a
    // perspective view, per unit of error for an orthographic one).
    camera: vec4<f32>,
    error_pixels: f32,
    near: f32,
    // Reserved alignment word. All instances remain eligible at every projected size.
    reserved_instance_padding: f32,
    instance_count: u32,
    // Non-zero for an orthographic view (a directional light's shadow cascade).
    orthographic: u32,
    // The threshold for meshes only terrain draws (MeshInfo.terrain), in place of error_pixels.
    terrain_error_pixels: f32,
    instance_mask_base: u32,
    // Host run stamp (retained in the shared cull parameter ABI).
    epoch: u32,
}

// Where a pooled mesh starts in each shared buffer, its UV range and its bounding sphere.
struct MeshInfo {
    vertex_base: u32,
    cluster_base: u32,
    vertex_ref_base: u32,
    index_base: u32,
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
    // Immutable scratch slice in instance/cluster source order, assigned on upload.
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
@group(0) @binding(5) var<storage, read_write> counts: array<u32>;
@group(0) @binding(6) var<storage, read> meshes: array<MeshInfo>;
// Per draw: instance, mesh, cluster within the mesh.
@group(0) @binding(7) var<storage, read_write> visible: array<vec4<u32>>;
// Four tightly packed N-word planes: selected counts, local ranks / draw prefixes,
// draw tiles, stable surviving source IDs. Counts and local ranks are rewritten for
// every current instance before compaction; a rejected local rank is 0xffffffff.
@group(0) @binding(8) var<storage, read_write> instance_list: array<u32>;
// The indirect dispatch of pass 2, with the survivor count in the fourth word. A dispatch
// dimension is at most 65,535 workgroups, so a view with more survivors than that spreads them over
// rows of 65,535: x grows to at most 65,535, y to the rows needed, and pass 2 skips the unused tail.
@group(0) @binding(9) var<storage, read_write> dispatch: array<u32, 4>;

const DISPATCH_ROW: u32 = 65535u;
const REJECTED_INSTANCE: u32 = 0xffffffffu;
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


// Scratch is indexed by immutable source slots. Zero rejects a cluster; otherwise its
// x keeps triangle count plus one (including valid zero-triangle clusters); y keeps first_index.
// Rejected instances need no scratch clear: their count is zero.
@group(0) @binding(11) var<storage, read_write> prefixes: array<vec2<u32>>;
@group(0) @binding(12) var<storage, read_write> selected_clusters: array<vec2<u32>>;
@group(0) @binding(13) var<storage, read> source_order: array<u32>;

// prefixes[0].x is the current survivor count. Other entries are (tile sum, tile prefix).
// The instance stages and draw stages reuse those entries after their consumers finish.
// Packed instance planes keep each stage's adjacent source reads/writes coalesced.
var<workgroup> scan_small: array<array<u32, 64>, 2>;
var<workgroup> scan_large: array<array<u32, 256>, 2>;
var<workgroup> selected_count: atomic<u32>;
var<workgroup> uniform_value: u32;

// Storage loads are conservatively nonuniform in WGSL validation. Only lane zero
// publishes bounds controlling barriers, then every lane observes the same bound.
fn uniform_count(value: u32, lane: u32) -> u32 {
    workgroupBarrier();
    if lane == 0u { uniform_value = value; }
    return workgroupUniformLoad(&uniform_value);
}

// Ping-pong scan storage needs one barrier per step; in-place scans need two.
fn scan64(value: u32, lane: u32) -> vec2<u32> {
    scan_small[0][lane] = value;
    workgroupBarrier();
    var source = 0u;
    for (var step = 1u; step < 64u; step *= 2u) {
        var add = 0u;
        if lane >= step { add = scan_small[source][lane - step]; }
        scan_small[1u - source][lane] = scan_small[source][lane] + add;
        workgroupBarrier();
        source = 1u - source;
    }
    let result = vec2<u32>(scan_small[source][lane] - value, scan_small[source][63]);
    // Every lane must finish reading before the next invocation reuses bank zero.
    workgroupBarrier();
    return result;
}

// Ping-pong scan storage needs one barrier per step; in-place scans need two.
fn scan256(value: u32, lane: u32) -> vec2<u32> {
    scan_large[0][lane] = value;
    workgroupBarrier();
    var source = 0u;
    for (var step = 1u; step < 256u; step *= 2u) {
        var add = 0u;
        if lane >= step { add = scan_large[source][lane - step]; }
        scan_large[1u - source][lane] = scan_large[source][lane] + add;
        workgroupBarrier();
        source = 1u - source;
    }
    let result = vec2<u32>(scan_large[source][lane] - value, scan_large[source][255]);
    // Every lane must finish reading before the next invocation reuses bank zero.
    workgroupBarrier();
    return result;
}

fn instance_in_view(instance_id: u32) -> bool {
    let instance = instances[instance_id];
    if batch_table[instance.batch].x == 0u { return false; }
    let bits = batch_table[params.instance_mask_base + (instance_id >> 7u)];
    if (bits[(instance_id >> 5u) & 3u] & (1u << (instance_id & 31u))) == 0u { return false; }
    let mesh = meshes[instance.mesh];
    let center = (instance.world * vec4<f32>(mesh.bounds.xyz, 1.0)).xyz;
    let radius = mesh.bounds.w * largest_scale(instance.world);
    if !sphere_in_view(center, radius) { return false; }
    return true;
}

@compute @workgroup_size(64)
fn cull_instances(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let tile = wg.y * DISPATCH_ROW + wg.x;
    if tile >= (params.instance_count + 63u) / 64u { return; }
    let instance_id = tile * 64u + lane;
    var accepted = false;
    // Queue plans the order before PrepareResources appends new blocks. The header
    // snapshots that plan's high-watermark; a same-frame append must wait for the
    // next plan, even if the current per-view mask already enables it. This also
    // prevents reads from a newly appended batch beyond the planned table.
    if instance_id < min(params.instance_count, source_order[2]) {
        accepted = instance_in_view(instance_id);
    }
    let scan = scan64(select(0u, 1u, accepted), lane);
    if lane == 0u { prefixes[1u + tile] = vec2<u32>(scan.y, 0u); }
    if instance_id < params.instance_count {
        instance_list[instance_id] = 0u;
        instance_list[params.instance_count + instance_id] = select(REJECTED_INSTANCE, scan.x, accepted);
    }
}

@compute @workgroup_size(256)
fn prefix_instances(@builtin(local_invocation_index) lane: u32) {
    let tiles = (params.instance_count + 63u) / 64u;
    var carry = 0u;
    for (var first = 0u; first < tiles; first += 256u) {
        let tile = first + lane;
        var count = 0u;
        if tile < tiles { count = prefixes[1u + tile].x; }
        let scan = scan256(count, lane);
        if tile < tiles { prefixes[1u + tile].y = carry + scan.x; }
        carry += scan.y;
    }
    if lane == 0u {
        prefixes[0].x = carry;
        dispatch[0] = min(carry, DISPATCH_ROW);
        dispatch[1] = max((carry + DISPATCH_ROW - 1u) / DISPATCH_ROW, 1u);
        dispatch[2] = 1u;
        dispatch[3] = carry;
    }
}

@compute @workgroup_size(64)
fn scatter_instances(@builtin(global_invocation_id) id: vec3<u32>) {
    let instance_id = id.y * DISPATCH_ROW * 64u + id.x;
    if instance_id >= params.instance_count { return; }
    let local_rank = instance_list[params.instance_count + instance_id];
    if local_rank != REJECTED_INSTANCE {
        let rank = prefixes[1u + instance_id / 64u].y + local_rank;
        instance_list[params.instance_count * 3u + rank] = instance_id;
    }
    // cull_instances already zeroed every selected count, including rejected instances.
}

fn selected_cluster(instance: Instance, mesh: MeshInfo, cluster_id: u32, scale: f32) -> u32 {
    let base = (mesh.cluster_base + cluster_id) * 12u;
    let packed = clusters[base + 8u];
    let group = mesh.group_base + (packed >> 16u);
    let refined = clusters[base + 9u] & 0xffffu;
    let threshold = select(params.error_pixels, params.terrain_error_pixels, mesh.terrain == 1u);
    var selected = projected_error(group_bounds(group), group_error(group), instance.world, scale) > threshold;
    if selected && refined != NO_GROUP {
        let refined_group = mesh.group_base + refined;
        selected = projected_error(group_bounds(refined_group), group_error(refined_group), instance.world, scale) <= threshold;
    }
    if !selected { return 0u; }
    let bounds = vec4<f32>(bitcast<f32>(clusters[base]), bitcast<f32>(clusters[base + 1u]), bitcast<f32>(clusters[base + 2u]), bitcast<f32>(clusters[base + 3u]));
    let center = (instance.world * vec4<f32>(bounds.xyz, 1.0)).xyz;
    if !sphere_in_view(center, bounds.w * scale) { return 0u; }
    return ((packed >> 8u) & 0xffu) + 1u;
}

@compute @workgroup_size(64)
fn cull(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let rank = wg.y * DISPATCH_ROW + wg.x;
    let survivors = uniform_count(prefixes[0].x, lane);
    if rank >= survivors { return; }
    let instance_id = instance_list[params.instance_count * 3u + rank];
    let instance = instances[instance_id];
    let mesh = meshes[instance.mesh];
    let scale = largest_scale(instance.world);
    if lane == 0u { atomicStore(&selected_count, 0u); }
    workgroupBarrier();
    var count = 0u;
    for (var cluster = lane; cluster < mesh.cluster_count; cluster += 64u) {
        let selection = selected_cluster(instance, mesh, cluster, scale);
        // Carry the index range from the stage that already binds cluster records. The
        // scatter stages stay within eight storage bindings, including on Metal.
        let triangle_offset = clusters[(mesh.cluster_base + cluster) * 12u + 7u];
        selected_clusters[instance.region_start + cluster] = vec2<u32>(
            selection, mesh.index_base + triangle_offset * 3u);
        count += select(0u, 1u, selection != 0u);
    }
    // This atomic is a commutative workgroup reduction, never an append or ordering decision.
    if count != 0u { atomicAdd(&selected_count, count); }
    workgroupBarrier();
    if lane == 0u { instance_list[instance_id] = atomicLoad(&selected_count); }
}

@compute @workgroup_size(256)
fn prefix_draw_tiles(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let tile = wg.y * DISPATCH_ROW + wg.x;
    let tile_count = uniform_count(source_order[1], lane);
    if tile >= tile_count { return; }
    let base = 4u + source_order[0] * 4u + tile * 4u;
    let length = source_order[base + 1u];
    var instance_id = 0u;
    var count = 0u;
    if lane < length {
        instance_id = source_order[source_order[base] + lane];
        count = instance_list[instance_id];
    }
    let scan = scan256(count, lane);
    if lane < length {
        instance_list[params.instance_count + instance_id] = scan.x;
        instance_list[params.instance_count * 2u + instance_id] = tile;
    }
    if lane == 0u { prefixes[1u + tile] = vec2<u32>(scan.y, 0u); }
}

@compute @workgroup_size(256)
fn prefix_draw_groups(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let group = wg.y * DISPATCH_ROW + wg.x;
    let group_count = uniform_count(source_order[0], lane);
    if group >= group_count { return; }
    let base = 4u + group * 4u;
    let first_tile = source_order[base];
    let tiles = uniform_count(source_order[base + 1u], lane);
    var carry = 0u;
    for (var first = 0u; first < tiles; first += 256u) {
        let local_tile = first + lane;
        let tile = first_tile + local_tile;
        var count = 0u;
        if local_tile < tiles { count = prefixes[1u + tile].x; }
        let scan = scan256(count, lane);
        if local_tile < tiles { prefixes[1u + tile].y = carry + scan.x; }
        carry += scan.y;
    }
    if lane == 0u { counts[group] = carry; }
}

fn draw_base(instance_id: u32) -> u32 {
    let offset = instance_list[params.instance_count + instance_id];
    let tile_id = instance_list[params.instance_count * 2u + instance_id];
    let tile = 4u + source_order[0] * 4u + tile_id * 4u;
    let group = source_order[tile + 2u];
    return source_order[4u + group * 4u + 2u] + prefixes[1u + tile_id].y + offset;
}

// Avoid allocating a whole workgroup and running a scan for the common short lists.
// Their bounded serial walk preserves cluster order without any shared-memory barriers.
@compute @workgroup_size(64)
fn scatter_clusters(@builtin(global_invocation_id) id: vec3<u32>) {
    let instance_id = id.y * DISPATCH_ROW * 64u + id.x;
    if instance_id >= params.instance_count { return; }
    if instance_list[instance_id] == 0u { return; }
    let instance = instances[instance_id];
    let mesh = meshes[instance.mesh];
    if mesh.cluster_count > 64u { return; }
    let base = draw_base(instance_id);
    var written = 0u;
    for (var cluster = 0u; cluster < mesh.cluster_count; cluster += 1u) {
        let selected = selected_clusters[instance.region_start + cluster];
        let selection = selected.x;
        if selection != 0u {
            let slot = base + written;
            args[slot] = DrawArgs((selection - 1u) * 3u, 1u, selected.y, 0, slot);
            visible[slot] = vec4<u32>(instance_id, instance.mesh, cluster, 0u);
            written += 1u;
        }
    }
}

// Large meshes keep parallel cluster traversal, regardless of content/mod cluster counts.
// CPU-known source IDs schedule these workgroups; visibility remains entirely GPU-derived.
@compute @workgroup_size(64)
fn scatter_large_clusters(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let rank = wg.y * DISPATCH_ROW + wg.x;
    let first = source_order[3];
    let count = uniform_count(source_order[first], lane);
    if rank >= count { return; }
    let instance_id = source_order[first + 1u + rank];
    let selected = uniform_count(instance_list[instance_id], lane);
    if selected == 0u { return; }
    let instance = instances[instance_id];
    let mesh = meshes[instance.mesh];
    let base = draw_base(instance_id);
    var written = 0u;
    let cluster_count = uniform_count(mesh.cluster_count, lane);
    for (var first_cluster = 0u; first_cluster < cluster_count; first_cluster += 256u) {
        var selections: array<u32, 4>;
        var total = 0u;
        for (var i = 0u; i < 4u; i += 1u) {
            let cluster = first_cluster + lane * 4u + i;
            // Reset every element on every chunk, including its out-of-range tail.
            selections[i] = 0u;
            if cluster < mesh.cluster_count { selections[i] = selected_clusters[instance.region_start + cluster].x; }
            total += select(0u, 1u, selections[i] != 0u);
        }
        let scan = scan64(total, lane);
        var prefix = written + scan.x;
        for (var i = 0u; i < 4u; i += 1u) {
            let selection = selections[i];
            if selection != 0u {
                let slot = base + prefix;
                let cluster = first_cluster + lane * 4u + i;
                let first_index = selected_clusters[instance.region_start + cluster].y;
                args[slot] = DrawArgs((selection - 1u) * 3u, 1u, first_index, 0, slot);
                visible[slot] = vec4<u32>(instance_id, instance.mesh, first_cluster + lane * 4u + i, 0u);
                prefix += 1u;
            }
        }
        written += scan.y;
    }
}
