use engine::config::{ConfigAction, EngineConfig, HELP_TEXT};

/// Exit code for a command line the engine cannot use.
const EXIT_USAGE: i32 = 2;

/// Dispatch help, version, grass-tuning information and runtime commands.
fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    match EngineConfig::from_env() {
        Ok(ConfigAction::Run(config)) => {
            if config.grass_tuning_info {
                if let Err(error) = config.validate_grass() {
                    eprintln!("error: {error}");
                    std::process::exit(EXIT_USAGE);
                }
                let state =
                    engine::grass_tuning::GrassTuningState::for_info(config.grass_tuning.clone());
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
            match engine::run(*config) {
                Err(error) if error.downcast_ref::<engine::app::DeviceLost>().is_some() => {
                    eprintln!("error: {error}");
                    std::process::exit(i32::from(engine::app::EXIT_DEVICE_LOST));
                }
                result => result,
            }
        }
        Ok(ConfigAction::Help) => {
            print!("{HELP_TEXT}");
            Ok(())
        }
        Ok(ConfigAction::Version) => {
            println!("{}", shared::VERSION_LINE);
            Ok(())
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(EXIT_USAGE);
        }
    }
}
