//! Compare observed differences between two world databases. Input identity and completeness are
//! not verified; see `docs/research/record-reader-divergence.md`.
//!
//! ```text
//! reader-diff compare LEGACY_DB INHOUSE_DB [--json FILE] [--samples N]
//! reader-diff build DATA_DIR PLUGINS_TXT NEW_OUTPUT_ROOT [--json FILE] [--samples N]
//! ```
//!
//! `build` atomically creates a new output root before either writer runs. The root must not
//! already exist. It writes `legacy/skyrim_world.db` (records only) and `inhouse/` (the in-house
//! record bundle, no asset conversion) under that root, then compares them. The result is a
//! diagnostic, not a reader correctness or source-completeness result.
use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::reader_diff::{Options, compare, summary};
use std::{
    fs,
    path::{Path, PathBuf},
};

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
            create_new_output_root(&output)?;
            fs::create_dir(output.join("legacy"))?;
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
            "usage: reader-diff compare LEGACY_DB INHOUSE_DB | build DATA_DIR PLUGINS_TXT NEW_OUTPUT_ROOT [--json FILE] [--samples N]"
        ),
    };
    let report = compare(&legacy, &inhouse, &options)?;
    print!("{}", summary(&report));
    if let Some(path) = json {
        fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    Ok(())
}

fn create_new_output_root(output: &Path) -> Result<()> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .wrap_err_with(|| format!("creating output parent {}", parent.display()))?;
    fs::create_dir(output).wrap_err_with(|| {
        format!(
            "creating fresh output root {} (the root must not already exist)",
            output.display()
        )
    })?;
    Ok(())
}
