//! Reading retail object LOD from the game data and compiling it for a pack. Shared by
//! `converter derive-object-lod` and the conversion pipeline (normal run and metadata rebuild).
//!
//! The level-16 `.bto` blocks and the object atlas are read from the Skyrim `Data` folder, loose
//! files first and then `.bsa` archives, through the converter's archive reader. Nothing here
//! touches the ingestion cache or its file selection, so an existing pack's NIF, texture and
//! script outputs stay valid.

use crate::{
    archive::ArchiveExtractor,
    cache::hash_bytes,
    config::PipelineConfig,
    lod::{
        near_match::FileIndex,
        objects::{
            MissingObjectUv, ObjectChunk, chunk_key_for_block, compile_object_chunk_shared_atlas,
            object_chunk_input_hash, publish_object_atlas, publish_object_chunk,
        },
        reconciliation::LodReconciliation,
        retail::{
            atlas::read_dds_info,
            bto::{ObjectLodFile, read_object_lod},
            name::{RetailLodKind, parse_lod_file_name},
        },
    },
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure},
};
use log::warn;
use rusqlite::Connection;
use shared::lod::{LodOrigin, object_chunk_payload_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// The block levels the retail files ship (no level 32) and this compiler handles.
pub(crate) const LEVELS: [u32; 3] = [16, 8, 4];

/// A block the run did not compile, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedBlock {
    /// Block level (16, 8 or 4).
    pub level: u32,
    pub cell: (i32, i32),
    /// The class of the reason, the key of the counts.
    pub class: &'static str,
    pub detail: String,
}

/// A world with object chunks and no normal atlas.
pub const MISSING_NORMAL_ATLAS: &str = "missing_normal_atlas";

/// A world skipped because it has object blocks and no diffuse atlas.
pub const MISSING_ATLAS: &str = "missing_atlas";

/// The game data has level 16, 8 or 4 object blocks for a world, but no object atlas file under
/// either name that retail uses (see [`atlas_paths`]).
///
/// Only an absent file is this error. An atlas that exists but cannot be read or is not a valid
/// DDS is a different error and still fails the run. The full pipeline skips a world with this
/// error. `converter derive-object-lod` names one world on purpose, so it refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingObjectAtlas {
    /// The atlas paths that were searched, lowercase with forward slashes.
    pub paths: Vec<String>,
    /// The absent paths and the near matches in the game data, from [`FileIndex`].
    pub detail: String,
    /// The level and south-west cell of every source block the listing found. A later block
    /// outside this set is not covered by this world-level skip.
    pub blocks: BTreeSet<(u32, i32, i32)>,
}

impl std::fmt::Display for MissingObjectAtlas {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "refused: the game data has {}, the object atlas",
            self.detail
        )
    }
}

impl std::error::Error for MissingObjectAtlas {}

/// A world the pipeline did not compile, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedWorld {
    pub editor_id: String,
    /// The class of the reason, for example [`MISSING_ATLAS`].
    pub class: &'static str,
    /// What is absent and which near matches exist.
    pub detail: String,
}

/// The two game-data paths a world's object atlas can have, in the order they are tried.
///
/// Retail proves both forms. The 12 worlds of the base game, Dawnguard and Dragonborn that ship
/// object LOD use `<world>.objects.dds`. The Creation Club worlds `ccbgssse067deadlandsworld` and
/// `cckrtsse001qnworld` use `<world>objectslod.dds`. In every one of these 14 worlds the archives
/// hold exactly one of the two, and the texture set inside the world's `.bto` blocks names that
/// same file. The paths are lowercase with forward slashes.
pub(crate) fn atlas_paths(world_file: &str, normal: bool) -> [String; 2] {
    // STAND-IN(#940): live resolution of the joined name is unverified.
    let suffix = if normal { "_n" } else { "" };
    [
        format!("textures/terrain/{world_file}/objects/{world_file}.objects{suffix}.dds"),
        format!("textures/terrain/{world_file}/objects/{world_file}objectslod{suffix}.dds"),
    ]
}

/// One `.bto` block read from the game data, before compilation.
pub struct BlockCandidate {
    /// Block level, from the file name (16, 8 or 4).
    pub level: u32,
    /// South-west cell, from the file name.
    pub cell: (i32, i32),
    /// SHA-256 of the `.bto` bytes.
    pub sha256: String,
    /// The parsed block, or why it could not be read.
    pub parsed: std::result::Result<ObjectLodFile, String>,
}

/// The worldspace the chunks belong to.
#[derive(Debug, Clone, Copy)]
pub struct WorldFacts {
    pub id: u32,
    pub origin: LodOrigin,
}

/// The `.bto` blocks and the object atlas read from the game folder.
pub(crate) struct Retail {
    pub blocks: Vec<BlockCandidate>,
    pub atlas_dds: Vec<u8>,
    pub normal_atlas_dds: Option<Vec<u8>>,
    /// Where each input came from, for the receipt.
    pub sources: Vec<serde_json::Value>,
    /// Blocks that could not be read at all.
    pub skipped: Vec<SkippedBlock>,
}

/// What reading the retail object LOD of one world found.
pub(crate) enum RetailRead {
    Found(Retail),
    /// The game data has no level 16, 8 or 4 block of this world. Normal for most worldspaces.
    /// Holds the near-match text for a caller that refuses.
    NoBlocks(String),
}

/// Where one game-data file can be read from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Provider {
    Loose(PathBuf),
    Archive { name: String, path: PathBuf },
}

