//! Upgrade converted GLBs with authored NIF collision without rerunning texture conversion.
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use converter::mesh::MeshConverter;
use std::{
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

fn main() -> Result<()> {
    color_eyre::install()?;
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    ensure!(
        args.len() == 3,
        "usage: collision-annotate <nif-mesh-root> <glb-mesh-root> <output-mesh-root>"
    );
    let [nif_root, glb_root, output_root] = <[PathBuf; 3]>::try_from(args).unwrap();
    ensure!(
        nif_root.is_dir() && glb_root.is_dir(),
        "NIF and GLB roots must be directories"
    );
    ensure!(
        !output_root.exists(),
        "output root already exists: {}",
        output_root.display()
    );
    let mut models = 0usize;
    let mut authored = 0usize;
    let mut absent = 0usize;
    let mut unsupported = 0usize;
    for entry in WalkDir::new(&glb_root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|ext| ext != "glb") {
            continue;
        }
        let relative = entry.path().strip_prefix(&glb_root)?;
        let source = nif_root.join(relative).with_extension("nif");
        ensure!(
            source.is_file(),
            "NIF source missing for {}",
            relative.display()
        );
        let output = output_root.join(relative);
        fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
        fs::copy(entry.path(), &output)
            .wrap_err_with(|| format!("copying {}", relative.display()))?;
        let collision = MeshConverter::annotate_glb_collision(&source, &output)
            .wrap_err_with(|| format!("extracting collision from {}", relative.display()))?;
        models += 1;
        if !collision.shapes.is_empty() {
            authored += 1;
        } else if collision.skipped.is_empty() {
            absent += 1;
        } else {
            unsupported += 1;
        }
        if !collision.skipped.is_empty() {
            eprintln!("{}: {}", relative.display(), collision.skipped.join("; "));
        }
    }
    println!(
        "annotated {models} GLBs: {authored} with collision, {absent} authored absent, {unsupported} unsupported"
    );
    Ok(())
}
