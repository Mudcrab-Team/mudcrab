//! Build provenance for exported diagnostics. Git is never queried by the running engine.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn git(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    println!("cargo:rerun-if-env-changed=MUDCRAB_BUILD_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=MUDCRAB_BUILD_GIT_DIRTY");
    // New source modules can be untracked while being tested locally. Watch source directories
    // as well as the tracked-file list so those edits refresh the binary's dirty provenance.
    for source in ["crates/engine/src", "crates/shared/src"] {
        println!("cargo:rerun-if-changed={}", root.join(source).display());
    }
    // Cargo's PROFILE is `release` for custom release-derived profiles. The output directory
    // retains the actual profile's name, including `quick` and cross-compiled target layouts.
    let profile = env::var_os("OUT_DIR")
        .and_then(|path| {
            PathBuf::from(path)
                .ancestors()
                .nth(3)
                .map(Path::to_path_buf)
        })
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .or_else(|| env::var("PROFILE").ok())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=MUDCRAB_BUILD_PROFILE={profile}");
    println!(
        "cargo:rustc-env=MUDCRAB_BUILD_TARGET={}",
        env::var("TARGET").unwrap_or_else(|_| "unknown".into())
    );
    let supplied_commit = env::var("MUDCRAB_BUILD_GIT_COMMIT").ok();
    let commit = supplied_commit
        .clone()
        .or_else(|| git(&root, &["rev-parse", "HEAD"]))
        .filter(|value| {
            matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .unwrap_or_else(|| "unknown".into());
    // A packaging-supplied commit does not imply the packaging checkout's dirty state.
    let dirty = env::var("MUDCRAB_BUILD_GIT_DIRTY")
        .ok()
        .filter(|value| value == "true" || value == "false")
        .or_else(|| {
            supplied_commit
                .is_none()
                .then(|| {
                    git(
                        &root,
                        &["status", "--porcelain", "--untracked-files=normal"],
                    )
                })
                .flatten()
                .map(|status| (!status.is_empty()).to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=MUDCRAB_BUILD_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=MUDCRAB_BUILD_GIT_DIRTY={dirty}");
    // Resolve Git paths rather than assuming `.git` is a directory: linked worktrees have
    // their own HEAD/index and share refs and packed-refs with the common repository.
    for name in ["HEAD", "index", "packed-refs"] {
        watch_git_path(&root, name);
    }
    if let Some(reference) = git(&root, &["symbolic-ref", "-q", "HEAD"]) {
        watch_git_path(&root, &reference);
    }
    // Changes to tracked source and metadata must refresh the dirty bit even without a
    // changed index. Restrict watches to tracked files; never traverse converted assets.
    if let Some(files) = git(&root, &["ls-files", "-z"]) {
        for file in files.split('\0').filter(|file| !file.is_empty()) {
            println!("cargo:rerun-if-changed={}", root.join(file).display());
        }
    } else {
        println!("cargo:rerun-if-changed=src");
        println!("cargo:rerun-if-changed=Cargo.toml");
    }
    println!("cargo:rerun-if-changed=build.rs");
}

fn watch_git_path(root: &Path, name: &str) {
    if let Some(path) = git(root, &["rev-parse", "--git-path", name]) {
        let path = PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        // Cargo reruns on every build for a missing watched path, so skip absent
        // files such as `packed-refs` or a ref stored only in `packed-refs`.
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
