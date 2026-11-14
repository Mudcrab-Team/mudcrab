//! Wires the merged-terrain outputs into a conversion (#215): after the world database and the
//! land textures exist, this builds the land texture arrays, one merged mesh per exterior cell,
//! and the terrain chunk files, into the staging folder, and reports what it did.
//!
//! This is the caller of the three terrain libraries: [`crate::terrain_arrays`] turns converted
//! land textures into KTX2 arrays plus `terrain/land-textures.json`,
//! [`crate::esm::terrain_mesh::build_cell_mesh`] turns one LAND record into a cell mesh, and
//! [`crate::terrain_chunks`] groups and writes the meshes. Every winning LTEX of the merged
//! records becomes an array source; each exterior cell with a grid square builds one mesh from
//! the one LAND it keeps — the LAND whose version comes from the plugin latest in the load
//! order ([`crate::esm::cell_cache::merged_terrain_lands`], the choice the cell cache uses too) — keyed
//! by [`crate::terrain_chunks::cell_mesh_key`] over the LAND, its plugin's master list and the
//! LTEX and TXST records the LAND names. The LANDs a cell drops are reported in
//! [`TerrainReport::warnings`].
//!
//! # What is left out, and what stops the run
//!
//! Interiors are skipped: a LAND whose cell has no grid square (or no cell record) is reported in
//! [`TerrainReport::skipped_cells`] with its reason and left out, as is a cell whose mesh fails to
//! build (`build_cell_mesh` refuses more than 16 slots per quadrant and malformed height or layer
//! data); the engine falls back for such a cell. A land texture the array builder cannot read is
//! skipped by name there and its cells fall back to the default texture. The chunk writer is the
//! last check: it refuses a cell written twice and a mesh it cannot validate, and either stops
//! the run — those are wiring bugs, not content.
//!
//! The option is off by default (`PipelineConfig::merged_terrain`): the engine half of #215 is
//! not in place yet, so nothing reads these outputs. When it is off neither the arrays nor the
//! chunk files are written.
//!
//! # What a run keeps for this stage
//!
//! The stage reads LAND, LTEX, TXST, CELL and WRLD records, so a conversion that asks for it
//! keeps exactly those out of the merge ([`terrain_records`]) as soon as the world database is
//! exported, rather than holding every record of the load order through its texture, mesh,
//! script and LOD stages.
//!
//! # The default texture
//!
//! Entry 0 of the table stands for a cell quadrant with no BTXT (or a null one). With `white`
//! (the default) that is the array builder's generated 4 x 4 white diffuse; with `skyrim-ini` it
//! is `[Landscape] sDefaultLandDiffuseTexture` and `sDefaultLandNormalTexture` from the
//! configured Skyrim.ini if a path to one is configured, else the built-in
//! `textures/landscape/Dirt02.dds` and `Dirt02_N.dds` (which also stand in for a key the ini does
//! not name). The report says which source was used and what came of it
//! ([`TerrainReport::default_texture_source`]): a `skyrim-ini` default whose diffuse cannot be
//! converted leaves entry 0 the generated white texture, and the report names that fallback
//! instead of a texture the run did not use. Entry 0's LTEX label is the lowest LTEX FormID whose
//! texture set names the resolved diffuse, or 0 when no LTEX names it.
//!
//! [`TerrainDefaultDiffuse`] resolves the same default for the LOD bake, so the ground keeps its
//! colour across the LOD switch distance.

use crate::{
    asset_path::{AssetKind, canonical_asset_path},
    cache::hash_bytes,
    config::{PipelineConfig, RecordReader, TerrainDefaultTexture},
    esm::{
        cell_cache::{extract_texture_layers, merged_terrain_lands},
        extractors::{SubrecordView, extract_cell_info},
        load_order::LoadOrder,
        records::RawRecord,
        terrain_mesh::{CellMeshReport, build_cell_mesh},
    },
    pipeline::PipelineReport,
    progress::{ProgressEvent, ProgressStage},
    terrain_arrays::{
        ArrayClassReport, ArrayOptions, DefaultTexture, LandTextureSource, SkippedTexture,
        TextureReport, build_land_texture_arrays,
    },
    terrain_chunks::{PlacedCellMesh, cell_mesh_key, write_terrain_chunks},
};
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, eyre},
};
use serde::{Deserialize, Serialize};
use shared::terrain_mesh::{
    DEFAULT_LAND_TEXTURE_FORM_ID, LAND_TEXTURE_TABLE_PATH, LandTextureDefault, LandTextureRequest,
    LandTextureSettings, TERRAIN_CHUNK_SIDE_CELLS, terrain_chunk_for_cell, terrain_chunk_path,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};
use tokio::{sync::mpsc::Sender, task::spawn_blocking};

/// The built-in `[Landscape]` defaults of Skyrim, used with `skyrim-ini` when no ini path is
/// configured or the ini does not name them.
const DIRT02_DIFFUSE: &str = "textures/landscape/Dirt02.dds";
const DIRT02_NORMAL: &str = "textures/landscape/Dirt02_N.dds";

/// The master lists of the plugins a conversion merged, by load-order priority slot: the input
/// [`build_merged_terrain`] keys every cell with.
pub type PluginMasters = BTreeMap<u32, Vec<String>>;

/// The record types [`build_merged_terrain`] reads: the LAND records and their cells, the land
/// textures and texture sets those name, and the worldspaces their cells sit in.
pub const TERRAIN_RECORD_TYPES: [&[u8; 4]; 5] = [b"LAND", b"LTEX", b"TXST", b"CELL", b"WRLD"];

/// Keeps only the records the merged-terrain stage reads out of the merge the world database was
/// exported from, dropping the rest as a run without the option does.
///
/// This runs right after the database stage, so the whole map — every record of every plugin — is
/// not held through the texture, mesh, script and LOD stages; the stage's own lookups are by
/// FormID, so the record's plugin master list (`RawRecord::load_order`) is carried along with it
/// and nothing else is needed.
pub fn terrain_records(merged: HashMap<u32, RawRecord>) -> HashMap<u32, RawRecord> {
    merged
        .into_iter()
        .filter(|(_, record)| TERRAIN_RECORD_TYPES.contains(&&record.record_type))
        .collect()
}

/// The native record positions the cell cache orders LANDs by, for the reader a conversion used.
/// The in-house reader writes them into the database it exports, so they are read back from it;
/// the legacy reader has none, so every LAND is ordered by load order and FormID alone.
pub fn cell_cache_source_offsets(
    reader: RecordReader,
    db_path: &Path,
) -> Result<HashMap<u32, u64>> {
    match reader {
        RecordReader::Inhouse => crate::esm::inhouse::terrain_source_offsets(db_path),
        RecordReader::Legacy => Ok(HashMap::new()),
    }
}

/// The conversion settings a merged-terrain build reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerrainOptions {
    /// Where entry 0 of the land texture table comes from.
    pub default_texture: TerrainDefaultTexture,
    /// The Skyrim.ini a `skyrim-ini` default is read from; `None` uses the built-in Dirt02 paths.
    pub terrain_ini: Option<PathBuf>,
    /// Whether a land texture larger than the largest size class is downscaled into it instead of
    /// stopping the build (`terrain_arrays::ArrayOptions::allow_downscale`).
    pub allow_downscale: bool,
}

impl TerrainOptions {
    /// The options one conversion's configuration selects.
    pub fn from_config(config: &PipelineConfig) -> Self {
        Self {
            default_texture: config.terrain_default_texture,
            terrain_ini: config.terrain_ini.clone(),
            allow_downscale: config.terrain_allow_downscale,
        }
    }
}

/// Where entry 0's diffuse comes from, for the LOD bake (`crate::pipeline::compile_lod_chunks`).
/// It reads the same setting and the same [`landscape_defaults`] as the merged-terrain arrays
/// ([`run_merged_terrain_stage`]), so a quadrant with no `BTXT` (or a null one) is painted the
/// same texture on both sides of the LOD switch distance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerrainDefaultDiffuse {
    /// The generated white diffuse: `TerrainDefaultTexture::White`, the default.
    GeneratedWhite,
    /// The DDS a `skyrim-ini` conversion selected, as a canonical Bethesda path relative to the
    /// converted output root: `vfs/<path>` holds the source.
    Image(String),
}

impl TerrainDefaultDiffuse {
    /// The default `options` select. `Err` is a configured Skyrim.ini that cannot be read (named
    /// in the error, as the merged stage reports it). A selected diffuse that cannot be a texture
    /// path falls back to the generated white one with its reason, which entry 0 does too.
    pub(crate) fn resolve(options: &TerrainOptions) -> Result<(Self, Option<String>)> {
        if options.default_texture == TerrainDefaultTexture::White {
            return Ok((Self::GeneratedWhite, None));
        }
        let defaults = landscape_defaults(options.terrain_ini.as_deref())?;
        Ok(
            match canonical_asset_path(&defaults.diffuse, AssetKind::Texture, "dds") {
                Ok(diffuse) => (Self::Image(diffuse), None),
                Err(error) => (
                    Self::GeneratedWhite,
                    Some(format!(
                        "the default land texture cannot be resolved: {error:#}"
                    )),
                ),
            },
        )
    }
}

/// The built-in `[Landscape]` default diffuse of Skyrim: what a `skyrim-ini` conversion names when
/// it has no Skyrim.ini, or the ini names no diffuse.
pub(crate) fn builtin_default_diffuse() -> Result<String> {
    canonical_asset_path(DIRT02_DIFFUSE, AssetKind::Texture, "dds")
}

