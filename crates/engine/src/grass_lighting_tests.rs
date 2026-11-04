/// Execute only the scalar/vector operations used by the extracted lighting code.
/// Unsupported expressions fail loudly instead of falling back to a copied formula.
fn lighting_fixture_expression(
    function: &wgpu::naga::Function,
    handle: wgpu::naga::Handle<wgpu::naga::Expression>,
    arguments: &[Vec<f32>],
    locals: &[Option<Vec<f32>>],
    emitted: &[Option<Vec<f32>>],
) -> Vec<f32> {
    use wgpu::naga::{BinaryOperator as B, Expression as E, Literal, MathFunction as M};
    if let Some(value) = &emitted[handle.index()] {
        return value.clone();
    }
    let eval = |handle| lighting_fixture_expression(function, handle, arguments, locals, emitted);
    match &function.expressions[handle] {
        E::Literal(Literal::F32(value)) => vec![*value],
        E::Literal(Literal::U32(value)) => vec![*value as f32],
        E::Literal(Literal::Bool(value)) => vec![u32::from(*value) as f32],
        E::FunctionArgument(index) => arguments[*index as usize].clone(),
        E::LocalVariable(local) => locals[local.index()]
            .clone()
            .expect("assigned lighting local"),
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
        E::As {
            expr,
            kind: wgpu::naga::ScalarKind::Float,
            ..
        } => eval(*expr),
        E::As {
            expr,
            kind: wgpu::naga::ScalarKind::Uint,
            ..
        } => eval(*expr)
            .into_iter()
            .map(|value| value as u32 as f32)
            .collect(),
        E::Binary { op, left, right } => {
            let (left, right) = (eval(*left), eval(*right));
            let lanes = left.len().max(right.len());
            assert!(left.len() == lanes || left.len() == 1);
            assert!(right.len() == lanes || right.len() == 1);
            (0..lanes)
                .map(|lane| {
                    let (a, b) = (left[lane % left.len()], right[lane % right.len()]);
                    match op {
                        B::Add => a + b,
                        B::Subtract => a - b,
                        B::Multiply => a * b,
                        B::Divide => a / b,
                        B::And => ((a as u32) & (b as u32)) as f32,
                        B::NotEqual => u32::from(a != b) as f32,
                        B::Greater => u32::from(a > b) as f32,
                        _ => panic!("unsupported lighting operator: {op:?}"),
                    }
                })
                .collect()
        }
        E::Select {
            condition,
            accept,
            reject,
        } => {
            let condition = eval(*condition);
            assert_eq!(condition.len(), 1);
            eval(if condition[0] != 0.0 {
                *accept
            } else {
                *reject
            })
        }
        E::Math {
            fun,
            arg,
            arg1,
            arg2,
            ..
        } => {
            let a = eval(*arg);
            match fun {
                M::Clamp => {
                    let lo = eval(arg1.unwrap())[0];
                    let hi = eval(arg2.unwrap())[0];
                    a.into_iter().map(|v| v.clamp(lo, hi)).collect()
                }
                M::Abs => a.into_iter().map(f32::abs).collect(),
                M::Normalize => {
                    let length = a.iter().map(|value| value * value).sum::<f32>().sqrt();
                    a.into_iter().map(|value| value / length).collect()
                }
                M::Dot => {
                    let b = eval(arg1.expect("dot second operand"));
                    assert_eq!(a.len(), b.len());
                    vec![a.iter().zip(b).map(|(a, b)| a * b).sum()]
                }
                M::Max => {
                    let b = eval(arg1.expect("max second operand"));
                    let lanes = a.len().max(b.len());
                    assert!(a.len() == lanes || a.len() == 1);
                    assert!(b.len() == lanes || b.len() == 1);
                    (0..lanes)
                        .map(|i| a[i % a.len()].max(b[i % b.len()]))
                        .collect()
                }
                _ => panic!("unsupported lighting math: {fun:?}"),
            }
        }
        expression => panic!("unsupported lighting expression: {expression:?}"),
    }
}

