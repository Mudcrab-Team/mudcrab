//! Finds and reads the retail terrain LOD files of a Skyrim `Data` folder.
//!
//! Files come from the loose `meshes/terrain` and `textures/terrain` folders and from every `.bsa`
//! in `Data`. A loose file replaces what the archives hold. Archive entries are decoded individually from their open source.
//!
//! Nothing here fills a gap. A block whose colour map, normal map or mesh is missing, unreadable
//! or provided with different bytes by several archives is returned as a [`Skipped`] entry with its
//! reason, for the caller to count and report.

use super::{
    near_match::FileIndex,
    retail::name::{RetailLodKind, parse_lod_file_name},
};
use crate::archive::OpenArchive;
use color_eyre::{
    Result,
    eyre::{ContextCompat, WrapErr, ensure, eyre},
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
#[derive(Default)]
pub struct RetailTerrainFiles {
    pub blocks: BTreeMap<BlockId, BlockProviders>,
    pub archive_warnings: Vec<String>,
    /// Every terrain file listed from the archives and loose folders, for near-match reports.
    pub index: FileIndex,
    archives: BTreeMap<PathBuf, OpenArchive>,
}

impl RetailTerrainFiles {
    pub(crate) fn archives_used<'a>(
        &'a self,
        ids: impl Iterator<Item = &'a BlockId>,
    ) -> BTreeMap<&'a str, &'a Path> {
        let mut used = BTreeMap::new();
        for id in ids {
            let block = &self.blocks[id];
            for provider in block.mesh.iter().chain(&block.colour).chain(&block.normal) {
                if let Provider::Archive { name, path } = provider {
                    used.insert(name.as_str(), path.as_path());
                }
            }
        }
        used
    }

    pub(crate) fn archive_hash(&self, path: &Path) -> Result<String> {
        self.archives
            .get(path)
            .wrap_err("archive was not listed")?
            .sha256()
            .wrap_err_with(|| format!("cannot verify archive {}", path.display()))
    }

    pub(crate) fn check_archives(&self, ids: &[&BlockId]) -> Result<()> {
        for (name, path) in self.archives_used(ids.iter().copied()) {
            self.archives[path]
                .check_unchanged()
                .wrap_err_with(|| format!("archive {name} changed during terrain LOD import"))?;
        }
        Ok(())
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

/// Whether a canonical path is under a terrain folder, the only place retail LOD files live.
fn is_terrain_path(path: &str) -> bool {
    path.starts_with("meshes/terrain/") || path.starts_with("textures/terrain/")
}

/// What one canonical path is, if it is a terrain LOD mesh or map of a block.
pub(crate) fn classify(path: &str) -> Option<(BlockId, Kind)> {
    if !is_terrain_path(path) {
        return None;
    }
    let (kind, stem) = if let Some(stem) = path.strip_suffix(".btr") {
        (Kind::Mesh, stem)
    } else if let Some(stem) = path.strip_suffix("_n.dds") {
        (Kind::Normal, stem)
    } else {
        (Kind::Colour, path.strip_suffix(".dds")?)
    };
    let name = parse_lod_file_name(&format!("{stem}.btr"))?;
    if name.kind != RetailLodKind::Terrain {
        return None;
    }
    let (x, y) = name.cell?;
    let id = (name.world, name.level?, x, y);
    (path == canonical_path(&id, kind)).then_some((id, kind))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Mesh,
    Colour,
    Normal,
}

/// Lists the retail terrain LOD files in `data`: every `.bsa` there, then the loose folders.
pub fn list(data: &Path) -> Result<RetailTerrainFiles> {
    let mut files = RetailTerrainFiles::default();
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
    for (name, archive) in archive_names(archives)? {
        // Every terrain entry is listed, so a missing file can name its near matches.
        let opened = match OpenArchive::open(&archive, |path| is_terrain_path(&normal(path))) {
            Ok(opened) => opened,
            Err(error) => {
                files
                    .archive_warnings
                    .push(format!("cannot list archive {name}: {error:#}"));
                continue;
            }
        };
        files.index.note_archive();
        let mut has_block_file = false;
        for path in opened.paths() {
            let path = normal(path);
            files.index.add(&path);
            if let Some((id, kind)) = classify(&path) {
                has_block_file = true;
                providers(&mut files.blocks, id, kind).push(Provider::Archive {
                    name: name.clone(),
                    path: archive.clone(),
                });
            }
        }
        if has_block_file {
            files.archives.insert(archive, opened);
        }
    }
    // Loose files replace what the archives hold, so they become the only provider of a path.
    let mut loose: Vec<(String, PathBuf)> = Vec::new();
    for root in ["meshes", "textures"] {
        for top in children_ci(data, root)? {
            for terrain in children_ci(&top, "terrain")? {
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
                            files
                                .index
                                .add(&format!("{root}/terrain/{world_name}/{name}"));
                            loose.push((
                                format!("{root}/terrain/{world_name}/{name}"),
                                entry.path(),
                            ));
                        }
                    }
                }
            }
        }
    }
    loose.sort();
    for (path, location) in loose {
        if let Some((id, kind)) = classify(&path) {
            let slot = providers(&mut files.blocks, id, kind);
            slot.retain(|provider| matches!(provider, Provider::Loose(_)));
            slot.push(Provider::Loose(location));
        }
    }
    Ok(files)
}

