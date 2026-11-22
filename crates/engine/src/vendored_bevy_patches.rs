//! Pins the Bevy fixes that Mudcrab carries in `vendor/bevy_pbr` and `vendor/bevy_render`.
//!
//! `vendor/BEVY_PATCHES.md` lists each carried change. Every hunk has a `mudcrab:` marker comment.
//! These tests read the vendored sources from the repository and never skip, so a re-vendor, a
//! Bevy upgrade or a hand edit that drops a hunk fails in CI instead of bringing the bug back:
//!
//! - bevyengine/bevy#26504: the morph descriptor is pending until its mesh is ready;
//! - bevyengine/bevy#26095: stale GPU bin-unpacking jobs and bind groups are cleared every frame;
//! - bevyengine/bevy#26588: the GPU clustering staging-buffer leak.
//!
//! Code checks run on source with whole-line `//` comments removed and whitespace collapsed, so
//! code turned into a comment, or re-wrapped by `rustfmt`, is judged by what it really does. Marker
//! checks read the raw text. The morph shader bounds have their own test in `vendored_morph_shader`.

use crate::bevy_source::{read_source, required_source};
use std::path::{Path, PathBuf};

const PBR_MORPH: &str = "src/render/morph.rs";
const PBR_MESH: &str = "src/render/mesh.rs";
const PBR_PREPROCESS: &str = "src/render/gpu_preprocess.rs";
const PBR_CLUSTER: &str = "src/cluster/gpu.rs";
const RENDER_PREPROCESSING: &str = "src/batching/gpu_preprocessing.rs";

const MARKER_26504: &str = "mudcrab: bevyengine/bevy#26504";
const MARKER_26095: &str = "mudcrab: bevyengine/bevy#26095";
const MARKER_26588: &str = "mudcrab: bevyengine/bevy#26588";
const MARKER_POWER_RAMP: &str = "mudcrab: ";

fn vendor(crate_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor")
        .join(crate_name)
}

/// `source` without whole-line `//` comments (including `///` and `//!`), with every run of
/// whitespace collapsed to one space.
fn code(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The raw text of the top-level Rust function `name`, from `fn name(` to the closing `}` at
/// column 0.
fn function<'a>(source: &'a str, file: &str, name: &str) -> &'a str {
    let start = source
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("{file} has no fn {name}: re-port the vendored patch"));
    let end = source[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("fn {name} in {file} has no end"));
    &source[start..start + end]
}

/// Whether `manifest` has the exact entry `name = { path = "vendor/name" }` as a whole,
/// non-comment line inside the `[patch.crates-io]` table.
fn patches_crate(manifest: &str, name: &str) -> bool {
    let entry = format!("{name} = {{ path = \"vendor/{name}\" }}");
    let mut in_patch = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_patch = line == "[patch.crates-io]";
        } else if in_patch && line == entry {
            return true;
        }
    }
    false
}

