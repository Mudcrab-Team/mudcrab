//! Derive retail object LOD into an APFS clone of a registered pack, without reconverting any NIF.
//!
//! The pack already holds the terrain LOD and its world database. This command adds what object
//! LOD needs and nothing else: one object chunk GLB per retail level-16 `.bto` block (compiled by
//! [`crate::lod::objects`]), the additive `lod_object_chunks` table and its spatial index, the
//! additive fields of `lod-manifest.json` (`object_chunks`, `objects_compiler_version`,
//! `object_chunk_inputs`) and a receipt. It reads the `.bto` blocks and the object atlas from the
//! Skyrim `Data` folder (loose files, then `.bsa` archives, through the converter's archive
//! reader). It does not touch any NIF, texture or script output, `conversion-manifest.json`,
//! `integration-report.json`, or the terrain chunks and their `lod_build` identity.
//!
//! Fails closed. It refuses a target that is not an exact clone of a registered pack (see
//! [`crate::pack_clone`]), a pack without terrain LOD, a pack that already has object chunks
//! (unless `force`), an archive that is not the one the pack was converted from, and a block that
//! the compiler does not understand. Such a block is skipped and counted with its reason; nothing
//! is guessed. If the finished pack does not validate, the run removes what it added.

use crate::{
    archive::ArchiveExtractor,
    cache::{CONVERTER_SCHEMA_VERSION, ConversionManifest, hash_bytes, hash_file},
    lod::{
        objects::{
            OBJECT_COMPILER_VERSION, ObjectChunk, chunk_key_for_block, compile_object_chunk,
            object_chunk_input_hash, publish_object_chunk,
        },
        retail::{
            atlas::read_dds_info,
            bto::{ObjectLodFile, read_object_lod},
            name::{RetailLodKind, parse_lod_file_name},
        },
        reuse::LodManifest,
    },
    pack_clone::check_clone,
    texture::{TextureConverter, TextureEncoding, inspect_ktx2},
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
    time::Instant,
};

/// Name of the receipt written into the clone.
pub const RECEIPT_FILE: &str = "object-lod-overlay-receipt.json";
/// Receipt format name, so a reader can recognise the file.
pub const RECEIPT_FORMAT: &str = "object-lod-overlay-receipt/1";
/// The only block level compiled in this slice.
const LEVEL: u32 = 16;

/// Inputs of `derive-object-lod`.
#[derive(Debug, Clone)]
pub struct DeriveObjectLodOptions {
    /// The APFS clone that receives the object LOD.
    pub target: PathBuf,
    /// `packs.json` of the shared storage. The command only reads it.
    pub registry: PathBuf,
    /// Registered pack that `target` must be a clone of.
    pub clone_of: String,
    /// The Skyrim `Data` folder the pack was converted from. Read only.
    pub data: PathBuf,
    /// Editor id of the worldspace, `Tamriel` by default.
    pub worldspace: String,
    /// Replace object chunks that the target already has for this worldspace.
    pub force: bool,
    /// Git head of the converter build, recorded in the receipt. The caller supplies it.
    pub converter_head: Option<String>,
    /// The command line as the caller ran it, recorded in the receipt.
    pub command: Vec<String>,
}

/// A block the run did not compile, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedBlock {
    pub cell: (i32, i32),
    /// The class of the reason, the key of the counts.
    pub class: &'static str,
    pub detail: String,
}

/// What a successful run produced.
#[derive(Debug)]
pub struct DeriveObjectLodReport {
    pub receipt: PathBuf,
    /// Blocks found in the game data.
    pub blocks_found: usize,
    pub chunks: usize,
    pub triangles: usize,
    pub vertices: usize,
    pub skipped: Vec<SkippedBlock>,
    pub skipped_by_class: BTreeMap<&'static str, usize>,
    pub bytes_added: u64,
    pub atlas_ktx2_bytes: usize,
    pub elapsed_ms: u128,
}

/// One retail block read from the game data, before compilation.
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

/// What [`derive_into`] changed.
pub struct CoreReport {
    pub chunks: Vec<(ObjectChunk, String)>,
    pub skipped: Vec<SkippedBlock>,
    pub atlas_ktx2_bytes: usize,
    pub atlas_width: u32,
    pub atlas_height: u32,
    pub atlas_levels: u32,
}

