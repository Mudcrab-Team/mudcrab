//! Produce a diagnostic in-house database/cache bundle without asset conversion.
use color_eyre::{Result, eyre::ensure};
use std::{path::PathBuf, time::Instant};

/// Export selected plugins to a fresh evidence directory without publishing a runtime pack.
fn main() -> Result<()> {
    color_eyre::install()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3 || args.len() == 5,
        "usage: inhouse-records <Skyrim Data> <plugins.txt> <new output directory> [--typed-signatures WEAP,NPC_,SPEL]"
    );
    let data = PathBuf::from(&args[0]);
    let plugins = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);
    let started = Instant::now();
    let mut signatures = Vec::new();
    if args.len() == 5 {
        ensure!(args[3] == "--typed-signatures", "unknown diagnostic option");
        for tag in args[4].to_string_lossy().split(',') {
            ensure!(
                tag.len() == 4 && tag.is_ascii(),
                "typed signatures must be four ASCII bytes"
            );
            signatures.push(tag.as_bytes().try_into().expect("checked signature"));
        }
    }
    let cells =
        converter::esm::inhouse::export_record_bundle_typed(&data, &plugins, &output, &signatures)?;
    println!(
        "inhouse diagnostic records: cells={cells} seconds={:.3} output={}",
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}