/// Normalises an archive entry path: lowercase, forward slashes.
fn normal(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// Whether an archive entry (by its original name) is under a terrain folder, the only place retail
/// object LOD files live. Filtering by name first keeps an archive whose unrelated entries collide
/// searchable.
fn is_terrain_entry_name(name: &str) -> bool {
    let name = name.replace('\\', "/").to_ascii_lowercase();
    name.starts_with("meshes/terrain/") || name.starts_with("textures/terrain/")
}

/// Finds the files `wanted` accepts in loose files and archives of `data`. Loose files win over
/// archives; otherwise every archive that has the file is listed. The index holds every terrain
/// file that was listed, wanted or not, for near-match reports.
pub(crate) fn providers(
    data: &Path,
    wanted: &dyn Fn(&str) -> bool,
) -> Result<(BTreeMap<String, Vec<Provider>>, FileIndex)> {
    let mut found: BTreeMap<String, Vec<Provider>> = BTreeMap::new();
    let mut index = FileIndex::default();
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
            .ok_or_else(|| {
                color_eyre::eyre::eyre!("archive path {} has no file name", archive.display())
            })?;
        let paths = ArchiveExtractor::list_paths_with_filter(&archive, is_terrain_entry_name)
            .wrap_err_with(|| format!("cannot list archive {name}"))?;
        index.note_archive();
        for path in paths {
            let key = normal(&path);
            index.add(&key);
            if wanted(&key) {
                found.entry(key).or_default().push(Provider::Archive {
                    name: name.clone(),
                    path: archive.clone(),
                });
            }
        }
    }
    // Loose files: walk the two terrain folders (found case-insensitively).
    for root in ["meshes", "textures"] {
        let mut stack = vec![(PathBuf::from(root), child_ci(data, root)?)];
        while let Some((relative, Some(directory))) = stack.pop() {
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                let relative = relative.join(&name);
                let path = entry.path();
                if path.is_dir() {
                    // Only terrain trees can hold object LOD files.
                    if relative.components().count() == 2 && name != "terrain" {
                        continue;
                    }
                    stack.push((relative, Some(path)));
                } else {
                    let key = normal(&relative);
                    index.add(&key);
                    if !wanted(&key) {
                        continue;
                    }
                    // A loose file replaces whatever the archives hold.
                    found.insert(key, vec![Provider::Loose(path)]);
                }
            }
        }
    }
    Ok((found, index))
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

/// The bytes of one file and every provider that holds them, or why the providers disagree.
type CopyRead = std::result::Result<(Vec<u8>, Vec<Provider>), String>;

/// Reads the bytes of `relative` from its providers. `Ok(Err(..))` is a conflict: several
/// providers hold different bytes, which this command does not resolve by guessing.
fn read_provider_bytes(
    relative: &str,
    options: &[Provider],
    extracted: &BTreeMap<PathBuf, PathBuf>,
) -> Result<CopyRead> {
    let mut copies: Vec<(Vec<u8>, Provider)> = Vec::new();
    for provider in options {
        let bytes = match provider {
            Provider::Loose(path) => fs::read(path)?,
            Provider::Archive { path, .. } => {
                let root = extracted.get(path).ok_or_else(|| {
                    color_eyre::eyre::eyre!("archive {} was not extracted", path.display())
                })?;
                fs::read(root.join(relative))
                    .wrap_err_with(|| format!("{relative} was not extracted from its archive"))?
            }
        };
        copies.push((bytes, provider.clone()));
    }
    let Some((first, _)) = copies.first() else {
        bail!("{relative} has no provider");
    };
    if copies.iter().any(|(bytes, _)| bytes != first) {
        let names: Vec<String> = copies
            .iter()
            .map(|(_, provider)| describe(provider))
            .collect();
        return Ok(Err(format!(
            "{relative} differs between {}",
            names.join(", ")
        )));
    }
    let bytes = copies[0].0.clone();
    Ok(Ok((
        bytes,
        copies.into_iter().map(|(_, provider)| provider).collect(),
    )))
}

fn describe(provider: &Provider) -> String {
    match provider {
        Provider::Loose(_) => "a loose file".to_owned(),
        Provider::Archive { name, .. } => format!("archive {name}"),
    }
}

