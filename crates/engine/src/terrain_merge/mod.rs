//! Merged terrain: one mesh per exterior cell and one material for all of them (#215).
//!
//! The converter writes, per exterior cell, one [`TerrainCellMesh`] (four unwelded quadrants with
//! per-vertex texture slots) in chunk files, and game-wide land texture arrays plus a lookup
//! table. This module loads them: [`TerrainMerge`] opens the table, waits for the device check
//! and the arrays, builds the one shared material, and hands the streaming code a validated cell
//! on request. Any cell without usable merged data, and every cell when the merged path is
//! unavailable, is drawn by the quadrant path of `streaming.rs` instead; nothing is dropped
//! silently, the streaming metrics count both kinds of cell.
//!
//! Assumed converter outputs (all from `shared::terrain_mesh`): the table at
//! [`LAND_TEXTURE_TABLE_PATH`] naming each array file by path, and one chunk file per
//! [`terrain_chunk_path`].

pub mod classes;
pub mod material;
pub mod mesh;
#[cfg(test)]
mod shader_check;

use crate::{render::terrain_layer_sampler, streaming::StreamingMetrics, world::database::CellKey};
use bevy::{
    asset::{AssetId, LoadState, RenderAssetUsages, embedded_asset},
    image::{ImageLoaderSettings, ImageSampler},
    prelude::*,
    render::{
        RenderApp, RenderStartup,
        render_resource::{TextureFormat, TextureViewDimension},
        renderer::RenderDevice,
        storage::ShaderBuffer,
    },
};
use classes::{AdapterLimits, ClassSlot, TerrainClasses};
use material::{MergedTerrainExtension, MergedTerrainMaterial};
use memmap2::Mmap;
use rkyv::rancor::Error as RkyvError;
use shared::terrain_mesh::{
    ArchivedTerrainChunkFile, LAND_TEXTURE_TABLE_PATH, LandTextureFormat, LandTextureTable,
    LandTextureUsage, TERRAIN_MESH_FORMAT_VERSION, TerrainCellMesh, terrain_chunk_path,
};
use std::{
    collections::{HashMap, VecDeque},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

/// Chunk files kept open at once. A 17 x 17 cell window touches about 25 chunks; the cap leaves
/// room for turning around without reopening, and a chunk dropped from the cache is simply read
/// again.
const CHUNK_CACHE_LIMIT: usize = 128;

/// How long the merged path may take to become ready (device check, array load, validation)
/// before it is refused. Exterior cells with terrain wait that long at most. The arrays are a
/// few hundred MiB of block-compressed data, so a slow disk needs seconds, not minutes; a render
/// world that never reports (no renderer, a failed device) would otherwise hold terrain forever.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether a setup that began at `started` has run out of time at `now`.
pub fn setup_expired(started: Instant, now: Instant) -> bool {
    now.saturating_duration_since(started) >= SETUP_TIMEOUT
}

/// What [`TerrainMerge`] found at startup, and how far along it is.
#[derive(Debug, Clone)]
pub enum MergeState {
    /// The conversion has no land texture table: it predates merged terrain.
    Absent,
    /// Merged terrain is not used, for the reason given: an unreadable table, a device that
    /// cannot bind the arrays, or arrays that failed to load or validate.
    Refused(String),
    /// The table is valid and waits for the device check.
    AwaitingDevice(Arc<TerrainClasses>),
    /// The device accepted the table; the arrays are loading.
    LoadingArrays {
        classes: Arc<TerrainClasses>,
        diffuse: Vec<Handle<Image>>,
        normal: Vec<Handle<Image>>,
        entries: Handle<ShaderBuffer>,
    },
    /// The shared material is built; cells may use merged data.
    Ready {
        classes: Arc<TerrainClasses>,
        material: Handle<MergedTerrainMaterial>,
        /// Every land texture array the material binds, diffuse then normal: what a merged
        /// cell's draw needs from the render world besides its mesh and the material itself
        /// (`crate::swap_bridge::SwapSurface`).
        images: Arc<[AssetId<Image>]>,
    },
}

impl MergeState {
    /// A short word for the metrics.
    pub fn label(&self) -> String {
        match self {
            Self::Absent => "absent".to_owned(),
            Self::Refused(reason) => format!("refused: {reason}"),
            Self::AwaitingDevice(_) => "awaiting device".to_owned(),
            Self::LoadingArrays { .. } => "loading arrays".to_owned(),
            Self::Ready { .. } => "ready".to_owned(),
        }
    }
}

/// What the merged data offers one cell.
#[derive(Debug)]
pub enum CellLookup {
    /// The merged path is not in use (no table, refused, or not an exterior cell): the cell is
    /// drawn by the quadrant path, which is not a per-cell event.
    Unavailable,
    /// The merged path is ready but the chunk file or the cell's entry is missing.
    NoData,
    /// The chunk holds the cell but it cannot be used.
    Invalid(String),
    /// A cell that passed [`TerrainCellMesh::validate`] against the table.
    Found(Box<TerrainCellMesh>),
}

/// One chunk file: its bytes, validated once when opened.
enum ChunkEntry {
    Missing,
    Invalid(String),
    Open(Mmap),
}

/// The chunk files, opened on demand and kept while they are in use.
pub struct ChunkStore {
    root: PathBuf,
    chunks: HashMap<String, ChunkEntry>,
    order: VecDeque<String>,
}

impl ChunkStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            chunks: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// The converted output root the chunk and array paths are relative to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The chunk at `relative`, opened on first use. A chunk that does not exist is cached as
    /// [`ChunkEntry::Missing`] on purpose: the converted assets are fixed for the whole run (the
    /// engine holds a shared lock on them), so a missing file stays missing, and rechecking the
    /// disk for every cell of a worldspace with no merged data would cost a failed `stat` per cell.
    fn entry(&mut self, relative: &str, worldspace_id: u32) -> &ChunkEntry {
        if !self.chunks.contains_key(relative) {
            while self.order.len() >= CHUNK_CACHE_LIMIT {
                if let Some(oldest) = self.order.pop_front() {
                    self.chunks.remove(&oldest);
                }
            }
            let entry = open_chunk(&self.root.join(relative), worldspace_id);
            self.order.push_back(relative.to_owned());
            self.chunks.insert(relative.to_owned(), entry);
        }
        &self.chunks[relative]
    }

    /// The cell `cell_id` of the chunk that holds exterior cell (`grid_x`, `grid_y`).
    pub fn cell(
        &mut self,
        worldspace_id: u32,
        grid_x: i32,
        grid_y: i32,
        cell_id: u32,
    ) -> CellLookup {
        let relative = terrain_chunk_path(worldspace_id, grid_x, grid_y);
        match self.entry(&relative, worldspace_id) {
            ChunkEntry::Missing => CellLookup::NoData,
            ChunkEntry::Invalid(reason) => CellLookup::Invalid(reason.clone()),
            ChunkEntry::Open(bytes) => {
                // SAFETY: `open_chunk` validated exactly these bytes with `rkyv::access`, and the
                // mapping is never changed while it is held here.
                let archived = unsafe { rkyv::access_unchecked::<ArchivedTerrainChunkFile>(bytes) };
                let Some(cell) = archived
                    .cells
                    .iter()
                    .find(|cell| u32::from(cell.cell_id) == cell_id)
                else {
                    return CellLookup::NoData;
                };
                match rkyv::deserialize::<TerrainCellMesh, RkyvError>(cell) {
                    Ok(cell) => CellLookup::Found(Box::new(cell)),
                    Err(error) => CellLookup::Invalid(format!("{relative}: {error}")),
                }
            }
        }
    }
}