/// The master lists of a load order, by priority slot (`LoadOrder::metadata[slot].masters`).
pub fn load_order_masters(order: &LoadOrder) -> PluginMasters {
    order
        .metadata
        .iter()
        .enumerate()
        .map(|(slot, metadata)| (slot as u32, metadata.masters.clone()))
        .collect()
}

/// Where entry 0's texture came from, for the report: the resolved outcome, not what the run was
/// asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum DefaultTextureSource {
    /// The generated white texture, because `white` was selected.
    White,
    /// The `skyrim-ini` defaults, converted: the ini they were read from (`None` for the built-in
    /// Dirt02 paths) and the converted textures entry 0 holds. `normal` is `None` when the ini
    /// named no normal map or its normal could not be converted.
    SkyrimIni {
        ini: Option<PathBuf>,
        diffuse: String,
        normal: Option<String>,
    },
    /// The `skyrim-ini` defaults were selected, but the diffuse could not be converted, so entry
    /// 0 is the generated white texture: the ini, the DDS paths it named, and why.
    SkyrimIniFallback {
        ini: Option<PathBuf>,
        diffuse: String,
        normal: Option<String>,
        reason: String,
    },
}

/// A land texture left out before the array builder saw it: its LTEX has no usable texture set,
/// so the LTEX has no table entry and its cells fall back to the default texture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedTerrainTexture {
    pub ltex_form_id: u32,
    /// The converted texture path the LTEX would have used, when one was resolved.
    pub source: Option<String>,
    pub reason: String,
}

/// A cell left out of the merged terrain, with its reason; the engine falls back for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedCell {
    pub cell_id: u32,
    /// The worldspace the LAND sits in, when it could be read.
    pub worldspace_id: Option<u32>,
    /// The cell's grid square, when it could be read.
    pub grid: Option<[i32; 2]>,
    pub reason: String,
}

/// One worldspace's share of the chunk files written, as the report serializes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldspaceChunks {
    pub worldspace_id: u32,
    /// Chunk files written for this worldspace.
    pub files: u64,
    /// Cell meshes written across those files.
    pub cells: u64,
}

/// Wall time of each step of the merged-terrain build, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerrainTimings {
    /// The land texture arrays and their table.
    pub arrays_ms: u128,
    /// One merged mesh per exterior cell.
    pub meshes_ms: u128,
    /// The terrain chunk files.
    pub chunks_ms: u128,
    pub total_ms: u128,
}

/// What one conversion's merged-terrain build did, for the conversion report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainReport {
    /// Where entry 0 of the table came from: the requested default texture, or the generated
    /// white one when the requested one could not be read.
    pub default_texture: DefaultTexture,
    /// Which default-texture source this run resolved.
    pub default_texture_source: DefaultTextureSource,
    /// One entry per array file: its class (usage, block format, size), layer and level counts
    /// and bytes.
    pub arrays: Vec<ArrayClassReport>,
    /// One row per texture placed in an array, with the action it took to get there.
    pub textures: Vec<TextureReport>,
    /// Every land texture left out, with its reason: this stage's own (an LTEX with no usable
    /// texture set) and the array builder's (a texture it could not read).
    pub skipped_textures: Vec<SkippedTerrainTexture>,
    /// Cells whose mesh was written.
    pub cells_written: u64,
    /// Cells with at least one quadrant holding more than 6 slots: the deliberate change from
    /// today's 6-layer cap.
    pub cells_over_six_slots: u64,
    /// Cells needing the 16-slot stride because a quadrant holds more than 8 slots.
    pub cells_needing_stride_16: u64,
    /// Cells with the default texture in slot 0 of a quadrant (no BTXT, a null one, or a BTXT
    /// whose LTEX has no table entry).
    pub cells_with_default_texture: u64,
    /// Layers the cell builds left out, across all cells; the cell meshes name which.
    pub skipped_layers: u64,
    /// ATXT assignments replaced by a later record for the same quadrant and layer index.
    pub overridden_layers: u64,
    /// Cells left out, with their reasons; the engine falls back for each.
    pub skipped_cells: Vec<SkippedCell>,
    /// The terrain chunk files written, relative to the output root.
    pub chunk_files: Vec<String>,
    /// Bytes written across those files.
    pub chunk_bytes: u64,
    /// One entry per worldspace, with its chunk and cell counts.
    pub worldspaces: Vec<WorldspaceChunks>,
    pub timings: TerrainTimings,
    /// The lines a front end should show: every skipped texture and cell, and every texture that
    /// was not copied block for block.
    pub warnings: Vec<String>,
}

/// Builds the merged terrain of one conversion into `staging`.
///
/// `merged` is the winning-record map the world database was exported from (a caller with the
/// option on keeps it with [`terrain_records`]) and `masters` the master lists of the same load
/// order (`load_order_masters`), so a cell's key covers exactly the records and plugin context its
/// mesh was built from. `options` selects entry 0's source and the array builder's settings (see
/// the module documentation).
///
/// The land texture arrays and the table are built first, so `known` — which LTEX a cell mesh may
/// use — is exactly the table's entries: a texture the array builder skipped leaves its cells
/// falling back to the default rather than naming a layer the table lacks. Nothing is dropped
/// silently: every skipped texture and cell is in the returned report, and every file written is
/// named in it so the caller can publish it.
pub fn build_merged_terrain(
    staging: &Path,
    merged: &HashMap<u32, RawRecord>,
    masters: &PluginMasters,
    source_offsets: &HashMap<u32, u64>,
    options: &TerrainOptions,
) -> Result<TerrainReport> {
    let started = Instant::now();

    // 1. The land texture arrays and their table.
    let arrays_started = Instant::now();
    let mut pre_skipped: Vec<SkippedTerrainTexture> = Vec::new();
    let mut sources: Vec<LandTextureSource> = Vec::new();
    for record in records_of(merged, b"LTEX") {
        match texture_paths(merged, record) {
            Ok((diffuse, normal)) => sources.push(LandTextureSource {
                ltex_form_id: record.form_id,
                diffuse: staging.join(diffuse),
                normal: normal.map(|normal| staging.join(normal)),
            }),
            Err(reason) => pre_skipped.push(SkippedTerrainTexture {
                ltex_form_id: record.form_id,
                source: None,
                reason,
            }),
        }
    }
    let defaults = match options.default_texture {
        TerrainDefaultTexture::White => None,
        TerrainDefaultTexture::SkyrimIni => {
            Some(landscape_defaults(options.terrain_ini.as_deref())?)
        }
    };
    // Taken from the same read that parsed the defaults, before any array is built.
    let ini_sha256 = defaults
        .as_ref()
        .and_then(|defaults| defaults.ini_sha256.clone());
    let (default, default_texture_source) =
        default_source(staging, merged, defaults.as_ref(), &mut pre_skipped)?;
    let array_options = ArrayOptions {
        allow_downscale: options.allow_downscale,
        ..ArrayOptions::default()
    };
    let (mut table, mut array_report) =
        build_land_texture_arrays(&sources, default, staging, &array_options)
            .wrap_err("failed to build the land texture arrays")?;
    let arrays_ms = arrays_started.elapsed().as_millis();

    // The table's entries are the textures a cell mesh may name; entry 0 (the default) is not an
    // LTEX a LAND can name, so it is not part of the set.
    let known: HashSet<u32> = table
        .entries
        .iter()
        .map(|entry| entry.ltex_form_id)
        .filter(|form_id| *form_id != DEFAULT_LAND_TEXTURE_FORM_ID)
        .collect();

    // 2. One merged mesh per exterior cell with a LAND record.
    let meshes_started = Instant::now();
    let mut placed: Vec<PlacedCellMesh> = Vec::new();
    let mut skipped_cells: Vec<SkippedCell> = Vec::new();
    let mut stats = CellStats::default();
    // One LAND per cell, chosen the way the cell cache chooses (the same native positions order
    // it): the dropped LANDs are reported with the stage's other warnings, and two LANDs for one
    // cell from the same load-order slot without positions are an error. The conversion has
    // already checked this before the texture and LOD stages, so an error here is a wiring bug.
    let (lands, land_warnings) = merged_terrain_lands(merged, source_offsets)?;
    for form_id in lands.values() {
        let land = &merged[form_id];
        match cell_mesh(merged, masters, land, &known) {
            Ok((cell, report)) => {
                stats.observe(&report);
                placed.push(cell);
            }
            Err(skipped) => skipped_cells.push(skipped),
        }
    }
    let meshes_ms = meshes_started.elapsed().as_millis();

    // 3. The chunk files, at the paths the engine reads.
    let chunks_started = Instant::now();
    let chunk_files = chunk_paths(&placed);
    let chunk_report =
        write_terrain_chunks(staging, placed).wrap_err("failed to write the terrain chunks")?;
    let chunks_ms = chunks_started.elapsed().as_millis();

    // The report. The array builder's source paths are the staging folder's; strip it so the
    // report does not name this machine's folders (and before `warnings`, which quotes them).
    for texture in &mut array_report.textures {
        if let Some(source) = &texture.source {
            texture.source = Some(relative_path(staging, Path::new(source)));
        }
    }
    for skipped in &mut array_report.skipped {
        skipped.source = relative_path(staging, Path::new(&skipped.source));
    }
    // The source was chosen before the arrays were built; say what survived building them.
    let default_texture_source = reconcile_default_source(default_texture_source, &array_report);
    // The arrays were built before the outcome was known, so the table is rewritten with the
    // settings it was built from: a stored table then says which default texture it holds.
    table.settings = Some(land_texture_settings(
        &default_texture_source,
        options,
        ini_sha256,
    ));
    let table_file = staging.join(LAND_TEXTURE_TABLE_PATH);
    fs::write(&table_file, serde_json::to_vec_pretty(&table)?)
        .wrap_err_with(|| format!("failed to rewrite {}", table_file.display()))?;
    let mut warnings = land_warnings;
    warnings.extend(array_report.warnings());
    let default_fell_back = matches!(
        default_texture_source,
        DefaultTextureSource::SkyrimIniFallback { .. }
    );
    for skipped in &pre_skipped {
        if skipped.ltex_form_id == DEFAULT_LAND_TEXTURE_FORM_ID {
            warnings.push(default_skip_warning(&skipped.reason, default_fell_back));
            continue;
        }
        warnings.push(format!(
            "land texture {:08X} has no array entry, its cells fall back: {}",
            skipped.ltex_form_id, skipped.reason
        ));
    }
    for cell in &skipped_cells {
        warnings.push(format!(
            "cell {:08X}{} left out of the merged terrain, the engine falls back: {}",
            cell.cell_id,
            cell.worldspace_id
                .map(|worldspace| format!(" of worldspace {worldspace:08X}"))
                .unwrap_or_default(),
            cell.reason
        ));
    }
    let mut skipped_textures = pre_skipped;
    skipped_textures.extend(array_report.skipped.iter().map(skipped_from_array));

    Ok(TerrainReport {
        default_texture: array_report.default_texture,
        default_texture_source,
        arrays: array_report.arrays,
        textures: array_report.textures,
        skipped_textures,
        cells_written: chunk_report.cells as u64,
        cells_over_six_slots: stats.cells_over_six_slots,
        cells_needing_stride_16: stats.cells_needing_stride_16,
        cells_with_default_texture: stats.cells_with_default_texture,
        skipped_layers: stats.skipped_layers,
        overridden_layers: stats.overridden_layers,
        skipped_cells,
        chunk_files,
        chunk_bytes: chunk_report.bytes,
        worldspaces: chunk_report
            .worldspaces
            .iter()
            .map(|worldspace| WorldspaceChunks {
                worldspace_id: worldspace.worldspace_id,
                files: worldspace.files as u64,
                cells: worldspace.cells as u64,
            })
            .collect(),
        timings: TerrainTimings {
            arrays_ms,
            meshes_ms,
            chunks_ms,
            total_ms: started.elapsed().as_millis(),
        },
        warnings,
    })
}

