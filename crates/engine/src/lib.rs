pub mod actor_animation;
pub mod actor_body_weight;
pub mod actor_drive;
pub mod actor_head;
pub mod actor_outfit;
pub mod actor_skin_tone;
pub mod actors;
#[cfg(test)]
mod ambient_pixel_tests;
pub mod app;
mod background_window;
#[cfg(test)]
mod bevy_source;
pub mod bug_report;
mod capture_frame;
pub mod cell_commands;
pub mod color_pipeline;
pub mod config;
pub mod console;
mod console_selection;
pub mod door_crossing;
pub mod doors;
pub mod emittance;
pub mod extensions;
pub mod fingerprint;
mod focus_pause;
pub mod frame_budget;
pub mod frame_sequence;
pub mod grass;
pub mod grass_render;
pub mod grass_tuning;
pub mod hdr_probe;
pub mod indirect_metrics;
pub mod light_dynamics;
pub mod lights;
mod live_console;
mod matched_route;
pub mod mesh_preparation_retry;
pub mod metrics;
#[cfg(test)]
mod movement_speed_guard_tests;
pub mod movement_speeds;
pub mod nif_depth;
pub mod nif_material;
pub mod object_materials;
pub mod output_scale;
pub mod pacing;
pub mod papyrus_host;
pub mod papyrus_plugin;
pub mod papyrus_runtime;
pub mod physics;
#[cfg(test)]
mod post_process_order;
pub mod prepass_vertex_alpha;
pub mod profiling;
pub mod raw_texel;
pub mod render;
mod render_teardown;
pub mod render_timing;
mod renderer_init;
pub mod retail_fog;
pub mod retail_hdr;
pub mod retail_hdr_chain;
#[cfg(test)]
mod retail_hdr_gpu_tests;
mod retail_hdr_render;
mod retail_image_space;
pub mod retail_lighting;
#[cfg(test)]
mod retail_lighting_gpu_tests;
pub mod room_culling;
pub mod route;
mod scene_evidence;
pub mod schedule_timing;
pub mod settings;
pub mod settings_ui;
#[cfg(test)]
mod shader_validation;
pub mod shots;
pub mod skin_material;
pub mod sky;
pub mod skyrim_ini;
pub mod streaming;
mod streaming_gpu_metrics;
mod streaming_preparation;
mod streaming_trace;
pub mod sun;
pub mod swap_bridge;
mod terrain_lod_noise;
pub mod terrain_merge;
#[cfg(test)]
mod terrain_normal;
pub mod terrain_upload;
#[cfg(test)]
mod test_support;
pub mod texture_fallback;
pub mod tone_mapping;
mod underwater;
mod underwater_pass;
#[cfg(test)]
mod vendored_bevy_patches;
#[cfg(test)]
mod vendored_morph_shader;
pub mod visibility_optimization;
pub mod visual_profile;
mod water_depth;
pub mod weather_sky;
pub mod world;

pub use app::run;

mod mesh_residency_audit;

#[doc(hidden)]
pub mod collision_measure;

#[cfg(feature = "collision-measure")]
mod scene_placement_probe;