/// Reads the level-16 blocks and the object atlas of `world_file` from the game data.
/// `verify_archive` is called for every archive that provides a wanted file, before it is read; it
/// returns a record for the receipt, or refuses the archive.
///
/// Returns [`RetailRead::NoBlocks`] when the game data has no level-16 block of this world at all,
/// which is normal for most worldspaces. The atlas is `<world>.objects.dds` or `<world>objectslod.dds`
/// (see [`atlas_paths`]). A world with blocks but neither atlas file fails with
/// [`MissingObjectAtlas`], so a caller can tell an absent file from a damaged one. A world with an
/// atlas that cannot be read or is not a valid DDS fails with another error.
pub(crate) fn read_retail(
    data: &Path,
    world_file: &str,
    scratch: &Path,
    verify_archive: &mut dyn FnMut(&str, &Path) -> Result<serde_json::Value>,
) -> Result<RetailRead> {
    let atlas_names = atlas_paths(world_file, false);
    let normal_names = atlas_paths(world_file, true);
    let is_block = |path: &str| {
        parse_lod_file_name(path).is_some_and(|name| {
            name.kind == RetailLodKind::Object
                && name.world == world_file
                && name.level.is_some_and(|level| LEVELS.contains(&level))
        }) && path.starts_with("meshes/")
    };
    let wanted = |path: &str| {
        atlas_names
            .iter()
            .chain(&normal_names)
            .any(|name| name == path)
            || is_block(path)
    };
    let (found, index) = providers(data, &wanted)?;
    if !found.keys().any(|path| is_block(path)) {
        // The folder, not a file, is what is absent. A block of this world has this shape.
        let block = format!("meshes/terrain/{world_file}/objects/{world_file}.16.<x>.<y>.bto");
        return Ok(RetailRead::NoBlocks(
            index.describe_missing(&[&block], Some(world_file)),
        ));
    }
    // The atlas has one of two names. Both at once is not in the retail data and is not guessed.
    let present: Vec<&String> = atlas_names
        .iter()
        .filter(|name| found.contains_key(*name))
        .collect();
    let atlas_path = match present.as_slice() {
        [only] => (*only).clone(),
        [] => {
            // An absent atlas is known from the file listing. Fail before any archive is read.
            let names: Vec<&str> = atlas_names.iter().map(String::as_str).collect();
            let blocks = found
                .keys()
                .filter(|path| is_block(path))
                .filter_map(|path| {
                    let name = parse_lod_file_name(path)?;
                    let (level, (x, y)) = (name.level?, name.cell?);
                    Some((level, x, y))
                })
                .collect();
            return Err(MissingObjectAtlas {
                paths: atlas_names.to_vec(),
                detail: index.describe_missing(&names, Some(world_file)),
                blocks,
            }
            .into());
        }
        _ => bail!(
            "refused: the object atlas is ambiguous: the game data has both {} and {}",
            atlas_names[0],
            atlas_names[1]
        ),
    };

    // Every archive that provides a wanted file is checked by the caller before it is read.
    let used: BTreeSet<(String, PathBuf)> = found
        .values()
        .flatten()
        .filter_map(|provider| match provider {
            Provider::Archive { name, path } => Some((name.clone(), path.clone())),
            Provider::Loose(_) => None,
        })
        .collect();
    let mut sources = Vec::new();
    let mut extracted: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for (name, path) in &used {
        sources.push(verify_archive(name, path)?);
        let root = scratch.join(format!("archive-{}", extracted.len()));
        let wanted_here: BTreeSet<String> = found
            .iter()
            .filter(|(_, options)| {
                options.iter().any(
                    |provider| matches!(provider, Provider::Archive { path: p, .. } if p == path),
                )
            })
            .map(|(key, _)| key.clone())
            .collect();
        ArchiveExtractor::extract_matching_with_name_filter(
            path,
            &root,
            is_terrain_entry_name,
            |entry| wanted_here.contains(&normal(entry)),
        )
        .wrap_err_with(|| format!("failed to read from archive {name}"))?;
        extracted.insert(path.clone(), root);
    }

    let mut read_atlas = |atlas_path: &str| -> Result<Vec<u8>> {
        let atlas_options = found
            .get(atlas_path)
            .ok_or_else(|| color_eyre::eyre::eyre!("{atlas_path} was listed and then lost"))?;
        let (atlas_dds, atlas_providers) =
            match read_provider_bytes(atlas_path, atlas_options, &extracted)? {
                Ok(read) => read,
                Err(conflict) => bail!("refused: the object atlas is ambiguous: {conflict}"),
            };
        let info = read_dds_info(&atlas_dds).map_err(|error| {
            color_eyre::eyre::eyre!("refused: the object atlas is not a valid DDS: {error}")
        })?;
        let atlas_length =
            u64::try_from(atlas_dds.len()).wrap_err("the object atlas is too large")?;
        let header_length =
            u64::try_from(info.data_offset).wrap_err("the object atlas header is too large")?;
        ensure!(
            info.expected_payload_bytes()
                .and_then(|payload| header_length.checked_add(payload))
                == Some(atlas_length),
            "refused: the object atlas DDS has the wrong length for its header"
        );
        sources.push(serde_json::json!({
            "file": atlas_path,
            "providers": atlas_providers.iter().map(describe).collect::<Vec<_>>(),
            "sha256": hash_bytes(&atlas_dds),
        }));

        Ok(atlas_dds)
    };
    let atlas_dds = read_atlas(&atlas_path)?;
    let mut normal_paths = normal_names.iter().filter(|path| found.contains_key(*path));
    let normal_atlas_dds = normal_paths
        .next()
        .map(|path| read_atlas(path))
        .transpose()?;
    ensure!(
        normal_paths.next().is_none(),
        "refused: the object normal atlas is ambiguous"
    );

    // The blocks.
    let mut blocks = Vec::new();
    let mut skipped = Vec::new();
    let mut loose = 0usize;
    for (relative, options) in found.iter().filter(|(path, _)| is_block(path)) {
        let Some(name) = parse_lod_file_name(relative) else {
            continue;
        };
        let (Some(cell), Some(level)) = (name.cell, name.level) else {
            continue;
        };
        let read = match read_provider_bytes(relative, options, &extracted) {
            Ok(read) => read,
            Err(error) => {
                skipped.push(SkippedBlock {
                    level,
                    cell,
                    class: "block cannot be read",
                    detail: format!("{relative}: {error:#}"),
                });
                continue;
            }
        };
        match read {
            Err(conflict) => skipped.push(SkippedBlock {
                level,
                cell,
                class: "several providers with different bytes",
                detail: conflict,
            }),
            Ok((bytes, used_providers)) => {
                loose += used_providers
                    .iter()
                    .filter(|provider| matches!(provider, Provider::Loose(_)))
                    .count();
                let parsed = read_object_lod(&bytes, Path::new(relative))
                    .map_err(|error| format!("{error:#}"));
                blocks.push(BlockCandidate {
                    level,
                    cell,
                    sha256: hash_bytes(&bytes),
                    parsed,
                });
            }
        }
    }
    sources.push(serde_json::json!({
        "blocks": blocks.len() + skipped.len(),
        "loose_files_used": loose,
    }));
    Ok(RetailRead::Found(Retail {
        blocks,
        atlas_dds,
        normal_atlas_dds,
        sources,
        skipped,
    }))
}

