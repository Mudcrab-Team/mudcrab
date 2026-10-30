// View-local near-stock clip, shared by terrain forward, prepass and shadow fragments.
// mesh.tag is an ABSOLUTE vec4 index into this view's group-4 binding-6 buffer,
// after the GPU-written visible draw slots. The CPU appends two records of ten immutable vectors:
// float origin/size, float state, then eight vec4<u32> quadrant mask rows.
// Row zero is north-most; columns count east. Inactive/unavailable records discard.
#define_import_path cluster_terrain::clip

#import bevy_pbr::mesh_bindings::mesh

@group(4) @binding(6) var<storage, read> terrain_view_records: array<vec4<u32>>;

fn terrain_clip(world_position: vec4<f32>, instance_index: u32, pass_offset: u32) -> bool {
    let start = mesh[instance_index].tag;
    let base = start + pass_offset;
    if base < start { return true; }
    let count = arrayLength(&terrain_view_records);
    // Subtraction avoids overflow from a malformed or stale absolute index.
    if base >= count || count - base < 10u {
        return true;
    }
    let origin_and_size = bitcast<vec4<f32>>(terrain_view_records[base]);
    let state = bitcast<vec4<f32>>(terrain_view_records[base + 1u]);
    // Reject NaN/infinity by their IEEE exponent before division or integer
    // conversion. WGSL comparisons alone do not reject every non-finite value.
    let exponent = vec4<u32>(0x7f800000u);
    if any((bitcast<vec4<u32>>(origin_and_size) & exponent) == exponent)
        || (bitcast<u32>(state.x) & 0x7f800000u) == 0x7f800000u
        || state.x < 0.5
        || origin_and_size.z <= 0.0
        || origin_and_size.w < 1.0 || origin_and_size.w > 32.0
        || floor(origin_and_size.w) != origin_and_size.w {
        return true;
    }
    let local = (world_position.xz - origin_and_size.xy) / origin_and_size.z;
    let local_exponent = vec2<u32>(0x7f800000u);
    if any((bitcast<vec2<u32>>(local) & local_exponent) == local_exponent) {
        return true;
    }
    // Bound the float coordinates before conversion, including finite values
    // outside the signed integer range.
    if any(local < vec2<f32>(0.0)) || any(local >= vec2<f32>(origin_and_size.w)) {
        return false;
    }
    let column = i32(floor(local.x));
    let row = i32(floor(local.y));
    let row_index = u32(row);
    let word = terrain_view_records[base + 2u + (row_index >> 2u)][row_index & 3u];
    return ((word >> u32(column)) & 1u) != 0u;
}
