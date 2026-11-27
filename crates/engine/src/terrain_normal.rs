//! Tests that the near landscape's normal-map sign and tangent frame agree (#963).
//!
//! `shaders/terrain_normal.wgsl` holds the sign and the frame. These tests evaluate the production
//! WGSL functions with naga's constant evaluator (no GPU), and compare the lit result with a CPU
//! Lambert reference built from the production mesh's own coordinates. The frame the tests use is
//! the one the shader source builds, so an edit to the shader changes the result. The
//! `terrain_normal_probe` example runs the same module on a device against the same reference.
//!
//! The retail rule these tests encode (skyrim-research NEEDS 21, reviewed, https://github.com/Mudcrab-Team/skyrim_decomp/pull/1082): the near
//! landscape normal is n = (2R - 1, 2G - 1, 2B - 1), with tangent-space Y along +V, so a texel with
//! G above 0.5 tilts the lit normal toward +V. A DirectX-convention map stores green that way. The
//! tests build their texels by that rule and check the shader's frame and sign against the mesh.

use crate::{
    streaming::build_terrain_quadrant_mesh,
    world::cache::{TerrainLayerSnapshot, TerrainSnapshot},
};
use bevy::mesh::{Mesh, VertexAttributeValues};

const NORMAL_SHADER: &str = include_str!("shaders/terrain_normal.wgsl");
const NEAR_SHADER: &str = include_str!("shaders/terrain.wgsl");
const MERGED_SHADER: &str = include_str!("terrain_merge/terrain_merged.wgsl");

/// `NORMAL_GREEN_SIGN` as the production shader declares it.
fn shader_green_sign() -> f64 {
    let declaration = NORMAL_SHADER
        .lines()
        .find(|line| line.starts_with("const NORMAL_GREEN_SIGN: f32 = "))
        .expect("terrain_normal.wgsl declares NORMAL_GREEN_SIGN");
    declaration
        .trim_start_matches("const NORMAL_GREEN_SIGN: f32 = ")
        .trim_end_matches(';')
        .parse()
        .expect("NORMAL_GREEN_SIGN is a number")
}

/// Evaluate a production function of `terrain_normal.wgsl` with naga's constant evaluator. The
/// function's `let` lines become constants, so the arithmetic is the shader's own. A line that calls
/// another user function is skipped: the caller supplies that value in `inputs` (`tilt`).
/// `sign` replaces the declared sign so a test can run the other one.
fn evaluate(function: &str, sign: f64, inputs: &str, component: &str) -> f64 {
    use wgpu::naga::{Expression, Literal, front::wgsl};
    let (_, rest) = NORMAL_SHADER
        .split_once(&format!("fn {function}("))
        .expect("production function");
    let (_, body) = rest.split_once('{').expect("function body");
    let (body, _) = body.split_once("\n}").expect("function end");
    let (equations, result) = body.rsplit_once("return ").expect("function result");
    let equations: String = equations
        .lines()
        .filter(|line| !line.contains("terrain_tilt("))
        .map(|line| line.replace("let ", "const "))
        .collect::<Vec<_>>()
        .join("\n");
    let result = result.trim().trim_end_matches(';');
    let source = format!(
        "const NORMAL_GREEN_SIGN: f32 = {sign:?};\n{inputs}\n{equations}\nconst result = ({result}){component};"
    );
    let module = wgsl::parse_str(&source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&source)));
    let (_, result) = module
        .constants
        .iter()
        .find(|(_, item)| item.name.as_deref() == Some("result"))
        .expect("result constant");
    match module.global_expressions[result.init] {
        Expression::Literal(Literal::F32(value)) => f64::from(value),
        ref other => panic!("expected a folded f32, got {other:?}"),
    }
}

/// `terrain_tangent_normal` of the shader for a stored (red, green) pair.
fn tangent_normal(sample_xy: [f64; 2], sign: f64) -> [f64; 3] {
    let input = format!(
        "const sample_xy = vec2<f32>({:?}, {:?});",
        sample_xy[0] as f32, sample_xy[1] as f32
    );
    let tilt = ["x", "y"].map(|axis| evaluate("terrain_tilt", sign, &input, &format!(".{axis}")));
    let input = format!(
        "const tilt = vec2<f32>({:?}, {:?});",
        tilt[0] as f32, tilt[1] as f32
    );
    ["x", "y", "z"]
        .map(|axis| evaluate("terrain_tangent_normal", sign, &input, &format!(".{axis}")))
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let length = dot(a, a).sqrt();
    [a[0] / length, a[1] / length, a[2] / length]
}

