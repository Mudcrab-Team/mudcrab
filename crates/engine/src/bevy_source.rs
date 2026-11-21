//! Test helpers that read the Bevy sources this build is locked to.
//!
//! A runtime shader patch (for example [`crate::prepass_vertex_alpha`]) matches text in a Bevy
//! shader, and `crate::vendored_morph_shader` pins a vendored one. Their tests read the real
//! shader from `vendor/` or the cargo registry, so a Bevy upgrade that changes the text fails in
//! CI instead of turning the patch into a silent no-op at runtime.

use std::path::{Path, PathBuf};

/// `text` with every CRLF line ending turned into LF.
///
/// A Windows checkout with `core.autocrlf=true` gives source files CRLF endings unless
/// `.gitattributes` says otherwise. The guards match exact text, and their test fixtures edit it
/// with LF strings, so every source read here is normalised once, before either use.
pub(crate) fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// A source file as LF text, whatever line endings the checkout gave it.
pub(crate) fn read_source(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path).map(|text| normalize_line_endings(&text))
}

/// The `bevy_pbr` version `Cargo.lock` pins.
pub(crate) fn locked_bevy_pbr_version() -> String {
    let lock =
        read_source(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"))
            .expect("the workspace has a Cargo.lock");
    let entry = lock
        .split("[[package]]")
        .find(|entry| {
            entry
                .lines()
                .any(|line| line.trim() == "name = \"bevy_pbr\"")
        })
        .expect("Cargo.lock pins bevy_pbr");
    entry
        .lines()
        .find_map(|line| line.trim().strip_prefix("version = \""))
        .and_then(|version| version.strip_suffix('"'))
        .expect("the bevy_pbr entry has a version")
        .to_owned()
}

/// The directories that can hold the `bevy_pbr` crate this build compiles: the workspace's
/// `vendor/bevy_pbr` (a vendored build) and every index of the local cargo registry.
fn bevy_pbr_candidates(version: &str) -> Vec<PathBuf> {
    let mut candidates = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/bevy_pbr")];
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|home| Path::new(&home).join(".cargo"))
        });
    if let Some(cargo_home) = cargo_home
        && let Ok(indexes) = std::fs::read_dir(cargo_home.join("registry").join("src"))
    {
        candidates.extend(
            indexes
                .filter_map(Result::ok)
                .map(|index| index.path().join(format!("bevy_pbr-{version}"))),
        );
    }
    candidates
}

/// The first candidate that is the `bevy_pbr` crate of `version`: its `Cargo.toml` names the
/// package and the version.
fn find_bevy_pbr_crate(version: &str, candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|dir| {
            std::fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|manifest| {
                manifest.contains("name = \"bevy_pbr\"")
                    && manifest.contains(&format!("version = \"{version}\""))
            })
        })
        .cloned()
}

/// The source directory of the `bevy_pbr` crate this build compiles. It never skips: cargo
/// unpacks every crate it compiles into the registry (or the crate is vendored), so a missing
/// directory means the shader-patch checks cannot run, and that must fail.
pub(crate) fn bevy_pbr_crate_dir(version: &str) -> PathBuf {
    let candidates = bevy_pbr_candidates(version);
    find_bevy_pbr_crate(version, &candidates).unwrap_or_else(|| {
        panic!(
            "no bevy_pbr {version} crate in vendor/bevy_pbr or the cargo registry (searched \
             {candidates:?}); the runtime shader patches cannot be checked against Bevy's sources"
        )
    })
}

/// A source file that a runtime shader patch depends on. It never skips: a missing or unreadable
/// file (for example a shader renamed to `.wesl`) means the patch matches nothing at runtime.
pub(crate) fn required_source(dir: &Path, relative: &str) -> String {
    let path = dir.join(relative);
    read_source(&path).unwrap_or_else(|error| {
        panic!(
            "{} is missing or unreadable ({error}): Bevy moved or renamed it (for example to \
             .wesl), so the shader patch that depends on it would silently do nothing",
            path.display()
        )
    })
}

/// A file of a Bevy source directory, or `None` (with a note) when it is unreadable.
pub(crate) fn registry_source(dir: &Path, relative: &str) -> Option<String> {
    let path = dir.join(relative);
    match read_source(&path) {
        Ok(source) => Some(source),
        Err(error) => {
            eprintln!("{} is unreadable ({error}); skipped", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crate_dir(root: &Path, version: &str) -> PathBuf {
        let dir = root.join(format!("bevy_pbr-{version}"));
        std::fs::create_dir_all(dir.join("src/render")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"bevy_pbr\"\nversion = \"{version}\"\n"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn the_crate_of_the_locked_version_is_found() {
        let root = tempfile::tempdir().unwrap();
        let other = crate_dir(root.path(), "0.20.1");
        let wanted = crate_dir(root.path(), "0.19.0");
        let missing = root.path().join("bevy_pbr-0.18.0");
        let candidates = [missing, other, wanted.clone()];
        assert_eq!(find_bevy_pbr_crate("0.19.0", &candidates), Some(wanted));
        assert_eq!(find_bevy_pbr_crate("0.17.0", &candidates), None);
    }

    #[test]
    fn crlf_and_lf_sources_read_the_same() {
        let root = tempfile::tempdir().unwrap();
        let lf = "fn a() {\n    b();\n}\n";
        for (name, ending) in [("lf.wgsl", "\n"), ("crlf.wgsl", "\r\n")] {
            std::fs::write(root.path().join(name), lf.replace('\n', ending)).unwrap();
            assert_eq!(required_source(root.path(), name), lf, "{name}");
            assert_eq!(
                registry_source(root.path(), name).as_deref(),
                Some(lf),
                "{name}"
            );
        }
        // A lone CR is not a line ending here and is left alone.
        assert_eq!(normalize_line_endings("a\rb\r\nc\n"), "a\rb\nc\n");
    }

    #[test]
    #[should_panic(expected = "morph.wgsl is missing or unreadable")]
    fn a_missing_shader_fails_instead_of_skipping() {
        let root = tempfile::tempdir().unwrap();
        let dir = crate_dir(root.path(), "0.19.0");
        required_source(&dir, "src/render/morph.wgsl");
    }

    #[test]
    #[should_panic(expected = "morph.wgsl is missing or unreadable")]
    fn a_shader_moved_to_wesl_fails_instead_of_skipping() {
        let root = tempfile::tempdir().unwrap();
        let dir = crate_dir(root.path(), "0.19.0");
        std::fs::write(dir.join("src/render/morph.wesl"), "fn layer_count() {}").unwrap();
        required_source(&dir, "src/render/morph.wgsl");
    }
}
