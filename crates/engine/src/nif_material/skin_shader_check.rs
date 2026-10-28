//! CPU composition and Naga validation of the production Skyrim skin WGSL.
//!
//! Bevy 0.19's renderer WGSL is a private asset registry, so the imported Bevy modules below
//! model the fields and signatures consumed by this shader. The two Mudcrab WGSL files are
//! included verbatim. This checks composition, shader fields and WGSL semantics, not Bevy's
//! runtime pipeline layout or GPU/driver execution.

use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, NagaModuleDescriptor, ShaderDefValue, ShaderLanguage,
    ShaderType,
};
use std::collections::HashMap;

const SHADER: &str = include_str!("../shaders/skyrim_skin.wgsl");
const LIGHTING: &str = include_str!("../shaders/cs_lighting_msn.wgsl");

const MESH_TYPES: &str = r#"
#define_import_path bevy_pbr::mesh_types
struct Mesh {
    current_skin_index: u32,
    local_from_world_transpose_a: mat2x4<f32>,
    local_from_world_transpose_b: f32,
    first_vertex_index: u32,
    morph_descriptor_index: u32,
}
#ifdef MORPH_TARGETS
struct MorphDescriptor {
    current_weights_offset: u32,
    prev_weights_offset: u32,
    targets_offset: u32,
    vertex_count: u32,
    weight_count: u32,
}
struct MorphAttributes {
    position: vec3<f32>,
    pad_a: f32,
    normal: vec3<f32>,
    pad_b: f32,
    tangent: vec3<f32>,
    pad_c: f32,
}
#endif
"#;
const MESH_BINDINGS: &str = r#"
#define_import_path bevy_pbr::mesh_bindings
#import bevy_pbr::mesh_types::Mesh
@group(2) @binding(0) var<storage> mesh: array<Mesh>;
"#;
const FORWARD_IO: &str = r#"
#define_import_path bevy_pbr::forward_io
struct Vertex {
    @builtin(instance_index) instance_index: u32,
#ifdef VERTEX_POSITIONS
    @location(0) position: vec3<f32>,
#endif
#ifdef VERTEX_NORMALS
    @location(1) normal: vec3<f32>,
#endif
#ifdef VERTEX_UVS_A
    @location(2) uv: vec2<f32>,
#endif
#ifdef VERTEX_UVS_B
    @location(3) uv_b: vec2<f32>,
#endif
#ifdef VERTEX_TANGENTS
    @location(4) tangent: vec4<f32>,
#endif
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
#ifdef SKINNED
    @location(6) joint_indices: vec4<u32>,
    @location(7) joint_weights: vec4<f32>,
#endif
#ifdef MORPH_TARGETS
    @builtin(vertex_index) index: u32,
#endif
}
struct VertexOutput {
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
}
struct FragmentOutput { @location(0) color: vec4<f32> }
"#;
const MESH_FUNCTIONS: &str = r#"
#define_import_path bevy_pbr::mesh_functions
#import bevy_pbr::mesh_bindings::mesh
fn get_world_from_local(instance_index: u32) -> mat4x4<f32> {
    return mat4x4<f32>(vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
}
fn mesh_normal_local_to_world(normal: vec3<f32>, instance_index: u32) -> vec3<f32> { return normalize(normal); }
fn mesh_position_local_to_world(world: mat4x4<f32>, position: vec4<f32>) -> vec4<f32> { return world * position; }
fn mesh_tangent_local_to_world(world: mat4x4<f32>, tangent: vec4<f32>, instance_index: u32) -> vec4<f32> { return tangent; }
fn get_visibility_range_dither_level(instance_index: u32, world_position: vec4<f32>) -> i32 { return 0; }
"#;
const SKINNING: &str = r#"
#define_import_path bevy_pbr::skinning
fn skin_model(indexes: vec4<u32>, weights: vec4<f32>, instance_index: u32) -> mat4x4<f32> {
    return mat4x4<f32>(vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
}
fn inverse_transpose_3x3m(input: mat3x3<f32>) -> mat3x3<f32> { return input; }
fn skin_normals(world: mat4x4<f32>, normal: vec3<f32>) -> vec3<f32> { return normal; }
"#;
const MORPH: &str = r#"
#define_import_path bevy_pbr::morph
#import bevy_pbr::mesh_types::{MorphAttributes, MorphDescriptor}
#import bevy_pbr::mesh_bindings::mesh
@group(2) @binding(2) var<storage> morph_weights: array<f32>;
@group(2) @binding(3) var<storage> morph_targets: array<MorphAttributes>;
@group(2) @binding(8) var<storage> morph_descriptors: array<MorphDescriptor>;
fn layer_count(instance_index: u32) -> u32 {
    let descriptor = mesh[instance_index].morph_descriptor_index;
    return morph_descriptors[descriptor].weight_count;
}
fn weight_at(weight_index: u32, instance_index: u32) -> f32 {
    let descriptor = mesh[instance_index].morph_descriptor_index;
    let offset = morph_descriptors[descriptor].current_weights_offset;
    return morph_weights[offset + weight_index];
}
fn morph_position(vertex_index: u32, weight_index: u32, instance_index: u32) -> vec3<f32> {
    let descriptor = mesh[instance_index].morph_descriptor_index;
    let target_index = morph_descriptors[descriptor].targets_offset + weight_index * morph_descriptors[descriptor].vertex_count + vertex_index;
    return morph_targets[target_index].position;
}
fn morph_normal(vertex_index: u32, weight_index: u32, instance_index: u32) -> vec3<f32> {
    let descriptor = mesh[instance_index].morph_descriptor_index;
    let target_index = morph_descriptors[descriptor].targets_offset + weight_index * morph_descriptors[descriptor].vertex_count + vertex_index;
    return morph_targets[target_index].normal;
}
fn morph_tangent(vertex_index: u32, weight_index: u32, instance_index: u32) -> vec3<f32> {
    let descriptor = mesh[instance_index].morph_descriptor_index;
    let target_index = morph_descriptors[descriptor].targets_offset + weight_index * morph_descriptors[descriptor].vertex_count + vertex_index;
    return morph_targets[target_index].tangent;
}
"#;
const MATHS: &str = r#"
#define_import_path bevy_render::maths
fn mat2x4_f32_to_mat3x3_unpack(a: mat2x4<f32>, b: f32) -> mat3x3<f32> {
    return mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
}
"#;
const MESH_VIEW: &str = r#"
#define_import_path bevy_pbr::mesh_view_bindings
struct View { mip_bias: f32 }
@group(1) @binding(0) var<uniform> view: View;
"#;
const VIEW_TRANSFORMATIONS: &str = r#"
#define_import_path bevy_pbr::view_transformations
fn position_world_to_clip(world: vec3<f32>) -> vec4<f32> { return vec4<f32>(world, 1.0); }
"#;
const PBR_TYPES: &str = r#"
#define_import_path bevy_pbr::pbr_types
struct StandardMaterial { base_color: vec4<f32>, uv_transform: mat3x3<f32>, reflectance: vec3<f32>, flags: u32 }
struct PbrInput { material: StandardMaterial, N: vec3<f32>, clearcoat_N: vec3<f32>, world_normal: vec3<f32> }
const STANDARD_MATERIAL_FLAGS_UNLIT_BIT: u32 = 1u << 5u;
const STANDARD_MATERIAL_FLAGS_DOUBLE_SIDED_BIT: u32 = 1u << 4u;
const STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS: u32 = 7u << 29u;
const STANDARD_MATERIAL_FLAGS_ALPHA_MODE_OPAQUE: u32 = 0u << 29u;
"#;
const PBR_FUNCTIONS: &str = r#"
#define_import_path bevy_pbr::pbr_functions
#import bevy_pbr::pbr_types::{PbrInput, StandardMaterial}
fn alpha_discard(material: StandardMaterial, color: vec4<f32>) -> vec4<f32> { return color; }
fn apply_pbr_lighting(input: PbrInput) -> vec4<f32> { return input.material.base_color; }
fn main_pass_post_lighting_processing(input: PbrInput, color: vec4<f32>) -> vec4<f32> { return color; }
fn visibility_range_dither(position: vec4<f32>, dither: i32) {}
"#;
const PBR_FRAGMENT: &str = r#"
#define_import_path bevy_pbr::pbr_fragment
#import bevy_pbr::{forward_io::VertexOutput, pbr_types::PbrInput}
fn pbr_input_from_standard_material(input: VertexOutput, is_front: bool) -> PbrInput {
    var result: PbrInput;
    result.material.base_color = vec4<f32>(1.0);
    result.material.uv_transform = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    result.material.reflectance = vec3<f32>(0.5);
    result.material.flags = 0u;
    result.N = input.world_normal;
    result.clearcoat_N = input.world_normal;
    result.world_normal = input.world_normal;
    return result;
}
"#;
const DECALS: &str = r#"
#define_import_path bevy_pbr::decal::clustered
#import bevy_pbr::pbr_types::PbrInput
fn apply_decals(input: ptr<function, PbrInput>) {}
"#;
const OIT: &str = r#"
#define_import_path bevy_core_pipeline::oit
fn oit_draw(position: vec4<f32>, color: vec4<f32>) {}
"#;

fn shader_defs(skinned: bool, morphed: bool) -> HashMap<String, ShaderDefValue> {
    let mut defs = HashMap::new();
    defs.insert("MATERIAL_BIND_GROUP".into(), ShaderDefValue::UInt(3));
    for name in [
        "VERTEX_POSITIONS",
        "VERTEX_NORMALS",
        "VERTEX_UVS_A",
        "VERTEX_UVS_B",
        "VERTEX_TANGENTS",
        "VERTEX_COLORS",
        "VERTEX_OUTPUT_INSTANCE_INDEX",
        "OIT_ENABLED",
    ] {
        defs.insert(name.into(), ShaderDefValue::Bool(true));
    }
    if skinned {
        defs.insert("SKINNED".into(), ShaderDefValue::Bool(true));
    }
    if morphed {
        defs.insert("MORPH_TARGETS".into(), ShaderDefValue::Bool(true));
    }
    defs
}

fn compose(source: &str, skinned: bool, morphed: bool) -> Result<wgpu::naga::Module, String> {
    let shader_defs = shader_defs(skinned, morphed);
    let mut composer = Composer::default();
    for (path, module_source) in [
        ("cs_lighting_msn.wgsl", LIGHTING),
        ("mesh_types.wgsl", MESH_TYPES),
        ("mesh_bindings.wgsl", MESH_BINDINGS),
        ("forward_io.wgsl", FORWARD_IO),
        ("mesh_functions.wgsl", MESH_FUNCTIONS),
        ("skinning.wgsl", SKINNING),
        ("morph.wgsl", MORPH),
        ("maths.wgsl", MATHS),
        ("mesh_view_bindings.wgsl", MESH_VIEW),
        ("view_transformations.wgsl", VIEW_TRANSFORMATIONS),
        ("pbr_types.wgsl", PBR_TYPES),
        ("pbr_functions.wgsl", PBR_FUNCTIONS),
        ("pbr_fragment.wgsl", PBR_FRAGMENT),
        ("decals.wgsl", DECALS),
        ("oit.wgsl", OIT),
    ] {
        composer
            .add_composable_module(ComposableModuleDescriptor {
                source: module_source,
                file_path: path,
                language: ShaderLanguage::Wgsl,
                shader_defs: shader_defs.clone(),
                ..Default::default()
            })
            .map_err(|error| format!("{path}: {error:?}"))?;
    }
    composer
        .make_naga_module(NagaModuleDescriptor {
            source,
            file_path: "skyrim_skin.wgsl",
            shader_type: ShaderType::Wgsl,
            shader_defs,
            additional_imports: &[],
        })
        .map_err(|error| error.emit_to_string(&composer))
}

#[test]
fn production_skin_shader_composes_for_static_and_skinned_meshes() {
    for (skinned, morphed) in [(false, false), (true, false), (false, true), (true, true)] {
        let source = if morphed {
            SHADER.to_owned()
        } else {
            SHADER.replace("#ifdef MORPH_TARGETS", "#ifdef DISABLED_MORPH_TARGETS")
        };
        let module = compose(&source, skinned, morphed).unwrap_or_else(|error| {
            panic!(
                "{}{} mesh shader failed:\n{error}",
                if skinned { "skinned" } else { "static" },
                if morphed { " morphed" } else { "" }
            )
        });
        assert_facegen_bindings(&module);
    }
}

fn assert_facegen_bindings(module: &wgpu::naga::Module) {
    for (name, expected) in [
        ("facegen_tint_texture", 105),
        ("facegen_tint_sampler", 106),
        ("facegen_detail_texture", 107),
        ("facegen_detail_sampler", 108),
    ] {
        let binding = module
            .global_variables
            .iter()
            .find_map(|(_, variable)| {
                (variable.name.as_deref() == Some(name)).then_some(variable.binding.as_ref())
            })
            .flatten()
            .unwrap_or_else(|| panic!("missing FaceGen binding {name}"));
        assert_eq!(
            binding.binding, expected,
            "wrong FaceGen binding for {name}"
        );
    }
}

#[test]
fn missing_skinned_vertex_field_fails_composition() {
    let broken = SHADER.replace("vertex.joint_indices", "vertex.missing_joint_indices");
    assert_ne!(broken, SHADER);
    let error = compose(&broken, true, false).expect_err("missing skinned vertex field must fail");
    assert!(
        error.contains("missing_joint_indices"),
        "unexpected error: {error}"
    );
}

#[test]
fn missing_facegen_sampler_symbol_fails_composition() {
    let broken = SHADER.replace(
        "var facegen_detail_sampler: sampler;",
        "var facegen_missing_sampler: sampler;",
    );
    assert_ne!(broken, SHADER);
    let error = compose(&broken, false, false).expect_err("missing FaceGen sampler must fail");
    assert!(
        error.contains("facegen_detail_sampler"),
        "unexpected error: {error}"
    );
}
