//! Headless contract checks for the actual effect palette forward and prepass shaders.

use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, NagaModuleDescriptor, ShaderDefValue, ShaderLanguage,
    ShaderType,
};
use std::collections::{HashMap, HashSet};

const FUNCTIONS: &str = include_str!("../shaders/effect_palette_functions.wgsl");
const MAIN: &str = include_str!("../shaders/effect_palette.wgsl");
const PREPASS: &str = include_str!("../shaders/effect_palette_prepass.wgsl");
const SHADOW_ALPHA_CUTOFF: f32 = 0.05;

const PBR_TYPES: &str = r#"
#define_import_path bevy_pbr::pbr_types
struct StandardMaterial {
    base_color: vec4<f32>,
    flags: u32,
    alpha_cutoff: f32,
    uv_transform: mat3x3<f32>,
}
struct PbrInput { material: StandardMaterial }
const STANDARD_MATERIAL_FLAGS_BASE_COLOR_TEXTURE_BIT: u32 = 1u;
const STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS: u32 = 7u << 29u;
const STANDARD_MATERIAL_FLAGS_ALPHA_MODE_OPAQUE: u32 = 0u << 29u;
const STANDARD_MATERIAL_FLAGS_ALPHA_MODE_MASK: u32 = 1u << 29u;
const STANDARD_MATERIAL_FLAGS_ALPHA_MODE_BLEND: u32 = 2u << 29u;
const STANDARD_MATERIAL_FLAGS_ALPHA_MODE_PREMULTIPLIED: u32 = 3u << 29u;
const STANDARD_MATERIAL_FLAGS_ALPHA_MODE_ADD: u32 = 4u << 29u;
const STANDARD_MATERIAL_FLAGS_ALPHA_MODE_ALPHA_TO_COVERAGE: u32 = 6u << 29u;
"#;

const PBR_BINDINGS: &str = r#"
#define_import_path bevy_pbr::pbr_bindings
#import bevy_pbr::pbr_types::StandardMaterial
@group(0) @binding(0) var<uniform> material: StandardMaterial;
@group(0) @binding(1) var base_color_texture: texture_2d<f32>;
@group(0) @binding(2) var base_color_sampler: sampler;
"#;

const MESH_VIEW_BINDINGS: &str = r#"
#define_import_path bevy_pbr::mesh_view_bindings
struct View { mip_bias: f32 }
@group(1) @binding(0) var<uniform> view: View;
"#;

const PREPASS_IO: &str = r#"
#define_import_path bevy_pbr::prepass_io
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) previous_world_position: vec4<f32>,
    @location(2) world_normal: vec3<f32>,
#ifdef VERTEX_UVS_A
    @location(3) uv: vec2<f32>,
#endif
#ifdef VERTEX_UVS_B
    @location(4) uv_b: vec2<f32>,
#endif
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
    @location(6) visibility_range_dither: i32,
}
struct FragmentOutput {
    @builtin(frag_depth) frag_depth: f32,
    @location(0) normal: vec4<f32>,
    @location(1) motion_vector: vec2<f32>,
}
"#;

const FORWARD_IO: &str = r#"
#define_import_path bevy_pbr::forward_io
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
#ifdef VERTEX_UVS_A
    @location(0) uv: vec2<f32>,
#endif
#ifdef VERTEX_UVS_B
    @location(1) uv_b: vec2<f32>,
#endif
#ifdef VERTEX_COLORS
    @location(2) color: vec4<f32>,
#endif
}
struct FragmentOutput { @location(0) color: vec4<f32> }
"#;

const PBR_FUNCTIONS: &str = r#"
#define_import_path bevy_pbr::pbr_functions
#import bevy_pbr::pbr_types::{PbrInput, StandardMaterial}
fn visibility_range_dither(position: vec4<f32>, dither: i32) {}
fn alpha_discard(material: StandardMaterial, color: vec4<f32>) -> vec4<f32> { return color; }
fn main_pass_post_lighting_processing(input: PbrInput, color: vec4<f32>) -> vec4<f32> {
    return color;
}
"#;

const PREPASS_FUNCTIONS: &str = r#"
#define_import_path bevy_pbr::pbr_prepass_functions
const PREMULTIPLIED_ALPHA_CUTOFF: f32 = 0.05;
fn calculate_motion_vector(world: vec4<f32>, previous_world: vec4<f32>) -> vec2<f32> {
    return world.xy - previous_world.xy;
}
"#;

const PBR_FRAGMENT: &str = r#"
#define_import_path bevy_pbr::pbr_fragment
#import bevy_pbr::{forward_io::VertexOutput, pbr_bindings, pbr_types::PbrInput}
fn pbr_input_from_standard_material(input: VertexOutput, is_front: bool) -> PbrInput {
    var result: PbrInput;
    result.material = pbr_bindings::material;
#ifdef VERTEX_COLORS
    result.material.base_color *= input.color;
#endif
    return result;
}
"#;