/// `terrain_tbn` of the shader: the columns (t, b, n).
fn tbn(n: [f64; 3]) -> [[f64; 3]; 3] {
    let input = format!(
        "const n = vec3<f32>({:?}, {:?}, {:?});",
        n[0] as f32, n[1] as f32, n[2] as f32
    );
    std::array::from_fn(|column| {
        ["x", "y", "z"]
            .map(|axis| evaluate("terrain_tbn", 1.0, &input, &format!("[{column}].{axis}")))
    })
}

fn to_world(frame: [[f64; 3]; 3], tangent: [f64; 3]) -> [f64; 3] {
    normalize(std::array::from_fn(|axis| {
        frame[0][axis] * tangent[0] + frame[1][axis] * tangent[1] + frame[2][axis] * tangent[2]
    }))
}

fn lambert(normal: [f64; 3], light: [f64; 3]) -> f64 {
    dot(normal, light).max(0.0)
}

/// A flat 33 x 33 height field, shaped by `height(x, y)` in Creation units.
fn terrain(height: impl Fn(f32, f32) -> f32) -> TerrainSnapshot {
    let mut heights = Vec::with_capacity(33 * 33);
    for y in 0..33 {
        for x in 0..33 {
            heights.push(height(x as f32 * 128.0, y as f32 * 128.0));
        }
    }
    TerrainSnapshot {
        cell_id: 1,
        width: 33,
        height: 33,
        heights,
        normals: (0..33 * 33).flat_map(|_| [0, 0, 127]).collect(),
        vertex_colors: Vec::new(),
        layers: (0..4)
            .map(|quadrant| TerrainLayerSnapshot {
                texture_form_id: u32::from(quadrant) + 1,
                quadrant,
                layer: 0,
                is_base: true,
                weights: Vec::new(),
            })
            .collect(),
        water_height: None,
        water_type_form_id: None,
    }
}

/// Position and texture coordinate of one vertex of the production near-terrain mesh.
fn vertex(mesh: &Mesh, index: usize) -> ([f64; 3], [f64; 2]) {
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        panic!("terrain mesh has positions")
    };
    let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0) else {
        panic!("terrain mesh has texture coordinates")
    };
    (positions[index].map(f64::from), uvs[index].map(f64::from))
}

#[test]
fn one_source_declares_the_sign_and_the_frame() {
    assert_eq!(
        NORMAL_SHADER.matches("const NORMAL_GREEN_SIGN").count(),
        1,
        "the sign has exactly one declaration"
    );
    for (name, shader) in [
        ("terrain.wgsl", NEAR_SHADER),
        ("terrain_merged.wgsl", MERGED_SHADER),
    ] {
        assert!(
            shader.contains(
                "#import engine::terrain_normal::{retail_terrain_normal, terrain_tangent_normal, terrain_tbn}"
            ),
            "{name} imports the shared normal module"
        );
        assert!(
            !shader.contains("NORMAL_GREEN_SIGN") && !shader.contains("fn terrain_tbn"),
            "{name} must not keep its own copy of the sign or the frame"
        );
        assert!(
            shader.contains("terrain_tangent_normal(sample.xy)")
                && shader.contains("retail_terrain_normal(sample.xyz)"),
            "{name} decodes normal samples through the shared functions"
        );
    }
}

/// The frame's axes are the mesh's texture axes. This is the fact the sign depends on.
#[test]
fn the_frame_follows_the_mesh_texture_axes() {
    // Quadrant 0 spans grid points 0..16 of 32. Take a unit step in U and in V at its origin.
    let flat = build_terrain_quadrant_mesh(&terrain(|_, _| 0.0), 0).unwrap();
    let (origin, origin_uv) = vertex(&flat, 0);
    let (along_x, along_x_uv) = vertex(&flat, 1);
    let (along_y, along_y_uv) = vertex(&flat, 17);
    let du = [
        along_x[0] - origin[0],
        along_x[1] - origin[1],
        along_x[2] - origin[2],
    ];
    let dv = [
        along_y[0] - origin[0],
        along_y[1] - origin[1],
        along_y[2] - origin[2],
    ];
    assert!(along_x_uv[0] > origin_uv[0] && along_x_uv[1] == origin_uv[1]);
    assert!(along_y_uv[1] > origin_uv[1] && along_y_uv[0] == origin_uv[0]);
    let [t, b, n] = tbn([0.0, 1.0, 0.0]);
    assert_eq!(n, [0.0, 1.0, 0.0]);
    // U grows along world +X, V along world -Z, and the frame's bitangent is the V direction.
    assert!(
        dot(normalize(du), t) > 0.999_999,
        "tangent follows U: {du:?} vs {t:?}"
    );
    assert!(
        dot(normalize(dv), b) > 0.999_999,
        "bitangent follows V: {dv:?} vs {b:?}"
    );
    assert!(b[2] < -0.999_999, "bitangent is world -Z: {b:?}");
    // The frame is right-handed: t x b = n.
    let handed = cross(t, b);
    assert!(dot(handed, n) > 0.999_999);
}

