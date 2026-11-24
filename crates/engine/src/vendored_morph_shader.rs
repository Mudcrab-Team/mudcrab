//! Pins the bounded morph shader that Mudcrab carries in `vendor/bevy_pbr`.
//!
//! Bevy 0.19's `layer_count` can read `morph_descriptors[u32::MAX]` (bevyengine/bevy#25966), and on
//! Metal that read is a GPU page fault. `vendor/bevy_pbr` carries bevyengine/bevy#26504 plus a
//! Mudcrab bound in `prev_weight_at` (see `vendor/BEVY_PATCHES.md`). These tests read the vendored
//! WGSL from the repository and never skip, so a re-vendor, a Bevy upgrade or a hand edit that
//! drops a bound, renames the file or breaks the WGSL fails in CI instead of bringing the fault
//! back.

use crate::bevy_source::{read_source, required_source};
use bevy::shader::{Shader, ShaderImport};
use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, NagaModuleDescriptor, ShaderDefValue, ShaderLanguage,
    ShaderType,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// The `bevy_pbr` version the vendored patches were ported to.
const VENDORED_BEVY_PBR: &str = "0.19.0";

const MORPH_WGSL: &str = "src/render/morph.wgsl";
const DESCRIPTOR_MARKER: &str = "mudcrab: bounded morph descriptor read";
const PREVIOUS_WEIGHTS_MARKER: &str = "mudcrab: bounded previous-weights read";

/// The bounded read in `layer_count`, storage-buffer branch, as the vendored file carries it.
const BOUNDED_LAYER_COUNT: &str =
    "    if (morph_descriptor_index >= arrayLength(&morph_descriptors)) {
        return 0u;
    }
    return morph_descriptors[morph_descriptor_index].weight_count;";

/// The bounded read in `prev_weight_at`, storage-buffer branch.
const BOUNDED_PREVIOUS_WEIGHT: &str =
    "    let prev_weight_count = arrayLength(&prev_morph_weights);
    if (weights_offset >= prev_weight_count || weight_index >= prev_weight_count - weights_offset) {
        return weight_at(weight_index, instance_index);
    }
    return prev_morph_weights[weights_offset + weight_index];";

/// An entry point that reaches every function of the morph library the vertex shaders use.
const ENTRY: &str = "#import bevy_pbr::morph
@vertex
fn vertex(@builtin(instance_index) instance_index: u32) -> @builtin(position) vec4<f32> {
    var sum = vec3<f32>(0.0);
    for (var i: u32 = 0u; i < bevy_pbr::morph::layer_count(instance_index); i++) {
        let weight = bevy_pbr::morph::weight_at(i, instance_index)
            + bevy_pbr::morph::prev_weight_at(i, instance_index);
        sum += weight * (bevy_pbr::morph::morph_position(0u, i, instance_index)
            + bevy_pbr::morph::morph_normal(0u, i, instance_index)
            + bevy_pbr::morph::morph_tangent(0u, i, instance_index));
    }
    return vec4<f32>(sum, 1.0);
}
";

fn vendored_bevy_pbr() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/bevy_pbr")
}

/// The text of `function` in `source`, from `fn name(` to the next top-level `}`.
fn function<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("{MORPH_WGSL} has no fn {name}"));
    let end = source[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("fn {name} in {MORPH_WGSL} has no end"));
    &source[start..start + end]
}

/// Composes the morph library, with its real `mesh_types` and `mesh_bindings` imports from the
/// same crate, and validates it with Naga.
fn compose(dir: &Path, morph: &str, shader_defs: HashMap<String, ShaderDefValue>) {
    let mut composer = Composer::default();
    let mesh_types = required_source(dir, "src/render/mesh_types.wgsl");
    let mesh_bindings = required_source(dir, "src/render/mesh_bindings.wgsl");
    for (path, source) in [
        ("mesh_types.wgsl", mesh_types.as_str()),
        ("mesh_bindings.wgsl", mesh_bindings.as_str()),
        ("morph.wgsl", morph),
    ] {
        composer
            .add_composable_module(ComposableModuleDescriptor {
                source,
                file_path: path,
                language: ShaderLanguage::Wgsl,
                shader_defs: shader_defs.clone(),
                ..Default::default()
            })
            .unwrap_or_else(|error| panic!("{path} does not compose: {error:?}"));
    }
    if let Err(error) = composer.make_naga_module(NagaModuleDescriptor {
        source: ENTRY,
        file_path: "morph_entry.wgsl",
        shader_type: ShaderType::Wgsl,
        shader_defs,
        additional_imports: &[],
    }) {
        panic!(
            "the morph library fails Naga validation with {:?}:\n{}",
            ENTRY.lines().next(),
            error.emit_to_string(&composer)
        );
    }
}