/// Runs the merged-terrain stage for one route that has just built its world database: the
/// records, the load order's masters, the publish list and the report, around
/// [`build_merged_terrain`].
///
/// `records` is the merged record map the database was exported from ([`terrain_records`] for a
/// caller that kept only what this stage reads); the option being on without one (no plugin
/// produced a world database) is an error naming `--merged-terrain`, never a silent skip. The
/// build is heavy CPU and disk work, so it runs on the blocking pool rather than on the async
/// runtime, as the LOD stage keeps its compilers off it. A stop cannot be honoured inside the
/// build; it is checked by the caller at its next safe point.
pub async fn run_merged_terrain_stage(
    staging: &Path,
    records: Option<HashMap<u32, RawRecord>>,
    source_offsets: HashMap<u32, u64>,
    plugins: &[PathBuf],
    options: &TerrainOptions,
    progress_tx: &Sender<ProgressEvent>,
    report: &mut PipelineReport,
) -> Result<()> {
    let Some(records) = records else {
        bail!(
            "--merged-terrain was requested, but no plugin produced a world database for the \
             merged terrain to be built from"
        );
    };
    let started = Instant::now();
    let _ = progress_tx
        .send(ProgressEvent::new(
            ProgressStage::LodChunks,
            0,
            1,
            None,
            "Building merged terrain",
        ))
        .await;
    let order = LoadOrder::read(plugins)
        .wrap_err("failed to read the load order for the merged terrain")?;
    let masters = load_order_masters(&order);
    let staging = staging.to_path_buf();
    let options = options.clone();
    let terrain = spawn_blocking(move || {
        build_merged_terrain(&staging, &records, &masters, &source_offsets, &options)
    })
    .await
    .wrap_err("the merged terrain worker panicked")??;
    // Everything the stage wrote is published by name, as the LOD stage does with its payloads.
    report
        .artifacts
        .push(PathBuf::from(LAND_TEXTURE_TABLE_PATH));
    report.artifacts.extend(
        terrain
            .arrays
            .iter()
            .map(|array| PathBuf::from(&array.path)),
    );
    report
        .artifacts
        .extend(terrain.chunk_files.iter().map(PathBuf::from));
    report.terrain_warnings = terrain.warnings.clone();
    report.terrain_elapsed_ms = started.elapsed().as_millis();
    report.terrain = Some(terrain);
    let _ = progress_tx
        .send(ProgressEvent::new(
            ProgressStage::LodChunks,
            1,
            1,
            None,
            "Built merged terrain",
        ))
        .await;
    Ok(())
}

/// The #185 derived-data key of one cell's merged terrain, exactly as a conversion computes it.
///
/// The key hashes the LAND record, the master list of the plugin that declared it
/// (`masters[record.load_order]`), and every LTEX the LAND names together with the TXST each
/// names — the LTEX and TXST only when the load order has them: a missing LTEX or TXST is not a
/// record of its own, and contributes only its FormID inside the LAND bytes, so a later record
/// for it (or its removal) changes the key through those bytes. A malformed LAND record is an
/// error, not a key.
pub fn cell_key(
    merged: &HashMap<u32, RawRecord>,
    masters: &PluginMasters,
    land: &RawRecord,
    known: &HashSet<u32>,
) -> Result<[u8; 32]> {
    let land_masters = masters_of(masters, land)?;
    let (layers, _) = extract_texture_layers(&land.subrecords)
        .wrap_err_with(|| format!("invalid LAND layers for cell record {:08X}", land.form_id))?;
    let mut named: BTreeSet<u32> = BTreeSet::new();
    for layer in &layers {
        if layer.texture_form_id != DEFAULT_LAND_TEXTURE_FORM_ID {
            named.insert(layer.texture_form_id);
        }
    }
    // The named LTEX records the table has no entry for: the mesh skips those layers.
    let skipped: Vec<u32> = named
        .iter()
        .copied()
        .filter(|form_id| !known.contains(form_id))
        .collect();
    let mut textures: Vec<(&RawRecord, &[String])> = Vec::new();
    for form_id in named {
        let Some(ltex) = merged
            .get(&form_id)
            .filter(|record| record.record_type == *b"LTEX")
        else {
            continue;
        };
        textures.push((ltex, masters_of(masters, ltex)?));
        let Some(txst_id) = SubrecordView::new(&ltex.subrecords)
            .get_form_id(b"TNAM")
            .filter(|form_id| *form_id != 0)
        else {
            continue;
        };
        if let Some(txst) = merged
            .get(&txst_id)
            .filter(|record| record.record_type == *b"TXST")
        {
            textures.push((txst, masters_of(masters, txst)?));
        }
    }
    Ok(cell_mesh_key(land, land_masters, &textures, &skipped))
}

/// One cell's mesh and placement, or why it was left out.
fn cell_mesh(
    merged: &HashMap<u32, RawRecord>,
    masters: &PluginMasters,
    land: &RawRecord,
    known: &HashSet<u32>,
) -> std::result::Result<(PlacedCellMesh, CellMeshReport), SkippedCell> {
    let cell_id = land.cell_form_id.unwrap_or(land.form_id);
    let skip = |grid: Option<[i32; 2]>, reason: String| SkippedCell {
        cell_id,
        worldspace_id: land.worldspace_form_id,
        grid,
        reason,
    };
    let Some(cell) = merged
        .get(&cell_id)
        .filter(|record| record.record_type == *b"CELL")
    else {
        return Err(skip(
            None,
            format!("its cell {cell_id:08X} is not in the load order"),
        ));
    };
    let (grid_x, grid_y, _) = extract_cell_info(&cell.subrecords);
    let (Some(grid_x), Some(grid_y)) = (grid_x, grid_y) else {
        return Err(skip(
            None,
            "its cell has no grid square (an interior cell has no LAND terrain)".to_string(),
        ));
    };
    let grid = Some([grid_x, grid_y]);
    let Some(worldspace_id) = cell.worldspace_form_id.or(land.worldspace_form_id) else {
        return Err(skip(grid, "its cell sits in no worldspace".to_string()));
    };
    let key = cell_key(merged, masters, land, known).map_err(|error| {
        skip(
            grid,
            format!("its derived-data key cannot be computed: {error:#}"),
        )
    })?;
    let (mesh, report) = build_cell_mesh(cell_id, land, known, key)
        .map_err(|error| skip(grid, format!("its mesh cannot be built: {error:#}")))?;
    Ok((
        PlacedCellMesh {
            worldspace_id,
            grid_x,
            grid_y,
            mesh,
        },
        report,
    ))
}

/// What the cell builds left out and changed, summed over every cell.
#[derive(Default)]
struct CellStats {
    cells_over_six_slots: u64,
    cells_needing_stride_16: u64,
    cells_with_default_texture: u64,
    skipped_layers: u64,
    overridden_layers: u64,
}

impl CellStats {
    fn observe(&mut self, report: &CellMeshReport) {
        self.cells_over_six_slots += u64::from(!report.quadrants_over_six_slots.is_empty());
        self.cells_needing_stride_16 += u64::from(report.needed_stride_16);
        self.cells_with_default_texture +=
            u64::from(!report.quadrants_with_default_texture.is_empty());
        self.skipped_layers += report.skipped_layers.len() as u64;
        self.overridden_layers += report.overridden_layers.len() as u64;
    }
}

