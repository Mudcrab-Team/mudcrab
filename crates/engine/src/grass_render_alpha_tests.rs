/// Which actual shader expression the synthetic texel should reach after vertex tinting.
#[derive(Clone, Copy, Debug)]
enum AlphaFixturePath {
    Tint,
    Fragment,
    Prepass,
}

/// Evaluate the small scalar/vector subset used by the extracted tint and opacity code.
/// Unsupported operations panic so shader changes cannot silently escape the fixture.
fn alpha_fixture_expression(
    function: &wgpu::naga::Function,
    handle: wgpu::naga::Handle<wgpu::naga::Expression>,
    arguments: &[Vec<f32>],
    locals: &[Option<Vec<f32>>],
    emitted: &[Option<Vec<f32>>],
) -> Vec<f32> {
    use wgpu::naga::{BinaryOperator, Expression as E, Literal};
    if let Some(value) = &emitted[handle.index()] {
        return value.clone();
    }
    let eval = |handle| alpha_fixture_expression(function, handle, arguments, locals, emitted);
    match &function.expressions[handle] {
        E::Literal(Literal::F32(value)) => vec![*value],
        E::FunctionArgument(index) => arguments[*index as usize].clone(),
        E::LocalVariable(local) => locals[local.index()]
            .clone()
            .expect("fixture local must be assigned before reading"),
        E::Load { pointer } => eval(*pointer),
        E::AccessIndex { base, index } => vec![eval(*base)[*index as usize]],
        E::Splat { size, value } => vec![eval(*value)[0]; *size as usize],
        E::Compose { components, .. } => components.iter().flat_map(|part| eval(*part)).collect(),
        E::Swizzle {
            size,
            vector,
            pattern,
        } => {
            let vector = eval(*vector);
            pattern[..*size as usize]
                .iter()
                .map(|component| vector[*component as usize])
                .collect()
        }
        E::Binary {
            op: BinaryOperator::Multiply,
            left,
            right,
        } => {
            let (left, right) = (eval(*left), eval(*right));
            let lanes = left.len().max(right.len());
            assert!(left.len() == lanes || left.len() == 1);
            assert!(right.len() == lanes || right.len() == 1);
            (0..lanes)
                .map(|lane| left[lane % left.len()] * right[lane % right.len()])
                .collect()
        }
        expression => panic!("unsupported tint/opacity expression: {expression:?}"),
    }
}

/// Parse, validate and execute source extracted from the production vertex and pass code.
/// Uniforms/attributes and sampling use synthetic inputs. Normal/lighting setup is
/// outside this opacity fixture and must leave the sampled color unchanged.
fn shader_alpha_fixture(
    material: [f32; 4],
    brightness: f32,
    vertex: [f32; 4],
    texel: [f32; 4],
    path: AlphaFixturePath,
) -> Vec<f32> {
    use wgpu::naga::{Expression, Statement};
    let shader = include_str!("shaders/grass.wgsl");
    let tint = shader
        .split_once("    out.color =")
        .expect("vertex tint assignment")
        .1
        .split_once("    out.normal =")
        .expect("end of vertex tint block")
        .0;
    let tint = format!("out.color ={tint}")
        .replace("#ifdef COLOR", "")
        .replace("#endif", "")
        .replace("grass.color", "material")
        .replace("input.color", "vertex_color")
        .replace("out.color", "tint");
    assert!(!tint.contains('#'), "unhandled vertex shader conditional");
    let result = match path {
        AlphaFixturePath::Tint => "return tint;".to_owned(),
        AlphaFixturePath::Fragment | AlphaFixturePath::Prepass => {
            let (entry, end, result) = match path {
                AlphaFixturePath::Fragment => ("fn fragment(", "    var out:Fragment;", "base.a"),
                AlphaFixturePath::Prepass => ("fn prepass(", "    return coverage(", "alpha"),
                AlphaFixturePath::Tint => unreachable!(),
            };
            let body = shader.split_once(entry).expect("shader pass").1;
            let body = body.split_once('{').expect("pass body").1;
            let body = if matches!(path, AlphaFixturePath::Fragment) {
                let start = body.find("    let base =").expect("fragment opacity sample");
                assert!(
                    !body[..start].contains("input.color"),
                    "fragment setup must not rewrite the sampled color"
                );
                &body[start..]
            } else {
                body
            };
            let sampling = body.split_once(end).expect("end of sampling expressions").0;
            let sampling = sampling
                .replace("textureSample(albedo,albedo_sampler,input.uv)", "texel")
                .replace("input.color", "tint");
            format!("{sampling}\nreturn {result};")
        }
    };
    let result_type = match path {
        AlphaFixturePath::Tint => "vec4<f32>",
        _ => "f32",
    };
    let source = format!(
        "fn fixture(material:vec4<f32>, brightness:f32, vertex_color:vec4<f32>, texel:vec4<f32>) -> {result_type} {{\nvar tint:vec4<f32>;\n{tint}\n{result}\n}}"
    );
    let module = wgpu::naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&source)));
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("extracted tint/opacity fixture must validate");
    let (_, function) = module.functions.iter().next().expect("fixture function");
    let arguments = [
        material.to_vec(),
        vec![brightness],
        vertex.to_vec(),
        texel.to_vec(),
    ];
    let mut locals = vec![None; function.local_variables.len()];
    let mut emitted = vec![None; function.expressions.len()];
    for (_, local) in function.local_variables.iter() {
        assert!(
            local.init.is_none(),
            "unsupported fixture local initializer"
        );
    }
    for statement in function.body.iter() {
        match statement {
            Statement::Emit(range) => {
                for handle in range.clone() {
                    let value =
                        alpha_fixture_expression(function, handle, &arguments, &locals, &emitted);
                    emitted[handle.index()] = Some(value);
                }
            }
            Statement::Store { pointer, value } => {
                let Expression::LocalVariable(local) = function.expressions[*pointer] else {
                    panic!("unsupported fixture store destination");
                };
                locals[local.index()] = Some(alpha_fixture_expression(
                    function, *value, &arguments, &locals, &emitted,
                ));
            }
            Statement::Return { value: Some(value) } => {
                return alpha_fixture_expression(function, *value, &arguments, &locals, &emitted);
            }
            statement => panic!("unsupported tint/opacity statement: {statement:?}"),
        }
    }
    panic!("fixture must return tint or opacity");
}