/// Every check on one pair of `bevy_pbr` and `bevy_render` directories. Nothing here skips: a
/// missing file, marker or hunk panics.
fn check_patches(pbr: &Path, render: &Path) {
    // bevyengine/bevy#26504 (the shader half is in `vendored_morph_shader`). The wait only works
    // if `prepare_morph_descriptors` records the pending entities: a re-vendor that restores
    // 0.19.0's bare `continue;` leaves the set empty and brings the `u32::MAX` index back.
    let mesh = required_source(pbr, PBR_MESH);
    assert!(
        mesh.contains(MARKER_26504),
        "{PBR_MESH} lost its `{MARKER_26504}` marker"
    );
    assert!(
        code(&mesh).contains("if morph_indices.morph_descriptor_pending(entity) { return None; }"),
        "{PBR_MESH} no longer returns None while the morph descriptor is pending (bevy#26504): \
         an instance can get the missing descriptor index u32::MAX"
    );
    let morph = required_source(pbr, PBR_MORPH);
    let morph_code = code(&morph);
    for needed in [
        "mesh_id: AssetId<Mesh>,",
        "pending_descriptors: MainEntityHashSet,",
        "pending_descriptors.contains(&main_entity)",
    ] {
        assert!(
            morph_code.contains(needed),
            "{PBR_MORPH} lost `{needed}` (bevy#26504)"
        );
    }
    let descriptors = code(function(&morph, PBR_MORPH, "prepare_morph_descriptors"));
    assert_eq!(
        descriptors.matches("pending_descriptors.clear();").count(),
        1,
        "prepare_morph_descriptors must clear pending_descriptors once per frame (bevy#26504)"
    );
    let insert = "pending_descriptors.insert(morph_target_main_entity);";
    assert_eq!(
        descriptors.matches(insert).count(),
        2,
        "prepare_morph_descriptors must record a pending entity in both places where it skips \
         one (bevy#26504): the set stays empty, the mesh.rs wait never fires, and the u32::MAX \
         descriptor index, a Metal page fault, returns"
    );
    for (skip, needed) in [
        (
            "mesh missing",
            format!("let Some(mesh) = meshes.get(mesh_id) else {{ {insert} continue; }};"),
        ),
        (
            "morph slice missing",
            format!(
                "mesh_allocator.mesh_morph_target_slice(&mesh_id) else {{ \
                 if mesh.has_morph_targets() {{ {insert} }} continue; }};"
            ),
        ),
    ] {
        assert!(
            descriptors.contains(&needed),
            "prepare_morph_descriptors lost the pending record when the {skip} (bevy#26504)"
        );
    }

    // bevyengine/bevy#26095, bevy_pbr half: bind groups are cleared every frame.
    let preprocess = required_source(pbr, PBR_PREPROCESS);
    assert_eq!(
        preprocess.matches(MARKER_26095).count(),
        1,
        "{PBR_PREPROCESS} must carry exactly one `{MARKER_26095}` marker"
    );
    let bind_groups = function(
        &preprocess,
        PBR_PREPROCESS,
        "prepare_preprocess_bind_groups",
    );
    assert!(
        bind_groups.contains(MARKER_26095)
            && code(bind_groups).contains("bin_unpacking_bind_groups.clear();"),
        "prepare_preprocess_bind_groups no longer clears the bin-unpacking bind groups \
         (bevy#26095): stale jobs can bind other jobs' metadata"
    );

    // bevyengine/bevy#26095, bevy_render half: the jobs of every view are cleared every frame.
    let preprocessing = required_source(render, RENDER_PREPROCESSING);
    assert!(
        preprocessing.contains(MARKER_26095),
        "{RENDER_PREPROCESSING} lost its `{MARKER_26095}` markers"
    );
    let clear = function(
        &preprocessing,
        RENDER_PREPROCESSING,
        "clear_bin_unpacking_buffers",
    );
    assert!(
        clear.contains(MARKER_26095),
        "clear_bin_unpacking_buffers lost its `{MARKER_26095}` marker"
    );
    let clear_code = code(clear);
    for needed in [
        "bin_unpacking_buffers.bin_unpacking_metadata.clear();",
        "for view_phase_buffers in bin_unpacking_buffers.view_phase_buffers.values_mut() {",
        "view_phase_buffers.indexed_unpacking_jobs.clear();",
        "view_phase_buffers.non_indexed_unpacking_jobs.clear();",
    ] {
        assert!(
            clear_code.contains(needed),
            "clear_bin_unpacking_buffers lost `{needed}` (bevy#26095): a skipped view keeps \
             last frame's unpacking jobs, which point at metadata that was rewritten:\n{clear}"
        );
    }

    // bevyengine/bevy#26588: the write-only staging list stays removed.
    let cluster = required_source(pbr, PBR_CLUSTER);
    assert!(
        cluster.contains(MARKER_26588),
        "{PBR_CLUSTER} lost its `{MARKER_26588}` marker"
    );
    assert!(
        !code(&cluster).contains("metadata_staging_pending_buffers"),
        "{PBR_CLUSTER} has the write-only metadata_staging_pending_buffers list again \
         (bevy#26588): GPU clustering leaks a buffer handle per view per frame"
    );

    // Mudcrab's own fog falloff, not a Bevy change (#891): the weather's exterior fog is drawn by
    // `FogFalloff::PowerRamp`. Without it the engine's fog would not build, or would draw nothing.
    for (file, needed) in [
        ("src/fog.rs", "PowerRamp {"),
        ("src/render/fog.rs", "GPU_FOG_MODE_POWER_RAMP"),
        ("src/render/mesh_view_types.wgsl", "FOG_MODE_POWER_RAMP"),
        ("src/render/fog.wgsl", "fn power_ramp_fog("),
        ("src/render/pbr_functions.wgsl", "power_ramp_fog("),
    ] {
        let source = required_source(pbr, file);
        assert!(
            source.contains(MARKER_POWER_RAMP) && code(&source).contains(needed),
            "{file} lost the power ramp fog (`{needed}` or its `{MARKER_POWER_RAMP}` marker)"
        );
    }
    assert!(
        code(&required_source(pbr, "src/render/fog.wgsl"))
            .contains("min(exp2(log2(s) * power), far_color.a)"),
        "the power ramp fog shader no longer computes min(s^power, max)"
    );
}

