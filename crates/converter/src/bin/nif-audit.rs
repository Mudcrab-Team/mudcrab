use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::{
    cache::hash_file,
    mesh::{MeshConverter, NifUnparsedRegion},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
struct AuditFile {
    path: String,
    size: u64,
    sha256: String,
    classification: &'static str,
    block_count: usize,
    parsed_block_count: usize,
    geometry_block_count: usize,
    scene_node_count: usize,
    max_scene_depth: usize,
    block_types: BTreeMap<String, usize>,
    fallback_blocks: BTreeMap<String, usize>,
    fallback_offsets: BTreeMap<String, Vec<usize>>,
    material_shape_count: usize,
    validated_material_shape_count: usize,
    excluded_material_shape_count: usize,
    material_exclusions: BTreeMap<String, usize>,
    unparsed_regions: usize,
    unparsed_bytes: usize,
    unparsed_by_reason: BTreeMap<String, usize>,
    unparsed_samples: Vec<NifUnparsedRegion>,
    parse_error: Option<String>,
    conversion_error: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct AuditSummary {
    total_files: usize,
    total_file_bytes: u64,
    skipped_non_regular_entries: usize,
    skipped_non_regular_samples: Vec<String>,
    unaccounted_file_bytes: u64,
    parsed_files: usize,
    structural_failures: usize,
    material_failures: usize,
    renderable_candidates: usize,
    unsupported_geometry_files: usize,
    non_renderable_files: usize,
    files_with_fallbacks: usize,
    conversion_attempts: usize,
    conversion_successes: usize,
    conversion_failures: usize,
    scene_nodes: usize,
    max_scene_depth: usize,
    block_types: BTreeMap<String, usize>,
    fallback_blocks: BTreeMap<String, usize>,
    material_shapes: usize,
    validated_material_shapes: usize,
    excluded_material_shapes: usize,
    material_exclusions: BTreeMap<String, usize>,
    unparsed_regions: usize,
    unparsed_bytes: usize,
    unparsed_by_reason: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct AuditReport {
    format_version: u32,
    root: PathBuf,
    elapsed_ms: u128,
    summary: AuditSummary,
    files: Vec<AuditFile>,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let (root, report_path, convert_output) = parse_args(std::env::args_os().skip(1).collect())?;
    let report = audit(root, convert_output)?;
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)
        .wrap_err_with(|| format!("failed to write {}", report_path.display()))?;
    println!("NIF audit written to {}", report_path.display());
    Ok(())
}

fn collect_nif_paths(root: &Path, summary: &mut AuditSummary) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .follow_root_links(true)
        .sort_by_file_name()
    {
        let entry = entry?;
        let kind = entry.file_type();
        // walkdir traverses a linked root but still reports that entry as a symlink.
        if kind.is_dir() || (entry.depth() == 0 && kind.is_symlink()) {
            continue;
        }
        if !kind.is_file() {
            summary.skipped_non_regular_entries += 1;
            if summary.skipped_non_regular_samples.len() < 8 {
                summary.skipped_non_regular_samples.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap_or(entry.path())
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
            continue;
        }
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("nif"))
        {
            paths.push(entry.into_path());
        }
    }
    paths.sort_by_cached_key(|path| (path.to_string_lossy().to_ascii_lowercase(), path.clone()));
    Ok(paths)
}

fn audit(root: PathBuf, convert_output: Option<PathBuf>) -> Result<AuditReport> {
    let started = Instant::now();
    let mut summary = AuditSummary::default();
    let paths = collect_nif_paths(&root, &mut summary)?;
    summary.total_files = paths.len();
    let mut files = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let relative = path.strip_prefix(&root).unwrap_or(path);
        let relative_string = relative.to_string_lossy().replace('\\', "/");
        let size = fs::metadata(path)?.len();
        summary.total_file_bytes = summary
            .total_file_bytes
            .checked_add(size)
            .ok_or_else(|| color_eyre::eyre::eyre!("NIF input byte total overflow"))?;
        let sha256 = hash_file(path)?;
        let inspected = MeshConverter::inspect_nif(path);
        let mut file = match inspected {
            Ok(diagnostics) => {
                summary.parsed_files += 1;
                merge_counts(&mut summary.block_types, &diagnostics.block_types);
                merge_counts(&mut summary.fallback_blocks, &diagnostics.fallback_blocks);
                merge_counts(
                    &mut summary.material_exclusions,
                    &diagnostics.material_exclusions,
                );
                summary.material_shapes += diagnostics.material_shape_count;
                summary.validated_material_shapes += diagnostics.validated_material_shape_count;
                summary.excluded_material_shapes += diagnostics.excluded_material_shape_count;
                summary.unparsed_regions += diagnostics.unparsed_regions;
                summary.unparsed_bytes += diagnostics.unparsed_bytes;
                merge_counts(
                    &mut summary.unparsed_by_reason,
                    &diagnostics.unparsed_by_reason,
                );
                if !diagnostics.fallback_blocks.is_empty() {
                    summary.files_with_fallbacks += 1;
                }
                summary.scene_nodes += diagnostics.scene_node_count;
                summary.max_scene_depth = summary.max_scene_depth.max(diagnostics.max_scene_depth);
                let declared_geometry_count = diagnostics
                    .block_types
                    .iter()
                    .filter(|(block_type, _)| is_geometry_block_type(block_type))
                    .map(|(_, count)| *count)
                    .sum::<usize>();
                let classification = if diagnostics.geometry_block_count > 0 {
                    summary.renderable_candidates += 1;
                    "renderable"
                } else if declared_geometry_count > 0 {
                    summary.unsupported_geometry_files += 1;
                    "unsupported_geometry"
                } else {
                    summary.non_renderable_files += 1;
                    "non_renderable"
                };
                AuditFile {
                    path: relative_string,
                    size,
                    sha256,
                    classification,
                    block_count: diagnostics.block_count,
                    parsed_block_count: diagnostics.parsed_block_count,
                    geometry_block_count: diagnostics.geometry_block_count,
                    scene_node_count: diagnostics.scene_node_count,
                    max_scene_depth: diagnostics.max_scene_depth,
                    block_types: diagnostics.block_types,
                    fallback_blocks: diagnostics.fallback_blocks,
                    fallback_offsets: diagnostics.fallback_offsets,
                    material_shape_count: diagnostics.material_shape_count,
                    validated_material_shape_count: diagnostics.validated_material_shape_count,
                    excluded_material_shape_count: diagnostics.excluded_material_shape_count,
                    material_exclusions: diagnostics.material_exclusions,
                    unparsed_regions: diagnostics.unparsed_regions,
                    unparsed_bytes: diagnostics.unparsed_bytes,
                    unparsed_by_reason: diagnostics.unparsed_by_reason,
                    unparsed_samples: diagnostics.unparsed_samples,
                    parse_error: None,
                    conversion_error: None,
                }
            }
            Err(error) => {
                summary.unaccounted_file_bytes = summary
                    .unaccounted_file_bytes
                    .checked_add(size)
                    .ok_or_else(|| {
                    color_eyre::eyre::eyre!("unaccounted NIF byte total overflow")
                })?;
                let error = format!("{error:#}");
                let material_failure = error.contains("invalid NIF material");
                if material_failure {
                    summary.material_failures += 1;
                } else {
                    summary.structural_failures += 1;
                }
                AuditFile {
                    path: relative_string,
                    size,
                    sha256,
                    classification: if material_failure {
                        "material_failure"
                    } else {
                        "structural_failure"
                    },
                    block_count: 0,
                    parsed_block_count: 0,
                    geometry_block_count: 0,
                    scene_node_count: 0,
                    max_scene_depth: 0,
                    block_types: BTreeMap::new(),
                    fallback_blocks: BTreeMap::new(),
                    fallback_offsets: BTreeMap::new(),
                    material_shape_count: 0,
                    validated_material_shape_count: 0,
                    excluded_material_shape_count: 0,
                    material_exclusions: BTreeMap::new(),
                    unparsed_regions: 0,
                    unparsed_bytes: 0,
                    unparsed_by_reason: BTreeMap::new(),
                    unparsed_samples: Vec::new(),
                    parse_error: Some(error),
                    conversion_error: None,
                }
            }
        };
        if file.classification == "renderable"
            && let Some(output_root) = &convert_output
        {
            summary.conversion_attempts += 1;
            let output = output_root.join(relative).with_extension("glb");
            match MeshConverter::convert_nif_to_glb(path, &output) {
                Ok(()) => summary.conversion_successes += 1,
                Err(error) => {
                    summary.conversion_failures += 1;
                    file.conversion_error = Some(format!("{error:#}"));
                }
            }
        }
        files.push(file);
        if (index + 1) % 250 == 0 || index + 1 == paths.len() {
            eprintln!("Audited {}/{} NIF files", index + 1, paths.len());
        }
    }
    Ok(AuditReport {
        format_version: 4,
        root,
        elapsed_ms: started.elapsed().as_millis(),
        summary,
        files,
    })
}

