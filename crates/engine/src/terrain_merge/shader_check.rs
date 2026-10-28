//! Compiles `terrain_merged.wgsl` with naga for every specialisation the engine can ask for.
//!
//! Bevy builds shaders at pipeline creation, on a GPU. This test composes the same source with the
//! shader defs [`super::material::shader_defs`] produces, against stand-ins for the handful of Bevy
//! modules the shader imports (their real signatures, trivial bodies), and runs naga's validator
//! over the result: a typo, a binding the defs do not declare, a texture used with the wrong
//! sampling function or a stride the shader cannot express fails here. It does not replace one run
//! on a device, which also checks the pipeline layout; that is reported separately.

use super::{
    classes::{MAX_CLASSES_PER_USAGE, TerrainClassKey},
    material::shader_defs,
};
use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, NagaModuleDescriptor, ShaderDefValue, ShaderLanguage,
    ShaderType,
};
use std::collections::HashMap;

const SHADER: &str = include_str!("terrain_merged.wgsl");

const PBR_TYPES: &str = "
#define_import_path bevy_pbr::pbr_types
struct StandardMaterial { base_color: vec4<f32>, flags: u32 }
const STANDARD_MATERIAL_FLAGS_UNLIT_BIT: u32 = 32u;
struct PbrInput {
    material: StandardMaterial,
    world_normal: vec3<f32>,
    N: vec3<f32>,
}
";

const FORWARD_IO: &str = "
#define_import_path bevy_pbr::forward_io
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
#ifdef VERTEX_UVS_A
    @location(2) uv: vec2<f32>,
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
struct FragmentOutput {
    @location(0) color: vec4<f32>,
}
";

const MESH_FUNCTIONS: &str = "
#define_import_path bevy_pbr::mesh_functions
fn get_world_from_local(instance_index: u32) -> mat4x4<f32> {
    return mat4x4<f32>(
        vec4<f32>(1.0, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, 1.0, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 1.0, 0.0),
        vec4<f32>(0.0, 0.0, 0.0, 1.0));
}
fn mesh_normal_local_to_world(vertex_normal: vec3<f32>, instance_index: u32) -> vec3<f32> {
    return vertex_normal;
}
fn mesh_position_local_to_world(world_from_local: mat4x4<f32>, vertex_position: vec4<f32>) -> vec4<f32> {
    return world_from_local * vertex_position;
}
fn get_visibility_range_dither_level(instance_index: u32, world_position: vec4<f32>) -> i32 {
    return 0;
}
";

const PBR_FRAGMENT: &str = "
#define_import_path bevy_pbr::pbr_fragment
#import bevy_pbr::{forward_io::VertexOutput, pbr_types::PbrInput}
fn pbr_input_from_standard_material(in: VertexOutput, is_front: bool) -> PbrInput {
    var pbr_input: PbrInput;
    pbr_input.world_normal = in.world_normal;
    pbr_input.N = in.world_normal;
    pbr_input.material.base_color = vec4<f32>(1.0);
    return pbr_input;
}
";

const PBR_FUNCTIONS: &str = "
#define_import_path bevy_pbr::pbr_functions
#import bevy_pbr::pbr_types::{PbrInput, StandardMaterial}
fn alpha_discard(material: StandardMaterial, output_color: vec4<f32>) -> vec4<f32> {
    return output_color;
}
fn apply_pbr_lighting(in: PbrInput) -> vec4<f32> {
    return in.material.base_color * vec4<f32>(in.N, 1.0);
}
fn main_pass_post_lighting_processing(input: PbrInput, output_color: vec4<f32>) -> vec4<f32> {
    return output_color;
}
";

const VIEW_TRANSFORMATIONS: &str = "
#define_import_path bevy_pbr::view_transformations
fn position_world_to_clip(world_pos: vec3<f32>) -> vec4<f32> {
    return vec4<f32>(world_pos, 1.0);
}
";

/// Composes the shader with `defs` plus the defs Bevy's own mesh pipeline adds for a mesh with
/// positions, normals, UVs and colours.
fn compose(defs: Vec<bevy::shader::ShaderDefVal>) -> Result<(), String> {
    let mut composer = Composer::default();
    let mut shader_defs: HashMap<String, ShaderDefValue> = HashMap::new();
    for name in [
        "VERTEX_UVS_A",
        "VERTEX_COLORS",
        "VERTEX_OUTPUT_INSTANCE_INDEX",
        "VISIBILITY_RANGE_DITHER",
    ] {
        shader_defs.insert(name.to_owned(), ShaderDefValue::Bool(true));
    }
    shader_defs.insert("MATERIAL_BIND_GROUP".to_owned(), ShaderDefValue::UInt(3));
    for def in defs {
        match def {
            bevy::shader::ShaderDefVal::Bool(name, value) => {
                shader_defs.insert(name, ShaderDefValue::Bool(value));
            }
            other => panic!("unexpected shader def {other:?}"),
        }
    }
    for (path, source) in [
        ("pbr_types.wgsl", PBR_TYPES),
        ("forward_io.wgsl", FORWARD_IO),
        ("mesh_functions.wgsl", MESH_FUNCTIONS),
        ("pbr_fragment.wgsl", PBR_FRAGMENT),
        ("pbr_functions.wgsl", PBR_FUNCTIONS),
        ("view_transformations.wgsl", VIEW_TRANSFORMATIONS),
    ] {
        composer
            .add_composable_module(ComposableModuleDescriptor {
                source,
                file_path: path,
                language: ShaderLanguage::Wgsl,
                shader_defs: shader_defs.clone(),
                ..Default::default()
            })
            .map_err(|error| format!("{path}: {error:?}"))?;
    }
    composer
        .make_naga_module(NagaModuleDescriptor {
            source: SHADER,
            file_path: "terrain_merged.wgsl",
            shader_type: ShaderType::Wgsl,
            shader_defs,
            additional_imports: &[],
        })
        .map(|_| ())
        .map_err(|error| error.emit_to_string(&composer))
}

fn key(diffuse_classes: u8, normal_classes: u8) -> TerrainClassKey {
    TerrainClassKey {
        diffuse_classes,
        normal_classes,
    }
}

#[test]
fn the_shader_compiles_for_both_strides_with_a_vanilla_class_set() {
    for wide in [false, true] {
        compose(shader_defs(key(4, 2), wide)).unwrap_or_else(|error| {
            panic!("stride {} failed:\n{error}", if wide { 16 } else { 8 })
        });
    }
}

#[test]
fn the_shader_compiles_for_the_smallest_and_the_largest_class_sets() {
    let most = MAX_CLASSES_PER_USAGE as u8;
    for wide in [false, true] {
        for (diffuse, normal) in [(1, 0), (1, 1), (most, most), (most, 0), (2, most)] {
            compose(shader_defs(key(diffuse, normal), wide)).unwrap_or_else(|error| {
                panic!("{diffuse} diffuse and {normal} normal classes, wide {wide}:\n{error}")
            });
        }
    }
}

/// The quadrant path's meshes carry tangents, so Bevy never flips their normal on a back face; the
/// merged mesh has none, so the shader must report every fragment as a front face to get the same
/// lighting. The GPU probe renders a turned-over cell to check the result; this pins the source.
#[test]
fn back_faces_are_not_flipped_to_match_the_quadrant_path() {
    assert!(SHADER.contains("pbr_input_from_standard_material(standard_in, true)"));
    assert!(!SHADER.contains("front_facing"));
}
