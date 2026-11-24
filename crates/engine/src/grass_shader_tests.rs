//! CPU validation of the production grass shader with locked Bevy imports.
//!
//! Reads WGSL from the workspace's vendored crates and the locked Cargo registry
//! crates supplied by a normal dependency fetch/build. Missing sources fail validation.
//! This checks WGSL semantics; GPU pipeline layouts and visual fidelity need separate evidence.

use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, NagaModuleDescriptor, ShaderDefValue, ShaderLanguage,
    ShaderType, get_preprocessor_data,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::OnceLock,
};
use wgpu::naga::{
    AddressSpace, ImageClass, ImageDimension, Module, ShaderStage, TypeInner,
    valid::{Capabilities, ValidationFlags, Validator},
};

const SHADER: &str = include_str!("shaders/grass.wgsl");
type Definitions = HashMap<String, ShaderDefValue>;
type Sources = HashMap<String, (PathBuf, String)>;

/// Index real library declarations, including libraries whose filename differs from their import.
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
            // PbrPlugin loads the shipped dummy library; MeshletPlugin replaces it.
            // Grass does not use MeshletPlugin. Keep Bevy's own selection.
            if path.ends_with(Path::new("meshlet/visibility_buffer_resolve.wgsl")) {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            if let Some(name) = get_preprocessor_data(&source).0 {
                assert!(
                    sources.insert(name.clone(), (path, source)).is_none(),
                    "duplicate real shader import {name}"
                );
            }
        }
    }
}

/// Discover Cargo registry roots read-only, honoring Cargo's configured home first.
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

/// Resolve local and registry package paths from the lockfile, independent of TOML key order.
fn locked_bevy_crates(lock: &str, vendor: &Path) -> [PathBuf; 3] {
    let document = lock
        .parse::<toml_edit::Document<String>>()
        .expect("parse Cargo.lock for real Bevy shader sources");
    let packages = document
        .get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .expect("Cargo.lock package tables");
    ["bevy_pbr", "bevy_render", "bevy_core_pipeline"].map(|name| {
        let mut matches = packages
            .iter()
            .filter(|package| package.get("name").and_then(|value| value.as_str()) == Some(name));
        let package = matches
            .next()
            .unwrap_or_else(|| panic!("Cargo.lock is missing {name}"));
        assert!(
            matches.next().is_none(),
            "ambiguous locked Bevy crate {name}"
        );
        let version = package
            .get("version")
            .and_then(|value| value.as_str())
            .unwrap_or_else(|| panic!("Cargo.lock has no version for {name}"));
        let Some(source) = package.get("source") else {
            return vendor.join(name);
        };
        assert!(
            source
                .as_str()
                .is_some_and(|source| source.starts_with("registry+")),
            "unsupported locked source for {name}"
        );
        PathBuf::from(format!("{name}-{version}"))
    })
}

/// LF and Windows CRLF lockfiles resolve the same exact packages despite intervening keys.
#[test]
fn locked_bevy_sources_accept_both_line_endings_and_key_order() {
    let lock = r#"version = 4
[[package]]
name = "unrelated"
version = "99.0.0"
[[package]]
name = "bevy_pbr"
version = "0.19.0"
[[package]]
version = "0.19.1"
name = "bevy_render"
[[package]]
source = "registry+https://github.com/rust-lang/crates.io-index"
version = "0.19.2"
name = "bevy_core_pipeline"
"#;
    let vendor = Path::new("vendor");
    for newline in ["\n", "\r\n"] {
        assert_eq!(
            locked_bevy_crates(&lock.replace('\n', newline), vendor),
            [
                vendor.join("bevy_pbr"),
                vendor.join("bevy_render"),
                PathBuf::from("bevy_core_pipeline-0.19.2")
            ]
        );
    }
}