/// Every check on one `bevy_pbr` crate directory. Nothing here skips: a missing manifest or
/// shader, a missing bound or invalid WGSL panics.
fn check_bounded_morph_shader(dir: &Path) {
    let manifest = required_source(dir, "Cargo.toml");
    assert!(
        manifest.contains("name = \"bevy_pbr\"")
            && manifest.contains(&format!("version = \"{VENDORED_BEVY_PBR}\"")),
        "{} is not bevy_pbr {VENDORED_BEVY_PBR}: re-port the morph bounds \
         (vendor/BEVY_PATCHES.md), or remove them if this Bevy fixes bevyengine/bevy#25966, then \
         update VENDORED_BEVY_PBR",
        dir.display()
    );
    let morph = required_source(dir, MORPH_WGSL);

    // Bevy loads exactly this file, under the import path the vertex shaders use.
    assert!(
        required_source(dir, "src/render/mesh.rs")
            .contains("load_shader_library!(app, \"morph.wgsl\")"),
        "bevy_pbr no longer loads morph.wgsl from render/mesh.rs"
    );
    assert_eq!(
        Shader::from_wgsl(morph.clone(), "morph.wgsl").import_path,
        ShaderImport::Custom("bevy_pbr::morph".to_owned()),
        "morph.wgsl no longer defines the bevy_pbr::morph import path"
    );

    // Both bounds are present, once, inside the functions they protect.
    assert_eq!(
        morph.matches(DESCRIPTOR_MARKER).count(),
        1,
        "{MORPH_WGSL} must carry exactly one `{DESCRIPTOR_MARKER}` marker"
    );
    assert_eq!(
        morph.matches(PREVIOUS_WEIGHTS_MARKER).count(),
        1,
        "{MORPH_WGSL} must carry exactly one `{PREVIOUS_WEIGHTS_MARKER}` marker"
    );
    let layer_count = function(&morph, "layer_count");
    assert!(
        layer_count.contains(DESCRIPTOR_MARKER) && layer_count.contains(BOUNDED_LAYER_COUNT),
        "layer_count lost its morph descriptor bound:\n{layer_count}"
    );
    let prev_weight_at = function(&morph, "prev_weight_at");
    assert!(
        prev_weight_at.contains(PREVIOUS_WEIGHTS_MARKER)
            && prev_weight_at.contains(BOUNDED_PREVIOUS_WEIGHT),
        "prev_weight_at lost its previous-weights bound:\n{prev_weight_at}"
    );

    // The hand-ported WGSL is valid on both morph paths: storage buffers (Metal, Vulkan, DX12)
    // and uniform buffers with a morph texture (WebGL-class limits).
    let morphed = ("MORPH_TARGETS".to_owned(), ShaderDefValue::Bool(true));
    compose(dir, &morph, HashMap::from([morphed.clone()]));
    compose(
        dir,
        &morph,
        HashMap::from([
            morphed,
            (
                "SKINS_USE_UNIFORM_BUFFERS".to_owned(),
                ShaderDefValue::Bool(true),
            ),
            (
                "PER_OBJECT_BUFFER_BATCH_SIZE".to_owned(),
                ShaderDefValue::UInt(64),
            ),
        ]),
    );
}

#[test]
fn the_vendored_morph_shader_stays_bounded() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(
        required_source(&workspace, "Cargo.toml")
            .contains("bevy_pbr = { path = \"vendor/bevy_pbr\" }"),
        "the workspace no longer patches bevy_pbr with vendor/bevy_pbr, so the build uses an \
         unbounded morph shader: restore the patch, or move these checks to the new source"
    );
    assert_eq!(
        crate::bevy_source::locked_bevy_pbr_version(),
        VENDORED_BEVY_PBR,
        "Cargo.lock no longer pins the vendored bevy_pbr version"
    );
    check_bounded_morph_shader(&vendored_bevy_pbr());
}

/// A copy of the vendored crate in a temporary directory, without `skip` and with `edit` applied
/// to `morph.wgsl`. The files are written with LF endings.
fn vendored_copy(skip: Option<&str>, edit: impl Fn(String) -> String) -> tempfile::TempDir {
    vendored_copy_with_endings(skip, edit, "\n")
}