const COLOR_OPERATIONS: &str = r#"
#define_import_path bevy_render::color_operations
fn linear_to_srgb(color: vec3<f32>) -> vec3<f32> { return color; }
"#;

fn shader_defs(defs: &[&str]) -> HashMap<String, ShaderDefValue> {
    let mut result = HashMap::new();
    result.insert("MATERIAL_BIND_GROUP".to_owned(), ShaderDefValue::UInt(2));
    for name in defs {
        result.insert((*name).to_owned(), ShaderDefValue::Bool(true));
    }
    result
}

fn compose(source: &str, file_path: &str, defs: &[&str]) -> Result<wgpu::naga::Module, String> {
    let shader_defs = shader_defs(defs);
    let mut composer = Composer::default();
    for (path, module_source) in [
        ("pbr_types.wgsl", PBR_TYPES),
        ("pbr_bindings.wgsl", PBR_BINDINGS),
        ("mesh_view_bindings.wgsl", MESH_VIEW_BINDINGS),
        ("prepass_io.wgsl", PREPASS_IO),
        ("forward_io.wgsl", FORWARD_IO),
        ("pbr_functions.wgsl", PBR_FUNCTIONS),
        ("pbr_prepass_functions.wgsl", PREPASS_FUNCTIONS),
        ("pbr_fragment.wgsl", PBR_FRAGMENT),
        ("color_operations.wgsl", COLOR_OPERATIONS),
        ("effect_palette_functions.wgsl", FUNCTIONS),
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
            file_path,
            shader_type: ShaderType::Wgsl,
            shader_defs,
            additional_imports: &[],
        })
        .map_err(|error| error.emit_to_string(&composer))
}

fn collect_calls(
    block: &wgpu::naga::Block,
    module: &wgpu::naga::Module,
    calls: &mut HashSet<String>,
) {
    use wgpu::naga::Statement;

    for statement in block {
        match statement {
            Statement::Call { function, .. } => {
                if let Some(name) = module.functions[*function].name.as_ref() {
                    calls.insert(name.clone());
                }
            }
            Statement::Block(inner) => collect_calls(inner, module, calls),
            Statement::If { accept, reject, .. } => {
                collect_calls(accept, module, calls);
                collect_calls(reject, module, calls);
            }
            Statement::Switch { cases, .. } => {
                for case in cases {
                    collect_calls(&case.body, module, calls);
                }
            }
            Statement::Loop {
                body, continuing, ..
            } => {
                collect_calls(body, module, calls);
                collect_calls(continuing, module, calls);
            }
            _ => {}
        }
    }
}

fn contains_kill(block: &wgpu::naga::Block) -> bool {
    use wgpu::naga::Statement;

    block.iter().any(|statement| match statement {
        Statement::Kill => true,
        Statement::Block(inner) => contains_kill(inner),
        Statement::If { accept, reject, .. } => contains_kill(accept) || contains_kill(reject),
        Statement::Switch { cases, .. } => cases.iter().any(|case| contains_kill(&case.body)),
        Statement::Loop {
            body, continuing, ..
        } => contains_kill(body) || contains_kill(continuing),
        _ => false,
    })
}

fn function<'a>(module: &'a wgpu::naga::Module, name: &str) -> &'a wgpu::naga::Function {
    module
        .functions
        .iter()
        .find_map(|(_, function)| (function.name.as_deref() == Some(name)).then_some(function))
        .unwrap_or_else(|| panic!("composed WGSL lacks function {name}"))
}

fn entry_calls(module: &wgpu::naga::Module, name: &str) -> HashSet<String> {
    let entry = module
        .entry_points
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("composed WGSL lacks entry point {name}"));
    let mut calls = HashSet::new();
    collect_calls(&entry.function.body, module, &mut calls);
    calls
}

fn shadow_discards(mode: AlphaMode, color: [f32; 4], mask_cutoff: f32) -> bool {
    match mode {
        AlphaMode::Mask => color[3] < mask_cutoff,
        AlphaMode::Blend | AlphaMode::Add | AlphaMode::AlphaToCoverage => {
            color[3] < SHADOW_ALPHA_CUTOFF
        }
        AlphaMode::Premultiplied => color.iter().all(|channel| *channel < SHADOW_ALPHA_CUTOFF),
        AlphaMode::Opaque | AlphaMode::Unsupported => false,
    }
}

#[derive(Clone, Copy)]
enum AlphaMode {
    Opaque,
    Mask,
    Blend,
    Add,
    AlphaToCoverage,
    Premultiplied,
    Unsupported,
}

fn effect_color(
    standard_color: [f32; 4],
    palette_rgb: Option<[f32; 3]>,
    effect_alpha: f32,
    scale: f32,
) -> [f32; 4] {
    let rgb = palette_rgb.unwrap_or([standard_color[0], standard_color[1], standard_color[2]]);
    [rgb[0] * scale, rgb[1] * scale, rgb[2] * scale, effect_alpha]
}

