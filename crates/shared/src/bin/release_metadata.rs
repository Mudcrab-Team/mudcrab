use serde::Serialize;
use std::process::Command;

#[derive(Serialize)]
struct ReleaseMetadata {
    version: String,
    tag: String,
    git_commit: String,
    converter_schema: u32,
    min_runtime_converter_schema: u32,
    world_database_schema: u32,
    min_runtime_world_database_schema: u32,
    cell_cache_version: u32,
    streaming_cost_schema: u32,
}

fn main() {
    let version = env!("CARGO_PKG_VERSION").to_string();
    let git_commit = std::env::var("GITHUB_SHA").unwrap_or_else(|_| {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .ok();
        out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    });

    let tag = std::env::var("GITHUB_REF_NAME").unwrap_or_else(|_| format!("v{version}"));

    let meta = ReleaseMetadata {
        version,
        tag,
        git_commit,
        converter_schema: shared::LOD_CONVERTER_SCHEMA_VERSION,
        min_runtime_converter_schema: shared::MIN_RUNTIME_CONVERTER_SCHEMA_VERSION,
        world_database_schema: shared::WORLD_DATABASE_SCHEMA_VERSION,
        min_runtime_world_database_schema: shared::MIN_RUNTIME_WORLD_DATABASE_SCHEMA_VERSION,
        cell_cache_version: shared::CELL_CACHE_VERSION,
        streaming_cost_schema: shared::streaming_costs::STREAMING_COST_SCHEMA_VERSION,
    };

    println!("{}", serde_json::to_string_pretty(&meta).unwrap());
}
