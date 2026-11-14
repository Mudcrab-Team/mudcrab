//! Upgrade converted GLBs with authored NIF collision without rerunning texture conversion.
use color_eyre::{
    Result,
    eyre::{WrapErr, ensure},
};
use converter::mesh::MeshConverter;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

/// Annotates usable models and records per-model failures before returning incomplete status.
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
    let mut failures = BTreeMap::new();
    for entry in WalkDir::new(&glb_root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|ext| ext != "glb") {
            continue;
        }
        let relative = entry.path().strip_prefix(&glb_root)?;
        let source = nif_root.join(relative).with_extension("nif");
        if !source.is_file() {
            record_failure(&mut failures, relative, "NIF source missing");
            continue;
        }
        let output = output_root.join(relative);
        fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
        fs::copy(entry.path(), &output)
            .wrap_err_with(|| format!("copying {}", relative.display()))?;
        let collision = match MeshConverter::annotate_glb_collision(&source, &output) {
            Ok(collision) => collision,
            Err(error) => {
                // This copy has no valid annotation. Cleanup failure is an output
                // integrity error; a bad source model is a recorded asset failure.
                fs::remove_file(&output)
                    .wrap_err_with(|| format!("removing failed annotation {}", output.display()))?;
                record_failure(&mut failures, relative, &format!("{error:#}"));
                continue;
            }
        };
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
    fs::create_dir_all(&output_root)?;
    let report_path = output_root.join("collision-annotation-report.json");
    fs::write(
        &report_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "complete": failures.is_empty(), "annotated": models,
            "authored": authored, "absent": absent, "unsupported": unsupported,
            "failures": failures,
        }))?,
    )?;
    println!(
        "annotated {models} GLBs: {authored} with collision, {absent} authored absent, {unsupported} unsupported; {} failed models",
        failures.len()
    );
    // Signal incomplete output after every usable neighbor has been processed.
    ensure!(
        failures.is_empty(),
        "{} model(s) failed; see {}",
        failures.len(),
        report_path.display()
    );
    Ok(())
}

/// Keeps one bounded diagnostic per model in both stderr and the machine-readable report.
fn record_failure(failures: &mut BTreeMap<String, String>, relative: &Path, error: &str) {
    let key = relative.to_string_lossy().replace('\\', "/");
    let message: String = error.chars().take(2048).collect();
    eprintln!("warning: {key}: {message}");
    failures.insert(key, message);
}