/// Every winning record of one type, by FormID so the build order never depends on the map's.
fn records_of<'a>(
    merged: &'a HashMap<u32, RawRecord>,
    record_type: &[u8; 4],
) -> Vec<&'a RawRecord> {
    let mut records: Vec<&RawRecord> = merged
        .values()
        .filter(|record| record.record_type == *record_type)
        .collect();
    records.sort_unstable_by_key(|record| record.form_id);
    records
}

/// One record's plugin master list, by the priority slot the merge recorded on it.
fn masters_of<'a>(masters: &'a PluginMasters, record: &RawRecord) -> Result<&'a [String]> {
    masters
        .get(&record.load_order)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            eyre!(
                "{} record {:08X} came from load-order slot {}, which the load order does not have",
                String::from_utf8_lossy(&record.record_type),
                record.form_id,
                record.load_order
            )
        })
}

/// The converted diffuse and, when the texture set has one, normal texture of one LTEX, as
/// paths relative to the output root. An `Err` is why the LTEX has no usable texture set.
fn texture_paths(
    merged: &HashMap<u32, RawRecord>,
    record: &RawRecord,
) -> std::result::Result<(String, Option<String>), String> {
    let view = SubrecordView::new(&record.subrecords);
    let Some(txst_id) = view.get_form_id(b"TNAM").filter(|form_id| *form_id != 0) else {
        return Err("it names no texture set (no TNAM)".to_string());
    };
    let Some(txst) = merged
        .get(&txst_id)
        .filter(|record| record.record_type == *b"TXST")
    else {
        return Err(format!(
            "its texture set {txst_id:08X} is not in the load order"
        ));
    };
    let view = SubrecordView::new(&txst.subrecords);
    let diffuse = view.get_string(b"TX00").unwrap_or_default();
    if diffuse.trim().is_empty() {
        return Err(format!(
            "its texture set {txst_id:08X} names no diffuse texture (no TX00)"
        ));
    }
    let diffuse = converted_texture_path(&diffuse, "diffuse")?;
    let normal = view.get_string(b"TX01").unwrap_or_default();
    let normal = if normal.trim().is_empty() {
        None
    } else {
        Some(converted_texture_path(&normal, "normal")?)
    };
    Ok((diffuse, normal))
}

/// One texture path as the converted `.ktx2` path the pipeline writes, or why it cannot be one.
fn converted_texture_path(path: &str, usage: &str) -> std::result::Result<String, String> {
    canonical_asset_path(path, AssetKind::Texture, "ktx2").map_err(|error| {
        format!("its {usage} texture {path} cannot be a converted path: {error:#}")
    })
}

/// Entry 0's source for the resolved `skyrim-ini` defaults — `None` for the array builder's
/// generated white one — with what was actually resolved, for the report. A default that cannot
/// be resolved is named in `skipped` and leaves entry 0 the white texture, as the array builder
/// does with a default it cannot read at planning time; the report says so rather than naming a
/// source this run did not use.
fn default_source(
    staging: &Path,
    merged: &HashMap<u32, RawRecord>,
    defaults: Option<&LandscapeDefaults>,
    skipped: &mut Vec<SkippedTerrainTexture>,
) -> Result<(Option<LandTextureSource>, DefaultTextureSource)> {
    let Some(defaults) = defaults else {
        return Ok((None, DefaultTextureSource::White));
    };
    let diffuse = match converted_texture_path(&defaults.diffuse, "default diffuse") {
        Ok(diffuse) => diffuse,
        Err(reason) => {
            let reason = format!("the default land texture cannot be resolved: {reason}");
            skipped.push(SkippedTerrainTexture {
                ltex_form_id: DEFAULT_LAND_TEXTURE_FORM_ID,
                source: Some(defaults.diffuse.clone()),
                reason: reason.clone(),
            });
            let normal = (!defaults.normal.trim().is_empty()).then(|| defaults.normal.clone());
            return Ok((
                None,
                DefaultTextureSource::SkyrimIniFallback {
                    ini: defaults.ini.clone(),
                    diffuse: defaults.diffuse.clone(),
                    normal,
                    reason,
                },
            ));
        }
    };
    let mut normal = if defaults.normal.trim().is_empty() {
        None
    } else {
        match converted_texture_path(&defaults.normal, "default normal") {
            Ok(normal) => Some(normal),
            Err(reason) => {
                skipped.push(SkippedTerrainTexture {
                    ltex_form_id: DEFAULT_LAND_TEXTURE_FORM_ID,
                    source: Some(defaults.normal.clone()),
                    reason: format!("the default normal texture cannot be resolved: {reason}"),
                });
                None
            }
        }
    };
    // The label is the lowest LTEX that names the resolved diffuse texture; 0 (the default slot)
    // when no LTEX does, because then the texture is a file with no record of its own.
    let ltex_form_id = records_of(merged, b"LTEX")
        .into_iter()
        .find(|record| texture_paths(merged, record).is_ok_and(|(own, _)| own == diffuse))
        .map_or(DEFAULT_LAND_TEXTURE_FORM_ID, |record| record.form_id);
    let source = LandTextureSource {
        ltex_form_id,
        diffuse: staging.join(&diffuse),
        normal: normal.as_ref().map(|normal| staging.join(normal)),
    };
    Ok((
        Some(source),
        DefaultTextureSource::SkyrimIni {
            ini: defaults.ini.clone(),
            diffuse,
            normal: normal.take(),
        },
    ))
}

/// What the default texture really is once the arrays are built. [`default_source`] decides from
/// the paths alone, before the array builder opens a file: a converted default that is missing or
/// unreadable makes the builder fall back to the generated white texture, and an unreadable normal
/// map is dropped. The report must say so, or it would claim a `skyrim-ini` default that is not in
/// the arrays. `report` has its source paths relative to the staging folder, as the report's do.
fn reconcile_default_source(
    source: DefaultTextureSource,
    report: &crate::terrain_arrays::ArrayReport,
) -> DefaultTextureSource {
    let DefaultTextureSource::SkyrimIni {
        ini,
        diffuse,
        normal,
    } = source
    else {
        return source;
    };
    let skipped_for = |path: &str| report.skipped.iter().find(|skipped| skipped.source == path);
    if matches!(report.default_texture, DefaultTexture::White) {
        let reason = skipped_for(&diffuse).map_or_else(
            || "the converted default land texture could not be used".to_owned(),
            |skipped| {
                format!(
                    "the default land texture was left out of the arrays: {}",
                    skipped.reason
                )
            },
        );
        return DefaultTextureSource::SkyrimIniFallback {
            ini,
            diffuse,
            normal,
            reason,
        };
    }
    let normal = normal.filter(|normal| skipped_for(normal).is_none());
    DefaultTextureSource::SkyrimIni {
        ini,
        diffuse,
        normal,
    }
}

/// The settings a merged-terrain build's land texture table records: what was requested, what entry
/// 0 holds once the arrays are built, the converted `.ktx2` textures it holds (only when they are
/// the ini's own), the SHA-256 of the Skyrim.ini bytes the defaults were parsed from, and the
/// downscale allowance. `ini_sha256` is the hash taken at parse time and is never re-read here.
/// The raw DDS paths a run asked for stay in the report, not in these settings.
pub(crate) fn land_texture_settings(
    source: &DefaultTextureSource,
    options: &TerrainOptions,
    ini_sha256: Option<String>,
) -> LandTextureSettings {
    let request = match options.default_texture {
        TerrainDefaultTexture::White => LandTextureRequest::White,
        TerrainDefaultTexture::SkyrimIni => LandTextureRequest::SkyrimIni,
    };
    let (default, diffuse, normal) = match source {
        DefaultTextureSource::White => (LandTextureDefault::GeneratedWhite, None, None),
        DefaultTextureSource::SkyrimIni {
            diffuse, normal, ..
        } => (
            LandTextureDefault::SkyrimIni,
            Some(diffuse.clone()),
            normal.clone(),
        ),
        DefaultTextureSource::SkyrimIniFallback { .. } => {
            (LandTextureDefault::SkyrimIniFallbackWhite, None, None)
        }
    };
    let terrain_ini_sha256 = match source {
        DefaultTextureSource::White => None,
        DefaultTextureSource::SkyrimIni { .. } | DefaultTextureSource::SkyrimIniFallback { .. } => {
            ini_sha256
        }
    };
    LandTextureSettings {
        request,
        default,
        default_diffuse: diffuse,
        default_normal: normal,
        terrain_ini_sha256,
        allow_downscale: options.allow_downscale,
    }
}

/// The warning for an entry-0 source that could not be used. Entry 0 is the generated white texture
/// only when the default diffuse fell back, which the default source already reports; a skipped
/// default normal map leaves the diffuse in place, so the warning must not claim the white one.
fn default_skip_warning(reason: &str, default_fell_back: bool) -> String {
    if default_fell_back {
        format!("the default land texture falls back to the generated white one: {reason}")
    } else {
        format!("the default land normal map is left out, the default diffuse stays: {reason}")
    }
}

/// The `[Landscape]` defaults a `skyrim-ini` run resolved, still as Bethesda DDS paths.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LandscapeDefaults {
    /// The ini they were read from; `None` for the built-in Dirt02 paths.
    ini: Option<PathBuf>,
    /// SHA-256 of the bytes the paths were parsed from, taken from that same read: the identity
    /// the settings record, so a later change to the file cannot alter what the run recorded.
    ini_sha256: Option<String>,
    diffuse: String,
    normal: String,
}