fn open_chunk(path: &Path, worldspace_id: u32) -> ChunkEntry {
    if !path.is_file() {
        return ChunkEntry::Missing;
    }
    let describe = |what: String| format!("{}: {what}", path.display());
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return ChunkEntry::Invalid(describe(error.to_string())),
    };
    // SAFETY: converted assets are read-only while the engine holds the asset lock.
    let bytes = match unsafe { Mmap::map(&file) } {
        Ok(bytes) => bytes,
        Err(error) => return ChunkEntry::Invalid(describe(error.to_string())),
    };
    match rkyv::access::<ArchivedTerrainChunkFile, RkyvError>(&bytes) {
        Err(error) => ChunkEntry::Invalid(describe(error.to_string())),
        Ok(chunk) if u32::from(chunk.format_version) != TERRAIN_MESH_FORMAT_VERSION => {
            ChunkEntry::Invalid(describe(format!(
                "terrain format version {}, expected {TERRAIN_MESH_FORMAT_VERSION}",
                u32::from(chunk.format_version)
            )))
        }
        Ok(chunk) if u32::from(chunk.worldspace_id) != worldspace_id => {
            ChunkEntry::Invalid(describe(format!(
                "chunk is for worldspace {:08X}, expected {worldspace_id:08X}",
                u32::from(chunk.worldspace_id)
            )))
        }
        Ok(_) => ChunkEntry::Open(bytes),
    }
}

/// The merged terrain of one run.
#[derive(Resource)]
pub struct TerrainMerge {
    pub state: MergeState,
    store: ChunkStore,
    /// When setup began, for [`SETUP_TIMEOUT`].
    started: Instant,
    /// Whether the "holding cells" line has been logged.
    hold_logged: bool,
}

impl TerrainMerge {
    /// Reads the land texture table under `assets_dir`. A conversion without one is
    /// [`MergeState::Absent`]; a table that cannot be read or validated is refused, with the
    /// reason logged once.
    pub fn open(assets_dir: &Path) -> Self {
        let table_path = assets_dir.join(LAND_TEXTURE_TABLE_PATH);
        let state = if !table_path.is_file() {
            info!("no land texture table in this conversion; terrain is drawn by quadrant");
            MergeState::Absent
        } else {
            match read_table(&table_path).and_then(TerrainClasses::from_table) {
                Ok(classes) => MergeState::AwaitingDevice(Arc::new(classes)),
                Err(reason) => {
                    error!(%reason, path = %table_path.display(), "merged terrain refused");
                    MergeState::Refused(reason)
                }
            }
        };
        Self {
            state,
            store: ChunkStore::new(assets_dir.to_path_buf()),
            started: Instant::now(),
            hold_logged: false,
        }
    }

    /// A merge in a given state, for tests.
    #[cfg(test)]
    pub(crate) fn with_state(state: MergeState, root: PathBuf) -> Self {
        Self {
            state,
            store: ChunkStore::new(root),
            started: Instant::now(),
            hold_logged: false,
        }
    }

    /// The same, with setup having begun `ago` earlier, for the timeout tests.
    #[cfg(test)]
    pub(crate) fn with_state_started(state: MergeState, root: PathBuf, ago: Duration) -> Self {
        let mut merge = Self::with_state(state, root);
        merge.started = Instant::now().checked_sub(ago).unwrap();
        merge
    }

    /// The classes the render world checks against the device, when a table is configured.
    pub fn pending_classes(&self) -> Option<Arc<TerrainClasses>> {
        match &self.state {
            MergeState::AwaitingDevice(classes)
            | MergeState::LoadingArrays { classes, .. }
            | MergeState::Ready { classes, .. } => Some(classes.clone()),
            MergeState::Absent | MergeState::Refused(_) => None,
        }
    }

    /// Whether cells must wait: the table is valid but the arrays are not ready, so a cell
    /// committed now could not pick between merged and fallback honestly.
    pub fn holds_cells(&self) -> bool {
        matches!(
            self.state,
            MergeState::AwaitingDevice(_) | MergeState::LoadingArrays { .. }
        )
    }

    /// Whether a cell must wait for the merged path: only an exterior cell that has terrain
    /// does (interiors and cells with no LAND never use merged terrain), and only while setup is
    /// in progress. `has_terrain` is whether the cell has a cached LAND.
    pub fn holds_cell(&self, key: CellKey, has_terrain: bool) -> bool {
        has_terrain && matches!(key, CellKey::Exterior { .. }) && self.holds_cells()
    }

