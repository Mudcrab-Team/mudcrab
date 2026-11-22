//! Finds and reads the retail terrain LOD files of a Skyrim `Data` folder.
//!
//! Files come from the loose `meshes/terrain` and `textures/terrain` folders and from every `.bsa`
//! in `Data`. A loose file replaces what the archives hold. Archive bytes are extracted into a
//! scratch folder the caller owns; nothing is written into `Data`.
//!
//! Nothing here fills a gap. A block whose colour map, normal map or mesh is missing, unreadable
//! or provided with different bytes by several archives is returned as a [`Skipped`] entry with its
//! reason, for the caller to count and report.

use super::retail::name::{RetailLodKind, parse_lod_file_name};
use crate::archive::ArchiveExtractor;
use color_eyre::{
    Result,
    eyre::{WrapErr, eyre},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// Where a file lives.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provider {
    Loose(PathBuf),
    /// Lowercase archive file name, and its path.
    Archive {
        name: String,
        path: PathBuf,
    },
}

impl Provider {
    pub fn describe(&self) -> String {
        match self {
            Provider::Loose(_) => "a loose file".to_owned(),
            Provider::Archive { name, .. } => format!("archive {name}"),
        }
    }
}

/// Identity of one retail block: world file name, level, south-west cell.
pub type BlockId = (String, u32, i32, i32);

/// The three files of one block and who provides them.
#[derive(Debug, Clone, Default)]
pub struct BlockProviders {
    pub mesh: Vec<Provider>,
    pub colour: Vec<Provider>,
    pub normal: Vec<Provider>,
}

/// Everything the game data offers for retail terrain LOD.
#[derive(Debug, Default)]
pub struct RetailTerrainFiles {
    pub blocks: BTreeMap<BlockId, BlockProviders>,
}

impl RetailTerrainFiles {
    /// Every archive that provides at least one file, by lowercase name.
    pub fn archives_used(&self) -> BTreeMap<String, PathBuf> {
        let mut used = BTreeMap::new();
        for block in self.blocks.values() {
            for provider in block.mesh.iter().chain(&block.colour).chain(&block.normal) {
                if let Provider::Archive { name, path } = provider {
                    used.insert(name.clone(), path.clone());
                }
            }
        }
        used
    }

    /// The worlds that have at least one block, by file name.
    pub fn worlds(&self) -> BTreeSet<String> {
        self.blocks
            .keys()
            .map(|(world, ..)| world.clone())
            .collect()
    }
}

/// Canonical form of an archive-relative path: lowercase, forward slashes.
fn normal(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// What one canonical path is, if it is a terrain LOD mesh or map of a block.
fn classify(path: &str) -> Option<(BlockId, Kind)> {
    let (kind, stem_path) = if path.starts_with("meshes/terrain/") && path.ends_with(".btr") {
        let name = parse_lod_file_name(path)?;
        if name.kind != RetailLodKind::Terrain {
            return None;
        }
        // Only the world's own folder: `meshes/terrain/<world>/<file>`.
        if path.matches('/').count() != 3 {
            return None;
        }
        let (level, (x, y)) = (name.level?, name.cell?);
        return Some(((name.world, level, x, y), Kind::Mesh));
    } else if path.starts_with("textures/terrain/") && path.ends_with("_n.dds") {
        (Kind::Normal, path.strip_suffix("_n.dds")?)
    } else if path.starts_with("textures/terrain/") && path.ends_with(".dds") {
        (Kind::Colour, path.strip_suffix(".dds")?)
    } else {
        return None;
    };
    if path.matches('/').count() != 3 {
        return None;
    }
    // `<world>.<level>.<x>.<y>`; the mesh parser already knows this grid encoding.
    let name = parse_lod_file_name(&format!("{stem_path}.btr"))?;
    Some(((name.world, name.level?, name.cell?.0, name.cell?.1), kind))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mesh,
    Colour,
    Normal,
}

/// Lists the retail terrain LOD files in `data`: every `.bsa` there, then the loose folders.
pub fn list(data: &Path) -> Result<RetailTerrainFiles> {
    let mut files = RetailTerrainFiles::default();
    let mut add = |path: &str, provider: Provider| {
        if let Some((id, kind)) = classify(path) {
            let block = files.blocks.entry(id).or_default();
            let slot = match kind {
                Kind::Mesh => &mut block.mesh,
                Kind::Colour => &mut block.colour,
                Kind::Normal => &mut block.normal,
            };
            slot.push(provider);
        }
    };
    let mut archives: Vec<PathBuf> = fs::read_dir(data)
        .wrap_err_with(|| format!("cannot read the game data folder {}", data.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("bsa"))
        })
        .collect();
    archives.sort();
    for archive in archives {
        let name = archive
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .ok_or_else(|| eyre!("archive path {} has no file name", archive.display()))?;
        for path in ArchiveExtractor::list_paths(&archive)
            .wrap_err_with(|| format!("cannot list archive {name}"))?
        {
            add(
                &normal(&path),
                Provider::Archive {
                    name: name.clone(),
                    path: archive.clone(),
                },
            );
        }
    }
    // Loose files replace what the archives hold, so they become the only provider of a path.
    let mut loose: Vec<(String, PathBuf)> = Vec::new();
    for root in ["meshes", "textures"] {
        let Some(top) = child_ci(data, root)? else {
            continue;
        };
        let Some(terrain) = child_ci(&top, "terrain")? else {
            continue;
        };
        for world in fs::read_dir(&terrain)? {
            let world = world?;
            if !world.path().is_dir() {
                continue;
            }
            let world_name = world.file_name().to_string_lossy().to_ascii_lowercase();
            for entry in fs::read_dir(world.path())? {
                let entry = entry?;
                if entry.path().is_file() {
                    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                    loose.push((format!("{root}/terrain/{world_name}/{name}"), entry.path()));
                }
            }
        }
    }
    for (path, location) in loose {
        if let Some((id, kind)) = classify(&path) {
            let block = files.blocks.entry(id).or_default();
            let slot = match kind {
                Kind::Mesh => &mut block.mesh,
                Kind::Colour => &mut block.colour,
                Kind::Normal => &mut block.normal,
            };
            *slot = vec![Provider::Loose(location)];
        }
    }
    Ok(files)
}