/// Derive object LOD into `options.target`. See the module documentation for the rules.
pub fn run(options: &DeriveObjectLodOptions) -> Result<DeriveObjectLodReport> {
    let started = Instant::now();
    let clone = check_clone(&options.registry, &options.clone_of, &options.target)?;
    let target = clone.target.clone();
    let conversion = ConversionManifest::load(&target.join("conversion-manifest.json"))
        .wrap_err("refused: the target conversion manifest cannot be read")?;
    ensure!(
        conversion.schema_version == CONVERTER_SCHEMA_VERSION,
        "refused: the target manifest has schema {}, this converter writes schema {CONVERTER_SCHEMA_VERSION}",
        conversion.schema_version
    );
    ensure!(
        conversion.complete,
        "refused: the target conversion manifest is not complete"
    );
    // The engine's own contract for a pack with terrain LOD: object chunks hang off it.
    shared::world_assets::validate_lod_build_contract(&target, CONVERTER_SCHEMA_VERSION)
        .wrap_err("refused: the target has no valid terrain LOD for object LOD to attach to")?;
    ensure!(
        target.join("lod-manifest.json").is_file(),
        "refused: the target has no lod-manifest.json"
    );
    ensure!(
        options.force || !target.join(RECEIPT_FILE).exists(),
        "refused: the target already has {RECEIPT_FILE}; pass --force to derive again"
    );
    let world = world_facts(&target, &options.worldspace)?;
    let world_file = options.worldspace.to_ascii_lowercase();
    ensure!(
        !world_file.is_empty() && !world_file.contains(['/', '\\', '.']),
        "refused: `{}` is not a usable worldspace editor id",
        options.worldspace
    );
    if !options.force {
        refuse_existing_object_lod(&target)?;
    }

    // Read the retail inputs. Every archive that provides a file must be the one the pack was
    // converted from.
    let scratch = tempfile::tempdir().wrap_err("failed to create a scratch folder")?;
    let retail = read_retail(&options.data, &world_file, &conversion, scratch.path())?;
    let blocks_found = retail.blocks.len();
    let atlas_sha256 = hash_bytes(&retail.atlas_dds);

    let manifest_before = fs::read(target.join("lod-manifest.json"))?;
    let core = derive_into(
        &target,
        world,
        retail.blocks,
        &retail.atlas_dds,
        &atlas_sha256,
        options.force,
    )?;
    drop(scratch);
    // Object chunks do not touch terrain LOD, so the engine's contract holds unless something
    // else changed. If it does not, this clone is not safe to use.
    shared::world_assets::validate_lod_build_contract(&target, CONVERTER_SCHEMA_VERSION).wrap_err(
        "the derived pack no longer satisfies the engine's LOD contract; discard this clone",
    )?;

    let triangles = core.chunks.iter().map(|(chunk, _)| chunk.triangles).sum();
    let vertices = core
        .chunks
        .iter()
        .map(|(chunk, _)| chunk.vertices)
        .sum::<usize>();
    let mut skipped_by_class: BTreeMap<&'static str, usize> = BTreeMap::new();
    for block in retail.skipped.iter().chain(&core.skipped) {
        *skipped_by_class.entry(block.class).or_default() += 1;
    }
    let skipped: Vec<SkippedBlock> = retail.skipped.into_iter().chain(core.skipped).collect();
    let chunk_bytes: u64 = core
        .chunks
        .iter()
        .map(|(chunk, _)| chunk.glb.len() as u64)
        .sum();
    let elapsed_ms = started.elapsed().as_millis();
    let receipt = serde_json::json!({
        "format": RECEIPT_FORMAT,
        "command": options.command,
        "converter_head": options.converter_head,
        "object_compiler_version": OBJECT_COMPILER_VERSION,
        "clone_of": {
            "name": clone.name,
            "manifests": clone.manifests,
        },
        "worldspace": {
            "editor_id": options.worldspace,
            "id": world.id,
            "lod_origin": [world.origin.grid_x, world.origin.grid_y],
        },
        "game_data_read": true,
        "sources": retail.sources,
        "atlas": {
            "retail_dds_sha256": atlas_sha256,
            "ktx2_bytes": core.atlas_ktx2_bytes,
            "width": core.atlas_width,
            "height": core.atlas_height,
            "mip_levels": core.atlas_levels,
        },
        "blocks_found": blocks_found,
        "chunks": core.chunks.iter().map(|(chunk, input)| serde_json::json!({
            "path": object_chunk_payload_path(chunk.key),
            "triangles": chunk.triangles,
            "vertices": chunk.vertices,
            "bytes": chunk.glb.len(),
            "sha256": hash_bytes(&chunk.glb),
            "input_sha256": input,
        })).collect::<Vec<_>>(),
        "totals": {
            "chunks": core.chunks.len(),
            "triangles": triangles,
            "vertices": vertices,
            "chunk_bytes": chunk_bytes,
            "skipped": skipped.len(),
        },
        "skipped_by_class": skipped_by_class,
        "skipped": skipped.iter().map(|block| serde_json::json!({
            "cell": [block.cell.0, block.cell.1],
            "class": block.class,
            "detail": block.detail,
        })).collect::<Vec<_>>(),
        "lod_manifest_sha256": {
            "before": hash_bytes(&manifest_before),
            "after": hash_file(&target.join("lod-manifest.json"))?,
        },
        "unverified": [
            "Material: alpha mask 128/255 and single-sided unless node flags & 0x108 (reviewed static research, culling medium confidence).",
            "Vertex alpha is written as stored; whether retail multiplies it into the cutout is not verified.",
            "The atlas is embedded in every chunk; a shared texture is a later optimisation.",
        ],
        "elapsed_ms": elapsed_ms,
    });
    let text = serde_json::to_string_pretty(&receipt)? + "\n";
    fs::write(target.join(RECEIPT_FILE), &text)
        .wrap_err("failed to write the object LOD receipt")?;
    // The registered manifests are untouched by this command; prove it.
    for (file, expected) in &clone.manifests {
        ensure!(
            &hash_file(&target.join(file))? == expected,
            "the derive changed {file}, which it must not touch"
        );
    }
    Ok(DeriveObjectLodReport {
        receipt: target.join(RECEIPT_FILE),
        blocks_found,
        chunks: core.chunks.len(),
        triangles,
        vertices,
        skipped,
        skipped_by_class,
        bytes_added: chunk_bytes + text.len() as u64,
        atlas_ktx2_bytes: core.atlas_ktx2_bytes,
        elapsed_ms,
    })
}