/// The `[Landscape]` default diffuse and normal paths of a Skyrim.ini, or the built-in Dirt02
/// paths when no ini is configured or the ini does not name them. A configured ini that cannot
/// be read is an error naming the file.
fn landscape_defaults(ini_path: Option<&Path>) -> Result<LandscapeDefaults> {
    let Some(path) = ini_path else {
        return Ok(LandscapeDefaults {
            ini: None,
            ini_sha256: None,
            diffuse: DIRT02_DIFFUSE.to_string(),
            normal: DIRT02_NORMAL.to_string(),
        });
    };
    let bytes = fs::read(path).wrap_err_with(|| {
        format!(
            "failed to read the Skyrim.ini {} the default land texture should come from",
            path.display()
        )
    })?;
    let mut section = String::new();
    let mut diffuse: Option<String> = None;
    let mut normal: Option<String> = None;
    for line in decode_ini(&bytes).lines() {
        let line = line.trim();
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        if section != "landscape" || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim().to_string();
        match key.trim().to_ascii_lowercase().as_str() {
            "sdefaultlanddiffusetexture" => diffuse = Some(value),
            "sdefaultlandnormaltexture" => normal = Some(value),
            _ => {}
        }
    }
    let intact = |value: Option<String>, fallback: &str| {
        value
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| fallback.to_string())
    };
    Ok(LandscapeDefaults {
        ini: Some(path.to_path_buf()),
        ini_sha256: Some(hash_bytes(&bytes)),
        diffuse: intact(diffuse, DIRT02_DIFFUSE),
        normal: intact(normal, DIRT02_NORMAL),
    })
}

