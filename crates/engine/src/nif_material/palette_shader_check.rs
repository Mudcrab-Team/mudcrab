//! Headless contract checks for the actual effect palette forward and prepass shaders.

use naga_oil::compose::comment_strip_iter::CommentReplaceExt;
use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, NagaModuleDescriptor, ShaderDefValue, ShaderLanguage,
    ShaderType,
};
use std::collections::{HashMap, HashSet};

const FUNCTIONS: &str = include_str!("../shaders/effect_palette_functions.wgsl");
const MAIN: &str = include_str!("../shaders/effect_palette.wgsl");
const PREPASS: &str = include_str!("../shaders/effect_palette_prepass.wgsl");
const SHADOW_ALPHA_CUTOFF: f32 = 0.05;
const SCALAR_CUTOFF_CHECK: &str = "if color.a < PREMULTIPLIED_ALPHA_CUTOFF {";
const ALL_CHANNELS_CUTOFF_CHECK: &str = "if all(color < vec4<f32>(PREMULTIPLIED_ALPHA_CUTOFF)) {";

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

#[test]
fn prepass_mirrors_bevys_premultiplied_alpha_cutoff_locally() {
    // naga_oil substitutes an imported `pbr_prepass_functions` constant with its qualified name
    // but never emits the definition, so an imported cutoff only fails when a real pipeline
    // compiles against Bevy's shader. The stub module above accepts it, which is why the
    // composition tests cannot see it.
    assert!(
        !PREPASS.contains("pbr_prepass_functions::PREMULTIPLIED_ALPHA_CUTOFF"),
        "importing the cutoff from pbr_prepass_functions fails at pipeline creation"
    );
    let declaration = PREPASS
        .lines()
        .find(|line| {
            line.trim_start()
                .starts_with("const PREMULTIPLIED_ALPHA_CUTOFF")
        })
        .expect("the prepass shader declares its own alpha cutoff");
    assert_eq!(
        declaration.trim(),
        format!("const PREMULTIPLIED_ALPHA_CUTOFF: f32 = {SHADOW_ALPHA_CUTOFF};")
    );
}

/// Source with WGSL comments blanked to whitespace, so a commented-out line cannot satisfy a guard.
fn active_source(source: &str) -> String {
    let mut lines = source.lines();
    let mut active = String::new();
    for line in lines.replace_comments() {
        active.push_str(&line);
        active.push('\n');
    }
    active
}