/// Execute branches, stores and calls in source order, propagating early returns.
fn lighting_fixture_block(
    module: &wgpu::naga::Module,
    function: &wgpu::naga::Function,
    block: &wgpu::naga::Block,
    arguments: &[Vec<f32>],
    locals: &mut [Option<Vec<f32>>],
    emitted: &mut [Option<Vec<f32>>],
) -> Option<Vec<f32>> {
    use wgpu::naga::Statement;
    for statement in block.iter() {
        match statement {
            Statement::Emit(range) => {
                for handle in range.clone() {
                    let value =
                        lighting_fixture_expression(function, handle, arguments, locals, emitted);
                    emitted[handle.index()] = Some(value);
                }
            }
            Statement::Call {
                function: callee,
                arguments: inputs,
                result,
            } => {
                let inputs: Vec<_> = inputs
                    .iter()
                    .map(|input| {
                        lighting_fixture_expression(function, *input, arguments, locals, emitted)
                    })
                    .collect();
                let value = lighting_fixture_call(module, &module.functions[*callee], &inputs);
                emitted[result.expect("lighting call result").index()] = Some(value);
            }
            Statement::Return { value: Some(value) } => {
                return Some(lighting_fixture_expression(
                    function, *value, arguments, locals, emitted,
                ));
            }
            Statement::If {
                condition,
                accept,
                reject,
            } => {
                let condition =
                    lighting_fixture_expression(function, *condition, arguments, locals, emitted);
                assert_eq!(condition.len(), 1);
                let branch = if condition[0] != 0.0 { accept } else { reject };
                if let Some(value) =
                    lighting_fixture_block(module, function, branch, arguments, locals, emitted)
                {
                    return Some(value);
                }
            }
            Statement::Store { pointer, value } => {
                let wgpu::naga::Expression::LocalVariable(local) = function.expressions[*pointer]
                else {
                    panic!("unsupported lighting store destination");
                };
                locals[local.index()] = Some(lighting_fixture_expression(
                    function, *value, arguments, locals, emitted,
                ));
            }
            statement => panic!("unsupported lighting statement: {statement:?}"),
        }
    }
    None
}

/// Initialize mutable locals and execute an extracted function, including actual calls.
fn lighting_fixture_call(
    module: &wgpu::naga::Module,
    function: &wgpu::naga::Function,
    arguments: &[Vec<f32>],
) -> Vec<f32> {
    let mut locals = vec![None; function.local_variables.len()];
    let mut emitted = vec![None; function.expressions.len()];
    for (handle, local) in function.local_variables.iter() {
        if let Some(init) = local.init {
            locals[handle.index()] = Some(lighting_fixture_expression(
                function, init, arguments, &locals, &emitted,
            ));
        }
    }
    lighting_fixture_block(
        module,
        function,
        &function.body,
        arguments,
        &mut locals,
        &mut emitted,
    )
    .expect("lighting fixture must return a value")
}

/// Extract a complete named production function without assuming its equation.
fn lighting_fixture_function<'a>(shader: &'a str, name: &str) -> Option<&'a str> {
    let start = shader.find(&format!("fn {name}("))?;
    let body = shader[start..].find('{').expect("lighting body") + start;
    let mut depth = 0;
    for (offset, character) in shader[body..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&shader[start..body + offset + 1]);
                }
            }
            _ => {}
        }
    }
    panic!("unterminated lighting function");
}