/// A Skyrim.ini's text: UTF-16LE when it carries the byte order mark, else UTF-8 with the
/// invalid sequences replaced, as the game's own file is ANSI.
fn decode_ini(bytes: &[u8]) -> String {
    match bytes.strip_prefix(&[0xFF, 0xFE]) {
        Some(utf16) => String::from_utf16_lossy(
            &utf16
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect::<Vec<_>>(),
        ),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// One array-builder skip as this stage's own kind.
fn skipped_from_array(skipped: &SkippedTexture) -> SkippedTerrainTexture {
    SkippedTerrainTexture {
        ltex_form_id: skipped.ltex_form_id,
        source: Some(skipped.source.clone()),
        reason: skipped.reason.clone(),
    }
}

/// The chunk file of every group the cells land in, the same path rule the chunk writer uses.
fn chunk_paths(cells: &[PlacedCellMesh]) -> Vec<String> {
    let groups: BTreeSet<(u32, i32, i32)> = cells
        .iter()
        .map(|cell| {
            let (chunk_x, chunk_y) = terrain_chunk_for_cell(cell.grid_x, cell.grid_y);
            (cell.worldspace_id, chunk_x, chunk_y)
        })
        .collect();
    groups
        .into_iter()
        .map(|(worldspace_id, chunk_x, chunk_y)| {
            terrain_chunk_path(
                worldspace_id,
                chunk_x * TERRAIN_CHUNK_SIDE_CELLS,
                chunk_y * TERRAIN_CHUNK_SIDE_CELLS,
            )
        })
        .collect()
}

/// `path` relative to `root` when it sits inside it, else unchanged.
fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::esm::EsmParser;

    fn record(record_type: [u8; 4], form_id: u32, subrecords: &[(&[u8; 4], Vec<u8>)]) -> RawRecord {
        RawRecord {
            form_id,
            record_type,
            flags: 0,
            subrecords: subrecords
                .iter()
                .map(|(tag, data)| (tag.to_vec(), data.clone()))
                .collect(),
            cell_form_id: None,
            worldspace_form_id: None,
            load_order: 0,
        }
    }

    /// A LAND whose four quadrants all name `texture`, in a cell and worldspace.
    fn land(cell: u32, worldspace: u32, texture: u32) -> RawRecord {
        land_at_height(cell, worldspace, texture, 0.0)
    }

    /// The same, flat except for a VHGT offset that discriminates which LAND a mesh was built
    /// from (a whole-cell offset decodes to `offset * 8` everywhere).
    fn land_at_height(cell: u32, worldspace: u32, texture: u32, offset: f32) -> RawRecord {
        let side = usize::from(shared::LAND_SIDE);
        let mut vhgt = offset.to_le_bytes().to_vec();
        vhgt.extend(std::iter::repeat_n(0u8, side * side));
        vhgt.extend([0u8; 3]);
        let mut subrecords: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"VHGT", vhgt)];
        for quadrant in 0u8..4 {
            let mut btxt = texture.to_le_bytes().to_vec();
            btxt.extend([quadrant, 0]);
            btxt.extend(0u16.to_le_bytes());
            subrecords.push((b"BTXT", btxt));
        }
        let mut record = record(*b"LAND", cell + 1, &subrecords);
        record.cell_form_id = Some(cell);
        record.worldspace_form_id = Some(worldspace);
        record
    }

    /// An exterior cell at one grid square of a worldspace.
    fn exterior_cell(form_id: u32, worldspace: u32, grid: [i32; 2]) -> RawRecord {
        let mut grid_bytes = grid[0].to_le_bytes().to_vec();
        grid_bytes.extend(grid[1].to_le_bytes());
        let mut cell = record(*b"CELL", form_id, &[(b"XCLC", grid_bytes)]);
        cell.worldspace_form_id = Some(worldspace);
        cell
    }

    fn ltex(form_id: u32, texture: &str, txst: u32) -> RawRecord {
        record(
            *b"LTEX",
            form_id,
            &[
                (b"TNAM", txst.to_le_bytes().to_vec()),
                (b"ICON", texture.as_bytes().to_vec()),
            ],
        )
    }

    fn txst(form_id: u32, diffuse: &str, normal: &str) -> RawRecord {
        record(
            *b"TXST",
            form_id,
            &[
                (b"TX00", diffuse.as_bytes().to_vec()),
                (b"TX01", normal.as_bytes().to_vec()),
            ],
        )
    }

    /// One plugin slot with its master list.
    fn masters(slot: u32, names: &[&str]) -> PluginMasters {
        BTreeMap::from([(slot, names.iter().map(|name| name.to_string()).collect())])
    }

    fn merged(records: &[RawRecord]) -> HashMap<u32, RawRecord> {
        records
            .iter()
            .map(|record| (record.form_id, record.clone()))
            .collect()
    }

    /// One subrecord as a plugin file holds it.
    fn sub(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
        [tag.as_slice(), &(bytes.len() as u16).to_le_bytes(), bytes].concat()
    }

    /// One record as a plugin file holds it, from the subrecords the builders above made.
    fn record_bytes(record: &RawRecord) -> Vec<u8> {
        let payload: Vec<u8> = record
            .subrecords
            .iter()
            .flat_map(|(tag, data)| sub(tag.as_slice().try_into().unwrap(), data))
            .collect();
        [
            record.record_type.as_slice(),
            &(payload.len() as u32).to_le_bytes(),
            &0u32.to_le_bytes(),
            &record.form_id.to_le_bytes(),
            &[0; 8],
            &payload,
        ]
        .concat()
    }

    /// Writes a plugin carrying one cell's LAND, LTEX and TXST records, with `masters` in its
    /// header, and returns its path.
    fn terrain_plugin(root: &Path, name: &str, masters: &[&str]) -> PathBuf {
        let mut header = Vec::new();
        for master in masters {
            header.extend(sub(b"MAST", format!("{master}\0").as_bytes()));
            header.extend(sub(b"DATA", &[0; 8]));
        }
        let mut bytes = [
            b"TES4".as_slice(),
            &(header.len() as u32).to_le_bytes(),
            &0u32.to_le_bytes(),
            &0u32.to_le_bytes(),
            &[0; 8],
            &header,
        ]
        .concat();
        for record in [
            land(0x1000, 0x003C, 0x100),
            ltex(0x100, "textures\\landscape\\fieldgrass01.dds", 0x200),
            txst(
                0x200,
                "textures\\landscape\\fieldgrass01.dds",
                "textures\\landscape\\fieldgrass01_n.dds",
            ),
        ] {
            bytes.extend(record_bytes(&record));
        }
        let path = root.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    /// The one LAND of `merged` that the plugin in `slot` declared.
    fn land_of_slot(merged: &HashMap<u32, RawRecord>, slot: u32) -> RawRecord {
        merged
            .values()
            .find(|record| record.record_type == *b"LAND" && record.load_order == slot)
            .unwrap_or_else(|| panic!("no LAND from load-order slot {slot}"))
            .clone()
    }

    /// The LAND of `merged` with `form_id`, whatever plugin slot it came from.
    fn land_of_form_id(merged: &HashMap<u32, RawRecord>, form_id: u32) -> RawRecord {
        merged
            .get(&form_id)
            .filter(|record| record.record_type == *b"LAND")
            .unwrap_or_else(|| panic!("no LAND {form_id:08X} in the merge"))
            .clone()
    }

    #[test]
    fn only_the_records_the_stage_reads_are_kept() {
        // The fixture conversion's merge, as `pipeline.rs` and `metadata.rs` hand it to the
        // filter: what the stage keeps out of it is what it reads, and nothing else is held
        // through the texture, mesh, script and LOD stages.
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("Data");
        dummy_content::layout::prepare_directory(&data, false).unwrap();
        let formats = dummy_content::layout::Formats::parse("dds,pex,nif,esm").unwrap();
        let written =
            dummy_content::layout::generate(&data, dummy_content::layout::DEFAULT_SEED, formats)
                .unwrap();
        let plugins: Vec<PathBuf> = written
            .into_iter()
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| matches!(extension, "esm" | "esp" | "esl"))
            })
            .collect();
        assert!(!plugins.is_empty(), "the fixture writes a plugin");
        let all =
            EsmParser::convert_plugins_with_records(&plugins, &directory.path().join("world.db"))
                .unwrap();
        let total = all.len();
        let kept = terrain_records(all);
        let dropped = total - kept.len();
        println!(
            "merged terrain keeps {} of {total} merged fixture records ({dropped} dropped)",
            kept.len()
        );
        assert!(
            dropped > 0,
            "the merge holds records the stage does not read"
        );
        assert!(
            kept.values()
                .all(|record| TERRAIN_RECORD_TYPES.contains(&&record.record_type)),
            "only the five record types the stage reads survive the filter"
        );
        for record_type in TERRAIN_RECORD_TYPES {
            let count = kept
                .values()
                .filter(|record| record.record_type == *record_type)
                .count();
            println!("  {}: {count}", String::from_utf8_lossy(record_type));
            assert!(
                count > 0,
                "the fixture has {} records",
                String::from_utf8_lossy(record_type)
            );
        }
        // A record type the stage never reads is dropped even when it is the only one.
        let unrelated = merged(&[record(*b"STAT", 0x300, &[])]);
        assert!(terrain_records(unrelated).is_empty());
    }

    /// Two plugins add a LAND to one exterior cell: the stage builds one mesh, from the later
    /// plugin's LAND, and reports the dropped one — the same choice the cell cache makes.
    #[test]
    fn two_lands_for_one_cell_build_one_mesh_from_the_later_plugin() {
        // The two LANDs hold different terrain (a VHGT offset of 0.0 vs 1.0, decoding to
        // heights of 0.0 vs 8.0), so the mesh names its LAND.
        let mut raised = land_at_height(0x1000, 0x3C, 0x100, 1.0);
        raised.form_id = 0x0100_1001;
        for (flat_slot, raised_slot) in [(0, 1), (1, 0)] {
            let mut flat = land_at_height(0x1000, 0x3C, 0x100, 0.0);
            flat.load_order = flat_slot;
            raised.load_order = raised_slot;
            let records = merged(&[exterior_cell(0x1000, 0x3C, [-44, 37]), flat, raised.clone()]);
            let masters = BTreeMap::from([
                (0, Vec::<String>::new()),
                (1, vec!["Skyrim.esm".to_string()]),
            ]);
            let staging = tempfile::tempdir().unwrap();
            let report = build_merged_terrain(
                staging.path(),
                &records,
                &masters,
                &HashMap::new(),
                &TerrainOptions::default(),
            )
            .unwrap();
            assert_eq!(report.cells_written, 1, "one mesh for the cell");
            let chunk = crate::terrain_chunks::read_terrain_chunk(
                &staging.path().join(&report.chunk_files[0]),
            )
            .unwrap();
            assert_eq!(chunk.cells.len(), 1);
            let expected = if raised_slot == 1 { 8.0 } else { 0.0 };
            assert!(
                chunk.cells[0]
                    .heights
                    .iter()
                    .all(|height| *height == expected),
                "the mesh is the slot {raised_slot} LAND's terrain"
            );
            let warning = report
                .warnings
                .iter()
                .find(|warning| warning.contains("cell 00001000"))
                .expect("the dropped LAND is reported");
            let (kept, dropped, kept_slot, dropped_slot) = if raised_slot == 1 {
                ("01001001", "00001001", 1, 0)
            } else {
                ("00001001", "01001001", 1, 0)
            };
            assert!(
                warning.contains(&format!("LAND {kept} from load-order slot {kept_slot}"))
                    && warning.contains(&format!(
                        "dropped LAND {dropped} from load-order slot {dropped_slot}"
                    )),
                "{warning}"
            );
        }
    }

    /// The table's settings identify the default texture and every input that changes its bytes:
    /// a different request, source, ini file contents, or downscale allowance is a different table,
    /// and the built-in Dirt02 paths carry no ini hash.
    #[test]
    fn table_settings_identify_the_default_source_and_its_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let ini_a = dir.path().join("a.ini");
        let ini_b = dir.path().join("b.ini");
        fs::write(&ini_a, "[Landscape]\nsDefaultLandDiffuseTexture=a.dds\n").unwrap();
        fs::write(&ini_b, "[Landscape]\nsDefaultLandDiffuseTexture=b.dds\n").unwrap();
        let skyrim = |ini: Option<PathBuf>| DefaultTextureSource::SkyrimIni {
            ini,
            diffuse: "textures/landscape/dirt02.ktx2".into(),
            normal: Some("textures/landscape/dirt02_n.ktx2".into()),
        };
        let options = |texture, downscale| TerrainOptions {
            default_texture: texture,
            allow_downscale: downscale,
            ..TerrainOptions::default()
        };
        let skyrim_options = options(TerrainDefaultTexture::SkyrimIni, false);
        let hash_of = |path: &Path| Some(hash_bytes(&fs::read(path).unwrap()));
        let white = land_texture_settings(
            &DefaultTextureSource::White,
            &options(TerrainDefaultTexture::White, false),
            None,
        );
        assert_eq!(white.request, LandTextureRequest::White);
        assert_eq!(white.default, LandTextureDefault::GeneratedWhite);
        assert_eq!(white.default_diffuse, None);
        assert_eq!(white.terrain_ini_sha256, None);

        let built_in = land_texture_settings(&skyrim(None), &skyrim_options, None);
        assert_eq!(
            built_in.terrain_ini_sha256, None,
            "built-in paths name no ini"
        );
        let from_a = land_texture_settings(
            &skyrim(Some(ini_a.clone())),
            &skyrim_options,
            hash_of(&ini_a),
        );
        let from_b = land_texture_settings(
            &skyrim(Some(ini_b.clone())),
            &skyrim_options,
            hash_of(&ini_b),
        );
        assert_ne!(from_a.terrain_ini_sha256, from_b.terrain_ini_sha256);
        assert_ne!(from_a, from_b, "same paths, different ini bytes");
        assert_ne!(from_a, built_in);
        assert_ne!(from_a, white);

        // A fallen-back default persists no texture paths: entry 0 is white, and the paths it
        // named are only in the report.
        let fallback_source = DefaultTextureSource::SkyrimIniFallback {
            ini: Some(ini_a.clone()),
            diffuse: DIRT02_DIFFUSE.into(),
            normal: None,
            reason: "unreadable".into(),
        };
        let fallback = land_texture_settings(&fallback_source, &skyrim_options, hash_of(&ini_a));
        assert_eq!(fallback.default, LandTextureDefault::SkyrimIniFallbackWhite);
        assert_eq!(fallback.default_diffuse, None);
        assert_eq!(fallback.default_normal, None);
        assert_ne!(
            fallback, from_a,
            "a fallen-back default is not the ini texture"
        );

        let downscaled = land_texture_settings(
            &skyrim(Some(ini_a.clone())),
            &options(TerrainDefaultTexture::SkyrimIni, true),
            hash_of(&ini_a),
        );
        assert_ne!(
            downscaled, from_a,
            "downscale changes the arrays, so it is identity"
        );
    }

    /// The ini hash is the one taken from the read that parsed the defaults. Changing or deleting
    /// the file afterwards does not change what the run records, and the default source and its
    /// reconciliation are the real paths, not hand-built values.
    #[test]
    fn the_ini_hash_is_taken_at_parse_time_and_never_reread() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Skyrim.ini");
        let original = b"[Landscape]\nsDefaultLandDiffuseTexture=textures\\landscape\\Rock.dds\n\
            sDefaultLandNormalTexture=textures\\landscape\\Rock_N.dds\n";
        fs::write(&ini, original).unwrap();
        let recorded = hash_bytes(original);
        let defaults = landscape_defaults(Some(&ini)).unwrap();
        assert_eq!(defaults.ini_sha256.as_deref(), Some(recorded.as_str()));
        let options = TerrainOptions {
            default_texture: TerrainDefaultTexture::SkyrimIni,
            terrain_ini: Some(ini.clone()),
            ..TerrainOptions::default()
        };
        let source_of = |defaults: &LandscapeDefaults| {
            let mut skipped = Vec::new();
            default_source(dir.path(), &HashMap::new(), Some(defaults), &mut skipped)
                .unwrap()
                .1
        };

        // Overwrite the file with other bytes: the recorded identity is still the parsed one.
        fs::write(&ini, "[Landscape]\nsDefaultLandDiffuseTexture=Other.dds\n").unwrap();
        let source = source_of(&defaults);
        let settings = land_texture_settings(&source, &options, defaults.ini_sha256.clone());
        assert_eq!(
            settings.terrain_ini_sha256.as_deref(),
            Some(recorded.as_str())
        );
        assert_ne!(
            hash_bytes(&fs::read(&ini).unwrap()),
            recorded,
            "the file really changed"
        );

        // Delete it: the same settings still come out, with the same hash.
        fs::remove_file(&ini).unwrap();
        let settings = land_texture_settings(&source, &options, defaults.ini_sha256.clone());
        assert_eq!(
            settings.terrain_ini_sha256.as_deref(),
            Some(recorded.as_str())
        );

        // The persisted texture is the converted .ktx2 entry 0 holds, not the DDS the ini named.
        assert_eq!(
            settings.default_diffuse.as_deref(),
            Some("textures/landscape/rock.ktx2")
        );
        assert_eq!(
            settings.default_normal.as_deref(),
            Some("textures/landscape/rock_n.ktx2")
        );

        // A builder that drops the diffuse makes the outcome a fallback: reconciliation sees the
        // arrays' own report, and the settings then persist no texture path at all.
        let dropped = crate::terrain_arrays::ArrayReport {
            default_texture: DefaultTexture::White,
            arrays: Vec::new(),
            textures: Vec::new(),
            skipped: vec![SkippedTexture {
                ltex_form_id: DEFAULT_LAND_TEXTURE_FORM_ID,
                usage: shared::terrain_mesh::LandTextureUsage::DiffuseSrgb,
                source: "textures/landscape/rock.ktx2".into(),
                reason: "unreadable".into(),
            }],
        };
        let reconciled = reconcile_default_source(source, &dropped);
        assert!(matches!(
            reconciled,
            DefaultTextureSource::SkyrimIniFallback { .. }
        ));
        let settings = land_texture_settings(&reconciled, &options, defaults.ini_sha256.clone());
        assert_eq!(settings.default, LandTextureDefault::SkyrimIniFallbackWhite);
        assert_eq!(settings.default_diffuse, None);
        assert_eq!(settings.default_normal, None);
        assert_eq!(
            settings.terrain_ini_sha256.as_deref(),
            Some(recorded.as_str())
        );
    }

    /// The default both painters read: entry 0 under `white` is the generated texture (no file to
    /// read), under `skyrim-ini` it is the DDS the ini names — or the built-in Dirt02 when it names
    /// none — and a diffuse that cannot be a texture path falls back to the generated white one
    /// with its reason instead of stopping the run.
    #[test]
    fn the_default_diffuse_is_resolved_from_the_conversion_settings() {
        let white = TerrainOptions {
            default_texture: TerrainDefaultTexture::White,
            ..TerrainOptions::default()
        };
        assert_eq!(
            TerrainDefaultDiffuse::resolve(&white).unwrap(),
            (TerrainDefaultDiffuse::GeneratedWhite, None)
        );

        let built_in = TerrainOptions {
            default_texture: TerrainDefaultTexture::SkyrimIni,
            ..TerrainOptions::default()
        };
        assert_eq!(
            TerrainDefaultDiffuse::resolve(&built_in).unwrap(),
            (
                TerrainDefaultDiffuse::Image(builtin_default_diffuse().unwrap()),
                None
            )
        );

        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("Skyrim.ini");
        fs::write(
            &ini,
            b"[Landscape]\nsDefaultLandDiffuseTexture=textures\\landscape\\Rock.dds\n\
              sDefaultLandNormalTexture=textures\\landscape\\Rock_N.dds\n",
        )
        .unwrap();
        let configured = TerrainOptions {
            default_texture: TerrainDefaultTexture::SkyrimIni,
            terrain_ini: Some(ini.clone()),
            ..TerrainOptions::default()
        };
        assert_eq!(
            TerrainDefaultDiffuse::resolve(&configured).unwrap(),
            (
                TerrainDefaultDiffuse::Image("textures/landscape/rock.dds".into()),
                None
            )
        );

        // A path that cannot be a texture path leaves entry 0 the generated white texture.
        fs::write(
            &ini,
            "[Landscape]\nsDefaultLandDiffuseTexture=../outside.dds\n",
        )
        .unwrap();
        let (resolved, reason) = TerrainDefaultDiffuse::resolve(&configured).unwrap();
        assert_eq!(resolved, TerrainDefaultDiffuse::GeneratedWhite);
        let reason = reason.expect("the fallback is named");
        assert!(reason.contains("../outside.dds"), "{reason}");

        // A configured ini that cannot be read is a configuration error, not a silent fallback.
        fs::remove_file(&ini).unwrap();
        let error =
            TerrainDefaultDiffuse::resolve(&configured).expect_err("an unreadable ini is an error");
        assert!(format!("{error:#}").contains("Skyrim.ini"), "{error:#}");
    }

    /// A skipped default normal map does not turn the default albedo into the white texture in the
    /// warning; only a fallen-back diffuse does.
    #[test]
    fn a_skipped_default_normal_map_does_not_claim_the_white_albedo() {
        let normal = default_skip_warning("the default normal texture cannot be resolved", false);
        assert!(!normal.contains("white"), "{normal}");
        assert!(normal.contains("the default diffuse stays"), "{normal}");
        let diffuse = default_skip_warning("the default land texture cannot be resolved", true);
        assert!(
            diffuse.contains("falls back to the generated white one"),
            "{diffuse}"
        );
    }

    /// Two distinct LANDs for one cell from the same load-order slot stop the terrain build:
    /// the game's order for that case is not established, so nothing is picked.
    #[test]
    fn two_lands_from_one_plugin_for_one_cell_stop_the_terrain_build() {
        let first = land_at_height(0x1000, 0x3C, 0x100, 0.0);
        let mut second = land_at_height(0x1000, 0x3C, 0x100, 1.0);
        second.form_id = 0x0100_1001;
        let records = merged(&[exterior_cell(0x1000, 0x3C, [-44, 37]), first, second]);
        let staging = tempfile::tempdir().unwrap();
        let error = build_merged_terrain(
            staging.path(),
            &records,
            &masters(0, &["Skyrim.esm"]),
            &HashMap::new(),
            &TerrainOptions::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("cell 00001000"), "{error}");
        assert!(error.contains("00001001"), "{error}");
        assert!(error.contains("01001001"), "{error}");
        assert!(error.contains("slot 0"), "{error}");
    }

    #[test]
    fn a_skyrim_ini_default_reports_what_the_run_resolved() {
        let directory = tempfile::tempdir().unwrap();
        // Both defaults convert: the report names the converted paths entry 0 holds.
        let defaults = LandscapeDefaults {
            ini: Some(PathBuf::from("C:/Games/Skyrim/Skyrim.ini")),
            ini_sha256: None,
            diffuse: "textures\\landscape\\Dirt02.dds".to_string(),
            normal: "textures\\landscape\\Dirt02_N.dds".to_string(),
        };
        let (source, reported) = default_source(
            directory.path(),
            &HashMap::new(),
            Some(&defaults),
            &mut Vec::new(),
        )
        .unwrap();
        let source = source.expect("the converted defaults are entry 0's source");
        let DefaultTextureSource::SkyrimIni {
            diffuse,
            normal,
            ini,
        } = reported
        else {
            panic!("the resolved defaults are reported as skyrim-ini");
        };
        assert_eq!(
            ini.as_deref(),
            Some(Path::new("C:/Games/Skyrim/Skyrim.ini"))
        );
        assert_eq!(source.diffuse, directory.path().join(&diffuse));
        assert!(
            diffuse.ends_with(".ktx2"),
            "{diffuse} is the converted diffuse"
        );
        assert_eq!(
            normal
                .as_deref()
                .map(|normal| directory.path().join(normal))
                .as_ref(),
            source.normal.as_ref()
        );

        // A diffuse that cannot be converted leaves entry 0 the generated white texture, and the
        // report says that instead of naming a texture the run did not use.
        let mut skipped = Vec::new();
        let unusable = LandscapeDefaults {
            diffuse: "../outside.dds".to_string(),
            ..defaults
        };
        let (source, reported) = default_source(
            directory.path(),
            &HashMap::new(),
            Some(&unusable),
            &mut skipped,
        )
        .unwrap();
        assert!(source.is_none(), "the white texture stands in");
        assert_eq!(skipped.len(), 1, "the fallback is reported as skipped too");
        match reported {
            DefaultTextureSource::SkyrimIniFallback {
                diffuse, reason, ..
            } => {
                assert_eq!(diffuse, "../outside.dds");
                assert!(reason.contains("../outside.dds"), "{reason}");
            }
            other => panic!("expected the fallback, got {other:?}"),
        }
    }

    #[test]
    fn a_land_from_the_second_plugin_keys_with_that_plugins_masters() {
        let directory = tempfile::tempdir().unwrap();
        let base = terrain_plugin(directory.path(), "Base.esm", &[]);
        let patch = terrain_plugin(directory.path(), "Patch.esp", &["Base.esm"]);
        let order = LoadOrder::read(&[base.clone(), patch.clone()]).unwrap();
        assert_eq!(order.names, ["base.esm", "patch.esp"]);
        let masters = load_order_masters(&order);
        assert_eq!(masters[&0], Vec::<String>::new());
        assert_eq!(masters[&1], vec!["Base.esm".to_string()]);

        let merged = EsmParser::convert_plugins_with_records(
            &[base.clone(), patch.clone()],
            &directory.path().join("world.db"),
        )
        .unwrap();
        let land = land_of_slot(&merged, 1);
        let key = cell_key(&merged, &masters, &land, &HashSet::new()).unwrap();
        assert_eq!(
            key,
            cell_key(&merged, &masters, &land, &HashSet::new()).unwrap(),
            "the same records and load order give the same key"
        );

        // The second plugin's LAND keys with the second plugin's masters: changing the first
        // plugin's master list leaves it alone, changing its own moves it.
        let mut first_changed = masters.clone();
        first_changed.insert(0, vec!["Dawnguard.esm".to_string()]);
        assert_eq!(
            key,
            cell_key(&merged, &first_changed, &land, &HashSet::new()).unwrap(),
            "a LAND keys with its own plugin's masters, not the first plugin's"
        );
        let mut own_changed = masters.clone();
        own_changed.insert(1, vec!["Update.esm".to_string()]);
        assert_ne!(
            key,
            cell_key(&merged, &own_changed, &land, &HashSet::new()).unwrap(),
            "its own plugin's master list is part of the key"
        );

        // A plugin inserted before the second one that the second one does not depend on leaves
        // its master list alone, and with it the key: load-order numbering is by master index, so
        // neither the LAND's master list nor the FormIDs inside it move with the plugin's slot.
        let inserted = terrain_plugin(directory.path(), "Inserted.esp", &["Base.esm"]);
        let shifted_order =
            LoadOrder::read(&[base.clone(), inserted.clone(), patch.clone()]).unwrap();
        assert_eq!(
            shifted_order.names,
            ["base.esm", "inserted.esp", "patch.esp"]
        );
        let shifted_masters = load_order_masters(&shifted_order);
        let shifted = EsmParser::convert_plugins_with_records(
            &[base.clone(), inserted.clone(), patch],
            &directory.path().join("shifted.db"),
        )
        .unwrap();
        // The second plugin's records keep their load-order FormIDs (numbering is by master
        // index), so the same LAND is the one at that FormID, now declared from a later slot.
        let shifted_land = land_of_form_id(&shifted, land.form_id);
        assert_eq!(shifted_land.load_order, 2);
        assert_eq!(
            key,
            cell_key(&shifted, &shifted_masters, &shifted_land, &HashSet::new()).unwrap(),
            "a plugin that keeps its master list keeps its key when another loads around it"
        );

        // A plugin the LAND's own plugin comes to depend on does change the key: its master list
        // grew and its records sit at a new master index, which is what the key hashes.
        let moved = terrain_plugin(directory.path(), "Moved.esp", &["Base.esm", "Inserted.esp"]);
        let moved_order =
            LoadOrder::read(&[base.clone(), inserted.clone(), moved.clone()]).unwrap();
        assert_eq!(moved_order.names, ["base.esm", "inserted.esp", "moved.esp"]);
        let moved_masters = load_order_masters(&moved_order);
        assert_eq!(
            moved_masters[&2],
            vec!["Base.esm".to_string(), "Inserted.esp".to_string()]
        );
        let moved_merged = EsmParser::convert_plugins_with_records(
            &[base, inserted, moved],
            &directory.path().join("moved.db"),
        )
        .unwrap();
        let moved_land = land_of_slot(&moved_merged, 2);
        assert_ne!(
            key,
            cell_key(&moved_merged, &moved_masters, &moved_land, &HashSet::new()).unwrap(),
            "a plugin earlier in the order joining the LAND's master list moves its key"
        );
    }

    #[test]
    fn editing_an_ltex_the_land_names_changes_the_key() {
        let land = land(0x1000, 0x003C, 0x100);
        let ltex = ltex(0x100, "textures\\landscape\\fieldgrass01.dds", 0x200);
        let texture_set = txst(
            0x200,
            "textures\\landscape\\fieldgrass01.dds",
            "textures\\landscape\\fieldgrass01_n.dds",
        );
        let records = merged(&[land.clone(), ltex.clone(), texture_set.clone()]);
        let order = masters(0, &["Skyrim.esm"]);
        let key = cell_key(&records, &order, &land, &HashSet::new()).unwrap();
        assert_eq!(
            key,
            cell_key(&records, &order, &land, &HashSet::new()).unwrap(),
            "the same records and masters give the same key"
        );

        // The LTEX the LAND names moves to another texture set.
        let mut moved = records.clone();
        let mut moved_ltex = ltex.clone();
        moved_ltex.subrecords[0].1 = 0x201u32.to_le_bytes().to_vec();
        moved.insert(moved_ltex.form_id, moved_ltex);
        assert_ne!(
            key,
            cell_key(&moved, &order, &land, &HashSet::new()).unwrap(),
            "a texture set change on a named LTEX must rebuild the cell"
        );

        // An edit that leaves the TNAM alone still changes the key: the record's own bytes are
        // hashed.
        let mut edited = records.clone();
        let mut edited_ltex = ltex.clone();
        edited_ltex.subrecords[1].1.push(b'x');
        edited.insert(edited_ltex.form_id, edited_ltex);
        assert_ne!(
            key,
            cell_key(&edited, &order, &land, &HashSet::new()).unwrap(),
            "any edit to a named LTEX must rebuild the cell"
        );

        // An edit to the texture set's own payload does too.
        let mut edited_txst = records.clone();
        let mut edited_set = texture_set.clone();
        edited_set.subrecords[0].1.push(b'.');
        edited_txst.insert(edited_set.form_id, edited_set);
        assert_ne!(
            key,
            cell_key(&edited_txst, &order, &land, &HashSet::new()).unwrap(),
            "an edit to an existing texture set's payload must rebuild the cell"
        );

        // An LTEX no quadrant names is not part of the key.
        let mut unrelated = records;
        unrelated.insert(0x300, txst(0x300, "textures\\landscape\\other.dds", ""));
        assert_eq!(
            key,
            cell_key(&unrelated, &order, &land, &HashSet::new()).unwrap(),
            "only the LTEX the LAND names are hashed"
        );
    }

    #[test]
    fn a_load_order_shift_changes_the_key_of_the_same_records() {
        // The plugin is alone in slot 0: its LAND and LTEX are in its own numbering.
        let land = land(0x1000, 0x003C, 0x100);
        let ltex = ltex(0x100, "textures\\landscape\\fieldgrass01.dds", 0x200);
        let texture_set = txst(0x200, "textures\\landscape\\fieldgrass01.dds", "");
        let before = cell_key(
            &merged(&[land.clone(), ltex.clone(), texture_set.clone()]),
            &masters(0, &["Skyrim.esm"]),
            &land,
            &HashSet::new(),
        )
        .unwrap();

        // `Update.esm` loads between Skyrim.esm and the plugin, so the same records now come from
        // slot 1, with the new plugin's master list and every local FormID remapped to master
        // index 1, the way `esm::remap_record_form_ids` rewrites them (the LAND's BTXT payloads
        // included).
        let mut shifted_land = land.clone();
        shifted_land.form_id = 0x0100_1001;
        shifted_land.cell_form_id = Some(0x0100_1000);
        shifted_land.worldspace_form_id = Some(0x0100_003C);
        shifted_land.load_order = 1;
        for quadrant in 0..4 {
            // The BTXT's FormID is its first four bytes; the rest is quadrant and layer index.
            shifted_land.subrecords[1 + quadrant].1[..4]
                .copy_from_slice(&0x0100_0100u32.to_le_bytes());
        }
        let mut shifted_ltex = ltex.clone();
        shifted_ltex.form_id = 0x0100_0100;
        shifted_ltex.load_order = 1;
        let mut shifted_txst = texture_set.clone();
        shifted_txst.form_id = 0x0100_0200;
        shifted_txst.load_order = 1;
        let after = cell_key(
            &merged(&[shifted_land.clone(), shifted_ltex, shifted_txst]),
            &BTreeMap::from([
                (0, vec!["Skyrim.esm".to_string()]),
                (1, vec!["Skyrim.esm".to_string(), "Update.esm".to_string()]),
            ]),
            &shifted_land,
            &HashSet::new(),
        )
        .unwrap();
        assert_ne!(
            before, after,
            "the same records in a shifted load order must rebuild the cell"
        );

        // The master list alone shifts the key, even with every record byte unchanged.
        let unchanged = merged(&[land.clone(), ltex, texture_set]);
        assert_ne!(
            before,
            cell_key(
                &unchanged,
                &masters(0, &["Skyrim.esm", "Update.esm"]),
                &land,
                &HashSet::new()
            )
            .unwrap(),
            "a master list change moves every FormID the record means"
        );
    }

    fn report_with(
        default_texture: DefaultTexture,
        skipped: Vec<SkippedTexture>,
    ) -> crate::terrain_arrays::ArrayReport {
        crate::terrain_arrays::ArrayReport {
            default_texture,
            arrays: Vec::new(),
            textures: Vec::new(),
            skipped,
        }
    }

    fn skyrim_ini_source() -> DefaultTextureSource {
        DefaultTextureSource::SkyrimIni {
            ini: None,
            diffuse: "textures/landscape/dirt02.ktx2".to_owned(),
            normal: Some("textures/landscape/dirt02_n.ktx2".to_owned()),
        }
    }

    fn skipped_default(
        path: &str,
        usage: shared::terrain_mesh::LandTextureUsage,
    ) -> SkippedTexture {
        SkippedTexture {
            ltex_form_id: 0,
            usage,
            source: path.to_owned(),
            reason: "unreadable".to_owned(),
        }
    }

    #[test]
    fn a_default_the_builder_used_keeps_its_skyrim_ini_source() {
        let report = report_with(DefaultTexture::Provided { ltex_form_id: 0 }, Vec::new());
        assert_eq!(
            reconcile_default_source(skyrim_ini_source(), &report),
            skyrim_ini_source()
        );
    }

    #[test]
    fn a_default_diffuse_the_builder_dropped_is_reported_as_the_white_fallback() {
        use shared::terrain_mesh::LandTextureUsage;
        let report = report_with(
            DefaultTexture::White,
            vec![skipped_default(
                "textures/landscape/dirt02.ktx2",
                LandTextureUsage::DiffuseSrgb,
            )],
        );
        match reconcile_default_source(skyrim_ini_source(), &report) {
            DefaultTextureSource::SkyrimIniFallback {
                reason, diffuse, ..
            } => {
                assert!(reason.contains("unreadable"), "{reason}");
                assert_eq!(diffuse, "textures/landscape/dirt02.ktx2");
            }
            other => panic!("expected the white fallback, got {other:?}"),
        }
    }

    #[test]
    fn a_default_normal_the_builder_dropped_is_cleared_from_the_source() {
        use shared::terrain_mesh::LandTextureUsage;
        let report = report_with(
            DefaultTexture::Provided { ltex_form_id: 0 },
            vec![skipped_default(
                "textures/landscape/dirt02_n.ktx2",
                LandTextureUsage::NormalLinear,
            )],
        );
        assert_eq!(
            reconcile_default_source(skyrim_ini_source(), &report),
            DefaultTextureSource::SkyrimIni {
                ini: None,
                diffuse: "textures/landscape/dirt02.ktx2".to_owned(),
                normal: None,
            }
        );
    }

    #[test]
    fn a_white_default_source_needs_no_reconciling() {
        let report = report_with(DefaultTexture::White, Vec::new());
        assert_eq!(
            reconcile_default_source(DefaultTextureSource::White, &report),
            DefaultTextureSource::White
        );
    }
}
