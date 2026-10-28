// SPDX-License-Identifier: MIT OR Apache-2.0
// Derived from Bevy 0.19.0 `bevy_pbr/render/mesh.wgsl` and `bevy_pbr/render/pbr.wgsl`.
// Skyrim skin material: Bevy's PBR with Skyrim's model-space normal maps.
//
// The vertex stage is Bevy 0.19's `bevy_pbr/render/mesh.wgsl` with the same calls in the same
// order, so positions match the depth prepass bit for bit (the opaque pass tests GreaterEqual
// against it). It adds one output: the 3x3 that carries a shape-space normal to world space (the
// skinned or mesh inverse transpose), which a model-space normal map needs per fragment and which
// the stock `VertexOutput` does not carry.
//
// The fragment stage is `bevy_pbr/render/pbr.wgsl` with the normal and the specular mask taken
// from the model-space maps (`mudcrab::cs_lighting_msn`), on FaceGen heads the base colour run
// through Skyrim's FaceGen tint and detail maps, on tinted skin (body, hands, feet) the base
// colour blended with the actor's skin tone, and on hair, brows and beards the base colour
// multiplied by the NIF's hair tint. Hair keeps the stock tangent-space normal map: only
// model-space materials replace the normal. Everything else is the stock `StandardMaterial` path.

#import bevy_pbr::{
    mesh_bindings::mesh,
    mesh_functions,
    skinning,
    morph::{morph_position, morph_normal, morph_tangent},
    forward_io::{Vertex, VertexOutput, FragmentOutput},
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
    pbr_types,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_fragment::pbr_input_from_standard_material,
    decal::clustered::apply_decals,
}
#import bevy_render::maths::mat2x4_f32_to_mat3x3_unpack
#import mudcrab::cs_lighting_msn::{
    decode_model_space_normal,
    get_facegen_base_color,
    get_facegen_rgb_tint_base_color,
    get_hair_vertex_color,
    material_glossiness,
    model_space_glossiness,
    model_space_world_normal,
}

#ifdef VISIBILITY_RANGE_DITHER
#import bevy_pbr::pbr_functions::visibility_range_dither;
#endif

#ifdef OIT_ENABLED
#import bevy_core_pipeline::oit::oit_draw
#endif

// Keep in sync with `SkyrimSkinFlags` in `skin_material.rs`.
const FLAG_MODEL_SPACE_NORMALS: u32 = 1u;
const FLAG_SPECULAR_MAP: u32 = 2u;
const FLAG_SPECULAR: u32 = 4u;
const FLAG_FACEGEN: u32 = 8u;
const FLAG_SKIN_TINT: u32 = 16u;
const FLAG_HAIR_TINT: u32 = 32u;
const FLAG_DEBUG_WORLD_NORMAL: u32 = 65536u;
const FLAG_DEBUG_VERTEX_NORMAL: u32 = 131072u;

struct SkyrimSkinSettings {
    flags: u32,
    // The skin tint (`FLAG_SKIN_TINT`) or hair tint (`FLAG_HAIR_TINT`), encoded RGB in 0..1;
    // alpha unused.
    tint_color: vec4<f32>,
    detail: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> skin: SkyrimSkinSettings;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var model_space_normal_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var model_space_normal_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var specular_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var specular_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var facegen_tint_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var facegen_tint_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var facegen_detail_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var facegen_detail_sampler: sampler;

// `forward_io::VertexOutput` plus the normal transform at locations 8-10.
struct SkinVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
#ifdef VERTEX_UVS_A
    @location(2) uv: vec2<f32>,
#endif
#ifdef VERTEX_UVS_B
    @location(3) uv_b: vec2<f32>,
#endif
#ifdef VERTEX_TANGENTS
    @location(4) world_tangent: vec4<f32>,
#endif
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    @location(6) @interpolate(flat) instance_index: u32,
#endif
#ifdef VISIBILITY_RANGE_DITHER
    @location(7) @interpolate(flat) visibility_range_dither: i32,
#endif
    @location(8) normal_from_local_x: vec3<f32>,
    @location(9) normal_from_local_y: vec3<f32>,
    @location(10) normal_from_local_z: vec3<f32>,
}

#ifdef MORPH_TARGETS
// As in mesh.wgsl: the instance_index parameter must match vertex_in.instance_index (wgpu dx12).
fn morph_vertex(vertex_in: Vertex, instance_index: u32) -> Vertex {
    var vertex = vertex_in;
    let first_vertex = mesh[instance_index].first_vertex_index;
    let vertex_index = vertex.index - first_vertex;

    let weight_count = bevy_pbr::morph::layer_count(instance_index);
    for (var i: u32 = 0u; i < weight_count; i ++) {
        let weight = bevy_pbr::morph::weight_at(i, instance_index);
        if weight == 0.0 {
            continue;
        }
        vertex.position += weight * morph_position(vertex_index, i, instance_index);
#ifdef VERTEX_NORMALS
        vertex.normal += weight * morph_normal(vertex_index, i, instance_index);
#endif
#ifdef VERTEX_TANGENTS
        vertex.tangent += vec4(weight * morph_tangent(vertex_index, i, instance_index), 0.0);
#endif
    }
    return vertex;
}
#endif