fn archive_names(archives: Vec<PathBuf>) -> Result<BTreeMap<String, PathBuf>> {
    let mut named = BTreeMap::new();
    for path in archives {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .ok_or_else(|| eyre!("archive path {} has no file name", path.display()))?;
        ensure!(
            !named.contains_key(&name),
            "refused: more than one archive has the lowercase name {name}"
        );
        named.insert(name, path);
    }
    Ok(named)
}

fn providers(
    blocks: &mut BTreeMap<BlockId, BlockProviders>,
    id: BlockId,
    kind: Kind,
) -> &mut Vec<Provider> {
    let block = blocks.entry(id).or_default();
    match kind {
        Kind::Mesh => &mut block.mesh,
        Kind::Colour => &mut block.colour,
        Kind::Normal => &mut block.normal,
    }
}

fn children_ci(directory: &Path, name: &str) -> Result<Vec<PathBuf>> {
    let mut children = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name().eq_ignore_ascii_case(name) && entry.path().is_dir() {
            children.push(entry.path());
        }
    }
    children.sort();
    Ok(children)
}

pub(crate) fn canonical_path(id: &BlockId, kind: Kind) -> String {
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

impl RetailTerrainFiles {
    /// Reads the three files of `id`, or says why it cannot.
    pub fn read(&self, id: &BlockId) -> std::result::Result<BlockBytes, Skipped> {
        let block = self.blocks.get(id).ok_or(Skipped {
            class: "unlisted",
            detail: "the block is not in the game data".to_owned(),
        })?;
        let read = |kind: Kind, providers: &[Provider], missing: &'static str| {
            if providers.is_empty() {
                return Err(Skipped {
                    class: missing,
                    detail: format!(
                        "the game data has {}",
                        self.index
                            .describe_missing(&[&canonical_path(id, kind)], Some(&id.0))
                    ),
                });
            }
            let mut copies: Vec<(Vec<u8>, &Provider)> = Vec::new();
            for provider in providers {
                let relative = canonical_path(id, kind);
                let read = match provider {
                    Provider::Loose(path) => fs::read(path).wrap_err("cannot read loose file"),
                    Provider::Archive { path, .. } => {
                        self.archives[path].read(Path::new(&relative))
                    }
                };
                let bytes = read.map_err(|error| Skipped {
                    class: "unreadable",
                    detail: format!("{relative} from {}: {error:#}", provider.describe()),
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
    fn case_colliding_archive_names_are_refused() {
        // Pass both names directly so this also runs on case-insensitive filesystems.
        let error = archive_names(vec![
            PathBuf::from("Terrain.bsa"),
            PathBuf::from("TERRAIN.BSA"),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("lowercase name terrain.bsa"));
    }

    #[test]
    fn only_recoverable_archive_paths_supply_a_block() {
        use dummy_content::{Entry, bsa};
        let data = tempfile::tempdir().unwrap();
        let bytes = bsa::v105(
            &[
                Entry::new("Meshes/Terrain/W/W.4.0.0.btr", b"mesh"),
                Entry::new("Textures/Terrain/W/W.4.0.0.dds", b"colour"),
                Entry::new("Textures/Terrain/W/W.4.0.0_n.dds", b"normal"),
                Entry::new("Meshes/Terrain/Other/W.4.4.0.btr", b"wrong world folder"),
                Entry::new("Meshes/Terrain/W/W.04.4.0.btr", b"noncanonical level"),
            ],
            bsa::Compression::None,
        )
        .unwrap();
        fs::write(data.path().join("Terrain.bsa"), bytes).unwrap();
        fs::write(data.path().join("Unreadable.bsa"), b"invalid").unwrap();

        let files = list(data.path()).unwrap();
        let id = ("w".to_owned(), 4, 0, 0);
        assert_eq!(files.blocks.keys().collect::<Vec<_>>(), vec![&id]);
        let block = files.read(&id).ok().unwrap();
        assert_eq!(block.mesh, b"mesh");
        assert_eq!(block.colour, b"colour");
        assert_eq!(block.normal, b"normal");
        assert_eq!(files.archive_warnings.len(), 1);
        assert!(files.archive_warnings[0].contains("unreadable.bsa"));
    }

    #[test]
    fn a_damaged_archive_entry_does_not_hide_its_readable_neighbour() {
        use crate::lod::{
            generation::StagedGeneration,
            retail::btr::tests::land_block,
            retail_terrain_import::{LodWorld, stage_world},
        };
        use dummy_content::{Entry, bsa, dds, rng::Rng};
        use rusqlite::Connection;
        use shared::lod::LodOrigin;

        let data = tempfile::tempdir().unwrap();
        let mut bytes = bsa::v105(
            &[
                Entry::new("meshes/terrain/w/w.16.0.0.btr", b"damaged"),
                Entry::new("meshes/terrain/w/w.16.16.0.btr", &land_block()),
            ],
            bsa::Compression::Lz4,
        )
        .unwrap();
        let compressed_entry = lz4_flex::block::compress(b"damaged");
        let payload = bytes
            .windows(compressed_entry.len())
            .position(|bytes| bytes == compressed_entry)
            .unwrap();
        bytes[payload - 4..payload].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(data.path().join("Terrain.bsa"), bytes).unwrap();
        let textures = data.path().join("textures/terrain/w");
        fs::create_dir_all(&textures).unwrap();
        let map = dds::generate(
            &dds::Spec::new(dds::Format::X8R8G8B8, 8, 8),
            &mut Rng::new(5),
        )
        .unwrap();
        for x in [0, 16] {
            fs::write(textures.join(format!("w.16.{x}.0.dds")), &map).unwrap();
            fs::write(textures.join(format!("w.16.{x}.0_n.dds")), &map).unwrap();
        }
        let files = list(data.path()).unwrap();
        let ids: Vec<_> = files.blocks.keys().cloned().collect();
        let output = tempfile::tempdir().unwrap();
        let mut connection = Connection::open_in_memory().unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO worldspaces(id, editor_id, flags) VALUES (60, 'W', 0)",
                [],
            )
            .unwrap();
        let tx = connection.transaction().unwrap();
        let mut staged = StagedGeneration::new(output.path()).unwrap();
        let world = LodWorld {
            id: 60,
            editor_id: "W".to_owned(),
            file_name: "w".to_owned(),
            origin: LodOrigin::new(0, 0),
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let imported = stage_world(
            &tx,
            &mut staged,
            &world,
            &files,
            &ids,
            &pool,
            &BTreeMap::new(),
            || Ok(()),
        )
        .unwrap();

        assert_eq!(imported.report.skipped.len(), 1);
        let (damaged, reason) = &imported.report.skipped[0];
        assert_eq!(damaged, &("w".to_owned(), 16, 0, 0));
        assert_eq!(reason.class, "unreadable");
        assert_eq!(imported.report.compiled.get(&16), Some(&1));
        let anchor: i32 = tx
            .query_row("SELECT anchor_x FROM lod_chunks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(anchor, 1);
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
        let skipped = files.read(&("tamriel".to_owned(), 4, 0, 0)).err().unwrap();
        assert_eq!(skipped.class, "missing_colour_map");
    }

    #[test]
    fn a_missing_map_names_the_near_matches_the_game_data_does_hold() {
        use dummy_content::{Entry, bsa};
        let data = tempfile::tempdir().unwrap();
        let bytes = bsa::v105(
            &[
                Entry::new("Meshes/Terrain/W/W.4.0.0.btr", b"mesh"),
                Entry::new("Textures/Terrain/W/W.4.0.0_n.dds", b"normal"),
                // The colour map exists under another name. It is listed, not used.
                Entry::new("Textures/Terrain/W/W.4.0.0.colour.dds", b"colour"),
            ],
            bsa::Compression::None,
        )
        .unwrap();
        fs::write(data.path().join("Terrain.bsa"), bytes).unwrap();
        let files = list(data.path()).unwrap();
        let gap = files.read(&("w".to_owned(), 4, 0, 0)).err().unwrap();
        assert_eq!(gap.class, "missing_colour_map");
        assert!(
            gap.detail
                .contains("the game data has no textures/terrain/w/w.4.0.0.dds; near matches: "),
            "{}",
            gap.detail
        );
        assert!(
            gap.detail.contains("textures/terrain/w/w.4.0.0.colour.dds"),
            "{}",
            gap.detail
        );
        assert_eq!(files.index.archives(), 1);
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
        let ok = files.read(&("w".to_owned(), 4, 0, 0)).ok().unwrap();
        assert_eq!(
            (ok.mesh, ok.colour, ok.normal),
            (b"m".to_vec(), b"c".to_vec(), b"n".to_vec())
        );
        let gap = files.read(&("w".to_owned(), 4, 4, 0)).err().unwrap();
        assert_eq!(gap.class, "missing_normal_map");
        let unlisted = files.read(&("w".to_owned(), 8, 0, 0)).err().unwrap();
        assert_eq!(unlisted.class, "unlisted");
    }
}
