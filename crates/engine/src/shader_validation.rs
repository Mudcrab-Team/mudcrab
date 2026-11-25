//! Composition and Naga validation of every first-party and vendored WGSL shader.
//!
//! Bevy's WGSL is not plain WGSL: it uses `#import`, `#define_import_path` and `#ifdef`
//! (naga_oil and Bevy shader defs). This test registers the real vendored and locked Bevy
//! modules, composes each shader with the shader defs the engine or Bevy uses for it, and
//! validates the result with `naga::valid::Validator`. It needs no GPU.
//!
//! A shader that cannot be composed this way is listed in [`KNOWN_EXCEPTIONS`] with the reason,
//! so a skipped shader stays visible. Every other `.wgsl` file under `crates/engine/src/shaders`
//! and `vendor/` must compose and validate, or the test fails and names the file, the shader-def
//! set and the Naga message with its line.

use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, NagaModuleDescriptor, ShaderDefValue, ShaderLanguage,
    ShaderType, get_preprocessor_data,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::OnceLock,
};
use wgpu::naga::valid::{Capabilities, ValidationFlags, Validator};

type Definitions = HashMap<String, ShaderDefValue>;
type Sources = HashMap<String, (PathBuf, String)>;

/// Shaders that cannot be composed outside the full Bevy app. Each entry is the workspace-relative
/// path and the reason it is not composed here. A skipped shader must be visible, so an entry is
/// checked against the files on disk and the reason must not be empty.
const KNOWN_EXCEPTIONS: &[(&str, &str)] = &[];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The workspace-relative path with forward slashes, for stable keys and messages on Windows.
fn relative_path(path: &Path) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn registry_roots() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(home) = std::env::var_os("CARGO_HOME").filter(|home| !home.is_empty()) {
        homes.push(PathBuf::from(home));
    }
    for variable in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(variable).filter(|home| !home.is_empty()) {
            let home = PathBuf::from(home).join(".cargo");
            if !homes.contains(&home) {
                homes.push(home);
            }
        }
    }
    homes
        .into_iter()
        .map(|home| home.join("registry/src"))
        .collect()
}

fn index_sources(directory: &Path, sources: &mut Sources) {
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| entry.expect("shader source directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            index_sources(&path, sources);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "wgsl")
        {
            // PbrPlugin loads the shipped dummy library and only MeshletPlugin replaces it with
            // the real one. Mudcrab does not enable Bevy's `meshlet` feature, but the meshlet
            // shaders under `vendor/` still need the real module to compose, so index the real
            // module here and validate the dummy as a top-level shader.
            if path.ends_with(Path::new("meshlet/dummy_visibility_buffer_resolve.wgsl")) {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            if let Some(name) = get_preprocessor_data(&source).0 {
                assert!(
                    sources
                        .insert(name.clone(), (path.clone(), source))
                        .is_none(),
                    "duplicate shader import {name} at {}",
                    path.display()
                );
            }
        }
    }
}

fn locked_package(lock: &str, name: &str) -> (String, Option<String>) {
    let document = lock
        .parse::<toml_edit::Document<String>>()
        .expect("parse Cargo.lock");
    let packages = document
        .get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .expect("Cargo.lock package tables");
    let package = packages
        .iter()
        .find(|package| package.get("name").and_then(|value| value.as_str()) == Some(name))
        .unwrap_or_else(|| panic!("Cargo.lock is missing {name}"));
    (
        package
            .get("version")
            .and_then(|value| value.as_str())
            .unwrap_or_else(|| panic!("Cargo.lock has no version for {name}"))
            .to_owned(),
        package
            .get("source")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
    )
}

/// Every composable shader module this build can load: the three Bevy crates whose sources the
/// engine composes against, and the engine's own shader libraries.
fn real_sources() -> &'static Sources {
    static SOURCES: OnceLock<Sources> = OnceLock::new();
    SOURCES.get_or_init(|| {
        let root = workspace_root();
        let lock = std::fs::read_to_string(root.join("Cargo.lock"))
            .expect("read Cargo.lock for the locked Bevy shader crates");
        let mut sources = Sources::new();
        index_sources(&root.join("crates/engine/src/shaders"), &mut sources);
        let mut registry_crates = Vec::new();
        for name in ["bevy_pbr", "bevy_render", "bevy_core_pipeline"] {
            let (version, source) = locked_package(&lock, name);
            if source.is_some() {
                registry_crates.push(format!("{name}-{version}"));
            } else {
                index_sources(&root.join("vendor").join(name).join("src"), &mut sources);
            }
        }
        let mut found = false;
        for registry_root in registry_roots() {
            let Ok(entries) = std::fs::read_dir(&registry_root) else {
                continue;
            };
            for registry in entries.map(|entry| entry.expect("registry entry").path()) {
                let directories: Vec<_> = registry_crates
                    .iter()
                    .map(|name| registry.join(name).join("src"))
                    .collect();
                if directories.iter().all(|directory| directory.is_dir()) {
                    for directory in directories {
                        index_sources(&directory, &mut sources);
                    }
                    found = true;
                    break;
                }
            }
            if found {
                break;
            }
        }
        assert!(
            found,
            "locked Bevy shader sources are missing; fetch the locked registry crates"
        );
        sources
    })
}