#[test]
fn actual_palette_shaders_compose_with_shadow_discard_enabled_and_disabled() {
    let shadow = compose(
        PREPASS,
        "effect_palette_prepass.wgsl",
        &["MAY_DISCARD", "VERTEX_UVS_A", "VERTEX_COLORS"],
    )
    .unwrap_or_else(|error| panic!("shadow prepass composition failed:\n{error}"));
    let shadow_calls = entry_calls(&shadow, "fragment");
    assert!(
        shadow_calls
            .iter()
            .any(|name| name.contains("palette_alpha_discard"))
    );
    let discard = function(&shadow, "palette_alpha_discard");
    let mut discard_calls = HashSet::new();
    collect_calls(&discard.body, &shadow, &mut discard_calls);
    assert!(
        discard_calls
            .iter()
            .any(|name| name.contains("effect_color")),
        "prepass calls: {discard_calls:?}"
    );
    assert!(contains_kill(&discard.body));

    let normal_prepass = compose(
        PREPASS,
        "effect_palette_prepass.wgsl",
        &[
            "MAY_DISCARD",
            "PREPASS_FRAGMENT",
            "NORMAL_PREPASS",
            "MOTION_VECTOR_PREPASS",
            "VERTEX_UVS_A",
            "VERTEX_UVS_B",
            "STANDARD_MATERIAL_BASE_COLOR_UV_B",
            "VERTEX_COLORS",
        ],
    )
    .unwrap_or_else(|error| panic!("normal prepass composition failed:\n{error}"));
    let normal_calls = entry_calls(&normal_prepass, "fragment");
    assert!(
        normal_calls
            .iter()
            .any(|name| name.contains("palette_alpha_discard"))
    );

    let without_discard = compose(PREPASS, "effect_palette_prepass.wgsl", &["VERTEX_UVS_A"])
        .unwrap_or_else(|error| panic!("opaque shadow composition failed:\n{error}"));
    assert!(!contains_kill(
        &function(&without_discard, "palette_alpha_discard").body
    ));

    let main = compose(
        MAIN,
        "effect_palette.wgsl",
        &["VERTEX_UVS_A", "VERTEX_UVS_B", "VERTEX_COLORS"],
    )
    .unwrap_or_else(|error| panic!("forward shader composition failed:\n{error}"));
    assert!(
        entry_calls(&main, "fragment")
            .iter()
            .any(|name| name.contains("effect_color"))
    );

    compose(
        MAIN,
        "effect_palette.wgsl",
        &["VERTEX_UVS_A", "VERTEX_COLORS", "RAW_COLOUR_TEXELS"],
    )
    .unwrap_or_else(|error| panic!("raw-texel forward composition failed:\n{error}"));
    compose(
        PREPASS,
        "effect_palette_prepass.wgsl",
        &[
            "MAY_DISCARD",
            "VERTEX_UVS_A",
            "VERTEX_COLORS",
            "RAW_COLOUR_TEXELS",
        ],
    )
    .unwrap_or_else(|error| panic!("raw-texel prepass composition failed:\n{error}"));
}

#[test]
fn palette_effect_shadow_cutoffs_match_bevy_alpha_modes() {
    for mode in [AlphaMode::Blend, AlphaMode::Add] {
        assert!(shadow_discards(mode, [1.0, 1.0, 1.0, 0.0], 0.5));
        assert!(!shadow_discards(mode, [1.0, 1.0, 1.0, 0.05], 0.5));
    }
    assert!(!shadow_discards(
        AlphaMode::AlphaToCoverage,
        [1.0, 1.0, 1.0, 0.1],
        0.5,
    ));
    assert!(shadow_discards(AlphaMode::Mask, [1.0, 1.0, 1.0, 0.49], 0.5));
    assert!(!shadow_discards(AlphaMode::Mask, [1.0, 1.0, 1.0, 0.5], 0.5));
    assert!(shadow_discards(
        AlphaMode::Premultiplied,
        [0.01, 0.02, 0.03, 0.04],
        0.5,
    ));
    assert!(!shadow_discards(
        AlphaMode::Premultiplied,
        [0.2, 0.0, 0.0, 0.0],
        0.5,
    ));
    assert!(!shadow_discards(AlphaMode::Opaque, [0.0; 4], 0.5));
    assert!(!shadow_discards(AlphaMode::Unsupported, [0.0; 4], 0.5));

    let palette_dark = effect_color([0.8, 0.7, 0.6, 1.0], Some([0.04, 0.02, 0.01]), 0.0, 0.5);
    assert_eq!(palette_dark, [0.02, 0.01, 0.005, 0.0]);
    assert!(shadow_discards(AlphaMode::Premultiplied, palette_dark, 0.5));

    let source_fallback = effect_color([0.2, 0.3, 0.4, 1.0], None, 0.0, 0.5);
    assert_eq!(source_fallback, [0.1, 0.15, 0.2, 0.0]);
    assert!(!shadow_discards(
        AlphaMode::Premultiplied,
        source_fallback,
        0.5
    ));
}