/// Load real dependencies from vendor and one registry, failing if sources are absent.
fn real_sources() -> &'static Sources {
    static SOURCES: OnceLock<Sources> = OnceLock::new();
    SOURCES.get_or_init(|| {
        let vendor = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor");
        let crates = locked_bevy_crates(include_str!("../../../Cargo.lock"), &vendor);
        let roots = registry_roots();
        for root in &roots {
            let entries = match std::fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => panic!("read Cargo registry {}: {error}", root.display()),
            };
            let mut registries: Vec<_> = entries
                .map(|entry| entry.expect("Cargo registry directory entry").path())
                .filter(|path| path.is_dir())
                .collect();
            registries.sort();
            for registry in registries {
                // Joining an absolute vendor path keeps it outside the registry.
                let directories = crates
                    .each_ref()
                    .map(|path| registry.join(path).join("src"));
                if directories.iter().all(|directory| directory.is_dir()) {
                    let mut sources = Sources::new();
                    for directory in &directories {
                        index_sources(directory, &mut sources);
                    }
                    return sources;
                }
            }
        }
        panic!(
            "real Bevy WGSL sources unavailable: require paths {crates:?} with one Cargo \
             registry/src/<registry> directory; searched {roots:?}. Check the vendor tree \
             and fetch/build the locked registry dependencies with Cargo."
        );
    })
}

/// Register dependencies first, just as Bevy's shader cache does, without replacing source text.
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
        return Err(format!("cycle in real Bevy import graph at {name}"));
    }
    let (path, source) = real_sources()
        .get(name)
        .ok_or_else(|| format!("real Bevy source not found for import {name}"))?;
    // naga_oil requires even conditionally imported dependencies to be registered.
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

/// Supply desktop mesh/view constants and the grass pipeline's two bind-group arrangements.
pub(crate) fn definitions(scene: bool, samples: u32, attributes: u8, tonemap: bool) -> Definitions {
    let mut defs = Definitions::new();
    for (name, value) in [
        ("SAMPLES", samples),
        ("GRASS_GROUP", if scene { 2 } else { 1 }),
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
        (
            "TONEMAPPING_LUT_TEXTURE_BINDING_INDEX",
            if scene { 18 } else { 1 },
        ),
        (
            "TONEMAPPING_LUT_SAMPLER_BINDING_INDEX",
            if scene { 19 } else { 2 },
        ),
    ] {
        defs.insert(name.into(), ShaderDefValue::UInt(value));
    }
    for (bit, name) in [(1, "NORMAL"), (2, "UV"), (4, "COLOR"), (8, "TANGENT")] {
        if attributes & bit != 0 {
            defs.insert(name.into(), ShaderDefValue::Bool(true));
        }
    }
    for (enabled, name) in [
        (scene, "GRASS_SCENE"),
        (scene && samples > 1, "MULTISAMPLED"),
        (tonemap, "TONEMAP_IN_SHADER"),
        (tonemap, "TONEMAP_METHOD_ACES_FITTED"),
        (scene, "SHADOW_FILTER_METHOD_HARDWARE_2X2"),
    ] {
        // #ifdef tests presence, so disabled definitions must be absent.
        if enabled {
            defs.insert(name.into(), ShaderDefValue::Bool(true));
        }
    }
    defs
}

/// Compose unmodified production source and independently validate all of its entry points.
pub(crate) fn compose(source: &str, defs: Definitions) -> Result<Module, String> {
    let mut composer = Composer::default();
    let mut visiting = HashSet::new();
    for import in get_preprocessor_data(source).1 {
        add_import(&import.import, &mut composer, &defs, &mut visiting)?;
    }
    let module = composer
        .make_naga_module(NagaModuleDescriptor {
            source,
            file_path: "grass.wgsl",
            shader_type: ShaderType::Wgsl,
            shader_defs: defs,
            additional_imports: &[],
        })
        .map_err(|error| error.emit_to_string(&composer))?;
    Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(&module)
        .map_err(|error| format!("grass WGSL validation: {error:?}"))?;
    Ok(module)
}