/// Check actual tint, pass opacity and shared coverage for masked low-alpha vertices.
fn assert_low_vertex_alpha_samples(path: AlphaFixturePath) {
    for samples in [1, 4] {
        for (texture_alpha, material_alpha) in [
            (1.0, 1.0),
            (1.0, 0.8),
            (0.8, 1.0),
            (0.5, 0.5),
            (60.0 / 255.0, 1.0),
        ] {
            let template = alpha_template(Some(AlphaMode::Mask(60.0 / 255.0)), material_alpha);
            assert_eq!(template.cutoff, 60.0 / 255.0);
            for vertex_alpha in [0.0, 0.01, 0.05, 0.1, 0.2] {
                let alpha = shader_alpha_fixture(
                    template.color.to_array(),
                    0.75,
                    [0.25, 0.5, 0.875, vertex_alpha],
                    [0.125, 0.375, 0.625, texture_alpha],
                    path,
                )[0];
                assert_eq!(
                    shader_coverage_samples(alpha, template.cutoff, 0.0, samples),
                    samples as usize,
                    "{path:?}: vertex alpha {vertex_alpha} must not cut accepted texture alpha {texture_alpha}/material alpha {material_alpha} at {samples}x MSAA (computed alpha {alpha})"
                );
            }
        }
    }
}

#[test]
/// Masked color samples survive low authored vertex alpha with opaque texture/material alpha.
fn g3_low_vertex_alpha_preserves_fragment_samples() {
    assert_low_vertex_alpha_samples(AlphaFixturePath::Fragment);
}

#[test]
/// Masked depth samples survive low authored vertex alpha with opaque texture/material alpha.
fn g3_low_vertex_alpha_preserves_prepass_samples() {
    assert_low_vertex_alpha_samples(AlphaFixturePath::Prepass);
}

#[test]
/// Texture and material opacity below the mask cutoff must still discard both passes.
fn g3_texture_and_material_alpha_still_cut_out() {
    for path in [AlphaFixturePath::Fragment, AlphaFixturePath::Prepass] {
        for samples in [1, 4] {
            for (texture_alpha, material_alpha) in [(0.0, 1.0), (0.2, 1.0), (1.0, 0.2), (0.4, 0.5)]
            {
                let template = alpha_template(Some(AlphaMode::Mask(60.0 / 255.0)), material_alpha);
                for vertex_alpha in [0.0, 0.2, 1.0] {
                    let alpha = shader_alpha_fixture(
                        template.color.to_array(),
                        0.75,
                        [0.25, 0.5, 0.875, vertex_alpha],
                        [0.125, 0.375, 0.625, texture_alpha],
                        path,
                    )[0];
                    assert_eq!(
                        shader_coverage_samples(alpha, template.cutoff, 0.0, samples),
                        0,
                        "{path:?}: texture alpha {texture_alpha}, material alpha {material_alpha}, vertex alpha {vertex_alpha}, {samples}x MSAA"
                    );
                }
            }
        }
    }
}

#[test]
/// Preserve exact material RGB times brightness times vertex RGB independently of vertex alpha.
fn g3_vertex_rgb_tint_is_unchanged() {
    for vertex_alpha in [0.0, 0.01, 0.05, 0.1, 0.2, 1.0] {
        let tint = shader_alpha_fixture(
            [0.25, 0.5, 0.75, 0.8],
            0.75,
            [0.25, 0.25, 0.875, vertex_alpha],
            [1.0; 4],
            AlphaFixturePath::Tint,
        );
        // Independent binary-exact literals distinguish all three channels and factors.
        assert_eq!(&tint[..3], &[0.046875, 0.09375, 0.4921875]);
    }
}