/// Resolve only GRASS_SCENE conditionals; baseline source without macros passes unchanged.
fn lighting_fixture_preprocess(source: &str, scene: bool) -> String {
    let mut active = true;
    let mut parents = Vec::new();
    let mut result = String::new();
    for line in source.lines() {
        match line.trim() {
            "#ifdef GRASS_SCENE" => {
                parents.push(active);
                active &= scene;
            }
            "#else" => {
                active = *parents.last().expect("conditional else") && !active;
            }
            "#endif" => {
                active = parents.pop().expect("conditional end");
            }
            directive if directive.starts_with('#') => {
                panic!("unsupported lighting directive: {directive}")
            }
            _ if active => {
                result.push_str(line);
                result.push('\n');
            }
            _ => {}
        }
    }
    assert!(parents.is_empty(), "unterminated lighting conditional");
    result
}

/// Parse and validate real lighting, vertex-light assignment and fragment color code.
/// Substitute only uniforms, varyings, base RGB and shadow visibility; omit coverage
/// and tone mapping. Visibility 1 models the shadow helper's disabled/unshadowed result;
/// Bevy imports and bindings need separate validation; helper control flow is tested below.
fn shader_lighting_fixture(
    normal: [f32; 3],
    vertex_normal: [f32; 3],
    vertex_lighting: bool,
    shadow: f32,
) -> Vec<f32> {
    let shader = include_str!("shaders/grass.wgsl");
    let vertex = shader
        .split_once("out.light =")
        .expect("vertex lighting assignment")
        .1;
    let vertex = vertex
        .split_once(';')
        .expect("vertex lighting expression")
        .0;
    let fragment = shader
        .split_once("fn fragment(")
        .expect("fragment entry point")
        .1;
    let fragment = fragment
        .split_once("out.mask =")
        .expect("fragment coverage assignment")
        .1;
    let fragment = fragment.split_once(';').expect("coverage assignment end").1;
    let fragment = fragment
        .split_once("#ifdef TONEMAP_IN_SHADER")
        .expect("fragment tone mapping boundary")
        .0;
    let fragment = fragment
        // These fixtures isolate lighting. Fog composition has its own real-import checks.
        .replace(
            "out.color = grass_fog(out.color,input.world_position.xyz,input.position.xy);",
            "",
        )
        .replace("directional_shadow(input)", "synthetic_shadow")
        .replace("input.normal", "normal")
        .replace("input.light", "vertex_light(vertex_normal)")
        .replace("base.rgb", "vec3<f32>(0.25,0.5,0.75)")
        .replace("out.color =", "return");
    let source = format!(
        "{}\nfn vertex_light(normal:vec3<f32>) -> vec3<f32> {{ return {}; }}\n\
         fn fixture(normal:vec3<f32>, vertex_normal:vec3<f32>, flags:f32, synthetic_shadow:f32) -> vec4<f32> {{ {} }}",
        lighting_fixture_preprocess(lighting_fixture_function(shader, "lighting").expect("lighting function"), false), vertex.replace("out.normal", "normal"), fragment,
    )
    .replace("grass.sun_direction.xyz", "vec3<f32>(0.0,0.0,4.0)")
    .replace("grass.sun_color.rgb", "vec3<f32>(1.5,3.0,5.0)")
    .replace("grass.ambient.rgb", "vec3<f32>(0.25,0.5,1.0)")
    .replace("grass.absolute_grid.z", "flags")
    .replace("view.exposure", "2.0");
    lighting_fixture_run(
        &source,
        &[
            normal.to_vec(),
            vertex_normal.to_vec(),
            vec![u32::from(vertex_lighting) as f32],
            vec![shadow],
        ],
    )
}

/// Validate and execute a generated entry point against synthetic arguments.
fn lighting_fixture_run(source: &str, arguments: &[Vec<f32>]) -> Vec<f32> {
    let module = wgpu::naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("extracted lighting fixture must validate");
    let (_, function) = module
        .functions
        .iter()
        .find(|(_, function)| function.name.as_deref() == Some("fixture"))
        .expect("lighting fixture function");
    lighting_fixture_call(&module, function, arguments)
}