fn add_import(
    name: &str,
    composer: &mut Composer,
    defs: &Definitions,
    visiting: &mut HashSet<String>,
) -> Result<(), String> {
    if composer.contains_module(name) {
        return Ok(());
    }
    if !visiting.insert(name.to_owned()) {
        return Err(format!("cycle in import graph at {name}"));
    }
    let sources = real_sources();
    let Some((path, source)) = sources.get(name) else {
        return Err(format!("no source for import {name}"));
    };
    for import in get_preprocessor_data(source).1 {
        add_import(&import.import, composer, defs, visiting)?;
    }
    if let Err(error) = composer.add_composable_module(ComposableModuleDescriptor {
        source,
        file_path: &path.to_string_lossy(),
        language: ShaderLanguage::Wgsl,
        shader_defs: defs.clone(),
        ..Default::default()
    }) {
        return Err(error.emit_to_string(composer));
    }
    visiting.remove(name);
    Ok(())
}

fn defs_value(value: &ShaderDefValue) -> String {
    match value {
        ShaderDefValue::Bool(value) => value.to_string(),
        ShaderDefValue::Int(value) => value.to_string(),
        ShaderDefValue::UInt(value) => value.to_string(),
    }
}

/// A stable, sorted `NAME=value` list for an error message.
fn format_defs(defs: &Definitions) -> String {
    let mut entries: Vec<_> = defs
        .iter()
        .map(|(name, value)| format!("{name}={}", defs_value(value)))
        .collect();
    entries.sort();
    if entries.is_empty() {
        "<none>".to_owned()
    } else {
        entries.join(", ")
    }
}

fn compose(source: &str, file_path: &str, defs: Definitions) -> Result<wgpu::naga::Module, String> {
    // Bevy composes with the capabilities the device reports. Use all of them: this test checks
    // WGSL, not which device feature happens to be on.
    let mut composer = Composer::default().with_capabilities(Capabilities::all());
    let mut visiting = HashSet::new();
    for import in get_preprocessor_data(source).1 {
        add_import(&import.import, &mut composer, &defs, &mut visiting)?;
    }
    let module = composer
        .make_naga_module(NagaModuleDescriptor {
            source,
            file_path,
            shader_type: ShaderType::Wgsl,
            shader_defs: defs,
            additional_imports: &[],
        })
        .map_err(|error| error.emit_to_string(&composer))?;
    Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(&module)
        .map_err(|error| format!("{error:?}"))?;
    Ok(module)
}

fn base_defs() -> Definitions {
    let mut defs = Definitions::new();
    for (name, value) in [
        ("SAMPLES", 4),
        (
            "MAX_DIRECTIONAL_LIGHTS",
            bevy::pbr::MAX_DIRECTIONAL_LIGHTS as u32,
        ),
        (
            "MAX_CASCADES_PER_LIGHT",
            bevy::pbr::MAX_CASCADES_PER_LIGHT as u32,
        ),
        ("MAX_RECT_LIGHTS", bevy::pbr::MAX_RECT_LIGHTS as u32),
        ("AVAILABLE_STORAGE_BUFFER_BINDINGS", 8),
        ("MATERIAL_BIND_GROUP", 3),
    ] {
        defs.insert(name.to_owned(), ShaderDefValue::UInt(value));
    }
    for name in [
        "VERTEX_POSITIONS",
        "VERTEX_NORMALS",
        "VERTEX_UVS",
        "VERTEX_UVS_A",
        "VERTEX_UVS_B",
        "VERTEX_TANGENTS",
        "VERTEX_COLORS",
        "VERTEX_OUTPUT_INSTANCE_INDEX",
        "STANDARD_MATERIAL_BASE_COLOR_UV_B",
        "STANDARD_MATERIAL_NORMAL_MAP",
    ] {
        defs.insert(name.to_owned(), ShaderDefValue::Bool(true));
    }
    defs
}