@vertex
fn vertex(vertex_no_morph: Vertex) -> SkinVertexOutput {
    var out: SkinVertexOutput;

#ifdef MORPH_TARGETS
    var vertex = morph_vertex(vertex_no_morph, vertex_no_morph.instance_index);
#else
    var vertex = vertex_no_morph;
#endif

    let mesh_world_from_local = mesh_functions::get_world_from_local(vertex_no_morph.instance_index);

#ifdef SKINNED
    var world_from_local = skinning::skin_model(
        vertex.joint_indices,
        vertex.joint_weights,
        vertex_no_morph.instance_index
    );
    // What `skinning::skin_normals` applies to the vertex normal.
    let normal_from_local = skinning::inverse_transpose_3x3m(mat3x3<f32>(
        world_from_local[0].xyz,
        world_from_local[1].xyz,
        world_from_local[2].xyz,
    ));
#else
    var world_from_local = mesh_world_from_local;
    // What `mesh_functions::mesh_normal_local_to_world` applies to the vertex normal.
    let normal_from_local = mat2x4_f32_to_mat3x3_unpack(
        mesh[vertex_no_morph.instance_index].local_from_world_transpose_a,
        mesh[vertex_no_morph.instance_index].local_from_world_transpose_b,
    );
#endif
    out.normal_from_local_x = normal_from_local[0];
    out.normal_from_local_y = normal_from_local[1];
    out.normal_from_local_z = normal_from_local[2];

#ifdef VERTEX_NORMALS
#ifdef SKINNED
    out.world_normal = skinning::skin_normals(world_from_local, vertex.normal);
#else
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex_no_morph.instance_index
    );
#endif
#endif

#ifdef VERTEX_POSITIONS
    out.world_position = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(vertex.position, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
#endif

#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif

#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        world_from_local,
        vertex.tangent,
        vertex_no_morph.instance_index
    );
#endif

#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex_no_morph.instance_index;
#endif

#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex_no_morph.instance_index, mesh_world_from_local[3]);
#endif

    return out;
}

fn stock_vertex_output(skin_in: SkinVertexOutput) -> VertexOutput {
    var in: VertexOutput;
    in.position = skin_in.position;
    in.world_position = skin_in.world_position;
    in.world_normal = skin_in.world_normal;
#ifdef VERTEX_UVS_A
    in.uv = skin_in.uv;
#endif
#ifdef VERTEX_UVS_B
    in.uv_b = skin_in.uv_b;
#endif
#ifdef VERTEX_TANGENTS
    in.world_tangent = skin_in.world_tangent;
#endif
#ifdef VERTEX_COLORS
    in.color = skin_in.color;
    if (skin.flags & FLAG_HAIR_TINT) != 0u {
        // Hair's vertex colour masks the tint in green and is not a colour; Bevy would multiply
        // all of it into the base colour. Only its alpha scales the alpha, as in Skyrim.
        in.color = vec4<f32>(1.0, 1.0, 1.0, skin_in.color.a);
    }
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    in.instance_index = skin_in.instance_index;
#endif
#ifdef VISIBILITY_RANGE_DITHER
    in.visibility_range_dither = skin_in.visibility_range_dither;
#endif
    return in;
}