    /// Records that a cell was held and says whether this is the first one, so the caller logs
    /// the wait once.
    pub fn note_hold(&mut self) -> bool {
        !std::mem::replace(&mut self.hold_logged, true)
    }

    /// Whether setup has run out of time at `now`.
    pub fn setup_expired(&self, now: Instant) -> bool {
        setup_expired(self.started, now)
    }

    /// The shared material, once the arrays are ready.
    pub fn material(&self) -> Option<&Handle<MergedTerrainMaterial>> {
        match &self.state {
            MergeState::Ready { material, .. } => Some(material),
            _ => None,
        }
    }

    pub fn classes(&self) -> Option<&Arc<TerrainClasses>> {
        match &self.state {
            MergeState::Ready { classes, .. } => Some(classes),
            _ => None,
        }
    }

    /// The images the shared material binds, once the arrays are ready.
    pub fn material_images(&self) -> Option<&Arc<[AssetId<Image>]>> {
        match &self.state {
            MergeState::Ready { images, .. } => Some(images),
            _ => None,
        }
    }

    /// The merged data of the exterior cell `key` (`cell_id` its CELL FormID), validated against
    /// the table. Anything but [`CellLookup::Found`] means the cell takes the quadrant path.
    pub fn lookup(&mut self, key: CellKey, cell_id: u32) -> CellLookup {
        let MergeState::Ready { classes, .. } = &self.state else {
            return CellLookup::Unavailable;
        };
        let classes = classes.clone();
        let CellKey::Exterior {
            worldspace_id,
            grid_x,
            grid_y,
        } = key
        else {
            return CellLookup::Unavailable;
        };
        match self.store.cell(worldspace_id, grid_x, grid_y, cell_id) {
            CellLookup::Found(cell) => match cell.validate(&classes.ids_by_form_id) {
                Ok(()) => CellLookup::Found(cell),
                Err(error) => CellLookup::Invalid(error.to_string()),
            },
            other => other,
        }
    }
}

fn read_table(path: &Path) -> Result<LandTextureTable, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// The device's verdict on the table, written once by the render world and read by the main one.
#[derive(Resource, Clone, Default)]
pub struct TerrainMergeVerdict(pub Arc<OnceLock<Result<(), String>>>);

/// The classes the render world checks, inserted into the render app by the plugin.
#[derive(Resource, Clone)]
struct RenderClasses(Arc<TerrainClasses>);

pub struct TerrainMergePlugin {
    classes: Option<Arc<TerrainClasses>>,
}

impl TerrainMergePlugin {
    /// `merge` is the run's [`TerrainMerge`] (or none, for runs that draw no streamed world).
    pub fn new(merge: Option<&TerrainMerge>) -> Self {
        Self {
            classes: merge.and_then(TerrainMerge::pending_classes),
        }
    }
}

impl Plugin for TerrainMergePlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "terrain_merged.wgsl");
        let verdict = TerrainMergeVerdict::default();
        app.add_plugins(MaterialPlugin::<MergedTerrainMaterial>::default())
            .insert_resource(verdict.clone())
            .add_systems(PreUpdate, advance_terrain_merge);
        let Some(classes) = self.classes.clone() else {
            return;
        };
        match app.get_sub_app_mut(RenderApp) {
            Some(render_app) => {
                render_app
                    .insert_resource(RenderClasses(classes))
                    .insert_resource(verdict)
                    .add_systems(RenderStartup, check_device);
            }
            // No renderer (a headless run without the render plugin): nothing would ever check
            // the device, so the table is refused now instead of holding terrain for good.
            None => {
                post_missing_render_world(&verdict);
            }
        }
    }
}

/// Refuses the table because nothing will ever check it against a device.
fn post_missing_render_world(verdict: &TerrainMergeVerdict) {
    let _ = verdict.0.set(Err(
        "there is no render world to check the land texture arrays against a device".to_owned(),
    ));
}

/// The device numbers [`TerrainClasses::check_adapter`] reads.
fn adapter_limits(device: &RenderDevice) -> AdapterLimits {
    let limits = device.limits();
    AdapterLimits {
        max_sampled_textures_per_shader_stage: limits.max_sampled_textures_per_shader_stage,
        max_texture_array_layers: limits.max_texture_array_layers,
        max_texture_dimension_2d: limits.max_texture_dimension_2d,
    }
}

fn check_device(
    device: Res<RenderDevice>,
    classes: Res<RenderClasses>,
    verdict: Res<TerrainMergeVerdict>,
) {
    let _ = verdict
        .0
        .set(classes.0.check_adapter(adapter_limits(&device)));
}

/// The texture format a class's array must have once loaded: the table's block format in the
/// colour space its usage decodes in.
pub fn expected_format(format: LandTextureFormat, usage: LandTextureUsage) -> TextureFormat {
    let srgb = usage == LandTextureUsage::DiffuseSrgb;
    match (format, srgb) {
        (LandTextureFormat::Bc1, true) => TextureFormat::Bc1RgbaUnormSrgb,
        (LandTextureFormat::Bc1, false) => TextureFormat::Bc1RgbaUnorm,
        (LandTextureFormat::Bc3, true) => TextureFormat::Bc3RgbaUnormSrgb,
        (LandTextureFormat::Bc3, false) => TextureFormat::Bc3RgbaUnorm,
        (LandTextureFormat::Bc5, _) => TextureFormat::Bc5RgUnorm,
        (LandTextureFormat::Bc7, true) => TextureFormat::Bc7RgbaUnormSrgb,
        (LandTextureFormat::Bc7, false) => TextureFormat::Bc7RgbaUnorm,
    }
}

fn sampler_repeats(sampler: &ImageSampler) -> bool {
    matches!(
        sampler,
        ImageSampler::Descriptor(descriptor)
            if descriptor.address_mode_u == bevy::image::ImageAddressMode::Repeat
                && descriptor.address_mode_v == bevy::image::ImageAddressMode::Repeat
    )
}