fn is_geometry_block_type(block_type: &str) -> bool {
    matches!(
        block_type,
        "BSTriShape"
            | "BSDynamicTriShape"
            | "BSSubIndexTriShape"
            | "BSMeshLODTriShape"
            | "BSLODTriShape"
            | "NiTriShape"
            | "NiTriStrips"
    )
}

fn merge_counts(target: &mut BTreeMap<String, usize>, source: &BTreeMap<String, usize>) {
    for (key, value) in source {
        *target.entry(key.clone()).or_default() += value;
    }
}

fn parse_args(args: Vec<OsString>) -> Result<(PathBuf, PathBuf, Option<PathBuf>)> {
    if !(2..=3).contains(&args.len()) {
        bail!("usage: nif-audit <nif-root> <report.json> [converted-glb-root]");
    }
    let mut args = args.into_iter();
    let root = PathBuf::from(args.next().unwrap());
    if !root.is_dir() {
        bail!("NIF root does not exist: {}", root.display());
    }
    let report = PathBuf::from(args.next().unwrap());
    let converted = args.next().map(PathBuf::from);
    Ok((root, report, converted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_files_keep_their_whole_byte_denominator() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("broken.nif"), b"invalid NIF").unwrap();
        fs::write(root.path().join("empty.NIF"), []).unwrap();
        fs::write(
            root.path().join("ignored.txt"),
            b"outside the NIF denominator",
        )
        .unwrap();
        let report = audit(root.path().to_owned(), None).unwrap();
        assert_eq!(report.format_version, 4);
        assert_eq!(report.summary.total_files, 2);
        assert_eq!(report.summary.parsed_files, 0);
        assert_eq!(report.summary.total_file_bytes, 11);
        assert_eq!(report.summary.unaccounted_file_bytes, 11);
        assert_eq!(report.summary.unparsed_bytes, 0);
        assert!(report.files.iter().all(|file| file.parse_error.is_some()));
        assert_eq!(
            report.summary.unaccounted_file_bytes,
            report
                .files
                .iter()
                .filter(|file| file.parse_error.is_some())
                .map(|file| file.size)
                .sum::<u64>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn linked_root_is_audited_while_descendant_symlinks_stay_counted_and_unfollowed() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(root.path().join("kept.nif"), b"source").unwrap();
        fs::write(outside.path().join("hidden.nif"), b"not traversed").unwrap();
        symlink(
            outside.path().join("hidden.nif"),
            outside.path().join("skipped-descendant.nif"),
        )
        .unwrap();
        symlink(outside.path(), root.path().join("linked-directory")).unwrap();
        for index in 0..10 {
            symlink(
                outside.path().join("hidden.nif"),
                root.path().join(format!("link-{index}.nif")),
            )
            .unwrap();
        }
        let mut summary = AuditSummary::default();
        let paths = collect_nif_paths(root.path(), &mut summary).unwrap();
        assert_eq!(paths, [root.path().join("kept.nif")]);
        assert_eq!(summary.skipped_non_regular_entries, 11);
        assert_eq!(summary.skipped_non_regular_samples.len(), 8);
        let mut linked_root = AuditSummary::default();
        assert_eq!(
            collect_nif_paths(&root.path().join("linked-directory"), &mut linked_root).unwrap(),
            [root.path().join("linked-directory/hidden.nif")]
        );
        assert_eq!(linked_root.skipped_non_regular_entries, 1);
        assert_eq!(
            linked_root.skipped_non_regular_samples,
            ["skipped-descendant.nif"]
        );
        let report = audit(root.path().join("linked-directory"), None).unwrap();
        assert_eq!(report.summary.total_files, 1);
        assert_eq!(report.summary.total_file_bytes, 13);
        assert_eq!(report.summary.unaccounted_file_bytes, 13);
        assert_eq!(report.summary.skipped_non_regular_entries, 1);
        assert_eq!(report.files[0].path, "hidden.nif");
    }

    #[test]
    fn traversal_errors_cannot_shrink_the_denominator_silently() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            collect_nif_paths(&root.path().join("missing"), &mut AuditSummary::default()).is_err()
        );
    }
}