fn with_ints(mut defs: Definitions, entries: &[(&str, i32)]) -> Definitions {
    for (name, value) in entries {
        defs.insert((*name).to_owned(), ShaderDefValue::Int(*value));
    }
    defs
}

fn with_uints(mut defs: Definitions, entries: &[(&str, u32)]) -> Definitions {
    for (name, value) in entries {
        defs.insert((*name).to_owned(), ShaderDefValue::UInt(*value));
    }
    defs
}

fn with_flags(mut defs: Definitions, names: &[&str]) -> Definitions {
    for name in names {
        defs.insert((*name).to_owned(), ShaderDefValue::Bool(true));
    }
    defs
}

fn variant(label: &str, defs: Definitions) -> (String, Definitions) {
    (label.to_owned(), defs)
}

/// The shader-def sets each shader is composed with. The first-party cases name the engine loader
/// that sets the defs; the vendored cases use Bevy's pipeline defs from its own Rust code.
fn variants(relative: &str) -> Vec<(String, Definitions)> {
    let base = base_defs();
    match relative {
        // `grass_render.rs` sets GRASS_GROUP, the vertex flags, the sample count and, for a scene
        // pipeline, GRASS_SCENE, MULTISAMPLED and the tonemapping defs.
        "crates/engine/src/shaders/grass.wgsl" => vec![
            variant(
                "grass without scene",
                with_uints(base.clone(), &[("GRASS_GROUP", 1)]),
            ),
            variant(
                "grass scene, MSAA and ACES tonemapping",
                with_uints(
                    with_flags(
                        base,
                        &[
                            "GRASS_SCENE",
                            "MULTISAMPLED",
                            "TONEMAP_IN_SHADER",
                            "TONEMAP_METHOD_ACES_FITTED",
                            "SHADOW_FILTER_METHOD_HARDWARE_2X2",
                        ],
                    ),
                    &[
                        ("GRASS_GROUP", 2),
                        ("TONEMAPPING_LUT_TEXTURE_BINDING_INDEX", 18),
                        ("TONEMAPPING_LUT_SAMPLER_BINDING_INDEX", 19),
                    ],
                ),
            ),
        ],
        // `retail_fog.rs` sets DEPTH_SAMPLES and, above one sample, MULTISAMPLED.
        "crates/engine/src/shaders/retail_fog.wgsl" => vec![
            variant(
                "one depth sample",
                with_uints(base.clone(), &[("DEPTH_SAMPLES", 1)]),
            ),
            variant(
                "multisampled depth",
                with_flags(with_uints(base, &[("DEPTH_SAMPLES", 4)]), &["MULTISAMPLED"]),
            ),
        ],
        // `water_depth.rs` sets BED_DEPTH_SAMPLES and, above one sample, MULTISAMPLED.
        "crates/engine/src/shaders/water_depth.wgsl" => vec![
            variant(
                "one bed-depth sample",
                with_uints(base.clone(), &[("BED_DEPTH_SAMPLES", 1)]),
            ),
            variant(
                "multisampled bed depth",
                with_flags(
                    with_uints(base, &[("BED_DEPTH_SAMPLES", 4)]),
                    &["MULTISAMPLED"],
                ),
            ),
        ],
        // `nif_material.rs` renders the colour pass and the prepass with MAY_DISCARD for masked
        // effects. The prepass entry point exists only under PREPASS_FRAGMENT.
        "crates/engine/src/shaders/effect_palette_prepass.wgsl" => vec![variant(
            "normal prepass with alpha test",
            with_flags(
                base,
                &[
                    "MAY_DISCARD",
                    "PREPASS_FRAGMENT",
                    "NORMAL_PREPASS",
                    "NORMAL_PREPASS_OR_DEFERRED_PREPASS",
                ],
            ),
        )],
        "crates/engine/src/shaders/effect_palette_functions.wgsl" => vec![
            variant("palette lookup", base.clone()),
            variant(
                "raw colour texels",
                with_flags(base, &["RAW_COLOUR_TEXELS"]),
            ),
        ],
        // `skin_material.rs` can add all of these to one pipeline: skinned and morphed actors,
        // OIT, visibility-range dithering, raw texels and the hair coverage ramp.
        "crates/engine/src/shaders/skyrim_skin.wgsl" => vec![
            variant(
                "static skin",
                with_flags(base.clone(), &["VISIBILITY_RANGE_DITHER"]),
            ),
            variant(
                "skinned, morphed, OIT, raw texels and hair coverage",
                with_flags(
                    base,
                    &[
                        "SKINNED",
                        "MORPH_TARGETS",
                        "OIT_ENABLED",
                        "VISIBILITY_RANGE_DITHER",
                        "RAW_COLOUR_TEXELS",
                        "SKYRIM_HAIR_ALPHA_TO_COVERAGE",
                    ],
                ),
            ),
        ],
        // `decal/forward_decal.wgsl` reads the depth prepass when the view has one.
        "vendor/bevy_pbr/src/decal/forward_decal.wgsl" => {
            vec![variant(
                "depth prepass",
                with_flags(base, &["DEPTH_PREPASS"]),
            )]
        }
        // `deferred_lighting.wgsl` runs after a deferred prepass that writes depth.
        "vendor/bevy_pbr/src/deferred/deferred_lighting.wgsl" => vec![variant(
            "deferred and depth prepass",
            with_flags(base, &["DEFERRED_PREPASS", "DEPTH_PREPASS"]),
        )],
        // `environment_map.wgsl` is the environment-map library; its bindings exist only when
        // Bevy's material pipeline sets ENVIRONMENT_MAP.
        "vendor/bevy_pbr/src/light_probe/environment_map.wgsl" => vec![
            variant(
                "environment map",
                with_flags(base.clone(), &["ENVIRONMENT_MAP"]),
            ),
            variant(
                "environment map with an array of light probes",
                with_flags(base, &["ENVIRONMENT_MAP", "MULTIPLE_LIGHT_PROBES_IN_ARRAY"]),
            ),
        ],
        // `ssao/mod.rs` sets SLICE_COUNT and SAMPLES_PER_SLICE_SIDE as Int shader defs.
        "vendor/bevy_pbr/src/ssao/ssao.wgsl" => vec![variant(
            "four SSAO slices",
            with_ints(base, &[("SLICE_COUNT", 4), ("SAMPLES_PER_SLICE_SIDE", 4)]),
        )],
        // `ssr` reads the depth prepass and the deferred gbuffer.
        "vendor/bevy_pbr/src/ssr/raymarch.wgsl" | "vendor/bevy_pbr/src/ssr/ssr.wgsl" => {
            vec![variant(
                "screen-space reflections with a depth and deferred prepass",
                with_flags(
                    base,
                    &[
                        "SCREEN_SPACE_REFLECTIONS",
                        "DEPTH_PREPASS",
                        "DEFERRED_PREPASS",
                    ],
                ),
            )]
        }
        // `meshlet/pipelines.rs` chooses one of these def sets for each of its pipelines.
        "vendor/bevy_pbr/src/meshlet/clear_visibility_buffer.wgsl"
        | "vendor/bevy_pbr/src/meshlet/resolve_render_targets.wgsl" => vec![variant(
            "visibility-buffer output",
            with_flags(base, &["MESHLET_VISIBILITY_BUFFER_RASTER_PASS_OUTPUT"]),
        )],
        "vendor/bevy_pbr/src/meshlet/cull_instances.wgsl" => vec![
            variant(
                "first instance cull",
                with_flags(
                    base.clone(),
                    &[
                        "MESHLET_INSTANCE_CULLING_PASS",
                        "MESHLET_FIRST_CULLING_PASS",
                    ],
                ),
            ),
            variant(
                "second instance cull",
                with_flags(
                    base,
                    &[
                        "MESHLET_INSTANCE_CULLING_PASS",
                        "MESHLET_SECOND_CULLING_PASS",
                    ],
                ),
            ),
        ],
        "vendor/bevy_pbr/src/meshlet/cull_bvh.wgsl" => vec![
            variant(
                "first BVH cull",
                with_flags(
                    base.clone(),
                    &["MESHLET_BVH_CULLING_PASS", "MESHLET_FIRST_CULLING_PASS"],
                ),
            ),
            variant(
                "second BVH cull",
                with_flags(
                    base,
                    &["MESHLET_BVH_CULLING_PASS", "MESHLET_SECOND_CULLING_PASS"],
                ),
            ),
        ],
        "vendor/bevy_pbr/src/meshlet/cull_clusters.wgsl" => vec![
            variant(
                "first cluster cull",
                with_flags(
                    base.clone(),
                    &["MESHLET_CLUSTER_CULLING_PASS", "MESHLET_FIRST_CULLING_PASS"],
                ),
            ),
            variant(
                "second cluster cull",
                with_flags(
                    base,
                    &[
                        "MESHLET_CLUSTER_CULLING_PASS",
                        "MESHLET_SECOND_CULLING_PASS",
                    ],
                ),
            ),
        ],
        "vendor/bevy_pbr/src/meshlet/meshlet_bindings.wgsl"
        | "vendor/bevy_pbr/src/meshlet/meshlet_cull_shared.wgsl" => vec![variant(
            "first instance cull bindings",
            with_flags(
                base,
                &[
                    "MESHLET_INSTANCE_CULLING_PASS",
                    "MESHLET_FIRST_CULLING_PASS",
                ],
            ),
        )],
        "vendor/bevy_pbr/src/meshlet/meshlet_mesh_material.wgsl"
        | "vendor/bevy_pbr/src/meshlet/visibility_buffer_resolve.wgsl" => vec![variant(
            "meshlet material pass",
            with_flags(base, &["MESHLET_MESH_MATERIAL_PASS"]),
        )],
        "vendor/bevy_pbr/src/meshlet/fill_counts.wgsl" => {
            vec![variant(
                "2D dispatch",
                with_flags(base, &["MESHLET_2D_DISPATCH"]),
            )]
        }
        "vendor/bevy_pbr/src/meshlet/remap_1d_to_2d_dispatch.wgsl" => {
            vec![variant("1D to 2D dispatch", base)]
        }
        "vendor/bevy_pbr/src/meshlet/visibility_buffer_hardware_raster.wgsl" => vec![variant(
            "hardware raster with visibility-buffer output",
            with_flags(
                base,
                &[
                    "MESHLET_VISIBILITY_BUFFER_RASTER_PASS",
                    "MESHLET_VISIBILITY_BUFFER_RASTER_PASS_OUTPUT",
                ],
            ),
        )],
        "vendor/bevy_pbr/src/meshlet/visibility_buffer_software_raster.wgsl" => vec![variant(
            "software raster with 2D dispatch and visibility-buffer output",
            with_flags(
                base,
                &[
                    "MESHLET_VISIBILITY_BUFFER_RASTER_PASS",
                    "MESHLET_VISIBILITY_BUFFER_RASTER_PASS_OUTPUT",
                    "MESHLET_2D_DISPATCH",
                ],
            ),
        )],
        _ => vec![variant("engine base", base)],
    }
}

