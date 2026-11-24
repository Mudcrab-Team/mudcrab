use color_eyre::{
    Result,
    eyre::{WrapErr, bail},
};
use converter::{mesh::MeshConverter, texture::inspect_runtime_ktx2};
use serde::Serialize;
use std::{env, fs, path::PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
struct AuditEntry {
    path: String,
    kind: &'static str,
    status: &'static str,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct AuditReport {
    format_version: u32,
    assets_root: PathBuf,
    glb_files: usize,
    ktx2_files: usize,
    valid_files: usize,
    invalid_files: usize,
    passed: bool,
    entries: Vec<AuditEntry>,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args = env::args_os().skip(1);
    let assets_root = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("usage: runtime-asset-audit <assets-root> <report.json>")
        })?
        .canonicalize()
        .wrap_err("assets root does not exist")?;
    let report_path = args.next().map(PathBuf::from).ok_or_else(|| {
        color_eyre::eyre::eyre!("usage: runtime-asset-audit <assets-root> <report.json>")
    })?;
    if args.next().is_some() {
        bail!("usage: runtime-asset-audit <assets-root> <report.json>");
    }

    let report = audit(&assets_root);
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)
        .wrap_err_with(|| format!("failed to write {}", report_path.display()))?;
    println!("runtime asset audit written to {}", report_path.display());
    color_eyre::eyre::ensure!(report.passed, "runtime GLB/KTX2 audit failed");
    Ok(())
}

fn audit(assets_root: &std::path::Path) -> AuditReport {
    let mut paths = Vec::new();
    let mut entries = Vec::new();
    for entry in WalkDir::new(assets_root) {
        match entry {
            Ok(entry) if entry.file_type().is_file() => {
                let path = entry.into_path();
                if path.extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("glb") || extension.eq_ignore_ascii_case("ktx2")
                }) {
                    paths.push(path);
                }
            }
            Ok(_) => {}
            Err(error) => entries.push(AuditEntry {
                path: error
                    .path()
                    .unwrap_or(assets_root)
                    .to_string_lossy()
                    .replace('\\', "/"),
                kind: "filesystem",
                status: "invalid",
                error: Some(error.to_string()),
            }),
        }
    }
    paths.sort_by_key(|path| path.to_string_lossy().to_ascii_lowercase());

    let mut glb_files = 0usize;
    let mut ktx2_files = 0usize;
    entries.reserve(paths.len());
    for path in paths {
        let relative = path
            .strip_prefix(assets_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let (kind, result) = if extension.eq_ignore_ascii_case("glb") {
            glb_files += 1;
            ("glb", inspect_glb(&path))
        } else {
            ktx2_files += 1;
            ("ktx2", inspect_ktx2_file(&path))
        };
        entries.push(match result {
            Ok(()) => AuditEntry {
                path: relative,
                kind,
                status: "valid",
                error: None,
            },
            Err(error) => AuditEntry {
                path: relative,
                kind,
                status: "invalid",
                error: Some(format!("{error:#}")),
            },
        });
    }
    let invalid_files = entries
        .iter()
        .filter(|entry| entry.status == "invalid")
        .count();
    let valid_files = entries.len() - invalid_files;
    AuditReport {
        format_version: 1,
        assets_root: assets_root.to_owned(),
        glb_files,
        ktx2_files,
        valid_files,
        invalid_files,
        passed: glb_files > 0 && ktx2_files > 0 && invalid_files == 0,
        entries,
    }
}

fn inspect_glb(path: &std::path::Path) -> Result<()> {
    MeshConverter::glb_texture_uris(path)?;
    match MeshConverter::drawable_bounds(path) {
        Ok(_) => Ok(()),
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("GLB contains no bounded POSITION accessor")
                || message.contains("GLB has no accessors")
            {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

fn inspect_ktx2_file(path: &std::path::Path) -> Result<()> {
    let bytes = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    inspect_runtime_ktx2(&bytes).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use converter::texture::{TextureConverter, TextureEncoding};
    use ddsfile::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, NewDxgiParams};

    fn empty_scene_glb() -> Vec<u8> {
        let mut json = br#"{"asset":{"version":"2.0"}}"#.to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total_length = 20 + json.len();
        let mut glb = Vec::with_capacity(total_length);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2_u32.to_le_bytes());
        glb.extend_from_slice(&(total_length as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb
    }

    fn runtime_ktx2() -> Vec<u8> {
        let mut dds = Dds::new_dxgi(NewDxgiParams {
            height: 4,
            width: 4,
            depth: None,
            format: DxgiFormat::R8G8B8A8_UNorm,
            mipmap_levels: None,
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: D3D10ResourceDimension::Texture2D,
            alpha_mode: AlphaMode::Straight,
        })
        .unwrap();
        dds.data.fill(128);
        let mut source = Vec::new();
        dds.write(&mut source).unwrap();
        TextureConverter::convert(&source, TextureEncoding::ColorSrgb).unwrap()
    }

    #[test]
    fn accepts_valid_renderable_container_formats() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("empty.glb"), empty_scene_glb()).unwrap();
        fs::write(directory.path().join("color.ktx2"), runtime_ktx2()).unwrap();
        let report = audit(directory.path());
        assert_eq!(report.valid_files, 2);
        assert_eq!(report.invalid_files, 0);
        assert!(report.passed);
    }

    #[test]
    fn rejects_invalid_runtime_assets_and_requires_both_formats() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("broken.glb"), b"not a glb").unwrap();
        fs::write(directory.path().join("broken.ktx2"), b"not a ktx2").unwrap();
        let report = audit(directory.path());
        assert_eq!((report.glb_files, report.ktx2_files), (1, 1));
        assert_eq!(report.invalid_files, 2);
        assert!(!report.passed);

        fs::remove_file(directory.path().join("broken.ktx2")).unwrap();
        let report = audit(directory.path());
        assert_eq!(report.ktx2_files, 0);
        assert!(!report.passed);
    }
}