/// Ensure both color and depth coverage entry points survive, with real shadow resources in scene mode.
fn check_contract(module: &Module, scene: bool) {
    // Bevy's prepared lights and shadow resources are fragment-only. Vertex
    // lighting must use the selected sun copied into the grass batch uniform.
    let info = Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(module)
        .expect("validated grass module");
    let vertex = module
        .entry_points
        .iter()
        .position(|entry| entry.name == "vertex")
        .unwrap();
    for (handle, global) in module.global_variables.iter() {
        if global
            .binding
            .as_ref()
            .is_some_and(|binding| binding.group == 0 && [1, 5, 6].contains(&binding.binding))
        {
            assert!(
                info.get_entry_point(vertex)[handle].is_empty(),
                "vertex must not access fragment-only prepared lights/shadows"
            );
        }
    }
    let mut entry_points: Vec<_> = module
        .entry_points
        .iter()
        .map(|entry| (entry.name.as_str(), entry.stage))
        .collect();
    entry_points.sort_by_key(|(name, _)| *name);
    assert_eq!(
        entry_points,
        [
            ("fragment", ShaderStage::Fragment),
            ("prepass", ShaderStage::Fragment),
            ("vertex", ShaderStage::Vertex),
        ],
        "exact grass entrypoint names and stages"
    );
    let bindings: HashSet<_> = module
        .global_variables
        .iter()
        .filter_map(|(_, global)| {
            global
                .binding
                .as_ref()
                .map(|binding| (binding.group, binding.binding))
        })
        .collect();
    let grass_group = if scene { 2 } else { 1 };
    let grass_bindings: HashSet<_> = bindings
        .iter()
        .copied()
        .filter(|(group, _)| *group == grass_group)
        .collect();
    assert_eq!(
        grass_bindings,
        HashSet::from([
            (grass_group, 0),
            (grass_group, 1),
            (grass_group, 2),
            (grass_group, 3),
            (grass_group, 4),
        ]),
        "exact grass uniform, albedo and normal map binding coordinates"
    );
    let resource_count = module
        .global_variables
        .iter()
        .filter(|(_, global)| global.binding.is_some())
        .count();
    assert_eq!(
        resource_count,
        bindings.len(),
        "duplicate resource binding coordinates"
    );
    assert!(bindings.contains(&(0, 0)), "missing real View binding");
    if scene {
        for binding in [1, 5, 6] {
            assert!(
                bindings.contains(&(0, binding)),
                "missing real light/shadow binding {binding}"
            );
        }
        let shadow_texture = module
            .global_variables
            .iter()
            .find_map(|(_, global)| {
                global
                    .binding
                    .as_ref()
                    .filter(|binding| binding.group == 0 && binding.binding == 5)
                    .map(|_| &module.types[global.ty].inner)
            })
            .expect("directional shadow texture at (0, 5)");
        assert!(
            matches!(
                shadow_texture,
                TypeInner::Image {
                    dim: ImageDimension::D2,
                    arrayed: true,
                    class: ImageClass::Depth { multi: false },
                }
            ),
            "directional shadows must use Bevy's depth texture array"
        );
        let shadow_sampler = module
            .global_variables
            .iter()
            .find_map(|(_, global)| {
                global
                    .binding
                    .as_ref()
                    .filter(|binding| binding.group == 0 && binding.binding == 6)
                    .map(|_| &module.types[global.ty].inner)
            })
            .expect("directional shadow sampler at (0, 6)");
        assert!(
            matches!(shadow_sampler, TypeInner::Sampler { comparison: true }),
            "directional shadows must use Bevy's comparison sampler"
        );
        assert!(
            module.global_variables.iter().any(|(_, global)| {
                global.space == AddressSpace::Uniform
                    && global
                        .binding
                        .as_ref()
                        .is_some_and(|binding| binding.group == 0 && binding.binding == 1)
            }),
            "prepared light uniform must occupy (0, 1)"
        );
    }
}

/// Check every optional grass vertex attribute, both coverage paths and HDR/LDR source branches.
#[test]
fn production_grass_baseline_and_depth_validate_with_real_bevy() {
    for samples in [1, 2, 4, 8] {
        for attributes in 0..16 {
            for tonemap in [false, true] {
                let module = compose(SHADER, definitions(false, samples, attributes, tonemap))
                    .unwrap_or_else(|error| panic!("baseline/depth samples={samples} attributes={attributes} tonemap={tonemap}:\n{error}"));
                check_contract(&module, false);
            }
        }
    }
}