/// The worldspace id and its LOD origin, from the pack's own database.
fn world_facts(target: &Path, editor_id: &str) -> Result<WorldFacts> {
    let connection = Connection::open_with_flags(
        target.join("skyrim_world.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .wrap_err("refused: cannot open the target world database")?;
    let row: Option<(u32, Option<i32>, Option<i32>)> = connection
        .query_row(
            "SELECT id, lod_origin_x, lod_origin_y FROM worldspaces WHERE editor_id = ?1 COLLATE NOCASE",
            [editor_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some((id, Some(x), Some(y))) = row else {
        bail!("refused: worldspace `{editor_id}` is not in the pack or has no LOD origin");
    };
    let has_terrain: i64 = connection.query_row(
        "SELECT count(*) FROM lod_chunks WHERE worldspace_id = ?1",
        [id],
        |row| row.get(0),
    )?;
    ensure!(
        has_terrain > 0,
        "refused: the pack has no terrain LOD chunks for `{editor_id}`"
    );
    Ok(WorldFacts {
        id,
        origin: LodOrigin::new(x, y),
    })
}

/// Refuse a target that already has object chunks, in the table or in the manifest.
fn refuse_existing_object_lod(target: &Path) -> Result<()> {
    let manifest: LodManifest =
        serde_json::from_slice(&fs::read(target.join("lod-manifest.json"))?)
            .wrap_err("refused: lod-manifest.json is not valid")?;
    ensure!(
        manifest.object_chunks == 0 && manifest.object_chunk_inputs.is_empty(),
        "refused: lod-manifest.json already lists {} object chunks; pass --force to replace them",
        manifest.object_chunks
    );
    let connection = Connection::open_with_flags(
        target.join("skyrim_world.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let rows = object_row_count(&connection)?;
    ensure!(
        rows == 0,
        "refused: the database already has {rows} object chunks; pass --force to replace them"
    );
    Ok(())
}

fn object_row_count(connection: &Connection) -> Result<i64> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lod_object_chunks')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(0);
    }
    Ok(
        connection.query_row("SELECT count(*) FROM lod_object_chunks", [], |row| {
            row.get(0)
        })?,
    )
}

/// Retail data read from the game folder.
struct Retail {
    blocks: Vec<BlockCandidate>,
    atlas_dds: Vec<u8>,
    /// Where each input came from, for the receipt.
    sources: Vec<serde_json::Value>,
    /// Blocks that could not be read at all.
    skipped: Vec<SkippedBlock>,
}

/// Where one retail file can be read from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Provider {
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
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
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
            .unwrap_or_default();
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
        let mut stack = vec![(PathBuf::from(root), child_ci(data, root))];
        while let Some((relative, Some(directory))) = stack.pop() {
            for entry in fs::read_dir(&directory)?.filter_map(|entry| entry.ok()) {
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
fn child_ci(directory: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(directory)
        .ok()?
        .filter_map(|entry| entry.ok())
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(name)
        })
        .map(|entry| entry.path())
}

/// Reads the bytes of `relative` from its providers. `Ok(Err(..))` is a conflict: several
/// providers hold different bytes, which this command does not resolve by guessing.
fn read_provider_bytes(
    relative: &str,
    options: &[Provider],
    extracted: &BTreeMap<PathBuf, PathBuf>,
) -> Result<std::result::Result<(Vec<u8>, Vec<Provider>), String>> {
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
fn read_retail(
    data: &Path,
    world_file: &str,
    conversion: &ConversionManifest,
    scratch: &Path,
) -> Result<Retail> {
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

    // Every archive that provides a wanted file must be the one the pack was converted from.
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
        let recorded = conversion.archives.get(name).ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "refused: archive {name} provides retail LOD files but the pack's manifest does not list it, so it is not proven to be the archive the pack was converted from"
            )
        })?;
        let actual = hash_file(path)?;
        ensure!(
            actual == recorded.source_hash,
            "refused: archive {name} differs from the one the pack was converted from (hash {actual}, manifest {})",
            recorded.source_hash
        );
        sources.push(
            serde_json::json!({ "archive": name, "sha256": actual, "matches_pack_manifest": true }),
        );
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
    ensure!(
        info.expected_payload_bytes().is_some_and(|payload| {
            u64::try_from(info.data_offset)
                .ok()
                .and_then(|offset| offset.checked_add(payload))
                .is_some_and(|total| u64::try_from(atlas_dds.len()).ok() == Some(total))
        }),
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
    ensure!(
        !blocks.is_empty(),
        "refused: the game data has no level-{LEVEL} object LOD blocks for `{world_file}`"
    );
    Ok(Retail {
        blocks,
        atlas_dds,
        sources,
        skipped,
    })
}

/// Compiles the candidates and adds the chunks to the pack at `target`: the files, the database
/// rows and the manifest fields. A finished pack must validate; otherwise what was added is
/// removed again (when `force` replaced existing chunks the removal cannot restore them, and the
/// error says so).
pub fn derive_into(
    target: &Path,
    world: WorldFacts,
    candidates: Vec<BlockCandidate>,
    atlas_dds: &[u8],
    atlas_sha256: &str,
    force: bool,
) -> Result<CoreReport> {
    // The existing texture stage turns the retail DDS into the KTX2 every chunk embeds.
    let atlas = TextureConverter::convert(atlas_dds, TextureEncoding::ColorSrgb)
        .wrap_err("the object atlas could not be converted to KTX2")?;
    let metadata = inspect_ktx2(&atlas, TextureEncoding::ColorSrgb)?;

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
        match compile_object_chunk(key, world.origin, block, &atlas) {
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
    ensure!(
        !chunks.is_empty(),
        "refused: no block compiled ({} skipped); nothing was written. First reason: {}",
        skipped.len(),
        skipped
            .first()
            .map_or("none", |block| block.detail.as_str())
    );

    let database_path = target.join("skyrim_world.db");
    let manifest_path = target.join("lod-manifest.json");
    let manifest_text = fs::read_to_string(&manifest_path)?;
    let mut manifest: LodManifest =
        serde_json::from_str(&manifest_text).wrap_err("refused: lod-manifest.json is not valid")?;
    let connection = Connection::open(&database_path)
        .wrap_err("cannot open the target world database for writing")?;
    let had_table = object_has_table(&connection)?;
    let previous: Vec<String> = if had_table {
        connection
            .prepare("SELECT payload_path FROM lod_object_chunks WHERE worldspace_id = ?1")?
            .query_map([world.id], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?
    } else {
        Vec::new()
    };
    ensure!(
        force || previous.is_empty(),
        "refused: the database already has object chunks for this worldspace"
    );

    let mut written: Vec<String> = Vec::new();
    let publish = (|| -> Result<()> {
        connection.execute_batch("BEGIN IMMEDIATE")?;
        if force && had_table {
            connection.execute(
                "DELETE FROM lod_object_chunks_spatial WHERE worldspace_id = ?1",
                [world.id],
            )?;
            connection.execute(
                "DELETE FROM lod_object_chunks WHERE worldspace_id = ?1",
                [world.id],
            )?;
        }
        for (chunk, _) in &chunks {
            written.push(object_chunk_payload_path(chunk.key));
            publish_object_chunk(&connection, target, chunk)?;
        }
        connection.execute_batch("COMMIT")?;
        Ok(())
    })();
    if let Err(error) = publish {
        let _ = connection.execute_batch("ROLLBACK");
        remove_files(target, &written);
        return Err(error.wrap_err("publishing object chunks failed; the clone was left as it was"));
    }

    // The manifest fields. Keys of this worldspace are replaced; other worlds keep theirs.
    let world_prefix = format!("lod/{:08x}/", world.id);
    manifest
        .object_chunk_inputs
        .retain(|path, _| !path.starts_with(&world_prefix));
    for (chunk, input) in &chunks {
        manifest
            .object_chunk_inputs
            .insert(object_chunk_payload_path(chunk.key), input.clone());
    }
    let total: i64 = connection.query_row("SELECT count(*) FROM lod_object_chunks", [], |row| {
        row.get(0)
    })?;
    manifest.object_chunks = u64::try_from(total).wrap_err("object chunk count")?;
    manifest.objects_compiler_version = OBJECT_COMPILER_VERSION;
    let finish = (|| -> Result<()> {
        write_atomic(&manifest_path, &serde_json::to_vec_pretty(&manifest)?)?;
        let artifacts: Vec<PathBuf> = written.iter().map(PathBuf::from).collect();
        crate::pipeline::validate_artifacts(target, &artifacts, &BTreeMap::new(), 4)
            .wrap_err("the pack with object LOD does not validate")?;
        Ok(())
    })();
    if let Err(error) = finish {
        let _ = write_atomic(&manifest_path, manifest_text.as_bytes());
        if force {
            return Err(error.wrap_err(
                "validation failed after --force replaced existing object chunks; discard this clone and clone the pack again",
            ));
        }
        let undone = (|| -> Result<()> {
            if had_table {
                connection.execute(
                    "DELETE FROM lod_object_chunks WHERE worldspace_id = ?1",
                    [world.id],
                )?;
                connection.execute(
                    "DELETE FROM lod_object_chunks_spatial WHERE worldspace_id = ?1",
                    [world.id],
                )?;
            } else {
                connection.execute_batch(
                    "DROP TABLE IF EXISTS lod_object_chunks_spatial; DROP TABLE IF EXISTS lod_object_chunks;",
                )?;
            }
            Ok(())
        })();
        remove_files(target, &written);
        if let Err(undo) = undone {
            return Err(error.wrap_err(format!(
                "the run could not remove its database rows either ({undo:#}); discard this clone"
            )));
        }
        return Err(error.wrap_err("the clone was restored"));
    }
    // A forced run drops the files of chunks it no longer produces.
    let kept: BTreeSet<&String> = written.iter().collect();
    let stale: Vec<String> = previous
        .into_iter()
        .filter(|path| !kept.contains(path))
        .collect();
    remove_files(target, &stale);

    Ok(CoreReport {
        chunks,
        skipped,
        atlas_ktx2_bytes: atlas.len(),
        atlas_width: metadata.width,
        atlas_height: metadata.height,
        atlas_levels: metadata.levels,
    })
}

fn object_has_table(connection: &Connection) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='lod_object_chunks')",
        [],
        |row| row.get(0),
    )?)
}

/// Removes files this run wrote. A file that cannot be removed is reported on stderr, never
/// ignored.
fn remove_files(root: &Path, relative: &[String]) {
    for path in relative {
        if let Err(error) = fs::remove_file(root.join(path))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("Warning: could not remove {path}: {error}");
        }
    }
}

