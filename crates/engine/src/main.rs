use engine::config::{ConfigAction, EngineConfig, HELP_TEXT};

/// Exit code for a command line the engine cannot use.
const EXIT_USAGE: i32 = 2;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    match EngineConfig::from_env() {
        Ok(ConfigAction::Run(config)) => {
            if config.grass_tuning_info {
                let state = engine::grass_tuning::GrassTuningState::new(config.grass_tuning);
                println!("{}", serde_json::to_string(&state.receipt())?);
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
