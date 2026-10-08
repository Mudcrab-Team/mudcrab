#[path = "src/records/deciders/mod.rs"]
#[allow(dead_code)]
mod deciders;
#[path = "src/records/schema_format.rs"]
#[allow(dead_code)]
mod schema_format;

/// Validate the authored schema before building the native texture bridge.
fn main() {
    let schema = std::fs::read("src/records/schema.json").expect("read record schema");
    let mut families = Vec::new();
    for family in [
        "items",
        "actors",
        "magic",
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
}