/// On a sloped plane the tangent and bitangent still follow dP/du and dP/dv.
#[test]
fn the_frame_follows_the_texture_axes_on_slopes() {
    for (name, height) in [
        (
            "rises along x",
            (|x: f32, _: f32| 0.2 * x) as fn(f32, f32) -> f32,
        ),
        ("rises along y", |_, y| 0.3 * y),
    ] {
        let mesh = build_terrain_quadrant_mesh(&terrain(height), 0).unwrap();
        let (origin, _) = vertex(&mesh, 0);
        let (along_x, _) = vertex(&mesh, 1);
        let (along_y, _) = vertex(&mesh, 17);
        let du = normalize([
            along_x[0] - origin[0],
            along_x[1] - origin[1],
            along_x[2] - origin[2],
        ]);
        let dv = normalize([
            along_y[0] - origin[0],
            along_y[1] - origin[1],
            along_y[2] - origin[2],
        ]);
        let n = normalize(cross(du, dv));
        assert!(n[1] > 0.0, "{name}: the mesh normal points up");
        let [t, b, frame_n] = tbn(n);
        assert!(dot(du, t) > 0.999, "{name}: tangent follows U");
        assert!(dot(dv, b) > 0.999, "{name}: bitangent follows V");
        assert!(dot(cross(t, b), frame_n) > 0.999, "{name}: right-handed");
    }
}

/// A DirectX-convention dome, lit the way the shader lights it, against a CPU Lambert reference.
///
/// The relief is a height h(u, v) over texture space. The reference normal comes from the mesh's
/// own coordinates and the displaced surface P + k h n, with no tangent frame and no sign. The
/// stored texel is the DirectX encoding of the same relief: green = the tilt along +V.
struct Dome {
    /// Texel (u, v) in 0..1, the stored RGB pair, and the true world normal.
    texels: Vec<([f64; 2], [f64; 2], [f64; 3])>,
}

fn dome() -> Dome {
    // World length of one texture unit, from the production mesh: quadrant 0 spans half the cell.
    let mesh = build_terrain_quadrant_mesh(&terrain(|_, _| 0.0), 0).unwrap();
    let (origin, origin_uv) = vertex(&mesh, 0);
    let (corner, corner_uv) = vertex(&mesh, 17 * 17 - 1);
    let length_u = (corner[0] - origin[0]) / (corner_uv[0] - origin_uv[0]);
    let length_v = -(corner[2] - origin[2]) / (corner_uv[1] - origin_uv[1]);
    assert!((length_u - length_v).abs() < 1.0e-6);
    // A dome of radius 0.4 texture units about the middle of the texture, 0.15 world-lengths high.
    let height = |u: f64, v: f64| {
        let r2 = ((u - 0.5).powi(2) + (v - 0.5).powi(2)) / 0.4_f64.powi(2);
        0.15 * length_u * (1.0 - r2).max(0.0)
    };
    let gradient = |u: f64, v: f64| {
        let r2 = ((u - 0.5).powi(2) + (v - 0.5).powi(2)) / 0.4_f64.powi(2);
        if r2 >= 1.0 {
            (0.0, 0.0)
        } else {
            (
                -0.15 * length_u * 2.0 * (u - 0.5) / 0.4_f64.powi(2),
                -0.15 * length_u * 2.0 * (v - 0.5) / 0.4_f64.powi(2),
            )
        }
    };
    assert!(height(0.5, 0.5) > 0.0);
    let mut texels = Vec::new();
    for row in 0..32 {
        for column in 0..32 {
            let (u, v) = ((column as f64 + 0.5) / 32.0, (row as f64 + 0.5) / 32.0);
            let (hu, hv) = gradient(u, v);
            // The displaced surface S(u, v) = P(u, v) + h(u, v) * (+Y), P from the mesh: U along
            // +X, V along -Z. Its partials are (L, h_u, 0) and (0, h_v, -L).
            let world = normalize(cross([length_u, hu, 0.0], [0.0, hv, -length_v]));
            assert!(world[1] > 0.0);
            // The same relief as a tangent-space normal in the frame (dP/du, dP/dv, n):
            // (-h_u, -h_v, L). DirectX stores green as the +V component.
            let tangent = normalize([-hu, -hv, length_u]);
            texels.push((
                [u, v],
                [0.5 + 0.5 * tangent[0], 0.5 + 0.5 * tangent[1]],
                world,
            ));
        }
    }
    Dome { texels }
}