/// Every `.wgsl` file the gate covers, in a stable order.
fn shader_files() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut files = Vec::new();
    for directory in [root.join("crates/engine/src/shaders"), root.join("vendor")] {
        let mut stack = vec![directory];
        while let Some(directory) = stack.pop() {
            let mut entries: Vec<_> = std::fs::read_dir(&directory)
                .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
                .map(|entry| entry.expect("shader directory entry").path())
                .collect();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    stack.push(path);
                } else if path
                    .extension()
                    .is_some_and(|extension| extension == "wgsl")
                {
                    files.push(path);
                }
            }
        }
    }
    files.push(root.join("crates/engine/src/streaming/lod_object.wgsl"));
    files.sort();
    files
}

/// Compose and validate one shader for one def set, or describe why it did not.
fn check_one(relative: &str, label: &str, defs: &Definitions) -> Result<(), String> {
    let path = workspace_root().join(relative);
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"));
    compose(&source, relative, defs.clone())
        .map(|_| ())
        .map_err(|error| {
            format!(
                "shader {relative} (variant: {label}; shader defs: {}) failed:\n{error}",
                format_defs(defs)
            )
        })
}

/// Every listed shader composes with the def sets its engine or Bevy pipeline uses and passes
/// Naga validation. A file may have several variants; all of them must pass.
#[test]
fn every_first_party_and_vendored_shader_composes_and_validates() {
    let exceptions: HashSet<&str> = KNOWN_EXCEPTIONS.iter().map(|(path, _)| *path).collect();
    let mut failures = Vec::new();
    let mut first_party_files = 0usize;
    let mut vendored_files = 0usize;
    let mut checked = 0usize;
    for path in shader_files() {
        let relative = relative_path(&path);
        if relative.starts_with("vendor/") {
            vendored_files += 1;
        } else {
            first_party_files += 1;
        }
        if exceptions.contains(relative.as_str()) {
            continue;
        }
        for (label, defs) in variants(&relative) {
            checked += 1;
            if let Err(error) = check_one(&relative, &label, &defs) {
                failures.push(error);
            }
        }
    }
    assert!(
        first_party_files > 0 && vendored_files > 0,
        "the shader walk found {first_party_files} first-party and {vendored_files} vendored \
         files; it must cover both trees"
    );
    assert!(
        checked >= (first_party_files + vendored_files).saturating_sub(exceptions.len()),
        "only {checked} shader variants were checked for {} files",
        first_party_files + vendored_files
    );
    assert!(
        failures.is_empty(),
        "{} shader variant(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn object_lod_shader_uses_signed_rgb_without_reconstructing_blue() {
    let source = include_str!("streaming/lod_object.wgsl");
    let (_, decode) = source
        .split_once("fn decode_object_atlas(rgb: vec3<f32>) -> vec3<f32> {")
        .expect("object atlas decode function");
    let expression = decode
        .trim_start()
        .strip_prefix("return ")
        .and_then(|body| body.split_once(';'))
        .expect("object atlas decode expression")
        .0;
    for (blue, expected_z) in [("0.125", "-0.75"), ("0.875", "0.75")] {
        let fixture = format!(
            "const rgb = vec3(0.75, 0.25, {blue});
             const normal = {expression};
             const_assert normal.x == 0.5 && normal.y == -0.5 && normal.z == {expected_z};"
        );
        wgpu::naga::front::wgsl::parse_str(&fixture)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&fixture)));
    }
    assert!(
        source
            .contains("decode_object_atlas(textureSample(normal_atlas, atlas_sampler, in.uv).rgb)")
    );
    let mut defs = base_defs();
    for name in [
        "VERTEX_UVS_B",
        "VERTEX_TANGENTS",
        "STANDARD_MATERIAL_BASE_COLOR_UV_B",
        "STANDARD_MATERIAL_NORMAL_MAP",
    ] {
        defs.remove(name);
    }
    let mut defs = with_flags(defs, &["MAY_DISCARD", "DISTANCE_FOG"]);
    compose(source, "lod_object.wgsl", defs.clone()).unwrap_or_else(|error| panic!("{error}"));
    defs.remove("VERTEX_COLORS");
    compose(source, "lod_object.wgsl", defs).unwrap_or_else(|error| panic!("{error}"));
}