/// Exercise all desktop native shadow filters and real prepared-light/cascade imports.
#[test]
fn production_grass_scene_shadows_validate_with_real_bevy() {
    for samples in [1, 2, 4, 8] {
        for attributes in 0..16 {
            for tonemap in [false, true] {
                for filter in [
                    "SHADOW_FILTER_METHOD_HARDWARE_2X2",
                    "SHADOW_FILTER_METHOD_GAUSSIAN",
                    "SHADOW_FILTER_METHOD_TEMPORAL",
                ] {
                    let mut defs = definitions(true, samples, attributes, tonemap);
                    defs.remove("SHADOW_FILTER_METHOD_HARDWARE_2X2");
                    defs.insert(filter.into(), ShaderDefValue::Bool(true));
                    let module = compose(SHADER, defs)
                        .unwrap_or_else(|error| panic!("scene samples={samples} attributes={attributes} tonemap={tonemap} filter={filter}:\n{error}"));
                    // A pre-fix shader without shadow imports must compose successfully,
                    // then fail this resource contract rather than fail test setup.
                    check_contract(&module, true);
                }
            }
        }
    }
}

/// Validate LUT tone mapping with the distinct scene and standalone view binding indices.
#[test]
fn production_grass_lut_tonemapping_validates_with_real_bevy() {
    for scene in [false, true] {
        for samples in [1, 2, 4, 8] {
            for attributes in 0..16 {
                let mut defs = definitions(scene, samples, attributes, true);
                defs.remove("TONEMAP_METHOD_ACES_FITTED");
                defs.insert(
                    "TONEMAP_METHOD_TONY_MC_MAPFACE".into(),
                    ShaderDefValue::Bool(true),
                );
                let module = compose(SHADER, defs).unwrap_or_else(|error| {
                    panic!("LUT scene={scene} samples={samples} attributes={attributes}:\n{error}")
                });
                check_contract(&module, scene);
                for expected in if scene { [18, 19] } else { [1, 2] } {
                    assert!(
                        module.global_variables.iter().any(|(_, global)| {
                            global.binding.as_ref().is_some_and(|binding| {
                                binding.group == 0 && binding.binding == expected
                            })
                        }),
                        "missing tone mapping resource at (0, {expected})"
                    );
                }
            }
        }
    }
}

/// Prove that a missing native shadow function cannot pass through an unused mock implementation.
#[test]
fn production_grass_validation_rejects_a_missing_shadow_function() {
    // First require the real shader to pass, so a setup error cannot satisfy the negative control.
    let defs = definitions(true, 4, 7, false);
    compose(SHADER, defs.clone()).expect("unmodified shader must validate before mutation");
    assert!(SHADER.contains("fetch_directional_shadow"));
    let broken = SHADER.replace("fetch_directional_shadow", "missing_grass_shadow_function");
    let error = compose(&broken, defs).expect_err("nonexistent native shadow function must fail");
    assert!(
        error.contains("missing_grass_shadow_function"),
        "unexpected failure: {error}"
    );
}

#[test]
/// Compile every experimental branch against real Bevy imports, coverage and shadow filters.
fn unlit_shadow_variants_validate_with_real_bevy() {
    for definition in [None::<&str>] {
        for attributes in 0..16 {
            for scene in [false, true] {
                for samples in [1, 4] {
                    for tonemap in [false, true] {
                        for filter in [
                            "SHADOW_FILTER_METHOD_HARDWARE_2X2",
                            "SHADOW_FILTER_METHOD_GAUSSIAN",
                            "SHADOW_FILTER_METHOD_TEMPORAL",
                        ] {
                            let mut defs = definitions(scene, samples, attributes, tonemap);
                            if let Some(definition) = definition {
                                defs.insert(definition.into(), ShaderDefValue::Bool(true));
                            }
                            if scene {
                                defs.remove("SHADOW_FILTER_METHOD_HARDWARE_2X2");
                                defs.insert(filter.into(), ShaderDefValue::Bool(true));
                            }
                            let module = compose(SHADER, defs).unwrap_or_else(|error| panic!("{definition:?} scene={scene} samples={samples} tonemap={tonemap} filter={filter}:\n{error}"));
                            check_contract(&module, scene);
                        }
                    }
                }
            }
        }
    }
}

