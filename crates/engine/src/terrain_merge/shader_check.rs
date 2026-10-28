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
struct StandardMaterial { base_color: vec4<f32>, flags: u32, reflectance: vec3<f32> }
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
#ifdef VERTEX_TANGENTS
    @location(3) world_tangent: vec4<f32>,
#endif
#ifdef VERTEX_UVS_B
    @location(4) uv_b: vec2<f32>,
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
fn compose_source(source: &str, defs: Vec<bevy::shader::ShaderDefVal>) -> Result<(), String> {
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
            source,
            file_path: "terrain_merged.wgsl",
            shader_type: ShaderType::Wgsl,
            shader_defs,
            additional_imports: &[],
        })
        .map(|_| ())
        .map_err(|error| error.emit_to_string(&composer))
}

/// Compile the merged path with its production specialisation flags.
fn compose(defs: Vec<bevy::shader::ShaderDefVal>) -> Result<(), String> {
    compose_source(SHADER, defs)
}

/// Build one production terrain class key for compilation coverage.
fn key(diffuse_classes: u8, normal_classes: u8) -> TerrainClassKey {
    TerrainClassKey {
        diffuse_classes,
        normal_classes,
    }
}

/// Both vertex strides compile with the usual vanilla texture classes.
#[test]
fn the_shader_compiles_for_both_strides_with_a_vanilla_class_set() {
    for wide in [false, true] {
        compose(shader_defs(key(4, 2), wide)).unwrap_or_else(|error| {
            panic!("stride {} failed:\n{error}", if wide { 16 } else { 8 })
        });
    }
}

/// Every supported texture-class count compiles, including no normal maps.
#[test]
fn the_shader_compiles_for_the_smallest_and_the_largest_class_sets() {
    let most = MAX_CLASSES_PER_USAGE as u8;
    for wide in [false, true] {
        for (diffuse, normal) in (1..=most).flat_map(|d| (0..=most).map(move |n| (d, n))) {
            compose(shader_defs(key(diffuse, normal), wide)).unwrap_or_else(|error| {
                panic!("{diffuse} diffuse and {normal} normal classes, wide {wide}:\n{error}")
            });
        }
    }
}

/// The quadrant path compiles with each optional weight carrier and normal-map combination.
#[test]
fn quadrant_shader_compiles_with_every_weight_carrier() {
    for tangents in [false, true] {
        for uv_b in [false, true] {
            let defs = [("VERTEX_TANGENTS", tangents), ("VERTEX_UVS_B", uv_b)]
                .into_iter()
                .filter(|(_, enabled)| *enabled)
                .map(|(name, _)| bevy::shader::ShaderDefVal::Bool(name.to_owned(), true))
                .collect();
            compose_source(include_str!("../shaders/terrain.wgsl"), defs).unwrap();
        }
    }
}

/// Evaluate the small pure material-parameter function in the actual WGSL, without a GPU.
/// Absence means the old shader's unmodified material reflectance, so the baseline fails on
/// a numerical assertion rather than a parser or compilation error.
fn shader_reflectance(source: &str, reflectance: [f32; 3], mask_sum: f32, total: f32) -> Vec<f32> {
    use wgpu::naga::{Expression, Literal, MathFunction, Statement};

    /// Interpret just the scalar/vector arithmetic used by the parameter function.
    fn evaluate(
        function: &wgpu::naga::Function,
        expression: wgpu::naga::Handle<Expression>,
        arguments: &[Vec<f32>],
    ) -> Vec<f32> {
        let eval = |handle| evaluate(function, handle, arguments);
        match function.expressions[expression] {
            Expression::Literal(Literal::F32(value)) => vec![value],
            Expression::FunctionArgument(index) => arguments[index as usize].clone(),
            Expression::Binary { op, left, right } => {
                let left = eval(left);
                let right = eval(right);
                (0..left.len().max(right.len()))
                    .map(|i| match op {
                        wgpu::naga::BinaryOperator::Multiply => {
                            left[i % left.len()] * right[i % right.len()]
                        }
                        wgpu::naga::BinaryOperator::Divide => {
                            left[i % left.len()] / right[i % right.len()]
                        }
                        _ => panic!("unsupported parameter operator {op:?}"),
                    })
                    .collect()
            }
            Expression::Math {
                fun: MathFunction::Sqrt,
                arg,
                ..
            } => eval(arg).into_iter().map(f32::sqrt).collect(),
            Expression::Math {
                fun: MathFunction::Clamp,
                arg,
                arg1: Some(low),
                arg2: Some(high),
                ..
            } => {
                let low = eval(low)[0];
                let high = eval(high)[0];
                eval(arg)
                    .into_iter()
                    .map(|value| value.clamp(low, high))
                    .collect()
            }
            ref other => panic!("unsupported parameter expression {other:?}"),
        }
    }

    let Some(start) = source.find("fn terrain_reflectance(") else {
        return reflectance.to_vec();
    };
    let end = start + source[start..].find("\n}").unwrap() + 2;
    let module = wgpu::naga::front::wgsl::parse_str(&source[start..end]).unwrap();
    let (_, function) = module.functions.iter().next().unwrap();
    let result = function
        .body
        .iter()
        .find_map(|statement| match statement {
            Statement::Return { value } => *value,
            _ => None,
        })
        .unwrap();
    evaluate(
        function,
        result,
        &[reflectance.to_vec(), vec![mask_sum], vec![total]],
    )
}

/// Normal alpha scales dielectric F0 linearly; applying it to reflectance directly would square
/// the mask. Asymmetric RGB and non-unit totals distinguish both mistakes and overpaint handling.
#[test]
fn terrain_specular_mask_scales_f0_instead_of_squaring_it() {
    for source in [SHADER, include_str!("../shaders/terrain.wgsl")] {
        for (mask_sum, total, expected_f0) in [
            (0.0, 1.0, [0.0, 0.0, 0.0]),
            (0.25, 1.0, [0.01, 0.0025, 0.0225]),
            (0.75, 1.5, [0.02, 0.005, 0.045]),
            (1.0, 1.0, [0.04, 0.01, 0.09]),
            (1.2, 1.0, [0.04, 0.01, 0.09]),
        ] {
            let actual = shader_reflectance(source, [0.5, 0.25, 0.75], mask_sum, total);
            for (reflectance, expected) in actual.into_iter().zip(expected_f0) {
                assert!(
                    (0.16 * reflectance * reflectance - expected).abs() < 1.0e-6,
                    "mask_sum={mask_sum}, total={total}, expected F0={expected}, reflectance={reflectance}"
                );
            }
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
