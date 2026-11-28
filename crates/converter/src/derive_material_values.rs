//! Write the material values sidecar of an existing pack, without reconverting anything.
//!
//! A pack converted before `materials[].extras.openSkyrim.materialValues` existed holds its
//! specular values clamped and holds no environment map scale. This command reads the source
//! NIFs again and writes [`shared::material_values::MATERIAL_VALUES_FILE`]. It writes that one
//! file and nothing else; the pack it reads is not changed.
//!
//! A NIF is used only when its bytes have the SHA-256 the pack's conversion manifest records for
//! that mesh (the first part of a skinned mesh's `<mesh>:<skeleton>` hash), whether it comes from a loose file or a `.bsa` archive of the Skyrim `Data` folder.
//! A loose file is read by its spelling on disk. A copy in an archive that does not decompress
//! is reported, and the other archives are still searched. A mesh whose source is not found or
//! does not parse is counted and left out; nothing is filled in.
//!
//! The command refuses an output inside the pack it reads or inside a registered pack, and an
//! output that exists. The sidecar gets its name only after its bytes are written and synced.

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
    /// Archive copies of a listed mesh that did not decompress (`<archive>: <mesh>`), with the
    /// reason. Another copy may still have supplied the mesh.
    pub failed_copies: Vec<(String, String)>,
    /// Meshes whose source did not parse, with the reason.
    pub unreadable: Vec<(String, String)>,
}

/// One mesh the manifest lists: where it was published and the hash of its source.
struct Wanted {
    output: String,
    source_hash: String,
}

