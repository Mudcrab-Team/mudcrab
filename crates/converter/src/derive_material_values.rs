//! Write the material values sidecar of an existing pack, without reconverting anything.
//!
//! A pack converted before `materials[].extras.openSkyrim.materialValues` existed holds its
//! specular values clamped and holds no environment map scale. This command reads the source
//! NIFs again and writes [`shared::material_values::MATERIAL_VALUES_FILE`]. It writes that one
//! file and nothing else; the pack it reads is not changed.
//!
//! A NIF is used only when its bytes have the SHA-256 the pack's conversion manifest records for
//! that mesh, whether it comes from a loose file or a `.bsa` archive of the Skyrim `Data` folder.
//! A mesh whose source is not found or does not parse is counted and left out; nothing is
//! filled in. The command refuses an output inside a registered pack, and an output that exists.

use crate::{
    archive,
    cache::{ConversionManifest, hash_bytes},
    material::{NifMaterialDisposition, build_nif_material_contract, material_values},
    mesh::MeshConverter,
    pack_clone::require_outside_registered_packs,
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use shared::material_values::{MaterialValues, MaterialValuesFile};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Inputs of `derive-material-values`.
#[derive(Debug, Clone)]
pub struct DeriveMaterialValuesOptions {
    /// The pack whose meshes the values belong to. Read only.
    pub pack: PathBuf,
    /// `packs.json` of the shared storage. The command only reads it.
    pub registry: PathBuf,
    /// The Skyrim `Data` folder the pack was converted from. Read only.
    pub data: PathBuf,
    /// The sidecar to create.
    pub output: PathBuf,
}

/// What a run found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeriveMaterialValuesReport {
    /// NIF meshes the manifest lists.
    pub meshes: usize,
    /// Meshes with at least one lighting material in the sidecar.
    pub written_meshes: usize,
    /// Materials in the sidecar.
    pub materials: usize,
    /// Materials with an environment map scale.
    pub environment_materials: usize,
    /// Meshes whose recorded source bytes were not found.
    pub missing_sources: Vec<String>,
    /// Meshes whose source did not parse, with the reason.
    pub unreadable: Vec<(String, String)>,
}

/// One mesh the manifest lists: where it was published and the hash of its source.
struct Wanted {
    output: String,
    source_hash: String,
}

/// Derives the sidecar. See the module documentation for the rules.
pub fn run(options: &DeriveMaterialValuesOptions) -> Result<DeriveMaterialValuesReport> {
    require_outside_registered_packs(&options.registry, &options.output)?;
    ensure!(
        !options.output.exists(),
        "refused: {} exists; this command only creates a new file",
        options.output.display()
    );
    let manifest_path = options.pack.join("conversion-manifest.json");
    ensure!(
        manifest_path.is_file(),
        "refused: {} is not a pack: it has no conversion manifest",
        options.pack.display()
    );
    // The manifest as written: `ConversionManifest::load` drops the entries of a producer whose
    // outputs a conversion may not reuse, and an older pack still names its sources.
    let manifest: ConversionManifest = serde_json::from_slice(&fs::read(&manifest_path)?)
        .wrap_err("refused: the pack's conversion manifest cannot be read")?;
    let mut wanted: BTreeMap<String, Wanted> = manifest
        .entries
        .iter()
        .filter(|(source, entry)| source.ends_with(".nif") && entry.output.ends_with(".glb"))
        .map(|(source, entry)| {
            (
                MaterialValuesFile::key(source),
                Wanted {
                    output: entry.output.clone(),
                    source_hash: entry.source_hash.clone(),
                },
            )
        })
        .collect();
    let mut report = DeriveMaterialValuesReport {
        meshes: wanted.len(),
        ..Default::default()
    };
    let mut file = MaterialValuesFile::new();

    // Loose files first, then every archive. The hash decides which copy the pack was made from.
    let loose: Vec<String> = wanted.keys().cloned().collect();
    for source in loose {
        let path = options.data.join(&source);
        if !path.is_file() {
            continue;
        }
        let bytes = fs::read(&path).wrap_err_with(|| format!("cannot read {}", path.display()))?;
        take(&source, &bytes, &mut wanted, &mut file, &mut report);
    }
    let mut archives = Vec::new();
    for entry in fs::read_dir(&options.data).wrap_err_with(|| {
        format!(
            "cannot read the game data folder {}",
            options.data.display()
        )
    })? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bsa"))
        {
            archives.push(path);
        }
    }
    archives.sort();
    for path in archives {
        if wanted.is_empty() {
            break;
        }
        archive::for_each_bsa_entry(&path, &mut |name, decompress| {
            let source = MaterialValuesFile::key(name);
            if wanted.contains_key(&source) {
                take(&source, &decompress()?, &mut wanted, &mut file, &mut report);
            }
            Ok(())
        })
        .wrap_err_with(|| format!("cannot read archive {}", path.display()))?;
    }
    report.missing_sources = wanted.into_keys().collect();
    if report.meshes > 0 && report.missing_sources.len() == report.meshes {
        bail!(
            "refused: none of the {} source meshes of the pack is in {}",
            report.meshes,
            options.data.display()
        );
    }
    report.written_meshes = file.meshes.len();
    write_new(&options.output, &serde_json::to_vec(&file)?)?;
    Ok(report)
}