/// Evaluate the actual scene shadow helper, replacing just uniforms and the GPU fetch.
/// Before the helper existed the fragment was always unshadowed, represented by return 1;
/// enabled-shadow expectations therefore still fail numerically on the baseline shader.
fn shader_directional_shadow_fixture(scene: bool, count: u32, flags: u32, sampled: f32) -> f32 {
    let shader = include_str!("shaders/grass.wgsl");
    let helper = lighting_fixture_function(shader, "directional_shadow")
        .map(|helper| lighting_fixture_preprocess(helper, scene));
    let body = if let Some(helper) = helper {
        let mut body = helper
            .split_once('{')
            .expect("shadow helper body")
            .1
            .to_owned();
        if let Some(start) = body.find("fetch_directional_shadow(") {
            let open = start + "fetch_directional_shadow".len();
            let mut depth = 0;
            let mut end = None;
            for (offset, character) in body[open..].char_indices() {
                match character {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + offset + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            body.replace_range(start..end.expect("complete shadow fetch call"), "sampled");
        }
        body.replace("lights.n_directional_lights", "count")
            .replace("lights.directional_lights[0].flags", "flags")
    } else {
        "return 1.0; }".to_owned()
    };
    let source = format!("fn fixture(count:u32, flags:u32, sampled:f32) -> f32 {{ {body}");
    lighting_fixture_run(
        &source,
        &[vec![count as f32], vec![flags as f32], vec![sampled]],
    )[0]
}

#[test]
/// No light or a cleared shadow bit bypasses sampling; enabled shadows return the sample.
fn g3_directional_shadow_helper_obeys_light_count_and_flags() {
    for sampled in [0.0, 0.375, 1.0] {
        for flags in [0, 1, 2, 3] {
            assert_eq!(
                shader_directional_shadow_fixture(true, 0, flags, sampled),
                1.0
            );
            for count in [1, 2] {
                let expected = if flags & 1 != 0 { sampled } else { 1.0 };
                assert_eq!(
                    shader_directional_shadow_fixture(true, count, flags, sampled),
                    expected,
                    "count {count}, flags {flags}, sampled {sampled}"
                );
                assert_eq!(
                    shader_directional_shadow_fixture(false, count, flags, sampled),
                    1.0
                );
            }
        }
    }
}

/// Compare independently calculated RGB literals, also pinning opaque output alpha.
fn assert_lighting_rgb(actual: &[f32], expected: [f32; 3]) {
    assert_eq!(actual.len(), 4);
    for channel in 0..3 {
        assert!(
            (actual[channel] - expected[channel]).abs() < 0.000002,
            "channel {channel}: expected {expected:?}, got {actual:?}"
        );
    }
    assert_eq!(actual[3], 1.0);
}

#[test]
/// An authored blade normal pointing away from the sun receives ambient on either path.
fn g3_back_normal_receives_ambient_only() {
    for vertex in [false, true] {
        let color = shader_lighting_fixture([0.0, 0.0, -2.0], [0.0, 0.0, -2.0], vertex, 1.0);
        assert_lighting_rgb(&color, [0.039788735, 0.15915494, 0.47746483]);
    }
}

#[test]
/// Opposing varying normals prove that the GRAS flag selects the actual vertex result.
fn g3_vertex_lighting_flag_selects_the_correct_path() {
    for vertex in [false, true] {
        let color = shader_lighting_fixture([0.0, 0.0, -2.0], [0.0, 0.0, 3.0], vertex, 1.0);
        let expected = if vertex {
            [0.27852115, 1.1140846, 2.864789]
        } else {
            [0.039788735, 0.15915494, 0.47746483]
        };
        assert_lighting_rgb(&color, expected);
    }
}

#[test]
/// Full directional occlusion removes direct sunlight while retaining ambient and exposure.
fn g3_directional_shadow_preserves_ambient_on_both_paths() {
    for vertex in [false, true] {
        let color = shader_lighting_fixture([0.0, 0.0, 2.0], [0.0, 0.0, 3.0], vertex, 0.0);
        assert_lighting_rgb(&color, [0.039788735, 0.15915494, 0.47746483]);
    }
}

#[test]
/// Partial visibility attenuates only direct sunlight, independently in every RGB channel.
fn g3_partial_shadow_attenuates_direct_light_only() {
    for vertex in [false, true] {
        let color = shader_lighting_fixture([0.0, 0.0, 2.0], [0.0, 0.0, 3.0], vertex, 0.5);
        assert_lighting_rgb(&color, [0.15915494, 0.63661975, 1.6711269]);
    }
}

#[test]
/// Actual disabled/no-light helper results preserve literal unshadowed fallback lighting.
fn g3_disabled_shadow_visibility_returns_unshadowed_light() {
    for vertex in [false, true] {
        for (count, flags) in [(1, 0), (1, 2), (0, 1)] {
            let visibility = shader_directional_shadow_fixture(true, count, flags, 0.0);
            let color =
                shader_lighting_fixture([0.0, 0.0, 2.0], [0.0, 0.0, 3.0], vertex, visibility);
            assert_lighting_rgb(&color, [0.27852115, 1.1140846, 2.864789]);
        }
    }
}

#[test]
/// Decode actual packed brightness without allowing the unit maximum to exceed one.
fn g3_brightness_byte_decodes_as_unit_range() {
    let shader = include_str!("shaders/grass.wgsl");
    let expression = shader
        .split_once("let brightness =")
        .unwrap()
        .1
        .split_once(';')
        .unwrap()
        .0
        .replace("input.packed.z", "packed");
    let source = format!("fn fixture(packed:u32) -> f32 {{ return {expression}; }}");
    for (packed, expected) in [(0u32, 0.0), (127, 0.49803922), (255, 1.0), (0x1234ff, 1.0)] {
        let actual = lighting_fixture_run(&source, &[vec![packed as f32]])[0];
        assert!(
            (actual - expected).abs() < 0.000001,
            "packed {packed}: expected {expected}, got {actual}"
        );
    }
}

/// Evaluate the production decoder with a tilted, asymmetric normal and rotated
/// frame; unlike a flat normal this distinguishes swapped tangent axes/signs.
#[test]
fn directional_normal_fixture_uses_authored_axes_and_handedness() {
    let shader = include_str!("shaders/grass.wgsl");
    let source = lighting_fixture_function(shader, "tangent_normal")
        .unwrap()
        .replace("fn tangent_normal(", "fn fixture(");
    let args = [
        vec![0.8, 0.3, 0.9],
        vec![0.0, 1.0, 0.0],
        vec![0.0, 0.0, -1.0],
        vec![1.0, 0.0, 0.0],
    ];
    let actual = lighting_fixture_run(&source, &args);
    let length = 1.16_f32.sqrt();
    assert_lighting_rgb(&actual, [-0.4 / length, 0.8 / length, -0.6 / length]);
    let mut mirrored = args.clone();
    mirrored[3][0] = -1.0;
    let actual = lighting_fixture_run(&source, &mirrored);
    assert_lighting_rgb(&actual, [0.4 / length, 0.8 / length, -0.6 / length]);
}

#[test]
fn authored_alpha_weights_native_wind_without_erasing_roots() {
    let shader = include_str!("shaders/grass.wgsl");
    let source = lighting_fixture_function(shader, "wind_weight")
        .unwrap()
        .replace("fn wind_weight(", "fn fixture(");
    for (alpha, expected) in [
        (-0.2, 0.0),
        (0.0, 0.0),
        (0.35, 0.35),
        (1.0, 1.0),
        (1.5, 1.0),
    ] {
        assert_eq!(
            lighting_fixture_run(&source, &[vec![alpha]]),
            vec![expected]
        );
    }
    assert!(shader.contains("sway = wind_weight(input.color.a)"));
}