/// Chunks (each with its input proof) and skipped blocks of one world.
pub(crate) type Compiled = (Vec<(ObjectChunk, String)>, Vec<SkippedBlock>);

/// Compiles the candidates of one world into object chunks that reference the worldspace's shared
/// atlas file. A block that
/// cannot be read, is off the LOD grid, or that the compiler refuses is skipped with its class and
/// reason. The caller decides whether skipped blocks or an empty result are errors.
pub(crate) fn compile_candidates(
    world: WorldFacts,
    candidates: Vec<BlockCandidate>,
    atlas_sha256: &str,
) -> Result<Compiled> {
    let mut skipped = Vec::new();
    let mut chunks: Vec<(ObjectChunk, String)> = Vec::new();
    for candidate in candidates {
        let block = match &candidate.parsed {
            Ok(block) => block,
            Err(reason) => {
                skipped.push(SkippedBlock {
                    level: candidate.level,
                    cell: candidate.cell,
                    class: "block cannot be read",
                    detail: reason.clone(),
                });
                continue;
            }
        };
        let key = match chunk_key_for_block(
            world.id,
            world.origin,
            candidate.level,
            candidate.cell.0,
            candidate.cell.1,
        ) {
            Ok(key) => key,
            Err(error) => {
                skipped.push(SkippedBlock {
                    level: candidate.level,
                    cell: candidate.cell,
                    class: "block is not on the LOD grid",
                    detail: format!("{error:#}"),
                });
                continue;
            }
        };
        match compile_object_chunk_shared_atlas(key, world.origin, block) {
            Ok(chunk) => {
                let input =
                    object_chunk_input_hash(key, world.origin, &candidate.sha256, atlas_sha256)?;
                chunks.push((chunk, input));
            }
            Err(error) => skipped.push(SkippedBlock {
                level: candidate.level,
                cell: candidate.cell,
                class: if error.is::<MissingObjectUv>() {
                    "block lacks required vertex data"
                } else {
                    "block refused by the compiler"
                },
                detail: format!("{error:#}"),
            }),
        }
    }
    Ok((chunks, skipped))
}

/// Counts of the chunks of one block level.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LevelCounts {
    pub chunks: usize,
    pub triangles: usize,
    pub vertices: usize,
    /// Cell-tagged pieces (level-4 segments).
    pub cell_pieces: usize,
    /// Chunks whose source `.bto` carries large-reference data.
    pub large_ref_chunks: usize,
    pub bytes: u64,
}

/// Counts per block level of a set of compiled chunks.
pub fn level_counts(chunks: &[(ObjectChunk, String)]) -> BTreeMap<u32, LevelCounts> {
    let mut counts: BTreeMap<u32, LevelCounts> = BTreeMap::new();
    for (chunk, _) in chunks {
        let level = u32::try_from(chunk.key.tier.side_cells()).unwrap_or(0);
        let entry = counts.entry(level).or_default();
        entry.chunks += 1;
        entry.triangles += chunk.triangles;
        entry.vertices += chunk.vertices;
        entry.cell_pieces += chunk.cell_pieces;
        entry.large_ref_chunks += usize::from(chunk.large_ref_blocks > 0);
        entry.bytes += chunk.glb.len() as u64;
    }
    counts
}

/// What the pipeline stage added to the staged pack.
#[derive(Debug, Default)]
pub(crate) struct ObjectLodStage {
    /// Payload path and input proof of each published chunk.
    pub chunks: Vec<(String, String)>,
    /// Payload path and SHA-256 of each shared atlas file (one per worldspace with chunks).
    pub atlases: Vec<(String, String)>,
    pub normal_atlases: Vec<(String, String)>,
    pub triangles: usize,
    /// Counts per block level.
    pub levels: BTreeMap<u32, LevelCounts>,
    /// Notices that do not make the run incomplete (for example, worlds without object LOD).
    pub notices: Vec<String>,
    /// One line per skipped block or refused world, counted in the LOD warnings.
    pub warnings: Vec<String>,
    /// The worlds that have object blocks but were skipped, each with its reason class.
    pub skipped_worlds: Vec<SkippedWorld>,
    /// Structured skip counts for `converter reconcile`.
    pub reconciliation: LodReconciliation,
}