/// The `{ ... }` body that follows `marker` in comment-free source, without its braces.
fn branch_body<'a>(active: &'a str, marker: &str) -> Result<&'a str, String> {
    let start = active
        .find(marker)
        .ok_or_else(|| format!("the active shader lacks {marker}"))?;
    let open = start + active[start..].find('{').ok_or("the branch lacks a body")?;
    let mut depth = 0;
    for (offset, character) in active[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&active[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    Err(format!("the body after {marker} is not closed"))
}

/// The comment-free body of the called `palette_alpha_discard`, without its braces. Exactly one
/// definition must exist, so an unused copy elsewhere in the file cannot stand in for it.
fn discard_body(active: &str) -> Result<&str, String> {
    const HEADER: &str = "fn palette_alpha_discard(";
    if active.matches(HEADER).count() != 1 {
        return Err(format!("the active shader must define `{HEADER}` once"));
    }
    branch_body(active, HEADER)
}

/// Each local-cutoff comparison must be active code inside its own alpha-mode branch of the
/// called `palette_alpha_discard`.
fn local_cutoff_sites(source: &str) -> Result<(), String> {
    let active = active_source(source);
    let function = discard_body(&active)?;
    let scalar_body = branch_body(function, "STANDARD_MATERIAL_FLAGS_ALPHA_MODE_BLEND")?;
    if !scalar_body.contains(SCALAR_CUTOFF_CHECK) {
        return Err(format!(
            "the BLEND/ADD/ALPHA_TO_COVERAGE body lacks `{SCALAR_CUTOFF_CHECK}`"
        ));
    }
    let all_channels_body =
        branch_body(function, "STANDARD_MATERIAL_FLAGS_ALPHA_MODE_PREMULTIPLIED")?;
    if !all_channels_body.contains(ALL_CHANNELS_CUTOFF_CHECK) {
        return Err(format!(
            "the PREMULTIPLIED body lacks `{ALL_CHANNELS_CUTOFF_CHECK}`"
        ));
    }
    Ok(())
}

#[test]
fn prepass_applies_the_local_cutoff_at_each_comparison_site() {
    // The test above shows the declaration exists, not where it is used. Pin the scalar guard
    // inside the BLEND/ADD/ALPHA_TO_COVERAGE branch and the all-channel guard inside the
    // PREMULTIPLIED branch, so a literal or a different cutoff at either site fails here.
    let active = active_source(PREPASS);
    let function = discard_body(&active).unwrap_or_else(|error| panic!("{error}"));
    let blend = function
        .find("STANDARD_MATERIAL_FLAGS_ALPHA_MODE_BLEND ||")
        .expect("the prepass shader branches on BLEND");
    let scalar = function
        .find(SCALAR_CUTOFF_CHECK)
        .expect("the BLEND branch compares alpha to the local cutoff");
    let premultiplied = function
        .find("STANDARD_MATERIAL_FLAGS_ALPHA_MODE_PREMULTIPLIED {")
        .expect("the prepass shader branches on PREMULTIPLIED");
    let all_channels = function
        .find(ALL_CHANNELS_CUTOFF_CHECK)
        .expect("the PREMULTIPLIED branch compares every channel to the local cutoff");
    assert!(
        blend < scalar && scalar < premultiplied && premultiplied < all_channels,
        "each cutoff comparison must sit in its own alpha-mode branch, in order"
    );
    local_cutoff_sites(PREPASS).unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn prepass_cutoff_guard_rejects_literals_behind_commented_originals() {
    // Each mutant keeps the correct comparison in a line or block comment and makes one active
    // site a literal. compose() must accept each mutant, so a rejection cannot be a syntax error.
    let mut mutants = Vec::new();
    for (original, literal) in [
        (SCALAR_CUTOFF_CHECK, "if color.a < 0.5 {"),
        (
            ALL_CHANNELS_CUTOFF_CHECK,
            "if all(color < vec4<f32>(0.5)) {",
        ),
    ] {
        assert_eq!(
            PREPASS.matches(original).count(),
            1,
            "{original} occurs once"
        );
        for comment in [format!("// {original}"), format!("/* {original} */")] {
            let replacement = format!("{comment}\n        {literal}");
            mutants.push(PREPASS.replacen(original, &replacement, 1));
        }
    }
    // The exact BLEND comparison moves into the PREMULTIPLIED body, and BLEND keeps a literal.
    mutants.push(
        PREPASS
            .replacen(SCALAR_CUTOFF_CHECK, "if color.a < 0.5 {", 1)
            .replacen(
                "STANDARD_MATERIAL_FLAGS_ALPHA_MODE_PREMULTIPLIED {",
                "STANDARD_MATERIAL_FLAGS_ALPHA_MODE_PREMULTIPLIED {\n        if color.a < PREMULTIPLIED_ALPHA_CUTOFF {\n            discard;\n        }",
                1,
            ),
    );

    assert_eq!(local_cutoff_sites(PREPASS), Ok(()));
    for mutant in &mutants {
        compose(
            mutant,
            "effect_palette_prepass.wgsl",
            &["MAY_DISCARD", "VERTEX_UVS_A", "VERTEX_COLORS"],
        )
        .unwrap_or_else(|error| panic!("mutant must stay valid WGSL:\n{error}"));
        assert!(
            local_cutoff_sites(mutant).is_err(),
            "the guard accepted a literal cutoff behind a commented original"
        );
    }
}

#[test]
fn prepass_cutoff_guard_rejects_literals_beside_an_unused_correct_copy() {
    // The called discard uses literal cutoffs, and an unused earlier copy keeps the correct
    // comparisons. The copy satisfies a whole-file search, so the scoped guard must reject the
    // mutant. compose() must still accept it, so the rejection is not a syntax error.
    let active = active_source(PREPASS);
    let original_body = discard_body(&active).unwrap_or_else(|error| panic!("{error}"));
    let decoy = format!(
        "fn old_palette_alpha_discard(in: prepass_io::VertexOutput) {{{original_body}}}\n\n"
    );
    let literal = PREPASS
        .replacen(SCALAR_CUTOFF_CHECK, "if color.a < 0.5 {", 1)
        .replacen(
            ALL_CHANNELS_CUTOFF_CHECK,
            "if all(color < vec4<f32>(0.5)) {",
            1,
        );
    let mutant = literal.replacen(
        "fn palette_alpha_discard(",
        &format!("{decoy}fn palette_alpha_discard("),
        1,
    );

    let mutant_active = active_source(&mutant);
    assert!(
        mutant_active.contains(SCALAR_CUTOFF_CHECK)
            && mutant_active.contains(ALL_CHANNELS_CUTOFF_CHECK),
        "the unused copy must satisfy a whole-file search"
    );
    compose(
        &mutant,
        "effect_palette_prepass.wgsl",
        &["MAY_DISCARD", "VERTEX_UVS_A", "VERTEX_COLORS"],
    )
    .unwrap_or_else(|error| panic!("decoy mutant must stay valid WGSL:\n{error}"));
    assert!(
        local_cutoff_sites(&mutant).is_err(),
        "the guard accepted literal cutoffs in the called discard beside a correct unused copy"
    );
}
