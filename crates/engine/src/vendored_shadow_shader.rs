//! Pins the two shadow-shader hunks that Mudcrab carries in `vendor/bevy_pbr`.
//!
//! - **Retail sun shadow distance fade** (`RF-LIGHT-GPU-020`), in `fetch_directional_shadow`
//!   (`src/render/shadows.wgsl`): the shadow strength is multiplied by `1 - (d / S)^16`.
//! - **Receiver-only fog shadow**, in `apply_fog` (`src/render/pbr_functions.wgsl`): Bevy's fog
//!   samples the sun shadow map for directional in-scattering with no receiver check. A mesh that
//!   does not receive shadows (LOD land and objects, `RF-SHADER-013`) now gets full visibility
//!   for that term only; the fog colour and density are unchanged.
//!
//! `vendor/BEVY_PATCHES.md` lists both. These tests read the vendored sources from the repository
//! and never skip, so a re-vendor, a Bevy upgrade or a hand edit that drops a hunk fails in CI
//! instead of bringing the abrupt shadow end or the fogged LOD shadow back. Code checks run on
//! source with comments removed and whitespace collapsed.

use crate::{bevy_source::required_source, sun_shadow_fade::SUN_SHADOW_FADE_EXPONENT};
use std::path::{Path, PathBuf};

/// The `bevy_pbr` version the hunks were written against.
const VENDORED_BEVY_PBR: &str = "0.19.0";

const SHADOWS_WGSL: &str = "src/render/shadows.wgsl";
const PBR_FUNCTIONS_WGSL: &str = "src/render/pbr_functions.wgsl";

const FADE_MARKER: &str = "mudcrab: retail sun shadow distance fade";
const FOG_PARAMETER_MARKER: &str = "mudcrab: whether the mesh receives shadows";
const FOG_SAMPLE_MARKER: &str = "mudcrab: only a mesh that receives shadows samples";
const FOG_CALL_MARKER: &str = "mudcrab: receives_shadow";

fn vendored_bevy_pbr() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/bevy_pbr")
}

/// `source` without `//` comments (whole-line and trailing), whitespace collapsed to one space.
fn code(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The raw text of the top-level WGSL function `name`, up to the `}` at column 0.
fn function<'a>(source: &'a str, file: &str, name: &str) -> &'a str {
    let start = source
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("{file} has no fn {name}: re-port the vendored hunk"));
    let end = source[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("fn {name} in {file} has no end"));
    &source[start..start + end]
}

/// Every check on one `bevy_pbr` crate directory. Nothing here skips: a missing file, marker or
/// hunk panics.
fn check_shadow_hunks(dir: &Path) {
    let manifest = required_source(dir, "Cargo.toml");
    assert!(
        manifest.contains("name = \"bevy_pbr\"")
            && manifest.contains(&format!("version = \"{VENDORED_BEVY_PBR}\"")),
        "{} is not bevy_pbr {VENDORED_BEVY_PBR}: re-port the shadow hunks \
         (vendor/BEVY_PATCHES.md), then update VENDORED_BEVY_PBR",
        dir.display()
    );

    // The fade.
    let shadows = required_source(dir, SHADOWS_WGSL);
    assert_eq!(
        shadows.matches(FADE_MARKER).count(),
        1,
        "{SHADOWS_WGSL} must carry exactly one `{FADE_MARKER}` marker"
    );
    let fetch = function(&shadows, SHADOWS_WGSL, "fetch_directional_shadow");
    assert!(
        fetch.contains(FADE_MARKER),
        "the retail fade is not inside fetch_directional_shadow:\n{fetch}"
    );
    let fetch_code = code(fetch);
    for needed in [
        "let mudcrab_fade_far = (*light).cascades[(*light).num_cascades - 1u].far_bound;",
        "var mudcrab_fade_x = saturate(length(frag_position.xyz - view_bindings::view.world_position) / mudcrab_fade_far);",
        "return mix(1.0, shadow, 1.0 - mudcrab_fade_x);",
        // The early "lit beyond the last cascade" return stays: the fade is 0 there.
        "if (cascade_index >= (*light).num_cascades) { return 1.0; }",
    ] {
        assert!(
            fetch_code.contains(needed),
            "fetch_directional_shadow lost `{needed}` (RF-LIGHT-GPU-020)"
        );
    }
    // (d/S)^16 is four squarings of the ratio.
    assert!(SUN_SHADOW_FADE_EXPONENT.is_power_of_two());
    assert_eq!(
        fetch_code
            .matches("mudcrab_fade_x = mudcrab_fade_x * mudcrab_fade_x;")
            .count() as u32,
        SUN_SHADOW_FADE_EXPONENT.trailing_zeros(),
        "the vendored fade must square the ratio log2({SUN_SHADOW_FADE_EXPONENT}) times"
    );

    // The receiver-only fog shadow.
    let functions = required_source(dir, PBR_FUNCTIONS_WGSL);
    let fog = function(&functions, PBR_FUNCTIONS_WGSL, "apply_fog");
    for marker in [FOG_PARAMETER_MARKER, FOG_SAMPLE_MARKER] {
        assert!(
            fog.contains(marker),
            "apply_fog lost its `{marker}` marker (the fogged LOD shadow comes back)"
        );
    }
    let fog_code = code(fog);
    for needed in [
        "receives_shadow: bool,",
        "if (receives_shadow && (light.flags & mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u) { shadow = shadows::fetch_directional_shadow(",
    ] {
        assert!(
            fog_code.contains(needed),
            "apply_fog lost `{needed}`: a mesh that does not receive shadows is darkened by the \
             sun shadow map through the fog again"
        );
    }
    let post = function(
        &functions,
        PBR_FUNCTIONS_WGSL,
        "main_pass_post_lighting_processing",
    );
    assert!(
        post.contains(FOG_CALL_MARKER),
        "main_pass_post_lighting_processing lost its `{FOG_CALL_MARKER}` marker"
    );
    assert!(
        code(post).contains("(pbr_input.flags & MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u"),
        "apply_fog is not told whether the mesh receives shadows"
    );
}

