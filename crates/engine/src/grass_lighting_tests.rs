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
        E::Unary {
            op: wgpu::naga::UnaryOperator::Negate,
            expr,
        } => eval(*expr).into_iter().map(|v| -v).collect(),
        E::Unary {
            op: wgpu::naga::UnaryOperator::LogicalNot,
            expr,
        } => eval(*expr)
            .into_iter()
            .map(|v| f32::from(v == 0.0))
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
                        B::Equal => u32::from(a == b) as f32,
                        B::LogicalAnd => u32::from(a != 0.0 && b != 0.0) as f32,
                        B::LogicalOr => u32::from(a != 0.0 || b != 0.0) as f32,
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
                M::Sqrt => a.into_iter().map(f32::sqrt).collect(),
                M::Pow => {
                    let b = eval(arg1.unwrap());
                    a.into_iter()
                        .enumerate()
                        .map(|(i, v)| v.powf(b[i % b.len()]))
                        .collect()
                }
                M::Length => vec![a.iter().map(|v| v * v).sum::<f32>().sqrt()],
                M::Mix | M::Clamp => {
                    let b = eval(arg1.unwrap());
                    let c = eval(arg2.unwrap());
                    let lanes = a.len().max(b.len()).max(c.len());
                    (0..lanes)
                        .map(|i| {
                            let (x, y, z) = (a[i % a.len()], b[i % b.len()], c[i % c.len()]);
                            if *fun == M::Mix {
                                x * (1.0 - z) + y * z
                            } else {
                                x.clamp(y, z)
                            }
                        })
                        .collect()
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
                use wgpu::naga::Expression;
                let value =
                    lighting_fixture_expression(function, *value, arguments, locals, emitted);
                match function.expressions[*pointer] {
                    Expression::LocalVariable(local) => locals[local.index()] = Some(value),
                    Expression::AccessIndex { base, index } => {
                        let Expression::LocalVariable(local) = function.expressions[base] else {
                            panic!("unsupported lighting component destination");
                        };
                        assert_eq!(value.len(), 1);
                        locals[local.index()]
                            .as_mut()
                            .expect("initialized lighting vector")[index as usize] = value[0];
                    }
                    _ => panic!("unsupported lighting store destination"),
                }
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

/// Run the real vertex light assignment and fragment body with substituted uniforms.
/// `instance_normal` is the world-space instance normal the vertex shader lights. Only the
/// texture sample, the shadow fetch and fog/tone mapping are replaced; visibility 1 models the
/// shadow helper's disabled result.
fn shader_retail_light_fixture(
    instance_normal: [f32; 3],
    shadow: f32,
    output_scale: f32,
    tuning: [bevy::prelude::Vec4; 4],
) -> Vec<f32> {
    shader_light_fixture(instance_normal, shadow, output_scale, tuning, [0.0; 3])
}

/// Evaluate production fragment composition with a controlled, nonzero sheen contribution.
fn shader_light_fixture(
    instance_normal: [f32; 3],
    shadow: f32,
    output_scale: f32,
    tuning: [bevy::prelude::Vec4; 4],
    sheen: [f32; 3],
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
        .replace("out.color =", "return")
        .replace(
            "tuning_sheen(normal,input.world_position.xyz)",
            &format!("vec3<f32>({:?},{:?},{:?})", sheen[0], sheen[1], sheen[2]),
        )
        .replace("input.root_fraction", "0.4")
        .replace("grass.fade.z", &format!("{output_scale:?}"));
    let helpers = ["tuning_diffuse", "tuning_illumination", "tuning_finish"]
        .map(|name| lighting_fixture_function(shader, name).unwrap())
        .join("\n");
    let source = format!(
        "{helpers}\n{}\nfn vertex_light(light_normal:vec3<f32>) -> vec3<f32> {{ return {}; }}\n\
         fn fixture(normal:vec3<f32>, vertex_normal:vec3<f32>, synthetic_shadow:f32) -> vec4<f32> {{ {} }}",
        lighting_fixture_preprocess(lighting_fixture_function(shader, "lighting").expect("lighting function"), false), vertex, fragment,
    )
    .replace("grass.sun_direction.xyz", "vec3<f32>(0.0,0.0,4.0)")
    .replace("grass.sun_color.rgb", "vec3<f32>(1.5,3.0,5.0)")
    .replace("grass.ambient.rgb", "vec3<f32>(0.25,0.5,1.0)")
    .replace("view.exposure", "2.0");
    let mut source = source;
    for (name, value) in [
        "tuning_light",
        "tuning_surface",
        "tuning_color",
        "tuning_specular",
    ]
    .into_iter()
    .zip(tuning)
    {
        source = source.replace(
            &format!("grass.{name}"),
            &format!(
                "vec4<f32>({:?},{:?},{:?},{:?})",
                value.x, value.y, value.z, value.w
            ),
        );
    }
    lighting_fixture_run(
        &source,
        &[vec![0.0, 1.0, 0.0], instance_normal.to_vec(), vec![shadow]],
    )
}

/// Output scaling applies to both diffuse and the optional nonzero specular contribution.
#[test]
fn grass_output_scale_includes_nonzero_sheen() {
    let tuning = crate::grass_tuning::GrassTuning {
        specular_strength: 0.5,
        ..Default::default()
    };
    let uniforms = tuning.as_uniforms();
    let full = shader_light_fixture([0.0, 0.0, 1.0], 0.6, 1.0, uniforms, [0.2, 0.4, 0.8]);
    let scaled = shader_light_fixture([0.0, 0.0, 1.0], 0.6, 0.5, uniforms, [0.2, 0.4, 0.8]);
    for channel in 0..3 {
        assert!(
            (scaled[channel] - full[channel] * 0.5).abs() < 1.0e-6,
            "channel {channel}: scaled {} versus half {}",
            scaled[channel],
            full[channel]
        );
    }
}

/// The retail grass pixel formula, written out independently of the shader:
/// `(NdotL*C*Dir*albedo*lerp(floor,1,mask) + C*Ambient*albedo) * scale`, then the engine's
/// exposure/pi factor (exact for the sun; Bevy's ambient has no 1/pi). The fixture's sun, ambient and albedo are fixed in the fixture.
fn retail_grass_pixel(n_dot_l: f32, shadow_mask: f32, floor: f32, output_scale: f32) -> [f32; 3] {
    let (sun, ambient, albedo) = ([1.5, 3.0, 5.0], [0.25, 0.5, 1.0], [0.25, 0.5, 0.75]);
    let shadow = floor + (1.0 - floor) * shadow_mask;
    let mut out = [0.0; 3];
    for c in 0..3 {
        let lit = n_dot_l * sun[c] * albedo[c] * shadow + ambient[c] * albedo[c];
        out[c] = lit * output_scale * 2.0 / std::f32::consts::PI;
    }
    out
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

#[test]
/// The retail pixel formula holds for three shadow masks, three instance normals and both
/// output scales. The ambient term is not shadowed (the mask 0 result keeps it); the shadow
/// floor 0.3 scales only the direct term; the scale is the renderer-wide output scale.
fn retail_grass_pixel_formula_matches_for_shadow_masks_and_normals() {
    use crate::grass_tuning::{GrassTuning, SHADOW_CLAMP_VALUE};
    let tuning = GrassTuning::default().as_uniforms();
    // Sun direction (0,0,4): N.L is 1 for +Z, 0 for -Z, and 0.8 for (0,3,4)/5.
    for (normal, n_dot_l) in [
        ([0.0, 0.0, 3.0], 1.0),
        ([0.0, 0.0, -2.0], 0.0),
        ([0.0, 3.0, 4.0], 0.8),
    ] {
        for mask in [0.0, 0.5, 1.0] {
            for scale in [1.0, 1.0 / 1.2] {
                let actual = shader_retail_light_fixture(normal, mask, scale, tuning);
                let expected = retail_grass_pixel(n_dot_l, mask, SHADOW_CLAMP_VALUE, scale);
                assert_eq!(actual.len(), 4);
                for c in 0..3 {
                    assert!(
                        (actual[c] - expected[c]).abs() < 1.0e-5,
                        "normal {normal:?} mask {mask} scale {scale} channel {c}: \
                         expected {expected:?}, got {actual:?}"
                    );
                }
                assert_eq!(actual[3], 1.0);
            }
        }
    }
}

#[test]
/// Fully shadowed pixels keep the unshadowed ambient and 30 percent of the direct light.
fn retail_grass_ambient_is_not_shadowed() {
    use crate::grass_tuning::GrassTuning;
    let tuning = GrassTuning::default().as_uniforms();
    let facing_away = shader_retail_light_fixture([0.0, 0.0, -2.0], 0.0, 1.0, tuning);
    let facing_sun = shader_retail_light_fixture([0.0, 0.0, 3.0], 0.0, 1.0, tuning);
    let ambient_only = retail_grass_pixel(0.0, 1.0, 0.3, 1.0);
    for c in 0..3 {
        assert!((facing_away[c] - ambient_only[c]).abs() < 1.0e-5);
        let direct = facing_sun[c] - facing_away[c];
        let unshadowed =
            shader_retail_light_fixture([0.0, 0.0, 3.0], 1.0, 1.0, tuning)[c] - facing_away[c];
        assert!((direct - 0.3 * unshadowed).abs() < 1.0e-5, "channel {c}");
    }
}

#[test]
/// Actual disabled/no-light shadow helper results give the fully lit (mask 1) pixel.
fn g3_disabled_shadow_visibility_returns_unshadowed_light() {
    use crate::grass_tuning::GrassTuning;
    let tuning = GrassTuning::default().as_uniforms();
    for (count, flags) in [(1, 0), (1, 2), (0, 1)] {
        let visibility = shader_directional_shadow_fixture(true, count, flags, 0.0);
        let color = shader_retail_light_fixture([0.0, 0.0, 3.0], visibility, 1.0, tuning);
        let expected = retail_grass_pixel(1.0, 1.0, 0.3, 1.0);
        for c in 0..3 {
            assert!((color[c] - expected[c]).abs() < 1.0e-5);
        }
    }
}

#[test]
/// The packed brightness byte reaches the shader as byte/255; with both tint strengths at the
/// retail default 1 the tuning helper returns it unchanged, so the unit maximum stays one.
fn g3_brightness_byte_decodes_as_unit_range() {
    let shader = include_str!("shaders/grass.wgsl");
    assert!(shader.contains("tuning_brightness(f32(input.packed.z & 255u)/255.0,"));
    for (byte, expected) in [(0u32, 0.0), (127, 0.49803922), (255, 1.0)] {
        let actual = tuning_helper(
            "tuning_brightness",
            &[
                vec![byte as f32 / 255.0],
                vec![0.3],
                vec![-0.4],
                vec![1.0],
                vec![1.0],
            ],
        )[0];
        assert!(
            (actual - expected).abs() < 0.000001,
            "byte {byte}: {actual}"
        );
    }
}

/// Execute an extracted production tuning helper, never a copied implementation.
fn tuning_helper(name: &str, arguments: &[Vec<f32>]) -> Vec<f32> {
    let shader = include_str!("shaders/grass.wgsl");
    let helper = lighting_fixture_function(shader, name).expect("production tuning helper");
    let source = helper.replacen(&format!("fn {name}("), "fn fixture(", 1);
    lighting_fixture_run(&source, arguments)
}

#[test]
/// Separate factors remain tunable even when their old combined byte is zero.
fn grass_tuning_independent_land_and_range() {
    let run = |tint, range| {
        tuning_helper(
            "tuning_brightness",
            &[vec![0.0], vec![0.4], vec![-0.5], vec![tint], vec![range]],
        )[0]
    };
    assert_eq!(run(1.0, 1.0), 0.0);
    assert_eq!(run(0.0, 0.0), 1.0);
    assert!((run(1.0, 0.0) - 0.4).abs() < 1e-6);
    assert!((run(1.0, 0.5) - 0.1).abs() < 1e-6);
    for byte in 0..=255 {
        let original = byte as f32 / 255.0;
        assert_eq!(
            tuning_helper(
                "tuning_brightness",
                &[vec![original], vec![0.4], vec![-0.5], vec![1.0], vec![1.0]]
            )[0],
            original
        );
    }
}

#[test]
/// Angle, wrap and backlight independently affect literal perpendicular/back normals.
fn grass_tuning_angle_wrap_backlight() {
    let run = |normal, weight, wrap, back| {
        tuning_helper(
            "tuning_diffuse",
            &[
                normal,
                vec![0.0, 0.0, 4.0],
                vec![1.0, 1.0, 1.0, weight],
                vec![wrap, 1.0, 0.0, back],
            ],
        )[0]
    };
    assert_eq!(run(vec![0.0, 2.0, 0.0], 1.0, 0.0, 0.0), 0.0);
    assert_eq!(run(vec![0.0, 2.0, 0.0], 0.0, 0.0, 0.0), 1.0);
    assert!((run(vec![0.0, 2.0, 0.0], 1.0, 0.5, 0.0) - 1.0 / 3.0).abs() < 1e-6);
    assert_eq!(run(vec![0.0, 0.0, -2.0], 1.0, 0.0, 0.25), 0.25);
}

#[test]
/// Ambient, sun, strength and floor use independently calculated scalar results.
fn grass_tuning_light_and_shadow_controls() {
    let run = |ambient, sun, strength, floor, mode| {
        tuning_helper(
            "tuning_illumination",
            &[
                vec![2.0; 3],
                vec![6.0; 3],
                vec![0.25],
                vec![std::f32::consts::PI],
                vec![1.0, ambient, sun, 1.0],
                vec![0.0, strength, floor, 0.0],
                vec![0.0, 0.5, mode, 0.0],
            ],
        )
    };
    assert_eq!(run(1.0, 1.0, 1.0, 0.0, 0.0), vec![3.5; 3]);
    assert_eq!(run(0.5, 2.0, 1.0, 0.0, 0.0), vec![4.0; 3]);
    assert_eq!(run(1.0, 1.0, 0.0, 0.0, 0.0), vec![8.0; 3]);
    for value in run(1.0, 1.0, 0.5, 0.0, 0.0) {
        assert!((value - 5.75).abs() < 1e-6);
    }
}

#[test]
/// Final brightness, luminance saturation and base darkening each have literal targets.
fn grass_tuning_final_color_controls() {
    let run = |brightness, saturation, root, dark| {
        tuning_helper(
            "tuning_finish",
            &[
                vec![0.2, 0.4, 0.8],
                vec![root],
                vec![brightness, 1.0, 1.0, 1.0],
                vec![saturation, 1.0, 1.0, dark],
            ],
        )
    };
    assert_eq!(run(1.0, 1.0, 0.0, 0.0), vec![0.2, 0.4, 0.8]);
    assert_eq!(run(0.5, 1.0, 0.0, 0.0), vec![0.1, 0.2, 0.4]);
    for value in run(1.0, 0.0, 0.0, 0.0) {
        assert!((value - 0.38636).abs() < 1e-6);
    }
    assert_eq!(run(1.0, 1.0, 1.0, 0.5), vec![0.1, 0.2, 0.4]);
}

#[test]
/// At normal incidence GGX has a closed-form reference; grazing/back sun stays finite.
fn grass_tuning_ggx_roughness() {
    let run = |roughness, ndl| {
        tuning_helper(
            "tuning_ggx",
            &[vec![1.0], vec![1.0], vec![ndl], vec![1.0], vec![roughness]],
        )[0]
    };
    // alpha=.25, D=16/pi, V=.25, F=.04, NdotL=1.
    assert!((run(0.5, 1.0) - 0.05092958).abs() < 1e-6);
    assert!((run(1.0, 1.0) - 0.0031830988).abs() < 1e-6);
    assert_eq!(run(0.5, 0.0), 0.0);
    assert!(run(0.0, 1.0).is_finite());
}

#[test]
/// Execute production sheen strength with the actual GGX helper.
fn grass_tuning_specular_strength() {
    let shader = include_str!("shaders/grass.wgsl");
    let ggx = lighting_fixture_function(shader, "tuning_ggx").unwrap();
    let sheen = lighting_fixture_function(shader, "tuning_sheen")
        .unwrap()
        .replace("grass.tuning_specular.x", "strength")
        .replace("grass.tuning_specular.y", "0.5")
        .replace("grass.sun_direction.xyz", "vec3(0.0,0.0,2.0)")
        .replace("grass.sun_color.rgb", "vec3(1.5,3.0,5.0)")
        .replace("grass.tuning_light.z", "2.0")
        .replace("view.world_position", "vec3(0.0,0.0,3.0)")
        .replace("view.exposure", "2.0")
        .replace("world:vec3<f32>)", "world:vec3<f32>, strength:f32)");
    let source = format!(
        "{ggx}\n{sheen}\nfn fixture(strength:f32) -> vec3<f32> {{ return tuning_sheen(vec3(0.0,0.0,1.0),vec3(0.0),strength); }}"
    );
    assert_eq!(lighting_fixture_run(&source, &[vec![0.0]]), vec![0.0; 3]);
    let color = lighting_fixture_run(&source, &[vec![0.5]]);
    for (actual, expected) in color.into_iter().zip([0.15278874, 0.305_577_5, 0.5092958]) {
        assert!((actual - expected).abs() < 1e-6);
    }
}

/// Evaluate the production decoder with a tilted, asymmetric normal and rotated
/// frame; distinguish swapped axes/signs and RGB versus BC5 Z reconstruction.
#[test]
fn directional_normal_fixture_uses_authored_axes_and_handedness() {
    let assert_normal = |actual: Vec<f32>, expected: [f32; 3]| {
        assert_eq!(actual.len(), 3);
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 0.000002);
        }
    };
    let shader = include_str!("shaders/grass.wgsl");
    let source = lighting_fixture_function(shader, "tangent_normal")
        .unwrap()
        .replace("fn tangent_normal(", "fn fixture(")
        // Supply the production uniform lane as a synthetic argument, preserving
        // the decoder's real conditional and component store for CPU execution.
        .replace("grass.normal_params.z", "reconstruct_z")
        .replace("b:vec3<f32>)", "b:vec3<f32>,reconstruct_z:f32)");
    let args = [
        vec![0.8, 0.3, 0.9],
        vec![0.0, 1.0, 0.0],
        vec![0.0, 0.0, -1.0],
        vec![1.0, 0.0, 0.0],
        vec![0.0],
    ];
    let actual = lighting_fixture_run(&source, &args);
    let length = 1.16_f32.sqrt();
    assert_normal(actual, [-0.4 / length, 0.8 / length, -0.6 / length]);
    let mut mirrored = args.clone();
    mirrored[3][0] = -1.0;
    let actual = lighting_fixture_run(&source, &mirrored);
    assert_normal(actual, [0.4 / length, 0.8 / length, -0.6 / length]);
    // BC5 stores XY only. The positive Z root is sqrt(1-.6^2-.4^2),
    // independent of the sampled blue channel and the authored bitangent sign.
    for blue in [0.1, 0.9] {
        for handedness in [-1.0, 1.0] {
            let mut bc5 = args.clone();
            bc5[0][2] = blue;
            bc5[3][0] = handedness;
            bc5[4][0] = 1.0;
            assert_normal(
                lighting_fixture_run(&source, &bc5),
                [-0.4 * handedness, 0.48_f32.sqrt(), -0.6],
            );
        }
    }
    // The positive-Z reconstruction clamps XY samples outside the unit disk.
    let mut outside_disk = args.clone();
    outside_disk[0] = vec![1.0, 1.0, 0.9];
    outside_disk[4][0] = 1.0;
    let length = 2.0_f32.sqrt();
    assert_normal(
        lighting_fixture_run(&source, &outside_disk),
        [1.0 / length, 0.0, -1.0 / length],
    );
}

#[test]
fn grass_shader_has_no_unsourced_wind_displacement() {
    // The engine has no blended WTHR wind speed or wind direction yet, so blades stay at their
    // placed positions. Placement, fade and GRAS height/slope handling do not depend on wind.
    let shader = include_str!("shaders/grass.wgsl");
    for removed in [
        "gust",
        "sway",
        "wind_weight",
        "textureSampleLevel",
        "var wind",
    ] {
        assert!(!shader.contains(removed), "shader still has `{removed}`");
    }
    assert!(
        shader.contains("grass.fade.y-distance"),
        "distance fade stays"
    );
    assert!(
        shader.contains("signed_byte(input.packed.z"),
        "slope fit stays"
    );
}