/// Uses `bytes` for the mesh at `source` when they are the bytes the pack was converted from.
fn take(
    source: &str,
    bytes: &[u8],
    wanted: &mut BTreeMap<String, Wanted>,
    file: &mut MaterialValuesFile,
    report: &mut DeriveMaterialValuesReport,
) {
    let Some(mesh) = wanted.get(source) else {
        return;
    };
    if hash_bytes(bytes) != mesh.source_hash {
        return;
    }
    let output = MaterialValuesFile::key(&mesh.output);
    wanted.remove(source);
    match values_of(source, bytes) {
        Ok(values) if values.is_empty() => {}
        Ok(values) => {
            report.materials += values.len();
            report.environment_materials += values
                .values()
                .filter(|values| values.environment_map_scale.is_some())
                .count();
            file.meshes.insert(output, values);
        }
        Err(error) => report
            .unreadable
            .push((source.to_owned(), format!("{error:#}"))),
    }
}

/// The values of every validated lighting material of a NIF, by shader block.
fn values_of(source: &str, bytes: &[u8]) -> Result<BTreeMap<u32, MaterialValues>> {
    let path = Path::new(source);
    let (nif, _) = MeshConverter::parse_nif_bytes(bytes, path)?;
    Ok(build_nif_material_contract(&nif, path)?
        .iter()
        .filter_map(|shape| match &shape.disposition {
            NifMaterialDisposition::Validated { material } => {
                Some((material.shader_block, material_values(material)?))
            }
            NifMaterialDisposition::Excluded { .. } => None,
        })
        .collect())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| format!("cannot create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheEntry;

    fn registry(root: &Path) -> PathBuf {
        let pack = root.join("registered");
        fs::create_dir_all(&pack).unwrap();
        let registry = root.join("packs.json");
        fs::write(
            &registry,
            serde_json::to_vec(&serde_json::json!({"base": {"path": pack}})).unwrap(),
        )
        .unwrap();
        registry
    }

    #[test]
    fn an_output_inside_a_registered_pack_or_over_a_file_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let options = |output: PathBuf| DeriveMaterialValuesOptions {
            pack: root.path().join("clone"),
            registry: registry(root.path()),
            data: root.path().join("data"),
            output,
        };
        let inside = options(root.path().join("registered/material-values.json"));
        let error = run(&inside).unwrap_err();
        assert!(
            format!("{error:#}").contains("registered pack"),
            "{error:#}"
        );
        let existing = root.path().join("existing.json");
        fs::write(&existing, b"{}").unwrap();
        let error = run(&options(existing)).unwrap_err();
        assert!(format!("{error:#}").contains("exists"), "{error:#}");
    }

    #[test]
    fn only_the_recorded_source_bytes_are_used() {
        let mut wanted = BTreeMap::from([(
            "meshes/a.nif".to_owned(),
            Wanted {
                output: "meshes/a.glb".to_owned(),
                source_hash: hash_bytes(b"recorded"),
            },
        )]);
        let mut file = MaterialValuesFile::new();
        let mut report = DeriveMaterialValuesReport::default();
        take(
            "meshes/a.nif",
            b"another copy",
            &mut wanted,
            &mut file,
            &mut report,
        );
        assert_eq!(wanted.len(), 1, "another copy leaves the mesh wanted");
        assert!(report.unreadable.is_empty());
        // The recorded bytes are consumed; these are not a NIF, which is reported.
        take(
            "meshes/a.nif",
            b"recorded",
            &mut wanted,
            &mut file,
            &mut report,
        );
        assert!(wanted.is_empty() && file.meshes.is_empty());
        assert_eq!(report.unreadable.len(), 1);
    }

    #[test]
    fn a_data_folder_without_any_source_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let pack = root.path().join("clone");
        let data = root.path().join("data");
        fs::create_dir_all(&pack).unwrap();
        fs::create_dir_all(&data).unwrap();
        let mut manifest = ConversionManifest::default();
        manifest.entries.insert(
            "meshes/a.nif".to_owned(),
            CacheEntry {
                source_hash: hash_bytes(b"recorded"),
                output: "meshes/a.glb".to_owned(),
                output_size: 0,
                output_hash: String::new(),
            },
        );
        fs::write(
            pack.join("conversion-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let options = DeriveMaterialValuesOptions {
            pack,
            registry: registry(root.path()),
            data: data.clone(),
            output: root.path().join("material-values.json"),
        };
        let error = run(&options).unwrap_err();
        assert!(format!("{error:#}").contains("none of the 1"), "{error:#}");
        assert!(!options.output.exists());
        // A loose file with the recorded bytes is found; it is reported, not invented.
        fs::create_dir_all(data.join("meshes")).unwrap();
        fs::write(data.join("meshes/a.nif"), b"recorded").unwrap();
        let report = run(&options).unwrap();
        assert_eq!((report.meshes, report.materials), (1, 0));
        assert_eq!(report.unreadable.len(), 1);
        let written = MaterialValuesFile::parse(&fs::read(&options.output).unwrap()).unwrap();
        assert!(written.meshes.is_empty());
    }
}