/// The sun direction (toward the sun) at 30 degrees elevation.
fn sun(toward: [f64; 3]) -> [f64; 3] {
    let elevation = 30.0_f64.to_radians();
    let flat = normalize([toward[0], 0.0, toward[2]]);
    [
        flat[0] * elevation.cos(),
        elevation.sin(),
        flat[2] * elevation.cos(),
    ]
}

const SUN_NORTH: [f64; 3] = [0.0, 0.0, -1.0]; // Creation +Y is runtime -Z.
const SUN_EAST: [f64; 3] = [1.0, 0.0, 0.0];

fn worst_difference(dome: &Dome, sign: f64, light: [f64; 3]) -> f64 {
    let frame = tbn([0.0, 1.0, 0.0]);
    dome.texels
        .iter()
        .map(|(_, stored, truth)| {
            // 8-bit storage, as the DDS holds it.
            let stored = stored.map(|value: f64| (value * 255.0).round() / 255.0);
            let shaded = to_world(frame, tangent_normal(stored, sign));
            (lambert(shaded, light) - lambert(*truth, light)).abs()
        })
        .fold(0.0, f64::max)
}

#[test]
fn the_shader_sign_lights_a_directx_dome_like_the_cpu_reference() {
    let dome = dome();
    let sign = shader_green_sign();
    for (name, light) in [("north", sun(SUN_NORTH)), ("east", sun(SUN_EAST))] {
        let worst = worst_difference(&dome, sign, light);
        assert!(
            worst < 0.02,
            "sun {name}: the shader's lit dome differs from the reference by {worst} \
             (NORMAL_GREEN_SIGN = {sign})"
        );
    }
}

#[test]
fn the_north_half_of_the_dome_is_brighter_under_a_north_sun() {
    let dome = dome();
    let sign = shader_green_sign();
    let light = sun(SUN_NORTH);
    let frame = tbn([0.0, 1.0, 0.0]);
    let mean = |north: bool| {
        let lit: Vec<f64> = dome
            .texels
            .iter()
            .filter(|(uv, _, truth)| {
                // Inside the dome, so the half-plane test sees relief and not flat ground.
                (uv[0] - 0.5).hypot(uv[1] - 0.5) < 0.35
                    && ((uv[1] > 0.5) == north)
                    && truth[1] < 0.999
            })
            .map(|(_, stored, _)| lambert(to_world(frame, tangent_normal(*stored, sign)), light))
            .collect();
        lit.iter().sum::<f64>() / lit.len() as f64
    };
    // North is -Z, the direction V grows, which is the lower half of the image (larger row).
    assert!(
        mean(true) > mean(false) + 0.1,
        "the half facing a north sun must be brighter: north {}, south {}",
        mean(true),
        mean(false)
    );
}

/// The negative control: the other sign inverts the north-south relief and leaves east-west alone.
/// This is what proves the tests above can see a wrong sign.
#[test]
fn the_wrong_sign_fails_north_south_and_passes_east_west() {
    let dome = dome();
    let wrong = -shader_green_sign();
    assert!(
        worst_difference(&dome, wrong, sun(SUN_NORTH)) > 0.1,
        "the wrong sign must visibly fail under a north sun"
    );
    assert!(
        worst_difference(&dome, wrong, sun(SUN_EAST)) < 0.02,
        "red is unaffected by the sign: the east control must pass with either"
    );
}

/// A tilted geometric normal turns the frame with the surface, and the sign still holds. This
/// checks the reference on a slope the dome sits on, not only on flat ground.
#[test]
fn the_sign_holds_on_a_north_south_slope() {
    // A ground plane that rises toward the north (-Z) by 0.3 per unit.
    let n = normalize([0.0, 1.0, 0.3]);
    let ground = tbn(n);
    // A texel that is one DirectX dome flank: the surface tilts toward +V by 0.2 in tangent space.
    let flank = [0.5, 0.5 + 0.5 * 0.2];
    let world = to_world(ground, tangent_normal(flank, shader_green_sign()));
    let along_v = ground[1];
    assert!(
        dot(world, along_v) > dot(n, along_v),
        "a +V tilt in green moves the lit normal toward +V (the bitangent)"
    );
}

/// The derivation in `terrain_normal.wgsl` gives +1 for this frame. A change to the frame or to
/// the sign that breaks the dome tests above must also revisit this value and that comment.
#[test]
fn the_shipped_sign_is_plus_one() {
    assert_eq!(shader_green_sign(), 1.0);
}