/// Checks a loaded array against what the table promised: the block format in the right colour
/// space, a 2D array view of the listed layer count and size, and a repeating sampler.
pub fn validate_array_image(
    image: &Image,
    slot: &ClassSlot,
    usage: LandTextureUsage,
) -> Result<(), String> {
    let describe = |what: String| format!("land texture array {}: {what}", slot.path);
    let wanted = expected_format(slot.format, usage);
    let descriptor = &image.texture_descriptor;
    if descriptor.format != wanted {
        return Err(describe(format!(
            "format {:?}, expected {wanted:?}",
            descriptor.format
        )));
    }
    let dimension = image
        .texture_view_descriptor
        .as_ref()
        .and_then(|view| view.dimension);
    if dimension != Some(TextureViewDimension::D2Array) {
        return Err(describe(format!(
            "view dimension {dimension:?}, expected a 2D array"
        )));
    }
    let size = descriptor.size;
    if size.width != slot.size
        || size.height != slot.size
        || size.depth_or_array_layers != slot.layers
    {
        return Err(describe(format!(
            "{}x{} with {} layers, expected {}x{} with {}",
            size.width, size.height, size.depth_or_array_layers, slot.size, slot.size, slot.layers
        )));
    }
    if !sampler_repeats(&image.sampler) {
        return Err(describe("did not load with a repeating sampler".to_owned()));
    }
    Ok(())
}

/// The block formats a KTX2 file may hold for `usage` and the table's `format`: the sRGB variants
/// for diffuse arrays, the linear (UNORM) ones for normal arrays.
fn allowed_ktx2_formats(format: LandTextureFormat, usage: LandTextureUsage) -> Vec<ktx2::Format> {
    use ktx2::Format as F;
    match (usage, format) {
        (LandTextureUsage::DiffuseSrgb, LandTextureFormat::Bc1) => {
            vec![F::BC1_RGBA_SRGB_BLOCK, F::BC1_RGB_SRGB_BLOCK]
        }
        (LandTextureUsage::DiffuseSrgb, LandTextureFormat::Bc3) => vec![F::BC3_SRGB_BLOCK],
        (LandTextureUsage::DiffuseSrgb, LandTextureFormat::Bc7) => vec![F::BC7_SRGB_BLOCK],
        (LandTextureUsage::NormalLinear, LandTextureFormat::Bc3) => vec![F::BC3_UNORM_BLOCK],
        (LandTextureUsage::NormalLinear, LandTextureFormat::Bc5) => vec![F::BC5_UNORM_BLOCK],
        (LandTextureUsage::NormalLinear, LandTextureFormat::Bc7) => vec![F::BC7_UNORM_BLOCK],
        // `LandTextureTable::validate` refuses these combinations before any file is read.
        (LandTextureUsage::DiffuseSrgb, LandTextureFormat::Bc5)
        | (LandTextureUsage::NormalLinear, LandTextureFormat::Bc1) => Vec::new(),
    }
}

/// Checks the 80-byte header of an array file against the table: the file's own `vkFormat` must
/// be the table's block format in the usage's colour space (sRGB for diffuse, linear for
/// normals), and its size and layer count must be the listed ones. Bevy's KTX2 loader takes the
/// colour space from the load settings rather than the file, so the loaded image can never show
/// a wrong-colour-space file; only the header can.
pub fn check_ktx2_header(
    header: &[u8; ktx2::Header::LENGTH],
    slot: &ClassSlot,
    usage: LandTextureUsage,
) -> Result<(), String> {
    let describe = |what: String| format!("land texture array {}: {what}", slot.path);
    let header = ktx2::Header::from_bytes(header)
        .map_err(|error| describe(format!("not a KTX2 file: {error:?}")))?;
    let allowed = allowed_ktx2_formats(slot.format, usage);
    match header.format {
        Some(format) if allowed.contains(&format) => {}
        Some(format) => {
            let colour_space = match usage {
                LandTextureUsage::DiffuseSrgb => "an sRGB",
                LandTextureUsage::NormalLinear => "a linear",
            };
            return Err(describe(format!(
                "stored as {format:?}, expected {colour_space} {:?} block format ({allowed:?})",
                slot.format
            )));
        }
        None => {
            return Err(describe(
                "stored in a supercompressed format with no block format of its own".to_owned(),
            ));
        }
    }
    if header.pixel_width != slot.size
        || header.pixel_height != slot.size
        || header.layer_count != slot.layers
    {
        return Err(describe(format!(
            "{}x{} with {} layers, expected {}x{} with {}",
            header.pixel_width,
            header.pixel_height,
            header.layer_count,
            slot.size,
            slot.size,
            slot.layers
        )));
    }
    Ok(())
}

/// Reads and checks the header of `slot`'s file under `root`.
fn check_array_file(root: &Path, slot: &ClassSlot, usage: LandTextureUsage) -> Result<(), String> {
    let path = root.join(&slot.path);
    let mut header = [0u8; ktx2::Header::LENGTH];
    std::fs::File::open(&path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| format!("land texture array {}: {error}", path.display()))?;
    check_ktx2_header(&header, slot, usage)
}

fn load_array(
    asset_server: &AssetServer,
    slot: &ClassSlot,
    usage: LandTextureUsage,
) -> Handle<Image> {
    asset_server
        .load_builder()
        .with_settings(move |settings: &mut ImageLoaderSettings| {
            settings.sampler = ImageSampler::Descriptor(terrain_layer_sampler());
            // The loader takes the colour space from this setting, not from the file; the
            // file's own format is checked against the usage before loading
            // (`check_array_file`).
            settings.is_srgb = usage == LandTextureUsage::DiffuseSrgb;
            // The pixels are only needed on the GPU: Bevy drops the CPU copy after upload, and
            // the checks read the image's descriptor, which stays.
            settings.asset_usage = RenderAssetUsages::RENDER_WORLD;
        })
        .load(slot.path.clone())
}

