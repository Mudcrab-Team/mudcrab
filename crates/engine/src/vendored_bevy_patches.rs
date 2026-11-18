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
//! The morph shader bounds have their own test in `vendored_morph_shader`.

use crate::bevy_source::required_source;
use std::path::{Path, PathBuf};

const PBR_MORPH: &str = "src/render/morph.rs";
const PBR_MESH: &str = "src/render/mesh.rs";
const PBR_PREPROCESS: &str = "src/render/gpu_preprocess.rs";
const PBR_CLUSTER: &str = "src/cluster/gpu.rs";
const RENDER_PREPROCESSING: &str = "src/batching/gpu_preprocessing.rs";

const MARKER_26504: &str = "mudcrab: bevyengine/bevy#26504";
const MARKER_26095: &str = "mudcrab: bevyengine/bevy#26095";
const MARKER_26588: &str = "mudcrab: bevyengine/bevy#26588";

fn vendor(crate_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor")
        .join(crate_name)
}

/// The text of the top-level Rust function `name`, from `fn name(` to the closing `}` at column 0.
fn function<'a>(source: &'a str, file: &str, name: &str) -> &'a str {
    let start = source
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("{file} has no fn {name}: re-port the vendored patch"));
    let end = source[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("fn {name} in {file} has no end"));
    &source[start..start + end]
}

/// Every check on one pair of `bevy_pbr` and `bevy_render` directories. Nothing here skips: a
/// missing file, marker or hunk panics.
fn check_patches(pbr: &Path, render: &Path) {
    // bevyengine/bevy#26504 (the shader half is in `vendored_morph_shader`).
    let mesh = required_source(pbr, PBR_MESH);
    assert!(
        mesh.contains(MARKER_26504)
            && mesh.contains("morph_indices.morph_descriptor_pending(entity)"),
        "{PBR_MESH} lost the bevy#26504 wait for a pending morph descriptor"
    );
    let morph = required_source(pbr, PBR_MORPH);
    assert!(
        morph.contains("pending_descriptors")
            && morph.contains("fn morph_descriptor_pending(")
            && morph.contains("mesh_id: AssetId<Mesh>"),
        "{PBR_MORPH} lost the bevy#26504 pending morph descriptors"
    );

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
            && bind_groups.contains("bin_unpacking_bind_groups.clear();"),
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
    for needed in [
        MARKER_26095,
        "bin_unpacking_buffers.bin_unpacking_metadata.clear();",
        "for view_phase_buffers in bin_unpacking_buffers.view_phase_buffers.values_mut() {",
        "view_phase_buffers.indexed_unpacking_jobs.clear();",
        "view_phase_buffers.non_indexed_unpacking_jobs.clear();",
    ] {
        assert!(
            clear.contains(needed),
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
        !cluster.contains("metadata_staging_pending_buffers"),
        "{PBR_CLUSTER} has the write-only metadata_staging_pending_buffers list again \
         (bevy#26588): GPU clustering leaks a buffer handle per view per frame"
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
            manifest.contains(&format!(
                "{crate_name} = {{ path = \"vendor/{crate_name}\" }}"
            )),
            "the workspace no longer patches {crate_name} with vendor/{crate_name}, so the build \
             lacks the carried Bevy fixes: restore the patch or move these checks"
        );
    }
}

#[test]
fn the_vendored_bevy_fixes_are_present() {
    check_patches(&vendor("bevy_pbr"), &vendor("bevy_render"));
}

/// A copy of the vendored files the checks read, with `edit` applied to `target`.
fn vendored_copy(target: &str, edit: impl Fn(String) -> String) -> tempfile::TempDir {
    let copy = tempfile::tempdir().unwrap();
    for (crate_name, relative) in [
        ("bevy_pbr", PBR_MORPH),
        ("bevy_pbr", PBR_MESH),
        ("bevy_pbr", PBR_PREPROCESS),
        ("bevy_pbr", PBR_CLUSTER),
        ("bevy_render", RENDER_PREPROCESSING),
    ] {
        let destination = copy.path().join(crate_name).join(relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let text = std::fs::read_to_string(vendor(crate_name).join(relative)).unwrap();
        let text = if relative == target { edit(text) } else { text };
        std::fs::write(destination, text).unwrap();
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
#[should_panic(expected = "no longer clears the bin-unpacking bind groups")]
fn dropping_the_bind_group_clear_fails() {
    check_copy(&vendored_copy(PBR_PREPROCESS, |text| {
        text.replace("bin_unpacking_bind_groups.clear();", "")
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
#[should_panic(expected = "lost the bevy#26504 wait")]
fn dropping_the_pending_descriptor_wait_fails() {
    check_copy(&vendored_copy(PBR_MESH, |text| {
        text.replace("morph_indices.morph_descriptor_pending(entity)", "false")
    }));
}