#[test]
fn the_workspace_still_patches_both_bevy_crates() {
    let manifest = required_source(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        "Cargo.toml",
    );
    for crate_name in ["bevy_pbr", "bevy_render"] {
        assert!(
            patches_crate(&manifest, crate_name),
            "[patch.crates-io] no longer has the line `{crate_name} = {{ path = \
             \"vendor/{crate_name}\" }}`, so the build lacks the carried Bevy fixes: restore the \
             patch or move these checks"
        );
    }
}

#[test]
fn a_patch_entry_must_be_a_live_line_in_the_patch_table() {
    let live = "[patch.crates-io]\nbevy_pbr = { path = \"vendor/bevy_pbr\" }\n";
    assert!(patches_crate(live, "bevy_pbr"));
    assert!(patches_crate(
        &format!("[workspace]\n\n{live}\n[lints]\n"),
        "bevy_pbr"
    ));
    assert!(!patches_crate(live, "bevy_render"), "another crate's line");
    for (why, manifest) in [
        (
            "commented out",
            "[patch.crates-io]\n# bevy_pbr = { path = \"vendor/bevy_pbr\" }\n",
        ),
        (
            "commented out and indented",
            "[patch.crates-io]\n  #bevy_pbr = { path = \"vendor/bevy_pbr\" }\n",
        ),
        (
            "in another table",
            "[workspace.dependencies]\nbevy_pbr = { path = \"vendor/bevy_pbr\" }\n[patch.crates-io]\n",
        ),
        (
            "after the patch table ended",
            "[patch.crates-io]\n[other]\nbevy_pbr = { path = \"vendor/bevy_pbr\" }\n",
        ),
        (
            "a different path",
            "[patch.crates-io]\nbevy_pbr = { path = \"elsewhere/bevy_pbr\" }\n",
        ),
    ] {
        assert!(!patches_crate(manifest, "bevy_pbr"), "{why} must not count");
    }
}

#[test]
fn the_code_helper_drops_comment_lines_and_collapses_whitespace() {
    assert_eq!(
        code("    // a.clear();\n    /// b.clear();\n    c.clear(\n    );\n"),
        "c.clear( );"
    );
}

#[test]
fn the_vendored_bevy_fixes_are_present() {
    check_patches(&vendor("bevy_pbr"), &vendor("bevy_render"));
}

/// A copy of the vendored files the checks read, with `edit` applied to `target`. The files are
/// written with LF endings.
fn vendored_copy(target: &str, edit: impl Fn(String) -> String) -> tempfile::TempDir {
    vendored_copy_with_endings(target, edit, "\n")
}