#[test]
fn the_vendored_shadow_shader_keeps_its_hunks() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(
        required_source(&workspace, "Cargo.toml")
            .contains("bevy_pbr = { path = \"vendor/bevy_pbr\" }"),
        "the workspace no longer patches bevy_pbr with vendor/bevy_pbr, so the build has neither \
         the shadow fade nor the receiver-only fog shadow: restore the patch or move the hunks"
    );
    assert_eq!(
        crate::bevy_source::locked_bevy_pbr_version(),
        VENDORED_BEVY_PBR,
        "Cargo.lock no longer pins the vendored bevy_pbr version"
    );
    check_shadow_hunks(&vendored_bevy_pbr());
}

/// A copy of the vendored crate's checked files in a temporary directory, with `edit` applied to
/// the named file.
fn vendored_copy(edited: &str, edit: impl Fn(String) -> String) -> tempfile::TempDir {
    let source = vendored_bevy_pbr();
    let copy = tempfile::tempdir().unwrap();
    for relative in ["Cargo.toml", SHADOWS_WGSL, PBR_FUNCTIONS_WGSL] {
        let text = std::fs::read_to_string(source.join(relative)).unwrap();
        let text = if relative == edited { edit(text) } else { text };
        let path = copy.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    copy
}

fn fails_with(edited: &str, edit: impl Fn(String) -> String) -> bool {
    let copy = vendored_copy(edited, edit);
    std::panic::catch_unwind(|| check_shadow_hunks(copy.path())).is_err()
}

/// The guard is not a no-op: dropping any one hunk makes it fail.
#[test]
fn dropping_a_hunk_fails_the_guard() {
    // An untouched copy passes.
    assert!(!fails_with(SHADOWS_WGSL, |text| text));
    // The fade: marker, the return, and one squaring.
    assert!(fails_with(SHADOWS_WGSL, |text| text.replace(FADE_MARKER, "x")));
    assert!(fails_with(SHADOWS_WGSL, |text| text.replace(
        "return mix(1.0, shadow, 1.0 - mudcrab_fade_x);",
        "return shadow;"
    )));
    assert!(fails_with(SHADOWS_WGSL, |text| text.replacen(
        "    mudcrab_fade_x = mudcrab_fade_x * mudcrab_fade_x;\n",
        "",
        1
    )));
    // The fog: the parameter, the receiver test, and the call argument.
    assert!(fails_with(PBR_FUNCTIONS_WGSL, |text| text
        .replace("    receives_shadow: bool,\n", "")));
    assert!(fails_with(PBR_FUNCTIONS_WGSL, |text| text.replace(
        "if (receives_shadow && (light.flags",
        "if ((light.flags"
    )));
    assert!(fails_with(PBR_FUNCTIONS_WGSL, |text| text.replace(
        "(pbr_input.flags & MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u, // mudcrab: receives_shadow",
        "true, // mudcrab: receives_shadow"
    )));
}
