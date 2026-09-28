use color_eyre::{Result, eyre::bail};
use converter::lip::{LipFile, VISEME_NAMES, decode};
use std::{env, path::PathBuf};
use walkdir::WalkDir;

fn main() -> Result<()> {
    color_eyre::install()?;
    let root = PathBuf::from(
        env::args_os()
            .nth(1)
            .ok_or_else(|| color_eyre::eyre::eyre!("usage: lip-check <lip-file-or-directory>"))?,
    );
    if !root.exists() {
        bail!("input does not exist: {}", root.display());
    }
    let mut paths = Vec::new();
    if root.is_dir() {
        for entry in WalkDir::new(&root) {
            let entry = entry?;
            let is_file = entry.file_type().is_file();
            let path = entry.into_path();
            if is_file
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lip"))
            {
                paths.push(path);
            }
        }
    } else {
        paths.push(root.clone());
    }
    paths.sort_by_key(|path| path.to_string_lossy().to_ascii_lowercase());
    if paths.is_empty() {
        bail!("no .lip files found under {}", root.display());
    }

    let mut passed = 0;
    let mut failed = 0;
    for (index, path) in paths.iter().enumerate() {
        match decode(path) {
            Ok(lip) => match validate_translation(&lip) {
                Ok(()) => {
                    passed += 1;
                    if paths.len() == 1 {
                        println!(
                            "OK {} frames={} decoded_rows={} visemes=16 timing={}",
                            path.display(),
                            lip.header.frames,
                            lip.grid.len(),
                            if lip.timing_first.is_some() {
                                "reliable"
                            } else {
                                "ambiguous"
                            }
                        );
                    }
                }
                Err(error) => {
                    failed += 1;
                    eprintln!("FAIL {}: {error}", path.display());
                }
            },
            Err(error) => {
                failed += 1;
                eprintln!("FAIL {}: {error:#}", path.display());
            }
        }
        if paths.len() > 1 && ((index + 1) % 500 == 0 || index + 1 == paths.len()) {
            eprintln!(
                "Checked {}/{}: {passed} passed, {failed} failed",
                index + 1,
                paths.len()
            );
        }
    }
    println!(
        "Checked {} .lip file(s): {passed} passed, {failed} failed",
        paths.len()
    );
    if failed > 0 {
        bail!("one or more .lip files failed validation");
    }
    Ok(())
}

fn validate_translation(lip: &LipFile) -> Result<()> {
    for (frame, values) in lip.visemes().iter().enumerate() {
        for (index, value) in values.iter().enumerate() {
            if !value.is_finite() || !(-1e-4..=1.0001).contains(value) {
                bail!(
                    "invalid {} viseme value {value} at frame {frame}",
                    VISEME_NAMES[index]
                );
            }
        }
    }
    Ok(())
}
