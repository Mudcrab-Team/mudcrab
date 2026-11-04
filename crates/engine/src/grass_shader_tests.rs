//! CPU validation of the production grass shader with real Bevy 0.19.0 imports.
//!
//! Reads downloaded WGSL sources under Cargo's registry in `CARGO_HOME`, or the
//! standard user `.cargo` directory. No custom environment variable is required.
//! Missing sources are errors, never skipped tests or mock imports. These tests
//! require the unpacked `bevy_pbr`, `bevy_render`, and `bevy_core_pipeline` 0.19.0
//! crates that a normal Cargo dependency fetch/build supplies.
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
        .map(|entry| entry.expect("registry directory entry").path())
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

/// Load one coherent registry's real dependencies once, failing clearly if they are absent.
fn real_sources() -> &'static Sources {
    static SOURCES: OnceLock<Sources> = OnceLock::new();
    SOURCES.get_or_init(|| {
        let crates = [
            "bevy_pbr-0.19.0",
            "bevy_render-0.19.0",
            "bevy_core_pipeline-0.19.0",
        ];
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
                if crates
                    .iter()
                    .all(|name| registry.join(name).join("src").is_dir())
                {
                    let mut sources = Sources::new();
                    for name in crates {
                        index_sources(&registry.join(name).join("src"), &mut sources);
                    }
                    return sources;
                }
            }
        }
        panic!(
            "real Bevy WGSL sources unavailable: require {crates:?} together in a Cargo \
             registry/src/<registry> directory; searched {roots:?}. Fetch/build the locked \
             dependencies with Cargo; preserve their unpacked sources when running tests. \
             No shader validation was skipped."
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
            (grass_group, 5),
            (grass_group, 6),
        ]),
        "exact grass uniform, albedo and wind binding coordinates"
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
    for samples in [1, 4] {
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
    for samples in [1, 4] {
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
        for samples in [1, 4] {
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
    for definition in [
        None,
        Some("GRASS_SHADING_E"),
        Some("GRASS_SHADING_F"),
        Some("GRASS_TUNING"),
    ] {
        for attributes in 0..8 {
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
