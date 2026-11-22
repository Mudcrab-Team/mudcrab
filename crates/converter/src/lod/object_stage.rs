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
        objects::{
            ObjectChunk, chunk_key_for_block, compile_object_chunk, object_chunk_input_hash,
            publish_object_chunk,
        },
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
use rusqlite::Connection;
use shared::lod::{LodOrigin, object_chunk_payload_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// The only block level compiled in this slice.
pub(crate) const LEVEL: u32 = 16;

/// A block the run did not compile, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedBlock {
    pub cell: (i32, i32),
    /// The class of the reason, the key of the counts.
    pub class: &'static str,
    pub detail: String,
}

/// One `.bto` block read from the game data, before compilation.
pub struct BlockCandidate {
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
    /// Where each input came from, for the receipt.
    pub sources: Vec<serde_json::Value>,
    /// Blocks that could not be read at all.
    pub skipped: Vec<SkippedBlock>,
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

/// Finds the files `wanted` accepts in loose files and archives of `data`. Loose files win over
/// archives; otherwise every archive that has the file is listed.
fn providers(
    data: &Path,
    wanted: &dyn Fn(&str) -> bool,
) -> Result<BTreeMap<String, Vec<Provider>>> {
    let mut found: BTreeMap<String, Vec<Provider>> = BTreeMap::new();
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
        let paths = ArchiveExtractor::list_paths(&archive)
            .wrap_err_with(|| format!("cannot list archive {name}"))?;
        for path in paths {
            let key = normal(&path);
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
                } else if wanted(&normal(&relative)) {
                    let key = normal(&relative);
                    // A loose file replaces whatever the archives hold.
                    found.insert(key, vec![Provider::Loose(path)]);
                }
            }
        }
    }
    Ok(found)
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
/// Returns `Ok(None)` when the game data has no level-16 block of this world at all, which is
/// normal for most worldspaces. A world with blocks but no usable atlas is an error.
pub(crate) fn read_retail(
    data: &Path,
    world_file: &str,
    scratch: &Path,
    verify_archive: &mut dyn FnMut(&str, &Path) -> Result<serde_json::Value>,
) -> Result<Option<Retail>> {
    let atlas_path = format!("textures/terrain/{world_file}/objects/{world_file}.objects.dds");
    let is_block = |path: &str| {
        parse_lod_file_name(path).is_some_and(|name| {
            name.kind == RetailLodKind::Object
                && name.world == world_file
                && name.level == Some(LEVEL)
        }) && path.starts_with("meshes/")
    };
    let wanted = |path: &str| path == atlas_path || is_block(path);
    let found = providers(data, &wanted)?;
    if !found.keys().any(|path| is_block(path)) {
        return Ok(None);
    }

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
        ArchiveExtractor::extract_matching(path, &root, |entry| {
            wanted_here.contains(&normal(entry))
        })
        .wrap_err_with(|| format!("failed to read from archive {name}"))?;
        extracted.insert(path.clone(), root);
    }

    // The atlas.
    let atlas_options = found.get(&atlas_path).ok_or_else(|| {
        color_eyre::eyre::eyre!("refused: the game data has no {atlas_path}, the object atlas")
    })?;
    let (atlas_dds, atlas_providers) =
        match read_provider_bytes(&atlas_path, atlas_options, &extracted)? {
            Ok(read) => read,
            Err(conflict) => bail!("refused: the object atlas is ambiguous: {conflict}"),
        };
    let info = read_dds_info(&atlas_dds).map_err(|error| {
        color_eyre::eyre::eyre!("refused: the object atlas is not a valid DDS: {error}")
    })?;
    let atlas_length = u64::try_from(atlas_dds.len()).wrap_err("the object atlas is too large")?;
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

    // The blocks.
    let mut blocks = Vec::new();
    let mut skipped = Vec::new();
    let mut loose = 0usize;
    for (relative, options) in found.iter().filter(|(path, _)| is_block(path)) {
        let Some(name) = parse_lod_file_name(relative) else {
            continue;
        };
        let Some(cell) = name.cell else { continue };
        match read_provider_bytes(relative, options, &extracted)? {
            Err(conflict) => skipped.push(SkippedBlock {
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
    Ok(Some(Retail {
        blocks,
        atlas_dds,
        sources,
        skipped,
    }))
}

/// Compiles the candidates of one world into object chunks with the shared atlas. A block that
/// cannot be read, is off the LOD grid, or that the compiler refuses is skipped with its class and
/// reason. The caller decides whether skipped blocks or an empty result are errors.
pub(crate) fn compile_candidates(
    world: WorldFacts,
    candidates: Vec<BlockCandidate>,
    atlas_ktx2: &[u8],
    atlas_sha256: &str,
) -> Result<(Vec<(ObjectChunk, String)>, Vec<SkippedBlock>)> {
    let atlas = atlas_ktx2;
    let mut skipped = Vec::new();
    let mut chunks: Vec<(ObjectChunk, String)> = Vec::new();
    for candidate in candidates {
        let block = match &candidate.parsed {
            Ok(block) => block,
            Err(reason) => {
                skipped.push(SkippedBlock {
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
            LEVEL,
            candidate.cell.0,
            candidate.cell.1,
        ) {
            Ok(key) => key,
            Err(error) => {
                skipped.push(SkippedBlock {
                    cell: candidate.cell,
                    class: "block is not on the LOD grid",
                    detail: format!("{error:#}"),
                });
                continue;
            }
        };
        match compile_object_chunk(key, world.origin, block, atlas) {
            Ok(chunk) => {
                let input =
                    object_chunk_input_hash(key, world.origin, &candidate.sha256, atlas_sha256)?;
                chunks.push((chunk, input));
            }
            Err(error) => skipped.push(SkippedBlock {
                cell: candidate.cell,
                class: "block refused by the compiler",
                detail: format!("{error:#}"),
            }),
        }
    }
    Ok((chunks, skipped))
}

/// What the pipeline stage added to the staged pack.
#[derive(Debug, Default)]
pub(crate) struct ObjectLodStage {
    /// Payload path and input proof of each published chunk.
    pub chunks: Vec<(String, String)>,
    pub triangles: usize,
    /// Notices that do not make the run incomplete (for example, worlds without object LOD).
    pub notices: Vec<String>,
    /// One line per skipped block or refused world, counted in the LOD warnings.
    pub warnings: Vec<String>,
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
        let world_scratch = scratch.path().join(format!("world-{id:08x}"));
        let Some(retail) = read_retail(&config.data_dir, &file_name, &world_scratch, &mut verify)
            .wrap_err_with(|| {
            format!("object LOD of worldspace {editor_id} could not be read")
        })?
        else {
            without += 1;
            continue;
        };
        let world = WorldFacts {
            id,
            origin: LodOrigin::new(origin_x, origin_y),
        };
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
        let (chunks, skipped) = compile_candidates(world, retail.blocks, &atlas, &atlas_sha256)?;
        for block in retail.skipped.iter().chain(&skipped) {
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
            "Object LOD: {without} worldspace(s) with terrain LOD have no level-{LEVEL} object blocks in the game data"
        ));
    }
    if !stage.chunks.is_empty() {
        stage.notices.push(format!(
            "Object LOD: {} chunks, {} triangles",
            stage.chunks.len(),
            stage.triangles
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
        let vertices = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]];
        let block = shape_block(
            [x as f32 * 4096.0, y as f32 * 4096.0, 0.0],
            16.0,
            &vertices,
            &[[0, 1, 2]],
            segments,
            true,
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
        assert!(stage.warnings.is_empty(), "{:?}", stage.warnings);
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
        assert_eq!(stage.warnings.len(), 2, "{:?}", stage.warnings);
        assert!(
            stage
                .warnings
                .iter()
                .all(|line| line.contains("skipped block"))
        );
    }

    #[test]
    fn a_world_with_blocks_but_no_atlas_fails_the_stage() {
        let data = data_with(&[((0, -16), bto(0, -16, &[(0, 0, 1)]))]);
        fs::remove_file(
            data.path()
                .join("textures/terrain/tamriel/objects/tamriel.objects.dds"),
        )
        .unwrap();
        let staging = tempfile::tempdir().unwrap();
        let connection = staged(staging.path());
        let error = build_object_lod(&config(data.path()), staging.path(), &connection)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("atlas"), "{error:#}");
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