/// The child of `directory` named `name`, compared without case.
fn child_ci(directory: &Path, name: &str) -> Result<Option<PathBuf>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// Extracted bytes by canonical path, for the blocks of one world.
pub struct Extracted {
    roots: BTreeMap<PathBuf, PathBuf>,
}

/// Extracts every archive-provided file of `ids` into `scratch` (one folder per archive).
pub fn extract(files: &RetailTerrainFiles, ids: &[BlockId], scratch: &Path) -> Result<Extracted> {
    let mut per_archive: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    for id in ids {
        let Some(block) = files.blocks.get(id) else {
            continue;
        };
        for (kind, providers) in [
            (Kind::Mesh, &block.mesh),
            (Kind::Colour, &block.colour),
            (Kind::Normal, &block.normal),
        ] {
            for provider in providers {
                if let Provider::Archive { path, .. } = provider {
                    per_archive
                        .entry(path.clone())
                        .or_default()
                        .insert(PathBuf::from(canonical_path(id, kind)));
                }
            }
        }
    }
    let mut roots = BTreeMap::new();
    for (index, (archive, wanted)) in per_archive.into_iter().enumerate() {
        let root = scratch.join(format!("archive-{index}"));
        ArchiveExtractor::extract_paths(&archive, &root, &wanted)
            .wrap_err_with(|| format!("failed to read from {}", archive.display()))?;
        roots.insert(archive, root);
    }
    Ok(Extracted { roots })
}

fn canonical_path(id: &BlockId, kind: Kind) -> String {
    let (world, level, x, y) = id;
    match kind {
        Kind::Mesh => format!("meshes/terrain/{world}/{world}.{level}.{x}.{y}.btr"),
        Kind::Colour => format!("textures/terrain/{world}/{world}.{level}.{x}.{y}.dds"),
        Kind::Normal => format!("textures/terrain/{world}/{world}.{level}.{x}.{y}_n.dds"),
    }
}

/// Why a block was not imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// A short stable class, for counting.
    pub class: &'static str,
    pub detail: String,
}

/// The three files of a block as bytes.
pub struct BlockBytes {
    pub mesh: Vec<u8>,
    pub colour: Vec<u8>,
    pub normal: Vec<u8>,
}

