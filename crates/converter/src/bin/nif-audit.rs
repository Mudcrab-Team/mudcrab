use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::{cache::hash_file, mesh::MeshConverter};
use serde::Serialize;
use std::{collections::BTreeMap, ffi::OsString, fs, path::PathBuf, time::Instant};
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
    animation_skipped_channels: BTreeMap<String, usize>,
    parse_error: Option<String>,
    conversion_error: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct AuditSummary {
    total_files: usize,
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
    animation_skipped_channels: BTreeMap<String, usize>,
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
    let started = Instant::now();
    let mut paths: Vec<_> = WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("nif"))
        })
        .collect();
    paths.sort_by_key(|path| path.to_string_lossy().to_ascii_lowercase());
    let mut summary = AuditSummary {
        total_files: paths.len(),
        ..Default::default()
    };
    let mut files = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let relative = path.strip_prefix(&root).unwrap_or(path);
        let relative_string = relative.to_string_lossy().replace('\\', "/");
        let size = fs::metadata(path)?.len();
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
                merge_counts(
                    &mut summary.animation_skipped_channels,
                    &diagnostics.animation_skipped_channels,
                );
                summary.material_shapes += diagnostics.material_shape_count;
                summary.validated_material_shapes += diagnostics.validated_material_shape_count;
                summary.excluded_material_shapes += diagnostics.excluded_material_shape_count;
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
                    animation_skipped_channels: diagnostics.animation_skipped_channels,
                    parse_error: None,
                    conversion_error: None,
                }
            }
            Err(error) => {
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
                    animation_skipped_channels: BTreeMap::new(),
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
    let report = AuditReport {
        format_version: 2,
        root,
        elapsed_ms: started.elapsed().as_millis(),
        summary,
        files,
    };
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)
        .wrap_err_with(|| format!("failed to write {}", report_path.display()))?;
    println!("NIF audit written to {}", report_path.display());
    Ok(())
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
