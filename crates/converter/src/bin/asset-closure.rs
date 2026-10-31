use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::asset_path::{AssetKind, canonical_asset_path, resolve_asset_uri};
use converter::{mesh::MeshConverter, texture::TextureSemantic};
use rusqlite::Connection;
use serde::Serialize;
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
struct ClosureAsset {
    model_path: String,
    classification: &'static str,
    glb_path: Option<String>,
    bounds: Option<SerializableBounds>,
    texture_uris: Vec<String>,
    missing_textures: Vec<String>,
    texture_dependencies: Vec<ClosureTextureDependency>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ClosureTextureDependency {
    uri: String,
    semantic: TextureSemantic,
    required: bool,
    status: &'static str,
    resolved_path: Option<String>,
    error: Option<String>,
}

struct AssetInspection {
    bounds: Option<shared::Bounds3>,
    texture_uris: Vec<String>,
    missing_textures: Vec<String>,
    texture_dependencies: Vec<ClosureTextureDependency>,
}

#[derive(Debug, Serialize)]
struct SerializableBounds {
    min: [f32; 3],
    max: [f32; 3],
}

impl From<shared::Bounds3> for SerializableBounds {
    fn from(bounds: shared::Bounds3) -> Self {
        Self {
            min: bounds.min,
            max: bounds.max,
        }
    }
}

#[derive(Debug, Default, Serialize)]
struct ClosureSummary {
    unique_models: usize,
    valid_models: usize,
    missing_models: usize,
    invalid_models: usize,
    invalid_geometry_models: usize,
    non_renderable_models: usize,
    unavailable_source_models: usize,
    external_texture_references: usize,
    missing_texture_references: usize,
    missing_required_texture_references: usize,
    missing_optional_texture_references: usize,
    unavailable_texture_source_references: usize,
    invalid_texture_references: usize,
}

#[derive(Debug, Serialize)]
struct ClosureReport {
    format_version: u32,
    record_scope: [&'static str; 3],
    assets_root: PathBuf,
    database_schema: u32,
    summary: ClosureSummary,
    assets: Vec<ClosureAsset>,
    geometry_passed: bool,
    passed: bool,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args = env::args_os().skip(1);
    let assets_root = args.next().map(PathBuf::from).ok_or_else(|| {
        color_eyre::eyre::eyre!("usage: asset-closure <assets-root> <report.json>")
    })?;
    let report_path = args.next().map(PathBuf::from).ok_or_else(|| {
        color_eyre::eyre::eyre!("usage: asset-closure <assets-root> <report.json>")
    })?;
    let source_mesh_root = args.next().map(PathBuf::from);
    let source_texture_root = args.next().map(PathBuf::from);
    if args.next().is_some() {
        bail!(
            "usage: asset-closure <assets-root> <report.json> [source-mesh-root] [source-texture-root]"
        );
    }
    let assets_root = assets_root
        .canonicalize()
        .wrap_err_with(|| format!("assets root does not exist: {}", assets_root.display()))?;
    let source_mesh_root = source_mesh_root
        .map(|path| path.canonicalize())
        .transpose()
        .wrap_err("source mesh root does not exist")?;
    let source_texture_root = source_texture_root
        .map(|path| path.canonicalize())
        .transpose()
        .wrap_err("source texture root does not exist")?;
    let database_path = assets_root.join("skyrim_world.db");
    let connection =
        Connection::open_with_flags(&database_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .wrap_err_with(|| format!("failed to open {}", database_path.display()))?;
    let database_schema =
        connection.query_row("SELECT version FROM schema_info LIMIT 1", [], |row| {
            row.get::<_, u32>(0)
        })?;
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

    let files = file_index(&assets_root)?;
    let mut summary = ClosureSummary {
        unique_models: models.len(),
        ..Default::default()
    };
    let mut assets = Vec::with_capacity(models.len());
    for model_path in models {
        let canonical = match canonical_asset_path(&model_path, AssetKind::Mesh, "nif") {
            Ok(canonical) => canonical,
            Err(error) => {
                // A stored path the canonicalizer refuses (absolute, traversing, or another
                // asset kind) is reported instead of being joined onto either root.
                summary.invalid_models += 1;
                assets.push(ClosureAsset {
                    model_path,
                    classification: "invalid_model_path",
                    glb_path: None,
                    bounds: None,
                    texture_uris: Vec::new(),
                    missing_textures: Vec::new(),
                    texture_dependencies: Vec::new(),
                    error: Some(format!("{error:#}")),
                });
                continue;
            }
        };
        let key = converted_model_key(&canonical);
        let Some(glb_path) = files.get(&key) else {
            let source_unavailable = source_mesh_root
                .as_ref()
                .is_some_and(|root| !root.join(relative_model_path(&canonical)).is_file());
            if source_unavailable {
                summary.unavailable_source_models += 1;
            } else {
                summary.missing_models += 1;
            }
            assets.push(ClosureAsset {
                model_path,
                classification: if source_unavailable {
                    "unavailable_source"
                } else {
                    "missing_conversion"
                },
                glb_path: None,
                bounds: None,
                texture_uris: Vec::new(),
                missing_textures: Vec::new(),
                texture_dependencies: Vec::new(),
                error: (!source_unavailable).then(|| "converted GLB is missing".to_owned()),
            });
            continue;
        };
        let relative_glb = normalize(glb_path.strip_prefix(&assets_root).unwrap_or(glb_path));
        match inspect_asset(&assets_root, glb_path, source_texture_root.as_deref()) {
            Ok(AssetInspection {
                bounds,
                texture_uris,
                missing_textures,
                texture_dependencies,
            }) => {
                summary.external_texture_references += texture_uris.len();
                summary.missing_texture_references += missing_textures.len();
                summary.missing_required_texture_references += texture_dependencies
                    .iter()
                    .filter(|dependency| dependency.required && dependency.status == "missing")
                    .count();
                summary.missing_optional_texture_references += texture_dependencies
                    .iter()
                    .filter(|dependency| !dependency.required && dependency.status == "missing")
                    .count();
                summary.unavailable_texture_source_references += texture_dependencies
                    .iter()
                    .filter(|dependency| dependency.status == "unavailable_source")
                    .count();
                summary.invalid_texture_references += texture_dependencies
                    .iter()
                    .filter(|dependency| dependency.status == "invalid")
                    .count();
                let has_blocking_texture = texture_dependencies.iter().any(|dependency| {
                    dependency.status == "invalid"
                        || (dependency.required && dependency.status == "missing")
                });
                if bounds.is_none() {
                    summary.non_renderable_models += 1;
                } else if !has_blocking_texture {
                    summary.valid_models += 1;
                } else {
                    summary.invalid_models += 1;
                }
                assets.push(ClosureAsset {
                    model_path,
                    classification: if bounds.is_none() {
                        "non_renderable"
                    } else if !has_blocking_texture {
                        "renderable"
                    } else {
                        "missing_textures"
                    },
                    glb_path: Some(relative_glb),
                    bounds: bounds.map(Into::into),
                    texture_uris,
                    missing_textures,
                    texture_dependencies,
                    error: None,
                });
            }
            Err(error) => {
                summary.invalid_models += 1;
                summary.invalid_geometry_models += 1;
                assets.push(ClosureAsset {
                    model_path,
                    classification: "invalid_glb",
                    glb_path: Some(relative_glb),
                    bounds: None,
                    texture_uris: Vec::new(),
                    missing_textures: Vec::new(),
                    texture_dependencies: Vec::new(),
                    error: Some(format!("{error:#}")),
                });
            }
        }
    }
    let geometry_passed = database_schema == shared::WORLD_DATABASE_SCHEMA_VERSION
        && summary.missing_models == 0
        && summary.invalid_geometry_models == 0;
    let passed = geometry_passed
        && summary.valid_models + summary.non_renderable_models + summary.unavailable_source_models
            == summary.unique_models
        && summary.missing_models == 0
        && summary.invalid_models == 0
        && summary.missing_required_texture_references == 0
        && summary.invalid_texture_references == 0;
    let report = ClosureReport {
        format_version: 2,
        record_scope: ["STAT", "MSTT", "FURN"],
        assets_root,
        database_schema,
        summary,
        assets,
        geometry_passed,
        passed,
    };
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)
        .wrap_err_with(|| format!("failed to write {}", report_path.display()))?;
    println!("asset closure written to {}", report_path.display());
    if !report.passed {
        bail!("Phase 2 asset closure failed");
    }
    Ok(())
}

fn inspect_asset(
    assets_root: &Path,
    glb_path: &Path,
    source_texture_root: Option<&Path>,
) -> Result<AssetInspection> {
    let bounds = match MeshConverter::glb_bounds(glb_path) {
        Ok(bounds) => Some(bounds),
        Err(error)
            if format!("{error:#}").contains("GLB contains no bounded POSITION accessor")
                || format!("{error:#}").contains("GLB has no accessors") =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    let texture_uris = MeshConverter::glb_texture_uris(glb_path)?;
    let dependencies = MeshConverter::glb_texture_dependencies(glb_path)?;
    let mut missing = Vec::new();
    let mut resolved = Vec::with_capacity(dependencies.len());
    for dependency in dependencies {
        if dependency.uri.starts_with("data:") {
            resolved.push(ClosureTextureDependency {
                uri: dependency.uri,
                semantic: dependency.semantic,
                required: dependency.required,
                status: "embedded",
                resolved_path: None,
                error: None,
            });
            continue;
        }
        match resolve_asset_uri(assets_root, glb_path, &dependency.uri) {
            Ok(candidate) => {
                let exists = candidate.is_file();
                if !exists {
                    missing.push(dependency.uri.clone());
                }
                let source_unavailable = !exists
                    && source_texture_root.is_some_and(|root| {
                        candidate
                            .strip_prefix(assets_root)
                            .ok()
                            .and_then(|relative| {
                                canonical_asset_path(
                                    &relative.to_string_lossy(),
                                    AssetKind::Texture,
                                    "dds",
                                )
                                .ok()
                            })
                            .and_then(|key| {
                                Path::new(&key)
                                    .strip_prefix("textures")
                                    .ok()
                                    .map(|relative| root.join(relative))
                            })
                            .is_none_or(|source| !source.is_file())
                    });
                resolved.push(ClosureTextureDependency {
                    uri: dependency.uri,
                    semantic: dependency.semantic,
                    required: dependency.required,
                    status: if exists {
                        "available"
                    } else if source_unavailable {
                        "unavailable_source"
                    } else {
                        "missing"
                    },
                    resolved_path: candidate.strip_prefix(assets_root).ok().map(normalize),
                    error: None,
                });
            }
            Err(error) => resolved.push(ClosureTextureDependency {
                uri: dependency.uri,
                semantic: dependency.semantic,
                required: dependency.required,
                status: "invalid",
                resolved_path: None,
                error: Some(format!("{error:#}")),
            }),
        }
    }
    missing.sort_by_key(|uri| uri.to_ascii_lowercase());
    missing.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    Ok(AssetInspection {
        bounds,
        texture_uris,
        missing_textures: missing,
        texture_dependencies: resolved,
    })
}

fn relative_model_path(canonical_source: &str) -> PathBuf {
    let canonical = Path::new(canonical_source);
    canonical
        .strip_prefix("meshes")
        .unwrap_or(canonical)
        .to_owned()
}

fn file_index(root: &Path) -> Result<HashMap<String, PathBuf>> {
    let mut files = HashMap::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(root)?;
            let key = normalize(relative);
            if let Some(previous) = files.insert(key.clone(), entry.path().to_owned()) {
                bail!(
                    "normalized asset collision for {key}: {} and {}",
                    previous.display(),
                    entry.path().display()
                );
            }
        }
    }
    Ok(files)
}

/// The converted GLB key for a canonical source path: the same path with the GLB extension.
fn converted_model_key(canonical_source: &str) -> String {
    let mut key = PathBuf::from(canonical_source);
    key.set_extension("glb");
    normalize(&key)
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_model_paths() {
        assert_eq!(
            converted_model_key("meshes/architecture/wall.nif"),
            "meshes/architecture/wall.glb"
        );
        assert_eq!(
            relative_model_path("meshes/architecture/wall.nif"),
            Path::new("architecture/wall.nif")
        );
    }

    #[test]
    fn refuses_model_paths_that_leave_the_assets_root() {
        for escaping in [
            "..\\..\\payload.nif",
            "C:/payload.nif",
            "/payload.nif",
            "meshes/../../payload.nif",
        ] {
            assert!(
                canonical_asset_path(escaping, AssetKind::Mesh, "nif").is_err(),
                "{escaping} was accepted"
            );
        }
    }

    #[test]
    fn rejects_texture_escape_outside_assets() {
        let root = Path::new("C:/assets");
        let glb = root.join("meshes/a/model.glb");
        assert!(resolve_asset_uri(root, &glb, "../../textures/a.ktx2").is_ok());
        assert!(resolve_asset_uri(root, &glb, "../../../secret.ktx2").is_err());
    }
}
