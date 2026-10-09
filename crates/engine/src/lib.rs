pub mod app;
pub mod color_pipeline;
pub mod config;
pub mod indirect_metrics;
pub mod lights;
pub mod mesh_preparation_retry;
pub mod metrics;
pub mod nif_depth;
pub mod nif_material;
pub mod papyrus_runtime;
pub mod physics;
pub mod prepass_vertex_alpha;
pub mod profiling;
pub mod render;
pub mod render_timing;
mod renderer_init;
mod scene_evidence;
pub mod shots;
pub mod sky;
pub mod skyrim_ini;
pub mod streaming;
pub mod terrain_upload;
pub mod visibility_optimization;
pub mod world;

pub use app::run;

mod mesh_residency_audit;
