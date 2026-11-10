use engine::config::{ConfigAction, EngineConfig, HELP_TEXT};

/// Exit code for a command line the engine cannot use.
const EXIT_USAGE: i32 = 2;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    match EngineConfig::from_env() {
        Ok(ConfigAction::Run(config)) => {
            if config.grass_tuning_info {
                let state =
                    engine::grass_tuning::GrassTuningState::new(config.grass_tuning.clone());
                let mut receipt = state.receipt();
                receipt["grass_snow_gate"] =
                    serde_json::json!(engine::grass::generation::MAJORITY_GRASS_SUPPORT);
                receipt["range"] = serde_json::json!({
                    "stream_radius": config.stream_radius,
                    "grass_radius": config.effective_grass_radius(),
                    "fade": config.effective_grass_fade(),
                    "cache_mib": config.grass_cache_mib,
                    "uploads_per_frame": config.grass_uploads_per_frame,
                });
                println!("{}", serde_json::to_string(&receipt)?);
                return Ok(());
            }
            engine::run(*config)
        }
        Ok(ConfigAction::Help) => {
            print!("{HELP_TEXT}");
            Ok(())
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(EXIT_USAGE);
        }
    }
}
