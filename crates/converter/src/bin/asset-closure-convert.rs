use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::{
    asset_path::{AssetKind, canonical_asset_path},
    mesh::MeshConverter,
};
use rusqlite::Connection;
use serde::Serialize;
use std::{
    collections::VecDeque,
    env, fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug, Serialize)]
struct ConversionResult {
    model_path: String,
    source_path: Option<String>,
    output_path: String,
    status: &'static str,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ConversionReport {
    format_version: u32,
    source_root: PathBuf,
    assets_root: PathBuf,
    requested: usize,
    converted: usize,
    reused: usize,
    missing_sources: usize,
    /// Stored model paths the canonicalizer refused (absolute, traversing, or another asset
    /// kind). Nothing was read or written for them.
    invalid_model_paths: usize,
    failures: usize,
    passed: bool,
    results: Vec<ConversionResult>,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args = env::args_os().skip(1);
    let source_root = required(&mut args, "source-mesh-root")?;
    let assets_root = required(&mut args, "assets-root")?;
    let report_path = required(&mut args, "report.json")?;
    if args.next().is_some() {
        bail!("usage: asset-closure-convert <source-mesh-root> <assets-root> <report.json>");
    }
    let source_root = source_root
        .canonicalize()
        .wrap_err_with(|| format!("source root does not exist: {}", source_root.display()))?;
    fs::create_dir_all(&assets_root)?;
    let assets_root = assets_root.canonicalize()?;
    let database = assets_root.join("skyrim_world.db");
    let connection =
        Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut models = connection
        .prepare(
            "SELECT DISTINCT s.model_path FROM statics s \
             INNER JOIN \"references\" r ON r.base_form_id = s.id \
             WHERE s.model_path IS NOT NULL AND s.model_path <> '' \
             ORDER BY s.model_path COLLATE NOCASE",
        )?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    models.sort_by_key(|model| model.to_ascii_lowercase());
    models.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    let requested = models.len();
    let queue = Arc::new(Mutex::new(VecDeque::from(models)));
    let results = Arc::new(Mutex::new(Vec::with_capacity(requested)));
    let completed = Arc::new(AtomicUsize::new(0));
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(8);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            let completed = Arc::clone(&completed);
            let source_root = &source_root;
            let assets_root = &assets_root;
            scope.spawn(move || {
                loop {
                    let Some(model_path) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let result = convert_model(model_path, source_root, assets_root);
                    results.lock().unwrap().push(result);
                    let count = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    if count.is_multiple_of(100) || count == requested {
                        eprintln!("Processed {count}/{requested} reachable NIF files");
                    }
                }
            });
        }
    });
    let mut results = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
    results.sort_by_key(|result| result.model_path.to_ascii_lowercase());
    let converted = count_status(&results, "converted");
    let reused = count_status(&results, "reused");
    let missing_sources = count_status(&results, "missing_source");
    let invalid_model_paths = count_status(&results, "invalid_model_path");
    let failures = count_status(&results, "failed");
    // Records may legitimately reference editor-only or otherwise unavailable
    // source assets. The geometry gate covers every source that exists in the
    // selected load-order extraction and fails only on conversion failures and
    // on stored paths the canonicalizer refuses.
    let passed = converted + reused + missing_sources == requested
        && failures == 0
        && invalid_model_paths == 0;
    let report = ConversionReport {
        format_version: 1,
        source_root,
        assets_root,
        requested,
        converted,
        reused,
        missing_sources,
        invalid_model_paths,
        failures,
        passed,
        results,
    };
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("closure conversion written to {}", report_path.display());
    if !passed {
        bail!("reachable NIF conversion is incomplete");
    }
    Ok(())
}

fn required(args: &mut impl Iterator<Item = std::ffi::OsString>, name: &str) -> Result<PathBuf> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| color_eyre::eyre::eyre!("missing {name}"))
}

/// Converts or reuses one reachable model. A stored path the canonicalizer refuses is reported,
/// never joined onto the roots: a raw `..` or absolute plugin string would read or write outside
/// both the source and the assets tree.
fn convert_model(model_path: String, source_root: &Path, assets_root: &Path) -> ConversionResult {
    let relative = match relative_model_path(&model_path) {
        Ok(relative) => relative,
        Err(error) => {
            return ConversionResult {
                model_path,
                source_path: None,
                output_path: String::new(),
                status: "invalid_model_path",
                error: Some(format!("{error:#}")),
            };
        }
    };
    let source = source_root.join(&relative);
    let output = assets_root
        .join("meshes")
        .join(&relative)
        .with_extension("glb");
    let relative_output = output
        .strip_prefix(assets_root)
        .unwrap_or(&output)
        .to_string_lossy()
        .replace('\\', "/");
    if !source.is_file() {
        return ConversionResult {
            model_path,
            source_path: None,
            output_path: relative_output,
            status: "missing_source",
            error: Some("source NIF is missing".to_owned()),
        };
    }
    if output.is_file()
        && MeshConverter::glb_bounds(&output).is_ok()
        && MeshConverter::glb_texture_uris(&output).is_ok()
    {
        return ConversionResult {
            model_path,
            source_path: Some(source.to_string_lossy().into_owned()),
            output_path: relative_output,
            status: "reused",
            error: None,
        };
    }
    match MeshConverter::convert_nif_to_glb(&source, &output) {
        Ok(()) => ConversionResult {
            model_path,
            source_path: Some(source.to_string_lossy().into_owned()),
            output_path: relative_output,
            status: "converted",
            error: None,
        },
        Err(error) => ConversionResult {
            model_path,
            source_path: Some(source.to_string_lossy().into_owned()),
            output_path: relative_output,
            status: "failed",
            error: Some(format!("{error:#}")),
        },
    }
}

/// The source path relative to the mesh root. A stored path the canonicalizer refuses is an
/// error, not a raw string to join onto either root.
fn relative_model_path(model_path: &str) -> Result<PathBuf> {
    let canonical = PathBuf::from(canonical_asset_path(model_path, AssetKind::Mesh, "nif")?);
    Ok(canonical
        .strip_prefix("meshes")
        .unwrap_or(&canonical)
        .to_owned())
}

fn count_status(results: &[ConversionResult], status: &str) -> usize {
    results
        .iter()
        .filter(|result| result.status == status)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_optional_meshes_prefix() {
        assert_eq!(
            relative_model_path("Meshes\\Architecture\\Wall.NIF").unwrap(),
            Path::new("architecture/wall.nif")
        );
    }

    #[test]
    fn refuses_a_model_path_that_leaves_the_roots() {
        let directory = tempfile::tempdir().unwrap();
        let source_root = directory.path().join("source");
        let assets_root = directory.path().join("assets");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&assets_root).unwrap();
        for escaping in [
            "..\\..\\payload.nif",
            "C:/payload.nif",
            "meshes/../../payload.nif",
        ] {
            let result = convert_model(escaping.to_owned(), &source_root, &assets_root);
            assert_eq!(result.status, "invalid_model_path", "{escaping}");
            assert!(result.error.is_some(), "{escaping}");
        }
        assert!(
            !directory.path().join("payload.glb").exists(),
            "a refused model path wrote outside the assets root"
        );
    }
}