// The sRGB transfer function and its inverse, extended past 1 with the power segment: the
// FaceGen detail map can lift the encoded colour slightly above 1.
fn srgb_encode(linear: vec3<f32>) -> vec3<f32> {
    let low = linear * 12.92;
    let high = 1.055 * pow(max(linear, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, linear <= vec3<f32>(0.0031308));
}

fn srgb_decode(encoded: vec3<f32>) -> vec3<f32> {
    let low = encoded / 12.92;
    let high = pow((max(encoded, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, encoded <= vec3<f32>(0.04045));
}

// Writes `value` (0..1) so that it lands unchanged in an sRGB render target.
fn debug_color(value: vec3<f32>) -> vec4<f32> {
    return vec4<f32>(pow(clamp(value, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(2.2)), 1.0);
}

@fragment
fn fragment(skin_in: SkinVertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    let in = stock_vertex_output(skin_in);

#ifdef VISIBILITY_RANGE_DITHER
    visibility_range_dither(in.position, in.visibility_range_dither);
#endif

    var pbr_input = pbr_input_from_standard_material(in, is_front);
#ifdef SKYRIM_HAIR_ALPHA_TO_COVERAGE
    // A masked hair card under MSAA (`skin_material.rs`, "Hair card edges"): coverage instead of
    // the alpha test. Full at the authored cutoff, where the prepass and shadows stop, and fading
    // out over the pixel below it.
    let hair_alpha = pbr_input.material.base_color.a;
    pbr_input.material.base_color.a = saturate(
        (hair_alpha - pbr_input.material.alpha_cutoff) / max(fwidth(hair_alpha), 0.0001) + 1.0
    );
#else
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
#endif

#ifdef VERTEX_UVS_A
    if (skin.flags & FLAG_FACEGEN) != 0u {
        // Skyrim samples every texture without sRGB decoding and blends the FaceGen maps on
        // those encoded values, so Bevy's decoded base colour is re-encoded first and the tint
        // and detail maps are bound as linear (raw) images. Bevy has already multiplied the
        // base colour factor and vertex colour in, which Skyrim applies after this blend; the two
        // orders agree for the white factor and vertex colours FaceGen heads carry.
        let uv = (pbr_input.material.uv_transform * vec3<f32>(in.uv, 1.0)).xy;
        let raw_base_color = srgb_encode(pbr_input.material.base_color.rgb);
        let facegen = get_facegen_base_color(
            raw_base_color,
            uv,
            facegen_tint_texture,
            facegen_tint_sampler,
            facegen_detail_texture,
            facegen_detail_sampler,
        );
        pbr_input.material.base_color = vec4<f32>(
            srgb_decode(facegen),
            pbr_input.material.base_color.a,
        );
    }

    if (skin.flags & FLAG_SKIN_TINT) != 0u {
        // As for FaceGen above: the game blends on encoded values, and the tint colour is stored
        // encoded (`NPC_/QNAM`), so the base colour is re-encoded around the blend.
        let raw_base_color = srgb_encode(pbr_input.material.base_color.rgb);
        let tinted = get_facegen_rgb_tint_base_color(raw_base_color, skin.tint_color.rgb);
        pbr_input.material.base_color = vec4<f32>(
            srgb_decode(tinted),
            pbr_input.material.base_color.a,
        );
    }

    if (skin.flags & FLAG_MODEL_SPACE_NORMALS) != 0u {
        // The same UV and bias the base material samples its own textures with.
        let uv = (pbr_input.material.uv_transform * vec3<f32>(in.uv, 1.0)).xy;
        let normal = decode_model_space_normal(textureSampleBias(
            model_space_normal_texture,
            model_space_normal_sampler,
            uv,
            view.mip_bias,
        ));
        let tbn = mat3x3<f32>(
            skin_in.normal_from_local_x,
            skin_in.normal_from_local_y,
            skin_in.normal_from_local_z,
        );
        var world_normal = model_space_world_normal(tbn, normal.xyz);
        let double_sided =
            (pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_DOUBLE_SIDED_BIT) != 0u;
        if double_sided && !is_front {
            world_normal = -world_normal;
        }
        pbr_input.N = world_normal;
        pbr_input.clearcoat_N = world_normal;

        // An unbound `_s` samples as zero in Direct3D 11, so a material without one has no gloss.
        var glossiness = 0.0;
        if (skin.flags & FLAG_SPECULAR_MAP) != 0u {
            glossiness = model_space_glossiness(
                textureSampleBias(specular_texture, specular_sampler, uv, view.mip_bias)
            );
        }
        glossiness = material_glossiness((skin.flags & FLAG_SPECULAR) != 0u, glossiness);
        // Skyrim scales the specular term by the mask linearly. Bevy's F0 is
        // 0.16 * reflectance^2, so reflectance takes the square root to keep F0 linear in it.
        pbr_input.material.reflectance *= sqrt(glossiness);
    }
#endif

    if (skin.flags & FLAG_HAIR_TINT) != 0u {
        // A shape without vertex colours reads 1, as Skyrim's vertex shader writes for it.
        var tint_mask = 1.0;
#ifdef VERTEX_COLORS
        tint_mask = skin_in.color.g;
#endif
        // The tint is stored encoded and lerped on encoded values; `SRGBToWorking` is the decode.
        let vertex_color = srgb_decode(get_hair_vertex_color(skin.tint_color.rgb, tint_mask));
        pbr_input.material.base_color = vec4<f32>(
            pbr_input.material.base_color.rgb * vertex_color,
            pbr_input.material.base_color.a,
        );
    }

    apply_decals(&pbr_input);

    var out: FragmentOutput;
    if (skin.flags & FLAG_DEBUG_WORLD_NORMAL) != 0u {
        out.color = debug_color(pbr_input.N * 0.5 + 0.5);
        return out;
    }
    if (skin.flags & FLAG_DEBUG_VERTEX_NORMAL) != 0u {
        out.color = debug_color(normalize(pbr_input.world_normal) * 0.5 + 0.5);
        return out;
    }

    if (pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

#ifdef OIT_ENABLED
    let alpha_mode = pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS;
    if alpha_mode != pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_OPAQUE {
        oit_draw(in.position, out.color);
        discard;
    }
#endif

    return out;
}