/// The hash of the mesh's own bytes in a manifest source hash. A skinned mesh records
/// `<mesh>:<skeleton>`.
fn mesh_hash(source_hash: &str) -> &str {
    source_hash
        .split_once(':')
        .map_or(source_hash, |(mesh, _)| mesh)
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
    require_outside_pack(&options.pack, &options.output)?;
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
                    source_hash: mesh_hash(&entry.source_hash).to_owned(),
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
    for (source, path) in loose_sources(&options.data, &wanted)? {
        if !wanted.contains_key(&source) {
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
            if !wanted.contains_key(&source) {
                return Ok(());
            }
            match decompress() {
                Ok(bytes) => take(&source, &bytes, &mut wanted, &mut file, &mut report),
                Err(error) => {
                    let copy = format!("{}: {source}", path.display());
                    let reason = format!("{error:#}");
                    log::warn!("cannot decompress {copy}: {reason}");
                    report.failed_copies.push((copy, reason));
                }
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
    let bytes = serde_json::to_vec(&file)?;
    publish_new(&options.output, |sidecar| sidecar.write_all(&bytes))?;
    Ok(report)
}

/// Refuses an output inside the pack the command reads, registered or not. The output's folder
/// must exist.
fn require_outside_pack(pack: &Path, output: &Path) -> Result<()> {
    let folder = fs::canonicalize(folder_of(output))
        .wrap_err_with(|| format!("refused: the folder of {} does not exist", output.display()))?;
    let pack = fs::canonicalize(pack)
        .wrap_err_with(|| format!("refused: cannot resolve the pack {}", pack.display()))?;
    ensure!(
        !folder.starts_with(&pack),
        "refused: {} is inside the pack {}, which this command only reads",
        output.display(),
        pack.display()
    );
    Ok(())
}

fn folder_of(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// The loose files of `data` that name a wanted mesh, as `(key, path as spelled on disk)`. A
/// manifest key is lower case, so joining it to `data` misses `Meshes/A.NIF` on a
/// case-sensitive volume. Every spelling is returned.
fn loose_sources(data: &Path, wanted: &BTreeMap<String, Wanted>) -> Result<Vec<(String, PathBuf)>> {
    let mut sources = Vec::new();
    for entry in walkdir::WalkDir::new(data)
        .follow_links(true)
        .sort_by_file_name()
    {
        let entry = entry
            .wrap_err_with(|| format!("cannot read the game data folder {}", data.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        // A name that is not UTF-8 matches no manifest key, with or without its lossy letters.
        let key = MaterialValuesFile::key(&entry.path().strip_prefix(data)?.to_string_lossy());
        if wanted.contains_key(&key) {
            sources.push((key, entry.into_path()));
        }
    }
    Ok(sources)
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

/// Creates the file at `path` with the bytes `write` gives it. The bytes go to a temporary file
/// in the same folder, which takes the name only after they are written and synced. A failure
/// leaves no file at `path`, and an existing file is never replaced.
fn publish_new(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> Result<()> {
    let mut builder = tempfile::Builder::new();
    // The default mode of a temporary file is 0600; the sidecar is an ordinary file.
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    let mut temporary = builder
        .tempfile_in(folder_of(path))
        .wrap_err_with(|| format!("cannot create {}", path.display()))?;
    write(temporary.as_file_mut())
        .and_then(|()| temporary.as_file().sync_all())
        .wrap_err_with(|| format!("cannot write {}", path.display()))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .wrap_err_with(|| format!("cannot create {}", path.display()))?;
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
                // A skinned mesh's hash names its skeleton too.
                source_hash: format!("{}:{}", hash_bytes(b"recorded"), hash_bytes(b"rig")),
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
        assert_eq!(mesh_hash("abc:def"), "abc");
        assert_eq!(mesh_hash("abc"), "abc");
        // A loose file with the recorded bytes is found; it is reported, not invented.
        fs::create_dir_all(data.join("meshes")).unwrap();
        fs::write(data.join("meshes/a.nif"), b"recorded").unwrap();
        let report = run(&options).unwrap();
        assert_eq!((report.meshes, report.materials), (1, 0));
        assert_eq!(report.unreadable.len(), 1);
        let written = MaterialValuesFile::parse(&fs::read(&options.output).unwrap()).unwrap();
        assert!(written.meshes.is_empty());
    }

    /// A pack with one listed mesh whose recorded source bytes are `recorded`.
    fn one_mesh_pack(root: &Path) -> DeriveMaterialValuesOptions {
        let pack = root.join("clone");
        let data = root.join("data");
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
        DeriveMaterialValuesOptions {
            pack,
            registry: registry(root),
            data,
            output: root.join("material-values.json"),
        }
    }

    #[test]
    fn a_loose_file_is_read_by_its_spelling_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let options = one_mesh_pack(root.path());
        fs::create_dir_all(options.data.join("Meshes")).unwrap();
        fs::write(options.data.join("Meshes/A.NIF"), b"recorded").unwrap();
        fs::write(options.data.join("Meshes/B.NIF"), b"not listed").unwrap();
        let wanted = BTreeMap::from([(
            "meshes/a.nif".to_owned(),
            Wanted {
                output: "meshes/a.glb".to_owned(),
                source_hash: hash_bytes(b"recorded"),
            },
        )]);
        // The path keeps the spelling: the lower case key would not open on a case-sensitive
        // volume.
        assert_eq!(
            loose_sources(&options.data, &wanted).unwrap(),
            [(
                "meshes/a.nif".to_owned(),
                options.data.join("Meshes").join("A.NIF")
            )]
        );
        let report = run(&options).unwrap();
        assert!(report.missing_sources.is_empty(), "{report:?}");
    }

    #[test]
    fn an_output_inside_the_unregistered_input_pack_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let mut options = one_mesh_pack(root.path());
        fs::create_dir_all(options.pack.join("meshes")).unwrap();
        for output in ["material-values.json", "meshes/material-values.json"] {
            options.output = options.pack.join(output);
            let error = run(&options).unwrap_err();
            assert!(
                format!("{error:#}").contains("inside the pack"),
                "{error:#}"
            );
            assert!(!options.output.exists());
        }
    }

    #[test]
    fn a_copy_that_does_not_decompress_leaves_the_other_archives_searched() {
        use dummy_content::{Entry, bsa};
        let root = tempfile::tempdir().unwrap();
        let options = one_mesh_pack(root.path());
        let entries = [Entry::new("meshes/a.nif", b"recorded")];
        let mut broken = bsa::v105(&entries, bsa::Compression::Zlib).unwrap();
        // The payload is the uncompressed size and then the zlib header.
        let header = broken
            .windows(5)
            .position(|bytes| bytes[..4] == 8_u32.to_le_bytes() && bytes[4] == 0x78)
            .expect("a zlib payload");
        broken[header + 4] = 0;
        fs::write(options.data.join("a.bsa"), broken).unwrap();
        fs::write(
            options.data.join("b.bsa"),
            bsa::v105(&entries, bsa::Compression::None).unwrap(),
        )
        .unwrap();
        let report = run(&options).unwrap();
        assert!(report.missing_sources.is_empty(), "{report:?}");
        // The good copy reached the parser, which reports that these bytes are not a NIF.
        assert_eq!(report.unreadable.len(), 1);
        assert_eq!(report.failed_copies.len(), 1);
        assert!(report.failed_copies[0].0.contains("a.bsa: meshes/a.nif"));
    }

    #[test]
    fn a_failed_write_publishes_nothing_and_an_existing_file_stays() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("material-values.json");
        let error = publish_new(&path, |file| {
            file.write_all(b"half")?;
            Err(std::io::Error::other("the disk is full"))
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("disk is full"), "{error:#}");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        publish_new(&path, |file| file.write_all(b"first")).unwrap();
        assert!(publish_new(&path, |file| file.write_all(b"second")).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