/// A broken shader reports its file, its def set and the Naga message with a line. This is the
/// fail case of the gate: the same report the test prints for a real shader.
#[test]
fn a_broken_shader_names_the_file_defs_and_line() {
    let source = r#"
@fragment
fn fragment() -> @location(0) vec4<f32> {
    let colour = vec4<f32>(1.0, 0.0, 0.0, 1.0)
    return colour;
}
"#;
    let defs = with_flags(base_defs(), &["THIS_IS_A_TEST_DEF"]);
    let error = compose(source, "broken_test_shader.wgsl", defs.clone())
        .expect_err("the missing semicolon must fail");
    let report = format!(
        "shader broken_test_shader.wgsl (variant: test; shader defs: {}) failed:\n{error}",
        format_defs(&defs)
    );
    for needle in [
        "broken_test_shader.wgsl",
        "THIS_IS_A_TEST_DEF=true",
        "broken_test_shader.wgsl:5",
    ] {
        assert!(
            report.contains(needle),
            "the failure report does not name {needle:?}:\n{report}"
        );
    }
}

/// Every exception is a real file and states a reason, so a skip cannot hide a renamed or
/// deleted shader or become an unexplained "temporarily disabled" entry.
#[test]
fn known_exceptions_are_real_files_with_reasons() {
    for (relative, reason) in KNOWN_EXCEPTIONS {
        assert!(
            !reason.trim().is_empty(),
            "the exception for {relative} has no reason"
        );
        assert!(
            workspace_root().join(relative).is_file(),
            "the exception for {relative} names a file that does not exist"
        );
    }
}