/// Moves a table from "valid" to "ready": waits for the device check, loads the arrays, validates
/// them and builds the shared material. A failure anywhere refuses the merged path with a reason,
/// and every cell then takes the quadrant path.
#[allow(clippy::too_many_arguments)]
fn advance_terrain_merge(
    merge: Option<ResMut<TerrainMerge>>,
    verdict: Res<TerrainMergeVerdict>,
    asset_server: Res<AssetServer>,
    images: Res<Assets<Image>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut materials: ResMut<Assets<MergedTerrainMaterial>>,
    metrics: Option<ResMut<StreamingMetrics>>,
) {
    let Some(mut merge) = merge else {
        return;
    };
    let expired = merge.setup_expired(Instant::now());
    let mut metrics = metrics;
    if let Some(metrics) = metrics.as_deref_mut()
        && metrics.terrain_merge_state.is_empty()
    {
        metrics.terrain_merge_state = merge.state.label();
    }
    let next = match &merge.state {
        MergeState::AwaitingDevice(classes) => match verdict.0.get() {
            None if expired => Some(MergeState::Refused(format!(
                "the device check of the land texture arrays did not report within {} s",
                SETUP_TIMEOUT.as_secs()
            ))),
            None => return,
            Some(Err(reason)) => Some(MergeState::Refused(reason.clone())),
            Some(Ok(())) => 'load: {
                // The files' own formats first: a wrong colour space or size is refused before
                // anything is loaded.
                let files = classes
                    .diffuse
                    .iter()
                    .map(|slot| (slot, LandTextureUsage::DiffuseSrgb))
                    .chain(
                        classes
                            .normal
                            .iter()
                            .map(|slot| (slot, LandTextureUsage::NormalLinear)),
                    );
                for (slot, usage) in files {
                    if let Err(reason) = check_array_file(merge.store.root(), slot, usage) {
                        break 'load Some(MergeState::Refused(reason));
                    }
                }
                let diffuse = classes
                    .diffuse
                    .iter()
                    .map(|slot| load_array(&asset_server, slot, LandTextureUsage::DiffuseSrgb))
                    .collect();
                let normal = classes
                    .normal
                    .iter()
                    .map(|slot| load_array(&asset_server, slot, LandTextureUsage::NormalLinear))
                    .collect();
                let entries = buffers.add(ShaderBuffer::new(
                    &classes.entry_bytes(),
                    RenderAssetUsages::RENDER_WORLD,
                ));
                Some(MergeState::LoadingArrays {
                    classes: classes.clone(),
                    diffuse,
                    normal,
                    entries,
                })
            }
        },
        MergeState::LoadingArrays {
            classes,
            diffuse,
            normal,
            entries,
        } => {
            let all = classes
                .diffuse
                .iter()
                .zip(diffuse)
                .map(|(slot, handle)| (slot, handle, LandTextureUsage::DiffuseSrgb))
                .chain(
                    classes
                        .normal
                        .iter()
                        .zip(normal)
                        .map(|(slot, handle)| (slot, handle, LandTextureUsage::NormalLinear)),
                );
            let mut pending = Vec::new();
            let mut refusal = None;
            for (slot, handle, usage) in all {
                match asset_server.load_state(handle) {
                    LoadState::Failed(error) => {
                        refusal = Some(format!("land texture array {}: {error}", slot.path));
                        break;
                    }
                    LoadState::Loaded => match images.get(handle) {
                        Some(image) => {
                            if let Err(reason) = validate_array_image(image, slot, usage) {
                                refusal = Some(reason);
                                break;
                            }
                        }
                        None => pending.push(slot.path.clone()),
                    },
                    _ => pending.push(slot.path.clone()),
                }
            }
            if let Some(reason) = refusal {
                Some(MergeState::Refused(reason))
            } else if !pending.is_empty() {
                if !expired {
                    return;
                }
                Some(MergeState::Refused(format!(
                    "land texture arrays still loading after {} s: {}",
                    SETUP_TIMEOUT.as_secs(),
                    pending.join(", ")
                )))
            } else {
                // Every array the material binds, for the bridge's declaration of each cell.
                let bound = diffuse.iter().chain(normal).map(Handle::id).collect();
                match MergedTerrainExtension::new(diffuse, normal, entries.clone()) {
                    Ok(extension) => Some(MergeState::Ready {
                        images: bound,
                        classes: classes.clone(),
                        material: materials.add(MergedTerrainMaterial {
                            base: StandardMaterial {
                                base_color: Color::WHITE,
                                perceptual_roughness: 0.92,
                                cull_mode: None,
                                double_sided: true,
                                ..default()
                            },
                            extension,
                        }),
                    }),
                    Err(reason) => Some(MergeState::Refused(reason)),
                }
            }
        }
        _ => return,
    };
    if let Some(state) = next {
        match &state {
            MergeState::Refused(reason) => {
                error!(%reason, "merged terrain refused; terrain is drawn by quadrant");
            }
            MergeState::Ready { classes, .. } => info!(
                diffuse_classes = classes.diffuse.len(),
                normal_classes = classes.normal.len(),
                textures = classes.table.entries.len(),
                "merged terrain ready"
            ),
            _ => {}
        }
        if let Some(metrics) = metrics.as_deref_mut() {
            metrics.terrain_merge_state = state.label();
        }
        merge.state = state;
    }
}

#[cfg(test)]
mod tests {
    use super::classes::tests::{mesh_with_slots, vanilla_table};
    use super::*;
    use bevy::render::render_resource::{Extent3d, TextureViewDescriptor};
    use shared::terrain_mesh::TerrainChunkFile;

    fn chunk_bytes(worldspace_id: u32, cells: Vec<TerrainCellMesh>) -> Vec<u8> {
        rkyv::to_bytes::<RkyvError>(&TerrainChunkFile {
            format_version: TERRAIN_MESH_FORMAT_VERSION,
            worldspace_id,
            cells,
        })
        .unwrap()
        .to_vec()
    }