/// Like [`vendored_copy`], but written with `ending` as the line ending, as a checkout made with
/// `core.autocrlf` would. `edit` always sees LF text: the sources are read through
/// [`read_source`], so it does not depend on the checkout that runs the test.
fn vendored_copy_with_endings(
    target: &str,
    edit: impl Fn(String) -> String,
    ending: &str,
) -> tempfile::TempDir {
    let copy = tempfile::tempdir().unwrap();
    for (crate_name, relative) in [
        ("bevy_pbr", PBR_MORPH),
        ("bevy_pbr", PBR_MESH),
        ("bevy_pbr", PBR_PREPROCESS),
        ("bevy_pbr", PBR_CLUSTER),
        ("bevy_pbr", "src/fog.rs"),
        ("bevy_pbr", "src/render/fog.rs"),
        ("bevy_pbr", "src/render/mesh_view_types.wgsl"),
        ("bevy_pbr", "src/render/fog.wgsl"),
        ("bevy_pbr", "src/render/pbr_functions.wgsl"),
        ("bevy_render", RENDER_PREPROCESSING),
    ] {
        let destination = copy.path().join(crate_name).join(relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let text = read_source(&vendor(crate_name).join(relative)).unwrap();
        let text = if relative == target { edit(text) } else { text };
        std::fs::write(destination, text.replace('\n', ending)).unwrap();
    }
    copy
}

fn check_copy(copy: &tempfile::TempDir) {
    check_patches(
        &copy.path().join("bevy_pbr"),
        &copy.path().join("bevy_render"),
    );
}

#[test]
#[should_panic(expected = "lost the power ramp fog")]
fn dropping_the_power_ramp_shader_branch_fails() {
    check_copy(&vendored_copy("src/render/pbr_functions.wgsl", |text| {
        text.replace("power_ramp_fog(", "linear_fog(")
    }));
}

#[test]
#[should_panic(expected = "no longer computes")]
fn changing_the_power_ramp_formula_fails() {
    check_copy(&vendored_copy("src/render/fog.wgsl", |text| {
        text.replace("exp2(log2(s) * power)", "s")
    }));
}

#[test]
fn an_unchanged_copy_passes_the_checks() {
    check_copy(&vendored_copy("", |text| text));
}

#[test]
#[should_panic(expected = "lost `for view_phase_buffers in")]
fn dropping_the_per_view_clear_loop_fails() {
    check_copy(&vendored_copy(RENDER_PREPROCESSING, |text| {
        text.replace(
            "for view_phase_buffers in bin_unpacking_buffers.view_phase_buffers.values_mut() {",
            "for view_phase_buffers in std::iter::empty::<&mut ViewPhaseBinUnpackingBuffers>() {",
        )
    }));
}

#[test]
#[should_panic(expected = "lost `view_phase_buffers.non_indexed_unpacking_jobs.clear();`")]
fn dropping_one_job_list_clear_fails() {
    check_copy(&vendored_copy(RENDER_PREPROCESSING, |text| {
        text.replace("view_phase_buffers.non_indexed_unpacking_jobs.clear();", "")
    }));
}

#[test]
#[should_panic(expected = "lost `view_phase_buffers.indexed_unpacking_jobs.clear();`")]
fn turning_a_job_list_clear_into_a_comment_fails() {
    check_copy(&vendored_copy(RENDER_PREPROCESSING, |text| {
        text.replace(
            "        view_phase_buffers.indexed_unpacking_jobs.clear();",
            "        // view_phase_buffers.indexed_unpacking_jobs.clear();",
        )
    }));
}

#[test]
#[should_panic(expected = "no longer clears the bin-unpacking bind groups")]
fn dropping_the_bind_group_clear_fails() {
    check_copy(&vendored_copy(PBR_PREPROCESS, |text| {
        text.replace("bin_unpacking_bind_groups.clear();", "")
    }));
}

#[test]
#[should_panic(expected = "no longer clears the bin-unpacking bind groups")]
fn turning_the_bind_group_clear_into_a_comment_fails() {
    check_copy(&vendored_copy(PBR_PREPROCESS, |text| {
        text.replace(
            "    bin_unpacking_bind_groups.clear();",
            "    // bin_unpacking_bind_groups.clear();",
        )
    }));
}

#[test]
#[should_panic(expected = "must carry exactly one")]
fn a_preprocess_file_without_the_marker_fails() {
    check_copy(&vendored_copy(PBR_PREPROCESS, |text| {
        text.replace(MARKER_26095, "upstream")
    }));
}

#[test]
#[should_panic(expected = "write-only metadata_staging_pending_buffers")]
fn the_clustering_leak_coming_back_fails() {
    check_copy(&vendored_copy(PBR_CLUSTER, |text| {
        text.replace(
            "    max_index_list_capacity: usize,\n",
            "    max_index_list_capacity: usize,\n    metadata_staging_pending_buffers: Vec<Buffer>,\n",
        )
    }));
}

#[test]
#[should_panic(expected = "no longer returns None while the morph descriptor is pending")]
fn dropping_the_pending_descriptor_wait_fails() {
    check_copy(&vendored_copy(PBR_MESH, |text| {
        text.replace("morph_indices.morph_descriptor_pending(entity)", "false")
    }));
}

#[test]
#[should_panic(expected = "no longer returns None while the morph descriptor is pending")]
fn turning_the_pending_descriptor_wait_into_a_comment_fails() {
    check_copy(&vendored_copy(PBR_MESH, |text| {
        text.replace(
            "        if morph_indices.morph_descriptor_pending(entity) {\n            return None;\n        }",
            "        // if morph_indices.morph_descriptor_pending(entity) {\n        //     return None;\n        // }",
        )
    }));
}

/// Bevy 0.19.0's `prepare_morph_descriptors`: a bare `continue;` where the patch records the
/// entity. The field, the getter and `mesh_id` all still exist.
#[test]
#[should_panic(expected = "must record a pending entity in both places")]
fn restoring_the_bare_continue_fails() {
    check_copy(&vendored_copy(PBR_MORPH, |text| {
        text.replace("pending_descriptors.insert(morph_target_main_entity);", "")
    }));
}

#[test]
#[should_panic(expected = "must record a pending entity in both places")]
fn recording_only_the_missing_mesh_fails() {
    check_copy(&vendored_copy(PBR_MORPH, |text| {
        text.replace(
            "            if mesh.has_morph_targets() {\n                pending_descriptors.insert(morph_target_main_entity);\n            }\n",
            "",
        )
    }));
}

#[test]
#[should_panic(expected = "must clear pending_descriptors once per frame")]
fn never_clearing_the_pending_set_fails() {
    check_copy(&vendored_copy(PBR_MORPH, |text| {
        text.replace("    pending_descriptors.clear();\n", "")
    }));
}

#[test]
#[should_panic(expected = "must record a pending entity in both places")]
fn commenting_out_a_pending_record_fails() {
    check_copy(&vendored_copy(PBR_MORPH, |text| {
        text.replace(
            "            pending_descriptors.insert(morph_target_main_entity);\n            continue;",
            "            // pending_descriptors.insert(morph_target_main_entity);\n            continue;",
        )
    }));
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

/// Intact and mutated sources give the same verdicts with LF and with CRLF endings on disk (a
/// Windows checkout with `core.autocrlf`). The mutations are LF strings: they only change a CRLF
/// checkout because the sources are normalised before the guards and the fixtures read them.
#[test]
fn every_line_ending_gives_the_same_verdicts() {
    type Edit = fn(String) -> String;
    let drop_loop: Edit = |text| {
        text.replace(
            "for view_phase_buffers in bin_unpacking_buffers.view_phase_buffers.values_mut() {",
            "for view_phase_buffers in std::iter::empty::<&mut ViewPhaseBinUnpackingBuffers>() {",
        )
    };
    let restore_continue: Edit =
        |text| text.replace("pending_descriptors.insert(morph_target_main_entity);", "");
    let drop_bind_group_clear: Edit = |text| text.replace("bin_unpacking_bind_groups.clear();", "");
    for ending in ["\n", "\r\n"] {
        let intact = vendored_copy_with_endings("", |text| text, ending);
        let on_disk =
            std::fs::read_to_string(intact.path().join("bevy_render").join(RENDER_PREPROCESSING))
                .unwrap();
        assert_eq!(
            on_disk.contains("\r\n"),
            ending == "\r\n",
            "the copy really has {ending:?} endings"
        );
        check_copy(&intact);

        for (file, edit, expected) in [
            (
                RENDER_PREPROCESSING,
                drop_loop,
                "lost `for view_phase_buffers in",
            ),
            (
                PBR_MORPH,
                restore_continue,
                "must record a pending entity in both places",
            ),
            (
                PBR_PREPROCESS,
                drop_bind_group_clear,
                "no longer clears the bin-unpacking",
            ),
        ] {
            let mutated = vendored_copy_with_endings(file, edit, ending);
            let message = panic_message(|| check_copy(&mutated))
                .unwrap_or_else(|| panic!("{file} mutated: the check passed with {ending:?}"));
            assert!(
                message.contains(expected),
                "{file} with {ending:?}: {message}"
            );
        }
    }
}
