//! CPU composition and validation of the ordinary indexed cluster vertex entries.
//!
//! Prerequisites: installed locked Bevy 0.19.0 registry sources
//! (`bevy_core_pipeline`, `bevy_light`) and the checkout's patched
//! `vendor/bevy_pbr-0.19.0/src` and `vendor/bevy_render-0.19.0/src`.
//! Uses existing `naga_oil` and `wgpu` dependencies.
//! No Git history, optional modules, fixtures, GPU or new dependency required at runtime.
//! This restores eight CPU shader checks; it does not prove device pipeline layouts.

use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, NagaModuleDescriptor, ShaderDefValue,
    get_preprocessor_data,
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use wgpu::naga::{
    self,
    valid::{Capabilities, ValidationFlags, Validator},
};

type Sources = HashMap<String, (String, String)>;

fn registry() -> PathBuf {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .expect("CARGO_HOME, USERPROFILE or HOME required"),
            )
            .join(".cargo")
        });
    fs::read_dir(home.join("registry/src"))
        .expect("installed Cargo registry sources required")
        .map(|entry| entry.expect("Cargo registry entry").path())
        .find(|path| {
            ["bevy_core_pipeline", "bevy_light"]
                .iter()
                .all(|package| path.join(format!("{package}-0.19.0/src")).is_dir())
        })
        .expect("installed locked bevy_core_pipeline and bevy_light 0.19.0 sources required")
}

fn collect(path: &Path, sources: &mut Sources) {
    for entry in fs::read_dir(path).unwrap_or_else(|error| {
        panic!(
            "installed Bevy source directory {}: {error}",
            path.display()
        )
    }) {
        let path = entry.expect("Bevy source entry").path();
        if path.is_dir() {
            collect(&path, sources);
        } else if path.extension().is_some_and(|ext| ext == "wgsl") {
            // PbrPlugin loads the dummy resolver under this import name. Only Bevy's
            // experimental MeshletPlugin replaces it; our cluster renderer does not.
            if path.ends_with("meshlet/visibility_buffer_resolve.wgsl") {
                continue;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("real shader source {}: {error}", path.display()));
            if let Some(name) = get_preprocessor_data(&source).0 {
                if let Some((previous, _)) = sources.get(&name) {
                    panic!(
                        "duplicate real shader module {name}: {previous} and {}",
                        path.display()
                    );
                }
                sources.insert(name, (path.display().to_string(), source));
            }
        }
    }
}

// Match Cargo's source ownership: unpatched crates from the registry, patched
// PBR and renderer from this checkout, plus the current terrain clip source.
fn source_map() -> Sources {
    let registry = registry();
    let mut sources = Sources::new();
    for package in ["bevy_core_pipeline", "bevy_light"] {
        collect(
            &registry.join(format!("{package}-0.19.0/src")),
            &mut sources,
        );
    }
    for package in ["bevy_pbr", "bevy_render"] {
        collect(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../vendor/{package}-0.19.0/src")),
            &mut sources,
        );
    }
    sources.insert(
        "cluster_terrain::clip".into(),
        (
            "current/terrain_clip.wgsl".into(),
            include_str!("shaders/terrain_clip.wgsl").into(),
        ),
    );
    sources
}

fn add(
    name: &str,
    sources: &Sources,
    composer: &mut Composer,
    defs: &HashMap<String, ShaderDefValue>,
) {
    if composer.contains_module(name) {
        return;
    }
    let (path, source) = sources
        .get(name)
        .unwrap_or_else(|| panic!("missing real shader module {name}"));
    for import in get_preprocessor_data(source).1 {
        add(&import.import, sources, composer, defs);
    }
    if let Err(error) = composer.add_composable_module(ComposableModuleDescriptor {
        source,
        file_path: path,
        shader_defs: defs.clone(),
        ..Default::default()
    }) {
        panic!("{}", error.emit_to_string(composer));
    }
}

fn compose(
    source: &str,
    path: &str,
    sources: &Sources,
    defs: HashMap<String, ShaderDefValue>,
) -> naga::Module {
    let mut composer = Composer::default().with_capabilities(Capabilities::all());
    for import in get_preprocessor_data(source).1 {
        // Match Bevy's ShaderCache: resolve every declared import, including
        // imports inside disabled preprocessor branches, before composition.
        add(&import.import, sources, &mut composer, &defs);
    }
    let module = composer
        .make_naga_module(NagaModuleDescriptor {
            source,
            file_path: path,
            shader_defs: defs,
            ..Default::default()
        })
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&composer)));
    Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(&module)
        .unwrap_or_else(|error| panic!("{path}: {error:?}"));
    module
}

fn definitions() -> HashMap<String, ShaderDefValue> {
    // Expanded result of the original definitions(false, false, fields), with
    // lighting_tests::defs(false, 4, &[]) as its base. Preserve its complete set,
    // including the derived VERTEX_UVS flag; bindless and visibility dither stay off.
    let mut defs = HashMap::new();
    for name in [
        "MESH_PIPELINE",
        "VERTEX_OUTPUT_INSTANCE_INDEX",
        "VERTEX_POSITIONS",
        "VERTEX_NORMALS",
        "VIEW_PROJECTION_PERSPECTIVE",
        "DISTANCE_FOG",
        "DFG_LUT",
        "PBR_SPECULAR_TEXTURES_SUPPORTED",
        "VERTEX_UVS_A",
        "VERTEX_UVS_B",
        "VERTEX_COLORS",
        "VERTEX_TANGENTS",
        "NORMAL_PREPASS",
        "NORMAL_PREPASS_OR_DEFERRED_PREPASS",
        "MOTION_VECTOR_PREPASS",
        "VERTEX_UVS",
    ] {
        defs.insert(name.into(), ShaderDefValue::Bool(true));
    }
    for (name, value) in [
        ("MATERIAL_BIND_GROUP", 3),
        ("AVAILABLE_STORAGE_BUFFER_BINDINGS", 8),
        ("MAX_DIRECTIONAL_LIGHTS", 10),
        ("MAX_CASCADES_PER_LIGHT", 4),
        ("MAX_RECT_LIGHTS", 8),
    ] {
        defs.insert(name.into(), ShaderDefValue::UInt(value));
    }
    defs
}

#[test]
fn indexed_cluster_vertex_entries_validate_for_camera_and_shadow_variants() {
    let sources = source_map();
    for (path, source) in [
        (
            "cluster_draw.wgsl",
            include_str!("shaders/cluster_draw.wgsl"),
        ),
        (
            "cluster_prepass.wgsl",
            include_str!("shaders/cluster_prepass.wgsl"),
        ),
        (
            "cluster_shadow_mask.wgsl",
            include_str!("shaders/cluster_shadow_mask.wgsl"),
        ),
        (
            "cluster_shadow.wgsl",
            include_str!("shaders/cluster_shadow.wgsl"),
        ),
    ] {
        for emulated_depth in [false, true] {
            let mut defs = definitions();
            if emulated_depth {
                defs.insert(
                    "UNCLIPPED_DEPTH_ORTHO_EMULATION".into(),
                    ShaderDefValue::Bool(true),
                );
            }
            let module = compose(source, path, &sources, defs);
            assert!(
                module
                    .entry_points
                    .iter()
                    .any(|entry| entry.stage == naga::ShaderStage::Vertex),
                "{path}, emulated_depth={emulated_depth}: missing vertex entry"
            );
            assert!(
                module
                    .global_variables
                    .iter()
                    .all(|(_, global)| global.name.as_deref() != Some("cluster_triangles")),
                "{path}, emulated_depth={emulated_depth}: obsolete triangle storage"
            );
        }
    }
}