impl Extracted {
    /// Reads the three files of `id`, or says why it cannot.
    pub fn read(
        &self,
        files: &RetailTerrainFiles,
        id: &BlockId,
    ) -> std::result::Result<BlockBytes, Skipped> {
        let block = files.blocks.get(id).ok_or(Skipped {
            class: "unlisted",
            detail: "the block is not in the game data".to_owned(),
        })?;
        let read = |kind: Kind, providers: &[Provider], missing: &'static str| {
            if providers.is_empty() {
                return Err(Skipped {
                    class: missing,
                    detail: format!("the game data has no {}", canonical_path(id, kind)),
                });
            }
            let mut copies: Vec<(Vec<u8>, &Provider)> = Vec::new();
            for provider in providers {
                let location = match provider {
                    Provider::Loose(path) => path.clone(),
                    Provider::Archive { path, .. } => self
                        .roots
                        .get(path)
                        .map(|root| root.join(canonical_path(id, kind)))
                        .ok_or(Skipped {
                            class: "unreadable",
                            detail: format!("{} was not extracted", path.display()),
                        })?,
                };
                let bytes = fs::read(&location).map_err(|error| Skipped {
                    class: "unreadable",
                    detail: format!("{}: {error}", canonical_path(id, kind)),
                })?;
                copies.push((bytes, provider));
            }
            if copies.iter().any(|(bytes, _)| *bytes != copies[0].0) {
                let names: Vec<String> = copies.iter().map(|(_, p)| p.describe()).collect();
                return Err(Skipped {
                    class: "ambiguous",
                    detail: format!(
                        "{} differs between {}",
                        canonical_path(id, kind),
                        names.join(", ")
                    ),
                });
            }
            Ok(copies.swap_remove(0).0)
        };
        Ok(BlockBytes {
            mesh: read(Kind::Mesh, &block.mesh, "missing_mesh")?,
            colour: read(Kind::Colour, &block.colour, "missing_colour_map")?,
            normal: read(Kind::Normal, &block.normal, "missing_normal_map")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_block_files_in_a_world_folder_are_classified() {
        let id = ("tamriel".to_owned(), 16, -32, 0);
        assert_eq!(
            classify("meshes/terrain/tamriel/tamriel.16.-32.0.btr"),
            Some((id.clone(), Kind::Mesh))
        );
        assert_eq!(
            classify("textures/terrain/tamriel/tamriel.16.-32.0.dds"),
            Some((id.clone(), Kind::Colour))
        );
        assert_eq!(
            classify("textures/terrain/tamriel/tamriel.16.-32.0_n.dds"),
            Some((id, Kind::Normal))
        );
        for other in [
            "meshes/terrain/tamriel/objects/tamriel.16.-32.0.btr",
            "meshes/terrain/tamriel/tamriel.16.-32.0.bto",
            "textures/terrain/tamriel/objects/tamriel.objects.dds",
            "textures/terrain/tamriel/trees/tamriel.treelod.dds",
            "textures/terrain/tamriel/tamriel.noblock.dds",
            "meshes/other/tamriel.16.-32.0.btr",
        ] {
            assert_eq!(classify(other), None, "{other}");
        }
    }

    #[test]
    fn a_loose_file_replaces_the_archive_providers() {
        let data = tempfile::tempdir().unwrap();
        let folder = data.path().join("Meshes/Terrain/Tamriel");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("Tamriel.4.0.0.btr"), b"loose").unwrap();
        let files = list(data.path()).unwrap();
        let block = &files.blocks[&("tamriel".to_owned(), 4, 0, 0)];
        assert_eq!(block.mesh.len(), 1);
        assert!(matches!(block.mesh[0], Provider::Loose(_)));
        assert!(block.colour.is_empty() && block.normal.is_empty());
        // Reading it: the colour map is missing and the block is reported, not filled.
        let extracted = extract(&files, &[("tamriel".to_owned(), 4, 0, 0)], data.path()).unwrap();
        let skipped = extracted
            .read(&files, &("tamriel".to_owned(), 4, 0, 0))
            .err()
            .unwrap();
        assert_eq!(skipped.class, "missing_colour_map");
    }

    #[test]
    fn loose_files_are_read_and_each_gap_has_its_own_class() {
        let data = tempfile::tempdir().unwrap();
        let meshes = data.path().join("meshes/terrain/w");
        let textures = data.path().join("textures/terrain/w");
        fs::create_dir_all(&meshes).unwrap();
        fs::create_dir_all(&textures).unwrap();
        fs::write(meshes.join("w.4.0.0.btr"), b"m").unwrap();
        fs::write(textures.join("w.4.0.0.dds"), b"c").unwrap();
        fs::write(textures.join("w.4.0.0_n.dds"), b"n").unwrap();
        fs::write(meshes.join("w.4.4.0.btr"), b"m").unwrap();
        fs::write(textures.join("w.4.4.0.dds"), b"c").unwrap();
        let files = list(data.path()).unwrap();
        assert_eq!(files.worlds(), BTreeSet::from(["w".to_owned()]));
        let extracted = extract(&files, &[], data.path()).unwrap();
        let ok = extracted
            .read(&files, &("w".to_owned(), 4, 0, 0))
            .ok()
            .unwrap();
        assert_eq!(
            (ok.mesh, ok.colour, ok.normal),
            (b"m".to_vec(), b"c".to_vec(), b"n".to_vec())
        );
        let gap = extracted
            .read(&files, &("w".to_owned(), 4, 4, 0))
            .err()
            .unwrap();
        assert_eq!(gap.class, "missing_normal_map");
        let unlisted = extracted
            .read(&files, &("w".to_owned(), 8, 0, 0))
            .err()
            .unwrap();
        assert_eq!(unlisted.class, "unlisted");
    }
}