    fn write_chunk(root: &Path, worldspace_id: u32, gx: i32, gy: i32, bytes: &[u8]) {
        let path = root.join(terrain_chunk_path(worldspace_id, gx, gy));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn ready(root: &Path) -> TerrainMerge {
        let classes = Arc::new(TerrainClasses::from_table(vanilla_table(6)).unwrap());
        TerrainMerge::with_state(
            MergeState::Ready {
                classes,
                material: Handle::default(),
                images: Arc::from([]),
            },
            root.to_path_buf(),
        )
    }

    fn exterior(grid_x: i32, grid_y: i32) -> CellKey {
        CellKey::Exterior {
            worldspace_id: 0x3C,
            grid_x,
            grid_y,
        }
    }

    #[test]
    fn a_missing_table_is_absent_and_serves_no_cells() {
        let dir = tempfile::tempdir().unwrap();
        let mut merge = TerrainMerge::open(dir.path());
        assert!(matches!(merge.state, MergeState::Absent));
        assert!(!merge.holds_cells());
        assert!(matches!(
            merge.lookup(exterior(0, 0), 1),
            CellLookup::Unavailable
        ));
    }

    #[test]
    fn a_valid_table_waits_for_the_device_and_holds_cells() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LAND_TEXTURE_TABLE_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&vanilla_table(6)).unwrap()).unwrap();
        let merge = TerrainMerge::open(dir.path());
        assert!(matches!(merge.state, MergeState::AwaitingDevice(_)));
        assert!(merge.holds_cells());
        assert_eq!(merge.pending_classes().unwrap().diffuse.len(), 3);
    }

    #[test]
    fn a_broken_table_is_refused_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LAND_TEXTURE_TABLE_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut table = vanilla_table(3);
        table.entries[2].id = 7;
        std::fs::write(&path, serde_json::to_string(&table).unwrap()).unwrap();
        let merge = TerrainMerge::open(dir.path());
        let MergeState::Refused(reason) = &merge.state else {
            panic!("expected a refusal, got {:?}", merge.state);
        };
        assert!(reason.contains("land texture table"), "{reason}");
        assert!(!merge.holds_cells());

        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            TerrainMerge::open(dir.path()).state,
            MergeState::Refused(_)
        ));
    }

    #[test]
    fn lookups_find_cells_by_chunk_and_report_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let mut known = mesh_with_slots(8);
        known.cell_id = 0xA1;
        known.quadrant_slots[0] = 0x100;
        let mut unknown_texture = mesh_with_slots(8);
        unknown_texture.cell_id = 0xA2;
        unknown_texture.quadrant_slots[0] = 0x0BAD;
        // Cells (1, 1) and (2, 3) share chunk (0, 0); cell (-1, 0) is in chunk (-1, 0).
        write_chunk(
            dir.path(),
            0x3C,
            1,
            1,
            &chunk_bytes(0x3C, vec![known.clone(), unknown_texture]),
        );
        let mut merge = ready(dir.path());

        match merge.lookup(exterior(1, 1), 0xA1) {
            CellLookup::Found(cell) => assert_eq!(*cell, known),
            other => panic!("expected the cell, got {other:?}"),
        }
        // Same chunk file, second cell: the FormID is not in the table, so validation refuses it.
        match merge.lookup(exterior(2, 3), 0xA2) {
            CellLookup::Invalid(reason) => assert!(reason.contains("00000BAD"), "{reason}"),
            other => panic!("expected an invalid cell, got {other:?}"),
        }
        // The chunk exists but has no entry for this cell.
        assert!(matches!(
            merge.lookup(exterior(3, 3), 0xFF),
            CellLookup::NoData
        ));
        // No chunk file at all.
        assert!(matches!(
            merge.lookup(exterior(-1, 0), 0xA1),
            CellLookup::NoData
        ));
        // Interiors never use merged terrain.
        assert!(matches!(
            merge.lookup(CellKey::Interior(5), 5),
            CellLookup::Unavailable
        ));
    }

    #[test]
    fn a_corrupt_or_foreign_chunk_is_invalid_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        write_chunk(dir.path(), 0x3C, 0, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        write_chunk(dir.path(), 0x3C, 4, 0, &chunk_bytes(0x99, vec![]));
        let mut merge = ready(dir.path());
        assert!(matches!(
            merge.lookup(exterior(0, 0), 1),
            CellLookup::Invalid(_)
        ));
        match merge.lookup(exterior(4, 0), 1) {
            CellLookup::Invalid(reason) => assert!(reason.contains("worldspace"), "{reason}"),
            other => panic!("expected a worldspace refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_chunk_cache_is_bounded_and_rereads_evicted_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ChunkStore::new(dir.path().to_path_buf());
        for chunk in 0..(CHUNK_CACHE_LIMIT as i32 + 10) {
            let _ = store.cell(0x3C, chunk * 4, 0, 1);
        }
        assert!(store.chunks.len() <= CHUNK_CACHE_LIMIT);
        assert_eq!(store.chunks.len(), store.order.len());
        let mut cell = mesh_with_slots(8);
        cell.cell_id = 9;
        write_chunk(dir.path(), 0x3C, 0, 0, &chunk_bytes(0x3C, vec![cell]));
        // The earlier miss for chunk (0, 0) was evicted, so the new file is seen.
        assert!(matches!(store.cell(0x3C, 0, 0, 9), CellLookup::Found(_)));
    }

    fn array_image(format: TextureFormat, size: u32, layers: u32, repeat: bool) -> Image {
        let mut image = Image::default();
        image.texture_descriptor.format = format;
        image.texture_descriptor.size = Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        };
        image.texture_view_descriptor = Some(TextureViewDescriptor {
            dimension: Some(TextureViewDimension::D2Array),
            ..default()
        });
        image.sampler = if repeat {
            ImageSampler::Descriptor(terrain_layer_sampler())
        } else {
            ImageSampler::default()
        };
        image
    }

    #[test]
    fn array_images_are_checked_against_the_table() {
        let classes = TerrainClasses::from_table(vanilla_table(6)).unwrap();
        let slot = &classes.diffuse[0];
        let usage = LandTextureUsage::DiffuseSrgb;
        let good = array_image(
            TextureFormat::Bc1RgbaUnormSrgb,
            slot.size,
            slot.layers,
            true,
        );
        validate_array_image(&good, slot, usage).unwrap();

        let linear = array_image(TextureFormat::Bc1RgbaUnorm, slot.size, slot.layers, true);
        assert!(
            validate_array_image(&linear, slot, usage)
                .unwrap_err()
                .contains("expected Bc1RgbaUnormSrgb")
        );
        let short = array_image(TextureFormat::Bc1RgbaUnormSrgb, slot.size, 3, true);
        assert!(
            validate_array_image(&short, slot, usage)
                .unwrap_err()
                .contains("with 3 layers")
        );
        let clamped = array_image(
            TextureFormat::Bc1RgbaUnormSrgb,
            slot.size,
            slot.layers,
            false,
        );
        assert!(
            validate_array_image(&clamped, slot, usage)
                .unwrap_err()
                .contains("repeating sampler")
        );
        let mut flat = good.clone();
        flat.texture_view_descriptor = None;
        assert!(
            validate_array_image(&flat, slot, usage)
                .unwrap_err()
                .contains("2D array")
        );
        // A normal array is linear.
        let normal = &classes.normal[0];
        let linear_bc3 = array_image(
            TextureFormat::Bc3RgbaUnorm,
            normal.size,
            normal.layers,
            true,
        );
        validate_array_image(&linear_bc3, normal, LandTextureUsage::NormalLinear).unwrap();
        let srgb_bc3 = array_image(
            TextureFormat::Bc3RgbaUnormSrgb,
            normal.size,
            normal.layers,
            true,
        );
        assert!(validate_array_image(&srgb_bc3, normal, LandTextureUsage::NormalLinear).is_err());
    }

    /// An 80-byte KTX2 header with the given `vkFormat`, size and layer count.
    fn ktx2_header(vk_format: u32, size: u32, layers: u32) -> [u8; 80] {
        let mut header = [0u8; 80];
        header[..12].copy_from_slice(&[
            0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
        ]);
        // vkFormat, typeSize, width, height, depth, layers, faces, levels, supercompression.
        let fields = [vk_format, 1, size, size, 0, layers, 1, 11, 0];
        for (index, value) in fields.iter().enumerate() {
            header[12 + 4 * index..16 + 4 * index].copy_from_slice(&value.to_le_bytes());
        }
        header
    }

    #[test]
    fn array_headers_must_hold_the_usages_colour_space() {
        use ktx2::Format as F;
        let classes = TerrainClasses::from_table(vanilla_table(6)).unwrap();
        let diffuse = &classes.diffuse[0];
        let normal = &classes.normal[0];
        let (size, layers) = (diffuse.size, diffuse.layers);
        let diffuse_ok = |format: F| {
            check_ktx2_header(
                &ktx2_header(format.value(), size, layers),
                diffuse,
                LandTextureUsage::DiffuseSrgb,
            )
        };
        diffuse_ok(F::BC1_RGBA_SRGB_BLOCK).unwrap();
        diffuse_ok(F::BC1_RGB_SRGB_BLOCK).unwrap();
        let linear = diffuse_ok(F::BC1_RGBA_UNORM_BLOCK).unwrap_err();
        assert!(linear.contains("an sRGB"), "{linear}");
        assert!(linear.contains(&diffuse.path), "{linear}");
        let wrong_family = diffuse_ok(F::BC3_SRGB_BLOCK).unwrap_err();
        assert!(wrong_family.contains("BC3_SRGB_BLOCK"), "{wrong_family}");

        let normal_ok = |format: F| {
            check_ktx2_header(
                &ktx2_header(format.value(), normal.size, normal.layers),
                normal,
                LandTextureUsage::NormalLinear,
            )
        };
        normal_ok(F::BC3_UNORM_BLOCK).unwrap();
        let srgb = normal_ok(F::BC3_SRGB_BLOCK).unwrap_err();
        assert!(srgb.contains("a linear"), "{srgb}");

        let small = check_ktx2_header(
            &ktx2_header(F::BC1_RGBA_SRGB_BLOCK.value(), 512, layers),
            diffuse,
            LandTextureUsage::DiffuseSrgb,
        )
        .unwrap_err();
        assert!(small.contains("512x512"), "{small}");
        let short = check_ktx2_header(
            &ktx2_header(F::BC1_RGBA_SRGB_BLOCK.value(), size, 3),
            diffuse,
            LandTextureUsage::DiffuseSrgb,
        )
        .unwrap_err();
        assert!(short.contains("with 3 layers"), "{short}");
        let junk = check_ktx2_header(&[0; 80], diffuse, LandTextureUsage::DiffuseSrgb).unwrap_err();
        assert!(junk.contains("not a KTX2 file"), "{junk}");
        let supercompressed = check_ktx2_header(
            &ktx2_header(0, size, layers),
            diffuse,
            LandTextureUsage::DiffuseSrgb,
        )
        .unwrap_err();
        assert!(
            supercompressed.contains("supercompressed"),
            "{supercompressed}"
        );
    }

    #[test]
    fn array_files_are_read_from_the_output_root() {
        let dir = tempfile::tempdir().unwrap();
        let classes = TerrainClasses::from_table(vanilla_table(6)).unwrap();
        let slot = &classes.diffuse[0];
        let missing = check_array_file(dir.path(), slot, LandTextureUsage::DiffuseSrgb);
        assert!(missing.unwrap_err().contains(&slot.path));
        let path = dir.path().join(&slot.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            ktx2_header(
                ktx2::Format::BC1_RGBA_SRGB_BLOCK.value(),
                slot.size,
                slot.layers,
            ),
        )
        .unwrap();
        check_array_file(dir.path(), slot, LandTextureUsage::DiffuseSrgb).unwrap();
        // A file shorter than a header is an error, not a panic.
        std::fs::write(&path, [0xAB, 0x4B]).unwrap();
        assert!(check_array_file(dir.path(), slot, LandTextureUsage::DiffuseSrgb).is_err());
    }

    #[test]
    fn setup_runs_out_of_time_exactly_at_the_timeout() {
        let now = Instant::now();
        let started = now.checked_sub(SETUP_TIMEOUT).unwrap();
        assert!(setup_expired(started, now));
        assert!(!setup_expired(started + Duration::from_millis(1), now));
        assert!(!setup_expired(now, now));
    }

    #[test]
    fn only_exterior_cells_with_terrain_wait_for_the_setup() {
        let classes = Arc::new(TerrainClasses::from_table(vanilla_table(6)).unwrap());
        let mut merge =
            TerrainMerge::with_state(MergeState::AwaitingDevice(classes.clone()), PathBuf::new());
        assert!(merge.holds_cell(exterior(0, 0), true));
        assert!(!merge.holds_cell(exterior(0, 0), false), "no LAND");
        assert!(!merge.holds_cell(CellKey::Interior(7), true), "an interior");
        // The wait is logged once.
        assert!(merge.note_hold());
        assert!(!merge.note_hold());

        merge.state = MergeState::Refused("no".to_owned());
        assert!(!merge.holds_cell(exterior(0, 0), true));
        merge.state = MergeState::Absent;
        assert!(!merge.holds_cell(exterior(0, 0), true));
        let ready = ready(Path::new(""));
        assert!(!ready.holds_cell(exterior(0, 0), true));
    }

    fn setup_app(merge: TerrainMerge, verdict: Option<Result<(), String>>) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<ShaderBuffer>()
            .init_asset::<MergedTerrainMaterial>()
            .insert_resource(merge)
            .init_resource::<StreamingMetrics>();
        let slot = TerrainMergeVerdict::default();
        if let Some(verdict) = verdict {
            slot.0.set(verdict).unwrap();
        }
        app.insert_resource(slot)
            .add_systems(Update, advance_terrain_merge);
        app
    }

    fn awaiting(root: &Path, ago: Duration) -> TerrainMerge {
        let classes = Arc::new(TerrainClasses::from_table(vanilla_table(6)).unwrap());
        TerrainMerge::with_state_started(
            MergeState::AwaitingDevice(classes),
            root.to_path_buf(),
            ago,
        )
    }

    fn refusal(app: &App) -> String {
        match &app.world().resource::<TerrainMerge>().state {
            MergeState::Refused(reason) => reason.clone(),
            other => panic!("expected a refusal, got {}", other.label()),
        }
    }

    #[test]
    fn a_device_refusal_is_posted_and_counted() {
        let mut app = setup_app(
            awaiting(Path::new(""), Duration::ZERO),
            Some(Err("the device allows 16 sampled textures".to_owned())),
        );
        app.update();
        assert!(refusal(&app).contains("allows 16 sampled"));
        let metrics = app.world().resource::<StreamingMetrics>();
        assert!(metrics.terrain_merge_state.starts_with("refused: "));
    }

    #[test]
    fn a_render_world_that_never_reports_is_refused_after_the_timeout() {
        let mut waiting = setup_app(awaiting(Path::new(""), Duration::from_secs(5)), None);
        waiting.update();
        assert!(matches!(
            waiting.world().resource::<TerrainMerge>().state,
            MergeState::AwaitingDevice(_)
        ));
        assert_eq!(
            waiting
                .world()
                .resource::<StreamingMetrics>()
                .terrain_merge_state,
            "awaiting device"
        );

        let mut late = setup_app(awaiting(Path::new(""), SETUP_TIMEOUT), None);
        late.update();
        let reason = refusal(&late);
        assert!(reason.contains("did not report within 120 s"), "{reason}");
    }

    #[test]
    fn no_render_world_posts_a_refusal_at_once() {
        let verdict = TerrainMergeVerdict::default();
        post_missing_render_world(&verdict);
        let posted = verdict.0.get().unwrap().clone().unwrap_err();
        assert!(posted.contains("no render world"), "{posted}");
    }

    #[test]
    fn arrays_with_the_wrong_files_are_refused_before_anything_loads() {
        // The device accepts, but the files are missing.
        let dir = tempfile::tempdir().unwrap();
        let mut app = setup_app(awaiting(dir.path(), Duration::ZERO), Some(Ok(())));
        app.update();
        let reason = refusal(&app);
        assert!(reason.contains("land-diffuse"), "{reason}");

        // All files present and right: the arrays start loading.
        let classes = TerrainClasses::from_table(vanilla_table(6)).unwrap();
        let write = |slot: &ClassSlot, format: ktx2::Format| {
            let path = dir.path().join(&slot.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, ktx2_header(format.value(), slot.size, slot.layers)).unwrap();
        };
        for slot in &classes.diffuse {
            write(
                slot,
                match slot.format {
                    LandTextureFormat::Bc1 => ktx2::Format::BC1_RGBA_SRGB_BLOCK,
                    _ => ktx2::Format::BC3_SRGB_BLOCK,
                },
            );
        }
        // The normal array is stored sRGB, which is the wrong colour space for a normal map.
        write(&classes.normal[0], ktx2::Format::BC3_SRGB_BLOCK);
        let mut app = setup_app(awaiting(dir.path(), Duration::ZERO), Some(Ok(())));
        app.update();
        let reason = refusal(&app);
        assert!(reason.contains("a linear"), "{reason}");

        write(&classes.normal[0], ktx2::Format::BC3_UNORM_BLOCK);
        let mut app = setup_app(awaiting(dir.path(), Duration::ZERO), Some(Ok(())));
        app.update();
        assert!(matches!(
            app.world().resource::<TerrainMerge>().state,
            MergeState::LoadingArrays { .. }
        ));
    }

    #[test]
    fn arrays_that_never_finish_loading_are_refused_after_the_timeout() {
        let classes = Arc::new(TerrainClasses::from_table(vanilla_table(6)).unwrap());
        let state = || MergeState::LoadingArrays {
            classes: classes.clone(),
            diffuse: vec![Handle::default(); classes.diffuse.len()],
            normal: vec![Handle::default(); classes.normal.len()],
            entries: Handle::default(),
        };
        let mut waiting = setup_app(
            TerrainMerge::with_state_started(state(), PathBuf::new(), Duration::from_secs(1)),
            Some(Ok(())),
        );
        waiting.update();
        assert!(matches!(
            waiting.world().resource::<TerrainMerge>().state,
            MergeState::LoadingArrays { .. }
        ));
        let mut late = setup_app(
            TerrainMerge::with_state_started(state(), PathBuf::new(), SETUP_TIMEOUT),
            Some(Ok(())),
        );
        late.update();
        let reason = refusal(&late);
        assert!(reason.contains("still loading after 120 s"), "{reason}");
        assert!(reason.contains(&classes.diffuse[0].path), "{reason}");
    }
}
