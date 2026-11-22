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

pub use crate::lod::object_stage::{BlockCandidate, SkippedBlock, WorldFacts};
use crate::{
    cache::{CONVERTER_SCHEMA_VERSION, ConversionManifest, hash_bytes, hash_file},
    lod::{
        object_stage::{LevelCounts, compile_candidates, level_counts, read_retail},
        objects::{
            OBJECT_COMPILER_VERSION, ObjectChunk, publish_object_atlas, publish_object_chunk,
            segment_cell_fit,
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
use shared::lod::{LodOrigin, object_atlas_path, object_chunk_payload_path};
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

/// What a successful run produced.
#[derive(Debug)]
pub struct DeriveObjectLodReport {
    pub receipt: PathBuf,
    /// Blocks found in the game data.
    pub blocks_found: usize,
    pub chunks: usize,
    pub triangles: usize,
    /// Counts per block level (16, 8, 4).
    pub levels: BTreeMap<u32, LevelCounts>,
    pub vertices: usize,
    pub skipped: Vec<SkippedBlock>,
    pub skipped_by_class: BTreeMap<&'static str, usize>,
    pub bytes_added: u64,
    pub atlas_ktx2_bytes: usize,
    pub elapsed_ms: u128,
}

/// What [`derive_into`] changed.
pub struct CoreReport {
    pub chunks: Vec<(ObjectChunk, String)>,
    pub skipped: Vec<SkippedBlock>,
    pub atlas_ktx2_bytes: usize,
    /// The shared atlas file and its SHA-256.
    pub atlas_path: Option<String>,
    pub atlas_file_sha256: Option<String>,
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
    let mut verify = |name: &str, path: &Path| -> Result<serde_json::Value> {
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
        Ok(serde_json::json!({ "archive": name, "sha256": actual, "matches_pack_manifest": true }))
    };
    let retail =
        read_retail(&options.data, &world_file, scratch.path(), &mut verify)?.ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "refused: the game data has no level 16, 8 or 4 object LOD blocks for `{world_file}`"
            )
        })?;
    let blocks_found = retail.blocks.len();
    let atlas_sha256 = hash_bytes(&retail.atlas_dds);

    let segment_check = retail
        .blocks
        .iter()
        .filter(|block| block.level == 4)
        .filter_map(|block| {
            block
                .parsed
                .as_ref()
                .ok()
                .map(|parsed| (block.cell, parsed))
        })
        .fold((0usize, 0usize), |(fitted, total), (cell, parsed)| {
            let (block_fit, block_total) = segment_cell_fit(parsed, cell);
            (fitted + block_fit, total + block_total)
        });
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

    let levels = level_counts(&core.chunks);
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
            "shared_file": core.atlas_path,
            "shared_file_sha256": core.atlas_file_sha256,
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
        "levels": levels.iter().map(|(level, counts)| (level.to_string(), serde_json::json!({
            "chunks": counts.chunks,
            "triangles": counts.triangles,
            "vertices": counts.vertices,
            "cell_pieces": counts.cell_pieces,
            "chunks_with_large_reference_data": counts.large_ref_chunks,
            "bytes": counts.bytes,
        }))).collect::<serde_json::Map<_, _>>(),
        "skipped_by_class": skipped_by_class,
        "skipped": skipped.iter().map(|block| serde_json::json!({
            "level": block.level,
            "cell": [block.cell.0, block.cell.1],
            "class": block.class,
            "detail": block.detail,
        })).collect::<Vec<_>>(),
        "lod_manifest_sha256": {
            "before": hash_bytes(&manifest_before),
            "after": hash_file(&target.join("lod-manifest.json"))?,
        },
        "verified": [
            "Level-4 segment i covers cell (x + i / 4, y + i % 4): verified in the retail loader (skyrim_decomp#816, static reading). Level-8 and level-16 shapes are whole shapes, not split per cell.",
        ],
        "level_4_segment_geometry_check": {
            "segments": segment_check.1,
            "within_0_75_cell_of_assigned_cell": segment_check.0,
            "misfits": segment_check.1 - segment_check.0,
            "note": "Mean vertex of each non-empty multi-segment slice against its assigned cell. A misfit is geometry that reaches beyond its cell (an inference); the research's own geometry test gave 6,698 of 6,708. The count is reported, not corrected.",
        },
        "unverified": [
            "Material: alpha mask 128/255 and single-sided unless node flags & 0x108 (reviewed static research, culling medium confidence).",
            "Vertex alpha is written as stored; whether retail multiplies it into the cutout is not verified.",
            "Large-reference data (levels 8 and 4) is kept undecoded and not applied.",
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
        levels,
        vertices,
        skipped,
        skipped_by_class,
        bytes_added: chunk_bytes + core.atlas_ktx2_bytes as u64 + text.len() as u64,
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
    // The existing texture stage turns the retail DDS into the KTX2 atlas the chunks share.
    let atlas = TextureConverter::convert(atlas_dds, TextureEncoding::ColorSrgb)
        .wrap_err("the object atlas could not be converted to KTX2")?;
    let metadata = inspect_ktx2(&atlas, TextureEncoding::ColorSrgb)?;
    let (chunks, skipped) = compile_candidates(world, candidates, atlas_sha256)?;
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
    let mut atlas_entry: Option<(String, String)> = None;
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
        // The atlas is written once for the world and every chunk references it.
        written.push(object_atlas_path(world.id));
        atlas_entry = Some(publish_object_atlas(&connection, target, world.id, &atlas)?);
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
    manifest
        .object_atlases
        .retain(|path, _| !path.starts_with(&world_prefix));
    if let Some((path, hash)) = &atlas_entry {
        manifest.object_atlases.insert(path.clone(), hash.clone());
    }
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
                connection.execute(
                    "DELETE FROM lod_object_atlases WHERE worldspace_id = ?1",
                    [world.id],
                )?;
            } else {
                connection.execute_batch(
                    "DROP TABLE IF EXISTS lod_object_chunks_spatial; DROP TABLE IF EXISTS lod_object_chunks; DROP TABLE IF EXISTS lod_object_atlases;",
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
        atlas_path: atlas_entry.as_ref().map(|(path, _)| path.clone()),
        atlas_file_sha256: atlas_entry.map(|(_, hash)| hash),
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
    use crate::lod::{
        objects::chunk_key_for_block,
        retail::bto::{ObjectLodFile, ObjectLodSegment, ObjectLodShape, ObjectLodVertex},
    };

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
            level: 16,
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
                object_atlases: BTreeMap::new(),
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