/// Write `bytes` to `path` through a temporary file, so a reader never sees half a manifest.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)
        .wrap_err_with(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, path).wrap_err_with(|| format!("failed to publish {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lod::retail::bto::{ObjectLodSegment, ObjectLodShape, ObjectLodVertex};

    const WORLD: WorldFacts = WorldFacts {
        id: 60,
        origin: LodOrigin::new(-96, -96),
    };

    fn atlas_dds() -> Vec<u8> {
        dummy_content::dds::generate(
            &dummy_content::dds::Spec::new(dummy_content::dds::Format::X8R8G8B8, 8, 8),
            &mut dummy_content::rng::Rng::new(0),
        )
        .unwrap()
    }

    fn shape(segments: u32) -> ObjectLodShape {
        let vertex = |position| ObjectLodVertex {
            position,
            normal: Some([0.0, 0.0, 1.0]),
            uv: Some([0.5, 0.5]),
            color: Some([255; 4]),
        };
        ObjectLodShape {
            block_index: 1,
            flags: 0,
            translation: [0.0, -65_536.0, 0.0],
            rotation_columns: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            scale: 16.0,
            shader_property: u32::MAX,
            vertices: vec![
                vertex([0.0, 0.0, 0.0]),
                vertex([10.0, 0.0, 0.0]),
                vertex([0.0, 10.0, 0.0]),
            ],
            triangles: vec![[0, 1, 2]],
            segments: (0..segments)
                .map(|index| ObjectLodSegment {
                    flag: 0,
                    value: index,
                    num_primitives: u32::from(index == 0),
                })
                .collect(),
        }
    }

    fn block(shapes: Vec<ObjectLodShape>) -> ObjectLodFile {
        ObjectLodFile {
            user_version: 12,
            bethesda_version: 100,
            block_types: BTreeMap::new(),
            unhandled_blocks: BTreeMap::new(),
            shapes,
            large_ref_blocks: Vec::new(),
        }
    }

    fn candidate(
        cell: (i32, i32),
        parsed: std::result::Result<ObjectLodFile, String>,
    ) -> BlockCandidate {
        BlockCandidate {
            cell,
            sha256: hash_bytes(format!("{cell:?}").as_bytes()),
            parsed,
        }
    }

    /// A minimal pack: a world database with the shared schema and a terrain-only manifest.
    fn pack(root: &Path) {
        let connection = Connection::open(root.join("skyrim_world.db")).unwrap();
        crate::esm::exporter::create_tables(&connection).unwrap();
        fs::write(
            root.join("lod-manifest.json"),
            serde_json::to_vec_pretty(&LodManifest {
                build_identity: "0".repeat(64),
                converter_schema: 28,
                world_database_schema: 9,
                chunks: 1,
                land_texture_repeats_per_cell: 24.0,
                terrain_sources: BTreeMap::new(),
                compiler_version: 4,
                chunk_inputs: BTreeMap::new(),
                object_chunks: 0,
                objects_compiler_version: 0,
                object_chunk_inputs: BTreeMap::new(),
            })
            .unwrap(),
        )
        .unwrap();
    }

    fn rows(root: &Path) -> i64 {
        let connection = Connection::open(root.join("skyrim_world.db")).unwrap();
        object_row_count(&connection).unwrap()
    }

    fn derive(root: &Path, candidates: Vec<BlockCandidate>, force: bool) -> Result<CoreReport> {
        derive_into(root, WORLD, candidates, &atlas_dds(), "atlas", force)
    }

    #[test]
    fn a_block_compiles_into_a_listed_validated_chunk() {
        let root = tempfile::tempdir().unwrap();
        pack(root.path());
        let before = fs::read(root.path().join("lod-manifest.json")).unwrap();
        let report = derive(
            root.path(),
            vec![candidate((0, -16), Ok(block(vec![shape(1)])))],
            false,
        )
        .unwrap();
        assert_eq!(report.chunks.len(), 1);
        assert!(report.skipped.is_empty());
        let manifest: LodManifest =
            serde_json::from_slice(&fs::read(root.path().join("lod-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest.object_chunks, 1);
        assert_eq!(manifest.objects_compiler_version, OBJECT_COMPILER_VERSION);
        assert_eq!(manifest.object_chunk_inputs.len(), 1);
        assert_eq!(manifest.chunks, 1, "terrain fields are untouched");
        assert_ne!(
            before,
            fs::read(root.path().join("lod-manifest.json")).unwrap()
        );
        assert_eq!(rows(root.path()), 1);
        crate::lod::objects::validate_object_lod_package(root.path()).unwrap();
        // A second run without --force refuses; with --force it replaces.
        assert!(
            derive(
                root.path(),
                vec![candidate((0, -16), Ok(block(vec![shape(1)])))],
                false
            )
            .is_err()
        );
        derive(
            root.path(),
            vec![candidate((0, -16), Ok(block(vec![shape(1)])))],
            true,
        )
        .unwrap();
        assert_eq!(rows(root.path()), 1);
    }

    #[test]
    fn every_skipped_block_is_reported_with_its_class() {
        let root = tempfile::tempdir().unwrap();
        pack(root.path());
        let report = derive(
            root.path(),
            vec![
                candidate((0, -16), Ok(block(vec![shape(1)]))),
                candidate((16, -16), Err("truncated".to_owned())),
                candidate((3, -16), Ok(block(vec![shape(1)]))),
                candidate((32, -16), Ok(block(vec![shape(2)]))),
                candidate((48, -16), Ok(block(Vec::new()))),
            ],
            false,
        )
        .unwrap();
        assert_eq!(report.chunks.len(), 1);
        let classes: Vec<_> = report.skipped.iter().map(|block| block.class).collect();
        assert_eq!(
            classes,
            vec![
                "block cannot be read",
                "block is not on the LOD grid",
                "block refused by the compiler",
                "block refused by the compiler",
            ]
        );
        assert_eq!(rows(root.path()), 1, "skipped blocks have no row");
    }

    #[test]
    fn nothing_is_written_when_no_block_compiles() {
        let root = tempfile::tempdir().unwrap();
        pack(root.path());
        let before = fs::read(root.path().join("lod-manifest.json")).unwrap();
        let error = derive(
            root.path(),
            vec![candidate((0, -16), Err("bad".to_owned()))],
            false,
        )
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("no block compiled"));
        assert_eq!(rows(root.path()), 0);
        assert_eq!(
            before,
            fs::read(root.path().join("lod-manifest.json")).unwrap()
        );
        assert!(!root.path().join("lod").exists());
    }

    #[test]
    fn a_pack_that_fails_validation_is_restored() {
        let root = tempfile::tempdir().unwrap();
        pack(root.path());
        // An orphan spatial entry makes the finished pack disagree with itself.
        let connection = Connection::open(root.path().join("skyrim_world.db")).unwrap();
        crate::lod::objects::create_object_chunk_tables(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO lod_object_chunks_spatial(id,minX,maxX,minY,maxY,worldspace_id,tier,anchor_x,anchor_y) VALUES (1,0,1,0,1,7,16,0,0)",
                [],
            )
            .unwrap();
        drop(connection);
        let before = fs::read(root.path().join("lod-manifest.json")).unwrap();
        let error = derive(
            root.path(),
            vec![candidate((0, -16), Ok(block(vec![shape(1)])))],
            false,
        )
        .err()
        .unwrap();
        assert!(
            format!("{error:#}").contains("does not validate"),
            "{error:#}"
        );
        assert_eq!(rows(root.path()), 0);
        assert_eq!(
            before,
            fs::read(root.path().join("lod-manifest.json")).unwrap()
        );
        assert!(
            !root
                .path()
                .join(object_chunk_payload_path(
                    chunk_key_for_block(60, WORLD.origin, 16, 0, -16).unwrap()
                ))
                .exists()
        );
    }
}
