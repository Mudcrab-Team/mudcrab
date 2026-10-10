use color_eyre::{Result, eyre::bail};
use converter::streaming_costs::export_pack;
use shared::streaming_costs::EstimateQuality;
use std::{env, path::PathBuf};

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args = env::args_os().skip(1);
    let mut assets = None;
    let mut output = None;
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--assets") if assets.is_none() => assets = args.next().map(PathBuf::from),
            Some("--output") if output.is_none() => output = args.next().map(PathBuf::from),
            Some("--help" | "-h") => {
                println!(
                    "usage: streaming-costs --assets <immutable-converted-pack> --output <new-sidecar.json>\nReads pack metadata without decoding assets. Output parent must exist and be outside the pack."
                );
                return Ok(());
            }
            _ => bail!(
                "usage: streaming-costs --assets <immutable-converted-pack> --output <new-sidecar.json>"
            ),
        }
    }
    let (Some(assets), Some(output)) = (assets, output) else {
        bail!(
            "usage: streaming-costs --assets <immutable-converted-pack> --output <new-sidecar.json>"
        );
    };
    let catalog = export_pack(&assets, &output)?;
    let unknown = catalog
        .resources
        .values()
        .filter(|resource| resource.quality == EstimateQuality::Unknown)
        .count();
    println!(
        "Wrote {} scenes and {} resources ({} unknown estimates) to {}",
        catalog.scenes.len(),
        catalog.resources.len(),
        unknown,
        output.display()
    );
    Ok(())
}
