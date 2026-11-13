//! Resolve a complete native load order before requesting the existing legacy database writer.
use color_eyre::{Result, eyre::ensure};
use converter::esm::{EsmParser, form_key_merge::LegacyWriterOutcome};
use std::path::PathBuf;

/// Keep extended native admission explicit while writer/reader formats remain unchanged.
fn main() -> Result<()> {
    color_eyre::install()?;
    let args: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    ensure!(
        matches!(args.len(), 3 | 4),
        "usage: formkey-records <Data> <plugins.txt> <legacy database> [Skyrim.ccc]"
    );
    let (converted, writer) = EsmParser::convert_selected_form_key_plugins(
        &args[0],
        &args[1],
        args.get(3).map(|p| p.as_path()),
        None,
        &args[2],
    )?;
    println!(
        "FormKey native conversion: plugins={} winners={} warnings={}",
        converted.order.plugin_order().len(),
        converted.records.len(),
        converted.warnings.len()
    );
    match writer {
        LegacyWriterOutcome::Published => println!("legacy database published"),
        LegacyWriterOutcome::Unavailable { .. } => {
            for warning in converted.order.projection_warnings() {
                eprintln!("{warning}");
            }
            println!("native conversion complete; legacy database unavailable")
        }
    }
    Ok(())
}
