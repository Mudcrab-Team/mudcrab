//! Produce a diagnostic in-house database/cache bundle without asset conversion.
use color_eyre::{Result, eyre::ensure};
use std::{path::PathBuf, time::Instant};

/// Export selected plugins to a fresh evidence directory without publishing a runtime pack.
fn main() -> Result<()> {
    color_eyre::install()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "usage: inhouse-records <Skyrim Data> <plugins.txt> <new output directory>"
    );
    let data = PathBuf::from(&args[0]);
    let plugins = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);
    let started = Instant::now();
    let cells = converter::esm::inhouse::export_record_bundle(&data, &plugins, &output)?;
    println!(
        "inhouse diagnostic records: cells={cells} seconds={:.3} output={}",
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}
