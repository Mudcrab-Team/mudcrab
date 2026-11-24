//! Refresh bounds after installing or removing native GLB winners in an offline pack.

use color_eyre::{
    Result,
    eyre::{WrapErr, ensure, eyre},
};
use std::path::PathBuf;

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: room-model-bounds-refresh <private-assets-dir> <meshes/path.glb>...";
    let root = PathBuf::from(args.next().ok_or_else(|| eyre!(usage))?);
    let paths = args
        .map(|path| {
            path.into_string()
                .map_err(|_| eyre!("GLB path must be UTF-8"))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!paths.is_empty(), "{usage}");
    let report = converter::integration::refresh_native_model_bounds(&root, &paths).wrap_err(
        "Keep the pack offline until the refresh succeeds or you restore the previous GLBs",
    )?;
    for (model, error) in report.unbounded_models {
        eprintln!("model has no usable bounds {model}: {error}");
    }
    println!(
        "Updated bounds for {} base forms and cleared bounds for {}",
        report.updated_base_forms, report.cleared_base_forms
    );
    Ok(())
}
