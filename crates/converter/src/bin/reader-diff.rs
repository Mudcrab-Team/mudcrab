//! Compare what the legacy and in-house record readers publish from the same plugins.
//!
//! ```text
//! reader-diff compare <legacy skyrim_world.db> <in-house skyrim_world.db> [--json FILE] [--samples N]
//! reader-diff build <Data dir> <plugins.txt> <empty output dir> [--json FILE] [--samples N]
//! ```
//!
//! `build` writes `legacy/skyrim_world.db` (the legacy reader, records only) and `inhouse/` (the
//! in-house record bundle, records only, no asset conversion) under the output directory, then
//! compares them. See `docs/research/record-reader-divergence.md`.
use color_eyre::{Result, eyre::bail};
use converter::reader_diff::{Options, compare, summary};
use std::{fs, path::PathBuf};

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut options = Options::default();
    let mut json = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" if index + 1 < args.len() => {
                json = Some(PathBuf::from(args.remove(index + 1)));
                args.remove(index);
            }
            "--samples" if index + 1 < args.len() => {
                options.samples = args.remove(index + 1).parse()?;
                args.remove(index);
            }
            _ => index += 1,
        }
    }
    let (legacy, inhouse) = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["compare", legacy, inhouse] => (PathBuf::from(legacy), PathBuf::from(inhouse)),
        ["build", data, plugins, output] => {
            let (data, plugins, output) = (
                PathBuf::from(data),
                PathBuf::from(plugins),
                PathBuf::from(output),
            );
            fs::create_dir_all(output.join("legacy"))?;
            let legacy = output.join("legacy").join("skyrim_world.db");
            let paths = converter::esm::read_plugins_txt(&plugins, &data)?;
            eprintln!("legacy reader: {} plugins", paths.len());
            converter::esm::EsmParser::convert_plugins(&paths, &legacy)?;
            eprintln!("in-house reader");
            converter::esm::inhouse::export_record_bundle(
                &data,
                &plugins,
                &output.join("inhouse"),
            )?;
            (legacy, output.join("inhouse").join("skyrim_world.db"))
        }
        _ => bail!(
            "usage: reader-diff compare <legacy.db> <inhouse.db> | build <Data> <plugins.txt> <out> [--json FILE] [--samples N]"
        ),
    };
    let report = compare(&legacy, &inhouse, &options)?;
    print!("{}", summary(&report));
    if let Some(path) = json {
        fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    Ok(())
}
