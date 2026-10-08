#[path = "src/records/schema_format.rs"]
#[allow(dead_code)]
mod schema_format;

/// Validate the authored schema before building the native texture bridge.
fn main() {
    let schema = std::fs::read("src/records/schema.json").expect("read record schema");
    schema_format::Schema::parse(&schema).expect("validate record schema");
    println!("cargo:rerun-if-changed=src/records/schema.json");
    println!("cargo:rerun-if-changed=src/records/schema_format.rs");
    cc::Build::new()
        .cpp(true)
        .flag_if_supported("/std:c++14")
        .flag_if_supported("-std=c++14")
        .file("src/basis_ktx2_bridge.cpp")
        .compile("opensky_basis_ktx2_bridge");
    println!("cargo:rerun-if-changed=src/basis_ktx2_bridge.cpp");
}
