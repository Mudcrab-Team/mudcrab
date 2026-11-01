#[path = "src/records/deciders/mod.rs"]
#[allow(dead_code)]
mod deciders;
#[path = "src/records/schema_format.rs"]
#[allow(dead_code)]
mod schema_format;

use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Validate the authored schema before building the native texture bridge.
fn main() {
    let schema = std::fs::read("src/records/schema.json").expect("read record schema");
    let mut families = Vec::new();
    for family in [
        "items",
        "actors",
        "magic",
        "dialogue",
        "conditions",
        "ai",
        "perks",
        "world_extras",
        "visual_extras",
        "audio_extras",
    ] {
        let path = format!("src/records/families/{family}.json");
        families.push((family, std::fs::read(&path).expect("read family schema")));
        println!("cargo:rerun-if-changed={path}");
    }
    let parts: Vec<_> = families
        .iter()
        .map(|(name, bytes)| (*name, bytes.as_slice()))
        .collect();
    let assembled =
        schema_format::Schema::assemble(&schema, &parts).expect("validate combined record schema");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("build output"));
    std::fs::write(output.join("records-schema.json"), assembled)
        .expect("write compiled record schema");
    println!("cargo:rerun-if-changed=src/records/deciders");
    println!("cargo:rerun-if-changed=src/records/schema.json");
    println!("cargo:rerun-if-changed=src/records/schema_format.rs");
    cc::Build::new()
        .cpp(true)
        .flag_if_supported("/std:c++14")
        .flag_if_supported("-std=c++14")
        .file("src/basis_ktx2_bridge.cpp")
        .compile("opensky_basis_ktx2_bridge");
    println!("cargo:rerun-if-changed=src/basis_ktx2_bridge.cpp");
    emit_cluster_source_hashes();
}

/// Hashes of the sources the cluster builder is made of, so the builder identity changes when a
/// vendored file or the Rust that reads and encodes geometry is patched, not only when a version
/// string is (ADR-0013 section 1).
fn emit_cluster_source_hashes() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendor = manifest.join("../../vendor/meshoptimizer");
    let wrapper = manifest.join("../meshopt-cluster/cpp");

    let clusterlod = vendor.join("demo/clusterlod.h");
    println!("cargo:rerun-if-changed={}", clusterlod.display());
    println!(
        "cargo:rustc-env=CLUSTERLOD_SOURCE_SHA256={}",
        hash_file(&clusterlod)
    );

    let mut sources: Vec<PathBuf> = fs::read_dir(vendor.join("src"))
        .expect("read vendor/meshoptimizer/src")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "cpp" || extension == "h")
        })
        .collect();
    sources.sort();
    println!("cargo:rerun-if-changed={}", vendor.join("src").display());
    let mut combined = Sha256::new();
    for path in &sources {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path.file_name().expect("file name").to_string_lossy();
        combined.update(format!("{name}:{}\n", hash_file(path)));
    }
    println!(
        "cargo:rustc-env=MESHOPTIMIZER_SOURCES_SHA256={:x}",
        combined.finalize()
    );

    let mut wrapper_hash = Sha256::new();
    for name in ["mlc_wrapper.cpp", "mlc_wrapper.h"] {
        let path = wrapper.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        wrapper_hash.update(format!("{name}:{}\n", hash_file(&path)));
    }
    println!(
        "cargo:rustc-env=MLC_WRAPPER_SHA256={:x}",
        wrapper_hash.finalize()
    );

    let mut rust_hash = Sha256::new();
    // Every Rust source that decides what a payload, a reject, a mapping row or the provenance
    // means, listed in `cluster-identity-sources.txt` (paths relative to this crate). A unit test
    // checks that the list covers `src/cluster/`, so a new module cannot be forgotten.
    let list = manifest.join("cluster-identity-sources.txt");
    println!("cargo:rerun-if-changed={}", list.display());
    let text = fs::read_to_string(&list)
        .unwrap_or_else(|error| panic!("read {}: {error}", list.display()));
    let mut rust_sources: Vec<(String, PathBuf)> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| (line.to_owned(), manifest.join(line)))
        .collect();
    rust_sources.sort();
    for (name, path) in &rust_sources {
        println!("cargo:rerun-if-changed={}", path.display());
        rust_hash.update(format!("{name}:{}\n", hash_file(path)));
    }
    println!(
        "cargo:rustc-env=CLUSTER_RUST_SOURCES_SHA256={:x}",
        rust_hash.finalize()
    );
}

/// SHA-256 of the file with CRLF folded to LF, so a Windows checkout with `autocrlf` and an LF
/// checkout of the same commit get the same builder identity.
fn hash_file(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let mut normalised = Vec::with_capacity(bytes.len());
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            continue;
        }
        normalised.push(byte);
    }
    format!("{:x}", Sha256::digest(normalised))
}
