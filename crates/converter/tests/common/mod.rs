use std::path::PathBuf;

/// Keep deterministic pipeline fixtures independent of GPU availability.
pub fn cpu_lod_config(
    data_dir: impl Into<PathBuf>,
    output_dir: impl Into<PathBuf>,
) -> converter::PipelineConfig {
    let mut config = converter::PipelineConfig::new(data_dir, output_dir);
    config.lod_texture_encoder = converter::TextureEncoder::Cpu;
    config
}
