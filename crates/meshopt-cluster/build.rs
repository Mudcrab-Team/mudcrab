// Builds meshoptimizer v1.3 (MIT, vendor/meshoptimizer) and the C wrapper in cpp/ with cc.
use std::env;
use std::path::PathBuf;

/// meshoptimizer sources compiled into the crate (everything else under `src/` is unused).
const VENDOR_SOURCES: &[&str] = &[
    "clusterizer.cpp",
    "indexgenerator.cpp",
    "meshletutils.cpp",
    "partition.cpp",
    "simplifier.cpp",
    "spatialorder.cpp",
];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendor = manifest.join("../../vendor/meshoptimizer");
    let src = vendor.join("src");
    let cpp = manifest.join("cpp");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(&src)
        .include(vendor.join("demo"))
        .include(&cpp);

    // meshoptimizer's asserts follow NDEBUG; keep them on in debug builds to catch misuse early
    if env::var("PROFILE").as_deref() == Ok("release") {
        build.define("NDEBUG", None);
    }

    // Only the meshoptimizer sources that clusterlod.h links against; see MUDCRAB-README.md.
    let mut sources: Vec<PathBuf> = VENDOR_SOURCES.iter().map(|name| src.join(name)).collect();
    sources.push(cpp.join("mlc_wrapper.cpp"));

    for path in &sources {
        println!("cargo:rerun-if-changed={}", path.display());
        build.file(path);
    }
    for header in [
        src.join("meshoptimizer.h"),
        vendor.join("demo/clusterlod.h"),
        cpp.join("mlc_wrapper.h"),
    ] {
        println!("cargo:rerun-if-changed={}", header.display());
    }

    build.compile("meshopt_cluster");
}