/// Follow a compiled argument through loads/swizzles to its actual view uniform.
fn expression_global(
    function: &wgpu::naga::Function,
    expression: wgpu::naga::Handle<wgpu::naga::Expression>,
) -> Option<wgpu::naga::Handle<wgpu::naga::GlobalVariable>> {
    use wgpu::naga::Expression as E;
    match function.expressions[expression] {
        E::GlobalVariable(global) => Some(global),
        E::Load { pointer } => expression_global(function, pointer),
        E::AccessIndex { base, .. } => expression_global(function, base),
        E::Swizzle { vector, .. } => expression_global(function, vector),
        _ => None,
    }
}

/// Real Bevy apply_fog receives the view's fog, eye and each grass fragment's position.
#[test]
fn grass_fog_uses_view_uniforms_before_tonemapping() {
    use wgpu::naga::{Expression as E, Statement};
    for tonemap in [false, true] {
        let mut defs = definitions(true, 4, 15, tonemap);
        defs.insert("GRASS_TUNING".into(), ShaderDefValue::Bool(true));
        defs.insert("DISTANCE_FOG".into(), ShaderDefValue::Bool(true));
        let module = compose(SHADER, defs).expect("real fog shader validates");
        let (helper_handle, helper) = module
            .functions
            .iter()
            .find(|(_, function)| function.name.as_deref() == Some("grass_fog"))
            .expect("grass fog helper");
        let (callee, arguments) = helper
            .body
            .iter()
            .find_map(|statement| {
                if let Statement::Call {
                    function,
                    arguments,
                    ..
                } = statement
                {
                    Some((*function, arguments))
                } else {
                    None
                }
            })
            .expect("grass must call native fog");
        assert!(
            module.functions[callee]
                .name
                .as_deref()
                .unwrap()
                .contains("apply_fog")
        );
        assert_eq!(arguments.len(), 5);
        for (index, binding) in [(0, 13), (3, 0)] {
            let global =
                expression_global(helper, arguments[index]).expect("prepared view uniform");
            let resource = module.global_variables[global].binding.as_ref().unwrap();
            assert_eq!((resource.group, resource.binding), (0, binding));
        }
        for (index, expected) in [(1, 0), (2, 1), (4, 2)] {
            assert!(
                matches!(helper.expressions[arguments[index]], E::FunctionArgument(actual) if actual == expected)
            );
        }
        let fragment = &module
            .entry_points
            .iter()
            .find(|entry| entry.name == "fragment")
            .unwrap()
            .function;
        let calls: Vec<_> = fragment
            .body
            .iter()
            .filter_map(|statement| {
                if let Statement::Call { function, .. } = statement {
                    Some(*function)
                } else {
                    None
                }
            })
            .collect();
        let fog_index = calls
            .iter()
            .position(|handle| *handle == helper_handle)
            .expect("fragment fog path");
        if tonemap {
            let tone_index = calls
                .iter()
                .position(|handle| {
                    module.functions[*handle]
                        .name
                        .as_deref()
                        .is_some_and(|name| name.contains("tone_mapping"))
                })
                .expect("tone mapping call");
            assert!(
                fog_index < tone_index,
                "fog must compose in linear HDR before tone mapping"
            );
        }
        assert!(SHADER.contains("grass_fog(out.color,input.world_position.xyz,input.position.xy)"));
    }
}

/// With no DistanceFog component the compiled helper returns its exact input RGBA.
#[test]
fn grass_without_distance_fog_returns_unchanged_hdr() {
    use wgpu::naga::{Expression as E, Statement};
    for scene in [false, true] {
        let mut defs = definitions(scene, 4, 15, false);
        defs.insert("GRASS_TUNING".into(), ShaderDefValue::Bool(true));
        let module = compose(SHADER, defs).unwrap();
        let (_, helper) = module
            .functions
            .iter()
            .find(|(_, function)| function.name.as_deref() == Some("grass_fog"))
            .unwrap();
        assert!(
            !helper
                .body
                .iter()
                .any(|statement| matches!(statement, Statement::Call { .. }))
        );
        let Statement::Return { value: Some(value) } = helper.body.last().unwrap() else {
            panic!("fog-disabled return")
        };
        assert!(
            matches!(helper.expressions[*value], E::FunctionArgument(0)),
            "all input HDR channels and alpha must pass through unchanged"
        );
    }
}