/// Like [`vendored_copy`], but written with `ending` as the line ending, as a checkout made with
/// `core.autocrlf` would. `edit` always sees LF text: the sources are read through
/// [`read_source`], so it does not depend on the checkout that runs the test.
fn vendored_copy_with_endings(
    skip: Option<&str>,
    edit: impl Fn(String) -> String,
    ending: &str,
) -> tempfile::TempDir {
    let source = vendored_bevy_pbr();
    let copy = tempfile::tempdir().unwrap();
    for relative in [
        "Cargo.toml",
        "src/render/mesh.rs",
        "src/render/mesh_types.wgsl",
        "src/render/mesh_bindings.wgsl",
        MORPH_WGSL,
    ] {
        if Some(relative) == skip {
            continue;
        }
        let target = copy.path().join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let text = read_source(&source.join(relative)).unwrap();
        let text = if relative == MORPH_WGSL {
            edit(text)
        } else {
            text
        };
        std::fs::write(target, text.replace('\n', ending)).unwrap();
    }
    copy
}

#[test]
fn an_unchanged_copy_passes_the_checks() {
    check_bounded_morph_shader(vendored_copy(None, |text| text).path());
}

#[test]
#[should_panic(expected = "morph.wgsl is missing or unreadable")]
fn a_missing_morph_shader_fails() {
    check_bounded_morph_shader(vendored_copy(Some(MORPH_WGSL), |text| text).path());
}

#[test]
#[should_panic(expected = "morph.wgsl is missing or unreadable")]
fn a_morph_shader_moved_to_wesl_fails() {
    let copy = vendored_copy(Some(MORPH_WGSL), |text| text);
    let original = read_source(&vendored_bevy_pbr().join(MORPH_WGSL)).unwrap();
    std::fs::write(copy.path().join("src/render/morph.wesl"), original).unwrap();
    check_bounded_morph_shader(copy.path());
}

#[test]
#[should_panic(expected = "layer_count lost its morph descriptor bound")]
fn an_unbounded_layer_count_fails() {
    // Upstream Bevy 0.19.0's read, with the marker kept as a comment elsewhere in the function.
    let copy = vendored_copy(None, |text| {
        text.replace(
            BOUNDED_LAYER_COUNT,
            "    return morph_descriptors[morph_descriptor_index].weight_count;",
        )
    });
    check_bounded_morph_shader(copy.path());
}

#[test]
#[should_panic(expected = "must carry exactly one")]
fn a_shader_without_the_marker_fails() {
    let copy = vendored_copy(None, |text| text.replace(DESCRIPTOR_MARKER, "upstream"));
    check_bounded_morph_shader(copy.path());
}

#[test]
#[should_panic(expected = "fails Naga validation")]
fn invalid_wgsl_in_the_port_fails() {
    let copy = vendored_copy(None, |text| {
        text.replace(
            "let vertex_count = morph_descriptors[morph_descriptor_index].vertex_count;",
            "let vertex_count = morph_descriptors[morph_descriptor_index].vertex_count + undefined_name;",
        )
    });
    check_bounded_morph_shader(copy.path());
}

/// The message of the panic that `run` raises, or `None` when it does not panic.
fn panic_message(run: impl FnOnce()) -> Option<String> {
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).err()?;
    Some(
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
            })
            .unwrap_or_default(),
    )
}

/// An intact and a mutated shader give the same verdict with LF and with CRLF endings on disk
/// (a Windows checkout with `core.autocrlf`).
#[test]
fn every_line_ending_gives_the_same_verdict() {
    for ending in ["\n", "\r\n"] {
        let intact = vendored_copy_with_endings(None, |text| text, ending);
        let on_disk = std::fs::read_to_string(intact.path().join(MORPH_WGSL)).unwrap();
        assert_eq!(
            on_disk.contains("\r\n"),
            ending == "\r\n",
            "the copy really has {ending:?} endings"
        );
        check_bounded_morph_shader(intact.path());

        let mutated = vendored_copy_with_endings(
            None,
            |text| {
                text.replace(
                    BOUNDED_LAYER_COUNT,
                    "    return morph_descriptors[morph_descriptor_index].weight_count;",
                )
            },
            ending,
        );
        let message = panic_message(|| check_bounded_morph_shader(mutated.path()))
            .unwrap_or_else(|| panic!("an unbounded layer_count passed with {ending:?} endings"));
        assert!(
            message.contains("layer_count lost its morph descriptor bound"),
            "{ending:?}: {message}"
        );
    }
}
