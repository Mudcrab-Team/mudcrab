// Moves every instance to its place for the current render origin: its absolute translation plus
// the origin offset, written into the instance table and into the mesh uniforms the stock vertex
// code reads. Recomputed from the absolute translation each time, so no rounding accumulates over
// many origin changes, and it runs on the GPU so a cell crossing uploads nothing but 32 bytes.

struct ShiftParams {
    offset: vec4<f32>,
    count: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> params: ShiftParams;
// Per instance: absolute translation (xyz).
@group(0) @binding(1) var<storage, read> absolute: array<vec4<f32>>;
// Instance records are 20 words (a column-major matrix, then mesh, batch, slice start, worldspace).
@group(0) @binding(2) var<storage, read_write> instances: array<f32>;
// Mesh uniforms are 44 words: world-from-local as three rows of four (translation in the fourth
// word of each row), the same again for the previous frame, then the rest.
@group(0) @binding(3) var<storage, read_write> mesh_uniforms: array<f32>;

const INSTANCE_WORDS: u32 = 20u;
const UNIFORM_WORDS: u32 = 44u;

@compute @workgroup_size(64)
fn shift(@builtin(global_invocation_id) id: vec3<u32>) {
    // A dispatch dimension is at most 65,535 workgroups, so a large scene spreads over rows.
    let instance = id.y * (65535u * 64u) + id.x;
    if instance >= params.count {
        return;
    }
    let t = absolute[instance].xyz + params.offset.xyz;
    let instance_base = instance * INSTANCE_WORDS;
    instances[instance_base + 12u] = t.x;
    instances[instance_base + 13u] = t.y;
    instances[instance_base + 14u] = t.z;
    let uniform_base = instance * UNIFORM_WORDS;
    mesh_uniforms[uniform_base + 3u] = t.x;
    mesh_uniforms[uniform_base + 7u] = t.y;
    mesh_uniforms[uniform_base + 11u] = t.z;
    mesh_uniforms[uniform_base + 15u] = t.x;
    mesh_uniforms[uniform_base + 19u] = t.y;
    mesh_uniforms[uniform_base + 23u] = t.z;
}