/// Compiles the retail object LOD of every world that has terrain LOD and publishes it into the
/// staged pack: the chunk files, the `lod_object_chunks` rows and the shared atlas encode. The
/// caller writes the manifest fields from the result.
///
/// Used by the normal conversion and the metadata rebuild alike, so both read the same files in the
/// same way. It runs after the terrain stage, because object chunks hang off the worldspace's LOD
/// origin and the pack's terrain LOD. It reads the Skyrim `Data` folder directly (loose files, then
/// archives) and does not use the ingestion selection, so it changes no existing output.
pub(crate) fn build_object_lod(
    config: &PipelineConfig,
    staging: &Path,
    connection: &Connection,
) -> Result<ObjectLodStage> {
    let mut stage = ObjectLodStage::default();
    if config.mo2.is_some() {
        stage.warnings.push(
            "object LOD was not built: the .bto blocks and the object atlas are read from the Data folder, which an MO2 selection does not provide"
                .to_owned(),
        );
        return Ok(stage);
    }
    let worlds: Vec<(u32, Option<String>, i32, i32)> = connection
        .prepare(
            "SELECT w.id, w.editor_id, w.lod_origin_x, w.lod_origin_y FROM worldspaces w \
             WHERE w.lod_origin_x IS NOT NULL AND w.lod_origin_y IS NOT NULL \
             AND EXISTS(SELECT 1 FROM lod_chunks c WHERE c.worldspace_id = w.id) ORDER BY w.id",
        )?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    let scratch =
        tempfile::tempdir().wrap_err("failed to create a scratch folder for object LOD")?;
    let mut without = 0usize;
    let mut verify = |name: &str, path: &Path| -> Result<serde_json::Value> {
        Ok(serde_json::json!({ "archive": name, "path": path }))
    };
    for (id, editor_id, origin_x, origin_y) in worlds {
        let Some(editor_id) = editor_id else {
            stage.warnings.push(format!(
                "object LOD skipped for worldspace {id:08X}: it has no editor id"
            ));
            continue;
        };
        let file_name = editor_id.to_ascii_lowercase();
        if file_name.is_empty() || file_name.contains(['/', '\\', '.']) {
            stage.warnings.push(format!(
                "object LOD skipped for worldspace {id:08X}: `{editor_id}` is not a usable file name"
            ));
            continue;
        }
        stage.reconciliation.record_object_world(&file_name);
        let world_scratch = scratch.path().join(format!("world-{id:08x}"));
        let retail = match read_retail(&config.data_dir, &file_name, &world_scratch, &mut verify) {
            Ok(RetailRead::Found(retail)) => retail,
            Ok(RetailRead::NoBlocks(_)) => {
                without += 1;
                continue;
            }
            // Only an absent atlas skips the world. A corrupt or unreadable atlas still fails.
            Err(error) => match error.downcast::<MissingObjectAtlas>() {
                Ok(missing) => {
                    let message = format!(
                        "object LOD {editor_id}: world skipped: {MISSING_ATLAS}: the game data has {}",
                        missing.detail
                    );
                    warn!("{message}");
                    stage.warnings.push(message);
                    stage.reconciliation.record_object_world_skip(
                        &file_name,
                        MISSING_ATLAS,
                        missing.blocks.clone(),
                    );
                    stage.skipped_worlds.push(SkippedWorld {
                        editor_id,
                        class: MISSING_ATLAS,
                        detail: missing.detail,
                    });
                    continue;
                }
                Err(error) => {
                    return Err(error).wrap_err_with(|| {
                        format!("object LOD of worldspace {editor_id} could not be read")
                    });
                }
            },
        };
        let world = WorldFacts {
            id,
            origin: LodOrigin::new(origin_x, origin_y),
        };
        if retail.normal_atlas_dds.is_none() {
            stage
                .warnings
                .push(format!("object LOD {editor_id}: {MISSING_NORMAL_ATLAS}"));
        }
        let atlas_sha256 = hash_bytes(&retail.atlas_dds);
        let atlas = crate::texture::TextureConverter::convert_with_options(
            &retail.atlas_dds,
            crate::texture::TextureEncoding::ColorSrgb,
            config.texture_fallback_quality,
            config.texture_uastc_level,
            config.texture_zstd_level,
        )
        .wrap_err_with(|| format!("the object atlas of {editor_id} could not be converted"))?;
        crate::texture::inspect_ktx2(&atlas, crate::texture::TextureEncoding::ColorSrgb)
            .wrap_err_with(|| format!("the object atlas of {editor_id} is not a valid KTX2"))?;
        let (chunks, skipped) = compile_candidates(world, retail.blocks, &atlas_sha256)?;
        for block in retail.skipped.iter().chain(&skipped) {
            stage.reconciliation.record_object_skip(
                &file_name,
                block.level,
                block.class,
                block.cell,
            );
            stage.warnings.push(format!(
                "object LOD {editor_id}: skipped block ({}, {}): {}: {}",
                block.cell.0, block.cell.1, block.class, block.detail
            ));
        }
        if chunks.is_empty() {
            stage.warnings.push(format!(
                "object LOD {editor_id}: no block compiled, so the world has no object chunks"
            ));
            continue;
        }
        for (level, counts) in level_counts(&chunks) {
            if level != 4 && counts.large_ref_chunks > 0 {
                stage.warnings.push(format!(
                    "object LOD {editor_id}: {} level-{level} blocks carry large-reference data outside the segmented tier",
                    counts.large_ref_chunks
                ));
            }
            let total = stage.levels.entry(level).or_default();
            total.chunks += counts.chunks;
            total.triangles += counts.triangles;
            total.vertices += counts.vertices;
            total.cell_pieces += counts.cell_pieces;
            total.large_ref_chunks += counts.large_ref_chunks;
            total.bytes += counts.bytes;
        }
        // The atlas is written once for the world; every chunk references it.
        stage.atlases.push(
            publish_object_atlas(connection, staging, id, &atlas)
                .wrap_err_with(|| format!("failed to publish the object atlas of {editor_id}"))?,
        );
        if let Some(dds) = retail.normal_atlas_dds {
            stage
                .normal_atlases
                .push(super::objects::publish_object_normal_atlas(
                    staging, id, &dds,
                )?);
        }
        for (chunk, input) in &chunks {
            publish_object_chunk(connection, staging, chunk)
                .wrap_err_with(|| format!("failed to publish an object chunk of {editor_id}"))?;
            stage.triangles += chunk.triangles;
            stage
                .chunks
                .push((object_chunk_payload_path(chunk.key), input.clone()));
        }
    }
    if without > 0 {
        stage.notices.push(format!(
            "Object LOD: {without} worldspace(s) with terrain LOD have no level 16, 8 or 4 object blocks in the game data"
        ));
    }
    if !stage.skipped_worlds.is_empty() {
        stage.notices.push(format!(
            "Object LOD: {} worldspace(s) skipped: {MISSING_ATLAS}: {}",
            stage.skipped_worlds.len(),
            stage
                .skipped_worlds
                .iter()
                .map(|world| world.editor_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !stage.chunks.is_empty() {
        let per_level: Vec<String> = stage
            .levels
            .iter()
            .rev()
            .map(|(level, counts)| {
                format!(
                    "level {level}: {} chunks, {} triangles",
                    counts.chunks, counts.triangles
                )
            })
            .collect();
        stage.notices.push(format!(
            "Object LOD: {} chunks, {} triangles ({})",
            stage.chunks.len(),
            stage.triangles,
            per_level.join("; ")
        ));
    }
    Ok(stage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lod::retail::bto::tests::{SHAPE, nif_bytes, shape_block};

    /// A Data folder with loose retail object LOD for `tamriel`: the given level-16 blocks (cell
    /// plus raw bytes) and a small atlas.
    fn data_with(blocks: &[((i32, i32), Vec<u8>)]) -> tempfile::TempDir {
        let data = tempfile::tempdir().unwrap();
        let meshes = data.path().join("Meshes/Terrain/Tamriel/Objects");
        let textures = data.path().join("textures/terrain/tamriel/objects");
        fs::create_dir_all(&meshes).unwrap();
        fs::create_dir_all(&textures).unwrap();
        for ((x, y), bytes) in blocks {
            fs::write(meshes.join(format!("Tamriel.16.{x}.{y}.bto")), bytes).unwrap();
        }
        let dds = dummy_content::dds::generate(
            &dummy_content::dds::Spec::new(dummy_content::dds::Format::X8R8G8B8, 8, 8),
            &mut dummy_content::rng::Rng::new(0),
        )
        .unwrap();
        fs::write(textures.join("tamriel.objects.dds"), dds).unwrap();
        data
    }

    /// A `.bto` with one level-16 shape whose block origin is cell `(x, y)`.
    fn bto(x: i32, y: i32, segments: &[(u8, u32, u32)]) -> Vec<u8> {
        bto_with_uvs(x, y, segments, true)
    }

    fn bto_with_uvs(x: i32, y: i32, segments: &[(u8, u32, u32)], with_uvs: bool) -> Vec<u8> {
        let vertices = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]];
        let block = shape_block(
            [x as f32 * 4096.0, y as f32 * 4096.0, 0.0],
            16.0,
            &vertices,
            &[[0, 1, 2]],
            segments,
            true,
            with_uvs,
        );
        nif_bytes(&[(SHAPE, block)])
    }

    /// A staged world database with Tamriel, its LOD origin and one terrain chunk.
    fn staged(root: &Path) -> Connection {
        let connection = Connection::open(root.join("skyrim_world.db")).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO worldspaces(id, editor_id, flags, lod_origin_x, lod_origin_y) VALUES (60, 'Tamriel', 0, -96, -96)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO worldspaces(id, editor_id, flags, lod_origin_x, lod_origin_y) VALUES (61, 'NoLodWorld', 0, 0, 0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO lod_chunks(worldspace_id, tier, anchor_x, anchor_y, payload_path, content_hash, bounds_min_x, bounds_min_y, bounds_min_z, bounds_max_x, bounds_max_y, bounds_max_z, source_cells) VALUES (60, 16, 6, 5, 'lod/0000003c/16/cell_6_5.glb', 'h', 0, 0, 0, 1, 1, 1, '0,-16')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO lod_chunks(worldspace_id, tier, anchor_x, anchor_y, payload_path, content_hash, bounds_min_x, bounds_min_y, bounds_min_z, bounds_max_x, bounds_max_y, bounds_max_z, source_cells) VALUES (61, 16, 0, 0, 'lod/0000003d/16/cell_0_0.glb', 'h', 0, 0, 0, 1, 1, 1, '0,0')",
                [],
            )
            .unwrap();
        connection
    }

    fn config(data: &Path) -> PipelineConfig {
        PipelineConfig::new(data, data.join("out"))
    }

    #[test]
    fn loose_blocks_become_published_chunks_with_one_input_proof_each() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let stage = build_object_lod(&config(data.path()), staging.path(), &connection).unwrap();
        assert_eq!(stage.chunks.len(), 1);
        assert_eq!(stage.triangles, 1);
        assert!(
            stage
                .warnings
                .iter()
                .all(|warning| warning.contains(MISSING_NORMAL_ATLAS)),
            "{:?}",
            stage.warnings
        );
        let rows: i64 = connection
            .query_row("SELECT count(*) FROM lod_object_chunks", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
        assert!(staging.path().join(&stage.chunks[0].0).is_file());
        // The world without any block is a counted notice, not an error.
        assert!(
            stage
                .notices
                .iter()
                .any(|notice| notice.contains("1 worldspace"))
        );
    }

    #[test]
    fn levels_8_and_4_are_built_beside_level_16_with_counts_per_level() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        let meshes = data.path().join("Meshes/Terrain/Tamriel/Objects");
        fs::write(
            meshes.join("Tamriel.8.0.-16.bto"),
            bto(0, -16, &[(0, 0, 1)]),
        )
        .unwrap();
        // A level-4 shape with two segments, one empty: one cell-tagged piece.
        fs::write(
            meshes.join("Tamriel.4.0.-16.bto"),
            bto(0, -16, &[(0, 0, 1), (0, 3, 0)]),
        )
        .unwrap();
        // Level 32 is not a shipped level: it is not read.
        fs::write(
            meshes.join("Tamriel.32.0.-32.bto"),
            bto(0, -32, &[(0, 0, 1)]),
        )
        .unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let stage = build_object_lod(&config(data.path()), staging.path(), &connection).unwrap();
        assert!(
            stage
                .warnings
                .iter()
                .all(|warning| warning.contains(MISSING_NORMAL_ATLAS)),
            "{:?}",
            stage.warnings
        );
        assert_eq!(stage.chunks.len(), 3);
        assert_eq!(
            stage.levels.keys().copied().collect::<Vec<_>>(),
            vec![4, 8, 16]
        );
        assert_eq!(stage.levels[&4].cell_pieces, 1);
        assert_eq!(stage.levels[&8].cell_pieces, 0);
        let tiers: Vec<i64> = connection
            .prepare("SELECT tier FROM lod_object_chunks ORDER BY tier")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(tiers, vec![4, 8, 16]);
        assert!(
            stage
                .notices
                .iter()
                .any(|notice| notice.contains("level 4: 1 chunks"))
        );
    }

    #[test]
    fn a_skipped_block_is_a_warning_with_its_reason_and_the_rest_still_publish() {
        let data = data_with(&[
            ((0, -16), bto(0, -16, &[(0, 0, 1)])),
            // Two segments: the level-16 compiler refuses it.
            ((16, -16), bto(16, -16, &[(0, 0, 1), (0, 1, 0)])),
            ((3, -16), b"not a nif".to_vec()),
        ]);
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let stage = build_object_lod(&config(data.path()), staging.path(), &connection).unwrap();
        assert_eq!(stage.chunks.len(), 1);
        assert_eq!(stage.warnings.len(), 3, "{:?}", stage.warnings);
        assert!(
            stage
                .warnings
                .iter()
                .all(|line| line.contains("skipped block") || line.contains(MISSING_NORMAL_ATLAS))
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_and_missing_uv_blocks_are_counted_and_the_other_block_publishes() {
        let data = data_with(&[
            ((0, -16), bto(0, -16, &[(0, 0, 1)])),
            ((16, -16), bto_with_uvs(16, -16, &[(0, 0, 1)], false)),
        ]);
        std::os::unix::fs::symlink(
            data.path().join("missing"),
            data.path()
                .join("Meshes/Terrain/Tamriel/Objects/Tamriel.16.32.-16.bto"),
        )
        .unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let stage = build_object_lod(&config(data.path()), staging.path(), &connection).unwrap();
        assert_eq!(stage.chunks.len(), 1);
        assert_eq!(stage.warnings.len(), 3);
        assert!(
            stage
                .warnings
                .iter()
                .any(|warning| warning.contains("block cannot be read"))
        );
        assert!(stage.warnings.iter().any(|warning| {
            warning.contains("block lacks required vertex data") && warning.contains("TEXCOORD_0")
        }));
    }

    const ATLAS_DOTTED: &str = "textures/terrain/tamriel/objects/tamriel.objects.dds";
    const ATLAS_LOD: &str = "textures/terrain/tamriel/objects/tamrielobjectslod.dds";

    /// The atlas rows, or 0 when the pack has no object tables at all (nothing was published).
    fn atlas_object_rows(connection: &Connection) -> i64 {
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'lod_object_atlases'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if tables == 0 {
            return 0;
        }
        connection
            .query_row("SELECT count(*) FROM lod_object_atlases", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn the_atlas_is_found_under_the_dotted_name() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        fs::copy(
            data.path().join(ATLAS_DOTTED),
            data.path().join(&atlas_paths("tamriel", true)[0]),
        )
        .unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let stage = build_object_lod(&config(data.path()), staging.path(), &connection).unwrap();
        assert_eq!(stage.chunks.len(), 1);
        assert_eq!(stage.normal_atlases.len(), 1);
        assert!(stage.skipped_worlds.is_empty());
        assert!(stage.warnings.is_empty(), "{:?}", stage.warnings);
    }

    #[test]
    fn the_atlas_is_found_under_the_objectslod_name() {
        // The Creation Club worlds ship `<world>objectslod.dds` and no dotted atlas.
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        fs::rename(data.path().join(ATLAS_DOTTED), data.path().join(ATLAS_LOD)).unwrap();
        fs::copy(
            data.path().join(ATLAS_LOD),
            data.path().join(&atlas_paths("tamriel", true)[1]),
        )
        .unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let stage = build_object_lod(&config(data.path()), staging.path(), &connection).unwrap();
        assert_eq!(stage.chunks.len(), 1, "{:?}", stage.warnings);
        assert_eq!(stage.normal_atlases.len(), 1);
        assert!(stage.skipped_worlds.is_empty());
        assert!(stage.warnings.is_empty(), "{:?}", stage.warnings);
        assert_eq!(atlas_object_rows(&connection), 1);
    }

    #[test]
    fn a_missing_normal_atlas_keeps_the_chunks() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let stage = build_object_lod(&config(data.path()), staging.path(), &connection).unwrap();
        assert_eq!(stage.chunks.len(), 1);
        assert!(stage.normal_atlases.is_empty());
        assert_eq!(
            stage
                .warnings
                .iter()
                .filter(|warning| warning.contains(MISSING_NORMAL_ATLAS))
                .count(),
            1
        );
    }

    #[test]
    fn both_atlas_names_at_once_are_refused_not_guessed() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        fs::copy(data.path().join(ATLAS_DOTTED), data.path().join(ATLAS_LOD)).unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let error = build_object_lod(&config(data.path()), staging.path(), &connection)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("ambiguous"), "{error:#}");
    }

    #[test]
    fn a_world_with_blocks_but_no_atlas_is_skipped_and_reported_with_near_matches() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        fs::remove_file(data.path().join(ATLAS_DOTTED)).unwrap();
        // A file that is near but is not an atlas name: it is listed, never used.
        let near = data
            .path()
            .join("textures/terrain/tamriel/objects/tamriel.objects_n.dds");
        fs::write(&near, b"normal map").unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let stage = build_object_lod(&config(data.path()), staging.path(), &connection).unwrap();
        // The world has no object rows. No stand-in atlas exists.
        assert!(stage.chunks.is_empty() && stage.atlases.is_empty());
        assert_eq!(atlas_object_rows(&connection), 0);
        assert_eq!(stage.skipped_worlds.len(), 1);
        let skipped = &stage.skipped_worlds[0];
        assert_eq!(skipped.editor_id, "Tamriel");
        assert_eq!(skipped.class, MISSING_ATLAS);
        assert!(skipped.detail.contains(ATLAS_DOTTED), "{}", skipped.detail);
        assert!(skipped.detail.contains(ATLAS_LOD), "{}", skipped.detail);
        assert!(
            skipped
                .detail
                .contains("near matches: textures/terrain/tamriel/objects/tamriel.objects_n.dds"),
            "{}",
            skipped.detail
        );
        assert!(
            stage.warnings.iter().any(|line| {
                line.contains("Tamriel")
                    && line.contains("world skipped: missing_atlas")
                    && line.contains(ATLAS_DOTTED)
            }),
            "{:?}",
            stage.warnings
        );
        assert!(
            stage
                .notices
                .iter()
                .any(|line| line.contains("1 worldspace(s) skipped: missing_atlas: Tamriel")),
            "{:?}",
            stage.notices
        );
    }

    #[test]
    fn a_corrupt_atlas_still_fails_the_stage() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        fs::write(data.path().join(ATLAS_DOTTED), b"this is not a dds file").unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let error = build_object_lod(&config(data.path()), staging.path(), &connection)
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("not a valid DDS"),
            "{error:#}"
        );
        assert!(error.downcast_ref::<MissingObjectAtlas>().is_none());
    }

    #[test]
    fn a_corrupt_objectslod_atlas_still_fails_the_stage() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        fs::remove_file(data.path().join(ATLAS_DOTTED)).unwrap();
        fs::write(data.path().join(ATLAS_LOD), b"this is not a dds file").unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let error = build_object_lod(&config(data.path()), staging.path(), &connection)
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("not a valid DDS"),
            "{error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_atlas_that_exists_but_cannot_be_read_still_fails_the_stage() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        let atlas = data.path().join(ATLAS_DOTTED);
        fs::remove_file(&atlas).unwrap();
        std::os::unix::fs::symlink(data.path().join("missing"), &atlas).unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let error = build_object_lod(&config(data.path()), staging.path(), &connection)
            .err()
            .unwrap();
        assert!(error.downcast_ref::<MissingObjectAtlas>().is_none());
    }

    #[test]
    fn the_reader_refuses_a_missing_atlas_with_the_typed_error_and_near_matches() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        fs::remove_file(data.path().join(ATLAS_DOTTED)).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let mut verify = |_: &str, _: &Path| Ok(serde_json::json!({}));
        let error = read_retail(data.path(), "tamriel", scratch.path(), &mut verify)
            .err()
            .unwrap();
        let missing = error.downcast_ref::<MissingObjectAtlas>().unwrap();
        assert_eq!(missing.paths, atlas_paths("tamriel", false).to_vec());
        let text = format!("{error:#}");
        assert!(text.starts_with("refused: the game data has no "), "{text}");
        assert!(text.ends_with(", the object atlas"), "{text}");
    }

    #[test]
    fn a_world_without_blocks_names_near_matches_when_the_reader_refuses() {
        let data = data_with(&[]);
        let scratch = tempfile::tempdir().unwrap();
        let mut verify = |_: &str, _: &Path| Ok(serde_json::json!({}));
        let RetailRead::NoBlocks(detail) =
            read_retail(data.path(), "tamriel", scratch.path(), &mut verify).unwrap()
        else {
            panic!("the world has no blocks");
        };
        assert!(
            detail.contains("near matches: textures/terrain/tamriel/objects/tamriel.objects.dds"),
            "{detail}"
        );
    }

    #[test]
    fn a_data_folder_without_object_lod_adds_nothing() {
        let data = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let stage = build_object_lod(&config(data.path()), staging.path(), &connection).unwrap();
        assert!(stage.chunks.is_empty());
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name LIKE 'lod_object%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "no object table for a pack without object LOD");
    }
}
